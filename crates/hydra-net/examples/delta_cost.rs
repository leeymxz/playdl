//! What does a request actually cost, with and without connection reuse?
//!
//! `delta` — the per-request setup cost — is not a cosmetic number here. The
//! repair deadband is floored at it and the profitability test is denominated in
//! it, so `delta` decides how readily the scheduler moves work. With every request
//! carrying `Connection: close`, `delta` was a full handshake on every one.
//!
//! This measures the difference on the in-process origin, which is the LOWER bound
//! on the effect: a duplex pipe has no TCP handshake, no TLS, and no round-trip
//! time, so what is measured here is only the request/response framing and task
//! setup. On a real TLS origin the saved cost is a full TCP handshake plus a TLS
//! handshake — 2-3 round trips — which is where the hundreds of milliseconds the
//! scheduler was pricing against actually live.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::OriginSet;
use pdl_net::{run_transfer, Target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// A transfer over an object with holes, so the scheduler issues several
/// sequential ranges to one endpoint — the shape reuse exists for, and the one
/// that does not depend on repair timing to occur.
async fn run(keep_alive: bool, spans: u64) -> (f64, u64, u64) {
    const SIZE: u64 = 8 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, ctl) = net.spawn(SIZE, 512 * 1024 * 1024);
    ctl.keep_alive.store(keep_alive, Ordering::Relaxed);

    let path = std::env::temp_dir().join("hydra_delta_cost.bin");
    let outs = path.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&path);

    let mut sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 512.0e6,
            delta_est: 0.001,
            ..Default::default()
        }],
        &[1],
    )
    .with_stall_timeout(5.0);
    // Punch holes so the remaining work is `spans` disjoint pieces.
    let piece = SIZE / (2 * spans);
    for k in 0..spans {
        let lo = piece + k * 2 * piece;
        if lo + piece <= SIZE {
            sched.mark_done(lo, lo + piece);
        }
    }

    let t0 = std::time::Instant::now();
    run_transfer(
        net.clone(),
        vec![Target::direct("127.0.0.1", port, "/obj")],
        &[1],
        SIZE,
        &outs,
        sched,
    )
    .await
    .expect("transfer");
    let elapsed = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = std::fs::remove_file(&path);
    (
        elapsed,
        ctl.requests.load(Ordering::Relaxed),
        ctl.connections.load(Ordering::Relaxed),
    )
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // Read the CONNECTION COUNT, not the milliseconds.
    //
    // On the in-process origin a "handshake" is allocating a duplex pipe, so the
    // wall clock here is dominated by the scheduler's 20 ms tick and says almost
    // nothing about the saving. What it does establish is the mechanism: whether
    // several ranges to one endpoint travel over one socket or over one each. The
    // time saved per avoided handshake is a property of the network — one RTT for
    // TCP, two or three more for TLS — and has to be measured against a real origin.
    println!("in-process origin: read the connection count, not the milliseconds\n");
    println!(
        "{:>10}  {:>7}  {:>8}  {:>12}  {:>9}",
        "keep_alive", "spans", "requests", "connections", "ms"
    );
    for &spans in &[4u64, 8, 16] {
        for &ka in &[false, true] {
            // Median of 5, so one scheduling hiccup is not the number.
            let mut ms: Vec<f64> = Vec::new();
            let mut rq = 0;
            let mut cn = 0;
            for _ in 0..5 {
                let (t, r, c) = run(ka, spans).await;
                ms.push(t);
                rq = r;
                cn = c;
            }
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
            println!(
                "{:>10}  {:>7}  {:>8}  {:>12}  {:>8.1}",
                ka,
                spans,
                rq,
                cn,
                ms[ms.len() / 2]
            );
        }
    }
    println!(
        "\nconnections == requests means every range paid its own handshake.\n\
         connections == 1 means the socket was reused for all of them."
    );
}
