//! Real-network benchmark: test scheduler against actual HTTP origins.
//!
//! Benchmark features:
//! * TCP connection setup and congestion control on every connection;
//! * Real round-trip times and packet dynamics;
//! * Multi-origin downloads from independent hosts serving identical files;
//! * Comparison against single-stream baseline transfers.

use pdl_core::{Admission, Admit, DeltaEstimator, Scheduler, Source};
use pdl_net::{fetch_range_retry, run_transfer_tick, SparseSink, Target, TcpConnector};
use std::sync::Arc;
use std::time::Instant;

/// A real object, reachable over plain HTTP, with a strong validator.
pub struct RealObject {
    pub name: &'static str,
    pub size: u64,
    pub path: &'static str,
    /// Origin authorities serving byte-identical copies (verified by ETag).
    pub origins: &'static [&'static str],
}

/// The size ladder for the scaling experiment.
///
/// Five sizes spanning 33x (0.42 MB to 13.94 MB), every one served by both CRAN
/// mirrors with byte-identical strong ETags — verified against both mirrors before
/// the run, since two mirrors that disagree cannot be assembled from and the
/// experiment would be measuring the wrong thing.
///
/// The sizes below are the measured `Content-Length`, not a figure read off a
/// rounded display: four of the five were initially wrong by a few hundred to a few
/// thousand bytes because they were derived from a two-decimal MB printout, which a
/// byte-exact digest check would have caught only after a wasted run.
///
/// The range is bounded above by what these mirrors host as a single source
/// tarball. That is a real limit on the claim and it is stated in the analysis
/// rather than papered over: nothing here tests a multi-gigabyte object, where the
/// per-request setup cost becomes negligible against transfer time and the
/// scheduler's advantage should be at its largest.
pub fn objects() -> Vec<RealObject> {
    vec![
        RealObject {
            name: "jsonlite_1.7.2.tar.gz",
            size: 421_716,
            path: "/src/contrib/Archive/jsonlite/jsonlite_1.7.2.tar.gz",
            origins: &["cran.r-project.org", "cloud.r-project.org"],
        },
        RealObject {
            name: "Rcpp_1.0.9.tar.gz",
            size: 2_957_812,
            path: "/src/contrib/Archive/Rcpp/Rcpp_1.0.9.tar.gz",
            origins: &["cran.r-project.org", "cloud.r-project.org"],
        },
        RealObject {
            name: "data.table_1.14.2.tar.gz",
            size: 5_301_817,
            path: "/src/contrib/Archive/data.table/data.table_1.14.2.tar.gz",
            origins: &["cran.r-project.org", "cloud.r-project.org"],
        },
        RealObject {
            name: "spatstat_1.64-1.tar.gz",
            size: 7_943_393,
            path: "/src/contrib/Archive/spatstat/spatstat_1.64-1.tar.gz",
            origins: &["cran.r-project.org", "cloud.r-project.org"],
        },
        RealObject {
            name: "BH_1.81.0-1.tar.gz",
            size: 13_938_979,
            path: "/src/contrib/Archive/BH/BH_1.81.0-1.tar.gz",
            origins: &["cran.r-project.org", "cloud.r-project.org"],
        },
    ]
}

fn proxy() -> Option<(String, u16)> {
    let raw = std::env::var("http_proxy")
        .or_else(|_| std::env::var("HTTP_PROXY"))
        .ok()?;
    let rest = raw.split("://").last()?.trim_end_matches('/');
    let (h, p) = rest.rsplit_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn targets(obj: &RealObject, n_origins: usize) -> Vec<Target> {
    let (ph, pp) = proxy().expect("http_proxy must be set");
    obj.origins
        .iter()
        .take(n_origins)
        .map(|o| Target::via_proxy(&ph, pp, o, obj.path))
        .collect()
}

/// Digest of the assembled file, so a mis-assembly cannot pass as a success.
fn digest(path: &str, expect_len: u64) -> Option<String> {
    let d = std::fs::read(path).ok()?;
    if d.len() as u64 != expect_len {
        return None;
    }
    // FNV-1a: enough to detect misplaced or duplicated ranges; the reference
    // digest is the single-stream fetch of the same bytes in the same run.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in &d {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    Some(format!("{h:016x}"))
}

/// Single sequential stream baseline.
async fn run_single(obj: &RealObject) -> Option<(f64, String)> {
    let t = targets(obj, 1).remove(0);
    let out = std::env::temp_dir().join(format!("hydra_real_single_{}", obj.name));
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&outs, obj.size).ok()?);
    let c = Arc::new(TcpConnector);
    let t0 = Instant::now();
    let ok = fetch_range_retry(c, t, 0, obj.size, sink.clone(), 4, 30.0)
        .await
        .is_ok();
    let el = t0.elapsed().as_secs_f64();
    drop(sink);
    let d = digest(&outs, obj.size);
    let _ = std::fs::remove_file(&out);
    if ok {
        d.map(|d| (el, d))
    } else {
        None
    }
}

