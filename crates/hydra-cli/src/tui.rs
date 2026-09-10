//! Full-screen interactive download manager.
//!
//! Hand-rolled on `crossterm` rather than a widget framework. The reason is the
//! same one that shaped `progress.rs`: the state worth showing is per-connection,
//! and a generic table widget flattens it. It also keeps the dependency surface
//! small enough that this builds offline.
//!
//! All queue behaviour lives in [`crate::queue`] and is unit-tested there. This
//! module is only input handling and drawing, so a rendering change cannot break
//! scheduling.
//!
//! # Terminal state is restored on every exit path
//!
//! Raw mode and the alternate screen are process-global terminal state. Leaving
//! them set because a transfer panicked hands the user a shell with no echo and no
//! line editing, which they then have to fix with `reset`. The guard below restores
//! on drop, so panics and `?` returns both clean up.

use crate::queue::{EventLog, Queue, State};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::{cursor, execute, terminal};
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;

/// Restores terminal state on drop, including during a panic.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(io::stdout(), terminal::EnterAlternateScreen, cursor::Hide)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), cursor::Show, terminal::LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

/// What the user asked for on this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    /// Quit and stop every running transfer.
    Quit,
    /// Leave the UI but let running transfers continue in the background.
    ///
    /// Distinct from Quit because the two are opposite intentions and sharing one key
    /// for both is how people lose a 4 GB download: `q` cancels, `b` detaches. The queue
    /// file is the handoff â€?a later session reads it and reattaches.
    Background,
    Pause(u64),
    Resume(u64),
    Cancel(u64),
    MoveUp(u64),
    MoveDown(u64),
    ClearFinished,
    /// Adjust how many transfers may run at once.
    Concurrency(isize),
    /// Open the per-connection detail screen for an item.
    OpenDetail(u64),
    /// Return to the list.
    CloseDetail,
    Add(String),
    None,
}

/// Which pane has focus / what the UI is doing.
#[derive(Clone, PartialEq, Eq, Debug)]
/// Which screen is showing.
///
/// A detail view exists because the list row cannot answer the question a stalled
/// download raises: *which* mirror is slow, which byte range is stuck, how many repairs
/// have fired. That state is per-connection and there is no room for it in a single
/// line, so it gets its own screen rather than a wider table.
pub enum Mode {
    List,
    /// Typing a URL to add.
    Adding(String),
    /// Per-connection detail for one queue item.
    Detail,
    Help,
}

pub struct Ui {
    /// Live per-connection state, by queue id, as reported by running transfers.
    pub live: std::collections::HashMap<u64, crate::download::Tick>,
    /// The item whose detail screen is showing, if any.
    pub detail: Option<u64>,
    pub selected: usize,
    pub mode: Mode,
    pub log: EventLog,
    /// Sparkline of aggregate rate.
    history: Vec<f64>,
}

impl Default for Ui {
    fn default() -> Self {
        Self::new()
    }
}

impl Ui {
    pub fn new() -> Self {
        Self {
            live: std::collections::HashMap::new(),
            detail: None,
            selected: 0,
            mode: Mode::List,
            log: EventLog::new(256),
            history: Vec::new(),
        }
    }

