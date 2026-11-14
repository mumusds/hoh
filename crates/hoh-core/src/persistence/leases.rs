//! Leases (REQ-037, REQ-041; contracts/sql/001_initial.sql `leases`): one row per project and slot (`writer`,
//! `reader-1`…), kept for good. Each grant adds 1 to the slot's epoch through a compare-and-set on the epoch it read,
//! so epochs rise strictly and a holder fenced out by a newer grant can tell (STALE_LEASE_EPOCH).
//!
//! A lease is held while it is unexpired or while its holder attempt has not ended (SETTLED, FAILED, CANCELLED, or
//! UNKNOWN_EFFECT, entered only once its process is gone; ADR-019 decision 5):
//! expiry alone never frees a slot, because the old process may still run (REQ-037 b7: confirm the previous holder
//! ended or was fenced before a new grant; the reap sequence is M2-T05's). Releasing is ending the holder and
//! expiring the lease in the same transaction. Every function runs inside the caller's transaction.

use super::{PersistError, sqlite_error};
use crate::time::rfc3339_utc;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::time::{Duration, SystemTime};

/// How long a grant lasts before it must be renewed (REQ-041: 120 s, renewed every 30 s).
pub const LEASE_TTL: Duration = Duration::from_secs(120);

/// A lease just granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub kind: String,
    pub epoch: i64,
    pub expires_at: String,
}

/// A lease row as of some time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseView {
    pub kind: String,
    pub attempt_id: String,
    pub epoch: i64,
    pub expires_at: String,
    /// It still holds its slot: unexpired, or its holder attempt has not ended.
    pub effective: bool,
}

/// The holder's attempt states that end it: UNKNOWN_EFFECT too, whose process is gone (what is unknown is an effect,
/// not a writer still running; ADR-019 decision 5 amends ADR-017 decision 4).
const ENDED: &str = "('SETTLED','FAILED','CANCELLED','UNKNOWN_EFFECT')";

struct Row {
    attempt_id: String,
    epoch: i64,
    expires_at: String,
    holder_ended: bool,
}

