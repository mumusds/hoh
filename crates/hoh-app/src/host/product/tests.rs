//! The product executor's pure parts: the prompt it sends, how a turn's facts become the attempt's outcome.

use super::*;
use hoh_agents::adapter::GateStatus;
use hoh_core::persistence::Assignment;
use hoh_core::staffing::AgentIdentity;

fn work(prompt: Option<&str>) -> Work {
    Work {
        project_id: "p1".to_owned(),
        run_id: "R1".to_owned(),
        task_id: "T1".to_owned(),
        attempt_id: "R1/T1/a1".to_owned(),
        role: "developer".to_owned(),
        effect_key: "k".to_owned(),
        assignment: Assignment {
            spec_draft: false,
            inputs: Vec::new(),
            context_seq: 0,
            resource_bundle: None,
            agent: Some("claude".to_owned()),
            model: None,
            role_epoch: 1,
            config_revision: 0,
            session_id: Some("R1/session/developer/1".to_owned()),
            native_session: None,
            agent_version: Some("2.1.318".to_owned()),
            session_capabilities: None,
            session_identity: None,
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
            writer: true,
            handoff: None,
        },
        run_dir: None,
        helpers: None,
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

fn contract(text: &str) -> hoh_core::prompt::Resolved {
    hoh_core::prompt::Resolved {
        chain: vec![hoh_core::config::Contract {
            layer: "project",
            path: ".hoh/prompts/c.md".to_owned(),
            text: text.to_owned(),
        }],
        conflicts: Vec::new(),
    }
}

#[test]
fn a_work_turn_is_told_its_identity_and_its_bridge_file() -> Result<(), Box<dyn std::error::Error>>
{
    // docs/adr/ADR-028.md decision 2: a developer's prompt starts its task context with the identity its TaskResult must
    // repeat and the bridge file in its own attempt directory; the directory exists before the turn.
    let dir = tempfile::tempdir()?;
    let w = work(None);
    let (text, _, _) = executor(dir.path())
        .pack(&w, &contract("Contract.\n\n"), None, false)
        .map_err(|r| format!("{r:?}"))?;
    let bridge = attempt_logs(dir.path(), &w.attempt_id).join(BRIDGE);
    for want in [
        "#### 本次 Attempt",
        "- run_id：R1",
        "- task_id：T1",
        "- attempt_id：R1/T1/a1",
        "- role：developer",
        "- role_epoch：1",
        &bridge.display().to_string(),
        // A writer is told where its self-test records go (docs/adr/ADR-028.md decision 14).
        ".hoh/self-tests/R1%2FT1%2Fa1/",
    ] {
        assert!(text.contains(want), "{want}: {text}");
    }
    assert!(bridge.parent().is_some_and(Path::is_dir));
    Ok(())
}

/// Run git in `dir` with a fixed identity.
fn git_in(dir: &Path, args: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

#[test]
fn a_candidates_changed_files_are_read_from_its_commit_in_path_order_within_their_limits()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-028.md decision 14's code source: git diff --name-only base..commit, by path, at most 50 files, each
    // read at the candidate's commit (never the workspace); over 64 KiB, not UTF-8 or deleted: its path only.
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git_in(root, &["init", "-q"])?;
    std::fs::write(root.join("a.txt"), "old\n")?;
    std::fs::write(root.join("gone.txt"), "soon gone\n")?;
    git_in(root, &["add", "."])?;
    git_in(root, &["commit", "-q", "-m", "base"])?;
    let base = git_in(root, &["rev-parse", "HEAD"])?;
    std::fs::write(root.join("a.txt"), "new\n")?;
    std::fs::remove_file(root.join("gone.txt"))?;
    std::fs::write(root.join("z.txt"), "last\n")?;
    std::fs::write(root.join("big.txt"), "x".repeat(70 * 1024))?;
    std::fs::write(root.join("bin.dat"), [0xff_u8, 0xfe, 0x00])?;
    std::fs::create_dir_all(root.join("many"))?;
    for n in 0..55 {
        std::fs::write(root.join(format!("many/f{n:02}.txt")), format!("{n}\n"))?;
    }
    git_in(root, &["add", "-A"])?;
    git_in(root, &["commit", "-q", "-m", "candidate"])?;
    let commit = git_in(root, &["rev-parse", "HEAD"])?;
    // The workspace moved on: the candidate's commit is what is read.
    std::fs::write(root.join("a.txt"), "workspace\n")?;
    let (files, total) = code_files(
        root,
        &hoh_core::context::CodeSource {
            base,
            commit: commit.clone(),
        },
    )?;
    assert_eq!(total, 60);
    assert_eq!(files.len(), hoh_core::context::CODE_FILES);
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        &paths[..6],
        [
            "a.txt",
            "big.txt",
            "bin.dat",
            "gone.txt",
            "many/f00.txt",
            "many/f01.txt"
        ]
    );
    assert_eq!(files[0].text, Ok("new\n".to_owned()));
    assert!(
        matches!(&files[1].text, Err(why) if why.contains("64 KiB")),
        "{:?}",
        files[1]
    );
    assert!(
        matches!(&files[2].text, Err(why) if why.contains("UTF-8")),
        "{:?}",
        files[2]
    );
    assert!(
        matches!(&files[3].text, Err(why) if why.contains("删除")),
        "{:?}",
        files[3]
    );
    // From git's empty tree (a first writer that started without a commit): every file of the candidate.
    let (all, total) = code_files(
        root,
        &hoh_core::context::CodeSource {
            base: "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned(),
            commit,
        },
    )?;
    assert_eq!((total, all[0].path.as_str()), (59, "a.txt"));
    Ok(())
}

#[test]
fn work_context_selects_code_by_task_paths_at_the_workspaces_head()
-> Result<(), Box<dyn std::error::Error>> {
    // REQ-039 by task scope (ADR-026 decision 12; docs/adr/ADR-035.md decision 3): the files under the task's paths in
    // the workspace's HEAD (a file or a directory, recursively), by path, within the code source's limits, read at
    // that commit (not the work tree); none outside them. Packed, they are the task layer's code entries.
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git_in(root, &["init", "-q"])?;
    std::fs::create_dir_all(root.join("src/deep"))?;
    std::fs::create_dir_all(root.join("docs"))?;
    std::fs::write(root.join("src/scope.rs"), "fn in_scope() {}\n")?;
    std::fs::write(root.join("src/deep/mod.rs"), "fn deep_in_scope() {}\n")?;
    std::fs::write(root.join("src/big.rs"), "x".repeat(70 * 1024))?;
    std::fs::write(root.join("docs/out.md"), "SENTINEL-OUT-OF-SCOPE\n")?;
    std::fs::write(root.join("README"), "SENTINEL-OUT-OF-SCOPE\n")?;
    git_in(root, &["add", "."])?;
    git_in(root, &["commit", "-q", "-m", "base"])?;
    let head = git_in(root, &["rev-parse", "HEAD"])?;
    // The work tree moved on: HEAD is what is read.
    std::fs::write(root.join("src/scope.rs"), "SENTINEL-WORK-TREE-EDIT\n")?;
    let (commit, files, total) = scoped_files(root, &["src".to_owned(), "missing/".to_owned()])?;
    assert_eq!(commit, head);
    assert_eq!(total, 3);
    let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["src/big.rs", "src/deep/mod.rs", "src/scope.rs"]);
    assert!(
        matches!(&files[0].text, Err(why) if why.contains("64 KiB")),
        "{:?}",
        files[0]
    );
    assert_eq!(files[2].text, Ok("fn in_scope() {}\n".to_owned()));
    let (_, one, total) = scoped_files(root, &["./src/deep/mod.rs".to_owned()])?;
    assert_eq!((total, one[0].path.as_str()), (1, "src/deep/mod.rs"));
    let mut w = work(None);
    if let Some(facts) = w.assignment.context.as_mut() {
        facts.code_scope = Some(vec!["src/".to_owned()]);
    }
    let (text, _, _) = executor(root)
        .pack(&w, &contract("Contract.\n\n"), None, false)
        .map_err(|r| format!("{r:?}"))?;
    let task = text.find("### 任务上下文").ok_or("no task layer")?;
    let code = text.find("fn deep_in_scope()").ok_or("no scoped file")?;
    assert!(code > task, "{text}");
    assert!(text.contains(&head), "{text}");
    for absent in ["SENTINEL-OUT-OF-SCOPE", "SENTINEL-WORK-TREE-EDIT"] {
        assert!(!text.contains(absent), "{absent}: {text}");
    }
    // No commit yet: why, never a silent empty source.
    let empty = tempfile::tempdir()?;
    git_in(empty.path(), &["init", "-q"])?;
    assert!(scoped_files(empty.path(), &["src".to_owned()]).is_err());
    Ok(())
}

