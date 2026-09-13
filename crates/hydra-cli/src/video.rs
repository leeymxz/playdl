// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! `playdl video` — download from video sites (YouTube, Bilibili, Douyin, ...).
//!
//! Video sites sign their media URLs, so this shells out to `yt-dlp` (the
//! same tool everyone else relies on) and streams its progress through.

use std::io::Read;
use std::path::PathBuf;
use std::process::Command;

/// A real Chrome UA so video sites (Bilibili, Douyin, Kuaishou, ...) do not
/// 412/403 the Python-based yt-dlp client.
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// True when `url` is a signed direct media link that yt-dlp cannot parse
/// but the PlayDL engine can download as-is. WeChat Channels (视频号) share
/// links live on `finder.video.qq.com`; other sites' direct CDN links with
/// signature query params also fit.
fn is_direct_media_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("finder.video.qq.com")
        || (lower.contains("encfilekey=") && lower.contains("finder"))
        || lower.contains("/stodownload?")
        || lower.contains("video.qq.com")
}

/// Rewrite share URLs into the canonical form the extractors accept.
/// Returns an owned String; the input is returned as-is when no rewrite
/// applies (or when parsing fails).
fn normalize_share_url(url: &str) -> String {
    // Douyin share cards: douyin.com/jingxuan?modal_id=123456... (and the
    // `v.douyin.com/xxx` short links) resolve to a video page yt-dlp knows.
    // Handle the direct "精选" form here; short links are followed by the
    // extractor itself.
    let lower = url.to_ascii_lowercase();
    if lower.contains("douyin.com/jingxuan") || lower.contains("douyin.com/video") {
        if let Some(modal) = extract_query_param(url, "modal_id") {
            let base = if lower.contains("douyin.com/jingxuan") {
                "https://www.douyin.com/video/"
            } else {
                "https://www.douyin.com/video/"
            };
            return format!("{base}{modal}");
        }
    }
    url.to_string()
}

/// Extract a single query parameter value (URL-decoded) from a URL string.
fn extract_query_param(url: &str, name: &str) -> Option<String> {
    let q = url.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        if k == name {
            let v = it.next().unwrap_or("");
            return Some(percent_decode(v));
        }
    }
    None
}

/// Minimal percent-decoding (utf-8 aware) for query values.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Pick a browser whose cookie DB exists on this machine for
/// `--cookies-from-browser`. yt-dlp reads them itself; we only name the
/// browser with the best chance of having logged-in sessions.
/// Locate the yt-dlp executable: explicit flag, same-dir, then PATH,
/// then the `python -m yt_dlp` fallback.
fn find_ytdlp(explicit: Option<&PathBuf>) -> Option<YtDlp> {
    if let Some(p) = explicit {
        if p.exists() {
            return Some(YtDlp::Exe(p.clone()));
        }
        // explicit but missing: let the caller report it clearly
        return Some(YtDlp::Exe(p.clone()));
    }
    // Same directory as this binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in ["yt-dlp.exe", "yt-dlp", "yt-dlp.exe"] {
                let p = dir.join(name);
                if p.exists() {
                    return Some(YtDlp::Exe(p));
                }
            }
        }
    }
    // PATH.
    for name in ["yt-dlp", "yt-dlp.exe"] {
        if let Ok(Some(p)) = which(name) {
            return Some(YtDlp::Exe(p));
        }
    }
    // python -m yt_dlp (pip install)
    for py in ["python", "python3", "py"] {
        if let Ok(Some(p)) = which(py) {
            return Some(YtDlp::PythonModule(p));
        }
    }
    None
}

/// How yt-dlp is run.
enum YtDlp {
    /// A standalone executable (`yt-dlp`, `yt-dlp.exe`).
    Exe(PathBuf),
    /// Via a Python interpreter: `python -m yt_dlp`.
    PythonModule(PathBuf),
}

