// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: GPL-3.0-or-later

//! `playdl wxchannel` — download WeChat Channels (视频号) media.
//!
//! Scope: the **download side only**. 视频号 media links are short-lived,
//! signed URLs of the form
//!
//! ```text
//! https://finder.video.qq.com/251/20302/stodownload?encfilekey=…&token=…&sign=…&svrnonce=…
//! ```
//!
//! which the page obtains from an API call inside a logged-in session. Nothing
//! here tries to reproduce that part: it is the vendor's own private protocol,
//! and reverse-engineering it — or reproducing the head-of-file masking the
//! player applies — is deliberately out of scope. What this command does is
//! take links or manifests the user already has, fetch them with the full
//! multi-connection PlayDL engine, and give clear, honest diagnostics when a
//! link has expired or a payload is not a plain media container.
//!
//! Three input shapes are accepted, all resolved by [`load_items`]:
//!
//! * one direct `finder.video.qq.com` URL,
//! * a JSON array (or one object per line) with `video_url` / `title` /
//!   `author` / … fields, the shape the common capture tools export,
//! * a plain text file with one URL per line.

use std::path::{Path, PathBuf};

/// One entry to fetch.
///
/// The names follow what the usual 视频号 capture tools write out, so their
/// exports load without hand-editing.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub author: String,
    pub url: String,
    pub cover: Option<String>,
    pub size: Option<u64>,
    pub duration_s: Option<u64>,
    pub resolution: Option<String>,
}

/// Options for one `wxchannel` invocation.
#[derive(Clone, Debug)]
pub struct WxOpts {
    pub source: String,
    pub dir: Option<PathBuf>,
    pub conns: usize,
    pub by_author: bool,
    pub cover: bool,
    pub referer: bool,
    pub json: bool,
    pub list_only: bool,
    /// Filename template, e.g. `{author}/{title}_{res}`. Empty means the
    /// built-in `作者 - 标题` naming.
    pub template: Option<String>,
    /// How many entries to fetch at once. 1 keeps the serial behaviour.
    pub jobs: usize,
    /// Extra attempts per entry after a failure (connection-level retries are
    /// separate and stay inside the engine).
    pub retries: usize,
    /// Where the download ledger lives. `None` with `no_record` disables it.
    pub record: Option<PathBuf>,
    pub no_record: bool,
    /// Fetch even when the ledger already has the entry.
    pub force: bool,
    /// Write this run's result as JSON (`.json`) or CSV (`.csv`).
    pub export: Option<PathBuf>,
    /// Input format: `auto`, `json`, `csv` or `har`.
    pub from: String,
}

/// Result of one attempted download, for the run summary.
#[derive(Clone, Debug)]
struct Row {
    name: String,
    bytes: u64,
    ok: bool,
    masked: bool,
    note: String,
    /// Where it landed, for the ledger and the export.
    file: String,
    url: String,
    /// This entry's own console lines, replayed in manifest order when several
    /// entries were in flight — interleaved progress for three files reads as
    /// noise, and a "failed" line landing before its own "downloading" line is
    /// actively misleading.
    log: Vec<String>,
}

/// One line of the ledger: what was fetched, when, and where it went.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Rec {
    ts: String,
    id: String,
    title: String,
    author: String,
    url: String,
    size: Option<u64>,
    duration_s: Option<u64>,
    resolution: Option<String>,
    file: String,
    ok: bool,
    protected: bool,
}

pub async fn run(opts: WxOpts) -> i32 {
    let items = match load_items(&opts.source, &opts).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!("wxchannel: {e}");
            return 1;
        }
    };
    if items.is_empty() {
        eprintln!("wxchannel: 清单里没有可用的条目");
        return 1;
    }

    if opts.list_only {
        print_list(&items, &opts);
        return 0;
    }

    if let Some(dir) = &opts.dir {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("wxchannel: 无法创建输出目录 {}: {e}", dir.display());
            return 1;
        }
    }

    let ledger = if opts.no_record {
        None
    } else {
        Some(opts.record.clone().unwrap_or_else(|| default_ledger(&opts)))
    };
    let done: std::collections::HashSet<String> = match (&ledger, opts.force) {
        (Some(p), false) => load_records(p).into_iter().filter(|r| r.ok).map(|r| r.url).collect(),
        _ => std::collections::HashSet::new(),
    };

    // Which entries still need fetching, in manifest order.
    let todo: Vec<(usize, &Item)> = items
        .iter()
        .enumerate()
        .filter(|(_, it)| {
            let key = record_key(it);
            if key.is_empty() || !done.contains(&key) {
                return true;
            }
            if !opts.json {
                println!("跳过  {}  下载记录里已经有了", stem_for(it));
            }
            false
        })
        .collect();

    let jobs = opts.jobs.max(1);
    let skipped = items.len() - todo.len();
    let rows = run_queue(&todo, &opts, jobs).await;

    if let Some(p) = &ledger {
        let recs: Vec<Rec> = rows
            .iter()
            .zip(todo.iter())
            .filter(|(r, _)| r.ok)
            .map(|(r, (_, it))| Rec {
                ts: now_iso(),
                id: it.id.clone(),
                title: it.title.clone(),
                author: it.author.clone(),
                url: it.url.clone(),
                size: Some(r.bytes),
                duration_s: it.duration_s,
                resolution: it.resolution.clone(),
                file: r.file.clone(),
                ok: r.ok,
                protected: r.masked,
            })
            .collect();
        if !recs.is_empty() {
            if let Err(e) = append_records(p, &recs) {
                eprintln!("wxchannel: 写入下载记录失败（不影响已下载的文件）：{e}");
            }
        }
    }

    if let Some(p) = &opts.export {
        if let Err(e) = export_rows(p, &rows) {
            eprintln!("wxchannel: 导出失败：{e}");
        } else if !opts.json {
            println!("已导出台账 {}", p.display());
        }
    }

    let ok_count = rows.iter().filter(|r| r.ok).count();
    let masked = rows.iter().filter(|r| r.masked).count();
    if opts.json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name, "bytes": r.bytes, "ok": r.ok,
                    "protected": r.masked, "note": r.note, "file": r.file,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
    } else {
        let total = items.len();
        let good = ok_count + skipped;
        println!("\n—— 汇总 ——  成功 {good}/{total}  受限 {masked}  跳过 {skipped}");
        for r in rows.iter().filter(|r| !r.ok || r.masked) {
            let tag = if r.ok { "受限" } else { "失败" };
            println!("  [{tag}] {}  {}", r.name, r.note);
        }
    }

    if ok_count == rows.len() {
        0
    } else {
        1
    }
}

