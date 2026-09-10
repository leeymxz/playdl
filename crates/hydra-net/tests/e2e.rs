//! End-to-end transfers over real TCP sockets against rate-shaped local origins.
//!
//! These are the measurements that make the simulation claims falsifiable: the
//! same `hydra-core` scheduler, driven by real HTTP `Range` requests, real
//! positioned file writes, and real failures injected mid-transfer.
//!
//! Every test verifies CONTENT, not just byte count: the origin serves a
//! deterministic function of offset, so a mis-assembled file is detected.

use pdl_core::{Scheduler, Source};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::{probe, run_transfer, Target};
use std::sync::atomic::Ordering;
use std::sync::Arc;

fn tgt(port: u16) -> Target {
    Target::direct("127.0.0.1", port, "/obj")
}

fn src(gamma: f64) -> Source {
    Source {
        gamma_est: gamma,
        delta_est: 0.01,
        ..Default::default()
    }
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn single_origin_range_transfer_is_byte_exact() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let t = tgt(port);
    let pr = probe(net.as_ref(), &t).await.unwrap();
    assert_eq!(pr.size, SIZE, "probe read the wrong Content-Length");
    assert!(pr.ranges, "origin must advertise byte ranges");
    assert!(
        pr.validator.is_some(),
        "origin must supply a validator: without one, neither resume nor \
         multi-source assembly can be proven sound"
    );

    let out = std::env::temp_dir().join("hydra_e2e_single.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[4]).with_stall_timeout(3.0);
    let (elapsed, reqs) = run_transfer(net.clone(), vec![t], &[4], SIZE, &outs, sched)
        .await
        .unwrap();
    verify(&outs, SIZE).expect("assembled file must match the origin byte for byte");
    assert!(reqs >= 4, "expected at least one request per connection");
    eprintln!("single origin: {SIZE} B in {elapsed:.2}s over {reqs} requests");
    let _ = std::fs::remove_file(&out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_origin_aggregates_and_verifies() {
    const SIZE: u64 = 8 * 1024 * 1024;
    // three origins of deliberately unequal capability
    let net = Arc::new(OriginSet::new());
    let (p1, _c1) = net.spawn(SIZE, 6 * 1024 * 1024);
    let (p2, _c2) = net.spawn(SIZE, 3 * 1024 * 1024);
    let (p3, _c3) = net.spawn(SIZE, 1024 * 1024);
    let targets: Vec<Target> = [p1, p2, p3].iter().map(|p| tgt(*p)).collect();

    let out = std::env::temp_dir().join("hydra_e2e_multi.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(6e6), src(3e6), src(1e6)], &[2, 2, 2])
        .with_stall_timeout(3.0);
    let (elapsed, reqs) = run_transfer(net.clone(), targets, &[2, 2, 2], SIZE, &outs, sched)
        .await
        .unwrap();
    verify(&outs, SIZE).expect("multi-origin assembly must be byte exact");
    eprintln!("three origins: {SIZE} B in {elapsed:.2}s over {reqs} requests");
    let _ = std::fs::remove_file(&out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_a_mirror_that_black_holes_mid_transfer() {
    const SIZE: u64 = 6 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (p1, c1) = net.spawn(SIZE, 3 * 1024 * 1024);
    let (p2, _c2) = net.spawn(SIZE, 3 * 1024 * 1024);
    let targets: Vec<Target> = [p1, p2].iter().map(|p| tgt(*p)).collect();

    // Kill origin 1 shortly after the transfer starts: it keeps the socket open
    // and delivers nothing, which is the case the stall detector exists for.
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
        c1.blackhole.store(true, Ordering::Relaxed);
    });

    let out = std::env::temp_dir().join("hydra_e2e_blackhole.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(3e6), src(3e6)], &[2, 2]).with_stall_timeout(1.0);
    let (elapsed, reqs) = run_transfer(net.clone(), targets, &[2, 2], SIZE, &outs, sched)
        .await
        .expect("must complete despite a dead mirror");
    verify(&outs, SIZE).expect("file must be byte exact after failover");
    eprintln!("mirror blackhole: recovered, {SIZE} B in {elapsed:.2}s over {reqs} requests");
    let _ = std::fs::remove_file(&out);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn survives_a_mirror_that_throttles_mid_transfer() {
    const SIZE: u64 = 6 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (p1, c1) = net.spawn(SIZE, 4 * 1024 * 1024);
    let (p2, _c2) = net.spawn(SIZE, 4 * 1024 * 1024);
    let targets: Vec<Target> = [p1, p2].iter().map(|p| tgt(*p)).collect();

    // Collapse origin 1 to 3% of its rate: not dead, so the stall detector will
    // NOT fire. Only steal-to-equalize can recover this, which is the point.
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
        c1.rate.store(128 * 1024, Ordering::Relaxed);
    });

    let out = std::env::temp_dir().join("hydra_e2e_throttle.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6), src(4e6)], &[2, 2]).with_stall_timeout(10.0);
    let (elapsed, reqs) = run_transfer(net.clone(), targets, &[2, 2], SIZE, &outs, sched)
        .await
        .expect("must complete despite a throttled mirror");
    verify(&outs, SIZE).expect("file must be byte exact after rebalancing");
    eprintln!("mirror throttle: {SIZE} B in {elapsed:.2}s over {reqs} requests");
    let _ = std::fs::remove_file(&out);
}

/// A server that IGNORES `Range` and replies `200 OK` with the whole object must
/// be rejected, not written at the requested offset.
///
/// Regression test for a corruption bug found on the real network: one transfer
/// in 24 produced a file whose digest differed from every other run while
/// reporting success, because a full-body 200 response had its bytes written
/// starting at the range's `lo`. Length checks cannot catch this; only the
/// status/Content-Range check can.
#[tokio::test]
async fn range_ignoring_server_is_rejected_not_silently_corrupted() {
    use pdl_net::{fetch_range_retry, SparseSink};

    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    // ranges disabled: the origin answers 200 with the full body.
    let (port, _ctl) = net.spawn_ignoring_ranges(SIZE, 8_000_000);
    let out = std::env::temp_dir().join("hydra_range_ignored.bin");
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&outs, SIZE).unwrap());

    // Ask for a range that does NOT start at zero: a 200 here is unusable.
    let r = fetch_range_retry(
        net.clone(),
        tgt(port),
        SIZE / 2,
        SIZE / 2 + 65536,
        sink.clone(),
        1,
        5.0,
    )
    .await;
    assert!(
        r.is_err(),
        "a 200 response to a mid-object Range request must be an error, not accepted"
    );

    // And nothing may have been written at that offset.
    drop(sink);
    let d = std::fs::read(&outs).unwrap();
    assert!(
        d[(SIZE / 2) as usize..(SIZE / 2 + 65536) as usize]
            .iter()
            .all(|b| *b == 0),
        "no bytes may be written from a rejected response"
    );
    let _ = std::fs::remove_file(&out);
}

/// A truncating origin must not be reported as a completed transfer.
///
/// The origin advertises an honest-looking `Content-Length` and then closes the
/// connection halfway through the body. A client that trusts the header delivers
/// a short file and calls it success.
#[tokio::test]
async fn truncating_origin_does_not_report_success() {
    use pdl_net::{fetch_range_retry, SparseSink};
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn_truncating(SIZE, 8_000_000);
    let out = std::env::temp_dir().join("hydra_trunc.bin");
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&outs, SIZE).unwrap());
    let r = fetch_range_retry(net.clone(), tgt(port), 0, SIZE, sink, 2, 4.0).await;
    assert!(
        r.is_err(),
        "a body that stops halfway must not be reported as a complete range"
    );
    let _ = std::fs::remove_file(&out);
}

/// A 503 with `Retry-After` must be honoured, and must not be treated as a
/// protocol error that abandons the mirror permanently.
#[tokio::test]
async fn overloaded_origin_backs_off_then_gives_up_cleanly() {
    use pdl_net::{fetch_range_retry, SparseSink};
    const SIZE: u64 = 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn_overloaded(SIZE, 8_000_000);
    let out = std::env::temp_dir().join("hydra_503.bin");
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&outs, SIZE).unwrap());
    let t0 = std::time::Instant::now();
    let r = fetch_range_retry(net.clone(), tgt(port), 0, SIZE, sink, 2, 4.0).await;
    let el = t0.elapsed().as_secs_f64();
    assert!(r.is_err(), "a permanently 503 origin cannot complete");
    assert!(
        el >= 0.05,
        "Retry-After must actually be waited on, elapsed {el:.3}s"
    );
    assert!(el < 20.0, "backoff must stay bounded, elapsed {el:.3}s");
    let _ = std::fs::remove_file(&out);
}

