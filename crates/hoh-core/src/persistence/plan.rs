//! A plan's import (REQ-038, AC-038-03): [`check`] validates it against its run, [`import_in`] writes its tasks,
//! dependencies and requirements with one `plan.imported` event in the caller's transaction, moving no run
//! (docs/adr/ADR-031.md decision 2: a Commander's create_tasks is imported in its decision's transaction, the run moved
//! once by the decision). [`super::Store::import_plan`] is both in one IMMEDIATE transaction with the run's one
//! revision move, or nothing at all.

use super::decide::child_id;
use super::writer::{self, Change, Commit, Entity, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::dag;
use crate::schedule::DEFAULT_COST;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// An imported plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The new tasks in their canonical order (the order of their `created_seq`).
    pub task_ids: Vec<String>,
    /// The run's new revision (an import moves it, so proposals made before it are stale).
    pub run_revision: i64,
    pub event_seq: i64,
}

/// A plan [`check`] found valid for its run, as the transaction that checked it read the run: what [`import_in`]
/// writes in that transaction.
pub(super) struct ValidPlan {
    run_id: String,
    project_id: String,
    spec_id: String,
    /// The run's revision when it was checked.
    run_revision: i64,
    plan: dag::Plan,
    /// The run's tasks and their states (whether a planned task's dependencies are all ACCEPTED).
    states: BTreeMap<String, String>,
    /// The run's highest `created_seq`: the plan's tasks come after it.
    base: i64,
}

