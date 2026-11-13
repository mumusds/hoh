//! The join at a candidate's verification barrier (docs/adr/ADR-029.md decision 11, J3, J8, J9; ADR-028 I4, I5): in
//! the reconcile's transaction, once every round on the candidate is closed and every development task of them not
//! cancelled waits at a passable current round. It classifies the candidate as a whole with the rounds' one function,
//! over each reverification chain's current round's CURRENT reports and the candidate's whole history of checks and
//! rows, and then accepts the candidate and its participants together (when the gate's predicate agrees), rejects it
//! and reworks its owners, verifies each chain again, or waits for a decision on a flaky criterion.

use super::closing::{
    self, Inputs, Item, Reconciled, Round, Scoped, ac_rows, candidate_state, current, exists,
    run_of, task_criteria, task_state,
};
use super::verification::Source;
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::protocol::TaskState;
use crate::verification::classify::{self, Class};
use rusqlite::{Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// What one join did: its id (`child_id(candidate, "join/<rounds on it>")`, the barrier's), its class and its outcome
/// (`accepted`, `refused`, `rejected`, `reverify`, `flaky`, or `skipped` when its candidate was no longer VERIFYING).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Joined {
    pub join_id: String,
    pub class: Class,
    pub outcome: &'static str,
}

/// A participant of a join: a development task of a round on the candidate, not cancelled, and its revision.
struct Participant {
    task: String,
    revision: i64,
}