#[test]
fn a_turns_loads_and_code_source_are_read_into_its_prompt() -> Result<(), Box<dyn std::error::Error>>
{
    // docs/adr/ADR-028.md decision 14: the store names a registered object and the code source, the executor reads
    // them: the object's text under its label in the evidence layer, the changed file's text in the task layer.
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git_in(root, &["init", "-q"])?;
    std::fs::write(root.join("README"), "r\n")?;
    git_in(root, &["add", "."])?;
    git_in(root, &["commit", "-q", "-m", "base"])?;
    let base = git_in(root, &["rev-parse", "HEAD"])?;
    std::fs::write(root.join("lib.rs"), "fn changed_by_the_candidate() {}\n")?;
    git_in(root, &["add", "."])?;
    git_in(root, &["commit", "-q", "-m", "candidate"])?;
    let commit = git_in(root, &["rev-parse", "HEAD"])?;
    let sha = hoh_core::objects::put(root, b"SENTINEL-SELF-TEST ALL TESTS PASSED\n")?;
    let mut w = work(None);
    if let Some(facts) = w.assignment.context.as_mut() {
        facts.loads.push(hoh_core::context::Load {
            path: "self-tests/report.txt".to_owned(),
            sha256: sha,
            name: "report.txt".to_owned(),
            label: "开发者自测（diagnostic）".to_owned(),
            relevance: 0,
        });
        facts.code = Some(hoh_core::context::CodeSource { base, commit });
    }
    let (text, _, _) = executor(root)
        .pack(&w, &contract("Contract.\n\n"), None, false)
        .map_err(|r| format!("{r:?}"))?;
    let evidence = text.find("### 证据上下文").ok_or("no evidence layer")?;
    let task = text.find("### 任务上下文").ok_or("no task layer")?;
    let sentinel = text.find("SENTINEL-SELF-TEST").ok_or("no self-test text")?;
    assert!(sentinel > evidence, "{text}");
    assert!(
        text[..sentinel]
            .rfind("开发者自测（diagnostic）")
            .is_some_and(|l| l > evidence)
    );
    let code = text
        .find("fn changed_by_the_candidate()")
        .ok_or("no changed file")?;
    assert!(code > task && code < evidence, "{text}");
    Ok(())
}

