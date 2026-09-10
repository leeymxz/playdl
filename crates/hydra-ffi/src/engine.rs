// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Core engine architecture, job representations, and connection management.

use crate::abi::{
    hydra_error_code_t as E, hydra_event_t, hydra_event_type_t as EV, hydra_job_state_t as S,
    hydra_metrics_t, hydra_progress_t, hydra_runtime_policy_t,
};
use crate::err::Detail;
use crate::event::EventQueue;
use crate::gate::Gate;
use pdl_net::polite::RateLimiter;
use pdl_net::TlsCapableConnector;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

/// Returns milliseconds since Unix epoch.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ------------------------------------------------------------ owned settings

/// Internal engine configuration.
#[derive(Clone, Debug)]
pub(crate) struct EngineCfg {
    pub max_jobs: usize,
    pub max_connections: usize,
    pub max_retries: u32,
    pub progress_interval_ms: u64,
    pub event_queue_capacity: usize,
    pub worker_threads: usize,
    pub max_bytes_per_second: u64,
    pub adaptive_concurrency: bool,
    pub range_stealing: bool,
    pub allow_insecure_tls: bool,
    pub state_path: Option<String>,
    pub user_agent: String,
}

impl Default for EngineCfg {
    fn default() -> Self {
        Self {
            max_jobs: 4,
            max_connections: 8,
            max_retries: 3,
            progress_interval_ms: 250,
            event_queue_capacity: 1024,
            worker_threads: 0,
            max_bytes_per_second: 0,
            adaptive_concurrency: true,
            range_stealing: true,
            allow_insecure_tls: false,
            state_path: None,
            user_agent: pdl_net::DEFAULT_USER_AGENT.to_string(),
        }
    }
}

/// Supported checksum algorithms for post-download verification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Blake3,
}

impl Algo {
    /// Expected byte length of the checksum digest.
    pub(crate) fn len(self) -> usize {
        match self {
            Algo::Md5 => 16,
            Algo::Sha1 => 20,
            Algo::Sha256 => 32,
            Algo::Sha512 => 64,
            Algo::Blake3 => 32,
        }
    }

    /// String name of the algorithm.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Algo::Md5 => "md5",
            Algo::Sha1 => "sha1",
            Algo::Sha256 => "sha256",
            Algo::Sha512 => "sha512",
            Algo::Blake3 => "blake3",
        }
    }
}

/// Internal proxy configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProxyCfg {
    pub kind: pdl_net::ProxyKind,
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl ProxyCfg {
    /// Generates a cache key for connection pool sharing.
    pub(crate) fn cache_key(&self) -> String {
        format!(
            "{}://{}:{}#{}",
            self.kind.as_str(),
            self.host,
            self.port,
            self.username.as_deref().unwrap_or("")
        )
    }

    /// Converts to network crate proxy struct.
    pub(crate) fn to_net(&self) -> pdl_net::Proxy {
        pdl_net::Proxy {
            kind: self.kind.clone(),
            host: self.host.clone(),
            port: self.port,
            username: self.username.clone(),
            password: self.password.clone(),
        }
    }
}

/// Per-job authentication credentials.
#[derive(Clone, Debug, Default)]
pub(crate) struct Creds {
    pub username: Option<String>,
    pub password: Option<String>,
}

/// Internal job configuration parameters.
#[derive(Clone, Debug)]
pub(crate) struct JobCfg {
    pub urls: Vec<String>,
    pub headers: Vec<(String, String)>,
    pub withheld_headers: Vec<String>,
    pub proxy: Option<ProxyCfg>,
    pub checksum: Option<(Algo, Vec<u8>)>,
    pub max_connections: usize,
    pub max_retries: u32,
    pub priority: u32,
    pub max_bytes_per_second: u64,
    pub resume: bool,
    pub adaptive: bool,
    /// Publisher ranking and per-mirror ceilings, index-aligned with `urls`.
    ///
    /// Empty for a job created from a plain URL list, which is exactly what an
    /// unranked set of interchangeable mirrors is â€?so nothing changes for a
    /// caller that never touches a Metalink.
    pub source_plans: Vec<pdl_core::SourcePlan>,
    /// Object size stated by a Metalink document rather than by a mirror.
    ///
    /// This is what makes multi-source assembly possible across a real mirror
    /// list. Without it, agreement has to be established pairwise on a strong
    /// validator, and independent mirror operators cannot share an `ETag` â€?so
    /// the gate keeps exactly one source out of nineteen. A size published by
    /// whoever built the object, from a host that is usually not any of the
    /// mirrors, is both stronger evidence and satisfiable.
    pub attested_size: Option<u64>,
    /// Per-chunk digests published by the document, when its `<pieces>` tile the
    /// stated size.
    ///
    /// Verified after the transfer, with a failing chunk refetched from a
    /// different mirror. That is the difference between "the download is
    /// corrupt, start again" and "chunk 412 is corrupt, refetch 4 MiB".
    pub pieces: Option<pdl_net::manifest::Manifest>,
    /// Where the size, digest and pieces came from, for the log.
    pub attested_by: Option<String>,
}

