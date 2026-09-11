// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Per-engine structured log sink and dispatch.

use crate::abi::{hydra_log_callback, hydra_log_level_t as L};
use std::ffi::CString;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

struct Inner {
    f: hydra_log_callback,
    user_data: *mut std::ffi::c_void,
}

// SAFETY: `user_data` is an opaque pointer managed by the host application.
unsafe impl Send for Inner {}
// SAFETY: `user_data` is an opaque pointer managed by the host application.
unsafe impl Sync for Inner {}

/// Thread-safe engine log sink.
#[derive(Default)]
pub(crate) struct LogSink {
    inner: Mutex<Option<Inner>>,
    level: AtomicU32,
}

impl LogSink {
    /// Sets or clears the log callback and minimum verbosity level.
    pub(crate) fn set(&self, f: hydra_log_callback, user_data: *mut std::ffi::c_void, level: u32) {
        self.level.store(level, Ordering::Relaxed);
        let mut g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        *g = f.map(|_| Inner { f, user_data });
    }

    /// Whether anything would consume a message at this level.
    pub(crate) fn enabled(&self, level: L) -> bool {
        (level as u32) <= self.level.load(Ordering::Relaxed)
            && self.inner.lock().map(|g| g.is_some()).unwrap_or(false)
    }

    /// Deliver a message, if anything is listening.
    pub(crate) fn emit(&self, level: L, message: &str) {
        if (level as u32) > self.level.load(Ordering::Relaxed) {
            return;
        }
        let g = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(s) = g.as_ref() else { return };
        let Some(f) = s.f else { return };
        // A NUL inside the message would truncate it at the boundary; replace
        // rather than lose the tail.
        let owned = match CString::new(message.replace('\0', "?")) {
            Ok(c) => c,
            Err(_) => return,
        };
        // SAFETY: `f` came from the host through
        // `hydra_engine_set_log_callback`, and `owned` outlives the call.
        unsafe { f(level as u32, owned.as_ptr(), s.user_data) };
    }
}

/// Log against an engine's sink, formatting only if something will read it.
macro_rules! log_at {
    ($engine:expr, $level:expr, $($arg:tt)*) => {
        if $engine.logs.enabled($level) {
            $engine.logs.emit($level, &format!($($arg)*));
        }
    };
}

pub(crate) use log_at;
