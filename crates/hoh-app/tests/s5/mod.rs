//! S5: real PTY and product host, with a recording fake executor (no live agents).
use super::*;
use hoh_app::host::{Executor, Host, HostConfig, RunnerConfig};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct RecordingAgent {
    inputs: Mutex<Vec<(String, String)>>,
    leaders: Mutex<Vec<(u32, String)>>,
    releases: Arc<Mutex<Vec<std::process::ChildStdin>>>,
    hold_next_steer: Arc<std::sync::atomic::AtomicBool>,
    steer_entered: Arc<tokio::sync::Notify>,
    steer_release: Arc<tokio::sync::Notify>,
}

impl Executor for RecordingAgent {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        Sixty.deliver(work)
    }
    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "read -r go; [ \"$go\" = go ] || exit 1; read -r done; exit 0",
            ])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let start = hoh_agents::identity::start_id(pid)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "missing fake process identity".to_owned())?;
        self.leaders
            .lock()
            .map_err(|e| e.to_string())?
            .push((pid, start));
        let mut stdin = child.stdin.take().ok_or_else(|| "stdin".to_owned())?;
        let releases = self.releases.clone();
        let (tx, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = tx.send(hoh_app::host::Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: format!("{status:?}"),
                group_gone: crate::world::group_exit::group_gone_after_reap(pid),
                class: hoh_core::retry::FailureClass::Decision,
                observed: Default::default(),
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate: hoh_app::host::Gate::new(move || {
                use std::io::Write as _;
                stdin.write_all(b"go\n").map_err(|e| e.to_string())?;
                releases.lock().map_err(|e| e.to_string())?.push(stdin);
                Ok(())
            }),
            ended,
            prompt: None,
            ran: None,
        })
    }
    fn steer(
        &self,
        work: &hoh_app::host::Work,
        input: &hoh_core::persistence::InputRow,
    ) -> tokio::sync::oneshot::Receiver<hoh_agents::codex::SteerReceipt> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        if self
            .hold_next_steer
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            let entered = self.steer_entered.clone();
            let release = self.steer_release.clone();
            tokio::spawn(async move {
                entered.notify_one();
                release.notified().await;
                let _ = tx.send(hoh_agents::codex::SteerReceipt::NotNow);
            });
            return rx;
        }
        let receipt = match (self.inputs.lock(), input.payload["text"].as_str()) {
            (Ok(mut inputs), Some(text)) => {
                inputs.push((work.role.clone(), text.into()));
                hoh_agents::codex::SteerReceipt::Delivered {
                    turn_id: work.attempt_id.clone(),
                }
            }
            _ => hoh_agents::codex::SteerReceipt::Rejected {
                reason: "fake could not record input".into(),
            },
        };
        let _ = tx.send(receipt);
        rx
    }
}

struct Cleanup(Arc<RecordingAgent>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let leaders = self
            .0
            .leaders
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        for (pid, start) in leaders.iter() {
            if hoh_agents::identity::start_id(*pid).ok().flatten().as_ref() == Some(start)
                && let Some(pid) = i32::try_from(*pid)
                    .ok()
                    .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pid, Signal::KILL);
            }
        }
    }
}

fn fixture(
    runtime: &tokio::runtime::Runtime,
    world: &world::World,
    active: bool,
    agent: Arc<RecordingAgent>,
) -> R<Host> {
    world.configure(&[("cto", "claude"), ("developer", "claude")], "")?;
    let initial = runtime.block_on(world.start_plain_host(None))?;
    runtime.block_on(initial.shutdown());
    let p = world.query("SELECT project_id FROM projects")?;
    world.exec(&format!("{}\nINSERT INTO role_bindings(run_id,role,agent,role_epoch,config_revision,bound_at)
        VALUES ('R','cto','claude',1,1,'2026-11-22T00:00:00Z');
        UPDATE sessions SET capabilities_json=json_set(capabilities_json,'$.mid_turn_steer',json('true'));
        UPDATE tasks SET state='{}';", ATTACH_WORLD.replace(":p", &format!("'{p}'")), if active {"READY"} else {"BLOCKED"}))?;
    let mut config = HostConfig::new(&world.project());
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    if active {
        config.runner = Some(RunnerConfig {
            executor: agent,
            context: hoh_core::decision::Context {
                required_capabilities: [("developer".to_owned(), Default::default())].into(),
                ..Default::default()
            },
        });
    }
    let host = runtime.block_on(start_when_free(config))?;
    if active {
        world.wait(
            "SELECT state FROM attempts WHERE task_id='T1'",
            "RUNNING",
            WAIT,
        )?;
    }
    Ok(host)
}

fn runtime() -> R<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()?)
}

