//! Sessions and attempts through the store: the attempt machine, its events, and the typed reads.

use super::*;
use crate::persistence::{Change, Options, StateCas, Store, selftest};
use std::path::{Path, PathBuf};

type R = Result<(), Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> Result<(Store, PathBuf), PersistError> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

fn seed(path: &Path) -> R {
    Connection::open(path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', '2026-11-04T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('T1', 'R', 's1', 'LEASED', 'developer', 1);",
    )?;
    Ok(())
}

fn session(native: Option<&str>) -> Session {
    Session {
        session_id: "S1".to_owned(),
        agent: "claude".to_owned(),
        role: "developer".to_owned(),
        native_id: native.map(str::to_owned),
        native_path: None,
        agent_version: "2.1.318".to_owned(),
        capabilities: json!({"structured_events": true, "resume": true}),
    }
}

fn attempt() -> NewAttempt {
    NewAttempt {
        attempt_id: "A1".to_owned(),
        task_id: "T1".to_owned(),
        session_id: Some("S1".to_owned()),
        role_epoch: 1,
        host_epoch: 1,
        config_revision: 0,
        agent: Some("claude".to_owned()),
    }
}

fn step(from: AttemptState, to: AttemptState, facts: AttemptFacts) -> AttemptMove {
    AttemptMove {
        attempt_id: "A1".to_owned(),
        from,
        to,
        facts,
    }
}

