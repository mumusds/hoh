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

/// The kinds and seqs of `run`'s supervisor events from `kinds`, in order: `kind wake [target]`.
fn supervisor(path: &PathBuf, run: &str) -> R<Vec<(i64, String)>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT seq, kind || ' ' || coalesce(json_extract(payload_json, '$.wake'), '-') \
         || coalesce(' ' || json_extract(payload_json, '$.target_attempt'), '') \
         FROM events WHERE run_id = ?1 AND kind IN ('supervisor.woken', 'supervisor.preempt') ORDER BY seq",
    )?;
    let rows = s
        .query_map([run], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// An integrity violation on `run` (task `task`) committed and its step run at `ms`: the integrity wake.
async fn integrity(
    store: &Store,
    clock: &TestClock,
    run: &str,
    task: &str,
    tag: &str,
    ms: u64,
) -> R<String> {
    clock.set(ms);
    event(
        store,
        run,
        &format!("{run}/integrity/{tag}"),
        "integrity_violation",
        json!({"task_id": task, "reason": tag, "fingerprint": format!("iv/{tag}")}),
    )
    .await?;
    let fired = store.supervise(run, context(clock, ms)).await?.fired;
    Ok(fired.first().ok_or("integrity fired")?.trigger_id.clone())
}

#[tokio::test]
async fn an_integrity_wake_preempts_its_runs_turn_once_and_takes_the_slot_next() -> R {
    // M2T08D-800, -850, -953, -1056 (decision 3): an integrity wake preempts its own run's turn in flight of a wake never
    // preempted, in the transaction of its woken; once the target has ended the slot goes to the named integrity wake
    // first, before an older aged wake of another run; the preempted wake is rearmed (preempted, no failure) and its
    // next turn is protected: a second integrity wake does not preempt it.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    // S's wake A, older, while S is paused.
    let a_wake = wake(&store, &clock, "S", "E", "a", 0).await?;
    exec(
        &path,
        "UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING' WHERE run_id = 'S';",
    )?;
    let w = wake(&store, &clock, "R", "D", "w", 2500).await?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 5000))
        .await?
        .ok_or("W allocated")?;
    assert_eq!(turn.wake, w);
    exec(
        &path,
        "UPDATE runs SET state = 'RUNNING', resume_state = NULL WHERE run_id = 'S';",
    )?;
    // 16 min later A is aged; an integrity violation on R preempts W's turn in its woken's transaction.
    let i = integrity(&store, &clock, "R", "D", "one", 16 * 60_000).await?;
    let events = supervisor(&path, "R")?;
    let woken_at = events
        .iter()
        .position(|(_, e)| *e == format!("supervisor.woken {i}"))
        .ok_or("woken")?;
    assert_eq!(
        events.get(woken_at + 1).map(|(_, e)| e.clone()),
        Some(format!("supervisor.preempt {i} {}", turn.attempt_id))
    );
    assert_eq!(
        events[woken_at + 1].0,
        events[woken_at].0 + 1,
        "the same commit"
    );
    let stops = store
        .supervision_stops("p1", context(&clock, 16 * 60_000 + 10))
        .await?;
    assert_eq!(
        stops
            .iter()
            .map(|s| (s.attempt_id.as_str(), s.reason))
            .collect::<Vec<_>>(),
        [(turn.attempt_id.as_str(), "preempted")]
    );
    // The host stops it (here: ended); the named integrity wake goes first, before the aged A.
    end(&path, &turn.attempt_id, "FAILED")?;
    let next = store
        .allocate_supervision("p1", context(&clock, 16 * 60_000 + 100))
        .await?
        .ok_or("allocated")?;
    assert_eq!(next.wake, i);
    let rearmed = rearms(&path)?;
    assert_eq!(
        (&rearmed[0]["reason"], &rearmed[0]["failures"]),
        (&json!("preempted"), &json!(0))
    );
    end(&path, &next.attempt_id, "SETTLED")?;
    // Then the aged A (other run), then W again; W's turn is protected now.
    let after = store
        .allocate_supervision("p1", context(&clock, 16 * 60_000 + 200))
        .await?
        .ok_or("allocated")?;
    assert_eq!(after.wake, a_wake);
    end(&path, &after.attempt_id, "SETTLED")?;
    let again = store
        .allocate_supervision("p1", context(&clock, 16 * 60_000 + 300))
        .await?
        .ok_or("allocated")?;
    assert_eq!(again.wake, w);
    integrity(&store, &clock, "R", "D", "two", 16 * 60_000 + 400).await?;
    assert_eq!(
        supervisor(&path, "R")?
            .iter()
            .filter(|(_, e)| e.starts_with("supervisor.preempt"))
            .count(),
        1,
        "a preempted wake's turn is not preempted again"
    );
    Ok(())
}

#[tokio::test]
async fn integrity_does_not_preempt_integrity_nor_another_runs_turn() -> R {
    // Decision 3: an integrity wake does not preempt an integrity wake's turn, nor a turn of another run.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    integrity(&store, &clock, "R", "D", "first", 0).await?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 100))
        .await?
        .ok_or("allocated")?;
    integrity(&store, &clock, "R", "D", "second", 200).await?;
    integrity(&store, &clock, "S", "E", "other", 300).await?;
    end(&path, &turn.attempt_id, "SETTLED")?;
    let preempts: i64 = Connection::open(&path)?.query_row(
        "SELECT count(*) FROM events WHERE kind = 'supervisor.preempt'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(preempts, 0);
    Ok(())
}

#[tokio::test]
async fn a_turn_past_its_limit_is_stopped_and_counts_as_a_failure() -> R {
    // Decision 3 (a turn's limit): a supervision turn in flight longer than 15 min is named for the host to stop
    // (turn_limit, recorded once); a PREPARED one is withdrawn in the store (its task CANCELLED, its notice settled,
    // its slot freed) and its wake rearmed as a failure.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    assert!(
        store
            .supervision_stops("p1", context(&clock, 3000 + 14 * 60_000))
            .await?
            .is_empty()
    );
    let late = 3000 + 15 * 60_000 + 1;
    let stops = store.supervision_stops("p1", context(&clock, late)).await?;
    assert_eq!(
        stops
            .iter()
            .map(|s| (s.attempt_id.as_str(), s.reason, s.state.as_str()))
            .collect::<Vec<_>>(),
        [(turn.attempt_id.as_str(), "turn_limit", "PREPARED")]
    );
    store
        .supervision_stops("p1", context(&clock, late + 1))
        .await?;
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.turn_limit'"
        )?,
        json!(1)
    );
    assert!(store.withdraw_turn(&turn.attempt_id, t(late + 2)).await?);
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_array(a.state, t.state, (SELECT state FROM outbox WHERE json_extract(payload_json, '$.attempt_id') = a.attempt_id)) \
                 FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE a.attempt_id = '{}'",
                turn.attempt_id
            )
        )?,
        json!(["CANCELLED", "CANCELLED", "ACKED"])
    );
    store
        .allocate_supervision("p1", context(&clock, late + 3))
        .await?;
    let rearmed = rearms(&path)?;
    assert_eq!(
        (&rearmed[0]["reason"], &rearmed[0]["failures"]),
        (&json!("failure"), &json!(1))
    );
    Ok(())
}

/// R's state, blocked reasons and the supervisor's open claims (`reason source`).
fn blocked(path: &PathBuf) -> R<(String, Value, Vec<String>)> {
    let (state, reasons): (String, String) = Connection::open(path)?.query_row(
        "SELECT state, blocked_reasons_json FROM runs WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT kind, json_extract(payload_json, '$.reason'), json_extract(payload_json, '$.source') FROM events \
         WHERE run_id = 'R' AND kind IN ('run.blocker_claimed', 'run.blocker_released') ORDER BY seq",
    )?;
    let mut open: Vec<String> = Vec::new();
    for row in s.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })? {
        let (kind, reason, source) = row?;
        let claim = format!("{reason} {source}");
        if kind == "run.blocker_claimed" {
            open.push(claim);
        } else {
            open.retain(|c| *c != claim);
        }
    }
    Ok((state, serde_json::from_str(&reasons)?, open))
}

fn unavailable(path: &PathBuf) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT kind, json_extract(payload_json, '$.reasons') FROM events WHERE run_id = 'R' \
         AND kind IN ('supervisor.unavailable', 'supervisor.available') ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(kind, reasons)| {
            Ok(json!([
                kind,
                reasons
                    .map(|r| serde_json::from_str::<Value>(&r))
                    .transpose()?
            ]))
        })
        .collect()
}

#[tokio::test]
async fn a_run_whose_commander_is_unavailable_is_blocked_while_it_owes_a_decision() -> R {
    // M2-T08 done_when; docs/adr/ADR-025.md decision 5: the commander's binding BLOCKED makes it unavailable; with a
    // need_decision wake owed the run is BLOCKED by the supervisor's claims (COMMANDER_UNAVAILABLE, NEED_DECISION) and
    // nothing is allocated; the binding ACTIVE again, the claims are released, the run is back at work and the wake is
    // allocated. Unavailable with nothing owed blocks nothing.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH' WHERE run_id = 'R' AND role = 'commander';"
        ),
    )?;
    store.supervise("R", context(&clock, 0)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING", "nothing owed: not blocked");
    let w = wake(&store, &clock, "R", "D", "fp", 100).await?;
    assert_eq!(
        blocked(&path)?,
        (
            "BLOCKED".to_owned(),
            json!(["COMMANDER_UNAVAILABLE", "NEED_DECISION"]),
            vec![
                "COMMANDER_UNAVAILABLE supervisor".to_owned(),
                "NEED_DECISION supervisor".to_owned()
            ]
        )
    );
    assert_eq!(
        unavailable(&path)?,
        [json!(["supervisor.unavailable", ["binding_blocked"]])]
    );
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.blocked'"
        )?,
        json!(1)
    );
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3000))
            .await?
            .is_none()
    );
    exec(
        &path,
        "UPDATE role_bindings SET state = 'ACTIVE', blocked_reason = NULL WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", context(&clock, 4000)).await?;
    assert_eq!(
        blocked(&path)?,
        ("RUNNING".to_owned(), json!([]), Vec::<String>::new())
    );
    assert_eq!(
        unavailable(&path)?,
        [
            json!(["supervisor.unavailable", ["binding_blocked"]]),
            json!(["supervisor.available", null])
        ]
    );
    let a = store
        .allocate_supervision("p1", context(&clock, 4100))
        .await?
        .ok_or("allocated once available")?;
    assert_eq!(a.wake, w);
    Ok(())
}

