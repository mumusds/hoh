//! What the native adapters share (docs/13-adapters.md): the attempt a turn runs as, its errors, and the moves of
//! that attempt through the store (every end through its cancel latch, M1T03R-10).

use hoh_core::lifecycle::{AttemptState, InputState, Lifecycle};
use hoh_core::persistence::{
    AttemptFacts, AttemptMove, CancelLatch, InputMove, PersistError, Store,
};
use hoh_core::staffing::{AgentIdentity, FileStamp, Stamp};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

/// The attempt a turn runs as, and its session.
#[derive(Debug, Clone)]
pub struct AttemptRef {
    pub attempt_id: String,
    /// HoH's session id (`sessions.session_id`).
    pub session_id: String,
    pub role: String,
    pub agent_version: String,
}

/// The start gate of a turn in the runner's mode (docs/adr/ADR-022.md decision 13): `program` with `args`, then the
/// agent's own program and argv. It reads one line from stdin and, if the line is `go`, execs the agent in its place
/// (same pid, group, environment and cgroup); otherwise it exits and the agent never runs. `env` is added to the
/// agent's environment (its attempt's marker).
///
/// With `expect` and `status` (the product's every turn, docs/adr/ADR-030.md decision 9) the gate is opened by
/// `go <key>` and, before it execs, verifies that the implementation it is about to run is the one `expect` names,
/// recording its decision in `status.path`; without them (a gate tests drive directly) it opens on `go` unchecked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateCommand {
    pub program: std::path::PathBuf,
    pub args: Vec<std::ffi::OsString>,
    pub env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    pub expect: Option<Expect>,
    pub status: Option<GateStatus>,
}

impl GateCommand {
    /// The gate's arguments before the agent's program and argv: `args`, then, with `expect` and `status`,
    /// `--expect <agent> <launcher_path> <launcher_sha256> <implementation_path> <implementation_sha256> --stamp
    /// <launcher's dev:ino:ctime_sec:ctime_nsec> <the implementation's> --plan <fd|path> --status <path> --`
    /// (docs/adr/ADR-030.md decision 9); the key is never among them.
    pub fn argv(&self) -> Vec<std::ffi::OsString> {
        let mut argv = self.args.clone();
        if let Some(expect) = &self.expect {
            let identity = &expect.identity;
            argv.push("--expect".into());
            argv.push(expect.agent.as_str().into());
            for part in [
                &identity.launcher_path,
                &identity.launcher_sha256,
                &identity.implementation_path,
                &identity.implementation_sha256,
            ] {
                argv.push(part.into());
            }
            argv.push("--stamp".into());
            argv.push(stamp_text(&expect.stamp.launcher).into());
            argv.push(stamp_text(&expect.stamp.implementation).into());
            argv.push("--plan".into());
            argv.push(expect.plan.as_str().into());
        }
        if let Some(status) = &self.status {
            argv.push("--status".into());
            argv.push(status.path.clone().into_os_string());
        }
        if self.expect.is_some() || self.status.is_some() {
            argv.push("--".into());
        }
        argv
    }

    /// The line that opens the gate: `go`, or `go <key>` when it records to `status` (docs/adr/ADR-030.md decision 9:
    /// the key reaches the gate only through it, on the stdin it reads one byte at a time).
    pub(crate) fn go(&self) -> Vec<u8> {
        match &self.status {
            Some(status) => format!("go {}\n", status.key).into_bytes(),
            None => b"go\n".to_vec(),
        }
    }
}

/// A file's stamp as the gate's command line gives it: `dev:ino:ctime_sec:ctime_nsec`.
pub fn stamp_text(stamp: &FileStamp) -> String {
    format!(
        "{}:{}:{}:{}",
        stamp.dev, stamp.ino, stamp.ctime_sec, stamp.ctime_nsec
    )
}

/// The stamp `text` gives as [`stamp_text`] writes it.
pub fn stamp_of_text(text: &str) -> Option<FileStamp> {
    let mut parts = text.split(':');
    let stamp = FileStamp {
        dev: parts.next()?.parse().ok()?,
        ino: parts.next()?.parse().ok()?,
        ctime_sec: parts.next()?.parse().ok()?,
        ctime_nsec: parts.next()?.parse().ok()?,
    };
    parts.next().is_none().then_some(stamp)
}

/// What a turn's gate verifies before it execs, all from the fact F its prepare used (docs/adr/ADR-030.md decision 9):
/// the agent, its identity, the stamps of its launcher and implementation as F's look read them, and how the
/// implementation is to be executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expect {
    pub agent: crate::compat::Agent,
    pub identity: AgentIdentity,
    pub stamp: Stamp,
    pub plan: crate::probe::Exec,
}

/// Where a turn's gate records its decision (`gate.jsonl` in the attempt's log directory) and the key, drawn for the
/// attempt and kept in HoH's memory only, that marks a refusal there as the gate's own (docs/adr/ADR-030.md decision
/// 9, "闸门的结果通道"). Its `Debug` never shows the key.
#[derive(Clone, PartialEq, Eq)]
pub struct GateStatus {
    pub path: std::path::PathBuf,
    pub key: String,
}

impl std::fmt::Debug for GateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateStatus")
            .field("path", &self.path)
            .field("key", &"<withheld>")
            .finish()
    }
}

/// The most of a turn's `gate.jsonl` that is read for its gate's refusal: the gate writes one line before it exits or
/// execs; whatever an agent that ran appends after it is no refusal of its gate's (it has no key).
const GATE_RECORD_LIMIT: u64 = 1 << 20;

/// A refusal its gate recorded with the attempt's key (docs/adr/ADR-030.md decision 9): what did not match, whether
/// only the stamps did not, what was expected and what was seen. Made only by [`gate_refusal`].
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    item: String,
    stamp_only: bool,
    expected: serde_json::Value,
    seen: serde_json::Value,
}

impl Refusal {
    pub fn item(&self) -> &str {
        &self.item
    }

    pub fn stamp_only(&self) -> bool {
        self.stamp_only
    }

    pub fn expected(&self) -> &serde_json::Value {
        &self.expected
    }

    pub fn seen(&self) -> &serde_json::Value {
        &self.seen
    }
}

/// The refusal the gate recorded in `status.path` with `status.key` (docs/adr/ADR-030.md decision 9, "适配器读"): the
/// last line whose `outcome` is `refused` and whose `key` equals the attempt's; every other line (`verified`,
/// `exec_failed`, another key, none, one that does not parse) is ignored, and so is anything past the first
/// [`GATE_RECORD_LIMIT`] bytes. `None` when there is no such line or no file.
pub fn gate_refusal(status: &GateStatus) -> Option<Refusal> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    std::fs::File::open(&status.path)
        .ok()?
        .take(GATE_RECORD_LIMIT)
        .read_to_end(&mut bytes)
        .ok()?;
    bytes
        .split(|b| *b == b'\n')
        .rev()
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .find(|record| {
            record["outcome"] == "refused" && record["key"].as_str() == Some(status.key.as_str())
        })
        .map(|record| Refusal {
            item: record["item"].as_str().unwrap_or_default().to_owned(),
            stamp_only: record["stamp_only"].as_bool().unwrap_or(false),
            expected: record["expected"].clone(),
            seen: record["seen"].clone(),
        })
}

/// Where a typed COMPAT_UNVERIFIED of a turn came from (docs/adr/ADR-030.md decision 6): its gate's keyed refusal, or
/// the re-check before its first input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Gate,
    BeforeInput,
}

/// HoH's own finding that a turn's agent is not the one verified (docs/adr/ADR-030.md decision 6, K9): made only in
/// this module, from its gate's keyed refusal ([`Facts::gate_refused`]) or its re-check before the first input
/// ([`Facts::before_input`]), so no failure text of an agent's is ever taken for one. `stamp_only`: only the files'
/// stamps differed, the identity did not (a transient failure).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatRefusal {
    origin: Origin,
    stamp_only: bool,
}

impl CompatRefusal {
    pub fn origin(&self) -> Origin {
        self.origin
    }

    pub fn stamp_only(&self) -> bool {
        self.stamp_only
    }
}

