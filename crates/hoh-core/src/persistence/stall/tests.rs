//! The stall detector against a real store (docs/adr/ADR-031.md design correction C; the test plan's 停滞探测 and the
//! rest of 结束的安静条件). Each tick is the host's (host/runner.rs `tick`) in its order, from the runner's lists: the
//! pace and the rounds, the supervisor's step, the allocation, the detector last. A Commander turn allocated by a tick
//! is started after it and ended before the next with what a scripted Commander answers. Nothing is injected: only
//! the clock and the ticks move (scenario (6)'s interleaving is the writer's job-order hook, as the plan says).

use super::*;
use crate::decision::Context;
use crate::delivery::Clock;
use crate::persistence::{Allocated, FaultHooks, Options, Store, StoreClock, selftest};
use std::path::PathBuf;
use std::sync::Arc;
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

fn open(dir: &tempfile::TempDir, clock: &Arc<TestClock>) -> R<(Store, PathBuf)> {
    open_with(dir, clock, Arc::new(crate::persistence::NoFaults))
}

fn open_with(
    dir: &tempfile::TempDir,
    clock: &Arc<TestClock>,
    faults: Arc<dyn FaultHooks>,
) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        clock: Some(StoreClock(clock.clone())),
        faults,
        ..Options::new(crate::spawn::Direct::shared())
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R on spec s3 (revision 3) in `state`, supervised by its Commander (codex, session sc, bound and active), and
/// `extra`: nothing else of it.
/// Spec s3's specification review a CURRENT PASS (`world`'s `extra`): readiness lets its run into READY
/// (docs/adr/ADR-035.md decision 4).
const REVIEWED: &str = "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
   ('p1', 'reviewed-s3', NULL, 'spec_review.reported', json_object('review_id', 'rv-s3', 'spec_id', 's3',
    'spec_revision', 3, 'status', 'PASS', 'state', 'CURRENT', 'reasons', json_array(), 'session_id', 'sv',
    'findings', json_array()), '2026-11-07T00:00:00Z');";

fn world(path: &PathBuf, state: &str, extra: &str) -> R {
    exec(
        path,
        &format!(
            "INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-07T00:00:00Z');
             INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s3', 'p1', printf('%064d', 3), 3);
             INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
               ('R', 'p1', 's3', '{state}', 'AUTO', 1, '2026-11-07T00:00:00Z');
             INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
               ('sc', 'codex', 'commander', '1', '{{}}');
             INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
               ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-07T00:00:00Z');
             {extra}"
        ),
    )
}

/// Two waking events of R in its history (`no_progress`, fingerprints a and b): two wakes once taken in and fired.
const TWO_WAKES: &str =
    "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
   ('p1', 'R/np/a', 'R', 'no_progress', '{\"fingerprint\": \"a\"}', '2026-11-07T00:00:00Z'),
   ('p1', 'R/np/b', 'R', 'no_progress', '{\"fingerprint\": \"b\"}', '2026-11-07T00:00:00Z');";

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

fn state(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT state || coalesce('/' || blocked_reason, '') FROM runs WHERE run_id = 'R'",
    )
}

fn context(clock: &TestClock, ms: u64) -> Context {
    clock.set(ms);
    Context {
        now: Some(t(ms)),
        wall: Some(t(ms)),
        ..Context::default()
    }
}

/// The base fingerprints (`stalled/<P>/<n>`) of R's STALLED re-triggers, in order.
fn stalled(path: &PathBuf) -> R<Vec<String>> {
    Ok(serde_json::from_value(value(
        path,
        "SELECT json_group_array(b) FROM (SELECT json_extract(payload_json, '$.base_fingerprint') AS b FROM events \
         WHERE kind = 'supervisor.retrigger' AND json_extract(payload_json, '$.reason') = 'STALLED' ORDER BY seq)",
    )?)?)
}

/// R's re-triggers of stale decisions (any reason but STALLED), their reasons.
fn stale(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT json_group_array(json_extract(payload_json, '$.reason')) FROM events \
         WHERE kind = 'supervisor.retrigger' AND json_extract(payload_json, '$.reason') <> 'STALLED'",
    )
}

fn questions(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT json_group_array(json_object('reason', json_extract(payload_json, '$.reason'), 'turns', \
         json_extract(payload_json, '$.turns'), 'choices', json(json_extract(payload_json, '$.choices')))) \
         FROM events WHERE kind = 'question.asked'",
    )
}

fn decisions(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT json_group_array(d) FROM (SELECT state || coalesce(':' || reason_code, '') AS d FROM decisions \
         ORDER BY rowid)",
    )
}

fn last_kind(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT kind FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1",
    )
}

/// What the scripted Commander answers a turn with.
#[derive(Clone, Debug)]
enum Answer {
    Nothing,
    /// These decisions, in one turn, each at the run's revision as the turn ends.
    Decide(Vec<Value>),
    /// The task named moved between READY and PENDING.
    Flip(&'static str),
    /// The run moved to the next of these states; nothing once it is in the last.
    Advance(&'static [&'static str]),
    /// One create_tasks planning one READY task more (`P<n>`, n its number among the run's tasks).
    Plan,
}

fn assumption() -> Value {
    json!({"kind": "record_assumption", "statement": "s", "basis": "b"})
}

fn to(state: &str) -> Answer {
    Answer::Decide(vec![json!({"kind": "transition_run", "to_state": state})])
}

/// The first half of a tick at `ms` in the host's order: the pace and the rounds of the paced runs, then the
/// supervisor's step of the supervised runs. The runner's lists are read once, at the tick's start, as the host does.
async fn before_allocation(
    store: &Store,
    clock: &TestClock,
    ms: u64,
) -> R<crate::persistence::Waiting> {
    let waiting = store.runner_waiting("p1").await?;
    for run in &waiting.paced {
        store.pace_run(run, context(clock, ms)).await?;
        while store
            .schedule_round(run, context(clock, ms))
            .await?
            .dispatched
            .is_some()
        {}
    }
    for run in &waiting.supervised {
        store.supervise(run, context(clock, ms)).await?;
    }
    Ok(waiting)
}

/// The rest of the tick: the allocation, then the detector over the runner's stall list; R's detection.
async fn from_allocation(
    store: &Store,
    clock: &TestClock,
    ms: u64,
    waiting: &crate::persistence::Waiting,
) -> R<(Detected, Option<Allocated>)> {
    let turn = store.allocate_supervision("p1", context(clock, ms)).await?;
    let mut detected = Detected::Nothing;
    for run in &waiting.stall {
        let d = store
            .detect_stall(run, Default::default(), context(clock, ms))
            .await?;
        if run == "R" {
            detected = d;
        }
    }
    Ok((detected, turn))
}

async fn tick(store: &Store, clock: &TestClock, ms: u64) -> R<(Detected, Option<Allocated>)> {
    let waiting = before_allocation(store, clock, ms).await?;
    from_allocation(store, clock, ms, &waiting).await
}

/// The events turn `attempt`'s dispatch shows it (its supervision input's summary).
async fn shown(store: &Store, attempt: &str) -> R<Vec<Value>> {
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no dispatch notice for the turn")?;
    let input = notice
        .assignment
        .supervision
        .ok_or("no supervision input")?;
    Ok(input.summary["events"]
        .as_array()
        .cloned()
        .unwrap_or_default())
}

/// Turn `attempt`'s dispatch taken and its process running at `ms`.
async fn start(store: &Store, clock: &TestClock, attempt: &str, ms: u64) -> R {
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
    Ok(())
}

