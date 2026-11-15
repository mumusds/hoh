//! docs/adr/ADR-031.md decision 3 and 5 against a real store: every transaction that leaves a run in a stopping state
//! E ends with that run's one `run.result`, a valid `$defs.RunResultEvent` whose `last_event_seq` is its own seq, its id
//! from the run's revision; a Touch, an Unblock, and a run that leaves E again in the same transaction write none; two
//! runs in E get theirs in run id order; a commit that went around `writer::commit` is caught by the next begin; the
//! counts are requirements accepted under the run's specification, never criteria.

use crate::persistence::writer::{self, Ctx};
use crate::persistence::{
    Change, Commit, Entity, Intent, NewEvent, Options, PersistError, StateCas, Store, selftest,
};
use crate::protocol::RunState;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Runs `ids` of project p1 in `state`, revision 1, under spec s1 (revision 2, no requirement).
fn world(path: &Path, ids: &[&str], state: &str) -> R {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-15T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 2);",
    )?;
    for id in ids {
        conn.execute(
            "INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) \
             VALUES (?1, 'p1', 's1', ?2, 'AUTO', 1, '2026-11-15T00:00:00Z')",
            rusqlite::params![id, state],
        )?;
    }
    Ok(())
}

fn event(id: &str, run: &str) -> NewEvent {
    NewEvent {
        project_id: "p1".to_owned(),
        event_id: id.to_owned(),
        run_id: Some(run.to_owned()),
        kind: "test.moved".to_owned(),
        payload: json!({}),
        intent: Intent::Progress,
    }
}

fn revision(path: &Path, run: &str) -> R<i64> {
    Ok(Connection::open(path)?.query_row(
        "SELECT revision FROM runs WHERE run_id = ?1",
        [run],
        |r| r.get(0),
    )?)
}

fn cas(path: &Path, run: &str, change: Change) -> R<StateCas> {
    Ok(StateCas {
        id: run.to_owned(),
        expected_revision: revision(path, run)?,
        change,
    })
}