#[test]
fn a_turn_is_sent_its_roles_contract_then_its_layers() -> Result<(), Box<dyn std::error::Error>> {
    // ADR-022 decision 11 and docs/adr/ADR-026.md decisions 2 and 7: a contract without placeholders first, verbatim,
    // then the four layers (the run's goal, the task's id and title, the role); every input kept as an object and the
    // record names them.
    let dir = tempfile::tempdir()?;
    let (text, record, _) = executor(dir.path())
        .pack(&work(None), &contract("Contract.\n\n"), None, false)
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
    let (text, _, _) = executor(dir.path())
        .pack(
            &work(None),
            &contract("Contract.\n\n"),
            Some(&package),
            false,
        )
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
        .pack(&tight, &contract("Contract.\n\n"), None, false)
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
        .pack(&work(None), &contract("{{GOAL}} only"), None, false)
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
    // The package's object, the bytes whose digest its handoffs row names (docs/adr/ADR-028.md decision 9).
    let bytes = hoh_core::persistence::package_bytes(&start.package)?;
    assert_eq!(
        hoh_core::objects::get(project.path(), &sha256_hex(&bytes))?,
        bytes
    );
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

/// The facts of a turn that failed with `code` and no typed COMPAT_UNVERIFIED (`Facts.compat` none).
fn failed(code: Option<&str>) -> Facts {
    Facts {
        state: AttemptState::Failed,
        failure: code.map(str::to_owned),
        native_session: None,
        resolved_model: None,
        usage: None,
        raw_log_path: "/logs".to_owned(),
        max_buffered: None,
        group_reaped: true,
        recorded: Vec::new(),
        degraded: None,
        final_text: None,
        compat: None,
        retry_at: None,
    }
}

#[test]
fn an_adapters_failure_code_fails_its_attempt_by_its_class() {
    // Decision 16 (ADR-020 decision 1); docs/adr/ADR-030.md decision 6: MODEL_UNAVAILABLE is its own class, and only
    // the codes the adapters themselves recognise give a class: a bare COMPAT_UNVERIFIED string (no typed source) and
    // an agent's own text marked native are transient.
    assert_eq!(class_of(&failed(Some("BLOCKED_AUTH"))), FailureClass::Auth);
    assert_eq!(
        class_of(&failed(Some("BLOCKED_CAPABILITY"))),
        FailureClass::Capability
    );
    assert_eq!(
        class_of(&failed(Some("MODEL_UNAVAILABLE"))),
        FailureClass::Model
    );
    for code in [
        Some("PROTOCOL_FRAME_TOO_LARGE"),
        Some("CAPABILITY_UNSUPPORTED"),
        Some("TRANSIENT_EXTERNAL"),
        Some("COMPAT_UNVERIFIED"),
        Some("AGENT_UNAVAILABLE"),
        Some("native:BLOCKED_AUTH"),
        Some("native:MODEL_UNAVAILABLE"),
        Some("native:BLOCKED_CAPABILITY"),
        Some("native:COMPAT_UNVERIFIED"),
        None,
    ] {
        assert_eq!(
            class_of(&failed(code)),
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
        final_text: Some("done\n```hoh-report\n{}\n```".to_owned()),
        compat: None,
        retry_at: Some(std::time::UNIX_EPOCH + Duration::from_secs(9_000)),
    };
    let o = outcome(facts);
    // The provider's reset goes with the outcome as it came: the runner makes it the failure's Retry-After at the time
    // it records the end, by its own clock (M2T07R-256).
    assert_eq!(
        o.retry_at,
        Some(std::time::UNIX_EPOCH + Duration::from_secs(9_000))
    );
    // Its final assistant message, where a verifier's report is read (docs/adr/ADR-028.md decision 15).
    assert_eq!(
        o.observed.final_text.as_deref(),
        Some("done\n```hoh-report\n{}\n```")
    );
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
fn a_commander_turn_is_told_its_wake_the_values_to_name_its_nonce_and_its_decision_block()
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
    let (text, _, _) = executor(dir.path())
        .pack(
            &work,
            &hoh_core::prompt::Resolved {
                chain: vec![builtin],
                conflicts: Vec::new(),
            },
            None,
            false,
        )
        .map_err(|r| format!("{r:?}"))?;
    // docs/adr/ADR-032.md decision 1: the decisions are the final message's block, and no output file is named.
    for part in [
        "causation_id：R1/trigger/3".to_owned(),
        "expected_revision（Run 的 revision）：7".to_owned(),
        "role_epoch：2".to_owned(),
        "nonce：n0nce".to_owned(),
        format!("项目根：{}", dir.path().display()),
        "DecisionPayload".to_owned(),
        "```hoh-decisions".to_owned(),
        format!("决策块：{}", hoh_core::context::DECISION_BLOCK),
    ] {
        assert!(text.contains(&part), "{part}: {text}");
    }
    assert!(
        !text.contains("输出文件") && !text.contains("messages.jsonl"),
        "{text}"
    );
    assert!(!text.contains("{{"), "{text}");
    Ok(())
}

#[test]
fn a_bridge_is_read_line_by_line_within_its_bounds() -> Result<(), Box<dyn std::error::Error>> {
    // Decision 8: the file's non-empty lines in order; none when it is missing; at most BRIDGE_LINES lines and
    // BRIDGE_BYTES bytes are taken, and a read cut at either limit says so (docs/adr/ADR-028.md decision 2, M2T16D-456).
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("messages.jsonl");
    assert_eq!(read_bridge(&file), (Vec::<String>::new(), false));
    std::fs::write(&file, "one\n\ntwo\n")?;
    assert_eq!(
        read_bridge(&file),
        (vec!["one".to_owned(), "two".to_owned()], false)
    );
    let exact: String = (0..BRIDGE_LINES).map(|i| format!("{i}\n")).collect();
    std::fs::write(&file, exact)?;
    assert_eq!(read_bridge(&file).0.len(), BRIDGE_LINES);
    assert!(!read_bridge(&file).1);
    let many: String = (0..BRIDGE_LINES + 5).map(|i| format!("{i}\n")).collect();
    std::fs::write(&file, many)?;
    let (lines, truncated) = read_bridge(&file);
    assert_eq!((lines.len(), truncated), (BRIDGE_LINES, true));
    std::fs::write(&file, "x".repeat(BRIDGE_BYTES + 10))?;
    let (lines, truncated) = read_bridge(&file);
    assert!(lines.iter().map(String::len).sum::<usize>() <= BRIDGE_BYTES);
    assert!(truncated);
    Ok(())
}

#[test]
fn a_bridge_that_is_not_a_regular_file_is_never_waited_on() -> Result<(), Box<dyn std::error::Error>>
{
    // docs/adr/ADR-027.md decision 8's file access: an agent that leaves a FIFO where its bridge goes gets no lines and
    // the read returns at once, never blocking on the FIFO's other end.
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("messages.jsonl");
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &file,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )?;
    let started = std::time::Instant::now();
    assert_eq!(read_bridge(&file), (Vec::<String>::new(), false));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
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

#[test]
fn a_verifiers_confirm_ids_are_recorded_in_its_context_packed_and_its_entries_are_in_its_mandatory_scope()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-029.md decision 10: the Issue ids the store delivers with their description are recorded in the
    // attempt's `context.packed` (`confirm_ids`), fixed from then on; the entries are scope lines of the task context,
    // never trimmed. A turn with none records none.
    let dir = tempfile::tempdir()?;
    let mut w = work(None);
    if let Some(facts) = w.assignment.context.as_mut() {
        facts
            .scope
            .push("请确认已修复：Issue I-1（critical）：SENTINEL 越界".to_owned());
        facts.confirm_ids = Some(vec!["I-1".to_owned()]);
    }
    let (text, record, _) = executor(dir.path())
        .pack(&w, &contract("Contract.\n\n"), None, false)
        .map_err(|r| format!("{r:?}"))?;
    assert!(text.contains("SENTINEL 越界"), "{text}");
    let packed = record.packed.ok_or("no packing")?;
    assert_eq!(packed.event["confirm_ids"], serde_json::json!(["I-1"]));
    let (_, plain, _) = executor(dir.path())
        .pack(&work(None), &contract("Contract.\n\n"), None, false)
        .map_err(|r| format!("{r:?}"))?;
    assert!(
        plain
            .packed
            .ok_or("no packing")?
            .event
            .get("confirm_ids")
            .is_none()
    );
    Ok(())
}

#[test]
fn a_prepare_without_its_agent_is_refused_as_unavailable() -> Result<(), Box<dyn std::error::Error>>
{
    // docs/adr/ADR-030.md decision 6: prepare's "the binding names no agent" and an agent whose last look found it
    // unusable (`Some(Err)`) are refused AGENT_UNAVAILABLE in the Unavailable class (the task BLOCKED(AGENT_UNAVAILABLE),
    // decision 7's vacancy, no longer BLOCKED_CAPABILITY); an agent not looked at yet stays transient.
    let project = tempfile::tempdir()?;
    let executor = executor(project.path());
    let mut unnamed = work(None);
    unnamed.assignment.agent = None;
    let refusal = executor.prepare(&unnamed).err().ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("AGENT_UNAVAILABLE", FailureClass::Unavailable)
    );
    let refusal = executor.prepare(&work(None)).err().ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        (
            "AGENT_UNAVAILABLE",
            FailureClass::Transient { retry_after: None }
        ),
        "not looked at yet"
    );
    // The registry's PATH is empty: its look finds no claude.
    assert!(executor.registry.look(Agent::Claude).is_err());
    let refusal = executor.prepare(&work(None)).err().ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("AGENT_UNAVAILABLE", FailureClass::Unavailable)
    );
    Ok(())
}

const PROTOCOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");

/// A fake codex 0.166.0 in `bin` answering the probe from its recording (`note` makes it another identity); a turn
/// is never run through it here.
fn fake_codex(bin: &Path, note: &str) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let script = bin.join("codex");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\n# {note}\nf='{PROTOCOL}/codex/0.166.0'\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0'; exit 0;;\n \
             --help) cat \"$f/help.txt\"; exit 0;;\n exec) [ \"$2\" = --help ] && {{ cat \"$f/help-exec.txt\"; exit 0; }}; exit 2;;\n \
             app-server) case \"$2\" in\n  --help) cat \"$f/help-app-server.txt\"; exit 0;;\n  generate-json-schema) \
             [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R \"$f/app-server-schema/.\" \"$4/\"; exit 0;;\n esac;;\nesac\nexit 2\n"
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// A registry for `project` whose codex is the fake in `bin`, looked at and verified: its manifest entry is the look's,
/// PASS under this build's suite set, its read_only case too (as every suite of this build has it).
fn verified_codex(bin: &Path, project: &Path) -> Result<Arc<Registry>, Box<dyn std::error::Error>> {
    use hoh_agents::compat::{CompatManifest, SUITE_SET_SHA256, Suite, SuiteCase, SuiteStatus};
    let registry = Registry::with_pi_dir(format!("{}:/usr/bin:/bin", bin.display()).into(), None);
    registry.open_manifest(project);
    registry.want(&["codex".to_owned()]);
    registry.refresh();
    let mut entry = registry.seen(Agent::Codex).ok_or("not seen")??.entry;
    entry.suite = Suite {
        status: SuiteStatus::Pass,
        fixture_set_sha256: SUITE_SET_SHA256.as_str().to_owned(),
        run_at: Some(Some("2026-11-14T00:00:00Z".to_owned())),
        cases: Some(vec![SuiteCase {
            case: "read_only".to_owned(),
            status: SuiteStatus::Pass,
        }]),
    };
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/compat-manifest.json"),
        serde_json::to_vec_pretty(&CompatManifest::new([(Agent::Codex, entry)].into()))?,
    )?;
    registry.open_manifest(project);
    Ok(Arc::new(registry))
}

