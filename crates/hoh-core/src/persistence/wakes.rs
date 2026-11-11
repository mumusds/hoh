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
const HORIZON: &str = "supervisor.horizon";
/// The supervisor's blocker claims' source (decision 5).
const SUPERVISOR: &str = "supervisor";
/// The cooldown before a half-open probe: ADR-020's largest backoff (decision 5).
const COOLDOWN: Duration = crate::retry::CAP;
const TURN_LIMIT: &str = "supervisor.turn_limit";
const WORKDIR: &str = "commander.workdir";

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
    /// Its attempt has ended: a terminal state, or UNKNOWN_EFFECT (its outcome unknown, no answer recorded; decision 3:
    /// the wake is rearmed, a failure, and asked again; found after round 3: it was left in flight for ever).
    fn ended(&self) -> bool {
        self.state
            .is_none_or(|s| s.is_terminal() || s == AttemptState::UnknownEffect)
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
    /// The latest backoff a woken of it set (a Commander's own mistake re-triggered, decision 6), with that woken's
    /// seq: it holds until a turn is allocated after it.
    pub not_before: Option<(String, i64)>,
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

    /// Not to be allocated before this time: the later of its last failure's backoff and the backoff a woken of it set
    /// after its last allocation (two re-triggers merged take the later, -853).
    pub fn retry_not_before(&self) -> Option<std::time::SystemTime> {
        let rearm = self
            .turns
            .last()
            .and_then(|t| t.rearm.as_ref())
            .and_then(|r| r.retry_not_before.as_deref())
            .and_then(crate::time::parse_rfc3339_millis);
        let woken = self
            .not_before
            .as_ref()
            .filter(|(_, seq)| self.turns.last().is_none_or(|t| t.allocated_seq < *seq))
            .and_then(|(at, _)| crate::time::parse_rfc3339_millis(at));
        rearm.max(woken)
    }
}

