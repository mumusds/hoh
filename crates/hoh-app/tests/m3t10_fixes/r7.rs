//! Whole-lifecycle review regressions: message refusals, terminal runs, and withdrawn attempts.
use super::*;
use hoh_core::persistence::{Change, Commit, Decided, NewInput, StateCas, Store};
use hoh_core::protocol::{DecisionPayload, RunState};

#[derive(Debug)]
struct BridgeQueries {
    inner: FakeConsult,
    messages: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}
impl hoh_app::host::Executor for BridgeQueries {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.inner.deliver(work)
    }
    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let mut started = self.inner.start(work)?;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let original = std::mem::replace(&mut started.ended, rx);
        let messages = self.messages.clone();
        tokio::spawn(async move {
            if let Ok(mut outcome) = original.await {
                match messages.lock() {
                    Ok(lines) => outcome.observed.messages.extend(lines.clone()),
                    Err(error) => {
                        outcome.success = false;
                        outcome.detail = error.to_string();
                    }
                }
                let _ = tx.send(outcome);
            }
        });
        Ok(started)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_023_01_m3k600_runner_records_rejection_and_still_settles() -> R {
    use std::io::Write as _;
    let dir = tempfile::Builder::new()
        .prefix("m3t10-r7-runner-")
        .tempdir()?;
    Host::start(config(dir.path())).await?.shutdown().await;
    sql(dir.path(), WORLD, false)?;
    sql(
        dir.path(),
        PARTICIPANTS
            .replace(
                "'D','R','s1','RUNNING','developer',1,1",
                "'D','R','s1','READY','developer',0,1",
            )
            .split("INSERT INTO attempts")
            .next()
            .ok_or("participants")?,
        false,
    )?;
    let (stx, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (itx, _) = tokio::sync::mpsc::unbounded_channel();
    let messages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let fake = std::sync::Arc::new(BridgeQueries {
        inner: FakeConsult {
            started: stx,
            steers: itx,
            research: Default::default(),
        },
        messages: messages.clone(),
    });
    let clock = std::sync::Arc::new(Clock::default());
    let mut c = config(dir.path());
    c.clock = hoh_app::host::HostClock(clock.clone());
    c.runner = Some(hoh_app::host::RunnerConfig {
        executor: fake,
        context: Context {
            required_capabilities: [("developer".into(), Default::default())].into(),
            ..Default::default()
        },
    });
    let host = Host::start(c).await?;
    let (work, mut release) = tokio::time::timeout(Duration::from_secs(20), starts.recv())
        .await?
        .ok_or("started")?;
    assert_eq!(work.role, "developer");
    let mut first = message(
        "reuse-runner",
        json!({"intent":"opinion","controversy_id":"r7","text":"first"}),
    )?;
    first.attempt_id = work.attempt_id.clone();
    first.expected_revision = sql(
        dir.path(),
        "SELECT revision FROM tasks WHERE task_id='D'",
        true,
    )?[0]["revision"]
        .as_u64()
        .ok_or("revision")?;
    first.role_epoch = work.assignment.role_epoch as u64;
    let mut second = first.clone();
    second
        .payload
        .insert("text".into(), json!("different content"));
    let lines = vec![
        serde_json::to_string(&first)?,
        serde_json::to_string(&second)?,
    ];
    *messages.lock().map_err(|e| e.to_string())? = lines.clone();
    let bridge = hoh_app::host::product::bridge_of(dir.path(), &work.attempt_id);
    std::fs::write(bridge, lines.join("\n") + "\n")?;
    release.write_all(b"done\n")?;
    wait_sql(
        dir.path(),
        &clock,
        &format!(
            "SELECT state FROM attempts WHERE attempt_id='{}'",
            work.attempt_id
        ),
        |v| v[0]["state"] == "SETTLED",
    )
    .await?;
    let rejected = events(dir.path(), "runner.diagnostic")?;
    assert_eq!(
        rejected
            .iter()
            .filter(
                |d| d["message_id"] == "reuse-runner" && d["reply"]["reason_code"] == "KEY_REUSE"
            )
            .count(),
        1
    );
    assert_eq!(events(dir.path(), "discussion.opinion")?.len(), 1);
    host.shutdown().await;
    let restarted = Host::start(config(dir.path())).await?;
    assert_eq!(events(dir.path(), "runner.diagnostic")?, rejected);
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT state FROM attempts WHERE attempt_id='{}'",
                work.attempt_id
            ),
            true
        )?[0]["state"],
        "SETTLED"
    );
    restarted.shutdown().await;
    Ok(())
}