#[tokio::test]
async fn prepare_refuses_a_session_granted_for_another_identity()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-030.md decision 9 (d) (M2T15D-100): the registry's fact F is codex B (verified, logged in, its files
    // as stamped); a turn whose session S was granted for A (same version and capabilities, another identity) is refused
    // COMPAT_UNVERIFIED in the Compat class, its detail naming S and both identities, before its adapter starts (no
    // gate is spawned); as is one whose S has another version, other capabilities or no identity. S granted for B
    // passes, and its gate expects F's identity. F is read once each time (one read of the registry).
    use std::os::unix::fs::PermissionsExt as _;
    let bin = tempfile::tempdir()?;
    fake_codex(bin.path(), "B")?;
    let project = tempfile::tempdir()?;
    let registry = verified_codex(bin.path(), project.path())?;
    let b = match registry.fact(Agent::Codex) {
        Some(Fact::Usable(usable)) if usable.compat == Compat::Verified => *usable,
        other => return Err(format!("not verified: {other:?}").into()),
    };
    // The gate spawned for a turn records its argv in `runs` and waits for its input's end.
    let gates = tempfile::tempdir()?;
    let runs = gates.path().join("runs");
    let gate = gates.path().join("gate");
    std::fs::write(
        &gate,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{runs}'\ncat > /dev/null\n: > '{runs}.ended'\n",
            runs = runs.display()
        ),
    )?;
    std::fs::set_permissions(&gate, std::fs::Permissions::from_mode(0o755))?;
    let executor = ProductExecutor::new(
        registry.clone(),
        project.path().to_owned(),
        gate,
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let session =
        |version: &str, capabilities: &serde_json::Value, identity: Option<&AgentIdentity>| {
            let mut w = work(None);
            w.assignment.agent = Some("codex".to_owned());
            w.assignment.agent_version = Some(version.to_owned());
            w.assignment.session_capabilities = Some(capabilities.clone());
            w.assignment.session_identity = identity.cloned();
            w
        };
    let a = AgentIdentity {
        implementation_sha256: "a".repeat(64),
        ..b.identity.clone()
    };
    let mut fewer = b.session.clone();
    if let Some(caps) = fewer.as_object_mut() {
        caps.insert("resume".to_owned(), serde_json::json!(false));
    }
    assert_ne!(fewer, b.session);
    let other_version = "codex-cli 0.165.0";
    for (case, w) in [
        ("granted for A", session(&b.version, &b.session, Some(&a))),
        (
            "another version",
            session(other_version, &b.session, Some(&b.identity)),
        ),
        (
            "other capabilities",
            session(&b.version, &fewer, Some(&b.identity)),
        ),
        ("no identity", session(&b.version, &b.session, None)),
    ] {
        let reads = registry.reads();
        let refusal = executor.prepare(&w).err().ok_or(case)?;
        assert_eq!(registry.reads() - reads, 1, "{case}: F is read once");
        assert_eq!(
            (refusal.reason.as_str(), refusal.class),
            ("COMPAT_UNVERIFIED", FailureClass::Compat),
            "{case}"
        );
        let detail = refusal.detail.unwrap_or_default();
        assert!(
            detail.contains("session R1/session/developer/1 was granted for")
                && detail.contains("it waits for its rebind")
                && detail.contains(&b.identity.implementation_sha256),
            "{case}: {detail}"
        );
        let refused = executor.start(&w).err().ok_or(case)?;
        assert_eq!(refused.reason, "COMPAT_UNVERIFIED", "{case}");
        assert!(!runs.exists(), "{case}: an adapter started");
        if case == "granted for A" {
            assert!(detail.contains(&"a".repeat(64)), "{detail}");
        }
        if case == "another version" {
            assert!(detail.contains(other_version), "{detail}");
        }
    }
    // Granted for B: prepared, its gate expecting F's identity and stamps.
    let w = session(&b.version, &b.session, Some(&b.identity));
    let reads = registry.reads();
    let turn = executor.prepare(&w).map_err(|r| format!("{r:?}"))?;
    assert_eq!(registry.reads() - reads, 1, "F is read once");
    assert_eq!(turn.ran.identity, b.identity);
    let command = executor
        .gate(&w, turn.agent, &turn.usable)
        .map_err(|r| format!("{r:?}"))?;
    let expect = command.expect.ok_or("no expect")?;
    assert_eq!(
        (expect.agent, &expect.identity, expect.stamp, expect.plan),
        (
            Agent::Codex,
            &b.identity,
            b.stamp,
            hoh_agents::probe::Exec::Path
        )
    );
    let status = command.status.ok_or("no status")?;
    assert_eq!(
        status.path,
        attempt_logs(project.path(), &w.attempt_id).join("gate.jsonl")
    );
    assert!(
        status.key.len() == 64
            && status
                .key
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
        "a key of 32 bytes in hex"
    );
    // Started: its adapter spawns the gate once, told that expectation.
    let started = executor.start(&w).map_err(|r| format!("{r:?}"))?;
    drop(started);
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while !runs.with_extension("ended").exists() && std::time::Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let argv = std::fs::read_to_string(&runs)?;
    assert_eq!(argv.lines().count(), 1, "{argv}");
    let id = &b.identity;
    assert!(
        argv.starts_with(&format!(
            "__gate --expect codex {} {} {} {} --stamp ",
            id.launcher_path, id.launcher_sha256, id.implementation_path, id.implementation_sha256
        )),
        "{argv}"
    );
    Ok(())
}

#[test]
fn prepare_refuses_a_pi_whose_current_version_moved_since_the_last_look()
-> Result<(), Box<dyn std::error::Error>> {
    // M2T15R-01 (docs/adr/ADR-030.md decision 9's (a), the CTO's design correction): prepare's (a) is the registry's
    // one predicate "the agent is still what its last look found". The fact F is pi A (verified, its session fresh);
    // pi's install/current-version moves to B with A's files kept, so none of F's stamps changed: prepare refuses
    // COMPAT_UNVERIFIED in the Compat class, "files changed since the last look" naming both implementation paths,
    // from its one read of the registry. Unchanged, the same turn is prepared.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    crate::host::agents::tests::pi_install(&root, &["A", "B"], "A")?;
    let (project, registry) = crate::host::agents::tests::verified_pi(
        format!("{}:/usr/bin:/bin", root.join("bin").display()).into(),
    )?;
    let a = match registry.fact(Agent::Pi) {
        Some(Fact::Usable(usable)) if usable.compat == Compat::Verified => *usable,
        other => return Err(format!("not verified: {other:?}").into()),
    };
    let executor = ProductExecutor::new(
        registry.clone(),
        project.path().to_owned(),
        PathBuf::from("hoh"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let mut w = work(None);
    w.assignment.agent = Some("pi".to_owned());
    w.assignment.agent_version = Some(a.version.clone());
    w.assignment.session_capabilities = Some(a.session.clone());
    w.assignment.session_identity = Some(a.identity.clone());
    executor
        .prepare(&w)
        .map_err(|r| format!("unchanged: {r:?}"))?;
    std::fs::write(root.join("install/current-version"), "B\n")?;
    let reads = registry.reads();
    let refusal = executor
        .prepare(&w)
        .err()
        .ok_or("prepared after the move")?;
    assert_eq!(registry.reads() - reads, 1, "F is read once");
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("COMPAT_UNVERIFIED", FailureClass::Compat)
    );
    let detail = refusal.detail.unwrap_or_default();
    let b = a
        .identity
        .implementation_path
        .replace("/install/releases/A/", "/install/releases/B/");
    assert!(
        detail.starts_with("pi: files changed since the last look")
            && detail.contains(&a.identity.implementation_path)
            && detail.contains(&b),
        "{detail}"
    );
    Ok(())
}

#[test]
fn a_typed_gate_refusal_is_compat_unless_only_its_stamps_differed()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-030.md decisions 6 and 9 (-101, -201): class_of reads the facts' typed CompatRefusal, never the
    // failure string: COMPAT_UNVERIFIED with none is transient; the gate's keyed refusal is Compat, and transient when
    // only the stamps differed.
    let key = "c".repeat(64);
    let dir = tempfile::tempdir()?;
    let status = GateStatus {
        path: dir.path().join("gate.jsonl"),
        key: key.clone(),
    };
    assert_eq!(
        class_of(&failed(Some("COMPAT_UNVERIFIED"))),
        FailureClass::Transient { retry_after: None }
    );
    for (stamp_only, class) in [
        (false, FailureClass::Compat),
        (true, FailureClass::Transient { retry_after: None }),
    ] {
        std::fs::write(
            &status.path,
            serde_json::json!({"outcome": "refused", "key": key, "code": "COMPAT_UNVERIFIED", "item": "stamp",
                               "stamp_only": stamp_only, "expected": {}, "seen": {}})
            .to_string()
                + "\n",
        )?;
        let refusal = hoh_agents::adapter::gate_refusal(&status).ok_or("no refusal")?;
        let facts = Facts::gate_refused("/logs".to_owned(), None, refusal);
        assert_eq!(facts.failure.as_deref(), Some("COMPAT_UNVERIFIED"));
        assert_eq!(class_of(&facts), class, "stamp_only {stamp_only}");
        assert_eq!(outcome(facts).class, class);
    }
    Ok(())
}

