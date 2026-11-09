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

/// `at` in milliseconds since the epoch, as the FakeClock log records it (M2T19R-61).
fn millis_of(at: SystemTime) -> u128 {
    at.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
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
    log.push(
        json!({"at_ms": millis_of(clock), "dispatched": dispatched, "run": run_state(&path)?}),
    );
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
        assert!(store.runner_received(notice, clock).await?);
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
                        observed: Default::default(),
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
                log.push(json!({"at_ms": millis_of(clock), "before_next_retry_at": rfc3339_utc(next_at), "dispatched": none, "run": run_state(&path)?}));
            }
        }
        clock = next_at.max(clock);
        dispatched = tick(&store, clock).await?;
        assert_eq!(dispatched.len(), 1, "failure {n}: at next_retry_at");
        if next_at > failed_at {
            // Woken for its due task (the resume names it).
            let due: String = Connection::open(&path)?.query_row(
                "SELECT json_extract(payload_json, '$.due') FROM events WHERE kind = 'run.resumed' ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(due, "[\"T1\"]", "failure {n}");
        }
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
    // Refused for a decision: the task is BLOCKED for one (ADR-020 decision 10, M2T19R-56).
    let reason: String = Connection::open(&path)?.query_row(
        "SELECT blocked_reason FROM tasks WHERE task_id = 'T1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        (task.as_str(), reason.as_str()),
        ("BLOCKED", "NEED_DECISION")
    );
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

#[tokio::test]
async fn a_failure_recorded_before_a_crash_is_retried_by_its_recorded_class() -> R {
    // ADR-020 decision 1 and 3: a host killed after its attempt's 429 was recorded (OBSERVED, then COMMITTED) leaves
    // it RUNNING; the reconcile, long after, ends it FAILED at the time its work ended (M2T19R-03): its task waits the
    // recorded Retry-After from then, counted, and only the 5 s it ran join the used duration (M2T19R-55).
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let key = crate::reconcile::work_key("A1");
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'RUNNING', revision = 3, attempt_count = 2 WHERE task_id = 'T1';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('A1', 'T1', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{key}', 'A1', 'COMMITTED');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value) VALUES ('run', 'R', 'wall_seconds', 3600);
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', '{key}/effect.observed', 'R', 'effect.observed',
            '{{\"effect_key\": \"{key}\", \"detail\": {{\"success\": false, \"detail\": \"HTTP 429\", \"class\": \"transient\", \"retry_after_ms\": 120000, \"ended_at\": \"2026-11-07T00:00:05Z\"}}}}',
            '2026-11-07T00:00:05Z');"
    ))?;
    let at = at("2026-11-07T00:02:00Z")?;
    let rows = store.reconcile_scan("p1", at).await?;
    let [row] = rows.as_slice() else {
        return Err(format!("unfinished: {rows:?}").into());
    };
    assert_eq!(
        row.work_class,
        Some(FailureClass::Transient {
            retry_after: Some(Duration::from_secs(120))
        })
    );
    store
        .reconcile_apply(
            row.clone(),
            Disposition::Ended {
                to: crate::lifecycle::AttemptState::Failed,
                task_to: crate::protocol::TaskState::Ready,
                effects: Vec::new(),
            },
            Process::Gone {
                basis: "no_process".to_owned(),
            },
            "reconcile",
            at,
            0,
        )
        .await?;
    let (state, count, next): (String, i64, String) = Connection::open(&path)?.query_row(
        "SELECT state, attempt_count, next_retry_at FROM tasks WHERE task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        (state.as_str(), count, next.as_str()),
        ("READY", 3, "2026-11-07T00:02:05Z")
    );
    let (ended, used): (String, f64) = Connection::open(&path)?.query_row(
        "SELECT a.ended_at, b.used_value FROM attempts a, budgets b WHERE a.attempt_id = 'A1' AND b.kind = 'wall_seconds'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((ended.as_str(), used), ("2026-11-07T00:00:05Z", 5.0));
    Ok(())
}