/// HYDRA, `conns` connections per origin across `n_origins` real hosts.
async fn run_hydra(obj: &RealObject, n_origins: usize, conns: usize) -> Option<(f64, u64, String)> {
    let tg = targets(obj, n_origins);
    let per: Vec<usize> = (0..tg.len()).map(|_| conns).collect();
    let sources: Vec<Source> = tg
        .iter()
        .map(|_| Source {
            // Deliberately uninformed: the scheduler must discover real rates.
            gamma_est: 1.0e6,
            delta_est: 0.15,
            ..Default::default()
        })
        .collect();
    let out = std::env::temp_dir().join(format!("hydra_real_{}_{}x{}", obj.name, n_origins, conns));
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(obj.size, sources, &per).with_stall_timeout(8.0);
    let c = Arc::new(TcpConnector);
    let t0 = Instant::now();
    let r = run_transfer_tick(c, tg, &per, obj.size, &outs, sched, 20)
        .await
        .ok();
    let el = t0.elapsed().as_secs_f64();
    let d = digest(&outs, obj.size);
    let _ = std::fs::remove_file(&out);
    match (r, d) {
        (Some((_, reqs)), Some(d)) => Some((el, reqs, d)),
        _ => None,
    }
}

/// HYDRA with adaptive concurrency discovery based on measured marginal goodput.
async fn run_hydra_adaptive(obj: &RealObject, max_per_origin: usize) -> Option<AdaptiveRun> {
    let t_start = Instant::now();
    let all = targets(obj, obj.origins.len());

    // --- probe phase: measure delta and marginal value of concurrency ---
    // The probe holds total bytes fixed across levels to measure true marginal scaling.
    const PROBE_TOTAL: u64 = 1024 * 1024;
    let mut adm = Admission::new(0.15, max_per_origin);
    let mut dest = DeltaEstimator::new(0.05);
    let c = Arc::new(TcpConnector);
    let mut level = 1usize;
    let mut probe_off = 0u64;
    loop {
        // Distinct byte region per level so no level is served from a warm cache
        // that the previous level populated.
        let base = probe_off % (obj.size.saturating_sub(PROBE_TOTAL * 2)).max(1);
        let slice = PROBE_TOTAL / level as u64;
        let probe_out = std::env::temp_dir().join(format!("hydra_probe_{}", obj.name));
        let po = probe_out.to_string_lossy().to_string();
        let sink = Arc::new(SparseSink::create(&po, obj.size).ok()?);
        let t0 = Instant::now();
        let mut hs = Vec::new();
        for k in 0..level {
            let lo = base + k as u64 * slice;
            let t = all[k % all.len()].clone();
            let (cc, sk) = (c.clone(), sink.clone());
            hs.push(tokio::spawn(async move {
                let s = Instant::now();
                let r = fetch_range_retry(cc, t, lo, lo + slice, sk, 2, 25.0).await;
                (r.is_ok(), s.elapsed().as_secs_f64())
            }));
        }
        let mut okbytes = 0u64;
        let mut per_req = Vec::new();
        for h in hs {
            if let Ok((ok, el)) = h.await {
                if ok {
                    okbytes += slice;
                    per_req.push(el);
                }
            }
        }
        let el = t0.elapsed().as_secs_f64().max(1e-3);
        drop(sink);
        let _ = std::fs::remove_file(&probe_out);
        // delta: a request's wall time minus the time its bytes should have taken
        // at the observed aggregate rate. That isolates setup from streaming.
        if okbytes > 0 {
            let rate = okbytes as f64 / el;
            for t in &per_req {
                dest.observe((t - slice as f64 / rate.max(1.0)).max(1e-3));
            }
        }
        probe_off = base + PROBE_TOTAL;
        match adm.observe(okbytes as f64 / el) {
            Admit::Stop => break,
            Admit::Add => level += 1,
        }
    }
    let n_conns = adm.settled().unwrap_or(1).max(1);
    let delta = dest.get();
    let probe_s = t_start.elapsed().as_secs_f64();
    let t_xfer = Instant::now();

    // --- transfer phase, using what the probe learned ---
    // Concurrency is spread across origins first, then allocated per origin.
    let n_origins = n_conns.min(all.len()).max(1);
    let per_origin = n_conns.div_ceil(n_origins);
    let tg: Vec<Target> = all.into_iter().take(n_origins).collect();
    let per: Vec<usize> = (0..tg.len()).map(|_| per_origin).collect();
    let sources: Vec<Source> = tg
        .iter()
        .map(|_| Source {
            gamma_est: adm.best_goodput().max(1.0e5) / n_conns as f64,
            delta_est: delta,
            ..Default::default()
        })
        .collect();
    let out = std::env::temp_dir().join(format!("hydra_adaptive_{}", obj.name));
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(obj.size, sources, &per)
        .with_stall_timeout((8.0 * delta).max(4.0))
        .with_theta_scale(4.0);
    let r = run_transfer_tick(c, tg, &per, obj.size, &outs, sched, 20)
        .await
        .ok();
    let xfer_s = t_xfer.elapsed().as_secs_f64();
    let d = digest(&outs, obj.size);
    let _ = std::fs::remove_file(&out);
    match (r, d) {
        (Some((_, reqs)), Some(d)) => Some(AdaptiveRun {
            total_s: probe_s + xfer_s,
            probe_s,
            xfer_s,
            requests: reqs,
            digest: d,
            n_conns,
            delta,
        }),
        _ => None,
    }
}

