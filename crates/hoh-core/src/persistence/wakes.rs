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
use crate::protocol::{BlockedReason, RunState, TaskState};
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
pub(super) const PREEMPT: &str = "supervisor.preempt";
const UNAVAILABLE: &str = "supervisor.unavailable";
const AVAILABLE: &str = "supervisor.available";
const PROBE: &str = "supervisor.probe";
/// The supervisor's blocker claims' source (decision 5).
const SUPERVISOR: &str = "supervisor";
/// The cooldown before a half-open probe: ADR-020's largest backoff (decision 5).
const COOLDOWN: Duration = crate::retry::CAP;
const TURN_LIMIT: &str = "supervisor.turn_limit";

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
    /// When it was allocated (RFC 3339 UTC) and that event's seq.
    pub allocated_at: Option<String>,
    pub allocated_seq: i64,
    /// Its attempt's role epoch and end (RFC 3339 UTC).
    pub role_epoch: i64,
    pub ended_at: Option<String>,
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
    /// Its events' seqs, over every trigger merged into it.
    pub event_seqs: Vec<i64>,
    pub turns: Vec<Turn>,
    pub superseded: bool,
    /// A turn of it was preempted: its later turns are protected (decision 3, -800).
    pub preempted: bool,
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
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3, ?4, ?5, ?6) \
             ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, String, String)> = statement
        .query_map(
            params![run_id, WOKEN, ALLOCATED, REARMED, SUPERSEDED, PREEMPT],
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
                let seqs: Vec<i64> = payload["event_seqs"]
                    .as_array()
                    .map(|s| s.iter().filter_map(Value::as_i64).collect())
                    .unwrap_or_default();
                if let Some(i) = wake(&mut wakes).filter(|_| payload["merged_into"].is_string()) {
                    wakes[i].severity = wakes[i].severity.max(severity);
                    wakes[i].task_ids.extend(tasks);
                    wakes[i].event_seqs.extend(seqs);
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
                        event_seqs: seqs,
                        turns: Vec::new(),
                        superseded: false,
                        preempted: false,
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
                        allocated_at: text(&payload, "allocated_at"),
                        allocated_seq: seq,
                        role_epoch: 0,
                        ended_at: None,
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
            PREEMPT => {
                if let Some(i) = text(&payload, "target_wake")
                    .and_then(|id| wakes.iter().position(|w| w.id == id))
                {
                    wakes[i].preempted = true;
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
            let row: Option<(String, i64, Option<String>)> = tx
                .query_row(
                    "SELECT state, role_epoch, ended_at FROM attempts WHERE attempt_id = ?1",
                    params![turn.attempt_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            if let Some((state, epoch, ended_at)) = row {
                turn.state = Some(super::writer::parse::<AttemptState>(&state)?);
                turn.role_epoch = epoch;
                turn.ended_at = ended_at;
            }
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
        let overdue: bool = tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = ?2 \
                 AND json_extract(payload_json, '$.attempt_id') = ?3)",
                params![run_id, TURN_LIMIT, turn.attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        // A turn stopped at its limit is a failure even when withdrawn before it started (decision 3).
        let reason = if preempted {
            "preempted"
        } else if turn.state == Some(AttemptState::Cancelled) && !overdue {
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
    /// Its turn is a probe of an unavailable Commander (decision 5).
    probe: bool,
    run_id: String,
    project_id: String,
    spec_id: String,
    run_revision: i64,
    role_epoch: u64,
    wake: Wake,
    /// 0 named by a preempt, 1 aged, 2 integrity, 3 the rest.
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
    // Preempts whose target has ended name the wake that takes the slot next, while it is still to be allocated
    // (decision 3, -800, -850).
    let named: Vec<(String, String, i64)> = {
        let mut statement = tx
            .prepare(
                "SELECT e.run_id, json_extract(e.payload_json, '$.wake'), e.seq FROM events e \
                 JOIN runs r ON r.run_id = e.run_id JOIN attempts a \
                 ON a.attempt_id = json_extract(e.payload_json, '$.target_attempt') \
                 WHERE r.project_id = ?1 AND e.kind = ?2 \
                 AND a.state IN ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT')",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id, PREEMPT], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
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
        // An unavailable Commander gets no turn but a probe (decision 5).
        let state: String = tx
            .query_row(
                "SELECT state FROM role_bindings WHERE run_id = ?1 AND role = ?2",
                params![run_id, COMMANDER],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let reasons = unavailability(&tx, &run_id, &wakes, epoch, &state)?;
        let probing = if reasons.is_empty() {
            None
        } else {
            match probe(&tx, &run_id, &reasons, &wakes, epoch, now)? {
                Some(target) => Some(target),
                None => continue,
            }
        };
        for wake in wakes
            .into_iter()
            .filter(Wake::owed)
            .filter(|w| w.retry_not_before().is_none_or(|t| t <= stamp.as_str()))
            .filter(|w| {
                probing
                    .as_ref()
                    .is_none_or(|target| target.as_ref().is_none_or(|id| *id == w.id))
            })
        {
            let aged = parse_rfc3339_millis(&wake.woken_at)
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|waited| waited > SUPERVISION_TURN);
            let first = named.iter().any(|(run, id, seq)| {
                *run == run_id
                    && *id == wake.id
                    && wake.turns.last().is_none_or(|t| t.allocated_seq < *seq)
            });
            let level = if first {
                0
            } else if aged {
                1
            } else if wake.severity == Severity::Integrity {
                2
            } else {
                3
            };
            candidates.push(Candidate {
                probe: probing.is_some(),
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
    if chosen.probe {
        rearmed.events.push(NewEvent {
            project_id: chosen.project_id.clone(),
            event_id: child_id(&task_id, "probe"),
            run_id: Some(chosen.run_id.clone()),
            kind: PROBE.to_owned(),
            payload: json!({"wake": chosen.wake.id, "attempt_id": attempt_id, "observation": null, "at": rfc3339_utc(now)}),
        });
    }
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

/// Whether run `run_id` could give a wake whose backoff ends at `not_before` a turn once its turn in flight stops
/// (decision 3, the preempt's condition): its Commander's binding ACTIVE with a session, its budget not stopping it,
/// the backoff over. The run's state is the caller's (it fires only in an admitting state).
pub(super) fn could_allocate(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
    not_before: Option<&str>,
) -> Result<bool, PersistError> {
    let bound: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM role_bindings WHERE run_id = ?1 AND role = ?2 AND state = 'ACTIVE' \
             AND session_id IS NOT NULL)",
            params![run_id, COMMANDER],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let now = rfc3339_utc(context.now());
    Ok(bound
        && super::retry::budget_stop(tx, run_id, context)?.is_none()
        && not_before.is_none_or(|t| t <= now.as_str()))
}

/// Why run `run_id`'s Commander, at role epoch `epoch` and binding state `binding`, is unavailable now (empty:
/// available; decision 5): its binding BLOCKED (`binding_blocked`); an owed wake with MAX_RETRIES failures under the
/// epoch (`failures_capped`); its last turn of the epoch that ended failed for authentication or capability (`auth`,
/// `capability`).
fn unavailability(
    tx: &Transaction<'_>,
    run_id: &str,
    wakes: &[Wake],
    epoch: i64,
    binding: &str,
) -> Result<Vec<&'static str>, PersistError> {
    let mut reasons = Vec::new();
    if binding == "BLOCKED" {
        reasons.push("binding_blocked");
    }
    let cap = i64::from(crate::retry::MAX_RETRIES);
    if wakes.iter().any(|w| w.owed() && w.failures(epoch) >= cap) {
        reasons.push("failures_capped");
    }
    let last = wakes
        .iter()
        .flat_map(|w| w.turns.iter())
        .filter(|t| t.role_epoch == epoch && t.ended())
        .max_by_key(|t| t.ended_at.clone());
    if let Some(turn) = last.filter(|t| t.state != Some(AttemptState::Settled)) {
        let class: Option<String> = tx
            .query_row(
                "SELECT json_extract(payload_json, '$.failure.class') FROM events WHERE run_id = ?1 \
                 AND kind = 'supervisor.turn_failed' AND json_extract(payload_json, '$.attempt_id') = ?2",
                params![run_id, turn.attempt_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?
            .flatten();
        match class.as_deref() {
            Some("auth") => reasons.push("auth"),
            Some("capability") => reasons.push("capability"),
            _ => {}
        }
    }
    Ok(reasons)
}

/// The run's last `supervisor.unavailable` or `supervisor.available`.
struct Recorded {
    unavailable: bool,
    reasons: Value,
    /// Its time (the supervisor's clock) and seq.
    at: String,
    seq: i64,
}

fn recorded(tx: &Transaction<'_>, run_id: &str) -> Result<Option<Recorded>, PersistError> {
    let row: Option<(i64, String, String)> = tx
        .query_row(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3) \
             ORDER BY seq DESC LIMIT 1",
            params![run_id, UNAVAILABLE, AVAILABLE],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(|(seq, kind, payload)| {
        let payload: Value = serde_json::from_str(&payload)
            .map_err(|e| PersistError::IntegrityViolation(format!("{kind} of {run_id}: {e}")))?;
        Ok(Recorded {
            unavailable: kind == UNAVAILABLE,
            reasons: payload["reasons"].clone(),
            at: text(&payload, "at").unwrap_or_default(),
            seq,
        })
    })
    .transpose()
}

/// Judge run `run_id`'s Commander at `context.now()` (decision 5), in the supervisor's step: unavailable, it records
/// `supervisor.unavailable{reasons}` when that changes and, while the run owes it a decision (an owed wake or a
/// PENDING trigger), claims COMMANDER_UNAVAILABLE (and NEED_DECISION when a need_decision is owed) for the run, which
/// is BLOCKED by it, its routine (normal) triggers superseded; available again, `supervisor.available` and the claims
/// released (the run back in its state once nothing else is claimed). Only in a state that admits supervision.
pub(super) fn judge(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Commit, PersistError> {
    let mut commit = Commit::default();
    let row: Option<(String, i64, String, Option<String>)> = tx
        .query_row(
            "SELECT b.state, b.role_epoch, r.state, r.resume_state FROM role_bindings b JOIN runs r \
             ON r.run_id = b.run_id WHERE b.run_id = ?1 AND b.role = ?2",
            params![run_id, COMMANDER],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((binding, epoch, state, resume)) = row else {
        return Ok(commit);
    };
    let state = writer::parse::<RunState>(&state)?;
    let working = crate::lifecycle::is_working(state);
    if !working && !matches!(state, RunState::Blocked | RunState::WaitingExternal) {
        return Ok(commit);
    }
    let now = context.now();
    let at = rfc3339_utc(now);
    let wakes = of_run(tx, run_id)?;
    let reasons = unavailability(tx, run_id, &wakes, epoch, &binding)?;
    let last = recorded(tx, run_id)?;
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let number: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE run_id = ?1 AND kind IN (?2, ?3)",
            params![run_id, UNAVAILABLE, AVAILABLE],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let mut desired: Vec<BlockedReason> = Vec::new();
    let mut owed_records = Vec::new();
    if reasons.is_empty() {
        if last.as_ref().is_some_and(|r| r.unavailable) {
            commit.events.push(event(
                child_id(run_id, &format!("supervisor/{number}")),
                AVAILABLE,
                json!({"at": at, "role_epoch": epoch}),
            ));
        }
    } else {
        if last
            .as_ref()
            .is_none_or(|r| !r.unavailable || r.reasons != json!(reasons))
        {
            commit.events.push(event(
                child_id(run_id, &format!("supervisor/{number}")),
                UNAVAILABLE,
                json!({"reasons": reasons, "observation": null, "at": at, "role_epoch": epoch}),
            ));
        }
        // What the run owes its Commander: its owed wakes and its PENDING triggers.
        let mut seqs: Vec<i64> = Vec::new();
        for wake in wakes.iter().filter(|w| w.owed()) {
            seqs.extend(&wake.event_seqs);
            owed_records.push(json!({"wake": wake.id, "fingerprint": wake.fingerprint}));
        }
        let triggers: Vec<(String, String, String)> = {
            let mut statement = tx
                .prepare(
                    "SELECT trigger_id, severity, event_seqs_json FROM supervisor_triggers \
                     WHERE run_id = ?1 AND state = 'PENDING' ORDER BY first_seq",
                )
                .map_err(sqlite_error)?;
            statement
                .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?
        };
        for (trigger_id, severity, trigger_seqs) in triggers {
            if severity == Severity::Normal.as_str() {
                // A routine trigger is not kept while the Commander is unavailable (decision 5).
                tx.execute(
                    "UPDATE supervisor_triggers SET state = 'SUPERSEDED' WHERE trigger_id = ?1 AND state = 'PENDING'",
                    params![trigger_id],
                )
                .map_err(sqlite_error)?;
                commit.events.push(event(
                    child_id(&trigger_id, "superseded"),
                    SUPERSEDED,
                    json!({"trigger_id": trigger_id, "reason": "COMMANDER_UNAVAILABLE"}),
                ));
                continue;
            }
            seqs.extend(serde_json::from_str::<Vec<i64>>(&trigger_seqs).unwrap_or_default());
            owed_records.push(json!({"trigger_id": trigger_id}));
        }
        if !owed_records.is_empty() {
            desired.push(BlockedReason::CommanderUnavailable);
            let decision: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'need_decision' \
                     AND seq IN (SELECT value FROM json_each(?1)))",
                    params![json!(seqs).to_string()],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if decision {
                desired.push(BlockedReason::NeedDecision);
            }
        }
    }
    let mut claims = super::blockers::Claims::load(tx, project_id, run_id)?;
    let before = claims.reasons();
    let had = claims.held_by(SUPERVISOR);
    claims.set(SUPERVISOR, &desired);
    let union = claims.reasons();
    let changed = union != before;
    commit.events.extend(claims.store(tx)?);
    let revision: i64 = tx
        .query_row(
            "SELECT revision FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !desired.is_empty() && !had {
        commit.events.push(event(
            child_id(run_id, &format!("supervisor/{number}/blocked")),
            "supervisor.blocked",
            json!({"reasons": reasons, "owed": owed_records}),
        ));
    }
    if !union.is_empty() && state != RunState::Blocked {
        let (moves, withdrawn) = super::control::withdraw(
            tx,
            project_id,
            run_id,
            &at,
            super::control::Withdraw::Blocked,
            ctx,
        )?;
        commit.cas.extend(moves);
        commit.events.extend(withdrawn);
        commit.cas.push(StateCas {
            id: run_id.to_owned(),
            expected_revision: revision,
            change: Change::Run(RunState::Blocked),
        });
        commit.events.push(event(
            child_id(run_id, &format!("blocked/{revision}")),
            "run.blocked",
            json!({"reasons": union.iter().map(|r| r.as_str()).collect::<Vec<_>>(), "detail": "the commander is unavailable"}),
        ));
    } else if state == RunState::Blocked && changed {
        if union.is_empty() {
            let to = resume
                .as_deref()
                .map(writer::parse::<RunState>)
                .transpose()?
                .unwrap_or(RunState::Running);
            commit.cas.push(StateCas {
                id: run_id.to_owned(),
                expected_revision: revision,
                change: Change::Unblock(to),
            });
            commit.events.push(event(
                child_id(run_id, &format!("resumed/{revision}")),
                "run.resumed",
                json!({"from": state.as_str(), "to": to.as_str(), "due": []}),
            ));
        } else {
            commit.cas.push(StateCas {
                id: run_id.to_owned(),
                expected_revision: revision,
                change: Change::Touch(writer::Entity::Run),
            });
        }
    }
    Ok(commit)
}

/// Whether run `run_id`'s Commander, unavailable for `reasons`, may be probed at `now` (decision 5): only for capped
/// failures or a failed turn's class, its binding active; one probe per cooldown from the unavailability (or from the
/// last probe's failed end), the next at once after a probe that settled. The wake to probe: the oldest capped one.
fn probe(
    tx: &Transaction<'_>,
    run_id: &str,
    reasons: &[&str],
    wakes: &[Wake],
    epoch: i64,
    now: std::time::SystemTime,
) -> Result<Option<Option<String>>, PersistError> {
    if reasons.is_empty()
        || reasons
            .iter()
            .any(|r| !matches!(*r, "failures_capped" | "auth" | "capability"))
    {
        return Ok(None);
    }
    let Some(since) = recorded(tx, run_id)?.filter(|r| r.unavailable) else {
        return Ok(None);
    };
    let last: Option<String> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.attempt_id') FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND seq > ?3 ORDER BY seq DESC LIMIT 1",
            params![run_id, PROBE, since.seq],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    // The cooldown runs from the unavailability, or from the last probe's failed end; a probe that settled lets the
    // next one go at once.
    let cooled = match last {
        None => crate::time::parse_rfc3339(&since.at).is_some_and(|from| from + COOLDOWN <= now),
        Some(attempt) => match wakes
            .iter()
            .flat_map(|w| w.turns.iter())
            .find(|t| t.attempt_id == attempt)
        {
            Some(t) if t.state == Some(AttemptState::Settled) => true,
            Some(t) if t.ended() => t
                .ended_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .is_some_and(|from| from + COOLDOWN <= now),
            _ => false,
        },
    };
    if !cooled {
        return Ok(None);
    }
    let cap = i64::from(crate::retry::MAX_RETRIES);
    Ok(Some(
        wakes
            .iter()
            .filter(|w| w.owed() && w.failures(epoch) >= cap)
            .min_by_key(|w| w.woken_seq)
            .map(|w| w.id.clone()),
    ))
}

/// A supervision turn in flight the host must stop now (decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub run_id: String,
    pub wake: String,
    pub attempt_id: String,
    /// `preempted` (a preempt names it) or `turn_limit` (in flight longer than [`SUPERVISION_TURN`]).
    pub reason: &'static str,
    /// Its attempt's state: a PREPARED one is withdrawn in the store, a STARTING or RUNNING one settled by the host.
    pub state: String,
}

/// The supervision turns of project `project_id` to stop at `context.now()`: those a preempt names, and those in flight
/// longer than a turn's limit, whose `supervisor.turn_limit` is recorded once here.
pub(super) fn stops(
    conn: &mut Connection,
    project_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Vec<Stop>, PersistError> {
    let now = context.now();
    let tx = writer::begin(conn, ctx)?;
    let runs: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT run_id FROM runs WHERE project_id = ?1 ORDER BY created_at, run_id")
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let mut found = Vec::new();
    let mut events = Vec::new();
    for run_id in runs {
        for wake in of_run(&tx, &run_id)?.into_iter().filter(Wake::in_flight) {
            let Some(turn) = wake.turns.last() else {
                continue;
            };
            let preempted: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = ?2 \
                     AND json_extract(payload_json, '$.target_attempt') = ?3)",
                    params![run_id, PREEMPT, turn.attempt_id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            let overdue = turn
                .allocated_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .is_some_and(|at| at + SUPERVISION_TURN < now);
            let reason = if preempted {
                "preempted"
            } else if overdue {
                let recorded: bool = tx
                    .query_row(
                        "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = ?2 \
                         AND json_extract(payload_json, '$.attempt_id') = ?3)",
                        params![run_id, TURN_LIMIT, turn.attempt_id],
                        |r| r.get(0),
                    )
                    .map_err(sqlite_error)?;
                if !recorded {
                    events.push(NewEvent {
                        project_id: project_id.to_owned(),
                        event_id: child_id(&turn.attempt_id, "turn_limit"),
                        run_id: Some(run_id.clone()),
                        kind: TURN_LIMIT.to_owned(),
                        payload: json!({
                            "wake": wake.id,
                            "attempt_id": turn.attempt_id,
                            "allocated_at": turn.allocated_at,
                            "limit_ms": u64::try_from(SUPERVISION_TURN.as_millis()).unwrap_or(u64::MAX),
                            "at": rfc3339_utc(now),
                        }),
                    });
                }
                "turn_limit"
            } else {
                continue;
            };
            found.push(Stop {
                run_id: run_id.clone(),
                wake: wake.id.clone(),
                attempt_id: turn.attempt_id.clone(),
                reason,
                state: turn
                    .state
                    .map_or_else(String::new, |s| s.as_str().to_owned()),
            });
        }
    }
    if !events.is_empty() {
        writer::write(
            &tx,
            &Commit {
                events,
                ..Commit::default()
            },
            ctx,
        )?;
        ctx.before_commit()?;
        tx.commit().map_err(sqlite_error)?;
    }
    Ok(found)
}

/// Withdraw supervision turn `attempt_id` while it is PREPARED, at `at` (decision 3: a preempted or overdue turn not
/// started yet): its attempt and task CANCELLED, its dispatch notice settled, its slot free; whether it was withdrawn.
pub(super) fn withdraw_turn(
    conn: &mut Connection,
    attempt_id: &str,
    at: std::time::SystemTime,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let row: Option<(String, String, i64, String, String, String)> = tx
        .query_row(
            "SELECT a.state, t.task_id, t.revision, t.state, t.run_id, r.project_id FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id \
             WHERE a.attempt_id = ?1 AND t.role = ?2",
            params![attempt_id, COMMANDER],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, task_id, revision, task_state, run_id, project_id)) = row else {
        return Ok(false);
    };
    if state != AttemptState::Prepared.as_str() {
        return Ok(false);
    }
    super::attempts::move_attempt(
        &tx,
        &super::attempts::AttemptMove {
            attempt_id: attempt_id.to_owned(),
            from: AttemptState::Prepared,
            to: AttemptState::Cancelled,
            facts: super::attempts::AttemptFacts {
                ended_at: Some(rfc3339_utc(at)),
                ..super::attempts::AttemptFacts::default()
            },
        },
        ctx,
    )?;
    tx.execute(
        "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE state = 'PENDING' \
         AND json_extract(payload_json, '$.notice') IS 'runtime' AND json_extract(payload_json, '$.kind') IS 'dispatch' \
         AND json_extract(payload_json, '$.attempt_id') = ?1",
        params![attempt_id],
    )
    .map_err(sqlite_error)?;
    let mut cas = Vec::new();
    if writer::parse::<TaskState>(&task_state)? == TaskState::Leased {
        cas.push(StateCas {
            id: task_id.clone(),
            expected_revision: revision,
            change: Change::Task(TaskState::Cancelled),
        });
    }
    writer::write(
        &tx,
        &Commit {
            cas,
            events: vec![NewEvent {
                project_id,
                event_id: child_id(attempt_id, "withdrawn"),
                run_id: Some(run_id),
                kind: "supervisor.withdrawn".to_owned(),
                payload: json!({"attempt_id": attempt_id, "task_id": task_id}),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