#[tokio::test]
async fn an_ending_records_what_the_agents_turn_showed() -> R {
    // docs/adr/ADR-022.md decision 12: the resolved model, the usage, the agent's own session id and the turn's events
    // are written in the commit of the attempt's end.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let start = at("2026-11-07T00:00:00Z")?;
    let [attempt] = tick(&store, start)
        .await?
        .try_into()
        .map_err(|d| format!("{d:?}"))?;
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no notice")?;
    assert!(store.runner_received(notice, start).await?);
    let session: Option<String> = Connection::open(&path)?.query_row(
        "SELECT session_id FROM attempts WHERE attempt_id = ?1",
        [&attempt],
        |r| r.get(0),
    )?;
    let session = session.ok_or("the attempt has no session")?;
    store
        .runner_finished(
            &attempt,
            Ending {
                success: false,
                detail: "PROTOCOL_FRAME_TOO_LARGE".to_owned(),
                class: FailureClass::Transient { retry_after: None },
                at: start + Duration::from_secs(3),
                draw: 0,
                observed: crate::persistence::Observed {
                    resolved_model: Some("m-test".to_owned()),
                    usage: Some(json!({"input_tokens": 3})),
                    native_session: Some("native-1".to_owned()),
                    prompt: Some(crate::persistence::PromptRecord {
                        layer: "project".to_owned(),
                        role: "developer".to_owned(),
                        path: ".hoh/prompts/developer.md".to_owned(),
                        content_sha256: format!("{:064}", 7),
                        sent_sha256: format!("{:064}", 8),
                    }),
                    events: vec![(
                        "frame-too-large".to_owned(),
                        "adapter.diagnostic".to_owned(),
                        json!({"attempt_id": attempt, "reason": "PROTOCOL_FRAME_TOO_LARGE", "length": 8}),
                    )],
                    raw_log_path: Some("/logs/a".to_owned()),
                },
            },
        )
        .await?;
    let conn = Connection::open(&path)?;
    let (model, usage, failure, logs): (String, String, String, String) = conn.query_row(
        "SELECT resolved_model, usage_json, failure_reason, raw_log_path FROM attempts WHERE attempt_id = ?1",
        [&attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(
        (
            model.as_str(),
            serde_json::from_str::<Value>(&usage)?,
            failure.as_str(),
            logs.as_str()
        ),
        (
            "m-test",
            json!({"input_tokens": 3}),
            "PROTOCOL_FRAME_TOO_LARGE",
            "/logs/a"
        )
    );
    let native: String = conn.query_row(
        "SELECT native_id FROM sessions WHERE session_id = ?1",
        [&session],
        |r| r.get(0),
    )?;
    assert_eq!(native, "native-1");
    let (event_id, payload): (String, String) = conn.query_row(
        "SELECT event_id, payload_json FROM events WHERE kind = 'adapter.diagnostic'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(event_id, format!("{attempt}/frame-too-large"));
    let (revision, sent): (String, String) = conn.query_row(
        "SELECT prompt_revision, prompt_sha256 FROM attempts WHERE attempt_id = ?1",
        [&attempt],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(sent, format!("{:064}", 8));
    let (layer, path, content): (String, String, String) = conn.query_row(
        "SELECT layer, path, content_sha256 FROM prompt_revisions WHERE revision_id = ?1",
        [&revision],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        (layer.as_str(), path.as_str(), content),
        ("project", ".hoh/prompts/developer.md", format!("{:064}", 7))
    );
    assert_eq!(serde_json::from_str::<Value>(&payload)?["length"], 8);
    Ok(())
}

#[tokio::test]
async fn an_attempts_running_time_is_added_to_its_runs_used_duration_when_it_ends() -> R {
    // ADR-020 decision 7: the used duration moves only when an attempt ends.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES ('run', 'R', 'wall_seconds', 3600, 10);"
    ))?;
    let start = at("2026-11-07T00:00:00Z")?;
    let [attempt] = tick(&store, start)
        .await?
        .try_into()
        .map_err(|d| format!("{d:?}"))?;
    let notice = store
        .runner_waiting("p1")
        .await?
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == attempt)
        .ok_or("no notice")?;
    // Its start is taken by the runner's clock (M2T19R-53).
    assert!(store.runner_received(notice, start).await?);
    store
        .runner_finished(
            &attempt,
            Ending {
                success: false,
                detail: "HTTP 503".to_owned(),
                class: FailureClass::Transient { retry_after: None },
                at: start + Duration::from_millis(95_400),
                draw: 0,
                observed: Default::default(),
            },
        )
        .await?;
    let used: f64 = Connection::open(&path)?.query_row(
        "SELECT used_value FROM budgets WHERE kind = 'wall_seconds'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(used, 10.0 + 95.0);
    Ok(())
}

#[tokio::test]
async fn a_run_whose_budget_is_used_up_dispatches_nothing_and_ends_incomplete() -> R {
    // REQ-042 behaviour 6 (ADR-020 decision 7): its iterations used up, or its deadline come, a run dispatches
    // nothing more and, nothing being in flight, is INCOMPLETE with the tasks left listed.
    for (stop, setup) in [
        (
            "max_iterations",
            "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES ('run', 'R', 'iterations', 2, 2);",
        ),
        (
            "deadline_at",
            "UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(&format!("{WORLD}{setup}"))?;
        let now = at("2026-11-07T00:01:00Z")?;
        assert!(
            store
                .schedule_round("R", context(now))
                .await?
                .dispatched
                .is_none(),
            "{stop}: dispatched"
        );
        let paced = store.pace_run("R", context(now)).await?;
        assert_eq!(paced.incomplete, Some(stop));
        assert_eq!(run_state(&path)?, "INCOMPLETE", "{stop}");
        let remaining: String = Connection::open(&path)?.query_row(
            "SELECT json_extract(payload_json, '$.remaining') FROM events WHERE kind = 'run.incomplete'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(remaining, "[\"T1\"]", "{stop}");
        assert_eq!(count(&path, "SELECT count(*) FROM attempts")?, 0, "{stop}");
    }
    // A moment before the deadline, the task is dispatched.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';"
    ))?;
    let before = at("2026-11-07T00:00:59Z")?;
    assert_eq!(tick(&store, before).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_task_waiting_for_its_retry_is_skipped_by_the_rounds_of_a_run_still_at_work() -> R {
    // REQ-042 behaviour 2: while another task of the run runs, the run is not parked, so its rounds themselves must
    // leave a task alone until its next_retry_at: not 1 ms before, then at it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:01:00Z' WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'RUNNING', 3, 'developer', 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('T2/attempt-1', 'T2', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');"
    ))?;
    let early = at("2026-11-07T00:01:00Z")? - Duration::from_millis(1);
    assert!(
        tick(&store, early).await?.is_empty(),
        "dispatched 1 ms early"
    );
    assert_eq!(run_state(&path)?, "RUNNING");
    assert_eq!(
        tick(&store, at("2026-11-07T00:01:00Z")?).await?,
        ["T1/attempt-1"]
    );
    Ok(())
}

#[tokio::test]
async fn a_running_attempts_time_counts_against_the_duration_before_it_ends() -> R {
    // M2T19R-02: with 60 s allowed and an attempt running for 120 s, nothing more is dispatched although nothing has
    // ended yet; the used duration is settled only when it ends.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'RUNNING', 3, 'developer', 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('T2/attempt-1', 'T2', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value) VALUES ('run', 'R', 'wall_seconds', 60);"
    ))?;
    assert_eq!(
        tick(&store, at("2026-11-07T00:00:30Z")?).await?.len(),
        1,
        "within the duration"
    );
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'RUNNING', 3, 'developer', 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('T2/attempt-1', 'T2', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value) VALUES ('run', 'R', 'wall_seconds', 60);"
    ))?;
    assert!(
        tick(&store, at("2026-11-07T00:02:00Z")?).await?.is_empty(),
        "dispatched past the duration"
    );
    assert_eq!(
        run_state(&path)?,
        "RUNNING",
        "INCOMPLETE only once nothing runs"
    );
    let used: f64 = Connection::open(&path)?.query_row(
        "SELECT used_value FROM budgets WHERE kind = 'wall_seconds'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(used, 0.0);
    Ok(())
}

