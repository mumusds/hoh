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
            root: None,
            file: None,
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

/// Run R1 on spec s1: a binding change hands its role over with a package naming the run's spec, and a run with
/// none has every change refused NO_SPEC (docs/adr/ADR-024.md decisions 1 and 10).
fn on_a_spec(path: &PathBuf) -> R {
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         UPDATE runs SET spec_id = 's1' WHERE run_id = 'R1';",
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd2', 'claude', 'developer', '1', '{\"structured_events\": true}');
         UPDATE role_bindings SET session_id = 'sd2' WHERE run_id = 'R1' AND role = 'developer';",
    )?;
    // The new holder has its first input first (ADR-024 decision 4): its session start delivered to sd2.
    let start = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .ok_or("no session start")?;
    assert!(
        store
            .deliver_session_start(&start.message_id, "sd2")
            .await?
    );
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
    on_a_spec(&path)?;
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
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
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
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
    // M2T07R-152: a removed role is not a change of the revisions that follow, and the view tells it from a role
    // blocked for its availability.
    exec(
        &path,
        "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'AGENT_UNAVAILABLE' \
         WHERE run_id = 'R1' AND role = 'developer'",
    )?;
    let removed = || -> R<Vec<(String, bool)>> {
        Ok(super::view(&Connection::open(&path)?, "R1", "p1")?
            .members
            .into_iter()
            .filter(|m| m.state == "BLOCKED")
            .map(|m| (m.role, m.removed))
            .collect())
    };
    assert_eq!(
        removed()?,
        vec![
            ("developer".to_owned(), false),
            ("auditor".to_owned(), true)
        ]
    );
    store
        .submit_command(team_change("other", 2, json!({"cto": {"model": "m-cto"}})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        auditor()?,
        vec![vec![
            json!("pi"),
            json!(2),
            json!(3),
            json!("BLOCKED"),
            json!("AGENT_UNAVAILABLE")
        ]]
    );
    assert_eq!(
        removed()?,
        vec![
            ("developer".to_owned(), false),
            ("auditor".to_owned(), true)
        ]
    );
    store.submit_command(team_change("again", 3, added)).await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        auditor()?,
        vec![vec![
            json!("pi"),
            json!(3),
            json!(4),
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
            vec!["cto:2".to_owned()],
            vec!["auditor:3".to_owned()]
        ]
    );
    Ok(())
}

/// R1's binding of `role`: agent, model, role_epoch, config_revision, state, blocked_reason, session_id.
fn binding(path: &PathBuf, role: &str) -> R<Vec<Value>> {
    Ok(values(
        path,
        &format!(
            "SELECT agent, model, role_epoch, config_revision, state, blocked_reason, session_id FROM role_bindings \
             WHERE run_id = 'R1' AND role = '{role}'"
        ),
    )?
    .pop()
    .ok_or("no binding")?)
}

fn applied(path: &PathBuf) -> R<i64> {
    Ok(rows::<i64>(
        path,
        "SELECT count(*) FROM events WHERE kind = 'team.binding_applied'",
    )?[0][0])
}

#[tokio::test]
async fn a_role_blocked_for_its_availability_is_not_a_changed_role() -> R {
    // M2T07R-01, -51 (decision 8, second version): a revision that changes another role leaves a role blocked for its
    // availability as it is (state, reason, epoch, session) and moves it to the revision; removing such a role is a
    // change: epoch+1 and an event.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    on_a_spec(&path)?;
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sq', 'pi', 'qa', '1', '{}');
         UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH', session_id = 'sq'
           WHERE run_id = 'R1' AND role = 'qa';
         UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'AGENT_UNAVAILABLE'
           WHERE run_id = 'R1' AND role = 'researcher';",
    )?;
    store
        .submit_command(team_change("cto", 0, json!({"cto": {"model": "m-cto"}})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "qa")?,
        [
            json!("pi"),
            Value::Null,
            json!(1),
            json!(1),
            json!("BLOCKED"),
            json!("BLOCKED_AUTH"),
            json!("sq")
        ]
    );
    assert_eq!(applied(&path)?, 1, "only the cto");
    store
        .submit_command(team_change("drop", 1, json!({"researcher": null})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(binding(&path, "researcher")?[2..4], [json!(2), json!(2)]);
    assert_eq!(applied(&path)?, 2, "the removal is an event");
    Ok(())
}

#[tokio::test]
async fn an_unchanged_role_moves_to_the_revision_while_its_attempt_runs() -> R {
    // M2T07R-03: a role the revision does not change is not held back by its live attempt; its next attempt records
    // the new revision, and the run follows once the changed roles are there too.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change("cto", 0, json!({"cto": {"model": "m-cto"}})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(binding(&path, "developer")?[2..4], [json!(1), json!(1)]);
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![1]]
    );
    let context = crate::decision::Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events".to_owned()].into(),
        )]
        .into(),
        ..crate::decision::Context::default()
    };
    let round = store.schedule_round("R1", context).await?;
    assert!(round.dispatched.is_some(), "{round:?}");
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM attempts WHERE task_id = 'T1'"
        )?,
        vec![vec![1]]
    );
    Ok(())
}

