//! M3H-50/M3H-54: host recovery at the Take and external-turn Stage boundaries.
use super::*;
use hoh_app::host::{Executor, Refusal, Started, Work};
use hoh_core::persistence::NewInput;

const PROJECT: &str = "HOH_TEST_INPUT_START_PROJECT";
const FAULT: &str = "HOH_TEST_INPUT_START_FAULT";

#[derive(Debug)]
struct BeforeStart {
    project: PathBuf,
    fault: String,
    inner: FakeExecutor,
    holds: Arc<std::sync::Mutex<Vec<std::process::ChildStdin>>>,
}
impl Executor for BeforeStart {
    fn deliver(&self, work: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.inner.deliver(work)
    }
    fn start(&self, work: &Work) -> Result<Started, Refusal> {
        if self.fault == "stage" && work.task_id == "T1" {
            use std::os::unix::process::CommandExt as _;
            let mut child = Command::new("sh")
                .args(["-c", "read -r go; [ \"$go\" = go ] || exit 1; read -r done"])
                .process_group(0)
                .env(hoh_app::host::ATTEMPT_ENV, work.marker())
                .stdin(std::process::Stdio::piped())
                .spawn()
                .map_err(|e| e.to_string())?;
            let pid = child.id();
            let start = hoh_agents::identity::start_id(pid)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "start id".to_string())?;
            std::fs::write(
                self.project.join("stage-agent"),
                format!("proc {pid} {start}\n"),
            )
            .map_err(|e| e.to_string())?;
            let mut stdin = child.stdin.take().ok_or_else(|| "stdin".to_string())?;
            let holds = self.holds.clone();
            // Keep stdin until the test kills this host, so the fake turn cannot finish before Stage.
            let gate = hoh_app::host::Gate::new(move || {
                use std::io::Write as _;
                stdin.write_all(b"go\n").map_err(|e| e.to_string())?;
                holds.lock().map_err(|e| e.to_string())?.push(stdin);
                Ok(())
            });
            return Ok(Started {
                pid,
                gate,
                ended: reaping(child),
                prompt: None,
                ran: None,
            });
        }
        let record = json!({"attempt_id":work.attempt_id,"inputs":work.assignment.inputs,
            "pid":std::process::id(),"start_id":hoh_agents::identity::start_id(std::process::id()).map_err(|e|e.to_string())?});
        if work.task_id == "T1" {
            if self.fault != "restart" {
                std::fs::write(self.project.join("before-start.json"), record.to_string())
                    .map_err(|e| e.to_string())?;
            }
            if self.fault == "kill" {
                rustix::process::kill_process(
                    rustix::process::getpid(),
                    rustix::process::Signal::KILL,
                )
                .map_err(|e| e.to_string())?;
                loop {
                    std::thread::park();
                }
            }
            return Err(Refusal {
                reason: "SPAWN_REFUSED".into(),
                class: hoh_core::retry::FailureClass::Decision,
                detail: Some("injected before process creation".into()),
                details: None,
            });
        }
        std::fs::write(self.project.join("next-start.json"), record.to_string())
            .map_err(|e| e.to_string())?;
        self.inner.start(work)
    }
    fn steer(
        &self,
        work: &Work,
        input: &hoh_core::persistence::InputRow,
    ) -> tokio::sync::oneshot::Receiver<hoh_agents::codex::SteerReceipt> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let result = (|| -> R {
            let record = json!({"attempt_id":work.attempt_id,"input_id":input.input_id,"payload":input.payload,"pid":std::process::id(),
                "start_id":hoh_agents::identity::start_id(std::process::id())?});
            std::fs::write(self.project.join("stage.json"), record.to_string())?;
            if self.fault == "stage" {
                rustix::process::kill_process(
                    rustix::process::getpid(),
                    rustix::process::Signal::KILL,
                )?;
            }
            Ok(())
        })();
        assert!(result.is_ok(), "Stage fault injection failed: {result:?}");
        let _ = tx.send(hoh_agents::codex::SteerReceipt::NotNow);
        rx
    }
}

