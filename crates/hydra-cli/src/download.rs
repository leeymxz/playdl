//! The download engine the CLI drives: probe, plan, transfer, verify.
//!
//! This is where the theory meets a user's file. The scheduler decides *which
//! bytes go where*; this module decides everything around that â€?how many
//! connections politeness permits, whether a partial file can be resumed, what
//! the sidecar records, and whether the delivered bytes are the bytes asked for.

use crate::progress::{ConnView, Counters, Progress};
use crate::url::{proxy_from_env, Sidecar, Url};
use pdl_core::{detect_format, Category, Scheduler, Source};
use pdl_net::polite::{Politeness, RateLimiter};
use pdl_net::{fetch_range_retry, probe_resilient, SparseSink, Target, TlsCapableConnector};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

/// A requested byte range, kept symbolic until the object size is known.
///
/// `Suffix` cannot be resolved at parse time â€?"the last 512 bytes" depends on
/// the size, which only the probe reveals â€?so it stays an explicit variant
/// rather than a sentinel value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeSpec {
    /// `lo-hi` inclusive, as HTTP spells ranges.
    Closed(u64, u64),
    /// `lo-`: from an offset to the end.
    From(u64),
    /// `-n`: the final n bytes.
    Suffix(u64),
}

impl RangeSpec {
    /// Resolve against a known object size into a half-open `[lo, hi)`.
    pub fn resolve(self, size: u64) -> Option<(u64, u64)> {
        let (lo, hi) = match self {
            RangeSpec::Closed(lo, hi) => (lo.min(size), (hi.saturating_add(1)).min(size)),
            RangeSpec::From(lo) => (lo.min(size), size),
            RangeSpec::Suffix(n) => (size.saturating_sub(n), size),
        };
        if hi > lo {
            Some((lo, hi))
        } else {
            None
        }
    }
}

/// Everything the engine needs for one transfer.
/// Live progress from a running transfer.
///
/// The queue manager needs this because a transfer is opaque otherwise: `run()` only
/// returns when it is finished, so a UI driving several jobs had nothing to show until
/// each one completed â€?every row sat at `?` for the whole download. Per-connection
/// detail rides along because that is the state that makes a multi-source transfer
/// debuggable (which mirror is slow, which range is stuck).
#[derive(Clone, Debug)]
pub struct Tick {
    pub id: u64,
    pub done: u64,
    pub size: Option<u64>,
    pub rate: f64,
    pub requests: u64,
    pub repairs: u64,
    pub conns: Vec<ConnLine>,
}

/// One connection's state, flattened for display.
#[derive(Clone, Debug)]
pub struct ConnLine {
    pub host: String,
    pub lo: u64,
    pub hi: u64,
    pub pos: u64,
    pub rate: f64,
    pub health: String,
}

#[derive(Clone)]
pub struct Job {
    /// Where to send live progress, and the id to tag it with.
    pub ticks: Option<(u64, tokio::sync::mpsc::UnboundedSender<Tick>)>,
    pub urls: Vec<String>,
    pub output: Option<PathBuf>,
    pub conns: Option<usize>,
    pub resume: bool,
    pub limit_rate: u64,
    /// Redirect hops permitted before giving up. `0` refuses to follow any.
    pub max_redirs: u32,
    /// `-4` / `-6`: restrict every connection to one IP version.
    pub ip_family: pdl_net::IpFamily,
    /// `--show-error`: print failure reasons to stderr even under `-q`.
    pub show_error: bool,
    /// `--logfile` (truncate) or `--logfile-append`: human output goes to this
    /// file instead of the terminal. `bool` is the append flag.
    pub logfile: Option<(PathBuf, bool)>,
    pub tries: u32,
    pub timeout_s: f64,
    pub checksum: Option<String>,
    pub headers: Vec<String>,
    pub user_agent: String,
    pub verbose: u8,
    pub quiet: bool,
    pub no_progress: bool,
    pub polite: Politeness,
    /// Run the marginal-goodput search even though `conns` names a number,
    /// treating that number as a ceiling rather than a target (`--adaptive`).
    ///
    /// Separate from `probe` below because the two answer different questions.
    /// `probe` is "no number was given, go and find one"; this is "a number was
    /// given, but check how much of it is useful". Conflating them would either
    /// make `-x N` silently measure â€?breaking a flag whose whole purpose is to
    /// pin a value for a reproduction or a comparison against another client â€?or
    /// leave `--adaptive` unable to express a ceiling.
    pub adaptive: bool,
    /// Measure the useful connection count when no `-x` was given.
    ///
    /// True by default; `--no-probe` sets it false, in which case a run with no
    /// `-x` takes ONE connection rather than guessing a multi-connection default.
    /// Guessing is what produced transfers slower than a single stream on a
    /// saturated access link.
    pub probe: bool,
    /// Write to stdout rather than a file.
    pub to_stdout: bool,
    /// Skip entirely if the output already exists.
    pub no_clobber: bool,
    /// Create the output directory if missing.
    pub create_dirs: bool,
    /// Directory to place the output in.
    pub output_dir: Option<PathBuf>,
    /// Probe only: report what the server says and do not fetch the body.
    pub spider: bool,
    /// Print response headers.
    pub server_response: bool,
    /// Retrieve only this byte range.
    pub range: Option<RangeSpec>,
    /// Refuse an object larger than this.
    pub max_filesize: Option<u64>,
    /// Explicit proxy, overriding the environment.
    pub proxy: Option<String>,
    /// Ignore any proxy in the environment.
    pub no_proxy: bool,
    /// Set the local mtime from the server.
    pub remote_time: bool,
    /// Write the object's ETag here.
    pub etag_save: Option<PathBuf>,
    /// Skip if the stored ETag still matches.
    pub etag_compare: Option<PathBuf>,
    /// Sort the output into a per-category subdirectory based on file type.
    pub sort_by_type: bool,
    /// Content-Type the server reported, for classification.
    pub content_type: Option<String>,
    /// Accept any TLS certificate.
    pub insecure: bool,
    /// Overwrite an existing file without asking.
    pub force: bool,
    /// Discard the bytes instead of saving them.
    pub no_save: bool,
    /// Report the object's SHA-256; see `Cli::print_checksum`. Off by default:
    /// hashing is a pass over every byte and nothing needs it unless asked.
    pub print_checksum: bool,
    /// Write a per-chunk digest manifest for what arrived.
    pub emit_manifest: Option<PathBuf>,
    /// Verify each chunk against this manifest as it arrives.
    pub chunk_digests: Option<PathBuf>,
    /// Chunk grid for --emit-manifest.
    pub chunk_size: Option<u64>,
    /// Publisher ranking and per-mirror ceilings, index-aligned with `urls`.
    ///
    /// Empty means unranked, which is exactly what a bare list of URLs is â€?so
    /// every caller that does not read a mirror list gets the behaviour it
    /// always had.
    pub source_plans: Vec<pdl_core::SourcePlan>,
    /// Size and digests attested by a Metalink document rather than by a mirror.
    ///
    /// See [`crate::metalink`] for why this changes what mirror agreement means:
    /// independent mirror operators cannot share an `ETag`, so the pairwise
    /// validator gate is unsatisfiable across a real mirror list, while a
    /// document that states the size and a content digest establishes the same
    /// thing more strongly and from outside the mirrors.
    pub attested: Option<crate::metalink::Attested>,
    /// Treat a URL that turns out to SERVE a Metalink document as a mirror list
    /// rather than as the file to save.
    ///
    /// Decided at probe time from `Content-Type`, because that is the only point
    /// at which the answer is free: `https://mirrors.example/metalink?repo=x`
    /// has no extension to read and probing it twice to find out would cost a
    /// round trip on every download. Off means "save whatever the URL serves",
    /// which is what a user debugging a redirector wants.
    pub follow_metalink: bool,
    /// What reading the mirror list revealed, for the run log.
    ///
    /// Carried on the job rather than printed where it was discovered so it goes
    /// through `Progress::event` like everything else: verbosity-gated, routed
    /// to `--logfile`, and silenced by `-q`. A bare `eprintln!` obeys none of
    /// those, and a mirror list has plenty to say.
    pub metalink_notes: Vec<String>,
    /// Which entry and which mirrors to take, when a document is followed.
    ///
    /// Carried on the job rather than read from `Cli` because the follow happens
    /// inside the engine â€?the discovery is a `Content-Type` on a probe that has
    /// already been paid for â€?and the engine has no access to the parsed
    /// command line.
    pub metalink_select: crate::metalink::Selection,
}

/// What happened, for `--json` and for the report.
///
/// `Default` is the canonical "nothing happened yet" value (`ok: false`, every
/// counter zero, every option `None`); construct partial outcomes with
/// struct-update syntax rather than spelling out all 22 fields.
#[derive(serde::Serialize, Clone, Default)]
pub struct Outcome {
    pub url: String,
    pub output: String,
    pub size: u64,
    /// Wall time for the whole invocation, including probing and the concurrency
    /// measurement. This is the number a user timing the command sees.
    pub elapsed_s: f64,
    /// Wall time for the byte transfer alone.
    ///
    /// Reported separately because the two differ by seconds on a slow path â€?the
    /// progress bar's clock starts when bytes start, so a single `elapsed_s` next to a
    /// bar reading "1.7s" looked like two clocks disagreeing. Setup is a per-path cost
    /// a real client caches; transfer is the steady-state figure.
    pub transfer_s: f64,
    /// Wall time spent probing before the transfer began.
    pub setup_s: f64,
    pub throughput_bps: f64,
    pub requests: u64,
    /// The connection count the transfer actually ran at.
    pub connections: usize,
    /// The highest active limit reached, which under `--adaptive` is the top of the
    /// search rather than the answer. Equal to `connections` for fixed `-x`.
    pub peak_connections: usize,
    /// The most connections that ever actually held a range simultaneously. Differs from
    /// the limit in both directions: lowering the limit lets a busy connection finish, and
    /// a connection can be idle between ranges.
    pub peak_busy_connections: usize,
    /// Integral of busy connections over time. The only concurrency figure here that
    /// summarises the whole run rather than a moment or a bound.
    pub connection_seconds: f64,
    pub delta_s: f64,
    pub sha256: Option<String>,
    pub checksum_ok: Option<bool>,
    pub resumed_from: u64,
    pub ok: bool,
    pub note: Option<String>,
    /// Detected format name, e.g. "gzip".
    pub format: Option<String>,
    /// Detected category, e.g. "archive".
    pub category: Option<String>,
    /// Disagreement between the payload, the name, and the server's type.
    pub format_conflict: Option<String>,
    /// Short human label for the format, e.g. "gzip-compressed tar".
    pub format_label: Option<String>,
    /// One-sentence explanation, for a CLI hint or a GUI tooltip.
    pub format_description: Option<String>,
    /// What the category is and what to do with it.
    pub category_description: Option<String>,
}

fn targets_for(
    urls: &[String],
    headers: &[String],
    agent: &str,
    proxy: Option<&str>,
    no_proxy: bool,
) -> Result<Vec<(Url, Target)>, String> {
    // Precedence: --no-proxy beats --proxy beats the environment. A user who
    // passes --no-proxy and still egresses through one would be misled about
    // where their traffic went.
    //
    // The scheme is load-bearing and must NOT be stripped: a SOCKS proxy carries a raw
    // TCP stream, so the request stays origin-form and the target must look direct. An
    // earlier version discarded the scheme and treated every proxy as HTTP, which would
    // have sent absolute-form GETs at a SOCKS port.
    let px = if no_proxy {
        None
    } else if let Some(spec) = proxy {
        match pdl_net::Proxy::parse(spec) {
            // SOCKS: handled by the connector, so the target is built as if direct.
            Ok(p) if p.kind.is_socks() => None,
            Ok(p) => Some((p.host, p.port)),
            Err(e) => return Err(format!("--proxy: {e}")),
        }
    } else {
        proxy_from_env()
    };
    let pxr = px.as_ref().map(|(h, p)| (h.as_str(), *p));
    urls.iter()
        .map(|u| {
            let parsed = Url::parse(u).ok_or_else(|| {
                // Name the scheme's own reason when there is one: "sftp is not implemented
                // yet, and here is what to use instead" is actionable where "unsupported"
                // is not.
                match u.split_once("://") {
                    Some((s, _)) => format!(
                        "{u}: {} (supported: {})",
                        pdl_net::scheme::unsupported_reason(&s.to_ascii_lowercase()),
                        pdl_net::scheme::supported().join(", ")
                    ),
                    None => format!(
                        "unparsable URL: {u} (supported schemes: {})",
                        pdl_net::scheme::supported().join(", ")
                    ),
                }
            })?;
            // `to_target` builds an HTTP request target and rejects any other scheme. FTP
            // does not use one â€?it gets an Endpoint instead â€?so a placeholder is paired
            // here and the FTP branch replaces it. Calling to_target for an ftp:// URL
            // returned an error BEFORE the FTP branch was ever reached, which is why an
            // ftp:// fetch failed silently with exit 1 and no message.
            if parsed.is_ftp() {
                let t = pdl_net::Target::direct(&parsed.host, parsed.port, &parsed.path);
                return Ok((parsed, t));
            }
            let t = parsed
                .to_target(pxr)?
                .with_headers(headers.to_vec(), Some(agent.to_string()));
            Ok((parsed, t))
        })
        .collect()
}

/// Probe every mirror, and keep only those that agree with the first on both
/// size and validator.
///
/// This is a correctness gate, not an optimisation. Assembling ranges from two
/// mirrors that serve *different* bytes produces a corrupt file that passes every
/// length check â€?the unsound case the capability lattice names. Mirrors that
/// disagree are dropped with a warning rather than silently mixed in.
/// Probe an object, following redirects and falling back from HEAD to a ranged GET.
///
/// Three real behaviours make this more than a single request:
///   * a redirect: GitHub release assets answer HEAD with `302` and
///     `Content-Length: 0`, so the redirect must be followed rather than described;
///   * a server that does not answer HEAD at all: a public speed-test host closes the
///     connection with no reply (and no TLS `close_notify`) on HEAD while answering
///     GET normally on the same path;
///   * a server that omits `Content-Length` on HEAD, which a ranged GET recovers from
///     `Content-Range`.
///
/// The redirect budget is bounded, so a redirect loop costs a fixed number of round
/// trips rather than running forever.
/// Proxy setting for a URL, honouring `--no-proxy` semantics via the environment.
/// How many bytes of `path` are really there.
///
/// A resume record is authoritative when present. Otherwise fall back to allocated
/// blocks, which for a sparse file is far smaller than its apparent length. On a
/// filesystem that does not report blocks, treat a file whose apparent length equals
/// its allocation as fully present.
fn bytes_present(path: &Path, sidecar: Option<&Sidecar>) -> u64 {
    if let Some(sc) = sidecar {
        return sc.bytes_done();
    }
    let Ok(md) = std::fs::metadata(path) else {
        return 0;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let allocated = md.blocks() * 512;
        // Allocation is rounded up to a block, so it can exceed the apparent length by
        // less than one block; clamp rather than report more bytes than the file has.
        allocated.min(md.len())
    }
    #[cfg(not(unix))]
    {
        md.len()
    }
}

/// An Outcome with every field at a neutral value, for tests that care about two of them.
#[cfg(test)]
pub fn stub_outcome() -> Outcome {
    Outcome::default()
}

/// Where a job's bytes are destined.
///
/// Decided before a transfer begins so that `--no-save` discards bytes in memory
/// without creating or leaving temporary files on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    /// Write to this path and keep it.
    File(String),
    /// Write nothing anywhere: measure the stream and drop the bytes.
    Discard,
    /// Assemble, then stream to stdout. Needs storage because positioned writes
    /// land out of order, so the object is only correct once complete.
    Stdout(String),
}

/// Decide the destination for a job that would otherwise write `out_path`.
pub fn output_target(job: &Job, out_path: &str) -> OutputTarget {
    // `--stdout` is checked first: it needs a real file even under `--no-save`,
    // because out-of-order ranges cannot be streamed to a pipe as they arrive.
    if job.to_stdout {
        return OutputTarget::Stdout(out_path.to_string());
    }
    if job.no_save {
        return OutputTarget::Discard;
    }
    OutputTarget::File(out_path.to_string())
}

/// Rebuild a job around the mirror list a URL turned out to be serving.
///
/// The derived job keeps everything the user asked for â€?output path, rate cap,
/// headers, politeness â€?and replaces only the source list, the attestation, and
/// the ranking. `follow_metalink` is cleared on the result, which bounds the
/// recursion at one hop: a document that names itself, or a redirector that
/// answers a mirror URL with another mirror list, costs one wasted fetch rather
/// than an unbounded chain.
///
/// The output NAME is left alone when the user gave `-O`, and otherwise comes
/// from the document rather than from the URL â€?`metalink?repo=fedora-40` is not
/// a filename, and the document knows what the object is called.
async fn follow_metalink(job: &Job, from: &Url) -> Result<Job, String> {
    let conn = pdl_net::TlsCapableConnector::with_insecure(job.insecure)
        .map(Arc::new)
        .map_err(|e| format!("tls setup failed: {e}"))?;
    let url = from.to_string();
    let doc = crate::metalink::load_url(&conn, &url, &job.headers, &job.user_agent, job.max_redirs)
        .await?;
    let origin = crate::metalink::Origin::Url(url);
    let files = crate::metalink::resolve(&doc, &job.metalink_select, &origin)?;
    // One job fetches one object. A document describing several needs one job
    // each, which only the command-line path can arrange â€?so a follow that
    // lands on a multi-file document takes the first entry and says so, rather
    // than silently fetching one of several and reporting success.
    let first = files
        .first()
        .ok_or_else(|| format!("{origin}: no usable file entry"))?;
    if files.len() > 1 {
        eprintln!(
            "playdl: {origin} describes {} files; fetching {:?}. Use --metalink <url> --metalink-file NAME to choose another, or --metalink <url> alone to fetch them all.",
            files.len(),
            first.name
        );
    }
    Ok(job.clone().with_metalink(first))
}

impl Job {
    /// Point this job at the sources, ranking and attestation of one document
    /// entry.
    pub fn with_metalink(mut self, r: &crate::metalink::Resolved) -> Job {
        self.urls = r.urls.clone();
        self.source_plans = r.plans.clone();
        self.attested = r.attested.clone();
        self.metalink_notes = r.notes.clone();
        if let Some(n) = r.signature_note() {
            self.metalink_notes.push(n.to_string());
        }
        // One hop only; see `follow_metalink`.
        self.follow_metalink = false;
        if self.output.is_none() {
            self.output = Some(PathBuf::from(&r.name));
        }
        self
    }
}

/// How wide a transfer driven by a MIRROR LIST should open, in connections.
///
/// # Why this is not `usable.len()`
///
/// It is tempting to seat every mirror the document offers â€?they are all
/// there, and each takes at least one connection. Measured, that is worse: a
/// real twelve-mirror Fedora document seated at eleven took 9.4 s against 6.0 s
/// at eight, because the initial split hands every seated source a share of the
/// object and the slowest hosts then have to have it taken back off them one
/// repair at a time. A mirror list makes more sources AVAILABLE; it does not
/// make more of them useful, and the useful number is still bounded by the
/// client's own link.
///
/// # It is the width a single-URL download would open, spread across hosts
///
/// A mirror list makes more sources AVAILABLE; it does not make more of them
/// useful. The useful number is still bounded by the client's own link, and
/// `per_host` is already this build's answer to "how many connections is a
/// download worth" â€?so a mirror list opens the same number and points each one
/// at a different server, which is strictly politer than pointing them all at
/// one. The aggregate ceiling still binds on top.
///
/// Measured on the twelve-mirror Fedora document, four interleaved reps of
/// each width, medians of total wall clock: 5.79 s at three sources, 6.37 s at
/// five, 6.73 s at eight, 8.16 s at twelve. Wider is monotonically worse once
/// the link is saturated â€?the first split hands every seated source a share of
/// the object, and the slow ones then have to have it taken back off them one
/// repair at a time. The surplus of a long list pays as RESERVES, not as seats.
///
/// (An earlier revision bounded this by the aggregate ceiling instead, and an
/// in-band ramp was tried in place of a fixed width. Both measured worse â€?the
/// ramp notably so, at a 9.8 s median with an 18.5 s worst case â€?so the
/// numbers above are what the code does.)
fn mirror_list_width(job: &Job, sources: usize) -> usize {
    let ceiling = job.polite.per_host.min(job.polite.total).max(1);
    sources.clamp(1, ceiling)
}