/// The facts of a turn whose agent ended without any frame and whose gate recorded a refusal with this turn's key
/// ([`gate_refusal`]); `None` for a gate without a status, a turn not seen to end, one that wrote anything on stdout,
/// or no such refusal: its end is then judged by the adapter's own rules. `recorded` is what the turn recorded so far.
pub(crate) fn refused_at_gate(
    gate: &GateCommand,
    raw_log_path: &str,
    report: Option<&crate::process::ExitReport>,
    recorded: &[Recorded],
) -> Option<Facts> {
    let status = gate.status.as_ref()?;
    let report = report.filter(|r| r.stdout.bytes == 0)?;
    let refusal = gate_refusal(status)?;
    let mut facts = Facts::gate_refused(raw_log_path.to_owned(), Some(report), refusal);
    let mut all = recorded.to_vec();
    all.append(&mut facts.recorded);
    facts.recorded = all;
    Some(facts)
}

/// How often a running turn's agent files are looked at again (docs/adr/ADR-030.md decision 9, "turn 中的再核对"): the
/// runner's tick.
pub const RECHECK_EVERY: std::time::Duration = std::time::Duration::from_millis(200);

/// The failure of a turn whose re-check before its first input found its agent's files changed: it sends no input and
/// closes; [`Facts::before_input`] types it.
pub(crate) const COMPAT_UNVERIFIED: &str = "COMPAT_UNVERIFIED";

/// The diagnostic of a re-check before the first input that found the agent's files changed: no input was sent.
pub const CHANGED_BEFORE_INPUT: &str = "IMPLEMENTATION_CHANGED_BEFORE_INPUT";

/// The diagnostic of a re-check while the turn ran that found the agent's files changed: recorded only.
pub const CHANGED_IN_TURN: &str = "IMPLEMENTATION_CHANGED_IN_TURN";

/// One of an agent's two files a re-check stats: its launcher or its implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentFile {
    Launcher,
    Implementation,
}

impl AgentFile {
    /// Its name in a diagnostic (`file`).
    pub fn as_str(self) -> &'static str {
        match self {
            AgentFile::Launcher => "launcher",
            AgentFile::Implementation => "implementation",
        }
    }
}

/// What a re-check found changed (docs/adr/ADR-030.md decision 9, "首个输入之前的再核对"): the first of the agent's
/// files, the implementation first, whose stamp is not the expected one, that stamp, the one seen now (`None`: the file
/// cannot be read), and whether the agent's identity, derived again the gate's way, is still the expected one (only
/// the stamps changed: a transient failure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changed {
    file: AgentFile,
    expected: FileStamp,
    seen: Option<FileStamp>,
    stamp_only: bool,
}

impl Changed {
    pub fn file(&self) -> AgentFile {
        self.file
    }

    pub fn expected(&self) -> FileStamp {
        self.expected
    }

    pub fn seen(&self) -> Option<FileStamp> {
        self.seen
    }

    pub fn stamp_only(&self) -> bool {
        self.stamp_only
    }

    /// The `adapter.diagnostic` of a re-check before the first input that found it (key, payload): no input was sent.
    pub(crate) fn event(&self) -> (String, serde_json::Value) {
        (
            "implementation-changed-before-input".to_owned(),
            json!({"reason": CHANGED_BEFORE_INPUT, "file": self.file.as_str(), "expected": self.expected,
                   "seen": seen_json(self.seen), "stamp_only": self.stamp_only}),
        )
    }
}

/// A stamp seen now as a diagnostic gives it: `missing` when the file cannot be read.
fn seen_json(seen: Option<FileStamp>) -> serde_json::Value {
    seen.map_or_else(|| json!("missing"), |s| json!(s))
}

/// What [`recheck`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recheck {
    /// Both files' stamps are the expected ones.
    Same,
    Changed(Changed),
}

impl Recheck {
    /// What changed, if anything did.
    pub fn changed(self) -> Option<Changed> {
        match self {
            Recheck::Same => None,
            Recheck::Changed(changed) => Some(changed),
        }
    }
}

/// Whether the launcher and the implementation `expect` names are still the files its gate verified
/// (docs/adr/ADR-030.md decision 9, "首个输入之前的再核对", M2T15D-201): each path `stat`ed (following links) and its
/// (dev, ino, ctime) compared with `expect.stamp`, in memory; no record of any file's is read. On a difference the
/// agent's identity is derived again as its gate derives it ([`identity_now`]): the same as `expect.identity` → only
/// the stamps changed (`stamp_only`); another one, or none that can be derived → not.
pub fn recheck(expect: &Expect) -> Recheck {
    let [launcher, implementation] = stamps_now(expect);
    let differs = [
        (
            AgentFile::Implementation,
            expect.stamp.implementation,
            implementation,
        ),
        (AgentFile::Launcher, expect.stamp.launcher, launcher),
    ]
    .into_iter()
    .find(|(_, expected, seen)| *seen != Some(*expected));
    match differs {
        None => Recheck::Same,
        Some((file, expected, seen)) => Recheck::Changed(Changed {
            file,
            expected,
            seen,
            stamp_only: identity_now(expect).as_ref() == Some(&expect.identity),
        }),
    }
}

/// The stamps of the launcher and the implementation `expect` names, as their paths are now (`stat`, following links;
/// metadata only): `None` for one that cannot be read.
pub fn stamps_now(expect: &Expect) -> [Option<FileStamp>; 2] {
    let stat = |path: &str| std::fs::metadata(path).ok().map(|m| file_stamp(&m));
    [
        stat(&expect.identity.launcher_path),
        stat(&expect.identity.implementation_path),
    ]
}

/// A file's stamp from its metadata: (dev, ino, ctime).
fn file_stamp(m: &std::fs::Metadata) -> FileStamp {
    use std::os::unix::fs::MetadataExt as _;
    FileStamp {
        dev: m.dev(),
        ino: m.ino(),
        ctime_sec: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    }
}

/// The identity of the agent `expect` names as its files are now, derived the gate's way: the implementation resolved
/// again from the launcher as the probe resolves it ([`crate::probe::resolve_implementation`]), the launcher and the
/// implementation each opened and hashed through its descriptor, its stamp (`fstat`) the same before and after; `None`
/// when any of that fails or a file changed while it was hashed.
pub fn identity_now(expect: &Expect) -> Option<AgentIdentity> {
    let launcher = Path::new(&expect.identity.launcher_path);
    let implementation = crate::probe::resolve_implementation(expect.agent, launcher).ok()?;
    // Hashed through the descriptor it was opened as, which is the file whose stamp is read before and after.
    let hashed = |path: &Path| -> Option<String> {
        let file = std::fs::File::open(path).ok()?;
        let before = file.metadata().ok().map(|m| file_stamp(&m))?;
        let sha256 = sha256_read(&file).ok()?;
        let after = file.metadata().ok().map(|m| file_stamp(&m))?;
        (before == after).then_some(sha256)
    };
    Some(AgentIdentity {
        launcher_path: expect.identity.launcher_path.clone(),
        launcher_sha256: hashed(launcher)?,
        implementation_path: implementation.to_str()?.to_owned(),
        implementation_sha256: hashed(&implementation)?,
    })
}

/// A test's look at each diagnostic a turn of the runner's mode collects, called with the attempt's id and the
/// diagnostic's payload as it is collected, before the turn ends (docs/adr/ADR-030.md decision 9, M2T15D-401: a
/// turn's diagnostics are in memory until then); the product has none.
#[derive(Clone)]
pub struct Observe(pub Seen);

/// Called with an attempt's id and a diagnostic of its turn's.
pub type Seen = std::sync::Arc<dyn Fn(&str, &serde_json::Value) + Send + Sync>;

impl std::fmt::Debug for Observe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Observe")
    }
}

/// Who is shown a turn's diagnostics as they are collected: the attempt and its test's [`Observe`], if any.
#[derive(Debug, Clone, Default)]
pub(crate) struct Observer {
    attempt_id: String,
    observe: Option<Observe>,
}

impl Observer {
    pub(crate) fn new(attempt_id: &str, observe: Option<Observe>) -> Observer {
        Observer {
            attempt_id: attempt_id.to_owned(),
            observe,
        }
    }

