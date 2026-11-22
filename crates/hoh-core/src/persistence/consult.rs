//! Scoped consultation transactions (REQ-023, ADR-037).
mod bounds;
mod children;
mod listing;
mod recovery;
mod returned;
pub(super) use bounds::{accrue_wall, exclude_unused_holds, stop as budget_stop, unreserved_wall};
pub use listing::TasksPage;
pub(super) use listing::page;
pub(super) use returned::{finish, input_current, sweep};

use super::writer::{self, Ctx, Intent, NewEvent};
use super::{PersistError, closing, sqlite_error};
use crate::ids::child_id;
use crate::protocol::{Message, MessageKind};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

pub(super) fn event(
    tx: &Transaction<'_>,
    project: &str,
    message: &Message,
    id: String,
    kind: &str,
    payload: Value,
) -> Result<(), PersistError> {
    if !closing::exists(tx, &id)? {
        writer::insert_event(
            tx,
            &NewEvent {
                project_id: project.into(),
                run_id: Some(message.run_id.clone()),
                event_id: id,
                kind: kind.into(),
                payload,
                intent: Intent::Progress,
            },
        )?;
    }
    Ok(())
}
fn rejected(reason: &str, details: Value) -> Value {
    json!({"status":"rejected","reason_code":reason,"details":details})
}

