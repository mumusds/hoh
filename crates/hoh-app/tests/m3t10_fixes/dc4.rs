//! DC-T10d: all consult outcomes close atomically without an owner question.
use super::*;
use hoh_core::persistence::{Change, Commit, Decided, PersistError, StateCas, Store};
use hoh_core::protocol::{DecisionPayload, TaskState};

async fn setup() -> R<(tempfile::TempDir, Store, String, String)> {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-dc4-")
        .tempdir_in("/home/ai/dev")?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE sessions SET capabilities_json='{\"mid_turn_steer\":true}'; UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);",
        false,
    )?;
    let reply = store
        .consult_request(message(
            "dc4",
            json!({"intent":"consult","question":"return every outcome"}),
        )?)
        .await?
        .ok_or("consult")?;
    let child = reply["task_id"].as_str().ok_or("child")?.to_owned();
    Ok((dir, store, project, child))
}
fn context() -> Context {
    Context {
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    }
}
fn closed(root: &Path, store: &Store, child: &str, status: &str) -> R {
    assert_eq!(
        sql(
            root,
            &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
            true
        )?[0]["state"],
        if status == "CANCELLED" {
            "CANCELLED"
        } else {
            "IMPLEMENTED"
        }
    );
    let ended = events(root, "task.consult_finished")?;
    assert_eq!(ended.len(), 1, "exactly one consult end");
    assert_eq!(ended[0]["status"], status);
    assert!(store.reader()?.wait_for("R")?.is_empty());
    assert_eq!(
        sql(
            root,
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        0.0
    );
    assert_eq!(
        sql(root, "SELECT count(*) n FROM input_queue", true)?[0]["n"],
        1
    );
    assert!(
        !store
            .reader()?
            .run_projection("R")?
            .ok_or("run")?
            .5
            .iter()
            .any(|q| q.task_id.as_deref() == Some(child)),
        "consults never ask the owner"
    );
    store.reader()?.check_question_anchors()?;
    Ok(())
}
async fn delivered(root: &Path, store: &Store, child: &str, status: &str) -> R {
    let id = hoh_core::ids::child_id(child, "consult/result");
    let input = super::returned::stage(store, root, &id)
        .await?
        .steer
        .ok_or("parent input")?;
    let result = if status == "CANCELLED" {
        &input.payload["consultation_diagnostic"]
    } else {
        &input.payload["source_message"]["payload"]
    };
    assert_eq!(result["status"], status);
    if status != "CANCELLED" {
        assert_eq!(result["summary"], "research outcome");
    }
    store
        .route_input(hoh_core::persistence::InputRouting::Finish {
            input_id: id,
            attempt: "A".into(),
            outcome: hoh_core::persistence::InputOutcome::Delivered,
        })
        .await?;
    Ok(())
}

async fn unknown(restart: bool) -> R {
    let (dir, store, project, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    let store = if restart {
        drop(store);
        Store::open(&db(dir.path()), config(dir.path()).store)?
    } else {
        store
    };
    if restart {
        let row = store
            .reconcile_scan(&project, SystemTime::now())
            .await?
            .into_iter()
            .find(|r| r.attempt_id == "C")
            .ok_or("unfinished")?;
        store
            .reconcile_apply(
                row,
                hoh_core::reconcile::Disposition::UnknownEffect {
                    effects: vec![],
                    unknown: vec!["unconfirmed".into()],
                },
                hoh_core::reconcile::Process::Gone {
                    basis: "scripted restart observation".into(),
                },
                "reconcile",
                SystemTime::now(),
                0,
            )
            .await?;
    } else {
        let mut end = child_ending(Value::Null);
        end.success = false;
        end.unknown_effect = true;
        end.observed = Default::default();
        store.runner_finished("C", end).await?;
    }
    closed(dir.path(), &store, &child, "CANCELLED")?;
    assert_eq!(
        events(dir.path(), "task.consult_finished")?[0]["reason"],
        "unknown_effect"
    );
    delivered(dir.path(), &store, &child, "CANCELLED").await?;
    for _ in 0..2 {
        store.pace_run("R", context()).await?;
        store.settle_ended_inputs(&project).await?;
    }
    closed(dir.path(), &store, &child, "CANCELLED")?;
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc4_m3k500_runner_unknown_ends_consult() -> R {
    unknown(false).await
}
#[tokio::test]
async fn test_023_01_dc4_m3k500_reconcile_unknown_ends_consult() -> R {
    unknown(true).await
}

async fn result(status: &str, success: bool) -> R {
    let (dir, store, project, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    let mut end = child_ending(child_result(&child, "C", status));
    end.success = success;
    store.runner_finished("C", end.clone()).await?;
    closed(dir.path(), &store, &child, status)?;
    delivered(dir.path(), &store, &child, status).await?;
    assert!(
        store.runner_finished("C", end).await.is_err(),
        "a duplicate runner end is rejected"
    );
    closed(dir.path(), &store, &child, status)?;
    drop(store);
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    store.settle_ended_inputs(&project).await?;
    closed(dir.path(), &store, &child, status)?;
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc4_m3k501_failed_result_reaches_parent() -> R {
    result("FAILED", true).await
}
#[tokio::test]
async fn test_023_01_dc4_m3k501_blocked_result_reaches_parent() -> R {
    result("BLOCKED", true).await
}
#[tokio::test]
async fn test_023_01_dc4_m3k501_incomplete_result_reaches_parent() -> R {
    result("INCOMPLETE", true).await
}
#[tokio::test]
async fn test_023_01_dc4_task_result_survives_unsuccessful_exit() -> R {
    result("FAILED", false).await
}

#[tokio::test]
async fn test_023_01_dc4_m3k550_exhaustion_has_no_cancel_question_before_pace() -> R {
    let (dir, store, project, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    sql(
        dir.path(),
        &format!(
            "UPDATE tasks SET attempt_count={} WHERE task_id='{child}'; UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D'; UPDATE budgets SET used_value=90 WHERE scope_kind='run';",
            hoh_core::retry::MAX_RETRIES
        ),
        false,
    )?;
    let mut end = child_ending(Value::Null);
    end.success = false;
    end.observed = Default::default();
    end.class = hoh_core::retry::FailureClass::Transient { retry_after: None };
    store.runner_finished("C", end).await?;
    // The owner's cancel answer cannot exist: no child question is created, even before sweeping.
    closed(dir.path(), &store, &child, "CANCELLED")?;
    assert!(events(dir.path(), "need_decision")?.is_empty());
    store.pace_run("R", context()).await?;
    store.settle_ended_inputs(&project).await?;
    closed(dir.path(), &store, &child, "CANCELLED")?;
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc4_deadline_pace_before_sweep_releases_hold() -> R {
    let (dir, store, _, child) = setup().await?;
    sql(
        dir.path(),
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D'; UPDATE budgets SET used_value=90 WHERE scope_kind='run'; UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult';",
        false,
    )?;
    store.pace_run("R", context()).await?;
    closed(dir.path(), &store, &child, "CANCELLED")?;
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc4_m3k502_finished_child_rejects_commander_and_direct_moves() -> R {
    let (dir, store, _, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    store
        .runner_finished("C", child_ending(child_result(&child, "C", "COMPLETED")))
        .await?;
    closed(dir.path(), &store, &child, "COMPLETED")?;
    let revision = sql(
        dir.path(),
        "SELECT revision FROM runs WHERE run_id='R'",
        true,
    )?[0]["revision"]
        .as_u64()
        .ok_or("revision")?;
    let proposal = hoh_core::decision::Proposal {
        decision_id: "reopen-child".into(),
        run_id: "R".into(),
        role_epoch: 1,
        expected_revision: revision,
        spec_sha256: format!("{:064}", 1),
        candidate_id: None,
        causation_id: None,
        payload: DecisionPayload::TransitionTask {
            task_id: child.clone(),
            to_state: TaskState::Ready,
            reason: Some("try again".into()),
        },
    };
    let decided = store.decide(proposal, context()).await?;
    assert!(
        matches!(
            decided,
            Decided::Rejected {
                reason: hoh_core::protocol::RejectionReason::IllegalTransition,
                ..
            }
        ),
        "{decided:?}"
    );
    for to in [TaskState::Ready, TaskState::Cancelled, TaskState::Blocked] {
        let revision = sql(
            dir.path(),
            &format!("SELECT revision FROM tasks WHERE task_id='{child}'"),
            true,
        )?[0]["revision"]
            .as_i64()
            .ok_or("revision")?;
        let moved = store
            .commit(Commit {
                cas: vec![StateCas {
                    id: child.clone(),
                    expected_revision: revision,
                    change: Change::Task(to),
                }],
                ..Default::default()
            })
            .await;
        assert!(
            matches!(moved, Err(PersistError::IllegalTransition { .. })),
            "{moved:?}"
        );
    }
    assert!(
        store
            .schedule_round("R", context())
            .await?
            .dispatched
            .is_none()
    );
    closed(dir.path(), &store, &child, "COMPLETED")?;
    Ok(())
}
#[tokio::test]
async fn test_023_01_dc4_invariant_rejects_terminal_open_consult() -> R {
    for state in ["IMPLEMENTED", "CANCELLED", "ACCEPTED"] {
        let (dir, store, _, child) = setup().await?;
        store.reader()?.check_question_anchors()?;
        sql(
            dir.path(),
            &format!("UPDATE tasks SET state='{state}' WHERE task_id='{child}'"),
            false,
        )?;
        assert!(
            matches!(store.reader()?.check_question_anchors(), Err(PersistError::IntegrityViolation(s)) if s.contains("consult")),
            "open consult with terminal {state}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc4_terminal_transaction_rolls_back_and_retries_once() -> R {
    let (dir, store, project, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    sql(
        dir.path(),
        "CREATE TRIGGER fail_consult_end BEFORE INSERT ON events WHEN NEW.kind='task.consult_finished' BEGIN SELECT RAISE(ABORT,'injected consult end failure'); END",
        false,
    )?;
    let end = child_ending(child_result(&child, "C", "BLOCKED"));
    assert!(store.runner_finished("C", end.clone()).await.is_err());
    assert!(events(dir.path(), "task.consult_finished")?.is_empty());
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
            true
        )?[0]["state"],
        "RUNNING"
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT state FROM attempts WHERE attempt_id='C'",
            true
        )?[0]["state"],
        "RUNNING"
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        10.0
    );
    assert_eq!(
        sql(dir.path(), "SELECT count(*) n FROM input_queue", true)?[0]["n"],
        0
    );
    assert_eq!(store.reader()?.wait_for("R")?.len(), 1);
    sql(dir.path(), "DROP TRIGGER fail_consult_end", false)?;
    store.runner_finished("C", end).await?;
    store.settle_ended_inputs(&project).await?;
    closed(dir.path(), &store, &child, "BLOCKED")?;
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc4_runtime_failure_classes_retry_without_owner_question() -> R {
    use hoh_core::retry::FailureClass;
    for class in [
        FailureClass::Auth,
        FailureClass::Capability,
        FailureClass::Model,
        FailureClass::Unavailable,
        FailureClass::Compat,
        FailureClass::Decision,
        FailureClass::Transient {
            retry_after: Some(Duration::from_secs(60)),
        },
    ] {
        let (dir, store, project, child) = setup().await?;
        seed_child_attempt(dir.path(), &child, "C")?;
        let mut end = child_ending(Value::Null);
        end.success = false;
        end.class = class;
        end.observed = Default::default();
        store.runner_finished("C", end).await?;
        assert_eq!(events(dir.path(), "task.retry_scheduled")?.len(), 1);
        assert!(events(dir.path(), "need_decision")?.is_empty());
        assert_eq!(store.reader()?.wait_for("R")?.len(), 1);
        assert!(events(dir.path(), "task.consult_finished")?.is_empty());
        // Independent task deadline bounds the wait even if the capability cannot recover.
        sql(
            dir.path(),
            "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult'",
            false,
        )?;
        store.pace_run("R", context()).await?;
        store.settle_ended_inputs(&project).await?;
        closed(dir.path(), &store, &child, "CANCELLED")?;
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc4_cancel_run_preserves_finished_consult() -> R {
    let (dir, store, _, child) = setup().await?;
    seed_child_attempt(dir.path(), &child, "C")?;
    store
        .runner_finished("C", child_ending(child_result(&child, "C", "COMPLETED")))
        .await?;
    let before = sql(
        dir.path(),
        &format!("SELECT * FROM tasks WHERE task_id='{child}'"),
        true,
    )?;
    sql(
        dir.path(),
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE runs SET state='CANCEL_REQUESTED' WHERE run_id='R'",
        false,
    )?;
    store
        .finish_cancel("R", json!({}), Default::default(), SystemTime::now())
        .await?;
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT * FROM tasks WHERE task_id='{child}'"),
            true
        )?,
        before
    );
    assert_eq!(
        sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
        "CANCELLED"
    );
    store.reader()?.check_question_anchors()?;
    Ok(())
}