    /// Map a key to a command against the current queue.
    ///
    /// Pure: takes the queue read-only and returns a command, so every binding is
    /// testable without a terminal.
    pub fn on_key(&mut self, k: KeyEvent, q: &Queue) -> Command {
        // Ctrl-C quits from anywhere, including mid-typing.
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Command::Quit;
        }
        match &mut self.mode {
            Mode::Adding(buf) => match k.code {
                KeyCode::Esc => {
                    self.mode = Mode::List;
                    Command::None
                }
                KeyCode::Enter => {
                    let url = buf.trim().to_string();
                    self.mode = Mode::List;
                    if url.is_empty() {
                        Command::None
                    } else {
                        Command::Add(url)
                    }
                }
                KeyCode::Backspace => {
                    buf.pop();
                    Command::None
                }
                KeyCode::Char(c) => {
                    buf.push(c);
                    Command::None
                }
                _ => Command::None,
            },
            Mode::Help => {
                self.mode = Mode::List;
                Command::None
            }
            // Detail view: Esc (or q) returns to the list, and the item-level actions
            // still work so a stalled download can be paused without going back first.
            Mode::Detail => {
                let sel = self.detail;
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                        self.mode = Mode::List;
                        self.detail = None;
                        Command::CloseDetail
                    }
                    KeyCode::Char('p') => sel.map(Command::Pause).unwrap_or(Command::None),
                    KeyCode::Char('r') => sel.map(Command::Resume).unwrap_or(Command::None),
                    KeyCode::Char('d') => sel.map(Command::Cancel).unwrap_or(Command::None),
                    KeyCode::Char('?') | KeyCode::Char('h') => {
                        self.mode = Mode::Help;
                        Command::None
                    }
                    _ => Command::None,
                }
            }
            Mode::List => {
                let sel = q.items.get(self.selected).map(|i| i.id);
                match k.code {
                    // `q` stops everything; `b` detaches and lets transfers continue.
                    // Esc is NOT a quit key here: it is the "go back" key everywhere
                    // else in this UI, and making it also mean "cancel all downloads"
                    // is how someone loses a transfer by reflex.
                    KeyCode::Char('q') => Command::Quit,
                    KeyCode::Char('b') => Command::Background,
                    KeyCode::Enter => sel.map(Command::OpenDetail).unwrap_or(Command::None),
                    KeyCode::Char('?') | KeyCode::Char('h') => {
                        self.mode = Mode::Help;
                        Command::None
                    }
                    KeyCode::Char('a') => {
                        self.mode = Mode::Adding(String::new());
                        Command::None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        if !q.items.is_empty() {
                            self.selected = (self.selected + 1).min(q.items.len() - 1);
                        }
                        Command::None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.selected = self.selected.saturating_sub(1);
                        Command::None
                    }
                    KeyCode::Char('p') => sel.map(Command::Pause).unwrap_or(Command::None),
                    KeyCode::Char('r') => sel.map(Command::Resume).unwrap_or(Command::None),
                    KeyCode::Char('d') | KeyCode::Delete => {
                        sel.map(Command::Cancel).unwrap_or(Command::None)
                    }
                    KeyCode::Char('K') => sel.map(Command::MoveUp).unwrap_or(Command::None),
                    KeyCode::Char('J') => sel.map(Command::MoveDown).unwrap_or(Command::None),
                    KeyCode::Char('c') => Command::ClearFinished,
                    KeyCode::Char('+') | KeyCode::Char('=') => Command::Concurrency(1),
                    KeyCode::Right | KeyCode::Char('l') => {
                        sel.map(Command::OpenDetail).unwrap_or(Command::None)
                    }
                    KeyCode::Char('-') => Command::Concurrency(-1),
                    _ => Command::None,
                }
            }
        }
    }

    /// Keep the selection inside the list after items are removed.
    pub fn clamp_selection(&mut self, q: &Queue) {
        if q.items.is_empty() {
            self.selected = 0;
        } else if self.selected >= q.items.len() {
            self.selected = q.items.len() - 1;
        }
    }

    pub fn note_rate(&mut self, rate: f64) {
        self.history.push(rate);
        if self.history.len() > 64 {
            self.history.remove(0);
        }
    }

    /// Render the whole screen into a string of ANSI, so drawing is testable.
    /// Per-connection detail for one item.
    ///
    /// Answers the question the list cannot: which mirror is slow, which byte range is
    /// stuck, how much has actually arrived. Falls back to a clear message when the item
    /// is not running, rather than drawing an empty table that looks broken.
    pub fn render_detail(&self, q: &Queue, id: u64, cols: u16, _rows: u16) -> String {
        let w = cols.max(60) as usize;
        let mut o = String::new();
        let Some(item) = q.get(id) else {
            return "  that item is gone â€?press Esc\r\n".into();
        };
        o.push_str("\x1b[H\x1b[2J");
        o.push_str(&format!(
            "\x1b[1m {}\x1b[0m  \x1b[90m#{} {}\x1b[0m\r\n",
            item.name(),
            item.id,
            item.state.as_str()
        ));
        o.push_str(&format!(" \x1b[90m{}\x1b[0m\r\n", "â”€".repeat(w.min(150))));

        let live = self.live.get(&id);
        let done = live.map(|t| t.done).unwrap_or(item.done_bytes);
        let size = live.and_then(|t| t.size).or(item.size);
        match size {
            Some(sz) if sz > 0 => {
                let frac = done as f64 / sz as f64;
                let bar_w = (w.saturating_sub(34)).clamp(10, 60);
                let filled = ((frac * bar_w as f64).round() as usize).min(bar_w);
                o.push_str(&format!(
                    "  \x1b[36m{}\x1b[0m{}  {:>5.1}%  {} / {}\r\n",
                    "â”?.repeat(filled),
                    "â”€".repeat(bar_w - filled),
                    frac * 100.0,
                    crate::progress::human(done),
                    crate::progress::human(sz)
                ));
            }
            _ => o.push_str(&format!(
                "  {} downloaded (total size unknown)\r\n",
                crate::progress::human(done)
            )),
        }
        if let Some(t) = live {
            o.push_str(&format!(
                "  {}/s aggregate   {} request(s)   {} repair(s)\r\n",
                crate::progress::human(t.rate as u64),
                t.requests,
                t.repairs
            ));
        }
        o.push_str("\r\n");

        match live {
            Some(t) if !t.conns.is_empty() => {
                o.push_str("  \x1b[90mconn  source                     range                    rate      health\x1b[0m\r\n");
                for (i, c) in t.conns.iter().enumerate() {
                    let span = c.hi.saturating_sub(c.lo);
                    let got = c.pos.saturating_sub(c.lo);
                    let mini = if span > 0 {
                        let cells = 10usize;
                        let f = ((got as f64 / span as f64) * cells as f64).round() as usize;
                        format!(
                            "[{}{}]",
                            "â–?.repeat(f.min(cells)),
                            "Â·".repeat(cells - f.min(cells))
                        )
                    } else {
                        "[----------]".into()
                    };
                    let colour = match c.health.as_str() {
                        "healthy" => "\x1b[32m",
                        "suspect" => "\x1b[33m",
                        "degraded" => "\x1b[31m",
                        "stalled" => "\x1b[35m",
                        _ => "\x1b[90m",
                    };
                    let host: String = c.host.chars().take(24).collect();
                    o.push_str(&format!(
                        "  #{i:<4} {host:<24} {mini} {:>9}-{:<9} {:>9}/s  {colour}{}\x1b[0m\r\n",
                        c.lo,
                        c.hi,
                        crate::progress::human(c.rate as u64),
                        c.health
                    ));
                }
            }
            _ => o.push_str("  no live connection detail (the item is not transferring)\r\n"),
        }
        if let Some(e) = &item.error {
            o.push_str(&format!("\r\n  \x1b[31m{e}\x1b[0m\r\n"));
        }
        o.push_str(&format!(
            "\r\n \x1b[90m{}\x1b[0m\r\n",
            "â”€".repeat(w.min(150))
        ));
        o.push_str("  \x1b[90mEsc back   p pause   r resume   d cancel   ? help\x1b[0m\r\n");
        o
    }

    pub fn render(&self, q: &Queue, cols: u16, rows: u16) -> String {
        use std::fmt::Write as _;
        let w = cols.max(40) as usize;
        let mut s = String::new();
        let _ = write!(s, "\x1b[H\x1b[2J");

        // ---- header ----
        let (queued, running, done, failed) = q.counts();
        let _ = writeln!(
            s,
            "\x1b[1;36m hydra\x1b[0m  \x1b[90mfile retriever\x1b[0m{:>width$}\r",
            format!(
                "{} running  {} queued  {} done  {} failed  |  {}/s  |  max {}",
                running,
                queued,
                done,
                failed,
                crate::progress::human(q.total_rate() as u64),
                q.max_active
            ),
            width = w.saturating_sub(24)
        );
        let _ = writeln!(s, "\x1b[90m{}\x1b[0m\r", "â”€".repeat(w));

        if self.mode == Mode::Help {
            for line in [
                "  Keys",
                "",
                "    Enter     open per-connection detail for the selected item",
                "    Esc       leave the detail screen (never quits)",
                "    a         add a URL",
                "    j / k     move selection down / up",
                "    p / r     pause / resume the selected transfer",
                "    d         cancel the selected transfer",
                "    J / K     move the selected item down / up in the queue",
                "    c         clear finished and cancelled items",
                "    + / -     how many transfers run AT ONCE (queue parallelism)",
                "    ? or h    this help",
                "    b         background: leave the UI, transfers keep running",
                "    q         quit: stop the running transfers and exit",
                "",
                "  + / - is queue parallelism, not connection count",
                "",
                "    It sets how many QUEUED ITEMS may transfer simultaneously (1-16).",
                "    Each item still opens as many CONNECTIONS as the scheduler measures",
                "    to be useful for its own mirrors, which is a separate number chosen",
                "    per path: on a link already saturated by one connection, more",
                "    connections add no speed, so the scheduler declines to open them.",
                "    Raising this multiplies total connections; on a shared or metered",
                "    link that is the setting to lower first.",
                "",
                "  b and q are different on purpose",
                "",
                "    b detaches and leaves transfers running, recording the queue so a",
                "    later session reattaches. q stops them. One key for both is how a",
                "    large download gets lost by reflex, so Esc is never a quit key.",
                "",
                "  Paused and failed transfers keep their bytes; resuming continues",
                "  from where they stopped, provided the server still offers the same",
                "  validator. Without one, resume is refused rather than risking a",
                "  file spliced from two different versions of the object.",
                "",
                "  press any key to return",
            ] {
                let _ = writeln!(s, "{line}\r");
            }
            return s;
        }

        // ---- list ----
        let list_rows = (rows as usize).saturating_sub(8).max(3);
        if q.items.is_empty() {
            let _ = writeln!(
                s,
                "\r\n  \x1b[90mthe queue is empty â€?press 'a' to add a URL\x1b[0m\r"
            );
        }
        for (idx, it) in q.items.iter().take(list_rows).enumerate() {
            let sel = idx == self.selected;
            let marker = if sel { "\x1b[7mâ–? } else { " " };
            let (tag, colour) = match it.state {
                State::Running => ("run ", "\x1b[32m"),
                State::Queued => ("wait", "\x1b[36m"),
                State::Paused => ("hold", "\x1b[33m"),
                State::Done => ("done", "\x1b[92m"),
                State::Failed => ("fail", "\x1b[31m"),
                State::Cancelled => ("gone", "\x1b[90m"),
            };
            let bar_w = 22usize;
            let bar = match it.fraction() {
                Some(fr) => {
                    let k = (fr * bar_w as f64).round() as usize;
                    format!("{}{}", "â”?.repeat(k), "â”€".repeat(bar_w - k))
                }
                None => "?".repeat(bar_w),
            };
            let pct = it
                .fraction()
                .map(|f| format!("{:5.1}%", 100.0 * f))
                .unwrap_or_else(|| "    ?".into());
            let size = it
                .size
                .map(crate::progress::human)
                .unwrap_or_else(|| "?".into());
            let _ = writeln!(
                s,
                "{marker} {colour}{tag}\x1b[0m {:<28} {bar} {pct} {:>10}/{:<10} {:>10}/s\x1b[0m\r",
                trunc(&it.name(), 28),
                crate::progress::human(it.done_bytes),
                size,
                crate::progress::human(it.rate as u64)
            );
            if let Some(e) = &it.error {
                let _ = writeln!(
                    s,
                    "      \x1b[31m{}\x1b[0m\r",
                    trunc(e, w.saturating_sub(8))
                );
            }
        }

        // ---- footer ----
        let _ = writeln!(s, "\x1b[90m{}\x1b[0m\r", "â”€".repeat(w));
        for line in self.log.recent(3) {
            let _ = writeln!(s, "  \x1b[90m{}\x1b[0m\r", trunc(line, w.saturating_sub(4)));
        }
        match &self.mode {
            Mode::Adding(buf) => {
                let _ = writeln!(s, "\r\n  add URL: \x1b[4m{buf}\x1b[0mâ–?  \x1b[90m(enter to add, esc to cancel)\x1b[0m\r");
            }
            _ => {
                let _ = writeln!(
                    s,
                    "\r\n  \x1b[90mEnter\x1b[0m detail  \x1b[90ma\x1b[0m add  \x1b[90mp\x1b[0m pause  \
                     \x1b[90mr\x1b[0m resume  \x1b[90md\x1b[0m cancel  \x1b[90mJ/K\x1b[0m reorder  \
                     \x1b[90mc\x1b[0m clear  \x1b[90m+/-\x1b[0m parallel jobs  \x1b[90m?\x1b[0m help  \
                     \x1b[90mb\x1b[0m background  \x1b[90mq\x1b[0m quit\r"
                );
            }
        }
        s
    }
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let keep: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{keep}â€?)
    }
}

