//! A role's handover (REQ-019; docs/adr/ADR-024.md): what the checkpoint that rebinds a role to a new holder writes in
//! its own transaction beside the new binding, a handoff.prepared event and one runtime session.start notice to the
//! new holder carrying its checkpoint package (b2, b4).

use super::{PersistError, sqlite_error};
use crate::handoff::{self, RoleChange};
use crate::protocol::HandoffState;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

/// The Commander: its own messages carry its epoch (decision 6).
pub(super) const COMMANDER: &str = "commander";

/// Run `run_id`'s current specification: its digest and revision, none before the run has one.
pub(super) fn spec(conn: &Connection, run_id: &str) -> Result<Option<(String, u64)>, PersistError> {
    let spec: Option<(String, i64)> = conn
        .query_row(
            "SELECT s.sha256, s.revision FROM runs r JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    spec.map(|(sha256, revision)| {
        u64::try_from(revision)
            .map(|revision| (sha256, revision))
            .map_err(|_| PersistError::IntegrityViolation(format!("spec revision {revision}")))
    })
    .transpose()
}

/// Whether run `run_id`'s `role` at `role_epoch` still waits for its first input: its session.start notice is there
/// and not delivered (decision 4). Such a holder is dispatched nothing, so no task reaches it before its package.
pub(super) fn first_input_pending(
    conn: &Connection,
    run_id: &str,
    role: &str,
    role_epoch: i64,
) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox WHERE message_id = ?1 AND state <> 'ACKED')",
        params![crate::ids::child_id(
            run_id,
            &format!("session-start/{role}/{role_epoch}")
        )],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// Whether HoH session `session_id` owes its first input and has an attempt in flight (decision 4, M2T17R-201): its
/// session.start was delivered to it, no turn of it was answered by the agent yet (no native session recorded), and
/// one of its attempts is PREPARED, STARTING or RUNNING. Its role is then dispatched nothing more, so the first input
/// goes to one turn at a time until the agent has answered one.
pub(super) fn first_input_in_flight(
    conn: &Connection,
    session_id: &str,
) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions s WHERE s.session_id = ?1 AND s.native_id IS NULL \
         AND EXISTS(SELECT 1 FROM outbox o JOIN receipts r ON r.message_id = o.message_id WHERE o.state = 'ACKED' \
          AND json_extract(o.payload_json, '$.kind') = 'session.start' \
          AND json_extract(r.result_json, '$.session_id') = s.session_id) \
         AND EXISTS(SELECT 1 FROM attempts a WHERE a.session_id = s.session_id \
          AND a.state IN ('PREPARED', 'STARTING', 'RUNNING')))",
        params![session_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The ids `sql` (one `?1`, the run) reads.
fn ids(conn: &Connection, run_id: &str, sql: &str) -> Result<Vec<String>, PersistError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The writes that hand run `run_id`'s `role` over to its binding at `role_epoch` (decisions 1 and 2): the
/// handoff.prepared event and the session.start notice pointing at it, to be committed with the binding. `spec` is the
/// run's (see [`spec`]), none while it has none (CR-014); `from` and `to` are the bindings as team.binding_applied records them.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    conn: &Connection,
    project_id: &str,
    run_id: &str,
    role: &str,
    role_epoch: i64,
    spec: Option<(String, u64)>,
    from: &serde_json::Value,
    to: &serde_json::Value,
) -> Result<(super::writer::NewEvent, super::writer::NewOutbox), PersistError> {
    let epoch = u64::try_from(role_epoch)
        .map_err(|_| PersistError::IntegrityViolation(format!("role epoch {role_epoch}")))?;
    let state = HandoffState {
        applied_decision_ids: ids(
            conn,
            run_id,
            "SELECT decision_id FROM decisions WHERE run_id = ?1 AND state = 'APPLIED'",
        )?,
        open_issue_ids: ids(
            conn,
            run_id,
            "SELECT issue_id FROM issues WHERE run_id = ?1 AND state IN ('OPEN', 'REOPENED')",
        )?,
        unacked_message_ids: ids(
            conn,
            run_id,
            "SELECT message_id FROM outbox WHERE run_id = ?1 AND state <> 'ACKED'",
        )?,
        queued_input_ids: ids(
            conn,
            run_id,
            "SELECT input_id FROM input_queue WHERE run_id = ?1 AND state = 'QUEUED'",
        )?,
    };
    let goal = super::results::started_goal(conn, run_id)?
        .unwrap_or_else(|| format!("接手运行 {run_id} 的 {role} 职责"));
    let package = handoff::role_change(&RoleChange {
        run_id: run_id.to_owned(),
        role: role.to_owned(),
        goal,
        role_epoch: epoch,
        spec,
        candidate: super::outbox::current_candidate(conn, run_id)?,
        state,
    });
    let package = crate::protocol::encode(&package)
        .map_err(|e| PersistError::IntegrityViolation(format!("the handover package: {e}")))?;
    let message_id = crate::ids::child_id(run_id, &format!("session-start/{role}/{epoch}"));
    let event = super::writer::NewEvent {
        project_id: project_id.to_owned(),
        event_id: crate::ids::child_id(run_id, &format!("handoff/{role}/{epoch}")),
        run_id: Some(run_id.to_owned()),
        kind: "handoff.prepared".to_owned(),
        payload: json!({
            "run_id": run_id, "role": role, "role_epoch": epoch, "from": from, "to": to,
            "message_id": message_id, "spec_revision": package["spec_revision"],
            "candidate_id": package["candidate"]["id"], "state": package["state"],
        }),
    };
    let notice = super::writer::NewOutbox {
        message_id,
        // Set by the caller to the event's index in its commit.
        event: 0,
        run_id: Some(run_id.to_owned()),
        recipient_role: role.to_owned(),
        role_epoch,
        payload: json!({
            "notice": "runtime", "kind": "session.start", "run_id": run_id, "role": role,
            "role_epoch": epoch, "package": package,
        }),
    };
    Ok((event, notice))
}

