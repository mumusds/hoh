//! The product's agents (docs/adr/ADR-022.md decisions 9 and 10, docs/adr/ADR-030.md decisions 1 to 5): the registry
//! that finds and probes each agent CLI outside the writer and keeps what staffing knows of each, and staffing, which
//! judges each binding against one snapshot of it and has the store write the judgment: a session granted, a binding
//! blocked or unblocked, a fallback or a rebind.

use hoh_agents::capabilities::Probed;
use hoh_agents::compat::{self, Agent, AgentEntry, CompatManifest};
use hoh_agents::launch;
use hoh_agents::login::{self, Login};
use hoh_agents::probe::{self, ProbeEnv, ProbeError};
use hoh_core::decision::Observation;
use hoh_core::persistence::{Evidence, StaffChange, StaffWrite, Store};
use hoh_core::staffing::{
    AgentIdentity, BindingState, Cause, Compat, Fact, FileStamp, Judgment, Kind, Snapshot, Staffed,
    Stamp, Usable,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long one probe command may take.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a wanted agent's login is looked at again (its status command, docs/adr/ADR-024.md decision 7), and how
/// soon a temporary probe failure is probed again though its files did not change (docs/adr/ADR-030.md decision 2).
const AUTH_EVERY: Duration = Duration::from_secs(30);

/// How often a codex host check that did not find confined runs again (docs/adr/ADR-032.md decision 4, -101): a
/// recovery (the host fixed) shows within it; a confined one runs again only when codex's files change.
const HOST_CHECK_EVERY: Duration = Duration::from_secs(60);

/// The capability codex's host facts can withhold (docs/adr/ADR-032.md decision 4).
const READ_ONLY_WORKSPACE: &str = "read_only_workspace";

/// The diagnostic of a compat manifest that cannot be read or is not schema-valid (docs/adr/ADR-030.md decision 10).
pub const MANIFEST_UNREADABLE: &str = "compat.manifest_unreadable";

/// The diagnostic of a turn's BLOCKED_AUTH that is not the agent's fact: another identity's, or older than the agent's
/// latest recovery (docs/adr/ADR-030.md decision 2, M2T15D-07).
pub const TURN_AUTH_IGNORED: &str = "registry.turn_auth_ignored";

/// An agent found and probed: what starting its turns needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub launcher: PathBuf,
    /// The first line of its `--version`.
    pub version: String,
    /// What its probe found.
    pub capabilities: Probed,
    pub facts: launch::Facts,
    /// The probe's entry: the agent's identity, help and schema hashes and capabilities, its suite not run
    /// (docs/adr/ADR-030.md decision 2).
    pub entry: AgentEntry,
    /// The stamps of the launcher and implementation, read before they were probed.
    pub stamp: Stamp,
    /// The `n` of the look that probed it; with the host's epoch, its fact's generation.
    pub look: u64,
}

/// The class of a look's failure: the variant of the [`ProbeError`] it came from (docs/adr/ADR-030.md decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeClass {
    NotFound,
    Unsupported,
    TimedOut,
    NotStarted,
    Failed,
    Io,
}

impl ProbeClass {
    pub fn of(error: &ProbeError) -> ProbeClass {
        match error {
            ProbeError::NotFound { .. } => ProbeClass::NotFound,
            ProbeError::Unsupported { .. } => ProbeClass::Unsupported,
            ProbeError::TimedOut { .. } => ProbeClass::TimedOut,
            ProbeError::NotStarted { .. } => ProbeClass::NotStarted,
            ProbeError::Failed { .. } => ProbeClass::Failed,
            ProbeError::Io(_) => ProbeClass::Io,
        }
    }

    /// Its name, as a staffing evidence object records it (docs/adr/ADR-030.md decision 5).
    pub fn name(self) -> &'static str {
        match self {
            ProbeClass::NotFound => "NotFound",
            ProbeClass::Unsupported => "Unsupported",
            ProbeClass::TimedOut => "TimedOut",
            ProbeClass::NotStarted => "NotStarted",
            ProbeClass::Failed => "Failed",
            ProbeClass::Io => "Io",
        }
    }

    /// A temporary failure is probed again [`AUTH_EVERY`] after it though its files did not change; a permanent one
    /// only once they change.
    pub fn temporary(self) -> bool {
        !matches!(self, ProbeClass::NotFound | ProbeClass::Unsupported)
    }
}

/// Why a look found an agent unusable: its class and what the probe said (docs/adr/ADR-030.md decision 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub class: ProbeClass,
    pub detail: String,
}

impl Probe {
    fn of(error: &ProbeError) -> Probe {
        Probe {
            class: ProbeClass::of(error),
            detail: error.to_string(),
        }
    }

    /// The staffing fact it is.
    pub fn fact(&self) -> Fact {
        match self.class {
            ProbeClass::NotFound => Fact::NotInstalled(self.detail.clone()),
            ProbeClass::Unsupported => Fact::Unsupported(self.detail.clone()),
            ProbeClass::TimedOut | ProbeClass::NotStarted | ProbeClass::Failed | ProbeClass::Io => {
                Fact::ProbeFailed(self.detail.clone())
            }
        }
    }
}

/// An agent as a turn prepared now finds it ([`Registry::prepared`]): read together, under one lock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Current {
    pub ready: Ready,
    pub ran: Ran,
    /// The agent's fact F (docs/adr/ADR-030.md decision 1): prepare checks it and the gate's expectation is taken from it.
    pub fact: Fact,
    /// `compat::verdict` of `ready`'s entry against the manifest the registry holds, `Pending` when the files changed
    /// since the look (F's own when it is usable).
    pub compat: Compat,
    /// Why the agent is not what its last look found (the registry's one predicate of that, which F's compat is
    /// `Pending` by too), `None` while it is: prepare's (a) (docs/adr/ADR-030.md decision 9, M2T15R-01).
    pub changed: Option<String>,
    /// codex: this host's check of its read-only sandbox as the registry keeps it (`{check, reason}`), read in the
    /// same look as F (docs/adr/ADR-032.md decision 4); `None` for the others and before any check.
    pub host_check: Option<serde_json::Value>,
    /// codex: its App Server degraded to exec under the identity it has now, so this turn uses exec; read in the same
    /// look as F, whose `withheld` the same fact decides (M2T20R-01).
    pub exec_only: bool,
}

/// What a turn ran as (docs/adr/ADR-030.md decision 2, M2T15D-07): its agent, the identity of the fact its prepare
/// used and the registry's generation `(host_epoch, n)` of that agent then. A turn's BLOCKED_AUTH is latched as the
/// agent's fact only by it ([`Registry::turn_auth_failed`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    pub agent: Agent,
    pub identity: AgentIdentity,
    pub generation: (i64, u64),
}

/// Something the registry found that the host records (its kind, as an event's, and what it says); the registry
/// itself writes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub kind: &'static str,
    pub detail: String,
}

/// One file's stamp as the cache compares it (`stat`, following links): device, inode, size, modification and change
/// time. The change time moves with a chmod or an xattr too, so a change of metadata alone is probed again
/// (docs/adr/ADR-030.md decision 2, M2T15D-201).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileKey {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl FileKey {
    fn read(path: &Path) -> std::io::Result<FileKey> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::metadata(path)?;
        Ok(FileKey {
            dev: m.dev(),
            ino: m.ino(),
            size: m.size(),
            mtime: (m.mtime(), m.mtime_nsec()),
            ctime: (m.ctime(), m.ctime_nsec()),
        })
    }

    fn stamp(&self) -> FileStamp {
        FileStamp {
            dev: self.dev,
            ino: self.ino,
            ctime_sec: self.ctime.0,
            ctime_nsec: self.ctime.1,
        }
    }
}

/// The launcher's and the implementation's.
type Key = [FileKey; 2];

fn key(launcher: &Path, implementation: &Path) -> std::io::Result<Key> {
    Ok([FileKey::read(launcher)?, FileKey::read(implementation)?])
}

