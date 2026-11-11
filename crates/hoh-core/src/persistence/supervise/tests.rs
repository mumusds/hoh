//! The supervisor's triggers and cursor against a real store (docs/adr/ADR-025.md decision 2), with a test clock.

use crate::decision::Context;
use crate::delivery::Clock;
use crate::persistence::{Commit, NewEvent, Options, Store, StoreClock, selftest};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The test's clock, set by the test: the store stamps commits with it and the supervisor steps at it.
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

fn open(dir: &tempfile::TempDir, clock: &std::sync::Arc<TestClock>) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        clock: Some(StoreClock(clock.clone())),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R RUNNING on spec s3 (revision 3; s4 is revision 4), the delivery world's developer task D with its candidate.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s3', 'p1', printf('%064d', 3), 3), ('s4', 'p1', printf('%064d', 4), 4);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's3', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-021', 's3', 'supervision', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES ('AC-021-01', 'REQ-021', 's3', 'storm', 'unit');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{}'), ('sq', 'pi', 'qa', '1', '{}'), ('sr', 'codex', 'reviewer', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z'),
  ('R', 'qa', 'pi', 'sq', 1, 1, '2026-11-07T00:00:00Z'),
  ('R', 'reviewer', 'codex', 'sr', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('D', 'R', 's3', 'RUNNING', 4, 'developer', 1, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-021', 's3');
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('A', 'D', 'RUNNING', 1, 1, 1);
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256, source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 11), printf('%064d', 3), printf('%064d', 5), printf('%064d', 6), '/c1', 'FROZEN', '2026-11-07T00:00:01Z');
";

/// `ms` milliseconds into the test's time.
fn t(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000) + Duration::from_millis(ms)
}

/// The test's clock moved to `ms`, and the supervisor's context then.
fn at(clock: &TestClock, ms: u64) -> Context {
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

fn rows(path: &PathBuf, sql: &str) -> R<Vec<Vec<Value>>> {
    use rusqlite::types::ValueRef;
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let width = statement.column_count();
    let rows = statement
        .query_map([], |r| {
            (0..width)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(x) => json!(x),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(b) => json!(b),
                    })
                })
                .collect()
        })?
        .collect::<Result<Vec<Vec<Value>>, _>>()?;
    Ok(rows)
}

fn count(path: &PathBuf, sql: &str) -> R<i64> {
    Ok(Connection::open(path)?.query_row(sql, [], |r| r.get(0))?)
}

/// Run R's events `(kind, payload)` committed in order, one commit; their seqs.
async fn events(store: &Store, tag: &str, list: &[(&str, Value)]) -> R<Vec<i64>> {
    let committed = store
        .commit(Commit {
            events: list
                .iter()
                .enumerate()
                .map(|(i, (kind, payload))| NewEvent {
                    project_id: "p1".to_owned(),
                    event_id: format!("{tag}/{i}"),
                    run_id: Some("R".to_owned()),
                    kind: (*kind).to_owned(),
                    payload: payload.clone(),
                })
                .collect(),
            ..Commit::default()
        })
        .await?;
    Ok(committed.event_seqs)
}

fn developer_result() -> R<crate::protocol::TaskResult> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "D", "attempt_id": "A", "role": "developer",
        "role_epoch": 1, "status": "COMPLETED", "summary": "done", "candidate_id": "C1", "evidence": [],
        "issues": [], "next_action": "verify"
    }))?)
}

