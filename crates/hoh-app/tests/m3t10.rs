//! Product host/store integration with scripted facts and FakeAgents (REQ-023, REQ-040).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::Context;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

mod m3t10_fixes;
mod pty;
mod world;

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
    use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
    let world = world::World::named("m3t10-progress")?;
    world.configure(&[], "")?;
    let root = world.project();
    let host = Host::start(config(&root)).await?;
    sql(&root, WORLD, false)?;
    sql(&root, CURRENT_SPEC_REVIEW, false)?;
    for n in 1..=4 {
        round(&root, n)?;
        let closed = host
            .store()
            .reconcile_verification("R", Default::default())
            .await?;
        assert_eq!(closed.closed.len(), 1);
        let found = events(&root, "no_progress")?;
        assert_eq!(
            found.len(),
            usize::from(n >= 3),
            "diagnosis after round {n}"
        );
        if n >= 3 {
            assert_eq!(found[0]["window"], 3);
            assert_eq!(found[0]["fingerprints"], json!(["F"]));
            assert_eq!(found[0]["last_progress_seq"], 0);
            assert_eq!(
                found[0]["vector"],
                json!({
                    "accepted": 0,
                    "closed_issues": 0,
                    "reopened_issues": 0,
                    "new_critical_high": 1,
                    "decision_risks": 0
                }),
                "exact net-progress vector components: {}",
                found[0]["vector"]
            );
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
        &root,
        "SELECT seq FROM events WHERE kind='no_progress'",
        true,
    )?[0]["seq"]
        .as_i64()
        .ok_or("diagnosis seq")?;
    assert_eq!(
        first
            .fired
            .iter()
            .chain(&second.fired)
            .filter(|f| f.event_seqs.contains(&diagnosis_seq)
                && f.severity == hoh_core::supervise::Severity::High)
            .count(),
        1,
        "the diagnosis event must fire at High priority exactly once"
    );
    let woken = sql(
        &root,
        &format!(
            "SELECT json_extract(payload_json,'$.severity') AS severity FROM events \
             WHERE kind='supervisor.woken' AND run_id='R' \
             AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.event_seqs') WHERE value={diagnosis_seq})"
        ),
        true,
    )?;
    assert_eq!(
        woken,
        json!([{"severity": "high"}]),
        "persisted supervisor.woken must record exactly one High wake"
    );
    let triggers = sql(
        &root,
        &format!(
            "SELECT state, severity FROM supervisor_triggers \
             WHERE run_id='R' AND EXISTS (SELECT 1 FROM json_each(event_seqs_json) WHERE value={diagnosis_seq})"
        ),
        true,
    )?;
    assert_eq!(
        triggers,
        json!([{"state": "HANDLED", "severity": "high"}]),
        "persisted supervisor_triggers must record one HANDLED High trigger"
    );
    host.shutdown().await;

    // Real host process SIGKILL and restart: the restarted host process itself reconciles round 5.
    let spawn_host = || -> R<pty::Hoh> {
        Ok(pty::Hoh(
            std::process::Command::new(world::HOH)
                .args(["__host"])
                .arg(&root)
                .envs(world.env())
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        ))
    };
    let wait_host_pid = |expected: u32| -> R {
        let end = std::time::Instant::now() + Duration::from_secs(20);
        let path = root.join(".hoh/host.json");
        loop {
            if let Ok(bytes) = std::fs::read(&path)
                && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
                && v["pid"].as_u64() == Some(u64::from(expected))
            {
                return Ok(());
            }
            if std::time::Instant::now() >= end {
                return Err(format!("host.json did not record pid {expected}").into());
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    let mut first_proc = spawn_host()?;
    let first_pid = first_proc.0.id();
    wait_host_pid(first_pid)?;
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(first_pid)?).ok_or("pid")?,
        rustix::process::Signal::KILL,
    )?;
    let killed = first_proc.0.wait()?;
    assert_eq!(
        killed.signal(),
        Some(9),
        "first host process must end by SIGKILL"
    );

    let mut restarted = spawn_host()?;
    let restarted_pid = restarted.0.id();
    wait_host_pid(restarted_pid)?;
    round(&root, 5)?;
    let wait_end = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let closed = sql(
            &root,
            "SELECT count(*) AS n FROM events WHERE kind='verification.round_closed'",
            true,
        )?[0]["n"]
            .as_i64()
            .unwrap_or(0);
        if closed >= 5 {
            break;
        }
        if std::time::Instant::now() >= wait_end {
            return Err(format!("restarted host did not close round 5 (closed={closed})").into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(events(&root, "no_progress")?.len(), 1);
    let woken_after = sql(
        &root,
        &format!(
            "SELECT json_extract(payload_json,'$.severity') AS severity FROM events \
             WHERE kind='supervisor.woken' AND run_id='R' \
             AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.event_seqs') WHERE value={diagnosis_seq})"
        ),
        true,
    )?;
    assert_eq!(woken_after, json!([{"severity": "high"}]));
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(restarted_pid)?).ok_or("pid")?,
        rustix::process::Signal::TERM,
    );
    let _ = restarted.0.wait();
    Ok(())
}

#[tokio::test]
async fn test_040_01_severity_fall_allows_a_new_persisted_fingerprint_window() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-severity-")
        .tempdir()?;
    let mut host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    for n in 1..=7 {
        round(dir.path(), n)?;
        if n >= 4 {
            sql(
                dir.path(),
                &format!(
                    r#"
UPDATE issues SET severity='low',fingerprint='G' WHERE issue_id='I';
UPDATE events SET payload_json=json_set(payload_json,'$.fingerprint','G','$.severity','low') WHERE event_id='i{n}';
"#
                ),
                false,
            )?;
        }
        host.store()
            .reconcile_verification("R", Default::default())
            .await?;
        let found = events(dir.path(), "no_progress")?;
        assert_eq!(
            found.len(),
            usize::from(n >= 3) + usize::from(n >= 6),
            "round {n}"
        );
        if n >= 6 {
            assert_eq!(found[0]["fingerprints"], json!(["F"]));
            assert_eq!(found[1]["fingerprints"], json!(["G"]));
            assert_eq!(found[0]["last_progress_seq"], found[1]["last_progress_seq"]);
        }
        if n == 5 {
            host.shutdown().await;
            host = Host::start(config(dir.path())).await?;
        }
    }
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

// Scripted prerequisite, as in supervision.rs: these tests start after the specification's review,
// so no spec_review.waiting wake competes with the consultation/no_progress wake they count exactly.
const CURRENT_SPEC_REVIEW: &str = r#"
INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'reviewed-s1',NULL,
 'spec_review.reported',json_object('review_id','rv-s1','spec_id','s1','spec_revision',1,'status','PASS',
 'state','CURRENT','reasons',json_array(),'session_id','sv','findings',json_array()),'2026-11-22T00:00:00Z');
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
        // M3K-03 / M3QA-011: both project A (max=2) and project B (max=3) wake Commander exactly once, observed in persisted records.
        let seq = sql(
            dir.path(),
            "SELECT seq FROM events WHERE kind='need_decision' AND run_id='R'",
            true,
        )?[0]["seq"]
            .as_i64()
            .ok_or("discussion seq")?;
        // Quiet is 2s (supervise::QUIET). The step's clock must be past the trigger's due time.
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
        assert_eq!(
            first
                .fired
                .iter()
                .filter(|fired| fired.event_seqs.contains(&seq))
                .count(),
            1,
            "discussion limit fires once (max={max}): {:?}",
            first.fired
        );
        assert!(
            !second
                .fired
                .iter()
                .any(|fired| fired.event_seqs.contains(&seq)),
            "a second supervise must not re-fire it (max={max}): {:?}",
            second.fired
        );
        let woken = sql(
            dir.path(),
            &format!(
                "SELECT json_extract(payload_json,'$.severity') AS severity FROM events \
                 WHERE kind='supervisor.woken' AND run_id='R' \
                 AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.event_seqs') WHERE value={seq})"
            ),
            true,
        )?;
        assert_eq!(
            woken,
            json!([{"severity": "high"}]),
            "persisted supervisor.woken must record exactly one High wake (max={max})"
        );
        let triggers = sql(
            dir.path(),
            &format!(
                "SELECT state, severity FROM supervisor_triggers \
                 WHERE run_id='R' AND EXISTS (SELECT 1 FROM json_each(event_seqs_json) WHERE value={seq})"
            ),
            true,
        )?;
        assert_eq!(
            triggers,
            json!([{"state": "HANDLED", "severity": "high"}]),
            "persisted supervisor_triggers must record one HANDLED High trigger (max={max})"
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
        let conclusions = events(dir.path(), "discussion.conclusion")?;
        assert_eq!(
            conclusions.len(),
            1,
            "product must persist the discussion conclusion event (max={max})"
        );
        assert_eq!(conclusions[0]["controversy_id"], "architecture");
        assert_eq!(conclusions[0]["conclusion"]["adr"], "docs/adr/ADR-037.md");
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

#[tokio::test]
async fn test_023_03_early_stop_closes_the_controversy_across_restart() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-early-stop-")
        .tempdir()?;
    let mut host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET config_snapshot_json=json_set(config_snapshot_json,'$.supervision.max_discussion_rounds',3) WHERE run_id='R';",
        false,
    )?;
    for (n, evidence) in ["e1", "e1", "e2", "e3", "e4"].into_iter().enumerate() {
        let request = message(
            &format!("early-{n}"),
            json!({"intent":"opinion","controversy_id":"early","evidence_sha":evidence}),
        )?;
        let reply = host
            .store()
            .consult_request(request.clone())
            .await?
            .ok_or("opinion")?;
        assert_eq!(
            reply["status"],
            if n == 0 { "accepted" } else { "rejected" },
            "opinion {n}"
        );
        if n > 0 {
            assert_eq!(reply["reason_code"], "DISCUSSION_LIMIT");
            assert_eq!(reply["details"]["cause"], "no_new_evidence");
        }
        assert_eq!(host.store().consult_request(request).await?, Some(reply));
        if n == 1 {
            host.shutdown().await;
            host = Host::start(config(dir.path())).await?;
        }
    }
    assert_eq!(events(dir.path(), "discussion.opinion")?.len(), 1);
    let escalation = events(dir.path(), "need_decision")?;
    assert_eq!(escalation.len(), 1);
    assert_eq!(
        escalation[0]["opinions"],
        json!(["A/consult/early-0/opinion"])
    );
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
    plan: Option<&'static str>,
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
        let plan_task = |id: &str, role: &str| json!({"kind":"create_tasks","plan":{"tasks":[{"task_id":id,"title":id,"role":role,"writer":false,"depends_on":["D"],"requirement_ids":["REQ-040"]}]}});
        let payloads = match self.plan {
            Some("replan") => vec![
                json!({"kind":"transition_task","task_id":"repeat","to_state":"CANCELLED","reason":"replace repeated failure"}),
                plan_task("replacement", "developer"),
            ],
            Some("research") => vec![plan_task("investigation", "researcher")],
            _ => vec![
                json!({"kind":"record_assumption","statement":"Conclusion: docs/adr/ADR-037.md","basis":input.summary.to_string()}),
            ],
        };
        let messages=payloads.into_iter().enumerate().map(|(n,payload)| {
            let message=json!({"schema_version":"1.0","message_id":format!("answer-{n}"),"run_id":work.run_id,"task_id":work.task_id,"attempt_id":work.attempt_id,"sender_role":"commander","recipient_role":"runtime","role_epoch":input.role_epoch,"correlation_id":"c","causation_id":input.wake,"idempotency_key":format!("k-{n}"),"expected_revision":input.run_revision+n as i64,"spec_sha256":format!("{:064}",1),"candidate_id":null,"kind":"decision","payload":payload});
            json!({"nonce":input.nonce,"message":message}).to_string()
        }).collect();
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
                group_gone: world::group_exit::group_gone_after_reap(pid),
                class: hoh_core::retry::FailureClass::Capability,
                observed: hoh_core::persistence::Observed {
                    messages,
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
    // Seed the complete discussion before a runner can observe the partially built world and wake for STALLED.
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), CURRENT_SPEC_REVIEW, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    for n in 1..=4 {
        let reply=host.store().consult_request(message(&format!("opinion-{n}"),json!({"intent":"opinion","controversy_id":"architecture","text":format!("opinion {n}")}))?).await?.expect("discussion request must be handled before waking Commander");
        assert_eq!(
            reply["status"],
            if n <= 2 { "accepted" } else { "rejected" }
        );
    }
    host.shutdown().await;
    let host = Host::start(c).await?;
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
    assert_eq!(summaries.len(), 1, "{summaries:#?}");
    for opinion in refs.as_array().ok_or("refs")? {
        assert!(
            summaries[0]
                .to_string()
                .contains(opinion["event_id"].as_str().ok_or("id")?)
        );
    }
    let decision = sql(
        dir.path(),
        "SELECT state, payload_json FROM decisions",
        true,
    )?;
    assert_eq!(decision.as_array().map(Vec::len), Some(1), "{decision}");
    assert_eq!(decision[0]["state"], "APPLIED", "{decision}");
    let payload: Value =
        serde_json::from_str(decision[0]["payload_json"].as_str().ok_or("decision")?)?;
    assert_eq!(payload["kind"], "record_assumption", "{payload}");
    assert!(
        payload["statement"]
            .as_str()
            .is_some_and(|s| s.contains("docs/adr/ADR-037.md")),
        "{payload}"
    );
    let applied_events = events(dir.path(), "decision.applied")?;
    assert_eq!(applied_events.len(), 1, "{applied_events:?}");
    assert!(
        applied_events[0]["payload"]["statement"]
            .as_str()
            .is_some_and(|s| s.contains("docs/adr/ADR-037.md")),
        "{applied_events:?}"
    );
    assert_eq!(events(dir.path(), "need_decision")?.len(), 1);
    let seq = sql(
        dir.path(),
        "SELECT seq FROM events WHERE kind='need_decision' AND run_id='R'",
        true,
    )?[0]["seq"]
        .as_i64()
        .ok_or("need_decision seq")?;
    let woken = sql(
        dir.path(),
        &format!(
            "SELECT json_extract(payload_json,'$.severity') AS severity FROM events \
             WHERE kind='supervisor.woken' AND run_id='R' \
             AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.event_seqs') WHERE value={seq})"
        ),
        true,
    )?;
    assert_eq!(
        woken,
        json!([{"severity": "high"}]),
        "Commander must have exactly one persisted High wake for need_decision"
    );
    let triggers = sql(
        dir.path(),
        &format!(
            "SELECT state, severity FROM supervisor_triggers \
             WHERE run_id='R' AND EXISTS (SELECT 1 FROM json_each(event_seqs_json) WHERE value={seq})"
        ),
        true,
    )?;
    assert_eq!(
        triggers,
        json!([{"state": "HANDLED", "severity": "high"}]),
        "persisted trigger for need_decision must be HANDLED once"
    );
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
    let scheduled = host
        .store()
        .schedule_round(
            "R",
            Context {
                required_capabilities: [("developer".into(), Default::default())].into(),
                ..Default::default()
            },
        )
        .await?;
    assert!(
        scheduled.dispatched.is_none(),
        "no task may be dispatched before escalation is handled: {scheduled:?}"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_023_04_consultation_conclusion_escalates_and_schedules_nothing_before_commander() -> R
{
    let dir = tempfile::Builder::new()
        .prefix("m3t10-major-host-")
        .tempdir()?;
    Host::start(config(dir.path())).await?.shutdown().await;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), CURRENT_SPEC_REVIEW, false)?;
    sql(
        dir.path(),
        PARTICIPANTS
            .replace(
                "'D','R','s1','RUNNING','developer',1,1",
                "'D','R','s1','READY','developer',0,1",
            )
            .split("INSERT INTO attempts")
            .next()
            .ok_or("seed")?,
        false,
    )?;
    sql(
        dir.path(),
        "UPDATE sessions SET capabilities_json='{\"mid_turn_steer\":true}'; UPDATE runs SET deadline_at='2099-01-01T00:00:00Z' WHERE run_id='R'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',600);",
        false,
    )?;
    let (stx, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (itx, mut inputs) = tokio::sync::mpsc::unbounded_channel();
    let fake = std::sync::Arc::new(FakeConsult {
        started: stx,
        steers: itx,
        research: Default::default(),
        summary: Some("改公开接口的签名"),
    });
    let clock = std::sync::Arc::new(Clock::default());
    let mut c = config(dir.path());
    c.clock = hoh_app::host::HostClock(clock.clone());
    c.runner = Some(hoh_app::host::RunnerConfig {
        executor: fake.clone(),
        context: Context {
            required_capabilities: [
                ("developer".into(), Default::default()),
                ("researcher".into(), Default::default()),
            ]
            .into(),
            ..Default::default()
        },
    });
    let host = Host::start(c).await?;
    let outcome: R = async {
        let (work, mut release) = tokio::time::timeout(Duration::from_secs(20), starts.recv())
            .await?
            .ok_or("developer started")?;
        let helpers = work
            .helpers
            .as_ref()
            .ok_or("running developer must have request bridge")?;
        let revision = sql(
            dir.path(),
            "SELECT revision FROM tasks WHERE task_id='D'",
            true,
        )?[0]["revision"]
            .as_u64()
            .ok_or("revision")?;
        // 1. Real developer -> researcher consultation through the host request bridge and delivery loop.
        let mut consult = message(
            "consult-api",
            json!({"intent":"consult","question":"是否需要改公开接口的签名？","budget":{"wall_seconds":30},"deadline":"2098-01-01T00:00:00Z"}),
        )?;
        consult.attempt_id = work.attempt_id.clone();
        consult.role_epoch = work.assignment.role_epoch as u64;
        consult.expected_revision = revision;
        let consult_file = dir.path().join("consult-api.json");
        std::fs::write(&consult_file, serde_json::to_vec(&consult)?)?;
        let ask = helpers.ask.clone();
        let asked = tokio::task::spawn_blocking(move || {
            std::process::Command::new(ask).arg(consult_file).output()
        })
        .await??;
        assert!(asked.status.success(), "{}", String::from_utf8_lossy(&asked.stderr));
        let consult_reply: Value = serde_json::from_slice(&asked.stdout)?;
        assert_eq!(consult_reply["status"], "queued", "{consult_reply}");
        let child_task = consult_reply["task_id"].as_str().ok_or("child task_id")?.to_owned();
        let delivered = tokio::time::timeout(Duration::from_secs(20), inputs.recv())
            .await?
            .ok_or("consultation conclusion delivered")?;
        assert_eq!(delivered.target_role.as_deref(), Some("developer"));
        assert!(
            delivered.payload.to_string().contains("改公开接口的签名"),
            "researcher conclusion must reach developer: {:?}",
            delivered.payload
        );
        // 2. Developer attempts to apply that consultation conclusion directly through the request bridge.
        let mut apply = message(
            "apply-public-api",
            json!({"intent":"apply","decision_class":"public_api","statement":"改公开接口的签名"}),
        )?;
        apply.attempt_id = work.attempt_id.clone();
        apply.role_epoch = work.assignment.role_epoch as u64;
        apply.expected_revision = revision;
        let apply_file = dir.path().join("apply-api.json");
        std::fs::write(&apply_file, serde_json::to_vec(&apply)?)?;
        let ask = helpers.ask.clone();
        let applied = tokio::task::spawn_blocking(move || {
            std::process::Command::new(ask).arg(apply_file).output()
        })
        .await??;
        assert!(applied.status.success(), "{}", String::from_utf8_lossy(&applied.stderr));
        let apply_reply: Value = serde_json::from_slice(&applied.stdout)?;
        assert_eq!(apply_reply["status"], "rejected", "{apply_reply}");
        assert_eq!(apply_reply["reason_code"], "ILLEGAL_TRANSITION", "{apply_reply}");
        assert_eq!(apply_reply["details"]["decision_class"], "public_api", "{apply_reply}");
        assert_eq!(
            apply_reply["details"]["cause"],
            "commander_decision_required",
            "{apply_reply}"
        );
        // 3. Escalated as Commander's need_decision; High wake recorded; nothing dispatched before handling.
        let escalated = events(dir.path(), "need_decision")?;
        assert_eq!(escalated.len(), 1, "{escalated:?}");
        assert_eq!(escalated[0]["decision_class"], "public_api");
        assert_eq!(escalated[0]["statement"], "改公开接口的签名");
        let need_seq = sql(
            dir.path(),
            "SELECT seq FROM events WHERE kind='need_decision' AND run_id='R'",
            true,
        )?[0]["seq"]
            .as_i64()
            .ok_or("need_decision seq")?;
        let now = SystemTime::now() + Duration::from_secs(20);
        let supervised = host
            .store()
            .supervise(
                "R",
                Context {
                    now: Some(now),
                    ..Default::default()
                },
            )
            .await?;
        assert!(
            supervised
                .fired
                .iter()
                .any(|f| f.event_seqs.contains(&need_seq) && f.severity == hoh_core::supervise::Severity::High),
            "need_decision must fire at High priority: {:?}",
            supervised.fired
        );
        let woken = sql(
            dir.path(),
            &format!(
                "SELECT json_extract(payload_json,'$.severity') AS severity FROM events \
                 WHERE kind='supervisor.woken' AND run_id='R' \
                 AND EXISTS (SELECT 1 FROM json_each(payload_json,'$.event_seqs') WHERE value={need_seq})"
            ),
            true,
        )?;
        assert_eq!(woken, json!([{"severity": "high"}]), "{woken}");
        // Run an explicit scheduling round before Commander handles the escalation: nothing is dispatched.
        let scheduled = host
            .store()
            .schedule_round(
                "R",
                Context {
                    required_capabilities: [
                        ("developer".into(), Default::default()),
                        ("researcher".into(), Default::default()),
                    ]
                    .into(),
                    ..Default::default()
                },
            )
            .await?;
        assert!(
            scheduled.dispatched.is_none(),
            "no task may be dispatched for the unapproved conclusion: {scheduled:?}"
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT count(*) n FROM events WHERE kind='schedule.round' AND seq>{need_seq}"),
                true,
            )?[0]["n"],
            0
        );
        let task_ids = sql(
            dir.path(),
            "SELECT task_id FROM tasks WHERE role<>'commander' ORDER BY task_id",
            true,
        )?;
        let mut expected_tasks = vec!["D".to_owned(), child_task];
        expected_tasks.sort();
        let actual_tasks: Vec<String> = task_ids
            .as_array()
            .ok_or("tasks")?
            .iter()
            .filter_map(|r| r["task_id"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(actual_tasks, expected_tasks);
        assert_eq!(sql(dir.path(), "SELECT count(*) n FROM decisions", true)?[0]["n"], 0);
        use std::io::Write as _;
        release.write_all(b"done\n")?;
        drop(release);
        Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
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

async fn get_tasks(host: &Host, path: &str) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",host.port,host.token).as_bytes()).await?;
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let response = String::from_utf8(bytes)?;
    let (headers, body) = response.split_once("\r\n\r\n").ok_or("HTTP headers")?;
    let status = headers.split_whitespace().nth(1).ok_or("status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}
#[tokio::test]
async fn test_023_01_consult_becomes_child_task() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-consult-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z' WHERE run_id='R'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',600);",
        false,
    )?;
    let request = message(
        "consult",
        json!({"intent":"consult","question":"Which existing protocol can carry this result?","budget":{"wall_seconds":30},"deadline":"2098-01-01T00:00:00Z"}),
    )?;
    let reply = host
        .store()
        .consult_request(request.clone())
        .await?
        .expect("consult must create a child task");
    assert_eq!(reply["status"], "queued");
    let child = reply["task_id"].as_str().ok_or("child id")?;
    assert_eq!(
        host.store().consult_request(request).await?,
        Some(reply.clone())
    );
    let tasks = sql(
        dir.path(),
        &format!("SELECT * FROM tasks WHERE task_id='{child}'"),
        true,
    )?;
    assert_eq!(tasks[0]["role"], "researcher");
    assert_eq!(tasks[0]["writer"], 0);
    assert_eq!(tasks[0]["state"], "READY");
    assert_eq!(tasks[0]["spec_id"], "s1");
    let budgets = sql(
        dir.path(),
        &format!(
            "SELECT kind,limit_value FROM budgets WHERE scope_kind='task' AND scope_id='{child}'"
        ),
        true,
    )?;
    assert_eq!(budgets, json!([{"kind":"wall_seconds","limit_value":30.0}]));
    let consult = events(dir.path(), "task.consult")?;
    assert_eq!(consult.len(), 1);
    assert_eq!(consult[0]["parent_task_id"], "D");
    assert_eq!(consult[0]["deadline"], "2098-01-01T00:00:00Z");
    assert_eq!(
        host.store().reader()?.wait_for("R")?,
        vec![("developer".into(), "researcher".into())]
    );
    let (status, page) = get_tasks(&host, "/v1/runs/R/tasks?limit=1").await?;
    assert_eq!(status, 200, "{page}");
    assert!(page["watermark"].as_i64().is_some_and(|s| s > 0));
    let mut all = page["items"].as_array().ok_or("items")?.clone();
    let mut cursor = page["next_cursor"].clone();
    while let Some(next) = cursor.as_str() {
        let (_, page) =
            get_tasks(&host, &format!("/v1/runs/R/tasks?limit=1&cursor={next}")).await?;
        all.extend(page["items"].as_array().ok_or("items")?.clone());
        cursor = page["next_cursor"].clone();
    }
    let shown = all
        .iter()
        .find(|i| i["task_id"] == child)
        .ok_or("child on task list")?;
    assert_eq!(shown["requirement_ids"], json!([]));
    assert_eq!(shown["depends_on"], json!([]));
    assert_eq!(shown["lease"], Value::Null);
    assert_eq!(shown["revision"], tasks[0]["revision"]);
    assert_eq!(shown["parent_task_id"], "D");
    assert_eq!(shown["deadline"], "2098-01-01T00:00:00Z");
    assert_eq!(shown["budget"]["wall_seconds"], 30.0);
    assert_eq!(shown["dependencies"], json!([]));
    assert_eq!(shown["lease_holder"], Value::Null);
    for path in [
        "/v1/runs/missing/tasks",
        "/v1/runs/R/tasks?limit=0",
        "/v1/runs/R/tasks?limit=501",
        "/v1/runs/R/tasks?cursor=invalid",
    ] {
        assert_eq!(get_tasks(&host, path).await?.0, 404);
    }
    host.shutdown().await;
    Ok(())
}
#[tokio::test]
async fn test_023_01_task_list_is_available_without_consultations() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-task-list-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let (status, page) = get_tasks(&host, "/v1/runs/R/tasks").await?;
    assert_eq!(status, 200, "task list endpoint missing: {page}");
    assert_eq!(page["items"][0]["task_id"], "D");
    assert_eq!(page["items"][0]["parent_task_id"], Value::Null);
    host.shutdown().await;
    Ok(())
}
#[tokio::test]
async fn test_023_01_consult_requires_a_bounded_current_request() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-consult-bounds-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let base = json!({"intent":"consult","question":"bounded research","budget":{"wall_seconds":30},"deadline":"2098-01-01T00:00:00Z"});
    for (id, field, value) in [
        ("empty", "question", json!("")),
        ("bad-budget", "budget", json!({"wall_seconds":-1})),
        ("bad-deadline", "deadline", json!("tomorrow")),
    ] {
        let mut payload = base.clone();
        payload[field] = value;
        let response = host
            .store()
            .consult_request(message(id, payload)?)
            .await?
            .expect("malformed consult must be rejected");
        assert_eq!(response["status"], "rejected");
    }
    let mut missing = message("missing", base.clone())?;
    missing.recipient_role = "unbound".into();
    assert_eq!(
        host.store()
            .consult_request(missing)
            .await?
            .expect("unbound request")["status"],
        "rejected"
    );
    assert!(events(dir.path(), "task.consult")?.is_empty());
    sql(
        dir.path(),
        "UPDATE runs SET deadline_at='2099-01-01T00:00:00Z'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',600);",
        false,
    )?;
    let result = host
        .store()
        .consult_request(message(
            "default",
            json!({"intent":"consult","question":"use run bounds"}),
        )?)
        .await?
        .expect("default bounded consult");
    assert_eq!(result["status"], "queued");
    let facts = events(dir.path(), "task.consult")?;
    assert_eq!(facts[0]["budget"]["wall_seconds"], 60.0);
    assert_eq!(facts[0]["deadline"], "2099-01-01T00:00:00Z");
    host.shutdown().await;
    Ok(())
}

