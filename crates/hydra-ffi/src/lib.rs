// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// Permissive on purpose, and NOT GPL like the `hydra` CLI that ships beside it.
// The entire point of this crate is that a third party can embed the engine in
// an application of their own; copyleft here would defeat that, and Rust's
// static linking would propagate it to every downstream binary. This crate must
// therefore never gain a dependency on hya-cli, hya-gui or hya-host.
// See LICENSING.md.

//! # libhydra — C ABI for the hydra download engine
//!
//! Provides a stable C ABI over `hya-core` and `hya-net`, enabling hydra
//! to be embedded into applications across C, Go, Swift, Kotlin, Dart, C#, and Python.
//!
//! ## Design principles
//!
//! `libhydra` is an embeddable engine, not a Rust-to-C wrapper. The ABI
//! prioritises, in this order: stable binary compatibility, explicit
//! ownership, panic isolation, runtime independence, thread safety, bounded
//! asynchronous events, cross-language interoperability, platform
//! independence.
//!
//! - **Canonical C ABI**: Exposes stable C symbols with opaque handles (`hydra_engine_t`) and integer job IDs (`hydra_job_id_t`).
//! - **Isolated Memory**: Hydra manages its own allocations; callers free returned objects via matching `*_free` functions.
//! - **Asynchronous Queue**: Non-blocking, bounded event queue with coalescing progress events and guaranteed terminal delivery. The queue is the primitive; callbacks are an optional convenience.
//! - **Panic Safety**: All exported functions catch panics internally and translate them to error codes.
//! - **Self-Contained Runtime**: The engine manages its internal Tokio worker threads without requiring a host event loop.
//!
//! ## The ABI contract
//!
//! `docs/ffi/ABI.md` is the canonical specification, and `include/hydra.h` is
//! generated from this crate. Within ABI version 1 nothing already published
//! may move: fields keep their offset, width and meaning, enumerator values are
//! never reassigned, exported symbols never disappear, and new fields may only
//! be appended to the two size-prefixed configuration structs. Anything else
//! requires `HYDRA_FFI_ABI_VERSION = 2`.
//!
//! That is enforced rather than asserted. `crates/hydra-ffi/abi/abi-1.manifest`
//! holds the frozen layout and `scripts/ffi-abi-compat.sh` checks this crate
//! against it on every pull request, alongside a build of every published
//! header against the library from the current branch.

#![allow(non_camel_case_types)]
#![warn(missing_docs)]
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod abi;
mod convert;
mod driver;
mod engine;
mod err;
mod event;
mod exports;
mod gate;
mod guard;
mod log;
mod mem;
mod metalink;
mod persist;
mod url;
mod verify;

pub use abi::*;
pub use exports::*;

/// The ABI version implemented by this library.
pub const HYDRA_FFI_ABI_VERSION: u32 = 1;

/// The version field value stamped by `hydra_engine_config_init`.
pub const HYDRA_ENGINE_CONFIG_VERSION: u32 = 1;

/// The version field value stamped by `hydra_job_config_init`.
pub const HYDRA_JOB_CONFIG_VERSION: u32 = 1;

/// Value indicating an indefinite wait for `hydra_event_wait`.
pub const HYDRA_WAIT_FOREVER: u32 = u32::MAX;

/// Library version string.
///
/// `crates/hydra-ffi/Cargo.toml` is the single place this version is written;
/// everything else — this constant, the numeric components below,
/// `hydra_ffi_version_string()` and the `#define`s in `include/hydra.h` — is
/// derived from it. Bumping the crate version is the whole edit.
pub const HYDRA_FFI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Major version component of `HYDRA_FFI_VERSION`.
pub const HYDRA_FFI_VERSION_MAJOR: u32 = version_component(env!("CARGO_PKG_VERSION_MAJOR"));
/// Minor version component of `HYDRA_FFI_VERSION`.
pub const HYDRA_FFI_VERSION_MINOR: u32 = version_component(env!("CARGO_PKG_VERSION_MINOR"));
/// Patch version component of `HYDRA_FFI_VERSION`.
pub const HYDRA_FFI_VERSION_PATCH: u32 = version_component(env!("CARGO_PKG_VERSION_PATCH"));

