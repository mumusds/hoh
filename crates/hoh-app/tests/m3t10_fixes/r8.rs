//! M3K-700: withdrawing an unstarted attempt spends neither the consult nor the Run budget.
use super::*;
use hoh_core::persistence::{Change, Commit, StateCas, Store};
use hoh_core::protocol::RunState;

fn context() -> Context {
    Context {
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    }
}

async fn withdrawn(restart: bool, run_limit: bool) -> R {
    let dir = tempfile::Builder::new().prefix("m3t10-r8-").tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    // The Run-only case leaves the child room for two attempts so its own limit cannot mask the Run count.
    let attempts = if run_limit { 2 } else { 1 };
    sql(
        dir.path(),
        &format!(
            "INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','attempts',{});",
            if run_limit { 1 } else { 10 }
        ),
        false,
    )?;
    let reply = store
        .consult_request(message(
            "withdrawn-budget",
            json!({"intent":"consult","question":"withdrawals are not work","budget":{"attempts":attempts},"deadline":"2099-01-01T00:00:00Z"}),
        )?)
        .await?
        .ok_or("consult")?;
    let child = reply["task_id"].as_str().ok_or("child")?.to_owned();
    sql(
        dir.path(),
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D';",
        false,
    )?;
    let first = store
        .schedule_round("R", context())
        .await?
        .dispatched
        .ok_or("initial child dispatch")?;
    assert_eq!(first.task_id, child);
    let store = if restart {
        sql(
            dir.path(),
            &format!(
                "UPDATE leases SET expires_at='2020-01-01T00:00:00Z' WHERE attempt_id='{}'",
                first.attempt_id
            ),
            false,
        )?;
        drop(store);
        let host = Host::start(config(dir.path())).await?;
        host.shutdown().await;
        Store::open(&db(dir.path()), config(dir.path()).store)?
    } else {
        let revision = sql(
            dir.path(),
            "SELECT revision FROM runs WHERE run_id='R'",
            true,
        )?[0]["revision"]
            .as_i64()
            .ok_or("revision")?;
        store
            .commit(Commit {
                cas: vec![StateCas {
                    id: "R".into(),
                    expected_revision: revision,
                    change: Change::Run(RunState::PauseRequested),
                }],
                ..Default::default()
            })
            .await?;
        store.pause_step("R", SystemTime::now()).await?;
        store
    };
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT state,started_at,cancel_requested FROM attempts WHERE attempt_id='{}'",
                first.attempt_id
            ),
            true,
        )?[0],
        json!({"state":"CANCELLED","started_at":null,"cancel_requested":0})
    );
    store.settle_ended_inputs(&project).await?;
    assert!(
        events(dir.path(), "task.consult_finished")?.is_empty(),
        "a withdrawal must not exhaust the {} attempts budget",
        if run_limit { "Run" } else { "consult" }
    );
    assert_eq!(store.reader()?.wait_for("R")?.len(), 1);
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state,attempt_count FROM tasks WHERE task_id='{child}'"),
            true,
        )?[0],
        json!({"state":"READY","attempt_count":0})
    );
    if !restart {
        assert_eq!(
            sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
            "PAUSED"
        );
        let revision = sql(
            dir.path(),
            "SELECT revision FROM runs WHERE run_id='R'",
            true,
        )?[0]["revision"]
            .as_i64()
            .ok_or("revision")?;
        store
            .commit(Commit {
                cas: vec![StateCas {
                    id: "R".into(),
                    expected_revision: revision,
                    change: Change::Run(RunState::Running),
                }],
                ..Default::default()
            })
            .await?;
    }
    assert!(store.pace_run("R", context()).await?.incomplete.is_none());
    let next = store
        .schedule_round("R", context())
        .await?
        .dispatched
        .ok_or("child redispatched without spending the withdrawal")?;
    assert_eq!(next.task_id, child);
    assert_ne!(next.attempt_id, first.attempt_id);
    assert!(events(dir.path(), "task.consult_finished")?.is_empty());
    store.reader()?.check_question_anchors()?;
    Ok(())
}

#[tokio::test]
async fn test_023_01_m3k700_pause_preserves_one_attempt_consult_budget() -> R {
    withdrawn(false, false).await
}

#[tokio::test]
async fn test_023_01_m3k700_restart_preserves_one_attempt_consult_budget() -> R {
    withdrawn(true, false).await
}

#[tokio::test]
async fn test_023_01_m3k700_pause_does_not_spend_run_attempts() -> R {
    withdrawn(false, true).await
}

#[tokio::test]
async fn test_023_01_m3k700_restart_does_not_spend_run_attempts() -> R {
    withdrawn(true, true).await
}
