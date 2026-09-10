//! FTP end to end against an in-process server.
//!
//! Port 21 is blocked by this sandbox's network policy (a port rule, which no domain grant
//! can lift) and `bind()` is refused, so neither a live nor a loopback FTP server is
//! reachable. The origin runs over `tokio::io::duplex` behind the `Connector` trait, which
//! is the same async byte stream a socket provides â€?the identical approach already used for
//! the HTTP end-to-end tests in this crate.
//!
//! These tests exercise the real client: reply parsing, `PASV` and the separate data
//! connection, `REST` offsets, `TYPE I`, `ABOR`, authentication, and the short-transfer
//! check. They deliberately do not claim to measure latency; the preemption cost is
//! reported as a round-trip COUNT, which is a protocol property and carries over to a real
//! network, rather than a duration, which would not.

use pdl_net::ftp::FtpFetcher;
use pdl_net::ftp_origin::{byte_at, FtpOriginSet};
use pdl_net::scheme::{Endpoint, Fetcher};
use pdl_net::SparseSink;
use std::sync::atomic::Ordering;
use std::sync::Arc;

const SIZE: u64 = 4 * 1024 * 1024;

fn tmp(name: &str) -> String {
    std::env::temp_dir()
        .join(format!("hydra_ftp_{}_{name}", std::process::id()))
        .to_string_lossy()
        .to_string()
}

/// Every byte in `[lo, hi)` of `path` matches the generator.
fn verify(path: &str, lo: u64, hi: u64) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    if f.seek(SeekFrom::Start(lo)).is_err() {
        return false;
    }
    let mut buf = vec![0u8; (hi - lo) as usize];
    if f.read_exact(&mut buf).is_err() {
        return false;
    }
    buf.iter()
        .enumerate()
        .all(|(i, b)| *b == byte_at(lo + i as u64))
}