fn stamp_of(key: &Key) -> Stamp {
    Stamp {
        launcher: key[0].stamp(),
        implementation: key[1].stamp(),
    }
}

/// An agent's files as a look finds them: its launcher (the first on the PATH), the implementation
/// [`probe::resolve_implementation`] gives for it, and their stamps.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Files {
    launcher: PathBuf,
    implementation: PathBuf,
    key: Key,
}

/// Why an agent is not what its last look found ([`since`]).
#[derive(Debug)]
struct Changed {
    /// Its files now, or why none can be found or read.
    now: Result<Files, ProbeError>,
    /// What differs from the look's, as its fact's `Pending` and prepare's (a) say it.
    detail: String,
}

/// Whether `agent` is still what the look that found `last` found (docs/adr/ADR-030.md decisions 1, 2 and 9, the
/// implementation correction of M2T15R-01): the one predicate of a look's decision to probe again, of its fact's
/// compat and of prepare's (a). Its files are found now as a look finds them, on `path` (the registry's PATH): the
/// launcher first on it, the implementation resolved for that launcher, their stamps (`stat`, following links); and
/// compared with `last`'s: the launcher's path, the implementation's path, the stamps. File-system reads only, never a
/// process, so it is read under the registry's lock. Its files now when they are `last`'s; else what differs, and its
/// files now or why none were found (with no `last` there is nothing to be current with).
fn since(agent: Agent, path: &OsStr, last: Option<&Files>) -> Result<Files, Box<Changed>> {
    let changed = |now, what: String| {
        Box::new(Changed {
            now,
            detail: format!("files changed since the last look: {what}"),
        })
    };
    let found = |of: fn(&Files) -> &PathBuf| {
        last.map_or_else(
            || "no look found one".to_owned(),
            |last| format!("the look found {}", of(last).display()),
        )
    };
    let Some(launcher) = probe::find_on_path(agent.command(), path) else {
        let what = format!(
            "no {} is found on the PATH ({})",
            agent.command(),
            found(|f| &f.launcher)
        );
        return Err(changed(Err(ProbeError::NotFound { agent }), what));
    };
    let implementation = match probe::resolve_implementation(agent, &launcher) {
        Ok(implementation) => implementation,
        Err(e) => {
            let what = format!(
                "the implementation of {} is not resolved: {e} ({})",
                launcher.display(),
                found(|f| &f.implementation)
            );
            return Err(changed(Err(e), what));
        }
    };
    let key = match key(&launcher, &implementation) {
        Ok(key) => key,
        Err(e) => {
            let what = format!(
                "{} and {} cannot be read: {e}",
                launcher.display(),
                implementation.display()
            );
            return Err(changed(Err(ProbeError::Io(e)), what));
        }
    };
    let now = Files {
        launcher,
        implementation,
        key,
    };
    let what = match last {
        None => "no look found its files".to_owned(),
        Some(last) if now.launcher != last.launcher => format!(
            "the launcher found on the PATH is {}, {}",
            now.launcher.display(),
            found(|f| &f.launcher)
        ),
        Some(last) if now.implementation != last.implementation => format!(
            "{} resolves the implementation {}, {}",
            now.launcher.display(),
            now.implementation.display(),
            found(|f| &f.implementation)
        ),
        Some(last) if now.key != last.key => format!(
            "{} and {} were {}, are {}",
            now.launcher.display(),
            now.implementation.display(),
            serde_json::json!(stamp_of(&last.key)),
            serde_json::json!(stamp_of(&now.key)),
        ),
        Some(_) => return Ok(now),
    };
    Err(changed(Ok(now), what))
}

/// What a look at an agent does ([`Registry::cached`]).
#[derive(Debug)]
enum Look {
    /// What was last seen of it: it is what that look found.
    Cached(Box<Result<Ready, Probe>>),
    /// Probe these files, found now.
    Probe(Files),
    /// None was found: why.
    Missing(ProbeError),
}

/// What an actual look at an agent found.
#[derive(Debug, Clone)]
struct Seen {
    /// The files it was found under; `None` when there were none to read (no launcher, or not readable): looked at again
    /// each time. Its fact is current while the agent is what they are ([`since`]).
    files: Option<Files>,
    /// When: a temporary failure is probed again [`AUTH_EVERY`] after it.
    at: Instant,
    outcome: Result<Ready, Probe>,
    /// A ready agent's session capabilities in the registry's project, once per actual look (`None`: no project yet).
    session: Option<Result<serde_json::Value, String>>,
}

/// An agent's login as its status command last said it, and when.
#[derive(Debug, Clone)]
struct LoginSeen {
    at: Instant,
    /// What logging in takes, when it clearly said logged out.
    missing: Option<String>,
}

/// What the registry knows of one agent.
#[derive(Debug, Default)]
struct Known {
    seen: Option<Seen>,
    login: Option<LoginSeen>,
    /// Its login check's last clear answer: a logged-out one followed by a logged-in one is a visible recovery.
    said: Option<Login>,
    /// Actual looks at it (a probe, a PATH that lacks it, a login check; a cache unchanged is none): the `n` of an
    /// observation's identity (docs/adr/ADR-025.md decision 5).
    looks: u64,
    /// The identity its last usable look found.
    identity: Option<AgentIdentity>,
    /// The `n` of its latest recovery fact (a look that found another identity, a visible login): 0 before any.
    recovered_at: u64,
    /// A turn's BLOCKED_AUTH, its fact until the next recovery (docs/adr/ADR-030.md decision 2).
    latched: Option<String>,
    /// codex: this host's check of its own read-only sandbox, and when (docs/adr/ADR-032.md decision 4). Kept on the
    /// agent, not on one look, with its own refresh ([`HOST_CHECK_EVERY`]).
    host_check: Option<(probe::SandboxCheck, Instant)>,
    /// codex: its App Server degraded to exec in this host's run, as the executor said, for the identity it said it of
    /// (docs/adr/ADR-032.md decision 10 (A)); cleared by another identity, and by the host's restart.
    exec_degraded: Option<AgentIdentity>,
}

impl Known {
    /// Whether codex's App Server degraded to exec in this host's run under the identity it has now: the one fact that
    /// both sends its turns to exec and withholds the read-only profile (docs/adr/ADR-022.md decision 15,
    /// docs/adr/ADR-032.md decision 10 (A), M2T20R-01).
    fn degraded(&self) -> bool {
        self.exec_degraded.is_some() && self.exec_degraded == self.identity
    }

    /// The capabilities this host's facts withhold from the agent now (docs/adr/ADR-032.md decision 4): codex's read-only
    /// workspace while its host check is unconfined or unavailable, or its App Server degraded to exec under its current
    /// identity.
    fn withheld(&self) -> BTreeSet<String> {
        let checked = self
            .host_check
            .as_ref()
            .is_some_and(|(check, _)| check.withholds());
        if checked || self.degraded() {
            [READ_ONLY_WORKSPACE.to_owned()].into()
        } else {
            BTreeSet::new()
        }
    }
}

/// The registry's state, under its one lock.
#[derive(Debug, Default)]
struct State {
    /// The agents the runner needs looked at (its team's and fallbacks, its bindings'): [`Registry::refresh`] looks at
    /// them, off the runner's tick, so no probe holds up a tick or the host's stop.
    wanted: BTreeSet<Agent>,
    agents: BTreeMap<Agent, Known>,
    /// The project's compat manifest as the host last read it (docs/adr/ADR-030.md decision 10); `None`: empty.
    manifest: Option<CompatManifest>,
    /// The project whose session capabilities the facts carry ([`Registry::open_manifest`]).
    root: Option<PathBuf>,
    /// The host epoch the host last named (a snapshot's or observations'): the generations are of it.
    host_epoch: i64,
}

