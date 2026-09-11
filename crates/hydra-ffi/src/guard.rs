// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Panic boundary safety wrappers.
//!
//! Catches unwinding panics before crossing FFI boundaries and translates them
//! to `HYDRA_ERR_INTERNAL` while storing the panic message in thread-local error state.

use crate::abi::hydra_error_code_t as E;
use crate::err;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Executes `f`, catching panics and returning `HYDRA_ERR_INTERNAL` on unwind.
pub(crate) fn shield<F: FnOnce() -> E>(f: F) -> E {
    err::clear();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(code) => code,
        Err(p) => {
            err::set(E::HYDRA_ERR_INTERNAL, panic_message(&p));
            E::HYDRA_ERR_INTERNAL
        }
    }
}

/// Executes `f`, returning `fallback` if an unwinding panic is caught.
pub(crate) fn shield_value<T, F: FnOnce() -> T>(fallback: T, f: F) -> T {
    err::clear();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => v,
        Err(p) => {
            err::set(E::HYDRA_ERR_INTERNAL, panic_message(&p));
            fallback
        }
    }
}

/// Executes `f`, silently catching any unwinding panic.
pub(crate) fn shield_unit<F: FnOnce()>(f: F) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

/// Extracts a string message from a caught panic payload.
fn panic_message(p: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        format!("internal error (panic): {s}")
    } else if let Some(s) = p.downcast_ref::<String>() {
        format!("internal error (panic): {s}")
    } else {
        "internal error (panic)".to_string()
    }
}
