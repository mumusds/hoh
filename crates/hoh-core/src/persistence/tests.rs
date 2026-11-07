//! Unit and fault-injection tests of the store (M2-T01).

use super::writer::{Fault, Writer};
use super::*;
use crate::lifecycle::Lifecycle;
use crate::protocol::{AllVariants, RunState, TaskState};
use serde_json::json;
use std::time::Instant;

type R = Result<(), Box<dyn std::error::Error>>;

fn db_in(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(".hoh/state.db")
}

/// Options for stores opened by these tests.
fn test_options() -> Options {
    Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    }
}

/// A store whose writer has `fault` injected.
fn store_with(path: &Path, options: Options, fault: Fault) -> Result<Store, PersistError> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))
        .map_err(|e| PersistError::Storage(e.to_string()))?;
    let conn = Connection::open(path).map_err(sqlite_error)?;
    configure(&conn, &options, true)?;
    migrate(&conn, MIGRATIONS)?;
    let (committed, watermark) = tokio::sync::watch::channel(0);
    let writer = Writer::spawn_with(conn, &options, fault, committed)?;
    Ok(Store {
        path: path.to_path_buf(),
        options,
        writer,
        watermark,
    })
}

/// Project `p1` and run `r1` (CREATED, revision 0), written outside the store as a fixture.
fn seed(path: &Path) -> R {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
         INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES ('r1', 'p1', 'CREATED', 'AUTO', '2026-11-04T00:00:00Z');",
    )?;
    Ok(())
}

fn event(id: &str) -> NewEvent {
    NewEvent {
        project_id: "p1".to_owned(),
        event_id: id.to_owned(),
        run_id: Some("r1".to_owned()),
        kind: "run.state_changed".to_owned(),
        payload: json!({"id": id}),
    }
}

fn message(id: &str, event: usize) -> NewOutbox {
    NewOutbox {
        message_id: id.to_owned(),
        event,
        run_id: Some("r1".to_owned()),
        recipient_role: "commander".to_owned(),
        role_epoch: 1,
        payload: json!({"message": id}),
    }
}

fn cas(expected: i64, state: RunState) -> StateCas {
    StateCas {
        id: "r1".to_owned(),
        expected_revision: expected,
        change: Change::Run(state),
    }
}

/// (run state, run revision, events, outbox rows).
fn snapshot(path: &Path) -> Result<(String, i64, i64, i64), Box<dyn std::error::Error>> {
    let conn = Connection::open(path)?;
    let (state, revision) = conn.query_row(
        "SELECT state, revision FROM runs WHERE run_id = 'r1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let events = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
    let outbox = conn.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
    Ok((state, revision, events, outbox))
}

#[tokio::test]
async fn opening_creates_a_migrated_database_with_verified_settings() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    let reader = store.reader()?;
    let expected = Pragmas {
        journal_mode: "wal".to_owned(),
        foreign_keys: 1,
        busy_timeout_ms: 5000,
        synchronous: 2,
    };
    assert_eq!(reader.pragmas()?, expected);
    let (version, tables): (i64, i64) = reader.with(|c| {
        Ok((
            c.pragma_query_value(None, "user_version", |r| r.get(0))?,
            c.query_row(
                "SELECT count(*) FROM sqlite_master WHERE type = 'table'",
                [],
                |r| r.get(0),
            )?,
        ))
    })?;
    assert_eq!(version, 1);
    assert!(tables >= 30, "{tables} tables");
    // Readers cannot write.
    let refused = reader.with(|c| c.execute("INSERT INTO schema_migrations VALUES (9, 'x')", []));
    assert!(
        matches!(&refused, Err(PersistError::Storage(m)) if m.contains("readonly")),
        "{refused:?}"
    );
    // Only the database, its WAL and wal-index are left: the self-test cleaned up.
    let mut files: Vec<String> = std::fs::read_dir(dir.path().join(".hoh"))?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .collect();
    files.sort();
    assert_eq!(files, ["state.db", "state.db-shm", "state.db-wal"]);
    Ok(())
}

#[tokio::test]
async fn reopening_keeps_the_schema_and_a_newer_one_is_refused() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    drop(Store::open(&path, test_options())?);
    let store = Store::open(&path, test_options())?;
    let applied: i64 = store
        .reader()?
        .with(|c| c.query_row("SELECT count(*) FROM schema_migrations", [], |r| r.get(0)))?;
    assert_eq!(applied, 1, "migration 1 ran once");
    drop(store);
    Connection::open(&path)?.pragma_update(None, "user_version", 2)?;
    match Store::open(&path, test_options()) {
        Err(PersistError::SchemaTooNew {
            found: 2,
            supported: 1,
        }) => Ok(()),
        other => Err(format!("{other:?}").into()),
    }
}

#[tokio::test]
async fn a_failing_migration_is_rolled_back() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    drop(Store::open(&path, test_options())?);
    let broken = "BEGIN; CREATE TABLE extra(x); INSERT INTO missing VALUES (1); PRAGMA user_version = 2; COMMIT;";
    let migrations = [MIGRATIONS[0], (2, broken)];
    match Store::open_checked(&path, test_options(), &migrations) {
        Err(PersistError::Migration { version: 2, .. }) => {}
        other => return Err(format!("{other:?}").into()),
    }
    let conn = Connection::open(&path)?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let extra: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name = 'extra'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!((version, extra), (1, 0));
    // A script that runs but does not set its version is a failure too, and leaves no schema change (M2T01R-05).
    let silent = [MIGRATIONS[0], (2, "BEGIN; CREATE TABLE extra(x); COMMIT;")];
    match Store::open_checked(&path, test_options(), &silent) {
        Err(PersistError::Migration { version: 2, detail }) => {
            assert!(detail.contains("user_version is 1"), "{detail}")
        }
        other => return Err(format!("{other:?}").into()),
    }
    let schema = |path: &Path| -> Result<(i64, i64), Box<dyn std::error::Error>> {
        let conn = Connection::open(path)?;
        let version = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let extra = conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'extra'",
            [],
            |r| r.get(0),
        )?;
        Ok((version, extra))
    };
    assert_eq!(
        schema(&path)?,
        (1, 0),
        "the failed migration left schema changes"
    );
    // Comments before the script's transaction control do not hide it (M2T01R-09): each script is skipped past its
    // own BEGIN/COMMIT/END and then fails only for its missing version, leaving no schema change.
    for script in [
        "-- leading comment\nBEGIN; CREATE TABLE extra(x);",
        "CREATE TABLE extra(x); -- before commit\nCOMMIT;",
        "/* block */ BEGIN TRANSACTION; CREATE TABLE extra(x); /* before end */ END;",
    ] {
        match Store::open_checked(&path, test_options(), &[MIGRATIONS[0], (2, script)]) {
            Err(PersistError::Migration { version: 2, detail }) => {
                assert!(
                    detail.contains("user_version is 1 afterwards"),
                    "{script}: {detail}"
                )
            }
            other => return Err(format!("{script}: {other:?}").into()),
        }
        assert_eq!(schema(&path)?, (1, 0), "{script} left schema changes");
    }
    // Empty statements and a byte-order mark before transaction control do not hide it either (M2T01R-15).
    for script in [
        "CREATE TABLE extra(x); ; COMMIT;",
        "CREATE TABLE extra(x); /* c */ ; END;",
        "\u{feff}CREATE TABLE extra(x); \u{feff}COMMIT;",
    ] {
        match Store::open_checked(&path, test_options(), &[MIGRATIONS[0], (2, script)]) {
            Err(PersistError::Migration { version: 2, .. }) => {}
            other => return Err(format!("{script:?}: {other:?}").into()),
        }
        assert_eq!(schema(&path)?, (1, 0), "{script:?} left schema changes");
    }
    // A statement that ends the runner's transaction fails the migration.
    match Store::open_checked(
        &path,
        test_options(),
        &[MIGRATIONS[0], (2, "CREATE TABLE extra(x); ROLLBACK;")],
    ) {
        Err(PersistError::Migration { version: 2, detail }) => {
            assert!(detail.contains("not allowed in a migration"), "{detail}")
        }
        other => return Err(format!("{other:?}").into()),
    }
    assert_eq!(schema(&path)?, (1, 0));
    // Once fixed, the same migration applies.
    let fixed = [
        MIGRATIONS[0],
        (
            2,
            "BEGIN; CREATE TABLE extra(x); PRAGMA user_version = 2; COMMIT;",
        ),
    ];
    drop(Store::open_checked(&path, test_options(), &fixed)?);
    assert_eq!(schema(&path)?, (2, 1));
    Ok(())
}

