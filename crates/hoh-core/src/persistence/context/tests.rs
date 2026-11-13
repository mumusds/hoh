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
    // The turn's own identity, which its answers name (M2T09R-03).
    for id in ["- run_id：R", "- task_id：W/turn/1", "- attempt_id：A9"] {
        assert!(m.task.contains(id), "{id} missing from {}", m.task);
    }
    assert!(
        m.goal.contains("唤醒 W") && m.goal.contains("supervisor.retrigger"),
        "the wake's kind (-54, -159): {}",
        m.goal
    );
    assert!(m.schemas.contains("DecisionPayload") && m.schemas.contains("Message"));
    // Entries as gathered: the run's state and the event (the packer ranks the events first, -51); a TaskResult
    // inside an event a reference only.
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
    // docs/adr/ADR-026.md decision 11: causal 2 when a subject field matches, 1 when it names a snapshot task (directly
    // or by its attempt), 0 else; relevance = the supervisor's severity band (ADR-025 decision 7: integrity 3, critical
    // 2, high and need_decision-like 1, the rest 0) x 3 + causal (M2T09R-54); a TaskResult or a raw frame inside an
    // event, at any depth, is a reference only.
    let (_dir, conn) = world()?;
    let s = supervision(
        json!({"T1": 5, "T2": 2}),
        &[("role", "qa")],
        json!([
            {"seq": 1, "kind": "team.binding_blocked", "payload": {"role": "qa"}},
            {"seq": 2, "kind": "need_decision", "payload": {"attempt_id": "A0"}},
            {"seq": 3, "kind": "need_decision", "payload": {"task_id": "T9", "task_result_json": "SENTINEL", "detail": {"frame": "SENTINEL"}}},
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
    assert_eq!(
        of(1).map(|e| (e.causal, e.relevance)),
        Some((2, 5)),
        "high, the subject's role"
    );
    assert_eq!(
        of(2).map(|e| (e.causal, e.relevance)),
        Some((1, 4)),
        "A0 is T1's attempt"
    );
    assert_eq!(of(3).map(|e| (e.causal, e.relevance)), Some((0, 3)));
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

#[test]
fn a_large_run_state_never_pushes_the_wakes_events_out() -> R {
    // M2T09R-51: the run's state entries rank after the wake's events and each has a short form, so under a tight
    // evidence share the events (an integrity violation first) still go in, at least as their summaries.
    let (_dir, conn) = world()?;
    let mut s = supervision(
        json!({"T1": 5}),
        &[("task_id", "T1")],
        json!([
            {"seq": 7, "kind": "integrity_violation", "payload": {"task_id": "T1", "detail": "x".repeat(20_000)}},
            {"seq": 8, "kind": "need_decision", "payload": {"task_id": "T1", "detail": "y".repeat(20_000)}}
        ]),
    );
    let others: Vec<Value> = (0..200)
        .map(|n| json!({"id": format!("R/trigger/{n}"), "fingerprint": "f".repeat(60), "severity": "high"}))
        .collect();
    s.summary["other_wakes"] = json!(others);
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    let m = crate::context::mandatory(&facts, "commander", "", Some("/p/b"), Some("/p"))?;
    let template = crate::config::builtin_contract("commander").ok_or("commander")?;
    let budget = crate::context::required(template, &m)? + 1_000;
    let packed = crate::context::pack(template, &m, &facts.entries, budget)?;
    let placed = |path: &str| {
        packed
            .inputs
            .iter()
            .find(|i| i.path == path)
            .map(|i| i.disposition)
    };
    assert_ne!(
        placed("events/7"),
        Some(crate::context::Disposition::Reference),
        "the integrity violation"
    );
    assert_ne!(
        placed("events/8"),
        Some(crate::context::Disposition::Reference)
    );
    let order: Vec<&str> = packed.inputs.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(&order[..2], ["events/7", "events/8"], "{order:?}");
    Ok(())
}

#[test]
fn test_039_02_a_verification_turns_mandatory_items_come_from_its_package_and_rows() -> R {
    // docs/adr/ADR-026.md decisions 3 and 10 (M2T09R-04, -50): the collector M2-T16's QA and Reviewer dispatch will
    // use: the package's acceptance_ids' text under its spec, its candidate, its references as references only, and
    // the Developer's own account (status and summary only, marked a claim, ranked last); nothing else of its
    // TaskResult.
    let (_dir, conn) = world()?;
    let result = json!({"status": "COMPLETED", "summary": "自测通过", "issues": [{"note": "SENTINEL-R50"}],
        "next_action": "SENTINEL-R50"});
    conn.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
         ('p1', 'ready-1', 'R', 'candidate_ready', ?1, '2026-11-07T00:00:00Z')",
        rusqlite::params![
            json!({"task_id": "T1", "candidate_id": "CAND-7", "attempt_id": "A0",
            "task_result_json": result.to_string()})
            .to_string()
        ],
    )?;
    let sha = "d".repeat(64);
    let package: crate::protocol::ContextHandoff = serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T1", "goal": "交付上下文装箱", "role": "qa",
        "role_epoch": 1, "spec_sha256": format!("{:064}", 2), "spec_revision": 2,
        "candidate": {"id": "CAND-7", "git_commit": format!("{:040}", 7), "git_tree": format!("{:040}", 8),
            "spec_sha256": format!("{:064}", 2), "environment_sha256": format!("{:064}", 3),
            "test_plan_sha256": format!("{:064}", 4)},
        // Not T1's own: one of T1's and one of T2's, so only the package says which (M2T09R-152).
        "acceptance_ids": ["AC-1-02", "AC-2-01"],
        "references": [{"path": crate::objects::relative_path(&sha), "sha256": sha}],
        "output_contract": "TaskResult", "budget_tokens": 20000, "handoff_kind": "verification", "state": null
    }))?;
    let facts = verification(&conn, &package, &["T1".to_owned()])?;
    let m = crate::context::mandatory(&facts, "qa", "", None, None)?;
    for body in texts(
        &conn,
        "SELECT body FROM acceptance_criteria WHERE ac_id IN ('AC-1-02', 'AC-2-01')",
    )? {
        assert!(m.task.contains(&body), "AC text missing: {body}");
    }
    assert!(
        !m.task.contains("强制项逐字节出现"),
        "T1's AC-1-01 is not in the package"
    );
    assert!(
        m.task.contains(&format!("{:040}", 8)),
        "the candidate's tree"
    );
    assert!(
        m.schemas.contains("TaskResult") && m.schemas.contains("AcceptanceObservation"),
        "qa's schemas"
    );
    assert!(m.task.contains("CAND-7") && m.task.contains(&format!("{:040}", 7)));
    assert!(m.project.contains(&format!("{:064}", 2)));
    assert_eq!(
        facts.params.map(|p| p.budget_tokens),
        Some(20_000),
        "the package's budget"
    );
    let claim = facts
        .entries
        .iter()
        .find(|e| e.path == "claims/CAND-7")
        .ok_or("no claim")?;
    assert!(claim.text.contains("开发者自述，仅作线索") && claim.text.contains("自测通过"));
    assert!(
        facts
            .entries
            .iter()
            .all(|e| !e.text.contains("SENTINEL-R50")),
        "only status and summary"
    );
    assert!(
        facts.entries.iter().any(|e| e.text.contains(&sha)),
        "the reference listed"
    );
    assert!(
        facts
            .entries
            .iter()
            .filter(|e| e.path != claim.path)
            .all(|e| e.relevance > claim.relevance),
        "the claim ranks strictly last"
    );
    // Packed: the claim goes in after every other evidence entry.
    let template = crate::config::builtin_contract("qa").ok_or("qa")?;
    let packed = crate::context::pack(
        template,
        &m,
        &facts.entries,
        crate::context::required(template, &m)? + 2_000,
    )?;
    let evidence: Vec<&str> = packed
        .inputs
        .iter()
        .filter(|i| i.class == crate::context::Class::Evidence)
        .map(|i| i.path.as_str())
        .collect();
    assert_eq!(evidence.last(), Some(&"claims/CAND-7"), "{evidence:?}");
    // An AC the package names that is not in its spec, or a spec the project lacks: an error, never a context
    // without it (M2T09R-102).
    let mut missing = package.clone();
    missing.acceptance_ids.push("AC-9-99".to_owned());
    assert!(verification(&conn, &missing, &["T1".to_owned()]).is_err());
    let mut unknown = package.clone();
    unknown.spec_sha256 = Some("9".repeat(64));
    assert!(verification(&conn, &unknown, &["T1".to_owned()]).is_err());
    Ok(())
}

