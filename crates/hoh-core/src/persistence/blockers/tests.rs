//! Run-level blocker claims against a real store (docs/adr/ADR-025.md decision 5): the pace's claims recomputed as a
//! whole, each source releasing only its own, a legacy row taken over, a BLOCKED run paced and dispatching only its
//! scheduled retries, and a retry's identity kept until its attempt settles.

use crate::decision::Context;
use crate::persistence::{AttemptFacts, Ending, Options, Store, selftest};
use crate::reconcile::EffectState;
use crate::retry::FailureClass;
use crate::time::parse_rfc3339;
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

/// The pace, then rounds until nothing more is dispatched; the attempts dispatched.
async fn tick(store: &Store, now: SystemTime) -> R<Vec<String>> {
    store.pace_run("R", context(now)).await?;
    let mut dispatched = Vec::new();
    while let Some(d) = store.schedule_round("R", context(now)).await?.dispatched {
        dispatched.push(d.attempt_id);
    }
    Ok(dispatched)
}

/// The run's state, its blocked_reason and its blocker set.
fn run(path: &Path) -> R<(String, Option<String>, String)> {
    Ok(Connection::open(path)?.query_row(
        "SELECT state, blocked_reason, blocked_reasons_json FROM runs WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

fn revision(path: &Path) -> R<i64> {
    Ok(Connection::open(path)?.query_row(
        "SELECT revision FROM runs WHERE run_id = 'R'",
        [],
        |r| r.get(0),
    )?)
}

/// The run's claim events in order, as `claimed|released reason source` (with ` migrated` for a taken-over row).
fn claims(path: &Path) -> R<Vec<String>> {
    let conn = Connection::open(path)?;
    let mut s = conn.prepare(
        "SELECT kind, payload_json FROM events WHERE run_id = 'R' \
         AND kind IN ('run.blocker_claimed', 'run.blocker_released') ORDER BY seq",
    )?;
    let rows = s
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(kind, payload)| {
            let p: Value = serde_json::from_str(&payload)?;
            let verb = kind.trim_start_matches("run.blocker_");
            let migrated = if p["migrated"] == json!(true) {
                " migrated"
            } else {
                ""
            };
            Ok(format!(
                "{verb} {} {}{migrated}",
                p["reason"].as_str().unwrap_or("?"),
                p["source"].as_str().unwrap_or("?")
            ))
        })
        .collect()
}

/// A claim by a source this store does not implement yet (an answer's `commander:<decision_id>`, M2-T13): its event
/// and the run's set, as that source will write them.
fn foreign_claim(path: &Path, reason: &str, source: &str) -> R {
    let conn = Connection::open(path)?;
    conn.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
         ('p1', ?1, 'R', 'run.blocker_claimed', json_object('reason', ?2, 'source', ?3), '2026-11-07T00:00:00Z')",
        rusqlite::params![format!("foreign/{source}/{reason}"), reason, source],
    )?;
    conn.execute(
        "UPDATE runs SET blocked_reasons_json = (SELECT json_group_array(value) FROM \
         (SELECT DISTINCT value FROM json_each(json_insert(blocked_reasons_json, '$[#]', ?1)))), \
         blocked_reason = COALESCE(blocked_reason, ?1) WHERE run_id = 'R'",
        [reason],
    )?;
    Ok(())
}

#[tokio::test]
async fn a_stalled_run_is_blocked_by_the_paces_claims_and_works_again_once_a_task_can_move() -> R {
    // ADR-025 decision 5: with no task able to move on, the pace claims what blocks its tasks; once one can move on, it
    // releases its claims, the set is empty and the run is back in its resume state.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'T1';"
    ))?;
    assert!(tick(&store, at("2026-11-07T00:00:10Z")?).await?.is_empty());
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    assert_eq!(claims(&path)?, ["claimed NEED_DECISION pace"]);
    // The decision is taken: T1 READY again.
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'READY', blocked_reason = NULL, revision = revision + 1 WHERE task_id = 'T1';",
    )?;
    let dispatched = tick(&store, at("2026-11-07T00:00:20Z")?).await?;
    assert_eq!(run(&path)?, ("RUNNING".to_owned(), None, "[]".to_owned()));
    assert_eq!(
        claims(&path)?,
        ["claimed NEED_DECISION pace", "released NEED_DECISION pace"]
    );
    assert_eq!(dispatched.len(), 1, "back at work, T1 is dispatched");
    let (from, to): (String, String) = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json, '$.from'), json_extract(payload_json, '$.to') FROM events \
         WHERE kind = 'run.resumed' ORDER BY seq DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!((from.as_str(), to.as_str()), ("BLOCKED", "RUNNING"));
    Ok(())
}