/// Fail the wake's turns until it has `n` failures, each allocated after the previous backoff; the time after.
async fn fail_turns(
    store: &Store,
    clock: &TestClock,
    n: usize,
    mut ms: u64,
    class: crate::retry::FailureClass,
) -> R<u64> {
    for i in 0..n {
        let a = store
            .allocate_supervision("p1", context(clock, ms))
            .await?
            .ok_or(format!("failure {i}: allocated"))?;
        run_turn(store, clock, &a.attempt_id, ms, false, class).await?;
        ms += 301_000;
    }
    Ok(ms)
}

#[tokio::test]
async fn a_wake_failing_up_to_its_cap_blocks_its_run_until_a_probe_of_it_settles() -> R {
    // M2T08D-950, -1054 (decisions 3 and 5): MAX_RETRIES failures of one wake under the current epoch make the
    // Commander unavailable: the run is BLOCKED, no ordinary allocation; past the cooldown one probe is allocated, to
    // the capped wake, on the BLOCKED run; once it settles the supervisor's claims are released and the run works.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let mut ms = fail_turns(&store, &clock, 8, 3000, TRANSIENT).await?;
    // The eighth failure's rearm, then the step that judges it.
    assert!(
        store
            .allocate_supervision("p1", context(&clock, ms))
            .await?
            .is_none()
    );
    store.supervise("R", context(&clock, ms)).await?;
    assert_eq!(
        blocked(&path)?.1,
        json!(["COMMANDER_UNAVAILABLE", "NEED_DECISION"])
    );
    assert_eq!(
        unavailable(&path)?,
        [json!(["supervisor.unavailable", ["failures_capped"]])]
    );
    // Within the cooldown: no probe.
    assert!(
        store
            .allocate_supervision("p1", context(&clock, ms + 1000))
            .await?
            .is_none()
    );
    ms += 300_000;
    let probe = store
        .allocate_supervision("p1", context(&clock, ms))
        .await?
        .ok_or("the probe")?;
    assert_eq!(probe.wake, w);
    assert_eq!(
        blocked(&path)?.0,
        "BLOCKED",
        "the probe runs on the BLOCKED run"
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.wake') FROM events WHERE kind = 'supervisor.probe'"
        )?,
        json!(w)
    );
    run_turn(&store, &clock, &probe.attempt_id, ms, true, TRANSIENT).await?;
    store.supervise("R", context(&clock, ms + 1000)).await?;
    assert_eq!(
        blocked(&path)?,
        ("RUNNING".to_owned(), json!([]), Vec::<String>::new())
    );
    Ok(())
}

#[tokio::test]
async fn failures_of_an_earlier_epoch_and_an_auth_failure_are_judged_by_the_current_commander() -> R
{
    // M2T08D-1054a (decision 5): a handover to a new Commander epoch leaves its failures behind: available at once; an
    // Auth failure of a turn makes it unavailable at once (reason auth).
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let ms = fail_turns(&store, &clock, 8, 3000, TRANSIENT).await?;
    store
        .allocate_supervision("p1", context(&clock, ms))
        .await?;
    store.supervise("R", context(&clock, ms)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", context(&clock, ms + 1000)).await?;
    assert_eq!(
        blocked(&path)?.0,
        "RUNNING",
        "the new epoch has no failures"
    );
    let a = store
        .allocate_supervision("p1", context(&clock, ms + 2000))
        .await?
        .ok_or("allocated at the new epoch")?;
    run_turn(
        &store,
        &clock,
        &a.attempt_id,
        ms + 2000,
        false,
        crate::retry::FailureClass::Auth,
    )
    .await?;
    store
        .allocate_supervision("p1", context(&clock, ms + 3000))
        .await?;
    store.supervise("R", context(&clock, ms + 3000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    assert_eq!(
        unavailable(&path)?.last().cloned(),
        Some(json!(["supervisor.unavailable", ["auth"]]))
    );
    Ok(())
}

/// A Commander decision for R answering `wake` (its causation), with the envelope `revision` and `epoch`.
fn decision(
    id: &str,
    wake: &str,
    revision: u64,
    epoch: u64,
    payload: Value,
) -> R<crate::decision::Proposal> {
    let message: crate::protocol::Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": epoch, "correlation_id": "c",
        "causation_id": wake, "idempotency_key": format!("k-{id}"), "expected_revision": revision,
        "spec_sha256": format!("{:064}", 3), "candidate_id": null, "kind": "decision", "payload": payload
    }))?;
    Ok(crate::decision::Proposal::from_message(&message)?)
}

fn assumption() -> Value {
    json!({"kind": "record_assumption", "statement": "s", "basis": "b"})
}

fn retriggers(path: &PathBuf) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT json_object('decision_id', json_extract(payload_json, '$.decision_id'), 'reason', \
         json_extract(payload_json, '$.reason'), 'base_fingerprint', json_extract(payload_json, '$.base_fingerprint'), \
         'task_ids', json(json_extract(payload_json, '$.task_ids')), 'own_mistake', CASE json_extract(payload_json, '$.own_mistake') WHEN 1 THEN json('true') ELSE json('false') END, \
         'n', json_extract(payload_json, '$.n')) FROM events WHERE kind = 'supervisor.retrigger' ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.iter().map(|r| Ok(serde_json::from_str(r)?)).collect()
}

#[tokio::test]
async fn a_decision_on_a_changed_task_is_stale_and_retriggered_once_with_the_new_revision() -> R {
    // REQ-021 behaviour 6, docs/adr/ADR-025.md decision 6 (the task-snapshot fence): the wake's turn was shown D at
    // revision 4; D moves to 5 before the answer; the answer, at the run's current revision and the Commander's
    // epoch, is REJECTED STALE_REVISION with exactly one supervisor.retrigger naming D; the re-trigger fires after
    // the debounce, its turn is shown D at 5, and its answer is APPLIED.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let first_turn = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    exec(&path, "UPDATE tasks SET revision = 5 WHERE task_id = 'D';")?;
    let first = store
        .decide(
            decision("d1", &w, 1, 1, assumption())?,
            context(&clock, 3100),
        )
        .await?;
    assert!(
        matches!(
            &first,
            crate::persistence::Decided::Rejected {
                reason: crate::protocol::RejectionReason::StaleRevision,
                ..
            }
        ),
        "{first:?}"
    );
    assert_eq!(
        retriggers(&path)?,
        [
            json!({"decision_id": "d1", "reason": "STALE_REVISION", "base_fingerprint": "fp", "task_ids": ["D"],
                "own_mistake": false, "n": 0})
        ]
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_array(json_extract(payload_json, '$.causation_id'), json_extract(payload_json, '$.decided_at') IS NOT NULL) FROM events WHERE kind = 'decision.rejected'"
        )?,
        json!([w, 1])
    );
    let fired = store.supervise("R", context(&clock, 5200)).await?.fired;
    let [again] = fired.as_slice() else {
        return Err(format!("{fired:?}").into());
    };
    assert_eq!(again.fingerprint, "fp#retrigger");
    // The stale answer's turn completed: its wake is answered; the re-trigger carries it on.
    end(&path, &first_turn.attempt_id, "SETTLED")?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 5300))
        .await?
        .ok_or("the re-trigger allocated")?;
    assert_eq!(turn.wake, again.trigger_id);
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_extract(payload_json, '$.snapshots') FROM events WHERE kind = 'supervisor.allocated' AND json_extract(payload_json, '$.wake') = '{}'",
                turn.wake
            )
        )?,
        json!({"D": 5})
    );
    let second = store
        .decide(
            decision("d2", &turn.wake, 1, 1, assumption())?,
            context(&clock, 5400),
        )
        .await?;
    assert!(
        matches!(second, crate::persistence::Decided::Applied { .. }),
        "{second:?}"
    );
    assert_eq!(retriggers(&path)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn only_stale_revisions_and_epochs_retrigger_and_a_commanders_own_mistake_is_backed_off() -> R
{
    // Decision 6: STALE_ROLE_EPOCH re-triggers; SPEC_MISMATCH does not. Envelope values that differ from what the
    // turn was given are the Commander's own mistake: n counts them in a row, the re-triggered wake is allocated only
    // from its woken time + retry::bound(n - 1); an APPLIED decision sets n back to 0.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    // Not what it was given: revision 0 (it was given 1).
    store
        .decide(
            decision("d1", &w, 0, 1, assumption())?,
            context(&clock, 3100),
        )
        .await?;
    // A wrong epoch.
    store
        .decide(
            decision("d2", &w, 1, 7, assumption())?,
            context(&clock, 3200),
        )
        .await?;
    // A wrong specification: no re-trigger.
    let mut wrong = decision("d3", &w, 1, 1, assumption())?;
    wrong.spec_sha256 = format!("{:064}", 9);
    store.decide(wrong, context(&clock, 3300)).await?;
    let seen: Vec<(Value, Value, Value)> = retriggers(&path)?
        .into_iter()
        .map(|r| {
            (
                r["reason"].clone(),
                r["own_mistake"].clone(),
                r["n"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (json!("STALE_REVISION"), json!(true), json!(1)),
            (json!("STALE_ROLE_EPOCH"), json!(true), json!(2))
        ]
    );
    // The re-trigger (both join one trigger: the later backoff, n = 2: 4 s from its woken time).
    end(&path, &a.attempt_id, "SETTLED")?;
    let fired = store.supervise("R", context(&clock, 5300)).await?.fired;
    assert_eq!(fired.len(), 1);
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 5300 + 3999))
            .await?
            .is_none()
    );
    let turn = store
        .allocate_supervision("p1", context(&clock, 5300 + 4000))
        .await?
        .ok_or("allocated after the backoff")?;
    let applied = store
        .decide(
            decision("d4", &turn.wake, 1, 1, assumption())?,
            context(&clock, 9400),
        )
        .await?;
    assert!(
        matches!(applied, crate::persistence::Decided::Applied { .. }),
        "{applied:?}"
    );
    store
        .decide(
            decision("d5", &turn.wake, 0, 1, assumption())?,
            context(&clock, 9500),
        )
        .await?;
    assert_eq!(
        retriggers(&path)?.last().map(|r| r["n"].clone()),
        Some(json!(1))
    );
    Ok(())
}

