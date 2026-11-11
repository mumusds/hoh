//! The supervisor's triggers and cursor (REQ-021, docs/adr/ADR-025.md decision 2): one step of a run takes in its
//! events after its cursor, joins each that wakes the Commander to its key's PENDING trigger (one per key, the unique
//! index), moves the cursor past every event it looked at, and fires the triggers due. Times are the host clock's
//! (`Context.now`), to the millisecond; an event's own `created_at` is the writer's wall clock and plays no part.

use super::writer::{self, Commit, Ctx, NewEvent};
use super::{PersistError, sqlite_error, wakes};
use crate::decision::Context;
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::RunState;
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
    "supervisor.retrigger",
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

/// How many of a run's events one batch of a step looks at at most, in one transaction and one writer job; the step
/// goes on from the batch's cursor, a job each, until it has caught up (docs/adr/ADR-025.md decision 2, v13).
const STEP: i64 = 10_000;

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

/// One batch of run `run_id`'s supervisor step at `context.now()`, one writer job: at most [`STEP`] of its events
/// after its cursor taken in, in one transaction. None when that batch has not caught up with what is committed: the
/// caller submits the next batch as another job, so the writer's priority lane (a cancel, a pause) waits one batch at
/// most (v13, M2T08R-03). Once caught up, the due triggers are fired by the step's one verdict and the Commander
/// judged, in that batch's transaction, so no trigger fires on part of what was committed before the step (M2T08D-900,
/// docs/adr/ADR-025.md decision 2); then the outputs of its turns and the close of an ended run.
pub(super) fn step(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Option<Supervised>, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (project_id, high, caught_up) = take_in_batch(&tx, run_id, context, ctx)?;
    if !caught_up {
        ctx.before_commit()?;
        tx.commit().map_err(sqlite_error)?;
        return Ok(None);
    }
    let (fired, events, verdict) = fire(&tx, &project_id, run_id, context, ctx)?;
    writer::write(
        &tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    // Its Commander judged by the same verdict, with what this step woke counted (decision 5); none for a run the
    // step does not judge (no binding, no spec, a state that holds or supersedes).
    if let Some(verdict) = verdict {
        let judged = wakes::judge(&tx, &project_id, run_id, &verdict, context, ctx)?;
        if !judged.cas.is_empty() || !judged.events.is_empty() {
            writer::write(&tx, &judged, ctx)?;
        }
    }
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    // Then the outputs of its turns, one transaction each, after the judgement: a probe that settled releases its
    // claims before its answer is disposed of (decision 5, -454).
    super::outputs::dispose(conn, run_id, context, ctx)?;
    close(conn, run_id, context, ctx)?;
    Ok(Some(Supervised {
        cursor: high,
        fired,
    }))
}

/// Run `run_id`'s supervision closed, once it is cancelled or ended and nothing is left of it (v13, M2T08R-06): no
/// PENDING trigger, no wake neither answered nor superseded, no output pending, and a woken or an output since the
/// last close. `supervisor.closed` then; the host steps an ended run only while it has something since its last.
fn close(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (project_id, state): (String, String) = tx
        .query_row(
            "SELECT project_id, state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let state = writer::parse::<RunState>(&state)?;
    if !matches!(admission(state), Admission::Supersede) {
        return Ok(());
    }
    let last: Option<i64> = tx
        .query_row(
            "SELECT max(seq) FROM events WHERE run_id = ?1 AND kind IN (?2, ?3) AND seq > \
             (SELECT coalesce(max(seq), 0) FROM events WHERE run_id = ?1 AND kind = ?4)",
            params![run_id, wakes::WOKEN, super::outputs::OUTPUT, CLOSED],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let Some(last) = last else {
        return Ok(());
    };
    let pending: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM supervisor_triggers WHERE run_id = ?1 AND state = 'PENDING')",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if pending
        || wakes::of_run(&tx, run_id)?
            .iter()
            .any(|w| !w.superseded && !w.settled())
        || super::outputs::pending(&tx, run_id)?.is_some()
    {
        return Ok(());
    }
    writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                project_id,
                event_id: child_id(run_id, &format!("supervisor/closed/{last}")),
                run_id: Some(run_id.to_owned()),
                kind: CLOSED.to_owned(),
                payload: json!({"state": state.as_str(), "at": rfc3339_millis_utc(context.now())}),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)
}

/// Run `run_id`'s supervisor step failed with `error`: `supervisor.step_failed{error}` in a transaction of its own,
/// unless the run's last one has the same error (v15, M2T08R-152: seen in the run's events, once per error); whether
/// it was written.
pub(super) fn step_failed(
    conn: &mut Connection,
    run_id: &str,
    error: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let (n, last): (i64, Option<String>) = tx
        .query_row(
            "SELECT count(*), (SELECT json_extract(payload_json, '$.error') FROM events WHERE run_id = ?1 \
             AND kind = ?2 ORDER BY seq DESC LIMIT 1) FROM events WHERE run_id = ?1 AND kind = ?2",
            params![run_id, STEP_FAILED],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    if last.as_deref() == Some(error) {
        return Ok(false);
    }
    writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                project_id,
                event_id: child_id(run_id, &format!("supervisor/step_failed/{n}")),
                run_id: Some(run_id.to_owned()),
                kind: STEP_FAILED.to_owned(),
                payload: json!({"error": error}),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

const STEP_FAILED: &str = "supervisor.step_failed";

/// A run's supervision closed (v13, M2T08R-06).
pub(super) const CLOSED: &str = "supervisor.closed";

/// One batch of run `run_id`'s events after its cursor (at most [`STEP`]) taken in inside `tx`: its project, the
/// cursor after it, and whether it reached the last event committed (a batch of fewer than STEP events).
fn take_in_batch(
    tx: &Transaction<'_>,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<(String, i64, bool), PersistError> {
    let run: Option<(String, Option<i64>, Option<String>)> = tx
        .query_row(
            "SELECT r.project_id, s.revision, json_extract(r.config_snapshot_json, '$.supervision.policy') FROM runs r \
             LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, revision, policy)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    // The policy frozen in the run's snapshot at StartRun (decision 9); balanced when it names none.
    let policy = policy
        .as_deref()
        .and_then(Policy::named)
        .unwrap_or(Policy::Balanced);
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
    // At most STEP events per batch: the cursor stops at the last one looked at (docs/adr/ADR-025.md decision 2).
    let (high, looked): (i64, i64) = tx
        .query_row(
            "SELECT coalesce(max(seq), ?2), count(*) FROM (SELECT seq FROM events WHERE run_id = ?1 AND seq > ?2 \
             ORDER BY seq LIMIT ?3)",
            params![run_id, cursor, STEP],
            |r| Ok((r.get(0)?, r.get(1)?)),
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
    // The batch's waking events joined by key in memory, in the order of each key's first: then one read and one
    // write per key (v13, M2T08R-03; once per event rewrote the whole seq array each time).
    let mut joined: Vec<Joined> = Vec::new();
    let mut index: std::collections::BTreeMap<(String, Option<String>), usize> =
        std::collections::BTreeMap::new();
    let mut specs: std::collections::BTreeMap<i64, Option<String>> =
        std::collections::BTreeMap::new();
    for (seq, kind, payload) in &waking {
        let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
        let Some(wake) = supervise::classify(policy, kind, &payload, revision) else {
            continue;
        };
        let spec_id = match wake.key.spec_revision {
            Some(revision) => match specs.get(&revision) {
                Some(known) => known.clone(),
                None => {
                    let found = tx
                        .query_row(
                            "SELECT spec_id FROM specs WHERE project_id = ?1 AND revision = ?2",
                            params![project_id, revision],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                        .map_err(sqlite_error)?;
                    specs.insert(revision, found.clone());
                    found
                }
            },
            None => None,
        };
        // Timed from its commit (REQ-021 decision 4); an event an earlier writer committed, from now.
        // A revision the project has no spec row for keys by the revision itself, so revisions never merge.
        // A re-trigger's base fingerprint already carries the revision of the wake it re-triggers (M2T08D-954);
        // known by its kind, not by a suffix a producer's fingerprint may have too (M2T08D-1000; escaped, v13).
        let fingerprint = match (spec_id.as_deref(), wake.key.spec_revision) {
            (None, Some(revision)) if !wake.retrigger => {
                format!("{}@r{revision}", wake.key.fingerprint)
            }
            _ => wake.key.fingerprint.clone(),
        };
        let at = ctx.committed_at(*seq).unwrap_or(now);
        let key = (fingerprint, spec_id);
        match index.get(&key) {
            Some(&i) => {
                let j = &mut joined[i];
                j.seqs.push(*seq);
                j.last_at = at;
                j.severity = j.severity.max(wake.severity);
            }
            None => {
                index.insert(key.clone(), joined.len());
                joined.push(Joined {
                    fingerprint: key.0,
                    spec_id: key.1,
                    severity: wake.severity,
                    seqs: vec![*seq],
                    first_at: at,
                    last_at: at,
                });
            }
        }
    }
    for j in &joined {
        take_in(tx, run_id, j)?;
    }
    if high > cursor {
        tx.execute(
            "INSERT INTO supervisor_cursor(run_id, last_handled_seq) VALUES (?1, ?2) \
             ON CONFLICT(run_id) DO UPDATE SET last_handled_seq = max(last_handled_seq, excluded.last_handled_seq)",
            params![run_id, high],
        )
        .map_err(sqlite_error)?;
    }
    Ok((project_id, high, looked < STEP))
}

/// A batch's events of one key, joined in memory: their highest severity, their seqs, and when the first and the last
/// committed.
struct Joined {
    fingerprint: String,
    spec_id: Option<String>,
    severity: Severity,
    seqs: Vec<i64>,
    first_at: SystemTime,
    last_at: SystemTime,
}

/// A batch's events of one key `j`: joined to the key's PENDING trigger, else a new one (REQ-021 decision 3), in one
/// write; its due time again by decision 1, from its first and last events' commits.
fn take_in(tx: &Transaction<'_>, run_id: &str, j: &Joined) -> Result<(), PersistError> {
    let (Some(&first_seq), Some(&last_seq)) = (j.seqs.first(), j.seqs.last()) else {
        return Ok(());
    };
    let pending: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT trigger_id, severity, event_seqs_json, created_at FROM supervisor_triggers \
             WHERE run_id = ?1 AND fingerprint = ?2 AND ifnull(spec_id, '') = ifnull(?3, '') AND state = 'PENDING'",
            params![run_id, j.fingerprint, j.spec_id],
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
                .max(j.severity);
            let mut seqs: Vec<i64> = serde_json::from_str(&seqs).map_err(|e| {
                PersistError::IntegrityViolation(format!(
                    "trigger {trigger_id}: event_seqs_json: {e}"
                ))
            })?;
            seqs.extend(&j.seqs);
            tx.execute(
                "UPDATE supervisor_triggers SET severity = ?2, event_seqs_json = ?3, last_seq = ?4, due_at = ?5 \
                 WHERE trigger_id = ?1",
                params![
                    trigger_id,
                    severity.as_str(),
                    json!(seqs).to_string(),
                    last_seq,
                    rfc3339_millis_utc(supervise::due_at(severity, first, j.last_at))
                ],
            )
            .map_err(sqlite_error)?;
        }
        None => {
            tx.execute(
                "INSERT INTO supervisor_triggers(trigger_id, run_id, fingerprint, spec_id, severity, event_seqs_json, \
                 first_seq, last_seq, due_at, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'PENDING', ?10)",
                params![
                    child_id(run_id, &format!("trigger/{first_seq}")),
                    run_id,
                    j.fingerprint,
                    j.spec_id,
                    j.severity.as_str(),
                    json!(j.seqs).to_string(),
                    first_seq,
                    last_seq,
                    rfc3339_millis_utc(supervise::due_at(j.severity, j.first_at, j.last_at)),
                    rfc3339_millis_utc(j.first_at)
                ],
            )
            .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

/// What a run's state allows its supervisor (docs/adr/ADR-025.md decision 3, admission by run state).
enum Admission {
    /// Working, BLOCKED or WAITING_EXTERNAL: triggers fire.
    Fire,
    /// PAUSE_REQUESTED, PAUSED or INCOMPLETE: nothing fires; what is owed stays.
    Hold,
    /// CANCEL_REQUESTED or ended: what is owed is superseded.
    Supersede,
}

fn admission(state: RunState) -> Admission {
    match state {
        RunState::PauseRequested | RunState::Paused | RunState::Incomplete => Admission::Hold,
        RunState::CancelRequested
        | RunState::Cancelled
        | RunState::Failed
        | RunState::Completed => Admission::Supersede,
        _ => Admission::Fire,
    }
}

/// A PENDING trigger of a run.
struct Pending {
    trigger_id: String,
    fingerprint: String,
    spec_id: Option<String>,
    severity: String,
    seqs: Vec<i64>,
    created_at: String,
    first_seq: i64,
}

/// The record every `supervisor.*` event of a trigger carries (decision 2, -161).
fn record(run_id: &str, t: &Pending, now: SystemTime, ctx: &Ctx<'_>) -> Value {
    let basis = if ctx.committed_at(t.first_seq).is_some() {
        "commit"
    } else {
        "across_restart"
    };
    let latency = parse_rfc3339_millis(&t.created_at)
        .and_then(|first| now.duration_since(first).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    json!({
        "trigger_id": t.trigger_id,
        "dedup_key": {"run_id": run_id, "fingerprint": t.fingerprint, "spec_id": t.spec_id},
        "severity": t.severity,
        "event_seqs": t.seqs,
        "first_event_at": t.created_at,
        "latency_basis": basis,
        "latency_ms": latency,
    })
}

/// Fire run `run_id`'s triggers due at `now`, as its state allows and by the step's one verdict on its Commander
/// (v13): the fired ones, the events to write, and the verdict when the step is to judge the Commander by it (a run
/// with a binding and a spec, in a state that fires).
#[allow(clippy::type_complexity)]
fn fire(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<(Vec<Fired>, Vec<NewEvent>, Option<wakes::Verdict>), PersistError> {
    let now = context.now();
    let (state, run_spec): (String, Option<String>) = tx
        .query_row(
            "SELECT state, spec_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let state = writer::parse::<RunState>(&state)?;
    // A run with no commander binding is not supervised (decision 5; test worlds only).
    let bound: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM role_bindings WHERE run_id = ?1 AND role = 'commander')",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let mut fired = Vec::new();
    let mut events = Vec::new();
    if !bound {
        return Ok((fired, events, None));
    }
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let pending = |due_only: bool| -> Result<Vec<Pending>, PersistError> {
        let mut statement = tx
            .prepare(
                "SELECT trigger_id, fingerprint, spec_id, severity, event_seqs_json, created_at, first_seq \
                 FROM supervisor_triggers WHERE run_id = ?1 AND state = 'PENDING' AND (?2 OR due_at <= ?3) \
                 ORDER BY due_at, first_seq",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![run_id, !due_only, rfc3339_millis_utc(now)], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get::<_, String>(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        rows.into_iter()
            .map(
                |(trigger_id, fingerprint, spec_id, severity, seqs, created_at, first_seq)| {
                    let seqs = serde_json::from_str(&seqs).map_err(|e| {
                        PersistError::IntegrityViolation(format!(
                            "trigger {trigger_id}: event_seqs_json: {e}"
                        ))
                    })?;
                    Ok(Pending {
                        trigger_id,
                        fingerprint,
                        spec_id,
                        severity,
                        seqs,
                        created_at,
                        first_seq,
                    })
                },
            )
            .collect()
    };
    match admission(state) {
        Admission::Hold => return Ok((fired, events, None)),
        Admission::Supersede => {
            // Nothing more is owed to a run that is cancelled or ended: its PENDING triggers and its owed wakes.
            for t in pending(false)? {
                tx.execute(
                    "UPDATE supervisor_triggers SET state = 'SUPERSEDED' WHERE trigger_id = ?1 AND state = 'PENDING'",
                    params![t.trigger_id],
                )
                .map_err(sqlite_error)?;
                let mut payload = record(run_id, &t, now, ctx);
                payload["reason"] = json!(state.as_str());
                events.push(event(
                    child_id(&t.trigger_id, "superseded"),
                    wakes::SUPERSEDED,
                    payload,
                ));
            }
            for wake in wakes::of_run(tx, run_id)?
                .into_iter()
                .filter(wakes::Wake::owed)
            {
                events.push(event(
                    child_id(&wake.id, "superseded"),
                    wakes::SUPERSEDED,
                    json!({"wake": wake.id, "reason": state.as_str()}),
                ));
            }
            return Ok((fired, events, None));
        }
        Admission::Fire => {}
    }
    let due = pending(true)?;
    if run_spec.is_none() {
        // No supervision task without a spec (decision 4): each due trigger waits, deferred once; the Commander is not
        // judged (decision 4 before 5, v13, M2T08R-04).
        for t in due {
            let deferred: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'supervisor.deferred' \
                     AND json_extract(payload_json, '$.trigger_id') = ?2 \
                     AND json_extract(payload_json, '$.reason') = 'NO_SPEC')",
                    params![run_id, t.trigger_id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if !deferred {
                let mut payload = record(run_id, &t, now, ctx);
                payload["reason"] = json!("NO_SPEC");
                events.push(event(
                    child_id(&t.trigger_id, "deferred/NO_SPEC"),
                    "supervisor.deferred",
                    payload,
                ));
            }
        }
        return Ok((fired, events, None));
    }
    // The ended turns folded first (their rearms), as the allocation folds them, so the step's verdict and the
    // allocation's count the same failures (v14, M2T08R-102); then the step's one verdict, before anything fires
    // (v13, M2T08R-05, -08).
    let mut ended = wakes::of_run(tx, run_id)?;
    let (cas, rearmed) = wakes::rearm(tx, project_id, run_id, &mut ended, now)?;
    if !cas.is_empty() || !rearmed.is_empty() {
        writer::write(
            tx,
            &Commit {
                cas,
                events: rearmed,
                ..Commit::default()
            },
            ctx,
        )?;
    }
    let Some(verdict) = wakes::verdict(tx, run_id, context)? else {
        return Ok((fired, events, None));
    };
    let all = wakes::of_run(tx, run_id)?;
    // The run's turn in flight, if any, and whether its wake may still be preempted (decision 3): a non-integrity wake
    // never preempted before.
    let mut target = all
        .iter()
        .find(|w| w.in_flight())
        .filter(|w| w.severity != Severity::Integrity && !w.preempted)
        .and_then(|w| w.turns.last().map(|t| (w.id.clone(), t.attempt_id.clone())));
    let mut owed: Vec<wakes::Wake> = all.into_iter().filter(wakes::Wake::owed).collect();
    // The first owed wake of each key.
    let mut keyed: std::collections::BTreeMap<(String, Option<String>), usize> =
        std::collections::BTreeMap::new();
    for (i, w) in owed.iter().enumerate() {
        keyed
            .entry((w.fingerprint.clone(), w.spec_id.clone()))
            .or_insert(i);
    }
    for t in due {
        let severity = Severity::named(&t.severity).unwrap_or(Severity::Normal);
        // A routine trigger is not kept while the Commander is unavailable, however late the step that finds it due
        // (decision 5, v13, M2T08R-05).
        if severity == Severity::Normal && !verdict.available() {
            tx.execute(
                "UPDATE supervisor_triggers SET state = 'SUPERSEDED' WHERE trigger_id = ?1 AND state = 'PENDING'",
                params![t.trigger_id],
            )
            .map_err(sqlite_error)?;
            let mut payload = record(run_id, &t, now, ctx);
            payload["reason"] = json!("COMMANDER_UNAVAILABLE");
            events.push(event(
                child_id(&t.trigger_id, "superseded"),
                wakes::SUPERSEDED,
                payload,
            ));
            continue;
        }
        tx.execute(
            "UPDATE supervisor_triggers SET state = 'HANDLED' WHERE trigger_id = ?1 AND state = 'PENDING'",
            params![t.trigger_id],
        )
        .map_err(sqlite_error)?;
        // The tasks its events name: what the wake's turn will be shown, and fenced by (decisions 3 and 6).
        // A re-trigger names every task its stale turn was shown (task_ids); a Commander's own mistakes in a row back
        // its wake off from now (decision 6, -700, -752).
        // Its events read by one statement and their tasks kept once each by a set: linear in its events (v14,
        // M2T08R-101; a lookup per task made firing quadratic).
        let mut task_ids: Vec<String> = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        let mut mistakes = 0;
        let rows: Vec<(String, String)> = {
            let mut statement = tx
                .prepare(
                    "SELECT kind, payload_json FROM events WHERE seq IN (SELECT value FROM json_each(?1)) \
                     ORDER BY seq",
                )
                .map_err(sqlite_error)?;
            statement
                .query_map(params![json!(t.seqs).to_string()], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?
        };
        for (kind, payload) in rows {
            let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let named = payload["task_id"].as_str().into_iter().chain(
                payload["task_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str),
            );
            for task in named {
                if seen.insert(task.to_owned()) {
                    task_ids.push(task.to_owned());
                }
            }
            if kind == "supervisor.retrigger" && payload["own_mistake"] == json!(true) {
                mistakes = mistakes.max(payload["n"].as_u64().unwrap_or(1));
            }
        }
        let not_before = (mistakes > 0).then(|| {
            rfc3339_millis_utc(
                now + crate::retry::bound(u32::try_from(mistakes - 1).unwrap_or(u32::MAX)),
            )
        });
        let mut payload = record(run_id, &t, now, ctx);
        payload["woken_at"] = json!(rfc3339_millis_utc(now));
        payload["wake_latency_ms"] = payload["latency_ms"].clone();
        payload["task_ids"] = json!(task_ids);
        if let Some(not_before) = &not_before {
            payload["retry_not_before"] = json!(not_before);
        }
        // A trigger of the key of a wake still owed and not in flight joins it (decision 3, -951, -1053); found by
        // its key (v14, -101).
        let joined = keyed
            .get(&(t.fingerprint.clone(), t.spec_id.clone()))
            .copied()
            .map(|i| &mut owed[i]);
        // The integrity edge: a new integrity wake, or a merge that makes a wake integrity (-1053).
        let (wake_id, edge, wake_not_before) = match joined {
            Some(wake) => {
                let edge = wake.severity != Severity::Integrity && severity == Severity::Integrity;
                wake.severity = wake.severity.max(severity);
                if let Some(at) = &not_before {
                    let later = wake
                        .not_before
                        .as_ref()
                        .map_or(at.clone(), |(was, _)| was.clone().max(at.clone()));
                    wake.not_before = Some((later, i64::MAX));
                }
                payload["wake"] = json!(wake.id);
                payload["merged_into"] = json!(wake.id);
                payload["wake_severity"] = json!(wake.severity.as_str());
                (wake.id.clone(), edge, wake.retry_not_before())
            }
            None => {
                payload["wake"] = json!(t.trigger_id);
                payload["wake_severity"] = json!(severity.as_str());
                keyed
                    .entry((t.fingerprint.clone(), t.spec_id.clone()))
                    .or_insert(owed.len());
                owed.push(wakes::Wake {
                    id: t.trigger_id.clone(),
                    fingerprint: t.fingerprint.clone(),
                    spec_id: t.spec_id.clone(),
                    severity,
                    woken_at: rfc3339_millis_utc(now),
                    woken_seq: 0,
                    task_ids: task_ids.iter().cloned().collect(),
                    event_seqs: t.seqs.clone(),
                    turns: Vec::new(),
                    superseded: false,
                    preempted: false,
                    not_before: not_before.clone().map(|at| (at, i64::MAX)),
                });
                (
                    t.trigger_id.clone(),
                    severity == Severity::Integrity,
                    not_before.as_deref().and_then(parse_rfc3339_millis),
                )
            }
        };
        let record = payload.clone();
        events.push(event(
            child_id(&t.trigger_id, "woken"),
            wakes::WOKEN,
            payload,
        ));
        // At its edge an integrity wake preempts its own run's turn in flight of a wake never preempted, when it could
        // be allocated once that turn stops; in this woken's transaction (decision 3, -800, -953, -1056).
        if edge
            && let Some((target_wake, target_attempt)) = target.take()
            && wakes::could_allocate(tx, run_id, context, wake_not_before, &verdict)?
        {
            let mut preempt = record;
            preempt["wake"] = json!(wake_id);
            preempt["target_wake"] = json!(target_wake);
            preempt["target_attempt"] = json!(target_attempt);
            events.push(event(
                child_id(&target_attempt, "preempt"),
                wakes::PREEMPT,
                preempt,
            ));
        }
        fired.push(Fired {
            trigger_id: t.trigger_id,
            fingerprint: t.fingerprint,
            spec_id: t.spec_id,
            severity,
            event_seqs: t.seqs,
        });
    }
    Ok((fired, events, Some(verdict)))
}

#[cfg(test)]
mod tests;
