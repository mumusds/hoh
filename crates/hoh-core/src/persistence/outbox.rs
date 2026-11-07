//! The outbox and its receipts (REQ-022, REQ-062; docs/adr/ADR-018.md): taking messages for delivery, settling a
//! delivery, consuming a message once behind its fences, and recording a message that is not one.

use super::attempts::{self, NewAttempt};
use super::decide::child_id;
use super::writer::{self, Commit, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::protocol::{self, HandoffKind, Message, MessageKind, RejectionReason};
use crate::time::rfc3339_utc;
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use std::time::{Duration, SystemTime};

/// An outbox row taken for delivery.
#[derive(Debug, Clone, PartialEq)]
pub struct OutboxRow {
    pub message_id: String,
    pub run_id: Option<String>,
    pub recipient_role: String,
    pub role_epoch: i64,
    pub payload: Value,
    /// Deliveries so far, this one included.
    pub attempt_count: i64,
    /// When this delivery counts as lost and the row may be taken again.
    pub next_retry_at: Option<String>,
}

/// How a delivery ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    /// The consumer answered (an ack or a rejection: both are delivered).
    Delivered,
    /// No answer reached the sender (a lost ACK, a consumer down): tried again until the limit.
    Failed(String),
    /// The row holds no valid message: dead at once.
    Invalid(String),
}

/// What a consumer does with a message that passes its fences, in the same transaction as its receipt.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConsumeEffect {
    pub cas: Vec<StateCas>,
    pub events: Vec<NewEvent>,
    /// New attempts (row and `attempt.created` event), after the events.
    pub attempts: Vec<NewAttempt>,
}

/// The answer to a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consumed {
    /// The reply exactly as stored with the receipt: a kind=ack or kind=rejected Message.
    pub result_json: String,
    /// The receipt existed: nothing was written now.
    pub replayed: bool,
    /// The reason, when the message was refused.
    pub rejected: Option<RejectionReason>,
}

/// The `k`th delivery's wait before the row may be taken again: 1, 2, 4 … 60 s.
pub fn backoff(k: i64) -> Duration {
    let exponent = u32::try_from((k - 1).clamp(0, 6)).unwrap_or(6);
    Duration::from_secs(2_u64.pow(exponent).min(60))
}

/// Take up to `limit` messages for `roles` that are due at `now`, oldest event first; each counts one more delivery
/// and may be taken again after its backoff if it is not settled by then.
pub(super) fn claim(
    conn: &mut Connection,
    now: SystemTime,
    roles: &[String],
    limit: usize,
) -> Result<Vec<OutboxRow>, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let stamp = rfc3339_utc(now);
    let mut rows = {
        let mut statement = tx
            .prepare(
                "SELECT message_id, run_id, recipient_role, role_epoch, payload_json, attempt_count FROM outbox \
                 WHERE ((state = 'PENDING' AND (next_retry_at IS NULL OR next_retry_at <= ?1)) \
                        OR (state = 'DELIVERING' AND next_retry_at <= ?1)) \
                   AND recipient_role IN (SELECT value FROM json_each(?2)) \
                 ORDER BY event_seq, message_id LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let found = statement
            .query_map(
                params![
                    stamp,
                    json!(roles).to_string(),
                    i64::try_from(limit).unwrap_or(i64::MAX)
                ],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<String>>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, i64>(5)?,
                    ))
                },
            )
            .map_err(sqlite_error)?;
        found
            .map(|row| {
                let (message_id, run_id, recipient_role, role_epoch, payload, count) =
                    row.map_err(sqlite_error)?;
                let payload = serde_json::from_str(&payload).map_err(|e| {
                    PersistError::IntegrityViolation(format!("outbox {message_id}: {e}"))
                })?;
                Ok(OutboxRow {
                    message_id,
                    run_id,
                    recipient_role,
                    role_epoch,
                    payload,
                    attempt_count: count,
                    next_retry_at: None,
                })
            })
            .collect::<Result<Vec<_>, PersistError>>()?
    };
    for row in &mut rows {
        row.attempt_count += 1;
        let retry = rfc3339_utc(now + backoff(row.attempt_count));
        tx.execute(
            "UPDATE outbox SET state = 'DELIVERING', attempt_count = ?1, next_retry_at = ?2 WHERE message_id = ?3",
            params![row.attempt_count, retry, row.message_id],
        )
        .map_err(sqlite_error)?;
        row.next_retry_at = Some(retry);
    }
    tx.commit().map_err(sqlite_error)?;
    Ok(rows)
}

