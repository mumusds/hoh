//! M3H-50: the host dies or refuses start after Take committed, before any agent ran.
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
}
impl Executor for BeforeStart {
    fn deliver(&self, work: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.inner.deliver(work)
    }
    fn start(&self, work: &Work) -> Result<Started, Refusal> {
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
            fault,
            inner: FakeExecutor(work, project.clone()),
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
        let mut watermark = host.store().watermark();
        let result = tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let ended = host
                    .store()
                    .reader()?
                    .attempt("T2/attempt-1")?
                    .is_some_and(|a| a.state == hoh_core::lifecycle::AttemptState::Settled);
                if ended {
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
