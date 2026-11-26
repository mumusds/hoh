use super::*;
use serde_json::json;
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn db() -> R<Connection> {
    let conn = Connection::open_in_memory()?;
    crate::persistence::migrate(&conn, crate::persistence::MIGRATIONS)?;
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO runs(run_id,project_id,state,mode,created_at) VALUES('r','p','RUNNING','AUTO','now');")?;
    Ok(conn)
}
fn append(conn: &Connection, kind: &str, payload: Value) -> R<i64> {
    let seq: i64 = conn.query_row("SELECT coalesce(max(seq),0)+1 FROM events", [], |r| {
        r.get(0)
    })?;
    conn.execute("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p',?1,'r',?2,?3,'now')",params![format!("event/{seq}"),kind,payload.to_string()])?;
    inserted(conn, seq, Some("r"), kind, &payload)?;
    Ok(seq)
}
#[test]
fn test_013_05_terminal_paused_and_incomplete_resume_snapshot_boundaries() -> R {
    for state in [
        "COMPLETED",
        "FAILED",
        "CANCELLED",
        "INCOMPLETE",
        "PAUSED",
        "BLOCKED",
    ] {
        let conn = db()?;
        append(&conn, "task.started", json!({"task_id":"same"}))?;
        let close = append(&conn, "iteration.completed", json!({"iteration":1}))?;
        let tail = append(&conn, "task.started", json!({"task_id":"same"}))?;
        let h1 = append(&conn, "run.result", json!({"state":state}))?;
        let before = at(&conn, "r", h1)?;
        let hidden = matches!(state, "COMPLETED" | "FAILED" | "CANCELLED" | "INCOMPLETE");
        assert_eq!(before[1].seq, close);
        assert_eq!(before[1].iteration_key, Some(child_id("r", "iteration/1")));
        assert_eq!(before[2].seq, tail);
        assert_eq!(
            before[2].iteration_key,
            (!hidden).then(|| child_id("r", "iteration/2")),
            "{state}"
        );
        if state == "INCOMPLETE" {
            append(
                &conn,
                "reconcile.run",
                json!({"from":"INCOMPLETE","command_id":"resume"}),
            )?;
            let h2 = append(
                &conn,
                "run.resumed",
                json!({"command_id":"resume","reason":"continue"}),
            )?;
            assert_eq!(
                at(&conn, "r", h2)?[2].iteration_key,
                Some(child_id("r", "iteration/2"))
            );
            assert_eq!(
                at(&conn, "r", h1)?,
                before,
                "historical H remains byte-for-byte stable"
            );
            assert_eq!(
                conn.query_row(
                    "SELECT count(*) FROM ui_iteration_ranges WHERE close_seq IS NOT NULL",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
        }
    }
    Ok(())
}

#[test]
fn test_013_05_backfill_checkpoints_are_bounded_and_restart_keeps_generation() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("old.db");
    let mut conn = Connection::open(&path)?;
    crate::persistence::migrate(&conn, &crate::persistence::MIGRATIONS[..2])?;
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO runs(run_id,project_id,state,mode,created_at) VALUES('r','p','RUNNING','AUTO','now');")?;
    for n in 1..=1001 {
        conn.execute("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p',?1,'r','task.updated','{}','now')",[format!("e{n}")])?;
    }
    crate::persistence::migrate(&conn, crate::persistence::MIGRATIONS)?;
    crate::persistence::hold::install(&conn)?;
    let first = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
    assert_eq!(first.indexed_through, 500);
    assert!(!first.index_ready);
    assert!(
        matches!(at(&conn, "r", 1001), Err(PersistError::Busy(_))),
        "never return a partial page as complete"
    );
    drop(conn);
    let mut conn = Connection::open(&path)?;
    crate::persistence::hold::install(&conn)?;
    let second = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
    assert_eq!(second.generation, first.generation);
    assert_eq!(second.indexed_through, 1000);
    let final_state = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
    assert!(final_state.index_ready);
    assert_eq!(final_state.indexed_through, 1001);
    assert_eq!(at(&conn, "r", 1001)?.len(), 1001);
    assert_eq!(
        backfill(&mut conn, &super::super::writer::Ctx::quiet())?,
        final_state
    );
    Ok(())
}

#[test]
fn test_013_05_missing_resume_state_refuses_a_complete_index() -> R {
    let conn = db()?;
    append(&conn, "task.started", json!({}))?;
    append(&conn, "run.resumed", json!({}))?;
    assert!(
        !status(&conn)?.index_ready,
        "missing resume fields are not inferred"
    );
    Ok(())
}

#[test]
fn test_013_05_missing_close_event_refuses_complete_backfill() -> R {
    let mut conn = db()?;
    conn.execute_batch("INSERT INTO iterations VALUES('closed-but-event-missing','r',1)")?;
    append(&conn, "task.started", json!({}))?;
    let result = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
    assert!(
        !result.index_ready,
        "a persisted closed engineering round needs its actual close event"
    );
    let diagnosed:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM events WHERE kind='runner.diagnostic' AND json_extract(payload_json,'$.reason')='ui_missing_close')",[],|r|r.get(0))?;
    assert!(diagnosed);
    Ok(())
}

#[test]
fn test_013_05_incomplete_missing_state_payload_is_not_guessed() -> R {
    let conn = db()?;
    append(&conn, "run.incomplete", json!({}))?;
    assert!(
        !status(&conn)?.index_ready,
        "run.incomplete requires its stop/from/remaining facts"
    );
    Ok(())
}

#[test]
fn test_013_05_ready_index_integrity_check_is_read_only() -> R {
    let mut conn = db()?;
    append(&conn, "task.started", json!({}))?;
    let before = status(&conn)?;
    assert!(before.index_ready);
    conn.execute_batch("PRAGMA query_only=ON")?;
    assert_eq!(
        backfill(&mut conn, &super::super::writer::Ctx::quiet())?,
        before
    );
    Ok(())
}

#[test]
fn test_013_05_damaged_backfill_retry_is_idempotent_without_duplicate_diagnostic() -> R {
    for missing_close in [true, false] {
        let mut conn = Connection::open_in_memory()?;
        crate::persistence::migrate(&conn, &crate::persistence::MIGRATIONS[..2])?;
        conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO runs(run_id,project_id,state,mode,created_at) VALUES('r','p','RUNNING','AUTO','now');")?;
        if missing_close {
            conn.execute_batch("INSERT INTO iterations VALUES('lost','r',1); INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e1','r','task.started','{}','now');")?;
        } else {
            conn.execute_batch("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e1','r','run.incomplete','{}','now');")?;
        }
        crate::persistence::migrate(&conn, crate::persistence::MIGRATIONS)?;
        crate::persistence::hold::install(&conn)?;
        let first = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
        assert!(!first.index_ready);
        let second = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
        assert!(!second.index_ready);
        assert_eq!(second.generation, first.generation);
        let third = backfill(&mut conn, &super::super::writer::Ctx::quiet())?;
        assert_eq!(third, second);
        let diagnostics: i64 = conn.query_row(
            "SELECT count(*) FROM events WHERE kind='runner.diagnostic'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(diagnostics, 1);
    }
    Ok(())
}
