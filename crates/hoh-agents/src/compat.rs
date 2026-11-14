//! The product compat manifest (`contracts/compat-manifest.schema.json`, REQ-027): one current entry per agent, written
//! by [`crate::probe::probe`]. A version is usable for a new attempt only while its implementation, help and schema
//! hashes match the entry and `suite.status` is `PASS`.

use hoh_core::nullable;
use hoh_core::staffing::{AgentIdentity, Compat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// This build's suite-set digest, recorded as a PASS entry's `suite.fixture_set_sha256` (docs/adr/ADR-030.md decisions
/// 9 and 10, M2T15D-102). The build's real digest (embedded fixtures, the request builders' output, the suite's
/// definition) arrives in slice S6a; until then it is empty, which matches no manifest the locked schema accepts (64
/// hex), so every agent is `Compat::Pending`: the conservative direction.
pub const SUITE_SET_SHA256: &str = "";

/// The native agents HoH drives (the product configuration's, REQ-018).
pub use hoh_core::config::Agent;

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

impl AgentEntry {
    /// The agent's identity: its launcher and implementation by path and hash, compared as a whole
    /// (docs/adr/ADR-030.md decision 2, M2T15D-104).
    pub fn identity(&self) -> AgentIdentity {
        AgentIdentity {
            launcher_path: self.launcher_path.clone(),
            launcher_sha256: self.launcher_sha256.clone(),
            implementation_path: self.implementation_path.clone(),
            implementation_sha256: self.implementation_sha256.clone(),
        }
    }
}

/// Whether `manifest` verifies `agent` as its probe found it (`entry`), this build's suite set being `suite_set`
/// (docs/adr/ADR-030.md decision 9, "一个谓词"; the gate, staffing's facts and doctor share it). The manifest's entry
/// for the agent matches when its whole identity, help and schema hashes and version line equal the probe's and it was
/// recorded under `suite_set`; comparing more than the schema title's three hashes only leaves more versions
/// unverified. No manifest, no entry or no match: pending. A match: its suite's status, a NOT_RUN that ran (`run_at`)
/// being undecided and one only probed pending.
pub fn verdict(
    manifest: Option<&CompatManifest>,
    agent: &str,
    entry: &AgentEntry,
    suite_set: &str,
) -> Compat {
    let Some(manifest) = manifest else {
        return Compat::Pending("there is no compat manifest".to_owned());
    };
    let Some(recorded) = Agent::named(agent).and_then(|a| manifest.agents.get(&a)) else {
        return Compat::Pending(format!("the compat manifest has no entry for {agent}"));
    };
    let differs = |item: &str, recorded: &dyn std::fmt::Debug, probed: &dyn std::fmt::Debug| {
        Compat::Pending(format!(
            "the manifest's {item} is {recorded:?}, the probe found {probed:?}"
        ))
    };
    if recorded.identity() != entry.identity() {
        return differs("identity", &recorded.identity(), &entry.identity());
    }
    if recorded.help_sha256 != entry.help_sha256 {
        return differs("help_sha256", &recorded.help_sha256, &entry.help_sha256);
    }
    if recorded.schema_sha256 != entry.schema_sha256 {
        return differs(
            "schema_sha256",
            &recorded.schema_sha256,
            &entry.schema_sha256,
        );
    }
    if recorded.cli_version != entry.cli_version {
        return differs("cli_version", &recorded.cli_version, &entry.cli_version);
    }
    if recorded.suite.fixture_set_sha256 != suite_set {
        return Compat::Pending(format!(
            "the manifest's suite ran under suite set {:?}, not this build's {suite_set:?}",
            recorded.suite.fixture_set_sha256
        ));
    }
    let cases = |status: SuiteStatus| {
        recorded
            .suite
            .cases
            .iter()
            .flatten()
            .filter(|c| c.status == status)
            .map(|c| c.case.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match (recorded.suite.status, &recorded.suite.run_at) {
        (SuiteStatus::Pass, _) => Compat::Verified,
        (SuiteStatus::Fail, _) => Compat::Failed(format!(
            "the compat suite failed (cases: {})",
            cases(SuiteStatus::Fail)
        )),
        (SuiteStatus::NotRun, Some(Some(run_at))) => Compat::Undecided(format!(
            "the compat suite ran at {run_at} and could not decide (cases: {})",
            cases(SuiteStatus::NotRun)
        )),
        (SuiteStatus::NotRun, _) => {
            Compat::Pending("the compat suite has not run for this identity".to_owned())
        }
    }
}

/// Where a project's compat manifest is: `.hoh/compat-manifest.json` (ADR-015 decision 2).
pub fn manifest_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".hoh").join(MANIFEST_FILE)
}

/// The manifest's file name in the project's `.hoh/`.
const MANIFEST_FILE: &str = "compat-manifest.json";

/// A temporary file a publish writes the whole new manifest to before renaming it over [`manifest_path`]
/// (docs/adr/ADR-030.md decision 10), `tag` making it the writer's own: `.hoh/.compat-manifest.json.<tag>.tmp`. The host
/// deletes those a crash left when it opens the manifest ([`is_manifest_temp`]).
pub fn manifest_temp(root: &std::path::Path, tag: &str) -> std::path::PathBuf {
    root.join(".hoh")
        .join(format!(".{MANIFEST_FILE}.{tag}.tmp"))
}

/// Whether `name` (a file name in `.hoh/`) is one of [`manifest_temp`]'s.
pub fn is_manifest_temp(name: &str) -> bool {
    name.strip_prefix(&format!(".{MANIFEST_FILE}."))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|tag| !tag.is_empty())
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