#[test]
#[ignore = "child host for M3H-50; fake executor, no live agent"]
fn input_start_host() -> R {
    let Some(project) = std::env::var_os(PROJECT).map(PathBuf::from) else {
        return Ok(());
    };
    let fault = std::env::var(FAULT)?;
    let work = project.join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let mut settings = config(&project);
    settings.runner = Some(hoh_app::host::RunnerConfig {
        executor: Arc::new(BeforeStart {
            project: project.clone(),
            fault: fault.clone(),
            inner: FakeExecutor(work, project.clone()),
            holds: Default::default(),
        }),
        context: Context {
            reader_slots: Some(1),
            ..developer_context()
        },
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let host = Host::start(settings).await?;
        if fault == "stage" {
            let mut changes = host.store().watermark();
            tokio::time::timeout(Duration::from_secs(30), async {
                // M3QA-004: wait until T1 is RUNNING (not merely STARTING, which running_attempts() includes) so
                // under cgroup tracking the steer hits the running turn's Stage path rather than next_attempt.
                while !host
                    .store()
                    .reader()?
                    .running_attempts("R")?
                    .iter()
                    .any(|a| {
                        a.task_id == "T1" && a.state == hoh_core::lifecycle::AttemptState::Running
                    })
                {
                    changes.changed().await?;
                }
                R::Ok(())
            })
            .await??;
            host.store()
                .enqueue_input(NewInput {
                    input_id: "I-stage".into(),
                    run_id: "R".into(),
                    kind: "STEER".into(),
                    target_role: Some("developer".into()),
                    payload: json!({"text":"stage crash"}),
                })
                .await?;
        }
        let mut watermark = host.store().watermark();
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let ended = host
                    .store()
                    .reader()?
                    .attempt("T2/attempt-1")?
                    .is_some_and(|a| a.state == hoh_core::lifecycle::AttemptState::Settled);
                if ended {
                    // The input outcome commits before the transport ACK, which emits no watermark.
                    // Let the dispatcher settle before shutting down this recovered host.
                    if fault == "restart-stage"
                        && query(
                            &db(&project),
                            "SELECT state FROM outbox WHERE message_id='I-stage'",
                        )? != "ACKED"
                    {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        continue;
                    }
                    return R::Ok(());
                }
                watermark.changed().await?;
            }
        })
        .await;
        host.shutdown().await;
        result??;
        Ok(())
    })
}

fn survives(fault: &str) -> R {
    let dir = tempfile::Builder::new().prefix("m3t09-start-").tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir(&project)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        Host::start(config(&project)).await?.shutdown().await;
        R::Ok(())
    })?;
    seed(
        &project,
        &format!(
            "{WRITERS} UPDATE tasks SET writer=0; UPDATE tasks SET state='BLOCKED' WHERE task_id='T2';"
        ),
    )?;
    runtime.block_on(async {
        let host = Host::start(config(&project)).await?;
        host.store()
            .enqueue_input(NewInput {
                input_id: "I1".into(),
                run_id: "R".into(),
                kind: "STEER".into(),
                target_role: Some("developer".into()),
                payload: json!({"text":"preserve before start"}),
            })
            .await?;
        host.shutdown().await;
        R::Ok(())
    })?;
    let launch = |fault: &str| -> R<ChildGuard> {
        let child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "input_start::input_start_host",
                "--ignored",
                "--nocapture",
            ])
            .env(PROJECT, &project)
            .env(FAULT, fault)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        std::fs::write(
            project.join(format!("host-{fault}.json")),
            json!({"pid":child.id(),"start_id":hoh_agents::identity::start_id(child.id())?})
                .to_string(),
        )?;
        Ok(ChildGuard(child))
    };
    let first = launch(fault)?;
    if fault == "kill" {
        let (status, stderr) = wait(first, Duration::from_secs(60))?;
        assert_eq!(status.signal(), Some(9), "{stderr}");
        assert_eq!(
            query(
                &db(&project),
                "SELECT state FROM attempts WHERE task_id='T1'"
            )?,
            "STARTING"
        );
        assert_eq!(
            query(
                &db(&project),
                "SELECT count(*) FROM events WHERE kind='attempt.moved' AND json_extract(payload_json,'$.to')='RUNNING'"
            )?,
            "0"
        );
        seed(
            &project,
            "UPDATE tasks SET state='BLOCKED' WHERE task_id='T1'; UPDATE tasks SET state='READY' WHERE task_id='T2';",
        )?;
        let (status, stderr) = wait(launch("restart")?, Duration::from_secs(80))?;
        assert!(status.success(), "{stderr}");
    } else {
        assert!(wait_for(
            &db(&project),
            "SELECT state FROM attempts WHERE task_id='T1'",
            "FAILED",
            Duration::from_secs(60)
        )?);
        seed(
            &project,
            "UPDATE tasks SET state='READY' WHERE task_id='T2';",
        )?;
        let (status, stderr) = wait(first, Duration::from_secs(80))?;
        assert!(status.success(), "{stderr}");
    }
    let before: Value = serde_json::from_slice(&std::fs::read(project.join("before-start.json"))?)?;
    let next: Value = serde_json::from_slice(&std::fs::read(project.join("next-start.json"))?)?;
    assert_eq!(before["inputs"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        next["inputs"], before["inputs"],
        "the next process must receive the never-sent input"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT state FROM input_queue WHERE input_id='I1'"
        )?,
        "APPLIED"
    );
    for kind in ["input.delivered", "input.applied"] {
        assert_eq!(
            query(
                &db(&project),
                &format!(
                    "SELECT count(*) FROM events WHERE kind='{kind}' AND json_extract(payload_json,'$.input_id')='I1'"
                )
            )?,
            "1"
        );
    }
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM receipts WHERE message_id='I1'"
        )?,
        "1"
    );
    keep(
        &format!("input-start-{fault}.json"),
        &json!({"before":before,"next":next,"mode":"product","agent":"fake","applied":1}),
    )
}