/// Result of an adaptive run. Probe and transfer are reported separately because
/// the probe is a per-PATH cost, not a per-transfer one: a real downloader caches
/// what it learned about a host, so `xfer_s` is the steady-state figure and
/// `total_s` is the cold-start figure. Both are reported; neither is hidden.
pub struct AdaptiveRun {
    pub total_s: f64,
    pub probe_s: f64,
    pub xfer_s: f64,
    pub requests: u64,
    pub digest: String,
    pub n_conns: usize,
    pub delta: f64,
}

pub async fn bench(reps: usize) {
    if proxy().is_none() {
        eprintln!("http_proxy not set; a direct-origin run needs DNS, which is unavailable here");
        return;
    }
    println!("object,size_mb,config,rep,elapsed_s,throughput_mbps,requests,digest,ok,learned_conns,learned_delta_s,probe_s,xfer_s");
    for obj in objects() {
        let mb = obj.size as f64 / 1_048_576.0;
        for rep in 1..=reps {
            // Baseline first in each rep, so both see similar network conditions.
            match run_single(&obj).await {
                Some((el, d)) => println!(
                    "{},{mb:.2},single stream (baseline),{rep},{el:.3},{:.2},1,{d},1,,,,",
                    obj.name,
                    mb / el
                ),
                None => println!(
                    "{},{mb:.2},single stream (baseline),{rep},,,,,0,,,,",
                    obj.name
                ),
            }
            for (no, cn, label) in [
                (1usize, 4usize, "1 origin x 4 conns"),
                (2, 2, "2 origins x 2 conns"),
                (2, 4, "2 origins x 4 conns"),
            ] {
                match run_hydra(&obj, no, cn).await {
                    Some((el, reqs, d)) => println!(
                        "{},{mb:.2},HYDRA {label},{rep},{el:.3},{:.2},{reqs},{d},1,,,,",
                        obj.name,
                        mb / el
                    ),
                    None => println!("{},{mb:.2},HYDRA {label},{rep},,,,,0,,,,", obj.name),
                }
            }
            match run_hydra_adaptive(&obj, 8).await {
                Some(a) => {
                    println!(
                        "{},{mb:.2},HYDRA adaptive cold (probe+transfer),{rep},{:.3},{:.2},{},{},1,{},{:.3},{:.3},{:.3}",
                        obj.name, a.total_s, mb / a.total_s, a.requests, a.digest, a.n_conns,
                        a.delta, a.probe_s, a.xfer_s
                    );
                    println!(
                        "{},{mb:.2},HYDRA adaptive warm (transfer only),{rep},{:.3},{:.2},{},{},1,{},{:.3},{:.3},{:.3}",
                        obj.name, a.xfer_s, mb / a.xfer_s, a.requests, a.digest, a.n_conns,
                        a.delta, a.probe_s, a.xfer_s
                    );
                }
                None => println!(
                    "{},{mb:.2},HYDRA adaptive cold (probe+transfer),{rep},,,,,0,,,,",
                    obj.name
                ),
            }
        }
    }
}