fn attached(world: &world::World) -> R<(Pty, Hoh)> {
    let pty = Pty::new(34, 150)?;
    let child = world.hoh_pty(&pty, &["attach", "R"])?;
    pty::screen_until(&pty, 34, 150, WAIT, |s| {
        s.lines().next().is_some_and(|s| s.contains("run R"))
    })?;
    Ok((pty, child))
}

/// The marker is drawn only after the preceding input batch and its commands were handled.
fn submit(pty: &mut Pty, text: &str, marker: &str) -> R<String> {
    pty.send(format!("{text}\r{marker}").as_bytes())?;
    pty::screen_until(pty, 34, 150, WAIT, |s| {
        s.lines().any(|s| s == format!("> {marker}"))
    })
}

fn stop(pty: &mut Pty, child: &mut Hoh) -> R {
    signal(child, Signal::TERM)?;
    assert_eq!(exit_within(child, WAIT)?.code(), Some(143));
    pty.drained(WAIT)?;
    Ok(())
}

fn evidence(name: &str, value: &str) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(Path::new(&dir).join(name), value)?;
    }
    Ok(())
}

#[test]
fn test_007_03_picker_commands_and_explicit_agent_forwarding() -> R {
    let runtime = runtime()?;
    let world = world::World::named("s5-slash")?;
    let agent = Arc::new(RecordingAgent::default());
    let _cleanup = Cleanup(agent.clone());
    let host = fixture(&runtime, &world, true, agent.clone())?;
    let (mut pty, mut child) = attached(&world)?;
    // Enter on /help and the marker give an observable barrier even before the picker is implemented.
    pty.send(b"/")?;
    pty::screen_until(&pty, 34, 150, WAIT, |s| s.lines().any(|s| s == "> /"))?;
    let screen = pty::snapshot(&pty.output(), 34, 150)?;
    for command in [
        "/plan", "/build", "/auto", "/team", "/model", "/status", "/resume", "/help",
    ] {
        assert!(screen.contains(command), "missing {command}: {screen}");
    }
    evidence("test-007-03-menu.txt", &screen)?;
    pty.send(b"\x03")?;
    let before = world.query("SELECT count(*) FROM commands")?;
    for (i, text) in [
        "/foo",
        "/agent",
        "/agent /compact",
        "/agent developer",
        "/agent absent /compact",
    ]
    .iter()
    .enumerate()
    {
        let screen = submit(&mut pty, text, &format!("invalid{i}"))?;
        assert_eq!(
            world.query("SELECT count(*) FROM commands")?,
            before,
            "{screen}"
        );
        if *text == "/foo" {
            assert!(screen.contains("unknown command"), "{screen}");
        }
        pty.send(b"\x03")?;
    }
    for (i, (text, expected)) in [
        ("/st", "状态 RUNNING"),
        ("/help", "Shift+Tab"),
        ("/team", "编辑属后续任务"),
        ("/model", "编辑属后续任务"),
    ]
    .into_iter()
    .enumerate()
    {
        let screen = submit(&mut pty, text, &format!("local{i}"))?;
        assert!(screen.contains(expected), "{screen}");
        assert_eq!(world.query("SELECT count(*) FROM commands")?, before);
        pty.send(b"\x03")?;
    }
    submit(&mut pty, "/plan", "mode")?;
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='SetMode'")?,
        "1"
    );
    assert!(agent.inputs.lock().unwrap().is_empty());
    pty.send(b"\x03")?;
    let screen = submit(&mut pty, "/agent developer /compact", "sent")?;
    assert!(screen.contains("→ developer"), "{screen}");
    world.wait(
        "SELECT count(*) FROM input_queue WHERE target_role='developer' AND state='DELIVERED'",
        "1",
        WAIT,
    )?;
    assert_eq!(
        *agent.inputs.lock().unwrap(),
        vec![("developer".into(), "/compact".into())]
    );
    assert_eq!(
        world
            .query("SELECT kind || ':' || json_extract(payload_json,'$.text') FROM input_queue")?,
        "STEER:/compact"
    );
    evidence("test-007-03-forward.txt", &screen)?;
    stop(&mut pty, &mut child)?;
    runtime.block_on(host.shutdown());
    Ok(())
}