#[tokio::test]
async fn a_decision_id_seen_again_is_replayed_or_a_key_reuse() -> R {
    // Decision 6: the same decision id with the same content is a replay (nothing written again); with other content a
    // key reuse (nothing written); neither is an integrity violation.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let first = store
        .decide(
            decision("d1", &w, 1, 1, assumption())?,
            context(&clock, 3100),
        )
        .await?;
    assert!(
        matches!(first, crate::persistence::Decided::Applied { .. }),
        "{first:?}"
    );
    let events = value(&path, "SELECT count(*) FROM events")?;
    let again = store
        .decide(
            decision("d1", &w, 1, 1, assumption())?,
            context(&clock, 3200),
        )
        .await?;
    assert_eq!(
        again,
        crate::persistence::Decided::Replayed {
            state: "APPLIED".to_owned()
        }
    );
    let other = store
        .decide(
            decision(
                "d1",
                &w,
                1,
                1,
                json!({"kind": "record_assumption", "statement": "other", "basis": "b"}),
            )?,
            context(&clock, 3300),
        )
        .await?;
    assert_eq!(other, crate::persistence::Decided::KeyReuse);
    assert_eq!(value(&path, "SELECT count(*) FROM events")?, events);
    Ok(())
}

/// sha256 of `text`, lower-case hex.
fn sha256(text: &str) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The nonce turn `attempt`'s dispatch notice carries.
fn nonce_of(path: &PathBuf, attempt: &str) -> R<String> {
    Ok(Connection::open(path)?.query_row(
        "SELECT json_extract(payload_json, '$.nonce') FROM outbox WHERE json_extract(payload_json, '$.attempt_id') = ?1",
        [attempt],
        |r| r.get(0),
    )?)
}

/// One line of the file bridge: `message` wrapped with `nonce`.
fn line(nonce: &str, message: Value) -> String {
    json!({"nonce": nonce, "message": message}).to_string()
}

/// A Commander message of R's turn `attempt` answering `wake`.
fn output(id: &str, kind: &str, wake: &str, attempt: &str, revision: u64, payload: Value) -> Value {
    json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": attempt,
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 1, "correlation_id": "c",
        "causation_id": wake, "idempotency_key": format!("k-{id}"), "expected_revision": revision,
        "spec_sha256": format!("{:064}", 3), "candidate_id": null, "kind": kind, "payload": payload
    })
}

/// Run turn `attempt` to its end with `lines` in its file bridge: OBSERVED with them, then SETTLED or FAILED.
async fn bridged_turn(
    store: &Store,
    clock: &TestClock,
    attempt: &str,
    ms: u64,
    success: bool,
    lines: &[String],
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
    assert!(
        store
            .runner_effect(
                attempt,
                &format!("{attempt}/work"),
                crate::reconcile::EffectState::Started,
                crate::reconcile::EffectState::Observed,
                json!({"success": success, "detail": "turn", "messages": lines}),
            )
            .await?
    );
    assert!(
        store
            .runner_finished(
                attempt,
                crate::persistence::Ending {
                    success,
                    detail: "turn".to_owned(),
                    class: TRANSIENT,
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

fn ends(path: &PathBuf) -> R<Vec<(String, String)>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT json_extract(payload_json, '$.output_id'), CASE kind WHEN 'supervisor.output_disposed' \
         THEN json_extract(payload_json, '$.outcome') || coalesce(':' || json_extract(payload_json, '$.reason'), '') \
         ELSE kind END FROM events WHERE json_extract(payload_json, '$.output_id') IS NOT NULL \
         AND kind IN ('supervisor.output_disposed', 'decision.applied', 'decision.rejected') ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[tokio::test]
async fn a_turn_has_a_nonce_and_its_outputs_are_recorded_only_when_it_succeeds() -> R {
    // docs/adr/ADR-025.md decision 8 (-100, -258, -266): the dispatch notice carries the turn's nonce, the allocation
    // only its sha256; a turn that succeeds has each line of its bridge recorded as supervisor.output in the
    // transaction of its OBSERVED work (not repeated in that event's detail); a failed turn's lines are discarded.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    assert_eq!(nonce.len(), 64, "{nonce}");
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.nonce_sha256') FROM events WHERE kind = 'supervisor.allocated'"
        )?,
        json!(sha256(&nonce))
    );
    let lines = [
        line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, assumption()),
        ),
        line(
            &nonce,
            output("m2", "decision", &w, &a.attempt_id, 1, assumption()),
        ),
    ];
    bridged_turn(&store, &clock, &a.attempt_id, 3000, true, &lines).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_group_array(json_extract(payload_json, '$.output_id')) FROM events WHERE kind = 'supervisor.output'"
        )?,
        json!([
            format!("{}/output/0", a.attempt_id),
            format!("{}/output/1", a.attempt_id)
        ])
    );
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT json_extract(payload_json, '$.detail.messages') IS NULL FROM events WHERE kind = 'effect.observed' AND json_extract(payload_json, '$.attempt_id') = '{}'",
                a.attempt_id
            )
        )?,
        json!(1)
    );
    // A failed turn: its lines discarded.
    let w2 = wake(&store, &clock, "R", "D", "fp2", 4000).await?;
    let b = store
        .allocate_supervision("p1", context(&clock, 6100))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &b.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &b.attempt_id,
        6100,
        false,
        &[line(
            &nonce,
            output("m1", "decision", &w2, &b.attempt_id, 1, assumption()),
        )],
    )
    .await?;
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.output'"
        )?,
        json!(2)
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.count') FROM events WHERE kind = 'supervisor.output_discarded'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn each_output_of_a_turn_ends_exactly_once() -> R {
    // Decision 8 (-109, -150, -206, -263a): the supervisor's step disposes of each recorded output once, in order: a
    // decision through decide in the output's transaction (its decision event names the output), the same message again
    // a replay, its id with other content a key reuse, another kind deferred, a wrong nonce or causation invalid, a
    // request for a person past decide's fences (at the revision the applied m1 left) unsupported with NEED_DECISION
    // claimed for it (commander:<decision id>), its decisions row PROPOSED, the same request again a replay (v13,
    // M2T08R-02); a later step writes nothing more.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    let at = a.attempt_id.as_str();
    let ask = json!({"kind": "request_user_decision", "question_id": "q1", "question": "which?", "requires_user": true});
    let lines = [
        line(&nonce, output("m1", "decision", &w, at, 1, assumption())),
        line(&nonce, output("m1", "decision", &w, at, 1, assumption())),
        line(
            &nonce,
            output(
                "m1",
                "decision",
                &w,
                at,
                1,
                json!({"kind": "record_assumption", "statement": "x", "basis": "y"}),
            ),
        ),
        line(
            &nonce,
            output("m2", "steer", &w, at, 1, json!({"text": "go on"})),
        ),
        line(
            "not-the-nonce",
            output("m3", "decision", &w, at, 1, assumption()),
        ),
        line(
            &nonce,
            output("m4", "decision", "another-wake", at, 1, assumption()),
        ),
        "not json".to_owned(),
        line(&nonce, output("m5", "decision", &w, at, 2, ask.clone())),
        line(&nonce, output("m5", "decision", &w, at, 2, ask)),
    ];
    bridged_turn(&store, &clock, at, 3000, true, &lines).await?;
    store.supervise("R", context(&clock, 4000)).await?;
    let o = |n: usize| format!("{at}/output/{n}");
    assert_eq!(
        ends(&path)?,
        [
            (o(0), "decision.applied".to_owned()),
            (o(1), "replay".to_owned()),
            (o(2), "invalid:KEY_REUSE".to_owned()),
            (o(3), "deferred".to_owned()),
            (o(4), "invalid:NONCE".to_owned()),
            (o(5), "invalid:CAUSATION".to_owned()),
            (o(6), "invalid:FORMAT".to_owned()),
            (o(7), "unsupported".to_owned()),
            (o(8), "replay".to_owned()),
        ]
    );
    assert_eq!(
        value(
            &path,
            &format!("SELECT state FROM decisions WHERE decision_id = '{at}/m5'")
        )?,
        json!("PROPOSED")
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.decision_id') FROM events WHERE kind = 'decision.applied'"
        )?,
        json!(format!("{at}/m1"))
    );
    assert_eq!(
        blocked(&path)?,
        (
            "BLOCKED".to_owned(),
            json!(["NEED_DECISION"]),
            vec![format!("NEED_DECISION commander:{at}/m5")]
        )
    );
    let count = value(&path, "SELECT count(*) FROM events")?;
    store.supervise("R", context(&clock, 5000)).await?;
    assert_eq!(value(&path, "SELECT count(*) FROM events")?, count);
    Ok(())
}