impl State {
    /// Every looked-at agent's session record again, in the project and against the manifest now held.
    fn sessions_again(&mut self, suite_set: &str) {
        let State {
            agents,
            manifest,
            root,
            ..
        } = self;
        let Some(root) = root.as_deref() else {
            return;
        };
        for (agent, known) in agents.iter_mut() {
            if let Some(seen) = known.seen.as_mut()
                && let Ok(ready) = &seen.outcome
            {
                seen.session =
                    Some(ready.session_record(*agent, root, manifest.as_ref(), suite_set));
            }
        }
    }

    /// `agent`'s fact and observation as the registry knows them now: the one constructor of a snapshot's items and of
    /// [`Registry::fact`] (docs/adr/ADR-030.md decision 1, M2T15D-100).
    fn item(&self, agent: Agent, basis: Basis<'_>) -> (Option<Fact>, Option<Observation>) {
        let Some(known) = self.agents.get(&agent) else {
            return (None, None);
        };
        let fact = self.fact(agent, known, basis);
        let observation = known
            .seen
            .as_ref()
            .filter(|_| known.looks > 0)
            .map(|seen| Observation {
                host_epoch: self.host_epoch,
                n: known.looks,
                available: seen.outcome.is_ok(),
                logged_in: known.latched.is_none()
                    && known.login.as_ref().is_none_or(|l| l.missing.is_none()),
                verified: matches!(&fact, Some(Fact::Usable(u)) if u.compat == Compat::Verified),
            });
        (fact, observation)
    }

    /// What `agent`'s last look found, as the evidence of a staffing Block or Switch records it (docs/adr/ADR-030.md
    /// decision 5): its probe's class and detail (a ready agent's: none, or why its fact is not usable), its entry's
    /// version, launcher, hashes and capabilities, its compat verdict and the look's identity `(host_epoch, n)`; `None`
    /// while it was not looked at. Read under the same lock as its fact.
    fn probe(&self, agent: Agent, basis: Basis<'_>) -> Option<serde_json::Value> {
        let known = self.agents.get(&agent)?;
        let seen = known.seen.as_ref()?;
        let observation = serde_json::json!({"host_epoch": self.host_epoch, "n": known.looks});
        Some(match &seen.outcome {
            Err(probe) => serde_json::json!({
                "class": probe.class.name(), "detail": probe.detail, "cli_version": null, "launcher_path": null,
                "implementation_sha256": null, "help_sha256": null, "schema_sha256": null, "capabilities": null,
                "compat": null, "observation": observation,
            }),
            Ok(ready) => {
                let detail = match self.fact(agent, known, basis) {
                    Some(Fact::LoginMissing(why) | Fact::Unsupported(why)) => Some(why),
                    _ => None,
                };
                let entry = &ready.entry;
                let mut evidence = serde_json::json!({
                    "class": null, "detail": detail, "cli_version": entry.cli_version,
                    "launcher_path": entry.launcher_path, "implementation_sha256": entry.implementation_sha256,
                    "help_sha256": entry.help_sha256, "schema_sha256": entry.schema_sha256,
                    "capabilities": entry.capabilities,
                    "compat": self.compat(agent, seen, ready, basis),
                    "observation": observation,
                });
                // codex: this host's facts on its read-only profile, which a Block or Switch for them must show
                // (docs/adr/ADR-032.md decision 4).
                if let Some((check, _)) = &known.host_check {
                    evidence["host_check"] =
                        serde_json::json!({"check": check.name(), "reason": check.reason()});
                }
                if known.degraded() {
                    evidence["exec_degraded"] = serde_json::json!(true);
                }
                evidence
            }
        })
    }

    /// Decision 2's fact of `agent`; `None` while it was not looked at, or while the registry has no project to say
    /// what a session of it would have.
    fn fact(&self, agent: Agent, known: &Known, basis: Basis<'_>) -> Option<Fact> {
        let seen = known.seen.as_ref()?;
        let ready = match &seen.outcome {
            Ok(ready) => ready,
            Err(probe) => return Some(probe.fact()),
        };
        let session = match seen.session.as_ref()? {
            Ok(session) => session,
            Err(why) => return Some(Fact::Unsupported(why.clone())),
        };
        // The status command's "not logged in" first: it says what logging in takes.
        if let Some(why) = known
            .login
            .as_ref()
            .and_then(|l| l.missing.as_ref())
            .or(known.latched.as_ref())
        {
            return Some(Fact::LoginMissing(why.clone()));
        }
        // The session record is the binary's; a host fact only takes from what is granted now (decision 4).
        let recorded = hoh_core::decision::granted_capabilities(session);
        let withheld: BTreeSet<String> =
            known.withheld().intersection(&recorded).cloned().collect();
        Some(Fact::Usable(Box::new(Usable {
            version: ready.version.clone(),
            identity: ready.entry.identity(),
            stamp: ready.stamp,
            generation: (self.host_epoch, ready.look),
            session: session.clone(),
            granted: recorded.difference(&withheld).cloned().collect(),
            withheld,
            compat: self.compat(agent, seen, ready, basis),
        })))
    }

    /// Whether `agent` as the look `seen` found it (`ready`) is verified: decision 9's `compat::verdict` of its entry
    /// against the manifest held, but `Pending` while the agent is not what that look found ([`since`], the one
    /// predicate a look's decision to probe again and prepare's (a) use too: the launcher on the PATH now, the
    /// implementation resolved for it now and their stamps, against the look's; not found or not readable is not it):
    /// only the next look probes what is there now (docs/adr/ADR-030.md decision 1, its implementation corrections in
    /// S6c and M2T15R-01). Read under the registry's lock, so a snapshot, `fact` and `prepared` all see the change as
    /// soon as it is there, and no task is put back for a version the registry has not looked at (decision 13 (b)).
    fn compat(&self, agent: Agent, seen: &Seen, ready: &Ready, basis: Basis<'_>) -> Compat {
        match since(agent, basis.path, seen.files.as_ref()) {
            Ok(_) => compat::verdict(
                self.manifest.as_ref(),
                agent.as_str(),
                &ready.entry,
                basis.suite_set,
            ),
            Err(changed) => Compat::Pending(changed.detail),
        }
    }
}

/// What the registry's facts are judged by: the PATH its looks find the agents on and the suite set their entries are
/// verified under.
#[derive(Debug, Clone, Copy)]
struct Basis<'a> {
    path: &'a OsStr,
    suite_set: &'a str,
}

/// The host's agents, found on its PATH and probed lazily (decision 10).
#[derive(Debug)]
pub struct Registry {
    path: OsString,
    state: Mutex<State>,
    /// The host's stop: every probe command running is killed and none starts (decision 10 v6, M2T07R-202, -300, -450,
    /// -700).
    stopping: probe::ProbeStop,
    /// Where pi keeps its settings, which name the provider its login is checked for (docs/adr/ADR-024.md decision 7).
    pi_dir: Option<PathBuf>,
    /// [`PROBE_TIMEOUT`] and [`AUTH_EVERY`], shorter in the tests.
    probe_timeout: Duration,
    every: Duration,
    /// [`HOST_CHECK_EVERY`], shorter in the tests.
    host_check_every: Duration,
    /// This build's suite set ([`compat::SUITE_SET_SHA256`]); the tests verify against one of their own.
    suite_set: String,
    /// The stop of the compat suite running, if one is: stopped with the registry's (docs/adr/ADR-030.md decision 10,
    /// "有界、停止与崩溃"), and on its own when the suite runs past its limit.
    suite_stop: Mutex<Option<probe::ProbeStop>>,
    /// How many times [`Registry::state`] was taken (a prepare reads it once, decision 1 -100; tests count).
    reads: std::sync::atomic::AtomicU64,
}

impl Registry {
    /// Agents looked for on `path`; pi's settings where pi keeps them (`$PI_CODING_AGENT_DIR`, else `~/.pi/agent`).
    pub fn new(path: OsString) -> Registry {
        Registry::with_pi_dir(path, hoh_agents::login::pi_agent_dir())
    }

