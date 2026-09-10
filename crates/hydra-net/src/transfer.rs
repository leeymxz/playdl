//! The transfer loop: drive a [`Scheduler`] over live connections until the
//! object is complete.
//!
//! `hydra-net` owns sockets, the caller owns pixels: the `observe` callback is
//! how the CLI renders per-connection state without this crate depending on a
//! UI.

use crate::http::fetch_range;
use crate::polite::Pace;
use crate::sink::SparseSink;
use crate::{Arrival, Connector, Target};
use pdl_core::{Action, Scheduler};
use std::io;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;

/// Drive a transfer to completion. Returns (elapsed seconds, requests issued).
/// Ceiling on the upward-probe backoff multiplier.
///
/// Bounded so a transfer long enough to accumulate refusals never stops looking
/// entirely: at the 10 s floor on `probe_interval` this caps the wait at ~5 min,
/// which is rare enough to be free and short enough that a limit lifted mid
/// transfer is still noticed.
const MAX_PROBE_BACKOFF: f64 = 32.0;

pub async fn run_transfer<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    out_path: &str,
    sched: Scheduler,
) -> io::Result<(f64, u64)> {
    run_transfer_tick(
        connector,
        targets,
        conns_per_target,
        size,
        out_path,
        sched,
        20,
    )
    .await
}

/// As [`run_transfer`], with an explicit scheduler tick period in milliseconds.
///
/// The tick period bounds repair latency: a rate collapse can go unnoticed for
/// one tick plus one EWMA time-constant. On short transfers that is a
/// measurable fraction of the makespan, which is why it is a parameter and not
/// a constant.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_tick<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    out_path: &str,
    sched: Scheduler,
    tick_ms: u64,
) -> io::Result<(f64, u64)> {
    run_transfer_observed(
        connector,
        targets,
        conns_per_target,
        size,
        out_path,
        sched,
        tick_ms,
        &mut |_: &Scheduler, _: u64| {},
    )
    .await
}

/// As [`run_transfer_tick`], calling `observe(&sched, bytes_done)` once per tick.
///
/// The callback is how the CLI renders per-connection state without this crate
/// depending on a UI: `hydra-net` owns sockets, the caller owns pixels.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_observed<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    out_path: &str,
    sched: Scheduler,
    tick_ms: u64,
    // `Send` is required, not incidental: the queue manager spawns each transfer
    // onto a task, and a non-Send observer makes the whole future non-Send.
    observe: &mut (dyn FnMut(&Scheduler, u64) + Send),
) -> io::Result<(f64, u64)> {
    run_transfer_paced(
        connector,
        targets,
        conns_per_target,
        size,
        out_path,
        sched,
        tick_ms,
        observe,
        Pace::unlimited(),
    )
    .await
}

/// As [`run_transfer_observed`], with an aggregate rate cap.
///
/// Separate from [`run_transfer_observed`] rather than an extra argument on it
/// because almost every caller — the cross-validation harness, the benchmark
/// runner, the e2e tests — has no cap to apply, and threading `Pace::unlimited()`
/// through all of them buys nothing. `--limit-rate` is the one caller that does.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_paced<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    out_path: &str,
    sched: Scheduler,
    tick_ms: u64,
    observe: &mut (dyn FnMut(&Scheduler, u64) + Send),
    pace: Pace,
) -> io::Result<(f64, u64)> {
    let sink = Arc::new(SparseSink::create(out_path, size)?);
    run_transfer_into(
        connector,
        targets,
        conns_per_target,
        size,
        sink,
        sched,
        tick_ms,
        observe,
        pace,
    )
    .await
}

/// As [`run_transfer_observed`], but the caller supplies the sink.
///
/// This is what `--no-save` needs: it passes [`SparseSink::discarding`] so no
/// file is ever created. The earlier implementation wrote a real file and
/// deleted it at the end, which left the bytes on disk for the whole transfer
/// and survived an interrupted run.
///
/// A caller that also wants the object's digest attaches one to the sink with
/// [`SparseSink::with_digest`] before handing it over — the sink is the one place
/// every fragment passes through, so it is where a stream observer belongs.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_into<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    sink: Arc<SparseSink>,
    sched: Scheduler,
    tick_ms: u64,
    observe: &mut (dyn FnMut(&Scheduler, u64) + Send),
    pace: Pace,
) -> io::Result<(f64, u64)> {
    run_transfer_cancellable(
        connector,
        targets,
        conns_per_target,
        size,
        sink,
        sched,
        tick_ms,
        observe,
        pace,
        None,
    )
    .await
}

/// As [`run_transfer_into`], with an external stop signal.
///
/// This is the GUI's Pause/Cancel: setting `cancel` makes the loop abort every
/// in-flight fetch and return `ErrorKind::Interrupted` at the next tick, so the
/// caller gets the socket teardown the scheduler's own `Action::Cancel` path
/// performs — not detached tasks streaming on. Bytes already written through
/// the sink stay valid at their offsets; a later run resumes by `mark_done`.
/// `None` is exactly the previous behaviour, which is how every existing entry
/// point calls it.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_cancellable<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    sink: Arc<SparseSink>,
    sched: Scheduler,
    tick_ms: u64,
    observe: &mut (dyn FnMut(&Scheduler, u64) + Send),
    pace: Pace,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> io::Result<(f64, u64)> {
    run_transfer_with_reserves(
        connector,
        targets,
        conns_per_target,
        size,
        sink,
        sched,
        tick_ms,
        observe,
        pace,
        cancel,
        Bench::default(),
        None,
    )
    .await
}

/// Called after a reserve mirror takes a failed source's place.
///
/// A named alias rather than the type spelled inline: the signature is already
/// twelve arguments long, and `Option<&mut (dyn FnMut(usize, &Reserve) + Send)>`
/// in the middle of it obscures which argument is which.
pub type OnSubstitute<'a> = Option<&'a mut (dyn FnMut(usize, &Reserve) + Send)>;

/// The mirrors a transfer can fall back on, and how they arrive.
///
/// # Why the bench is a STREAM and not a snapshot
///
/// Filling it needs a probe per mirror, and those probes are paid entirely
/// before the first byte — so a transfer that waits for the whole list waits
/// for its slowest host to answer a HEAD, in order to learn about mirrors it
/// will not touch unless something fails. Measured against a real twelve-mirror
/// Fedora document, that was 2.0 s of dead time in front of a 5 s transfer.
///
/// Splitting the two lets the caller start as soon as it has enough mirrors to
/// SEAT — which is the only thing the scheduler needs up front — and keep
/// probing the rest while bytes move. Reserves that arrive late are just as
/// useful as reserves that arrived early: nothing consults the bench until a
/// source fails.
#[derive(Debug, Default)]
pub struct Bench {
    /// Reserves known before the transfer starts, best-ranked first.
    pub ready: Vec<Reserve>,
    /// Reserves still being probed, delivered as they are admitted.
    ///
    /// Drained on every tick. `None` is a caller that has everything already,
    /// which is exactly the previous behaviour.
    pub late: Option<mpsc::UnboundedReceiver<Reserve>>,
}

impl Bench {
    /// A bench that is complete before the transfer begins.
    pub fn fixed(ready: Vec<Reserve>) -> Self {
        Bench { ready, late: None }
    }
}

/// A mirror held back from the transfer, ready to replace one that fails.
#[derive(Clone, Debug)]
pub struct Reserve {
    pub target: Target,
    /// Ranking and any ceiling the source stated about itself.
    pub plan: pdl_core::SourcePlan,
    /// Host name, for the caller's progress view after a substitution.
    pub host: String,
}

