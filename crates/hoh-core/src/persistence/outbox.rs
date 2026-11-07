//! The outbox and its receipts (REQ-022, REQ-062; docs/adr/ADR-018.md): taking messages for delivery, settling a
//! delivery, consuming a message once behind its fences, and recording a message that is not one.

use super::attempts::{self, NewAttempt};
use super::decide::child_id;
use super::writer::{self, Commit, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::protocol::{self, Candidate, Message, MessageKind, RejectionReason};
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

/// Take the next message due at `now` for one of `roles` and start its delivery (ADR-018 decision 4): every row but
/// the runtime notices (exactly `"notice": "runtime"`: decide's and the round's dispatch, PauseRun's run.control), so a
/// row that should hold an HCP message and does not, whatever else it carries, is validated and recorded, never skipped
/// (M2T04R-101, -201); one whose deliveries are spent goes to DEAD_LETTER on the way, with its event. The row
/// returned carries this delivery's generation (its new `attempt_count`).
#[allow(clippy::too_many_arguments)]
pub(super) fn claim_next(
    conn: &mut Connection,
    now: SystemTime,
    roles: &[String],
    max_attempts: i64,
    returned: &[OutboxRow],
    live: &std::collections::BTreeSet<String>,
    after_events: &dyn Fn(),
) -> Result<Option<OutboxRow>, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    // First the claims returned without reaching a transport: undone here, so none is claimed again (and counted)
    // before its undo (M2T04R-400, -450).
    for row in returned {
        undo(&tx, row)?;
    }
    let stamp = rfc3339_utc(now);
    let roles = json!(roles).to_string();
    let live = json!(live).to_string();
    loop {
        let due: Option<(String, Option<String>, String, i64, String, i64)> = tx
            .query_row(
                "SELECT message_id, run_id, recipient_role, role_epoch, payload_json, attempt_count FROM outbox \
                 WHERE ((state = 'PENDING' AND (next_retry_at IS NULL OR next_retry_at <= ?1)) \
                        OR (state = 'DELIVERING' AND next_retry_at <= ?1)) \
                   AND recipient_role IN (SELECT value FROM json_each(?2)) \
                   AND json_extract(payload_json, '$.notice') IS NOT 'runtime' \
                   AND message_id NOT IN (SELECT value FROM json_each(?3)) \
                 ORDER BY event_seq, message_id LIMIT 1",
                params![stamp, roles, live],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((message_id, run_id, recipient_role, role_epoch, payload, count)) = due else {
            tx.commit().map_err(sqlite_error)?;
            return Ok(None);
        };
        if count >= max_attempts {
            // Its deliveries were taken and never settled (a crash, a cancelled tick): spent all the same.
            dead_letter(
                &tx,
                &message_id,
                run_id,
                &recipient_role,
                count,
                "no answer to its last delivery",
                after_events,
            )?;
            continue;
        }
        let generation = count + 1;
        let retry = rfc3339_utc(now + backoff(generation));
        tx.execute(
            "UPDATE outbox SET state = 'DELIVERING', attempt_count = ?1, next_retry_at = ?2 WHERE message_id = ?3",
            params![generation, retry, message_id],
        )
        .map_err(sqlite_error)?;
        let payload = serde_json::from_str(&payload)
            .map_err(|e| PersistError::IntegrityViolation(format!("outbox {message_id}: {e}")))?;
        tx.commit().map_err(sqlite_error)?;
        return Ok(Some(OutboxRow {
            message_id,
            run_id,
            recipient_role,
            role_epoch,
            payload,
            attempt_count: generation,
            next_retry_at: Some(retry),
        }));
    }
}

/// Undo the claim of `row`, which never reached a transport: the delivery it never had is not counted, and a row still
/// DELIVERING is due again at once. Until its undo nothing settles that generation and nothing claims the row again, so
/// the row is still at that generation and either DELIVERING or ACKED by a late answer of an earlier delivery, whose
/// state stands (M2T04R-500; ADR-018 decision 4).
fn undo(tx: &Transaction<'_>, row: &OutboxRow) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE outbox SET attempt_count = attempt_count - 1, \
                state = CASE state WHEN 'DELIVERING' THEN 'PENDING' ELSE state END, \
                next_retry_at = NULL \
         WHERE message_id = ?1 AND state IN ('DELIVERING', 'ACKED') AND attempt_count = ?2",
        params![row.message_id, row.attempt_count],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// Undo `rows` in one transaction (the store closes, M2T04R-301).
pub(super) fn unclaim_all(conn: &mut Connection, rows: &[OutboxRow]) -> Result<(), PersistError> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    for row in rows {
        undo(&tx, row)?;
    }
    tx.commit().map_err(sqlite_error)
}

/// DEAD_LETTER and its `outbox.dead_letter` event, in the caller's transaction.
fn dead_letter(
    tx: &Transaction<'_>,
    message_id: &str,
    run_id: Option<String>,
    recipient: &str,
    count: i64,
    error: &str,
    after_events: &dyn Fn(),
) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE outbox SET state = 'DEAD_LETTER', next_retry_at = NULL, last_error = ?1 WHERE message_id = ?2",
        params![error, message_id],
    )
    .map_err(sqlite_error)?;
    let project_id = project_of(tx, run_id.as_deref(), message_id)?;
    writer::write(
        tx,
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
    )
    .map(drop)
}

