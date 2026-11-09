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
    let text = prompt_text("Contract.\n\n", &work(None));
    assert!(text.starts_with("Contract.\n\n---\n"), "{text}");
    for part in ["Ship it", "T1 Write the parser", "developer"] {
        assert!(text.contains(part), "{part}: {text}");
    }
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