/// Import `raw` (a `PlanProposal`) for run `run_id` in one transaction of its own, with the run's one revision move.
pub(super) fn import(
    conn: &mut Connection,
    run_id: &str,
    raw: &Value,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Imported, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let plan = check(&tx, run_id, raw)?;
    let touched = writer::write(
        &tx,
        &Commit {
            cas: vec![StateCas {
                id: run_id.to_owned(),
                expected_revision: plan.run_revision,
                change: Change::Touch(Entity::Run),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    let (task_ids, event_seq) = import_in(&tx, plan, None, ctx)?;
    super::writer::commit_unhooked(tx, ctx)?;
    Ok(Imported {
        task_ids,
        run_revision: touched.revisions[0],
        event_seq,
    })
}

/// Validate `raw` (a `PlanProposal`) for run `run_id` in the caller's transaction, writing nothing: its schema, scores,
/// ids and dependencies (`dag::validate`), no task of the Commander's and none behind a supervision task, and only
/// requirements of the run's specification. A refused plan is `PersistError::Plan`.
pub(super) fn check(tx: &Connection, run_id: &str, raw: &Value) -> Result<ValidPlan, PersistError> {
    let run: Option<(String, Option<String>, i64)> = tx
        .query_row(
            "SELECT project_id, spec_id, revision FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, spec_id, run_revision)) = run else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    let spec_id = spec_id.ok_or_else(|| {
        PersistError::IntegrityViolation(format!("run {run_id} has no specification to plan for"))
    })?;
    let mut states: BTreeMap<String, String> = BTreeMap::new();
    {
        let mut statement = tx
            .prepare("SELECT task_id, state FROM tasks WHERE run_id = ?1")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sqlite_error)?;
        for row in rows {
            let (id, state) = row.map_err(sqlite_error)?;
            states.insert(id, state);
        }
    }
    let named: Vec<&str> = raw
        .get("tasks")
        .and_then(Value::as_array)
        .map(|tasks| {
            tasks
                .iter()
                .filter_map(|t| t.get("task_id").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    let taken: BTreeSet<String> = {
        let mut statement = tx
            .prepare("SELECT task_id FROM tasks WHERE run_id <> ?1 AND task_id IN (SELECT value FROM json_each(?2))")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![run_id, json!(named).to_string()], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)?
    };
    let existing: BTreeSet<String> = states.keys().cloned().collect();
    let plan = dag::validate(raw, &existing, &taken).map_err(PersistError::Plan)?;
    // The Commander does no work task (CONTRACT 6): no planned task of its role, and none behind a supervision task
    // (docs/adr/ADR-025.md appendix, -66).
    let supervision: BTreeSet<String> = {
        let mut statement = tx
            .prepare("SELECT task_id FROM tasks WHERE run_id = ?1 AND role = 'commander'")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![run_id], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)?
    };
    let refused: Vec<String> = plan
        .tasks
        .iter()
        .flat_map(|t| {
            let own = (t.role == "commander")
                .then(|| format!("task {}: the commander does no work task", t.task_id));
            let behind = t
                .depends_on
                .iter()
                .filter(|d| supervision.contains(*d))
                .map(move |d| {
                    format!(
                        "task {}: {d} is a supervision task, nothing depends on it",
                        t.task_id
                    )
                });
            own.into_iter().chain(behind)
        })
        .collect();
    if !refused.is_empty() {
        return Err(PersistError::Plan(dag::PlanError::Invalid(refused)));
    }
    // A task that references no requirement makes the whole plan invalid (REQ-028 behaviour 7, REQ-031: no rewrite of
    // the architecture without a requirement in the user's approved scope; docs/adr/ADR-039.md decision 6).
    let unreferenced: Vec<String> = plan
        .tasks
        .iter()
        .filter(|t| t.requirement_ids.is_empty())
        .map(|t| {
            format!(
                "task {}: references no requirement of the approved specification",
                t.task_id
            )
        })
        .collect();
    if !unreferenced.is_empty() {
        return Err(PersistError::Plan(dag::PlanError::Invalid(unreferenced)));
    }
    // A requirement the run's specification does not have makes the plan invalid (M2T03R-55): a bad request, not a
    // constraint failure of the insert.
    let known: BTreeSet<String> = {
        let mut statement = tx
            .prepare("SELECT requirement_id FROM requirements WHERE spec_id = ?1")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![spec_id], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)?
    };
    let spec = spec_id.as_str();
    let unknown: Vec<String> = plan
        .tasks
        .iter()
        .flat_map(|t| {
            t.requirement_ids
                .iter()
                .filter(|r| !known.contains(*r))
                .map(move |r| {
                    format!(
                        "task {}: requirement {r} is not in specification {spec}",
                        t.task_id
                    )
                })
        })
        .collect();
    if !unknown.is_empty() {
        return Err(PersistError::Plan(dag::PlanError::Invalid(unknown)));
    }
    let base: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(created_seq), 0) FROM tasks WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(ValidPlan {
        run_id: run_id.to_owned(),
        project_id,
        spec_id,
        run_revision,
        plan,
        states,
        base,
    })
}

/// Write `plan`'s tasks, dependencies and requirements and its `plan.imported` in the caller's transaction, the one
/// that checked it; the run's revision is not moved here (its caller moves it once). The event names `decision`, the
/// create_tasks that imports it, when there is one (CR-043: a client links the created tasks to it by id). The new
/// tasks' ids in their canonical order and the event's seq.
pub(super) fn import_in(
    tx: &Transaction<'_>,
    plan: ValidPlan,
    decision: Option<&str>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(Vec<String>, i64), PersistError> {
    let ValidPlan {
        run_id,
        project_id,
        spec_id,
        plan,
        states,
        base,
        ..
    } = plan;
    let run_id = run_id.as_str();
    // A task another one of the plan waits for unblocks it (layer 1); the rest are normal (layer 2). Layer 0 is set
    // for critical fixes by their own path (M2-T16/T18).
    let depended_on: BTreeSet<&str> = plan
        .tasks
        .iter()
        .flat_map(|t| t.depends_on.iter().map(String::as_str))
        .collect();
    let mut listed = Vec::with_capacity(plan.tasks.len());
    for (task, seq) in plan.tasks.iter().zip(base + 1..) {
        let ready = task
            .depends_on
            .iter()
            .all(|d| states.get(d).is_some_and(|s| s == "ACCEPTED"));
        let state = if ready { "READY" } else { "PENDING" };
        let layer = if depended_on.contains(task.task_id.as_str()) {
            1
        } else {
            2
        };
        tx.execute(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, user_value, \
             unblock_value, risk_reduction, information_gain, cost_estimate, layer) \
             VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                task.task_id,
                run_id,
                spec_id,
                state,
                task.role,
                task.writer,
                seq,
                task.user_value.unwrap_or(0),
                task.unblock_value.unwrap_or(0),
                task.risk_reduction.unwrap_or(0),
                task.information_gain.unwrap_or(0),
                task.cost_estimate.unwrap_or(DEFAULT_COST),
                layer
            ],
        )
        .map_err(sqlite_error)?;
        listed.push(json!({
            "task_id": task.task_id,
            "title": task.title,
            "state": state,
            "layer": layer,
            "created_seq": seq,
            // Its path scope (docs/adr/ADR-035.md decision 3, CR-033): kept here, not in a column.
            "paths": task.paths,
        }));
    }
    for task in &plan.tasks {
        for dep in &task.depends_on {
            tx.execute(
                "INSERT INTO task_dependencies(task_id, depends_on) VALUES (?1, ?2)",
                params![task.task_id, dep],
            )
            .map_err(sqlite_error)?;
        }
        for requirement in &task.requirement_ids {
            tx.execute(
                "INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES (?1, ?2, ?3)",
                params![task.task_id, requirement, spec_id],
            )
            .map_err(sqlite_error)?;
        }
    }
    let task_ids: Vec<String> = plan.tasks.iter().map(|t| t.task_id.clone()).collect();
    let mut imported = json!({ "run_id": run_id, "tasks": listed });
    if let Some(decision) = decision {
        imported["decision_id"] = json!(decision);
    }
    let mut commit = Commit {
        events: vec![NewEvent {
            intent: Intent::Progress,
            project_id,
            event_id: child_id(run_id, &format!("plan/{}", base + 1)),
            run_id: Some(run_id.to_owned()),
            kind: "plan.imported".to_owned(),
            payload: imported,
        }],
        ..Commit::default()
    };
    commit.events.extend(super::spec::replace_after_import(
        tx,
        run_id,
        &spec_id,
        &task_ids,
        &commit.events[0],
    )?);
    let done = writer::write(tx, &commit, ctx)?;
    Ok((task_ids, done.event_seqs[0]))
}

