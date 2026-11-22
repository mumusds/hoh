//! AC-017-03: runtime inputs use the product command path without a model.
use super::*;

/// External-agent fixture: no dispatch receives an answer and no model can start.
#[derive(Debug, Default)]
struct Unavailable {
    sent: std::sync::Mutex<Vec<tokio::sync::oneshot::Sender<Result<(), String>>>>,
    starts: std::sync::atomic::AtomicUsize,
}

impl hoh_app::host::Executor for Unavailable {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.sent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(sender);
        receiver
    }
    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err("all agents are unavailable".to_owned().into())
    }
}

async fn running(world: &World) -> R<(Host, String)> {
    let (host, run, _) = approved(world).await?;
    world.exec(&format!(
        "UPDATE runs SET state='RUNNING',resume_state=NULL,blocked_reason=NULL,blocked_reasons_json='[]' WHERE run_id='{run}';
         INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
         SELECT 'working',run_id,spec_id,'RUNNING','developer',1,1 FROM runs WHERE run_id='{run}';
         INSERT INTO task_requirements SELECT 'working','REQ-A',spec_id FROM runs WHERE run_id='{run}';"
    ))?;
    Ok((host, run))
}

async fn send(host: &Host, run: &str, key: &str, kind: &str, text: &str) -> R<(Value, Value)> {
    let body = command(
        host,
        run,
        key,
        "SendInput",
        json!({"text":text,"input_kind":kind,"target_role":null}),
    )
    .await?;
    let (status, response) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{response}");
    Ok((body, response))
}

#[tokio::test]
async fn test_017_03_unknown_control_rejected_without_changing_run() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    let before = world.rows("SELECT * FROM runs")?;
    let tasks = world.rows("SELECT * FROM tasks")?;
    let (body, response) = send(&host, &run, "unknown", "CONTROL", "暂停并删除").await?;
    let input = world.rows("SELECT state,payload_json FROM input_queue")?;
    assert_eq!(
        input[0]["state"], "REJECTED",
        "unknown CONTROL must be rejected at acceptance"
    );
    let payload: Value =
        serde_json::from_str(input[0]["payload_json"].as_str().ok_or("no payload")?)?;
    assert_eq!(payload["reason"], "unknown_control");
    assert_eq!(world.rows("SELECT * FROM runs")?, before);
    assert_eq!(world.rows("SELECT * FROM tasks")?, tasks);
    assert_eq!(
        world.query(
            "SELECT count(*) FROM commands WHERE kind IN ('PauseRun','ResumeRun','CancelRun')"
        )?,
        "0"
    );
    assert_eq!(world.query("SELECT count(*) FROM attempts")?, "0");
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?,
        (202, response)
    );
    assert_eq!(world.query("SELECT count(*) FROM input_queue")?, "1");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_017_03_control_normalizes_pause_resume_cancel_without_models() -> R {
    for texts in [
        ["　 PaUsE \t", "　ReSuMe　", " CaNcEl "],
        [" 暂停　", "恢复", "取消"],
    ] {
        let world = World::new()?;
        let (host, run) = running(&world).await?;
        for (n, (text, kind, state)) in texts
            .into_iter()
            .zip([
                ("PauseRun", "PAUSED"),
                ("ResumeRun", "RUNNING"),
                ("CancelRun", "CANCELLED"),
            ])
            .enumerate()
            .map(|(n, (text, (kind, state)))| (n, (text, kind, state)))
        {
            let key = format!("control-{n}");
            let (body, response) = send(&host, &run, &key, "CONTROL", text).await?;
            let id = hoh_core::ids::child_id(&format!("cmd-{key}"), "input");
            assert_eq!(
                world.query(&format!(
                    "SELECT state FROM input_queue WHERE input_id='{id}'"
                ))?,
                "APPLIED",
                "{text}: control is applied in the acceptance transaction"
            );
            assert_eq!(
                world.query(&format!(
                    "SELECT count(*) FROM commands WHERE kind='{kind}'"
                ))?,
                "1"
            );
            if kind == "PauseRun" {
                assert_eq!(
                    world.query("SELECT count(*) FROM events WHERE kind='run.pause_requested'")?,
                    "1"
                );
            }
            world
                .until(
                    &format!("SELECT state FROM runs WHERE run_id='{run}'"),
                    state,
                    std::time::Duration::from_secs(10),
                )
                .await?;
            assert_eq!(
                world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?,
                (202, response)
            );
            assert_eq!(
                world.query(&format!(
                    "SELECT count(*) FROM commands WHERE kind='{kind}'"
                ))?,
                "1"
            );
        }
        assert_eq!(world.query("SELECT count(*) FROM attempts")?, "0");
        assert_eq!(
            world.query("SELECT count(*) FROM receipts WHERE consumer='runtime'")?,
            "3"
        );
        assert_eq!(world.query("SELECT count(*) FROM outbox WHERE json_extract(payload_json,'$.kind')='run.control'")?, "2", "ResumeRun uses the state transition, without a control notice");
        let states = world.rows("SELECT kind FROM events WHERE kind IN ('run.pause_requested','run.paused') ORDER BY seq")?;
        assert_eq!(
            states,
            vec![
                json!({"kind":"run.pause_requested"}),
                json!({"kind":"run.paused"})
            ]
        );
        host.shutdown().await;
    }
    Ok(())
}

