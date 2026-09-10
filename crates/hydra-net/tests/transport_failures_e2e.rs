//! Transport failures must be acted on when they happen, not rediscovered by a
//! timeout seconds later.
//!
//! Each test here is a way a real server ends a request that the scheduler-driven
//! transport could not see: it spawned a fetch and discarded its result, so a
//! closed socket, a truncated body and a protocol violation were all
//! indistinguishable from a connection that was merely slow. What noticed, in
//! every case, was the stall timeout â€?4 to 45 s of dead air per occurrence.
//!
//! The first case is the one users hit, and the reason they hit it near the end:
//!
//! Every real origin closes idle keep-alive sockets on its own schedule, and it
//! does so silently: a FIN, not a `Connection: close` on the previous response.
//! The client is holding that socket for reuse and cannot tell it apart from a
//! live one until the next request's first read returns zero â€?the write goes
//! through, because only the server's write half is closed.
//!
//! This is the endgame case: near completion, the remaining work is carried by
//! one or two connections, so a single connection that goes silent freezes the
//! whole transfer for a full stall timeout.
//!
//! The second test covers the same defect from the other side: a fetch that fails
//! LOUDLY was discarded by the transport just as completely, so a truncated body
//! also cost a stall timeout before anything re-requested the remainder.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::{run_transfer_tick, Target};
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_origin_that_closes_pooled_connections_does_not_stall_the_transfer() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, ctl) = net.spawn(SIZE, 16 * 1024 * 1024);
    // Keep-alive offered, so the client pools sockets...
    ctl.keep_alive.store(true, Ordering::Relaxed);
    // ...and closed after every single response, so every reuse finds a
    // half-closed socket. The worst case rather than a realistic rate: the
    // question is whether the client RECOVERS at all, and a rare race makes for a
    // test that passes by never reproducing anything.
    ctl.close_after_requests.store(1, Ordering::Relaxed);

    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join("hydra_pool_idle_close.bin");
    let outs = out.to_string_lossy().to_string();

    let sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 8e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[8],
    )
    // The GUI's shipping configuration: the budget is open but only one
    // connection starts active, so the in-band ramp admits the rest. That is what
    // produces a stream of short range requests â€?and therefore pooled-connection
    // reuse â€?instead of one maximal range per connection.
    .with_active_limit(1)
    .with_stall_timeout(3.0);

    let t0 = std::time::Instant::now();
    let (elapsed, reqs) = run_transfer_tick(net.clone(), vec![t], &[8], SIZE, &outs, sched, 20)
        .await
        .expect("a server closing idle keep-alives must not fail the transfer");
    verify(&outs, SIZE).expect("assembled file must match the origin byte for byte");
    let wall = t0.elapsed().as_secs_f64();
    eprintln!(
        "pool-closing origin: {SIZE} B in {elapsed:.2}s over {reqs} requests \
         ({} connections, {} served)",
        ctl.connections.load(Ordering::Relaxed),
        ctl.served.load(Ordering::Relaxed),
    );
    // The object moves at 16 MB/s, so 4 MiB is a quarter-second of transfer. A
    // dead pooled socket that is noticed at the next read costs one redial; one
    // that is noticed only by the stall detector costs 3 s of silence. The
    // threshold sits between the two, so this fails loudly on regression rather
    // than merely getting slower.
    assert!(
        wall < 3.0,
        "transfer took {wall:.1}s: dead pooled connections are being waited out \
         by the stall detector instead of retried"
    );
    let _ = std::fs::remove_file(&out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_truncated_response_is_re_requested_without_waiting_for_the_stall_timeout() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, ctl) = net.spawn(SIZE, 16 * 1024 * 1024);
    // Every third response is cut in half: the CDN node recycled mid-body, the
    // load balancer that drops a connection. The client gets a real error from
    // the fetch â€?which is exactly the point, because the transport used to throw
    // that error away and let the stall detector rediscover it seconds later.
    ctl.truncate_every.store(3, Ordering::Relaxed);

    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join("hydra_truncating_origin.bin");
    let outs = out.to_string_lossy().to_string();

    let sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 8e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[4],
    )
    .with_stall_timeout(3.0);

    let t0 = std::time::Instant::now();
    let (elapsed, reqs) = run_transfer_tick(net.clone(), vec![t], &[4], SIZE, &outs, sched, 20)
        .await
        .expect("an origin that truncates some responses must not fail the transfer");
    verify(&outs, SIZE).expect("assembled file must match the origin byte for byte");
    let wall = t0.elapsed().as_secs_f64();
    let answered = ctl.requests.load(Ordering::Relaxed);
    eprintln!(
        "truncating origin: {SIZE} B in {elapsed:.2}s over {reqs} requests \
         ({answered} answered, {} served)",
        ctl.served.load(Ordering::Relaxed),
    );
    // The test is only meaningful if the origin actually cut something: with four
    // ranges and every third response truncated, the remainders have to be
    // re-requested, so the client must issue more requests than it has ranges.
    assert!(
        answered > 4,
        "the origin never truncated: {answered} responses for 4 ranges"
    );
    assert!(
        wall < 3.0,
        "transfer took {wall:.1}s: a failed fetch is being waited out by the \
         stall detector instead of re-requested"
    );
    let _ = std::fs::remove_file(&out);
}

/// A server that cannot serve ranges must be reported for THAT, quickly.
///
/// The scheduler-driven transport used to discard fetch errors entirely, so a
/// protocol violation it could never recover from looked exactly like slowness:
/// every connection failed instantly, was re-requested on the next tick, failed
/// again, and the only thing that ever ended it was the no-progress deadline â€?
/// which then blamed the symptom ("every source stalled or unreachable") instead
/// of the cause the server had stated on every single response.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_server_that_ignores_ranges_fails_fast_and_says_why() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn_ignoring_ranges(SIZE, 16 * 1024 * 1024);
    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join("hydra_range_ignoring_transfer.bin");
    let outs = out.to_string_lossy().to_string();

    let sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 8e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[4],
    )
    .with_stall_timeout(3.0);

    let t0 = std::time::Instant::now();
    let err = run_transfer_tick(net.clone(), vec![t], &[4], SIZE, &outs, sched, 20)
        .await
        .expect_err("a server answering 200 to mid-object ranges cannot be used");
    let wall = t0.elapsed().as_secs_f64();
    let msg = err.to_string();
    eprintln!("range-ignoring origin: failed in {wall:.2}s: {msg}");
    assert!(
        wall < 5.0,
        "took {wall:.1}s to give up on an unusable server: the failure is being \
         rediscovered by the no-progress deadline instead of reported"
    );
    assert!(
        msg.contains("ignored Range") || msg.contains("Content-Range"),
        "the failure must name what the server did, got: {msg}"
    );
    let _ = std::fs::remove_file(&out);
}
