// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Minimal HTTP GET on top of hya-net's connector: redirects, whole-body
//! fetches, and a streaming download with progress.
//!
//! hya-net's own fetch paths are shaped for the transfer engine (range
//! scheduling, probes); the updater needs the opposite shape â€?follow the
//! `github.com -> objects.githubusercontent.com` redirect chain, then either
//! hand back a small body whole or stream a large one to disk reporting
//! progress. Plain `http://` targets stay supported because that is what the
//! mock server in the tests speaks.

use pdl_net::{header_lookup, Connector, MaybeTls, Target, TlsCapableConnector};
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A parsed absolute URL, just enough for a GET.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    pub tls: bool,
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn parse(url: &str) -> io::Result<Url> {
        let (tls, rest) = if let Some(r) = url.strip_prefix("https://") {
            (true, r)
        } else if let Some(r) = url.strip_prefix("http://") {
            (false, r)
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("not an http(s) URL: {url}"),
            ));
        };
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, "/"),
        };
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => (
                h,
                p.parse()
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad port"))?,
            ),
            _ => (authority, if tls { 443 } else { 80 }),
        };
        if host.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty host"));
        }
        Ok(Url {
            tls,
            host: host.to_string(),
            port,
            path: path.to_string(),
        })
    }

    /// Resolve a `Location` header against this URL (absolute, or
    /// origin-relative starting with `/`).
    fn join(&self, location: &str) -> io::Result<Url> {
        if location.starts_with("http://") || location.starts_with("https://") {
            Url::parse(location)
        } else if location.starts_with('/') {
            Ok(Url {
                path: location.to_string(),
                ..self.clone()
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unresolvable redirect: {location}"),
            ))
        }
    }
}

/// An open response: status line parsed, body not yet read.
struct Response {
    status: u16,
    head: String,
    stream: MaybeTls,
    /// Body bytes that arrived in the same read as the header terminator.
    prefix: Vec<u8>,
}

async fn open(url: &Url, user_agent: &str) -> io::Result<Response> {
    let conn = TlsCapableConnector::new()?;
    let target = if url.tls {
        Target::direct_tls(&url.host, url.port, &url.path)
    } else {
        Target::direct(&url.host, url.port, &url.path)
    };
    let mut stream = conn.connect(&target).await?;
    let default_port = (url.tls && url.port == 443) || (!url.tls && url.port == 80);
    let host_hdr = if default_port {
        url.host.clone()
    } else {
        format!("{}:{}", url.host, url.port)
    };
    let req = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        url.path, host_hdr, user_agent
    );
    stream.write_all(req.as_bytes()).await?;

    let mut buf = Vec::with_capacity(4096);
    let mut chunk = vec![0u8; 8192];
    let split = loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before response headers",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find_crlf2(&buf) {
            break i;
        }
        if buf.len() > 64 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response headers exceed 64 KB",
            ));
        }
    };
    let head = String::from_utf8_lossy(&buf[..split]).to_string();
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let prefix = buf[split..].to_vec();
    Ok(Response {
        status,
        head,
        stream,
        prefix,
    })
}

/// GET `url`, following up to 8 redirects, and require a 2xx.
async fn get(url: &str, user_agent: &str) -> io::Result<Response> {
    let mut u = Url::parse(url)?;
    for _ in 0..8 {
        let resp = open(&u, user_agent).await?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            let loc = header_lookup(&resp.head, "location").ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "redirect without Location")
            })?;
            u = u.join(loc.trim())?;
            continue;
        }
        if (200..300).contains(&resp.status) {
            return Ok(resp);
        }
        return Err(io::Error::other(format!(
            "server returned {} for {}",
            resp.status, u.path
        )));
    }
    Err(io::Error::other("too many redirects"))
}

/// GET a small object whole. The cap is a refusal, not a truncation.
pub async fn get_bytes(url: &str, user_agent: &str, cap: usize) -> io::Result<Vec<u8>> {
    let mut resp = get(url, user_agent).await?;
    let mut body = resp.prefix;
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let n = match resp.stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            // `Connection: close` endings are routinely unclean under TLS.
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        body.extend_from_slice(&chunk[..n]);
        if body.len() > cap {
            return Err(io::Error::other("response exceeds the fetch cap"));
        }
    }
    if header_lookup(&resp.head, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        return Ok(dechunk(&body));
    }
    Ok(body)
}

