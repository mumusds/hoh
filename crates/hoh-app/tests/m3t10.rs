//! Product host/store integration with scripted facts and FakeAgents (REQ-023, REQ-040).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn config(root: &Path) -> HostConfig {
    let mut c = HostConfig::new(root);
    c.port = 0;
    c.store.wal_probe = Some(ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: vec![],
        answer_timeout: Duration::from_secs(10),
    });
    c
}
fn db(root: &Path) -> PathBuf {
    root.join(".hoh/state.db")
}
fn sql(root: &Path, script: &str, query: bool) -> R<Value> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("python3")
        .args([
            "-c",
            r#"
import sqlite3,sys,json
c=sqlite3.connect(sys.argv[1],timeout=10); c.row_factory=sqlite3.Row
p=c.execute('SELECT project_id FROM projects').fetchone()[0]
s=sys.stdin.read().replace(':p',repr(p))
if sys.argv[2]=='query': print(json.dumps([dict(r) for r in c.execute(s)]))
else: c.executescript(s); c.commit(); print('null')
"#,
        ])
        .arg(db(root))
        .arg(if query { "query" } else { "seed" })
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("stdin")?
        .write_all(script.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}
fn events(root: &Path, kind: &str) -> R<Vec<Value>> {
    sql(
        root,
        &format!("SELECT payload_json FROM events WHERE kind='{kind}' ORDER BY seq"),
        true,
    )?
    .as_array()
    .ok_or("array")?
    .iter()
    .map(|v| {
        Ok(serde_json::from_str(
            v["payload_json"].as_str().ok_or("payload")?,
        )?)
    })
    .collect()
}
const WORLD: &str = r#"
INSERT INTO specs(spec_id,project_id,sha256,revision) VALUES ('s1',:p,printf('%064d',1),1);
INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at,config_snapshot_json) VALUES ('R',:p,'s1','RUNNING','AUTO','2026-11-22T00:00:00Z','{"supervision":{"policy":"efficient"}}');
INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES ('sc','codex','commander','1','{}');
INSERT INTO role_bindings(run_id,role,agent,session_id,role_epoch,config_revision,bound_at) VALUES ('R','commander','codex','sc',1,1,'2026-11-22T00:00:00Z');
"#;
fn round(root: &Path, n: usize) -> R {
    sql(root, &format!(r#"
INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,state,created_at)
 VALUES ('C{n}','R',printf('%040d',{n}),printf('%040d',{n}),printf('%064d',1),printf('%064d',2),printf('%064d',3),'.','VERIFYING','2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('q{n}','R','s1','IMPLEMENTED','qa',1),('v{n}','R','s1','IMPLEMENTED','reviewer',1);
INSERT OR IGNORE INTO issues(issue_id,run_id,fingerprint,severity,state,description) VALUES ('I','R','F','high','OPEN','repeated failure');
INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES
 (:p,'r{n}','R','verification.round_opened','{{"round_id":"r{n}","candidate_id":"C{n}","source_task_ids":[],"acceptance_ids":[],"tasks":{{"qa":"q{n}","reviewer":"v{n}"}}}}','2026-11-22T00:00:00Z'),
 (:p,'i{n}','R','issue.opened','{{"issue_id":"I","round_id":"r{n}","fingerprint":"F","severity":"high"}}','2026-11-22T00:00:00Z');
"#), false).map(drop)
}
#[tokio::test]
async fn test_040_01_third_round_writes_one_no_progress() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-progress-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    for n in 1..=4 {
        round(dir.path(), n)?;
        let closed = host
            .store()
            .reconcile_verification("R", Default::default())
            .await?;
        assert_eq!(closed.closed.len(), 1);
        let found = events(dir.path(), "no_progress")?;
        assert_eq!(
            found.len(),
            usize::from(n >= 3),
            "diagnosis after round {n}"
        );
        if n >= 3 {
            assert_eq!(found[0]["window"], 3);
            assert_eq!(found[0]["fingerprints"], json!(["F"]));
            assert_eq!(found[0]["last_progress_seq"], 0);
            assert!(found[0]["vector"].is_object());
        }
    }
    let now = SystemTime::now() + Duration::from_secs(20);
    let first = host
        .store()
        .supervise(
            "R",
            Context {
                now: Some(now),
                ..Default::default()
            },
        )
        .await?;
    let second = host
        .store()
        .supervise(
            "R",
            Context {
                now: Some(now + Duration::from_secs(3)),
                ..Default::default()
            },
        )
        .await?;
    let diagnosis_seq = sql(
        dir.path(),
        "SELECT seq FROM events WHERE kind='no_progress'",
        true,
    )?[0]["seq"]
        .as_i64()
        .ok_or("diagnosis seq")?;
    assert!(
        first
            .fired
            .iter()
            .chain(&second.fired)
            .any(|f| f.event_seqs.contains(&diagnosis_seq)
                && f.severity == hoh_core::supervise::Severity::High),
        "the actual diagnosis event must fire at High priority"
    );
    host.shutdown().await;
    let host = Host::start(config(dir.path())).await?;
    host.store()
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(events(dir.path(), "no_progress")?.len(), 1);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_040_01_stalled_suppresses_duplicate_diagnosis() -> R {
    for claim in [false, true] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-stalled-")
            .tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        sql(dir.path(), WORLD, false)?;
        sql(
            dir.path(),
            if claim {
                r#"INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'stalled','R','run.blocker_claimed','{"source":"runtime:stalled","reason":"NEED_DECISION"}','2026-11-22T00:00:00Z');"#
            } else {
                r#"INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'stalled','R','supervisor.retrigger','{}','2026-11-22T00:00:00Z');
INSERT INTO supervisor_triggers(trigger_id,run_id,fingerprint,severity,event_seqs_json,first_seq,last_seq,due_at,state,created_at) SELECT 'stalled','R','stalled/P/1#retrigger','high',json_array(seq),seq,seq,'2100-01-01T00:00:00Z','PENDING','2026-11-22T00:00:00Z' FROM events WHERE event_id='stalled';"#
            },
            false,
        )?;
        for n in 1..=3 {
            round(dir.path(), n)?;
            host.store()
                .reconcile_verification("R", Default::default())
                .await?;
        }
        assert!(events(dir.path(), "no_progress")?.is_empty());
        host.shutdown().await;
    }
    Ok(())
}

const ACCEPTED: &str = r#"
INSERT INTO requirements(requirement_id,spec_id,title,required_release) VALUES ('REQ-A','s1','a',1);
INSERT INTO acceptance_criteria(ac_id,requirement_id,spec_id,body,check_id) VALUES ('AC-A-01','REQ-A','s1','a','unit'),('AC-A-02','REQ-A','s1','b','unit');
INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,state,created_at)
 VALUES ('C0','R',printf('%040d',0),printf('%040d',0),printf('%064d',1),printf('%064d',2),printf('%064d',3),'.','ACCEPTED','2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('accepted','R','s1','ACCEPTED','qa',1);
INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES ('a0','accepted','SETTLED',1,1,1);
INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES ('artifact',printf('%064d',4),'.hoh/test.json',1,'application/json');
INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at) VALUES ('e0','C0','a0','artifact','unit','check','PASS','product','2026-11-22T00:00:00Z');
INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status) VALUES ('AC-A-01','s1','C0','e0','PASS');
"#;
#[tokio::test]
async fn test_040_01_milestone_and_release_producers_use_current_acceptance_once() -> R {
    let dir = tempfile::Builder::new().prefix("m3t10-ready-").tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), ACCEPTED, false)?;
    round(dir.path(), 1)?;
    host.store()
        .reconcile_verification("R", Default::default())
        .await?;
    assert!(
        events(dir.path(), "milestone_ready")?.is_empty(),
        "one passed AC is not a passed requirement"
    );
    assert!(events(dir.path(), "release_request")?.is_empty());
    let observed = events(dir.path(), "progress.observed")?;
    assert_eq!(observed.len(), 1, "a closed round records progress facts");
    assert_eq!(
        observed[0]["round"]["vector"]["accepted"], 1,
        "partial AC progress must count"
    );
    sql(
        dir.path(),
        "INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status) VALUES ('AC-A-02','s1','C0','e0','PASS');",
        false,
    )?;
    round(dir.path(), 2)?;
    host.store()
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(events(dir.path(), "milestone_ready")?.len(), 1);
    assert!(
        events(dir.path(), "release_request")?.is_empty(),
        "open high issue prevents release"
    );
    sql(
        dir.path(),
        "UPDATE issues SET severity='low' WHERE issue_id='I';",
        false,
    )?;
    round(dir.path(), 3)?;
    host.store()
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(events(dir.path(), "release_request")?.len(), 1);
    round(dir.path(), 4)?;
    host.store()
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(events(dir.path(), "milestone_ready")?.len(), 1);
    assert_eq!(events(dir.path(), "release_request")?.len(), 1);
    host.shutdown().await;
    Ok(())
}
#[tokio::test]
async fn test_040_03_requirement_growth_resets_persisted_window() -> R {
    let dir = tempfile::Builder::new().prefix("m3t10-growth-").tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    for n in 1..=3 {
        if n == 2 {
            sql(
                dir.path(),
                "INSERT INTO requirements(requirement_id,spec_id,title,required_release) VALUES ('REQ-NEW','s1','new',1);",
                false,
            )?;
        }
        round(dir.path(), n)?;
        host.store()
            .reconcile_verification("R", Default::default())
            .await?;
    }
    assert!(events(dir.path(), "no_progress")?.is_empty());
    let reset = events(dir.path(), "progress.reset")?;
    assert_eq!(reset.len(), 1);
    assert_eq!(reset[0]["reason"], "规格新增 REQ");
    host.shutdown().await;
    Ok(())
}

