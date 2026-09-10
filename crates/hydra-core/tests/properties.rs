//! Property tests for the two invariants that matter.
//!
//! `coverage_holds` is SAFETY: no byte is lost or double-owned.
//! `liveness_holds` is PROGRESS: some enabled transition reduces the unheld
//! count. The distinction is not academic -- the reference simulator once
//! livelocked with coverage intact, because a fully-stolen range left a
//! connection idle while holding a pipelined queue it would never start.

use pdl_core::intervals::{IntervalSet, Range};
use pdl_core::sched::{greedy_concurrency, Scheduler, Source};
use proptest::prelude::*;

fn src(gamma: f64) -> Source {
    Source {
        gamma_est: gamma,
        delta_est: 0.05,
        ..Default::default()
    }
}

/// A scheduler over `n` equal 1 MB/s sources with one connection each â€?the
/// standard rig for the failure-injection properties, which perturb ONE
/// source and need the rest uniform.
fn uniform_sched(n: usize, size: u64) -> Scheduler {
    let sources: Vec<Source> = (0..n).map(|_| src(1.0e6)).collect();
    Scheduler::new(size, sources, &vec![1; n]).with_stall_timeout(0.3)
}

proptest! {
    /// A random sequence of inserts and removes leaves the set sorted,
    /// coalesced, and with a total matching an independent bitmap.
    #[test]
    fn interval_set_matches_bitmap(
        ops in prop::collection::vec((0u64..256, 0u64..256, any::<bool>()), 1..60)
    ) {
        const N: usize = 256;
        let mut set = IntervalSet::new();
        let mut bitmap = [false; N];
        for (a, b, is_insert) in ops {
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            if is_insert {
                set.insert(Range::new(lo, hi));
                for i in lo..hi { bitmap[i as usize] = true; }
            } else {
                set.remove(lo, hi);
                for i in lo..hi { bitmap[i as usize] = false; }
            }
            prop_assert!(set.invariant_holds(), "coalescing invariant broken: {:?}", set);
            let expect = bitmap.iter().filter(|b| **b).count() as u64;
            prop_assert_eq!(set.total(), expect, "total diverged from bitmap");
        }
    }

    /// take_front never returns more than requested and never loses bytes.
    #[test]
    fn take_front_conserves(size in 1u64..100_000, takes in prop::collection::vec(1u64..5000, 1..40)) {
        let mut set = IntervalSet::full(size);
        let mut taken = 0u64;
        for n in takes {
            if let Some(r) = set.take_front(n) {
                prop_assert!(r.len() <= n);
                taken += r.len();
            }
            prop_assert_eq!(taken + set.total(), size);
            prop_assert!(set.invariant_holds());
        }
    }

    /// Both invariants hold and the transfer terminates under random rates,
    /// including connections that deliver nothing at all.
    #[test]
    fn scheduler_terminates_under_random_rates(
        size in 50_000u64..1_000_000,
        rates in prop::collection::vec(0u64..40_000, 2..7),
    ) {
        // At least one connection must be able to deliver, else (R4) is violated
        // and no algorithm terminates -- a precondition, not a bug.
        let mut rates = rates;
        if rates.iter().all(|r| *r == 0) { rates[0] = 10_000; }

        let n = rates.len();
        let sources: Vec<Source> = rates.iter().map(|r| src(*r as f64 * 100.0)).collect();
        let mut s = Scheduler::new(size, sources, &vec![1; n]).with_stall_timeout(0.5);

        let mut now = 0.0f64;
        let dt = 0.01;
        let mut steps = 0u32;
        while !s.is_complete() && steps < 200_000 {
            s.tick(now);
            for (j, r) in rates.iter().enumerate().take(n) {
                if *r > 0 {
                    s.on_bytes(j, *r, now, dt);
                }
            }
            prop_assert!(s.coverage_holds(), "coverage broke at t={}", now);
            prop_assert!(s.liveness_holds(), "livelocked at t={}", now);
            now += dt;
            steps += 1;
        }
        prop_assert!(s.is_complete(), "did not finish: {} of {} after {} steps", s.bytes_held(), size, steps);
    }

    /// Mid-transfer collapse of an arbitrary connection: the reclaim path must
    /// return the stranded bytes so the transfer still finishes.
    #[test]
    fn scheduler_survives_mid_transfer_collapse(
        size in 100_000u64..800_000,
        victim in 0usize..4,
        collapse_at in 5u32..200,
    ) {
        let n = 4usize;
        let mut s = uniform_sched(n, size);
        let mut now = 0.0f64;
        let dt = 0.01;
        let mut steps = 0u32;
        while !s.is_complete() && steps < 200_000 {
            s.tick(now);
            for j in 0..n {
                let dead = j == victim % n && steps >= collapse_at;
                if !dead { s.on_bytes(j, 10_000, now, dt); }
            }
            prop_assert!(s.coverage_holds());
            prop_assert!(s.liveness_holds(), "livelocked after collapse at t={}", now);
            now += dt;
            steps += 1;
        }
        prop_assert!(s.is_complete(), "stalled at {} of {}", s.bytes_held(), size);
    }

    /// Suspending a source at an arbitrary time loses no bytes and does not
    /// prevent completion, provided another source survives.
    #[test]
    fn suspension_preserves_coverage_and_completion(
        size in 100_000u64..600_000,
        suspend_at in 1u32..80,
    ) {
        let n = 3usize;
        let mut s = uniform_sched(n, size);
        let mut now = 0.0f64;
        let dt = 0.01;
        let mut steps = 0u32;
        while !s.is_complete() && steps < 200_000 {
            if steps == suspend_at { s.suspend_source(0, now + 5.0); }
            s.tick(now);
            for j in 1..n { s.on_bytes(j, 10_000, now, dt); }
            prop_assert!(s.coverage_holds());
            prop_assert!(s.liveness_holds());
            now += dt;
            steps += 1;
        }
        prop_assert!(s.is_complete());
    }

    /// Greedy concurrency is optimal: checked against exhaustive
    /// search over the feasible integer set.
    #[test]
    fn greedy_concurrency_is_exactly_optimal(
        rho_in in prop::collection::vec(1u64..40, 1..4),
        gamma_in in prop::collection::vec(1u64..20, 1..4),
        cap in 1u64..120,
        budget in 1usize..7,
    ) {
        let m = rho_in.len().min(gamma_in.len());
        let rho: Vec<f64> = rho_in[..m].iter().map(|v| *v as f64).collect();
        let gamma: Vec<f64> = gamma_in[..m].iter().map(|v| *v as f64).collect();
        let cap = cap as f64;

        let g = |n: &[usize]| -> f64 {
            let s: f64 = (0..m).map(|i| rho[i].min(n[i] as f64 * gamma[i])).sum();
            s.min(cap)
        };
        let got = g(&greedy_concurrency(&rho, &gamma, cap, budget));

        let mut best = 0.0f64;
        let mut idx = vec![0usize; m];
        loop {
            if idx.iter().sum::<usize>() <= budget {
                best = best.max(g(&idx));
            }
            let mut k = 0usize;
            loop {
                if k == m { break; }
                idx[k] += 1;
                if idx[k] <= budget { break; }
                idx[k] = 0;
                k += 1;
            }
            if k == m { break; }
        }
        prop_assert!((got - best).abs() < 1e-9,
            "greedy {} vs optimum {} (rho={:?} gamma={:?} cap={} budget={})",
            got, best, rho, gamma, cap, budget);
    }
}

