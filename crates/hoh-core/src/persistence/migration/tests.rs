//! AC-008-02 foundation: the locked SQL Evolution contract, using bundled SQLite.

use super::*;
use rusqlite::types::Value;
use std::collections::BTreeMap;

type R = Result<(), Box<dyn std::error::Error>>;
const V2: &str = include_str!("../../../../../contracts/sql/002_spec_drafting.sql");

#[test]
fn test_008_02_existing_transaction_refusal_does_not_claim_or_perform_rollback() -> R {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "BEGIN; CREATE TABLE caller_owned(value); INSERT INTO caller_owned VALUES(7);",
    )?;
    let error = migrate(&conn, &MIGRATIONS[..2]).unwrap_err();
    assert!(
        !conn.is_autocommit(),
        "the caller still owns its transaction"
    );
    assert_eq!(
        conn.query_row("SELECT value FROM caller_owned", [], |r| r.get::<_, i64>(0))?,
        7
    );
    assert!(!error.to_string().contains("was rolled back"), "{error}");
    assert!(
        error
            .to_string()
            .contains("caller transaction was preserved")
    );
    conn.execute_batch("ROLLBACK")?;
    Ok(())
}

#[test]
fn test_008_02_post_commit_cleanup_failure_does_not_claim_rollback() -> R {
    let conn = Connection::open_in_memory()?;
    let committed = migrate(&conn, &MIGRATIONS[..2]);
    // Deterministic fault at the cleanup-result seam: a successful COMMIT cannot be undone by a failed restore.
    let error = combine_migration(
        committed,
        Err(PersistError::Pragma {
            name: "foreign_keys",
            expected: "1".into(),
            actual: "0".into(),
        }),
        2,
        "restore foreign_keys",
    )
    .unwrap_err();
    assert_eq!(user_version(&conn)?, 2);
    assert!(conn.is_autocommit());
    assert!(!error.to_string().contains("was rolled back"), "{error}");
    assert!(error.to_string().contains("restore foreign_keys"));
    Ok(())
}

fn populated() -> Result<Connection, Box<dyn std::error::Error>> {
    let conn = Connection::open_in_memory()?;
    migrate(&conn, &MIGRATIONS[..1])?;
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/project','now');
        INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
        INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now');
        INSERT INTO iterations VALUES('i','r',1);
        INSERT INTO tasks(task_id,run_id,iteration_id,spec_id,state,revision,role,writer,created_seq,user_value,unblock_value,risk_reduction,information_gain,cost_estimate,layer,wait_rounds,first_eligible_seq,attempt_count,next_retry_at,blocked_reason)
          VALUES('t','r','i','s','READY',7,'developer',1,8,1,2,3,4,5,0,9,3,2,'later','reason');
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t0','r','s','ACCEPTED','qa',1);
        INSERT INTO task_dependencies VALUES('t','t0');
        INSERT INTO requirements VALUES('REQ-X','s','title',1,'required');
        INSERT INTO acceptance_criteria VALUES('AC-X-01','REQ-X','s','body','unit');
        INSERT INTO task_requirements VALUES('t','REQ-X','s');
        INSERT INTO sessions VALUES('session','claude','developer','native',NULL,'v','{}');
        INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','session','PREPARED',1,2,3);
        INSERT INTO leases VALUES('p','writer','a',1,'later');
        INSERT INTO effects VALUES('effect','a','OBSERVED','ref',NULL);
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e','r','test','{}','now');")?;
    Ok(conn)
}

type Snapshot = BTreeMap<String, (Vec<String>, Vec<Vec<Value>>)>;
fn snapshot(conn: &Connection) -> rusqlite::Result<Snapshot> {
    let names = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    names
        .into_iter()
        .map(|table| {
            let mut stmt = conn.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid"))?;
            let names = stmt
                .column_names()
                .map(|s| s.to_string())
                .collect::<Vec<_>>();
            let rows = stmt
                .query_map([], |r| (0..names.len()).map(|i| r.get(i)).collect())?
                .collect::<rusqlite::Result<Vec<Vec<Value>>>>()?;
            Ok((table, (names, rows)))
        })
        .collect()
}