async fn setup() -> R<(tempfile::TempDir, Store, String, String)> {
    let dir = tempfile::Builder::new().prefix("m3t10-r7-").tempdir()?;
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
            "r7",
            json!({"intent":"consult","question":"keep the lifecycle bounded"}),
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

fn open(root: &Path, store: &Store, child: &str) -> R {
    assert!(
        events(root, "task.consult_finished")?.is_empty(),
        "a withdrawn attempt must not end its consult"
    );
    assert_eq!(store.reader()?.wait_for("R")?.len(), 1);
    assert_eq!(
        sql(
            root,
            "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
            true
        )?[0]["reserved_value"],
        10.0
    );
    assert_eq!(
        sql(
            root,
            &format!("SELECT state,attempt_count FROM tasks WHERE task_id='{child}'"),
            true
        )?[0],
        json!({"state":"READY","attempt_count":0})
    );
    assert!(events(root, "need_decision")?.is_empty());
    store.reader()?.check_question_anchors()?;
    Ok(())
}

async fn withdrawn(why: &str) -> R {
    let (dir, store, project, child) = setup().await?;
    sql(
        dir.path(),
        &format!(
            "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D'; UPDATE tasks SET state='LEASED' WHERE task_id='{child}'; INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('C','{child}','sr','PREPARED',1,1,1);"
        ),
        false,
    )?;
    match why {
        "pause" => {
            sql(
                dir.path(),
                "UPDATE runs SET state='PAUSE_REQUESTED',resume_state='RUNNING' WHERE run_id='R'",
                false,
            )?;
            store.pause_step("R", SystemTime::now()).await?;
        }
        "blocked" => {
            sql(
                dir.path(),
                "INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'external-block','R','run.blocker_claimed','{\"source\":\"commander:fixture\",\"reason\":\"BLOCKED_AUTH\"}','2026-11-23T00:00:00Z')",
                false,
            )?;
            store.pace_run("R", context()).await?;
        }
        "restart" => {
            sql(
                dir.path(),
                "INSERT INTO leases(project_id,kind,attempt_id,epoch,expires_at) VALUES (:p,'reader-1','C',1,'2020-01-01T00:00:00Z')",
                false,
            )?;
            drop(store);
            let host = Host::start(config(dir.path())).await?;
            host.store().settle_ended_inputs(&project).await?;
            open(dir.path(), host.store(), &child)?;
            let dispatched = host
                .store()
                .schedule_round("R", context())
                .await?
                .dispatched
                .ok_or("child redispatched")?;
            assert_eq!(dispatched.task_id, child);
            host.shutdown().await;
            return Ok(());
        }
        _ => return Err("unknown withdrawal".into()),
    }
    store.settle_ended_inputs(&project).await?;
    open(dir.path(), &store, &child)?;
    assert_eq!(
        sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
        if why == "pause" { "PAUSED" } else { "BLOCKED" }
    );
    if why == "blocked" {
        sql(
            dir.path(),
            "INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'external-unblock','R','run.blocker_released','{\"source\":\"commander:fixture\",\"reason\":\"BLOCKED_AUTH\"}','2026-11-23T00:00:01Z')",
            false,
        )?;
        store.pace_run("R", context()).await?;
    } else {
        let revision = sql(
            dir.path(),
            "SELECT revision FROM runs WHERE run_id='R'",
            true,
        )?[0]["revision"]
            .as_i64()
            .ok_or("revision")?;
        store
            .commit(Commit {
                cas: vec![StateCas {
                    id: "R".into(),
                    expected_revision: revision,
                    change: Change::Run(RunState::Running),
                }],
                ..Default::default()
            })
            .await?;
    }
    let dispatched = store
        .schedule_round("R", context())
        .await?
        .dispatched
        .ok_or("child redispatched")?;
    assert_eq!(dispatched.task_id, child);
    assert_ne!(dispatched.attempt_id, "C");
    assert!(events(dir.path(), "task.consult_finished")?.is_empty());
    store.reader()?.check_question_anchors()?;
    Ok(())
}

#[tokio::test]
async fn test_023_01_m3k602_pause_resume_keeps_consult() -> R {
    withdrawn("pause").await
}
#[tokio::test]
async fn test_023_01_m3k602_block_unblock_keeps_consult() -> R {
    withdrawn("blocked").await
}
#[tokio::test]
async fn test_023_01_m3k602_restart_prepared_keeps_consult() -> R {
    withdrawn("restart").await
}

async fn terminal(to: RunState) -> R {
    for state in ["ready", "blocked", "retry", "vacant"] {
        let (dir, store, project, child) = setup().await?;
        sql(
            dir.path(),
            "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D';",
            false,
        )?;
        if to == RunState::Completed {
            sql(
                dir.path(),
                "UPDATE runs SET state='VERIFYING' WHERE run_id='R'",
                false,
            )?;
        }
        match state {
            "blocked" => {
                sql(
                    dir.path(),
                    &format!(
                        "UPDATE tasks SET state='BLOCKED',blocked_reason='BLOCKED_AUTH' WHERE task_id='{child}'"
                    ),
                    false,
                )?;
            }
            "retry" => {
                sql(
                    dir.path(),
                    &format!(
                        "UPDATE tasks SET next_retry_at='2098-01-01T00:00:00Z',attempt_count=1 WHERE task_id='{child}'"
                    ),
                    false,
                )?;
            }
            "vacant" => {
                sql(
                    dir.path(),
                    "UPDATE role_bindings SET state='BLOCKED',blocked_reason='BLOCKED_AUTH',session_id=NULL WHERE role='researcher'",
                    false,
                )?;
            }
            _ => {}
        }
        // Scripted acceptance facts satisfy the unchanged Commander completion gate.
        sql(
            dir.path(),
            r#"
INSERT INTO requirements(requirement_id,spec_id,title,required_release) VALUES ('REQ-fixture','s1','fixture',1);
INSERT INTO acceptance_criteria(ac_id,requirement_id,spec_id,body,check_id) VALUES ('AC-fixture','REQ-fixture','s1','fixture criterion','unit');
INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,state,created_at) VALUES ('C-good','R',printf('%040d',1),printf('%040d',2),printf('%064d',1),printf('%064d',2),printf('%064d',3),'.','ACCEPTED','2026-11-23T00:00:00Z');
INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES ('artifact',printf('%064d',9),'fixture',1,'text/plain');
INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at) VALUES ('E','C-good','A','artifact','unit','test','PASS','product','2026-11-23T00:00:00Z');
INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status) VALUES ('AC-fixture','s1','C-good','E','PASS');
"#,
            false,
        )?;
        let revision = sql(
            dir.path(),
            "SELECT revision FROM runs WHERE run_id='R'",
            true,
        )?[0]["revision"]
            .as_u64()
            .ok_or("revision")?;
        let proposal = hoh_core::decision::Proposal {
            decision_id: "end-run".into(),
            run_id: "R".into(),
            role_epoch: 1,
            expected_revision: revision,
            spec_sha256: format!("{:064}", 1),
            candidate_id: Some("C-good".into()),
            causation_id: None,
            payload: DecisionPayload::TransitionRun {
                to_state: to,
                accepted_requirements: None,
                reason: Some("fixture lifecycle end".into()),
            },
        };
        if state == "ready" {
            sql(
                dir.path(),
                "CREATE TRIGGER fail_consult_end BEFORE INSERT ON events WHEN NEW.kind='task.consult_finished' BEGIN SELECT RAISE(ABORT,'injected consult end failure'); END",
                false,
            )?;
            assert!(store.decide(proposal.clone(), context()).await.is_err());
            assert!(events(dir.path(), "task.consult_finished")?.is_empty());
            assert!(events(dir.path(), "run.result")?.is_empty());
            assert_eq!(
                sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
                if to == RunState::Completed {
                    "VERIFYING"
                } else {
                    "RUNNING"
                }
            );
            open(dir.path(), &store, &child)?;
            sql(dir.path(), "DROP TRIGGER fail_consult_end", false)?;
        }
        let done = store.decide(proposal, context()).await?;
        assert!(matches!(done, Decided::Applied { .. }), "{state}: {done:?}");
        assert_eq!(
            events(dir.path(), "task.consult_finished")?.len(),
            1,
            "{state}: terminal run must end consult in its transaction"
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
                true
            )?[0]["state"],
            "CANCELLED"
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
        let ordered = sql(
            dir.path(),
            "SELECT (SELECT max(seq) FROM events WHERE kind='task.consult_finished') < (SELECT max(seq) FROM events WHERE kind='run.result') AS ended_before_result",
            true,
        )?;
        assert_eq!(ordered[0]["ended_before_result"], 1);
        sql(
            dir.path(),
            &WORLD[WORLD.find("INSERT INTO runs").ok_or("run fixture")?..]
                .replace("'R'", "'R2'")
                .replace("'sc'", "'sc2'"),
            false,
        )?;
        store
            .enqueue_input(NewInput {
                input_id: "later-input".into(),
                run_id: "R2".into(),
                kind: "ASK".into(),
                target_role: Some("commander".into()),
                payload: json!({"text":"later run still receives inputs"}),
            })
            .await?;
        for _ in 0..2 {
            store.settle_ended_inputs(&project).await?;
        }
        let routed = super::returned::stage(&store, dir.path(), "later-input").await?;
        assert!(routed.steer.is_none());
        assert_eq!(
            store
                .reader()?
                .input("later-input")?
                .ok_or("input")?
                .payload["boundary"],
            "next_attempt"
        );
        store.reader()?.check_question_anchors()?;
        drop(store);
        let host = Host::start(config(dir.path())).await?;
        host.store().settle_ended_inputs(&project).await?;
        assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
        host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_m3k602_real_task_cancellation_still_ends_consult() -> R {
    for requested in [false, true] {
        let (dir, store, project, child) = setup().await?;
        if requested {
            seed_child_attempt(dir.path(), &child, "C")?;
            sql(
                dir.path(),
                "UPDATE attempts SET cancel_requested=1 WHERE attempt_id='C'",
                false,
            )?;
            store
                .runner_finished("C", child_ending(Value::Null))
                .await?;
        } else {
            store
                .commit(Commit {
                    cas: vec![StateCas {
                        id: child.clone(),
                        expected_revision: 0,
                        change: Change::Task(hoh_core::protocol::TaskState::Cancelled),
                    }],
                    ..Default::default()
                })
                .await?;
        }
        store.settle_ended_inputs(&project).await?;
        assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
        assert_eq!(
            events(dir.path(), "task.consult_finished")?[0]["reason"],
            "cancelled"
        );
        assert_eq!(
            sql(
                dir.path(),
                "SELECT reserved_value FROM budgets WHERE scope_kind='run'",
                true
            )?[0]["reserved_value"],
            0.0
        );
        assert!(store.reader()?.wait_for("R")?.is_empty());
        store.reader()?.check_question_anchors()?;
    }
    Ok(())
}
#[tokio::test]
async fn test_023_01_m3k601_completed_run_ends_open_consults() -> R {
    terminal(RunState::Completed).await
}
#[tokio::test]
async fn test_023_01_m3k601_failed_run_ends_open_consults() -> R {
    terminal(RunState::Failed).await
}