/// Verify invariants when all connections stall:
/// `coverage_holds` stays true and `liveness_holds` stays true.
/// The higher-level transport loop enforces wall-clock timeouts when no sources make progress.
#[test]
fn total_collapse_keeps_both_invariants_while_never_completing() {
    let n = 3usize;
    let sources: Vec<Source> = (0..n).map(|_| src(1.0e6)).collect();
    let mut s = Scheduler::new(400_000, sources, &vec![1; n]).with_stall_timeout(0.3);
    let mut now = 0.0f64;
    for _ in 0..5_000 {
        s.tick(now);
        // Nobody ever delivers a byte.
        assert!(
            s.coverage_holds(),
            "safety must survive total collapse: the bytes are unobtainable, not lost"
        );
        assert!(
            s.liveness_holds(),
            "a reclaimable stall IS an enabled transition, so liveness holds vacuously"
        );
        now += 0.01;
    }
    assert!(
        !s.is_complete(),
        "nothing was delivered, so the transfer cannot be complete"
    );
    assert_eq!(s.bytes_held(), 0, "no progress was ever made");
}

/// A rate sample must never be taken over a microsecond interval.
///
/// Reported directly from a real 8-connection transfer: per-connection rates read
/// 70.6 and 128.6 MiB/s while the aggregate was 2.78 MiB/s, and every connection
/// was graded `bad`. One cause. An arrival is one `read()` return, and a read
/// served from the socket's already-buffered data completes in microseconds, so
/// `bytes/dt` for that arrival measures memcpy speed. Those inflated samples then
/// raised the detector's reference so every honest sample looked like a collapse.
#[test]
fn buffered_reads_do_not_inflate_the_rate_estimate() {
    let n = 1usize;
    let sources: Vec<Source> = (0..n).map(|_| src(1.0e6)).collect();
    let mut s = Scheduler::new(8 << 20, sources, &vec![1; n]).with_stall_timeout(5.0);
    let mut now = 0.0f64;
    s.tick(now);

    // A link genuinely doing 1 MB/s, delivered the way a socket actually delivers:
    // a 64 KiB window arrives, then four reads drain it back-to-back in ~20 us
    // each. Per-arrival, those four imply ~3 GB/s.
    // `dt` in the transport is the gap between consecutive arrivals, so the wait
    // for a window belongs to the FIRST read of that window; the three that drain
    // it behind that read carry microsecond gaps.
    let window = 64.0 * 1024.0;
    let wait = window / 1.0e6 - 60e-6; // 64 KiB at 1 MB/s, minus the drain time
    for _ in 0..40 {
        s.on_bytes(0, 16 << 10, now, wait);
        now += wait;
        for _ in 0..3 {
            s.on_bytes(0, 16 << 10, now, 20e-6);
            now += 20e-6;
        }
        s.tick(now);
    }

    let est = s.conn_rate(0);
    assert!(
        est < 3.0e6,
        "rate estimate {est:.0} B/s reflects buffer drain, not the ~1 MB/s link"
    );
    assert!(
        est > 0.3e6,
        "rate estimate {est:.0} B/s is implausibly low for a ~1 MB/s link"
    );
    assert!(
        !s.conn_health(0).is_suspect_or_worse(),
        "a healthy steady link must not be graded {:?}",
        s.conn_health(0)
    );
}
