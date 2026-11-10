//! The Commander's handover (REQ-019; docs/adr/ADR-024.md): what the checkpoint that rebinds the commander writes in
//! its own transaction beside the new binding, a handoff.prepared event and one runtime session.start notice to the
//! new Commander carrying its checkpoint package (b2, b4).

use super::{PersistError, sqlite_error};
use crate::handoff::{self, RoleChange};
use crate::protocol::HandoffState;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::json;

/// The role whose change hands its state over in a package (decision 1): the other roles take over from their next
/// task's own handoff.
pub(super) const COMMANDER: &str = "commander";

/// Why a commander change is refused while its run has no specification (decision 10): a role-change package needs
/// the run's spec digest and revision, and none is invented.
pub(super) const NO_SPEC: &str = "NO_SPEC";

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

/// The ids `sql` (one `?1`, the run) reads.
fn ids(conn: &Connection, run_id: &str, sql: &str) -> Result<Vec<String>, PersistError> {
    let mut statement = conn.prepare(sql).map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The writes that hand run `run_id`'s commander over to its binding at `role_epoch` (decision 2): the
/// handoff.prepared event and the session.start notice pointing at it, to be committed with the binding. `spec` is the
/// run's (see [`spec`]); `from` and `to` are the bindings as team.binding_applied records them.
pub(super) fn prepare(
    conn: &Connection,
    project_id: &str,
    run_id: &str,
    role_epoch: i64,
    spec: (String, u64),
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
        .unwrap_or_else(|| format!("接手运行 {run_id} 的 Commander 职责"));
    let package = handoff::role_change(&RoleChange {
        run_id: run_id.to_owned(),
        goal,
        role_epoch: epoch,
        spec_sha256: spec.0,
        spec_revision: spec.1,
        candidate: super::outbox::current_candidate(conn, run_id)?,
        state,
    });
    let package = crate::protocol::encode(&package)
        .map_err(|e| PersistError::IntegrityViolation(format!("the handover package: {e}")))?;
    let message_id = crate::ids::child_id(run_id, &format!("session-start/{COMMANDER}/{epoch}"));
    let event = super::writer::NewEvent {
        project_id: project_id.to_owned(),
        event_id: crate::ids::child_id(run_id, &format!("handoff/{COMMANDER}/{epoch}")),
        run_id: Some(run_id.to_owned()),
        kind: "handoff.prepared".to_owned(),
        payload: json!({
            "run_id": run_id, "role": COMMANDER, "role_epoch": epoch, "from": from, "to": to,
            "message_id": message_id, "spec_revision": package["spec_revision"],
            "candidate_id": package["candidate"]["id"], "state": package["state"],
        }),
    };
    let notice = super::writer::NewOutbox {
        message_id,
        // Set by the caller to the event's index in its commit.
        event: 0,
        run_id: Some(run_id.to_owned()),
        recipient_role: COMMANDER.to_owned(),
        role_epoch,
        payload: json!({
            "notice": "runtime", "kind": "session.start", "run_id": run_id, "role": COMMANDER,
            "role_epoch": epoch, "package": package,
        }),
    };
    Ok((event, notice))
}