/// Run `todo` with at most `jobs` entries in flight, retrying failures.
///
/// Results come back in manifest order regardless of completion order: the
/// summary and the ledger read the same way every time.
async fn run_queue(todo: &[(usize, &Item)], opts: &WxOpts, jobs: usize) -> Vec<Row> {
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(jobs));
    let mut set = tokio::task::JoinSet::new();
    for (slot, (idx, item)) in todo.iter().enumerate() {
        let permit = sem.clone().acquire_owned().await.expect("semaphore closed");
        let item = (*item).clone();
        let o = opts.clone();
        let idx = *idx;
        let slot = slot;
        set.spawn(async move {
            let r = fetch_with_retries(&item, &o, slot).await;
            drop(permit);
            (idx, r)
        });
    }
    let mut out: Vec<(usize, Row)> = Vec::with_capacity(todo.len());
    while let Some(res) = set.join_next().await {
        if let Ok((idx, r)) = res {
            out.push((idx, r));
        }
    }
    out.sort_by_key(|(i, _)| *i);
    if jobs > 1 && !opts.json {
        // Replay in order, so entry 3's story is not split by entry 1's.
        for (_, r) in &out {
            for line in &r.log {
                println!("{line}");
            }
        }
    }
    out.into_iter().map(|(_, r)| r).collect()
}

/// One entry, retried `opts.retries` extra times before it is given up on.
async fn fetch_with_retries(item: &Item, opts: &WxOpts, slot: usize) -> Row {
    let mut last = fetch_one(item, opts, slot).await;
    // Each attempt builds its own Row, so the earlier attempts' lines have to
    // be carried forward or the retry history disappears from the replay.
    let mut acc: Vec<String> = std::mem::take(&mut last.log);
    for attempt in 1..=opts.retries {
        if last.ok {
            break;
        }
        let line = format!(
            "重试  {}  （第 {attempt} 次，共 {} 次）  {}",
            last.name, opts.retries, last.note
        );
        if !opts.json && opts.jobs > 1 {
            acc.push(line);
        } else if !opts.json {
            println!("{line}");
        }
        let mut next = fetch_one(item, opts, slot).await;
        acc.extend(std::mem::take(&mut next.log));
        last = next;
    }
    last.log = acc;
    last
}

/// Read items from a URL, a JSON/JSONL manifest, a CSV table, a HAR capture,
/// or a URL-per-line file.
///
/// `--from` forces the reading; `auto` sniffs. The sniffing order matters: a
/// HAR is valid JSON, so it is recognised by its shape before the generic JSON
/// path gets a chance to turn it into one useless item.
pub async fn load_items(source: &str, opts: &WxOpts) -> Result<Vec<Item>, String> {
    let t = source.trim();
    if looks_like_url(t) {
        // A bare link has no title, so it needs *some* discriminator or every
        // video pasted this way lands on the same filename.
        return Ok(vec![Item {
            url: t.to_string(),
            id: short_token(t),
            ..Item::default()
        }]);
    }
    let path = PathBuf::from(t);
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取清单 {}: {e}", path.display()))?;

    let lower = opts.from.trim().to_ascii_lowercase();
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let kind = if !lower.is_empty() && lower != "auto" {
        lower.clone()
    } else if ext == "har" || is_har(&text) {
        "har".to_string()
    } else if ext == "csv" || ext == "tsv" {
        "csv".to_string()
    } else {
        "json".to_string()
    };

    match kind.as_str() {
        "har" => parse_har(&text).map_err(|e| format!("解析 HAR 失败：{e}")),
        "csv" | "tsv" => Ok(csv_to_items(&text, if ext == "tsv" { '\t' } else { ',' })),
        "json" | "jsonl" => parse_manifest(&text),
        other => Err(format!("不支持的清单格式 {other}（可用 auto/json/csv/har）")),
    }
}

/// True when `text` is an HTTP Archive document: its `log.entries` array is the
/// giveaway, and no manifest shape has it.
fn is_har(text: &str) -> bool {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return false;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
        return false;
    };
    v.get("log")
        .and_then(|l| l.get("entries"))
        .and_then(|e| e.as_array())
        .is_some()
}

/// Pull media requests out of a HAR capture.
///
/// The point of supporting this: getting the link is the one step this command
/// cannot do for you, and the honest way to do it is to watch YOUR OWN traffic
/// with a proxy you control (mitmproxy, Fiddler, Charles) and export it. No
/// injection into anybody's client, no certificate PlayDL has to install.
fn parse_har(text: &str) -> Result<Vec<Item>, String> {
    let v: serde_json::Value =
        serde_json::from_str(text.trim_start()).map_err(|e| e.to_string())?;
    let entries = v
        .get("log")
        .and_then(|l| l.get("entries"))
        .and_then(|e| e.as_array())
        .ok_or("HAR 里没有 log.entries")?;

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (n, e) in entries.iter().enumerate() {
        let url = e
            .get("request")
            .and_then(|r| r.get("url"))
            .and_then(|u| u.as_str())
            .unwrap_or_default()
            .to_string();
        if url.is_empty() {
            continue;
        }
        let ctype = e
            .get("response")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get("mimeType"))
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string();
        if !har_entry_is_media(&url, &ctype) {
            continue;
        }
        if !seen.insert(url.clone()) {
            continue; // the same object is often requested twice
        }
        // Metadata hides in the API replies that share the capture: look for
        // the fields we know wherever they sit in the response body.
        let body = e
            .get("response")
            .and_then(|r| r.get("content"))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str());
        let meta = body.and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok());

        let title = meta
            .as_ref()
            .and_then(|m| deep_find(m, &["title", "desc", "description", "object_desc"]))
            .unwrap_or_default();
        let author = meta
            .as_ref()
            .and_then(|m| deep_find(m, &["nickname", "author", "author_name", "creator"]))
            .unwrap_or_default();
        let size = e
            .get("response")
            .and_then(|r| r.get("bodySize"))
            .and_then(|b| b.as_u64())
            .filter(|n| *n > 0);
        out.push(Item {
            id: format!("har{n:04}"),
            title,
            author,
            url,
            cover: None,
            size,
            duration_s: meta
                .as_ref()
                .and_then(|m| deep_find_u64(m, &["duration", "duration_ms"])),
            resolution: None,
        });
    }
    if out.is_empty() {
        return Err("这份 HAR 里没有找到媒体请求".to_string());
    }
    Ok(out)
}

