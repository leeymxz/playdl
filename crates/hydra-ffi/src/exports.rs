// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Exported C FFI function declarations and entry points.

use crate::abi::*;
use crate::convert;
use crate::driver;
use crate::engine::{Creds, Engine, Stop};
use crate::err::{self, Detail};
use crate::guard::{shield, shield_unit, shield_value};
use crate::mem;
use crate::persist;
use hydra_error_code_t as E;
use hydra_job_state_t as S;
use std::os::raw::c_char;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Opaque engine instance handle type.
pub enum hydra_engine_t {}

/// Magic number for verifying engine handle validity.
const MAGIC: u64 = 0x4879_6472_4146_4649;

struct EngineBox {
    magic: u64,
    /// Owned here rather than inside `Engine` because dropping a `tokio`
    /// runtime from one of its own worker threads panics, and every transfer
    /// task holds an `Arc<Engine>`.
    rt: Option<tokio::runtime::Runtime>,
    engine: Arc<Engine>,
}

/// Borrow the engine behind a handle.
///
/// # Safety
///
/// `p` must be a handle from [`hydra_engine_create`] that has not been passed
/// to [`hydra_engine_destroy`].
unsafe fn boxed<'a>(p: *mut hydra_engine_t) -> Result<&'a mut EngineBox, hydra_error_code_t> {
    if p.is_null() {
        return Err(err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "engine is NULL"));
    }
    // SAFETY: the caller's contract is that `p` came from `hydra_engine_create`,
    // which hands out exactly `Box::into_raw` of an `EngineBox`. The magic check
    // below rejects the common violations of that contract.
    let b = unsafe { &mut *(p as *mut EngineBox) };
    if b.magic != MAGIC {
        return Err(err::set(
            E::HYDRA_ERR_INVALID_ARGUMENT,
            "engine handle is not valid (already destroyed, or not from hydra)",
        ));
    }
    Ok(b)
}

/// Borrow the engine, refusing once shutdown has begun.
///
/// # Safety
///
/// As [`boxed`].
unsafe fn live<'a>(p: *mut hydra_engine_t) -> Result<&'a Arc<Engine>, hydra_error_code_t> {
    // SAFETY: the pointer satisfies this function's documented contract and outlives the call.
    let b = unsafe { boxed(p)? };
    if b.engine.shutdown.load(Ordering::Relaxed) {
        return Err(err::set(E::HYDRA_ERR_SHUTDOWN, "engine has been shut down"));
    }
    Ok(&b.engine)
}

/// Record a `Detail` in this thread's slot and return its code.
fn fail(d: Detail) -> hydra_error_code_t {
    let code = d.code;
    err::set_detail(d);
    err::to_code(code)
}

fn job_of(
    engine: &Arc<Engine>,
    id: hydra_job_id_t,
) -> Result<Arc<crate::engine::Job>, hydra_error_code_t> {
    engine
        .job(id)
        .ok_or_else(|| err::set(E::HYDRA_ERR_NOT_FOUND, format!("no job with id {id}")))
}

// =========================================================== version and errors

/// The ABI version this library implements.
///
/// Compare against `HYDRA_FFI_ABI_VERSION` from the header you compiled with;
/// a mismatch means the header and the library disagree about every struct
/// below and the program should refuse to continue.
///
/// Thread-safe. Non-blocking. Does not allocate.
#[no_mangle]
pub extern "C" fn hydra_ffi_abi_version() -> u32 {
    crate::HYDRA_FFI_ABI_VERSION
}

/// The library's own version, as a static NUL-terminated string.
///
/// Never freed by the caller: it points into the library's read-only data.
///
/// Thread-safe. Non-blocking. Does not allocate.
#[no_mangle]
pub extern "C" fn hydra_ffi_version_string() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// The stable spelling of an error code, as a static NUL-terminated string.
///
/// Never freed by the caller. Unknown codes return `"HYDRA_ERR_UNKNOWN"` rather
/// than NULL, so a caller can print the result unconditionally.
///
/// Thread-safe. Non-blocking. Does not allocate.
#[no_mangle]
pub extern "C" fn hydra_error_name(code: u32) -> *const c_char {
    // Every arm is a literal with an explicit NUL, so the pointer is static.
    macro_rules! s {
        ($e:expr) => {
            concat!($e, "\0").as_ptr() as *const c_char
        };
    }
    match err::name(code) {
        "HYDRA_OK" => s!("HYDRA_OK"),
        "HYDRA_ERR_INVALID_ARGUMENT" => s!("HYDRA_ERR_INVALID_ARGUMENT"),
        "HYDRA_ERR_INVALID_URL" => s!("HYDRA_ERR_INVALID_URL"),
        "HYDRA_ERR_INVALID_STATE" => s!("HYDRA_ERR_INVALID_STATE"),
        "HYDRA_ERR_UNSUPPORTED" => s!("HYDRA_ERR_UNSUPPORTED"),
        "HYDRA_ERR_AGAIN" => s!("HYDRA_ERR_AGAIN"),
        "HYDRA_ERR_NETWORK" => s!("HYDRA_ERR_NETWORK"),
        "HYDRA_ERR_CONNECTION" => s!("HYDRA_ERR_CONNECTION"),
        "HYDRA_ERR_TIMEOUT" => s!("HYDRA_ERR_TIMEOUT"),
        "HYDRA_ERR_PROTOCOL" => s!("HYDRA_ERR_PROTOCOL"),
        "HYDRA_ERR_IO" => s!("HYDRA_ERR_IO"),
        "HYDRA_ERR_PERMISSION" => s!("HYDRA_ERR_PERMISSION"),
        "HYDRA_ERR_NO_SPACE" => s!("HYDRA_ERR_NO_SPACE"),
        "HYDRA_ERR_CHECKSUM" => s!("HYDRA_ERR_CHECKSUM"),
        "HYDRA_ERR_VERIFICATION" => s!("HYDRA_ERR_VERIFICATION"),
        "HYDRA_ERR_CANCELLED" => s!("HYDRA_ERR_CANCELLED"),
        "HYDRA_ERR_NOT_FOUND" => s!("HYDRA_ERR_NOT_FOUND"),
        "HYDRA_ERR_ALREADY_EXISTS" => s!("HYDRA_ERR_ALREADY_EXISTS"),
        "HYDRA_ERR_RESOURCE_LIMIT" => s!("HYDRA_ERR_RESOURCE_LIMIT"),
        "HYDRA_ERR_SHUTDOWN" => s!("HYDRA_ERR_SHUTDOWN"),
        "HYDRA_ERR_INTERNAL" => s!("HYDRA_ERR_INTERNAL"),
        _ => s!("HYDRA_ERR_UNKNOWN"),
    }
}

/// The detail behind this thread's most recent failure.
///
/// The slot is **thread-local**: it describes the last hydra call made on the
/// calling thread and is cleared at the start of every call, so reading it after
/// a success reports `HYDRA_OK` rather than a stale failure.
///
/// Returns `HYDRA_ERR_NOT_FOUND` when nothing has failed on this thread.
///
/// Thread-safe. Non-blocking. **Allocates**: `out->message` is owned by the
/// caller and must be released with [`hydra_error_free`].
///
/// # Safety
///
/// `out` must point to a writable [`hydra_error_t`].
#[no_mangle]
pub unsafe extern "C" fn hydra_last_error(out: *mut hydra_error_t) -> hydra_error_code_t {
    // Deliberately does NOT go through `shield`, which clears the slot.
    if out.is_null() {
        return E::HYDRA_ERR_INVALID_ARGUMENT;
    }
    match err::take() {
        Some(d) => {
            // SAFETY: `out` is a writable, caller-owned struct per the contract.
            unsafe { std::ptr::write(out, d.into_abi()) };
            // The return value reports whether a detail was AVAILABLE, not what
            // it says: the failure itself is in `out->code`, and conflating the
            // two would make "the last call failed" indistinguishable from
            // "this call failed".
            E::HYDRA_OK
        }
        None => {
            // SAFETY: as above.
            unsafe { std::ptr::write(out, err::ok_error()) };
            E::HYDRA_ERR_NOT_FOUND
        }
    }
}

