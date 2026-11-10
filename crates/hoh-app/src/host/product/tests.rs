//! The product executor's pure parts: the prompt it sends, how a turn's facts become the attempt's outcome.

use super::*;
use hoh_core::persistence::Assignment;

fn work(prompt: Option<&str>) -> Work {
    Work {
        project_id: "p1".to_owned(),
        run_id: "R1".to_owned(),
        task_id: "T1".to_owned(),
        attempt_id: "R1/T1/a1".to_owned(),
        role: "developer".to_owned(),
        effect_key: "k".to_owned(),
        assignment: Assignment {
            agent: Some("claude".to_owned()),
            model: None,
            role_epoch: 1,
            config_revision: 0,
            session_id: Some("R1/session/developer/1".to_owned()),
            native_session: None,
            agent_version: Some("2.1.318".to_owned()),
            prompt: prompt.map(str::to_owned),
            goal: Some("Ship it".to_owned()),
            title: Some("Write the parser".to_owned()),
        },
    }
}

#[test]
fn a_turn_is_sent_its_roles_contract_then_the_tasks_header() {
    // ADR-022 decision 11: the contract, then the run's goal, the task's id and title, and the role.
    let text = prompt_text("Contract.\n\n", None, &work(None));
    assert!(text.starts_with("Contract.\n\n---\n"), "{text}");
    for part in ["Ship it", "T1 Write the parser", "developer"] {
        assert!(text.contains(part), "{part}: {text}");
    }
    assert!(!text.contains("交接包"), "{text}");
}

#[test]
fn a_sessions_first_turn_is_sent_its_handover_package_after_the_contract_and_before_the_task()
-> Result<(), Box<dyn std::error::Error>> {
    // REQ-019 b4 (docs/adr/ADR-024.md decision 4, M2T17R-03): a new holder's first turn has its kept package, whole,
    // between its role's contract and the task's header.
    let package =
        serde_json::json!({"handoff_kind": "role_change", "role": "developer", "run_id": "R9"});
    let text = prompt_text("Contract.\n\n", Some(&package), &work(None));
    let (contract, package_at, task) = (
        text.find("Contract.").ok_or("no contract")?,
        text.find("\"run_id\": \"R9\"").ok_or("no package")?,
        text.find("# 本次任务").ok_or("no task")?,
    );
    assert!(contract < package_at && package_at < task, "{text}");
    let block = text
        .split("```json\n")
        .nth(1)
        .and_then(|rest| rest.split("\n```").next())
        .ok_or("no package block")?;
    assert_eq!(serde_json::from_str::<serde_json::Value>(block)?, package);
    Ok(())
}

#[test]
fn a_kept_first_input_is_read_for_its_session_and_none_is_none()
-> Result<(), Box<dyn std::error::Error>> {
    // Decision 4: the package kept for a session is its first turn's; a session given none (bound at its run's start)
    // has none; a kept input that is no JSON is an error, not taken as none.
    let dir = tempfile::tempdir()?;
    assert_eq!(first_input(dir.path(), "R/session/developer/2")?, None);
    let path = first_input_path(dir.path(), "R/session/developer/2");
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    std::fs::write(
        &path,
        br#"{"message_id": "m", "session_id": "R/session/developer/2", "package": {"role": "developer"}}"#,
    )?;
    assert_eq!(
        first_input(dir.path(), "R/session/developer/2")?,
        Some(serde_json::json!({"role": "developer"}))
    );
    std::fs::write(&path, b"{")?;
    assert!(first_input(dir.path(), "R/session/developer/2").is_err());
    Ok(())
}

#[test]
fn an_adapters_failure_code_fails_its_attempt_by_its_class() {
    // Decision 16 (ADR-020 decision 1).
    assert_eq!(class_of(Some("BLOCKED_AUTH")), FailureClass::Auth);
    assert_eq!(
        class_of(Some("BLOCKED_CAPABILITY")),
        FailureClass::Capability
    );
    assert_eq!(
        class_of(Some("MODEL_UNAVAILABLE")),
        FailureClass::Capability
    );
    for code in [
        Some("PROTOCOL_FRAME_TOO_LARGE"),
        Some("CAPABILITY_UNSUPPORTED"),
        Some("TRANSIENT_EXTERNAL"),
        None,
    ] {
        assert_eq!(
            class_of(code),
            FailureClass::Transient { retry_after: None },
            "{code:?}"
        );
    }
}

#[test]
fn a_pi_session_id_is_uuid_shaped_and_the_same_for_its_hoh_session() {
    // ADR-013 decision 1: HoH chooses it and keeps it for every turn.
    let id = pi_session_id("R1/session/developer/1");
    assert_eq!(id, pi_session_id("R1/session/developer/1"));
    assert_ne!(id, pi_session_id("R1/session/developer/2"));
    let parts: Vec<usize> = id.split('-').map(str::len).collect();
    assert_eq!(parts, [8, 4, 4, 4, 12], "{id}");
    assert!(id.chars().all(|c| c == '-' || c.is_ascii_hexdigit()));
    assert_eq!(component("a/b%c"), "a%2Fb%25c");
    assert_ne!(component("a/b"), component("a%2Fb"));
}

