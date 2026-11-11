//! Wakes given Commander turns against a real store (docs/adr/ADR-025.md decision 3): one turn per wake, its tasks'
//! revisions recorded at allocation, the admission, the slot, the order across runs, and a supervision turn that
//! changes neither its run's state nor its revision.

use crate::decision::Context;
use crate::delivery::Clock;
use crate::persistence::{Allocated, Commit, NewEvent, Options, Store, StoreClock, selftest};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Default)]
struct TestClock(std::sync::Mutex<u64>);

impl TestClock {
    fn set(&self, ms: u64) {
        if let Ok(mut now) = self.0.lock() {
            *now = ms;
        }
    }
}

impl Clock for TestClock {
    fn now(&self) -> SystemTime {
        t(self.0.lock().map(|m| *m).unwrap_or(0))
    }
}

fn t(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000) + Duration::from_millis(ms)
}

fn open(dir: &tempfile::TempDir, clock: &std::sync::Arc<TestClock>) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        clock: Some(StoreClock(clock.clone())),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Runs R and S RUNNING on spec s3, each supervised by its commander's binding (session sc, active), each with a
/// developer task (D of R RUNNING at revision 4, E of S READY at revision 2).
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s3', 'p1', printf('%064d', 3), 3);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's3', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z'),
  ('S', 'p1', 's3', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:01Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{}'), ('sc', 'codex', 'commander', '1', '{}'), ('sc2', 'codex', 'commander', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z'),
  ('S', 'commander', 'codex', 'sc2', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('D', 'R', 's3', 'RUNNING', 4, 'developer', 1, 1),
  ('E', 'S', 's3', 'READY', 2, 'developer', 0, 1);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('A', 'D', 'RUNNING', 1, 1, 1);
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
";

fn context(clock: &TestClock, ms: u64) -> Context {
    clock.set(ms);
    Context {
        now: Some(t(ms)),
        wall: Some(t(ms)),
        ..Context::default()
    }
}

fn exec(path: &PathBuf, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

fn value(path: &PathBuf, sql: &str) -> R<Value> {
    let conn = Connection::open(path)?;
    Ok(conn.query_row(sql, [], |r| {
        Ok(match r.get_ref(0)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            rusqlite::types::ValueRef::Text(t) => {
                let text = String::from_utf8_lossy(t).into_owned();
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            }
            other => json!(format!("{other:?}")),
        })
    })?)
}

/// `run`'s event `(kind, payload)`, committed alone.
async fn event(store: &Store, run: &str, id: &str, kind: &str, payload: Value) -> R {
    store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: id.to_owned(),
                run_id: Some(run.to_owned()),
                kind: kind.to_owned(),
                payload,
            }],
            ..Commit::default()
        })
        .await?;
    Ok(())
}

/// Wake `run` for `task` (a need_decision of fingerprint `fp`) at `ms`: its trigger fires 2 s later; the wake's id.
async fn wake(
    store: &Store,
    clock: &TestClock,
    run: &str,
    task: &str,
    fp: &str,
    ms: u64,
) -> R<String> {
    clock.set(ms);
    event(
        store,
        run,
        &format!("{run}/{fp}"),
        "need_decision",
        json!({"task_id": task, "fingerprint": fp}),
    )
    .await?;
    let fired = store.supervise(run, context(clock, ms + 2000)).await?.fired;
    let [fired] = fired.as_slice() else {
        return Err(format!("{run}/{fp}: fired {fired:?}").into());
    };
    Ok(fired.trigger_id.clone())
}

/// End the supervision turn `attempt` as `state` and free its slot, as an ending does.
fn end(path: &PathBuf, attempt: &str, state: &str) -> R {
    exec(
        path,
        &format!(
            "UPDATE attempts SET state = '{state}', ended_at = '2027-01-15T08:00:00Z' WHERE attempt_id = '{attempt}';
             UPDATE leases SET expires_at = '2000-01-01T00:00:00Z' WHERE attempt_id = '{attempt}';"
        ),
    )
}

