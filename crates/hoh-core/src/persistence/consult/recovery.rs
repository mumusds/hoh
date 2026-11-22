//! Carry a durable result into one later parent attempt, without retrying an uncertain native steer.
use super::super::inputs::{self, NewInput};
use super::*;

pub(super) fn sweep(
    tx: &Transaction<'_>,
    project: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let originals: Vec<String> = {
        let mut q=tx.prepare(
            "SELECT i.input_id FROM input_queue i JOIN runs r USING(run_id) JOIN attempts a ON a.attempt_id=json_extract(i.payload_json,'$.attempt_id') \
             WHERE r.project_id=?1 AND r.state IN ('RUNNING','VERIFYING','BLOCKED','WAITING_EXTERNAL') \
             AND i.state='REJECTED' AND json_extract(i.payload_json,'$.reason') IN ('UNKNOWN_DELIVERY','attempt_ended') \
             AND json_extract(i.payload_json,'$.consultation_task_id') IS NOT NULL \
             AND json_extract(i.payload_json,'$.consultation_recovery_of') IS NULL \
             AND json_extract(i.payload_json,'$.source_message.kind')='result' \
             AND a.state IN ('FAILED','CANCELLED','UNKNOWN_EFFECT') ORDER BY i.rowid"
        ).map_err(sqlite_error)?;
        q.query_map(params![project], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    for id in originals {
        let old = inputs::read_input(tx, &id)?
            .ok_or_else(|| PersistError::IntegrityViolation("consult result disappeared".into()))?;
        let child = old.payload["consultation_task_id"]
            .as_str()
            .ok_or_else(|| PersistError::IntegrityViolation("missing consultation task".into()))?;
        let done = child_id(child, "consult/recovered");
        if closing::exists(tx, &done)? || !super::returned::parent_valid(tx, &old.run_id, child)? {
            continue;
        }
        let parent:Option<String>=tx.query_row(
            "SELECT t.task_id FROM events e JOIN tasks t ON t.task_id=json_extract(e.payload_json,'$.parent_task_id') \
             JOIN role_bindings b ON b.run_id=t.run_id AND b.role=t.role \
             WHERE e.run_id=?1 AND e.kind='task.consult' AND json_extract(e.payload_json,'$.task_id')=?2 \
             AND b.role_epoch=?3 AND b.role_epoch=json_extract(e.payload_json,'$.parent_epoch') \
             AND t.task_id=(SELECT task_id FROM attempts WHERE attempt_id=?4)",
            params![old.run_id,child,old.payload["role_epoch"].as_i64(),old.payload["attempt_id"].as_str()],|r|r.get(0),
        ).optional().map_err(sqlite_error)?;
        let Some(parent) = parent else {
            continue;
        };
        // Reconstruct the content from the child's persisted output, not an instruction to rerun its work.
        let source: Message = serde_json::from_value(old.payload["source_message"].clone())
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        let raw: String = tx
            .query_row(
                "SELECT payload_json FROM events WHERE event_id=?1",
                params![child_id(&source.attempt_id, "outputs")],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let output: Value = serde_json::from_str(&raw)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        let lines: Vec<String> = output["lines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let parsed = crate::bridge::parse(
            &lines,
            &crate::bridge::Identity {
                run_id: source.run_id.clone(),
                task_id: child.into(),
                attempt_id: source.attempt_id.clone(),
                role: source.sender_role.clone(),
                role_epoch: source.role_epoch,
            },
        );
        let result = parsed
            .task_result
            .filter(|_| output["truncated"] != true)
            .ok_or_else(|| {
                PersistError::IntegrityViolation("consultation recovery lost its TaskResult".into())
            })?;
        let result =
            serde_json::to_value(result).map_err(|e| PersistError::Storage(e.to_string()))?;
        if result != Value::Object(source.payload.clone()) {
            return Err(PersistError::IntegrityViolation(
                "consultation result differs from child outputs".into(),
            ));
        }
        let input = child_id(child, "consult/result/recovered");
        let source =
            serde_json::to_value(source).map_err(|e| PersistError::Storage(e.to_string()))?;
        inputs::enqueue(
            tx,
            &NewInput {
                input_id: input.clone(),
                run_id: old.run_id.clone(),
                kind: "STEER".into(),
                target_role: old.target_role.clone(),
                payload: json!({"text":source.to_string(),"source_message":source,"consultation_task_id":child,
                "consultation_parent_task_id":parent,"consultation_recovery_of":id,"boundary":"next_attempt"}),
            },
            ctx,
        )?;
        writer::insert_event(
            tx,
            &NewEvent {
                project_id: project.into(),
                run_id: Some(old.run_id),
                event_id: done,
                kind: "task.consult_recovered".into(),
                intent: Intent::Progress,
                payload: json!({"task_id":child,"parent_task_id":parent,"input_id":input,"original_input_id":id,
                "previous_attempt_id":old.payload["attempt_id"],"reason":old.payload["reason"],"boundary":"next_attempt"}),
            },
        )?;
    }
    Ok(())
}
