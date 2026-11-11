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
            first_input: false,
            supervision: None,
            context: Some(hoh_core::context::Facts {
                goal: Some("Ship it".to_owned()),
                scope: vec!["任务：T1 Write the parser（职位 developer）".to_owned()],
                ..hoh_core::context::Facts::default()
            }),
        },
    }
}

fn executor(project: &Path) -> ProductExecutor {
    ProductExecutor::new(
        Arc::new(Registry::new(std::ffi::OsString::new())),
        project.to_owned(),
        PathBuf::from("hoh"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    )
}

fn contract(text: &str) -> hoh_core::config::Contract {
    hoh_core::config::Contract {
        layer: "project",
        path: ".hoh/prompts/c.md".to_owned(),
        text: text.to_owned(),
    }
}

#[test]
fn a_turn_is_sent_its_roles_contract_then_its_layers() -> Result<(), Box<dyn std::error::Error>> {
    // ADR-022 decision 11 and docs/adr/ADR-026.md decisions 2 and 7: a contract without placeholders first, verbatim,
    // then the four layers (the run's goal, the task's id and title, the role); every input kept as an object and the
    // record names them.
    let dir = tempfile::tempdir()?;
    let (text, record) = executor(dir.path())
        .pack(&work(None), &contract("Contract.\n\n"), None)
        .map_err(|r| format!("{r:?}"))?;
    assert!(text.starts_with("Contract.\n\n### 当前目标\n"), "{text}");
    for part in ["Ship it", "T1 Write the parser", "developer", "TaskResult"] {
        assert!(text.contains(part), "{part}: {text}");
    }
    assert!(!text.contains("交接包"), "{text}");
    assert_eq!(record.sent_sha256, sha256_hex(text.as_bytes()));
    let packed = record.packed.ok_or("no packing record")?;
    assert_eq!(
        packed.resource_bundle_version,
        hoh_core::objects::resource_bundle_version()
    );
    for artifact in &packed.artifacts {
        let bytes = std::fs::read(dir.path().join(&artifact.path))?;
        assert_eq!(sha256_hex(&bytes), artifact.sha256, "{}", artifact.path);
    }
    let manifest = packed.event["manifest"].as_str().ok_or("no manifest")?;
    let again = hoh_core::objects::repack(dir.path(), manifest)?;
    assert_eq!(again.prompt, text, "repacked from its objects alone");
    Ok(())
}

#[test]
fn a_sessions_first_turn_is_sent_its_whole_handover_package_before_every_other_context()
-> Result<(), Box<dyn std::error::Error>> {
    // REQ-019 b4 (docs/adr/ADR-024.md decision 4; docs/adr/ADR-026.md decision 3): a new holder's first turn has its
    // kept package, whole, after the contract and the goal and before the task and every other context.
    let dir = tempfile::tempdir()?;
    let package =
        serde_json::json!({"handoff_kind": "role_change", "role": "developer", "run_id": "R9"});
    let (text, _) = executor(dir.path())
        .pack(&work(None), &contract("Contract.\n\n"), Some(&package))
        .map_err(|r| format!("{r:?}"))?;
    let (contract_at, goal, package_at, task) = (
        text.find("Contract.").ok_or("no contract")?,
        text.find("Ship it").ok_or("no goal")?,
        text.find("\"run_id\": \"R9\"").ok_or("no package")?,
        text.find("T1 Write the parser").ok_or("no task")?,
    );
    assert!(
        contract_at < goal && goal < package_at && package_at < task,
        "{text}"
    );
    let block = text
        .split("```json\n")
        .nth(1)
        .and_then(|rest| rest.split("\n```").next())
        .ok_or("no package block")?;
    assert_eq!(serde_json::from_str::<serde_json::Value>(block)?, package);
    Ok(())
}

#[test]
fn a_turn_whose_mandatory_items_exceed_its_budget_is_refused_with_both_numbers()
-> Result<(), Box<dyn std::error::Error>> {
    // REQ-039 failure mode 1 (docs/adr/ADR-026.md decision 9): CONTEXT_BUDGET_TOO_SMALL, deterministic, with what the
    // mandatory items need and what the budget is; nothing kept, nothing sent.
    let dir = tempfile::tempdir()?;
    let mut tight = work(None);
    if let Some(facts) = tight.assignment.context.as_mut() {
        facts.params = Some(hoh_core::context::Params {
            budget_tokens: 10,
            shares: [45, 35, 20],
        });
    }
    let refusal = executor(dir.path())
        .pack(&tight, &contract("Contract.\n\n"), None)
        .err()
        .ok_or("packed within 10 tokens")?;
    assert_eq!(refusal.reason, "CONTEXT_BUDGET_TOO_SMALL");
    assert_eq!(refusal.class, FailureClass::Decision);
    let details = refusal.details.ok_or("no details")?;
    assert_eq!(details["available"], 10);
    assert!(
        details["required"].as_u64().is_some_and(|r| r > 10),
        "{details}"
    );
    assert_eq!(details["estimated"], true);
    assert!(
        !dir.path().join(".hoh/objects").exists(),
        "nothing kept for a turn that never starts"
    );
    // A contract with only some placeholders is refused the same way, as PROMPT_UNAVAILABLE.
    let refusal = executor(dir.path())
        .pack(&work(None), &contract("{{GOAL}} only"), None)
        .err()
        .ok_or("packed a malformed contract")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("PROMPT_UNAVAILABLE", FailureClass::Decision)
    );
    Ok(())
}