#[tokio::test]
async fn a_parked_run_whose_last_waiting_task_is_blocked_is_blocked() -> R {
    // M2T19R-05: WAITING_EXTERNAL for T1's retry; T1's fourth local repair is refused, so it is BLOCKED for a decision
    // and nothing waits any more: the run is BLOCKED (NEED_DECISION), not left waiting for ever.
    use crate::persistence::{Repair, RepairRequest};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:01:00Z' WHERE task_id = 'T1';"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    assert_eq!(run_state(&path)?, "WAITING_EXTERNAL");
    let request = || RepairRequest {
        run_id: "R".to_owned(),
        task_id: "T1".to_owned(),
        repair: Repair::Local {
            fingerprint: "F".to_owned(),
        },
        limit: 0,
        detail: "refused".to_owned(),
    };
    store.request_repair(request()).await?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:20Z")?))
        .await?;
    let (state, reason): (String, String) = Connection::open(&path)?.query_row(
        "SELECT state, blocked_reason FROM runs WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        (state.as_str(), reason.as_str()),
        ("BLOCKED", "NEED_DECISION")
    );
    Ok(())
}

#[tokio::test]
async fn a_wait_past_the_deadline_ends_incomplete_at_the_deadline() -> R {
    // REQ-042 failure mode 4, ADR-020 decision 8 (M2T19R-52): a Retry-After past deadline_at is not waited out: the
    // run waits until the deadline, then is INCOMPLETE with its tasks listed, nothing dispatched.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:10:00Z' WHERE task_id = 'T1';
         UPDATE runs SET state = 'WAITING_EXTERNAL', resume_state = 'RUNNING', deadline_at = '2026-11-07T00:05:00Z'
           WHERE run_id = 'R';"
    ))?;
    assert!(tick(&store, at("2026-11-07T00:04:59Z")?).await?.is_empty());
    assert_eq!(run_state(&path)?, "WAITING_EXTERNAL");
    assert!(tick(&store, at("2026-11-07T00:05:00Z")?).await?.is_empty());
    assert_eq!(run_state(&path)?, "INCOMPLETE");
    let (stop, remaining): (String, String) = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json, '$.stop'), json_extract(payload_json, '$.remaining') FROM events \
         WHERE kind = 'run.incomplete'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        (stop.as_str(), remaining.as_str()),
        ("deadline_at", "[\"T1\"]")
    );
    assert_eq!(count(&path, "SELECT count(*) FROM attempts")?, 0);
    Ok(())
}

