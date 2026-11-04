//! HoH domain core: specification, orchestration, execution state, verification and
//! persistence ports (docs/04-architecture.md).

pub mod integer;
pub mod nullable;
pub mod persistence;
pub mod protocol;
pub mod schema;
pub mod time;

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