#[tokio::test]
async fn outputs_wait_while_their_run_is_paused_and_are_superseded_when_it_is_cancelled() -> R {
    // Decision 6 (non-working states): a paused run's outputs are not disposed of; a cancelled run's are superseded.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &a.attempt_id,
        3000,
        true,
        &[line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, assumption()),
        )],
    )
    .await?;
    exec(
        &path,
        "UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING' WHERE run_id = 'R';",
    )?;
    store.supervise("R", context(&clock, 4000)).await?;
    assert!(ends(&path)?.is_empty());
    exec(
        &path,
        "UPDATE runs SET state = 'CANCEL_REQUESTED' WHERE run_id = 'R';",
    )?;
    store.supervise("R", context(&clock, 5000)).await?;
    assert_eq!(
        ends(&path)?,
        [(
            format!("{}/output/0", a.attempt_id),
            "superseded".to_owned()
        )]
    );
    Ok(())
}

#[tokio::test]
async fn a_turns_assignment_carries_its_wake_its_nonce_and_a_bounded_summary() -> R {
    // Decision 7 (-05, -110, -262, -852): a supervision turn's dispatch gives the executor what the Commander is shown:
    // the one wake it answers (its events, most severe first, at most 50, each payload bounded), the revisions of its
    // tasks and the run and the Commander's epoch it is to name, the run's specification and candidate, the other wakes
    // still owed (id and fingerprint only), the roles' bindings, the run's recent rejected answers, and the turn's
    // nonce.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    clock.set(0);
    store
        .commit(Commit {
            events: (0..60)
                .map(|i| NewEvent {
                    project_id: "p1".to_owned(),
                    event_id: format!("nd/{i}"),
                    run_id: Some("R".to_owned()),
                    kind: "need_decision".to_owned(),
                    payload: json!({"task_id": "D", "fingerprint": "fp", "n": i, "detail": "x".repeat(10_000)}),
                })
                .collect(),
            ..Commit::default()
        })
        .await?;
    let w = store.supervise("R", context(&clock, 2000)).await?.fired[0]
        .trigger_id
        .clone();
    let other = wake(&store, &clock, "R", "D", "other", 2100).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 4200))
        .await?
        .ok_or("allocated")?;
    assert_eq!(a.wake, w);
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == a.attempt_id)
        .ok_or("no dispatch notice")?;
    let s = notice
        .assignment
        .supervision
        .ok_or("no supervision input")?;
    assert_eq!(s.wake, w);
    assert_eq!(s.nonce, nonce_of(&path, &a.attempt_id)?);
    assert_eq!((s.run_revision, s.role_epoch), (1, 1));
    let summary = &s.summary;
    assert_eq!(summary["wake"]["id"], json!(w));
    assert_eq!(summary["snapshots"], json!({"D": 4}));
    assert_eq!(summary["run"]["spec_sha256"], json!(format!("{:064}", 3)));
    let events = summary["events"].as_array().ok_or("events")?;
    assert_eq!(events.len(), 50);
    assert_eq!(summary["events_total"], json!(60));
    assert!(
        events.iter().all(|e| e.to_string().len() < 5_000),
        "bounded payloads"
    );
    assert_eq!(
        summary["other_wakes"],
        json!([{"id": other, "fingerprint": "other", "severity": "high"}])
    );
    let roles: Vec<&str> = summary["roles"]
        .as_array()
        .ok_or("roles")?
        .iter()
        .filter_map(|r| r["role"].as_str())
        .collect();
    assert_eq!(roles, ["commander", "developer"]);
    assert!(summary["recent_rejections"].is_array());
    // A work task's assignment has none.
    let work = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .all(|d| d.attempt_id == a.attempt_id || d.assignment.supervision.is_none());
    assert!(work);
    Ok(())
}

fn deferred(path: &PathBuf) -> R<Vec<String>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'supervisor.deferred' ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

#[tokio::test]
async fn a_commander_with_no_session_is_deferred_once_then_unavailable_past_the_limit() -> R {
    // Decision 3 ("not active yet"): an owed wake whose Commander has no session waits, with one
    // supervisor.deferred{NO_SESSION}; past DEFER_LIMIT (30 min) still so, the Commander is unavailable (inactive) and
    // the run BLOCKED; once it has its session it is available and the wake allocated.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD} UPDATE role_bindings SET session_id = NULL WHERE run_id = 'R' AND role = 'commander';"
        ),
    )?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 3000))
            .await?
            .is_none()
    );
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 4000))
            .await?
            .is_none()
    );
    assert_eq!(deferred(&path)?, ["NO_SESSION"]);
    store
        .supervise("R", context(&clock, 3000 + 29 * 60_000))
        .await?;
    assert_eq!(blocked(&path)?.0, "RUNNING", "within the limit");
    store
        .supervise("R", context(&clock, 3000 + 30 * 60_000 + 1))
        .await?;
    assert_eq!(
        blocked(&path)?.1,
        json!(["COMMANDER_UNAVAILABLE", "NEED_DECISION"])
    );
    assert_eq!(
        unavailable(&path)?.last().cloned(),
        Some(json!(["supervisor.unavailable", ["inactive"]]))
    );
    exec(
        &path,
        "UPDATE role_bindings SET session_id = 'sc' WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store
        .supervise("R", context(&clock, 31 * 60_000 + 5000))
        .await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    let a = store
        .allocate_supervision("p1", context(&clock, 31 * 60_000 + 6000))
        .await?
        .ok_or("allocated")?;
    assert_eq!(a.wake, w);
    Ok(())
}

#[tokio::test]
async fn a_turn_on_a_reader_slot_waits_while_work_holds_them_all() -> R {
    // Decision 3: with reserve_supervisor_slot = false, an owed wake waits while work holds every reader slot, with
    // one supervisor.deferred{NO_SLOT}.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
               ('W1', 'R', 's3', 'RUNNING', 2, 'developer', 0, 5), ('W2', 'R', 's3', 'RUNNING', 2, 'developer', 0, 6);
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
               ('AW1', 'W1', 'RUNNING', 1, 1, 1), ('AW2', 'W2', 'RUNNING', 1, 1, 1);
             INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
               ('p1', 'reader-1', 'AW1', 1, '2100-01-01T00:00:00Z'), ('p1', 'reader-2', 'AW2', 1, '2100-01-01T00:00:00Z');"
        ),
    )?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let readers = |ms| Context {
        reserve_supervisor_slot: Some(false),
        ..context(&clock, ms)
    };
    assert!(
        store
            .allocate_supervision("p1", readers(3000))
            .await?
            .is_none()
    );
    assert!(
        store
            .allocate_supervision("p1", readers(4000))
            .await?
            .is_none()
    );
    assert_eq!(deferred(&path)?, ["NO_SLOT"]);
    Ok(())
}

#[tokio::test]
async fn a_commander_session_is_given_its_own_directory_outside_the_project_once() -> R {
    // M2T08D-551, -652, -955, -1057 (decision 3): with the host's base directory for Commanders, the first turn of a
    // Commander session records commander.workdir{session_id, path}: <base>/<project digest>/<session>; its later turns
    // use the recorded path (no second record) and are told it.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let base = dir.path().join("state/hoh/commander");
    let with_base = |ms| Context {
        commander_dir: Some(base.clone()),
        ..context(&clock, ms)
    };
    wake(&store, &clock, "R", "D", "f1", 0).await?;
    let a = store
        .allocate_supervision("p1", with_base(3000))
        .await?
        .ok_or("allocated")?;
    end(&path, &a.attempt_id, "SETTLED")?;
    wake(&store, &clock, "R", "D", "f2", 4000).await?;
    let b = store
        .allocate_supervision("p1", with_base(7000))
        .await?
        .ok_or("allocated")?;
    let recorded = value(
        &path,
        "SELECT json_group_array(json_object('session', json_extract(payload_json, '$.session_id'), 'path', \
         json_extract(payload_json, '$.path'))) FROM events WHERE kind = 'commander.workdir'",
    )?;
    let digest = &sha256("p1")[..16];
    let expected = base.join(digest).join("sc").display().to_string();
    assert_eq!(recorded, json!([{"session": "sc", "path": expected}]));
    let told = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == b.attempt_id)
        .and_then(|d| d.assignment.supervision)
        .and_then(|s| s.workdir);
    assert_eq!(told.as_deref(), Some(expected.as_str()));
    Ok(())
}

#[tokio::test]
async fn a_commander_whose_agent_the_registry_finds_unavailable_waits_for_a_newer_look() -> R {
    // Decision 5 (-306, -355): the registry's last look at the Commander's agent finding it unavailable (or not logged
    // in) makes the Commander unavailable, the look's identity recorded with the agent it is of (v16, M2T08R-200); a host that has not looked yet leaves it so;
    // only a later look (by its identity, host epoch first) finding it available ends it.
    use crate::decision::Observation;
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let seen = |ms, looks: Vec<(&str, Observation)>| Context {
        observations: Some(looks.into_iter().map(|(a, o)| (a.to_owned(), o)).collect()),
        ..context(&clock, ms)
    };
    let look = |epoch, n, available, logged_in| Observation {
        host_epoch: epoch,
        n,
        available,
        logged_in,
    };
    store
        .supervise("R", seen(3000, vec![("codex", look(1, 4, true, false))]))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    assert_eq!(
        value(
            &path,
            "SELECT json_object('reasons', json(json_extract(payload_json, '$.reasons')), 'observation', json(json_extract(payload_json, '$.observation'))) FROM events WHERE kind = 'supervisor.unavailable'"
        )?,
        json!({"reasons": ["login_missing"], "observation": {"agent": "codex", "host_epoch": 1, "n": 4}})
    );
    // A new host that has not looked yet: still unavailable.
    store.supervise("R", seen(4000, vec![])).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    // The same look again: still.
    store
        .supervise("R", seen(5000, vec![("codex", look(1, 4, true, true))]))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    // A later look (the new host's first) finding it available and logged in: available.
    store
        .supervise("R", seen(6000, vec![("codex", look(2, 1, true, true))]))
        .await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    Ok(())
}