    /// Show an event a turn collects to the observer if it is a diagnostic.
    pub(crate) fn collected(&self, kind: &str, payload: &serde_json::Value) {
        if kind == "adapter.diagnostic"
            && let Some(Observe(observe)) = &self.observe
        {
            observe(&self.attempt_id, payload);
        }
    }

    /// The facts of a turn its gate refused ([`refused_at_gate`]), their GATE_REFUSED diagnostic, collected as the
    /// turn's end is judged, shown to the observer.
    pub(crate) fn refused(&self, facts: Facts) -> Facts {
        for recorded in &facts.recorded {
            if let Recorded::Event { key, kind, payload } = recorded
                && key == "gate-refused"
            {
                self.collected(kind, payload);
            }
        }
        facts
    }
}

/// A turn's watch over its agent's files (docs/adr/ADR-030.md decision 9): the gate's expectation, re-checked before
/// the first input where the protocol has HoH speak first ([`recheck`] of [`Watch::expect`]) and every
/// [`RECHECK_EVERY`] while the turn runs, from its gate's opening to its process's end ([`Watch::due`],
/// [`Watch::check`]). The re-check while it runs only records: a file seen changed is an
/// `adapter.diagnostic{IMPLEMENTATION_CHANGED_IN_TURN}`, once per new stamp it is seen with; the turn, its outcome and
/// its class are left as they are (REQ-027 behaviour 4, AC-027-04).
#[derive(Debug)]
pub(crate) struct Watch {
    expect: Option<Expect>,
    /// What the launcher and the implementation were last seen as: `expect`'s stamps at first.
    seen: [Option<FileStamp>; 2],
    due: tokio::time::Instant,
    recorded: usize,
}

impl Watch {
    /// The watch of a turn whose gate expects `expect` (none: nothing is re-checked); its first re-check due
    /// [`RECHECK_EVERY`] from now.
    pub(crate) fn new(expect: Option<Expect>) -> Watch {
        let seen = expect.as_ref().map_or([None, None], |e| {
            [Some(e.stamp.launcher), Some(e.stamp.implementation)]
        });
        Watch {
            expect,
            seen,
            due: tokio::time::Instant::now() + RECHECK_EVERY,
            recorded: 0,
        }
    }

    /// Whether it re-checks: its turn has an expectation.
    pub(crate) fn on(&self) -> bool {
        self.expect.is_some()
    }

    /// Ready once a re-check is due: at its first poll when it is due already, so an event loop that polls it before
    /// its next event re-checks on time however long events stay ready (M2T15D-400); else when it comes, so an idle
    /// loop wakes for it.
    pub(crate) async fn due(&self) {
        if tokio::time::Instant::now() < self.due {
            tokio::time::sleep_until(self.due).await;
        }
    }

    /// The re-check while the turn runs, the next one due [`RECHECK_EVERY`] from now: each file whose stamp is not the
    /// expected one and not the one it was last seen with is an `IMPLEMENTATION_CHANGED_IN_TURN` diagnostic (the
    /// launcher only when it is not the same file as the implementation seen changed with it); the events to record
    /// (key, payload).
    pub(crate) fn check(&mut self) -> Vec<(String, serde_json::Value)> {
        self.due = tokio::time::Instant::now() + RECHECK_EVERY;
        let Some(expect) = &self.expect else {
            return Vec::new();
        };
        let now = stamps_now(expect);
        let expected = [expect.stamp.launcher, expect.stamp.implementation];
        // A file is recorded when its stamp is not the expected one and it was not seen with it already.
        let new = |i: usize| now[i] != Some(expected[i]) && now[i] != self.seen[i];
        let implementation = new(1);
        // The launcher seen as the same file as the implementation recorded with it is that one change.
        let launcher = new(0) && !(implementation && now[0] == now[1]);
        let mut events = Vec::new();
        for (i, file, recorded) in [
            (1, AgentFile::Implementation, implementation),
            (0, AgentFile::Launcher, launcher),
        ] {
            if recorded {
                self.recorded += 1;
                events.push((
                    format!("implementation-changed-in-turn-{}", self.recorded),
                    json!({"reason": CHANGED_IN_TURN, "file": file.as_str(), "expected": expected[i],
                           "seen": seen_json(now[i])}),
                ));
            }
        }
        self.seen = now;
        events
    }

    /// The expectation its re-checks compare with: the re-check before the first input uses it too ([`recheck`]).
    pub(crate) fn expect(&self) -> Option<&Expect> {
        self.expect.as_ref()
    }
}

/// One thing a turn recorded for its attempt: an event (key, kind, payload, as the direct mode writes them), or the
/// agent's own session id.
#[derive(Debug, Clone, PartialEq)]
pub enum Recorded {
    Event {
        key: String,
        kind: String,
        payload: serde_json::Value,
    },
    NativeSession(String),
}

/// What a turn in the runner's mode showed (docs/adr/ADR-022.md decisions 12 and 14): its verdict and what the runner
/// records. The adapter moved no attempt state and wrote nothing to the store.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    /// The adapter's verdict: SETTLED, FAILED, or CANCELLED after a native cancel.
    pub state: AttemptState,
    /// The failure's code (a reason code, or the agent's own result subtype), unless it settled.
    pub failure: Option<String>,
    pub native_session: Option<String>,
    pub resolved_model: Option<String>,
    pub usage: Option<serde_json::Value>,
    /// The directory of the turn's raw logs.
    pub raw_log_path: String,
    /// The most stdout bytes the frame reader held at once (AC-027-03).
    pub max_buffered: Option<usize>,
    /// Whether the leader was reaped and its process group then seen empty.
    pub group_reaped: bool,
    /// What the turn recorded, in order.
    pub recorded: Vec<Recorded>,
    /// Why the agent's preferred transport could not serve this turn (Codex's App Server, decision 15): the next
    /// attempt is to use the other.
    pub degraded: Option<String>,
    /// The text of its final assistant message, where a verifier's report block is read (docs/adr/ADR-028.md
    /// decision 15); none when it gave none.
    pub final_text: Option<String>,
    /// HoH's own finding that its agent was not the verified one (docs/adr/ADR-030.md decision 6): only this module
    /// makes one, so its failure class never comes from a failure string.
    pub compat: Option<CompatRefusal>,
    /// When the limit this turn FAILED on lifts, as the agent's provider said it (unix seconds; docs/adr/ADR-012.md,
    /// M2T07R-256): only for a turn that failed on a usage or rate limit whose stream carried that instant (Claude's
    /// usage limit, codex App Server's used-up rate-limit window), for the host to wait for as the provider's
    /// Retry-After (REQ-042 behaviour 1). None otherwise, and for every stream that carries no such instant: a plain
    /// 429/503 Retry-After the CLI waited or dropped itself, codex exec, pi.
    pub retry_at: Option<SystemTime>,
}

/// The instant `value` names in unix seconds, when it is a positive whole number of them that is one.
pub(crate) fn unix_seconds(value: &serde_json::Value) -> Option<SystemTime> {
    let secs = value.as_u64().filter(|s| *s > 0)?;
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs))
}

impl Facts {
    /// A turn whose gate was dropped unopened: the agent never ran.
    pub(crate) fn gate_closed(
        raw_log_path: String,
        report: Option<&crate::process::ExitReport>,
    ) -> Facts {
        Facts {
            state: AttemptState::Failed,
            failure: Some("GATE_CLOSED".to_owned()),
            native_session: None,
            resolved_model: None,
            usage: None,
            raw_log_path,
            max_buffered: report.map(|r| r.stdout.max_buffered),
            group_reaped: report.is_some_and(|r| r.group_reaped),
            recorded: Vec::new(),
            degraded: None,
            final_text: None,
            compat: None,
            retry_at: None,
        }
    }

    /// A turn whose gate refused to exec its agent (docs/adr/ADR-030.md decision 9, "适配器读"): FAILED
    /// COMPAT_UNVERIFIED, typed as the gate's refusal (Compat, or transient when only the stamps differed), with an
    /// `adapter.diagnostic{reason: GATE_REFUSED, item, expected, seen}`. The agent never ran.
    pub fn gate_refused(
        raw_log_path: String,
        report: Option<&crate::process::ExitReport>,
        refusal: Refusal,
    ) -> Facts {
        Facts {
            failure: Some("COMPAT_UNVERIFIED".to_owned()),
            recorded: vec![Recorded::Event {
                key: "gate-refused".to_owned(),
                kind: "adapter.diagnostic".to_owned(),
                payload: json!({"reason": "GATE_REFUSED", "item": refusal.item,
                                "expected": refusal.expected, "seen": refusal.seen}),
            }],
            compat: Some(CompatRefusal {
                origin: Origin::Gate,
                stamp_only: refusal.stamp_only,
            }),
            ..Facts::gate_closed(raw_log_path, report)
        }
    }