/// Breadth-first hunt for the first string under any of `keys`.
///
/// Capture exports bury the interesting fields at unpredictable depths
/// (`data.object.desc`, `object_list[0].nickname`, …), so a fixed path would
/// find nothing.
fn deep_find(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let mut queue: Vec<&serde_json::Value> = vec![v];
    let mut guard = 0;
    while let Some(cur) = queue.pop() {
        guard += 1;
        if guard > 20_000 {
            break;
        }
        match cur {
            serde_json::Value::Object(m) => {
                for k in keys {
                    if let Some(s) = m.get(*k).and_then(|x| x.as_str()) {
                        let s = s.trim();
                        if !s.is_empty() {
                            return Some(s.to_string());
                        }
                    }
                }
                for x in m.values() {
                    queue.push(x);
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    queue.push(x);
                }
            }
            _ => {}
        }
    }
    None
}

fn deep_find_u64(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    let mut queue: Vec<&serde_json::Value> = vec![v];
    let mut guard = 0;
    while let Some(cur) = queue.pop() {
        guard += 1;
        if guard > 20_000 {
            break;
        }
        match cur {
            serde_json::Value::Object(m) => {
                for k in keys {
                    if let Some(n) = m.get(*k).and_then(|x| x.as_u64()) {
                        if n > 0 {
                            return Some(if n > 86_400 { n / 1000 } else { n });
                        }
                    }
                }
                for x in m.values() {
                    queue.push(x);
                }
            }
            serde_json::Value::Array(a) => {
                for x in a {
                    queue.push(x);
                }
            }
            _ => {}
        }
    }
    None
}

/// Does one HAR entry look like the bytes we want?
fn har_entry_is_media(url: &str, ctype: &str) -> bool {
    let l = url.to_ascii_lowercase();
    if l.contains("stodownload") || l.contains("encfilekey") || l.contains("finder.video.qq.com") {
        return true;
    }
    let ct = ctype.to_ascii_lowercase();
    if ct.starts_with("video/") || ct.starts_with("audio/") {
        return true;
    }
    let path = l.split('?').next().unwrap_or(&l);
    matches!(
        path.rsplit('.').next().unwrap_or(""),
        "mp4" | "m4a" | "mov" | "webm" | "mkv" | "flv" | "ts" | "m4s" | "mp3" | "aac"
    )
}

/// Minimal RFC 4180 reader: quotes, escaped `""`, CRLF and a custom separator.
fn csv_rows(text: &str, sep: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut it = text.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '"' if in_q => {
                if it.peek() == Some(&'"') {
                    it.next();
                    cur.push('"');
                } else {
                    in_q = false;
                }
            }
            '"' => in_q = true,
            '\r' => {}
            '\n' if !in_q => {
                row.push(std::mem::take(&mut cur));
                rows.push(std::mem::take(&mut row));
            }
            c if c == sep && !in_q => {
                row.push(std::mem::take(&mut cur));
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() || !row.is_empty() {
        row.push(cur);
        rows.push(row);
    }
    rows
}

/// Turn an exported table (from a capture tool or a spreadsheet) into items.
///
/// Headers are matched by alias in several languages, and a headerless file
/// that is just a column of links still works.
fn csv_to_items(text: &str, sep: char) -> Vec<Item> {
    let rows = csv_rows(text, sep);
    let mut iter = rows.iter();
    let Some(header) = iter.find(|r| r.iter().any(|c| !c.trim().is_empty())) else {
        return Vec::new();
    };
    let head: Vec<String> = header.iter().map(|c| c.trim().to_ascii_lowercase()).collect();
    let has_alias = |keys: &[&str]| head.iter().any(|h| keys.contains(&h.as_str()));
    let col = |keys: &[&str]| -> Option<usize> {
        head.iter().position(|h| keys.contains(&h.as_str()))
    };

    let i_url = col(&["video_url", "url", "link", "media_url", "play_url", "链接", "地址", "下载地址"]);
    let i_title = col(&["title", "desc", "description", "name", "标题", "描述", "视频标题"]);
    let i_author = col(&["author", "nickname", "author_name", "creator", "作者", "昵称"]);
    let i_cover = col(&["cover_url", "cover", "coverurl", "thumb", "封面"]);
    let i_size = col(&["size", "file_size", "filesize", "filesize_bytes", "大小", "字节"]);
    let i_dur = col(&["duration", "duration_ms", "durationms", "时长", "时长(秒)"]);
    let i_res = col(&["resolution", "spec", "分辨率"]);
    let i_id = col(&["id", "video_id", "export_id", "编号"]);

    // No recognisable header: fall back to "any cell that is a link".
    if i_url.is_none() && !has_alias(&["title", "标题"]) {
        return rows
            .iter()
            .flat_map(|r| r.iter())
            .filter(|c| looks_like_url(c.trim()))
            .map(|c| Item {
                url: c.trim().to_string(),
                id: short_token(c.trim()),
                ..Item::default()
            })
            .collect();
    }

    let mut out = Vec::new();
    for r in rows.iter().skip(1) {
        let get = |i: Option<usize>| -> String {
            i.and_then(|k| r.get(k)).map(|s| s.trim().to_string()).unwrap_or_default()
        };
        let url = get(i_url);
        if !looks_like_url(&url) {
            continue;
        }
        let size = i_size.and_then(|k| r.get(k)).and_then(|s| s.trim().parse::<u64>().ok());
        let dur = i_dur.and_then(|k| r.get(k)).and_then(|s| s.trim().parse::<u64>().ok());
        out.push(Item {
            id: get(i_id),
            title: get(i_title),
            author: get(i_author),
            url,
            cover: i_cover.and_then(|k| r.get(k)).map(|s| s.trim().to_string()).filter(|s| looks_like_url(s)),
            size,
            duration_s: dur.map(|n| if n > 86_400 { n / 1000 } else { n }),
            resolution: i_res.and_then(|k| r.get(k)).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        });
    }
    out
}

fn looks_like_url(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// Parse a manifest document. Accepts a JSON array, `{items:[…]}`-style
/// wrappers, JSON Lines, and a bare list of URLs.
fn parse_manifest(text: &str) -> Result<Vec<Item>, String> {
    let trimmed = text.trim_start();
    // Try the whole document first, but a failure here is not fatal: a JSON
    // Lines file also starts with `{` yet is not one JSON value, so fall
    // through to the line-wise reader below.
    let whole: Option<serde_json::Value> = serde_json::from_str(trimmed).ok();
    if let Some(v) = &whole {
        if v.is_array() {
            return value_to_items(v);
        }
        if let Some(arr) = v
            .get("items")
            .or_else(|| v.get("data"))
            .or_else(|| v.get("list"))
        {
            return value_to_items(arr);
        }
        return Ok(vec![object_to_item(v)]);
    }
    // Fall back to line-wise: JSON objects, URLs, or `#`-commented URL lists.
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('{') {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(v) => out.push(object_to_item(&v)),
                Err(_) => return Err(format!("无法解析这一行 JSON: {line}")),
            }
        } else if looks_like_url(line) {
            out.push(Item {
                url: line.to_string(),
                ..Item::default()
            });
        }
    }
    Ok(out)
}

