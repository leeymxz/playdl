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
}

/// Result of one attempted download, for the run summary.
#[derive(Clone, Debug)]
struct Row {
    name: String,
    bytes: u64,
    ok: bool,
    masked: bool,
    note: String,
}

pub async fn run(opts: WxOpts) -> i32 {
    let items = match load_items(&opts.source).await {
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

    let mut rows = Vec::with_capacity(items.len());
    for item in &items {
        rows.push(fetch_one(item, &opts).await);
    }

    let ok_count = rows.iter().filter(|r| r.ok).count();
    let masked = rows.iter().filter(|r| r.masked).count();
    if opts.json {
        let arr: Vec<serde_json::Value> = rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name, "bytes": r.bytes, "ok": r.ok,
                    "protected": r.masked, "note": r.note,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
    } else {
        println!("\n—— 汇总 ——  成功 {ok_count}/{}  受限 {masked}", rows.len());
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

/// Read items from a URL, a JSON/JSONL manifest, or a URL-per-line file.
pub async fn load_items(source: &str) -> Result<Vec<Item>, String> {
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
    parse_manifest(&text)
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
/// filename component.
fn sanitize_name(s: &str, limit: usize) -> String {
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
        return "untitled".to_string();
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

fn target_path(item: &Item, opts: &WxOpts, ext: &str, masked: bool) -> PathBuf {
    let stem = stem_for(item);
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

async fn fetch_one(item: &Item, opts: &WxOpts) -> Row {
    let display = stem_for(item);
    if item.url.trim().is_empty() {
        let note = "条目没有下载地址".to_string();
        if !opts.json {
            println!("跳过  {display}  {note}");
        }
        return Row { name: display, bytes: 0, ok: false, masked: false, note };
    }

    if let Some(age) = nonce_age_secs(&item.url) {
        if age > 86_400 {
            let days = age / 86_400;
            if !opts.json {
                println!("提示  {display}  这份链接签发于 {days} 天前，很可能已过期");
            }
        }
    }

    let ext = extension_of(&item.url, "mp4");
    // A previous run may already have stored this entry under its masked name.
    let plain = target_path(item, opts, &ext, false);
    let masked_path = target_path(item, opts, &ext, true);
    if let Some(size) = item.size {
        for p in [&plain, &masked_path] {
            if let Ok(md) = std::fs::metadata(p) {
                if md.len() == size {
                    if !opts.json {
                        println!("跳过  {display}  已存在且大小一致（{} 字节）", md.len());
                    }
                    return Row {
                        name: display.clone(),
                        bytes: md.len(),
                        ok: true,
                        masked: p == &masked_path,
                        note: "已存在且大小一致，跳过".to_string(),
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
    job.show_error = true;
    if opts.referer {
        job.headers
            .push("Referer: https://channels.weixin.qq.com/".to_string());
    }
    if opts.json {
        job.quiet = true;
        job.no_progress = true;
    } else {
        job.quiet = false;
        job.no_progress = false;
    }

    // `display` already carries the author as `作者 - 标题`, so it is not
    // repeated here.
    if !opts.json {
        println!("\n下载  {display}");
    }

    let out = crate::download::run(job).await;
    let mut row = Row {
        name: display.clone(),
        bytes: out.size,
        ok: out.ok,
        masked: false,
        note: String::new(),
    };

    if !out.ok {
        row.note = classify_failure(out.note.as_deref());
        if !opts.json {
            println!("失败  {display}  {}", row.note);
        }
        return row;
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
            row.note = "内容不是标准容器（可能加了头部保护），已原样保存并标注 .masked".to_string();
            if !opts.json {
                println!("受限  {display}  {}", row.note);
            }
        }
    }

    if opts.cover {
        if let Some(cover) = &item.cover {
            if looks_like_url(cover) {
                let mut cj = crate::download::default_job();
                cj.urls = vec![cover.clone()];
                let cext = extension_of(cover, "jpg");
                cj.output = Some(target_path(item, opts, &cext, false));
                cj.resume = true;
                cj.create_dirs = true;
                cj.quiet = true;
                cj.no_progress = true;
                let _ = crate::download::run(cj).await;
            }
        }
    }

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
