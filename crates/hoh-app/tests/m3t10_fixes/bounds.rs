//! Consultation reservations and both dispatch gates (M3K-50).
use super::*;

fn context() -> Context {
    Context {
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_023_01_default_consult_does_not_reserve_candidate_iterations() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-iterations-")
        .tempdir()?;
    Host::start(config(dir.path())).await?.shutdown().await;
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
        INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,used_value,price_known) VALUES
        ('run','R','iterations',10,0,1), ('run','R','wall_seconds',100,0,1),
        ('run','R','attempts',100,0,1), ('run','R','tokens',1000,0,1),
        ('run','R','usd',100,0,1);",
        false,
    )?;
    let reply = store
        .consult_request(message(
            "no-iterations",
            json!({
                "intent":"consult","question":"read-only research near the iteration limit"
            }),
        )?)
        .await?
        .ok_or("consult")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    sql(
        dir.path(),
        "UPDATE budgets SET used_value=9 WHERE scope_kind='run' AND kind='iterations'",
        false,
    )?;
    assert_eq!(
        sql(
            dir.path(),
            "SELECT used_value,reserved_value FROM budgets
        WHERE scope_kind='run' AND kind='iterations'",
            true
        )?,
        json!([{"used_value":9.0,"reserved_value":0.0}]),
        "a read-only consult cannot consume a candidate iteration"
    );
    assert_eq!(
        events(dir.path(), "task.consult")?[0]["reserved_budget"],
        json!({"wall_seconds":10.0,"attempts":10.0,"tokens":100.0,"usd":10.0})
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT count(*) n FROM budgets
        WHERE scope_kind='task' AND scope_id='{child}' AND kind='iterations'"
            ),
            true
        )?[0]["n"],
        0
    );
    assert_eq!(
        store
            .schedule_round("R", context())
            .await?
            .dispatched
            .map(|d| d.task_id),
        Some(child.into())
    );
    assert!(store.pace_run("R", context()).await?.incomplete.is_none());
    Ok(())
}

async fn dispatch_decision(
    store: &hoh_core::persistence::Store,
    root: &Path,
    task: &str,
    id: &str,
) -> R<hoh_core::persistence::Decided> {
    let revision = sql(root, "SELECT revision FROM runs WHERE run_id='R'", true)?[0]["revision"]
        .as_u64()
        .ok_or("revision")?;
    Ok(store
        .decide(
            hoh_core::decision::Proposal {
                decision_id: id.into(),
                run_id: "R".into(),
                role_epoch: 1,
                expected_revision: revision,
                spec_sha256: format!("{:064}", 1),
                candidate_id: None,
                causation_id: None,
                payload: serde_json::from_value(
                    json!({"kind":"dispatch","task_id":task,"role":"researcher"}),
                )?,
            },
            context(),
        )
        .await?)
}

