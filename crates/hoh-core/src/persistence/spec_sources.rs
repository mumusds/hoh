//! The sources of a specification's decisions (REQ-030 behaviour 2 and 5, docs/adr/ADR-035.md decisions 2 and 9): each
//! decision a proposed or approved revision states must cite what the project's database holds. A `question_id` is a
//! persisted question (the decision id the Runtime gave it, never the Commander's own id) that a user's command
//! answered (`question.answered` of an AnswerQuestion or SendInput); an `input_id` is a run's interactive input of
//! the project; an `event_id` is an applied record_assumption of the project. A decision without its source makes the
//! command SPEC_INVALID with every such entry listed; nothing is written.

use super::{PersistError, sqlite_error};
use crate::specindex::Decision;
use rusqlite::{Connection, params};
use serde_json::{Value, json};

/// The decisions of `decisions` whose source project `project_id` does not hold, each as
/// `details.unsourced[]` lists it: `{path, line, text, source: {kind, id}, missing}`.
pub(super) fn unsourced(
    conn: &Connection,
    project_id: &str,
    decisions: &[Decision],
) -> Result<Vec<Value>, PersistError> {
    let mut missing = Vec::new();
    for decision in decisions {
        let (sql, wanted) = match decision.source_kind.as_str() {
            "question_id" => (
                "SELECT EXISTS(SELECT 1 FROM events e JOIN commands c \
                   ON c.command_id = json_extract(e.payload_json, '$.command_id') \
                   WHERE e.project_id = ?1 AND e.kind = 'question.answered' \
                   AND json_extract(e.payload_json, '$.question_id') = ?2 \
                   AND c.kind IN ('AnswerQuestion', 'SendInput'))",
                "question.answered",
            ),
            "input_id" => (
                "SELECT EXISTS(SELECT 1 FROM input_queue i JOIN runs r ON r.run_id = i.run_id \
                   WHERE r.project_id = ?1 AND i.input_id = ?2)",
                "input",
            ),
            "event_id" => (
                "SELECT EXISTS(SELECT 1 FROM events WHERE project_id = ?1 AND event_id = ?2 \
                   AND kind = 'decision.applied' AND json_extract(payload_json, '$.kind') = 'record_assumption')",
                "record_assumption",
            ),
            other => {
                return Err(PersistError::IntegrityViolation(format!(
                    "a parsed decision has source kind {other}"
                )));
            }
        };
        let found: bool = conn
            .query_row(sql, params![project_id, decision.source_id], |r| r.get(0))
            .map_err(sqlite_error)?;
        if !found {
            missing.push(json!({
                "path": decision.path, "line": decision.line, "text": decision.text,
                "source": {"kind": decision.source_kind, "id": decision.source_id},
                "missing": wanted,
            }));
        }
    }
    Ok(missing)
}

/// `Ok` when every decision of `decisions` has its source in project `project_id`; else the command's
/// [`PersistError::SpecUnsourced`] listing them.
pub(super) fn check(
    conn: &Connection,
    project_id: &str,
    decisions: &[Decision],
) -> Result<(), PersistError> {
    let missing = unsourced(conn, project_id, decisions)?;
    if missing.is_empty() {
        return Ok(());
    }
    Err(PersistError::SpecUnsourced(Box::new(
        json!({"unsourced": missing}),
    )))
}

#[cfg(test)]
mod tests;
