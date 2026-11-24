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
        // Pause/restart withdrawals did no work (ADR-021 decision 7). Keep active allocations,
        // started attempts and explicit cancellations charged in the shared task/Run budget view.
        let attempts: f64 = tx
            .query_row(
                "SELECT count(*) FROM attempts WHERE task_id=?1 \
                 AND NOT (state='CANCELLED' AND started_at IS NULL AND cancel_requested=0)",
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
        "SELECT a.task_id,json_extract(e.payload_json,'$.reserved_budget.wall_seconds'),b.used_value FROM attempts a JOIN budgets b ON b.scope_kind='task' AND b.scope_id=a.task_id AND b.kind='wall_seconds' JOIN events e ON e.run_id=?2 AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=a.task_id WHERE a.attempt_id=?1 AND json_extract(e.payload_json,'$.reserved_budget.wall_seconds') IS NOT NULL AND NOT EXISTS (SELECT 1 FROM events d WHERE d.run_id=?2 AND d.kind='task.consult_finished' AND json_extract(d.payload_json,'$.task_id')=a.task_id)",
        params![attempt, run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)),
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

#[cfg(test)]
mod tests {
    use super::accrue_wall;
    use crate::persistence::{Options, Store, selftest};
    use rusqlite::Connection;

    type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn reserved(path: &std::path::Path) -> R<f64> {
        Ok(Connection::open(path)?.query_row(
            "SELECT reserved_value FROM budgets WHERE scope_kind='run' AND scope_id='R' AND kind='wall_seconds'",
            [],
            |r| r.get(0),
        )?)
    }

    /// M3K-103: another run's `task.consult` / `task.consult_finished` for the same task id must not
    /// move this run's reserved wall time. `task_id` is globally unique, but event payloads are not.
    #[tokio::test]
    async fn wall_accrual_ignores_another_runs_consult_events() -> R {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.db");
        let store = Store::open(
            &path,
            Options {
                wal_probe: Some(selftest::test_probe(None)),
                ..Options::new(crate::spawn::Direct::shared())
            },
        )?;
        drop(store);
        Connection::open(&path)?.execute_batch(
            "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p', '/unused', 'now');
             INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s', 'p', printf('%064d', 1), 1);
             INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES
               ('R', 'p', 's', 'RUNNING', 'AUTO', 'now'),
               ('S', 'p', 's', 'RUNNING', 'AUTO', 'now');
             INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES
               ('T', 'R', 's', 'RUNNING', 'developer', 1);
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
               ('A', 'T', 'RUNNING', 1, 1, 0);
             INSERT INTO budgets(scope_kind, scope_id, kind, reserved_value, used_value) VALUES
               ('run', 'R', 'wall_seconds', 100, 0),
               ('task', 'T', 'wall_seconds', 0, 0);
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p', 'other-finished', 'S', 'task.consult_finished', '{\"task_id\":\"T\"}', 'now'),
               ('p', 'mine', 'R', 'task.consult', '{\"task_id\":\"T\",\"reserved_budget\":{\"wall_seconds\":10}}', 'now');",
        )?;
        let mut conn = Connection::open(&path)?;
        let tx = conn.transaction()?;
        accrue_wall(&tx, "R", "A", 4.0)?;
        tx.commit()?;
        assert_eq!(
            reserved(&path)?,
            96.0,
            "this run's unfinished consult still accrues; the other run's finished event does not suppress it"
        );

        Connection::open(&path)?.execute_batch(
            "DELETE FROM events WHERE event_id='mine';
             UPDATE budgets SET reserved_value=100 WHERE scope_kind='run' AND scope_id='R';
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p', 'other-consult', 'S', 'task.consult', '{\"task_id\":\"T\",\"reserved_budget\":{\"wall_seconds\":10}}', 'now');",
        )?;
        let mut conn = Connection::open(&path)?;
        let tx = conn.transaction()?;
        accrue_wall(&tx, "R", "A", 4.0)?;
        tx.commit()?;
        assert_eq!(
            reserved(&path)?,
            100.0,
            "a consult event of another run is not this attempt's allocation"
        );
        Ok(())
    }
}