#[test]
fn a_turns_output_is_its_raw_stdout_and_stderr_copies_and_nothing_else_of_its_directory()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-022.md decision 19 item 3: every adapter's copies (claude's and pi's, codex exec's and App
    // Server's names), not the gate's record, the bridge file or the diagnostics.
    let project = tempfile::tempdir()?;
    let executor = executor(project.path());
    let w = work(None);
    assert_eq!(executor.output_bytes(&w), None, "no directory yet");
    let logs = attempt_logs(project.path(), &w.attempt_id);
    std::fs::create_dir_all(&logs)?;
    assert_eq!(executor.output_bytes(&w), None, "no copy yet");
    for (name, bytes) in [
        ("stdout.jsonl", 3),
        ("stderr.log", 5),
        ("app-server.stdout.jsonl", 7),
        ("exec.stderr.log", 11),
        (GATE_RECORD, 100),
        (BRIDGE, 200),
        ("diagnostics.jsonl", 400),
    ] {
        std::fs::write(logs.join(name), vec![b'x'; bytes])?;
    }
    assert_eq!(executor.output_bytes(&w), Some(26));
    Ok(())
}

/// A fake codex 0.166.0 in `bin` like [`fake_codex`], its schema export without `read-only` among SandboxMode's values:
/// the binary itself offers no read-only profile (docs/adr/ADR-032.md decision 4).
fn fake_codex_without_read_only(bin: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::PermissionsExt as _;
    let export = bin.join("schema");
    let status = std::process::Command::new("cp")
        .arg("-R")
        .arg(format!("{PROTOCOL}/codex/0.166.0/app-server-schema"))
        .arg(&export)
        .status()?;
    assert!(status.success());
    let file = export.join("ClientRequest.json");
    let mut requests: serde_json::Value = serde_json::from_slice(&std::fs::read(&file)?)?;
    requests["definitions"]["SandboxMode"]["enum"]
        .as_array_mut()
        .ok_or("SandboxMode.enum")?
        .retain(|m| m != "read-only");
    std::fs::write(&file, serde_json::to_vec(&requests)?)?;
    let script = bin.join("codex");
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nf='{PROTOCOL}/codex/0.166.0'\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0'; exit 0;;\n \
             --help) cat \"$f/help.txt\"; exit 0;;\n exec) [ \"$2\" = --help ] && {{ cat \"$f/help-exec.txt\"; exit 0; }}; exit 2;;\n \
             app-server) case \"$2\" in\n  --help) cat \"$f/help-app-server.txt\"; exit 0;;\n  generate-json-schema) \
             [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{export}/.' \"$4/\"; exit 0;;\n esac;;\nesac\nexit 2\n",
            export = export.display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// `w` as the Commander's supervision turn of wake W1, working in `workdir`.
fn supervising(mut w: Work, workdir: &Path) -> Work {
    w.role = "commander".to_owned();
    w.assignment.supervision = Some(hoh_core::persistence::Supervision {
        wake: "R1/trigger/1".to_owned(),
        nonce: "n".to_owned(),
        run_revision: 1,
        role_epoch: 1,
        summary: serde_json::json!({}),
        workdir: Some(workdir.display().to_string()),
        subject: Vec::new(),
    });
    w
}

#[tokio::test]
async fn one_degradation_fact_decides_both_the_read_only_profile_and_the_transport()
-> Result<(), Box<dyn std::error::Error>> {
    // M2T20R-01: codex's degrade to exec is one fact, the registry's, keyed by the whole identity it was seen of, and a
    // prepare reads it once with the agent's fact: the Developer's turn goes to exec exactly when the Commander is
    // refused the read-only profile for it (AGENT_UNAVAILABLE, Unavailable), and never otherwise. Before, the executor
    // kept its own (launcher, version) set beside the registry's, read apart from it.
    let bin = tempfile::tempdir()?;
    fake_codex(bin.path(), "B")?;
    let project = tempfile::tempdir()?;
    let registry = verified_codex(bin.path(), project.path())?;
    let b = match registry.fact(Agent::Codex) {
        Some(Fact::Usable(usable)) => *usable,
        other => return Err(format!("not usable: {other:?}").into()),
    };
    let executor = ProductExecutor::new(
        registry.clone(),
        project.path().to_owned(),
        PathBuf::from("/bin/true"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let developer = {
        let mut w = work(None);
        w.assignment.agent = Some("codex".to_owned());
        w.assignment.agent_version = Some(b.version.clone());
        w.assignment.session_capabilities = Some(b.session.clone());
        w.assignment.session_identity = Some(b.identity.clone());
        w
    };
    let commander = supervising(developer.clone(), &project.path().join("commander-ws"));
    let on_exec = |w: &Work| -> Result<bool, Box<dyn std::error::Error>> {
        match executor.prepare(w).map_err(|r| format!("{r:?}"))?.prepared {
            Prepared::Codex(turn) => Ok(turn.launch.app_server_help.is_none()),
            other => Err(format!("not codex: {other:?}").into()),
        }
    };
    assert!(!on_exec(&developer)?);
    assert!(!on_exec(&commander)?);
    // The degrade, as the executor reports it, for B's identity: both follow at once.
    registry.exec_degraded(Agent::Codex, b.identity.clone());
    assert!(on_exec(&developer)?, "the Developer's turn uses exec");
    let refusal = executor.prepare(&commander).err().ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("AGENT_UNAVAILABLE", FailureClass::Unavailable)
    );
    // A degrade reported of another identity (an implementation since replaced) moves neither.
    let bin = tempfile::tempdir()?;
    fake_codex(bin.path(), "C")?;
    let project = tempfile::tempdir()?;
    let registry = verified_codex(bin.path(), project.path())?;
    let c = match registry.fact(Agent::Codex) {
        Some(Fact::Usable(usable)) => *usable,
        other => return Err(format!("not usable: {other:?}").into()),
    };
    registry.exec_degraded(Agent::Codex, b.identity.clone());
    let executor = ProductExecutor::new(
        registry,
        project.path().to_owned(),
        PathBuf::from("/bin/true"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let mut w = developer;
    w.assignment.agent_version = Some(c.version.clone());
    w.assignment.session_capabilities = Some(c.session.clone());
    w.assignment.session_identity = Some(c.identity.clone());
    let commander = supervising(w.clone(), &project.path().join("commander-ws"));
    for (name, work) in [("developer", &w), ("commander", &commander)] {
        match executor
            .prepare(work)
            .map_err(|r| format!("{name}: {r:?}"))?
            .prepared
        {
            Prepared::Codex(turn) => assert!(turn.launch.app_server_help.is_some(), "{name}"),
            other => return Err(format!("not codex: {other:?}").into()),
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_supervision_turn_launches_read_only_and_a_missing_profile_is_refused_by_its_source()
-> Result<(), Box<dyn std::error::Error>> {
    // docs/adr/ADR-032.md decision 2: the supervision turn, and only it, gets the read-only profile; no fallback. Decision
    // 4 (-201): its prepare checks the granted read_only_workspace and, lacking it, refuses by the source the registry
    // names: a host fact (here codex's degrade to exec) is AGENT_UNAVAILABLE in the Unavailable class, written as no
    // exclusion; the binary's (a schema without read-only) BLOCKED_CAPABILITY in the Capability class.
    let bin = tempfile::tempdir()?;
    fake_codex(bin.path(), "B")?;
    let project = tempfile::tempdir()?;
    let registry = verified_codex(bin.path(), project.path())?;
    let b = match registry.fact(Agent::Codex) {
        Some(Fact::Usable(usable)) => *usable,
        other => return Err(format!("not usable: {other:?}").into()),
    };
    assert!(b.granted.contains("read_only_workspace"), "{:?}", b.granted);
    let executor = ProductExecutor::new(
        registry.clone(),
        project.path().to_owned(),
        PathBuf::from("/bin/true"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let developer = {
        let mut w = work(None);
        w.assignment.agent = Some("codex".to_owned());
        w.assignment.agent_version = Some(b.version.clone());
        w.assignment.session_capabilities = Some(b.session.clone());
        w.assignment.session_identity = Some(b.identity.clone());
        w
    };
    let commander = supervising(developer.clone(), &project.path().join("commander-ws"));
    let profile = |w: &Work| -> Result<hoh_agents::Profile, Box<dyn std::error::Error>> {
        match executor.prepare(w).map_err(|r| format!("{r:?}"))?.prepared {
            Prepared::Codex(turn) => Ok(turn.launch.profile),
            other => Err(format!("not codex: {other:?}").into()),
        }
    };
    assert_eq!(profile(&commander)?, hoh_agents::Profile::ReadOnly);
    assert_eq!(profile(&developer)?, hoh_agents::Profile::Yolo);
    // A host fact withholds it: the commander's turn is refused as unavailable, the developer's still runs.
    registry.exec_degraded(Agent::Codex, b.identity.clone());
    let refusal = executor.prepare(&commander).err().ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("AGENT_UNAVAILABLE", FailureClass::Unavailable)
    );
    assert!(
        refusal
            .detail
            .unwrap_or_default()
            .contains("read_only_workspace")
    );
    assert_eq!(profile(&developer)?, hoh_agents::Profile::Yolo);
    // The binary lacks it: BLOCKED_CAPABILITY.
    let bin = tempfile::tempdir()?;
    fake_codex_without_read_only(bin.path())?;
    let project = tempfile::tempdir()?;
    let registry = verified_codex(bin.path(), project.path())?;
    let c = match registry.fact(Agent::Codex) {
        Some(Fact::Usable(usable)) => *usable,
        other => return Err(format!("not usable: {other:?}").into()),
    };
    assert!(!c.granted.contains("read_only_workspace") && c.withheld.is_empty());
    let executor = ProductExecutor::new(
        registry,
        project.path().to_owned(),
        PathBuf::from("/bin/true"),
        (Duration::from_secs(10), Duration::from_secs(5)),
    );
    let mut w = developer;
    w.assignment.agent_version = Some(c.version.clone());
    w.assignment.session_capabilities = Some(c.session.clone());
    w.assignment.session_identity = Some(c.identity.clone());
    let refusal = executor
        .prepare(&supervising(w, &project.path().join("commander-ws")))
        .err()
        .ok_or("prepared")?;
    assert_eq!(
        (refusal.reason.as_str(), refusal.class),
        ("BLOCKED_CAPABILITY", FailureClass::Capability)
    );
    Ok(())
}

#[test]
fn test_047_03_packing_uses_the_attempt_pin_not_the_current_binary()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::Builder::new()
        .prefix("m3t03-pack-pin-")
        .tempdir()?;
    let mut old = hoh_core::resource_bundle::Bundle::current();
    old.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "old developer\n".to_owned(),
    );
    let mut w = work(None);
    w.assignment.resource_bundle = Some(old.store(dir.path())?);
    let (_, record, _) = executor(dir.path())
        .pack(&w, &contract("old developer\n"), None, false)
        .map_err(|r| r.reason)?;
    assert_eq!(
        record.packed.ok_or("no packing")?.resource_bundle_version,
        old.version()
    );
    Ok(())
}

#[test]
fn test_047_03_unpinned_pack_and_drift_use_the_host_bundle()
-> Result<(), Box<dyn std::error::Error>> {
    // M3G-54: with no pin, packing and drift follow the injected host bundle, not Bundle::current.
    let dir = tempfile::Builder::new()
        .prefix("m3t03-host-bundle-")
        .tempdir()?;
    let mut host = hoh_core::resource_bundle::Bundle::current();
    host.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "host developer\n".to_owned(),
    );
    hoh_core::prompt_files::save_override_based_on(
        dir.path(),
        ".hoh/prompts/developer.md",
        "custom developer\n",
        "developer",
        &host,
    )?;
    let resolved = hoh_core::prompt_files::load(dir.path(), None, "R1", "developer", None)?;
    let (_, record, _) = executor(dir.path())
        .with_resources(host.clone())
        .pack(&work(None), &resolved, None, false)
        .map_err(|r| format!("{r:?}"))?;
    let packing = record.packed.ok_or("no packing")?;
    assert_eq!(packing.resource_bundle_version, host.version());
    assert_ne!(
        packing.resource_bundle_version,
        hoh_core::resource_bundle::Bundle::current().version()
    );
    assert!(
        packing.event["resource_drift"]
            .as_array()
            .ok_or("drift")?
            .is_empty(),
        "drift against the injected bundle: {}",
        packing.event["resource_drift"]
    );
    Ok(())
}

#[test]
fn test_047_03_incompatible_old_pin_is_transient_and_the_host_bundle_stays_decision()
-> Result<(), Box<dyn std::error::Error>> {
    // M3G-52: a pin this binary cannot render is transient so the checkpoint can move on.
    // The same bytes as the host bundle stay a decision. An identity mismatch stays a decision.
    let dir = tempfile::Builder::new().prefix("m3g52-schema-").tempdir()?;
    let mut broken = hoh_core::resource_bundle::Bundle::current();
    let mut schema: serde_json::Value =
        serde_json::from_str(broken.text("contracts/protocol.schema.json")?)?;
    schema["$defs"]
        .as_object_mut()
        .ok_or("no defs")?
        .remove("TaskResult");
    broken.files.insert(
        "contracts/protocol.schema.json".to_owned(),
        serde_json::to_string(&schema)?,
    );
    let mut pinned = work(None);
    pinned.assignment.resource_bundle = Some(broken.store(dir.path())?);
    let err = executor(dir.path())
        .pack(&pinned, &contract("old developer\n"), None, false)
        .err()
        .ok_or("old pin packed")?;
    assert_eq!(err.reason, "PROMPT_UNAVAILABLE");
    assert_eq!(
        err.class,
        FailureClass::Transient { retry_after: None },
        "{err:?}"
    );
    let err = executor(dir.path())
        .with_resources(broken.clone())
        .pack(&work(None), &contract("host developer\n"), None, false)
        .err()
        .ok_or("host bundle packed")?;
    assert_eq!(err.reason, "PROMPT_UNAVAILABLE");
    assert_eq!(err.class, FailureClass::Decision, "{err:?}");
    let stored = broken.store(dir.path())?;
    let mut mismatch = work(None);
    mismatch.assignment.resource_bundle = Some(hoh_core::resource_bundle::Pin {
        resource_bundle_version: "not-the-bytes".to_owned(),
        bundle_sha256: stored.bundle_sha256,
    });
    let err = executor(dir.path())
        .pack(&mismatch, &contract("x\n"), None, false)
        .err()
        .ok_or("identity mismatch packed")?;
    assert_eq!(err.class, FailureClass::Decision, "{err:?}");
    Ok(())
}

#[test]
fn test_047_03_drift_persists_three_way_object() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::Builder::new().prefix("m3t03-drift-").tempdir()?;
    let mut old = hoh_core::resource_bundle::Bundle::current();
    old.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "old developer\n".to_owned(),
    );
    hoh_core::prompt_files::save_override_based_on(
        dir.path(),
        ".hoh/prompts/developer.md",
        "custom developer\n",
        "developer",
        &old,
    )?;
    let resolved = hoh_core::prompt_files::load(dir.path(), None, "R1", "developer", None)?;
    let (_, record, _) = executor(dir.path())
        .pack(&work(None), &resolved, None, false)
        .map_err(|r| r.reason)?;
    let packing = record.packed.ok_or("no packing")?;
    assert!(
        packing.event["resource_drift"].is_array(),
        "drift is absent: {}",
        packing.event
    );
    let reports = packing.event["resource_drift"].as_array().unwrap();
    assert_eq!(reports.len(), 1);
    let sha = reports[0]["sha256"].as_str().ok_or("drift sha")?;
    let report: serde_json::Value =
        serde_json::from_slice(&hoh_core::objects::get(dir.path(), sha)?)?;
    assert_eq!(
        report["diff"],
        hoh_core::prompt::three_way_diff(
            "old developer\n",
            hoh_core::config::builtin_contract("developer").unwrap(),
            "custom developer\n"
        )
    );
    assert!(
        packing
            .artifacts
            .iter()
            .any(|a| a.sha256 == sha && a.kind == "resource_drift")
    );
    Ok(())
}