fn value_to_items(v: &serde_json::Value) -> Result<Vec<Item>, String> {
    let Some(arr) = v.as_array() else {
        return Err("清单应为 JSON 数组".into());
    };
    Ok(arr.iter().map(object_to_item).collect())
}

fn object_to_item(v: &serde_json::Value) -> Item {
    Item {
        id: pick_str(v, &["id", "video_id", "export_id"]).unwrap_or_default(),
        title: pick_str(v, &["title", "desc", "description", "name"]).unwrap_or_default(),
        author: pick_str(v, &["author", "nickname", "author_name", "creator"])
            .unwrap_or_default(),
        url: pick_str(v, &["video_url", "url", "link", "media_url", "play_url"])
            .unwrap_or_default(),
        cover: pick_str(v, &["cover_url", "cover", "coverUrl", "thumb"]),
        size: pick_u64(v, &["size", "file_size", "filesize", "fileSize"]),
        duration_s: duration_secs(v),
        resolution: pick_str(v, &["resolution", "spec"]),
    }
}

/// First present string among `keys`, also accepting numbers rendered as
/// strings (`{"duration": "42"}` shows up in some exports).
fn pick_str(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(x) = v.get(*k) {
            let s = match x {
                serde_json::Value::String(s) => s.trim().to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => continue,
            };
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

fn pick_u64(v: &serde_json::Value, keys: &[&str]) -> Option<u64> {
    for k in keys {
        if let Some(x) = v.get(*k) {
            if let Some(n) = x.as_u64() {
                return Some(n);
            }
            if let Some(s) = x.as_str() {
                if let Ok(n) = s.trim().parse::<u64>() {
                    return Some(n);
                }
            }
        }
    }
    None
}

/// Durations arrive in seconds or milliseconds depending on the exporter, and
/// the field is called `duration` either way, so the number alone is ambiguous.
/// Three rules, in order:
///
/// * explicit millisecond keys (`duration_ms`/`durationMs`) are never seconds;
/// * anything past a day's worth of seconds cannot be seconds for a feed video;
/// * otherwise, cross-check against the object's own bitrate. A 3.3 MB file
///   advertised as 16834 seconds would be 1.6 kbit/s, which no encoder emits;
///   read as milliseconds it is 16.8 s at 1.6 Mbit/s, which is ordinary. This
///   what catches short videos, where the pure magnitude rule cannot.
fn duration_secs(v: &serde_json::Value) -> Option<u64> {
    if let Some(n) = pick_u64(v, &["duration_ms", "durationMs"]) {
        return Some(n / 1000);
    }
    let n = pick_u64(v, &["duration"])?;
    Some(resolve_duration(
        n,
        pick_u64(v, &["size", "file_size", "filesize", "fileSize"]),
    ))
}

fn resolve_duration(n: u64, size: Option<u64>) -> u64 {
    if n > 86_400 {
        return n / 1000;
    }
    if let Some(sz) = size {
        if n >= 1000 {
            let bps = sz.saturating_mul(8) / n;
            // Below ~50 kbit/s no real encoder sits; a whole order of magnitude
            // of headroom keeps slideshow-quality clips out of this branch.
            if bps < 50_000 {
                return n / 1000;
            }
        }
    }
    n
}

/// Turn arbitrary (often multi-line, hashtag-laden) titles into one usable
/// filename component. Empty input becomes `untitled` — see [`clean_part`] for
/// the variant that keeps it empty, which templates need.
fn sanitize_name(s: &str, limit: usize) -> String {
    let r = clean_part(s, limit);
    if r.is_empty() {
        "untitled".to_string()
    } else {
        r
    }
}

/// Same cleaning, but empty in means empty out.
fn clean_part(s: &str, limit: usize) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        // Video titles carry newlines and 话题分隔符; fold them to spaces.
        if ch.is_control() {
            out.push(' ');
            continue;
        }
        if matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
            continue;
        }
        out.push(ch);
    }
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim().trim_end_matches('.').trim().to_string();
    if trimmed.is_empty() {
        return String::new();
    }
    // Windows counts bytes for its path budget; cap on characters, which is
    // what the user reads, and leave room for " - " and the suffix.
    let mut kept = String::new();
    for (i, ch) in trimmed.chars().enumerate() {
        if i >= limit {
            break;
        }
        kept.push(ch);
    }
    kept.trim_end().to_string()
}

fn extension_of(url: &str, fallback: &str) -> String {
    let path_part = url.split('?').next().unwrap_or(url);
    if let Some((_, last)) = path_part.rsplit_once('/') {
        if let Some((_, ext)) = last.rsplit_once('.') {
            let ext = ext.to_ascii_lowercase();
            if ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                return ext;
            }
        }
    }
    fallback.trim_start_matches('.').to_string()
}

/// `作者 - 标题` when we know the author, else just the title, else the id.
fn stem_for(item: &Item) -> String {
    let title = if !item.title.trim().is_empty() {
        item.title.clone()
    } else if !item.id.trim().is_empty() {
        item.id.clone()
    } else {
        String::new()
    };
    let title = if title.is_empty() {
        String::new()
    } else {
        sanitize_name(&title, 60)
    };
    let author = if item.author.trim().is_empty() {
        String::new()
    } else {
        sanitize_name(item.author.trim(), 30)
    };
    match (author.is_empty(), title.is_empty()) {
        (true, true) => "wxchannel-video".to_string(),
        (true, false) => title,
        (false, true) => author,
        (false, false) => format!("{author} - {title}"),
    }
}