#[tokio::test]
async fn a_blocked_run_is_named_by_its_first_reason_and_waits_behind_a_blocked_dependency() -> R {
    // The run's blocked_reason follows BlockedReason's precedence (M2T19R-54), and a PENDING task behind a BLOCKED
    // dependency cannot move the run on (M2T19R-63).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate, blocked_reason) VALUES
           ('T2', 'R', 's1', 'BLOCKED', 1, 'developer', 2, 1, 'BLOCKED_AUTH'),
           ('T3', 'R', 's1', 'PENDING', 1, 'developer', 3, 1, NULL);
         INSERT INTO task_dependencies(task_id, depends_on) VALUES ('T3', 'T1');"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    let (state, reason, set): (String, String, String) = Connection::open(&path)?.query_row(
        "SELECT state, blocked_reason, blocked_reasons_json FROM runs WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        (state.as_str(), reason.as_str(), set.as_str()),
        (
            "BLOCKED",
            "BLOCKED_AUTH",
            "[\"BLOCKED_AUTH\",\"NEED_DECISION\"]"
        )
    );
    Ok(())
}

#[tokio::test]
async fn an_ended_attempts_time_is_counted_once() -> R {
    // M2T19R-152: an ended attempt's time is in `used`, never reserved again: 50 of 60 s used by an attempt that
    // started 100 s ago and ended, and another task is still dispatched.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'IMPLEMENTED', 3, 'developer', 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at, ended_at) VALUES
           ('T2/attempt-1', 'T2', 'SETTLED', 1, 1, 1, '2026-11-07T00:00:00Z', '2026-11-07T00:00:50Z');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES ('run', 'R', 'wall_seconds', 60, 50);"
    ))?;
    assert_eq!(tick(&store, at("2026-11-07T00:01:40Z")?).await?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_parked_run_left_with_nothing_to_wait_for_or_blocked_works_again() -> R {
    // M2T19R-153: the other half of M2T19R-05: its last waiting task cancelled, nothing blocked, the parked run goes
    // back to its state (its resume naming no due task).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:01:00Z' WHERE task_id = 'T1';"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    assert_eq!(run_state(&path)?, "WAITING_EXTERNAL");
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'CANCELLED', next_retry_at = NULL, revision = revision + 1 WHERE task_id = 'T1';",
    )?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:20Z")?))
        .await?;
    assert_eq!(run_state(&path)?, "RUNNING");
    let due: String = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json, '$.due') FROM events WHERE kind = 'run.resumed'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(due, "[]");
    Ok(())
}