#[tokio::test]
async fn an_attempt_moves_along_the_table_with_its_facts_and_events() -> R {
    use AttemptState::*;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    store.upsert_session(session(None)).await?;
    store.create_attempt(attempt()).await?;
    let reader = store.reader()?;
    assert_eq!(reader.attempt("A1")?.map(|a| a.state), Some(Prepared));
    store
        .move_attempt(step(
            Prepared,
            Starting,
            AttemptFacts {
                started_at: Some("2026-11-04T00:00:01Z".to_owned()),
                ..AttemptFacts::default()
            },
        ))
        .await?;
    store
        .move_attempt(step(
            Starting,
            Running,
            AttemptFacts {
                pid: Some(4242),
                ..AttemptFacts::default()
            },
        ))
        .await?;
    // A resumed session writes its row again; there is still one row, now with the native id.
    store.upsert_session(session(Some("native-1"))).await?;
    store
        .move_attempt(step(
            Running,
            Settled,
            AttemptFacts {
                ended_at: Some("2026-11-04T00:00:09Z".to_owned()),
                resolved_model: Some("claude-x".to_owned()),
                usage: Some(json!({"input_tokens": 3})),
                cost_usd: Some(0.01),
                raw_log_path: Some("/logs/A1".to_owned()),
                ..AttemptFacts::default()
            },
        ))
        .await?;
    let row = reader.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (
            row.state,
            row.pid,
            row.started_at.as_deref(),
            row.resolved_model.as_deref()
        ),
        (
            Settled,
            Some(4242),
            Some("2026-11-04T00:00:01Z"),
            Some("claude-x")
        )
    );
    assert_eq!(row.usage, Some(json!({"input_tokens": 3})));
    assert_eq!(
        (row.cost_usd, row.raw_log_path.as_deref()),
        (Some(0.01), Some("/logs/A1"))
    );
    let s = reader.session("S1")?.ok_or("no session")?;
    assert_eq!(s.native_id.as_deref(), Some("native-1"));
    let sessions: i64 =
        Connection::open(&path)?.query_row("SELECT count(*) FROM sessions", [], |r| r.get(0))?;
    assert_eq!(sessions, 1);
    let kinds: Vec<(String, Value)> = reader.events("R")?;
    assert_eq!(
        kinds
            .iter()
            .map(|(k, p)| format!("{k}:{}", p["to"].as_str().unwrap_or("PREPARED")))
            .collect::<Vec<_>>(),
        [
            "attempt.created:PREPARED",
            "attempt.moved:STARTING",
            "attempt.moved:RUNNING",
            "attempt.moved:SETTLED"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn moves_outside_the_table_or_from_another_state_write_nothing() -> R {
    use AttemptState::*;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    store.upsert_session(session(None)).await?;
    store.create_attempt(attempt()).await?;
    let events = || store.reader().and_then(|r| r.events("R")).map(|e| e.len());
    // PREPARED never becomes UNKNOWN_EFFECT: nothing external has happened yet.
    match store
        .move_attempt(step(Prepared, UnknownEffect, AttemptFacts::default()))
        .await
    {
        Err(PersistError::IllegalTransition {
            entity: Entity::Attempt,
            ..
        }) => {}
        other => return Err(format!("PREPARED -> UNKNOWN_EFFECT: {other:?}").into()),
    }
    match store
        .move_attempt(step(Running, Settled, AttemptFacts::default()))
        .await
    {
        Err(e @ PersistError::StaleState { .. }) => {
            assert!(e.to_string().contains("PREPARED"), "{e}")
        }
        other => return Err(format!("stale move: {other:?}").into()),
    }
    // A move from another state is stale even when its pair is outside the table too: the caller's view is old.
    match store
        .move_attempt(step(Settled, Starting, AttemptFacts::default()))
        .await
    {
        Err(PersistError::StaleState { actual, .. }) => {
            assert_eq!(actual.as_deref(), Some("PREPARED"))
        }
        other => return Err(format!("stale and illegal move: {other:?}").into()),
    }
    let mut unknown = step(Prepared, Starting, AttemptFacts::default());
    unknown.attempt_id = "nope".to_owned();
    assert!(matches!(
        store.move_attempt(unknown).await,
        Err(PersistError::StaleState { actual: None, .. })
    ));
    // An attempt event is recorded once under its name.
    store
        .attempt_event(
            "A1",
            "diag-1",
            "adapter.diagnostic",
            json!({"line": "not json"}),
        )
        .await?;
    assert!(matches!(
        store
            .attempt_event("A1", "diag-1", "adapter.diagnostic", json!({}))
            .await,
        Err(PersistError::IntegrityViolation(_))
    ));
    assert!(matches!(
        store
            .attempt_event("A9", "diag-1", "adapter.diagnostic", json!({}))
            .await,
        Err(PersistError::IntegrityViolation(_))
    ));
    // A long event name gives a bounded id and leaves the writer working (M1T03R-06).
    store
        .attempt_event("A1", &"n".repeat(400), "adapter.diagnostic", json!({}))
        .await?;
    let before = events()?;
    // A terminal attempt stays terminal; a retry is a new attempt.
    store
        .move_attempt(step(Prepared, Failed, AttemptFacts::default()))
        .await?;
    let after_fail = events()?;
    assert!(matches!(
        store
            .move_attempt(step(Failed, Starting, AttemptFacts::default()))
            .await,
        Err(PersistError::IllegalTransition { .. })
    ));
    assert_eq!(events()?, after_fail);
    assert_eq!(after_fail, before + 1);
    // Attempts have no revision, so the generic compare-and-set refuses them.
    let touch = Commit {
        cas: vec![StateCas {
            id: "A1".to_owned(),
            expected_revision: 0,
            change: Change::Touch(Entity::Attempt),
        }],
        ..Commit::default()
    };
    assert!(matches!(
        store.commit(touch).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    // An attempt of an unknown task is refused whole.
    let mut orphan = attempt();
    orphan.attempt_id = "A2".to_owned();
    orphan.task_id = "T9".to_owned();
    assert!(matches!(
        store.create_attempt(orphan).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    assert_eq!(store.reader()?.attempt("A2")?, None);
    assert_eq!(events()?, after_fail);
    Ok(())
}

#[tokio::test]
async fn a_cancel_and_the_end_are_decided_under_one_latch() -> R {
    use AttemptState::*;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    store.upsert_session(session(None)).await?;
    let to_running = |id: &str| -> [AttemptMove; 2] {
        let mut a = step(Prepared, Starting, AttemptFacts::default());
        let mut b = step(Starting, Running, AttemptFacts::default());
        a.attempt_id = id.to_owned();
        b.attempt_id = id.to_owned();
        [a, b]
    };
    let settle = |id: &str| {
        let mut m = step(
            Running,
            Settled,
            AttemptFacts {
                failure_reason: Some("ignored when cancelled".to_owned()),
                resolved_model: Some("m".to_owned()),
                ..AttemptFacts::default()
            },
        );
        m.attempt_id = id.to_owned();
        m
    };
    // A cancel requested before the end makes the end CANCELLED, keeping its other facts.
    store.create_attempt(attempt()).await?;
    for m in to_running("A1") {
        store.move_attempt(m).await?;
    }
    let latch = CancelLatch::new();
    assert_eq!(latch.request(), CancelReceipt::Accepted);
    assert!(latch.is_requested());
    assert_eq!(store.end_attempt(settle("A1"), &latch).await?, Cancelled);
    let row = store.reader()?.attempt("A1")?.ok_or("no A1")?;
    assert_eq!(
        (
            row.state,
            row.cancel_requested,
            row.failure_reason.as_deref(),
            row.resolved_model.as_deref()
        ),
        (Cancelled, true, None, Some("m"))
    );
    // After the end a cancel is refused.
    assert_eq!(latch.request(), CancelReceipt::AlreadyEnded);
    // Without a cancel the end is the one asked for; a failed end leaves the latch open.
    let mut second = attempt();
    second.attempt_id = "A2".to_owned();
    store.create_attempt(second).await?;
    let latch = CancelLatch::new();
    let mut stale = settle("A2");
    stale.from = Starting;
    assert!(matches!(
        store.end_attempt(stale, &latch).await,
        Err(PersistError::StaleState { .. })
    ));
    for m in to_running("A2") {
        store.move_attempt(m).await?;
    }
    assert_eq!(store.end_attempt(settle("A2"), &latch).await?, Settled);
    assert_eq!(latch.request(), CancelReceipt::AlreadyEnded);
    assert!(!latch.is_requested());
    let row = store.reader()?.attempt("A2")?.ok_or("no A2")?;
    assert_eq!((row.state, row.cancel_requested), (Settled, false));
    // Only an end goes through the latch.
    let mut third = attempt();
    third.attempt_id = "A3".to_owned();
    store.create_attempt(third).await?;
    let mut not_an_end = step(Prepared, Starting, AttemptFacts::default());
    not_an_end.attempt_id = "A3".to_owned();
    assert!(matches!(
        store.end_attempt(not_an_end, &CancelLatch::new()).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    // A waiter wakes on the request.
    let latch = CancelLatch::new();
    let waiter = latch.clone();
    let woke = tokio::spawn(async move { waiter.requested().await });
    latch.request();
    tokio::time::timeout(std::time::Duration::from_secs(5), woke).await??;
    Ok(())
}