/// Release the owned parts of an error.
///
/// Safe to call on a zeroed struct and safe to call twice in the sense that the
/// second call sees a NULL message — but the second call is still a bug, and the
/// pointer is cleared here to make it a harmless one.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `e` must be NULL or point to an [`hydra_error_t`] this library produced.
#[no_mangle]
pub unsafe extern "C" fn hydra_error_free(e: *mut hydra_error_t) {
    shield_unit(|| {
        if e.is_null() {
            return;
        }
        // SAFETY: the caller's contract; the write below prevents a repeat free
        // from doing anything.
        unsafe {
            let s = std::ptr::replace(&mut (*e).message, hydra_string_t::null());
            mem::string_drop(s);
        }
    })
}

/// Release a string this library produced.
///
/// Never call `free()` on `value.data`: a static libhydra may not share an
/// allocator with the host program.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `value` must be a string this library produced and not yet freed, or the
/// NULL value.
#[no_mangle]
pub unsafe extern "C" fn hydra_string_free(value: hydra_string_t) {
    shield_unit(|| mem::string_drop(value))
}

// ================================================================ config init

/// Fill an engine configuration with defaults.
///
/// `struct_size` is `sizeof(hydra_engine_config_t)` **as the caller's header
/// declares it**, and passing it is what makes this safe in both directions: a
/// program built against an older header has a smaller struct, and a library
/// that wrote its own `sizeof` would run off the end of it. Use the
/// `HYDRA_ENGINE_CONFIG_INIT` convenience macro from the header, which supplies
/// it from `sizeof`.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `config` must point to at least `struct_size` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_config_init(
    config: *mut hydra_engine_config_t,
    struct_size: u32,
) -> hydra_error_code_t {
    shield(|| {
        if config.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "config is NULL");
        }
        let n = (struct_size as usize).min(std::mem::size_of::<hydra_engine_config_t>());
        if n < 8 {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                "struct_size is too small to be a hydra_engine_config_t",
            );
        }
        let d = crate::engine::EngineCfg::default();
        let full = hydra_engine_config_t {
            size: struct_size,
            version: crate::HYDRA_ENGINE_CONFIG_VERSION,
            max_jobs: d.max_jobs as u32,
            max_connections: d.max_connections as u32,
            max_retries: d.max_retries,
            progress_interval_ms: d.progress_interval_ms as u32,
            event_queue_capacity: d.event_queue_capacity as u32,
            worker_threads: 0,
            max_bytes_per_second: 0,
            adaptive_concurrency: u8::from(d.adaptive_concurrency),
            range_stealing: u8::from(d.range_stealing),
            allow_insecure_tls: 0,
            reserved0: 0,
            network_policy: hydra_network_policy_t::HYDRA_NETWORK_ANY as u32,
            power_mode: hydra_power_mode_t::HYDRA_POWER_NORMAL as u32,
            state_path: std::ptr::null(),
            user_agent: std::ptr::null(),
            reserved: [0; 32],
        };
        // SAFETY: exactly `n` bytes are written, and `n` is clamped to the
        // caller's own declared size, so an older caller's smaller struct is
        // never overrun.
        unsafe {
            std::ptr::copy_nonoverlapping(&full as *const _ as *const u8, config as *mut u8, n);
        }
        E::HYDRA_OK
    })
}

/// Fill a job configuration with defaults.
///
/// See [`hydra_engine_config_init`] for why `struct_size` is a parameter. Use
/// the `HYDRA_JOB_CONFIG_INIT` macro from the header.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `config` must point to at least `struct_size` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_config_init(
    config: *mut hydra_job_config_t,
    struct_size: u32,
) -> hydra_error_code_t {
    shield(|| {
        if config.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "config is NULL");
        }
        let n = (struct_size as usize).min(std::mem::size_of::<hydra_job_config_t>());
        if n < 8 {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                "struct_size is too small to be a hydra_job_config_t",
            );
        }
        let full = hydra_job_config_t {
            size: struct_size,
            version: crate::HYDRA_JOB_CONFIG_VERSION,
            urls: std::ptr::null(),
            url_count: 0,
            output_path: std::ptr::null(),
            headers: std::ptr::null(),
            header_count: 0,
            username: std::ptr::null(),
            password: std::ptr::null(),
            proxy: std::ptr::null(),
            checksum: hydra_checksum_t::none(),
            max_connections: 0,
            max_retries: 0,
            priority: hydra_priority_t::HYDRA_PRIORITY_NORMAL as u32,
            reserved0: 0,
            max_bytes_per_second: 0,
            resume: 1,
            adaptive: 1,
            auto_start: 0,
            reserved1: 0,
            reserved: [0; 32],
        };
        // SAFETY: as in `hydra_engine_config_init`.
        unsafe {
            std::ptr::copy_nonoverlapping(&full as *const _ as *const u8, config as *mut u8, n);
        }
        E::HYDRA_OK
    })
}

/// Fill a runtime policy with the permissive defaults: any network, full power.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `policy` must point to a writable [`hydra_runtime_policy_t`].
#[no_mangle]
pub unsafe extern "C" fn hydra_runtime_policy_init(
    policy: *mut hydra_runtime_policy_t,
) -> hydra_error_code_t {
    shield(|| {
        if policy.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "policy is NULL");
        }
        // SAFETY: caller-owned writable struct per the contract.
        unsafe {
            std::ptr::write(
                policy,
                hydra_runtime_policy_t {
                    network_policy: hydra_network_policy_t::HYDRA_NETWORK_ANY as u32,
                    power_mode: hydra_power_mode_t::HYDRA_POWER_NORMAL as u32,
                    allow_cellular: 1,
                    allow_metered: 1,
                    pause_on_low_battery: 0,
                    pause_when_backgrounded: 0,
                    reserved: [0; 4],
                },
            )
        };
        E::HYDRA_OK
    })
}

// ==================================================================== engine

/// Create an engine.
///
/// Returns NULL on failure; call [`hydra_last_error`] on the same thread for the
/// reason. The engine starts its own threads and is ready to accept jobs when
/// this returns.
///
/// Thread-safe. **Blocking** only in the sense that it builds a thread pool and
/// a TLS root store, which is milliseconds. Allocates.
///
/// # Safety
///
/// `config` must have been initialised by [`hydra_engine_config_init`] and all
/// its string pointers must be valid for this call.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_create(
    config: *const hydra_engine_config_t,
) -> *mut hydra_engine_t {
    shield_value(std::ptr::null_mut(), || {
        // SAFETY: the caller's contract, checked field by field inside.
        let cfg = match unsafe { convert::engine_cfg(config) } {
            Ok(c) => c,
            Err(d) => {
                fail(d);
                return std::ptr::null_mut();
            }
        };
        let mut b = tokio::runtime::Builder::new_multi_thread();
        b.enable_all().thread_name("hydra-engine");
        if cfg.worker_threads > 0 {
            b.worker_threads(cfg.worker_threads);
        }
        let rt = match b.build() {
            Ok(rt) => rt,
            Err(e) => {
                fail(err::from_io(&e));
                return std::ptr::null_mut();
            }
        };
        let engine = Engine::new(cfg, rt.handle().clone());
        let boxed = Box::new(EngineBox {
            magic: MAGIC,
            rt: Some(rt),
            engine,
        });
        Box::into_raw(boxed) as *mut hydra_engine_t
    })
}

/// Stop every running job and stop accepting new work.
///
/// Running jobs are paused, not cancelled: their partial data and range maps
/// survive, and if the engine has a `state_path` they are written to it before
/// this returns. A final `HYDRA_EVENT_ENGINE_SHUTDOWN` is published, after which
/// the queue is closed and every blocked [`hydra_event_wait`] returns.
///
/// Post-conditions, which are deterministic and worth stating exactly:
///
/// * no job is `HYDRA_JOB_CANCELLED` merely because a shutdown happened;
/// * every job that was running is `HYDRA_JOB_PAUSED`;
/// * incomplete work is retained in durable state when `state_path` is set;
/// * the event queue is closed and every blocked [`hydra_event_wait`] returns.
///
/// Returns `HYDRA_OK` when every transfer stopped within `timeout_ms`, and in
/// that case no network operation is still in flight. Returns
/// `HYDRA_ERR_TIMEOUT` when one did not: the engine is still shut down and the
/// post-conditions above still hold, but a socket may still be draining until
/// [`hydra_engine_destroy`] tears the runtime down. The distinction exists so a
/// host that needs "nothing is running" as a fact can tell whether it has one.
///
/// In **both** cases, once this returns, **no new network work can start**.
/// Every job has been told to stop, no job can be started, and a transfer that
/// is still unwinding will not issue another request. That is the guarantee
/// that matters on a platform which is about to suspend the process: a timeout
/// means "something has not finished letting go", never "something may still
/// begin".
///
/// `timeout_ms` bounds only that wait, not the whole call.
///
/// Calling this before [`hydra_engine_destroy`] is optional but is the way to
/// get a deterministic stop. Idempotent: a second call returns `HYDRA_OK`.
///
/// Thread-safe. **Blocking** for up to roughly `timeout_ms`.
///
/// # Safety
///
/// `engine` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_shutdown(
    engine: *mut hydra_engine_t,
    timeout_ms: u32,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        if shutdown_engine(&eng, timeout_ms) {
            hydra_error_code_t::HYDRA_OK
        } else {
            err::set(
                E::HYDRA_ERR_TIMEOUT,
                format!("a transfer was still running after {timeout_ms} ms"),
            )
        }
    })
}