// ------------------------------------------------------------------- the job

/// Reason for requesting job cancellation or pause.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Running normally.
    None,
    /// Pause download and preserve range map.
    Pause,
    /// Cancel download and retain partial file.
    CancelKeep,
    /// Cancel download and delete partial file.
    CancelRemove,
}

/// Source statistics for active mirror inspection.
#[derive(Clone, Debug, Default)]
pub(crate) struct SourceStat {
    pub url: String,
    pub bytes: u64,
    pub rate: u64,
    pub latency_us: u64,
    pub conns: u32,
    pub errors: u32,
    pub active: bool,
}

/// Dynamic runtime state of a download job.
#[derive(Debug)]
pub(crate) struct JobState {
    pub state: u32,
    pub progress: hydra_progress_t,
    /// Byte spans already on disk. This is what makes resume real: the ranges,
    /// not the file length, because positioned writes leave holes.
    pub held: Vec<(u64, u64)>,
    pub size: Option<u64>,
    pub file_name: Option<String>,
    /// The URL actually being fetched, after redirects.
    pub resolved_url: Option<String>,
    pub output_path: String,
    pub error: Option<Detail>,
    pub stop: Stop,
    pub sources: Vec<SourceStat>,
    pub created_at_ms: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    /// The running attempt's stop flag, if the job is executing.
    pub cancel: Option<Arc<AtomicBool>>,
    /// Bumped every time the job is started, so a late finishing attempt cannot
    /// overwrite the state of the attempt that replaced it.
    pub generation: u64,
}

impl JobState {
    fn new(output_path: String) -> Self {
        Self {
            state: S::HYDRA_JOB_CREATED as u32,
            progress: hydra_progress_t::default(),
            held: Vec::new(),
            size: None,
            file_name: None,
            resolved_url: None,
            output_path,
            error: None,
            stop: Stop::None,
            sources: Vec::new(),
            created_at_ms: now_ms(),
            started_at_ms: 0,
            finished_at_ms: 0,
            cancel: None,
            generation: 0,
        }
    }

    /// True once the job can no longer change on its own.
    pub(crate) fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            x if x == S::HYDRA_JOB_COMPLETED as u32
                || x == S::HYDRA_JOB_FAILED as u32
                || x == S::HYDRA_JOB_CANCELLED as u32
        )
    }

    /// True while an attempt is executing.
    pub(crate) fn is_running(&self) -> bool {
        matches!(
            self.state,
            x if x == S::HYDRA_JOB_QUEUED as u32
                || x == S::HYDRA_JOB_RESOLVING as u32
                || x == S::HYDRA_JOB_DOWNLOADING as u32
                || x == S::HYDRA_JOB_VERIFYING as u32
        )
    }
}

/// A durable download.
pub(crate) struct Job {
    pub id: u64,
    /// Creation order, for FIFO within a priority band.
    pub seq: u64,
    pub cfg: JobCfg,
    pub st: Mutex<JobState>,
    /// Credentials, replaceable after a restore. See [`Creds`].
    pub creds_cell: Mutex<Creds>,
    /// This job's rate ceiling. Held across attempts so a live `set_limit` is
    /// not lost when a transfer retries.
    pub limiter: Arc<RateLimiter>,
}

impl std::fmt::Debug for Job {
    /// Names the job without printing its credentials or its URL's userinfo.
    /// A `Debug` derive would put a password into every log line and panic
    /// message that formats one.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("state", &self.lock().state)
            .field("sources", &self.cfg.urls.len())
            .finish()
    }
}