/// Apply a `--template`, falling back to the built-in naming when it produces
/// nothing usable.
///
/// `/` in a template means a subdirectory, so `{author}/{title}` sorts things
/// without needing `--by-author`.
fn stem_with(item: &Item, tpl: Option<&str>, index: usize) -> String {
    let Some(tpl) = tpl.map(|s| s.trim()).filter(|s| !s.is_empty()) else {
        return stem_for(item);
    };
    let value = |ph: &str| -> String {
        match ph {
            "author" | "作者" => item.author.clone(),
            "title" | "标题" => item.title.clone(),
            "id" => item.id.clone(),
            "res" | "resolution" => item.resolution.clone().unwrap_or_default(),
            "dur" | "duration" => item.duration_s.map(|d| d.to_string()).unwrap_or_default(),
            "date" => today_str(),
            "index" | "n" => (index + 1).to_string(),
            _ => String::new(),
        }
    };
    let chars: Vec<char> = tpl.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '{' {
            if let Some(rel) = chars[i..].iter().position(|c| *c == '}') {
                let ph: String = chars[i + 1..i + rel].iter().collect();
                out.push_str(&clean_part(&value(ph.trim()), 60));
                i += rel + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    // A placeholder that resolved to nothing leaves a dangling separator:
    // "{author} - {title}" with no author must not start with " - ".
    let parts: Vec<String> = out
        .split('/')
        .map(|p| {
            // Strip what is left of a placeholder that resolved to nothing:
            // "{author} - {title}" with no author must not start with " - ".
            let s = p.trim_matches(|c: char| c == '_' || c == '-' || c.is_whitespace());
            clean_part(s, 60)
        })
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        stem_for(item)
    } else {
        parts.join("/")
    }
}

fn today_str() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn now_iso() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Where to look.
fn default_ledger(opts: &WxOpts) -> PathBuf {
    match &opts.dir {
        Some(d) => d.join(".playdl-wxchannel.jsonl"),
        None => PathBuf::from(".playdl-wxchannel.jsonl"),
    }
}

/// What identifies an entry in the ledger.
///
/// The signed URL is the identity that matters: the same video re-exported
/// carries the same link while it is still valid, and a fresh link means the
/// bytes must be fetched again regardless of any id.
fn record_key(item: &Item) -> String {
    let u = item.url.trim();
    if !u.is_empty() {
        u.to_string()
    } else {
        item.id.trim().to_string()
    }
}

/// The ledger is append-only JSON Lines: a half-written last line is dropped
/// rather than fatal, because a crash mid-write must not lose the whole record.
fn load_records(path: &Path) -> Vec<Rec> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Rec>(l).ok())
        .collect()
}

fn append_records(path: &Path, recs: &[Rec]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    use std::io::Write;
    for r in recs {
        if let Ok(line) = serde_json::to_string(r) {
            writeln!(f, "{line}")?;
        }
    }
    Ok(())
}

fn csv_cell(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Write the run's outcome as a ledger file: JSON by default, CSV when the
/// path ends in `.csv` (with a BOM, or Excel reads the Chinese as mojibake).
fn export_rows(path: &Path, rows: &[Row]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let is_csv = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);
    if is_csv {
        let mut s = String::from("\u{FEFF}");
        s.push_str("状态,文件名,字节,说明\n");
        for r in rows {
            let tag = if !r.ok { "失败" } else if r.masked { "受限" } else { "成功" };
            s.push_str(&format!(
                "{},{},{},{}\n",
                tag,
                csv_cell(&r.name),
                r.bytes,
                csv_cell(&r.note)
            ));
        }
        std::fs::write(path, s)
    } else {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name, "file": r.file, "url": r.url, "bytes": r.bytes,
                    "ok": r.ok, "protected": r.masked, "note": r.note,
                })
            })
            .collect();
        let s = serde_json::to_string_pretty(&arr).unwrap_or_default();
        std::fs::write(path, s + "\n")
    }
}

/// Where the file goes, honouring `--template` when one was given.
fn target_path(item: &Item, opts: &WxOpts, ext: &str, masked: bool, index: usize) -> PathBuf {
    let stem = stem_with(item, opts.template.as_deref(), index);
    let suffix = if masked { ".masked" } else { "" };
    let name = format!("{stem}{suffix}.{ext}");
    match (&opts.dir, opts.by_author) {
        (None, false) => PathBuf::from(name),
        (Some(d), false) => d.join(name),
        (None, true) => {
            let sub = sanitize_name(item.author.trim(), 30);
            let sub = if sub.is_empty() { "未署名".to_string() } else { sub };
            PathBuf::from(sub).join(name)
        }
        (Some(d), true) => {
            let sub = sanitize_name(item.author.trim(), 30);
            let sub = if sub.is_empty() { "未署名".to_string() } else { sub };
            d.join(sub).join(name)
        }
    }
}

/// `svrnonce` in these URLs is the sign time in epoch seconds; how old the
/// link already is, for a warning that saves a doomed fetch.
fn nonce_age_secs(url: &str) -> Option<i64> {
    let v = query_param(url, "svrnonce")?;
    let secs: i64 = v.parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    Some(now - secs)
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let q = url.split('?').nth(1)?;
    q.split('#').next()?
        .split('&')
        .filter_map(|p| {
            let mut it = p.splitn(2, '=');
            Some((it.next()?, it.next().unwrap_or("")))
        })
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

/// A stable, filesystem-safe discriminator for a URL that carries no title.
///
/// WeChat's own `encfilekey` is per-object and appears in every one of these
/// links, so it both identifies the video and keeps two pastes from colliding.
fn short_token(url: &str) -> String {
    let raw = query_param(url, "encfilekey")
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            url.split('?')
                .next()
                .unwrap_or(url)
                .rsplit('/')
                .find(|s| !s.is_empty())
                .unwrap_or("")
                .to_string()
        });
    let keep: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    if keep.is_empty() {
        "wxchannel-video".to_string()
    } else {
        format!("wxchannel-{keep}")
    }
}

/// True when `head` (the first bytes of the file) is **not** a standard media
/// container — the signature of a payload the player has to unmask itself.
fn looks_protected(head: &[u8]) -> bool {
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return false; // ISO-BMFF: mp4 / mov / m4a
    }
    if head.len() >= 4 && (&head[..4] == b"RIFF" || &head[..4] == b"OggS" || &head[..4] == b"fLaC") {
        return false;
    }
    if head.len() >= 3 && &head[..3] == b"ID3" {
        return false;
    }
    if head.len() >= 4 && head[0] == 0x1A && head[1] == 0x45 && head[2] == 0xDF && head[3] == 0xA3 {
        return false; // Matroska / WebM
    }
    if head.len() >= 2 && head[0] == 0xFF && (head[1] == 0xF1 || head[1] == 0xF9) {
        return false; // AAC ADTS
    }
    // MPEG-TS: 188-byte packets, each starting with the sync byte 0x47.
    if head.len() >= 188 * 2 && head[0] == 0x47 && head[188] == 0x47 {
        return false;
    }
    if head.len() >= 2 && head[0] == 0x47 && (head[1] & 0x1F) == 0x07 {
        return false; // fMP4-adjacent m2ts variants
    }
    true
}

fn head_of(path: &Path, n: usize) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; n];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(buf)
}

/// One entry's own console line: printed now when entries run one at a time,
/// held for an ordered replay when they run together.
fn emit(log: &mut Vec<String>, opts: &WxOpts, line: String) {
    if opts.json {
        return;
    }
    if opts.jobs > 1 {
        log.push(line);
    } else {
        println!("{line}");
    }
}

