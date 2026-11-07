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

/// The slot a task needs, if one is free at `now`: `writer` for a writer task, else the first free `reader-k` with
/// k ≤ `reader_slots`.
pub(super) fn free_slot(
    tx: &Connection,
    project_id: &str,
    writer: bool,
    reader_slots: u32,
    now: SystemTime,
) -> Result<Option<String>, PersistError> {
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

/// Grant `kind` to `attempt_id` at `now`: refused while the slot is held.
pub(super) fn grant(
    tx: &Transaction<'_>,
    project_id: &str,
    kind: &str,
    attempt_id: &str,
    now: SystemTime,
) -> Result<Grant, PersistError> {
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