/// Lower-case hex of the sha256 of `bytes`.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// 32 random bytes as hex: a turn's nonce.
fn nonce() -> Result<String, PersistError> {
    let mut bytes = [0u8; 32];
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())
                .map_err(|e| PersistError::Storage(format!("no random bytes for a nonce: {e}")))?;
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
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
pub(super) fn of_run(tx: &Connection, run_id: &str) -> Result<Vec<Wake>, PersistError> {
    // Only after the run's horizon: the wakes before it are closed for good (v19, M2T08R-352).
    let from = horizon(tx, run_id)?;
    let mut statement = tx
        .prepare(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3, ?4, ?5, ?6) \
             AND seq >= ?7 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, String, String)> = statement
        .query_map(
            params![run_id, WOKEN, ALLOCATED, REARMED, SUPERSEDED, PREEMPT, from],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut wakes: Vec<Wake> = Vec::new();
    // Each wake's place by its id: linear in the run's supervisor events (v14, M2T08R-101).
    let mut index: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for (seq, kind, payload) in rows {
        let payload: Value = serde_json::from_str(&payload).map_err(|e| {
            PersistError::IntegrityViolation(format!("supervisor event {seq} of {run_id}: {e}"))
        })?;
        let named = text(&payload, "wake");
        let wake = || -> Option<usize> { named.as_deref().and_then(|id| index.get(id).copied()) };
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
                let not_before = text(&payload, "retry_not_before").map(|at| (at, seq));
                if let Some(i) = wake().filter(|_| payload["merged_into"].is_string()) {
                    wakes[i].severity = wakes[i].severity.max(severity);
                    wakes[i].task_ids.extend(tasks);
                    wakes[i].event_seqs.extend(seqs);
                    if let Some((at, seq)) = not_before {
                        let later = wakes[i]
                            .not_before
                            .as_ref()
                            .map_or(at.clone(), |(was, _)| was.clone().max(at));
                        wakes[i].not_before = Some((later, seq));
                    }
                } else {
                    let id = named
                        .or_else(|| text(&payload, "trigger_id"))
                        .unwrap_or_default();
                    index.entry(id.clone()).or_insert(wakes.len());
                    wakes.push(Wake {
                        id,
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
                        not_before,
                    });
                }
            }
            ALLOCATED => {
                if let (Some(i), Some(attempt_id)) = (wake(), text(&payload, "attempt_id")) {
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
                if let (Some(i), Some(attempt_id)) = (wake(), text(&payload, "attempt_id"))
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
                if let Some(i) =
                    text(&payload, "target_wake").and_then(|id| index.get(&id).copied())
                {
                    wakes[i].preempted = true;
                }
            }
            _ => {
                if let Some(i) = wake() {
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
/// again, with `supervisor.rearmed{reason, failures, retry_not_before, role_epoch}` (a failure counted under the epoch
/// of the attempt that ended, not the binding's now (v13, M2T08R-53), and backed off by ADR-020's bound; a preempted
/// or withdrawn turn neither), and its supervision task CANCELLED if no ending moved it yet. The moves and events for
/// the caller's commit.
pub(super) fn rearm(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    wakes: &mut [Wake],
    now: std::time::SystemTime,
) -> Result<(Vec<StateCas>, Vec<NewEvent>), PersistError> {
    let mut cas = Vec::new();
    let mut events = Vec::new();
    for wake in wakes.iter_mut().filter(|w| !w.superseded && !w.settled()) {
        let Some(epoch) = wake.turns.last().map(|t| t.role_epoch) else {
            continue;
        };
        let failures = wake.failures(epoch);
        let Some(turn) = wake.turns.last_mut() else {
            continue;
        };
        if !turn.ended() || turn.rearm.is_some() {
            continue;
        }
        // By their event ids, `<attempt>/preempt` and `<attempt>/turn_limit` (v18: no JSON comparison per item).
        let preempted = has_event(tx, &child_id(&turn.attempt_id, "preempt"))?;
        let overdue = has_event(tx, &child_id(&turn.attempt_id, "turn_limit"))?;
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

/// What a supervision turn's executor gives the Commander (decisions 7 and 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supervision {
    /// The wake it answers: its messages name it as their causation.
    pub wake: String,
    /// Its bridge's nonce.
    pub nonce: String,
    /// The run's revision and the Commander's epoch its decisions are to name.
    pub run_revision: i64,
    pub role_epoch: i64,
    /// What it is shown (bounded, deterministic).
    pub summary: Value,
    /// Its session's own working directory, recorded at its first turn (decision 3); none: the project's.
    pub workdir: Option<String>,
}

/// How many of its wake's events a turn is shown, and how much of each payload; how many of its tasks and of the run's
/// other owed wakes (decision 7: bounded, v13 M2T08R-57; the snapshots are never cut).
const SHOWN_EVENTS: usize = 50;
const SHOWN_PAYLOAD: usize = 2_000;
const SHOWN_TASKS: usize = 50;
const SHOWN_WAKES: usize = 50;

/// How urgent an event of a wake is, by its kind and its payload's `severity`, most first: integrity, critical, high,
/// the rest (decision 7's order).
fn urgency(kind: &str, severity: Option<&str>) -> u8 {
    match (kind, severity) {
        ("integrity_violation", _) => 0,
        ("issue.opened", Some("critical")) => 1,
        ("issue.opened", Some("high"))
        | (
            "need_decision"
            | "no_progress"
            | "budget.risk"
            | "team.binding_blocked"
            | "supervisor.retrigger",
            _,
        ) => 2,
        _ => 3,
    }
}

/// The input of supervision turn `attempt_id` of run `run_id`, from its allocation (decisions 7 and 8): none for an
/// attempt that is no supervision turn.
pub(super) fn supervision_input(
    conn: &Connection,
    run_id: &str,
    attempt_id: &str,
) -> Result<Option<Supervision>, PersistError> {
    // By its event id, `<turn task>/allocated` (v18).
    let task: Option<String> = conn
        .query_row(
            "SELECT a.task_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE a.attempt_id = ?1 AND t.run_id = ?2",
            params![attempt_id, run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let allocation: Option<String> = match task {
        Some(task) => conn
            .query_row(
                "SELECT payload_json FROM events WHERE event_id = ?1 AND kind = ?2",
                params![child_id(&task, "allocated"), ALLOCATED],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?,
        None => None,
    };
    let Some(allocation) = allocation else {
        return Ok(None);
    };
    let allocation: Value = serde_json::from_str(&allocation).map_err(|e| {
        PersistError::IntegrityViolation(format!("allocation of {attempt_id}: {e}"))
    })?;
    let nonce: Option<String> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.nonce') FROM outbox WHERE message_id = ?1",
            params![child_id(attempt_id, "dispatch")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let wake_id = allocation["wake"].as_str().unwrap_or_default().to_owned();
    let wakes = of_run(conn, run_id)?;
    let Some(this) = wakes.iter().find(|w| w.id == wake_id) else {
        return Ok(None);
    };
    // Its events, most urgent first, then the newest: ordered by kind and severity read in one statement, and only the
    // shown ones' payloads read (linear in its events, v14 M2T08R-101).
    let mut order: Vec<(u8, i64)> = {
        let mut statement = conn
            .prepare(
                "SELECT seq, kind, json_extract(payload_json, '$.severity') FROM events \
                 WHERE seq IN (SELECT value FROM json_each(?1))",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(i64, String, Option<String>)> = statement
            .query_map(params![json!(this.event_seqs).to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        rows.into_iter()
            .map(|(seq, kind, severity)| (urgency(&kind, severity.as_deref()), -seq))
            .collect()
    };
    order.sort_unstable();
    let total = order.len();
    let mut shown: Vec<Value> = Vec::new();
    for (_, seq) in order.into_iter().take(SHOWN_EVENTS) {
        let (kind, payload): (String, String) = conn
            .query_row(
                "SELECT kind, payload_json FROM events WHERE seq = ?1",
                params![-seq],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite_error)?;
        let payload = if payload.len() > SHOWN_PAYLOAD {
            json!({"truncated": true, "head": payload.chars().take(SHOWN_PAYLOAD).collect::<String>()})
        } else {
            serde_json::from_str(&payload).unwrap_or(Value::Null)
        };
        shown.push(json!({"seq": -seq, "kind": kind, "payload": payload}));
    }
    let (state, spec_id, spec_sha256): (String, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT r.state, r.spec_id, s.sha256 FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id \
             WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?;
    let candidate = super::outbox::current_candidate(conn, run_id)?.map(|c| c.id);
    let mut tasks = Vec::new();
    for task in this.task_ids.iter().take(SHOWN_TASKS) {
        let row: Option<(String, i64, String, Option<String>)> = conn
            .query_row(
                "SELECT state, revision, role, blocked_reason FROM tasks WHERE task_id = ?1",
                params![task],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some((state, revision, role, blocked)) = row {
            tasks.push(json!({"task_id": task, "state": state, "revision": revision, "role": role, "blocked_reason": blocked}));
        }
    }
    let others: Vec<&Wake> = wakes
        .iter()
        .filter(|w| w.id != wake_id && w.owed())
        .collect();
    let other: Vec<Value> = others
        .iter()
        .take(SHOWN_WAKES)
        .map(|w| json!({"id": w.id, "fingerprint": w.fingerprint, "severity": w.severity.as_str()}))
        .collect();
    let mut roles = Vec::new();
    {
        let mut statement = conn
            .prepare(
                "SELECT role, agent, state, role_epoch, blocked_reason FROM role_bindings WHERE run_id = ?1 ORDER BY role",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(String, String, String, i64, Option<String>)> = statement
            .query_map(params![run_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        for (role, agent, state, epoch, blocked) in rows {
            let pending = super::handover::first_input_pending(conn, run_id, &role, epoch)?;
            roles.push(
                json!({"role": role, "agent": agent, "state": state, "role_epoch": epoch,
                "blocked_reason": blocked, "first_input_pending": pending}),
            );
        }
    }
    // The latest ten, from two index range scans newest first (v18: no scan of the run's whole history).
    let rejections: Vec<Value> = {
        // The latest ten, newest first: the rejections by their own index range, the invalid ends only after the
        // horizon (v18, v20 M2T08R-403: no scan of the run's whole history).
        let read =
            |sql: &str, from: Option<i64>| -> Result<Vec<(i64, String, String)>, PersistError> {
                let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
                let map = |r: &rusqlite::Row<'_>| Ok((r.get(0)?, r.get(1)?, r.get(2)?));
                let rows = match from {
                    Some(from) => statement.query_map(params![run_id, from], map),
                    None => statement.query_map(params![run_id], map),
                }
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?;
                Ok(rows)
            };
        let mut rows = read(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind = 'decision.rejected' \
             ORDER BY seq DESC LIMIT 10",
            None,
        )?;
        rows.extend(read(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind = 'supervisor.output_disposed' \
             AND json_extract(payload_json, '$.outcome') = 'invalid' AND seq >= ?2 ORDER BY seq DESC LIMIT 10",
            Some(horizon(conn, run_id)?),
        )?);
        rows.sort_by_key(|(seq, _, _)| std::cmp::Reverse(*seq));
        rows.truncate(10);
        rows.into_iter()
            .map(|(_, kind, payload)| (kind, payload))
            .map(|(kind, payload)| {
                let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                json!({"kind": kind, "decision_id": p["decision_id"], "output_id": p["output_id"],
                    "reason": p.get("reason_code").or_else(|| p.get("reason")).cloned().unwrap_or(Value::Null),
                    "detail": p["detail"]})
            })
            .collect()
    };
    let run_revision = allocation["run_revision"].as_i64().unwrap_or(-1);
    let role_epoch = allocation["role_epoch"].as_i64().unwrap_or(-1);
    // Its session's own directory, as its first turn recorded it (decision 3, -1057).
    let workdir: Option<String> = conn
        .query_row(
            "SELECT json_extract(e.payload_json, '$.path') FROM events e JOIN attempts a ON a.attempt_id = ?2 \
             WHERE e.run_id = ?1 AND e.kind = ?3 AND json_extract(e.payload_json, '$.session_id') = a.session_id \
             ORDER BY e.seq LIMIT 1",
            params![run_id, attempt_id, WORKDIR],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    Ok(Some(Supervision {
        workdir,
        wake: wake_id.clone(),
        nonce: nonce.unwrap_or_default(),
        run_revision,
        role_epoch,
        summary: json!({
            "wake": {"id": wake_id, "fingerprint": this.fingerprint, "severity": this.severity.as_str(),
                "spec_id": this.spec_id, "woken_at": this.woken_at},
            "turn": {"attempt_id": attempt_id, "generation": allocation["generation"],
                "run_revision": run_revision, "role_epoch": role_epoch},
            "run": {"run_id": run_id, "state": state, "spec_id": spec_id, "spec_sha256": spec_sha256,
                "candidate_id": candidate},
            "snapshots": allocation["snapshots"],
            "tasks": tasks,
            "tasks_total": this.task_ids.len(),
            "events": shown,
            "events_total": total,
            "other_wakes": other,
            "other_wakes_total": others.len(),
            "roles": roles,
            "recent_rejections": rejections,
        }),
    }))
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
    // Each run's latest allocation, by one index lookup per run (v20, M2T08R-451: no aggregation over all of them).
    let mut last: BTreeMap<String, i64> = BTreeMap::new();
    for (run_id, _, _) in &runs {
        if let Some((seq, _)) = latest_allocation(&tx, run_id)? {
            last.insert(run_id.clone(), seq);
        }
    }
    let mut candidates = Vec::new();
    let mut rearmed = Commit::default();
    for (run_id, spec_id, run_revision) in runs {
        let Some(binding) = decide::load_binding(&tx, &run_id, COMMANDER)? else {
            continue;
        };
        let mut wakes = of_run(&tx, &run_id)?;
        let epoch = decide::to_i64(binding.role_epoch, "role_epoch")?;
        let (moves, events) = rearm(&tx, project_id, &run_id, &mut wakes, now)?;
        // Preempts whose target has ended name the wake that takes the slot next, while it is still to be allocated
        // (decision 3, -800, -850): this run's, after its horizon, by the wake they name (v20, M2T08R-402).
        let named: BTreeMap<String, i64> = {
            let mut statement = tx
                .prepare(
                    "SELECT json_extract(e.payload_json, '$.wake'), max(e.seq) FROM events e JOIN attempts a \
                     ON a.attempt_id = json_extract(e.payload_json, '$.target_attempt') \
                     WHERE e.run_id = ?1 AND e.kind = ?2 AND e.seq >= ?3 \
                     AND a.state IN ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT') \
                     GROUP BY json_extract(e.payload_json, '$.wake')",
                )
                .map_err(sqlite_error)?;
            let rows: Vec<(Option<String>, i64)> = statement
                .query_map(params![run_id, PREEMPT, horizon(&tx, &run_id)?], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?;
            rows.into_iter()
                .filter_map(|(wake, seq)| Some((wake?, seq)))
                .collect()
        };
        rearmed.cas.extend(moves);
        rearmed.events.extend(events);
        if !binding.active {
            // Not active yet: an owed wake waits, said once (decision 3, "not active yet").
            if wakes.iter().any(Wake::owed)
                && let Some(reason) = inactive_reason(&tx, &run_id)?
            {
                rearmed.events.extend(defer_once(
                    &tx, project_id, &run_id, reason, epoch, &wakes, now,
                )?);
            }
            continue;
        }
        if super::retry::budget_stop(&tx, &run_id, context)?.is_some() {
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
        let (reasons, _) = unavailability(&tx, &run_id, &wakes, epoch, &state, context)?;
        // Only on the verdict the step recorded, and only when this view agrees with it: available both, an ordinary
        // turn; unavailable both, a probe; else nothing until the next step records the change (v17, M2T08R-251).
        let recorded_unavailable = recorded(&tx, &run_id)?.is_some_and(|r| r.unavailable);
        if reasons.is_empty() && recorded_unavailable {
            continue;
        }
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
            .filter(|w| w.retry_not_before().is_none_or(|t| t <= now))
            .filter(|w| {
                probing
                    .as_ref()
                    .is_none_or(|target| target.as_ref().is_none_or(|id| *id == w.id))
            })
        {
            let aged = parse_rfc3339_millis(&wake.woken_at)
                .and_then(|at| now.duration_since(at).ok())
                .is_some_and(|waited| waited > SUPERVISION_TURN);
            let first = named
                .get(&wake.id)
                .is_some_and(|seq| wake.turns.last().is_none_or(|t| t.allocated_seq < *seq));
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
    // No slot: with reader slots, work holds them all; each run waiting for one is told once (decision 3).
    let Some(kind) = kind else {
        if !context.reserve_supervisor_slot() {
            let mut told = BTreeSet::new();
            for c in &candidates {
                if told.insert(c.run_id.clone()) {
                    let wakes = of_run(&tx, &c.run_id)?;
                    rearmed.events.extend(defer_once(
                        &tx,
                        project_id,
                        &c.run_id,
                        "NO_SLOT",
                        decide::to_i64(c.role_epoch, "role_epoch")?,
                        &wakes,
                        now,
                    )?);
                }
            }
        }
        if !rearmed.events.is_empty() {
            writer::write(&tx, &rearmed, ctx)?;
            ctx.before_commit()?;
            tx.commit().map_err(sqlite_error)?;
        }
        return Ok(None);
    };
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
    // The run's allocations so far: its latest's generation and one (v20, M2T08R-451: not a count over them all).
    let generation: i64 = latest_allocation(&tx, &chosen.run_id)?.map_or(0, |(_, g)| g + 1);
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
    // The turn's nonce: in its dispatch notice, which a delivery after a restart gives again; the allocation keeps
    // only its sha256 (decision 8, -266, -455).
    let nonce = nonce()?;
    // The Commander session's own directory, outside every project, recorded at its first turn (decision 3, -551,
    // -955, -1057): <base>/<project digest>/<session>, the same for its later turns whatever the environment then.
    if let Some(base) = &context.commander_dir {
        let session: Option<String> = tx
            .query_row(
                "SELECT session_id FROM attempts WHERE attempt_id = ?1",
                params![attempt_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if let Some(session) = session {
            let recorded: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = ?2 \
                     AND json_extract(payload_json, '$.session_id') = ?3)",
                    params![chosen.run_id, WORKDIR, session],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if !recorded {
                let digest = sha256_hex(chosen.project_id.as_bytes());
                let path = base
                    .join(&digest[..16])
                    .join(session.replace('%', "%25").replace('/', "%2F"));
                rearmed.events.push(NewEvent {
                    project_id: chosen.project_id.clone(),
                    event_id: child_id(&task_id, "workdir"),
                    run_id: Some(chosen.run_id.clone()),
                    kind: WORKDIR.to_owned(),
                    payload: json!({"session_id": session, "path": path.display().to_string()}),
                });
            }
        }
    }
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
                "nonce_sha256": sha256_hex(nonce.as_bytes()),
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
                "nonce": nonce,
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
/// (decision 3, the preempt's condition): its Commander available by the step's `verdict` (v13, M2T08R-08), its
/// binding ACTIVE with a session, its budget not stopping it, the backoff over. The run's state is the caller's (it
/// fires only in an admitting state).
pub(super) fn could_allocate(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
    not_before: Option<std::time::SystemTime>,
    verdict: &Verdict,
) -> Result<bool, PersistError> {
    if !verdict.available() {
        return Ok(false);
    }
    let bound: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM role_bindings WHERE run_id = ?1 AND role = ?2 AND state = 'ACTIVE' \
             AND session_id IS NOT NULL)",
            params![run_id, COMMANDER],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(bound
        && super::retry::budget_stop(tx, run_id, context)?.is_none()
        && not_before.is_none_or(|t| t <= context.now()))
}

/// Whether the registry's looks keep run `run_id`'s Commander unavailable (decision 5, -306, -355): its last look at
/// the Commander's agent found it unavailable (`agent_unavailable`) or not logged in (`login_missing`); once so recorded,
/// it stays so until a look of a later identity `(host_epoch, n)` finds it available and logged in (a host that has not
/// looked yet, or the same look again, leaves it). The reason, and the identity of the look it stands on.
fn registry_reason(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
) -> Result<(Option<&'static str>, Value), PersistError> {
    let agent: Option<String> = tx
        .query_row(
            "SELECT agent FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, COMMANDER],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let look = agent
        .as_deref()
        .and_then(|a| context.observations.as_ref().and_then(|o| o.get(a)))
        .copied();
    // A recorded registry fault is the fact of the agent it names: it applies only while that agent holds the role
    // (looks are counted per agent; v16, M2T08R-200).
    let last = recorded(tx, run_id)?.filter(|r| {
        r.unavailable
            && r.observation["agent"].as_str().is_some()
            && r.observation["agent"].as_str() == agent.as_deref()
    });
    let before = last.as_ref().and_then(|r| {
        let reasons = r.reasons.as_array()?;
        let reason = reasons.iter().find_map(|x| match x.as_str() {
            Some("agent_unavailable") => Some("agent_unavailable"),
            Some("login_missing") => Some("login_missing"),
            _ => None,
        })?;
        Some((reason, r.observation.clone()))
    });
    let identity = |o: &crate::decision::Observation| json!({"agent": agent, "host_epoch": o.host_epoch, "n": o.n});
    Ok(match (look, before) {
        (Some(o), _) if !o.available => (Some("agent_unavailable"), identity(&o)),
        (Some(o), _) if !o.logged_in => (Some("login_missing"), identity(&o)),
        (Some(o), Some((reason, seen))) => {
            let later = (o.host_epoch, o.n)
                > (
                    seen["host_epoch"].as_i64().unwrap_or(i64::MIN),
                    seen["n"].as_u64().unwrap_or(0),
                );
            if later {
                (None, identity(&o))
            } else {
                (Some(reason), seen)
            }
        }
        (Some(o), None) => (None, identity(&o)),
        (None, Some((reason, seen))) => (Some(reason), seen),
        (None, None) => (None, Value::Null),
    })
}

/// Why run `run_id`'s Commander, at role epoch `epoch` and binding state `binding`, is unavailable now (empty:
/// available; decision 5): its binding BLOCKED (`binding_blocked`); the registry's looks; inactive past its limit; a
/// wake not answered (owed, or its probe in flight: v13, M2T08R-01) with MAX_RETRIES failures under the epoch
/// (`failures_capped`); its last turn of the epoch that ended failed for authentication or capability (`auth`,
/// `capability`). Every judgement of a step and of the allocation goes through it.
fn unavailability(
    tx: &Transaction<'_>,
    run_id: &str,
    wakes: &[Wake],
    epoch: i64,
    binding: &str,
    context: &Context,
) -> Result<(Vec<&'static str>, Value), PersistError> {
    let mut reasons = Vec::new();
    if binding == "BLOCKED" {
        reasons.push("binding_blocked");
    }
    let (registry, observation) = registry_reason(tx, run_id, context)?;
    reasons.extend(registry);
    if inactive_too_long(tx, run_id, context.now())? {
        reasons.push("inactive");
    }
    let cap = i64::from(crate::retry::MAX_RETRIES);
    if wakes
        .iter()
        .any(|w| !w.superseded && !w.settled() && w.failures(epoch) >= cap)
    {
        reasons.push("failures_capped");
    }
    // Auth or Capability: a turn of the epoch failed so and no turn of the epoch settled after it, in the turns'
    // order; a turn that failed otherwise, was withdrawn or preempted recovers nothing (v14, M2T08R-100).
    let classes: BTreeMap<String, String> = {
        let mut statement = tx
            .prepare(
                "SELECT json_extract(payload_json, '$.attempt_id'), json_extract(payload_json, '$.failure.class') \
                 FROM events WHERE run_id = ?1 AND kind = 'supervisor.turn_failed' AND seq >= ?2",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(Option<String>, Option<String>)> = statement
            .query_map(params![run_id, horizon(tx, run_id)?], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        rows.into_iter()
            .filter_map(|(attempt, class)| Some((attempt?, class?)))
            .collect()
    };
    let mut ended: Vec<&Turn> = wakes
        .iter()
        .flat_map(|w| w.turns.iter())
        .filter(|t| t.role_epoch == epoch && t.ended())
        .collect();
    ended.sort_by_key(|t| t.allocated_seq);
    let mut held: BTreeSet<&'static str> = BTreeSet::new();
    for turn in ended {
        if turn.state == Some(AttemptState::Settled) {
            held.clear();
            continue;
        }
        match classes.get(&turn.attempt_id).map(String::as_str) {
            Some("auth") => {
                held.insert("auth");
            }
            Some("capability") => {
                held.insert("capability");
            }
            _ => {}
        }
    }
    reasons.extend(
        ["auth", "capability"]
            .into_iter()
            .filter(|r| held.contains(r)),
    );
    Ok((reasons, observation))
}

/// The run's last `supervisor.unavailable` or `supervisor.available`.
struct Recorded {
    unavailable: bool,
    reasons: Value,
    /// The registry look its reasons stand on (null: none).
    observation: Value,
    /// Its time (the supervisor's clock) and seq.
    at: String,
    seq: i64,
}

fn recorded(tx: &Connection, run_id: &str) -> Result<Option<Recorded>, PersistError> {
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
            observation: payload["observation"].clone(),
            at: text(&payload, "at").unwrap_or_default(),
            seq,
        })
    })
    .transpose()
}

/// A run's Commander as one step judges it (docs/adr/ADR-025.md v13 decision 5): why it is unavailable (empty:
/// available) and the registry look that stands behind it. Firing, the preempt's condition and the claims of the step
/// all go by it.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Verdict {
    pub reasons: Vec<&'static str>,
    pub observation: Value,
}

impl Verdict {
    pub fn available(&self) -> bool {
        self.reasons.is_empty()
    }
}

/// Run `run_id`'s verdict at `context.now()`: none for a run with no Commander binding (not supervised).
pub(super) fn verdict(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
) -> Result<Option<Verdict>, PersistError> {
    let binding: Option<(String, i64)> = tx
        .query_row(
            "SELECT state, role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, COMMANDER],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((binding, epoch)) = binding else {
        return Ok(None);
    };
    let wakes = of_run(tx, run_id)?;
    let (reasons, observation) = unavailability(tx, run_id, &wakes, epoch, &binding, context)?;
    Ok(Some(Verdict {
        reasons,
        observation,
    }))
}

/// Judge run `run_id`'s Commander by the step's `verdict` at `context.now()` (decision 5), in the supervisor's step:
/// unavailable, it records `supervisor.unavailable{reasons}` when that changes and, while the run owes it a decision (a
/// wake not answered, owed or in flight (v13), or a PENDING trigger), claims COMMANDER_UNAVAILABLE (and NEED_DECISION
/// when a need_decision is owed) for the run, which is BLOCKED by it, its routine (normal) triggers superseded;
/// available again, `supervisor.available` and the claims released (the run back in its state once nothing else is
/// claimed). Only in a state that admits supervision.
pub(super) fn judge(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    verdict: &Verdict,
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
    let Some((_binding, epoch, state, _resume)) = row else {
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
    let (reasons, observation) = (&verdict.reasons, &verdict.observation);
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
        // Rewritten when the reasons or their subject change: a registry fault names its agent, so a fault of another
        // agent under the same reason's name is a new record (v18, M2T08R-300).
        let registry = reasons
            .iter()
            .any(|r| matches!(*r, "agent_unavailable" | "login_missing"));
        if last.as_ref().is_none_or(|r| {
            !r.unavailable
                || r.reasons != json!(reasons)
                || (registry && r.observation["agent"] != observation["agent"])
        }) {
            commit.events.push(event(
                child_id(run_id, &format!("supervisor/{number}")),
                UNAVAILABLE,
                json!({"reasons": reasons, "observation": observation, "at": at, "role_epoch": epoch}),
            ));
        }
        // What the run owes its Commander, blocking (v15): its owed wakes; a wake whose last turn is a probe in flight
        // or settled with outputs still to dispose of (M2T08R-01, -50, -103); and its PENDING triggers. An ordinary
        // turn, allocated while the Commander was available, is answering its wake and blocks nothing (M2T08R-150).
        let waiting = super::outputs::pending_attempts(tx, run_id)?;
        let probes = probe_attempts(tx, run_id)?;
        let mut seqs: Vec<i64> = Vec::new();
        for wake in wakes.iter().filter(|w| {
            w.owed()
                || (!w.superseded
                    && w.turns.last().is_some_and(|t| {
                        probes.contains(&t.attempt_id)
                            && (!t.ended()
                                || (t.state == Some(AttemptState::Settled)
                                    && waiting.contains(&t.attempt_id)))
                    }))
        }) {
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
    if !desired.is_empty() && !had {
        // One per blocking episode, numbered by the run's earlier ones (v13, M2T08R-51).
        let episode: i64 = tx
            .query_row(
                "SELECT count(*) FROM events WHERE run_id = ?1 AND kind = 'supervisor.blocked'",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        commit.events.push(event(
            child_id(run_id, &format!("supervisor/blocked/{episode}")),
            "supervisor.blocked",
            json!({"reasons": reasons, "owed": owed_records}),
        ));
    }
    let moved = super::blockers::follow(
        tx,
        project_id,
        run_id,
        changed,
        &union,
        &at,
        "the commander is unavailable",
        ctx,
    )?;
    commit.cas.extend(moved.cas);
    commit.events.extend(moved.events);
    Ok(commit)
}

/// Run `run_id`'s latest `supervisor.allocated`: its seq and generation (one index lookup; v20).
fn latest_allocation(conn: &Connection, run_id: &str) -> Result<Option<(i64, i64)>, PersistError> {
    conn.query_row(
        "SELECT seq, coalesce(json_extract(payload_json, '$.generation'), 0) FROM events \
         WHERE run_id = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT 1",
        params![run_id, ALLOCATED],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sqlite_error)
}

/// Whether event `event_id` exists (the unique index; v18).
fn has_event(tx: &Connection, event_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// Whether the verdict the step last recorded for run `run_id` is unavailable (v19).
pub(super) fn recorded_unavailable(conn: &Connection, run_id: &str) -> Result<bool, PersistError> {
    Ok(recorded(conn, run_id)?.is_some_and(|r| r.unavailable))
}

/// Run `run_id`'s horizon: the seq of its latest `supervisor.horizon` (0: none). Every wake woken before it is closed
/// for good (docs/adr/ADR-025.md v19 decision 2, M2T08R-352).
pub(super) fn horizon(conn: &Connection, run_id: &str) -> Result<i64, PersistError> {
    conn.query_row(
        "SELECT coalesce((SELECT json_extract(payload_json, '$.seq') FROM events WHERE run_id = ?1 AND kind = ?2 \
         ORDER BY seq DESC LIMIT 1), 0)",
        params![run_id, HORIZON],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The horizon run `run_id` may move to now: the seq its earliest open wake was woken at (open: neither superseded
/// nor answered with its turn's outputs disposed of), or, none open, past its last supervision event; the event
/// recording it when it is past the recorded one (v19).
pub(super) fn advance_horizon(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
) -> Result<Option<NewEvent>, PersistError> {
    // The readers table (v20): kept are the open wakes, every wake with an unterminated output on any of its turns
    // (a superseded one too, M2T08R-401), and the wake of the current Commander epoch's last SETTLED turn (the Auth
    // and Capability recovery, the probe's cooldown, M2T08R-400).
    let waiting = super::outputs::pending_attempts(tx, run_id)?;
    let epoch: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, COMMANDER],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let wakes = of_run(tx, run_id)?;
    let last_settled = wakes
        .iter()
        .flat_map(|w| w.turns.iter().map(move |t| (w, t)))
        .filter(|(_, t)| Some(t.role_epoch) == epoch && t.state == Some(AttemptState::Settled))
        .max_by_key(|(_, t)| t.allocated_seq)
        .map(|(w, _)| w.woken_seq);
    let open = wakes
        .iter()
        .filter(|w| {
            let unterminated = w.turns.iter().any(|t| waiting.contains(&t.attempt_id));
            unterminated || (!w.superseded && !w.settled())
        })
        .map(|w| w.woken_seq)
        .chain(last_settled)
        .min();
    let next = match open {
        Some(seq) => seq,
        // Past its last supervision event (not any event: the horizon moves only as supervision does).
        None => tx
            .query_row(
                "SELECT coalesce(max(seq), 0) + 1 FROM events WHERE run_id = ?1 AND kind IN \
                 (?2, ?3, ?4, ?5, ?6, 'supervisor.output', 'supervisor.output_disposed', 'supervisor.turn_failed')",
                params![run_id, WOKEN, ALLOCATED, REARMED, SUPERSEDED, PREEMPT],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?,
    };
    // Recorded only when it passes a wake (none otherwise to leave behind).
    let passed: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = ?2 AND seq >= ?3 AND seq < ?4)",
            params![run_id, WOKEN, horizon(tx, run_id)?, next],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !passed {
        return Ok(None);
    }
    Ok(Some(NewEvent {
        project_id: project_id.to_owned(),
        event_id: child_id(run_id, &format!("supervisor/horizon/{next}")),
        run_id: Some(run_id.to_owned()),
        kind: HORIZON.to_owned(),
        payload: json!({"seq": next}),
    }))
}

/// The attempts of run `run_id` allocated as probes (`supervisor.probe`).
fn probe_attempts(tx: &Transaction<'_>, run_id: &str) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT json_extract(payload_json, '$.attempt_id') FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND seq >= ?3",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<Option<String>> = statement
        .query_map(params![run_id, PROBE, horizon(tx, run_id)?], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows.into_iter().flatten().collect())
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

/// How long a Commander may be without a session, or with its first input undelivered, while a decision is owed before
/// it counts as unavailable (decision 3: longer than a turn's limit and a lease's TTL together).
const DEFER_LIMIT: Duration = Duration::from_secs(30 * 60);

/// Why run `run_id`'s Commander is not active: no session (`NO_SESSION`), its handover's session.start not delivered
/// (`FIRST_INPUT_UNDELIVERED`), a team change waiting for its checkpoint (`CHANGE_PENDING`), or its first input in
/// a turn now (`FIRST_INPUT_IN_FLIGHT`); none for a BLOCKED binding (that is unavailability, decision 5).
fn inactive_reason(conn: &Connection, run_id: &str) -> Result<Option<&'static str>, PersistError> {
    let row: Option<(String, i64, Option<String>)> = conn
        .query_row(
            "SELECT state, role_epoch, session_id FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, COMMANDER],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, epoch, session)) = row else {
        return Ok(None);
    };
    if state != "ACTIVE" {
        return Ok(None);
    }
    Ok(match session {
        None => Some("NO_SESSION"),
        Some(_) if super::handover::first_input_pending(conn, run_id, COMMANDER, epoch)? => {
            Some("FIRST_INPUT_UNDELIVERED")
        }
        Some(_) if super::team::pending(conn, run_id, COMMANDER)? => Some("CHANGE_PENDING"),
        Some(_) => Some("FIRST_INPUT_IN_FLIGHT"),
    })
}

/// One `supervisor.deferred{reason}` for run `run_id`, unless one of that reason was written since its last
/// allocation.
#[allow(clippy::too_many_arguments)]
fn defer_once(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    reason: &str,
    epoch: i64,
    wakes: &[Wake],
    now: std::time::SystemTime,
) -> Result<Option<NewEvent>, PersistError> {
    // Once per reason and Commander epoch since the run's last allocation (v16: a deferral is its holder's fact).
    let (told, n): (bool, i64) = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'supervisor.deferred' \
               AND json_extract(payload_json, '$.reason') = ?2 AND json_extract(payload_json, '$.role_epoch') = ?4 \
               AND seq > coalesce((SELECT max(seq) FROM events WHERE run_id = ?1 AND kind = ?3), 0)), \
             (SELECT count(*) FROM events WHERE run_id = ?1 AND kind = 'supervisor.deferred')",
            params![run_id, reason, ALLOCATED, epoch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    if told {
        return Ok(None);
    }
    let owed: Vec<&str> = wakes
        .iter()
        .filter(|w| w.owed())
        .map(|w| w.id.as_str())
        .collect();
    Ok(Some(NewEvent {
        project_id: project_id.to_owned(),
        event_id: child_id(run_id, &format!("deferred/{n}")),
        run_id: Some(run_id.to_owned()),
        kind: "supervisor.deferred".to_owned(),
        payload: json!({"reason": reason, "role_epoch": epoch, "wakes": owed, "at": rfc3339_utc(now)}),
    }))
}

/// Whether run `run_id`'s Commander has been without a session, or its first input undelivered, longer than
/// [`DEFER_LIMIT`] while a decision is owed: its latest such deferral since its last allocation, the cause still there.
fn inactive_too_long(
    tx: &Transaction<'_>,
    run_id: &str,
    now: std::time::SystemTime,
) -> Result<bool, PersistError> {
    let Some(reason) = inactive_reason(tx, run_id)?
        .filter(|r| matches!(*r, "NO_SESSION" | "FIRST_INPUT_UNDELIVERED"))
    else {
        return Ok(false);
    };
    // Counted from the deferral of the Commander's current epoch, not an earlier holder's (v16).
    let epoch: i64 = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, COMMANDER],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let since: Option<String> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.at') FROM events WHERE run_id = ?1 AND kind = 'supervisor.deferred' \
             AND json_extract(payload_json, '$.reason') = ?2 AND json_extract(payload_json, '$.role_epoch') = ?4 \
             AND seq > coalesce((SELECT max(seq) FROM events WHERE run_id = ?1 AND kind = ?3), 0) \
             ORDER BY seq LIMIT 1",
            params![run_id, reason, ALLOCATED, epoch],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    Ok(since
        .as_deref()
        .and_then(crate::time::parse_rfc3339)
        .is_some_and(|since| since + DEFER_LIMIT < now))
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
    // Only the runs with a supervision task in flight, by the tasks' (run, state) index (v19, M2T08R-351).
    let runs: Vec<String> = {
        let mut statement = tx
            .prepare(
                "SELECT r.run_id FROM runs r WHERE r.project_id = ?1 AND EXISTS (SELECT 1 FROM tasks t \
                 WHERE t.run_id = r.run_id AND t.state IN ('LEASED', 'RUNNING') AND t.role = ?2) \
                 ORDER BY r.created_at, r.run_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![project_id, COMMANDER], |r| r.get(0))
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
            let preempted = has_event(&tx, &child_id(&turn.attempt_id, "preempt"))?;
            let overdue = turn
                .allocated_at
                .as_deref()
                .and_then(crate::time::parse_rfc3339)
                .is_some_and(|at| at + SUPERVISION_TURN < now);
            let reason = if preempted {
                "preempted"
            } else if overdue {
                let recorded = has_event(&tx, &child_id(&turn.attempt_id, "turn_limit"))?;
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
        "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE state = 'PENDING' AND message_id = ?1",
        params![child_id(attempt_id, "dispatch")],
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
