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

// Compat suites and the writer slot (docs/adr/ADR-030.md decision 10, K7; M2T15D-01, -04, -151, -202).

use crate::persistence::suites::{self, Admission, Publication, Published, SuiteSubject};
use serde_json::{Value, json};

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

/// `agent` at the implementation whose hash is all `implementation`.
fn subject(agent: &str, implementation: char) -> SuiteSubject {
    SuiteSubject {
        agent: agent.to_owned(),
        cli_version: format!("{agent} 1.0"),
        identity: crate::staffing::AgentIdentity {
            launcher_path: format!("/opt/bin/{agent}"),
            launcher_sha256: hex('a'),
            implementation_path: format!("/opt/lib/{agent}"),
            implementation_sha256: hex(implementation),
        },
    }
}

/// The schema-valid entry a suite on `agent` at `implementation` earned, PASS.
fn entry(agent: &str, implementation: char) -> Value {
    json!({
        "cli_version": format!("{agent} 1.0"),
        "launcher_path": format!("/opt/bin/{agent}"),
        "launcher_sha256": hex('a'),
        "implementation_path": format!("/opt/lib/{agent}"),
        "implementation_sha256": hex(implementation),
        "transport": if agent == "codex" { "app-server" } else { "stream-json" },
        "help_sha256": hex('c'),
        "schema_sha256": null,
        "capabilities": {"structured_events": true},
        "suite": {"status": "PASS", "fixture_set_sha256": hex('e'), "run_at": "2026-11-14T00:00:00Z",
                  "cases": [{"case": "probe", "status": "PASS"}, {"case": "live", "status": "NOT_RUN"}]},
    })
}

fn admit(
    conn: &mut Connection,
    project: &str,
    subject: &SuiteSubject,
    secs: u64,
) -> Result<Admission, PersistError> {
    suites::begin(
        conn,
        project,
        subject,
        at(secs),
        &crate::persistence::writer::Ctx::quiet(),
    )
}

fn publish(
    conn: &mut Connection,
    dir: &tempfile::TempDir,
    suite_id: &str,
    agent: &str,
    entry: Value,
    base: Option<Value>,
) -> Result<Published, PersistError> {
    suites::publish(
        conn,
        &Publication {
            project_id: "p1".to_owned(),
            suite_id: suite_id.to_owned(),
            agent: agent.to_owned(),
            entry,
            base,
            files: crate::compat_manifest::Files::of(dir.path(), "test"),
            now: at(60),
        },
        &crate::persistence::writer::Ctx::quiet(),
    )
}

fn end_suite(conn: &mut Connection, suite_id: &str, status: &str) -> Result<bool, PersistError> {
    suites::end(
        conn,
        "p1",
        suite_id,
        status,
        None,
        &crate::persistence::writer::Ctx::quiet(),
    )
}

/// The project's compat events in order: (kind, payload).
fn suite_events(conn: &Connection) -> Result<Vec<(String, Value)>, Box<dyn std::error::Error>> {
    let rows: Vec<(String, String)> = conn
        .prepare("SELECT kind, payload_json FROM events WHERE kind LIKE 'compat.%' ORDER BY seq")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    rows.into_iter()
        .map(|(kind, payload)| Ok((kind, serde_json::from_str(&payload)?)))
        .collect()
}

fn of_kind<'a>(events: &'a [(String, Value)], kind: &str) -> Vec<&'a Value> {
    events
        .iter()
        .filter(|(k, _)| k == kind)
        .map(|(_, p)| p)
        .collect()
}

