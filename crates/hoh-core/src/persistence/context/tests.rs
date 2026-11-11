//! The collector against a real database (docs/adr/ADR-026.md decision 3, M2T09D-156): each scope's mandatory items
//! read from the rows, checked against the rows themselves, not against strings the test passed in.

use super::*;
use crate::persistence::Supervision;
use rusqlite::Connection;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', 'p1', printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at, config_snapshot_json)
  VALUES ('R', 'p1', 's2', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z',
  '{\"context\": {\"budget_tokens\": 20000, \"shares\": [50, 30, 20]}}');
INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, expected_revision, accepted_at)
  VALUES ('c1', 'R', 'k1', 'StartRun', '{\"goal\": \"交付上下文装箱\"}', printf('%064d', 0), 0, '2026-11-07T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES
  ('REQ-1', 's2', '一', 1), ('REQ-2', 's2', '二', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-1-01', 'REQ-1', 's2', '强制项逐字节出现 {{GOAL}}', 'unit'),
  ('AC-1-02', 'REQ-1', 's2', '份额是硬上限', 'unit'),
  ('AC-2-01', 'REQ-2', 's2', '另一任务的验收', 'unit');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('T0', 'R', 's2', 'IMPLEMENTED', 3, 'developer', 1, 1),
  ('T1', 'R', 's2', 'RUNNING', 5, 'developer', 1, 2),
  ('T2', 'R', 's2', 'READY', 2, 'qa', 0, 3);
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('T1', 'T0');
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('T1', 'REQ-1', 's2'), ('T2', 'REQ-2', 's2');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at)
  VALUES ('CAND-7', 'R', printf('%040d', 7), printf('%040d', 8), printf('%064d', 2), printf('%064d', 3),
  printf('%064d', 4), '/repo', 'FROZEN', '2026-11-07T00:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'plan', 'R', 'plan.imported', '{\"tasks\": [{\"task_id\": \"T1\", \"title\": \"装箱器\"}]}', '2026-11-07T00:00:00Z');
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, failure_reason, ended_at)
  VALUES ('A0', 'T1', 'FAILED', 1, 1, 1, 'CONTEXT_BUDGET_TOO_SMALL', '2026-11-07T00:01:00Z'),
         ('A1', 'T1', 'STARTING', 1, 1, 1, NULL, NULL);
";

fn world() -> R<(tempfile::TempDir, Connection)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("state.db");
    let options = crate::persistence::Options {
        wal_probe: Some(crate::persistence::selftest::test_probe(None)),
        ..crate::persistence::Options::default()
    };
    drop(crate::persistence::Store::open(&path, options)?);
    let conn = Connection::open(&path)?;
    conn.execute_batch(WORLD)?;
    Ok((dir, conn))
}

/// Every row of `sql` (one text column).
fn texts(conn: &Connection, sql: &str) -> R<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

#[test]
fn test_039_02_a_work_tasks_mandatory_items_come_from_its_rows() -> R {
    let (_dir, conn) = world()?;
    let facts = facts(&conn, "R", "T1", "A1", None)?;
    let m = crate::context::mandatory(&facts, "developer", "", None, None)?;
    let all = format!("{}\n{}\n{}\n{}", m.goal, m.project, m.task, m.schemas);
    // The rows themselves, read independently.
    for body in texts(
        &conn,
        "SELECT a.body FROM acceptance_criteria a JOIN task_requirements t ON t.requirement_id = a.requirement_id \
         WHERE t.task_id = 'T1'",
    )? {
        assert!(all.contains(&body), "AC text missing: {body}");
    }
    assert!(
        !all.contains("另一任务的验收"),
        "another task's AC is not in scope"
    );
    for value in texts(&conn, "SELECT sha256 FROM specs")?.into_iter().chain(
        texts(
            &conn,
            "SELECT candidate_id || ' ' || git_commit || ' ' || git_tree FROM candidates",
        )?
        .iter()
        .flat_map(|l| l.split(' ').map(str::to_owned).collect::<Vec<_>>()),
    ) {
        assert!(all.contains(&value), "{value} missing");
    }
    assert!(m.goal.contains("交付上下文装箱"));
    assert!(
        m.task.contains("T1 装箱器") && m.task.contains("依赖：T0") && m.task.contains("写者"),
        "{}",
        m.task
    );
    assert!(m.schemas.contains("TaskResult"));
    // Its run's own parameters; its earlier attempt's end as evidence, by runtime fields only.
    assert_eq!(
        facts.params,
        Some(crate::context::Params {
            budget_tokens: 20_000,
            shares: [50, 30, 20]
        })
    );
    assert_eq!(facts.entries.len(), 1);
    assert_eq!(facts.entries[0].path, "attempts/A0");
    assert!(facts.entries[0].text.contains("CONTEXT_BUDGET_TOO_SMALL"));
    Ok(())
}

fn supervision(snapshots: Value, subject: &[(&str, &str)], events: Value) -> Supervision {
    Supervision {
        wake: "W".to_owned(),
        nonce: "n".to_owned(),
        run_revision: 1,
        role_epoch: 1,
        summary: json!({"wake": {"id": "W", "fingerprint": "fp", "severity": "high"}, "snapshots": snapshots,
            "run": {"state": "RUNNING"}, "events": events, "events_total": 1, "tasks": [], "tasks_total": 2,
            "other_wakes": [], "other_wakes_total": 0, "roles": [], "recent_rejections": []}),
        workdir: None,
        subject: subject
            .iter()
            .map(|(f, v)| ((*f).to_owned(), (*v).to_owned()))
            .collect(),
    }
}

#[test]
fn test_039_02_a_supervision_turn_takes_every_snapshot_tasks_acceptance() -> R {
    // M2T09D-03 (its internal task has no task_requirements) and -200 (a retrigger's subject is task_ids): the in-scope
    // AC are all its snapshot tasks', whatever the subject.
    let (_dir, conn) = world()?;
    let s = supervision(
        json!({"T1": 5, "T2": 2}),
        &[("task_id", "T2")],
        json!([{"seq": 9, "kind": "supervisor.retrigger", "payload": {"task_ids": ["T2"], "reason": "STALE_REVISION"}}]),
    );
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    let m = crate::context::mandatory(&facts, "commander", "", Some("/p/b"), Some("/p"))?;
    for body in texts(&conn, "SELECT body FROM acceptance_criteria")? {
        assert!(m.task.contains(&body), "AC text missing: {body}");
    }
    assert!(
        m.task.contains("T1 装箱器")
            && m.task.contains("revision 5")
            && m.task.contains("revision 2")
    );
    assert!(
        m.task.contains("- nonce：n")
            && m.task.contains("- 输出文件：/p/b")
            && m.task.contains("- 项目根：/p")
    );
    assert!(m.goal.contains("唤醒 W"));
    assert!(m.schemas.contains("DecisionPayload") && m.schemas.contains("Message"));
    // The run's state first, then the event, a TaskResult inside an event a reference only.
    let paths: Vec<&str> = facts.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "summary/tasks",
            "summary/roles",
            "summary/other_wakes",
            "summary/recent_rejections",
            "events/9"
        ]
    );
    Ok(())
}