#[tokio::test]
async fn test_021_01_ten_thousand_deltas_wake_nothing_and_candidate_ready_hands_to_qa_and_reviewer()
-> R {
    // AC-021-01 (balanced): one run streams 10000 token.delta, 500 tool.output, 60 heartbeat and 3 medium issues, and
    // its developer's result makes one candidate_ready; the clock then runs 3600 s with the supervisor's step every
    // second. No trigger, no wake, no decision; exactly one handoff each to qa and reviewer; the cursor passes them all.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let decisions = count(&path, "SELECT count(*) FROM decisions")?;
    let mut stream: Vec<(&str, Value)> = Vec::new();
    for i in 0..10_000 {
        stream.push((
            "token.delta",
            json!({"attempt_id": "A", "text": format!("t{i}")}),
        ));
    }
    for i in 0..500 {
        stream.push((
            "tool.output",
            json!({"attempt_id": "A", "line": format!("o{i}")}),
        ));
    }
    for _ in 0..60 {
        stream.push(("heartbeat", json!({"attempt_id": "A"})));
    }
    for i in 0..3 {
        stream.push((
            "issue.opened",
            json!({"severity": "medium", "fingerprint": format!("m{i}")}),
        ));
    }
    let mut seqs = Vec::new();
    for (n, chunk) in stream.chunks(1000).enumerate() {
        seqs.extend(events(&store, &format!("storm/{n}"), chunk).await?);
    }
    let outcome = store.accept_task_result(developer_result()?).await?;
    assert!(
        matches!(
            outcome,
            crate::persistence::TaskResultOutcome::Accepted { .. }
        ),
        "{outcome:?}"
    );
    let mut woken = 0;
    for second in 0..=3600 {
        let step = store.supervise("R", at(&clock, second * 1000)).await?;
        woken += step.fired.len();
    }
    assert_eq!(woken, 0);
    assert_eq!(count(&path, "SELECT count(*) FROM supervisor_triggers")?, 0);
    assert_eq!(count(&path, "SELECT count(*) FROM decisions")?, decisions);
    assert_eq!(
        rows(
            &path,
            "SELECT recipient_role, count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'handoff' \
             GROUP BY recipient_role ORDER BY recipient_role"
        )?,
        [[json!("qa"), json!(1)], [json!("reviewer"), json!(1)]]
    );
    let last = count(&path, "SELECT max(seq) FROM events WHERE run_id = 'R'")?;
    assert!(last > *seqs.last().ok_or("no seqs")?);
    assert_eq!(
        count(
            &path,
            "SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = 'R'"
        )?,
        last
    );
    Ok(())
}

fn issue(fingerprint: &str, spec_revision: i64) -> Value {
    json!({"severity": "high", "fingerprint": fingerprint, "spec_revision": spec_revision})
}

#[tokio::test]
async fn test_021_02_the_store_keeps_one_pending_trigger_per_key_and_fires_each_on_time() -> R {
    // AC-021-02 against the store, the supervisor stepping every 100 ms on the test's clock: K1 (fp1, revision 3) at
    // 0, 0.5 and 1.0 s fires at 3.0 s with its three seqs; K2 (fp2, 3) at 0.2 s and K3 (fp1, 4) at 0.3 s fire alone;
    // a K1 stream of one event a second from 30 s fires first 10.0 s after its first event; the store never holds two
    // PENDING triggers of one key; an integrity violation at 60 s fires in that step. Each fired trigger records its
    // key, seqs, first event time, wake time and latency.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let mut plan: Vec<(u64, &str, Value)> = vec![
        (0, "issue.opened", issue("fp1", 3)),
        (200, "issue.opened", issue("fp2", 3)),
        (300, "issue.opened", issue("fp1", 4)),
        (500, "issue.opened", issue("fp1", 3)),
        (1000, "issue.opened", issue("fp1", 3)),
    ];
    for i in 0..15 {
        plan.push((30_000 + i * 1000, "issue.opened", issue("fp1", 3)));
    }
    plan.push((
        60_000,
        "integrity_violation",
        json!({"reason": "candidate_changed"}),
    ));
    let mut seq_at: Vec<(u64, i64)> = Vec::new();
    let mut fired: Vec<(u64, crate::persistence::Fired)> = Vec::new();
    let mut next = 0;
    let mut most_pending = 0;
    for tick in 0..=700 {
        let now = tick * 100;
        clock.set(now);
        while next < plan.len() && plan[next].0 <= now {
            let (_, kind, payload) = &plan[next];
            let seqs = events(&store, &format!("plan/{next}"), &[(kind, payload.clone())]).await?;
            seq_at.push((plan[next].0, seqs[0]));
            next += 1;
        }
        for f in store.supervise("R", at(&clock, now)).await?.fired {
            fired.push((now, f));
        }
        most_pending = most_pending.max(count(
            &path,
            "SELECT coalesce(max(n), 0) FROM (SELECT count(*) AS n FROM supervisor_triggers \
             WHERE state = 'PENDING' GROUP BY run_id, fingerprint, ifnull(spec_id, ''))",
        )?);
    }
    let seq = |ms: u64| -> i64 {
        seq_at
            .iter()
            .find(|(at, _)| *at == ms)
            .map_or(-1, |(_, s)| *s)
    };
    let fired_at = |fingerprint: &str, spec: &str| -> Vec<(u64, Vec<i64>)> {
        fired
            .iter()
            .filter(|(_, f)| f.fingerprint == fingerprint && f.spec_id.as_deref() == Some(spec))
            .map(|(now, f)| (*now, f.event_seqs.clone()))
            .collect()
    };
    let k1 = fired_at("fp1", "s3");
    assert_eq!(k1[0], (3000, vec![seq(0), seq(500), seq(1000)]), "{k1:?}");
    assert_eq!(fired_at("fp2", "s3"), [(2200, vec![seq(200)])]);
    assert_eq!(fired_at("fp1", "s4"), [(2300, vec![seq(300)])]);
    assert_eq!(
        k1[1].0, 40_000,
        "the stream capped 10.0 s after its first event: {k1:?}"
    );
    let stream: Vec<i64> = k1[1..].iter().flat_map(|(_, s)| s.clone()).collect();
    assert_eq!(
        stream,
        (0..15)
            .map(|i| seq(30_000 + i * 1000))
            .collect::<Vec<i64>>(),
        "every event of the stream in exactly one trigger"
    );
    assert_eq!(most_pending, 1);
    let integrity: Vec<&(u64, crate::persistence::Fired)> = fired
        .iter()
        .filter(|(_, f)| f.event_seqs == [seq(60_000)])
        .collect();
    assert_eq!(integrity.len(), 1);
    assert_eq!(integrity[0].0, 60_000, "fired in its own step");
    // What was fired is queryable: its key, seqs, first event time, wake time and latency (REQ-021 behaviour 7).
    let woken = rows(
        &path,
        "SELECT json_extract(payload_json, '$.dedup_key'), json_extract(payload_json, '$.event_seqs'), \
         json_extract(payload_json, '$.first_event_at'), json_extract(payload_json, '$.woken_at'), \
         json_extract(payload_json, '$.wake_latency_ms') FROM events WHERE kind = 'supervisor.woken' ORDER BY seq",
    )?;
    assert_eq!(woken.len(), fired.len());
    assert_eq!(
        woken[0][4],
        json!(2000),
        "K2: 2.0 s from its event to its wake"
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM supervisor_triggers WHERE state = 'HANDLED'"
        )?,
        i64::try_from(fired.len())?
    );
    Ok(())
}

