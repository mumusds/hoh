//! The baseline test command owned by its run (docs/adr/ADR-039.md decision 8, DC-T07): it starts only in a run state
//! that owns it, its process is recorded before it can have any effect, a recorded process not known to have ended
//! holds the run's pause and cancel and its rerun, and a timed-out run's failed-test set is incomplete.

use crate::persistence::{Finished, Options, PauseStep, Store, selftest};
use serde_json::{Value, json};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Run R in `state`, its baseline with test command `make test` and the user's answer `run` to its question.
fn world(state: &str) -> String {
    format!(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-22T00:00:00Z');
         INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
           ('R', 'p1', '{state}', 'AUTO', 1, '2026-11-22T00:00:00Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'R/baseline', 'R', 'run.baseline', json_object('run_id', 'R', 'dirty', json_array(),
              'untracked', json_array(), 'test_commands', json_array('make test')), '2026-11-22T00:00:00Z'),
           ('p1', 'answered', 'R', 'question.answered', json_object('question_id', 'R/baseline_tests/question',
              'choice_id', 'run'), '2026-11-22T00:00:00Z');"
    )
}

fn open(dir: &std::path::Path, state: &str) -> R<(Store, rusqlite::Connection)> {
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(&world(state))?;
    Ok((store, conn))
}

/// A process record as the host writes it before its gate opens.
fn process(index: usize, nonce: &str) -> Value {
    json!({"index": index, "nonce": nonce, "command": "make test", "pid": 4_000_000, "boot_id": "b",
        "process_start_id": "s", "log": format!(".hoh/baseline/R/{index}-{nonce}.log")})
}

fn count(conn: &rusqlite::Connection, kind: &str) -> Result<i64, rusqlite::Error> {
    conn.query_row("SELECT count(*) FROM events WHERE kind = ?1", [kind], |r| {
        r.get(0)
    })
}

