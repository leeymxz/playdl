//! An origin that refuses concurrency must be met with less of it.
//!
//! A `429` is the server answering a question the client never asked: how many
//! requests at once will you accept? Standing down for `Retry-After` and coming
//! back with the same connection count asks it again and gets the same answer.
//! `ash-speed.hetzner.com` serves happily at two connections and refuses
//! everything at four, so eight connections downloaded zero bytes in thirty
//! seconds â€?one refusal every two seconds, each aborting what the other seven
//! had in flight. The transfer has to converge on a count the origin will serve.

use pdl_core::{Scheduler, Source};
use pdl_net::{run_transfer, Target, TlsCapableConnector};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SIZE: u64 = 16 * 1024 * 1024;
/// What this origin will serve at once. Anything above it is refused.
const ALLOWED: usize = 2;
/// The slow-first-byte origin's artificial delay before a granted request's
/// first body byte, in milliseconds. Shared with the `Source::delta_est` the
/// scheduler is given for that origin: a real client would have this from a
/// probe before the transfer starts (`hydra-cli` measures it that way), and
/// starting the scheduler from a `delta_est` an order of magnitude below the
/// origin's real cost â€?as `Source::default()`'s 50 ms is here â€?makes the
/// repair profitability test misjudge what a steal actually pays, which is a
/// property of an unrealistic test fixture, not of the scheduler.
const SLOW_FIRST_BYTE_MS: u64 = 400;

fn byte_at(off: u64) -> u8 {
    (off % 251) as u8
}

