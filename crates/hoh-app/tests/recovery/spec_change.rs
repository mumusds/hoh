//! AC-033-02: product approval/cancel/reconcile with fake verifiers; never live model evidence.
use super::world::{self, World};
use super::{R, Value, json};
use hoh_app::host::{Host, HostConfig};
use hoh_app::verify::freeze::{Inputs, freeze};
use hoh_core::persistence::{CancelPass, Frozen};
use hoh_core::specindex::{self, Source};
use std::os::unix::process::ExitStatusExt as _;
use std::time::{Duration, SystemTime};

fn world() -> R<World> {
    let root = std::env::var_os("HOH_S6_SCRATCH_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    World::in_directory(
        tempfile::Builder::new().prefix("m3t02-").tempdir_in(root)?,
        "p",
    )
}

async fn command(host: &Host, run: &str, key: &str, kind: &str, payload: Value) -> R<Value> {
    let (_, row) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,
        "run_id":run,"expected_revision":row["revision"],"kind":kind,"payload":payload});
    let (status, reply) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{kind}: {reply}");
    Ok(body)
}

fn change(world: &World) -> R<String> {
    let path = "docs/spec/fixture.md";
    let source = world.project().join(path);
    let text = std::fs::read_to_string(&source)?
        .replace("fixture 目标已完成", "fixture 目标已完成（修订 2）");
    std::fs::write(&source, &text)?;
    let index = specindex::generate_index(&[Source {
        path: path.into(),
        bytes: text.into_bytes(),
    }])?;
    std::fs::write(world.project().join(specindex::PATH), index.bytes())?;
    world.git(&["add", "docs/spec"])?;
    world.git(&["commit", "-qm", "changed specification"])?;
    Ok(index.sha256().to_owned())
}

async fn propose(host: &Host, run: &str, hash: &str) -> R {
    command(
        host,
        run,
        "propose-s6",
        "ProposeSpecChange",
        json!({"spec_sha256":hash,"reason":"S6 specification change"}),
    )
    .await?;
    assert!(!hash.is_empty());
    Ok(())
}

async fn approve(host: &Host, run: &str, hash: &str) -> R<Value> {
    command(
        host,
        run,
        "approve-s6",
        "ApproveSpec",
        json!({"spec_sha256":hash,"decision":"APPROVE_ONLY"}),
    )
    .await
}

struct Fixture {
    host: Host,
    world: World,
    run: String,
    candidate: String,
}

impl Fixture {
    async fn new() -> R<Self> {
        let world = world()?;
        world.commit_project(&world::fixture_spec())?;
        world.init_store().await?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "s6-start", "S6 verification").await?;
        let index = specindex::generate_index(&[Source {
            path: "docs/spec/fixture.md".into(),
            bytes: std::fs::read(world.project().join("docs/spec/fixture.md"))?,
        }])?;
        command(
            &host,
            &run,
            "initial-s6",
            "ApproveSpec",
            json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
        )
        .await?;
        world.exec(&format!("UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}';"))?;
        std::fs::write(world.project().join(".hoh/environment.json"), b"{}\n")?;
        let Frozen::New {
            candidate_id: candidate,
            ..
        } = freeze(
            host.store(),
            &world.project(),
            &world.project().join(".hoh"),
            &run,
            &Inputs {
                spec: specindex::PATH.into(),
                environment: ".hoh/environment.json".into(),
                test_plan: "tests/check-plan.json".into(),
            },
        )
        .await?
        else {
            return Err("new candidate".into());
        };
        for (id, role, state, writer) in [
            ("D", "developer", "VERIFYING", 1),
            ("Q", "qa", "RUNNING", 0),
            ("V", "reviewer", "RUNNING", 0),
            ("other", "qa", "RUNNING", 0),
            ("writer", "developer", "RUNNING", 1),
        ] {
            world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) SELECT '{id}',run_id,spec_id,'{state}','{role}',{writer},1 FROM runs WHERE run_id='{run}';
                INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES('s-{id}','claude','{role}','fake','[]');
                INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES('a-{id}','{id}','s-{id}','{}',1,1,0);",if id=="D" {"SETTLED"} else {"RUNNING"}))?;
        }
        let opened = json!({"round_id":"round-s6","candidate_id":candidate,"number":1,"source_task_ids":["D"],"acceptance_ids":["AC-901-01"],"tasks":{"qa":"Q","reviewer":"V"},"handoffs":[]});
        world.exec(&format!("UPDATE candidates SET state='VERIFYING' WHERE candidate_id='{candidate}';
            INSERT INTO task_requirements(task_id,requirement_id,spec_id) SELECT 'D',requirement_id,spec_id FROM requirements;
            INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at) SELECT 'round-opened',project_id,run_id,'verification.round_opened','{opened}',created_at FROM runs WHERE run_id='{run}';
            INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES('report-object','{}','fixture-report',0,'application/json');", "a".repeat(64)))?;
        Ok(Self {
            host,
            world,
            run,
            candidate,
        })
    }

    fn report(&self) -> R {
        self.world.exec(&format!("INSERT INTO independent_reports(report_id,run_id,candidate_id,role,session_id,spec_sha256,status,state,attempt_id,report_artifact_id,raw_session_artifact_id,created_at) SELECT 'report-s6','{run}','{candidate}','qa','s-Q',spec_sha256,'PASS','CURRENT','a-Q','report-object','report-object','2026-11-21T00:00:00Z' FROM candidates WHERE candidate_id='{candidate}';",run=self.run,candidate=self.candidate))
    }

    fn closes(&self) -> R<Vec<Value>> {
        self.world
            .rows("SELECT seq,payload_json FROM events WHERE kind='verification.round_closed'")
    }

    async fn end(&self, id: &str) -> R {
        // State fixture: the process controller's end proof, through the real CancelPass transaction.
        self.host
            .store()
            .cancel_pass(CancelPass {
                attempt_id: id.into(),
                sent: vec![],
                term_incomplete: false,
                end: hoh_core::cancel::PassEnd::Empty,
                at: SystemTime::now(),
                workspace: None,
            })
            .await?;
        Ok(())
    }
}

