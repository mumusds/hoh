//! Run-level blocker claims (docs/adr/ADR-025.md decision 5): a run's blocker set is the union of the reasons of its
//! claims not yet released. A claim is a `run.blocker_claimed{reason, source}` event and its release a
//! `run.blocker_released{reason, source}`, written with the run's `blocked_reasons_json` and `blocked_reason` in one
//! transaction; each source releases only its own claims. The sources: `pace` (what blocks the tasks of a run none of
//! whose tasks can move on), `budget` (a cost limit it cannot enforce), `supervisor` and `commander:<decision_id>`.

use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{BlockedReason, RunState};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The pace's source: what blocks the tasks of a stalled run.
pub(super) const PACE: &str = "pace";
/// The budget's source: a cost limit that cannot be enforced.
pub(super) const BUDGET: &str = "budget";

const CLAIMED: &str = "run.blocker_claimed";
const RELEASED: &str = "run.blocker_released";

/// BlockedReason precedence for a run (protocol.schema.json): BLOCKED_AUTH > COMMANDER_UNAVAILABLE >
/// BUDGET_UNENFORCEABLE > BLOCKED_CAPABILITY > NEED_DECISION; the others after them.
pub(super) fn precedence(reason: BlockedReason) -> u8 {
    match reason {
        BlockedReason::BlockedAuth => 0,
        BlockedReason::CommanderUnavailable => 1,
        BlockedReason::BudgetUnenforceable => 2,
        BlockedReason::BlockedCapability => 3,
        BlockedReason::NeedDecision => 4,
        _ => 5,
    }
}

/// Order reasons by precedence (then by name), each once.
pub(super) fn ordered(reasons: impl IntoIterator<Item = BlockedReason>) -> Vec<BlockedReason> {
    let mut reasons: Vec<BlockedReason> = reasons.into_iter().collect();
    reasons.sort_by_key(|r| (precedence(*r), r.as_str()));
    reasons.dedup();
    reasons
}

/// A run's open claims, read in a transaction, and the claim events this transaction adds.
pub(super) struct Claims {
    project_id: String,
    run_id: String,
    /// (source, reason) of every claim not yet released.
    open: BTreeSet<(String, BlockedReason)>,
    /// The run's set as its row holds it.
    stored: Vec<BlockedReason>,
    events: Vec<NewEvent>,
    /// The run's claim events so far: the next one's number.
    next: usize,
}

