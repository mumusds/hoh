//! [`super::Store::schedule_round`]: one scheduling round of a run in one IMMEDIATE transaction (REQ-035, REQ-037,
//! REQ-038). PENDING tasks whose dependencies are all ACCEPTED count as READY; the candidates are ranked with the run
//! snapshot's parameters ([`crate::schedule`]); the first whose slot is free is leased to a new PREPARED attempt.
//! A round is one selection: when nothing is selected, nothing is written. Otherwise the promotions, the lease, the
//! task's READY -> LEASED move, one `schedule.round` event (the parameters, the score, the waiting tasks) and the
//! dispatch message commit together, and every eligible task not selected counts one more waited round.

use super::decide::{self, LeaseRequest, Leased, child_id};
use super::leases;
use super::writer::{self, Change, Commit, Entity, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::decision::Context;
use crate::protocol::{RunState, SchedulerSettings, TaskState};
use crate::schedule::{self, Candidate, Eligibility, Params, Slots};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde_json::{Value, json};

/// A task dispatched by a round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dispatched {
    pub task_id: String,
    pub attempt_id: String,
    pub lease: leases::Grant,
    /// `critical`, `aged`, `unblock` or `normal`.
    pub tier: &'static str,
    pub score: schedule::Score,
}

/// What a round did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoundOutcome {
    pub dispatched: Option<Dispatched>,
    /// The eligible tasks not selected, by task id.
    pub waited: Vec<String>,
    /// PENDING tasks moved to READY.
    pub promoted: Vec<String>,
    pub event_seq: Option<i64>,
}

/// The states in which a run dispatches (CR-003: BLOCKED stops new dispatches).
const WORKING: [RunState; 2] = [RunState::Running, RunState::Verifying];

struct Row {
    candidate: Candidate,
    state: TaskState,
    revision: i64,
    role: String,
}