#[tokio::test]
async fn test_028_02_baseline_tests_start_only_in_a_run_state_that_owns_them() -> R {
    // DC-T07 (4): a paused, pausing, cancelling or ended run neither waits for its tests nor records a process.
    for state in [
        "PAUSE_REQUESTED",
        "PAUSED",
        "CANCEL_REQUESTED",
        "CANCELLED",
        "COMPLETED",
        "FAILED",
        "INCOMPLETE",
    ] {
        let dir = tempfile::tempdir()?;
        let (store, conn) = open(dir.path(), state)?;
        assert!(
            store.runner_waiting("p1").await?.baseline_tests.is_empty(),
            "{state}"
        );
        assert_eq!(
            store.baseline_process_started("R", process(0, "n")).await?,
            None,
            "{state}"
        );
        assert_eq!(count(&conn, "run.baseline_process")?, 0, "{state}");
    }
    // An owning state: waiting, its process recorded; while that record is not known to have ended, no rerun waits.
    let dir = tempfile::tempdir()?;
    let (store, conn) = open(dir.path(), "DISCOVERING")?;
    assert_eq!(
        store.runner_waiting("p1").await?.baseline_tests,
        vec![("R".to_owned(), vec!["make test".to_owned()])]
    );
    let id = store
        .baseline_process_started("R", process(0, "n1"))
        .await?
        .ok_or("not recorded")?;
    assert_eq!(count(&conn, "run.baseline_process")?, 1);
    assert!(store.runner_waiting("p1").await?.baseline_tests.is_empty());
    let unsettled = store.reader()?.unsettled_baseline_processes("p1")?;
    assert_eq!(unsettled.len(), 1);
    assert_eq!(
        (unsettled[0].process_id.as_str(), unsettled[0].pid),
        (id.as_str(), Some(4_000_000))
    );
    // Ended (killed by a pause, say): it waits again, to run with a new log.
    assert!(
        store
            .baseline_process_ended("R", &id, json!({"settled_by": "host"}))
            .await?
    );
    assert!(
        !store
            .baseline_process_ended("R", &id, json!({"settled_by": "host"}))
            .await?,
        "once"
    );
    assert!(
        store
            .reader()?
            .unsettled_baseline_processes("p1")?
            .is_empty()
    );
    assert_eq!(store.runner_waiting("p1").await?.baseline_tests.len(), 1);
    // Recorded tests: nothing more is started.
    store
        .record_baseline_tests("R", json!({"commands": []}))
        .await?;
    assert_eq!(
        store
            .baseline_process_started("R", process(0, "n2"))
            .await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn test_028_02_pause_and_cancel_wait_for_a_baseline_process_not_known_ended() -> R {
    // DC-T07 (2): PAUSED and CANCELLED are written only once every recorded baseline process of the run ended.
    for (state, pausing) in [("PAUSE_REQUESTED", true), ("CANCEL_REQUESTED", false)] {
        let dir = tempfile::tempdir()?;
        let (store, conn) = open(dir.path(), "DISCOVERING")?;
        let id = store
            .baseline_process_started("R", process(0, "n"))
            .await?
            .ok_or("not recorded")?;
        conn.execute("UPDATE runs SET state = ?1", [state])?;
        let step = || async {
            if pausing {
                store
                    .pause_step("R", std::time::SystemTime::now())
                    .await
                    .map(|s| format!("{s:?}"))
            } else {
                store
                    .finish_cancel(
                        "R",
                        json!({}),
                        Default::default(),
                        std::time::SystemTime::now(),
                    )
                    .await
                    .map(|f| format!("{f:?}"))
            }
        };
        assert_eq!(step().await?, "Running(1)", "{state}");
        let now: String = conn.query_row("SELECT state FROM runs", [], |r| r.get(0))?;
        assert_eq!(now, state);
        store
            .baseline_process_ended("R", &id, json!({"settled_by": "host"}))
            .await?;
        let done = step().await?;
        let want = if pausing {
            format!("{:?}", PauseStep::Paused)
        } else {
            format!("{:?}", Finished::Cancelled)
        };
        assert_eq!(done, want, "{state}");
    }
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_timed_out_baseline_run_is_no_complete_failed_set() -> R {
    // DC-T07 (5), M3N-053: a command killed past its limit leaves its failed tests incomplete; the regression judgement
    // says so instead of taking the partial list as the baseline's whole failed set.
    let dir = tempfile::tempdir()?;
    let (store, conn) = open(dir.path(), "DISCOVERING")?;
    store
        .record_baseline_tests(
            "R",
            json!({"commands": [{"command": "make test", "exit_code": null, "timed_out": true, "incomplete": true,
                "status": "fail", "failed_tests": ["a"]}]}),
        )
        .await?;
    conn.execute_batch(
        "INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,
           test_plan_sha256,source_path,created_at)
         VALUES('C','R',printf('%.40c','1'),printf('%.40c','1'),printf('%.64c','0'),printf('%.64c','0'),
           printf('%.64c','0'),'fixture','2026-11-22T00:00:00Z');",
    )?;
    let regression = super::regression(&conn, "R")?;
    assert_eq!(
        regression,
        json!({"candidate_id": "C", "baseline_complete": false, "baseline_failed": ["a"], "candidate_failed": [],
            "introduced": null, "not_in_baseline": []})
    );
    Ok(())
}

/// Candidate C of run R, frozen before or after its baseline's tests.
fn candidate(conn: &rusqlite::Connection) -> R {
    conn.execute_batch(
        "INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,
           test_plan_sha256,source_path,created_at)
         VALUES('C','R',printf('%.40c','1'),printf('%.40c','1'),printf('%.64c','0'),printf('%.64c','0'),
           printf('%.64c','0'),'fixture','2026-11-22T00:00:00Z');",
    )?;
    Ok(())
}

/// The regression's `(baseline_complete, introduced)` of run R.
fn judged(conn: &rusqlite::Connection) -> R<(Value, Value)> {
    let regression = super::regression(conn, "R")?;
    Ok((
        regression["baseline_complete"].clone(),
        regression["introduced"].clone(),
    ))
}

#[tokio::test]
async fn test_028_02_after_a_run_answer_no_recorded_tests_is_no_complete_baseline() -> R {
    // M3N-102, M3N-150 (docs/adr/ADR-039.md, round 2): the user answered `run` and a candidate exists before the tests
    // are recorded: while none ran yet, while one runs, and once a pause interrupted it, the baseline is incomplete and
    // nothing is said to be introduced; resumed, its recorded tests complete it.
    let dir = tempfile::tempdir()?;
    let (store, conn) = open(dir.path(), "DISCOVERING")?;
    candidate(&conn)?;
    assert_eq!(
        judged(&conn)?,
        (json!(false), Value::Null),
        "before it runs"
    );
    let id = store
        .baseline_process_started("R", process(0, "n1"))
        .await?
        .ok_or("not recorded")?;
    assert_eq!(judged(&conn)?, (json!(false), Value::Null), "while it runs");
    conn.execute("UPDATE runs SET state = 'PAUSE_REQUESTED'", [])?;
    store
        .baseline_process_ended("R", &id, json!({"settled_by": "host"}))
        .await?;
    assert_eq!(
        format!(
            "{:?}",
            store.pause_step("R", std::time::SystemTime::now()).await?
        ),
        format!("{:?}", PauseStep::Paused)
    );
    assert_eq!(
        judged(&conn)?,
        (json!(false), Value::Null),
        "paused, interrupted"
    );
    conn.execute("UPDATE runs SET state = 'DISCOVERING'", [])?;
    let id = store
        .baseline_process_started("R", process(0, "n2"))
        .await?
        .ok_or("not recorded")?;
    store
        .baseline_process_ended("R", &id, json!({"settled_by": "host"}))
        .await?;
    store
        .record_baseline_tests(
            "R",
            json!({"commands": [{"command": "make test", "exit_code": 1, "timed_out": false, "incomplete": false,
                "status": "fail", "failed_tests": ["a"]}]}),
        )
        .await?;
    assert_eq!(
        judged(&conn)?,
        (json!(true), json!([])),
        "resumed and recorded"
    );
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_recorded_run_with_a_live_process_is_no_complete_baseline() -> R {
    let dir = tempfile::tempdir()?;
    let (store, conn) = open(dir.path(), "DISCOVERING")?;
    candidate(&conn)?;
    store
        .baseline_process_started("R", process(0, "n"))
        .await?
        .ok_or("not recorded")?;
    store
        .record_baseline_tests("R", json!({"commands": []}))
        .await?;
    assert_eq!(judged(&conn)?, (json!(false), Value::Null));
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_skip_is_complete_and_a_run_never_asked_stays_as_it_is() -> R {
    // An explicit skip is a complete (empty) baseline.
    let dir = tempfile::tempdir()?;
    let (store, conn) = open(dir.path(), "DISCOVERING")?;
    conn.execute(
        "UPDATE events SET payload_json = json_set(payload_json, '$.choice_id', 'skip') WHERE event_id = 'answered'",
        [],
    )?;
    candidate(&conn)?;
    assert_eq!(
        judged(&conn)?,
        (json!(false), Value::Null),
        "skip not yet recorded"
    );
    store
        .record_baseline_tests("R", json!({"commands": [], "skipped": true}))
        .await?;
    assert_eq!(judged(&conn)?, (json!(true), json!([])), "skipped");
    // Asked and not answered: no baseline yet (the run's state does not matter to the judgement).
    let dir = tempfile::tempdir()?;
    let (_store, conn) = open(dir.path(), "DISCOVERING")?;
    conn.execute_batch(
        "DELETE FROM events WHERE event_id = 'answered';
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'R/baseline_tests/question', 'R', 'question.asked',
            json_object('question_id', 'R/baseline_tests/question', 'reason', 'baseline_tests'),
            '2026-11-22T00:00:00Z');",
    )?;
    candidate(&conn)?;
    assert_eq!(
        judged(&conn)?,
        (json!(false), Value::Null),
        "asked, not answered"
    );
    // Never asked (no tests detected): as it is.
    let dir = tempfile::tempdir()?;
    let (_store, conn) = open(dir.path(), "DISCOVERING")?;
    conn.execute("DELETE FROM events WHERE event_id = 'answered'", [])?;
    candidate(&conn)?;
    assert_eq!(judged(&conn)?, (json!(true), json!([])), "never asked");
    Ok(())
}
