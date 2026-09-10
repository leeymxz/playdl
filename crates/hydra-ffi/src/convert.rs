// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Input validation and conversion from C ABI types to internal engine models.

use crate::abi::*;
use crate::engine::{Algo, Creds, EngineCfg, JobCfg, ProxyCfg};
use crate::err::Detail;
use crate::mem::{cstr_opt, cstr_req};
use hydra_error_code_t as E;

/// Maximum allowable size in bytes for a configuration struct.
const MAX_CONFIG_BYTES: usize = 4096;

/// Reads a versioned configuration struct from caller memory.
///
/// Truncates copy to caller's declared `size` and defaults remaining fields to zero.
///
/// # Safety
/// `p` must be non-null and point to at least `p->size` readable bytes.
unsafe fn read_versioned<T: Copy>(
    p: *const T,
    max_version: u32,
    what: &'static str,
) -> Result<T, Detail> {
    let bad = |m: String| Detail {
        code: E::HYDRA_ERR_INVALID_ARGUMENT as u32,
        message: m,
        ..Default::default()
    };
    if p.is_null() {
        return Err(bad(format!("{what} is NULL")));
    }
    let head = p as *const u32;
    // SAFETY: caller guarantees `p` points to a valid struct starting with size and version fields.
    let size = unsafe { head.read_unaligned() } as usize;
    // SAFETY: offset is within the 8-byte minimum header prefix.
    let version = unsafe { head.add(1).read_unaligned() };
    if !(8..=MAX_CONFIG_BYTES).contains(&size) {
        return Err(bad(format!(
            "{what}.size is {size}; call hydra_{what}_init first"
        )));
    }
    if version == 0 || version > max_version {
        return Err(bad(format!(
            "{what}.version is {version}; this build supports 1..={max_version}"
        )));
    }
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    let n = size.min(std::mem::size_of::<T>());
    // SAFETY: copying `n` bytes from valid source into zero-initialised target buffer.
    unsafe {
        std::ptr::copy_nonoverlapping(p as *const u8, out.as_mut_ptr() as *mut u8, n);
        Ok(out.assume_init())
    }
}

/// Validates that an integer discriminant is within enum bounds.
fn enum_in_range(v: u32, max: u32, what: &str) -> Result<u32, Detail> {
    if v > max {
        return Err(Detail {
            code: E::HYDRA_ERR_INVALID_ARGUMENT as u32,
            message: format!("{what} is {v}, which is not a valid value"),
            ..Default::default()
        });
    }
    Ok(v)
}

fn invalid(msg: impl Into<String>) -> Detail {
    Detail {
        code: E::HYDRA_ERR_INVALID_ARGUMENT as u32,
        message: msg.into(),
        ..Default::default()
    }
}

/// Validates and converts an engine configuration from C ABI.
///
/// # Safety
/// `p` must point to a readable `hydra_engine_config_t`.
pub(crate) unsafe fn engine_cfg(p: *const hydra_engine_config_t) -> Result<EngineCfg, Detail> {
    // SAFETY: caller provides valid pointer to engine config struct.
    let c = unsafe { read_versioned(p, crate::HYDRA_ENGINE_CONFIG_VERSION, "engine_config")? };
    let mut out = EngineCfg::default();

    if c.max_jobs != 0 {
        out.max_jobs = c.max_jobs.min(4096) as usize;
    }
    if c.max_connections != 0 {
        out.max_connections = c.max_connections.min(64) as usize;
    }
    if c.max_retries != 0 {
        out.max_retries = c.max_retries.min(64);
    }
    if c.progress_interval_ms != 0 {
        out.progress_interval_ms = c.progress_interval_ms.max(10) as u64;
    }
    if c.event_queue_capacity != 0 {
        out.event_queue_capacity = c.event_queue_capacity.clamp(8, 1 << 20) as usize;
    }
    out.worker_threads = c.worker_threads.min(1024) as usize;
    out.max_bytes_per_second = c.max_bytes_per_second;

    let reaches = |off: usize| (c.size as usize) >= off;
    let flags_off = std::mem::offset_of!(hydra_engine_config_t, reserved0) + 1;
    if reaches(flags_off) {
        out.adaptive_concurrency = c.adaptive_concurrency != 0;
        out.range_stealing = c.range_stealing != 0;
        out.allow_insecure_tls = c.allow_insecure_tls != 0;
    }

    enum_in_range(c.network_policy, 2, "engine_config.network_policy")?;
    enum_in_range(c.power_mode, 2, "engine_config.power_mode")?;

    // SAFETY: strings are borrowed for duration of call.
    out.state_path = unsafe { cstr_opt(c.state_path) }
        .map_err(|_| invalid("engine_config.state_path is not valid UTF-8"))?
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    // SAFETY: strings are borrowed for duration of call.
    if let Some(ua) = unsafe { cstr_opt(c.user_agent) }
        .map_err(|_| invalid("engine_config.user_agent is not valid UTF-8"))?
        .filter(|s| !s.is_empty())
    {
        if ua.chars().any(|ch| ch.is_control()) {
            return Err(invalid(
                "engine_config.user_agent contains control characters",
            ));
        }
        out.user_agent = ua.to_string();
    }
    Ok(out)
}

