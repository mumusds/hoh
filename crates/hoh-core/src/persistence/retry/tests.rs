//! TEST-042-01 against a real store: a scripted agent's failures, the persisted retry state after each, and the
//! dispatches the pace and the rounds make on either side of each `next_retry_at` (a FakeClock through the context, a
//! fixed jitter seed).

use crate::decision::Context;
use crate::persistence::{Ending, Options, Store, selftest};
use crate::retry::{self, FailureClass, SplitMix64};
use crate::time::{parse_rfc3339, rfc3339_utc};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R (RUNNING) with one developer task T1 READY, its role bound to a session with the developer's capabilities.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'READY', 1, 'developer', 1, 1);
";

fn context(now: SystemTime) -> Context {
    Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events", "command_exec"]
                .map(str::to_owned)
                .into_iter()
                .collect(),
        )]
        .into(),
        now: Some(now),
        ..Context::default()
    }
}

fn at(text: &str) -> R<SystemTime> {
    parse_rfc3339(text).ok_or_else(|| format!("not a time: {text}").into())
}

fn run_state(path: &Path) -> R<String> {
    Ok(
        Connection::open(path)?.query_row(
            "SELECT state FROM runs WHERE run_id = 'R'",
            [],
            |r| r.get(0),
        )?,
    )
}

/// What the tick does at `now`: the pace, then rounds until nothing more is dispatched; the attempts dispatched.
async fn tick(store: &Store, now: SystemTime) -> R<Vec<String>> {
    store.pace_run("R", context(now)).await?;
    let mut dispatched = Vec::new();
    while let Some(d) = store.schedule_round("R", context(now)).await?.dispatched {
        dispatched.push(d.attempt_id);
    }
    Ok(dispatched)
}

fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn test_042_01_failures_wait_for_their_retry_after_or_jitter_and_only_then_dispatch() -> R {
    const SEED: u64 = 0x0042_0001;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let mut rng = SplitMix64::new(SEED);
    // The scripted agent: 503 without Retry-After five times, then 429 with Retry-After: 120.
    let script: Vec<(&str, FailureClass)> = (0..5)
        .map(|_| ("503", FailureClass::Transient { retry_after: None }))
        .chain([(
            "429",
            FailureClass::Transient {
                retry_after: Some(Duration::from_secs(120)),
            },
        )])
        .collect();
    let mut clock = at("2026-11-07T00:00:00Z")?;
    let mut log = Vec::new();
    let mut snapshots = Vec::new();
    let mut dispatched = tick(&store, clock).await?;
    log.push(json!({"at": rfc3339_utc(clock), "dispatched": dispatched, "run": run_state(&path)?}));
    for (n, (status, class)) in script.iter().enumerate() {
        let [attempt] = dispatched.as_slice() else {
            return Err(format!("failure {n}: dispatched {dispatched:?}").into());
        };
        let attempt = attempt.clone();
        let notice = store
            .runner_waiting("p1")
            .await?
            .dispatches
            .into_iter()
            .find(|d| d.attempt_id == attempt)
            .ok_or("no dispatch notice")?;
        assert!(store.runner_received(notice).await?);
        // It fails a second and a bit after its dispatch (the store keeps the second).
        let failed_at = clock + Duration::from_millis(1_300);
        let draw = rng.next_u32();
        assert!(
            store
                .runner_finished(
                    &attempt,
                    Ending {
                        success: false,
                        detail: format!("HTTP {status}"),
                        class: *class,
                        at: failed_at,
                        draw,
                    },
                )
                .await?
        );
        let (count, next, ended): (i64, String, String) = Connection::open(&path)?.query_row(
            "SELECT t.attempt_count, t.next_retry_at, a.ended_at FROM tasks t JOIN attempts a ON a.attempt_id = ?1 \
             WHERE t.task_id = 'T1'",
            [&attempt],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let (next_at, ended_at) = (at(&next)?, at(&ended)?);
        let waited = next_at.duration_since(ended_at)?;
        let bound = retry::bound(u32::try_from(n)?);
        assert_eq!(count, i64::try_from(n)? + 1, "failure {n}");
        match class {
            FailureClass::Transient { retry_after: None } => {
                assert!(waited <= bound, "failure {n}: {waited:?} > {bound:?}");
            }
            FailureClass::Transient {
                retry_after: Some(after),
            } => assert_eq!(waited, *after, "the 429 waits its Retry-After exactly"),
            _ => unreachable!(),
        }
        snapshots.push(json!({
            "failure": n, "status": status, "attempt": attempt, "draw": draw, "failed_at": rfc3339_utc(failed_at),
            "ended_at": ended, "attempt_count": count, "next_retry_at": next, "waited_s": waited.as_secs(),
            // The jitter's bound, for a wait without Retry-After.
            "bound_s": matches!(class, FailureClass::Transient { retry_after: None }).then(|| bound.as_secs()),
        }));
        clock = failed_at;
        let none = tick(&store, clock).await?;
        assert!(none.is_empty(), "failure {n}: dispatched at once: {none:?}");
        // The wait: WAITING_EXTERNAL until next_retry_at, no dispatch 1 ms before it, one at it.
        if next_at > clock {
            assert_eq!(run_state(&path)?, "WAITING_EXTERNAL", "failure {n}");
            log.push(
                json!({"at": rfc3339_utc(clock), "dispatched": none, "run": run_state(&path)?}),
            );
            let before = next_at - Duration::from_millis(1);
            if before > clock {
                clock = before;
                let none = tick(&store, clock).await?;
                assert!(none.is_empty(), "failure {n}: dispatched 1 ms early");
                assert_eq!(run_state(&path)?, "WAITING_EXTERNAL", "failure {n}");
                log.push(json!({"at_ms_before": rfc3339_utc(next_at), "dispatched": none, "run": run_state(&path)?}));
            }
        }
        clock = next_at.max(clock);
        dispatched = tick(&store, clock).await?;
        assert_eq!(dispatched.len(), 1, "failure {n}: at next_retry_at");
        assert_eq!(run_state(&path)?, "RUNNING", "failure {n}");
        log.push(
            json!({"at": rfc3339_utc(clock), "dispatched": dispatched, "run": run_state(&path)?}),
        );
    }
    keep(
        "test-042-01.json",
        &json!({"seed": SEED, "clock_and_dispatches": log, "snapshots": snapshots}),
    )?;
    Ok(())
}

fn count(path: &Path, sql: &str) -> R<i64> {
    Ok(Connection::open(path)?.query_row(sql, [], |r| r.get(0))?)
}

#[tokio::test]
async fn test_042_05_repairs_past_their_limits_are_refused_for_a_decision_across_a_restart() -> R {
    use crate::persistence::{Repair, RepairRequest, RepairVerdict};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'IMPLEMENTED', revision = 4 WHERE task_id = 'T1';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, ended_at) VALUES
           ('A1', 'T1', 'SETTLED', 1, 1, 1, '2026-11-07T00:00:00Z');"
    ))?;
    let local = |n: u32| RepairRequest {
        run_id: "R".to_owned(),
        task_id: "T1".to_owned(),
        repair: Repair::Local {
            fingerprint: "F".to_owned(),
        },
        limit: 3,
        detail: format!("local repair {n} of F"),
    };
    let protocol = |n: u32| RepairRequest {
        run_id: "R".to_owned(),
        task_id: "T1".to_owned(),
        repair: Repair::Protocol {
            attempt_id: "A1".to_owned(),
        },
        limit: 2,
        detail: format!("protocol correction {n} of A1"),
    };
    // F's three local repairs (each failed) and A1's two protocol corrections.
    for n in 1..=3 {
        assert_eq!(
            store.request_repair(local(n)).await?,
            RepairVerdict::Granted { count: n }
        );
    }
    for n in 1..=2 {
        assert_eq!(
            store.request_repair(protocol(n)).await?,
            RepairVerdict::Granted { count: n }
        );
    }
    // A restart: the store closed and opened again reads the persisted counts.
    drop(store);
    let (store, path) = open(&dir)?;
    let counts = (
        store.repair_count("R", &local(0).repair).await?,
        store.repair_count("R", &protocol(0).repair).await?,
    );
    assert_eq!(counts, (3, 2));
    let attempts_before = count(&path, "SELECT count(*) FROM attempts")?;
    let dispatches_before = count(
        &path,
        "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch'",
    )?;
    // The fourth local repair and the third protocol correction: refused, each escalated to a decision.
    assert_eq!(
        store.request_repair(local(4)).await?,
        RepairVerdict::Refused { count: 3 }
    );
    assert_eq!(
        store.request_repair(protocol(3)).await?,
        RepairVerdict::Refused { count: 2 }
    );
    let decisions = count(
        &path,
        "SELECT count(*) FROM events WHERE kind = 'need_decision'",
    )?;
    let dispatches = count(
        &path,
        "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch'",
    )? - dispatches_before;
    let attempts = count(&path, "SELECT count(*) FROM attempts")? - attempts_before;
    let task: String = Connection::open(&path)?.query_row(
        "SELECT state FROM tasks WHERE task_id = 'T1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!((decisions, dispatches, attempts), (2, 0, 0));
    assert_ne!(task, "ACCEPTED");
    // Still refused after another restart: the counts do not reset.
    drop(store);
    let (store, _) = open(&dir)?;
    assert_eq!(
        store.request_repair(local(5)).await?,
        RepairVerdict::Refused { count: 3 }
    );
    keep(
        "test-042-05.json",
        &json!({
            "counts_after_reload": {"local_F": counts.0, "protocol_A1": counts.1},
            "need_decision_events": decisions, "dispatches": dispatches, "new_attempts": attempts, "task_state": task,
        }),
    )?;
    Ok(())
}