#[derive(Debug)]
struct FakeConsult {
    started: tokio::sync::mpsc::UnboundedSender<(hoh_app::host::Work, std::process::ChildStdin)>,
    steers: tokio::sync::mpsc::UnboundedSender<hoh_core::persistence::InputRow>,
    research: std::sync::Mutex<Vec<hoh_app::host::Work>>,
    summary: Option<&'static str>,
}
impl hoh_app::host::Executor for FakeConsult {
    fn deliver(
        &self,
        _: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Ok(()));
        rx
    }
    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let researcher = work.role == "researcher";
        if researcher {
            self.research
                .lock()
                .map_err(|e| e.to_string())?
                .push(work.clone());
        }
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                if researcher {
                    "read -r go; exit 0"
                } else {
                    "read -r go; read -r done; exit 0"
                },
            ])
            .process_group(0)
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let mut stdin = child.stdin.take().ok_or_else(|| "stdin".to_owned())?;
        let held = work.clone();
        let started = self.started.clone();
        let result = json!({"schema_version":"1.0","run_id":work.run_id,"task_id":work.task_id,"attempt_id":work.attempt_id,"role":work.role,"role_epoch":work.assignment.role_epoch,"status":"COMPLETED","summary":self.summary.unwrap_or("Use the existing durable input delivery loop"),"candidate_id":null,"evidence":[],"issues":[],"next_action":"Return to parent"});
        let (tx, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = tx.send(hoh_app::host::Outcome {
                success: status.is_ok_and(|s| s.success()),
                detail: "scripted consultation".into(),
                class: hoh_core::retry::FailureClass::Capability,
                group_gone: world::group_exit::group_gone_after_reap(pid),
                observed: hoh_core::persistence::Observed {
                    messages: vec![result.to_string()],
                    ..Default::default()
                },
                unknown_effect: false,
                retry_at: None,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate: hoh_app::host::Gate::new(move || {
                use std::io::Write as _;
                stdin.write_all(b"go\n").map_err(|e| e.to_string())?;
                if !researcher {
                    started.send((held, stdin)).map_err(|e| e.to_string())?;
                }
                Ok(())
            }),
            ended,
            prompt: None,
            ran: None,
        })
    }
    fn steer(
        &self,
        _: &hoh_app::host::Work,
        input: &hoh_core::persistence::InputRow,
    ) -> tokio::sync::oneshot::Receiver<hoh_agents::codex::SteerReceipt> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = self.steers.send(input.clone());
        let _ = tx.send(hoh_agents::codex::SteerReceipt::Delivered {
            turn_id: "consult-parent".into(),
        });
        rx
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_023_01_running_request_and_result_use_existing_delivery_loop() -> R {
    consult_delivery(false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_023_01_dc2_runtime_cancelled_outcome_reaches_parent_delivery_loop() -> R {
    consult_delivery(true).await
}

async fn consult_delivery(runtime_stop: bool) -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-consult-host-")
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
            .ok_or("seed")?,
        false,
    )?;
    sql(
        dir.path(),
        "UPDATE sessions SET capabilities_json='{\"mid_turn_steer\":true}'; UPDATE runs SET deadline_at='2099-01-01T00:00:00Z' WHERE run_id='R'; INSERT INTO budgets(scope_kind,scope_id,kind,limit_value) VALUES ('run','R','wall_seconds',600);",
        false,
    )?;
    let (stx, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (itx, mut inputs) = tokio::sync::mpsc::unbounded_channel();
    let fake = std::sync::Arc::new(FakeConsult {
        started: stx,
        steers: itx,
        research: Default::default(),
        summary: None,
    });
    let clock = std::sync::Arc::new(Clock::default());
    let mut c = config(dir.path());
    c.clock = hoh_app::host::HostClock(clock.clone());
    c.runner = Some(hoh_app::host::RunnerConfig {
        executor: fake.clone(),
        context: Context {
            required_capabilities: [
                ("developer".into(), Default::default()),
                ("researcher".into(), Default::default()),
            ]
            .into(),
            ..Default::default()
        },
    });
    let host = Host::start(c).await?;
    let (work, mut release) = tokio::time::timeout(Duration::from_secs(20), starts.recv())
        .await?
        .ok_or("parent started")?;
    let helpers = work
        .helpers
        .as_ref()
        .ok_or("running developer must have the existing request bridge")?;
    let revision = sql(
        dir.path(),
        "SELECT revision FROM tasks WHERE task_id='D'",
        true,
    )?[0]["revision"]
        .as_u64()
        .ok_or("revision")?;
    let mut request = message(
        "live-consult",
        json!({"intent":"consult","question":"Which existing protocol can carry this result?","budget":{"wall_seconds":30},"deadline":"2098-01-01T00:00:00Z"}),
    )?;
    if runtime_stop {
        // An explicit, non-default bound below the dispatch estimate makes the Runtime stop before
        // starting any researcher. This deterministically exercises the parent delivery loop.
        request
            .payload
            .insert("budget".into(), json!({"attempts":0.5}));
    }
    request.attempt_id = work.attempt_id.clone();
    request.role_epoch = work.assignment.role_epoch as u64;
    request.expected_revision = revision;
    let request_file = dir.path().join("consult-request.json");
    std::fs::write(&request_file, serde_json::to_vec(&request)?)?;
    let ask = helpers.ask.clone();
    let asking = tokio::task::spawn_blocking(move || {
        std::process::Command::new(ask).arg(request_file).output()
    });
    let input = tokio::time::timeout(Duration::from_secs(20), inputs.recv())
        .await?
        .ok_or("consultation result delivered")?;
    assert_eq!(input.target_role.as_deref(), Some("developer"));
    if runtime_stop {
        let asked = asking.await??;
        assert!(
            asked.status.success(),
            "{}",
            String::from_utf8_lossy(&asked.stderr)
        );
        let response: Value = serde_json::from_slice(&asked.stdout)?;
        let child = response["task_id"].as_str().ok_or("child")?;
        wait_sql(
            dir.path(),
            &clock,
            &format!(
                "SELECT state FROM input_queue WHERE input_id='{}'",
                input.input_id
            ),
            |v| v[0]["state"] == "DELIVERED",
        )
        .await?;
        use std::io::Write as _;
        release.write_all(b"done\n")?;
        drop(release);
        wait_sql(
            dir.path(),
            &clock,
            &format!(
                "SELECT state FROM input_queue WHERE input_id='{}'",
                input.input_id
            ),
            |v| v[0]["state"] == "APPLIED",
        )
        .await?;
        host.store().settle_ended_inputs(&host.project_id).await?;
        let tasks = sql(
            dir.path(),
            &format!("SELECT state,blocked_reason FROM tasks WHERE task_id='{child}'"),
            true,
        )?;
        let finished = events(dir.path(), "task.consult_finished")?;
        let receipt = sql(
            dir.path(),
            &format!(
                "SELECT count(*) n FROM receipts WHERE message_id='{}'",
                input.input_id
            ),
            true,
        )?;
        let anchors = host.store().reader()?.check_question_anchors();
        host.shutdown().await;
        assert_eq!(tasks, json!([{"state":"CANCELLED","blocked_reason":null}]));
        anchors?;
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0]["status"], "CANCELLED");
        assert_eq!(finished[0]["reason"], "budget_exhausted");
        let diagnostic = &input.payload["consultation_diagnostic"];
        assert_eq!(diagnostic["status"], "CANCELLED");
        assert_eq!(diagnostic["correlation_id"], "live-consult");
        assert_eq!(diagnostic["detail"], "consult_attempts");
        assert_eq!(diagnostic["origin"], "runtime");
        assert!(
            input.payload.get("source_message").is_none(),
            "no fabricated TaskResult"
        );
        assert_eq!(receipt[0]["n"], 1);
        assert!(fake.research.lock().map_err(|e| e.to_string())?.is_empty());
        return Ok(());
    }
    let returned = &input.payload["source_message"];
    assert_eq!(returned["kind"], "result");
    assert_eq!(returned["correlation_id"], "live-consult");
    assert_eq!(returned["payload"]["status"], "COMPLETED");
    assert_eq!(
        returned["payload"]["summary"],
        "Use the existing durable input delivery loop"
    );
    hoh_core::protocol::decode::<hoh_core::protocol::Message>(returned)?.check_payload()?;
    let agent_text: Value = serde_json::from_str(
        input.payload["text"]
            .as_str()
            .ok_or("agent-facing result")?,
    )?;
    assert_eq!(
        &agent_text, returned,
        "the native adapter and next-attempt prompt receive the complete result envelope"
    );
    let asked = asking.await??;
    assert!(
        asked.status.success(),
        "{}",
        String::from_utf8_lossy(&asked.stderr)
    );
    let response: Value = serde_json::from_slice(&asked.stdout)?;
    assert_eq!(response["status"], "queued");
    let run_check = helpers.run_check.clone();
    let refused = tokio::task::spawn_blocking(move || {
        std::process::Command::new(run_check).arg("unit").output()
    })
    .await??;
    assert_eq!(refused.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&refused.stdout).contains("frozen verification assignment"));
    wait_sql(
        dir.path(),
        &clock,
        &format!(
            "SELECT state FROM input_queue WHERE input_id='{}'",
            input.input_id
        ),
        |v| v[0]["state"] == "DELIVERED",
    )
    .await?;
    use std::io::Write as _;
    release.write_all(b"done\n")?;
    drop(release);
    wait_sql(
        dir.path(),
        &clock,
        &format!(
            "SELECT state FROM input_queue WHERE input_id='{}'",
            input.input_id
        ),
        |v| v[0]["state"] == "APPLIED",
    )
    .await?;
    let research = fake.research.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(research.len(), 1);
    assert!(!research[0].assignment.writer);
    assert!(
        research[0]
            .assignment
            .context
            .as_ref()
            .ok_or("child context")?
            .scope
            .iter()
            .any(|s| s.contains("Which existing protocol"))
    );
    assert_eq!(host.store().reader()?.wait_for("R")?, vec![]);
    assert_eq!(
        sql(
            dir.path(),
            "SELECT count(*) n FROM attempts a LEFT JOIN tasks t USING(task_id) WHERE t.task_id IS NULL",
            true
        )?[0]["n"],
        0
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT state FROM outbox WHERE message_id='{}'",
                input.input_id
            ),
            true
        )?[0]["state"],
        "ACKED"
    );
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT count(*) n FROM receipts WHERE message_id='{}'",
                input.input_id
            ),
            true
        )?[0]["n"],
        1
    );
    host.shutdown().await;
    Ok(())
}