#[test]
fn test_039_02_a_wake_about_no_task_says_so_and_lists_no_acceptance() -> R {
    let (_dir, conn) = world()?;
    let s = supervision(json!({}), &[], json!([]));
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    let m = crate::context::mandatory(&facts, "commander", "", None, None)?;
    assert!(m.task.contains("本唤醒不涉及具体任务"));
    assert!(m.task.contains("（无）"));
    Ok(())
}

#[test]
fn an_events_causal_rank_comes_from_the_wake_subject_and_its_snapshot() -> R {
    // docs/adr/ADR-026.md decision 11: 2 when a subject field matches, 1 when it names a snapshot task (directly or by
    // its attempt), 0 else; a TaskResult inside an event is a reference only.
    let (_dir, conn) = world()?;
    let s = supervision(
        json!({"T1": 5, "T2": 2}),
        &[("role", "qa")],
        json!([
            {"seq": 1, "kind": "team.binding_blocked", "payload": {"role": "qa"}},
            {"seq": 2, "kind": "need_decision", "payload": {"attempt_id": "A0"}},
            {"seq": 3, "kind": "need_decision", "payload": {"task_id": "T9", "task_result_json": "SENTINEL"}},
            {"seq": 4, "kind": "issue.opened", "payload": {"task_id": "T9", "severity": "critical"}}
        ]),
    );
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    let of = |seq: u64| {
        facts
            .entries
            .iter()
            .find(|e| e.path == format!("events/{seq}"))
    };
    assert_eq!(of(1).map(|e| (e.causal, e.relevance)), Some((2, 2)));
    assert_eq!(
        of(2).map(|e| (e.causal, e.relevance)),
        Some((1, 1)),
        "A0 is T1's attempt"
    );
    assert_eq!(of(3).map(|e| (e.causal, e.relevance)), Some((0, 0)));
    assert_eq!(
        of(4).map(|e| (e.causal, e.relevance)),
        Some((0, 6)),
        "critical"
    );
    assert!(
        facts.entries.iter().all(|e| !e.text.contains("SENTINEL")),
        "a TaskResult is a reference only"
    );
    Ok(())
}

fn produced(conn: &Connection, attempt: &str, items: &[(&str, &str)]) -> R {
    let items: Vec<Value> = items
        .iter()
        .map(|(sha, name)| {
            json!({"artifact_id": format!("{sha}:p"), "sha256": sha, "path": crate::objects::relative_path(sha),
                "bytes": 1, "mime": "text/plain", "kind": "raw_log", "name": name})
        })
        .collect();
    conn.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
         ('p1', ?1, 'R', 'attempt.artifacts', ?2, '2026-11-07T00:00:00Z')",
        rusqlite::params![
            crate::ids::child_id(attempt, "artifacts"),
            json!({"attempt_id": attempt, "items": items}).to_string()
        ],
    )?;
    Ok(())
}

#[test]
fn a_handovers_references_are_its_producers_registered_objects_newest_first_and_bounded() -> R {
    // docs/adr/ADR-026.md decision 10 (M2T09D-100): references come from attempt.artifacts, each attempt its own
    // event, objects shared by content counted once; never more than the limit.
    let (_dir, conn) = world()?;
    let (a, b, c) = ("a".repeat(64), "b".repeat(64), "c".repeat(64));
    produced(&conn, "A0", &[(&a, "stdout.jsonl"), (&b, "stderr.log")])?;
    // A1 produced the same stderr bytes as A0: one object, both producers.
    produced(&conn, "A1", &[(&c, "stdout.jsonl"), (&b, "stderr.log")])?;
    let refs = references(&conn, &["A1".to_owned(), "A0".to_owned()], 20)?;
    let shas: Vec<&str> = refs.iter().map(|r| r.sha256.as_str()).collect();
    assert_eq!(shas, [c.as_str(), b.as_str(), a.as_str()]);
    assert!(
        refs.iter()
            .all(|r| r.path == crate::objects::relative_path(&r.sha256))
    );
    assert_eq!(
        references(&conn, &["A1".to_owned(), "A0".to_owned()], 2)?.len(),
        2
    );
    assert!(references(&conn, &["nobody".to_owned()], 20)?.is_empty());
    // The run's budget for its packages: its snapshot's.
    assert_eq!(budget(&conn, "R")?, 20_000);
    Ok(())
}
