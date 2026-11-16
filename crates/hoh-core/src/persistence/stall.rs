//! The stall detector (docs/adr/ADR-031.md design correction C): one judgement of a run's own persistent facts at the
//! end of each runner tick, in a writer transaction of its own, instead of a continuation duty per path. A run in a
//! working state W, supervised (its Commander bound, its specification adopted), is stalled when (a) none of its
//! attempts is unfinished, (b) nothing of its work would move on its own (RUNNING, VERIFYING: the pace's judgement;
//! the Commander's stages: always), (c) its supervisor has nothing to do — no PENDING trigger, no owed wake, no output
//! to dispose of, no waking event not yet taken in, (d) no live recovery obligation is due, and (e) no budget stop
//! stands. Then, on its progress marker P, the latest obligation settled first: a `STALLED` re-trigger for fewer than K
//! of them since the last question on P, else the run-level question `runtime:stalled`.

use super::blockers::{self, Claims};
use super::writer::{self, Commit, Ctx, Intent, NewEvent};
use super::{PersistError, sqlite_error};
use crate::decision::Context;
use crate::ids::child_id;
use crate::protocol::{BlockedReason, RunState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// How many turns of the Commander without progress on one marker before the run's question (decision: K = 2).
pub(super) const K: usize = 2;

/// The re-trigger's reason (REQ-021 behaviour 6's kind, `supervisor.retrigger`).
pub(super) const STALLED: &str = "STALLED";

/// The question's claim source and reason.
const SOURCE: &str = "runtime:stalled";
const REASON: &str = "stalled";

/// The working states the detector looks at.
const W: [RunState; 6] = [
    RunState::Created,
    RunState::Discovering,
    RunState::Specifying,
    RunState::Ready,
    RunState::Running,
    RunState::Verifying,
];

/// What one detection did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Detected {
    /// Not stalled, not looked at, or its latest obligation not settled yet: nothing written.
    #[default]
    Nothing,
    /// `supervisor.retrigger{STALLED}` number `n` on its marker.
    Retriggered(usize),
    /// The run-level question asked, the run BLOCKED (NEED_DECISION).
    Asked,
}