fn fk_on(conn: &Connection) -> R {
    assert!(conn.is_autocommit());
    assert_eq!(
        conn.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))?,
        1
    );
    Ok(())
}

#[test]
fn test_008_02_v1_to_v2_preserves_every_original_column_and_row() -> R {
    let conn = populated()?;
    let before = snapshot(&conn)?;
    assert_eq!(migrate(&conn, &MIGRATIONS[..2])?, 2);
    let after = snapshot(&conn)?;
    for (table, (columns, rows)) in &before {
        let (next_columns, next_rows) = &after[table];
        if table == "schema_migrations" {
            assert_eq!(&next_rows[..rows.len()], rows);
            assert_eq!(next_rows.len(), rows.len() + 1);
        } else if table == "tasks" {
            assert_eq!(&next_columns[..columns.len()], columns);
            assert_eq!(next_rows.len(), rows.len());
            for (old, new) in rows.iter().zip(next_rows) {
                assert_eq!(&new[..old.len()], old);
                assert_eq!(new.last(), Some(&Value::Text("work".into())));
            }
        } else {
            assert_eq!(before[table], after[table], "{table}");
        }
    }
    assert!(
        conn.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none()
    );
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    for name in [
        "tasks_run_state",
        "tasks_retry",
        "tasks_spec_draft_insert",
        "tasks_spec_draft_update",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_master WHERE name=?1",
                [name],
                |r| r.get::<_, i64>(0)
            )?,
            1,
            "{name}"
        );
    }
    assert!(matches!(
        migrate(&conn, &MIGRATIONS[..1]),
        Err(PersistError::SchemaTooNew {
            found: 2,
            supported: 1
        })
    ));
    fk_on(&conn)
}

#[test]
fn test_008_02_fk_off_precedes_begin_and_on_follows_every_outcome() -> R {
    let conn = populated()?;
    let sql = "CREATE TABLE setting(x CHECK(x=0)); INSERT INTO setting SELECT foreign_keys FROM pragma_foreign_keys; PRAGMA user_version=2;";
    assert_eq!(migrate(&conn, &[MIGRATIONS[0], (2, sql)])?, 2);
    fk_on(&conn)?;
    conn.pragma_update(None, "foreign_keys", false)?;
    assert_eq!(migrate(&conn, &[MIGRATIONS[0], (2, sql)])?, 2);
    fk_on(&conn)?;
    conn.pragma_update(None, "foreign_keys", false)?;
    assert!(matches!(
        migrate(&conn, &MIGRATIONS[..1]),
        Err(PersistError::SchemaTooNew { .. })
    ));
    fk_on(&conn)
}

#[test]
fn test_008_02_mid_rebuild_and_fk_check_failures_roll_back_to_v1() -> R {
    for script in [
        V2.replace(
            "ALTER TABLE tasks_new RENAME TO tasks;",
            "INSERT INTO missing VALUES(1); ALTER TABLE tasks_new RENAME TO tasks;",
        ),
        V2.replace(
            "PRAGMA user_version = 2;",
            "INSERT INTO task_dependencies VALUES('missing','t'); PRAGMA user_version = 2;",
        ),
    ] {
        let conn = populated()?;
        let before = snapshot(&conn)?;
        let err = migrate(&conn, &[MIGRATIONS[0], (2, &script)]).unwrap_err();
        assert!(
            matches!(err, PersistError::Migration { version: 2, .. }),
            "{err}"
        );
        if script.contains("'missing','t'") {
            assert!(err.to_string().contains("foreign_key_check"), "{err}");
        }
        assert_eq!(user_version(&conn)?, 1);
        assert_eq!(snapshot(&conn)?, before);
        fk_on(&conn)?;
        assert_eq!(migrate(&conn, &[MIGRATIONS[0], (2, V2)])?, 2);
        fk_on(&conn)?;
    }
    Ok(())
}

