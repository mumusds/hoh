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

/// Take turn `attempt`'s dispatch and end it as the runner does: started, its work committed and settled when
/// `success`; else failed with `class` before it ran (a process that never started can only fail).
async fn run_turn(
    store: &Store,
    clock: &TestClock,
    attempt: &str,
    ms: u64,
    success: bool,
    class: crate::retry::FailureClass,
) -> R {
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no dispatch notice for the turn")?;
    clock.set(ms);
    assert!(store.runner_received(notice, t(ms)).await?);
    if success {
        assert!(
            store
                .runner_running(
                    attempt,
                    crate::persistence::AttemptFacts {
                        pid: Some(4242),
                        ..Default::default()
                    },
                    json!({"kind": "proc"}),
                )
                .await?
        );
        store
            .runner_effect(
                attempt,
                &format!("{attempt}/work"),
                crate::reconcile::EffectState::Started,
                crate::reconcile::EffectState::Committed,
                json!({}),
            )
            .await?;
    }
    assert!(
        store
            .runner_finished(
                attempt,
                crate::persistence::Ending {
                    success,
                    detail: if success { "answered" } else { "failed" }.to_owned(),
                    class,
                    at: t(ms + 500),
                    draw: 0,
                    observed: Default::default(),
                    unknown_effect: false,
                },
            )
            .await?
    );
    Ok(())
}

fn rearms(path: &PathBuf) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT json_object('reason', json_extract(payload_json, '$.reason'), 'failures', \
         json_extract(payload_json, '$.failures'), 'retry_not_before', json_extract(payload_json, '$.retry_not_before'), \
         'role_epoch', json_extract(payload_json, '$.role_epoch')) FROM events WHERE kind = 'supervisor.rearmed' ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|r| Ok(serde_json::from_str(r)?)).collect()
}

const TRANSIENT: crate::retry::FailureClass =
    crate::retry::FailureClass::Transient { retry_after: None };

#[tokio::test]
async fn a_failed_turn_cancels_its_supervision_task_and_rearms_its_wake_after_a_backoff() -> R {
    // Decision 3 (other endings and rearming): a supervision turn that fails is not retried by the task's policy: its
    // task is CANCELLED (no attempt counted, no need_decision); the wake is owed again (supervisor.rearmed, reason
    // failure, failures 1) and allocated again only from its retry_not_before (ADR-020's backoff), as generation 1.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    run_turn(&store, &clock, &a.attempt_id, 3000, false, TRANSIENT).await?;
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_array(state, attempt_count, next_retry_at) FROM tasks WHERE task_id = '{}'",
                a.task_id
            )
        )?,
        json!(["CANCELLED", 0, null])
    );
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'need_decision' AND run_id = 'R'"
        )?,
        json!(1),
        "only the need_decision that woke it"
    );
    // At once: rearmed, but not allocated before its backoff.
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3600))
            .await?
            .is_none()
    );
    let rearmed = rearms(&path)?;
    assert_eq!(rearmed.len(), 1);
    assert_eq!(
        (
            &rearmed[0]["reason"],
            &rearmed[0]["failures"],
            &rearmed[0]["role_epoch"]
        ),
        (&json!("failure"), &json!(1), &json!(1))
    );
    let not_before = rearmed[0]["retry_not_before"]
        .as_str()
        .ok_or("retry_not_before")?;
    let not_before_ms = crate::time::parse_rfc3339(not_before)
        .ok_or("a time")?
        .duration_since(t(0))?
        .as_millis();
    assert!(not_before_ms > 3500, "{not_before}");
    let again = store
        .allocate_supervision("p1", context(&clock, u64::try_from(not_before_ms)?))
        .await?
        .ok_or("allocated after the backoff")?;
    assert_eq!((again.wake.as_str(), again.generation), (w.as_str(), 1));
    Ok(())
}