/// The lines of turn `turn`'s file bridge: `answer`'s decisions, each wrapped with the turn's nonce.
fn lines(path: &PathBuf, turn: &Allocated, answer: &Answer) -> R<Vec<String>> {
    let payloads = match answer {
        Answer::Nothing => Vec::new(),
        Answer::Decide(payloads) => payloads.clone(),
        Answer::Flip(task) => {
            let now = value(
                path,
                &format!("SELECT state FROM tasks WHERE task_id = '{task}'"),
            )?;
            let to = if now == json!("READY") {
                "PENDING"
            } else {
                "READY"
            };
            vec![json!({"kind": "transition_task", "task_id": task, "to_state": to})]
        }
        Answer::Plan => {
            let n = value(
                path,
                "SELECT count(*) + 1 FROM tasks WHERE run_id = 'R' AND role <> 'commander'",
            )?;
            let id = format!("P{n}");
            vec![
                json!({"kind": "create_tasks", "plan": {"tasks": [{"task_id": id, "title": id,
                "role": "developer", "writer": true, "depends_on": [], "requirement_ids": []}]}}),
            ]
        }
        Answer::Advance(states) => {
            let now = value(path, "SELECT state FROM runs WHERE run_id = 'R'")?;
            states
                .iter()
                .position(|s| json!(s) == now)
                .and_then(|i| states.get(i + 1))
                .map(|next| vec![json!({"kind": "transition_run", "to_state": next})])
                .unwrap_or_default()
        }
    };
    if payloads.is_empty() {
        return Ok(Vec::new());
    }
    let nonce: String = Connection::open(path)?.query_row(
        "SELECT json_extract(payload_json, '$.nonce') FROM outbox WHERE json_extract(payload_json, '$.attempt_id') = ?1",
        [&turn.attempt_id],
        |r| r.get(0),
    )?;
    let revision = value(path, "SELECT revision FROM runs WHERE run_id = 'R'")?;
    Ok(payloads
        .into_iter()
        .enumerate()
        .map(|(i, payload)| {
            let id = format!("{}/m{i}", turn.attempt_id);
            json!({"nonce": nonce, "message": {
                "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-",
                "attempt_id": turn.attempt_id, "sender_role": "commander", "recipient_role": "runtime",
                "role_epoch": 1, "correlation_id": "c", "causation_id": turn.wake,
                "idempotency_key": format!("k-{id}"), "expected_revision": revision,
                "spec_sha256": format!("{:064}", 3), "candidate_id": null, "kind": "decision", "payload": payload}})
            .to_string()
        })
        .collect())
}