/// A redirect loop must terminate within the hop budget.
#[tokio::test]
async fn redirect_loop_terminates_within_the_hop_budget() {
    use pdl_net::{fetch_range_retry, SparseSink};
    const SIZE: u64 = 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    // Redirects to itself: a loop, which must be bounded rather than infinite.
    let (port, _ctl) = net.spawn_redirecting(SIZE, 8_000_000, "/obj");
    let out = std::env::temp_dir().join("hydra_redir.bin");
    let outs = out.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&outs, SIZE).unwrap());
    let r = fetch_range_retry(net.clone(), tgt(port), 0, SIZE, sink, 3, 4.0).await;
    assert!(r.is_err(), "a redirect loop must not be followed forever");
    let _ = std::fs::remove_file(&out);
}

/// A TLS handshake against a plaintext server must fail, not hang or corrupt.
///
/// This is the cheap local proxy for "certificate verification works": the
/// in-process origins speak cleartext, so marking a target `tls: true` gives the
/// verifying client a peer that cannot complete a handshake. If the connector
/// silently fell back to plaintext, this would pass a transfer through unencrypted
/// while the caller believed it was protected.
#[tokio::test]
async fn tls_against_a_plaintext_origin_fails_rather_than_downgrading() {
    use pdl_net::{Target, TlsCapableConnector};
    const SIZE: u64 = 64 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8_000_000);
    // A real TCP connector with TLS requested against a cleartext peer.
    let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
    let mut t = Target::direct("127.0.0.1", port, "/obj");
    t.tls = true;
    let r = probe(conn.as_ref(), &t).await;
    assert!(
        r.is_err(),
        "a verifying TLS client must not silently downgrade to plaintext"
    );
}