/// One event of run R.
fn event(conn: &Connection, id: &str, kind: &str, payload: &Value) -> R {
    conn.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
         ('p1', ?1, 'R', ?2, ?3, '2026-11-07T00:00:00Z')",
        rusqlite::params![id, kind, payload.to_string()],
    )?;
    Ok(())
}

/// The package of a verification of CAND-7 for T1's AC-1-02.
fn package_of_cand7() -> R<crate::protocol::ContextHandoff> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T1", "goal": "交付上下文装箱", "role": "qa",
        "role_epoch": 1, "spec_sha256": format!("{:064}", 2), "spec_revision": 2,
        "candidate": {"id": "CAND-7", "git_commit": format!("{:040}", 7), "git_tree": format!("{:040}", 8),
            "spec_sha256": format!("{:064}", 2), "environment_sha256": format!("{:064}", 3),
            "test_plan_sha256": format!("{:064}", 4)},
        "acceptance_ids": ["AC-1-02"], "references": [], "output_contract": "TaskResult", "budget_tokens": 20000,
        "handoff_kind": "verification", "state": null
    }))?)
}

#[test]
fn a_verification_turn_names_the_developers_registered_self_tests_and_its_rounds_changed_files() -> R
{
    // docs/adr/ADR-028.md decision 14: the self-test records the completion registered (kind self_test) are loads
    // labelled diagnostic and ranked last, the raw logs never; the Developer's summary is marked diagnostic too; the
    // code source runs from the earliest start HEAD of the round's sources' writer attempts to the candidate's commit.
    let (_dir, conn) = world()?;
    conn.execute_batch(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('T3', 'R', 's2', 'VERIFYING', 2, 'developer', 1, 4);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision)
           VALUES ('A3', 'T3', 'SETTLED', 1, 1, 1);",
    )?;
    // T3's start is the earlier one; A1 of T1 started later from another HEAD; T0 is no source of the round.
    event(
        &conn,
        "ws-T0",
        "attempt.workspace_start",
        &json!({"attempt_id": "A9", "head": "0".repeat(40)}),
    )?;
    event(
        &conn,
        "ws-A3",
        "attempt.workspace_start",
        &json!({"attempt_id": "A3", "head": "3".repeat(40)}),
    )?;
    event(
        &conn,
        "ws-A0",
        "attempt.workspace_start",
        &json!({"attempt_id": "A0", "head": "5".repeat(40)}),
    )?;
    produced(&conn, "A0", &[(&"e".repeat(64), "stdout.jsonl")])?;
    let item = json!({"artifact_id": "x", "sha256": "f".repeat(64),
        "path": crate::objects::relative_path(&"f".repeat(64)), "bytes": 17, "mime": "text/plain",
        "kind": "self_test", "name": ".hoh/self-tests/A0/report.txt"});
    event(
        &conn,
        &crate::ids::child_id("A0", "artifacts/self_tests"),
        "attempt.artifacts",
        &json!({"attempt_id": "A0", "items": [item]}),
    )?;
    event(
        &conn,
        "ready-1",
        "candidate_ready",
        &json!({"task_id": "T1", "candidate_id": "CAND-7", "attempt_id": "A0",
            "task_result_json": json!({"status": "COMPLETED", "summary": "自测通过"}).to_string()}),
    )?;
    let facts = verification(
        &conn,
        &package_of_cand7()?,
        &["T1".to_owned(), "T3".to_owned()],
    )?;
    assert_eq!(facts.loads.len(), 1, "{:?}", facts.loads);
    let load = &facts.loads[0];
    assert_eq!(load.sha256, "f".repeat(64));
    assert_eq!(load.name, ".hoh/self-tests/A0/report.txt");
    assert!(load.label.contains("diagnostic"), "{}", load.label);
    assert_eq!(load.relevance, 0);
    let claim = facts
        .entries
        .iter()
        .find(|e| e.path == "claims/CAND-7")
        .ok_or("no claim")?;
    assert!(claim.text.contains("diagnostic"), "{}", claim.text);
    assert_eq!(
        facts.code,
        Some(crate::context::CodeSource {
            base: "3".repeat(40),
            commit: format!("{:040}", 7)
        })
    );
    Ok(())
}