    /// The facts `self` of a turn whose re-check before its first input found `changed` (docs/adr/ADR-030.md decision 9,
    /// "首个输入之前的再核对"): it sent no input and closed, and its adapter judged it FAILED COMPAT_UNVERIFIED; typed as
    /// that re-check's (Compat, or transient when only the stamps changed), as only this module types one. Another
    /// verdict (a cancel, a raw log cut short) stays as it was judged.
    pub(crate) fn before_input(self, changed: Option<&Changed>) -> Facts {
        match changed {
            Some(changed)
                if self.state == AttemptState::Failed
                    && self.failure.as_deref() == Some(COMPAT_UNVERIFIED) =>
            {
                Facts {
                    compat: Some(CompatRefusal {
                        origin: Origin::BeforeInput,
                        stamp_only: changed.stamp_only,
                    }),
                    ..self
                }
            }
            _ => self,
        }
    }

    /// A turn whose drive failed before it had a verdict.
    pub(crate) fn incomplete(raw_log_path: String, recorded: Vec<Recorded>) -> Facts {
        Facts {
            state: AttemptState::Failed,
            failure: Some("PROTOCOL_INCOMPLETE".to_owned()),
            native_session: None,
            resolved_model: None,
            usage: None,
            raw_log_path,
            max_buffered: None,
            group_reaped: false,
            recorded,
            degraded: None,
            final_text: None,
            compat: None,
            retry_at: None,
        }
    }
}

/// Close a held gate's input unopened and wait for its process to end; its exit report.
pub(crate) async fn close_gate(
    process: &mut crate::process::Process,
) -> Option<crate::process::ExitReport> {
    drop(process.take_stdin());
    while let Some(event) = process.next().await {
        if let crate::process::Event::Exit(report) = event {
            return Some(*report);
        }
    }
    None
}

/// A turn in the runner's mode, started and held at its gate.
#[derive(Debug)]
pub struct Held {
    /// The exact Codex turn's input port; absent on adapters without mid-turn steering.
    pub steers: Option<tokio::sync::mpsc::Sender<crate::codex::Steer>>,
    /// The gate's pid: the agent's once the gate execs it, the leader of its own process group.
    pub pid: u32,
    /// Sent: the gate opens and the turn's input follows. Dropped unsent: the gate reads the end of its input and the
    /// agent never runs.
    pub open: tokio::sync::oneshot::Sender<()>,
    /// A native cancel (the agent's own way to stop a turn); the host's ladder sends SIGTERM and SIGKILL.
    pub cancel: std::sync::Arc<tokio::sync::Notify>,
    /// The turn's facts once it ended.
    pub ended: tokio::task::JoinHandle<Facts>,
}

/// Why a turn in the runner's mode did not start: its code (`BLOCKED_CAPABILITY`, `LOGS`, `SPAWN`) and detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub code: String,
    pub detail: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl std::error::Error for Refused {}

/// Where a turn's writes go: the store in the direct mode; in the runner's, a list of what it saw (input moves are
/// the runner's then, and refused).
pub(crate) struct Sink<'a> {
    pub(crate) store: Option<&'a Store>,
    pub(crate) recorded: Vec<Recorded>,
    /// Shown each diagnostic the list collects (the runner's mode).
    pub(crate) observer: Observer,
}

impl Sink<'_> {
    pub(crate) async fn attempt_event(
        &mut self,
        attempt_id: &str,
        key: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), PersistError> {
        match self.store {
            Some(store) => store.attempt_event(attempt_id, key, kind, payload).await,
            None => {
                self.observer.collected(kind, &payload);
                self.recorded.push(Recorded::Event {
                    key: key.to_owned(),
                    kind: kind.to_owned(),
                    payload,
                });
                Ok(())
            }
        }
    }

    pub(crate) async fn soft_move(
        &mut self,
        attempt_id: &str,
        input_id: &str,
        from: InputState,
        to: InputState,
        reason: Option<String>,
    ) -> Result<bool, PersistError> {
        match self.store {
            Some(store) => soft_move(store, attempt_id, input_id, from, to, reason).await,
            None => Ok(false),
        }
    }
}

/// Where a turn's cancel comes from: the attempt's latch (the direct mode: the adapter stops the turn as its plan
/// says), or the runner's native cancel (the adapter does only the agent's own cancel; the host's ladder does the
/// rest, docs/adr/ADR-022.md decision 14).
pub(crate) enum Stop<'a> {
    Latch(&'a CancelLatch),
    Native(&'a tokio::sync::Notify),
}

impl Stop<'_> {
    pub(crate) fn requested_now(&self) -> bool {
        match self {
            Stop::Latch(latch) => latch.is_requested(),
            Stop::Native(_) => false,
        }
    }

    pub(crate) async fn requested(&self) {
        match self {
            Stop::Latch(latch) => latch.requested().await,
            Stop::Native(notify) => notify.notified().await,
        }
    }

    pub(crate) fn is_native(&self) -> bool {
        matches!(self, Stop::Native(_))
    }
}

/// Where a turn records what it sees: the store, at once (the direct mode), or a list the runner records, each
/// diagnostic shown to its observer as it is collected.
pub(crate) enum Recorder<'a> {
    Store {
        store: &'a Store,
        attempt: &'a AttemptRef,
        agent: &'static str,
        capabilities: &'a serde_json::Value,
    },
    Collect(Vec<Recorded>, Observer),
}

impl Recorder<'_> {
    pub(crate) async fn event(
        &mut self,
        key: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), PersistError> {
        match self {
            Recorder::Store { store, attempt, .. } => {
                store
                    .attempt_event(&attempt.attempt_id, key, kind, payload)
                    .await
            }
            Recorder::Collect(recorded, observer) => {
                observer.collected(kind, &payload);
                recorded.push(Recorded::Event {
                    key: key.to_owned(),
                    kind: kind.to_owned(),
                    payload,
                });
                Ok(())
            }
        }
    }

    /// The agent named its own session: the session row records it (or the runner does).
    pub(crate) async fn native_session(&mut self, native: &str) -> Result<(), PersistError> {
        match self {
            Recorder::Store {
                store,
                attempt,
                agent,
                capabilities,
            } => {
                store
                    .upsert_session(hoh_core::persistence::Session {
                        session_id: attempt.session_id.clone(),
                        agent: (*agent).to_owned(),
                        role: attempt.role.clone(),
                        native_id: Some(native.to_owned()),
                        native_path: None,
                        agent_version: attempt.agent_version.clone(),
                        capabilities: (*capabilities).clone(),
                    })
                    .await
            }
            Recorder::Collect(recorded, _) => {
                recorded.push(Recorded::NativeSession(native.to_owned()));
                Ok(())
            }
        }
    }
}

/// What an adapter's own judging makes of frames replayed through it, no process started (docs/adr/ADR-030.md
/// decision 10, the compat suite's replay): the end it judges, its failure code and how many `adapter.unknown_event`s
/// it records. A replay runs a turn of the runner's mode (a cancel is the agent's own only) that the host started with
/// the product's YOLO launch, a new session, no model, the suite's prompt and session id
/// ([`crate::compat::SUITE_PROMPT`], [`crate::compat::SUITE_SESSION`]), its prompt written whole, its process exiting
/// with code 0 after the last frame with both streams read to their end.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Replayed {
    pub state: AttemptState,
    pub failure: Option<String>,
    pub unknown_events: usize,
}

/// The `adapter.diagnostic` payload of a frame over the limit that the adapter's rule knew and the reader skipped
/// (CR-040): its type, length, sha256 and first 4 KiB.
pub(crate) fn skipped_frame(skipped: &crate::frame::SkippedFrame) -> serde_json::Value {
    serde_json::json!({
        "reason": SKIPPED_FRAME,
        "type": skipped.kind,
        "length": skipped.length,
        "sha256": skipped.sha256,
        "head": String::from_utf8_lossy(&skipped.head),
    })
}

