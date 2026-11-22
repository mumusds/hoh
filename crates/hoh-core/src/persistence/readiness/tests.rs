//! The readiness step and reads against a real database (REQ-031 behaviour 5 to 7, docs/adr/ADR-035.md decisions 1, 4
//! and 8). The approved revision, the plan and the specification review report are SQL fixtures here; the product
//! paths that write them are covered by `crates/hoh-app/tests/readiness.rs` and `spec_review.rs`.

use super::*;
use crate::persistence::completion::tests::{R, texts};
use crate::persistence::{Options, Store, selftest};
use std::path::{Path, PathBuf};

/// Project p1, revision 1 (s1): REQ-A with AC-A-01, REQ-B1 required without a criterion, REQ-D degradable without
/// one. Run R RUNNING on s1 with planned tasks TA (REQ-A) and TB (REQ-B1), both READY, and TC (REQ-A) PENDING on TA.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-22T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%.64c', '1'), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-22T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release, release_class) VALUES
  ('REQ-A', 's1', 'a', 1, 'required'), ('REQ-B1', 's1', 'b', 1, 'required'), ('REQ-D', 's1', 'd', 1, 'degradable');
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-A-01', 'REQ-A', 's1', json_object('id', 'AC-A-01', 'given', 'g', 'when', 'w', 'then', 't', 'test_id', 'TEST-A-01'), 'unit');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('TA', 'R', 's1', 'READY', 0, 'developer', 1, 1), ('TB', 'R', 's1', 'READY', 0, 'developer', 1, 2),
  ('TC', 'R', 's1', 'PENDING', 0, 'developer', 1, 3);
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('TC', 'TA');
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES
  ('TA', 'REQ-A', 's1'), ('TB', 'REQ-B1', 's1'), ('TC', 'REQ-A', 's1');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'ev-plan', 'R', 'plan.imported', json_object('run_id', 'R', 'tasks', json_array(
     json_object('task_id', 'TA', 'paths', json_array('src/a')), json_object('task_id', 'TB', 'paths', json_array('src/b')),
     json_object('task_id', 'TC', 'paths', json_array()))), '2026-11-22T00:00:00Z');
";

/// A CURRENT PASS report of revision `spec` by HoH session `session` (not in `sessions`: no other identity).
fn pass(path: &Path, spec: &str, session: &str) -> R {
    rusqlite::Connection::open(path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
         ('p1', 'ev-pass-' || ?1, 'R', 'spec_review.reported', json_object('review_id', 'rv-' || ?1, 'spec_id', ?1, \
          'spec_revision', 1, 'status', 'PASS', 'state', 'CURRENT', 'reasons', json_array(), 'session_id', ?2, \
          'findings', json_array()), '2026-11-22T00:00:00Z')",
        rusqlite::params![spec, session],
    )?;
    Ok(())
}

fn world(dir: &Path) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    };
    let store = Store::open(&path, options)?;
    rusqlite::Connection::open(&path)?.execute_batch(WORLD)?;
    Ok((store, path))
}

fn states(path: &Path) -> R<Vec<String>> {
    texts(
        path,
        "SELECT task_id || ' ' || state || ' ' || coalesce(blocked_reason, '-') FROM tasks ORDER BY task_id",
    )
}

/// TEST-031-02's store half: only the task of the requirement without a criterion is BLOCKED(READINESS_NOT_MET), with
/// its reasons and path scope; the run is untouched; a second step writes nothing; once the revision's REQ-B1 has a
/// criterion, the task goes back to READY with task.unblocked.
#[tokio::test]
async fn test_031_02_settle_blocks_only_the_unready_task_and_releases_it() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    pass(&path, "s1", "sv")?;
    assert!(store.settle_readiness("R").await?);
    assert_eq!(
        states(&path)?,
        ["TA READY -", "TB BLOCKED READINESS_NOT_MET", "TC PENDING -"]
    );
    let blocked: Vec<Value> = texts(
        &path,
        "SELECT payload_json FROM events WHERE kind = 'task.blocked' ORDER BY seq",
    )?
    .iter()
    .map(|t| serde_json::from_str(t))
    .collect::<Result<_, _>>()?;
    assert_eq!(
        blocked,
        [
            json!({"task_id": "TB", "reason": "READINESS_NOT_MET", "spec_revision": 1,
                "details": {"reasons": [{"reason": "missing_ac", "ids": ["REQ-B1"]}], "paths": ["src/b"]}})
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT state || ' ' || blocked_reasons_json FROM runs"
        )?,
        ["RUNNING []"]
    );
    assert!(!store.settle_readiness("R").await?, "nothing more to write");
    rusqlite::Connection::open(&path)?.execute_batch(
        "INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES \
         ('AC-B1-01', 'REQ-B1', 's1', 'then', 'unit');",
    )?;
    assert!(store.settle_readiness("R").await?);
    assert_eq!(states(&path)?, ["TA READY -", "TB READY -", "TC PENDING -"]);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.task_id') || ' ' || json_extract(payload_json, '$.to') \
             FROM events WHERE kind = 'task.unblocked'"
        )?,
        ["TB READY"]
    );
    Ok(())
}

