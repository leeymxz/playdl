// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end update flow against a mock release server: check â†?pick asset
//! â†?download (with a redirect hop, progress, checksum) â†?extract â†?apply.
//!
//! This is the same code path the GUI's "Update Now" and the CLI's
//! `hydra update` run â€?only the endpoint differs, and the endpoint is a
//! parameter. Once the flow is proven on every platform, production simply
//! uses the GitHub default in [`pdl_updater::api_base`].

use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Serve a canned mock release over plain HTTP on an ephemeral port.
///
/// Routes:
/// - `GET /repos/ja7ad/hydra/releases/latest` â€?release JSON pointing back
///   at this server
/// - `GET /assets/<name>` â€?302 redirect to `/blob/<name>` (GitHub's asset
///   URLs redirect to a CDN; the client must follow)
/// - `GET /blob/<name>` â€?the archive bytes
/// - `GET /assets/SHA256SUMS.txt` â€?checksums for the archive
async fn mock_server(
    archive_name: String,
    archive: Vec<u8>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let base = format!("http://127.0.0.1:{port}");

    let sum = {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("hydra-mock-sum-{port}"));
        std::fs::File::create(&tmp)
            .unwrap()
            .write_all(&archive)
            .unwrap();
        let s = pdl_updater::file_sha256(&tmp).unwrap();
        let _ = std::fs::remove_file(&tmp);
        s
    };
    let release_json = format!(
        concat!(
            "{{\"tag_name\":\"v9.9.9\",\"name\":\"hydra 9.9.9\",",
            "\"body\":\"## What's Changed\\n- Everything is faster\\n- **Bold** fix\",",
            "\"html_url\":\"{base}/releases/v9.9.9\",",
            "\"published_at\":\"2026-08-19T00:00:00Z\",",
            "\"assets\":[",
            "{{\"name\":\"{name}\",\"browser_download_url\":\"{base}/assets/{name}\",\"size\":{size}}},",
            "{{\"name\":\"SHA256SUMS.txt\",\"browser_download_url\":\"{base}/assets/SHA256SUMS.txt\",\"size\":0}}",
            "]}}"
        ),
        base = base,
        name = archive_name,
        size = archive.len()
    );
    let sums_body = format!("{sum}  {archive_name}\n");

    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let release_json = release_json.clone();
            let sums_body = sums_body.clone();
            let archive = archive.clone();
            let archive_name = archive_name.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&req);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, extra, body): (&str, String, Vec<u8>) =
                    if path == "/repos/ja7ad/hydra/releases/latest" {
                        ("200 OK", String::new(), release_json.into_bytes())
                    } else if path == format!("/assets/{archive_name}") {
                        (
                            "302 Found",
                            format!("Location: /blob/{archive_name}\r\n"),
                            Vec::new(),
                        )
                    } else if path == format!("/blob/{archive_name}") {
                        ("200 OK", String::new(), archive)
                    } else if path == "/assets/SHA256SUMS.txt" {
                        ("200 OK", String::new(), sums_body.into_bytes())
                    } else {
                        ("404 Not Found", String::new(), b"not found".to_vec())
                    };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, handle)
}

/// A minimal release archive: one bundle directory holding fake binaries.
fn make_tar_gz(bundle_dir: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (name, content) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, format!("{bundle_dir}/{name}"), *content)
            .unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