/// The graceful stop, independent of the C handle.
///
/// Separate from the exported function because `hydra_engine_destroy` needs the
/// same work done at a point where the handle is deliberately no longer usable:
/// it has taken ownership of the box back and poisoned the magic word, so
/// calling the exported function from there would be rejected by its own
/// validity check — silently skipping the state write — and would alias the
/// `Box` that frame now holds.
///
/// Returns whether every transfer had stopped by the deadline. A second call is
/// a no-op and reports success.
fn shutdown_engine(eng: &Arc<Engine>, timeout_ms: u32) -> bool {
    if eng.shutdown.swap(true, Ordering::SeqCst) {
        return true;
    }
    for job in eng.all_jobs() {
        let mut g = job.lock();
        if g.is_running() {
            // Paused, not cancelled: shutting down must not throw away partial
            // data the user can resume from.
            g.stop = Stop::Pause;
            if let Some(c) = &g.cancel {
                c.store(true, Ordering::Relaxed);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
    let mut settled = false;
    loop {
        if eng.all_jobs().iter().all(|j| !j.lock().is_running()) {
            settled = true;
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // Saved after the wait, so the spans a stopping transfer recorded on its way
    // out are in the file rather than a snapshot taken before it stopped.
    persist::autosave(eng);
    eng.events.push(hydra_event_t {
        kind: hydra_event_type_t::HYDRA_EVENT_ENGINE_SHUTDOWN,
        timestamp_ms: crate::engine::now_ms(),
        ..Default::default()
    });
    eng.events.close();
    settled
}

/// Destroy an engine and release everything it owns.
///
/// [`hydra_engine_shutdown`] is the lifecycle transition; this is resource
/// release. Prefer calling them in that order, and treat this as the
/// destructor it is — a C++ `~Engine`, a Swift `deinit`, a Go finalizer or a
/// JNI `close()` should not be where a network lifecycle happens.
///
/// If shutdown was not called, this performs a **best-effort emergency
/// shutdown** first — with a fixed internal grace period rather than one you
/// choose — so that the simple path (create, use, destroy) is still correct and
/// still writes state. That is a safety net, not the intended sequence.
///
/// **Synchronisation-sensitive.** This must not race with any other call on the
/// same engine, and the handle must not be used afterwards. Passing NULL is a
/// no-op.
///
/// Thread-safe with respect to *other* engines. Blocking for up to a few hundred
/// milliseconds while runtime threads are joined.
///
/// # Safety
///
/// `engine` must be NULL or a handle from [`hydra_engine_create`] that has not
/// already been destroyed, and no other thread may be inside a hydra call on it.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_destroy(engine: *mut hydra_engine_t) {
    shield_unit(|| {
        if engine.is_null() {
            return;
        }
        // SAFETY: caller's contract; a handle that is not ours is rejected by
        // the magic check rather than dereferenced further.
        if unsafe { boxed(engine) }.is_err() {
            return;
        }
        // SAFETY: `boxed` confirmed this is our allocation.
        let mut b = unsafe { Box::from_raw(engine as *mut EngineBox) };
        // Poison first: if anything below panics, the handle can no longer be
        // mistaken for a live one.
        b.magic = 0;
        // Through the internal function, never the exported one: the magic word
        // above has already been cleared, so an exported call would reject its
        // own handle and silently skip the state write, and passing `engine` on
        // would alias the `Box` this frame now owns. Destroying without an
        // explicit shutdown therefore still stops jobs and still persists.
        let eng = b.engine.clone();
        // The return value is deliberately ignored: `destroy` has no way to
        // report a timeout and no better option than tearing down anyway.
        let _ = shutdown_engine(&eng, 2000);
        drop(eng);
        b.engine.events.close();
        if let Some(rt) = b.rt.take() {
            // Bounded rather than unbounded: a socket read that will never
            // return must not make `destroy` hang the host program. Tasks still
            // running after the grace period are dropped with their sockets.
            rt.shutdown_timeout(Duration::from_millis(500));
        }
        drop(b);
    })
}

/// Replace the platform policy.
///
/// Takes effect for jobs started after the call; a running transfer keeps the
/// connection count it was admitted with, because tearing down live connections
/// to satisfy a new ceiling costs more than it saves.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `policy` must point to a readable
/// [`hydra_runtime_policy_t`].
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_set_policy(
    engine: *mut hydra_engine_t,
    policy: *const hydra_runtime_policy_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        if policy.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "policy is NULL");
        }
        // SAFETY: caller's contract.
        let p = unsafe { *policy };
        if p.network_policy > 2 || p.power_mode > 2 {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                "policy carries an unknown network_policy or power_mode",
            );
        }
        *eng.policy.lock().unwrap_or_else(|x| x.into_inner()) = p;
        E::HYDRA_OK
    })
}

/// Read the active platform policy.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_get_policy(
    engine: *mut hydra_engine_t,
    out: *mut hydra_runtime_policy_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, eng.policy()) };
        E::HYDRA_OK
    })
}

/// Change the engine-wide rate ceiling, in bytes per second. 0 = unlimited.
///
/// Applies immediately, including to transfers already running — to every job,
/// whether or not it has a ceiling of its own, since the engine-wide limiter is
/// an aggregate over all of them. See the note on `max_bytes_per_second` in the
/// header.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_set_max_bytes_per_second(
    engine: *mut hydra_engine_t,
    bytes_per_second: u64,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        match unsafe { live(engine) } {
            Ok(eng) => {
                eng.limiter.set_rate(bytes_per_second);
                E::HYDRA_OK
            }
            Err(e) => e,
        }
    })
}

/// Change how many jobs may execute at once.
///
/// Raising it admits queued jobs immediately. Lowering it never preempts a
/// running job; the new ceiling takes hold as jobs finish.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_set_max_jobs(
    engine: *mut hydra_engine_t,
    max_jobs: u32,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        if max_jobs == 0 {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "max_jobs must be at least 1");
        }
        eng.gate.set_limit(max_jobs as usize);
        E::HYDRA_OK
    })
}

/// Read the engine's counters.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_get_metrics(
    engine: *mut hydra_engine_t,
    out: *mut hydra_metrics_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, eng.metrics()) };
        E::HYDRA_OK
    })
}

/// List every job the engine knows about, in creation order.
///
/// Thread-safe. Non-blocking. **Allocates**: release with
/// [`hydra_job_id_array_free`].
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_list_jobs(
    engine: *mut hydra_engine_t,
    out: *mut hydra_job_id_array_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let ids: Vec<hydra_job_id_t> = eng.all_jobs().iter().map(|j| j.id).collect();
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, mem::ids_out(ids)) };
        E::HYDRA_OK
    })
}

/// Release a job-id array.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `a` must be NULL or an array this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_id_array_free(a: *mut hydra_job_id_array_t) {
    shield_unit(|| {
        if a.is_null() {
            return;
        }
        // SAFETY: caller's contract; the struct is zeroed so a second free is
        // harmless.
        unsafe {
            let taken = std::ptr::replace(
                a,
                hydra_job_id_array_t {
                    items: std::ptr::null_mut(),
                    len: 0,
                },
            );
            mem::ids_drop(taken);
        }
    })
}

