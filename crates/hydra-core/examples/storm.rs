//! Reproduce the repair storm against the shipped scheduler, and measure what
//! each fix is worth.
//!
//! Run: `cargo run --release -p hya-core --example storm -- [out.csv]`
//!
//! The existing simulator does not reproduce it, and this is why: its origin
//! model gives every connection an independent rate and charges nothing for a
//! request. Reality gives neither.
//!
//!  * **Shared bottleneck.** n connections to one origin do not have n
//!    independent capacities; they share one. Moving bytes from connection A to
//!    connection B cannot make B faster except by making A slower, so the
//!    per-connection rate divergence the repair rule fires on is not evidence
//!    of anything repairable.
//!  * **Setup cost and slow start.** A repair hands the taker a range it must
//!    open a request for. That costs `delta`, and the new flow starts with a
//!    small congestion window, so for several RTTs it delivers well below the
//!    share the scheduler priced the repair against.
//!
//! Model: capacity `C` split across connections in proportion to their ramp
//! factor `1 - exp(-age/tau)`, renormalised so the aggregate is exactly `C`
//! (stationary transfer: the split jitters, the total does not). A connection
//! that has just been given a range delivers nothing for `delta` and then
//! restarts its ramp.
//!
//! With that model the correct repair count on a stationary transfer is zero.

use pdl_core::{Action, Scheduler, Source};