#[test]
fn migration_statements_are_classified_by_their_keyword() -> R {
    // M2T01R-20: SQLite's read-only flag is not a statement class. BEGIN IMMEDIATE/EXCLUSIVE are not read-only but are
    // the script's own transaction control; REINDEX of a table without indexes is read-only but a legitimate step.
    let dir = tempfile::tempdir()?;
    for (version, script) in [
        (
            1,
            "BEGIN IMMEDIATE; CREATE TABLE a(x); PRAGMA user_version = 1; COMMIT;",
        ),
        (
            2,
            "BEGIN EXCLUSIVE TRANSACTION; CREATE TABLE b(x); REINDEX b; ANALYZE; PRAGMA user_version = 2; END;",
        ),
        // A byte-order mark, an empty statement or a comment before the script's own BEGIN does not hide it.
        (
            3,
            "\u{feff}BEGIN; CREATE TABLE c(x); PRAGMA user_version = 3; COMMIT;",
        ),
        (
            4,
            "; BEGIN IMMEDIATE; CREATE TABLE d(x); PRAGMA user_version = 4; ; COMMIT;",
        ),
        (
            5,
            "-- header\n/* note */ ; BEGIN; CREATE TABLE e(x); PRAGMA user_version = 5; END;",
        ),
    ] {
        let conn = Connection::open(dir.path().join(format!("ok{version}.db")))?;
        assert_eq!(migrate(&conn, &[(version, script)])?, version, "{script}");
    }
    // Statements that cannot run inside the runner's transaction, or would leave it, are refused unexecuted.
    for script in [
        "VACUUM;",
        "ATTACH 'x.db' AS x;",
        "SAVEPOINT s;",
        "RELEASE s;",
        "ROLLBACK;",
    ] {
        let conn = Connection::open(dir.path().join("refused.db"))?;
        let full = format!("CREATE TABLE c(x); {script} PRAGMA user_version = 1;");
        match migrate(&conn, &[(1, full.as_str())]) {
            Err(PersistError::Migration { version: 1, detail }) => {
                assert!(
                    detail.contains("not allowed in a migration"),
                    "{script}: {detail}"
                )
            }
            other => return Err(format!("{script}: {other:?}").into()),
        }
        let tables: i64 = conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get(0))?;
        assert_eq!(tables, 0, "{script} left schema changes");
    }
    Ok(())
}

#[test]
fn migration_scripts_keep_trigger_bodies_and_roll_back_a_failed_commit() -> R {
    let dir = tempfile::tempdir()?;
    let conn = Connection::open(dir.path().join("m.db"))?;
    conn.pragma_update(None, "foreign_keys", true)?;
    // A trigger's BEGIN ... END body is part of its CREATE statement, not transaction control.
    let trigger = "BEGIN; CREATE TABLE a(x); CREATE TABLE log(x); \
                   CREATE TRIGGER t AFTER INSERT ON a BEGIN INSERT INTO log VALUES (new.x); END; \
                   PRAGMA user_version = 1; COMMIT;";
    assert_eq!(migrate(&conn, &[(1, trigger)])?, 1);
    conn.execute("INSERT INTO a VALUES (7)", [])?;
    let logged: i64 = conn.query_row("SELECT x FROM log", [], |r| r.get(0))?;
    assert_eq!(logged, 7);
    // COMMIT itself fails on a deferred foreign key: the migration is rolled back and the connection is usable
    // (M2T01R-12).
    let deferred = "CREATE TABLE p(id INTEGER PRIMARY KEY); \
                    CREATE TABLE c(pid INTEGER REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED); \
                    INSERT INTO c VALUES (1); PRAGMA user_version = 2;";
    match migrate(&conn, &[(1, trigger), (2, deferred)]) {
        Err(PersistError::Migration { version: 2, .. }) => {}
        other => return Err(format!("{other:?}").into()),
    }
    assert!(
        conn.is_autocommit(),
        "the failed migration left its transaction open"
    );
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let tables: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('p', 'c')",
        [],
        |r| r.get(0),
    )?;
    assert_eq!((version, tables), (1, 0));
    Ok(())
}

#[test]
fn a_connection_with_a_wrong_setting_is_refused() -> R {
    let dir = tempfile::tempdir()?;
    let options = test_options();
    let wal = |name: &str| -> Result<Connection, Box<dyn std::error::Error>> {
        let conn = Connection::open(dir.path().join(name))?;
        configure(&conn, &options, true)?;
        Ok(conn)
    };
    let conn = wal("fk.db")?;
    conn.pragma_update(None, "foreign_keys", false)?;
    let conn2 = wal("busy.db")?;
    conn2.busy_timeout(Duration::from_millis(100))?;
    let conn3 = wal("sync.db")?;
    conn3.pragma_update(None, "synchronous", "NORMAL")?;
    let conn4 = Connection::open(dir.path().join("delete.db"))?;
    for (conn, setting) in [
        (&conn, "foreign_keys"),
        (&conn2, "busy_timeout"),
        (&conn3, "synchronous"),
        (&conn4, "journal_mode"),
    ] {
        match verify_pragmas(conn, options.busy_timeout) {
            Err(PersistError::Pragma { name, .. }) => assert_eq!(name, setting),
            other => return Err(format!("{setting}: {other:?}").into()),
        }
    }
    verify_pragmas(&wal("ok.db")?, options.busy_timeout)?;
    Ok(())
}