#[tokio::test]
async fn the_paces_claims_are_recomputed_as_a_whole_while_the_run_stays_blocked() -> R {
    // ADR-025 decision 5: the pace's claims follow its tasks: one reason gone, its claim is released and the run is named
    // by what is left; the set's change moves the run's revision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate, blocked_reason) VALUES
           ('T2', 'R', 's1', 'BLOCKED', 1, 'developer', 2, 1, 'BLOCKED_AUTH');"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("BLOCKED_AUTH".to_owned()),
            "[\"BLOCKED_AUTH\",\"NEED_DECISION\"]".to_owned()
        )
    );
    let before = revision(&path)?;
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'CANCELLED', blocked_reason = NULL, revision = revision + 1 WHERE task_id = 'T2';",
    )?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:20Z")?))
        .await?;
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    assert_eq!(
        claims(&path)?,
        [
            "claimed BLOCKED_AUTH pace",
            "claimed NEED_DECISION pace",
            "released BLOCKED_AUTH pace"
        ]
    );
    assert_eq!(revision(&path)?, before + 1);
    // Nothing changed: the pace writes nothing.
    store
        .pace_run("R", context(at("2026-11-07T00:00:30Z")?))
        .await?;
    assert_eq!(revision(&path)?, before + 1);
    assert_eq!(claims(&path)?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn each_source_releases_only_its_own_claims() -> R {
    // ADR-025 decision 5: the budget's claim goes when the cost limit can be enforced, the pace's when a task can move
    // on; a claim of another source (an answer's) stays, and so the run stays BLOCKED for it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'T1';
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value, price_known) VALUES
           ('run', 'R', 'usd', 5, 0, 0);"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("BUDGET_UNENFORCEABLE".to_owned()),
            "[\"BUDGET_UNENFORCEABLE\",\"NEED_DECISION\"]".to_owned()
        )
    );
    foreign_claim(&path, "NEED_DECISION", "commander:d1")?;
    // The price is known now: the budget's claim goes, the others stay.
    Connection::open(&path)?.execute_batch("UPDATE budgets SET price_known = 1;")?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:20Z")?))
        .await?;
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    // T1 can move on: the pace's claim goes; the answer's keeps the run BLOCKED.
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'READY', blocked_reason = NULL, revision = revision + 1 WHERE task_id = 'T1';",
    )?;
    assert!(tick(&store, at("2026-11-07T00:00:30Z")?).await?.is_empty());
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    assert_eq!(
        claims(&path)?,
        [
            "claimed BUDGET_UNENFORCEABLE budget",
            "claimed NEED_DECISION pace",
            "claimed NEED_DECISION commander:d1",
            "released BUDGET_UNENFORCEABLE budget",
            "released NEED_DECISION pace"
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_blocked_row_without_claims_is_taken_over_as_the_paces_and_the_budgets() -> R {
    // ADR-025 decision 5: a run M2-T19 blocked has its set and no claim events; the first pace records its reasons as
    // the budget's and the pace's claims, then recomputes them.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'T1';
         UPDATE runs SET state = 'BLOCKED', resume_state = 'RUNNING', blocked_reason = 'BUDGET_UNENFORCEABLE',
           blocked_reasons_json = '[\"BUDGET_UNENFORCEABLE\",\"NEED_DECISION\"]' WHERE run_id = 'R';
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value, price_known) VALUES
           ('run', 'R', 'usd', 5, 0, 1);"
    ))?;
    store
        .pace_run("R", context(at("2026-11-07T00:00:10Z")?))
        .await?;
    assert_eq!(
        claims(&path)?,
        [
            "claimed BUDGET_UNENFORCEABLE budget migrated",
            "claimed NEED_DECISION pace migrated",
            "released BUDGET_UNENFORCEABLE budget"
        ]
    );
    assert_eq!(
        run(&path)?,
        (
            "BLOCKED".to_owned(),
            Some("NEED_DECISION".to_owned()),
            "[\"NEED_DECISION\"]".to_owned()
        )
    );
    Ok(())
}

