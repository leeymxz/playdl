// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! A local mock release server for exercising the whole update flow by hand
//! before pointing anything at real GitHub releases.
//!
//! ```text
//! cargo run -p hya-updater --example mock_server
//! cargo run -p hya-updater --example mock_server -- 2.0.1
//! cargo run -p hya-updater --example mock_server -- 2.0.1 --size-mb 60 --rate-mbps 2
//! cargo run -p hya-updater --example mock_server -- 0.9.0 /path/to/real-archive.tar.gz
//! cargo run -p hya-updater --example mock_server -- 2.0.1 --rc 2.1.0-rc1
//!
//! # then, in another shell:
//! HYDRA_UPDATE_API=http://127.0.0.1:8642 hydra update        # CLI check
//! HYDRA_UPDATE_API=http://127.0.0.1:8642 hydra-gui           # GUI startup modal
//! ```
//!
//! Arguments (all optional):
//! - first positional: the version to advertise (default `9.9.9`)
//! - second positional: a real archive to serve as the GUI asset
//! - `--size-mb N`: size of the generated payload (default 24) — big enough
//!   that the GUI's progress bar visibly fills
//! - `--rate-mbps N`: throttle asset downloads to N MB/s (default 4; `0`
//!   sends at full speed)
//! - `--port N`: listen port (default 8642)
//! - `--rc VERSION`: also publish that version as a pre-release, listed on
//!   the `/releases` route the beta channel scans (`/releases/latest` still
//!   returns only the stable release, like GitHub)
//!
//! Both release assets are published: the GUI bundle (`hydra-…`) and the
//! standalone CLI archive (`hydra-cli-…`), so `hydra update` and the GUI
//! each find theirs. Generated bundles contain only file names no real
//! install has (`hydra-mock-payload`, …), so even a full "Update Now" run
//! swaps nothing on disk — it proves download progress, checksum,
//! extraction, and the finisher launch, harmlessly. Pass a real release
//! archive to rehearse an actual swap.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

struct Config {
    version: String,
    rc_version: Option<String>,
    archive_path: Option<String>,
    size_mb: usize,
    rate_mbps: usize,
    port: u16,
}

fn parse_args() -> Config {
    let mut cfg = Config {
        version: "9.9.9".into(),
        rc_version: None,
        archive_path: None,
        size_mb: 24,
        rate_mbps: 4,
        port: 8642,
    };
    let mut positional = 0;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--rc" {
            cfg.rc_version = Some(args.next().expect("--rc needs a version"));
            continue;
        }
        let mut flag = |name: &str| -> Option<usize> {
            if a == name {
                Some(
                    args.next()
                        .and_then(|v| v.parse().ok())
                        .unwrap_or_else(|| panic!("{name} needs a number")),
                )
            } else {
                None
            }
        };
        if let Some(v) = flag("--size-mb") {
            cfg.size_mb = v.clamp(1, 2048);
        } else if let Some(v) = flag("--rate-mbps") {
            cfg.rate_mbps = v;
        } else if let Some(v) = flag("--port") {
            cfg.port = v as u16;
        } else if positional == 0 {
            cfg.version = a;
            positional += 1;
        } else if positional == 1 {
            cfg.archive_path = Some(a);
            positional += 1;
        } else {
            eprintln!("mock_server: unexpected argument {a}");
            std::process::exit(2);
        }
    }
    cfg
}

