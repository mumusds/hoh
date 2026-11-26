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
        frame_kind: None,
        count: None,
        input_id: None,
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

#[tokio::test]
async fn test_011_05_correlatable_native_user_echo_is_not_duplicated() -> R {
    use crate::persistence::{CommandOutcome, Input, Intent, NewEvent, RunTarget, SubmitCommand};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now');
         INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
         INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now');
         INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1);
         INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);",
    )?;
    let command_id = "cmd-echo-1";
    let input_id = crate::ids::child_id(command_id, "input");
    let accepted = store
        .submit_command(SubmitCommand {
            command_id: command_id.into(),
            idempotency_key: command_id.into(),
            kind: "SendInput".into(),
            run_id: "r".into(),
            expected_revision: 0,
            payload: serde_json::json!({"text":"please fix X","input_kind":"STEER","target_role":"developer"}),
            payload_sha256: "a".repeat(64),
            target: RunTarget::Input(Input::Send {
                text: "please fix X".into(),
                target_role: Some("developer".into()),
                input_kind: "STEER".into(),
            }),
            event: NewEvent {
                intent: Intent::Progress,
                project_id: "p".into(),
                run_id: Some("r".into()),
                event_id: command_id.into(),
                kind: "input.accepted".into(),
                payload: serde_json::json!({"command_id": command_id}),
            },
            outbox: None,
        })
        .await?;
    assert!(matches!(accepted, CommandOutcome::Accepted { .. }));
    let original_seq: i64 = Connection::open(&path)?.query_row(
        "SELECT created_seq FROM public_input_blocks WHERE input_id=?1",
        [&input_id],
        |r| r.get(0),
    )?;
    let original_block: String = Connection::open(&path)?.query_row(
        "SELECT block_id FROM public_input_blocks WHERE input_id=?1",
        [&input_id],
        |r| r.get(0),
    )?;

    // Native echo carrying the accepted input_id must correlate to the existing user block, not duplicate it.
    let mut echo = part("echo-frame-1", 0, 0);
    echo.speaker = Some("user".into());
    echo.text = "please fix X".into();
    echo.block_key = format!("input/{input_id}");
    echo.input_id = Some(input_id.clone());
    let linked = store.record_public("a", 1, echo).await?;
    assert_eq!(
        linked,
        Some(original_seq),
        "correlatable native echo must link to the existing accepted input seq"
    );
    let msg_count: i64 = Connection::open(&path)?.query_row(
        "SELECT count(*) FROM events WHERE kind='public.message'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        msg_count, 1,
        "native echo of accepted input must not create a duplicate public.message"
    );
    let linked_block: String = Connection::open(&path)?.query_row(
        "SELECT block_id FROM public_native_parts WHERE attempt_id='a' AND native_item_id='echo-frame-1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(linked_block, original_block);

    // An uncorrelatable native echo (unknown input_id) must never impersonate a new user submission.
    let mut unverified = part("echo-frame-unknown", 0, 1);
    unverified.speaker = Some("user".into());
    unverified.text = "forged user echo".into();
    unverified.block_key = "input/unknown".into();
    unverified.input_id = Some("unknown-input-id".into());
    store.record_public("a", 1, unverified).await?;
    let msg_count_after: i64 = Connection::open(&path)?.query_row(
        "SELECT count(*) FROM events WHERE kind='public.message'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        msg_count_after, 1,
        "unverified native echo must not create a user public.message"
    );
    let diag_source: String = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json,'$.source') FROM events WHERE kind='public.diagnostic' ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(diag_source, "native_echo");
    Ok(())
}

/// Run R (RUNNING) with one developer task T1 READY, its role bound to session `sd`.
const SESSION_WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'READY', 1, 'developer', 1, 1);
";

