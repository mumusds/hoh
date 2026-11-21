//! Revision-bound task migration and one-shot replacement pairing (ADR-033 Q5/Q6).

use super::super::writer::{Change, Commit, Entity, NewEvent, StateCas};
use super::super::{PersistError, questions, sqlite_error};
use crate::ids::child_id;
use crate::protocol::TaskState;
use rusqlite::{Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::json;

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

/// Move every non-terminal task with its Run. Only a current, non-refused writer work question about
/// SPEC_MISMATCH also releases a block; other blocks and retry metadata survive the revision change.
pub(super) fn migrate(
    tx: &Transaction<'_>,
    run: &str,
    spec: (&str, i64),
    template: &NewEvent,
) -> Result<Commit, PersistError> {
    let (spec_id, spec_revision) = spec;
    let mut commit = Commit::default();
    let mut cancelled = Vec::new();
    let questions = questions::open(tx, run)?;
    let tasks = tx.prepare("SELECT task_id,spec_id,state,revision,created_seq,writer FROM tasks WHERE run_id=?1 AND state NOT IN ('ACCEPTED','CANCELLED') AND spec_id<>?2 ORDER BY task_id")
        .map_err(sqlite_error)?.query_map(params![run,spec_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,i64>(4)?,r.get::<_,bool>(5)?)))
        .map_err(sqlite_error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite_error)?;
    for (task, old, state, revision, created_seq, writer) in tasks {
        let question = questions
            .iter()
            .find(|q| q.task_id.as_deref() == Some(task.as_str()));
        let release = writer
            && state == "BLOCKED"
            && question.is_some_and(|q| {
                q.subject == Some("work")
                    && q.reason.as_deref() == Some("spec_mismatch")
                    && !q.refusal
            });
        let requirements: Vec<(String,bool)> = tx.prepare("SELECT DISTINCT tr.requirement_id,EXISTS(SELECT 1 FROM requirements q WHERE q.requirement_id=tr.requirement_id AND q.spec_id=?2) FROM task_requirements tr WHERE tr.task_id=?1 ORDER BY tr.requirement_id")
            .map_err(sqlite_error)?.query_map(params![task,spec_id], |r|Ok((r.get(0)?,r.get(1)?)))
            .map_err(sqlite_error)?.collect::<Result<_,_>>().map_err(sqlite_error)?;
        let deleted: Vec<String> = requirements
            .iter()
            .filter(|(_, present)| !present)
            .map(|(id, _)| id.clone())
            .collect();
        if deleted.is_empty() {
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
            // The host's single-Attempt cancel proves the process ended and releases its lease. Persist its
            // request with task cancellation so a writer cannot finish against an already CANCELLED task.
            tx.execute(
                "UPDATE attempts SET cancel_requested=1 WHERE task_id=?1 \
                 AND state IN ('PREPARED','STARTING','RUNNING')",
                params![task],
            )
            .map_err(sqlite_error)?;
            cancelled.push(Cancelled {
                from: task.clone(),
                deleted_requirement_ids: deleted.clone(),
                created_seq,
            });
        }
        if release || !deleted.is_empty() {
            tx.execute(
                "UPDATE tasks SET blocked_reason=NULL,next_retry_at=NULL WHERE task_id=?1",
                params![task],
            )
            .map_err(sqlite_error)?;
        }
        commit.cas.push(StateCas {
            id: task.clone(),
            expected_revision: revision,
            change: if !deleted.is_empty() {
                Change::Task(TaskState::Cancelled)
            } else if release {
                Change::Task(TaskState::Ready)
            } else {
                Change::Touch(Entity::Task)
            },
        });
        commit.events.push(event(template,child_id(&template.event_id,&format!("task/{task}")),run,
            if deleted.is_empty() {"task.migrated"} else {"task.cancelled"},
            json!({"task_id":task,"from_spec_id":old,"to_spec_id":spec_id,"spec_revision":spec_revision,
                "reason":"spec_changed","question_id":question.map(|q| &q.question_id),"deleted_requirement_ids":deleted})));
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