fn edit_export(world: &World) -> R<Index> {
    let mut files = sources();
    files[0].bytes = String::from_utf8(files[0].bytes.clone())?
        .replacen("then: result 1", "then: export CSV", 1)
        .into_bytes();
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    Ok(specindex::generate_index(&files)?)
}

fn protected(world: &World) -> R<Vec<Vec<Value>>> {
    [
        "SELECT * FROM specs ORDER BY revision",
        "SELECT run_id,spec_id FROM runs ORDER BY run_id",
        "SELECT * FROM tasks ORDER BY task_id",
        "SELECT * FROM requirements ORDER BY requirement_id",
        "SELECT * FROM acceptance_criteria ORDER BY ac_id",
    ]
    .iter()
    .map(|sql| world.rows(sql))
    .collect()
}

async fn settled(world: &World, id: &str) -> R<Value> {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let rows = world.rows(&format!(
            "SELECT state,payload_json FROM input_queue WHERE input_id='{id}'"
        ))?;
        let row = rows.into_iter().next().ok_or("missing input")?;
        if matches!(row["state"].as_str(), Some("APPLIED" | "REJECTED"))
            || std::time::Instant::now() >= end
        {
            return Ok(row);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn test_017_03_change_proposes_then_control_pauses_without_models() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    // Earlier approval belongs to fixture setup, not to the CHANGE/CONTROL under test.
    world.exec(&format!("INSERT INTO supervisor_cursor(run_id,last_handled_seq) VALUES('{run}',(SELECT max(seq) FROM events)) ON CONFLICT(run_id) DO UPDATE SET last_handled_seq=excluded.last_handled_seq;"))?;
    host.shutdown().await;
    let agents = std::sync::Arc::new(Unavailable::default());
    let mut config = hoh_app::host::HostConfig::new(&world.project());
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: agents.clone(),
        context: hoh_core::decision::Context {
            commander_dir: Some(world.state().join("hoh/commander")),
            ..Default::default()
        },
    });
    let host = Host::start(config).await?;
    let before = protected(&world)?;
    let next = edit_export(&world)?;
    let (body, response) = send(&host, &run, "change", "CHANGE", "把导出格式改为 CSV").await?;
    let id = hoh_core::ids::child_id("cmd-change", "input");
    let input = settled(&world, &id).await?;
    let crs = world.rows("SELECT state,reason,impact_json FROM change_requests")?;
    assert_eq!(crs.len(), 1, "CHANGE must propose one CR; input={input}");
    assert_eq!(crs[0]["state"], "PROPOSED");
    assert_eq!(crs[0]["reason"], "把导出格式改为 CSV");
    let impact: Value = serde_json::from_str(crs[0]["impact_json"].as_str().ok_or("no impact")?)?;
    assert_eq!(
        impact,
        json!({"requirements":["REQ-A"],"acceptance":["AC-A-01","AC-A-02"],"tests":["TEST-A-01","TEST-A-02"],"tasks":["working"]})
    );
    assert_eq!(input["state"], "APPLIED");
    assert_eq!(
        protected(&world)?,
        before,
        "proposal never applies a specification or changes tasks"
    );
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    let commands =
        world.rows("SELECT payload_json FROM commands WHERE kind='ProposeSpecChange'")?;
    assert_eq!(commands.len(), 1);
    let proposal: Value =
        serde_json::from_str(commands[0]["payload_json"].as_str().ok_or("no payload")?)?;
    assert_eq!(
        proposal,
        json!({"reason":"把导出格式改为 CSV","spec_sha256":next.sha256()})
    );
    let moves = world.rows(&format!("SELECT json_extract(payload_json,'$.to') AS state FROM events WHERE kind IN ('input.queued','input.delivered','input.applied') AND json_extract(payload_json,'$.input_id')='{id}' ORDER BY seq"))?;
    assert_eq!(
        moves,
        vec![
            json!({"state":"QUEUED"}),
            json!({"state":"DELIVERED"}),
            json!({"state":"APPLIED"})
        ]
    );
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?,
        (202, response)
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    send(&host, &run, "pause", "CONTROL", "暂停").await?;
    world
        .until(
            &format!("SELECT state FROM runs WHERE run_id='{run}'"),
            "PAUSED",
            std::time::Duration::from_secs(10),
        )
        .await?;
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='PauseRun'")?,
        "1"
    );
    assert_eq!(world.query("SELECT count(*) FROM attempts")?, "0");
    assert!(
        agents
            .sent
            .lock()
            .map_err(|_| "fixture lock poisoned")?
            .is_empty()
    );
    assert_eq!(agents.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_017_03_change_invalid_source_and_unchanged_digest() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    let before = protected(&world)?;
    // ADR-033's same-digest rule repairs the generated file without inventing a CR.
    std::fs::write(world.project().join(specindex::PATH), b"manual drift")?;
    send(&host, &run, "regenerate", "CHANGE", "重新生成索引").await?;
    let id = hoh_core::ids::child_id("cmd-regenerate", "input");
    assert_eq!(settled(&world, &id).await?["state"], "APPLIED");
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        specindex::generate_index(&sources())?.bytes()
    );
    assert_eq!(protected(&world)?, before);
    std::fs::write(
        world.project().join("docs/spec/a.md"),
        b"malformed specification",
    )?;
    let (body, response) = send(&host, &run, "invalid", "CHANGE", "invalid source").await?;
    let id = hoh_core::ids::child_id("cmd-invalid", "input");
    let input = settled(&world, &id).await?;
    assert_eq!(input["state"], "REJECTED");
    let payload: Value = serde_json::from_str(input["payload_json"].as_str().ok_or("payload")?)?;
    assert_eq!(payload["reason"], "SPEC_INVALID");
    assert_eq!(protected(&world)?, before);
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
        "1"
    );
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?,
        (202, response)
    );
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