#[tokio::test]
async fn a_reopened_store_takes_in_only_the_events_after_its_cursor() -> R {
    // REQ-021 behaviour 5 (decision 2 of docs/adr/ADR-025.md): the cursor is persisted; a store opened again over the
    // database takes in only the events after it.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    let first = events(&store, "a", &[("need_decision", json!({"task_id": "D"}))]).await?;
    store.supervise("R", at(&clock, 0)).await?;
    store.supervise("R", at(&clock, 3000)).await?;
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM supervisor_triggers WHERE state = 'HANDLED'"
        )?,
        1
    );
    drop(store);
    let (store, _) = open(&dir, &clock)?;
    clock.set(10_000);
    let second = events(&store, "b", &[("need_decision", json!({"task_id": "D"}))]).await?;
    store.supervise("R", at(&clock, 10_000)).await?;
    let step = store.supervise("R", at(&clock, 13_000)).await?;
    assert_eq!(step.fired.len(), 1);
    assert_eq!(step.fired[0].event_seqs, second, "not {first:?} again");
    Ok(())
}

#[tokio::test]
async fn a_late_step_times_the_trigger_from_its_events_commit_not_from_when_it_took_it_in() -> R {
    // REQ-021 decision 4 (docs/adr/ADR-025.md decision 2, M2T08D-09): an event committed at 0 s and first looked at by
    // the supervisor at 10 s is due at 2 s: it fires in that late step, its first event time the commit's, its latency
    // 10 s; the window does not start again at the step. An event committed by an earlier writer (the store opened
    // again) has no commit instant here: its trigger counts from when it was taken in and says so.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    clock.set(0);
    let seqs = events(
        &store,
        "late",
        &[("need_decision", json!({"task_id": "D"}))],
    )
    .await?;
    let step = store.supervise("R", at(&clock, 10_000)).await?;
    assert_eq!(step.fired.len(), 1, "due at 2 s, fired in the late step");
    assert_eq!(step.fired[0].event_seqs, seqs);
    let woken = rows(
        &path,
        "SELECT json_extract(payload_json, '$.first_event_at'), json_extract(payload_json, '$.wake_latency_ms'), \
         json_extract(payload_json, '$.latency_basis') FROM events WHERE kind = 'supervisor.woken'",
    )?;
    assert_eq!(
        woken,
        [[
            json!(crate::time::rfc3339_millis_utc(t(0))),
            json!(10_000),
            json!("commit")
        ]]
    );
    clock.set(20_000);
    let earlier = events(
        &store,
        "earlier",
        &[("need_decision", json!({"task_id": "D"}))],
    )
    .await?;
    drop(store);
    let (store, _) = open(&dir, &clock)?;
    store.supervise("R", at(&clock, 30_000)).await?;
    let step = store.supervise("R", at(&clock, 32_000)).await?;
    assert_eq!(step.fired.len(), 1);
    assert_eq!(step.fired[0].event_seqs, earlier);
    let restarted = rows(
        &path,
        "SELECT json_extract(payload_json, '$.wake_latency_ms'), json_extract(payload_json, '$.latency_basis') \
         FROM events WHERE kind = 'supervisor.woken' ORDER BY seq DESC LIMIT 1",
    )?;
    assert_eq!(restarted, [[json!(2000), json!("across_restart")]]);
    Ok(())
}

