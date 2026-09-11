// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Error mapping, conversion, and thread-local error state management.

use crate::abi::hydra_error_code_t as E;
use crate::abi::{hydra_error_t, hydra_string_t};
use std::cell::RefCell;

/// Internal error container.
#[derive(Clone, Debug, Default)]
pub(crate) struct Detail {
    pub code: u32,
    pub os_error: i32,
    pub http_status: i32,
    pub message: String,
}

thread_local! {
    static LAST: RefCell<Option<Detail>> = const { RefCell::new(None) };
}

/// Sets the calling thread's last error and returns the code.
pub(crate) fn set(code: E, message: impl Into<String>) -> E {
    set_detail(Detail {
        code: code as u32,
        os_error: 0,
        http_status: 0,
        message: message.into(),
    });
    code
}

/// Sets the calling thread's last detailed error.
pub(crate) fn set_detail(d: Detail) {
    LAST.with(|c| *c.borrow_mut() = Some(d));
}

/// Clears the calling thread's last error.
pub(crate) fn clear() {
    LAST.with(|c| *c.borrow_mut() = None);
}

/// Takes the calling thread's last error, if set.
pub(crate) fn take() -> Option<Detail> {
    LAST.with(|c| c.borrow_mut().take())
}

impl Detail {
    /// Converts this detail into an ABI-compatible owned error struct.
    pub(crate) fn into_abi(self) -> hydra_error_t {
        hydra_error_t {
            code: to_code(self.code),
            os_error: self.os_error,
            http_status: self.http_status,
            message: crate::mem::string_out(&self.message),
        }
    }
}

impl Default for hydra_error_t {
    fn default() -> Self {
        Self::ok()
    }
}

/// Maps an `std::io::Error` to an appropriate ABI error code and detail.
pub(crate) fn from_io(e: &std::io::Error) -> Detail {
    use std::io::ErrorKind as K;
    let code = match e.kind() {
        K::PermissionDenied => E::HYDRA_ERR_PERMISSION,
        K::NotFound => E::HYDRA_ERR_NOT_FOUND,
        K::AlreadyExists => E::HYDRA_ERR_ALREADY_EXISTS,
        K::TimedOut => E::HYDRA_ERR_TIMEOUT,
        K::Interrupted => E::HYDRA_ERR_CANCELLED,
        K::ConnectionRefused | K::ConnectionReset | K::ConnectionAborted | K::NotConnected => {
            E::HYDRA_ERR_CONNECTION
        }
        K::AddrInUse | K::AddrNotAvailable | K::BrokenPipe => E::HYDRA_ERR_CONNECTION,
        K::InvalidInput => E::HYDRA_ERR_INVALID_ARGUMENT,
        K::InvalidData | K::UnexpectedEof => E::HYDRA_ERR_PROTOCOL,
        _ => E::HYDRA_ERR_IO,
    };
    let os = e.raw_os_error().unwrap_or(0);
    // ENOSPC is 28 across Unix/Windows standard errnos.
    let code = if os == 28 {
        E::HYDRA_ERR_NO_SPACE
    } else {
        code
    };
    Detail {
        code: code as u32,
        os_error: os,
        http_status: 0,
        message: e.to_string(),
    }
}

/// Maps an integer error code to its ABI enum value.
pub(crate) fn to_code(code: u32) -> E {
    match code {
        0 => E::HYDRA_OK,
        1 => E::HYDRA_ERR_INVALID_ARGUMENT,
        2 => E::HYDRA_ERR_INVALID_URL,
        3 => E::HYDRA_ERR_INVALID_STATE,
        4 => E::HYDRA_ERR_UNSUPPORTED,
        5 => E::HYDRA_ERR_AGAIN,
        6 => E::HYDRA_ERR_NETWORK,
        7 => E::HYDRA_ERR_CONNECTION,
        8 => E::HYDRA_ERR_TIMEOUT,
        9 => E::HYDRA_ERR_PROTOCOL,
        10 => E::HYDRA_ERR_IO,
        11 => E::HYDRA_ERR_PERMISSION,
        12 => E::HYDRA_ERR_NO_SPACE,
        13 => E::HYDRA_ERR_CHECKSUM,
        14 => E::HYDRA_ERR_VERIFICATION,
        15 => E::HYDRA_ERR_CANCELLED,
        16 => E::HYDRA_ERR_NOT_FOUND,
        17 => E::HYDRA_ERR_ALREADY_EXISTS,
        18 => E::HYDRA_ERR_RESOURCE_LIMIT,
        19 => E::HYDRA_ERR_SHUTDOWN,
        _ => E::HYDRA_ERR_INTERNAL,
    }
}

/// Maps an integer job state to its ABI enum value.
pub(crate) fn to_state(state: u32) -> crate::abi::hydra_job_state_t {
    use crate::abi::hydra_job_state_t as S;
    match state {
        1 => S::HYDRA_JOB_QUEUED,
        2 => S::HYDRA_JOB_RESOLVING,
        3 => S::HYDRA_JOB_DOWNLOADING,
        4 => S::HYDRA_JOB_PAUSED,
        5 => S::HYDRA_JOB_VERIFYING,
        6 => S::HYDRA_JOB_COMPLETED,
        7 => S::HYDRA_JOB_FAILED,
        8 => S::HYDRA_JOB_CANCELLED,
        _ => S::HYDRA_JOB_CREATED,
    }
}

/// The stable spelling of a code, for logs and for bindings that want to print
/// one without carrying their own table.
pub(crate) fn name(code: u32) -> &'static str {
    match code {
        0 => "HYDRA_OK",
        1 => "HYDRA_ERR_INVALID_ARGUMENT",
        2 => "HYDRA_ERR_INVALID_URL",
        3 => "HYDRA_ERR_INVALID_STATE",
        4 => "HYDRA_ERR_UNSUPPORTED",
        5 => "HYDRA_ERR_AGAIN",
        6 => "HYDRA_ERR_NETWORK",
        7 => "HYDRA_ERR_CONNECTION",
        8 => "HYDRA_ERR_TIMEOUT",
        9 => "HYDRA_ERR_PROTOCOL",
        10 => "HYDRA_ERR_IO",
        11 => "HYDRA_ERR_PERMISSION",
        12 => "HYDRA_ERR_NO_SPACE",
        13 => "HYDRA_ERR_CHECKSUM",
        14 => "HYDRA_ERR_VERIFICATION",
        15 => "HYDRA_ERR_CANCELLED",
        16 => "HYDRA_ERR_NOT_FOUND",
        17 => "HYDRA_ERR_ALREADY_EXISTS",
        18 => "HYDRA_ERR_RESOURCE_LIMIT",
        19 => "HYDRA_ERR_SHUTDOWN",
        20 => "HYDRA_ERR_INTERNAL",
        _ => "HYDRA_ERR_UNKNOWN",
    }
}

/// The empty error object, for an out-parameter on a successful call.
pub(crate) fn ok_error() -> hydra_error_t {
    hydra_error_t {
        code: E::HYDRA_OK,
        os_error: 0,
        http_status: 0,
        message: hydra_string_t::null(),
    }
}