fn row(tx: &Connection, project_id: &str, kind: &str) -> Result<Option<Row>, PersistError> {
    tx.query_row(
        &format!(
            "SELECT l.attempt_id, l.epoch, l.expires_at, a.state IN {ENDED} FROM leases l \
             JOIN attempts a ON a.attempt_id = l.attempt_id WHERE l.project_id = ?1 AND l.kind = ?2"
        ),
        params![project_id, kind],
        |r| {
            Ok(Row {
                attempt_id: r.get(0)?,
                epoch: r.get(1)?,
                expires_at: r.get(2)?,
                holder_ended: r.get(3)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

fn effective(row: &Row, now: &str) -> bool {
    row.expires_at.as_str() > now || !row.holder_ended
}

/// The compat suite open in a project's current host epoch (docs/adr/ADR-030.md decision 10, K7): opened at its first
/// deferral or admission, ended by its publication or by the host; at most one at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OpenSuite {
    /// Member `n` of the epoch's opened series.
    pub(super) suite_id: String,
    pub(super) n: u64,
    /// The id its end takes: member `n` of the epoch's ended series.
    pub(super) ended_id: String,
    pub(super) agent: String,
    pub(super) implementation_sha256: String,
    /// It was admitted to run (`compat.suite_started`), not only deferred.
    pub(super) started: bool,
}

/// The stems of host epoch `host_epoch`'s opened and ended suites in project `project_id` (decision 5's numbering,
/// M2T15D-202).
pub(super) fn suite_series(project_id: &str, host_epoch: i64) -> (String, String) {
    (
        crate::ids::series(project_id, &format!("compat-suite-opened/{host_epoch}")),
        crate::ids::series(project_id, &format!("compat-suite-ended/{host_epoch}")),
    )
}

/// The id of `suite_id`'s `compat.suite_started`.
pub(super) fn suite_started_id(suite_id: &str) -> String {
    crate::ids::child_id(suite_id, "started")
}

/// The suite open in `project_id`'s current host epoch, if one is: the epoch's opened suites outnumber its ended ones
/// (counted on the event ids' unique index, decision 5), and the last opened is the open one. Only the current epoch
/// counts: a suite a host that stopped or crashed left open is closed by the next host's epoch (decision 10, "有界、停止
/// 与崩溃").
pub(super) fn suite_open(
    tx: &Connection,
    project_id: &str,
) -> Result<Option<OpenSuite>, PersistError> {
    let epoch: Option<i64> = tx
        .query_row(
            "SELECT host_epoch FROM projects WHERE project_id = ?1",
            params![project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(epoch) = epoch else {
        return Ok(None);
    };
    let (opened, ended) = suite_series(project_id, epoch);
    let n = super::series::count(tx, &opened)?;
    if n <= super::series::count(tx, &ended)? {
        return Ok(None);
    }
    let suite_id = crate::ids::member(&opened, n);
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![suite_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let payload: serde_json::Value = payload
        .as_deref()
        .and_then(|p| serde_json::from_str(p).ok())
        .ok_or_else(|| {
            PersistError::IntegrityViolation(format!("compat suite {suite_id} has no opening"))
        })?;
    let started: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
            params![suite_started_id(&suite_id)],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let text = |key: &str| payload[key].as_str().unwrap_or_default().to_owned();
    Ok(Some(OpenSuite {
        ended_id: crate::ids::member(&ended, n),
        agent: text("agent"),
        implementation_sha256: text("implementation_sha256"),
        suite_id,
        n,
        started,
    }))
}

/// The holder of the project's writer slot at `now` (RFC 3339), if its lease is effective: its attempt and epoch.
pub(super) fn writer_holder(
    tx: &Connection,
    project_id: &str,
    now: &str,
) -> Result<Option<(String, i64)>, PersistError> {
    Ok(row(tx, project_id, "writer")?
        .filter(|held| effective(held, now))
        .map(|held| (held.attempt_id, held.epoch)))
}

/// The slot a task needs, if one is free at `now`: `writer` for a writer task, else the first free `reader-k` with
/// k ≤ `reader_slots`. The writer slot is not free while a compat suite is open, waiting or running (docs/adr/ADR-030.md
/// decision 10, K7): a suite deferred for the holder is not starved by the writers after it.
pub(super) fn free_slot(
    tx: &Connection,
    project_id: &str,
    writer: bool,
    reader_slots: u32,
    now: SystemTime,
) -> Result<Option<String>, PersistError> {
    if writer && suite_open(tx, project_id)?.is_some() {
        return Ok(None);
    }
    let now = rfc3339_utc(now);
    let kinds: Vec<String> = if writer {
        vec!["writer".to_owned()]
    } else {
        (1..=reader_slots).map(|k| format!("reader-{k}")).collect()
    };
    for kind in kinds {
        match row(tx, project_id, &kind)? {
            Some(held) if effective(&held, &now) => {}
            _ => return Ok(Some(kind)),
        }
    }
    Ok(None)
}

/// Whether slot `kind` is free at `now`.
pub(super) fn is_free(
    tx: &Connection,
    project_id: &str,
    kind: &str,
    now: SystemTime,
) -> Result<bool, PersistError> {
    let now = rfc3339_utc(now);
    Ok(row(tx, project_id, kind)?.is_none_or(|held| !effective(&held, &now)))
}

/// Grant `kind` to `attempt_id` at `now`: refused while the slot is held, and the writer slot while a compat suite is
/// open (docs/adr/ADR-030.md decision 10, K7).
pub(super) fn grant(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    attempt_id: &str,
    now: SystemTime,
) -> Result<Grant, PersistError> {
    if kind == "writer"
        && let Some(open) = suite_open(tx, project_id)?
    {
        return Err(PersistError::LeaseUnavailable(format!(
            "the writer slot waits for compat suite {}",
            open.suite_id
        )));
    }
    let stamp = rfc3339_utc(now);
    match row(tx, project_id, kind)? {
        None => {
            let expires_at = rfc3339_utc(now + LEASE_TTL);
            tx.execute(
                "INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES (?1, ?2, ?3, 1, ?4)",
                params![project_id, kind, attempt_id, expires_at],
            )
            .map_err(sqlite_error)?;
            Ok(Grant {
                kind: kind.to_owned(),
                epoch: 1,
                expires_at,
            })
        }
        Some(held) if effective(&held, &stamp) => Err(PersistError::LeaseUnavailable(format!(
            "{kind} is held by {} (epoch {}, expires {}{})",
            held.attempt_id,
            held.epoch,
            held.expires_at,
            if held.holder_ended {
                ""
            } else {
                ", holder not ended"
            }
        ))),
        Some(held) => cas(tx, project_id, kind, attempt_id, held.epoch, now),
    }
}

/// The compare-and-set of a grant: the next epoch, only if the slot still has the epoch `seen` (compared in the
/// UPDATE itself, so a change between the read and the write is caught).
pub(super) fn cas(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    attempt_id: &str,
    seen: i64,
    now: SystemTime,
) -> Result<Grant, PersistError> {
    let expires_at = rfc3339_utc(now + LEASE_TTL);
    let changed = tx
        .execute(
            "UPDATE leases SET attempt_id = ?1, epoch = epoch + 1, expires_at = ?2 \
             WHERE project_id = ?3 AND kind = ?4 AND epoch = ?5",
            params![attempt_id, expires_at, project_id, kind, seen],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(PersistError::StaleLeaseEpoch(format!(
            "{kind} is no longer at epoch {seen}"
        )));
    }
    Ok(Grant {
        kind: kind.to_owned(),
        epoch: seen + 1,
        expires_at,
    })
}

/// Expire, at `now` (RFC 3339), every unexpired lease `attempt_id` holds (it is ending in this transaction).
pub(super) fn release_held_by(
    tx: &Transaction<'_>,
    attempt_id: &str,
    now: &str,
) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE leases SET expires_at = ?1 WHERE attempt_id = ?2 AND expires_at > ?1",
        params![now, attempt_id],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// Whether the project's writer lease row names `attempt_id`, expired or not (docs/adr/ADR-024.md decision 9: an
/// expired lease of a holder not ended still holds the slot).
pub(super) fn names_writer(tx: &Transaction<'_>, attempt_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM leases WHERE kind = 'writer' AND attempt_id = ?1)",
        params![attempt_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// A writer task's attempt may start only while it holds the unexpired writer lease (REQ-037 b2); other tasks need
/// none here.
pub(super) fn require_writer(
    tx: &Transaction<'_>,
    attempt_id: &str,
    task_id: &str,
    at: &str,
) -> Result<(), PersistError> {
    let writer: bool = tx
        .query_row(
            "SELECT writer FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !writer {
        return Ok(());
    }
    let holds: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM leases WHERE kind = 'writer' AND attempt_id = ?1 AND expires_at > ?2)",
            params![attempt_id, at],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if holds {
        Ok(())
    } else {
        Err(PersistError::LeaseUnavailable(format!(
            "attempt {attempt_id} of writer task {task_id} holds no unexpired writer lease at {at}"
        )))
    }
}

/// The project's leases at `now`, effective or not, by kind.
pub(super) fn held(
    tx: &Connection,
    project_id: &str,
    now: SystemTime,
) -> Result<Vec<LeaseView>, PersistError> {
    let now = rfc3339_utc(now);
    let mut statement = tx
        .prepare(&format!(
            "SELECT l.kind, l.attempt_id, l.epoch, l.expires_at, a.state IN {ENDED} FROM leases l \
             JOIN attempts a ON a.attempt_id = l.attempt_id WHERE l.project_id = ?1 ORDER BY l.kind"
        ))
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Row {
                    attempt_id: r.get(1)?,
                    epoch: r.get(2)?,
                    expires_at: r.get(3)?,
                    holder_ended: r.get(4)?,
                },
            ))
        })
        .map_err(sqlite_error)?;
    rows.map(|row| {
        let (kind, row) = row.map_err(sqlite_error)?;
        Ok(LeaseView {
            effective: effective(&row, &now),
            kind,
            attempt_id: row.attempt_id,
            epoch: row.epoch,
            expires_at: row.expires_at,
        })
    })
    .collect()
}

#[cfg(test)]
mod tests;
