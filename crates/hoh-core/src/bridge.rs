//! A work attempt's file-bridge lines (docs/adr/ADR-028.md decision 2): its TaskResult and the HCP messages it sent,
//! each checked against the attempt's identity header; pure, so the settle, a reconcile and the tests read the same
//! lines the same way.

use crate::protocol::{self, Message, MessageKind, TaskResult};
use serde_json::Value;

/// The identity an attempt's prompt gave it: every output must name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    pub role_epoch: u64,
}

/// A line refused, by its position, the sha256 of its text and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub line: usize,
    pub sha256: String,
    pub reason: String,
}

/// What the lines said.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// The TaskResult of the last TaskResult-shaped line (an object with `status`), when that line is valid.
    pub task_result: Option<TaskResult>,
    /// The last TaskResult-shaped line was refused: the attempt's result is invalid, whatever came before it
    /// (docs/adr/ADR-028.md decision 2, M2T16D-359).
    pub result_invalid: bool,
    /// The valid `query` and `steer` messages, in order.
    pub messages: Vec<Message>,
    pub refused: Vec<Refused>,
}

/// Parse `lines` (non-empty bridge lines, in order) against `identity`: a line is a TaskResult (it has `status`, whatever
/// else it holds) or else an HCP message (it has `message_id`); either must hold under the protocol schema and name this attempt's run, task,
/// attempt, role and role epoch. Only `query` and `steer` messages are kept; anything else is refused, alone. The last
/// TaskResult-shaped line decides the result: refused, the result is invalid.
pub fn parse(lines: &[String], identity: &Identity) -> Parsed {
    let mut parsed = Parsed::default();
    for (n, text) in lines.iter().enumerate() {
        match line(text, identity) {
            Ok(Line::Result(result)) => {
                parsed.task_result = Some(*result);
                parsed.result_invalid = false;
            }
            Ok(Line::Message(message)) => parsed.messages.push(*message),
            Err(reason) => {
                if result_shaped(text) {
                    parsed.task_result = None;
                    parsed.result_invalid = true;
                }
                parsed.refused.push(Refused {
                    line: n,
                    sha256: crate::context::sha256_hex(text.as_bytes()),
                    reason,
                });
            }
        }
    }
    parsed
}

/// A JSON object with `status`, whatever else it holds: meant as a TaskResult, valid or not (M2T16D-504).
fn result_shaped(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .is_ok_and(|v| v.as_object().is_some_and(|o| o.contains_key("status")))
}

enum Line {
    Result(Box<TaskResult>),
    Message(Box<Message>),
}

fn line(text: &str, identity: &Identity) -> Result<Line, String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("not JSON: {e}"))?;
    let object = value.as_object().ok_or("not a JSON object")?;
    if object.contains_key("status") {
        let result: TaskResult =
            protocol::decode(&value).map_err(|e| format!("not a TaskResult: {e}"))?;
        same(
            identity,
            [
                ("run_id", &result.run_id),
                ("task_id", &result.task_id),
                ("attempt_id", &result.attempt_id),
                ("role", &result.role),
            ],
            result.role_epoch,
        )?;
        Ok(Line::Result(Box::new(result)))
    } else if object.contains_key("message_id") {
        let message: Message =
            protocol::decode(&value).map_err(|e| format!("not an HCP Message: {e}"))?;
        if !matches!(message.kind, MessageKind::Query | MessageKind::Steer) {
            return Err(format!(
                "a {:?} message is not one a role sends through its bridge",
                message.kind
            ));
        }
        same(
            identity,
            [
                ("run_id", &message.run_id),
                ("task_id", &message.task_id),
                ("attempt_id", &message.attempt_id),
                ("sender_role", &message.sender_role),
            ],
            message.role_epoch,
        )?;
        Ok(Line::Message(Box::new(message)))
    } else {
        Err("neither a TaskResult nor an HCP Message".to_owned())
    }
}

/// The four names and the role epoch a line gives, against the attempt's identity header.
fn same(identity: &Identity, named: [(&str, &String); 4], role_epoch: u64) -> Result<(), String> {
    let want = [
        &identity.run_id,
        &identity.task_id,
        &identity.attempt_id,
        &identity.role,
    ];
    for ((field, got), want) in named.into_iter().zip(want) {
        if got != want {
            return Err(format!("{field} {got:?} is not this attempt's {want:?}"));
        }
    }
    if role_epoch != identity.role_epoch {
        return Err(format!(
            "role_epoch {role_epoch} is not this attempt's {}",
            identity.role_epoch
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