#[tokio::test]
async fn a_recorded_end_counts_only_once_its_work_was_committed() -> R {
    // M2T19R-155: OBSERVED but not COMMITTED (its group not yet seen empty) is no end of the work: the reconcile
    // takes its own time for the end, the duration and the retry.
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let key = crate::reconcile::work_key("A1");
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'RUNNING', revision = 3 WHERE task_id = 'T1';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('A1', 'T1', 'RUNNING', 1, 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{key}', 'A1', 'OBSERVED');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', '{key}/effect.observed', 'R', 'effect.observed',
            '{{\"effect_key\": \"{key}\", \"detail\": {{\"success\": false, \"detail\": \"HTTP 429\", \"class\": \"transient\", \"retry_after_ms\": 120000, \"ended_at\": \"2026-11-07T00:00:05Z\"}}}}',
            '2026-11-07T00:00:05Z');"
    ))?;
    let at = at("2026-11-07T00:02:00Z")?;
    let rows = store.reconcile_scan("p1", at).await?;
    let [row] = rows.as_slice() else {
        return Err(format!("unfinished: {rows:?}").into());
    };
    store
        .reconcile_apply(
            row.clone(),
            Disposition::Ended {
                to: crate::lifecycle::AttemptState::Failed,
                task_to: crate::protocol::TaskState::Ready,
                effects: Vec::new(),
            },
            Process::Gone {
                basis: "no_process".to_owned(),
            },
            "reconcile",
            at,
            0,
        )
        .await?;
    let (ended, next): (String, String) = Connection::open(&path)?.query_row(
        "SELECT a.ended_at, t.next_retry_at FROM attempts a, tasks t WHERE a.attempt_id = 'A1' AND t.task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        (ended.as_str(), next.as_str()),
        ("2026-11-07T00:02:00Z", "2026-11-07T00:04:00Z")
    );
    Ok(())
}

/// The reconcile at `at` of A1 (T1, a writer task, RUNNING since 01:00:00 by an earlier host life's clock; its writer
/// lease last renewed at 01:01:00, which is no evidence of running, M2T19R-502) as Ended(FAILED), with `records` seeded and T2 (a writer task) READY behind that lease: A1's
/// ended_at, the run's used duration, T1's next_retry_at, the reconcile event's `clock_behind_ms` and how many attempts
/// a tick at `at` dispatches.
async fn reconcile_a1_at(at: SystemTime, records: &str) -> R<(String, f64, String, Value, usize)> {
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'RUNNING', revision = 3, writer = 1 WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'READY', 1, 'developer', 1, 2, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
           ('A1', 'T1', 'RUNNING', 1, 1, 1, '2026-11-07T01:00:00Z');
         INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
           ('p1', 'writer', 'A1', 1, '2026-11-07T01:03:00Z');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value) VALUES ('run', 'R', 'wall_seconds', 600);
         {records}"
    ))?;
    let rows = store.reconcile_scan("p1", at).await?;
    let [row] = rows.as_slice() else {
        return Err(format!("unfinished: {rows:?}").into());
    };
    store
        .reconcile_apply(
            row.clone(),
            Disposition::Ended {
                to: crate::lifecycle::AttemptState::Failed,
                task_to: crate::protocol::TaskState::Ready,
                effects: Vec::new(),
            },
            Process::Gone {
                basis: "no_process".to_owned(),
            },
            "reconcile",
            at,
            0,
        )
        .await?;
    let dispatched = tick(&store, at).await?.len();
    let conn = Connection::open(&path)?;
    let (ended, used, next): (String, f64, String) = conn.query_row(
        "SELECT a.ended_at, b.used_value, t.next_retry_at FROM attempts a, budgets b, tasks t \
         WHERE a.attempt_id = 'A1' AND b.kind = 'wall_seconds' AND t.task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let behind: Option<i64> = conn.query_row(
        "SELECT json_extract(payload_json, '$.clock_behind_ms') FROM events WHERE kind = 'reconcile.attempt'",
        [],
        |r| r.get(0),
    )?;
    Ok((ended, used, next, json!(behind), dispatched))
}