/// Task `task_id`'s path scope as the latest plan.imported naming it recorded it (docs/adr/ADR-035.md decision 3).
pub(super) fn task_paths(conn: &Connection, task_id: &str) -> Result<Vec<String>, PersistError> {
    let paths: Option<Option<String>> = conn
        .query_row(
            "SELECT json_extract(j.value, '$.paths') FROM events e, \
             json_each(json_extract(e.payload_json, '$.tasks')) j \
             WHERE e.run_id = (SELECT run_id FROM tasks WHERE task_id = ?1) AND e.kind = 'plan.imported' \
             AND json_extract(j.value, '$.task_id') = ?1 ORDER BY e.seq DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    match paths.flatten() {
        None => Ok(Vec::new()),
        Some(text) => serde_json::from_str(&text).map_err(|e| {
            PersistError::IntegrityViolation(format!("task {task_id}'s planned paths: {e}"))
        }),
    }
}

/// Mode-level rejection precedes import, and Commander decisions persist it as ILLEGAL_TRANSITION.
pub(super) fn mode_refusal(
    conn: &Connection,
    run: &str,
    raw: &Value,
) -> Result<Option<crate::decision::Rejection>, PersistError> {
    let (mode, spec): (String, Option<String>) = conn
        .query_row(
            "SELECT mode,spec_id FROM runs WHERE run_id=?1",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| PersistError::RunNotFound(run.into()))?;
    let tasks: Vec<&Value> = raw["tasks"].as_array().into_iter().flatten().collect();
    let blocked: Vec<&str> = tasks
        .iter()
        .filter(|t| t["writer"] == true)
        .filter_map(|t| t["task_id"].as_str())
        .collect();
    let (detail, details) = if mode == "PLAN" && !blocked.is_empty() {
        (
            "PLAN 不实施",
            json!({"message":"PLAN 不实施","task_ids":blocked}),
        )
    } else if mode == "BUILD" {
        let mut missing = BTreeSet::new();
        for requirement in tasks
            .iter()
            .flat_map(|t| t["requirement_ids"].as_array().into_iter().flatten())
            .filter_map(Value::as_str)
        {
            let known: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM requirements WHERE spec_id=?1 AND requirement_id=?2)",
                params![spec, requirement], |r| r.get(0),
            ).map_err(sqlite_error)?;
            if !known {
                missing.insert(requirement.to_owned());
            }
        }
        if missing.is_empty() {
            return Ok(None);
        }
        (
            "BUILD 不改规格，用 /plan 或 CR",
            json!({"message":"BUILD 不改规格，用 /plan 或 CR","requirement_ids":missing}),
        )
    } else {
        return Ok(None);
    };
    Ok(Some(crate::decision::Rejection {
        reason: crate::protocol::RejectionReason::IllegalTransition,
        detail: detail.into(),
        details: Some(details),
    }))
}