/// Every event from seq `after` on, as the API serves it (`$defs.Event`).
fn events_after(path: &Path, after: i64) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT seq, project_id, event_id, run_id, kind, payload_json, created_at FROM events WHERE seq > ?1 \
         ORDER BY seq",
    )?;
    let rows = statement.query_map([after], |r| {
        Ok(json!({
            "schema_version": "1.0",
            "seq": r.get::<_, i64>(0)?,
            "project_id": r.get::<_, String>(1)?,
            "event_id": r.get::<_, String>(2)?,
            "run_id": r.get::<_, Option<String>>(3)?,
            "attempt_id": null,
            "kind": r.get::<_, String>(4)?,
            "payload": serde_json::from_str::<Value>(&r.get::<_, String>(5)?).unwrap_or(Value::Null),
            "at": r.get::<_, String>(6)?,
        }))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn max_seq(path: &Path) -> R<i64> {
    Ok(Connection::open(path)?
        .query_row("SELECT coalesce(max(seq), 0) FROM events", [], |r| r.get(0))?)
}

/// `result` is run `run`'s run.result: valid, at its own seq, its id from the run's revision now, in `state`.
fn assert_result(path: &Path, result: &Value, run: &str, state: &str) -> R {
    let violations = crate::schema::protocol().validate_def("RunResultEvent", result)?;
    assert!(violations.is_ok(), "{violations:?}: {result}");
    assert_eq!(result["kind"], "run.result");
    assert_eq!(result["run_id"], run);
    assert_eq!(result["payload"]["state"], state);
    assert_eq!(result["payload"]["run_id"], run);
    assert_eq!(result["payload"]["last_event_seq"], result["seq"]);
    assert_eq!(
        result["event_id"],
        crate::ids::child_id(run, &format!("result/{}", revision(path, run)?))
    );
    Ok(())
}

/// The legal way into each state of E from a working state: from where, through where.
fn path_to(state: RunState) -> (&'static str, Vec<RunState>) {
    match state {
        RunState::Cancelled => ("RUNNING", vec![RunState::CancelRequested, state]),
        RunState::Paused => ("RUNNING", vec![RunState::PauseRequested, state]),
        _ => ("VERIFYING", vec![state]),
    }
}

const E: [RunState; 6] = [
    RunState::Completed,
    RunState::Failed,
    RunState::Blocked,
    RunState::Incomplete,
    RunState::Cancelled,
    RunState::Paused,
];

#[tokio::test]
async fn each_stopping_state_ends_its_commit_with_one_valid_run_result() -> R {
    use crate::lifecycle::Lifecycle as _;
    for state in E {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        let (from, through) = path_to(state);
        world(&path, &["R"], from)?;
        if state == RunState::Blocked {
            Connection::open(&path)?.execute(
                "UPDATE runs SET blocked_reason = 'NEED_DECISION' WHERE run_id = 'R'",
                [],
            )?;
        }
        for (n, to) in through.iter().enumerate() {
            let before = max_seq(&path)?;
            let done = store
                .commit(Commit {
                    cas: vec![cas(&path, "R", Change::Run(*to))?],
                    events: vec![
                        event(&format!("moved/{n}"), "R"),
                        event(&format!("also/{n}"), "R"),
                    ],
                    ..Commit::default()
                })
                .await?;
            let written = events_after(&path, before)?;
            if crate::exit::run_code(*to).is_none() {
                assert_eq!(written.len(), 2, "{to:?}: {written:?}");
                continue;
            }
            // The commit's own events first, then its run.result, the last event of the commit.
            assert_eq!(written.len(), 3, "{state:?}: {written:?}");
            assert_eq!(
                done.event_seqs,
                [written[0]["seq"].clone(), written[1]["seq"].clone()]
                    .iter()
                    .filter_map(Value::as_i64)
                    .collect::<Vec<_>>()
            );
            assert_result(&path, &written[2], "R", to.as_str())?;
            let payload = &written[2]["payload"];
            assert_eq!(
                payload["exit_code"],
                json!(crate::exit::run_code(*to)),
                "{state:?}"
            );
            assert_eq!(
                payload["blocked_reason"],
                if *to == RunState::Blocked {
                    json!("NEED_DECISION")
                } else {
                    Value::Null
                }
            );
            assert_eq!(payload["spec_revision"], 2);
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_touch_an_unblock_and_a_run_that_leaves_e_in_the_same_commit_write_no_result() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, &["R"], "VERIFYING")?;
    // A Touch, in a working state and in a stay state.
    store
        .commit(Commit {
            cas: vec![cas(&path, "R", Change::Touch(Entity::Run))?],
            ..Commit::default()
        })
        .await?;
    Connection::open(&path)?.execute(
        "UPDATE runs SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION', blocked_reasons_json = '[]', \
         resume_state = 'VERIFYING' WHERE run_id = 'R'",
        [],
    )?;
    let before = max_seq(&path)?;
    store
        .commit(Commit {
            cas: vec![cas(&path, "R", Change::Touch(Entity::Run))?],
            events: vec![event("touched", "R")],
            ..Commit::default()
        })
        .await?;
    // An Unblock back to work.
    store
        .commit(Commit {
            cas: vec![cas(&path, "R", Change::Unblock(RunState::Verifying))?],
            events: vec![event("unblocked", "R")],
            ..Commit::default()
        })
        .await?;
    // Into BLOCKED and out of it again in one commit.
    Connection::open(&path)?.execute(
        "UPDATE runs SET blocked_reason = 'NEED_DECISION' WHERE run_id = 'R'",
        [],
    )?;
    let r = revision(&path, "R")?;
    store
        .commit(Commit {
            cas: vec![
                StateCas {
                    id: "R".to_owned(),
                    expected_revision: r,
                    change: Change::Run(RunState::Blocked),
                },
                StateCas {
                    id: "R".to_owned(),
                    expected_revision: r + 1,
                    change: Change::Unblock(RunState::Verifying),
                },
            ],
            events: vec![event("in-and-out", "R")],
            ..Commit::default()
        })
        .await?;
    let kinds: Vec<Value> = events_after(&path, before)?
        .into_iter()
        .map(|e| e["kind"].clone())
        .collect();
    assert_eq!(kinds, ["test.moved", "test.moved", "test.moved"]);
    Ok(())
}

#[tokio::test]
async fn two_runs_stopping_in_one_commit_get_their_results_last_in_run_id_order() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, &["R2", "R1"], "VERIFYING")?;
    let before = max_seq(&path)?;
    store
        .commit(Commit {
            cas: vec![
                cas(&path, "R2", Change::Run(RunState::Failed))?,
                cas(&path, "R1", Change::Run(RunState::Completed))?,
            ],
            events: vec![event("e2", "R2"), event("e1", "R1")],
            ..Commit::default()
        })
        .await?;
    let written = events_after(&path, before)?;
    assert_eq!(written.len(), 4);
    assert_eq!(
        written
            .iter()
            .map(|e| e["event_id"].clone())
            .collect::<Vec<_>>()[..2],
        [json!("e2"), json!("e1")]
    );
    assert_result(&path, &written[2], "R1", "COMPLETED")?;
    assert_result(&path, &written[3], "R2", "FAILED")?;
    Ok(())
}

#[tokio::test]
async fn a_commit_around_the_writers_commit_is_caught_by_the_next_begin() -> R {
    // A transaction that registered a run's result and committed without writer::commit leaves its row: the next
    // begin of the writer refuses to go on (decision 3's guard).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, &["R"], "VERIFYING")?;
    drop(store);
    let mut conn = Connection::open(&path)?;
    let ctx = Ctx::quiet();
    let tx = writer::begin(&mut conn, &ctx)?;
    writer::write(
        &tx,
        &Commit {
            cas: vec![cas(&path, "R", Change::Run(RunState::Completed))?],
            ..Commit::default()
        },
        &ctx,
    )?;
    tx.commit()?;
    let refused = writer::begin(&mut conn, &ctx);
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{:?}",
        refused.map(|_| ())
    );
    Ok(())
}

