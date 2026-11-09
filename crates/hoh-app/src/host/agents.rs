//! The product's agents (docs/adr/ADR-022.md decisions 9 and 10): the registry that finds and probes each agent CLI
//! outside the writer, and the binder that gives each binding its session with what its agent's probe found, or
//! blocks it when its agent is unavailable.

use hoh_agents::capabilities::Probed;
use hoh_agents::compat::Agent;
use hoh_agents::launch;
use hoh_agents::probe::{self, ProbeEnv};
use hoh_core::persistence::{PersistError, SessionGrant, Store, Unbound};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long one probe command may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);

/// An agent found and probed: what starting its turns needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    pub launcher: PathBuf,
    /// The first line of its `--version`.
    pub version: String,
    /// What its probe found.
    pub capabilities: Probed,
    pub facts: launch::Facts,
}

/// The identity of a launcher and its implementation (device, inode, size, modification time): probed again when it
/// changes.
type Stamp = Vec<(u64, u64, u64, i64)>;

#[derive(Debug, Clone)]
struct Seen {
    stamp: Stamp,
    outcome: Result<Ready, String>,
}

/// The host's agents, found on its PATH and probed lazily (decision 10).
#[derive(Debug)]
pub struct Registry {
    path: OsString,
    seen: Mutex<BTreeMap<Agent, Seen>>,
    /// The agents the runner needs looked at (its team's, its unbound bindings'): [`Registry::refresh`] looks at them,
    /// off the runner's tick, so no probe holds up a tick or the host's stop.
    wanted: Mutex<BTreeSet<Agent>>,
    /// The host's stop: every probe command running is killed and none starts (decision 10 v6, M2T07R-202, -300, -450,
    /// -700).
    stopping: probe::ProbeStop,
}

impl Registry {
    /// Agents looked for on `path`.
    pub fn new(path: OsString) -> Registry {
        Registry {
            path,
            seen: Mutex::new(BTreeMap::new()),
            wanted: Mutex::new(BTreeSet::new()),
            stopping: probe::ProbeStop::default(),
        }
    }

    /// The host stops: every probe command in its slot is killed (its group and its leader) before this returns, one
    /// still being spawned by its starter as it puts it in its slot, and none starts; it never waits on a probing
    /// thread: a lock still held when the probe's STOP_WAIT (200 ms) has passed is left, with its commands, to the
    /// probes' own looks at the stop (`probe::ProbeStop`); what a probing thread that is not run leaves alive is the
    /// residual of decision 10 v6 (M2T07R-300, -450, -500, -700, -853).
    pub fn stop(&self) {
        self.stopping.stop();
    }

