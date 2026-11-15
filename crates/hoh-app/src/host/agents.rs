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
use hoh_core::persistence::{Evidence, PersistError, StaffChange, StaffWrite, Store};
use hoh_core::staffing::{
    AgentIdentity, BindingState, Cause, Compat, Fact, FileStamp, Judgment, Kind, Snapshot, Staffed,
    Stamp, Usable,
};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long one probe command may take.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// How often a wanted agent's login is looked at again (its status command, docs/adr/ADR-024.md decision 7), and how
/// soon a temporary probe failure is probed again though its files did not change (docs/adr/ADR-030.md decision 2).
const AUTH_EVERY: Duration = Duration::from_secs(30);

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

/// What an actual look at an agent found.
#[derive(Debug, Clone)]
struct Seen {
    /// The files' stamps it was found under; `None` when there were none to read (no launcher, or not readable): looked
    /// at again each time.
    key: Option<Key>,
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
    /// `agent`'s fact and observation as the registry knows them now: the one constructor of a snapshot's items and of
    /// [`Registry::fact`] (docs/adr/ADR-030.md decision 1, M2T15D-100).
    fn item(&self, agent: Agent, suite_set: &str) -> (Option<Fact>, Option<Observation>) {
        let Some(known) = self.agents.get(&agent) else {
            return (None, None);
        };
        let fact = self.fact(agent, known, suite_set);
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
    fn probe(&self, agent: Agent, suite_set: &str) -> Option<serde_json::Value> {
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
                let detail = match self.fact(agent, known, suite_set) {
                    Some(Fact::LoginMissing(why) | Fact::Unsupported(why)) => Some(why),
                    _ => None,
                };
                let entry = &ready.entry;
                serde_json::json!({
                    "class": null, "detail": detail, "cli_version": entry.cli_version,
                    "launcher_path": entry.launcher_path, "implementation_sha256": entry.implementation_sha256,
                    "help_sha256": entry.help_sha256, "schema_sha256": entry.schema_sha256,
                    "capabilities": entry.capabilities,
                    "compat": self.compat(agent, ready, suite_set),
                    "observation": observation,
                })
            }
        })
    }

    /// Decision 2's fact of `agent`; `None` while it was not looked at, or while the registry has no project to say
    /// what a session of it would have.
    fn fact(&self, agent: Agent, known: &Known, suite_set: &str) -> Option<Fact> {
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
        Some(Fact::Usable(Box::new(Usable {
            version: ready.version.clone(),
            identity: ready.entry.identity(),
            stamp: ready.stamp,
            generation: (self.host_epoch, ready.look),
            session: session.clone(),
            granted: hoh_core::decision::granted_capabilities(session),
            compat: self.compat(agent, ready, suite_set),
        })))
    }

    /// Whether `agent` as its look `ready` found it is verified (decision 9's `compat::verdict` of its entry against
    /// the manifest held), but `Pending` once its launcher's or implementation's stamp read now (`stat`, following
    /// links, metadata only: prepare's (a)) is not the look's, or cannot be read: its files changed since the last
    /// look, which only the next look probes (docs/adr/ADR-030.md decision 1, its implementation correction in S6c).
    /// Read under the registry's lock, so a snapshot, `fact` and `prepared` all see the change as soon as it is there,
    /// and no task is put back for a version the registry has not looked at (decision 13 (b)).
    fn compat(&self, agent: Agent, ready: &Ready, suite_set: &str) -> Compat {
        let implementation = Path::new(&ready.entry.implementation_path);
        match stamp_now(&ready.launcher, implementation) {
            Some(now) if now == ready.stamp => compat::verdict(
                self.manifest.as_ref(),
                agent.as_str(),
                &ready.entry,
                suite_set,
            ),
            now => Compat::Pending(format!(
                "files changed since the last look: {} and {} were {}, are {}",
                ready.launcher.display(),
                implementation.display(),
                serde_json::json!(ready.stamp),
                now.map_or_else(
                    || "unreadable".to_owned(),
                    |s| serde_json::json!(s).to_string()
                ),
            )),
        }
    }
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

    /// The registry verifying against a test's suite set instead of this build's ([`compat::SUITE_SET_SHA256`]).
    #[cfg(test)]
    fn with_suite_set(mut self, suite_set: &str) -> Registry {
        self.suite_set = suite_set.to_owned();
        self
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
        self.state().manifest = Some(manifest);
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
        let moved = state.root.as_deref() != Some(root);
        state.root = Some(root.to_path_buf());
        for (agent, known) in &mut state.agents {
            if let Some(seen) = known.seen.as_mut()
                && let Ok(ready) = &seen.outcome
                && (moved || seen.session.is_none())
            {
                seen.session = Some(ready.session_capabilities(*agent, root));
            }
        }
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
        }
    }

    /// `agent` as it is now: probed again when its launcher or implementation changed (their stamps, change time
    /// included) or a temporary failure's [`AUTH_EVERY`] passed, else as last seen. Blocking: run it outside the writer
    /// and off the async workers.
    pub fn look(&self, agent: Agent) -> Result<Ready, String> {
        let env = self.env();
        let found = (|| {
            let launcher = probe::find_on_path(agent.command(), &env.path)
                .ok_or_else(|| Probe::of(&ProbeError::NotFound { agent }))?;
            let implementation =
                probe::resolve_implementation(agent, &launcher).map_err(|e| Probe::of(&e))?;
            let key = key(&launcher, &implementation).map_err(|e| Probe::of(&ProbeError::Io(e)))?;
            Ok((launcher, key))
        })();
        let (key, outcome, looked) = match found {
            Err(probe) => (None, Err(probe), true),
            Ok((launcher, key)) => match self.cached(agent, &key) {
                Some(outcome) => (Some(key), outcome, false),
                None => (Some(key), probed(agent, &launcher, &key, &env), true),
            },
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
        }
        let outcome = if looked {
            self.record(agent, key, outcome)
        } else {
            outcome
        };
        outcome.map_err(|probe| probe.detail)
    }

    /// What was last seen of `agent` under the stamps `key`, unless it is to be probed again: another key, or a
    /// temporary failure [`AUTH_EVERY`] old.
    fn cached(&self, agent: Agent, key: &Key) -> Option<Result<Ready, Probe>> {
        let mut state = self.state();
        let root = state.root.clone();
        let seen = state.agents.get_mut(&agent)?.seen.as_mut()?;
        if seen.key.as_ref() != Some(key) {
            return None;
        }
        if let Err(probe) = &seen.outcome
            && probe.class.temporary()
            && seen.at.elapsed() >= self.every
        {
            return None;
        }
        if let (Ok(ready), None, Some(root)) = (&seen.outcome, &seen.session, &root) {
            seen.session = Some(ready.session_capabilities(agent, root));
        }
        Some(seen.outcome.clone())
    }

    /// Record an actual look at `agent`: its `n` moves; a ready agent gets its session capabilities in the project and,
    /// when its identity is not the one last found, its recovery (docs/adr/ADR-030.md decision 2).
    fn record(
        &self,
        agent: Agent,
        key: Option<Key>,
        mut outcome: Result<Ready, Probe>,
    ) -> Result<Ready, Probe> {
        let mut state = self.state();
        let root = state.root.clone();
        let known = state.agents.entry(agent).or_default();
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
            session = root.map(|root| ready.session_capabilities(agent, &root));
        }
        known.seen = Some(Seen {
            key,
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
                let observation = state.item(*agent, &self.suite_set).1?;
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
            let (fact, observation) = state.item(*agent, &self.suite_set);
            let name = agent.as_str().to_owned();
            if let Some(fact) = fact {
                snapshot.facts.insert(name.clone(), fact);
            }
            if let Some(probe) = state.probe(*agent, &self.suite_set) {
                snapshot.probes.insert(name.clone(), probe);
            }
            if let Some(observation) = observation {
                snapshot.observations.insert(name, observation);
            }
        }
        Arc::new(snapshot)
    }

    /// `agent`'s fact now, as a snapshot taken now would have it (the same constructor, M2T15D-100); `None` while it
    /// was not looked at.
    pub fn fact(&self, agent: Agent) -> Option<Fact> {
        self.state().item(agent, &self.suite_set).0
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
        let fact = state.item(agent, &self.suite_set).0?;
        let compat = match &fact {
            Fact::Usable(usable) => usable.compat.clone(),
            _ => state.compat(agent, ready, &self.suite_set),
        };
        Some(Ok(Current {
            ready: ready.clone(),
            ran: Ran {
                agent,
                identity: ready.entry.identity(),
                generation: (state.host_epoch, known.looks),
            },
            fact,
            compat,
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

/// Probe `agent` at `launcher`, its files' stamps being `key`: its entry and launch facts.
fn probed(agent: Agent, launcher: &Path, key: &Key, env: &ProbeEnv) -> Result<Ready, Probe> {
    let entry = probe::probe(agent, env).map_err(|e| Probe::of(&e))?;
    // The same commands the probe just ran: their failure now is a temporary one.
    let facts = launch::read(agent, launcher, env).map_err(|detail| Probe {
        class: ProbeClass::Failed,
        detail,
    })?;
    Ok(Ready {
        launcher: launcher.to_path_buf(),
        version: entry.cli_version.clone(),
        capabilities: entry.capabilities.clone(),
        facts,
        entry,
        stamp: stamp_of(key),
        look: 0,
    })
}

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
    /// A turn of `agent` as `spec` says, under the YOLO profile; or why this agent's facts are not its own.
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
                        yolo: true,
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
                    yolo: true,
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
            }),
            _ => {
                return Err(format!(
                    "{}'s launch facts are another agent's",
                    agent.as_str()
                ));
            }
        })
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
) -> Result<bool, PersistError> {
    let mut wrote = false;
    for staffed in staffing {
        let judgment = hoh_core::staffing::judge(&staffed, &snapshot.facts, &staffed.excluded);
        let Some(write) = write_of(&staffed, judgment, snapshot, root).await else {
            continue;
        };
        wrote |= store
            .staff(StaffChange {
                run_id: staffed.run_id,
                role: staffed.role,
                role_epoch: staffed.role_epoch,
                agent: staffed.agent,
                write,
            })
            .await?;
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
    let sha256 = tokio::task::spawn_blocking(move || hoh_core::objects::put(&root, &bytes))
        .await
        .ok()?
        .ok()?;
    Some(Evidence {
        sha256,
        bytes: size,
    })
}

#[cfg(test)]
mod tests;