#[test]
fn test_017_05_unstarted_input_survives_host_kill9_after_take() -> R {
    survives("kill")
}
#[test]
fn test_017_02_unstarted_input_survives_host_start_refusal() -> R {
    survives("refuse")
}

#[test]
fn test_017_05_host_kill9_after_stage_never_retries_the_unknown_steer() -> R {
    let dir = tempfile::Builder::new().prefix("m3low-stage-").tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir(&project)?;
    let _agents = common::Recorded(project.join("stage-agent"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    runtime.block_on(async {
        Host::start(config(&project)).await?.shutdown().await;
        R::Ok(())
    })?;
    seed(&project,&format!("{WRITERS} UPDATE tasks SET writer=0; UPDATE tasks SET state='BLOCKED' WHERE task_id='T2';
        UPDATE sessions SET capabilities_json=json_set(capabilities_json,'$.mid_turn_steer',json('true'));"))?;
    let launch = |fault: &str| -> R<ChildGuard> {
        let child = Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "input_start::input_start_host",
                "--ignored",
                "--nocapture",
            ])
            .env(PROJECT, &project)
            .env(FAULT, fault)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        std::fs::write(
            project.join(format!("host-{fault}.json")),
            json!({"pid":child.id(),"start_id":hoh_agents::identity::start_id(child.id())?})
                .to_string(),
        )?;
        Ok(ChildGuard(child))
    };
    let (status, stderr) = wait(launch("stage")?, Duration::from_secs(45))?;
    assert_eq!(status.signal(), Some(9), "{stderr}");
    let staged: Value = serde_json::from_slice(&std::fs::read(project.join("stage.json"))?)?;
    assert_eq!(staged["payload"]["delivery_pending"], true);
    assert_eq!(staged["payload"]["boundary"], "turn");
    assert_eq!(
        query(
            &db(&project),
            "SELECT state||':'||json_extract(payload_json,'$.delivery_pending') FROM input_queue WHERE input_id='I-stage'"
        )?,
        "QUEUED:1"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM receipts WHERE message_id='I-stage'"
        )?,
        "0"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT state||':'||attempt_count FROM outbox WHERE message_id='I-stage'"
        )?,
        "DELIVERING:1"
    );
    seed(
        &project,
        "UPDATE tasks SET state='BLOCKED' WHERE task_id='T1'; UPDATE tasks SET state='READY' WHERE task_id='T2';",
    )?;
    let (status, stderr) = wait(launch("restart-stage")?, Duration::from_secs(60))?;
    assert!(status.success(), "{stderr}");
    assert_eq!(
        query(
            &db(&project),
            "SELECT state||':'||json_extract(payload_json,'$.reason') FROM input_queue WHERE input_id='I-stage'"
        )?,
        "REJECTED:UNKNOWN_DELIVERY"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT state||':'||attempt_count FROM outbox WHERE message_id='I-stage'"
        )?,
        "ACKED:2"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM receipts WHERE message_id='I-stage'"
        )?,
        "1"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM events WHERE kind='input.rejected' AND json_extract(payload_json,'$.input_id')='I-stage'"
        )?,
        "1"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM events WHERE kind IN ('input.delivered','input.applied') AND json_extract(payload_json,'$.input_id')='I-stage'"
        )?,
        "0"
    );
    let next: Value = serde_json::from_slice(&std::fs::read(project.join("next-start.json"))?)?;
    assert_eq!(
        next["inputs"],
        json!([]),
        "the unknown input must never be merged into the next Attempt"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(project.join("stage.json"))?)?,
        staged,
        "no second external steer call"
    );
    keep(
        "m3low-stage-recovery.json",
        &json!({"staged":staged,"next":next,"mode":"product","agent":"fake","host_signal":9}),
    )
}
