//! [`super::Store::import_plan`]: a validated plan's tasks, dependencies and requirements in one IMMEDIATE
//! transaction with one `plan.imported` event, or nothing at all (REQ-038, AC-038-03).

use super::decide::child_id;
use super::writer::{self, Change, Commit, Entity, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::dag;
use crate::schedule::DEFAULT_COST;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
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

pub(super) fn import(
    conn: &mut Connection,
    run_id: &str,
    raw: &Value,
    after_events: &dyn Fn(),
) -> Result<Imported, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
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
    let base: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(created_seq), 0) FROM tasks WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
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
    let commit = Commit {
        cas: vec![StateCas {
            id: run_id.to_owned(),
            expected_revision: run_revision,
            change: Change::Touch(Entity::Run),
        }],
        events: vec![NewEvent {
            project_id,
            event_id: child_id(run_id, &format!("plan/{}", base + 1)),
            run_id: Some(run_id.to_owned()),
            kind: "plan.imported".to_owned(),
            payload: json!({ "run_id": run_id, "tasks": listed }),
        }],
        outbox: Vec::new(),
    };
    let done = writer::write(&tx, &commit, after_events)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Imported {
        task_ids,
        run_revision: done.revisions[0],
        event_seq: done.event_seqs[0],
    })
}
