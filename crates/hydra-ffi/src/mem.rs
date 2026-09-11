// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Memory management and ownership transfer across the C ABI boundary.
//!
//! Guidelines:
//! - All memory allocated by Rust must be freed via corresponding `*_free` functions.
//! - Pointers passed into hydra functions are borrowed only for the duration of the call.

use crate::abi::{
    hydra_job_id_array_t, hydra_job_id_t, hydra_metalink_file_array_t, hydra_metalink_file_t,
    hydra_metalink_url_array_t, hydra_metalink_url_t, hydra_source_array_t, hydra_source_info_t,
    hydra_string_t,
};
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// Converts a Rust string slice into an owned, NUL-terminated C string container.
pub(crate) fn string_out(s: &str) -> hydra_string_t {
    let cleaned;
    let src = if s.as_bytes().contains(&0) {
        cleaned = s.replace('\0', "\u{fffd}");
        cleaned.as_str()
    } else {
        s
    };
    let len = src.len();
    match CString::new(src) {
        Ok(c) => hydra_string_t {
            data: c.into_raw(),
            len,
        },
        Err(_) => hydra_string_t::null(),
    }
}

/// Frees an owned `hydra_string_t`. Safe to call on null values.
pub(crate) fn string_drop(s: hydra_string_t) {
    if !s.data.is_null() {
        // SAFETY: pointer originates from `CString::into_raw` in `string_out`.
        unsafe {
            drop(CString::from_raw(s.data));
        }
    }
}

/// Borrows a C string as an optional UTF-8 string slice. Returns `Ok(None)` for null pointers.
///
/// # Safety
/// `p` must be NULL or point to a valid NUL-terminated C string for the call duration.
pub(crate) unsafe fn cstr_opt<'a>(p: *const c_char) -> Result<Option<&'a str>, ()> {
    if p.is_null() {
        return Ok(None);
    }
    // SAFETY: caller guarantees valid NUL-terminated string.
    unsafe { CStr::from_ptr(p) }
        .to_str()
        .map(Some)
        .map_err(|_| ())
}

/// Borrows a required non-null C string as a UTF-8 string slice.
///
/// # Safety
/// `p` must point to a valid NUL-terminated C string.
pub(crate) unsafe fn cstr_req<'a>(p: *const c_char) -> Result<&'a str, ()> {
    // SAFETY: caller guarantees valid NUL-terminated string.
    match unsafe { cstr_opt(p) }? {
        Some(s) => Ok(s),
        None => Err(()),
    }
}

/// Converts a `Vec<T>` into a raw pointer and length pair.
fn vec_out<T>(v: Vec<T>) -> (*mut T, usize) {
    if v.is_empty() {
        return (std::ptr::null_mut(), 0);
    }
    let b = v.into_boxed_slice();
    let len = b.len();
    (Box::into_raw(b) as *mut T, len)
}

/// Reclaims a pointer/length slice allocation created by `vec_out`.
///
/// # Safety
/// `ptr` and `len` must match values produced by `vec_out`.
unsafe fn vec_drop<T>(ptr: *mut T, len: usize) {
    if !ptr.is_null() && len != 0 {
        // SAFETY: slice was allocated via `Box::into_raw` from a boxed slice of length `len`.
        drop(unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) });
    }
}

/// Exports a list of source info records to an ABI-compatible array.
pub(crate) fn sources_out(v: Vec<hydra_source_info_t>) -> hydra_source_array_t {
    let (items, len) = vec_out(v);
    hydra_source_array_t { items, len }
}

/// Frees an owned source array and its internal string allocations.
///
/// # Safety
/// `a` must be an array created by `sources_out`.
pub(crate) unsafe fn sources_drop(a: hydra_source_array_t) {
    if a.items.is_null() || a.len == 0 {
        return;
    }
    // SAFETY: reconstructs the boxed slice from original pointer and length.
    let items = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(a.items, a.len)) };
    for it in items.iter() {
        string_drop(hydra_string_t {
            data: it.url.data,
            len: it.url.len,
        });
    }
    drop(items);
}

/// Exports a list of job IDs to an ABI-compatible array.
pub(crate) fn ids_out(v: Vec<hydra_job_id_t>) -> hydra_job_id_array_t {
    let (items, len) = vec_out(v);
    hydra_job_id_array_t { items, len }
}

/// Frees an owned job ID array.
///
/// # Safety
/// `a` must be an array created by `ids_out`.
pub(crate) unsafe fn ids_drop(a: hydra_job_id_array_t) {
    // SAFETY: array allocation was created by `vec_out`.
    unsafe { vec_drop(a.items, a.len) }
}

/// Exports a list of Metalink file entries to an ABI-compatible array.
pub(crate) fn metalink_files_out(v: Vec<hydra_metalink_file_t>) -> hydra_metalink_file_array_t {
    let (items, len) = vec_out(v);
    hydra_metalink_file_array_t { items, len }
}

/// Frees an owned file array and its internal string allocations.
///
/// # Safety
/// `a` must be an array created by `metalink_files_out`.
pub(crate) unsafe fn metalink_files_drop(a: hydra_metalink_file_array_t) {
    if a.items.is_null() || a.len == 0 {
        return;
    }
    // SAFETY: reconstructs the boxed slice from original pointer and length.
    let items = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(a.items, a.len)) };
    for it in items.iter() {
        for s in [&it.name, &it.digest, &it.version] {
            string_drop(hydra_string_t {
                data: s.data,
                len: s.len,
            });
        }
    }
    drop(items);
}

/// Exports a list of Metalink mirrors to an ABI-compatible array.
pub(crate) fn metalink_urls_out(v: Vec<hydra_metalink_url_t>) -> hydra_metalink_url_array_t {
    let (items, len) = vec_out(v);
    hydra_metalink_url_array_t { items, len }
}

/// Frees an owned mirror array and its internal string allocations.
///
/// # Safety
/// `a` must be an array created by `metalink_urls_out`.
pub(crate) unsafe fn metalink_urls_drop(a: hydra_metalink_url_array_t) {
    if a.items.is_null() || a.len == 0 {
        return;
    }
    // SAFETY: reconstructs the boxed slice from original pointer and length.
    let items = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(a.items, a.len)) };
    for it in items.iter() {
        for s in [&it.url, &it.location, &it.protocol] {
            string_drop(hydra_string_t {
                data: s.data,
                len: s.len,
            });
        }
    }
    drop(items);
}
