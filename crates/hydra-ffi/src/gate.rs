// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Priority-aware job admission gate.
//!
//! Manages job execution concurrency based on configured limits and priority levels.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

/// Queued job waiter entry with priority and sequence ordering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Waiter {
    prio: u32,
    seq: u64,
}

struct State {
    running: usize,
    waiting: Vec<Waiter>,
}

/// Bounded, priority-ordered admission gate.
pub(crate) struct Gate {
    limit: AtomicUsize,
    state: Mutex<State>,
    wake: Notify,
}

/// Queued admission ticket held while waiting for execution capacity.
pub(crate) struct Ticket {
    gate: Arc<Gate>,
    me: Waiter,
    consumed: bool,
}

/// Active execution permit granted when a job enters execution.
pub(crate) struct Permit {
    gate: Arc<Gate>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        {
            let mut g = self.gate.lock();
            g.running = g.running.saturating_sub(1);
        }
        self.gate.wake.notify_waiters();
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        if self.consumed {
            return;
        }
        let mut g = self.gate.lock();
        g.waiting.retain(|w| *w != self.me);
    }
}

impl Gate {
    /// Creates an admission gate with the specified concurrency limit.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit: AtomicUsize::new(limit.max(1)),
            state: Mutex::new(State {
                running: 0,
                waiting: Vec::new(),
            }),
            wake: Notify::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Dynamically updates the concurrency limit.
    pub(crate) fn set_limit(self: &Arc<Self>, limit: usize) {
        self.limit.store(limit.max(1), Ordering::Relaxed);
        self.wake.notify_waiters();
    }

    /// Enqueues a job with priority and sequence identifier.
    pub(crate) fn enqueue(self: &Arc<Self>, prio: u32, seq: u64) -> Ticket {
        let me = Waiter { prio, seq };
        self.lock().waiting.push(me);
        Ticket {
            gate: self.clone(),
            me,
            consumed: false,
        }
    }

    /// Attempts to acquire an execution slot if available and eligible.
    fn try_take(&self, me: Waiter) -> bool {
        let limit = self.limit.load(Ordering::Relaxed);
        let mut g = self.lock();
        if g.running >= limit {
            return false;
        }
        let best = g
            .waiting
            .iter()
            .copied()
            .max_by(|a, b| a.prio.cmp(&b.prio).then(b.seq.cmp(&a.seq)));
        if best != Some(me) {
            return false;
        }
        g.waiting.retain(|w| *w != me);
        g.running += 1;
        true
    }
}

impl Ticket {
    /// Awaits an execution permit from the gate.
    pub(crate) async fn wait(mut self) -> Permit {
        loop {
            let notified = self.gate.wake.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.gate.try_take(self.me) {
                self.consumed = true;
                return Permit {
                    gate: self.gate.clone(),
                };
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn rt(threads: usize) -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(threads)
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn a_high_priority_waiter_is_admitted_before_an_earlier_normal_one() {
        rt(2).block_on(async {
            let gate = Arc::new(Gate::new(1));
            let held = gate.enqueue(1, 0).wait().await;

            let order = Arc::new(Mutex::new(Vec::new()));
            // Both places are taken here, in this order, so the test exercises
            // the priority comparison rather than a scheduling race.
            let normal = gate.enqueue(1, 1);
            let high = gate.enqueue(2, 2);

            let o1 = order.clone();
            let t1 = tokio::spawn(async move {
                let _p = normal.wait().await;
                o1.lock().unwrap().push("normal");
            });
            let o2 = order.clone();
            let t2 = tokio::spawn(async move {
                let _p = high.wait().await;
                o2.lock().unwrap().push("high");
            });

            drop(held);
            t2.await.unwrap();
            t1.await.unwrap();
            assert_eq!(*order.lock().unwrap(), vec!["high", "normal"]);
        });
    }

    /// Arrival order is the order `enqueue` was called in, not the order the
    /// runtime happened to poll the waiting tasks in. Regression: with both
    /// steps inside the spawned task, starting A then B with `max_jobs = 1` ran
    /// B first roughly half the time.
    #[test]
    fn equal_priority_jobs_run_in_the_order_they_were_queued() {
        for round in 0..50 {
            rt(4).block_on(async {
                let gate = Arc::new(Gate::new(1));
                let order = Arc::new(Mutex::new(Vec::new()));
                let first = gate.enqueue(1, 0);
                let second = gate.enqueue(1, 1);

                let o2 = order.clone();
                // Spawned in the OPPOSITE order, to make the point that the
                // spawn order is not what decides.
                let t2 = tokio::spawn(async move {
                    let _p = second.wait().await;
                    o2.lock().unwrap().push(1u64);
                });
                let o1 = order.clone();
                let t1 = tokio::spawn(async move {
                    let _p = first.wait().await;
                    o1.lock().unwrap().push(0u64);
                });
                t1.await.unwrap();
                t2.await.unwrap();
                assert_eq!(*order.lock().unwrap(), vec![0, 1], "round {round}");
            });
        }
    }

    /// The waiter must not miss a release that lands between its eligibility
    /// test and its await. Repeated, because the window is small enough that a
    /// single iteration passes even with the bug present.
    #[test]
    fn a_release_racing_the_wait_is_never_missed() {
        rt(4).block_on(async {
            for round in 0..200u64 {
                let gate = Arc::new(Gate::new(1));
                let held = gate.enqueue(1, round * 2).wait().await;
                let ticket = gate.enqueue(1, round * 2 + 1);
                let waiter = tokio::spawn(async move {
                    let _p = ticket.wait().await;
                });
                // No yield: the release should land at an arbitrary point in
                // the waiter's registration sequence.
                drop(held);
                tokio::time::timeout(Duration::from_secs(5), waiter)
                    .await
                    .unwrap_or_else(|_| panic!("round {round}: waiter was never woken"))
                    .unwrap();
            }
        });
    }

    #[test]
    fn an_abandoned_waiter_does_not_consume_or_block_a_slot() {
        rt(2).block_on(async {
            let gate = Arc::new(Gate::new(1));
            let held = gate.enqueue(1, 0).wait().await;

            // Dropped without ever being awaited: the place must be released.
            let abandoned = gate.enqueue(1, 1);
            drop(abandoned);

            // And one dropped mid-wait.
            let ticket = gate.enqueue(1, 2);
            let task = tokio::spawn(async move {
                let _p = ticket.wait().await;
            });
            tokio::task::yield_now().await;
            task.abort();
            let _ = task.await;

            drop(held);
            let next = gate.enqueue(1, 3);
            tokio::time::timeout(Duration::from_secs(2), next.wait())
                .await
                .expect("a phantom waiter must not block the queue");
        });
    }
}