fn child_result(child: &str, attempt: &str, status: &str) -> Value {
    json!({"schema_version":"1.0","run_id":"R","task_id":child,"attempt_id":attempt,"role":"researcher","role_epoch":1,"status":status,"summary":"research outcome","candidate_id":null,"evidence":[],"issues":[],"next_action":"return to parent"})
}
fn seed_child_attempt(root: &Path, child: &str, attempt: &str) -> R {
    sql(root,&format!("UPDATE tasks SET state='RUNNING' WHERE task_id='{child}'; INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES ('{attempt}','{child}','sr','RUNNING',1,1,1);"),false).map(drop)
}
fn child_ending(result: Value) -> hoh_core::persistence::Ending {
    hoh_core::persistence::Ending {
        success: true,
        detail: "scripted research end".into(),
        class: hoh_core::retry::FailureClass::Capability,
        at: SystemTime::now(),
        draw: 0,
        observed: hoh_core::persistence::Observed {
            messages: vec![result.to_string()],
            ..Default::default()
        },
        unknown_effect: false,
    }
}
#[tokio::test]
async fn test_023_01_return_is_atomic_replayable_and_rejects_obsolete_parent() -> R {
    for scenario in [
        "completed",
        "failed",
        "blocked",
        "incomplete",
        "cancelled",
        "stale_parent",
    ] {
        let dir = tempfile::Builder::new().prefix("m3t10-return-").tempdir()?;
        let host = Host::start(config(dir.path())).await?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        let reply=host.store().consult_request(message("return",json!({"intent":"consult","question":"research","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}))?).await?.expect("consult child");
        let child = reply["task_id"].as_str().ok_or("child")?;
        seed_child_attempt(dir.path(), child, "C")?;
        if scenario == "stale_parent" {
            sql(
                dir.path(),
                "UPDATE role_bindings SET role_epoch=2 WHERE role='developer'",
                false,
            )?;
        }
        if scenario == "cancelled" {
            sql(
                dir.path(),
                &format!(
                    "UPDATE attempts SET cancel_requested=1 WHERE attempt_id='C'; UPDATE tasks SET state='CANCELLED' WHERE task_id='{child}';"
                ),
                false,
            )?;
        }
        host.store()
            .runner_finished(
                "C",
                child_ending(child_result(
                    child,
                    "C",
                    match scenario {
                        "failed" => "FAILED",
                        "blocked" => "BLOCKED",
                        "incomplete" => "INCOMPLETE",
                        _ => "COMPLETED",
                    },
                )),
            )
            .await?;
        let rows = sql(
            dir.path(),
            "SELECT payload_json FROM input_queue WHERE target_role='developer'",
            true,
        )?;
        assert_eq!(
            rows.as_array().ok_or("rows")?.len(),
            usize::from(scenario != "stale_parent")
        );
        if scenario != "stale_parent" {
            let payload: Value =
                serde_json::from_str(rows[0]["payload_json"].as_str().ok_or("payload")?)?;
            if scenario == "cancelled" {
                assert!(payload["source_message"].is_null());
                assert_eq!(payload["consultation_diagnostic"]["reason"], "cancelled");
            } else {
                assert_eq!(
                    payload["source_message"]["payload"]["status"],
                    match scenario {
                        "failed" => "FAILED",
                        "blocked" => "BLOCKED",
                        "incomplete" => "INCOMPLETE",
                        _ => "COMPLETED",
                    }
                );
            }
        }
        assert_eq!(host.store().reader()?.wait_for("R")?.len(), 0);
        let ended = events(dir.path(), "task.consult_finished")?;
        assert_eq!(ended.len(), 1);
        assert!(
            !host
                .store()
                .reader()?
                .run_projection("R")?
                .ok_or("run")?
                .5
                .iter()
                .any(|q| q.task_id.as_deref() == Some(child))
        );
        host.store().reader()?.check_question_anchors()?;
        host.shutdown().await;
        let restarted = Host::start(config(dir.path())).await?;
        restarted
            .store()
            .settle_ended_inputs(&restarted.project_id)
            .await?;
        assert_eq!(events(dir.path(), "task.consult_finished")?, ended);
        assert_eq!(
            sql(
                dir.path(),
                "SELECT payload_json FROM input_queue WHERE target_role='developer'",
                true
            )?
            .as_array()
            .ok_or("rows")?
            .len(),
            rows.as_array().ok_or("rows")?.len()
        );
        restarted.store().reader()?.check_question_anchors()?;
        restarted.shutdown().await;
    }
    Ok(())
}
#[tokio::test]
async fn test_023_01_bad_result_uses_bounded_protocol_repairs() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-bad-result-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let reply=host.store().consult_request(message("bad",json!({"intent":"consult","question":"research","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}))?).await?.expect("consult child");
    let child = reply["task_id"].as_str().ok_or("child")?;
    for n in 1..=3 {
        let attempt = format!("bad-{n}");
        seed_child_attempt(dir.path(), child, &attempt)?;
        host.store()
            .runner_finished(&attempt, child_ending(json!({"status":"COMPLETED"})))
            .await?;
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT state FROM tasks WHERE task_id='{child}'"),
                true
            )?[0]["state"],
            if n < 3 { "READY" } else { "CANCELLED" }
        );
        assert_eq!(
            events(dir.path(), "task.consult_finished")?.len(),
            usize::from(n == 3),
            "corrections keep the consult open only while its correction bound allows"
        );
    }
    assert_eq!(host.store().reader()?.wait_for("R")?.len(), 0);
    assert_eq!(
        sql(
            dir.path(),
            "SELECT count(*) n FROM input_queue WHERE target_role='developer'",
            true
        )?[0]["n"],
        1
    );
    host.store().reader()?.check_question_anchors()?;
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_01_reconcile_returns_observed_result_once() -> R {
    use hoh_core::reconcile::EffectState;
    let dir = tempfile::Builder::new()
        .prefix("m3t10-observed-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let reply=host.store().consult_request(message("observed",json!({"intent":"consult","question":"recover the result","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}))?).await?.expect("consult child");
    let child = reply["task_id"].as_str().ok_or("child")?;
    seed_child_attempt(dir.path(), child, "observed-child")?;
    // Record the identity of our own gated child, then observe its natural exit before simulating the crash.
    use std::io::Write as _;
    use std::os::unix::process::CommandExt as _;
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
            "UPDATE attempts SET pid={pid},boot_id='{boot}',process_start_id='{start}' WHERE attempt_id='observed-child'"
        ),
        false,
    )?;
    process.stdin.take().ok_or("stdin")?.write_all(b"go\n")?;
    assert!(process.wait()?.success());
    let key = hoh_core::reconcile::work_key("observed-child");
    sql(
        dir.path(),
        &format!(
            "INSERT INTO effects(effect_key,attempt_id,state) VALUES ('{key}','observed-child','STARTED');"
        ),
        false,
    )?;
    assert!(host.store().runner_effect("observed-child",&key,EffectState::Started,EffectState::Observed,json!({"success":true,"messages":[child_result(child,"observed-child","COMPLETED").to_string()],"messages_truncated":false})).await?);
    assert!(events(dir.path(), "task.consult_finished")?.is_empty());
    host.shutdown().await;
    let restarted = Host::start(config(dir.path())).await?;
    assert_eq!(
        sql(
            dir.path(),
            "SELECT state FROM attempts WHERE attempt_id='observed-child'",
            true
        )?[0]["state"],
        "SETTLED"
    );
    assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
    assert_eq!(
        sql(
            dir.path(),
            "SELECT count(*) n FROM input_queue WHERE target_role='developer'",
            true
        )?[0]["n"],
        1
    );
    let recovered: Value = serde_json::from_str(
        sql(
            dir.path(),
            "SELECT payload_json FROM input_queue WHERE target_role='developer'",
            true,
        )?[0]["payload_json"]
            .as_str()
            .ok_or("recovered input")?,
    )?;
    assert_eq!(recovered["source_message"]["correlation_id"], "observed");
    assert_eq!(recovered["source_message"]["kind"], "result");
    assert_eq!(
        recovered["source_message"]["payload"]["status"],
        "COMPLETED"
    );
    assert_eq!(
        recovered["source_message"]["payload"]["summary"],
        "research outcome"
    );
    assert_eq!(
        recovered["source_message"]["payload"],
        child_result(child, "observed-child", "COMPLETED")
    );
    restarted
        .store()
        .settle_ended_inputs(&restarted.project_id)
        .await?;
    assert_eq!(events(dir.path(), "task.consult_finished")?.len(), 1);
    restarted.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_01_reconcile_routes_a_workers_unanswered_bridge_query() -> R {
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
    std::fs::write(bridge, serde_json::to_string(&request)? + "\n")?;
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
            json!({"success":true,"messages":[serde_json::to_string(&request)?]}),
        )
        .await?;
    host.shutdown().await;
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
    restarted.shutdown().await;
    let again = Host::start(config(dir.path())).await?;
    assert_eq!(events(dir.path(), "task.consult")?.len(), 1);
    again.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_01_unknown_pricing_is_preserved_in_child_budget() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-consult-pricing-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    sql(
        dir.path(),
        "INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known) VALUES ('run','R','usd',10,0)",
        false,
    )?;
    let reply = host
        .store()
        .consult_request(message(
            "pricing",
            json!({"intent":"consult","question":"research","deadline":"2099-01-01T00:00:00Z"}),
        )?)
        .await?
        .expect("bounded child");
    let child = reply["task_id"].as_str().ok_or("child")?;
    assert_eq!(
        sql(
            dir.path(),
            &format!(
                "SELECT price_known FROM budgets WHERE scope_kind='task' AND scope_id='{child}' AND kind='usd'"
            ),
            true
        )?[0]["price_known"],
        0,
        "allocating a limit cannot establish token pricing"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_023_01_queued_result_is_rejected_when_parent_is_cancelled() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t10-result-cancel-")
        .tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    sql(dir.path(), WORLD, false)?;
    sql(dir.path(), PARTICIPANTS, false)?;
    let reply=host.store().consult_request(message("cancel-later",json!({"intent":"consult","question":"research","budget":{"wall_seconds":10},"deadline":"2099-01-01T00:00:00Z"}))?).await?.expect("child");
    let child = reply["task_id"].as_str().ok_or("child")?;
    seed_child_attempt(dir.path(), child, "C")?;
    host.store()
        .runner_finished("C", child_ending(child_result(child, "C", "COMPLETED")))
        .await?;
    sql(
        dir.path(),
        "UPDATE tasks SET state='CANCELLED' WHERE task_id='D'",
        false,
    )?;
    host.store().settle_ended_inputs(&host.project_id).await?;
    assert_eq!(
        sql(
            dir.path(),
            "SELECT state FROM input_queue WHERE target_role='developer'",
            true
        )?[0]["state"],
        "REJECTED",
        "a queued answer cannot survive its parent cancellation"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_040_04_commander_replans_or_researches_without_automatic_continue() -> R {
    for response in ["replan", "research"] {
        let dir = tempfile::Builder::new().prefix("m3t10-replan-").tempdir()?;
        let clock = std::sync::Arc::new(Clock::default());
        let plain = Host::start(config(dir.path())).await?;
        sql(dir.path(), WORLD, false)?;
        sql(dir.path(), CURRENT_SPEC_REVIEW, false)?;
        sql(dir.path(), PARTICIPANTS, false)?;
        sql(
            dir.path(),
            "INSERT INTO requirements(requirement_id,spec_id,title,required_release) VALUES ('REQ-040','s1','Diagnose no progress',1);
             INSERT INTO acceptance_criteria(ac_id,requirement_id,spec_id,body,check_id) VALUES ('AC-040-04','REQ-040','s1','A diagnosis imports one replacement or research task','integration');
             INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES ('D','REQ-040','s1');
             INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES (:p,'initial-plan','R','plan.imported','{\"tasks\":[{\"task_id\":\"D\"}]}','2026-11-22T00:00:00Z');
             INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES ('repeat','R','s1','IMPLEMENTED','developer',2)",
            false,
        )?;
        // D is an existing planned dependency with observable acceptance criteria (ADR-035, ADR-039).
        let initial_plans = events(dir.path(), "plan.imported")?.len();
        assert_eq!(initial_plans, 1);
        for n in 1..=3 {
            round(dir.path(), n)?;
            plain
                .store()
                .reconcile_verification("R", Default::default())
                .await?;
        }
        assert_eq!(events(dir.path(), "no_progress")?.len(), 1);
        plain.shutdown().await;
        let fake = std::sync::Arc::new(FakeCommander {
            plan: Some(response),
            ..Default::default()
        });
        let mut c = config(dir.path());
        c.clock = hoh_app::host::HostClock(clock.clone());
        c.runner = Some(hoh_app::host::RunnerConfig {
            executor: fake.clone(),
            context: Context::default(),
        });
        let host = Host::start(c).await?;
        let expected = if response == "replan" { 2 } else { 1 };
        if let Err(e) = wait_sql(
            dir.path(),
            &clock,
            "SELECT count(*) n FROM decisions WHERE state='APPLIED'",
            |v| v[0]["n"] == expected,
        )
        .await
        {
            return Err(format!(
                "{e}; decisions {}; events {}",
                sql(dir.path(), "SELECT * FROM decisions", true)?,
                sql(
                    dir.path(),
                    "SELECT kind,payload_json FROM events ORDER BY seq DESC LIMIT 12",
                    true
                )?
            )
            .into());
        }
        wait_sql(
            dir.path(),
            &clock,
            "SELECT a.state FROM attempts a JOIN tasks t USING(task_id) WHERE t.role='commander'",
            |v| v[0]["state"] == "SETTLED",
        )
        .await?;
        let summaries = fake.summaries.lock().map_err(|e| e.to_string())?.clone();
        assert_eq!(summaries.len(), 1);
        let diagnosis = &summaries[0]["events"]
            .as_array()
            .ok_or("wake events")?
            .iter()
            .find(|e| e["kind"] == "no_progress")
            .ok_or("diagnosis in wake")?["payload"];
        assert_eq!(diagnosis["fingerprints"], json!(["F"]));
        assert_eq!(diagnosis["last_progress_seq"], 0);
        assert_eq!(diagnosis["suggestions"], json!(["升级", "研究", "改计划"]));
        let id = if response == "replan" {
            "replacement"
        } else {
            "investigation"
        };
        let task = sql(
            dir.path(),
            &format!("SELECT role,state FROM tasks WHERE task_id='{id}'"),
            true,
        )?;
        assert_eq!(
            task[0]["role"],
            if response == "replan" {
                "developer"
            } else {
                "researcher"
            }
        );
        assert_eq!(
            task[0]["state"], "PENDING",
            "ordinary dependency scheduling is retained"
        );
        if response == "replan" {
            assert_eq!(
                sql(
                    dir.path(),
                    "SELECT state FROM tasks WHERE task_id='repeat'",
                    true
                )?[0]["state"],
                "CANCELLED"
            );
        }
        assert_eq!(
            events(dir.path(), "plan.imported")?.len(),
            initial_plans + 1
        );
        assert_eq!(
            sql(
                dir.path(),
                &format!("SELECT requirement_id FROM task_requirements WHERE task_id='{id}'"),
                true,
            )?,
            json!([{"requirement_id":"REQ-040"}])
        );
        // Work D is still in flight and each new task waits for it: no new stall or work failure can request a different turn.
        for _ in 0..3 {
            clock.advance();
            host.store()
                .supervise(
                    "R",
                    Context {
                        now: Some(hoh_core::delivery::Clock::now(clock.as_ref())),
                        ..Default::default()
                    },
                )
                .await?;
            assert!(
                host.store()
                    .allocate_supervision(
                        &host.project_id,
                        Context {
                            now: Some(hoh_core::delivery::Clock::now(clock.as_ref())),
                            ..Default::default()
                        }
                    )
                    .await?
                    .is_none()
            );
        }
        assert_eq!(
            sql(
                dir.path(),
                "SELECT count(*) n FROM attempts a JOIN tasks t USING(task_id) WHERE t.role='commander'",
                true
            )?[0]["n"],
            1
        );
        assert_eq!(fake.summaries.lock().map_err(|e| e.to_string())?.len(), 1);
        host.shutdown().await;
    }
    Ok(())
}