/// The reason of a skipped frame's diagnosis (CR-040).
pub const SKIPPED_FRAME: &str = "known frame over the limit skipped";

/// One step of a replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Replay {
    /// A line of the agent's stdout (framed as [`crate::frame::FrameReader`] frames it: on LF only, one CR before it
    /// dropped, bytes after the last LF a frame cut short).
    Frame(Vec<u8>),
    /// The host's cancel at this point: the line [`REPLAY_CANCEL`], which is no frame of the agent's.
    Cancel,
}

/// The line of a replayed stream that stands for the host's cancel (docs/adr/ADR-030.md decision 10).
pub const REPLAY_CANCEL: &str = r#"{"hoh_replay":"cancel"}"#;

/// The steps of a replayed stream. No frame is refused as too large: the replay judges the frames, not the reader.
pub(crate) fn replay_steps(bytes: &[u8]) -> Vec<Replay> {
    let mut steps = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let (line, after) = match rest.iter().position(|b| *b == b'\n') {
            Some(at) => {
                let line = &rest[..at];
                (line.strip_suffix(b"\r").unwrap_or(line), &rest[at + 1..])
            }
            None => (rest, &rest[rest.len()..]),
        };
        steps.push(if line == REPLAY_CANCEL.as_bytes() {
            Replay::Cancel
        } else {
            Replay::Frame(line.to_vec())
        });
        rest = after;
    }
    steps
}

/// The output of a future that is ready at its first poll, with no runtime: an adapter's judging with nothing to wait
/// for (a replay writes to no store and no process). `None` if it would wait.
pub(crate) fn at_once<F: std::future::Future>(future: F) -> Option<F::Output> {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    match future.as_mut().poll(&mut context) {
        std::task::Poll::Ready(output) => Some(output),
        std::task::Poll::Pending => None,
    }
}

/// How a replayed turn's process ended: by itself with code 0 after its last frame, both streams read to their end.
pub(crate) fn replayed_exit() -> crate::process::ExitReport {
    let complete = crate::process::StreamReport {
        eof: true,
        ..crate::process::StreamReport::default()
    };
    crate::process::ExitReport {
        pid: 0,
        status: Some(crate::process::Termination::Code(0)),
        cause: crate::process::Cause::Exited,
        steps: Vec::new(),
        orphans: Some(0),
        group_reaped: true,
        stdout: complete.clone(),
        stderr: complete,
        stderr_tail: Vec::new(),
    }
}

/// How many `adapter.unknown_event`s a turn recorded.
pub(crate) fn unknown_events(recorded: &[Recorded]) -> usize {
    recorded
        .iter()
        .filter(|r| matches!(r, Recorded::Event { kind, .. } if kind == "adapter.unknown_event"))
        .count()
}

/// The replay of a turn whose judging could not finish at once or could not record (neither happens without a store
/// or a process): FAILED with why, so it never passes for an expected end.
pub(crate) fn unreplayable(why: &str) -> Replayed {
    Replayed {
        state: AttemptState::Failed,
        failure: Some(format!("REPLAY_INCOMPLETE: {why}")),
        unknown_events: 0,
    }
}

/// Why a turn could not be recorded or run.
#[derive(Debug)]
pub enum AdapterError {
    Store(PersistError),
    Io(std::io::Error),
}

impl From<PersistError> for AdapterError {
    fn from(e: PersistError) -> Self {
        AdapterError::Store(e)
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e)
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Store(e) => write!(f, "store: {e}"),
            AdapterError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for AdapterError {}

/// The sha256 of a raw log, read in fixed-size pieces: a turn's logs are not bounded by the frame limit or the stderr
/// tail, so the whole file is never held in memory (M1R-05).
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    sha256_read(&std::fs::File::open(path)?)
}

