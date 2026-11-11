//! The supervisor's triggers and cursor (REQ-021, docs/adr/ADR-025.md decision 2): one step of a run takes in its
//! events after its cursor, joins each that wakes the Commander to its key's PENDING trigger (one per key, the unique
//! index), moves the cursor past every event it looked at, and fires the triggers due. Times are the host clock's
//! (`Context.now`), to the millisecond; an event's own `created_at` is the writer's wall clock and plays no part.

use super::writer::{self, Commit, Ctx, NewEvent};
use super::{PersistError, sqlite_error};
use crate::decision::Context;
use crate::ids::child_id;
use crate::supervise::{self, Policy, Severity};
use crate::time::{parse_rfc3339_millis, rfc3339_millis_utc};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::time::SystemTime;

/// The event kinds that can wake the Commander under some policy (`supervise::classify`'s): the step reads only these
/// from the run's events after its cursor, and moves the cursor past the rest without reading them.
const KINDS: &[&str] = &[
    "integrity_violation",
    "issue.opened",
    "need_decision",
    "no_progress",
    "budget.risk",
    "team.binding_blocked",
    "team.binding_refused",
    "spec.changed",
    "milestone_ready",
    "release_request",
    "iteration.completed",
    "task.implemented",
];

/// The events this writer committed, by transaction: the transaction's last seq, the seq before it, and when it
/// committed by the store's clock; the latest [`KEPT`] transactions (docs/adr/ADR-025.md decision 2).
#[derive(Debug, Default)]
pub(super) struct CommitTimes(std::collections::BTreeMap<i64, (i64, SystemTime)>);

/// How many transactions' commit instants a writer keeps.
const KEPT: usize = 100_000;

impl CommitTimes {
    /// A transaction committed the events after `before` up to `last`, at `at`.
    pub(super) fn record(&mut self, before: i64, last: i64, at: SystemTime) {
        self.0.insert(last, (before, at));
        while self.0.len() > KEPT {
            self.0.pop_first();
        }
    }

    /// When event `seq` committed, if this writer committed it (not an earlier host's, not forgotten).
    pub(super) fn of(&self, seq: i64) -> Option<SystemTime> {
        self.0
            .range(seq..)
            .next()
            .filter(|(_, (before, _))| *before < seq)
            .map(|(_, (_, at))| *at)
    }
}

/// A trigger fired by a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fired {
    pub trigger_id: String,
    pub fingerprint: String,
    pub spec_id: Option<String>,
    pub severity: Severity,
    pub event_seqs: Vec<i64>,
}

/// What one step of the supervisor did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Supervised {
    /// The cursor after the step: every event of the run up to it was taken in or found not to wake.
    pub cursor: i64,
    pub fired: Vec<Fired>,
}