impl Job {
    pub(crate) fn lock(&self) -> MutexGuard<'_, JobState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A copy of the current credentials.
    pub(crate) fn creds(&self) -> Creds {
        self.creds_cell
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Replace the credentials. Takes effect on the next attempt.
    pub(crate) fn set_creds(&self, c: Creds) {
        *self.creds_cell.lock().unwrap_or_else(|p| p.into_inner()) = c;
    }
}

// -------------------------------------------------------------------- counts

/// Engine-wide counters.
#[derive(Debug, Default)]
pub(crate) struct Metrics {
    pub bytes_received: AtomicU64,
    pub bytes_written: AtomicU64,
    pub request_count: AtomicU64,
    pub retry_count: AtomicU64,
    pub error_count: AtomicU64,
    pub stall_count: AtomicU64,
    pub jobs_created: AtomicU64,
    pub jobs_completed: AtomicU64,
    pub jobs_failed: AtomicU64,
}

impl Metrics {
    fn snapshot(&self, events_dropped: u64) -> hydra_metrics_t {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        hydra_metrics_t {
            bytes_received: g(&self.bytes_received),
            bytes_written: g(&self.bytes_written),
            request_count: g(&self.request_count),
            retry_count: g(&self.retry_count),
            error_count: g(&self.error_count),
            stall_count: g(&self.stall_count),
            jobs_created: g(&self.jobs_created),
            jobs_completed: g(&self.jobs_completed),
            jobs_failed: g(&self.jobs_failed),
            events_dropped,
        }
    }
}

// ----------------------------------------------------------------- theengine

/// The shared engine state. Cloned as an `Arc` into every running transfer.
pub(crate) struct Engine {
    pub cfg: EngineCfg,
    /// A handle rather than the runtime itself: the runtime is owned by the
    /// C-visible handle box, because dropping a `Runtime` from inside one of its
    /// own worker threads panics, and a transfer task holding an `Arc<Engine>`
    /// is exactly such a thread.
    pub rt: tokio::runtime::Handle,
    pub jobs: Mutex<BTreeMap<u64, Arc<Job>>>,
    pub events: Arc<EventQueue>,
    pub gate: Arc<Gate>,
    /// The aggregate ceiling across every job.
    pub limiter: Arc<RateLimiter>,
    pub policy: Mutex<hydra_runtime_policy_t>,
    pub metrics: Metrics,
    /// This engine's diagnostics sink. Per engine rather than per process, so
    /// two independent consumers in one process cannot reconfigure each
    /// other's logging.
    pub logs: crate::log::LogSink,
    pub shutdown: AtomicBool,
    next_id: AtomicU64,
    next_seq: AtomicU64,
    /// One connector per proxy configuration, shared across every job that uses
    /// it. Connectors carry a connection pool, a TLS session cache and a parsed
    /// root store, all of which are designed to outlive a single transfer â€?
    /// hya-net measures 1.6-2.0 s of setup recovered when a probe's handshake
    /// feeds the transfer that follows it. Building one per job throws all
    /// three away.
    connectors: Mutex<HashMap<String, Arc<TlsCapableConnector>>>,
}

impl Engine {
    pub(crate) fn new(cfg: EngineCfg, rt: tokio::runtime::Handle) -> Arc<Self> {
        // 0 means unlimited. Every job's transfer holds this limiter whatever
        // its rate, and reads that rate live, so the engine-wide cap can be
        // raised, lowered or switched on while transfers are running.
        let limiter = Arc::new(RateLimiter::new(cfg.max_bytes_per_second));
        Arc::new(Self {
            events: Arc::new(EventQueue::new(cfg.event_queue_capacity)),
            gate: Arc::new(Gate::new(cfg.max_jobs)),
            limiter,
            policy: Mutex::new(hydra_runtime_policy_t {
                network_policy: 0,
                power_mode: 0,
                allow_cellular: 1,
                allow_metered: 1,
                pause_on_low_battery: 0,
                pause_when_backgrounded: 0,
                reserved: [0; 4],
            }),
            metrics: Metrics::default(),
            logs: crate::log::LogSink::default(),
            shutdown: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            next_seq: AtomicU64::new(0),
            jobs: Mutex::new(BTreeMap::new()),
            connectors: Mutex::new(HashMap::new()),
            cfg,
            rt,
        })
    }

    fn jobs(&self) -> MutexGuard<'_, BTreeMap<u64, Arc<Job>>> {
        self.jobs.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Register a job and return its durable id.
    pub(crate) fn insert_job(&self, cfg: JobCfg, output_path: String, creds: Creds) -> Arc<Job> {
        self.insert_job_with_id(
            self.next_id.fetch_add(1, Ordering::Relaxed),
            cfg,
            output_path,
            creds,
        )
    }

    /// Register a job under a specific id, for restore.
    pub(crate) fn insert_job_with_id(
        &self,
        id: u64,
        cfg: JobCfg,
        output_path: String,
        creds: Creds,
    ) -> Arc<Job> {
        // Keep the allocator ahead of any restored id, so a job created after a
        // restore cannot collide with one that came out of the state file.
        self.next_id.fetch_max(id + 1, Ordering::Relaxed);
        let limiter = Arc::new(RateLimiter::new(cfg.max_bytes_per_second));
        let job = Arc::new(Job {
            id,
            seq: self.next_seq.fetch_add(1, Ordering::Relaxed),
            cfg,
            st: Mutex::new(JobState::new(output_path)),
            creds_cell: Mutex::new(creds),
            limiter,
        });
        self.jobs().insert(id, job.clone());
        self.metrics.jobs_created.fetch_add(1, Ordering::Relaxed);
        job
    }

