//! The Commander's wakes (docs/adr/ADR-025.md decision 3), read from a run's `supervisor.*` events: a fired trigger
//! owes the Commander one turn. A wake is its first `supervisor.woken` (its id is that trigger's id); a later trigger
//! of its key woken while it is owed and not in flight joins it (`merged_into`), adding its tasks and raising its
//! severity. A wake is owed until a turn of it SETTLES or it is superseded; a turn of it is in flight from its
//! `supervisor.allocated` until its attempt ends.

use super::decide::{self, LeaseRequest, Leased};
use super::writer::{self, Change, Commit, Ctx, NewEvent, NewOutbox, StateCas};
use super::{PersistError, leases, sqlite_error};
use crate::decision::Context;
use crate::ids::child_id;
use crate::lifecycle::{AttemptState, Lifecycle as _};
use crate::protocol::TaskState;
use crate::supervise::Severity;
use crate::time::{parse_rfc3339_millis, rfc3339_utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

pub(super) const WOKEN: &str = "supervisor.woken";
pub(super) const ALLOCATED: &str = "supervisor.allocated";
pub(super) const REARMED: &str = "supervisor.rearmed";
pub(super) const SUPERSEDED: &str = "supervisor.superseded";

/// How long a supervision turn may be in flight, and how long a wake waits before it goes first (decision 3).
pub const SUPERVISION_TURN: Duration = Duration::from_secs(15 * 60);

/// The supervision task's role: the Commander does no work task (CONTRACT 6, docs/06).
pub(super) const COMMANDER: &str = "commander";

/// What a turn's `supervisor.rearmed` records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rearm {
    /// `failure`, `preempted` or `withdrawn`.
    pub reason: String,
    /// The wake's failures under `role_epoch`, this one counted.
    pub failures: i64,
    pub role_epoch: i64,
    pub retry_not_before: Option<String>,
}

/// One turn given to a wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Turn {
    pub generation: i64,
    pub attempt_id: String,
    /// Its attempt's state now (`None`: no such attempt).
    pub state: Option<AttemptState>,
    /// Its `supervisor.rearmed`, once written.
    pub rearm: Option<Rearm>,
}

impl Turn {
    fn ended(&self) -> bool {
        self.state.is_none_or(AttemptState::is_terminal)
    }
}

/// A wake as its events tell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wake {
    pub id: String,
    pub fingerprint: String,
    pub spec_id: Option<String>,
    pub severity: Severity,
    /// When it was first woken (milliseconds UTC) and that event's seq.
    pub woken_at: String,
    pub woken_seq: i64,
    /// The tasks its events name, over every trigger merged into it.
    pub task_ids: BTreeSet<String>,
    pub turns: Vec<Turn>,
    pub superseded: bool,
}

impl Wake {
    /// A turn of it SETTLED: it is answered.
    pub fn settled(&self) -> bool {
        self.turns
            .last()
            .is_some_and(|t| t.state == Some(AttemptState::Settled))
    }

    /// A turn of it has not ended.
    pub fn in_flight(&self) -> bool {
        self.turns.last().is_some_and(|t| !t.ended())
    }

    /// Owed a turn: not answered, not superseded, none in flight.
    pub fn owed(&self) -> bool {
        !self.superseded && !self.settled() && !self.in_flight()
    }

    /// Its failures under the Commander's epoch `epoch`: a failure of another epoch does not count, a preempted or
    /// withdrawn turn neither counts nor breaks the run (M2T08D-950, -1054).
    pub fn failures(&self, epoch: i64) -> i64 {
        let counted = self
            .turns
            .iter()
            .filter_map(|t| t.rearm.as_ref())
            .filter(|r| r.reason == "failure" && r.role_epoch == epoch)
            .count();
        i64::try_from(counted).unwrap_or(i64::MAX)
    }

    /// Not to be allocated before this time: its last failure's backoff.
    pub fn retry_not_before(&self) -> Option<&str> {
        self.turns
            .last()
            .and_then(|t| t.rearm.as_ref())
            .and_then(|r| r.retry_not_before.as_deref())
    }
}

/// Whether `task_id` is a supervision task (role commander).
pub(super) fn is_supervision(tx: &Connection, task_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM tasks WHERE task_id = ?1 AND role = ?2)",
        params![task_id, COMMANDER],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

fn text(payload: &Value, field: &str) -> Option<String> {
    payload[field].as_str().map(str::to_owned)
}