/// Join candidate `candidate` of run `run_id` at its barrier (the caller holds it: every round on it closed, every
/// participant VERIFYING at a passable current round on it with nothing left to consume), `all` the run's rounds.
/// Once per barrier number: none when its `verification.joined` was written (J9). Its class is
/// `classify(inputs)` over: QA and Review by `report_status` over the participants' current rounds' CURRENT reports;
/// the checks over every planned-check evidence on the candidate; the union of those rounds' scopes, each AC over every
/// row on the candidate; the open Issues of every round on it; the FAIL rows of ACs outside the scope; and whether a
/// critical or high Issue of the participants' confirm set, recomputed now, is unresolved. Then: passable, the
/// candidate and its participants ACCEPTED when the gate's predicate (`requirements_passed`, the run's one candidate
/// in use) holds for each, else an `integrity_violation` and the refused ones BLOCKED for a decision; failed, the
/// candidate REJECTED, its owned items registered (I6) with their acceptance Issues, and consumed at once; inconclusive,
/// each current round's chain verified again; flaky, every participant BLOCKED for a decision. A candidate no longer
/// VERIFYING (no barrier reaches it) is only an `integrity_violation` and a skipped join; the rest of the transaction
/// goes on.
pub(super) fn join(
    tx: &Transaction<'_>,
    run_id: &str,
    candidate: &str,
    all: &[Round],
    ctx: &Ctx<'_>,
    done: &mut Reconciled,
) -> Result<Option<Joined>, PersistError> {
    let on: Vec<&Round> = all.iter().filter(|r| r.candidate == candidate).collect();
    let join_id = child_id(candidate, &format!("join/{}", on.len()));
    if exists(tx, &child_id(&join_id, "joined"))? {
        return Ok(None);
    }
    let (project_id, spec_id, _) = run_of(tx, run_id)?;
    let spec_id = spec_id.unwrap_or_default();
    // The Issues as the facts say before they are read (decision 10's materialisation; the unconfirmed set below is
    // evaluated directly all the same).
    super::issues::settle_all(tx, run_id)?;
    let mut participants = Vec::new();
    let sources: BTreeSet<&str> = on
        .iter()
        .flat_map(|r| r.sources.iter().map(String::as_str))
        .collect();
    for task in sources {
        if let Some((state, revision, _)) = task_state(tx, task)?
            && state != TaskState::Cancelled
        {
            participants.push(Participant {
                task: task.to_owned(),
                revision,
            });
        }
    }
    let tasks: Vec<String> = participants.iter().map(|p| p.task.clone()).collect();
    // J: each participant's current round, each chain's (CR-018).
    let mut current_rounds: Vec<&Round> = Vec::new();
    for p in &participants {
        if let Some(round) = current(all, &p.task).filter(|r| r.candidate == candidate)
            && !current_rounds.iter().any(|r| r.id == round.id)
        {
            current_rounds.push(round);
        }
    }
    current_rounds.sort_by_key(|r| r.seq);
    let mut attempts = Vec::new();
    for round in &current_rounds {
        attempts.push(closing::attempts(tx, round)?);
    }
    let reports = [
        super::reports::report_status(tx, &attempts, "qa")?,
        super::reports::report_status(tx, &attempts, "reviewer")?,
    ];
    let evidence = closing::candidate_evidence(tx, candidate)?;
    let scope: BTreeSet<String> = current_rounds
        .iter()
        .flat_map(|r| r.scope.iter().cloned())
        .collect();
    let mut scoped = Vec::new();
    for ac in &scope {
        let rows = ac_rows(tx, candidate, ac, Some(&spec_id))?;
        scoped.push(Scoped {
            ac: ac.clone(),
            degradable: rows.iter().any(|r| r.degradable),
            rows: rows
                .into_iter()
                .map(|r| (r.status, r.evidence, r.reason))
                .collect(),
        });
    }
    let outside = outside(tx, candidate, &spec_id, &scope)?;
    let mut issues: BTreeSet<(String, String)> = BTreeSet::new();
    for round in &on {
        issues.extend(closing::round_issues(tx, round)?);
    }
    let unconfirmed = !super::issues::unresolved_grave(tx, run_id, &tasks, candidate)?.is_empty();
    let (input, failed) = closing::judge(
        tx,
        Inputs {
            reports,
            evidence: &evidence,
            scoped,
            outside,
            failing_issues: !issues.is_empty(),
            still: Vec::new(),
            unconfirmed,
        },
    )?;
    let class = classify::classify(&input);
    let at = Joining {
        project_id: &project_id,
        run_id,
        candidate,
        join_id: &join_id,
        payload: json!({"candidate_id": candidate, "join_id": join_id,
            "class": format!("{class:?}").to_lowercase(), "inputs": closing::inputs_json(&input),
            "rounds": current_rounds.iter().map(|r| r.id.clone()).collect::<Vec<_>>(), "tasks": tasks}),
    };
    let outcome = match class {
        Class::Passable => accept(tx, &at, &spec_id, &participants, ctx)?,
        Class::Failed => {
            let mut items: BTreeSet<Item> = BTreeSet::new();
            let to_all = |items: &mut BTreeSet<Item>, item: &str| {
                for p in &participants {
                    items.insert(Item {
                        task_id: p.task.clone(),
                        item: item.to_owned(),
                    });
                }
            };
            for (_, fingerprint) in &issues {
                to_all(&mut items, &format!("issue:{fingerprint}"));
            }
            for (ac, item) in &failed {
                // An AC's item is the participants' whose scope has it (I6); one no participant's scope has (a cancelled
                // source's) is every participant's, so a failed join always has an owned item.
                let mut owners = Vec::new();
                if let Some(ac) = ac {
                    for p in &participants {
                        if task_criteria(tx, &p.task, Some(&spec_id))?.contains(ac) {
                            owners.push(p.task.clone());
                        }
                    }
                }
                if owners.is_empty() {
                    to_all(&mut items, item);
                }
                for task_id in owners {
                    items.insert(Item {
                        task_id,
                        item: item.clone(),
                    });
                }
            }
            reject(tx, &at, &items, ctx, done)?
        }
        Class::Inconclusive => reverify(tx, &at, &participants, &current_rounds, all, ctx, done)?,
        Class::Flaky => flaky(tx, &at, &participants, all, ctx)?,
    };
    Ok(Some(Joined {
        join_id,
        class,
        outcome,
    }))
}