fn open_store(world: &World) -> R<hoh_core::persistence::Store> {
    let mut options = hoh_app::host::HostConfig::new(&world.project()).store;
    options.wal_probe = Some(world::probe());
    Ok(hoh_core::persistence::Store::open(&world.db(), options)?)
}

async fn queued_change(
    store: &hoh_core::persistence::Store,
    run: &str,
    id: &str,
) -> R<hoh_core::persistence::OutboxRow> {
    store
        .enqueue_input(hoh_core::persistence::NewInput {
            input_id: id.into(),
            run_id: run.into(),
            kind: "CHANGE".into(),
            target_role: Some("developer".into()),
            payload: json!({"text":"change"}),
        })
        .await?;
    store
        .claim_inputs(std::time::SystemTime::now(), vec!["runtime".into()], 5)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or_else(|| "CHANGE was not claimed independently of agent first-input/epoch".into())
}

fn request(world: &World, digest: &str) -> R<hoh_core::persistence::SpecRequest> {
    let sources = sources()
        .into_iter()
        .map(|mut source| {
            source.bytes = std::fs::read(world.project().join(&source.path))?;
            Ok(source)
        })
        .collect::<R<Vec<_>>>()?;
    let bytes = std::fs::read(world.project().join(specindex::PATH))?;
    Ok(hoh_core::persistence::SpecRequest {
        action: hoh_core::persistence::SpecAction::Propose(hoh_core::protocol::ProposeSpecChange {
            reason: "change".into(),
            spec_sha256: digest.into(),
        }),
        files: Ok(hoh_core::persistence::SpecFiles {
            sources,
            existing_index: Some(bytes.clone()),
            tracked_index: Some(bytes),
        }),
    })
}

