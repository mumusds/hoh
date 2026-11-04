//! The product compat manifest (`contracts/compat-manifest.schema.json`, REQ-027): one current entry per agent, written
//! by [`crate::probe::probe`]. A version is usable for a new attempt only while its implementation, help and schema
//! hashes match the entry and `suite.status` is `PASS`.

use hoh_core::nullable;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The native agents HoH drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Codex,
    Claude,
    Pi,
}

impl Agent {
    /// The executable name looked up on PATH.
    pub fn command(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Pi => "pi",
        }
    }
}

/// The native transport an adapter uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    AppServer,
    Exec,
    StreamJson,
    Rpc,
}

/// Outcome of the compat suite for a version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuiteStatus {
    Pass,
    Fail,
    NotRun,
}

/// One compat suite case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuiteCase {
    pub case: String,
    pub status: SuiteStatus,
}

/// The compat suite state of an entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suite {
    pub status: SuiteStatus,
    /// Tree hash of the fixture set the suite runs (or would run) for this version.
    pub fixture_set_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none", with = "nullable")]
    pub run_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cases: Option<Vec<SuiteCase>>,
}

/// One agent's entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    /// First line of `<cli> --version`, verbatim.
    pub cli_version: String,
    pub launcher_path: String,
    pub launcher_sha256: String,
    pub implementation_path: String,
    pub implementation_sha256: String,
    pub transport: Transport,
    /// sha256 of the concatenated relevant `--help` outputs.
    pub help_sha256: String,
    /// Native protocol schema hash; `None` when the CLI has no schema command.
    pub schema_sha256: Option<String>,
    /// Probed capabilities; `None` means unknown (not derivable from the probe, left to the compat suite).
    pub capabilities: BTreeMap<String, Option<bool>>,
    pub suite: Suite,
}

/// The manifest document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatManifest {
    pub schema_version: String,
    pub generated_at: String,
    pub agents: BTreeMap<Agent, AgentEntry>,
}

impl CompatManifest {
    /// A manifest generated now.
    pub fn new(agents: BTreeMap<Agent, AgentEntry>) -> Self {
        Self {
            schema_version: "1.0".to_owned(),
            generated_at: hoh_core::time::now_rfc3339(),
            agents,
        }
    }
}