#[tokio::test]
async fn a_blocked_run_dispatches_only_its_scheduled_retries_and_does_not_park() -> R {
    // ADR-025 decision 5 (CR-003): BLOCKED stops the decision-class dispatches only: the pace does not park the run
    // for T1's retry, the round dispatches T1 at its next_retry_at, marked as a retry, and never T2's first dispatch.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:01:00Z' WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'READY', 1, 'developer', 2, 1);"
    ))?;
    foreign_claim(&path, "NEED_DECISION", "commander:d1")?;
    Connection::open(&path)?.execute_batch(
        "UPDATE runs SET state = 'BLOCKED', resume_state = 'RUNNING', revision = 2 WHERE run_id = 'R';",
    )?;
    assert!(tick(&store, at("2026-11-07T00:00:30Z")?).await?.is_empty());
    assert_eq!(run(&path)?.0, "BLOCKED");
    let dispatched = tick(&store, at("2026-11-07T00:01:00Z")?).await?;
    assert_eq!(dispatched, ["T1/attempt-1"]);
    assert_eq!(run(&path)?.0, "BLOCKED");
    let (origin, next): (Option<String>, Option<String>) = Connection::open(&path)?.query_row(
        "SELECT json_extract(o.payload_json, '$.origin'), t.next_retry_at FROM outbox o, tasks t \
         WHERE json_extract(o.payload_json, '$.attempt_id') = 'T1/attempt-1' AND t.task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        (origin.as_deref(), next.as_deref()),
        (Some("retry"), Some("2026-11-07T00:01:00Z")),
        "a retry's dispatch is marked and keeps its next_retry_at"
    );
    // The runner takes a retry's dispatch on a BLOCKED run.
    let waiting = store.runner_waiting("p1").await?;
    let notice = waiting
        .dispatches
        .into_iter()
        .find(|d| d.attempt_id == "T1/attempt-1")
        .ok_or("the retry's dispatch is not offered to the runner")?;
    assert!(
        waiting.runs.contains(&"R".to_owned()),
        "a BLOCKED run is paced"
    );
    assert!(
        store
            .runner_received(notice, at("2026-11-07T00:01:00Z")?)
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn a_retrys_identity_lasts_until_its_attempt_settles() -> R {
    // ADR-025 decision 5: next_retry_at is not cleared by the dispatch, only by the attempt's SETTLED end; so a task
    // that succeeded and comes back READY (rework) is no retry.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:01:00Z' WHERE task_id = 'T1';"
    ))?;
    let start = at("2026-11-07T00:01:00Z")?;
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
    assert!(
        store
            .runner_running(
                &attempt,
                AttemptFacts {
                    pid: Some(4242),
                    ..Default::default()
                },
                json!({"kind": "proc"}),
            )
            .await?
    );
    let next = |path: &Path| -> R<Option<String>> {
        Ok(Connection::open(path)?.query_row(
            "SELECT next_retry_at FROM tasks WHERE task_id = 'T1'",
            [],
            |r| r.get(0),
        )?)
    };
    assert_eq!(next(&path)?.as_deref(), Some("2026-11-07T00:01:00Z"));
    store
        .runner_effect(
            &attempt,
            &format!("{attempt}/work"),
            EffectState::Started,
            EffectState::Committed,
            json!({}),
        )
        .await?;
    assert!(
        store
            .runner_finished(
                &attempt,
                Ending {
                    success: true,
                    detail: "done".to_owned(),
                    class: FailureClass::Transient { retry_after: None },
                    at: start + Duration::from_secs(5),
                    draw: 0,
                    observed: Default::default(),
                    unknown_effect: false,
                },
            )
            .await?
    );
    let state: String = Connection::open(&path)?.query_row(
        "SELECT state FROM attempts WHERE attempt_id = ?1",
        [&attempt],
        |r| r.get(0),
    )?;
    assert_eq!(state, "SETTLED");
    assert_eq!(next(&path)?, None);
    Ok(())
}