#[tokio::test]
async fn test_017_03_spec_conflict_rejected_with_same_reason_and_replayed_receipt() -> R {
    use hoh_core::persistence::ChangeInput;
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    host.shutdown().await;
    let before = protected(&world)?;
    edit_export(&world)?;
    let store = open_store(&world)?;
    let row = queued_change(&store, &run, "conflict").await?;
    let result = store
        .change_input(ChangeInput::Prepare {
            row: row.clone(),
            request: Some(request(&world, &"0".repeat(64))?),
        })
        .await?;
    let input = store.reader()?.input("conflict")?.ok_or("no input")?;
    assert_eq!(input.state, hoh_core::lifecycle::InputState::Rejected);
    assert_eq!(input.payload["reason"], "SPEC_CONFLICT");
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='ProposeSpecChange'")?,
        "0"
    );
    assert_eq!(protected(&world)?, before);
    let replay = store
        .change_input(ChangeInput::Prepare { row, request: None })
        .await?;
    assert_eq!(result.reply, replay.reply);
    assert_eq!(
        world.query("SELECT count(*) FROM receipts WHERE message_id='conflict'")?,
        "1"
    );
    Ok(())
}

#[tokio::test]
async fn test_017_03_change_resumes_publication_after_reopen_without_another_cr() -> R {
    use hoh_core::persistence::ChangeInput;
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    host.shutdown().await;
    let before = protected(&world)?;
    let next = edit_export(&world)?;
    let store = open_store(&world)?;
    let row = queued_change(&store, &run, "interrupted").await?;
    let prepared = store
        .change_input(ChangeInput::Prepare {
            row: row.clone(),
            request: Some(request(&world, &next.sha256())?),
        })
        .await?;
    assert!(prepared.publication.is_some());
    assert!(prepared.reply.is_none());
    assert_eq!(
        store.reader()?.input("interrupted")?.ok_or("input")?.state,
        hoh_core::lifecycle::InputState::Delivered
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    assert_ne!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    drop(store);
    // A reopened writer uses the persisted snapshot even when no new workspace request is supplied.
    let store = open_store(&world)?;
    let replay = store
        .change_input(ChangeInput::Prepare {
            row: row.clone(),
            request: None,
        })
        .await?;
    assert_eq!(replay.publication, prepared.publication);
    store
        .settle_outbox(
            &row.message_id,
            row.attempt_count,
            hoh_core::persistence::Settlement::Unapplied,
            5,
        )
        .await?;
    drop(store);
    let host = world.start_plain_host(None).await?;
    assert_eq!(settled(&world, "interrupted").await?["state"], "APPLIED");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='ProposeSpecChange'")?,
        "1"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM receipts WHERE message_id='interrupted'")?,
        "1"
    );
    assert_eq!(protected(&world)?, before);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_017_03_control_failure_rolls_back_input_and_child_command() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    let before = snapshot(&world)?;
    world.exec("CREATE TRIGGER refuse_control BEFORE INSERT ON commands WHEN NEW.kind='PauseRun' BEGIN SELECT RAISE(ABORT,'control test rollback'); END;")?;
    let body = command(
        &host,
        &run,
        "rollback",
        "SendInput",
        json!({"text":"pause","input_kind":"CONTROL","target_role":"developer"}),
    )
    .await?;
    let (status, _) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_ne!(status, 202);
    assert_eq!(snapshot(&world)?, before);
    assert_eq!(world.query("SELECT count(*) FROM input_queue")?, "0");
    assert_eq!(world.query("SELECT count(*) FROM receipts")?, "0");
    world.exec("DROP TRIGGER refuse_control;")?;
    let (status, _) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202);
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='PauseRun'")?,
        "1"
    );
    world
        .until(
            &format!("SELECT state FROM runs WHERE run_id='{run}'"),
            "PAUSED",
            std::time::Duration::from_secs(10),
        )
        .await?;
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_017_03_publication_failure_retries_without_duplicate_proposal() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    let before = protected(&world)?;
    let next = edit_export(&world)?;
    let pending = world.project().join(".hoh/pending");
    assert!(!pending.exists());
    std::fs::write(&pending, b"owned publication failure fixture")?;
    send(&host, &run, "publish-failure", "CHANGE", "change").await?;
    let id = hoh_core::ids::child_id("cmd-publish-failure", "input");
    world.until(&format!("SELECT count(*) FROM outbox WHERE message_id='{id}' AND state='PENDING' AND last_error IS NOT NULL"),"1",std::time::Duration::from_secs(5)).await?;
    assert_eq!(
        world.query(&format!(
            "SELECT state FROM input_queue WHERE input_id='{id}'"
        ))?,
        "DELIVERED"
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM receipts WHERE message_id='{id}'"
        ))?,
        "0"
    );
    host.shutdown().await;
    assert_eq!(
        std::fs::read(&pending)?,
        b"owned publication failure fixture"
    );
    std::fs::remove_file(&pending)?;
    let host = world.start_plain_host(None).await?;
    assert_eq!(settled(&world, &id).await?["state"], "APPLIED");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    assert_eq!(protected(&world)?, before);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_017_03_cancelled_change_cannot_be_applied_by_late_publication() -> R {
    use hoh_core::persistence::{ChangeInput, PersistError};
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    host.shutdown().await;
    let next = edit_export(&world)?;
    let store = open_store(&world)?;
    let row = queued_change(&store, &run, "cancelled-change").await?;
    store
        .change_input(ChangeInput::Prepare {
            row: row.clone(),
            request: Some(request(&world, &next.sha256())?),
        })
        .await?;
    let project = world.query(&format!("SELECT project_id FROM runs WHERE run_id='{run}'"))?;
    world.exec(&format!(
        "UPDATE runs SET state='CANCELLED' WHERE run_id='{run}';"
    ))?;
    store.settle_ended_inputs(&project).await?;
    let result = store
        .change_input(ChangeInput::Published { row: row.clone() })
        .await?;
    let input = store.reader()?.input("cancelled-change")?.ok_or("input")?;
    assert_eq!(input.state, hoh_core::lifecycle::InputState::Rejected);
    assert_eq!(input.payload["reason"], "run_ended");
    assert_eq!(
        serde_json::from_str::<Value>(result.reply.as_deref().ok_or("receipt")?)?["reason"],
        "run_ended"
    );
    let mut forged = row;
    forged.run_id = Some("another-run".into());
    assert!(matches!(
        store
            .change_input(ChangeInput::Published { row: forged })
            .await,
        Err(PersistError::KeyReuse(_))
    ));
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
    Ok(())
}

#[tokio::test]
async fn test_017_03_runtime_routing_keeps_other_input_defaults() -> R {
    let world = World::new()?;
    let (host, run) = running(&world).await?;
    host.shutdown().await;
    let store = open_store(&world)?;
    for (kind, role) in [
        ("REPLY", "runtime"),
        ("ASK", "commander"),
        ("STEER", "commander"),
    ] {
        store
            .enqueue_input(hoh_core::persistence::NewInput {
                input_id: kind.into(),
                run_id: run.clone(),
                kind: kind.into(),
                target_role: None,
                payload: json!({"text":"fixture"}),
            })
            .await?;
        assert_eq!(
            world.query(&format!(
                "SELECT recipient_role FROM outbox WHERE message_id='{kind}'"
            ))?,
            role,
            "S7 changes only CHANGE/CONTROL routing"
        );
    }
    Ok(())
}
