//! CHANGE and CONTROL belong to the runtime, independent of agent availability.
use super::*;
use crate::persistence::commands::{self, RunTarget, SubmitCommand};
use crate::persistence::{OutboxRow, SpecAction, SpecFiles, SpecRequest};
use crate::protocol::RunState;

/// Propose once, retaining the exact workspace snapshot until publication succeeds.
#[derive(Debug, Clone)]
pub enum ChangeInput {
    Prepare {
        row: OutboxRow,
        request: Option<SpecRequest>,
    },
    Published {
        row: OutboxRow,
    },
}

#[derive(Debug, Default)]
pub struct ChangedInput {
    pub publication: Option<SpecFiles>,
    pub reply: Option<String>,
}

pub(in crate::persistence) fn apply_change(
    conn: &mut Connection,
    job: ChangeInput,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<ChangedInput, PersistError> {
    let (claimed, request, published) = match job {
        ChangeInput::Prepare { row, request } => (row, request, false),
        ChangeInput::Published { row } => (row, None, true),
    };
    let tx = writer::begin(conn, ctx)?;
    let (run, role, epoch, raw): (String, String, i64, String) = tx
        .query_row(
            "SELECT run_id,recipient_role,role_epoch,payload_json FROM outbox WHERE message_id=?1",
            params![claimed.message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(sqlite_error)?;
    if claimed.run_id.as_deref() != Some(&run)
        || claimed.recipient_role != role
        || claimed.role_epoch != epoch
        || serde_json::from_str::<Value>(&raw).map_err(|e| PersistError::Storage(e.to_string()))?
            != claimed.payload
        || role != "runtime"
        || epoch != 0
        || claimed.payload["notice"] != "input"
        || claimed.payload["kind"] != "CHANGE"
    {
        return Err(PersistError::KeyReuse(claimed.message_id));
    }
    let mut row = read_input(&tx, &claimed.message_id)?
        .filter(|r| r.run_id == run && r.kind == "CHANGE")
        .ok_or_else(|| PersistError::IntegrityViolation("missing CHANGE input".into()))?;
    let stored: Option<String> = tx
        .query_row(
            "SELECT result_json FROM receipts WHERE consumer='runtime' AND message_id=?1",
            params![row.input_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(reply) = stored {
        check_receipt(&tx, &row.input_id, &reply)?;
        return Ok(ChangedInput {
            reply: Some(reply),
            ..Default::default()
        });
    }
    if matches!(row.state, InputState::Applied | InputState::Rejected) {
        let reply = receipt(&tx, &row, row.payload["reason"].as_str())?;
        writer::commit(tx, ctx)?;
        return Ok(ChangedInput {
            reply: Some(reply),
            ..Default::default()
        });
    }
    super::super::hold::writable(&tx, &run)?;
    if row.state == InputState::Queued {
        let request = request.ok_or_else(|| {
            PersistError::BadRequest("CHANGE requires a workspace snapshot".into())
        })?;
        let SpecAction::Propose(proposal) = &request.action else {
            return Err(PersistError::BadRequest(
                "CHANGE only proposes, never approves".into(),
            ));
        };
        if row.payload["text"] != proposal.reason {
            return Err(PersistError::BadRequest(
                "CHANGE reason must be its input text".into(),
            ));
        }
        let payload = json!({"reason":proposal.reason,"spec_sha256":proposal.spec_sha256});
        let command = command(
            &tx,
            &row,
            "ProposeSpecChange",
            payload,
            RunTarget::Specification(Box::new(request.clone())),
            "spec.proposed",
        )?;
        match commands::submit_in(&tx, &command, ctx) {
            Ok(_) => {}
            Err(error) => {
                let reason = match &error {
                    PersistError::SpecConflict(_) => "SPEC_CONFLICT",
                    PersistError::SpecInvalid(error) => {
                        super::super::spec::failed(&tx, &command, error, ctx)?;
                        "SPEC_INVALID"
                    }
                    PersistError::BadRequest(_) => "BAD_REQUEST",
                    _ => return Err(error),
                };
                change(&tx, &row, InputState::Rejected, None, Some(reason), ctx)?;
                let reply = receipt(&tx, &row, Some(reason))?;
                writer::commit(tx, ctx)?;
                return Ok(ChangedInput {
                    reply: Some(reply),
                    ..Default::default()
                });
            }
        }
        let files = request.files.map_err(PersistError::SpecInvalid)?;
        row.payload["spec_files"] =
            serde_json::to_value(files).map_err(|e| PersistError::Storage(e.to_string()))?;
        row.payload["command_id"] = json!(command.command_id);
        let cr = child_id(&command.command_id, "cr");
        let cr: Option<String> = tx
            .query_row(
                "SELECT cr_id FROM change_requests WHERE cr_id=?1",
                params![cr],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        row.payload["cr_id"] = json!(cr);
        change(&tx, &row, InputState::Delivered, None, None, ctx)?;
        row.state = InputState::Delivered;
    }
    let done = if published {
        change(&tx, &row, InputState::Applied, None, None, ctx)?;
        ChangedInput {
            reply: Some(receipt(&tx, &row, None)?),
            ..Default::default()
        }
    } else {
        let files = serde_json::from_value(row.payload["spec_files"].clone()).map_err(|e| {
            PersistError::IntegrityViolation(format!("CHANGE publication snapshot: {e}"))
        })?;
        ChangedInput {
            publication: Some(files),
            ..Default::default()
        }
    };
    writer::commit(tx, ctx)?;
    Ok(done)
}

pub(in crate::persistence) fn control_kind(text: &str) -> Option<&'static str> {
    match text.trim().to_ascii_lowercase().as_str() {
        "暂停" | "pause" => Some("PauseRun"),
        "恢复" | "resume" => Some("ResumeRun"),
        "取消" | "cancel" => Some("CancelRun"),
        _ => None,
    }
}

pub(in crate::persistence) fn reject_control(
    tx: &Transaction<'_>,
    input: &NewInput,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<i64, PersistError> {
    let seq = enqueue_outcome(tx, input, Some("unknown_control"), ctx)?;
    let row = read_input(tx, &input.input_id)?
        .ok_or_else(|| PersistError::IntegrityViolation("missing rejected control input".into()))?;
    receipt(tx, &row, Some("unknown_control"))?;
    tx.execute(
        "UPDATE outbox SET state='ACKED' WHERE message_id=?1",
        params![input.input_id],
    )
    .map_err(sqlite_error)?;
    Ok(seq)
}

pub(in crate::persistence) fn control(
    tx: &Transaction<'_>,
    input: &str,
    ctx: &super::super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let row = read_input(tx, input)?
        .ok_or_else(|| PersistError::IntegrityViolation("missing control input".into()))?;
    let text = row.payload["text"]
        .as_str()
        .ok_or_else(|| PersistError::IntegrityViolation("control has no text".into()))?;
    let control = match control_kind(text) {
        Some("PauseRun") => Some((
            "PauseRun",
            RunTarget::To(RunState::PauseRequested),
            "run.pause_requested",
            "PAUSE_REQUESTED",
        )),
        Some("ResumeRun") => Some(("ResumeRun", RunTarget::Resume, "run.resumed", "RESUME")),
        Some("CancelRun") => Some((
            "CancelRun",
            RunTarget::To(RunState::CancelRequested),
            "run.cancel_requested",
            "CANCEL_REQUESTED",
        )),
        _ => None,
    };
    if let Some((kind, target, event, state)) = control {
        let mut command = command(tx, &row, kind, json!({"reason":text}), target, event)?;
        command.event.payload["reason"] = json!(text);
        if kind != "ResumeRun" {
            command.event.payload["state"] = json!(state);
            command.outbox = Some(NewOutbox {
                message_id: child_id(&command.command_id, "notice"),
                event: 0,
                run_id: Some(row.run_id.clone()),
                recipient_role: "commander".into(),
                role_epoch: 0,
                payload: json!({"notice":"runtime","kind":"run.control","control":state,
                "run_id":row.run_id,"command_id":command.command_id,"event_id":command.event.event_id,"reason":text}),
            });
        }
        commands::submit_in(tx, &command, ctx)?;
        let mut row = row;
        row.payload["command_id"] = json!(command.command_id);
        change(tx, &row, InputState::Delivered, None, None, ctx)?;
        row.state = InputState::Delivered;
        change(tx, &row, InputState::Applied, None, None, ctx)?;
        receipt(tx, &row, None)?;
    } else {
        return Err(PersistError::IntegrityViolation(
            "unknown control must be rejected at intake".into(),
        ));
    }
    tx.execute(
        "UPDATE outbox SET state='ACKED' WHERE message_id=?1",
        params![input],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn receipt(
    tx: &Transaction<'_>,
    row: &InputRow,
    reason: Option<&str>,
) -> Result<String, PersistError> {
    let result = json!({"message_id":row.input_id,"kind":if reason.is_some(){"rejected"}else{"ack"},"reason":reason,"input_sha256":fingerprint(row)}).to_string();
    tx.execute("INSERT INTO receipts(consumer,message_id,applied_at,result_json) VALUES('runtime',?1,?2,?3)",
        params![row.input_id,crate::time::now_rfc3339(),result]).map_err(sqlite_error)?;
    Ok(result)
}

fn command(
    tx: &Transaction<'_>,
    row: &InputRow,
    kind: &str,
    payload: Value,
    target: RunTarget,
    event: &str,
) -> Result<SubmitCommand, PersistError> {
    let run = commands::snapshot_rows(tx, &row.run_id)?
        .map(|(run, _)| run)
        .ok_or_else(|| PersistError::RunNotFound(row.run_id.clone()))?;
    let id = child_id(&row.input_id, kind);
    Ok(SubmitCommand {
        command_id: id.clone(),
        idempotency_key: id.clone(),
        kind: kind.into(),
        run_id: row.run_id.clone(),
        expected_revision: run.revision,
        payload_sha256: crate::specindex::digest(
            json!({"input_id":row.input_id,"kind":kind,"payload":payload})
                .to_string()
                .as_bytes(),
        ),
        payload,
        target,
        event: NewEvent {
            intent: Intent::Progress,
            project_id: run.project_id,
            run_id: Some(row.run_id.clone()),
            event_id: child_id(&id, "event"),
            kind: event.into(),
            payload: json!({"command_id":id,"input_id":row.input_id}),
        },
        outbox: None,
    })
}