    /// Agents looked for on `path`, pi's settings read from `pi_dir` only (tests give a directory of their own,
    /// M2T17R-59).
    pub fn with_pi_dir(path: OsString, pi_dir: Option<PathBuf>) -> Registry {
        Registry {
            path,
            state: Mutex::new(State::default()),
            stopping: probe::ProbeStop::default(),
            pi_dir,
            probe_timeout: PROBE_TIMEOUT,
            every: AUTH_EVERY,
            host_check_every: HOST_CHECK_EVERY,
            suite_set: compat::SUITE_SET_SHA256.as_str().to_owned(),
            suite_stop: Mutex::new(None),
            reads: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// The registry with a probe command's limit and [`AUTH_EVERY`] of a test's.
    #[cfg(test)]
    fn with_timings(mut self, probe_timeout: Duration, every: Duration) -> Registry {
        self.probe_timeout = probe_timeout;
        self.every = every;
        self
    }

    /// The registry with a test's [`HOST_CHECK_EVERY`] (the integration tests' too: TEST-021-06 shortens it).
    #[doc(hidden)]
    pub fn with_host_check_every(mut self, every: Duration) -> Registry {
        self.host_check_every = every;
        self
    }

    /// The registry verifying against a test's suite set instead of this build's ([`compat::SUITE_SET_SHA256`]).
    #[cfg(test)]
    fn with_suite_set(mut self, suite_set: &str) -> Registry {
        self.suite_set = suite_set.to_owned();
        self
    }

    /// What its facts are judged by: its PATH, the one its looks find the agents on, and its suite set.
    fn basis(&self) -> Basis<'_> {
        Basis {
            path: &self.path,
            suite_set: &self.suite_set,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The host stops: every probe command in its slot is killed (its group and its leader) before this returns, one
    /// still being spawned by its starter as it puts it in its slot, and none starts; it never waits on a probing
    /// thread: a lock still held when the probe's STOP_WAIT (200 ms) has passed is left, with its commands, to the
    /// probes' own looks at the stop (`probe::ProbeStop`); what a probing thread that is not run leaves alive is the
    /// residual of decision 10 v6 (M2T07R-300, -450, -500, -700, -853).
    pub fn stop(&self) {
        self.stopping.stop();
        let running = self
            .suite_stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(suite) = running {
            suite.stop();
        }
    }

    /// The stop of a compat suite about to run (docs/adr/ADR-030.md decision 10): its own, so the host can end it past
    /// its limit, and registered here, so the host's stop ends it too (already stopped if the host is stopping).
    pub(super) fn suite_stop(&self) -> probe::ProbeStop {
        let stop = probe::ProbeStop::default();
        *self
            .suite_stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(stop.clone());
        if self.stopping.is_stopped() {
            stop.stop();
        }
        stop
    }

    /// What a compat suite runs with: the probe's PATH and per-command limit (decision 10: each command 20 s); its
    /// own stop is set by the suite ([`compat::suite::run`]).
    pub(super) fn suite_env(&self) -> ProbeEnv {
        ProbeEnv {
            cancel: None,
            ..self.env()
        }
    }

    /// The wanted agents whose last look found them ready and whose verdict is `Pending`, in name order, with that look:
    /// the ones a compat suite is due for (decision 10, "何时运行与互斥"). `Failed` and `Undecided` are not run again for
    /// the same identity; another identity or this build's suite set makes them pending again.
    pub(super) fn pending_suites(&self) -> Vec<(Agent, Ready)> {
        let state = self.state();
        state
            .wanted
            .iter()
            .filter_map(|agent| {
                let ready = state
                    .agents
                    .get(agent)?
                    .seen
                    .as_ref()?
                    .outcome
                    .as_ref()
                    .ok()?;
                let verdict = compat::verdict(
                    state.manifest.as_ref(),
                    agent.as_str(),
                    &ready.entry,
                    &self.suite_set,
                );
                matches!(verdict, Compat::Pending(_)).then(|| (*agent, ready.clone()))
            })
            .collect()
    }

    /// The manifest the registry holds, as JSON (none: empty): the base a publication replaces one entry of.
    pub(super) fn manifest(&self) -> Option<serde_json::Value> {
        self.state()
            .manifest
            .as_ref()
            .and_then(|m| serde_json::to_value(m).ok())
    }

    /// Hold `manifest`, which a publication just put in place (decision 10: only after it committed), as the one the
    /// facts are verified against; one that is not a manifest is refused and the one held is kept.
    pub(super) fn adopt(&self, manifest: serde_json::Value) -> Result<(), String> {
        let manifest: CompatManifest =
            serde_json::from_value(manifest).map_err(|e| e.to_string())?;
        let mut state = self.state();
        state.manifest = Some(manifest);
        // Its read_only cases are part of the session records (docs/adr/ADR-032.md decision 4, item 3).
        state.sessions_again(&self.suite_set);
        Ok(())
    }

    /// The agents named `names` are the ones to look at from now on (names that are not agents are left out).
    pub fn want<'a>(&self, names: impl IntoIterator<Item = &'a String>) {
        let agents: BTreeSet<Agent> = names.into_iter().filter_map(|n| Agent::named(n)).collect();
        self.state().wanted = agents;
    }

    /// Look at every wanted agent now (blocking: probed again only when its files changed, or a temporary failure's
    /// [`AUTH_EVERY`] passed).
    pub fn refresh(&self) {
        let wanted: Vec<Agent> = self.state().wanted.iter().copied().collect();
        for agent in wanted {
            let _ = self.look(agent);
        }
    }

    /// Read the project's compat manifest at `root` into the registry, the one the facts are verified against
    /// (docs/adr/ADR-030.md decision 10; the host calls it as it starts): missing, it is empty; unreadable or not valid
    /// against the locked schema, it is empty too and the diagnostic returned says why, for the host to record. The
    /// temporary files a publish that crashed left beside it are deleted. `root` is the project the facts' session
    /// capabilities are for.
    pub fn open_manifest(&self, root: &Path) -> Option<Diagnostic> {
        if let Ok(entries) = std::fs::read_dir(root.join(".hoh")) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(compat::is_manifest_temp)
                {
                    // Best effort: a leftover kept is never read, and the next start deletes it.
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        let (manifest, said) = match read_manifest(&compat::manifest_path(root)) {
            Ok(manifest) => (manifest, None),
            Err(detail) => (
                None,
                Some(Diagnostic {
                    kind: MANIFEST_UNREADABLE,
                    detail,
                }),
            ),
        };
        let mut state = self.state();
        state.manifest = manifest;
        state.root = Some(root.to_path_buf());
        state.sessions_again(&self.suite_set);
        said
    }

    /// Look at `agent`'s login (installed: `ready`) unless it was looked at within [`AUTH_EVERY`]: its status command,
    /// under the host's stop like every probe command (decision 7). A look the stop ended says nothing. A clear
    /// "logged in" after a clear "not logged in" is a recovery (docs/adr/ADR-030.md decision 2).
    fn look_at_login(&self, agent: Agent, ready: &Ready) {
        let recent = self
            .state()
            .agents
            .get(&agent)
            .and_then(|known| known.login.as_ref())
            .is_some_and(|login| login.at.elapsed() < self.every);
        if recent {
            return;
        }
        let checked = login::status(agent, &ready.launcher, &self.env(), self.pi_dir.as_deref());
        if self.stopping.is_stopped() {
            return;
        }
        // Only a clear "not logged in" is missing; one that says nothing clear, or could not be run, is not (as
        // `login::missing`).
        let (said, missing) = match checked {
            Ok((out, provider)) => {
                let said = login::conclusion(agent, &out);
                let missing =
                    (said == Some(Login::Missing)).then(|| login::hint(agent, provider.as_deref()));
                (said, missing)
            }
            Err(_) => (None, None),
        };
        let mut state = self.state();
        let known = state.agents.entry(agent).or_default();
        known.looks += 1;
        if said == Some(Login::LoggedIn) && known.said == Some(Login::Missing) {
            known.recovered_at = known.looks;
            known.latched = None;
        }
        if said.is_some() {
            known.said = said;
        }
        known.login = Some(LoginSeen {
            at: Instant::now(),
            missing,
        });
    }

    /// What was last seen of every agent, without looking (decision 8): those found available, those found unavailable
    /// with why; any other was not looked at yet.
    pub fn availability(&self) -> hoh_core::persistence::Availability {
        let mut known = hoh_core::persistence::Availability {
            available: Some(BTreeSet::new()),
            unavailable: BTreeMap::new(),
            auth_missing: BTreeMap::new(),
        };
        let state = self.state();
        for (agent, about) in &state.agents {
            let name = agent.as_str().to_owned();
            // A turn's BLOCKED_AUTH latched is a missing login as much as the status command's (decision 2).
            if let Some(why) = about
                .login
                .as_ref()
                .and_then(|l| l.missing.as_ref())
                .or(about.latched.as_ref())
            {
                known.auth_missing.insert(name.clone(), why.clone());
            }
            match about.seen.as_ref().map(|seen| &seen.outcome) {
                // An agent counts as available only once its login was looked at too: before that, a change to it
                // waits as for an agent not looked at yet, so it never takes effect ahead of the login check
                // (decision 7).
                Some(Ok(_)) if about.login.is_some() => {
                    if let Some(available) = known.available.as_mut() {
                        available.insert(name);
                    }
                }
                Some(Err(probe)) => {
                    known.unavailable.insert(name, probe.detail.clone());
                }
                Some(Ok(_)) | None => {}
            }
        }
        known
    }

    fn env(&self) -> ProbeEnv {
        ProbeEnv {
            path: self.path.clone(),
            fixture_root: None,
            timeout: self.probe_timeout,
            log: None,
            deadline: None,
            cancel: Some(self.stopping.clone()),
            spawner: hoh_core::spawn::Direct::shared(),
        }
    }

    /// `agent` as it is now: probed again when it is not what its last look found ([`since`]: the launcher on the PATH,
    /// the implementation resolved for it, their stamps, change time included) or a temporary failure's
    /// [`AUTH_EVERY`] passed, else as last seen. Blocking: run it outside the writer and off the async workers.
    pub fn look(&self, agent: Agent) -> Result<Ready, String> {
        let env = self.env();
        let (files, outcome, looked) = match self.cached(agent, &env.path) {
            Look::Cached(outcome) => (None, *outcome, false),
            Look::Probe(files) => {
                let outcome = probed(agent, &files, &env);
                (Some(files), outcome, true)
            }
            Look::Missing(error) => (None, Err(Probe::of(&error)), true),
        };
        // A look the host's stop ended says nothing of the agent: not recorded, so it stays as last seen (or not looked
        // at), and no dispatch takes the stop for the agent being unavailable (M2T07R-352).
        if self.stopping.is_stopped() {
            return outcome.map_err(|probe| probe.detail);
        }
        // Its login too, once it is usable (decision 7): looked at before the agent is recorded, so an agent seen
        // usable always has its login looked at.
        if let Ok(ready) = &outcome {
            self.look_at_login(agent, ready);
            if self.stopping.is_stopped() {
                return outcome.map_err(|probe| probe.detail);
            }
            self.look_at_sandbox(agent, ready, looked);
            if self.stopping.is_stopped() {
                return outcome.map_err(|probe| probe.detail);
            }
        }
        let outcome = if looked {
            self.record(agent, files, outcome)
        } else {
            outcome
        };
        outcome.map_err(|probe| probe.detail)
    }

    /// codex on its App Server: this host's check of its own read-only sandbox ([`probe::codex_sandbox_check`],
    /// docs/adr/ADR-032.md decision 4, -101), under the host's stop like every probe command. Run at the first look of
    /// this host, on an actual look (its files changed), and while it is not confined at most every
    /// [`HOST_CHECK_EVERY`]; never in a tick (the registry looks off it). A check the stop ended says nothing.
    fn look_at_sandbox(&self, agent: Agent, ready: &Ready, probed: bool) {
        if agent != Agent::Codex
            || ready.entry.transport != hoh_agents::compat::Transport::AppServer
        {
            return;
        }
        let due = self
            .state()
            .agents
            .get(&agent)
            .and_then(|known| known.host_check.as_ref())
            .is_none_or(|(check, at)| {
                probed
                    || (*check != probe::SandboxCheck::Confined
                        && at.elapsed() >= self.host_check_every)
            });
        if !due {
            return;
        }
        let check = probe::codex_sandbox_check(&ready.launcher, &self.env());
        if self.stopping.is_stopped() {
            return;
        }
        self.state().agents.entry(agent).or_default().host_check = Some((check, Instant::now()));
    }

    /// The executor's word that `agent`'s App Server degraded to exec in this host's run (docs/adr/ADR-032.md decision
    /// 10 (A)): for the identity `identity` it ran as, the read-only profile is withheld from it until another identity
    /// is found or the host restarts. A word for an older identity does nothing.
    pub fn exec_degraded(&self, agent: Agent, identity: AgentIdentity) {
        let mut state = self.state();
        let known = state.agents.entry(agent).or_default();
        if known.identity.as_ref() == Some(&identity) {
            known.exec_degraded = Some(identity);
        }
    }

    /// What a look at `agent` does now, on `path` (the registry's PATH): what was last seen of it while it is what that
    /// look found ([`since`]) and that is no temporary failure [`AUTH_EVERY`] old; else probe its files found now, or
    /// none were found. Read under the registry's lock (`since` runs no process).
    fn cached(&self, agent: Agent, path: &OsStr) -> Look {
        let mut state = self.state();
        let State {
            agents,
            manifest,
            root,
            ..
        } = &mut *state;
        let seen = agents.get_mut(&agent).and_then(|known| known.seen.as_mut());
        let last = seen.as_ref().and_then(|seen| seen.files.as_ref());
        let files = match since(agent, path, last) {
            Ok(files) => files,
            Err(changed) => {
                return match changed.now {
                    Ok(files) => Look::Probe(files),
                    Err(error) => Look::Missing(error),
                };
            }
        };
        // Current only against a look's files, so there was one.
        let Some(seen) = seen else {
            return Look::Probe(files);
        };
        if let Err(probe) = &seen.outcome
            && probe.class.temporary()
            && seen.at.elapsed() >= self.every
        {
            return Look::Probe(files);
        }
        if let (Ok(ready), None, Some(root)) = (&seen.outcome, &seen.session, &root) {
            seen.session =
                Some(ready.session_record(agent, root, manifest.as_ref(), &self.suite_set));
        }
        Look::Cached(Box::new(seen.outcome.clone()))
    }

    /// Record an actual look at `agent`: its `n` moves; a ready agent gets its session capabilities in the project and,
    /// when its identity is not the one last found, its recovery (docs/adr/ADR-030.md decision 2).
    fn record(
        &self,
        agent: Agent,
        files: Option<Files>,
        mut outcome: Result<Ready, Probe>,
    ) -> Result<Ready, Probe> {
        let mut state = self.state();
        let State {
            agents,
            manifest,
            root,
            ..
        } = &mut *state;
        let known = agents.entry(agent).or_default();
        known.looks += 1;
        let mut session = None;
        if let Ok(ready) = &mut outcome {
            ready.look = known.looks;
            let identity = ready.entry.identity();
            if known
                .identity
                .as_ref()
                .is_some_and(|last| *last != identity)
            {
                known.recovered_at = known.looks;
                known.latched = None;
            }
            known.identity = Some(identity);
            session = root
                .as_deref()
                .map(|root| ready.session_record(agent, root, manifest.as_ref(), &self.suite_set));
        }
        known.seen = Some(Seen {
            files,
            at: Instant::now(),
            outcome: outcome.clone(),
            session,
        });
        outcome
    }

    /// The last actual look at each agent this registry looked at, with its identity `(host_epoch, n)`
    /// (docs/adr/ADR-025.md decision 5): available when its probe succeeded, logged in unless its login check said
    /// otherwise or a turn's BLOCKED_AUTH is latched, verified when its fact is usable and verified.
    pub fn observations(&self, host_epoch: i64) -> BTreeMap<String, Observation> {
        let mut state = self.state();
        state.host_epoch = host_epoch;
        state
            .agents
            .keys()
            .filter_map(|agent| {
                let observation = state.item(*agent, self.basis()).1?;
                Some((agent.as_str().to_owned(), observation))
            })
            .collect()
    }

    /// Each wanted agent's fact and observation, read under the registry's lock in one go (docs/adr/ADR-030.md
    /// decision 1); an agent not looked at yet has neither.
    pub fn snapshot(&self, host_epoch: i64) -> Arc<Snapshot> {
        let mut state = self.state();
        state.host_epoch = host_epoch;
        let mut snapshot = Snapshot::default();
        for agent in &state.wanted {
            let (fact, observation) = state.item(*agent, self.basis());
            let name = agent.as_str().to_owned();
            if let Some(fact) = fact {
                snapshot.facts.insert(name.clone(), fact);
            }
            if let Some(probe) = state.probe(*agent, self.basis()) {
                snapshot.probes.insert(name.clone(), probe);
            }
            if let Some(observation) = observation {
                snapshot.observations.insert(name, observation);
            }
        }
        Arc::new(snapshot)
    }

    /// The tick's `snapshot`'s observations as the supervisor and its allocation judge by them (docs/adr/ADR-030.md
    /// decision 11, the note of 2026-11-17), read under the registry's lock in one go: an agent verified in the
    /// snapshot stays so only while its fact now (the same constructor, its files' stamps read again) is still usable,
    /// verified and of the snapshot's identity. Another file on its path, another identity looked at, or another
    /// identity verified since (its binding not yet rebound by staffing, which judged by the snapshot) make it not
    /// verified here, so a wake owed meanwhile is deferred rather than given a turn its start refuses. Nothing the
    /// snapshot did not find verified is made so; the other fields are the snapshot's.
    pub fn still_verified(&self, snapshot: &Snapshot) -> BTreeMap<String, Observation> {
        let state = self.state();
        snapshot
            .observations
            .iter()
            .map(|(name, observation)| {
                let mut observation = *observation;
                if observation.verified {
                    let now =
                        Agent::named(name).and_then(|agent| state.item(agent, self.basis()).0);
                    observation.verified = matches!(
                        (&now, snapshot.facts.get(name)),
                        (Some(Fact::Usable(now)), Some(Fact::Usable(then)))
                            if now.compat == Compat::Verified && now.identity == then.identity
                    );
                }
                (name.clone(), observation)
            })
            .collect()
    }

    /// `agent`'s fact now, as a snapshot taken now would have it (the same constructor, M2T15D-100); `None` while it
    /// was not looked at.
    pub fn fact(&self, agent: Agent) -> Option<Fact> {
        self.state().item(agent, self.basis()).0
    }

    /// A turn that `ran` as it says failed BLOCKED_AUTH (docs/adr/ADR-030.md decision 2, M2T15D-07): latched as the
    /// agent's `LoginMissing` fact when it ran under the agent's current identity and no earlier than its latest
    /// recovery, until the next recovery (a look finding another identity, a visible logged-out then logged-in) or the
    /// host's restart; a look that only says "logged in" does not clear it. Otherwise it is an older identity's or from
    /// before the recovery: the fact is left as it is, and the diagnostic returned says so for the host to record.
    pub fn turn_auth_failed(&self, ran: &Ran, attempt_id: &str) -> Option<Diagnostic> {
        let mut state = self.state();
        let epoch = state.host_epoch;
        let ignored = |detail: String| {
            Some(Diagnostic {
                kind: TURN_AUTH_IGNORED,
                detail,
            })
        };
        let Some(known) = state.agents.get_mut(&ran.agent) else {
            return ignored(format!(
                "turn {attempt_id} failed BLOCKED_AUTH; {} was not looked at",
                ran.agent.as_str()
            ));
        };
        let current = known
            .seen
            .as_ref()
            .and_then(|seen| seen.outcome.as_ref().ok())
            .map(|ready| ready.entry.identity());
        let recovered = (epoch, known.recovered_at);
        match current {
            Some(identity) if identity == ran.identity && ran.generation >= recovered => {
                known.latched = Some(format!("turn {attempt_id} failed BLOCKED_AUTH"));
                None
            }
            Some(identity) if identity == ran.identity => ignored(format!(
                "turn {attempt_id} failed BLOCKED_AUTH; it started at generation {:?}, before {}'s recovery at {recovered:?}",
                ran.generation,
                ran.agent.as_str()
            )),
            Some(identity) => ignored(format!(
                "turn {attempt_id} failed BLOCKED_AUTH under {:?}; {} is now {identity:?}",
                ran.identity,
                ran.agent.as_str()
            )),
            None => ignored(format!(
                "turn {attempt_id} failed BLOCKED_AUTH; {} was not found ready at its last look",
                ran.agent.as_str()
            )),
        }
    }

    /// `agent` as a turn prepared now finds it, read under the registry's lock in one go (docs/adr/ADR-030.md decisions
    /// 1, 2 and 9, M2T15D-07, -100): its last look, what the turn runs as (that look's identity and the registry's
    /// generation `(host_epoch, n)` of the agent now), its fact F (the constructor of [`Registry::fact`] and the
    /// snapshot's) and the verdict of the look's entry against the manifest the registry holds. `None` if it never was
    /// looked at (or the registry has no project yet); why its last look found it unusable.
    pub fn prepared(&self, agent: Agent) -> Option<Result<Current, String>> {
        let state = self.state();
        let known = state.agents.get(&agent)?;
        let seen = known.seen.as_ref()?;
        let ready = match &seen.outcome {
            Ok(ready) => ready,
            Err(probe) => return Some(Err(probe.detail.clone())),
        };
        let basis = self.basis();
        let fact = state.item(agent, basis).0?;
        let compat = match &fact {
            Fact::Usable(usable) => usable.compat.clone(),
            _ => state.compat(agent, seen, ready, basis),
        };
        let changed = since(agent, basis.path, seen.files.as_ref())
            .err()
            .map(|changed| changed.detail);
        Some(Ok(Current {
            ready: ready.clone(),
            ran: Ran {
                agent,
                identity: ready.entry.identity(),
                generation: (state.host_epoch, known.looks),
            },
            fact,
            compat,
            changed,
            host_check: known.host_check.as_ref().map(
                |(check, _)| serde_json::json!({"check": check.name(), "reason": check.reason()}),
            ),
            exec_only: known.degraded(),
        }))
    }

    /// How many times the registry's state was read (its lock taken): a test's count of a prepare's reads.
    #[cfg(test)]
    pub(super) fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// `agent` as it was last looked at, without looking again; `None` if it never was.
    pub fn seen(&self, agent: Agent) -> Option<Result<Ready, String>> {
        self.state()
            .agents
            .get(&agent)?
            .seen
            .as_ref()
            .map(|seen| seen.outcome.clone().map_err(|probe| probe.detail))
    }
}

/// The stamps of `launcher` and `implementation` now (`stat`, following links), if both can be read: what a compat
/// suite's end compares with the look it ran for (decision 10, "结束时另重读身份戳").
pub(crate) fn stamp_now(launcher: &Path, implementation: &Path) -> Option<Stamp> {
    key(launcher, implementation).ok().map(|key| stamp_of(&key))
}

/// `agent` found on `env`'s PATH and probed as a look does, without its login check: its entry and the stamps read
/// before the probe ([`crate::compat::register`]).
pub(crate) fn probed_entry(agent: Agent, env: &ProbeEnv) -> Result<(AgentEntry, Stamp), String> {
    let launcher = probe::find_on_path(agent.command(), &env.path)
        .ok_or_else(|| ProbeError::NotFound { agent }.to_string())?;
    let implementation =
        probe::resolve_implementation(agent, &launcher).map_err(|e| e.to_string())?;
    let key = key(&launcher, &implementation).map_err(|e| e.to_string())?;
    let entry = probe::probe(agent, env).map_err(|e| e.to_string())?;
    Ok((entry, stamp_of(&key)))
}

/// Probe `agent`, whose `files` a look just found: its entry and launch facts. The probe finds the agent again: an entry
/// of other files than the look's (they changed in between, or their paths are not UTF-8 and the entry's are not
/// theirs) is no fact of the look's, which [`since`] could ever call current; it is a temporary failure, probed again
/// once the files are seen to change or [`AUTH_EVERY`] passed.
fn probed(agent: Agent, files: &Files, env: &ProbeEnv) -> Result<Ready, Probe> {
    let entry = probe::probe(agent, env).map_err(|e| Probe::of(&e))?;
    if Path::new(&entry.launcher_path) != files.launcher
        || Path::new(&entry.implementation_path) != files.implementation
    {
        return Err(Probe {
            class: ProbeClass::Failed,
            detail: format!(
                "{}'s files changed while it was probed: the look found {} and {}, the probe {} and {}",
                agent.as_str(),
                files.launcher.display(),
                files.implementation.display(),
                entry.launcher_path,
                entry.implementation_path
            ),
        });
    }
    // The same commands the probe just ran: their failure now is a temporary one.
    let facts = launch::read(agent, &files.launcher, env).map_err(|detail| Probe {
        class: ProbeClass::Failed,
        detail,
    })?;
    Ok(Ready {
        launcher: files.launcher.clone(),
        version: entry.cli_version.clone(),
        capabilities: entry.capabilities.clone(),
        facts,
        entry,
        stamp: stamp_of(&files.key),
        look: 0,
    })
}

// The read_only case's status: hoh-agents', which the client's doctor reads too without naming the host (ADR-031,
// 设计修正 2, the client's crate boundary).
pub use hoh_agents::compat::read_only_case;

/// The compat manifest at `path`: `None` when there is none; why when it cannot be read or is not valid against the
/// locked schema.
pub(crate) fn read_manifest(path: &Path) -> Result<Option<CompatManifest>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    hoh_core::schema::compat_manifest()
        .validate(&value)
        .map_err(|violations| {
            let first: Vec<String> = violations
                .iter()
                .take(3)
                .map(|v| format!("{} {}", v.instance_path, v.message))
                .collect();
            format!("{}: not schema-valid: {}", path.display(), first.join("; "))
        })?;
    serde_json::from_value(value)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The capabilities every role needs, and those a role following `contract`'s role contract needs too (REQ-020
/// decision 1, the part that has names; decision 10): structured events always, resume for the commander and the
/// reviewer. One function for the dispatch and for staffing's judgment (docs/adr/ADR-030.md decisions 3 and 12), kept
/// in `hoh_core::staffing` so the store's staffing read gives each binding its role's.
pub use hoh_core::staffing::required;

/// What a turn of an agent is given besides what its registry entry says (decisions 11, 12 and 15).
#[derive(Debug, Clone)]
pub(super) struct Spec {
    /// The project: the agent's working directory.
    pub cwd: PathBuf,
    pub model: Option<String>,
    /// The agent's own session to continue (Claude's session, Codex's thread, Pi's session id).
    pub resume: Option<String>,
    pub prompt: String,
    /// The turn's raw logs.
    pub logs: PathBuf,
    /// Pi's session directory and, for a new session, the id HoH gives it (kept for every turn of the session).
    pub pi_session: (PathBuf, String),
    /// Codex's App Server is not to be used: it degraded on this implementation (decision 15).
    pub exec_only: bool,
    /// The launch profile: `ReadOnly` for a supervision turn, `Yolo` for every other (docs/adr/ADR-032.md decision 2).
    pub profile: hoh_agents::Profile,
    /// How long a native cancel and then SIGTERM may take before the adapter's next step (execution's
    /// cancel_native_grace_seconds and cancel_term_grace_seconds).
    pub graces: (Duration, Duration),
}

/// A turn of one of the agents, ready to start.
#[derive(Debug)]
pub(super) enum Prepared {
    Claude(hoh_agents::claude::Turn),
    Codex(hoh_agents::codex::Turn),
    Pi(hoh_agents::pi::Turn),
}

impl Ready {
    /// A turn of `agent` as `spec` says, under its profile; or why this agent's facts are not its own.
    pub(super) fn turn(&self, agent: Agent, spec: Spec) -> Result<Prepared, String> {
        let graced = |mut escalation: hoh_agents::process::Escalation| {
            escalation.native_grace = spec.graces.0;
            escalation.term_grace = spec.graces.1;
            escalation
        };
        Ok(match (agent, &self.facts) {
            (Agent::Claude, launch::Facts::Claude { help }) => {
                Prepared::Claude(hoh_agents::claude::Turn {
                    launch: hoh_agents::claude::Launch {
                        program: self.launcher.clone(),
                        help: help.clone(),
                        cwd: spec.cwd.clone(),
                        profile: spec.profile,
                        partial: false,
                        resume: spec.resume.clone(),
                        model: spec.model.clone(),
                        root: rustix::process::geteuid().is_root(),
                        capabilities: self.capabilities.clone(),
                    },
                    prompt: spec.prompt.clone().into_bytes(),
                    logs: spec.logs.clone(),
                    escalation: graced(hoh_agents::claude::Turn::default_escalation()),
                    deadline: None,
                    observe: None,
                    spawner: hoh_core::spawn::Direct::shared(),
                })
            }
            (
                Agent::Codex,
                launch::Facts::Codex {
                    app_server_help,
                    exec_help,
                    exec_resume_help,
                    known,
                    requests,
                    client_notifications,
                },
            ) => Prepared::Codex(hoh_agents::codex::Turn {
                launch: hoh_agents::codex::Launch {
                    program: self.launcher.clone(),
                    app_server_help: app_server_help.clone().filter(|_| !spec.exec_only),
                    exec_help: exec_help.clone(),
                    exec_resume_help: exec_resume_help.clone(),
                    cwd: spec.cwd.clone(),
                    profile: spec.profile,
                    resume: spec.resume.clone(),
                    model: spec.model.clone(),
                    known: known.clone(),
                    requests: requests.clone(),
                    client_notifications: client_notifications.clone(),
                    capabilities: self.capabilities.clone(),
                },
                prompt: spec.prompt.clone(),
                logs: spec.logs.clone(),
                escalation: graced(hoh_agents::codex::Turn::default_escalation()),
                handshake_timeout: Duration::from_secs(60),
                deadline: None,
                observe: None,
                spawner: hoh_core::spawn::Direct::shared(),
            }),
            (Agent::Pi, launch::Facts::Pi { help }) => Prepared::Pi(hoh_agents::pi::Turn {
                launch: hoh_agents::pi::Launch {
                    program: self.launcher.clone(),
                    help: help.clone(),
                    cwd: spec.cwd.clone(),
                    session_dir: spec.pi_session.0.clone(),
                    session_id: spec
                        .resume
                        .clone()
                        .unwrap_or_else(|| spec.pi_session.1.clone()),
                    resume: spec.resume.is_some(),
                    provider: None,
                    model: spec.model.clone(),
                    settled_event: hoh_agents::pi::emits_settled(Some(&self.version)),
                    profile: spec.profile,
                    capabilities: self.capabilities.clone(),
                },
                prompt: spec.prompt.clone(),
                input_id: None,
                logs: spec.logs.clone(),
                escalation: graced(hoh_agents::pi::Turn::default_escalation()),
                deadline: None,
                clock: Arc::new(hoh_agents::clock::SystemClock),
                events: None,
                observe: None,
                spawner: hoh_core::spawn::Direct::shared(),
            }),
            _ => {
                return Err(format!(
                    "{}'s launch facts are another agent's",
                    agent.as_str()
                ));
            }
        })
    }

    /// The session record of this agent in the project at `cwd` (decision 9): [`Ready::session_capabilities`], and once
    /// `manifest` has an entry verifying it under `suite_set`, its read_only_workspace true only while that entry's
    /// read_only case passed: a failed one makes it false, any other (not run, no such case) unknown. So a verified
    /// agent's is the probe rule, the read-only builder and the case together, the binary's facts
    /// (docs/adr/ADR-032.md decision 4, items 1 to 3); a host's facts never touch it. Before its suite verified it the
    /// agent's compat verdict is what waits (ADR-030 decision 11), not its capabilities.
    pub fn session_record(
        &self,
        agent: Agent,
        cwd: &Path,
        manifest: Option<&CompatManifest>,
        suite_set: &str,
    ) -> Result<serde_json::Value, String> {
        let mut session = self.session_capabilities(agent, cwd)?;
        if session.get(READ_ONLY_WORKSPACE) == Some(&serde_json::Value::Bool(true))
            && compat::verdict(manifest, agent.as_str(), &self.entry, suite_set) == Compat::Verified
        {
            session[READ_ONLY_WORKSPACE] =
                match read_only_case(manifest, agent, &self.entry, suite_set) {
                    Some(compat::SuiteStatus::Pass) => serde_json::Value::Bool(true),
                    Some(compat::SuiteStatus::Fail) => serde_json::Value::Bool(false),
                    _ => serde_json::Value::Null,
                };
        }
        Ok(session)
    }

    /// What a session of this agent has (decision 9), for a turn in `cwd` under the YOLO profile; or why no turn of it
    /// can start.
    pub fn session_capabilities(
        &self,
        agent: Agent,
        cwd: &Path,
    ) -> Result<serde_json::Value, String> {
        let spec = Spec {
            cwd: cwd.to_path_buf(),
            model: None,
            resume: None,
            prompt: String::new(),
            logs: cwd.to_path_buf(),
            pi_session: (cwd.join(".hoh/sessions"), String::new()),
            exec_only: false,
            graces: (Duration::from_secs(10), Duration::from_secs(5)),
            // The session row is measured under the YOLO launch whatever a turn's profile (ADR-032 decision 4).
            profile: hoh_agents::Profile::Yolo,
        };
        match self.turn(agent, spec)? {
            Prepared::Claude(turn) => {
                hoh_agents::claude::session_capabilities(&turn.launch).map_err(|e| e.0)
            }
            Prepared::Codex(turn) => {
                hoh_agents::codex::session_capabilities(&turn).map_err(|e| e.0)
            }
            Prepared::Pi(turn) => {
                hoh_agents::pi::session_capabilities(&turn.launch).map_err(|e| e.0)
            }
        }
    }
}

/// Staffing (docs/adr/ADR-030.md decisions 1, 4 and 5): each binding of `staffing` judged against `snapshot`, the
/// registry's one snapshot of this tick, and what the judgment writes handed to the store, which writes it only while
/// the binding is still the one judged. A Block or a Switch first keeps its evidence, every candidate evaluated with
/// what its look found, as an object of the project at `root`; when it cannot be written the judgment is not written
/// this tick (no evidence, no switch and no block; the next tick judges again). A judgment the binding already holds
/// (the same block, the same change pending) writes nothing. It reads what the registry last saw: no probe runs in a
/// tick or a writer job. Whether anything was written.
pub(super) async fn staff(
    store: &Store,
    root: &Path,
    snapshot: &Snapshot,
    staffing: Vec<Staffed>,
    held: &mut super::runner::Held,
) -> Result<bool, super::runner::TickError> {
    let mut wrote = false;
    for staffed in staffing {
        // A run found stopped this tick (Held) is not staffed again in it (docs/adr/ADR-031.md correction D rule 2 (b)).
        if held.contains(&staffed.run_id) {
            continue;
        }
        let judgment = hoh_core::staffing::judge(&staffed, &snapshot.facts, &staffed.excluded);
        let Some(write) = write_of(&staffed, judgment, snapshot, root).await else {
            continue;
        };
        // One binding's write: a run that stopped since the tick's look is Held, its binding skipped and the run
        // noted; the other bindings are staffed in this tick all the same (M2T13R-01).
        wrote |= held
            .item(
                store
                    .staff(StaffChange {
                        run_id: staffed.run_id,
                        role: staffed.role,
                        role_epoch: staffed.role_epoch,
                        agent: staffed.agent,
                        write,
                    })
                    .await,
            )?
            .unwrap_or(false);
    }
    Ok(wrote)
}

/// What `judgment` of `staffed` writes, if anything: none when the binding already holds it, or when the evidence of a
/// Block or a Switch could not be kept.
async fn write_of(
    staffed: &Staffed,
    judgment: Judgment,
    snapshot: &Snapshot,
    root: &Path,
) -> Option<StaffWrite> {
    let c = &staffed.agent;
    let verdict =
        hoh_core::staffing::verdict(c, &staffed.required, &snapshot.facts, &staffed.excluded);
    let pending = |cause: Cause, to: &str| {
        staffed
            .pending
            .as_ref()
            .is_some_and(|p| p.cause == cause && p.to == to)
    };
    let blocked_for = |reason: hoh_core::protocol::BlockedReason| {
        staffed.state == BindingState::Blocked && staffed.blocked_reason == Some(reason)
    };
    match judgment {
        // The bound agent usable and its session fresh: a change still pending is withdrawn (decision 5).
        Judgment::Nothing => (verdict.kind() == Kind::Usable && staffed.pending.is_some())
            .then_some(StaffWrite::Unblock),
        Judgment::Grant => match snapshot.facts.get(c) {
            Some(Fact::Usable(usable)) => Some(StaffWrite::Grant {
                agent_version: usable.version.clone(),
                capabilities: usable.session.clone(),
                identity: usable.identity.clone(),
            }),
            _ => None,
        },
        Judgment::Unblock => Some(StaffWrite::Unblock),
        Judgment::Rebind { .. } => {
            (!(staffed.live && pending(Cause::Rebind, c))).then_some(StaffWrite::Rebind)
        }
        Judgment::Block { reason, missing } => {
            if blocked_for(reason) && staffed.missing == missing && staffed.pending.is_none() {
                return None;
            }
            let evidence = evidence(root, snapshot, &missing).await?;
            Some(StaffWrite::Block {
                reason,
                missing,
                evidence,
            })
        }
        Judgment::Switch {
            to,
            model,
            evaluated,
        } => {
            let reason = verdict.reason()?;
            if staffed.live && blocked_for(reason) && pending(Cause::Fallback, &to) {
                return None;
            }
            let line = verdict.line(c);
            let mut lines = evaluated.clone();
            if !lines.iter().any(|l| agent_of(l) == c.as_str()) {
                lines.insert(0, line.clone());
            }
            let evidence = evidence(root, snapshot, &lines).await?;
            Some(StaffWrite::Switch {
                to,
                model,
                reason,
                line,
                evaluated,
                evidence,
            })
        }
    }
}

/// The agent a verdict's line `<agent>:<code>` names.
fn agent_of(line: &str) -> &str {
    line.split_once(':').map_or(line, |(agent, _)| agent)
}

/// The evidence of a judgment (decision 5): `{agent, line, probe}` of each candidate `lines` evaluated, in order, as
/// canonical JSON kept as an object of the project at `root`; `None` when it could not be written.
async fn evidence(root: &Path, snapshot: &Snapshot, lines: &[String]) -> Option<Evidence> {
    let items: Vec<serde_json::Value> = lines
        .iter()
        .map(|line| {
            let agent = agent_of(line);
            serde_json::json!({
                "agent": agent, "line": line,
                "probe": snapshot.probes.get(agent).cloned().unwrap_or(serde_json::Value::Null),
            })
        })
        .collect();
    let bytes = compat::canonical(&serde_json::Value::Array(items));
    let root = root.to_path_buf();
    let size = bytes.len() as u64;
    let sha256 = super::begun::spawn_blocking(move || hoh_core::objects::put(&root, &bytes))
        .await
        .ok()?
        .ok()?;
    Some(Evidence {
        sha256,
        bytes: size,
    })
}

#[cfg(test)]
pub(super) mod tests;
