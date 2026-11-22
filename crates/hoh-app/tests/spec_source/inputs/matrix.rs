//! Generated intake matrix for ADR-036's single business-rejection boundary.
use super::*;
use hoh_core::lifecycle::{InputState, Lifecycle};
use hoh_core::persistence::{
    Change, CommandOutcome, Commit, Input, Intent, NewEvent, PersistError, RunTarget, StateCas,
    SubmitCommand,
};
use hoh_core::protocol::{AllVariants, RunState};

fn input_command(
    world: &World,
    run: &str,
    kind: &str,
    target: Option<&str>,
    text: &str,
) -> R<SubmitCommand> {
    let payload = json!({"text":text,"input_kind":kind,"target_role":target});
    Ok(SubmitCommand {
        command_id: "matrix-input".into(),
        idempotency_key: "matrix-input".into(),
        kind: "SendInput".into(),
        run_id: run.into(),
        expected_revision: world.query("SELECT revision FROM runs")?.parse()?,
        payload_sha256: specindex::digest(payload.to_string().as_bytes()),
        payload,
        target: RunTarget::Input(Input::Send {
            text: text.into(),
            target_role: target.map(str::to_owned),
            input_kind: kind.into(),
        }),
        event: NewEvent {
            intent: Intent::Progress,
            project_id: world.query("SELECT project_id FROM runs")?,
            run_id: Some(run.into()),
            event_id: "matrix-accepted".into(),
            kind: "input.accepted".into(),
            payload: json!({"command_id":"matrix-input"}),
        },
        outbox: None,
    })
}

// This expectation is an explicit behavioral oracle, not the product transition helper.
fn control_destination(state: RunState, verb: &str) -> Option<&'static str> {
    use RunState::*;
    match (verb, state) {
        (
            "pause",
            Created | Discovering | Specifying | Ready | Running | Verifying | WaitingExternal
            | Blocked,
        ) => Some("PAUSE_REQUESTED"),
        ("resume", Paused | Incomplete | WaitingExternal) => Some("RUNNING"),
        (
            "cancel",
            Created | Discovering | Specifying | Ready | Running | Verifying | WaitingExternal
            | Blocked | PauseRequested | Paused | Incomplete,
        ) => Some("CANCEL_REQUESTED"),
        _ => None,
    }
}