/// Write every job's durable state to the engine's `state_path`.
///
/// The write is atomic — a temporary file and a rename — so a process death
/// during it cannot leave a truncated state file. Returns
/// `HYDRA_ERR_INVALID_STATE` when the engine was created without a `state_path`.
///
/// This also happens automatically whenever a job reaches a terminal state; call
/// it explicitly when the platform tells you the process is about to be
/// suspended.
///
/// Thread-safe. **Blocking**: performs file I/O. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_snapshot(engine: *mut hydra_engine_t) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        match persist::save(&eng) {
            Ok(()) => E::HYDRA_OK,
            Err(d) => fail(d),
        }
    })
}

/// Load persisted jobs from the engine's `state_path`.
///
/// Restores identities and range maps, not execution: every job that was
/// running when the state was written comes back as `HYDRA_JOB_PAUSED`, and
/// nothing starts until the application calls [`hydra_job_resume`]. On a phone
/// that is the correct division — whether work may run now is the platform
/// layer's decision, not the engine's.
///
/// Credentials are never persisted. A job whose configuration included a
/// password or a credential-bearing header comes back without them; use
/// [`hydra_job_set_credentials`] before resuming it.
///
/// `out_restored` receives how many jobs were added; it may be NULL. Ids already
/// present are skipped rather than overwritten.
///
/// Thread-safe. **Blocking**: performs file I/O. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid; `out_restored` must be NULL or writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_restore(
    engine: *mut hydra_engine_t,
    out_restored: *mut usize,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e.clone(),
            Err(e) => return e,
        };
        match persist::restore(&eng) {
            Ok(n) => {
                if !out_restored.is_null() {
                    // SAFETY: caller's contract.
                    unsafe { std::ptr::write(out_restored, n) };
                }
                E::HYDRA_OK
            }
            Err(d) => fail(d),
        }
    })
}

// ====================================================================== jobs

/// Create a job and return its durable id.
///
/// Nothing happens on the network until the job is started, unless
/// `config->auto_start` is set. The id is stable for the life of the engine and,
/// with persistence enabled, across process restarts.
///
/// Every string in `config` is borrowed for this call only.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid, `config` must have been initialised by
/// [`hydra_job_config_init`], and every pointer in it must be valid for this
/// call.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_create(
    engine: *mut hydra_engine_t,
    config: *const hydra_job_config_t,
    out_job_id: *mut hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        if out_job_id.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_job_id is NULL");
        }
        // SAFETY: caller's contract, validated field by field inside.
        let (cfg, output, creds) = match unsafe { convert::job_cfg(config, &eng.cfg) } {
            Ok(v) => v,
            Err(d) => return fail(d),
        };
        // SAFETY: caller's contract.
        let auto_start = unsafe { (*config).auto_start } != 0;
        let job = eng.insert_job(cfg, output, creds);
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out_job_id, job.id) };
        eng.emit(&job, hydra_event_type_t::HYDRA_EVENT_JOB_CREATED);
        crate::log::log_at!(
            eng,
            hydra_log_level_t::HYDRA_LOG_INFO,
            "job {} created for {}",
            job.id,
            job.cfg.urls[0]
        );
        if auto_start {
            if let Err(d) = driver::spawn(eng, &job) {
                return fail(d);
            }
        }
        E::HYDRA_OK
    })
}

/// Start a job.
///
/// Legal from `HYDRA_JOB_CREATED`, and also from `HYDRA_JOB_PAUSED`,
/// `HYDRA_JOB_FAILED` and `HYDRA_JOB_CANCELLED` — restarting a failed job is a
/// thing applications legitimately do, and refusing it would only make them
/// recreate the job and lose its range map. A job already running returns
/// `HYDRA_ERR_INVALID_STATE`; a completed one does too.
///
/// What "start" means for a job that has already been somewhere is decided by
/// the range map, not by the previous state, and the rule is one sentence:
/// **whatever spans hydra still records as present are reused; everything else
/// is fetched.** So
///
/// * a paused or failed job continues from where it stopped;
/// * a job cancelled with `HYDRA_CANCEL_KEEP_PARTIAL` continues too, because
///   the file and its range map both survived;
/// * a job cancelled with `HYDRA_CANCEL_REMOVE_PARTIAL` starts from zero,
///   because cancelling that way cleared both.
///
/// A job created with `resume = 0` always starts from zero.
///
/// Returns as soon as the job is queued. The transfer runs on engine threads and
/// reports through the event queue.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_start(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        match driver::spawn(eng, &job) {
            Ok(()) => E::HYDRA_OK,
            Err(d) => fail(d),
        }
    })
}

/// Stop a running job, preserving everything needed to resume it.
///
/// The sockets are closed; the partial file, the range map and the source
/// information all survive. The job reaches `HYDRA_JOB_PAUSED` asynchronously
/// and publishes `HYDRA_EVENT_PAUSED` when it gets there — this call only asks.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_pause(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        let mut g = job.lock();
        if !g.is_running() {
            return err::set(
                E::HYDRA_ERR_INVALID_STATE,
                format!("job {job_id} is not running"),
            );
        }
        g.stop = Stop::Pause;
        if let Some(c) = &g.cancel {
            c.store(true, Ordering::Relaxed);
        }
        E::HYDRA_OK
    })
}

/// Resume a paused job.
///
/// Only legal from `HYDRA_JOB_PAUSED`; use [`hydra_job_start`] for anything
/// else. The transfer picks up from the recorded range map rather than from
/// zero, which is what makes an interrupted 4 GB download cost the remainder
/// and not the whole thing.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_resume(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        if job.lock().state != S::HYDRA_JOB_PAUSED as u32 {
            return err::set(
                E::HYDRA_ERR_INVALID_STATE,
                format!("job {job_id} is not paused"),
            );
        }
        match driver::spawn(eng, &job) {
            Ok(()) => {
                eng.emit(&job, hydra_event_type_t::HYDRA_EVENT_RESUMED);
                E::HYDRA_OK
            }
            Err(d) => fail(d),
        }
    })
}

/// Cancel a job.
///
/// `mode` is one of [`hydra_cancel_mode_t`] and decides what happens to the
/// partial file. Cancelling is safe from every non-terminal state — queued,
/// resolving, downloading, verifying, paused — and always ends at
/// `HYDRA_JOB_CANCELLED`.
///
/// A job that is not running is cancelled synchronously; a running one is asked
/// to stop and reaches the terminal state when its transfer unwinds.
///
/// Thread-safe. Non-blocking. May remove a file when `mode` is
/// `HYDRA_CANCEL_REMOVE_PARTIAL`.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_cancel(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    mode: u32,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        if mode > hydra_cancel_mode_t::HYDRA_CANCEL_REMOVE_PARTIAL as u32 {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                format!("cancel mode {mode} is not a valid value"),
            );
        }
        let job = match job_of(&eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        let stop = if mode == hydra_cancel_mode_t::HYDRA_CANCEL_REMOVE_PARTIAL as u32 {
            Stop::CancelRemove
        } else {
            Stop::CancelKeep
        };
        let running = {
            let mut g = job.lock();
            if g.is_terminal() {
                return err::set(
                    E::HYDRA_ERR_INVALID_STATE,
                    format!("job {job_id} has already finished"),
                );
            }
            g.stop = stop;
            if let Some(c) = &g.cancel {
                c.store(true, Ordering::Relaxed);
            }
            g.is_running()
        };
        if !running {
            // Nothing is executing, so nothing will observe the stop flag: take
            // the job to its terminal state here.
            if stop == Stop::CancelRemove {
                let path = job.lock().output_path.clone();
                let _ = std::fs::remove_file(&path);
                let mut g = job.lock();
                g.held.clear();
                g.progress.bytes_downloaded = 0;
            }
            {
                let mut g = job.lock();
                g.state = S::HYDRA_JOB_CANCELLED as u32;
                g.finished_at_ms = crate::engine::now_ms();
                g.stop = Stop::None;
            }
            eng.emit(&job, hydra_event_type_t::HYDRA_EVENT_CANCELLED);
            persist::autosave(&eng);
        }
        E::HYDRA_OK
    })
}

