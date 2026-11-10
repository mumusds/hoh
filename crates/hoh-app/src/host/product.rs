//! The product executor (docs/adr/ADR-022.md decisions 11 to 16): each attempt runs a turn of its binding's agent in
//! the adapter's runner mode, its process started at the `hoh __gate` start gate, its prompt the role's contract with
//! the task's header. The adapter moves no state; the turn's facts come back as the attempt's [`Outcome`].

use super::agents::{Prepared, Ready, Registry, Spec};
use super::runner::{Executor, Gate, Outcome, Refusal, Started, Work};
use hoh_agents::adapter::{AttemptRef, Facts, GateCommand, Recorded};
use hoh_agents::compat::Agent;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{Observed, PromptRecord, SessionStart};
use hoh_core::retry::FailureClass;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};

/// The product's executor: the agents of the roles through the host's registry.
#[derive(Debug)]
pub struct ProductExecutor {
    registry: Arc<Registry>,
    /// The project: the agents' working directory, where prompt overrides are looked for.
    project: PathBuf,
    /// The start gate's program (`hoh` itself; tests pass the built binary).
    gate: PathBuf,
    /// The native cancel's and SIGTERM's graces (execution.cancel_native_grace_seconds, cancel_term_grace_seconds).
    graces: (Duration, Duration),
    /// The native cancel of each attempt whose turn runs.
    cancels: Arc<Mutex<BTreeMap<String, Arc<Notify>>>>,
    /// Codex implementations (launcher, version) whose App Server degraded: their next turns use exec (decision 15).
    exec_only: Arc<Mutex<BTreeSet<(PathBuf, String)>>>,
}

impl ProductExecutor {
    pub fn new(
        registry: Arc<Registry>,
        project: PathBuf,
        gate: PathBuf,
        graces: (Duration, Duration),
    ) -> ProductExecutor {
        ProductExecutor {
            registry,
            project,
            gate,
            graces,
            cancels: Arc::default(),
            exec_only: Arc::default(),
        }
    }
}

/// How long a delivery waits for the registry's first look at its agent (its probe's commands each have 20 s).
const DELIVER_WAIT: Duration = Duration::from_secs(60);

/// Lower-case hex of the sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `id` as one path component: `%` and `/` escaped, so distinct ids stay distinct.
fn component(id: &str) -> String {
    id.replace('%', "%25").replace('/', "%2F")
}

/// Where HoH session `session_id`'s first input is kept for its first turn (docs/adr/ADR-024.md decision 4).
pub fn first_input_path(project: &Path, session_id: &str) -> PathBuf {
    project
        .join(".hoh/first-inputs")
        .join(format!("{}.json", component(session_id)))
}

