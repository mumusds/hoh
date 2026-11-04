//! HoH native agent adapters (Codex, Claude Code, Pi): capability probing and the compat manifest
//! (docs/13-adapters.md, REQ-027), protocol framing and process supervision (M1-T02). The adapters follow in
//! M1-T03..M1-T05.

pub mod compat;
pub mod frame;
pub mod probe;
pub mod process;

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
