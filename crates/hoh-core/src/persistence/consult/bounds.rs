//! Task bounds are admission gates; default allocations also reserve their Run's headroom.
use super::*;
use std::time::SystemTime;

/// Actual task consumption, shared by its limit and its remaining Run reservation.
pub(in super::super) fn consumed(
    tx: &Transaction<'_>,
    task: &str,
    kind: &str,
    mut used: f64,
    now: SystemTime,
) -> Result<Option<f64>, PersistError> {
    if kind == "attempts" {
        let attempts: f64 = tx
            .query_row(
                "SELECT count(*) FROM attempts WHERE task_id=?1",
                params![task],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        used = used.max(attempts);
    }
    if kind == "wall_seconds" {
        let mut q=tx.prepare("SELECT attempt_id,started_at FROM attempts WHERE task_id=?1 AND state IN ('STARTING','RUNNING') AND started_at IS NOT NULL").map_err(sqlite_error)?;
        for row in q
            .query_map(params![task], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?
        {
            let (attempt, started) = row.map_err(sqlite_error)?;
            if let Some(started) = crate::time::parse_rfc3339(&started) {
                used += super::super::retry::ran(
                    started,
                    now,
                    super::super::retry::observed_end(tx, &attempt)?,
                ) as f64;
            }
        }
    }
    if matches!(kind, "tokens" | "usd") {
        let mut q=tx.prepare("SELECT usage_json,cost_usd FROM attempts WHERE task_id=?1 AND state NOT IN ('PREPARED','STARTING','RUNNING') AND started_at IS NOT NULL").map_err(sqlite_error)?;
        let mut total = 0.0;
        for row in q
            .query_map(params![task], |r| {
                Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<f64>>(1)?))
            })
            .map_err(sqlite_error)?
        {
            let (usage, cost) = row.map_err(sqlite_error)?;
            let usage: Value = usage
                .map(|s| serde_json::from_str(&s))
                .transpose()
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?
                .unwrap_or(Value::Null);
            let value = if kind == "usd" {
                cost.or_else(|| usage["reported_cost_usd"].as_f64())
            } else {
                tokens(&usage)
            };
            let Some(value) = value.filter(|v| v.is_finite() && *v >= 0.0) else {
                return Ok(None);
            };
            total += value;
        }
        used = used.max(total);
    }
    Ok(Some(used))
}

fn tokens(usage: &Value) -> Option<f64> {
    // Native adapter usage shapes: Codex app-server/exec, Pi, Claude.
    usage
        .pointer("/last/totalTokens")
        .or_else(|| usage.get("totalTokens"))
        .and_then(Value::as_f64)
        .or_else(|| {
            Some(
                usage.get("input_tokens")?.as_f64()?
                    + usage.get("output_tokens")?.as_f64()?
                    + usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                    + usage
                        .get("cache_read_input_tokens")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
            )
        })
}

pub(in super::super) fn accrue_wall(
    tx: &Transaction<'_>,
    run: &str,
    attempt: &str,
    seconds: f64,
) -> Result<(), PersistError> {
    // Transfer consumed default allocation from reserved to used, without double-counting Run time.
    let allocation:Option<(String,f64,f64)>=tx.query_row(
        "SELECT a.task_id,json_extract(e.payload_json,'$.reserved_budget.wall_seconds'),b.used_value FROM attempts a JOIN budgets b ON b.scope_kind='task' AND b.scope_id=a.task_id AND b.kind='wall_seconds' JOIN events e ON e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=a.task_id WHERE a.attempt_id=?1 AND json_extract(e.payload_json,'$.reserved_budget.wall_seconds') IS NOT NULL AND NOT EXISTS (SELECT 1 FROM events d WHERE d.kind='task.consult_finished' AND json_extract(d.payload_json,'$.task_id')=a.task_id)",
        params![attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
    ).optional().map_err(sqlite_error)?;
    if let Some((_, limit, used)) = allocation {
        tx.execute("UPDATE budgets SET reserved_value=max(0,reserved_value-?1) WHERE scope_kind='run' AND scope_id=?2 AND kind='wall_seconds'",params![seconds.min((limit-used).max(0.0)),run]).map_err(sqlite_error)?;
    }
    tx.execute("UPDATE budgets SET used_value=used_value+?1 WHERE scope_kind='task' AND scope_id=(SELECT task_id FROM attempts WHERE attempt_id=?2) AND kind='wall_seconds'",params![seconds,attempt]).map_err(sqlite_error)?;
    Ok(())
}

pub(super) fn release(
    tx: &Transaction<'_>,
    run: &str,
    task: &str,
    meta: &Value,
) -> Result<(), PersistError> {
    for (kind, limit) in meta["reserved_budget"].as_object().into_iter().flatten() {
        let used: f64 = if kind == "wall_seconds" {
            tx.query_row("SELECT used_value FROM budgets WHERE scope_kind='task' AND scope_id=?1 AND kind=?2",params![task,kind],|r|r.get(0)).map_err(sqlite_error)?
        } else {
            0.0
        };
        let limit = limit
            .as_f64()
            .ok_or_else(|| PersistError::IntegrityViolation("invalid reserved budget".into()))?;
        tx.execute("UPDATE budgets SET reserved_value=max(0,reserved_value-?1) WHERE scope_kind='run' AND scope_id=?2 AND kind=?3",params![(limit-used).max(0.0),run,kind]).map_err(sqlite_error)?;
    }
    Ok(())
}
