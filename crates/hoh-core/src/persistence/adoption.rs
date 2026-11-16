//! A run's specification (docs/adr/ADR-031.md decision 1, owner question Q1 (a)): StartRun adopts the project's latest
//! approved specification in its own transaction, its `spec.changed{cause: "adopted"}` waking the Commander; a project
//! with none blocks the new run on the run-level question `runtime:no_spec` (W13), whose one exit, `adopt`, adopts the
//! specification approved since or leaves the run as it is.
//!
//! A specs row is approved by construction: its only writer is to be M3-T02's ApproveSpec, which inserts approved
//! revisions only (a draft is no specs row); the locked table has no approval column to filter by (M2T13D-59).

use super::need::{self, RunQuestion, RunReason};
use super::writer::{self, Commit, Ctx, Intent, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

/// The question's one choice (`questions::route_run(RunReason::NoSpec)`'s).
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

/// The question `runtime:no_spec` of run `run_id` (W13), asked through the run-level questions' one entry
/// (`need::run_question`) in the caller's transaction after the run's own events: NEED_DECISION claimed by
/// `runtime:no_spec`, `question.asked{question_id: <its own id>, reason: "no_spec", text, choices: [adopt]}`, and the
/// run BLOCKED by the claim (`blockers::follow`; a working run, CREATED included). Not a `need_decision` event: a run
/// with no specification has no supervision task, a wake would never be allocated.
pub(super) fn ask(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let at = crate::time::rfc3339_utc(ctx.now());
    let commit = need::run_question(
        tx,
        RunQuestion {
            project_id,
            run_id,
            reason: RunReason::NoSpec,
            question_id: child_id(run_id, "no_spec/question"),
            fields: json!({"text": TEXT, "choices": [ADOPT]}),
            at: &at,
            detail: "the project has no approved specification",
        },
        ctx,
    )?;
    writer::write(tx, &commit, ctx)?;
    Ok(())
}

/// The question's `adopt` exit (decision 8's table; `questions::answer`'s `Route::Adopt`), in the answer's
/// transaction: the project's latest approved specification adopted ([`adopt`]) with its `spec.changed` written;
/// true, and the answer releases the claim, its run unblocked by its follow when nothing else holds it. Still none:
/// nothing written, false: the run stays BLOCKED and the question open (the answer's commit touches the run and ends
/// with its run.result).
pub(super) fn adopt_exit(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let Some(adopted) = adopt(tx, project_id, run_id)? else {
        return Ok(false);
    };
    writer::write(
        tx,
        &Commit {
            events: vec![adopted],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(true)
}

#[cfg(test)]
mod tests;
