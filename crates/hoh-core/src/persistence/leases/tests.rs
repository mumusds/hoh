use super::*;
use crate::lifecycle::{AttemptState, Lifecycle as _};
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
        &crate::persistence::writer::Ctx::quiet(),
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
        &crate::persistence::writer::Ctx::quiet(),
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
        &crate::persistence::writer::Ctx::quiet(),
    );
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    // Its own unexpired lease: it starts; an expired one does not.
    attempts::apply(
        &mut conn,
        &moving("a-w2", AttemptState::Prepared, AttemptState::Starting, 1),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    end(&conn, "a-w2")?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(200)))?;
    let late = attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 400),
        &crate::persistence::writer::Ctx::quiet(),
    );
    assert!(
        matches!(late, Err(PersistError::LeaseUnavailable(_))),
        "{late:?}"
    );
    // A reader task needs no writer lease to start.
    attempts::apply(
        &mut conn,
        &moving("a-q1", AttemptState::Prepared, AttemptState::Starting, 1),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    Ok(())
}

fn moving_at(attempt: &str, from: AttemptState, to: AttemptState, stamp: &str) -> AttemptJob {
    let mut facts = AttemptFacts::default();
    if to == AttemptState::Starting {
        facts.started_at = Some(stamp.to_owned());
    } else {
        facts.ended_at = Some(stamp.to_owned());
    }
    AttemptJob::Move(AttemptMove {
        attempt_id: attempt.to_owned(),
        from,
        to,
        facts,
    })
}

/// The instant `secs` written as local time `hours` east (or west) of UTC.
fn shifted(secs: u64, hours: u64, east: bool) -> String {
    let offset = Duration::from_secs(hours * 3600);
    let local = if east {
        at(secs) + offset
    } else {
        at(secs) - offset
    };
    let text = crate::time::rfc3339_utc(local);
    format!(
        "{}{}{hours:02}:00",
        &text[..19],
        if east { '+' } else { '-' }
    )
}

#[test]
fn times_with_offsets_are_judged_as_instants() -> R {
    // M2T03R-01: granted at 0 s, so the lease expires at 120 s; 121 s written with a -05:00 offset is past it, and an
    // end written with +05:00 frees the slot at that instant.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    let late = attempts::apply(
        &mut conn,
        &moving_at(
            "a-w1",
            AttemptState::Prepared,
            AttemptState::Starting,
            &shifted(121, 5, false),
        ),
        &crate::persistence::writer::Ctx::quiet(),
    );
    assert!(
        matches!(late, Err(PersistError::LeaseUnavailable(_))),
        "{late:?}"
    );
    attempts::apply(
        &mut conn,
        &moving_at(
            "a-w1",
            AttemptState::Prepared,
            AttemptState::Failed,
            &shifted(5, 5, true),
        ),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    let held = in_tx(&mut conn, |tx| held(tx, "p1", at(5)))?;
    assert_eq!(held[0].expires_at, crate::time::rfc3339_utc(at(5)));
    // Not a date-time at all: refused, nothing written.
    let refused = attempts::apply(
        &mut conn,
        &moving_at(
            "a-w2",
            AttemptState::Prepared,
            AttemptState::Failed,
            "yesterday",
        ),
        &crate::persistence::writer::Ctx::quiet(),
    );
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    // A leap second and a time past 9999 in UTC are no date-times of the store either, on either path
    // (M2T03R-100, -101).
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(10)))?;
    for (to, stamp) in [
        (AttemptState::Starting, "9999-12-31T23:59:59-23:59"),
        (AttemptState::Failed, "2027-01-15T08:00:60Z"),
    ] {
        let refused = attempts::apply(
            &mut conn,
            &moving_at("a-w2", AttemptState::Prepared, to, stamp),
            &crate::persistence::writer::Ctx::quiet(),
        );
        assert!(
            matches!(refused, Err(PersistError::IntegrityViolation(_))),
            "{stamp}: {refused:?}"
        );
    }
    let state: String = conn.query_row(
        "SELECT state FROM attempts WHERE attempt_id = 'a-w2'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(state, "PREPARED");
    Ok(())
}

#[test]
fn an_attempt_whose_effect_is_unknown_lets_its_lease_go_at_that_move() -> R {
    // ADR-019 decision 5 (amends ADR-017 decision 4): UNKNOWN_EFFECT is entered only once the attempt's process is
    // gone, so it holds no slot; its task gets no new attempt by itself, but another writer task may run. A writer's
    // move there carries its workspace's record (REQ-019 behaviour 6, docs/adr/ADR-024.md decision 9).
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 1),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    attempts::apply(
        &mut conn,
        &recorded(
            moving(
                "a-w1",
                AttemptState::Starting,
                AttemptState::UnknownEffect,
                9,
            ),
            serde_json::json!({"head": null}),
        ),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    let held = in_tx(&mut conn, |tx| held(tx, "p1", at(9)))?;
    assert_eq!(held[0].expires_at, crate::time::rfc3339_utc(at(9)));
    assert!(!held[0].effective);
    let next = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(9)))?;
    assert_eq!(next.epoch, 2);
    Ok(())
}

/// `job` with the workspace record `record`.
fn recorded(job: AttemptJob, record: serde_json::Value) -> AttemptJob {
    match job {
        AttemptJob::Move(mut m) => {
            m.facts.workspace = Some(record);
            AttemptJob::Move(m)
        }
        other => other,
    }
}

