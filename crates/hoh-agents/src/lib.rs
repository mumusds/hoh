//! Native agent adapters for Codex, Claude Code and Pi, plus protocol framing and process
//! supervision (docs/13-adapters.md). Implementation starts in M1-T02.

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
