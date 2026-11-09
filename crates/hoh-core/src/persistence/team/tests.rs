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

/// The project's ConfigureTeam at team revision `expected` with `bindings`.
fn team_change(key: &str, expected: i64, bindings: Value) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{key}"),
        idempotency_key: format!("k-{key}"),
        kind: "ConfigureTeam".to_owned(),
        run_id: String::new(),
        expected_revision: expected,
        payload: json!({"bindings": bindings, "apply_at": "checkpoint"}),
        payload_sha256: format!("{:064}", key.len() + 100),
        target: RunTarget::Team(crate::persistence::TeamChange {
            bindings: Some(bindings),
            scheduler: None,
        }),
        event: NewEvent {
            project_id: "p1".to_owned(),
            event_id: format!("e-{key}"),
            run_id: None,
            kind: "team.configured".to_owned(),
            payload: json!({"revision": expected + 1}),
        },
        outbox: None,
    }
}

fn exec(path: &PathBuf, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

/// R1 started (team revision 0), RUNNING on spec s1, with developer task T0 RUNNING (attempt A0 RUNNING) and T1 READY,
/// both readers; the developer bound to session sd.
async fn working(store: &Store, path: &PathBuf) -> R {
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R1';
         INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd', 'claude', 'developer', '1', '{\"structured_events\": true}');
         UPDATE role_bindings SET session_id = 'sd' WHERE run_id = 'R1' AND role = 'developer';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T0', 'R1', 's1', 'RUNNING', 2, 'developer', 0, 1, 1),
           ('T1', 'R1', 's1', 'READY', 1, 'developer', 0, 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, agent, started_at)
           VALUES ('A0', 'T0', 'RUNNING', 1, 1, 0, 'claude', '2026-11-09T00:00:00Z');",
    )
}

/// A0 ends and T0 with it.
fn a0_ends(path: &PathBuf) -> R {
    exec(
        path,
        "UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-09T00:01:00Z' WHERE attempt_id = 'A0';
         UPDATE tasks SET state = 'IMPLEMENTED', revision = 3 WHERE task_id = 'T0';",
    )
}

#[tokio::test]
async fn a_changed_role_is_rebound_at_its_checkpoint_with_the_next_epoch() -> R {
    // ADR-022 decision 8 (REQ-018 behaviour 5): a change waits while the role has an attempt PREPARED, STARTING or
    // RUNNING; the roles it does not change move to the new revision at once; once the role's attempt ended, its
    // binding becomes the new one at epoch+1 with no session yet, one team.binding_applied event; then the run is at
    // the revision. A second checkpoint changes nothing.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex", "model": "m-test"}}),
        ))
        .await?;
    store.team_checkpoint("R1").await?;
    let bindings = |path: &PathBuf| {
        values(
            path,
            "SELECT role, agent, model, role_epoch, config_revision, session_id FROM role_bindings \
             WHERE run_id = 'R1' ORDER BY role",
        )
    };
    let waiting = bindings(&path)?;
    let developer = waiting
        .iter()
        .find(|row| row[0] == "developer")
        .ok_or("no developer")?;
    assert_eq!(
        developer[1..],
        [
            json!("claude"),
            Value::Null,
            json!(1),
            json!(0),
            json!("sd")
        ],
        "A0 still runs"
    );
    for row in waiting.iter().filter(|row| row[0] != "developer") {
        assert_eq!((&row[3], &row[4]), (&json!(1), &json!(1)), "{row:?}");
    }
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![0]]
    );
    let applied = "SELECT count(*) FROM events WHERE kind = 'team.binding_applied'";
    assert_eq!(rows::<i64>(&path, applied)?, vec![vec![0]]);
    a0_ends(&path)?;
    store.team_checkpoint("R1").await?;
    let developer = bindings(&path)?
        .into_iter()
        .find(|row| row[0] == "developer")
        .ok_or("no developer")?;
    assert_eq!(
        developer[1..],
        [
            json!("codex"),
            json!("m-test"),
            json!(2),
            json!(1),
            Value::Null
        ]
    );
    let event: Value = serde_json::from_str(
        &rows::<String>(
            &path,
            "SELECT payload_json FROM events WHERE kind = 'team.binding_applied'",
        )?[0][0],
    )?;
    assert_eq!(
        event,
        json!({
            "run_id": "R1", "role": "developer", "config_revision": 1,
            "from": {"agent": "claude", "model": null, "role_epoch": 1, "session_id": "sd"},
            "to": {"agent": "codex", "model": "m-test", "role_epoch": 2},
        })
    );
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![1]]
    );
    let events = rows::<i64>(&path, "SELECT count(*) FROM events")?;
    store.team_checkpoint("R1").await?;
    assert_eq!(rows::<i64>(&path, "SELECT count(*) FROM events")?, events);
    Ok(())
}