/// Whether the writer slot is granted to `attempt` at `secs` (the grant is rolled back either way).
fn writer_granted(conn: &mut Connection, attempt: &str, secs: u64) -> Result<bool, PersistError> {
    let tx = conn.transaction().map_err(sqlite_error)?;
    let granted = grant(&tx, "p1", "writer", attempt, at(secs));
    let free = free_slot(&tx, "p1", true, 2, at(secs))?;
    match granted {
        Ok(_) => Ok(true),
        Err(PersistError::LeaseUnavailable(why)) => {
            assert_eq!(free, None, "free_slot and grant disagree: {why}");
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

fn started_id(admission: &Admission) -> String {
    match admission {
        Admission::Started(id) => id.clone(),
        other => panic!("not started: {other:?}"),
    }
}

#[test]
fn a_released_writer_row_admits_the_suite() -> R {
    // M2T15D-01: the settle expires the writer's lease in its own transaction and keeps the row and its epoch; the
    // suite's admission judges by the `effective` predicate, never by the row being there.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    running(&mut conn, "a-w1", "writer")?;
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Running, AttemptState::Settled, 9),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    let held = in_tx(&mut conn, |tx| held(tx, "p1", at(9)))?;
    assert_eq!(held.len(), 1, "the row is kept");
    assert_eq!((held[0].kind.as_str(), held[0].epoch), ("writer", 1));
    assert!(!held[0].effective);
    let admitted = admit(&mut conn, "p1", &subject("codex", 'b'), 9)?;
    let suite = started_id(&admitted);
    let events = suite_events(&conn)?;
    assert_eq!(
        events.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["compat.suite_opened", "compat.suite_started"]
    );
    assert_eq!(
        events[0].1,
        json!({"suite_id": suite, "agent": "codex", "implementation_sha256": hex('b'), "host_epoch": 0})
    );
    assert_eq!(events[1].1, json!({"suite_id": suite}));
    Ok(())
}

#[test]
fn an_expired_lease_of_a_live_holder_defers_the_suite() -> R {
    // REQ-037 b7: expired while its holder has not ended, the writer lease still holds the slot; the suite is deferred
    // (AC-027-04: the suite's request is refused while the write lease is active), once per holder.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    let first = admit(&mut conn, "p1", &subject("codex", 'b'), 500)?;
    let again = admit(&mut conn, "p1", &subject("codex", 'b'), 501)?;
    let Admission::Deferred(suite) = first else {
        panic!("not deferred: {first:?}");
    };
    assert_eq!(again, Admission::Deferred(suite.clone()));
    let events = suite_events(&conn)?;
    assert_eq!(
        events.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        ["compat.suite_deferred", "compat.suite_opened"]
    );
    assert_eq!(
        events[0].1,
        json!({"agent": "codex", "cli_version": "codex 1.0", "implementation_sha256": hex('b'),
               "lease_attempt_id": "a-w1", "lease_epoch": 1, "reason": "write_lease_held",
               "detail": "维护窗口：写租约释放后自动运行兼容套件"})
    );
    assert_eq!(events[1].1["suite_id"], json!(suite));
    Ok(())
}

#[test]
fn an_open_suite_closes_the_writer_slot_only() -> R {
    // M2T15D-04: while a suite is open the writer slot's two ways in refuse; the reader slots do not; once it ended the
    // writer is granted again.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let suite = started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 0)?);
    assert_eq!(
        in_tx(&mut conn, |tx| free_slot(tx, "p1", true, 2, at(1)))?,
        None
    );
    let refused = in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(1)));
    assert!(
        matches!(&refused, Err(PersistError::LeaseUnavailable(why)) if why.contains(&suite)),
        "{refused:?}"
    );
    assert_eq!(
        in_tx(&mut conn, |tx| free_slot(tx, "p1", false, 2, at(1)))?,
        Some("reader-1".to_owned())
    );
    in_tx(&mut conn, |tx| grant(tx, "p1", "reader-1", "a-q1", at(1)))?;
    assert!(end_suite(&mut conn, &suite, "PASS")?);
    assert_eq!(
        in_tx(&mut conn, |tx| free_slot(tx, "p1", true, 2, at(2)))?,
        Some("writer".to_owned())
    );
    assert_eq!(
        in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(2)))?.epoch,
        1
    );
    Ok(())
}

#[test]
fn a_grant_first_defers_the_suite() -> R {
    // The two orders of the writer's grant and the suite's admission (K7): granted first, the admission sees the holder
    // and defers, so the suite never starts while it holds.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    assert!(matches!(
        admit(&mut conn, "p1", &subject("codex", 'b'), 1)?,
        Admission::Deferred(_)
    ));
    let events = suite_events(&conn)?;
    assert_eq!(of_kind(&events, "compat.suite_deferred").len(), 1);
    assert_eq!(of_kind(&events, "compat.suite_opened").len(), 1);
    assert!(of_kind(&events, "compat.suite_started").is_empty());
    Ok(())
}

#[test]
fn an_admission_first_refuses_the_grant() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 0)?);
    assert!(!writer_granted(&mut conn, "a-w1", 1)?);
    Ok(())
}