#[test]
fn a_turns_facts_become_its_outcome_with_the_prompt_it_was_sent() {
    // Decision 12: the verdict, the failure code as the reason and its class, the group's end, and what the turn showed
    // (events in order, the agent's own session, model, usage, the raw logs, the prompt record).
    let prompt = PromptRecord {
        layer: "builtin".to_owned(),
        role: "developer".to_owned(),
        path: "resources/prompts/developer.md".to_owned(),
        content_sha256: "c".repeat(64),
        sent_sha256: "s".repeat(64),
    };
    let facts = Facts {
        state: AttemptState::Failed,
        failure: Some("PROTOCOL_FRAME_TOO_LARGE".to_owned()),
        native_session: None,
        resolved_model: Some("m".to_owned()),
        usage: None,
        raw_log_path: "/logs".to_owned(),
        max_buffered: Some(10),
        group_reaped: true,
        recorded: vec![
            Recorded::NativeSession("n-1".to_owned()),
            Recorded::Event {
                key: "k".to_owned(),
                kind: "adapter.diagnostic".to_owned(),
                payload: serde_json::json!({"reason": "PROTOCOL_FRAME_TOO_LARGE"}),
            },
        ],
        degraded: Some("initialize timed out".to_owned()),
    };
    let o = outcome(facts, prompt.clone());
    assert!(!o.success);
    assert_eq!(o.detail, "PROTOCOL_FRAME_TOO_LARGE");
    assert!(o.group_gone);
    assert_eq!(o.class, FailureClass::Transient { retry_after: None });
    assert_eq!(o.observed.native_session.as_deref(), Some("n-1"));
    assert_eq!(o.observed.resolved_model.as_deref(), Some("m"));
    assert_eq!(o.observed.raw_log_path.as_deref(), Some("/logs"));
    assert_eq!(o.observed.prompt, Some(prompt));
    // A degraded App Server's event is the adapter's own, recorded once (M2T07R-201).
    let kinds: Vec<&str> = o.observed.events.iter().map(|e| e.1.as_str()).collect();
    assert_eq!(kinds, ["adapter.diagnostic"]);
    assert_eq!(o.observed.events[0].2["reader_max_buffered"], 10);
}

#[test]
fn a_native_cancel_says_whether_it_reached_a_turn_the_executor_holds() {
    // M2T07R-453 (docs/adr/ADR-022.md decision 17): the cancel reaches only a turn still held: one the executor holds
    // (its notifier registered) is asked and the answer is true; once the turn ended (or for work it never started)
    // nothing is asked and the answer is false, so the settle waits the SIGTERM grace and records no native cancel.
    let executor = ProductExecutor::new(
        Arc::new(Registry::new(std::ffi::OsString::new())),
        PathBuf::from("/nonexistent"),
        PathBuf::from("hoh"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let work = work(None);
    assert!(
        !executor.cancel(&work),
        "no turn held, yet the cancel was sent"
    );
    let notify = Arc::new(tokio::sync::Notify::new());
    if let Ok(mut cancels) = executor.cancels.lock() {
        cancels.insert(work.attempt_id.clone(), notify.clone());
    }
    assert!(executor.cancel(&work));
    if let Ok(mut cancels) = executor.cancels.lock() {
        cancels.remove(&work.attempt_id);
    }
    assert!(!executor.cancel(&work));
}

#[test]
fn a_first_input_is_kept_once_whole_and_another_is_refused()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-024.md decision 4: the package is kept at its session's place; the same notice given again (an
    // answer lost in a crash) is taken; another input for the same session is refused, the first one kept; no staged
    // file is left.
    let project = tempfile::tempdir()?;
    let package = hoh_core::handoff::role_change(&hoh_core::handoff::RoleChange {
        role: "commander".to_owned(),
        run_id: "R".to_owned(),
        goal: "g".to_owned(),
        role_epoch: 2,
        spec_sha256: format!("{:064}", 1),
        spec_revision: 1,
        candidate: None,
        state: hoh_core::protocol::HandoffState {
            applied_decision_ids: vec!["D1".to_owned()],
            open_issue_ids: Vec::new(),
            unacked_message_ids: Vec::new(),
            queued_input_ids: Vec::new(),
        },
    });
    let start = SessionStart {
        message_id: "R/session-start/commander/2".to_owned(),
        run_id: "R".to_owned(),
        role: "commander".to_owned(),
        role_epoch: 2,
        package,
        current_epoch: 2,
        session_id: Some("R/session/commander/2".to_owned()),
        agent: "pi".to_owned(),
    };
    keep_first_input(project.path(), &start)?;
    let path = first_input_path(project.path(), "R/session/commander/2");
    let kept = std::fs::read(&path)?;
    let input: serde_json::Value = serde_json::from_slice(&kept)?;
    assert_eq!(
        input["package"]["state"]["applied_decision_ids"],
        serde_json::json!(["D1"])
    );
    keep_first_input(project.path(), &start)?;
    let mut other = start.clone();
    other.message_id = "R/another".to_owned();
    assert!(keep_first_input(project.path(), &other).is_err());
    assert_eq!(std::fs::read(&path)?, kept);
    let names: Vec<_> = std::fs::read_dir(path.parent().ok_or("dir")?)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    assert_eq!(names.len(), 1, "{names:?}");
    // A crash between the staging and its removal left a staged file (another pid's): the notice given again keeps
    // the input and leaves no staged file (M2T17R-55).
    let leftover = path
        .parent()
        .ok_or("dir")?
        .join(format!(".1.{}", "R%2Fsession-start%2Fcommander%2F2"));
    std::fs::write(&leftover, b"half")?;
    keep_first_input(project.path(), &start)?;
    assert!(!leftover.exists(), "the leftover staged file");
    assert_eq!(std::fs::read(&path)?, kept);
    Ok(())
}
