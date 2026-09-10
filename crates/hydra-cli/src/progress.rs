//! Live progress rendering.
//!
//! Hand-rolled ANSI rather than a progress-bar crate, for one reason: this
//! scheduler's interesting state is *per connection* 鈥?which source it is on,
//! what range it holds, how fast it is moving, and what the collapse detector
//! thinks of it. A single aggregate bar hides exactly the information that makes
//! a multi-source download debuggable.
//!
//! Everything degrades to plain lines when stdout is not a terminal, so piping
//! to a file or a CI log produces something readable rather than escape soup.

use pdl_core::Health;
use std::fmt::Write as _;
use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

const BAR_W: usize = 34;
const SPARK: [char; 8] = ['鈻?, '鈻?, '鈻?, '鈻?, '鈻?, '鈻?, '鈻?, '鈻?];

/// One connection's live state, as the renderer needs it.
pub struct ConnView {
    pub idx: usize,
    pub host: String,
    pub range: Option<(u64, u64, u64)>,
    pub rate: f64,
    pub health: Health,
}

/// Aggregate counters worth showing.
#[derive(Clone, Copy, Default)]
pub struct Counters {
    pub requests: u64,
    pub repairs: u64,
    pub reclaims: u64,
    pub wasted: u64,
    pub retries: u64,
}

pub struct Progress {
    total: Option<u64>,
    started: Instant,
    last_draw: Instant,
    last_bytes: u64,
    /// Bytes already held when this run started (resume), excluded from averages.
    baseline: u64,
    /// Recent aggregate rates for the sparkline.
    history: Vec<f64>,
    /// Smoothed transfer rate, for the number a human reads.
    ///
    /// The raw quotient over one ~80 ms redraw interval is an unusably noisy
    /// estimator: a single TCP window arriving late halves it, and one arriving
    /// early doubles it, so the figure changed several times per second while the
    /// actual throughput was steady. A number that unstable cannot be read at all
    /// 鈥?you cannot tell a slow link from a jittery one.
    ///
    /// Exponentially-weighted, which is what every download client that displays
    /// a readable rate does. The raw samples still feed `history`, so the
    /// sparkline keeps showing real variance: the smoothing is for the digits, not
    /// for the evidence.
    smoothed: Option<f64>,
    /// Lines drawn last frame, so the cursor can be rewound exactly.
    drawn_lines: usize,
    tty: bool,
    verbose: u8,
    /// Suppress the animated frame (`--no-progress`).
    quiet: bool,
    /// Suppress everything including the summary (`-q`).
    silent: bool,
    name: String,
    /// What the client is doing before bytes flow (redirect, probe, measure).
    phase: Option<String>,
    /// Bytes the concurrency probe already wrote into the real output.
    probe_bytes: u64,
    /// True when the OBJECT is going to stdout, so no human output may.
    ///
    /// `--stdout` makes stdout a data channel. Anything written there that is not
    /// payload corrupts it 鈥?measured: `hydra --no-save --stdout <url> > f`
    /// produced 34 385 bytes for a 34 041-byte object, the extra 344 being the
    /// summary line and the format hint appended to the archive. The file looked
    /// plausible and failed to decompress. Same rule `--json` follows: a machine
    /// channel carries one thing.
    stdout_is_payload: bool,
    /// `--logfile` / `--logfile-append`: human output goes HERE, not to a terminal.
    ///
    /// Every human line funnels through `emit`, so this one field redirects all of
    /// them. The animated frame is suppressed rather than written: a log file that
    /// replays cursor-up escapes and 12 fps of redraws is not a log, so a file sink
    /// gets the same append-only lines a pipe gets.
    log: Option<std::sync::Mutex<std::fs::File>>,
}

impl Progress {
    /// `no_frame` suppresses the animated display; `silent` suppresses all
    /// output including the final summary.
    pub fn new(name: &str, total: Option<u64>, verbose: u8, no_frame: bool, silent: bool) -> Self {
        Self {
            total,
            started: Instant::now(),
            last_draw: Instant::now() - Duration::from_secs(1),
            last_bytes: 0,
            smoothed: None,
            baseline: 0,
            history: Vec::new(),
            drawn_lines: 0,
            tty: std::io::stdout().is_terminal(),
            verbose,
            quiet: no_frame || silent,
            silent,
            name: name.to_string(),
            phase: None,
            probe_bytes: 0,
            stdout_is_payload: false,
            log: None,
        }
    }

    /// Send human output to `path` instead of the terminal (`-o` / `-a` logfile).
    ///
    /// `append` distinguishes `--logfile-append` from `--logfile`, which truncates.
    /// Opening eagerly and reporting the error is deliberate: a log file that
    /// cannot be created must fail the run rather than silently fall back to the
    /// terminal the user redirected away from.
    pub fn set_logfile(&mut self, path: &std::path::Path, append: bool) -> std::io::Result<()> {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(append)
            .truncate(!append)
            .open(path)?;
        self.log = Some(std::sync::Mutex::new(f));
        // A file is not a terminal: no cursor addressing, no animated frame.
        self.tty = false;
        Ok(())
    }

    /// True when human output is going to a file rather than a terminal.
    #[allow(dead_code)]
    pub fn logging_to_file(&self) -> bool {
        self.log.is_some()
    }

    /// Write one human line that is NOT part of the animated frame.
    ///
    /// The format note and the checksum verdict are decided after the transfer by
    /// the caller rather than by this type, but they are still human output and
    /// belong wherever the rest of it went. They printed straight to the terminal,
    /// so `--logfile` captured the summary and left these two beside it.
    ///
    /// `to_stderr` marks a line that must never touch a stdout carrying the
    /// object; it is ignored once a log file is in play, since neither stream is
    /// the destination then.
    pub fn note(&self, msg: &str, to_stderr: bool) {
        if self.silent {
            return;
        }
        if self.log.is_some() {
            self.emit(msg);
        } else if to_stderr || self.stdout_is_payload {
            eprintln!("{msg}");
        } else {
            println!("{msg}");
        }
    }

    /// Write one human line to wherever human output belongs.
    ///
    /// The three destinations are not interchangeable and the choice is made here
    /// once rather than at each call site: a log file when one was named, stderr
    /// when stdout is carrying the object (writing there would corrupt the
    /// payload), stdout otherwise.
    fn emit(&self, s: &str) {
        use std::io::Write as _;
        if let Some(f) = &self.log {
            if let Ok(mut g) = f.lock() {
                let _ = writeln!(g, "{s}");
                let _ = g.flush();
            }
        } else if self.stdout_is_payload {
            eprintln!("{s}");
        } else {
            println!("{s}");
        }
    }

