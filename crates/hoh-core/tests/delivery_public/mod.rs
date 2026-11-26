//! Integration owner evidence for CR-028 identities and persisted occurrence semantics.
use super::*;
use hoh_core::ids::child_id;
use hoh_core::lifecycle::Lifecycle;
use hoh_core::public_output::{PublicKind, PublicPart};
use sha2::{Digest, Sha256};
const WORLD: &str = "INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO specs VALUES('s','p',printf('%064d',1),1); INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now'); INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1); INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);";
fn message(id: &str, offset: u64, part: u64) -> PublicPart {
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
        frame_kind: None,
        count: None,
        input_id: None,
    }
}
#[tokio::test]
async fn test_011_05_native_occurrences_survive_upgrade_reopen_and_stale_epoch() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    seed(
        &path,
        include_str!("../../../../contracts/sql/001_initial.sql"),
    )?;
    seed(&path, WORLD)?;
    let (store, _) = open(&dir)?;
    assert_eq!(one(&path, "PRAGMA user_version")?, 3);
    let original = message("a", 0, 0);
    let first = store.record_public("a", 1, original.clone()).await?;
    assert!(first.is_some());
    store.record_public("a", 1, message("b", 0, 0)).await?;
    store
        .record_public("a", 1, message("a", original.text.len() as u64, 1))
        .await?;
    assert_eq!(one(&path, "SELECT count(*) FROM public_segments")?, 3);
    assert_eq!(
        one(
            &path,
            "SELECT count(DISTINCT artifact_id) FROM public_segments"
        )?,
        1
    );
    let before = store.reader()?.events("r")?;
    assert!(
        store
            .record_public("a", 2, message("wrong", 0, 0))
            .await
            .is_err()
    );
    assert_eq!(store.reader()?.events("r")?, before);
    drop(store);
    let (store, _) = open(&dir)?;
    let segment_offset = original.text.len() as u64;
    assert_eq!(store.record_public("a", 1, original).await?, first);
    store
        .record_public("a", 1, message("a", segment_offset, 1))
        .await?;
    assert_eq!(store.reader()?.events("r")?, before);
    assert!(rows(&path, "PRAGMA foreign_key_check")?.is_empty());
    for row in rows(&path, "SELECT path,sha256 FROM artifacts")? {
        let text = std::fs::read(dir.path().join(row[0].as_str().ok_or("path")?))?;
        assert_eq!(text, "same segment".repeat(2000).as_bytes());
        assert_eq!(
            Sha256::digest(&text)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            row[1].as_str().ok_or("sha")?
        );
    }
    Ok(())
}
#[tokio::test]
async fn test_013_05_upgrade_backfill_snapshot_boundaries_and_resume_keep_keys() -> R {
    for state in [
        "COMPLETED",
        "FAILED",
        "CANCELLED",
        "PAUSED",
        "BLOCKED",
        "INCOMPLETE",
    ] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(".hoh/state.db");
        std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
        seed(
            &path,
            include_str!("../../../../contracts/sql/001_initial.sql"),
        )?;
        seed(&path, WORLD)?;
        seed(
            &path,
            &format!(
                "INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e1','r','task.started','{{\"attempt_id\":\"a\"}}','now'),('p','e2','r','iteration.completed','{{\"iteration\":1}}','now'),('p','e3','r','task.started','{{\"attempt_id\":\"a\"}}','now'),('p','e4','r','run.result','{{\"state\":\"{state}\"}}','now');"
            ),
        )?;
        let (store, _) = open(&dir)?;
        assert!(matches!(
            store.reader()?.ui_identities("r", 4),
            Err(PersistError::Busy(_))
        ));
        let indexed = store.ui_backfill().await?;
        assert!(indexed.index_ready);
        let h1 = store.reader()?.ui_identities("r", 4)?;
        assert_eq!(h1[0].iteration_key, Some(child_id("r", "iteration/1")));
        assert_eq!(h1[1].iteration_key, h1[0].iteration_key);
        assert_eq!(
            h1[2].iteration_key,
            if matches!(state, "PAUSED" | "BLOCKED") {
                Some(child_id("r", "iteration/2"))
            } else {
                None
            }
        );
        if state == "INCOMPLETE" {
            store
                .commit(Commit {
                    events: vec![
                        NewEvent {
                            intent: Intent::Progress,
                            project_id: "p".into(),
                            run_id: Some("r".into()),
                            event_id: "resume/reconcile".into(),
                            kind: "reconcile.run".into(),
                            payload: json!({"from":"INCOMPLETE","command_id":"resume"}),
                        },
                        NewEvent {
                            intent: Intent::Progress,
                            project_id: "p".into(),
                            run_id: Some("r".into()),
                            event_id: "resume".into(),
                            kind: "run.resumed".into(),
                            payload: json!({"command_id":"resume","reason":"continue"}),
                        },
                    ],
                    ..Default::default()
                })
                .await?;
            assert_eq!(
                store.reader()?.ui_identities("r", 6)?[2].iteration_key,
                Some(child_id("r", "iteration/2"))
            );
            assert_eq!(store.reader()?.ui_identities("r", 4)?, h1);
        }
        drop(store);
        let (store, _) = open(&dir)?;
        assert_eq!(store.ui_backfill().await?.generation, indexed.generation);
        assert_eq!(store.reader()?.ui_identities("r", 4)?, h1);
        assert_eq!(one(&path, "SELECT count(*) FROM iterations")?, 0);
    }
    Ok(())
}