#[tokio::test]
async fn state_event_and_outbox_commit_together() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let done = store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering)],
            events: vec![event("e1"), event("e2")],
            outbox: vec![message("m1", 1)],
        })
        .await?;
    assert_eq!(done.revisions, vec![1]);
    let [s1, s2] = done.event_seqs[..] else {
        return Err(format!("{:?}", done.event_seqs).into());
    };
    assert!(s2 > s1);
    assert_eq!(snapshot(&path)?, ("DISCOVERING".to_owned(), 1, 2, 1));
    let (seq, state): (i64, String) = store.reader()?.with(|c| {
        c.query_row(
            "SELECT event_seq, state FROM outbox WHERE message_id = 'm1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    })?;
    assert_eq!((seq, state.as_str()), (s2, "PENDING"));
    // The next commit's events come after.
    let next = store
        .commit(Commit {
            cas: vec![cas(1, RunState::Specifying)],
            events: vec![event("e3")],
            outbox: vec![],
        })
        .await?;
    assert!(next.event_seqs[0] > s2);
    assert_eq!(next.revisions, vec![2]);
    Ok(())
}

#[tokio::test]
async fn any_violation_rolls_back_the_whole_commit() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    store
        .commit(Commit {
            cas: vec![],
            events: vec![event("existing")],
            outbox: vec![message("taken", 0)],
        })
        .await?;
    let before = snapshot(&path)?;
    let mut unknown_run = event("e-fk");
    unknown_run.run_id = Some("no-such-run".to_owned());
    // Every commit starts with a valid state change and event, then breaks one rule.
    let valid = || Commit {
        cas: vec![cas(0, RunState::Discovering)],
        events: vec![event("e-ok")],
        outbox: vec![],
    };
    let cases: Vec<(&str, Commit)> = vec![
        (
            // CREATED -> BLOCKED is in the table, but a blocked run needs a blocked_reason (CHECK).
            "CHECK blocked_reason",
            Commit {
                cas: vec![cas(0, RunState::Blocked)],
                ..valid()
            },
        ),
        (
            "FOREIGN KEY",
            Commit {
                events: vec![event("e-ok"), unknown_run],
                ..valid()
            },
        ),
        (
            "UNIQUE event_id",
            Commit {
                events: vec![event("e-ok"), event("existing")],
                ..valid()
            },
        ),
        (
            "UNIQUE message_id",
            Commit {
                outbox: vec![message("m-ok", 0), message("taken", 0)],
                ..valid()
            },
        ),
        (
            "outbox event index",
            Commit {
                outbox: vec![message("m-ok", 5)],
                ..valid()
            },
        ),
    ];
    for (name, commit) in cases {
        match store.commit(commit).await {
            Err(PersistError::IntegrityViolation(_)) => {}
            other => return Err(format!("{name}: {other:?}").into()),
        }
        assert_eq!(snapshot(&path)?, before, "{name} left a partial write");
    }
    match store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering), cas(7, RunState::Ready)],
            ..valid()
        })
        .await
    {
        Err(
            e @ PersistError::StaleRevision {
                expected: 7,
                actual: Some(1),
                ..
            },
        ) => {
            assert_eq!(e.rejection(), Some(RejectionReason::StaleRevision));
        }
        other => return Err(format!("stale revision: {other:?}").into()),
    }
    assert_eq!(
        snapshot(&path)?,
        before,
        "the stale commit left a partial write"
    );
    // The store still works afterwards.
    store.commit(valid()).await?;
    Ok(())
}