#[tokio::test]
async fn a_withdrawn_turn_rearms_without_a_failure_or_a_backoff() -> R {
    // Decision 3: a PREPARED turn withdrawn by a pause: its supervision task CANCELLED (not READY), the wake rearmed
    // (withdrawn) with no failure counted, allocated at once when the run works again.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             UPDATE attempts SET state = 'SETTLED', ended_at = '2027-01-15T08:00:00Z' WHERE attempt_id = 'A';
             UPDATE tasks SET state = 'IMPLEMENTED', revision = 5 WHERE task_id = 'D';"
        ),
    )?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    exec(
        &path,
        "UPDATE runs SET state = 'PAUSE_REQUESTED', resume_state = 'RUNNING', revision = revision + 1 WHERE run_id = 'R';",
    )?;
    assert_eq!(
        store.pause_step("R", t(3100)).await?,
        crate::persistence::PauseStep::Paused
    );
    assert_eq!(
        value(
            &path,
            &format!("SELECT state FROM tasks WHERE task_id = '{}'", a.task_id)
        )?,
        json!("CANCELLED")
    );
    exec(
        &path,
        "UPDATE runs SET state = 'RUNNING', resume_state = NULL WHERE run_id = 'R';",
    )?;
    let again = store
        .allocate_supervision("p1", context(&clock, 3200))
        .await?
        .ok_or("allocated at once")?;
    assert_eq!(again.generation, 1);
    let rearmed = rearms(&path)?;
    assert_eq!(
        (
            &rearmed[0]["reason"],
            &rearmed[0]["failures"],
            &rearmed[0]["retry_not_before"]
        ),
        (&json!("withdrawn"), &json!(0), &Value::Null)
    );
    Ok(())
}

#[tokio::test]
async fn a_wakes_failures_count_under_the_current_commander_epoch_only() -> R {
    // M2T08D-950, -1054 (decision 3): failures are the wake's own, under the Commander's current epoch: two failures
    // count 2; after a handover to epoch 2 the next failure counts 1.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let mut ms = 3000;
    for _ in 0..2 {
        let a = store
            .allocate_supervision("p1", context(&clock, ms))
            .await?
            .ok_or("allocated")?;
        run_turn(&store, &clock, &a.attempt_id, ms, false, TRANSIENT).await?;
        store
            .allocate_supervision("p1", context(&clock, ms + 600))
            .await?;
        ms += 600_000;
    }
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    let a = store
        .allocate_supervision("p1", context(&clock, ms))
        .await?
        .ok_or("allocated at the new epoch")?;
    run_turn(&store, &clock, &a.attempt_id, ms, false, TRANSIENT).await?;
    store
        .allocate_supervision("p1", context(&clock, ms + 600))
        .await?;
    let counted: Vec<(Value, Value)> = rearms(&path)?
        .into_iter()
        .map(|r| (r["failures"].clone(), r["role_epoch"].clone()))
        .collect();
    assert_eq!(
        counted,
        [
            (json!(1), json!(1)),
            (json!(2), json!(1)),
            (json!(1), json!(2))
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_settled_attempt_records_task_implemented_for_work_only() -> R {
    // Decision 3 (the shared rule of completion): a work attempt that settles records task.implemented with its
    // task's risk_reduction (intensive's round boundary); a supervision turn that settles leaves its task IMPLEMENTED
    // and records none; the settled wake is owed nothing more.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             UPDATE tasks SET risk_reduction = 4 WHERE task_id = 'E';
             UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING' WHERE run_id = 'R';"
        ),
    )?;
    wake(&store, &clock, "S", "E", "fp", 0).await?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    run_turn(&store, &clock, &turn.attempt_id, 3000, true, TRANSIENT).await?;
    assert_eq!(
        value(
            &path,
            &format!("SELECT state FROM tasks WHERE task_id = '{}'", turn.task_id)
        )?,
        json!("IMPLEMENTED")
    );
    // E's own attempt, dispatched by a round.
    exec(
        &path,
        "INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('S', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');",
    )?;
    let work = store
        .schedule_round(
            "S",
            Context {
                required_capabilities: [("developer".to_owned(), Default::default())].into(),
                ..context(&clock, 4000)
            },
        )
        .await?
        .dispatched
        .ok_or("E dispatched")?;
    run_turn(&store, &clock, &work.attempt_id, 4000, true, TRANSIENT).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_group_array(json_object('task_id', json_extract(payload_json, '$.task_id'), \
             'risk_reduction', json_extract(payload_json, '$.risk_reduction'))) FROM events WHERE kind = 'task.implemented'"
        )?,
        json!([{"task_id": "E", "risk_reduction": 4}])
    );
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 5000))
            .await?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