/// Turn `turn`, running, ended at `ms` with `answer`: its work OBSERVED with the lines, then SETTLED.
async fn finish(
    store: &Store,
    clock: &TestClock,
    path: &PathBuf,
    turn: &Allocated,
    ms: u64,
    answer: &Answer,
) -> R {
    let attempt = &turn.attempt_id;
    let lines = lines(path, turn, answer)?;
    clock.set(ms);
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
    assert!(
        store
            .runner_finished(
                attempt,
                crate::persistence::Ending {
                    success: true,
                    detail: "turn".to_owned(),
                    class: crate::retry::FailureClass::Transient { retry_after: None },
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

/// What one tick of [`drive`] saw: its time, R's detection, the wake of the turn it allocated.
type Seen = Vec<(u64, Detected, Option<String>)>;

/// A tick every second after `from`, at most `n`; a turn a tick allocates runs between it and the next and answers
/// `answer`. Stops after the tick (and its turn) after which `done` holds. What each tick saw, and the last time.
async fn drive(
    store: &Store,
    clock: &TestClock,
    path: &PathBuf,
    from: u64,
    n: usize,
    answer: &Answer,
    done: impl Fn(&PathBuf) -> R<bool>,
) -> R<(Seen, u64)> {
    let mut ms = from;
    let mut seen = Vec::new();
    for _ in 0..n {
        ms += 1000;
        let (detected, turn) = tick(store, clock, ms).await?;
        seen.push((ms, detected, turn.as_ref().map(|t| t.wake.clone())));
        if let Some(turn) = turn {
            start(store, clock, &turn.attempt_id, ms + 100).await?;
            finish(store, clock, path, &turn, ms + 300, answer).await?;
        }
        if done(path)? {
            break;
        }
    }
    Ok((seen, ms))
}

/// Ticks from `from` until one allocates a turn (at most 10): the turn and its tick's time; every detection on the
/// way is nothing.
async fn next_turn(store: &Store, clock: &TestClock, from: u64) -> R<(Allocated, u64)> {
    let mut ms = from;
    for _ in 0..10 {
        ms += 1000;
        let (detected, turn) = tick(store, clock, ms).await?;
        assert_eq!(detected, Detected::Nothing, "at {ms}");
        if let Some(turn) = turn {
            return Ok((turn, ms));
        }
    }
    Err(format!("no turn allocated after {from}").into())
}

fn asked(path: &PathBuf) -> R<bool> {
    Ok(questions(path)? != json!([]))
}

/// The run-level question and its effects on R: `runtime:stalled` claims NEED_DECISION, question.asked{stalled,
/// turns, [retry, cancel]}, BLOCKED (NEED_DECISION) and the run.result last.
fn assert_asked(path: &PathBuf, turns: i64) -> R {
    assert_eq!(
        questions(path)?,
        json!([{"reason": "stalled", "turns": turns, "choices": ["retry", "cancel"]}])
    );
    assert_eq!(state(path)?, json!("BLOCKED/NEED_DECISION"));
    assert_eq!(
        value(
            path,
            "SELECT json_extract(payload_json, '$.source') FROM events WHERE kind = 'run.blocker_claimed' \
             AND json_extract(payload_json, '$.reason') = 'NEED_DECISION'"
        )?,
        json!("runtime:stalled")
    );
    assert_eq!(last_kind(path)?, json!("run.result"));
    Ok(())
}

fn not_listed(waiting: &crate::persistence::Waiting) -> bool {
    let r = "R".to_owned();
    !waiting.paced.contains(&r) && !waiting.supervised.contains(&r) && !waiting.stall.contains(&r)
}

#[tokio::test]
async fn a_rejected_end_is_decided_again_once_the_run_is_quiet() -> R {
    // 停滞探测 (1) (M2T13D-01) and (15), with FAILED for COMPLETED (a COMPLETED needs the evidence gate, which this
    // world has not; the quiet condition is the same): turn T1 ends deciding FAILED after the tick's step; T2 is
    // allocated and started before T1's output is disposed of; the next step disposes of it: REJECTED (T2 is
    // unfinished). T2 ends saying nothing; the next tick's detector writes stalled/<P>/1, no obligation before it; its
    // turn T3 decides FAILED on the quiet run: applied, its run.result last, R in none of the runner's lists after.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "RUNNING", TWO_WAKES)?;
    let fail = to("FAILED");
    let (detected, none) = tick(&store, &clock, 0).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    let (detected, t1) = tick(&store, &clock, 2500).await?;
    let t1 = t1.ok_or("T1 allocated")?;
    assert_eq!(detected, Detected::Nothing);
    start(&store, &clock, &t1.attempt_id, 2600).await?;
    // T1 ends between the step and the allocation of the tick at 3000: T2 takes the slot.
    let waiting = before_allocation(&store, &clock, 3000).await?;
    finish(&store, &clock, &path, &t1, 3000, &fail).await?;
    let (detected, t2) = from_allocation(&store, &clock, 3000, &waiting).await?;
    let t2 = t2.ok_or("T2 allocated")?;
    assert_ne!(t1.wake, t2.wake);
    assert_eq!(detected, Detected::Nothing);
    start(&store, &clock, &t2.attempt_id, 3100).await?;
    let (detected, none) = tick(&store, &clock, 4000).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    assert_eq!(decisions(&path)?, json!(["REJECTED:ILLEGAL_TRANSITION"]));
    assert_eq!(state(&path)?, json!("RUNNING"));
    finish(&store, &clock, &path, &t2, 4500, &Answer::Nothing).await?;
    let (detected, none) = tick(&store, &clock, 5000).await?;
    assert_eq!((detected, none.is_none()), (Detected::Retriggered(1), true));
    let marker = progress(&Connection::open(&path)?, "R")?;
    assert_eq!(stalled(&path)?, [format!("stalled/{marker}/1")]);
    assert_eq!(stale(&path)?, json!([]), "(15): no obligation before it");
    drive(&store, &clock, &path, 5000, 10, &fail, |p| {
        Ok(state(p)? == json!("FAILED"))
    })
    .await?;
    assert_eq!(state(&path)?, json!("FAILED"));
    assert_eq!(
        decisions(&path)?,
        json!(["REJECTED:ILLEGAL_TRANSITION", "APPLIED"])
    );
    assert_eq!(last_kind(&path)?, json!("run.result"));
    assert_eq!(stalled(&path)?.len(), 1);
    let waiting = store.runner_waiting("p1").await?;
    assert!(not_listed(&waiting), "{waiting:?}");
    Ok(())
}

#[tokio::test]
async fn an_end_refused_while_work_runs_leaves_the_detector_and_the_commander_idle() -> R {
    // The rest of 结束的安静条件: while the Developer's attempt runs, the Commander's FAILED is REJECTED
    // ILLEGAL_TRANSITION and nothing re-triggers it; in the 5 ticks after, the attempt still running, the detector
    // writes nothing ((a) does not hold) and no Commander turn is allocated (no idling).
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(
        &path,
        "RUNNING",
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd', 'claude', 'developer', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('D', 'R', 's3', 'RUNNING', 4, 'developer', 1, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('A', 'D', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
           ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'R/np/a', 'R', 'no_progress', '{\"fingerprint\": \"a\"}', '2026-11-07T00:00:00Z');",
    )?;
    let (_, done) = drive(&store, &clock, &path, 0, 6, &to("FAILED"), |p| {
        Ok(value(
            p,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.allocated'",
        )? == json!(1))
    })
    .await?;
    let (seen, _) = drive(&store, &clock, &path, done, 5, &Answer::Nothing, |_| {
        Ok(false)
    })
    .await?;
    assert_eq!(decisions(&path)?, json!(["REJECTED:ILLEGAL_TRANSITION"]));
    assert_eq!(state(&path)?, json!("RUNNING"));
    assert_eq!(seen.len(), 5);
    assert!(
        seen.iter()
            .all(|(_, d, turn)| *d == Detected::Nothing && turn.is_none()),
        "{seen:?}"
    );
    assert_eq!(stalled(&path)?, Vec::<String>::new());
    assert_eq!(stale(&path)?, json!([]));
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'supervisor.allocated'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn a_cancelled_last_retry_put_back_by_the_pace_with_a_turn_running_is_stalled_after_it() -> R
{
    // 停滞探测 (3) (M2T13D-200): R parked WAITING_EXTERNAL on its last task W's retry; turn T1 cancels W and ends after
    // the step; T2 takes the slot. T1's cancel is applied, the pace puts R back (due = []) while T2 runs: no STALLED;
    // T2 ends saying nothing: the next tick's detector writes stalled/<P>/1 (no obligation before it), and its turn T3
    // is allocated.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(
        &path,
        "RUNNING",
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, attempt_count, next_retry_at)
               VALUES ('W', 'R', 's3', 'READY', 1, 'developer', 1, 1, '{}');
             {TWO_WAKES}",
            crate::time::rfc3339_utc(t(600_000))
        ),
    )?;
    tick(&store, &clock, 0).await?;
    assert_eq!(state(&path)?, json!("WAITING_EXTERNAL"));
    let t1 = tick(&store, &clock, 2500).await?.1.ok_or("T1 allocated")?;
    start(&store, &clock, &t1.attempt_id, 2600).await?;
    let waiting = before_allocation(&store, &clock, 3000).await?;
    finish(
        &store,
        &clock,
        &path,
        &t1,
        3000,
        &Answer::Decide(vec![
            json!({"kind": "transition_task", "task_id": "W", "to_state": "CANCELLED", "reason": "not needed"}),
        ]),
    )
    .await?;
    let t2 = from_allocation(&store, &clock, 3000, &waiting)
        .await?
        .1
        .ok_or("T2 allocated")?;
    start(&store, &clock, &t2.attempt_id, 3100).await?;
    let (detected, none) = tick(&store, &clock, 4000).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    assert_eq!(decisions(&path)?, json!(["APPLIED"]));
    let (detected, none) = tick(&store, &clock, 5000).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    assert_eq!(state(&path)?, json!("RUNNING"), "the pace put it back");
    assert_eq!(
        value(
            &path,
            "SELECT json_extract(payload_json, '$.due') FROM events WHERE kind = 'run.resumed'"
        )?,
        json!([])
    );
    assert_eq!(stalled(&path)?, Vec::<String>::new(), "T2 runs");
    finish(&store, &clock, &path, &t2, 5500, &Answer::Nothing).await?;
    assert_eq!(
        tick(&store, &clock, 6000).await?.0,
        Detected::Retriggered(1)
    );
    assert_eq!(stale(&path)?, json!([]));
    let marker = progress(&Connection::open(&path)?, "R")?;
    assert_eq!(stalled(&path)?, [format!("stalled/{marker}/1")]);
    next_turn(&store, &clock, 6000).await?;
    Ok(())
}

#[tokio::test]
async fn a_parked_run_put_back_with_nothing_due_is_stalled_in_the_same_tick() -> R {
    // 停滞探测 (4) (M2T13D-250), the world of M2T19R-153 (retry/tests.rs): R parked WAITING_EXTERNAL on its last task's
    // retry; the task cancelled; no turn in flight. The next tick's pace puts R back (due = []) and the same tick's
    // detector writes stalled/<P>/1, though the tick's lists were read while R was still parked.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(
        &path,
        "RUNNING",
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, attempt_count, next_retry_at)
               VALUES ('W', 'R', 's3', 'READY', 1, 'developer', 1, 1, '{}');",
            crate::time::rfc3339_utc(t(60_000))
        ),
    )?;
    let (detected, none) = tick(&store, &clock, 0).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    assert_eq!(state(&path)?, json!("WAITING_EXTERNAL"));
    exec(
        &path,
        "UPDATE tasks SET state = 'CANCELLED', next_retry_at = NULL, revision = revision + 1 WHERE task_id = 'W';",
    )?;
    let (detected, _) = tick(&store, &clock, 10_000).await?;
    assert_eq!(state(&path)?, json!("RUNNING"));
    assert_eq!(detected, Detected::Retriggered(1));
    assert_eq!(stalled(&path)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_commander_that_never_decides_is_retriggered_twice_then_asked() -> R {
    // 停滞探测 (7) and (15): a supervised RUNNING run with nothing to do and no obligation: the first tick writes
    // stalled/<P>/1 (nothing of the supervisor before it); while its turn runs, nothing is written (the precondition)
    // and no turn is allocated; it ends saying nothing; the next tick writes stalled/<P>/2; its turn says nothing too;
    // the tick after it asks: runtime:stalled's claim, question.asked{stalled, turns: 2, [retry, cancel]}, BLOCKED
    // (NEED_DECISION), its run.result. Then no STALLED, question or turn more: R is out of W.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "RUNNING", "")?;
    let (detected, none) = tick(&store, &clock, 0).await?;
    assert_eq!((detected, none.is_none()), (Detected::Retriggered(1), true));
    assert_eq!(
        value(
            &path,
            "SELECT count(*) FROM events WHERE kind <> 'supervisor.retrigger'"
        )?,
        json!(0),
        "(15): no obligation, nothing of the supervisor, before it"
    );
    let marker = progress(&Connection::open(&path)?, "R")?;
    let mut ms = 0;
    for n in 1..=2 {
        let (turn, at) = next_turn(&store, &clock, ms).await?;
        // Its turn is shown the STALLED and how many turns in a row moved the run no further, this one's included.
        let shown = shown(&store, &turn.attempt_id).await?;
        assert!(
            shown.iter().any(|e| e["payload"]["reason"] == "STALLED"
                && e["payload"]["turns"] == n
                && e["payload"]["text"] == format!("已连续 {n} 个没有进展的 turn")),
            "STALLED {n}: {shown:?}"
        );
        start(&store, &clock, &turn.attempt_id, at + 100).await?;
        let (detected, none) = tick(&store, &clock, at + 1000).await?;
        assert_eq!(
            (detected, none.is_none()),
            (Detected::Nothing, true),
            "STALLED {n}'s turn runs"
        );
        finish(&store, &clock, &path, &turn, at + 1300, &Answer::Nothing).await?;
        ms = at + 2000;
        let (detected, _) = tick(&store, &clock, ms).await?;
        let expected = if n == 1 {
            Detected::Retriggered(2)
        } else {
            Detected::Asked
        };
        assert_eq!(detected, expected);
    }
    assert_eq!(
        stalled(&path)?,
        [format!("stalled/{marker}/1"), format!("stalled/{marker}/2")]
    );
    assert_asked(&path, 2)?;
    let (seen, _) = drive(&store, &clock, &path, ms, 3, &Answer::Nothing, |_| {
        Ok(false)
    })
    .await?;
    assert!(
        seen.iter()
            .all(|(_, d, turn)| *d == Detected::Nothing && turn.is_none()),
        "{seen:?}"
    );
    assert_eq!(stalled(&path)?.len(), 2);
    assert_eq!(questions(&path)?.as_array().map(Vec::len), Some(1));
    Ok(())
}

#[tokio::test]
async fn a_plan_only_turn_does_not_count_as_progress_and_a_moved_state_does() -> R {
    // 停滞探测 (8) (M2T13D-304): in SPECIFYING, a Commander whose every turn only records an assumption, only moves a
    // task between READY and PENDING, or only plans one READY task more (create_tasks, decision 2) leaves P as it is:
    // two STALLED turns, then the question. Its outputs still to dispose of hold the detector (the precondition). One
    // whose every turn moves the run on (CREATED -> DISCOVERING -> SPECIFYING -> READY) changes P each time: no
    // question.
    for answer in [
        Answer::Decide(vec![assumption()]),
        Answer::Flip("P1"),
        Answer::Plan,
    ] {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(
            &path,
            "SPECIFYING",
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq) VALUES
               ('P1', 'R', 's3', 'READY', 1, 'developer', 1);",
        )?;
        assert_eq!(tick(&store, &clock, 0).await?.0, Detected::Retriggered(1));
        let (turn, at) = next_turn(&store, &clock, 0).await?;
        start(&store, &clock, &turn.attempt_id, at + 100).await?;
        finish(&store, &clock, &path, &turn, at + 300, &answer).await?;
        // Ended after a tick's step: its output waits for the next step; that tick's detector writes nothing.
        assert_eq!(
            store
                .detect_stall("R", Default::default(), context(&clock, at + 400))
                .await?,
            Detected::Nothing,
            "{answer:?}: its output is not disposed of"
        );
        drive(&store, &clock, &path, at + 400, 12, &answer, asked).await?;
        assert_eq!(stalled(&path)?.len(), 2, "{answer:?}");
        assert_asked(&path, 2)?;
        assert_eq!(
            decisions(&path)?,
            json!(["APPLIED", "APPLIED"]),
            "{answer:?}"
        );
    }
    // Moving the run on each turn.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "CREATED", REVIEWED)?;
    const STATES: &[&str] = &["CREATED", "DISCOVERING", "SPECIFYING", "READY"];
    drive(
        &store,
        &clock,
        &path,
        0,
        30,
        &Answer::Advance(STATES),
        |p| Ok(state(p)? == json!("READY")),
    )
    .await?;
    assert_eq!(state(&path)?, json!("READY"));
    assert_eq!(questions(&path)?, json!([]));
    // One STALLED on each of the four states, the first of its marker each time.
    let markers = stalled(&path)?;
    assert_eq!(markers.len(), 4, "{markers:?}");
    assert!(markers.iter().all(|b| b.ends_with("/1")), "{markers:?}");
    Ok(())
}