#[tokio::test]
async fn a_reconcile_after_the_wall_clock_was_set_back_charges_what_the_attempts_own_life_recorded()
-> R {
    // M2T19R-200 by ADR-020 decision 4 (v3): stored times are UTC, so a later host life's time can be behind an
    // earlier life's when the wall clock was set back across the restart. Nothing is moved for it: with its work only
    // OBSERVED, A1 ends at the reconcile's own time (00:02:00), its lease is released then (T2 runs at once, M2T19R-300)
    // and its retry counts from then (+ Retry-After 120 s); with its work COMMITTED its recorded end is the earlier
    // life's time (the residual, below). Only the charge keeps what A1's own life recorded it ran: 5 s by its work's
    // OBSERVED end, never a lease time (M2T19R-301). How far behind A1's start the reconcile was is recorded.
    let key = crate::reconcile::work_key("A1");
    let observed = format!(
        "INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{key}', 'A1', 'OBSERVED');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', '{key}/effect.observed', 'R', 'effect.observed',
            '{{\"effect_key\": \"{key}\", \"detail\": {{\"success\": false, \"detail\": \"HTTP 429\", \"class\": \"transient\", \"retry_after_ms\": 120000, \"ended_at\": \"2026-11-07T01:00:05Z\"}}}}',
            '2026-11-07T01:00:05Z');"
    );
    let back = at("2026-11-07T00:02:00Z")?;
    assert_eq!(
        reconcile_a1_at(back, &observed).await?,
        (
            "2026-11-07T00:02:00Z".to_owned(),
            5.0,
            "2026-11-07T00:04:00Z".to_owned(),
            json!(3_480_000),
            1
        ),
        "its work's recorded end"
    );
    // With no record of its end nothing of its run is known to this life: no charge (the accepted residual), the
    // reversal recorded; its retry (no Retry-After, the draw 0) is due at once and runs.
    assert_eq!(
        reconcile_a1_at(back, "").await?,
        (
            "2026-11-07T00:02:00Z".to_owned(),
            0.0,
            "2026-11-07T00:02:00Z".to_owned(),
            json!(3_480_000),
            1
        ),
        "no record"
    );
    // Its work COMMITTED (its group seen empty): the recorded end is its end (decision 1), so its lease is released
    // and its retry counted at the earlier life's 01:00:05, ahead of this life's time: T2 waits for it, the residual
    // of decision 4 (shifted by the correction). The reversal is still recorded by this reconcile's own time
    // (M2T19R-452, -500).
    let committed = observed.replace(
        "VALUES ('A1/work', 'A1', 'OBSERVED')",
        "VALUES ('A1/work', 'A1', 'COMMITTED')",
    );
    assert_ne!(committed, observed);
    assert_eq!(
        reconcile_a1_at(back, &committed).await?,
        (
            "2026-11-07T01:00:05Z".to_owned(),
            5.0,
            "2026-11-07T01:02:05Z".to_owned(),
            json!(3_480_000),
            0
        ),
        "its work committed"
    );
    // A reconcile not behind the start charges its own time as before and records nothing.
    let (ended, used, next, behind, _) =
        reconcile_a1_at(at("2026-11-07T01:00:30Z")?, &observed).await?;
    assert_eq!(
        (ended.as_str(), used, next.as_str(), behind),
        (
            "2026-11-07T01:00:30Z",
            30.0,
            "2026-11-07T01:02:30Z",
            Value::Null
        )
    );
    Ok(())
}