/// How a source is named in the progress view and in the log.
///
/// The host, plus the port when it is not the scheme's default. The port is
/// almost always redundant and occasionally the only thing that distinguishes
/// two rows â€?several mirrors behind one name on different ports, or a local
/// test set â€?and a connection row that cannot be told from its neighbour is
/// not a diagnostic.
fn source_label(u: &Url) -> String {
    let default = match u.scheme.as_str() {
        "https" => 443,
        "ftp" => 21,
        _ => 80,
    };
    if u.port == default {
        u.host.clone()
    } else {
        format!("{}:{}", u.host, u.port)
    }
}

/// A scratch filename unique to this verification.
///
/// Per-CALL, not per-process: several files verify concurrently when multiple URLs are
/// given, and a shared name has them overwriting each other's window.
fn scratch_name() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "hydra_verify_{}_{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Check that the bytes already on disk are a genuine prefix of the remote object.
///
/// Re-fetches a window ending at the current file length and compares it byte for byte.
/// This is what lets `hydra <url>` resume a file it did not write: an interrupted
/// download leaves a valid prefix, and one small range request turns that assumption
/// into a check. The window is the last `WINDOW` bytes rather than the whole file
/// because a mismatch anywhere earlier would have to have been written by a *different*
/// object, and the tail is where a truncated write leaves damage.
///
/// Returns the number of verified bytes, or `None` when the prefix does not match.
async fn verify_prefix<C: pdl_net::Connector>(
    c: &Arc<C>,
    t: &Target,
    path: &Path,
    on_disk: u64,
    tries: u32,
    timeout_s: f64,
) -> Option<u64> {
    const WINDOW: u64 = 64 * 1024;
    if on_disk == 0 {
        return Some(0);
    }
    let lo = on_disk.saturating_sub(WINDOW);
    let want = (on_disk - lo) as usize;
    // Read what we have.
    let mut local = vec![0u8; want];
    {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(path).ok()?;
        f.seek(SeekFrom::Start(lo)).ok()?;
        f.read_exact(&mut local).ok()?;
    }
    // Fetch the same window into a scratch sink.
    //
    // The name must be unique PER VERIFICATION, not per process: with several files
    // verifying concurrently (multiple URLs on one command line) a PID-only name has
    // every one of them writing the same scratch file, so each reads back another
    // object's bytes and reports a mismatch on a file that is in fact identical.
    let tmp = std::env::temp_dir().join(scratch_name());
    let tmps = tmp.to_string_lossy().to_string();
    let sink = Arc::new(SparseSink::create(&tmps, on_disk).ok()?);
    let ok = fetch_range_retry(
        c.clone(),
        t.clone(),
        lo,
        on_disk,
        sink.clone(),
        tries,
        timeout_s,
    )
    .await
    .is_ok();
    drop(sink);
    let remote = if ok {
        let mut buf = vec![0u8; want];
        use std::io::{Read, Seek, SeekFrom};
        std::fs::File::open(&tmps).ok().and_then(|mut f| {
            f.seek(SeekFrom::Start(lo)).ok()?;
            f.read_exact(&mut buf).ok()?;
            Some(buf)
        })
    } else {
        None
    };
    let _ = std::fs::remove_file(&tmp);
    match remote {
        Some(r) if r == local => Some(on_disk),
        _ => None,
    }
}

/// Print the probe exchange with request prefixed by `>` and response prefixed by
/// `<`, so a pasted transcript is unambiguous about direction.
fn print_exchange(pr: &pdl_net::Probe) {
    for line in pr.raw_request.lines() {
        if !line.is_empty() {
            println!("> {line}");
        }
    }
    println!(">");
    for line in pr.raw_head.lines() {
        if !line.is_empty() {
            println!("< {line}");
        }
    }
    println!("<");
}

/// Fetch over FTP: single source, sequential, with the protocol's costs made explicit.
///
/// Deliberately not routed through the scheduler. The scheduler decides how to split work
/// across sources and when to reassign it, and both decisions rest on properties FTP does
/// not have: a validator to prove two sources agree, and free preemption. Driving it anyway
/// would produce reassignment decisions priced for the wrong protocol. What FTP does support
/// is a resumable sequential transfer, which is what this does.
async fn ftp_fetch(job: &Job, u: &Url, p: &mut Progress, outs: String) -> Outcome {
    // `Outcome::stopped` records a note but prints nothing; `failed` prints. Every failure
    // below goes through this so an exit code always arrives with a reason â€?an earlier
    // version returned a bare stopped Outcome and an ftp:// fetch exited 1 in silence.
    macro_rules! bail {
        ($($arg:tt)*) => {{
            let why = format!($($arg)*);
            p.end_phase();
            return failed(job, 0, why);
        }};
    }
    use pdl_net::scheme::Fetcher;
    let t_all = Instant::now();
    let px = proxy_for(u);
    let ep = u.to_endpoint(px.as_ref().map(|(h, pt)| (h.as_str(), *pt)));
    let conn = Arc::new(pdl_net::TcpConnector);
    // `--limit-rate` applies here too. One connection means one limiter, and it
    // is built for this fetch alone; the aggregate story the HTTP path tells
    // across connections has nothing to aggregate over.
    let pace = if job.limit_rate > 0 {
        pdl_net::polite::Pace::shared(Arc::new(pdl_net::polite::RateLimiter::new(job.limit_rate)))
    } else {
        pdl_net::polite::Pace::unlimited()
    };
    let f = pdl_net::ftp::FtpFetcher::new(conn).with_pace(pace);

    p.phase("connecting and logging in");
    let probe = match f.probe(&ep).await {
        Ok(pr) => pr,
        Err(e) => bail!("ftp: {e}"),
    };
    p.end_phase();
    p.event(
        1,
        &format!(
            "ftp {} -> {} bytes, ranges={}, login={}",
            u.host,
            probe.size,
            if probe.ranged { "yes (REST)" } else { "no" },
            if ep.has_credentials() {
                "explicit"
            } else {
                "anonymous"
            }
        ),
    );
    p.event(
        1,
        &format!(
            "ftp: preemption costs {:.0} control round trips here (HTTP pays none), so the \
             object is fetched sequentially from one source",
            f.capabilities().preempt_cost_rtt
        ),
    );
    if job.server_response {
        for line in probe.raw.lines() {
            println!("{line}");
        }
    }
    if job.spider {
        let ok = probe.size > 0;
        return Outcome::stopped(
            job,
            outs.clone(),
            probe.size,
            ok,
            "spider: FTP object exists",
        );
    }
    if probe.size == 0 {
        bail!(
            "ftp: the server does not implement SIZE, and FTP offers no other way to learn \
             the length; refusing rather than writing a file of unknown extent"
        );
    }

    // Resume uses REST, which is exactly a ranged read â€?the one place FTP's range support
    // is a clean fit.
    let start = if job.resume {
        std::fs::metadata(&outs).map(|m| m.len()).unwrap_or(0)
    } else {
        0
    };
    if start >= probe.size {
        return Outcome::stopped(job, outs.clone(), probe.size, true, "already complete");
    }
    let sink = match pdl_net::SparseSink::create(&outs, probe.size) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            bail!("cannot create {outs}: {e}");
        }
    };
    // Draw the same progress bar HTTP gets.
    //
    // The HTTP path renders from the scheduler's per-tick observer callback, which
    // FTP has no equivalent of: `fetch_range` is one `await` that returns when the
    // whole object has landed, so awaiting it directly meant a multi-megabyte FTP
    // download sat in silence and then printed a finished summary. There is no
    // scheduler to observe here, but there is a sink, and the sink counts every
    // byte it writes â€?so the bar is driven from that counter while the fetch runs.
    //
    // `select!` on a pinned future rather than a spawned task: `Progress` is a
    // `&mut` the caller owns and the fetch borrows `ep`, so neither can cross a
    // task boundary without restructuring both.
    p.end_phase();
    // The size FTP already answered has to reach the renderer, or the bar cannot
    // draw. `ftp_fetch` is handed the setup-phase `Progress`, built before any
    // request when no size was known, and a `None` total renders an empty rule, a
    // `?` percentage and a `?` ETA for the whole transfer when total size is unknown,
    // even though SIZE had returned the exact length during the probe two lines above.
    p.set_total(probe.size);
    p.set_baseline(start);
    let t_xfer = Instant::now();
    let r = {
        let fut = f.fetch_range(&ep, start, probe.size, sink.clone());
        tokio::pin!(fut);
        // 100 ms: `draw` already rate-limits itself to ~12 fps, so a faster tick
        // buys nothing and a slower one makes the bar visibly stutter.
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        ticker.tick().await; // the first tick completes immediately
        let mut last_done = start;
        let mut last_at = Instant::now();
        let mut rate = 0.0f64;
        loop {
            tokio::select! {
                res = &mut fut => break res,
                _ = ticker.tick() => {
                    let done = start + sink.written.load(std::sync::atomic::Ordering::Relaxed);
                    let dt = last_at.elapsed().as_secs_f64();
                    if dt > 0.0 {
                        let sample = done.saturating_sub(last_done) as f64 / dt;
                        // Same smoothing the scheduler applies to its own rate
                        // estimate, so the two paths report comparably rather
                        // than FTP showing a jumpier number for the same link.
                        rate = if rate <= 0.0 { sample } else { 0.3 * sample + 0.7 * rate };
                        last_done = done;
                        last_at = Instant::now();
                    }
                    // One connection, by design: FTP has no validator, so mirrors
                    // cannot be proven to serve identical bytes and the fetch is
                    // single-source. The view says so rather than implying a fan-out.
                    // `(lo, pos, hi)`, in that order â€?the renderer fills the row
                    // from `(pos - lo) / (hi - lo)` and prints `lo`-`hi` as the
                    // extent. Passing `(start, size, done)` swapped the cursor
                    // with the end, so the fraction was `(size - start) /
                    // (done - start)`: greater than 1 for the whole transfer,
                    // clamped to a permanently full `[â–ªâ–ªâ–ªâ–ªâ–ªâ–ªâ–ªâ–ªâ–ªâ–ª]`, with the
                    // bytes-so-far printed where the object's length belongs.
                    let views = vec![ConnView {
                        idx: 0,
                        host: u.host.clone(),
                        range: Some((start, done, probe.size)),
                        rate,
                        health: pdl_core::detect::Health::Healthy,
                    }];
                    p.draw(done, &views, Counters { requests: 1, ..Default::default() });
                }
            }
        }
    };
    let xfer = t_xfer.elapsed().as_secs_f64();
    drop(sink);
    if let Err(e) = r {
        return failed(job, start, format!("ftp: {e}"));
    }

    // Classify what arrived, exactly as the HTTP path does: the payload is the only
    // trustworthy signal, and FTP supplies no content type at all.
    let head = {
        use std::io::Read as _;
        let mut buf = vec![0u8; 8192];
        match std::fs::File::open(&outs).and_then(|mut fh| fh.read(&mut buf)) {
            Ok(n) => {
                buf.truncate(n);
                buf
            }
            Err(_) => Vec::new(),
        }
    };
    let name = std::path::Path::new(&outs)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let detection = detect_format(&head, &name, None);
    let digest = {
        use sha2::{Digest as _, Sha256};
        use std::io::Read as _;
        std::fs::File::open(&outs).ok().and_then(|mut fh| {
            let mut h = Sha256::new();
            let mut buf = vec![0u8; 1 << 20];
            loop {
                match fh.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => h.update(&buf[..n]),
                    Err(_) => return None,
                }
            }
            Some(pdl_net::digest::to_lower_hex(&h.finalize()))
        })
    };
    let elapsed = t_all.elapsed().as_secs_f64();
    p.finish(
        probe.size,
        true,
        crate::progress::Counters {
            requests: 1,
            ..Default::default()
        },
        digest.as_deref(),
    );
    if !job.quiet && job.verbose == 0 {
        if let Some(fm) = detection.format {
            println!("  {}", fm.hint());
        }
    }
    Outcome {
        url: job.urls[0].clone(),
        output: outs,
        size: probe.size,
        elapsed_s: elapsed,
        transfer_s: xfer,
        setup_s: (elapsed - xfer).max(0.0),
        throughput_bps: if xfer > 0.0 {
            (probe.size - start) as f64 / xfer
        } else {
            0.0
        },
        requests: 1,
        connections: 1,
        // No concurrency search on this path, so the peak is what it ran at.
        peak_connections: 1,
        // FTP has no validator, so fetches are single-source by design: one connection
        // held a range for the whole transfer.
        peak_busy_connections: 1,
        connection_seconds: elapsed,
        delta_s: 0.0,
        sha256: digest,
        checksum_ok: None,
        resumed_from: start,
        ok: true,
        note: None,
        format: detection.format.map(|fm| fm.name.to_string()),
        category: Some(detection.category.as_str().to_string()),
        format_conflict: detection.conflict,
        format_label: detection.format.map(|fm| fm.label().to_string()),
        format_description: detection.format.map(|fm| fm.description().to_string()),
        category_description: Some(detection.category.description().to_string()),
    }
}

/// Resolve the proxy for a URL, for callers outside the download engine.
pub fn proxy_for_public(_u: &Url, proxy: Option<&str>, no_proxy: bool) -> Option<(String, u16)> {
    if no_proxy {
        return None;
    }
    match proxy {
        Some(spec) => match pdl_net::Proxy::parse(spec) {
            Ok(px) if !px.kind.is_socks() => Some((px.host, px.port)),
            _ => None,
        },
        None => crate::url::proxy_from_env(),
    }
}

/// A Job with every option at its default, for callers that only need one or two set.
pub fn default_job() -> Job {
    Job {
        ticks: None,
        urls: Vec::new(),
        output: None,
        conns: None,
        resume: false,
        limit_rate: 0,
        max_redirs: 8,
        ip_family: pdl_net::IpFamily::Any,
        show_error: false,
        logfile: None,
        tries: 3,
        timeout_s: 30.0,
        checksum: None,
        headers: Vec::new(),
        user_agent: pdl_net::DEFAULT_USER_AGENT.into(),
        verbose: 0,
        quiet: true,
        no_progress: true,
        polite: pdl_net::polite::Politeness::default(),
        adaptive: false,
        probe: true,
        to_stdout: false,
        no_clobber: false,
        create_dirs: false,
        output_dir: None,
        spider: false,
        server_response: false,
        range: None,
        max_filesize: None,
        proxy: None,
        no_proxy: false,
        remote_time: false,
        etag_save: None,
        etag_compare: None,
        sort_by_type: false,
        content_type: None,
        insecure: false,
        force: false,
        no_save: false,
        print_checksum: false,
        emit_manifest: None,
        chunk_digests: None,
        chunk_size: None,
        source_plans: Vec::new(),
        attested: None,
        metalink_notes: Vec::new(),
        metalink_select: crate::metalink::Selection::default(),
        // On by default: a URL that answers with `application/metalink4+xml` is
        // a mirror list, and saving it as `big.iso` would hand the user 6 KB of
        // XML named after the 4 GB image they asked for.
        follow_metalink: true,
    }
}

/// A `Progress` configured from the job: logfile attached, and stdout reserved
/// for the payload under `--stdout`.
///
/// `run` builds two â€?one for the setup phase (size unknown), one for the
/// transfer (size known) â€?and both must agree on this configuration: the
/// setup-phase instance missing the stdout reservation was what prepended 71
/// bytes to a piped archive. stdout belongs to the object, on the same
/// principle `--json` follows: a machine channel carries one thing, or it
/// carries nothing usable.
fn progress_for(job: &Job, name: &str, size: Option<u64>) -> Result<Progress, String> {
    let mut p = Progress::new(name, size, job.verbose, job.no_progress, job.quiet);
    if let Some((path, append)) = &job.logfile {
        if let Err(e) = p.set_logfile(path, *append) {
            return Err(format!("cannot open log file {}: {e}", path.display()));
        }
    }
    if job.to_stdout {
        p.reserve_stdout_for_payload();
    }
    Ok(p)
}

fn proxy_for(u: &Url) -> Option<(String, u16)> {
    let _ = u;
    crate::url::proxy_from_env()
}

/// Probe `u` for metadata, following redirects, for the reporting commands.
///
/// `hydra checksum` and `--server-response` want the headers of the FINAL
/// response â€?a 302's headers answer a different question than the one asked.
/// Unlike the transfer path's [`probe_resolving`] there is no `Progress` to
/// log hops to, and reporting prefers to describe what it reached over
/// refusing: on an exhausted hop budget or an unusable `Location` the last
/// probe is returned as-is rather than as an error.
///
/// A HEAD that fails or reports no size falls back to a one-byte ranged GET,
/// the same recovery the transfer path uses â€?CDNs that mishandle HEAD
/// (unclean TLS close, `Content-Length: 0`) answer the GET correctly.
///
/// Returns the final probe and the URL it came from.
pub async fn probe_public<C: pdl_net::Connector>(
    c: &C,
    u: &Url,
    args: &crate::cli::Cli,
) -> Result<(pdl_net::Probe, Url), String> {
    let mut cur = u.clone();
    let mut hops = 0u32;
    loop {
        let px = proxy_for_public(&cur, args.proxy.as_deref(), args.no_proxy);
        let target = cur
            .to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p)))?
            .with_headers(args.headers.clone(), Some(args.user_agent.clone()));
        // One rule for "HEAD said nothing usable", shared with the GUI and the
        // engine rather than restated here: a HEAD that states `Content-Length: 0`
        // has answered, and a ranged GET against a zero-length object is refused.
        let pr = pdl_net::probe_resilient(c, &target)
            .await
            .map_err(|e| e.to_string())?;
        if pr.is_redirect() && hops < args.max_redirs {
            let next = pr
                .location
                .as_deref()
                .and_then(|loc| crate::url::Url::parse(loc).or_else(|| cur.join(loc)));
            if let Some(next) = next {
                cur = next;
                hops += 1;
                continue;
            }
        }
        // The same forwarding expressed in HTML rather than in a header, so
        // that `--server-response` and `hydra checksum` describe the object
        // reached rather than the referrer stripper in front of it.
        if pr.maybe_redirector() && hops < args.max_redirs {
            if let Some(next) = pdl_net::html_redirect(c, &target)
                .await
                .and_then(|loc| cur.join(&loc))
            {
                cur = next;
                hops += 1;
                continue;
            }
        }
        return Ok((pr, cur));
    }
}

/// One source after redirect resolution: what described it, and where it ended up.
struct Resolved {
    probe: pdl_net::Probe,
    /// The target the transfer must use â€?post-redirect, which is commonly a
    /// different host from the one asked for.
    target: Target,
    /// The URL the last hop arrived at.
    url: Url,
    /// At least one hop was a redirector PAGE rather than a `3xx`.
    ///
    /// Kept because it changes what the file should be called: a `3xx` chain
    /// starts from a URL the user named, so its filename is theirs to keep
    /// (this is curl's and wget's rule, and `-O` exists for the rest), while a
    /// redirector page means the URL they had named a forwarding stub. Nobody
    /// asked for `index.html`.
    via_html: bool,
}

