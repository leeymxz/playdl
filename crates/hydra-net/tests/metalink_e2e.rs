//! End-to-end: a mirror list that survives its mirrors.
//!
//! The claim Metalink support is worth making is not "hydra can parse XML". It
//! is that a document naming more sources than the client opens turns a transfer
//! that would have failed into one that finishes — and that the bytes it
//! finishes with are the right bytes. Both halves are measured here over real
//! TCP against origins that fail in the two ways a mirror actually fails:
//! answering wrongly, and answering not at all.

use pdl_core::{Scheduler, Source, SourcePlan};
use pdl_net::origin::{byte_at, OriginSet};
use pdl_net::polite::Pace;
use pdl_net::{Reserve, SparseSink, Target};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

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

fn reserve(port: u16, priority: u32) -> Reserve {
    Reserve {
        target: tgt(port),
        plan: SourcePlan::ranked(priority),
        host: format!("127.0.0.1:{port}"),
    }
}

fn verify(path: &str, size: u64) -> Result<(), String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    if data.len() as u64 != size {
        return Err(format!("size {} != {size}", data.len()));
    }
    match data
        .iter()
        .enumerate()
        .find(|(i, b)| **b != byte_at(*i as u64))
    {
        Some((i, _)) => Err(format!("content mismatch at byte {i}")),
        None => Ok(()),
    }
}

/// Run a transfer with a bench, returning the substitutions that happened.
#[allow(clippy::too_many_arguments)]
async fn run_with_bench(
    net: Arc<OriginSet>,
    targets: Vec<Target>,
    per: &[usize],
    size: u64,
    out: &str,
    sched: Scheduler,
    bench: Vec<Reserve>,
) -> (std::io::Result<(f64, u64)>, Vec<(usize, String)>) {
    let swaps: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(SparseSink::create(out, size).unwrap());
    let rec = swaps.clone();
    let mut on_sub = move |src: usize, r: &Reserve| {
        rec.lock().unwrap().push((src, r.host.clone()));
    };
    let res = pdl_net::run_transfer_with_reserves(
        net,
        targets,
        per,
        size,
        sink,
        sched,
        20,
        &mut |_: &Scheduler, _: u64| {},
        Pace::unlimited(),
        None,
        pdl_net::Bench::fixed(bench),
        Some(&mut on_sub),
    )
    .await;
    let got = swaps.lock().unwrap().clone();
    (res, got)
}

/// A source that black-holes never returns an error, so an error count cannot
/// see it. Without a bench the transfer waits out its no-progress deadline and
/// dies; with one, the scheduler's stall accounting is the signal that swaps it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_black_holing_mirror_is_replaced_from_the_bench() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (dead, dead_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let (spare, spare_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);

    let out = std::env::temp_dir().join("hydra_metalink_blackhole.bin");
    let outs = out.to_string_lossy().to_string();
    // One source, one connection, and a reserve. One source is the harsh case:
    // there is nowhere else for the work to go, so the bench is the only thing
    // between this transfer and a timeout.
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[1]).with_stall_timeout(0.5);
    dead_ctl.blackhole.store(true, Ordering::Relaxed);

    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(dead)],
        &[1],
        SIZE,
        &outs,
        sched,
        vec![reserve(spare, 1)],
    )
    .await;

    res.expect("the bench must rescue a transfer whose only source went silent");
    verify(&outs, SIZE).expect("the rescued file must be byte-exact");
    assert_eq!(swaps.len(), 1, "exactly one substitution: {swaps:?}");
    assert_eq!(swaps[0].0, 0, "the failed source's slot is reused in place");
    assert!(swaps[0].1.ends_with(&spare.to_string()));
    assert!(
        spare_ctl.served.load(Ordering::Relaxed) > 0,
        "the replacement must actually have served the bytes"
    );
    let _ = std::fs::remove_file(&out);
}

/// The other way a mirror fails: it answers, and the answer is unusable. Two
/// consecutive failures with no progress between them is the threshold — one is
/// a CDN node having a bad moment.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mirror_that_answers_wrongly_is_replaced_before_the_transfer_fails() {
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (bad, bad_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let (good, _good_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);

    // Advertises a length it does not deliver, then closes: a truncating origin,
    // which the client must refuse rather than accept as complete.
    bad_ctl.lie_length.store(true, Ordering::Relaxed);

    let out = std::env::temp_dir().join("hydra_metalink_truncating.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[1]).with_stall_timeout(0.5);

    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(bad)],
        &[1],
        SIZE,
        &outs,
        sched,
        vec![reserve(good, 1)],
    )
    .await;

    res.expect("a truncating mirror must cost a substitution, not the transfer");
    verify(&outs, SIZE).expect("byte-exact from the replacement");
    assert!(!swaps.is_empty(), "no substitution happened");
    let _ = std::fs::remove_file(&out);
}