/// Minimal PATH lookup (no external crate).
fn which(name: &str) -> std::io::Result<Option<PathBuf>> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// The `playdl video` entry point. Returns a process exit code.
pub fn run(url_in: &str, opts: VideoOpts) -> i32 {
    // Normalise share links that yt-dlp's extractors do not recognise:
    //   - Douyin "精选" pages  douyin.com/jingxuan?modal_id=<id>
    //     -> canonical       douyin.com/video/<id>
    let url = normalize_share_url(url_in);
    let url = url.as_str();
    // Direct WeChat Channels (视频号) media URLs carry their own signature
    // (encfilekey/token/sign) and yt-dlp has no extractor for them, but the
    // signed URL is a plain HTTP(S) object PlayDL can range-fetch directly.
    // Forward those straight to the engine instead of yt-dlp.
    if is_direct_media_url(url) {
        eprintln!("playdl video: direct media URL detected — downloading with the PlayDL engine (multi-connection)");
        if opts.get_url {
            println!("{url}");
            return 0;
        }
        if opts.list_formats {
            eprintln!("playdl video: a direct media URL has a single format");
            return 0;
        }
        // Re-exec as a normal download so the full engine (parallel ranges,
        // resume, progress) does the work.
        let exe = std::env::current_exe().unwrap_or_else(|_| "playdl".into());
        let mut cmd = Command::new(&exe);
        cmd.arg("-x").arg("8");
        if let Some(out) = &opts.output {
            cmd.arg("-o").arg(out);
        }
        cmd.arg(url);
        return match cmd.status() {
            Ok(st) => st.code().unwrap_or(1),
            Err(e) => {
                eprintln!("playdl video: failed to start downloader: {e}");
                1
            }
        };
    }

    let yt = match find_ytdlp(opts.ytdlp.as_ref()) {
        Some(y) => y,
        None => {
            eprintln!(
                "playdl video: yt-dlp not found\n\
                 install it with:  python -m pip install yt-dlp\n\
                 or place yt-dlp.exe next to this binary"
            );
            return 1;
        }
    };

    let mut cmd = match &yt {
        YtDlp::Exe(p) => {
            if !p.exists() {
                eprintln!("playdl video: yt-dlp not found at {}", p.display());
                return 1;
            }
            let mut c = Command::new(p);
            // --newline: emit progress on its own line so a console/pipe can
            // follow it in real time instead of the carriage-return dance.
            c.arg("--no-warnings").arg("--progress").arg("--newline");
            // Sites like Bilibili/Douyin reject plain Python clients with
            // HTTP 412/403; present a real Chrome UA so extraction works.
            c.arg("--user-agent").arg(BROWSER_UA);
            // NOTE: no automatic `--cookies-from-browser` here. Modern
            // Chrome/Edge encrypt the cookie DB (DPAPI + app-bound keys) and
            // yt-dlp's decryption fails while the browser is running, which
            // aborts the whole download with "Failed to decrypt with DPAPI".
            // Users who need logged-in content pass --cookies <file> instead.
            c
        }
        YtDlp::PythonModule(py) => {
            let mut c = Command::new(py);
            c.arg("-m").arg("yt_dlp");
            c.arg("--no-warnings").arg("--progress").arg("--newline");
            c.arg("--user-agent").arg(BROWSER_UA);
            c
        }
    };

    if opts.list_formats {
        cmd.arg("--list-formats");
    } else if opts.get_url {
        cmd.arg("--get-url");
    }

    // Optional cookies file (e.g. `--cookies cookies.txt`) for sites that
    // gate content behind a login — Bilibili members-only, YouTube age
    // checks, etc. No automatic browser-cookie probing: decrypting a live
    // Chrome/Edge cookie DB fails with DPAPI errors and aborts the run.
    if let Some(cf) = &opts.cookies {
        if cf.exists() {
            cmd.arg("--cookies").arg(cf);
        } else {
            eprintln!("playdl video: cookies file not found: {}", cf.display());
            return 1;
        }
    }

    if let Some(fmt) = &opts.format {
        cmd.arg("-f").arg(fmt);
    } else if !opts.list_formats && !opts.get_url {
        // Prefer widely-compatible H.264 video + m4a audio (plays everywhere,
        // including Windows' built-in players). AV1/Opus is the fallback:
        // YouTube's best bitrate is AV1 these days, but many players cannot
        // decode it (audio-only playback or black frames), so only use it
        // when no H.264 option exists.
        cmd.args([
            "-f",
            "bv*[vcodec^=avc1]+ba[acodec^=mp4a]/bv*[vcodec^=avc1]+ba/b",
        ]);
        cmd.arg("--merge-output-format").arg("mp4");
        // Force the final container to .mp4 even when the best audio is Opus
        // (remux after merge so every player can open the file).
        cmd.arg("--remux-video").arg("mp4");
    }

    if opts.playlist {
        cmd.arg("--yes-playlist");
    } else {
        cmd.arg("--no-playlist");
    }

    if let Some(out) = &opts.output {
        cmd.arg("-o").arg(out);
    } else {
        cmd.arg("-o").arg("%(title)s.%(ext)s");
    }

    cmd.arg(url);

    match &yt {
        YtDlp::Exe(p) => eprintln!("playdl video: {} {}", p.display(), url),
        YtDlp::PythonModule(py) => eprintln!("playdl video: {} -m yt_dlp {}", py.display(), url),
    }
    let code = match cmd.status() {
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("playdl video: failed to run yt-dlp: {e}");
            1
        }
    };
    // When launched from the GUI into its own console (CREATE_NEW_CONSOLE)
    // the window closes on exit and the error scrolls out of sight. On
    // failure, hold the window open with a hint so the reason stays visible.
    if code != 0 {
        eprintln!();
        eprintln!("playdl video: 下载失败（退出码 {code}）。请检查上面的错误信息。");
        eprintln!("常见原因：网络不通、需要登录/会员、站点风控（B站 412）等。");
        eprintln!("按回车键关闭此窗口…");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

/// Options for [`run`].
pub struct VideoOpts {
    pub output: Option<String>,
    pub list_formats: bool,
    pub format: Option<String>,
    pub playlist: bool,
    pub get_url: bool,
    pub ytdlp: Option<PathBuf>,
    /// Optional Netscape-format cookies file for logged-in content.
    pub cookies: Option<PathBuf>,
}
