//! The product compat manifest (`contracts/compat-manifest.schema.json`, REQ-027): one current entry per agent, written
//! by [`crate::probe::probe`]. A version is usable for a new attempt only while its implementation, help and schema
//! hashes match the entry and `suite.status` is `PASS`.

use crate::codex::app_server::requests;
use hoh_core::nullable;
use hoh_core::staffing::{AgentIdentity, Compat};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::LazyLock;

mod definition;
pub mod fixtures;
pub mod suite;

#[cfg(test)]
mod tests;

/// This build's suite set, recorded as a PASS entry's `suite.fixture_set_sha256` (docs/adr/ADR-030.md decisions 9 and
/// 10, "套件集摘要", M2T15D-52, -102): [`suite_set`] of the embedded fixtures ([`fixture_members`]), what the request
/// builders give for every launch variant ([`request_members`]) and the suite's definition ([`DEFINITION_SHA256`]),
/// computed once on first use. A PASS recorded under any other is pending for this build.
pub static SUITE_SET_SHA256: LazyLock<String> =
    LazyLock::new(|| suite_set(&fixture_members(), &request_members(), &DEFINITION_SHA256));

/// The suite's definition digest (docs/adr/ADR-030.md decision 10), computed once on first use from the files this
/// build embeds ([`definition_files`]: this crate's sources, the builders, the validator, the replays and the embedded
/// fixtures among them; its Cargo.toml; the digest of the workspace's Cargo.lock; the frame registries the replays
/// judge by): sha256 over the sorted lines `<path in the workspace> <sha256>\n` (the probe's tree digest), so changing
/// how requests are built, checked or replayed changes the key.
pub static DEFINITION_SHA256: LazyLock<String> = LazyLock::new(|| {
    crate::probe::members_sha256(
        definition_files()
            .into_iter()
            .map(|(name, bytes)| {
                let digest = Sha256::digest(bytes)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                (name, digest)
            })
            .collect(),
    )
});

/// The files [`DEFINITION_SHA256`] covers, by their path in the workspace, sorted.
pub static DEFINITION_FILES: LazyLock<Vec<String>> = LazyLock::new(|| {
    definition_files()
        .into_iter()
        .map(|(name, _)| name)
        .collect()
});

/// Every file of the suite's definition with the bytes this build embeds, by its path in the workspace, sorted:
/// `definition::FILES` and the embedded fixtures ([`fixtures::FILES`] under `crates/hoh-agents/src/compat/fixtures/`).
pub fn definition_files() -> Vec<(String, &'static [u8])> {
    let mut files: Vec<(String, &'static [u8])> = definition::FILES
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), *bytes))
        .chain(fixtures::FILES.iter().map(|(name, bytes)| {
            (
                format!("crates/hoh-agents/src/compat/fixtures/{name}"),
                *bytes,
            )
        }))
        .collect();
    files.sort_unstable();
    files
}

/// The fixed inputs the request builders are given for the suite set, and the launch a replay judges
/// (docs/adr/ADR-030.md decision 10): the working directory, the prompt, the model, the session (claude's session,
/// codex's thread, pi's session id), the turn, a steer's text and pi's session directory.
pub const SUITE_CWD: &str = "/hoh-suite";
pub const SUITE_PROMPT: &str = "Reply with exactly HOH_SUITE_OK and nothing else.";
pub const SUITE_MODEL: &str = "hoh-suite-model";
pub const SUITE_SESSION: &str = "hoh-suite-session";
pub const SUITE_TURN: &str = "hoh-suite-turn";
pub const SUITE_STEER: &str = "Also say HOH_SUITE_STEER.";
pub const SUITE_PI_SESSION_DIR: &str = "/hoh-suite/pi-session";

/// The server requests of codex's `tool` case, which the adapter answers headless ([`crate::codex::headless_answer`]),
/// each with the file of the App Server's schema export that describes its answer's `result` (0.166.0's names; an
/// export without it cannot tell, docs/adr/ADR-030.md decision 10).
pub const TOOL_SERVER_REQUESTS: [(&str, &str); 2] = [
    (
        "item/commandExecution/requestApproval",
        "CommandExecutionRequestApprovalResponse.json",
    ),
    (
        "item/fileChange/requestApproval",
        "FileChangeRequestApprovalResponse.json",
    ),
];

