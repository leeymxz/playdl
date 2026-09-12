// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! `playdl video` — download from video sites (YouTube, Bilibili, Douyin, ...).
//!
//! Video sites sign their media URLs, so this shells out to `yt-dlp` (the
//! same tool everyone else relies on) and streams its progress through.

use std::path::PathBuf;
use std::process::Command;

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
pub fn run(url: &str, opts: VideoOpts) -> i32 {
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
            c.arg("--no-warnings").arg("--progress");
            c
        }
        YtDlp::PythonModule(py) => {
            let mut c = Command::new(py);
            c.arg("-m").arg("yt_dlp");
            c.arg("--no-warnings").arg("--progress");
            c
        }
    };

    if opts.list_formats {
        cmd.arg("--list-formats");
    } else if opts.get_url {
        cmd.arg("--get-url");
    }

    if let Some(fmt) = &opts.format {
        cmd.arg("-f").arg(fmt);
    } else if !opts.list_formats && !opts.get_url {
        // best video + best audio, merged to mp4 by ffmpeg
        cmd.args(["-f", "bv*+ba/b"]);
        cmd.arg("--merge-output-format").arg("mp4");
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
    match cmd.status() {
        Ok(st) => st.code().unwrap_or(1),
        Err(e) => {
            eprintln!("playdl video: failed to run yt-dlp: {e}");
            1
        }
    }
}

/// Options for [`run`].
pub struct VideoOpts {
    pub output: Option<String>,
    pub list_formats: bool,
    pub format: Option<String>,
    pub playlist: bool,
    pub get_url: bool,
    pub ytdlp: Option<PathBuf>,
}