/// (state, revision, resume_state) of run r1.
fn run_row(path: &Path) -> Result<(String, i64, Option<String>), Box<dyn std::error::Error>> {
    Ok(Connection::open(path)?.query_row(
        "SELECT state, revision, resume_state FROM runs WHERE run_id = 'r1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

#[tokio::test]
async fn an_illegal_transition_is_refused_and_writes_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let before = snapshot(&path)?;
    for to in [RunState::Running, RunState::Completed, RunState::Paused] {
        match store
            .commit(Commit {
                cas: vec![cas(0, to)],
                events: vec![event("e-illegal")],
                outbox: vec![message("m-illegal", 0)],
            })
            .await
        {
            Err(e @ PersistError::IllegalTransition { .. }) => {
                assert_eq!(e.rejection(), Some(RejectionReason::IllegalTransition));
                assert!(
                    e.to_string()
                        .contains(&format!("CREATED -> {}", to.as_str())),
                    "{e}"
                );
            }
            other => return Err(format!("CREATED -> {to:?}: {other:?}").into()),
        }
        assert_eq!(snapshot(&path)?, before, "CREATED -> {to:?} left a write");
    }
    // A legal change in the same commit as an illegal one is rolled back with it.
    let mixed = Commit {
        cas: vec![cas(0, RunState::Discovering), cas(1, RunState::Completed)],
        events: vec![event("e-mixed")],
        outbox: vec![],
    };
    assert!(matches!(
        store.commit(mixed).await,
        Err(PersistError::IllegalTransition { .. })
    ));
    assert_eq!(snapshot(&path)?, before);
    // Tasks are checked against their own machine.
    Connection::open(&path)?.execute_batch(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('t1', 'r1', 's1', 'READY', 'developer', 1);",
    )?;
    let task = |to| StateCas {
        id: "t1".to_owned(),
        expected_revision: 0,
        change: Change::Task(to),
    };
    for to in [TaskState::Running, TaskState::Accepted] {
        let Err(PersistError::IllegalTransition {
            entity: Entity::Task,
            ..
        }) = store
            .commit(Commit {
                cas: vec![task(to)],
                events: vec![event("e-task")],
                outbox: vec![],
            })
            .await
        else {
            return Err(format!("READY -> {to:?} was not refused").into());
        };
    }
    assert_eq!(snapshot(&path)?, before);
    let done = store
        .commit(Commit {
            cas: vec![task(TaskState::Leased)],
            events: vec![event("e-task")],
            outbox: vec![],
        })
        .await?;
    assert_eq!(done.revisions, vec![1]);
    Ok(())
}

#[tokio::test]
async fn every_legal_run_edge_is_written_and_every_illegal_one_refused() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let conn = Connection::open(&path)?;
    for &from in RunState::ALL {
        for &to in RunState::ALL {
            // Place r1 in `from`, with a resume_state that admits a resume to `to` when `to` is a working state.
            let resume = (!crate::lifecycle::is_working(from) && crate::lifecycle::is_working(to))
                .then_some(to.as_str());
            let blocked = (from == RunState::Blocked).then_some("NEED_DECISION");
            conn.execute(
                "UPDATE runs SET state = ?1, revision = 0, resume_state = ?2, blocked_reason = ?3 WHERE run_id = 'r1'",
                rusqlite::params![from.as_str(), resume, blocked],
            )?;
            let result = store
                .commit(Commit {
                    cas: vec![cas(0, to)],
                    ..Commit::default()
                })
                .await;
            let legal = crate::lifecycle::check(from, to).is_ok();
            let working = crate::lifecycle::is_working(to);
            match (legal, to, result) {
                (true, _, Ok(done)) => {
                    assert_eq!(done.revisions, vec![1]);
                    // Blockers are facts that no move clears (M2T02R-02).
                    let reason: Option<String> = conn.query_row(
                        "SELECT blocked_reason FROM runs WHERE run_id = 'r1'",
                        [],
                        |r| r.get(0),
                    )?;
                    assert_eq!(reason.as_deref(), blocked, "{from:?} -> {to:?}");
                }
                // A blocked run needs its reason, which this commit does not carry (ADR-011 §2).
                (true, RunState::Blocked, Err(PersistError::IntegrityViolation(_))) => {}
                // A blocked run does not work again while its blockers are recorded.
                (true, _, Err(PersistError::IllegalTransition { why, .. }))
                    if from == RunState::Blocked && working && why.contains("blockers") => {}
                (false, _, Err(PersistError::IllegalTransition { .. })) => {}
                (_, _, other) => return Err(format!("{from:?} -> {to:?}: {other:?}").into()),
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn blockers_survive_a_pause_and_hold_back_work() -> R {
    // M2T02R-02: a run BLOCKED on NEED_DECISION that is paused and resumed stays blocked; it works again only once
    // its blockers are removed (by the blocking mechanism; here by hand).
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "UPDATE runs SET state = 'BLOCKED', resume_state = 'RUNNING', blocked_reason = 'NEED_DECISION',
         blocked_reasons_json = '[\"NEED_DECISION\"]' WHERE run_id = 'r1'",
    )?;
    let step = |revision, to| {
        store.commit(Commit {
            cas: vec![cas(revision, to)],
            ..Commit::default()
        })
    };
    step(0, RunState::PauseRequested).await?;
    step(1, RunState::Paused).await?;
    let blockers = |conn: &Connection| -> rusqlite::Result<(Option<String>, String)> {
        conn.query_row(
            "SELECT blocked_reason, blocked_reasons_json FROM runs WHERE run_id = 'r1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    };
    assert_eq!(
        blockers(&conn)?,
        (
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    match step(2, RunState::Running).await {
        Err(PersistError::IllegalTransition { why, .. }) => {
            assert!(why.contains("blockers"), "{why}")
        }
        other => return Err(format!("PAUSED -> RUNNING with blockers: {other:?}").into()),
    }
    step(2, RunState::Blocked).await?;
    assert_eq!(
        run_row(&path)?,
        ("BLOCKED".to_owned(), 3, Some("RUNNING".to_owned()))
    );
    conn.execute_batch(
        "UPDATE runs SET blocked_reason = NULL, blocked_reasons_json = '[]', state = 'PAUSED' WHERE run_id = 'r1'",
    )?;
    step(3, RunState::Running).await?;
    assert_eq!(run_row(&path)?, ("RUNNING".to_owned(), 4, None));
    // The blocker set is judged by its JSON content: any empty array is empty, anything but an array is blocked.
    for (json, resumes) in [
        ("[ ]", true),
        ("[\n]", true),
        ("{}", false),
        ("[\"NEED_DECISION\"]", false),
    ] {
        conn.execute(
            "UPDATE runs SET state = 'PAUSED', revision = 0, resume_state = 'RUNNING', blocked_reason = NULL, \
             blocked_reasons_json = ?1 WHERE run_id = 'r1'",
            [json],
        )?;
        let result = step(0, RunState::Running).await;
        assert_eq!(result.is_ok(), resumes, "{json:?}: {result:?}");
    }
    // Tasks likewise: a blocked task with a recorded reason becomes ready only once the reason is removed.
    conn.execute_batch(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq, blocked_reason)
           VALUES ('t1', 'r1', 's1', 'BLOCKED', 'developer', 1, 'BLOCKED_AUTH');",
    )?;
    let task = |to| StateCas {
        id: "t1".to_owned(),
        expected_revision: 0,
        change: Change::Task(to),
    };
    for to in [TaskState::Ready, TaskState::Pending] {
        let Err(PersistError::IllegalTransition { .. }) = store
            .commit(Commit {
                cas: vec![task(to)],
                ..Commit::default()
            })
            .await
        else {
            return Err(format!("blocked task -> {to:?} was not refused").into());
        };
    }
    store
        .commit(Commit {
            cas: vec![task(TaskState::Cancelled)],
            ..Commit::default()
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_suspended_run_resumes_only_to_its_saved_state() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let step = |revision, to| {
        store.commit(Commit {
            cas: vec![cas(revision, to)],
            ..Commit::default()
        })
    };
    for (revision, to) in [
        (0, RunState::Discovering),
        (1, RunState::WaitingExternal),
        (2, RunState::PauseRequested),
        (3, RunState::Paused),
    ] {
        step(revision, to).await?;
        assert_eq!(run_row(&path)?.0, to.as_str());
    }
    // Entered from DISCOVERING; kept through the other suspended states.
    assert_eq!(run_row(&path)?.2.as_deref(), Some("DISCOVERING"));
    for wrong in [RunState::Running, RunState::Created, RunState::Verifying] {
        match step(4, wrong).await {
            Err(PersistError::IllegalTransition { .. }) => {}
            other => return Err(format!("PAUSED -> {wrong:?}: {other:?}").into()),
        }
    }
    step(4, RunState::Discovering).await?;
    assert_eq!(run_row(&path)?, ("DISCOVERING".to_owned(), 5, None));
    step(5, RunState::Incomplete).await?;
    assert_eq!(run_row(&path)?.2.as_deref(), Some("DISCOVERING"));
    // A run that reaches a terminal state keeps no resume_state.
    step(6, RunState::Failed).await?;
    assert_eq!(run_row(&path)?, ("FAILED".to_owned(), 7, None));
    Ok(())
}

#[tokio::test]
async fn a_revision_only_change_keeps_the_state() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let touch = |expected| StateCas {
        id: "r1".to_owned(),
        expected_revision: expected,
        change: Change::Touch(Entity::Run),
    };
    let done = store
        .commit(Commit {
            cas: vec![touch(0)],
            ..Commit::default()
        })
        .await?;
    assert_eq!(done.revisions, vec![1]);
    assert_eq!(run_row(&path)?, ("CREATED".to_owned(), 1, None));
    assert!(matches!(
        store
            .commit(Commit {
                cas: vec![touch(0)],
                ..Commit::default()
            })
            .await,
        Err(PersistError::StaleRevision {
            expected: 0,
            actual: Some(1),
            ..
        })
    ));
    Ok(())
}

#[tokio::test]
async fn a_locked_database_is_busy_and_changes_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let options = Options {
        busy_timeout: Duration::from_millis(300),
        ..test_options()
    };
    let store = Store::open(&path, options)?;
    seed(&path)?;
    let before = snapshot(&path)?;
    let external = Connection::open(&path)?;
    external.execute_batch("BEGIN IMMEDIATE")?;
    let started = Instant::now();
    let result = store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering)],
            events: vec![event("e1")],
            outbox: vec![],
        })
        .await;
    let waited = started.elapsed();
    match &result {
        Err(e @ PersistError::Busy(_)) => {
            assert!(e.retryable());
            assert_eq!(e.error_code(), Some(ErrorCode::DbBusy));
        }
        other => return Err(format!("{other:?}").into()),
    }
    assert!(
        waited >= Duration::from_millis(300) && waited < Duration::from_secs(3),
        "{waited:?}"
    );
    external.execute_batch("ROLLBACK")?;
    assert_eq!(snapshot(&path)?, before);
    store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering)],
            events: vec![event("e1")],
            outbox: vec![],
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_full_queue_holds_callers_back_and_loses_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let options = Options {
        busy_timeout: Duration::from_millis(200),
        queue: 1,
        ..test_options()
    };
    let store = std::sync::Arc::new(store_with(
        &path,
        options,
        Fault::Stall(Duration::from_millis(800)),
    )?);
    seed(&path)?;
    let only_event = |id: &str| Commit {
        cas: vec![],
        events: vec![event(id)],
        outbox: vec![],
    };
    let first = tokio::spawn({
        let store = store.clone();
        let c = only_event("first");
        async move { store.commit(c).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await; // the writer took `first` and stalls
    let second = tokio::spawn({
        let store = store.clone();
        let c = only_event("second");
        async move { store.commit(c).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await; // `second` fills the queue
    let started = Instant::now();
    let third = store.commit(only_event("third")).await;
    assert!(
        matches!(&third, Err(e @ PersistError::Busy(_)) if e.retryable()),
        "{third:?}"
    );
    assert!(started.elapsed() >= Duration::from_millis(200));
    first.await??;
    second.await??;
    let ids: Vec<String> = store.reader()?.with(|c| {
        let mut stmt = c.prepare("SELECT event_id FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    })?;
    assert_eq!(
        ids,
        ["first", "second"],
        "every accepted commit is stored, the refused one is not"
    );
    Ok(())
}

const CRASH_DB: &str = "HOH_TEST_CRASH_DB";

#[tokio::test]
async fn a_crash_inside_a_transaction_leaves_no_partial_commit() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    drop(Store::open(&path, test_options())?);
    seed(&path)?;
    let before = snapshot(&path)?;
    // The child is this test binary running only `crash_child`, which aborts inside the transaction.
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            "persistence::tests::crash_child",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(CRASH_DB, &path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(
        status.signal(),
        Some(6),
        "the child should have aborted: {status:?}"
    );
    assert_eq!(snapshot(&path)?, before);
    let check: String =
        Connection::open(&path)?.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    assert_eq!(check, "ok");
    // A store opened afterwards commits normally.
    let store = Store::open(&path, test_options())?;
    store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering)],
            events: vec![event("after")],
            outbox: vec![message("m-after", 0)],
        })
        .await?;
    assert_eq!(snapshot(&path)?, ("DISCOVERING".to_owned(), 1, 1, 1));
    Ok(())
}

/// Helper process for the crash test; does nothing unless started by it.
#[tokio::test]
#[ignore = "helper process of a_crash_inside_a_transaction_leaves_no_partial_commit"]
async fn crash_child() -> R {
    let Some(path) = std::env::var_os(CRASH_DB) else {
        return Ok(());
    };
    let store = store_with(Path::new(&path), test_options(), Fault::AbortAfterEvents)?;
    let _ = store
        .commit(Commit {
            cas: vec![cas(0, RunState::Discovering)],
            events: vec![event("lost-1"), event("lost-2")],
            outbox: vec![message("lost", 0)],
        })
        .await;
    Err("the writer should have aborted the process".into())
}

#[test]
fn links_and_parent_components_are_resolved_before_the_location_check() -> R {
    // M2T01R-01: the check must look at where the database really goes, not at the path as written.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    std::fs::create_dir_all(root.join("local"))?;
    std::fs::create_dir_all(root.join("network"))?;
    let mountinfo = format!(
        "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:50 / {} rw,relatime - nfs4 server:/export rw\n",
        root.join("network").display()
    );
    // A dangling link for the database file itself, and a linked directory.
    std::os::unix::fs::symlink(root.join("network/state.db"), root.join("local/state.db"))?;
    std::os::unix::fs::symlink("../network", root.join("local/linked"))?;
    for path in [
        root.join("local/state.db"),
        root.join("local/linked/.hoh/state.db"),
        root.join("local/missing/../../network/state.db"),
    ] {
        match Store::open_with_mountinfo(&path, test_options(), &mountinfo) {
            Err(PersistError::OnNetworkFs { fstype, .. }) => {
                assert_eq!(fstype, "nfs4", "{}", path.display())
            }
            other => return Err(format!("{}: {other:?}", path.display()).into()),
        }
    }
    let created: Vec<_> = std::fs::read_dir(root.join("network"))?.collect();
    assert!(
        created.is_empty(),
        "files were created on the network mount: {created:?}"
    );
    assert!(!root.join("local/missing").exists());
    // The same kind of link to local storage is followed: the database is created at the link's target.
    std::fs::remove_file(root.join("local/state.db"))?;
    std::fs::create_dir_all(root.join("elsewhere"))?;
    std::os::unix::fs::symlink(root.join("elsewhere/state.db"), root.join("local/state.db"))?;
    let store =
        Store::open_with_mountinfo(&root.join("local/state.db"), test_options(), &mountinfo)?;
    assert_eq!(store.path(), root.join("elsewhere/state.db"));
    assert!(root.join("elsewhere/state.db").exists());
    Ok(())
}

#[test]
fn a_path_swapped_after_the_check_is_refused() -> R {
    // M2T01R-08: between the location check and the open, the checked file or its directory is replaced by a link to
    // a network mount. Nothing may be opened or created there.
    for swap in ["file", "directory"] {
        let dir = tempfile::tempdir()?;
        let root = dir.path().canonicalize()?;
        std::fs::create_dir_all(root.join("local"))?;
        std::fs::create_dir_all(root.join("network"))?;
        let mountinfo = format!(
            "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:50 / {} rw,relatime - nfs4 server:/export rw\n",
            root.join("network").display()
        );
        let swap_in = || {
            let replaced = match swap {
                "file" => std::os::unix::fs::symlink(
                    root.join("network/state.db"),
                    root.join("local/state.db"),
                ),
                _ => std::fs::rename(root.join("local"), root.join("moved")).and_then(|()| {
                    std::os::unix::fs::symlink(root.join("network"), root.join("local"))
                }),
            };
            assert!(replaced.is_ok(), "{swap}: {replaced:?}");
        };
        let opened = Store::open_inner(
            &root.join("local/state.db"),
            test_options(),
            &mountinfo,
            MIGRATIONS,
            &swap_in,
        );
        match (swap, &opened) {
            // A link in place of the directory is caught before the self-test touches it.
            ("directory", Err(PersistError::Storage(m))) => {
                assert!(m.contains("changed while it was being checked"), "{m}")
            }
            ("file", Err(_)) => {}
            _ => return Err(format!("{swap}: {opened:?}").into()),
        }
        let created: Vec<_> = std::fs::read_dir(root.join("network"))?.collect();
        assert!(
            created.is_empty(),
            "{swap}: files were created on the network mount: {created:?}"
        );
    }
    Ok(())
}

#[test]
fn a_database_on_a_network_file_system_is_refused_before_any_file_exists() -> R {
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let mountinfo = format!(
        "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:50 / {} rw,relatime - nfs4 server:/export rw\n",
        root.display()
    );
    let path = root.join("project/.hoh/state.db");
    match Store::open_with_mountinfo(&path, test_options(), &mountinfo) {
        Err(e @ PersistError::OnNetworkFs { .. }) => {
            assert_eq!(e.error_code(), Some(ErrorCode::DbOnNetworkFs));
            assert!(e.to_string().contains("DB_ON_NETWORK_FS"), "{e}");
        }
        other => return Err(format!("{other:?}").into()),
    }
    assert!(!root.join("project").exists(), "nothing may be created");
    Ok(())
}

/// The values of `CHECK(state IN (...))` in the CREATE TABLE statement of `table`.
fn state_check(sql: &str, table: &str) -> Result<Vec<String>, String> {
    let start = sql
        .find(&format!("CREATE TABLE {table}("))
        .ok_or(format!("no table {table}"))?;
    let statement = &sql[start..start + sql[start..].find(");").ok_or("unterminated")?];
    let open = "CHECK(state IN (";
    let list = &statement[statement
        .find(open)
        .ok_or(format!("{table}: no state CHECK"))?
        + open.len()..];
    let list = &list[..list.find(')').ok_or("unterminated list")?];
    Ok(list
        .split(',')
        .map(|v| v.trim().trim_matches('\'').to_owned())
        .collect())
}

fn names<S: Lifecycle>() -> Vec<String> {
    S::ALL.iter().map(|s| s.as_str().to_owned()).collect()
}

#[test]
fn test_036_01_state_sets_equal_the_sql_check_enumerations() -> R {
    // The script the store migrates with, i.e. contracts/sql/001_initial.sql.
    let [(1, sql)] = MIGRATIONS else {
        return Err("expected exactly migration 1".into());
    };
    let sorted = |mut v: Vec<String>| {
        v.sort();
        v
    };
    for (table, table_names, count) in [
        ("runs", names::<RunState>(), 15),
        ("tasks", names::<TaskState>(), 9),
        ("attempts", names::<crate::lifecycle::AttemptState>(), 7),
        ("input_queue", names::<crate::lifecycle::InputState>(), 4),
    ] {
        let sql_names = state_check(sql, table)?;
        assert_eq!(sql_names.len(), count, "{table}: {sql_names:?}");
        assert_eq!(
            sorted(sql_names),
            sorted(table_names),
            "{table}: SQL CHECK and transition table differ"
        );
    }
    Ok(())
}

#[test]
fn allowed_values_are_read_from_the_schema() {
    let runs = allowed_values("runs", "state").expect("runs.state");
    assert_eq!(runs.len(), 15);
    assert_eq!(
        (runs[0].as_str(), runs[14].as_str()),
        ("CREATED", "COMPLETED")
    );
    assert_eq!(
        allowed_values("runs", "mode"),
        Some(vec![
            "PLAN".to_owned(),
            "BUILD".to_owned(),
            "AUTO".to_owned()
        ])
    );
    assert_eq!(allowed_values("tasks", "state").map(|v| v.len()), Some(9));
    assert_eq!(allowed_values("runs", "no_such_column"), None);
    assert_eq!(allowed_values("no_such_table", "state"), None);
}

/// `scratch_task` gives an empty scratch database (`hoh doctor --live`) a project, spec, RUNNING run and LEASED task
/// to hang attempts on, and is refused, writing nothing, wherever a run exists: it never stands in for the command
/// path of a real project.
#[tokio::test]
async fn a_scratch_task_is_only_for_an_empty_database() -> R {
    let dir = tempfile::tempdir()?;
    let store = Store::open(&db_in(&dir), test_options())?;
    store.scratch_task("p1", "/repo", "R", "T1").await?;
    store
        .create_attempt(NewAttempt {
            attempt_id: "A1".to_owned(),
            task_id: "T1".to_owned(),
            session_id: None,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 0,
            agent: Some("claude".to_owned()),
        })
        .await?;
    let reader = store.reader()?;
    let states: (String, String, i64) = reader.with(|c| {
        Ok((
            c.query_row("SELECT state FROM runs WHERE run_id = 'R'", [], |r| {
                r.get(0)
            })?,
            c.query_row("SELECT state FROM tasks WHERE task_id = 'T1'", [], |r| {
                r.get(0)
            })?,
            c.query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?,
        ))
    })?;
    assert_eq!(states, ("RUNNING".to_owned(), "LEASED".to_owned(), 1));
    let again = store.scratch_task("p2", "/other", "R2", "T2").await;
    assert!(
        matches!(again, Err(PersistError::NotScratch(_))),
        "{again:?}"
    );
    let counts: (i64, i64, i64) = reader.with(|c| {
        Ok((
            c.query_row("SELECT count(*) FROM projects", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM tasks", [], |r| r.get(0))?,
        ))
    })?;
    assert_eq!(counts, (1, 1, 1), "nothing written by the refused call");
    Ok(())
}

/// A PauseRun for `r1` with key `key` and payload hash `hash`.
fn pause(key: &str, hash: &str, expected: i64) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{key}-{hash}"),
        idempotency_key: key.to_owned(),
        kind: "PauseRun".to_owned(),
        run_id: "r1".to_owned(),
        expected_revision: expected,
        payload: json!({"reason": "test"}),
        payload_sha256: hash.repeat(64),
        target: RunTarget::To(RunState::PauseRequested),
        event: event(&format!("e-{key}-{hash}")),
        outbox: Some(message(&format!("m-{key}-{hash}"), 0)),
    }
}

/// (commands, events, outbox) row counts.
fn command_rows(store: &Store) -> Result<(i64, i64, i64), PersistError> {
    store.reader()?.with(|c| {
        c.query_row(
            "SELECT (SELECT count(*) FROM commands), (SELECT count(*) FROM events), (SELECT count(*) FROM outbox)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
    })
}

/// The same key and payload apply once and replay the stored result byte for byte; another payload under the key is
/// KEY_REUSE and writes nothing (REQ-022, REQ-041).
#[tokio::test]
async fn a_command_applies_once_per_key_and_another_payload_is_key_reuse() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let first = store.submit_command(pause("k", "a", 0)).await?;
    let CommandOutcome::Accepted {
        result_json,
        revision,
        ..
    } = &first
    else {
        return Err(format!("{first:?}").into());
    };
    assert_eq!(*revision, 1);
    let body: serde_json::Value = serde_json::from_str(result_json)?;
    assert_eq!(
        body,
        json!({"command_id": "c-k-a", "run_id": "r1", "revision": 1, "status": "ACCEPTED"})
    );
    let again = store.submit_command(pause("k", "a", 0)).await?;
    assert_eq!(
        again,
        CommandOutcome::Replayed {
            result_json: result_json.clone()
        }
    );
    let reuse = store.submit_command(pause("k", "b", 1)).await;
    assert!(
        matches!(&reuse, Err(PersistError::KeyReuse(key)) if key == "k"),
        "{reuse:?}"
    );
    assert_eq!(
        reuse.err().and_then(|e| e.rejection()),
        Some(crate::protocol::RejectionReason::KeyReuse)
    );
    assert_eq!(command_rows(&store)?, (1, 1, 1));
    assert_eq!(snapshot(&path)?.0, "PAUSE_REQUESTED");
    Ok(())
}

/// A commit that fails as a full disk would is retryable DB_FULL and leaves nothing: no command, event or outbox row,
/// the run unchanged, the watermark where it was; the same command then applies (REQ-036, CR-006).
#[tokio::test]
async fn a_failed_commit_is_retryable_and_leaves_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let full = Arc::new(FullDisk(std::sync::atomic::AtomicBool::new(true)));
    let options = Options {
        faults: full.clone(),
        ..test_options()
    };
    let store = Store::open(&path, options)?;
    seed(&path)?;
    let watermark = store.watermark();
    let before = *watermark.borrow();
    let failed = store.submit_command(pause("k", "a", 0)).await;
    let error = failed.err().ok_or("the commit did not fail")?;
    assert!(matches!(error, PersistError::Full(_)), "{error:?}");
    assert!(error.retryable());
    assert_eq!(error.error_code(), Some(crate::protocol::ErrorCode::DbFull));
    assert_eq!(command_rows(&store)?, (0, 0, 0));
    assert_eq!(snapshot(&path)?.0, "CREATED");
    assert_eq!(*watermark.borrow(), before);
    full.0.store(false, std::sync::atomic::Ordering::SeqCst);
    let applied = store.submit_command(pause("k", "a", 0)).await?;
    assert!(
        matches!(applied, CommandOutcome::Accepted { revision: 1, .. }),
        "{applied:?}"
    );
    assert_eq!(command_rows(&store)?, (1, 1, 1));
    Ok(())
}

/// The watermark moves to the event's seq only once its transaction has committed (REQ-036).
#[tokio::test]
async fn the_watermark_follows_committed_events() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let mut watermark = store.watermark();
    assert_eq!(*watermark.borrow_and_update(), 0);
    let CommandOutcome::Accepted { event_seq, .. } =
        store.submit_command(pause("k", "a", 0)).await?
    else {
        return Err("not applied".into());
    };
    tokio::time::timeout(Duration::from_secs(5), watermark.changed()).await??;
    assert_eq!(*watermark.borrow(), event_seq);
    let reader = store.reader()?;
    assert_eq!(reader.max_seq()?, event_seq);
    let events = reader.events_after(0, 10)?;
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0].seq, events[0].kind.as_str()),
        (event_seq, "run.state_changed")
    );
    let (run, seen) = reader.snapshot("r1")?.ok_or("no run")?;
    assert_eq!(
        (run.state.as_str(), run.revision, seen),
        ("PAUSE_REQUESTED", 1, event_seq)
    );
    assert!(reader.snapshot("missing")?.is_none());
    assert!(reader.refuses_writes());
    Ok(())
}