#[test]
fn a_first_input_found_in_place_is_taken_only_once_its_directory_is_synced()
-> Result<(), Box<dyn std::error::Error>> {
    // M2T17R-102: the link is made and the directory's sync fails, so the keep fails; the notice given again finds the
    // same input in place, and is taken only once the directory's sync done on that call succeeds.
    let project = tempfile::tempdir()?;
    let mut start = SessionStart {
        message_id: "R/session-start/developer/2".to_owned(),
        run_id: "R".to_owned(),
        role: "developer".to_owned(),
        role_epoch: 2,
        package: hoh_core::handoff::role_change(&hoh_core::handoff::RoleChange {
            role: "developer".to_owned(),
            run_id: "R".to_owned(),
            goal: "g".to_owned(),
            role_epoch: 2,
            spec: Some((format!("{:064}", 1), 1)),
            candidate: None,
            state: hoh_core::protocol::HandoffState {
                applied_decision_ids: Vec::new(),
                open_issue_ids: Vec::new(),
                unacked_message_ids: Vec::new(),
                queued_input_ids: Vec::new(),
            },
            references: Vec::new(),
            budget_tokens: hoh_core::handoff::BUDGET_TOKENS,
        }),
        current_epoch: 2,
        session_id: Some("R/session/developer/2".to_owned()),
        agent: "codex".to_owned(),
    };
    let syncs = std::cell::Cell::new(0);
    let failing = |_: &Path| -> std::io::Result<()> {
        syncs.set(syncs.get() + 1);
        Err(std::io::Error::other("EIO"))
    };
    assert!(keep_first_input_with(project.path(), &start, &failing).is_err());
    assert!(first_input_path(project.path(), "R/session/developer/2").exists());
    assert!(
        keep_first_input_with(project.path(), &start, &failing).is_err(),
        "taken although its directory is not synced"
    );
    let synced = |_: &Path| -> std::io::Result<()> {
        syncs.set(syncs.get() + 1);
        Ok(())
    };
    keep_first_input_with(project.path(), &start, &synced)?;
    assert_eq!(syncs.get(), 3, "a sync on every call");
    start.message_id = "R/another".to_owned();
    assert!(keep_first_input_with(project.path(), &start, &synced).is_err());
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
fn a_turns_facts_become_its_outcome() {
    // Decision 12: the verdict, the failure code as the reason and its class, the group's end, and what the turn showed
    // (events in order, the agent's own session, model, usage, the raw logs). The prompt record is no longer part of
    // it: it is recorded when the attempt is marked RUNNING (docs/adr/ADR-026.md decision 7).
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
    let o = outcome(facts);
    assert!(!o.success);
    assert_eq!(o.detail, "PROTOCOL_FRAME_TOO_LARGE");
    assert!(o.group_gone);
    assert_eq!(o.class, FailureClass::Transient { retry_after: None });
    assert_eq!(o.observed.native_session.as_deref(), Some("n-1"));
    assert_eq!(o.observed.resolved_model.as_deref(), Some("m"));
    assert_eq!(o.observed.raw_log_path.as_deref(), Some("/logs"));
    assert_eq!(o.observed.prompt, None);
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
        spec: Some((format!("{:064}", 1), 1)),
        candidate: None,
        state: hoh_core::protocol::HandoffState {
            applied_decision_ids: vec!["D1".to_owned()],
            open_issue_ids: Vec::new(),
            unacked_message_ids: Vec::new(),
            queued_input_ids: Vec::new(),
        },
        references: Vec::new(),
        budget_tokens: hoh_core::handoff::BUDGET_TOKENS,
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

#[test]
fn a_commander_turn_is_told_its_wake_the_values_to_name_its_nonce_and_its_bridge()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-025.md decisions 7 and 8, docs/adr/ADR-026.md decision 3: the wake it answers (its causation), the
    // run's revision and the Commander's epoch its decision names, the nonce, the bridge file and the project's root,
    // in its task layer, never cut.
    let dir = tempfile::tempdir()?;
    let mut work = work(None);
    work.role = "commander".to_owned();
    work.assignment.supervision = Some(hoh_core::persistence::Supervision {
        wake: "R1/trigger/3".to_owned(),
        nonce: "n0nce".to_owned(),
        run_revision: 7,
        role_epoch: 2,
        summary: serde_json::json!({"wake": {"id": "R1/trigger/3"}, "snapshots": {"T1": 4}}),
        workdir: None,
        subject: Vec::new(),
    });
    if let Some(facts) = work.assignment.context.as_mut() {
        facts.header = vec![
            ("causation_id".to_owned(), "R1/trigger/3".to_owned()),
            (
                "expected_revision（Run 的 revision）".to_owned(),
                "7".to_owned(),
            ),
            ("role_epoch".to_owned(), "2".to_owned()),
            ("nonce".to_owned(), "n0nce".to_owned()),
        ];
    }
    let builtin = hoh_core::config::Contract {
        layer: "builtin",
        path: "resources/prompts/commander.md".to_owned(),
        text: hoh_core::config::builtin_contract("commander")
            .ok_or("no commander")?
            .to_owned(),
    };
    let (text, _) = executor(dir.path())
        .pack(&work, &builtin, None)
        .map_err(|r| format!("{r:?}"))?;
    let bridge = dir
        .path()
        .join(".hoh/attempts")
        .join(component("R1/T1/a1"))
        .join("messages.jsonl");
    for part in [
        "causation_id：R1/trigger/3".to_owned(),
        "expected_revision（Run 的 revision）：7".to_owned(),
        "role_epoch：2".to_owned(),
        "nonce：n0nce".to_owned(),
        format!("输出文件：{}", bridge.display()),
        format!("项目根：{}", dir.path().display()),
        "DecisionPayload".to_owned(),
    ] {
        assert!(text.contains(&part), "{part}: {text}");
    }
    assert!(!text.contains("{{"), "{text}");
    Ok(())
}

#[test]
fn a_bridge_is_read_line_by_line_within_its_bounds() -> Result<(), Box<dyn std::error::Error>> {
    // Decision 8: the file's non-empty lines in order; none when it is missing; at most BRIDGE_LINES lines and
    // BRIDGE_BYTES bytes are taken.
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("messages.jsonl");
    assert!(read_bridge(&file).is_empty());
    std::fs::write(&file, "one\n\ntwo\n")?;
    assert_eq!(read_bridge(&file), ["one", "two"]);
    let many: String = (0..BRIDGE_LINES + 5).map(|i| format!("{i}\n")).collect();
    std::fs::write(&file, many)?;
    assert_eq!(read_bridge(&file).len(), BRIDGE_LINES);
    std::fs::write(&file, "x".repeat(BRIDGE_BYTES + 10))?;
    assert!(read_bridge(&file).iter().map(String::len).sum::<usize>() <= BRIDGE_BYTES);
    Ok(())
}

#[test]
fn a_log_that_cannot_be_kept_as_an_object_is_registered_in_place_and_one_unreadable_is_a_recorded_failure()
-> Result<(), Box<dyn std::error::Error>> {
    // M2T09R-01 (docs/adr/ADR-026.md decision 10): no log is dropped in silence.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let logs = dir.path().join(".hoh/attempts/A");
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("stdout.jsonl"), b"{\"a\":1}\n")?;
    std::fs::write(logs.join("stderr.log"), b"warn\n")?;
    // Normally: each an object.
    let (rows, failed) = keep_logs(dir.path(), &logs);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(
        rows.iter()
            .all(|r| r.kind == "raw_log" && r.path.starts_with(".hoh/objects/")),
        "{rows:?}"
    );
    // The object directory unwritable: registered where they are, under their own sha256.
    std::fs::remove_dir_all(dir.path().join(".hoh/objects"))?;
    std::fs::write(dir.path().join(".hoh/objects"), b"not a directory")?;
    let (rows, failed) = keep_logs(dir.path(), &logs);
    assert!(failed.is_empty(), "{failed:?}");
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(row.kind, "raw_log_in_place");
        let bytes = std::fs::read(dir.path().join(&row.path))?;
        assert_eq!(sha256_hex(&bytes), row.sha256, "{}", row.path);
    }
    // Unreadable too: a recorded failure naming it.
    std::fs::set_permissions(
        logs.join("stderr.log"),
        std::fs::Permissions::from_mode(0o000),
    )?;
    if std::fs::read(logs.join("stderr.log")).is_ok() {
        eprintln!(
            "skipped: this user reads mode-000 files (root), so the unreadable branch cannot run"
        );
    } else {
        let (rows, failed) = keep_logs(dir.path(), &logs);
        assert_eq!(rows.len(), 1);
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0]["name"], "stderr.log");
    }
    std::fs::set_permissions(
        logs.join("stderr.log"),
        std::fs::Permissions::from_mode(0o644),
    )?;
    Ok(())
}

#[test]
fn a_log_directory_gone_is_a_recorded_failure() -> Result<(), Box<dyn std::error::Error>> {
    // M2T09R-100: its logs vanished before they were kept: said, not silently nothing.
    let dir = tempfile::tempdir()?;
    let logs = dir.path().join(".hoh/attempts/gone");
    let (rows, failed) = keep_logs(dir.path(), &logs);
    assert!(rows.is_empty());
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["name"], logs.display().to_string());
    Ok(())
}