/// Render a representative queue screen to stdout, for documentation and for
/// reviewing the layout without a terminal.
///
/// Drives the real renderer, so what appears here is what a user sees.
pub fn demo_screen() {
    let mut q = Queue::new(3);
    q.add(
        vec!["http://mirror.example.org/ubuntu-24.04.2-desktop-amd64.iso".into()],
        "ubuntu-24.04.2-desktop-amd64.iso".into(),
    );
    q.add(
        vec!["http://mirror.example.org/dataset-2026.tar.zst".into()],
        "dataset-2026.tar.zst".into(),
    );
    q.add(
        vec!["http://cdn.example.net/model-weights.safetensors".into()],
        "model-weights.safetensors".into(),
    );
    q.add(
        vec!["http://cdn.example.net/lecture-03.mkv".into()],
        "lecture-03.mkv".into(),
    );
    q.add(
        vec!["http://broken.example.net/missing.bin".into()],
        "missing.bin".into(),
    );
    q.mark_running(1);
    q.progress(1, 2_950_000_000, Some(6_203_355_136), 24.4e6);
    q.mark_running(2);
    q.progress(2, 411_000_000, Some(890_000_000), 11.2e6);
    q.mark_running(3);
    q.progress(3, 96_000_000, Some(4_100_000_000), 1.1e6);
    q.pause(4);
    q.progress(4, 240_000_000, Some(700_000_000), 0.0);
    q.mark_running(5);
    q.max_attempts = 1;
    q.fail(5, "404 Not Found (no source served this object)".into());
    let mut ui = Ui::new();
    ui.log.push("start #3 model-weights.safetensors");
    ui.log
        .push("warning #2: content is zstd (archive) but the name says tar.zst (archive)");
    ui.log.push("fail #5: 404 Not Found");
    ui.selected = 2;
    print!("{}", ui.render(&q, 108, 26));
    println!();
}