/// Each host start checks the database and takes the next host epoch of its project, made on the first start
/// (REQ-041).
#[tokio::test]
async fn each_host_start_takes_the_next_epoch() -> R {
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    let first = store.begin_host("p-new", "/somewhere").await?;
    assert_eq!(
        first,
        HostStart {
            project_id: "p-new".to_owned(),
            host_epoch: 1
        }
    );
    // A later start keeps the project made first, whatever id it proposes.
    let second = store.begin_host("p-other", "/somewhere").await?;
    assert_eq!(
        second,
        HostStart {
            project_id: "p-new".to_owned(),
            host_epoch: 2
        }
    );
    let pragmas = store.writer_pragmas().await?;
    assert_eq!(pragmas, store.reader()?.pragmas()?);
    Ok(())
}

/// A disk that is full while its flag is set: the test's fault hook (the product's does nothing, CR-006).
#[derive(Debug)]
struct FullDisk(std::sync::atomic::AtomicBool);

impl FaultHooks for FullDisk {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            Err(sqlite_error(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
                Some("injected".to_owned()),
            )))
        } else {
            Ok(())
        }
    }
}

/// SQLITE_FULL is DB_FULL and retryable, as the host answers it (CR-006).
#[test]
fn a_full_disk_is_db_full_and_retryable() {
    let full = sqlite_error(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
        None,
    ));
    assert!(matches!(full, PersistError::Full(_)), "{full:?}");
    assert!(full.retryable());
    assert_eq!(full.error_code(), Some(crate::protocol::ErrorCode::DbFull));
}