/// Deterministic PRNG, so a run is reproducible.
struct Rng(u64);
impl Rng {
    fn next_f64(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Approximately standard normal (sum of 4 uniforms, Irwin-Hall).
    fn normal(&mut self) -> f64 {
        let s: f64 = (0..4).map(|_| self.next_f64()).sum();
        (s - 2.0) * 1.732_050_8
    }
}

#[derive(Clone, Copy)]
struct Flow {
    /// Wall clock at which this flow's current request finishes setting up.
    ready_at: f64,
    /// Whether the flow currently holds a request at all.
    live: bool,
    /// The `hi` the running fetch task captured at spawn. `fetch_range` loops
    /// `while off < hi` on THIS value, so a scheduler-side shrink is invisible
    /// to it: the victim keeps pulling bytes the taker is now also pulling.
    task_hi: u64,
    /// Where the victim's socket has actually got to.
    task_off: u64,
    /// Persistent share weight. TCP flows sharing one bottleneck do NOT settle
    /// at equal rates: the share goes as roughly 1/RTT, and cwnd history makes
    /// the asymmetry outlive any single round trip. This is the term the
    /// existing simulator omits, and it is a property of the PATH, so a repair
    /// cannot move it — handing a laggard's bytes to a fast flow does not make
    /// the laggard faster, it just costs one more setup.
    weight: f64,
}

struct Params {
    size: u64,
    n: usize,
    /// Aggregate bottleneck capacity, bytes/s.
    capacity: f64,
    /// Per-request setup cost, seconds.
    delta: f64,
    /// Slow-start time constant, seconds.
    tau: f64,
    /// Per-connection multiplicative jitter, as a fraction.
    sigma: f64,
    /// Lognormal spread of the PERSISTENT per-flow share weight.
    sigma_w: f64,
    theta_scale: f64,
    seed: u64,
    /// At this fraction of the fluid optimum, connection 0's share weight is cut
    /// by `collapse_to`. This is the case repair EXISTS for, and the guard rails
    /// added for the shared-bottleneck case must not suppress it: a scheduler that
    /// never repairs is trivially free of repair storms and useless against a
    /// mirror that dies mid-transfer.
    collapse_at: Option<f64>,
    collapse_to: f64,
}

struct Outcome {
    makespan: f64,
    requests: u64,
    repairs: u64,
    /// Fraction of the object already held when each repair fired, averaged.
    /// A value near 1.0 means repairs cluster in the endgame.
    repair_progress: Vec<f64>,
    /// theta at the moment of each repair, seconds.
    repair_theta: Vec<f64>,
    /// Connection-seconds spent in setup rather than receiving.
    setup_waste: f64,
    /// Bytes that crossed the wire twice because a shrunk victim kept sending.
    dup_bytes: u64,
}

fn run(p: &Params, zombies_bite: bool) -> Outcome {
    let dt = 0.010;
    let sources = vec![Source {
        gamma_est: p.capacity / p.n as f64,
        delta_est: p.delta,
        ..Default::default()
    }];
    let mut sched = Scheduler::new(p.size, sources, &[p.n]).with_theta_scale(p.theta_scale);
    let mut rng = Rng(p.seed | 1);
    let mut flows: Vec<Flow> = (0..p.n)
        .map(|_| Flow {
            ready_at: 0.0,
            live: false,
            weight: (p.sigma_w * rng.normal()).exp(),
            task_hi: 0,
            task_off: 0,
        })
        .collect();
    let mut setup_waste = 0.0;
    let mut dup_bytes = 0u64;
    let mut collapsed = false;
    let mut repair_progress: Vec<f64> = Vec::new();
    let mut repair_theta: Vec<f64> = Vec::new();
    let mut last_repairs = 0u64;
    let mut now = 0.0;

    // Generous horizon: 100x the fluid optimum.
    let horizon = 100.0 * p.size as f64 / p.capacity;
    while now < horizon {
        for act in sched.tick(now) {
            match act {
                Action::Request { conn, range } => {
                    // A new request means a new flow: pay setup, restart the ramp.
                    flows[conn].ready_at = now + p.delta;
                    flows[conn].live = true;
                    flows[conn].task_hi = range.hi;
                    flows[conn].task_off = range.lo;
                }
                Action::Cancel { conn } => flows[conn].live = false,
                // The fix under test. `zombies_bite == false` is the transport
                // honouring it: the victim's loop stops at the new far end. Left
                // unhandled (`true`) it models the old behaviour, where the
                // scheduler shrank its own copy and the socket never learned.
                Action::Shrink { conn, hi } => {
                    if !zombies_bite {
                        flows[conn].task_hi = hi;
                    }
                }
            }
        }
        if sched.stats.repairs > last_repairs {
            let k = (sched.stats.repairs - last_repairs) as usize;
            let frac = sched.bytes_held() as f64 / p.size as f64;
            let th = sched.theta_now(now);
            for _ in 0..k {
                repair_progress.push(frac);
                repair_theta.push(th);
            }
            last_repairs = sched.stats.repairs;
        }
        if sched.is_complete() {
            break;
        }

        // Adversary: one connection's share of the bottleneck collapses.
        if let Some(frac) = p.collapse_at {
            let at = frac * p.size as f64 / p.capacity;
            if now >= at && !collapsed {
                flows[0].weight *= p.collapse_to;
                collapsed = true;
            }
        }

        // Who can receive right now, and with how much of the window open.
        //
        // A "zombie" is a connection whose scheduler-side range was shrunk by a
        // repair while its socket keeps streaming to the `hi` it captured. It
        // still competes for the bottleneck, and none of what it delivers is
        // credited. Modelling it is the difference between reproducing the storm
        // and not.
        let mut ramp = vec![0.0f64; p.n];
        let mut zombie = vec![false; p.n];
        for j in 0..p.n {
            let sched_hi = sched.conn_range(j).map(|(_, _, hi)| hi);
            if zombies_bite {
                let live_socket = flows[j].live && flows[j].task_off < flows[j].task_hi;
                let shrunk = match sched_hi {
                    Some(hi) => hi < flows[j].task_hi,
                    None => true,
                };
                if live_socket && shrunk && now >= flows[j].ready_at {
                    let age = now - flows[j].ready_at;
                    let base = 1.0 - (-age / p.tau).exp();
                    ramp[j] = (base * flows[j].weight).max(0.0);
                    zombie[j] = true;
                    continue;
                }
            }
            let has_range = sched.conn_range(j).is_some();
            if has_range && flows[j].live && now >= flows[j].ready_at {
                let age = now - flows[j].ready_at;
                let base = 1.0 - (-age / p.tau).exp();
                let noise = (p.sigma * rng.normal()).exp();
                ramp[j] = (base * noise * flows[j].weight).max(0.0);
            } else if has_range && flows[j].live {
                setup_waste += dt;
            }
        }
        let total: f64 = ramp.iter().sum();
        if total > 0.0 {
            for j in 0..p.n {
                if ramp[j] <= 0.0 {
                    continue;
                }
                // Renormalise: the SPLIT jitters, the aggregate is conserved.
                let rate = p.capacity * ramp[j] / total;
                let bytes = (rate * dt) as u64;
                if bytes == 0 {
                    continue;
                }
                if zombie[j] {
                    // Bytes burned on a span the taker is fetching too.
                    let room = flows[j].task_hi.saturating_sub(flows[j].task_off);
                    let step = bytes.min(room);
                    flows[j].task_off += step;
                    dup_bytes += step;
                    if flows[j].task_off >= flows[j].task_hi {
                        flows[j].live = false;
                    }
                    continue;
                }
                if let Some((_, pos, _)) = sched.conn_range(j) {
                    sched.on_bytes_at(j, pos, bytes, now, dt);
                    flows[j].task_off = pos + bytes;
                }
            }
        }
        now += dt;
    }

    Outcome {
        makespan: now,
        requests: sched.stats.requests,
        repairs: sched.stats.repairs,
        setup_waste,
        dup_bytes,
        repair_progress,
        repair_theta,
    }
}

fn main() {
    let out = std::env::args().nth(1);
    let size = 5_300_000u64;
    let capacity = 1_400_000.0; // ~1.4 MB/s, the CRAN measurements' ballpark
    let oracle = size as f64 / capacity;
    let seeds = 12u64;

    let mut csv = String::from(
        "n,zombie,seed,makespan_s,oracle_ratio,requests,repairs,dup_bytes,setup_waste_s\n",
    );

    println!(
        "object {} bytes, shared capacity {:.0} B/s, fluid oracle {:.2}s, {} seeds",
        size, capacity, oracle, seeds
    );
    println!("stationary transfer: the correct repair count is 0.");
    println!("zombie=yes is the SHIPPED transport: a repair shrinks the victim's range");
    println!("scheduler-side but emits no Cancel, and fetch_range loops on the `hi` it");
    println!("captured at spawn, so the stolen span crosses the wire twice.\n");
    println!(
        "{:>3}  {:>7}  {:>9}  {:>7}  {:>8}  {:>8}  {:>9}",
        "n", "zombie", "makespan", "ratio", "requests", "repairs", "dup_MB"
    );

    for &n in &[1usize, 2, 4, 8, 16] {
        for &z in &[true, false] {
            let (mut ms, mut rq, mut rp, mut db) = (0.0, 0u64, 0u64, 0u64);
            let mut all_prog: Vec<f64> = Vec::new();
            let mut all_theta: Vec<f64> = Vec::new();
            for sd in 0..seeds {
                let p = Params {
                    size,
                    n,
                    capacity,
                    delta: 0.12,
                    tau: 0.35,
                    sigma: 0.10,
                    sigma_w: 0.45,
                    theta_scale: 1.0,
                    seed: 0x5EED + sd * 7919,
                    collapse_at: None,
                    collapse_to: 1.0,
                };
                let o = run(&p, z);
                csv.push_str(&format!(
                    "{},{},{},{:.4},{:.4},{},{},{},{:.4}\n",
                    n,
                    if z { 1 } else { 0 },
                    p.seed,
                    o.makespan,
                    o.makespan / oracle,
                    o.requests,
                    o.repairs,
                    o.dup_bytes,
                    o.setup_waste
                ));
                all_prog.extend(o.repair_progress.iter().copied());
                all_theta.extend(o.repair_theta.iter().copied());
                ms += o.makespan;
                rq += o.requests;
                rp += o.repairs;
                db += o.dup_bytes;
            }
            // Where in the transfer do the surviving repairs fire, and how wide was
            // the deadband when they did? theta = scale*sqrt(delta*T_rem/n) goes to
            // zero as the remaining time does, so the deadband is narrowest exactly
            // when a repair has the least time left to pay itself back.
            if !z && !all_prog.is_empty() {
                let nr = all_prog.len() as f64;
                let mean_p = all_prog.iter().sum::<f64>() / nr;
                let late = all_prog.iter().filter(|x| **x > 0.9).count();
                let mean_th = all_theta.iter().sum::<f64>() / nr;
                let min_th = all_theta.iter().cloned().fold(f64::INFINITY, f64::min);
                eprintln!(
                    "    n={n}: {} repairs | mean progress at repair {:.2} | {} fired past 90% \
                     | theta mean {:.3}s min {:.4}s",
                    all_prog.len(),
                    mean_p,
                    late,
                    mean_th,
                    min_th
                );
            }
            let f = seeds as f64;
            println!(
                "{:>3}  {:>7}  {:>8.2}s  {:>7.3}  {:>8.1}  {:>8.1}  {:>8.2}",
                n,
                if z { "yes" } else { "no" },
                ms / f,
                (ms / f) / oracle,
                rq as f64 / f,
                rp as f64 / f,
                db as f64 / f / 1e6
            );
        }
    }

    // ---- the case repair exists for -------------------------------------------
    //
    // Everything above is a STATIONARY transfer, where the correct repair count is
    // zero and every repair is waste. That makes it the right test for a storm and
    // the wrong test for the mechanism: a scheduler that simply never repaired
    // would score perfectly on it. So run the opposite scenario too — one
    // connection's share of the bottleneck collapses to 5% at 30% of the way
    // through — and check that repairs still fire and still help.
    println!("\ncollapse arm: connection 0 drops to 5% of its share at 30% progress");
    println!(
        "{:>3}  {:>9}  {:>7}  {:>8}  {:>8}",
        "n", "makespan", "ratio", "requests", "repairs"
    );
    for &n in &[2usize, 4, 8] {
        let (mut ms, mut rq, mut rp) = (0.0, 0u64, 0u64);
        for sd in 0..seeds {
            let o = run(
                &Params {
                    size,
                    n,
                    capacity,
                    delta: 0.12,
                    tau: 0.35,
                    sigma: 0.10,
                    sigma_w: 0.45,
                    theta_scale: 1.0,
                    seed: 0x5EED + sd * 7919,
                    collapse_at: Some(0.30),
                    collapse_to: 0.05,
                },
                false,
            );
            ms += o.makespan;
            rq += o.requests;
            rp += o.repairs;
        }
        let f = seeds as f64;
        println!(
            "{:>3}  {:>8.2}s  {:>7.3}  {:>8.1}  {:>8.1}",
            n,
            ms / f,
            (ms / f) / oracle,
            rq as f64 / f,
            rp as f64 / f
        );
    }

    if let Some(path) = out {
        std::fs::write(&path, csv).expect("write csv");
        println!("\nwrote {}", path);
    }
}
