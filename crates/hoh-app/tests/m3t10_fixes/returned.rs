//! Persistent kill windows around consultation result delivery; no live agents.
use super::*;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptMove, InputOutcome, InputRouting, OutboxRow, Store};
use std::os::unix::process::ExitStatusExt as _;

fn outbox(root: &Path, id: &str) -> R<OutboxRow> {
    let rows = sql(
        root,
        &format!("SELECT * FROM outbox WHERE message_id='{id}'"),
        true,
    )?;
    let row = &rows[0];
    Ok(OutboxRow {
        message_id: id.into(),
        run_id: row["run_id"].as_str().map(str::to_owned),
        recipient_role: row["recipient_role"].as_str().ok_or("role")?.into(),
        role_epoch: row["role_epoch"].as_i64().ok_or("epoch")?,
        payload: serde_json::from_str(row["payload_json"].as_str().ok_or("payload")?)?,
        attempt_count: row["attempt_count"].as_i64().ok_or("count")?,
        next_retry_at: row["next_retry_at"].as_str().map(str::to_owned),
    })
}
pub(super) async fn stage(
    store: &Store,
    root: &Path,
    id: &str,
) -> R<hoh_core::persistence::InputRouted> {
    Ok(store
        .route_input(InputRouting::Stage {
            row: outbox(root, id)?,
            attempt: Some("A".into()),
            now: SystemTime::now(),
        })
        .await?)
}

#[test]
#[ignore = "child process for TEST-023-01 kill windows; fake work only"]
fn consultation_crash_host() -> R {
    let Some(root) = std::env::var_os("HOH_M3K_RETURN_PROJECT").map(PathBuf::from) else {
        return Ok(());
    };
    let window = std::env::var("HOH_M3K_RETURN_WINDOW")?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async {
        Host::start(config(&root)).await?.shutdown().await;
        let store = Store::open(&db(&root),config(&root).store)?;
        sql(&root,WORLD,false)?;
        sql(&root,PARTICIPANTS,false)?;
        let pid=std::process::id();
        let start=hoh_agents::identity::start_id(pid)?.ok_or("start identity")?;
        let boot=hoh_agents::identity::boot_id()?;
        std::fs::write(root.join("host-identity.json"),json!({"pid":pid,"start_id":start,"boot_id":boot}).to_string())?;
        sql(&root,&format!("UPDATE tasks SET writer=0 WHERE task_id='D'; UPDATE attempts SET pid={pid},process_start_id='{start}',boot_id='{boot}' WHERE attempt_id='A';"),false)?;
        let reply=store.consult_request(message("kill-query",json!({"intent":"consult","question":"preserve this answer","budget":{"wall_seconds":30},"deadline":"2099-01-01T00:00:00Z"}))?).await?.ok_or("child")?;
        let child=reply["task_id"].as_str().ok_or("child")?;
        seed_child_attempt(&root,child,"answer")?;
        store.runner_finished("answer",child_ending(child_result(child,"answer","COMPLETED"))).await?;
        let input=hoh_core::ids::child_id(child,"consult/result");
        assert!(stage(&store,&root,&input).await?.steer.is_some());
        if window=="rejected" {
            assert!(stage(&store,&root,&input).await?.steer.is_none());
            assert_eq!(store.reader()?.input(&input)?.ok_or("input")?.payload["reason"],"UNKNOWN_DELIVERY");
        } else if window=="delivered" || window=="applied" {
            store.route_input(InputRouting::Finish {input_id:input.clone(),attempt:"A".into(),outcome:InputOutcome::Delivered}).await?;
            if window=="applied" {
                store.move_attempt(AttemptMove {attempt_id:"A".into(),from:AttemptState::Running,to:AttemptState::Settled,facts:Default::default()}).await?;
            }
        }
        if window == "stale_epoch" {
            sql(&root,"UPDATE role_bindings SET role_epoch=2 WHERE role='developer'",false)?;
        } else if window == "stale_spec" {
            sql(&root,"INSERT INTO specs(spec_id,project_id,sha256,revision) VALUES ('s2',:p,printf('%064d',2),2); UPDATE runs SET spec_id='s2' WHERE run_id='R';",false)?;
        } else if window == "cancel_parent" {
            sql(&root,"UPDATE tasks SET state='CANCELLED' WHERE task_id='D'",false)?;
        }
        // An unconfirmed result cannot be regenerated while its original recipient can still consume it.
        store.settle_ended_inputs(sql(&root,"SELECT project_id FROM projects",true)?[0]["project_id"].as_str().ok_or("project")?).await?;
        assert!(events(&root,"task.consult_recovered")?.is_empty());
        rustix::process::kill_process(rustix::process::getpid(),rustix::process::Signal::KILL)?;
        unreachable!("SIGKILL returned")
    })
}

