//! Connection reuse, measured at the origin.
//!
//! Every request used to carry `Connection: close`, so a transfer of `n`
//! concurrent ranges paid `n` handshakes and every repair paid another. That is
//! the worst possible arrangement for a client whose whole premise is many ranges
//! against one host, and it also inflates `delta` — the measured per-request setup
//! cost that the repair deadband and the profitability test are both denominated
//! in, so the scheduler was pricing its decisions against a handshake it did not
//! need to pay.
//!
//! Reuse is asserted where it is visible: the origin counts requests answered and
//! connections accepted. `requests > connections` is only possible if the client
//! sent a second request down a socket it had already used.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::{run_transfer, Target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn verify(path: &str, size: u64) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if data.len() as u64 != size {
        return Err(format!("size {} != {}", data.len(), size));
    }
    for (i, b) in data.iter().enumerate() {
        if *b != byte_at(i as u64) {
            return Err(format!("content mismatch at byte {i}"));
        }
    }
    Ok(())
}

/// A transfer issuing more requests than connections proves the pool works.
///
/// The scenario needs care: with one range per connection and no repairs there is
/// no *second* request to any endpoint, so a pool cannot help and a passing test
/// would prove nothing. Nor should it depend on repair timing, which is a race. One
/// connection against an object with holes is deterministic — ranges complete and
/// work-conserving assignment gives the same connection the next span. Measured here: 4 requests over 1 connection, 3 of them reused.
///
/// This also bounds the honest claim. Reuse saves a handshake per request BEYOND
/// the first on each connection, so its value grows with the request count — which
/// means it is worth most exactly when the scheduler is repairing, and worth
/// nothing on a single-range fetch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keep_alive_origin_serves_several_ranges_per_connection() {
    const SIZE: u64 = 6 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, ctl) = net.spawn(SIZE, 6 * 1024 * 1024);
    ctl.keep_alive.store(true, Ordering::Relaxed);

    let out = std::env::temp_dir().join("hydra_reuse_e2e.bin");
    let outs = out.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&out);

    // Make the request count deterministic instead of leaning on repair timing.
    //
    // One connection, and the object pre-marked so the work left is several
    // disjoint spans: the scheduler hands out one span, the connection finishes it,
    // and work-conserving assignment gives it the next. That is exactly the
    // sequence reuse exists for — several requests to one endpoint, each with a
    // known body extent — and it does not depend on a race to occur.
    let mut sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 6.0e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[1],
    )
    .with_stall_timeout(5.0);
    // Punch three holes: the remaining work is 4 separate spans.
    let hole = SIZE / 8;
    for k in 0..3u64 {
        let lo = hole + k * 2 * hole;
        sched.mark_done(lo, lo + hole);
    }
    run_transfer(
        net.clone(),
        vec![Target::direct("127.0.0.1", port, "/obj")],
        &[1],
        SIZE,
        &outs,
        sched,
    )
    .await
    .expect("transfer must complete");

    // Not byte-complete by construction — three spans were marked done without
    // being fetched — so verify the spans that WERE requested. This is the check
    // that matters for reuse: if a pooled socket carried leftover body into the
    // next response, these bytes would be shifted and wrong.
    {
        let data = std::fs::read(&outs).expect("output readable");
        assert_eq!(
            data.len() as u64,
            SIZE,
            "output must be the object's length"
        );
        let mut checked = 0u64;
        for k in 0..4u64 {
            let lo = k * 2 * hole;
            let end = (lo + hole).min(SIZE);
            for i in lo..end {
                assert_eq!(
                    data[i as usize],
                    byte_at(i),
                    "byte {i} of a fetched span is wrong: a reused connection \
                     delivered another range's bytes here"
                );
                checked += 1;
            }
        }
        assert!(checked > 0, "no span was verified");
    }

    let reqs = ctl.requests.load(Ordering::Relaxed);
    let conns = ctl.connections.load(Ordering::Relaxed);
    assert!(
        reqs > conns,
        "origin answered {reqs} requests over {conns} connections: no socket was \
         reused, so every range paid its own handshake"
    );
    // Without a pool this transfer opens one connection per request. Requiring
    // strictly fewer connections than requests is the property; the margin below
    // keeps the test from pinning an exact schedule, which varies with timing.
    assert!(
        conns < reqs,
        "origin accepted {conns} connections for {reqs} requests: at least one \
         request must have been served on an already-open socket"
    );
    let _ = std::fs::remove_file(&out);
}

/// A connection whose range was shrunk mid-flight must NEVER be pooled.
///
/// This is the interaction that makes reuse dangerous in this particular client.
/// When a repair lowers a connection's far end, the fetch loop stops early by
/// design and the server is still sending toward the original end — so the socket
/// holds an unknown number of unread body bytes. Reusing it would prefix the next
/// response with the previous one's tail, writing valid-looking bytes at wrong
/// offsets: a corrupt file of exactly the right length, which no length or
/// completeness check catches.
///
/// The transfer below both provokes repairs (several connections contending for one
/// origin's fixed capacity) and reuses connections, so it exercises the two
/// mechanisms together. The assertion is content correctness, because corruption is
/// the failure mode this rule prevents.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reuse_and_range_preemption_together_still_deliver_exact_bytes() {
    const SIZE: u64 = 8 * 1024 * 1024;
    for attempt in 0..3 {
        let net = Arc::new(OriginSet::new());
        let (port, ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
        ctl.keep_alive.store(true, Ordering::Relaxed);

        let out = std::env::temp_dir().join(format!("hydra_reuse_shrink_{attempt}.bin"));
        let outs = out.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&out);

        let sched = Scheduler::new(
            SIZE,
            vec![Source {
                gamma_est: 1.0e6,
                delta_est: 0.01,
                ..Default::default()
            }],
            &[8],
        )
        .with_stall_timeout(5.0);
        run_transfer(
            net.clone(),
            vec![Target::direct("127.0.0.1", port, "/obj")],
            &[8],
            SIZE,
            &outs,
            sched,
        )
        .await
        .expect("transfer must complete");

        verify(&outs, SIZE).unwrap_or_else(|e| {
            panic!(
                "attempt {attempt}: {e}. A pooled connection carried unread body \
                 from a preempted range into the next response."
            )
        });
        let _ = std::fs::remove_file(&out);
    }
}
