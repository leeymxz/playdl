// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! A small real HTTP origin for the ABI tests.
//!
//! Real sockets rather than the in-process duplex connector hya-net's own tests
//! use, and that is the point: these tests exercise the library the way a C
//! program does — through `hydra_engine_create`, over a TCP connection, into a
//! file on disk. A test that stubbed the transport would not catch an ABI
//! problem, which is the entire class of bug this suite exists for.

use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// What the origin should do.
#[derive(Clone, Copy, Debug)]
pub struct Behaviour {
    /// Advertise `Accept-Ranges: bytes` and honour `Range`.
    pub ranges: bool,
    /// Send a strong `ETag`.
    pub validator: bool,
    /// Sleep this long between body chunks, to make a transfer slow enough to
    /// pause deterministically.
    pub delay_ms: u64,
    /// Bytes per body chunk when `delay_ms` is non-zero.
    pub chunk: usize,
    /// Answer everything with this status instead, when set.
    pub force_status: Option<u16>,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            ranges: true,
            validator: true,
            delay_ms: 0,
            chunk: 64 * 1024,
            force_status: None,
        }
    }
}

/// A running origin. Dropping it stops the listener.
pub struct Origin {
    pub port: u16,
    /// The bytes being served. Held so a test can compare against them without
    /// keeping its own copy.
    #[allow(dead_code)]
    pub body: Arc<Vec<u8>>,
    /// Requests answered, for a test that wants to assert on connection reuse.
    #[allow(dead_code)]
    pub requests: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Drop for Origin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock the accept loop so the thread can observe the stop flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

impl Origin {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// Deterministic pseudo-random body, so a wrong byte at a wrong offset is
/// detectable rather than hidden by a run of zeroes.
pub fn make_body(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len + 8);
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    while v.len() < len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.truncate(len);
    v
}

/// Start an origin serving `body` on a loopback port.
pub fn serve(body: Vec<u8>, behaviour: Behaviour) -> Origin {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let port = listener.local_addr().unwrap().port();
    let body = Arc::new(body);
    let stop = Arc::new(AtomicBool::new(false));
    let requests = Arc::new(AtomicU64::new(0));

    let (b, s, r) = (body.clone(), stop.clone(), requests.clone());
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            if s.load(Ordering::Relaxed) {
                break;
            }
            let Ok(sock) = conn else { continue };
            let (b, s, r) = (b.clone(), s.clone(), r.clone());
            std::thread::spawn(move || {
                let _ = handle(sock, &b, behaviour, &s, &r);
            });
        }
    });
    Origin {
        port,
        body,
        requests,
        stop,
    }
}

fn read_head(sock: &mut TcpStream) -> std::io::Result<Option<String>> {
    use std::io::Read;
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match sock.read(&mut byte)? {
            0 => return Ok(None),
            _ => buf.push(byte[0]),
        }
        if buf.ends_with(b"\r\n\r\n") {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }
        if buf.len() > 16 * 1024 {
            return Ok(None);
        }
    }
}

fn handle(
    mut sock: TcpStream,
    body: &[u8],
    b: Behaviour,
    stop: &AtomicBool,
    requests: &AtomicU64,
) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    // Keep-alive: hya-net pools connections, and a server that closed after
    // every response would hide whether pooling works at all.
    loop {
        let Some(head) = read_head(&mut sock)? else {
            return Ok(());
        };
        requests.fetch_add(1, Ordering::Relaxed);
        let start = head.lines().next().unwrap_or("");
        let method = start.split(' ').next().unwrap_or("");
        let range = head
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
            .and_then(|l| parse_range(l, body.len()));
        // hya-net asks for `Connection: close` on the single-stream path, which
        // has no Content-Length to stop at and reads to EOF. A test server that
        // ignored the request header would hang that path forever — and would be
        // testing the wrong thing, since a real server honours it.
        let close = head
            .lines()
            .any(|l| l.to_ascii_lowercase().starts_with("connection: close"));

        if let Some(code) = b.force_status {
            let msg =
                format!("HTTP/1.1 {code} Nope\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            sock.write_all(msg.as_bytes())?;
            return Ok(());
        }

        let validator = if b.validator {
            "ETag: \"v1-strong\"\r\n"
        } else {
            ""
        };
        let accept = if b.ranges {
            "Accept-Ranges: bytes\r\n"
        } else {
            ""
        };

        if method == "HEAD" {
            let h = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{accept}{validator}\r\n",
                body.len()
            );
            sock.write_all(h.as_bytes())?;
            if close {
                return Ok(());
            }
            continue;
        }

        let (lo, hi) = match (b.ranges, range) {
            (true, Some((lo, hi))) => (lo, hi),
            _ => (0usize, body.len()),
        };
        let slice = &body[lo.min(body.len())..hi.min(body.len())];
        let h = if b.ranges && slice.len() != body.len() {
            format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\n{accept}{validator}\r\n",
                slice.len(),
                lo,
                hi.saturating_sub(1),
                body.len()
            )
        } else {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n{accept}{validator}\r\n",
                slice.len()
            )
        };
        sock.write_all(h.as_bytes())?;

        if b.delay_ms == 0 {
            sock.write_all(slice)?;
        } else {
            for part in slice.chunks(b.chunk.max(1)) {
                if stop.load(Ordering::Relaxed) || sock.write_all(part).is_err() {
                    return Ok(());
                }
                let _ = sock.flush();
                std::thread::sleep(std::time::Duration::from_millis(b.delay_ms));
            }
        }
        let _ = sock.flush();
        if close {
            return Ok(());
        }
    }
}

fn parse_range(line: &str, total: usize) -> Option<(usize, usize)> {
    let spec = line.split(':').nth(1)?.trim();
    let spec = spec.strip_prefix("bytes=")?;
    let (a, z) = spec.split_once('-')?;
    let lo: usize = a.trim().parse().ok()?;
    let hi = match z.trim() {
        "" => total,
        v => v.parse::<usize>().ok()? + 1,
    };
    Some((lo, hi.min(total)))
}
