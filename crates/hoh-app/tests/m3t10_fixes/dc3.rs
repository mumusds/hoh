//! DC-T10c: a recoverable block must not orphan a consultation result.
use super::*;
use hoh_core::persistence::{InputOutcome, InputRouting, Store};
use sha2::{Digest as _, Sha256};

fn context() -> Context {
    Context {
        required_capabilities: [("researcher".into(), Default::default())].into(),
        ..Default::default()
    }
}

async fn answer_retry(store: &Store, root: &Path, project: &str, child: &str) -> R {
    let question = store
        .reader()?
        .run_projection("R")?
        .ok_or("run")?
        .5
        .into_iter()
        .find(|q| q.task_id.as_deref() == Some(child))
        .ok_or("retry question")?;
    let revision = sql(root, "SELECT revision FROM runs WHERE run_id='R'", true)?[0]["revision"]
        .as_i64()
        .ok_or("revision")?;
    let id = format!("retry-answer/{}", question.question_id);
    let result = store
        .submit_command(hoh_core::persistence::SubmitCommand {
            command_id: id.clone(),
            idempotency_key: id.clone(),
            kind: "AnswerQuestion".into(),
            run_id: "R".into(),
            expected_revision: revision,
            payload: json!({}),
            payload_sha256: Sha256::digest(id.as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            target: hoh_core::persistence::RunTarget::Input(hoh_core::persistence::Input::Answer {
                question_id: question.question_id,
                answer: "retry the consultation".into(),
                // The existing work-question API calls its retry choice `continue`.
                choice_id: Some("continue".into()),
            }),
            event: hoh_core::persistence::NewEvent {
                intent: hoh_core::persistence::Intent::Progress,
                project_id: project.into(),
                event_id: format!("{id}/accepted"),
                run_id: Some("R".into()),
                kind: "question.answer_accepted".into(),
                payload: json!({}),
            },
            outbox: None,
        })
        .await?;
    assert!(
        matches!(
            result,
            hoh_core::persistence::CommandOutcome::Accepted { .. }
        ),
        "{result:?}"
    );
    Ok(())
}

async fn blocked_consult(block: &str, expire: bool) -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-dc3-blocked-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(dir.path(), "UPDATE sessions SET capabilities_json='{\"mid_turn_steer\":true}';
        UPDATE runs SET deadline_at='2099-01-01T00:00:00Z';
        INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',100);", false)?;
    let reply = store
        .consult_request(message(
            "dc3-blocked",
            json!({"intent":"consult","question":"preserve the eventual answer"}),
        )?)
        .await?
        .ok_or("child")?;
    let child = reply["task_id"].as_str().ok_or("child")?.to_owned();
    if block == "vacancy" {
        // The binding becomes vacant after request acceptance, before the first dispatch.
        sql(
            dir.path(),
            "UPDATE role_bindings SET state='BLOCKED',blocked_reason='BLOCKED_AUTH' WHERE role='researcher'",
            false,
        )?;
        let paced = store.pace_run("R", context()).await?;
        assert_eq!(paced.vacant, vec![child.clone()]);
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none()
        );
    } else {
        seed_child_attempt(dir.path(), &child, "exhausted")?;
        sql(
            dir.path(),
            &format!(
                "UPDATE tasks SET attempt_count={} WHERE task_id='{child}'",
                hoh_core::retry::MAX_RETRIES
            ),
            false,
        )?;
        let mut ending = child_ending(child_result(&child, "exhausted", "BLOCKED"));
        if block == "retry" {
            ending.success = false;
            ending.class = hoh_core::retry::FailureClass::Transient { retry_after: None };
            ending.observed = Default::default();
        }
        store.runner_finished("exhausted", ending).await?;
        assert!(
            store
                .reader()?
                .run_projection("R")?
                .ok_or("run")?
                .5
                .iter()
                .any(|q| q.task_id.as_deref() == Some(&child))
        );
    }
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
            true
        )?[0]["state"],
        "BLOCKED"
    );
    store.reader()?.check_question_anchors()?;
    // Both the attempt-ending transaction and repeated input sweeps must preserve the wait.
    for _ in 0..2 {
        store.settle_ended_inputs(&project).await?;
    }
    if !expire {
        assert!(
            events(dir.path(), "task.consult_finished")?.is_empty(),
            "a recoverable block is not a consult outcome"
        );
        assert_eq!(store.reader()?.wait_for("R")?.len(), 1);
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
    }
    drop(store);
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    if expire {
        sql(
            dir.path(),
            "UPDATE events SET payload_json=json_set(payload_json,'$.deadline','2020-01-01T00:00:00Z') WHERE kind='task.consult'",
            false,
        )?;
        store.settle_ended_inputs(&project).await?;
        assert_eq!(
            sql(
                dir.path(),
                &format!(
                    "SELECT state,blocked_reason,next_retry_at FROM tasks WHERE task_id='{child}'"
                ),
                true
            )?,
            json!([{"state":"CANCELLED","blocked_reason":null,"next_retry_at":null}])
        );
    }
    if block == "vacancy" {
        sql(
            dir.path(),
            "UPDATE role_bindings SET state='ACTIVE',blocked_reason=NULL WHERE role='researcher'",
            false,
        )?;
        let paced = store.pace_run("R", context()).await?;
        assert_eq!(
            paced.recovered,
            if expire { vec![] } else { vec![child.clone()] }
        );
    } else if !expire {
        answer_retry(&store, dir.path(), &project, &child).await?;
    } else {
        assert!(
            !store
                .reader()?
                .run_projection("R")?
                .ok_or("run")?
                .5
                .iter()
                .any(|q| q.task_id.as_deref() == Some(&child))
        );
    }
    if !expire {
        if block == "agent_question" {
            let dispatch = store
                .schedule_round("R", context())
                .await?
                .dispatched
                .ok_or("second question dispatch")?;
            sql(
                dir.path(),
                &format!(
                    "UPDATE tasks SET state='RUNNING' WHERE task_id='{child}'; UPDATE attempts SET state='RUNNING' WHERE attempt_id='{}'",
                    dispatch.attempt_id
                ),
                false,
            )?;
            store
                .runner_finished(
                    &dispatch.attempt_id,
                    child_ending(child_result(&child, &dispatch.attempt_id, "BLOCKED")),
                )
                .await?;
            store.settle_ended_inputs(&project).await?;
            assert!(events(dir.path(), "task.consult_finished")?.is_empty());
            store.reader()?.check_question_anchors()?;
            answer_retry(&store, dir.path(), &project, &child).await?;
        }
        let dispatch = store
            .schedule_round("R", context())
            .await?
            .dispatched
            .ok_or("recovered dispatch")?;
        assert_eq!(dispatch.task_id, child);
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none(),
            "only one recovery dispatch"
        );
        // Script the external Agent at the existing runner completion boundary.
        sql(
            dir.path(),
            &format!(
                "UPDATE tasks SET state='RUNNING' WHERE task_id='{child}'; UPDATE attempts SET state='RUNNING' WHERE attempt_id='{}'",
                dispatch.attempt_id
            ),
            false,
        )?;
        store
            .runner_finished(
                &dispatch.attempt_id,
                child_ending(child_result(&child, &dispatch.attempt_id, "COMPLETED")),
            )
            .await?;
    }
    store.settle_ended_inputs(&project).await?;
    store.reader()?.check_question_anchors()?;
    if !expire {
        // IMPLEMENTED alone or a claimed COMPLETED event is insufficient for the invariant audit.
        sql(
            dir.path(),
            "UPDATE events SET payload_json=json_set(payload_json,'$.truncated',json('true')) WHERE kind='attempt.outputs'",
            false,
        )?;
        assert!(
            matches!(store.reader()?.check_question_anchors(), Err(hoh_core::persistence::PersistError::IntegrityViolation(s)) if s.contains("consult"))
        );
        sql(
            dir.path(),
            "UPDATE events SET payload_json=json_set(payload_json,'$.truncated',json('false')) WHERE kind='attempt.outputs'",
            false,
        )?;
        store.reader()?.check_question_anchors()?;
    }
    let finished = events(dir.path(), "task.consult_finished")?;
    assert_eq!(finished.len(), 1);
    assert_eq!(
        finished[0]["status"],
        if expire { "CANCELLED" } else { "COMPLETED" }
    );
    assert!(store.reader()?.wait_for("R")?.is_empty());
    assert_eq!(
        sql(
            dir.path(),
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        0.0
    );
    // Use the M3-T09 turn routing and receipt boundary, inspecting exactly what the parent receives.
    let input = hoh_core::ids::child_id(&child, "consult/result");
    let delivered = super::returned::stage(&store, dir.path(), &input)
        .await?
        .steer
        .ok_or("parent steer")?;
    assert_eq!(delivered.target_role.as_deref(), Some("developer"));
    if expire {
        assert_eq!(
            delivered.payload["consultation_diagnostic"]["status"],
            "CANCELLED"
        );
        assert_eq!(
            delivered.payload["consultation_diagnostic"]["detail"],
            "consult_deadline"
        );
        assert_eq!(
            delivered.payload["consultation_diagnostic"]["correlation_id"],
            "dc3-blocked"
        );
    } else {
        assert_eq!(delivered.payload["source_message"]["kind"], "result");
        assert_eq!(
            delivered.payload["source_message"]["correlation_id"],
            "dc3-blocked"
        );
        assert_eq!(
            delivered.payload["source_message"]["payload"]["status"],
            "COMPLETED"
        );
        assert_eq!(
            delivered.payload["source_message"]["payload"]["summary"],
            "research outcome"
        );
    }
    store
        .route_input(InputRouting::Finish {
            input_id: input.clone(),
            attempt: "A".into(),
            outcome: InputOutcome::Delivered,
        })
        .await?;
    store
        .move_attempt(hoh_core::persistence::AttemptMove {
            attempt_id: "A".into(),
            from: hoh_core::lifecycle::AttemptState::Running,
            to: hoh_core::lifecycle::AttemptState::Settled,
            facts: Default::default(),
        })
        .await?;
    assert_eq!(
        store.reader()?.input(&input)?.ok_or("input")?.state,
        hoh_core::lifecycle::InputState::Applied
    );
    let attempts = sql(
        dir.path(),
        &format!("SELECT count(*) n FROM attempts WHERE task_id='{child}'"),
        true,
    )?[0]["n"]
        .clone();
    assert_eq!(
        attempts,
        json!(
            usize::from(block != "vacancy")
                + usize::from(!expire)
                + usize::from(block == "agent_question")
        )
    );
    drop(store);
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    for _ in 0..2 {
        store.settle_ended_inputs(&project).await?;
        store.pace_run("R", context()).await?;
        assert!(
            store
                .schedule_round("R", context())
                .await?
                .dispatched
                .is_none()
        );
        store.reader()?.check_question_anchors()?;
    }
    assert_eq!(events(dir.path(), "task.consult_finished")?, finished);
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT count(*) n FROM attempts WHERE task_id='{child}'"),
            true
        )?[0]["n"],
        attempts
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT count(*) n FROM receipts WHERE message_id='{input}'"),
            true
        )?[0]["n"],
        1
    );
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc3_vacancy_recovery_returns_completed_result_once() -> R {
    blocked_consult("vacancy", false).await
}
#[tokio::test]
async fn test_023_01_dc3_retry_question_recovery_returns_completed_result_once() -> R {
    blocked_consult("retry", false).await
}
#[tokio::test]
async fn test_023_01_dc3_deadline_while_blocked_cancels_delivers_and_never_revives() -> R {
    for block in ["vacancy", "retry"] {
        blocked_consult(block, true).await?;
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_dc3_repeated_agent_questions_keep_distinct_open_anchors() -> R {
    blocked_consult("agent_question", false).await
}
