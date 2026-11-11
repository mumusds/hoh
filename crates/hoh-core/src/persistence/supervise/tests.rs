//! The supervisor's triggers and cursor against a real store (docs/adr/ADR-025.md decision 2), with a test clock.

use crate::decision::Context;
use crate::persistence::{Commit, NewEvent, Options, Store, selftest};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
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

/// The supervisor's context at `ms`.
fn at(ms: u64) -> Context {
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
    let (store, path) = open(&dir)?;
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
        let step = store.supervise("R", at(second * 1000)).await?;
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
    let (store, path) = open(&dir)?;
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
        while next < plan.len() && plan[next].0 <= now {
            let (_, kind, payload) = &plan[next];
            let seqs = events(&store, &format!("plan/{next}"), &[(kind, payload.clone())]).await?;
            seq_at.push((plan[next].0, seqs[0]));
            next += 1;
        }
        for f in store.supervise("R", at(now)).await?.fired {
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
    let (store, path) = open(&dir)?;
    exec(&path, WORLD)?;
    let first = events(&store, "a", &[("need_decision", json!({"task_id": "D"}))]).await?;
    store.supervise("R", at(0)).await?;
    store.supervise("R", at(3000)).await?;
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM supervisor_triggers WHERE state = 'HANDLED'"
        )?,
        1
    );
    drop(store);
    let (store, _) = open(&dir)?;
    let second = events(&store, "b", &[("need_decision", json!({"task_id": "D"}))]).await?;
    store.supervise("R", at(10_000)).await?;
    let step = store.supervise("R", at(13_000)).await?;
    assert_eq!(step.fired.len(), 1);
    assert_eq!(step.fired[0].event_seqs, second, "not {first:?} again");
    Ok(())
}