async fn a_supervision_task_is_never_ready_and_takes_no_repair() -> R {
    // Appendix: rounds see READY and PENDING tasks only, and a supervision task never is either; request_repair
    // refuses a supervision task.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    run_turn(&store, &clock, &a.attempt_id, 3000, false, TRANSIENT).await?;
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM tasks WHERE role = 'commander' AND state IN ('READY', 'PENDING')"
        )?,
        json!(0)
    );
    let refused = store
        .request_repair(crate::persistence::RepairRequest {
            run_id: "R".to_owned(),
            task_id: a.task_id.clone(),
            repair: crate::persistence::Repair::Local {
                fingerprint: "F".to_owned(),
            },
            limit: 3,
            detail: "a repair of a supervision task".to_owned(),
        })
        .await;
    assert!(refused.is_err(), "{refused:?}");
    Ok(())
}

fn planned(id: &str, role: &str, deps: &[&str]) -> Value {
    json!({"task_id": id, "title": format!("task {id}"), "role": role, "writer": false, "depends_on": deps,
           "requirement_ids": ["REQ-021"]})
}

#[tokio::test]
async fn a_plan_naming_the_commander_or_a_supervision_task_is_refused() -> R {
    // Appendix (-66): the Commander does no work task (CONTRACT 6): a plan task of role commander is refused, and so is
    // one depending on a supervision task; a plain plan still imports.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-021', 's3', 'supervision', 1);"
        ),
    )?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let commander = store
        .import_plan("R", json!({"tasks": [planned("X", "commander", &[])]}))
        .await;
    assert!(
        matches!(
            commander,
            Err(crate::persistence::PersistError::Plan(
                crate::dag::PlanError::Invalid(_)
            ))
        ),
        "{commander:?}"
    );
    let behind = store
        .import_plan(
            "R",
            json!({"tasks": [planned("Y", "developer", &[a.task_id.as_str()])]}),
        )
        .await;
    assert!(
        matches!(
            behind,
            Err(crate::persistence::PersistError::Plan(
                crate::dag::PlanError::Invalid(_)
            ))
        ),
        "{behind:?}"
    );
    store
        .import_plan("R", json!({"tasks": [planned("Z", "developer", &["D"])]}))
        .await?;
    Ok(())
}

#[tokio::test]
async fn a_task_result_for_a_supervision_task_is_refused() -> R {
    // Appendix: a Commander turn answers through its outputs (decision 8), not a TaskResult: one of role commander is
    // rejected, named as a supervision task's.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == a.attempt_id)
        .ok_or("notice")?;
    assert!(store.runner_received(notice, t(3000)).await?);
    assert!(
        store
            .runner_running(
                &a.attempt_id,
                crate::persistence::AttemptFacts {
                    pid: Some(4242),
                    ..Default::default()
                },
                json!({"kind": "proc"}),
            )
            .await?
    );
    let result: crate::protocol::TaskResult = serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": a.task_id, "attempt_id": a.attempt_id, "role": "commander",
        "role_epoch": 1, "status": "COMPLETED", "summary": "done", "candidate_id": null, "evidence": [],
        "issues": [], "next_action": "verify"
    }))?;
    let outcome = store.accept_task_result(result).await?;
    let crate::persistence::TaskResultOutcome::Rejected { detail, .. } = &outcome else {
        return Err(format!("{outcome:?}").into());
    };
    assert!(detail.contains("supervision task"), "{detail}");
    let _ = value(&path, "SELECT 1")?;
    Ok(())
}