async fn fetch_one(item: &Item, opts: &WxOpts, index: usize) -> Row {
    let display = stem_with(item, opts.template.as_deref(), index);
    // Several entries in flight: hold our own lines and let run_queue replay
    // them in manifest order.
    let mut log: Vec<String> = Vec::new();
    if item.url.trim().is_empty() {
        let note = "条目没有下载地址".to_string();
        emit(&mut log, opts, format!("跳过  {display}  {note}"));
        return Row {
            name: display.clone(),
            bytes: 0,
            ok: false,
            masked: false,
            note,
            file: String::new(),
            url: item.url.clone(),
            log: std::mem::take(&mut log),
        };
    }

    if let Some(age) = nonce_age_secs(&item.url) {
        if age > 86_400 {
            let days = age / 86_400;
            emit(&mut log, opts, format!("提示  {display}  这份链接签发于 {days} 天前，很可能已过期"));
        }
    }

    let ext = extension_of(&item.url, "mp4");
    // A previous run may already have stored this entry under its masked name.
    let plain = target_path(item, opts, &ext, false, index);
    let masked_path = target_path(item, opts, &ext, true, index);
    if let Some(size) = item.size {
        for p in [&plain, &masked_path] {
            if let Ok(md) = std::fs::metadata(p) {
                if md.len() == size {
                    emit(&mut log, opts, format!(
                        "跳过  {display}  已存在且大小一致（{} 字节）",
                        md.len()
                    ));
                    return Row {
                        name: display.clone(),
                        bytes: md.len(),
                        ok: true,
                        masked: p == &masked_path,
                        note: "已存在且大小一致，跳过".to_string(),
                        file: p.display().to_string(),
                        url: item.url.clone(),
                        log,
                    };
                }
            }
        }
    }

    let mut job = crate::download::default_job();
    job.urls = vec![item.url.clone()];
    job.output = Some(plain.clone());
    job.conns = Some(opts.conns.max(1));
    job.resume = true;
    job.create_dirs = true;
    job.tries = 3;
    // In parallel mode the engine's own `hydra: …` diagnostics would interleave
    // with the ordered replay `run_queue` prints afterwards, so they are silenced
    // here; the failure reason is reported by our own 失败/受限 lines instead.
    let parallel = opts.jobs > 1 && !opts.json;
    job.show_error = !parallel;
    if opts.referer {
        job.headers
            .push("Referer: https://channels.weixin.qq.com/".to_string());
    }
    // Several entries in flight would interleave progress bars and the engine's
    // own diagnostics into unreadable noise, so past one job the engine goes
    // quiet and each entry's story is replayed once, in order, by `run_queue`.
    job.quiet = opts.json || parallel;
    job.no_progress = opts.json || parallel;

    // `display` already carries the author as `作者 - 标题`, so it is not
    // repeated here.
    emit(&mut log, opts, format!("\n下载  {display}"));

    let out = crate::download::run(job).await;
    let mut row = Row {
        name: display.clone(),
        bytes: out.size,
        ok: out.ok,
        masked: false,
        note: String::new(),
        file: out.output.clone(),
        url: item.url.clone(),
        log: Vec::new(), // filled in on the way out: more lines follow
    };

    if !out.ok {
        row.note = classify_failure(out.note.as_deref());
        emit(&mut log, opts, format!("失败  {display}  {}", row.note));
        row.log = std::mem::take(&mut log);
        return row;
    }
    if opts.jobs > 1 {
        emit(&mut log, opts, format!("完成  {display}  {} 字节", out.size));
    }

    // Payload check: some objects are delivered with the player's own head-of-
    // file masking. We keep the bytes exactly as received — unmasking them is
    // reproducing the vendor's protection bypass, which this project does not
    // do — but we mark the file so it is never mistaken for a playable video.
    let saved = std::path::PathBuf::from(out.output.clone());
    if let Ok(head) = head_of(&saved, 4096) {
        if looks_protected(&head) {
            let _ = std::fs::rename(&saved, &masked_path);
            row.masked = true;
            row.file = masked_path.display().to_string();
            row.note = "内容不是标准容器（可能加了头部保护），已原样保存并标注 .masked".to_string();
            emit(&mut log, opts, format!("受限  {display}  {}", row.note));
        }
    }

    if opts.cover {
        if let Some(cover) = &item.cover {
            if looks_like_url(cover) {
                let mut cj = crate::download::default_job();
                cj.urls = vec![cover.clone()];
                let cext = extension_of(cover, "jpg");
                cj.output = Some(target_path(item, opts, &cext, false, index));
                cj.resume = true;
                cj.create_dirs = true;
                cj.quiet = true;
                cj.no_progress = true;
                let _ = crate::download::run(cj).await;
            }
        }
    }

    row.log = std::mem::take(&mut log);
    row
}

/// Turn engine failure text into something actionable for this job.
fn classify_failure(note: Option<&str>) -> String {
    let s = note.unwrap_or_default().to_ascii_lowercase();
    if s.contains("expire") {
        return "腾讯返回链接已过期（需重新获取）".to_string();
    }
    if s.contains("403") || s.contains("forbidden") {
        return "服务器拒绝（403）：链接多半已失效或缺少会话".to_string();
    }
    if s.contains("400") || s.contains("bad request") {
        return "服务器拒绝（400）：链接无效或已过期".to_string();
    }
    if s.contains("404") || s.contains("not found") {
        return "资源不存在（404）：链接已过期".to_string();
    }
    if s.contains("timed out") || s.contains("timeout") {
        return "连接超时".to_string();
    }
    match note {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => "未知错误".to_string(),
    }
}