#[tokio::test]
async fn a_budget_stop_in_the_commanders_stages_is_paced_and_the_detector_leaves_it() -> R {
    // 停滞探测 (9) (M2T13D-301): SPECIFYING, a spec.changed wake owed, the deadline passed before its first allocation:
    // the next tick's pace writes INCOMPLETE, run.incomplete and its run.result; no turn, and the detector writes
    // nothing. The same with the deadline passed while the host was stopped: the first tick after the restart. READY
    // with a cost limit and an unknown price: BLOCKED (BUDGET_UNENFORCEABLE). A stop the pace has not settled yet holds
    // the detector itself ((e)).
    {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(
            &path,
            "SPECIFYING",
            &format!(
                "UPDATE runs SET deadline_at = '{}' WHERE run_id = 'R';",
                crate::time::rfc3339_utc(t(1500))
            ),
        )?;
        assert_eq!(
            store
                .detect_stall("R", Default::default(), context(&clock, 2500))
                .await?,
            Detected::Nothing
        );
        assert_eq!(state(&path)?, json!("SPECIFYING"));
    }
    for restart in [false, true] {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(
            &path,
            "SPECIFYING",
            &format!(
                "UPDATE runs SET deadline_at = '{}' WHERE run_id = 'R';
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
                   ('p1', 'R/spec', 'R', 'spec.changed', '{{\"spec_revision\": 3}}', '2026-11-07T00:00:00Z');",
                crate::time::rfc3339_utc(t(1500))
            ),
        )?;
        let (detected, none) = tick(&store, &clock, 0).await?;
        assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
        let store = if restart {
            drop(store);
            open(&dir, &clock)?.0
        } else {
            store
        };
        let (detected, none) = tick(&store, &clock, 2500).await?;
        assert_eq!(
            (detected, none.is_none()),
            (Detected::Nothing, true),
            "restart {restart}"
        );
        assert_eq!(state(&path)?, json!("INCOMPLETE"));
        assert_eq!(
            value(
                &path,
                "SELECT json_group_array(kind) FROM (SELECT kind FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 2)"
            )?,
            json!(["run.result", "run.incomplete"])
        );
        assert_eq!(stalled(&path)?, Vec::<String>::new());
        assert_eq!(
            value(
                &path,
                "SELECT count(*) FROM events WHERE kind = 'supervisor.allocated'"
            )?,
            json!(0)
        );
    }
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(
        &path,
        "READY",
        "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES ('run', 'R', 'usd', 5, 0);",
    )?;
    assert_eq!(tick(&store, &clock, 0).await?.0, Detected::Nothing);
    assert_eq!(state(&path)?, json!("BLOCKED/BUDGET_UNENFORCEABLE"));
    assert_eq!(stalled(&path)?, Vec::<String>::new());
    Ok(())
}

