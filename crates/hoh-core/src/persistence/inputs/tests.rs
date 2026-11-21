//! The input queue through the store: the input machine, its events, and the typed read.

use super::*;
use crate::persistence::{Options, Store, selftest};
use std::path::{Path, PathBuf};

type R = Result<(), Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> Result<(Store, PathBuf), PersistError> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    };
    Ok((Store::open(&path, options)?, path))
}

fn seed(path: &Path) -> R {
    Connection::open(path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', '2026-11-04T00:00:00Z');",
    )?;
    Ok(())
}

fn steer(id: &str) -> NewInput {
    NewInput {
        input_id: id.to_owned(),
        run_id: "R".to_owned(),
        kind: "STEER".to_owned(),
        target_role: Some("developer".to_owned()),
        payload: json!({"text": "Stop and reply STEERED."}),
    }
}

fn mv(id: &str, from: InputState, to: InputState, reason: Option<&str>) -> InputMove {
    InputMove {
        input_id: id.to_owned(),
        from,
        to,
        reason: reason.map(str::to_owned),
    }
}

#[tokio::test]
async fn an_input_moves_along_the_queue_with_its_events() -> R {
    use InputState::*;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    store.enqueue_input(steer("I1")).await?;
    let reader = store.reader()?;
    let row = reader.input("I1")?.ok_or("no I1")?;
    assert_eq!(
        (row.state, row.kind.as_str(), row.target_role.as_deref()),
        (Queued, "STEER", Some("developer"))
    );
    assert_eq!(row.payload["text"], "Stop and reply STEERED.");
    store.move_input(mv("I1", Queued, Delivered, None)).await?;
    store.move_input(mv("I1", Delivered, Applied, None)).await?;
    assert_eq!(reader.input("I1")?.map(|r| r.state), Some(Applied));
    store.enqueue_input(steer("I2")).await?;
    store
        .move_input(mv("I2", Queued, Rejected, Some("expected active turn id")))
        .await?;
    let events: Vec<String> = reader
        .events("R")?
        .into_iter()
        .map(|(k, p)| {
            format!(
                "{k}:{}:{}",
                p["input_id"].as_str().unwrap_or(""),
                p["to"].as_str().unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        events,
        [
            "input.queued:I1:QUEUED",
            "input.moved:I1:DELIVERED",
            "input.moved:I1:APPLIED",
            "input.queued:I2:QUEUED",
            "input.moved:I2:REJECTED"
        ]
    );
    let rejected = reader.events("R")?.into_iter().last().ok_or("no event")?.1;
    assert_eq!(rejected["reason"], "expected active turn id");
    Ok(())
}

#[tokio::test]
async fn stale_illegal_and_orphan_inputs_write_nothing() -> R {
    use InputState::*;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    store.enqueue_input(steer("I1")).await?;
    let events = || store.reader().and_then(|r| r.events("R")).map(|e| e.len());
    let before = events()?;
    assert!(matches!(
        store.move_input(mv("I1", Delivered, Applied, None)).await,
        Err(PersistError::StaleState { actual: Some(ref a), .. }) if a == "QUEUED"
    ));
    assert!(matches!(
        store.move_input(mv("I1", Queued, Applied, None)).await,
        Err(PersistError::IllegalTransition {
            entity: Entity::Input,
            ..
        })
    ));
    assert!(matches!(
        store.move_input(mv("nope", Queued, Delivered, None)).await,
        Err(PersistError::StaleState { actual: None, .. })
    ));
    let mut orphan = steer("I9");
    orphan.run_id = "R9".to_owned();
    assert!(matches!(
        store.enqueue_input(orphan).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    let mut malformed = steer("malformed");
    malformed.payload = json!("not an object");
    assert!(matches!(
        store.enqueue_input(malformed).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    let mut bad = steer("I8");
    bad.kind = "WHISPER".to_owned();
    assert!(store.enqueue_input(bad).await.is_err());
    assert!(matches!(
        store.enqueue_input(steer("I1")).await,
        Err(PersistError::IntegrityViolation(_))
    ));
    assert_eq!(events()?, before);
    assert_eq!(store.reader()?.input("I9")?, None);
    Ok(())
}

#[tokio::test]
async fn inputs_ride_outbox_and_wait_for_the_first_input_without_spending_delivery() -> R {
    use crate::persistence::{Claimed, Commit, Intent, NewEvent, NewOutbox};
    let dir = tempfile::Builder::new().prefix("m3t09-input-").tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path)?;
    let conn = Connection::open(&path)?;
    conn.execute_batch("INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES ('sc','codex','commander','1','{}');
        INSERT INTO role_bindings(run_id,role,agent,session_id,role_epoch,config_revision,bound_at) VALUES ('R','commander','codex','sc',2,1,'2026-11-21T00:00:00Z');")?;
    let start = crate::ids::child_id("R", "session-start/commander/2");
    store
        .commit(Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id: "p1".to_owned(),
                run_id: Some("R".to_owned()),
                event_id: "start".to_owned(),
                kind: "session.start".to_owned(),
                payload: json!({}),
            }],
            outbox: vec![NewOutbox {
                message_id: start.clone(),
                event: 0,
                run_id: Some("R".to_owned()),
                recipient_role: "commander".to_owned(),
                role_epoch: 2,
                payload: json!({"notice":"runtime","kind":"session.start"}),
            }],
            ..Default::default()
        })
        .await?;
    store
        .enqueue_input(NewInput {
            input_id: "ask".to_owned(),
            run_id: "R".to_owned(),
            kind: "ASK".to_owned(),
            target_role: None,
            payload: json!({"text":"progress?"}),
        })
        .await?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM outbox WHERE message_id='ask'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1,
        "input acceptance must enqueue delivery"
    );
    let now = std::time::SystemTime::now();
    assert!(
        store
            .claim_outbox(now, vec!["commander".to_owned()], 3)
            .await?
            .is_none()
    );
    conn.execute(
        "UPDATE outbox SET state='ACKED' WHERE message_id=?1",
        params![start],
    )?;
    conn.execute("INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES ('commander',?1,'2026-11-21T00:00:00Z',?2)",params![start,json!({"session_id":"sc"}).to_string()])?;
    assert!(
        store
            .claim_outbox(now, vec!["commander".to_owned()], 3)
            .await?
            .is_none(),
        "ACK alone does not pay the first-input debt"
    );
    assert_eq!(
        conn.query_row(
            "SELECT attempt_count FROM outbox WHERE message_id='ask'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    conn.execute_batch("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) VALUES ('T','R','s1','IMPLEMENTED','commander',0,1);
        INSERT INTO attempts(attempt_id,task_id,state,session_id,role_epoch,host_epoch,config_revision) VALUES ('A','T','SETTLED','sc',2,1,1);")?;
    let row = store
        .claim_outbox(now, vec!["commander".to_owned()], 3)
        .await?
        .and_then(Claimed::start)
        .ok_or("no input after first turn")?;
    assert_eq!(row.message_id, "ask");
    assert_eq!(row.attempt_count, 1);
    assert_eq!(row.role_epoch, 2);
    // The debt can return after the claim; consume rechecks and gives the generation back.
    conn.execute(
        "UPDATE attempts SET state='FAILED' WHERE attempt_id='A'",
        [],
    )?;
    assert_eq!(store.consume_input(row.clone(), now).await?, None);
    store
        .settle_outbox(
            "ask",
            row.attempt_count,
            crate::persistence::Settlement::Unapplied,
            3,
        )
        .await?;
    assert_eq!(
        conn.query_row(
            "SELECT attempt_count FROM outbox WHERE message_id='ask'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    conn.execute(
        "UPDATE attempts SET state='SETTLED' WHERE attempt_id='A'",
        [],
    )?;
    let row = store
        .claim_inputs(now, vec!["commander".to_owned()], 3)
        .await?
        .and_then(Claimed::start)
        .ok_or("no reclaimed input")?;
    let answer = store
        .consume_input(row.clone(), now)
        .await?
        .ok_or("not delivered")?;
    assert_eq!(store.consume_input(row.clone(), now).await?, Some(answer));
    store
        .settle_outbox(
            "ask",
            row.attempt_count,
            crate::persistence::Settlement::Delivered,
            3,
        )
        .await?;
    assert_eq!(
        store.reader()?.input("ask")?.ok_or("input")?.state,
        InputState::Delivered
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM receipts WHERE message_id='ask'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );

    for id in ["stale", "exhausted", "ended"] {
        store
            .enqueue_input(NewInput {
                input_id: id.to_owned(),
                run_id: "R".to_owned(),
                kind: "ASK".to_owned(),
                target_role: None,
                payload: json!({"text":"question"}),
            })
            .await?;
    }
    let stale = store
        .claim_inputs(now, vec!["commander".to_owned()], 3)
        .await?
        .and_then(Claimed::start)
        .ok_or("stale input")?;
    assert_eq!(stale.message_id, "stale");
    conn.execute(
        "UPDATE role_bindings SET role_epoch=3 WHERE run_id='R' AND role='commander'",
        [],
    )?;
    let rejected: Value = serde_json::from_str(
        &store
            .consume_input(stale.clone(), now)
            .await?
            .ok_or("stale refused")?,
    )?;
    assert_eq!(rejected["reason"], "STALE_ROLE_EPOCH");
    store
        .settle_outbox(
            "stale",
            stale.attempt_count,
            crate::persistence::Settlement::Delivered,
            3,
        )
        .await?;
    assert_eq!(
        store.reader()?.input("stale")?.ok_or("input")?.state,
        InputState::Rejected
    );
    let exhausted = store
        .claim_inputs(now, vec!["commander".to_owned()], 1)
        .await?
        .and_then(Claimed::start)
        .ok_or("exhausted input")?;
    assert_eq!(exhausted.message_id, "exhausted");
    store
        .settle_outbox(
            "exhausted",
            exhausted.attempt_count,
            crate::persistence::Settlement::Failed("consumer lost".to_owned()),
            1,
        )
        .await?;
    let rejected = store.reader()?.input("exhausted")?.ok_or("input")?;
    assert_eq!(rejected.state, InputState::Rejected);
    assert_eq!(rejected.payload["reason"], "delivery_exhausted");
    conn.execute("UPDATE runs SET state='CANCELLED' WHERE run_id='R'", [])?;
    store.settle_ended_inputs("p1").await?;
    for id in ["ask", "ended"] {
        let rejected = store.reader()?.input(id)?.ok_or("input")?;
        assert_eq!(rejected.state, InputState::Rejected);
        assert_eq!(rejected.payload["reason"], "run_ended");
    }

    Ok(())
}