#[tokio::test]
async fn a_run_result_counts_requirements_accepted_under_the_runs_spec_not_criteria() -> R {
    // Decision 5, CR-020: two ACCEPTED candidates each pass a different requirement and a VERIFYING one a third; a
    // fourth has one criterion of two passed (a per-criterion count would see it). accepted is the union of the
    // ACCEPTED ones' requirements, remaining the others in order.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, &["R"], "VERIFYING")?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES
           ('REQ-001', 's1', 'a', 1), ('REQ-002', 's1', 'b', 1), ('REQ-003', 's1', 'c', 1), ('REQ-004', 's1', 'd', 1);
         INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-001-01', 'REQ-001', 's1', 'x', 'unit'), ('AC-002-01', 'REQ-002', 's1', 'x', 'unit'),
           ('AC-003-01', 'REQ-003', 's1', 'x', 'unit'), ('AC-004-01', 'REQ-004', 's1', 'x', 'unit'),
           ('AC-004-02', 'REQ-004', 's1', 'x', 'unit');
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('Q', 'R', 's1', 'ACCEPTED', 'qa', 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, cost_usd) VALUES
           ('QA1', 'Q', 'SETTLED', 1, 1, 1, 0.25);
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES ('run', 'R', 'usd', 10, 1);
         INSERT INTO artifacts(artifact_id, sha256, path, bytes, mime) VALUES
           ('a1', printf('%064d', 7), 'execution.json', 1, 'application/json');
         INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at) VALUES
           ('C1', 'R', printf('%040d', 1), printf('%040d', 2), printf('%064d', 1), printf('%064d', 4),
            printf('%064d', 5), '/repo', 'ACCEPTED', '2026-11-15T00:00:00Z'),
           ('C2', 'R', printf('%040d', 3), printf('%040d', 4), printf('%064d', 1), printf('%064d', 4),
            printf('%064d', 5), '/repo', 'ACCEPTED', '2026-11-15T00:00:00Z'),
           ('C3', 'R', printf('%040d', 5), printf('%040d', 6), printf('%064d', 1), printf('%064d', 4),
            printf('%064d', 5), '/repo', 'VERIFYING', '2026-11-15T00:00:00Z');
         INSERT INTO evidence(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode,
           created_at) VALUES
           ('E1', 'C1', 'QA1', 'a1', 'unit', 'check', 'PASS', 'product', '2026-11-15T00:00:00Z'),
           ('E2', 'C2', 'QA1', 'a1', 'unit', 'check', 'PASS', 'product', '2026-11-15T00:00:00Z'),
           ('E3', 'C3', 'QA1', 'a1', 'unit', 'check', 'PASS', 'product', '2026-11-15T00:00:00Z');
         INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status) VALUES
           ('AC-001-01', 's1', 'C1', 'E1', 'PASS'), ('AC-004-01', 's1', 'C1', 'E1', 'PASS'),
           ('AC-002-01', 's1', 'C2', 'E2', 'PASS'), ('AC-003-01', 's1', 'C3', 'E3', 'PASS');",
    )?;
    let before = max_seq(&path)?;
    store
        .commit(Commit {
            cas: vec![cas(&path, "R", Change::Run(RunState::Completed))?],
            ..Commit::default()
        })
        .await?;
    let result = events_after(&path, before)?.pop().ok_or("no result")?;
    assert_result(&path, &result, "R", "COMPLETED")?;
    let payload = &result["payload"];
    assert_eq!(
        (
            &payload["accepted"],
            &payload["total"],
            &payload["remaining_requirements"]
        ),
        (&json!(2), &json!(4), &json!(["REQ-003", "REQ-004"]))
    );
    assert_eq!(
        (&payload["cost_usd"], &payload["cost_status"]),
        (&json!(0.25), &json!("known"))
    );
    // With no specification: nothing to count; an unknown price: an unknown cost.
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "UPDATE runs SET spec_id = NULL WHERE run_id = 'R';
         UPDATE budgets SET price_known = 0 WHERE scope_id = 'R';",
    )?;
    let none = super::compute(&conn, "R", 99)?;
    assert_eq!(
        (
            none.spec_revision,
            none.accepted,
            none.total,
            none.remaining_requirements
        ),
        (None, 0, 0, Vec::<String>::new())
    );
    assert_eq!(
        (none.cost_usd, none.cost_status, none.last_event_seq),
        (None, crate::protocol::CostStatus::Unknown, 99)
    );
    Ok(())
}