    /// The agents named `names` are the ones to look at from now on (names that are not agents are left out).
    pub fn want<'a>(&self, names: impl IntoIterator<Item = &'a String>) {
        let agents: BTreeSet<Agent> = names.into_iter().filter_map(|n| Agent::named(n)).collect();
        if let Ok(mut wanted) = self.wanted.lock() {
            *wanted = agents;
        }
    }

    /// Look at every wanted agent now (blocking: probed again only when its files changed).
    pub fn refresh(&self) {
        let wanted: Vec<Agent> = self
            .wanted
            .lock()
            .map(|w| w.iter().copied().collect())
            .unwrap_or_default();
        for agent in wanted {
            let _ = self.look(agent);
        }
    }

    /// What was last seen of every agent, without looking (decision 8): those found available, those found unavailable
    /// with why; any other was not looked at yet.
    pub fn availability(&self) -> hoh_core::persistence::Availability {
        let mut known = hoh_core::persistence::Availability {
            available: Some(BTreeSet::new()),
            unavailable: BTreeMap::new(),
        };
        if let Ok(seen) = self.seen.lock() {
            for (agent, seen) in seen.iter() {
                let name = agent.as_str().to_owned();
                match &seen.outcome {
                    Ok(_) => {
                        if let Some(available) = known.available.as_mut() {
                            available.insert(name);
                        }
                    }
                    Err(why) => {
                        known.unavailable.insert(name, why.clone());
                    }
                }
            }
        }
        known
    }

    fn env(&self) -> ProbeEnv {
        ProbeEnv {
            path: self.path.clone(),
            fixture_root: None,
            timeout: PROBE_TIMEOUT,
            log: None,
            deadline: None,
            cancel: Some(self.stopping.clone()),
        }
    }

    /// `agent` as it is now: probed again when its launcher or implementation changed, else as last seen. Blocking: run
    /// it outside the writer and off the async workers.
    pub fn look(&self, agent: Agent) -> Result<Ready, String> {
        let env = self.env();
        let outcome = (|| {
            let launcher = probe::find_on_path(agent.command(), &env.path)
                .ok_or_else(|| format!("{} is not on PATH", agent.command()))?;
            let implementation =
                probe::resolve_implementation(agent, &launcher).map_err(|e| e.to_string())?;
            let stamp = stamp(&[&launcher, &implementation]).map_err(|e| e.to_string())?;
            if let Ok(seen) = self.seen.lock()
                && let Some(seen) = seen.get(&agent)
                && seen.stamp == stamp
            {
                return Ok((stamp, seen.outcome.clone()));
            }
            let probed = probe::probe(agent, &env)
                .map_err(|e| e.to_string())
                .and_then(|entry| {
                    let facts = launch::read(agent, &launcher, &env)?;
                    Ok(Ready {
                        launcher: launcher.clone(),
                        version: entry.cli_version,
                        capabilities: entry.capabilities,
                        facts,
                    })
                });
            Ok((stamp, probed))
        })();
        let (stamp, outcome) = match outcome {
            Ok(found) => found,
            Err(why) => (Stamp::new(), Err(why)),
        };
        // A look the host's stop ended says nothing of the agent: not recorded, so it stays as last seen (or not looked
        // at), and no dispatch takes the stop for the agent being unavailable (M2T07R-352).
        if self.stopping.is_stopped() {
            return outcome;
        }
        if let Ok(mut seen) = self.seen.lock() {
            seen.insert(
                agent,
                Seen {
                    stamp,
                    outcome: outcome.clone(),
                },
            );
        }
        outcome
    }

    /// `agent` as it was last looked at, without looking again; `None` if it never was.
    pub fn seen(&self, agent: Agent) -> Option<Result<Ready, String>> {
        self.seen
            .lock()
            .ok()
            .and_then(|seen| seen.get(&agent).map(|s| s.outcome.clone()))
    }
}

fn stamp(paths: &[&Path]) -> std::io::Result<Stamp> {
    use std::os::unix::fs::MetadataExt as _;
    paths
        .iter()
        .map(|p| {
            std::fs::metadata(p).map(|m| {
                (
                    m.dev(),
                    m.ino(),
                    m.size(),
                    m.mtime() * 1_000_000_000 + m.mtime_nsec(),
                )
            })
        })
        .collect()
}

/// The capabilities every role needs, and those a role following `contract`'s role contract needs too (REQ-020
/// decision 1, the part that has names; decision 10): structured events always, resume for the commander and the
/// reviewer.
pub fn required(contract: Option<&str>) -> BTreeSet<String> {
    let mut needs: BTreeSet<String> = ["structured_events".to_owned()].into();
    if matches!(contract, Some("commander" | "reviewer")) {
        needs.insert("resume".to_owned());
    }
    needs
}

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

/// The binder (decision 9): each binding waiting for its session gets one if its agent is available (its version and
/// the capabilities a turn will have), is blocked (AGENT_UNAVAILABLE) with why if not, or waits while its agent was not
/// looked at yet. It reads what the registry last saw: no probe runs in a tick or a writer job.
pub(super) async fn bind(
    store: &Store,
    registry: &Arc<Registry>,
    unbound: Vec<Unbound>,
    cwd: &Path,
) -> Result<(), PersistError> {
    for waiting in unbound {
        let Some(agent) = Agent::named(&waiting.agent) else {
            continue;
        };
        let Some(looked) = registry.seen(agent) else {
            continue;
        };
        let granted = looked.and_then(|ready| {
            let capabilities = ready.session_capabilities(agent, cwd)?;
            Ok((ready.version, capabilities))
        });
        match granted {
            Ok((agent_version, capabilities)) => {
                store
                    .bind_session(SessionGrant {
                        run_id: waiting.run_id,
                        role: waiting.role,
                        role_epoch: waiting.role_epoch,
                        agent: waiting.agent,
                        agent_version,
                        capabilities,
                    })
                    .await?;
            }
            Err(why) => {
                store.block_binding(waiting, why).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