/// The help texts the argv builders are given for the suite set: each declares every option its builder may pass, so
/// the argv is the one the product builds wherever the tested help declares them (the suite checks the tested help).
const CLAUDE_HELP: &str = "Usage: claude [options]\n  -p, --print\n  --output-format <format>  text, json, stream-json\n  --verbose\n  --include-partial-messages\n  --resume [sessionId]\n  --model <model>\n  --dangerously-skip-permissions\n  --tools <tools...>\n  --allowedTools <tools...>\n  --permission-mode <mode>  default, dontAsk\n  --strict-mcp-config\n";
const CODEX_EXEC_HELP: &str = "Usage: codex exec [OPTIONS] [PROMPT]\n      --json\n      --dangerously-bypass-approvals-and-sandbox\n  -m, --model <MODEL>\n";
const PI_HELP: &str = "Usage: pi [options]\n  --mode <mode>  text, json, rpc\n  --session-dir <dir>\n  --session-id <id>\n  --provider <name>\n  --model <id>\n  --tools <tools>\n  --no-mcp\n  --no-extensions\n\nBuilt-in Tool Names:\n  read - Read\n  grep - Grep\n  find - Find\n  ls - List\n";

/// Every embedded fixture file as a member of the suite set: `fixtures/<path>` with its bytes.
pub fn fixture_members() -> Vec<(String, Vec<u8>)> {
    fixtures::FILES
        .iter()
        .map(|(name, bytes)| (format!("fixtures/{name}"), bytes.to_vec()))
        .collect()
}

/// What the request builders give for every launch variant of every suite's cases, as members of the suite set:
/// `requests/<suite>/<case>/<variant>` with the canonical JSON ([`canonical`]) of the App Server's messages and
/// headless answers (codex), or the argv (claude, codex exec, pi), built from the fixed inputs (docs/adr/ADR-030.md
/// decision 10). A builder refusing a variant gives `{"refused": why}`.
pub fn request_members() -> Vec<(String, Vec<u8>)> {
    let table = fixtures::variants();
    let mut members = Vec::new();
    for suite in fixtures::SUITES {
        for case in fixtures::CASES {
            for name in table.of(suite, case) {
                let Some(variant) = table.variant(name) else {
                    continue;
                };
                let built = built(suite, case, variant);
                members.push((format!("requests/{suite}/{case}/{name}"), canonical(&built)));
            }
        }
    }
    members
}

/// What `suite`'s builders give for `case` launched as `variant`, the argv builders given the suite set's helps: the App
/// Server's `{"messages": […], "answers": {method: {"result"|"error": …}}}`, an argv's `{"argv": […]}`, or
/// `{"refused": why}`.
pub fn built(suite: &str, case: &str, variant: fixtures::Variant) -> Value {
    let model = variant.model.then_some(SUITE_MODEL);
    match suite {
        "codex" => {
            let thread = if variant.resume {
                requests::thread_resume(1, variant.profile, model, SUITE_SESSION)
            } else {
                requests::thread_start(1, variant.profile, model)
            };
            let mut messages = vec![
                requests::initialize(0),
                requests::initialized(),
                thread,
                requests::turn_start(2, Some(SUITE_SESSION), SUITE_PROMPT),
            ];
            let mut answers = serde_json::Map::new();
            match case {
                "cancel" => messages.push(requests::turn_interrupt(3, SUITE_SESSION, SUITE_TURN)),
                "tool" => {
                    messages.push(requests::turn_steer(
                        3,
                        Some(SUITE_SESSION),
                        Some(SUITE_TURN),
                        SUITE_STEER,
                    ));
                    for (method, _) in TOOL_SERVER_REQUESTS {
                        let answer = match crate::codex::headless_answer(method) {
                            Ok(result) => json!({"result": result}),
                            Err((code, message)) => {
                                json!({"error": {"code": code, "message": message}})
                            }
                        };
                        answers.insert(method.to_owned(), answer);
                    }
                }
                _ => {}
            }
            json!({"messages": messages, "answers": answers})
        }
        "codex-exec" | "claude" | "pi" => {
            let help = match suite {
                "claude" => CLAUDE_HELP,
                "pi" => PI_HELP,
                _ => CODEX_EXEC_HELP,
            };
            match argv(suite, variant, help, Some(CODEX_EXEC_HELP)) {
                Ok(argv) => json!({"argv": argv}),
                Err(why) => json!({"refused": why}),
            }
        }
        other => json!({"refused": format!("no suite {other}")}),
    }
}

