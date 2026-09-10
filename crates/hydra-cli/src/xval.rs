//! Cross-validation: the SAME scheduler core, driven by real HTTP, against the
//! fluid oracle and against the competing policies.
//!
//! The simulator predicts a makespan ratio for each scenario. This harness
//! reproduces those scenarios over the transport and reports the measured ratio,
//! so the theory is checked against the shipped implementation rather than
//! against a model of it.
//!
//! Competing policies are implemented here as *policies*:
//! `EqualStatic` is static partitioning (N equal parts up front, never
//! reassigned), `StealOnIdle` is work-stealing on idle.
//! The comparison is a benchmark of algorithm behavior.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::{byte_at, OriginControl, OriginSet};
use pdl_net::{run_transfer_tick, Target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

pub struct Scenario {
    pub name: &'static str,
    /// per-origin steady rate, bytes/s
    pub rates: Vec<u64>,
    /// (delay_ms, origin, new_rate) applied mid-transfer
    pub events: Vec<(u64, usize, u64)>,
    /// origins that go silent (delay_ms, origin)
    pub blackholes: Vec<(u64, usize)>,
}

pub fn scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "stationary_het",
            rates: vec![8 << 20, 4 << 20, 2 << 20],
            events: vec![],
            blackholes: vec![],
        },
        Scenario {
            name: "throttle_midway",
            rates: vec![8 << 20, 8 << 20, 8 << 20],
            events: vec![(300, 0, 256 << 10)],
            blackholes: vec![],
        },
        Scenario {
            name: "mirror_failure",
            rates: vec![6 << 20, 6 << 20, 6 << 20],
            events: vec![],
            blackholes: vec![(300, 2)],
        },
        Scenario {
            name: "volatile",
            rates: vec![6 << 20, 6 << 20, 6 << 20],
            events: vec![
                (200, 0, 1 << 20),
                (500, 1, 1 << 20),
                (800, 0, 8 << 20),
                (1100, 2, 1 << 20),
                (1400, 1, 8 << 20),
            ],
            blackholes: vec![],
        },
    ]
}

/// The fluid oracle for this scenario: bytes / (time-integrated aggregate rate).
/// Computed by integrating the rate schedule forward, exactly as `fluid_oracle`
/// does in the Python simulator.
pub fn fluid_oracle(sc: &Scenario, size: u64) -> f64 {
    let n = sc.rates.len();
    let mut rate: Vec<f64> = sc.rates.iter().map(|r| *r as f64).collect();
    let mut evs: Vec<(f64, usize, f64)> = sc
        .events
        .iter()
        .map(|(ms, i, r)| (*ms as f64 / 1000.0, *i, *r as f64))
        .collect();
    for (ms, i) in &sc.blackholes {
        evs.push((*ms as f64 / 1000.0, *i, 0.0));
    }
    evs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());

    let mut t = 0.0f64;
    let mut done = 0.0f64;
    let target = size as f64;
    for (te, i, r) in evs {
        let agg: f64 = rate.iter().sum();
        if agg > 0.0 {
            let need = (target - done) / agg;
            if t + need <= te {
                return t + need;
            }
            done += agg * (te - t);
        }
        t = te;
        if i < n {
            rate[i] = r;
        }
    }
    let agg: f64 = rate.iter().sum();
    if agg <= 0.0 {
        return f64::INFINITY;
    }
    t + (target - done) / agg
}

fn spawn_scenario(sc: &Scenario, size: u64) -> (Arc<OriginSet>, Vec<Target>, Vec<OriginControl>) {
    let net = Arc::new(OriginSet::new());
    let mut targets = Vec::new();
    let mut ctls = Vec::new();
    for r in &sc.rates {
        let (p, c) = net.spawn(size, *r);
        targets.push(Target::direct("127.0.0.1", p, "/obj"));
        ctls.push(c);
    }
    (net, targets, ctls)
}