/// Moves the test clock by `by_ms` just before each commit while armed: a transaction whose COMMIT takes that long.
#[derive(Debug)]
struct SlowCommit {
    clock: std::sync::Arc<TestClock>,
    by_ms: u64,
    armed: std::sync::atomic::AtomicBool,
}

impl crate::persistence::FaultHooks for SlowCommit {
    fn before_commit(&self) -> Result<(), crate::persistence::PersistError> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            let now = self.clock.0.lock().map(|m| *m).unwrap_or(0);
            self.clock.set(now + self.by_ms);
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_slow_commit_times_its_events_from_after_the_commit() -> R {
    // REQ-021 decision 4 (docs/adr/ADR-025.md decision 2, M2T08D-202, -301): an event's commit instant is read after
    // its COMMIT, before the caller is answered. A transaction that begins at 0 s and commits at 3 s makes a trigger
    // due at 5 s (2 s of quiet after the commit), not at 2 s, with a 2 s latency when it fires at 5 s.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let hooks = std::sync::Arc::new(SlowCommit {
        clock: clock.clone(),
        by_ms: 3000,
        armed: std::sync::atomic::AtomicBool::new(false),
    });
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
    clock.set(0);
    hooks.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let seqs = events(
        &store,
        "slow",
        &[("need_decision", json!({"task_id": "D"}))],
    )
    .await?;
    hooks
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        store.supervise("R", at(&clock, 4900)).await?.fired.len(),
        0,
        "due 2 s after the commit at 3 s, not after the start at 0 s"
    );
    let step = store.supervise("R", at(&clock, 5000)).await?;
    assert_eq!(step.fired.len(), 1);
    assert_eq!(step.fired[0].event_seqs, seqs);
    let latency = rows(
        &path,
        "SELECT json_extract(payload_json, '$.wake_latency_ms') FROM events WHERE kind = 'supervisor.woken'",
    )?;
    assert_eq!(latency, [[json!(2000)]]);
    Ok(())
}

#[tokio::test]
async fn a_spec_revision_the_project_lacks_keys_by_its_revision_and_never_merges_with_another() -> R
{
    // docs/adr/ADR-025.md decision 2 (M2T08D-61, -164): an event naming spec revision 9, which the project has no row
    // for, keys with a NULL spec and its fingerprint carrying "@r9"; one naming revision 8 (no row either) stays apart.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    events(
        &store,
        "revs",
        &[
            ("issue.opened", issue("fpx", 9)),
            ("issue.opened", issue("fpx", 8)),
        ],
    )
    .await?;
    store.supervise("R", at(&clock, 0)).await?;
    assert_eq!(
        rows(
            &path,
            "SELECT fingerprint, spec_id FROM supervisor_triggers ORDER BY fingerprint"
        )?,
        [
            [json!("fpx@r8"), Value::Null],
            [json!("fpx@r9"), Value::Null]
        ]
    );
    Ok(())
}

/// Counts the transactions committed while armed.
#[derive(Debug, Default)]
struct Commits {
    armed: std::sync::atomic::AtomicBool,
    n: std::sync::atomic::AtomicUsize,
}