/// Run the interactive manager.
///
/// Transfers are driven by the same engine the one-shot CLI uses, so the
/// interactive path cannot drift from the scripted one.
/// Run the manager, optionally forcing headless mode.
///
/// `force_headless` exists because the detached worker must never try to take a terminal:
/// it has none, and probing for one would make the decision depend on how it was spawned.
pub async fn run_with(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
    force_headless: bool,
) -> io::Result<()> {
    if force_headless {
        return run_headless(queue_path, initial, max_active).await;
    }
    // Raw mode requires a terminal. Without this check the failure surfaces as
    // "Operation not permitted (os error 1)", which tells the user nothing about
    // what to do. Headless mode below runs the same queue without a screen, so a
    // script or a CI job is not locked out of the queue manager.
    use std::io::IsTerminal as _;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return run_headless(queue_path, initial, max_active).await;
    }
    run_interactive(queue_path, initial, max_active).await
}

/// Load the queue (or start a fresh one) and enqueue the URLs given on the
/// command line. The shared entry step of both manager modes â€?headless and
/// interactive must agree on how a queue resumes and how a bare URL is named.
fn load_queue(queue_path: &std::path::Path, initial: Vec<String>, max_active: usize) -> Queue {
    let mut q = Queue::load(queue_path).unwrap_or_else(|| Queue::new(max_active));
    q.max_active = max_active.max(1);
    for url in initial {
        let name = crate::url::Url::parse(&url)
            .map(|u| u.suggested_filename())
            .unwrap_or_else(|| "download".into());
        q.add(vec![url], PathBuf::from(name));
    }
    q
}