/// What a store keeps is dropped only after the writer ended, so after every queued write: a commit held in its hook
/// keeps it while it waits, and the writer ends only once it is done (ADR-016 decision 12).
#[tokio::test]
async fn what_the_writer_keeps_goes_after_the_last_write() -> R {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    #[derive(Debug)]
    struct Held(Arc<AtomicBool>);
    impl Drop for Held {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    #[derive(Debug)]
    struct Latched(std::sync::Mutex<(mpsc::Sender<()>, mpsc::Receiver<()>)>);
    impl FaultHooks for Latched {
        fn before_commit(&self) -> Result<(), PersistError> {
            let latch = self
                .0
                .lock()
                .map_err(|_| PersistError::Storage("latch".to_owned()))?;
            let _ = latch.0.send(());
            let _ = latch.1.recv();
            Ok(())
        }
    }
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Arc::new(Store::open(
        &path,
        Options {
            faults: Arc::new(Latched(std::sync::Mutex::new((entered_tx, release_rx)))),
            ..test_options()
        },
    )?);
    seed(&path)?;
    let dropped = Arc::new(AtomicBool::new(false));
    store
        .keep_until_closed(Box::new(Held(dropped.clone())))
        .await?;
    let ended = store.writer_ended();
    let writer = store.clone();
    let command = tokio::spawn(async move { writer.submit_command(pause("k", "a", 0)).await });
    drop(store);
    // Waited for off the runtime's thread, so the command's task can reach the writer.
    tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(5))).await??;
    // The commit waits in its hook; the command's task holds the last handle.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !dropped.load(Ordering::SeqCst),
        "kept item dropped while a write was pending"
    );
    release.send(())?;
    assert!(matches!(command.await??, CommandOutcome::Accepted { .. }));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), ended).await?,
        "a writer that finished read as failed"
    );
    assert!(dropped.load(Ordering::SeqCst));
    let (state, revision, events, outbox) = snapshot(&path)?;
    assert_eq!(
        (state.as_str(), revision, events, outbox),
        ("PAUSE_REQUESTED", 1, 1, 1)
    );
    Ok(())
}