/// T1 dispatched and RUNNING in session `sd`: its attempt and role epoch.
async fn running_in_session(store: &Store, now: std::time::SystemTime) -> R<(String, i64)> {
    let context = crate::decision::Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events", "command_exec"]
                .map(str::to_owned)
                .into_iter()
                .collect(),
        )]
        .into(),
        now: Some(now),
        ..crate::decision::Context::default()
    };
    store.pace_run("R", context.clone()).await?;
    let attempt = store
        .schedule_round("R", context)
        .await?
        .dispatched
        .ok_or("T1 not dispatched")?
        .attempt_id;
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no notice")?;
    assert!(store.runner_received(notice, now).await?);
    assert!(
        store
            .runner_running(
                &attempt,
                crate::persistence::AttemptFacts {
                    pid: Some(4242),
                    ..Default::default()
                },
                serde_json::json!({"kind": "proc"}),
            )
            .await?
    );
    Ok((attempt, 1))
}

/// DC-M4T09c: every kind of public gap (a native stream left unfinished, a dropped body, an interrupted attempt and
/// the process-end loss) carries the attempt's identity and is indexed in the session of the attempt's public blocks.
#[tokio::test]
async fn test_011_05_every_public_gap_kind_is_indexed_with_its_attempts_session() -> R {
    use crate::persistence::{Ending, Observed};
    use crate::public_output::{PublicDrop, PublicEndGap};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch(SESSION_WORLD)?;
    let now = crate::time::parse_rfc3339("2026-11-07T00:00:00Z").ok_or("time")?;
    let (attempt, epoch) = running_in_session(&store, now).await?;
    let mut message = part("m1", 0, 0);
    message.text = "hello\n".into();
    store.record_public(&attempt, epoch, message).await?;
    let mut unfinished = part("gap/9/native_stream_unfinished", 0, 2);
    unfinished.kind = PublicKind::Gap;
    unfinished.block_key = "gap/9".into();
    unfinished.received_offset = 9;
    unfinished.speaker = None;
    unfinished.text = String::new();
    unfinished.activity = Some("native_stream_unfinished".into());
    store.record_public(&attempt, epoch, unfinished).await?;
    let mut place = part("m2", 6, 1);
    place.text = String::new();
    store
        .record_public_gap(
            &attempt,
            epoch,
            PublicDrop {
                part: place,
                dropped_bytes: 7,
                reason: "public_storage_error".into(),
                details: serde_json::Map::new(),
            },
        )
        .await?;
    assert!(
        store
            .runner_finished(
                &attempt,
                Ending {
                    success: false,
                    detail: "FAILED".into(),
                    class: crate::retry::FailureClass::Transient { retry_after: None },
                    at: now + std::time::Duration::from_secs(1),
                    draw: 0,
                    observed: Observed {
                        public_end_gap: Some(PublicEndGap {
                            reason: "public_output_lost".into(),
                            first_dropped_offset: Some(3),
                            unrecorded_parts: 1,
                            dropped_bytes: 4,
                            ..PublicEndGap::default()
                        }),
                        ..Observed::default()
                    },
                    unknown_effect: false,
                },
            )
            .await?
    );
    while !store.ui_backfill().await?.index_ready {}
    let conn = Connection::open(&path)?;
    let session: String = conn.query_row(
        "SELECT i.session_id FROM events e JOIN ui_event_index i ON i.seq=e.seq WHERE e.kind='public.message' AND i.attempt_id=?1",
        [&attempt],
        |r| r.get(0),
    )?;
    assert_eq!(session, "sd");
    let message: serde_json::Value = serde_json::from_str(&conn.query_row(
        "SELECT payload_json FROM events WHERE kind='public.message'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    let gaps: i64 = conn.query_row(
        "SELECT count(*) FROM events WHERE kind='public.gap'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(gaps, 4, "one gap of each kind");
    // An interrupted attempt's gap is named by the state it ended in.
    let ended: String = conn.query_row(
        "SELECT state FROM attempts WHERE attempt_id=?1",
        [&attempt],
        |r| r.get(0),
    )?;
    for reason in [
        "native_stream_unfinished",
        "public_storage_error",
        ended.as_str(),
        "public_output_lost",
    ] {
        let (payload, indexed): (String, Option<String>) = conn
            .query_row(
                "SELECT e.payload_json,i.session_id FROM events e LEFT JOIN ui_event_index i ON i.seq=e.seq WHERE e.kind='public.gap' AND json_extract(e.payload_json,'$.reason')=?1",
                [reason],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("{reason}: {e}"))?;
        let gap: serde_json::Value = serde_json::from_str(&payload)?;
        assert_eq!(
            indexed.as_deref(),
            Some(session.as_str()),
            "{reason}: {gap}"
        );
        for key in [
            "attempt_id",
            "run_id",
            "task_id",
            "role",
            "role_epoch",
            "session_id",
            "config_revision",
            "spec_id",
        ] {
            assert_eq!(gap[key], message[key], "{reason} {key}: {gap}");
        }
        assert!(gap["block_id"].is_string(), "{reason}: {gap}");
        assert!(gap["current"].is_boolean(), "{reason}: {gap}");
        assert_eq!(gap["complete"], false, "{reason}: {gap}");
    }
    let end: serde_json::Value = serde_json::from_str(&conn.query_row(
        "SELECT payload_json FROM events WHERE kind='public.gap' AND json_extract(payload_json,'$.reason')='public_output_lost'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    assert_eq!(end["first_dropped_offset"], 3, "{end}");
    assert_eq!(end["unrecorded_parts"], 1, "{end}");
    assert_eq!(end["dropped_bytes"], 4, "{end}");
    assert_eq!(end["dropped_parts"], 0, "{end}");
    assert_eq!(end["frames_after_close"], 0, "{end}");
    assert_eq!(
        end["current"], false,
        "an ended attempt's gap is history: {end}"
    );
    Ok(())
}

/// DC-M4T09c source guard: a `public.gap` event is built only by the store's one gap builder, never by a payload
/// assembled elsewhere (an inline, interrupted, unfinished, process-end or drain-lost gap alike).
#[test]
fn test_011_05_public_gap_events_have_one_builder() -> R {
    fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if path.is_dir() {
                if name != "tests" {
                    sources(&path, out)?;
                }
            } else if name.ends_with(".rs") && name != "tests.rs" {
                out.push(path);
            }
        }
        Ok(())
    }
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no crates dir")?;
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates)? {
        let src = entry?.path().join("src");
        if src.is_dir() {
            sources(&src, &mut files)?;
        }
    }
    assert!(
        files.len() > 50,
        "the guard scans the workspace: {}",
        files.len()
    );
    let mut offenders = Vec::new();
    let mut builder = 0;
    for file in &files {
        let text = std::fs::read_to_string(file)?;
        // An inline test module closes the file; its fixtures are not production payloads.
        let text = text
            .find("#[cfg(test)]\nmod tests {")
            .map_or(text.as_str(), |at| &text[..at]);
        let rel = file.strip_prefix(crates)?.display().to_string();
        let named = text.matches("\"public.gap\"").count();
        let allowed = match rel.as_str() {
            // The kind's name, and the list of kinds a stopped run still records: neither builds a payload.
            "hoh-core/src/public_output.rs" | "hoh-core/src/persistence/hold.rs" => 1,
            _ => 0,
        };
        if named != allowed {
            offenders.push(format!("{rel}: \"public.gap\" x{named}"));
        }
        let kinds = text.matches(".event_kind()").count();
        if rel == "hoh-core/src/persistence/public.rs" {
            builder = text.matches("PublicKind::Gap.event_kind()").count();
            // The body path's kind, behind the dispatch that sends a native gap part to the gap builder.
            if kinds != 2 || !text.contains("if part.kind == PublicKind::Gap {") {
                offenders.push(format!("{rel}: .event_kind() x{kinds} or no gap dispatch"));
            }
        } else if kinds != 0 {
            offenders.push(format!("{rel}: .event_kind() x{kinds}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "public.gap built outside its builder: {offenders:?}"
    );
    assert_eq!(builder, 1, "one gap builder in persistence/public.rs");
    Ok(())
}