#[test]
fn test_008_02_sql_draft_constraints_keep_historical_null() -> R {
    let conn = Connection::open_in_memory()?;
    assert_eq!(migrate(&conn, &MIGRATIONS[..2])?, 2);
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now'); INSERT INTO runs(run_id,project_id,state,mode,created_at) VALUES('r','p','SPECIFYING','PLAN','now');")?;
    let insert = "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,kind) VALUES(?1,'r',?2,'READY','cto',?3,1,?4)";
    for (kind, writer) in [("work", 1), ("spec_draft", 0)] {
        let err = conn
            .execute(
                insert,
                rusqlite::params!["bad", Option::<String>::None, writer, kind],
            )
            .unwrap_err();
        assert!(err.to_string().contains("CHECK"), "{err}");
    }
    conn.execute(
        insert,
        rusqlite::params!["draft", Option::<String>::None, 1, "spec_draft"],
    )?;
    conn.execute_batch(
        "INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
        VALUES('draft-attempt','draft','PREPARED',1,1,1);
        INSERT INTO leases VALUES('p','writer','draft-attempt',1,'later');
        INSERT INTO effects VALUES('draft-effect','draft-attempt','PREPARED',NULL,NULL);",
    )?;
    let bad_child = conn
        .execute(
            "INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
        VALUES('orphan','missing','PREPARED',1,1,1)",
            [],
        )
        .unwrap_err();
    assert!(bad_child.to_string().contains("FOREIGN KEY"));
    assert!(
        conn.prepare("PRAGMA foreign_key_check")?
            .query([])?
            .next()?
            .is_none()
    );
    conn.execute_batch("INSERT INTO specs VALUES('s','p',printf('%064d',1),1); INSERT INTO specs VALUES('s2','p',printf('%064d',2),2);")?;
    let err = conn
        .execute(
            insert,
            rusqlite::params!["late", Option::<String>::None, 1, "spec_draft"],
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("project has an approved specification")
    );
    conn.execute(insert, rusqlite::params!["work", "s", 1, "work"])?;
    for sql in [
        "UPDATE tasks SET spec_id='s' WHERE task_id='draft'",
        "UPDATE tasks SET spec_id=NULL WHERE task_id='work'",
        "UPDATE tasks SET kind='spec_draft' WHERE task_id='work'",
        "UPDATE tasks SET kind='work' WHERE task_id='draft'",
    ] {
        let err = conn.execute(sql, []).unwrap_err();
        assert!(err.to_string().contains("keeps its kind"), "{sql}: {err}");
    }
    conn.execute("UPDATE tasks SET spec_id='s2' WHERE task_id='work'", [])?;
    conn.execute(
        "UPDATE tasks SET state='ACCEPTED' WHERE task_id='draft'",
        [],
    )?;
    assert_eq!(
        conn.query_row("SELECT spec_id FROM tasks WHERE task_id='draft'", [], |r| r
            .get::<_, Option<String>>(0))?,
        None
    );
    fk_on(&conn)
}

#[test]
fn test_008_02_commit_failure_and_busy_begin_restore_foreign_keys() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("migration.db");
    let writer = Connection::open(&path)?;
    writer.busy_timeout(Duration::ZERO)?;
    migrate(&writer, &MIGRATIONS[..1])?;
    let reader = Connection::open(&path)?;
    reader.execute_batch("BEGIN; SELECT * FROM tasks;")?;
    let before = snapshot(&writer)?;
    let err = migrate(&writer, &[MIGRATIONS[0], (2, V2)]).unwrap_err();
    assert!(err.to_string().contains("locked"), "{err}");
    assert_eq!(snapshot(&writer)?, before);
    fk_on(&writer)?;
    reader.execute_batch("ROLLBACK; BEGIN IMMEDIATE;")?;
    writer.pragma_update(None, "foreign_keys", false)?;
    assert!(migrate(&writer, &MIGRATIONS[..2]).is_err());
    fk_on(&writer)?;
    reader.execute_batch("ROLLBACK")?;
    assert_eq!(migrate(&writer, &MIGRATIONS[..2])?, 2);
    fk_on(&writer)
}

