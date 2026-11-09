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
}

impl Registry {
    /// Agents looked for on `path`.
    pub fn new(path: OsString) -> Registry {
        Registry {
            path,
            seen: Mutex::new(BTreeMap::new()),
        }
    }

    fn env(&self) -> ProbeEnv {
        ProbeEnv {
            path: self.path.clone(),
            fixture_root: None,
            timeout: PROBE_TIMEOUT,
            log: None,
            deadline: None,
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

    /// The agents whose last look found them unavailable (decision 8: a change to one waits).
    pub fn unavailable(&self) -> BTreeSet<String> {
        self.seen
            .lock()
            .map(|seen| {
                seen.iter()
                    .filter(|(_, s)| s.outcome.is_err())
                    .map(|(agent, _)| agent.as_str().to_owned())
                    .collect()
            })
            .unwrap_or_default()
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

impl Ready {
    /// What a session of this agent has (decision 9), for a turn in `cwd` under the YOLO profile; or why no turn of it
    /// can start.
    pub fn session_capabilities(
        &self,
        agent: Agent,
        cwd: &Path,
    ) -> Result<serde_json::Value, String> {
        match (agent, &self.facts) {
            (Agent::Claude, launch::Facts::Claude { help }) => {
                hoh_agents::claude::session_capabilities(&hoh_agents::claude::Launch {
                    program: self.launcher.clone(),
                    help: help.clone(),
                    cwd: cwd.to_path_buf(),
                    yolo: true,
                    partial: false,
                    resume: None,
                    model: None,
                    root: rustix::process::geteuid().is_root(),
                    capabilities: self.capabilities.clone(),
                })
                .map_err(|e| e.0)
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
            ) => hoh_agents::codex::session_capabilities(&hoh_agents::codex::Turn {
                launch: hoh_agents::codex::Launch {
                    program: self.launcher.clone(),
                    app_server_help: app_server_help.clone(),
                    exec_help: exec_help.clone(),
                    exec_resume_help: exec_resume_help.clone(),
                    cwd: cwd.to_path_buf(),
                    yolo: true,
                    resume: None,
                    model: None,
                    known: known.clone(),
                    requests: requests.clone(),
                    client_notifications: client_notifications.clone(),
                    capabilities: self.capabilities.clone(),
                },
                prompt: String::new(),
                logs: cwd.to_path_buf(),
                escalation: hoh_agents::codex::Turn::default_escalation(),
                handshake_timeout: Duration::from_secs(60),
                deadline: None,
            })
            .map_err(|e| e.0),
            (Agent::Pi, launch::Facts::Pi { help }) => {
                hoh_agents::pi::session_capabilities(&hoh_agents::pi::Launch {
                    program: self.launcher.clone(),
                    help: help.clone(),
                    cwd: cwd.to_path_buf(),
                    session_dir: cwd.join(".hoh/pi-sessions"),
                    session_id: String::new(),
                    resume: false,
                    provider: None,
                    model: None,
                    settled_event: hoh_agents::pi::emits_settled(Some(&self.version)),
                    capabilities: self.capabilities.clone(),
                })
                .map_err(|e| e.0)
            }
            _ => Err(format!(
                "{}'s launch facts are another agent's",
                agent.as_str()
            )),
        }
    }
}

/// The binder (decision 9): each binding waiting for its session gets one if its agent is available now (its version
/// and the capabilities a turn will have), else is blocked (AGENT_UNAVAILABLE) with why. Probing runs off the async
/// workers, never in a writer job.
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
        let looking = registry.clone();
        let looked = tokio::task::spawn_blocking(move || looking.look(agent))
            .await
            .unwrap_or_else(|e| Err(format!("the probe did not finish: {e}")));
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