/// Settle a delivery: ACKED, back to PENDING (its next_retry_at kept), or DEAD_LETTER with its event once
/// `max_attempts` deliveries failed or the row holds no message. Rows are never deleted.
pub(super) fn settle(
    conn: &mut Connection,
    message_id: &str,
    settlement: &Settlement,
    max_attempts: i64,
    after_events: &dyn Fn(),
) -> Result<(), PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let row: Option<(i64, String, Option<String>, String)> = tx
        .query_row(
            "SELECT attempt_count, state, run_id, recipient_role FROM outbox WHERE message_id = ?1",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((count, state, run_id, recipient)) = row else {
        return Err(PersistError::IntegrityViolation(format!(
            "no outbox message {message_id}"
        )));
    };
    if state != "DELIVERING" {
        // Settled already (a late answer to a delivery taken again): the first settlement stands.
        return Ok(());
    }
    let dead = match settlement {
        Settlement::Delivered => {
            tx.execute(
                "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL, last_error = NULL WHERE message_id = ?1",
                params![message_id],
            )
            .map_err(sqlite_error)?;
            None
        }
        Settlement::Failed(error) if count < max_attempts => {
            tx.execute(
                "UPDATE outbox SET state = 'PENDING', last_error = ?1 WHERE message_id = ?2",
                params![error, message_id],
            )
            .map_err(sqlite_error)?;
            None
        }
        Settlement::Failed(error) | Settlement::Invalid(error) => Some(error),
    };
    if let Some(error) = dead {
        tx.execute(
            "UPDATE outbox SET state = 'DEAD_LETTER', next_retry_at = NULL, last_error = ?1 WHERE message_id = ?2",
            params![error, message_id],
        )
        .map_err(sqlite_error)?;
        let project_id = project_of(&tx, run_id.as_deref(), message_id)?;
        writer::write(
            &tx,
            &Commit {
                events: vec![NewEvent {
                    project_id,
                    event_id: child_id(message_id, "dead_letter"),
                    run_id,
                    kind: "outbox.dead_letter".to_owned(),
                    payload: json!({
                        "message_id": message_id,
                        "recipient_role": recipient,
                        "attempt_count": count,
                        "last_error": error,
                    }),
                }],
                ..Commit::default()
            },
            after_events,
        )?;
    }
    tx.commit().map_err(sqlite_error)?;
    Ok(())
}

