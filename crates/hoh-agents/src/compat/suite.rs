//! The compat suite (docs/adr/ADR-030.md decision 10, "PASS 取决于被测二进制"; REQ-027 behaviour 3, decisions 2 and 3):
//! [`run`] tests an installed agent CLI, offline, and gives the manifest entry it earns. Its cases: `probe` (a whole
//! probe at the start, which must find the registry's entry), the six categories of REQ-027 behaviour 3, each the AND
//! of request conformance (what the adapter's own builders send, checked against the tested binary's schema or help,
//! [`crate::probe::conform`]), frame acceptance (the App Server only: the embedded notifications the adapter judges by,
//! against the tested `ServerNotification.json`) and the replay of its embedded fixture against `expected.json`; and
//! `live`, never run by the product. Any "no" fails a case, an "unknown" with no "no" leaves it NOT_RUN, so evidence
//! that falls short is never a PASS (REQ-027 failure modes 1, 2).

use crate::adapter::Replayed;
use crate::compat::{
    Agent, AgentEntry, SUITE_SET_SHA256, Suite, SuiteCase, SuiteStatus, Transport, fixtures,
};
pub use crate::probe::Truth;
use crate::probe::{self, ProbeEnv, ProbeStop};
use serde_json::Value;

#[cfg(test)]
mod tests;

/// The case that probes the tested launcher at the suite's start.
pub const PROBE_CASE: &str = "probe";

/// The case that stands for a real turn of the tested version, which the product never runs (it costs): its response
/// frames, claude's prompt on stdin and pi's RPC commands cannot be judged offline, and the adapter-live check records
/// them apart (REQ-027 decision 2). Always NOT_RUN, so the manifest says PASS does not rest on it.
pub const LIVE_CASE: &str = "live";

/// The case of the read-only workspace profile (docs/adr/ADR-032.md decision 4, "按档的符合性"): the four read-only
/// variants built by the same builders and checked against the tested binary. Like `live` it comes after the summary
/// and never counts toward `suite.status`; its FAIL or NOT_RUN only keeps `read_only_workspace` from being true.
pub const READ_ONLY_CASE: &str = "read_only";

/// Which check of a case a finding belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Part {
    /// The probe at the suite's start.
    Probe,
    /// What the adapter sends: the App Server's requests, notification and headless answers against the tested
    /// schema, or the argv against the tested help.
    Requests,
    /// The App Server's notifications the adapter judges by, against the tested `ServerNotification.json`.
    Frames,
    /// The embedded fixture through the adapter's own judging against `expected.json`.
    Replay,
    /// A real turn: never run.
    Live,
}

/// One thing a case found, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub part: Part,
    pub truth: Truth,
    pub detail: String,
}

impl Finding {
    pub fn new(part: Part, truth: Truth, detail: impl Into<String>) -> Self {
        Self {
            part,
            truth,
            detail: detail.into(),
        }
    }
}

/// One launch of a planned case: the product's YOLO launch as one variant of the table.
#[derive(Debug, Clone, PartialEq)]
pub struct Planned {
    /// The variant's name in the table (`new`, `new-model`, `resumed`, `resumed-model`).
    pub variant: String,
    pub launch: fixtures::Variant,
    /// What the builders give for it, the suite set's member ([`crate::compat::built`]): for the App Server the
    /// messages and answers [`crate::probe::conform`] checks; for an argv the one built with the suite set's helps,
    /// which `conform` builds again with the tested help (a builder refusing it is non-conformance).
    pub built: Value,
}

/// One case of a plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedCase {
    pub case: String,
    pub launches: Vec<Planned>,
    /// The App Server only: the case's embedded server notifications the adapter judges by
    /// ([`crate::codex::APP_SERVER_NOTIFICATIONS`]), each a whole frame.
    pub frames: Vec<Value>,
}

/// What the suite asks of the tested binary: the six cases of one suite ([`fixtures::SUITES`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub suite: String,
    pub cases: Vec<PlannedCase>,
}

/// The suite whose fixtures and launches test a transport: the App Server's `codex`, exec's `codex-exec`, claude's
/// stream-json and pi's RPC.
pub fn suite_of(transport: Transport) -> &'static str {
    match transport {
        Transport::AppServer => "codex",
        Transport::Exec => "codex-exec",
        Transport::StreamJson => "claude",
        Transport::Rpc => "pi",
    }
}

