//! Product host with scripted agents; these diagnostics are not acceptance or live-agent evidence.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
mod pty;
mod world;
use pty::R;
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone, Copy)]
enum EarlyDraftFailure {
    Prepare,
    GitInit,
    Identity,
    Refusal,
}

impl hoh_app::host::HostHooks for EarlyDraftFailure {
    fn draft_discovered(&self, project: &std::path::Path, _: &str) -> Result<(), String> {
        if matches!(self, Self::Prepare) {
            // Inject only after the read-only baseline, so this exercises prepare,
            // not discovery's refusal to inspect a malformed Git repository.
            std::fs::create_dir(project.join(".git")).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

// Fault-only executor: it creates no process and never opens a work gate.
#[derive(Debug)]
struct FailingDraftStart {
    failure: EarlyDraftFailure,
    starts: std::sync::atomic::AtomicUsize,
}
impl hoh_app::host::Executor for FailingDraftStart {
    fn deliver(
        &self,
        _: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (send, ack) = tokio::sync::oneshot::channel();
        let _ = send.send(Ok(()));
        ack
    }
    fn start(
        &self,
        _: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if matches!(self.failure, EarlyDraftFailure::Refusal) {
            return Err(hoh_app::host::Refusal {
                reason: "scripted auth refusal".into(),
                class: hoh_core::retry::FailureClass::Auth,
                detail: None,
                details: None,
            });
        }
        let (_, ended) = tokio::sync::oneshot::channel();
        Ok(hoh_app::host::Started {
            pid: u32::MAX, // No such /proc identity; this fixture spawned nothing.
            gate: hoh_app::host::Gate::new(|| panic!("invalid identity must never open a gate")),
            ended,
            prompt: None,
            ran: None,
        })
    }
}

async fn early_draft_failure(failure: EarlyDraftFailure) -> R {
    #[derive(Debug)]
    struct FixedClock(std::time::SystemTime);
    impl hoh_core::delivery::Clock for FixedClock {
        fn now(&self) -> std::time::SystemTime {
            self.0
        }
    }
    let world = world::World::named("m3t04-start-failure")?;
    world.configure(&[], "")?;
    if !matches!(
        failure,
        EarlyDraftFailure::Prepare | EarlyDraftFailure::GitInit
    ) {
        world.git(&["init", "-q"])?;
    }
    let executor = std::sync::Arc::new(FailingDraftStart {
        failure,
        starts: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut config = hoh_app::host::HostConfig::new(&world.project());
    config.port = 0;
    config.hooks = std::sync::Arc::new(failure);
    config.store.wal_probe = Some(world::probe());
    let now = std::time::SystemTime::now();
    config.clock = hoh_app::host::HostClock(std::sync::Arc::new(FixedClock(now)));
    config.jitter_seed = Some(7);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: hoh_core::decision::Context {
            required_capabilities: world::TEAM
                .iter()
                .map(|(role, _)| ((*role).into(), Default::default()))
                .collect(),
            ..Default::default()
        },
    });
    let host = hoh_app::host::Host::start(config).await?;
    let permissions = std::fs::metadata(world.project())?.permissions();
    let result: R = async {
        let run = world::start_run(&host, "start", "scripted start failure").await?;
        world.staff(&run)?;
        if matches!(failure, EarlyDraftFailure::GitInit) {
            use std::os::unix::fs::PermissionsExt;
            // The host's existing .hoh directory remains writable; only git init's creation
            // of .git fails, after the Runtime has recorded its STARTED effect.
            std::fs::set_permissions(world.project(), std::fs::Permissions::from_mode(0o555))?;
        }
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        world.until("SELECT count(*)>0 FROM attempts WHERE ended_at IS NOT NULL", "1", Duration::from_secs(15)).await?;
        std::fs::set_permissions(world.project(), permissions.clone())?;
        if matches!(failure, EarlyDraftFailure::GitInit) {
            let detail = world.query("SELECT failure_reason FROM attempts")?;
            assert!(detail.contains("git init --quiet: exit status: 1"), "{detail}");
            assert_eq!(world.query("SELECT state FROM attempts")?, "UNKNOWN_EFFECT");
            let key = hoh_core::ids::child_id(&world.query("SELECT attempt_id FROM attempts")?, "git-init");
            assert_eq!(world.query(&format!("SELECT state FROM effects WHERE effect_key='{key}'"))?, "UNKNOWN");
            assert_eq!(world.query("SELECT state||'/'||attempt_count FROM tasks WHERE kind='spec_draft'")?, "LEASED/0");
            assert_eq!(world.query("SELECT count(*) FROM tasks WHERE next_retry_at IS NOT NULL")?, "0");
            assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='runner.diagnostic' AND json_extract(payload_json,'$.code')='UNKNOWN_EFFECT'")?, "1");
            assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='attempt.workspace' AND json_extract(payload_json,'$.from')='STARTING' AND json_extract(payload_json,'$.to')='UNKNOWN_EFFECT'")?, "1");
            assert_eq!(world.query("SELECT count(*) FROM effects WHERE state='PREPARED'")?, "1");
            assert!(!host.store().dispatch_draft(&run, hoh_core::decision::Context { now: Some(now + Duration::from_secs(3600)), ..Default::default() }).await?);
            assert_eq!(world.query("SELECT count(*) FROM attempts")?, "1");
            assert_eq!(executor.starts.load(std::sync::atomic::Ordering::SeqCst), 0);
            return Ok(());
        }
        assert_eq!(world.query("SELECT state FROM attempts ORDER BY rowid LIMIT 1")?, "FAILED");
        assert_eq!(world.query("SELECT attempt_count FROM tasks WHERE kind='spec_draft'")?, "1");
        match failure {
            EarlyDraftFailure::Refusal => {
                assert_eq!(world.query("SELECT state||'/'||blocked_reason FROM tasks WHERE kind='spec_draft'")?, "BLOCKED/BLOCKED_AUTH");
                assert_eq!(world.query("SELECT count(*) FROM tasks WHERE next_retry_at IS NOT NULL")?, "0");
            }
            _ => {
                assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?, "READY");
                assert_eq!(world.query("SELECT count(*) FROM tasks t JOIN attempts a USING(task_id) WHERE t.next_retry_at>a.ended_at")?, "1");
                let detail = world.query("SELECT failure_reason FROM attempts")?;
                assert!(match failure {
                    EarlyDraftFailure::Prepare => detail.contains("rev-parse --show-toplevel: exit status: 128"),
                    _ => detail.contains("identity of its process could not be read"),
                }, "{detail}");
            }
        }
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='attempt.workspace_start'")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state<>'PREPARED'")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM leases l JOIN attempts a USING(attempt_id) WHERE l.expires_at<=a.ended_at")?, "1");
        assert!(!host.store().dispatch_draft(&run, hoh_core::decision::Context { now: Some(now), ..Default::default() }).await?);
        assert_eq!(world.query("SELECT count(*) FROM attempts")?, "1");
        assert_eq!(executor.starts.load(std::sync::atomic::Ordering::SeqCst), usize::from(!matches!(failure, EarlyDraftFailure::Prepare)));
        Ok(())
    }.await;
    std::fs::set_permissions(world.project(), permissions)?;
    host.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_m3l600_failed_git_init_keeps_unknown_effect() -> R {
    early_draft_failure(EarlyDraftFailure::GitInit).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_m3l500_prepare_failure_keeps_backoff() -> R {
    early_draft_failure(EarlyDraftFailure::Prepare).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_m3l500_identity_failure_keeps_backoff() -> R {
    early_draft_failure(EarlyDraftFailure::Identity).await
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_m3l500_executor_refusal_keeps_class() -> R {
    early_draft_failure(EarlyDraftFailure::Refusal).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_first_draft_uses_runtime_writer_and_git_init_effect() -> R {
    let world = world::World::named("m3t04-drafting")?;
    world.configure(&[], "")?;
    let team = world::Team::working(|_| Vec::new(), Duration::from_secs(60));
    let host = world.team_host(team, None).await?;
    let outcome=async {
        let run=world::start_run(&host,"draft-start","起草规格").await?;
        world.staff(&run)?;
        assert_eq!(world.query("SELECT count(*) FROM tasks")?,"0");
        let revision:i64=world.query("SELECT revision FROM runs")?.parse()?;
        let body=json!({"schema_version":"1.0","command_id":"draft","idempotency_key":"draft",
            "expected_revision":revision,"kind":"SendInput","run_id":run,
            "payload":{"text":"draft","target_role":null,"input_kind":"REPLY"}});
        let (status,result)=world::exchange(&host,"POST","/v1/commands",Some(&body)).await?;
        assert_eq!(status,202,"{result}");
        world.until("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft' AND a.state='RUNNING'","1",Duration::from_secs(15)).await?;
        assert_eq!(world.query("SELECT state||'/'||mode||'/'||coalesce(spec_id,'NULL') FROM runs")?,"SPECIFYING/PLAN/NULL");
        assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft' AND spec_id IS NULL AND role='cto' AND writer=1")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM task_requirements")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.drafting'")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='schedule.round'")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state='COMMITTED'")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='run.baseline'")?,"1");
        assert_eq!(world.query("SELECT (SELECT seq FROM events WHERE kind='run.baseline') < (SELECT min(seq) FROM events WHERE kind IN ('spec.drafting','spec.draft_dispatched'))")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM leases WHERE kind='writer'")?,"1");
        assert!(world.project().join(".git").is_dir());
        assert!(!std::fs::read_to_string(world.project().join(".git/config"))?.contains("[remote"));
        let phases=world.rows("SELECT json_extract(payload_json,'$.to') AS state FROM events WHERE kind='run.transitioned'")?;
        assert_eq!(phases,vec![json!({"state":"DISCOVERING"}),json!({"state":"SPECIFYING"})]);
        std::fs::create_dir_all(world.project().join("docs/spec"))?;
        std::fs::write(world.project().join("docs/spec/kept.md"),"用户要求保留的起草内容")?;
        world.git(&["add","docs/spec/kept.md"])?;world.git(&["commit","-m","draft before cancellation"])?;
        let kept_commit=world.git(&["rev-parse","HEAD"])?;
        let second=world::start_run(&host,"draft-second","另一起草").await?;
        world.staff(&second)?;
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{second}'"))?.parse()?;
        let request=json!({"schema_version":"1.0","command_id":"draft-second-answer","idempotency_key":"draft-second-answer",
            "expected_revision":revision,"kind":"SendInput","run_id":second,
            "payload":{"text":"draft","target_role":null,"input_kind":"REPLY"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&request)).await?.0,202);
        world.until("SELECT count(*) FROM tasks WHERE kind='spec_draft'","2",Duration::from_secs(15)).await?;
        assert_eq!(world.query(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{second}'"))?,"0","one project writer");
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse()?;
        let cancel=json!({"schema_version":"1.0","command_id":"cancel","idempotency_key":"cancel",
            "expected_revision":revision,"kind":"CancelRun","run_id":run,"payload":{"reason":"test"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&cancel)).await?.0,202);
        world.until(&format!("SELECT state FROM runs WHERE run_id='{run}'"),"CANCELLED",Duration::from_secs(15)).await?;
        assert_eq!(world.query(&format!("SELECT state FROM tasks WHERE kind='spec_draft' AND run_id='{run}'"))?,"CANCELLED");
        assert!(world.project().join(".git").exists(),"cancellation preserves the workspace");
        assert_eq!(std::fs::read_to_string(world.project().join("docs/spec/kept.md"))?,"用户要求保留的起草内容");
        assert_eq!(world.git(&["rev-parse","HEAD"])?,kept_commit,"cancel must not reset commits");
        world.until(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{second}' AND a.state='RUNNING'"),"1",Duration::from_secs(15)).await?;
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state='COMMITTED'")?,"1","existing repository is not initialized again");
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{second}'"))?.parse()?;
        let cancel=json!({"schema_version":"1.0","command_id":"cancel-second","idempotency_key":"cancel-second",
            "expected_revision":revision,"kind":"CancelRun","run_id":second,"payload":{"reason":"test"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&cancel)).await?.0,202);
        world.until(&format!("SELECT state FROM runs WHERE run_id='{second}'"),"CANCELLED",Duration::from_secs(15)).await?;
        Ok(())
    }.await;
    host.shutdown().await;
    outcome
}

async fn input(
    host: &hoh_app::host::Host,
    world: &world::World,
    run: &str,
    key: &str,
    kind: &str,
    payload: serde_json::Value,
) -> R<serde_json::Value> {
    for _ in 0..20 {
        let revision: i64 = world
            .query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?
            .parse()?;
        let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,"expected_revision":revision,"kind":kind,"run_id":run,"payload":payload});
        let (status, reply) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
        if status == 409 && reply["code"] == "STALE_REVISION" {
            continue;
        }
        assert_eq!(status, 202, "{reply}");
        return Ok(reply);
    }
    Err("run kept changing".into())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_first_draft_completes_then_all_three_user_entrances_approve() -> R {
    for kind in ["ApproveSpec", "AnswerQuestion", "SendInput"] {
        let world = world::World::named("m3t04-draft-approval")?;
        world.configure(&[], "")?;
        let host = world
            .team_host(world::Team::drafting(world.project()), None)
            .await?;
        let result: R=async{
   let run=world::start_run(&host,"start","规格起草").await?;world.staff(&run)?;
   let second=world::start_run(&host,"second","等待规格").await?;world.staff(&second)?;
   if kind == "SendInput" {
    let mut cli=world.command(&["-p","draft","--resume",&run]);
    let output=tokio::task::spawn_blocking(move ||cli.output()).await??;
    assert_eq!(output.status.code(),Some(3),"{}",String::from_utf8_lossy(&output.stderr));
   } else {
   input(&host,&world,&run,"draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   }
   world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(20)).await?;
   assert_eq!(world.query("SELECT count(*) FROM candidates")?,"0");
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");
   assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,"IMPLEMENTED");
   assert_eq!(world.query(&format!("SELECT state||'/'||resume_state FROM runs WHERE run_id='{run}'"))?,"BLOCKED/SPECIFYING");
   let q:serde_json::Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval' ORDER BY seq DESC LIMIT 1")?)?;
   let payload=match kind{
    "ApproveSpec"=>json!({"spec_sha256":q["spec_sha256"],"decision":"APPROVE_ONLY"}),
    "AnswerQuestion"=>json!({"question_id":q["question_id"],"choice_id":"approve","answer":"approve"}),
    _=>json!({"text":"approve","target_role":null,"input_kind":"REPLY"}),
   };
   if kind == "SendInput" {
    let mut cli=world.command(&["-p","approve","--resume",&run,"--detach"]);
    let output=tokio::task::spawn_blocking(move ||cli.output()).await??;
    assert_eq!(output.status.code(),Some(0),"{}",String::from_utf8_lossy(&output.stderr));
   } else { input(&host,&world,&run,"approval",kind,payload).await?; }
   assert_eq!(world.query("SELECT count(*) FROM specs WHERE revision=1")?,"1");
   assert_eq!(world.query("SELECT state||'/'||coalesce(spec_id,'NULL') FROM tasks WHERE kind='spec_draft'")?,"ACCEPTED/NULL");
   assert_eq!(world.query("SELECT count(*) FROM commands WHERE kind='ApproveSpec'")?,"1");
   input(&host,&world,&second,"adopt-draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,"1");
   assert_eq!(world.query(&format!("SELECT count(*) FROM runs WHERE run_id='{second}' AND spec_id IS NOT NULL"))?,"1");
   assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.drafting'")?,"1");
   Ok(())
  }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}

async fn marked_request(
    host: &hoh_app::host::Host,
    body: &serde_json::Value,
) -> R<(u16, serde_json::Value)> {
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        host.port,
        host.token,
        body.to_string().len(),
        body
    );
    let port = host.port.to_string();
    let output = tokio::task::spawn_blocking(move || {
        world::approval::python(
            r#"
import socket,sys
s=socket.create_connection(('127.0.0.1',int(sys.argv[1])),15)
s.sendall(sys.argv[2].encode())
while True:
 data=s.recv(65536)
 if not data: break
 sys.stdout.buffer.write(data)
"#,
            &[("HOH_ATTEMPT_ID", "another-project/attempt")],
        )
        .args([port, request])
        .output()
    })
    .await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = String::from_utf8(output.stdout)?;
    let (header, body) = response.split_once("\r\n\r\n").ok_or("response")?;
    Ok((
        header.split_whitespace().nth(1).ok_or("status")?.parse()?,
        serde_json::from_str(body)?,
    ))
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_05_answer_and_reply_refuse_attempt_origin_before_application_and_replay() -> R {
    for kind in ["AnswerQuestion", "SendInput"] {
        let world = world::World::named("m3t04-draft-origin")?;
        world.configure(&[], "")?;
        let host = world
            .team_host(world::Team::drafting(world.project()), None)
            .await?;
        let result:R=async{
   let run=world::start_run(&host,"start","draft authority").await?;world.staff(&run)?;
   input(&host,&world,&run,"draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(20)).await?;
   let q:serde_json::Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval'")?)?;
   let payload=if kind=="AnswerQuestion"{json!({"question_id":q["question_id"],"choice_id":"approve","answer":"approve"})}else{json!({"text":"approve","target_role":null,"input_kind":"REPLY"})};
   let revision:i64=world.query("SELECT revision FROM runs")?.parse()?;
   let body=json!({"schema_version":"1.0","command_id":"approve","idempotency_key":"approve","run_id":run,"expected_revision":revision,"kind":kind,"payload":payload});
   for _ in 0..2{
    let (status,rejected)=marked_request(&host,&body).await?;
    assert_eq!(status,403,"{rejected}");assert_eq!(rejected["code"],"SPEC_APPROVAL_NOT_USER");assert_eq!(rejected["details"]["reason"],"marker");
    assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");assert_eq!(world.query("SELECT count(*) FROM commands WHERE command_id='approve'")?,"0");
    assert_eq!(world.query("SELECT revision FROM runs")?,revision.to_string());
   }
   assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&body)).await?.0,202);
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");
   assert_eq!(marked_request(&host,&body).await?.0,403,"origin guard precedes the user's accepted replay");
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");Ok(())
  }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_04_m3l102_failed_draft_checks_committed_product_writes() -> R {
    let world = world::World::named("m3t04-failed-draft")?;
    world.configure(&[], "")?;
    let host = world
        .team_host(
            world::Team::working(|_| Vec::new(), Duration::from_secs(3)),
            None,
        )
        .await?;
    let result: R = async {
        let run = world::start_run(&host, "start", "draft boundary on failure").await?;
        world.staff(&run)?;
        input(
            &host,
            &world,
            &run,
            "draft",
            "SendInput",
            json!({"text":"draft","target_role":null,"input_kind":"REPLY"}),
        )
        .await?;
        world
            .until(
                "SELECT count(*) FROM attempts WHERE state='RUNNING'",
                "1",
                Duration::from_secs(15),
            )
            .await?;
        std::fs::write(
            world.project().join("product.rs"),
            "outside the draft boundary",
        )?;
        world.git(&["add", "product.rs"])?;
        world.git(&["commit", "-m", "scripted draft violates boundary"])?;
        world
            .until(
                "SELECT count(*) FROM attempts WHERE state='FAILED'",
                "1",
                Duration::from_secs(15),
            )
            .await?;
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='plan.write_violation'")?,
            "1"
        );
        assert_eq!(
            world.query("SELECT state||'/'||resume_state FROM runs")?,
            "BLOCKED/SPECIFYING"
        );
        assert_eq!(
            world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,
            "BLOCKED"
        );
        assert_eq!(world.query("SELECT count(*) FROM leases l JOIN attempts a ON a.attempt_id=l.attempt_id WHERE a.state IN ('PREPARED','STARTING','RUNNING')")?, "0");
        assert_eq!(
            std::fs::read_to_string(world.project().join("product.rs"))?,
            "outside the draft boundary"
        );
        Ok(())
    }
    .await;
    host.shutdown().await;
    result
}

#[derive(Debug)]
struct DraftBarrier {
    point: hoh_app::host::RunnerPoint,
    hits: std::sync::atomic::AtomicUsize,
    release: std::sync::atomic::AtomicBool,
}
impl hoh_app::host::HostHooks for DraftBarrier {
    fn runner_of(&self, point: hoh_app::host::RunnerPoint, _attempt: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        if point == self.point && self.hits.fetch_add(1, SeqCst) == 0 {
            tokio::task::block_in_place(|| {
                while !self.release.load(SeqCst) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            });
        }
    }
}
struct ReleaseDraft(std::sync::Arc<DraftBarrier>);
impl Drop for ReleaseDraft {
    fn drop(&mut self) {
        self.0
            .release
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_009_03_m3l150_dispatch_then_set_mode_releases_draft_with_proof() -> R {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    for point in [
        hoh_app::host::RunnerPoint::AfterAck,
        hoh_app::host::RunnerPoint::AfterSpawn,
    ] {
        let world = world::World::named("m3t04-mode-gate")?;
        world.configure(&[], "")?;
        world.git(&["init", "-q"])?;
        let barrier = std::sync::Arc::new(DraftBarrier {
            point,
            hits: AtomicUsize::new(0),
            release: AtomicBool::new(false),
        });
        let release = ReleaseDraft(barrier.clone());
        let host = world
            .team_host_hooked(world::Team::drafting(world.project()), barrier.clone())
            .await?;
        let result: R = async {
            let run = world::start_run(&host, "start", "mode race").await?;
            world.staff(&run)?;
            input(
                &host,
                &world,
                &run,
                "draft",
                "SendInput",
                json!({"text":"draft","target_role":null,"input_kind":"REPLY"}),
            )
            .await?;
            tokio::time::timeout(Duration::from_secs(15), async {
                while barrier.hits.load(SeqCst) == 0 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            input(
                &host,
                &world,
                &run,
                "build",
                "SetMode",
                json!({"mode":"BUILD"}),
            )
            .await?;
            assert_eq!(
                world.query("SELECT count(*) FROM events WHERE kind='run.mode_change_pending'")?,
                "1"
            );
            barrier.release.store(true, SeqCst);
            tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if barrier.hits.load(SeqCst) > 1
                        || world
                            .query("SELECT mode FROM runs")
                            .is_ok_and(|m| m == "BUILD")
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await?;
            assert_eq!(world.query("SELECT mode FROM runs")?, "BUILD", "{point:?}");
            assert_eq!(
                world.query(
                    "SELECT count(*) FROM attempts WHERE state IN ('PREPARED','STARTING','RUNNING')"
                )?,
                "0"
            );
            assert_eq!(
                world.query("SELECT count(*) FROM events WHERE kind='spec.draft_start_refused'")?,
                "1",
                "{point:?} refusal needs a termination proof: {:?}", world.rows("SELECT kind,payload_json FROM events WHERE kind LIKE '%refus%' OR kind='attempt.moved' OR kind='reconcile.attempt'")?
            );
            assert_eq!(
                world.query("SELECT count(*) FROM events WHERE kind='spec.draft_completed'")?,
                "0"
            );
            assert!(
                !world.project().join("docs/spec/main.md").exists(),
                "the rejected gate never ran draft code"
            );
            Ok(())
        }
        .await;
        drop(release);
        host.shutdown().await;
        result?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_009_01_build_host_rejects_unknown_req_then_imports_approved_plan() -> R {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use world::Team as FakeCommander;

    let world = world::World::named("m3preqa-g5-build-plan")?;
    world.configure(&[], "")?;
    world.commit_project(&[world::Requirement {
        id: "REQ-A".into(),
        title: "Approved requirement".into(),
        criteria: vec![world::Criterion {
            id: "AC-A-01".into(),
            body: "Approved behavior".into(),
            test_id: "TEST-A-01".into(),
            passes: true,
        }],
    }])?;
    world.init_store().await?;
    let spec = world.seed_spec()?;
    // Team supplies a fake Commander through the real runner and message bridge; it takes no developer work.
    // Hold the second proposal until the test has observed the first decision's durable, empty import.
    let phase = Arc::new(AtomicUsize::new(0));
    let script_phase = phase.clone();
    let commander = FakeCommander::new(move |_| {
        let task = |id, req| {
            json!({"task_id":id,"title":id,"role":"developer","writer":true,
            "depends_on":[],"requirement_ids":[req]})
        };
        if script_phase
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            vec![world::rejected(
                json!({"kind":"create_tasks","plan":{"tasks":[
                    task("A", "REQ-A"), task("Z", "REQ-Z")
                ]}}),
            )]
        } else if script_phase
            .compare_exchange(2, 3, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            vec![json!({"kind":"create_tasks","plan":{"tasks":[task("A", "REQ-A")]}})]
        } else {
            Vec::new()
        }
    });
    let host = world.team_host(commander, None).await?;
    let result: R = async {
        let body = json!({"schema_version":"1.0","command_id":"start","idempotency_key":"start",
            "expected_revision":0,"kind":"StartRun","run_id":null,
            "payload":{"goal":"Build approved scope","mode":"BUILD","interactive":false}});
        let (status, accepted) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run_id"].as_str().ok_or("run")?;
        world.staff(run)?;
        assert_eq!(world.query("SELECT mode FROM runs")?, "BUILD");
        assert_eq!(world.query("SELECT spec_id FROM runs")?, spec);
        assert_eq!(world.query("SELECT requirement_id FROM requirements")?, "REQ-A");
        let decisions = "SELECT state,reason_code FROM decisions WHERE json_extract(payload_json,'$.kind')='create_tasks' ORDER BY rowid";
        world.until("SELECT count(*) FROM decisions WHERE json_extract(payload_json,'$.kind')='create_tasks'", "1", Duration::from_secs(30)).await?;
        assert_eq!(world.rows(decisions)?, vec![json!({"state":"REJECTED","reason_code":"ILLEGAL_TRANSITION"})]);
        let details: serde_json::Value = serde_json::from_str(&world.query(
            "SELECT json_extract(payload_json,'$.details') FROM events WHERE kind='decision.rejected'"
        )?)?;
        assert!(details.to_string().contains("BUILD 不改规格，用 /plan 或 CR"), "{details}");
        assert!(details.to_string().contains("REQ-Z"), "{details}");
        // Supervisor bookkeeping is the only task the host has created; neither proposed work task was imported.
        assert_eq!(world.query("SELECT count(*) FROM tasks WHERE role<>'commander'")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM task_requirements")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='plan.imported'")?, "0");

        phase.store(2, Ordering::SeqCst);
        input(&host, &world, run, "approved-only", "SendInput",
            json!({"text":"Plan only REQ-A","target_role":"commander","input_kind":"STEER"})).await?;
        world.until("SELECT count(*) FROM decisions WHERE json_extract(payload_json,'$.kind')='create_tasks'", "2", Duration::from_secs(30)).await?;
        assert_eq!(world.rows(decisions)?, vec![
            json!({"state":"REJECTED","reason_code":"ILLEGAL_TRANSITION"}),
            json!({"state":"APPLIED","reason_code":null}),
        ]);
        assert_eq!(world.rows("SELECT task_id FROM tasks WHERE role<>'commander'")?, vec![json!({"task_id":"A"})]);
        assert_eq!(world.rows("SELECT task_id,requirement_id,spec_id FROM task_requirements")?,
            vec![json!({"task_id":"A","requirement_id":"REQ-A","spec_id":spec})]);
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='plan.imported'")?, "1");
        assert_eq!(world.query("SELECT mode FROM runs")?, "BUILD");
        assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
        Ok(())
    }.await;
    host.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_009_02_auto_drafts_pending_cr_while_build_blocks() -> R {
    for (mode, interactive) in [("AUTO", true), ("AUTO", false), ("BUILD", false)] {
        let world = world::World::named("m3t04-s7-readiness")?;
        world.configure(&[], "")?;
        let original = world.commit_project(&[world::Requirement {
            id: "REQ-A".into(),
            title: "标题".into(),
            criteria: vec![],
        }])?;
        world.init_store().await?;
        let spec = world.seed_spec()?;
        let setup = world.start_plain_host(None).await?;
        let body = json!({"schema_version":"1.0","command_id":"start","idempotency_key":"start","expected_revision":0,"kind":"StartRun","run_id":null,"payload":{"goal":"补齐 AC","mode":mode,"interactive":interactive}});
        let (status, accepted) =
            world::exchange(&setup, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{accepted}");
        let run = accepted["run_id"].as_str().ok_or("run")?.to_owned();
        world.staff(&run)?;
        world.exec(&format!("UPDATE runs SET state='RUNNING' WHERE run_id='{run}'; INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,cost_estimate) VALUES('work','{run}','{spec}','READY','developer',1,1,1); INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES('work','REQ-A','{spec}'); INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES(:p,'{run}','s7-plan','plan.imported',json_object('run_id','{run}','tasks',json_array(json_object('task_id','work','paths',json_array('src/lib.rs')))),'2026-11-23T00:00:00Z');"))?;
        setup.shutdown().await;
        let source=hoh_core::specindex::Source{path:"docs/spec/fixture.md".into(),bytes:"# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A 标题\n### AC-A-01\ngiven: 前提\nwhen: 操作\nthen: 结果\ncheck: unit\n".as_bytes().to_vec()};
        let host = world
            .team_host(
                world::Team::planning(world.project(), source, |_| vec![], None),
                None,
            )
            .await?;
        let result:R=async {
            // Only unattended AUTO needs the CLI exit here. Sending a free answer to BUILD's open question
            // explicitly chooses its default PLAN, so it would no longer be the unchanged-BUILD scenario.
            let cli = if !interactive && mode=="AUTO" {let mut cli=world.command(&["-p","请补齐缺少的验收条件","--resume",&run]);Some(tokio::task::spawn_blocking(move||cli.output()))} else {None};
            if mode=="AUTO" {
                if let Err(e)=world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(25)).await {
                    return Err(format!("{e}; mode={mode}, interactive={interactive}; runs={:?}; tasks={:?}; events={:?}",world.rows("SELECT state,mode,blocked_reasons_json FROM runs")?,world.rows("SELECT task_id,kind,state,blocked_reason FROM tasks")?,world.rows("SELECT kind,payload_json FROM events ORDER BY seq DESC LIMIT 18")?).into());
                }
                assert_eq!(world.query("SELECT count(*) FROM change_requests WHERE state='PROPOSED'")?,"1");
                assert_eq!(world.query("SELECT state||'/'||blocked_reason||'/'||resume_state FROM runs")?,"BLOCKED/NEED_DECISION/RUNNING");
                assert_eq!(world.query("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft'")?,"1");
                input(&host,&world,&run,"build","SetMode",json!({"mode":"BUILD"})).await?;
            } else {
                world.until("SELECT state||'/'||blocked_reason FROM tasks WHERE task_id='work'","BLOCKED/READINESS_NOT_MET",Duration::from_secs(20)).await?;
                world.until("SELECT count(*) FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='build_readiness'","1",Duration::from_secs(20)).await?;
                assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,"0");
                let question=world.query("SELECT payload_json FROM events WHERE kind='question.asked' ORDER BY seq DESC LIMIT 1")?;assert!(question.contains("AUTO")&&question.contains("PLAN"),"{question}");
            }
            assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");assert_eq!(world.query("SELECT sha256 FROM specs")?,original);assert_eq!(world.query("SELECT spec_id FROM runs")?,spec);
            assert_eq!(world.query("SELECT count(*) FROM attempts WHERE task_id='work'")?,"0");
            if let Some(cli)=cli {let output=cli.await??;assert_eq!(output.status.code(),Some(3),"{}",String::from_utf8_lossy(&output.stderr));}
            Ok(())
        }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_05_s8_pending_cr_rejects_all_attempt_entries_then_user_cli_approves() -> R {
    let world = world::World::named("m3t04-cr-origin")?;
    world.configure(&[], "")?;
    world.commit_project(&[world::Requirement {
        id: "REQ-A".into(),
        title: "标题".into(),
        criteria: vec![],
    }])?;
    world.init_store().await?;
    world.seed_spec()?;
    let setup = world.start_plain_host(None).await?;
    let run = world::start_run(&setup, "start", "补规格").await?;
    world.staff(&run)?;
    world.exec(&format!(
        "UPDATE runs SET state='RUNNING',mode='PLAN' WHERE run_id='{run}';"
    ))?;
    setup.shutdown().await;
    let source=hoh_core::specindex::Source{path:"docs/spec/fixture.md".into(),bytes:"# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A 标题\n### AC-A-01\ngiven: 前提\nwhen: 操作\nthen: 结果\ncheck: unit\n".as_bytes().to_vec()};
    let host = world
        .team_host(
            world::Team::planning(world.project(), source, |_| vec![], None),
            None,
        )
        .await?;
    let result:R=async {
        world.until("SELECT count(*) FROM change_requests WHERE state='PROPOSED'","1",Duration::from_secs(25)).await?;
        world.until("SELECT state FROM runs","BLOCKED",Duration::from_secs(10)).await?;
        let q:serde_json::Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval' ORDER BY seq DESC LIMIT 1")?)?;
        let before=world.rows("SELECT kind,count(*) AS n FROM commands GROUP BY kind ORDER BY kind")?;
        for kind in ["ApproveSpec","AnswerQuestion","SendInput"] {
            let revision:i64=world.query("SELECT revision FROM runs")?.parse()?;
            let payload=match kind {"ApproveSpec"=>json!({"spec_sha256":q["spec_sha256"],"decision":"APPROVE_ONLY"}),"AnswerQuestion"=>json!({"question_id":q["question_id"],"choice_id":"approve","answer":"approve"}),_=>json!({"text":"approve","target_role":null,"input_kind":"REPLY"})};
            let body=json!({"schema_version":"1.0","command_id":kind,"idempotency_key":kind,"expected_revision":revision,"kind":kind,"run_id":run,"payload":payload});
            let (status,reply)=marked_request(&host,&body).await?;assert_eq!(status,403,"{reply}");assert_eq!(reply["code"],"SPEC_APPROVAL_NOT_USER");
        }
        let mut cli=world.command(&["-p","approve","--resume",&run,"--detach"]);cli.env(hoh_app::host::ATTEMPT_ENV,"m3t04-scripted-attempt");
        let output=tokio::task::spawn_blocking(move||cli.output()).await??;assert!(!output.status.success());assert!(String::from_utf8_lossy(&output.stderr).contains("SPEC_APPROVAL_NOT_USER"),"{}",String::from_utf8_lossy(&output.stderr));
        assert_eq!(world.rows("SELECT kind,count(*) AS n FROM commands GROUP BY kind ORDER BY kind")?,before);
        assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");assert_eq!(world.query("SELECT count(*) FROM input_queue")?,"0");assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,"IMPLEMENTED");
        let mut cli=world.command(&["-p","approve","--resume",&run,"--detach"]);let output=tokio::task::spawn_blocking(move||cli.output()).await??;assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
        assert_eq!(world.query("SELECT count(*) FROM specs")?,"2");assert_eq!(world.query("SELECT state FROM change_requests")?,"APPLIED");assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,"ACCEPTED");Ok(())
    }.await;
    host.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_01_s8_stop_between_dispatch_and_gate_starts_no_draft() -> R {
    use hoh_app::host::RunnerPoint;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst},
    };
    for point in [RunnerPoint::AfterAck, RunnerPoint::AfterSpawn] {
        for (kind, state) in [("PauseRun", "PAUSED"), ("CancelRun", "CANCELLED")] {
            let world = world::World::named("m3t04-stop-gate")?;
            world.configure(&[], "")?;
            let barrier = Arc::new(DraftBarrier {
                point,
                hits: AtomicUsize::new(0),
                release: AtomicBool::new(false),
            });
            let release = ReleaseDraft(barrier.clone());
            let host = world
                .team_host_hooked(world::Team::drafting(world.project()), barrier.clone())
                .await?;
            let result:R=async {
                let run=world::start_run(&host,"start","停止窗口").await?;world.staff(&run)?;
                input(&host,&world,&run,"draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
                let end=std::time::Instant::now()+Duration::from_secs(15);while barrier.hits.load(SeqCst)==0 {if std::time::Instant::now()>end{return Err("draft did not reach barrier".into())}tokio::time::sleep(Duration::from_millis(5)).await;}
                input(&host,&world,&run,"stop",kind,json!({"reason":"fixture stop"})).await?;barrier.release.store(true,SeqCst);
                world.until("SELECT state FROM runs",state,Duration::from_secs(20)).await?;
                assert_eq!(world.query("SELECT count(*) FROM attempts WHERE state IN ('PREPARED','STARTING','RUNNING')")?,"0","{point:?}/{kind}");
                assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.draft_completed'")?,"0");assert!(!world.project().join("docs/spec/main.md").exists());Ok(())
            }.await;
            drop(release);
            host.shutdown().await;
            result?;
        }
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_01_m3l201_http_input_requests_next_draft_only_in_plan() -> R {
    for mode in ["PLAN", "AUTO"] {
        for (kind, target) in [
            ("REPLY", None),
            ("STEER", None),
            ("STEER", Some("cto")),
            ("ASK", None),
        ] {
            let world = world::World::named("m3t04-input-generation")?;
            world.configure(&[], "")?;
            world.commit_project(&world::fixture_spec())?;
            world.init_store().await?;
            world.seed_spec()?;
            let host = world.start_plain_host(None).await?;
            let result: R = async {
                let run = world::start_run(&host, "start", "request next draft").await?;
                world.staff(&run)?;
                // An accepted generation with an approved base and no open question.
                world.exec(&format!("UPDATE runs SET state='RUNNING',mode='{mode}' WHERE run_id='{run}';
                    INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,kind,created_seq)
                    SELECT 'previous','{run}',spec_id,'ACCEPTED','cto',1,'spec_draft',1 FROM runs WHERE run_id='{run}';
                    INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at)
                    VALUES('previous-request',:p,'{run}','spec.draft_requested','{{}}','now');"))?;
                input(&host, &world, &run, "next", "SendInput", json!({"text":"继续修改规格", "target_role":target, "input_kind":kind})).await?;
                let stored = if kind == "REPLY" { "STEER" } else { kind };
                assert_eq!(world.query("SELECT kind FROM input_queue")?, stored);
                assert_eq!(host.store().draft_step(&run, false).await?, mode == "PLAN" && stored == "STEER" && target.is_none(), "{mode} {kind} {target:?}");
                assert!(!host.store().draft_step(&run, false).await?, "one input, one generation");
                if mode == "PLAN" && stored == "STEER" && target.is_none() {
                    assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?, "2");
                    assert_eq!(world.query("SELECT json_extract(payload_json,'$.note.text') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?, "继续修改规格");
                }
                Ok(())
            }.await;
            host.shutdown().await;
            result?;
        }
    }
    Ok(())
}

// A scripted product writer in a second Run; it commits through its own gated, marked process.
#[derive(Debug)]
struct ProductBetweenDrafts {
    team: std::sync::Arc<world::Team>,
    root: std::path::PathBuf,
}
impl hoh_app::host::Executor for ProductBetweenDrafts {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.team.deliver(work)
    }
    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        if work.task_id != "other-work" {
            return self.team.start(work);
        }
        use std::io::Write;
        use std::os::unix::process::CommandExt;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r go || exit 1; set -e; echo 'pub fn other_run() {}' > src/lib.rs; git add src/lib.rs; git -c user.name=test -c user.email=test@example.invalid commit -qm other-run"])
            .current_dir(&self.root).process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .spawn().map_err(|e| hoh_app::host::Refusal {reason:e.to_string(),class:hoh_core::retry::FailureClass::Transient{retry_after:None},detail:None,details:None})?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let (send, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = send.send(hoh_app::host::Outcome {
                success: false,
                detail: format!("scripted product commit: {status:?}"),
                group_gone: hoh_agents::identity::group_empty(pid),
                class: hoh_core::retry::FailureClass::Auth,
                observed: Default::default(),
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate: hoh_app::host::Gate::new(move || {
                stdin
                    .ok_or("stdin")?
                    .write_all(b"go\n")
                    .map_err(|e| e.to_string())
            }),
            ended,
            prompt: None,
            ran: None,
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_04_m3l200_other_run_commits_between_draft_attempts() -> R {
    let world = world::World::named("m3t04-window")?;
    world.configure(&[], "")?;
    world.commit_project(&world::fixture_spec())?;
    world.init_store().await?;
    let spec = world.seed_spec()?;
    let indexed: serde_json::Value = serde_json::from_slice(&std::fs::read(
        world.project().join("docs/spec/index.json"),
    )?)?;
    let ac = indexed["requirements"][0]["acceptance"][0]
        .to_string()
        .replace('\'', "''");
    world.exec(&format!(
        "UPDATE acceptance_criteria SET body='{ac}' WHERE spec_id='{spec}';"
    ))?;
    let setup = world.start_plain_host(None).await?;
    let run = world::start_run(&setup, "draft-run", "draft windows").await?;
    let other = world::start_run(&setup, "other-run", "product writer").await?;
    world.staff(&run)?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',mode='PLAN' WHERE run_id='{run}'; UPDATE runs SET state='RUNNING',mode='BUILD' WHERE run_id='{other}';"))?;
    setup.shutdown().await;
    let source = hoh_core::specindex::Source {
        path: "docs/spec/fixture.md".into(),
        bytes: std::fs::read_to_string(world.project().join("docs/spec/fixture.md"))?
            .replace("fixture 目标已完成", "fixture 目标与边界已完成")
            .into_bytes(),
    };
    let team = world::Team::planning(
        world.project(),
        source,
        |_| vec![],
        Some(Duration::from_secs(1)),
    );
    let mut config = hoh_app::host::HostConfig::new(&world.project());
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(ProductBetweenDrafts {
            team,
            root: world.project(),
        }),
        context: hoh_core::decision::Context {
            required_capabilities: world::TEAM
                .iter()
                .map(|(role, _)| ((*role).to_owned(), Default::default()))
                .collect(),
            ..Default::default()
        },
    });
    let host = hoh_app::host::Host::start(config).await?;
    let result:R=async {
        if let Err(e)=world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(20)).await {return Err(format!("{e}; {:?}",world.rows("SELECT kind,payload_json FROM events ORDER BY seq DESC LIMIT 12")?).into());}
        let first=world.git(&["rev-parse","HEAD"])?;
        world.staff(&other)?;
        world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,cost_estimate) VALUES('other-work','{other}','{spec}','READY','developer',1,1,1);
            INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES('other-work','REQ-901','{spec}');"))?;
        world.until("SELECT count(*) FROM attempts WHERE task_id='other-work' AND state='FAILED'","1",Duration::from_secs(20)).await?;
        let second=world.git(&["rev-parse","HEAD"])?;
        assert_ne!(first,second,"the other Run must really commit");
        assert!(std::fs::read_to_string(world.project().join("src/lib.rs"))?.contains("other_run"));
        let q=world.query(&format!("SELECT json_extract(payload_json,'$.question_id') FROM events WHERE kind='question.asked' AND run_id='{run}' ORDER BY seq DESC LIMIT 1"))?;
        input(&host,&world,&run,"reject","AnswerQuestion",json!({"question_id":q,"choice_id":"reject","answer":"再检查文档"})).await?;
        world.until(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{run}' AND t.kind='spec_draft' AND a.state IN ('SETTLED','FAILED')"),"2",Duration::from_secs(20)).await?;
        assert_eq!(world.query(&format!("SELECT count(*) FROM events WHERE run_id='{run}' AND kind='plan.write_violation'"))?,"0");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.draft_completed'")?,"2");
        assert_eq!(world.git(&["diff","--name-only",second.trim(),"HEAD","--","src/"])?,"");
        Ok(())
    }.await;
    host.shutdown().await;
    result
}

struct DraftStartReadFailure {
    world: std::sync::Arc<world::World>,
    reached: tokio::sync::Notify,
}
impl std::fmt::Debug for DraftStartReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DraftStartReadFailure")
    }
}
impl DraftStartReadFailure {
    fn restore(&self) -> R {
        self.world.exec("UPDATE events SET payload_json=CAST(payload_json AS TEXT) WHERE kind='attempt.workspace_start'")
    }
}
impl hoh_app::host::HostHooks for DraftStartReadFailure {
    fn runner_of(&self, point: hoh_app::host::RunnerPoint, attempt: &str) {
        if point == hoh_app::host::RunnerPoint::AfterEffectCommitted {
            // Valid JSON stored as a BLOB makes draft_start's String read fail. The completion hook restores
            // the fixture before the writer transaction, so this isolates the read boundary from write failures.
            let result = self.world.exec(&format!("UPDATE events SET payload_json=CAST(payload_json AS BLOB) WHERE kind='attempt.workspace_start' AND json_extract(payload_json,'$.attempt_id')='{attempt}'"));
            assert!(result.is_ok(), "inject start read error: {result:?}");
        }
    }
    fn completing(&self, _attempt: &str) {
        let result = self.restore();
        assert!(
            result.is_ok(),
            "restore after failed start read: {result:?}"
        );
    }
    fn writer_completion_returned(&self, _attempt: &str) {
        let result = self.restore();
        assert!(
            result.is_ok(),
            "restore after completion returned: {result:?}"
        );
        self.reached.notify_one();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_04_m3l302_successful_draft_settles_after_start_read_failure() -> R {
    let world = std::sync::Arc::new(world::World::named("m3t04-start-read")?);
    world.configure(&[], "")?;
    let hooks = std::sync::Arc::new(DraftStartReadFailure {
        world: world.clone(),
        reached: tokio::sync::Notify::new(),
    });
    let team = world::Team::drafting(world.project());
    let host = world.team_host_hooked(team, hooks.clone()).await?;
    let result: R = async {
        let run = world::start_run(&host, "start", "completion read failure").await?;
        world.staff(&run)?;
        input(
            &host,
            &world,
            &run,
            "draft",
            "SendInput",
            json!({"text":"draft","input_kind":"REPLY","target_role":null}),
        )
        .await?;
        tokio::time::timeout(Duration::from_secs(15), hooks.reached.notified()).await?;
        Ok(())
    }
    .await;
    // The return hook observes both success and error. Shutdown only after the call returned;
    // stopping earlier aborts in-flight attempts for the next host's reconcile.
    let drained = host.shutdown().await;
    hooks.restore()?;
    result?;
    assert!(drained);
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='plan.write_observation_failed'")?,
        "1",
        "the successful ending must reach its transaction despite a start read error; attempts={:?}; events={:?}",
        world.rows("SELECT attempt_id,state FROM attempts")?,
        world.rows("SELECT kind,payload_json FROM events ORDER BY seq DESC LIMIT 8")?
    );
    assert_eq!(world.query("SELECT a.state||'/'||t.state FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft'")?, "SETTLED/BLOCKED");
    assert_eq!(
        world.query("SELECT state||'/'||resume_state FROM runs")?,
        "BLOCKED/SPECIFYING"
    );
    // Lease rows preserve their epoch history; release expires the row at the holder's end.
    assert_eq!(
        world.query("SELECT count(*) FROM leases WHERE kind='writer'")?,
        "1"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM leases l JOIN attempts a ON a.attempt_id=l.attempt_id WHERE l.kind='writer' AND (a.ended_at IS NULL OR l.expires_at>a.ended_at OR a.state<>'SETTLED')")?,
        "0"
    );
    assert_eq!(world.query("SELECT count(*) FROM candidates")?, "0");
    assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval'")?, "0");
    assert!(world.query("SELECT json_extract(payload_json,'$.detail') FROM events WHERE kind='plan.write_observation_failed'")?.contains("start observation failed"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_01_m3l304_only_steer_after_latest_plan_entry_becomes_draft_note() -> R {
    for old_mode in ["BUILD", "AUTO"] {
        let world = world::World::named("m3t04-plan-input-window")?;
        world.configure(&[], "")?;
        world.commit_project(&world::fixture_spec())?;
        world.init_store().await?;
        world.seed_spec()?;
        let host = world.start_plain_host(None).await?;
        let result: R = async {
            let run = world::start_run(&host, "start", "plan input window").await?;
            world.staff(&run)?;
            world.exec(&format!("UPDATE runs SET state='RUNNING',mode='{old_mode}' WHERE run_id='{run}';
                INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,kind,created_seq) SELECT 'previous','{run}',spec_id,'ACCEPTED','cto',1,'spec_draft',1 FROM runs WHERE run_id='{run}';
                INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at) VALUES('previous-request',:p,'{run}','spec.draft_requested','{{}}','now');"))?;
            input(&host, &world, &run, "old-input", "SendInput", json!({"text":"old product instruction","target_role":null,"input_kind":"STEER"})).await?;
            input(&host, &world, &run, "plan", "SetMode", json!({"mode":"PLAN"})).await?;
            assert!(host.store().draft_step(&run, false).await?);
            assert!(world.query("SELECT json_extract(payload_json,'$.cause') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?.starts_with("plan/"), "{old_mode}: a preceding product steer is not a planning request");
            world.exec("UPDATE tasks SET state='ACCEPTED' WHERE kind='spec_draft'")?;
            // A steer from the preceding PLAN interval also cannot cross a later PLAN entry.
            input(&host, &world, &run, "previous-plan-input", "SendInput", json!({"text":"previous planning interval","target_role":null,"input_kind":"STEER"})).await?;
            input(&host, &world, &run, "build", "SetMode", json!({"mode":"BUILD"})).await?;
            input(&host, &world, &run, "plan-again", "SetMode", json!({"mode":"PLAN"})).await?;
            assert!(host.store().draft_step(&run, false).await?);
            assert!(world.query("SELECT json_extract(payload_json,'$.cause') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?.starts_with("plan/"));
            world.exec("UPDATE tasks SET state='ACCEPTED' WHERE kind='spec_draft'")?;
            input(&host, &world, &run, "new-input", "SendInput", json!({"text":"current planning request","target_role":null,"input_kind":"STEER"})).await?;
            assert!(host.store().draft_step(&run, false).await?);
            assert!(world.query("SELECT json_extract(payload_json,'$.cause') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?.starts_with("input/"));
            assert_eq!(world.query("SELECT json_extract(payload_json,'$.note.text') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?, "current planning request");
            assert!(!host.store().draft_step(&run, false).await?);
            Ok(())
        }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_m3l450_refuse_draft_gate_ends_retryable_and_releases_lease() -> R {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    let world = world::World::named("m3t04-refused-gate")?;
    world.configure(&[], "")?;
    world.git(&["init", "-q"])?;
    let barrier = std::sync::Arc::new(DraftBarrier {
        point: hoh_app::host::RunnerPoint::AfterSpawn,
        hits: AtomicUsize::new(0),
        release: AtomicBool::new(false),
    });
    let release = ReleaseDraft(barrier.clone());
    let host = world
        .team_host_hooked(world::Team::drafting(world.project()), barrier.clone())
        .await?;
    let result: R = async {
        let run = world::start_run(&host, "start", "refuse gate after effect changed").await?;
        world.staff(&run)?;
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        tokio::time::timeout(Duration::from_secs(15), async {
            while barrier.hits.load(SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await?;
        let attempt = world.query("SELECT attempt_id FROM attempts WHERE state='STARTING'")?;
        let effect = hoh_core::reconcile::work_key(&attempt);
        world.exec(&format!("UPDATE effects SET state='OBSERVED' WHERE effect_key='{effect}'"))?;
        assert_eq!(world.query(&format!("SELECT state FROM effects WHERE effect_key='{effect}'"))?, "OBSERVED");
        // Hold the Run so a retry cannot hide the refused Attempt's resulting Task state.
        input(&host, &world, &run, "pause", "PauseRun", json!({"reason":"inspect refused attempt"})).await?;
        barrier.release.store(true, SeqCst);
        world.until(&format!("SELECT count(*) FROM events WHERE kind='reconcile.attempt' AND json_extract(payload_json,'$.attempt_id')='{attempt}'"), "1", Duration::from_secs(20)).await?;
        assert_eq!(world.query(&format!("SELECT a.state||'/'||t.state FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE a.attempt_id='{attempt}'"))?, "CANCELLED/READY");
        assert_eq!(world.query(&format!("SELECT count(*) FROM leases l JOIN attempts a ON a.attempt_id=l.attempt_id WHERE a.attempt_id='{attempt}' AND l.expires_at<=a.ended_at"))?, "1");
        assert_eq!(world.query(&format!("SELECT count(*) FROM attempts WHERE attempt_id='{attempt}' AND pid IS NOT NULL AND boot_id IS NOT NULL AND process_start_id IS NOT NULL"))?, "1");
        assert_eq!(world.query(&format!("SELECT state FROM effects WHERE effect_key='{effect}'"))?, "OBSERVED");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.draft_start_refused'" )?, "1");
        assert_eq!(world.query("SELECT count(*) FROM candidates" )?, "0");
        assert!(!world.project().join("docs/spec/main.md").exists());
        Ok(())
    }.await;
    drop(release);
    host.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_01_m3l452_plan_to_plan_keeps_current_steer_window() -> R {
    let world = world::World::named("m3t04-plan-noop")?;
    world.configure(&[], "")?;
    world.commit_project(&world::fixture_spec())?;
    world.init_store().await?;
    world.seed_spec()?;
    let host = world.start_plain_host(None).await?;
    let result: R = async {
        let run = world::start_run(&host, "start", "plan noop").await?;
        world.staff(&run)?;
        world.exec(&format!("UPDATE runs SET state='RUNNING',mode='BUILD' WHERE run_id='{run}';"))?;
        input(&host, &world, &run, "plan", "SetMode", json!({"mode":"PLAN"})).await?;
        assert!(host.store().draft_step(&run, false).await?);
        world.exec("UPDATE tasks SET state='ACCEPTED' WHERE kind='spec_draft'")?;
        input(&host, &world, &run, "steer", "SendInput", json!({"text":"keep this planning request","target_role":null,"input_kind":"STEER"})).await?;
        input(&host, &world, &run, "plan-noop", "SetMode", json!({"mode":"PLAN"})).await?;
        assert!(host.store().draft_step(&run, false).await?);
        assert_eq!(world.query("SELECT json_extract(payload_json,'$.note.text') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?, "keep this planning request");
        assert!(world.query("SELECT json_extract(payload_json,'$.cause') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?.starts_with("input/"));
        world.exec("UPDATE tasks SET state='ACCEPTED' WHERE kind='spec_draft'")?;
        input(&host, &world, &run, "another-noop", "SetMode", json!({"mode":"PLAN"})).await?;
        assert!(!host.store().draft_step(&run, false).await?, "PLAN to PLAN alone is not a new generation");
        Ok(())
    }.await;
    host.shutdown().await;
    result
}

/// The project-relative paths commit `commit` changes (a root commit: every path it adds).
fn changed(world: &world::World, commit: &str) -> R<Vec<String>> {
    Ok(world
        .git(&[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "--root",
            commit,
        ])?
        .lines()
        .map(str::to_owned)
        .collect())
}

/// AC-028-04 for one project shape: an empty directory (`history` false) or a Git repository with commits, neither with
/// an approved specification; the user answers runtime:no_spec with draft and the scripted cto (no live agent) writes
/// only docs/spec/ and commits.
async fn draft_from_no_spec(history: bool) -> R {
    use std::os::unix::fs::MetadataExt as _;
    let world = world::World::named(if history {
        "m3t07-history"
    } else {
        "m3t07-empty"
    })?;
    world.configure(&[], "")?;
    let before = if history {
        world.git(&["init", "-q"])?;
        for (path, body, message) in [
            ("README.md", "# 已有项目\n", "first"),
            ("src/lib.rs", "pub fn kept() {}\n", "second"),
        ] {
            let file = world.project().join(path);
            std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
            std::fs::write(file, body)?;
            world.git(&["add", path])?;
            world.git(&["commit", "-q", "-m", message])?;
        }
        let git = std::fs::metadata(world.project().join(".git"))?;
        Some((world.git(&["rev-list", "HEAD"])?, git.ino()))
    } else {
        None
    };
    let host = world
        .team_host(world::Team::drafting(world.project()), None)
        .await?;
    let result: R = async {
        let run = world::start_run(&host, "start", "规格起草").await?;
        world.staff(&run)?;
        world
            .until(
                &format!("SELECT count(*) FROM events WHERE run_id='{run}' AND kind='question.asked' AND json_extract(payload_json,'$.reason')='no_spec'"),
                "1",
                Duration::from_secs(15),
            )
            .await?;
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        world
            .until(&format!("SELECT count(*) FROM events WHERE run_id='{run}' AND kind='spec.draft_completed'"), "1", Duration::from_secs(20))
            .await?;
        assert_eq!(world.query(&format!("SELECT state FROM tasks WHERE run_id='{run}' AND kind='spec_draft'"))?, "IMPLEMENTED");
        let attempt = world.query(&format!("SELECT a.attempt_id FROM attempts a JOIN tasks t USING(task_id) WHERE t.run_id='{run}' AND t.kind='spec_draft'"))?;
        let attempts = format!("(SELECT a.attempt_id FROM attempts a JOIN tasks t USING(task_id) WHERE t.run_id='{run}')");

        // The baseline: one, with the project's HEAD then (none in the empty directory), before every Attempt and effect
        // record of the Run.
        assert_eq!(world.query(&format!("SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline'"))?, "1");
        let baseline: i64 = world.query(&format!("SELECT seq FROM events WHERE run_id='{run}' AND kind='run.baseline'"))?.parse()?;
        let head = world.query(&format!("SELECT coalesce(json_extract(payload_json,'$.head'),'NULL') FROM events WHERE seq={baseline}"))?;
        match &before {
            None => assert_eq!(head, "NULL"),
            Some((list, _)) => assert_eq!(head, list.lines().next().ok_or("no history")?),
        }
        let records = format!("events WHERE (json_extract(payload_json,'$.attempt_id') IN {attempts} OR json_extract(payload_json,'$.effect_key') IN (SELECT effect_key FROM effects WHERE attempt_id IN {attempts}) OR (run_id='{run}' AND (kind GLOB 'attempt.*' OR kind GLOB 'effect.*')))");
        assert!(world.query(&format!("SELECT count(*) FROM {records}"))?.parse::<i64>()? > 0);
        assert_eq!(world.query(&format!("SELECT count(*) FROM {records} AND seq<={baseline}"))?, "0", "no Attempt or effect record before the baseline");
        assert_eq!(world.query(&format!("SELECT count(*) FROM attempts WHERE attempt_id IN {attempts} AND NOT EXISTS (SELECT 1 FROM events WHERE json_extract(payload_json,'$.attempt_id')=attempts.attempt_id AND seq>{baseline})"))?, "0", "every Attempt is recorded after the baseline");
        assert_eq!(world.query(&format!("SELECT count(*) FROM effects WHERE attempt_id IN {attempts} AND NOT EXISTS (SELECT 1 FROM events WHERE kind GLOB 'effect.*' AND json_extract(payload_json,'$.effect_key')=effects.effect_key AND seq>{baseline})"))?, "0", "every effect is recorded after the baseline");

        // git init: the drafting Attempt's own effect in its preparation (dispatched, before its workspace start), local only.
        let init = hoh_core::ids::child_id(&attempt, "git-init");
        let inits = world.query(&format!("SELECT count(*) FROM effects WHERE attempt_id IN {attempts} AND effect_key GLOB '*/git-init'"))?;
        match &before {
            None => {
                assert_eq!(inits, "1");
                assert_eq!(world.query(&format!("SELECT attempt_id||'/'||state FROM effects WHERE effect_key='{init}'"))?, format!("{attempt}/COMMITTED"));
                let seq = |kind: &str, key: &str, value: &str| world.query(&format!("SELECT coalesce(min(seq),-1) FROM events WHERE kind='{kind}' AND json_extract(payload_json,'$.{key}')='{value}'"));
                let (dispatched, started, committed, workspace) = (
                    seq("spec.draft_dispatched", "attempt_id", &attempt)?.parse::<i64>()?,
                    seq("effect.started", "effect_key", &init)?.parse::<i64>()?,
                    seq("effect.committed", "effect_key", &init)?.parse::<i64>()?,
                    seq("attempt.workspace_start", "attempt_id", &attempt)?.parse::<i64>()?,
                );
                assert!(baseline < dispatched && dispatched < started && started < committed && committed < workspace, "{baseline} {dispatched} {started} {committed} {workspace}");
            }
            Some(_) => assert_eq!(inits, "0", "an existing repository is not initialised again"),
        }
        assert_eq!(world.git(&["remote"])?, "", "no remote");
        assert!(!std::fs::read_to_string(world.project().join(".git/config"))?.contains("[remote"));

        // Commits: the draft's commit is HEAD; the history is kept; every new commit touches only docs/spec/, docs/, .hoh/.
        let drafted = world.query(&format!("SELECT json_extract(payload_json,'$.git_commit') FROM events WHERE run_id='{run}' AND kind='spec.draft_completed'"))?;
        let list = world.git(&["rev-list", "HEAD"])?;
        assert_eq!(list.lines().next(), Some(drafted.as_str()));
        let new: Vec<&str> = match &before {
            None => list.lines().collect(),
            Some((old, ino)) => {
                assert!(list.ends_with(old.as_str()), "history kept: {list} / {old}");
                assert_eq!(std::fs::metadata(world.project().join(".git"))?.ino(), *ino, "the same .git directory");
                list[..list.len() - old.len()].lines().collect()
            }
        };
        assert!(!new.is_empty());
        for commit in new {
            let paths = changed(&world, commit)?;
            assert!(!paths.is_empty(), "{commit}");
            for path in paths {
                assert!(["docs/spec/", "docs/", ".hoh/"].iter().any(|p| path.starts_with(p)), "{commit} touches {path}");
            }
        }
        Ok(())
    }
    .await;
    host.shutdown().await;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_028_04_draft_from_empty_directory_and_existing_history_after_baseline() -> R {
    draft_from_no_spec(false).await?;
    draft_from_no_spec(true).await
}