    /// Look a job up by id.
    pub(crate) fn job(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs().get(&id).cloned()
    }

    /// Forget a job. Only legal once it is no longer running.
    pub(crate) fn remove_job(&self, id: u64) -> Option<Arc<Job>> {
        self.jobs().remove(&id)
    }

    /// Every job, in creation order.
    pub(crate) fn all_jobs(&self) -> Vec<Arc<Job>> {
        self.jobs().values().cloned().collect()
    }

    /// The connector for a job's proxy configuration, built once and reused.
    pub(crate) fn connector(
        &self,
        proxy: Option<&ProxyCfg>,
    ) -> Result<Arc<TlsCapableConnector>, Detail> {
        let key = match proxy {
            // SOCKS is handled inside the connector; an HTTP proxy is handled
            // by the request target, so both share the direct connector.
            Some(p) if p.kind.is_socks() => p.cache_key(),
            _ => "direct".to_string(),
        };
        let mut g = self.connectors.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = g.get(&key) {
            return Ok(c.clone());
        }
        let built = TlsCapableConnector::with_insecure(self.cfg.allow_insecure_tls)
            .map(|c| match proxy {
                Some(p) if p.kind.is_socks() => c.with_socks(p.to_net()),
                _ => c,
            })
            .map_err(|e| Detail {
                code: E::HYDRA_ERR_NETWORK as u32,
                os_error: e.raw_os_error().unwrap_or(0),
                http_status: 0,
                message: format!("TLS setup failed: {e}"),
            })?;
        let arc = Arc::new(built);
        g.insert(key, arc.clone());
        Ok(arc)
    }

    /// Snapshot the counters.
    pub(crate) fn metrics(&self) -> hydra_metrics_t {
        self.metrics.snapshot(self.events.dropped())
    }

    /// The active policy.
    pub(crate) fn policy(&self) -> hydra_runtime_policy_t {
        *self.policy.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The per-job connection ceiling under the current power mode.
    ///
    /// The mode is supplied by the platform layer, never read from a battery
    /// API here â€?that is the line that keeps the core free of Android and iOS
    /// code. A restricted device gets one connection; battery saver gets half
    /// the ceiling.
    pub(crate) fn connection_ceiling(&self, requested: usize) -> usize {
        let base = if requested == 0 {
            self.cfg.max_connections
        } else {
            requested.min(self.cfg.max_connections)
        };
        match self.policy().power_mode {
            2 => 1,
            1 => (base / 2).max(1),
            _ => base.max(1),
        }
    }

    /// The progress-event interval under the current power mode.
    pub(crate) fn progress_interval_ms(&self) -> u64 {
        let base = self.cfg.progress_interval_ms.max(10);
        match self.policy().power_mode {
            2 => base.max(2000),
            1 => base.max(1000),
            _ => base,
        }
    }

    /// Whether the platform layer currently permits network use at all.
    ///
    /// Returns the reason when it does not, so the refusal reaching the
    /// application says which policy blocked it rather than "failed".
    pub(crate) fn network_blocked(&self) -> Option<&'static str> {
        let p = self.policy();
        match p.network_policy {
            1 if p.allow_metered == 0 => {
                Some("policy requires an unmetered network and metered use is disallowed")
            }
            2 if p.allow_cellular != 0 => None,
            _ => None,
        }
    }

    /// Publish a job event with the job's current progress and state attached.
    pub(crate) fn emit(&self, job: &Job, kind: EV) {
        let (state, progress) = {
            let g = job.lock();
            (g.state, g.progress)
        };
        self.events.push(hydra_event_t {
            kind,
            state: crate::err::to_state(state),
            job_id: job.id,
            progress,
            error: E::HYDRA_OK,
            http_status: 0,
            os_error: 0,
            reserved: 0,
            timestamp_ms: now_ms(),
            dropped_events: 0,
        });
    }

    /// Publish a failure event carrying the structured detail.
    pub(crate) fn emit_error(&self, job: &Job, kind: EV, d: &Detail) {
        let (state, progress) = {
            let g = job.lock();
            (g.state, g.progress)
        };
        self.events.push(hydra_event_t {
            kind,
            state: crate::err::to_state(state),
            job_id: job.id,
            progress,
            error: crate::err::to_code(d.code),
            http_status: d.http_status,
            os_error: d.os_error,
            reserved: 0,
            timestamp_ms: now_ms(),
            dropped_events: 0,
        });
    }
}
