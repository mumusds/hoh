//! The rows of a run's requirement trace (REQ-032; docs/adr/ADR-039.md decision 2), read inside the caller's read
//! transaction and projected by the pure `crate::trace`. The spec file is the current candidate's frozen spec object
//! (`.hoh/objects`, beside the store), else the run's revision rebuilt from the rows its approval wrote.

use super::{PersistError, sqlite_error};
use crate::trace::{CandidateRow, EvidenceRow, Input, ResultRow, SpecSource, TaskRow};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::PathBuf;

/// The project root of the store at `conn`: the parent of its `.hoh/` (the host always opens `<project>/.hoh/…`).
pub(super) fn project_root(conn: &Connection) -> Option<PathBuf> {
    let db = std::path::Path::new(conn.path()?);
    let state = db.parent()?;
    Some(if state.file_name().is_some_and(|n| n == ".hoh") {
        state.parent().unwrap_or(state).to_owned()
    } else {
        state.to_owned()
    })
}

/// Run `run_id`'s trace export inside the caller's read transaction; none when there is no such run.
pub(super) fn export(conn: &Connection, run_id: &str) -> Result<Option<Value>, PersistError> {
    let run: Option<(Option<String>, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT r.spec_id, s.revision, s.sha256 FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id \
             WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((spec_id, spec_revision, digest)) = run else {
        return Ok(None);
    };
    // The run's latest candidate (its last candidate.frozen, as the acceptance projection orders them) is current
    // when it binds the current revision's digest.
    let latest: Option<(String, String)> = conn
        .query_row(
            "SELECT c.candidate_id, c.spec_sha256, coalesce((SELECT max(e.seq) FROM events e WHERE e.run_id = c.run_id \
               AND e.kind = 'candidate.frozen' AND json_extract(e.payload_json, '$.candidate_id') = c.candidate_id), 0) \
               AS frozen FROM candidates c WHERE c.run_id = ?1 ORDER BY frozen DESC, c.rowid DESC LIMIT 1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let current = latest.filter(|(_, sha)| digest.as_deref() == Some(sha.as_str()));
    let object = current.as_ref().and_then(|(_, sha)| {
        let bytes = crate::objects::get(&project_root(conn)?, sha).ok()?;
        serde_json::from_slice::<Value>(&bytes).ok()
    });
    // A revision whose approved rows cannot be rebuilt (an AC body that is not its indexed JSON) is shown as such in
    // the export, not as a failed read of the whole run.
    let mut spec_error = None;
    let (spec_source, spec) = match (object, &spec_id) {
        (Some(spec), _) => (SpecSource::Candidate, spec),
        (None, Some(spec_id)) => match super::spec::indexed(conn, spec_id) {
            Ok(spec) => (SpecSource::Revision, spec),
            Err(PersistError::IntegrityViolation(e)) => {
                spec_error = Some(e);
                (SpecSource::Revision, Value::Null)
            }
            Err(e) => return Err(e),
        },
        (None, None) => (SpecSource::Revision, Value::Null),
    };
    // Evidence and results belong to the run by their candidate, or, when that candidate's row is missing, by the
    // task of the attempt that produced them: a dangling node stays in the export with its missing link reported
    // (REQ-032 behaviour 2 and failure mode 1; docs/adr/ADR-039.md, round 1 corrections, M3N-004).
    Ok(Some(crate::trace::export(&Input {
        run_id: run_id.to_owned(),
        spec_revision,
        spec_source,
        spec,
        spec_error,
        candidate_id: current.map(|(id, _)| id),
        tasks: tasks(conn, run_id)?,
        candidates: rows(
            conn,
            "SELECT candidate_id, state FROM candidates WHERE run_id = ?1 ORDER BY rowid",
            run_id,
            |r| {
                Ok(CandidateRow {
                    candidate_id: r.get(0)?,
                    state: r.get(1)?,
                })
            },
        )?,
        evidence: rows(
            conn,
            "SELECT e.evidence_id, e.candidate_id, e.check_id, e.outcome FROM evidence e \
             LEFT JOIN candidates c ON c.candidate_id = e.candidate_id \
             LEFT JOIN attempts p ON p.attempt_id = e.attempt_id LEFT JOIN tasks t ON t.task_id = p.task_id \
             WHERE c.run_id = ?1 OR (c.candidate_id IS NULL AND t.run_id = ?1) ORDER BY e.rowid",
            run_id,
            |r| {
                Ok(EvidenceRow {
                    evidence_id: r.get(0)?,
                    candidate_id: r.get(1)?,
                    check_id: r.get(2)?,
                    outcome: r.get(3)?,
                })
            },
        )?,
        results: rows(
            conn,
            "SELECT a.ac_id, s.revision, a.candidate_id, a.evidence_id, a.status FROM acceptance_results a \
             JOIN specs s ON s.spec_id = a.spec_id LEFT JOIN candidates c ON c.candidate_id = a.candidate_id \
             LEFT JOIN evidence e ON e.evidence_id = a.evidence_id LEFT JOIN attempts p ON p.attempt_id = e.attempt_id \
             LEFT JOIN tasks t ON t.task_id = p.task_id \
             WHERE c.run_id = ?1 OR (c.candidate_id IS NULL AND t.run_id = ?1) ORDER BY a.rowid",
            run_id,
            |r| {
                Ok(ResultRow {
                    ac_id: r.get(0)?,
                    spec_revision: r.get(1)?,
                    candidate_id: r.get(2)?,
                    evidence_id: r.get(3)?,
                    status: r.get(4)?,
                })
            },
        )?,
    })))
}

/// Every row `sql` (one parameter, the run) yields, mapped by `map`.
fn rows<T>(
    conn: &Connection,
    sql: &str,
    run_id: &str,
    map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, PersistError> {
    conn.prepare(sql)
        .map_err(sqlite_error)?
        .query_map(params![run_id], map)
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}

/// Run `run_id`'s tasks in their creation order, each with the requirements it references (sorted).
fn tasks(conn: &Connection, run_id: &str) -> Result<Vec<TaskRow>, PersistError> {
    rows(
        conn,
        "SELECT t.task_id, (SELECT json_group_array(requirement_id) FROM (SELECT DISTINCT requirement_id \
           FROM task_requirements q WHERE q.task_id = t.task_id ORDER BY requirement_id)) \
         FROM tasks t WHERE t.run_id = ?1 ORDER BY t.created_seq, t.task_id",
        run_id,
        |r| {
            let ids: String = r.get(1)?;
            Ok(TaskRow {
                task_id: r.get(0)?,
                requirement_ids: serde_json::from_str(&ids).unwrap_or_default(),
            })
        },
    )
}

/// One item of `GET /v1/runs/{run_id}/tasks` (openapi `listTasks`; REQ-032 behaviour 4): a task with its state, its
/// requirements and dependencies (sorted) and the lease its attempt holds, if any.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskItem {
    pub task_id: String,
    pub state: String,
    pub role: String,
    pub writer: bool,
    pub spec_id: String,
    pub requirement_ids: Vec<String>,
    pub depends_on: Vec<String>,
    /// `{kind, attempt_id}` of the lease one of its attempts holds.
    pub lease: Option<Value>,
    /// The consultation projection's aliases retain the same dependencies and lease holder.
    pub dependencies: Vec<String>,
    pub lease_holder: Option<String>,
    /// Revision, parent, deadline and budget from the consultation ledger (ADR-037).
    #[serde(flatten)]
    pub details: Value,
}

/// A page of [`TaskItem`]s by creation order: the next page's cursor (the last item's created_seq and id) when there
/// are more, and the watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TasksPage {
    pub items: Vec<TaskItem>,
    pub next: Option<(i64, String)>,
    pub watermark: i64,
}

/// A page of run `run_id`'s tasks after `after` (created_seq, task_id), at most `limit`, in one read transaction; none
/// when there is no such run.
pub(super) fn tasks_page(
    conn: &Connection,
    run_id: &str,
    limit: usize,
    after: Option<(i64, String)>,
) -> Result<Option<TasksPage>, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = (|| {
        let watermark = super::commands::max_seq(conn)?;
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id = ?1)",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if !exists {
            return Ok(None);
        }
        let (seq, id) = after.unwrap_or((i64::MIN, String::new()));
        let mut items = conn
            .prepare(
                "SELECT t.task_id, t.state, t.role, t.writer, t.spec_id, t.created_seq, \
                   (SELECT json_group_array(requirement_id) FROM (SELECT DISTINCT requirement_id \
                     FROM task_requirements q WHERE q.task_id = t.task_id ORDER BY requirement_id)), \
                   (SELECT json_group_array(depends_on) FROM (SELECT depends_on FROM task_dependencies d \
                     WHERE d.task_id = t.task_id ORDER BY depends_on)), \
                   (SELECT json_object('kind', l.kind, 'attempt_id', l.attempt_id) FROM leases l \
                     JOIN attempts a ON a.attempt_id = l.attempt_id WHERE a.task_id = t.task_id \
                     ORDER BY l.kind LIMIT 1) \
                 FROM tasks t WHERE t.run_id = ?1 AND (t.created_seq > ?2 OR (t.created_seq = ?2 AND t.task_id > ?3)) \
                 ORDER BY t.created_seq, t.task_id LIMIT ?4",
            )
            .map_err(sqlite_error)?
            .query_map(
                params![run_id, seq, id, i64::try_from(limit).unwrap_or(i64::MAX).saturating_add(1)],
                |r| {
                    let text = |i: usize| -> rusqlite::Result<Vec<String>> {
                        Ok(serde_json::from_str(&r.get::<_, String>(i)?).unwrap_or_default())
                    };
                    let lease: Option<String> = r.get(8)?;
                    let lease = lease.and_then(|l| serde_json::from_str::<Value>(&l).ok());
                    let dependencies = text(7)?;
                    Ok((
                        r.get::<_, i64>(5)?,
                        TaskItem {
                            task_id: r.get(0)?,
                            state: r.get(1)?,
                            role: r.get(2)?,
                            writer: r.get(3)?,
                            spec_id: r.get(4)?,
                            requirement_ids: text(6)?,
                            depends_on: dependencies.clone(),
                            dependencies,
                            lease_holder: lease
                                .as_ref()
                                .and_then(|l| l["attempt_id"].as_str())
                                .map(str::to_owned),
                            lease,
                            details: Value::Null,
                        },
                    ))
                },
            )
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        let next = (items.len() > limit)
            .then(|| {
                items
                    .get(limit.saturating_sub(1))
                    .map(|(seq, t)| (*seq, t.task_id.clone()))
            })
            .flatten();
        items.truncate(limit);
        for (_, item) in &mut items {
            item.details = super::consult::task_details(conn, &item.task_id)?;
        }
        Ok(Some(TasksPage {
            items: items.into_iter().map(|(_, t)| t).collect(),
            next,
            watermark,
        }))
    })();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}
