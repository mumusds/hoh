//! Commander decisions in the store (REQ-035, docs/adr/ADR-011.md §3). Recording the proposal, reading the snapshot
//! its verdict depends on, and writing the outcome are one IMMEDIATE transaction: an applied decision moves its
//! task/run by compare-and-set and queues its dispatch together with the APPLIED row; a rejected one changes no task
//! or run and writes exactly one `decision.rejected` event.

use super::attempts::{self, NewAttempt};
use super::leases;
use super::writer::{self, Change, Commit, Entity, Intent, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::decision::{
    self, BindingView, COMMANDER, Context, Effect, Evidence, Proposal, Rejection, RunView,
    Snapshot, TaskView,
};
use crate::lifecycle::Lifecycle;
use crate::protocol::{self, Message, RejectionReason, RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The outcome of a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decided {
    /// Applied: the run's new revision and the seqs of the events written.
    Applied {
        run_revision: i64,
        event_seqs: Vec<i64>,
    },
    /// Rejected and kept as history, with its one `decision.rejected` event.
    Rejected {
        reason: RejectionReason,
        detail: String,
        event_seq: i64,
    },
    /// Its decision id was decided before with the same content: nothing written again; the state it got.
    Replayed { state: String },
    /// Its decision id was decided before with other content: nothing written (KEY_REUSE).
    KeyReuse,
    /// Past every fence, of a kind its answer applies (`request_user_decision`, docs/adr/ADR-031.md decision 7): its
    /// decisions row is written PROPOSED inside the caller's transaction, nothing else (docs/adr/ADR-025.md v13
    /// decision 8). [`decide`] gives [`PersistError::Unsupported`] for it and writes nothing.
    Unsupported { kind: &'static str },
}

/// See [`crate::ids::child_id`].
pub(super) fn child_id(parent: &str, suffix: &str) -> String {
    crate::ids::child_id(parent, suffix)
}

pub(super) fn to_i64(value: u64, field: &str) -> Result<i64, PersistError> {
    i64::try_from(value)
        .map_err(|_| PersistError::InvalidProposal(format!("{field} {value} is out of range")))
}

fn reason_name(reason: RejectionReason) -> String {
    match serde_json::to_value(reason) {
        Ok(Value::String(name)) => name,
        _ => format!("{reason:?}"),
    }
}

pub(super) fn decide(
    conn: &mut Connection,
    proposal: &Proposal,
    context: &Context,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Decided, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let decided = decide_in(&tx, proposal, context, ctx, None, None)?;
    if let Decided::Unsupported { kind } = decided {
        return Err(PersistError::Unsupported(kind));
    }
    if !matches!(decided, Decided::Replayed { .. } | Decided::KeyReuse) {
        super::writer::commit_unhooked(tx, ctx)?;
    }
    Ok(decided)
}

/// The turn a decision answers: its wake's latest allocation, as `supervisor.allocated` recorded it.
struct Answered {
    wake: String,
    fingerprint: String,
    spec_id: Option<String>,
    snapshots: serde_json::Map<String, Value>,
    run_revision: i64,
    role_epoch: i64,
}

fn answered(
    tx: &Transaction<'_>,
    run_id: &str,
    causation: Option<&str>,
) -> Result<Option<Answered>, PersistError> {
    let Some(wake) = causation else {
        return Ok(None);
    };
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE run_id = ?1 AND kind = 'supervisor.allocated' \
             AND json_extract(payload_json, '$.wake') = ?2 ORDER BY seq DESC LIMIT 1",
            params![run_id, wake],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("allocation of {wake}: {e}")))?;
    answered_by(&payload).map(Some)
}

/// The turn an allocation's `payload` gave.
fn answered_by(payload: &Value) -> Result<Answered, PersistError> {
    let wake = payload["wake"].as_str().unwrap_or_default();
    Ok(Answered {
        wake: wake.to_owned(),
        fingerprint: payload["dedup_key"]["fingerprint"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        spec_id: payload["dedup_key"]["spec_id"].as_str().map(str::to_owned),
        snapshots: payload["snapshots"]
            .as_object()
            .cloned()
            .unwrap_or_default(),
        run_revision: payload["run_revision"].as_i64().unwrap_or(-1),
        role_epoch: payload["role_epoch"].as_i64().unwrap_or(-1),
    })
}

/// The task-snapshot fence (docs/adr/ADR-025.md decision 6): a task the answered turn was shown has moved since; all
/// its snapshots compared in one statement, the first moved by task id (v18, M2T08R-301).
fn fenced(tx: &Transaction<'_>, turn: &Answered) -> Result<Option<Rejection>, PersistError> {
    if turn.snapshots.is_empty() {
        return Ok(None);
    }
    let moved: Option<(String, Option<i64>, Option<i64>)> = tx
        .query_row(
            "SELECT j.key, j.value, t.revision FROM json_each(?1) j LEFT JOIN tasks t ON t.task_id = j.key \
             WHERE t.revision IS NOT j.value ORDER BY j.key LIMIT 1",
            params![Value::Object(turn.snapshots.clone()).to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((task, shown, now)) = moved {
        let shown = shown.map_or_else(|| "none".to_owned(), |n| n.to_string());
        {
            return Ok(Some(Rejection {
                reason: RejectionReason::StaleRevision,
                detail: format!(
                    "task {task} was at revision {shown} when wake {} was given its turn; it is at {} now",
                    turn.wake,
                    now.map_or_else(|| "none".to_owned(), |n| n.to_string())
                ),
                details: None,
            }));
        }
    }
    Ok(None)
}

/// `decide` inside the caller's transaction (docs/adr/ADR-025.md decision 8: the output bridge commits a decision with
/// its own record): a decision id seen before is a replay (same content) or a key reuse (other content), nothing
/// written; otherwise the decisions row, its verdict, its effect and its event, and for a stale answer to a wake its
/// one re-trigger.
/// `allocation`: the `supervisor.allocated` of the turn whose output this decision is, already read by the caller (its
/// own turn is the one it answers; v18, M2T08R-301): none to look the wake's latest up.
pub(super) fn decide_in(
    tx: &Transaction<'_>,
    proposal: &Proposal,
    context: &Context,
    ctx: &super::writer::Ctx<'_>,
    output_id: Option<&str>,
    allocation: Option<&Value>,
) -> Result<Decided, PersistError> {
    // The payload is checked against $defs.DecisionPayload again on the way in, whoever built the proposal.
    let payload = protocol::encode(&proposal.payload)
        .map_err(|e| PersistError::InvalidProposal(format!("{}: {e}", proposal.decision_id)))?;
    let role_epoch = to_i64(proposal.role_epoch, "role_epoch")?;
    let expected_revision = to_i64(proposal.expected_revision, "expected_revision")?;
    let seen: Option<(String, i64, i64, String, String)> = tx
        .query_row(
            "SELECT run_id, role_epoch, expected_revision, payload_json, state FROM decisions WHERE decision_id = ?1",
            params![proposal.decision_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((run_id, epoch, revision, recorded, state)) = seen {
        let same = run_id == proposal.run_id
            && epoch == role_epoch
            && revision == expected_revision
            && serde_json::from_str::<Value>(&recorded).ok().as_ref() == Some(&payload);
        return Ok(if same {
            Decided::Replayed { state }
        } else {
            Decided::KeyReuse
        });
    }
    let (project_id, run) = load_run(tx, &proposal.run_id)?;
    // A create_tasks's plan is checked before its decisions row: an invalid plan is no decision, refused with its
    // plan error (docs/adr/ADR-031.md decision 2); a valid one is imported if the decision is applied.
    let mut plan = match &proposal.payload {
        protocol::DecisionPayload::CreateTasks { .. } => {
            Some(super::plan::check(tx, &proposal.run_id, &payload["plan"])?)
        }
        _ => None,
    };
    tx.execute(
        "INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, payload_json, state) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'PROPOSED')",
        params![
            proposal.decision_id,
            proposal.run_id,
            role_epoch,
            expected_revision,
            payload.to_string()
        ],
    )
    .map_err(sqlite_error)?;
    let mut snapshot = load_snapshot(tx, &project_id, proposal, context, run, ctx)?;
    // A dispatch of a task of the run is admitted in this transaction (docs/adr/ADR-035.md decision 12, M3J-100): its
    // refusal is the verdict's, its admission the lease's.
    let mut admission = match &proposal.payload {
        protocol::DecisionPayload::Dispatch { task_id, .. } if snapshot.task.is_some() => {
            match super::admission::admit_one(
                tx,
                &proposal.run_id,
                task_id,
                super::admission::By::Commander,
            )? {
                Ok(admitted) => Some(admitted),
                Err(refusal) => {
                    snapshot.admission = Some(refusal.rejection(task_id));
                    None
                }
            }
        }
        _ => None,
    };
    let turn = match allocation.filter(|a| a["wake"].as_str() == proposal.causation_id.as_deref()) {
        Some(allocation) => Some(answered_by(allocation)?),
        None => answered(tx, &proposal.run_id, proposal.causation_id.as_deref())?,
    };
    // The fences in order: the Commander's epoch, the run's revision, then the tasks its turn was shown.
    let mut verdict = match decision::validate(proposal, &snapshot) {
        Err(r)
            if matches!(
                r.reason,
                RejectionReason::StaleRoleEpoch | RejectionReason::StaleRevision
            ) =>
        {
            Err(r)
        }
        verdict => match turn.as_ref().map(|t| fenced(tx, t)).transpose()?.flatten() {
            Some(stale) => Err(stale),
            None => verdict,
        },
    };
    if verdict.is_ok()
        && let protocol::DecisionPayload::TransitionTask { task_id, .. } = &proposal.payload
        && super::consult::finished(tx, task_id)?
    {
        verdict = Err(Rejection {
            reason: RejectionReason::IllegalTransition,
            detail: "finished consult tasks cannot move".into(),
            details: None,
        });
    }
    if let Err(ref mut rejection) = verdict {
        explain_stale_completion(tx, proposal, rejection)?;
    }
    let verdict = match verdict {
        Ok(Effect::Dispatch { .. })
            if snapshot.task.as_ref().is_some_and(|task| task.writer)
                && super::modes::pending(tx, &proposal.run_id)?.is_some() =>
        {
            Err(Rejection {
                reason: RejectionReason::IllegalTransition,
                detail: "mode_change_pending: writer dispatch waits for the mode safe point"
                    .to_owned(),
                details: None,
            })
        }
        other => other,
    };
    let decided = match verdict {
        Ok(Effect::Unsupported(kind)) => return Ok(Decided::Unsupported { kind }),
        Ok(effect) => {
            let lease = match &effect {
                Effect::Dispatch {
                    task_id,
                    role,
                    role_epoch,
                    lease_kind,
                    ..
                } => Some(lease_task(
                    tx,
                    &LeaseRequest {
                        project_id: &project_id,
                        run_id: &proposal.run_id,
                        attempt_id: child_id(&proposal.decision_id, "attempt"),
                        task_id,
                        role,
                        role_epoch: *role_epoch,
                        kind: lease_kind,
                        now: context.now(),
                        admission: admission.take().ok_or_else(|| {
                            PersistError::IntegrityViolation(format!(
                                "dispatch {} applied without its admission",
                                proposal.decision_id
                            ))
                        })?,
                    },
                )?),
                _ => None,
            };
            let mut commit = applied(
                &project_id,
                proposal,
                &snapshot,
                &effect,
                &payload,
                lease.as_ref(),
            )?;
            commit.events[0].payload["causation_id"] = json!(proposal.causation_id);
            commit.events[0].payload["decided_at"] = json!(crate::time::rfc3339_utc(context.now()));
            if let Some(output_id) = output_id {
                commit.events[0].payload["output_id"] = json!(output_id);
            }
            let done = writer::write(tx, &commit, ctx)?;
            let mut event_seqs = done.event_seqs;
            if matches!(effect, Effect::CreateTasks) {
                let plan = plan.take().ok_or_else(|| {
                    PersistError::IntegrityViolation(format!(
                        "create_tasks {} applied without its checked plan",
                        proposal.decision_id
                    ))
                })?;
                event_seqs.push(super::plan::import_in(tx, plan, ctx)?.1);
            }
            resolve(tx, &proposal.decision_id, "APPLIED", None)?;
            Decided::Applied {
                run_revision: done.revisions[0],
                event_seqs,
            }
        }
        Err(rejection) => {
            let mut event = rejected(&project_id, proposal, &snapshot, context, &rejection);
            event.payload["causation_id"] = json!(proposal.causation_id);
            event.payload["decided_at"] = json!(crate::time::rfc3339_utc(context.now()));
            if let Some(output_id) = output_id {
                event.payload["output_id"] = json!(output_id);
            }
            let mut events = vec![event];
            // A stale answer to a wake is re-triggered, exactly once (REQ-021 behaviour 6, decision 6).
            if let Some(turn) = turn.as_ref().filter(|_| {
                matches!(
                    rejection.reason,
                    RejectionReason::StaleRevision | RejectionReason::StaleRoleEpoch
                )
            }) {
                events.push(retrigger(tx, &project_id, proposal, &rejection, turn)?);
            }
            let commit = Commit {
                events,
                ..Commit::default()
            };
            let done = writer::write(tx, &commit, ctx)?;
            resolve(
                tx,
                &proposal.decision_id,
                "REJECTED",
                Some(rejection.reason),
            )?;
            Decided::Rejected {
                reason: rejection.reason,
                detail: rejection.detail,
                event_seq: done.event_seqs[0],
            }
        }
    };
    Ok(decided)
}

/// Refine an already refused completion's diagnostic for revision-bound stale evidence (REQ-033).
/// The gate and its acceptance predicate are unchanged; all other candidate mismatches keep their reason.
fn explain_stale_completion(
    tx: &Transaction<'_>,
    proposal: &Proposal,
    rejection: &mut Rejection,
) -> Result<(), PersistError> {
    if rejection.reason != RejectionReason::CandidateMismatch
        || !matches!(
            proposal.payload,
            protocol::DecisionPayload::TransitionRun {
                to_state: RunState::Completed,
                ..
            }
        )
    {
        return Ok(());
    }
    let Some(candidate) = proposal.candidate_id.as_deref() else {
        return Ok(());
    };
    let current: Option<(String, i64)> = tx.query_row(
        "SELECT s.spec_id,s.revision FROM candidates c JOIN runs r ON r.run_id=c.run_id JOIN specs s ON s.spec_id=r.spec_id WHERE c.run_id=?1 AND c.candidate_id=?2 AND c.state='STALE' AND c.spec_sha256<>s.sha256",
        params![proposal.run_id,candidate], |r| Ok((r.get(0)?,r.get(1)?)),
    ).optional().map_err(sqlite_error)?;
    let Some((spec, revision)) = current else {
        return Ok(());
    };
    let criteria = strings(
        tx,
        "SELECT a.ac_id FROM acceptance_criteria a JOIN requirements q ON q.spec_id=a.spec_id AND q.requirement_id=a.requirement_id WHERE a.spec_id=?1 AND q.required_release=1",
        &[&spec],
    )?;
    rejection.reason = RejectionReason::EvidenceGateUnmet;
    rejection.detail = format!(
        "candidate {candidate} is STALE: its old-revision evidence cannot satisfy specification revision {revision} acceptance criteria {criteria:?}"
    );
    Ok(())
}

/// The `supervisor.retrigger` of a stale answer to `turn`'s wake (decision 6): the wake's base fingerprint and the
/// tasks its turn was shown; `own_mistake` when the envelope differs from what the turn was given, `n` such mistakes of
/// the run in a row since its last APPLIED decision (the re-triggered wake's backoff).
fn retrigger(
    tx: &Transaction<'_>,
    project_id: &str,
    proposal: &Proposal,
    rejection: &Rejection,
    turn: &Answered,
) -> Result<NewEvent, PersistError> {
    let own_mistake = to_i64(proposal.expected_revision, "expected_revision")? != turn.run_revision
        || to_i64(proposal.role_epoch, "role_epoch")? != turn.role_epoch;
    let n: i64 = if own_mistake {
        tx.query_row(
            "SELECT count(*) + 1 FROM events WHERE run_id = ?1 AND kind = 'supervisor.retrigger' \
             AND json_extract(payload_json, '$.own_mistake') = 1 \
             AND seq > coalesce((SELECT max(seq) FROM events WHERE run_id = ?1 AND kind = 'decision.applied'), 0)",
            params![proposal.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?
    } else {
        0
    };
    let spec_revision: Option<i64> = match &turn.spec_id {
        Some(spec) => tx
            .query_row(
                "SELECT revision FROM specs WHERE spec_id = ?1",
                params![spec],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?,
        None => None,
    };
    Ok(NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: child_id(&proposal.decision_id, "retrigger"),
        run_id: Some(proposal.run_id.clone()),
        kind: "supervisor.retrigger".to_owned(),
        payload: json!({
            "decision_id": proposal.decision_id,
            "reason": reason_name(rejection.reason),
            "base_fingerprint": turn.fingerprint.trim_end_matches("#retrigger"),
            "spec_revision": spec_revision,
            "task_ids": turn.snapshots.keys().collect::<Vec<_>>(),
            "trigger": {"wake": turn.wake, "fingerprint": turn.fingerprint, "spec_id": turn.spec_id},
            "own_mistake": own_mistake,
            "n": n,
        }),
    })
}

fn resolve(
    tx: &Transaction<'_>,
    decision_id: &str,
    state: &str,
    reason: Option<RejectionReason>,
) -> Result<(), PersistError> {
    let changed = tx
        .execute(
            "UPDATE decisions SET state = ?1, reason_code = ?2 WHERE decision_id = ?3 AND state = 'PROPOSED'",
            params![state, reason.map(reason_name), decision_id],
        )
        .map_err(sqlite_error)?;
    if changed == 1 {
        Ok(())
    } else {
        Err(PersistError::IntegrityViolation(format!(
            "decision {decision_id} was no longer PROPOSED"
        )))
    }
}

/// The task/run changes, the `decision.applied` event and the dispatch message of an accepted proposal. The run's
/// compare-and-set comes first: every applied decision moves the revision the next proposal must name.
fn applied(
    project_id: &str,
    proposal: &Proposal,
    snapshot: &Snapshot,
    effect: &Effect,
    payload: &Value,
    lease: Option<&Leased>,
) -> Result<Commit, PersistError> {
    let run_revision = to_i64(snapshot.run.revision, "run revision")?;
    let run = |change| StateCas {
        id: proposal.run_id.clone(),
        expected_revision: run_revision,
        change,
    };
    let task = |id: &str, revision: u64, to| -> Result<StateCas, PersistError> {
        Ok(StateCas {
            id: id.to_owned(),
            expected_revision: to_i64(revision, "task revision")?,
            change: Change::Task(to),
        })
    };
    let mut details = json!({
        "decision_id": proposal.decision_id,
        "kind": proposal.kind(),
        "payload": payload,
    });
    let mut outbox = Vec::new();
    let cas = match effect {
        Effect::Dispatch {
            task_id,
            task_revision,
            role,
            role_epoch,
            ..
        } => {
            let leased = lease.ok_or_else(|| {
                PersistError::IntegrityViolation(format!("dispatch of {task_id} without a lease"))
            })?;
            let lease = json!({
                "kind": leased.grant.kind,
                "epoch": leased.grant.epoch,
                "expires_at": leased.grant.expires_at,
            });
            details["task"] = json!({"task_id": task_id, "from": "READY", "to": "LEASED"});
            details["attempt_id"] = json!(leased.attempt_id);
            details["lease"] = lease.clone();
            outbox.push(NewOutbox {
                message_id: child_id(&proposal.decision_id, "dispatch"),
                event: 0,
                run_id: Some(proposal.run_id.clone()),
                recipient_role: role.clone(),
                role_epoch: to_i64(*role_epoch, "role_epoch")?,
                payload: json!({
                    "notice": "runtime",
                    "kind": "dispatch",
                    "decision_id": proposal.decision_id,
                    "task_id": task_id,
                    "role": role,
                    "attempt_id": leased.attempt_id,
                    "lease": lease,
                }),
            });
            vec![
                run(Change::Touch(Entity::Run)),
                task(task_id, *task_revision, TaskState::Leased)?,
            ]
        }
        Effect::TransitionTask {
            task_id,
            task_revision,
            from,
            to,
        } => {
            details["task"] = json!({"task_id": task_id, "from": from.as_str(), "to": to.as_str()});
            vec![
                run(Change::Touch(Entity::Run)),
                task(task_id, *task_revision, *to)?,
            ]
        }
        Effect::TransitionRun { from, to } => {
            details["run"] = json!({"from": from.as_str(), "to": to.as_str()});
            if *to == RunState::Running {
                // The host loop's word that the run's ready tasks may be dispatched, durable with the move (ADR-019
                // decision 3): a runtime notice, never an HCP message (ADR-018 decision 4).
                outbox.push(NewOutbox {
                    message_id: child_id(&proposal.decision_id, "run.started"),
                    event: 0,
                    run_id: Some(proposal.run_id.clone()),
                    recipient_role: "runtime".to_owned(),
                    role_epoch: 1,
                    payload: json!({
                        "notice": "runtime",
                        "kind": "run.started",
                        "decision_id": proposal.decision_id,
                        "run_id": proposal.run_id,
                        "from": from.as_str(),
                    }),
                });
            }
            vec![run(Change::Run(*to))]
        }
        Effect::RecordAssumption | Effect::CreateTasks => vec![run(Change::Touch(Entity::Run))],
        Effect::Unsupported(kind) => return Err(PersistError::Unsupported(kind)),
    };
    Ok(Commit {
        cas,
        events: vec![NewEvent {
            intent: Intent::Progress,
            project_id: project_id.to_owned(),
            event_id: child_id(&proposal.decision_id, "applied"),
            run_id: Some(proposal.run_id.clone()),
            kind: "decision.applied".to_owned(),
            payload: details,
        }],
        outbox,
    })
}

/// The PREPARED attempt of a dispatch and the lease it holds.
pub(super) struct Leased {
    pub(super) attempt_id: String,
    pub(super) grant: leases::Grant,
}

/// What a dispatch leases: a new PREPARED attempt of `task_id` for `role` (at `role_epoch`) holding `kind`.
pub(super) struct LeaseRequest<'a> {
    pub(super) project_id: &'a str,
    pub(super) run_id: &'a str,
    pub(super) attempt_id: String,
    pub(super) task_id: &'a str,
    pub(super) role: &'a str,
    pub(super) role_epoch: u64,
    pub(super) kind: &'a str,
    pub(super) now: std::time::SystemTime,
    /// The task's admission, in this transaction (docs/adr/ADR-035.md decision 12): no attempt without one.
    pub(super) admission: super::admission::Admission,
}

/// Create the PREPARED attempt of a dispatch (its row only: the dispatch's event records it) and grant it the lease,
/// in the caller's transaction (AC-035-02). The attempt carries the role's epoch, the host epoch, and its binding's
/// agent, session and configuration revision (the run's revision when the role has no binding).
pub(super) fn lease_task(
    tx: &Transaction<'_>,
    request: &LeaseRequest<'_>,
) -> Result<Leased, PersistError> {
    if request.admission.task_id() != request.task_id {
        return Err(PersistError::IntegrityViolation(format!(
            "an attempt of {} under the admission of {}",
            request.task_id,
            request.admission.task_id()
        )));
    }
    let (host_epoch, config_revision): (i64, i64) = tx
        .query_row(
            "SELECT p.host_epoch, r.config_revision FROM runs r JOIN projects p ON p.project_id = r.project_id \
             WHERE r.run_id = ?1",
            params![request.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    // The binding's agent, session and configuration revision (docs/adr/ADR-022.md decisions 8 and 9).
    let binding: Option<(String, Option<String>, i64)> = tx
        .query_row(
            "SELECT agent, session_id, config_revision FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![request.run_id, request.role],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (agent, session_id, config_revision) = match binding {
        Some((agent, session, revision)) => (Some(agent), session, revision),
        None => (None, None, config_revision),
    };
    attempts::insert_prepared(
        tx,
        &NewAttempt {
            attempt_id: request.attempt_id.clone(),
            task_id: request.task_id.to_owned(),
            session_id,
            role_epoch: to_i64(request.role_epoch, "role_epoch")?,
            host_epoch,
            config_revision,
            agent,
        },
    )?;
    let grant = leases::grant(
        tx,
        request.project_id,
        request.kind,
        &request.attempt_id,
        request.now,
    )?;
    // Leased, it waits no more: if it comes back to READY (a retry), it ages from the start again and is stamped
    // by the round it is next eligible in (M2T03R-53). Bookkeeping, not a state change.
    tx.execute(
        "UPDATE tasks SET wait_rounds = 0, first_eligible_seq = NULL WHERE task_id = ?1",
        params![request.task_id],
    )
    .map_err(sqlite_error)?;
    Ok(Leased {
        attempt_id: request.attempt_id.clone(),
        grant,
    })
}

/// The one event of a rejected proposal, with the fences and inputs the verdict read.
fn rejected(
    project_id: &str,
    proposal: &Proposal,
    snapshot: &Snapshot,
    context: &Context,
    rejection: &Rejection,
) -> NewEvent {
    let mut payload = json!({
        "decision_id": proposal.decision_id,
        "kind": proposal.kind(),
        "reason_code": reason_name(rejection.reason),
        "detail": rejection.detail,
        "role_epoch": proposal.role_epoch,
        "current_role_epoch": snapshot.commander_epoch,
        "expected_revision": proposal.expected_revision,
        "current_revision": snapshot.run.revision,
        "spec_sha256": proposal.spec_sha256,
        "current_spec_sha256": snapshot.run.spec_sha256,
        "inputs": {
            "required_capabilities": snapshot.required_capabilities,
            "budget_headroom": context.budget_headroom,
        },
    });
    // What the rejection names, machine-readable, when it has more than its detail (ADR-035 decision 10).
    if let Some(details) = &rejection.details {
        payload["details"] = details.clone();
    }
    NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: child_id(&proposal.decision_id, "rejected"),
        run_id: Some(proposal.run_id.clone()),
        kind: "decision.rejected".to_owned(),
        payload,
    }
}

fn revision(value: i64) -> Result<u64, PersistError> {
    u64::try_from(value)
        .map_err(|_| PersistError::IntegrityViolation(format!("negative revision {value}")))
}

/// (project_id, state, revision, spec_id, spec sha256) of a run.
type RunRow = (String, String, i64, Option<String>, Option<String>);

fn load_run(tx: &Transaction<'_>, run_id: &str) -> Result<(String, RunView), PersistError> {
    let row: Option<RunRow> = tx
        .query_row(
            "SELECT r.project_id, r.state, r.revision, r.spec_id, s.sha256 \
             FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, rev, spec_id, spec_sha256)) = row else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    Ok((
        project_id,
        RunView {
            state: writer::parse::<RunState>(&state)?,
            revision: revision(rev)?,
            spec_id,
            spec_sha256,
        },
    ))
}

fn load_snapshot(
    tx: &Transaction<'_>,
    project_id: &str,
    proposal: &Proposal,
    context: &Context,
    run: RunView,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Snapshot, PersistError> {
    let run_id = proposal.run_id.as_str();
    let commander_epoch =
        load_binding(tx, run_id, COMMANDER, &ctx.resource_bundle.version())?.map(|b| b.role_epoch);
    let task = match proposal.task_id() {
        Some(id) => load_task(tx, run_id, id, run.spec_id.as_deref(), context.now())?,
        None => None,
    };
    let target = match proposal.target_role() {
        Some(role) => load_binding(tx, run_id, role, &ctx.resource_bundle.version())?,
        None => None,
    };
    let required_capabilities = task
        .as_ref()
        .and_then(|t| context.required_capabilities.get(&t.role))
        .cloned();
    let free_slot = match &task {
        Some(t) if proposal.kind() == "dispatch" => leases::free_slot(
            tx,
            project_id,
            t.writer,
            context.reader_slots(),
            context.now(),
        )?,
        _ => None,
    };
    let budget_stop = match &task {
        Some(task) if proposal.kind() == "dispatch" => {
            let budget = super::budget_view::load(tx, run_id, context.now(), context.wall())?;
            budget.task_stop(&task.task_id).map(|d| d.to_string())
        }
        _ => None,
    };
    let evidence = if proposal.needs_evidence() {
        load_evidence(tx, run_id, &run)?
    } else {
        Evidence::default()
    };
    let quiet = if proposal.kind() == "transition_run" {
        let count = |n: i64| u64::try_from(n).unwrap_or(0);
        let limit = ctx.unread_limit();
        crate::decision::Quiet {
            attempts_unfinished: count(super::retry::unfinished_attempts(tx, run_id)?.0),
            open_rounds: count(
                i64::try_from(super::closing::open_rounds(tx, run_id)?).unwrap_or(i64::MAX),
            ),
            unread_wakes: count(super::supervise::unread_wakes(tx, run_id, limit)?),
            unread_limit: count(limit),
        }
    } else {
        crate::decision::Quiet::default()
    };
    let assumptions = if proposal.kind() == "record_assumption" {
        load_assumptions(tx, run_id)?
    } else {
        None
    };
    // Only a move to READY reads readiness (REQ-031 behaviour 1, C5).
    let readiness = match &proposal.payload {
        crate::protocol::DecisionPayload::TransitionRun { to_state, .. }
            if *to_state == crate::protocol::RunState::Ready =>
        {
            super::readiness::run_readiness(tx, run_id)?
        }
        _ => None,
    };
    let baselined = proposal.kind() == "transition_run" && super::baseline::recorded(tx, run_id)?;
    Ok(Snapshot {
        run,
        commander_epoch,
        task,
        target,
        required_capabilities,
        budget_headroom: context.budget_headroom,
        budget_stop,
        free_slot,
        evidence,
        quiet,
        assumptions,
        readiness,
        admission: None,
        baselined,
    })
}

/// The decision classes an unattended run's configuration snapshot lists under `[assumptions]` (REQ-030 behaviour 4);
/// `None` for an interactive run, or a snapshot without `unattended` (docs/adr/ADR-035.md decision 11).
fn load_assumptions(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<BTreeSet<String>>, PersistError> {
    let snapshot: Option<String> = tx
        .query_row(
            "SELECT config_snapshot_json FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let snapshot: Value = match snapshot {
        Some(text) => serde_json::from_str(&text).map_err(|e| {
            PersistError::IntegrityViolation(format!("run {run_id}'s configuration snapshot: {e}"))
        })?,
        None => return Ok(None),
    };
    if snapshot["unattended"] != Value::Bool(true) {
        return Ok(None);
    }
    Ok(Some(
        snapshot["assumptions"]
            .as_object()
            .map(|listed| listed.keys().cloned().collect())
            .unwrap_or_default(),
    ))
}

fn strings(
    tx: &Transaction<'_>,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = tx.prepare(sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map(args, |r| r.get::<_, String>(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

fn load_task(
    tx: &Transaction<'_>,
    run_id: &str,
    task_id: &str,
    run_spec: Option<&str>,
    now: std::time::SystemTime,
) -> Result<Option<TaskView>, PersistError> {
    #[allow(clippy::type_complexity)]
    let row: Option<(String, i64, Option<String>, String, i64, bool, Option<String>, String)> = tx
        .query_row(
            "SELECT state, revision, spec_id, role, cost_estimate, writer, next_retry_at, kind FROM tasks \
             WHERE task_id = ?1 AND run_id = ?2",
            params![task_id, run_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, rev, spec_id, role, cost, writer, next_retry_at, kind)) = row else {
        return Ok(None);
    };
    let kind = crate::decision::TaskKind::from_name(&kind)
        .ok_or_else(|| PersistError::IntegrityViolation(format!("unknown task kind {kind}")))?;
    let mut statement = tx
        .prepare(
            "SELECT d.depends_on, t.state FROM task_dependencies d JOIN tasks t ON t.task_id = d.depends_on \
             WHERE d.task_id = ?1 ORDER BY d.depends_on",
        )
        .map_err(sqlite_error)?;
    let dependencies = statement
        .query_map(params![task_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?
        .map(|row| {
            let (id, state) = row.map_err(sqlite_error)?;
            Ok((id, writer::parse::<TaskState>(&state)?))
        })
        .collect::<Result<Vec<_>, PersistError>>()?;
    let requirements = match run_spec {
        Some(spec) => strings(
            tx,
            "SELECT requirement_id FROM task_requirements WHERE task_id = ?1 AND spec_id = ?2",
            &[&task_id, &spec],
        )?,
        None => BTreeSet::new(),
    };
    Ok(Some(TaskView {
        task_id: task_id.to_owned(),
        state: writer::parse::<TaskState>(&state)?,
        revision: revision(rev)?,
        spec_id,
        kind,
        role,
        cost_estimate: u32::try_from(cost)
            .map_err(|_| PersistError::IntegrityViolation(format!("cost_estimate {cost}")))?,
        writer,
        dependencies,
        requirements,
        // Whole seconds on both sides (the store's form): ahead of `now` until its second has come.
        retry_at: next_retry_at
            .filter(|next| next.as_str() > crate::time::rfc3339_utc(now).as_str()),
        runtime_owned: kind == crate::decision::TaskKind::SpecDraft
            || super::verification::verifier_tasks(tx, run_id)?.contains(task_id),
        held_by_open_round: super::verification::held_by_open_rounds(tx, run_id)?.contains(task_id),
    }))
}

pub(super) fn load_binding(
    tx: &Transaction<'_>,
    run_id: &str,
    role: &str,
    resource_version: &str,
) -> Result<Option<BindingView>, PersistError> {
    let row: Option<(i64, String, Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT b.role_epoch, b.state, s.capabilities_json, b.session_id FROM role_bindings b \
             LEFT JOIN sessions s ON s.session_id = b.session_id WHERE b.run_id = ?1 AND b.role = ?2",
            params![run_id, role],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((epoch, state, capabilities, session)) = row else {
        return Ok(None);
    };
    // Probed capabilities are an object of booleans (REQ-024); anything else counts as none.
    let capabilities = capabilities
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
        .map(|v| crate::decision::granted_capabilities(&v))
        .unwrap_or_default();
    // What keeps an ACTIVE binding from being dispatched now, each named (CR-025, ROLE_BINDING_PENDING): a role whose
    // configuration or resource change waits for its checkpoint, or that has no session yet, is not dispatched. Its
    // tasks are not judged by capabilities it does not have yet (docs/adr/ADR-022.md decision 8, M2T07R-50); nor while staffing holds a rebind
    // of it pending for the attempt in flight (docs/adr/ADR-030.md decision 5: the new session is granted once it
    // ended); nor is a new holder whose first input, its handover package, is not delivered yet (docs/adr/ADR-024.md
    // decision 4, M2T17R-03); nor, while the session owes its first input, a second attempt of it (M2T17R-201). A BLOCKED
    // binding is not dispatched whatever else holds.
    use crate::decision::Pending;
    let mut pending = Vec::new();
    if state == "ACTIVE" {
        if super::team::pending(tx, run_id, role, resource_version)? {
            pending.push(Pending::TeamChange);
        }
        if super::staffing::pending_change(tx, run_id, role, epoch)?
            .is_some_and(|(change, _)| change.cause == crate::staffing::Cause::Rebind)
        {
            pending.push(Pending::Rebind);
        }
        if session.is_none() {
            pending.push(Pending::NoSession);
        }
        if super::handover::first_input_pending(tx, run_id, role, epoch)? {
            pending.push(Pending::HandoverUndelivered);
        }
        if let Some(session) = session.as_deref()
            && super::handover::first_input_in_flight(tx, run_id, role, epoch, session)?
        {
            pending.push(Pending::FirstInputInFlight);
        }
    }
    Ok(Some(BindingView {
        role_epoch: revision(epoch)?,
        active: state == "ACTIVE" && pending.is_empty(),
        blocked: state != "ACTIVE",
        pending,
        capabilities,
    }))
}

/// ADR-011 §3 gate: the run's candidates in use, and for each candidate of the run that is ACCEPTED (only the join at
/// its verification barrier accepts one, docs/adr/ADR-029.md decision 11: a VERIFYING candidate counts for nothing, so
/// no proposal accepts before the join or on a FAIL/UNKNOWN) the requirements it passed ([`requirements_passed`]).
fn load_evidence(
    tx: &Transaction<'_>,
    run_id: &str,
    run: &RunView,
) -> Result<Evidence, PersistError> {
    let (Some(spec_id), Some(_)) = (&run.spec_id, &run.spec_sha256) else {
        return Ok(Evidence::default());
    };
    let required = strings(
        tx,
        "SELECT requirement_id FROM requirements WHERE spec_id = ?1 AND required_release = 1",
        &[spec_id],
    )?;
    let live = strings(
        tx,
        "SELECT candidate_id FROM candidates WHERE run_id = ?1 AND state IN ('FROZEN', 'VERIFYING', 'ACCEPTED')",
        &[&run_id],
    )?;
    let mut evidence = Evidence {
        required,
        live,
        ..Evidence::default()
    };
    let accepted = strings(
        tx,
        "SELECT candidate_id FROM candidates WHERE run_id = ?1 AND state = 'ACCEPTED'",
        &[&run_id],
    )?;
    for candidate in accepted {
        let passed = requirements_passed(tx, run_id, spec_id, &candidate)?;
        if !passed.is_empty() {
            evidence.candidates.insert(candidate, passed);
        }
    }
    Ok(evidence)
}

/// The requirements of specification `spec_id` that candidate `candidate` of run `run_id` passed (docs/adr/ADR-029.md
/// decision 12, the gate's predicate; ADR-011 §3): none unless the candidate was frozen on that specification (its
/// spec_sha256 is the specification's, -158); otherwise each requirement with at least one acceptance criterion, every
/// one of which the display projection of its rows on the candidate satisfies (decision 2, no profile: PASS, or
/// compliant DEGRADED where the requirement may degrade; a DEGRADED of one that may not is a FAIL; an INVALID, vetoed or
/// STALE candidate satisfies none), each of those rows backed by evidence of the candidate from the criterion's own
/// check whose outcome is the row's status (M2T02R-03). `load_evidence` asks it for ACCEPTED candidates, the join for
/// the VERIFYING one, so the Runtime's acceptance and the Commander's gate are one function.
pub(super) fn requirements_passed(
    conn: &Connection,
    run_id: &str,
    spec_id: &str,
    candidate: &str,
) -> Result<BTreeSet<String>, PersistError> {
    let mut passed = std::collections::BTreeMap::new();
    for ((requirement, _), satisfied) in criteria_satisfied(conn, run_id, spec_id, candidate)? {
        *passed.entry(requirement).or_insert(true) &= satisfied;
    }
    Ok(passed
        .into_iter()
        .filter_map(|(req, ok)| ok.then_some(req))
        .collect())
}

/// The same evidence-bound judgement per AC, shared with net progress (ADR-037).
pub(super) fn criteria_satisfied(
    conn: &Connection,
    run_id: &str,
    spec_id: &str,
    candidate: &str,
) -> Result<std::collections::BTreeMap<(String, String), bool>, PersistError> {
    use crate::verification::acceptance::{self, Observation, RowFact, Standing};
    let found: Option<(String, bool, bool)> = conn
        .query_row(
            "SELECT c.state, c.spec_sha256 = s.sha256, EXISTS(SELECT 1 FROM evidence e \
               WHERE e.candidate_id = c.candidate_id AND e.outcome = 'INVALID') \
             FROM candidates c JOIN specs s ON s.spec_id = ?2 WHERE c.candidate_id = ?3 AND c.run_id = ?1",
            params![run_id, spec_id, candidate],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, frozen_on_it, vetoed)) = found else {
        return Ok(std::collections::BTreeMap::new());
    };
    if !frozen_on_it {
        return Ok(std::collections::BTreeMap::new());
    }
    let standing = match state.as_str() {
        _ if vetoed => Standing::Invalid,
        "INVALID" => Standing::Invalid,
        "STALE" => Standing::Stale,
        _ => Standing::Usable,
    };
    let mut statement = conn
        .prepare(
            "SELECT q.requirement_id, q.release_class = 'degradable', a.ac_id, a.check_id FROM requirements q \
             JOIN acceptance_criteria a ON a.requirement_id = q.requirement_id AND a.spec_id = q.spec_id \
             WHERE q.spec_id = ?1 ORDER BY q.requirement_id, a.ac_id",
        )
        .map_err(sqlite_error)?;
    let criteria: Vec<(String, bool, String, String)> = statement
        .query_map(params![spec_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut rows = conn
        .prepare(
            "SELECT x.status, e.candidate_id = x.candidate_id AND e.check_id = ?4 AND e.outcome = x.status \
             FROM acceptance_results x LEFT JOIN evidence e ON e.evidence_id = x.evidence_id \
             WHERE x.ac_id = ?1 AND x.spec_id = ?2 AND x.candidate_id = ?3",
        )
        .map_err(sqlite_error)?;
    let mut passed: std::collections::BTreeMap<(String, String), bool> =
        std::collections::BTreeMap::new();
    for (requirement, degradable, ac, check) in criteria {
        let found: Vec<(String, Option<bool>)> = rows
            .query_map(params![ac, spec_id, candidate, check], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let observation = Observation {
            rows: found
                .iter()
                .map(|(status, _)| RowFact::of(super::closing::row_of(status)))
                .collect(),
            degradable,
            unobserved_claim: false,
        };
        let satisfied = found.iter().all(|(_, backed)| *backed == Some(true))
            && acceptance::display(standing, &observation, None)
                .label
                .satisfied();
        passed.insert((requirement, ac), satisfied);
    }
    Ok(passed)
}

#[cfg(test)]
mod tests;

/// What the Commander's fence answers for one of its messages (docs/adr/ADR-024.md decision 6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fenced {
    /// Its epoch is the commander's binding now: it passes, nothing written (where it goes is its router's).
    Current,
    /// Its epoch is not the binding's (an old Commander's, after a handover): only history, a decisions row REJECTED
    /// with this reason and its decision.rejected event, nothing else changed.
    Rejected {
        reason: RejectionReason,
        detail: String,
        event_seq: i64,
    },
}

/// Whether a Commander's `message` is in history already under its id (a decisions row one of its fences wrote): the
/// same message is (a replay); another message under its id is a reused key, whichever fence sees it (M2T17R-101).
pub(super) fn recorded_commander_message(
    tx: &Connection,
    message: &Message,
) -> Result<bool, PersistError> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM decisions WHERE decision_id = ?1",
            params![message.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(payload) = payload else {
        return Ok(false);
    };
    let recorded = serde_json::to_value(message)
        .map_err(|e| PersistError::InvalidProposal(format!("{}: {e}", message.message_id)))?;
    if serde_json::from_str::<Value>(&payload).ok().as_ref() == Some(&recorded) {
        Ok(true)
    } else {
        Err(PersistError::KeyReuse(format!(
            "{}: another message was recorded under this id",
            message.message_id
        )))
    }
}

/// The sender's fence of a Commander's steer, query or other message (REQ-019 behaviour 3; ADR-024 decision 6): the
/// one its every output passes when it enters, as a decision does in [`decide`]. A message whose role_epoch (the
/// sender's: every message the Commander sends carries its own epoch) is not the commander binding's now is recorded
/// as a decision REJECTED STALE_ROLE_EPOCH, its payload the message, with one decision.rejected event, in one
/// transaction; nothing else is written. A decision is refused here: it goes through [`decide`].
pub(super) fn commander_message(
    conn: &mut Connection,
    message: &Message,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Fenced, PersistError> {
    if message.sender_role != COMMANDER || message.kind == protocol::MessageKind::Decision {
        return Err(PersistError::InvalidProposal(format!(
            "{}: the Commander's fence takes the Commander's messages other than decisions",
            message.message_id
        )));
    }
    let role_epoch = to_i64(message.role_epoch, "role_epoch")?;
    let expected_revision = to_i64(message.expected_revision, "expected_revision")?;
    let tx = super::writer::begin(conn, ctx)?;
    let (project_id, _) = load_run(&tx, &message.run_id)?;
    let current: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![message.run_id, COMMANDER],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    // Another message under the id of one recorded is a reused key, at the current epoch too (M2T17R-05, -101).
    let existing = recorded_commander_message(&tx, message)?;
    if current == Some(role_epoch) {
        return Ok(Fenced::Current);
    }
    let reason = RejectionReason::StaleRoleEpoch;
    let detail = format!(
        "role epoch {} is not the commander's current {current:?}",
        message.role_epoch
    );
    let recorded = serde_json::to_value(message)
        .map_err(|e| PersistError::InvalidProposal(format!("{}: {e}", message.message_id)))?;
    // Given again (a lost answer, a retry): the rejection recorded is the answer, and nothing more is written. Its row
    // may come from consume's fence, which records no decision.rejected event: that event is written once, here.
    if existing {
        let event: Option<(i64, String)> = tx
            .query_row(
                "SELECT seq, payload_json FROM events WHERE event_id = ?1",
                params![child_id(&message.message_id, "rejected")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some((event_seq, payload)) = event {
            let detail = serde_json::from_str::<Value>(&payload)
                .ok()
                .and_then(|v| v.get("detail").and_then(Value::as_str).map(str::to_owned))
                .unwrap_or_default();
            return Ok(Fenced::Rejected {
                reason,
                detail,
                event_seq,
            });
        }
    } else {
        tx.execute(
            "INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, reason_code, payload_json, \
             state) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'REJECTED')",
            params![
                message.message_id,
                message.run_id,
                role_epoch,
                expected_revision,
                reason_name(reason),
                recorded.to_string()
            ],
        )
        .map_err(sqlite_error)?;
    }
    let done = writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id,
                event_id: child_id(&message.message_id, "rejected"),
                run_id: Some(message.run_id.clone()),
                kind: "decision.rejected".to_owned(),
                payload: json!({
                    "decision_id": message.message_id,
                    "kind": message.kind,
                    "reason_code": reason_name(reason),
                    "detail": detail,
                    "role_epoch": message.role_epoch,
                    "current_role_epoch": current,
                }),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    super::writer::commit(tx, ctx)?;
    Ok(Fenced::Rejected {
        reason,
        detail,
        event_seq: done.event_seqs[0],
    })
}