/// Candidates C1 and C2 of R, VERIFYING, each with a round opened; C1's records failed and were judged unrecoverable;
/// and `extra`.
fn candidates(extra: &str) -> String {
    format!(
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at) VALUES
           ('C1', 'R', printf('%040d', 1), printf('%040d', 1), printf('%064d', 3), printf('%064d', 0), printf('%064d', 0),
            '/repo', 'VERIFYING', '2026-11-07T00:00:00Z'),
           ('C2', 'R', printf('%040d', 2), printf('%040d', 2), printf('%064d', 3), printf('%064d', 0), printf('%064d', 0),
            '/repo', 'VERIFYING', '2026-11-07T00:00:01Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'C1/opened', 'R', 'verification.round_opened', '{{\"round_id\": \"r1\", \"candidate_id\": \"C1\"}}', '2026-11-07T00:00:00Z'),
           ('p1', 'C1/reconcile_failed/1', 'R', 'candidate.reconcile_failed', '{{\"candidate_id\": \"C1\"}}', '2026-11-07T00:00:00Z'),
           ('p1', 'C1/stale', 'R', 'candidate.stale', '{{\"candidate_id\": \"C1\", \"reason\": \"evidence_unrecoverable\"}}', '2026-11-07T00:00:00Z'),
           ('p1', 'C2/opened', 'R', 'verification.round_opened', '{{\"round_id\": \"r2\", \"candidate_id\": \"C2\"}}', '2026-11-07T00:00:01Z');
         {extra}"
    )
}

#[tokio::test]
async fn only_a_live_recovery_obligation_holds_the_detector() -> R {
    // 停滞探测 (10) (M2T13D-303): C1's records were judged unrecoverable (candidate.stale{evidence_unrecoverable}, its
    // pending file kept for audit), C2's round is closed: (d) holds and the detector writes. C2's records failed and
    // back off (a live obligation still to recover): it does not. A restart changes neither.
    let pending: BTreeSet<String> = ["C1".to_owned()].into_iter().collect();
    for (extra, expected) in [
        (
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', 'C2/closed', 'R', 'verification.round_closed', '{\"round_id\": \"r2\"}', '2026-11-07T00:00:02Z');",
            Detected::Retriggered(1),
        ),
        (
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', 'C2/reconcile_failed/1', 'R', 'candidate.reconcile_failed',
                '{\"candidate_id\": \"C2\", \"recovery_not_before\": \"2100-01-01T00:00:00Z\"}', '2026-11-07T00:00:02Z');",
            Detected::Nothing,
        ),
    ] {
        for restart in [false, true] {
            let dir = tempfile::tempdir()?;
            let clock = Arc::new(TestClock::default());
            let (store, path) = open(&dir, &clock)?;
            world(&path, "RUNNING", &candidates(extra))?;
            let store = if restart {
                drop(store);
                open(&dir, &clock)?.0
            } else {
                store
            };
            assert_eq!(
                store
                    .detect_stall("R", pending.clone(), context(&clock, 0))
                    .await?,
                expected,
                "restart {restart}: {extra}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn nothing_is_written_while_the_run_has_something_that_moves_or_is_not_looked_at() -> R {
    // 停滞探测 (11): a dispatchable task, an attempt in flight (work, supervision), a waking event not taken in, a
    // PENDING trigger, an owed wake: no STALLED (an output to dispose of is (8)'s); PAUSE_REQUESTED, PAUSED, INCOMPLETE,
    // BLOCKED, WAITING_EXTERNAL, and a run with no Commander bound or no specification: not looked at.
    let developer = "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd', 'claude', 'developer', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');";
    let np = "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'R/np/a', 'R', 'no_progress', '{\"fingerprint\": \"a\"}', '2026-11-07T00:00:00Z');";
    // (state, world, the supervisor's steps before the detection at 2500, what is there)
    let cases: Vec<(&str, String, &[u64], &str)> = vec![
        (
            "RUNNING",
            format!(
                "{developer} INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq) VALUES
                   ('D', 'R', 's3', 'READY', 1, 'developer', 1);"
            ),
            &[],
            "a dispatchable task",
        ),
        (
            "RUNNING",
            format!(
                "{developer} INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq) VALUES
                   ('D', 'R', 's3', 'RUNNING', 1, 'developer', 1);
                 INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
                   ('A', 'D', 'RUNNING', 1, 1, 1);"
            ),
            &[],
            "a work attempt",
        ),
        (
            "SPECIFYING",
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq) VALUES
               ('S', 'R', 's3', 'RUNNING', 1, 'commander', 1);
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
               ('S/1', 'S', 'RUNNING', 1, 1, 1);"
                .to_owned(),
            &[],
            "a supervision attempt",
        ),
        ("RUNNING", np.to_owned(), &[], "a waking event not taken in"),
        ("RUNNING", np.to_owned(), &[2400], "a PENDING trigger"),
        ("RUNNING", np.to_owned(), &[0, 2500], "an owed wake"),
    ];
    for (state_name, extra, steps, what) in cases {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(&path, state_name, &extra)?;
        for at in steps {
            store.supervise("R", context(&clock, *at)).await?;
        }
        assert_eq!(
            store
                .detect_stall("R", Default::default(), context(&clock, 2500))
                .await?,
            Detected::Nothing,
            "{what}"
        );
        assert_eq!(stalled(&path)?, Vec::<String>::new(), "{what}");
    }
    for (state_name, extra) in [
        ("PAUSE_REQUESTED", ""),
        ("PAUSED", ""),
        ("INCOMPLETE", ""),
        ("WAITING_EXTERNAL", ""),
        (
            "RUNNING",
            "UPDATE runs SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION';",
        ),
        (
            "RUNNING",
            "DELETE FROM role_bindings WHERE role = 'commander';",
        ),
        ("RUNNING", "UPDATE runs SET spec_id = NULL;"),
    ] {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(&path, state_name, extra)?;
        assert_eq!(
            store
                .detect_stall("R", Default::default(), context(&clock, 0))
                .await?,
            Detected::Nothing,
            "{state_name} {extra}"
        );
        assert_eq!(stalled(&path)?, Vec::<String>::new());
    }
    Ok(())
}

/// A context at `ms` whose registry's look `n` finds the Commander's agent (codex) available (its compatibility
/// verified) or not.
fn looked(clock: &TestClock, ms: u64, n: u64, available: bool) -> Context {
    Context {
        observations: Some(
            [(
                "codex".to_owned(),
                crate::decision::Observation {
                    host_epoch: 1,
                    n,
                    available,
                    logged_in: true,
                    verified: available,
                },
            )]
            .into_iter()
            .collect(),
        ),
        ..context(clock, ms)
    }
}

#[tokio::test]
async fn an_unavailable_commanders_stalled_run_is_blocked_by_its_owed_wake_not_asked() -> R {
    // 停滞探测 (12): the Commander's agent is gone and the run stalls with no owed wake: the detector writes one STALLED;
    // the supervisor's judge, by that owed wake, blocks the run COMMANDER_UNAVAILABLE; no question. The agent back,
    // the run works again and that wake is allocated.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "RUNNING", "")?;
    assert_eq!(
        store
            .detect_stall("R", Default::default(), looked(&clock, 0, 1, false))
            .await?,
        Detected::Retriggered(1)
    );
    store.supervise("R", looked(&clock, 100, 1, false)).await?;
    store.supervise("R", looked(&clock, 3000, 1, false)).await?;
    assert_eq!(state(&path)?, json!("BLOCKED/COMMANDER_UNAVAILABLE"));
    assert!(
        store
            .allocate_supervision("p1", looked(&clock, 3100, 1, false))
            .await?
            .is_none()
    );
    assert_eq!(
        store
            .detect_stall("R", Default::default(), looked(&clock, 3200, 1, false))
            .await?,
        Detected::Nothing
    );
    assert_eq!(questions(&path)?, json!([]));
    store.supervise("R", looked(&clock, 4000, 2, true)).await?;
    assert_eq!(state(&path)?, json!("RUNNING"));
    let turn = store
        .allocate_supervision("p1", looked(&clock, 4100, 2, true))
        .await?
        .ok_or("the STALLED wake allocated once available")?;
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT fingerprint FROM supervisor_triggers WHERE trigger_id = '{}'",
                turn.wake
            )
        )?,
        json!(format!("{}#retrigger", stalled(&path)?[0]))
    );
    assert_eq!(stalled(&path)?.len(), 1);
    Ok(())
}

/// Fails the commit of the next Stall request once armed.
#[derive(Debug, Default)]
struct FailStall {
    armed: std::sync::atomic::AtomicBool,
    job: std::sync::Mutex<Option<&'static str>>,
}

impl FaultHooks for FailStall {
    fn before_job(&self, job: &'static str) {
        if let Ok(mut current) = self.job.lock() {
            *current = Some(job);
        }
    }

    fn before_commit(&self) -> Result<(), PersistError> {
        let stall = self.job.lock().is_ok_and(|j| *j == Some("Stall"));
        if stall && self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(PersistError::Storage("injected".to_owned()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_detection_failed_or_repeated_writes_one_stalled() -> R {
    // 停滞探测 (13): the detector's transaction fails before its commit; tried again: one stalled/<P>/1; looked at
    // again in the same tick: nothing more.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let faults = Arc::new(FailStall::default());
    let (store, path) = open_with(&dir, &clock, faults.clone())?;
    world(&path, "RUNNING", "")?;
    faults
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    assert!(
        store
            .detect_stall("R", Default::default(), context(&clock, 0))
            .await
            .is_err()
    );
    assert_eq!(stalled(&path)?, Vec::<String>::new());
    assert_eq!(
        store
            .detect_stall("R", Default::default(), context(&clock, 0))
            .await?,
        Detected::Retriggered(1)
    );
    assert_eq!(
        store
            .detect_stall("R", Default::default(), context(&clock, 0))
            .await?,
        Detected::Nothing
    );
    assert_eq!(stalled(&path)?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_stalled_retrigger_not_taken_in_before_a_restart_is_written_once() -> R {
    // 停滞探测 (5), its second half (M2T13D-300; the first, after a ResumeRun, is slice S1d's): STALLED written, the
    // host restarted before the step took it in: the first detection after the restart sees it after the cursor and
    // writes nothing; the step takes it in and its wake is allocated; one stalled/<P>/1.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "RUNNING", "")?;
    // The detector alone (the tick's step ran before it wrote).
    assert_eq!(
        store
            .detect_stall("R", Default::default(), context(&clock, 0))
            .await?,
        Detected::Retriggered(1)
    );
    drop(store);
    let (store, _) = open(&dir, &clock)?;
    assert_eq!(
        store
            .detect_stall("R", Default::default(), context(&clock, 500))
            .await?,
        Detected::Nothing,
        "not taken in yet"
    );
    let (turn, _) = next_turn(&store, &clock, 500).await?;
    let base = stalled(&path)?;
    assert_eq!(base.len(), 1);
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT fingerprint FROM supervisor_triggers WHERE trigger_id = '{}'",
                turn.wake
            )
        )?,
        json!(format!("{}#retrigger", base[0]))
    );
    Ok(())
}

/// Before the first Stall request once armed, commits `sql` from another connection: what another writer's job
/// committed between the tick's step and its detection.
#[derive(Debug, Default)]
struct BeforeStall {
    commit: std::sync::Mutex<Option<(PathBuf, String)>>,
    failed: std::sync::Mutex<Vec<String>>,
}

impl FaultHooks for BeforeStall {
    fn before_job(&self, job: &'static str) {
        if job != "Stall" {
            return;
        }
        let Some((db, sql)) = self.commit.lock().ok().and_then(|mut c| c.take()) else {
            return;
        };
        if let Err(e) = Connection::open(db).and_then(|c| c.execute_batch(&sql))
            && let Ok(mut failed) = self.failed.lock()
        {
            failed.push(e.to_string());
        }
    }
}

#[tokio::test]
async fn a_waking_event_committed_between_the_step_and_the_detection_holds_it() -> R {
    // 停滞探测 (6), its second half (M2T13D-300; the first, an AnswerQuestion, is slice S2's): an output's disposal
    // writes a stale decision's re-trigger after the tick's step, before its detection (the writer's job-order hook):
    // the detector sees a waking event after the cursor and writes nothing; the next ticks take it in and run its turn;
    // only after it the detector writes stalled/<P>/1; the stale re-trigger exactly once, no question.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let hook = Arc::new(BeforeStall::default());
    let (store, path) = open_with(&dir, &clock, hook.clone())?;
    world(&path, "RUNNING", "")?;
    if let Ok(mut commit) = hook.commit.lock() {
        *commit = Some((
            path.clone(),
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', 'd0/retrigger', 'R', 'supervisor.retrigger', json_object('decision_id', 'd0', 'reason',
                'STALE_REVISION', 'base_fingerprint', 'x', 'spec_revision', 3, 'task_ids', json('[]'),
                'own_mistake', json('false'), 'n', 0), '2026-11-07T00:00:00Z');"
                .to_owned(),
        ));
    }
    let (detected, none) = tick(&store, &clock, 0).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    assert_eq!(
        hook.failed.lock().map(|f| f.clone()).unwrap_or_default(),
        Vec::<String>::new()
    );
    assert_eq!(stale(&path)?, json!(["STALE_REVISION"]));
    let (seen, _) = drive(&store, &clock, &path, 0, 8, &Answer::Nothing, |p| {
        Ok(!stalled(p)?.is_empty())
    })
    .await?;
    let turn = seen
        .iter()
        .position(|(_, _, turn)| turn.is_some())
        .ok_or("the stale re-trigger's turn")?;
    let first = seen
        .iter()
        .position(|(_, d, _)| *d == Detected::Retriggered(1))
        .ok_or("STALLED once its turn ended")?;
    assert!(turn < first, "{seen:?}");
    assert_eq!(stale(&path)?, json!(["STALE_REVISION"]));
    assert_eq!(questions(&path)?, json!([]));
    Ok(())
}

#[tokio::test]
async fn a_stalled_obligation_past_the_horizon_is_read_by_its_identity() -> R {
    // 停滞探测 (14) (M2T13D-402): stalled/<P>/1's turn submits a decision gone stale (two decisions at the revision its
    // turn ended with: the second is STALE_REVISION): its re-trigger exactly once; that one's turn settles too and
    // changes nothing; the horizon passes the STALLED wake (a supervisor.horizon beyond its woken); P is unchanged; the
    // detector reads the STALLED obligation settled by its identity and writes stalled/<P>/2.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "SPECIFYING", "")?;
    assert_eq!(tick(&store, &clock, 0).await?.0, Detected::Retriggered(1));
    let marker = progress(&Connection::open(&path)?, "R")?;
    let (first, at) = next_turn(&store, &clock, 0).await?;
    start(&store, &clock, &first.attempt_id, at + 100).await?;
    finish(
        &store,
        &clock,
        &path,
        &first,
        at + 300,
        &Answer::Decide(vec![assumption(), assumption()]),
    )
    .await?;
    drive(&store, &clock, &path, at, 12, &Answer::Nothing, |p| {
        Ok(stalled(p)?.len() == 2)
    })
    .await?;
    assert_eq!(
        decisions(&path)?,
        json!(["APPLIED", "REJECTED:STALE_REVISION"])
    );
    assert_eq!(stale(&path)?, json!(["STALE_REVISION"]));
    assert_eq!(
        stalled(&path)?,
        [format!("stalled/{marker}/1"), format!("stalled/{marker}/2")]
    );
    let woken = value(
        &path,
        &format!(
            "SELECT seq FROM events WHERE kind = 'supervisor.woken' AND json_extract(payload_json, '$.trigger_id') = '{}'",
            first.wake
        ),
    )?;
    let horizon = value(
        &path,
        "SELECT max(json_extract(payload_json, '$.seq')) FROM events WHERE kind = 'supervisor.horizon'",
    )?;
    assert!(
        horizon
            .as_i64()
            .zip(woken.as_i64())
            .is_some_and(|(h, w)| h > w),
        "the horizon {horizon} passed STALLED 1's woken {woken}"
    );
    Ok(())
}

/// R's history before the world's present, `n` events (M2T13R-03): stale re-triggers and allocations of old wakes (the
/// kinds the detector's obligations and settlement read), other events of R, and failed recoveries of another run's
/// candidates (the project's); the supervisor's cursor and horizon after them all.
fn history(path: &PathBuf, n: u64) -> R {
    exec(
        path,
        &format!(
            "INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
               ('Q', 'p1', 's3', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:01Z');
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {n})
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', CASE i % 4 WHEN 3 THEN 'Q-c' || i || '/reconcile_failed/1' ELSE 'h' || i END,
                 CASE i % 4 WHEN 3 THEN 'Q' ELSE 'R' END,
                 CASE i % 4 WHEN 0 THEN 'supervisor.retrigger' WHEN 1 THEN 'supervisor.allocated'
                   WHEN 2 THEN 'old.progress' ELSE 'candidate.reconcile_failed' END,
                 CASE i % 4 WHEN 0 THEN '{{\"reason\": \"STALE_REVISION\", \"base_fingerprint\": \"x\", \"n\": 0}}'
                   WHEN 1 THEN '{{\"wake\": \"old\", \"run_id\": \"R\"}}'
                   WHEN 3 THEN json_object('candidate_id', 'Q-c' || i) ELSE '{{}}' END,
                 '2026-11-07T00:00:00Z' FROM n;
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'R/supervisor/horizon/' || (max(seq) + 1), 'R', 'supervisor.horizon',
                 json_object('seq', max(seq) + 1), '2026-11-07T00:00:00Z' FROM events;
             INSERT INTO supervisor_cursor(run_id, last_handled_seq) SELECT 'R', max(seq) FROM events;"
        ),
    )
}

#[tokio::test]
#[ignore = "a measurement (M2T13R-03): run with --ignored --nocapture to see the detector's time on a long history"]
async fn the_detectors_work_does_not_grow_with_the_runs_history() -> R {
    // The detection that writes stalled/<P>/1 and the one that writes stalled/<P>/2 (its precondition read through the
    // first's settlement), timed on a run with 1 000 and with 100 000 events of history.
    let mut times = Vec::new();
    for n in [1_000, 100_000] {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(&path, "RUNNING", "")?;
        history(&path, n)?;
        let began = std::time::Instant::now();
        let first = store
            .detect_stall("R", Default::default(), context(&clock, 0))
            .await?;
        let first_took = began.elapsed();
        assert_eq!(first, Detected::Retriggered(1), "{n}");
        let (turn, at) = next_turn(&store, &clock, 0).await?;
        start(&store, &clock, &turn.attempt_id, at + 100).await?;
        finish(&store, &clock, &path, &turn, at + 300, &Answer::Nothing).await?;
        let waiting = before_allocation(&store, &clock, at + 1000).await?;
        assert!(
            store
                .allocate_supervision("p1", context(&clock, at + 1000))
                .await?
                .is_none()
        );
        let began = std::time::Instant::now();
        let second = store
            .detect_stall("R", Default::default(), context(&clock, at + 1000))
            .await?;
        let second_took = began.elapsed();
        drop(waiting);
        assert_eq!(second, Detected::Retriggered(2), "{n}");
        eprintln!("history {n:>6}: first detection {first_took:?}, second {second_took:?}");
        times.push((first_took, second_took));
    }
    let (small, large) = (times[0], times[1]);
    let bounded = |a: Duration, b: Duration| b <= a * 5 + Duration::from_millis(5);
    assert!(
        bounded(small.0, large.0) && bounded(small.1, large.1),
        "{times:?}"
    );
    Ok(())
}

/// R's input command `id` at its revision now: an AnswerQuestion of `question` with `text`, or a ResumeRun.
fn input(
    path: &PathBuf,
    id: &str,
    target: crate::persistence::RunTarget,
    kind: &str,
) -> R<crate::persistence::SubmitCommand> {
    let revision = value(path, "SELECT revision FROM runs WHERE run_id = 'R'")?
        .as_i64()
        .ok_or("no revision")?;
    let event = match kind {
        "AnswerQuestion" => "question.answer_accepted",
        _ => "run.resumed",
    };
    Ok(crate::persistence::SubmitCommand {
        command_id: id.to_owned(),
        idempotency_key: format!("k-{id}"),
        kind: kind.to_owned(),
        run_id: "R".to_owned(),
        expected_revision: revision,
        payload: json!({}),
        payload_sha256: crate::context::sha256_hex(id.as_bytes()),
        target,
        event: crate::persistence::NewEvent {
            intent: crate::persistence::Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{id}"),
            run_id: Some("R".to_owned()),
            kind: event.to_owned(),
            payload: json!({"command_id": id}),
        },
        outbox: None,
    })
}

fn answer_of(
    path: &PathBuf,
    id: &str,
    question: &str,
    text: &str,
) -> R<crate::persistence::SubmitCommand> {
    input(
        path,
        id,
        crate::persistence::RunTarget::Input(crate::persistence::Input::Answer {
            question_id: question.to_owned(),
            answer: text.to_owned(),
            choice_id: None,
        }),
        "AnswerQuestion",
    )
}

/// The id of R's latest stalled question.
fn question_id(path: &PathBuf) -> R<String> {
    Ok(value(
        path,
        "SELECT event_id FROM events WHERE kind = 'question.asked' ORDER BY seq DESC LIMIT 1",
    )?
    .as_str()
    .ok_or("no question")?
    .to_owned())
}

/// R's latest run.result: its state and exit code.
fn result(path: &PathBuf) -> R<Value> {
    value(
        path,
        "SELECT json_array(json_extract(payload_json, '$.state'), json_extract(payload_json, '$.exit_code')) \
         FROM events WHERE run_id = 'R' AND kind = 'run.result' ORDER BY seq DESC LIMIT 1",
    )
}

#[tokio::test]
async fn a_stalled_question_answered_retry_gives_one_turn_then_one_stalled_then_asks_again_and_cancel_ends_it()
-> R {
    // 停滞探测 (6)'s AnswerQuestion half and (7)'s answers, decision 8's stalled row, S1c's c and precondition with the
    // retry answer (stall.rs): asked after two STALLED turns (its run.result BLOCKED, exit 3: `hoh -p` exits 3). An
    // AnswerQuestion(retry) committed between a tick's step and its detection: the claim released, the run back to
    // RUNNING, need_decision{answered, retry, progress P}; that tick's detector sees it unread and writes nothing; the
    // next ticks take it in and allocate the retry turn, shown the user's words. That turn moves nothing: one STALLED
    // (n = 3: the retry answer's turn counted in c), its turn moves nothing: the second question. Answered cancel: the
    // run CANCEL_REQUESTED, then CANCELLED by the cancel's step, its run.result exit 5.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "RUNNING", "")?;
    let (_, ms) = drive(&store, &clock, &path, 0, 12, &Answer::Nothing, asked).await?;
    assert_asked(&path, 2)?;
    assert_eq!(result(&path)?, json!(["BLOCKED", 3]));
    // Its marker, P as the run was in W when asked.
    let marker = value(
        &path,
        "SELECT json_extract(payload_json, '$.progress') FROM events WHERE kind = 'question.asked'",
    )?;
    let first = question_id(&path)?;
    let open = crate::persistence::questions::open(&Connection::open(&path)?, "R")?;
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].question_id, first);
    // The answer lands between the next tick's step and its detection.
    let ms = ms + 1000;
    let waiting = before_allocation(&store, &clock, ms).await?;
    store
        .submit_command(answer_of(&path, "c1", &first, "retry 请推进到 READY")?)
        .await?;
    assert_eq!(state(&path)?, json!("RUNNING"));
    let woke = value(
        &path,
        "SELECT payload_json FROM events WHERE kind = 'need_decision' ORDER BY seq DESC LIMIT 1",
    )?;
    assert_eq!(
        (
            &woke["reason"],
            &woke["choice_id"],
            &woke["progress"],
            &woke["question_id"]
        ),
        (
            &json!("answered"),
            &json!("retry"),
            &json!(marker),
            &json!(first)
        )
    );
    let (detected, none) = from_allocation(&store, &clock, ms, &waiting).await?;
    assert_eq!((detected, none.is_none()), (Detected::Nothing, true));
    let (turn, at) = next_turn(&store, &clock, ms).await?;
    let shown = shown(&store, &turn.attempt_id).await?;
    assert!(
        shown
            .iter()
            .any(|e| e["payload"]["answer"] == "retry 请推进到 READY"),
        "the retry turn sees the user's words: {shown:?}"
    );
    start(&store, &clock, &turn.attempt_id, at + 100).await?;
    finish(&store, &clock, &path, &turn, at + 300, &Answer::Nothing).await?;
    let (detected, _) = tick(&store, &clock, at + 1000).await?;
    assert_eq!(detected, Detected::Retriggered(3));
    let (turn, at) = next_turn(&store, &clock, at + 1000).await?;
    start(&store, &clock, &turn.attempt_id, at + 100).await?;
    finish(&store, &clock, &path, &turn, at + 300, &Answer::Nothing).await?;
    let (detected, _) = tick(&store, &clock, at + 1000).await?;
    assert_eq!(detected, Detected::Asked);
    let second = question_id(&path)?;
    assert_ne!(second, first);
    assert_eq!(questions(&path)?.as_array().map(Vec::len), Some(2));
    assert_eq!(state(&path)?, json!("BLOCKED/NEED_DECISION"));
    store
        .submit_command(answer_of(&path, "c2", &second, "cancel")?)
        .await?;
    assert_eq!(state(&path)?, json!("CANCEL_REQUESTED/NEED_DECISION"));
    assert_eq!(
        crate::persistence::questions::open(&Connection::open(&path)?, "R")?,
        Vec::new()
    );
    let finished = store
        .finish_cancel("R", json!({}), Default::default(), t(at + 2000))
        .await?;
    assert_eq!(finished, crate::persistence::Finished::Cancelled);
    assert_eq!(result(&path)?, json!(["CANCELLED", 5]));
    Ok(())
}