#[tokio::test]
async fn a_change_waits_for_a_prepared_or_starting_attempt_too() -> R {
    // M2T07R-55 (1): the checkpoint is a role with no attempt PREPARED, STARTING or RUNNING.
    for state in ["PREPARED", "STARTING"] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        working(&store, &path).await?;
        exec(
            &path,
            &format!("UPDATE attempts SET state = '{state}' WHERE attempt_id = 'A0';"),
        )?;
        store
            .submit_command(team_change(
                "dev",
                0,
                json!({"developer": {"agent": "codex"}}),
            ))
            .await?;
        store
            .team_checkpoint("R1", crate::persistence::Availability::default())
            .await?;
        assert_eq!(
            binding(&path, "developer")?[0..3],
            [json!("claude"), Value::Null, json!(1)],
            "{state}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_change_of_the_prompt_alone_is_a_new_epoch() -> R {
    // M2T07R-55 (2): fallback, prompt and session independence are part of a role's configuration.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    on_a_spec(&path)?;
    store
        .submit_command(team_change(
            "prompt",
            0,
            json!({"developer": {"prompt": "builtin:cto"}, "qa": {"fallback": ["claude"]}}),
        ))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(binding(&path, "developer")?[2], json!(2));
    assert_eq!(binding(&path, "qa")?[2], json!(2));
    assert_eq!(binding(&path, "cto")?[2], json!(1));
    Ok(())
}

#[tokio::test]
async fn a_change_to_an_unavailable_agent_keeps_the_old_binding_until_the_agent_is_there() -> R {
    // M2T07R-52 (REQ-019 decision 2, failure mode 3): with its new agent unavailable the role keeps its binding and
    // epoch, one team.binding_refused is written for the role and revision, and it waits; once the agent is available it
    // is applied.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    on_a_spec(&path)?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    let codex: std::collections::BTreeSet<String> = ["codex".to_owned()].into();
    store.team_checkpoint("R1", codex.clone()).await?;
    store.team_checkpoint("R1", codex).await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("claude"), Value::Null, json!(1), json!(0)]
    );
    let refused = values(
        &path,
        "SELECT json_extract(payload_json, '$.role'), json_extract(payload_json, '$.agent') FROM events \
         WHERE kind = 'team.binding_refused'",
    )?;
    assert_eq!(refused, vec![vec![json!("developer"), json!("codex")]]);
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![0]]
    );
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("codex"), Value::Null, json!(2), json!(1)]
    );
    Ok(())
}

/// The developer of R1 as the team view shows it: its agent, epoch and what is pending for it.
fn shown_developer(path: &PathBuf) -> R<Value> {
    let view = super::view(&Connection::open(path)?, "R1", "p1")?;
    let member = view
        .members
        .into_iter()
        .find(|m| m.role == "developer")
        .ok_or("no developer")?;
    Ok(json!([member.agent, member.role_epoch, member.pending]))
}

#[tokio::test]
async fn a_refused_change_leaves_the_old_binding_working_until_its_agent_is_there() -> R {
    // M2T07R-150 (REQ-019 decision 2 and failure mode 3, ADR-022 decision 8 third version): while the change's new agent
    // is unavailable the change is refused and the old binding, its epoch and session go on being dispatched, the role
    // never vacant; once the agent is there the role is held again until its next checkpoint, where the change is
    // applied. Each turn of the verdict is one event, however many checkpoints see it.
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
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    let codex: std::collections::BTreeSet<String> = ["codex".to_owned()].into();
    let refused = json!(["claude", 1, {"agent": "codex", "model": null, "config_revision": 1, "refused": "AGENT_UNAVAILABLE"}]);
    // Refused while A0 runs too: the old binding is not held back for a change that cannot be applied.
    store.team_checkpoint("R1", codex.clone()).await?;
    assert_eq!(shown_developer(&path)?, refused);
    a0_ends(&path)?;
    store.team_checkpoint("R1", codex.clone()).await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("claude"), Value::Null, json!(1), json!(0)]
    );
    assert_eq!(
        shown_developer(&path)?,
        json!(["claude", 1, {"agent": "codex", "model": null, "config_revision": 1, "refused": "AGENT_UNAVAILABLE"}])
    );
    store.pace_run("R1", context.clone()).await?;
    let round = store.schedule_round("R1", context.clone()).await?;
    assert!(round.dispatched.is_some(), "{round:?}");
    assert_eq!(
        values(
            &path,
            "SELECT agent, role_epoch, session_id, config_revision FROM attempts WHERE task_id = 'T1'"
        )?,
        vec![vec![json!("claude"), json!(1), json!("sd"), json!(0)]]
    );
    // The agent is there while T1's attempt is live: the role is held, T2 waits.
    exec(
        &path,
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T2', 'R1', 's1', 'READY', 1, 'developer', 0, 3, 1);",
    )?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("claude"), Value::Null, json!(1), json!(0)]
    );
    assert_eq!(
        shown_developer(&path)?,
        json!(["claude", 1, {"agent": "codex", "model": null, "config_revision": 1, "refused": null}])
    );
    let round = store.schedule_round("R1", context.clone()).await?;
    assert!(round.dispatched.is_none(), "{round:?}");
    exec(
        &path,
        "UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-09T00:02:00Z' WHERE task_id = 'T1';
         UPDATE tasks SET state = 'IMPLEMENTED' WHERE task_id = 'T1';",
    )?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("codex"), Value::Null, json!(2), json!(1)]
    );
    assert_eq!(
        rows::<String>(
            &path,
            "SELECT kind FROM events WHERE kind LIKE 'team.binding_%' ORDER BY seq"
        )?,
        vec![
            vec!["team.binding_refused".to_owned()],
            vec!["team.binding_pending".to_owned()],
            vec!["team.binding_applied".to_owned()]
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_change_to_an_agent_not_looked_at_yet_waits_without_a_verdict() -> R {
    // ADR-022 decisions 8 and 10: the registry looks at agents in its own task; until it has looked at the change's new
    // agent the change waits, held, with no event; then it is judged.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    on_a_spec(&path)?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    // The registry has looked at claude only: codex, which the change names, is not known yet (M2T07R-255: whatever
    // the team read before the checkpoint named).
    let unknown = crate::persistence::Availability {
        available: Some(["claude".to_owned()].into()),
        unavailable: Default::default(),
        auth_missing: Default::default(),
    };
    store.team_checkpoint("R1", unknown).await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("claude"), Value::Null, json!(1), json!(0)]
    );
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT count(*) FROM events WHERE kind LIKE 'team.binding_%'"
        )?,
        vec![vec![0]]
    );
    assert_eq!(
        shown_developer(&path)?,
        json!(["claude", 1, {"agent": "codex", "model": null, "config_revision": 1, "refused": null}])
    );
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("codex"), Value::Null, json!(2), json!(1)]
    );
    Ok(())
}

