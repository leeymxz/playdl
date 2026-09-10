//! A server that refuses HEAD must not turn a ranged object into an unresumable
//! stream.
//!
//! `ash-speed.hetzner.com` answers a HEAD by closing the connection with an empty
//! reply. `probe` reports that as a successful response — status 0, no length, no
//! range support — because a peer that hangs up after a complete header block is
//! merely impolite, and the read loop cannot tell "impolite" from "said nothing".
//! Believing it sends a ten-gigabyte object down the single-stream path: unknown
//! size, no resume, one connection. The same URL answers `bytes=0-0` with `206`,
//! its full length and a strong ETag, which is what `probe_resilient` asks for.

use pdl_net::{fetch_streaming_observed, polite::Pace, probe, probe_resilient, Target};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const TOTAL: u64 = 10_737_418_240;

/// Read one request head, and report `(method, range_header)`.
async fn read_request(s: &mut tokio::net::TcpStream) -> Option<(String, String, Option<String>)> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut buf).await.ok()?;
        if n == 0 {
            return None;
        }
        head.extend_from_slice(&buf[..n]);
        if head.len() > 16 * 1024 {
            return None;
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut parts = text.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let range = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))
        .and_then(|l| l.split_once(':').map(|(_, v)| v.trim().to_string()));
    Some((method, path, range))
}

/// The hetzner shape: HEAD is answered by hanging up, ranged GET is answered
/// properly, and a plain GET drips forever so a cancel has something to interrupt.
async fn spawn_origin() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let Some((method, path, range)) = read_request(&mut s).await else {
                    return;
                };
                if path.ends_with("/empty") {
                    // A zero-length object, answered the way real servers answer
                    // one: HEAD states the length, and a `bytes=0-0` GET against
                    // it is unsatisfiable. This path answers HEAD, because what
                    // is under test is what the FALLBACK does with the `416`.
                    let reply: &[u8] = if method == "HEAD" {
                        b"HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\n\
                          Content-Length: 0\r\nETag: \"empty\"\r\n\r\n"
                    } else {
                        b"HTTP/1.1 416 Range Not Satisfiable\r\n\
                          Content-Range: bytes */0\r\nContent-Length: 0\r\n\r\n"
                    };
                    let _ = s.write_all(reply).await;
                    return;
                }
                if method == "HEAD" {
                    // No status line, no headers, no close_notify: just a socket
                    // that goes away. This is the whole bug.
                    return;
                }
                if let Some(r) = range {
                    let spec = r.trim_start_matches("bytes=");
                    let (lo, hi) = spec.split_once('-').unwrap_or(("0", "0"));
                    let lo: u64 = lo.parse().unwrap_or(0);
                    let hi: u64 = hi.parse().unwrap_or(0);
                    let len = hi - lo + 1;
                    let head = format!(
                        "HTTP/1.1 206 Partial Content\r\nServer: nginx\r\n\
                         Content-Type: application/octet-stream\r\nContent-Length: {len}\r\n\
                         ETag: \"60c9b8b5-280000000\"\r\n\
                         Content-Range: bytes {lo}-{hi}/{TOTAL}\r\n\r\n"
                    );
                    let _ = s.write_all(head.as_bytes()).await;
                    let _ = s.write_all(&vec![7u8; len as usize]).await;
                    return;
                }
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {TOTAL}\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes()).await;
                // Drip, so the body outlives the test and a cancel is what ends it.
                loop {
                    if s.write_all(&[7u8; 16 * 1024]).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_head_refusing_origin_still_yields_size_and_range_support() {
    let port = spawn_origin().await;
    let conn = Arc::new(pdl_net::TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/10GB.bin");

    // What the bare HEAD reports, and why it cannot be trusted: a complete
    // "response" that says nothing at all.
    let bare = probe(conn.as_ref(), &t)
        .await
        .expect("HEAD returns no error");
    assert_eq!(bare.status, 0, "an empty reply parses as no status");
    assert_eq!(bare.size, 0, "an empty reply carries no length");
    assert!(!bare.ranges, "an empty reply advertises nothing");

    let p = probe_resilient(conn.as_ref(), &t)
        .await
        .expect("the ranged GET must answer where HEAD would not");
    assert_eq!(p.size, TOTAL, "the ranged GET carries the object's length");
    assert!(
        p.ranges,
        "a 206 proves range support, so the transfer must be resumable and parallel"
    );
    assert!(p.validator.is_some(), "the ETag must survive the fallback");
}

/// The fallback must not turn an empty object into a failed download.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_zero_length_object_is_answered_by_head_alone() {
    let port = spawn_origin().await;
    let conn = Arc::new(pdl_net::TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/empty");

    let p = probe_resilient(conn.as_ref(), &t)
        .await
        .expect("an empty object is a legitimate object");
    assert_eq!(p.status, 200, "the 416 from the ranged GET must not win");
    assert_eq!(p.size, 0, "the object really is empty");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_streaming_fetch_reports_progress_and_stops_when_cancelled() {
    let port = spawn_origin().await;
    let conn = Arc::new(pdl_net::TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/10GB.bin");
    let out = std::env::temp_dir().join("hydra_stream_cancel.bin");
    let outs = out.to_string_lossy().to_string();

    let written = Arc::new(AtomicU64::new(0));
    let cancel = Arc::new(AtomicBool::new(false));
    let (w, c) = (written.clone(), cancel.clone());
    tokio::spawn(async move {
        // Long enough that bytes are certainly moving, short enough that a
        // regression to "cancel is never read" fails the test rather than
        // waiting out the object.
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            w.load(Ordering::Relaxed) > 0,
            "the byte counter must move while the body is still arriving"
        );
        c.store(true, Ordering::Relaxed);
    });

    let started = std::time::Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(10),
        fetch_streaming_observed(
            conn.as_ref(),
            &t,
            &outs,
            &written,
            Some(cancel.as_ref()),
            &Pace::unlimited(),
        ),
    )
    .await
    .expect("a cancelled stream must not run to the object's end");
    let waited = started.elapsed();
    let _ = std::fs::remove_file(&out);

    let e = r.expect_err("a cancelled stream reports interruption, not success");
    assert_eq!(
        e.kind(),
        std::io::ErrorKind::Interrupted,
        "cancellation must be distinguishable from a transport failure, got {e}"
    );
    assert!(
        waited < Duration::from_secs(3),
        "stop must be acted on promptly, took {waited:?}"
    );
    assert!(
        written.load(Ordering::Relaxed) > 0,
        "the bytes already written must be reported"
    );
}