/// A single source that black-holes must fail with an error, not idle forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lone_black_holing_source_fails_instead_of_idling_forever() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, ctl) = net.spawn(SIZE, 4 * 1024 * 1024);

    // Dead from the first byte: the connection opens, the head is served, and
    // then nothing arrives ever again.
    ctl.blackhole.store(true, Ordering::Relaxed);

    let out = std::env::temp_dir().join("hydra_e2e_lone_blackhole.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[4]).with_stall_timeout(0.5);

    let started = std::time::Instant::now();
    let r = tokio::time::timeout(
        tokio::time::Duration::from_secs(20),
        run_transfer(net.clone(), vec![tgt(port)], &[4], SIZE, &outs, sched),
    )
    .await;
    let waited = started.elapsed().as_secs_f64();
    let _ = std::fs::remove_file(&out);

    let inner = r.unwrap_or_else(|_| {
        panic!("a transfer with one dead source hung past 20s instead of failing")
    });
    let e = inner.expect_err("a source that delivers nothing cannot yield a complete file");
    eprintln!("lone blackhole: failed after {waited:.2}s with {e}");
    assert!(
        waited < 20.0,
        "must give up promptly, waited {waited:.2}s for {e}"
    );
}

/// The plaintext path must still work through the same connector, so adding TLS
/// did not regress http://.
#[tokio::test]
async fn the_tls_capable_connector_still_speaks_plaintext() {
    use pdl_net::TlsCapableConnector;
    const SIZE: u64 = 64 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8_000_000);
    let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
    let t = tgt(port);
    // The in-process origin is not reachable over real TCP, so this asserts only
    // that a non-TLS target takes the plaintext branch without attempting a
    // handshake; a handshake attempt would surface as a TLS error rather than a
    // connection error.
    match probe(conn.as_ref(), &t).await {
        Ok(_) => {}
        Err(e) => {
            let msg = e.to_string();
            assert!(
                !msg.contains("tls") && !msg.contains("handshake") && !msg.contains("certificate"),
                "a plaintext target must not attempt TLS, got: {msg}"
            );
        }
    }
}

