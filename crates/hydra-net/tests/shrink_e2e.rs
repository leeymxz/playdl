//! Range preemption must be free on the wire, not just in the theory.
//!
//! The scheduler's central claim is that shrinking a laggard's range costs
//! nothing, because an HTTP range request names both ends and the far end is
//! enforced by the client. Every other test in this suite verifies the delivered
//! FILE, and a file is byte-exact whether a span arrived once or three times â€?
//! the duplicate copy is written over the first and no client-side check can see
//! it. So the property that makes the claim true is invisible from the client and
//! has to be measured at the origin: how many payload bytes did the server put on
//! the wire for a transfer of a known size?
//!
//! That number is what the repair storm was made of. With the victim's fetch loop
//! bounded by the `hi` it captured at spawn, each repair made the stolen span
//! travel twice; the duplicate traffic slowed the honest connections, which read
//! as fresh divergence, which triggered more repairs.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::{run_transfer, Target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn tgt(port: u16) -> Target {
    Target::direct("127.0.0.1", port, "/obj")
}

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

/// A transfer that provokes repairs must still put each byte on the wire once.
///
/// The scenario is the one the storm was measured in: several connections to a
/// single origin whose aggregate rate is fixed, so the connections contend and
/// their finish times diverge, so repairs fire. Whatever the scheduler decides,
/// the origin's served-byte count is the audit â€?a client that re-fetches a
/// preempted span shows up here as served > SIZE even though its output file is
/// perfect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_preempted_span_is_never_fetched_twice() {
    const SIZE: u64 = 6 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    // One origin, one aggregate rate: the connections share it, which is what
    // makes their projected finishes drift apart and repair fire.
    let (port, ctl) = net.spawn(SIZE, 6 * 1024 * 1024);

    let out = std::env::temp_dir().join("hydra_shrink_e2e.bin");
    let outs = out.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&out);

    let sched = Scheduler::new(
        SIZE,
        vec![Source {
            gamma_est: 1.5e6,
            delta_est: 0.01,
            ..Default::default()
        }],
        &[6],
    )
    .with_stall_timeout(5.0);

    let (_elapsed, reqs) = run_transfer(net.clone(), vec![tgt(port)], &[6], SIZE, &outs, sched)
        .await
        .expect("transfer must complete");

    verify(&outs, SIZE).expect("delivered file must be byte-exact");

    let served = ctl.served.load(Ordering::Relaxed);
    // The file being correct is necessary but not sufficient; this is the part
    // that was silently false. Some slack is unavoidable: when the bound drops,
    // the server is already sending toward the old end, so bytes in flight are
    // discarded. That waste is bounded by the receive window and does NOT scale
    // with the size of the span given away, which is exactly the difference
    // between preempting a range and re-requesting it.
    //
    // The bound is calibrated to DISCRIMINATE, which is the only kind of bound
    // worth asserting here. Measured over 6 runs of this exact scenario (n=6, 18
    // repairs each): 37.6-61.6 KB of duplicate traffic with the shrink honoured,
    // 111.9-287.0 KB with it ignored. 96 KiB sits above the first range and below
    // the second, so this test fails if the transport ever stops honouring
    // Action::Shrink.
    //
    // Note the hermetic harness UNDERSTATES the defect: the duplex pipe is 64 KiB,
    // so a victim streaming a span it no longer owns back-pressures almost
    // immediately, where a real bandwidth-delay product lets it run. The
    // live-network cost is correspondingly larger.
    let slack = 96 * 1024;
    assert!(
        served <= SIZE + slack,
        "origin served {served} payload bytes for a {SIZE}-byte object across \
         {reqs} requests ({} bytes of duplicate traffic, slack {slack}). A shrunk \
         connection is still streaming the span it gave away.",
        served.saturating_sub(SIZE)
    );
    assert!(
        served >= SIZE,
        "origin served {served} for a {SIZE}-byte object: fewer bytes than the \
         object contains means the file was assembled from somewhere else"
    );
    let _ = std::fs::remove_file(&out);
}

/// The same audit with the object handed out in one range per connection and a
/// mid-transfer collapse, which is the case repair exists for.
///
/// A collapse makes repair CORRECT rather than spurious: the victim genuinely
/// cannot finish, and moving its tail is the right call. The invariant under test
/// is unchanged â€?the tail must be fetched by exactly one of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repair_after_a_real_collapse_does_not_duplicate_the_tail() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (fast, fctl) = net.spawn(SIZE, 4 * 1024 * 1024);
    let (slow, sctl) = net.spawn(SIZE, 4 * 1024 * 1024);

    // Collapse the second origin shortly after the transfer starts.
    let sctl2 = sctl.clone();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(120)).await;
        sctl2.rate.store(48 * 1024, Ordering::Relaxed);
    });

    let out = std::env::temp_dir().join("hydra_shrink_collapse.bin");
    let outs = out.to_string_lossy().to_string();
    let _ = std::fs::remove_file(&out);

    let src = |g: f64| Source {
        gamma_est: g,
        delta_est: 0.01,
        ..Default::default()
    };
    let sched = Scheduler::new(SIZE, vec![src(2e6), src(2e6)], &[2, 2]).with_stall_timeout(5.0);
    run_transfer(
        net.clone(),
        vec![tgt(fast), tgt(slow)],
        &[2, 2],
        SIZE,
        &outs,
        sched,
    )
    .await
    .expect("transfer must complete despite the collapse");

    verify(&outs, SIZE).expect("delivered file must be byte-exact");

    let served = fctl.served.load(Ordering::Relaxed) + sctl.served.load(Ordering::Relaxed);
    // Calibrated like the test above, but this scenario is noisier and its floor
    // is higher: a real collapse means the victim is genuinely mid-body when its
    // tail moves, so more in-flight bytes are discarded than in the stationary
    // case. Observed up to 67 584 bytes with the shrink honoured, which is why a
    // one-window (65 536) bound was flaky here and 64 KiB per connection is the
    // honest allowance. Kept as a guard against unbounded duplication rather than
    // as a tight discriminator; the stationary test above is the discriminator.
    let slack = 192 * 1024;
    assert!(
        served <= SIZE + slack,
        "origins served {served} payload bytes for a {SIZE}-byte object \
         ({} duplicated, slack {slack})",
        served.saturating_sub(SIZE)
    );
    let _ = std::fs::remove_file(&out);
}