/// `attempt` RUNNING (granted `kind` at 0, started at 1, running at 2).
fn running(conn: &mut Connection, attempt: &str, kind: &str) -> R {
    in_tx(conn, |tx| grant(tx, "p1", kind, attempt, at(0)))?;
    let quiet = crate::persistence::writer::Ctx::quiet();
    attempts::apply(
        conn,
        &moving(attempt, AttemptState::Prepared, AttemptState::Starting, 1),
        &quiet,
    )?;
    attempts::apply(
        conn,
        &moving(attempt, AttemptState::Starting, AttemptState::Running, 2),
        &quiet,
    )?;
    Ok(())
}

fn workspace_events(conn: &Connection, attempt: &str) -> Result<Vec<String>, rusqlite::Error> {
    conn.prepare(
        "SELECT payload_json FROM events WHERE kind = 'attempt.workspace' \
         AND json_extract(payload_json, '$.attempt_id') = ?1",
    )?
    .query_map([attempt], |r| r.get(0))?
    .collect()
}

#[test]
fn an_interrupted_writer_lets_its_lease_go_only_with_its_workspace_record() -> R {
    // REQ-019 behaviour 6 (docs/adr/ADR-024.md decision 9, M2T17R-52, M2T17FR-50): a writer whose end is interrupted
    // (to UNKNOWN_EFFECT, or from RUNNING to CANCELLED or FAILED) without the workspace's record is refused: nothing is written, its lease
    // stays effective and no other writer is granted. With the record, the record is written in the end's transaction
    // and the next writer is granted. Its lease expired meanwhile changes nothing (the settle's shape).
    let quiet = crate::persistence::writer::Ctx::quiet();
    for (to, expired) in [
        (AttemptState::UnknownEffect, false),
        (AttemptState::UnknownEffect, true),
        (AttemptState::Cancelled, false),
        (AttemptState::Cancelled, true),
        // HoH's own stop of a writer (an oversize frame, a deadline) ends it FAILED too (M2T17FR-50).
        (AttemptState::Failed, false),
        (AttemptState::Failed, true),
    ] {
        let dir = tempfile::tempdir()?;
        let mut conn = world(&dir)?;
        running(&mut conn, "a-w1", "writer")?;
        let ended = if expired { 300 } else { 9 };
        let refused = attempts::apply(
            &mut conn,
            &moving("a-w1", AttemptState::Running, to, ended),
            &quiet,
        );
        assert!(
            matches!(refused, Err(PersistError::WorkspaceRecordRequired(_))),
            "{to:?} expired={expired}: {refused:?}"
        );
        let state: String = conn.query_row(
            "SELECT state FROM attempts WHERE attempt_id = 'a-w1'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "RUNNING", "{to:?} expired={expired}");
        let held = in_tx(&mut conn, |tx| held(tx, "p1", at(ended)))?;
        assert!(held[0].effective, "{to:?} expired={expired}: {held:?}");
        assert!(
            in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(ended))).is_err(),
            "{to:?} expired={expired}: another writer granted before the record"
        );
        attempts::apply(
            &mut conn,
            &recorded(
                moving("a-w1", AttemptState::Running, to, ended),
                serde_json::json!({"head": "h", "workspace_sha256": "w"}),
            ),
            &quiet,
        )?;
        let records = workspace_events(&conn, "a-w1")?;
        assert_eq!(records.len(), 1, "{to:?} expired={expired}");
        let record: serde_json::Value = serde_json::from_str(&records[0])?;
        assert_eq!(record["workspace"]["workspace_sha256"], "w");
        assert_eq!(record["to"], to.as_str());
        let next = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(ended)))?;
        assert_eq!(next.epoch, 2, "{to:?} expired={expired}");
    }
    Ok(())
}

#[test]
fn only_an_interrupted_writers_end_needs_a_workspace_record() -> R {
    // REQ-019 behaviour 6 (decision 9 after round 1, M2T17FR-50, -53): a writer that completed (SETTLED) ended by
    // itself; a STARTING writer cancelled or refused its start never ran its agent; a reader is no writer: none of
    // them needs the record, and their leases go at their end. A STARTING writer moved to UNKNOWN_EFFECT does.
    let quiet = crate::persistence::writer::Ctx::quiet();
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    running(&mut conn, "a-w1", "writer")?;
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Running, AttemptState::Settled, 9),
        &quiet,
    )?;
    assert!(workspace_events(&conn, "a-w1")?.is_empty());
    assert_eq!(
        in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(9)))?.epoch,
        2
    );
    for to in [AttemptState::Cancelled, AttemptState::Failed] {
        let dir = tempfile::tempdir()?;
        let mut conn = world(&dir)?;
        in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
        attempts::apply(
            &mut conn,
            &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 1),
            &quiet,
        )?;
        attempts::apply(
            &mut conn,
            &moving("a-w1", AttemptState::Starting, to, 9),
            &quiet,
        )?;
        assert_eq!(
            in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w2", at(9)))?.epoch,
            2,
            "{to:?}"
        );
    }
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Prepared, AttemptState::Starting, 1),
        &quiet,
    )?;
    let refused = attempts::apply(
        &mut conn,
        &moving(
            "a-w1",
            AttemptState::Starting,
            AttemptState::UnknownEffect,
            9,
        ),
        &quiet,
    );
    assert!(
        matches!(refused, Err(PersistError::WorkspaceRecordRequired(_))),
        "{refused:?}"
    );
    for to in [
        AttemptState::UnknownEffect,
        AttemptState::Cancelled,
        AttemptState::Failed,
    ] {
        let dir = tempfile::tempdir()?;
        let mut conn = world(&dir)?;
        running(&mut conn, "a-q1", "reader-1")?;
        attempts::apply(
            &mut conn,
            &moving("a-q1", AttemptState::Running, to, 9),
            &quiet,
        )?;
        assert!(workspace_events(&conn, "a-q1")?.is_empty());
    }
    Ok(())
}