fn message(id: &str, payload: Value) -> R<hoh_core::protocol::Message> {
    Ok(serde_json::from_value(
        json!({"schema_version":"1.0","message_id":id,"run_id":"R","task_id":"D","attempt_id":"A","sender_role":"developer","recipient_role":"researcher","role_epoch":1,"correlation_id":id,"causation_id":null,"idempotency_key":id,"expected_revision":0,"spec_sha256":format!("{:064}",1),"candidate_id":null,"kind":"query","payload":payload}),
    )?)
}
const PARTICIPANTS: &str = r#"
INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES ('sd','claude','developer','1','{}'),('sr','pi','researcher','1','{}');
INSERT INTO role_bindings(run_id,role,agent,session_id,role_epoch,config_revision,bound_at) VALUES ('R','developer','claude','sd',1,1,'2026-11-22T00:00:00Z'),('R','researcher','pi','sr',1,1,'2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) VALUES ('D','R','s1','RUNNING','developer',1,1);
INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('A','D','sd','RUNNING',1,1,1);
"#;

#[tokio::test]
async fn test_023_03_discussion_limit_follows_max_rounds() -> R {
    for max in [2, 3] {
        let dir = tempfile::Builder::new()
            .prefix("m3t10-discussion-")
            .tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        if max == 3 {
            sql(
                dir.path(),
                "UPDATE runs SET config_snapshot_json=json_set(config_snapshot_json,'$.supervision.max_discussion_rounds',3) WHERE run_id='R';",
                false,
            )?;
        }
        for n in 1..=max + 2 {
            let request = message(
                &format!("opinion-{n}"),
                json!({"intent":"opinion","controversy_id":"architecture","text":format!("opinion {n}"),"evidence_sha":format!("{n:064}")}),
            )?;
            let reply = host
                .store()
                .consult_request(request.clone())
                .await?
                .ok_or("query not handled")?;
            assert_eq!(
                reply["status"],
                if n <= max { "accepted" } else { "rejected" }
            );
            if n > max {
                assert_eq!(reply["reason_code"], "DISCUSSION_LIMIT");
            }
            assert_eq!(host.store().consult_request(request).await?, Some(reply));
        }
        let opinions = events(dir.path(), "discussion.opinion")?;
        assert_eq!(opinions.len(), max as usize);
        let escalation = events(dir.path(), "need_decision")?;
        assert_eq!(escalation.len(), 1);
        assert_eq!(
            escalation[0]["opinions"].as_array().map(Vec::len),
            Some(max as usize)
        );
        assert_eq!(
            sql(dir.path(), "SELECT state FROM runs WHERE run_id='R'", true)?[0]["state"],
            "RUNNING"
        );
        let conclude = message(
            "conclude",
            json!({"intent":"conclude","controversy_id":"architecture","adr":"docs/adr/ADR-037.md"}),
        )?;
        assert_eq!(
            host.store()
                .consult_request(conclude)
                .await?
                .ok_or("conclude")?["status"],
            "accepted"
        );
        assert_eq!(
            events(dir.path(), "discussion.opinion")?.len(),
            max as usize
        );
        host.shutdown().await;
    }
    Ok(())
}
#[tokio::test]
async fn test_023_03_repeated_evidence_and_stale_sender_do_not_add_opinions() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-evidence-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let payload =
        json!({"intent":"opinion","controversy_id":"c","text":"one","evidence_sha":"same"});
    assert_eq!(
        host.store()
            .consult_request(message("first", payload.clone())?)
            .await?
            .ok_or("opinion")?["status"],
        "accepted"
    );
    let reply = host
        .store()
        .consult_request(message("again", payload)?)
        .await?
        .ok_or("opinion")?;
    assert_eq!(reply["reason_code"], "DISCUSSION_LIMIT");
    assert_eq!(reply["details"]["cause"], "no_new_evidence");
    assert_eq!(events(dir.path(), "discussion.opinion")?.len(), 1);
    let mut stale = message(
        "stale",
        json!({"intent":"opinion","controversy_id":"other","text":"late"}),
    )?;
    stale.role_epoch = 2;
    assert_eq!(
        host.store().consult_request(stale).await?.ok_or("stale")?["reason_code"],
        "STALE_ROLE_EPOCH"
    );
    assert_eq!(events(dir.path(), "discussion.opinion")?.len(), 1);
    host.shutdown().await;
    Ok(())
}

