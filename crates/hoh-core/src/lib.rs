//! HoH domain core: specification, orchestration, execution state, verification and
//! persistence ports (docs/04-architecture.md). Implementation starts in M1-T01.

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