/// Keep `start`'s package as its session's first input: written whole to a file of its own, then linked to its place,
/// which fails if anything is there already, so the place only ever holds a whole input and only one. The same input
/// there already (a notice given again after a crash) is taken; another one is refused.
fn keep_first_input(project: &Path, start: &SessionStart) -> Result<(), String> {
    use std::io::Write as _;
    let session = start
        .session_id
        .as_deref()
        .ok_or("the binding has no session")?;
    let path = first_input_path(project, session);
    let body = serde_json::to_vec(&serde_json::json!({
        "message_id": start.message_id, "session_id": session, "package": start.package,
    }))
    .map_err(|e| e.to_string())?;
    let dir = path.parent().ok_or("no directory")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let staged = dir.join(format!(
        ".{}.{}",
        std::process::id(),
        component(&start.message_id)
    ));
    let written = (|| {
        let mut file = std::fs::File::create(&staged)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::hard_link(&staged, &path)
    })();
    let _ = std::fs::remove_file(&staged);
    match written {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => match std::fs::read(&path) {
            Ok(there) if there == body => Ok(()),
            Ok(_) => Err(format!("{} holds another first input", path.display())),
            Err(e) => Err(format!("{}: {e}", path.display())),
        },
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The id HoH gives a new Pi session for HoH session `session_id` (ADR-013 decision 1): a UUID-shaped digest of it, the
/// same for every turn and every host.
fn pi_session_id(session_id: &str) -> String {
    let d = Sha256::digest(session_id.as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The text a turn is sent: the role's contract, then the task's header (decision 11).
fn prompt_text(contract: &str, work: &Work) -> String {
    let a = &work.assignment;
    format!(
        "{contract}\n\n---\n# 本次任务\n\n- Run 目标：{goal}\n- 任务：{task}{title}\n- 职位：{role}\n",
        contract = contract.trim_end(),
        goal = a.goal.as_deref().unwrap_or("（未给出）"),
        task = work.task_id,
        title = a
            .title
            .as_deref()
            .map(|t| format!(" {t}"))
            .unwrap_or_default(),
        role = work.role,
    )
}

/// How an adapter's failure code fails its attempt (decision 16, ADR-020 decision 1).
fn class_of(code: Option<&str>) -> FailureClass {
    match code {
        Some("BLOCKED_AUTH") => FailureClass::Auth,
        Some("BLOCKED_CAPABILITY" | "MODEL_UNAVAILABLE") => FailureClass::Capability,
        _ => FailureClass::Transient { retry_after: None },
    }
}

/// A refusal of `reason`, `class`, with `detail` kept in the attempt's event.
fn refused(reason: &str, class: FailureClass, detail: String) -> Refusal {
    Refusal {
        reason: reason.to_owned(),
        class,
        detail: Some(detail),
    }
}

/// The attempt's outcome from its turn's facts and the prompt it was sent.
fn outcome(facts: Facts, prompt: PromptRecord) -> Outcome {
    let success = facts.state == AttemptState::Settled;
    let mut native_session = facts.native_session.clone();
    let mut events = Vec::new();
    for recorded in facts.recorded {
        match recorded {
            Recorded::Event {
                key,
                kind,
                mut payload,
            } => {
                // A frame too large: how much the reader held at most, the test probe of AC-027-03.
                if kind == "adapter.diagnostic"
                    && payload["reason"] == "PROTOCOL_FRAME_TOO_LARGE"
                    && let Some(object) = payload.as_object_mut()
                {
                    object.insert(
                        "reader_max_buffered".to_owned(),
                        serde_json::json!(facts.max_buffered),
                    );
                }
                events.push((key, kind, payload));
            }
            Recorded::NativeSession(id) => native_session = native_session.or(Some(id)),
        }
    }
    let detail = match facts.state {
        AttemptState::Settled => "SETTLED".to_owned(),
        AttemptState::Cancelled => facts
            .failure
            .clone()
            .unwrap_or_else(|| "CANCELLED".to_owned()),
        _ => facts.failure.clone().unwrap_or_else(|| "FAILED".to_owned()),
    };
    Outcome {
        success,
        detail,
        group_gone: facts.group_reaped,
        class: class_of(facts.failure.as_deref()),
        observed: Observed {
            resolved_model: facts.resolved_model,
            usage: facts.usage,
            native_session,
            events,
            prompt: Some(prompt),
            raw_log_path: Some(facts.raw_log_path),
        },
        unknown_effect: facts.state == AttemptState::UnknownEffect,
    }
}

/// The turn's diagnostics also in `<raw logs>/diagnostics.jsonl` (decision 18), one JSON line each.
fn keep_diagnostics(outcome: &Outcome) {
    use std::io::Write as _;
    let Some(logs) = &outcome.observed.raw_log_path else {
        return;
    };
    let lines: String = outcome
        .observed
        .events
        .iter()
        .filter(|(_, kind, _)| kind == "adapter.diagnostic")
        .map(|(_, _, payload)| format!("{payload}\n"))
        .collect();
    if lines.is_empty() {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(logs).join("diagnostics.jsonl"))
    {
        let _ = file.write_all(lines.as_bytes());
    }
}

impl ProductExecutor {
    /// The turn `work` runs and the record of its prompt, or why it cannot start.
    fn prepare(&self, work: &Work) -> Result<(Agent, Ready, Prepared, PromptRecord), Refusal> {
        let a = &work.assignment;
        let agent = a.agent.as_deref().and_then(Agent::named).ok_or_else(|| {
            refused(
                "AGENT_UNAVAILABLE",
                FailureClass::Capability,
                format!("the binding of {} names no agent", work.role),
            )
        })?;
        let ready = match self.registry.seen(agent) {
            Some(Ok(ready)) => ready,
            Some(Err(why)) => {
                return Err(refused("AGENT_UNAVAILABLE", FailureClass::Capability, why));
            }
            None => {
                return Err(refused(
                    "AGENT_UNAVAILABLE",
                    FailureClass::Transient { retry_after: None },
                    format!("{} was not looked at yet", agent.as_str()),
                ));
            }
        };
        let contract = hoh_core::config::contract(&self.project, &work.role, a.prompt.as_deref())
            .map_err(|why| {
            refused(
                "PROMPT_UNAVAILABLE",
                FailureClass::Transient { retry_after: None },
                why,
            )
        })?;
        let text = prompt_text(&contract.text, work);
        let record = PromptRecord {
            layer: contract.layer.to_owned(),
            role: work.role.clone(),
            path: contract.path,
            content_sha256: sha256_hex(contract.text.as_bytes()),
            sent_sha256: sha256_hex(text.as_bytes()),
        };
        let session_id = a.session_id.clone().unwrap_or_default();
        let exec_only = self
            .exec_only
            .lock()
            .is_ok_and(|e| e.contains(&(ready.launcher.clone(), ready.version.clone())));
        let hoh = self.project.join(".hoh");
        let spec = Spec {
            cwd: self.project.clone(),
            model: a.model.clone(),
            resume: a.native_session.clone(),
            prompt: text,
            logs: attempt_logs(&self.project, &work.attempt_id),
            pi_session: (
                hoh.join("sessions").join(component(&session_id)),
                pi_session_id(&session_id),
            ),
            exec_only,
            graces: self.graces,
        };
        let prepared = ready
            .turn(agent, spec)
            .map_err(|why| refused("BLOCKED_CAPABILITY", FailureClass::Capability, why))?;
        Ok((agent, ready, prepared, record))
    }
}

impl Executor for ProductExecutor {
    fn registry(&self) -> Option<Arc<Registry>> {
        Some(self.registry.clone())
    }

    /// Taken once the registry has looked at the work's agent (at once, but for a binding made before this host's
    /// first look, as after a restart): a turn is never failed for an agent not looked at yet (M2T07R-250). Not taken
    /// by then, the runner hands it again.
    fn deliver(&self, work: &Work) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let agent = work.assignment.agent.as_deref().and_then(Agent::named);
        match agent {
            Some(agent) if self.registry.seen(agent).is_none() => {
                let registry = self.registry.clone();
                tokio::spawn(async move {
                    let until = std::time::Instant::now() + DELIVER_WAIT;
                    while registry.seen(agent).is_none() && std::time::Instant::now() < until {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    let _ = tx.send(match registry.seen(agent) {
                        Some(_) => Ok(()),
                        None => Err(format!("{} was not looked at yet", agent.as_str())),
                    });
                });
            }
            _ => {
                let _ = tx.send(Ok(()));
            }
        }
        rx
    }

    /// The new holder's first input kept for its session's first turn (`first_input_path`): the Commander's turns
    /// begin with it (M2-T08).
    fn open_session(&self, start: &SessionStart) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(keep_first_input(&self.project, start));
        rx
    }

    fn start(&self, work: &Work) -> Result<Started, Refusal> {
        let (agent, ready, prepared, record) = self.prepare(work)?;
        let attempt = AttemptRef {
            attempt_id: work.attempt_id.clone(),
            session_id: work.assignment.session_id.clone().unwrap_or_default(),
            role: work.role.clone(),
            agent_version: work
                .assignment
                .agent_version
                .clone()
                .unwrap_or_else(|| ready.version.clone()),
        };
        let gate = GateCommand {
            program: self.gate.clone(),
            args: vec!["__gate".into()],
            env: vec![(super::ATTEMPT_ENV.into(), work.marker().into())],
        };
        let held = match &prepared {
            Prepared::Claude(turn) => hoh_agents::claude::start(&attempt, turn, &gate),
            Prepared::Codex(turn) => hoh_agents::codex::start(&attempt, turn, &gate),
            Prepared::Pi(turn) => hoh_agents::pi::start(&attempt, turn, &gate),
        }
        .map_err(|r| {
            let class = class_of(Some(&r.code));
            refused(&r.code, class, r.detail)
        })?;
        let id = work.attempt_id.clone();
        if let Ok(mut cancels) = self.cancels.lock() {
            cancels.insert(id.clone(), held.cancel.clone());
        }
        let (tx, ended) = oneshot::channel();
        let cancels = self.cancels.clone();
        let exec_only = self.exec_only.clone();
        let implementation = (ready.launcher.clone(), ready.version.clone());
        let turn = held.ended;
        tokio::spawn(async move {
            let facts = turn.await;
            if let Ok(mut cancels) = cancels.lock() {
                cancels.remove(&id);
            }
            // A turn whose task was lost: its end is not known, so the sender goes unsent (the runner settles it).
            let Ok(facts) = facts else {
                return;
            };
            if agent == Agent::Codex
                && facts.degraded.is_some()
                && let Ok(mut e) = exec_only.lock()
            {
                e.insert(implementation);
            }
            let outcome = outcome(facts, record);
            keep_diagnostics(&outcome);
            let _ = tx.send(outcome);
        });
        let open = held.open;
        Ok(Started {
            pid: held.pid,
            gate: Gate::new(move || {
                open.send(())
                    .map_err(|()| "the turn ended before its gate opened".to_owned())
            }),
            ended,
        })
    }

    fn cancel(&self, work: &Work) -> bool {
        let held = self
            .cancels
            .lock()
            .ok()
            .and_then(|c| c.get(&work.attempt_id).cloned());
        if let Some(cancel) = &held {
            cancel.notify_one();
        }
        held.is_some()
    }
}

/// Where the raw logs of attempt `attempt_id` go in the project at `project`.
fn attempt_logs(project: &Path, attempt_id: &str) -> PathBuf {
    project.join(".hoh/attempts").join(component(attempt_id))
}

#[cfg(test)]
mod tests;