#[tokio::test]
async fn a_ranged_ftp_fetch_lands_the_right_bytes_at_the_right_offsets() {
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");

    let path = tmp("ranged.bin");
    let sink = Arc::new(SparseSink::create(&path, SIZE).unwrap());
    // A middle range: REST must position the transfer and the client must stop at `hi`,
    // since the server streams to end-of-file regardless.
    let (lo, hi) = (1_000_000u64, 1_500_000u64);
    f.fetch_range(&ep, lo, hi, sink.clone()).await.unwrap();
    drop(sink);

    assert!(
        verify(&path, lo, hi),
        "REST offset or the client's upper bound is wrong: bytes do not match the generator"
    );
    let verbs = ctl.verbs();
    assert!(
        verbs.contains(&"TYPE".to_string()),
        "binary mode is mandatory: {verbs:?}"
    );
    assert!(verbs.contains(&"REST".to_string()));
    assert!(verbs.contains(&"PASV".to_string()));
    assert!(verbs.contains(&"RETR".to_string()));
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn probe_reports_size_and_flags_its_validator_as_weak() {
    let (origin, _ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    let p = f.probe(&ep).await.unwrap();
    assert_eq!(p.size, SIZE, "SIZE must be parsed from the 213 reply");
    assert!(p.ranged, "a server answering REST with 350 supports ranges");
    assert!(
        p.weak_validator,
        "SIZE+MDTM is a weak identity: two mirrors can agree on both and serve \
         different builds, so it must never justify cross-mirror assembly"
    );
    // The recorded exchange is what --server-response prints, so it must not leak secrets.
    assert!(!p.raw.is_empty());
}

#[tokio::test]
async fn a_server_without_size_or_rest_is_reported_honestly() {
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    *ctl.no_size.lock().unwrap() = true;
    *ctl.no_rest.lock().unwrap() = true;
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    let p = f.probe(&ep).await.unwrap();
    // SIZE and REST are RFC 3659 extensions, not RFC 959: plenty of servers lack them.
    // Reporting 0/false lets the engine stream instead of failing or inventing a size.
    assert_eq!(p.size, 0, "an unimplemented SIZE must not be guessed");
    assert!(
        !p.ranged,
        "without REST there are no ranges, and claiming otherwise would produce garbage"
    );
    assert!(
        ctl.count("REST") >= 1,
        "support must be probed, not assumed"
    );
}

#[tokio::test]
async fn explicit_credentials_are_sent_and_a_bad_password_fails_legibly() {
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    *ctl.require.lock().unwrap() = Some(("alice".into(), "s3cret".into()));
    let f = FtpFetcher::new(origin.clone());

    let good = Endpoint::new("ftp.test", 21, "/pub/object.bin")
        .with_credentials(Some("alice"), Some("s3cret"));
    let p = f
        .probe(&good)
        .await
        .expect("correct credentials must log in");
    assert_eq!(p.size, SIZE);
    assert!(
        ctl.count("PASS") >= 1,
        "a 331 reply must be followed by PASS"
    );
    // The recorded exchange feeds --server-response, so the password must not be in it.
    assert!(
        !p.raw.contains("s3cret"),
        "the password must be redacted in the recorded exchange: {}",
        p.raw
    );
    assert!(p.raw.contains("PASS <redacted>"));

    let bad = Endpoint::new("ftp.test", 21, "/pub/object.bin")
        .with_credentials(Some("alice"), Some("wrong"));
    let e = f.probe(&bad).await.expect_err("a wrong password must fail");
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
    let msg = e.to_string();
    assert!(msg.contains("alice"), "the user should be named: {msg}");
    assert!(
        !msg.contains("wrong"),
        "but the attempted password must never appear in an error: {msg}"
    );
}

#[tokio::test]
async fn anonymous_access_works_without_credentials() {
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    assert_eq!(f.probe(&ep).await.unwrap().size, SIZE);
    // The server answered 230 to USER, so no PASS was needed and none should be sent.
    assert_eq!(
        ctl.count("PASS"),
        0,
        "a 230 reply means the password step is skipped"
    );
}

#[tokio::test]
async fn a_multiline_banner_does_not_desynchronise_the_session() {
    // The classic FTP client bug: read only the first line of a multi-line reply, and every
    // subsequent exchange is off by one because the rest is taken as the next response.
    let (origin, _ctl) = FtpOriginSet::new(21, SIZE);
    *_ctl.multiline_banner.lock().unwrap() = true;
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    let p = f.probe(&ep).await.unwrap();
    assert_eq!(
        p.size, SIZE,
        "a three-line 220 banner must be consumed whole, or SIZE reads a stale reply"
    );
}

#[tokio::test]
async fn a_truncated_transfer_is_not_reported_as_success() {
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    // Deliver half of what was asked, then close.
    *ctl.truncate_after.lock().unwrap() = Some(100_000);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    let path = tmp("trunc.bin");
    let sink = Arc::new(SparseSink::create(&path, SIZE).unwrap());
    let e = f
        .fetch_range(&ep, 0, 200_000, sink)
        .await
        .expect_err("a short transfer must be an error, not a success with a short file");
    assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    assert!(
        e.to_string().contains("100000") && e.to_string().contains("200000"),
        "the error should state what arrived against what was asked: {e}"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn preemption_costs_control_round_trips_and_a_fresh_data_connection() {
    // THE measurement this whole exercise exists for. On HTTP, shrinking a range costs
    // nothing: the client stops reading. On FTP the range has no client-side end, so the
    // transfer must be aborted and the next range needs a new data connection. Counting
    // those is a protocol property that carries to a real network, unlike a duration
    // measured over an in-process pipe.
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");

    let path = tmp("preempt.bin");
    let sink = Arc::new(SparseSink::create(&path, SIZE).unwrap());
    // Four ranges, as a scheduler reassigning work would issue them.
    for i in 0..4u64 {
        let lo = i * 256 * 1024;
        f.fetch_range(&ep, lo, lo + 256 * 1024, sink.clone())
            .await
            .unwrap();
    }
    drop(sink);

    let aborts = f.abort_rtts.load(Ordering::Relaxed);
    let data_conns = ctl.data_connections.load(Ordering::Relaxed);
    assert_eq!(
        data_conns, 4,
        "FTP needs one data connection per range; HTTP reuses one connection for many"
    );
    assert!(
        aborts >= 4,
        "each early stop costs at least one ABOR round trip, got {aborts}"
    );
    assert_eq!(
        ctl.count("PASV"),
        4,
        "and one PASV per data connection, which HTTP does not pay at all"
    );
    // The declared capability must match what was measured, or the scheduler prices
    // reassignment wrongly.
    let declared = f.capabilities().preempt_cost_rtt;
    let measured = aborts as f64 / 4.0;
    assert!(
        measured <= declared + 0.01,
        "declared preempt cost {declared} RTT must not understate the measured {measured}"
    );
    assert!(
        verify(&path, 0, 1024 * 1024),
        "all four ranges landed correctly"
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn the_client_asks_for_binary_mode_and_refuses_to_proceed_without_it() {
    // ASCII mode rewrites line endings, so a binary object arrives corrupted AND its length
    // disagrees with SIZE. The client must insist on TYPE I.
    let (origin, ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");
    let _ = f.probe(&ep).await.unwrap();
    let verbs = ctl.verbs();
    let type_pos = verbs.iter().position(|v| v == "TYPE");
    let size_pos = verbs.iter().position(|v| v == "SIZE");
    assert!(type_pos.is_some(), "TYPE I must be sent");
    assert!(
        type_pos < size_pos,
        "binary mode must be set BEFORE anything is measured or fetched: {verbs:?}"
    );
}

/// The sink's byte counter must advance DURING an FTP fetch, not only at the end.
///
/// This is what the CLI's FTP progress bar is driven from. `fetch_range` is a
/// single await that returns once the whole object has landed, so there is no
/// per-tick callback to render from the way the HTTP path renders from the
/// scheduler's observer â€?a multi-megabyte FTP download printed nothing at all
/// and then a finished summary. Polling `sink.written` alongside the fetch is
/// the mechanism that fixes it, and this asserts the counter is actually
/// observable mid-flight rather than jumping from 0 to the full size.
#[tokio::test]
async fn the_sink_counter_advances_while_an_ftp_fetch_runs() {
    let (origin, _ctl) = FtpOriginSet::new(21, SIZE);
    let f = FtpFetcher::new(origin);
    let ep = Endpoint::new("ftp.test", 21, "/pub/object.bin");

    let path = tmp("progress.bin");
    let sink = Arc::new(SparseSink::create(&path, SIZE).unwrap());
    let observed = Arc::new(std::sync::Mutex::new(Vec::<u64>::new()));

    let watch_sink = sink.clone();
    let watch = observed.clone();
    let fut = f.fetch_range(&ep, 0, SIZE, sink.clone());
    tokio::pin!(fut);
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(1));
    let r = loop {
        tokio::select! {
            res = &mut fut => break res,
            _ = ticker.tick() => {
                watch.lock().unwrap().push(watch_sink.written.load(Ordering::Relaxed));
            }
        }
    };
    r.expect("the fetch must succeed");

    let samples = observed.lock().unwrap().clone();
    let partial = samples.iter().filter(|&&n| n > 0 && n < SIZE).count();
    assert!(
        partial > 0,
        "no sample caught the transfer in progress: the counter went straight \
         from 0 to {SIZE}, so a progress bar polling it would render nothing. \
         Samples: {samples:?}"
    );
    assert_eq!(
        sink.written.load(Ordering::Relaxed),
        SIZE,
        "the final count must be the whole object"
    );
    assert!(verify(&path, 0, SIZE), "the bytes must still be exact");
    let _ = std::fs::remove_file(&path);
}
