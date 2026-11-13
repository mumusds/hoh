//! Commands accepted through the host API, the host's start, and the reads its queries need (REQ-022, REQ-036,
//! REQ-041, REQ-062; docs/adr/ADR-016.md).
//!
//! A command is one `BEGIN IMMEDIATE` transaction: its idempotency key is looked up first (the same payload hash
//! returns the stored result byte for byte, another hash is `KEY_REUSE`), then the run moves by compare-and-set with
//! its event and outbox message, and the exact accepted result is stored with the command. Only accepted commands get
//! a row (`accepted_at` is required): a rejected command is decided again when it is retried.

use super::writer::{self, Change, Commit, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::lifecycle::Lifecycle as _;
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
    /// Where the run moves.
    pub target: RunTarget,
    /// The event appended for the move, and the outbox message for it.
    pub event: NewEvent,
    pub outbox: Option<NewOutbox>,
}

/// Where a command moves its run.
#[derive(Debug, Clone, PartialEq)]
pub enum RunTarget {
    /// To this state, by compare-and-set.
    To(RunState),
    /// Back to the state saved when it was paused (ResumeRun), after a reconcile of the run recorded first
    /// (docs/adr/ADR-021.md decision 7).
    Resume,
    /// A new run, CREATED, in this mode (StartRun, decision 5): `run_id` is the id the host gave it. It binds the
    /// project's current team and gets `budget` (docs/adr/ADR-022.md decision 7).
    Start {
        mode: String,
        budget: crate::config::Budget,
        /// `supervision.policy`, frozen into the run's snapshot (docs/adr/ADR-025.md decision 9).
        policy: String,
    },
    /// The project's team (ConfigureTeam, docs/adr/ADR-022.md decisions 3 and 4): revision r+1 over the current r,
    /// applied at once; `expected_revision` is the team revision. An empty `run_id` is the project's command (null in
    /// the request); a run id names a run that must exist.
    Team(super::team::TeamChange),
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
    let mut team_revision = None;
    let (revision, event_seq) = match &command.target {
        RunTarget::Start {
            mode,
            budget,
            policy,
        } => {
            if command.expected_revision != 0 {
                return Err(PersistError::StaleRevision {
                    entity: writer::Entity::Run,
                    id: command.run_id.clone(),
                    expected: command.expected_revision,
                    actual: None,
                });
            }
            tx.execute(
                "INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES (?1, ?2, 'CREATED', ?3, 0, ?4)",
                params![
                    command.run_id,
                    command.event.project_id,
                    mode,
                    crate::time::now_rfc3339()
                ],
            )
            .map_err(sqlite_error)?;
            super::team::start_run(
                &tx,
                &command.run_id,
                &command.event.project_id,
                budget,
                policy,
            )?;
            let done = writer::write(
                &tx,
                &Commit {
                    events: vec![command.event.clone()],
                    outbox: command.outbox.clone().into_iter().collect(),
                    ..Commit::default()
                },
                ctx,
            )?;
            (0, done.event_seqs[0])
        }
        RunTarget::Team(change) => {
            if !command.run_id.is_empty() {
                let found: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM runs WHERE run_id = ?1",
                        params![command.run_id],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                if found.is_none() {
                    return Err(PersistError::RunNotFound(command.run_id.clone()));
                }
                // A run keeps its snapshot's scheduler (CR-007 S3): refused after the key was looked up (M2T07R-05).
                if change.scheduler.is_some() {
                    return Err(PersistError::RunConfigFrozen(command.run_id.clone()));
                }
            }
            let made = super::team::configure(&tx, command, change)?;
            let revision = made.revision;
            team_revision = Some(made);
            let done = writer::write(
                &tx,
                &Commit {
                    events: vec![command.event.clone()],
                    outbox: command.outbox.clone().into_iter().collect(),
                    ..Commit::default()
                },
                ctx,
            )?;
            (revision, done.event_seqs[0])
        }
        target => {
            let run: Option<(String, Option<String>, String)> = tx
                .query_row(
                    "SELECT state, resume_state, project_id FROM runs WHERE run_id = ?1",
                    params![command.run_id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((state, saved, project_id)) = run else {
                return Err(PersistError::RunNotFound(command.run_id.clone()));
            };
            let mut events = Vec::new();
            let to = match target {
                RunTarget::To(to) => *to,
                RunTarget::Resume => {
                    let to = saved
                        .as_deref()
                        .map(writer::parse::<RunState>)
                        .transpose()?
                        .filter(|_| state == RunState::Paused.as_str());
                    let Some(to) = to else {
                        return Err(PersistError::IllegalTransition {
                            entity: writer::Entity::Run,
                            id: command.run_id.clone(),
                            from: state,
                            to: "resume_state".to_owned(),
                            why: "only a paused run resumes, to the state it was paused in"
                                .to_owned(),
                        });
                    };
                    events.push(resume_reconcile(&tx, &project_id, command)?);
                    to
                }
                RunTarget::Start { .. } | RunTarget::Team(_) => unreachable!("handled above"),
            };
            let at = events.len();
            events.push(command.event.clone());
            // A resume while blockers are claimed returns to BLOCKED, its resume state kept for when they are all
            // released (docs/adr/ADR-025.md decision 5).
            let mut to = to;
            let mut cas = Vec::new();
            if *target == RunTarget::Resume {
                let reasons: String = tx
                    .query_row(
                        "SELECT blocked_reasons_json FROM runs WHERE run_id = ?1",
                        params![command.run_id],
                        |r| r.get(0),
                    )
                    .map_err(sqlite_error)?;
                let reasons: Vec<String> = serde_json::from_str(&reasons).unwrap_or_default();
                if !reasons.is_empty() {
                    to = RunState::Blocked;
                    let (moves, withdrawn) = super::control::withdraw(
                        &tx,
                        &project_id,
                        &command.run_id,
                        &crate::time::now_rfc3339(),
                        super::control::Withdraw::Blocked,
                        ctx,
                    )?;
                    cas.extend(moves);
                    events.extend(withdrawn);
                    events.push(NewEvent {
                        project_id: project_id.clone(),
                        event_id: crate::ids::child_id(&command.run_id, &format!("resumed-blocked/{}", command.command_id)),
                        run_id: Some(command.run_id.clone()),
                        kind: "run.blocked".to_owned(),
                        payload: serde_json::json!({"reasons": reasons, "detail": "resumed with its blockers claimed"}),
                    });
                }
            }
            // A notice to a role is addressed to its binding as it is now, in this transaction: after a handover the
            // commander's epoch is no longer the one the caller may have written (docs/adr/ADR-024.md decision 6).
            let mut outbox = Vec::new();
            for o in command.outbox.iter().cloned() {
                let current: Option<i64> = tx
                    .query_row(
                        "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
                        params![command.run_id, o.recipient_role],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                outbox.push(NewOutbox {
                    event: o.event + at,
                    role_epoch: current.unwrap_or(o.role_epoch),
                    ..o
                });
            }
            let run_at = cas.len();
            cas.push(StateCas {
                id: command.run_id.clone(),
                expected_revision: command.expected_revision,
                change: Change::Run(to),
            });
            let done = writer::write(
                &tx,
                &Commit {
                    cas,
                    events,
                    outbox,
                },
                ctx,
            )?;
            (done.revisions[run_at], done.event_seqs[at])
        }
    };

    // A cancel marks the run's attempts not yet ended in the same transaction: each ends CANCELLED, before any other
    // disposition (REQ-043 behaviour 3, docs/adr/ADR-021.md decision 8b and 8e).
    if command.target == RunTarget::To(RunState::CancelRequested) {
        tx.execute(
            "UPDATE attempts SET cancel_requested = 1 WHERE state IN ('PREPARED', 'STARTING', 'RUNNING') \
             AND task_id IN (SELECT task_id FROM tasks WHERE run_id = ?1)",
            params![command.run_id],
        )
        .map_err(sqlite_error)?;
    }
    // The exact 202 body (openapi submitCommand); a replay returns these bytes.
    // A project's command (ConfigureTeam with a null run_id) names no run.
    let run_id = (!command.run_id.is_empty()).then_some(&command.run_id);
    let result_json = serde_json::json!({
        "command_id": command.command_id,
        "run_id": run_id,
        "revision": revision,
        "status": "ACCEPTED",
    })
    .to_string();
    tx.execute(
        "INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, \
         expected_revision, result_json, accepted_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            command.command_id,
            run_id,
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
    if let Some(made) = &team_revision {
        super::team::record(&tx, &command.event.project_id, &command.command_id, made)?;
    }
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(CommandOutcome::Accepted {
        result_json,
        revision,
        event_seq,
    })
}

/// A resume's reconcile of its run, recorded before the run works again so that every later dispatch comes after it
/// (REQ-043 behaviour 2, docs/adr/ADR-021.md decision 7): the run's attempts still unfinished (none, for a paused run),
/// those whose effect is unknown, and the leases its attempts hold.
fn resume_reconcile(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    command: &SubmitCommand,
) -> Result<NewEvent, PersistError> {
    let ids = |sql: &str| -> Result<Vec<String>, PersistError> {
        let mut statement = tx.prepare(sql).map_err(sqlite_error)?;
        statement
            .query_map(params![command.run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)
    };
    let unfinished = ids(
        "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
         AND a.state IN ('PREPARED', 'STARTING', 'RUNNING') ORDER BY a.rowid",
    )?;
    let unknown = ids(
        "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
         AND a.state = 'UNKNOWN_EFFECT' ORDER BY a.rowid",
    )?;
    let leases = ids(
        "SELECT l.kind FROM leases l JOIN attempts a ON a.attempt_id = l.attempt_id JOIN tasks t ON t.task_id = a.task_id \
         WHERE t.run_id = ?1 ORDER BY l.kind",
    )?;
    if !unfinished.is_empty() {
        return Err(PersistError::IllegalTransition {
            entity: writer::Entity::Run,
            id: command.run_id.clone(),
            from: RunState::Paused.as_str().to_owned(),
            to: "resume_state".to_owned(),
            why: format!("attempts of it are not finished: {}", unfinished.join(", ")),
        });
    }
    Ok(NewEvent {
        project_id: project_id.to_owned(),
        event_id: crate::ids::child_id(&command.run_id, &format!("resume/{}", command.command_id)),
        run_id: Some(command.run_id.clone()),
        kind: "reconcile.run".to_owned(),
        payload: serde_json::json!({
            "run_id": command.run_id,
            "command_id": command.command_id,
            "unfinished": unfinished,
            "unknown_effect": unknown,
            "leases": leases,
        }),
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
    let read = snapshot_rows(conn, run_id);
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// The run `run_id` and the highest event seq, inside the caller's read transaction.
fn snapshot_rows(conn: &Connection, run_id: &str) -> Result<Option<(RunRow, i64)>, PersistError> {
    {
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
    }
}

/// The run `run_id`, its team and the highest event seq, read in one read transaction.
pub(super) fn run_view(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, super::team::TeamView, i64)>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let Some((run, _)) = snapshot_rows(conn, run_id)? else {
            return Ok(None);
        };
        let team = super::team::view(conn, run_id, &run.project_id)?;
        Ok::<_, PersistError>(Some((run, team, max_seq(conn)?)))
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// [`run_view`] with the run's acceptance snapshot (docs/adr/ADR-029.md decision 5), in one read transaction.
pub(super) fn run_projection(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<(RunRow, super::team::TeamView, super::AcceptanceView, i64)>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let Some((run, _)) = snapshot_rows(conn, run_id)? else {
            return Ok(None);
        };
        let team = super::team::view(conn, run_id, &run.project_id)?;
        let watermark = max_seq(conn)?;
        let Some(acceptance) = super::snapshot::view_in(conn, run_id, watermark)? else {
            return Ok(None);
        };
        Ok::<_, PersistError>(Some((run, team, acceptance, watermark)))
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// A page of runs: each with its creation time, the next page's cursor if there may be one, and the watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunsPage {
    pub runs: Vec<(RunRow, String)>,
    pub next: Option<i64>,
    pub watermark: i64,
}

/// A page of the project's runs, newest first (openapi `listRuns`): at most `limit` runs older than `before` (an
/// opaque cursor: the rowid of the last run of the previous page), only in `state` if given; each with its creation
/// time, the cursor of the next page if there may be one, and the watermark the page is consistent with.
pub(super) fn list_runs(
    conn: &Connection,
    state: Option<&str>,
    limit: usize,
    before: Option<i64>,
) -> Result<RunsPage, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let mut statement = conn
            .prepare(
                "SELECT run_id, project_id, state, mode, revision, resume_state, blocked_reason, created_at, rowid \
                 FROM runs WHERE (?1 IS NULL OR state = ?1) AND (?2 IS NULL OR rowid < ?2) ORDER BY rowid DESC LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(RunRow, String, i64)> = statement
            .query_map(
                params![state, before, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| {
                    Ok((
                        RunRow {
                            run_id: r.get(0)?,
                            project_id: r.get(1)?,
                            state: r.get(2)?,
                            mode: r.get(3)?,
                            revision: r.get(4)?,
                            resume_state: r.get(5)?,
                            blocked_reason: r.get(6)?,
                        },
                        r.get(7)?,
                        r.get(8)?,
                    ))
                },
            )
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let next = (rows.len() == limit)
            .then(|| rows.last().map(|r| r.2))
            .flatten();
        let watermark = max_seq(conn)?;
        Ok::<_, PersistError>(RunsPage {
            runs: rows.into_iter().map(|(run, at, _)| (run, at)).collect(),
            next,
            watermark,
        })
    })();
    let _ = conn.execute_batch("COMMIT");
    read
}

/// The payload of the latest event of `kind` about attempt `attempt_id` (its payload's `attempt_id`), if any.
pub(super) fn attempt_event(
    conn: &Connection,
    kind: &str,
    attempt_id: &str,
) -> Result<Option<serde_json::Value>, PersistError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = ?1 AND json_extract(payload_json, '$.attempt_id') = ?2 \
             ORDER BY seq DESC LIMIT 1",
            params![kind, attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    text.map(|t| {
        serde_json::from_str(&t).map_err(|e| PersistError::IntegrityViolation(e.to_string()))
    })
    .transpose()
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