#[tokio::test]
async fn a_refusal_holds_for_its_revision_only() -> R {
    // M2T07R-150 (ADR-022 decision 8 third version): a later revision is judged afresh, whatever the earlier one's
    // verdict: pending until its own checkpoint, then applied if its agent is there.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    on_a_spec(&path)?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    let codex: std::collections::BTreeSet<String> = ["codex".to_owned()].into();
    store.team_checkpoint("R1", codex.clone()).await?;
    store
        .submit_command(team_change("pi", 1, json!({"developer": {"agent": "pi"}})))
        .await?;
    assert_eq!(
        shown_developer(&path)?,
        json!(["claude", 1, {"agent": "pi", "model": null, "config_revision": 2, "refused": null}])
    );
    store.team_checkpoint("R1", codex).await?;
    assert_eq!(
        binding(&path, "developer")?[0..4],
        [json!("pi"), Value::Null, json!(2), json!(2)]
    );
    Ok(())
}

#[tokio::test]
async fn a_role_with_no_session_yet_has_its_tasks_wait_not_blocked() -> R {
    // M2T07R-50: right after its checkpoint a role has no session (the binder gives it one); the pace that follows does
    // not judge its tasks by capabilities it does not have yet, and no round dispatches them.
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
    a0_ends(&path)?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(binding(&path, "developer")?[6], Value::Null);
    store.pace_run("R1", context.clone()).await?;
    let round = store.schedule_round("R1", context).await?;
    assert!(round.dispatched.is_none(), "{round:?}");
    assert_eq!(
        values(
            &path,
            "SELECT state, blocked_reason FROM tasks WHERE task_id = 'T1'"
        )?,
        vec![vec![json!("READY"), Value::Null]]
    );
    Ok(())
}

#[tokio::test]
async fn an_attempt_records_its_bindings_revision_while_the_run_is_behind() -> R {
    // M2T07R-55 (3), REQ-018 behaviour 6: while the developer's change waits for A0, the run stays at revision 0; the
    // cto, unchanged, is at revision 1, and its next attempt records 1, its binding's, not the run's.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc', 'claude', 'cto', '1', '{\"structured_events\": true}');
         UPDATE role_bindings SET session_id = 'sc' WHERE run_id = 'R1' AND role = 'cto';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T9', 'R1', 's1', 'READY', 1, 'cto', 0, 9, 1);",
    )?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT config_revision FROM runs WHERE run_id = 'R1'"
        )?,
        vec![vec![0]]
    );
    let context = crate::decision::Context {
        required_capabilities: [
            ("cto".to_owned(), ["structured_events".to_owned()].into()),
            (
                "developer".to_owned(),
                ["structured_events".to_owned()].into(),
            ),
        ]
        .into(),
        ..crate::decision::Context::default()
    };
    let round = store.schedule_round("R1", context).await?;
    assert!(round.dispatched.is_some(), "{round:?}");
    assert_eq!(
        values(
            &path,
            "SELECT task_id, config_revision FROM attempts WHERE task_id IN ('T1', 'T9')"
        )?,
        vec![vec![json!("T9"), json!(1)]]
    );
    Ok(())
}

#[tokio::test]
async fn the_binder_binds_a_session_per_role_and_epoch_or_blocks_an_unavailable_agent() -> R {
    // docs/adr/ADR-022.md decision 9: the bindings waiting for a session are listed (a removed role is not); a grant
    // writes the session (one per run, role and epoch) and binds it; an unavailable agent blocks the binding, and a
    // later grant makes it active again.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    store
        .submit_command(team_change("drop", 0, json!({"researcher": null})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    let waiting = store.runner_waiting("p1").await?.unbound;
    let roles: Vec<&str> = waiting.iter().map(|u| u.role.as_str()).collect();
    assert_eq!(roles, ["commander", "cto", "developer", "qa", "reviewer"]);
    let developer = waiting
        .iter()
        .find(|u| u.role == "developer")
        .ok_or("no developer")?
        .clone();
    let grant = |epoch: i64| crate::persistence::SessionGrant {
        run_id: "R1".to_owned(),
        role: "developer".to_owned(),
        role_epoch: epoch,
        agent: "claude".to_owned(),
        agent_version: "2.1.318 (Claude Code)".to_owned(),
        capabilities: json!({"structured_events": true, "resume": true}),
    };
    assert!(
        !store.bind_session(grant(2)).await?,
        "another epoch is not granted"
    );
    assert!(
        store
            .block_binding(developer.clone(), "claude is not on PATH".to_owned())
            .await?
    );
    assert_eq!(
        binding(&path, "developer")?[4..6],
        [json!("BLOCKED"), json!("AGENT_UNAVAILABLE")]
    );
    assert!(store.bind_session(grant(1)).await?);
    let session = crate::persistence::session_id("R1", "developer", 1);
    assert_eq!(
        binding(&path, "developer")?[4..7],
        [json!("ACTIVE"), Value::Null, json!(session)]
    );
    assert_eq!(
        values(
            &path,
            &format!(
                "SELECT agent, role, agent_version FROM sessions WHERE session_id = '{session}'"
            )
        )?,
        vec![vec![
            json!("claude"),
            json!("developer"),
            json!("2.1.318 (Claude Code)")
        ]]
    );
    assert!(!store.bind_session(grant(1)).await?, "bound once");
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT count(*) FROM events WHERE kind IN ('team.session_bound', 'team.binding_blocked')"
        )?,
        vec![vec![2]]
    );
    Ok(())
}

