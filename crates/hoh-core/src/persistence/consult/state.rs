//! All consult task moves pass here; only returned::end can write a terminal state.
use super::super::writer::{Ctx, StateCas};
use super::*;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::TaskState;

pub(in super::super) fn is_child(conn: &Connection, task: &str) -> Result<bool, PersistError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.consult' AND json_extract(payload_json,'$.task_id')=?1)", params![task], |r| r.get(0)).map_err(sqlite_error)
}
pub(in super::super) fn finished(conn: &Connection, task: &str) -> Result<bool, PersistError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE kind='task.consult_finished' AND json_extract(payload_json,'$.task_id')=?1)", params![task], |r| r.get(0)).map_err(sqlite_error)
}
pub(in super::super) fn move_task(
    tx: &Transaction<'_>,
    cas: &StateCas,
    to: TaskState,
    ctx: &Ctx<'_>,
) -> Result<Option<i64>, PersistError> {
    if !is_child(tx, &cas.id)? {
        return Ok(None);
    }
    let from: String = tx
        .query_row(
            "SELECT state FROM tasks WHERE task_id=?1",
            params![cas.id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if finished(tx, &cas.id)?
        || matches!(
            to,
            TaskState::Implemented | TaskState::Accepted | TaskState::Verifying
        )
    {
        return Err(PersistError::IllegalTransition {
            entity: super::super::Entity::Task,
            id: cas.id.clone(),
            from,
            to: to.as_str().into(),
            why: "consult outcomes are immutable and owned by consult completion".into(),
            details: None,
        });
    }
    if to == TaskState::Cancelled {
        return returned::end(tx, &cas.id, None, Some("cancelled"), None, ctx).map(Some);
    }
    let changed = tx.execute("UPDATE tasks SET state=?1,revision=revision+1 WHERE task_id=?2 AND revision=?3 AND ?1 IN ('PENDING','READY','LEASED','RUNNING','BLOCKED')", params![to.as_str(),cas.id,cas.expected_revision]).map_err(sqlite_error)?;
    if changed != 1 {
        return Err(PersistError::IntegrityViolation(format!(
            "consult {} lost its nonterminal CAS",
            cas.id
        )));
    }
    Ok(Some(cas.expected_revision + 1))
}