#[test]
fn test_017_04_role_resolution_and_stale_epoch() -> R {
    let runtime = runtime()?;
    let world = world::World::named("s5-role")?;
    let agent = Arc::new(RecordingAgent::default());
    let _cleanup = Cleanup(agent.clone());
    let host = fixture(&runtime, &world, true, agent.clone())?;
    let (mut pty, mut child) = attached(&world)?;
    let screen = submit(&mut pty, "@claude 先写测试", "routed")?;
    assert_eq!(
        world.query("SELECT target_role FROM input_queue")?,
        "developer",
        "{screen}"
    );
    assert!(screen.contains("→ developer"), "{screen}");
    world.wait("SELECT state FROM input_queue", "DELIVERED", WAIT)?;
    assert_eq!(
        *agent.inputs.lock().unwrap(),
        vec![("developer".into(), "先写测试".into())]
    );
    evidence("test-017-04-resolved.txt", &screen)?;
    // ASK cannot interrupt this turn, so it remains queued for the next attempt.
    let revision: i64 = world
        .query("SELECT revision FROM runs WHERE run_id='R'")?
        .parse()?;
    let ask = json!({"schema_version":"1.0","command_id":"late","idempotency_key":"late","kind":"SendInput","run_id":"R","expected_revision":revision,
        "payload":{"text":"late input","input_kind":"ASK","target_role":"developer"}});
    assert_eq!(
        runtime
            .block_on(world::exchange(&host, "POST", "/v1/commands", Some(&ask)))?
            .0,
        202
    );
    let id = hoh_core::ids::child_id("late", "input");
    world.wait(
        &format!("SELECT state FROM outbox WHERE message_id='{id}'"),
        "ACKED",
        WAIT,
    )?;
    let receipt = world.query(&format!(
        "SELECT result_json FROM receipts WHERE message_id='{id}'"
    ))?;
    // Hold the per-role delivery dispatcher in flight on one steer so a second epoch-1 input stays PENDING in outbox
    // until after the host's runner applies the rebind checkpoint; then the host's own delivery loop rejects it.
    agent
        .hold_next_steer
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let revision: i64 = world
        .query("SELECT revision FROM runs WHERE run_id='R'")?
        .parse()?;
    let hold_cmd = json!({"schema_version":"1.0","command_id":"hold-steer","idempotency_key":"hold-steer","kind":"SendInput","run_id":"R","expected_revision":revision,
        "payload":{"text":"hold steer","input_kind":"STEER","target_role":"developer"}});
    assert_eq!(
        runtime
            .block_on(world::exchange(
                &host,
                "POST",
                "/v1/commands",
                Some(&hold_cmd)
            ))?
            .0,
        202
    );
    runtime.block_on(async { tokio::time::timeout(WAIT, agent.steer_entered.notified()).await })?;
    let revision: i64 = world
        .query("SELECT revision FROM runs WHERE run_id='R'")?
        .parse()?;
    let late_outbox = json!({"schema_version":"1.0","command_id":"late-outbox","idempotency_key":"late-outbox","kind":"SendInput","run_id":"R","expected_revision":revision,
        "payload":{"text":"late outbox input","input_kind":"STEER","target_role":"developer"}});
    assert_eq!(
        runtime
            .block_on(world::exchange(
                &host,
                "POST",
                "/v1/commands",
                Some(&late_outbox)
            ))?
            .0,
        202
    );
    let late_outbox_id = hoh_core::ids::child_id("late-outbox", "input");
    assert_eq!(
        world.query(&format!(
            "SELECT state || ':' || role_epoch FROM outbox WHERE message_id='{late_outbox_id}'"
        ))?,
        "PENDING:1"
    );
    let team_revision: i64 = world
        .query("SELECT max(revision) FROM team_revisions")?
        .parse()?;
    let change = json!({"schema_version":"1.0","command_id":"rebind","idempotency_key":"rebind","kind":"ConfigureTeam","run_id":"R","expected_revision":team_revision,
        "payload":{"bindings":{"developer":{"agent":"codex"}},"apply_at":"checkpoint"}});
    let (status, reply) = runtime.block_on(world::exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&change),
    ))?;
    assert_eq!(status, 202, "{reply}");
    // Release the fake turn through its pipe; the host runner settles it and applies the rebind checkpoint itself.
    for mut stdin in agent.releases.lock().unwrap().drain(..) {
        use std::io::Write as _;
        stdin.write_all(b"done\n")?;
    }
    world.wait(
        "SELECT state FROM attempts WHERE task_id='T1'",
        "SETTLED",
        WAIT,
    )?;
    world.wait(
        "SELECT agent || ':' || role_epoch FROM role_bindings WHERE role='developer'",
        "codex:2",
        WAIT,
    )?;
    // Unblock the held steer; the host's own input delivery loop delivers late-outbox (epoch 1) and sweeps late (ASK, epoch 1).
    agent.steer_release.notify_one();
    world.wait(
        &format!("SELECT state FROM outbox WHERE message_id='{late_outbox_id}'"),
        "ACKED",
        WAIT,
    )?;
    for stale_id in [&late_outbox_id, &id] {
        world.wait(
            &format!("SELECT state FROM input_queue WHERE input_id='{stale_id}'"),
            "REJECTED",
            WAIT,
        )?;
        assert_eq!(
            world.query(&format!(
                "SELECT json_extract(payload_json,'$.reason') FROM events WHERE kind='input.rejected' AND json_extract(payload_json,'$.input_id')='{stale_id}'"
            ))?,
            "STALE_ROLE_EPOCH",
            "{stale_id}"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE kind='input.rejected' AND json_extract(payload_json,'$.input_id')='{stale_id}'"
            ))?,
            "1",
            "{stale_id}"
        );
    }
    // Current-epoch inputs submitted through HTTP stay QUEUED across the host's sweep loop.
    let mut current_ids = Vec::new();
    for (cmd_id, kind) in [("current-ask", "ASK"), ("current-steer", "STEER")] {
        let revision: i64 = world
            .query("SELECT revision FROM runs WHERE run_id='R'")?
            .parse()?;
        let body = json!({"schema_version":"1.0","command_id":cmd_id,"idempotency_key":cmd_id,"kind":"SendInput","run_id":"R","expected_revision":revision,
            "payload":{"text":cmd_id,"input_kind":kind,"target_role":"developer"}});
        assert_eq!(
            runtime
                .block_on(world::exchange(&host, "POST", "/v1/commands", Some(&body)))?
                .0,
            202
        );
        current_ids.push(hoh_core::ids::child_id(cmd_id, "input"));
    }
    // Wait past at least two 200ms ticks of the host's input delivery/sweep loop.
    std::thread::sleep(Duration::from_millis(500));
    for current_id in current_ids {
        assert_eq!(
            world.query(&format!(
                "SELECT state || ':' || json_extract(payload_json,'$.role_epoch') FROM input_queue WHERE input_id='{current_id}'"
            ))?,
            "QUEUED:2",
            "host sweep must keep current-epoch {current_id}"
        );
    }
    assert_eq!(
        world.query(&format!(
            "SELECT result_json FROM receipts WHERE message_id='{id}'"
        ))?,
        receipt,
        "routing receipt remains immutable"
    );
    assert_eq!(
        world.query(
            "SELECT state FROM input_queue WHERE json_extract(payload_json,'$.text')='先写测试'"
        )?,
        "APPLIED"
    );
    let rows =
        world.rows("SELECT target_role,state,payload_json FROM input_queue ORDER BY rowid")?;
    evidence(
        "test-017-04-inputs.json",
        &serde_json::to_string_pretty(&rows)?,
    )?;
    stop(&mut pty, &mut child)?;
    runtime.block_on(host.shutdown());
    Ok(())
}