///
/// Logs into `log` rather than straight to `Progress`, because the callers probe
/// mirrors CONCURRENTLY and a shared renderer cannot be borrowed by several
/// futures at once. Replaying the buffers in index order afterwards also keeps
/// the output deterministic: interleaved by completion, the same mirror list
/// would print in a different order on every run.
async fn probe_resolving<C>(
    c: &C,
    u: &Url,
    t: &Target,
    log: &mut Vec<(u8, String)>,
    max_redirs: u32,
) -> Result<Resolved, String>
where
    C: pdl_net::Connector,
{
    // The hop budget is the CLI's, not a constant. `--max-redirs` was parsed,
    // validated, and listed in `--help`, but the resolver used a hardcoded
    // `MAX_HOPS = 8` that nothing could influence: `--max-redirs 0` followed the
    // redirect and downloaded the object anyway.
    //
    // `0` means refuse to follow any redirect hops. One probe still
    // happens â€?that is how a redirect is discovered at all â€?but the hop is not
    // taken.
    let max_hops = max_redirs as usize;
    let mut target = t.clone();
    let mut current = u.clone();
    let mut via_html = false;
    // `0..=max_hops`: the extra pass is what answers the request that the last
    // permitted hop arrived at. Without it a budget of N would resolve only N-1.
    for hop in 0..=max_hops {
        // HEAD, then a ranged GET when it gives nothing usable â€?see
        // [`pdl_net::probe_resilient`], which is also what the GUI and the engine
        // ask, so the three cannot drift apart on which servers they can read.
        let pr = probe_resilient(c, &target)
            .await
            .map_err(|e| e.to_string())?;

        if pr.is_redirect() {
            let loc = pr.location.clone().unwrap_or_default();
            if hop >= max_hops {
                return Err(if max_hops == 0 {
                    format!("refusing to follow a redirect to {loc:?}: --max-redirs 0")
                } else {
                    format!("too many redirects (--max-redirs {max_redirs})")
                });
            }
            let next = current
                .join(&loc)
                .ok_or_else(|| format!("unparsable redirect target {loc:?}"))?;
            log.push((1, format!("redirect {} -> {}", current.host, next.host)));
            let px = proxy_for(&next);
            target = next
                .to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p)))
                .map_err(|e| format!("redirect target unusable: {e}"))?;
            current = next;
            continue;
        }

        // A redirect the server expressed in HTML instead of in a header: a
        // referrer stripper or link filter answering `200` with a page whose
        // whole content is "go here instead". Charged to the SAME hop budget â€?
        // two such pages pointing at each other is a loop like any other, and
        // `--max-redirs` is what bounds it.
        if pr.maybe_redirector() && hop < max_hops {
            if let Some(next) = pdl_net::html_redirect(c, &target)
                .await
                .and_then(|loc| current.join(&loc))
            {
                log.push((
                    1,
                    format!("html redirect {} -> {}", current.host, next.host),
                ));
                let px = proxy_for(&next);
                target = next
                    .to_target(px.as_ref().map(|(h, p)| (h.as_str(), *p)))
                    .map_err(|e| format!("redirect target unusable: {e}"))?;
                current = next;
                via_html = true;
                continue;
            }
        }
        // An error status is an ANSWER, not a description of the object.
        //
        // `probe_resilient` deliberately returns the status rather than failing,
        // so that a `404` learned from HEAD is reported instead of "the ranged GET
        // was unsatisfiable" â€?the better error. That leaves the test to the
        // caller, and this caller did not make it: a `400 Bad Request` with a
        // 24-byte JSON body became a 24-byte object, the transfer was planned,
        // those 24 bytes were split across eight connections, and only the range
        // requests failed a second later with an internal message. The FFI driver
        // and the stream inspector already check here; this is the one path that
        // did not.
        if pr.status >= 400 {
            // No host in the message: both callers name it themselves, and a
            // message that repeats it prints the host twice on one line.
            return Err(format!(
                "server answered {}",
                pdl_net::describe_status(pr.status)
            ));
        }
        return Ok(Resolved {
            probe: pr,
            target,
            url: current,
            via_html,
        });
    }
    Err(format!("too many redirects (--max-redirs {max_redirs})"))
}

/// One mirror's probe, as it comes back from the concurrent fan-out.
///
/// The index is what everything downstream is aligned to; the URL is carried
/// alongside because a probe may fail before there is a `Resolved` to read it
/// from, and a failure that cannot name its host is not a diagnostic.
type ProbeOutcome = (usize, Url, Result<Resolved, String>, Vec<(u8, String)>);

/// Probe every mirror and keep the ones that can be assembled together.
///
/// # Two different tests, because there are two different kinds of evidence
///
/// Without a mirror list the only evidence available is what the mirrors
/// themselves say, so agreement has to be established PAIRWISE: same size and
/// the same strong validator as the first source. That is the right test for its
/// evidence, and it is deliberately strict â€?two mirrors serving different
/// builds produce a file that passes every length check and is silently wrong.
///
/// `attested_size` is different evidence. It comes from a Metalink document,
/// published by whoever built the object, on a host that is usually not any of
/// the mirrors, alongside a content digest for the whole file. Against that, the
/// pairwise validator test is not merely unnecessary â€?it is unsatisfiable:
/// independent mirror operators run independent web servers and cannot share an
/// `ETag`, so requiring one keeps exactly ONE source out of a nineteen-mirror
/// list. The document's size is the admission test instead, and its digest (per
/// chunk, where `<pieces>` is published) is what actually catches a mirror
/// serving something else.
/// May a mirror that answered AFTER the transfer started join the bench?
///
/// The oath is the seats': a reserve's bytes are spliced into the same file on
/// substitution, so its admission cannot be weaker than the front door's. With
/// a document, that is the attested size. Without one, the pairwise gate: the
/// first source's size AND its strong validator â€?a weak or absent validator
/// admits nothing late, exactly as it admits nothing up front. Ranges are
/// required either way, because the first thing a substituted source is asked
/// for is a range.
fn bench_admission(
    attested_size: Option<u64>,
    first_size: u64,
    first_strong_validator: Option<&str>,
    pr: &pdl_net::Probe,
) -> bool {
    if !pr.ranges {
        return false;
    }
    match attested_size {
        Some(want) => pr.size == want,
        None => {
            pr.size == first_size
                && !pr.weak_validator
                && first_strong_validator.is_some()
                && pr.validator.as_deref() == first_strong_validator
        }
    }
}

/// Mirrors admitted after the transfer has already started.
///
/// `(index into the caller's `pairs`, post-redirect target)`. The caller pairs
/// them with the rankings it holds and feeds them to the reserve bench.
type LateMirrors = tokio::sync::mpsc::UnboundedReceiver<(usize, Target)>;

/// What probing the mirror list produced.
struct Probed {
    /// The probe the transfer's size, validator and type are read from.
    first: pdl_net::Probe,
    /// Indices of the mirrors that may be SEATED, in the caller's order.
    keep: Vec<usize>,
    /// Post-redirect targets, by index.
    resolved: Vec<(usize, Target)>,
    /// The name a redirector page moved the object to, if it did.
    renamed: Option<String>,
    /// Mirrors still being probed when the transfer was allowed to start.
    late: Option<LateMirrors>,
    /// Wall clock to the first successful probe, as the per-request setup cost.
    ///
    /// This is the only measurement of the path's latency taken before the
    /// transfer, and everything timed in units of `delta` depends on it: the
    /// ramp's measurement windows, the stall timeout, the repair deadband. It
    /// used to be discarded and a 50 ms prior used instead, which is roughly
    /// right for a nearby origin and half the truth on a transatlantic one â€?
    /// measured on a 100 ms path, the ramp judged each newly admitted connection
    /// over a 0.15 s window, caught it mid-slow-start, concluded it was slower
    /// than the level below, and settled at one connection on a link that
    /// scaled to eight.
    first_rtt: f64,
}

async fn probe_all(
    conn: &Arc<TlsCapableConnector>,
    pairs: &[(Url, Target)],
    p: &mut Progress,
    max_redirs: u32,
    attested_size: Option<u64>,
    want_seats: usize,
) -> Result<Probed, String> {
    // Probed CONCURRENTLY, bounded.
    //
    // Sequentially, this was the single most expensive thing about using a
    // mirror list: a real Fedora document lists a dozen fetchable mirrors on
    // three continents, and one HEAD each, in series, cost 14.4 s before the
    // first byte of a 5.9 KB object. The probes are independent â€?each asks one
    // host what it holds â€?so the whole set costs about what the slowest one
    // does.
    //
    // Bounded, but not by politeness: every probe in this set goes to a
    // DIFFERENT host, and one HEAD each is not something any of them feels. The
    // per-host ceilings elsewhere are the ones that answer that question. This
    // cap exists only so a forty-mirror document cannot open forty sockets at
    // once and run into an fd limit.
    //
    // Sixteen rather than a handful because the cost here is latency, not
    // bandwidth, and it is paid before the first byte: measured against a real
    // Fedora document (twelve fetchable mirrors on three continents), six in
    // flight took two waves and 4.1 s of setup, which was 3.6 s of the 4.5 s by
    // which the mirror-list run trailed a single-mirror one. One wave removes
    // almost all of it.
    const PROBE_FANOUT: usize = 16;
    let gate = Arc::new(tokio::sync::Semaphore::new(PROBE_FANOUT));
    let mut set = tokio::task::JoinSet::new();
    for (i, (u, t)) in pairs.iter().enumerate() {
        let c = conn.clone();
        let gate = gate.clone();
        let (u, t) = (u.clone(), t.clone());
        set.spawn(async move {
            let mut log: Vec<(u8, String)> = Vec::new();
            let permit = gate.acquire_owned().await;
            let r = probe_resolving(c.as_ref(), &u, &t, &mut log, max_redirs).await;
            drop(permit);
            (i, u, r, log)
        });
    }
    // How long to wait for SEATS â€?and only for seats.
    //
    // The probe phase is paid entirely before the first byte, so its cost is the
    // SLOWEST mirror the transfer waits for, not the average. This window used
    // to be the point at which slow mirrors were ABANDONED, which is why it was
    // generous: cutting it short threw away working sources. It is not that any
    // more. A mirror that misses the window keeps probing in the background and
    // joins the reserve bench when it answers, so the only thing the window now
    // decides is how long the transfer holds still hoping for one more SEAT.
    //
    // That makes it safe to be short, and being short is worth real time: the
    // floor alone was 2.0 s in front of a ~5 s transfer. Six hundred
    // milliseconds is still several times a healthy intercontinental round trip.
    //
    // Relative, not fixed, because "slow" is a property of the path: three times
    // the fastest mirror's own round trip, floored so a LAN-fast first answer
    // cannot make it unreasonably tight, capped so a pathological one cannot
    // reintroduce the wait. It opens only once a mirror has been ADMITTED â€?a
    // run whose first answers all fail still waits, because the alternative is
    // failing while a working mirror is still dialling â€?and never applies to a
    // single-source run, which has nothing to choose between.
    const PROBE_GRACE_MULTIPLE: f64 = 3.0;
    const PROBE_GRACE_MIN: std::time::Duration = std::time::Duration::from_millis(600);
    const PROBE_GRACE_MAX: std::time::Duration = std::time::Duration::from_secs(10);
    // Enough is enough: stop waiting once the seats are filled.
    //
    // This is the whole cost of using a mirror list. The transfer needs exactly
    // two things from the probe phase â€?a size and enough mirrors to seat the
    // connection budget â€?and everything past that only fills a RESERVE bench
    // nothing consults until a source fails. Waiting for it means waiting for
    // the slowest host on the list to answer a HEAD, in front of a transfer
    // that could already be running: measured against a real twelve-mirror
    // Fedora document, 2.0 s of dead time before the first byte, against a
    // ~5 s transfer.
    //
    // So the loop stops at `want_seats` and the mirrors still in flight are
    // handed to the caller as a stream. They keep probing while bytes move and
    // join the bench as they are admitted â€?a reserve is worth the same
    // whenever it arrives, because nothing looks at the bench until something
    // breaks. The grace window below is now only the backstop for a list that
    // never yields enough seats at all.
    let (late_tx, late_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut done: Vec<ProbeOutcome> = Vec::new();
    let probe_start = Instant::now();
    let mut first_ok: Option<std::time::Duration> = None;
    let mut seated = 0usize;
    let mut streaming = false;
    while !set.is_empty() {
        if seated >= want_seats {
            streaming = true;
            break;
        }
        let joined = match first_ok {
            Some(fastest) if pairs.len() > 1 => {
                let window = std::time::Duration::from_secs_f64(
                    fastest.as_secs_f64() * PROBE_GRACE_MULTIPLE,
                )
                .clamp(PROBE_GRACE_MIN, PROBE_GRACE_MAX);
                let left = window.saturating_sub(probe_start.elapsed());
                match tokio::time::timeout(left, set.join_next()).await {
                    Ok(v) => v,
                    // Out of patience for seats â€?not out of interest. The
                    // stragglers keep probing and become reserves.
                    Err(_) => {
                        streaming = true;
                        break;
                    }
                }
            }
            _ => set.join_next().await,
        };
        let Some(joined) = joined else { break };
        match joined {
            Ok(v) => {
                if v.2.is_ok() {
                    if first_ok.is_none() {
                        first_ok = Some(probe_start.elapsed());
                    }
                    // Counted on the ADMISSION test the loop below applies, so
                    // "enough seats" means enough usable mirrors and not merely
                    // enough answers. A document that states a size makes this
                    // exact. Without one the pairwise validator gate can still
                    // reject a counted mirror afterwards, so the transfer may
                    // open on fewer sources than it hoped â€?a slight
                    // undershoot, healed by substitution from the bench, and
                    // cheaper than holding the transfer to find out.
                    let usable = match (attested_size, v.2.as_ref().ok()) {
                        (Some(want), Some(r)) => r.probe.size == want,
                        _ => true,
                    };
                    seated += usize::from(usable);
                }
                done.push(v);
            }
            Err(e) if e.is_cancelled() => {}
            Err(e) => return Err(format!("probe task failed: {e}")),
        }
    }
    if streaming && !set.is_empty() {
        p.event(
            1,
            &format!(
                "starting on {seated} source(s) after {:.2}s; {} more mirror(s) are still \
                 being probed and join the reserve bench as they answer",
                probe_start.elapsed().as_secs_f64(),
                set.len(),
            ),
        );
    }
    // Back into the ORDER THE CALLER GAVE, not the order the network answered
    // in. Everything downstream is index-aligned with `pairs` â€?the mirror
    // ranking, the per-source connection split, the progress rows â€?and "which
    // mirror is source 0" must not depend on which handshake finished first, or
    // two runs against the same document cannot be compared.
    done.sort_by_key(|(i, ..)| *i);

    let mut first: Option<pdl_net::Probe> = None;
    // The first probe failure, worded for a user and naming its host. Kept
    // because a lone source's failure IS the transfer's error: summarising a
    // one-item list as "every probe failed" throws away the only thing the
    // user can act on, which for an object that answers `400` is the status
    // the server already gave.
    let mut lone_failure: Option<String> = None;
    let mut keep = Vec::new();
    // Targets after redirect resolution, paired with the index they came from.
    let mut resolved_targets: Vec<(usize, Target)> = Vec::new();
    // The name the FIRST source ended up at, when a redirector page moved it.
    let mut renamed: Option<String> = None;
    for (i, u, res, log) in done {
        for (level, line) in log {
            p.event(level, &line);
        }
        match res {
            Ok(r) => {
                let (pr, resolved) = (r.probe, r.target);
                // A redirect may have moved the object to a different host; the
                // transfer must use the resolved target, not the one we started from.
                resolved_targets.push((i, resolved));
                if i == 0 && r.via_html {
                    // Content-Disposition first: a redirector's destination is
                    // as entitled to name its own file as any other object.
                    renamed = pr
                        .suggested_filename()
                        .or_else(|| Some(r.url.suggested_filename()));
                }
                let (size, ranges, validator) = (pr.size, pr.ranges, pr.validator.clone());
                p.event(
                    1,
                    &format!(
                        "probe {} -> {} bytes, ranges={}, validator={}",
                        u.host,
                        size,
                        if ranges { "yes" } else { "no" },
                        validator.as_deref().unwrap_or("none")
                    ),
                );
                // A document's stated size is the admission test when there is
                // one, for every source including the first: a mirror that
                // disagrees with the document is wrong even if it happens to be
                // the one probed first.
                if let Some(want) = attested_size {
                    if size == want {
                        keep.push(i);
                        if first.is_none() {
                            first = Some(pr);
                        }
                    } else {
                        p.event(
                            0,
                            &format!(
                                "skipping {}: serves {size} bytes, the mirror list says {want}",
                                u.host
                            ),
                        );
                    }
                    continue;
                }
                match &first {
                    None => {
                        first = Some(pr);
                        keep.push(i);
                    }
                    Some(f0) => {
                        let same_size = f0.size == size;
                        let same_val = match (&f0.validator, &validator) {
                            (Some(a), Some(b)) => a == b,
                            // Without validators on both sides, byte identity
                            // across mirrors is unverifiable. Refuse to mix.
                            _ => false,
                        };
                        if same_size && same_val {
                            keep.push(i);
                        } else {
                            p.event(
                                0,
                                &format!(
                                    "skipping {}: {} (cannot prove it serves the same bytes)",
                                    u.host,
                                    if same_size {
                                        "validator differs"
                                    } else {
                                        "size differs"
                                    }
                                ),
                            );
                        }
                    }
                }
            }
            Err(e) => {
                p.event(0, &format!("probe failed for {}: {e}", u.host));
                if lone_failure.is_none() {
                    lone_failure = Some(format!("{e} for {}", u.host));
                }
            }
        }
    }
    // Only now â€?with `first` in hand â€?can the stragglers be given their
    // admission test, so this is where the background prober starts.
    //
    // # The bench takes the SAME oath the seats took
    //
    // A reserve is not a spectator: on substitution its bytes are spliced into
    // the same file the seated mirrors are filling. So the test cannot be
    // weaker than the one at the front door. With a document, that is the
    // attested size. Without one, it is the pairwise gate â€?same size as the
    // first source AND the same strong validator â€?and an earlier revision of
    // this spawn skipped exactly that, which would have let `-x 2 --mirrors a
    // b c` bench mirror `c` unexamined and splice whatever it served into the
    // file when a seat failed. Requiring ranges as well is not optional
    // either: a substituted source is immediately asked for ranges, which is
    // the one thing a no-ranges mirror cannot answer.
    let late = match (streaming && !set.is_empty(), &first) {
        (true, Some(f0)) => {
            let first_size = f0.size;
            // Only a STRONG validator authorises pairwise splicing; a weak one
            // admits nothing late, exactly as it admits nothing up front.
            let first_validator = f0.validator.clone().filter(|_| !f0.weak_validator);
            tokio::spawn(async move {
                while let Some(joined) = set.join_next().await {
                    let Ok((i, _u, res, _log)) = joined else {
                        continue;
                    };
                    let Ok(r) = res else { continue };
                    if !bench_admission(
                        attested_size,
                        first_size,
                        first_validator.as_deref(),
                        &r.probe,
                    ) {
                        continue;
                    }
                    // A closed receiver means the transfer ended; nothing left
                    // to probe for.
                    if late_tx.send((i, r.target)).is_err() {
                        return;
                    }
                }
            });
            Some(late_rx)
        }
        _ => None,
    };
    match first {
        Some(pr) if !keep.is_empty() => Ok(Probed {
            first: pr,
            keep,
            resolved: resolved_targets,
            renamed,
            late,
            // A probe is a connect plus a request, so this overstates one
            // round trip and understates nothing. Clamped the way the GUI
            // clamps the same measurement.
            first_rtt: first_ok
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.05)
                .clamp(0.05, 45.0),
        }),
        // One URL, one answer. With a mirror list the per-source lines above
        // have already said what each one did, and the summary is the honest
        // description of the set; with a single source there is no set.
        _ => Err(match lone_failure {
            Some(e) if pairs.len() == 1 => e,
            _ => "no usable source: every probe failed".into(),
        }),
    }
}

