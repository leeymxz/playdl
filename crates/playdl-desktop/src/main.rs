// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! PlayDL Desktop GUI - a real double-click download manager.
//!
//! Runs a tiny local HTTP server that serves a polished TDM-style web UI,
//! and shells out to the `playdl` CLI for the actual downloading.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

// ---------------- app state ----------------

struct Job {
    id: u64,
    url: String,
    file: String,
    status: String, // queued | running | done | failed
    progress: f64,
    speed: f64, // bytes/sec (estimated)
    done_bytes: u64,
    total_bytes: u64,
    error: String,
    child: Option<Child>,
    last_seen: std::time::Instant,
    started_bytes: u64,
    started_at: std::time::Instant,
}

impl Job {
    fn new(id: u64, url: String, file: String) -> Self {
        Self {
            id,
            url,
            file,
            status: "queued".into(),
            progress: 0.0,
            speed: 0.0,
            done_bytes: 0,
            total_bytes: 0,
            error: String::new(),
            child: None,
            last_seen: std::time::Instant::now(),
            started_bytes: 0,
            started_at: std::time::Instant::now(),
        }
    }
}

#[derive(Default)]
struct State {
    jobs: Vec<Job>,
    next_id: u64,
}
type Shared = Arc<Mutex<State>>;

// ---------------- helpers ----------------

fn sanitize_filename(url: &str) -> String {
    let path = url
        .split('?')
        .next()
        .unwrap_or("download")
        .trim_end_matches('/');
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or("download")
        .to_string();
    if name.is_empty() || name == "download" {
        "download.bin".into()
    } else {
        name
    }
}