#[tokio::test]
async fn test_011_05_reconcile_keeps_native_history_and_marks_the_interruption() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    let part = message("partial", 0, 0);
    let seq = store.record_public("a", 1, part.clone()).await?;
    let now = SystemTime::now();
    let row = store.reconcile_scan("p", now).await?.remove(0);
    store
        .reconcile_apply(
            row,
            hoh_core::reconcile::Disposition::UnknownEffect {
                effects: Vec::new(),
                unknown: Vec::new(),
            },
            hoh_core::reconcile::Process::Gone {
                basis: "integration fixture process gone".into(),
            },
            "reconcile",
            now,
            0,
        )
        .await?;
    let before = store.reader()?.events("r")?;
    assert!(
        before
            .iter()
            .any(|(k, p)| k == "public.gap" && p["attempt_id"] == "a" && p["current"] == false)
    );
    drop(store);
    let (store, _) = open(&dir)?;
    assert_eq!(store.record_public("a", 1, part).await?, seq);
    assert_eq!(store.reader()?.events("r")?, before);
    assert!(
        store
            .record_public("a", 2, message("partial", 0, 0))
            .await
            .is_err()
    );
    store.record_public("a", 1, message("late", 0, 0)).await?;
    let late = store
        .reader()?
        .events("r")?
        .into_iter()
        .find(|(_, p)| p["native_item_id"] == "late")
        .ok_or("late history")?
        .1;
    assert_eq!(late["current"], false);
    assert_eq!(late["role_epoch"], 1);
    Ok(())
}

#[tokio::test]
async fn test_013_05_upgrade_backfill_resumes_a_durable_bounded_checkpoint() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
    seed(
        &path,
        include_str!("../../../../contracts/sql/001_initial.sql"),
    )?;
    seed(&path, WORLD)?;
    seed(
        &path,
        "WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<1001) INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) SELECT 'p','old/'||x,'r','task.updated','{}','now' FROM n;",
    )?;
    let (store, _) = open(&dir)?;
    let first = store.ui_backfill().await?;
    assert_eq!(first.indexed_through, 500);
    assert!(!first.index_ready);
    let stopped = store.writer_ended();
    drop(store);
    assert!(stopped.await);
    let (store, _) = open(&dir)?;
    let second = store.ui_backfill().await?;
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.indexed_through, 1000);
    assert!(!second.index_ready);
    let last = store.ui_backfill().await?;
    assert!(last.index_ready);
    assert_eq!(last.indexed_through, 1001);
    assert_eq!(store.reader()?.ui_identities("r", 1001)?.len(), 1001);
    assert_eq!(store.ui_backfill().await?, last);
    assert_eq!(one(&path, "SELECT count(*) FROM events")?, 1001);
    Ok(())
}