/// Set a file's modification time from a Unix timestamp.
fn set_mtime(path: &Path, secs: u64) -> std::io::Result<()> {
    let f = std::fs::File::options().write(true).open(path)?;
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

/// Reduce a range-mode output to just the fetched span.
///
/// The transfer wrote `[lo, hi)` at its true offsets inside a file of the whole
/// object's length, so everything outside the span is a hole that reads as
/// zeros. This moves the span to offset 0 and truncates.
///
/// Copied in bounded blocks rather than read whole: the point of positioned
/// writes is that memory does not scale with the object, and a range can be
/// gigabytes. `lo == 0` skips the move and only truncates.
fn extract_span(path: &Path, lo: u64, hi: u64) -> std::io::Result<()> {
    use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
    let span = hi.saturating_sub(lo);
    if lo > 0 {
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let mut buf = vec![0u8; 1 << 20];
        let (mut src, mut dst, mut left) = (lo, 0u64, span);
        while left > 0 {
            let want = (buf.len() as u64).min(left) as usize;
            f.seek(SeekFrom::Start(src))?;
            f.read_exact(&mut buf[..want])?;
            f.seek(SeekFrom::Start(dst))?;
            f.write_all(&buf[..want])?;
            src += want as u64;
            dst += want as u64;
            left -= want as u64;
        }
        f.flush()?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_len(span)
}

/// Verify a finished file chunk-by-chunk against a manifest, refetching any
/// chunk whose digest does not match.
///
/// Returns a human summary. An unrepairable mismatch is an error, not a warning:
/// the file is known-wrong at a known offset, and reporting success for it would
/// be the silent-corruption failure this project keeps designing against.
async fn verify_and_repair_chunks(
    conn: &Arc<TlsCapableConnector>,
    usable: &[(Url, Target)],
    out_path: &Path,
    manifest_path: &Path,
    job: &Job,
    p: &mut Progress,
) -> Result<String, String> {
    use pdl_net::manifest::{Manifest, Trust};

    let text = std::fs::read_to_string(manifest_path)
        .map_err(|e| format!("cannot read manifest {}: {e}", manifest_path.display()))?;
    let m = Manifest::parse(&text).map_err(|e| format!("{}: {e}", manifest_path.display()))?;

    // A manifest handed as a local file is trusted for chunk verification.
    verify_and_repair(conn, usable, out_path, m, Trust::Trusted, job, p).await
}

/// Verify every chunk against a manifest already in hand, refetching failures.
///
/// Split from [`verify_and_repair_chunks`] because the manifest does not always
/// come from a file. A Metalink `<pieces>` list is the same thing arriving by a
/// different road, and it is the road that matters more: `--chunk-digests`
/// requires a user who already has a manifest, while a mirror list ships one
/// with the mirrors it belongs to.
///
/// `trust` decides only whether the digests may name erasure positions for a
/// parity decode â€?see [`pdl_net::manifest::Trust`]. Detection and targeted
/// refetch work at either level, which is what this function does.
async fn verify_and_repair(
    conn: &Arc<TlsCapableConnector>,
    usable: &[(Url, Target)],
    out_path: &Path,
    m: pdl_net::manifest::Manifest,
    trust: pdl_net::manifest::Trust,
    job: &Job,
    p: &mut Progress,
) -> Result<String, String> {
    use pdl_net::manifest::ChunkVerifier;

    let mut v = ChunkVerifier::new(m, trust);

    p.phase("verifying chunks");
    {
        let mut f = std::fs::File::open(out_path)
            .map_err(|e| format!("cannot reopen {} to verify: {e}", out_path.display()))?;
        v.write_reader(&mut f)
            .map_err(|e| format!("read failed while verifying: {e}"))?;
    }
    p.end_phase();

    if v.all_verified() {
        return Ok(format!("all {} chunks verified", v.verified_count()));
    }

    let bad = v.failed_indices().to_vec();
    p.event(
        0,
        &format!(
            "{} chunk(s) failed verification: {:?} â€?refetching",
            bad.len(),
            bad
        ),
    );

    let sink = Arc::new(
        pdl_net::SparseSink::create(&out_path.to_string_lossy(), v.manifest().object.size)
            .map_err(|e| format!("cannot reopen {} to repair: {e}", out_path.display()))?,
    );

    let mut repaired = 0usize;
    for (nth, idx) in bad.into_iter().enumerate() {
        let (lo, hi) = v.manifest().span(idx);
        // Rotate through the sources, starting past the first. Which host
        // served the corrupt chunk is unknowable from here, so a FIXED
        // alternate is a coin-flip that repeats itself: if the alternate is the
        // bad mirror, every refetch fails and the repair dies on its first
        // candidate. Rotation costs nothing and puts each retry somewhere new.
        let t = usable[(1 + nth) % usable.len()].1.clone();
        p.event(1, &format!("refetching chunk {idx} [{lo},{hi})"));
        pdl_net::fetch_range_retry(
            conn.clone(),
            t,
            lo,
            hi,
            sink.clone(),
            job.tries,
            job.timeout_s,
        )
        .await
        .map_err(|e| format!("chunk {idx} refetch failed: {e}"))?;

        // Re-verify from disk. A refetch that is ALSO corrupt must not be
        // accepted just because it was requested again.
        let mut fresh = vec![0u8; (hi - lo) as usize];
        {
            use std::io::{Read as _, Seek as _, SeekFrom};
            let mut f = std::fs::File::open(out_path).map_err(|e| e.to_string())?;
            f.seek(SeekFrom::Start(lo)).map_err(|e| e.to_string())?;
            f.read_exact(&mut fresh).map_err(|e| e.to_string())?;
        }
        v.retry(idx);
        if !v.write(lo, &fresh).is_empty() {
            return Err(format!(
                "chunk {idx} [{lo},{hi}) still fails its digest after refetch: the source is \
                 serving bytes that do not match the manifest, so the file cannot be completed \
                 correctly from it"
            ));
        }
        repaired += 1;
    }

    Ok(format!(
        "{} chunks verified, {repaired} repaired by targeted refetch",
        v.verified_count()
    ))
}

/// Hash a file in fixed-size chunks.
///
/// # Why not `std::fs::read`
///
/// Reading the whole object into a `Vec` to hash it makes peak memory scale with the
/// object, which is the one thing a downloader must never do: the transfer itself
/// writes at exact offsets and holds no reassembly buffer, so the digest was the only
/// part of the program that could not fetch a file larger than RAM.
///
/// Measured on a 121.7 MiB release asset: buffering the entire file scaled RSS
/// to 127 MB instead of a constant ~11-14 MB. The signature that identified it was
/// that hydra's FAILED runs used 11 MB â€?a failed transfer skips the digest, so the
/// footprint was entirely this function. A 1 GiB download would have needed 1 GiB
/// of resident memory.
///
/// 1 MiB chunks: large enough that syscall overhead is negligible against disk
/// throughput, small enough to stay in L2 and to keep the resident cost constant
/// regardless of object size.
/// Hash a file with any algorithm a mirror list may publish.
///
/// # Why this is not just `sha256_file`
///
/// A Metalink publishes whatever the project's build system produced, and that
/// is frequently not SHA-256: Metalink 3.0 documents in the wild carry MD5 and
/// SHA-1 far more often, sometimes alongside SHA-256 and SHA-512 and sometimes
/// instead of it. Verifying only when the document happens to have published a
/// SHA-256 would silently skip the check on the documents most likely to need
/// it.
///
/// Streamed in fixed chunks for the same reason [`sha256_file`] is: buffering
/// the object to hash it makes peak memory scale with the object, which is the
/// one thing a downloader must never do.
///
/// MD5 and SHA-1 are used here as INTEGRITY checks, not as authentication.
/// Against a transmission fault, a truncating proxy, or a mirror serving a stale
/// build â€?which is what a published digest is actually for â€?they work. Against
/// an adversary who chose the bytes, they do not, and no amount of care at this
/// call site changes that: the digest and the object came down the same wire.
fn digest_file(path: &Path, algo: pdl_net::digest::Algo) -> Option<String> {
    use pdl_net::digest::Algo;
    use std::io::Read;
    // CRC32 is not implemented here: it is an error-detecting code whose
    // collisions are arithmetic, and a "verified" that means that little is
    // worse than an honest "not checked".
    if matches!(algo, Algo::Crc32 | Algo::Crc32c) {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 1 << 20];
    let mut sha256 = Sha256::new();
    let mut sha512 = sha2::Sha512::new();
    let mut sha1 = sha1::Sha1::new();
    let mut md5 = md5::Md5::new();
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => match algo {
                Algo::Sha256 => sha256.update(&buf[..n]),
                Algo::Sha512 => sha512.update(&buf[..n]),
                Algo::Sha1 => sha1.update(&buf[..n]),
                Algo::Md5 => md5.update(&buf[..n]),
                Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
            },
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(pdl_net::digest::to_lower_hex(&match algo {
        Algo::Sha256 => sha256.finalize().to_vec(),
        Algo::Sha512 => sha512.finalize().to_vec(),
        Algo::Sha1 => sha1.finalize().to_vec(),
        Algo::Md5 => md5.finalize().to_vec(),
        Algo::Crc32 | Algo::Crc32c => unreachable!("refused above"),
    }))
}

/// Split an `algo:hex` digest spec, defaulting a bare digest to SHA-256.
///
/// A bare 64-hex value is what `--checksum` has always accepted; anything with a
/// prefix comes from a mirror list and names its own algorithm.
fn parse_digest_spec(spec: &str) -> Option<(pdl_net::digest::Algo, String)> {
    let t = spec.trim().to_ascii_lowercase();
    match t.split_once(':') {
        Some((a, h)) => pdl_net::digest::Algo::parse(a).map(|algo| (algo, h.trim().to_string())),
        None => Some((pdl_net::digest::Algo::Sha256, t)),
    }
}

fn sha256_file(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        match f.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => h.update(&buf[..n]),
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(pdl_net::digest::to_lower_hex(&h.finalize()))
}

pub async fn run(job: Job) -> Outcome {
    let pairs = match targets_for(
        &job.urls,
        &job.headers,
        &job.user_agent,
        job.proxy.as_deref(),
        job.no_proxy,
    ) {
        Ok(v) => v,
        Err(e) => return failed(&job, 0, e),
    };
    let name = job
        .output
        .clone()
        .map(|p| {
            p.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_else(|| pairs[0].0.suggested_filename());
    let mut out_path = job.output.clone().unwrap_or_else(|| PathBuf::from(&name));
    if let Some(dir) = &job.output_dir {
        if out_path.is_relative() {
            out_path = dir.join(&out_path);
        }
    }
    if job.create_dirs {
        if let Some(parent) = out_path.parent() {
            if !parent.as_os_str().is_empty() {
                let _ = std::fs::create_dir_all(parent);
            }
        }
    }
    // --no-clobber must short-circuit BEFORE any request: its purpose is to
    // avoid touching the network at all for a file already present.
    if job.no_clobber && out_path.exists() && !job.resume {
        if !job.quiet {
            eprintln!(
                "playdl: {} already exists; not retrieved (--no-clobber)",
                out_path.display()
            );
        }
        return Outcome::stopped(
            &job,
            out_path.to_string_lossy().to_string(),
            std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0),
            true,
            "skipped: file exists",
        );
    }

    // Reserved on BOTH progress instances (see `progress_for`): this first one
    // covers the setup phase (probe, redirects, concurrency measurement), and
    // its setup line was what prepended 71 bytes to a piped archive.
    let mut p = match progress_for(&job, &name, None) {
        Ok(p) => p,
        Err(e) => return failed(&job, 0, e),
    };
    let t_start = Instant::now();

    // What reading the mirror list revealed. Level 1 because it is real
    // diagnostic detail about a source list the user did not type â€?which
    // mirrors were dropped and why, whether per-chunk verification is available,
    // whether the publisher capped the connection count â€?but not the answer to
    // "did it work", which is the bar and the result line.
    for n in &job.metalink_notes {
        p.event(1, n);
    }

    // One connector for the whole job: it carries the TLS session cache, so the
    // second and later connections to a host skip a full handshake. That directly
    // lowers the per-request setup cost the scheduler is measuring.
    // Resolve the proxy spec once. A SOCKS proxy is configured on the CONNECTOR (it
    // carries the TCP stream); an HTTP proxy is configured on each TARGET (it rewrites
    // the request). Conflating them sends a CONNECT to the origin, or an absolute-form
    // request to a SOCKS port.
    let socks = match &job.proxy {
        Some(raw) => match pdl_net::Proxy::parse(raw) {
            Ok(px) if px.kind.is_socks() => Some(px),
            Ok(_) => None,
            Err(e) => return failed(&job, 0, format!("--proxy: {e}")),
        },
        None => None,
    };
    let conn = match TlsCapableConnector::with_insecure(job.insecure) {
        // A SOCKS proxy belongs on the connector: it carries the TCP stream and never
        // parses HTTP, so every connection this client opens must go through it.
        Ok(c) => Arc::new(
            match socks.clone() {
                Some(px) => {
                    p.event(
                        1,
                        &format!(
                            "routing through {} proxy {}:{}",
                            px.kind.as_str(),
                            px.host,
                            px.port
                        ),
                    );
                    c.with_socks(px)
                }
                None => c,
            }
            .with_family(job.ip_family),
        ),
        Err(e) => return failed(&job, 0, format!("tls setup failed: {e}")),
    };

    // ---- FTP takes a separate path ---------------------------------------
    //
    // Not because the scheduler cannot drive it, but because two of its properties differ
    // in ways that change the right behaviour rather than just the syntax:
    //
    //  * No validator. SIZE+MDTM cannot prove two mirrors serve identical bytes (this
    //    project has already observed HTTP mirrors agreeing on size while serving
    //    different builds), so multi-source assembly is refused rather than attempted.
    //    A file spliced from two versions passes every length check and is silently wrong.
    //  * Preemption costs two control round trips (ABOR+reply, PASV+reply) against zero
    //    for HTTP, because REST names a start and nothing names an end. Reassigning as
    //    eagerly as HTTP would spend the benefit on control traffic.
    //
    // So an FTP fetch is single-source and sequential, with the reason stated.
    if pairs.first().map(|(u, _)| u.is_ftp()).unwrap_or(false) {
        if pairs.len() > 1 {
            p.event(
                0,
                "ftp: using the first source only â€?FTP offers no validator that can prove \
                 two servers hold identical bytes",
            );
        }
        let outs = out_path.to_string_lossy().to_string();
        return ftp_fetch(&job, &pairs[0].0, &mut p, outs).await;
    }

    p.phase("resolving and probing sources");
    // How many mirrors the probe phase must produce before the transfer may
    // start. Every mirror can take at least one connection, so the connection
    // budget is the seat count â€?bounded by the list itself and by politeness.
    // Anything past this only fills the reserve bench, which is why it does not
    // have to be waited for.
    let want_seats = match job.conns {
        Some(n) => n.clamp(1, job.polite.total.max(1)),
        None => mirror_list_width(&job, pairs.len()),
    }
    .min(pairs.len());
    let Probed {
        first: probe_info,
        keep,
        resolved,
        renamed,
        late: late_mirrors,
        first_rtt,
    } = match probe_all(
        &conn,
        &pairs,
        &mut p,
        job.max_redirs,
        job.attested.as_ref().map(|a| a.size),
        want_seats,
    )
    .await
    {
        Ok(v) => v,
        Err(e) => return failed(&job, 0, e),
    };
    // ---- the URL is a MIRROR LIST, not the object -------------------------
    //
    // `https://mirrors.fedoraproject.org/metalink?repo=fedora-40&arch=x86_64` has
    // no extension to read, so nothing before this point could have known. The
    // probe already happened and already carries the `Content-Type`, so asking
    // here costs nothing, while asking earlier would cost a round trip on every
    // download that is not a Metalink.
    //
    // Saving it instead would hand the user a few kilobytes of XML under the
    // name of the multi-gigabyte image they asked for â€?which passes every check
    // this program makes and is entirely the wrong file.
    if job.follow_metalink && job.attested.is_none() && probe_info.serves_metalink() {
        p.event(
            0,
            &format!(
                "{} serves a Metalink document; reading it as a mirror list \
                 (--no-follow-metalink to save it instead)",
                pairs[keep[0]].0.host
            ),
        );
        p.end_phase();
        return match follow_metalink(&job, &pairs[keep[0]].0).await {
            Ok(next) => {
                // Say WHAT the document turned out to describe, at level 0.
                //
                // The user typed one URL and is about to receive a file with a
                // different name and possibly a very different size â€?a
                // repository-metadata redirector and an image redirector look
                // identical on the command line, and only the document knows
                // which one this was. Reporting the mirror list without
                // reporting its contents leaves "why did I get 6 KiB?" as the
                // first thing the user has to work out for themselves.
                p.event(
                    0,
                    &format!(
                        "the document describes {} ({}) on {} mirror(s)",
                        next.output
                            .as_deref()
                            .map(|o| o.to_string_lossy().to_string())
                            .unwrap_or_else(|| "one file".into()),
                        next.attested
                            .as_ref()
                            .map(|a| crate::progress::human(a.size))
                            .unwrap_or_else(|| "size not stated".into()),
                        next.urls.len(),
                    ),
                );
                Box::pin(run(next)).await
            }
            Err(e) => failed(&job, 0, e),
        };
    }
    // The requested URL was a redirector PAGE, so the name taken from it names
    // the stub, not the file â€?`href.li/?â€¦` yields `index.html`. Adopt the name
    // the object actually landed under. Only when the user named nothing: `-O`
    // and `--output-dir`-relative paths are explicit and are never second-guessed.
    let (name, mut out_path) = match renamed.filter(|_| job.output.is_none()) {
        Some(fresh) => {
            p.event(1, &format!("naming the output {fresh} (redirector page)"));
            let path = match &job.output_dir {
                Some(dir) => dir.join(&fresh),
                None => PathBuf::from(&fresh),
            };
            (fresh, path)
        }
        None => (name, out_path),
    };
    // Adopt the post-redirect targets: a release asset commonly redirects to a
    // different host, and fetching from the pre-redirect URL would 302 on every range.
    let mut pairs = pairs;
    for (i, t) in resolved {
        if let Some(slot) = pairs.get_mut(i) {
            slot.1 = t;
        }
    }
    // A HEAD is allowed to omit Content-Length, and CDNs do. Falling back to a
    // one-byte range request is what turns "0 bytes, success" into a real transfer.
    let mut size = probe_info.size;
    let validator = probe_info.validator.clone();
    // Kept separately from `validator`: `--remote-time` needs the DATE, and the
    // validator is whichever of the two headers is better for resume â€?an ETag
    // when the server sent one, which is opaque and carries no time.
    let last_modified = probe_info.last_modified.clone();
    // The server's own Content-Type, used as the weakest of the three
    // classification signals.
    let served_type = job
        .content_type
        .clone()
        .or_else(|| probe_info.content_type.clone());
    let usable: Vec<(Url, Target)> = keep.iter().map(|&i| pairs[i].clone()).collect();
    // `Content-Length: 0` is a SIZE, not a missing size.
    //
    // The two are indistinguishable in `size`, and treating the second as the first
    // fails a download that had already succeeded: a zero-length object (
    // `speedtest.bitel.io/Testdateien/0B`) went through the size fallback, then the
    // unknown-size streaming path, and came out as "the server sent no body" â€?
    // exit code 1 over a correctly written empty file.
    let empty_object = probe_info.stated_length() == Some(0);
    if size == 0 && !job.spider && !empty_object {
        // `keep` holds the indices that probed consistently; any of them can answer.
        match pdl_net::probe_size_via_range(conn.as_ref(), &pairs[keep[0]].1).await {
            Ok(n) if n > 0 => {
                p.event(1, &format!("size from Content-Range: {n} bytes"));
                size = n;
            }
            Ok(_) => {}
            Err(e) => p.event(1, &format!("size fallback failed: {e}")),
        }
    }
    if size == 0 && !job.spider {
        // No knowable size. Multi-source scheduling is impossible â€?with no total there
        // are no ranges to divide â€?but FETCHING is not, which standard HTTP clients
        // do routinely for dynamic pages and `Content-Range: bytes 0-0/*` replies. Degrade to a
        // single sequential stream and say so, rather than failing where other tools succeed.
        if job.server_response {
            // Clear the spinner first: it shares the row and would prefix the request
            // line with a partial frame.
            p.end_phase();
            print_exchange(&probe_info);
        }
        p.event(
            0,
            if empty_object {
                "the object is empty (the server stated Content-Length: 0)"
            } else {
                "size unknown: streaming with one connection (no parallelism, no resume)"
            },
        );
        p.end_phase();
        let outs = out_path.to_string_lossy().to_string();
        let t0 = Instant::now();
        return match pdl_net::fetch_streaming(conn.as_ref(), &usable[0].1, &outs).await {
            // An empty body is the whole object when the server said so, and a
            // failure only when it did not: nothing arrived and nothing said
            // nothing was supposed to.
            Ok(0) if !empty_object => failed(&job, 0, "the server sent no body".into()),
            Ok(n) => {
                let el = t0.elapsed().as_secs_f64();
                p.event(
                    0,
                    &format!("streamed {} in {:.1}s", crate::progress::human(n), el),
                );
                let mut o = Outcome::stopped(
                    &job,
                    out_path.to_string_lossy().to_string(),
                    n,
                    true,
                    "streamed (size was not knowable in advance)",
                );
                // Classification still applies, and matters more here: an unknown-size
                // response is very often HTML where a file was expected.
                // Read the first bytes back for classification; the file is written
                // sequentially here so its head is its head.
                let head = {
                    use std::io::Read;
                    let mut b = vec![0u8; 4096];
                    match std::fs::File::open(&out_path).and_then(|mut f| f.read(&mut b)) {
                        Ok(k) => {
                            b.truncate(k);
                            b
                        }
                        Err(_) => Vec::new(),
                    }
                };
                let det = detect_format(&head, &name, probe_info.content_type.as_deref());
                o.format = det.format.map(|f| f.name.to_string());
                o.category = Some(det.category.as_str().to_string());
                o.format_conflict = det.conflict.clone();
                if !job.quiet {
                    if let Some(c) = &det.conflict {
                        eprintln!("playdl: warning: {c}");
                    }
                    if let Some(f) = det.format {
                        println!("  {}", f.hint());
                    }
                    // An HTML body where a file was expected is the captive-portal /
                    // login-wall case, and on an unknown-size response it is the most
                    // likely outcome of all â€?worth saying plainly.
                    if det.category == Category::Markup && job.output.is_some() {
                        eprintln!(
                            "playdl: note: this is a web page, not a file. If you meant a \
                             release asset, use the download URL rather than the page URL."
                        );
                    }
                }
                o
            }
            Err(e) => failed(&job, 0, format!("streaming fetch failed: {e}")),
        };
    }

    // --spider / -I: report and stop. No body is requested, so this is the safe
    // way to inspect a URL (and what a link checker wants).
    if job.spider {
        // `-S`/`-i` must be honoured here too. The only other place headers are
        // printed is after the transfer, which `--spider` returns before ever
        // reaching â€?so `hydra --spider -S <url>` silently showed no headers,
        // despite being the most natural way to ask for exactly them.
        if job.server_response && !job.quiet {
            p.end_phase();
            print_exchange(&probe_info);
        }
        if !job.quiet {
            println!(
                "{}  {} bytes  ranges={}  validator={}",
                name,
                size,
                if usable.len() > 1 {
                    "yes (multi-source usable)"
                } else {
                    "yes"
                },
                validator.as_deref().unwrap_or("none")
            );
        }
        return Outcome::stopped(&job, String::new(), size, true, "spider: headers only");
    }

    // --max-filesize: refuse before opening a socket for the body, which is the
    // only point at which refusing actually saves anything.
    if let Some(cap) = job.max_filesize {
        if size > cap {
            return failed(
                &job,
                size,
                format!("object is {size} bytes, exceeding --max-filesize {cap}"),
            );
        }
    }

    // --etag-compare: if the stored validator still matches, the object has not
    // changed and there is nothing to retrieve.
    if let Some(path) = &job.etag_compare {
        if let (Ok(stored), Some(current)) = (std::fs::read_to_string(path), validator.as_deref()) {
            if stored.trim() == current.trim() {
                if !job.quiet {
                    eprintln!(
                        "playdl: unchanged (ETag matches {}), not retrieved",
                        path.display()
                    );
                }
                return Outcome::stopped(
                    &job,
                    out_path.to_string_lossy().to_string(),
                    size,
                    true,
                    "unchanged: ETag matches",
                );
            }
        }
    }
    if let (Some(path), Some(v)) = (&job.etag_save, validator.as_deref()) {
        let _ = std::fs::write(path, v);
    }
    // A second Progress now that the size is known. The phase line from the probe
    // stage is cleared first so the two never share a terminal row.
    p.end_phase();
    let mut p = match progress_for(&job, &name, Some(size)) {
        Ok(p) => p,
        Err(e) => return failed(&job, size, e),
    };

    // ---- existing file --------------------------------------------------
    //
    // Four outcomes are possible and none is a safe default for every case, so an
    // interactive run asks. The flags are answers and are never re-asked; a
    // non-interactive run picks the option that cannot destroy data.
    let mut resumed_from = 0u64;
    let mut prior: Option<Sidecar> = None;
    let existing_sidecar = Sidecar::load(&out_path);
    // Bytes verified as a genuine prefix of the remote object, when resuming a file
    // that has no sidecar record.
    let mut adopted_prefix: Option<u64> = None;
    // Bytes ACTUALLY present, not the file's apparent length.
    //
    // The output is a sparse file created at full length before the first byte
    // arrives. Allocated blocks and resume records determine actual progress.
    let on_disk = bytes_present(&out_path, existing_sidecar.as_ref());

    // Where the bytes are going, decided BEFORE anything touches the filesystem.
    //
    // Under `--no-save` this is `Discard`, and the whole existing-file question
    // below becomes moot: a run that will never write cannot clobber, cannot
    // resume, and must not prompt about â€?or rename around â€?a file it is not
    // going to open. The previous implementation created the file, wrote it,
    // hashed it and deleted it at the end, so all of that machinery ran and the
    // bytes sat on disk for the duration.
    let destination = output_target(&job, &out_path.to_string_lossy());
    let discarding = destination == OutputTarget::Discard;

    if out_path.exists() && !job.to_stdout && !discarding {
        let offer = match &existing_sidecar {
            Some(sc) => match sc.can_resume(size, validator.as_deref()) {
                Ok(()) => crate::prompt::ResumeOffer::Sound(sc.bytes_done()),
                Err(why) => crate::prompt::ResumeOffer::Refused(why),
            },
            // No sidecar. That is the ordinary case for a file another tool started, or
            // one hydra was killed during before writing its record, and it is NOT a
            // reason to re-fetch from zero: the bytes can be checked against the server.
            None if !probe_info.ranges => crate::prompt::ResumeOffer::Refused(
                "the server does not support byte ranges, so a partial file cannot be \
                 continued from"
                    .into(),
            ),
            None if on_disk >= size => crate::prompt::ResumeOffer::LooksComplete(on_disk),
            None if on_disk > 0 => crate::prompt::ResumeOffer::Verifiable(on_disk),
            None => crate::prompt::ResumeOffer::Refused("the existing file is empty".into()),
        };
        let flags = crate::prompt::Flags {
            resume: job.resume,
            no_clobber: job.no_clobber,
            force: job.force,
            assume_default: job.quiet,
        };
        p.end_phase();
        let choice = crate::prompt::ask(&out_path, on_disk, size, &offer, flags)
            .unwrap_or(crate::prompt::Existing::Rename);
        match choice {
            crate::prompt::Existing::Skip => {
                return Outcome::stopped(
                    &job,
                    out_path.to_string_lossy().to_string(),
                    on_disk,
                    true,
                    "kept the existing file",
                );
            }
            crate::prompt::Existing::Rename => match crate::prompt::next_free_name(&out_path) {
                Some(fresh) => {
                    p.event(0, &format!("writing to {}", fresh.display()));
                    out_path = fresh;
                }
                None => {
                    return failed(
                        &job,
                        size,
                        "no free filename beside the existing one".into(),
                    )
                }
            },
            crate::prompt::Existing::Restart => {
                Sidecar::remove(&out_path);
                let _ = std::fs::remove_file(&out_path);
            }
            crate::prompt::Existing::Verify => {
                p.phase("verifying the existing file against the server");
                let full = verify_prefix(
                    &conn,
                    &usable[0].1,
                    &out_path,
                    on_disk.min(size),
                    job.tries,
                    job.timeout_s,
                )
                .await;
                p.end_phase();
                return match full {
                    Some(_) if on_disk == size => Outcome::stopped(
                        &job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        true,
                        "already complete: the file matches the server",
                    ),
                    Some(_) => Outcome::stopped(
                        &job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        false,
                        "the file is a valid prefix but is shorter than the object; \
                         re-run with -c to continue it",
                    ),
                    None => Outcome::stopped(
                        &job,
                        out_path.to_string_lossy().to_string(),
                        on_disk,
                        false,
                        "the existing file does NOT match the server",
                    ),
                };
            }
            crate::prompt::Existing::Resume if existing_sidecar.is_none() => {
                // Resuming a file we did not write: prove the prefix first. Without
                // this the transfer would append to bytes of unknown provenance, which
                // is precisely the silent-corruption class this project keeps finding.
                p.phase("checking the existing bytes against the server");
                let v = verify_prefix(
                    &conn,
                    &usable[0].1,
                    &out_path,
                    on_disk,
                    job.tries,
                    job.timeout_s,
                )
                .await;
                p.end_phase();
                match v {
                    Some(n) => {
                        p.event(
                            0,
                            &format!(
                                "verified {} already on disk; continuing from there",
                                crate::progress::human(n)
                            ),
                        );
                        adopted_prefix = Some(n);
                    }
                    None => {
                        p.event(
                            0,
                            "the existing bytes do not match the server; starting over",
                        );
                        let _ = std::fs::remove_file(&out_path);
                    }
                }
            }
            crate::prompt::Existing::Resume => {
                // Fall through: the resume block below picks up the sidecar.
            }
        }
    }

    // ---- resume ---------------------------------------------------------
    if job.resume || existing_sidecar.is_some() {
        if let Some(sc) = Sidecar::load(&out_path) {
            match sc.can_resume(size, validator.as_deref()) {
                Ok(()) => {
                    resumed_from = sc.bytes_done();
                    p.event(
                        0,
                        &format!(
                            "resuming: {} already held",
                            crate::progress::human(resumed_from)
                        ),
                    );
                    prior = Some(sc);
                }
                Err(why) => {
                    p.event(0, &format!("cannot resume ({why}); starting over"));
                    Sidecar::remove(&out_path);
                }
            }
        }
    }

    // ---- the discarding sink, created once -------------------------------
    // Created before the concurrency probe so probe bytes are recorded by the digest sink.
    // A digest is wanted unless the user declined it â€?and `--checksum` or a
    // document's digest is a request for one however they answered, since a
    // verification that cannot run is worse than one that costs a pass.
    let want_digest_value = job.print_checksum
        || job.checksum.is_some()
        || job.attested.as_ref().is_some_and(|a| a.digest.is_some());
    let discard_sink = discarding.then(|| {
        let sk = SparseSink::discarding();
        Arc::new(if want_digest_value {
            sk.with_digest(pdl_net::stream_digest::DEFAULT_REORDER_CAP)
        } else {
            sk
        })
    });

    // ---- concurrency ----------------------------------------------------
    // An explicit `-x N` / `-s N` is an instruction, not a hint. Measuring anyway
    // and then overriding it was a silent no-op: `-x 5` produced ONE connection
    // while the flag's own help says measurement is what happens when you OMIT
    // it. Whoever passes a number has a reason â€?a known-good mirror, a
    // reproduction, a comparison against another client â€?and a measurement that
    // quietly wins makes those impossible and looks like the flag is broken.
    //
    // `--adaptive` remains available to ask for measurement WITH a ceiling; the
    // probe is still what runs when no number is given at all.
    let (n_conns, delta, probe_filled): (usize, f64, Vec<(u64, u64)>) = match job.conns {
        // An explicit `-x N` is honoured as given â€?but `--adaptive` asks for the
        // measurement to run anyway, with N as a CEILING rather than a target.
        //
        // That distinction is what the measured slowdown needed. On four of five
        // live objects a fixed multi-connection setting was slower than a single
        // stream, because the access link saturated at one or two connections and
        // every further connection added setup cost against a capacity that was
        // already spoken for. The search that finds this is `Admission`, and it
        // only ever ran when `-x` was omitted â€?which no benchmark does.
        // `--adaptive` no longer probes. It opens the full connection budget but
        // starts the scheduler with only ONE active, and the in-band ramp
        // (`pdl_core::ramp`) admits the rest while the aggregate rate says they pay
        // for themselves. Measuring on the real transfer rather than on sample
        // transfers is what removes the probe's cost: on a 3.15 MB object over a
        // live path the climbing probe made the transfer 1.96x slower than not
        // probing (18.2 s vs 8.3 s median, paired over 9 interleaved reps,
        // p = 0.004), because the samples are paid for before the transfer starts
        // and every byte they move is re-fetched work.
        Some(n) => (job.polite.allow(n), first_rtt, Vec::new()),
        // `--no-probe` with no `-x` at all: the user has asked not to measure and
        // named no number, so take one connection rather than probing anyway.
        // Guessing a multi-connection default here is what produced transfers
        // slower than a single stream on a saturated link.
        None if !job.probe => (job.polite.allow(1), first_rtt, Vec::new()),
        // A MIRROR LIST answers the question the probe was going to ask.
        //
        // `learn_concurrency` measures marginal goodput against ONE host and
        // returns a per-host connection count. That is the right measurement
        // for one URL and the wrong one for a mirror list: the useful width of
        // a transfer across N independent servers is not a property of any one
        // of them, and the probe cannot see the other N-1. Measured on a real
        // twelve-mirror Fedora document it answered "2", and the transfer took
        // 10.2 s against 5.2 s for the eight the list could actually seat â€?the
        // probe was not merely paid for, it was paid for a worse answer.
        //
        // So with a ranked list in hand the count comes from the list: one
        // connection per usable mirror, under both politeness ceilings. Sizing
        // is still measured where measurement is the only source of truth â€?the
        // scheduler reassigns ranges continuously on observed rate, and a mirror
        // that cannot keep up loses its work whatever this number was.
        None if !job.source_plans.is_empty() && usable.len() > 1 => {
            let n = mirror_list_width(&job, usable.len());
            p.event(
                1,
                &format!(
                    "{n} source(s) from the mirror list; measuring one host's marginal goodput \
                     would not describe them"
                ),
            );
            (n, first_rtt, Vec::new())
        }
        // No `-x` and no mirror list: open what politeness allows.
        //
        // This used to measure first, and the measurement was the slowest answer in
        // every comparison. It sampled 768 KiB per level and timed the whole fetch,
        // so on any path where setup costs more than the sample takes to stream it
        // reported the path's latency as its bandwidth: on a 100 ms link serving
        // 104 MB/s it measured 0.93 MB/s at one connection and 1.05 MB/s at two,
        // called that a 14% gain, and settled on one connection. Measured on that
        // link, the default reached 90 MB/s where four connections reach 224 and
        // eight reach 287 â€?the probe spent 1.5 s to arrive at the worst
        // configuration available, and `curl` beat it without measuring anything.
        //
        // Sampling harder is not the fix. To make streaming dominate setup on that
        // path the sample would have to span tens of megabytes per level, which is
        // a large fraction of the object fetched at a concurrency the search has
        // not yet justified. The in-band ramp behind `--adaptive` exists precisely
        // because measurement belongs on the real transfer, where the bytes count
        // and the window can be as long as it needs to be.
        //
        // So the default is the politeness budget, and the transfer's own machinery
        // handles the paths that will not serve it: an origin that answers `429`
        // lowers the ceiling, one that accepts connections and starves them lowers
        // it too, and the scheduler moves ranges off any connection that lags. What
        // is left is the case this default is right for â€?a path with more capacity
        // than one stream can take â€?and there it is worth 2 to 3x.
        None => (job.polite.allow(job.polite.per_host), first_rtt, Vec::new()),
    };
    // Split the connections across sources under BOTH ceilings: per-host, and the
    // aggregate `--max-total-connections`. The earlier arithmetic divided the
    // count over the sources and rounded UP, which consulted neither the total nor
    // the per-host limit â€?`--max-total-connections 2 -x 8` reported eight
    // connections and opened eight, because nothing in this path ever read
    // `Politeness.total`.
    //
    // Plans for the sources that SURVIVED probing, in the order they survived
    // in. `keep` is a subset of the original URL list, so the ranking has to be
    // carried across by index or a dropped mirror shifts every rank after it â€?
    // the second-best host would be allocated as though it were the fourth.
    let plans: Vec<pdl_core::SourcePlan> = if job.source_plans.is_empty() {
        vec![pdl_core::SourcePlan::default(); usable.len()]
    } else {
        keep.iter()
            .map(|&i| job.source_plans.get(i).copied().unwrap_or_default())
            .collect()
    };
    let split = if job.source_plans.is_empty() {
        job.polite.split(n_conns, usable.len())
    } else {
        job.polite.split_plan(n_conns, &plans)
    };
    // Seated sources are the ones the split gave connections to; the rest are
    // the bench. With an even split the seated set is a prefix, but a ranked
    // allocation can leave a gap â€?a mirror that stated `maxconnections` may be
    // passed over while a lower-ranked one is seated â€?so this filters rather
    // than truncating.
    let seated: Vec<usize> = (0..usable.len()).filter(|&i| split[i] > 0).collect();
    let tgts: Vec<Target> = seated.iter().map(|&i| usable[i].1.clone()).collect();
    let per: Vec<usize> = seated.iter().map(|&i| split[i]).collect();
    // Everything the split did not seat, best-ranked first. This is what makes a
    // nineteen-mirror list worth more than a four-mirror one at four
    // connections: `run_transfer_with_reserves` substitutes from here in place
    // when a source dies, so the socket count stays what politeness authorised.
    let bench_ready: Vec<pdl_net::Reserve> = {
        let mut idx: Vec<usize> = (0..usable.len()).filter(|&i| split[i] == 0).collect();
        idx.sort_by_key(|&i| (plans[i].priority, i));
        idx.into_iter()
            .map(|i| pdl_net::Reserve {
                target: usable[i].1.clone(),
                plan: plans[i],
                host: source_label(&usable[i].0),
            })
            .collect()
    };
    // Mirrors still being probed when the transfer was allowed to start. They
    // arrive as `(index into pairs, target)`; the ranking and the display name
    // live here, so a small bridge turns each into a `Reserve` as it lands.
    // Nothing is awaited â€?the transfer already has its seats.
    let bench_late = late_mirrors.map(|mut rx| {
        let plans_all = job.source_plans.clone();
        let labels: Vec<(String, pdl_core::SourcePlan)> = pairs
            .iter()
            .enumerate()
            .map(|(i, (u, _))| {
                (
                    source_label(u),
                    plans_all.get(i).copied().unwrap_or_default(),
                )
            })
            .collect();
        let (tx, out) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Some((i, target)) = rx.recv().await {
                let Some((host, plan)) = labels.get(i).cloned() else {
                    continue;
                };
                if tx.send(pdl_net::Reserve { target, plan, host }).is_err() {
                    return;
                }
            }
        });
        out
    });
    let seated_plans: Vec<pdl_core::SourcePlan> = seated.iter().map(|&i| plans[i]).collect();
    // What the transfer will actually open, which is what `--json.connections`
    // must report: a number the run did not use is not a measurement.
    let n_conns: usize = per.iter().sum();
    // Level 1, not 0: at default verbosity the useful signal is the progress bar
    // and the one-line result. How many sources were chosen and what the measured
    // setup cost was are diagnostics â€?real ones, but the answer to "did it work"
    // should not be preceded by two lines of internals.
    p.event(
        1,
        &format!(
            "{} source(s), {} connection(s) total, delta ~{:.3}s",
            tgts.len(),
            n_conns,
            delta
        ),
    );

    // Where the size and digest being trusted came from. A verification result
    // is only as meaningful as its source, and a user reading "checksum OK"
    // should be able to find out which document said so.
    if let Some(a) = &job.attested {
        p.event(
            1,
            &format!(
                "attested by {}: {} bytes, {}",
                a.origin,
                a.size,
                a.digest.as_deref().unwrap_or("no digest published")
            ),
        );
    }

    let sources: Vec<Source> = seated_plans
        .iter()
        .map(|plan| Source {
            gamma_est: 1.0e6,
            delta_est: delta.max(1e-3),
            // The publisher's ranking, used once â€?for the first split, before
            // anything has been measured. See `pdl_core::sched::Source::priority`
            // for why it is deliberately not consulted again.
            priority: plan.priority,
            ..Default::default()
        })
        .collect();
    // Which mirror is which. Without this a multi-source run shows four
    // connection rows and no way to tell which host a rank was assigned to, so a
    // "mirror 2 is slow" observation cannot be turned into a URL to check.
    if !job.source_plans.is_empty() {
        for (k, &i) in seated.iter().enumerate() {
            p.event(
                1,
                &format!(
                    "source {k}: rank {} {} â€?{} connection(s){}",
                    plans[i].priority,
                    source_label(&usable[i].0),
                    split[i],
                    plans[i]
                        .max_connections
                        .map(|n| format!(", mirror states a ceiling of {n}"))
                        .unwrap_or_default(),
                ),
            );
            p.event(2, &format!("  {}", usable[i].0));
        }
    }
    if !bench_ready.is_empty() {
        p.event(
            1,
            &format!(
                "{} reserve mirror(s) held back; a source that fails is replaced rather than \
                 stranding its range",
                bench_ready.len()
            ),
        );
        // The whole bench at -vv: when a substitution happens, the next question
        // is always "to what", and the answer is deterministic and knowable now.
        for (k, r) in bench_ready.iter().enumerate() {
            p.event(
                2,
                &format!("  reserve {k}: rank {} {}", r.plan.priority, r.host),
            );
        }
    }
    let bench = pdl_net::Bench {
        ready: bench_ready,
        late: bench_late,
    };
    // --range: schedule only the requested interval. Resolving it here rather
    // than in the argument parser is what lets a suffix range like `-512` mean
    // "the last 512 bytes" â€?that needs the object size, known only now.
    let (want_lo, want_hi) = match job.range {
        None => (0u64, size),
        Some(spec) => match spec.resolve(size) {
            Some(r) => r,
            None => {
                return failed(
                    &job,
                    size,
                    format!("{spec:?} is empty for a {size}-byte object"),
                )
            }
        },
    };
    let partial = (want_lo, want_hi) != (0, size);
    if partial {
        p.event(
            0,
            &format!(
                "range mode: bytes {}-{} of {} ({})",
                want_lo,
                want_hi - 1,
                size,
                crate::progress::human(want_hi - want_lo)
            ),
        );
    }

    let t_transfer = Instant::now();
    let mut sched =
        Scheduler::new(size, sources, &per).with_stall_timeout((12.0 * delta).clamp(4.0, 45.0));
    // `--adaptive`: open the budget but start with one connection active and let the
    // in-band ramp earn the rest. Without this the scheduler runs every connection
    // from the first tick, which is the fixed-concurrency behaviour `-x` already
    // provides.
    if job.adaptive && n_conns > 1 {
        sched.set_active_limit(1);
    }
    // Everything outside the requested range is marked held so the scheduler
    // never issues a request for it.
    if partial {
        sched.mark_done(0, want_lo);
        sched.mark_done(want_hi, size);
    }
    // Pre-mark resumed ranges as held so they are never re-fetched.
    if let Some(sc) = &prior {
        for (lo, hi) in &sc.done {
            sched.mark_done(*lo, *hi);
        }
    }
    // A verified prefix from a sidecar-less file: mark it held so it is never
    // re-fetched. This is the whole point of verifying it.
    if let Some(n) = adopted_prefix {
        let n = n.min(want_hi);
        if n > want_lo {
            sched.mark_done(want_lo, n);
            resumed_from = n;
        }
    }
    // The concurrency probe already fetched these into the real output. Marking them
    // held is what makes the probe free rather than wasted: without this the scheduler
    // would re-request bytes that are already on disk.
    for (lo, hi) in &probe_filled {
        if *lo >= want_lo && *hi <= want_hi {
            sched.mark_done(*lo, *hi);
        }
    }

    p.set_baseline(resumed_from);
    // Live byte accounting, sampled from the scheduler as the transfer runs.
    //
    // The post-transfer completeness check needs to know what is present AFTER
    // the transfer, and every cheap way of asking that question afterwards lies
    // in some case: the pre-transfer sidecar is stale by construction, a file's
    // apparent length is the whole object from the first byte because the output
    // is sparse, and allocated blocks read as the full size on a filesystem that
    // does not do sparse files. The scheduler is the one component that knows
    // which bytes actually arrived, so its own count is carried out of the
    // transfer here rather than re-derived from a record written before it.
    //
    // Seeded from the scheduler's state as it stands right now â€?after every
    // `mark_done` above â€?because a transfer with nothing left to fetch completes
    // before the first observation tick and never updates this at all.
    let progress = Arc::new(std::sync::atomic::AtomicU64::new(sched.bytes_held()));
    let progress_obs = progress.clone();

    // Peak concurrency the transfer actually ran at, sampled from the scheduler.
    // Seeded from its current active limit so a transfer that completes before the
    // first observation tick still reports a real number.
    let used_conns = Arc::new(std::sync::atomic::AtomicUsize::new(
        sched.active_limit().min(sched.n_conns()),
    ));
    let used_conns_obs = used_conns.clone();
    // The limit the transfer FINISHED at, as opposed to the peak it explored.
    let settled_conns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let settled_conns_obs = settled_conns.clone();
    // The most connections that ever actually held a range at once, which is not the
    // same as the limit that permitted them.
    let peak_busy = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let peak_busy_obs = peak_busy.clone();
    // Busy-connection-seconds, in micro-units so the accumulator can be atomic.
    let conn_secs = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let conn_secs_obs = conn_secs.clone();
    let conn_secs_last = Arc::new(std::sync::Mutex::new(std::time::Instant::now()));

    // Requests the scheduler actually issued, sampled live. Needed because a failed
    // transfer returns no count, and reporting zero there printed a request count
    // the run did not measure.
    let observed_requests = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let observed_requests_obs = observed_requests.clone();

    // Checkpoint what is already held BEFORE the first byte of the transfer.
    //
    // The periodic checkpoint inside the render closure only fires once 2 seconds
    // have passed, so a ^C during the concurrency probe â€?or within 2s of the
    // transfer starting â€?left no sidecar at all, and the next `-c` restarted from
    // zero. The probe's bytes are real: it fetches at true offsets into the real
    // output and the scheduler marks those ranges held, which is what makes the
    // probe free rather than wasted. Discarding them on an interrupt throws away
    // a round trip the user already paid for.
    //
    // Written here rather than inside the probe because this is the first point
    // where the probe's ranges, a resume record, and `--range` have all been
    // folded into one authority on what is held.
    if !discarding {
        let held = sched.held_ranges();
        if !held.is_empty() && held != vec![(0, size)] {
            let rec = Sidecar {
                size,
                validator: validator.clone(),
                done: held,
                url: job.urls[0].clone(),
            };
            let _ = rec.save(&out_path);
        }
    }
    let limiter = Arc::new(if job.limit_rate > 0 {
        RateLimiter::new(job.limit_rate)
    } else {
        RateLimiter::unlimited()
    });
    let outs = out_path.to_string_lossy().to_string();
    let c = conn.clone();
    // Host names for the progress view, one per SOURCE â€?behind a lock because a
    // reserve substitution changes them mid-transfer. A view that keeps naming
    // the dead mirror is worse than one naming none: it attributes the
    // replacement's throughput to a machine that is not serving it.
    let hosts: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(
        seated.iter().map(|&i| source_label(&usable[i].0)).collect(),
    ));
    // Digest, head bytes and any unavailability reason gathered from the stream,
    // for the `--no-save` path that has no file to read them back from.
    let mut stream_result: Option<(Option<String>, Vec<u8>, Option<String>)> = None;
    // The SHA-256 of a saved file, hashed as the bytes landed rather than by
    // reading the finished file back.
    //
    // Measured on a loopback origin, 512 MiB: the transfer itself took 0.07 s,
    // the post-download hash 0.19-0.26 s, and the whole-file re-read the rest of
    // a 0.37 s run â€?hydra spent three times longer hashing than downloading,
    // and curl spends nothing. On a wide-area transfer the re-read is a cold read
    // of a multi-gigabyte object from disk after the bar has already reached
    // 100%. Hashing in-band overlaps with waiting for the network, so on any
    // network-bound transfer the digest becomes free.
    //
    // Only a single-connection, from-zero transfer can be hashed this way: the
    // stream digest hashes the contiguous prefix, and a parallel transfer opens
    // with one span per connection, so the second span starts at `size/n` and
    // the reorder buffer would need most of the object (see `stream_digest`).
    // Anything else, and anything the in-band hash could not finish, falls back
    // to hashing the file â€?the same value, at the old cost.
    let mut file_stream_sha256: Option<String> = None;
    let res = {
        // Clear the phase line before the first frame: they share a terminal row.
        p.end_phase();
        p.set_baseline(resumed_from);
        // Checkpoint the resume record AS THE TRANSFER RUNS, not only at the end.
        //
        // Writing it only on exit means a ^C or a crash leaves no record, so the next
        // run cannot tell which bytes are held â€?and because the output is a SPARSE
        // file created at full length from the start, its apparent size is the whole
        // object even when almost nothing has arrived. Together those two facts made an
        // interrupted 2 MB transfer look like a finished 121.7 MiB download. The record
        // carries explicit ranges rather than a byte count, because positioned writes
        // land out of order and a count cannot describe a hole.
        let tick_sink = job.ticks.clone();
        // Substitutions, published by the transport's callback and drained by the
        // renderer on its next tick.
        //
        // The callback cannot report them itself: `Progress` is owned by the
        // render closure for the duration of the transfer, and two closures
        // cannot hold it at once. Draining here rather than after the transfer is
        // what makes the message arrive when the mirror changed â€?reported at the
        // end it reads as a summary of something that is over, when in fact it is
        // the reason the next ten minutes look different from the last.
        let swaps: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let swaps_r = swaps.clone();
        let mut last_ckpt = Instant::now();
        let ckpt_path = out_path.clone();
        let ckpt_size = size;
        let ckpt_validator = validator.clone();
        let ckpt_url = job.urls[0].clone();
        // The last concurrency decision reported, so each is printed once.
        let mut last_reason = pdl_core::LimitReason::None;
        let mut render = |sc: &Scheduler, done: u64| {
            for line in swaps_r.lock().unwrap().drain(..) {
                p.event(0, &line);
            }
            // The transport's concurrency decisions, as they are made. Without
            // this the only account of the adaptive search was `HYDRA_RAMP_TRACE`,
            // and a run that settled at one connection looked like seven dropped
            // ones.
            let reason = sc.limit_reason();
            if reason != last_reason {
                last_reason = reason;
                use pdl_core::LimitReason as R;
                let human_rate = |r: f64| format!("{}/s", crate::progress::human(r as u64));
                match reason {
                    R::Measured {
                        chosen,
                        chosen_rate,
                        tried,
                        tried_rate,
                        // A rate of zero was never measured; see the GUI's
                        // `describe_limit` for the same guard.
                    } if tried != chosen && tried_rate > 0.0 && chosen_rate > 0.0 => p.event(
                        1,
                        &format!(
                            "adaptive: measured {tried} connections at {} against {chosen} \
                             at {}; using {chosen}",
                            human_rate(tried_rate),
                            human_rate(chosen_rate)
                        ),
                    ),
                    R::Measured { chosen, .. } if chosen < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "adaptive: {chosen} of {} connections pay for themselves",
                            sc.n_conns()
                        ),
                    ),
                    R::Refused { serving } if serving < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "origin refuses more than {serving} connection(s) at once; \
                             using {serving}"
                        ),
                    ),
                    R::Starved { serving } if serving < sc.n_conns() => p.event(
                        1,
                        &format!(
                            "origin serves {serving} connection(s) at once and starves the \
                             rest; using {serving}"
                        ),
                    ),
                    _ => {}
                }
            }
            // Carry the scheduler's own count out to the completeness check. A
            // monotonic max rather than a plain store: the observer is called on
            // every tick and the last tick before completion is not necessarily
            // the highest, so a bare store can report less than actually arrived.
            progress_obs.fetch_max(sc.bytes_held(), std::sync::atomic::Ordering::Relaxed);
            // The concurrency the transfer ACTUALLY reached. Under `--adaptive` the
            // in-band ramp starts at one connection and admits more only while they
            // pay, so the budget is a ceiling and reporting it would be reporting a
            // number the run did not use â€?the same defect that made
            // `--max-total-connections 2 -x 8` claim eight.
            observed_requests_obs
                .fetch_max(sc.stats.requests, std::sync::atomic::Ordering::Relaxed);
            // FOUR different numbers, and conflating any two of them has already cost a
            // wrong diagnosis. The distinctions are not pedantic:
            //
            //  * `peak_connections` â€?highest admission limit ever set. Under
            //    `--adaptive` this is the top of the SEARCH, not its answer: the ramp
            //    raises the limit to measure a level and lowers it when the level does
            //    not pay. Reporting this as "connections" made a correctly-settled
            //    search look like a runaway (traced windows showed it stopping at 2
            //    while the summary said 4) and sent the investigation after the ramp
            //    logic twice.
            //  * `settled_connections` â€?the admission limit at the end. This is a
            //    POLICY number: it says what the scheduler was willing to run, not what
            //    it did run.
            //  * `peak_busy_connections` â€?the most connections that ever actually held
            //    a range at once. This differs from the limit in both directions:
            //    lowering the limit lets an already-busy connection finish its range and
            //    go quiet, so real concurrency lags the limit downward; and a connection
            //    can be idle between ranges, so it lags upward too.
            //  * `connection_seconds` â€?the integral of busy connections over time,
            //    which is the only one of the four that summarises the WHOLE run rather
            //    than a moment or a bound. For research this is the number that belongs
            //    next to a throughput figure.
            let limit_now = sc.active_limit().min(sc.n_conns());
            used_conns_obs.fetch_max(limit_now, std::sync::atomic::Ordering::Relaxed);
            settled_conns_obs.store(limit_now, std::sync::atomic::Ordering::Relaxed);
            let busy_now = sc.busy_conns();
            peak_busy_obs.fetch_max(busy_now, std::sync::atomic::Ordering::Relaxed);
            // Rectangle rule over the observer's own interval. The observer runs on the
            // transfer's tick, so the interval is the tick period; deriving it from the
            // clock rather than assuming a constant keeps the integral honest if the
            // tick is ever retimed or a tick is late.
            {
                let mut last = conn_secs_last.lock().unwrap();
                let now = std::time::Instant::now();
                let dt = now.duration_since(*last).as_secs_f64();
                *last = now;
                // Stored as micro-connection-seconds in an integer so the accumulator
                // needs no lock of its own and cannot drift on float addition order.
                conn_secs_obs.fetch_add(
                    (busy_now as f64 * dt * 1e6) as u64,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            // No resume record under `--no-save`: there is no file to resume INTO,
            // so a sidecar would be a stray file created by the flag that promises
            // to create none. (Caught end-to-end, not by a unit test: the periodic
            // checkpoint runs inside this closure and is separate from the
            // completion path.)
            if !discarding && last_ckpt.elapsed().as_secs_f64() >= 2.0 {
                last_ckpt = Instant::now();
                let held = sc.held_ranges();
                if !held.is_empty() {
                    let rec = Sidecar {
                        size: ckpt_size,
                        validator: ckpt_validator.clone(),
                        done: held,
                        url: ckpt_url.clone(),
                    };
                    let _ = rec.save(&ckpt_path);
                }
            }
            let views = {
                let names = hosts.lock().unwrap();
                conn_views(sc, &names)
            };
            // Publish live state for any UI driving this job. Done here rather than in
            // the transport because this closure already has the scheduler, so there is
            // no extra plumbing and no extra tick rate to reconcile.
            if let Some((id, tx)) = &tick_sink {
                let _ = tx.send(Tick {
                    id: *id,
                    done,
                    size: Some(size),
                    rate: views.iter().map(|v| v.rate).sum(),
                    requests: sc.stats.requests,
                    repairs: sc.stats.repairs,
                    conns: views
                        .iter()
                        .map(|v| {
                            let (lo, hi, pos) = v.range.unwrap_or((0, 0, 0));
                            ConnLine {
                                host: v.host.clone(),
                                lo,
                                hi,
                                pos,
                                rate: v.rate,
                                health: format!("{:?}", v.health).to_lowercase(),
                            }
                        })
                        .collect(),
                });
            }
            p.draw(
                done,
                &views,
                Counters {
                    requests: sc.stats.requests,
                    repairs: sc.stats.repairs,
                    reclaims: sc.stats.reclaims,
                    wasted: 0,
                    retries: 0,
                },
            );
        };
        // Under `--no-save` the sink stores nothing and carries the digest
        // instead, so the size, checksum, and format classification are all
        // computed from the STREAM. There is no file at any point.
        let sink = discard_sink.clone();
        // One limiter for the whole transfer, however many connections it opens:
        // `--limit-rate 1M` means the transfer uses 1 MB/s, not 1 MB/s per
        // connection. `--no-save` is capped too â€?the bytes still cross the
        // network, which is what the flag is about.
        let pace = pdl_net::polite::Pace::shared(limiter.clone());
        // Keep the progress view honest across a substitution, and say so at
        // level 0: a mirror changing under a running transfer is exactly the
        // kind of thing a user wants in the log when they come back to a
        // download that took longer than expected.
        let sub_hosts = hosts.clone();
        let sub_note_w = swaps.clone();
        let mut on_sub = move |src: usize, r: &pdl_net::Reserve| {
            let mut names = sub_hosts.lock().unwrap();
            let was = names.get(src).cloned().unwrap_or_else(|| "?".into());
            if let Some(slot) = names.get_mut(src) {
                slot.clone_from(&r.host);
            }
            sub_note_w.lock().unwrap().push(format!(
                "{was} failed; switched to reserve mirror {}",
                r.host
            ));
        };
        // One transfer entry point for both paths. `--no-save`'s discarding sink
        // and the ordinary sparse file differ only in where the bytes land, and
        // splitting the call in two meant the reserve bench had to be threaded
        // through twice â€?so the sink is chosen here and the call is made once.
        // Hash in-band only where the stream digest can finish (one connection,
        // starting from zero, the whole object) â€?see `file_stream_sha256`.
        let hash_in_band = want_digest_value
            && !discarding
            && resumed_from == 0
            && job.range.is_none()
            && per.iter().sum::<usize>() == 1;
        let sk = match &sink {
            Some(sk) => sk.clone(),
            None => match SparseSink::create(&outs, size) {
                Ok(sk) if hash_in_band => {
                    Arc::new(sk.with_digest(pdl_net::stream_digest::DEFAULT_REORDER_CAP))
                }
                Ok(sk) => Arc::new(sk),
                Err(e) => return failed(&job, size, format!("cannot create {outs}: {e}")),
            },
        };
        let file_sink = sink.is_none().then(|| sk.clone());
        let r = pdl_net::run_transfer_with_reserves(
            c,
            tgts,
            &per,
            size,
            sk,
            sched,
            20,
            &mut render,
            pace,
            None,
            bench,
            Some(&mut on_sub),
        )
        .await;
        if let Some(discarding_sink) = &sink {
            stream_result = discarding_sink.take_digest(size);
        }
        if let Some(fs) = &file_sink {
            // `None` when no digest was attached or it could not finish; either
            // way the file is hashed afterwards, so nothing is lost but time.
            file_stream_sha256 = fs.take_digest(size).and_then(|(d, _, _)| d);
        }
        r
    };
    // Two clocks, both reported. `elapsed` is the whole invocation; `transfer_elapsed`
    // is what the progress bar measured. Conflating them made a 1.7s transfer report
    // 4.0s with a throughput the network never delivered.
    let transfer_elapsed = t_transfer.elapsed().as_secs_f64();
    let elapsed = t_start.elapsed().as_secs_f64();
    drop(limiter);

    // Report WHY a transfer failed, and stop claiming it made no requests.
    //
    // `Err(_) => (false, 0)` discarded both the error and the request count, so a
    // failed 88 MB transfer printed "83.6 MiB in 2m52s (496.7 KiB/s), 0 requests"
    // â€?a byte count that contradicted the byte-complete file on disk, a rate
    // derived from it, and a request count that was not measured but invented. The
    // "0 requests" was the only clue that the number was fabricated rather than
    // observed, and it took a reproduction on a real 88 MB object to notice.
    //
    // The transfer's own error is the one piece of evidence about what went wrong,
    // and it was being thrown away at the only point that had it.
    let transfer_error: Option<String> = res.as_ref().err().map(|e| e.to_string());
    let (ok, requests) = match &res {
        Ok((_, r)) => (true, *r),
        // The request count is a property of what the scheduler DID, not of
        // whether it finished. Sampled from the observer, which ran on every tick.
        Err(_) => (
            false,
            observed_requests.load(std::sync::atomic::Ordering::Relaxed),
        ),
    };
    if let Some(why) = transfer_error.as_deref() {
        if !job.quiet || job.show_error {
            eprintln!("playdl: transfer error: {why}");
        }
    }
    let counters = Counters {
        requests,
        ..Default::default()
    };

    // ---- verify ---------------------------------------------------------
    // Bytes ACTUALLY present, measured from the transfer that just ran.
    //
    // Every cheaper way of asking this question afterwards lies in some case.
    // The file's apparent length is the whole object from the very first byte,
    // because the output is a sparse file created at full length â€?using it here
    // once made an interrupted 2 MB transfer report "121.7 MiB on disk" and offer
    // to skip a download that had barely begun. Allocated blocks fix that but
    // read as the full size on a filesystem that does not store holes. And the
    // sidecar loaded BEFORE the transfer is stale by construction: reusing it
    // here reported a byte-exact `-c` resume as a failure (exit 1, `ok: false`,
    // `size` equal to the pre-resume byte count) while reporting a genuinely
    // incomplete resume the same way, so the tool's own output could not tell
    // the two apart.
    //
    // The scheduler is the one component that knows which bytes arrived, so its
    // live count â€?sampled through the observer above and seeded from its state
    // before the transfer â€?is what completeness is judged on. That count is on
    // the OBJECT's coordinate scale, which is the scale the comparison below
    // wants: in range mode the spans outside `[want_lo, want_hi)` are marked held
    // precisely so they are never requested, and they must keep counting as
    // present or a satisfied range would read as incomplete.
    //
    // With `--no-save` there is no file to measure, so the transfer's own success
    // is the only evidence â€?which is exactly what it should be, since the bytes
    // were verified as they streamed.
    let on_disk = if discarding {
        if ok {
            want_hi.min(size)
        } else {
            0
        }
    } else {
        progress.load(std::sync::atomic::Ordering::Relaxed)
    };
    // In range mode the sparse file is still `size` long but only the requested
    // span was fetched, so completion is judged on the scheduler's own accounting
    // rather than on file length.
    let complete = ok && on_disk >= want_hi.min(size);

    // ---- range mode: deliver the requested span, not the object's extent ----
    //
    // Positioned writes need a file of the OBJECT's length to write into, because
    // a range lands at its true offset. In range mode only `[want_lo, want_hi)`
    // is ever fetched, so the rest of that extent is a hole â€?and a hole reads
    // back as zeros. `hydra -r 0-1023` delivered a 34 041-byte file whose first
    // 1 024 bytes were correct and whose remaining 33 017 were zeros: right
    // prefix, plausible size, silently wrong file. The same failure shape as the
    // sparse-file and truncating-reopen bugs recorded elsewhere in this project.
    //
    // Byte-range retrieval should write only the requested bytes, and that is the only reading of
    // "retrieve only this byte range" that makes sense. A suffix range must give
    // a 512-byte file, not a 34 041-byte file whose last 512 bytes are real.
    //
    // Done BEFORE the digest deliberately: a digest must describe the bytes the
    // user receives. Hashing the padded extent would report a checksum for a file
    // that no longer exists after truncation.
    if partial && complete && !discarding && !job.to_stdout {
        if let Err(e) = extract_span(&out_path, want_lo, want_hi) {
            return failed(
                &job,
                size,
                format!(
                    "range mode: cannot reduce {} to its span: {e}",
                    out_path.display()
                ),
            );
        }
    }

    // What the user was handed. In range mode that is the span, not the object:
    // reporting "33.2 KiB in 1.2s" for a 1 KiB range described a transfer that
    // did not happen, and the throughput derived from it was wrong by the same
    // factor.
    let delivered = if partial {
        want_hi.saturating_sub(want_lo)
    } else {
        on_disk
    };

    // ---- per-chunk verification and targeted refetch ---------------------
    //
    // Runs after the transfer rather than during it because a chunk is only
    // checkable once its last byte has landed, and the scheduler may deliver any
    // part of any chunk on any connection. Verifying here costs one sequential
    // read of a file already in page cache, and it buys the thing the whole-file
    // digest cannot: WHICH chunk is wrong.
    //
    // A mismatch is repaired by refetching that chunk alone â€?preferring a
    // different source than the one that served it, since a mirror that served
    // corrupt bytes once is the least likely to serve them correctly now. This is
    // what BitTorrent and Metalink already do, and while the source is reachable
    // it beats carrying parity by roughly 400x.
    let mut chunk_report: Option<String> = None;
    if let Some(mpath) = job.chunk_digests.clone() {
        if complete && !discarding {
            match verify_and_repair_chunks(&conn, &usable, &out_path, &mpath, &job, &mut p).await {
                Ok(r) => chunk_report = Some(r),
                Err(e) => return failed(&job, size, e),
            }
        }
    } else if let Some(pieces) = job.attested.as_ref().and_then(|a| a.pieces.clone()) {
        // The document's `<pieces>`. This is the whole reason a mirror list is
        // worth more than a list of URLs at the moment something goes wrong: a
        // corrupt chunk costs one chunk refetched from a DIFFERENT mirror, not a
        // whole re-download, and the manifest says which chunk.
        //
        // `Advertised`, not `Trusted`, however the document arrived. Nothing here
        // has authenticated it â€?the `<signature>` is recorded and not verified â€?
        // so it may detect a bad chunk and drive a refetch, both of which are
        // self-correcting (the refetched bytes are checked against the same
        // digest), and may not name erasure positions for a parity decode, which
        // is not.
        if complete && !discarding {
            match verify_and_repair(
                &conn,
                &usable,
                &out_path,
                pieces,
                pdl_net::manifest::Trust::Advertised,
                &job,
                &mut p,
            )
            .await
            {
                Ok(r) => chunk_report = Some(r),
                Err(e) => return failed(&job, size, e),
            }
        }
    }

    let digest = if !complete {
        None
    } else if discarding {
        // Computed from the stream as it passed through the discarding sink.
        // `None` here means the digest genuinely could not be established (the
        // ranges arrived too far out of order to hash within the buffer budget),
        // and the reason is reported rather than a wrong value substituted.
        let d = stream_result.as_ref().and_then(|(d, _, _)| d.clone());
        if d.is_none() {
            if let Some(reason) = stream_result.as_ref().and_then(|(_, _, r)| r.clone()) {
                if !job.quiet {
                    eprintln!("playdl: {reason}");
                }
            }
        }
        d
    } else {
        // Chunk verification may have refetched and rewritten spans after the
        // transfer, so a digest taken during it no longer describes the file.
        file_stream_sha256
            .take()
            .filter(|_| chunk_report.is_none())
            // The whole-file pass is the expensive one: on a CPU without the SHA
            // extensions it is several seconds per gigabyte, and `--no-checksum`
            // exists to decline exactly that.
            .or_else(|| want_digest_value.then(|| sha256_file(&out_path)).flatten())
    };
    // What must the bytes hash to?
    //
    // `--checksum` first: a digest the user typed came from somewhere they chose
    // â€?a release page, a signed announcement, a colleague â€?and it outranks one
    // that arrived over the same session as the mirror list. The document's is
    // the fallback, and it is the common case, because nobody types a SHA-512 by
    // hand for a file they are about to download from nineteen mirrors.
    let want_digest: Option<String> = job
        .checksum
        .clone()
        .or_else(|| job.attested.as_ref().and_then(|a| a.digest.clone()));
    let checksum_ok = match (&want_digest, complete) {
        // Nothing to check against, or an incomplete file: an object that did
        // not arrive cannot match a digest, and saying `None` there would report
        // "not checked" about a file that is definitively wrong.
        (None, _) => None,
        (Some(_), false) => Some(false),
        (Some(spec), true) => match parse_digest_spec(spec) {
            None => {
                if !job.quiet {
                    eprintln!("playdl: cannot check {spec:?}: unknown digest algorithm");
                }
                None
            }
            Some((algo, want)) => {
                // The SHA-256 is already computed above for the report, so the
                // common case costs no second pass over the file.
                let got = match (algo, &digest) {
                    (pdl_net::digest::Algo::Sha256, Some(d)) => Some(d.clone()),
                    _ if discarding => {
                        // Nothing was written, so there is no file to re-read.
                        // The stream digest is SHA-256 only, so any other
                        // algorithm genuinely cannot be checked â€?which is worth
                        // saying rather than silently reporting "verified".
                        if !job.quiet {
                            eprintln!(
                                "playdl: --no-save keeps no file, so the {} digest could not be checked (sha256 is computed from the stream)",
                                algo.as_str()
                            );
                        }
                        None
                    }
                    _ => digest_file(&out_path, algo),
                };
                got.map(|g| g == want)
            }
        },
    };

    // ---- classify what actually arrived --------------------------------
    //
    // Classification happens AFTER the transfer because the payload is the only
    // trustworthy signal, and it is read from the head of the finished file
    // rather than from a separate probe request.
    let detection = {
        let mut head = vec![0u8; 0];
        if discarding {
            // The leading bytes were retained by the stream observer; there is no
            // file to reopen. Classification is therefore unchanged by --no-save,
            // which is the point of the flag: probe what a URL serves without
            // leaving anything behind.
            if let Some((_, h, _)) = &stream_result {
                head = h.clone();
            }
        } else if complete {
            use std::io::Read as _;
            if let Ok(mut fh) = std::fs::File::open(&out_path) {
                let mut buf = vec![0u8; 8192];
                if let Ok(n) = fh.read(&mut buf) {
                    buf.truncate(n);
                    head = buf;
                }
            }
        }
        detect_format(&head, &name, served_type.as_deref())
    };
    if let Some(msg) = &detection.conflict {
        // Worth saying even though nothing failed: an HTML body delivered where an
        // archive was expected is the signature of a captive portal or an error
        // page saved as a file, and both the byte count and the status look fine.
        if !job.quiet {
            eprintln!("playdl: warning: {msg}");
            if detection.looks_intercepted() {
                eprintln!(
                    "  the saved file is a web page, not the object requested \
                     (captive portal, login wall, or an error page served with status 200)"
                );
            }
        }
    } else if let Some(f) = detection.format {
        // At -v, name the format. At -vv, explain it: the description is aimed at
        // someone deciding what to do with the file they just fetched.
        p.event(1, &format!("detected {} ({})", f.name, f.category.as_str()));
        p.event(2, &format!("  {}", f.hint()));
    }

    // ---- sort into a category directory, if asked -----------------------
    let out_path = if job.sort_by_type && complete && detection.category != Category::Unknown {
        let base = job.output_dir.clone().unwrap_or_else(|| PathBuf::from("."));
        let dir = base.join(detection.category.directory());
        match std::fs::create_dir_all(&dir) {
            Ok(()) => {
                let dest = dir.join(out_path.file_name().unwrap_or_default());
                match std::fs::rename(&out_path, &dest) {
                    Ok(()) => {
                        p.event(0, &format!("sorted into {}", dir.display()));
                        dest
                    }
                    Err(e) => {
                        eprintln!("playdl: could not move into {}: {e}", dir.display());
                        out_path
                    }
                }
            }
            Err(e) => {
                eprintln!("playdl: could not create {}: {e}", dir.display());
                out_path
            }
        }
    } else {
        out_path
    };

    if job.server_response {
        p.end_phase();
        // Verbatim first, interpretation second. Print raw headers as received; the
        // paraphrase below is a convenience, and when the two disagree the raw block
        // is the evidence.
        print_exchange(&probe_info);
        println!("playdl: interpretation");
        println!("  size: {size}");
        println!("  validator: {}", validator.as_deref().unwrap_or("none"));
        println!(
            "  validator strength: {}",
            if probe_info.weak_validator {
                "weak (single-source only: a weak validator may compare equal across \
                 different bytes, so cross-mirror assembly is not sound)"
            } else if validator.is_some() {
                "strong (cross-mirror assembly permitted)"
            } else {
                "none"
            }
        );
        println!(
            "  range support: {}",
            if probe_info.ranges { "yes" } else { "no" }
        );
        println!("  sources accepted: {}", usable.len());
        println!("  connections used: {n_conns}");
    }

    if discarding {
        // No file exists, so there is nothing to resume and no record to keep.
        // Writing a sidecar here would recreate exactly the litter the flag is
        // supposed to avoid.
    } else if complete && checksum_ok != Some(false) {
        Sidecar::remove(&out_path);
    } else {
        // Keep the sidecar so `-c` can pick up where this left off.
        let sc = Sidecar {
            size,
            validator: validator.clone(),
            done: vec![(0, on_disk.min(size))],
            url: job.urls[0].clone(),
        };
        let _ = sc.save(&out_path);
    }

    // `--no-save` needs no cleanup: nothing was ever created. The digest and the
    // format classification came from the stream (see `stream_digest`), which is
    // what made deleting-afterwards unnecessary. The earlier create-write-hash-
    // delete implementation is what left a 45 MB file behind when a run was
    // interrupted.

    // --stdout: stream the assembled object out, then remove the temporary file.
    //
    // This deliberately does NOT stream as bytes arrive. Positioned writes mean
    // ranges land out of order, so the file is only correct once complete;
    // emitting partial state to a pipe would hand the consumer bytes in the wrong
    // order. Saying so is better than appearing to stream and corrupting a pipe.
    //
    // Copied in fixed-size chunks rather than read into a `Vec` first: the whole point
    // of `--stdout` is piping, and a pipeline is exactly where buffering the entire
    // object is least affordable. `std::io::copy` on a 1 GiB download needed 1 GiB
    // resident; the same defect in the digest cost 127 MB of peak RSS on a 121.7 MiB
    // object, keeping resident memory footprint bounded.
    if job.to_stdout && complete {
        match std::fs::File::open(&out_path) {
            Ok(mut f) => {
                let mut so = std::io::stdout().lock();
                match std::io::copy(&mut f, &mut so) {
                    Ok(_) => {
                        use std::io::Write as _;
                        let _ = so.flush();
                        drop(f);
                        let _ = std::fs::remove_file(&out_path);
                    }
                    Err(e) => eprintln!("playdl: cannot stream to stdout: {e}"),
                }
            }
            Err(e) => eprintln!("playdl: cannot stream to stdout: {e}"),
        }
    }

    // --remote-time: only possible when the server offered a date-form validator.
    // An ETag is opaque and carries no time, so the flag is a silent no-op there
    // rather than a fabricated timestamp.
    if job.remote_time && complete {
        // `Last-Modified` first, and on its own terms. Reading the collapsed
        // `validator` here meant any server that also sent an ETag â€?GitHub, S3,
        // most CDNs â€?had its date thrown away before the flag ran, and the tool
        // reported "no date-form validator" about a response that carried one.
        // The validator is still consulted as a fallback, for the servers that
        // send only a date: there it IS the Last-Modified value.
        match last_modified
            .as_deref()
            .or(validator.as_deref())
            .and_then(pdl_net::polite::parse_http_date)
        {
            Some(secs) => {
                let _ = set_mtime(&out_path, secs);
            }
            None => p.event(
                1,
                "--remote-time: server sent no Last-Modified header, skipped",
            ),
        }
    }

    p.finish(
        delivered,
        complete && checksum_ok != Some(false),
        counters,
        digest.as_deref(),
    );
    // At default verbosity a format note is printed only when it is a WARNING â€?
    // the served bytes are not what was asked for. "gzip stream â€?compresses a
    // single streamâ€? is a description of a successful download and belongs at
    // `-v`; "this is an HTML page where a file was expected" is the difference
    // between a good file and a captive-portal page saved with status 200, and
    // suppressing it would hide the failure this project cares most about.
    if !job.quiet {
        if let Some(f) = detection.format {
            // `Markup` is the tell: an HTML page delivered where a file was
            // expected is the captive-portal / login-wall / error-page-with-200
            // case. A conflict between the sniffed type and the served
            // Content-Type is the other.
            let suspicious = detection.conflict.is_some() || f.category == Category::Markup;
            // At default verbosity this is a VALUE, not a sentence: `HTML page`
            // rather than `HTML page â€?A web page. Where a real file was
            // expected, this usually means...`. The explanation is real and worth
            // having, but it belongs at `-v`; a user who has seen it once does not
            // need the paragraph on every subsequent run.
            let note = if job.verbose > 0 {
                f.hint()
            } else {
                f.label().to_string()
            };
            if suspicious || job.verbose > 0 {
                // Never onto stdout when the object is going there: appending a
                // hint to a piped archive corrupts it (measured: 344 extra bytes
                // on a 34 041-byte .tar.gz, which then failed to decompress).
                // Under `--logfile` it goes to the file with the rest of the run.
                p.note(&format!("  {note}"), suspicious);
            }
        }
    }
    if checksum_ok == Some(false) {
        p.note(
            "  checksum MISMATCH: the delivered bytes are not the bytes requested",
            true,
        );
    }

    // ---- emit a manifest for what arrived --------------------------------
    //
    // Only for a download that verified. A manifest over bytes we already
    // believe are wrong would record the corruption as if it were the truth,
    // and every later check against it would agree.
    if let Some(mpath) = &job.emit_manifest {
        if complete && checksum_ok != Some(false) && !discarding {
            let cs = job
                .chunk_size
                .unwrap_or(pdl_net::manifest::DEFAULT_CHUNK)
                .max(1);
            match pdl_net::manifest::from_file(
                &out_path.to_string_lossy(),
                cs,
                pdl_net::manifest::ChunkAlgo::Blake3,
                Some(job.urls[0].clone()),
                validator.clone(),
            ) {
                Ok(m) => match std::fs::write(mpath, m.to_json()) {
                    Ok(()) => {
                        if !job.quiet {
                            eprintln!(
                                "  manifest: {} chunks of {} bytes -> {}",
                                m.chunks.digests.len(),
                                cs,
                                mpath.display()
                            );
                        }
                    }
                    Err(e) => eprintln!("playdl: cannot write manifest {}: {e}", mpath.display()),
                },
                Err(e) => eprintln!("playdl: cannot build manifest: {e}"),
            }
        } else if !job.quiet {
            eprintln!(
                "playdl: --emit-manifest skipped: a manifest is only written for a download \
                 that verified"
            );
        }
    }
    if let Some(r) = &chunk_report {
        if !job.quiet {
            eprintln!("  chunk integrity: {r}");
        }
    }

    Outcome {
        url: job.urls[0].clone(),
        output: outs,
        // The bytes delivered, which in range mode is the span rather than the
        // object's length. A consumer of `--json` comparing `size` against the
        // file it just received must find them equal.
        size: delivered,
        elapsed_s: elapsed,
        transfer_s: transfer_elapsed,
        setup_s: (elapsed - transfer_elapsed).max(0.0),
        throughput_bps: if transfer_elapsed > 0.0 {
            // Throughput of the TRANSFER, not of the invocation: dividing by setup time
            // too reports a rate the network never achieved.
            delivered as f64 / transfer_elapsed
        } else {
            0.0
        },
        requests,
        // What the transfer actually ran at, not the budget it was allowed.
        // Report what the transfer RAN at, not the peak the search explored: a number
        // the run did not use is not a measurement. `peak_connections` carries the
        // exploration separately for anyone diagnosing the search itself.
        connections: settled_conns
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1)
            .min(used_conns.load(std::sync::atomic::Ordering::Relaxed).max(1)),
        peak_connections: used_conns.load(std::sync::atomic::Ordering::Relaxed).max(1),
        peak_busy_connections: peak_busy.load(std::sync::atomic::Ordering::Relaxed),
        connection_seconds: conn_secs.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
        delta_s: delta,
        sha256: digest,
        checksum_ok,
        resumed_from,
        ok: complete && checksum_ok != Some(false),
        note: res.err().map(|e| e.to_string()),
        format: detection.format.map(|f| f.name.to_string()),
        category: Some(detection.category.as_str().to_string()),
        format_conflict: detection.conflict,
        format_label: detection.format.map(|f| f.label().to_string()),
        format_description: detection.format.map(|f| f.description().to_string()),
        category_description: Some(detection.category.description().to_string()),
    }
}