fn arm_events(sc: &Scenario, ctls: &[OriginControl]) {
    for (ms, i, r) in sc.events.clone() {
        let c = ctls[i].clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
            c.rate.store(r, Ordering::Relaxed);
        });
    }
    for (ms, i) in sc.blackholes.clone() {
        let c = ctls[i].clone();
        tokio::spawn(async move {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
            c.blackhole.store(true, Ordering::Relaxed);
        });
    }
}

fn verify(path: &str, size: u64) -> bool {
    match std::fs::read(path) {
        Ok(d) => {
            d.len() as u64 == size && d.iter().enumerate().all(|(i, b)| *b == byte_at(i as u64))
        }
        Err(_) => false,
    }
}

/// HYDRA: the shipped scheduler.
async fn run_hydra(sc: &Scenario, size: u64, conns: usize) -> Option<(f64, u64)> {
    run_hydra_tick(sc, size, conns, 20).await
}

async fn run_hydra_tick(
    sc: &Scenario,
    size: u64,
    conns: usize,
    tick_ms: u64,
) -> Option<(f64, u64)> {
    run_hydra_with(sc, size, conns, tick_ms, "hydra_xval", |s| s).await
}

/// The shared body of every hydra-policy runner: spawn the scenario, arm its
/// events, build per-source estimates, run the transfer, verify the bytes,
/// clean up. `configure` is the one thing the variants differ in â€?what they
/// do to the scheduler before it runs.
async fn run_hydra_with(
    sc: &Scenario,
    size: u64,
    conns: usize,
    tick_ms: u64,
    file_tag: &str,
    configure: impl FnOnce(Scheduler) -> Scheduler,
) -> Option<(f64, u64)> {
    let (net, targets, ctls) = spawn_scenario(sc, size);
    arm_events(sc, &ctls);
    let per: Vec<usize> = sc.rates.iter().map(|_| conns).collect();
    let sources: Vec<Source> = sc
        .rates
        .iter()
        .map(|r| Source {
            gamma_est: *r as f64 / conns as f64,
            delta_est: 0.005,
            ..Default::default()
        })
        .collect();
    let out = std::env::temp_dir().join(format!("{file_tag}_{}.bin", sc.name));
    let outs = out.to_string_lossy().to_string();
    let sched = configure(Scheduler::new(size, sources, &per).with_stall_timeout(0.6));
    let r = run_transfer_tick(net, targets, &per, size, &outs, sched, tick_ms)
        .await
        .ok();
    let ok = verify(&outs, size);
    let _ = std::fs::remove_file(&out);
    r.filter(|_| ok)
}