/// Run `run_id`'s progress marker P (M2T13D-304): the first 16 hex digits of the sha256 of the canonical JSON of its
/// state, how many of its work attempts started and how many of those ended (a terminal state or UNKNOWN_EFFECT), and
/// its candidates with their states. Changing the plan (tasks, dependencies, assumptions, rejected decisions) does
/// not change it, nor does supervision.
pub(super) fn progress(conn: &Connection, run_id: &str) -> Result<String, PersistError> {
    let state: String = conn
        .query_row(
            "SELECT state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let (started, ended): (i64, i64) = conn
        .query_row(
            "SELECT count(*), count(*) FILTER (WHERE a.state IN ('SETTLED', 'FAILED', 'CANCELLED', 'UNKNOWN_EFFECT')) \
             FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.run_id = ?1 AND t.role <> 'commander' AND a.started_at IS NOT NULL",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let candidates: Vec<Value> = {
        let mut statement = conn
            .prepare("SELECT candidate_id, state FROM candidates WHERE run_id = ?1 ORDER BY candidate_id")
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| {
                Ok(json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?]))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let canonical =
        json!({"state": state, "started": started, "ended": ended, "candidates": candidates})
            .to_string();
    Ok(crate::context::sha256_hex(canonical.as_bytes())[..16].to_owned())
}

/// Look at run `run_id` once at `context.now()`, in a writer transaction of its own; `pending` holds the candidates
/// with check records still to be recorded (the tick's `.hoh/pending/`).
pub(super) fn detect(
    conn: &mut Connection,
    run_id: &str,
    pending: &BTreeSet<String>,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Detected, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let detected = judge(&tx, run_id, pending, context, ctx)?;
    if detected != Detected::Nothing {
        super::writer::commit(tx, ctx)?;
    }
    Ok(detected)
}

fn judge(
    tx: &Transaction<'_>,
    run_id: &str,
    pending: &BTreeSet<String>,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Detected, PersistError> {
    let row: Option<(String, String, Option<String>, Option<i64>)> = tx
        .query_row(
            "SELECT r.project_id, r.state, r.spec_id, s.revision FROM runs r \
             LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, state, spec_id, spec_revision)) = row else {
        return Err(PersistError::RunNotFound(run_id.to_owned()));
    };
    let state = writer::parse::<RunState>(&state)?;
    // Supervised runs in W only (docs/adr/ADR-025.md decisions 4 and 5).
    let bound: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM role_bindings WHERE run_id = ?1 AND role = 'commander')",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !W.contains(&state) || spec_id.is_none() || !bound {
        return Ok(Detected::Nothing);
    }
    // (a) No attempt of it unfinished, its work's and its supervision's.
    if super::retry::unfinished_attempts(tx, run_id)?.0 > 0 {
        return Ok(Detected::Nothing);
    }
    // (b) Nothing of its work moves on its own: the pace's judgement while it works; in the Commander's stages only
    // the Commander moves it.
    if matches!(state, RunState::Running | RunState::Verifying)
        && !super::retry::idle(tx, run_id, context.now())?
    {
        return Ok(Detected::Nothing);
    }
    // (c) Its supervisor has nothing to do: no PENDING trigger, no owed wake, no output to dispose of, no waking event
    // not taken in yet.
    let triggered: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM supervisor_triggers WHERE run_id = ?1 AND state = 'PENDING')",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if triggered
        || super::wakes::of_run(tx, run_id)?
            .iter()
            .any(super::wakes::Wake::owed)
        || !super::outputs::waiting(tx, run_id)?.is_empty()
        || super::supervise::unread_waking(tx, run_id)?
    {
        return Ok(Detected::Nothing);
    }
    // (e) A budget stop is the pace's to settle.
    if super::retry::budget_stop(tx, run_id, context)?.is_some() {
        return Ok(Detected::Nothing);
    }
    // (d), the dearer reads last (M2T13R-03): no live recovery obligation of its candidates (the runner's own filter:
    // pending or held by a failure, and due; one still backing off counts), the failures read for its candidates only.
    if recovery_owed(tx, run_id, pending)? {
        return Ok(Detected::Nothing);
    }
    let marker = progress(tx, run_id)?;
    // Its STALLED re-triggers and questions on P, read by their ids (each `child_id(run, "stalled/<P>/…")`, numbered
    // from 1 with no gap), not by a scan of its events.
    let stalled = numbered(tx, run_id, &|n| format!("stalled/{marker}/{n}"))?;
    let asked = numbered(tx, run_id, &|k| format!("stalled/{marker}/question/{k}"))?;
    // The precondition (M2T13D-300, -402, -451): the latest obligation on P consumed, its turn ended and its outputs
    // disposed of, read by its identity.
    if let Some(&latest) = stalled.last()
        && !settled(tx, run_id, &marker, stalled.len(), latest)?
    {
        return Ok(Detected::Nothing);
    }
    let since = asked.last().copied().unwrap_or(0);
    let c = stalled.iter().filter(|seq| **seq > since).count();
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        intent: Intent::Progress,
        project_id: project_id.clone(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    if c < K {
        let n = stalled.len() + 1;
        let base = format!("stalled/{marker}/{n}");
        let id = child_id(run_id, &base);
        if exists(tx, &id)? {
            return Ok(Detected::Nothing);
        }
        writer::write(
            tx,
            &Commit {
                // Its turn is shown it, and how many turns in a row (this one's included) moved the run no further.
                events: vec![event(
                    id,
                    "supervisor.retrigger",
                    json!({"reason": STALLED, "base_fingerprint": base, "progress": marker, "n": n,
                        "spec_revision": spec_revision, "turns": c + 1,
                        "text": format!("已连续 {} 个没有进展的 turn", c + 1)}),
                )],
                ..Commit::default()
            },
            ctx,
        )?;
        return Ok(Detected::Retriggered(n));
    }
    // c >= K: the run-level question (W14), its claim and the run BLOCKED.
    let k = asked.len() + 1;
    let id = child_id(run_id, &format!("stalled/{marker}/question/{k}"));
    if exists(tx, &id)? {
        return Ok(Detected::Nothing);
    }
    let text = format!(
        "Commander 连续 {c} 个 turn 没有让 Run 前进：答复 retry（可附指示）让它再试，或答复 cancel 取消本 Run"
    );
    let mut claims = Claims::load(tx, &project_id, run_id)?;
    claims.set(SOURCE, &[BlockedReason::NeedDecision]);
    let changed = claims.changed();
    let reasons = claims.reasons();
    let mut events = claims.store(tx)?;
    events.push(event(
        id.clone(),
        "question.asked",
        json!({"question_id": id, "reason": REASON, "progress": marker, "turns": c, "text": text,
            "choices": ["retry", "cancel"]}),
    ));
    let at = crate::time::rfc3339_utc(context.now());
    let follow = blockers::follow(
        tx,
        &project_id,
        run_id,
        changed,
        &reasons,
        &at,
        "the Commander's turns moved the run no further",
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
    Ok(Detected::Asked)
}

/// The seqs of run `run_id`'s events `child_id(run_id, name(1))`, `child_id(run_id, name(2))`, … up to the first
/// missing: each a lookup by its unique id.
fn numbered(
    tx: &Transaction<'_>,
    run_id: &str,
    name: &dyn Fn(usize) -> String,
) -> Result<Vec<i64>, PersistError> {
    let mut statement = tx
        .prepare("SELECT seq FROM events WHERE event_id = ?1 AND run_id = ?2")
        .map_err(sqlite_error)?;
    let mut seqs = Vec::new();
    loop {
        let id = child_id(run_id, &name(seqs.len() + 1));
        match statement
            .query_row(params![id, run_id], |r| r.get(0))
            .optional()
            .map_err(sqlite_error)?
        {
            Some(seq) => seqs.push(seq),
            None => return Ok(seqs),
        }
    }
}

/// Whether a candidate of run `run_id` has a live recovery obligation the runner would act on: its records pending
/// (`pending`) or unrecovered — the runner's own rule, `unrecovered_candidates`, of this run's candidates — and
/// `verification::live_obligation`.
fn recovery_owed(
    tx: &Transaction<'_>,
    run_id: &str,
    pending: &BTreeSet<String>,
) -> Result<bool, PersistError> {
    // Only a candidate in a state a live obligation needs (VERIFYING, REJECTED, INVALID) is looked at further.
    let candidates: Vec<String> = {
        let mut statement = tx
            .prepare(
                "SELECT candidate_id FROM candidates WHERE run_id = ?1 \
                 AND state IN ('VERIFYING', 'REJECTED', 'INVALID')",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    if candidates.is_empty() {
        return Ok(false);
    }
    let unrecovered = super::unrecovered_candidates(tx, Some(run_id))?;
    for candidate in &candidates {
        if (pending.contains(candidate) || unrecovered.contains(candidate))
            && super::verification::live_obligation(tx, candidate)?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether the STALLED re-trigger `stalled/<marker>/<n>` (event `seq`) of run `run_id` is settled: its trigger (keyed by
/// its fingerprint, `stalled/<marker>/<n>#retrigger`) HANDLED, and the wake it woke (the trigger's own, or the one it
/// was merged into, read from its `woken` by id) settled by its identity (`wakes::settled_by_id`).
fn settled(
    tx: &Transaction<'_>,
    run_id: &str,
    marker: &str,
    n: usize,
    seq: i64,
) -> Result<bool, PersistError> {
    let trigger: Option<(String, String)> = tx
        .query_row(
            "SELECT t.trigger_id, t.state FROM supervisor_triggers t WHERE t.run_id = ?1 AND t.fingerprint = ?2 \
             AND EXISTS(SELECT 1 FROM json_each(t.event_seqs_json) s WHERE s.value = ?3)",
            params![run_id, format!("stalled/{marker}/{n}#retrigger"), seq],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((trigger_id, state)) = trigger else {
        return Ok(false);
    };
    if state != "HANDLED" {
        return Ok(false);
    }
    let merged: Option<Option<String>> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.merged_into') FROM events WHERE event_id = ?1 AND run_id = ?2",
            params![child_id(&trigger_id, "woken"), run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let wake = merged.flatten().unwrap_or(trigger_id);
    super::wakes::settled_by_id(tx, run_id, &wake)
}

fn exists(tx: &Transaction<'_>, event_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

#[cfg(test)]
mod tests;