    /// Reserve stdout for the object's bytes, sending all human output to stderr.
    ///
    /// Called for `--stdout`. Without it the summary line and the format hint are
    /// appended to the payload on the same stream, silently corrupting a pipe.
    pub fn reserve_stdout_for_payload(&mut self) {
        self.stdout_is_payload = true;
        // The animated frame would land in the payload too, and a redraw uses
        // cursor movement that makes no sense interleaved with data.
        self.quiet = true;
        self.tty = false;
    }

    /// True when the object's bytes own stdout.
    // Read only by tests since the destination choice moved into `note`/`emit`,
    // where all three sinks are decided in one place; kept because it is the
    // predicate those tests assert the reservation through.
    #[allow(dead_code)]
    pub fn stdout_reserved(&self) -> bool {
        self.stdout_is_payload
    }

    /// Force the animated path on, for capturing a frame in a test or docs.
    pub fn force_tty(&mut self) {
        self.tty = true;
    }

    /// Declare the object's length once it is known, for a transfer whose size
    /// was not available when this instance was built.
    ///
    /// `total` decides the bar, the percentage, and the ETA all three: with
    /// `None` they render `鈹�鈹�鈹�鈹�鈹�`, `?`, and `?`, which is the honest display for
    /// a chunked HTTP response of genuinely unknown extent. It is the wrong
    /// display for FTP, where `SIZE` answered before the first byte moved. The
    /// HTTP path gets the size onto the renderer by building a second `Progress`
    /// once the probe returns; FTP is handed one instance for the whole fetch, so
    /// it needs a way to tell that instance what it learned. Setting the field
    /// rather than reconstructing is also the safer of the two: a second
    /// instance has to re-apply the logfile and the stdout reservation, and a
    /// setup-phase instance missing that reservation is what once prepended 71
    /// bytes to a piped archive.
    pub fn set_total(&mut self, bytes: u64) {
        self.total = Some(bytes);
    }

    /// Declare bytes that were already on disk before this run (a resume), so
    /// average-rate and ETA arithmetic describes this run rather than history.
    pub fn set_baseline(&mut self, bytes: u64) {
        self.baseline = bytes;
        self.last_bytes = bytes;
    }

    /// Redraw. Rate-limited to ~12 fps: redrawing per arrival makes the terminal
    /// the bottleneck on a fast transfer.
    pub fn draw(&mut self, done: u64, conns: &[ConnView], c: Counters) {
        if self.quiet || self.silent {
            return;
        }
        let dt = self.last_draw.elapsed();
        if dt < Duration::from_millis(80) {
            return;
        }
        // A near-zero interval makes the quotient meaningless: the first frame of
        // a resumed transfer credits every already-held byte to a few
        // milliseconds and prints an absurd rate. Require a real interval.
        let inst = if dt.as_secs_f64() >= 0.05 && done >= self.last_bytes {
            (done - self.last_bytes) as f64 / dt.as_secs_f64()
        } else {
            self.history.last().copied().unwrap_or(0.0)
        };
        self.last_draw = Instant::now();
        self.last_bytes = done;
        self.history.push(inst);

        // Time-based EWMA: the weight depends on how much wall clock the sample
        // covers, not on how many frames were drawn, so the smoothing has the
        // same time constant whether the terminal redraws at 12 fps or the
        // transfer stalls and a single frame covers a second. TAU is the time to
        // forget ~63% of the past 鈥?long enough to hold the digits still, short
        // enough that a genuine slowdown shows within a second.
        const TAU: f64 = 1.5;
        let a = 1.0 - (-dt.as_secs_f64() / TAU).exp();
        self.smoothed = Some(match self.smoothed {
            Some(prev) => prev + a * (inst - prev),
            // Seed with the first real sample rather than zero, so the display
            // does not spend the first second climbing out of a hole.
            None => inst,
        });
        let inst = self.smoothed.unwrap_or(inst);
        if self.history.len() > 48 {
            self.history.remove(0);
        }

        if !self.tty {
            // Plain, append-only, one line per redraw at a slower cadence.
            if self.history.len().is_multiple_of(12) {
                let pct = self
                    .total
                    .map(|t| format!("{:.1}%", 100.0 * done as f64 / t as f64))
                    .unwrap_or_else(|| "?".into());
                self.emit(&format!(
                    "{} {} {} {}/s reqs={} repairs={}",
                    self.name,
                    pct,
                    human(done),
                    human(inst as u64),
                    c.requests,
                    c.repairs
                ));
            }
            return;
        }

        let (out, lines) = self.frame(done, inst, conns, c);
        self.drawn_lines = lines;
        let mut so = std::io::stdout();
        let _ = so.write_all(out.as_bytes());
        let _ = so.flush();
    }

