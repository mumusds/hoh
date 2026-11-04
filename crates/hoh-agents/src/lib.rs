//! HoH native agent adapters (Codex, Claude Code, Pi): capability probing and the compat manifest
//! (docs/13-adapters.md, REQ-027). Protocol framing and process supervision follow in M1-T02..M1-T05.

pub mod compat;
pub mod probe;

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