/// Serves ranges, but only [`ALLOWED`] at a time; the rest get `429`.
async fn spawn_throttled_origin(refusals: Arc<AtomicUsize>, peak: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let inflight = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (inflight, refusals, peak) = (inflight.clone(), refusals.clone(), peak.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let range = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                    .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_default();
                let spec = range.trim_start_matches("bytes=");
                let (lo, hi) = spec.split_once('-').unwrap_or(("0", ""));
                let lo: u64 = lo.parse().unwrap_or(0);
                let hi: u64 = hi.parse().unwrap_or(SIZE - 1);

                let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                if n > ALLOWED {
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    refusals.fetch_add(1, Ordering::SeqCst);
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\n\
                              Content-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                peak.fetch_max(n, Ordering::SeqCst);
                let len = hi - lo + 1;
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {len}\r\nETag: \"throttled\"\r\n\
                     Content-Range: bytes {lo}-{hi}/{SIZE}\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes()).await;
                // In slices, with the request kept in flight long enough that
                // concurrent ones actually overlap.
                let mut off = lo;
                while off <= hi {
                    let end = (off + 32 * 1024 - 1).min(hi);
                    let body: Vec<u8> = (off..=end).map(byte_at).collect();
                    if s.write_all(&body).await.is_err() {
                        break;
                    }
                    off = end + 1;
                    // Sampled here, not only at grant time above: a client that has
                    // converged needs few requests, so a request granted while both
                    // slots are held can run to completion without a single further
                    // accept() â€?and a peak that only updates on accept would then
                    // see nothing for the rest of the transfer, wiped by the test's
                    // reset if that grant landed before it, and reporting collapse
                    // for a client that never collapsed. Concurrency actually held
                    // is what the assertion means; sampling every chunk this
                    // connection writes is what makes that observable regardless of
                    // when the request that is holding it was granted.
                    peak.fetch_max(inflight.load(Ordering::SeqCst), Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                inflight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_429_lowers_the_connection_count_instead_of_livelocking() {
    let refusals = Arc::new(AtomicUsize::new(0));
    // The most requests this origin serves at once ONCE THE CLIENT HAS SETTLED â€?
    // the counter is reset below, after the refusals have done their work.
    // Converging is only half the requirement: a client that answers a refusal by
    // collapsing to one connection has stopped being refused and is also
    // transferring at half the rate the origin was willing to give.
    let peak = Arc::new(AtomicUsize::new(0));
    let port = spawn_throttled_origin(refusals.clone(), peak.clone()).await;
    let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join("hydra_throttled_origin.bin");
    let outs = out.to_string_lossy().to_string();

    // Eight connections against an origin that serves two: without a concurrency
    // response to the refusals, every round is refused exactly as the last one was.
    // Forget the opening burst: every client reaches the origin's limit before the
    // first refusal has been felt. What is being measured is what it does after.
    let settle = peak.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        settle.store(0, Ordering::SeqCst);
    });

    let t0 = std::time::Instant::now();
    let sched = Scheduler::new(SIZE, vec![Source::default()], &[8]).with_stall_timeout(3.0);
    let r = tokio::time::timeout(
        Duration::from_secs(60),
        run_transfer(conn, vec![t], &[8], SIZE, &outs, sched),
    )
    .await
    .expect("a throttled transfer must converge, not livelock");
    r.expect("the object must be delivered at a connection count the origin serves");

    let refused = refusals.load(Ordering::SeqCst);
    eprintln!(
        "refusals={refused} elapsed={:.2}s",
        t0.elapsed().as_secs_f64()
    );
    let data = std::fs::read(&out).expect("output");
    let _ = std::fs::remove_file(&out);
    assert_eq!(data.len() as u64, SIZE, "short file");
    let bad = data
        .iter()
        .enumerate()
        .find(|(i, b)| **b != byte_at(*i as u64));
    assert!(
        bad.is_none(),
        "content mismatch at byte {:?}",
        bad.map(|(i, _)| i)
    );
    assert!(
        refused > 0,
        "the origin must actually have refused, or this proves nothing"
    );
    // Converging costs a bounded number of refusals â€?one round per halving.
    // Retrying at the same count instead of lowering it measured 129 refusals
    // over 26 s on this origin against 8 over 2.2 s, so the ceiling separates
    // "learned the limit" from "kept asking".
    assert_eq!(
        peak.load(Ordering::SeqCst),
        ALLOWED,
        "the transfer must settle at the concurrency the origin serves, not below it"
    );
    assert!(
        refused <= 24,
        "{refused} refusals means the client never lowered its concurrency"
    );
}

/// Concurrency a throttled origin refuses must cost nothing, not everything.
///
/// # The ordering this exists to reproduce
///
/// A refusal is a 162-byte body that comes back a round trip ahead of the first
/// body bytes of the requests beside it that were GRANTED. On loopback, where a
/// granted response delivers its first slice in microseconds, that ordering never
/// happens and [`a_429_lowers_the_connection_count_instead_of_livelocking`]
/// passes on a client that mishandles it completely. The `400 ms` first-byte
/// delay below is the whole point of this second origin.
///
/// Three things went wrong under that ordering, and all three read "nothing has
/// delivered yet" as "nothing is working":
///
/// * the client stood the whole SOURCE down on the first refusal, aborting the
///   requests the origin had just agreed to serve and paying their handshakes
///   again;
/// * the ceiling's floor â€?"never below what is visibly working" â€?was zero
///   whenever the working connections were between ranges, so a ceiling that had
///   correctly found the origin's limit was halved off it and the transfer
///   finished at one connection against an origin serving two;
/// * assignment kept reserving work for the six connections the ceiling had
///   already ruled out, handing the two live ones a budget-sized share at a time
///   and paying a request â€?a first byte, and on this origin a fresh handshake â€?
///   for each.
///
/// # What is asserted
///
/// Not a wall-clock number, which would be a claim about the machine: the same
/// object is fetched at one connection and at eight, and the eight-connection
/// transfer must not be dramatically worse. That is the property the user sees,
/// and it is what failed. Measured on this origin before the fix: 3.8 s at one
/// connection against 17.8 s at eight, monotonically worse at every count in
/// between (5.9 s at two, 10.5 s at four). After: 3.8 s against 4.2 s.
///
/// The live original is `ash-speed.hetzner.com`, which serves two connections per
/// address and refuses the rest. A 100 MB object there took 6 m 57 s at `-x 8`
/// against 2 m 05 s at `-x 2`, which is the same shape at a real RTT.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrency_the_origin_refuses_costs_nothing() {
    let single = fetch_from_slow_first_byte_origin(1).await;
    let wide = fetch_from_slow_first_byte_origin(8).await;
    eprintln!("single={single:?} wide={wide:?}");
    assert!(
        wide.refusals > 0,
        "the origin must actually have refused the wide transfer, or this proves nothing"
    );
    assert_eq!(
        single.refusals, 0,
        "one connection is inside this origin's limit and must never be refused"
    );
    // Generous: the wide transfer legitimately pays a few refused requests to find
    // the limit, and this runs on whatever CI machine it lands on. The failure it
    // catches was 4.7x, and every count between 1 and 8 was worse than the one
    // below it.
    assert!(
        wide.elapsed_s < single.elapsed_s * 1.75 + 1.0,
        "{:.2}s at eight connections against {:.2}s at one: concurrency the origin \
         refuses is costing the transfer instead of being dropped",
        wide.elapsed_s,
        single.elapsed_s
    );
    // The same failure seen as requests rather than as clock. Eight connections
    // against a two-connection origin needs a handful more requests than one does,
    // not an order of magnitude more.
    //
    // Do not loosen this to make a run go green. It was raised to 24 once, to
    // absorb grant counts of 17-22 that were not tail variance at all but a
    // scheduler regression: `active_limit` is `usize::MAX` for a caller that never
    // opted into the ramp, so a reserve test written against it was vacuously
    // true, maximal-range assignment became unreachable, and every transfer
    // re-requested its work a share at a time â€?the exact failure this line is
    // here to catch, hidden by the line itself. Measured 6-11 with that fixed.
    assert!(
        wide.grants <= 16,
        "{} granted requests to deliver the object at eight connections against {} \
         at one: the transfer is re-requesting work a share at a time",
        wide.grants,
        single.grants
    );
}

#[derive(Debug)]
struct ThrottledRun {
    elapsed_s: f64,
    refusals: usize,
    grants: usize,
}

/// Fetch the whole object at `n` connections from the slow-first-byte origin.
async fn fetch_from_slow_first_byte_origin(n: usize) -> ThrottledRun {
    let refusals = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let grants = Arc::new(AtomicUsize::new(0));
    let abandoned = Arc::new(AtomicUsize::new(0));
    let port = spawn_slow_first_byte_origin(
        refusals.clone(),
        peak.clone(),
        grants.clone(),
        abandoned.clone(),
    )
    .await;
    let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join(format!("hydra_throttled_slow_first_byte_{n}.bin"));
    let outs = out.to_string_lossy().to_string();

    let source = Source {
        delta_est: SLOW_FIRST_BYTE_MS as f64 / 1000.0,
        ..Default::default()
    };
    let sched = Scheduler::new(SIZE, vec![source], &[n]).with_stall_timeout(5.0);
    let t0 = std::time::Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(120),
        run_transfer(conn, vec![t], &[n], SIZE, &outs, sched),
    )
    .await
    .expect("a throttled transfer must converge, not livelock");
    r.expect("the object must be delivered at a connection count the origin serves");
    let elapsed_s = t0.elapsed().as_secs_f64();

    let data = std::fs::read(&out).expect("output");
    let _ = std::fs::remove_file(&out);
    assert_eq!(data.len() as u64, SIZE, "short file at {n} connections");
    let bad = data
        .iter()
        .enumerate()
        .find(|(i, b)| **b != byte_at(*i as u64));
    assert!(
        bad.is_none(),
        "content mismatch at byte {:?} at {n} connections",
        bad.map(|(i, _)| i)
    );
    ThrottledRun {
        elapsed_s,
        refusals: refusals.load(Ordering::SeqCst),
        grants: grants.load(Ordering::SeqCst),
    }
}

/// Like [`spawn_throttled_origin`], but a granted request waits before its first
/// body byte while a refusal is answered at once â€?the ordering every real path
/// with latency produces, and the one loopback hides.
async fn spawn_slow_first_byte_origin(
    refusals: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    grants: Arc<AtomicUsize>,
    abandoned: Arc<AtomicUsize>,
) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let inflight = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (inflight, refusals, peak, grants, abandoned) = (
                inflight.clone(),
                refusals.clone(),
                peak.clone(),
                grants.clone(),
                abandoned.clone(),
            );
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut buf = [0u8; 1024];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => head.extend_from_slice(&buf[..n]),
                    }
                }
                let text = String::from_utf8_lossy(&head).to_string();
                let range = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                    .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()))
                    .unwrap_or_default();
                let spec = range.trim_start_matches("bytes=");
                let (lo, hi) = spec.split_once('-').unwrap_or(("0", ""));
                let lo: u64 = lo.parse().unwrap_or(0);
                let hi: u64 = hi.parse().unwrap_or(SIZE - 1);

                let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                if n > ALLOWED {
                    inflight.fetch_sub(1, Ordering::SeqCst);
                    refusals.fetch_add(1, Ordering::SeqCst);
                    // Answered immediately: this is the round trip a refusal wins
                    // against a granted response on a real path.
                    let _ = s
                        .write_all(
                            b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\n\
                              Content-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await;
                    return;
                }
                peak.fetch_max(n, Ordering::SeqCst);
                grants.fetch_add(1, Ordering::SeqCst);
                let len = hi - lo + 1;
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {len}\r\nETag: \"throttled\"\r\n\
                     Content-Range: bytes {lo}-{hi}/{SIZE}\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes()).await;
                // The first byte costs a round trip the refusal did not.
                tokio::time::sleep(Duration::from_millis(SLOW_FIRST_BYTE_MS)).await;
                let mut off = lo;
                while off <= hi {
                    let end = (off + 32 * 1024 - 1).min(hi);
                    let body: Vec<u8> = (off..=end).map(byte_at).collect();
                    if s.write_all(&body).await.is_err() {
                        // The client hung up on a range this origin had agreed to
                        // serve: work granted and then thrown away.
                        abandoned.fetch_add(1, Ordering::SeqCst);
                        break;
                    }
                    off = end + 1;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                inflight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    port
}

/// Not an assertion: a bench of the same origin at fixed connection counts, so the
/// numbers the comments above quote can be reproduced. `--ignored` because it is a
/// measurement, not a property.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore]
async fn bench_fixed_counts_against_the_throttled_origin() {
    for n in [1usize, 2, 4, 8] {
        let refusals = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let grants = Arc::new(AtomicUsize::new(0));
        let abandoned = Arc::new(AtomicUsize::new(0));
        let port = spawn_slow_first_byte_origin(
            refusals.clone(),
            peak.clone(),
            grants.clone(),
            abandoned.clone(),
        )
        .await;
        let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
        let t = Target::direct("127.0.0.1", port, "/obj");
        let out = std::env::temp_dir().join(format!("hydra_bench_{n}.bin"));
        let outs = out.to_string_lossy().to_string();
        let source = Source {
            delta_est: SLOW_FIRST_BYTE_MS as f64 / 1000.0,
            ..Default::default()
        };
        let sched = Scheduler::new(SIZE, vec![source], &[n]).with_stall_timeout(5.0);
        let t0 = std::time::Instant::now();
        let r = tokio::time::timeout(
            Duration::from_secs(120),
            run_transfer(conn, vec![t], &[n], SIZE, &outs, sched),
        )
        .await
        .expect("no livelock");
        r.expect("delivered");
        eprintln!(
            "n={n} elapsed={:.2}s refusals={} grants={} abandoned={}",
            t0.elapsed().as_secs_f64(),
            refusals.load(Ordering::SeqCst),
            grants.load(Ordering::SeqCst),
            abandoned.load(Ordering::SeqCst)
        );
        let _ = std::fs::remove_file(&out);
    }
}
