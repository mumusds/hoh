//! The draft's user decision. All three command entrances use spec::apply (ADR-038).
use super::blockers::{self, Claims};
use super::commands::{Input, RunTarget, SubmitCommand};
use super::need::{self, RunReason};
use super::writer::{self, Change, Commit, Ctx, Intent, NewEvent, StateCas};
use super::{PersistError, SpecAction, SpecFiles, SpecRequest, sqlite_error};
use crate::ids::child_id;
use crate::protocol::{ApproveSpec, Mode, SpecDecision, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

pub(super) fn pending(
    conn: &Connection,
    run: &str,
) -> Result<Option<super::questions::Question>, PersistError> {
    Ok(super::questions::open(conn, run)?
        .into_iter()
        .find(|q| q.reason.as_deref() == Some("spec_approval")))
}

pub(super) fn answer(
    conn: &Connection,
    run: &str,
    input: &Input,
) -> Result<Option<(super::questions::Question, String)>, PersistError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE run_id=?1)",
            [run],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !exists {
        return Ok(None);
    }
    let open = super::questions::open(conn, run)?;
    let selected = match input {
        Input::Answer {
            question_id,
            answer,
            choice_id,
        } => open
            .into_iter()
            .find(|q| q.question_id == *question_id)
            .map(|q| (q, choice_id.clone().unwrap_or_else(|| answer.clone()))),
        Input::Send {
            text,
            target_role: None,
            input_kind,
        } if input_kind == "REPLY" && open.len() == 1 => {
            open.into_iter().next().map(|q| (q, text.clone()))
        }
        _ => None,
    };
    Ok(selected.filter(|(q, _)| q.reason.as_deref() == Some("spec_approval")))
}

pub(super) fn guard(
    conn: &Connection,
    command: &SubmitCommand,
    input: &Input,
    origin: crate::origin::Origin,
) -> Result<(), PersistError> {
    let replay: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM commands WHERE idempotency_key=?1 AND kind='ApproveSpec')",
            [child_id(&command.idempotency_key, "spec-approval")],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if (replay || answer(conn, &command.run_id, input)?.is_some())
        && let Some(reason) = origin.refusal()
    {
        return Err(PersistError::SpecApprovalNotUser(reason));
    }
    Ok(())
}

pub(super) fn apply_answer(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    input: &Input,
    origin: crate::origin::Origin,
    files: &Result<SpecFiles, crate::specindex::IndexError>,
    ctx: &Ctx<'_>,
) -> Result<Option<(i64, i64)>, PersistError> {
    let Some((q, text)) = answer(tx, &command.run_id, input)? else {
        return Ok(None);
    };
    guard(tx, command, input, origin)?;
    let approval = ApproveSpec {
        spec_sha256: q.anchor["spec_sha256"].as_str().unwrap_or_default().into(),
        decision: if text.trim() == "approve" {
            SpecDecision::ApproveOnly
        } else {
            SpecDecision::Reject
        },
        note: Some(match input {
            Input::Answer { answer, .. } => answer.clone(),
            Input::Send { text, .. } => text.clone(),
        }),
    };
    let payload = json!(approval);
    let mut derived = command.clone();
    derived.command_id = child_id(&command.command_id, "spec-approval");
    derived.idempotency_key = child_id(&command.idempotency_key, "spec-approval");
    derived.kind = "ApproveSpec".into();
    derived.payload_sha256 = crate::specindex::digest(payload.to_string().as_bytes());
    derived.payload = payload;
    derived.target = RunTarget::Specification(Box::new(SpecRequest {
        origin,
        action: SpecAction::Approve(approval),
        files: files.clone(),
    }));
    derived.event.event_id = child_id(&derived.command_id, "approved");
    derived.event.kind = "spec.changed".into();
    let done = super::commands::submit_in(tx, &derived, ctx)?;
    let super::commands::CommandOutcome::Accepted {
        revision,
        event_seq,
        ..
    } = done
    else {
        return Err(PersistError::IntegrityViolation(
            "a new answer unexpectedly replayed its approval".into(),
        ));
    };
    if let Input::Send { text, .. } = input {
        tx.execute("INSERT INTO input_queue(input_id,run_id,kind,target_role,state,payload_json) VALUES(?1,?2,'REPLY',NULL,'APPLIED',?3)",params![child_id(&command.command_id,"input"),command.run_id,json!({"text":text}).to_string()]).map_err(sqlite_error)?;
    }
    Ok(Some((revision, event_seq)))
}

