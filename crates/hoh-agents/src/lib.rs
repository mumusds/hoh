//! HoH native agent adapters (Codex, Claude Code, Pi): capability probing and the compat manifest
//! (docs/13-adapters.md, REQ-027), protocol framing and process supervision (M1-T02). The adapters follow in
//! M1-T03..M1-T05.

// No database access outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

pub mod adapter;
pub mod capabilities;
pub mod claude;
pub mod clock;
pub mod codex;
pub mod compat;
pub mod frame;
pub mod identity;
pub mod known;
pub mod launch;
pub mod login;
pub mod observed;
pub mod pi;
pub mod probe;
pub mod process;

/// The launch profile of a turn (docs/adr/ADR-032.md decision 2): `Yolo` is HoH's YOLO runtime profile (CR-002),
/// every turn's but the Commander's; `ReadOnly` the Commander's read-only workspace profile (decision 3, CR-024);
/// `Plain` neither bypass nor restriction (only `hoh doctor --live`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Profile {
    Yolo,
    ReadOnly,
    Plain,
}

/// Crate version, reported by the `hoh` binary for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