/// The plan of `suite`: each case's launch variants from the embedded table with what the builders give for them, and
/// for the App Server the embedded notifications the adapter judges by.
pub fn plan(suite: &str) -> Plan {
    let table = fixtures::variants();
    let cases = fixtures::CASES
        .iter()
        .chain([&READ_ONLY_CASE])
        .map(|case| PlannedCase {
            case: (*case).to_owned(),
            launches: table
                .of(suite, case)
                .iter()
                .filter_map(|name| {
                    let launch = table.variant(name)?;
                    Some(Planned {
                        variant: name.clone(),
                        launch,
                        built: crate::compat::built(suite, case, launch),
                    })
                })
                .collect(),
            frames: match suite {
                "codex" => judged_frames(fixtures::case(suite, case).unwrap_or_default()),
                _ => Vec::new(),
            },
        })
        .collect();
    Plan {
        suite: suite.to_owned(),
        cases,
    }
}

/// The notifications among an App Server stdout's lines that the adapter judges by: no `id`, a method of
/// [`crate::codex::APP_SERVER_NOTIFICATIONS`].
fn judged_frames(bytes: &[u8]) -> Vec<Value> {
    bytes
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|frame| {
            frame.get("id").is_none()
                && frame["method"]
                    .as_str()
                    .is_some_and(|m| crate::codex::APP_SERVER_NOTIFICATIONS.contains(&m))
        })
        .collect()
}

/// One case's result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub case: String,
    pub status: SuiteStatus,
    pub findings: Vec<Finding>,
}

impl CaseResult {
    /// The findings of `part`.
    pub fn of(&self, part: Part) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(move |f| f.part == part)
    }
}

/// What a suite run earned: the probe's entry with its `suite` filled (status, this build's suite set, `run_at`, the
/// cases), and each case with its findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteResult {
    pub entry: AgentEntry,
    pub cases: Vec<CaseResult>,
}

impl SuiteResult {
    /// The case named `case`.
    pub fn case(&self, case: &str) -> Option<&CaseResult> {
        self.cases.iter().find(|c| c.case == case)
    }
}

/// A run that earned nothing to publish, and why: the probe at its start failed or found another entry than the
/// registry's (the files changed: the next look starts again), or whoever runs it stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discarded(pub String);

impl std::fmt::Display for Discarded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the compat suite run was discarded: {}", self.0)
    }
}

impl std::error::Error for Discarded {}

