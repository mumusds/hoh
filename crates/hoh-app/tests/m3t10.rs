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