impl Claims {
    /// The run's open claims. A row with a set and no claim event (a run M2-T19 blocked) is taken over: its reasons
    /// become the budget's (BUDGET_UNENFORCEABLE) and the pace's (the others) claims, recorded as `migrated`.
    pub(super) fn load(
        tx: &Transaction<'_>,
        project_id: &str,
        run_id: &str,
    ) -> Result<Claims, PersistError> {
        let stored: String = tx
            .query_row(
                "SELECT blocked_reasons_json FROM runs WHERE run_id = ?1",
                params![run_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let stored = ordered(
            serde_json::from_str::<Vec<String>>(&stored)
                .unwrap_or_default()
                .iter()
                .filter_map(|r| BlockedReason::parse(r)),
        );
        let mut statement = tx
            .prepare(
                "SELECT kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3) ORDER BY seq",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(String, String)> = statement
            .query_map(params![run_id, CLAIMED, RELEASED], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let mut claims = Claims {
            project_id: project_id.to_owned(),
            run_id: run_id.to_owned(),
            open: BTreeSet::new(),
            stored,
            events: Vec::new(),
            next: rows.len(),
        };
        for (kind, payload) in &rows {
            let payload: Value = serde_json::from_str(payload).map_err(|e| {
                PersistError::IntegrityViolation(format!("a claim event of {run_id}: {e}"))
            })?;
            let (Some(source), Some(reason)) = (
                payload["source"].as_str(),
                payload["reason"].as_str().and_then(BlockedReason::parse),
            ) else {
                return Err(PersistError::IntegrityViolation(format!(
                    "a claim event of {run_id} names no source or reason: {payload}"
                )));
            };
            let claim = (source.to_owned(), reason);
            if kind == CLAIMED {
                claims.open.insert(claim);
            } else {
                claims.open.remove(&claim);
            }
        }
        if rows.is_empty() {
            for reason in claims.stored.clone() {
                let source = if reason == BlockedReason::BudgetUnenforceable {
                    BUDGET
                } else {
                    PACE
                };
                claims.record(CLAIMED, source, reason, true);
                claims.open.insert((source.to_owned(), reason));
            }
        }
        Ok(claims)
    }

    fn record(&mut self, kind: &str, source: &str, reason: BlockedReason, migrated: bool) {
        let mut payload = json!({"reason": reason.as_str(), "source": source});
        if migrated {
            payload["migrated"] = json!(true);
        }
        self.events.push(NewEvent {
            project_id: self.project_id.clone(),
            event_id: child_id(&self.run_id, &format!("blocker/{}", self.next)),
            run_id: Some(self.run_id.clone()),
            kind: kind.to_owned(),
            payload,
        });
        self.next += 1;
    }

    /// Make `source`'s open claims exactly `reasons`: its claims no longer named are released, the new ones claimed.
    pub(super) fn set(&mut self, source: &str, reasons: &[BlockedReason]) {
        let want = ordered(reasons.iter().copied());
        let have: Vec<BlockedReason> = ordered(
            self.open
                .iter()
                .filter(|(s, _)| s == source)
                .map(|(_, r)| *r),
        );
        for reason in have.iter().filter(|r| !want.contains(r)) {
            self.open.remove(&(source.to_owned(), *reason));
            self.record(RELEASED, source, *reason, false);
        }
        for reason in want.iter().filter(|r| !have.contains(r)) {
            self.open.insert((source.to_owned(), *reason));
            self.record(CLAIMED, source, *reason, false);
        }
    }

    /// Whether `source` holds any open claim.
    pub(super) fn held_by(&self, source: &str) -> bool {
        self.open.iter().any(|(s, _)| s == source)
    }

    /// The run's blocker set: the reasons of its open claims, by precedence.
    pub(super) fn reasons(&self) -> Vec<BlockedReason> {
        ordered(self.open.iter().map(|(_, r)| *r))
    }

    /// Whether the set differs from the one the run's row holds.
    pub(super) fn changed(&self) -> bool {
        self.reasons() != self.stored
    }

    /// Write the set into the run's row (its `blocked_reason` the first by precedence); the claim events to append in
    /// the same commit. A set emptied here is cleared by the run's own move out of BLOCKED (`Change::Unblock`), which
    /// the caller adds to the commit: a BLOCKED row always names a reason.
    pub(super) fn store(self, tx: &Transaction<'_>) -> Result<Vec<NewEvent>, PersistError> {
        let reasons = self.reasons();
        if reasons != self.stored && !reasons.is_empty() {
            let json: Vec<&str> = reasons.iter().map(|r| r.as_str()).collect();
            tx.execute(
                "UPDATE runs SET blocked_reasons_json = ?1, blocked_reason = ?2 WHERE run_id = ?3",
                params![
                    serde_json::to_string(&json)
                        .map_err(|e| PersistError::Storage(e.to_string()))?,
                    reasons.first().map(|r| r.as_str()),
                    self.run_id
                ],
            )
            .map_err(sqlite_error)?;
        }
        Ok(self.events)
    }
}

/// The run's move after its claims changed (`changed`) to `union`, in the caller's transaction: a working or
/// WAITING_EXTERNAL run with claims open is BLOCKED (its decision-class dispatches withdrawn, `run.blocked` with
/// `detail`); a BLOCKED run whose claims are all released goes back to its resume state (`Change::Unblock`); a
/// BLOCKED run whose set changed has its revision moved. Other states move nothing.
#[allow(clippy::too_many_arguments)]
pub(super) fn follow(
    tx: &rusqlite::Transaction<'_>,
    project_id: &str,
    run_id: &str,
    changed: bool,
    union: &[BlockedReason],
    at: &str,
    detail: &str,
    ctx: &Ctx<'_>,
) -> Result<Commit, PersistError> {
    let mut commit = Commit::default();
    let (state, revision, resume): (String, i64, Option<String>) = tx
        .query_row(
            "SELECT state, revision, resume_state FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?;
    let state = writer::parse::<RunState>(&state)?;
    let event = |id: String, kind: &str, payload: serde_json::Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id: id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let working = crate::lifecycle::is_working(state) || state == RunState::WaitingExternal;
    if !union.is_empty() && working {
        let (moves, withdrawn) = super::control::withdraw(
            tx,
            project_id,
            run_id,
            at,
            super::control::Withdraw::Blocked,
            ctx,
        )?;
        commit.cas.extend(moves);
        commit.events.extend(withdrawn);
        commit.cas.push(StateCas {
            id: run_id.to_owned(),
            expected_revision: revision,
            change: Change::Run(RunState::Blocked),
        });
        commit.events.push(event(
            child_id(run_id, &format!("blocked/{revision}")),
            "run.blocked",
            serde_json::json!({"reasons": union.iter().map(|r| r.as_str()).collect::<Vec<_>>(), "detail": detail}),
        ));
    } else if state == RunState::Blocked && changed {
        if union.is_empty() {
            let to = resume
                .as_deref()
                .map(writer::parse::<RunState>)
                .transpose()?
                .unwrap_or(RunState::Running);
            commit.cas.push(StateCas {
                id: run_id.to_owned(),
                expected_revision: revision,
                change: Change::Unblock(to),
            });
            commit.events.push(event(
                child_id(run_id, &format!("resumed/{revision}")),
                "run.resumed",
                serde_json::json!({"from": state.as_str(), "to": to.as_str(), "due": []}),
            ));
        } else {
            commit.cas.push(StateCas {
                id: run_id.to_owned(),
                expected_revision: revision,
                change: Change::Touch(writer::Entity::Run),
            });
        }
    }
    Ok(commit)
}

#[cfg(test)]
mod tests;