fn print_list(items: &[Item], opts: &WxOpts) {
    if opts.json {
        let arr: Vec<serde_json::Value> = items
            .iter()
            .map(|i| {
                serde_json::json!({
                    "id": i.id, "title": i.title, "author": i.author,
                    "duration_s": i.duration_s, "size": i.size,
                    "resolution": i.resolution, "url": i.url,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        return;
    }
    println!("共 {} 条：", items.len());
    for (n, i) in items.iter().enumerate() {
        let dur = i
            .duration_s
            .map(|d| format!("{:02}:{:02}", d / 60, d % 60))
            .unwrap_or_else(|| "--:--".to_string());
        let size = i
            .size
            .map(|s| format!("{:.1} MB", s as f64 / 1_048_576.0))
            .unwrap_or_else(|| "未知大小".to_string());
        let res = pad_to(&i.resolution.clone().unwrap_or_default(), 7);
        let who = format!(
            "{}｜{}",
            one_line(author_or_blank(i)),
            one_line(title_or_blank(i))
        );
        let who = pad_to(&truncate(&who, 26), 26);
        println!("  {:>3}. [{dur}] {res} {who} {size}", n + 1);
    }
}

fn author_or_blank(i: &Item) -> &str {
    if i.author.trim().is_empty() {
        "（未署名）"
    } else {
        i.author.as_str()
    }
}

fn title_or_blank(i: &Item) -> &str {
    if i.title.trim().is_empty() {
        "（无标题）"
    } else {
        i.title.as_str()
    }
}

/// Column layout is measured in **terminal cells**, not characters: CJK glyphs
/// occupy two. Padding by `char` count misaligns every row that mixes Latin and
/// Chinese, which is most rows here.
fn display_width(s: &str) -> usize {
    s.chars().map(|c| if wide_char(c) { 2 } else { 1 }).sum()
}

/// Full-width / wide East-Asian ranges (Unicode `East_Asian_Width` W and F).
fn wide_char(c: char) -> bool {
    matches!(c,
        '\u{1100}'..='\u{115F}'
        | '\u{2E80}'..='\u{303E}'
        | '\u{3041}'..='\u{33FF}'
        | '\u{3400}'..='\u{4DBF}'
        | '\u{4E00}'..='\u{9FFF}'
        | '\u{A000}'..='\u{A4CF}'
        | '\u{AC00}'..='\u{D7A3}'
        | '\u{F900}'..='\u{FAFF}'
        | '\u{FE30}'..='\u{FE4F}'
        | '\u{FF00}'..='\u{FF60}'
        | '\u{FFE0}'..='\u{FFE6}'
        | '\u{20000}'..='\u{3FFFD}')
}

/// Flatten a title for one-line display: these titles carry embedded newlines.
fn one_line(s: &str) -> String {
    let flat: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    flat.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Truncate to `max` cells, adding an ellipsis when anything was dropped.
fn truncate(s: &str, max: usize) -> String {
    if display_width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = if wide_char(c) { 2 } else { 1 };
        if w + cw + 1 > max {
            break; // +1 leaves room for the ellipsis
        }
        out.push(c);
        w += cw;
    }
    format!("{out}…")
}

/// Pad to `width` cells so columns line up regardless of script.
fn pad_to(s: &str, width: usize) -> String {
    let used = display_width(s);
    if used >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - used))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_array_manifest() {
        let doc = r#"[
          {"title":"测试\n#话题#","author":"某作者","video_url":"https://finder.video.qq.com/a.mp4","size":1234},
          {"title":"第二条","author":"乙","video_url":"https://finder.video.qq.com/b.mp4"}
        ]"#;
        let v = parse_manifest(doc).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].title, "测试\n#话题#");
        assert_eq!(v[0].author, "某作者");
        assert_eq!(v[0].size, Some(1234));
        assert_eq!(v[1].size, None);
    }

    #[test]
    fn json_lines_manifest() {
        let doc = concat!(
            "{\"video_url\":\"https://x/a.mp4\",\"title\":\"一\",\"author\":\"甲\"}\n",
            "{\"video_url\":\"https://x/b.mp4\",\"title\":\"二\",\"nickname\":\"乙\"}\n"
        );
        let v = parse_manifest(doc).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].author, "乙");
    }

    #[test]
    fn url_list_manifest() {
        let doc = "# exported\nhttps://finder.video.qq.com/a.mp4\n\nhttps://finder.video.qq.com/b.mp4\n";
        let v = parse_manifest(doc).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].url, "https://finder.video.qq.com/a.mp4");
    }

    #[test]
    fn wrapper_object_manifest() {
        let doc = r#"{"items":[{"url":"https://x/a.mp4","name":"标题"}]}"#;
        let v = parse_manifest(doc).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].title, "标题");
    }

    #[test]
    fn duration_ms_becomes_seconds() {
        let v: serde_json::Value = serde_json::from_str(r#"{"duration":184000}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(184));
        let v: serde_json::Value = serde_json::from_str(r#"{"duration":42}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(42));
        // Short clip: below a day, so only the bitrate cross-check saves it.
        let v: serde_json::Value =
            serde_json::from_str(r#"{"duration":16834,"size":3471161}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(16));
        // Same number with no size to reason about: left alone.
        let v: serde_json::Value = serde_json::from_str(r#"{"duration":16834}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(16834));
        // Explicit millisecond keys are honoured whatever their magnitude.
        let v: serde_json::Value = serde_json::from_str(r#"{"duration_ms":16834}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(16));
        // A genuinely long video stays in seconds ( plausible bitrate ).
        let v: serde_json::Value =
            serde_json::from_str(r#"{"duration":3600,"size":1073741824}"#).unwrap();
        assert_eq!(duration_secs(&v), Some(3600));
    }

    #[test]
    fn sanitize_drops_windows_hostile_characters() {
        let s = sanitize_name("a/b\\c:d*e?f\"g<h>i|j\n k", 200);
        assert!(!s.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']));
        assert!(!s.contains('\n'));
        assert_eq!(sanitize_name("", 10), "untitled");
        assert_eq!(sanitize_name("....", 10), "untitled");
        assert_eq!(sanitize_name("x".repeat(50).as_str(), 8).chars().count(), 8);
    }

    #[test]
    fn stems_use_author_and_plain_titles() {
        let i = Item {
            title: "我要感谢我的娘家人们\n#搞笑#".into(),
            author: "大崔乐呵呵".into(),
            ..Item::default()
        };
        let s = stem_for(&i);
        assert!(s.starts_with("大崔乐呵呵 - 我要感谢我的娘家人们"));
        assert!(!s.contains('\n'));
        let anon = Item { ..Item::default() };
        assert_eq!(stem_for(&anon), "wxchannel-video");
    }

    #[test]
    fn extensions_come_from_the_path() {
        assert_eq!(extension_of("https://a.b/c/d.mp4?x=1&y=2", "mp4"), "mp4");
        assert_eq!(
            extension_of("https://finder.video.qq.com/251/20302/stodownload?encfilekey=x", "mp4"),
            "mp4"
        );
        assert_eq!(extension_of("https://a.b/c/d.m4a", "mp4"), "m4a");
    }

    #[test]
    fn media_containers_are_recognised() {
        let mp4 = {
            let mut v = vec![0u8; 16];
            v[4..8].copy_from_slice(b"ftyp");
            v
        };
        assert!(!looks_protected(&mp4));
        let mut ts = vec![0u8; 400];
        ts[0] = 0x47;
        ts[188] = 0x47;
        assert!(!looks_protected(&ts));
        assert!(!looks_protected(b"ID3\x03\x00\x00\x00"));
        assert!(!looks_protected(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0]));
        assert!(looks_protected(&[0x75, 0xa2, 0xb8, 0x0f, 0x5d, 0xb2, 0x52, 0x8b]));
        assert!(looks_protected(&[]));
    }

    #[test]
    fn bare_links_get_a_unique_stem() {
        let a = "https://finder.video.qq.com/251/20302/stodownload?encfilekey=AbC123XyZ%2FqQ&token=t&svrnonce=1";
        let b = "https://finder.video.qq.com/251/20302/stodownload?encfilekey=ZzZ999&token=t&svrnonce=1";
        let sa = short_token(a);
        let sb = short_token(b);
        assert!(sa.starts_with("wxchannel-") && sb.starts_with("wxchannel-"));
        assert_ne!(sa, sb, "两个不同的视频不能落到同一个文件名上");
        assert!(!sa.contains(['/', '\\', '%']));
        // With nothing better available, fall back to the host: never empty,
        // never an illegal filename.
        let host_only = short_token("https://example.com/?v=1");
        assert!(host_only.starts_with("wxchannel-"));
        assert!(!host_only.contains(['/', '\\', ':', '?']));
    }

    fn har(entries: &[serde_json::Value]) -> String {
        serde_json::json!({ "log": { "version": "1.2", "entries": entries } }).to_string()
    }

    fn har_entry(url: &str, mime: &str, body: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "request": { "url": url },
            "response": { "bodySize": 207588679, "content": {
                "mimeType": mime, "text": body.unwrap_or("") } }
        })
    }

    #[test]
    fn har_capture_yields_only_media_entries() {
        // The API reply carries the metadata, but nested where a fixed path
        // would never look.
        let api = r#"{"data":{"object":{"desc":"测试标题","nickname":"甲作者","duration":184000}}}"#;
        let doc = har(&[
            har_entry("https://finder.video.qq.com/251/20302/stodownload?encfilekey=AAA", "", None),
            har_entry("https://cdn.example.com/clip.mp4", "video/mp4", Some(api)),
            har_entry("https://cdn.example.com/app.js", "application/javascript", None),
            har_entry("https://cdn.example.com/clip.mp4", "video/mp4", None), // duplicate
        ]);
        let v = parse_har(&doc).unwrap();
        assert_eq!(v.len(), 2, "脚本要排除，重复链接要去重");
        assert_eq!(v[0].url.contains("stodownload"), true);
        assert_eq!(v[1].title, "测试标题");
        assert_eq!(v[1].author, "甲作者");
        assert_eq!(v[1].duration_s, Some(184));
        assert!(is_har(&doc));
        assert!(!is_har("[{\"url\":\"https://a/b.mp4\"}]"));
    }

    #[test]
    fn csv_table_reads_headers_and_quotes() {
        let doc = concat!(
            "标题,作者,链接,时长,大小\n",
            "\"带,逗号的标题\",甲作者,https://x/a.mp4,184,207588679\n",
            "第二个,乙作者,https://x/b.mp4,,\n",
            "没有链接的一行,丙作者,,,\n"
        );
        let v = csv_to_items(doc, ',');
        assert_eq!(v.len(), 2, "没有链接的行要丢掉");
        assert_eq!(v[0].title, "带,逗号的标题", "引号里的逗号不能被切开");
        assert_eq!(v[0].author, "甲作者");
        assert_eq!(v[0].size, Some(207588679));
        assert_eq!(v[0].duration_s, Some(184));
        assert_eq!(v[1].size, None);
    }

    #[test]
    fn headerless_csv_is_a_url_list() {
        let doc = "https://a/one.mp4\nhttps://b/two.mp4\n";
        let v = csv_to_items(doc, ',');
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].url, "https://a/one.mp4");
    }

    #[test]
    fn templates_expand_and_drop_dangling_separators() {
        let i = Item {
            title: "标题".into(),
            author: "甲".into(),
            resolution: Some("1080x1920".into()),
            ..Item::default()
        };
        assert_eq!(stem_with(&i, Some("{author}/{title}_{res}"), 0), "甲/标题_1080x1920");
        assert_eq!(stem_with(&i, Some("{date}_{index}"), 4), format!("{}_5", today_str()));
        // No author: the separator must not survive as a dangling " - ".
        let anon = Item { title: "标题".into(), ..Item::default() };
        assert_eq!(stem_with(&anon, Some("{author} - {title}"), 0), "标题");
        // A template of nothing but placeholders that resolve to nothing still
        // has to produce a usable name.
        assert_eq!(stem_with(&Item::default(), Some("{author}/{title}"), 0), "wxchannel-video");
        // Windows-hostile characters do not come back through the template.
        let nasty = Item { title: "a/b:c".into(), ..Item::default() };
        assert_eq!(stem_with(&nasty, Some("{title}"), 0), "a_b_c");
    }

    #[test]
    fn ledger_survives_a_round_trip() {
        let p = std::env::temp_dir().join(format!("playdl-wx-test-{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        let recs = vec![Rec {
            ts: "2026-10-08 10:00:00".into(),
            id: "x1".into(),
            title: "标题".into(),
            author: "甲".into(),
            url: "https://x/a.mp4".into(),
            size: Some(10),
            duration_s: Some(3),
            resolution: None,
            file: "a.mp4".into(),
            ok: true,
            protected: false,
        }];
        append_records(&p, &recs).unwrap();
        let back = load_records(&p);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].url, "https://x/a.mp4");
        assert_eq!(record_key(&Item { url: back[0].url.clone(), ..Item::default() }), "https://x/a.mp4");
        // A truncated last line must not take the rest of the ledger with it.
        std::fs::write(&p, format!("{}\n{{\"broken", serde_json::to_string(&recs[0]).unwrap())).unwrap();
        assert_eq!(load_records(&p).len(), 1);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn csv_cells_are_quoted() {
        assert_eq!(csv_cell("plain"), "plain");
        assert_eq!(csv_cell("a,b"), "\"a,b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
    }

    #[test]
    fn list_columns_are_measured_in_cells() {
        assert!(!wide_char('a'));
        assert!(wide_char('作'));
        assert!(wide_char('｜'));
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("甲乙"), 4);
        assert_eq!(display_width(&pad_to("甲", 6)), 6);
        let cut = truncate("一二三四五六七八", 6);
        assert!(cut.ends_with('…') && display_width(&cut) <= 6);
        assert_eq!(one_line("甲\n乙"), "甲 乙");
    }

    #[test]
    fn nonce_age_reads_sign_time() {
        let url = "https://finder.video.qq.com/251/20302/stodownload?encfilekey=a&svrnonce=1789711320#x";
        assert!(query_param(url, "svrnonce") == Some("1789711320".to_string()));
        assert!(nonce_age_secs(url).unwrap() > 0);
        assert_eq!(nonce_age_secs("https://a/b"), None);
    }

    #[test]
    fn failures_get_actionable_text() {
        assert!(classify_failure(Some("HTTP 400 Bad Request")).contains("400"));
        assert!(classify_failure(Some("finder url expire!")).contains("过期"));
        assert!(classify_failure(Some("timeout waiting")).contains("超时"));
        assert!(classify_failure(None).contains("未知"));
    }
}