/// Numeric version encoded as `major * 1_000_000 + minor * 1_000 + patch` for preprocessor checks.
pub const HYDRA_FFI_VERSION_NUMBER: u32 =
    HYDRA_FFI_VERSION_MAJOR * 1_000_000 + HYDRA_FFI_VERSION_MINOR * 1_000 + HYDRA_FFI_VERSION_PATCH;

/// Parse one decimal `CARGO_PKG_VERSION_*` component at compile time.
///
/// `u32::from_str` is not a `const fn`, and these have to be constants: they
/// are what `HYDRA_FFI_VERSION_NUMBER` is built from and what the header's
/// `#define`s carry. A malformed component is a compile error, not a runtime
/// one — cargo only ever hands us digits, so reaching a panic here means the
/// manifest is not what we think it is.
const fn version_component(s: &str) -> u32 {
    let bytes = s.as_bytes();
    assert!(!bytes.is_empty(), "empty version component");
    let mut value: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let digit = bytes[i];
        assert!(
            digit.is_ascii_digit(),
            "non-decimal digit in version component"
        );
        value = value * 10 + (digit - b'0') as u32;
        i += 1;
    }
    value
}

const _: () = assert!(
    HYDRA_FFI_VERSION_MINOR < 1_000 && HYDRA_FFI_VERSION_PATCH < 1_000,
    "version component exceeds 3-digit limit for HYDRA_FFI_VERSION_NUMBER encoding"
);

#[cfg(test)]
mod version_tests {
    /// `include/hydra.h` is a committed artifact regenerated by
    /// `scripts/gen-ffi-header.sh`, so it is the one copy of the version that
    /// can still fall behind `Cargo.toml`. A consumer who compiles against a
    /// stale header and links a newer library sees the mismatch as a failed
    /// `HYDRA_FFI_VERSION` comparison at their end; catch it at ours instead.
    ///
    /// This checks the version macros only. The rest of the header is covered
    /// by `scripts/gen-ffi-header.sh --check`, which needs cbindgen; this test
    /// needs nothing but `cargo test`.
    const HEADER: &str = include_str!("../../../include/hydra.h");

    fn define(name: &str) -> &'static str {
        let needle = format!("#define {name} ");
        let line = HEADER
            .lines()
            .find(|l| l.starts_with(&needle))
            .unwrap_or_else(|| panic!("include/hydra.h has no `#define {name}`"));
        line[needle.len()..].trim()
    }

    #[test]
    fn the_header_version_string_matches_cargo_toml() {
        assert_eq!(
            define("HYDRA_FFI_VERSION"),
            format!("\"{}\"", super::HYDRA_FFI_VERSION),
            "include/hydra.h is out of date with the version in \
             crates/hydra-ffi/Cargo.toml; run scripts/gen-ffi-header.sh"
        );
    }

    #[test]
    fn the_header_numeric_version_matches_cargo_toml() {
        assert_eq!(
            [
                define("HYDRA_FFI_VERSION_MAJOR"),
                define("HYDRA_FFI_VERSION_MINOR"),
                define("HYDRA_FFI_VERSION_PATCH"),
            ],
            [
                super::HYDRA_FFI_VERSION_MAJOR.to_string(),
                super::HYDRA_FFI_VERSION_MINOR.to_string(),
                super::HYDRA_FFI_VERSION_PATCH.to_string(),
            ],
            "the version macros in include/hydra.h are out of date with the \
             version in crates/hydra-ffi/Cargo.toml; run scripts/gen-ffi-header.sh"
        );
    }

    /// A pre-release suffix (`0.2.0-rc1`) is not part of the numeric triple,
    /// so the string and the components agree only on the leading `x.y.z`.
    #[test]
    fn the_numeric_components_recompose_the_version_string() {
        assert_eq!(
            super::HYDRA_FFI_VERSION.split('-').next().unwrap(),
            format!(
                "{}.{}.{}",
                super::HYDRA_FFI_VERSION_MAJOR,
                super::HYDRA_FFI_VERSION_MINOR,
                super::HYDRA_FFI_VERSION_PATCH
            )
        );
    }
}