#[tokio::test]
async fn test_023_01_consult_own_hold_allows_both_dispatch_paths_at_run_limit() -> R {
    for kind in ["wall_seconds", "usd"] {
        for commander in [false, true] {
            let dir = tempfile::Builder::new()
                .prefix("m3t10-own-hold-")
                .tempdir()?;
            Host::start(config(dir.path())).await?.shutdown().await;
            let store =
                hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
            sql(dir.path(), WORLD, false)?;
            sql(dir.path(), PARTICIPANTS, false)?;
            sql(
                dir.path(),
                &format!(
                    "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
                INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,reserved_value,price_known)
                VALUES ('run','R','{kind}',100,20,1);"
                ),
                false,
            )?;
            let reply = store
                .consult_request(message(
                    "own-hold",
                    json!({
                        "intent":"consult","question":"use my reserved headroom"
                    }),
                )?)
                .await?
                .ok_or("consult")?;
            let child = reply["task_id"].as_str().ok_or("child")?;
            // The consult owns 8; other reservations own 20. Other work now uses 72.
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=72 WHERE scope_kind='run';
                INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq,user_value)
                VALUES ('other','R','s1','READY','researcher',100,5);",
                false,
            )?;
            assert!(
                matches!(
                    dispatch_decision(&store, dir.path(), "other", "other-held").await?,
                    hoh_core::persistence::Decided::Rejected {
                        reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                        ..
                    }
                ),
                "other work cannot spend the consult's hold"
            );
            // Actual exhaustion plus somebody else's reservation still stops the holder.
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=80 WHERE scope_kind='run'",
                false,
            )?;
            assert!(
                store
                    .schedule_round("R", context())
                    .await?
                    .dispatched
                    .is_none()
            );
            assert!(matches!(
                dispatch_decision(&store, dir.path(), child, "own-exhausted").await?,
                hoh_core::persistence::Decided::Rejected {
                    reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                    ..
                }
            ));
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=72 WHERE scope_kind='run'",
                false,
            )?;
            // A consumed part of the slice is never credited again.
            let (used, reserved) = if kind == "wall_seconds" {
                (80, 24)
            } else {
                (76, 28)
            };
            sql(dir.path(), &format!("UPDATE budgets SET used_value=4 WHERE scope_kind='task' AND scope_id='{child}';
                UPDATE budgets SET used_value={used},reserved_value={reserved} WHERE scope_kind='run';"), false)?;
            assert!(
                store
                    .schedule_round("R", context())
                    .await?
                    .dispatched
                    .is_none()
            );
            assert!(
                matches!(
                    dispatch_decision(&store, dir.path(), child, "spent-hold").await?,
                    hoh_core::persistence::Decided::Rejected {
                        reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                        ..
                    }
                ),
                "{kind}: consumed allocation is not spendable again"
            );
            // Restore the unspent fixture and check independent Run gates.
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=0 WHERE scope_kind='task';
                UPDATE budgets SET used_value=72,reserved_value=28 WHERE scope_kind='run';
                UPDATE runs SET deadline_at='2020-01-01T00:00:00Z';",
                false,
            )?;
            assert!(
                store
                    .schedule_round("R", context())
                    .await?
                    .dispatched
                    .is_none()
            );
            assert!(matches!(
                dispatch_decision(&store, dir.path(), child, "deadline-hold").await?,
                hoh_core::persistence::Decided::Rejected {
                    reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                    ..
                }
            ));
            sql(
                dir.path(),
                "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'",
                false,
            )?;
            if kind == "usd" {
                sql(
                    dir.path(),
                    "UPDATE budgets SET price_known=0 WHERE scope_kind='run'",
                    false,
                )?;
                assert!(
                    store
                        .schedule_round("R", context())
                        .await?
                        .dispatched
                        .is_none()
                );
                assert!(matches!(
                    dispatch_decision(&store, dir.path(), child, "unknown-price-hold").await?,
                    hoh_core::persistence::Decided::Rejected {
                        reason: hoh_core::protocol::RejectionReason::BudgetNotReservable,
                        ..
                    }
                ));
                sql(
                    dir.path(),
                    "UPDATE budgets SET price_known=1 WHERE scope_kind='run'",
                    false,
                )?;
            }
            drop(store);
            let store =
                hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
            if commander {
                let decided = dispatch_decision(&store, dir.path(), child, "own-allowed").await?;
                assert!(
                    !matches!(decided, hoh_core::persistence::Decided::Rejected { .. }),
                    "{kind} holder dispatch: {decided:?}"
                );
            } else {
                assert_eq!(
                    store
                        .schedule_round("R", context())
                        .await?
                        .dispatched
                        .map(|d| d.task_id),
                    Some(child.into()),
                    "{kind}: used=72 plus reserved=28 must admit its owner"
                );
            }
            assert_eq!(
                sql(
                    dir.path(),
                    &format!("SELECT count(*) n FROM attempts WHERE task_id='{child}'"),
                    true
                )?[0]["n"],
                1
            );
            assert_eq!(
                sql(
                    dir.path(),
                    "SELECT used_value,reserved_value FROM budgets WHERE scope_kind='run'",
                    true
                )?,
                json!([{"used_value":72.0,"reserved_value":28.0}]),
                "admission does not release the hold"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_unused_consult_hold_does_not_end_run_before_dispatch() -> R {
    for kind in ["wall_seconds", "usd"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-hold-pace-")
            .tempdir()?;
        Host::start(config(dir.path())).await?.shutdown().await;
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(
            dir.path(),
            &format!(
                "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
            INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known)
            VALUES ('run','R','{kind}',100,1);"
            ),
            false,
        )?;
        store
            .consult_request(message(
                "pace-hold",
                json!({
                    "intent":"consult","question":"awaiting reserved work"
                }),
            )?)
            .await?
            .ok_or("consult")?;
        sql(
            dir.path(),
            "UPDATE budgets SET used_value=90 WHERE scope_kind='run';
            UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
            UPDATE tasks SET state='IMPLEMENTED' WHERE task_id='D';",
            false,
        )?;
        assert!(
            store.pace_run("R", context()).await?.incomplete.is_none(),
            "{kind}: the unspent hold cannot prematurely end the run"
        );
        sql(
            dir.path(),
            "UPDATE budgets SET used_value=100 WHERE scope_kind='run'",
            false,
        )?;
        assert!(
            store.pace_run("R", context()).await?.incomplete.is_some(),
            "{kind}: actual exhaustion still ends the run"
        );
    }
    Ok(())
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
    for stop in ["consult_deadline", "consult_wall_seconds"] {
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
        if stop == "consult_deadline" {
            sql(
                dir.path(),
                "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2026-01-01T00:00:00Z') WHERE kind='task.consult';",
                false,
            )?;
        } else {
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=10 WHERE scope_kind='task'; UPDATE budgets SET used_value=10,reserved_value=0 WHERE scope_kind='run';",
                false,
            )?;
        }
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
        assert!(
            events(dir.path(), "need_decision")?.is_empty(),
            "Runtime budget/deadline stops notify the parent without an AgentBlocked Commander wake"
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT blocked_reason FROM tasks WHERE task_id='{child}'"),
                true
            )?[0]["blocked_reason"],
            "NEED_DECISION"
        );
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
        assert_eq!(input["consultation_diagnostic"]["detail"], stop);
        assert_eq!(input["consultation_diagnostic"]["origin"], "runtime");
        assert!(
            input["text"]
                .as_str()
                .is_some_and(|s| s.contains("dismiss_consult") && s.contains(child)),
            "the delivered text must tell the parent how to end its stopped child"
        );
        assert_eq!(
            events(dir.path(), "task.consult_finished")?[0]["detail"],
            stop
        );
        assert!(input.get("source_message").is_none());
        drop(store);
    }
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

