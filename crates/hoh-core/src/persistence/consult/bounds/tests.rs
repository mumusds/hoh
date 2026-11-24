
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
