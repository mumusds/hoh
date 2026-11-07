//! Commander decisions in the store (REQ-035, docs/adr/ADR-011.md §3). Recording the proposal, reading the snapshot
//! its verdict depends on, and writing the outcome are one IMMEDIATE transaction: an applied decision moves its
//! task/run by compare-and-set and queues its dispatch together with the APPLIED row; a rejected one changes no task
//! or run and writes exactly one `decision.rejected` event.

use super::attempts::{self, NewAttempt};
use super::leases;
use super::writer::{self, Change, Commit, Entity, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::decision::{
    self, BindingView, COMMANDER, Context, Effect, Evidence, Proposal, Rejection, RunView,
    Snapshot, TaskView,
};
use crate::lifecycle::Lifecycle;
use crate::protocol::{self, RejectionReason, RunState, TaskState};
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
    // The payload is checked against $defs.DecisionPayload again on the way in, whoever built the proposal.
    let payload = protocol::encode(&proposal.payload)
        .map_err(|e| PersistError::InvalidProposal(format!("{}: {e}", proposal.decision_id)))?;
    let role_epoch = to_i64(proposal.role_epoch, "role_epoch")?;
    let expected_revision = to_i64(proposal.expected_revision, "expected_revision")?;
    let tx = super::writer::begin(conn, ctx)?;
    let (project_id, run) = load_run(&tx, &proposal.run_id)?;
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
    let snapshot = load_snapshot(&tx, &project_id, proposal, context, run)?;
    let decided = match decision::validate(proposal, &snapshot) {
        Ok(Effect::Unsupported(kind)) => return Err(PersistError::Unsupported(kind)),
        Ok(effect) => {
            let lease = match &effect {
                Effect::Dispatch {
                    task_id,
                    role,
                    role_epoch,
                    lease_kind,
                    ..
                } => Some(lease_task(
                    &tx,
                    &LeaseRequest {
                        project_id: &project_id,
                        run_id: &proposal.run_id,
                        attempt_id: child_id(&proposal.decision_id, "attempt"),
                        task_id,
                        role,
                        role_epoch: *role_epoch,
                        kind: lease_kind,
                        now: context.now(),
                    },
                )?),
                _ => None,
            };
            let commit = applied(
                &project_id,
                proposal,
                &snapshot,
                &effect,
                &payload,
                lease.as_ref(),
            )?;
            let done = writer::write(&tx, &commit, ctx)?;
            resolve(&tx, &proposal.decision_id, "APPLIED", None)?;
            Decided::Applied {
                run_revision: done.revisions[0],
                event_seqs: done.event_seqs,
            }
        }
        Err(rejection) => {
            let event = rejected(&project_id, proposal, &snapshot, context, &rejection);
            let commit = Commit {
                events: vec![event],
                ..Commit::default()
            };
            let done = writer::write(&tx, &commit, ctx)?;
            resolve(
                &tx,
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
    tx.commit().map_err(sqlite_error)?;
    Ok(decided)
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
        Effect::RecordAssumption => vec![run(Change::Touch(Entity::Run))],
        Effect::Unsupported(kind) => return Err(PersistError::Unsupported(kind)),
    };
    Ok(Commit {
        cas,
        events: vec![NewEvent {
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
}

/// Create the PREPARED attempt of a dispatch (its row only: the dispatch's event records it) and grant it the lease,
/// in the caller's transaction (AC-035-02). The attempt carries the role's epoch, the host epoch and the run's config
/// revision.
pub(super) fn lease_task(
    tx: &Transaction<'_>,
    request: &LeaseRequest<'_>,
) -> Result<Leased, PersistError> {
    let (host_epoch, config_revision): (i64, i64) = tx
        .query_row(
            "SELECT p.host_epoch, r.config_revision FROM runs r JOIN projects p ON p.project_id = r.project_id \
             WHERE r.run_id = ?1",
            params![request.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let agent: Option<String> = tx
        .query_row(
            "SELECT agent FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![request.run_id, request.role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    attempts::insert_prepared(
        tx,
        &NewAttempt {
            attempt_id: request.attempt_id.clone(),
            task_id: request.task_id.to_owned(),
            session_id: None,
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
    NewEvent {
        project_id: project_id.to_owned(),
        event_id: child_id(&proposal.decision_id, "rejected"),
        run_id: Some(proposal.run_id.clone()),
        kind: "decision.rejected".to_owned(),
        payload: json!({
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
        }),
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
) -> Result<Snapshot, PersistError> {
    let run_id = proposal.run_id.as_str();
    let commander_epoch = load_binding(tx, run_id, COMMANDER)?.map(|b| b.role_epoch);
    let task = match proposal.task_id() {
        Some(id) => load_task(tx, run_id, id, run.spec_id.as_deref(), context.now())?,
        None => None,
    };
    let target = match proposal.target_role() {
        Some(role) => load_binding(tx, run_id, role)?,
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
        Some(_) if proposal.kind() == "dispatch" => {
            super::retry::budget_stop(tx, run_id, context)?.map(str::to_owned)
        }
        _ => None,
    };
    let evidence = if proposal.needs_evidence() {
        load_evidence(tx, run_id, &run)?
    } else {
        Evidence::default()
    };
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
    })
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
    let row: Option<(String, i64, String, String, i64, bool, Option<String>)> = tx
        .query_row(
            "SELECT state, revision, spec_id, role, cost_estimate, writer, next_retry_at FROM tasks \
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
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, rev, spec_id, role, cost, writer, next_retry_at)) = row else {
        return Ok(None);
    };
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
        role,
        cost_estimate: u32::try_from(cost)
            .map_err(|_| PersistError::IntegrityViolation(format!("cost_estimate {cost}")))?,
        writer,
        dependencies,
        requirements,
        // Whole seconds on both sides (the store's form): ahead of `now` until its second has come.
        retry_at: next_retry_at
            .filter(|next| next.as_str() > crate::time::rfc3339_utc(now).as_str()),
    }))
}

pub(super) fn load_binding(
    tx: &Transaction<'_>,
    run_id: &str,
    role: &str,
) -> Result<Option<BindingView>, PersistError> {
    let row: Option<(i64, String, Option<String>)> = tx
        .query_row(
            "SELECT b.role_epoch, b.state, s.capabilities_json FROM role_bindings b \
             LEFT JOIN sessions s ON s.session_id = b.session_id WHERE b.run_id = ?1 AND b.role = ?2",
            params![run_id, role],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((epoch, state, capabilities)) = row else {
        return Ok(None);
    };
    // Probed capabilities are an object of booleans (REQ-024); anything else counts as none.
    let capabilities = capabilities
        .and_then(|json| serde_json::from_str::<Value>(&json).ok())
        .map(|v| crate::decision::granted_capabilities(&v))
        .unwrap_or_default();
    Ok(Some(BindingView {
        role_epoch: revision(epoch)?,
        active: state == "ACTIVE",
        capabilities,
    }))
}

/// ADR-011 §3 gate: the run's candidates in use, and for each candidate of the run that is VERIFYING or ACCEPTED and
/// was frozen on the run's current specification, the requirements R that have acceptance criteria there each of
/// which has at least one acceptance result for C and only PASS results, each backed by PASS evidence of C from the
/// criterion's own check (M2T02R-03).
fn load_evidence(
    tx: &Transaction<'_>,
    run_id: &str,
    run: &RunView,
) -> Result<Evidence, PersistError> {
    let (Some(spec_id), Some(spec_sha256)) = (&run.spec_id, &run.spec_sha256) else {
        return Ok(Evidence::default());
    };
    let required = strings(
        tx,
        "SELECT requirement_id FROM requirements WHERE spec_id = ?1 AND required_release = 1",
        &[spec_id],
    )?;
    let mut statement = tx
        .prepare(
            "SELECT c.candidate_id, q.requirement_id FROM candidates c JOIN requirements q ON q.spec_id = ?2 \
             WHERE c.run_id = ?1 AND c.state IN ('VERIFYING', 'ACCEPTED') AND c.spec_sha256 = ?3 \
             AND EXISTS (SELECT 1 FROM acceptance_criteria a WHERE a.requirement_id = q.requirement_id AND a.spec_id = q.spec_id) \
             AND NOT EXISTS ( \
               SELECT 1 FROM acceptance_criteria a WHERE a.requirement_id = q.requirement_id AND a.spec_id = q.spec_id \
               AND (NOT EXISTS (SELECT 1 FROM acceptance_results x \
                                WHERE x.ac_id = a.ac_id AND x.spec_id = a.spec_id AND x.candidate_id = c.candidate_id) \
                    OR EXISTS (SELECT 1 FROM acceptance_results x \
                               WHERE x.ac_id = a.ac_id AND x.spec_id = a.spec_id AND x.candidate_id = c.candidate_id \
                               AND (x.status <> 'PASS' OR NOT EXISTS (SELECT 1 FROM evidence e \
                                    WHERE e.evidence_id = x.evidence_id AND e.candidate_id = c.candidate_id \
                                    AND e.outcome = 'PASS' AND e.check_id = a.check_id))))) \
             ORDER BY c.candidate_id, q.requirement_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id, spec_id, spec_sha256], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?;
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
    for row in rows {
        let (candidate, requirement) = row.map_err(sqlite_error)?;
        evidence
            .candidates
            .entry(candidate)
            .or_default()
            .insert(requirement);
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests;
