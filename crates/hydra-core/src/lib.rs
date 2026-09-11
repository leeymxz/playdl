// Copyright (C) 2026 leeymxz
// SPDX-License-Identifier: MIT OR Apache-2.0
//
// This library is intentionally permissive, not GPL, even though the `hydra`
// binary that ships it is GPL-3.0-or-later: Rust links statically, so copyleft
// here would propagate to every downstream crate. See LICENSING.md.

//! HYDRA scheduler core: multi-source download scheduler state machine
//! with no I/O dependencies.
//!
//! Key principles:
//! * **Dynamic range partitioning**: HTTP byte ranges are tracked client-side,
//!   allowing slow or stalled connections to be repartitioned and assigned to
//!   faster connections dynamically.
//! * **Liveness**: every reachable state has an enabled transition that
//!   decreases remaining work within a bounded window.
//! * **Safety**: byte coverage invariants are strictly verified without gaps
//!   or duplicate allocations.
//!
//! Safety (`coverage_holds`) and liveness (`liveness_holds`) properties are
//! both verified through property-based testing.

#![forbid(unsafe_code)]

pub mod admission;
pub mod detect;
pub mod format;
pub mod intervals;
pub mod plan;
pub mod ramp;
pub mod sched;

pub use admission::{Admission, Admit, DeltaEstimator};
pub use detect::{CollapseDetector, Health};
pub use format::{
    catalogue, describe, detect_format, from_extension as from_extension_pub, known_extensions,
    Category, Detection, Evidence, Format,
};
pub use intervals::{IntervalSet, Range};
pub use plan::{allocate, reserves, SourcePlan};
pub use ramp::{ConcurrencyRamp, Ramp, Verdict};
pub use sched::{
    greedy_concurrency, Action, Capability, LimitReason, Scheduler, Source, Stats, NO_PRIORITY,
    STEAL_QUANTUM,
};
