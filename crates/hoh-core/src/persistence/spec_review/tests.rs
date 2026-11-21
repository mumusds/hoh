//! A specification review's report taken in against a real database (REQ-031 behaviour 4, docs/adr/ADR-035.md):
//! `spec_review.opened` and `spec.drafting` are SQL fixtures until the review is opened by the product (M3-T06 S7).

use super::*;
use crate::context::sha256_hex;
use crate::persistence::completion::tests::{R, open, texts};
use crate::persistence::{PersistError, Store};
use crate::verification::report::ObjectRef;
use rusqlite::Connection;
use std::path::Path;

/// Revision 3 (spec s3) of project p1 reviewed in run R: reviewer task RV with attempts AR1..AR9, each in the
/// session the test names; the Commander bound in claude session sc (native N-cmd); drafting task DR, attempt AD in
/// codex session sdr (native N-draft) that `spec.drafting` names with the revision's digest; reviewer sessions srv
/// (claude, N-rev), srv2 (codex), srv3 (claude); and task OT, of another review.
const REVIEW: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s3', 'p1', printf('%.64c', '3'), 3);
INSERT INTO sessions(session_id, agent, role, native_id, agent_version, capabilities_json) VALUES
  ('sc', 'claude', 'commander', 'N-cmd', '1', '{}'), ('sdr', 'codex', 'developer', 'N-draft', '1', '{}'),
  ('srv', 'claude', 'reviewer', 'N-rev', '1', '{}'), ('srv2', 'codex', 'reviewer', NULL, '1', '{}'),
  ('srv3', 'claude', 'reviewer', NULL, '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'claude', 'sc', 1, 1, '2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('RV', 'R', 's3', 'RUNNING', 1, 'reviewer', 0, 20), ('DR', 'R', 's3', 'ACCEPTED', 1, 'developer', 1, 21),
  ('OT', 'R', 's3', 'RUNNING', 1, 'reviewer', 0, 22);
INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision) VALUES
  ('AD', 'DR', 'sdr', 'SETTLED', 1, 1, 1), ('AO', 'OT', 'srv', 'RUNNING', 1, 1, 1);
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'ev-opened', 'R', 'spec_review.opened', json_object('review_id', 'rv-1', 'run_id', 'R', 'spec_id', 's3',
     'spec_revision', 3, 'spec_sha256', printf('%.64c', '3'), 'git_commit', printf('%.40c', 'c'), 'task_id', 'RV'),
   '2026-11-22T00:00:00Z'),
  ('p1', 'ev-drafting', 'R', 'spec.drafting', json_object('task_id', 'DR', 'spec_sha256', printf('%.64c', '3')),
   '2026-11-22T00:00:00Z');
";

const RAW: &[u8] = b"{\"type\":\"result\"}\n";

fn opened() -> Binding {
    Binding {
        review_id: "rv-1".to_owned(),
        attempt_id: String::new(),
        spec_revision: 3,
        spec_sha256: "3".repeat(64),
        git_commit: "c".repeat(40),
    }
}

/// The store with the review world, and its database.
fn world(dir: &Path) -> R<(Store, std::path::PathBuf)> {
    let (store, path) = open(dir)?;
    Connection::open(&path)?.execute_batch(REVIEW)?;
    Ok((store, path))
}

/// Attempt `attempt` of task `task` in session `session`.
fn attempt(path: &Path, attempt: &str, task: &str, session: &str) -> R {
    Connection::open(path)?.execute(
        "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision) \
         VALUES (?1, ?2, ?3, 'RUNNING', 1, 1, 1)",
        rusqlite::params![attempt, task, session],
    )?;
    Ok(())
}

fn kept(value: &Value, kind: &str) -> KeptObject {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let sha256 = sha256_hex(&bytes);
    KeptObject {
        artifact: ArtifactRow {
            path: crate::objects::relative_path(&sha256),
            sha256,
            bytes: bytes.len() as u64,
            mime: "application/json".to_owned(),
            kind: kind.to_owned(),
            name: format!("{kind}.json"),
        },
        bytes,
    }
}

fn raw() -> ArtifactRow {
    ArtifactRow {
        path: ".hoh/runs/R/attempts/x/stdout.jsonl".to_owned(),
        sha256: sha256_hex(RAW),
        bytes: RAW.len() as u64,
        mime: "application/jsonl".to_owned(),
        kind: "raw_log".to_owned(),
        name: "stdout.jsonl".to_owned(),
    }
}