    /// Build one animated frame: the escape-laden string and the line count it
    /// occupies, with no state mutated and nothing written.
    ///
    /// Separated from [`draw`] so the frame's text and height can be tested
    /// and verified in unit tests without a terminal.
    fn frame(&self, done: u64, inst: f64, conns: &[ConnView], c: Counters) -> (String, usize) {
        let mut out = String::new();
        // Rewind exactly the lines drawn last frame.
        if self.drawn_lines > 0 {
            let _ = write!(out, "\x1b[{}A", self.drawn_lines);
        }
        let mut lines = 0usize;

        let elapsed = self.started.elapsed().as_secs_f64();
        // Same guard on the average, and it must exclude bytes that were already
        // on disk before this run started (a resume) or it reports a rate the
        // network never achieved.
        let moved = done.saturating_sub(self.baseline);
        let avg = if elapsed >= 0.25 {
            moved as f64 / elapsed
        } else {
            0.0
        };
        let (bar, pct_s, eta_s) = match self.total {
            Some(t) if t > 0 => {
                let frac = (done as f64 / t as f64).clamp(0.0, 1.0);
                let filled = (frac * BAR_W as f64).round() as usize;
                let bar = format!(
                    "\x1b[36m{}\x1b[0m{}",
                    "鈹?.repeat(filled),
                    "鈹�".repeat(BAR_W - filled)
                );
                let remain = t.saturating_sub(done) as f64;
                let eta = if avg > 1024.0 { remain / avg } else { f64::NAN };
                (bar, format!("{:5.1}%", 100.0 * frac), fmt_dur(eta))
            }
            _ => ("鈹�".repeat(BAR_W), "  ?  ".to_string(), "?".to_string()),
        };
        let total_s = self.total.map(human).unwrap_or_else(|| "?".into());
        let _ = writeln!(
            out,
            "\x1b[1m{}\x1b[0m  {}\x1b[K",
            trunc(&self.name, 46),
            spark(&self.history)
        );
        lines += 1;
        let _ = writeln!(
            out,
            "  {bar} {pct_s}  {}/{}  \x1b[32m{}/s\x1b[0m  avg {}/s  eta {}  {}\x1b[K",
            human(done),
            total_s,
            human(inst as u64),
            human(avg as u64),
            eta_s,
            fmt_dur(elapsed)
        );
        lines += 1;

        // Per-connection detail: the reason this renderer is hand-rolled.
        for cv in conns {
            let (tag, colour) = match cv.health {
                Health::Healthy => ("ok  ", "\x1b[32m"),
                Health::Suspect => ("slow", "\x1b[33m"),
                Health::Degraded => ("bad ", "\x1b[31m"),
                Health::Stalled => ("hung", "\x1b[35m"),
                Health::Dead => ("dead", "\x1b[90m"),
            };
            let rng = match cv.range {
                Some((lo, pos, hi)) if hi > lo => {
                    let f = ((pos - lo) as f64 / (hi - lo) as f64).clamp(0.0, 1.0);
                    let w = 10usize;
                    let k = (f * w as f64).round() as usize;
                    format!(
                        "[{}{}] {:>9}-{:<9}",
                        "鈻?.repeat(k),
                        "路".repeat(w - k),
                        lo,
                        hi
                    )
                }
                _ => format!("[{}] {:>19}", "路".repeat(10), "idle"),
            };
            let _ = writeln!(
                out,
                "   {colour}{tag}\x1b[0m #{:<2} {:<24} {rng} {:>9}/s\x1b[K",
                cv.idx,
                trunc(&cv.host, 24),
                human(cv.rate as u64)
            );
            lines += 1;
        }

        if self.verbose > 0 {
            let _ = writeln!(
                out,
                "   \x1b[90mrequests {}  repairs {}  reclaims {}  retries {}  wasted {}\x1b[0m\x1b[K",
                c.requests,
                c.repairs,
                c.reclaims,
                c.retries,
                human(c.wasted)
            );
            lines += 1;
        }

        (out, lines)
    }

    /// Final summary. **Erases** the live frame and prints one line of outcome.
    ///
    /// Erasing rather than leaving the frame behind: the bar, the per-connection
    /// rows, and the sparkline are scaffolding for a transfer that is still
    /// running 鈥?a 0%-progress bar and an `idle 0 B/s` connection row left on
    /// screen under a `鉁揱 describe a moment that has passed, and they push the
    /// one line the user actually wanted down the terminal. What remains is the
    /// result.
    pub fn finish(&mut self, done: u64, ok: bool, c: Counters, digest: Option<&str>) {
        if self.silent {
            return;
        }
        let el = self.started.elapsed().as_secs_f64();
        // Bytes THIS run moved, not bytes present. `draw` already subtracts the
        // baseline; `finish` did not, so a resumed run that transferred nothing
        // reported the whole resumed prefix as if it had just been fetched 鈥?
        // "24.4 MiB in 1m09s (362.2 KiB/s)" for a run that moved 0 bytes. The
        // number was plausible, which is why it needed the divergence with the
        // live bar to be visible at all.
        let moved = done.saturating_sub(self.baseline);
        let rate = if el > 0.0 { moved as f64 / el } else { 0.0 };
        // Uncoloured into a log file: a stored line should be greppable, and an
        // escape sequence in a file is noise the reader has to strip first.
        let mark = match (ok, self.log.is_some()) {
            (true, false) => "\x1b[32m鉁揬x1b[0m",
            (false, false) => "\x1b[31m鉁梊x1b[0m",
            (true, true) => "OK",
            (false, true) => "FAILED",
        };
        // Rewind over the frame we drew and clear to the end of the screen, so the
        // summary lands where the bar was instead of below it.
        if self.tty && self.drawn_lines > 0 {
            print!("\x1b[{}A\x1b[J", self.drawn_lines);
            let _ = std::io::Write::flush(&mut std::io::stdout());
            self.drawn_lines = 0;
        }
        // `line!` writes to stderr when stdout is carrying the object, so a pipe
        // receives payload and nothing else.
        macro_rules! line {
            ($($a:tt)*) => {
                self.emit(&format!($($a)*))
            };
        }
        // On a resumed run, say what THIS run fetched and what was already held.
        // "24.4 MiB" alone is ambiguous between "downloaded" and "now present",
        // and on the failing resume above it read as the former while meaning
        // the latter.
        let moved_str = if self.baseline > 0 {
            format!(
                "{} fetched (+{} resumed)",
                human(moved),
                human(self.baseline)
            )
        } else {
            human(done)
        };
        line!(
            "{mark} {} 鈥?{} in {} ({}/s), {} requests{}",
            self.name,
            moved_str,
            fmt_dur(el),
            human(rate as u64),
            c.requests,
            digest
                .map(|d| format!(", sha256 {}", &d[..d.len().min(16)]))
                .unwrap_or_default()
        );
        if c.wasted > 0 {
            line!(
                "  {} wasted, {} repairs, {} reclaims",
                human(c.wasted),
                c.repairs,
                c.reclaims
            );
        }
    }

    /// Verbose event line, printed above the live frame.
    ///
    /// Gated on `quiet` only, never on `no_progress`: suppressing the animated
    /// frame for a log file is a different request from suppressing the events a
    /// user explicitly asked for with `-v`.
    /// Announce what the client is doing before any bytes can flow.
    ///
    /// Setup is not instantaneous 鈥?a redirect, a probe, and a concurrency measurement
    /// each cost round trips, and on a slow path that is several seconds. Showing a
    /// silent terminal during it reads as a hang, which is what prompted this: the
    /// phase line is the difference between "it is stuck" and "it is measuring".
    /// Erase the phase line, called once bytes start flowing.
    /// Should the phase row be shown at all?
    ///
    /// Single source of truth for drawing AND clearing, so the two cannot disagree.
    fn phase_visible(&self) -> bool {
        // Never into a log file: the spinner is a carriage-return animation, so a
        // file would collect one line of overwritten spinner frames.
        !self.quiet && !self.silent && self.verbose > 0 && self.log.is_none()
    }