// DC-T10: two or three holders must not gate each other in the critical interval.
async fn critical_interval(holders: usize) -> R {
    for kind in ["wall_seconds", "usd"] {
        for commander in [false, true] {
            for offset in [0.0, 0.5, 0.99] {
                let dir = tempfile::Builder::new()
                    .prefix("m3t10-dc-holders-")
                    .tempdir()?;
                Host::start(config(dir.path())).await?.shutdown().await;
                let store =
                    hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
                sql(dir.path(), WORLD, false)?;
                sql(dir.path(), PARTICIPANTS, false)?;
                sql(
                    dir.path(),
                    &format!(
                        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
                    INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known)
                    VALUES ('run','R','{kind}',100,1);"
                    ),
                    false,
                )?;
                let mut children = Vec::new();
                for n in 0..holders {
                    let reply = store
                        .consult_request(message(
                            &format!("dc-{n}"),
                            json!({
                                "intent":"consult","question":"independent bounded research"
                            }),
                        )?)
                        .await?
                        .ok_or("consult")?;
                    children.push(reply["task_id"].as_str().ok_or("child")?.to_owned());
                }
                let held = sql(
                    dir.path(),
                    "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
                    true,
                )?[0]["reserved_value"]
                    .as_f64()
                    .ok_or("held")?;
                let low = 100.0 - (held - 10.0);
                let used = low + (100.0 - low) * offset;
                sql(
                    dir.path(),
                    &format!(
                        "UPDATE budgets SET used_value={used} WHERE scope_kind='run';
                    UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
                    UPDATE tasks SET state='IMPLEMENTED' WHERE task_id='D';"
                    ),
                    false,
                )?;
                drop(store);
                let store =
                    hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
                assert!(
                    store.pace_run("R", context()).await?.incomplete.is_none(),
                    "{holders}/{kind}/{used}: a holder is admissible"
                );
                if commander {
                    assert!(
                        !matches!(
                            dispatch_decision(&store, dir.path(), &children[0], "dc-dispatch")
                                .await?,
                            hoh_core::persistence::Decided::Rejected { .. }
                        ),
                        "{holders}/{kind}/{used}: Commander must admit a holder"
                    );
                } else {
                    let next = store.schedule_round("R", context()).await?.dispatched;
                    assert!(
                        next.as_ref().is_some_and(|d| children.contains(&d.task_id)),
                        "{holders}/{kind}/{used}: holders must not deadlock: {next:?}"
                    );
                }
                assert_eq!(
                    sql(
                        dir.path(),
                        "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
                        true
                    )?[0]["reserved_value"],
                    held,
                    "admission never releases the reservation"
                );
            }
        }
    }
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc_two_default_consults_in_critical_interval() -> R {
    critical_interval(2).await
}
#[tokio::test]
async fn test_023_01_dc_three_default_consults_in_critical_interval() -> R {
    critical_interval(3).await
}