#[tokio::test]
async fn a_dispatch_resumes_the_agents_own_session_only_where_the_session_can_be_resumed() -> R {
    // M2T07R-257 (ADR-022 decision 9): the agent's own session id goes with a dispatch only when the session's
    // capabilities have resume; otherwise the turn starts a session of its own.
    for (capabilities, resumed) in [
        (
            r#"{"structured_events": true, "resume": true}"#,
            Some("n-1"),
        ),
        (r#"{"structured_events": true, "resume": false}"#, None),
        (r#"{"structured_events": true}"#, None),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        working(&store, &path).await?;
        a0_ends(&path)?;
        exec(
            &path,
            &format!(
                "UPDATE sessions SET native_id = 'n-1', capabilities_json = '{capabilities}' WHERE session_id = 'sd';"
            ),
        )?;
        let context = crate::decision::Context {
            required_capabilities: [(
                "developer".to_owned(),
                ["structured_events".to_owned()].into(),
            )]
            .into(),
            ..crate::decision::Context::default()
        };
        let round = store.schedule_round("R1", context).await?;
        assert!(round.dispatched.is_some(), "{round:?}");
        let notice = store
            .runner_waiting("p1")
            .await?
            .dispatches
            .into_iter()
            .find(|d| d.task_id == "T1")
            .ok_or("no notice")?;
        assert_eq!(
            notice.assignment.native_session.as_deref(),
            resumed,
            "{capabilities}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn the_agents_to_look_at_include_every_bindings_not_only_the_teams() -> R {
    // M2T07R-250 (ADR-022 decision 10): an agent no role of the current team names, but a binding of a run not ended
    // still holds (a change not applied yet, or refused), is looked at too.
    let dir = tempfile::tempdir()?;
    let (store, _path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    store
        .submit_command(team_change(
            "away",
            0,
            json!({"cto": {"agent": "pi"}, "developer": {"agent": "codex"}}),
        ))
        .await?;
    let waiting = store.runner_waiting("p1").await?;
    assert!(waiting.agents.contains("claude"), "{:?}", waiting.agents);
    Ok(())
}

/// R1 as `working` leaves it, with the state a Commander's handover package names (REQ-019 b4): two APPLIED decisions
/// and a REJECTED one, an OPEN, a REOPENED and a RESOLVED issue, a QUEUED and a DELIVERED input, an outbox row
/// PENDING, one DEAD_LETTER and one ACKED, and candidate C1 FROZEN.
fn handover_state(path: &PathBuf) -> R {
    exec(
        path,
        "INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, payload_json, state) VALUES
           ('D2', 'R1', 1, 1, '{}', 'APPLIED'), ('D1', 'R1', 1, 1, '{}', 'APPLIED'),
           ('D3', 'R1', 1, 1, '{}', 'REJECTED');
         INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at) VALUES
           ('C1', 'R1', printf('%040d', 1), printf('%040d', 2), printf('%064d', 1), printf('%064d', 3),
            printf('%064d', 4), '/repo', 'FROZEN', '2026-11-09T00:00:00Z');
         INSERT INTO issues(issue_id, run_id, fingerprint, severity, state, description) VALUES
           ('I1', 'R1', 'f1', 'high', 'OPEN', 'd'), ('I2', 'R1', 'f2', 'low', 'REOPENED', 'd'),
           ('I3', 'R1', 'f3', 'low', 'RESOLVED', 'd');
         INSERT INTO input_queue(input_id, run_id, kind, target_role, state, payload_json) VALUES
           ('Q1', 'R1', 'STEER', 'developer', 'QUEUED', '{}'), ('Q2', 'R1', 'ASK', NULL, 'DELIVERED', '{}');
         INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, payload_json, state) VALUES
           ('M1', (SELECT min(seq) FROM events), 'R1', 'developer', '{}', 'PENDING'),
           ('M2', (SELECT min(seq) FROM events), 'R1', 'qa', '{}', 'DEAD_LETTER'),
           ('M3', (SELECT min(seq) FROM events), 'R1', 'qa', '{}', 'ACKED');",
    )
}

/// A store whose every commit fails while its flag is set.
#[derive(Debug, Default)]
struct FailingCommits(std::sync::atomic::AtomicBool);

impl crate::persistence::FaultHooks for FailingCommits {
    fn before_commit(&self) -> Result<(), crate::persistence::PersistError> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            Err(crate::persistence::PersistError::IntegrityViolation(
                "a failed commit".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn a_commander_change_writes_binding_package_and_session_start_in_one_transaction() -> R {
    // REQ-019 b2, b4 (M2-T17): the commander's change applies at its checkpoint with, in the same transaction, its
    // binding at epoch 2, team.binding_applied, a handoff.prepared event and one runtime session.start notice to the
    // commander at epoch 2 carrying the role-change package: the goal, the run's spec and candidate, and the ids of
    // its state as the database holds them. A checkpoint whose commit fails writes none of it.
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let faults = std::sync::Arc::new(FailingCommits::default());
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            faults: faults.clone(),
            ..Options::default()
        },
    )?;
    exec(
        &path,
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-09T00:00:00Z');",
    )?;
    working(&store, &path).await?;
    handover_state(&path)?;
    store
        .submit_command(team_change("pi", 0, json!({"commander": {"agent": "pi"}})))
        .await?;
    let before = rows::<i64>(
        &path,
        "SELECT (SELECT count(*) FROM events), (SELECT count(*) FROM outbox)",
    )?;
    faults.0.store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        store
            .team_checkpoint("R1", crate::persistence::Availability::default())
            .await
            .is_err()
    );
    assert_eq!(
        binding(&path, "commander")?[..3],
        [json!("codex"), Value::Null, json!(1)]
    );
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT (SELECT count(*) FROM events), (SELECT count(*) FROM outbox)"
        )?,
        before,
        "a failed commit writes nothing"
    );
    faults.0.store(false, std::sync::atomic::Ordering::SeqCst);
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "commander")?,
        [
            json!("pi"),
            Value::Null,
            json!(2),
            json!(1),
            json!("ACTIVE"),
            Value::Null,
            Value::Null
        ]
    );
    let prepared = values(
        &path,
        "SELECT seq, payload_json FROM events WHERE kind = 'handoff.prepared'",
    )?;
    assert_eq!(prepared.len(), 1, "one handoff.prepared event");
    let notices = values(
        &path,
        "SELECT message_id, event_seq, recipient_role, role_epoch, state, payload_json FROM outbox \
         WHERE json_extract(payload_json, '$.kind') = 'session.start'",
    )?;
    assert_eq!(notices.len(), 1, "one session.start notice");
    let notice = &notices[0];
    assert_eq!(
        notice[1], prepared[0][0],
        "the notice points at the handoff event"
    );
    assert_eq!(
        notice[2..5],
        [json!("commander"), json!(2), json!("PENDING")]
    );
    let payload: Value = serde_json::from_str(notice[5].as_str().ok_or("payload")?)?;
    assert_eq!(payload["notice"], "runtime");
    assert_eq!(
        (&payload["role"], &payload["role_epoch"]),
        (&json!("commander"), &json!(2))
    );
    let package: crate::protocol::ContextHandoff = crate::protocol::decode(&payload["package"])?;
    assert_eq!(
        (
            package.goal.as_str(),
            package.spec_revision,
            package.spec_sha256.clone()
        ),
        ("g", 1, format!("{:064}", 1))
    );
    assert_eq!(package.candidate.map(|c| c.id).as_deref(), Some("C1"));
    let ids = |sql: &str| -> R<Vec<String>> {
        Ok(rows::<String>(&path, sql)?.into_iter().flatten().collect())
    };
    assert_eq!(
        package.state,
        Some(crate::protocol::HandoffState {
            applied_decision_ids: ids(
                "SELECT decision_id FROM decisions WHERE run_id = 'R1' AND state = 'APPLIED' ORDER BY 1"
            )?,
            open_issue_ids: ids(
                "SELECT issue_id FROM issues WHERE run_id = 'R1' AND state IN ('OPEN', 'REOPENED') ORDER BY 1"
            )?,
            unacked_message_ids: ids(&format!(
                "SELECT message_id FROM outbox WHERE run_id = 'R1' AND state <> 'ACKED' AND message_id <> '{}' \
                 ORDER BY 1",
                notice[0].as_str().ok_or("id")?
            ))?,
            queued_input_ids: ids(
                "SELECT input_id FROM input_queue WHERE run_id = 'R1' AND state = 'QUEUED' ORDER BY 1"
            )?,
        })
    );
    assert_eq!(
        package
            .state
            .as_ref()
            .map(|s| s.applied_decision_ids.clone()),
        Some(vec!["D1".to_owned(), "D2".to_owned()]),
        "the fixture's own"
    );
    Ok(())
}