/// Settle delivery `generation` of `message_id` (ADR-018 decision 4): an answer from any generation makes an unsettled
/// message ACKED (its receipt is committed); a failure counts only for the current generation (a late one of an
/// earlier delivery is ignored): PENDING again, or DEAD_LETTER with its event at the limit or when the row holds no
/// message. Rows are never deleted.
pub(super) fn settle(
    conn: &mut Connection,
    message_id: &str,
    generation: i64,
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
    let unsettled = state == "DELIVERING" || state == "PENDING";
    let current = state == "DELIVERING" && count == generation;
    match settlement {
        Settlement::Delivered if unsettled => {
            tx.execute(
                "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL, last_error = NULL WHERE message_id = ?1",
                params![message_id],
            )
            .map_err(sqlite_error)?;
        }
        // A late answer to a message dead-lettered meanwhile: it was delivered after all (its receipt is committed), so
        // ACKED, with an event beside the dead letter's (M2T04R-103).
        Settlement::Delivered if state == "DEAD_LETTER" => {
            tx.execute(
                "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1",
                params![message_id],
            )
            .map_err(sqlite_error)?;
            let project_id = project_of(&tx, run_id.as_deref(), message_id)?;
            writer::write(
                &tx,
                &Commit {
                    events: vec![NewEvent {
                        project_id,
                        event_id: child_id(message_id, &format!("delivered-late/{generation}")),
                        run_id,
                        kind: "outbox.delivered_late".to_owned(),
                        payload: json!({"message_id": message_id, "generation": generation, "attempt_count": count}),
                    }],
                    ..Commit::default()
                },
                after_events,
            )?;
        }
        Settlement::Failed(error) if current && count < max_attempts => {
            tx.execute(
                "UPDATE outbox SET state = 'PENDING', last_error = ?1 WHERE message_id = ?2",
                params![error, message_id],
            )
            .map_err(sqlite_error)?;
        }
        Settlement::Failed(error) | Settlement::Invalid(error) if current => {
            dead_letter(
                &tx,
                message_id,
                run_id,
                &recipient,
                count,
                error,
                after_events,
            )?;
        }
        // Settled already, or a late answer of an earlier delivery: the settlement that came first stands.
        _ => {}
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
    spec_revision: Option<i64>,
    candidate_id: Option<String>,
    /// The current candidate's full identity.
    candidate: Option<Candidate>,
}

/// The run's current candidate (ADR-018 decision 2): the newest FROZEN or VERIFYING one. The one definition both the
/// consume fence and the Developer's TaskResult use (M2T04R-54).
pub(super) fn current_candidate(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<Candidate>, PersistError> {
    tx.query_row(
        "SELECT candidate_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256 FROM candidates \
         WHERE run_id = ?1 AND state IN ('FROZEN', 'VERIFYING') ORDER BY created_at DESC, candidate_id DESC LIMIT 1",
        params![run_id],
        |r| {
            Ok(Candidate {
                id: r.get(0)?,
                git_commit: r.get(1)?,
                git_tree: r.get(2)?,
                spec_sha256: r.get(3)?,
                environment_sha256: r.get(4)?,
                test_plan_sha256: r.get(5)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

fn current(
    tx: &Transaction<'_>,
    message: &Message,
    consumer: &str,
) -> Result<Current, PersistError> {
    let run: Option<(String, Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT r.project_id, s.sha256, s.revision FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id \
             WHERE r.run_id = ?1",
            params![message.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, spec_sha256, spec_revision)) = run else {
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
    let candidate = current_candidate(tx, &message.run_id)?;
    Ok(Current {
        project_id,
        role_epoch,
        task_revision,
        spec_sha256,
        spec_revision,
        candidate_id: candidate.as_ref().map(|c| c.id.clone()),
        candidate,
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
    if let Some(h) = &handoff
        && now.spec_revision.and_then(|r| u64::try_from(r).ok()) != Some(h.spec_revision)
    {
        return Some((
            RejectionReason::SpecMismatch,
            format!(
                "the package's specification revision {} is not the run's {:?}",
                h.spec_revision, now.spec_revision
            ),
        ));
    }
    if let Some(candidate) = message.candidate_id.as_deref()
        && now.candidate_id.as_deref() != Some(candidate)
    {
        return Some((
            RejectionReason::CandidateMismatch,
            format!(
                "candidate {candidate} is not the run's current {:?}",
                now.candidate_id
            ),
        ));
    }
    // A handoff's candidate is the current one in every field of its identity, not only by id (M2T04R-04).
    if let Some(h) = &handoff
        && h.candidate.is_some()
        && h.candidate != now.candidate
    {
        return Some((
            RejectionReason::CandidateMismatch,
            format!(
                "the package's candidate {:?} is not the run's current one",
                h.candidate.as_ref().map(|c| &c.id)
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
    // A message whose payload is not valid for its kind, or disagrees with its envelope, is no message to act on (the
    // transport records it as invalid; a caller that consumes directly is refused here too, M2T04R-06).
    // The whole envelope is a valid Message too: a caller may hand over one it built itself (M2T04R-102).
    protocol::encode(message).map_err(|e| {
        PersistError::IntegrityViolation(format!("message {}: {e}", message.message_id))
    })?;
    message
        .check_payload()
        .map_err(PersistError::IntegrityViolation)?;
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
    // Recorded already (a crash between recording and settling): the same event, not a second one (M2T04R-59).
    let event_id = child_id(message_id, &format!("{consumer}/invalid"));
    let recorded: Option<i64> = tx
        .query_row(
            "SELECT seq FROM events WHERE event_id = ?1",
            params![event_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(seq) = recorded {
        return Ok(seq);
    }
    let project_id = project_of(&tx, run_id, message_id)?;
    let done = writer::write(
        &tx,
        &Commit {
            events: vec![NewEvent {
                project_id,
                event_id,
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
        message
            .check_payload()
            .map_err(PersistError::IntegrityViolation)?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_backoff_doubles_from_one_second_up_to_a_minute() {
        // ADR-018 decision 4 (M2T04R-63).
        let secs: Vec<u64> = (0..=9).map(|k| super::backoff(k).as_secs()).collect();
        assert_eq!(secs, [1, 1, 2, 4, 8, 16, 32, 60, 60, 60]);
    }
}
