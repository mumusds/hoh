//! Read-only consultation tasks and their bounds are created with the waiting edge in one transaction.
use super::super::writer::{Commit, NewOutbox};
use super::*;

pub(super) fn create(
    tx: &Transaction<'_>,
    project: &str,
    message: &Message,
    key: &str,
    ctx: &Ctx<'_>,
) -> Result<Value, PersistError> {
    let refuse = |cause: &str| rejected("ILLEGAL_TRANSITION", json!({"cause":cause}));
    let Some(question) = message
        .payload
        .get("question")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(refuse("question_required"));
    };
    let target:Option<i64>=tx.query_row("SELECT b.role_epoch FROM role_bindings b JOIN sessions s USING(session_id) WHERE b.run_id=?1 AND b.role=?2 AND s.role=b.role",params![message.run_id,message.recipient_role],|r|r.get(0)).optional().map_err(sqlite_error)?;
    let Some(target_epoch) = target else {
        return Ok(refuse("recipient_unavailable"));
    };
    if matches!(message.recipient_role.as_str(), "runtime" | "commander") {
        return Ok(refuse("recipient_is_not_a_worker"));
    }
    let (spec, run_deadline): (String, Option<String>) = tx
        .query_row(
            "SELECT spec_id,deadline_at FROM runs WHERE run_id=?1",
            params![message.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let deadline = match message.payload.get("deadline") {
        Some(Value::String(s)) => Some(s.clone()),
        None => run_deadline,
        Some(_) => return Ok(refuse("invalid_deadline")),
    };
    let Some(deadline) = deadline else {
        return Ok(refuse("deadline_required"));
    };
    if crate::time::parse_rfc3339(&deadline).is_none_or(|at| at <= ctx.wall()) {
        return Ok(refuse("invalid_deadline"));
    }
    let mut budget = serde_json::Map::new();
    if let Some(explicit) = message.payload.get("budget") {
        let Some(limits) = explicit.as_object() else {
            return Ok(refuse("invalid_budget"));
        };
        for (kind, value) in limits {
            if !matches!(
                kind.as_str(),
                "wall_seconds" | "iterations" | "attempts" | "tokens" | "usd"
            ) || value.as_f64().is_none_or(|v| !v.is_finite() || v <= 0.0)
            {
                return Ok(refuse("invalid_budget"));
            }
            budget.insert(kind.clone(), value.clone());
        }
    } else {
        // Read-only consultations consume work, never candidate/verification iterations.
        let mut q=tx.prepare("SELECT kind,max(0,limit_value-used_value-reserved_value)/10.0 FROM budgets WHERE scope_kind='run' AND scope_id=?1 AND kind IN ('wall_seconds','attempts','tokens','usd') AND limit_value IS NOT NULL ORDER BY kind").map_err(sqlite_error)?;
        for row in q
            .query_map(params![message.run_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
            })
            .map_err(sqlite_error)?
        {
            let (kind, value) = row.map_err(sqlite_error)?;
            if value > 0.0 {
                budget.insert(kind, json!(value));
            }
        }
    }
    if budget.is_empty() {
        return Ok(refuse("budget_required"));
    }
    let reserved = if message.payload.contains_key("budget") {
        json!({})
    } else {
        for (kind, limit) in &budget {
            tx.execute("UPDATE budgets SET reserved_value=reserved_value+?1 WHERE scope_kind='run' AND scope_id=?2 AND kind=?3",params![limit.as_f64(),message.run_id,kind]).map_err(sqlite_error)?;
        }
        json!(budget)
    };
    let task = child_id(key, "task");
    let waiting = child_id(key, "waiting");
    tx.execute("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,information_gain,layer) SELECT ?1,?2,?3,'READY',?4,0,coalesce(max(created_seq),0)+1,1,1 FROM tasks WHERE run_id=?2",params![task,message.run_id,spec,message.recipient_role]).map_err(sqlite_error)?;
    for (kind, limit) in &budget {
        tx.execute(
            "INSERT INTO budgets(scope_kind,scope_id,kind,limit_value,price_known) VALUES ('task',?1,?2,?3,coalesce((SELECT price_known FROM budgets WHERE scope_kind='run' AND scope_id=?4 AND kind=?2),?5))",
            params![task, kind, limit.as_f64(),message.run_id,kind!="usd"],
        )
        .map_err(sqlite_error)?;
    }
    let source = serde_json::to_value(message).map_err(|e| PersistError::Storage(e.to_string()))?;
    let mut queued = source.clone();
    queued["notice"] = json!("consult");
    queued["child_task_id"] = json!(task);
    writer::write(
        tx,
        &Commit {
            events: vec![NewEvent {
                project_id: project.into(),
                run_id: Some(message.run_id.clone()),
                event_id: child_id(key, "created"),
                kind: "task.consult".into(),
                intent: Intent::Progress,
                payload: json!({"task_id":task,"parent_task_id":message.task_id,"parent_role":message.sender_role,"parent_epoch":message.role_epoch,"recipient_epoch":target_epoch,"spec_id":spec,"role":message.recipient_role,"question":question,"budget":budget,"reserved_budget":reserved,"deadline":deadline,"message_id":message.message_id,"waiting_id":waiting,"source_message":source}),
            }],
            outbox: vec![NewOutbox {
                message_id: waiting,
                event: 0,
                run_id: Some(message.run_id.clone()),
                recipient_role: message.recipient_role.clone(),
                role_epoch: target_epoch,
                payload: queued,
            }],
            ..Default::default()
        },
        ctx,
    )?;
    Ok(json!({"status":"queued","task_id":task}))
}