#[test]
fn a_finding_or_change_request_issue_an_earlier_closed_round_of_a_source_opened_is_delivered_to_confirm_fixed()
-> R {
    // docs/adr/ADR-029.md decision 10 (`confirm_set`, replacing ADR-028 decision 14's "a change request a rework took",
    // M2T16D-555): a finding's or a change request's Issue, OPEN or REOPENED, raised on another candidate by an earlier
    // round with one of this round's sources that is closed, is delivered in the verifier's mandatory scope (not a
    // trimmable evidence entry), marked 请确认已修复, its id in `confirm_ids`; one on the candidate under verification,
    // a RESOLVED one, one of an open round, an acceptance failure's and another source's round's are not.
    let (_dir, conn) = world()?;
    conn.execute_batch(
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at)
           VALUES ('CAND-6', 'R', printf('%040d', 6), printf('%040d', 9), printf('%064d', 2), printf('%064d', 3),
           printf('%064d', 4), '/repo', 'REJECTED', '2026-11-07T00:00:00Z');
         INSERT INTO issues(issue_id, run_id, candidate_id, fingerprint, severity, state, description) VALUES
           ('I-taken', 'R', 'CAND-6', 'fp-taken', 'high', 'OPEN', 'SENTINEL-TAKEN 改写请求：边界条件'),
           ('I-done', 'R', 'CAND-6', 'fp-done', 'high', 'RESOLVED', 'SENTINEL-DONE 已解决'),
           ('I-other', 'R', 'CAND-6', 'fp-other', 'high', 'OPEN', 'SENTINEL-OTHER 另一来源的轮次'),
           ('I-open', 'R', 'CAND-6', 'fp-open', 'high', 'OPEN', 'SENTINEL-OPEN 轮次未关闭'),
           ('I-ac', 'R', 'CAND-6', 'fp-ac', 'high', 'OPEN', 'SENTINEL-AC 验收失败'),
           ('I-own', 'R', 'CAND-7', 'fp-own', 'high', 'OPEN', 'SENTINEL-OWN 本轮自己的');",
    )?;
    let opened = |round: &str, candidate: &str, sources: &[&str]| {
        json!({"round_id": round, "candidate_id": candidate, "source_task_ids": sources,
            "tasks": {"qa": format!("{round}/task/qa"), "reviewer": format!("{round}/task/reviewer")}})
    };
    event(
        &conn,
        "o-6-1",
        "verification.round_opened",
        &opened("CAND-6/round/1", "CAND-6", &["T1"]),
    )?;
    event(
        &conn,
        "o-6-2",
        "verification.round_opened",
        &opened("CAND-6/round/2", "CAND-6", &["T3"]),
    )?;
    event(
        &conn,
        "o-6-3",
        "verification.round_opened",
        &opened("CAND-6/round/3", "CAND-6", &["T1"]),
    )?;
    for (issue, round, source) in [
        ("I-taken", "CAND-6/round/1", "change_request"),
        ("I-done", "CAND-6/round/1", "change_request"),
        ("I-other", "CAND-6/round/2", "change_request"),
        ("I-open", "CAND-6/round/3", "change_request"),
        ("I-ac", "CAND-6/round/1", "acceptance"),
        ("I-own", "CAND-6/round/1", "change_request"),
    ] {
        event(
            &conn,
            &format!("{issue}/opened"),
            "issue.opened",
            &json!({"issue_id": issue, "round_id": round, "source": source, "severity": "high"}),
        )?;
    }
    for round in ["CAND-6/round/1", "CAND-6/round/2"] {
        event(
            &conn,
            &format!("{round}/closed"),
            "verification.round_closed",
            &json!({"round_id": round}),
        )?;
    }
    event(
        &conn,
        "o-7-1",
        "verification.round_opened",
        &opened("CAND-7/round/1", "CAND-7", &["T1"]),
    )?;
    let delivery =
        crate::persistence::issues::deliver(&conn, "R", "CAND-7/round/1", &["T1".to_owned()])?;
    let listed: Vec<&String> = delivery
        .lines
        .iter()
        .filter(|l| l.contains("SENTINEL"))
        .collect();
    assert_eq!(listed.len(), 1, "{listed:?}");
    assert!(
        listed[0].contains("请确认已修复")
            && listed[0].contains("SENTINEL-TAKEN")
            && listed[0].contains("I-taken")
    );
    assert_eq!(delivery.confirm_ids, ["I-taken"]);
    // Another source's round delivers nothing of T1's.
    let other =
        crate::persistence::issues::deliver(&conn, "R", "CAND-7/round/1", &["T0".to_owned()])?;
    assert!(other.lines.is_empty() && other.confirm_ids.is_empty());
    // A verification turn's facts no longer list them as evidence entries: the store's assignment puts them in scope.
    let facts = verification(&conn, &package_of_cand7()?, &["T1".to_owned()])?;
    assert!(
        facts
            .entries
            .iter()
            .all(|e| !e.text.contains("请确认已修复"))
    );
    Ok(())
}