// Implementation review round 1 (docs/adr/ADR-025.md v13: one verdict per step, before firing).

#[tokio::test]
async fn a_probe_in_flight_keeps_its_run_blocked_until_it_settles() -> R {
    // M2T08R-01, -50 (decisions 3 and 5, v13): the capped wake whose probe turn is in flight is still capped and
    // still owed a decision: a step during the probe keeps the supervisor's claims, the run BLOCKED at its revision,
    // and records no availability; only the probe's SETTLED releases them.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let mut ms = fail_turns(&store, &clock, 8, 3000, TRANSIENT).await?;
    store
        .allocate_supervision("p1", context(&clock, ms))
        .await?;
    store.supervise("R", context(&clock, ms)).await?;
    let before = run_state(&path, "R")?;
    assert_eq!(before.0, "BLOCKED");
    ms += 300_000;
    let probe = store
        .allocate_supervision("p1", context(&clock, ms))
        .await?
        .ok_or("the probe")?;
    store.supervise("R", context(&clock, ms + 100)).await?;
    assert_eq!(run_state(&path, "R")?, before, "a step during the probe");
    assert_eq!(
        unavailable(&path)?,
        [json!(["supervisor.unavailable", ["failures_capped"]])]
    );
    run_turn(&store, &clock, &probe.attempt_id, ms + 200, true, TRANSIENT).await?;
    store.supervise("R", context(&clock, ms + 1000)).await?;
    assert_eq!(
        blocked(&path)?,
        ("RUNNING".to_owned(), json!([]), Vec::<String>::new())
    );
    Ok(())
}

#[tokio::test]
async fn an_auth_unavailable_commander_probed_again_and_again_keeps_its_run_blocked() -> R {
    // M2T08R-51 (decision 5, v13): an Auth failure makes the Commander unavailable; probe after probe failing for
    // Auth keeps the run BLOCKED in one blocking episode (one supervisor.blocked), and every step succeeds.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let auth = crate::retry::FailureClass::Auth;
    run_turn(&store, &clock, &a.attempt_id, 3000, false, auth).await?;
    store
        .allocate_supervision("p1", context(&clock, 4000))
        .await?;
    store.supervise("R", context(&clock, 4000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    let mut ms = 4000;
    for n in 0..3 {
        ms += 301_000;
        let probe = store
            .allocate_supervision("p1", context(&clock, ms))
            .await?
            .ok_or(format!("probe {n}"))?;
        store.supervise("R", context(&clock, ms + 100)).await?;
        assert_eq!(blocked(&path)?.0, "BLOCKED", "probe {n} in flight");
        run_turn(&store, &clock, &probe.attempt_id, ms + 200, false, auth).await?;
        store
            .allocate_supervision("p1", context(&clock, ms + 1000))
            .await?;
        store.supervise("R", context(&clock, ms + 1000)).await?;
        assert_eq!(blocked(&path)?.0, "BLOCKED", "probe {n} failed");
    }
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.blocked'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn an_overdue_routine_trigger_is_superseded_not_woken_while_the_commander_is_unavailable() -> R
{
    // M2T08R-05 (decision 5, v13): the verdict comes before firing: with the Commander unavailable a routine
    // trigger is superseded however late the step that finds it due, never woken into an obligation that blocks
    // the run.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD}
             UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH' WHERE run_id = 'R' AND role = 'commander';"
        ),
    )?;
    clock.set(0);
    event(
        &store,
        "R",
        "R/iteration",
        "iteration.completed",
        json!({"iteration": 1}),
    )
    .await?;
    store.supervise("R", context(&clock, 3000)).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_array((SELECT count(*) FROM events WHERE kind = 'supervisor.woken'), \
             (SELECT state FROM supervisor_triggers))"
        )?,
        json!([0, "SUPERSEDED"])
    );
    assert_eq!(blocked(&path)?.0, "RUNNING");
    Ok(())
}

#[tokio::test]
async fn an_unavailable_commander_is_not_preempted_for() -> R {
    // M2T08R-08 (decision 3, v13): the preempt's condition is the replacement's allocation as the verdict judges it:
    // with the registry finding the Commander's agent unavailable, an integrity wake does not preempt the turn in
    // flight (its replacement could not be given a turn).
    use crate::decision::Observation;
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "w", 0).await?;
    store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    clock.set(4000);
    event(
        &store,
        "R",
        "R/integrity",
        "integrity_violation",
        json!({"task_id": "D", "reason": "x", "fingerprint": "iv/x"}),
    )
    .await?;
    let gone = Observation {
        host_epoch: 1,
        n: 1,
        available: false,
        logged_in: false,
    };
    store
        .supervise(
            "R",
            Context {
                observations: Some([("codex".to_owned(), gone)].into_iter().collect()),
                ..context(&clock, 4000)
            },
        )
        .await?;
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.preempt'"
        )?,
        json!(0)
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_request_for_a_person_is_rejected_and_retriggered_and_claims_nothing() -> R {
    // M2T08R-02 (decision 8, v13): request_user_decision goes through decide's fences like every decision: answered
    // by an old Commander epoch it is REJECTED (STALE_ROLE_EPOCH, its decision event names the output), re-triggered
    // once, and claims nothing.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    let ask = json!({"kind": "request_user_decision", "question_id": "q1", "question": "which?", "requires_user": true});
    bridged_turn(
        &store,
        &clock,
        &a.attempt_id,
        3000,
        true,
        &[line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, ask),
        )],
    )
    .await?;
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", context(&clock, 4000)).await?;
    assert_eq!(
        ends(&path)?,
        [(
            format!("{}/output/0", a.attempt_id),
            "decision.rejected".to_owned()
        )]
    );
    assert_eq!(
        value(
            &path,
            "SELECT json_array(state, reason_code) FROM decisions"
        )?,
        json!(["REJECTED", "STALE_ROLE_EPOCH"])
    );
    assert_eq!(retriggers(&path)?.len(), 1);
    assert_eq!(
        blocked(&path)?,
        ("RUNNING".to_owned(), json!([]), Vec::<String>::new())
    );
    Ok(())
}

#[tokio::test]
async fn a_failure_rearmed_after_a_handover_counts_under_its_turns_epoch() -> R {
    // M2T08R-53 (decision 3, v13): a turn's failure is recorded under the epoch of the attempt that failed, not the
    // binding's epoch at the rearm: a failure of epoch 1 rearmed after a handover to epoch 2 is epoch 1's.
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
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store
        .allocate_supervision("p1", context(&clock, 4000))
        .await?;
    let rearmed = rearms(&path)?;
    assert_eq!(
        (&rearmed[0]["failures"], &rearmed[0]["role_epoch"]),
        (&json!(1), &json!(1))
    );
    Ok(())
}

#[tokio::test]
async fn an_ended_runs_owed_wake_is_stepped_superseded_and_closed() -> R {
    // M2T08R-06, -54 (decision 3, v13): a run that ended with a wake owed is still stepped: its wake is superseded,
    // supervisor.closed written once nothing is left, and the run is no longer stepped.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    exec(
        &path,
        "UPDATE runs SET state = 'COMPLETED' WHERE run_id = 'R';",
    )?;
    assert!(
        store
            .runner_waiting("p1")
            .await?
            .supervised
            .contains(&"R".to_owned()),
        "an ended run with a wake owed is stepped"
    );
    store.supervise("R", context(&clock, 3000)).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_group_array(kind || ' ' || coalesce(json_extract(payload_json, '$.wake'), '-')) FROM events \
             WHERE kind IN ('supervisor.superseded', 'supervisor.closed') ORDER BY seq"
        )?,
        json!([format!("supervisor.superseded {w}"), "supervisor.closed -"])
    );
    assert!(
        !store
            .runner_waiting("p1")
            .await?
            .supervised
            .contains(&"R".to_owned()),
        "closed"
    );
    Ok(())
}

#[tokio::test]
async fn a_turns_summary_bounds_its_tasks_and_other_wakes_and_keeps_every_snapshot() -> R {
    // M2T08R-57 (decision 7, v13): at most 50 tasks and 50 other owed wakes are shown, with their totals; the
    // snapshots are never cut.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let tasks: String = (0..60)
        .map(|i| {
            format!(
                "('T{i}', 'R', 's3', 'PENDING', 1, 'developer', 0, {})",
                10 + i
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    exec(
        &path,
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES {tasks};"
        ),
    )?;
    clock.set(0);
    store
        .commit(Commit {
            events: (0..60)
                .map(|i| NewEvent {
                    project_id: "p1".to_owned(),
                    event_id: format!("nd/{i}"),
                    run_id: Some("R".to_owned()),
                    kind: "need_decision".to_owned(),
                    payload: json!({"task_id": format!("T{i}"), "fingerprint": "many"}),
                })
                .chain((0..55).map(|i| NewEvent {
                    project_id: "p1".to_owned(),
                    event_id: format!("other/{i}"),
                    run_id: Some("R".to_owned()),
                    kind: "need_decision".to_owned(),
                    payload: json!({"task_id": "D", "fingerprint": format!("other-{i}")}),
                }))
                .collect(),
            ..Commit::default()
        })
        .await?;
    store.supervise("R", context(&clock, 2000)).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 2100))
        .await?
        .ok_or("allocated")?;
    let s = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == a.attempt_id)
        .and_then(|d| d.assignment.supervision)
        .ok_or("no supervision input")?;
    let summary = &s.summary;
    let many = summary["wake"]["fingerprint"] == json!("many");
    let (tasks_shown, others) = if many { (50, 50) } else { (1, 50) };
    assert_eq!(summary["tasks"].as_array().map(Vec::len), Some(tasks_shown));
    assert_eq!(
        summary["other_wakes"].as_array().map(Vec::len),
        Some(others)
    );
    assert_eq!(summary["other_wakes_total"], json!(55));
    if many {
        assert_eq!(summary["tasks_total"], json!(60));
        assert_eq!(summary["snapshots"].as_object().map(|o| o.len()), Some(60));
    }
    Ok(())
}