#[tokio::test]
async fn test_013_05_writer_cas_and_control_snapshots_use_run_close_intervals() -> R {
    use hoh_core::persistence::{RunTarget, SubmitCommand};
    use hoh_core::protocol::RunState;
    for state in [
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
        RunState::Paused,
        RunState::Incomplete,
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        seed(&path, WORLD)?;
        seed(
            &path,
            "UPDATE attempts SET state='SETTLED'; UPDATE tasks SET state='IMPLEMENTED'; INSERT INTO iterations VALUES('engineering-closed','r',1); UPDATE tasks SET iteration_id='engineering-closed'; INSERT INTO runs(run_id,project_id,state,mode,created_at) VALUES('other','p','RUNNING','AUTO','now');",
        )?;
        if state == RunState::Completed {
            seed(&path, "UPDATE runs SET state='VERIFYING' WHERE run_id='r'")?;
        }
        let mut seqs = Vec::new();
        for (id, run, kind, payload) in [
            (
                "first",
                Some("r"),
                "task.started",
                json!({"attempt_id":"a"}),
            ),
            ("other", Some("other"), "task.updated", json!({})),
            (
                "close",
                Some("r"),
                "iteration.completed",
                json!({"iteration":1}),
            ),
            ("tail", Some("r"), "task.started", json!({"attempt_id":"a"})),
            ("project", None, "milestone_ready", json!({"run_id":"r"})),
            ("release", Some("r"), "release_request", json!({})),
        ] {
            seqs.push(
                store
                    .commit(Commit {
                        events: vec![NewEvent {
                            intent: Intent::Progress,
                            project_id: "p".into(),
                            run_id: run.map(str::to_owned),
                            event_id: id.into(),
                            kind: kind.into(),
                            payload,
                        }],
                        ..Default::default()
                    })
                    .await?
                    .event_seqs[0],
            );
        }
        let opened = store
            .reader()?
            .ui_identities("r", *seqs.last().ok_or("seq")?)?;
        assert_eq!(opened[0].iteration_key, Some(child_id("r", "iteration/1")));
        assert_eq!(opened[1].iteration_key, opened[0].iteration_key);
        assert_eq!(opened[2].iteration_key, Some(child_id("r", "iteration/2")));
        assert_eq!(opened.len(), 4);
        assert!(one(&path,"SELECT iteration_key FROM ui_event_index WHERE seq=(SELECT seq FROM events WHERE event_id='project')")?.is_null());
        if matches!(state, RunState::Paused | RunState::Cancelled) {
            let (kind, event, to) = if state == RunState::Paused {
                ("PauseRun", "run.pause_requested", RunState::PauseRequested)
            } else {
                (
                    "CancelRun",
                    "run.cancel_requested",
                    RunState::CancelRequested,
                )
            };
            store
                .submit_command(SubmitCommand {
                    command_id: "control".into(),
                    idempotency_key: "control".into(),
                    kind: kind.into(),
                    run_id: "r".into(),
                    expected_revision: 0,
                    payload: json!({"reason":"test"}),
                    payload_sha256: sha(7),
                    target: RunTarget::To(to),
                    event: NewEvent {
                        intent: Intent::Progress,
                        project_id: "p".into(),
                        run_id: Some("r".into()),
                        event_id: "control".into(),
                        kind: event.into(),
                        payload: json!({"command_id":"control","reason":"test"}),
                    },
                    outbox: None,
                })
                .await?;
            let requested = one(&path, "SELECT max(seq) FROM events")?
                .as_i64()
                .ok_or("h")?;
            assert_eq!(
                store.reader()?.ui_identities("r", requested)?[2].iteration_key,
                Some(child_id("r", "iteration/2"))
            );
            if state == RunState::Paused {
                store.pause_step("r", SystemTime::now()).await?;
            } else {
                store
                    .finish_cancel("r", json!({}), BTreeSet::new(), SystemTime::now())
                    .await?;
            }
        } else if state == RunState::Incomplete {
            seed(
                &path,
                "UPDATE runs SET created_at='2026-11-07T00:00:00Z' WHERE run_id='r';
                 INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,used_value) VALUES('run','r','iterations',1,1);",
            )?;
            let paced = store
                .pace_run("r", hoh_core::decision::Context::default())
                .await?;
            assert_eq!(paced.incomplete, Some("max_iterations"));
        } else {
            move_run(&store, &path, "r", &[state]).await?;
        }
        let h1 = one(&path, "SELECT max(seq) FROM events")?
            .as_i64()
            .ok_or("h1")?;
        let snapshot = store.reader()?.ui_identities("r", h1)?;
        assert_eq!(
            snapshot[2].iteration_key,
            if state == RunState::Paused {
                Some(child_id("r", "iteration/2"))
            } else {
                None
            },
            "{state:?}"
        );
        let events = store.reader()?.events("r")?;
        assert!(
            events
                .iter()
                .any(|(k, p)| k == "run.result" && p["state"] == state.as_str())
        );
        if state == RunState::Incomplete {
            assert!(
                events
                    .iter()
                    .any(|(k, p)| k == "run.incomplete" && p["stop"] == "max_iterations"),
                "INCOMPLETE must come from the real budget path writing run.incomplete"
            );
            seed(
                &path,
                "UPDATE budgets SET limit_value=2 WHERE scope_id='r' AND kind='iterations'",
            )?;
            let rev = one(&path, "SELECT revision FROM runs WHERE run_id='r'")?
                .as_i64()
                .ok_or("rev")?;
            store
                .submit_command(SubmitCommand {
                    command_id: "resume".into(),
                    idempotency_key: "resume".into(),
                    kind: "ResumeRun".into(),
                    run_id: "r".into(),
                    expected_revision: rev,
                    payload: json!({"reason":"continue"}),
                    payload_sha256: sha(8),
                    target: RunTarget::Resume,
                    event: NewEvent {
                        intent: Intent::Progress,
                        project_id: "p".into(),
                        run_id: Some("r".into()),
                        event_id: "resume".into(),
                        kind: "run.resumed".into(),
                        payload: json!({"command_id":"resume","reason":"continue"}),
                    },
                    outbox: None,
                })
                .await?;
            let h2 = one(&path, "SELECT max(seq) FROM events")?
                .as_i64()
                .ok_or("h2")?;
            assert_eq!(
                store.reader()?.ui_identities("r", h2)?[2].iteration_key,
                Some(child_id("r", "iteration/2"))
            );
        }
        assert_eq!(store.reader()?.ui_identities("r", h1)?, snapshot);
        assert_eq!(one(&path, "SELECT count(*) FROM iterations")?, 1);
        let original = store.reader()?.events("r")?;
        let end = store.writer_ended();
        drop(store);
        assert!(end.await);
        let (store, _) = open(&dir)?;
        store.ui_backfill().await?;
        assert_eq!(store.reader()?.ui_identities("r", h1)?, snapshot);
        assert_eq!(store.reader()?.events("r")?, original);
    }
    Ok(())
}