/// Validates and converts a job configuration from C ABI.
///
/// Returns parsed configuration, output path, and credentials.
///
/// # Safety
/// `p` must point to a readable `hydra_job_config_t` with valid string arrays.
pub(crate) unsafe fn job_cfg(
    p: *const hydra_job_config_t,
    engine: &EngineCfg,
) -> Result<(JobCfg, String, Creds), Detail> {
    // SAFETY: caller provides valid pointer to job config struct.
    let c = unsafe { read_versioned(p, crate::HYDRA_JOB_CONFIG_VERSION, "job_config")? };

    // ---- urls ------------------------------------------------------------
    if c.urls.is_null() || c.url_count == 0 {
        return Err(invalid("job_config.urls is empty"));
    }
    if c.url_count > 256 {
        return Err(invalid(format!(
            "job_config.url_count is {}; at most 256 mirrors are accepted",
            c.url_count
        )));
    }
    let mut urls = Vec::with_capacity(c.url_count);
    let mut url_creds: Option<(String, Option<String>)> = None;
    for i in 0..c.url_count {
        // SAFETY: `i` is within the bounds of `url_count`.
        let p = unsafe { *c.urls.add(i) };
        // SAFETY: string pointer is validated as NUL-terminated UTF-8.
        let raw = unsafe { cstr_req(p) }
            .map_err(|_| invalid(format!("job_config.urls[{i}] is NULL or not valid UTF-8")))?;
        let parsed = crate::url::Url::parse(raw).map_err(|e| Detail {
            code: E::HYDRA_ERR_INVALID_URL as u32,
            message: e,
            ..Default::default()
        })?;
        if url_creds.is_none() {
            if let Some(u) = &parsed.user {
                url_creds = Some((u.clone(), parsed.pass.clone()));
            }
        }
        urls.push(parsed.redacted());
    }

    // ---- destination -----------------------------------------------------
    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
    let output = unsafe { cstr_req(c.output_path) }
        .map_err(|_| invalid("job_config.output_path is NULL or not valid UTF-8"))?;
    if output.is_empty() {
        return Err(invalid("job_config.output_path is empty"));
    }
    // The engine writes exactly where it is told and does not invent a
    // directory, but a destination that is a directory, or that ends in a
    // separator, cannot be a file and is a caller mistake worth naming now
    // rather than as an I/O error three seconds into a transfer.
    if output.ends_with(std::path::MAIN_SEPARATOR) || output.ends_with('/') {
        return Err(invalid(format!(
            "job_config.output_path {output:?} names a directory, not a file"
        )));
    }

    // ---- headers ---------------------------------------------------------
    if c.header_count > 512 {
        return Err(invalid(format!(
            "job_config.header_count is {}; at most 512 are accepted",
            c.header_count
        )));
    }
    let mut headers = Vec::with_capacity(c.header_count);
    if c.header_count > 0 {
        if c.headers.is_null() {
            return Err(invalid(
                "job_config.headers is NULL but header_count is not 0",
            ));
        }
        for i in 0..c.header_count {
            // SAFETY: `i` is below the count the caller declared, and the array is valid for this call.
            let h = unsafe { *c.headers.add(i) };
            // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
            let name = unsafe { cstr_req(h.name) }
                .map_err(|_| invalid(format!("job_config.headers[{i}].name is invalid")))?;
            // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
            let value = unsafe { cstr_req(h.value) }
                .map_err(|_| invalid(format!("job_config.headers[{i}].value is invalid")))?;
            // Header splitting: a CR or LF in either half turns one header into
            // several, which is a request-smuggling primitive when the value
            // came from outside the application.
            if name.is_empty()
                || name
                    .chars()
                    .any(|ch| ch.is_control() || ch == ':' || ch == ' ')
            {
                return Err(invalid(format!(
                    "job_config.headers[{i}].name {name:?} is not a valid field name"
                )));
            }
            if value.chars().any(|ch| ch == '\r' || ch == '\n') {
                return Err(invalid(format!(
                    "job_config.headers[{i}].value contains a line break"
                )));
            }
            headers.push((name.to_string(), value.to_string()));
        }
    }

    // ---- credentials -----------------------------------------------------
    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
    let username = unsafe { cstr_opt(c.username) }
        .map_err(|_| invalid("job_config.username is not valid UTF-8"))?
        .map(str::to_string);
    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
    let password = unsafe { cstr_opt(c.password) }
        .map_err(|_| invalid("job_config.password is not valid UTF-8"))?
        .map(str::to_string);
    if let Some(u) = &username {
        if u.chars().any(|ch| ch.is_control()) {
            return Err(invalid("job_config.username contains control characters"));
        }
    }
    if let Some(pw) = &password {
        if pw.chars().any(|ch| ch.is_control()) {
            return Err(invalid("job_config.password contains control characters"));
        }
    }
    // Explicit fields beat userinfo; userinfo fills in when they are absent.
    let (username, password) = match (username, password, url_creds) {
        (None, None, Some((u, p))) => (Some(u), p),
        (u, p, _) => (u, p),
    };

    // ---- proxy -----------------------------------------------------------
    let proxy = if c.proxy.is_null() {
        None
    } else {
        // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
        let pc = unsafe { *c.proxy };
        let kind = match enum_in_range(pc.kind, 4, "proxy_config.type")? {
            0 => None,
            1 => Some(pdl_net::ProxyKind::Http),
            2 => Some(pdl_net::ProxyKind::Socks4),
            3 => Some(pdl_net::ProxyKind::Socks4a),
            _ => Some(pdl_net::ProxyKind::Socks5),
        };
        match kind {
            None => None,
            Some(kind) => {
                // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
                let host = unsafe { cstr_req(pc.host) }
                    .map_err(|_| invalid("proxy_config.host is NULL or not valid UTF-8"))?;
                if host.is_empty() || pc.port == 0 {
                    return Err(invalid("proxy_config needs a host and a non-zero port"));
                }
                Some(ProxyCfg {
                    kind,
                    host: host.to_string(),
                    port: pc.port,
                    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
                    username: unsafe { cstr_opt(pc.username) }
                        .map_err(|_| invalid("proxy_config.username is not valid UTF-8"))?
                        .map(str::to_string),
                    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
                    password: unsafe { cstr_opt(pc.password) }
                        .map_err(|_| invalid("proxy_config.password is not valid UTF-8"))?
                        .map(str::to_string),
                })
            }
        }
    };

    // ---- checksum --------------------------------------------------------
    let checksum = match enum_in_range(c.checksum.algorithm, 5, "checksum.algorithm")? {
        0 => None,
        n => {
            let algo = match n {
                1 => Algo::Md5,
                2 => Algo::Sha1,
                3 => Algo::Sha256,
                4 => Algo::Sha512,
                _ => Algo::Blake3,
            };
            if c.checksum.digest.is_null() {
                return Err(invalid("checksum.digest is NULL"));
            }
            if c.checksum.digest_len != algo.len() {
                return Err(invalid(format!(
                    "checksum.digest_len is {} but {} digests are {} bytes",
                    c.checksum.digest_len,
                    algo.as_str(),
                    algo.len()
                )));
            }
            let bytes =
                // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
                unsafe { std::slice::from_raw_parts(c.checksum.digest, c.checksum.digest_len) };
            Some((algo, bytes.to_vec()))
        }
    };

    // ---- everything else -------------------------------------------------
    let priority = enum_in_range(c.priority, 2, "job_config.priority")?;
    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
    let reaches = |off: usize| (unsafe { (p as *const u32).read_unaligned() } as usize) >= off;
    let flags_off = std::mem::offset_of!(hydra_job_config_t, reserved1) + 1;
    let (resume, adaptive) = if reaches(flags_off) {
        (c.resume != 0, c.adaptive != 0)
    } else {
        (true, engine.adaptive_concurrency)
    };

    Ok((
        JobCfg {
            urls,
            headers,
            withheld_headers: Vec::new(),
            proxy,
            checksum,
            max_connections: c.max_connections.min(64) as usize,
            max_retries: if c.max_retries == 0 {
                engine.max_retries
            } else {
                c.max_retries.min(64)
            },
            priority,
            max_bytes_per_second: c.max_bytes_per_second,
            resume,
            adaptive,
            // A job built from a plain URL list has no publisher ranking and no
            // attestation; `hydra_job_create_from_metalink` is what supplies
            // them. See `crate::metalink`.
            source_plans: Vec::new(),
            attested_size: None,
            pieces: None,
            attested_by: None,
        },
        output.to_string(),
        Creds { username, password },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A caller built against a smaller (older) struct must still be readable,
    /// and must not have the fields it never had invented from its stack.
    #[test]
    fn a_short_config_is_read_as_a_prefix_and_defaults_the_rest() {
        let mut c = hydra_engine_config_t {
            size: 0,
            version: 1,
            max_jobs: 7,
            max_connections: 3,
            max_retries: 0,
            progress_interval_ms: 0,
            event_queue_capacity: 0,
            worker_threads: 0,
            max_bytes_per_second: 0,
            adaptive_concurrency: 0,
            range_stealing: 0,
            allow_insecure_tls: 1,
            reserved0: 0,
            network_policy: 0,
            power_mode: 0,
            state_path: std::ptr::null(),
            user_agent: std::ptr::null(),
            reserved: [0; 32],
        };
        // Pretend to be an old caller whose struct ended right after
        // `max_connections`.
        c.size = (std::mem::offset_of!(hydra_engine_config_t, max_connections) + 4) as u32;
        // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
        let got = unsafe { engine_cfg(&c) }.expect("short config is valid");
        assert_eq!(got.max_jobs, 7);
        assert_eq!(got.max_connections, 3);
        // Not reached by the old struct, so defaulted rather than read as 0.
        assert!(got.adaptive_concurrency);
        assert!(
            !got.allow_insecure_tls,
            "a field past `size` must not be read"
        );
    }

    #[test]
    fn an_uninitialised_or_future_config_is_refused() {
        // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
        let mut c: hydra_engine_config_t = unsafe { std::mem::zeroed() };
        // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
        assert!(unsafe { engine_cfg(&c) }.is_err(), "size 0 is not usable");
        c.size = std::mem::size_of::<hydra_engine_config_t>() as u32;
        c.version = 99;
        assert!(
            // SAFETY: `c` is a fully-initialised local; only its `size` and
            // `version` fields lie, which is exactly what is under test.
            unsafe { engine_cfg(&c) }.is_err(),
            "a future version is refused"
        );
        // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
        assert!(unsafe { engine_cfg(std::ptr::null()) }.is_err());
    }
}
