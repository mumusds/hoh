//! The HoH host as a library (docs/adr/ADR-016.md): the `hoh` binary runs it, and integration tests start it in
//! process.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

pub mod gate;
pub mod host;
pub mod unit;
