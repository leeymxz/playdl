// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Durable job state persistence and restoration.
//!
//! Stores job metadata and range maps to disk while stripping sensitive credentials.

use crate::abi::{hydra_error_code_t as E, hydra_job_state_t as S};
use crate::engine::{Algo, Creds, Engine, JobCfg, ProxyCfg};
use crate::err::Detail;
use serde::{Deserialize, Serialize};
use std::sync::atomic::Ordering;
use std::sync::Arc;

/// State file schema version.
const STATE_VERSION: u32 = 1;

/// Request headers stripped before writing state to disk.
const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

#[derive(Serialize, Deserialize)]
struct ProxyRecord {
    kind: String,
    host: String,
    port: u16,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct JobRecord {
    id: u64,
    urls: Vec<String>,
    output_path: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    withheld_headers: Vec<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    proxy: Option<ProxyRecord>,
    #[serde(default)]
    checksum: Option<(String, String)>,
    #[serde(default)]
    max_connections: u32,
    #[serde(default)]
    max_retries: u32,
    #[serde(default)]
    priority: u32,
    #[serde(default)]
    max_bytes_per_second: u64,
    #[serde(default = "yes")]
    resume: bool,
    #[serde(default = "yes")]
    adaptive: bool,
    /// Mirror ranking as `(priority, max_connections)`, index-aligned with
    /// `urls`. Empty for a job that never read a mirror list.
    ///
    /// Persisted because a restored job that lost its ranking would open the
    /// same mirrors in a different order and lose its reserve bench — a
    /// difference invisible until the mirror that failed before fails again.
    #[serde(default)]
    source_plans: Vec<(u32, u32)>,
    /// The size a Metalink document attested. Without it a restored job falls
    /// back to the pairwise validator gate, which a real mirror list cannot
    /// satisfy, so it would resume from one source instead of four.
    #[serde(default)]
    attested_size: Option<u64>,
    /// The document's `<pieces>` manifest, in its on-disk JSON form.
    #[serde(default)]
    pieces: Option<String>,
    /// Where the attestation came from, for the log.
    #[serde(default)]
    attested_by: Option<String>,

    state: u32,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    resolved_url: Option<String>,
    #[serde(default)]
    held: Vec<(u64, u64)>,
    #[serde(default)]
    created_at_ms: u64,
    #[serde(default)]
    started_at_ms: u64,
    #[serde(default)]
    finished_at_ms: u64,
    #[serde(default)]
    error_code: u32,
    #[serde(default)]
    error_message: String,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
struct StateFile {
    version: u32,
    #[serde(default)]
    saved_at_ms: u64,
    jobs: Vec<JobRecord>,
}

fn no_state_path() -> Detail {
    Detail {
        code: E::HYDRA_ERR_INVALID_STATE as u32,
        message: "engine was created without a state_path, so there is nowhere to persist to"
            .into(),
        ..Default::default()
    }
}

fn hex(bytes: &[u8]) -> String {
    pdl_net::digest::to_lower_hex(bytes)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok())
        .collect()
}

fn algo_from_str(s: &str) -> Option<Algo> {
    match s {
        "md5" => Some(Algo::Md5),
        "sha1" => Some(Algo::Sha1),
        "sha256" => Some(Algo::Sha256),
        "sha512" => Some(Algo::Sha512),
        "blake3" => Some(Algo::Blake3),
        _ => None,
    }
}

fn proxy_kind_name(k: &pdl_net::ProxyKind) -> String {
    k.as_str().to_string()
}

fn proxy_kind_from(s: &str) -> Option<pdl_net::ProxyKind> {
    match s {
        "http" => Some(pdl_net::ProxyKind::Http),
        "socks4" => Some(pdl_net::ProxyKind::Socks4),
        "socks4a" => Some(pdl_net::ProxyKind::Socks4a),
        "socks5" => Some(pdl_net::ProxyKind::Socks5),
        _ => None,
    }
}

/// Atomically saves engine job state to the configured `state_path`.
pub(crate) fn save(engine: &Arc<Engine>) -> Result<(), Detail> {
    let path = engine.cfg.state_path.clone().ok_or_else(no_state_path)?;
    let _serialise = crate::driver::PERSIST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let jobs: Vec<JobRecord> =
        engine
            .all_jobs()
            .into_iter()
            .map(|job| {
                let creds = job.creds();
                let g = job.lock();
                let (kept, withheld): (Vec<_>, Vec<_>) =
                    job.cfg.headers.iter().cloned().partition(|(n, _)| {
                        !SECRET_HEADERS.contains(&n.to_ascii_lowercase().as_str())
                    });
                JobRecord {
                    id: job.id,
                    urls: job.cfg.urls.clone(),
                    output_path: g.output_path.clone(),
                    headers: kept,
                    withheld_headers: withheld.into_iter().map(|(n, _)| n).collect(),
                    username: creds.username.clone(),
                    proxy: job.cfg.proxy.as_ref().map(|p| ProxyRecord {
                        kind: proxy_kind_name(&p.kind),
                        host: p.host.clone(),
                        port: p.port,
                        username: p.username.clone(),
                    }),
                    checksum: job
                        .cfg
                        .checksum
                        .as_ref()
                        .map(|(a, d)| (a.as_str().to_string(), hex(d))),
                    max_connections: job.cfg.max_connections as u32,
                    max_retries: job.cfg.max_retries,
                    priority: job.cfg.priority,
                    max_bytes_per_second: job.cfg.max_bytes_per_second,
                    resume: job.cfg.resume,
                    adaptive: job.cfg.adaptive,
                    source_plans: job
                        .cfg
                        .source_plans
                        .iter()
                        .map(|p| (p.priority, p.max_connections.unwrap_or(0).min(64) as u32))
                        .collect(),
                    attested_size: job.cfg.attested_size,
                    // Bounded: the state file is rewritten on every autosave,
                    // and the parser admits piece lists that serialize to tens
                    // of megabytes. Past the cap the grid is dropped from the
                    // RECORD only — the running job keeps verifying with it,
                    // and a restored job falls back to the whole-file checksum.
                    pieces: job
                        .cfg
                        .pieces
                        .as_ref()
                        .map(|m| m.to_json())
                        .filter(|j| j.len() <= 4 << 20),
                    attested_by: job.cfg.attested_by.clone(),
                    // A job that was executing when the process stopped is recorded
                    // as paused. It is the truth about the file on disk — bytes are
                    // there, nothing is moving — and it is the state from which
                    // `hydra_job_resume` is legal.
                    state: if g.is_running() {
                        S::HYDRA_JOB_PAUSED as u32
                    } else {
                        g.state
                    },
                    size: g.size,
                    file_name: g.file_name.clone(),
                    resolved_url: g.resolved_url.clone(),
                    held: g.held.clone(),
                    created_at_ms: g.created_at_ms,
                    started_at_ms: g.started_at_ms,
                    finished_at_ms: g.finished_at_ms,
                    error_code: g.error.as_ref().map(|e| e.code).unwrap_or(0),
                    error_message: g
                        .error
                        .as_ref()
                        .map(|e| e.message.clone())
                        .unwrap_or_default(),
                }
            })
            .collect();

    let file = StateFile {
        version: STATE_VERSION,
        saved_at_ms: crate::engine::now_ms(),
        jobs,
    };
    let body = serde_json::to_vec_pretty(&file).map_err(|e| Detail {
        code: E::HYDRA_ERR_INTERNAL as u32,
        message: format!("cannot serialise engine state: {e}"),
        ..Default::default()
    })?;

    if let Some(dir) = std::path::Path::new(&path).parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir).map_err(|e| crate::err::from_io(&e))?;
        }
    }
    let tmp = format!("{path}.tmp");
    std::fs::write(&tmp, &body).map_err(|e| crate::err::from_io(&e))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        crate::err::from_io(&e)
    })?;
    Ok(())
}