#[test]
fn test_017_04_ambiguous_agent_picker_cancel_and_select() -> R {
    let runtime = runtime()?;
    let world = world::World::named("s5-picker")?;
    let agent = Arc::new(RecordingAgent::default());
    let host = fixture(&runtime, &world, false, agent)?;
    let (mut pty, mut child) = attached(&world)?;
    pty.send("@claude 先写测试\r".as_bytes())?;
    let screen = pty::screen_until(&pty, 34, 150, WAIT, |s| s.contains("选择目标职位"))?;
    assert!(
        screen.contains("cto") && screen.contains("developer"),
        "{screen}"
    );
    assert_eq!(world.query("SELECT count(*) FROM input_queue")?, "0");
    pty.send(b"\x1b")?;
    pty::screen_until(&pty, 34, 150, WAIT, |s| {
        s.lines().any(|s| s == "> @claude 先写测试")
    })?;
    assert_eq!(world.query("SELECT count(*) FROM input_queue")?, "0");
    pty.send(b"\r")?;
    pty::screen_until(&pty, 34, 150, WAIT, |s| s.contains("选择目标职位"))?;
    let screen = submit(&mut pty, "\x1b[B", "chosen")?;
    assert_eq!(
        world.query("SELECT target_role FROM input_queue")?,
        "developer",
        "{screen}"
    );
    assert!(screen.contains("→ developer"), "{screen}");
    evidence("test-017-04-picker.txt", &screen)?;
    stop(&mut pty, &mut child)?;
    runtime.block_on(host.shutdown());
    Ok(())
}