/// Forget a job.
///
/// Only legal once the job is in a terminal state or has never started;
/// removing a running job would leave a transfer writing to a file nobody is
/// tracking.
///
/// The **file is not touched** — deleting a completed download because the
/// application stopped tracking it would be the wrong default, and
/// `HYDRA_CANCEL_REMOVE_PARTIAL` exists for when deletion is what you want.
///
/// The job's **persisted metadata is removed** along with it, so a later
/// [`hydra_engine_restore`] cannot resurrect it. That write is best effort, on
/// the same terms as every other automatic snapshot: call
/// [`hydra_engine_snapshot`] and read the code if you need to know it
/// succeeded.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_remove(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        let job = match job_of(&eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        if job.lock().is_running() {
            return err::set(
                E::HYDRA_ERR_INVALID_STATE,
                format!("job {job_id} is running; pause or cancel it first"),
            );
        }
        eng.remove_job(job_id);
        persist::autosave(&eng);
        E::HYDRA_OK
    })
}

/// Set or clear a job's credentials.
///
/// Used for HTTP basic authentication and for the `ftp://` login. Pass NULL for
/// both to clear them. Takes effect on the next attempt, so a running job must
/// be paused and resumed for a change to apply.
///
/// This exists mainly for restored jobs: credentials are deliberately never
/// written to a state file, so a job that comes back from disk needs them
/// supplied again before it can authenticate.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid; `username` and `password` must each be NULL or a
/// NUL-terminated UTF-8 string valid for this call.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_set_credentials(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    username: *const c_char,
    password: *const c_char,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        // SAFETY: caller's contract.
        let u = match unsafe { mem::cstr_opt(username) } {
            Ok(v) => v.map(str::to_string),
            Err(()) => {
                return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "username is not valid UTF-8")
            }
        };
        // SAFETY: caller's contract.
        let p = match unsafe { mem::cstr_opt(password) } {
            Ok(v) => v.map(str::to_string),
            Err(()) => {
                return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "password is not valid UTF-8")
            }
        };
        if u.iter()
            .chain(p.iter())
            .any(|s| s.chars().any(char::is_control))
        {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                "credentials contain control characters",
            );
        }
        job.set_creds(Creds {
            username: u,
            password: p,
        });
        E::HYDRA_OK
    })
}

/// Re-aim where the object is written.
///
/// Legal only from `HYDRA_JOB_CREATED`, `HYDRA_JOB_PAUSED`,
/// `HYDRA_JOB_FAILED` and `HYDRA_JOB_CANCELLED`. A job that is queued,
/// resolving, downloading or verifying returns `HYDRA_ERR_INVALID_STATE`;
/// pause it first.
///
/// That restriction is not caution, it is correctness. A running transfer has
/// connections writing at absolute offsets into a file it opened, and a range
/// map that describes *that* file. Letting the destination move underneath it
/// would leave the finished ranges in the old path, the retried ranges in the
/// new one, and a range map claiming both are present in a single object —
/// two partial files, each looking complete by length. This call used to take
/// effect "on the next attempt", which is exactly that bug.
///
/// A destination is a filesystem path in this ABI version and nothing else. It
/// covers desktop and an app-private directory on Android or iOS; it does not
/// cover a content URI, a security-scoped resource or a document-provider
/// handle. A future ABI may add other destination kinds alongside the path —
/// the path will keep working, but do not build on the assumption that it is
/// permanently the only storage model.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid and `path` must be a NUL-terminated UTF-8 string valid
/// for this call.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_set_output_path(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    path: *const c_char,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        let job = match job_of(&eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        // SAFETY: caller's contract.
        let p = match unsafe { mem::cstr_req(path) } {
            Ok(s) if !s.is_empty() => s.to_string(),
            _ => {
                return err::set(
                    E::HYDRA_ERR_INVALID_ARGUMENT,
                    "path is NULL, empty, or not valid UTF-8",
                )
            }
        };
        {
            let mut g = job.lock();
            if g.is_running() {
                return err::set(
                    E::HYDRA_ERR_INVALID_STATE,
                    format!(
                        "job {job_id} is active; pause it before moving its destination, \
                         or its ranges would be split across two files"
                    ),
                );
            }
            g.output_path = p;
        }
        persist::autosave(&eng);
        E::HYDRA_OK
    })
}

/// Change a job's rate ceiling, in bytes per second. 0 = unlimited.
///
/// Applies immediately, including to a transfer already running that started
/// with no ceiling of its own. The engine-wide cap still applies on top: the
/// job moves at the lower of the two.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_set_max_bytes_per_second(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    bytes_per_second: u64,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        // Stored raw, NOT as `min(job, engine)`: the engine's limiter is
        // attached to the transfer alongside this one, so the smaller of the
        // two already binds. Folding the engine figure in here would freeze it
        // — a later engine-wide change could then never lift this job's cap.
        job.limiter.set_rate(bytes_per_second);
        E::HYDRA_OK
    })
}

/// Read a job's state.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out_state` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_get_state(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    out_state: *mut hydra_job_state_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out_state.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_state is NULL");
        }
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out_state, err::to_state(job.lock().state)) };
        E::HYDRA_OK
    })
}

/// Read a job's progress.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_get_progress(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    out: *mut hydra_progress_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, job.lock().progress) };
        E::HYDRA_OK
    })
}

/// Take a consistent, owned picture of a job.
///
/// Everything in the result is copied. It stays valid no matter what the engine
/// does next, which is what makes it safe to hand to a UI thread.
///
/// Thread-safe. Non-blocking. **Allocates**: release with
/// [`hydra_job_snapshot_free`].
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_get_snapshot(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    out: *mut hydra_job_snapshot_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        let g = job.lock();
        let snap = hydra_job_snapshot_t {
            id: job.id,
            state: err::to_state(g.state),
            error_code: err::to_code(g.error.as_ref().map(|e| e.code).unwrap_or(0)),
            progress: g.progress,
            url: mem::string_out(g.resolved_url.as_deref().unwrap_or(&job.cfg.urls[0])),
            output_path: mem::string_out(&g.output_path),
            file_name: mem::string_out(g.file_name.as_deref().unwrap_or("")),
            error_message: mem::string_out(
                g.error.as_ref().map(|e| e.message.as_str()).unwrap_or(""),
            ),
            created_at_ms: g.created_at_ms,
            started_at_ms: g.started_at_ms,
            finished_at_ms: g.finished_at_ms,
        };
        drop(g);
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, snap) };
        E::HYDRA_OK
    })
}

/// Release a snapshot's owned strings.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `s` must be NULL or a snapshot this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_snapshot_free(s: *mut hydra_job_snapshot_t) {
    shield_unit(|| {
        if s.is_null() {
            return;
        }
        // SAFETY: caller's contract; each string is replaced with the NULL value
        // so a repeated free is harmless.
        unsafe {
            for f in [
                &mut (*s).url,
                &mut (*s).output_path,
                &mut (*s).file_name,
                &mut (*s).error_message,
            ] {
                let taken = std::ptr::replace(f, hydra_string_t::null());
                mem::string_drop(taken);
            }
        }
    })
}

/// What each source is contributing to a job.
///
/// **Experimental**: this call and [`hydra_source_info_t`] may change within
/// ABI 1. It exists because hydra's multi-source behaviour is worth making
/// visible — an application can show which mirror is carrying the transfer and
/// which one has stalled, instead of a single opaque rate.
///
/// Thread-safe. Non-blocking. **Allocates**: release with
/// [`hydra_source_array_free`].
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_get_sources(
    engine: *mut hydra_engine_t,
    job_id: hydra_job_id_t,
    out: *mut hydra_source_array_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let job = match job_of(eng, job_id) {
            Ok(j) => j,
            Err(e) => return e,
        };
        let g = job.lock();
        let items: Vec<hydra_source_info_t> = g
            .sources
            .iter()
            .enumerate()
            .map(|(i, s)| hydra_source_info_t {
                id: i as hydra_source_id_t,
                url: mem::string_out(&s.url),
                bytes_downloaded: s.bytes,
                bytes_per_second: s.rate,
                latency_us: s.latency_us,
                active_connections: s.conns,
                error_count: s.errors,
                active: u8::from(s.active),
                reserved: [0; 7],
            })
            .collect();
        drop(g);
        // SAFETY: caller's contract.
        unsafe { std::ptr::write(out, mem::sources_out(items)) };
        E::HYDRA_OK
    })
}

/// Release a source array and the strings inside it.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `a` must be NULL or an array this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_source_array_free(a: *mut hydra_source_array_t) {
    shield_unit(|| {
        if a.is_null() {
            return;
        }
        // SAFETY: caller's contract; zeroing makes a repeat free harmless.
        unsafe {
            let taken = std::ptr::replace(
                a,
                hydra_source_array_t {
                    items: std::ptr::null_mut(),
                    len: 0,
                },
            );
            mem::sources_drop(taken);
        }
    })
}

