//! A verifier's report taken in against a real store (the integration check): TEST-054-01. The database is seeded and
//! read with Python's sqlite3 module (test code; the product reaches SQLite only in persistence); the reports and their
//! raw session and native attempt objects are kept and checked as the host keeps them. The WAL self-test's probe is
//! this binary running `wal_probe_helper`.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::persistence::selftest::{self, ProbeCommand};
use hoh_core::persistence::{
    ArtifactRow, Ending, Freeze, IntakeState, Options, ReportIntake, ReportState, SelfTests, Store,
    WriterCompleted, WriterCompletion, WriterOutcome,
};
use hoh_core::verification::classify::Report;
use hoh_core::verification::{FileRef, Manifest, report};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The WAL self-test's second process: this binary, only this function.
#[test]
#[ignore = "run by the WAL self-test as its probe"]
fn wal_probe_helper() {
    if std::env::var_os(selftest::PROBE_DB_ENV).is_some() {
        std::process::exit(selftest::probe_main());
    }
}

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: std::env::current_exe().unwrap_or_default(),
        args: [
            "wal_probe_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(OsString::from)
        .to_vec(),
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

/// The store of the project at `dir` (its database `.hoh/state.db`).
fn open(dir: &Path) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(probe()),
            ..Options::default()
        },
    )?;
    Ok((store, path))
}

/// Run `script` (Python, sqlite3) on `db` with `stdin`; its stdout.
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn seed(db: &Path, sql: &str) -> R {
    python(
        db,
        "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],timeout=30)\nc.execute('PRAGMA foreign_keys=ON')\nc.executescript(sys.stdin.read())\nc.commit()",
        sql,
    )?;
    Ok(())
}