fn main() {
    let cfg = parse_args();
    let version = &cfg.version;

    let os = if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "amd64"
    };
    let ext = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };

    // Two assets per release, mirroring a real one: the GUI bundle and the
    // standalone CLI archive. A path argument replaces the stable GUI bundle.
    println!(
        "generating {} MB mock payload{}...",
        cfg.size_mb,
        if cfg.archive_path.is_some() {
            " (CLI asset only; GUI asset comes from the given archive)"
        } else {
            ""
        }
    );
    let make_assets = |version: &str, archive_path: Option<&String>| -> Vec<(String, Vec<u8>)> {
        let gui = match archive_path {
            Some(p) => {
                let name = std::path::Path::new(p)
                    .file_name()
                    .expect("archive path has a file name")
                    .to_string_lossy()
                    .into_owned();
                (name, std::fs::read(p).expect("readable archive"))
            }
            None => (
                format!("hydra-{version}-{os}-{arch}.{ext}"),
                generate_bundle(version, os, arch, cfg.size_mb),
            ),
        };
        let cli = (
            format!("hydra-cli-{version}-{os}-{arch}.{ext}"),
            generate_bundle(version, os, arch, cfg.size_mb),
        );
        vec![gui, cli]
    };
    let stable_assets = make_assets(version, cfg.archive_path.as_ref());
    let rc_assets: Vec<(String, Vec<u8>)> = match &cfg.rc_version {
        // The pre-release's assets are named from the workspace manifest
        // version, which the release pipeline leaves without the tag's
        // `-rc` suffix (a `v0.3.2-rc` tag ships `hydra-0.3.2-…` files), so
        // the mock rehearses the updater's fallback to that spelling.
        Some(rc) => make_assets(pdl_updater::version_core(rc), None),
        None => Vec::new(),
    };

    let base = format!("http://127.0.0.1:{}", cfg.port);
    let release_json = |version: &str, prerelease: bool, assets: &[(String, Vec<u8>)]| -> String {
        let asset_json: Vec<String> = assets
            .iter()
            .map(|(name, bytes)| {
                format!(
                    "{{\"name\":\"{name}\",\"browser_download_url\":\"{base}/assets/{name}\",\"size\":{}}}",
                    bytes.len()
                )
            })
            .collect();
        format!(
            concat!(
                "{{\"tag_name\":\"v{v}\",\"name\":\"hydra {v}\",\"prerelease\":{pre},",
                "\"body\":\"## What's Changed\\n",
                "- Multi-source scheduler: smarter connection ramp\\n",
                "- **GUI**: faster list rendering on very large queues\\n",
                "- Fixed resume after a mid-transfer network change\\n\\n",
                "**Full Changelog**: https://github.com/leeymxz/playdl/compare\",",
                "\"html_url\":\"https://github.com/leeymxz/playdl/releases/tag/v{v}\",",
                "\"published_at\":\"2026-08-19T00:00:00Z\",",
                "\"assets\":[{assets},",
                "{{\"name\":\"SHA256SUMS.txt\",\"browser_download_url\":\"{base}/assets/SHA256SUMS.txt\",\"size\":0}}",
                "]}}"
            ),
            v = version,
            pre = prerelease,
            assets = asset_json.join(","),
            base = base
        )
    };
    let latest_json = release_json(version, false, &stable_assets);
    // GitHub lists newest first and keeps pre-releases out of `latest`; the
    // list route is what the beta channel scans.
    let list_json = match &cfg.rc_version {
        Some(rc) => format!("[{},{latest_json}]", release_json(rc, true, &rc_assets)),
        None => format!("[{latest_json}]"),
    };

    // One flat asset store (and one sums file) serves every release.
    let assets: Arc<Vec<(String, Vec<u8>)>> = Arc::new(
        stable_assets
            .into_iter()
            .chain(rc_assets)
            .collect::<Vec<_>>(),
    );
    let sums_body: String = assets
        .iter()
        .map(|(name, bytes)| format!("{}  {name}\n", sha256_hex(bytes)))
        .collect();

    let listener = match TcpListener::bind(("127.0.0.1", cfg.port)) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "mock_server: port {} is already in use — another mock_server is \
                 probably still running (stop it, or pass --port <other>).",
                cfg.port
            );
            std::process::exit(1);
        }
        Err(e) => panic!("bind mock port: {e}"),
    };
    println!("mock release server on {base}");
    println!("  latest release : v{version}");
    if let Some(rc) = &cfg.rc_version {
        println!("  pre-release    : v{rc} (beta channel only)");
    }
    for (name, bytes) in assets.iter() {
        println!(
            "  asset          : {name} ({:.1} MB)",
            bytes.len() as f64 / (1024.0 * 1024.0)
        );
    }
    if cfg.rate_mbps > 0 {
        println!("  download rate  : throttled to {} MB/s", cfg.rate_mbps);
    } else {
        println!("  download rate  : unthrottled");
    }
    println!();
    println!("try:");
    println!("  HYDRA_UPDATE_API={base} hydra update");
    println!("  HYDRA_UPDATE_API={base} hydra-gui");

    for stream in listener.incoming() {
        let Ok(sock) = stream else { continue };
        // Thread per connection: a throttled asset download must not block
        // the next client's API check.
        let latest_json = latest_json.clone();
        let list_json = list_json.clone();
        let sums_body = sums_body.clone();
        let assets = assets.clone();
        let rate = cfg.rate_mbps;
        std::thread::spawn(move || {
            serve_one(
                sock,
                &latest_json,
                &list_json,
                &sums_body,
                assets.as_slice(),
                rate,
            );
        });
    }
}