// ==================================================================== events

fn take_event(
    eng: &Arc<Engine>,
    out: *mut hydra_event_t,
    wait: Option<Option<Duration>>,
) -> hydra_error_code_t {
    if out.is_null() {
        return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "event is NULL");
    }
    let got = match wait {
        None => eng.events.try_next(),
        Some(t) => eng.events.wait(t),
    };
    match got {
        Some(ev) => {
            // SAFETY: `out` is a caller-owned writable struct per the contract,
            // and `hydra_event_t` is plain data with no ownership.
            unsafe { std::ptr::write(out, ev) };
            E::HYDRA_OK
        }
        None => {
            if eng.shutdown.load(Ordering::Relaxed) {
                E::HYDRA_ERR_SHUTDOWN
            } else {
                E::HYDRA_ERR_AGAIN
            }
        }
    }
}

/// Take the next event, if one is pending.
///
/// Returns `HYDRA_ERR_AGAIN` when the queue is empty — not a failure, just
/// nothing to report — and `HYDRA_ERR_SHUTDOWN` once the engine has stopped and
/// drained.
///
/// Life-cycle events are delivered before pending progress events, so a
/// completion never waits behind a progress sample. Progress events **are**
/// coalesced: at most one per job is ever pending, and a newer sample replaces
/// an older one. Terminal events are **never** dropped.
///
/// The event is copied into `out`; there is nothing to free and nothing that
/// expires.
///
/// Thread-safe (intended for one consumer). Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_event_next(
    engine: *mut hydra_engine_t,
    out: *mut hydra_event_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        match unsafe { boxed(engine) } {
            Ok(b) => take_event(&b.engine.clone(), out, None),
            Err(e) => e,
        }
    })
}

/// Wait up to `timeout_ms` for an event.
///
/// `0` polls without waiting; `HYDRA_WAIT_FOREVER` waits indefinitely. Returns
/// `HYDRA_ERR_AGAIN` on timeout and `HYDRA_ERR_SHUTDOWN` once the engine has
/// stopped — a shutdown releases every waiter immediately rather than making
/// them sit out their timeouts.
///
/// This is the call a dedicated consumer thread should sit in: it costs nothing
/// while idle, where a polling loop costs a wake-up per interval on a device
/// with a battery.
///
/// Thread-safe (intended for one consumer). **Blocking**. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid and `out` writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_event_wait(
    engine: *mut hydra_engine_t,
    timeout_ms: u32,
    out: *mut hydra_event_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => b.engine.clone(),
            Err(e) => return e,
        };
        let t = match timeout_ms {
            0 => return take_event(&eng, out, None),
            crate::HYDRA_WAIT_FOREVER => None,
            ms => Some(Duration::from_millis(ms as u64)),
        };
        take_event(&eng, out, Some(t))
    })
}

/// Release every thread blocked in [`hydra_event_wait`].
///
/// The woken calls return `HYDRA_ERR_AGAIN`. This is how a host tells its own
/// consumer thread to look at something else — a flag of its own, a request to
/// exit — without having to shut the engine down first.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid.
#[no_mangle]
pub unsafe extern "C" fn hydra_event_wake(engine: *mut hydra_engine_t) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        match unsafe { boxed(engine) } {
            Ok(b) => {
                b.engine.events.wake();
                E::HYDRA_OK
            }
            Err(e) => e,
        }
    })
}

/// Install or clear the optional event callback.
///
/// **Experimental.** The queue is the stable mechanism; this is a convenience
/// layer over it and may change within ABI 1. Getting a foreign callback right
/// differs sharply between the JVM (the thread must be attached), .NET (the
/// delegate must be pinned), Go (a cgo callback lands on a non-Go stack), Swift
/// concurrency and Python (the GIL), and freezing an interface across all of
/// them before any of those bindings exist would be guessing.
///
/// Pass NULL to clear. The callback runs on an engine-owned thread, immediately
/// after the event is queued, and the event is **also** delivered to the queue
/// — installing a callback supplements polling rather than replacing it.
///
/// The callback **must not block and must not call back into the engine**.
///
/// **`user_data` is never owned by hydra and is never freed by hydra.** It is
/// stored, never dereferenced, and handed back verbatim. Freeing it while the
/// callback is installed is a use-after-free in your program, not in hydra's.
///
/// Thread-safe. Non-blocking. Does not allocate.
///
/// # Safety
///
/// `engine` must be valid. `callback` must remain a valid function pointer, and
/// `user_data` a valid token, until it is cleared or the engine is destroyed.
#[no_mangle]
pub unsafe extern "C" fn hydra_event_set_callback(
    engine: *mut hydra_engine_t,
    callback: hydra_event_callback,
    user_data: *mut std::ffi::c_void,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        match unsafe { boxed(engine) } {
            Ok(b) => {
                b.engine.events.set_callback(callback, user_data);
                E::HYDRA_OK
            }
            Err(e) => e,
        }
    })
}

/// Install or clear this engine's log sink.
///
/// Per engine, not per process. Two independent consumers can live in one
/// process — a host application and a plugin, two frameworks in one iOS app,
/// two libraries in one JVM — and a global sink would let the second one
/// silently reconfigure the first one's diagnostics.
///
/// Logs are not events. An event is a state transition your logic acts on and
/// is delivered through the queue with delivery guarantees; a log line is a
/// diagnostic for whoever is debugging, is fire-and-forget, and losing one
/// costs nothing. Do not build application behaviour on this.
///
/// Nothing is written anywhere unless you install a sink: a library that prints
/// to `stderr` on its own initiative corrupts the output of every program that
/// embeds it.
///
/// `max_level` is one of [`hydra_log_level_t`]; messages above it are discarded
/// before they are formatted. Pass NULL as `callback` to clear.
///
/// **`user_data` is never owned by hydra and is never freed by hydra.** It is
/// stored, never dereferenced, and handed back to your function verbatim. It
/// must stay valid until the callback is cleared or the engine is destroyed —
/// freeing it while a sink is installed is a use-after-free in your program,
/// not in hydra's.
///
/// Thread-safe. Non-blocking. Allocates per delivered message.
///
/// # Safety
///
/// `engine` must be valid. `callback` must remain a valid function pointer, and
/// `user_data` a valid token, until the sink is cleared or the engine is
/// destroyed, and the callback must tolerate being called from any thread.
#[no_mangle]
pub unsafe extern "C" fn hydra_engine_set_log_callback(
    engine: *mut hydra_engine_t,
    callback: hydra_log_callback,
    user_data: *mut std::ffi::c_void,
    max_level: u32,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { boxed(engine) } {
            Ok(b) => &b.engine,
            Err(e) => return e,
        };
        if max_level > hydra_log_level_t::HYDRA_LOG_TRACE as u32 {
            return err::set(
                E::HYDRA_ERR_INVALID_ARGUMENT,
                format!("log level {max_level} is not a valid value"),
            );
        }
        eng.logs.set(callback, user_data, max_level);
        E::HYDRA_OK
    })
}

// ================================================================== metalink

/// Magic number for verifying Metalink handle validity.
const ML_MAGIC: u64 = 0x4879_6472_614D_4C4B;

struct MetalinkBox {
    magic: u64,
    doc: crate::metalink::Doc,
}

/// Borrow the document behind a handle.
///
/// # Safety
///
/// `p` must be a handle from one of the `hydra_metalink_*` constructors that has
/// not been passed to [`hydra_metalink_free`].
unsafe fn ml<'a>(p: *mut hydra_metalink_t) -> Result<&'a MetalinkBox, hydra_error_code_t> {
    if p.is_null() {
        return Err(err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "document is NULL"));
    }
    // SAFETY: the caller's contract is that `p` came from a constructor here,
    // each of which hands out exactly `Box::into_raw` of a `MetalinkBox`. The
    // magic check below rejects the common violations of that contract.
    let b = unsafe { &*(p as *mut MetalinkBox) };
    if b.magic != ML_MAGIC {
        return Err(err::set(
            E::HYDRA_ERR_INVALID_ARGUMENT,
            "metalink handle is not valid (already freed, or not from hydra)",
        ));
    }
    Ok(b)
}