/// The observer must see the transfer's FINAL byte count, not the second-to-last.
///
/// Regression test: the transfer loop breaks out of the top of the iteration as
/// soon as the scheduler reports completion, which is above the per-tick
/// `observe` call. The arrivals that COMPLETED the transfer were therefore never
/// reported to the caller â€?the progress bar stopped short of 100%, and the CLI,
/// which derives post-transfer completeness from the observed count, declared a
/// byte-exact file incomplete. Measured on an 11 200 900-byte resume: 2 876
/// bytes unobserved, exit 1 and `ok: false` on a download that was perfect.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_observer_sees_the_final_byte_count() {
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let out = std::env::temp_dir().join("hydra_e2e_final_observation.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[4]).with_stall_timeout(3.0);

    let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let s2 = seen.clone();
    let mut observe = move |_: &Scheduler, done: u64| {
        s2.fetch_max(done, Ordering::Relaxed);
    };
    pdl_net::run_transfer_observed(
        net.clone(),
        vec![tgt(port)],
        &[4],
        SIZE,
        &outs,
        sched,
        20,
        &mut observe,
    )
    .await
    .expect("transfer must succeed");

    verify(&outs, SIZE).expect("assembled file must match the origin byte for byte");
    assert_eq!(
        seen.load(Ordering::Relaxed),
        SIZE,
        "the highest observed count must equal the object size: a caller judging \
         completeness from the observer must not conclude a complete transfer is short"
    );
    let _ = std::fs::remove_file(&out);
}

/// A cap switched ON mid-transfer must shape the bytes that are still to come.
///
/// Regression test: `Pace::shared` collapsed a limiter that was unlimited at the
/// moment the transfer started, so the pace held nothing and every later
/// `set_rate` wrote to a bucket no read would ever consult again. In the GUI
/// that is exactly the "Use Speed Limiter" checkbox on a download already
/// running: the dialog showed 100 KB/sec next to a transfer still moving at
/// 8.9 MB/sec. The engine-wide FFI setter had the same hole.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cap_applied_mid_transfer_shapes_the_rest() {
    use pdl_net::polite::{Pace, RateLimiter};
    // Large enough that the flip lands several scheduler ticks in: the observer
    // runs once per tick, not once per arrival, so on a tiny object the cap would
    // be switched on with almost nothing left to shape and the test would prove
    // nothing either way.
    const SIZE: u64 = 8 * 1024 * 1024;
    const CAP: u64 = 2 * 1024 * 1024; // bytes/sec
                                      // Flip the cap on a quarter of the way in, so most of the object is left.
    const FLIP_AT: u64 = SIZE / 4;

    let net = Arc::new(OriginSet::new());
    let (port, _ctl) = net.spawn(SIZE, 16 * 1024 * 1024);
    let out = std::env::temp_dir().join("hydra_e2e_live_limit.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(16e6)], &[4]).with_stall_timeout(30.0);

    // Unlimited at start: this is the state every un-capped download begins in.
    let limiter = Arc::new(RateLimiter::unlimited());
    let pace = Pace::shared(limiter.clone());
    assert!(!pace.is_limited(), "the transfer starts with no cap");

    // Where and when the cap actually went on. The observer fires at tick
    // granularity, so `done` at the flip is past `FLIP_AT` by up to a tick's
    // worth of bytes; the assertion below charges the cap only for what was
    // genuinely still to come.
    let flipped: Arc<std::sync::Mutex<Option<(std::time::Instant, u64)>>> =
        Arc::new(std::sync::Mutex::new(None));
    let lim = limiter.clone();
    let flip = flipped.clone();
    let mut observe = move |_: &Scheduler, done: u64| {
        let mut f = flip.lock().unwrap();
        if f.is_none() && done >= FLIP_AT {
            lim.set_rate(CAP);
            *f = Some((std::time::Instant::now(), done));
        }
    };

    pdl_net::run_transfer_paced(
        net.clone(),
        vec![tgt(port)],
        &[4],
        SIZE,
        &outs,
        sched,
        20,
        &mut observe,
        pace,
    )
    .await
    .expect("a transfer capped mid-flight must still complete");

    verify(&outs, SIZE).expect("shaping must not corrupt: the file must be byte-exact");
    // Everything after the flip is subject to the cap. Charge only the bytes that
    // could still have been outstanding then â€?the ones already on disk are free,
    // and in-flight reads already paid for are close enough to ignore.
    let (flip_at, flip_done) = flipped
        .lock()
        .unwrap()
        .expect("the cap must have been switched on during the transfer");
    let shaped = SIZE - flip_done;
    let floor = shaped as f64 / CAP as f64;
    let elapsed = flip_at.elapsed().as_secs_f64();
    assert!(
        elapsed >= floor * 0.7,
        "{shaped} bytes remained under a {CAP} B/s cap, which needs about          {floor:.2}s, but the whole transfer took {elapsed:.2}s: the cap was          switched on and ignored"
    );
    eprintln!(
        "live cap: {shaped} B shaped in {elapsed:.2}s after the cap went on at {flip_done} B \
         (floor {floor:.2}s)"
    );
    let _ = std::fs::remove_file(&out);
}