/// Substitution reuses the failed source's connection slot. If it grew the
/// connection set instead, a mirror list would quietly multiply the socket count
/// past whatever politeness authorised — the ceiling would be honoured on paper
/// and defeated in practice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn substitution_does_not_grow_the_socket_budget() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (dead, dead_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let (live, _live_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    let (spare, spare_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    dead_ctl.blackhole.store(true, Ordering::Relaxed);

    let out = std::env::temp_dir().join("hydra_metalink_budget.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6), src(4e6)], &[2, 2]).with_stall_timeout(0.5);

    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(dead), tgt(live)],
        &[2, 2],
        SIZE,
        &outs,
        sched,
        vec![reserve(spare, 1)],
    )
    .await;

    res.expect("transfer must finish");
    verify(&outs, SIZE).expect("byte-exact");
    // Two connections were authorised for the dead source's slot; the
    // replacement inherits exactly those, so the spare never exceeds them.
    assert!(
        spare_ctl.connections.load(Ordering::Relaxed) <= 2,
        "replacement opened {} connections against a budget of 2",
        spare_ctl.connections.load(Ordering::Relaxed)
    );
    if !swaps.is_empty() {
        assert_eq!(swaps[0].0, 0, "only the dead source's slot is reused");
    }
    let _ = std::fs::remove_file(&out);
}

/// An empty bench must behave exactly as the transport did before reserves
/// existed — that is what makes this addition safe for every existing caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_empty_bench_changes_nothing() {
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (p, _c) = net.spawn(SIZE, 8 * 1024 * 1024);
    let out = std::env::temp_dir().join("hydra_metalink_nobench.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[2]).with_stall_timeout(3.0);

    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(p)],
        &[2],
        SIZE,
        &outs,
        sched,
        Vec::new(),
    )
    .await;
    res.expect("a healthy transfer with no reserves is unaffected");
    assert!(swaps.is_empty());
    verify(&outs, SIZE).unwrap();
    let _ = std::fs::remove_file(&out);
}

/// A bench that runs out still fails, and fails with the reason — a client that
/// exhausts nineteen mirrors must say so rather than hang.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_exhausted_bench_fails_with_a_reason() {
    const SIZE: u64 = 2 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (a, ca) = net.spawn(SIZE, 8 * 1024 * 1024);
    let (b, cb) = net.spawn(SIZE, 8 * 1024 * 1024);
    ca.blackhole.store(true, Ordering::Relaxed);
    cb.blackhole.store(true, Ordering::Relaxed);

    let out = std::env::temp_dir().join("hydra_metalink_exhausted.bin");
    let outs = out.to_string_lossy().to_string();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[1]).with_stall_timeout(0.5);

    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(a)],
        &[1],
        SIZE,
        &outs,
        sched,
        vec![reserve(b, 1)],
    )
    .await;

    let e = res.expect_err("every mirror is silent; this cannot succeed");
    assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
    assert!(
        e.to_string().contains("no progress"),
        "the failure must name the symptom: {e}"
    );
    assert_eq!(swaps.len(), 1, "the one reserve was tried before giving up");
    let _ = std::fs::remove_file(&out);
}

/// A mirror that WORKS and is hopeless.
///
/// The two failure modes above are the ones a publisher's ranking cannot help
/// with either way: a dead host is dead whatever its `priority` said. This is
/// the case where the ranking is actively wrong — the document's best-ranked
/// mirror answers every request, delivers real bytes, and does so an order of
/// magnitude slower than the one it ranked below. No error count and no stall
/// count sees that, and only measurement can.
///
/// The test asserts the swap happens AND that the object is still byte-exact
/// afterwards, because a substitution that loses a range would look identical to
/// a successful one until the file is read back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_working_but_hopeless_mirror_is_replaced_by_a_faster_reserve() {
    // Large enough, against these rates, that the transfer is still running
    // when the ten seconds of evidence mature and there is still enough left
    // for the swap to repay its handshake. Both are properties of the RULE, not
    // of the test: a rule that fired on a two-second transfer would be spending
    // handshakes to save nothing.
    const SIZE: u64 = 96 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    // Two seated sources: one healthy, one crawling. The crawler is not broken —
    // it serves correct bytes the whole time — it is just 200x slower, which is
    // well past `LAGGARD_RATIO` and is exactly the mirror a stale ranking sends
    // work to.
    let (fast, _fast_ctl) = net.spawn(SIZE, 6 * 1024 * 1024);
    let (slow, _slow_ctl) = net.spawn(SIZE, 30 * 1024);
    let (spare, _spare_ctl) = net.spawn(SIZE, 6 * 1024 * 1024);

    let out = std::env::temp_dir().join("hydra-metalink-laggard.bin");
    let outs = out.to_string_lossy().into_owned();
    let sched = Scheduler::new(SIZE, vec![src(1e7), src(1e7)], &[2, 2]);
    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(slow), tgt(fast)],
        &[2, 2],
        SIZE,
        &outs,
        sched,
        vec![reserve(spare, 1)],
    )
    .await;

    assert!(res.is_ok(), "the transfer must finish: {res:?}");
    assert_eq!(
        swaps.len(),
        1,
        "the crawling mirror should have been swapped exactly once: {swaps:?}"
    );
    assert_eq!(swaps[0].0, 0, "source 0 is the crawler: {swaps:?}");
    assert_eq!(swaps[0].1, format!("127.0.0.1:{spare}"));
    verify(&outs, SIZE).expect("the object must still be byte-exact after a swap");
    let _ = std::fs::remove_file(&out);
}