    pub fn end_phase(&mut self) {
        // The condition must MATCH `phase()`'s, not be stricter than it. An earlier version
        // required verbose here while `phase()` only required non-quiet, so at default
        // verbosity the row was drawn and never erased 鈥?leaving a half-line of spinner in
        // front of the next message.
        let had = self.phase.take().is_some();
        if had && self.phase_visible() {
            eprint!("\r\x1b[K");
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }
    }

    pub fn phase(&mut self, what: &str) {
        if self.silent || self.quiet {
            return;
        }
        self.phase = Some(what.to_string());
        self.draw_phase();
    }

    fn draw_phase(&mut self) {
        // One gate for drawing, and `end_phase` mirrors it exactly. Splitting the condition
        // across phase()/draw_phase()/end_phase() is what left an un-erased spinner row at
        // default verbosity: the line was drawn by a looser condition than the one that
        // cleared it. The phase row is verbose-only (the user asked for that: at default
        // verbosity the useful signal is the progress bar, and a spinner announcing internal
        // stages competes for the same terminal row).
        if !self.phase_visible() {
            return;
        }
        let Some(what) = self.phase.clone() else {
            return;
        };
        let el = self.started.elapsed().as_secs_f64();
        let spin = ["|", "/", "-", "\\"][(el * 6.0) as usize % 4];
        let held = if self.probe_bytes > 0 {
            format!("  {} kept", human(self.probe_bytes))
        } else {
            String::new()
        };
        eprint!("\r\x1b[K  {spin} {what}{held}  {el:.1}s");
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }

    pub fn event(&mut self, level: u8, msg: &str) {
        if self.silent || self.verbose < level {
            return;
        }
        if self.tty && self.drawn_lines > 0 {
            print!("\x1b[{}A\x1b[J", self.drawn_lines);
            self.drawn_lines = 0;
        }
        // Every verbose line funnels through here, so this one branch keeps all of
        // them off a stdout that is carrying the object. The setup line ("1
        // source(s), 1 connection(s) each") was reaching a piped .tar.gz through
        // this path and prepending 71 bytes to the archive.
        let el = self.started.elapsed().as_secs_f64();
        // No ANSI dimming into a log file: escape codes in a file are noise a
        // reader has to strip before they can grep it.
        if self.log.is_some() {
            self.emit(&format!("  [{el:>7.3}s] {msg}"));
        } else {
            self.emit(&format!("  \x1b[90m[{el:>7.3}s]\x1b[0m {msg}"));
        }
    }
}

/// Render one representative frame, for documentation and layout review.
///
/// Uses the real renderer rather than a mock-up, so what appears here is exactly
/// what a user sees.
pub fn demo_frame() {
    let mut p = Progress::new(
        "ubuntu-24.04.2-desktop-amd64.iso",
        Some(6_203_355_136),
        1,
        false,
        false,
    );
    p.force_tty();
    // Backdate the start so the frame shows plausible rates rather than the
    // artefacts of a zero-length interval.
    p.started = Instant::now() - Duration::from_secs(112);
    p.last_bytes = 2_915_000_000;
    let conns = vec![
        ConnView {
            idx: 0,
            host: "mirror.example.org".into(),
            range: Some((0, 1_100_000_000, 1_550_838_784)),
            rate: 11.4e6,
            health: Health::Healthy,
        },
        ConnView {
            idx: 1,
            host: "mirror.example.org".into(),
            range: Some((1_550_838_784, 2_400_000_000, 3_101_677_568)),
            rate: 10.9e6,
            health: Health::Healthy,
        },
        ConnView {
            idx: 2,
            host: "cdn.example.net".into(),
            range: Some((3_101_677_568, 3_300_000_000, 4_652_516_352)),
            rate: 1.2e6,
            health: Health::Suspect,
        },
        ConnView {
            idx: 3,
            host: "cdn.example.net".into(),
            range: None,
            rate: 0.0,
            health: Health::Stalled,
        },
    ];
    p.last_draw = Instant::now() - Duration::from_secs(1);
    p.draw(
        2_950_000_000,
        &conns,
        Counters {
            requests: 23,
            repairs: 6,
            reclaims: 1,
            wasted: 0,
            retries: 2,
        },
    );
    println!();
}