#[tokio::test]
async fn a_role_waiting_for_its_checkpoint_is_not_dispatched_and_its_next_attempt_has_the_new_binding()
-> R {
    // ADR-022 decisions 8 and 9: while the developer's change waits for A0 to end, no developer task is leased under
    // the old binding; after the checkpoint (and a session for the new epoch) the next attempt carries the binding's
    // epoch, revision, agent and session.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    let context = crate::decision::Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events".to_owned()].into(),
        )]
        .into(),
        ..crate::decision::Context::default()
    };
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"model": "m-2"}}),
        ))
        .await?;
    let round = store.schedule_round("R1", context.clone()).await?;
    assert!(round.dispatched.is_none(), "{round:?}");
    a0_ends(&path)?;
    store.team_checkpoint("R1").await?;
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd2', 'claude', 'developer', '1', '{\"structured_events\": true}');
         UPDATE role_bindings SET session_id = 'sd2' WHERE run_id = 'R1' AND role = 'developer';",
    )?;
    let round = store.schedule_round("R1", context).await?;
    assert!(round.dispatched.is_some(), "{round:?}");
    assert_eq!(
        values(
            &path,
            "SELECT agent, role_epoch, config_revision, session_id FROM attempts WHERE task_id = 'T1'"
        )?,
        vec![vec![json!("claude"), json!(2), json!(1), json!("sd2")]]
    );
    Ok(())
}

#[tokio::test]
async fn a_removed_role_is_blocked_at_a_new_epoch_and_one_added_again_does_not_restart_at_1() -> R {
    // ADR-022 decision 8: a role the revision adds is bound at its checkpoint; one it removes keeps its row, BLOCKED
    // (AGENT_UNAVAILABLE) at epoch+1, so a holder of an old epoch stays fenced when the role comes back.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    let auditor = || {
        values(
            &path,
            "SELECT agent, role_epoch, config_revision, state, blocked_reason FROM role_bindings \
             WHERE run_id = 'R1' AND role = 'auditor'",
        )
    };
    let added = json!({"auditor": {"agent": "pi", "prompt": "builtin:qa"}});
    store
        .submit_command(team_change("add", 0, added.clone()))
        .await?;
    store.team_checkpoint("R1").await?;
    assert_eq!(
        auditor()?,
        vec![vec![
            json!("pi"),
            json!(1),
            json!(1),
            json!("ACTIVE"),
            Value::Null
        ]]
    );
    store
        .submit_command(team_change("remove", 1, json!({"auditor": null})))
        .await?;
    store.team_checkpoint("R1").await?;
    assert_eq!(
        auditor()?,
        vec![vec![
            json!("pi"),
            json!(2),
            json!(2),
            json!("BLOCKED"),
            json!("AGENT_UNAVAILABLE")
        ]]
    );
    store.submit_command(team_change("again", 2, added)).await?;
    store.team_checkpoint("R1").await?;
    assert_eq!(
        auditor()?,
        vec![vec![
            json!("pi"),
            json!(3),
            json!(3),
            json!("ACTIVE"),
            Value::Null
        ]]
    );
    let kinds = rows::<String>(
        &path,
        "SELECT json_extract(payload_json, '$.role') || ':' || coalesce(json_extract(payload_json, '$.to.role_epoch'), 'removed') \
         FROM events WHERE kind = 'team.binding_applied' ORDER BY seq",
    )?;
    assert_eq!(
        kinds,
        vec![
            vec!["auditor:1".to_owned()],
            vec!["auditor:removed".to_owned()],
            vec!["auditor:3".to_owned()]
        ]
    );
    Ok(())
}