fn make_zip(bundle_dir: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut z = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, content) in files {
        z.start_file(format!("{bundle_dir}/{name}"), opts).unwrap();
        z.write_all(content).unwrap();
    }
    z.finish().unwrap().into_inner()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("hydra-updater-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[tokio::test]
async fn full_gui_update_flow_against_mock() {
    let files: &[(&str, &[u8])] = &[
        ("hydra-gui", b"NEW GUI v9.9.9"),
        ("hydra", b"NEW CLI v9.9.9"),
        ("hydra-updater", b"NEW UPDATER v9.9.9"),
    ];
    let bundle = "hydra-9.9.9-testos-testarch";
    let archive_name = "hydra-9.9.9-testos-testarch.tar.gz".to_string();
    let archive = make_tar_gz(bundle, files);
    let (base, server) = mock_server(archive_name.clone(), archive).await;

    // 1. Check: the mock reports a newer release with notes and assets.
    let rel = pdl_updater::check_latest_at(&base, "ja7ad/hydra", "hydra-test/0.0")
        .await
        .unwrap();
    assert_eq!(rel.version(), "9.9.9");
    assert!(pdl_updater::is_newer(rel.version(), "0.2.3"));
    assert!(rel.body.contains("Everything is faster"));
    let asset = rel.asset(&archive_name).expect("asset listed");

    // 2. Download through the redirect hop, watching progress monotonicity.
    let stage = temp_dir("stage");
    let dest = stage.join(&archive_name);
    let seen = Arc::new(AtomicU64::new(0));
    let seen2 = seen.clone();
    pdl_updater::http::download_to_file(
        &asset.browser_download_url,
        "hydra-test/0.0",
        &dest,
        move |got, total| {
            assert!(got >= seen2.swap(got, Ordering::SeqCst));
            assert!(total.is_some());
            true
        },
    )
    .await
    .unwrap();
    assert!(seen.load(Ordering::SeqCst) > 0);

    // 3. Verify against the published checksums.
    let sums = pdl_updater::http::get_bytes(
        &rel.asset("SHA256SUMS.txt").unwrap().browser_download_url,
        "hydra-test/0.0",
        1024 * 1024,
    )
    .await
    .unwrap();
    let want = pdl_updater::sum_for(&String::from_utf8(sums).unwrap(), &archive_name).unwrap();
    assert_eq!(pdl_updater::file_sha256(&dest).unwrap(), want);

    // 4. Extract; the bundle directory becomes the root.
    let extracted = pdl_updater::extract(&dest, &stage.join("unpacked")).unwrap();
    assert!(extracted.ends_with(bundle));
    assert!(extracted.join("hydra-gui").is_file());

    // 5. Apply into a fake install dir: existing files are replaced, absent
    //    ones are skipped, and the swap is what the finisher bin runs.
    let install = temp_dir("install");
    std::fs::write(install.join("hydra-gui"), b"OLD GUI").unwrap();
    std::fs::write(install.join("hydra"), b"OLD CLI").unwrap();
    // No hydra-updater in the install dir: it must be skipped, not created.
    let report = pdl_updater::apply(&extracted, &install).unwrap();
    assert_eq!(
        std::fs::read(install.join("hydra-gui")).unwrap(),
        b"NEW GUI v9.9.9"
    );
    assert_eq!(
        std::fs::read(install.join("hydra")).unwrap(),
        b"NEW CLI v9.9.9"
    );
    assert!(!install.join("hydra-updater").exists());
    assert_eq!(report.replaced.len(), 2);
    assert!(report.skipped.iter().any(|s| s == "hydra-updater"));

    server.abort();
    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_dir_all(&install);
}

#[tokio::test]
async fn zip_archives_extract_and_apply() {
    let files: &[(&str, &[u8])] = &[("hydra-gui.exe", b"NEW GUI"), ("hydra.exe", b"NEW CLI")];
    let bundle = "hydra-9.9.9-windows-amd64";
    let stage = temp_dir("zip");
    let archive = stage.join("hydra-9.9.9-windows-amd64.zip");
    std::fs::write(&archive, make_zip(bundle, files)).unwrap();

    let extracted = pdl_updater::extract(&archive, &stage.join("unpacked")).unwrap();
    assert!(extracted.join("hydra-gui.exe").is_file());

    let install = temp_dir("zip-install");
    std::fs::write(install.join("hydra-gui.exe"), b"OLD").unwrap();
    pdl_updater::apply(&extracted, &install).unwrap();
    assert_eq!(
        std::fs::read(install.join("hydra-gui.exe")).unwrap(),
        b"NEW GUI"
    );

    let _ = std::fs::remove_dir_all(&stage);
    let _ = std::fs::remove_dir_all(&install);
}

#[tokio::test]
async fn cancelled_download_removes_the_partial_file() {
    let bundle = "hydra-9.9.9-testos-testarch";
    let archive_name = "hydra-9.9.9-testos-testarch.tar.gz".to_string();
    // Large enough that the body cannot ride in with the response head.
    let big: Vec<u8> = (0..1_000_000u32).map(|i| i as u8).collect();
    let archive = make_tar_gz(bundle, &[("hydra-gui", big.as_slice())]);
    let (base, server) = mock_server(archive_name.clone(), archive).await;

    let stage = temp_dir("cancel");
    let dest = stage.join(&archive_name);
    let err = pdl_updater::http::download_to_file(
        &format!("{base}/assets/{archive_name}"),
        "hydra-test/0.0",
        &dest,
        |_got, _total| false, // cancel on the first progress report
    )
    .await
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
    assert!(!dest.exists());

    server.abort();
    let _ = std::fs::remove_dir_all(&stage);
}

/// Serve only the two check routes: `/releases/latest` with the stable
/// release and `/releases` with the full list (rc first, like GitHub's
/// newest-first order).
async fn mock_channel_server(
    stable: &str,
    rc: Option<&str>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let release = |v: &str, pre: bool| format!("{{\"tag_name\":\"v{v}\",\"prerelease\":{pre}}}");
    let latest_json = release(stable, false);
    let list_json = match rc {
        Some(rc) => format!("[{},{latest_json}]", release(rc, true)),
        None => format!("[{latest_json}]"),
    };
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let latest_json = latest_json.clone();
            let list_json = list_json.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut req = Vec::new();
                loop {
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&req);
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let (status, body) = if path == "/repos/ja7ad/hydra/releases/latest" {
                    ("200 OK", latest_json)
                } else if path.split('?').next() == Some("/repos/ja7ad/hydra/releases") {
                    ("200 OK", list_json)
                } else {
                    ("404 Not Found", "not found".to_string())
                };
                let head = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(body.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (base, handle)
}

#[tokio::test]
async fn beta_channel_offers_the_rc_only_while_it_is_ahead() {
    let ua = "hydra-test/0.0";

    // rc ahead of stable: stable channel stays put, beta gets the rc.
    let (base, server) = mock_channel_server("0.2.4", Some("0.3.0-rc1")).await;
    let stable = pdl_updater::check_channel_at(&base, "ja7ad/hydra", ua, false)
        .await
        .unwrap();
    assert_eq!(stable.version(), "0.2.4");
    let beta = pdl_updater::check_channel_at(&base, "ja7ad/hydra", ua, true)
        .await
        .unwrap();
    assert_eq!(beta.version(), "0.3.0-rc1");
    server.abort();

    // Stable caught up with the rc's core version: beta falls back to it.
    let (base, server) = mock_channel_server("0.3.0", Some("0.3.0-rc1")).await;
    let beta = pdl_updater::check_channel_at(&base, "ja7ad/hydra", ua, true)
        .await
        .unwrap();
    assert_eq!(beta.version(), "0.3.0");
    server.abort();

    // No pre-release published: both channels serve the stable release.
    let (base, server) = mock_channel_server("0.2.4", None).await;
    let beta = pdl_updater::check_channel_at(&base, "ja7ad/hydra", ua, true)
        .await
        .unwrap();
    assert_eq!(beta.version(), "0.2.4");
    server.abort();
}

#[tokio::test]
async fn up_to_date_release_is_not_an_upgrade() {
    let archive = make_tar_gz("hydra-9.9.9-x-y", &[("hydra", b"x")]);
    let (base, server) = mock_server("hydra-9.9.9-x-y.tar.gz".into(), archive).await;
    let rel = pdl_updater::check_latest_at(&base, "ja7ad/hydra", "hydra-test/0.0")
        .await
        .unwrap();
    // A client already on (or past) the published version stays put.
    assert!(!pdl_updater::is_newer(rel.version(), "9.9.9"));
    assert!(!pdl_updater::is_newer(rel.version(), "10.0.0"));
    server.abort();
}