fn file_size(path: &std::path::Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Find the playdl binary: same dir, then cwd, then PATH.
fn find_playdl() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    for name in ["playdl.exe", "pdl.exe", "playdl", "pdl"] {
        let p = dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    for name in ["playdl.exe", "pdl.exe"] {
        if let Ok(cwd) = std::env::current_dir() {
            let p = cwd.join(name);
            if p.exists() {
                return Some(p);
            }
        }
    }
    None
}

// ---------------- background worker ----------------

/// Poll running children, update progress, reap finished jobs.
fn pump(state: &Shared) {
    let mut s = state.lock().unwrap();
    let now = std::time::Instant::now();
    let mut done_idx: Vec<usize> = Vec::new();

    for (i, job) in s.jobs.iter_mut().enumerate() {
        if job.status != "running" {
            continue;
        }
        // measure speed
        let elapsed = now.duration_since(job.started_at).as_secs_f64();
        let current = file_size(std::path::Path::new(&job.file));
        if elapsed > 0.5 {
            job.speed = (current - job.started_bytes) as f64 / elapsed;
        }
        job.done_bytes = current;
        // estimate total from progress sidecar if available
        if let Some(child) = job.child.as_mut() {
            if let Ok(Some(status)) = child.try_wait() {
                // finished
                if status.success() {
                    job.status = "done".into();
                    job.progress = 100.0;
                } else {
                    job.status = "failed".into();
                    job.error = format!("exit code {:?}", status.code());
                }
                job.child = None;
                done_idx.push(i);
            }
        }
    }
    drop(s);
}

// ---------------- HTML UI ----------------

const INDEX_HTML: &str = r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>PlayDL 下载器</title>
<style>
  :root {
    --bg: #0f1626;
    --panel: #161f38;
    --card: #1b2747;
    --accent: #00c8ff;
    --accent2: #00ff9d;
    --text: #dce6f5;
    --dim: #7a8aa8;
    --danger: #ff5c7a;
    --warn: #ffc93c;
  }
  * { margin: 0; padding: 0; box-sizing: border-box; }
  body {
    background: radial-gradient(1200px 600px at 80% -10%, #123a5e55, transparent),
                radial-gradient(900px 500px at 0% 100%, #0a2f5055, transparent),
                var(--bg);
    color: var(--text);
    font-family: "Segoe UI", "Microsoft YaHei", system-ui, sans-serif;
    min-height: 100vh;
  }
  .header {
    display: flex; align-items: center; gap: 14px;
    padding: 18px 28px;
    background: linear-gradient(90deg, #101a30, #0c2440);
    border-bottom: 1px solid #223a66;
  }
  .logo {
    width: 46px; height: 46px;
    border-radius: 12px;
    display: grid; place-items: center;
    background: linear-gradient(135deg, #00c8ff, #0070ff);
    font-size: 24px; color: #fff;
    box-shadow: 0 4px 16px #00c8ff44;
  }
  .title { font-size: 20px; font-weight: 700; letter-spacing: 0.5px; }
  .title span { color: var(--accent); }
  .ver { color: var(--dim); font-size: 12px; margin-top: 2px; }
  .engine {
    margin-left: auto; font-size: 13px;
    padding: 6px 14px; border-radius: 20px;
    background: #0d2f22; color: var(--accent2);
    border: 1px solid #1d5c44;
  }
  .main { max-width: 860px; margin: 24px auto; padding: 0 20px; }
  .input-row {
    display: flex; gap: 12px;
    background: var(--panel);
    padding: 14px; border-radius: 14px;
    border: 1px solid #223a66;
    box-shadow: 0 6px 24px #00000033;
  }
  .input-row input {
    flex: 1; background: #0b1222;
    border: 1px solid #243a63; border-radius: 9px;
    padding: 12px 16px; color: var(--text);
    font-size: 14px; outline: none;
    font-family: Consolas, monospace;
  }
  .input-row input:focus { border-color: var(--accent); }
  .btn {
    background: linear-gradient(135deg, var(--accent), #0070ff);
    border: none; color: #fff; font-size: 14px; font-weight: 600;
    padding: 0 26px; border-radius: 9px; cursor: pointer;
    transition: transform .1s, box-shadow .2s;
  }
  .btn:hover { transform: translateY(-1px); box-shadow: 0 6px 20px #00c8ff55; }
  .btn:disabled { opacity: .5; cursor: not-allowed; transform: none; }
  .conns {
    display: flex; align-items: center; gap: 8px;
    background: #0b1222; border: 1px solid #243a63;
    padding: 0 12px; border-radius: 9px; color: var(--dim); font-size: 13px;
  }
  .conns select {
    background: transparent; border: none; color: var(--text);
    font-size: 13px; outline: none; padding: 4px 0;
  }
  .section-title {
    margin: 26px 4px 12px; color: var(--dim);
    font-size: 13px; letter-spacing: 1px; text-transform: uppercase;
  }
  .job-card {
    background: var(--card);
    border: 1px solid #223a66; border-radius: 14px;
    padding: 14px 18px; margin-bottom: 12px;
    transition: border-color .2s;
  }
  .job-card:hover { border-color: #2f4a85; }
  .job-head { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
  .job-name { font-size: 14px; font-weight: 600; flex: 1; word-break: break-all; }
  .job-status { font-size: 12px; padding: 3px 10px; border-radius: 12px; }
  .st-running { background: #0d2f45; color: var(--accent); }
  .st-done    { background: #0d2f22; color: var(--accent2); }
  .st-failed  { background: #3a1420; color: var(--danger); }
  .st-queued  { background: #2a2410; color: var(--warn); }
  .bar { height: 8px; background: #0b1222; border-radius: 5px; overflow: hidden; }
  .bar-fill {
    height: 100%; width: 0%;
    background: linear-gradient(90deg, #0070ff, var(--accent));
    border-radius: 5px; transition: width .4s;
  }
  .job-meta {
    display: flex; justify-content: space-between;
    margin-top: 8px; color: var(--dim); font-size: 12px;
  }
  .job-meta b { color: var(--text); }
  .empty {
    text-align: center; color: var(--dim);
    padding: 60px 0; font-size: 14px;
  }
  .empty .big { font-size: 40px; display: block; margin-bottom: 10px; }
  .toast {
    position: fixed; bottom: 24px; left: 50%; transform: translateX(-50%);
    background: #1b2747; color: var(--text);
    padding: 12px 24px; border-radius: 10px;
    border: 1px solid #2f4a85; display: none;
    box-shadow: 0 8px 30px #00000055;
    font-size: 13px;
  }
</style>
</head>
<body>
<div class="header">
  <div class="logo">⚡</div>
  <div>
    <div class="title">Play<span>DL</span> 下载器</div>
    <div class="ver">v0.1.0 · Multi-Source Download Accelerator</div>
  </div>
  <div class="engine" id="engineStatus">● 引擎检测中...</div>
</div>

<div class="main">
  <div class="input-row">
    <input id="urlInput" type="text" placeholder="粘贴下载链接，支持 http:// https:// ..."
           autofocus spellcheck="false">
    <div class="conns">
      连接
      <select id="conns">
        <option value="">自动</option>
        <option value="4">4</option>
        <option value="8" selected>8</option>
        <option value="16">16</option>
      </select>
    </div>
    <button class="btn" id="dlBtn">⬇ 下载</button>
  </div>

  <div class="section-title">下载队列</div>
  <div id="jobList">
    <div class="empty"><span class="big">⚡</span>输入链接，点击下载开始加速</div>
  </div>
</div>

<div class="toast" id="toast"></div>

<script>
const $ = id => document.getElementById(id);
let currentTotal = 0;

async function refresh() {
  try {
    const r = await fetch('/api/jobs');
    const jobs = await r.json();
    render(jobs);
  } catch (e) {}
}

function render(jobs) {
  const list = $('jobList');
  if (!jobs.length) {
    list.innerHTML = '<div class="empty"><span class="big">⚡</span>输入链接，点击下载开始加速</div>';
    return;
  }
  list.innerHTML = jobs.map(j => {
    const pct = j.progress.toFixed(1);
    const speed = fmtSpeed(j.speed);
    const done = fmtSize(j.done_bytes);
    const total = j.total_bytes > 0 ? fmtSize(j.total_bytes) : '--';
    const cls = 'st-' + j.status;
    return `<div class="job-card">
      <div class="job-head">
        <span class="job-name">📄 ${esc(j.file)}</span>
        <span class="job-status ${cls}">${statusText(j.status)}</span>
      </div>
      <div class="bar"><div class="bar-fill" style="width:${Math.min(pct,100)}%"></div></div>
      <div class="job-meta">
        <span><b>${pct}%</b> · ${done} / ${total}</span>
        <span>速度 <b>${speed}</b></span>
      </div>
    </div>`;
  }).join('');
}

function statusText(s) {
  return {queued:'排队', running:'下载中', done:'完成', failed:'失败'}[s] || s;
}
function esc(s) { return s.replace(/[<>&"]/g, c => ({'<':'&lt;','>':'&gt;','&':'&amp;','"':'&quot;'}[c])); }
function fmtSize(b) {
  if (!b) return '0 B';
  const u = ['B','KB','MB','GB','TB']; let i = 0, v = b;
  while (v >= 1024 && i < u.length-1) { v /= 1024; i++; }
  return v.toFixed(i ? 1 : 0) + ' ' + u[i];
}
function fmtSpeed(bps) {
  if (!bps) return '--';
  return fmtSize(bps) + '/s';
}
function toast(msg) {
  const t = $('toast');
  t.textContent = msg; t.style.display = 'block';
  setTimeout(() => t.style.display = 'none', 2600);
}

$('dlBtn').addEventListener('click', async () => {
  const url = $('urlInput').value.trim();
  if (!url) { toast('请输入下载链接'); return; }
  const conns = $('conns').value;
  const btn = $('dlBtn');
  btn.disabled = true; btn.textContent = '⏳ 添加中...';
  try {
    const r = await fetch('/api/add', {
      method: 'POST',
      headers: {'Content-Type': 'application/json'},
      body: JSON.stringify({ url, conns })
    });
    const d = await r.json();
    if (d.ok) { $('urlInput').value = ''; toast('已加入下载队列 ⚡'); }
    else toast(d.error || '添加失败');
  } catch (e) { toast('网络错误'); }
  btn.disabled = false; btn.textContent = '⬇ 下载';
});

$('urlInput').addEventListener('keydown', e => { if (e.key === 'Enter') $('dlBtn').click(); });

(async () => {
  try {
    const r = await fetch('/api/engine');
    const d = await r.json();
    $('engineStatus').textContent = d.ok ? '● 引擎就绪' : '● 引擎未找到';
    $('engineStatus').style.color = d.ok ? 'var(--accent2)' : 'var(--danger)';
  } catch (e) {}
  refresh();
  setInterval(refresh, 800);
})();
</script>
</body>
</html>"#;

// ---------------- HTTP handlers ----------------

fn handle_request(
    mut req: tiny_http::Request,
    state: &Shared,
    playdl: Option<std::path::PathBuf>,
) {
    let url = req.url().to_string();
    let method = req.method().as_str().to_string();

    match (method.as_str(), url.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            let resp = tiny_http::Response::from_string(INDEX_HTML)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..]).unwrap());
            let _ = req.respond(resp);
        }
        ("GET", "/api/engine") => {
            let ok = playdl.is_some();
            let body = serde_json::json!({ "ok": ok }).to_string();
            let resp = tiny_http::Response::from_string(body)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
            let _ = req.respond(resp);
        }
        ("GET", "/api/jobs") => {
            let s = state.lock().unwrap();
            let jobs: Vec<serde_json::Value> = s
                .jobs
                .iter()
                .map(|j| {
                    serde_json::json!({
                        "id": j.id,
                        "url": j.url,
                        "file": j.file,
                        "status": j.status,
                        "progress": j.progress,
                        "speed": j.speed,
                        "done_bytes": j.done_bytes,
                        "total_bytes": j.total_bytes,
                        "error": j.error,
                    })
                })
                .collect();
            drop(s);
            let body = serde_json::to_string(&jobs).unwrap_or_else(|_| "[]".into());
            let resp = tiny_http::Response::from_string(body)
                .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
            let _ = req.respond(resp);
        }
        ("POST", "/api/add") => {
            let mut body = String::new();
            let _ = req.as_reader().read_to_string(&mut body);
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            let url = parsed["url"].as_str().unwrap_or("").to_string();
            let conns = parsed["conns"].as_str().unwrap_or("");

            let resp = if url.is_empty() {
                tiny_http::Response::from_string(serde_json::json!({"ok": false, "error": "empty url"}).to_string())
            } else {
                let file = sanitize_filename(&url);
                let mut s = state.lock().unwrap();
                s.next_id += 1;
                let id = s.next_id;
                let job = Job::new(id, url.clone(), file.clone());
                s.jobs.push(job);
                drop(s);

                // spawn playdl child
                if let Some(exe) = &playdl {
                    let mut cmd = Command::new(exe);
                    cmd.arg("--json");
                    if !conns.is_empty() {
                        cmd.arg("-x").arg(&conns);
                    }
                    cmd.arg("-o").arg(&file).arg(&url);
                    cmd.stdout(Stdio::null()).stderr(Stdio::null()).stdin(Stdio::null());
                    match cmd.spawn() {
                        Ok(child) => {
                            let mut s = state.lock().unwrap();
                            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                                j.status = "running".into();
                                j.child = Some(child);
                                j.started_at = std::time::Instant::now();
                            }
                            drop(s);
                            tiny_http::Response::from_string(serde_json::json!({"ok": true, "id": id}).to_string())
                        }
                        Err(e) => {
                            let mut s = state.lock().unwrap();
                            if let Some(j) = s.jobs.iter_mut().find(|j| j.id == id) {
                                j.status = "failed".into();
                                j.error = format!("spawn error: {e}");
                            }
                            drop(s);
                            tiny_http::Response::from_string(serde_json::json!({"ok": false, "error": format!("{e}")}).to_string())
                        }
                    }
                } else {
                    tiny_http::Response::from_string(serde_json::json!({"ok": false, "error": "playdl engine not found"}).to_string())
                }
            };
            let resp = resp.with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
            let _ = req.respond(resp);
        }
        _ => {
            let resp = tiny_http::Response::from_string("not found").with_status_code(404);
            let _ = req.respond(resp);
        }
    }
}

// ---------------- main ----------------

fn main() {
    let playdl = find_playdl();
    let state: Shared = Arc::new(Mutex::new(State::default()));

    // bind local server on an ephemeral port
    let server = match tiny_http::Server::http("127.0.0.1:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("playdl-gui: cannot start local server: {e}");
            return;
        }
    };
    let port = server.server_addr().to_ip().map(|a| a.port()).unwrap_or(0);

    // open the default browser
    let ui_url = format!("http://127.0.0.1:{port}/");
    let _ = webbrowser::open(&ui_url);

    println!("PlayDL GUI running at {ui_url}");
    println!("engine: {:?}", playdl);

    // background pump thread
    let pump_state = state.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(400));
        pump(&pump_state);
    });

    // serve until the process is killed
    for req in server.incoming_requests() {
        let st = state.clone();
        let pd = playdl.clone();
        std::thread::spawn(move || handle_request(req, &st, pd));
    }
}