#[test]
fn test_023_01_consult_result_survives_parent_kill_windows_without_blind_resend() -> R {
    for window in [
        "staged",
        "rejected",
        "delivered",
        "applied",
        "stale_epoch",
        "stale_spec",
        "cancel_parent",
    ] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-result-kill-")
            .tempdir()?;
        let log = std::fs::File::create(dir.path().join("child.log"))?;
        let mut child = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "m3t10_fixes::returned::consultation_crash_host",
                "--ignored",
                "--nocapture",
            ])
            .env("HOH_M3K_RETURN_PROJECT", dir.path())
            .env("HOH_M3K_RETURN_WINDOW", window)
            .stdout(log.try_clone()?)
            .stderr(log)
            .spawn()?;
        let identity = hoh_agents::identity::start_id(child.id())?.ok_or("child identity")?;
        std::fs::write(
            dir.path().join("launched.json"),
            json!({"pid":child.id(),"start_id":identity}).to_string(),
        )?;
        let until = std::time::Instant::now() + Duration::from_secs(30);
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if std::time::Instant::now() >= until {
                if hoh_agents::identity::start_id(child.id())?.as_deref() == Some(&identity) {
                    child.kill()?;
                }
                child.wait()?;
                return Err(format!(
                    "{window}: timed out: {}",
                    std::fs::read_to_string(dir.path().join("child.log"))?
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            status.signal(),
            Some(9),
            "{window}: {}",
            std::fs::read_to_string(dir.path().join("child.log"))?
        );
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        rt.block_on(async {
            let host=Host::start(config(dir.path())).await?;
            let project=host.project_id.clone();
            host.shutdown().await;
            let store=Store::open(&db(dir.path()),config(dir.path()).store)?;
            let task="A/consult/kill-query/task";
            let original=hoh_core::ids::child_id(task,"consult/result");
            let routed=stage(&store,dir.path(),&original).await?;
            assert!(routed.steer.is_none(),"never resend an uncertain turn");
            for _ in 0..2 { store.settle_ended_inputs(&project).await?; }
            assert_eq!(events(dir.path(),"task.consult_finished")?.len(),1);
            let recovered=events(dir.path(),"task.consult_recovered")?;
            if matches!(window,"applied" | "stale_epoch" | "stale_spec" | "cancel_parent") {
                assert!(recovered.is_empty(),"confirmed result must not be regenerated");
                return R::Ok(());
            }
            assert_eq!(recovered.len(),1,"{window}: recover the result once after the parent dies");
            let input=recovered[0]["input_id"].as_str().ok_or("recovered id")?;
            let row=store.reader()?.input(input)?.ok_or("recovered input")?;
            assert_eq!(row.payload["source_message"]["correlation_id"],"kill-query");
            assert_eq!(row.payload["source_message"]["payload"],child_result(task,"answer","COMPLETED"));
            assert_eq!(store.reader()?.input(&original)?.ok_or("original")?.state,hoh_core::lifecycle::InputState::Rejected);
            sql(dir.path(),"UPDATE tasks SET state='RUNNING' WHERE task_id='D'; INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('A2','D','sd','STARTING',1,1,1);",false)?;
            let taken=store.route_input(InputRouting::Take {attempt:"A2".into()}).await?;
            assert_eq!(taken.inputs.len(),1,"{window}: next attempt receives the answer");
            assert_eq!(taken.inputs[0]["input_id"],input);
            assert_eq!(serde_json::from_str::<Value>(taken.inputs[0]["text"].as_str().ok_or("text")?)?["payload"]["summary"],"research outcome");
            assert_eq!(store.route_input(InputRouting::Take {attempt:"A2".into()}).await?.inputs,taken.inputs);
            store.move_attempt(AttemptMove {attempt_id:"A2".into(),from:AttemptState::Starting,to:AttemptState::Running,facts:Default::default()}).await?;
            store.move_attempt(AttemptMove {attempt_id:"A2".into(),from:AttemptState::Running,to:AttemptState::Settled,facts:Default::default()}).await?;
            store.settle_ended_inputs(&project).await?;
            assert_eq!(events(dir.path(),"task.consult_recovered")?.len(),1);
            assert_eq!(store.reader()?.input(input)?.ok_or("recovered")?.state,hoh_core::lifecycle::InputState::Applied);
            assert_eq!(sql(dir.path(),"SELECT count(*) n FROM tasks WHERE role='researcher'",true)?[0]["n"],1);
            R::Ok(())
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn test_023_01_recovered_result_is_only_context_for_the_next_parent_attempt() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-result-boundary-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let project = host.project_id.clone();
    host.shutdown().await;
    let store = Store::open(&db(dir.path()), config(dir.path()).store)?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let reply=store.consult_request(message("boundary",json!({"intent":"consult","question":"answer once","budget":{"wall_seconds":30},"deadline":"2099-01-01T00:00:00Z"}))?).await?.ok_or("child")?;
    let task = reply["task_id"].as_str().ok_or("child")?;
    seed_child_attempt(dir.path(), task, "answer")?;
    store
        .runner_finished(
            "answer",
            child_ending(child_result(task, "answer", "COMPLETED")),
        )
        .await?;
    let original = hoh_core::ids::child_id(task, "consult/result");
    assert!(stage(&store, dir.path(), &original).await?.steer.is_some());
    assert!(stage(&store, dir.path(), &original).await?.steer.is_none());
    store
        .move_attempt(AttemptMove {
            attempt_id: "A".into(),
            from: AttemptState::Running,
            to: AttemptState::Failed,
            facts: Default::default(),
        })
        .await?;
    store.settle_ended_inputs(&project).await?;
    let input = hoh_core::ids::child_id(task, "consult/result/recovered");
    sql(
        dir.path(),
        "INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('U','R','s1','RUNNING','developer',10); INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('AU','U','sd','RUNNING',1,1,1),('A2','D','sd','STARTING',1,1,1);",
        false,
    )?;
    let staged = store
        .route_input(InputRouting::Stage {
            row: outbox(dir.path(), &input)?,
            attempt: Some("AU".into()),
            now: SystemTime::now(),
        })
        .await?;
    assert!(staged.reply.is_some());
    assert!(
        staged.steer.is_none(),
        "recovered content never enters a live native turn"
    );
    sql(
        dir.path(),
        "UPDATE attempts SET state='STARTING' WHERE attempt_id='AU'",
        false,
    )?;
    assert!(
        store
            .route_input(InputRouting::Take {
                attempt: "AU".into()
            })
            .await?
            .inputs
            .is_empty(),
        "another task in the same role cannot take the parent's result"
    );
    assert_eq!(
        store
            .route_input(InputRouting::Take {
                attempt: "A2".into()
            })
            .await?
            .inputs
            .len(),
        1
    );
    store
        .move_attempt(AttemptMove {
            attempt_id: "A2".into(),
            from: AttemptState::Starting,
            to: AttemptState::Running,
            facts: Default::default(),
        })
        .await?;
    store
        .move_attempt(AttemptMove {
            attempt_id: "A2".into(),
            from: AttemptState::Running,
            to: AttemptState::Failed,
            facts: Default::default(),
        })
        .await?;
    for _ in 0..2 {
        store.settle_ended_inputs(&project).await?;
    }
    assert_eq!(
        events(dir.path(), "task.consult_recovered")?.len(),
        1,
        "one regeneration, never a recovery loop"
    );
    assert_eq!(
        sql(dir.path(), "SELECT count(*) n FROM input_queue", true)?[0]["n"],
        2
    );
    Ok(())
}