/// The persistence module's product sources (test files and comments left out): (path, text).
fn persistence_sources() -> R<Vec<(PathBuf, String)>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/persistence");
    let mut found = Vec::new();
    let mut stack = vec![dir];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                let text = std::fs::read_to_string(&path)?;
                let code = text
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                found.push((path, code));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// The body of the first function of `text` whose signature starts with `signature`.
fn body<'a>(text: &'a str, signature: &str) -> Option<&'a str> {
    let start = text.find(signature)?;
    let open = start + text[start..].find('{')?;
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn every_commit_of_the_writer_is_its_commit_function() -> R {
    // Decision 3, scan: `.commit()` appears in persistence's product code only in writer::commit (and its form for the
    // transactions that never were a fault point), each after `finish`, and in begin_host, which moves the host epoch
    // outside writer::begin (migrations commit with SQL of their own).
    let mut found = Vec::new();
    for (path, text) in persistence_sources()? {
        let mut rest = text.clone();
        for (file, function) in [
            ("writer.rs", "fn commit(tx: Transaction"),
            ("writer.rs", "fn commit_unhooked(tx: Transaction"),
            ("commands.rs", "fn begin_host("),
        ] {
            if path.ends_with(file)
                && let Some(inside) = body(&text, function)
            {
                assert_eq!(inside.matches(".commit()").count(), 1, "{function}");
                assert!(
                    function.contains("begin_host") || inside.contains("finish(&tx, ctx)?;"),
                    "{function}"
                );
                rest = rest.replacen(inside, "", 1);
            }
        }
        let n = rest.matches(".commit()").count();
        if n > 0 {
            found.push(format!("{}: {n}", path.display()));
        }
    }
    assert_eq!(found, Vec::<String>::new());
    Ok(())
}