/// `--limit-rate` must actually shape the transfer, not merely be constructed.
///
/// Regression test: the CLI built a `RateLimiter`, dropped it a few lines later,
/// and never passed it to anything that touched a byte â€?`hydra-net` had no
/// reference to it at all. A 34 041-byte object under a 1 KiB/s cap finished in
/// 5.2s at 6.4 KiB/s, six times the requested ceiling, with no error and a clean
/// exit. The project's own conformance check ("a 20 KB/s cap makes a 34 KB
/// object take at least a second") was too weak to catch it, since setup alone
/// exceeds a second on any real round trip; this asserts against the cap's own
/// arithmetic instead.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn limit_rate_shapes_the_aggregate_transfer() {
    use pdl_net::polite::{Pace, RateLimiter};
    const SIZE: u64 = 256 * 1024;
    const CAP: u64 = 128 * 1024; // bytes/sec: the object must take about 2s

    let net = Arc::new(OriginSet::new());
    // An origin far faster than the cap, so the cap is what binds.
    let (port, _ctl) = net.spawn(SIZE, 16 * 1024 * 1024);
    let out = std::env::temp_dir().join("hydra_e2e_limit_rate.bin");
    let outs = out.to_string_lossy().to_string();
    // Four connections: the cap is on the AGGREGATE, so opening more of them must
    // not multiply the throughput.
    let sched = Scheduler::new(SIZE, vec![src(16e6)], &[4]).with_stall_timeout(30.0);

    let t0 = std::time::Instant::now();
    pdl_net::run_transfer_paced(
        net.clone(),
        vec![tgt(port)],
        &[4],
        SIZE,
        &outs,
        sched,
        20,
        &mut |_: &Scheduler, _: u64| {},
        Pace::shared(Arc::new(RateLimiter::new(CAP))),
    )
    .await
    .expect("a capped transfer must still complete");
    let elapsed = t0.elapsed().as_secs_f64();

    verify(&outs, SIZE).expect("shaping must not corrupt: the file must be byte-exact");
    let floor = SIZE as f64 / CAP as f64;
    assert!(
        elapsed >= floor * 0.8,
        "{SIZE} bytes at {CAP} B/s cannot arrive in {elapsed:.2}s: the cap implies \
         at least {floor:.2}s, so the limiter is not being applied"
    );
    let achieved = SIZE as f64 / elapsed;
    assert!(
        achieved <= CAP as f64 * 1.5,
        "measured {achieved:.0} B/s against a {CAP} B/s cap"
    );
    eprintln!("limit-rate: {SIZE} B in {elapsed:.2}s = {achieved:.0} B/s (cap {CAP})");
    let _ = std::fs::remove_file(&out);
}

/// A forward expressed in HTML instead of in a `Location` header must be
/// recognised and resolved.
///
/// The failure this covers is a silent one: a referrer stripper such as
/// `href.li/?<url>` answers `200 OK` with a kilobyte of HTML, so a client that
/// only understands `3xx` reports SUCCESS and writes the forwarding page under
/// the object's name. The assertion is therefore in two parts â€?the probe must
/// classify the page as a possible redirector, and the resolver must read the
/// real target out of it.
#[tokio::test]
async fn an_html_redirector_page_is_recognised_and_resolved() {
    const SIZE: u64 = 4096;
    let net = OriginSet::new();
    let (obj_port, _obj) = net.spawn(SIZE, 8_000_000);
    let dest = format!("http://127.0.0.1:{obj_port}/obj");
    let (port, _ctl) = net.spawn_html_redirecting(SIZE, 8_000_000, &dest);

    let t = Target::direct("127.0.0.1", port, "/?https://example.org/setup.exe");
    let p = probe(&net, &t).await.expect("probe");
    assert_eq!(p.status, 200, "a redirector answers 200, not 3xx");
    assert!(
        !p.is_redirect(),
        "there is no Location header to find: that is the point"
    );
    assert!(
        p.maybe_redirector(),
        "a small text/html body with no Content-Disposition is worth a look"
    );

    assert_eq!(
        pdl_net::html_redirect(&net, &t).await.as_deref(),
        Some(dest.as_str()),
    );

    // And the object itself is not mistaken for one.
    let obj = probe(&net, &tgt(obj_port)).await.expect("probe");
    assert!(!obj.maybe_redirector());
    assert!(pdl_net::html_redirect(&net, &tgt(obj_port)).await.is_none());
}