#[test]
fn a_finding_or_acceptance_issue_in_a_wake_is_marked_for_the_runtimes_rework_after_its_round() -> R
{
    // docs/adr/ADR-029.md decision 9 (M2T18D-258): like a change request's, an Issue a finding or an acceptance failure
    // opened is the Runtime's to rework once its round closes, and the wake says so.
    let (_dir, conn) = world()?;
    let s = supervision(
        json!({"T1": 5}),
        &[("role", "qa")],
        json!([
            {"seq": 5, "kind": "issue.opened", "payload": {"issue_id": "I1", "candidate_id": "CAND-7",
                "severity": "critical", "source": "finding", "report_id": "rep", "finding_id": "F1"}},
            {"seq": 6, "kind": "issue.opened", "payload": {"issue_id": "I2", "candidate_id": "CAND-7",
                "severity": "high", "source": "acceptance", "ac_id": "AC-1-01"}}
        ]),
    );
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    for seq in [5, 6] {
        let entry = facts
            .entries
            .iter()
            .find(|e| e.path == format!("events/{seq}"))
            .ok_or(format!("no event {seq}"))?;
        assert!(
            entry.text.contains("轮次关闭后由 Runtime 返工"),
            "{}",
            entry.text
        );
    }
    Ok(())
}

#[test]
fn a_reworked_developer_turn_is_given_its_rejected_candidates_failures_open_issues_and_changed_files()
-> R {
    // docs/adr/ADR-028.md decisions 12 and 14 (Reader::rework_context): the failure items its round's close gave the
    // task and the rejected candidate's open Issues, as evidence, and the code source with the rejected candidate.
    let (_dir, conn) = world()?;
    conn.execute_batch(
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at)
           VALUES ('CAND-6', 'R', printf('%040d', 6), printf('%040d', 9), printf('%064d', 2), printf('%064d', 3),
           printf('%064d', 4), '/repo', 'REJECTED', '2026-11-07T00:00:00Z');
         INSERT INTO issues(issue_id, run_id, candidate_id, fingerprint, severity, state, description) VALUES
           ('I-open', 'R', 'CAND-6', 'fp-open', 'critical', 'OPEN', 'SENTINEL-OPEN 边界条件'),
           ('I-done', 'R', 'CAND-6', 'fp-done', 'high', 'RESOLVED', 'SENTINEL-DONE 已解决');",
    )?;
    event(
        &conn,
        "ws-A0",
        "attempt.workspace_start",
        &json!({"attempt_id": "A0", "head": "5".repeat(40)}),
    )?;
    event(
        &conn,
        "opened-1",
        "verification.round_opened",
        &json!({"round_id": "CAND-6/round/1", "candidate_id": "CAND-6", "source_task_ids": ["T1"]}),
    )?;
    let before = facts(&conn, "R", "T1", "A1", None)?;
    assert!(before.code.is_none(), "no rework, no code source");
    // Taken before T1's last completion (its candidate_ready): not this rework's.
    event(
        &conn,
        "consumed-0",
        "verification.failures_consumed",
        &json!({"round_id": "CAND-5/round/1", "task_id": "T1", "by": "rework", "items": ["ac:SENTINEL-OLD"]}),
    )?;
    event(
        &conn,
        "ready-before",
        "candidate_ready",
        &json!({"task_id": "T1", "candidate_id": "CAND-6", "attempt_id": "A0"}),
    )?;
    event(
        &conn,
        "consumed-1",
        "verification.failures_consumed",
        &json!({"round_id": "CAND-6/round/1", "task_id": "T1", "by": "rework",
            "items": ["issue:fp-open", "ac:AC-1-02:SENTINEL-AC"]}),
    )?;
    // A second round's items, taken before T1's next completion: listed too.
    event(
        &conn,
        "opened-2",
        "verification.round_opened",
        &json!({"round_id": "CAND-6/round/2", "candidate_id": "CAND-6", "source_task_ids": ["T1"]}),
    )?;
    event(
        &conn,
        "consumed-2",
        "verification.failures_consumed",
        &json!({"round_id": "CAND-6/round/2", "task_id": "T1", "by": "rework", "items": ["ac:SENTINEL-SECOND"]}),
    )?;
    let rework = rework(&conn, "T1")?.ok_or("no rework context")?;
    assert_eq!(rework.candidate_id, "CAND-6");
    assert!(
        rework.items.iter().any(|i| i.contains("SENTINEL-AC"))
            && rework.items.iter().any(|i| i.contains("SENTINEL-SECOND")),
        "{:?}",
        rework.items
    );
    assert!(
        rework.items.iter().all(|i| !i.contains("SENTINEL-OLD")),
        "{:?}",
        rework.items
    );
    let got = facts(&conn, "R", "T1", "A1", None)?;
    let texts: Vec<&str> = got.entries.iter().map(|e| e.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("SENTINEL-AC")), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|t| t.contains("SENTINEL-OPEN") && t.contains("I-open")),
        "{texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("SENTINEL-DONE")),
        "{texts:?}"
    );
    assert_eq!(
        got.code,
        Some(crate::context::CodeSource {
            base: "5".repeat(40),
            commit: format!("{:040}", 6)
        })
    );
    Ok(())
}