#[tokio::test]
async fn a_resume_of_a_commander_decided_incomplete_run_is_stalled_at_that_ticks_end_across_a_restart()
-> R {
    // 停滞探测 (2) (M2T13D-101) and (5)'s first half: the scripted Commander decides INCOMPLETE in SPECIFYING (no stop
    // item); a ResumeRun takes the run back to SPECIFYING and writes no re-trigger; that tick's detector writes a
    // STALLED; its turn moves the run to READY. The same with the host restarted between the ResumeRun and the next
    // tick: the first tick after the restart writes it.
    for restart in [false, true] {
        let dir = tempfile::tempdir()?;
        let clock = Arc::new(TestClock::default());
        let (store, path) = open(&dir, &clock)?;
        world(&path, "SPECIFYING", REVIEWED)?;
        let (_, ms) = drive(&store, &clock, &path, 0, 10, &to("INCOMPLETE"), |p| {
            Ok(state(p)? == json!("INCOMPLETE"))
        })
        .await?;
        assert_eq!(state(&path)?, json!("INCOMPLETE"));
        let resume = input(
            &path,
            "c1",
            crate::persistence::RunTarget::Resume,
            "ResumeRun",
        )?;
        let before = value(&path, "SELECT max(seq) FROM events")?
            .as_i64()
            .unwrap_or(0);
        store.submit_command(resume).await?;
        assert_eq!(state(&path)?, json!("SPECIFYING"));
        assert_eq!(
            value(
                &path,
                &format!(
                    "SELECT count(*) FROM events WHERE seq > {before} AND kind = 'supervisor.retrigger'"
                )
            )?,
            json!(0),
            "the resume writes no re-trigger"
        );
        let n = stalled(&path)?.len();
        let store = if restart {
            drop(store);
            open(&dir, &clock)?.0
        } else {
            store
        };
        let (detected, _) = tick(&store, &clock, ms + 1000).await?;
        assert_eq!(detected, Detected::Retriggered(n + 1), "restart {restart}");
        drive(
            &store,
            &clock,
            &path,
            ms + 1000,
            10,
            &Answer::Advance(&["SPECIFYING", "READY"]),
            |p| Ok(state(p)? == json!("READY")),
        )
        .await?;
        assert_eq!(state(&path)?, json!("READY"), "restart {restart}");
    }
    Ok(())
}