/// The intake the host would build for attempt `attempt` of a reviewer in HoH session `session` whose native session
/// ids are `natives`: its record bound to `recorded` (`opened()` with the attempt, unless a test tampers with it),
/// and its report naming that record.
fn intake(
    attempt: &str,
    session: &str,
    natives: &[&str],
    recorded: impl FnOnce(&mut Binding),
) -> SpecReviewIntake {
    let mut binding = opened();
    binding.attempt_id = attempt.to_owned();
    let mut sessions = vec![session.to_owned()];
    sessions.extend(natives.iter().map(|n| (*n).to_owned()));
    let mut bound = binding.clone();
    recorded(&mut bound);
    let native = kept(
        &spec_report::native(&bound, &sessions, &sha256_hex(RAW)).unwrap_or_default(),
        "native_attempt",
    );
    let reference = |a: &ArtifactRow| ObjectRef {
        path: a.path.clone(),
        sha256: a.sha256.clone(),
    };
    let body = json!({"status": "PASS", "scope": ["docs/spec"], "findings": [
        {"id": "F-1", "severity": "medium", "summary": "an example would help"}]});
    let report = spec_report::build(
        &body,
        session,
        &binding,
        &reference(&raw()),
        &reference(&native.artifact),
    )
    .unwrap_or_default();
    SpecReviewIntake {
        review_id: "rv-1".to_owned(),
        attempt_id: attempt.to_owned(),
        report: Some(kept(&report, "spec_review_report")),
        native: Some(native),
        raw: raw(),
        raw_sha256: sha256_hex(RAW),
    }
}

fn reported(path: &Path) -> R<Vec<Value>> {
    texts(
        path,
        "SELECT payload_json FROM events WHERE kind = 'spec_review.reported' ORDER BY seq",
    )?
    .iter()
    .map(|p| Ok(serde_json::from_str(p)?))
    .collect()
}

fn count(path: &Path, table: &str) -> R<i64> {
    Ok(Connection::open(path)?
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?)
}

fn codes(reasons: &[String]) -> Vec<&str> {
    reasons
        .iter()
        .map(|r| r.split_once(':').map_or(r.as_str(), |(code, _)| code))
        .collect()
}

#[tokio::test]
async fn intake_a_consistent_pass_is_current_and_reported() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    attempt(&path, "AR1", "RV", "srv")?;
    let given = intake("AR1", "srv", &["N-rev"], |_| {});
    let (reports, candidates) = (
        count(&path, "independent_reports")?,
        count(&path, "candidates")?,
    );
    let taken = store.intake_spec_review(given.clone()).await?;
    assert_eq!(
        taken,
        SpecReviewTaken {
            state: ReportState::Current,
            reasons: Vec::new()
        }
    );
    let reference = |a: &ArtifactRow| json!({"path": a.path, "sha256": a.sha256});
    let (report, native) = (
        given.report.as_ref().ok_or("report")?,
        given.native.as_ref().ok_or("native")?,
    );
    assert_eq!(
        reported(&path)?,
        [json!({
            "review_id": "rv-1", "spec_id": "s3", "spec_revision": 3, "status": "PASS", "state": "CURRENT",
            "reasons": [], "session_id": "srv", "attempt_id": "AR1",
            "report": reference(&report.artifact), "raw_session": reference(&given.raw),
            "native_attempt": reference(&native.artifact), "raw_sha256": sha256_hex(RAW),
        })]
    );
    // The objects are registered; a specification review is no candidate report and opens nothing.
    for a in [&report.artifact, &native.artifact, &given.raw] {
        assert_eq!(
            texts(
                &path,
                &format!(
                    "SELECT path FROM artifacts WHERE sha256 = '{}' AND bytes = {}",
                    a.sha256, a.bytes
                )
            )?,
            std::slice::from_ref(&a.path)
        );
    }
    assert_eq!(count(&path, "independent_reports")?, reports);
    assert_eq!(count(&path, "candidates")?, candidates);
    // Replayed with the same objects: its outcome, nothing written; with others: key reuse.
    assert_eq!(store.intake_spec_review(given).await?, taken);
    assert_eq!(reported(&path)?.len(), 1);
    let other = intake("AR1", "srv", &["N-rev", "N-more"], |_| {});
    assert!(matches!(
        store.intake_spec_review(other).await,
        Err(PersistError::KeyReuse(_))
    ));
    assert_eq!(reported(&path)?.len(), 1);
    Ok(())
}