/// Save without reporting failure, for the automatic write after every terminal
/// transition.
///
/// Best effort by design: a job that just completed has completed whether or not
/// its bookkeeping could be written, and turning a full disk in the state
/// directory into a failed download would be the wrong trade. A caller that
/// needs to know calls `hydra_engine_snapshot` and reads the code.
pub(crate) fn autosave(engine: &Arc<Engine>) {
    if engine.cfg.state_path.is_none() {
        return;
    }
    let _ = save(engine);
}

/// Load persisted jobs into the engine.
///
/// Restores identities, not execution: every restored job that was running is
/// `HYDRA_JOB_PAUSED`, and nothing starts until the application says so. That
/// is deliberate — on Android or iOS the decision to run belongs to the
/// platform layer, which knows whether the app is foregrounded, whether the
/// network is metered and whether a service owns the work.
///
/// Returns how many jobs were restored. Jobs whose ids are already present are
/// skipped rather than overwritten: a live job is a better description of
/// reality than a file written before the process restarted.
pub(crate) fn restore(engine: &Arc<Engine>) -> Result<usize, Detail> {
    let path = engine.cfg.state_path.clone().ok_or_else(no_state_path)?;
    let _serialise = crate::driver::PERSIST_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());

    let body = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Detail {
                code: E::HYDRA_ERR_NOT_FOUND as u32,
                message: format!("no persisted state at {path}"),
                ..Default::default()
            })
        }
        Err(e) => return Err(crate::err::from_io(&e)),
    };
    let file: StateFile = serde_json::from_slice(&body).map_err(|e| Detail {
        code: E::HYDRA_ERR_PROTOCOL as u32,
        message: format!("{path} is not a hydra state file: {e}"),
        ..Default::default()
    })?;
    if file.version > STATE_VERSION {
        return Err(Detail {
            code: E::HYDRA_ERR_UNSUPPORTED as u32,
            message: format!(
                "{path} was written by a newer hydra (state version {}, this build reads {STATE_VERSION})",
                file.version
            ),
            ..Default::default()
        });
    }

    let mut n = 0usize;
    for r in file.jobs {
        if r.id == 0 || engine.job(r.id).is_some() {
            continue;
        }
        if r.urls.is_empty() || r.output_path.is_empty() {
            continue;
        }
        let checksum = r.checksum.as_ref().and_then(|(a, d)| {
            let algo = algo_from_str(a)?;
            let bytes = unhex(d)?;
            (bytes.len() == algo.len()).then_some((algo, bytes))
        });
        let proxy = r.proxy.as_ref().and_then(|p| {
            Some(ProxyCfg {
                kind: proxy_kind_from(&p.kind)?,
                host: p.host.clone(),
                port: p.port,
                username: p.username.clone(),
                // Never persisted; re-supplied by the application if needed.
                password: None,
            })
        });
        let cfg = JobCfg {
            urls: r.urls,
            headers: r.headers,
            withheld_headers: r.withheld_headers,
            proxy,
            checksum,
            max_connections: r.max_connections as usize,
            max_retries: if r.max_retries == 0 {
                engine.cfg.max_retries
            } else {
                r.max_retries
            },
            priority: r.priority.min(2),
            max_bytes_per_second: r.max_bytes_per_second,
            resume: r.resume,
            adaptive: r.adaptive,
            source_plans: r
                .source_plans
                .iter()
                .map(|&(priority, cap)| pdl_core::SourcePlan {
                    priority,
                    max_connections: (cap > 0).then_some(cap as usize),
                })
                .collect(),
            attested_size: r.attested_size,
            // A manifest that no longer parses is dropped rather than failing
            // the restore: the object is still fetchable and still checked
            // against its whole-file digest, and refusing to restore the job
            // would lose the range map as well.
            pieces: r
                .pieces
                .as_deref()
                .and_then(|j| pdl_net::manifest::Manifest::parse(j).ok()),
            attested_by: r.attested_by,
        };
        let job = engine.insert_job_with_id(
            r.id,
            cfg,
            r.output_path,
            Creds {
                username: r.username,
                password: None,
            },
        );
        let mut g = job.lock();
        g.state = r.state.min(S::HYDRA_JOB_CANCELLED as u32);
        g.size = r.size;
        g.file_name = r.file_name;
        g.resolved_url = r.resolved_url;
        g.held = r.held;
        g.created_at_ms = r.created_at_ms;
        g.started_at_ms = r.started_at_ms;
        g.finished_at_ms = r.finished_at_ms;
        g.progress.bytes_downloaded = g.held.iter().map(|(lo, hi)| hi.saturating_sub(*lo)).sum();
        g.progress.total_bytes = g.size.unwrap_or(0);
        g.progress.completed_ranges = g.held.len() as u32;
        g.progress.total_ranges = g.held.len() as u32;
        if r.error_code != 0 {
            g.error = Some(Detail {
                code: r.error_code,
                os_error: 0,
                http_status: 0,
                message: r.error_message,
            });
        }
        drop(g);
        n += 1;
    }
    engine
        .metrics
        .jobs_created
        .fetch_sub(n as u64, Ordering::Relaxed);
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let b = vec![0u8, 1, 0x0f, 0xff, 0xa5];
        assert_eq!(unhex(&hex(&b)).unwrap(), b);
        assert!(unhex("abc").is_none(), "odd length is not a digest");
        assert!(unhex("zz").is_none());
    }

    #[test]
    fn secret_headers_are_recognised_case_insensitively() {
        for h in [
            "Authorization",
            "AUTHORIZATION",
            "Cookie",
            "Proxy-Authorization",
        ] {
            assert!(
                SECRET_HEADERS.contains(&h.to_ascii_lowercase().as_str()),
                "{h} must never be written to a state file"
            );
        }
        assert!(!SECRET_HEADERS.contains(&"accept"));
    }
}