#[tokio::test]
async fn a_commander_change_of_a_run_without_a_spec_is_refused_until_the_run_has_one() -> R {
    // ADR-024 decision 10: a role-change package names the run's spec, so a run with none keeps its commander (binding
    // and epoch) with one team.binding_refused, reason NO_SPEC, and no notice; later checkpoints add nothing while it
    // stays so; once the run has a spec the change applies with its package.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    store
        .submit_command(team_change("pi", 0, json!({"commander": {"agent": "pi"}})))
        .await?;
    let notices = |path: &PathBuf| -> R<i64> {
        Ok(rows::<i64>(
            path,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'session.start'",
        )?[0][0])
    };
    for _ in 0..2 {
        store
            .team_checkpoint("R1", crate::persistence::Availability::default())
            .await?;
        assert_eq!(
            binding(&path, "commander")?[..3],
            [json!("codex"), Value::Null, json!(1)]
        );
        assert_eq!(
            rows::<String>(
                &path,
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'team.binding_refused'"
            )?,
            [["NO_SPEC".to_owned()]]
        );
        assert_eq!(notices(&path)?, 0);
    }
    exec(
        &path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 7), 1);
         UPDATE runs SET spec_id = 's1' WHERE run_id = 'R1';",
    )?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "commander")?[..3],
        [json!("pi"), Value::Null, json!(2)]
    );
    assert_eq!(notices(&path)?, 1);
    Ok(())
}