fn run_state(path: &PathBuf, run: &str) -> R<(String, i64)> {
    Ok(Connection::open(path)?.query_row(
        "SELECT state, revision FROM runs WHERE run_id = ?1",
        [run],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

#[tokio::test]
async fn an_owed_wake_is_given_one_commander_turn_with_its_tasks_revisions() -> R {
    // docs/adr/ADR-025.md decision 3: the wake becomes a supervision task `<wake>/turn/<n>` (role commander, LEASED)
    // with a PREPARED attempt on the supervisor slot, a runtime dispatch notice marked origin=supervision, and
    // supervisor.allocated naming the one wake with the revisions of its tasks, the run's and the Commander's epoch;
    // the run's revision does not move; while it is in flight nothing more is allocated.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let before = run_state(&path, "R")?;
    let Some(Allocated {
        wake,
        task_id,
        attempt_id,
        lease,
        ..
    }) = store
        .allocate_supervision("p1", context(&clock, 2500))
        .await?
    else {
        return Err("nothing allocated".into());
    };
    assert_eq!(wake, w);
    assert_eq!(task_id, format!("{w}/turn/0"));
    assert_eq!(lease.kind, "supervisor");
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_array(t.role, t.state, a.state) FROM tasks t JOIN attempts a ON a.task_id = t.task_id WHERE a.attempt_id = '{attempt_id}'"
            )
        )?,
        json!(["commander", "LEASED", "PREPARED"])
    );
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_array(recipient_role, json_extract(payload_json, '$.origin'), json_extract(payload_json, '$.wake')) FROM outbox WHERE json_extract(payload_json, '$.attempt_id') = '{attempt_id}'"
            )
        )?,
        json!(["commander", "supervision", w])
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_object('wake', json_extract(payload_json, '$.wake'), 'generation', json_extract(payload_json, '$.generation'), \
             'snapshots', json_extract(payload_json, '$.snapshots'), 'run_revision', json_extract(payload_json, '$.run_revision'), \
             'role_epoch', json_extract(payload_json, '$.role_epoch')) FROM events WHERE kind = 'supervisor.allocated'"
        )?,
        json!({"wake": w, "generation": 0, "snapshots": {"D": 4}, "run_revision": 1, "role_epoch": 1})
    );
    assert_eq!(run_state(&path, "R")?, before, "no Touch(Run)");
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3000))
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn one_turn_takes_one_wake_and_the_next_waits_for_the_next_turn() -> R {
    // M2T08D-750 (decision 3): two wakes of one run are two turns; the second is allocated once the first's turn has
    // ended and freed the slot; a settled wake is owed nothing more.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let first = wake(&store, &clock, "R", "D", "f1", 0).await?;
    let second = wake(&store, &clock, "R", "D", "f2", 100).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("first")?;
    assert_eq!(a.wake, first);
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3100))
            .await?
            .is_none()
    );
    end(&path, &a.attempt_id, "SETTLED")?;
    let b = store
        .allocate_supervision("p1", context(&clock, 3200))
        .await?
        .ok_or("second")?;
    assert_eq!(
        (b.wake.as_str(), b.task_id.as_str()),
        (second.as_str(), format!("{second}/turn/1").as_str())
    );
    end(&path, &b.attempt_id, "SETTLED")?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3300))
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn a_wake_is_allocated_only_while_its_run_admits_it_and_its_commander_is_active() -> R {
    // Decision 3 (admission): a PAUSED run's wake waits; a Commander not active (its binding BLOCKED) waits; a budget
    // that stops the run waits; with reserve_supervisor_slot = false the turn takes a reader slot.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    exec(
        &path,
        "UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING' WHERE run_id = 'R';",
    )?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3000))
            .await?
            .is_none()
    );
    exec(
        &path,
        "UPDATE runs SET state = 'RUNNING', resume_state = NULL WHERE run_id = 'R';
         UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH' WHERE run_id = 'R' AND role = 'commander';",
    )?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3100))
            .await?
            .is_none()
    );
    exec(
        &path,
        "UPDATE role_bindings SET state = 'ACTIVE', blocked_reason = NULL WHERE run_id = 'R' AND role = 'commander';
         UPDATE runs SET deadline_at = '2020-01-01T00:00:00Z' WHERE run_id = 'R';",
    )?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3200))
            .await?
            .is_none()
    );
    exec(
        &path,
        "UPDATE runs SET deadline_at = NULL WHERE run_id = 'R';",
    )?;
    let reader = store
        .allocate_supervision(
            "p1",
            Context {
                reserve_supervisor_slot: Some(false),
                ..context(&clock, 3300)
            },
        )
        .await?
        .ok_or("allocated")?;
    assert_eq!(reader.lease.kind, "reader-1");
    Ok(())
}

