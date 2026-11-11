//! HoH domain core: specification, orchestration, execution state, verification and
//! persistence ports (docs/04-architecture.md).
//!
//! Only `persistence` may call SQLite (AC-062-05): every other module forbids clippy's `disallowed_methods`, so no
//! attribute inside it, however written or generated, can allow the lint. This file holds module declarations only
//! (scripts/product/persistence_scan.py checks it). Unsafe code is forbidden in the whole crate, so SQLite's C interface
//! cannot be reached either.

#![forbid(unsafe_code)]

#[forbid(clippy::disallowed_methods)]
pub mod budget;
#[forbid(clippy::disallowed_methods)]
pub mod cancel;
#[cfg(hoh_clippy_canary)]
#[forbid(clippy::disallowed_methods)]
mod clippy_canary;
#[forbid(clippy::disallowed_methods)]
pub mod config;
#[forbid(clippy::disallowed_methods)]
pub mod dag;
#[forbid(clippy::disallowed_methods)]
pub mod decision;
#[forbid(clippy::disallowed_methods)]
pub mod delivery;
#[forbid(clippy::disallowed_methods)]
pub mod handoff;
#[forbid(clippy::disallowed_methods)]
pub mod ids;
#[forbid(clippy::disallowed_methods)]
pub mod integer;
#[forbid(clippy::disallowed_methods)]
pub mod lifecycle;
#[forbid(clippy::disallowed_methods)]
pub mod nullable;
pub mod persistence;
#[forbid(clippy::disallowed_methods)]
pub mod protocol;
#[forbid(clippy::disallowed_methods)]
pub mod reconcile;
#[forbid(clippy::disallowed_methods)]
pub mod retry;
#[forbid(clippy::disallowed_methods)]
pub mod schedule;
#[forbid(clippy::disallowed_methods)]
pub mod schema;
#[forbid(clippy::disallowed_methods)]
pub mod supervise;
#[forbid(clippy::disallowed_methods)]
pub mod time;

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