#[test]
fn a_deferred_suite_is_not_starved() -> R {
    // M2T15D-04: the suite is opened at its first deferral, so the next writer due is not granted; once the holder
    // ends the suite is admitted, runs and publishes, and only then is that writer granted.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    running(&mut conn, "a-w1", "writer")?;
    let Admission::Deferred(suite) = admit(&mut conn, "p1", &subject("codex", 'b'), 3)? else {
        panic!("not deferred");
    };
    assert!(
        !writer_granted(&mut conn, "a-w2", 4)?,
        "a writer granted past the waiting suite"
    );
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Running, AttemptState::Settled, 9),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    assert!(
        !writer_granted(&mut conn, "a-w2", 9)?,
        "granted before the suite ran"
    );
    assert_eq!(
        admit(&mut conn, "p1", &subject("codex", 'b'), 9)?,
        Admission::Started(suite.clone())
    );
    assert!(!writer_granted(&mut conn, "a-w2", 10)?);
    let published = publish(&mut conn, &dir, &suite, "codex", entry("codex", 'b'), None)?;
    assert!(matches!(published, Published::Replaced(_)), "{published:?}");
    assert!(writer_granted(&mut conn, "a-w2", 11)?);
    let events = suite_events(&conn)?;
    assert_eq!(
        events.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(),
        [
            "compat.suite_deferred",
            "compat.suite_opened",
            "compat.suite_started",
            "compat.suite_ended",
            "compat.suite_completed"
        ]
    );
    assert_eq!(events[3].1, json!({"suite_id": suite, "status": "PASS"}));
    assert_eq!(
        events[4].1,
        json!({"suite_id": suite, "agent": "codex", "status": "PASS", "implementation_sha256": hex('b'),
               "fixture_set_sha256": hex('e'),
               "cases": [{"case": "probe", "status": "PASS"}, {"case": "live", "status": "NOT_RUN"}]})
    );
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(crate::compat_manifest::path(dir.path()))?)?;
    assert_eq!(manifest["agents"]["codex"], entry("codex", 'b'));
    Ok(())
}

#[test]
fn a_suite_no_longer_needed_is_withdrawn() -> R {
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    in_tx(&mut conn, |tx| grant(tx, "p1", "writer", "a-w1", at(0)))?;
    let Admission::Deferred(suite) = admit(&mut conn, "p1", &subject("codex", 'b'), 1)? else {
        panic!("not deferred");
    };
    assert!(end_suite(&mut conn, &suite, suites::WITHDRAWN)?);
    // Closed: a second end and a publication do nothing, and the slot is the holder's alone again.
    assert!(!end_suite(&mut conn, &suite, suites::WITHDRAWN)?);
    assert_eq!(
        publish(&mut conn, &dir, &suite, "codex", entry("codex", 'b'), None)?,
        Published::Closed
    );
    end(&conn, "a-w1")?;
    in_tx(&mut conn, |tx| {
        release_held_by(tx, "a-w1", &crate::time::rfc3339_utc(at(2)))
    })?;
    assert!(writer_granted(&mut conn, "a-w2", 2)?);
    let events = suite_events(&conn)?;
    assert_eq!(
        of_kind(&events, "compat.suite_ended"),
        [&json!({"suite_id": suite, "status": "withdrawn"})]
    );
    Ok(())
}

#[test]
fn a_suite_of_an_older_host_epoch_is_closed() -> R {
    // A suite a host that stopped or crashed left open is closed by the next host's epoch: the slot is grantable, its
    // late publication changes nothing, and the identity is admitted again in the new epoch.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let old = started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 0)?);
    conn.execute("UPDATE projects SET host_epoch = host_epoch + 1", [])?;
    assert!(writer_granted(&mut conn, "a-w1", 1)?);
    assert_eq!(
        publish(&mut conn, &dir, &old, "codex", entry("codex", 'b'), None)?,
        Published::Closed
    );
    assert!(!crate::compat_manifest::path(dir.path()).exists());
    assert!(!end_suite(&mut conn, &old, suites::ABANDONED)?);
    let new = started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 2)?);
    assert_ne!(new, old);
    Ok(())
}

#[test]
fn a_late_publication_does_not_replace_the_manifest() -> R {
    // A suite past its limit is ended timed_out by the host; its result arriving later finds it closed: no rename, the
    // manifest's bytes as they were, no temporary file left.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let files = crate::compat_manifest::Files::of(dir.path(), "test");
    std::fs::write(&files.path, b"{\"the\": \"old manifest\"}")?;
    let suite = started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 0)?);
    assert!(end_suite(&mut conn, &suite, suites::TIMED_OUT)?);
    assert_eq!(
        publish(&mut conn, &dir, &suite, "codex", entry("codex", 'b'), None)?,
        Published::Closed
    );
    assert_eq!(std::fs::read(&files.path)?, b"{\"the\": \"old manifest\"}");
    assert!(!files.temp.exists());
    assert!(of_kind(&suite_events(&conn)?, "compat.suite_completed").is_empty());
    Ok(())
}

#[test]
fn a_schema_invalid_entry_is_not_published() -> R {
    // K6: a manifest that would not be schema-valid is not written; the suite ends unpublished with why, and the old
    // manifest stays byte for byte.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    let files = crate::compat_manifest::Files::of(dir.path(), "test");
    std::fs::write(&files.path, b"OLD")?;
    let suite = started_id(&admit(&mut conn, "p1", &subject("codex", 'b'), 0)?);
    let mut invalid = entry("codex", 'b');
    invalid["suite"]["status"] = json!("GREAT");
    let published = publish(&mut conn, &dir, &suite, "codex", invalid, None)?;
    assert!(
        matches!(&published, Published::Unpublished(why) if why.contains("schema")),
        "{published:?}"
    );
    assert_eq!(std::fs::read(&files.path)?, b"OLD");
    let events = suite_events(&conn)?;
    let ended = of_kind(&events, "compat.suite_ended");
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["status"], "unpublished");
    assert!(
        ended[0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("schema"))
    );
    assert!(writer_granted(&mut conn, "a-w1", 1)?, "the suite is closed");
    Ok(())
}

