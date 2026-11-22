//! DC-T10b: retry waits are not budget exhaustion; every default attempt slice can start work.
use super::*;

#[tokio::test]
async fn test_023_01_dc2_failed_holder_retries_when_due_without_incomplete() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-dc2-retry-")
        .tempdir()?;
    Host::start(config(dir.path())).await?.shutdown().await;
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(dir.path(), "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
        INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);", false)?;
    let reply = store
        .consult_request(message(
            "retry-holder",
            json!({"intent":"consult","question":"retry within my hold"}),
        )?)
        .await?
        .ok_or("consult")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    assert_eq!(
        events(dir.path(), "task.consult")?[0]["reserved_budget"]["wall_seconds"],
        10.0
    );
    seed_child_attempt(dir.path(), child, "failed-consult")?;
    let at = SystemTime::now();
    let mut ending = child_ending(Value::Null);
    ending.success = false;
    ending.at = at;
    ending.class = hoh_core::retry::FailureClass::Transient {
        retry_after: Some(Duration::from_secs(60)),
    };
    ending.observed = Default::default();
    store.runner_finished("failed-consult", ending).await?;
    assert_eq!(events(dir.path(), "task.retry_scheduled")?.len(), 1);
    sql(
        dir.path(),
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
        UPDATE tasks SET state='ACCEPTED' WHERE task_id='D';
        UPDATE budgets SET used_value=90 WHERE scope_kind='run';",
        false,
    )?;
    let context = |now| Context {
        now: Some(now),
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    };
    for n in 0..2 {
        let now = at + Duration::from_secs(n);
        let paced = store.pace_run("R", context(now)).await?;
        assert!(
            paced.incomplete.is_none(),
            "a retry wait is not exhaustion: {paced:?}"
        );
        if n == 0 {
            assert!(paced.waiting_until.is_some());
        }
        assert_eq!(
            sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
            "WAITING_EXTERNAL"
        );
        assert!(
            store
                .schedule_round("R", context(now))
                .await?
                .dispatched
                .is_none()
        );
    }
    drop(store);
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    let due = at + Duration::from_secs(61);
    assert!(
        store
            .pace_run("R", context(due))
            .await?
            .incomplete
            .is_none()
    );
    assert_eq!(
        store
            .schedule_round("R", context(due))
            .await?
            .dispatched
            .map(|d| d.task_id),
        Some(child.into())
    );
    assert!(
        store
            .pace_run("R", context(due))
            .await?
            .incomplete
            .is_none()
    );
    assert!(events(dir.path(), "run.incomplete")?.is_empty());
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT count(*) n FROM attempts WHERE task_id='{child}'"),
            true
        )?[0]["n"],
        2
    );
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc2_default_attempt_slice_has_a_dispatchable_minimum() -> R {
    for remaining in [1.0, 9.0, 10.0, 20.0, 0.0, 0.5] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-dc2-attempts-")
            .tempdir()?;
        Host::start(config(dir.path())).await?.shutdown().await;
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(dir.path(), &format!("UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
            INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,used_value,reserved_value) VALUES
            ('run','R','attempts',100,{},20), ('run','R','wall_seconds',100,0,0);", 80.0 - remaining), false)?;
        let request = message(
            "attempt-minimum",
            json!({"intent":"consult","question":"at least one attempt"}),
        )?;
        let reply = store
            .consult_request(request.clone())
            .await?
            .ok_or("reply")?;
        if remaining < 1.0 {
            assert_eq!(
                reply["status"], "rejected",
                "remaining={remaining}: no unstartable child: {reply}"
            );
            assert_eq!(reply["details"]["cause"], "attempt_budget_required");
            assert!(events(dir.path(), "task.consult")?.is_empty());
            assert!(store.reader()?.wait_for("R")?.is_empty());
            assert_eq!(
                sql(
                    dir.path(),
                    "SELECT count(*) n FROM budgets WHERE scope_kind='task'",
                    true
                )?[0]["n"],
                0
            );
            assert_eq!(
                sql(
                    dir.path(),
                    "SELECT reserved_value FROM budgets WHERE scope_kind='run' AND kind='attempts'",
                    true
                )?[0]["reserved_value"],
                20.0
            );
            continue;
        }
        assert_eq!(reply["status"], "queued");
        let child = reply["task_id"].as_str().ok_or("child")?;
        assert_eq!(store.consult_request(request).await?, Some(reply.clone()));
        let amount = events(dir.path(), "task.consult")?[0]["budget"]["attempts"]
            .as_f64()
            .ok_or("attempt slice")?;
        assert!(
            amount >= 1.0 && amount <= remaining,
            "slice={amount}, remaining={remaining}"
        );
        let context = Context {
            required_capabilities: [("researcher".into(), Default::default())].into(),
            ..Default::default()
        };
        assert_eq!(
            store
                .schedule_round("R", context)
                .await?
                .dispatched
                .map(|d| d.task_id),
            Some(child.into())
        );
        assert_eq!(
            sql(
                dir.path(),
                "SELECT reserved_value FROM budgets WHERE scope_kind='run' AND kind='attempts'",
                true
            )?[0]["reserved_value"],
            20.0 + amount
        );
    }
    Ok(())
}