/// Every row of `sql` as an object of its columns.
fn rows(db: &Path, sql: &str) -> R<Vec<Value>> {
    let out = python(
        db,
        "import sqlite3,sys,json\nc=sqlite3.connect(sys.argv[1],timeout=30)\nc.row_factory=sqlite3.Row\nprint(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?;
    Ok(serde_json::from_str(&out)?)
}

/// The evidence a check keeps (HOH_EVIDENCE_DIR, set by run_check).
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

/// The run specification's digest: the freeze's spec object (object 1).
fn spec_sha256() -> String {
    "1".repeat(64)
}

/// Run R on specification s1, developer task D RUNNING with attempt A (in S_dev) holding the write lease, its
/// requirement REQ-1 with one criterion; sessions S_cmd (codex, the Commander's binding), S_dev (claude, the
/// developer's binding and D's attempt), S_qa (pi, the qa binding) and S_rev (codex, the reviewer's binding).
fn world() -> String {
    let spec = spec_sha256();
    format!(
        "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-12T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', '{spec}', 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-12T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-1', 's1', 'r', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES ('AC-1-01', 'REQ-1', 's1', 'b', 'unit');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('S_cmd', 'codex', 'commander', '1', '{{}}'), ('S_dev', 'claude', 'developer', '1', '{{}}'),
  ('S_qa', 'pi', 'qa', '1', '{{}}'), ('S_rev', 'codex', 'reviewer', '1', '{{}}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'S_cmd', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'developer', 'claude', 'S_dev', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'qa', 'pi', 'S_qa', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'reviewer', 'codex', 'S_rev', 1, 1, '2026-11-12T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('D', 'R', 's1', 'RUNNING', 4, 'developer', 1, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-1', 's1');
INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
  ('A', 'D', 'S_dev', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:01Z');
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
"
    )
}

/// Object `n` of the freeze (its sha256 is `n` in hex, repeated).
fn object(n: u8, kind: &str, name: &str) -> ArtifactRow {
    let sha256: String = format!("{n:x}").repeat(64).chars().take(64).collect();
    ArtifactRow {
        path: format!(".hoh/objects/sha256/{}/{sha256}", &sha256[..2]),
        sha256,
        bytes: 10 + u64::from(n),
        mime: "application/json".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

/// D's completion: its COMPLETED TaskResult and the freeze of candidate C (spec, environment and test plan kept as
/// objects 1, 2 and 3, the export as 9).
fn completion() -> R<(String, WriterCompletion)> {
    let file = |path: &str, n: u8| FileRef {
        path: path.to_owned(),
        sha256: object(n, "", "").sha256,
    };
    let sealed = Manifest {
        git_commit: "a".repeat(40),
        git_tree: "b".repeat(40),
        spec: file("contracts/requirements.json", 1),
        environment: file(".hoh/environment.json", 2),
        test_plan: file("tests/check-plan.json", 3),
        source_files: vec![file("src/lib.rs", 4)],
    }
    .seal();
    let candidate = sealed.candidate_id.clone();
    let job = Freeze {
        run_id: "R".to_owned(),
        sealed,
        export: object(9, "candidate", "candidate.json"),
        inputs: vec![
            object(1, "spec", "contracts/requirements.json"),
            object(2, "environment", ".hoh/environment.json"),
            object(3, "test_plan", "tests/check-plan.json"),
        ],
    };
    let result = serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "D", "attempt_id": "A", "role": "developer",
        "role_epoch": 1, "status": "COMPLETED", "summary": "done", "candidate_id": null, "evidence": [],
        "issues": [], "next_action": "verify"
    }))?;
    let ending = Ending {
        success: true,
        detail: "turn".to_owned(),
        class: hoh_core::retry::FailureClass::Transient { retry_after: None },
        at: std::time::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        draw: 0,
        observed: Default::default(),
        unknown_effect: false,
    };
    Ok((
        candidate,
        WriterCompletion {
            attempt_id: "A".to_owned(),
            ending,
            outcome: WriterOutcome::Freeze {
                job: Box::new(job),
                result: Box::new(result),
                self_tests: SelfTests::default(),
            },
        },
    ))
}

/// `bytes` kept as an object of the project at `project`, registered as `kind` named `name`.
fn kept(project: &Path, bytes: &[u8], kind: &str, name: &str) -> R<ArtifactRow> {
    let sha256 = hoh_core::objects::put(project, bytes)?;
    Ok(ArtifactRow {
        path: hoh_core::objects::relative_path(&sha256),
        sha256,
        bytes: u64::try_from(bytes.len())?,
        mime: "application/json".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    })
}

/// A reviewer PASS report of attempt `attempt` in session `session` on `candidate`, made as the host makes one: its raw
/// session log, its native attempt export (COMPLETED, its sessions [`session`]) and the report under the locked schema,
/// each kept as an object of the project at `project`, then checked from their bytes. Its intake and the report's text.
fn reviewer_report(
    project: &Path,
    attempt: &str,
    session: &str,
    candidate: &str,
) -> R<(ReportIntake, String)> {
    let block = json!({"status": "PASS", "scope": ["AC-1-01"], "findings": []});
    let raw = format!(
        "{}\n",
        json!({"type": "result", "session_id": session, "result": format!("done\n```hoh-report\n{block}\n```\n")})
    );
    let mut raw_session = kept(project, raw.as_bytes(), "raw_log", "stdout.jsonl")?;
    raw_session.mime = "application/x-ndjson".to_owned();
    let native = serde_json::to_vec(&report::native_attempt(
        "reviewer",
        "COMPLETED",
        &[session.to_owned()],
        candidate,
        &raw_session.sha256,
    )?)?;
    let native_attempt = kept(project, &native, "native_attempt", "native-attempt.json")?;
    let reference = |a: &ArtifactRow| report::ObjectRef {
        path: a.path.clone(),
        sha256: a.sha256.clone(),
    };
    let built = report::build(
        &block,
        "reviewer",
        session,
        (candidate, &spec_sha256()),
        &reference(&raw_session),
        &reference(&native_attempt),
    )?;
    let text = serde_json::to_string(&built)?;
    let report_artifact = kept(
        project,
        text.as_bytes(),
        "independent_report",
        "independent-report.json",
    )?;
    let checked = report::verify(text.as_bytes(), &raw_session.sha256, &native)?;
    Ok((
        ReportIntake {
            attempt_id: attempt.to_owned(),
            report: checked.report,
            report_artifact,
            raw_artifact: raw_session,
            native_artifact: native_attempt,
            native_session_ids: checked.session_ids,
            native_status: checked.status,
        },
        text,
    ))
}

/// Run R's review status on its latest candidate, as the snapshot shows it: the candidate and the status.
fn review(store: &Store) -> R<(Option<String>, Report)> {
    let snapshot = store.reader()?.acceptance("R")?.ok_or("no run R")?;
    Ok((snapshot.candidate.map(|c| c.candidate_id), snapshot.review))
}

#[tokio::test]
async fn test_054_01_reports_in_conflicting_sessions_are_rejected() -> R {
    // AC-054-01 (REQ-054 behaviour 3, failure mode 1; docs/adr/ADR-029.md decisions 7 and 7a): S_cmd (codex, the
    // Commander's binding), S_dev (claude, the source task's developer attempt), S_qa (pi, the qa binding) and S_rev
    // (codex, the reviewer's binding). D's completion freezes candidate C and opens its round; the round's reviewer task
    // has an attempt in each session, and each submits a reviewer PASS report on C under the locked schema, in the
    // order S_cmd, S_dev, S_qa, S_rev. The first three are REJECTED for a session conflict, each with its
    // report.rejected, and not counted (C's review stays NOT_RUN); only S_rev's is accepted (CURRENT, review PASS);
    // the sessions table has S_rev a reviewer's, not one of the three.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(dir.path())?;
    seed(&db, &world())?;
    let (candidate, done) = completion()?;
    let completed = store.complete_writer_attempt(done).await?;
    assert!(
        matches!(&completed, WriterCompleted::Verifying { candidate_id, .. } if *candidate_id == candidate),
        "{completed:?}"
    );
    let opened = rows(
        &db,
        "SELECT json_extract(payload_json, '$.candidate_id') AS candidate, \
         json_extract(payload_json, '$.tasks.reviewer') AS reviewer FROM events \
         WHERE kind = 'verification.round_opened'",
    )?;
    assert_eq!(opened.len(), 1, "{opened:?}");
    assert_eq!(opened[0]["candidate"], json!(candidate));
    let reviewer = opened[0]["reviewer"]
        .as_str()
        .ok_or("no reviewer task")?
        .to_owned();
    let mut submitted: Vec<(&str, IntakeState, Value)> = Vec::new();
    let mut before_rev = None;
    for session in ["S_cmd", "S_dev", "S_qa", "S_rev"] {
        if session == "S_rev" {
            before_rev = Some(review(&store)?);
        }
        let attempt = hoh_core::ids::child_id(&reviewer, &format!("attempt-{session}"));
        seed(
            &db,
            &format!(
                "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, \
                 config_revision) VALUES ('{attempt}', '{reviewer}', '{session}', 'SETTLED', 1, 1, 1);"
            ),
        )?;
        let (intake, text) = reviewer_report(dir.path(), &attempt, session, &candidate)?;
        let object = json!({"path": intake.report_artifact.path, "sha256": intake.report_artifact.sha256,
            "text": text});
        let state = store.record_report(intake).await?;
        submitted.push((session, state, object));
    }
    let after_rev = review(&store)?;
    let reports = rows(
        &db,
        "SELECT report_id, run_id, candidate_id, role, session_id, spec_sha256, status, state, reject_reason, \
         attempt_id, report_artifact_id, raw_session_artifact_id FROM independent_reports ORDER BY rowid",
    )?;
    let sessions = rows(
        &db,
        "SELECT session_id, agent, role FROM sessions ORDER BY session_id",
    )?;
    let rejections: Vec<Value> = rows(
        &db,
        "SELECT payload_json FROM events WHERE kind = 'report.rejected' ORDER BY seq",
    )?
    .iter()
    .map(|r| serde_json::from_str(r["payload_json"].as_str().unwrap_or("null")))
    .collect::<Result<_, _>>()?;
    let observed = json!({
        "candidate_id": candidate,
        "submitted": submitted.iter().map(|(session, state, object)| json!({"session_id": session,
            "state": state.state.as_str(), "report_id": state.report_id, "reasons": state.reasons, "report": object}))
            .collect::<Vec<_>>(),
        "independent_reports": reports, "sessions": sessions, "report.rejected": rejections,
        "review_before_s_rev": format!("{before_rev:?}"), "review_after_s_rev": format!("{after_rev:?}"),
    });
    println!("{observed}");
    keep("test-054-01.json", &observed)?;
    for (session, state, _) in &submitted {
        let row = reports
            .iter()
            .find(|r| r["session_id"] == json!(session))
            .ok_or(format!("no row of {session}"))?;
        assert_eq!(row["candidate_id"], json!(candidate), "{session}: {row}");
        if *session == "S_rev" {
            assert_eq!(state.state, ReportState::Current, "{state:?}");
            assert_eq!(row["state"], "CURRENT");
            assert_eq!(row["reject_reason"], Value::Null);
        } else {
            assert_eq!(state.state, ReportState::Rejected, "{session}: {state:?}");
            assert_eq!(row["state"], "REJECTED", "{session}");
            assert!(
                row["reject_reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("session conflict")),
                "{session}: {row}"
            );
            assert_eq!(
                rejections
                    .iter()
                    .filter(|e| e["report_id"] == row["report_id"])
                    .count(),
                1,
                "{session}"
            );
        }
    }
    assert_eq!(
        reports.iter().filter(|r| r["state"] == "REJECTED").count(),
        3
    );
    assert_eq!(rejections.len(), 3);
    let accepted: Vec<&Value> = reports.iter().filter(|r| r["state"] == "CURRENT").collect();
    assert_eq!(accepted.len(), 1, "{reports:?}");
    assert_eq!(accepted[0]["session_id"], "S_rev");
    // Not counted: three REJECTED PASS reports leave C's review NOT_RUN; S_rev's alone makes it PASS.
    assert_eq!(before_rev, Some((Some(candidate.clone()), Report::NotRun)));
    assert_eq!(after_rev, (Some(candidate.clone()), Report::Pass));
    assert_eq!(
        rows(
            &db,
            "SELECT agent, role FROM sessions WHERE session_id = 'S_rev'"
        )?,
        [json!({"agent": "codex", "role": "reviewer"})]
    );
    assert!(
        !["S_cmd", "S_dev", "S_qa"]
            .contains(&accepted[0]["session_id"].as_str().unwrap_or_default())
    );
    assert_eq!(rows(&db, "SELECT issue_id FROM issues")?.len(), 0);
    Ok(())
}