/// Without a usable specification review every planned task is BLOCKED for `spec_review` (decision 8), dependent ones
/// included; the review's PASS releases each to READY or, while it waits for a dependency, PENDING.
#[tokio::test]
async fn a_missing_review_blocks_the_plan_until_its_pass() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    rusqlite::Connection::open(&path)?
        .execute_batch("DELETE FROM task_requirements WHERE task_id = 'TB';")?;
    assert!(store.settle_readiness("R").await?);
    assert_eq!(
        states(&path)?,
        [
            "TA BLOCKED READINESS_NOT_MET",
            "TB BLOCKED READINESS_NOT_MET",
            "TC BLOCKED READINESS_NOT_MET"
        ]
    );
    let reasons = texts(
        &path,
        "SELECT json_extract(payload_json, '$.details.reasons') FROM events WHERE kind = 'task.blocked' \
         AND json_extract(payload_json, '$.task_id') = 'TA'",
    )?;
    assert_eq!(
        serde_json::from_str::<Value>(&reasons[0])?,
        json!([{"reason": "spec_review", "ids": [], "detail": "missing"}])
    );
    pass(&path, "s1", "sv")?;
    assert!(store.settle_readiness("R").await?);
    assert_eq!(states(&path)?, ["TA READY -", "TB READY -", "TC PENDING -"]);
    Ok(())
}

/// REQ-031 behaviour 7 (decision 4): a SPECIFYING run moves to READY with readiness.passed once its readiness passes,
/// not before; runs out of the working states are not looked at.
#[tokio::test]
async fn a_specifying_run_is_ready_only_when_its_readiness_passes() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(
        "UPDATE runs SET state = 'SPECIFYING'; DELETE FROM task_dependencies; DELETE FROM task_requirements; \
         DELETE FROM tasks; DELETE FROM events WHERE kind = 'plan.imported';",
    )?;
    pass(&path, "s1", "sv")?;
    assert!(!store.settle_readiness("R").await?, "REQ-B1 has no AC");
    assert_eq!(texts(&path, "SELECT state FROM runs")?, ["SPECIFYING"]);
    conn.execute_batch(
        "INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES \
         ('AC-B1-01', 'REQ-B1', 's1', 'then', 'unit');",
    )?;
    assert!(store.settle_readiness("R").await?);
    assert_eq!(texts(&path, "SELECT state FROM runs")?, ["READY"]);
    assert_eq!(
        texts(
            &path,
            "SELECT payload_json FROM events WHERE kind = 'readiness.passed'"
        )?
        .iter()
        .map(|t| serde_json::from_str::<Value>(t))
        .collect::<Result<Vec<_>, _>>()?,
        [json!({"run_id": "R", "spec_id": "s1", "spec_revision": 1})]
    );
    assert!(!store.settle_readiness("R").await?);
    conn.execute_batch("UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING';")?;
    assert!(
        !store.settle_readiness("R").await?,
        "a paused run is not looked at"
    );
    Ok(())
}

/// TEST-031-03's store half: a non-blocking open question does not hold readiness and is listed, with its id, text,
/// mark and revision, in the run's projection read.
#[tokio::test]
async fn test_031_03_nonblocking_question_listed_in_the_projection() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    rusqlite::Connection::open(&path)?.execute_batch(
        "INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES \
         ('AC-B1-01', 'REQ-B1', 's1', 'then', 'unit');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
          ('p1', 'ev-q', 'R', 'spec.open_questions', json_object('spec_id', 's1', 'spec_revision', 1, 'questions',
            json_array(json_object('id', 'Q-color', 'text', '颜色', 'blocking', json('false'), 'path', 'docs/spec/a.md',
             'line', 13))), '2026-11-22T00:00:00Z');",
    )?;
    pass(&path, "s1", "sv")?;
    let reader = store.reader()?;
    let conn = rusqlite::Connection::open(&path)?;
    assert_eq!(run_readiness(&conn, "R")?, Some(Readiness::Ready));
    assert!(!store.settle_readiness("R").await?, "nothing to block");
    let projection = reader.run_projection("R")?.ok_or("no run")?;
    assert_eq!(
        projection
            .7
            .iter()
            .map(SpecQuestion::json)
            .collect::<Vec<_>>(),
        [json!({"id": "Q-color", "text": "颜色", "blocking": false, "spec_revision": 1})]
    );
    // The same question marked blocking holds every planned task (undecided).
    conn.execute_batch(
        "UPDATE events SET payload_json = json_set(payload_json, '$.questions[0].blocking', json('true')) \
         WHERE kind = 'spec.open_questions';",
    )?;
    assert!(!run_readiness(&conn, "R")?.is_some_and(|r| r.is_ready()));
    assert!(store.settle_readiness("R").await?);
    assert_eq!(
        states(&path)?,
        [
            "TA BLOCKED READINESS_NOT_MET",
            "TB BLOCKED READINESS_NOT_MET",
            "TC BLOCKED READINESS_NOT_MET"
        ]
    );
    Ok(())
}