impl Outcome {
    /// An outcome for a path that ended before any bytes moved: a refusal, a skip,
    /// or an unchanged object. Keeps the four early-return sites from each
    /// carrying their own copy of every field.
    fn stopped(job: &Job, output: String, size: u64, ok: bool, note: &str) -> Self {
        Self {
            url: job.urls.first().cloned().unwrap_or_default(),
            output,
            size,
            ok,
            note: Some(note.to_string()),
            ..Self::default()
        }
    }
}

fn failed(job: &Job, size: u64, why: String) -> Outcome {
    // `-q` silences normal output; `--show-error` carves out the exception for
    // failures when requested. Without it a quiet run that failed
    // was indistinguishable from a quiet run that succeeded: empty stdout, empty
    // stderr, and only the exit code to tell them apart.
    if !job.quiet || job.show_error {
        eprintln!("playdl: {why}");
    }
    Outcome {
        url: job.urls.first().cloned().unwrap_or_default(),
        size,
        note: Some(why),
        ..Outcome::default()
    }
}

/// Unused by the engine, but the renderer needs the type in scope for callers
/// that build views from a live scheduler.
pub fn conn_views(sched: &Scheduler, hosts: &[String]) -> Vec<ConnView> {
    (0..sched.n_conns())
        .map(|j| ConnView {
            idx: j,
            host: hosts.get(sched.conn_source(j)).cloned().unwrap_or_default(),
            range: sched.conn_range(j),
            rate: sched.conn_rate(j),
            health: sched.conn_health(j),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers every request with `400 Bad Request` and a 24-byte JSON body â€?
    /// the shape of a CDN's "no such file" answer, with a `Content-Length` that
    /// a probe reading only the size would take for the object's.
    async fn spawn_400_origin() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut head = Vec::new();
                    let mut buf = [0u8; 1024];
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    let body = br#"{"error":"bad request!"}"#;
                    let _ = s
                        .write_all(
                            format!(
                                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = s.write_all(body).await;
                });
            }
        });
        port
    }

    /// A ranged origin for whole-job tests: HEAD states the length, a ranged
    /// GET answers `206`, and a plain GET answers `200`. Keeps the connection
    /// open across requests, since the client pools them.
    async fn spawn_ranged_origin(body: std::sync::Arc<Vec<u8>>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = l.local_addr().expect("addr").port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    return;
                };
                let body = body.clone();
                tokio::spawn(async move {
                    let mut head = Vec::new();
                    let mut buf = [0u8; 4096];
                    loop {
                        // One request head, then answer, then the next.
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                            match s.read(&mut buf).await {
                                Ok(0) | Err(_) => return,
                                Ok(n) => head.extend_from_slice(&buf[..n]),
                            }
                        }
                        let end = head.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
                        let text = String::from_utf8_lossy(&head[..end]).to_string();
                        head.drain(..end);
                        let method = text.split_whitespace().next().unwrap_or("").to_string();
                        let range = text
                            .lines()
                            .find(|l| l.to_ascii_lowercase().starts_with("range:"))
                            .and_then(|l| l.split_once('=').map(|(_, v)| v.trim().to_string()));
                        let total = body.len();
                        let (status, lo, hi) = match range.as_deref() {
                            Some(r) => {
                                let (a, b) = r.split_once('-').unwrap_or(("0", ""));
                                let lo: usize = a.parse().unwrap_or(0);
                                let hi: usize = b.parse().unwrap_or(total - 1);
                                ("206 Partial Content", lo, hi.min(total - 1))
                            }
                            None => ("200 OK", 0, total - 1),
                        };
                        let len = hi - lo + 1;
                        let mut h = format!(
                            "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n\
                             ETag: \"ranged\"\r\nContent-Type: application/octet-stream\r\n"
                        );
                        if range.is_some() {
                            h.push_str(&format!("Content-Range: bytes {lo}-{hi}/{total}\r\n"));
                        }
                        h.push_str("\r\n");
                        if s.write_all(h.as_bytes()).await.is_err() {
                            return;
                        }
                        if method != "HEAD" && s.write_all(&body[lo..=hi]).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        port
    }

    /// The reported SHA-256 must be the file's SHA-256, however it was computed.
    ///
    /// A single-connection transfer hashes the bytes as they land instead of
    /// reading the finished file back (`file_stream_sha256`); anything else
    /// hashes the file. Both must agree with each other and with the object, or
    /// the optimisation has produced the failure this project fears most: a
    /// stable, plausible, wrong digest.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_reported_sha256_is_the_files_sha256_on_every_hashing_path() {
        let body: Vec<u8> = (0..3_000_017u64).map(|i| (i % 251) as u8).collect();
        let want = pdl_net::digest::to_lower_hex(&Sha256::digest(&body));
        let port = spawn_ranged_origin(std::sync::Arc::new(body)).await;
        let dir = std::env::temp_dir().join(format!("playdl_digest_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        // `None` is the default path, where the concurrency probe chooses the
        // count and may pre-fill bytes the sink never sees. It is covered here
        // because it is what a plain `hydra URL` runs, and because a pre-filled
        // prefix is exactly the case where the in-band hash must decline and let
        // the file be hashed instead.
        for conns in [Some(1usize), Some(2), None] {
            let label = conns.map_or("default".to_string(), |n| format!("-x {n}"));
            let out = dir.join(format!("obj{}.bin", conns.unwrap_or(0)));
            let mut job = default_job();
            job.urls = vec![format!("http://127.0.0.1:{port}/obj.bin")];
            job.output = Some(out.clone());
            job.conns = conns;
            // The digest is opt-in now, and this test is about its correctness.
            job.print_checksum = true;
            let o = run(job).await;
            assert!(o.ok, "transfer at {label} failed: {:?}", o.note);
            assert_eq!(
                o.sha256.as_deref(),
                Some(want.as_str()),
                "{label}: reported digest is not the object's"
            );
            assert_eq!(
                sha256_file(&out).as_deref(),
                Some(want.as_str()),
                "{label}: file on disk is not the object"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An error status from the probe is an answer about the URL, not a
    /// description of a 24-byte object.
    ///
    /// `probe_resilient` returns the status rather than failing, on purpose, so
    /// that a `404` learned from HEAD is the error reported. The test is the
    /// caller's to make, and this caller did not make it: the error body's
    /// `Content-Length` became the file size, a transfer was planned, 24 bytes
    /// were split across eight connections, and only the range requests failed a
    /// second later â€?with "unexpected status 400 for a range request", which
    /// names the symptom and not the answer the server had already given.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_error_status_from_the_probe_is_reported_not_downloaded() {
        let port = spawn_400_origin().await;
        let u =
            crate::url::Url::parse(&format!("http://127.0.0.1:{port}/VSCode.zip0")).expect("url");
        let t = u.to_target(None).expect("target");
        let c = pdl_net::TlsCapableConnector::new().expect("connector");
        let mut log = Vec::new();
        let r = probe_resolving(&c, &u, &t, &mut log, 8).await;
        let err = match r {
            Ok(res) => panic!(
                "a 400 resolved to a {}-byte object instead of an error",
                res.probe.size
            ),
            Err(e) => e,
        };
        assert!(
            err.contains("400 Bad Request"),
            "the error must name the status the server gave: {err:?}"
        );
        // The host belongs to the caller, which prints it either side of this
        // message; carrying it here too put it on the line twice.
        assert!(
            !err.contains("127.0.0.1"),
            "the probe error must not repeat the host: {err:?}"
        );
    }

    /// Regression test for a sentinel-encoding bug: "the last 512 bytes" was
    /// encoded as `u64::MAX - 512` and recognised by a `> u64::MAX / 2` test,
    /// which mis-resolved and fetched bytes from the middle of the object
    /// (measured landing at exactly size/2 instead of the tail).
    #[test]
    fn suffix_range_resolves_to_the_actual_tail() {
        let size = 2_957_812u64;
        assert_eq!(
            RangeSpec::Suffix(512).resolve(size),
            Some((size - 512, size)),
            "a suffix range must be the LAST n bytes"
        );
        // A suffix larger than the object is the whole object, not an underflow.
        assert_eq!(RangeSpec::Suffix(size + 99).resolve(size), Some((0, size)));
    }

    #[test]
    fn closed_ranges_are_inclusive_as_http_spells_them() {
        // A range 0-1023 represents 1024 bytes.
        assert_eq!(RangeSpec::Closed(0, 1023).resolve(10_000), Some((0, 1024)));
        assert_eq!(
            RangeSpec::Closed(1000, 2023).resolve(10_000),
            Some((1000, 2024))
        );
        // Clamped to the object rather than requesting past the end.
        assert_eq!(
            RangeSpec::Closed(9990, 99_999).resolve(10_000),
            Some((9990, 10_000))
        );
    }

    #[test]
    fn open_ended_range_runs_to_the_end() {
        assert_eq!(RangeSpec::From(4096).resolve(10_000), Some((4096, 10_000)));
    }

    #[test]
    fn empty_and_degenerate_ranges_are_rejected() {
        assert_eq!(
            RangeSpec::From(10_000).resolve(10_000),
            None,
            "start at EOF is empty"
        );
        assert_eq!(RangeSpec::From(99_999).resolve(10_000), None);
        assert_eq!(RangeSpec::Closed(500, 499).resolve(10_000), None);
        assert_eq!(RangeSpec::Suffix(0).resolve(10_000), None);
    }

    /// `--no-save` must never create the output file.
    #[test]
    fn no_save_never_creates_a_file() {
        let mut job = default_job();
        job.no_save = true;
        assert_eq!(
            output_target(&job, "/tmp/hydra_should_not_exist.bin"),
            OutputTarget::Discard,
            "--no-save must resolve to a discarding sink, not a file to delete later"
        );
    }

    /// `--no-save --stdout` still needs storage: positioned writes land out of
    /// order, so the object is only correct once complete and cannot be streamed
    /// as it arrives. The temporary file is removed after streaming.
    #[test]
    fn no_save_with_stdout_still_needs_a_staging_file() {
        let mut job = default_job();
        job.no_save = true;
        job.to_stdout = true;
        assert_eq!(
            output_target(&job, "/tmp/hydra_stage.bin"),
            OutputTarget::Stdout("/tmp/hydra_stage.bin".into())
        );
    }

    #[test]
    fn a_plain_job_writes_a_file() {
        let job = default_job();
        assert_eq!(
            output_target(&job, "/tmp/hydra_plain.bin"),
            OutputTarget::File("/tmp/hydra_plain.bin".into())
        );
    }

    /// A scratch object for the span tests: `size` patterned bytes (modulus
    /// 251, a prime, so no page-aligned repeat can mask an offset error) in a
    /// private temp directory. Returns the directory (for cleanup) and the
    /// file path.
    fn span_scratch_object(size: usize) -> (std::path::PathBuf, std::path::PathBuf, Vec<u8>) {
        let dir = std::env::temp_dir().join(format!("playdl_span_{}", scratch_name()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("obj.bin");
        let mut whole = vec![0u8; size];
        for (i, b) in whole.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        std::fs::write(&p, &whole).unwrap();
        (dir, p, whole)
    }

    /// Range mode must deliver the SPAN, not the object's extent.
    ///
    /// The transfer writes `[lo, hi)` at true offsets inside a file the length of
    /// the whole object, so everything outside the span is a hole reading as
    /// zeros. `hydra -r 0-1023` delivered 34 041 bytes: 1 024 correct, 33 017
    /// zeros. Right prefix, plausible size, silently wrong file.
    #[test]
    fn a_prefix_range_is_cut_down_to_its_span() {
        let (dir, p, whole) = span_scratch_object(4096);

        extract_span(&p, 0, 1024).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(
            got.len(),
            1024,
            "must be the span's length, not the object's"
        );
        assert_eq!(
            got[..],
            whole[..1024],
            "must be the object's first 1024 bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A suffix range must move the span to the front, not merely truncate.
    #[test]
    fn a_suffix_range_moves_its_span_to_the_front() {
        let (dir, p, whole) = span_scratch_object(4096);

        extract_span(&p, 3584, 4096).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got.len(), 512);
        assert_eq!(
            got[..],
            whole[3584..],
            "a suffix range must yield the LAST bytes, at offset 0"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The copy is blocked rather than read whole, so a span longer than the
    /// block size must still come out byte-exact.
    #[test]
    fn a_span_larger_than_one_copy_block_is_exact() {
        // 3 MiB object, 2.5 MiB span starting mid-block: crosses the 1 MiB
        // copy block boundary at a non-multiple offset.
        let (dir, p, whole) = span_scratch_object(3 << 20);

        let (lo, hi) = (300_000u64, 300_000 + (5 << 19));
        extract_span(&p, lo, hi).unwrap();
        let got = std::fs::read(&p).unwrap();
        assert_eq!(got.len() as u64, hi - lo);
        assert_eq!(got[..], whole[lo as usize..hi as usize]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Ensure metadata probe follows redirects to the target object.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn public_probe_follows_a_redirect_to_the_object() {
        let net = pdl_net::origin::OriginSet::new();
        let (real_port, _real) = net.spawn(64 * 1024, 1_000_000);
        let (hop_port, _hop) =
            net.spawn_redirecting(0, 1_000_000, &format!("http://127.0.0.1:{real_port}/obj"));

        // `--no-proxy`: the ambient http_proxy of a CI sandbox must not divert
        // an in-memory origin lookup.
        let args = crate::cli::Cli::parse_with_queries([
            "hydra",
            "--no-proxy",
            &format!("http://127.0.0.1:{hop_port}/obj"),
        ])
        .unwrap();
        let u = Url::parse(&args.urls[0]).unwrap();
        let (pr, final_url) = probe_public(&net, &u, &args)
            .await
            .expect("probe through the redirect");
        assert_eq!(pr.size, 64 * 1024, "size must come from the object");
        assert!(
            !pr.is_redirect(),
            "the reported response must be the final one"
        );
        assert_eq!(
            final_url.port, real_port,
            "must land on the redirect target"
        );
    }

    #[test]
    fn the_bench_takes_the_same_oath_the_seats_took() {
        // A reserve's bytes are spliced into the same file on substitution, so
        // a mirror that answers late must clear the same bar it would have
        // cleared at the front door. An earlier revision required only range
        // support here, which would have benched an unvalidated mirror.
        let probe = |size: u64, ranges: bool, validator: Option<&str>, weak: bool| pdl_net::Probe {
            size,
            ranges,
            validator: validator.map(str::to_string),
            weak_validator: weak,
            last_modified: None,
            content_type: None,
            disposition: None,
            status: 200,
            location: None,
            raw_head: String::new(),
            raw_request: String::new(),
        };

        // With a document: the attested size is the whole test.
        assert!(bench_admission(
            Some(9),
            0,
            None,
            &probe(9, true, None, false)
        ));
        assert!(!bench_admission(
            Some(9),
            0,
            None,
            &probe(8, true, None, false)
        ));
        // ...but never without ranges: the first thing a substituted source is
        // asked for is a range.
        assert!(!bench_admission(
            Some(9),
            0,
            None,
            &probe(9, false, None, false)
        ));

        // Without a document: the pairwise gate, exactly as at the front door.
        let first = Some("\"etag-1\"");
        assert!(bench_admission(
            None,
            9,
            first,
            &probe(9, true, Some("\"etag-1\""), false)
        ));
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, Some("\"etag-2\""), false)),
            "a different validator is a different build"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(8, true, Some("\"etag-1\""), false)),
            "a different size is a different object"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, Some("\"etag-1\""), true)),
            "a weak validator may compare equal across different bytes"
        );
        assert!(
            !bench_admission(None, 9, first, &probe(9, true, None, false)),
            "no validator, no proof"
        );
        // A first source with no strong validator authorises nothing late.
        assert!(!bench_admission(
            None,
            9,
            None,
            &probe(9, true, Some("\"x\""), false)
        ));
    }

    #[test]
    fn a_digest_spec_names_its_algorithm_or_defaults_to_sha256() {
        use pdl_net::digest::Algo;
        // The two roads a spec arrives by: `--checksum`, normalised to
        // `algo:hex` at parse time, and a Metalink digest, which always carries
        // its prefix. A bare value is the historical `--checksum` form.
        assert_eq!(
            parse_digest_spec("sha512:AbC1"),
            Some((Algo::Sha512, "abc1".into())),
            "the algorithm is read and the hex lowercased"
        );
        assert_eq!(
            parse_digest_spec("f00d"),
            Some((Algo::Sha256, "f00d".into())),
            "a bare digest has always meant sha256 here"
        );
        assert_eq!(
            parse_digest_spec("whirlpool:aa"),
            None,
            "an algorithm this build cannot compute is 'not checked', never 'ok'"
        );
    }

    #[test]
    fn digest_file_computes_every_algorithm_a_document_publishes_and_refuses_crc() {
        use pdl_net::digest::Algo;
        let p = std::env::temp_dir().join(format!("playdl-digest-file-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        // Published test vectors for "abc", so a wrong wiring of algorithm to
        // hasher is caught here rather than as a checksum mismatch against a
        // real mirror.
        for (algo, want) in [
            (Algo::Md5, "900150983cd24fb0d6963f7d28e17f72"),
            (Algo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
            (
                Algo::Sha256,
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
        ] {
            assert_eq!(
                digest_file(&p, algo).as_deref(),
                Some(want),
                "{} vector",
                algo.as_str()
            );
        }
        assert_eq!(digest_file(&p, Algo::Sha512).map(|h| h.len()), Some(128));
        // A CRC is an error-detecting code, not a digest; "verified" against
        // one would mean almost nothing, and None reports it as unchecked.
        assert_eq!(digest_file(&p, Algo::Crc32), None);
        // A file that cannot be read is "could not check", not a wrong value.
        assert_eq!(
            digest_file(std::path::Path::new("/definitely/not/here"), Algo::Sha256),
            None
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn verify_scratch_names_are_unique_per_call_not_per_process() {
        // Two concurrent verifications sharing one scratch file made each read back the
        // OTHER object's bytes and report a mismatch on a byte-identical file. The name
        // must therefore vary within a process, not just across processes.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..64 {
            assert!(
                seen.insert(scratch_name()),
                "a scratch name repeated within one process"
            );
        }
    }
}
