//! Commands accepted through the host API, the host's start, and the reads its queries need (REQ-022, REQ-036,
//! REQ-041, REQ-062; docs/adr/ADR-016.md).
//!
//! A command is one `BEGIN IMMEDIATE` transaction: its idempotency key is looked up first (the same payload hash
//! returns the stored result byte for byte, another hash is `KEY_REUSE`), then the run moves by compare-and-set with
//! its event and outbox message, and the exact accepted result is stored with the command. Only accepted commands get
//! a row (`accepted_at` is required): a rejected command is decided again when it is retried.

use super::writer::{self, Change, Commit, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::protocol::RunState;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

/// A command to apply to a run.
#[derive(Debug, Clone, PartialEq)]
pub struct SubmitCommand {
    pub command_id: String,
    pub idempotency_key: String,
    /// The contract name (`$defs.Command.kind`).
    pub kind: String,
    pub run_id: String,
    pub expected_revision: i64,
    /// The command's `payload` object as stored.
    pub payload: serde_json::Value,
    /// sha256 (hex) of the whole command body in canonical form: what the key is compared on.
    pub payload_sha256: String,
    /// The state the run moves to.
    pub target: RunState,
    /// The event appended for the move, and the outbox message for it.
    pub event: NewEvent,
    pub outbox: Option<NewOutbox>,
}

/// What a command came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Applied now: the stored result, the run's new revision and the event's seq.
    Accepted {
        result_json: String,
        revision: i64,
        event_seq: i64,
    },
    /// The same key and payload were accepted before: the result stored then.
    Replayed { result_json: String },
}

/// The project a host serves and its new host epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostStart {
    pub project_id: String,
    pub host_epoch: i64,
}

/// A run as the queries see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRow {
    pub run_id: String,
    pub project_id: String,
    pub state: String,
    pub mode: String,
    pub revision: i64,
    pub resume_state: Option<String>,
    pub blocked_reason: Option<String>,
}

/// A stored event.
#[derive(Debug, Clone, PartialEq)]
pub struct EventRow {
    pub seq: i64,
    pub project_id: String,
    pub event_id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
    pub created_at: String,
}

/// Apply `command` in one IMMEDIATE transaction; any error drops (rolls back) the transaction.
pub(super) fn submit(
    conn: &mut Connection,
    command: &SubmitCommand,
    ctx: &super::writer::Ctx<'_>,
) -> Result<CommandOutcome, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let stored: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT payload_sha256, result_json FROM commands WHERE idempotency_key = ?1",
            params![command.idempotency_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((hash, result)) = stored {
        if hash != command.payload_sha256 {
            return Err(PersistError::KeyReuse(command.idempotency_key.clone()));
        }
        let result_json = result.ok_or_else(|| {
            PersistError::IntegrityViolation(format!(
                "command {} has no stored result",
                command.idempotency_key
            ))
        })?;
        return Ok(CommandOutcome::Replayed { result_json });
    }
    let exists: bool = tx
        .query_row(
            "SELECT 1 FROM runs WHERE run_id = ?1",
            params![command.run_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(sqlite_error)?
        .is_some();
    if !exists {
        return Err(PersistError::RunNotFound(command.run_id.clone()));
    }
    let done = writer::write(
        &tx,
        &Commit {
            cas: vec![StateCas {
                id: command.run_id.clone(),
                expected_revision: command.expected_revision,
                change: Change::Run(command.target),
            }],
            events: vec![command.event.clone()],
            outbox: command.outbox.clone().into_iter().collect(),
        },
        ctx,
    )?;
    let (revision, event_seq) = (done.revisions[0], done.event_seqs[0]);
    // The exact 202 body (openapi submitCommand); a replay returns these bytes.
    let result_json = serde_json::json!({
        "command_id": command.command_id,
        "run_id": command.run_id,
        "revision": revision,
        "status": "ACCEPTED",
    })
    .to_string();
    tx.execute(
        "INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, \
         expected_revision, result_json, accepted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            command.command_id,
            command.run_id,
            command.idempotency_key,
            command.kind,
            command.payload.to_string(),
            command.payload_sha256,
            command.expected_revision,
            result_json,
            crate::time::now_rfc3339(),
        ],
    )
    .map_err(sqlite_error)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(CommandOutcome::Accepted {
        result_json,
        revision,
        event_seq,
    })
}

/// The host's start on the database (REQ-041): an integrity check, the project of `repo_path` (made if new, with
/// `project_id`), and its host epoch one higher.
pub(super) fn begin_host(
    conn: &mut Connection,
    project_id: &str,
    repo_path: &str,
) -> Result<HostStart, PersistError> {
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |r| r.get(0))
        .map_err(sqlite_error)?;
    if check != "ok" {
        return Err(PersistError::IntegrityViolation(format!(
            "the database failed its integrity check: {check}"
        )));
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(repo_path) DO NOTHING",
        params![project_id, repo_path, crate::time::now_rfc3339()],
    )
    .map_err(sqlite_error)?;
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM projects WHERE repo_path = ?1",
            params![repo_path],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let host_epoch: i64 = tx
        .query_row(
            "UPDATE projects SET host_epoch = host_epoch + 1 WHERE project_id = ?1 RETURNING host_epoch",
            params![project_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(HostStart {
        project_id,
        host_epoch,
    })
}

/// The highest event seq, 0 before the first event.
pub(super) fn max_seq(conn: &Connection) -> Result<i64, PersistError> {
    conn.query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))
        .map_err(sqlite_error)
}

/// The run `run_id` and the highest event seq, read in one read transaction.
pub(super) fn snapshot(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, i64)>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let run = conn
            .query_row(
                "SELECT run_id, project_id, state, mode, revision, resume_state, blocked_reason FROM runs \
                 WHERE run_id = ?1",
                params![run_id],
                |r| {
                    Ok(RunRow {
                        run_id: r.get(0)?,
                        project_id: r.get(1)?,
                        state: r.get(2)?,
                        mode: r.get(3)?,
                        revision: r.get(4)?,
                        resume_state: r.get(5)?,
                        blocked_reason: r.get(6)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok::<_, PersistError>(match run {
            Some(run) => Some((run, max_seq(conn)?)),
            None => None,
        })
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// At most `limit` events with seq above `after`, in seq order.
pub(super) fn events_after(
    conn: &Connection,
    after: i64,
    limit: usize,
) -> Result<Vec<EventRow>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT seq, project_id, event_id, run_id, kind, payload_json, created_at FROM events \
             WHERE seq > ?1 ORDER BY seq LIMIT ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(
            params![after, i64::try_from(limit).unwrap_or(i64::MAX)],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            },
        )
        .map_err(sqlite_error)?;
    let mut events = Vec::new();
    for row in rows {
        let (seq, project_id, event_id, run_id, kind, payload, created_at) =
            row.map_err(sqlite_error)?;
        let payload = serde_json::from_str(&payload).map_err(|e| {
            PersistError::IntegrityViolation(format!("event {seq} has an unreadable payload: {e}"))
        })?;
        events.push(EventRow {
            seq,
            project_id,
            event_id,
            run_id,
            kind,
            payload,
            created_at,
        });
    }
    Ok(events)
}

/// Whether the connection refuses a write: a statement that would write nothing still needs a write transaction.
pub(super) fn refuses_writes(conn: &Connection) -> bool {
    conn.execute("UPDATE projects SET host_epoch = host_epoch WHERE 0", [])
        .is_err()
}