/// A session.start notice waiting for the runner (decision 4): its package, and the binding of its role as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionStart {
    pub message_id: String,
    pub run_id: String,
    pub role: String,
    /// The epoch the notice was written for.
    pub role_epoch: i64,
    pub package: crate::protocol::ContextHandoff,
    /// The binding's epoch now: past the notice's, the notice is superseded.
    pub current_epoch: i64,
    /// The binding's session, none until the binder grants one.
    pub session_id: Option<String>,
    pub agent: String,
}

/// The session.start notices PENDING in project `project_id`'s runs not ended, oldest first.
pub(super) fn waiting(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<SessionStart>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT o.message_id, o.run_id, b.role, o.role_epoch, json_extract(o.payload_json, '$.package'), \
             b.role_epoch, b.session_id, b.agent FROM outbox o JOIN runs r ON r.run_id = o.run_id \
             JOIN role_bindings b ON b.run_id = o.run_id AND b.role = json_extract(o.payload_json, '$.role') \
             WHERE r.project_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'session.start' \
             AND r.state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE') ORDER BY o.event_seq, o.message_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![project_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, String>(7)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    rows.into_iter()
        .map(
            |(message_id, run_id, role, role_epoch, package, current_epoch, session_id, agent)| {
                let package = serde_json::from_str(&package)
                    .map_err(|e| e.to_string())
                    .and_then(|v| crate::protocol::decode(&v).map_err(|e| e.to_string()))
                    .map_err(|e| {
                        PersistError::IntegrityViolation(format!(
                            "the package of {message_id}: {e}"
                        ))
                    })?;
                Ok(SessionStart {
                    message_id,
                    run_id,
                    role,
                    role_epoch,
                    package,
                    current_epoch,
                    session_id,
                    agent,
                })
            },
        )
        .collect()
}

