//! Unit and fault-injection tests of the store (M2-T01).

use super::writer::{Fault, Writer};
use super::*;
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
    let writer = Writer::spawn_with(conn, &options, fault)?;
    Ok(Store {
        path: path.to_path_buf(),
        options,
        writer,
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

fn cas(expected: i64, state: &str) -> StateCas {
    StateCas {
        entity: Entity::Run,
        id: "r1".to_owned(),
        expected_revision: expected,
        state: state.to_owned(),
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
            cas: vec![cas(0, "DISCOVERING")],
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
            cas: vec![cas(1, "SPECIFYING")],
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
        cas: vec![cas(0, "DISCOVERING")],
        events: vec![event("e-ok")],
        outbox: vec![],
    };
    let cases: Vec<(&str, Commit)> = vec![
        (
            "CHECK state",
            Commit {
                cas: vec![cas(0, "BOGUS")],
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
            cas: vec![cas(0, "DISCOVERING"), cas(7, "READY")],
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
            cas: vec![cas(0, "DISCOVERING")],
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
            cas: vec![cas(0, "DISCOVERING")],
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
            cas: vec![cas(0, "DISCOVERING")],
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
            cas: vec![cas(0, "DISCOVERING")],
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
