//! Commands accepted through the host API, the host's start, and the reads its queries need (REQ-022, REQ-036,
//! REQ-041, REQ-062; docs/adr/ADR-016.md).
//!
//! A command is one `BEGIN IMMEDIATE` transaction: its idempotency key is looked up first (the same payload hash
//! returns the stored result byte for byte, another hash is `KEY_REUSE`), then the run moves by compare-and-set with
//! its event and outbox message, and the exact accepted result is stored with the command. Only accepted commands get
//! a row (`accepted_at` is required): a rejected command is decided again when it is retried.

use super::writer::{self, Change, Commit, Intent, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::lifecycle::Lifecycle as _;
use crate::protocol::RunState;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// A command to apply to a run.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitCommand {
    pub command_id: String,
    pub idempotency_key: String,
    /// The contract name (`$defs.Command.kind`).
    pub kind: String,
    pub run_id: String,
    pub expected_revision: i64,
    /// The command's `payload` object as stored.
    pub payload: serde_json::Value,
    /// sha256 (hex) of the whole command body in canonical form: what the key is compared on.
    pub payload_sha256: String,
    /// Where the run moves.
    pub target: RunTarget,
    /// The event appended for the move, and the outbox message for it.
    pub event: NewEvent,
    pub outbox: Option<NewOutbox>,
}

/// Where a command moves its run.
#[derive(Debug, Clone, PartialEq)]
pub enum RunTarget {
    /// To this state, by compare-and-set.
    To(RunState),
    /// Back to the state saved when it stopped (ResumeRun from PAUSED, INCOMPLETE, WAITING_EXTERNAL or BLOCKED), after
    /// a reconcile of the run recorded first (docs/adr/ADR-021.md decision 7), once that stop's own predicate no longer
    /// holds; to BLOCKED while blockers are claimed (REQ-036 decision 2, docs/adr/ADR-031.md decision 16).
    Resume,
    /// A new run, CREATED, in this mode (StartRun, decision 5): `run_id` is the id the host gave it. It binds the
    /// project's current team and gets `budget` (docs/adr/ADR-022.md decision 7), and adopts the project's latest
    /// approved specification, or is BLOCKED on its question `runtime:no_spec` when there is none
    /// (docs/adr/ADR-031.md decision 1).
    Start {
        mode: String,
        budget: crate::config::Budget,
        /// `supervision.policy`, frozen into the run's snapshot (docs/adr/ADR-025.md decision 9).
        policy: String,
        /// `[assumptions]`, frozen into the run's snapshot: what it may assume when unattended (REQ-030 behaviour 4).
        assumptions: std::collections::BTreeMap<String, crate::config::Assumed>,
        /// StartRun's `interactive: false` (`hoh -p`), frozen into the run's snapshot (docs/adr/ADR-035.md decision 11).
        unattended: bool,
        mode_cycle: Vec<String>,
    },
    /// The project's team (ConfigureTeam, docs/adr/ADR-022.md decisions 3 and 4): revision r+1 over the current r,
    /// applied at once; `expected_revision` is the team revision. An empty `run_id` is the project's command (null in
    /// the request); a run id names a run that must exist.
    Team(super::team::TeamChange),
    /// Mode intent applied now or at the writer safe point (ADR-036).
    SetMode(crate::protocol::Mode),
    /// An input to the run (AnswerQuestion, SendInput; docs/adr/ADR-031.md decision 7): accepted in any state but a
    /// terminal one, the run's revision moved (its move or a touch), its `run.result` at the end of the commit when it
    /// stays stopped. Working ASK and business rejection leave the revision unchanged (ADR-036).
    Input(Input),
    /// Approve the host's Markdown snapshot in the command transaction (ADR-033).
    Specification(Box<super::SpecRequest>),
}

/// What an input command brings (docs/adr/ADR-031.md decision 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// AnswerQuestion: the answer to one of the run's open questions, by a choice or by its text.
    Answer {
        question_id: String,
        answer: String,
        choice_id: Option<String>,
    },
    /// SendInput: the answer to the run's only open question when it is a REPLY to no role, else queued (a REPLY as a
    /// STEER, REQ-002's decision) for M3-T04 to deliver.
    Send {
        text: String,
        target_role: Option<String>,
        /// `ASK`, `STEER`, `CHANGE`, `CONTROL` or `REPLY`.
        input_kind: String,
    },
}

/// What a command came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Applied now: the stored result, the run's new revision and the event's seq.
    Accepted {
        result_json: String,
        revision: i64,
        event_seq: i64,
    },
    /// The same key and payload were accepted before: the result stored then.
    Replayed { result_json: String },
}

/// The project a host serves and its new host epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStart {
    pub project_id: String,
    pub host_epoch: i64,
}

