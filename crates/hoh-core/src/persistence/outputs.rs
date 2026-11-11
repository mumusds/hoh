//! A Commander turn's outputs (docs/adr/ADR-025.md decision 8): the lines of its file bridge, recorded as
//! `supervisor.output` when the turn succeeded, then each disposed of exactly once by the supervisor's step, one
//! transaction each. An output's end is the first `decision.applied`/`decision.rejected` that names it (a decision put
//! through `decide` in the output's transaction) or one `supervisor.output_disposed{outcome}`: `replay`, `deferred`
//! (not a decision), `invalid` (with a reason), `unsupported` (past decide's fences but not applied yet: a request for
//! a person, NEED_DECISION claimed for it; create_tasks) or `superseded` (the run cancelled or ended). A paused run's
//! outputs wait.

use super::blockers::{self, Claims};
use super::decide::{self, Decided};
use super::writer::{self, Commit, Ctx, NewEvent};
use super::{PersistError, sqlite_error};
use crate::decision::{Context, Proposal};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{self, BlockedReason, DecisionPayload, Message, MessageKind, RunState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

pub(super) const OUTPUT: &str = "supervisor.output";
const DISPOSED: &str = "supervisor.output_disposed";
const FAILED: &str = "supervisor.output_failed";
/// A disposal that fails for a reason other than the data ends the output INTERNAL at its third failure.
const FAILURES: i64 = 3;

/// The events recording turn `attempt_id`'s bridge `lines` in the transaction of its work's OBSERVED: one
/// `supervisor.output` per line when it `succeeded`, else one `supervisor.output_discarded` (decision 8, -100, -258).
pub(super) fn recorded(
    project_id: &str,
    run_id: &str,
    attempt_id: &str,
    succeeded: bool,
    lines: &[Value],
) -> Vec<NewEvent> {
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    if !succeeded {
        return if lines.is_empty() {
            Vec::new()
        } else {
            vec![event(
                child_id(attempt_id, "outputs-discarded"),
                "supervisor.output_discarded",
                json!({"attempt_id": attempt_id, "count": lines.len(), "detail": "the turn did not succeed"}),
            )]
        };
    }
    lines
        .iter()
        .enumerate()
        .map(|(n, text)| {
            let output_id = child_id(attempt_id, &format!("output/{n}"));
            event(
                output_id.clone(),
                OUTPUT,
                json!({
                    "output_id": output_id,
                    "attempt_id": attempt_id,
                    "line": n,
                    "text": text.as_str().map_or_else(|| text.to_string(), str::to_owned),
                }),
            )
        })
        .collect()
}

/// The next output of run `run_id` with no end yet: its id, turn and text.
pub(super) fn pending(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<Option<(String, String, String)>, PersistError> {
    tx.query_row(
        "SELECT json_extract(e.payload_json, '$.output_id'), json_extract(e.payload_json, '$.attempt_id'), \
         json_extract(e.payload_json, '$.text') FROM events e WHERE e.run_id = ?1 AND e.kind = ?2 \
         AND NOT EXISTS (SELECT 1 FROM events d WHERE d.run_id = e.run_id \
           AND d.kind IN (?3, 'decision.applied', 'decision.rejected') \
           AND json_extract(d.payload_json, '$.output_id') = json_extract(e.payload_json, '$.output_id')) \
         ORDER BY e.seq LIMIT 1",
        params![run_id, OUTPUT, DISPOSED],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()
    .map_err(sqlite_error)
}

/// The turns of run `run_id` with an output that has no end yet (docs/adr/ADR-025.md v14 decision 3: their wakes are
/// still owed).
pub(super) fn pending_attempts(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT DISTINCT json_extract(e.payload_json, '$.attempt_id') FROM events e WHERE e.run_id = ?1 \
             AND e.kind = ?2 AND NOT EXISTS (SELECT 1 FROM events d WHERE d.run_id = e.run_id \
               AND d.kind IN (?3, 'decision.applied', 'decision.rejected') \
               AND json_extract(d.payload_json, '$.output_id') = json_extract(e.payload_json, '$.output_id'))",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<Option<String>> = statement
        .query_map(params![run_id, OUTPUT, DISPOSED], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows.into_iter().flatten().collect())
}

/// How one output ends, short of a decision put through `decide`.
struct End {
    outcome: &'static str,
    reason: Option<&'static str>,
    extra: Value,
}

fn end(outcome: &'static str, reason: Option<&'static str>) -> End {
    End {
        outcome,
        reason,
        extra: json!({}),
    }
}

/// Dispose of run `run_id`'s outputs at `context.now()`, one transaction each, until none is left, the run's state
/// holds them, or one fails (its failure counted; the run's others wait for the next step).
pub(super) fn dispose(
    conn: &mut Connection,
    run_id: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    loop {
        let tx = writer::begin(conn, ctx)?;
        let (project_id, state): (String, String) = tx
            .query_row(
                "SELECT project_id, state FROM runs WHERE run_id = ?1",
                params![run_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(sqlite_error)?;
        let state = writer::parse::<RunState>(&state)?;
        if matches!(
            state,
            RunState::PauseRequested | RunState::Paused | RunState::Incomplete
        ) {
            return Ok(());
        }
        let Some((output_id, attempt_id, text)) = pending(&tx, run_id)? else {
            return Ok(());
        };
        let ended = if state.is_terminal() || state == RunState::CancelRequested {
            Ok(Some(end("superseded", None)))
        } else {
            one(
                &tx,
                &project_id,
                run_id,
                &output_id,
                &attempt_id,
                &text,
                context,
                ctx,
            )
        };
        match ended {
            Ok(done) => {
                if let Some(End {
                    outcome,
                    reason,
                    extra,
                }) = done
                {
                    let mut payload = json!({"output_id": output_id, "attempt_id": attempt_id, "outcome": outcome});
                    if let Some(reason) = reason {
                        payload["reason"] = json!(reason);
                    }
                    if let (Some(all), Some(more)) = (payload.as_object_mut(), extra.as_object()) {
                        all.extend(more.clone());
                    }
                    writer::write(
                        &tx,
                        &Commit {
                            events: vec![NewEvent {
                                project_id: project_id.clone(),
                                event_id: child_id(&output_id, "disposed"),
                                run_id: Some(run_id.to_owned()),
                                kind: DISPOSED.to_owned(),
                                payload,
                            }],
                            ..Commit::default()
                        },
                        ctx,
                    )?;
                }
                ctx.before_commit()?;
                tx.commit().map_err(sqlite_error)?;
            }
            Err(e) => {
                drop(tx);
                failed(conn, &project_id, run_id, &output_id, &attempt_id, &e, ctx)?;
                return Ok(());
            }
        }
    }
}

/// Count a failed disposal of `output_id` in a transaction of its own; the third ends it INTERNAL.
fn failed(
    conn: &mut Connection,
    project_id: &str,
    run_id: &str,
    output_id: &str,
    attempt_id: &str,
    error: &PersistError,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let n: i64 = tx
        .query_row(
            "SELECT count(*) + 1 FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND json_extract(payload_json, '$.output_id') = ?3",
            params![run_id, FAILED, output_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let mut events = vec![NewEvent {
        project_id: project_id.to_owned(),
        event_id: child_id(output_id, &format!("failed/{n}")),
        run_id: Some(run_id.to_owned()),
        kind: FAILED.to_owned(),
        payload: json!({"output_id": output_id, "attempt_id": attempt_id, "n": n, "error": error.to_string()}),
    }];
    if n >= FAILURES {
        events.push(NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(output_id, "disposed"),
            run_id: Some(run_id.to_owned()),
            kind: DISPOSED.to_owned(),
            payload: json!({"output_id": output_id, "attempt_id": attempt_id, "outcome": "invalid", "reason": "INTERNAL"}),
        });
    }
    writer::write(
        &tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)
}

/// One output's disposal in `tx`: its end to record, or none when a decision event ended it.
#[allow(clippy::too_many_arguments)]
fn one(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    output_id: &str,
    attempt_id: &str,
    text: &str,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Option<End>, PersistError> {
    let allocation: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE run_id = ?1 AND kind = 'supervisor.allocated' \
             AND json_extract(payload_json, '$.attempt_id') = ?2",
            params![run_id, attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let allocation: Value = allocation
        .and_then(|a| serde_json::from_str(&a).ok())
        .unwrap_or(Value::Null);
    let wake = allocation["wake"].as_str().unwrap_or_default();
    let Ok(Value::Object(line)) = serde_json::from_str::<Value>(text) else {
        return Ok(Some(end("invalid", Some("FORMAT"))));
    };
    let nonce = line
        .get("nonce")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if super::wakes::sha256_hex(nonce.as_bytes())
        != allocation["nonce_sha256"].as_str().unwrap_or("-")
    {
        return Ok(Some(end("invalid", Some("NONCE"))));
    }
    let Some(Ok(message)) = line.get("message").map(protocol::decode::<Message>) else {
        return Ok(Some(end("invalid", Some("FORMAT"))));
    };
    if message.run_id != run_id {
        return Ok(Some(end("invalid", Some("RUN"))));
    }
    if message.sender_role != crate::decision::COMMANDER {
        return Ok(Some(end("invalid", Some("SENDER"))));
    }
    if message.causation_id.as_deref() != Some(wake) {
        return Ok(Some(end("invalid", Some("CAUSATION"))));
    }
    if message.kind != MessageKind::Decision {
        // Steer, query and handoff are routed by the delivery loop (M2-T16).
        return Ok(Some(end("deferred", None)));
    }
    let Ok(mut proposal) = Proposal::from_message(&message) else {
        return Ok(Some(end("invalid", Some("PAYLOAD"))));
    };
    // Its id in this turn's namespace: a message id reused by another turn or run is another decision (-259).
    proposal.decision_id = child_id(attempt_id, &message.message_id);
    // Every decision, a request for a person too, passes decide's fences (v13, M2T08R-02).
    match decide::decide_in(tx, &proposal, context, ctx, Some(output_id)) {
        Ok(Decided::Applied { .. } | Decided::Rejected { .. }) => Ok(None),
        Ok(Decided::Unsupported { .. }) => match &proposal.payload {
            DecisionPayload::RequestUserDecision { .. } => {
                person(tx, project_id, run_id, &proposal, context, ctx).map(Some)
            }
            _ => Ok(Some(End {
                outcome: "unsupported",
                reason: None,
                extra: json!({"decision_id": proposal.decision_id}),
            })),
        },
        Ok(Decided::Replayed { .. }) => Ok(Some(End {
            outcome: "replay",
            reason: None,
            extra: json!({"decision_id": proposal.decision_id}),
        })),
        Ok(Decided::KeyReuse) => Ok(Some(End {
            outcome: "invalid",
            reason: Some("KEY_REUSE"),
            extra: json!({"decision_id": proposal.decision_id}),
        })),
        Err(PersistError::InvalidProposal(_)) => Ok(Some(end("invalid", Some("PAYLOAD")))),
        Err(e) => Err(e),
    }
}

/// A request for a person past decide's fences (decide does not apply it yet, M2-T13; its decisions row stays
/// PROPOSED, so the same id again is a replay or a key reuse): `unsupported`, with NEED_DECISION claimed for it (source
/// `commander:<decision_id>`, released by its answer) and the run blocked by it.
fn person(
    tx: &Transaction<'_>,
    project_id: &str,
    run_id: &str,
    proposal: &Proposal,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<End, PersistError> {
    let payload = protocol::encode(&proposal.payload)
        .map_err(|e| PersistError::InvalidProposal(e.to_string()))?;
    let digest = super::wakes::sha256_hex(payload.to_string().as_bytes());
    let mut claims = Claims::load(tx, project_id, run_id)?;
    let before = claims.reasons();
    claims.set(
        &format!("commander:{}", proposal.decision_id),
        &[BlockedReason::NeedDecision],
    );
    let union = claims.reasons();
    let mut commit = Commit {
        events: claims.store(tx)?,
        ..Commit::default()
    };
    let moved = blockers::follow(
        tx,
        project_id,
        run_id,
        union != before,
        &union,
        &crate::time::rfc3339_utc(context.now()),
        "the commander asks a person to decide",
        ctx,
    )?;
    commit.cas.extend(moved.cas);
    commit.events.extend(moved.events);
    writer::write(tx, &commit, ctx)?;
    Ok(End {
        outcome: "unsupported",
        reason: None,
        extra: json!({"decision_id": proposal.decision_id, "payload_sha256": digest}),
    })
}