/// Drive the queue to completion with no terminal, logging one line per event.
///
/// Same queue, same engine, no screen: this is what runs under `nohup`, in CI, or
/// anywhere stdin is not a terminal.
pub async fn run_headless(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
) -> io::Result<()> {
    let mut q = load_queue(&queue_path, initial, max_active);
    eprintln!(
        "playdl: no terminal; running the queue headless ({} items)",
        q.items.len()
    );
    let mut running: std::collections::HashMap<
        u64,
        tokio::task::JoinHandle<crate::download::Outcome>,
    > = std::collections::HashMap::new();

    // Headless mode still consumes ticks: one log line per interval is how a script or
    // CI job sees that a transfer is alive rather than hung.
    let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel::<crate::download::Tick>();
    let mut last_log = std::time::Instant::now();

    while !q.is_idle() || !running.is_empty() {
        for id in q.to_start() {
            let Some(item) = q.get(id).cloned() else {
                continue;
            };
            q.mark_running(id);
            // Persist immediately: the queue file is how a reattaching session sees that
            // this item is live and which process owns it. Saving only on completion made
            // a detached worker's progress invisible â€?the UI reloaded, saw `Queued` with
            // no owner, and treated a running transfer as a phantom.
            let _ = q.save(&queue_path);
            eprintln!("playdl: start #{id} {}", item.name());
            running.insert(
                id,
                tokio::spawn(crate::download::run(job_for(&item, Some(tick_tx.clone())))),
            );
        }
        let done: Vec<u64> = running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(i, _)| *i)
            .collect();
        for id in done {
            if let Some(h) = running.remove(&id) {
                match h.await {
                    Ok(out) if out.ok => {
                        q.progress(id, out.size, Some(out.size), 0.0);
                        q.finish(id, out.sha256.clone(), out.category.clone());
                        eprintln!(
                            "playdl: done #{id} {} {}",
                            crate::progress::human(out.size),
                            out.category.unwrap_or_default()
                        );
                        if let Some(c) = out.format_conflict {
                            eprintln!("playdl: warning #{id}: {c}");
                        }
                    }
                    Ok(out) => {
                        let why = out.note.unwrap_or_else(|| "failed".into());
                        let st = q.fail(id, why.clone());
                        eprintln!("playdl: {} #{id}: {why}", st.as_str());
                    }
                    Err(e) => {
                        let st = q.fail(id, format!("task error: {e}"));
                        eprintln!("playdl: {} #{id}: task error: {e}", st.as_str());
                    }
                }
                let _ = q.save(&queue_path);
            }
        }
        // Drain live progress: without this the queue only learned a transfer's size
        // when it FINISHED, so nothing moved for the whole download.
        while let Ok(tk) = tick_rx.try_recv() {
            q.progress(tk.id, tk.done, tk.size, tk.rate);
        }
        if last_log.elapsed().as_secs_f64() >= 2.0 && !q.is_idle() {
            last_log = std::time::Instant::now();
            // Checkpoint progress so a reattaching UI shows real numbers rather than the
            // values from whenever the last item finished.
            let _ = q.save(&queue_path);
            for it in q
                .items
                .iter()
                .filter(|i| i.state == crate::queue::State::Running)
            {
                eprintln!(
                    "playdl: #{} {} {} {}",
                    it.id,
                    it.name(),
                    match (it.done_bytes, it.size) {
                        (d, Some(s)) if s > 0 => format!("{:.1}%", 100.0 * d as f64 / s as f64),
                        (d, _) => crate::progress::human(d),
                    },
                    crate::progress::human(it.rate as u64) + "/s"
                );
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let (_, _, done, failed) = q.counts();
    eprintln!("playdl: queue finished â€?{done} done, {failed} failed");
    let _ = q.save(&queue_path);
    Ok(())
}

/// Start a detached process that keeps working the queue after the UI exits.
///
/// A re-exec of this binary in headless queue mode, in its own session so it survives the
/// terminal closing, with its streams sent to a log file beside the queue so a later
/// session can see what happened rather than losing the output to /dev/null.
fn spawn_worker(queue_path: &std::path::Path, max_active: usize) -> io::Result<u32> {
    let exe = std::env::current_exe()?;
    let log_path = queue_path.with_extension("log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let err = log.try_clone()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("interactive")
        .arg("--headless")
        .arg("--queue-file")
        .arg(queue_path)
        .arg("--max-active")
        .arg(max_active.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log))
        .stderr(std::process::Stdio::from(err));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // setsid: a new session with no controlling terminal, so closing the terminal
        // does not deliver SIGHUP to the worker.
        unsafe {
            cmd.pre_exec(|| {
                extern "C" {
                    fn setsid() -> i32;
                }
                if setsid() == -1 {
                    // Already a session leader is fine; anything else is not fatal
                    // either, so do not fail the detach over it.
                }
                Ok(())
            });
        }
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}

/// The job the queue manager runs for one item, in either mode.
fn job_for(
    item: &crate::queue::Item,
    ticks: Option<tokio::sync::mpsc::UnboundedSender<crate::download::Tick>>,
) -> crate::download::Job {
    // Everything not named here is the engine default (`default_job` is already
    // quiet and progress-free â€?the engine must not write to the screen while
    // the manager owns it).
    crate::download::Job {
        ticks: ticks.map(|tx| (item.id, tx)),
        urls: item.urls.clone(),
        output: Some(item.output.clone()),
        resume: true,
        create_dirs: true,
        force: true, // queued items were already decided by the queue, not a prompt
        ..crate::download::default_job()
    }
}

async fn run_interactive(
    queue_path: PathBuf,
    initial: Vec<String>,
    max_active: usize,
) -> io::Result<()> {
    let mut q = load_queue(&queue_path, initial, max_active);

    let _guard = TerminalGuard::enter()?;
    let mut ui = Ui::new();
    // Live progress from running transfers. Unbounded because dropping a tick is
    // harmless (the next one supersedes it) but blocking a transfer to deliver one is
    // not â€?a UI must never be able to stall the download it is displaying.
    let (tick_tx, mut tick_rx) = tokio::sync::mpsc::unbounded_channel::<crate::download::Tick>();
    // PID of the detached worker, when `b` handed the queue over.
    let mut background_pid: Option<u32> = None;
    ui.log.push("ready");

    // Running transfers, keyed by queue id.
    let mut running: std::collections::HashMap<
        u64,
        tokio::task::JoinHandle<crate::download::Outcome>,
    > = std::collections::HashMap::new();

    loop {
        // ---- start whatever may start ----
        for id in q.to_start() {
            let Some(item) = q.get(id).cloned() else {
                continue;
            };
            q.mark_running(id);
            ui.log.push(format!("start #{id} {}", item.name()));
            running.insert(
                id,
                tokio::spawn(crate::download::run(job_for(&item, Some(tick_tx.clone())))),
            );
        }

        // ---- reap finished transfers ----
        let finished: Vec<u64> = running
            .iter()
            .filter(|(_, h)| h.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in finished {
            if let Some(h) = running.remove(&id) {
                match h.await {
                    Ok(out) if out.ok => {
                        q.progress(id, out.size, Some(out.size), 0.0);
                        q.finish(id, out.sha256.clone(), out.category.clone());
                        ui.log.push(format!(
                            "done #{id} {} {}",
                            crate::progress::human(out.size),
                            out.category.unwrap_or_default()
                        ));
                        if let Some(c) = out.format_conflict {
                            ui.log.push(format!("warning #{id}: {c}"));
                        }
                    }
                    Ok(out) => {
                        let why = out.note.unwrap_or_else(|| "failed".into());
                        let st = q.fail(id, why.clone());
                        ui.log.push(format!("{} #{id}: {why}", st.as_str()));
                    }
                    Err(e) => {
                        let st = q.fail(id, format!("task error: {e}"));
                        ui.log.push(format!("{} #{id}: task error", st.as_str()));
                    }
                }
            }
        }

        // ---- input ----
        if event::poll(Duration::from_millis(120))? {
            if let Event::Key(k) = event::read()? {
                match ui.on_key(k, &q) {
                    Command::Quit => break,
                    Command::Pause(id) => {
                        q.pause(id);
                        if let Some(h) = running.remove(&id) {
                            // Abort rather than waiting: the sidecar already records
                            // what landed, so the bytes are not lost.
                            h.abort();
                        }
                        ui.log.push(format!("paused #{id}"));
                    }
                    Command::Resume(id) => {
                        q.resume(id);
                        ui.log.push(format!("resumed #{id}"));
                    }
                    Command::Cancel(id) => {
                        q.cancel(id);
                        if let Some(h) = running.remove(&id) {
                            h.abort();
                        }
                        ui.log.push(format!("cancelled #{id}"));
                    }
                    Command::MoveUp(id) => q.reorder(id, -1),
                    Command::MoveDown(id) => q.reorder(id, 1),
                    Command::ClearFinished => {
                        let n = q.clear_finished();
                        ui.log.push(format!("cleared {n}"));
                    }
                    Command::Concurrency(d) => {
                        q.max_active = (q.max_active as isize + d).clamp(1, 16) as usize;
                        ui.log.push(format!(
                            "max {} transfer(s) at once (each still opens its own connections)",
                            q.max_active
                        ));
                    }
                    // Open/close the detail screen. The queue is untouched â€?these are
                    // view changes, and keeping them out of the queue state is what
                    // makes both testable without a terminal.
                    Command::OpenDetail(id) => {
                        ui.detail = Some(id);
                        ui.mode = Mode::Detail;
                    }
                    Command::CloseDetail => {}
                    Command::Background => {
                        // Backgrounding cannot just leave the loop: the transfers are
                        // tasks in THIS process, so exiting kills them and a later session
                        // finds everything stopped. Hand the queue to a detached worker
                        // instead, which is what "keeps running" has to mean.
                        //
                        // In-process transfers are aborted first and their items demoted,
                        // because two processes writing one file is worse than restarting
                        // from a checkpoint â€?and the sidecar is checkpointed during the
                        // transfer, so the worker resumes rather than refetching.
                        for (id, h) in running.drain() {
                            h.abort();
                            if let Some(i) = q.items.iter_mut().find(|i| i.id == id) {
                                i.state = crate::queue::State::Queued;
                                i.owner_pid = None;
                                i.rate = 0.0;
                            }
                        }
                        let _ = q.save(&queue_path);
                        match spawn_worker(&queue_path, q.max_active) {
                            Ok(pid) => {
                                background_pid = Some(pid);
                                break;
                            }
                            Err(e) => {
                                ui.log.push(format!("could not detach: {e}"));
                            }
                        }
                    }
                    Command::Add(url) => match crate::url::Url::parse(&url) {
                        Some(u) => {
                            let id =
                                q.add(vec![url.clone()], PathBuf::from(u.suggested_filename()));
                            ui.log
                                .push(format!("queued #{id} {}", u.suggested_filename()));
                        }
                        None => ui.log.push(format!("not a usable URL: {url}")),
                    },
                    Command::None => {}
                }
                ui.clamp_selection(&q);
            }
        }

        ui.note_rate(q.total_rate());
        let (cols, rows) = terminal::size().unwrap_or((100, 30));
        while let Ok(tk) = tick_rx.try_recv() {
            q.progress(tk.id, tk.done, tk.size, tk.rate);
            ui.live.insert(tk.id, tk);
        }
        // The detail screen replaces the list rather than overlaying it: the
        // per-connection table needs the full width, and a split view at 80 columns
        // truncates both halves into uselessness.
        let frame = match (&ui.mode, ui.detail) {
            (Mode::Detail, Some(id)) => ui.render_detail(&q, id, cols, rows),
            _ => ui.render(&q, cols, rows),
        };
        let mut so = io::stdout();
        so.write_all(frame.as_bytes())?;
        so.flush()?;

        // Persist so a crash or a quit does not lose the plan.
        let _ = q.save(&queue_path);
    }

    // On `b` the queue was already handed to a detached worker, so the items must stay
    // queued for it to pick up â€?pausing them here would stop the very transfers the user
    // asked to keep running.
    if let Some(pid) = background_pid {
        let _ = q.save(&queue_path);
        eprintln!(
            "playdl: detached â€?worker pid {pid} is continuing {} item(s)",
            q.items.iter().filter(|i| !i.state.is_terminal()).count()
        );
        eprintln!("       queue:  {}", queue_path.display());
        eprintln!(
            "       log:    {}",
            queue_path.with_extension("log").display()
        );
        eprintln!(
            "       reattach with:  hydra interactive --queue-file {}",
            queue_path.display()
        );
        return Ok(());
    }

    // Anything still running is recorded as paused, not lost: its bytes and
    // sidecar are on disk and `-c` or a later session picks them up.
    for (id, h) in running.drain() {
        h.abort();
        q.pause(id);
    }
    let _ = q.save(&queue_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }
    fn shift(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }

    fn q2() -> Queue {
        let mut q = Queue::new(2);
        q.add(vec!["http://a/one.iso".into()], "one.iso".into());
        q.add(vec!["http://a/two.zip".into()], "two.zip".into());
        q
    }

    #[test]
    fn navigation_stays_in_bounds() {
        let mut ui = Ui::new();
        let q = q2();
        for _ in 0..5 {
            ui.on_key(key('j'), &q);
        }
        assert_eq!(ui.selected, 1, "must not run past the last item");
        for _ in 0..5 {
            ui.on_key(key('k'), &q);
        }
        assert_eq!(ui.selected, 0, "must not run before the first item");
    }

    #[test]
    fn navigation_on_an_empty_queue_does_not_panic() {
        let mut ui = Ui::new();
        let q = Queue::new(1);
        assert_eq!(ui.on_key(key('j'), &q), Command::None);
        assert_eq!(
            ui.on_key(key('p'), &q),
            Command::None,
            "nothing selected, nothing to pause"
        );
        assert_eq!(ui.selected, 0);
    }

    #[test]
    fn bindings_map_to_the_selected_item() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('j'), &q);
        assert_eq!(ui.on_key(key('p'), &q), Command::Pause(2));
        assert_eq!(ui.on_key(key('r'), &q), Command::Resume(2));
        assert_eq!(ui.on_key(key('d'), &q), Command::Cancel(2));
        assert_eq!(ui.on_key(shift('K'), &q), Command::MoveUp(2));
        assert_eq!(ui.on_key(shift('J'), &q), Command::MoveDown(2));
    }

    #[test]
    fn concurrency_and_clear_are_global_not_per_item() {
        let mut ui = Ui::new();
        let q = q2();
        assert_eq!(ui.on_key(key('+'), &q), Command::Concurrency(1));
        assert_eq!(ui.on_key(key('-'), &q), Command::Concurrency(-1));
        assert_eq!(ui.on_key(key('c'), &q), Command::ClearFinished);
    }

    #[test]
    fn adding_a_url_is_typed_and_confirmed() {
        let mut ui = Ui::new();
        let q = q2();
        assert_eq!(ui.on_key(key('a'), &q), Command::None);
        assert!(matches!(ui.mode, Mode::Adding(_)));
        for c in "http://x/f".chars() {
            ui.on_key(key(c), &q);
        }
        // Typed characters must not be interpreted as bindings: 'p' is in the URL.
        assert_eq!(
            ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &q),
            Command::Add("http://x/f".into())
        );
        assert_eq!(ui.mode, Mode::List);
    }

    #[test]
    fn backspace_and_escape_work_while_typing() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        for c in "htp".chars() {
            ui.on_key(key(c), &q);
        }
        ui.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE), &q);
        assert_eq!(ui.mode, Mode::Adding("ht".into()));
        ui.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &q);
        assert_eq!(ui.mode, Mode::List, "escape must abandon the input");
    }

    #[test]
    fn an_empty_url_is_not_added() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        assert_eq!(
            ui.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &q),
            Command::None
        );
    }

    #[test]
    fn ctrl_c_quits_even_mid_typing() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('a'), &q);
        ui.on_key(key('h'), &q);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(
            ui.on_key(ctrl_c, &q),
            Command::Quit,
            "a user must always be able to get out"
        );
    }

    #[test]
    fn help_is_dismissed_by_any_key() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('?'), &q);
        assert_eq!(ui.mode, Mode::Help);
        ui.on_key(key('x'), &q);
        assert_eq!(ui.mode, Mode::List);
    }

    #[test]
    fn selection_is_clamped_after_items_disappear() {
        let mut ui = Ui::new();
        let mut q = q2();
        ui.on_key(key('j'), &q);
        assert_eq!(ui.selected, 1);
        q.items.clear();
        ui.clamp_selection(&q);
        assert_eq!(
            ui.selected, 0,
            "a stale index would index out of bounds when drawing"
        );
    }

    #[test]
    fn render_includes_every_item_and_its_state() {
        let mut q = q2();
        q.mark_running(1);
        q.progress(1, 5 << 20, Some(10 << 20), 3.5e6);
        q.mark_running(2);
        q.fail(2, "connection reset".into());
        let ui = Ui::new();
        let out = ui.render(&q, 100, 30);
        assert!(out.contains("one.iso"), "running item missing");
        assert!(out.contains("two.zip"), "failed item missing");
        assert!(out.contains("50.0%"), "progress not shown: {out}");
        assert!(
            out.contains("connection reset"),
            "the error must be visible, not hidden"
        );
        assert!(out.contains("run "), "state tag missing");
    }

    #[test]
    fn render_survives_a_tiny_terminal() {
        let q = q2();
        let ui = Ui::new();
        // A 1x1 terminal must not panic on a subtraction or a repeat count.
        for (c, r) in [(1u16, 1u16), (10, 3), (40, 8), (300, 100)] {
            let out = ui.render(&q, c, r);
            assert!(!out.is_empty());
        }
    }

    #[test]
    fn render_shows_the_empty_state_rather_than_a_blank_screen() {
        let q = Queue::new(2);
        let ui = Ui::new();
        let out = ui.render(&q, 80, 24);
        assert!(
            out.contains("empty"),
            "an empty queue must say so and say what to press"
        );
        assert!(out.contains("'a'"));
    }

    #[test]
    fn help_screen_explains_the_resume_caveat() {
        let mut ui = Ui::new();
        let q = q2();
        ui.on_key(key('?'), &q);
        let out = ui.render(&q, 100, 40);
        assert!(
            out.contains("validator"),
            "the help must state why resume can be refused, since that surprises people"
        );
    }
}