/// The sha256 of what is read from the open file `file` from where it is (just opened: its start), in fixed-size
/// pieces.
fn sha256_read(mut file: &std::fs::File) -> std::io::Result<String> {
    use std::io::Read;
    let mut hasher = Sha256::new();
    let mut piece = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => hasher.update(&piece[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub(crate) fn now() -> Option<String> {
    Some(hoh_core::time::now_rfc3339())
}

pub(crate) async fn step(
    store: &Store,
    attempt: &AttemptRef,
    from: AttemptState,
    to: AttemptState,
    facts: AttemptFacts,
) -> Result<(), PersistError> {
    store
        .move_attempt(AttemptMove {
            attempt_id: attempt.attempt_id.clone(),
            from,
            to,
            facts,
        })
        .await
}

/// End the attempt through the latch: the state written (CANCELLED if a cancel was accepted first).
pub(crate) async fn finish(
    store: &Store,
    attempt: &AttemptRef,
    from: AttemptState,
    to: AttemptState,
    facts: AttemptFacts,
    cancel: &CancelLatch,
) -> Result<AttemptState, PersistError> {
    store
        .end_attempt(
            AttemptMove {
                attempt_id: attempt.attempt_id.clone(),
                from,
                to,
                facts,
            },
            cancel,
        )
        .await
}

/// The deepest indentation of an option's declaration in the recorded helps (clap: 2 or 6 spaces); descriptions and
/// their continuation lines are indented further.
const DECLARATION_INDENT: usize = 8;

/// The option names a line declares, or `None` for a line that declares no option. A declaration, indented at most
/// [`DECLARATION_INDENT`] spaces, is one or more names (`-x` or `-xy`, `--name`, comma-separated, a name may carry a
/// placeholder as `=<value>`, `=VALUE`, `[=<value>]` or `[=VALUE]`) and then only value placeholders (`<value>`,
/// `[value]`, `VALUE`, each maybe ending in `...`), up to two spaces or a tab before its description, or the end of
/// the line. Anything else is text: a continuation line, a list item `- x`, a mention like
/// `--output-format=stream-json)` or `--mode[=rpc]` (M1R-03, M1T07R-18).
pub(crate) fn declared(line: &str) -> Option<Vec<&str>> {
    declaration(line).map(|(names, _)| names)
}

/// The names a declaration line declares (see [`declared`]) and its description: the rest of the line after them.
fn declaration(line: &str) -> Option<(Vec<&str>, &str)> {
    let text = line.trim_start_matches(' ');
    if line.len() - text.len() > DECLARATION_INDENT || !text.starts_with('-') {
        return None;
    }
    let end = [text.find("  "), text.find('\t')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let mut names = Vec::new();
    // A name comes first or after a comma; a name without one ends the names (M1T07R-26).
    let mut name_next = true;
    for word in text[..end].split(' ') {
        let (word, comma) = match word.strip_suffix(',') {
            Some(word) => (word, true),
            None => (word, false),
        };
        if name_next && let Some(name) = flag_name(word) {
            names.push(name);
            name_next = comma;
        } else if name_next || !is_placeholder(word) || comma {
            return None;
        }
    }
    (!names.is_empty() && !name_next).then_some((names, &text[end..]))
}

/// The name of an option word (`-x`, `-xy` starting with a letter, `--name`, `--name=<value>`, `--name[=VALUE]`), or
/// `None`; what follows `=` or `[=` must be a placeholder, not a value (M1R-03).
fn flag_name(word: &str) -> Option<&str> {
    let cut = word.find(['=', '[']).unwrap_or(word.len());
    let (name, rest) = word.split_at(cut);
    let value = rest.is_empty()
        || rest
            .strip_prefix('=')
            .or_else(|| rest.strip_prefix("[=").and_then(|r| r.strip_suffix(']')))
            .is_some_and(names_a_value);
    let named = match (name.strip_prefix("--"), name.strip_prefix('-')) {
        (Some(long), _) => {
            long.starts_with(|c: char| c.is_ascii_alphanumeric())
                && long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        // A short name starts with a letter and may have several letters or digits (pi: `--no-tools, -nt`;
        // M1T07R-22); `-123` is a number, not an option (M1T07R-24).
        (None, Some(short)) => {
            short.starts_with(|c: char| c.is_ascii_alphabetic())
                && short.chars().all(|c| c.is_ascii_alphanumeric())
        }
        (None, None) => false,
    };
    (named && value).then_some(name)
}

/// A placeholder that names a value: `<value>` or an upper-case `VALUE` (a letter first, so `0`, `123` and `_` are
/// literals; M1R-03).
fn names_a_value(word: &str) -> bool {
    (word.len() > 2 && word.starts_with('<') && word.ends_with('>'))
        || (word.starts_with(|c: char| c.is_ascii_uppercase())
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))
}

/// A value placeholder: `<value>`, `[value]` or an upper-case `VALUE`, maybe ending in `...`.
fn is_placeholder(word: &str) -> bool {
    let word = word.strip_suffix("...").unwrap_or(word);
    names_a_value(word) || (word.len() > 2 && word.starts_with('[') && word.ends_with(']'))
}

/// Whether `help` (a probed `--help` text) declares the option `flag`. A flag only named in other text is not offered
/// (M1R-03).
pub fn offers(help: &str, flag: &str) -> bool {
    help.lines()
        .filter_map(declared)
        .any(|names| names.contains(&flag))
}

/// What `help` says about the option `flag`, over every declaration of it: each declaration's description and the
/// lines that follow it up to the next declaration or a blank line; `None` if `help` does not declare it. A value
/// counts only where the help says it, never as the declaration's placeholder (`--mode <rpc>`; M1T07R-19, M1R-03).
pub fn option_text(help: &str, flag: &str) -> Option<String> {
    let lines: Vec<&str> = help.lines().collect();
    let mut text: Option<Vec<&str>> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some((names, description)) = declaration(line)
            && names.contains(&flag)
        {
            let text = text.get_or_insert_with(Vec::new);
            text.push(description);
            text.extend(
                lines[i + 1..]
                    .iter()
                    .take_while(|l| !l.trim().is_empty() && declared(l).is_none()),
            );
        }
    }
    text.map(|text| text.join("\n"))
}

/// Whether `word` is a value listed in `text`: a token between white space, once what only wraps or follows a value is
/// taken off ([`listed_value`]). So `stream-json-v2` is not `stream-json`, `grpc` is not `rpc`, and a longer value
/// (`stdio:///socket`, `stdio://.invalid`, `stdio://[::1]`, `rpc(v2)`, `--output-format=stream-json`) is not the value
/// (M1T07R-17, -27, -30).
pub fn has_word(text: &str, word: &str) -> bool {
    !word.is_empty()
        && text
            .split_whitespace()
            .any(|token| listed_value(token) == word)
}

/// Whether `text` lists `name` as an item of a comma-separated list: each token between white space, taken off as a
/// value ([`listed_value`]), is split at `,`, and each piece taken off once more; a piece equal to `name` lists it. So
/// claude's `--tools` example `"Bash,Edit,Read").` lists `Bash`, while `BashOutput` and the `"Bash` of `"Bash Edit"`
/// do not (docs/adr/ADR-030.md decision 12, M2T15D-150).
pub fn lists_name(text: &str, name: &str) -> bool {
    !name.is_empty()
        && text.split_whitespace().any(|token| {
            listed_value(token)
                .split(',')
                .any(|piece| listed_value(piece) == name)
        })
}

/// A token without what only wraps or follows a whole value: openers `( [ {` before it; one sentence-final `.`, then
/// closers `) ] }` and list punctuation `,` `;` after it; then one pair of quotes around it (`"x"`, `'x'`, `` `x` ``).
/// Brackets, quotes or a `.` inside a value are part of it, and so is a second `.` (`stdio://..`; M1T07R-27).
fn listed_value(token: &str) -> &str {
    let value = token.trim_start_matches(['(', '[', '{']);
    let value = value
        .strip_suffix('.')
        .unwrap_or(value)
        .trim_end_matches([')', ']', '}', ',', ';']);
    ['"', '\'', '`']
        .into_iter()
        .find_map(|quote| {
            value
                .strip_prefix(quote)
                .and_then(|v| v.strip_suffix(quote))
        })
        .unwrap_or(value)
}

/// Whether `help` lists the subcommand `name` in the command column of its `Commands:` section (the least indented
/// lines of that section; wrapped descriptions are indented further), which ends at a blank line or the next
/// heading (M1T07R-20).
pub fn lists_command(help: &str, name: &str) -> bool {
    let section: Vec<&str> = help
        .lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case("commands:"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty() && line.starts_with([' ', '\t']))
        .collect();
    let indent = |line: &&str| line.len() - line.trim_start().len();
    let column = section.iter().map(indent).min();
    section
        .iter()
        .any(|line| Some(indent(line)) == column && line.split_whitespace().next() == Some(name))
}

/// A failure code made of an agent's own text `s` (claude's `result.subtype`, codex's `codexErrorInfo`): `native:<s>`
/// when `s` is one of HoH's reason codes (`$defs.ReasonCode`, as `hoh_core::protocol::ReasonCode` reads it), else `s`
/// as it is (docs/adr/ADR-030.md decision 6, M2T15D-300). So an agent's error named like BLOCKED_AUTH or
/// COMPAT_UNVERIFIED is its own failure, never one of the classes HoH gives only by its own recognitions.
pub fn native_code(s: &str) -> String {
    let reserved = serde_json::from_value::<hoh_core::protocol::ReasonCode>(
        serde_json::Value::String(s.to_owned()),
    )
    .is_ok();
    if reserved {
        format!("native:{s}")
    } else {
        s.to_owned()
    }
}

/// Numbers the diagnostics of input moves that did not happen, so two of one attempt never share an event id.
static NOT_MOVED: AtomicUsize = AtomicUsize::new(1);

/// Move an input a turn delivers; true if moved. An input someone else has moved on (stale) or a move the table refuses is
/// recorded and does not stop the turn (M1T05R-01); other store errors do.
pub(crate) async fn soft_move(
    store: &Store,
    attempt_id: &str,
    input_id: &str,
    from: InputState,
    to: InputState,
    reason: Option<String>,
) -> Result<bool, PersistError> {
    let moved = store
        .move_input(InputMove {
            input_id: input_id.to_owned(),
            from,
            to,
            reason,
        })
        .await;
    match moved {
        Ok(()) => Ok(true),
        Err(e @ (PersistError::StaleState { .. } | PersistError::IllegalTransition { .. })) => {
            store
                .attempt_event(
                    attempt_id,
                    &format!("input-not-moved-{}", NOT_MOVED.fetch_add(1, Ordering::Relaxed)),
                    "adapter.diagnostic",
                    json!({"attempt_id": attempt_id, "reason": "input not moved", "input_id": input_id,
                           "to": to.as_str(), "error": e.to_string()}),
                )
                .await?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod help_tests {
    use super::*;

    const HELP: &str = "Usage: tool [options]\n\nOptions:\n  -p, --print                 Print and exit\n  --verbose\tOverride verbose mode (see --quiet)\n  --output-format <format>    Output format: \"text\", \"json\" or\n                              \"stream-json\" (with --print)\n                              --quiet\n  -i, --image <FILE>...\n          Images to attach; works with\n      --output-format=stream-json)\n  --mode <mode>               Output mode: text or json\n      - text\n      - json\n  --mode <mode>               Also: rpc\n  --level <LEVEL>             One of: stream-json-v2, grpc\n\nCommands:\n  help    Print help\n          resume a session through another tool\n  review  Review code\nOther:\n  resume  Not a command\n";

    /// A flag counts only where an option line declares it: not in a continuation line, a description, a list item or
    /// after a tab (M1R-03).
    #[test]
    fn only_declarations_offer_a_flag() {
        for flag in [
            "-p",
            "--print",
            "--verbose",
            "--output-format",
            "-i",
            "--image",
            "--mode",
            "--level",
        ] {
            assert!(offers(HELP, flag), "{flag}");
        }
        for flag in ["--quiet", "--with", "-", "--print)"] {
            assert!(!offers(HELP, flag), "{flag}");
        }
        // The real --output-format declaration gone: its mention in --image's continuation line does not count.
        let without = HELP.replace(
            "  --output-format <format>    Output format: \"text\", \"json\" or\n",
            "",
        );
        assert!(!offers(&without, "--output-format"));
    }

    /// An option's text is the text of every declaration of it, list items included; a value is a whole word of it
    /// (M1T07R-17..19).
    #[test]
    fn values_are_whole_words_of_every_declaration_of_the_option() {
        let words = |flag: &str| option_text(HELP, flag).map(|t| has_word(&t, "stream-json"));
        assert_eq!(words("--output-format"), Some(true));
        assert_eq!(
            words("--level"),
            Some(false),
            "stream-json-v2 is not stream-json"
        );
        assert!(
            option_text(HELP, "--level").is_some_and(|t| !has_word(&t, "rpc")),
            "grpc is not rpc"
        );
        let mode = option_text(HELP, "--mode").unwrap_or_default();
        assert!(has_word(&mode, "json") && has_word(&mode, "rpc"), "{mode}");
        assert!(option_text(HELP, "--quiet").is_none());
    }

    /// Every declaration of the recorded pi 1.5.0 help is one, all eight multi-letter short aliases included, and
    /// none is read as the previous option's text (M1T07R-22, -25).
    #[test]
    fn the_recorded_pi_declarations_are_all_declarations() {
        let help = include_str!("../../../tests/fixtures/protocol/pi/1.5.0/help.txt");
        for (long, short) in [
            ("--no-tools", "-nt"),
            ("--no-builtin-tools", "-nbt"),
            ("--exclude-tools", "-xt"),
            ("--no-extensions", "-ne"),
            ("--no-skills", "-ns"),
            ("--no-prompt-templates", "-np"),
            ("--no-context-files", "-nc"),
            ("--no-approve", "-na"),
        ] {
            assert!(offers(help, long) && offers(help, short), "{long}, {short}");
        }
        for (option, own, other) in [
            ("--models", "fuzzy matching", "--no-tools"),
            ("--tools", "mcp__", "--exclude-tools"),
        ] {
            let text =
                option_text(help, option).unwrap_or_else(|| panic!("{option} is not declared"));
            assert!(text.contains(own), "{option} lost its own text: {text}");
            assert!(
                !text.contains(other),
                "{option} took {other}'s text: {text}"
            );
        }
    }

    /// Text that is no declaration is refused without fail: a word of any script, a negative number (M1T07R-23, -24).
    #[test]
    fn other_text_is_no_declaration() {
        assert_eq!(declared("  --other 中文说明"), None);
        assert_eq!(declared("  -中文"), None);
        assert_eq!(declared("      -123"), None);
        assert_eq!(declared("  -1"), None);
        assert_eq!(
            declared("  -nt, --no-tools"),
            Some(vec!["-nt", "--no-tools"])
        );
    }

    /// A value in the declaration's own syntax is no value: `=…` and `[=…]` hold a placeholder, not a literal, and an
    /// option's values are what its description says, never its placeholder (M1R-03).
    #[test]
    fn a_value_is_read_from_the_description_only() {
        for mention in [
            "      --output-format[=stream-json]",
            "      --mode[=rpc]",
            "      --json[=no]    not a real flag",
            "  --mode=rpc  Output mode",
        ] {
            assert_eq!(declared(mention), None, "{mention}");
        }
        for (line, name) in [
            ("  --color[=<WHEN>]  Colorize", "--color"),
            ("  --color[=WHEN]", "--color"),
            ("  --out=<FILE>  Write it", "--out"),
            ("  -r, --resume [value]  Resume", "--resume"),
        ] {
            assert!(
                declared(line).is_some_and(|names| names.contains(&name)),
                "{line}"
            );
        }
        // `[rpc]` and `[stream-json]` hold whole tokens: only the description-only rule keeps them out.
        let help = "Options:\n  --mode [rpc]  Output mode: text or json\n  --output-format [stream-json]\n  --level LEVEL  One of: rpc\n";
        let value = |help: &str, flag: &str, word: &str| {
            option_text(help, flag).map(|text| has_word(&text, word))
        };
        assert_eq!(value(help, "--mode", "rpc"), Some(false));
        assert_eq!(value(help, "--output-format", "stream-json"), Some(false));
        assert_eq!(value(help, "--level", "rpc"), Some(true));
        // The recorded helps still offer the values the adapters need.
        let claude = include_str!("../../../tests/fixtures/protocol/claude/2.1.318/help.txt");
        let pi = include_str!("../../../tests/fixtures/protocol/pi/1.5.0/help.txt");
        assert_eq!(value(claude, "--output-format", "stream-json"), Some(true));
        assert_eq!(value(pi, "--mode", "rpc"), Some(true));
    }

    /// A placeholder names a value: a number or `_` is a literal; and two names of one declaration are separated by
    /// a comma, so a description that names two options is not a declaration of them (M1R-03, M1T07R-26).
    #[test]
    fn literals_and_option_lists_are_no_declarations() {
        for line in [
            "  --json=0",
            "  --json[=0]",
            "  --json[=_]",
            "  --json=123",
            "  --json 0",
            "      --other --json  mentioned alongside another option",
            "  -p --print",
            "  --json,",
            "  -c, <FILE>",
        ] {
            assert_eq!(declared(line), None, "{line}");
        }
        for (line, names) in [
            ("  -p, --print  Print", vec!["-p", "--print"]),
            ("  --no-tools, -nt", vec!["--no-tools", "-nt"]),
            ("  -c, --config <key=value>", vec!["-c", "--config"]),
            (
                "  -p, --profile <CONFIG_PROFILE_V2>",
                vec!["-p", "--profile"],
            ),
            ("  --level LEVEL2  One", vec!["--level"]),
        ] {
            assert_eq!(declared(line), Some(names), "{line}");
        }
    }

    /// A value is a whole token of the text, whatever it is made of: `stdio://` as codex lists it, not `stdio://x`
    /// or `pipe://` (M1T07R-27); words as before.
    #[test]
    fn a_value_is_a_whole_token() {
        let listen = "Transport endpoint URL. Supported values: `stdio://` (default), `unix://`";
        assert!(has_word(listen, "stdio://") && has_word(listen, "unix://"));
        assert!(!has_word(
            "Supported values: `stdio://x`, `pipe://`",
            "stdio://"
        ));
        assert!(has_word("text, json, or rpc", "rpc") && !has_word("grpc", "rpc"));
        assert!(
            !has_word("stream-json-v2", "stream-json") && has_word("(stream-json)", "stream-json")
        );
        // A longer URI is another value (M1T07R-27, round 2); a value may end a sentence.
        for longer in [
            "Supported values: `stdio:///socket`",
            "Supported values: stdio://?mode=pipe",
            "Supported values: `stdio://.invalid`",
        ] {
            assert!(!has_word(longer, "stdio://"), "{longer}");
        }
        assert!(has_word("Supported values: unix://, stdio://.", "stdio://"));
        assert!(has_word("Output mode: text, json or rpc.", "rpc"));
        // Part of another token is no value: a file name, another option's syntax.
        assert!(!has_word(
            "Output mode: text or json (see docs/rpc.md)",
            "rpc"
        ));
        assert!(!has_word(
            "Implies --output-format=stream-json",
            "stream-json"
        ));
        // Brackets and quotes only wrap a whole value; inside it they are part of it (M1T07R-30).
        for longer in [
            "Supported values: stdio://[::1]",
            "Supported values: stdio://(socket)",
            "Supported values: stdio://.(invalid)",
            "Supported values: stdio://{mem}",
        ] {
            assert!(!has_word(longer, "stdio://"), "{longer}");
        }
        assert!(!has_word("Output format: stream-json[v2]", "stream-json"));
        assert!(!has_word("Output mode: rpc(v2)", "rpc"));
        assert!(!has_word("Output format: \"stream-json", "stream-json"));
        for listed in [
            "(choices: \"text\", \"json\", \"stream-json\")",
            "[default: stdio://]",
            "(`stdio://`).",
        ] {
            assert!(
                has_word(listed, "stream-json") || has_word(listed, "stdio://"),
                "{listed}"
            );
        }
        // One sentence-final period only: `stdio://..` is a longer value (M1T07R-27, round 4).
        for longer in ["stdio://..", "stdio://...", "stdio://.)"] {
            assert!(
                !has_word(&format!("Supported values: {longer}"), "stdio://"),
                "{longer}"
            );
        }
        assert!(!has_word("Output format: stream-json..", "stream-json"));
        assert!(!has_word("Output mode: rpc...", "rpc"));
        assert!(has_word("Supported values: (stdio://).", "stdio://"));
    }

    /// A name is listed as a piece of a comma-separated token, each piece taken off as a value: what wraps or follows
    /// the whole token and what wraps or follows the piece (docs/adr/ADR-030.md decision 12, M2T15D-150).
    #[test]
    fn a_name_is_a_piece_of_a_listed_token() {
        for token in [
            "\"a,b\").",
            "(a,",
            "'a'",
            "a;",
            "a",
            "b,a",
            "[\"x\", \"a\"]",
        ] {
            assert!(lists_name(token, "a"), "{token}");
        }
        for token in ["\"a", "ab", "ab,c", "b.a", "a(x)", "\"a b\"", ""] {
            assert!(!lists_name(token, "a"), "{token}");
        }
        assert!(!lists_name("a,b", ""), "an empty name is never listed");
    }

    /// A subcommand is a name in the command column of the `Commands:` section, not a wrapped description and not a
    /// later section (M1T07R-20).
    #[test]
    fn commands_are_read_from_their_column() {
        assert!(lists_command(HELP, "help") && lists_command(HELP, "review"));
        assert!(!lists_command(HELP, "resume"));
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    /// Set for the child process [`a_raw_log_larger_than_the_memory_allowed_is_hashed`] starts.
    const BOUNDED_CHILD: &str = "HOH_TEST_BOUNDED_HASH_FILE";

    /// A raw log is hashed in fixed-size pieces: a process allowed 64 MiB of data hashes a 256 MiB log, so a large log
    /// never turns a finished turn into an I/O error (M1R-05).
    #[test]
    fn a_raw_log_larger_than_the_memory_allowed_is_hashed() -> Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let log = tmp.path().join("stderr.log");
        std::fs::File::create(&log)?.set_len(256 << 20)?;
        // The same bytes hashed here, piece by piece, without the function under test.
        let mut expected = Sha256::new();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..256 {
            expected.update(&zeros);
        }
        let expected: String = expected
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let out = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "adapter::hash_tests::bounded_hash_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(BOUNDED_CHILD, &log)
            .output()?;
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(said.contains(&format!("hashed {expected}")), "{said}");
        Ok(())
    }

    #[test]
    #[ignore = "run by a_raw_log_larger_than_the_memory_allowed_is_hashed in a process of its own"]
    fn bounded_hash_child() -> Result<(), Box<dyn std::error::Error>> {
        let Some(log) = std::env::var_os(BOUNDED_CHILD) else {
            return Ok(());
        };
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        let old = getrlimit(Resource::Data);
        setrlimit(
            Resource::Data,
            Rlimit {
                current: Some(64 << 20),
                maximum: old.maximum,
            },
        )?;
        // The limit must hold, or the test proves nothing: a 128 MiB reservation fails under it (M1R-08).
        assert!(
            Vec::<u8>::new().try_reserve_exact(128 << 20).is_err(),
            "the data limit is not enforced here"
        );
        println!("hashed {}", sha256_file(Path::new(&log))?);
        Ok(())
    }
}

#[cfg(test)]
mod native_tests {
    use super::*;
    use hoh_core::protocol::{AllVariants, BlockedReason, ErrorCode, RejectionReason};

    /// Every code of `$defs.ReasonCode` (the three enums the protocol test compares with the schema).
    fn reason_codes() -> Vec<String> {
        fn names<T: AllVariants + serde::Serialize>() -> Vec<String> {
            T::ALL
                .iter()
                .filter_map(|v| serde_json::to_value(v).ok())
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        }
        let mut all = names::<BlockedReason>();
        all.extend(names::<RejectionReason>());
        all.extend(names::<ErrorCode>());
        all
    }

    #[test]
    fn a_native_text_named_like_a_reason_code_is_marked_native() {
        // docs/adr/ADR-030.md decision 6 (M2T15D-300): every reason code, and the five the ADR names, become
        // `native:<code>`; any other text is kept as it is.
        let codes = reason_codes();
        for named in [
            "BLOCKED_AUTH",
            "BLOCKED_CAPABILITY",
            "MODEL_UNAVAILABLE",
            "COMPAT_UNVERIFIED",
            "AGENT_UNAVAILABLE",
        ] {
            assert!(
                codes.iter().any(|c| c == named),
                "{named} is no reason code"
            );
        }
        for code in &codes {
            assert_eq!(native_code(code), format!("native:{code}"));
        }
        for text in [
            "error_max_turns",
            "error_during_execution",
            "contextWindowExceeded",
            "blocked_auth",
            "BLOCKED_AUTH ",
            "native:BLOCKED_AUTH",
            "",
        ] {
            assert_eq!(native_code(text), text);
        }
    }

    /// The places an agent's own text becomes a failure code (docs/adr/ADR-030.md decision 6: "以单元测试列出全部把原生
    /// 文字变成失败码的位置"): claude's `result.subtype` of a turn that did not succeed, and codex's `codexErrorInfo` it does
    /// not recognise (App Server and exec both go through `failure_of`). Each goes through [`native_code`]. The other
    /// texts the adapters keep are prefixed by what failed (pi's `get_state: <text>`, codex's `initialize: <message>`,
    /// `thread/start: <message>`, `turn/start: <message>`) and so never equal a code; pi's provider errors become fixed
    /// codes only.
    #[test]
    fn every_place_that_makes_a_failure_code_of_native_text_goes_through_native_code() {
        use crate::process::Termination;
        let codes = reason_codes();
        for code in &codes {
            let frame = crate::claude::ResultFrame {
                subtype: Some(code.clone()),
                is_error: Some(true),
                text: None,
            };
            let claude = crate::claude::verdict(&crate::claude::Ending {
                result: Some(&frame),
                exit: Some(Termination::Code(0)),
                ..crate::claude::Ending::default()
            });
            assert_eq!(
                claude,
                (AttemptState::Failed, Some(format!("native:{code}"))),
                "claude {code}"
            );
            let error = json!({"codexErrorInfo": code, "message": "m"});
            let codex = crate::codex::failure_of(Some(&error), false);
            assert_eq!(codex, format!("native:{code}"), "codex {code}");
        }
        // The adapters' own recognitions keep HoH's codes.
        let frame = crate::claude::ResultFrame {
            subtype: Some("error_during_execution".to_owned()),
            is_error: Some(true),
            text: None,
        };
        assert_eq!(
            crate::claude::verdict(&crate::claude::Ending {
                result: Some(&frame),
                auth_failed: true,
                exit: Some(Termination::Code(1)),
                ..crate::claude::Ending::default()
            }),
            (AttemptState::Failed, Some("BLOCKED_AUTH".to_owned()))
        );
        assert_eq!(
            crate::claude::verdict(&crate::claude::Ending {
                result: Some(&frame),
                exit: Some(Termination::Code(1)),
                ..crate::claude::Ending::default()
            }),
            (
                AttemptState::Failed,
                Some("error_during_execution".to_owned())
            )
        );
        assert_eq!(
            crate::codex::failure_of(Some(&json!({"codexErrorInfo": "unauthorized"})), false),
            "BLOCKED_AUTH"
        );
        assert_eq!(
            crate::codex::failure_of(
                Some(&json!({"codexErrorInfo": "other", "message": "unexpected status 401"})),
                false
            ),
            "BLOCKED_AUTH"
        );
        assert_eq!(
            crate::codex::failure_of(Some(&json!({"codexErrorInfo": "serverOverloaded"})), false),
            "TRANSIENT_EXTERNAL"
        );
        assert_eq!(
            crate::pi::error_class("No API key found for x"),
            "BLOCKED_AUTH"
        );
        assert_eq!(
            crate::pi::error_class("Model not found: y"),
            "MODEL_UNAVAILABLE"
        );
        assert_eq!(crate::pi::error_class("BLOCKED_AUTH"), "PERMANENT_EXTERNAL");
        assert_eq!(
            crate::pi::error_class("MODEL_UNAVAILABLE"),
            "PERMANENT_EXTERNAL"
        );
    }
}

#[cfg(test)]
mod tests;