/// A change of the binding a native attempt record names.
type Tamper = Box<dyn FnOnce(&mut Binding)>;

#[tokio::test]
async fn intake_rejects_each_tampered_record_with_its_reason() -> R {
    // AC-031-04 revision 3 through the same entry: the record's review_id, attempt_id, stdout_sha256, spec_sha256 and
    // git_commit each replaced once (its report naming the replaced record), and one without a record.
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    let cases: [(&str, Tamper, &str); 4] = [
        (
            "AR1",
            Box::new(|b: &mut Binding| b.review_id = "rv-other".to_owned()),
            codes::REVIEW_ID_MISMATCH,
        ),
        (
            "AR2",
            Box::new(|b: &mut Binding| b.attempt_id = "AR-other".to_owned()),
            codes::ATTEMPT_ID_MISMATCH,
        ),
        (
            "AR4",
            Box::new(|b: &mut Binding| b.spec_sha256 = "4".repeat(64)),
            "spec_sha256_mismatch",
        ),
        (
            "AR5",
            Box::new(|b: &mut Binding| b.git_commit = "d".repeat(40)),
            "git_commit_mismatch",
        ),
    ];
    let mut expected = Vec::new();
    for (id, tamper, code) in cases {
        attempt(&path, id, "RV", "srv")?;
        expected.push((
            store
                .intake_spec_review(intake(id, "srv", &[], tamper))
                .await?,
            code,
        ));
    }
    // The record's stdout is another log than the raw session's.
    attempt(&path, "AR3", "RV", "srv")?;
    let mut stdout = intake("AR3", "srv", &[], |_| {});
    let mut binding = opened();
    binding.attempt_id = "AR3".to_owned();
    let other = kept(
        &spec_report::native(&binding, &["srv".to_owned()], &"e".repeat(64)).unwrap_or_default(),
        "native_attempt",
    );
    let mut report: Value = serde_json::from_slice(&stdout.report.as_ref().ok_or("report")?.bytes)?;
    report["native_attempt"] =
        json!({"path": other.artifact.path, "sha256": other.artifact.sha256});
    stdout.report = Some(kept(&report, "spec_review_report"));
    stdout.native = Some(other);
    expected.push((
        store.intake_spec_review(stdout).await?,
        codes::STDOUT_SHA256_MISMATCH,
    ));
    attempt(&path, "AR6", "RV", "srv")?;
    let mut bare = intake("AR6", "srv", &[], |_| {});
    bare.native = None;
    expected.push((
        store.intake_spec_review(bare).await?,
        codes::NATIVE_ATTEMPT_MISSING,
    ));
    for (taken, code) in &expected {
        assert_eq!(taken.state, ReportState::Rejected, "{code}");
        assert_eq!(codes(&taken.reasons), [*code], "{:?}", taken.reasons);
    }
    let events = reported(&path)?;
    assert_eq!(events.len(), 6);
    for (event, (taken, _)) in events.iter().zip(&expected) {
        assert_eq!(event["state"], "REJECTED");
        assert_eq!(event["reasons"], json!(taken.reasons));
    }
    assert_eq!(events[5]["native_attempt"], Value::Null);
    assert_eq!(count(&path, "independent_reports")?, 0);
    Ok(())
}