/// Run `run_id`'s wakes, in the order they were first woken.
pub(super) fn of_run(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<Wake>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3, ?4, ?5) ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, String, String)> = statement
        .query_map(
            params![run_id, WOKEN, ALLOCATED, REARMED, SUPERSEDED],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut wakes: Vec<Wake> = Vec::new();
    for (seq, kind, payload) in rows {
        let payload: Value = serde_json::from_str(&payload).map_err(|e| {
            PersistError::IntegrityViolation(format!("supervisor event {seq} of {run_id}: {e}"))
        })?;
        let named = text(&payload, "wake");
        let wake = |wakes: &mut Vec<Wake>| -> Option<usize> {
            named
                .as_deref()
                .and_then(|id| wakes.iter().position(|w| w.id == id))
        };
        match kind.as_str() {
            WOKEN => {
                let severity = payload["severity"]
                    .as_str()
                    .and_then(Severity::named)
                    .unwrap_or(Severity::Normal);
                let tasks = payload["task_ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default();
                if let Some(i) = wake(&mut wakes).filter(|_| payload["merged_into"].is_string()) {
                    wakes[i].severity = wakes[i].severity.max(severity);
                    wakes[i].task_ids.extend(tasks);
                } else {
                    wakes.push(Wake {
                        id: named
                            .or_else(|| text(&payload, "trigger_id"))
                            .unwrap_or_default(),
                        fingerprint: text(&payload["dedup_key"], "fingerprint").unwrap_or_default(),
                        spec_id: text(&payload["dedup_key"], "spec_id"),
                        severity,
                        woken_at: text(&payload, "woken_at").unwrap_or_default(),
                        woken_seq: seq,
                        task_ids: tasks,
                        turns: Vec::new(),
                        superseded: false,
                    });
                }
            }
            ALLOCATED => {
                if let (Some(i), Some(attempt_id)) =
                    (wake(&mut wakes), text(&payload, "attempt_id"))
                {
                    wakes[i].turns.push(Turn {
                        generation: payload["generation"].as_i64().unwrap_or(0),
                        attempt_id,
                        state: None,
                        rearm: None,
                    });
                }
            }
            REARMED => {
                if let (Some(i), Some(attempt_id)) =
                    (wake(&mut wakes), text(&payload, "attempt_id"))
                    && let Some(turn) = wakes[i]
                        .turns
                        .iter_mut()
                        .find(|t| t.attempt_id == attempt_id)
                {
                    turn.rearm = Some(Rearm {
                        reason: text(&payload, "reason").unwrap_or_default(),
                        failures: payload["failures"].as_i64().unwrap_or(0),
                        role_epoch: payload["role_epoch"].as_i64().unwrap_or(0),
                        retry_not_before: text(&payload, "retry_not_before"),
                    });
                }
            }
            _ => {
                if let Some(i) = wake(&mut wakes) {
                    wakes[i].superseded = true;
                }
            }
        }
    }
    for wake in &mut wakes {
        for turn in &mut wake.turns {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM attempts WHERE attempt_id = ?1",
                    params![turn.attempt_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            turn.state = state
                .as_deref()
                .map(super::writer::parse::<AttemptState>)
                .transpose()?;
        }
    }
    Ok(wakes)
}

/// Rearm run `run_id`'s wakes whose last turn ended without settling and is not rearmed yet (decision 3): each owed
/// again, with `supervisor.rearmed{reason, failures, retry_not_before, role_epoch}` (a failure counted under the
/// Commander's epoch `epoch` and backed off by ADR-020's bound; a preempted or withdrawn turn neither), and its
/// supervision task CANCELLED if no ending moved it yet. The moves and events for the caller's commit.
fn rearm(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    wakes: &mut [Wake],
    epoch: i64,
    now: std::time::SystemTime,
) -> Result<(Vec<StateCas>, Vec<NewEvent>), PersistError> {
    let mut cas = Vec::new();
    let mut events = Vec::new();
    for wake in wakes.iter_mut().filter(|w| !w.superseded && !w.settled()) {
        let failures = wake.failures(epoch);
        let Some(turn) = wake.turns.last_mut() else {
            continue;
        };
        if !turn.ended() || turn.rearm.is_some() {
            continue;
        }
        let preempted: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'supervisor.preempt' \
                 AND json_extract(payload_json, '$.target_attempt') = ?2)",
                params![run_id, turn.attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let reason = if preempted {
            "preempted"
        } else if turn.state == Some(AttemptState::Cancelled) {
            "withdrawn"
        } else {
            "failure"
        };
        let (task_id, task_state, task_revision, ended_at): (String, String, i64, Option<String>) = tx
            .query_row(
                "SELECT t.task_id, t.state, t.revision, a.ended_at FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE a.attempt_id = ?1",
                params![turn.attempt_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .map_err(sqlite_error)?;
        let (failures, retry_not_before) = if reason == "failure" {
            let failures = failures + 1;
            let from = ended_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .unwrap_or(now);
            let n = u32::try_from(failures - 1).unwrap_or(u32::MAX);
            (failures, Some(rfc3339_utc(from + crate::retry::bound(n))))
        } else {
            (failures, None)
        };
        let task_state = writer::parse::<TaskState>(&task_state)?;
        if !matches!(
            task_state,
            TaskState::Cancelled | TaskState::Implemented | TaskState::Accepted
        ) {
            cas.push(StateCas {
                id: task_id.clone(),
                expected_revision: task_revision,
                change: Change::Task(TaskState::Cancelled),
            });
        }
        events.push(NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(&turn.attempt_id, "rearmed"),
            run_id: Some(run_id.to_owned()),
            kind: REARMED.to_owned(),
            payload: json!({
                "wake": wake.id,
                "dedup_key": {"run_id": run_id, "fingerprint": wake.fingerprint, "spec_id": wake.spec_id},
                "attempt_id": turn.attempt_id,
                "generation": turn.generation,
                "reason": reason,
                "failures": failures,
                "retry_not_before": retry_not_before,
                "role_epoch": epoch,
            }),
        });
        turn.rearm = Some(Rearm {
            reason: reason.to_owned(),
            failures,
            role_epoch: epoch,
            retry_not_before,
        });
    }
    Ok((cas, events))
}

/// A wake given a Commander turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Allocated {
    pub run_id: String,
    pub wake: String,
    /// The run's allocations before this one.
    pub generation: i64,
    pub task_id: String,
    pub attempt_id: String,
    pub lease: leases::Grant,
}

/// A wake that may be allocated now, with what orders it.
struct Candidate {
    run_id: String,
    project_id: String,
    spec_id: String,
    run_revision: i64,
    role_epoch: u64,
    wake: Wake,
    /// 0 aged, 1 integrity, 2 the rest.
    level: u8,
    /// The seq of the run's last allocation (-1: none): the run allocated last goes last within a level.
    last: i64,
}

/// One owed wake of project `project_id` given a Commander turn at `context.now()`, in one transaction (decision 3):
/// the supervisor slot (a reader slot when `reserve_supervisor_slot` is false) must be free; the wake's run must admit
/// it (working, BLOCKED or WAITING_EXTERNAL, with a spec), have no turn in flight, an active Commander and a budget
/// that does not stop it. The order: a wake waiting longer than a turn's limit, then integrity, then the rest, each
/// by wake time, the runs taking turns within a level. Neither the run's state nor its revision moves.
pub(super) fn allocate(
    conn: &mut Connection,
    project_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Option<Allocated>, PersistError> {
    let now = context.now();
    let tx = writer::begin(conn, ctx)?;
    let kind = if context.reserve_supervisor_slot() {
        leases::is_free(&tx, project_id, "supervisor", now)?.then(|| "supervisor".to_owned())
    } else {
        leases::free_slot(&tx, project_id, false, context.reader_slots(), now)?
    };
    let Some(kind) = kind else {
        return Ok(None);
    };
    let runs: Vec<(String, String, i64)> = {
        let mut statement = tx
            .prepare(
                "SELECT run_id, spec_id, revision FROM runs WHERE project_id = ?1 AND spec_id IS NOT NULL \
                 AND state IN ('CREATED', 'DISCOVERING', 'SPECIFYING', 'READY', 'RUNNING', 'VERIFYING', 'BLOCKED', \
                 'WAITING_EXTERNAL') ORDER BY created_at, run_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let mut last: BTreeMap<String, i64> = BTreeMap::new();
    {
        let mut statement = tx
            .prepare(
                "SELECT e.run_id, max(e.seq) FROM events e JOIN runs r ON r.run_id = e.run_id \
                 WHERE r.project_id = ?1 AND e.kind = ?2 GROUP BY e.run_id",
            )
            .map_err(sqlite_error)?;
        for row in statement
            .query_map(params![project_id, ALLOCATED], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(sqlite_error)?
        {
            let (run, seq): (String, i64) = row.map_err(sqlite_error)?;
            last.insert(run, seq);
        }
    }
    let mut candidates = Vec::new();
    let mut rearmed = Commit::default();
    let stamp = rfc3339_utc(now);
    for (run_id, spec_id, run_revision) in runs {
        let Some(binding) = decide::load_binding(&tx, &run_id, COMMANDER)? else {
            continue;
        };
        let mut wakes = of_run(&tx, &run_id)?;
        let epoch = decide::to_i64(binding.role_epoch, "role_epoch")?;
        let (moves, events) = rearm(&tx, project_id, &run_id, &mut wakes, epoch, now)?;
        rearmed.cas.extend(moves);
        rearmed.events.extend(events);
        if !binding.active || super::retry::budget_stop(&tx, &run_id, context)?.is_some() {
            continue;
        }
        if wakes.iter().any(Wake::in_flight) {
            continue;
        }
        for wake in wakes
            .into_iter()
            .filter(Wake::owed)
            .filter(|w| w.retry_not_before().is_none_or(|t| t <= stamp.as_str()))
        {
            let aged = parse_rfc3339_millis(&wake.woken_at)
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|waited| waited > SUPERVISION_TURN);
            let level = if aged {
                0
            } else if wake.severity == Severity::Integrity {
                1
            } else {
                2
            };
            candidates.push(Candidate {
                run_id: run_id.clone(),
                project_id: project_id.to_owned(),
                spec_id: spec_id.clone(),
                run_revision,
                role_epoch: binding.role_epoch,
                last: last.get(&run_id).copied().unwrap_or(-1),
                level,
                wake,
            });
        }
    }
    let Some(chosen) = candidates.into_iter().min_by(|a, b| {
        (a.level, a.last, &a.wake.woken_at, a.wake.woken_seq).cmp(&(
            b.level,
            b.last,
            &b.wake.woken_at,
            b.wake.woken_seq,
        ))
    }) else {
        if !rearmed.events.is_empty() {
            writer::write(&tx, &rearmed, ctx)?;
            ctx.before_commit()?;
            tx.commit().map_err(sqlite_error)?;
        }
        return Ok(None);
    };
    let generation: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE run_id = ?1 AND kind = ?2",
            params![chosen.run_id, ALLOCATED],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let task_id = format!("{}/turn/{generation}", chosen.wake.id);
    tx.execute(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES \
         (?1, ?2, ?3, 'LEASED', 0, ?4, 0, (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks WHERE run_id = ?2))",
        params![task_id, chosen.run_id, chosen.spec_id, COMMANDER],
    )
    .map_err(sqlite_error)?;
    let Leased { attempt_id, grant } = decide::lease_task(
        &tx,
        &LeaseRequest {
            project_id: &chosen.project_id,
            run_id: &chosen.run_id,
            attempt_id: child_id(&task_id, "attempt-1"),
            task_id: &task_id,
            role: COMMANDER,
            role_epoch: chosen.role_epoch,
            kind: &kind,
            now,
        },
    )?;
    // The revisions of the tasks it names, as its turn is shown them: the fence of its decisions (decisions 3, 6).
    let mut snapshots = serde_json::Map::new();
    for task in &chosen.wake.task_ids {
        let revision: Option<i64> = tx
            .query_row(
                "SELECT revision FROM tasks WHERE task_id = ?1",
                params![task],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(revision) = revision {
            snapshots.insert(task.clone(), json!(revision));
        }
    }
    let lease = json!({"kind": grant.kind, "epoch": grant.epoch, "expires_at": grant.expires_at});
    let role_epoch = decide::to_i64(chosen.role_epoch, "role_epoch")?;
    // The dispatch notice refers to the allocation's event, after the rearms.
    let allocated_at = rearmed.events.len();
    let commit = Commit {
        cas: rearmed.cas,
        events: rearmed
            .events
            .into_iter()
            .chain([NewEvent {
            project_id: chosen.project_id.clone(),
            event_id: child_id(&task_id, "allocated"),
            run_id: Some(chosen.run_id.clone()),
            kind: ALLOCATED.to_owned(),
            payload: json!({
                "wake": chosen.wake.id,
                "dedup_key": {"run_id": chosen.run_id, "fingerprint": chosen.wake.fingerprint, "spec_id": chosen.wake.spec_id},
                "generation": generation,
                "task_id": task_id,
                "attempt_id": attempt_id,
                "lease": lease,
                "snapshots": snapshots,
                "run_revision": chosen.run_revision,
                "role_epoch": role_epoch,
                "allocated_at": rfc3339_utc(now),
            }),
        }])
            .collect(),
        outbox: vec![NewOutbox {
            message_id: child_id(&attempt_id, "dispatch"),
            event: allocated_at,
            run_id: Some(chosen.run_id.clone()),
            recipient_role: COMMANDER.to_owned(),
            role_epoch,
            payload: json!({
                "notice": "runtime",
                "kind": "dispatch",
                "task_id": task_id,
                "role": COMMANDER,
                "attempt_id": attempt_id,
                "lease": lease,
                "origin": "supervision",
                "wake": chosen.wake.id,
            }),
        }],
    };
    writer::write(&tx, &commit, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Some(Allocated {
        run_id: chosen.run_id,
        wake: chosen.wake.id,
        generation,
        task_id,
        attempt_id,
        lease: grant,
    }))
}

#[cfg(test)]
mod tests;