/// Reject stale sources before any opinion, child task or escalation can be written.
fn source(tx: &Transaction<'_>, message: &Message) -> Result<Option<Value>, PersistError> {
    let epoch: Option<i64> = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id=?1 AND role=?2",
            params![message.run_id, message.sender_role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if epoch.is_none() || epoch != i64::try_from(message.role_epoch).ok() {
        return Ok(Some(rejected(
            "STALE_ROLE_EPOCH",
            json!({"role":message.sender_role}),
        )));
    }
    let current:Option<(String,i64,String)>=tx.query_row("SELECT a.state,t.revision,s.sha256 FROM attempts a JOIN tasks t USING(task_id) JOIN runs r USING(run_id) JOIN specs s ON s.spec_id=r.spec_id WHERE a.attempt_id=?1 AND t.task_id=?2 AND t.run_id=?3 AND t.role=?4 AND a.role_epoch=?5 AND t.state='RUNNING' AND a.cancel_requested=0 AND t.spec_id=r.spec_id",params![message.attempt_id,message.task_id,message.run_id,message.sender_role,epoch],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?;
    let Some((state, revision, spec)) = current else {
        return Ok(Some(rejected(
            "ILLEGAL_TRANSITION",
            json!({"cause":"request_source_mismatch"}),
        )));
    };
    if state != "RUNNING" {
        return Ok(Some(rejected(
            "ILLEGAL_TRANSITION",
            json!({"cause":"attempt_not_running"}),
        )));
    }
    if Some(revision) != i64::try_from(message.expected_revision).ok() {
        return Ok(Some(rejected(
            "STALE_REVISION",
            json!({"task_id":message.task_id,"revision":revision}),
        )));
    }
    if spec != message.spec_sha256 {
        return Ok(Some(rejected("SPEC_MISMATCH", json!({"spec_sha256":spec}))));
    }
    if let Some(candidate) = &message.candidate_id
        && super::outbox::current_candidate(tx, &message.run_id)?
            .as_ref()
            .map(|c| &c.id)
            != Some(candidate)
    {
        return Ok(Some(rejected(
            "CANDIDATE_MISMATCH",
            json!({"candidate_id":candidate}),
        )));
    }
    Ok(None)
}

pub(super) fn request(
    conn: &mut Connection,
    message: &Message,
    ctx: &Ctx<'_>,
) -> Result<Option<Value>, PersistError> {
    let intent = message
        .payload
        .get("intent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let major = intent == "apply"
        && matches!(
            message
                .payload
                .get("decision_class")
                .and_then(Value::as_str),
            Some("spec" | "adr" | "public_api" | "gate")
        );
    if !matches!(message.kind, MessageKind::Query | MessageKind::Steer)
        || (!major
            && (message.kind != MessageKind::Query
                || !matches!(intent, "opinion" | "conclude" | "consult")))
    {
        return Ok(None);
    }
    let tx = writer::begin(conn, ctx)?;
    let key = child_id(
        &message.attempt_id,
        &format!("consult/{}", message.message_id),
    );
    let submitted =
        serde_json::to_value(message).map_err(|e| PersistError::Storage(e.to_string()))?;
    let previous: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE event_id=?1",
            params![key],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(previous) = previous {
        let previous: Value = serde_json::from_str(&previous)
            .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
        return if previous["source_message"] == submitted {
            Ok(Some(previous["reply"].clone()))
        } else {
            Err(PersistError::KeyReuse(message.message_id.clone()))
        };
    }
    super::hold::writable(&tx, &message.run_id)?;
    let project: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id=?1",
            params![message.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let reply = if let Some(reply) = source(&tx, message)? {
        reply
    } else if intent == "consult" {
        let edges = wait_for(&tx, &message.run_id)?;
        if let Some(members) = cycle(&edges, (&message.sender_role, &message.recipient_role)) {
            rejected("CONSULT_CYCLE", json!({"members":members}))
        } else {
            children::create(&tx, &project, message, &key, ctx)?
        }
    } else if major {
        major_decision(&tx, &project, message)?
    } else {
        discussion(&tx, &project, message, &key, intent)?
    };
    let mut payload = json!({"message_id":message.message_id,"attempt_id":message.attempt_id,"source_message":submitted,"reply":reply});
    if reply["status"] == "rejected" {
        payload["reason_code"] = reply["reason_code"].clone();
        payload["details"] = reply["details"].clone();
    }
    event(
        &tx,
        &project,
        message,
        key,
        if reply["status"] == "rejected" {
            "request.rejected"
        } else {
            "consult.request"
        },
        payload,
    )?;
    writer::commit(tx, ctx)?;
    Ok(Some(reply))
}
fn discussion(
    tx: &Transaction<'_>,
    project: &str,
    message: &Message,
    key: &str,
    intent: &str,
) -> Result<Value, PersistError> {
    let Some(controversy) = message
        .payload
        .get("controversy_id")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(rejected(
            "ILLEGAL_TRANSITION",
            json!({"cause":"controversy_id_required"}),
        ));
    };
    if intent == "conclude" {
        event(
            tx,
            project,
            message,
            child_id(key, "conclusion"),
            "discussion.conclusion",
            json!({"controversy_id":controversy,"message_id":message.message_id,"conclusion":message.payload}),
        )?;
        return Ok(json!({"status":"accepted"}));
    }
    let max:i64=tx.query_row("SELECT coalesce(json_extract(config_snapshot_json,'$.supervision.max_discussion_rounds'),2) FROM runs WHERE run_id=?1",params![message.run_id],|r|r.get(0)).map_err(sqlite_error)?;
    if !(1..=5).contains(&max) {
        return Err(PersistError::IntegrityViolation(
            "invalid discussion limit in run snapshot".into(),
        ));
    }
    let mut q=tx.prepare("SELECT event_id,json_extract(payload_json,'$.evidence_sha') FROM events WHERE run_id=?1 AND kind='discussion.opinion' AND json_extract(payload_json,'$.controversy_id')=?2 ORDER BY seq").map_err(sqlite_error)?;
    let opinions = q
        .query_map(params![message.run_id, controversy], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let evidence = message
        .payload
        .get("evidence_sha")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let repeated =
        evidence.is_some_and(|e| opinions.iter().any(|(_, old)| old.as_deref() == Some(e)));
    // The single escalation closes the opinion phase, including an early evidence stop.
    let escalation_id =
        super::need::discussion_limit(project, &message.run_id, controversy, &[], "").event_id;
    let stopped: Option<String> = tx
        .query_row(
            "SELECT json_extract(payload_json,'$.cause') FROM events WHERE event_id=?1",
            params![escalation_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(cause) = stopped {
        return Ok(rejected(
            "DISCUSSION_LIMIT",
            json!({"cause":cause,"controversy_id":controversy,
            "opinions":opinions.iter().map(|(id,_)|id).collect::<Vec<_>>(),"max_discussion_rounds":max}),
        ));
    }
    if opinions.len() >= max as usize || repeated {
        let cause = if repeated {
            "no_new_evidence"
        } else {
            "max_discussion_rounds"
        };
        let refs = opinions.into_iter().map(|(id, _)| id).collect::<Vec<_>>();
        let escalation =
            super::need::discussion_limit(project, &message.run_id, controversy, &refs, cause);
        if !closing::exists(tx, &escalation.event_id)? {
            writer::insert_event(tx, &escalation)?;
        }
        return Ok(rejected(
            "DISCUSSION_LIMIT",
            json!({"cause":cause,"controversy_id":controversy,"opinions":refs,"max_discussion_rounds":max}),
        ));
    }
    let id = child_id(key, "opinion");
    event(
        tx,
        project,
        message,
        id.clone(),
        "discussion.opinion",
        json!({"controversy_id":controversy,"evidence_sha":evidence,"message_id":message.message_id,"opinion":message.payload}),
    )?;
    Ok(json!({"status":"accepted","event_id":id,"round":opinions.len()+1}))
}

fn major_decision(
    tx: &Transaction<'_>,
    project: &str,
    message: &Message,
) -> Result<Value, PersistError> {
    let class = &message.payload["decision_class"];
    let statement = ["statement", "text", "summary"]
        .iter()
        .find_map(|key| message.payload.get(*key).and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty());
    let Some(statement) = statement else {
        return Ok(rejected(
            "ILLEGAL_TRANSITION",
            json!({"decision_class":class,"cause":"statement_required"}),
        ));
    };
    let escalation = super::need::major_decision(project, &message.run_id, class, statement);
    if !closing::exists(tx, &escalation.event_id)? {
        writer::insert_event(tx, &escalation)?;
    }
    Ok(rejected(
        "ILLEGAL_TRANSITION",
        json!({"decision_class":class,"cause":"commander_decision_required"}),
    ))
}

/// The shared unfinished role-request view, read inside the same transaction as a new edge.
pub(super) fn wait_for(
    conn: &Connection,
    run: &str,
) -> Result<Vec<(String, String)>, PersistError> {
    let mut q=conn.prepare("SELECT DISTINCT json_extract(payload_json,'$.sender_role'),recipient_role FROM outbox WHERE run_id=?1 AND state IN ('PENDING','DELIVERING') AND json_extract(payload_json,'$.kind') IN ('query','steer') AND json_extract(payload_json,'$.sender_role') IS NOT NULL ORDER BY 1,2").map_err(sqlite_error)?;
    q.query_map(params![run], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}
fn cycle(edges: &[(String, String)], proposed: (&str, &str)) -> Option<Vec<String>> {
    use std::collections::{BTreeSet, VecDeque};
    let (sender, recipient) = proposed;
    if sender == recipient {
        return Some(vec![sender.into()]);
    }
    let mut queue = VecDeque::from([vec![sender.to_owned(), recipient.to_owned()]]);
    let mut visited = BTreeSet::new();
    while let Some(path) = queue.pop_front() {
        let last = path.last()?;
        if !visited.insert(last.clone()) {
            continue;
        }
        for (_, to) in edges.iter().filter(|(from, _)| from == last) {
            if to == sender {
                return Some(path);
            }
            let mut next = path.clone();
            next.push(to.clone());
            queue.push_back(next);
        }
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_023_02_cycle_is_ordered_and_ignores_unrelated_cycles() {
        let edges =
            [("b", "c"), ("c", "a"), ("x", "y"), ("y", "x")].map(|(a, b)| (a.into(), b.into()));
        assert_eq!(
            cycle(&edges, ("a", "b")),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(cycle(&edges, ("d", "x")), None);
        assert_eq!(cycle(&edges, ("a", "a")), Some(vec!["a".into()]));
        assert_eq!(cycle(&[], ("a", "b")), None);
    }
}