/// Static partitioning policy: N equal contiguous parts, assigned once, never reassigned.
/// A part whose connection dies is retried on the SAME connection.
async fn run_equal_static(sc: &Scenario, size: u64, conns: usize) -> Option<(f64, u64)> {
    let (net, targets, ctls) = spawn_scenario(sc, size);
    arm_events(sc, &ctls);
    let n = sc.rates.len() * conns;
    let out = std::env::temp_dir().join(format!("static_xval_{}.bin", sc.name));
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(pdl_net::SparseSink::create(&outs, size).ok()?);
    let t0 = std::time::Instant::now();

    // Each part is fetched independently and retried in place: no stealing.
    let mut handles = Vec::new();
    for k in 0..n {
        let lo = size * k as u64 / n as u64;
        let hi = size * (k as u64 + 1) / n as u64;
        let t = targets[k % targets.len()].clone();
        let (sk, nt) = (sink.clone(), net.clone());
        handles.push(tokio::spawn(async move {
            pdl_net::fetch_range_retry(nt, t, lo, hi, sk, 6, 0.6).await
        }));
    }
    let mut all_ok = true;
    for h in handles {
        if h.await.map(|r| r.is_err()).unwrap_or(true) {
            all_ok = false;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let ok = all_ok && verify(&outs, size);
    let _ = std::fs::remove_file(&out);
    if ok {
        Some((elapsed, n as u64))
    } else {
        None
    }
}

/// Size sweep on the throttle scenario.
///
/// If the residual gap to the oracle is DETECTION LAG (the EWMA needs several
/// slices to notice a rate collapse, and at the throttled rate those slices are
/// slow to arrive), it is a fixed cost in seconds and its share of the makespan
/// must fall as the object grows. If instead it is a per-byte inefficiency, the
/// ratio stays flat. This distinguishes them.
/// A/B the changepoint detector: identical scenarios, health-ranked victim
/// selection on and off. Reports both so the detector's contribution is measured
/// rather than assumed.
async fn run_hydra_detect(
    sc: &Scenario,
    size: u64,
    conns: usize,
    health_ranking: bool,
) -> Option<(f64, u64)> {
    // The A and B arms need distinct scratch files: detect_ab runs both per rep.
    let tag = format!("playdl_ab_{health_ranking}");
    run_hydra_with(sc, size, conns, 20, &tag, |s| {
        s.with_health_ranking(health_ranking)
    })
    .await
}

pub async fn detect_ab(reps: usize) {
    println!("scenario,size_mb,detector,rep,oracle_s,measured_s,ratio,excess_s,requests");
    for sc in scenarios() {
        for mb in [12u64, 24, 48] {
            let size = mb * 1024 * 1024;
            let oracle = fluid_oracle(&sc, size);
            for rep in 1..=reps {
                for on in [false, true] {
                    if let Some((t, r)) = run_hydra_detect(&sc, size, 2, on).await {
                        println!(
                            "{},{mb},{},{rep},{oracle:.3},{t:.3},{:.3},{:.3},{r}",
                            sc.name,
                            if on { "cusum" } else { "ewma-only" },
                            t / oracle,
                            t - oracle
                        );
                    }
                }
            }
        }
    }
}

pub async fn size_sweep() {
    println!("scenario,size_mb,oracle_s,measured_s,ratio,excess_s,requests");
    for sc in scenarios() {
        if sc.name != "throttle_midway" && sc.name != "stationary_het" {
            continue;
        }
        for mb in [12u64, 24, 48, 96, 192] {
            let size = mb * 1024 * 1024;
            let oracle = fluid_oracle(&sc, size);
            if let Some((t, r)) = run_hydra_tick(&sc, size, 2, 20).await {
                println!(
                    "{},{mb},{oracle:.3},{t:.3},{:.3},{:.3},{r}",
                    sc.name,
                    t / oracle,
                    t - oracle
                );
            }
        }
    }
}

/// Repair latency sweep: how much of the residual gap to the oracle is the
/// harness's tick period rather than the algorithm?
pub async fn tick_sweep() {
    const SIZE: u64 = 24 * 1024 * 1024;
    println!("scenario,tick_ms,oracle_s,measured_s,ratio,requests");
    for sc in scenarios() {
        let oracle = fluid_oracle(&sc, SIZE);
        for tick in [40u64, 20, 8, 3, 1] {
            if let Some((t, r)) = run_hydra_tick(&sc, SIZE, 2, tick).await {
                println!(
                    "{},{tick},{oracle:.3},{t:.3},{:.3},{r}",
                    sc.name,
                    t / oracle
                );
            } else {
                println!("{},{tick},{oracle:.3},,,", sc.name);
            }
        }
    }
}

pub async fn cross_validate() {
    const SIZE: u64 = 24 * 1024 * 1024;
    const CONNS: usize = 2;
    println!("scenario,policy,oracle_s,measured_s,ratio,requests,completed");
    for sc in scenarios() {
        let oracle = fluid_oracle(&sc, SIZE);
        match run_hydra(&sc, SIZE, CONNS).await {
            Some((t, r)) => println!(
                "{},HYDRA preemptive,{oracle:.3},{t:.3},{:.3},{r},1",
                sc.name,
                t / oracle
            ),
            None => println!("{},HYDRA preemptive,{oracle:.3},,,,0", sc.name),
        }
        match run_equal_static(&sc, SIZE, CONNS).await {
            Some((t, r)) => println!(
                "{},Equal static policy,{oracle:.3},{t:.3},{:.3},{r},1",
                sc.name,
                t / oracle
            ),
            None => println!("{},Equal static policy,{oracle:.3},,,,0", sc.name),
        }
    }
}