/// Consecutive failures charged to one SOURCE before it is replaced.
///
/// Two, not one: a single 5xx from one node of a CDN, or one refused connection
/// during a deploy, is worth another attempt on the same host — substituting on
/// the first error would burn the bench on transient faults and leave nothing
/// for the failure that is real. Two consecutive failures with no byte of
/// progress in between is no longer transient.
const SOURCE_FAILS_BEFORE_SUBSTITUTION: u32 = 2;

/// Consecutive scheduler-observed stalls before a source is replaced.
///
/// Higher than the error threshold because a stall is weaker evidence: an error
/// is the source saying something, while a stall is only the absence of bytes,
/// which a congested path produces too. Three rounds of reclaim-and-retry with
/// nothing arriving is the point at which a different host is the better bet.
const SOURCE_STALLS_BEFORE_SUBSTITUTION: u32 = 3;

/// How many times slower than the FASTEST source a mirror must be before its
/// sockets are worth moving to a reserve.
///
/// # Why this threshold is so far out
///
/// The scheduler already handles a merely-slow source, and handles it better
/// than a swap would: repair reassigns BYTES away from it continuously, on
/// measurement, at the cost of nothing but a range boundary. Substituting is the
/// blunter instrument — it pays a fresh connection setup and throws away
/// everything measured about the host — so it is only worth doing when the
/// source has stopped being a meaningful contributor at all.
///
/// Eight-to-one is that point. A mirror at half the speed of the best one is
/// still carrying a third of the transfer; a mirror at an eighth is carrying
/// almost none of it while holding sockets politeness counted against the
/// aggregate. Below that ratio the reserve is the better bet even after paying
/// for the handshake.
const LAGGARD_RATIO: f64 = 8.0;

/// ...and it must stay that slow for this long, continuously.
///
/// A rate estimate dips for reasons that are not the mirror's fault: a range
/// boundary, a repair, one slow read, a moment of congestion on the client's own
/// link. Ten seconds is many rate samples — the estimate is windowed and
/// smoothed before it is ever read here — so none of those survive it, while it
/// is still short enough to act on inside a transfer with minutes left to run.
const LAGGARD_SECONDS: f64 = 10.0;

/// Seconds of transfer that must remain before a swap can repay its handshake.
///
/// The cost of substituting is one connection setup on a host nothing is known
/// about; the gain is the difference between the reserve's rate and the
/// laggard's, for however long is left. Expressed in TIME rather than in bytes
/// because that is the form both sides of the comparison are in: a fixed byte
/// threshold means something different on a 200 KB/s link than on a gigabit one,
/// and would either never fire on the slow path or fire pointlessly on the fast
/// one.
const LAGGARD_MIN_PAYOFF_SECONDS: f64 = 5.0;