#[tokio::test]
async fn entering_blocked_withdraws_the_decision_class_dispatches_and_keeps_the_retries() -> R {
    // ADR-025 decision 3 (runner and BLOCKED): in the transaction that blocks the run, its PREPARED first dispatch is
    // withdrawn as a pause withdraws it (its task READY, its notice settled, not counted); its PREPARED retry stays.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET attempt_count = 1, next_retry_at = '2026-11-07T00:00:00Z' WHERE task_id = 'T1';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T2', 'R', 's1', 'READY', 1, 'developer', 2, 1);"
    ))?;
    let now = at("2026-11-07T00:00:10Z")?;
    let mut dispatched = tick(&store, now).await?;
    dispatched.sort();
    assert_eq!(dispatched, ["T1/attempt-1", "T2/attempt-1"]);
    // A cost limit that cannot be enforced: the budget claims, the run is BLOCKED.
    Connection::open(&path)?.execute_batch(
        "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value, price_known) VALUES
           ('run', 'R', 'usd', 5, 0, 0);",
    )?;
    store.pace_run("R", context(now)).await?;
    assert_eq!(run(&path)?.0, "BLOCKED");
    let rows: Vec<(String, String, String)> = {
        let conn = Connection::open(&path)?;
        let mut s = conn.prepare(
            "SELECT a.attempt_id, a.state, t.state FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             ORDER BY a.attempt_id",
        )?;
        s.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    assert_eq!(
        rows,
        [
            (
                "T1/attempt-1".to_owned(),
                "PREPARED".to_owned(),
                "LEASED".to_owned()
            ),
            (
                "T2/attempt-1".to_owned(),
                "CANCELLED".to_owned(),
                "READY".to_owned()
            )
        ]
    );
    let (withdrawn, notice, count): (String, String, i64) = Connection::open(&path)?.query_row(
        "SELECT (SELECT json_extract(payload_json, '$.attempt_id') FROM events WHERE kind = 'blocked.withdrawn'), \
         (SELECT state FROM outbox WHERE json_extract(payload_json, '$.attempt_id') = 'T2/attempt-1'), \
         (SELECT attempt_count FROM tasks WHERE task_id = 'T2')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        (withdrawn.as_str(), notice.as_str(), count),
        ("T2/attempt-1", "ACKED", 0)
    );
    Ok(())
}

#[tokio::test]
async fn a_blocked_runs_budget_stop_waits_for_its_unfinished_attempts() -> R {
    // ADR-025 decision 5: the pace judges a BLOCKED run's budget too: past its deadline it is INCOMPLETE once nothing
    // of it runs, not before.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'RUNNING', revision = 3 WHERE task_id = 'T1';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, started_at) VALUES
           ('A1', 'T1', 'RUNNING', 1, 1, 1, 4242, '2026-11-07T00:00:00Z');
         UPDATE runs SET deadline_at = '2026-11-07T00:05:00Z' WHERE run_id = 'R';"
    ))?;
    foreign_claim(&path, "NEED_DECISION", "commander:d1")?;
    Connection::open(&path)?.execute_batch(
        "UPDATE runs SET state = 'BLOCKED', resume_state = 'RUNNING', revision = 2 WHERE run_id = 'R';",
    )?;
    store
        .pace_run("R", context(at("2026-11-07T00:06:00Z")?))
        .await?;
    assert_eq!(run(&path)?.0, "BLOCKED", "A1 still runs");
    Connection::open(&path)?.execute_batch(
        "UPDATE attempts SET state = 'FAILED', ended_at = '2026-11-07T00:06:10Z' WHERE attempt_id = 'A1';",
    )?;
    store
        .pace_run("R", context(at("2026-11-07T00:06:20Z")?))
        .await?;
    assert_eq!(run(&path)?.0, "INCOMPLETE");
    Ok(())
}