fn project_of(
    tx: &Transaction<'_>,
    run_id: Option<&str>,
    what: &str,
) -> Result<String, PersistError> {
    let Some(run_id) = run_id else {
        return Err(PersistError::IntegrityViolation(format!(
            "{what} names no run"
        )));
    };
    tx.query_row(
        "SELECT project_id FROM runs WHERE run_id = ?1",
        params![run_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| PersistError::RunNotFound(run_id.to_owned()))
}

/// The values a message is fenced against (ADR-018 decision 2).
struct Current {
    project_id: String,
    role_epoch: Option<i64>,
    task_revision: Option<i64>,
    spec_sha256: Option<String>,
    candidate_id: Option<String>,
}

fn current(
    tx: &Transaction<'_>,
    message: &Message,
    consumer: &str,
) -> Result<Current, PersistError> {
    let run: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT r.project_id, s.sha256 FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![message.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, spec_sha256)) = run else {
        return Err(PersistError::RunNotFound(message.run_id.clone()));
    };
    let role_epoch = tx
        .query_row(
            "SELECT role_epoch FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![message.run_id, consumer],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let task_revision = tx
        .query_row(
            "SELECT revision FROM tasks WHERE task_id = ?1 AND run_id = ?2",
            params![message.task_id, message.run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let candidate_id = tx
        .query_row(
            "SELECT candidate_id FROM candidates WHERE run_id = ?1 AND state IN ('FROZEN', 'VERIFYING') \
             ORDER BY created_at DESC, candidate_id DESC LIMIT 1",
            params![message.run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(Current {
        project_id,
        role_epoch,
        task_revision,
        spec_sha256,
        candidate_id,
    })
}

/// The first fence the message fails, in ADR-018's order, with what it found.
fn fence(message: &Message, now: &Current) -> Option<(RejectionReason, String)> {
    let epoch = i64::try_from(message.role_epoch).ok();
    let revision = i64::try_from(message.expected_revision).ok();
    // A handoff's package carries the fences too; they must agree with the envelope.
    let handoff = (message.kind == MessageKind::Handoff)
        .then(|| {
            protocol::decode::<protocol::ContextHandoff>(&Value::Object(message.payload.clone()))
                .ok()
        })
        .flatten();
    if now.role_epoch.is_none() || epoch != now.role_epoch {
        return Some((
            RejectionReason::StaleRoleEpoch,
            format!(
                "role epoch {} is not the current {:?}",
                message.role_epoch, now.role_epoch
            ),
        ));
    }
    if let Some(h) = &handoff
        && h.role_epoch != message.role_epoch
    {
        return Some((
            RejectionReason::StaleRoleEpoch,
            format!(
                "the package's role epoch {} differs from the envelope's",
                h.role_epoch
            ),
        ));
    }
    if now.task_revision.is_none() || revision != now.task_revision {
        return Some((
            RejectionReason::StaleRevision,
            format!(
                "expected revision {} of task {}, it is {:?}",
                message.expected_revision, message.task_id, now.task_revision
            ),
        ));
    }
    if now.spec_sha256.as_deref() != Some(message.spec_sha256.as_str())
        || handoff
            .as_ref()
            .is_some_and(|h| h.spec_sha256 != message.spec_sha256)
    {
        return Some((
            RejectionReason::SpecMismatch,
            format!("specification {} is not the run's", message.spec_sha256),
        ));
    }
    let package_candidate = handoff
        .as_ref()
        .and_then(|h| h.candidate.as_ref())
        .map(|c| c.id.as_str());
    if let Some(candidate) = message.candidate_id.as_deref()
        && (now.candidate_id.as_deref() != Some(candidate)
            || package_candidate.is_some_and(|c| c != candidate))
    {
        return Some((
            RejectionReason::CandidateMismatch,
            format!(
                "candidate {candidate} is not the run's current {:?}",
                now.candidate_id
            ),
        ));
    }
    None
}

/// The reply to `message` from `consumer`.
fn reply(
    message: &Message,
    consumer: &str,
    kind: MessageKind,
    reason: Option<RejectionReason>,
    payload: Value,
) -> Result<String, PersistError> {
    let suffix = if kind == MessageKind::Ack {
        "ack"
    } else {
        "rejected"
    };
    let id = child_id(&message.message_id, suffix);
    let Value::Object(payload) = payload else {
        return Err(PersistError::Storage(
            "a reply payload must be an object".to_owned(),
        ));
    };
    let answer = Message {
        schema_version: "1.0".to_owned(),
        message_id: id.clone(),
        run_id: message.run_id.clone(),
        task_id: message.task_id.clone(),
        attempt_id: message.attempt_id.clone(),
        sender_role: consumer.to_owned(),
        recipient_role: message.sender_role.clone(),
        role_epoch: message.role_epoch,
        correlation_id: message.correlation_id.clone(),
        causation_id: Some(message.message_id.clone()),
        idempotency_key: id,
        expected_revision: message.expected_revision,
        spec_sha256: message.spec_sha256.clone(),
        candidate_id: message.candidate_id.clone(),
        kind,
        payload,
        reason_code: reason.map(Some),
    };
    let encoded = protocol::encode(&answer)
        .map_err(|e| PersistError::Storage(format!("reply to {}: {e}", message.message_id)))?;
    Ok(encoded.to_string())
}

/// Consume `message` as `consumer` once (ADR-018 decision 1): a stored receipt is answered as stored; otherwise the
/// fences decide, and the reply, the receipt, the effect and one event commit together, or nothing does.
pub(super) fn consume(
    conn: &mut Connection,
    consumer: &str,
    message: &Message,
    effect: &ConsumeEffect,
    now: SystemTime,
    after_events: &dyn Fn(),
) -> Result<Consumed, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let stored: Option<String> = tx
        .query_row(
            "SELECT result_json FROM receipts WHERE consumer = ?1 AND message_id = ?2",
            params![consumer, message.message_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(result_json) = stored {
        let rejected = serde_json::from_str::<Value>(&result_json)
            .ok()
            .and_then(|v| v.get("reason_code").cloned())
            .and_then(|c| serde_json::from_value::<RejectionReason>(c).ok());
        return Ok(Consumed {
            result_json,
            replayed: true,
            rejected,
        });
    }
    // A handoff whose package is not a ContextHandoff is no message to act on (the transport records it as invalid;
    // a caller that consumes directly is refused here too).
    if message.kind == MessageKind::Handoff
        && protocol::decode::<protocol::ContextHandoff>(&Value::Object(message.payload.clone()))
            .is_err()
    {
        return Err(PersistError::IntegrityViolation(format!(
            "handoff {} carries no valid ContextHandoff",
            message.message_id
        )));
    }
    let found = current(&tx, message, consumer)?;
    let (result_json, rejected) = match fence(message, &found) {
        Some((reason, detail)) => {
            let result_json = reply(
                message,
                consumer,
                MessageKind::Rejected,
                Some(reason),
                json!({
                    "current_revision": found.task_revision,
                    "current_role_epoch": found.role_epoch,
                    "current_spec_sha256": found.spec_sha256,
                    "current_candidate_id": found.candidate_id,
                    "detail": detail,
                }),
            )?;
            writer::write(
                &tx,
                &Commit {
                    events: vec![NewEvent {
                        project_id: found.project_id.clone(),
                        event_id: child_id(&message.message_id, &format!("{consumer}/rejected")),
                        run_id: Some(message.run_id.clone()),
                        kind: "message.rejected".to_owned(),
                        payload: json!({
                            "message_id": message.message_id,
                            "consumer": consumer,
                            "reason_code": reason,
                            "current_revision": found.task_revision,
                            "detail": detail,
                        }),
                    }],
                    ..Commit::default()
                },
                after_events,
            )?;
            (result_json, Some(reason))
        }
        None => {
            let mut events = effect.events.clone();
            events.push(NewEvent {
                project_id: found.project_id.clone(),
                event_id: child_id(&message.message_id, &format!("{consumer}/applied")),
                run_id: Some(message.run_id.clone()),
                kind: "message.applied".to_owned(),
                payload: json!({"message_id": message.message_id, "consumer": consumer, "kind": message.kind}),
            });
            writer::write(
                &tx,
                &Commit {
                    cas: effect.cas.clone(),
                    events,
                    outbox: Vec::new(),
                },
                after_events,
            )?;
            for attempt in &effect.attempts {
                attempts::create(&tx, attempt)?;
            }
            let revision: Option<i64> = tx
                .query_row(
                    "SELECT revision FROM tasks WHERE task_id = ?1",
                    params![message.task_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            let result_json = reply(
                message,
                consumer,
                MessageKind::Ack,
                None,
                json!({"task_revision": revision}),
            )?;
            (result_json, None)
        }
    };
    tx.execute(
        "INSERT INTO receipts(consumer, message_id, applied_at, result_json) VALUES (?1, ?2, ?3, ?4)",
        params![consumer, message.message_id, rfc3339_utc(now), result_json],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(Consumed {
        result_json,
        replayed: false,
        rejected,
    })
}

/// Record that `consumer` got something that is not a valid message (REQ-022 b1: refused and recorded, never
/// silently ignored): one `message.invalid` event of the run it names, no receipt and no reason code.
pub(super) fn invalid(
    conn: &mut Connection,
    consumer: &str,
    message_id: &str,
    run_id: Option<&str>,
    violations: &str,
    after_events: &dyn Fn(),
) -> Result<i64, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let project_id = project_of(&tx, run_id, message_id)?;
    let done = writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                project_id,
                event_id: child_id(message_id, &format!("{consumer}/invalid")),
                run_id: run_id.map(str::to_owned),
                kind: "message.invalid".to_owned(),
                payload: json!({"message_id": message_id, "consumer": consumer, "violations": violations}),
            }],
            ..Commit::default()
        },
        after_events,
    )?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done.event_seqs[0])
}

impl NewOutbox {
    /// An outbox row carrying the HCP `message` for the event at index `event`: the message must be a valid Message,
    /// a handoff's payload a valid ContextHandoff for the same role (so a handoff with no acceptance ids is never
    /// queued, REQ-022); the row's run, recipient and epoch are the envelope's.
    pub fn hcp(event: usize, message: &Message) -> Result<NewOutbox, PersistError> {
        let payload = protocol::encode(message).map_err(|e| {
            PersistError::IntegrityViolation(format!("message {}: {e}", message.message_id))
        })?;
        if message.kind == MessageKind::Handoff {
            let package: protocol::ContextHandoff =
                protocol::decode(&Value::Object(message.payload.clone())).map_err(|e| {
                    PersistError::IntegrityViolation(format!("handoff {}: {e}", message.message_id))
                })?;
            if package.role != message.recipient_role
                || (package.handoff_kind == HandoffKind::Verification
                    && package.acceptance_ids.is_empty())
            {
                return Err(PersistError::IntegrityViolation(format!(
                    "handoff {} is not for {} or names no acceptance criteria",
                    message.message_id, message.recipient_role
                )));
            }
        }
        Ok(NewOutbox {
            message_id: message.message_id.clone(),
            event,
            run_id: Some(message.run_id.clone()),
            recipient_role: message.recipient_role.clone(),
            role_epoch: i64::try_from(message.role_epoch).map_err(|_| {
                PersistError::IntegrityViolation(format!("role epoch of {}", message.message_id))
            })?,
            payload,
        })
    }
}
