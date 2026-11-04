//! The input queue through the store: the input machine, its events, and the typed read.

use super::*;
use crate::persistence::{Options, Store, selftest};
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
