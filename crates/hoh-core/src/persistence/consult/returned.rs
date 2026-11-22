//! Consultation completion and replay share the caller's transaction and the existing input outbox.
use super::super::{
    inputs::{self, NewInput},
    writer::{Change, Commit, StateCas},
};
use super::*;
use crate::protocol::{TaskState, TaskStatus};

pub(in super::super) fn finish(
    tx: &Transaction<'_>,
    attempt: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let task: Option<String> = tx
        .query_row(
            "SELECT task_id FROM attempts WHERE attempt_id=?1",
            params![attempt],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(task) = task {
        settle(tx, &task, ctx)?;
    }
    Ok(())
}
pub(in super::super) fn sweep(
    tx: &Transaction<'_>,
    project: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tasks: Vec<String> = {
        let mut q=tx.prepare("SELECT json_extract(e.payload_json,'$.task_id') FROM events e JOIN outbox o ON o.message_id=json_extract(e.payload_json,'$.waiting_id') WHERE e.project_id=?1 AND e.kind='task.consult' AND o.state IN ('PENDING','DELIVERING') ORDER BY e.seq").map_err(sqlite_error)?;
        q.query_map(params![project], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for task in tasks {
        settle(tx, &task, ctx)?;
    }
    let pending: Vec<(String, String, String)> = {
        let mut q=tx.prepare("SELECT input_id,i.run_id,json_extract(i.payload_json,'$.consultation_task_id') FROM input_queue i JOIN runs r USING(run_id) WHERE r.project_id=?1 AND i.state IN ('QUEUED','DELIVERED') AND json_extract(i.payload_json,'$.consultation_task_id') IS NOT NULL ORDER BY i.rowid").map_err(sqlite_error)?;
        q.query_map(params![project], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for (input, run, child) in pending {
        if !parent_valid(tx, &run, &child)? {
            inputs::exhausted(tx, &input, "CONSULTATION_OBSOLETE", ctx)?;
        }
    }
    super::recovery::sweep(tx, project, ctx)
}
fn settle(tx: &Transaction<'_>, task: &str, ctx: &Ctx<'_>) -> Result<(), PersistError> {
    let raw:Option<(String,String)>=tx.query_row("SELECT project_id,payload_json FROM events WHERE kind='task.consult' AND json_extract(payload_json,'$.task_id')=?1 ORDER BY seq LIMIT 1",params![task],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(sqlite_error)?;
    let Some((project, raw)) = raw else {
        return Ok(());
    };
    let meta: Value =
        serde_json::from_str(&raw).map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
    let request: Message = serde_json::from_value(meta["source_message"].clone())
        .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
    let done = child_id(task, "consult/finished");
    if closing::exists(tx, &done)? {
        return Ok(());
    }
    let parent_current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM tasks t JOIN runs r USING(run_id) JOIN role_bindings b ON b.run_id=r.run_id AND b.role=t.role WHERE t.task_id=?1 AND t.state<>'CANCELLED' AND r.state NOT IN ('CANCEL_REQUESTED','CANCELLED','COMPLETED','FAILED') AND r.spec_id=?2 AND t.spec_id=?2 AND b.role_epoch=?3)",params![request.task_id,meta["spec_id"].as_str(),meta["parent_epoch"].as_i64()],|r|r.get(0)).map_err(sqlite_error)?;
    let (state, revision): (String, i64) = tx
        .query_row(
            "SELECT state,revision FROM tasks WHERE task_id=?1",
            params![task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let latest:Option<(String,String,i64)>=tx.query_row("SELECT attempt_id,state,role_epoch FROM attempts WHERE task_id=?1 ORDER BY rowid DESC LIMIT 1",params![task],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?;
    let mut actual = None;
    let mut reason = None;
    let mut detail = None;
    if !parent_current {
        reason = Some("obsolete_parent");
        if !matches!(state.as_str(), "CANCELLED" | "ACCEPTED") {
            writer::write(
                tx,
                &Commit {
                    cas: vec![StateCas {
                        id: task.into(),
                        expected_revision: revision,
                        change: Change::Task(TaskState::Cancelled),
                    }],
                    ..Default::default()
                },
                ctx,
            )?;
        }
    } else if state == "CANCELLED" || latest.as_ref().is_some_and(|(_, s, _)| s == "CANCELLED") {
        reason = Some("cancelled");
    } else if matches!(state.as_str(), "READY" | "PENDING")
        && let Some(stop) =
            super::super::budget_view::load(tx, &request.run_id, ctx.now(), ctx.wall())?
                .consult_stop(task)
    {
        reason = Some("budget_exhausted");
        detail = Some(stop);
        // The parent receives the Runtime diagnostic; no AgentBlocked question is fabricated.
        tx.execute(
            "UPDATE tasks SET blocked_reason='NEED_DECISION' WHERE task_id=?1",
            params![task],
        )
        .map_err(sqlite_error)?;
        writer::write(
            tx,
            &Commit {
                cas: vec![StateCas {
                    id: task.into(),
                    expected_revision: revision,
                    change: Change::Task(TaskState::Blocked),
                }],
                ..Default::default()
            },
            ctx,
        )?;
    } else if let Some((attempt, attempt_state, epoch)) = &latest {
        if attempt_state == "UNKNOWN_EFFECT" {
            reason = Some("unknown_effect");
        } else if attempt_state == "SETTLED" && state == "IMPLEMENTED" {
            let source_current:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM role_bindings WHERE run_id=?1 AND role=?2 AND role_epoch=?3)",params![request.run_id,request.recipient_role,epoch],|r|r.get(0)).map_err(sqlite_error)?;
            if !source_current {
                reason = Some("stale_child_epoch");
            } else {
                let raw: Option<String> = tx
                    .query_row(
                        "SELECT payload_json FROM events WHERE event_id=?1",
                        params![child_id(attempt, "outputs")],
                        |r| r.get(0),
                    )
                    .optional()
                    .map_err(sqlite_error)?;
                let output: Value = raw
                    .map(|s| serde_json::from_str(&s))
                    .transpose()
                    .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?
                    .unwrap_or(Value::Null);
                let lines: Vec<String> = output["lines"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect();
                let identity = crate::bridge::Identity {
                    run_id: request.run_id.clone(),
                    task_id: task.into(),
                    attempt_id: attempt.clone(),
                    role: request.recipient_role.clone(),
                    role_epoch: u64::try_from(*epoch)
                        .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?,
                };
                let parsed = crate::bridge::parse(&lines, &identity);
                actual = parsed.task_result.filter(|r| {
                    output["truncated"] != true
                        && r.candidate_id
                            .as_ref()
                            .is_none_or(|c| Some(c) == request.candidate_id.as_ref())
                });
                if actual.is_none() {
                    let reasons = vec![
                        "Consultation output needs one current, untruncated TaskResult".into(),
                    ];
                    let verdict = super::super::completion::correct_readonly_output(
                        tx, attempt, task, &reasons, ctx,
                    )?;
                    if matches!(verdict, super::super::RepairVerdict::Granted { .. }) {
                        return Ok(());
                    }
                    reason = Some("invalid_task_result");
                }
            }
        } else if state == "BLOCKED" {
            reason = Some("child_blocked");
        } else {
            return Ok(());
        }
    } else if state == "BLOCKED" {
        reason = Some("child_blocked");
    } else {
        return Ok(());
    }
    if let Some(result) = &actual {
        let why = match result.status {
            TaskStatus::Completed => None,
            TaskStatus::Failed => Some(super::super::need::WorkReason::AgentFailed),
            TaskStatus::Blocked => Some(super::super::need::WorkReason::AgentBlocked),
            TaskStatus::Incomplete => Some(super::super::need::WorkReason::AgentIncomplete),
        };
        if let Some(why) = why {
            let event = super::super::need::task_event(
                tx,
                super::super::need::TaskNeed {
                    project_id: &project,
                    run_id: &request.run_id,
                    task_id: task,
                    event_id: child_id(&done, "decision"),
                    reason: super::super::need::TaskReason::Work(why),
                    fields: json!({"attempt_id":result.attempt_id,"detail":result.summary}),
                },
            )?;
            tx.execute(
                "UPDATE tasks SET blocked_reason='NEED_DECISION' WHERE task_id=?1",
                params![task],
            )
            .map_err(sqlite_error)?;
            writer::write(
                tx,
                &Commit {
                    cas: vec![StateCas {
                        id: task.into(),
                        expected_revision: revision,
                        change: Change::Task(TaskState::Blocked),
                    }],
                    events: vec![event],
                    ..Default::default()
                },
                ctx,
            )?;
        }
    }
    super::bounds::release(tx, &request.run_id, task, &meta)?;
    let input = child_id(task, "consult/result");
    if parent_current {
        let mut payload = if let Some(result) = &actual {
            let message = Message {
                schema_version: "1.0".into(),
                message_id: input.clone(),
                run_id: request.run_id.clone(),
                task_id: task.into(),
                attempt_id: result.attempt_id.clone(),
                sender_role: result.role.clone(),
                recipient_role: request.sender_role.clone(),
                role_epoch: result.role_epoch,
                correlation_id: request.message_id.clone(),
                causation_id: Some(request.message_id.clone()),
                idempotency_key: input.clone(),
                expected_revision: revision as u64,
                spec_sha256: request.spec_sha256.clone(),
                candidate_id: result.candidate_id.clone(),
                kind: MessageKind::Result,
                reason_code: None,
                payload: serde_json::to_value(result)
                    .map_err(|e| PersistError::Storage(e.to_string()))?
                    .as_object()
                    .cloned()
                    .ok_or_else(|| {
                        PersistError::IntegrityViolation("result is not an object".into())
                    })?,
            };
            message
                .check_payload()
                .map_err(PersistError::IntegrityViolation)?;
            let source =
                serde_json::to_value(&message).map_err(|e| PersistError::Storage(e.to_string()))?;
            json!({"text":source.to_string(),"source_message":source})
        } else {
            let disposition = (reason == Some("budget_exhausted"))
                .then(|| json!({"intent":"dismiss_consult","task_id":task}));
            let action = disposition
                .as_ref()
                .map(|p| format!(" To end this stopped child, send a query with payload {p}."))
                .unwrap_or_default();
            json!({"text":format!("Consultation {task} ended without a result: {}{}{action}",reason.unwrap_or("unavailable"),detail.as_ref().map(|s|format!(" ({s})")).unwrap_or_default()),"consultation_diagnostic":{"task_id":task,"correlation_id":request.message_id,"reason":reason,"detail":detail,"origin":"runtime","disposition":disposition}})
        };
        payload["consultation_task_id"] = json!(task);
        inputs::enqueue(
            tx,
            &NewInput {
                input_id: input.clone(),
                run_id: request.run_id.clone(),
                kind: "STEER".into(),
                target_role: Some(request.sender_role.clone()),
                payload,
            },
            ctx,
        )?;
    }
    tx.execute("UPDATE outbox SET state=?2,last_error=?3,next_retry_at=NULL WHERE message_id=?1 AND state IN ('PENDING','DELIVERING')",params![meta["waiting_id"].as_str(),if parent_current {"ACKED"} else {"DEAD_LETTER"},reason]).map_err(sqlite_error)?;
    if parent_current {
        tx.execute(
            "INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES (?1,?2,?3,?4)",
            params![
                request.recipient_role,
                meta["waiting_id"].as_str(),
                crate::time::rfc3339_utc(ctx.now()),
                json!({"kind":"ack","task_id":task,"result_input_id":input}).to_string()
            ],
        )
        .map_err(sqlite_error)?;
    }
    event(
        tx,
        &project,
        &request,
        done,
        "task.consult_finished",
        json!({"task_id":task,"parent_task_id":request.task_id,"attempt_id":latest.as_ref().map(|a|&a.0),"input_id":parent_current.then_some(input),"reason":reason,"detail":detail,"status":actual.as_ref().map(|r|r.status)}),
    )?;
    Ok(())
}

/// Recheck the original task and specification at each delivery boundary, not just when the answer was enqueued.
pub(in super::super) fn input_current(
    conn: &Connection,
    row: &super::super::InputRow,
) -> Result<bool, PersistError> {
    match row.payload["consultation_task_id"].as_str() {
        Some(child) => parent_valid(conn, &row.run_id, child),
        None => Ok(true),
    }
}
pub(super) fn parent_valid(
    conn: &Connection,
    run: &str,
    child: &str,
) -> Result<bool, PersistError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM events e JOIN tasks t ON t.task_id=json_extract(e.payload_json,'$.parent_task_id') JOIN runs r ON r.run_id=t.run_id WHERE e.run_id=?1 AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=?2 AND t.state<>'CANCELLED' AND t.spec_id=r.spec_id AND r.spec_id=json_extract(e.payload_json,'$.spec_id'))",params![run,child],|r|r.get(0)).map_err(sqlite_error)
}

/// A current parent can dispose of its own Runtime-stopped child, not cancel arbitrary work.
pub(super) fn dismiss(
    tx: &Transaction<'_>,
    project: &str,
    message: &Message,
    ctx: &Ctx<'_>,
) -> Result<Value, PersistError> {
    let Some(task) = message.payload.get("task_id").and_then(Value::as_str) else {
        return Ok(rejected(
            "ILLEGAL_TRANSITION",
            json!({"cause":"consult_task_required"}),
        ));
    };
    let revision: Option<i64> = tx.query_row(
        "SELECT t.revision FROM tasks t JOIN events e ON e.run_id=t.run_id AND e.kind='task.consult'
         AND json_extract(e.payload_json,'$.task_id')=t.task_id
         WHERE t.run_id=?1 AND t.task_id=?2 AND t.state='BLOCKED' AND t.blocked_reason='NEED_DECISION'
         AND json_extract(e.payload_json,'$.parent_task_id')=?3
         AND json_extract(e.payload_json,'$.parent_role')=?4
         AND json_extract(e.payload_json,'$.parent_epoch')=?5
         AND json_extract(e.payload_json,'$.spec_id')=t.spec_id
         AND EXISTS(SELECT 1 FROM events d WHERE d.run_id=t.run_id AND d.kind='task.consult_finished'
           AND json_extract(d.payload_json,'$.task_id')=t.task_id
           AND json_extract(d.payload_json,'$.reason')='budget_exhausted')
         AND NOT EXISTS(SELECT 1 FROM attempts a WHERE a.task_id=t.task_id
           AND a.state IN ('PREPARED','STARTING','RUNNING','UNKNOWN_EFFECT'))",
        params![message.run_id,task,message.task_id,message.sender_role,i64::try_from(message.role_epoch).ok()], |r| r.get(0),
    ).optional().map_err(sqlite_error)?;
    let Some(revision) = revision else {
        return Ok(rejected(
            "ILLEGAL_TRANSITION",
            json!({"cause":"not_owned_stopped_consult","task_id":task}),
        ));
    };
    tx.execute(
        "UPDATE tasks SET blocked_reason=NULL WHERE task_id=?1",
        params![task],
    )
    .map_err(sqlite_error)?;
    writer::write(
        tx,
        &Commit {
            cas: vec![StateCas {
                id: task.into(),
                expected_revision: revision,
                change: Change::Task(TaskState::Cancelled),
            }],
            ..Default::default()
        },
        ctx,
    )?;
    event(
        tx,
        project,
        message,
        child_id(task, "dismissed"),
        "task.consult_dismissed",
        json!({"task_id":task,"parent_task_id":message.task_id,"message_id":message.message_id}),
    )?;
    Ok(json!({"status":"accepted","task_id":task,"state":"CANCELLED"}))
}