#[test]
fn a_second_suite_waits_while_one_is_open() -> R {
    // M2T15D-151: at most one suite open per host epoch. Codex's, deferred, is open: claude's admission writes nothing
    // and is Busy with codex's suite; once codex's ended claude's is admitted, and its publication keeps the codex
    // entry codex's suite just published.
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    running(&mut conn, "a-w1", "writer")?;
    let Admission::Deferred(codex) = admit(&mut conn, "p1", &subject("codex", 'b'), 3)? else {
        panic!("not deferred");
    };
    assert_eq!(
        admit(&mut conn, "p1", &subject("claude", 'c'), 4)?,
        Admission::Busy(codex.clone())
    );
    let events = suite_events(&conn)?;
    assert!(
        events.iter().all(|(_, p)| p["agent"] != "claude"),
        "{events:?}"
    );
    attempts::apply(
        &mut conn,
        &moving("a-w1", AttemptState::Running, AttemptState::Settled, 9),
        &crate::persistence::writer::Ctx::quiet(),
    )?;
    assert_eq!(
        admit(&mut conn, "p1", &subject("codex", 'b'), 9)?,
        Admission::Started(codex.clone())
    );
    assert_eq!(
        admit(&mut conn, "p1", &subject("claude", 'c'), 9)?,
        Admission::Busy(codex.clone())
    );
    let Published::Replaced(after_codex) =
        publish(&mut conn, &dir, &codex, "codex", entry("codex", 'b'), None)?
    else {
        panic!("codex not published");
    };
    let claude = started_id(&admit(&mut conn, "p1", &subject("claude", 'c'), 10)?);
    assert_ne!(claude, codex);
    let Published::Replaced(after_claude) = publish(
        &mut conn,
        &dir,
        &claude,
        "claude",
        entry("claude", 'c'),
        Some(after_codex),
    )?
    else {
        panic!("claude not published");
    };
    assert_eq!(after_claude["agents"]["codex"], entry("codex", 'b'));
    assert_eq!(after_claude["agents"]["claude"], entry("claude", 'c'));
    let on_disk: Value =
        serde_json::from_slice(&std::fs::read(crate::compat_manifest::path(dir.path()))?)?;
    assert_eq!(on_disk, after_claude);
    Ok(())
}

#[test]
fn a_160_character_project_counts_its_suites() -> R {
    // M2T15D-202: with a 160-character project id the suites' ids are shortened series members; open, end, open again
    // gives n = 1, none, n = 2, and the writer slot is closed after the second opening.
    let long = "p".repeat(crate::ids::MAX_ID);
    let dir = tempfile::tempdir()?;
    let mut conn = world(&dir)?;
    conn.execute_batch(&format!(
        "INSERT INTO projects(project_id, repo_path, host_epoch, created_at) \
           VALUES ('{long}', '/repo2', 1, '2026-11-04T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', '{long}', printf('%064d', 2), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) \
           VALUES ('R2', '{long}', 's2', 'RUNNING', 'AUTO', 1, '2026-11-04T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) \
           VALUES ('X1', 'R2', 's2', 'LEASED', 1, 'developer', 1, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) \
           VALUES ('b-w1', 'X1', 'PREPARED', 1, 1, 0);"
    ))?;
    let open = |conn: &Connection| -> Result<Option<u64>, PersistError> {
        Ok(suite_open(conn, &long)?.map(|s| s.n))
    };
    let first = started_id(&admit(&mut conn, &long, &subject("codex", 'b'), 0)?);
    assert!(first.len() <= crate::ids::MAX_ID, "{first}");
    assert_eq!(open(&conn)?, Some(1));
    assert!(suites::end(
        &mut conn,
        &long,
        &first,
        "PASS",
        None,
        &crate::persistence::writer::Ctx::quiet()
    )?);
    assert_eq!(open(&conn)?, None);
    let second = started_id(&admit(&mut conn, &long, &subject("codex", 'b'), 1)?);
    assert_ne!(second, first);
    assert_eq!(open(&conn)?, Some(2));
    let refused = in_tx(&mut conn, |tx| grant(tx, &long, "writer", "b-w1", at(2)));
    assert!(
        matches!(refused, Err(PersistError::LeaseUnavailable(_))),
        "{refused:?}"
    );
    Ok(())
}