/// Bytes as a human-readable string, aligned for column output.
pub fn human(n: u64) -> String {
    // Ladder runs to EiB so no input can widen the column: u64::MAX in TiB is
    // "16777216.0 TiB", which is 15 characters and breaks the aligned
    // per-connection layout. Absurd sizes are not realistic, but a renderer that
    // corrupts its own table on unexpected input is a bug regardless.
    const U: [&str; 7] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else if v < 10.0 {
        format!("{v:.2} {}", U[i])
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn fmt_dur(s: f64) -> String {
    if !s.is_finite() {
        return "?".into();
    }
    let s = s.max(0.0);
    if s < 60.0 {
        format!("{s:.1}s")
    } else if s < 3600.0 {
        format!("{}m{:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    } else {
        format!(
            "{}h{:02}m",
            (s / 3600.0) as u64,
            ((s % 3600.0) / 60.0) as u64
        )
    }
}

fn spark(h: &[f64]) -> String {
    if h.len() < 2 {
        return String::new();
    }
    let mx = h.iter().cloned().fold(0.0f64, f64::max);
    if mx <= 0.0 {
        return String::new();
    }
    h.iter()
        .map(|v| {
            let k = ((v / mx) * (SPARK.len() - 1) as f64).round() as usize;
            SPARK[k.min(SPARK.len() - 1)]
        })
        .collect()
}

fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let keep: String = s.chars().take(n.saturating_sub(1)).collect();
        format!("{keep}鈥?)
    }
}

/// A completed file and what it was determined to be.
///
/// A struct rather than a tuple because the first version summed the wrong slot for the
/// total-bytes line: positional access through eight fields is not reviewable.
struct Finished {
    name: String,
    size: u64,
    secs: f64,
    ok: bool,
    ext: Option<String>,
    label: Option<String>,
    desc: Option<String>,
}

/// Aggregate progress for several independent files.
///
/// One renderer owns the screen: per-file `Progress` instances would each try to redraw
/// the same rows and the frames would interleave into noise. Each file gets one line 鈥?
/// name, bar, fraction, rate 鈥?plus the format detail that a downloader can actually
/// determine while the transfer runs: the extension it was served under, the media type
/// the server declared, and (under `-v`) the human description of what that format is.
pub struct Multi {
    names: Vec<String>,
    /// Latest tick per file id.
    live: std::collections::HashMap<u64, crate::download::Tick>,
    /// Finished files, in completion order, with what they turned out to be.
    finished: Vec<Finished>,
    /// Ids that have begun transferring. Everything else with no `Finished` row is
    /// still queued.
    begun: std::collections::HashSet<u64>,
    verbose: u8,
    quiet: bool,
    tty: bool,
    started: Instant,
    last_draw: Instant,
    drawn: usize,
}

impl Multi {
    pub fn new(names: Vec<String>, verbose: u8, quiet: bool) -> Self {
        Self {
            names,
            live: std::collections::HashMap::new(),
            finished: Vec::new(),
            begun: std::collections::HashSet::new(),
            verbose,
            quiet,
            tty: std::io::stderr().is_terminal(),
            started: Instant::now(),
            last_draw: Instant::now() - Duration::from_secs(1),
            drawn: 0,
        }
    }

    /// Mark a file as started, so a queued file is distinguishable from a stalled one.
    ///
    /// Without this, `--mode queue` showed only the running row and the other files were
    /// invisible 鈥?indistinguishable from not having been accepted at all.
    pub fn start(&mut self, id: u64) {
        self.begun.insert(id);
        self.draw_force();
    }

    pub fn tick(&mut self, t: crate::download::Tick) {
        self.live.insert(t.id, t);
        self.draw();
    }

    /// Record a finished file and what it was determined to be.
    pub fn done(&mut self, id: u64, o: &crate::download::Outcome) {
        self.live.remove(&id);
        let name = self
            .names
            .get(id as usize)
            .cloned()
            .unwrap_or_else(|| o.output.clone());
        // The extension is taken from the SAVED name, and the format from the payload, so
        // a disagreement between them is visible rather than hidden.
        let ext = std::path::Path::new(&o.output)
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()));
        self.finished.push(Finished {
            name,
            size: o.size,
            secs: o.transfer_s,
            ok: o.ok,
            ext,
            label: o.format_label.clone(),
            desc: o.format_description.clone(),
        });
        self.draw_force();
    }

    fn draw(&mut self) {
        if self.last_draw.elapsed() < Duration::from_millis(120) {
            return;
        }
        self.last_draw = Instant::now();
        self.draw_force();
    }

    fn draw_force(&mut self) {
        if self.quiet {
            return;
        }
        // Without a terminal there is no cursor to rewind, so redrawing every 120 ms
        // would emit the same block dozens of times into a log. Print nothing during the
        // transfer and let `finish()` report the result.
        if !self.tty {
            return;
        }
        let (out, lines) = self.frame();
        eprint!("{out}");
        let _ = std::io::Write::flush(&mut std::io::stderr());
        self.drawn = lines;
    }

    /// Build the frame text and its line count.
    ///
    /// Separated from drawing so the layout is testable without a terminal: the sandbox
    /// this was developed in has no pty, and asserting on escape sequences from a real
    /// screen is not a test anyone can debug.
    pub fn frame(&self) -> (String, usize) {
        let mut out = String::new();
        if self.tty && self.drawn > 0 {
            // Rewind exactly the number of lines drawn last frame.
            out.push_str(&format!("\x1b[{}A\r", self.drawn));
        }
        let mut lines = 0usize;

        let total_rate: f64 = self.live.values().map(|t| t.rate).sum();
        let done_n = self.finished.len();
        let failed_n = self.finished.iter().filter(|f| !f.ok).count();
        let queued_n = self.names.len().saturating_sub(done_n + self.live.len());
        out.push_str(&format!(
            "\x1b[K \x1b[1m{} file(s)\x1b[0m  \x1b[36m{} active\x1b[0m  \x1b[90m{} queued\x1b[0m  \x1b[32m{} done\x1b[0m{}  {}/s  {:.0}s\r\n",
            self.names.len(),
            self.live.len(),
            queued_n,
            done_n - failed_n,
            if failed_n > 0 {
                format!("  \x1b[31m{failed_n} failed\x1b[0m")
            } else {
                String::new()
            },
            human(total_rate as u64),
            self.started.elapsed().as_secs_f64()
        ));
        lines += 1;

        // Finished first, so completed work does not jump around as active rows change.
        for f in &self.finished {
            let (name, size, secs, ext, label, desc) =
                (&f.name, f.size, f.secs, &f.ext, &f.label, &f.desc);
            // A word alongside the glyph: a tick and a cross are easy to misread at a
            // glance, and colour alone conveys nothing to a colour-blind reader. The
            // column width matches "鈻?active   " and "路 queued   " so names align.
            let mark = if f.ok {
                "\x1b[32m鉁?done     \x1b[0m"
            } else {
                "\x1b[31m鉁?failed   \x1b[0m"
            };
            let what = match (ext.as_deref(), label.as_deref()) {
                (Some(e), Some(l)) => format!("{e}  {l}"),
                (None, Some(l)) => l.to_string(),
                (Some(e), None) => e.to_string(),
                (None, None) => String::new(),
            };
            out.push_str(&format!(
                "\x1b[K {mark}{:<26.26} {:>10}  {:>6.1}s  \x1b[90m{}\x1b[0m\r\n",
                name,
                human(size),
                secs,
                what
            ));
            lines += 1;
            // The description is a sentence, so it only earns a line when asked for.
            if self.verbose > 0 {
                if let Some(d) = desc {
                    out.push_str(&format!("\x1b[K     \x1b[90m{d}\x1b[0m\r\n"));
                    lines += 1;
                }
            }
        }

        let mut ids: Vec<u64> = self.live.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let t = &self.live[&id];
            let name = self
                .names
                .get(id as usize)
                .cloned()
                .unwrap_or_else(|| format!("#{id}"));
            let (bar, pct, amount) = match t.size {
                Some(sz) if sz > 0 => {
                    let f = (t.done as f64 / sz as f64).clamp(0.0, 1.0);
                    let w = 18usize;
                    let fill = (f * w as f64).round() as usize;
                    (
                        format!("{}{}", "鈹?.repeat(fill.min(w)), "鈹�".repeat(w - fill.min(w))),
                        format!("{:>5.1}%", f * 100.0),
                        format!("{} / {}", human(t.done), human(sz)),
                    )
                }
                // An unknown total is common enough (chunked, no Content-Length) that it
                // needs a real rendering rather than a bar stuck at zero.
                _ => (
                    "鈹�".repeat(18),
                    "    ?".into(),
                    format!("{} / ?", human(t.done)),
                ),
            };
            out.push_str(&format!(
                "\x1b[K \x1b[36m鈻?active   \x1b[0m{:<26.26} \x1b[36m{bar}\x1b[0m {pct}  {:<20}  {:>9}/s  \x1b[90m{} conn\x1b[0m\r\n",
                name,
                amount,
                human(t.rate as u64),
                t.conns.len()
            ));
            lines += 1;
            if self.verbose > 0 {
                for c in &t.conns {
                    out.push_str(&format!(
                        "\x1b[K       \x1b[90m{:<24.24} {:>12}-{:<12} {:>9}/s  {}\x1b[0m\r\n",
                        c.host,
                        c.lo,
                        c.hi,
                        human(c.rate as u64),
                        c.health
                    ));
                    lines += 1;
                }
            }
        }
        // Queued rows: every file that has neither finished nor started. Showing them is
        // the difference between "one download exists" and "one of three is running" 鈥?
        // in --mode queue the others were previously invisible.
        let done_names: std::collections::HashSet<&str> =
            self.finished.iter().map(|f| f.name.as_str()).collect();
        for (i, name) in self.names.iter().enumerate() {
            let id = i as u64;
            if self.live.contains_key(&id) || done_names.contains(name.as_str()) {
                continue;
            }
            out.push_str(&format!(
                "\x1b[K \x1b[90m路 queued   {name:<26.26}\x1b[0m\r\n"
            ));
            lines += 1;
        }

        (out, lines)
    }

    /// Final summary, printed once.
    pub fn finish(&mut self) {
        if self.quiet {
            return;
        }
        self.draw_force();
        if !self.tty {
            // The per-file lines are the whole record in a pipe or a log.
            for f in &self.finished {
                let mark = if f.ok { "done  " } else { "failed" };
                let what = match (f.ext.as_deref(), f.label.as_deref()) {
                    (Some(e), Some(l)) => format!("  {e}  {l}"),
                    (None, Some(l)) => format!("  {l}"),
                    (Some(e), None) => format!("  {e}"),
                    (None, None) => String::new(),
                };
                eprintln!(
                    " {mark} {:<30} {:>10}  {:>6.1}s{}",
                    f.name,
                    human(f.size),
                    f.secs,
                    what
                );
                if self.verbose > 0 {
                    if let Some(d) = &f.desc {
                        eprintln!("      {d}");
                    }
                }
            }
        }
        let ok = self.finished.iter().filter(|f| f.ok).count();
        // Sum SIZES of successful files. A named struct would make this unmistakable;
        // the tuple slots are why the first version summed the wrong field.
        let bytes: u64 = self.finished.iter().filter(|f| f.ok).map(|f| f.size).sum();
        let el = self.started.elapsed().as_secs_f64();
        eprintln!(
            " \x1b[1m{ok}/{}\x1b[0m file(s), {} in {:.1}s ({}/s aggregate)",
            self.names.len(),
            human(bytes),
            el,
            human(if el > 0.0 {
                (bytes as f64 / el) as u64
            } else {
                0
            })
        );
    }
}