#[tokio::test]
async fn a_commanders_question_blocks_the_run_and_its_answer_wakes_the_next_turn_with_it() -> R {
    // Decisions 6 and 7 through the Commander's real output path (outputs.rs `person`): its request_user_decision in
    // SPECIFYING writes question.asked with its words (`question` equal to the original, quotes and backslash kept),
    // claims NEED_DECISION and blocks the run, whose run.result is BLOCKED exit 3; the open question is the request's
    // decision. The answer applies it (APPLIED), releases the claim, unblocks the run to SPECIFYING and writes
    // need_decision{answered}; the next turn is shown the answer; the detector writes nothing meanwhile.
    let dir = tempfile::tempdir()?;
    let clock = Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    world(&path, "SPECIFYING", TWO_WAKES)?;
    let words = "登录要支持哪些方式？\"OAuth\" 还是 密码\\短信";
    let ask = Answer::Decide(vec![
        json!({"kind": "request_user_decision", "question_id": "q1",
        "question": words, "choices": [{"choice_id": "oauth", "label": "OAuth"},
        {"choice_id": "pw", "label": "密码"}], "requires_user": true}),
    ]);
    let (_, ms) = drive(&store, &clock, &path, 0, 10, &ask, |p| {
        Ok(state(p)? == json!("BLOCKED/NEED_DECISION"))
    })
    .await?;
    let asked = value(
        &path,
        "SELECT payload_json FROM events WHERE kind = 'question.asked'",
    )?;
    assert_eq!(asked["question"], words);
    assert_eq!(asked["commander_question_id"], "q1");
    assert_eq!(result(&path)?, json!(["BLOCKED", 3]));
    let open = crate::persistence::questions::open(&Connection::open(&path)?, "R")?;
    assert_eq!(open.len(), 1);
    let decision = open[0].question_id.clone();
    assert_eq!(asked["question_id"], json!(decision));
    assert_eq!(open[0].text, words);
    assert_eq!(decisions(&path)?, json!(["PROPOSED"]));
    store
        .submit_command(answer_of(&path, "c1", &decision, "OAuth")?)
        .await?;
    assert_eq!(state(&path)?, json!("SPECIFYING"));
    assert_eq!(decisions(&path)?, json!(["APPLIED"]));
    let (turn, _) = next_turn(&store, &clock, ms).await?;
    let shown = shown(&store, &turn.attempt_id).await?;
    assert!(
        shown.iter().any(|e| e["payload"]["reason"] == "answered"
            && e["payload"]["choice_id"] == "oauth"
            && e["payload"]["decision_id"] == json!(decision)),
        "{shown:?}"
    );
    Ok(())
}