#[tokio::test]
async fn a_session_start_goes_once_to_its_epochs_session_and_a_superseded_one_never() -> R {
    // ADR-024 decision 4: the runner sees a commander's session.start notice with its package and the binding's
    // epoch and session; it is delivered only to the session of the notice's own epoch, once (receipt, ACKED,
    // handoff.delivered); a notice whose epoch the binding has passed is never delivered, only superseded (receipt
    // rejected STALE_ROLE_EPOCH, ACKED, handoff.superseded).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    let change = |key: &str, expected: i64, agent: &str| {
        team_change(key, expected, json!({"commander": {"agent": agent}}))
    };
    let checkpoint = || store.team_checkpoint("R1", crate::persistence::Availability::default());
    store.submit_command(change("pi", 0, "pi")).await?;
    checkpoint().await?;
    let start = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .ok_or("no session start")?;
    assert_eq!(
        (
            start.role.as_str(),
            start.role_epoch,
            start.current_epoch,
            start.session_id.as_deref(),
            start.agent.as_str()
        ),
        ("commander", 2, 2, None, "pi")
    );
    assert_eq!(
        (start.package.role_epoch, start.package.run_id.as_str()),
        (2, "R1")
    );
    assert!(
        !store
            .deliver_session_start(&start.message_id, "R1/session/commander/2")
            .await?,
        "no session yet"
    );
    store
        .bind_session(crate::persistence::SessionGrant {
            run_id: "R1".to_owned(),
            role: "commander".to_owned(),
            role_epoch: 2,
            agent: "pi".to_owned(),
            agent_version: "1".to_owned(),
            capabilities: json!({}),
        })
        .await?;
    let start = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .ok_or("no session start")?;
    let session = start.session_id.clone().ok_or("no session")?;
    assert!(
        !store.supersede_session_start(&start.message_id).await?,
        "its epoch is the binding's"
    );
    assert!(
        !store
            .deliver_session_start(&start.message_id, "another")
            .await?
    );
    assert!(
        store
            .deliver_session_start(&start.message_id, &session)
            .await?
    );
    assert!(
        !store
            .deliver_session_start(&start.message_id, &session)
            .await?,
        "once"
    );
    assert!(store.runner_waiting("p1").await?.session_starts.is_empty());
    let settled = |id: &str| -> R<Vec<Vec<Value>>> {
        values(
            &path,
            &format!(
                "SELECT o.state, r.consumer, r.result_json, (SELECT group_concat(kind) FROM events \
                 WHERE kind IN ('handoff.delivered', 'handoff.superseded') \
                 AND json_extract(payload_json, '$.message_id') = o.message_id) \
                 FROM outbox o JOIN receipts r ON r.message_id = o.message_id WHERE o.message_id = '{id}'"
            ),
        )
    };
    let delivered = settled(&start.message_id)?;
    assert_eq!(
        delivered[0][..2],
        [json!("ACKED"), json!("runner")],
        "{delivered:?}"
    );
    assert_eq!(delivered[0][3], json!("handoff.delivered"));
    assert_eq!(
        serde_json::from_str::<Value>(delivered[0][2].as_str().ok_or("result")?)?["session_id"],
        json!(session)
    );
    // Two more changes before any delivery: the epoch-3 notice is passed by epoch 4.
    store.submit_command(change("claude", 1, "claude")).await?;
    checkpoint().await?;
    store.submit_command(change("codex", 2, "codex")).await?;
    checkpoint().await?;
    let starts = store.runner_waiting("p1").await?.session_starts;
    assert_eq!(
        starts
            .iter()
            .map(|s| (s.role_epoch, s.current_epoch))
            .collect::<Vec<_>>(),
        [(3, 4), (4, 4)]
    );
    let stale = &starts[0];
    assert!(
        !store
            .deliver_session_start(&stale.message_id, "any")
            .await?
    );
    // Not even to the session of the epoch that passed it: that session's first input is its own notice's package.
    store
        .bind_session(crate::persistence::SessionGrant {
            run_id: "R1".to_owned(),
            role: "commander".to_owned(),
            role_epoch: 4,
            agent: "codex".to_owned(),
            agent_version: "1".to_owned(),
            capabilities: json!({}),
        })
        .await?;
    let current = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .into_iter()
        .find(|s| s.role_epoch == 4)
        .and_then(|s| s.session_id)
        .ok_or("no epoch-4 session")?;
    assert!(
        !store
            .deliver_session_start(&stale.message_id, &current)
            .await?,
        "the epoch-3 notice delivered to the epoch-4 session"
    );
    assert!(store.supersede_session_start(&stale.message_id).await?);
    assert!(
        !store.supersede_session_start(&stale.message_id).await?,
        "once"
    );
    let superseded = settled(&stale.message_id)?;
    assert_eq!(superseded[0][0], json!("ACKED"));
    assert_eq!(superseded[0][3], json!("handoff.superseded"));
    assert_eq!(
        serde_json::from_str::<Value>(superseded[0][2].as_str().ok_or("result")?)?["rejected"],
        json!("STALE_ROLE_EPOCH")
    );
    Ok(())
}

#[tokio::test]
async fn a_change_to_an_agent_missing_its_auth_is_refused_blocked_auth_until_it_has_it() -> R {
    // REQ-019 failure mode 3 (ADR-024 decision 7): the registry found the commander's new agent installed but not
    // logged in; the change is refused (team.binding_refused, reason BLOCKED_AUTH, the old binding and epoch kept and
    // working); once the agent has its auth the change applies at epoch 2 with its handover.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change("pi", 0, json!({"commander": {"agent": "pi"}})))
        .await?;
    let logged_out = crate::persistence::Availability {
        auth_missing: [("pi".to_owned(), "run `pi auth login`".to_owned())].into(),
        ..crate::persistence::Availability::default()
    };
    for _ in 0..2 {
        store.team_checkpoint("R1", logged_out.clone()).await?;
        assert_eq!(
            binding(&path, "commander")?[..3],
            [json!("codex"), Value::Null, json!(1)]
        );
    }
    assert_eq!(
        rows::<String>(
            &path,
            "SELECT json_extract(payload_json, '$.reason') || '/' || json_extract(payload_json, '$.detail') \
             FROM events WHERE kind = 'team.binding_refused'"
        )?,
        [["BLOCKED_AUTH/run `pi auth login`".to_owned()]],
        "one refusal, its reason and what to do"
    );
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "commander")?[..3],
        [json!("pi"), Value::Null, json!(2)]
    );
    Ok(())
}