async fn cell(
    template_db: &std::path::Path,
    run: &str,
    state: RunState,
    kind: &str,
    target: Option<&str>,
    verb: &str,
) -> R {
    let world = World::new()?;
    let run = run.to_owned();
    // Each cell has a separate database. Copy one consistent, product-created
    // fixture snapshot instead of starting 360 identical background hosts.
    std::fs::create_dir_all(world.db().parent().ok_or("database parent")?)?;
    std::fs::copy(template_db, world.db())?;
    let state_name = state.as_str();
    world.exec(&format!("UPDATE runs SET state='{state_name}',resume_state={},blocked_reason={},blocked_reasons_json={} WHERE run_id='{run}'",
        if matches!(state, RunState::Created | RunState::Discovering | RunState::Specifying | RunState::Ready | RunState::Running | RunState::Verifying | RunState::Completed | RunState::Failed | RunState::Cancelled) { "NULL" } else { "'RUNNING'" },
        if state == RunState::Blocked { "'NEED_DECISION'" } else { "NULL" },
        if state == RunState::Blocked { "'[\"NEED_DECISION\"]'" } else { "'[]'" }))?;
    let store = open_store(&world)?;
    // Publish a real result at this fixture revision, including all terminal states.
    store
        .commit(Commit {
            cas: vec![StateCas {
                id: run.clone(),
                expected_revision: world.query("SELECT revision FROM runs")?.parse()?,
                change: Change::Touch(hoh_core::persistence::Entity::Run),
            }],
            ..Default::default()
        })
        .await?;
    let command = input_command(&world, &run, kind, target, verb)?;
    let before = snapshot(&world)?;
    let runs = world.rows("SELECT * FROM runs")?;
    let tasks = world.rows("SELECT * FROM tasks")?;
    let results = world.rows("SELECT * FROM events WHERE kind='run.result'")?;
    let inputs = world.rows("SELECT * FROM input_queue")?;
    let outcome = store.submit_command(command.clone()).await;
    if matches!(
        state,
        RunState::Completed | RunState::Failed | RunState::Cancelled
    ) {
        assert!(
            matches!(outcome, Err(PersistError::IllegalTransition { ref from, ref to, .. }) if from == state_name && to == "input"),
            "{outcome:?}"
        );
        assert_eq!(snapshot(&world)?, before);
        assert_eq!(world.rows("SELECT * FROM input_queue")?, inputs);
        return Ok(());
    }
    let CommandOutcome::Accepted {
        result_json,
        revision,
        ..
    } = outcome?
    else {
        return Err("fresh input was not accepted".into());
    };
    let id = hoh_core::ids::child_id(&command.command_id, "input");
    let row = store.reader()?.input(&id)?.ok_or("missing input")?;
    if kind == "CONTROL" {
        if let Some(destination) = control_destination(state, verb) {
            assert_eq!(row.state, InputState::Applied);
            assert_eq!(world.query("SELECT state FROM runs")?, destination);
            assert_eq!(revision, command.expected_revision + 1);
            assert_eq!(world.query("SELECT count(*) FROM commands WHERE kind IN ('PauseRun','ResumeRun','CancelRun')")?, "1");
        } else {
            assert_eq!(row.state, InputState::Rejected);
            let reason = if verb == "unknown" {
                "unknown_control"
            } else {
                "illegal_transition"
            };
            assert_eq!(row.payload["reason"], reason);
            if reason == "illegal_transition" {
                assert_eq!(row.payload["details"]["from"], state_name);
                assert_eq!(
                    row.payload["details"]["to"],
                    match verb {
                        "pause" => "PAUSE_REQUESTED",
                        "cancel" => "CANCEL_REQUESTED",
                        _ => "resume_state",
                    }
                );
                if state == RunState::Blocked && verb == "resume" {
                    assert_eq!(
                        row.payload["details"]["guard"]["reasons"],
                        json!(["NEED_DECISION"])
                    );
                }
            }
            assert_eq!(world.rows("SELECT * FROM runs")?, runs);
            assert_eq!(world.rows("SELECT * FROM tasks")?, tasks);
            assert_eq!(
                world.rows("SELECT * FROM events WHERE kind='run.result'")?,
                results
            );
            let records = world.rows(&format!(
                "SELECT * FROM events WHERE json_extract(payload_json,'$.input_id')='{id}'"
            ))?;
            assert_eq!(records.len(), 1, "rejection must not leave progress events");
            assert_eq!(records[0]["kind"], "input.rejected");
            let payload: Value =
                serde_json::from_str(records[0]["payload_json"].as_str().ok_or("record")?)?;
            assert_eq!(payload["reason"], reason);
            if reason == "illegal_transition" {
                assert_eq!(payload["details"], row.payload["details"]);
            }
            if state == RunState::Incomplete {
                assert!(records[0]["run_id"].is_null());
                assert_eq!(payload["run_id"], run);
            }
            assert_eq!(world.query("SELECT count(*) FROM commands WHERE kind IN ('PauseRun','ResumeRun','CancelRun')")?, "0");
        }
        assert_eq!(
            world.query(&format!("SELECT state FROM outbox WHERE message_id='{id}'"))?,
            "ACKED"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM receipts WHERE message_id='{id}'"
            ))?,
            "1"
        );
        let receipt: Value = serde_json::from_str(&world.query(&format!(
            "SELECT result_json FROM receipts WHERE message_id='{id}'"
        ))?)?;
        assert_eq!(receipt["reason"], row.payload["reason"]);
        assert_eq!(
            receipt["kind"],
            if row.state == InputState::Rejected {
                "rejected"
            } else {
                "ack"
            }
        );
    } else {
        assert_eq!(row.state, InputState::Queued);
        assert_eq!(row.payload["text"], verb);
        assert_eq!(world.query("SELECT state FROM runs")?, state_name);
        assert_eq!(world.rows("SELECT * FROM tasks")?, tasks);
        assert_eq!(
            world.query(
                "SELECT count(*) FROM commands WHERE kind IN ('PauseRun','ResumeRun','CancelRun')"
            )?,
            "0"
        );
        // Drive each queued cell to a final product disposition. A rebind rejects
        // role inputs even in a published stop; ending a run rejects pending CHANGE.
        if kind == "CHANGE" {
            world.exec("UPDATE runs SET state='CANCELLED',resume_state=NULL")?;
        } else {
            world.exec("UPDATE role_bindings SET role_epoch=role_epoch+1")?;
        }
        let final_run = world.rows("SELECT * FROM runs")?;
        let final_results = world.rows("SELECT * FROM events WHERE kind='run.result'")?;
        store
            .settle_ended_inputs(&world.query("SELECT project_id FROM runs")?)
            .await?;
        let row = store.reader()?.input(&id)?.ok_or("missing settled input")?;
        assert_eq!(row.state, InputState::Rejected);
        assert_eq!(
            row.payload["reason"],
            if kind == "CHANGE" {
                "run_ended"
            } else {
                "STALE_ROLE_EPOCH"
            }
        );
        assert_eq!(world.rows("SELECT * FROM runs")?, final_run);
        assert_eq!(world.rows("SELECT * FROM tasks")?, tasks);
        assert_eq!(
            world.rows("SELECT * FROM events WHERE kind='run.result'")?,
            final_results
        );
    }
    assert_eq!(world.query("SELECT count(*) FROM attempts")?, "0");
    let after = snapshot(&world)?;
    let inputs = world.rows("SELECT * FROM input_queue")?;
    let receipts = world.rows("SELECT * FROM receipts")?;
    assert_eq!(
        store.submit_command(command).await?,
        CommandOutcome::Replayed { result_json }
    );
    assert_eq!(snapshot(&world)?, after);
    assert_eq!(world.rows("SELECT * FROM input_queue")?, inputs);
    assert_eq!(world.rows("SELECT * FROM receipts")?, receipts);
    Ok(())
}

#[tokio::test]
async fn test_017_03_generated_input_kind_state_control_matrix() -> R {
    let template = World::new()?;
    let (host, run) = running(&template).await?;
    // Close the adoption question through the product. The separate stopped
    // boundary test covers REPLY answers; here plain text routes to Commander.
    send(&host, &run, "adopt-before-matrix", "REPLY", "adopt").await?;
    host.shutdown().await;
    let snapshot = template.project().join("matrix.db");
    template.exec(&format!("VACUUM INTO '{}'", snapshot.display()))?;
    let mut failures = Vec::new();
    let mut count = 0;
    for &state in RunState::ALL {
        for (kind, target) in [
            ("REPLY", None),
            ("REPLY", Some("developer")),
            ("ASK", None),
            ("STEER", Some("developer")),
            ("CHANGE", None),
            ("CONTROL", None),
        ] {
            for verb in ["pause", "resume", "cancel", "unknown"] {
                count += 1;
                if let Err(error) = cell(&snapshot, &run, state, kind, target, verb).await {
                    failures.push(format!("{state:?}/{kind}/{target:?}/{verb}: {error}"));
                }
            }
        }
    }
    println!("matrix: {count} cells, {} failures", failures.len());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    Ok(())
}
