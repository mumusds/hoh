//! A run's specification (docs/adr/ADR-031.md decision 1, owner question Q1 (a)): StartRun adopts the project's latest
//! approved specification in its own transaction, its `spec.changed{cause: "adopted"}` waking the Commander; a project
//! with none blocks the new run on the run-level question `runtime:no_spec` (W13), whose one exit, `adopt`, adopts the
//! specification approved since or leaves the run as it is.
//!
//! A specs row is approved by construction: its only writer is to be M3-T02's ApproveSpec, which inserts approved
//! revisions only (a draft is no specs row); the locked table has no approval column to filter by (M2T13D-59).

use super::blockers::{self, Claims};
use super::writer::{self, Commit, Ctx, Intent, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::protocol::BlockedReason;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

/// The question's claim source and reason (correction A's `RunReason::NoSpec`).
pub(super) const SOURCE: &str = "runtime:no_spec";
pub(super) const REASON: &str = "no_spec";

/// The question's one choice.
const ADOPT: &str = "adopt";

const TEXT: &str = "项目还没有已批准的规格：先在 TUI 用 /plan 生成并批准规格（M3），或在项目中放入已批准的规格后答复本问题";

/// The `spec.changed{cause: "adopted"}` of run `run_id` adopting project `project_id`'s latest approved specification
/// (the highest revision), its `spec_id` written into the run's row in the caller's transaction; none, and nothing
/// written, when the project has no specification. The run had none before (`from: null`): REQ-021 behaviour 1's
/// specification change, keyed on its `spec_revision`.
pub(super) fn adopt(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
) -> Result<Option<NewEvent>, PersistError> {
    let latest: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT spec_id, revision, sha256 FROM specs WHERE project_id = ?1 ORDER BY revision DESC LIMIT 1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((spec_id, revision, sha256)) = latest else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE runs SET spec_id = ?1 WHERE run_id = ?2",
        params![spec_id, run_id],
    )
    .map_err(sqlite_error)?;
    Ok(Some(NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: child_id(run_id, "spec/adopted"),
        run_id: Some(run_id.to_owned()),
        kind: "spec.changed".to_owned(),
        payload: json!({"run_id": run_id, "spec_id": spec_id, "spec_revision": revision, "spec_sha256": sha256,
            "from": null, "cause": "adopted"}),
    }))
}

/// The question `runtime:no_spec` of run `run_id` (W13), written in the caller's transaction after the run's own
/// events: NEED_DECISION claimed for it, `question.asked{question_id: <its own id>, reason: "no_spec", text, choices:
/// [adopt]}`, and the run BLOCKED by the claim (`blockers::follow`; a working run, CREATED included). Not a
/// `need_decision` event: a run with no specification has no supervision task, a wake would never be allocated.
pub(super) fn ask(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let mut claims = Claims::load(tx, project_id, run_id)?;
    claims.set(SOURCE, &[BlockedReason::NeedDecision]);
    let changed = claims.changed();
    let reasons = claims.reasons();
    let mut events = claims.store(tx)?;
    let id = child_id(run_id, "no_spec/question");
    events.push(NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id: id.clone(),
        run_id: Some(run_id.to_owned()),
        kind: "question.asked".to_owned(),
        payload: json!({"question_id": id, "reason": REASON, "text": TEXT, "choices": [ADOPT]}),
    });
    let follow = blockers::follow(
        tx,
        project_id,
        run_id,
        changed,
        &reasons,
        &crate::time::rfc3339_utc(ctx.now()),
        "the project has no approved specification",
        ctx,
    )?;
    events.extend(follow.events);
    writer::write(
        tx,
        &Commit {
            cas: follow.cas,
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

/// The question's `adopt` exit (decision 8's table), written in the caller's transaction after the answer's own
/// compare-and-set (it reads the run's revision): the project's latest approved specification adopted ([`adopt`]),
/// the claim released and the run unblocked when nothing else holds it (`blockers::follow`); true. Still none: nothing
/// written, the run stays BLOCKED and the question open (the answer's commit ends with its run.result); false.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the answer's exits (slice S2a's route_run) call it once the slices are integrated"
    )
)]
pub(super) fn adopt_exit(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let Some(adopted) = adopt(tx, project_id, run_id)? else {
        return Ok(false);
    };
    let mut claims = Claims::load(tx, project_id, run_id)?;
    claims.set(SOURCE, &[]);
    let changed = claims.changed();
    let reasons = claims.reasons();
    let mut events = vec![adopted];
    events.extend(claims.store(tx)?);
    let follow = blockers::follow(
        tx,
        project_id,
        run_id,
        changed,
        &reasons,
        &crate::time::rfc3339_utc(ctx.now()),
        "the project's approved specification was adopted",
        ctx,
    )?;
    events.extend(follow.events);
    writer::write(
        tx,
        &Commit {
            cas: follow.cas,
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests;