/// One step of run `run_id`'s supervisor at `context.now()`, in one transaction.
pub(super) fn step(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Supervised, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let run: Option<(String, Option<i64>)> = tx
        .query_row(
            "SELECT r.project_id, s.revision FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, revision)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    let policy = Policy::Balanced;
    let now = context.now();
    let cursor: i64 = tx
        .query_row(
            "SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or(0);
    let high: i64 = tx
        .query_row(
            "SELECT coalesce(max(seq), ?2) FROM events WHERE run_id = ?1 AND seq > ?2",
            params![run_id, cursor],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let kinds = KINDS
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(",");
    let waking: Vec<(i64, String, String)> = {
        let mut statement = tx
            .prepare(&format!(
                "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND seq > ?2 AND seq <= ?3 \
                 AND kind IN ({kinds}) ORDER BY seq"
            ))
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id, cursor, high], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for (seq, kind, payload) in &waking {
        let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        if let Some(wake) = supervise::classify(policy, kind, &payload, revision) {
            let spec_id = match wake.key.spec_revision {
                Some(revision) => tx
                    .query_row(
                        "SELECT spec_id FROM specs WHERE project_id = ?1 AND revision = ?2",
                        params![project_id, revision],
                        |r| r.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?,
                None => None,
            };
            // Timed from its commit (REQ-021 decision 4); an event an earlier writer committed, from now.
            take_in(
                &tx,
                run_id,
                &wake.key.fingerprint,
                spec_id.as_deref(),
                wake.severity,
                *seq,
                ctx.committed_at(*seq).unwrap_or(now),
            )?;
        }
    }
    if high > cursor {
        tx.execute(
            "INSERT INTO supervisor_cursor(run_id, last_handled_seq) VALUES (?1, ?2) \
             ON CONFLICT(run_id) DO UPDATE SET last_handled_seq = max(last_handled_seq, excluded.last_handled_seq)",
            params![run_id, high],
        )
        .map_err(sqlite_error)?;
    }
    let (fired, events) = fire(&tx, &project_id, run_id, now, ctx)?;
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
    Ok(Supervised {
        cursor: high,
        fired,
    })
}

/// Event `seq` of key (`fingerprint`, `spec_id`), committed at `at`: joined to the key's PENDING trigger, else a new
/// one (REQ-021 decision 3); its due time again by decision 1, from its first and last events' commits.
fn take_in(
    tx: &Transaction<'_>,
    run_id: &str,
    fingerprint: &str,
    spec_id: Option<&str>,
    severity: Severity,
    seq: i64,
    at: SystemTime,
) -> Result<(), PersistError> {
    let pending: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT trigger_id, severity, event_seqs_json, created_at FROM supervisor_triggers \
             WHERE run_id = ?1 AND fingerprint = ?2 AND ifnull(spec_id, '') = ifnull(?3, '') AND state = 'PENDING'",
            params![run_id, fingerprint, spec_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match pending {
        Some((trigger_id, was, seqs, created_at)) => {
            let first = parse_rfc3339_millis(&created_at).ok_or_else(|| {
                PersistError::IntegrityViolation(format!(
                    "trigger {trigger_id}: created_at {created_at}"
                ))
            })?;
            let severity = Severity::named(&was)
                .unwrap_or(Severity::Normal)
                .max(severity);
            let mut seqs: Vec<i64> = serde_json::from_str(&seqs).map_err(|e| {
                PersistError::IntegrityViolation(format!(
                    "trigger {trigger_id}: event_seqs_json: {e}"
                ))
            })?;
            seqs.push(seq);
            tx.execute(
                "UPDATE supervisor_triggers SET severity = ?2, event_seqs_json = ?3, last_seq = ?4, due_at = ?5 \
                 WHERE trigger_id = ?1",
                params![
                    trigger_id,
                    severity.as_str(),
                    json!(seqs).to_string(),
                    seq,
                    rfc3339_millis_utc(supervise::due_at(severity, first, at))
                ],
            )
            .map_err(sqlite_error)?;
        }
        None => {
            tx.execute(
                "INSERT INTO supervisor_triggers(trigger_id, run_id, fingerprint, spec_id, severity, event_seqs_json, \
                 first_seq, last_seq, due_at, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, 'PENDING', ?9)",
                params![
                    child_id(run_id, &format!("trigger/{seq}")),
                    run_id,
                    fingerprint,
                    spec_id,
                    severity.as_str(),
                    json!([seq]).to_string(),
                    seq,
                    rfc3339_millis_utc(supervise::due_at(severity, at, at)),
                    rfc3339_millis_utc(at)
                ],
            )
            .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

/// The run's PENDING triggers due at `now`, HANDLED, each with its `supervisor.woken` event: its key, events, first
/// event time, wake time and latency (REQ-021 behaviour 7), on the store's clock from its first event's commit; one
/// an earlier writer committed is timed from when it was taken in, `latency_basis` saying so.
fn fire(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    now: SystemTime,
    ctx: &Ctx<'_>,
) -> Result<(Vec<Fired>, Vec<NewEvent>), PersistError> {
    type Due = (String, String, Option<String>, String, String, String, i64);
    let due: Vec<Due> = {
        let mut statement = tx
            .prepare(
                "SELECT trigger_id, fingerprint, spec_id, severity, event_seqs_json, created_at, first_seq \
                 FROM supervisor_triggers WHERE run_id = ?1 AND state = 'PENDING' AND due_at <= ?2 \
                 ORDER BY due_at, first_seq",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id, rfc3339_millis_utc(now)], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let mut fired = Vec::new();
    let mut events = Vec::new();
    for (trigger_id, fingerprint, spec_id, severity, seqs, created_at, first_seq) in due {
        let event_seqs: Vec<i64> = serde_json::from_str(&seqs).map_err(|e| {
            PersistError::IntegrityViolation(format!("trigger {trigger_id}: event_seqs_json: {e}"))
        })?;
        let first = parse_rfc3339_millis(&created_at).ok_or_else(|| {
            PersistError::IntegrityViolation(format!(
                "trigger {trigger_id}: created_at {created_at}"
            ))
        })?;
        let basis = if ctx.committed_at(first_seq).is_some() {
            "commit"
        } else {
            "across_restart"
        };
        let latency = now
            .duration_since(first)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        tx.execute(
            "UPDATE supervisor_triggers SET state = 'HANDLED' WHERE trigger_id = ?1 AND state = 'PENDING'",
            params![trigger_id],
        )
        .map_err(sqlite_error)?;
        events.push(NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(&trigger_id, "woken"),
            run_id: Some(run_id.to_owned()),
            kind: "supervisor.woken".to_owned(),
            payload: json!({
                "trigger_id": trigger_id,
                "dedup_key": {"run_id": run_id, "fingerprint": fingerprint, "spec_id": spec_id},
                "severity": severity,
                "event_seqs": event_seqs,
                "first_event_at": created_at,
                "woken_at": rfc3339_millis_utc(now),
                "wake_latency_ms": latency,
                "latency_basis": basis,
            }),
        });
        fired.push(Fired {
            trigger_id,
            fingerprint,
            spec_id,
            severity: Severity::named(&severity).unwrap_or(Severity::Normal),
            event_seqs,
        });
    }
    Ok((fired, events))
}

#[cfg(test)]
mod tests;