#[tokio::test]
async fn intake_judges_the_session_and_its_independence() -> R {
    // CR-029: a HoH id or an (agent, native id) of the drafters or the Commander makes the reviewer not independent;
    // the same native id string of another agent does not.
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    for (id, session) in [
        ("A1", "srv"),
        ("A2", "srv2"),
        ("A3", "srv3"),
        ("A4", "srv"),
        ("A5", "srv"),
    ] {
        attempt(&path, id, "RV", session)?;
    }
    let judged = |taken: SpecReviewTaken| {
        codes(&taken.reasons)
            .iter()
            .map(|c| (*c).to_owned())
            .collect::<Vec<_>>()
    };
    // The drafter's own session reports in another's attempt.
    assert_eq!(
        judged(
            store
                .intake_spec_review(intake("A1", "sdr", &[], |_| {}))
                .await?
        ),
        [
            codes::SESSION_MISMATCH,
            codes::SESSION_ROLE_MISMATCH,
            codes::NOT_INDEPENDENT
        ]
    );
    // Another HoH session on the drafter's codex native session.
    assert_eq!(
        judged(
            store
                .intake_spec_review(intake("A2", "srv2", &["N-draft"], |_| {}))
                .await?
        ),
        [codes::NOT_INDEPENDENT]
    );
    // The drafter's native id string under claude is another session.
    let taken = store
        .intake_spec_review(intake("A3", "srv3", &["N-draft"], |_| {}))
        .await?;
    assert_eq!(taken.state, ReportState::Current, "{:?}", taken.reasons);
    // The Commander's HoH session in the record's session_ids.
    let taken = store
        .intake_spec_review(intake("A4", "srv", &["sc"], |_| {}))
        .await?;
    assert_eq!(codes(&taken.reasons), [codes::NOT_INDEPENDENT]);
    assert!(taken.reasons[0].contains("HoH session sc"), "{taken:?}");
    // A session sessions does not know.
    assert_eq!(
        judged(
            store
                .intake_spec_review(intake("A5", "S-ghost", &[], |_| {}))
                .await?
        ),
        [codes::SESSION_UNKNOWN]
    );
    // An attempt of another task than the review's.
    assert_eq!(
        judged(
            store
                .intake_spec_review(intake("AO", "srv", &[], |_| {}))
                .await?
        ),
        [codes::ATTEMPT_NOT_OF_REVIEW]
    );
    Ok(())
}

