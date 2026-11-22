//! Consultation reservations and both dispatch gates (M3K-50).
use super::*;

fn context() -> Context {
    Context {
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_023_01_default_slices_reserve_and_release_run_headroom() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-reserve-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let _project_id = host.project_id.clone();
    host.shutdown().await;
    // Drive the real Store explicitly so the host sweeper cannot race the gate under test.
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,used_value,reserved_value) VALUES ('run','R','wall_seconds',1000,100,100);",
        false,
    )?;
    let mut children = Vec::new();
    for (n, limit, reserved) in [(1, 80.0, 180.0), (2, 72.0, 252.0)] {
        let request = message(
            &format!("slice-{n}"),
            json!({"intent":"consult","question":"bounded research"}),
        )?;
        let reply = store
            .consult_request(request.clone())
            .await?
            .ok_or("consult")?;
        let task = reply["task_id"].as_str().ok_or("task")?.to_owned();
        assert_eq!(store.consult_request(request).await?, Some(reply));
        assert_eq!(
            sql(
                dir.path(),
                "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
                true
            )?[0]["reserved_value"],
            reserved
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!(
                    "SELECT limit_value FROM budgets WHERE scope_kind='task' AND scope_id='{task}'"
                ),
                true
            )?[0]["limit_value"],
            limit
        );
        children.push(task);
    }
    // Finishing one child releases exactly its allocation, leaving pre-existing and sibling reservations.
    seed_child_attempt(dir.path(), &children[0], "spent")?;
    sql(
        dir.path(),
        "UPDATE attempts SET started_at='2026-11-22T00:00:00Z' WHERE attempt_id='spent'",
        false,
    )?;
    let mut ending = child_ending(child_result(&children[0], "spent", "COMPLETED"));
    ending.at = hoh_core::time::parse_rfc3339("2026-11-22T00:00:30Z").ok_or("time")?;
    store.runner_finished("spent", ending).await?;
    store.settle_ended_inputs(&_project_id).await?;
    let run = sql(
        dir.path(),
        "SELECT used_value,reserved_value FROM budgets WHERE scope_kind='run'",
        true,
    )?;
    assert_eq!(run, json!([{"used_value":130.0,"reserved_value":172.0}]));
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT used_value FROM budgets WHERE scope_kind='task' AND scope_id='{}'",
                children[0]
            ),
            true
        )?[0]["used_value"],
        30.0
    );
    drop(store);
    Ok(())
}

