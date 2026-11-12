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

/// Whether run `run_id`'s `role` at `role_epoch`, in its session `session_id`, owes its first input (decision 4,
/// M2T17R-200, -300): the session.start of that epoch was delivered to that session, and no attempt of the session has
/// SETTLED (the first turn that completes began with the package, every turn does while it is owed). The notice is
/// looked up by its id, so a session never handed over costs one lookup.
pub(super) fn first_input_owed(
    conn: &Connection,
    run_id: &str,
    role: &str,
    role_epoch: i64,
    session_id: &str,
) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM outbox o JOIN receipts r ON r.message_id = o.message_id \
          WHERE o.message_id = ?1 AND o.state = 'ACKED' AND json_extract(r.result_json, '$.session_id') = ?2) \
         AND NOT EXISTS(SELECT 1 FROM attempts p WHERE p.session_id = ?2 AND p.state = 'SETTLED')",
        params![
            crate::ids::child_id(run_id, &format!("session-start/{role}/{role_epoch}")),
            session_id
        ],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// Whether the session owes its first input (see [`first_input_owed`]) and has an attempt in flight (decision 4,
/// M2T17R-201): one PREPARED, STARTING or RUNNING. Its role is then dispatched nothing more, so the first input goes to
/// one turn at a time until one turn completed.
pub(super) fn first_input_in_flight(
    conn: &Connection,
    run_id: &str,
    role: &str,
    role_epoch: i64,
    session_id: &str,
) -> Result<bool, PersistError> {
    if !first_input_owed(conn, run_id, role, role_epoch, session_id)? {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE session_id = ?1 AND state IN ('PREPARED', 'STARTING', 'RUNNING'))",
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

/// The attempts of the previous holder's session (`from`, as team.binding_applied records it), newest first.
fn previous(conn: &Connection, from: &serde_json::Value) -> Result<Vec<String>, PersistError> {
    let Some(session) = from.get("session_id").and_then(serde_json::Value::as_str) else {
        return Ok(Vec::new());
    };
    let mut statement = conn
        .prepare("SELECT attempt_id FROM attempts WHERE session_id = ?1 ORDER BY rowid DESC")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![session], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// The writes that hand run `run_id`'s `role` over to its binding at `role_epoch` (decisions 1 and 2): the
/// handoff.prepared event and the session.start notice pointing at it, to be committed with the binding. `spec` is the
/// run's (see [`spec`]), none while it has none (CR-014); `from` and `to` are the bindings as team.binding_applied records them.
#[allow(clippy::too_many_arguments)]
pub(super) fn prepare(
    conn: &rusqlite::Transaction<'_>,
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
        // What the role's previous holder produced in this run, newest first, by reference only: none of its content
        // is read (docs/adr/ADR-026.md decision 10).
        references: super::context::references(
            conn,
            &previous(conn, from)?,
            super::results::REFERENCES,
        )?,
        budget_tokens: super::context::budget(conn, run_id)?,
    });
    let message_id = crate::ids::child_id(run_id, &format!("session-start/{role}/{epoch}"));
    // Its handoffs row, PREPARED, its package registered as the object of its own bytes, which the executor writes
    // when it takes the first input (docs/adr/ADR-028.md decision 9).
    let bytes = package_bytes(&package)?;
    let sha256 = crate::context::sha256_hex(&bytes);
    let object = super::runner::ArtifactRow {
        path: crate::objects::relative_path(&sha256),
        sha256,
        bytes: bytes.len() as u64,
        mime: "application/json".to_owned(),
        kind: "handoff_package".to_owned(),
        name: message_id.clone(),
    };
    super::runner::register(conn, std::slice::from_ref(&object))?;
    conn.execute(
        "INSERT OR IGNORE INTO handoffs(handoff_id, run_id, from_role, to_role, role_epoch, package_artifact_id, state, \
         created_at) VALUES (?1, ?2, ?3, ?3, ?4, ?5, 'PREPARED', ?6)",
        params![
            message_id,
            run_id,
            role,
            role_epoch,
            super::runner::artifact_id(&object.sha256, &object.path),
            crate::time::now_rfc3339()
        ],
    )
    .map_err(sqlite_error)?;
    let package = crate::protocol::encode(&package)
        .map_err(|e| PersistError::IntegrityViolation(format!("the handover package: {e}")))?;
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

/// The bytes of handover package `package` as its object keeps them (docs/adr/ADR-028.md decision 9): the store
/// registers their digest when it writes the handoff, the executor writes them when it takes the first input.
pub fn package_bytes(package: &crate::protocol::ContextHandoff) -> Result<Vec<u8>, PersistError> {
    let value = crate::protocol::encode(package)
        .map_err(|e| PersistError::IntegrityViolation(format!("the handover package: {e}")))?;
    serde_json::to_vec(&value)
        .map_err(|e| PersistError::IntegrityViolation(format!("the handover package: {e}")))
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
    // Its handoffs row: DELIVERED with the package, REJECTED when superseded (docs/adr/ADR-028.md decision 9).
    let (state, from) = if delivered_to.is_some() {
        ("DELIVERED", "'PREPARED'")
    } else {
        ("REJECTED", "'PREPARED', 'DELIVERED'")
    };
    tx.execute(
        &format!("UPDATE handoffs SET state = ?2 WHERE handoff_id = ?1 AND state IN ({from})"),
        params![message_id, state],
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
