//! A run's project baseline (REQ-028; docs/adr/ADR-039.md decisions 4 and 5): the one `run.baseline` event the host's
//! read-only discovery writes while the run is in DISCOVERING, and the runs still waiting for it. Never a task, never a
//! revision of the run.

use super::writer::{self, Commit, Ctx, Intent, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::protocol::RunState;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// The kind of the baseline event.
pub(super) const KIND: &str = "run.baseline";

/// The id of run `run_id`'s baseline event: one per run.
fn event_id(run_id: &str) -> String {
    child_id(run_id, "baseline")
}

/// Whether run `run_id` has its baseline.
pub(super) fn recorded(conn: &Connection, run_id: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id(run_id)],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The project's runs in DISCOVERING without their baseline, oldest first.
pub(super) fn waiting(conn: &Connection, project_id: &str) -> Result<Vec<String>, PersistError> {
    let runs: Vec<String> = conn
        .prepare("SELECT run_id FROM runs WHERE project_id = ?1 AND state = 'DISCOVERING' ORDER BY rowid")
        .map_err(sqlite_error)?
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for run in runs {
        if !recorded(conn, &run)? {
            out.push(run);
        }
    }
    Ok(out)
}

/// Write run `run_id`'s baseline `payload` (the discovery's object; the run id is added): only while the run is in
/// DISCOVERING and has none, else nothing is written. Whether it was written.
pub(super) fn record(
    conn: &mut Connection,
    run_id: &str,
    payload: Value,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let run: Option<(String, String)> = tx
        .query_row(
            "SELECT project_id, state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    if writer::parse::<RunState>(&state)? != RunState::Discovering || recorded(&tx, run_id)? {
        return Ok(false);
    }
    let Value::Object(fields) = payload else {
        return Err(PersistError::IntegrityViolation(
            "a baseline is an object".to_owned(),
        ));
    };
    let mut payload = serde_json::Map::new();
    payload.insert("run_id".to_owned(), Value::String(run_id.to_owned()));
    payload.extend(fields);
    let tests_detected = payload
        .get("test_commands")
        .and_then(Value::as_array)
        .is_some_and(|c| {
            c.iter()
                .any(|c| c.as_str() != Some(crate::baseline::UNKNOWN))
        });
    writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id: project_id.clone(),
                event_id: event_id(run_id),
                run_id: Some(run_id.to_owned()),
                kind: KIND.to_owned(),
                payload: Value::Object(payload),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    // Detected tests run only when the user confirms (REQ-028 behaviour 3, O3): asked here, in the same transaction
    // (docs/adr/ADR-039.md decision 8).
    if tests_detected {
        let at = crate::time::rfc3339_utc(ctx.now());
        let commit = super::need::run_question(
            &tx,
            super::need::RunQuestion {
                project_id: &project_id,
                run_id,
                reason: super::need::RunReason::BaselineTests,
                question_id: child_id(run_id, "baseline_tests/question"),
                fields: serde_json::json!({"text": TESTS_TEXT, "choices": ["run", "skip"]}),
                at: &at,
                detail: "the project's detected tests wait for the user's confirmation",
            },
            ctx,
        )?;
        writer::write(&tx, &commit, ctx)?;
    }
    writer::commit(tx, ctx)?;
    Ok(true)
}

/// The question's text (docs/adr/ADR-039.md decision 8).
const TESTS_TEXT: &str = "发现检测到了项目的测试命令。答复 run：HoH 运行它们一次，按命令把通过与失败的测试记入基线，之后基线中已失败的测试不算 HoH 引入的回归；答复 skip：不运行。";

/// The kind of the event recording the baseline's test run.
pub(super) const TESTS_KIND: &str = "run.baseline_tests";

/// The id of run `run_id`'s baseline test run event: one per run.
fn tests_event_id(run_id: &str) -> String {
    child_id(run_id, "baseline/commands")
}

/// The user's `skip`: no test ran, recorded so (in the answer's transaction).
pub(super) fn skipped(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    if recorded_event(tx, &tests_event_id(run_id))? {
        return Ok(());
    }
    writer::write(
        tx,
        &Commit {
            events: vec![tests_event(
                project_id,
                run_id,
                serde_json::json!({"commands": [], "skipped": true}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

fn tests_event(project_id: &str, run_id: &str, fields: Value) -> NewEvent {
    let mut payload = serde_json::json!({"run_id": run_id});
    if let (Some(p), Value::Object(more)) = (payload.as_object_mut(), fields) {
        p.extend(more);
    }
    NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: tests_event_id(run_id),
        run_id: Some(run_id.to_owned()),
        kind: TESTS_KIND.to_owned(),
        payload,
    }
}

fn recorded_event(conn: &Connection, id: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE event_id = ?1)",
        params![id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The project's runs whose user answered `run` to the baseline tests question and whose tests have not been recorded:
/// each with its baseline's test commands.
pub(super) fn tests_waiting(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<(String, Vec<String>)>, PersistError> {
    let runs: Vec<String> = conn
        .prepare(
            "SELECT run_id FROM runs WHERE project_id = ?1 \
             AND state NOT IN ('COMPLETED', 'FAILED', 'CANCELLED', 'CANCEL_REQUESTED') ORDER BY rowid",
        )
        .map_err(sqlite_error)?
        .query_map(params![project_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for run in runs {
        if recorded_event(conn, &tests_event_id(&run))? {
            continue;
        }
        let confirmed: bool = conn
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'question.answered' \
                   AND json_extract(payload_json, '$.question_id') = ?2 \
                   AND json_extract(payload_json, '$.choice_id') = 'run')",
                params![run, child_id(&run, "baseline_tests/question")],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if !confirmed {
            continue;
        }
        let payload: Option<String> = conn
            .query_row(
                "SELECT payload_json FROM events WHERE event_id = ?1",
                params![event_id(&run)],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        let payload: Value = serde_json::from_str(payload.as_deref().unwrap_or("{}"))
            .map_err(|e| PersistError::IntegrityViolation(format!("run {run}'s baseline: {e}")))?;
        let commands = payload["test_commands"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter(|c| *c != crate::baseline::UNKNOWN)
            .map(str::to_owned)
            .collect();
        out.push((run, commands));
    }
    Ok(out)
}

/// Write run `run_id`'s baseline test run `payload` (`{commands: [...]}`), once. Whether it was written.
pub(super) fn record_tests(
    conn: &mut Connection,
    run_id: &str,
    payload: Value,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let project: Option<String> = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(project_id) = project else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    if recorded_event(&tx, &tests_event_id(run_id))? {
        return Ok(false);
    }
    writer::write(
        &tx,
        &Commit {
            events: vec![tests_event(&project_id, run_id, payload)],
            ..Commit::default()
        },
        ctx,
    )?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

/// The regression judgement of run `run_id`'s latest candidate against its baseline (REQ-028 failure mode 4;
/// docs/adr/ADR-039.md decision 8): the tests that failed in the baseline's test run, those its evidence's
/// `test-results.jsonl` records as FAIL, and of those the ones HoH introduced. Null without a candidate.
pub(super) fn regression(conn: &Connection, run_id: &str) -> Result<Value, PersistError> {
    let tested: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![tests_event_id(run_id)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let mut baseline_failed: Vec<String> = tested
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|t| {
            t["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|c| c["failed_tests"].as_array().cloned().unwrap_or_default())
                .filter_map(|n| n.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    baseline_failed.sort();
    baseline_failed.dedup();
    let latest: Option<String> = conn
        .query_row(
            "SELECT c.candidate_id FROM candidates c WHERE c.run_id = ?1 ORDER BY coalesce((SELECT max(e.seq) \
               FROM events e WHERE e.run_id = c.run_id AND e.kind = 'candidate.frozen' \
               AND json_extract(e.payload_json, '$.candidate_id') = c.candidate_id), 0) DESC, c.rowid DESC LIMIT 1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(candidate) = latest else {
        return Ok(Value::Null);
    };
    let records: Vec<String> = conn
        .prepare(
            "SELECT a.path FROM evidence e JOIN artifacts a ON a.artifact_id = e.artifact_id \
             WHERE e.candidate_id = ?1 ORDER BY e.rowid",
        )
        .map_err(sqlite_error)?
        .query_map(params![candidate], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let root = super::trace::project_root(conn);
    let mut failed = std::collections::BTreeSet::new();
    for record in records {
        let Some(root) = &root else { break };
        // The execution record's declared test-results.jsonl (project-relative, as the checks write it).
        let Some(record) = crate::objects::read_regular(&root.join(&record))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            continue;
        };
        for artifact in record["artifacts"].as_array().into_iter().flatten() {
            if artifact["name"] != "test-results.jsonl" {
                continue;
            }
            let Some(path) = artifact["path"].as_str() else {
                continue;
            };
            let bytes = crate::objects::read_regular(&root.join(path)).unwrap_or_default();
            for line in bytes.split(|b| *b == b'\n') {
                if let Ok(line) = serde_json::from_slice::<Value>(line)
                    && line["status"] == "FAIL"
                    && let Some(name) = line["name"].as_str()
                {
                    failed.insert(name.to_owned());
                }
            }
        }
    }
    let candidate_failed: Vec<String> = failed.into_iter().collect();
    let introduced = crate::baseline::regressions(&baseline_failed, &candidate_failed);
    Ok(
        serde_json::json!({"candidate_id": candidate, "baseline_failed": baseline_failed,
        "candidate_failed": candidate_failed, "introduced": introduced}),
    )
}

/// The paths run `run_id`'s baseline lists as uncommitted (its dirty and its untracked files), sorted; none without a
/// baseline.
pub(super) fn uncommitted(conn: &Connection, run_id: &str) -> Result<Vec<String>, PersistError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![event_id(run_id)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(payload) = payload else {
        return Ok(Vec::new());
    };
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("run {run_id}'s baseline: {e}")))?;
    let mut paths: Vec<String> = ["dirty", "untracked"]
        .iter()
        .flat_map(|side| payload[*side].as_array().cloned().unwrap_or_default())
        .filter_map(|f| f["path"].as_str().map(str::to_owned))
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Whether task `task_id` was asked about its uncommitted files once already: the user's answer to that question
/// decides, it is not asked again.
pub(super) fn asked(conn: &Connection, task_id: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'need_decision' \
           AND json_extract(payload_json, '$.task_id') = ?1 \
           AND json_extract(payload_json, '$.reason') = 'dirty_workspace')",
        params![task_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}