/// Print a representative multi-file frame, for documentation and for looking at the
/// layout without a terminal.
pub fn demo_multi() {
    let mut m = Multi::new(
        vec![
            "100MB.bin".into(),
            "meilisearch-linux-aarch64".into(),
            "sample.pdf".into(),
            "Rcpp_1.0.9.tar.gz".into(),
        ],
        0,
        false,
    );
    m.finished.push(Finished {
        name: "Rcpp_1.0.9.tar.gz".into(),
        size: 2_957_812,
        secs: 2.4,
        ok: true,
        ext: Some(".gz".into()),
        label: Some("gzip-compressed tar".into()),
        desc: None,
    });
    m.live.insert(
        0,
        crate::download::Tick {
            id: 0,
            done: 9_785_344,
            size: Some(104_857_600),
            rate: 8.93e6,
            requests: 1,
            repairs: 0,
            conns: Vec::new(),
        },
    );
    let (f, _) = m.frame();
    print!("{}", f.replace("\x1b[K", ""));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_units_are_readable_and_bounded() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1024), "1.00 KiB");
        assert_eq!(human(1_048_576), "1.00 MiB");
        assert_eq!(human(12_801_696), "12.2 MiB");
        assert_eq!(human(1 << 40), "1.00 TiB");
        // Never longer than a column budget, whatever the input.
        for n in [0u64, 1, 1023, 1 << 20, u64::MAX] {
            assert!(human(n).len() <= 10, "{} too wide: {}", n, human(n));
        }
    }

    #[test]
    fn durations_switch_units_sensibly() {
        assert_eq!(fmt_dur(4.25), "4.2s");
        assert_eq!(fmt_dur(90.0), "1m30s");
        assert_eq!(fmt_dur(3725.0), "1h02m");
        assert_eq!(fmt_dur(f64::NAN), "?");
        assert_eq!(
            fmt_dur(-1.0),
            "0.0s",
            "a negative interval must not print a sign"
        );
    }

    #[test]
    fn sparkline_is_empty_until_there_is_something_to_show() {
        assert_eq!(spark(&[]), "");
        assert_eq!(spark(&[5.0]), "");
        assert_eq!(spark(&[0.0, 0.0]), "", "an all-zero history has no shape");
        let s = spark(&[1.0, 2.0, 4.0, 8.0]);
        assert_eq!(s.chars().count(), 4);
        assert_eq!(
            s.chars().last().unwrap(),
            '鈻?,
            "the maximum must render full height"
        );
    }

    #[test]
    fn truncation_respects_character_boundaries() {
        assert_eq!(trunc("short", 10), "short");
        assert_eq!(trunc("abcdefghij", 5), "abcd鈥?);
        // Multi-byte input must not be sliced mid-character.
        let s = trunc("鏃ユ湰瑾炪伄銉曘偂銈ゃ儷鍚嶃仹銇?, 5);
        assert_eq!(s.chars().count(), 5);
    }

    #[test]
    fn frame_shows_every_connection_with_its_health() {
        // Rendering is a deliverable: a per-connection view that silently drops a
        // connection, or mislabels a collapsed one as fine, is worse than none.
        let mut p = Progress::new("payload.iso", Some(100 << 20), 1, false, false);
        p.force_tty();
        let conns = vec![
            ConnView {
                idx: 0,
                host: "a.example.org".into(),
                range: Some((0, 5 << 20, 25 << 20)),
                rate: 4.2e6,
                health: Health::Healthy,
            },
            ConnView {
                idx: 1,
                host: "b.example.org".into(),
                range: Some((25 << 20, 26 << 20, 50 << 20)),
                rate: 0.3e6,
                health: Health::Suspect,
            },
            ConnView {
                idx: 2,
                host: "c.example.org".into(),
                range: None,
                rate: 0.0,
                health: Health::Stalled,
            },
        ];
        p.last_draw = Instant::now() - Duration::from_secs(1);
        p.draw(
            30 << 20,
            &conns,
            Counters {
                requests: 7,
                repairs: 2,
                reclaims: 1,
                wasted: 0,
                retries: 0,
            },
        );
        // 1 title + 1 aggregate + 3 connections + 1 counters line (verbose >= 1)
        assert_eq!(p.drawn_lines, 6, "every connection must get a line");
    }

    /// A known size must reach the bar, however late it was learned.
    ///
    /// Regression test for the FTP display. The FTP path is handed the
    /// setup-phase `Progress`, built before any request when no size was known,
    /// and nothing ever told it what `SIZE` returned 鈥?so a transfer whose exact
    /// length was known before the first byte moved rendered an empty rule, a
    /// `?` percentage, a `?` total and a `?` ETA for its entire duration
    /// (measured on unknown-size stream: `334.9 KiB/?`).
    /// Asserted on the frame's text, because the previous suite could only count
    /// lines and a `?` where a percentage belongs occupies exactly one line.
    #[test]
    fn a_size_learned_after_construction_still_draws_a_bar() {
        let mut p = Progress::new("archive-latest.tar.gz", None, 0, false, false);
        p.force_tty();
        let (unknown, _) = p.frame(1 << 20, 4.0e5, &[], Counters::default());
        assert!(
            unknown.contains('?'),
            "an unknown total must still render honestly: {unknown:?}"
        );
        assert!(
            !unknown.contains('鈹?),
            "nothing may be claimed filled while the extent is unknown"
        );

        p.set_total(5 << 20);
        p.started = Instant::now() - Duration::from_secs(4);
        let (known, _) = p.frame(1 << 20, 4.0e5, &[], Counters::default());
        assert!(
            known.contains("20.0%"),
            "the percentage must follow from the declared total: {known:?}"
        );
        assert!(
            known.contains('鈹?),
            "a fifth of the bar must be filled, not left as a bare rule"
        );
        assert!(
            known.contains("1.00 MiB/5.00 MiB"),
            "the total must be printed, not left as ?: {known:?}"
        );
        assert!(
            !known.contains("eta ?"),
            "with a total and a rate the ETA is computable: {known:?}"
        );
    }

    /// The per-connection row must track the cursor, not sit full.
    ///
    /// Regression test for the FTP display: its `ConnView` was built as
    /// `(start, size, done)` against the renderer's `(lo, pos, hi)`, so the
    /// fraction it computed was `(size - start) / (done - start)` 鈥?above 1 for
    /// the whole transfer, clamped to a permanently full `[鈻柂鈻柂鈻柂鈻柂鈻柂]` from the
    /// first frame, with bytes-so-far printed where the object's length belongs.
    /// A full row under a 0% aggregate bar is a contradiction the display should
    /// never be able to show, so the two are asserted together.
    #[test]
    fn a_connection_row_tracks_the_cursor_rather_than_filling() {
        let total = 342_954u64;
        let mut p = Progress::new("archive-latest.tar.gz", Some(total), 0, false, false);
        p.force_tty();
        let at = |done: u64| {
            let views = vec![ConnView {
                idx: 0,
                host: "ftp.gnu.org".into(),
                range: Some((0, done, total)),
                rate: 3.0e5,
                health: Health::Healthy,
            }];
            p.frame(done, 3.0e5, &views, Counters::default()).0
        };

        let early = at(total / 10);
        assert!(
            !early.contains("鈻柂鈻柂鈻柂鈻柂鈻柂"),
            "a tenth done must not render a full row: {early:?}"
        );
        assert!(
            early.contains('鈻?),
            "a tenth done must render some of the row: {early:?}"
        );
        // The extent, not the cursor, belongs in the range label 鈥?the swapped
        // tuple printed `0-171477` at the halfway mark of a 342 954-byte object.
        assert!(
            early.contains(&total.to_string()),
            "the row must be labelled with the object's extent: {early:?}"
        );

        let mid = at(total / 2);
        assert!(
            mid.contains("鈻柂鈻柂鈻仿仿仿仿?),
            "halfway must fill half the row: {mid:?}"
        );
        assert!(
            at(total).contains("鈻柂鈻柂鈻柂鈻柂鈻柂"),
            "the row may only be full when the transfer is"
        );
    }

    /// A resumed transfer must not report a rate the network never achieved.
    #[test]
    fn resume_baseline_excludes_bytes_already_on_disk() {
        let mut p = Progress::new("f", Some(100 << 20), 0, false, false);
        p.force_tty();
        p.set_baseline(90 << 20); // 90 MiB already held from a previous run
        p.started = Instant::now() - Duration::from_secs(2);
        p.last_draw = Instant::now() - Duration::from_secs(1);
        // 2 MiB actually moved in 2 s => ~1 MiB/s, not 46 MiB/s.
        p.draw(92 << 20, &[], Counters::default());
        let moved = (92u64 << 20) - (90u64 << 20);
        let avg = moved as f64 / 2.0;
        assert!(
            avg < 2.0 * 1_048_576.0,
            "average must describe this run, not the resumed history"
        );
    }

    #[test]
    fn a_sub_threshold_interval_is_not_drawn() {
        let mut p = Progress::new("f", Some(1 << 30), 0, false, false);
        p.force_tty();
        // First draw is allowed (the constructor backdates last_draw).
        p.draw(1 << 20, &[], Counters::default());
        assert!(p.drawn_lines > 0);
        let lines = p.drawn_lines;
        // An immediate second draw must be suppressed: redrawing per arrival
        // makes the terminal the bottleneck on a fast transfer.
        p.draw(2 << 20, &[], Counters::default());
        assert_eq!(p.drawn_lines, lines, "frames must be rate-limited");
    }

    ///  must actually redirect human output into the named file.
    ///
    /// Regression test:  and  existed only as CLI struct
    /// fields and compat-layer entries. No code opened either path, so the file
    /// was never created and the summary went to the terminal exactly as if the
    /// flag had not been passed.
    #[test]
    fn a_logfile_receives_the_human_output_and_no_escape_codes() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("playdl_logfile_test_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);

        let mut p = Progress::new("obj.bin", Some(1000), 1, true, false);
        p.set_logfile(&path, false).expect("the log file must open");
        assert!(p.logging_to_file());
        p.event(1, "probing the source");
        p.note("  a format note", false);
        p.finish(1000, true, Counters::default(), None);

        let body = std::fs::read_to_string(&path).expect("the log file must exist");
        assert!(
            body.contains("probing the source"),
            "verbose events belong in the log, got: {body:?}"
        );
        assert!(
            body.contains("a format note"),
            "post-transfer notes belong in the log too, got: {body:?}"
        );
        assert!(
            body.contains("obj.bin"),
            "the summary line belongs in the log, got: {body:?}"
        );
        assert!(
            !body.contains('\x1b'),
            "a log file must be greppable: no ANSI escapes, got: {body:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    ///  must not truncate what a previous run wrote.
    #[test]
    fn logfile_append_keeps_the_previous_runs_lines() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("playdl_logappend_test_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, "earlier run\n").unwrap();

        let mut p = Progress::new("second.bin", Some(10), 0, true, false);
        p.set_logfile(&path, true).expect("append must open");
        p.finish(10, true, Counters::default(), None);

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("earlier run"), "append must not truncate");
        assert!(
            body.contains("second.bin"),
            "the new run must be recorded too"
        );

        // The truncating form is the other half of the contract.
        let mut q = Progress::new("third.bin", Some(10), 0, true, false);
        q.set_logfile(&path, false).expect("truncate must open");
        q.finish(10, true, Counters::default(), None);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            !body.contains("earlier run"),
            "--logfile truncates, got: {body:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn quiet_mode_draws_nothing() {
        let mut p0 = Progress::new("x", Some(100), 0, false, false);
        assert!(!p0.stdout_reserved(), "stdout is ours by default");
        p0.reserve_stdout_for_payload();
        assert!(p0.stdout_reserved());
        // Reserving must also silence the animated frame: a redraw uses cursor
        // movement, which is meaningless interleaved with payload bytes.
        p0.draw(
            50,
            &[ConnView {
                idx: 0,
                host: "h".into(),
                range: Some((0, 50, 100)),
                rate: 1.0,
                health: Health::Healthy,
            }],
            Counters::default(),
        );

        let mut p = Progress::new("x", Some(100), 0, true, true);
        p.draw(50, &[], Counters::default());
        assert_eq!(p.drawn_lines, 0);
    }

    #[test]
    fn every_file_appears_with_its_state_label() {
        // The bug: in --mode queue only the running row was drawn, so the other files
        // were invisible and it looked as though one download existed.
        let mut m = Multi::new(
            vec!["big.bin".into(), "second.pdf".into(), "third.mp3".into()],
            0,
            false,
        );
        m.tick(crate::download::Tick {
            id: 0,
            done: 9 << 20,
            size: Some(100 << 20),
            rate: 8.5e6,
            requests: 1,
            repairs: 0,
            conns: Vec::new(),
        });
        let (f, lines) = m.frame();
        assert!(
            f.contains("active"),
            "the transferring file must be labelled active"
        );
        assert!(
            f.contains("second.pdf") && f.contains("third.mp3"),
            "files not yet started must still be listed: {f}"
        );
        assert_eq!(
            f.matches("queued").count(),
            3,
            "two queued rows plus the header count: {f}"
        );
        assert_eq!(lines, 4, "header plus one row per file");
    }

    #[test]
    fn state_labels_are_words_not_only_colour() {
        // Colour alone fails for a colour-blind reader and vanishes in a pipe, so each
        // state carries a word.
        let mut m = Multi::new(vec!["a.bin".into(), "b.bin".into()], 0, false);
        m.done(
            0,
            &crate::download::Outcome {
                ok: true,
                size: 1024,
                output: "a.bin".into(),
                ..crate::download::stub_outcome()
            },
        );
        let (f, _) = m.frame();
        assert!(f.contains("done"), "a completed file says so: {f}");
        assert!(f.contains("queued"), "the untouched file says so: {f}");
        for word in ["active", "queued", "done"] {
            assert!(
                f.contains(word) || word == "active",
                "state vocabulary must be present in text form"
            );
        }
    }

    #[test]
    fn a_failed_file_is_counted_separately_from_a_completed_one() {
        let mut m = Multi::new(vec!["a.bin".into()], 0, false);
        m.done(
            0,
            &crate::download::Outcome {
                ok: false,
                size: 0,
                output: "a.bin".into(),
                ..crate::download::stub_outcome()
            },
        );
        let (f, _) = m.frame();
        assert!(
            f.contains("failed"),
            "a failure must be named, not implied: {f}"
        );
        assert!(
            !f.contains("1 done") || f.contains("0 done"),
            "a failed file must not be counted as done: {f}"
        );
    }
    /// The displayed rate must be steady under a jittery byte stream.
    ///
    /// Reported directly: on a real 8-connection transfer the aggregate figure
    /// changed several times a second while actual throughput was steady, because
    /// it was the raw quotient over one ~80 ms redraw. A number that unstable
    /// cannot be read. The sparkline still gets the raw samples; only the digits
    /// are smoothed.
    #[test]
    fn the_displayed_rate_is_smoothed_but_still_tracks_a_real_change() {
        let mut p = Progress::new("x", Some(100 << 20), 0, false, false);
        p.force_tty();

        // A steady 1 MiB/s carried by wildly uneven arrivals.
        let mut done = 0u64;
        let mut seen: Vec<f64> = Vec::new();
        for i in 0..40 {
            let bump = if i % 2 == 0 { 1600 << 10 } else { 400 << 10 };
            done += bump;
            p.last_draw = Instant::now() - Duration::from_secs_f64(1.0);
            p.draw(done, &[], Counters::default());
            if let Some(s) = p.smoothed {
                seen.push(s);
            }
        }
        assert!(
            seen.len() >= 20,
            "draw() produced no samples: {}",
            seen.len()
        );
        let tail = &seen[seen.len() - 10..];
        let lo = tail.iter().cloned().fold(f64::MAX, f64::min);
        let hi = tail.iter().cloned().fold(0.0f64, f64::max);
        // Raw samples swing 4x (400 KiB vs 1600 KiB per second). Smoothed must not.
        assert!(
            hi / lo < 1.6,
            "smoothed rate still swings {:.2}x ({lo:.0}..{hi:.0} B/s)",
            hi / lo
        );

        // ...but a genuine collapse must still show up promptly.
        for _ in 0..4 {
            p.last_draw = Instant::now() - Duration::from_secs_f64(1.0);
            p.draw(done, &[], Counters::default()); // no new bytes at all
        }
        let after = p.smoothed.unwrap();
        assert!(
            after < lo * 0.35,
            "a stall must be visible within a few seconds, still reading {after:.0} B/s"
        );
    }
}