/// A run as the queries see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: String,
    pub project_id: String,
    pub state: String,
    pub mode: String,
    pub revision: i64,
    pub resume_state: Option<String>,
    pub blocked_reason: Option<String>,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub project_id: String,
    pub event_id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

/// Apply `command` in one IMMEDIATE transaction; any error drops (rolls back) the transaction.
pub(super) fn submit(
    conn: &mut Connection,
    command: &SubmitCommand,
    ctx: &super::writer::Ctx<'_>,
) -> Result<CommandOutcome, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let outcome = submit_in(&tx, command, ctx);
    match outcome {
        Ok(CommandOutcome::Replayed { .. }) => outcome,
        Ok(_) => {
            writer::commit(tx, ctx)?;
            outcome
        }
        Err(PersistError::SpecInvalid(error)) => {
            super::spec::failed(&tx, command, &error, ctx)?;
            writer::commit(tx, ctx)?;
            Err(PersistError::SpecInvalid(error))
        }
        Err(error) => Err(error),
    }
}

/// The same command path inside a runtime input's accepting transaction.
pub(super) fn submit_in(
    tx: &rusqlite::Transaction<'_>,
    command: &SubmitCommand,
    ctx: &super::writer::Ctx<'_>,
) -> Result<CommandOutcome, PersistError> {
    let stored: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT payload_sha256, result_json FROM commands WHERE idempotency_key = ?1",
            params![command.idempotency_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((hash, result)) = stored {
        if hash != command.payload_sha256 {
            return Err(PersistError::KeyReuse(command.idempotency_key.clone()));
        }
        let result_json = result.ok_or_else(|| {
            PersistError::IntegrityViolation(format!(
                "command {} has no stored result",
                command.idempotency_key
            ))
        })?;
        return Ok(CommandOutcome::Replayed { result_json });
    }
    let mut team_revision = None;
    let (revision, event_seq) = match &command.target {
        RunTarget::Start {
            mode,
            budget,
            policy,
            assumptions,
            unattended,
            mode_cycle,
        } => {
            if command.expected_revision != 0 {
                return Err(PersistError::StaleRevision {
                    entity: writer::Entity::Run,
                    id: command.run_id.clone(),
                    expected: command.expected_revision,
                    actual: None,
                });
            }
            tx.execute(
                "INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES (?1, ?2, 'CREATED', ?3, 0, ?4)",
                params![
                    command.run_id,
                    command.event.project_id,
                    mode,
                    crate::time::now_rfc3339()
                ],
            )
            .map_err(sqlite_error)?;
            super::team::start_run(
                tx,
                &command.run_id,
                &command.event.project_id,
                super::team::Frozen {
                    mode_cycle,
                    budget,
                    policy,
                    assumptions,
                    unattended: *unattended,
                },
            )?;
            let resource_events = super::resources::events(
                tx,
                &command.event.project_id,
                &command.run_id,
                super::team::current(tx, &command.event.project_id)?
                    .1
                    .team
                    .into_keys(),
                ctx.resource_bundle,
            )?;
            // The project's latest approved specification, adopted after run.created; with none, the run is BLOCKED
            // on its question runtime:no_spec in this commit (docs/adr/ADR-031.md decision 1).
            let project_id = &command.event.project_id;
            let adopted = super::adoption::adopt(tx, project_id, &command.run_id)?;
            let asks = adopted.is_none();
            let done = writer::write(
                tx,
                &Commit {
                    events: std::iter::once(command.event.clone())
                        .chain(adopted)
                        .chain(resource_events)
                        .collect(),
                    outbox: command.outbox.clone().into_iter().collect(),
                    ..Commit::default()
                },
                ctx,
            )?;
            if asks {
                super::adoption::ask(tx, project_id, &command.run_id, ctx)?;
            }
            // The 202 body's revision is the run's after this transaction, as for every other run command: 0, or 1
            // once its question blocked it.
            let revision: i64 = tx
                .query_row(
                    "SELECT revision FROM runs WHERE run_id = ?1",
                    params![command.run_id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            (revision, done.event_seqs[0])
        }
        RunTarget::Team(change) => {
            if !command.run_id.is_empty() {
                let found: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM runs WHERE run_id = ?1",
                        params![command.run_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                if found.is_none() {
                    return Err(PersistError::RunNotFound(command.run_id.clone()));
                }
                // A run keeps its snapshot's scheduler (CR-007 S3): refused after the key was looked up (M2T07R-05).
                if change.scheduler.is_some() {
                    return Err(PersistError::RunConfigFrozen(command.run_id.clone()));
                }
            }
            let made = super::team::configure(tx, command, change)?;
            let revision = made.revision;
            team_revision = Some(made);
            let done = writer::write(
                tx,
                &Commit {
                    events: vec![command.event.clone()],
                    outbox: command.outbox.clone().into_iter().collect(),
                    ..Commit::default()
                },
                ctx,
            )?;
            (revision, done.event_seqs[0])
        }
        RunTarget::SetMode(mode) => {
            super::hold::command(tx, &command.run_id)?;
            super::modes::set(tx, command, *mode, ctx)?
        }
        RunTarget::Input(input) => input_command(tx, command, input, ctx)?,
        RunTarget::Specification(input) => {
            super::hold::command(tx, &command.run_id)?;
            super::spec::apply(tx, command, input, ctx)?
        }
        target => {
            // A run command: every event of its run in this transaction is the command's, written even while the run
            // is stopped (docs/adr/ADR-031.md correction D: an INCOMPLETE run takes commands).
            super::hold::command(tx, &command.run_id)?;
            let run: Option<(String, Option<String>, String)> = tx
                .query_row(
                    "SELECT state, resume_state, project_id FROM runs WHERE run_id = ?1",
                    params![command.run_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((state, saved, project_id)) = run else {
                return Err(PersistError::RunNotFound(command.run_id.clone()));
            };
            let mut events = Vec::new();
            let to = match target {
                RunTarget::To(to) => *to,
                RunTarget::Resume => {
                    let from = writer::parse::<RunState>(&state)?;
                    let to = saved
                        .as_deref()
                        .map(writer::parse::<RunState>)
                        .transpose()?
                        .filter(|_| STOPS.contains(&from));
                    let Some(to) = to else {
                        return Err(PersistError::IllegalTransition {
                            entity: writer::Entity::Run,
                            id: command.run_id.clone(),
                            from: state.clone(),
                            to: "resume_state".to_owned(),
                            why: "only a run in a stop state (PAUSED, INCOMPLETE, WAITING_EXTERNAL, BLOCKED) \
                                  resumes, to the state it stopped in"
                                .to_owned(),
                            details: Some(Box::new(serde_json::json!({"state": state}))),
                        });
                    };
                    // Reconcile first, then the stop's own predicate (docs/adr/ADR-031.md decision 16 (1), (2)).
                    events.push(resume_reconcile(tx, &project_id, from, command)?);
                    if let Some(details) = still_stopped(tx, &command.run_id, from, ctx)? {
                        return Err(PersistError::IllegalTransition {
                            entity: writer::Entity::Run,
                            id: command.run_id.clone(),
                            from: state,
                            to: "resume_state".to_owned(),
                            why: format!("its stop still holds: {details}"),
                            details: Some(Box::new(details)),
                        });
                    }
                    to
                }
                RunTarget::Start { .. }
                | RunTarget::Team(_)
                | RunTarget::Input(_)
                | RunTarget::Specification(_)
                | RunTarget::SetMode(_) => {
                    unreachable!("handled above")
                }
            };
            let at = events.len();
            events.push(command.event.clone());
            // A resume while blockers are claimed returns to BLOCKED, its resume state kept for when they are all
            // released (docs/adr/ADR-025.md decision 5).
            let mut to = to;
            let mut cas = Vec::new();
            if *target == RunTarget::Resume {
                let reasons = super::blockers::recorded(tx, &command.run_id)?;
                if !reasons.is_empty() {
                    to = RunState::Blocked;
                    let (moves, withdrawn) = super::control::withdraw(
                        tx,
                        &project_id,
                        &command.run_id,
                        &crate::time::now_rfc3339(),
                        super::control::Withdraw::Blocked,
                        ctx,
                    )?;
                    cas.extend(moves);
                    events.extend(withdrawn);
                    events.push(NewEvent {
                        intent: Intent::Progress,
                        project_id: project_id.clone(),
                        event_id: crate::ids::child_id(&command.run_id, &format!("resumed-blocked/{}", command.command_id)),
                        run_id: Some(command.run_id.clone()),
                        kind: "run.blocked".to_owned(),
                        payload: serde_json::json!({"reasons": reasons, "detail": "resumed with its blockers claimed"}),
                    });
                }
            }
            // A notice to a role is addressed to its binding as it is now, in this transaction: after a handover the
            // commander's epoch is no longer the one the caller may have written (docs/adr/ADR-024.md decision 6).
            let mut outbox = Vec::new();
            for o in command.outbox.iter().cloned() {
                let current: Option<i64> = tx
                    .query_row(
                        "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
                        params![command.run_id, o.recipient_role],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                outbox.push(NewOutbox {
                    event: o.event + at,
                    role_epoch: current.unwrap_or(o.role_epoch),
                    ..o
                });
            }
            let run_at = cas.len();
            cas.push(StateCas {
                id: command.run_id.clone(),
                expected_revision: command.expected_revision,
                change: Change::Run(to),
            });
            let done = writer::write(
                tx,
                &Commit {
                    cas,
                    events,
                    outbox,
                },
                ctx,
            )?;
            (done.revisions[run_at], done.event_seqs[at])
        }
    };

    // A cancel marks the run's attempts not yet ended in the same transaction: each ends CANCELLED, before any other
    // disposition (REQ-043 behaviour 3, docs/adr/ADR-021.md decision 8b and 8e).
    if command.target == RunTarget::To(RunState::CancelRequested) {
        tx.execute(
            "UPDATE attempts SET cancel_requested = 1 WHERE state IN ('PREPARED', 'STARTING', 'RUNNING') \
             AND task_id IN (SELECT task_id FROM tasks WHERE run_id = ?1)",
            params![command.run_id],
        )
        .map_err(sqlite_error)?;
    }
    // The exact 202 body (openapi submitCommand); a replay returns these bytes.
    // A project's command (ConfigureTeam with a null run_id) names no run.
    let run_id = (!command.run_id.is_empty()).then_some(&command.run_id);
    let result_json = serde_json::json!({
        "command_id": command.command_id,
        "run_id": run_id,
        "revision": revision,
        "status": "ACCEPTED",
    })
    .to_string();
    tx.execute(
        "INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, \
         expected_revision, result_json, accepted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            command.command_id,
            run_id,
            command.idempotency_key,
            command.kind,
            command.payload.to_string(),
            command.payload_sha256,
            command.expected_revision,
            result_json,
            crate::time::now_rfc3339(),
        ],
    )
    .map_err(sqlite_error)?;
    if let Some(made) = &team_revision {
        super::team::record(tx, &command.event.project_id, &command.command_id, made)?;
    }
    Ok(CommandOutcome::Accepted {
        result_json,
        revision,
        event_seq,
    })
}

/// An input command `command` of `input` in its transaction (docs/adr/ADR-031.md decision 7, E3, E5): its run must
/// exist, not have ended (409 ILLEGAL_TRANSITION with `details.state`) and be at `expected_revision`. Its acceptance
/// event first (`input.accepted{command_id, input_id, kind, question_id}`, or `question.answer_accepted{command_id,
/// question_id}`), then the answer's exit (`questions::answer`) or the queued input; the run's revision moved; its
/// result judged at the end. SendInput business refusals instead persist a rejection
/// Record and leave the run unchanged (ADR-036). Returns the revision and event seq.
fn input_command(
    tx: &rusqlite::Transaction<'_>,
    command: &SubmitCommand,
    input: &Input,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    let run: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT state, revision, project_id FROM runs WHERE run_id = ?1",
            params![command.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, project_id)) = run else {
        return Err(PersistError::RunNotFound(command.run_id.clone()));
    };
    if writer::parse::<RunState>(&state)?.is_terminal() {
        return Err(PersistError::IllegalTransition {
            entity: writer::Entity::Run,
            id: command.run_id.clone(),
            from: state.clone(),
            to: "input".to_owned(),
            why: format!("Run 已结束（{state}），不再受理输入"),
            details: Some(Box::new(serde_json::json!({"state": state}))),
        });
    }
    if revision != command.expected_revision {
        return Err(PersistError::StaleRevision {
            entity: writer::Entity::Run,
            id: command.run_id.clone(),
            expected: command.expected_revision,
            actual: Some(revision),
        });
    }
    let apply = || apply_input(tx, command, input, (&state, revision, &project_id), ctx);
    let Input::Send {
        text,
        target_role,
        input_kind,
    } = input
    else {
        return apply();
    };
    let rejected =
        if input_kind == "CONTROL" && super::inputs::runtime::control_kind(text).is_none() {
            super::inputs::Rejection::new("unknown_control")
        } else {
            match super::inputs::application(tx, apply) {
                Ok(result) => return Ok(result),
                Err(error) => match super::inputs::Rejection::from_error(&error, &command.run_id) {
                    Some(rejection) => rejection,
                    None => return Err(error),
                },
            }
        };
    // The failed application (including temp.command_run) is gone. Only a Record
    // and the durable input/receipt remain; the outer command records its replay.
    let seq = super::inputs::runtime::reject_input(
        tx,
        &super::inputs::NewInput {
            input_id: crate::ids::child_id(&command.command_id, "input"),
            run_id: command.run_id.clone(),
            kind: input_kind.clone(),
            target_role: target_role.clone(),
            payload: serde_json::json!({"text":text}),
        },
        &rejected,
        ctx,
    )?;
    Ok((revision, seq))
}

fn apply_input(
    tx: &rusqlite::Transaction<'_>,
    command: &SubmitCommand,
    input: &Input,
    run: (&str, i64, &str),
    ctx: &super::writer::Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    let (state, revision, project_id) = run;
    // What it answers, if anything, and the input row a SendInput keeps.
    let input_id = crate::ids::child_id(&command.command_id, "input");
    let mut accepted = command.event.clone();
    let (answer, queued) = match input {
        Input::Answer {
            question_id,
            answer,
            choice_id,
        } => {
            accepted.payload["question_id"] = serde_json::json!(question_id);
            (
                Some((question_id.clone(), answer.clone(), choice_id.clone())),
                None,
            )
        }
        Input::Send {
            text,
            target_role,
            input_kind,
        } => {
            let only = if input_kind == "REPLY" && target_role.is_none() {
                let open = super::questions::open(tx, &command.run_id)?;
                (open.len() == 1)
                    .then(|| open.into_iter().next().map(|q| q.question_id))
                    .flatten()
            } else {
                None
            };
            let kind = match (&only, input_kind.as_str()) {
                (Some(_), _) => "REPLY",
                (None, "REPLY") => "STEER",
                (None, other) => other,
            };
            accepted.payload["input_id"] = serde_json::json!(input_id);
            accepted.payload["kind"] = serde_json::json!(kind);
            accepted.payload["question_id"] = serde_json::json!(only);
            match only {
                Some(question_id) => {
                    tx.execute(
                        "INSERT INTO input_queue(input_id, run_id, kind, target_role, state, payload_json) \
                         VALUES (?1, ?2, 'REPLY', NULL, 'APPLIED', ?3)",
                        params![
                            input_id,
                            command.run_id,
                            serde_json::json!({"text": text, "question_id": question_id}).to_string()
                        ],
                    )
                    .map_err(sqlite_error)?;
                    (Some((question_id, text.clone(), None)), None)
                }
                None => (
                    None,
                    Some(super::inputs::NewInput {
                        input_id: input_id.clone(),
                        run_id: command.run_id.clone(),
                        kind: kind.to_owned(),
                        target_role: target_role.clone(),
                        payload: serde_json::json!({"text": text}),
                    }),
                ),
            }
        }
    };
    super::hold::command(tx, &command.run_id)?;
    // A working ASK is read-only (AC-017-01). In a stopped state the accepted
    // command still moves the revision and closes with a fresh result (ADR-031 E3).
    let publish_result = queued.as_ref().is_none_or(|input| input.kind != "CONTROL")
        && (queued.as_ref().is_none_or(|input| input.kind != "ASK")
            || matches!(state, "BLOCKED" | "PAUSED" | "INCOMPLETE"));
    // The acceptance event is the commit's first (decision 9 finds it by its command_id).
    let done = writer::write(
        tx,
        &Commit {
            events: vec![accepted],
            ..Commit::default()
        },
        ctx,
    )?;
    let seq = done.event_seqs[0];
    if let Some((question_id, answer, choice_id)) = answer {
        super::questions::answer(
            tx,
            &super::questions::Answer {
                project_id,
                run_id: &command.run_id,
                question_id: &question_id,
                answer: &answer,
                choice_id: choice_id.as_deref(),
                command_id: &command.command_id,
            },
            ctx,
        )?;
    } else {
        if let Some(input) = &queued {
            super::inputs::enqueue(tx, input, ctx)?;
            if input.kind == "CONTROL" {
                super::inputs::runtime::control(tx, &input.input_id, ctx)?;
            }
        }
        if publish_result {
            writer::write(
                tx,
                &Commit {
                    cas: vec![StateCas {
                        id: command.run_id.clone(),
                        expected_revision: revision,
                        change: Change::Touch(writer::Entity::Run),
                    }],
                    ..Commit::default()
                },
                ctx,
            )?;
        }
    }
    // The stopped-state command's result has its new revision; working ASK emits none.
    if publish_result {
        writer::result_due(tx, &command.run_id)?;
    }
    let now: i64 = tx
        .query_row(
            "SELECT revision FROM runs WHERE run_id = ?1",
            params![command.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok((now, seq))
}

/// The stop states a ResumeRun takes a run out of (REQ-036 decision 2, docs/adr/ADR-031.md decision 16).
const STOPS: [RunState; 4] = [
    RunState::Paused,
    RunState::Incomplete,
    RunState::WaitingExternal,
    RunState::Blocked,
];

/// What still holds of the stop of run `run_id`, in stop state `from`, judged in the command's transaction at the
/// host's clock (the store's, the one the pace's context carries; docs/adr/ADR-031.md decision 16): the pace's own
/// predicates, never another condition — INCOMPLETE's budget stop, WAITING_EXTERNAL's earliest retry still to come with
/// no task due, BLOCKED's blocker set (never empty in BLOCKED); PAUSED has none (only the user's request holds it). The
/// refusal's `details` (`state` and that item); `None`, the run may resume.
fn still_stopped(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    from: RunState,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Option<serde_json::Value>, PersistError> {
    let state = from.as_str();
    Ok(match from {
        RunState::Incomplete => super::retry::budget_and_stop(tx, run_id, ctx.now(), ctx.wall())?
            .1
            .map(|stop| serde_json::json!({"state": state, "stop": stop.as_str()})),
        RunState::WaitingExternal => super::retry::waiting_until(tx, run_id, ctx.now())?
            .map(|next| serde_json::json!({"state": state, "next_retry_at": next})),
        RunState::Blocked => {
            let reasons = super::blockers::recorded(tx, run_id)?;
            (!reasons.is_empty()).then(|| serde_json::json!({"state": state, "reasons": reasons}))
        }
        _ => None,
    })
}

/// A resume's reconcile of its run, recorded before the run works again so that every later dispatch comes after it
/// (REQ-043 behaviour 2, docs/adr/ADR-021.md decision 7): from which stop state, the run's attempts still unfinished
/// (none is expected in any of the four: an attempt not finished refuses the resume, named by that state, decision 16
/// (1)), those whose effect is unknown, and the leases its attempts hold.
fn resume_reconcile(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    from: RunState,
    command: &SubmitCommand,
) -> Result<NewEvent, PersistError> {
    let ids = |sql: &str| -> Result<Vec<String>, PersistError> {
        let mut statement = tx.prepare(sql).map_err(sqlite_error)?;
        statement
            .query_map(params![command.run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)
    };
    let unfinished = ids(
        "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
         AND a.state IN ('PREPARED', 'STARTING', 'RUNNING') ORDER BY a.rowid",
    )?;
    let unknown = ids(
        "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
         AND a.state = 'UNKNOWN_EFFECT' ORDER BY a.rowid",
    )?;
    let leases = ids(
        "SELECT l.kind FROM leases l JOIN attempts a ON a.attempt_id = l.attempt_id JOIN tasks t ON t.task_id = a.task_id \
         WHERE t.run_id = ?1 ORDER BY l.kind",
    )?;
    if !unfinished.is_empty() {
        return Err(PersistError::IllegalTransition {
            entity: writer::Entity::Run,
            id: command.run_id.clone(),
            from: from.as_str().to_owned(),
            to: "resume_state".to_owned(),
            why: format!("attempts of it are not finished: {}", unfinished.join(", ")),
            details: None,
        });
    }
    Ok(NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: crate::ids::child_id(&command.run_id, &format!("resume/{}", command.command_id)),
        run_id: Some(command.run_id.clone()),
        kind: "reconcile.run".to_owned(),
        payload: serde_json::json!({
            "run_id": command.run_id,
            "command_id": command.command_id,
            "from": from.as_str(),
            "unfinished": unfinished,
            "unknown_effect": unknown,
            "leases": leases,
        }),
    })
}

/// The host's start on the database (REQ-041): an integrity check, the project of `repo_path` (made if new, with
/// `project_id`), and its host epoch one higher.
pub(super) fn begin_host(
    conn: &mut Connection,
    project_id: &str,
    repo_path: &str,
) -> Result<HostStart, PersistError> {
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(sqlite_error)?;
    if check != "ok" {
        return Err(PersistError::IntegrityViolation(format!(
            "the database failed its integrity check: {check}"
        )));
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(repo_path) DO NOTHING",
        params![project_id, repo_path, crate::time::now_rfc3339()],
    )
    .map_err(sqlite_error)?;
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM projects WHERE repo_path = ?1",
            params![repo_path],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let host_epoch: i64 = tx
        .query_row(
            "UPDATE projects SET host_epoch = host_epoch + 1 WHERE project_id = ?1 RETURNING host_epoch",
            params![project_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(HostStart {
        project_id,
        host_epoch,
    })
}

/// The highest event seq, 0 before the first event.
pub(super) fn max_seq(conn: &Connection) -> Result<i64, PersistError> {
    conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))
        .map_err(sqlite_error)
}

/// The run `run_id` and the highest event seq, read in one read transaction.
pub(super) fn snapshot(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, i64)>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = snapshot_rows(conn, run_id);
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// The run `run_id` and the highest event seq, inside the caller's read transaction.
pub(super) fn snapshot_rows(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, i64)>, PersistError> {
    {
        let run = conn
            .query_row(
                "SELECT run_id, project_id, state, mode, revision, resume_state, blocked_reason FROM runs \
                 WHERE run_id = ?1",
                params![run_id],
                |r| {
                    Ok(RunRow {
                        run_id: r.get(0)?,
                        project_id: r.get(1)?,
                        state: r.get(2)?,
                        mode: r.get(3)?,
                        revision: r.get(4)?,
                        resume_state: r.get(5)?,
                        blocked_reason: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok::<_, PersistError>(match run {
            Some(run) => Some((run, max_seq(conn)?)),
            None => None,
        })
    }
}

/// The run `run_id`, its team and the highest event seq, read in one read transaction.
pub(super) fn run_view(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, super::team::TeamView, i64)>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let Some((run, _)) = snapshot_rows(conn, run_id)? else {
            return Ok(None);
        };
        let team = super::team::view(conn, run_id, &run.project_id)?;
        Ok::<_, PersistError>(Some((run, team, max_seq(conn)?)))
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// A run as getRun shows it: its row, its team, its acceptance summary, its result as of the watermark, and the
/// watermark it was read at.
pub type RunProjection = (
    RunRow,
    super::team::TeamView,
    super::AcceptanceView,
    super::results::ResultView,
    i64,
    Vec<super::questions::Question>,
    Vec<DegradedTracking>,
    Vec<String>,
    Vec<super::readiness::SpecQuestion>,
    Option<crate::readiness::Readiness>,
);

/// An attempt of a run whose processes are tracked by the degraded /proc scan, as its `attempt.tracking` recorded it
/// (docs/adr/ADR-021.md decision 8d, M2T06R-560): the attempt and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DegradedTracking {
    pub attempt_id: String,
    pub reason: String,
}

/// Run `run_id`'s attempts tracked by the /proc scan, in the order their tracking was recorded.
fn degraded_tracking(
    conn: &Connection,
    run_id: &str,
) -> Result<Vec<DegradedTracking>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.attempt_id'), coalesce(json_extract(payload_json, '$.reason'), '') \
             FROM events WHERE run_id = ?1 AND kind = 'attempt.tracking' \
             AND json_extract(payload_json, '$.tracking') = 'proc' ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok(DegradedTracking {
                attempt_id: r.get(0)?,
                reason: r.get(1)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}
/// [`run_view`] with the run's acceptance snapshot (docs/adr/ADR-029.md decision 5), in one read transaction.
pub(super) fn run_projection(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<RunProjection>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let Some((run, _)) = snapshot_rows(conn, run_id)? else {
            return Ok(None);
        };
        let team = super::team::view(conn, run_id, &run.project_id)?;
        let watermark = max_seq(conn)?;
        let Some(acceptance) = super::snapshot::view_in(conn, run_id, watermark)? else {
            return Ok(None);
        };
        // The run's result as of the watermark (docs/adr/ADR-031.md decision 12): in the same read.
        let result = super::results::view(conn, run_id, watermark)?;
        // Its open questions (docs/adr/ADR-031.md decision 6), in the same read.
        let questions = super::questions::open(conn, run_id)?;
        // Its attempts tracked degraded (docs/adr/ADR-021.md decision 8d, M2T06R-560), in the same read.
        let degraded = degraded_tracking(conn, run_id)?;
        // A client resolves a multi-role agent against running tasks at the current binding epoch.
        let mut statement = conn.prepare(
            "SELECT DISTINCT t.role FROM tasks t JOIN attempts a ON a.task_id=t.task_id \
             JOIN role_bindings b ON b.run_id=t.run_id AND b.role=t.role AND b.role_epoch=a.role_epoch \
             WHERE t.run_id=?1 AND t.state='RUNNING' AND a.state='RUNNING' ORDER BY t.role"
        ).map_err(sqlite_error)?;
        let active_roles = statement
            .query_map(params![run_id], |r| r.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        // Its revision's open questions (REQ-031 behaviour 6), in the same read.
        let open_questions = super::readiness::run_questions(conn, run_id)?;
        // Its readiness now (REQ-031 behaviour 1), none without a specification, in the same read.
        let readiness = super::readiness::run_readiness(conn, run_id)?;
        Ok::<_, PersistError>(Some((
            run,
            team,
            acceptance,
            result,
            watermark,
            questions,
            degraded,
            active_roles,
            open_questions,
            readiness,
        )))
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// A page of runs: each with its creation time, the next page's cursor if there may be one, and the watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsPage {
    pub runs: Vec<ListedRun>,
    pub next: Option<i64>,
    pub watermark: i64,
}

/// A run of a page (docs/adr/ADR-031.md decision 13): its row, its creation time, its goal (its StartRun's, if one
/// that is not blank) and, for a COMPLETED run only, the candidate it accepted last (`None` for any other run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedRun {
    pub run: RunRow,
    pub created_at: String,
    pub goal: Option<String>,
    pub accepted: Option<Option<AcceptedCandidate>>,
}

/// The candidate a run accepted last: its ACCEPTED candidate whose `candidate.accepted` event has the greatest seq.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCandidate {
    pub candidate_id: String,
    pub git_commit: String,
    pub seq: i64,
}

/// The candidate `run_id` accepted last (see [`AcceptedCandidate`]), if any.
fn accepted_last(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<AcceptedCandidate>, PersistError> {
    conn.query_row(
        "SELECT c.candidate_id, c.git_commit, e.seq FROM candidates c JOIN events e \
           ON e.run_id = c.run_id AND e.kind = 'candidate.accepted' \
           AND json_extract(e.payload_json, '$.candidate_id') = c.candidate_id \
         WHERE c.run_id = ?1 AND c.state = 'ACCEPTED' ORDER BY e.seq DESC LIMIT 1",
        params![run_id],
        |r| {
            Ok(AcceptedCandidate {
                candidate_id: r.get(0)?,
                git_commit: r.get(1)?,
                seq: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// A page of the project's runs, newest first (openapi `listRuns`): at most `limit` runs older than `before` (an
/// opaque cursor: the rowid of the last run of the previous page), only in `state` if given; each with its creation
/// time, its goal and a COMPLETED run's candidate accepted last (docs/adr/ADR-031.md decision 13), the cursor of the
/// next page if there may be one, and the watermark the page is consistent with.
pub(super) fn list_runs(
    conn: &Connection,
    state: Option<&str>,
    limit: usize,
    before: Option<i64>,
) -> Result<RunsPage, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let mut statement = conn
            .prepare(
                "SELECT run_id, project_id, state, mode, revision, resume_state, blocked_reason, created_at, rowid \
                 FROM runs WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR rowid < ?2) ORDER BY rowid DESC LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(RunRow, String, i64)> = statement
            .query_map(
                params![state, before, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| {
                    Ok((
                        RunRow {
                            run_id: r.get(0)?,
                            project_id: r.get(1)?,
                            state: r.get(2)?,
                            mode: r.get(3)?,
                            revision: r.get(4)?,
                            resume_state: r.get(5)?,
                            blocked_reason: r.get(6)?,
                        },
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let next = (rows.len() == limit)
            .then(|| rows.last().map(|r| r.2))
            .flatten();
        let watermark = max_seq(conn)?;
        let runs = rows
            .into_iter()
            .map(|(run, created_at, _)| {
                let goal = super::results::started_goal(conn, &run.run_id)?;
                let accepted = if run.state == RunState::Completed.as_str() {
                    Some(accepted_last(conn, &run.run_id)?)
                } else {
                    None
                };
                Ok(ListedRun {
                    run,
                    created_at,
                    goal,
                    accepted,
                })
            })
            .collect::<Result<_, PersistError>>()?;
        Ok::<_, PersistError>(RunsPage {
            runs,
            next,
            watermark,
        })
    })();
    let _ = conn.execute_batch("COMMIT");
    read
}

/// The payload of the latest event of `kind` about attempt `attempt_id` (its payload's `attempt_id`), if any.
pub(super) fn attempt_event(
    conn: &Connection,
    kind: &str,
    attempt_id: &str,
) -> Result<Option<serde_json::Value>, PersistError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = ?1 AND json_extract(payload_json, '$.attempt_id') = ?2 \
             ORDER BY seq DESC LIMIT 1",
            params![kind, attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    text.map(|t| {
        serde_json::from_str(&t).map_err(|e| PersistError::IntegrityViolation(e.to_string()))
    })
    .transpose()
}

/// At most `limit` events with seq above `after`, in seq order.
pub(super) fn events_after(
    conn: &Connection,
    after: i64,
    limit: usize,
) -> Result<Vec<EventRow>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT seq, project_id, event_id, run_id, kind, payload_json, created_at FROM events \
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(
            params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .map_err(sqlite_error)?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, project_id, event_id, run_id, kind, payload, created_at) =
            row.map_err(sqlite_error)?;
        let payload = serde_json::from_str(&payload).map_err(|e| {
            PersistError::IntegrityViolation(format!("event {seq} has an unreadable payload: {e}"))
        })?;
        events.push(EventRow {
            seq,
            project_id,
            event_id,
            run_id,
            kind,
            payload,
            created_at,
        });
    }
    Ok(events)
}

/// Whether the connection refuses a write: a statement that would write nothing still needs a write transaction.
pub(super) fn refuses_writes(conn: &Connection) -> bool {
    conn.execute("UPDATE projects SET host_epoch = host_epoch WHERE 0", [])
        .is_err()
}