fn serve_one(
    mut sock: TcpStream,
    latest_json: &str,
    list_json: &str,
    sums_body: &str,
    assets: &[(String, Vec<u8>)],
    rate_mbps: usize,
) {
    let mut buf = [0u8; 8192];
    let mut req = Vec::new();
    loop {
        match sock.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
        }
    }
    let head = String::from_utf8_lossy(&req);
    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
    println!("  <- GET {path}");
    let (status, extra, body): (&str, String, &[u8]) =
        if path == "/repos/leeymxz/playdl/releases/latest" {
            ("200 OK", String::new(), latest_json.as_bytes())
        } else if path
            .split('?')
            .next()
            .is_some_and(|p| p == "/repos/leeymxz/playdl/releases")
        {
            // The release list the beta channel scans; `?per_page=…` allowed.
            ("200 OK", String::new(), list_json.as_bytes())
        } else if path == "/assets/SHA256SUMS.txt" {
            ("200 OK", String::new(), sums_body.as_bytes())
        } else if let Some((name, _)) = assets
            .iter()
            .find(|(name, _)| path == format!("/assets/{name}"))
        {
            // GitHub asset URLs redirect to a CDN; rehearse that hop.
            (
                "302 Found",
                format!("Location: /blob/{name}\r\n"),
                b"".as_slice(),
            )
        } else if let Some((_, bytes)) = assets
            .iter()
            .find(|(name, _)| path == format!("/blob/{name}"))
        {
            ("200 OK", String::new(), bytes.as_slice())
        } else {
            ("404 Not Found", String::new(), b"not found".as_slice())
        };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
        body.len()
    );
    if sock.write_all(head.as_bytes()).is_err() {
        return;
    }
    // Only the blob (the actual archive) is throttled — checks stay instant.
    let throttle = rate_mbps > 0 && path.starts_with("/blob/");
    if !throttle {
        let _ = sock.write_all(body);
        return;
    }
    // rate MB/s in 64 KB slices: sleep (64 KB / rate) per slice. Coarse but
    // plenty accurate for making a progress bar move at a believable pace.
    let slice = 64 * 1024;
    let pause =
        std::time::Duration::from_secs_f64(slice as f64 / (rate_mbps as f64 * 1024.0 * 1024.0));
    for chunk in body.chunks(slice) {
        if sock.write_all(chunk).is_err() {
            return; // client cancelled mid-download
        }
        std::thread::sleep(pause);
    }
}

/// A harmless bundle: its file names collide with nothing a real install
/// has, so `apply` skips every one of them. `size_mb` of incompressible
/// filler keeps the compressed archive that size, so downloads take long
/// enough to watch.
fn generate_bundle(version: &str, os: &str, arch: &str, size_mb: usize) -> Vec<u8> {
    let dir = format!("hydra-{version}-{os}-{arch}");
    let payload = format!("mock hydra {version} payload\n");
    let filler = pseudorandom_bytes(size_mb * 1024 * 1024);
    let files: &[(&str, &[u8])] = &[
        ("hydra-mock-payload", payload.as_bytes()),
        ("MOCK-RELEASE.txt", b"generated by examples/mock_server.rs"),
        ("MOCK-PAYLOAD.bin", filler.as_slice()),
    ];
    if cfg!(target_os = "windows") {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, content) in files {
            z.start_file(format!("{dir}/{name}"), opts).unwrap();
            z.write_all(content).unwrap();
        }
        z.finish().unwrap().into_inner()
    } else {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut t = tar::Builder::new(gz);
        for (name, content) in files {
            let mut h = tar::Header::new_gnu();
            h.set_size(content.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            t.append_data(&mut h, format!("{dir}/{name}"), *content)
                .unwrap();
        }
        t.into_inner().unwrap().finish().unwrap()
    }
}

/// Incompressible filler (xorshift64*): gzip/deflate cannot shrink it, so
/// the served archive is as large as asked.
fn pseudorandom_bytes(n: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n);
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    while out.len() < n {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        let v = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.truncate(n);
    out
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