pub(super) fn round(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    after_events: &dyn Fn(),
) -> Result<RoundOutcome, PersistError> {
    let now = context.now();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let run: Option<(String, String, i64, Option<String>, String)> = tx
        .query_row(
            "SELECT project_id, state, revision, spec_id, config_snapshot_json FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, run_revision, run_spec, snapshot)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    if !WORKING.contains(&writer::parse::<RunState>(&state)?) {
        return Ok(RoundOutcome::default());
    }
    let params = parameters(&snapshot)?;
    let mut rows = candidates(&tx, run_id, run_spec.as_deref(), context)?;
    let slots = Slots {
        writer: leases::free_slot(&tx, &project_id, true, context.reader_slots(), now)?.is_some(),
        readers: u32::from(
            leases::free_slot(&tx, &project_id, false, context.reader_slots(), now)?.is_some(),
        ),
    };
    let board: Vec<Candidate> = rows.iter().map(|r| r.candidate.clone()).collect();
    let outcome = schedule::round(&params, &board, slots);
    let Some(selected) = outcome.selected else {
        return Ok(RoundOutcome::default());
    };
    let waited: Vec<String> = outcome
        .waited
        .iter()
        .map(|&i| rows[i].candidate.task_id.clone())
        .collect();
    let chosen = rows
        .iter()
        .position(|r| r.candidate.task_id == selected.task_id)
        .ok_or_else(|| PersistError::Storage(format!("{} left the round", selected.task_id)))?;
    let mut cas = vec![StateCas {
        id: run_id.to_owned(),
        expected_revision: run_revision,
        change: Change::Touch(Entity::Run),
    }];
    let mut promoted = Vec::new();
    for row in &mut rows {
        if row.state == TaskState::Pending {
            cas.push(StateCas {
                id: row.candidate.task_id.clone(),
                expected_revision: row.revision,
                change: Change::Task(TaskState::Ready),
            });
            row.revision += 1;
            promoted.push(row.candidate.task_id.clone());
        }
    }
    let task = &rows[chosen];
    let binding = decide::load_binding(&tx, run_id, &task.role)?
        .ok_or_else(|| PersistError::Storage(format!("role {} lost its binding", task.role)))?;
    let attempts: i64 = tx
        .query_row(
            "SELECT count(*) FROM attempts WHERE task_id = ?1",
            params![selected.task_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let kind = leases::free_slot(
        &tx,
        &project_id,
        task.candidate.writer,
        context.reader_slots(),
        now,
    )?
    .ok_or_else(|| PersistError::Storage("the slot the round chose is held".to_owned()))?;
    let Leased { attempt_id, grant } = decide::lease_task(
        &tx,
        &LeaseRequest {
            project_id: &project_id,
            run_id,
            attempt_id: child_id(&selected.task_id, &format!("attempt-{}", attempts + 1)),
            task_id: &selected.task_id,
            role: &task.role,
            role_epoch: binding.role_epoch,
            kind: &kind,
            now,
        },
    )?;
    cas.push(StateCas {
        id: selected.task_id.clone(),
        expected_revision: task.revision,
        change: Change::Task(TaskState::Leased),
    });
    let lease = json!({"kind": grant.kind, "epoch": grant.epoch, "expires_at": grant.expires_at});
    let w = params.weights;
    let commit = Commit {
        cas,
        events: vec![NewEvent {
            project_id,
            event_id: child_id(&attempt_id, "round"),
            run_id: Some(run_id.to_owned()),
            kind: "schedule.round".to_owned(),
            payload: json!({
                "task_id": selected.task_id,
                "attempt_id": attempt_id,
                "lease": lease,
                "sublayer": selected.tier.label(),
                "age_bonus": selected.age_bonus,
                "score": {
                    "numerator": selected.score.numerator,
                    "denominator": selected.score.denominator,
                    "value": selected.score.value(),
                },
                "weights": {
                    "user_value": w.user_value,
                    "unblock_value": w.unblock_value,
                    "risk_reduction": w.risk_reduction,
                    "information_gain": w.information_gain,
                },
                "max_wait_rounds": params.max_wait_rounds,
                "waited": waited,
                "promoted": promoted,
            }),
        }],
        outbox: vec![NewOutbox {
            message_id: child_id(&attempt_id, "dispatch"),
            event: 0,
            run_id: Some(run_id.to_owned()),
            recipient_role: task.role.clone(),
            role_epoch: decide::to_i64(binding.role_epoch, "role_epoch")?,
            payload: json!({
                "notice": "runtime",
                "kind": "dispatch",
                "task_id": selected.task_id,
                "role": task.role,
                "attempt_id": attempt_id,
                "lease": lease,
            }),
        }],
    };
    let done = writer::write(&tx, &commit, after_events)?;
    let seq = done.event_seqs[0];
    // The waiting tasks age by one round and keep the round they first became eligible in (the selected one was reset
    // when it was leased). Bookkeeping, not a state change: no revision moves.
    for id in &waited {
        tx.execute(
            "UPDATE tasks SET wait_rounds = wait_rounds + 1, first_eligible_seq = COALESCE(first_eligible_seq, ?1) \
             WHERE task_id = ?2",
            params![seq, id],
        )
        .map_err(sqlite_error)?;
    }
    tx.commit().map_err(sqlite_error)?;
    Ok(RoundOutcome {
        dispatched: Some(Dispatched {
            task_id: selected.task_id,
            attempt_id,
            lease: grant,
            tier: selected.tier.label(),
            score: selected.score,
        }),
        waited,
        promoted,
        event_seq: Some(seq),
    })
}

/// The snapshot's scheduler parameters (CR-007); what it leaves out is the default.
fn parameters(snapshot: &str) -> Result<Params, PersistError> {
    let snapshot: Value = serde_json::from_str(snapshot)
        .map_err(|e| PersistError::IntegrityViolation(format!("config snapshot: {e}")))?;
    // An empty object leaves everything to the defaults, as a missing key does (CR-007 S2, M2T03R-58).
    let mut scheduler = snapshot.get("scheduler").cloned().unwrap_or(Value::Null);
    if let Some(members) = scheduler.as_object_mut() {
        members.retain(|_, v| !matches!(v, Value::Object(o) if o.is_empty()) && !v.is_null());
    }
    let settings = match &scheduler {
        Value::Null => None,
        Value::Object(members) if members.is_empty() => None,
        value => Some(
            crate::protocol::decode::<SchedulerSettings>(value).map_err(|e| {
                PersistError::IntegrityViolation(format!("config snapshot scheduler: {e}"))
            })?,
        ),
    };
    Ok(Params::from_settings(settings.as_ref()))
}

/// The run's READY tasks and the PENDING ones whose dependencies are all ACCEPTED, with the five facts (REQ-038).
fn candidates(
    tx: &rusqlite::Transaction<'_>,
    run_id: &str,
    run_spec: Option<&str>,
    context: &Context,
) -> Result<Vec<Row>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT t.task_id, t.state, t.revision, t.spec_id, t.role, t.writer, t.created_seq, t.layer, t.user_value, \
             t.unblock_value, t.risk_reduction, t.information_gain, t.cost_estimate, t.wait_rounds, \
             t.first_eligible_seq, \
             (SELECT count(*) FROM task_dependencies d JOIN tasks u ON u.task_id = d.depends_on \
              WHERE d.task_id = t.task_id AND u.state <> 'ACCEPTED') \
             FROM tasks t WHERE t.run_id = ?1 AND t.state IN ('READY', 'PENDING') ORDER BY t.created_seq, t.task_id",
        )
        .map_err(sqlite_error)?;
    let raw = statement
        .query_map(params![run_id], |r| {
            Ok((
                (
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, i64>(6)?,
                    r.get::<_, u8>(7)?,
                ),
                (
                    r.get::<_, u8>(8)?,
                    r.get::<_, u8>(9)?,
                    r.get::<_, u8>(10)?,
                    r.get::<_, u8>(11)?,
                    r.get::<_, u8>(12)?,
                    r.get::<_, u32>(13)?,
                    r.get::<_, Option<i64>>(14)?,
                    r.get::<_, i64>(15)?,
                ),
            ))
        })
        .map_err(sqlite_error)?;
    let mut rows = Vec::new();
    let mut bindings = std::collections::BTreeMap::new();
    for row in raw {
        let (
            (task_id, state, revision, spec_id, role, writer, created_seq, layer),
            (user, unblock, risk, info, cost, wait_rounds, first_eligible_seq, open),
        ) = row.map_err(sqlite_error)?;
        let state = writer::parse::<TaskState>(&state)?;
        if state == TaskState::Pending && open > 0 {
            continue;
        }
        if !bindings.contains_key(&role) {
            let binding = decide::load_binding(tx, run_id, &role)?;
            bindings.insert(role.clone(), binding);
        }
        let capable = match (
            bindings.get(&role).and_then(Option::as_ref),
            context.required_capabilities.get(&role),
        ) {
            (Some(binding), Some(required)) => {
                binding.active && required.is_subset(&binding.capabilities)
            }
            _ => false,
        };
        rows.push(Row {
            candidate: Candidate {
                task_id,
                created_seq,
                layer,
                writer,
                user_value: user,
                unblock_value: unblock,
                risk_reduction: risk,
                information_gain: info,
                cost_estimate: Some(cost),
                wait_rounds,
                first_eligible_seq,
                eligibility: Eligibility {
                    dependencies_accepted: open == 0,
                    current_spec: run_spec == Some(spec_id.as_str()),
                    not_leased: true,
                    capable,
                    budget_reservable: context
                        .budget_headroom
                        .is_none_or(|headroom| u32::from(cost) <= headroom),
                },
            },
            state,
            revision,
            role,
        });
    }
    Ok(rows)
}