#[tokio::test]
async fn a_runs_cost_is_known_only_with_a_known_price_and_a_billed_cost_for_every_attempt() -> R {
    // CR-009, ADR-020 decision 9: an unknown price is an unknown cost, with no attempt too (AC-042-06's Rc and Rd).
    use crate::protocol::CostStatus;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let attempt = |id: &str, cost: &str| {
        format!(
            "INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, ended_at, cost_usd) \
             VALUES ('{id}', 'T1', 'FAILED', 1, 1, 1, '2026-11-07T00:00:00Z', {cost});"
        )
    };
    let cost = |store: &Store| store.reader().and_then(|r| r.run_cost("R"));
    // No usd row (no price table): unknown, without an attempt as with one.
    assert_eq!(cost(&store)?, (None, CostStatus::Unknown));
    Connection::open(&path)?.execute_batch(&attempt("A1", "NULL"))?;
    assert_eq!(cost(&store)?, (None, CostStatus::Unknown));
    // A cost limit with an unknown price (Rc): unknown.
    Connection::open(&path)?.execute_batch(
        "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES ('run', 'R', 'usd', 5, 0);",
    )?;
    assert_eq!(cost(&store)?, (None, CostStatus::Unknown));
    // A known price but an attempt without a billed cost: unknown; every attempt billed: their sum.
    Connection::open(&path)?
        .execute_batch("UPDATE budgets SET price_known = 1 WHERE kind = 'usd';")?;
    assert_eq!(cost(&store)?, (None, CostStatus::Unknown));
    Connection::open(&path)?.execute_batch(&format!(
        "UPDATE attempts SET cost_usd = 0.5 WHERE attempt_id = 'A1'; {}",
        attempt("A2", "0.25")
    ))?;
    assert_eq!(cost(&store)?, (Some(0.75), CostStatus::Known));
    // A known price and no attempt: known, 0.
    Connection::open(&path)?.execute_batch("DELETE FROM attempts;")?;
    assert_eq!(cost(&store)?, (Some(0.0), CostStatus::Known));
    Ok(())
}