/// The join being written: its run, candidate and id, its class, and its `verification.joined` payload (its outcome
/// added when known).
struct Joining<'a> {
    project_id: &'a str,
    run_id: &'a str,
    candidate: &'a str,
    join_id: &'a str,
    payload: Value,
}

impl Joining<'_> {
    fn event(&self, event_id: String, kind: &str, payload: Value) -> NewEvent {
        NewEvent {
            project_id: self.project_id.to_owned(),
            event_id,
            run_id: Some(self.run_id.to_owned()),
            kind: kind.to_owned(),
            payload,
        }
    }

    /// Its `verification.joined` with `outcome` (J9: written whatever the outcome).
    fn joined(&self, outcome: &str) -> NewEvent {
        let mut payload = self.payload.clone();
        payload["outcome"] = json!(outcome);
        self.event(
            child_id(self.join_id, "joined"),
            "verification.joined",
            payload,
        )
    }

    /// A join whose candidate is no longer VERIFYING (-61): `integrity_violation{join_candidate_not_verifying}` and its
    /// `verification.joined{outcome: skipped}`, no move.
    fn skipped(&self, tx: &Transaction<'_>, ctx: &Ctx<'_>) -> Result<&'static str, PersistError> {
        writer::write(
            tx,
            &Commit {
                events: vec![
                    self.event(
                        child_id(self.join_id, "not_verifying"),
                        "integrity_violation",
                        json!({"candidate_id": self.candidate, "join_id": self.join_id,
                            "reason": "join_candidate_not_verifying"}),
                    ),
                    self.joined("skipped"),
                ],
                ..Commit::default()
            },
            ctx,
        )?;
        Ok("skipped")
    }

    /// Whether its candidate is VERIFYING now.
    fn verifying(&self, tx: &Transaction<'_>) -> Result<bool, PersistError> {
        Ok(candidate_state(tx, self.candidate)?.as_deref() == Some("VERIFYING"))
    }
}

