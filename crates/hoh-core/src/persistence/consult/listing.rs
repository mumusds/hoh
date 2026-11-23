//! Consultation fields extend the shared task page inside its existing read transaction.
use super::*;

pub(in super::super) fn task_details(conn: &Connection, task: &str) -> Result<Value, PersistError> {
    let (revision, consult): (i64, Option<String>) = conn
        .query_row(
            "SELECT t.revision,(SELECT e.payload_json FROM events e WHERE e.run_id=t.run_id \
             AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=t.task_id \
             ORDER BY e.seq DESC LIMIT 1) FROM tasks t WHERE t.task_id=?1",
            params![task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let metadata = consult
        .map(|s| serde_json::from_str::<Value>(&s))
        .transpose()
        .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?
        .unwrap_or(Value::Null);
    let mut q = conn
        .prepare("SELECT kind,limit_value FROM budgets WHERE scope_kind='task' AND scope_id=?1 ORDER BY kind")
        .map_err(sqlite_error)?;
    let budget = q
        .query_map(params![task], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?
        .into_iter()
        .map(|(k, v)| (k, json!(v)))
        .collect::<serde_json::Map<_, _>>();
    Ok(json!({
        "revision": revision,
        "parent_task_id": metadata["parent_task_id"],
        "deadline": metadata["deadline"],
        "budget": budget,
    }))
}