/// As [`run_transfer_cancellable`], with a bench of reserve mirrors.
///
/// # What the bench is for
///
/// A mirror list names far more sources than politeness authorises sockets for —
/// a distribution image's Metalink commonly lists fifteen to twenty hosts
/// against four connections. Without substitution the surplus is decoration: the
/// transfer survives on the mirrors it opened with or it does not, and a client
/// holding nineteen working URLs fails because four of them went away.
///
/// A source is replaced when it has failed [`SOURCE_FAILS_BEFORE_SUBSTITUTION`]
/// times consecutively with no progress in between, or stalled
/// [`SOURCE_STALLS_BEFORE_SUBSTITUTION`] times — the second case being the one a
/// pure error count misses, because a black-holing mirror never returns an error
/// at all. Substitution happens IN PLACE: the failed source's connections are
/// relabelled rather than added to, so the socket count stays what politeness
/// authorised, and the caller's per-source bookkeeping stays index-aligned.
///
/// `on_substitute(source_index, new_target)` is called after each swap, because
/// a progress view that keeps naming the dead host is worse than one that shows
/// no host at all: it attributes the replacement's throughput to a machine that
/// is not serving it.
///
/// An empty bench is exactly the previous behaviour, which is how every existing
/// entry point calls this.
#[allow(clippy::too_many_arguments)]
pub async fn run_transfer_with_reserves<C: Connector>(
    connector: Arc<C>,
    targets: Vec<Target>,
    conns_per_target: &[usize],
    size: u64,
    sink: Arc<SparseSink>,
    mut sched: Scheduler,
    tick_ms: u64,
    observe: &mut (dyn FnMut(&Scheduler, u64) + Send),
    pace: Pace,
    cancel: Option<Arc<std::sync::atomic::AtomicBool>>,
    reserves: Bench,
    mut on_substitute: OnSubstitute<'_>,
) -> io::Result<(f64, u64)> {
    let mut targets = targets;
    let Bench {
        ready,
        late: mut late_reserves,
    } = reserves;
    let mut bench: std::collections::VecDeque<Reserve> = ready.into();
    // Failures charged to each SOURCE, not to each connection. A source with
    // four connections failing once each is a dead host, not four unlucky
    // sockets, and a per-connection counter never sees it.
    let mut src_fail: Vec<u32> = vec![0; conns_per_target.len().max(1)];
    // When each source first fell below `LAGGARD_RATIO`, cleared the moment it
    // recovers. A clock rather than a counter because the evidence being asked
    // for is duration: "slow right now" is noise, "slow for twenty seconds" is a
    // measurement.
    let mut laggard_since: Vec<Option<f64>> = vec![None; conns_per_target.len().max(1)];
    let (tx, mut rx) = mpsc::unbounded_channel::<Arrival>();
    // Fetch outcomes, reported by every spawned task as it ends.
    //
    // Without this the transport SPAWNS a fetch and never looks at what happened
    // to it: `let _ = fetch_range(...).await` discarded the result, so a refused
    // connection, a closed socket, a truncated body, a 503 and a protocol
    // violation were all indistinguishable from a connection that was merely
    // slow. The only thing that eventually noticed was the scheduler's stall
    // timeout, seconds later — see `Scheduler::on_conn_error` for what that costs
    // and why the endgame is where it shows.
    //
    // The generation number is not decoration: a task can finish in the same
    // instant its connection is superseded or cancelled, and its outcome would
    // then be charged against the request that replaced it.
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<(usize, u64, io::Result<()>)>();
    let mut gen_of: std::collections::HashMap<usize, u64> = std::collections::HashMap::new();
    let mut next_gen: u64 = 0;
    // Consecutive fetch failures since the last byte of progress, and the last
    // error seen. The streak drives per-connection backoff so a broken endpoint is
    // not hammered; the error is reported if the transfer ultimately fails, since
    // "every source stalled or unreachable" describes the symptom and never the
    // cause.
    let mut fail_streak: u32 = 0;
    let mut hard_streak: u32 = 0;
    let mut last_error: Option<io::Error> = None;
    // Concurrency ceiling learned from the source's own refusals.
    //
    // A 429 answers a question the client did not think to ask: how many requests
    // at once will this origin accept? Standing the source down for `Retry-After`
    // and then returning with the SAME connection count asks the identical
    // question again, gets the identical answer, and the transfer livelocks.
    // Measured against `ash-speed.hetzner.com`, which serves exactly two
    // connections per address and refuses the rest: eight connections downloaded
    // ZERO bytes in thirty seconds, one refusal every two seconds, each aborting
    // whatever the other seven had in flight.
    //
    // The ceiling moves the way congestion control moves — down hard on a refusal,
    // back up one at a time on evidence that the limit has lifted — for the same
    // reason: the client cannot see the origin's limit, only whether it is over it.
    // The ramp is clamped to it too, since a ramp that re-raises what a refusal
    // lowered is the same livelock with a longer period.
    let mut throttle_cap = sched.n_conns().max(1);
    // One reduction per refusal ROUND, not per refusal. Refusals arrive in bursts
    // — six of eight requests, in the same instant, all reporting the one fact
    // that eight was too many. Halving once per burst converges on the limit;
    // halving six times converges on one connection and stays there, which is a
    // transfer at a fraction of the rate the origin was willing to serve.
    let mut cap_settled_until = 0.0f64;
    // A refusal-free stretch this long, with bytes actually arriving, earns one
    // connection back. Without it the first burst caps the transfer permanently,
    // including for the far more common case of a limit that is momentary.
    let probe_interval = (20.0 * sched.worst_delta().max(0.25)).clamp(10.0, 60.0);
    // Multiplier on that interval, doubled every time a probe is refused and halved
    // every time one survives.
    //
    // A fixed interval treats every refusal as momentary. Against an origin whose
    // limit is a standing configuration — nginx `limit_conn`, a CDN's per-address
    // cap — the probe is refused every single time, so the transfer pays a wasted
    // handshake, a refusal, and a re-halved ceiling once per interval for its whole
    // life, and the ceiling never gets to sit still at the number that works.
    // Backing off converges on leaving a real limit alone without giving up on a
    // momentary one, which is the same argument the ceiling itself is built on.
    let mut probe_backoff = 1.0f64;
    let mut next_probe_at = f64::INFINITY;
    // Most connections seen streaming AT ONCE, over a recent stretch. This is the
    // floor the ceiling may not fall below.
    //
    // "What is streaming right now" is the wrong floor for the same reason it was
    // the wrong suspension test: a connection that has been granted its range but
    // whose first body byte is still a round trip away counts as nothing. So a
    // refusal that lands in the gap between one request finishing and the next
    // one's first byte sees zero, floors at one, and halves a ceiling that was
    // already correct. Observed on the throttled origin this is tuned against: the
    // ceiling reached the right answer of 2 and was then driven to 1 by a refusal
    // that arrived while both working connections happened to be between ranges,
    // and a transfer that had found the origin's limit spent the rest of its life
    // at half of it.
    //
    // A peak over a window is proof of a concurrency this origin HAS served, and
    // it is the same class of evidence as the refusal itself. Two buckets rather
    // than an all-time maximum so it decays: an origin that tightens its limit
    // mid-transfer must be able to push the floor back down, which an all-time
    // high-water mark would never allow.
    let mut served_peak_cur = 0usize;
    let mut served_peak_prev = 0usize;
    let mut served_peak_rotate_at = probe_interval;
    // Bytes held when the last refusal landed. The probe is earned by PROGRESS,
    // not by the clock: widening a transfer that is not moving adds requests to a
    // source that is already failing to serve the ones it has.
    let mut probe_held_mark: u64 = 0;
    let t0 = Instant::now();

    // conn index -> target index
    let mut owner = Vec::new();
    for (i, &k) in conns_per_target.iter().enumerate() {
        for _ in 0..k {
            owner.push(i);
        }
    }

    // Live fetch tasks, so a Cancel action can actually stop one. Without this
    // a black-holed connection's task lives forever holding its socket, and the
    // scheduler's reclaim has no effect on the wire.
    let mut inflight: std::collections::HashMap<usize, tokio::task::JoinHandle<()>> =
        std::collections::HashMap::new();

    // The live far end of each connection's in-flight range, shared with the task
    // streaming it. A repair lowers the victim's entry and the running task sees
    // it on its next read, which is what makes range preemption cost what the
    // theory says it costs — see `crate::Watermark`.
    let mut bounds: std::collections::HashMap<usize, crate::Watermark> =
        std::collections::HashMap::new();

    // Draw the next reserve mirror and put it in a failed source's place.
    //
    // A macro rather than a closure because it needs `&mut` on the scheduler,
    // the target list, the bench, and all three per-connection maps at once —
    // which a closure would have to borrow for its whole lifetime, and the
    // surrounding loop needs them too.
    //
    // In place, not in addition: the failed source's connection indices are
    // reused, so the socket count stays what politeness authorised and the
    // caller's per-source bookkeeping keeps its alignment.
    // Take everything a still-running probe has admitted since the last look.
    //
    // Cheap and non-blocking: a `try_recv` loop over a channel that is usually
    // empty. Called on every tick AND immediately before a substitution, so a
    // reserve that arrived microseconds ago is available to the failure that
    // needs it rather than to the one after.
    macro_rules! collect_late {
        () => {{
            if let Some(rx) = late_reserves.as_mut() {
                loop {
                    match rx.try_recv() {
                        Ok(r) => bench.push_back(r),
                        Err(mpsc::error::TryRecvError::Empty) => break,
                        // The prober is done. Dropping the receiver stops the
                        // per-tick work for the rest of the transfer.
                        Err(mpsc::error::TryRecvError::Disconnected) => {
                            late_reserves = None;
                            break;
                        }
                    }
                }
            }
        }};
    }

    macro_rules! substitute_source {
        ($src:expr) => {{
            let src: usize = $src;
            collect_late!();
            match bench.pop_front() {
                None => false,
                Some(r) => {
                    // Stop the dead source on the WIRE before the scheduler
                    // reclaims its ranges. A task left running would keep
                    // delivering bytes for a range that now belongs to somebody
                    // else, which is the duplicate-traffic failure `Watermark`
                    // exists to prevent, in a different disguise.
                    for j in 0..sched.n_conns() {
                        if sched.conn_source(j) == src {
                            if let Some(h) = inflight.remove(&j) {
                                h.abort();
                            }
                            bounds.remove(&j);
                            gen_of.remove(&j);
                        }
                    }
                    // The setup cost carries over — it is a property of this
                    // client's path, not of the dead host — but nothing else
                    // does. Inheriting the failed mirror's rate estimate would
                    // price the replacement by the failure it is replacing.
                    sched.replace_source(
                        src,
                        pdl_core::Source {
                            priority: r.plan.priority,
                            delta_est: sched.worst_delta().max(1e-3),
                            ..Default::default()
                        },
                    );
                    if let Some(slot) = targets.get_mut(src) {
                        *slot = r.target.clone();
                    }
                    if let Some(f) = src_fail.get_mut(src) {
                        *f = 0;
                    }
                    if let Some(l) = laggard_since.get_mut(src) {
                        *l = None;
                    }
                    if let Some(cb) = on_substitute.as_deref_mut() {
                        cb(src, &r);
                    }
                    true
                }
            }
        }};
    }

    // One pool for the whole transfer, shared by every connection. This is the
    // case connection reuse was missing from most: `n` ranges against one origin
    // used to mean `n` handshakes, and every repair another. The pool only ever
    // receives connections whose response ended where the client predicted — see
    // `crate::pool` — so a shrunk connection is dropped rather than reused.
    //
    // Taken from the connector when it offers one, so a caller that already spoke
    // to this origin — the CLI's size probe does, on every run — can hand over the
    // connection it is holding instead of letting the transfer redial. Measured on
    // a live TLS path: 1.6-2.0 s of setup before the first byte on a short transfer.
    // The gap was almost entirely handshakes that had already been paid for once.
    let pool: crate::pool::SharedPool<C::Stream> = connector
        .pool()
        .unwrap_or_else(|| Arc::new(crate::pool::ConnPool::new()));
    // Reported through the same channel as everything else measurable about a
    // transfer: a reuse rate that is claimed rather than counted is not a
    // measurement. `HYDRA_POOL_STATS=1` prints it after the transfer.
    let report_pool = std::env::var_os("HYDRA_POOL_STATS").is_some();
    let trace_errors = std::env::var_os("HYDRA_TRACE_ERRORS").is_some();
    let trace_repair = std::env::var_os("HYDRA_TRACE_REPAIR").is_some();

    // ---- no-progress watchdog --------------------------------------------
    //
    // The scheduler detects a stalled connection and reclaims its range, but
    // reclaiming is not recovering: it returns the bytes to the unassigned set,
    // and only a *different* live source can turn that into progress. With one
    // source — or with every source dead — the reclaimed range is handed back to
    // the same silent connection on the next tick and the loop spins.
    //
    // This is invisible to scheduler core invariants (bytes are tracked, and
    // reclaimable stalls allow state machine transitions). The higher-level
    // transport loop owns the wall clock and enforces real-world progress deadlines.
    //
    // The deadline is derived rather than hardcoded: a request costs `delta` to
    // set up, and the scheduler needs a few reclaim-and-retry rounds before
    // giving up is fair. Expressing it in units of the measured `delta` and the
    // configured stall timeout means a slow TLS-through-a-proxy path is granted
    // proportionally more patience than a LAN mirror, with no constant to
    // retune. The floor keeps a fast path from failing during normal setup
    // jitter; the ceiling keeps a pathological `delta` estimate from
    // reintroducing the original hang.
    let no_progress_deadline = {
        const RECLAIM_ROUNDS: f64 = 4.0;
        let d = sched.worst_delta().max(0.0);
        let st = sched.stall_timeout().max(0.0);
        (RECLAIM_ROUNDS * (st + d)).clamp(5.0, 60.0)
    };
    let mut last_progress_at = Instant::now();
    let mut last_held: u64 = sched.bytes_held();

    // Extra patience earned by DELIBERATE scheduler pauses, and the ceiling on it.
    //
    // `backoff_grace` accumulates only while every source is suspended — silence the
    // scheduler chose — and is added to the no-progress deadline. The cap is what
    // keeps this from becoming unbounded patience: a source that black-holes from the
    // first byte generates a fresh suspension after every stall, so an uncapped grace
    // would forgive deadline after deadline and the transfer would never fail.
    //
    // One extra deadline's worth is enough to cover a real backoff (`stall_timeout`
    // through 30s) once, which is the case worth surviving, while still bounding the
    // total wait at roughly twice the deadline.
    let mut backoff_grace = 0.0f64;
    let backoff_grace_cap = no_progress_deadline;
    let mut last_tick_at = Instant::now();

    let mut ticker = tokio::time::interval(tokio::time::Duration::from_millis(tick_ms.max(1)));
    // In-band concurrency ramp.
    //
    // Enabled by the caller starting the scheduler below its full connection count
    // (`Scheduler::with_active_limit`). The ramp then admits connections while the
    // aggregate rate says they pay for themselves, measuring on the real transfer
    // instead of on probe traffic — see `pdl_core::ramp` for why the probe was a
    // net loss (1.96x slower on a 3 MB object, p = 0.004).
    let mut ramp = if sched.active_limit() < sched.n_conns() {
        // Start at the scheduler's active limit, which the caller set deliberately,
        // rather than at one. The CLI sets it to 1 for `--adaptive`, so the search
        // begins at the configuration that measured fastest in the field and only
        // admits more on evidence of headroom — see `ConcurrencyRamp::starting_at`.
        let mut r = pdl_core::ConcurrencyRamp::starting_at(
            0.15,
            sched.active_limit().max(1),
            sched.n_conns(),
        );
        r.start(0.0, sched.worst_delta().max(1e-3));
        // Measure levels, not handshakes. `delta` is the per-request cost on a
        // pooled connection; opening a new one costs a TCP and a TLS handshake on
        // top of it, which on a high-RTT path outlasts the whole measurement window
        // — so the level read as no better than the one below it and the search
        // settled at one connection. The gate holds the window shut until the
        // level's connections are actually on the wire.
        r.arm_warmup(0.0, sched.worst_delta().max(1e-3));
        Some(r)
    } else {
        None
    };
    let mut ramp_last_held: u64 = sched.bytes_held();
    // The concurrency a FINISHED search rejected a higher level in favour of.
    //
    // Set only when the search measured a larger count and turned it down, never
    // when it merely ran into a ceiling it wanted to climb past. The refusal-free
    // probe below reads it as an upper bound: that probe recovers from a REFUSAL,
    // and an origin lifting its limit says nothing about whether the extra
    // connections pay. Without it, one refusal anywhere in a transfer let the
    // recovery walk the count back up to the budget and silently discard the
    // measurement — including on the origins this search exists to handle, where
    // one connection beat eight.
    let mut settled_limit: Option<usize> = None;
    if ramp.is_some() {
        sched.set_limit_reason(pdl_core::LimitReason::Measuring);
    }

    loop {
        // 0. external stop. Checked before arrivals are drained so a cancel
        // observed mid-tick still credits the bytes below it; abort teardown
        // mirrors the watchdog path.
        if let Some(c) = &cancel {
            if c.load(std::sync::atomic::Ordering::Relaxed) {
                // Report the final credited state so the caller's last snapshot
                // (held ranges, bytes done) includes everything on disk.
                observe(&sched, sched.bytes_held());
                for (_, h) in inflight.drain() {
                    h.abort();
                }
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }

        // 1. drain arrivals into the scheduler
        while let Ok(a) = rx.try_recv() {
            sched.on_bytes_at(a.conn, a.off, a.bytes, a.at, a.dt);
        }

        // 1a. act on fetch outcomes, at once rather than at the stall timeout.
        while let Ok((conn, g, res)) = done_rx.try_recv() {
            // Not the request this connection is running now: the task was
            // superseded or cancelled and finished on its way out. Its outcome
            // says nothing about the range now in flight.
            if gen_of.get(&conn) != Some(&g) {
                continue;
            }
            gen_of.remove(&conn);
            inflight.remove(&conn);
            bounds.remove(&conn);
            let Err(e) = res else {
                // A completed range is proof this source works. Without this the
                // counter is "failures ever" rather than "failures in a row", and
                // two transient faults an hour apart would spend a reserve.
                if let Some(f) = src_fail.get_mut(sched.conn_source(conn)) {
                    *f = 0;
                }
                continue;
            };
            let now = t0.elapsed().as_secs_f64();
            if trace_errors {
                eprintln!("[trace] t={now:.2} conn {conn}: {} — {e}", e.kind());
            }
            fail_streak = fail_streak.saturating_add(1);
            let failed_src = sched.conn_source(conn);
            if let Some(f) = src_fail.get_mut(failed_src) {
                *f = f.saturating_add(1);
            }
            let kind = e.kind();
            // A server that ignores `Range`, mislabels a `Content-Range`, answers
            // with a status this client cannot use, or redirects a transfer that
            // is already under way will do the same to the next request; retrying
            // is how a client hammers a broken endpoint instead of reporting it.
            // A streak rather than the first one, because a single 5xx from one
            // node of a CDN is worth another attempt.
            //
            // Reporting matters as much as stopping. An expired pre-signed URL —
            // a GitHub release asset, an S3 link — starts answering 403 part-way
            // through, and every request after that fails the same way. Spinning
            // on it until the no-progress deadline tells the user only that
            // nothing is arriving; failing on it tells them what the server said.
            hard_streak = if matches!(
                kind,
                io::ErrorKind::InvalidData | io::ErrorKind::NotConnected
            ) {
                hard_streak + 1
            } else {
                0
            };
            // Per-connection backoff, doubling with the streak. Zero would be a
            // hot retry loop against an endpoint that is refusing; the range is
            // back in the unassigned set either way, so a healthy connection can
            // pick it up on the next tick without waiting for this.
            let cool = (0.05 * (1u64 << fail_streak.min(6)) as f64).min(2.0);
            match kind {
                // 429/503 with a Retry-After: the origin refusing a request
                // because too many are in flight, or because it is shedding load.
                io::ErrorKind::WouldBlock => {
                    let secs = crate::http::retry_after_secs(&e)
                        .unwrap_or(1.0)
                        .clamp(0.05, 60.0);
                    let src = sched.conn_source(conn);
                    // What this source is DELIVERING right now, refusal and all.
                    // The distinction the old code missed: a refusal while other
                    // connections stream is the origin declining one more request,
                    // not the source going away, and tearing the working ones down
                    // throws away bytes that have to be pulled again.
                    let streaming = (0..sched.n_conns())
                        .filter(|&j| {
                            j != conn
                                && sched.conn_source(j) == src
                                && sched
                                    .conn_range(j)
                                    .map(|(lo, pos, _hi)| pos > lo)
                                    .unwrap_or(false)
                        })
                        .count();
                    // What is still ON THE WIRE for this source: a request that has
                    // been sent and has neither failed nor finished.
                    //
                    // Suspending on `streaming == 0` alone is what made an origin
                    // like `ash-speed.hetzner.com` — two connections per address,
                    // the rest refused — collapse the whole transfer. Every request
                    // in the opening burst leaves within milliseconds of the others,
                    // and a 429 is a 162-byte body that comes back a round trip
                    // ahead of the first body bytes of the 206s beside it. So at the
                    // instant the first refusal is handled NOTHING has delivered
                    // yet, `streaming` is 0, and the source stood down — aborting
                    // the two requests the origin had just GRANTED, closing their
                    // sockets, and paying both handshakes again on the retry.
                    // Measured on a 100 MB object over that origin: 162 requests and
                    // 6 m 57 s against 2 m 05 s for the same transfer at `-x 2`.
                    //
                    // A granted request is indistinguishable from a refused one
                    // until it answers, so the honest test is whether anything is
                    // still outstanding. While something is, this refusal is the
                    // origin declining ONE more request; only when the last one has
                    // failed too is it the source itself standing down.
                    let outstanding = (0..sched.n_conns())
                        .filter(|&j| {
                            j != conn && sched.conn_source(j) == src && inflight.contains_key(&j)
                        })
                        .count();
                    if streaming == 0 && outstanding == 0 {
                        // Nothing is getting through: the source itself stands
                        // down, and its in-flight fetches go with it, since their
                        // bytes would no longer be credited to any range.
                        sched.suspend_source(src, now + secs);
                        for j in 0..sched.n_conns() {
                            if sched.conn_source(j) == src {
                                if let Some(h) = inflight.remove(&j) {
                                    h.abort();
                                }
                                bounds.remove(&j);
                                gen_of.remove(&j);
                            }
                        }
                    } else {
                        // Only this request was refused. Cool this connection for
                        // as long as the origin asked and leave the rest alone.
                        sched.on_conn_error(conn, now, secs);
                    }
                    // One step down per burst, and never below what is visibly
                    // working: the connections that ARE streaming are proof of a
                    // count this origin accepts, so a refusal is evidence about
                    // the excess, not about them.
                    if now >= cap_settled_until {
                        let floor = streaming.max(served_peak_cur).max(served_peak_prev).max(1);
                        throttle_cap = (throttle_cap / 2).max(floor).min(sched.n_conns().max(1));
                        // Exactly `secs`, matching the cooldown `on_conn_error` just
                        // gave these connections: their retry is the next legitimate
                        // round, and it must not arrive before the gate reopens. An
                        // extra margin here (previously `+ sched.worst_delta()`) made
                        // the gate outlast that cooldown, so the retry burst that was
                        // supposed to confirm or correct this reduction was silently
                        // dropped, and the ceiling was left wherever the first,
                        // least-informed round put it.
                        cap_settled_until = now + secs;
                        // The floor's evidence describes the cap that was just
                        // abandoned. Keeping it means a count the origin has
                        // stopped granting goes on holding the floor up, and the
                        // ceiling cannot reach the count actually being served:
                        // measured on `ash-speed.hetzner.com` at `-x 8`, an origin
                        // that refuses nearly all concurrency, the ceiling reached
                        // 2 in a few seconds and then sat there being refused until
                        // t=30.7s of a transfer that takes 12, because a peak of 2
                        // from the opening burst outlived it. Re-basing on what is
                        // delivering right now keeps the guard against
                        // over-reduction — a connection between ranges is still
                        // counted through `streaming` — while letting the next
                        // round judge the new cap on the new cap's evidence.
                        served_peak_cur = streaming;
                        served_peak_prev = 0;
                        served_peak_rotate_at = now + probe_interval;
                        if trace_errors {
                            eprintln!(
                                "[trace] t={now:.2} throttled: {streaming} delivering, \
                                 {outstanding} outstanding, floor {floor}, \
                                 concurrency ceiling -> {throttle_cap}"
                            );
                        }
                    }
                    if sched.active_limit() > throttle_cap {
                        sched.set_active_limit(throttle_cap);
                    }
                    // Assignment reserves work for connections that are still to be
                    // admitted. Above the ceiling none are, so the reserve is work
                    // nobody comes for and the transfer re-requests it a share at a
                    // time — a round trip each, and against an origin that refuses,
                    // a fresh handshake each.
                    sched.set_conn_ceiling(throttle_cap);
                    if throttle_cap < sched.n_conns() {
                        sched.set_limit_reason(pdl_core::LimitReason::Refused {
                            serving: throttle_cap,
                        });
                    }
                    // The search must not ask for what the origin has just refused.
                    // Without this the ramp keeps doubling toward a budget it can
                    // never reach, and its warm-up gate waits out its whole deadline
                    // at every level because the connections it is waiting for are
                    // clamped away and never deliver.
                    if let Some(r) = ramp.as_mut() {
                        r.clamp_max(throttle_cap);
                    }
                    // Back off the upward probe as well. A ceiling that has been
                    // refused once is likely to be refused again, and probing it on
                    // a fixed interval spends one wasted request — a handshake, a
                    // refusal, and a re-halved ceiling — every interval for the whole
                    // transfer. Doubling the wait converges on leaving a real limit
                    // alone while still recovering from a momentary one.
                    probe_backoff = (probe_backoff * 2.0).min(MAX_PROBE_BACKOFF);
                    next_probe_at = now + probe_interval * probe_backoff;
                    probe_held_mark = sched.bytes_held();
                }
                _ => sched.on_conn_error(conn, now, cool),
            }
            // The bench, consulted BEFORE the transfer is failed. A dead or
            // hostile source is precisely what a mirror list is for, and giving
            // up while holding fifteen working URLs is the failure this whole
            // path exists to remove.
            if src_fail.get(failed_src).copied().unwrap_or(0) >= SOURCE_FAILS_BEFORE_SUBSTITUTION
                && substitute_source!(failed_src)
            {
                // The streaks describe the source that just left. Carrying them
                // onto its replacement would abort the transfer on the new
                // mirror's first hiccup.
                fail_streak = 0;
                hard_streak = 0;
                last_error = Some(e);
                continue;
            }
            if hard_streak >= 3 {
                for (_, h) in inflight.drain() {
                    h.abort();
                }
                // A redirect travels as `NotConnected` with the location in its
                // message, which is a protocol detail of the fetch path and not
                // something to hand a user verbatim.
                if kind == io::ErrorKind::NotConnected {
                    let loc = e.to_string();
                    let loc = loc.strip_prefix("redirect:").unwrap_or("").to_string();
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("server redirected mid-transfer to {loc}"),
                    ));
                }
                return Err(e);
            }
            last_error = Some(e);
        }

        // 1a2. give one connection back after a refusal-free stretch.
        //
        // A ceiling learned from one burst is a guess about a limit that may have
        // been momentary: a neighbour on the same address, a CDN node shedding
        // load for a second, a per-minute quota that has since rolled over. Left
        // alone, the first burst caps the transfer for its whole life. Probing
        // upward costs one refused request per interval where the limit is real,
        // and restores the transfer's full width where it was not.
        if throttle_cap < sched.n_conns() {
            let now = t0.elapsed().as_secs_f64();
            if now >= next_probe_at && sched.bytes_held() > probe_held_mark {
                throttle_cap += 1;
                // The probe that just went unrefused earns back half the patience
                // the last refusal cost, so a limit that has genuinely lifted is
                // climbed out of at something close to the base interval rather
                // than at whatever the worst refusal streak left behind.
                probe_backoff = (probe_backoff * 0.5).max(1.0);
                // Under a ramp the limit is the ramp's to set; this only lifts the
                // clamp it is measured against.
                if ramp.is_none() {
                    let want = settled_limit.map_or(throttle_cap, |s| s.min(throttle_cap));
                    sched.set_active_limit(want);
                }
                sched.set_conn_ceiling(throttle_cap);
                // The explanation follows the number whether or not a search is
                // running: a cap earned back to the budget is no cap at all, one
                // still below it now serves one more than it did, and a row the
                // SEARCH is holding back must not stay labelled a server limit.
                {
                    use pdl_core::LimitReason as R;
                    sched.set_limit_reason(if ramp.is_some() {
                        R::Measuring
                    } else if throttle_cap >= sched.n_conns() {
                        // The origin's limit is gone. A measured decision outlives
                        // it: those connections were turned down on evidence, not
                        // refused, so the rows must not revert to reading like an
                        // unexplained cap.
                        match sched.limit_reason() {
                            m @ R::Measured { .. } if settled_limit.is_some() => m,
                            _ => R::None,
                        }
                    } else {
                        match sched.limit_reason() {
                            R::Refused { .. } => R::Refused {
                                serving: throttle_cap,
                            },
                            R::Starved { .. } => R::Starved {
                                serving: throttle_cap,
                            },
                            other => other,
                        }
                    });
                }
                next_probe_at = now + probe_interval * probe_backoff;
                probe_held_mark = sched.bytes_held();
                if trace_errors {
                    eprintln!("[trace] t={now:.2} refusal-free: ceiling -> {throttle_cap}");
                }
            }
        }

        // 1b. count what is actually on the wire, then let the ramp decide.
        //
        // A connection counts as delivering once its cursor has moved off the start
        // of its range: bytes have arrived on THIS request, so its handshake, its
        // request and its first byte are all behind it. The scheduler's rate
        // estimate is not the same test — it survives a connection going idle, so
        // it would report a connection as warm before its replacement request has
        // produced anything.
        //
        // Counted every tick rather than only under a ramp, because the refusal
        // floor above needs the same number whether or not a search is running.
        let now_tick = t0.elapsed().as_secs_f64();
        let live = (0..sched.n_conns())
            .filter(|&j| {
                sched
                    .conn_range(j)
                    .map(|(lo, pos, _hi)| pos > lo)
                    .unwrap_or(false)
            })
            .count();
        served_peak_cur = served_peak_cur.max(live);
        if now_tick >= served_peak_rotate_at {
            served_peak_prev = served_peak_cur;
            served_peak_cur = live;
            served_peak_rotate_at = now_tick + probe_interval;
        }

        // Feed the ramp the AGGREGATE delivery.
        //
        // Aggregate rather than per-connection on purpose: on a saturated link each
        // connection's own rate falls as connections are added while the total stays
        // flat, so a per-connection view would read saturation as healthy scaling.
        if let Some(r) = ramp.as_mut() {
            let held = sched.bytes_held();
            r.observe(held.saturating_sub(ramp_last_held), now_tick);
            ramp_last_held = held;
            r.note_delivering(live);
            match r.poll(now_tick, sched.worst_delta().max(1e-3)) {
                pdl_core::Ramp::Raise(n) => sched.set_active_limit(n.min(throttle_cap)),
                pdl_core::Ramp::Settled(settled) => {
                    let n = settled.min(throttle_cap);
                    sched.set_active_limit(n);
                    // Say what was decided and why, in numbers the user can check
                    // against a single-stream download of the same object. A row
                    // that only reads "waiting" after this looks like a dropped
                    // connection; "measured slower" is a decision.
                    if n >= sched.n_conns() {
                        sched.set_limit_reason(pdl_core::LimitReason::None);
                    } else if settled <= throttle_cap {
                        if let Some(v) = r.verdict() {
                            // A search that rejected a level it MEASURED has made a
                            // decision worth keeping. One that merely stopped at a
                            // ceiling it wanted to climb past has not, and pinning
                            // the transfer to it would turn a momentary refusal into
                            // a permanent cap.
                            if v.tried > v.chosen {
                                settled_limit = Some(n);
                            }
                            sched.set_limit_reason(pdl_core::LimitReason::Measured {
                                chosen: n,
                                chosen_rate: v.chosen_rate,
                                tried: v.tried,
                                tried_rate: v.tried_rate,
                            });
                        }
                    }
                    // Otherwise a refusal or starvation clamped the search, and
                    // that reason — already recorded — is the one that explains
                    // the count.
                    // The search is over, so nothing is waiting to be admitted and
                    // the reserve assignment was keeping for later admissions is
                    // now just work the settled connections have to re-request a
                    // share at a time. Collapsing the ceiling onto the settled
                    // count lets them take maximal ranges again.
                    sched.set_conn_ceiling(n);
                    // Stop polling: the search is over and re-running it would
                    // re-pay its cost on a decision already made.
                    ramp = None;
                }
                pdl_core::Ramp::Hold => {}
            }
        }
        if sched.is_complete() {
            // Observe the FINAL state before leaving. The loop breaks here, above
            // the per-tick `observe` call, so without this the last arrivals — the
            // ones that completed the transfer — are never reported: the progress
            // bar stops short of 100%, and a caller deriving completeness from the
            // observed count concludes the transfer is short by whatever landed in
            // the final tick. Measured on an 11 200 900-byte resume: 2 876 bytes
            // unobserved, a byte-exact file reported as incomplete.
            observe(&sched, sched.bytes_held());
            break;
        }

        // 1b2. take in whatever the background prober has admitted.
        //
        // Before the two substitution rules below, because both are gated on
        // the bench being non-empty: a reserve that arrived during this tick is
        // exactly as good as one that was there at the start, and making it wait
        // a round would mean a source failing at the wrong moment finds an empty
        // bench and fails the transfer.
        collect_late!();

        // 1c. replace a source that has gone SILENT rather than wrong.
        //
        // A black-holing mirror — one that accepts connections and sends nothing
        // — never returns an error, so the failure count above never sees it.
        // The scheduler's own stall accounting does, and it is the only signal
        // there is for this case. Checked before the tick so the reclaimed
        // ranges are reassigned to the replacement in the same round rather than
        // handed straight back to the source that is not answering.
        if !bench.is_empty() {
            for src in 0..sched.n_sources() {
                if sched.source_is_live(src)
                    && sched.source_stalls(src) >= SOURCE_STALLS_BEFORE_SUBSTITUTION
                {
                    substitute_source!(src);
                }
            }
        }

        // 1d. replace a source that is WORKING and hopeless.
        //
        // Distinct from both cases above: this mirror answers, delivers bytes,
        // and never errors — it is simply so much slower than the others that
        // the sockets it holds are worth more on a different host. That is not a
        // fault the error count or the stall count can see, and the ranking the
        // publisher supplied cannot see it either: a document says which mirrors
        // it EXPECTS to serve well, and the whole reason this scheduler measures
        // is that the expectation is often wrong.
        //
        // Deliberately conservative — a wide ratio, a long window, and only
        // while there is enough of the object left for a fresh handshake to pay
        // for itself. Repair already moves bytes away from a slow source
        // continuously and for free; swapping is the blunter instrument and is
        // reserved for a source that has stopped contributing at all.
        if !bench.is_empty() && sched.live_sources() > 1 {
            let now = t0.elapsed().as_secs_f64();
            let mut rate = vec![0.0f64; sched.n_sources()];
            for j in 0..sched.n_conns() {
                let src = sched.conn_source(j);
                if let Some(slot) = rate.get_mut(src) {
                    *slot += sched.conn_rate(j).max(0.0);
                }
            }
            let best = rate.iter().copied().fold(0.0f64, f64::max);
            let remaining = size.saturating_sub(sched.bytes_held()) as f64;
            // Enough left to be worth a setup. Near the end of a transfer the
            // slow source is finishing its last range and replacing it would
            // cost a handshake to save nothing.
            let worth_it = best > 0.0 && remaining / best > LAGGARD_MIN_PAYOFF_SECONDS;
            for src in 0..sched.n_sources() {
                if !sched.source_is_live(src) {
                    continue;
                }
                let lagging = worth_it && rate[src] * LAGGARD_RATIO < best;
                match (lagging, laggard_since[src]) {
                    (false, _) => laggard_since[src] = None,
                    (true, None) => laggard_since[src] = Some(now),
                    (true, Some(since)) if now - since >= LAGGARD_SECONDS => {
                        if trace_errors {
                            eprintln!(
                                "[trace] t={now:.2} source {src}: {:.0} B/s against {best:.0} B/s for {:.0}s — replacing",
                                rate[src],
                                now - since
                            );
                        }
                        substitute_source!(src);
                    }
                    (true, Some(_)) => {}
                }
            }
        }

        // 1e. lower the ceiling for connections the origin admits and then STARVES.
        //
        // `throttle_cap` was driven only by a `429`/`503`. An origin that accepts
        // the TCP connection, answers the request, and then serves nothing on it
        // produces no error at all, so the cap never moved and the transfer
        // retried the same losing configuration for its whole life. Measured on
        // `saimei.ftp.acc.umu.se` at `-x 8`: two connections delivered, six sat
        // at 0 B/s for the whole stall timeout, all six were reclaimed at once,
        // re-requested, starved again, and the cycle repeated — a fresh handshake
        // and a lost congestion window each round, 2.2x slower than ONE
        // connection. The adaptive search handled the same origin correctly,
        // because it measures; only the fixed count had nothing to learn from.
        //
        // The evidence is the same shape as a refusal and is read the same way:
        // a connection that was requested and has delivered nothing by the time
        // the scheduler is about to reclaim it for silence, while another on the
        // same origin has been streaming the whole time, is a request the origin
        // has effectively refused. Connections that are delivering are proof of a
        // count this origin serves, and the cap steps down toward it. The
        // refusal-free probe above earns the connection back if the limit was
        // momentary.
        //
        // Decided BEFORE the tick on purpose. The tick that reclaims a starved
        // range also re-assigns it, and with the cap still at the budget it hands
        // the range straight back to a connection the origin is going to starve
        // again. `conn_stalling` is the scheduler's own reclaim predicate, so
        // what is lowered here is exactly what that tick would otherwise re-ask.
        let now = t0.elapsed().as_secs_f64();
        {
            let mut starved = 0usize;
            let mut starved_while_served = false;
            for j in 0..sched.n_conns() {
                // Requested, and nothing has arrived on that request.
                let unfed = inflight.contains_key(&j)
                    && sched
                        .conn_range(j)
                        .map(|(lo, pos, _hi)| pos == lo)
                        .unwrap_or(false);
                if unfed && sched.conn_stalling(j, now) {
                    starved += 1;
                    let src = sched.conn_source(j);
                    let served = (0..sched.n_conns()).any(|k| {
                        k != j
                            && sched.conn_source(k) == src
                            && sched
                                .conn_range(k)
                                .map(|(lo, pos, _hi)| pos > lo)
                                .unwrap_or(false)
                    });
                    starved_while_served |= served;
                }
            }
            if starved > 0 && starved_while_served && now >= cap_settled_until {
                let live_now = (0..sched.n_conns())
                    .filter(|&k| {
                        sched
                            .conn_range(k)
                            .map(|(lo, pos, _hi)| pos > lo)
                            .unwrap_or(false)
                    })
                    .count();
                // Never below what is visibly working, and never below one.
                let floor = live_now.max(served_peak_cur).max(served_peak_prev).max(1);
                // Never more than halve in one round, the same step the refusal
                // path takes. A starvation round costs a whole stall timeout, so
                // the temptation is to jump straight to `floor`; the reason not to
                // is that one badly timed round would then cap the transfer at a
                // single connection, and the upward probe needs one interval per
                // connection to undo it. Halving converges in log2 rounds and
                // bounds what a wrong reading can cost.
                let next = (throttle_cap / 2).max(floor);
                if next < throttle_cap {
                    throttle_cap = next;
                    // One reduction per stall round: the starved connections cross
                    // the timeout together, and the evidence is one fact, not six.
                    cap_settled_until = now + sched.stall_timeout().max(1.0);
                    // Same re-basing as the refusal path: the peak that justified
                    // the old cap says nothing about the new one.
                    served_peak_cur = live_now;
                    served_peak_prev = 0;
                    served_peak_rotate_at = now + probe_interval;
                    if sched.active_limit() > throttle_cap {
                        sched.set_active_limit(throttle_cap);
                    }
                    sched.set_conn_ceiling(throttle_cap);
                    sched.set_limit_reason(pdl_core::LimitReason::Starved {
                        serving: throttle_cap,
                    });
                    if let Some(r) = ramp.as_mut() {
                        r.clamp_max(throttle_cap);
                    }
                    probe_backoff = (probe_backoff * 2.0).min(MAX_PROBE_BACKOFF);
                    next_probe_at = now + probe_interval * probe_backoff;
                    probe_held_mark = sched.bytes_held();
                    if trace_errors {
                        eprintln!(
                            "[trace] t={now:.2} starved: {starved} requested and silent past \
                             the stall timeout, {live_now} delivering, concurrency \
                             ceiling -> {throttle_cap}"
                        );
                    }
                }
            }
        }

        // 2. let the scheduler decide, and act on what it returns
        for act in sched.tick(now) {
            match act {
                Action::Cancel { conn } => {
                    if let Some(h) = inflight.remove(&conn) {
                        h.abort();
                    }
                    bounds.remove(&conn);
                    // Forget the generation, so an outcome already in flight from
                    // the task being aborted is not charged against whatever this
                    // connection is given next.
                    gen_of.remove(&conn);
                }
                // A repair moved this connection's far end down. Publishing it is
                // the entire mechanism: the victim's own loop stops at the new
                // boundary, so the span handed to the taker crosses the wire once
                // rather than twice. No `abort` and no request — that is the point,
                // the connection keeps streaming the part it still owns.
                Action::Shrink { conn, hi } => {
                    if trace_repair {
                        eprintln!("act: t={now:.2} shrink conn={conn} hi={hi}");
                    }
                    if let Some(b) = bounds.get(&conn) {
                        b.shrink_to(hi);
                    }
                }
                Action::Request { conn, range } => {
                    if trace_repair {
                        eprintln!(
                            "act: t={now:.2} request conn={conn} lo={} hi={} ({:.1}MB)",
                            range.lo,
                            range.hi,
                            (range.hi - range.lo) as f64 / 1e6
                        );
                    }
                    if let Some(h) = inflight.remove(&conn) {
                        h.abort(); // supersede: never two live responses per conn
                    }
                    let t = targets[owner[conn]].clone();
                    let (sk, txc, cc) = (sink.clone(), tx.clone(), connector.clone());
                    // Every connection shares ONE limiter, so the cap applies to the
                    // aggregate. Cloning a `Pace` clones the `Arc`, not the bucket.
                    let pc = pace.clone();
                    // A fresh bound per request: a stale one from a superseded
                    // request may already have been shrunk, which would truncate
                    // this range before it started.
                    let bound = crate::Watermark::fixed(range.hi);
                    bounds.insert(conn, bound.clone());
                    let pl = pool.clone();
                    next_gen += 1;
                    let g = next_gen;
                    gen_of.insert(conn, g);
                    let dtx = done_tx.clone();
                    let h = tokio::spawn(async move {
                        let r =
                            fetch_range(cc, conn, t, range.lo, bound, sk, txc, t0, pc, Some(pl))
                                .await;
                        let _ = dtx.send((conn, g, r));
                    });
                    inflight.insert(conn, h);
                }
            }
        }

        // Wait for the TICK, and only the tick.
        //
        // This used to also wake on every arrival — `Some(a) = rx.recv()` beside
        // the ticker — which turned the tick loop into a per-read loop: every 16
        // to 64 KiB that landed on any socket woke this task, credited one
        // arrival, and then ran the whole loop body again, the stalled-connection
        // scan, the repair evaluation, the reason bookkeeping, the progress
        // callback with its per-connection rendering, all of it, before sleeping
        // for the next read. Measured on a 1 GB transfer at four connections
        // against `aria2c`: 19 000 voluntary and 5 700 involuntary context
        // switches against 1 800 and 40, and 40 000 and 22 000 over TLS where the
        // reads are 16 KiB records. The user time was already lower than
        // `aria2c`'s; all of the CPU gap was this.
        //
        // Nothing needs sub-tick latency. Arrivals carry their own timestamps, so
        // rate samples are exact whenever they are credited; the channel is
        // unbounded, so a tick's worth of them queue without back-pressure; and
        // every decision this loop makes is a tick-granularity decision anyway.
        // They are drained at the top of the loop, as they always were.
        ticker.tick().await;
        observe(&sched, sched.bytes_held());

        // Watchdog: any byte of progress resets the clock, so this fires only
        // when the whole transfer — not merely one connection — has been silent.
        // A slow-but-moving source is never killed by it, however slow.
        // Accrue grace for time spent under a deliberate suspension. Measured as
        // elapsed wall clock since the previous iteration rather than as the
        // suspension's nominal length, so a pause that ends early costs only what it
        // actually took.
        {
            let dt = last_tick_at.elapsed().as_secs_f64();
            last_tick_at = Instant::now();
            if sched
                .all_sources_suspended_until(t0.elapsed().as_secs_f64())
                .is_some()
            {
                backoff_grace = (backoff_grace + dt).min(backoff_grace_cap);
            }
        }

        let held_now = sched.bytes_held();
        if held_now > last_held {
            last_held = held_now;
            last_progress_at = Instant::now();
            // Bytes are moving, so whatever failed was transient and its backoff
            // must not accumulate against the next unrelated failure.
            fail_streak = 0;
            hard_streak = 0;
            last_error = None;
        } else if last_progress_at.elapsed().as_secs_f64() > no_progress_deadline + backoff_grace {
            // The deadline is extended by `backoff_grace`: the time the scheduler
            // has DELIBERATELY spent with every source suspended, capped.
            //
            // Silence the scheduler asked for is not a stall. After repeated stalls a
            // source is suspended for a backoff interval, and with a single source —
            // one URL, one CDN, the common case — nothing can move until it expires.
            // Charging that against the no-progress deadline made hydra abort
            // transfers it had itself paused: on a 121.7 MiB GitHub release asset, 4
            // of 8 multi-connection runs died at "no progress for 16s", three of them
            // holding 126.9-127.0 MB of 127.6 MB — 99.6% fetched, reported as failed.
            //
            // The grace is CAPPED, and the cap is the whole design. An uncapped
            // version (reset the clock whenever every source is suspended) hangs
            // forever on a source that black-holes from the first byte: each stall
            // triggers another suspension, which forgives another deadline. That is
            // the failure `a_lone_black_holing_source_fails_instead_of_idling_forever`
            // exists to catch, and it caught it.
            for (_, h) in inflight.drain() {
                h.abort();
            }
            // Report the last transport failure if there was one. "Every source
            // stalled or unreachable" is the symptom; the cause is the error the
            // fetches were actually returning, and a user cannot act on the first
            // without the second.
            let cause = match &last_error {
                Some(e) => format!(" (last error: {e})"),
                None => String::new(),
            };
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "no progress for {no_progress_deadline:.0}s: {} of {} bytes received, \
                     every source stalled or unreachable{cause}",
                    held_now, size
                ),
            ));
        }

        // Wall-clock ceiling scaled to the object: a fixed 120 s aborts a large
        // download on a slow link, which is a bug rather than a safety net. The
        // floor keeps small transfers from hanging indefinitely.
        let budget = (size as f64 / 32_768.0).clamp(120.0, 7200.0);
        if t0.elapsed().as_secs_f64() > budget {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("transfer exceeded {budget:.0}s budget"),
            ));
        }
    }
    for (_, h) in inflight.drain() {
        h.abort();
    }
    if report_pool {
        let (hits, misses) = pool.stats();
        eprintln!(
            "pool: {hits} reused, {misses} fresh ({} requests, {} repairs, {} reclaims)",
            sched.stats.requests, sched.stats.repairs, sched.stats.reclaims
        );
    }
    Ok((t0.elapsed().as_secs_f64(), sched.stats.requests))
}