/// Fault hooks that fail the `n`-th commit from when they are armed (1: the next one), and every one after it until
/// disarmed.
#[derive(Debug, Default)]
struct FailCommits {
    armed: std::sync::atomic::AtomicBool,
    n: std::sync::atomic::AtomicUsize,
    seen: std::sync::atomic::AtomicUsize,
}

impl FailCommits {
    fn arm(&self, n: usize) {
        use std::sync::atomic::Ordering::SeqCst;
        self.n.store(n, SeqCst);
        self.seen.store(0, SeqCst);
        self.armed.store(true, SeqCst);
    }

    fn disarm(&self) {
        self.armed.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

impl crate::persistence::FaultHooks for FailCommits {
    fn before_commit(&self) -> Result<(), crate::persistence::PersistError> {
        use std::sync::atomic::Ordering::SeqCst;
        if self.armed.load(SeqCst) && self.seen.fetch_add(1, SeqCst) + 1 >= self.n.load(SeqCst) {
            return Err(crate::persistence::PersistError::Storage(
                "the test failed this commit".to_owned(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn an_integrity_edges_woken_and_its_preempt_are_one_commit() -> R {
    // M2T08D-1056, M2T08R-56 (decision 3): the integrity edge's woken and its preempt are one commit: the step's first
    // commit failing leaves neither (and the cursor where it was); its second commit failing leaves both (they were in
    // the first, nothing of them waits for a second); the next step after a failure writes both, adjacent.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let hooks = std::sync::Arc::new(FailCommits::default());
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            clock: Some(StoreClock(clock.clone())),
            faults: hooks.clone(),
            ..Options::default()
        },
    )?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "w", 0).await?;
    let turn = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    clock.set(4000);
    event(
        &store,
        "R",
        "R/integrity",
        "integrity_violation",
        json!({"task_id": "D", "reason": "x", "fingerprint": "iv/x"}),
    )
    .await?;
    let cursor = value(&path, "SELECT last_handled_seq FROM supervisor_cursor")?;
    hooks.arm(1);
    assert!(store.supervise("R", context(&clock, 4000)).await.is_err());
    hooks.disarm();
    assert_eq!(
        supervisor(&path, "R")?.len(),
        1,
        "only the first wake's woken"
    );
    assert_eq!(
        value(&path, "SELECT last_handled_seq FROM supervisor_cursor")?,
        cursor
    );
    hooks.arm(2);
    let _ = store.supervise("R", context(&clock, 4100)).await;
    hooks.disarm();
    let events = supervisor(&path, "R")?;
    let [_, (woken, _), (preempt, what)] = events.as_slice() else {
        return Err(format!("{events:?}").into());
    };
    assert_eq!(*preempt, woken + 1);
    assert!(what.ends_with(&turn.attempt_id), "{what}");
    Ok(())
}

// Implementation review round 2 (docs/adr/ADR-025.md v14: the ended turns folded before the verdict; recovery only by
// its facts; an obligation until its turn's outputs are disposed). Steps run in the host's order: supervise, then
// allocate.

#[tokio::test]
async fn an_auth_unavailability_outlasts_a_probe_that_fails_for_another_reason() -> R {
    // M2T08R-100 (decision 5, v14): an Auth failure makes the Commander unavailable until a turn of its epoch settles
    // (or its epoch changes); a probe that fails for another reason only restarts the cooldown: the run stays BLOCKED
    // and no availability is recorded.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    run_turn(
        &store,
        &clock,
        &a.attempt_id,
        3000,
        false,
        crate::retry::FailureClass::Auth,
    )
    .await?;
    store.supervise("R", context(&clock, 4000)).await?;
    store
        .allocate_supervision("p1", context(&clock, 4000))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    store.supervise("R", context(&clock, 305_000)).await?;
    let probe = store
        .allocate_supervision("p1", context(&clock, 305_000))
        .await?
        .ok_or("the probe")?;
    run_turn(&store, &clock, &probe.attempt_id, 305_100, false, TRANSIENT).await?;
    store.supervise("R", context(&clock, 306_000)).await?;
    store
        .allocate_supervision("p1", context(&clock, 306_000))
        .await?;
    assert_eq!(
        blocked(&path)?.0,
        "BLOCKED",
        "a failed probe is no recovery"
    );
    assert_eq!(
        unavailable(&path)?,
        [json!(["supervisor.unavailable", ["auth"]])]
    );
    Ok(())
}

#[tokio::test]
async fn the_step_counts_an_ended_turns_failure_before_its_verdict() -> R {
    // M2T08R-102 (decisions 3 and 5, v14): the step folds the ended turns (their rearms) before its verdict, as the
    // allocation does: right after the eighth failure, the host's next step already finds the Commander unavailable,
    // supersedes a routine trigger due then, and blocks the run.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let ms = fail_turns(&store, &clock, 8, 3000, TRANSIENT).await?;
    clock.set(ms);
    event(
        &store,
        "R",
        "R/iteration",
        "iteration.completed",
        json!({"iteration": 1}),
    )
    .await?;
    store.supervise("R", context(&clock, ms + 3000)).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_array((SELECT count(*) FROM events WHERE kind = 'supervisor.woken' \
             AND json_extract(payload_json, '$.severity') = 'normal'), \
             (SELECT state FROM supervisor_triggers WHERE fingerprint LIKE 'iteration.completed%'))"
        )?,
        json!([0, "SUPERSEDED"])
    );
    assert_eq!(
        blocked(&path)?.1,
        json!(["COMMANDER_UNAVAILABLE", "NEED_DECISION"])
    );
    Ok(())
}

/// A context at `ms` whose registry's look finds the Commander's agent (codex) unavailable.
fn agent_gone(clock: &TestClock, ms: u64) -> Context {
    Context {
        observations: Some(
            [(
                "codex".to_owned(),
                crate::decision::Observation {
                    host_epoch: 1,
                    n: 1,
                    available: false,
                    logged_in: true,
                },
            )]
            .into_iter()
            .collect(),
        ),
        ..context(clock, ms)
    }
}

#[tokio::test]
async fn a_turn_in_flight_when_its_commander_becomes_unavailable_is_answered_and_blocks_nothing()
-> R {
    // M2T08R-150 (decision 3, v15): an ordinary turn, allocated while its Commander was available, is answering its
    // wake: the Commander becoming unavailable mid-turn does not block the run (state and revision unchanged), the
    // turn's answer is APPLIED, nothing is re-triggered; a new decision owed while it is still unavailable blocks.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    store.supervise("R", agent_gone(&clock, 3100)).await?;
    assert_eq!(run_state(&path, "R")?, ("RUNNING".to_owned(), 1));
    let nonce = nonce_of(&path, &a.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &a.attempt_id,
        3200,
        true,
        &[line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, assumption()),
        )],
    )
    .await?;
    store.supervise("R", agent_gone(&clock, 4000)).await?;
    assert_eq!(
        ends(&path)?,
        [(
            format!("{}/output/0", a.attempt_id),
            "decision.applied".to_owned()
        )]
    );
    assert!(retriggers(&path)?.is_empty());
    assert_eq!(blocked(&path)?.0, "RUNNING");
    clock.set(5000);
    event(
        &store,
        "R",
        "R/second",
        "need_decision",
        json!({"task_id": "D", "fingerprint": "second"}),
    )
    .await?;
    store.supervise("R", agent_gone(&clock, 7000)).await?;
    assert_eq!(
        blocked(&path)?.1,
        json!(["COMMANDER_UNAVAILABLE", "NEED_DECISION"])
    );
    Ok(())
}