#[tokio::test]
async fn a_write_of_a_host_whose_epoch_moved_on_changes_nothing() -> R {
    // REQ-041 failure mode 4, ADR-019 decision 2: host A began at epoch 1, host B on the same database at 2; A's next
    // write is an IntegrityViolation and changes 0 rows, B's goes on.
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let old = Store::open(&path, test_options())?;
    seed(&path)?;
    let first = old.begin_host("p1", "/repo").await?;
    let new = Store::open(&path, test_options())?;
    let second = new.begin_host("p1", "/repo").await?;
    assert_eq!(
        (first.host_epoch + 1, &second.project_id),
        (second.host_epoch, &first.project_id)
    );
    let refused = old
        .commit(Commit {
            events: vec![event("old")],
            ..Commit::default()
        })
        .await
        .expect_err("fenced");
    assert!(
        matches!(&refused, PersistError::IntegrityViolation(m) if m.contains("0 rows")),
        "{refused:?}"
    );
    new.commit(Commit {
        events: vec![event("new")],
        ..Commit::default()
    })
    .await?;
    let conn = Connection::open(&path)?;
    let ids: Vec<String> = conn
        .prepare("SELECT event_id FROM events ORDER BY seq")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(ids, ["new"]);
    Ok(())
}

#[tokio::test]
async fn a_store_whose_host_never_began_is_not_fenced() -> R {
    // ADR-019 decision 2: only a store that began a host is fenced (a scratch store, a test store).
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    let other = Store::open(&path, test_options())?;
    other.begin_host("p1", "/repo").await?;
    store
        .commit(Commit {
            events: vec![event("free")],
            ..Commit::default()
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn an_attempt_left_unfinished_is_disposed_of_again_with_events_of_its_own() -> R {
    // ADR-019 decision 5: a redelivered (or unreaped) attempt is still unfinished at the next start; its second
    // disposition must not collide with the first's event ids.
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('t1', 'r1', 's1', 'LEASED', 'developer', 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('a1', 't1', 'PREPARED', 1, 1, 1);",
    )?;
    for _ in 0..2 {
        let rows = store
            .reconcile_scan("p1", std::time::SystemTime::now())
            .await?;
        assert_eq!(rows.len(), 1);
        store
            .reconcile_apply(
                rows[0].clone(),
                Disposition::Redeliver,
                Process::NotStarted,
                "reconcile",
                std::time::SystemTime::now(),
                0,
            )
            .await?;
    }
    let conn = Connection::open(&path)?;
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM events WHERE kind = 'reconcile.attempt'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(n, 2);
    Ok(())
}

#[tokio::test]
async fn a_renewal_extends_only_leases_that_are_still_unexpired() -> R {
    // REQ-041 (renewed every 30 s for 120 s), ADR-019 decision 7: an expired lease is the runner's to settle, never
    // revived by a renewal.
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let store = Store::open(&path, test_options())?;
    seed(&path)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES
           ('t1', 'r1', 's1', 'RUNNING', 'developer', 1), ('t2', 'r1', 's1', 'RUNNING', 'developer', 2);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
           ('a1', 't1', 'RUNNING', 1, 1, 1), ('a2', 't2', 'RUNNING', 1, 1, 1);
         INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
           ('p1', 'reader-1', 'a1', 1, '2026-11-07T00:02:00Z'), ('p1', 'reader-2', 'a2', 1, '2026-11-07T00:00:30Z');",
    )?;
    let now = crate::time::parse_rfc3339("2026-11-07T00:01:00Z").ok_or("a time")?;
    let renewed = store
        .renew_leases(vec!["a1".to_owned(), "a2".to_owned()], now)
        .await?;
    assert_eq!(renewed, 1);
    let conn = Connection::open(&path)?;
    let leases: Vec<(String, String)> = conn
        .prepare("SELECT attempt_id, expires_at FROM leases ORDER BY attempt_id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        leases,
        [
            ("a1".to_owned(), "2026-11-07T00:03:00Z".to_owned()),
            ("a2".to_owned(), "2026-11-07T00:00:30Z".to_owned())
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_cancel_overtakes_the_writes_queued_before_it() -> R {
    // REQ-043 behaviour 3 (docs/adr/ADR-021.md decision 8a): a CancelRun goes through the writer's priority lane. With
    // the writer busy (each request stalls) and three writes queued, the cancel is written right after the one in
    // progress, before them.
    let dir = tempfile::tempdir()?;
    let path = db_in(&dir);
    let options = Options {
        queue: 8,
        ..test_options()
    };
    let store = std::sync::Arc::new(store_with(
        &path,
        options,
        Fault::Stall(Duration::from_millis(300)),
    )?);
    seed(&path)?;
    let only_event = |id: &str| Commit {
        cas: vec![],
        events: vec![event(id)],
        outbox: vec![],
    };
    let mut writes = Vec::new();
    for id in ["first", "b", "c", "d"] {
        let (store, commit) = (store.clone(), only_event(id));
        writes.push(tokio::spawn(async move { store.commit(commit).await }));
        // `first` is taken and stalls; the others queue behind it in order.
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let cancel = SubmitCommand {
        kind: "CancelRun".to_owned(),
        target: RunTarget::To(RunState::CancelRequested),
        event: event("cancel"),
        ..pause("cancel", "c", 0)
    };
    let accepted = store.submit_command(cancel).await?;
    assert!(
        matches!(accepted, CommandOutcome::Accepted { .. }),
        "{accepted:?}"
    );
    for write in writes {
        write.await??;
    }
    let order: Vec<String> = store.reader()?.with(|c| {
        let mut stmt = c.prepare("SELECT event_id FROM events ORDER BY seq")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect()
    })?;
    assert_eq!(order, ["first", "cancel", "b", "c", "d"]);
    Ok(())
}