/// Run the compat suite on `agent` as the registry's Ready look found it (`expected`), with the probe's commands,
/// limits (each command `env.timeout`, the schema reading its window and cleanup grace) and `stop` (docs/adr/ADR-030.md
/// decision 10). The probe at the start must find `expected`'s identity, version, transport and help and schema hashes,
/// else the run is discarded; so is a run `stop` stopped.
pub fn run(
    agent: Agent,
    expected: &AgentEntry,
    env: &ProbeEnv,
    stop: &ProbeStop,
) -> Result<SuiteResult, Discarded> {
    let env = ProbeEnv {
        cancel: Some(stop.clone()),
        ..env.clone()
    };
    let suite = suite_of(expected.transport);
    let plan = plan(suite);
    let probed = probe::conform(agent, &env, &plan);
    if stop.is_stopped() {
        return Err(Discarded("whoever runs it stopped".to_owned()));
    }
    let (mut entry, conformance) =
        probed.map_err(|e| Discarded(format!("the probe at its start failed: {e}")))?;
    if let Some(why) = differs(expected, &entry) {
        return Err(Discarded(format!(
            "the probe at its start found another {why} than the registry's look"
        )));
    }
    let run_at = hoh_core::time::now_rfc3339();
    let expected_replays = fixtures::expected(suite);
    let mut cases = vec![CaseResult {
        case: PROBE_CASE.to_owned(),
        status: SuiteStatus::Pass,
        findings: vec![Finding::new(
            Part::Probe,
            Truth::Yes,
            "the probe at the start found the registry's identity, version, transport and hashes",
        )],
    }];
    for case in fixtures::CASES {
        let mut findings = conformance.of(case).to_vec();
        // A check with nothing to look at decides nothing.
        if !findings.iter().any(|f| f.part == Part::Requests) {
            findings.push(Finding::new(
                Part::Requests,
                Truth::Unknown,
                "the probe checked no request of this case",
            ));
        }
        if suite == "codex" && !findings.iter().any(|f| f.part == Part::Frames) {
            findings.push(Finding::new(
                Part::Frames,
                Truth::Unknown,
                "the probe accepted no notification of this case",
            ));
        }
        let replayed =
            fixtures::case(suite, case).map(|bytes| replay(suite, bytes, &entry.cli_version));
        findings.push(match (replayed, expected_replays.get(case)) {
            (Some(Some(got)), Some(want)) if got == *want => Finding::new(
                Part::Replay,
                Truth::Yes,
                format!("replayed as expected: {got:?}"),
            ),
            (Some(Some(got)), Some(want)) => Finding::new(
                Part::Replay,
                Truth::No,
                format!("replayed as {got:?}, expected {want:?}"),
            ),
            (got, want) => Finding::new(
                Part::Replay,
                Truth::No,
                format!("no replay to compare (replayed {got:?}, expected {want:?})"),
            ),
        });
        // Request conformance, frame acceptance (the App Server's only) and the replay: their AND.
        cases.push(CaseResult {
            case: (*case).to_owned(),
            status: status_of(Truth::all(findings.iter().map(|f| f.truth))),
            findings,
        });
    }
    let status = status_of(Truth::all(cases.iter().map(|c| match c.status {
        SuiteStatus::Pass => Truth::Yes,
        SuiteStatus::Fail => Truth::No,
        SuiteStatus::NotRun => Truth::Unknown,
    })));
    cases.push(CaseResult {
        case: LIVE_CASE.to_owned(),
        status: SuiteStatus::NotRun,
        findings: vec![Finding::new(
            Part::Live,
            Truth::Unknown,
            "a real turn is never run by the product; the adapter-live check records it apart",
        )],
    });
    // The read-only workspace profile's conformance: each read-only variant built and checked, none skipped. A builder
    // that refuses the tested help is "not supported", a request the tested schema rejects "does not conform"; either
    // keeps read_only_workspace from being true, and neither touches the suite's status (ADR-032 decision 4).
    let mut findings = conformance.of(READ_ONLY_CASE).to_vec();
    if !findings.iter().any(|f| f.part == Part::Requests) {
        findings.push(Finding::new(
            Part::Requests,
            Truth::Unknown,
            "the probe checked no read-only launch",
        ));
    }
    let read_only = status_of(Truth::all(findings.iter().map(|f| f.truth)));
    match read_only {
        SuiteStatus::Pass => {}
        SuiteStatus::Fail => {
            entry
                .capabilities
                .insert("read_only_workspace".to_owned(), Some(false));
        }
        SuiteStatus::NotRun => {
            entry
                .capabilities
                .insert("read_only_workspace".to_owned(), None);
        }
    }
    cases.push(CaseResult {
        case: READ_ONLY_CASE.to_owned(),
        status: read_only,
        findings,
    });
    entry.suite = Suite {
        status,
        fixture_set_sha256: SUITE_SET_SHA256.clone(),
        run_at: Some(Some(run_at)),
        cases: Some(
            cases
                .iter()
                .map(|c| SuiteCase {
                    case: c.case.clone(),
                    status: c.status,
                })
                .collect(),
        ),
    };
    Ok(SuiteResult { entry, cases })
}

/// A case's status from its checks: all yes is PASS, any no FAIL, else NOT_RUN.
fn status_of(truth: Truth) -> SuiteStatus {
    match truth {
        Truth::Yes => SuiteStatus::Pass,
        Truth::No => SuiteStatus::Fail,
        Truth::Unknown => SuiteStatus::NotRun,
    }
}

/// What the suite's probe found differently from the registry's look, if anything: the identity, the version line, the
/// transport, the help or the schema hash.
fn differs(expected: &AgentEntry, probed: &AgentEntry) -> Option<String> {
    if expected.identity() != probed.identity() {
        return Some(format!(
            "identity ({:?}, not {:?})",
            probed.identity(),
            expected.identity()
        ));
    }
    let items: [(&str, String, String); 4] = [
        (
            "version line",
            expected.cli_version.clone(),
            probed.cli_version.clone(),
        ),
        (
            "transport",
            format!("{:?}", expected.transport),
            format!("{:?}", probed.transport),
        ),
        (
            "help_sha256",
            expected.help_sha256.clone(),
            probed.help_sha256.clone(),
        ),
        (
            "schema_sha256",
            format!("{:?}", expected.schema_sha256),
            format!("{:?}", probed.schema_sha256),
        ),
    ];
    items
        .into_iter()
        .find(|(_, want, got)| want != got)
        .map(|(item, want, got)| format!("{item} ({got}, not {want})"))
}

/// `suite`'s adapter replaying `bytes` as the tested `version`; `None` for no suite.
fn replay(suite: &str, bytes: &[u8], version: &str) -> Option<Replayed> {
    match suite {
        "claude" => Some(crate::claude::replay(bytes, version)),
        "codex" => Some(crate::codex::replay(bytes, version)),
        "codex-exec" => Some(crate::codex::replay_exec(bytes, version)),
        "pi" => Some(crate::pi::replay(bytes, version)),
        _ => None,
    }
}
