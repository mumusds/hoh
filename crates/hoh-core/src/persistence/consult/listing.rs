//! The existing tasks endpoint's read projection; pagination is independent of consultation creation.
use super::*;
#[derive(Debug, Clone)]
pub struct TasksPage {
    pub items: Vec<Value>,
    pub next: Option<i64>,
    pub watermark: i64,
}
pub(in super::super) fn page(
    conn: &Connection,
    run: &str,
    limit: usize,
    after: Option<i64>,
) -> Result<Option<TasksPage>, PersistError> {
    if !(1..=500).contains(&limit) {
        return Err(PersistError::BadRequest(
            "task page limit must be 1..500".into(),
        ));
    }
    let tx = conn.unchecked_transaction().map_err(sqlite_error)?;
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1)",
            params![run],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !exists {
        return Ok(None);
    }
    let watermark = tx
        .query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, Value)> = {
        let mut q=tx.prepare("SELECT t.rowid,t.task_id,t.role,t.state,t.revision,t.writer,t.spec_id,(SELECT e.payload_json FROM events e WHERE e.run_id=t.run_id AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=t.task_id ORDER BY e.seq DESC LIMIT 1),(SELECT a.attempt_id FROM leases l JOIN attempts a USING(attempt_id) WHERE a.task_id=t.task_id ORDER BY l.kind LIMIT 1) FROM tasks t WHERE t.run_id=?1 AND t.rowid>?2 ORDER BY t.rowid LIMIT ?3").map_err(sqlite_error)?;
        q.query_map(params![run,after.unwrap_or(0),(limit+1) as i64],|r| {let id:String=r.get(1)?;let consult:Option<String>=r.get(7)?;let metadata=consult.map(|s|serde_json::from_str::<Value>(&s)).transpose().map_err(|e|rusqlite::Error::FromSqlConversionFailure(7,rusqlite::types::Type::Text,Box::new(e)))?.unwrap_or(Value::Null);Ok((r.get(0)?,json!({"task_id":id,"role":r.get::<_,String>(2)?,"state":r.get::<_,String>(3)?,"revision":r.get::<_,i64>(4)?,"writer":r.get::<_,bool>(5)?,"spec_id":r.get::<_,String>(6)?,"parent_task_id":metadata["parent_task_id"],"deadline":metadata["deadline"],"lease_holder":r.get::<_,Option<String>>(8)?}))) }).map_err(sqlite_error)?.collect::<Result<_,_>>().map_err(sqlite_error)?
    };
    let next = if rows.len() > limit {
        rows.get(limit - 1).map(|r| r.0)
    } else {
        None
    };
    let mut items = Vec::new();
    for (_, mut item) in rows.into_iter().take(limit) {
        let id = item["task_id"]
            .as_str()
            .ok_or_else(|| PersistError::IntegrityViolation("task id missing".into()))?
            .to_owned();
        let mut q=tx.prepare("SELECT kind,limit_value FROM budgets WHERE scope_kind='task' AND scope_id=?1 ORDER BY kind").map_err(sqlite_error)?;
        let budget = q
            .query_map(params![id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<f64>>(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect::<serde_json::Map<_, _>>();
        let mut q = tx
            .prepare(
                "SELECT depends_on FROM task_dependencies WHERE task_id=?1 ORDER BY depends_on",
            )
            .map_err(sqlite_error)?;
        let dependencies = q
            .query_map(params![id], |r| r.get::<_, String>(0))
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        item["budget"] = json!(budget);
        item["dependencies"] = json!(dependencies);
        items.push(item);
    }
    Ok(Some(TasksPage {
        items,
        next,
        watermark,
    }))
}