#[tokio::test]
async fn test_023_01_task_limits_and_deadline_gate_both_dispatch_paths() -> R {
    for bound in [
        "wall_seconds",
        "attempts",
        "tokens",
        "iterations",
        "usd",
        "deadline",
    ] {
        let dir = tempfile::Builder::new().prefix("m3t10-bounds-").tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        let _project_id = host.project_id.clone();
        host.shutdown().await;
        // Drive the real Store explicitly so the host sweeper cannot race the gate under test.
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        let request = message(
            "bounded",
            json!({"intent":"consult","question":"bounded research","budget":{if bound == "deadline" {"wall_seconds"} else {bound}:10},"deadline":"2098-01-01T00:00:00Z"}),
        )?;
        let reply = store.consult_request(request).await?.ok_or("consult")?;
        let child = reply["task_id"].as_str().ok_or("child")?;
        // Known prices here isolate limit exhaustion from unenforceable pricing.
        sql(
            dir.path(),
            &format!(
                "UPDATE budgets SET used_value=10,price_known=1 WHERE scope_kind='task' AND scope_id='{child}';"
            ),
            false,
        )?;
        let mut at = context();
        if bound == "deadline" {
            sql(dir.path(), "UPDATE budgets SET used_value=0", false)?;
            at.now = hoh_core::time::parse_rfc3339("2098-01-01T00:00:00Z");
            at.wall = at.now;
        }
        assert!(
            store
                .schedule_round("R", at.clone())
                .await?
                .dispatched
                .is_none(),
            "{bound} must stop automatic dispatch"
        );
        let revision = sql(
            dir.path(),
            "SELECT revision FROM runs WHERE run_id='R'",
            true,
        )?[0]["revision"]
            .as_u64()
            .ok_or("revision")?;
        let proposal = hoh_core::decision::Proposal {
            decision_id: format!("dispatch-{bound}"),
            run_id: "R".into(),
            role_epoch: 1,
            expected_revision: revision,
            spec_sha256: format!("{:064}", 1),
            candidate_id: None,
            causation_id: None,
            payload: serde_json::from_value(
                json!({"kind":"dispatch","task_id":child,"role":"researcher"}),
            )?,
        };
        let decided = store.decide(proposal, at).await?;
        assert!(
            matches!(
                decided,
                hoh_core::persistence::Decided::Rejected {
                    reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                    ..
                }
            ),
            "{bound}: {decided:?}"
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT count(*) n FROM attempts WHERE task_id='{child}'"),
                true
            )?[0]["n"],
            0
        );
        // Retire the negative fixture before resetting the injected deadline clock.
        sql(
            dir.path(),
            &format!("UPDATE tasks SET state='CANCELLED' WHERE task_id='{child}'"),
            false,
        )?;
        // An independent consultation below the bound remains eligible; a stopped one is not reopened.
        let allowed = store.consult_request(message("allowed",json!({"intent":"consult","question":"within budget","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}))?).await?.ok_or("allowed")?;
        let allowed = allowed["task_id"].as_str().ok_or("allowed child")?;
        sql(
            dir.path(),
            &format!(
                "UPDATE budgets SET used_value=9 WHERE scope_kind='task' AND scope_id='{allowed}'"
            ),
            false,
        )?;
        assert_eq!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .ok_or("positive control did not dispatch")?
                .task_id,
            allowed
        );
        drop(store);
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_spent_attempts_and_running_time_count_toward_task_limits() -> R {
    for kind in ["attempts", "wall_seconds", "tokens", "usd"] {
        let dir = tempfile::Builder::new().prefix("m3t10-spent-").tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        let _project_id = host.project_id.clone();
        host.shutdown().await;
        // Drive the real Store explicitly so the host sweeper cannot race the gate under test.
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        let reply = store.consult_request(message("spent",json!({"intent":"consult","question":"count actual work","budget":{kind:1},"deadline":"2099-01-01T00:00:00Z"}))?).await?.ok_or("child")?;
        let child = reply["task_id"].as_str().ok_or("child")?;
        seed_child_attempt(dir.path(), child, "old")?;
        sql(
            dir.path(),
            &format!(
                "UPDATE budgets SET price_known=1; UPDATE tasks SET state='READY' WHERE task_id='{child}'; UPDATE attempts SET state='FAILED',started_at='2026-11-22T00:00:00Z',usage_json='{{\"input_tokens\":1,\"output_tokens\":1}}',cost_usd=1 WHERE attempt_id='old';"
            ),
            false,
        )?;
        if kind == "wall_seconds" {
            sql(
                dir.path(),
                "UPDATE attempts SET state='RUNNING' WHERE attempt_id='old';",
                false,
            )?;
        }
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none(),
            "{kind} actual usage"
        );
        drop(store);
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_exhausted_consultation_returns_a_diagnostic_and_releases_reservation() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-exhausted-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let _project_id = host.project_id.clone();
    host.shutdown().await;
    // Drive the real Store explicitly so the host sweeper cannot race the gate under test.
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);",
        false,
    )?;
    let reply = store
        .consult_request(message(
            "expired",
            json!({"intent":"consult","question":"expires before dispatch"}),
        )?)
        .await?
        .ok_or("child")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    sql(
        dir.path(),
        "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2026-01-01T00:00:00Z') WHERE kind='task.consult';",
        false,
    )?;
    for _ in 0..2 {
        store.settle_ended_inputs(&_project_id).await?;
    }
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
            true
        )?[0]["state"],
        "BLOCKED"
    );
    assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
    assert_eq!(
        sql(
            dir.path(),
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        0.0
    );
    let input: Value = serde_json::from_str(
        sql(
            dir.path(),
            "SELECT payload_json FROM input_queue WHERE target_role='developer'",
            true,
        )?[0]["payload_json"]
            .as_str()
            .ok_or("input")?,
    )?;
    assert_eq!(
        input["consultation_diagnostic"]["reason"],
        "budget_exhausted"
    );
    assert!(input.get("source_message").is_none());
    drop(store);
    Ok(())
}

#[tokio::test]
async fn test_023_01_running_default_allocation_is_not_reserved_twice() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-reserved-time-")
        .tempdir()?;
    Host::start(config(dir.path())).await?.shutdown().await;
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);",
        false,
    )?;
    let reply = store
        .consult_request(message(
            "reserved-time",
            json!({"intent":"consult","question":"already allocated"}),
        )?)
        .await?
        .ok_or("child")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    seed_child_attempt(dir.path(), child, "inflight")?;
    sql(
        dir.path(),
        "UPDATE attempts SET started_at='2026-11-22T00:00:00Z' WHERE attempt_id='inflight'; UPDATE budgets SET used_value=86 WHERE scope_kind='run'; INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('other','R','s1','READY','researcher',100);",
        false,
    )?;
    let at = Context {
        now: hoh_core::time::parse_rfc3339("2026-11-22T00:00:05Z"),
        ..context()
    };
    assert_eq!(
        store
            .schedule_round("R", at)
            .await?
            .dispatched
            .map(|d| d.task_id),
        Some("other".into()),
        "86 spent + 10 allocated leaves room; the in-flight 5 is already inside the 10"
    );
    Ok(())
}

#[tokio::test]
async fn test_023_01_late_child_end_does_not_release_another_reservation() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-late-budget-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,reserved_value) VALUES ('run','R','wall_seconds',100,20);",
        false,
    )?;
    let reply = store
        .consult_request(message(
            "late",
            json!({"intent":"consult","question":"cancelled parent"}),
        )?)
        .await?
        .ok_or("child")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    seed_child_attempt(dir.path(), child, "late-child")?;
    sql(
        dir.path(),
        "UPDATE attempts SET started_at='2026-11-22T00:00:00Z' WHERE attempt_id='late-child'; UPDATE tasks SET state='CANCELLED' WHERE task_id='D'",
        false,
    )?;
    store.settle_ended_inputs(&project).await?;
    assert_eq!(
        sql(
            dir.path(),
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        20.0
    );
    let mut ending = child_ending(child_result(child, "late-child", "COMPLETED"));
    ending.at = hoh_core::time::parse_rfc3339("2026-11-22T00:00:05Z").ok_or("time")?;
    store.runner_finished("late-child", ending).await?;
    store.settle_ended_inputs(&project).await?;
    assert_eq!(
        sql(
            dir.path(),
            "SELECT used_value,reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?,
        json!([{"used_value":5.0,"reserved_value":20.0}])
    );
    assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
    Ok(())
}