#[test]
fn test_008_02_waiting_opener_rereads_version_after_write_lock() -> R {
    use std::cell::RefCell;
    use std::sync::mpsc::{Receiver, Sender, channel};
    thread_local! {
        static BARRIER: RefCell<Option<(Sender<()>, Receiver<()>)>> = const { RefCell::new(None) };
    }
    fn busy(_: i32) -> bool {
        BARRIER.with(|barrier| {
            let pair = barrier.borrow_mut().take().expect("one blocked BEGIN");
            pair.0.send(()).unwrap();
            pair.1.recv_timeout(Duration::from_secs(10)).unwrap();
        });
        true
    }
    for ahead in [2, 3] {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("race.db");
        let first = Connection::open(&path)?;
        migrate(&first, &MIGRATIONS[..1])?;
        first.execute_batch("PRAGMA foreign_keys=OFF; BEGIN IMMEDIATE;")?;
        let (waiting, wait) = channel();
        let (release, resume) = channel();
        // Join even when the parent returns an error. Dropping a JoinHandle detaches the opener,
        // which is still blocked in its busy handler (M3T04O-100).
        std::thread::scope(|scope| -> R {
            let other = scope.spawn(move || {
                let mut second = Connection::open(path).unwrap();
                BARRIER.with(|b| *b.borrow_mut() = Some((waiting, resume)));
                second.busy_handler(Some(busy)).unwrap();
                let outcome = migrate(&second, &[MIGRATIONS[0], (2, V2)]);
                let fk: i64 = second
                    .pragma_query_value(None, "foreign_keys", |r| r.get(0))
                    .unwrap();
                (outcome, fk)
            });
            let waited = wait.recv_timeout(Duration::from_secs(10));
            let scripted = if waited.is_ok() {
                run_script(&first, V2).and_then(|_| {
                    first
                        .pragma_update(None, "user_version", ahead)
                        .map_err(|e| e.to_string())
                        .and_then(|_| {
                            first
                                .execute_batch("COMMIT; PRAGMA foreign_keys=ON;")
                                .map_err(|e| e.to_string())
                        })
                })
            } else {
                Ok(())
            };
            let _ = release.send(());
            let (outcome, fk) = other.join().expect("opener joined");
            waited?;
            scripted?;
            if ahead == 2 {
                assert_eq!(outcome?, 2);
            } else {
                assert!(
                    matches!(
                        outcome,
                        Err(PersistError::SchemaTooNew {
                            found: 3,
                            supported: 2
                        })
                    ),
                    "{outcome:?}"
                );
            }
            assert_eq!(fk, 1);
            Ok(())
        })?;
    }
    Ok(())
}