#[tokio::test]
async fn the_slot_goes_to_the_aged_then_integrity_then_the_rest_turn_about_across_runs() -> R {
    // Decision 3 (allocation order): a wake waiting longer than a turn's limit first, then integrity, then the rest by
    // wake time; within a level the run just allocated goes after the others.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let r_old = wake(&store, &clock, "R", "D", "r-old", 0).await?;
    let r_new = wake(&store, &clock, "R", "D", "r-new", 1000).await?;
    let s_new = wake(&store, &clock, "S", "E", "s-new", 2000).await?;
    // Integrity on S, later still.
    clock.set(5000);
    event(
        &store,
        "S",
        "S/integrity",
        "integrity_violation",
        json!({"task_id": "E", "reason": "x"}),
    )
    .await?;
    let integrity = store.supervise("S", context(&clock, 5000)).await?.fired;
    let integrity = integrity
        .first()
        .ok_or("integrity fired")?
        .trigger_id
        .clone();
    let mut order = Vec::new();
    // Under 15 min: integrity first, then by wake time with R and S taking turns.
    let mut ms = 6000;
    for _ in 0..4 {
        let a = store
            .allocate_supervision("p1", context(&clock, ms))
            .await?
            .ok_or("allocated")?;
        order.push(a.wake.clone());
        end(&path, &a.attempt_id, "SETTLED")?;
        ms += 100;
    }
    assert_eq!(order, [integrity, r_old, s_new, r_new]);
    // Aged: a wake waiting over 15 min goes before a newer integrity.
    let aged = wake(&store, &clock, "R", "D", "aged", 10_000).await?;
    clock.set(16 * 60_000);
    event(
        &store,
        "R",
        "R/integrity",
        "integrity_violation",
        json!({"task_id": "D", "reason": "y"}),
    )
    .await?;
    store.supervise("R", context(&clock, 16 * 60_000)).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 16 * 60_000 + 100))
        .await?
        .ok_or("allocated")?;
    assert_eq!(a.wake, aged);
    let _ = value(&path, "SELECT 1")?;
    Ok(())
}

#[tokio::test]
async fn a_supervision_turn_moves_neither_its_runs_state_nor_its_revision() -> R {
    // Decision 3 (the invariant) and decision 5: a stalled run is BLOCKED by the pace's claim; its supervision task and
    // attempt are not work: a pace with the turn in flight, and after it ended, leaves the run's state and revision as
    // they were.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             UPDATE attempts SET state = 'FAILED', ended_at = '2027-01-15T08:00:00Z' WHERE attempt_id = 'A';
             DELETE FROM leases WHERE attempt_id = 'A';
             UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'D';"
        ),
    )?;
    store.pace_run("R", context(&clock, 0)).await?;
    let blocked = run_state(&path, "R")?;
    assert_eq!(blocked.0, "BLOCKED");
    wake(&store, &clock, "R", "D", "fp", 100).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated on a BLOCKED run")?;
    store.pace_run("R", context(&clock, 3100)).await?;
    assert_eq!(run_state(&path, "R")?, blocked);
    end(&path, &a.attempt_id, "SETTLED")?;
    store.pace_run("R", context(&clock, 3200)).await?;
    assert_eq!(run_state(&path, "R")?, blocked);
    Ok(())
}
