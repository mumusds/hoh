//! HoH domain core: specification, orchestration, execution state, verification and
//! persistence ports (docs/04-architecture.md).
//!
//! Only `persistence` may call SQLite (AC-062-05): every other module forbids clippy's `disallowed_methods`, so no
//! attribute inside it, however written or generated, can allow the lint. This file holds module declarations only
//! (scripts/product/persistence_scan.py checks it). Unsafe code is forbidden in the whole crate, so SQLite's C interface
//! cannot be reached either.

#![forbid(unsafe_code)]

#[forbid(clippy::disallowed_methods)]
pub mod bridge;
#[forbid(clippy::disallowed_methods)]
pub mod budget;
// REQ-028: project discovery's pure parts (docs/adr/ADR-039.md).
#[forbid(clippy::disallowed_methods)]
pub mod baseline;
#[forbid(clippy::disallowed_methods)]
pub mod cancel;
#[cfg(hoh_clippy_canary)]
#[forbid(clippy::disallowed_methods)]
mod clippy_canary;
#[forbid(clippy::disallowed_methods)]
pub mod compat_manifest;
#[forbid(clippy::disallowed_methods)]
pub mod config;
#[forbid(clippy::disallowed_methods)]
pub mod context;
#[forbid(clippy::disallowed_methods)]
pub mod dag;
#[forbid(clippy::disallowed_methods)]
pub mod decision;
#[forbid(clippy::disallowed_methods)]
pub mod delivery;
#[forbid(clippy::disallowed_methods)]
pub mod exit;
#[forbid(clippy::disallowed_methods)]
pub mod handoff;
#[forbid(clippy::disallowed_methods)]
pub mod ids;
#[forbid(clippy::disallowed_methods)]
pub mod integer;
/// The names the client and the host share (docs/adr/ADR-031.md, 设计修正 2).
#[forbid(clippy::disallowed_methods)]
pub mod layout;
#[forbid(clippy::disallowed_methods)]
pub mod lifecycle;
#[forbid(clippy::disallowed_methods)]
pub mod nullable;
#[forbid(clippy::disallowed_methods)]
pub mod objects;
pub mod persistence;
#[forbid(clippy::disallowed_methods)]
pub mod progress;
#[forbid(clippy::disallowed_methods)]
pub mod prompt;
#[forbid(clippy::disallowed_methods)]
pub mod prompt_files;
#[forbid(clippy::disallowed_methods)]
pub mod protocol;
/// The Readiness Gate (REQ-031, docs/adr/ADR-035.md).
#[forbid(clippy::disallowed_methods)]
pub mod readiness;
#[forbid(clippy::disallowed_methods)]
pub mod reconcile;
#[forbid(clippy::disallowed_methods)]
pub mod retry;
#[forbid(clippy::disallowed_methods)]
pub mod schedule;
#[forbid(clippy::disallowed_methods)]
pub mod schema;
#[forbid(clippy::disallowed_methods)]
pub mod skills;
/// Who starts a process and how it is owned until it is reaped (docs/adr/ADR-031.md, 设计修正 2).
#[forbid(clippy::disallowed_methods)]
pub mod spawn;
#[forbid(clippy::disallowed_methods)]
pub mod specindex;
// REQ-032: the requirement trace export (docs/adr/ADR-039.md).
#[forbid(clippy::disallowed_methods)]
pub mod staffing;
#[forbid(clippy::disallowed_methods)]
pub mod state_dir;
#[forbid(clippy::disallowed_methods)]
pub mod supervise;
#[forbid(clippy::disallowed_methods)]
pub mod time;
#[forbid(clippy::disallowed_methods)]
pub mod trace;
/// The host's systemd user unit and the installation a running hoh holds (docs/adr/ADR-023.md).
#[forbid(clippy::disallowed_methods)]
pub mod unit;
#[forbid(clippy::disallowed_methods)]
pub mod verification;

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[forbid(clippy::disallowed_methods)]
pub mod resource_bundle;

#[forbid(clippy::disallowed_methods)]
pub mod origin;

#[forbid(clippy::disallowed_methods)]
pub mod templates;

#[forbid(clippy::disallowed_methods)]
pub mod template_files;

#[forbid(clippy::disallowed_methods)]
pub mod mode;
