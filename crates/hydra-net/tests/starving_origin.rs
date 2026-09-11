// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! An origin that admits connections it will not serve must be met with fewer
//! of them.
//!
//! The `429` path already lowers the concurrency ceiling when the origin SAYS
//! it is over its limit. This origin never says so: it accepts every TCP
//! connection, reads every request, serves two of them at a time, and lets the
//! rest sit in silence. Nothing errors, so nothing the error path could see
//! ever happens. Measured on `saimei.ftp.acc.umu.se` with `-x 8`: two
//! connections delivered, six sat at 0 B/s for the whole stall timeout, all six
//! flipped to `hung` at once, were reclaimed, re-requested, and starved again —
//! a fresh handshake and a lost congestion window every round, and a transfer
//! 2.2x SLOWER than a single connection against the same object.
//!
//! The transport has to read that silence as the refusal it is.

use pdl_core::{LimitReason, Scheduler, Source};
use pdl_net::{run_transfer_observed, Target, TlsCapableConnector};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const SIZE: u64 = 16 * 1024 * 1024;
/// What this origin serves at once. Anything above it is accepted and starved.
const ALLOWED: usize = 2;
/// Pause between 32 KiB slices, so the transfer outlasts several stall
/// timeouts: a client that never learns the limit pays the starvation round
/// again every one of them, and that repetition is what the assertion counts.
const SLICE_PAUSE_MS: u64 = 40;

fn byte_at(off: u64) -> u8 {
    (off % 251) as u8
}

/// Serves ranges to [`ALLOWED`] requests at a time. Every further request is
/// read in full and then left hanging — no status line, no bytes — until the
/// client gives up on it.
async fn spawn_starving_origin(starved: Arc<AtomicUsize>, grants: Arc<AtomicUsize>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = l.local_addr().expect("addr").port();
    let inflight = Arc::new(AtomicUsize::new(0));
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (inflight, starved, grants) = (inflight.clone(), starved.clone(), grants.clone());
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
                    starved.fetch_add(1, Ordering::SeqCst);
                    // Silence. The client sends nothing further on a request it
                    // has made, so this read returns only when the client closes
                    // the connection — which is the only way out of here.
                    let mut one = [0u8; 1];
                    let _ = s.read(&mut one).await;
                    return;
                }
                grants.fetch_add(1, Ordering::SeqCst);
                let len = hi - lo + 1;
                let head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\n\
                     Content-Length: {len}\r\nETag: \"starving\"\r\n\
                     Content-Range: bytes {lo}-{hi}/{SIZE}\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes()).await;
                let mut off = lo;
                while off <= hi {
                    let end = (off + 32 * 1024 - 1).min(hi);
                    let body: Vec<u8> = (off..=end).map(byte_at).collect();
                    if s.write_all(&body).await.is_err() {
                        break;
                    }
                    off = end + 1;
                    tokio::time::sleep(Duration::from_millis(SLICE_PAUSE_MS)).await;
                }
                inflight.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn silent_starvation_lowers_the_connection_count_like_a_refusal_does() {
    let starved = Arc::new(AtomicUsize::new(0));
    let grants = Arc::new(AtomicUsize::new(0));
    let port = spawn_starving_origin(starved.clone(), grants.clone()).await;
    let conn = Arc::new(TlsCapableConnector::new().expect("client must build"));
    let t = Target::direct("127.0.0.1", port, "/obj");
    let out = std::env::temp_dir().join("hydra_starving_origin.bin");
    let outs = out.to_string_lossy().to_string();

    const N: usize = 8;
    // A short stall timeout keeps the test short; the shape of the failure does
    // not depend on it. What matters is that the transfer lasts MANY of them.
    let sched = Scheduler::new(SIZE, vec![Source::default()], &[N]).with_stall_timeout(1.0);

    // What the transport told the scheduler about its own concurrency, sampled
    // on every tick: the lowest cap it ran at, and the reason it gave.
    let lowest_limit = Arc::new(AtomicUsize::new(usize::MAX));
    let saw_starved_reason = Arc::new(AtomicUsize::new(0));
    let (ll, sr) = (lowest_limit.clone(), saw_starved_reason.clone());
    let mut observe = move |s: &Scheduler, _done: u64| {
        ll.fetch_min(s.active_limit(), Ordering::SeqCst);
        if let LimitReason::Starved { serving } = s.limit_reason() {
            sr.store(serving, Ordering::SeqCst);
        }
    };

    let t0 = std::time::Instant::now();
    let r = tokio::time::timeout(
        Duration::from_secs(90),
        run_transfer_observed(conn, vec![t], &[N], SIZE, &outs, sched, 20, &mut observe),
    )
    .await
    .expect("a starving origin must not hang the transfer");
    r.expect("the object must be delivered at a connection count the origin serves");
    eprintln!(
        "starved={} grants={} elapsed={:.2}s lowest_limit={} starved_reason={}",
        starved.load(Ordering::SeqCst),
        grants.load(Ordering::SeqCst),
        t0.elapsed().as_secs_f64(),
        lowest_limit.load(Ordering::SeqCst),
        saw_starved_reason.load(Ordering::SeqCst),
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

    let starved = starved.load(Ordering::SeqCst);
    assert!(
        starved >= N - ALLOWED,
        "the opening burst must actually have been starved, or this proves nothing"
    );
    // The opening burst is unavoidable: no client can know the limit before it has
    // been felt. Everything after it is the client failing to learn. Without a
    // response to starvation this origin starved another six requests every stall
    // timeout for the whole transfer — measured 49 on this fixture. Learning it
    // costs two rounds, because the cap halves rather than jumping: eight to four
    // starves six, four to two starves two more, and then nothing. Measured 8-10
    // across runs, the spread being a live connection re-requesting in the
    // instant a round is being judged. Three rounds' worth leaves that alone
    // while still separating "learned" from "kept asking" by a factor of five.
    assert!(
        starved <= 3 * (N - ALLOWED),
        "{starved} starved requests means the client kept re-asking for connections \
         the origin never serves"
    );
    assert_eq!(
        lowest_limit.load(Ordering::SeqCst),
        ALLOWED,
        "the transfer must settle at the concurrency the origin serves, not below it"
    );
    assert_eq!(
        saw_starved_reason.load(Ordering::SeqCst),
        ALLOWED,
        "the scheduler must carry WHY the count was lowered, so a front end can say so"
    );
}