#[tokio::test]
async fn a_probe_settled_while_its_commander_is_unavailable_otherwise_is_owed_until_its_outputs_are_disposed_of()
-> R {
    // M2T08R-103 (decision 3, v15): a probe's wake is a blocking obligation while the probe is in flight and, once it
    // settled with the Commander unavailable for another reason (the registry lost its agent meanwhile), until its
    // outputs are disposed of: the step keeps the run BLOCKED, the probe's answer (at the revision it was given) is
    // APPLIED, and the next step releases the claims; a new obligation then is a second blocking episode.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let mut ms = fail_turns(&store, &clock, 8, 3000, TRANSIENT).await?;
    store.supervise("R", context(&clock, ms)).await?;
    store
        .allocate_supervision("p1", context(&clock, ms))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    ms += 300_000;
    let probe = store
        .allocate_supervision("p1", context(&clock, ms))
        .await?
        .ok_or("the probe")?;
    let revision = run_state(&path, "R")?.1;
    let nonce = nonce_of(&path, &probe.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &probe.attempt_id,
        ms + 100,
        true,
        &[line(
            &nonce,
            output(
                "m1",
                "decision",
                &w,
                &probe.attempt_id,
                u64::try_from(revision)?,
                assumption(),
            ),
        )],
    )
    .await?;
    store.supervise("R", agent_gone(&clock, ms + 1000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED", "its outputs were owed");
    assert_eq!(
        ends(&path)?.last().map(|(_, end)| end.clone()),
        Some("decision.applied".to_owned())
    );
    assert!(retriggers(&path)?.is_empty());
    store.supervise("R", agent_gone(&clock, ms + 2000)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING", "nothing owed any more");
    clock.set(ms + 3000);
    event(
        &store,
        "R",
        "R/second",
        "need_decision",
        json!({"task_id": "D", "fingerprint": "second"}),
    )
    .await?;
    store.supervise("R", agent_gone(&clock, ms + 6000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.blocked'"
        )?,
        json!(2)
    );
    Ok(())
}

// Implementation review round 3 (docs/adr/ADR-025.md v16: each unavailability reason judged on its own subject).

#[tokio::test]
async fn a_registry_fault_of_the_old_agent_does_not_hold_a_commander_handed_to_another() -> R {
    // M2T08R-200 (decision 5, v16): the registry's looks are counted per agent; an unavailability it recorded for the
    // Commander's old agent (codex, look 100) does not apply once the role is held by another agent (claude), whose
    // own look (2) finds it available: the run works again.
    use crate::decision::Observation;
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let look = |agent: &str, n, available, ms| Context {
        observations: Some(
            [(
                agent.to_owned(),
                Observation {
                    host_epoch: 1,
                    n,
                    available,
                    logged_in: true,
                },
            )]
            .into_iter()
            .collect(),
        ),
        ..context(&clock, ms)
    };
    store
        .supervise("R", look("codex", 100, false, 3000))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc3', 'claude', 'commander', '1', '{}');
         UPDATE role_bindings SET agent = 'claude', session_id = 'sc3', role_epoch = 2
           WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", look("claude", 2, true, 4000)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING", "the new agent's own look");
    Ok(())
}

#[tokio::test]
async fn a_commander_without_a_session_is_timed_from_its_own_epochs_deferral() -> R {
    // M2T08R-200's class (decision 3, v16): how long a Commander has been without a session is measured from the
    // deferral of its own epoch, not from an earlier holder's: a holder that has lacked one for 6 minutes is not
    // unavailable because the one before it lacked one 31 minutes ago.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD} UPDATE role_bindings SET session_id = NULL WHERE run_id = 'R' AND role = 'commander';"
        ),
    )?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    store.supervise("R", context(&clock, 3000)).await?;
    store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?;
    assert_eq!(deferred(&path)?, ["NO_SESSION"]);
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    let handed = 3000 + 25 * 60_000;
    store.supervise("R", context(&clock, handed)).await?;
    store
        .allocate_supervision("p1", context(&clock, handed))
        .await?;
    let later = 3000 + 31 * 60_000;
    store.supervise("R", context(&clock, later)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING", "6 minutes without a session");
    Ok(())
}

#[tokio::test]
async fn a_turns_outputs_are_disposed_of_at_most_fifty_per_writer_job() -> R {
    // M2T08R-201 (decision 2's jobs table, v16): one disposal job ends at most 50 outputs and says more are left; the
    // store's step submits job after job until all 120 of the turn's outputs have ended, in order.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let lines: Vec<String> = (0..120).map(|_| "not json".to_owned()).collect();
    bridged_turn(&store, &clock, &a.attempt_id, 3000, true, &lines).await?;
    let mut conn = Connection::open(&path)?;
    let ctx = crate::persistence::writer::Ctx::quiet();
    assert!(super::super::supervise::dispose(
        &mut conn,
        "R",
        i64::MAX,
        &context(&clock, 4000),
        &ctx
    )?);
    assert_eq!(ends(&path)?.len(), 50);
    store.supervise("R", context(&clock, 4100)).await?;
    let ended = ends(&path)?;
    assert_eq!(ended.len(), 120);
    assert!(
        ended.iter().enumerate().all(|(n, (id, end))| *id
            == format!("{}/output/{n}", a.attempt_id)
            && end == "invalid:FORMAT"),
        "{ended:?}"
    );
    Ok(())
}

// Implementation review round 3, Claude (docs/adr/ADR-025.md v17: every action only on facts the step's verdict saw).

/// Run R to a capped Commander, BLOCKED, with its probe allocated past the cooldown: the probe and the time.
async fn probing(store: &Store, clock: &TestClock, path: &PathBuf) -> R<(Allocated, String, u64)> {
    let w = wake(store, clock, "R", "D", "fp", 0).await?;
    let mut ms = fail_turns(store, clock, 8, 3000, TRANSIENT).await?;
    store.supervise("R", context(clock, ms)).await?;
    store.allocate_supervision("p1", context(clock, ms)).await?;
    assert_eq!(blocked(path)?.0, "BLOCKED");
    ms += 300_000;
    store.supervise("R", context(clock, ms)).await?;
    let probe = store
        .allocate_supervision("p1", context(clock, ms))
        .await?
        .ok_or("the probe")?;
    Ok((probe, w, ms))
}

/// Take turn `attempt`'s dispatch and run it to its OBSERVED work with `lines` in its bridge, not settled yet.
async fn observed(store: &Store, clock: &TestClock, attempt: &str, ms: u64, lines: &[String]) -> R {
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no dispatch notice for the turn")?;
    clock.set(ms);
    assert!(store.runner_received(notice, t(ms)).await?);
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
    assert!(
        store
            .runner_effect(
                attempt,
                &format!("{attempt}/work"),
                crate::reconcile::EffectState::Started,
                crate::reconcile::EffectState::Observed,
                json!({"success": true, "detail": "turn", "messages": lines}),
            )
            .await?
    );
    Ok(())
}

/// End turn `attempt` (OBSERVED before) as SETTLED.
async fn settled(store: &Store, attempt: &str, ms: u64) -> R {
    assert!(
        store
            .runner_finished(
                attempt,
                crate::persistence::Ending {
                    success: true,
                    detail: "turn".to_owned(),
                    class: TRANSIENT,
                    at: t(ms),
                    draw: 0,
                    observed: Default::default(),
                    unknown_effect: false,
                },
            )
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn a_probes_answer_waits_for_its_turn_to_settle_and_is_judged_on_the_working_run() -> R {
    // M2T08R-250 (decisions 2 and 5, v17): a probe's outputs are recorded at its OBSERVED work but disposed of only once
    // the step's verdict has seen the probe SETTLED; a step in between leaves its answer (a dispatch, refused on a
    // BLOCKED run) waiting and the run BLOCKED; once settled the claims are released first and the answer, made at the
    // BLOCKED revision, is stale by construction and re-triggered once (never lost as ILLEGAL_TRANSITION).
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(
        &path,
        &format!(
            "{WORLD} INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
               ('F', 'R', 's3', 'READY', 1, 'developer', 0, 50);"
        ),
    )?;
    let (probe, w, ms) = probing(&store, &clock, &path).await?;
    let revision = u64::try_from(run_state(&path, "R")?.1)?;
    let nonce = nonce_of(&path, &probe.attempt_id)?;
    let dispatch = json!({"kind": "dispatch", "task_id": "F", "role": "developer"});
    observed(
        &store,
        &clock,
        &probe.attempt_id,
        ms + 100,
        &[line(
            &nonce,
            output("m1", "decision", &w, &probe.attempt_id, revision, dispatch),
        )],
    )
    .await?;
    store.supervise("R", context(&clock, ms + 200)).await?;
    assert!(ends(&path)?.is_empty(), "its turn has not settled");
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    settled(&store, &probe.attempt_id, ms + 300).await?;
    store.supervise("R", context(&clock, ms + 400)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    assert_eq!(
        value(
            &path,
            "SELECT json_array(state, reason_code) FROM decisions"
        )?,
        json!(["REJECTED", "STALE_REVISION"])
    );
    assert_eq!(retriggers(&path)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn the_allocation_waits_for_the_step_when_its_facts_changed_since_the_verdict() -> R {
    // M2T08R-251 (decision 5, v17): a probe that settled after the step's verdict (still unavailable, claims held) and
    // before the allocation makes the allocation's own view disagree with the recorded verdict: it allocates nothing;
    // the next step records the Commander available and releases the claims, and only then is another wake allocated.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let (probe, _w, ms) = probing(&store, &clock, &path).await?;
    let w2 = wake(&store, &clock, "R", "D", "two", ms + 10).await?;
    run_turn(
        &store,
        &clock,
        &probe.attempt_id,
        ms + 3000,
        true,
        TRANSIENT,
    )
    .await?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, ms + 4000))
            .await?
            .is_none(),
        "the verdict recorded is still unavailable"
    );
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    store.supervise("R", context(&clock, ms + 5000)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    let next = store
        .allocate_supervision("p1", context(&clock, ms + 5000))
        .await?
        .ok_or("allocated after the step")?;
    assert_eq!(next.wake, w2);
    Ok(())
}

#[tokio::test]
async fn an_ordinary_answer_overtaken_by_its_run_blocking_for_another_decision_is_retriggered_once()
-> R {
    // M2T08R-252 (decision 3, v17, the exception stated): an ordinary turn in flight when its Commander becomes
    // unavailable and another decision is owed: the run blocks for that one, the turn's answer is stale by
    // construction and re-triggered exactly once; the run stays BLOCKED, nothing is lost.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    wake(&store, &clock, "R", "D", "other", 3100).await?;
    store.supervise("R", agent_gone(&clock, 5200)).await?;
    assert_eq!(
        blocked(&path)?.0,
        "BLOCKED",
        "blocked for the other decision"
    );
    let nonce = nonce_of(&path, &a.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &a.attempt_id,
        5300,
        true,
        &[line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, assumption()),
        )],
    )
    .await?;
    store.supervise("R", agent_gone(&clock, 6000)).await?;
    store.supervise("R", agent_gone(&clock, 9000)).await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_array(state, reason_code) FROM decisions"
        )?,
        json!(["REJECTED", "STALE_REVISION"])
    );
    assert_eq!(retriggers(&path)?.len(), 1);
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    Ok(())
}