pub(super) fn move_task(
    tx: &Transaction<'_>,
    task: &str,
    to: TaskState,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let revision = tx
        .query_row("SELECT revision FROM tasks WHERE task_id=?1", [task], |r| {
            r.get(0)
        })
        .map_err(sqlite_error)?;
    if to == TaskState::Ready {
        tx.execute(
            "UPDATE tasks SET blocked_reason=NULL,next_retry_at=NULL WHERE task_id=?1",
            [task],
        )
        .map_err(sqlite_error)?;
    }
    if to == TaskState::Blocked {
        tx.execute(
            "UPDATE tasks SET blocked_reason='NEED_DECISION' WHERE task_id=?1",
            [task],
        )
        .map_err(sqlite_error)?;
    }
    writer::write(
        tx,
        &Commit {
            cas: vec![StateCas {
                id: task.into(),
                expected_revision: revision,
                change: Change::Task(to),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

pub(super) fn answered(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    q: &super::questions::Question,
    approval: &ApproveSpec,
    ctx: &Ctx<'_>,
) -> Result<Commit, PersistError> {
    let task = q.anchor["task_id"]
        .as_str()
        .ok_or_else(|| PersistError::IntegrityViolation("draft question has no task".into()))?;
    if approval.decision == SpecDecision::Reject {
        move_task(tx, task, TaskState::Ready, ctx)?;
    } else {
        move_task(tx, task, TaskState::Verifying, ctx)?;
        move_task(tx, task, TaskState::Accepted, ctx)?;
    }
    if approval.decision == SpecDecision::ApproveAndBuild {
        super::modes::intent(tx, &command.run_id, &command.command_id, Mode::Build, ctx)?;
    }
    let mut claims = Claims::load(tx, &command.event.project_id, &command.run_id)?;
    claims.set(&need::run_source(RunReason::SpecApproval), &[]);
    let changed = claims.changed();
    let reasons = claims.reasons();
    let mut events = claims.store(tx)?;
    events.push(NewEvent{intent:Intent::Progress,project_id:command.event.project_id.clone(),run_id:Some(command.run_id.clone()),event_id:child_id(&command.command_id,"draft/answered"),kind:"question.answered".into(),payload:json!({"question_id":q.question_id,"reason":"spec_approval","subject":"run","exit":"draft","task_id":task,"answer":approval.note.as_deref().unwrap_or_default(),"note":approval.note,"choice_id":if approval.decision==SpecDecision::Reject{"reject"}else{"approve"},"decision":approval.decision,"command_id":command.command_id})});
    let mut follow = blockers::follow(
        tx,
        &command.event.project_id,
        &command.run_id,
        changed,
        &reasons,
        &crate::time::rfc3339_utc(ctx.now()),
        "draft approval answered",
        ctx,
    )?;
    events.append(&mut follow.events);
    follow.events = events;
    Ok(follow)
}

pub(super) fn retry(
    tx: &Transaction<'_>,
    a: &super::questions::Answer<'_>,
    q: &super::questions::Question,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let task = q.anchor["task_id"]
        .as_str()
        .ok_or_else(|| PersistError::IntegrityViolation("draft question has no task".into()))?;
    move_task(tx, task, TaskState::Ready, ctx)?;
    writer::write(
        tx,
        &Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id: a.project_id.into(),
                run_id: Some(a.run_id.into()),
                event_id: child_id(a.command_id, "draft/note"),
                kind: "spec.draft_note".into(),
                payload: json!({"task_id":task,"note":a.answer}),
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

/// Durable source snapshot used by normal completion and recovery alike.
pub(super) fn start(conn: &Connection, attempt: &str) -> Result<Option<Value>, PersistError> {
    let draft:bool=conn.query_row("SELECT t.kind='spec_draft' FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE a.attempt_id=?1",[attempt],|r|r.get(0)).map_err(sqlite_error)?;
    if !draft {
        return Ok(None);
    };
    let payload: Option<String> = conn
        .query_row(
            "SELECT e.payload_json FROM events e JOIN attempts a ON a.attempt_id=json_extract(e.payload_json,'$.attempt_id') \
             WHERE e.kind='attempt.workspace_start' AND a.attempt_id=?1 \
             ORDER BY e.seq LIMIT 1",
            [attempt],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(Some(
        payload
            .map(|s| serde_json::from_str(&s))
            .transpose()
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?
            .unwrap_or(Value::Null),
    ))
}