/// M3L-01: SIGKILL after migration 002 has copied `tasks` into `tasks_new`, and before it commits,
/// reopens at user_version 1 with the v1 row, then migrates to kind `work`.
#[test]
fn test_008_02_kill_during_migration_002_rolls_back_to_version_1() -> R {
    use std::os::unix::process::ExitStatusExt as _;
    if std::env::var_os("HOH_M3L01_CRASH").is_some() {
        let path = std::env::var("HOH_M3L01_CRASH_DB")?;
        let conn = Connection::open(path)?;
        let outcome = migrate(&conn, &MIGRATIONS[..2]);
        eprintln!("migration finished without a crash: {outcome:?}");
        std::process::exit(2);
    }
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("crash.db");
    let conn = Connection::open(&path)?;
    migrate(&conn, &MIGRATIONS[..1])?;
    conn.execute_batch(
        "INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/project','now');
         INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
         INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now');
         INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','READY','developer',1);",
    )?;
    drop(conn);
    let child = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "persistence::migration::tests::test_008_02_kill_during_migration_002_rolls_back_to_version_1",
            "--nocapture",
        ])
        .env("HOH_M3L01_CRASH", "1")
        .env("HOH_M3L01_CRASH_DB", &path)
        .output()?;
    assert_eq!(
        child.status.signal(),
        Some(9),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
    let conn = Connection::open(&path)?;
    assert_eq!(user_version(&conn)?, 1);
    assert_eq!(
        conn.query_row("SELECT task_id,state FROM tasks", [], |r| Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?
        )))?,
        ("t".to_owned(), "READY".to_owned())
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM pragma_table_info('tasks') WHERE name='kind'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0,
        "the rebuild must not be visible after the crash"
    );
    assert_eq!(
        conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
        "ok"
    );
    assert_eq!(migrate(&conn, &MIGRATIONS[..2])?, 2);
    assert_eq!(
        conn.query_row("SELECT kind FROM tasks WHERE task_id='t'", [], |r| r
            .get::<_, String>(0))?,
        "work"
    );
    let fk: i64 = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
    assert_eq!(fk, 1);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get::<_, i64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn test_013_05_migration_001_002_003_reopen_empty_indexes_and_foreign_keys() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("observability.db");
    {
        let conn = Connection::open(&path)?;
        migrate(&conn, &MIGRATIONS[..1])?;
        assert_eq!(user_version(&conn)?, 1);
        migrate(&conn, &MIGRATIONS[..2])?;
        assert_eq!(user_version(&conn)?, 2);
        assert_eq!(migrate(&conn, MIGRATIONS)?, 3);
        for table in [
            "ui_index_generations",
            "ui_iteration_ranges",
            "ui_event_index",
            "public_segments",
            "public_native_parts",
            "public_input_blocks",
        ] {
            assert_eq!(
                conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
        }
        fk_on(&conn)?;
    }
    let conn = Connection::open(&path)?;
    assert_eq!(migrate(&conn, MIGRATIONS)?, 3);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM schema_migrations", [], |r| r
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
    assert!(matches!(
        migrate(&conn, &MIGRATIONS[..2]),
        Err(PersistError::SchemaTooNew {
            found: 3,
            supported: 2
        })
    ));
    Ok(())
}

#[test]
fn test_011_05_segment_occurrences_reuse_artifacts_and_replay_after_upgrade() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("segments.db");
    {
        let conn = Connection::open(&path)?;
        migrate(&conn, &MIGRATIONS[..1])?;
        conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/project','now');
            INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
            INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','AUTO','now');
            INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq) VALUES('t','r','s','RUNNING','developer',1);
            INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('a','t','RUNNING',1,1,1);
            INSERT INTO input_queue VALUES('i1','r','STEER','developer','QUEUED','{}');
            INSERT INTO input_queue VALUES('i2','r','STEER','developer','QUEUED','{}');
            INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) VALUES('p','e','r','public.message','{}','now');
            INSERT INTO artifacts VALUES('object',printf('%064d',2),'objects/body',4,'text/plain');")?;
        assert_eq!(migrate(&conn, MIGRATIONS)?, 3);
        for (block, input, offset) in [("b1", "i1", 0), ("b2", "i2", 0), ("b1", "i1", 4)] {
            conn.execute("INSERT INTO public_segments VALUES('object','a',?1,?2,'text',?3,4,printf('%064d',2),1,1)",rusqlite::params![input,block,offset])?;
        }
        for (input, block) in [("i1", "b1"), ("i2", "b2")] {
            conn.execute(
                "INSERT INTO public_input_blocks VALUES(?1,?2,1)",
                rusqlite::params![input, block],
            )?;
        }
        conn.execute_batch(
            "INSERT INTO public_native_parts VALUES('a','stdout','native',0,'b1',0,1)",
        )?;
    }
    let conn = Connection::open(&path)?;
    assert_eq!(migrate(&conn, MIGRATIONS)?, 3);
    assert_eq!(conn.execute("INSERT OR IGNORE INTO public_segments VALUES('object','a','i1','b1','text',0,4,printf('%064d',2),1,1)",[])?,0);
    assert_eq!(
        conn.execute(
            "INSERT OR IGNORE INTO public_native_parts VALUES('a','stdout','native',0,'b1',0,1)",
            []
        )?,
        0
    );
    assert_eq!(
        conn.execute(
            "INSERT OR IGNORE INTO public_input_blocks VALUES('i1','b1',1)",
            []
        )?,
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM public_segments", [], |r| r
            .get::<_, i64>(0))?,
        3
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(DISTINCT artifact_id) FROM public_segments",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
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