#[tokio::test]
async fn an_auth_unavailability_ends_by_a_settled_turn_and_not_by_a_withdrawn_one() -> R {
    // M2T08R-253 (decision 5's reasons table): after an Auth failure a withdrawn probe recovers nothing (still
    // BLOCKED for auth); the next probe that settles ends it (RUNNING, supervisor.available).
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let auth = crate::retry::FailureClass::Auth;
    run_turn(&store, &clock, &a.attempt_id, 3000, false, auth).await?;
    store.supervise("R", context(&clock, 4000)).await?;
    store
        .allocate_supervision("p1", context(&clock, 4000))
        .await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    store.supervise("R", context(&clock, 305_000)).await?;
    let first = store
        .allocate_supervision("p1", context(&clock, 305_000))
        .await?
        .ok_or("the first probe")?;
    assert!(store.withdraw_turn(&first.attempt_id, t(305_100)).await?);
    store.supervise("R", context(&clock, 306_000)).await?;
    store
        .allocate_supervision("p1", context(&clock, 306_000))
        .await?;
    assert_eq!(
        blocked(&path)?.0,
        "BLOCKED",
        "a withdrawn probe recovers nothing"
    );
    store.supervise("R", context(&clock, 606_000)).await?;
    let second = store
        .allocate_supervision("p1", context(&clock, 606_000))
        .await?
        .ok_or("the second probe")?;
    run_turn(&store, &clock, &second.attempt_id, 606_100, true, TRANSIENT).await?;
    store.supervise("R", context(&clock, 607_000)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    assert_eq!(
        unavailable(&path)?.last().cloned(),
        Some(json!(["supervisor.available", null]))
    );
    Ok(())
}

#[tokio::test]
async fn an_auth_unavailability_ends_with_its_commanders_epoch() -> R {
    // M2T08R-253 (decision 5's reasons table): a handover to a new Commander epoch ends an Auth unavailability of the
    // old one.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    run_turn(
        &store,
        &clock,
        &a.attempt_id,
        3000,
        false,
        crate::retry::FailureClass::Auth,
    )
    .await?;
    store.supervise("R", context(&clock, 4000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    exec(
        &path,
        "UPDATE role_bindings SET role_epoch = 2 WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", context(&clock, 5000)).await?;
    assert_eq!(blocked(&path)?.0, "RUNNING");
    Ok(())
}

#[tokio::test]
async fn a_probes_stale_answer_keeps_its_run_blocked_through_the_retrigger() -> R {
    // M2T08R-254 (decision 3): a probe settled while its Commander is unavailable for another reason answers with a
    // stale envelope (its own mistake): the step keeps the run BLOCKED while the output waits, the answer is
    // re-triggered, and the next step, taking the re-trigger in, keeps it BLOCKED (a PENDING trigger is owed).
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let (probe, w, ms) = probing(&store, &clock, &path).await?;
    let stale = u64::try_from(run_state(&path, "R")?.1 - 1)?;
    let nonce = nonce_of(&path, &probe.attempt_id)?;
    bridged_turn(
        &store,
        &clock,
        &probe.attempt_id,
        ms + 100,
        true,
        &[line(
            &nonce,
            output("m1", "decision", &w, &probe.attempt_id, stale, assumption()),
        )],
    )
    .await?;
    store.supervise("R", agent_gone(&clock, ms + 1000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    assert_eq!(retriggers(&path)?.len(), 1);
    store.supervise("R", agent_gone(&clock, ms + 2000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED", "the re-trigger is owed");
    Ok(())
}

#[tokio::test]
async fn a_disposal_job_leaves_the_outputs_of_a_turn_its_step_did_not_see_settle() -> R {
    // M2T08R-250 (decision 2's jobs table, v17): the disposal jobs of a step dispose of the outputs of turns whose end
    // its verdict saw (at or below its cursor); a turn that settled after that cursor waits for the next step.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
    let a = store
        .allocate_supervision("p1", context(&clock, 3000))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    observed(
        &store,
        &clock,
        &a.attempt_id,
        3100,
        &[line(
            &nonce,
            output("m1", "decision", &w, &a.attempt_id, 1, assumption()),
        )],
    )
    .await?;
    let before: i64 =
        Connection::open(&path)?.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
    settled(&store, &a.attempt_id, 3200).await?;
    let mut conn = Connection::open(&path)?;
    let ctx = crate::persistence::writer::Ctx::quiet();
    assert!(!super::super::supervise::dispose(
        &mut conn,
        "R",
        before,
        &context(&clock, 3300),
        &ctx
    )?);
    assert!(ends(&path)?.is_empty(), "its end is past the step's cursor");
    store.supervise("R", context(&clock, 3400)).await?;
    assert_eq!(
        ends(&path)?,
        [(
            format!("{}/output/0", a.attempt_id),
            "decision.applied".to_owned()
        )]
    );
    Ok(())
}

#[tokio::test]
async fn a_turn_reconciled_to_an_unknown_effect_has_ended_and_its_wake_is_rearmed() -> R {
    // Decision 3 (other endings; found by the developer's audit after round 3): a supervision turn whose host died with
    // its work begun is reconciled to UNKNOWN_EFFECT; that ends the turn (no answer was recorded), so its wake is
    // rearmed (a failure) and given another turn, and the run's supervision goes on.
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let w = wake(&store, &clock, "R", "D", "fp", 0).await?;
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
        .ok_or("no dispatch")?;
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
    let rows = store.reconcile_scan("p1", t(3000 + 3_600_000)).await?;
    let row = rows
        .into_iter()
        .find(|r| r.attempt_id == a.attempt_id)
        .ok_or("the turn is unfinished")?;
    store
        .reconcile_apply(
            row,
            Disposition::UnknownEffect {
                effects: Vec::new(),
                unknown: vec![crate::reconcile::work_key(&a.attempt_id)],
            },
            Process::Gone {
                basis: "no_process".to_owned(),
            },
            "reconcile",
            t(3000 + 3_600_000),
            0,
        )
        .await?;
    let ms = 3000 + 3_600_000 + 10_000;
    store.supervise("R", context(&clock, ms)).await?;
    let next = store
        .allocate_supervision("p1", context(&clock, ms))
        .await?
        .ok_or("the wake is given another turn")?;
    assert_eq!(next.wake, w);
    assert_eq!(rearms(&path)?[0]["reason"], json!("failure"));
    Ok(())
}

// Implementation review round 4 (docs/adr/ADR-025.md v18: the two tables item by item).

#[tokio::test]
async fn a_registry_fault_recorded_follows_its_agent_through_a_handover_under_the_same_reason() -> R
{
    // M2T08R-300 (decision 5's reasons table, v18): codex unavailable blocks the run; handed to claude, also
    // unavailable, the record is rewritten for claude though the reason's name is the same; a new host with no look
    // yet keeps the run BLOCKED (the recorded fault is the current agent's).
    use crate::decision::Observation;
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    wake(&store, &clock, "R", "D", "fp", 0).await?;
    let look = |agent: &str, n, ms| Context {
        observations: Some(
            [(
                agent.to_owned(),
                Observation {
                    host_epoch: 1,
                    n,
                    available: false,
                    logged_in: true,
                },
            )]
            .into_iter()
            .collect(),
        ),
        ..context(&clock, ms)
    };
    store.supervise("R", look("codex", 100, 3000)).await?;
    assert_eq!(blocked(&path)?.0, "BLOCKED");
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc3', 'claude', 'commander', '1', '{}');
         UPDATE role_bindings SET agent = 'claude', session_id = 'sc3', role_epoch = 2
           WHERE run_id = 'R' AND role = 'commander';",
    )?;
    store.supervise("R", look("claude", 2, 4000)).await?;
    store
        .allocate_supervision("p1", look("claude", 2, 4000))
        .await?;
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.observation.agent') FROM events \
             WHERE kind = 'supervisor.unavailable' ORDER BY seq DESC LIMIT 1"
        )?,
        json!("claude")
    );
    store.supervise("R", context(&clock, 5000)).await?;
    assert!(
        store
            .allocate_supervision("p1", context(&clock, 5000))
            .await?
            .is_none()
    );
    assert_eq!(
        blocked(&path)?.0,
        "BLOCKED",
        "no look of claude found it available"
    );
    Ok(())
}

#[tokio::test]
async fn a_disposal_job_is_budgeted_by_the_snapshots_its_decisions_compare() -> R {
    // M2T08R-301 (decision 2's jobs table, v18): a turn shown 12 000 tasks answers with three decisions; one disposal
    // job compares one decision's 12 000 snapshots and stops (more left); the step's jobs dispose of all three.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    exec(
        &path,
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 11999)
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq)
         SELECT 'T' || i, 'R', 's3', 'PENDING', 1, 'developer', 0, 10 + i FROM n;",
    )?;
    clock.set(0);
    for c in 0..12 {
        store
            .commit(Commit {
                events: (0..1000)
                    .map(|i| NewEvent {
                        project_id: "p1".to_owned(),
                        event_id: format!("nd/{c}/{i}"),
                        run_id: Some("R".to_owned()),
                        kind: "need_decision".to_owned(),
                        payload: json!({"task_id": format!("T{}", c * 1000 + i), "fingerprint": "many"}),
                    })
                    .collect(),
                ..Commit::default()
            })
            .await?;
    }
    let w = store.supervise("R", context(&clock, 2000)).await?.fired[0]
        .trigger_id
        .clone();
    let a = store
        .allocate_supervision("p1", context(&clock, 2100))
        .await?
        .ok_or("allocated")?;
    let nonce = nonce_of(&path, &a.attempt_id)?;
    let lines: Vec<String> = (0..3)
        .map(|i| {
            line(
                &nonce,
                output(
                    &format!("m{i}"),
                    "decision",
                    &w,
                    &a.attempt_id,
                    1,
                    assumption(),
                ),
            )
        })
        .collect();
    observed(&store, &clock, &a.attempt_id, 2200, &lines).await?;
    settled(&store, &a.attempt_id, 2300).await?;
    let mut conn = Connection::open(&path)?;
    let ctx = crate::persistence::writer::Ctx::quiet();
    assert!(super::super::supervise::dispose(
        &mut conn,
        "R",
        i64::MAX,
        &context(&clock, 2400),
        &ctx
    )?);
    assert_eq!(ends(&path)?.len(), 1);
    store.supervise("R", context(&clock, 2500)).await?;
    assert_eq!(ends(&path)?.len(), 3);
    Ok(())
}