#[derive(Debug, Default)]
struct Clock(std::sync::atomic::AtomicU64);
impl hoh_core::delivery::Clock for Clock {
    fn now(&self) -> SystemTime {
        SystemTime::now() + Duration::from_secs(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}
impl Clock {
    fn advance(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
#[derive(Debug, Default)]
struct FakeCommander {
    summaries: std::sync::Mutex<Vec<Value>>,
}
impl hoh_app::host::Executor for FakeCommander {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(if work.role == "commander" {
            Ok(())
        } else {
            Err("fixture does not execute work".into())
        });
        rx
    }
    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let refused = |detail: String| hoh_app::host::Refusal {
            reason: "FIXTURE".into(),
            class: hoh_core::retry::FailureClass::Capability,
            detail: Some(detail),
            details: None,
        };
        let input = work
            .assignment
            .supervision
            .clone()
            .ok_or_else(|| refused("not supervision".into()))?;
        self.summaries
            .lock()
            .map_err(|e| refused(e.to_string()))?
            .push(input.summary.clone());
        let message = json!({"schema_version":"1.0","message_id":"answer","run_id":work.run_id,"task_id":work.task_id,"attempt_id":work.attempt_id,"sender_role":"commander","recipient_role":"runtime","role_epoch":input.role_epoch,"correlation_id":"c","causation_id":input.wake,"idempotency_key":"k","expected_revision":input.run_revision,"spec_sha256":format!("{:064}",1),"candidate_id":null,"kind":"decision","payload":{"kind":"record_assumption","statement":"Conclusion: docs/adr/ADR-037.md","basis":input.summary.to_string()}});
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r go; exit 0"])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| refused(e.to_string()))?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let gate = hoh_app::host::Gate::new(move || {
            use std::io::Write as _;
            stdin
                .ok_or("stdin")?
                .write_all(b"go\n")
                .map_err(|e| e.to_string())
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = tx.send(hoh_app::host::Outcome {
                success: status.is_ok_and(|s| s.success()),
                detail: "scripted decision".into(),
                group_gone: hoh_agents::identity::group_empty(pid),
                class: hoh_core::retry::FailureClass::Capability,
                observed: hoh_core::persistence::Observed {
                    messages: vec![json!({"nonce":input.nonce,"message":message}).to_string()],
                    ..Default::default()
                },
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended: rx,
            prompt: None,
            ran: None,
        })
    }
}
async fn wait_sql(
    root: &Path,
    clock: &Clock,
    query: &str,
    done: impl Fn(&Value) -> bool,
) -> R<Value> {
    let until = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let found = sql(root, query, true)?;
        if done(&found) {
            return Ok(found);
        }
        if std::time::Instant::now() > until {
            return Err(format!("waiting for {query}: {found}").into());
        }
        clock.advance();
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_023_03_commander_records_conclusion_with_opinion_references() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-conclusion-")
        .tempdir()?;
    let clock = std::sync::Arc::new(Clock::default());
    let fake = std::sync::Arc::new(FakeCommander::default());
    let mut c = config(dir.path());
    c.clock = hoh_app::host::HostClock(clock.clone());
    c.runner = Some(hoh_app::host::RunnerConfig {
        executor: fake.clone(),
        context: Context::default(),
    });
    let host = Host::start(c).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    for n in 1..=4 {
        let reply=host.store().consult_request(message(&format!("opinion-{n}"),json!({"intent":"opinion","controversy_id":"architecture","text":format!("opinion {n}")}))?).await?.expect("discussion request must be handled before waking Commander");
        assert_eq!(
            reply["status"],
            if n <= 2 { "accepted" } else { "rejected" }
        );
    }
    wait_sql(dir.path(), &clock, "SELECT state FROM decisions", |v| {
        v[0]["state"] == "APPLIED"
    })
    .await?;
    wait_sql(
        dir.path(),
        &clock,
        "SELECT a.state FROM attempts a JOIN tasks t USING(task_id) WHERE t.role='commander'",
        |v| v[0]["state"] == "SETTLED",
    )
    .await?;
    let refs = sql(
        dir.path(),
        "SELECT event_id FROM events WHERE kind='discussion.opinion' ORDER BY seq",
        true,
    )?;
    let summaries = fake.summaries.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(summaries.len(), 1);
    for opinion in refs.as_array().ok_or("refs")? {
        assert!(
            summaries[0]
                .to_string()
                .contains(opinion["event_id"].as_str().ok_or("id")?)
        );
    }
    let decision = sql(dir.path(), "SELECT payload_json FROM decisions", true)?;
    assert!(
        decision[0]["payload_json"]
            .as_str()
            .ok_or("decision")?
            .contains("ADR-037.md")
    );
    assert_eq!(events(dir.path(), "need_decision")?.len(), 1);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_04_major_decision_escalates_before_dispatch() -> R {
    let dir = tempfile::Builder::new().prefix("m3t10-major-").tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let tasks = sql(
        dir.path(),
        "SELECT task_id FROM tasks ORDER BY task_id",
        true,
    )?;
    for class in ["spec", "adr", "public_api", "gate"] {
        let request = message(
            class,
            json!({"intent":"apply","decision_class":class,"statement":format!("Change {class} signature")}),
        )?;
        let reply = host
            .store()
            .consult_request(request.clone())
            .await?
            .ok_or("major conclusion not handled")?;
        assert_eq!(reply["status"], "rejected");
        assert_eq!(reply["reason_code"], "ILLEGAL_TRANSITION");
        assert_eq!(reply["details"]["decision_class"], class);
        assert_eq!(host.store().consult_request(request).await?, Some(reply));
    }
    assert_eq!(events(dir.path(), "need_decision")?.len(), 4);
    assert_eq!(
        sql(
            dir.path(),
            "SELECT task_id FROM tasks ORDER BY task_id",
            true
        )?,
        tasks
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT count(*) n FROM outbox WHERE json_extract(payload_json,'$.kind')='dispatch'",
            true
        )?[0]["n"],
        0
    );
    assert_eq!(
        sql(dir.path(), "SELECT count(*) n FROM decisions", true)?[0]["n"],
        0
    );
    let plain = message(
        "plain",
        json!({"question":"Does this public API have a useful name?"}),
    )?;
    assert!(
        host.store().consult_request(plain).await?.is_none(),
        "free text is not a major-decision classifier"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_02_consult_cycle_and_change_request_issue() -> R {
    let dir = tempfile::Builder::new().prefix("m3t10-cycle-").tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        r#"
INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('research','R','s1','RUNNING','researcher',1);
INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('AR','research','sr','RUNNING',1,1,1);
INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'waiting','R','task.consult','{}','2026-11-22T00:00:00Z');
INSERT INTO outbox(message_id,event_seq,run_id,recipient_role,role_epoch,payload_json,state) SELECT 'waiting',seq,'R','researcher',1,'{"kind":"query","sender_role":"developer","recipient_role":"researcher"}','PENDING' FROM events WHERE event_id='waiting';
"#,
        false,
    )?;
    let before = sql(
        dir.path(),
        "SELECT task_id FROM tasks ORDER BY task_id",
        true,
    )?;
    let mut request = message(
        "cycle",
        json!({"intent":"consult","question":"Ask developer"}),
    )?;
    request.task_id = "research".into();
    request.attempt_id = "AR".into();
    request.sender_role = "researcher".into();
    request.recipient_role = "developer".into();
    let reply = host
        .store()
        .consult_request(request)
        .await?
        .ok_or("cycle not handled")?;
    assert_eq!(reply["reason_code"], "CONSULT_CYCLE");
    assert_eq!(
        reply["details"]["members"],
        json!(["researcher", "developer"])
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT task_id FROM tasks ORDER BY task_id",
            true
        )?,
        before
    );
    let mut self_wait = message("self", json!({"intent":"consult","question":"Ask myself"}))?;
    self_wait.recipient_role = "developer".into();
    assert_eq!(
        host.store()
            .consult_request(self_wait)
            .await?
            .ok_or("self wait")?["details"]["members"],
        json!(["developer"])
    );
    round(dir.path(), 1)?;
    sql(
        dir.path(),
        r#"
INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES ('sq','pi','qa','1','{}');
INSERT INTO role_bindings(run_id,role,agent,session_id,role_epoch,config_revision,bound_at) VALUES ('R','qa','pi','sq',1,1,'2026-11-22T00:00:00Z');
UPDATE tasks SET state='RUNNING' WHERE task_id='q1';
INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('AQ','q1','sq','RUNNING',1,1,1);
"#,
        false,
    )?;
    let issue = host
        .store()
        .verifier_request(hoh_core::persistence::VerifierRequest::RejectChange(
            hoh_core::persistence::ChangeRequest {
                run_id: "R".into(),
                attempt_id: "AQ".into(),
                message_id: "change".into(),
                round_id: "r1".into(),
                candidate_id: "C1".into(),
                severity: "high".into(),
                text: "Change the candidate under verification".into(),
            },
        ))
        .await?
        .ok_or("change issue")?;
    assert!(
        events(dir.path(), "request.rejected")?
            .iter()
            .any(|r| r["reason_code"] == "ILLEGAL_TRANSITION")
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!("SELECT state FROM issues WHERE issue_id='{issue}'"),
            true
        )?[0]["state"],
        "OPEN"
    );
    assert_eq!(
        sql(
            dir.path(),
            "SELECT count(*) n FROM outbox WHERE json_extract(payload_json,'$.kind')='dispatch'",
            true
        )?[0]["n"],
        0
    );
    host.shutdown().await;
    Ok(())
}
