//! M3B-150: real scheduled notices and the host's STARTING gate, with a fake executor.
use super::*;
use hoh_app::host::{
    Executor, Gate, HostHooks, Outcome, Refusal, RunnerConfig, RunnerPoint, Started, Work,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Debug)]
struct Barrier {
    point: RunnerPoint,
    entered: AtomicBool,
    release: AtomicBool,
}

impl HostHooks for Barrier {
    fn runner_of(&self, point: RunnerPoint, attempt: &str) {
        if point == self.point && attempt.starts_with("Q/") {
            self.entered.store(true, Ordering::SeqCst);
            tokio::task::block_in_place(|| {
                while !self.release.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
    }
}

struct Release(Arc<Barrier>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct Gated {
    spawned: Mutex<Vec<(u32, String)>>,
    opened: Arc<AtomicBool>,
    reaped: Arc<AtomicBool>,
}

impl Executor for Gated {
    fn deliver(&self, work: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (send, receive) = tokio::sync::oneshot::channel();
        let _ = send.send(if work.task_id == "Q" {
            Ok(())
        } else {
            Err("not this fixture's task".into())
        });
        receive
    }

    fn start(&self, work: &Work) -> Result<Started, Refusal> {
        use std::os::unix::process::CommandExt as _;
        use std::process::{Command, Stdio};
        let mut child = Command::new("sh")
            .args(["-c", "read -r go"])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Refusal::from(e.to_string()))?;
        let pid = child.id();
        let start = hoh_agents::identity::start_id(pid)
            .map_err(|e| Refusal::from(e.to_string()))?
            .ok_or_else(|| Refusal::from("gate identity".to_owned()))?;
        self.spawned
            .lock()
            .map_err(|e| Refusal::from(e.to_string()))?
            .push((pid, start));
        let stdin = child.stdin.take();
        let opened = self.opened.clone();
        let gate = Gate::new(move || {
            use std::io::Write as _;
            opened.store(true, Ordering::SeqCst);
            stdin
                .ok_or("gate pipe")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        let (send, ended) = tokio::sync::oneshot::channel();
        let reaped = self.reaped.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            reaped.store(true, Ordering::SeqCst);
            let _ = send.send(Outcome {
                success: status.is_ok_and(|s| s.success()),
                detail: "fake gate ended".into(),
                group_gone: hoh_agents::identity::group_empty(pid),
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: Default::default(),
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(Started {
            pid,
            gate,
            ended,
            prompt: None,
            ran: None,
        })
    }
}

async fn until(done: impl Fn() -> bool) -> R {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_033_02_approval_before_claim_or_gate_open_never_runs_the_agent() -> R {
    use hoh_core::handoff::{Recipient, Verification};
    for point in [RunnerPoint::AfterAck, RunnerPoint::AfterSpawn] {
        let f = Fixture::new().await?;
        f.host.shutdown().await;
        f.world.exec(
            "UPDATE attempts SET state='CANCELLED' WHERE attempt_id!='a-D';
            UPDATE tasks SET state='CANCELLED' WHERE task_id IN ('V','other','writer');
            UPDATE tasks SET state='READY' WHERE task_id='Q';",
        )?;
        f.world.staff(&f.run)?;
        let candidate = f.world.rows("SELECT candidate_id AS id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256 FROM candidates")?.remove(0);
        let spec = candidate["spec_sha256"].as_str().ok_or("spec")?.to_owned();
        let handoffs = hoh_core::handoff::verification(
            &Verification {
                round_id: "round-s6".into(),
                run_id: f.run.clone(),
                task_id: "D".into(),
                attempt_id: "a-D".into(),
                candidate: serde_json::from_value(candidate)?,
                spec_sha256: spec,
                spec_revision: 1,
                acceptance_ids: vec!["AC-901-01".into()],
                goal: "verify cancellation".into(),
                task_revision: 0,
                cause: "round-opened".into(),
                references: vec![],
                budget_tokens: 32000,
            },
            &[
                Recipient {
                    role: "qa".into(),
                    role_epoch: 1,
                },
                Recipient {
                    role: "reviewer".into(),
                    role_epoch: 1,
                },
            ],
        )
        .ok_or("handoffs")?;
        for message in handoffs {
            let payload = serde_json::to_string(&message)?.replace('\'', "''");
            f.world.exec(&format!("INSERT INTO outbox(message_id,event_seq,run_id,recipient_role,role_epoch,payload_json,state)
                SELECT '{}',seq,'{}','{}',1,'{payload}','PENDING' FROM events WHERE event_id='round-opened'",message.message_id,f.run,message.recipient_role))?;
        }
        let barrier = Arc::new(Barrier {
            point,
            entered: AtomicBool::new(false),
            release: AtomicBool::new(false),
        });
        let executor = Arc::new(Gated {
            spawned: Mutex::new(vec![]),
            opened: Arc::new(AtomicBool::new(false)),
            reaped: Arc::new(AtomicBool::new(false)),
        });
        let mut config = HostConfig::new(&f.world.project());
        config.port = 0;
        config.store.wal_probe = Some(world::probe());
        config.hooks = barrier.clone();
        config.runner = Some(RunnerConfig {
            executor: executor.clone(),
            context: hoh_core::decision::Context {
                required_capabilities: [("qa".into(), Default::default())].into(),
                commander_dir: Some(f.world.state().join("hoh/commander")),
                ..Default::default()
            },
        });
        let host = f.world.start_host_with(config).await?;
        let release = Release(barrier.clone());
        let outcome = async {
            until(|| barrier.entered.load(Ordering::SeqCst)).await?;
            let id = f.world.query("SELECT attempt_id FROM attempts WHERE task_id='Q' AND state IN ('PREPARED','STARTING')")?;
            let prepared = point == RunnerPoint::AfterAck;
            assert_eq!(f.world.query(&format!("SELECT state FROM attempts WHERE attempt_id='{id}'"))?, if prepared { "PREPARED" } else { "STARTING" });
            let notice = if prepared {
                Some(host.store().runner_waiting(&host.project_id).await?.dispatches.into_iter().find(|n| n.attempt_id == id).ok_or("real dispatch notice")?)
            } else { None };
            assert_eq!(f.world.query(&format!("SELECT count(*) FROM events WHERE kind='runner.received' AND json_extract(payload_json,'$.attempt_id')='{id}'"))?, if prepared { "0" } else { "1" });
            let hash = change(&f.world)?;
            propose(&host, &f.run, &hash).await?;
            approve(&host, &f.run, &hash).await?;
            assert_eq!(f.world.query(&format!("SELECT cancel_requested FROM attempts WHERE attempt_id='{id}'"))?, "1");
            if let Some(notice) = notice {
                assert!(!host.store().runner_received(notice, SystemTime::now()).await?, "approval must fence the real notice before claim");
            } else {
                let pid = executor.spawned.lock().map_err(|_| "spawn records")?[0].0;
                let who = hoh_agents::identity::of(pid)?.ok_or("gate identity")?;
                assert!(!host.store().runner_running(&id, hoh_agents::identity::running(&who), json!({"attempt_id":id,"tracking":"proc"})).await?,
                    "approval must refuse the STARTING commit before opening the gate");
            }
            barrier.release.store(true, Ordering::SeqCst);
            f.world.until(&format!("SELECT state FROM attempts WHERE attempt_id='{id}'"), "CANCELLED", Duration::from_secs(30)).await?;
            if !prepared { until(|| executor.reaped.load(Ordering::SeqCst)).await?; }
            assert!(!executor.opened.load(Ordering::SeqCst), "the STARTING gate must be dropped unopened");
            let spawned = executor.spawned.lock().map_err(|_| "spawn records")?;
            assert_eq!(spawned.len(), usize::from(!prepared), "PREPARED never spawns; STARTING only spawns its closed gate helper");
            for (pid, start) in spawned.iter() { assert!(hoh_agents::identity::ended(*pid, Some(start))?); }
            assert_eq!(f.world.query(&format!("SELECT count(*) FROM events WHERE kind='effect.started' AND json_extract(payload_json,'$.attempt_id')='{id}'"))?, "0");
            Ok::<_, Box<dyn std::error::Error>>(())
        }.await;
        drop(release);
        host.shutdown().await;
        outcome?;
    }
    Ok(())
}