#[tokio::test]
async fn test_033_02_approval_atomically_stales_reports_and_cancels_only_its_verifiers() -> R {
    let f = Fixture::new().await?;
    f.report()?;
    let hash = change(&f.world)?;
    propose(&f.host, &f.run, &hash).await?;
    let before = f.world.rows("SELECT * FROM attempts ORDER BY attempt_id")?;
    f.world.exec("CREATE TRIGGER reject_s6 BEFORE INSERT ON events WHEN NEW.kind='candidate.stale' BEGIN SELECT RAISE(ABORT,'S6 approval rollback'); END;")?;
    let (_, row) = world::exchange(&f.host, "GET", &format!("/v1/runs/{}", f.run), None).await?;
    let body = json!({"schema_version":"1.0","command_id":"rollback-s6","idempotency_key":"rollback-s6","run_id":f.run,"expected_revision":row["revision"],"kind":"ApproveSpec","payload":{"spec_sha256":hash,"decision":"APPROVE_ONLY"}});
    assert_ne!(
        world::exchange(&f.host, "POST", "/v1/commands", Some(&body))
            .await?
            .0,
        202
    );
    assert_eq!(
        f.world.rows("SELECT * FROM attempts ORDER BY attempt_id")?,
        before
    );
    assert_eq!(
        f.world.query("SELECT state FROM independent_reports")?,
        "CURRENT"
    );
    f.world.exec("DROP TRIGGER reject_s6;")?;
    let approval = approve(&f.host, &f.run, &hash).await?;
    assert_eq!(f.world.query("SELECT state FROM candidates")?, "STALE");
    assert_eq!(f.world.query("SELECT group_concat(attempt_id,',') FROM (SELECT attempt_id FROM attempts WHERE cancel_requested=1 ORDER BY attempt_id)")?,"a-Q,a-V","approval must persist cancellation only for this candidate's verification attempts");
    assert_eq!(
        f.world.query("SELECT state FROM independent_reports")?,
        "STALE",
        "report must stale in approval, before verification reconcile"
    );
    assert_eq!(f.world.query("SELECT state FROM runs")?, "RUNNING");
    assert!(f.closes()?.is_empty());
    assert_eq!(
        world::exchange(&f.host, "POST", "/v1/commands", Some(&approval))
            .await?
            .0,
        202
    );
    assert_eq!(
        f.world
            .query("SELECT count(*) FROM events WHERE kind='candidate.stale'")?,
        "1"
    );
    assert_eq!(
        f.world
            .query("SELECT count(*) FROM events WHERE kind='report.stale'")?,
        "1"
    );
    f.host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_02_approval_then_each_end_closes_once_only_after_both_proofs() -> R {
    for order in [["a-Q", "a-V"], ["a-V", "a-Q"]] {
        let f = Fixture::new().await?;
        let hash = change(&f.world)?;
        propose(&f.host, &f.run, &hash).await?;
        approve(&f.host, &f.run, &hash).await?;
        for ended in [None, Some(order[0]), Some(order[1])] {
            if let Some(id) = ended {
                f.end(id).await?;
            }
            f.host
                .store()
                .reconcile_verification(&f.run, Default::default())
                .await?;
            assert_eq!(
                f.closes()?.len(),
                usize::from(ended == Some(order[1])),
                "both verification process proofs must precede the one round closure"
            );
        }
        let closed = f.closes()?;
        let payload: Value =
            serde_json::from_str(closed[0]["payload_json"].as_str().ok_or("payload")?)?;
        assert_eq!(payload["verdict"], "superseded");
        f.host
            .store()
            .reconcile_verification(&f.run, Default::default())
            .await?;
        assert_eq!(f.closes()?, closed);
        assert_eq!(
            f.world
                .query("SELECT count(*) FROM candidates WHERE state='ACCEPTED'")?,
            "0"
        );
        assert_eq!(
            f.world.query("SELECT state FROM tasks WHERE task_id='D'")?,
            "READY"
        );
        assert_eq!(f.world.query("SELECT state FROM runs")?, "RUNNING");
        f.host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_033_02_prepared_starting_and_prior_judgment_are_cancelled_without_dispatch() -> R {
    let f = Fixture::new().await?;
    let judgment = hoh_core::ids::child_id(&f.candidate, "stale");
    f.world.exec(&format!("UPDATE candidates SET state='STALE'; UPDATE attempts SET state='PREPARED' WHERE attempt_id='a-Q';
        UPDATE attempts SET state='STARTING' WHERE attempt_id='a-V';
        UPDATE tasks SET state='LEASED' WHERE task_id IN ('Q','V');
        INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at) SELECT '{judgment}',project_id,run_id,'candidate.stale','{{\"candidate_id\":\"{}\",\"reason\":\"evidence_unrecoverable\"}}',created_at FROM runs WHERE run_id='{}';",f.candidate,f.run))?;
    let hash = change(&f.world)?;
    propose(&f.host, &f.run, &hash).await?;
    approve(&f.host, &f.run, &hash).await?;
    assert_eq!(
        f.world.query(
            "SELECT count(*) FROM attempts WHERE attempt_id IN ('a-Q','a-V') AND cancel_requested=1"
        )?,
        "2",
        "a previous stale judgment must not suppress cancellation"
    );
    f.world
        .until(
            "SELECT count(*) FROM attempts WHERE attempt_id IN ('a-Q','a-V') AND state='CANCELLED'",
            "2",
            Duration::from_secs(5),
        )
        .await?;
    f.host
        .store()
        .reconcile_verification(&f.run, Default::default())
        .await?;
    assert_eq!(f.closes()?.len(), 1);
    assert_eq!(
        f.world.query(
            "SELECT json_extract(payload_json,'$.reason') FROM events WHERE kind='candidate.stale'"
        )?,
        "evidence_unrecoverable"
    );
    assert_eq!(
        f.world
            .query("SELECT count(*) FROM events WHERE kind='runner.received'")?,
        "0"
    );
    f.host.shutdown().await;
    Ok(())
}

fn intake(f: &Fixture, role: &str) -> R<hoh_core::persistence::ReportIntake> {
    use hoh_core::persistence::{ArtifactRow, ReportIntake};
    let suffix = if role == "qa" { "Q" } else { "V" };
    let object = |n: u8, kind: &str| ArtifactRow {
        sha256: format!("{n:064x}"),
        path: format!(".hoh/objects/{role}-{n}"),
        bytes: 10,
        mime: "application/json".into(),
        kind: kind.into(),
        name: kind.into(),
    };
    let raw = object(1, "raw_log");
    let native = object(2, "native_attempt");
    let reference = |a: &ArtifactRow| hoh_core::verification::report::ObjectRef {
        path: a.path.clone(),
        sha256: a.sha256.clone(),
    };
    let spec = f.world.query("SELECT spec_sha256 FROM candidates")?;
    let report = hoh_core::verification::report::build(
        &json!({"status":"PASS","scope":["AC-901-01"],"findings":[]}),
        role,
        &format!("s-{suffix}"),
        (&f.candidate, &spec),
        &reference(&raw),
        &reference(&native),
    )?;
    Ok(ReportIntake {
        attempt_id: format!("a-{suffix}"),
        report,
        report_artifact: object(3, "independent_report"),
        raw_artifact: raw,
        native_artifact: native,
        native_session_ids: vec![format!("s-{suffix}")],
        native_status: "COMPLETED".into(),
    })
}

#[tokio::test]
async fn test_033_02_report_before_or_after_approval_never_counts_in_new_revision() -> R {
    for phase in [
        "before_approval",
        "while_cancelling",
        "after_cancel",
        "after_close",
    ] {
        let f = Fixture::new().await?;
        let report = intake(&f, "qa")?;
        let hash = change(&f.world)?;
        propose(&f.host, &f.run, &hash).await?;
        if phase == "before_approval" {
            let got = f.host.store().record_report(report.clone()).await?;
            assert_eq!(
                got.state,
                hoh_core::persistence::ReportState::Current,
                "{got:?}"
            );
        }
        approve(&f.host, &f.run, &hash).await?;
        if phase != "before_approval" {
            if matches!(phase, "after_cancel" | "after_close") {
                f.end("a-Q").await?;
            }
            if phase == "after_close" {
                f.end("a-V").await?;
                f.host
                    .store()
                    .reconcile_verification(&f.run, Default::default())
                    .await?;
                assert_eq!(f.closes()?.len(), 1);
            }
            let closed = f.closes()?;
            let got = f.host.store().record_report(report).await?;
            assert_eq!(
                got.state,
                hoh_core::persistence::ReportState::Stale,
                "a valid late PASS must stale ({phase}): {got:?}"
            );
            assert_eq!(f.closes()?, closed, "report intake never closes the round");
        }
        assert_eq!(
            f.world.query("SELECT state FROM independent_reports")?,
            "STALE"
        );
        let (_, run) =
            world::exchange(&f.host, "GET", &format!("/v1/runs/{}", f.run), None).await?;
        assert_eq!(run["projection"]["acceptance"]["accepted"], 0);
        assert_eq!(run["projection"]["acceptance"]["spec_revision"], 2);
        assert_eq!(
            f.world
                .query("SELECT count(*) FROM acceptance_results WHERE status='PASS'")?,
            "0"
        );
        f.host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_033_02_closed_round_before_approval_keeps_its_history() -> R {
    let f = Fixture::new().await?;
    for role in ["qa", "reviewer"] {
        let got = f.host.store().record_report(intake(&f, role)?).await?;
        assert_eq!(
            got.state,
            hoh_core::persistence::ReportState::Current,
            "{got:?}"
        );
    }
    // Settled state fixture. No check evidence: this round must close inconclusive, never as a pass.
    f.world.exec("UPDATE attempts SET state='SETTLED' WHERE attempt_id IN ('a-Q','a-V'); UPDATE tasks SET state='IMPLEMENTED' WHERE task_id IN ('Q','V'); UPDATE tasks SET state='ACCEPTED' WHERE task_id='D';")?;
    f.host
        .store()
        .reconcile_verification(&f.run, Default::default())
        .await?;
    let closed = f.closes()?;
    assert_eq!(closed.len(), 1);
    let hash = change(&f.world)?;
    propose(&f.host, &f.run, &hash).await?;
    approve(&f.host, &f.run, &hash).await?;
    f.host
        .store()
        .reconcile_verification(&f.run, Default::default())
        .await?;
    assert_eq!(
        f.closes()?,
        closed,
        "approval cannot rewrite a previously closed round"
    );
    assert_eq!(
        f.world
            .query("SELECT count(*) FROM independent_reports WHERE state='STALE'")?,
        "2"
    );
    assert_eq!(
        f.world
            .query("SELECT count(*) FROM attempts WHERE cancel_requested=1")?,
        "0"
    );
    f.host.shutdown().await;
    Ok(())
}

#[derive(Debug)]
struct HoldApproval(std::path::PathBuf);
impl hoh_core::persistence::FaultHooks for HoldApproval {
    fn at(&self, job: &'static str, point: hoh_core::persistence::WritePoint) {
        if job == "Command"
            && point == hoh_core::persistence::WritePoint::Committed
            && self.0.join("hold-approval").exists()
        {
            assert!(
                std::fs::write(self.0.join("approval-committed"), b"committed").is_ok(),
                "approval marker"
            );
            while !self.0.join("release-approval").exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
#[ignore = "child product host, launched only by the S6 process matrix"]
fn s6_host() -> R {
    let root = std::path::PathBuf::from(std::env::var_os("HOH_S6_WORLD").ok_or("S6 world")?);
    let project = root.join("p");
    let registry = std::sync::Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
        std::env::var_os("PATH").ok_or("fake PATH")?,
        Some(root.join("pi-agent")),
    ));
    let mut config = HostConfig::product_with(
        &project,
        hoh_core::config::load(&project)?,
        world::HOH.into(),
        registry,
    );
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.store.faults = std::sync::Arc::new(HoldApproval(root.clone()));
    config
        .runner
        .as_mut()
        .ok_or("runner")?
        .context
        .commander_dir = Some(root.join("state/hoh/commander"));
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    assert_eq!(runtime.block_on(hoh_app::host::run(config)), 0);
    Ok(())
}

fn child_host(world: &World) -> R<super::ChildGuard> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(world.root().join("host.log"))?;
    let mut child = std::process::Command::new(std::env::current_exe()?);
    child
        .args([
            "--exact",
            "recovery::spec_change::s6_host",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(world.env())
        .env("HOH_S6_WORLD", world.root())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = super::ChildGuard(child.spawn()?);
    let until = std::time::Instant::now() + Duration::from_secs(30);
    while super::announced(&world.project()) != Some(u64::from(child.0.id())) {
        if let Some(status) = child.0.try_wait()? {
            return Err(format!(
                "S6 host {status}: {}",
                std::fs::read_to_string(world.root().join("host.log"))?
            )
            .into());
        }
        if std::time::Instant::now() > until {
            return Err("S6 host start timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(child)
}

fn info(world: &World) -> R<Value> {
    Ok(serde_json::from_slice(&std::fs::read(
        world.project().join(".hoh/host.json"),
    )?)?)
}

fn remote_command(world: &World, run: &str, key: &str, kind: &str, payload: Value) -> R {
    let info = info(world)?;
    let revision: Value = serde_json::from_str(
        &world.query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?,
    )?;
    let (status, reply) = super::common::fresh_blocking(revision, |revision| {
        let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,"run_id":run,"expected_revision":revision,"kind":kind,"payload":payload});
        let (status, text) = super::http(&info, "POST", "/v1/commands", Some(&body.to_string()))?;
        Ok((status, serde_json::from_str(&text)?))
    })?;
    assert_eq!(status, 202, "{kind}: {reply}");
    Ok(())
}

#[test]
fn test_033_02_spec_change_cancels_live_verification() -> R {
    super::common::in_delegated_scope("recovery::spec_change::s6_live_inside")
}

#[test]
fn test_033_02_kill_9_during_cancel_restarts_the_same_attempts() -> R {
    super::common::in_delegated_scope("recovery::spec_change::s6_restart_inside")
}

#[test]
#[ignore = "called by test_033_02_spec_change_cancels_live_verification in a delegated cgroup"]
fn s6_live_inside() -> R {
    live(false)
}

#[test]
#[ignore = "called by test_033_02_kill_9_during_cancel_restarts_the_same_attempts in a delegated cgroup"]
fn s6_restart_inside() -> R {
    live(true)
}

fn live(restart: bool) -> R {
    assert!(
        std::env::var_os(super::common::IN_SCOPE).is_some(),
        "delegated cgroup required"
    );
    let world = world()?;
    let _processes = super::common::Recorded(world.root().join("owned-processes"));
    world.configure(
        &world::TEAM,
        "[execution]\ncancel_native_grace_seconds=1\ncancel_term_grace_seconds=1\n",
    )?;
    world.commit_project(&world::fixture_spec())?;
    // Only this world's fake: every turn records its exact process identity for cleanup. Verifiers keep running
    // after their report on SIGTERM so that the product must escalate and prove their entire cgroup empty.
    let agent = world.fakes().join("agent.py");
    let text=std::fs::read_to_string(&agent)?.replace("    cfg = settings()\n    text = texts(prompt)",
        "    with open(os.path.join(WORLD, 'owned-processes'), 'a') as f:\n        f.write('proc %d %s\\n' % (os.getpid(), open('/proc/self/stat').read().rsplit(')', 1)[1].split()[19]))\n    cfg = settings()\n    text = texts(prompt)")
        .replace("    v = cfg.get('verifiers', {}).get(role, {})", "    import signal\n    def late(signum, frame):\n        block = {'role': role, 'status': 'PASS', 'scope': ['AC-901-01'], 'findings': []}\n        task_result(ident, 'COMPLETED', 'late fake report', 'report')\n        append('late-pass.jsonl', {'role': role, 'attempt_id': ident['attempt_id'], 'report': block})\n        result = next(f for f in frames('claude', '2.1.318', 'success', 'stdout.jsonl') if f.get('type') == 'result')\n        result['result'] = '```hoh-report\\n' + json.dumps(block) + '\\n```'\n        result['session_id'] = S6_SESSION\n        emit(result)\n    signal.signal(signal.SIGTERM, late)\n    signal.signal(signal.SIGINT, signal.SIG_IGN)\n    v = cfg.get('verifiers', {}).get(role, {})")
        .replace("    scope = v.get('scope')", "    append('verifier-held.jsonl', {'role': role, 'attempt_id': ident['attempt_id']})\n    while not os.path.exists(os.path.join(WORLD, 'release-verifiers')):\n        time.sleep(0.01)\n    scope = v.get('scope')");
    let text = text.replace(
        "    def out(frame):",
        "    globals()['S6_SESSION'] = session\n\n    def out(frame):",
    );
    std::fs::write(&agent, text)?;
    let plan_path = world.project().join("tests/check-plan.json");
    let mut plan: Value = serde_json::from_slice(&std::fs::read(&plan_path)?)?;
    let script = format!(
        "import os,time,signal,json\nsignal.signal(signal.SIGTERM,signal.SIG_IGN)\npid=os.getpid(); start=open('/proc/self/stat').read().rsplit(')',1)[1].split()[19]\nwith open({owned:?},'a') as f: f.write('proc %d %s\\n'%(pid,start))\nwith open({mark:?},'w') as f: json.dump({{'pid':pid,'start':start,'cgroup':open('/proc/self/cgroup').read()}},f)\ntime.sleep(60)\n",
        owned = world.root().join("owned-processes").display().to_string(),
        mark = world
            .root()
            .join("check-started.json")
            .display()
            .to_string()
    );
    plan["checks"]["check-ac-901-01"]["argv"] = json!(["python3", "-c", script]);
    plan["checks"]["check-ac-901-01"]["timeout_seconds"] = json!(90);
    std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan)?)?;
    world.git(&["add", "tests/check-plan.json"])?;
    world.git(&["commit", "-qm", "slow verification check"])?;
    world.verifier(
        "qa",
        &world::Verifier {
            checks: vec!["check-ac-901-01".into()],
            ..Default::default()
        },
    )?;
    let runtime = tokio::runtime::Runtime::new()?;
    let run = runtime.block_on(async {
        world.init_store().await?;
        world.seed_spec()?;
        // The older World fixture stores AC prose; S6 needs the approved index object's canonical body.
        let index: Value =
            serde_json::from_slice(&std::fs::read(world.project().join(specindex::PATH))?)?;
        let ac = &index["requirements"][0]["acceptance"][0];
        world.exec(&format!(
            "UPDATE acceptance_criteria SET body='{}';",
            ac.to_string().replace('\'', "''")
        ))?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "live-start", "S6 process lifetime").await?;
        host.shutdown().await;
        Ok::<_, Box<dyn std::error::Error>>(run)
    })?;
    let mut child = child_host(&world)?;
    if let Err(error) = world.wait("SELECT count(*) FROM attempts a JOIN tasks t ON a.task_id=t.task_id WHERE t.role IN ('qa','reviewer') AND a.state='RUNNING'","2",Duration::from_secs(30)) {
        return Err(format!("{error}; host={}; runs={:?}; tasks={:?}; turns={:?}",std::fs::read_to_string(world.root().join("host.log"))?,world.rows("SELECT * FROM runs")?,world.rows("SELECT task_id,state,blocked_reason FROM tasks")?,world.log("turns.jsonl")?).into());
    }
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while !world.root().join("check-started.json").exists()
        || world.log("verifier-held.jsonl")?.is_empty()
    {
        if std::time::Instant::now() > until {
            return Err(format!(
                "verifiers did not reach barriers: {}",
                std::fs::read_to_string(world.root().join("host.log"))?
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let attempts=world.rows("SELECT a.attempt_id,a.pid,a.process_start_id FROM attempts a JOIN tasks t ON a.task_id=t.task_id WHERE t.role IN ('qa','reviewer') ORDER BY t.role")?;
    assert_eq!(attempts.len(), 2);
    let mut check: Value =
        serde_json::from_slice(&std::fs::read(world.root().join("check-started.json"))?)?;
    let tracking=world.rows("SELECT json_extract(payload_json,'$.attempt_id') AS attempt_id,payload_json FROM events WHERE kind='attempt.tracking'")?;
    for attempt in &attempts {
        let row = tracking
            .iter()
            .find(|row| row["attempt_id"] == attempt["attempt_id"])
            .ok_or("tracking")?;
        let p: Value =
            serde_json::from_str(row["payload_json"].as_str().ok_or("tracking payload")?)?;
        assert_eq!(p["tracking"], "cgroup", "{p}");
        if attempt["attempt_id"] == attempts[0]["attempt_id"] {
            let path = p["cgroup"]
                .as_str()
                .ok_or("tracking cgroup")?
                .strip_prefix("/sys/fs/cgroup")
                .ok_or("cgroup mount")?;
            // The existing launcher adopts the check after spawn. Its first instruction can precede that
            // adoption, so establish S6's live-in-cgroup precondition from /proc before approving the change.
            // Keep the startup snapshot too; it is not evidence of the membership at approval.
            let pid = check["pid"].as_u64().ok_or("check pid")? as u32;
            let expected = format!("0::{path}\n");
            let until = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                assert!(!hoh_agents::identity::ended(pid, check["start"].as_str())?);
                let current = std::fs::read_to_string(format!("/proc/{pid}/cgroup"))?;
                if current == expected {
                    check["cgroup_at_approval"] = json!(current);
                    break;
                }
                assert!(
                    std::time::Instant::now() < until,
                    "run-check must share QA's recorded cgroup before approval: {current:?} != {expected:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    world.developer(&world::Developer {
        seconds: 60.0,
        commit: false,
        ..Default::default()
    })?;
    let candidate = world.query("SELECT candidate_id FROM candidates")?;
    let hash = change(&world)?;
    remote_command(
        &world,
        &run,
        "live-propose",
        "ProposeSpecChange",
        json!({"spec_sha256":hash,"reason":"S6"}),
    )?;
    let state = world.query("SELECT state FROM runs")?;
    if restart {
        std::fs::write(world.root().join("hold-approval"), b"hold")?;
        let info = info(&world)?;
        let revision = world.query("SELECT revision FROM runs")?.parse::<i64>()?;
        let body=json!({"schema_version":"1.0","command_id":"live-approve","idempotency_key":"live-approve","run_id":run,"expected_revision":revision,"kind":"ApproveSpec","payload":{"spec_sha256":hash,"decision":"APPROVE_ONLY"}}).to_string();
        let request = std::thread::spawn(move || {
            super::http(&info, "POST", "/v1/commands", Some(&body)).map_err(|e| e.to_string())
        });
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while !world.root().join("approval-committed").exists() {
            if std::time::Instant::now() > until {
                return Err("approval commit barrier".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            world.query("SELECT count(*) FROM attempts WHERE cancel_requested=1")?,
            "2"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='verification.round_closed'")?,
            "0"
        );
        assert!(!hoh_agents::identity::ended(
            check["pid"].as_u64().ok_or("pid")? as u32,
            check["start"].as_str()
        )?);
        child.0.kill()?;
        assert_eq!(child.0.wait()?.signal(), Some(9));
        assert!(request.join().map_err(|_| "request panicked")?.is_err());
        std::fs::write(world.root().join("release-approval"), b"release")?;
        child = child_host(&world)?;
    } else {
        remote_command(
            &world,
            &run,
            "live-approve",
            "ApproveSpec",
            json!({"spec_sha256":hash,"decision":"APPROVE_ONLY"}),
        )?;
    }
    let ids: Vec<&str> = attempts
        .iter()
        .map(|a| a["attempt_id"].as_str().ok_or("attempt id"))
        .collect::<Result<_, _>>()?;
    world.wait(
        &format!(
            "SELECT count(*) FROM attempts WHERE attempt_id IN ('{}','{}') AND state='CANCELLED'",
            ids[0], ids[1]
        ),
        "2",
        Duration::from_secs(20),
    )?;
    world.wait(
        "SELECT count(*) FROM events WHERE kind='verification.round_closed'",
        "1",
        Duration::from_secs(10),
    )?;
    for a in &attempts {
        assert!(hoh_agents::identity::ended(
            a["pid"].as_u64().ok_or("pid")? as u32,
            a["process_start_id"].as_str()
        )?);
    }
    assert!(hoh_agents::identity::ended(
        check["pid"].as_u64().ok_or("pid")? as u32,
        check["start"].as_str()
    )?);
    assert_eq!(world.query("SELECT json_extract(payload_json,'$.verdict') FROM events WHERE kind='verification.round_closed'")?,"superseded");
    assert_eq!(world.query("SELECT state FROM runs")?, state);
    assert_eq!(
        world.query(
            "SELECT count(*) FROM events WHERE kind IN ('run.cancel_requested','run.cancelled')"
        )?,
        "0"
    );
    assert_eq!(
        world.query(&format!(
            "SELECT state FROM candidates WHERE candidate_id='{candidate}'"
        ))?,
        "STALE"
    );
    let (_, body) = super::http(&info(&world)?, "GET", &format!("/v1/runs/{run}"), None)?;
    let row: Value = serde_json::from_str(&body)?;
    assert_eq!(row["projection"]["acceptance"]["accepted"], 0);
    let late = world.log("late-pass.jsonl")?;
    assert!(
        late.iter()
            .any(|r| r["role"] == "qa" && r["report"]["status"] == "PASS"),
        "QA must actually write its late PASS: {late:?}"
    );
    let events=world.rows("SELECT seq,kind,payload_json FROM events WHERE kind IN ('cancel.reaped','reconcile.attempt','verification.round_closed') ORDER BY seq")?;
    let closed_seq = events
        .iter()
        .find(|e| e["kind"] == "verification.round_closed")
        .and_then(|e| e["seq"].as_i64())
        .ok_or("closure seq")?;
    for id in &ids {
        let proof = events
            .iter()
            .find(|e| {
                let p: Value = serde_json::from_str(e["payload_json"].as_str().unwrap_or(""))
                    .unwrap_or_default();
                p["attempt_id"] == *id
                    && (e["kind"] == "cancel.reaped"
                        || (e["kind"] == "reconcile.attempt"
                            && p["attempt"]["to"] == "CANCELLED"
                            && (p["process"]["reaped"] == true || p["process"]["found"] == "gone")))
            })
            .ok_or("termination proof")?;
        assert!(
            proof["seq"].as_i64().ok_or("proof seq")? < closed_seq,
            "round closed before attempt {id} was proved ended"
        );
    }
    if let Some(dir) =
        std::env::var_os("HOH_S6_EVIDENCE_DIR").or_else(|| std::env::var_os("HOH_EVIDENCE_DIR"))
    {
        use std::io::Write as _;
        // The recovery check runs this binary twice. Each process keeps its own append-only proof artifact.
        let name = format!(
            "s6-{}-processes-{}.json",
            if restart { "restart" } else { "tick" },
            std::process::id()
        );
        std::fs::OpenOptions::new().write(true).create_new(true)
            .open(std::path::Path::new(&dir).join(name))?
            .write_all(&serde_json::to_vec_pretty(
                &json!({"classification":"product with fake agents; self-test, not acceptance","attempts":attempts,"check":check,"tracking":tracking,"late_reports":late,"proofs_and_close":events}),
            )?)?;
    }
    // A second host startup repeats recovery/round reconciliation without another closure.
    child.0.kill()?;
    child.0.wait()?;
    let _again = child_host(&world)?;
    world.wait(
        "SELECT count(*) FROM events WHERE kind='verification.round_closed'",
        "1",
        Duration::from_secs(5),
    )?;
    Ok(())
}
