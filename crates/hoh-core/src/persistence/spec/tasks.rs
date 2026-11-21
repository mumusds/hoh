//! Revision-bound task migration and one-shot replacement pairing (ADR-033 Q5/Q6).

use super::super::writer::{Change, Commit, NewEvent, StateCas};
use super::super::{PersistError, questions, sqlite_error};
use crate::ids::child_id;
use crate::protocol::TaskState;
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;

const OPENED: &str = "spec.task_replacements_opened";
const CONSUMED: &str = "spec.task_replacements_consumed";

#[derive(Debug, Deserialize, Serialize)]
struct Cancelled {
    from: String,
    deleted_requirement_ids: Vec<String>,
    created_seq: i64,
}

#[derive(Deserialize)]
struct Opening {
    cancelled: Vec<Cancelled>,
}

pub(super) fn impact_tasks(
    tx: &Transaction<'_>,
    cr: &str,
) -> Result<BTreeSet<String>, PersistError> {
    let raw: String = tx
        .query_row(
            "SELECT json_extract(impact_json,'$.tasks') FROM change_requests WHERE cr_id=?1",
            params![cr],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    serde_json::from_str(&raw)
        .map_err(|e| PersistError::IntegrityViolation(format!("CR {cr} task closure: {e}")))
}

fn event(
    template: &NewEvent,
    id: String,
    run: &str,
    kind: &str,
    payload: serde_json::Value,
) -> NewEvent {
    NewEvent {
        event_id: id,
        run_id: Some(run.to_owned()),
        kind: kind.to_owned(),
        payload,
        ..template.clone()
    }
}

/// Only a current work question about SPEC_MISMATCH can release this task. In particular, an older mismatch
/// anchor cannot override a newer repair refusal or another reason for NEED_DECISION.
pub(super) fn migrate(
    tx: &Transaction<'_>,
    run: &str,
    spec: (&str, i64),
    affected: &BTreeSet<String>,
    template: &NewEvent,
) -> Result<Commit, PersistError> {
    let (spec_id, spec_revision) = spec;
    let mut commit = Commit::default();
    let mut cancelled = Vec::new();
    for question in questions::open(tx, run)? {
        if question.subject != Some("work")
            || question.reason.as_deref() != Some("spec_mismatch")
            || question.refusal
        {
            continue;
        }
        let Some(task) = question
            .task_id
            .as_deref()
            .filter(|t| affected.contains(*t))
        else {
            continue;
        };
        let row: Option<(String,i64,i64)> = tx.query_row("SELECT spec_id,revision,created_seq FROM tasks WHERE task_id=?1 AND run_id=?2 AND writer=1 AND state='BLOCKED' AND blocked_reason='NEED_DECISION' AND spec_id<>?3",params![task,run,spec_id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?;
        let Some((old, revision, created_seq)) = row else {
            continue;
        };
        let requirements: Vec<(String,bool)> = tx.prepare("SELECT DISTINCT tr.requirement_id,EXISTS(SELECT 1 FROM requirements q WHERE q.requirement_id=tr.requirement_id AND q.spec_id=?2) FROM task_requirements tr WHERE tr.task_id=?1 ORDER BY tr.requirement_id")
            .map_err(sqlite_error)?.query_map(params![task,spec_id], |r|Ok((r.get(0)?,r.get(1)?)))
            .map_err(sqlite_error)?.collect::<Result<_,_>>().map_err(sqlite_error)?;
        if requirements.is_empty() {
            continue;
        }
        let deleted: Vec<String> = requirements
            .iter()
            .filter(|(_, present)| !present)
            .map(|(id, _)| id.clone())
            .collect();
        let to = if deleted.is_empty() {
            TaskState::Ready
        } else {
            TaskState::Cancelled
        };
        if to == TaskState::Ready {
            // All mappings move, including unchanged requirements on a task with one changed requirement.
            tx.execute(
                "DELETE FROM task_requirements WHERE task_id=?1",
                params![task],
            )
            .map_err(sqlite_error)?;
            for (requirement, _) in &requirements {
                tx.execute("INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES(?1,?2,?3)",params![task,requirement,spec_id]).map_err(sqlite_error)?;
            }
            tx.execute(
                "UPDATE tasks SET spec_id=?1 WHERE task_id=?2",
                params![spec_id, task],
            )
            .map_err(sqlite_error)?;
        } else {
            // Partially deleted work is cancelled as a whole; do not silently drop one of its requirements.
            cancelled.push(Cancelled {
                from: task.to_owned(),
                deleted_requirement_ids: deleted.clone(),
                created_seq,
            });
        }
        tx.execute(
            "UPDATE tasks SET blocked_reason=NULL,next_retry_at=NULL WHERE task_id=?1",
            params![task],
        )
        .map_err(sqlite_error)?;
        commit.cas.push(StateCas {
            id: task.to_owned(),
            expected_revision: revision,
            change: Change::Task(to),
        });
        commit.events.push(event(template,child_id(&template.event_id,&format!("task/{task}")),run,
            if to == TaskState::Ready {"task.migrated"} else {"task.cancelled"},
            json!({"task_id":task,"from_spec_id":old,"to_spec_id":spec_id,"spec_revision":spec_revision,
                "reason":"spec_changed","question_id":question.question_id,"deleted_requirement_ids":deleted})));
    }
    if !cancelled.is_empty() {
        cancelled.sort_by(|a, b| (a.created_seq, &a.from).cmp(&(b.created_seq, &b.from)));
        commit.events.push(event(
            template,
            child_id(&template.event_id, &format!("task-replacements/{run}")),
            run,
            OPENED,
            json!({"spec_revision":spec_revision,"spec_id":spec_id,"cancelled":cancelled}),
        ));
    }
    Ok(commit)
}

/// The first successful plan import consumes every opening pending for this run. Each newly created task can
/// replace at most one cancelled task, even when several approvals occurred before this import. No in-memory queue.
pub(in crate::persistence) fn replace_after_import(
    tx: &Transaction<'_>,
    run: &str,
    spec: &str,
    created: &[String],
    template: &NewEvent,
) -> Result<Vec<NewEvent>, PersistError> {
    // The (run,kind,seq) index restricts this to replacement openings; consumption is checked by its unique id.
    let records: Vec<(String, String)> = tx
        .prepare(
            "SELECT event_id,payload_json FROM events WHERE run_id=?1 AND kind=?2 ORDER BY seq",
        )
        .map_err(sqlite_error)?
        .query_map(params![run, OPENED], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut pending = Vec::new();
    let mut cancelled = Vec::new();
    for (id, raw) in records {
        let consumed = child_id(&id, "consumed");
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id=?1)",
                params![consumed],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if exists {
            continue;
        }
        let opening: Opening = serde_json::from_str(&raw).map_err(|e| {
            PersistError::IntegrityViolation(format!("replacement opening {id}: {e}"))
        })?;
        for item in opening.cancelled {
            cancelled.push((id.clone(), item));
        }
        pending.push((id, consumed));
    }
    if pending.is_empty() {
        return Ok(Vec::new());
    }
    let revision: i64 = tx
        .query_row(
            "SELECT revision FROM specs WHERE spec_id=?1",
            params![spec],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    cancelled.sort_by(|a, b| (a.1.created_seq, &a.1.from).cmp(&(b.1.created_seq, &b.1.from)));
    let mut events = Vec::new();
    for ((opening, from), to) in cancelled.into_iter().zip(created) {
        events.push(event(
            template,
            child_id(&opening, &format!("replaced/{}", from.from)),
            run,
            "task.replaced",
            json!({"from":from.from,"to":to,"opening_id":opening,"spec_revision":revision}),
        ));
    }
    // Consume even unpaired cancellations. An empty creation list cannot reserve them for a later import.
    for (opening, consumed) in pending {
        events.push(event(template,consumed,run,CONSUMED,
            json!({"opening_id":opening,"plan_event_id":template.event_id,"spec_revision":revision})));
    }
    Ok(events)
}