#[tokio::test]
async fn every_roles_change_to_a_new_holder_hands_the_role_over_with_its_package() -> R {
    // REQ-019 b2, b4 (docs/adr/ADR-024.md decision 1, M2T17R-03): not only the commander's: the developer's change
    // from claude to codex applies once A0 ended with, in its transaction, the binding at epoch 2, a handoff.prepared
    // event and one session.start notice to the developer at epoch 2 whose package is the developer's (its role, epoch
    // and output contract, the run's goal and spec), which the runner sees as the developer's session start.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    a0_ends(&path)?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[..3],
        [json!("codex"), Value::Null, json!(2)]
    );
    let prepared = values(
        &path,
        "SELECT seq, json_extract(payload_json, '$.role'), json_extract(payload_json, '$.role_epoch') FROM events \
         WHERE kind = 'handoff.prepared'",
    )?;
    assert_eq!(prepared.len(), 1, "{prepared:?}");
    assert_eq!(prepared[0][1..], [json!("developer"), json!(2)]);
    let notices = values(
        &path,
        "SELECT event_seq, recipient_role, role_epoch, payload_json FROM outbox \
         WHERE json_extract(payload_json, '$.kind') = 'session.start'",
    )?;
    assert_eq!(notices.len(), 1, "{notices:?}");
    assert_eq!(
        notices[0][..3],
        [prepared[0][0].clone(), json!("developer"), json!(2)]
    );
    let payload: Value = serde_json::from_str(notices[0][3].as_str().ok_or("payload")?)?;
    let package: crate::protocol::ContextHandoff = crate::protocol::decode(&payload["package"])?;
    assert_eq!(
        (
            package.role.as_str(),
            package.role_epoch,
            package.output_contract.as_str(),
            package.goal.as_str(),
            package.spec_revision
        ),
        ("developer", 2, "TaskResult", "g", 1)
    );
    let starts = store.runner_waiting("p1").await?.session_starts;
    assert_eq!(
        starts
            .iter()
            .map(|s| (s.role.as_str(), s.role_epoch, s.agent.as_str()))
            .collect::<Vec<_>>(),
        [("developer", 2, "codex")]
    );
    Ok(())
}

#[tokio::test]
async fn a_new_holder_is_dispatched_nothing_before_it_has_its_first_input() -> R {
    // REQ-019 b4 (docs/adr/ADR-024.md decision 4, M2T17R-03): the developer's new holder (codex at epoch 2) has its
    // session, but its session.start notice is not delivered yet: the round dispatches nothing to it, so no task reaches
    // it before its package; once the executor took the package, the developer's READY task is dispatched.
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
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    a0_ends(&path)?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    store
        .bind_session(crate::persistence::SessionGrant {
            run_id: "R1".to_owned(),
            role: "developer".to_owned(),
            role_epoch: 2,
            agent: "codex".to_owned(),
            agent_version: "1".to_owned(),
            capabilities: json!({"structured_events": true}),
        })
        .await?;
    store.pace_run("R1", context.clone()).await?;
    let round = store.schedule_round("R1", context.clone()).await?;
    assert!(
        round.dispatched.is_none(),
        "dispatched before its first input: {round:?}"
    );
    let start = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .ok_or("no session start")?;
    let session = start.session_id.clone().ok_or("no session")?;
    assert!(
        store
            .deliver_session_start(&start.message_id, &session)
            .await?
    );
    let round = store.schedule_round("R1", context).await?;
    assert!(round.dispatched.is_some(), "{round:?}");
    Ok(())
}

#[tokio::test]
async fn a_first_input_the_executor_did_not_take_is_recorded_once_and_shown_pending() -> R {
    // M2T17R-150 (docs/adr/ADR-024.md decision 4): while the new developer's first input is not delivered, the team page
    // shows the role waiting for it; the executor's failure to take it is recorded once, as handoff.undelivered with
    // what it said, however many ticks it fails; once delivered, nothing is pending and nothing more is recorded.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    a0_ends(&path)?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    let shown = |path: &PathBuf| -> R<bool> {
        let view = super::view(&Connection::open(path)?, "R1", "p1")?;
        Ok(view
            .members
            .iter()
            .find(|m| m.role == "developer")
            .ok_or("no developer")?
            .first_input_pending)
    };
    assert!(shown(&path)?, "waiting for its first input");
    let start = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .ok_or("no session start")?;
    assert!(
        store
            .session_start_undelivered(&start.message_id, "no hard links here")
            .await?
    );
    assert!(
        !store
            .session_start_undelivered(&start.message_id, "still none")
            .await?,
        "once"
    );
    assert_eq!(
        rows::<String>(
            &path,
            "SELECT json_extract(payload_json, '$.role') || '/' || json_extract(payload_json, '$.detail') FROM events \
             WHERE kind = 'handoff.undelivered'"
        )?,
        [["developer/no hard links here".to_owned()]]
    );
    store
        .bind_session(crate::persistence::SessionGrant {
            run_id: "R1".to_owned(),
            role: "developer".to_owned(),
            role_epoch: 2,
            agent: "codex".to_owned(),
            agent_version: "1".to_owned(),
            capabilities: json!({}),
        })
        .await?;
    let session = store
        .runner_waiting("p1")
        .await?
        .session_starts
        .pop()
        .and_then(|s| s.session_id)
        .ok_or("no session")?;
    assert!(
        store
            .deliver_session_start(&start.message_id, &session)
            .await?
    );
    assert!(!shown(&path)?, "delivered");
    assert!(
        !store
            .session_start_undelivered(&start.message_id, "late")
            .await?,
        "settled: nothing to record"
    );
    Ok(())
}

