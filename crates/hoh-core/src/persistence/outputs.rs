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
use crate::lifecycle::{AttemptState, Lifecycle as _};
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

/// How many outputs one writer job disposes of at most (docs/adr/ADR-025.md v16 decision 2, M2T08R-201).
pub(super) const DISPOSE_BATCH: usize = 50;

/// An output of a run with no end yet: its id, its turn and its event's seq.
pub(super) struct Waiting {
    pub output_id: String,
    pub attempt_id: String,
    pub seq: i64,
}

/// The outputs of run `run_id` with no end yet, in order: one scan of the run's outputs and one of their ends, matched
/// by a set (v16, M2T08R-201: a correlated lookup per output made it quadratic).
pub(super) fn waiting(conn: &Connection, run_id: &str) -> Result<Vec<Waiting>, PersistError> {
    let ended: std::collections::BTreeSet<String> = {
        let mut statement = conn
            .prepare(
                "SELECT json_extract(payload_json, '$.output_id') FROM events WHERE run_id = ?1 \
                 AND kind IN (?2, 'decision.applied', 'decision.rejected')",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<Option<String>> = statement
            .query_map(params![run_id, DISPOSED], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        rows.into_iter().flatten().collect()
    };
    let mut statement = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.output_id'), json_extract(payload_json, '$.attempt_id'), seq \
             FROM events WHERE run_id = ?1 AND kind = ?2 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(Option<String>, Option<String>, i64)> = statement
        .query_map(params![run_id, OUTPUT], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|(output_id, attempt_id, seq)| {
            let output_id = output_id?;
            (!ended.contains(&output_id)).then_some(Waiting {
                output_id,
                attempt_id: attempt_id.unwrap_or_default(),
                seq,
            })
        })
        .collect())
}

/// The turns of run `run_id` with an output that has no end yet (docs/adr/ADR-025.md v14 decision 3: their wakes are
/// still owed).
pub(super) fn pending_attempts(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    Ok(waiting(tx, run_id)?
        .into_iter()
        .map(|w| w.attempt_id)
        .collect())
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

/// How turn `attempt_id` ended, by its `attempt.moved` event: the state and that event's seq; none while it has not.
fn turn_end(conn: &Connection, attempt_id: &str) -> Result<Option<(String, i64)>, PersistError> {
    let ids: Vec<String> = ["SETTLED", "FAILED", "CANCELLED", "UNKNOWN_EFFECT"]
        .iter()
        .map(|state| child_id(attempt_id, state))
        .collect();
    conn.query_row(
        "SELECT json_extract(payload_json, '$.to'), seq FROM events WHERE event_id IN (?1, ?2, ?3, ?4) \
         ORDER BY seq LIMIT 1",
        params![ids[0], ids[1], ids[2], ids[3]],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sqlite_error)
}

/// Dispose of run `run_id`'s outputs at `context.now()`, one transaction each, at most [`DISPOSE_BATCH`] in this
/// writer job (v16): whether some are left for another job. Only the outputs of turns the step's verdict saw end (their
/// end event at or below the step's cursor `upto`) are disposed of: a SETTLED turn's as usual, those of a turn that
/// ended otherwise discarded; the others wait (v17, M2T08R-250). A cancelled or ended run's are superseded whatever
/// their turn. It stops, none left for now, when the run's state holds them or one fails (its failure counted; the
/// run's others wait for the next step).
pub(super) fn dispose(
    conn: &mut Connection,
    run_id: &str,
    upto: i64,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let state: String = conn
        .query_row(
            "SELECT state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let superseding = {
        let state = writer::parse::<RunState>(&state)?;
        state.is_terminal() || state == RunState::CancelRequested
    };
    // The outputs that may be disposed of now, each with whether its turn settled.
    let mut ends: std::collections::BTreeMap<String, Option<(String, i64)>> =
        std::collections::BTreeMap::new();
    let mut list: Vec<(Waiting, bool)> = Vec::new();
    for w in waiting(conn, run_id)? {
        if superseding {
            list.push((w, true));
            continue;
        }
        if !ends.contains_key(&w.attempt_id) {
            ends.insert(w.attempt_id.clone(), turn_end(conn, &w.attempt_id)?);
        }
        if let Some((to, at)) = &ends[&w.attempt_id]
            && *at <= upto
        {
            let settled = to == AttemptState::Settled.as_str();
            list.push((w, settled));
        }
    }
    let more = list.len() > DISPOSE_BATCH;
    // Each turn's allocation read once in this job.
    let mut allocations: std::collections::BTreeMap<String, Value> =
        std::collections::BTreeMap::new();
    for (
        Waiting {
            output_id,
            attempt_id,
            seq,
        },
        settled,
    ) in list.into_iter().take(DISPOSE_BATCH)
    {
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
            return Ok(false);
        }
        let text: String = tx
            .query_row(
                "SELECT coalesce(json_extract(payload_json, '$.text'), '') FROM events WHERE seq = ?1",
                params![seq],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let ended = if state.is_terminal() || state == RunState::CancelRequested {
            Ok(Some(end("superseded", None)))
        } else if !settled {
            // Its turn ended without settling: its wake is owed again, another turn answers it (v17).
            Ok(Some(end("discarded", Some("TURN_NOT_SETTLED"))))
        } else {
            if !allocations.contains_key(&attempt_id) {
                let allocation = allocation(&tx, run_id, &attempt_id)?;
                allocations.insert(attempt_id.clone(), allocation);
            }
            one(
                &tx,
                &project_id,
                run_id,
                &output_id,
                &attempt_id,
                &text,
                &allocations[&attempt_id],
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
                return Ok(false);
            }
        }
    }
    Ok(more)
}

/// Turn `attempt_id`'s `supervisor.allocated` (null when there is none).
fn allocation(tx: &Transaction<'_>, run_id: &str, attempt_id: &str) -> Result<Value, PersistError> {
    let allocation: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE run_id = ?1 AND kind = 'supervisor.allocated' \
             AND json_extract(payload_json, '$.attempt_id') = ?2",
            params![run_id, attempt_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(allocation
        .and_then(|a| serde_json::from_str(&a).ok())
        .unwrap_or(Value::Null))
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
    allocation: &Value,
    context: &Context,
    ctx: &Ctx<'_>,
) -> Result<Option<End>, PersistError> {
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