/// GET a large object to `dest`, reporting `(bytes_so_far, total)` after
/// every write. The callback returning `false` cancels the download; the
/// partial file is removed and `ErrorKind::Interrupted` comes back.
pub async fn download_to_file(
    url: &str,
    user_agent: &str,
    dest: &std::path::Path,
    mut progress: impl FnMut(u64, Option<u64>) -> bool + Send,
) -> io::Result<()> {
    let mut resp = get(url, user_agent).await?;
    if header_lookup(&resp.head, "transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false)
    {
        // Release assets and the mock server both state Content-Length;
        // streaming de-chunking is complexity this path does not need.
        return Err(io::Error::other("chunked download responses unsupported"));
    }
    let total: Option<u64> =
        header_lookup(&resp.head, "content-length").and_then(|v| v.trim().parse().ok());

    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = tokio::fs::File::create(dest).await?;
    let mut got: u64 = 0;
    if !resp.prefix.is_empty() {
        file.write_all(&resp.prefix).await?;
        got += resp.prefix.len() as u64;
        if !progress(got, total) {
            return cancel(dest).await;
        }
    }
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = match resp.stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        file.write_all(&chunk[..n]).await?;
        got += n as u64;
        if !progress(got, total) {
            return cancel(dest).await;
        }
        if let Some(t) = total {
            if got >= t {
                break;
            }
        }
    }
    file.flush().await?;
    drop(file);
    if let Some(t) = total {
        if got != t {
            let _ = std::fs::remove_file(dest);
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("download truncated: {got} of {t} bytes"),
            ));
        }
    }
    Ok(())
}

async fn cancel(dest: &std::path::Path) -> io::Result<()> {
    let _ = tokio::fs::remove_file(dest).await;
    Err(io::Error::new(
        io::ErrorKind::Interrupted,
        "download cancelled",
    ))
}

fn find_crlf2(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

/// De-frame a complete chunked body already held in memory.
fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(i) = rest.windows(2).position(|w| w == b"\r\n") {
        let Ok(line) = std::str::from_utf8(&rest[..i]) else {
            break;
        };
        let Ok(n) = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16) else {
            break;
        };
        if n == 0 {
            break;
        }
        let start = i + 2;
        if start + n > rest.len() {
            out.extend_from_slice(&rest[start..]);
            break;
        }
        out.extend_from_slice(&rest[start..start + n]);
        rest = &rest[(start + n + 2).min(rest.len())..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_parsing() {
        let u = Url::parse("https://api.github.com/repos/ja7ad/hydra/releases/latest").unwrap();
        assert_eq!(
            u,
            Url {
                tls: true,
                host: "api.github.com".into(),
                port: 443,
                path: "/repos/ja7ad/hydra/releases/latest".into()
            }
        );
        let u = Url::parse("http://127.0.0.1:8642/latest").unwrap();
        assert_eq!(u.port, 8642);
        assert!(!u.tls);
        let u = Url::parse("https://example.com").unwrap();
        assert_eq!(u.path, "/");
        assert!(Url::parse("ftp://example.com/x").is_err());
        assert!(Url::parse("https:///nohost").is_err());
    }

    #[test]
    fn redirect_join() {
        let base =
            Url::parse("https://github.com/ja7ad/hydra/releases/download/v1/x.tar.gz").unwrap();
        let abs = base
            .join("https://objects.githubusercontent.com/blob/1")
            .unwrap();
        assert_eq!(abs.host, "objects.githubusercontent.com");
        let rel = base.join("/other/path").unwrap();
        assert_eq!(rel.host, "github.com");
        assert_eq!(rel.path, "/other/path");
        assert!(base.join("no-scheme-relative").is_err());
    }

    #[test]
    fn dechunk_reassembles() {
        let body = b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n";
        assert_eq!(dechunk(body), b"Wikipedia");
    }
}