#[test]
fn a_change_requests_issue_in_a_wake_is_marked_for_the_runtimes_rework_after_its_round() -> R {
    // docs/adr/ADR-028.md decision 13 (M2T16D-355): an Issue a verifier's change request opened is reworked by the
    // Runtime once its round closes; the wake that shows it says so, in its text and its short form, so the Commander
    // does not plan a repair of its own. Another Issue is shown as it is.
    let (_dir, conn) = world()?;
    let s = supervision(
        json!({"T1": 5}),
        &[("role", "qa")],
        json!([
            {"seq": 5, "kind": "issue.opened", "payload": {"issue_id": "I1", "round_id": "CAND-7/round/1",
                "candidate_id": "CAND-7", "severity": "critical", "source": "change_request"}},
            {"seq": 6, "kind": "issue.opened", "payload": {"issue_id": "I2", "severity": "critical"}}
        ]),
    );
    let facts = facts(&conn, "R", "W/turn/1", "A9", Some(&s))?;
    let of = |seq: u64| {
        facts
            .entries
            .iter()
            .find(|e| e.path == format!("events/{seq}"))
            .cloned()
            .ok_or(format!("no event {seq}"))
    };
    let marked = of(5)?;
    assert!(
        marked.text.contains("轮次关闭后由 Runtime 返工"),
        "{}",
        marked.text
    );
    assert!(
        marked
            .summary
            .as_deref()
            .is_some_and(|s| s.contains("轮次关闭后由 Runtime 返工")),
        "{:?}",
        marked.summary
    );
    assert!(!of(6)?.text.contains("轮次关闭后由 Runtime 返工"));
    Ok(())
}