fn ml_out(doc: crate::metalink::Doc, out: *mut *mut hydra_metalink_t) -> hydra_error_code_t {
    let b = Box::new(MetalinkBox {
        magic: ML_MAGIC,
        doc,
    });
    // SAFETY: `out` was checked non-null by every caller before this point.
    unsafe { std::ptr::write(out, Box::into_raw(b) as *mut hydra_metalink_t) };
    E::HYDRA_OK
}

/// Parse a Metalink document held in memory.
///
/// `xml` is the document text, NUL-terminated UTF-8. Both dialects are read:
/// Metalink 3.0 (`.metalink`, what mirrormanager and most distribution
/// redirectors emit) and Metalink 4 / RFC 5854 (`.meta4`). The two spell mirror
/// preference on scales that run in OPPOSITE directions; the reader normalises
/// them, so every priority this ABI reports has 1 as best.
///
/// On success `*out_document` owns a document that must be released with
/// [`hydra_metalink_free`].
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `xml` must be a valid NUL-terminated string and `out_document` must be
/// writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_parse(
    xml: *const c_char,
    out_document: *mut *mut hydra_metalink_t,
) -> hydra_error_code_t {
    shield(|| {
        if out_document.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_document is NULL");
        }
        // SAFETY: caller's contract.
        let text = match unsafe { mem::cstr_req(xml) } {
            Ok(s) => s,
            Err(()) => {
                return err::set(
                    E::HYDRA_ERR_INVALID_ARGUMENT,
                    "xml is NULL or not valid UTF-8",
                )
            }
        };
        match crate::metalink::parse(text, "<memory>") {
            Ok(doc) => ml_out(doc, out_document),
            Err(d) => fail(d),
        }
    })
}

/// Read a Metalink document from a local file.
///
/// Thread-safe. Blocking (one file read). Allocates internally.
///
/// # Safety
///
/// `path` must be a valid NUL-terminated string and `out_document` must be
/// writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_open(
    path: *const c_char,
    out_document: *mut *mut hydra_metalink_t,
) -> hydra_error_code_t {
    shield(|| {
        if out_document.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_document is NULL");
        }
        // SAFETY: caller's contract.
        let p = match unsafe { mem::cstr_req(path) } {
            Ok(s) => s,
            Err(()) => {
                return err::set(
                    E::HYDRA_ERR_INVALID_ARGUMENT,
                    "path is NULL or not valid UTF-8",
                )
            }
        };
        match crate::metalink::open(p) {
            Ok(doc) => ml_out(doc, out_document),
            Err(d) => fail(d),
        }
    })
}

/// Fetch a Metalink document over HTTP and read it.
///
/// Runs on the engine's own runtime and **blocks the calling thread** until the
/// document arrives or the fetch fails — a mirror list is kilobytes, and an
/// application that wants it off the UI thread has its own thread pool for that.
/// Redirects are followed, because mirror redirectors use them constantly.
///
/// The body is capped at 4 MiB: it is fetched before anything about it is known,
/// and an unbounded read of a body chosen by whoever answers is a
/// memory-exhaustion primitive no amount of care in the parser can fix.
///
/// Thread-safe. Blocking. Allocates internally.
///
/// # Safety
///
/// `engine` must be valid, `url` must be a valid NUL-terminated string, and
/// `out_document` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_fetch(
    engine: *mut hydra_engine_t,
    url: *const c_char,
    out_document: *mut *mut hydra_metalink_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        if out_document.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_document is NULL");
        }
        // SAFETY: caller's contract.
        let u = match unsafe { mem::cstr_req(url) } {
            Ok(s) => s.to_string(),
            Err(()) => {
                return err::set(
                    E::HYDRA_ERR_INVALID_ARGUMENT,
                    "url is NULL or not valid UTF-8",
                )
            }
        };
        let conn = match eng.connector(None) {
            Ok(c) => c,
            Err(d) => return fail(d),
        };
        let agent = eng.cfg.user_agent.clone();
        let rt = eng.rt.clone();
        let res = std::thread::scope(|s| {
            s.spawn(|| rt.block_on(crate::metalink::fetch(&conn, &u, &[], &agent, 10)))
                .join()
        });
        match res {
            Ok(Ok(doc)) => ml_out(doc, out_document),
            Ok(Err(d)) => fail(d),
            Err(_) => err::set(E::HYDRA_ERR_INTERNAL, "metalink fetch task panicked"),
        }
    })
}

/// Release a parsed document.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `document` must be NULL or a handle this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_free(document: *mut hydra_metalink_t) {
    shield_unit(|| {
        if document.is_null() {
            return;
        }
        // SAFETY: caller's contract. The magic is cleared first so a repeat free
        // is rejected by `ml` rather than freeing a second time.
        unsafe {
            let b = &mut *(document as *mut MetalinkBox);
            if b.magic != ML_MAGIC {
                return;
            }
            b.magic = 0;
            drop(Box::from_raw(document as *mut MetalinkBox));
        }
    })
}

/// Which dialect a document was written in.
///
/// Returns `HYDRA_METALINK_UNKNOWN` for an invalid handle, which is also what a
/// document with no recognisable namespace reports — the distinction is not one
/// a caller can act on differently.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `document` must be a valid handle.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_version(
    document: *mut hydra_metalink_t,
) -> hydra_metalink_version_t {
    shield_value(hydra_metalink_version_t::HYDRA_METALINK_UNKNOWN, || {
        // SAFETY: caller's contract.
        match unsafe { ml(document) } {
            Ok(b) => crate::metalink::version_of(&b.doc),
            Err(_) => hydra_metalink_version_t::HYDRA_METALINK_UNKNOWN,
        }
    })
}