#[tokio::test]
async fn test_023_01_dc_no_admissible_holder_stops_run() -> R {
    for stop in ["own_limit", "deadline", "ordinary_reservation"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-dc-no-holder-")
            .tempdir()?;
        Host::start(config(dir.path())).await?.shutdown().await;
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(dir.path(), "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
            INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);", false)?;
        for n in 0..2 {
            store
                .consult_request(message(
                    &format!("stopped-{n}"),
                    json!({"intent":"consult","question":"bounded"}),
                )?)
                .await?
                .ok_or("consult")?;
        }
        sql(
            dir.path(),
            "UPDATE budgets SET used_value=92 WHERE scope_kind='run';
            UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
            UPDATE tasks SET state='IMPLEMENTED' WHERE task_id='D';",
            false,
        )?;
        match stop {
            "own_limit" => {
                sql(
                    dir.path(),
                    "UPDATE budgets SET reserved_value=limit_value WHERE scope_kind='task'",
                    false,
                )?;
            }
            "deadline" => {
                sql(
                    dir.path(),
                    "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult'",
                    false,
                )?;
            }
            _ => {
                sql(
                    dir.path(),
                    "UPDATE budgets SET reserved_value=reserved_value+8 WHERE scope_kind='run'",
                    false,
                )?;
            }
        }
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none()
        );
        assert!(
            store.pace_run("R", context()).await?.incomplete.is_some(),
            "{stop}: a run that can admit nothing must stop"
        );
        assert_eq!(
            sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
            "INCOMPLETE"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc_parent_can_dismiss_only_its_stopped_consult() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-dc-dismiss-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(dir.path(), "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
        INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);", false)?;
    let reply = store
        .consult_request(message(
            "dismiss-child",
            json!({"intent":"consult","question":"bounded"}),
        )?)
        .await?
        .ok_or("child")?;
    let child = reply["task_id"].as_str().ok_or("child")?;
    let request =
        |id: &str, child: &str| message(id, json!({"intent":"dismiss_consult","task_id":child}));
    let early = store
        .consult_request(request("dismiss-early", child)?)
        .await?;
    assert!(
        early.as_ref().is_some_and(|v| v["status"] == "rejected"),
        "an active consult cannot be dismissed: {early:?}"
    );
    sql(
        dir.path(),
        "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult'",
        false,
    )?;
    store.settle_ended_inputs(&project).await?;
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT blocked_reason FROM tasks WHERE task_id='{child}'"),
            true
        )?[0]["blocked_reason"],
        "NEED_DECISION"
    );
    let mut stale = request("dismiss-stale", child)?;
    stale.role_epoch = 2;
    assert_eq!(
        store
            .consult_request(stale)
            .await?
            .ok_or("stale rejection")?["reason_code"],
        "STALE_ROLE_EPOCH"
    );
    assert_eq!(
        store
            .consult_request(request("dismiss-unowned", "D")?)
            .await?
            .ok_or("ownership rejection")?["status"],
        "rejected"
    );
    let mut old_spec = request("dismiss-spec", child)?;
    old_spec.spec_sha256 = format!("{:064}", 9);
    assert_eq!(
        store
            .consult_request(old_spec)
            .await?
            .ok_or("spec rejection")?["reason_code"],
        "SPEC_MISMATCH"
    );
    let reply = store
        .consult_request(request("dismiss-ok", child)?)
        .await?
        .ok_or("dismiss response")?;
    assert_eq!(reply["status"], "accepted");
    assert_eq!(
        store.consult_request(request("dismiss-ok", child)?).await?,
        Some(reply)
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state,blocked_reason FROM tasks WHERE task_id='{child}'"),
            true
        )?,
        json!([{"state":"CANCELLED","blocked_reason":null}])
    );
    assert_eq!(events(dir.path(), "task.consult_dismissed")?.len(), 1);
    assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
    assert!(events(dir.path(), "need_decision")?.is_empty());
    assert_eq!(
        sql(
            dir.path(),
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        0.0
    );
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc_stall_and_both_wake_gates_share_admission() -> R {
    for kind in ["wall_seconds", "usd"] {
        for holders in [2, 3] {
            let dir = tempfile::Builder::new()
                .prefix("m3t10-dc-wake-")
                .tempdir()?;
            let host = Host::start(config(dir.path())).await?;
            let project = host.project_id.clone();
            host.shutdown().await;
            let store =
                hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
            sql(dir.path(), WORLD, false)?;
            sql(dir.path(), PARTICIPANTS, false)?;
            sql(dir.path(), &format!("UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
                INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known) VALUES ('run','R','{kind}',100,1);"), false)?;
            for n in 0..holders {
                store
                    .consult_request(message(
                        &format!("wake-{n}"),
                        json!({"intent":"consult","question":"bounded"}),
                    )?)
                    .await?
                    .ok_or("child")?;
            }
            // In the Commander's stages the stall detector can reach condition (e) even with READY work.
            sql(
                dir.path(),
                "UPDATE budgets SET used_value=92 WHERE scope_kind='run';
                UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
                UPDATE tasks SET state='IMPLEMENTED' WHERE task_id='D';
                UPDATE runs SET state='SPECIFYING';",
                false,
            )?;
            assert!(store.pace_run("R", context()).await?.incomplete.is_none());
            assert_eq!(
                store
                    .detect_stall("R", Default::default(), context())
                    .await?,
                hoh_core::persistence::Detected::Retriggered(1),
                "{holders}/{kind}: condition (e) must use the pace's admission answer"
            );
            let now = SystemTime::now() + Duration::from_secs(30);
            let ctx = Context {
                now: Some(now),
                ..context()
            };
            store.supervise("R", ctx.clone()).await?;
            assert_eq!(
                events(dir.path(), "supervisor.woken")?.len(),
                1,
                "{holders}/{kind}: could_allocate must not suppress a wake while a consult can proceed"
            );
            assert!(
                store.allocate_supervision(&project, ctx).await?.is_some(),
                "{holders}/{kind}: wake allocation must use that same answer"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc_release_never_erases_consumed_budget() -> R {
    for kind in ["usd", "tokens", "attempts"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-dc-consumed-")
            .tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        let project = host.project_id.clone();
        host.shutdown().await;
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(dir.path(), &format!("UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
            INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known) VALUES ('run','R','{kind}',100,1);"), false)?;
        store
            .consult_request(message(
                "spent",
                json!({"intent":"consult","question":"bounded"}),
            )?)
            .await?
            .ok_or("child")?;
        sql(dir.path(), "UPDATE budgets SET used_value=95 WHERE scope_kind='run';
            UPDATE budgets SET used_value=5 WHERE scope_kind='task';
            UPDATE attempts SET state='SETTLED' WHERE attempt_id='A';
            UPDATE tasks SET state='IMPLEMENTED' WHERE task_id='D';
            UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult';", false)?;
        store.settle_ended_inputs(&project).await?;
        assert_eq!(
            sql(
                dir.path(),
                "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
                true
            )?[0]["reserved_value"],
            0.0
        );
        drop(store);
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        assert!(
            store.pace_run("R", context()).await?.incomplete.is_some(),
            "{kind}: releasing a hold must not erase its consumption"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc_admission_reserves_the_next_attempt_unit() -> R {
    for scope in ["run", "task"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-dc-estimate-")
            .tempdir()?;
        Host::start(config(dir.path())).await?.shutdown().await;
        let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(dir.path(), "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
            INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','attempts',100);", false)?;
        let reply = store
            .consult_request(message(
                "estimate",
                json!({"intent":"consult","question":"bounded"}),
            )?)
            .await?
            .ok_or("child")?;
        let task = reply["task_id"].as_str().ok_or("child")?;
        sql(
            dir.path(),
            &format!(
                "UPDATE budgets SET reserved_value=reserved_value+0.5,used_value=limit_value-1 WHERE scope_kind='{scope}';"
            ),
            false,
        )?;
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none(),
            "{scope}: the estimate is one attempt, not zero"
        );
        assert!(matches!(
            dispatch_decision(&store, dir.path(), task, "estimate-reject").await?,
            hoh_core::persistence::Decided::Rejected { .. }
        ));
        // Exactly one remaining attempt is admissible.
        sql(
            dir.path(),
            &format!(
                "UPDATE budgets SET reserved_value=reserved_value-0.5 WHERE scope_kind='{scope}'"
            ),
            false,
        )?;
        assert_eq!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .map(|d| d.task_id),
            Some(task.into())
        );
    }
    Ok(())
}