/// The FAIL rows on `candidate` under `spec_id` of ACs outside `scope`: (AC, the row as its evidence recorded it, its
/// reason).
fn outside(
    tx: &Transaction<'_>,
    candidate: &str,
    spec_id: &str,
    scope: &BTreeSet<String>,
) -> Result<Vec<(String, Value, String)>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT ac_id, evidence_id, coalesce(reason, '') FROM acceptance_results \
             WHERE candidate_id = ?1 AND spec_id = ?2 AND status = 'FAIL' ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(String, String, String)> = statement
        .query_map(params![candidate, spec_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for (ac, evidence, reason) in rows {
        if !scope.contains(&ac) {
            let row = closing::row_of_evidence(tx, &evidence, &ac)?;
            out.push((ac, row, reason));
        }
    }
    Ok(out)
}

/// A passable join (J3): each participant's requirements under the run's specification passed on the candidate
/// (`requirements_passed`, docs/adr/ADR-029.md decision 12) and no other candidate of the run in use; then the
/// candidate (its state the condition) and every participant ACCEPTED in this transaction, one iteration. Otherwise
/// (fail-closed, never partly): `integrity_violation{join_gate_disagrees}` and each refused participant BLOCKED for a
/// decision (`join_gate_refused`), the candidate VERIFYING.
fn accept(
    tx: &Transaction<'_>,
    at: &Joining<'_>,
    spec_id: &str,
    participants: &[Participant],
    ctx: &Ctx<'_>,
) -> Result<&'static str, PersistError> {
    let passed = super::decide::requirements_passed(tx, at.run_id, spec_id, at.candidate)?;
    let others: i64 = tx
        .query_row(
            "SELECT count(*) FROM candidates WHERE run_id = ?1 AND candidate_id <> ?2 \
             AND state IN ('FROZEN', 'VERIFYING', 'ACCEPTED')",
            params![at.run_id, at.candidate],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let mut refused = Vec::new();
    for p in participants {
        let mut statement = tx
            .prepare(
                "SELECT requirement_id FROM task_requirements WHERE task_id = ?1 AND spec_id = ?2",
            )
            .map_err(sqlite_error)?;
        let needed: BTreeSet<String> = statement
            .query_map(params![p.task, spec_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        if others > 0 || needed.is_empty() || !needed.is_subset(&passed) {
            refused.push(p);
        }
    }
    if !refused.is_empty() {
        let names: Vec<&str> = refused.iter().map(|p| p.task.as_str()).collect();
        let mut commit = Commit {
            events: vec![
                at.event(
                    child_id(at.join_id, "gate_disagrees"),
                    "integrity_violation",
                    json!({"candidate_id": at.candidate, "join_id": at.join_id, "tasks": names,
                        "reason": "join_gate_disagrees"}),
                ),
                at.joined("refused"),
            ],
            ..Commit::default()
        };
        for p in refused {
            tx.execute(
                "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
                params![p.task],
            )
            .map_err(sqlite_error)?;
            commit.cas.push(StateCas {
                id: p.task.clone(),
                expected_revision: p.revision,
                change: Change::Task(TaskState::Blocked),
            });
            commit.events.push(at.event(
                child_id(at.join_id, &format!("gate_refused/{}", p.task)),
                "need_decision",
                json!({"run_id": at.run_id, "task_id": p.task, "candidate_id": at.candidate,
                    "join_id": at.join_id, "reason": "join_gate_refused",
                    "detail": "the join was passable but the gate's predicate refuses the task's requirements on \
                        this candidate: a reverification or a rework is a decision"}),
            ));
        }
        writer::write(tx, &commit, ctx)?;
        return Ok("refused");
    }
    let changed = tx
        .execute(
            "UPDATE candidates SET state = 'ACCEPTED' WHERE candidate_id = ?1 AND state = 'VERIFYING'",
            params![at.candidate],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return at.skipped(tx, ctx);
    }
    let mut commit = Commit {
        events: vec![
            at.joined("accepted"),
            at.event(
                child_id(at.candidate, "accepted"),
                "candidate.accepted",
                json!({"candidate_id": at.candidate, "join_id": at.join_id}),
            ),
        ],
        ..Commit::default()
    };
    for p in participants {
        commit.cas.push(StateCas {
            id: p.task.clone(),
            expected_revision: p.revision,
            change: Change::Task(TaskState::Accepted),
        });
        commit.events.push(at.event(
            child_id(at.join_id, &format!("accepted/{}", p.task)),
            "task.accepted",
            json!({"task_id": p.task, "candidate_id": at.candidate, "join_id": at.join_id, "by": "runtime"}),
        ));
    }
    writer::write(tx, &commit, ctx)?;
    closing::iteration(
        tx,
        (at.project_id, at.run_id),
        at.candidate,
        "passable",
        ctx,
    )?;
    Ok("accepted")
}

/// A failed join (J8): the candidate REJECTED (its state the condition), `verification.failures{round_id: join_id}`
/// with its owned items, an acceptance Issue per AC item (decision 9), one iteration (repair), and the items consumed
/// in this transaction: a VERIFYING owner is reworked at once.
fn reject(
    tx: &Transaction<'_>,
    at: &Joining<'_>,
    items: &BTreeSet<Item>,
    ctx: &Ctx<'_>,
    done: &mut Reconciled,
) -> Result<&'static str, PersistError> {
    let changed = tx
        .execute(
            "UPDATE candidates SET state = 'REJECTED' WHERE candidate_id = ?1 AND state = 'VERIFYING'",
            params![at.candidate],
        )
        .map_err(sqlite_error)?;
    if changed != 1 {
        return at.skipped(tx, ctx);
    }
    let mut events = vec![
        at.joined("rejected"),
        at.event(
            child_id(at.candidate, "rejected"),
            "candidate.rejected",
            json!({"candidate_id": at.candidate, "join_id": at.join_id}),
        ),
    ];
    if !items.is_empty() {
        events.push(at.event(
            child_id(at.join_id, "failures"),
            "verification.failures",
            json!({"round_id": at.join_id, "candidate_id": at.candidate, "items": items.iter().map(|i| json!({
                "task_id": i.task_id, "item": i.item, "fingerprint": i.fingerprint(),
            })).collect::<Vec<_>>()}),
        ));
    }
    writer::write(
        tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    super::issues::open_acceptance(
        tx,
        at.run_id,
        at.candidate,
        &super::issues::ac_failures(items.iter().map(|i| i.item.as_str())),
    )?;
    closing::iteration(tx, (at.project_id, at.run_id), at.candidate, "repair", ctx)?;
    closing::consume(tx, at.run_id, at.project_id, false, ctx, done)?;
    Ok("rejected")
}

/// An inconclusive join (UNKNOWN never reworks): each current round's sources among the participants verified again
/// on its chain, the action `child_id(join_id, "reverify/<round>")` (the rounds' exit's function), and the round's next
/// round opened for those granted; a refusal is a decision.
fn reverify(
    tx: &Transaction<'_>,
    at: &Joining<'_>,
    participants: &[Participant],
    current_rounds: &[&Round],
    all: &[Round],
    ctx: &Ctx<'_>,
    done: &mut Reconciled,
) -> Result<&'static str, PersistError> {
    if !at.verifying(tx)? {
        return at.skipped(tx, ctx);
    }
    writer::write(
        tx,
        &Commit {
            events: vec![at.joined("reverify")],
            ..Commit::default()
        },
        ctx,
    )?;
    for round in current_rounds {
        let action = child_id(at.join_id, &format!("reverify/{}", round.id));
        let mut granted = Vec::new();
        for p in participants
            .iter()
            .filter(|p| current(all, &p.task).is_some_and(|r| r.id == round.id))
        {
            let ok = closing::reverify_source(
                tx,
                (at.project_id, at.run_id),
                round,
                (&p.task, p.revision),
                closing::Reverification {
                    action: &action,
                    join: Some(at.join_id),
                    detail: format!(
                        "the join {} of candidate {} was inconclusive",
                        at.join_id, at.candidate
                    ),
                },
                ctx,
            )?;
            if ok {
                granted.push(Source {
                    task_id: p.task.clone(),
                    revision: p.revision,
                });
            }
        }
        if !granted.is_empty() {
            done.opened.push(super::verification::reverify(
                tx,
                at.run_id,
                at.candidate,
                &round.id,
                &round.chain_root,
                granted,
                ctx,
            )?);
        }
    }
    Ok("reverify")
}

/// A flaky join: every participant BLOCKED for a decision (`need_decision{reason: flaky, candidate_id, join_id}`, the
/// rounds' exit's function); retesting the candidate cannot satisfy it (REQ-053 decision 1).
fn flaky(
    tx: &Transaction<'_>,
    at: &Joining<'_>,
    participants: &[Participant],
    all: &[Round],
    ctx: &Ctx<'_>,
) -> Result<&'static str, PersistError> {
    if !at.verifying(tx)? {
        return at.skipped(tx, ctx);
    }
    writer::write(
        tx,
        &Commit {
            events: vec![at.joined("flaky")],
            ..Commit::default()
        },
        ctx,
    )?;
    for p in participants {
        let round = current(all, &p.task).map(|r| r.id.clone());
        let deferred = match &round {
            Some(round) => super::verification::deferred_of(tx, round)?,
            None => Vec::new(),
        };
        closing::block_flaky(
            tx,
            (at.project_id, at.run_id),
            (&p.task, p.revision),
            at.join_id,
            json!({"candidate_id": at.candidate, "join_id": at.join_id, "round_id": round}),
            deferred,
            ctx,
        )?;
    }
    Ok("flaky")
}

#[cfg(test)]
mod tests;
