use super::*;
use crate::lifecycle::AttemptState;
use crate::persistence::attempts::{self, AttemptFacts, AttemptJob, AttemptMove};
use crate::persistence::{Options, Store, selftest};
use rusqlite::Connection;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R = Result<(), Box<dyn std::error::Error>>;

const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-04T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('W1', 'R', 's1', 'LEASED', 1, 'developer', 1, 1),
  ('W2', 'R', 's1', 'LEASED', 1, 'developer', 1, 2),
  ('Q1', 'R', 's1', 'LEASED', 1, 'qa', 0, 3),
  ('Q2', 'R', 's1', 'LEASED', 1, 'qa', 0, 4),
  ('Q3', 'R', 's1', 'LEASED', 1, 'qa', 0, 5);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
  ('a-w1', 'W1', 'PREPARED', 1, 1, 0), ('a-w2', 'W2', 'PREPARED', 1, 1, 0),
  ('a-q1', 'Q1', 'PREPARED', 1, 1, 0), ('a-q2', 'Q2', 'PREPARED', 1, 1, 0), ('a-q3', 'Q3', 'PREPARED', 1, 1, 0);
";

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000 + secs)
}

/// A store's database (schema made by the store), the store closed, and a connection to it.
fn world(dir: &tempfile::TempDir) -> Result<Connection, Box<dyn std::error::Error>> {
    let path = dir.path().join(".hoh/state.db");
    drop(Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::default()
        },
    )?);
    let conn = Connection::open(&path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    conn.execute_batch(WORLD)?;
    Ok(conn)
}

fn in_tx<T>(
    conn: &mut Connection,
    f: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, PersistError>,
) -> Result<T, PersistError> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let out = f(&tx)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(out)
}

fn end(conn: &Connection, attempt: &str) -> R {
    conn.execute(
        "UPDATE attempts SET state = 'SETTLED' WHERE attempt_id = ?1",
        [attempt],
    )?;
    Ok(())
}

#[test]
fn a_held_writer_lease_is_not_granted_again_and_epochs_rise() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let first = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    assert_eq!((first.kind.as_str(), first.epoch), ("writer", 1));
    assert_eq!(first.expires_at, crate::time::rfc3339_utc(at(120)));
    // Held: refused, nothing changes.
    let refused = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(10)));
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    // Expired, but its holder has not ended: still refused (REQ-037 b7, REQ-041).
    let refused = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(500)));
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    end(&conn, "a-w1")?;
    let second = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(500)))?;
    assert_eq!(second.epoch, 2);
    let rows: i64 = conn.query_row("SELECT count(*) FROM leases", [], |r| r.get(0))?;
    assert_eq!(rows, 1);
    Ok(())
}

#[test]
fn an_ended_holder_frees_its_lease_at_once_when_released() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    end(&conn, "a-w1")?;
    // Ended but not yet expired: still held until released or expired.
    let refused = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(5)));
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    in_tx(&mut conn, |tx| {
        release_held_by(tx, "a-w1", &crate::time::rfc3339_utc(at(5)))
    })?;
    let next = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(5)))?;
    assert_eq!(next.epoch, 2);
    Ok(())
}

#[test]
fn a_grant_on_a_slot_moved_by_someone_else_is_stale() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    end(&conn, "a-w1")?;
    // The CAS compares the epoch read inside the UPDATE itself: a change between the read and the write is caught.
    let stale = in_tx(&mut conn, |tx| {
        tx.execute("UPDATE leases SET epoch = epoch + 5", [])
            .map_err(sqlite_error)?;
        cas(tx, "p1", "writer", "a-w2", 1, at(500))
    });
    assert!(
        matches!(stale, Err(PersistError::StaleLeaseEpoch(_))),
        "{stale:?}"
    );
    Ok(())
}

#[test]
fn reader_slots_are_numbered_and_limited() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let slot = |conn: &mut Connection, now| in_tx(conn, |tx| free_slot(tx, "p1", false, 2, now));
    assert_eq!(slot(&mut conn, at(0))?, Some("reader-1".to_owned()));
    in_tx(&mut conn, |tx| grant(tx, "p1", "reader-1", "a-q1", at(0)))?;
    assert_eq!(slot(&mut conn, at(0))?, Some("reader-2".to_owned()));
    in_tx(&mut conn, |tx| grant(tx, "p1", "reader-2", "a-q2", at(0)))?;
    assert_eq!(slot(&mut conn, at(0))?, None, "a third reader slot");
    assert_eq!(
        in_tx(&mut conn, |tx| free_slot(tx, "p1", true, 2, at(0)))?,
        Some("writer".to_owned())
    );
    let held = in_tx(&mut conn, |tx| held(tx, "p1", at(0)))?;
    assert_eq!(
        held.iter().map(|l| l.kind.as_str()).collect::<Vec<_>>(),
        ["reader-1", "reader-2"]
    );
    assert!(held.iter().all(|l| l.effective));
    Ok(())
}

fn moving(attempt: &str, from: AttemptState, to: AttemptState, at_secs: u64) -> AttemptJob {
    let stamp = crate::time::rfc3339_utc(at(at_secs));
    let mut facts = AttemptFacts::default();
    if to == AttemptState::Starting {
        facts.started_at = Some(stamp);
    } else {
        facts.ended_at = Some(stamp);
    }
    AttemptJob::Move(AttemptMove {
        attempt_id: attempt.to_owned(),
        from,
        to,
        facts,
    })
}

#[test]
fn an_attempt_that_ends_lets_its_lease_go_at_its_end() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Failed, 7),
    )?;
    let held = in_tx(&mut conn, |tx| held(tx, "p1", at(7)))?;
    assert_eq!(held[0].expires_at, crate::time::rfc3339_utc(at(7)));
    assert!(!held[0].effective);
    let next = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(7)))?;
    assert_eq!(next.epoch, 2);
    Ok(())
}

#[test]
fn a_writer_attempt_starts_only_with_its_writer_lease() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    // REQ-037 b2: no writer lease, no start of a writer task.
    let refused = attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 1),
    );
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    // Another attempt's lease is not this one's.
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(0)))?;
    let refused = attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 1),
    );
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    // Its own unexpired lease: it starts; an expired one does not.
    attempts::apply(
        &mut conn,
        &moving("a-w2", AttemptState::Prepared, AttemptState::Starting, 1),
    )?;
    end(&conn, "a-w2")?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(200)))?;
    let late = attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 400),
    );
    assert!(
        matches!(late, Err(PersistError::LeaseUnavailable(_))),
        "{late:?}"
    );
    // A reader task needs no writer lease to start.
    attempts::apply(
        &mut conn,
        &moving("a-q1", AttemptState::Prepared, AttemptState::Starting, 1),
    )?;
    Ok(())
}