#[tokio::test]
async fn test_023_01_m3k601_stopped_historical_run_does_not_abort_project_sweep() -> R {
    let (dir, store, project, _) = setup().await?;
    // A historical database produced before the terminal-run fix; no new progress may be written to it.
    sql(
        dir.path(),
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'; UPDATE tasks SET state='ACCEPTED' WHERE task_id='D'; UPDATE runs SET state='COMPLETED',revision=revision+1 WHERE run_id='R'; INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) SELECT project_id,'R/result/'||revision,'R','run.result','{\"state\":\"COMPLETED\"}','2026-11-23T00:00:00Z' FROM runs WHERE run_id='R';",
        false,
    )?;
    for _ in 0..2 {
        store.settle_ended_inputs(&project).await?;
    }
    assert!(
        events(dir.path(), "task.consult_finished")?.is_empty(),
        "stopped history is not rewritten by the input sweep"
    );
    Ok(())
}

#[tokio::test]
async fn test_023_01_m3k600_reused_bridge_message_settles_across_restart() -> R {
    use hoh_core::reconcile::EffectState;
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
    let dir = tempfile::Builder::new()
        .prefix("m3t10-unanswered-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE tasks SET writer=0 WHERE task_id='D'",
        false,
    )?;
    let mut process = std::process::Command::new("sh")
        .args(["-c", "read -r go; exit 0"])
        .process_group(0)
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    let pid = process.id();
    let start = hoh_agents::identity::start_id(pid)?.ok_or("process identity")?;
    let boot = hoh_agents::identity::boot_id()?;
    sql(
        dir.path(),
        &format!(
            "UPDATE attempts SET pid={pid},boot_id='{boot}',process_start_id='{start}' WHERE attempt_id='A'"
        ),
        false,
    )?;
    process.stdin.take().ok_or("stdin")?.write_all(b"go\n")?;
    assert!(process.wait()?.success());
    let request = message(
        "unanswered",
        json!({"intent":"consult","question":"survive an interrupted handoff","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}),
    )?;
    let bridge = hoh_app::host::product::bridge_of(dir.path(), "A");
    std::fs::create_dir_all(bridge.parent().ok_or("bridge parent")?)?;
    let mut reused = request.clone();
    reused
        .payload
        .insert("question".into(), json!("different content, same identity"));
    std::fs::write(
        bridge,
        serde_json::to_string(&request)? + "\n" + &serde_json::to_string(&reused)? + "\n",
    )?;
    let key = hoh_core::reconcile::work_key("A");
    sql(
        dir.path(),
        &format!("INSERT INTO effects(effect_key,attempt_id,state) VALUES ('{key}','A','STARTED')"),
        false,
    )?;
    host.store()
        .runner_effect(
            "A",
            &key,
            EffectState::Started,
            EffectState::Observed,
            json!({"success":true,"messages":[serde_json::to_string(&request)?, serde_json::to_string(&reused)?]}),
        )
        .await?;
    host.shutdown().await;
    sql(
        dir.path(),
        "CREATE TRIGGER fail_bridge_diagnostic BEFORE INSERT ON events WHEN NEW.kind='runner.diagnostic' BEGIN SELECT RAISE(ABORT,'injected diagnostic write failure'); END",
        false,
    )?;
    let error = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("a storage fault must fail reconcile")?;
    assert!(
        error
            .to_string()
            .contains("injected diagnostic write failure"),
        "{error}"
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT state FROM attempts WHERE attempt_id='A'",
            true
        )?[0]["state"],
        "RUNNING"
    );
    sql(dir.path(), "DROP TRIGGER fail_bridge_diagnostic", false)?;
    let restarted = Host::start(config(dir.path())).await?;
    assert_eq!(
        events(dir.path(), "task.consult")?.len(),
        1,
        "recover the unhandled request before settling its source"
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT state FROM attempts WHERE attempt_id='A'",
            true
        )?[0]["state"],
        "SETTLED"
    );
    let diagnostics = events(dir.path(), "runner.diagnostic")?;
    assert!(
        diagnostics
            .iter()
            .any(|d| d["message_id"] == request.message_id
                && d["reply"]["reason_code"] == "KEY_REUSE"),
        "message rejection must remain observable: {diagnostics:?}"
    );
    restarted.shutdown().await;
    let again = Host::start(config(dir.path())).await?;
    assert_eq!(events(dir.path(), "task.consult")?.len(), 1);
    again.shutdown().await;
    Ok(())
}