impl crate::persistence::FaultHooks for Commits {
    fn before_commit(&self) -> Result<(), crate::persistence::PersistError> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.n.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_step_takes_in_its_events_in_batches_of_at_most_ten_thousand() -> R {
    // docs/adr/ADR-025.md decision 2 (M2T08D-263c, -900): one step takes in all of a run's committed events, at most
    // 10 000 per transaction: 11 000 events are two transactions, and the cursor ends past the last one.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let hooks = std::sync::Arc::new(Commits::default());
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
    let mut all = Vec::new();
    for n in 0..11 {
        let chunk: Vec<(&str, Value)> = (0..1000)
            .map(|i| ("token.delta", json!({"i": i})))
            .collect();
        all.extend(events(&store, &format!("cap/{n}"), &chunk).await?);
    }
    hooks.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let step = store.supervise("R", at(&clock, 0)).await?;
    hooks
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(step.cursor, *all.last().ok_or("none")?);
    assert_eq!(hooks.n.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(
        count(
            &path,
            "SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = 'R'"
        )?,
        step.cursor
    );
    Ok(())
}

#[tokio::test]
async fn a_capped_step_fires_nothing_before_it_has_caught_up_with_what_is_committed() -> R {
    // M2T08D-900 (docs/adr/ADR-025.md decision 2): K's first event is followed by 9 999 others at 0 s, K's second
    // (seq 10 001) is committed at 1.5 s. The step at 2 s must not fire K on its first 10 000 events alone: it takes
    // in batches until it has caught up, so K fires once, at 3.5 s, with both events; an integrity violation past the
    // first batch fires in that same step.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    clock.set(0);
    let mut first: Vec<(&str, Value)> = vec![("issue.opened", issue("fpk", 3))];
    first.extend((0..9_999).map(|i| ("token.delta", json!({"i": i}))));
    let mut seqs = events(&store, "k1", &first).await?;
    clock.set(1500);
    let second = events(&store, "k2", &[("issue.opened", issue("fpk", 3))]).await?;
    seqs.truncate(1);
    seqs.extend(second);
    let early = store.supervise("R", at(&clock, 2000)).await?;
    assert!(
        early.fired.is_empty(),
        "fired before the second event was taken in: {early:?}"
    );
    let late = store.supervise("R", at(&clock, 3500)).await?;
    assert_eq!(late.fired.len(), 1);
    assert_eq!(late.fired[0].event_seqs, seqs);
    // An integrity violation committed past a full batch fires in the step that sees it, the same step.
    clock.set(4000);
    let filler: Vec<(&str, Value)> = (0..10_000)
        .map(|i| ("token.delta", json!({"j": i})))
        .collect();
    events(&store, "filler", &filler).await?;
    let integrity = events(
        &store,
        "integrity",
        &[("integrity_violation", json!({"reason": "db"}))],
    )
    .await?;
    let step = store.supervise("R", at(&clock, 4000)).await?;
    assert_eq!(
        step.fired
            .iter()
            .map(|f| f.event_seqs.clone())
            .collect::<Vec<_>>(),
        [integrity]
    );
    Ok(())
}

#[tokio::test]
async fn a_retrigger_chain_on_an_unknown_revision_keeps_one_key() -> R {
    // M2T08D-954 (docs/adr/ADR-025.md decision 2): a re-trigger of a wake whose fingerprint already carries "@r9"
    // keys as "<fingerprint>#retrigger" once, and a re-trigger of that re-trigger keys the same.
    let dir = tempfile::tempdir()?;
    let clock = std::sync::Arc::new(TestClock::default());
    let (store, path) = open(&dir, &clock)?;
    exec(&path, WORLD)?;
    events(
        &store,
        "chain",
        &[
            ("supervisor.retrigger", json!({"base_fingerprint": "fp@r9", "spec_revision": 9, "task_ids": ["D"]})),
            ("supervisor.retrigger", json!({"base_fingerprint": "fp@r9#retrigger", "spec_revision": 9, "task_ids": ["D"]})),
        ],
    )
    .await?;
    store.supervise("R", at(&clock, 0)).await?;
    assert_eq!(
        rows(
            &path,
            "SELECT DISTINCT fingerprint FROM supervisor_triggers"
        )?,
        [[json!("fp@r9#retrigger")]]
    );
    Ok(())
}