/// The argv `suite`'s builder (`claude::argv`, `codex::exec_args`, `pi::args`) gives for the product's YOLO launch as
/// `variant` with the suite's fixed inputs, given `help` (claude's or pi's `--help`, `codex exec --help`) and, for codex
/// exec, `resume_help` (`codex exec resume --help`, `None` when it could not be read), or why it refuses
/// (docs/adr/ADR-030.md decision 10: the suite set builds with its own helps, the suite with the tested binary's).
pub fn argv(
    suite: &str,
    variant: fixtures::Variant,
    help: &str,
    resume_help: Option<&str>,
) -> Result<Vec<String>, String> {
    let model = variant.model.then(|| SUITE_MODEL.to_owned());
    let resume = variant.resume.then(|| SUITE_SESSION.to_owned());
    match suite {
        "codex-exec" => crate::codex::exec_args(&crate::codex::Launch {
            program: PathBuf::from("codex"),
            app_server_help: None,
            exec_help: help.to_owned(),
            exec_resume_help: resume_help.map(str::to_owned),
            cwd: PathBuf::from(SUITE_CWD),
            profile: variant.profile,
            resume,
            model,
            known: Default::default(),
            requests: Default::default(),
            client_notifications: Default::default(),
            capabilities: crate::capabilities::Probed::new(),
        })
        .map_err(|crate::codex::Blocked(why)| why),
        "claude" => crate::claude::argv(&crate::claude::Launch {
            program: PathBuf::from("claude"),
            help: help.to_owned(),
            cwd: PathBuf::from(SUITE_CWD),
            profile: variant.profile,
            partial: false,
            resume,
            model,
            root: false,
            capabilities: crate::capabilities::Probed::new(),
        })
        .map(|argv| argv.args)
        .map_err(|crate::claude::Blocked(why)| why),
        "pi" => crate::pi::args(&crate::pi::Launch {
            program: PathBuf::from("pi"),
            help: help.to_owned(),
            cwd: PathBuf::from(SUITE_CWD),
            session_dir: PathBuf::from(SUITE_PI_SESSION_DIR),
            session_id: SUITE_SESSION.to_owned(),
            resume: variant.resume,
            provider: None,
            model,
            settled_event: true,
            profile: variant.profile,
            capabilities: crate::capabilities::Probed::new(),
        })
        .map_err(|crate::pi::Blocked(why)| why),
        other => Err(format!("no argv suite {other}")),
    }
}

/// The canonical JSON of `value`: compact, every object's keys in sorted order.
pub fn canonical(value: &Value) -> Vec<u8> {
    fn sorted(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let ordered: BTreeMap<&String, Value> =
                    map.iter().map(|(k, v)| (k, sorted(v))).collect();
                let mut out = serde_json::Map::new();
                for (k, v) in ordered {
                    out.insert(k.clone(), v);
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(sorted).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_vec(&sorted(value)).unwrap_or_default()
}

/// The suite set of `fixtures` and `requests` (members by name, with their bytes) under the suite's `definition`
/// digest (docs/adr/ADR-030.md decision 10): the tree digest ([`crate::probe::members_sha256`], the probe's algorithm)
/// of every member by (name, sha256 of its bytes) and the member `definition` with `definition` as its digest. Pure.
pub fn suite_set(
    fixtures: &[(String, Vec<u8>)],
    requests: &[(String, Vec<u8>)],
    definition: &str,
) -> String {
    let hex = |bytes: &[u8]| -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let members = fixtures
        .iter()
        .chain(requests)
        .map(|(name, bytes)| (name.clone(), hex(bytes)))
        .chain([("definition".to_owned(), definition.to_owned())])
        .collect();
    crate::probe::members_sha256(members)
}

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
pub use hoh_core::compat_manifest::path as manifest_path;

/// A temporary file a publication writes the whole new manifest to before renaming it over [`manifest_path`]
/// (docs/adr/ADR-030.md decision 10): `.hoh/.compat-manifest.json.<tag>.tmp`.
pub use hoh_core::compat_manifest::temp as manifest_temp;

/// Whether `name` (a file name in `.hoh/`) is one of [`manifest_temp`]'s.
pub use hoh_core::compat_manifest::is_temp as is_manifest_temp;

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

/// The status of the read_only case (docs/adr/ADR-032.md decision 4, item 3) of `manifest`'s entry for `agent`, while
/// that entry verifies `entry` under `suite_set` (`compat::verdict`); `None` otherwise, or when it has no such case.
/// The host's session record and doctor's `read_only` item read it.
pub fn read_only_case(
    manifest: Option<&CompatManifest>,
    agent: Agent,
    entry: &AgentEntry,
    suite_set: &str,
) -> Option<SuiteStatus> {
    if verdict(manifest, agent.as_str(), entry, suite_set) != Compat::Verified {
        return None;
    }
    manifest?
        .agents
        .get(&agent)?
        .suite
        .cases
        .as_ref()?
        .iter()
        .find(|c| c.case == suite::READ_ONLY_CASE)
        .map(|c| c.status)
}
