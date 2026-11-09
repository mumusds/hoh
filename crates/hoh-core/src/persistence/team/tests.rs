//! Team revisions and a new run's bindings, snapshot and budget against a real store (docs/adr/ADR-022.md decisions
//! 3 and 7).

use crate::config::{Agent, Budget, Config};
use crate::persistence::{
    CommandOutcome, NewEvent, Options, RunTarget, Store, SubmitCommand, selftest,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    let store = Store::open(&path, options)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-09T00:00:00Z');",
    )?;
    Ok((store, path))
}

/// StartRun of run `run_id` with `budget`, as the host submits it.
fn start(run_id: &str, budget: Budget) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{run_id}"),
        idempotency_key: format!("k-{run_id}"),
        kind: "StartRun".to_owned(),
        run_id: run_id.to_owned(),
        expected_revision: 0,
        payload: json!({"goal": "g", "mode": "AUTO", "interactive": false}),
        payload_sha256: format!("{:064}", run_id.len()),
        target: RunTarget::Start {
            mode: "AUTO".to_owned(),
            budget,
        },
        event: NewEvent {
            project_id: "p1".to_owned(),
            event_id: format!("e-{run_id}"),
            run_id: Some(run_id.to_owned()),
            kind: "run.created".to_owned(),
            payload: json!({"to": "CREATED"}),
        },
        outbox: None,
    }
}

/// The rows `sql` reads, each column as JSON.
fn values(path: &PathBuf, sql: &str) -> R<Vec<Vec<Value>>> {
    use rusqlite::types::ValueRef;
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let width = statement.column_count();
    let rows = statement
        .query_map([], |r| {
            (0..width)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(x) => json!(x),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(b) => json!(b),
                    })
                })
                .collect()
        })?
        .collect::<Result<Vec<Vec<Value>>, _>>()?;
    Ok(rows)
}

fn rows<T: rusqlite::types::FromSql>(path: &PathBuf, sql: &str) -> R<Vec<Vec<T>>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let width = statement.column_count();
    let rows = statement
        .query_map([], |r| (0..width).map(|i| r.get::<_, T>(i)).collect())?
        .collect::<Result<Vec<Vec<T>>, _>>()?;
    Ok(rows)
}

#[tokio::test]
async fn a_started_run_binds_the_team_of_the_current_revision_at_epoch_1() -> R {
    // ADR-022 decisions 3 and 7: the first team transaction writes revision 0 (the built-in defaults, applied); StartRun
    // binds every role of the current revision in its own transaction, epoch 1, at that revision; a second run reuses
    // the revision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let accepted = store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    assert!(matches!(accepted, CommandOutcome::Accepted { .. }));
    let bindings = values(
        &path,
        "SELECT role, agent, model, role_epoch, config_revision, state, session_id FROM role_bindings \
         WHERE run_id = 'R1' ORDER BY role",
    )?;
    let expected: Vec<Vec<Value>> = Config::default()
        .team
        .iter()
        .map(|(role, binding)| {
            vec![
                json!(role),
                json!(binding.agent.as_str()),
                Value::Null,
                json!(1),
                json!(0),
                json!("ACTIVE"),
                Value::Null,
            ]
        })
        .collect();
    assert_eq!(bindings, expected);
    assert_eq!(Config::default().team["qa"].agent, Agent::Pi);
    let revisions = values(
        &path,
        "SELECT revision, state, command_id, applied_at IS NOT NULL FROM team_revisions WHERE project_id = 'p1'",
    )?;
    assert_eq!(
        revisions,
        vec![vec![json!(0), json!("APPLIED"), Value::Null, json!(1)]]
    );
    let config: Value = serde_json::from_str(
        &rows::<String>(&path, "SELECT config_json FROM team_revisions")?[0][0],
    )?;
    assert_eq!(
        config["team"],
        serde_json::to_value(&Config::default().team)?
    );
    assert_eq!(config["source"], json!({"kind": "builtin"}));
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![0]]
    );
    store
        .submit_command(start("R2", Config::default().budget))
        .await?;
    assert_eq!(
        rows::<i64>(&path, "SELECT count(*) FROM team_revisions")?,
        vec![vec![1]]
    );
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT count(*) FROM role_bindings WHERE run_id = 'R2' AND role_epoch = 1 AND config_revision = 0"
        )?,
        vec![vec![6]]
    );
    Ok(())
}

#[tokio::test]
async fn a_started_run_snapshots_the_scheduler_and_gets_its_budget() -> R {
    // CR-007 S2 and ADR-020 decision 7: StartRun records the effective scheduler in the run's snapshot and writes the
    // run's budget rows and deadline from the configuration; a cost cap is written with price_known=0 (no price table,
    // CR-002), no cost cap is no usd row.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    let snapshot: Value = serde_json::from_str(
        &rows::<String>(
            &path,
            "SELECT config_snapshot_json FROM runs WHERE run_id = 'R1'",
        )?[0][0],
    )?;
    assert_eq!(
        snapshot,
        json!({"scheduler": {"weights": {"user_value": 4, "unblock_value": 3, "risk_reduction": 3,
            "information_gain": 2}, "max_wait_rounds": 20}})
    );
    assert_eq!(
        values(
            &path,
            "SELECT kind, limit_value, used_value, reserved_value, price_known FROM budgets \
             WHERE scope_kind = 'run' AND scope_id = 'R1' ORDER BY kind"
        )?,
        vec![
            vec![
                json!("iterations"),
                json!(100.0),
                json!(0.0),
                json!(0.0),
                json!(1)
            ],
            vec![
                json!("wall_seconds"),
                json!(432_000.0),
                json!(0.0),
                json!(0.0),
                json!(1)
            ],
        ]
    );
    assert_eq!(
        values(&path, "SELECT deadline_at FROM runs WHERE run_id = 'R1'")?,
        vec![vec![Value::Null]]
    );
    let budget = Budget {
        max_duration_hours: 1.5,
        max_iterations: 7,
        max_usd: Some(20.0),
        deadline_at: Some("2026-12-31T00:00:00Z".to_owned()),
        ..Config::default().budget
    };
    store.submit_command(start("R2", budget)).await?;
    assert_eq!(
        values(
            &path,
            "SELECT kind, limit_value, price_known FROM budgets WHERE scope_id = 'R2' ORDER BY kind"
        )?,
        vec![
            vec![json!("iterations"), json!(7.0), json!(1)],
            vec![json!("usd"), json!(20.0), json!(0)],
            vec![json!("wall_seconds"), json!(5400.0), json!(1)],
        ]
    );
    assert_eq!(
        rows::<String>(&path, "SELECT deadline_at FROM runs WHERE run_id = 'R2'")?,
        vec![vec!["2026-12-31T00:00:00Z".to_owned()]]
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_start_writes_no_team_revision_and_no_binding() -> R {
    // A command that is refused writes nothing (ADR-016 decision 4): a stale StartRun leaves no revision 0 behind.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let mut stale = start("R1", Config::default().budget);
    stale.expected_revision = 1;
    assert!(store.submit_command(stale).await.is_err());
    for table in ["team_revisions", "role_bindings", "budgets", "runs"] {
        assert_eq!(
            rows::<i64>(&path, &format!("SELECT count(*) FROM {table}"))?,
            vec![vec![0]],
            "{table}"
        );
    }
    Ok(())
}