#[tokio::test]
async fn an_unfinished_attempt_reserves_no_less_than_its_own_life_recorded_it_ran() -> R {
    // M2T19R-450: A1 (T1, RUNNING since 01:00:00 by an earlier life's clock, its work OBSERVED at 01:00:50 but its group
    // not seen empty) is left unfinished by a reconcile at 00:02:00 (the wall clock set back): it has run 50 s by its
    // own life's record, so with 10 s allowed T2 is not dispatched, with 55 s it is (its lease's last renewal, 60 s
    // after its start, is no evidence of running, M2T19R-502). The reconcile records how far behind A1's start it was
    // for this disposition too.
    use crate::reconcile::{Disposition, Process};
    let key = crate::reconcile::work_key("A1");
    for (limit, dispatched) in [(10, 0), (55, 1)] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(&format!(
            "{WORLD}
             UPDATE tasks SET state = 'RUNNING', revision = 3 WHERE task_id = 'T1';
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
               ('T2', 'R', 's1', 'READY', 1, 'developer', 2, 1);
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
               ('A1', 'T1', 'RUNNING', 1, 1, 1, '2026-11-07T01:00:00Z');
             INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
               ('p1', 'reader-1', 'A1', 1, '2026-11-07T01:03:00Z');
             INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{key}', 'A1', 'OBSERVED');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', '{key}/effect.observed', 'R', 'effect.observed',
                '{{\"effect_key\": \"{key}\", \"detail\": {{\"success\": false, \"detail\": \"HTTP 503\", \"class\": \"transient\", \"ended_at\": \"2026-11-07T01:00:50Z\"}}}}',
                '2026-11-07T01:00:50Z');
             INSERT INTO budgets(scope_kind, scope_id, kind, limit_value) VALUES ('run', 'R', 'wall_seconds', {limit});"
        ))?;
        let back = at("2026-11-07T00:02:00Z")?;
        let rows = store.reconcile_scan("p1", back).await?;
        let [row] = rows.as_slice() else {
            return Err(format!("unfinished: {rows:?}").into());
        };
        store
            .reconcile_apply(
                row.clone(),
                Disposition::Unreaped,
                Process::Uncertain {
                    basis: "leader_gone_group_not_empty".to_owned(),
                },
                "reconcile",
                back,
                0,
            )
            .await?;
        assert_eq!(
            tick(&store, back).await?.len(),
            dispatched,
            "{limit} s allowed"
        );
        let (state, behind): (String, Option<i64>) = Connection::open(&path)?.query_row(
            "SELECT a.state, json_extract(e.payload_json, '$.clock_behind_ms') FROM attempts a, events e \
             WHERE a.attempt_id = 'A1' AND e.kind = 'reconcile.attempt'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        assert_eq!((state.as_str(), behind), ("RUNNING", Some(3_480_000)));
    }
    Ok(())
}

#[tokio::test]
async fn a_round_judges_the_deadline_by_the_wall_clock() -> R {
    // M2T19R-251: the rounds' budget rule reads the wall clock for deadline_at, the timers' clock for the rest: with
    // the timers before the deadline and the wall clock past it nothing is dispatched; with both before it T1 is.
    for (wall, dispatched) in [
        ("2026-11-07T00:05:00Z", false),
        ("2026-11-07T00:00:30Z", true),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(&format!(
            "{WORLD}
             UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';"
        ))?;
        let context = Context {
            wall: Some(at(wall)?),
            ..context(at("2026-11-07T00:00:30Z")?)
        };
        let round = store.schedule_round("R", context).await?;
        assert_eq!(
            round.dispatched.is_some(),
            dispatched,
            "wall clock at {wall}"
        );
    }
    Ok(())
}