/// Every file entry a document describes.
///
/// This is what a host application shows a user before anything is fetched: the
/// names, the sizes, whether a digest and a piece list are published, and how
/// many of the listed mirrors this build can actually fetch from. A mirror list
/// that silently loses two thirds of its entries to an unsupported scheme is
/// worth seeing before a multi-gigabyte download rather than after.
///
/// Release with [`hydra_metalink_file_array_free`].
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `document` must be a valid handle and `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_files(
    document: *mut hydra_metalink_t,
    out: *mut hydra_metalink_file_array_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let b = match unsafe { ml(document) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let items: Vec<hydra_metalink_file_t> = b
            .doc
            .inner
            .files
            .iter()
            .map(|f| {
                let safe = f.safe_name();
                let pieces = f.pieces.as_ref();
                hydra_metalink_file_t {
                    // Absent, not empty, when the name is refused: the field's
                    // contract says so, and an empty string invites the caller
                    // to save the file under it.
                    name: safe
                        .as_deref()
                        .map(mem::string_out)
                        .unwrap_or_else(|_| hydra_string_t::null()),
                    digest: f
                        .best_hash()
                        .map(|h| mem::string_out(&h.spec()))
                        .unwrap_or_else(hydra_string_t::null),
                    version: f
                        .version
                        .as_deref()
                        .map(mem::string_out)
                        .unwrap_or_else(hydra_string_t::null),
                    size: f.size.unwrap_or(0),
                    piece_length: pieces.map(|p| p.length).unwrap_or(0),
                    piece_count: pieces.map(|p| p.hashes.len()).unwrap_or(0),
                    mirror_count: f.urls.len(),
                    fetchable_count: f.fetchable_urls().len(),
                    max_connections: f.default_max_connections.unwrap_or(0).min(64) as u32,
                    pieces_tile: u8::from(
                        pieces.is_some_and(|p| f.size.is_some_and(|s| p.covers(s))),
                    ),
                    signed: u8::from(f.signature.is_some()),
                    name_usable: u8::from(safe.is_ok()),
                    reserved: [0; 5],
                }
            })
            .collect();
        // SAFETY: `out` was checked non-null above.
        unsafe { std::ptr::write(out, mem::metalink_files_out(items)) };
        E::HYDRA_OK
    })
}

/// Release a file array and the strings inside it.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `a` must be NULL or an array this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_file_array_free(a: *mut hydra_metalink_file_array_t) {
    shield_unit(|| {
        if a.is_null() {
            return;
        }
        // SAFETY: caller's contract; zeroing makes a repeat free harmless.
        unsafe {
            let taken = std::ptr::replace(
                a,
                hydra_metalink_file_array_t {
                    items: std::ptr::null_mut(),
                    len: 0,
                },
            );
            mem::metalink_files_drop(taken);
        }
    })
}

/// The mirrors of one file entry, in the order hydra would use them.
///
/// Best first, with `priority` renumbered densely from 1 whichever dialect the
/// document used — so a caller never has to know that Metalink 3.0's scale runs
/// the other way. Only mirrors this build has a transport for are returned;
/// `hydra_metalink_file_t.mirror_count` against `fetchable_count` is how many
/// were dropped.
///
/// Release with [`hydra_metalink_url_array_free`].
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `document` must be a valid handle and `out` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_mirrors(
    document: *mut hydra_metalink_t,
    file_index: usize,
    out: *mut hydra_metalink_url_array_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let b = match unsafe { ml(document) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        if out.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out is NULL");
        }
        let Some(f) = b.doc.inner.files.get(file_index) else {
            return err::set(
                E::HYDRA_ERR_NOT_FOUND,
                "file_index is past the end of the document",
            );
        };
        let items: Vec<hydra_metalink_url_t> = crate::metalink::ranked(f)
            .into_iter()
            .map(
                |(url, plan, location, protocol, _stated)| hydra_metalink_url_t {
                    url: mem::string_out(&url),
                    location: location
                        .as_deref()
                        .map(mem::string_out)
                        .unwrap_or_else(hydra_string_t::null),
                    protocol: mem::string_out(&protocol),
                    priority: plan.priority,
                    max_connections: plan.max_connections.unwrap_or(0).min(64) as u32,
                    fetchable: 1,
                    reserved: [0; 7],
                },
            )
            .collect();
        // SAFETY: `out` was checked non-null above.
        unsafe { std::ptr::write(out, mem::metalink_urls_out(items)) };
        E::HYDRA_OK
    })
}

/// Release a mirror array and the strings inside it.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `a` must be NULL or an array this library produced and not yet freed.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_url_array_free(a: *mut hydra_metalink_url_array_t) {
    shield_unit(|| {
        if a.is_null() {
            return;
        }
        // SAFETY: caller's contract; zeroing makes a repeat free harmless.
        unsafe {
            let taken = std::ptr::replace(
                a,
                hydra_metalink_url_array_t {
                    items: std::ptr::null_mut(),
                    len: 0,
                },
            );
            mem::metalink_urls_drop(taken);
        }
    })
}

/// Create a job for one entry of a Metalink document.
///
/// `config` is the ordinary job configuration and supplies everything about how
/// the transfer should behave — output path, headers, proxy, rate cap, retries,
/// priority. Its `urls` and `url_count` are IGNORED and may be NULL/0: the
/// document supplies the sources. Its `checksum` is honoured when set and
/// otherwise filled in from the document's strongest published digest, so a
/// caller with a digest from a signed announcement keeps it and a caller with
/// none still gets verification.
///
/// What the document adds beyond the URLs is the point of this call:
///
/// * the **size**, which admits every agreeing mirror to a multi-source transfer
///   without the `ETag` match independent mirror operators cannot produce;
/// * the **ranking**, which decides the first split and the reserve order;
/// * the **reserve bench** — mirrors past the connection budget, substituted in
///   place when a source dies, so nineteen mirrors are worth more than four;
/// * **`<pieces>`**, verified after the transfer with a failing chunk refetched
///   from a different mirror instead of the whole object being downloaded again.
///
/// A `<signature>` in the document is recorded and NOT verified. Verify it
/// yourself before trusting the digests it covers.
///
/// Thread-safe. Non-blocking. Allocates internally.
///
/// # Safety
///
/// `engine` and `document` must be valid, `config` must have been initialised by
/// [`hydra_job_config_init`], and `out_job_id` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_job_create_from_metalink(
    engine: *mut hydra_engine_t,
    document: *mut hydra_metalink_t,
    file_index: usize,
    config: *const hydra_job_config_t,
    out_job_id: *mut hydra_job_id_t,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let eng = match unsafe { live(engine) } {
            Ok(e) => e,
            Err(e) => return e,
        };
        // SAFETY: caller's contract.
        let b = match unsafe { ml(document) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        if out_job_id.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_job_id is NULL");
        }
        let chosen = match crate::metalink::choose(&b.doc, file_index) {
            Ok(c) => c,
            Err(d) => return fail(d),
        };
        // The document's URLs stand in for the caller's while the configuration
        // is validated, so every check `hydra_job_create` makes — output path,
        // headers, proxy, credentials — is made here too, on the same code.
        let holders: Vec<std::ffi::CString> = match chosen
            .urls
            .iter()
            .map(|u| std::ffi::CString::new(u.as_str()))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) => v,
            Err(_) => {
                return err::set(
                    E::HYDRA_ERR_INVALID_URL,
                    "the document names a URL containing a NUL byte",
                )
            }
        };
        let ptrs: Vec<*const c_char> = holders.iter().map(|c| c.as_ptr()).collect();
        // SAFETY: caller's contract that `config` is an initialised job config.
        let mut patched = unsafe { std::ptr::read(config) };
        patched.urls = ptrs.as_ptr();
        patched.url_count = ptrs.len();
        // SAFETY: `patched` is a local copy whose url array outlives the call.
        let (mut cfg, output, creds) = match unsafe { convert::job_cfg(&patched, &eng.cfg) } {
            Ok(v) => v,
            Err(d) => return fail(d),
        };
        cfg.source_plans = chosen.plans;
        cfg.attested_size = chosen.size;
        cfg.pieces = chosen.pieces;
        cfg.attested_by = Some(b.doc.origin.clone());
        // The document's digest, when the caller did not bring their own. A
        // digest the caller typed came from somewhere they chose and outranks
        // one that arrived over the same session as the mirror list.
        if cfg.checksum.is_none() {
            cfg.checksum = chosen
                .digest
                .as_deref()
                .and_then(crate::metalink::checksum_of);
        }
        // SAFETY: caller's contract.
        let auto_start = unsafe { (*config).auto_start } != 0;
        let job = eng.insert_job(cfg, output, creds);
        // SAFETY: `out_job_id` was checked non-null above.
        unsafe { std::ptr::write(out_job_id, job.id) };
        eng.emit(&job, hydra_event_type_t::HYDRA_EVENT_JOB_CREATED);
        crate::log::log_at!(
            eng,
            hydra_log_level_t::HYDRA_LOG_INFO,
            "job {} created from {} for {:?}: {} mirror(s), {} bytes attested, {} piece(s)",
            job.id,
            b.doc.origin,
            chosen.name,
            job.cfg.urls.len(),
            job.cfg.attested_size.unwrap_or(0),
            job.cfg
                .pieces
                .as_ref()
                .map(|m| m.chunks.digests.len())
                .unwrap_or(0)
        );
        if auto_start {
            if let Err(d) = driver::spawn(eng, &job) {
                return fail(d);
            }
        }
        E::HYDRA_OK
    })
}

/// Find a file entry by name.
///
/// Matches either the document's full relative name or just the base name,
/// because an application passes on what a user picked from a listing and a
/// listing generally shows the base name.
///
/// Returns `HYDRA_ERR_NOT_FOUND` when no entry matches, leaving `*out_index`
/// untouched.
///
/// Thread-safe. Non-blocking.
///
/// # Safety
///
/// `document` must be a valid handle, `name` must be a valid NUL-terminated
/// string, and `out_index` must be writable.
#[no_mangle]
pub unsafe extern "C" fn hydra_metalink_find_file(
    document: *mut hydra_metalink_t,
    name: *const c_char,
    out_index: *mut usize,
) -> hydra_error_code_t {
    shield(|| {
        // SAFETY: caller's contract.
        let b = match unsafe { ml(document) } {
            Ok(b) => b,
            Err(e) => return e,
        };
        if out_index.is_null() {
            return err::set(E::HYDRA_ERR_INVALID_ARGUMENT, "out_index is NULL");
        }
        // SAFETY: caller's contract.
        let want = match unsafe { mem::cstr_req(name) } {
            Ok(s) => s,
            Err(()) => {
                return err::set(
                    E::HYDRA_ERR_INVALID_ARGUMENT,
                    "name is NULL or not valid UTF-8",
                )
            }
        };
        match crate::metalink::index_of(&b.doc, want) {
            Some(i) => {
                // SAFETY: `out_index` was checked non-null above.
                unsafe { std::ptr::write(out_index, i) };
                E::HYDRA_OK
            }
            None => err::set(
                E::HYDRA_ERR_NOT_FOUND,
                "the document describes no file with that name",
            ),
        }
    })
}