/// Record that the executor did not take session.start notice `message_id` (decision 4, M2T17R-150): one
/// handoff.undelivered event with `detail`, while the notice is PENDING and only the first time, so the role's wait for
/// its first input is seen, not only said once on stderr. Whether it was written now.
pub(super) fn undelivered(
    conn: &mut Connection,
    message_id: &str,
    detail: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let row: Option<(String, String, i64, String)> = tx
        .query_row(
            "SELECT o.run_id, json_extract(o.payload_json, '$.role'), o.role_epoch, r.project_id FROM outbox o \
             JOIN runs r ON r.run_id = o.run_id WHERE o.message_id = ?1 AND o.state = 'PENDING' \
             AND json_extract(o.payload_json, '$.kind') IS 'session.start'",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((run_id, role, epoch, project_id)) = row else {
        return Ok(false);
    };
    let event_id = crate::ids::child_id(message_id, "handoff.undelivered");
    let seen: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
            params![event_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if seen {
        return Ok(false);
    }
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events: vec![super::writer::NewEvent {
                project_id,
                event_id,
                run_id: Some(run_id.clone()),
                kind: "handoff.undelivered".to_owned(),
                payload: json!({
                    "run_id": run_id, "role": role, "role_epoch": epoch, "message_id": message_id, "detail": detail,
                }),
            }],
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// Settle session.start notice `message_id` (decision 4), in one transaction: `Some(session)` once the executor took
/// its package for that session, which must be its binding's at the notice's own epoch (receipt, ACKED,
/// handoff.delivered); `None` when the binding has passed the notice's epoch, so it is never delivered (a rejected
/// receipt, STALE_ROLE_EPOCH, ACKED, handoff.superseded). Whether it was still PENDING and so settled.
pub(super) fn settle(
    conn: &mut Connection,
    message_id: &str,
    delivered_to: Option<&str>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let row: Option<(String, String, i64, i64, Option<String>, String)> = tx
        .query_row(
            "SELECT o.run_id, b.role, o.role_epoch, b.role_epoch, b.session_id, r.project_id FROM outbox o \
             JOIN runs r ON r.run_id = o.run_id \
             JOIN role_bindings b ON b.run_id = o.run_id AND b.role = json_extract(o.payload_json, '$.role') \
             WHERE o.message_id = ?1 AND o.state = 'PENDING' AND json_extract(o.payload_json, '$.notice') IS 'runtime' \
             AND json_extract(o.payload_json, '$.kind') IS 'session.start'",
            params![message_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((run_id, role, epoch, current, session, project_id)) = row else {
        return Ok(false);
    };
    let (kind, result) = match delivered_to {
        Some(to) if current == epoch && session.as_deref() == Some(to) => (
            "handoff.delivered",
            json!({"session_id": to, "role_epoch": epoch}),
        ),
        None if current > epoch => (
            "handoff.superseded",
            json!({"rejected": "STALE_ROLE_EPOCH", "role_epoch": epoch, "current_epoch": current}),
        ),
        _ => return Ok(false),
    };
    let at = crate::time::now_rfc3339();
    tx.execute(
        "INSERT INTO receipts(consumer, message_id, applied_at, result_json) VALUES (?1, ?2, ?3, ?4)",
        params![super::runner::RUNNER, message_id, at, result.to_string()],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "UPDATE outbox SET state = 'ACKED', next_retry_at = NULL WHERE message_id = ?1",
        params![message_id],
    )
    .map_err(sqlite_error)?;
    let mut payload = json!({"run_id": run_id, "role": role, "message_id": message_id});
    if let (Some(object), Some(extra)) = (payload.as_object_mut(), result.as_object()) {
        object.extend(extra.clone());
    }
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events: vec![super::writer::NewEvent {
                project_id,
                event_id: crate::ids::child_id(message_id, kind),
                run_id: Some(run_id),
                kind: kind.to_owned(),
                payload,
            }],
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}
