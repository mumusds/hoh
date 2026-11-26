use crate::persistence::{Options, Store, selftest};
use crate::public_output::{PublicKind, PublicPart};
use rusqlite::Connection;
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn part(id: &str, offset: u64, part: u64) -> PublicPart {
    PublicPart {
        native_stream: "claude".into(),
        native_item_id: id.into(),
        part,
        received_offset: part,
        block_key: id.into(),
        stream: "text".into(),
        first_offset: offset,
        kind: PublicKind::Message,
        speaker: Some("assistant".into()),
        name: None,
        text: "same segment".repeat(2000),
        activity: None,
        model: None,
        usage: None,
        redacted: false,
        complete: false,
    }
}
#[tokio::test]
async fn test_011_05_native_occurrences_and_offsets_are_durable_and_idempotent() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let options = || Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    };
    let store = Store::open(&path, options())?;
    Connection::open(&path)?.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO specs VALUES('s','p',printf('%064d',1),1); INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now'); INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1); INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);")?;
    let first = part("message-a", 0, 0);
    let seq = store.record_public("a", 1, first.clone()).await?;
    assert!(
        store.record_public("a", 2, first.clone()).await.is_err(),
        "replay still validates the attempt epoch"
    );
    assert!(
        seq.is_some(),
        "native public message commits while attempt runs"
    );
    store.record_public("a", 1, part("message-b", 0, 0)).await?;
    store
        .record_public("a", 1, part("message-a", first.text.len() as u64, 1))
        .await?;
    let events = store.reader()?.events("r")?;
    let messages: Vec<_> = events
        .iter()
        .filter(|(k, _)| k == "public.message")
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0].1["block_id"], messages[2].1["block_id"]);
    assert_ne!(messages[0].1["block_id"], messages[1].1["block_id"]);
    let ids: Vec<_> = messages
        .iter()
        .map(|(_, p)| p["refs"][0]["artifact_id"].clone())
        .collect();
    assert_eq!(ids[0], ids[1]);
    assert_eq!(ids[0], ids[2]);
    assert!(!ids[0].is_null());
    drop(store);
    let store = Store::open(&path, options())?;
    assert_eq!(store.record_public("a", 1, first).await?, seq);
    assert_eq!(store.reader()?.events("r")?, events);
    let conn = Connection::open(&path)?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM public_segments", [], |r| r
            .get::<_, i64>(0))?,
        3
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
            .get::<_, i64>(
            0
        ))?,
        0
    );
    Ok(())
}

#[tokio::test]
async fn test_011_05_recovery_preserves_committed_text_and_marks_an_unfinished_stream() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO specs VALUES('s','p',printf('%064d',1),1); INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now'); INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1); INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);")?;
    let seq = store.record_public("a", 1, part("partial", 0, 0)).await?;
    let now = std::time::SystemTime::now();
    let row = store.reconcile_scan("p", now).await?.remove(0);
    store
        .reconcile_apply(
            row,
            crate::reconcile::Disposition::UnknownEffect {
                effects: Vec::new(),
                unknown: Vec::new(),
            },
            crate::reconcile::Process::Gone {
                basis: "test process ended before public stream completion".into(),
            },
            "reconcile",
            now,
            0,
        )
        .await?;
    let events = store.reader()?.events("r")?;
    assert!(
        events
            .iter()
            .any(|(k, p)| k == "public.gap" && p["attempt_id"] == "a"),
        "recovery must not present a partial stream as complete"
    );
    assert_eq!(
        store.record_public("a", 1, part("partial", 0, 0)).await?,
        seq
    );
    assert_eq!(
        events.iter().filter(|(k, _)| k == "public.message").count(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn test_011_05_public_object_with_nul_is_classified_binary() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO specs VALUES('s','p',printf('%064d',1),1); INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now'); INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1); INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);")?;
    let mut nul = part("nul-body", 0, 0);
    nul.text = format!("prefix\0{}", "x".repeat(20_000));
    store.record_public("a", 1, nul).await?;
    let kind: String = Connection::open(&path)?.query_row(
        "SELECT text_kind FROM ui_object_metadata LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(kind, "binary");
    Ok(())
}