#[tokio::test]
async fn intake_without_a_report_is_rejected_and_an_unopened_review_is_refused() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    attempt(&path, "AR1", "RV", "srv")?;
    let mut missing = intake("AR1", "srv", &[], |_| {});
    missing.report = None;
    let taken = store.intake_spec_review(missing).await?;
    assert_eq!(codes(&taken.reasons), [codes::REPORT_MISSING]);
    let event = reported(&path)?.pop().ok_or("no event")?;
    assert_eq!(
        (&event["report"], &event["status"], &event["session_id"]),
        (&Value::Null, &Value::Null, &Value::Null)
    );
    attempt(&path, "AR2", "RV", "srv")?;
    let mut unopened = intake("AR2", "srv", &[], |_| {});
    unopened.review_id = "rv-never".to_owned();
    assert!(matches!(
        store.intake_spec_review(unopened).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    let mut forged = intake("AR2", "srv", &[], |_| {});
    if let Some(native) = forged.native.as_mut() {
        native.bytes.push(b' ');
    }
    assert!(matches!(
        store.intake_spec_review(forged).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    assert_eq!(reported(&path)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn intake_takes_the_drafting_roles_binding_and_history_as_drafters() -> R {
    // M3D-01, CR-026 correction A: the drafters are, for each drafting Task's run and role, its current binding, every
    // Attempt of that role in that run and the role's session.native records; another agent's same native id string
    // is no conflict.
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO sessions(session_id, agent, role, native_id, agent_version, capabilities_json) VALUES
           ('sdn', 'codex', 'developer', 'N-now', '1', '{}'), ('sdo', 'codex', 'developer', 'N-old', '1', '{}'),
           ('sdh', 'codex', 'developer', NULL, '1', '{}');
         UPDATE role_bindings SET agent = 'codex', session_id = 'sdn', role_epoch = 2
           WHERE run_id = 'R' AND role = 'developer';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('D2', 'R', 's3', 'ACCEPTED', 1, 'developer', 1, 23);
         INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision) VALUES
           ('AD2', 'D2', 'sdo', 'SETTLED', 1, 1, 1);
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'ev-native-h', 'R', 'session.native', json_object('session_id', 'sdh', 'native_id', 'N-hist'),
            '2026-11-22T00:00:00Z');",
    )?;
    let cases = [
        ("B1", "srv2", "N-now"),
        ("B2", "srv2", "N-old"),
        ("B3", "srv2", "N-hist"),
    ];
    for (id, session, native) in cases {
        attempt(&path, id, "RV", session)?;
        let taken = store
            .intake_spec_review(intake(id, session, &[native], |_| {}))
            .await?;
        assert_eq!(codes(&taken.reasons), [codes::NOT_INDEPENDENT], "{native}");
        assert!(
            taken.reasons[0].contains(&format!("codex native session {native}")),
            "{taken:?}"
        );
    }
    // The same strings under claude are other sessions.
    attempt(&path, "B4", "RV", "srv3")?;
    let taken = store
        .intake_spec_review(intake("B4", "srv3", &["N-now", "N-old", "N-hist"], |_| {}))
        .await?;
    assert_eq!(taken.state, ReportState::Current, "{:?}", taken.reasons);
    Ok(())
}

#[tokio::test]
async fn intake_rejects_objects_other_than_the_report_names() -> R {
    // M3D-02: the report's two FileRefs are the registered artifacts', and the raw artifact's digest is the one
    // recomputed from the turn log; any mismatch is a recorded reason, and a raw artifact whose digest is not its
    // log's is not registered.
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    let report_of = |given: &mut SpecReviewIntake, change: &dyn Fn(&mut Value)| -> R {
        let mut report: Value =
            serde_json::from_slice(&given.report.as_ref().ok_or("report")?.bytes)?;
        change(&mut report);
        given.report = Some(kept(&report, "spec_review_report"));
        Ok(())
    };
    attempt(&path, "C1", "RV", "srv")?;
    let mut paths = intake("C1", "srv", &[], |_| {});
    report_of(&mut paths, &|r| {
        r["raw_session"]["path"] = json!("missing-raw-file");
        r["native_attempt"]["path"] = json!("missing-native-file");
    })?;
    attempt(&path, "C2", "RV", "srv")?;
    let mut native_path = intake("C2", "srv", &[], |_| {});
    native_path.native.as_mut().ok_or("native")?.artifact.path = "other-native-object".to_owned();
    attempt(&path, "C3", "RV", "srv")?;
    let mut raw_digest = intake("C3", "srv", &[], |_| {});
    raw_digest.raw.sha256 = "f".repeat(64);
    let cases = [
        (
            paths,
            vec![
                codes::RAW_SESSION_REF_MISMATCH,
                codes::NATIVE_ATTEMPT_REF_MISMATCH,
            ],
        ),
        (native_path, vec![codes::NATIVE_ATTEMPT_REF_MISMATCH]),
        (
            raw_digest.clone(),
            vec![
                codes::RAW_SESSION_REF_MISMATCH,
                codes::RAW_SESSION_ARTIFACT_MISMATCH,
            ],
        ),
    ];
    for (given, want) in cases {
        let taken = store.intake_spec_review(given).await?;
        assert_eq!(taken.state, ReportState::Rejected, "{want:?}");
        assert_eq!(codes(&taken.reasons), want, "{:?}", taken.reasons);
    }
    let events = reported(&path)?;
    assert_eq!(events.len(), 3);
    for event in &events {
        assert_eq!(event["state"], "REJECTED");
    }
    assert_eq!(
        count(
            &path,
            &format!("artifacts WHERE sha256 = '{}'", raw_digest.raw.sha256)
        )?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn intake_replay_is_bound_to_the_review_and_the_computed_digest() -> R {
    // M3D-03: a replay of the same attempt with another review or another recomputed log digest is key reuse; the
    // identical replay keeps its CURRENT.
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
           SELECT project_id, 'ev-opened-2', run_id, kind,
                  json_set(payload_json, '$.review_id', 'rv-2', '$.task_id', 'OT'), created_at
           FROM events WHERE event_id = 'ev-opened';",
    )?;
    attempt(&path, "D1", "RV", "srv")?;
    let given = intake("D1", "srv", &[], |_| {});
    let first = store.intake_spec_review(given.clone()).await?;
    assert_eq!(first.state, ReportState::Current, "{:?}", first.reasons);
    let mut digest = given.clone();
    digest.raw_sha256 = "f".repeat(64);
    let mut review = given.clone();
    review.review_id = "rv-2".to_owned();
    for changed in [digest, review] {
        assert!(matches!(
            store.intake_spec_review(changed).await,
            Err(PersistError::KeyReuse(_))
        ));
    }
    assert_eq!(store.intake_spec_review(given).await?, first);
    let events = reported(&path)?;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["raw_sha256"], json!(sha256_hex(RAW)));
    Ok(())
}