#[tokio::test]
async fn a_change_of_any_role_in_a_run_without_a_spec_is_refused_until_the_run_has_one() -> R {
    // docs/adr/ADR-024.md decision 10 for every role (M2T17R-03): with no spec no package can be made, so the
    // developer's change is refused NO_SPEC as the commander's is, the old binding working on; once the run has a spec
    // it applies with its package.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    store
        .submit_command(team_change(
            "dev",
            0,
            json!({"developer": {"agent": "codex"}}),
        ))
        .await?;
    for _ in 0..2 {
        store
            .team_checkpoint("R1", crate::persistence::Availability::default())
            .await?;
        assert_eq!(
            binding(&path, "developer")?[..3],
            [json!("claude"), Value::Null, json!(1)]
        );
    }
    assert_eq!(
        rows::<String>(
            &path,
            "SELECT json_extract(payload_json, '$.role') || '/' || json_extract(payload_json, '$.reason') FROM events \
             WHERE kind = 'team.binding_refused'"
        )?,
        [["developer/NO_SPEC".to_owned()]]
    );
    on_a_spec(&path)?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    assert_eq!(
        binding(&path, "developer")?[..3],
        [json!("codex"), Value::Null, json!(2)]
    );
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'session.start' \
             AND recipient_role = 'developer' AND role_epoch = 2"
        )?,
        [[1]]
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_change_is_recorded_again_when_its_reason_changes_and_only_then() -> R {
    // M2T17R-51 (REQ-019 failure mode 3): the commander's new agent is first not installed (AGENT_UNAVAILABLE), then
    // installed and not logged in (BLOCKED_AUTH): each new reason, or a new detail, is a refusal of its own, so the
    // record says what to do now; the same refusal at a later checkpoint writes nothing.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change("pi", 0, json!({"commander": {"agent": "pi"}})))
        .await?;
    let missing = crate::persistence::Availability {
        unavailable: [("pi".to_owned(), "not found on PATH".to_owned())].into(),
        ..crate::persistence::Availability::default()
    };
    let logged_out = |hint: &str| crate::persistence::Availability {
        auth_missing: [("pi".to_owned(), hint.to_owned())].into(),
        ..crate::persistence::Availability::default()
    };
    for availability in [
        missing.clone(),
        missing,
        logged_out("run `pi auth login`"),
        logged_out("run `pi auth login`"),
        logged_out("run `pi auth login --provider p`"),
    ] {
        store.team_checkpoint("R1", availability).await?;
        assert_eq!(
            binding(&path, "commander")?[..3],
            [json!("codex"), Value::Null, json!(1)]
        );
    }
    assert_eq!(
        rows::<String>(
            &path,
            "SELECT json_extract(payload_json, '$.reason') || '/' || json_extract(payload_json, '$.detail') \
             FROM events WHERE kind = 'team.binding_refused' ORDER BY seq"
        )?,
        [
            ["AGENT_UNAVAILABLE/not found on PATH".to_owned()],
            ["BLOCKED_AUTH/run `pi auth login`".to_owned()],
            ["BLOCKED_AUTH/run `pi auth login --provider p`".to_owned()],
        ],
        "one refusal per reason or detail seen, in order"
    );
    // The team page says why as the last refusal does, not AGENT_UNAVAILABLE whatever the reason.
    let view = super::view(&Connection::open(&path)?, "R1", "p1")?;
    let commander = view
        .members
        .into_iter()
        .find(|m| m.role == "commander")
        .ok_or("no commander")?;
    assert_eq!(
        commander.pending.ok_or("not pending")?["refused"],
        json!("BLOCKED_AUTH")
    );
    Ok(())
}

#[tokio::test]
async fn a_control_notice_to_the_commander_carries_its_epoch_after_a_handover() -> R {
    // ADR-024 decision 6: a run control's notice to the commander (PauseRun's run.control) is addressed to the
    // commander's binding as it is in the command's own transaction, epoch 2 after the handover, whatever epoch the
    // caller wrote (the host wrote 1).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    working(&store, &path).await?;
    store
        .submit_command(team_change("pi", 0, json!({"commander": {"agent": "pi"}})))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    let revision: i64 = rows::<i64>(&path, "SELECT revision FROM runs WHERE run_id = 'R1'")?[0][0];
    store
        .submit_command(SubmitCommand {
            command_id: "c-pause".to_owned(),
            idempotency_key: "k-pause".to_owned(),
            kind: "PauseRun".to_owned(),
            run_id: "R1".to_owned(),
            expected_revision: revision,
            payload: json!({}),
            payload_sha256: format!("{:064}", 9),
            target: RunTarget::To(crate::protocol::RunState::PauseRequested),
            event: NewEvent {
                project_id: "p1".to_owned(),
                event_id: "e-pause".to_owned(),
                run_id: Some("R1".to_owned()),
                kind: "run.pause_requested".to_owned(),
                payload: json!({"to": "PAUSE_REQUESTED"}),
            },
            outbox: Some(crate::persistence::NewOutbox {
                message_id: "m-pause".to_owned(),
                event: 0,
                run_id: Some("R1".to_owned()),
                recipient_role: "commander".to_owned(),
                role_epoch: 1,
                payload: json!({"notice": "runtime", "kind": "run.control", "control": "pause"}),
            }),
        })
        .await?;
    assert_eq!(
        rows::<i64>(
            &path,
            "SELECT role_epoch FROM outbox WHERE message_id = 'm-pause'"
        )?,
        [[2]]
    );
    Ok(())
}