#[tokio::test]
async fn test_013_05_damaged_legacy_boundaries_keep_source_and_refuse_ready() -> R {
    for missing_close in [true, false] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(".hoh/state.db");
        std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
        seed(
            &path,
            include_str!("../../../../contracts/sql/001_initial.sql"),
        )?;
        seed(&path, WORLD)?;
        let kind = if missing_close {
            seed(&path, "INSERT INTO iterations VALUES('lost','r',1)")?;
            "task.started"
        } else {
            "run.incomplete"
        };
        seed(
            &path,
            &format!(
                "INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','bad','r','{kind}','{{}}','now')"
            ),
        )?;
        let (store, _) = open(&dir)?;
        let before = store.reader()?.events("r")?;
        assert!(!store.ui_backfill().await?.index_ready);
        assert!(matches!(
            store.reader()?.ui_identities("r", 1),
            Err(PersistError::Busy(_))
        ));
        assert_eq!(store.reader()?.events("r")?, before);
        assert_ne!(
            one(
                &path,
                "SELECT count(*) FROM events WHERE kind='runner.diagnostic'"
            )?,
            0
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_011_05_input_acceptance_replay_and_shared_body_after_001_002_003() -> R {
    use hoh_core::persistence::{CommandOutcome, Input, RunTarget, SubmitCommand};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    std::fs::create_dir_all(path.parent().ok_or("parent")?)?;
    seed(
        &path,
        include_str!("../../../../contracts/sql/001_initial.sql"),
    )?;
    seed(&path, WORLD)?;
    let (store, _) = open(&dir)?;
    assert_eq!(one(&path, "PRAGMA user_version")?, 3);
    let original = "accepted\r\noriginal\rtext\n".repeat(2000);
    let normalized = original.replace("\r\n", "\n").replace('\r', "\n");
    let command = |id: &str, revision: i64| SubmitCommand {
        command_id: id.into(),
        idempotency_key: id.into(),
        kind: "SendInput".into(),
        run_id: "r".into(),
        expected_revision: revision,
        payload: json!({"text":original,"input_kind":"STEER","target_role":"developer"}),
        payload_sha256: Sha256::digest(id.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        target: RunTarget::Input(Input::Send {
            text: original.clone(),
            target_role: Some("developer".into()),
            input_kind: "STEER".into(),
        }),
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p".into(),
            run_id: Some("r".into()),
            event_id: id.into(),
            kind: "input.accepted".into(),
            payload: json!({"command_id":id}),
        },
        outbox: None,
    };
    let first = command("accepted-1", 0);
    let accepted = store.submit_command(first.clone()).await?;
    assert!(matches!(accepted, CommandOutcome::Accepted { .. }));
    let revision = one(&path, "SELECT revision FROM runs WHERE run_id='r'")?
        .as_i64()
        .ok_or("revision")?;
    store
        .submit_command(command("accepted-2", revision))
        .await?;
    assert_eq!(one(&path, "SELECT count(*) FROM public_input_blocks")?, 2);
    assert_eq!(one(&path, "SELECT count(*) FROM public_segments")?, 2);
    assert_eq!(
        one(
            &path,
            "SELECT count(DISTINCT artifact_id) FROM public_segments"
        )?,
        1
    );
    let events = store.reader()?.events("r")?;
    let end = store.writer_ended();
    drop(store);
    assert!(end.await);
    let (store, _) = open(&dir)?;
    assert!(matches!(
        store.submit_command(first).await?,
        CommandOutcome::Replayed { .. }
    ));
    assert_eq!(store.reader()?.events("r")?, events);
    for row in rows(&path, "SELECT path,sha256 FROM artifacts")? {
        let body = std::fs::read(dir.path().join(row[0].as_str().ok_or("path")?))?;
        assert_eq!(body, normalized.as_bytes());
        assert_eq!(
            Sha256::digest(&body)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            row[1].as_str().ok_or("sha")?
        );
    }
    assert!(rows(&path, "PRAGMA foreign_key_check")?.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_013_05_online_and_forced_rebuild_identities_match_field_by_field() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    seed(
        &path,
        "INSERT INTO sessions VALUES('sess','claude','developer',NULL,NULL,'2.1.318','{}')",
    )?;
    let ev = |id: &str| NewEvent {
        intent: Intent::Progress,
        project_id: "p".into(),
        run_id: Some("r".into()),
        event_id: id.into(),
        kind: "task.started".into(),
        payload: json!({"attempt_id":"a"}),
    };
    store
        .commit(Commit {
            events: vec![ev("x1")],
            ..Default::default()
        })
        .await?;
    // Simulate late session binding on attempts (as move_attempt's COALESCE does).
    seed(
        &path,
        "UPDATE attempts SET session_id='sess' WHERE attempt_id='a'",
    )?;
    store
        .commit(Commit {
            events: vec![ev("x2")],
            ..Default::default()
        })
        .await?;
    let h = store
        .record_public("a", 1, message("pub-with-sess", 0, 0))
        .await?
        .ok_or("public seq")?;
    let online = store.reader()?.ui_identities("r", h)?;
    assert_eq!(online.len(), 3);
    assert_eq!(
        online[2].session_id.as_deref(),
        Some("sess"),
        "public events carry immutable session_id in payload"
    );
    let end = store.writer_ended();
    drop(store);
    assert!(end.await);
    // Force a new generation rebuild by deleting one index row.
    seed(
        &path,
        "DELETE FROM ui_event_index WHERE seq=(SELECT min(seq) FROM ui_event_index)",
    )?;
    let (store, _) = open(&dir)?;
    let mut ready = false;
    let mut generation = 0;
    for _ in 0..10 {
        let s = store.ui_backfill().await?;
        generation = s.generation;
        if s.index_ready {
            ready = true;
            break;
        }
    }
    assert!(ready);
    assert!(
        generation > 1,
        "forced rebuild must advance generation, got {generation}"
    );
    let rebuilt = store.reader()?.ui_identities("r", h)?;
    assert_eq!(
        online, rebuilt,
        "online and forced-rebuild identity sets must match field by field"
    );
    Ok(())
}