/// The other half of the same rule: a source that is merely slower must NOT be
/// swapped.
///
/// Repair already moves bytes away from it continuously, on measurement, at the
/// cost of a range boundary. Substituting instead pays a fresh handshake and
/// throws away everything measured about the host — so a rule that fired at 2:1
/// would make transfers worse while looking busy. Nothing here should move.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_merely_slower_mirror_keeps_its_sockets() {
    const SIZE: u64 = 8 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    let (fast, _a) = net.spawn(SIZE, 8 * 1024 * 1024);
    // Half the speed. Carrying a third of the transfer is not hopeless.
    let (slower, _b) = net.spawn(SIZE, 4 * 1024 * 1024);
    let (spare, _c) = net.spawn(SIZE, 8 * 1024 * 1024);

    let out = std::env::temp_dir().join("hydra-metalink-no-churn.bin");
    let outs = out.to_string_lossy().into_owned();
    let sched = Scheduler::new(SIZE, vec![src(1e7), src(1e7)], &[2, 2]);
    let (res, swaps) = run_with_bench(
        net.clone(),
        vec![tgt(fast), tgt(slower)],
        &[2, 2],
        SIZE,
        &outs,
        sched,
        vec![reserve(spare, 1)],
    )
    .await;

    assert!(res.is_ok(), "the transfer must finish: {res:?}");
    assert!(
        swaps.is_empty(),
        "a 2:1 difference must not spend a reserve: {swaps:?}"
    );
    verify(&outs, SIZE).expect("byte-exact");
    let _ = std::fs::remove_file(&out);
}

/// A reserve that ARRIVES DURING the transfer rescues it.
///
/// This is the property the streaming bench was built for: the transfer starts
/// as soon as it has seats, and mirrors still being probed join the bench when
/// they answer. If the drain were broken — the channel never polled, or polled
/// only at startup — this test would hang on the black-holed source until the
/// no-progress deadline and fail, because the reserve exists only AFTER the
/// transfer is already running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reserve_that_arrives_mid_transfer_still_rescues_a_dead_source() {
    const SIZE: u64 = 4 * 1024 * 1024;
    let net = Arc::new(OriginSet::new());
    // ONE source, black-holed, and nothing else: repair has nowhere to move the
    // work, so the reserve is the only way this transfer can finish — which is
    // exactly what makes the test discriminating. (A first version paired the
    // dead source with a healthy one, and repair quietly finished the object
    // before three stall rounds could accrue: correct behaviour, useless test.)
    let (dead, dead_ctl) = net.spawn(SIZE, 8 * 1024 * 1024);
    dead_ctl.blackhole.store(true, Ordering::Relaxed);
    let (spare, _s) = net.spawn(SIZE, 8 * 1024 * 1024);

    let out = std::env::temp_dir().join("hydra-metalink-late-bench.bin");
    let outs = out.to_string_lossy().into_owned();
    let sched = Scheduler::new(SIZE, vec![src(4e6)], &[1]).with_stall_timeout(0.5);

    // The bench starts EMPTY. The reserve is sent from a task that waits until
    // the stall clock is already running — the shape of a probe that answered
    // late.
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        let _ = tx.send(reserve(spare, 1));
    });

    let swaps: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::new(SparseSink::create(&outs, SIZE).unwrap());
    let rec = swaps.clone();
    let mut on_sub = move |src: usize, r: &pdl_net::Reserve| {
        rec.lock().unwrap().push((src, r.host.clone()));
    };
    let res = pdl_net::run_transfer_with_reserves(
        net.clone(),
        vec![tgt(dead)],
        &[1],
        SIZE,
        sink,
        sched,
        20,
        &mut |_: &Scheduler, _: u64| {},
        Pace::unlimited(),
        None,
        pdl_net::Bench {
            ready: Vec::new(),
            late: Some(rx),
        },
        Some(&mut on_sub),
    )
    .await;

    assert!(res.is_ok(), "the transfer must finish: {res:?}");
    let swaps = swaps.lock().unwrap().clone();
    assert_eq!(
        swaps,
        vec![(0, format!("127.0.0.1:{spare}"))],
        "the black-holed source is replaced by the reserve that arrived mid-transfer"
    );
    verify(&outs, SIZE).expect("byte-exact after a late substitution");
    let _ = std::fs::remove_file(&out);
}
