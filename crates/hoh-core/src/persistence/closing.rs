//! The verification reconcile (docs/adr/ADR-028.md decision 11): level-triggered, one transaction per run each tick
//! and at the host's start, reading only the state as it is now, idempotent. In order: (1) a round whose candidate is
//! no longer under verification (or whose run is being cancelled) has its unsettled verifier tasks cancelled; (2) a
//! round whose two verifier tasks are settled is closed with its verdict (superseded, invalidated, or the class of
//! `verification::classify`), its failures registered with their owners (I6), its candidate REJECTED when failed;
//! (2a) each development task with failure items not yet consumed takes them by its state (rework, attached and
//! counted, appended to its decision, or abandoned); (3) each VERIFYING source task whose current round is closed goes
//! where the exit table says (another round, a decision, waiting for a sibling, or back to be frozen again); (4) a
//! candidate every one of whose development tasks waits at a passable round is ready for its join (M2-T18's).

use super::retry::{Repair, RepairRequest, RepairVerdict};
use super::verification::{ROUND_CLOSED, ROUND_OPENED, Source};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::TaskState;
use crate::verification::classify::{self, Ac, Checks, Class, Report, Row};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The outcome reasons a FAIL is no conclusion with (decision 11's inconclusive set).
const INCONCLUSIVE: [&str; 5] = [
    "TIMED_OUT",
    "NOT_STARTED",
    "INTERRUPTED",
    "INCOMPLETE",
    "PLUGINS_MISSING",
];

/// What one reconcile did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub cancelled: Vec<String>,
    /// The rounds closed, with their verdicts.
    pub closed: Vec<(String, String)>,
    pub reworked: Vec<String>,
    pub opened: Vec<String>,
    pub joinable: Vec<String>,
}

/// A round as its `verification.round_opened` recorded it.
#[derive(Debug, Clone)]
struct Round {
    id: String,
    candidate: String,
    sources: Vec<String>,
    scope: Vec<String>,
    qa: String,
    reviewer: String,
    chain_root: String,
    seq: i64,
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn rounds(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<Round>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT seq, payload_json FROM events WHERE kind = ?1 AND run_id = ?2 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
    for row in rows {
        let (seq, payload) = row.map_err(sqlite_error)?;
        let p: Value = serde_json::from_str(&payload)
            .map_err(|e| PersistError::IntegrityViolation(format!("round_opened: {e}")))?;
        let id = p["round_id"].as_str().unwrap_or_default().to_owned();
        out.push(Round {
            candidate: p["candidate_id"].as_str().unwrap_or_default().to_owned(),
            sources: strings(&p["source_task_ids"]),
            scope: strings(&p["acceptance_ids"]),
            qa: p["tasks"]["qa"].as_str().unwrap_or_default().to_owned(),
            reviewer: p["tasks"]["reviewer"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            chain_root: p["chain_root"]
                .as_str()
                .map_or_else(|| id.clone(), str::to_owned),
            id,
            seq,
        });
    }
    Ok(out)
}

/// The verdict of each closed round of run `run_id`.
fn closed(tx: &Transaction<'_>, run_id: &str) -> Result<BTreeMap<String, String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT json_extract(payload_json, '$.round_id'), json_extract(payload_json, '$.verdict') FROM events \
             WHERE kind = ?1 AND run_id = ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_CLOSED, run_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

fn task_state(
    tx: &Transaction<'_>,
    task_id: &str,
) -> Result<Option<(TaskState, i64, Option<String>)>, PersistError> {
    let row: Option<(String, i64, Option<String>)> = tx
        .query_row(
            "SELECT state, revision, blocked_reason FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(|(s, r, b)| Ok((writer::parse::<TaskState>(&s)?, r, b)))
        .transpose()
}

fn candidate_state(tx: &Transaction<'_>, candidate: &str) -> Result<Option<String>, PersistError> {
    tx.query_row(
        "SELECT state FROM candidates WHERE candidate_id = ?1",
        params![candidate],
        |r| r.get(0),
    )
    .optional()
    .map_err(sqlite_error)
}

/// Candidate `candidate` has a persistent fact that it was superseded: a later freeze of its run listed it, or
/// `candidate.stale` names it (a specification change, evidence found unrecoverable); later vetoes do not change it.
fn superseded(tx: &Transaction<'_>, run_id: &str, candidate: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE kind = 'candidate.stale' \
           AND json_extract(payload_json, '$.candidate_id') = ?2) \
         OR EXISTS(SELECT 1 FROM events e, json_each(e.payload_json, '$.superseded') s \
           WHERE e.kind = 'candidate.frozen' AND e.run_id = ?1 AND s.value = ?2)",
        params![run_id, candidate],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The attempts of round `round`'s verifier tasks.
fn attempts(tx: &Transaction<'_>, round: &Round) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare("SELECT attempt_id FROM attempts WHERE task_id IN (?1, ?2) ORDER BY rowid")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![round.qa, round.reviewer], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// One evidence of a round's verifier attempts.
#[derive(Debug, Clone)]
struct Evidence {
    id: String,
    check_id: String,
    outcome: String,
    reason: Option<String>,
    reasons: Vec<String>,
    rows: Vec<Value>,
    deferred: bool,
}

/// The planned-check evidence (kind `check`, never an attempt-level marker record) of `attempts`.
fn evidence(tx: &Transaction<'_>, attempts: &[String]) -> Result<Vec<Evidence>, PersistError> {
    let mut out = Vec::new();
    for attempt in attempts {
        let mut statement = tx
            .prepare(
                "SELECT ev.evidence_id, ev.check_id, ev.outcome, \
                 (SELECT payload_json FROM events WHERE event_id = ev.evidence_id || '/evidence') \
                 FROM evidence ev WHERE ev.attempt_id = ?1 AND ev.kind = 'check' ORDER BY ev.rowid",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![attempt], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, Option<String>>(3)?,
                ))
            })
            .map_err(sqlite_error)?;
        for row in rows {
            let (id, check_id, outcome, payload) = row.map_err(sqlite_error)?;
            let p: Value = payload
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or(Value::Null);
            out.push(Evidence {
                id,
                check_id,
                outcome,
                reason: p["outcome_reason"].as_str().map(str::to_owned),
                reasons: strings(&p["reasons"]),
                rows: p["rows"].as_array().cloned().unwrap_or_default(),
                deferred: p["deferred_rows"] == json!(true),
            });
        }
    }
    Ok(out)
}

/// The status of the latest report of `role` among `attempts` (decision 15): NOT_RUN without one.
fn report(tx: &Transaction<'_>, attempts: &[String], role: &str) -> Result<Report, PersistError> {
    let mut status = None;
    for attempt in attempts {
        let found: Option<String> = tx
            .query_row(
                "SELECT json_extract(payload_json, '$.status') FROM events WHERE kind = 'verification.report' \
                 AND json_extract(payload_json, '$.attempt_id') = ?1 AND json_extract(payload_json, '$.role') = ?2 \
                 ORDER BY seq DESC LIMIT 1",
                params![attempt, role],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if found.is_some() {
            status = found;
        }
    }
    Ok(match status.as_deref() {
        Some("PASS") => Report::Pass,
        Some("FAIL") => Report::Fail,
        Some("UNKNOWN") => Report::Unknown,
        _ => Report::NotRun,
    })
}

fn row_of(status: &str) -> Row {
    match status {
        "PASS" => Row::Pass,
        "DEGRADED" => Row::Degraded,
        "FAIL" => Row::Fail,
        "BLOCKED" => Row::Blocked,
        "FLAKY" => Row::Flaky,
        "NOT_RUN" => Row::NotRun,
        "INVALID" => Row::Invalid,
        "STALE" => Row::Stale,
        _ => Row::Unknown,
    }
}

/// The open Issues of round `round` (decision 13's source predicate, so far the change requests its verifiers made):
/// (issue id, fingerprint).
fn round_issues(
    tx: &Transaction<'_>,
    round: &Round,
) -> Result<Vec<(String, String)>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT i.issue_id, i.fingerprint FROM issues i WHERE i.state IN ('OPEN', 'REOPENED') AND i.issue_id IN \
             (SELECT json_extract(payload_json, '$.issue_id') FROM events WHERE kind = 'issue.opened' \
              AND json_extract(payload_json, '$.round_id') = ?1) ORDER BY i.issue_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![round.id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The run's spec id and project.
fn run_of(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<(String, Option<String>, String), PersistError> {
    tx.query_row(
        "SELECT project_id, spec_id, state FROM runs WHERE run_id = ?1",
        params![run_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .map_err(sqlite_error)
}

/// A failure item and its owner.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Item {
    task_id: String,
    item: String,
}

impl Item {
    fn fingerprint(&self) -> String {
        crate::context::sha256_hex(format!("{}\n{}", self.task_id, self.item).as_bytes())
    }
}

/// Reconcile run `run_id`'s verification in one transaction; `pending` holds the candidates with check records still
/// to be recorded (`.hoh/pending/`), whose rounds wait for them unless superseded (M2T16D-601, -1101).
pub(super) fn reconcile(
    conn: &mut rusqlite::Connection,
    run_id: &str,
    pending: &BTreeSet<String>,
    ctx: &Ctx<'_>,
) -> Result<Reconciled, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let mut done = Reconciled::default();
    let (project_id, spec_id, run_state) = run_of(&tx, run_id)?;
    let cancelling = matches!(run_state.as_str(), "CANCEL_REQUESTED" | "CANCELLED");
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.clone(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let all = rounds(&tx, run_id)?;
    let mut verdicts = closed(&tx, run_id)?;
    // (1) A round whose candidate is no longer under verification: its unsettled verifier tasks cancelled.
    for round in all.iter().filter(|r| !verdicts.contains_key(&r.id)) {
        let state = candidate_state(&tx, &round.candidate)?;
        if state.as_deref() == Some("VERIFYING") && !cancelling {
            continue;
        }
        for (role, task) in [("qa", &round.qa), ("reviewer", &round.reviewer)] {
            let Some((state, revision, _)) = task_state(&tx, task)? else {
                continue;
            };
            if !matches!(
                state,
                TaskState::Ready | TaskState::Blocked | TaskState::Pending
            ) {
                continue;
            }
            tx.execute(
                "UPDATE tasks SET blocked_reason = NULL, next_retry_at = NULL WHERE task_id = ?1",
                params![task],
            )
            .map_err(sqlite_error)?;
            let mut events = vec![event(
                child_id(task, "cancelled"),
                "task.cancelled",
                json!({"task_id": task, "round_id": round.id, "reason": "superseded"}),
            )];
            // Its handoff never taken: DEAD_LETTER, superseded.
            let mut statement = tx
                .prepare(
                    "SELECT message_id FROM outbox WHERE recipient_role = ?1 AND state IN ('PENDING', 'DELIVERING') \
                     AND json_extract(payload_json, '$.payload.handoff_kind') = 'verification' \
                     AND (message_id = ?2 OR message_id IN (SELECT json_extract(payload_json, '$.to') FROM events \
                       WHERE kind = 'verification.handoff_reissued' AND json_extract(payload_json, '$.round_id') = ?3))",
                )
                .map_err(sqlite_error)?;
            let handoffs: Vec<String> = statement
                .query_map(
                    params![
                        role,
                        child_id(&round.id, &format!("verify/{role}")),
                        round.id
                    ],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?;
            for message in handoffs {
                tx.execute(
                    "UPDATE outbox SET state = 'DEAD_LETTER', last_error = 'superseded' WHERE message_id = ?1",
                    params![message],
                )
                .map_err(sqlite_error)?;
                events.push(event(
                    child_id(&message, "dead_letter"),
                    "outbox.dead_letter",
                    json!({"message_id": message, "recipient_role": role, "reason": "superseded"}),
                ));
            }
            writer::write(
                &tx,
                &Commit {
                    cas: vec![StateCas {
                        id: task.clone(),
                        expected_revision: revision,
                        change: Change::Task(TaskState::Cancelled),
                    }],
                    events,
                    ..Commit::default()
                },
                ctx,
            )?;
            done.cancelled.push(task.clone());
        }
    }
    // (2) Rounds to close: both verifier tasks settled.
    for round in &all {
        if verdicts.contains_key(&round.id) {
            continue;
        }
        let settled = |task: &str| -> Result<bool, PersistError> {
            Ok(task_state(&tx, task)?.is_some_and(|(s, _, _)| {
                matches!(s, TaskState::Implemented | TaskState::Cancelled)
            }))
        };
        if !settled(&round.qa)? || !settled(&round.reviewer)? {
            continue;
        }
        let was_superseded = superseded(&tx, run_id, &round.candidate)?;
        if pending.contains(&round.candidate) && !was_superseded {
            continue;
        }
        let candidate = candidate_state(&tx, &round.candidate)?.unwrap_or_default();
        let attempts = attempts(&tx, round)?;
        let evidence = evidence(&tx, &attempts)?;
        let vetoed: Vec<&Evidence> = evidence.iter().filter(|e| e.outcome == "INVALID").collect();
        let verdict =
            if was_superseded || cancelling || (candidate == "INVALID" && vetoed.is_empty()) {
                "superseded"
            } else if !vetoed.is_empty() {
                "invalidated"
            } else {
                ""
            };
        // This round's acceptance rows, written now (decision 11): PASS and DEGRADED left out of a vetoed candidate.
        let skip_passes = candidate == "INVALID" || !vetoed.is_empty();
        if let Some(spec) = &spec_id {
            for e in evidence.iter().filter(|e| e.deferred) {
                for row in &e.rows {
                    let status = row["status"].as_str().unwrap_or_default();
                    if skip_passes && matches!(status, "PASS" | "DEGRADED") {
                        continue;
                    }
                    tx.execute(
                        "INSERT OR IGNORE INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status, \
                         reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![
                            row["ac_id"].as_str().unwrap_or_default(),
                            row["spec_id"].as_str().unwrap_or(spec),
                            round.candidate,
                            e.id,
                            status,
                            row["reason"].as_str().unwrap_or_default()
                        ],
                    )
                    .map_err(sqlite_error)?;
                }
            }
        }
        let issues = round_issues(&tx, round)?;
        let mut items: BTreeSet<Item> = BTreeSet::new();
        let to_all = |items: &mut BTreeSet<Item>, item: String| {
            for task in &round.sources {
                items.insert(Item {
                    task_id: task.clone(),
                    item: item.clone(),
                });
            }
        };
        // Whatever the verdict, the round's open Issues are its sources' failure items (decision 11, -801).
        for (_, fingerprint) in &issues {
            to_all(&mut items, format!("issue:{fingerprint}"));
        }
        let (verdict, class, inputs) = if verdict.is_empty() {
            let (input, failed) = classify_round(
                &tx,
                round,
                run_id,
                spec_id.as_deref(),
                &attempts,
                &evidence,
                &issues,
            )?;
            let class = classify::classify(&input);
            if class == Class::Failed {
                for item in failed {
                    match item {
                        (Some(ac), text) => {
                            // An in-scope AC's FAIL belongs to the sources whose scope holds it.
                            for task in &round.sources {
                                if task_criteria(&tx, task, spec_id.as_deref())?.contains(&ac) {
                                    items.insert(Item {
                                        task_id: task.clone(),
                                        item: text.clone(),
                                    });
                                }
                            }
                        }
                        (None, text) => to_all(&mut items, text),
                    }
                }
            }
            let verdict = match class {
                Class::Failed => "repair",
                Class::Inconclusive => "reverify",
                Class::Flaky => "flaky",
                Class::Passable => "passable",
            };
            (
                verdict.to_owned(),
                Some(class),
                json!({
                    "qa": format!("{:?}", input.qa), "review": format!("{:?}", input.review),
                    "checks": format!("{:?}", input.checks),
                    "acs": input.acs.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>(),
                    "failing_issues": input.failing_issues,
                }),
            )
        } else {
            if verdict == "invalidated" {
                for e in &vetoed {
                    to_all(
                        &mut items,
                        format!("veto:{}:{}", e.check_id, e.reasons.join(",")),
                    );
                }
            }
            (verdict.to_owned(), None, Value::Null)
        };
        let mut events = vec![event(
            child_id(&round.id, "closed"),
            ROUND_CLOSED,
            json!({"round_id": round.id, "candidate_id": round.candidate, "verdict": verdict,
                "class": class.map(|c| format!("{c:?}").to_lowercase()), "inputs": inputs,
                "source_task_ids": round.sources, "candidate_state": candidate}),
        )];
        if !items.is_empty() {
            events.push(event(
                child_id(&round.id, "failures"),
                "verification.failures",
                json!({"round_id": round.id, "candidate_id": round.candidate, "items": items.iter().map(|i| json!({
                    "task_id": i.task_id, "item": i.item, "fingerprint": i.fingerprint(),
                })).collect::<Vec<_>>()}),
            ));
        }
        if verdict == "repair" && candidate == "VERIFYING" {
            tx.execute(
                "UPDATE candidates SET state = 'REJECTED' WHERE candidate_id = ?1 AND state = 'VERIFYING'",
                params![round.candidate],
            )
            .map_err(sqlite_error)?;
            events.push(event(
                child_id(&round.candidate, "rejected"),
                "candidate.rejected",
                json!({"candidate_id": round.candidate, "round_id": round.id}),
            ));
        }
        // Its settled verifier tasks end: IMPLEMENTED -> VERIFYING -> ACCEPTED (their work done; no requirement of theirs).
        let mut cas = Vec::new();
        for task in [&round.qa, &round.reviewer] {
            if let Some((TaskState::Implemented, revision, _)) = task_state(&tx, task)? {
                cas.push(StateCas {
                    id: task.clone(),
                    expected_revision: revision,
                    change: Change::Task(TaskState::Verifying),
                });
                cas.push(StateCas {
                    id: task.clone(),
                    expected_revision: revision + 1,
                    change: Change::Task(TaskState::Accepted),
                });
            }
        }
        // Its handoffs: ACCEPTED for a verifier that completed, REJECTED when the round was superseded.
        let to = if verdict == "superseded" {
            "REJECTED"
        } else {
            "ACCEPTED"
        };
        tx.execute(
            "UPDATE handoffs SET state = ?1 WHERE state = 'DELIVERED' AND (handoff_id IN (?2, ?3) OR handoff_id IN \
             (SELECT json_extract(payload_json, '$.to') FROM events WHERE kind = 'verification.handoff_reissued' \
              AND json_extract(payload_json, '$.round_id') = ?4))",
            params![
                to,
                child_id(&round.id, "verify/qa"),
                child_id(&round.id, "verify/reviewer"),
                round.id
            ],
        )
        .map_err(sqlite_error)?;
        writer::write(
            &tx,
            &Commit {
                cas,
                events,
                ..Commit::default()
            },
            ctx,
        )?;
        if matches!(verdict.as_str(), "repair" | "invalidated") {
            iteration(&tx, (&project_id, run_id), &round.candidate, &verdict, ctx)?;
        }
        verdicts.insert(round.id.clone(), verdict.clone());
        done.closed.push((round.id.clone(), verdict));
    }
    // (2a) Failure items not yet consumed, by their owner's state.
    consume(&tx, run_id, &project_id, cancelling, ctx, &mut done)?;
    // A refusal not cleared is a standing condition: a task that could be dispatched waits for its decision
    // (decision 12, M2T16D-853).
    for task in super::verification::refused(&tx, run_id)? {
        let Some((state @ (TaskState::Ready | TaskState::Pending), revision, _)) =
            task_state(&tx, &task)?
        else {
            continue;
        };
        tx.execute(
            "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
            params![task],
        )
        .map_err(sqlite_error)?;
        writer::write(
            &tx,
            &Commit {
                cas: vec![StateCas {
                    id: task.clone(),
                    expected_revision: revision,
                    change: Change::Task(TaskState::Blocked),
                }],
                events: vec![event(
                    child_id(&task, &format!("refusal_holds/{revision}")),
                    "task.blocked",
                    json!({"task_id": task, "reason": "NEED_DECISION", "from": state.as_str(),
                        "detail": "a refused repair waits for a decision"}),
                )],
                ..Commit::default()
            },
            ctx,
        )?;
    }
    // (3) and (4): the source tasks' exits and the candidates' barriers.
    exits(
        &tx,
        run_id,
        &project_id,
        &all,
        &verdicts,
        cancelling,
        ctx,
        &mut done,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

/// Task `task_id`'s acceptance criteria under specification `spec_id`.
fn task_criteria(
    tx: &Transaction<'_>,
    task_id: &str,
    spec_id: Option<&str>,
) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT ac.ac_id FROM task_requirements tr JOIN acceptance_criteria ac \
             ON ac.requirement_id = tr.requirement_id AND ac.spec_id = tr.spec_id WHERE tr.task_id = ?1 AND tr.spec_id = ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![task_id, spec_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The classification input of `round` and the failure items each failing input gives: (the in-scope AC it belongs
/// to, if any; the item).
#[allow(clippy::type_complexity)]
fn classify_round(
    tx: &Transaction<'_>,
    round: &Round,
    run_id: &str,
    spec_id: Option<&str>,
    attempts: &[String],
    evidence: &[Evidence],
    issues: &[(String, String)],
) -> Result<(classify::Input, Vec<(Option<String>, String)>), PersistError> {
    let _ = run_id;
    let mut failed = Vec::new();
    let qa = report(tx, attempts, "qa")?;
    let review = report(tx, attempts, "reviewer")?;
    for (role, status) in [("qa", qa), ("reviewer", review)] {
        if status == Report::Fail {
            failed.push((None, format!("report-fail:{role}")));
        }
    }
    let conclusive_fail = |e: &Evidence| {
        e.outcome == "FAIL"
            && !e
                .reason
                .as_deref()
                .is_some_and(|r| INCONCLUSIVE.contains(&r))
    };
    let checks = if evidence.iter().any(conclusive_fail) {
        Checks::Failed
    } else if !evidence.is_empty()
        && evidence
            .iter()
            .all(|e| matches!(e.outcome.as_str(), "PASS" | "DEGRADED"))
    {
        Checks::Passed
    } else {
        Checks::Inconclusive
    };
    // The rows of each in-scope AC on the candidate: those that existed when the round opened and the round's own
    // (a sibling's later rows are its own to judge, I2, -754).
    let own: BTreeSet<&str> = evidence.iter().map(|e| e.id.as_str()).collect();
    let mut acs = Vec::new();
    for ac in &round.scope {
        let mut statement = tx
            .prepare(
                "SELECT ar.status, ar.evidence_id, \
                 coalesce((SELECT seq FROM events WHERE event_id = ar.evidence_id || '/evidence'), 0), \
                 (SELECT r.release_class FROM acceptance_criteria c JOIN requirements r ON r.requirement_id = \
                  c.requirement_id AND r.spec_id = c.spec_id WHERE c.ac_id = ar.ac_id AND c.spec_id = ar.spec_id) \
                 FROM acceptance_results ar WHERE ar.candidate_id = ?1 AND ar.ac_id = ?2 AND ar.spec_id = ?3",
            )
            .map_err(sqlite_error)?;
        let rows: Vec<(String, String, i64, Option<String>)> = statement
            .query_map(params![round.candidate, ac, spec_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let degradable = rows.iter().any(|r| r.3.as_deref() == Some("degradable"));
        let seen: Vec<Row> = rows
            .iter()
            .filter(|(_, evidence, seq, _)| own.contains(evidence.as_str()) || *seq < round.seq)
            .map(|(status, ..)| row_of(status))
            .collect();
        let projected = classify::project(&seen, degradable);
        if projected == Ac::Failed {
            failed.push((Some(ac.clone()), format!("ac-fail:{ac}")));
        }
        acs.push(projected);
    }
    // A FAIL row of an AC outside the scope that this round's evidence wrote: every source's (decision 12).
    for e in evidence {
        for row in &e.rows {
            let ac = row["ac_id"].as_str().unwrap_or_default();
            if row["status"] == json!("FAIL") && !round.scope.iter().any(|s| s == ac) {
                failed.push((None, format!("ac-fail:{ac}")));
            }
        }
        if conclusive_fail(e) {
            failed.push((
                None,
                format!(
                    "check-fail:{}:{}",
                    e.check_id,
                    e.reason.as_deref().unwrap_or("")
                ),
            ));
        }
    }
    Ok((
        classify::Input {
            qa,
            review,
            checks,
            acs,
            failing_issues: !issues.is_empty(),
        },
        failed,
    ))
}

/// A (round, task)'s failure items not yet consumed: the round's candidate and each (item, fingerprint).
type Owed = (String, Vec<(String, String)>);

/// (2a) Every development task's failure items not yet consumed, by its state now (decision 11).
fn consume(
    tx: &Transaction<'_>,
    run_id: &str,
    project_id: &str,
    cancelling: bool,
    ctx: &Ctx<'_>,
    done: &mut Reconciled,
) -> Result<(), PersistError> {
    // (round, task) -> its items, not yet consumed.
    let mut statement = tx
        .prepare(
            "SELECT json_extract(e.payload_json, '$.round_id'), json_extract(e.payload_json, '$.candidate_id'), \
             json_extract(i.value, '$.task_id'), json_extract(i.value, '$.item'), \
             json_extract(i.value, '$.fingerprint') FROM events e, json_each(e.payload_json, '$.items') i \
             WHERE e.kind = 'verification.failures' AND e.run_id = ?1 AND NOT EXISTS (SELECT 1 FROM events c \
               WHERE c.kind = 'verification.failures_consumed' AND c.run_id = ?1 \
               AND json_extract(c.payload_json, '$.round_id') = json_extract(e.payload_json, '$.round_id') \
               AND json_extract(c.payload_json, '$.task_id') = json_extract(i.value, '$.task_id')) \
             ORDER BY e.seq, i.key",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(String, String, String, String, String)> = statement
        .query_map(params![run_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut groups: BTreeMap<(String, String), Owed> = BTreeMap::new();
    for (round, candidate, task, item, fingerprint) in rows {
        groups
            .entry((round, task))
            .or_insert_with(|| (candidate, Vec::new()))
            .1
            .push((item, fingerprint));
    }
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let limit = super::completion::repair_limit(tx, run_id, "local")?;
    for ((round, task), (candidate, items)) in groups {
        let Some((state, _, blocked)) = task_state(tx, &task)? else {
            continue;
        };
        let names: Vec<&str> = items.iter().map(|(i, _)| i.as_str()).collect();
        let consumed = |by: &str| {
            event(
                child_id(&round, &format!("consumed/{task}")),
                "verification.failures_consumed",
                json!({"round_id": round, "task_id": task, "by": by, "items": names}),
            )
        };
        if cancelling || matches!(state, TaskState::Cancelled | TaskState::Accepted) {
            writer::write(
                tx,
                &Commit {
                    events: vec![
                        event(
                            child_id(&round, &format!("abandoned/{task}")),
                            "verification.failures_abandoned",
                            json!({"round_id": round, "task_id": task, "items": names,
                                "reason": if cancelling { "run_cancelling" } else { state.as_str() }}),
                        ),
                        consumed("abandoned"),
                    ],
                    ..Commit::default()
                },
                ctx,
            )?;
            continue;
        }
        if state == TaskState::Blocked && blocked.as_deref() == Some("NEED_DECISION") {
            // Appended to its decision, consumed with it.
            writer::write(
                tx,
                &Commit {
                    events: vec![
                        event(
                            child_id(&round, &format!("appended/{task}")),
                            "need_decision.appended",
                            json!({"task_id": task, "round_id": round, "items": names}),
                        ),
                        consumed("decision"),
                    ],
                    ..Commit::default()
                },
                ctx,
            )?;
            continue;
        }
        // Counted: one Repair::Local per item (decision 12), each written before the next is asked.
        let mut counts = Vec::new();
        for (n, (item, fingerprint)) in items.iter().enumerate() {
            let (verdict, commit) = super::retry::repair_in(
                tx,
                &RepairRequest {
                    run_id: run_id.to_owned(),
                    task_id: task.clone(),
                    repair: Repair::Local {
                        fingerprint: fingerprint.clone(),
                    },
                    limit,
                    detail: format!("{item} (round {round})"),
                    action_id: Some(child_id(&round, &format!("rework/{task}/{n}"))),
                },
            )?;
            writer::write(tx, &commit, ctx)?;
            counts.push(match verdict {
                RepairVerdict::Granted { count } => {
                    json!({"item": item, "count": count, "granted": true})
                }
                RepairVerdict::Refused { count } => {
                    json!({"item": item, "count": count, "granted": false})
                }
            });
        }
        let mut commit = Commit::default();
        if let Some((TaskState::Verifying, revision, _)) = task_state(tx, &task)? {
            // Reworked: back to READY in the critical layer, its retry budget anew (decision 12).
            tx.execute(
                "UPDATE tasks SET layer = 0, attempt_count = 0, next_retry_at = NULL WHERE task_id = ?1",
                params![task],
            )
            .map_err(sqlite_error)?;
            commit.cas.push(StateCas {
                id: task.clone(),
                expected_revision: revision,
                change: Change::Task(TaskState::Ready),
            });
            commit.events.push(event(
                child_id(&round, &format!("rework/{task}")),
                "task.rework",
                json!({"task_id": task, "round_id": round, "candidate_id": candidate, "items": names,
                    "counts": counts}),
            ));
            done.reworked.push(task.clone());
        } else {
            commit.events.push(event(
                child_id(&round, &format!("attached/{task}")),
                "verification.failures_attached",
                json!({"task_id": task, "round_id": round, "state": state.as_str(), "items": names,
                    "counts": counts}),
            ));
        }
        commit.events.push(consumed("rework"));
        writer::write(tx, &commit, ctx)?;
    }
    Ok(())
}

/// Development task `task`'s current round: the latest that names it as a source.
fn current<'a>(all: &'a [Round], task: &str) -> Option<&'a Round> {
    all.iter()
        .rev()
        .find(|r| r.sources.iter().any(|s| s == task))
}

/// Task `task` has failure items not yet consumed.
fn unconsumed(tx: &Transaction<'_>, run_id: &str, task: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events e, json_each(e.payload_json, '$.items') i          WHERE e.kind = 'verification.failures' AND e.run_id = ?1 AND json_extract(i.value, '$.task_id') = ?2          AND NOT EXISTS (SELECT 1 FROM events c WHERE c.kind = 'verification.failures_consumed' AND c.run_id = ?1            AND json_extract(c.payload_json, '$.round_id') = json_extract(e.payload_json, '$.round_id')            AND json_extract(c.payload_json, '$.task_id') = ?2))",
        params![run_id, task],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// (3) Each VERIFYING source task's exit by its current round and its candidate now; (4) each candidate's barrier.
#[allow(clippy::too_many_arguments)]
fn exits(
    tx: &Transaction<'_>,
    run_id: &str,
    project_id: &str,
    all: &[Round],
    verdicts: &BTreeMap<String, String>,
    cancelling: bool,
    ctx: &Ctx<'_>,
    done: &mut Reconciled,
) -> Result<(), PersistError> {
    if cancelling {
        return Ok(());
    }
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.to_owned(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let sources: BTreeSet<&str> = all
        .iter()
        .flat_map(|r| r.sources.iter().map(String::as_str))
        .collect();
    // Rounds of reverification opened in this pass, by the round they follow.
    let mut reverified: BTreeMap<String, Vec<Source>> = BTreeMap::new();
    for task in sources {
        let Some((TaskState::Verifying, revision, _)) = task_state(tx, task)? else {
            continue;
        };
        let Some(round) = current(all, task) else {
            continue;
        };
        let Some(verdict) = verdicts.get(&round.id) else {
            continue;
        };
        let candidate = candidate_state(tx, &round.candidate)?.unwrap_or_default();
        match (candidate.as_str(), verdict.as_str()) {
            ("ACCEPTED", _) | ("VERIFYING", "passable") => {}
            ("VERIFYING", "reverify") => {
                // Verified again on the same candidate, counted on the round's chain (decision 12).
                let (verdict, commit) = super::retry::repair_in(
                    tx,
                    &RepairRequest {
                        run_id: run_id.to_owned(),
                        task_id: task.to_owned(),
                        repair: Repair::Reverify {
                            root: round.chain_root.clone(),
                        },
                        limit: super::retry::REVERIFY_LIMIT,
                        detail: format!("round {} was inconclusive", round.id),
                        action_id: Some(round.id.clone()),
                    },
                )?;
                writer::write(tx, &commit, ctx)?;
                if matches!(verdict, RepairVerdict::Granted { .. }) {
                    reverified
                        .entry(round.id.clone())
                        .or_default()
                        .push(Source {
                            task_id: task.to_owned(),
                            revision,
                        });
                }
            }
            ("VERIFYING", "flaky") => {
                tx.execute(
                    "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
                    params![task],
                )
                .map_err(sqlite_error)?;
                writer::write(
                    tx,
                    &Commit {
                        cas: vec![StateCas {
                            id: task.to_owned(),
                            expected_revision: revision,
                            change: Change::Task(TaskState::Blocked),
                        }],
                        events: vec![event(
                            child_id(&round.id, &format!("flaky/{task}")),
                            "need_decision",
                            json!({"run_id": run_id, "task_id": task, "round_id": round.id, "reason": "flaky",
                                "detail": "an acceptance criterion is flaky on this candidate: retesting it cannot satisfy \
                                    it; the flaky handling or a new candidate is a decision (REQ-055)"}),
                        )],
                        ..Commit::default()
                    },
                    ctx,
                )?;
            }
            _ => {
                // Its candidate is no longer under verification (a sibling's failure, a veto, a supersession): it waits for
                // a sibling still producing the successor, else goes back to be frozen again (I7).
                let mut waiting = false;
                for sibling in all
                    .iter()
                    .filter(|r| r.candidate == round.candidate)
                    .flat_map(|r| r.sources.iter())
                    .filter(|s| s.as_str() != task)
                {
                    if current(all, sibling).is_some_and(|r| r.candidate == round.candidate)
                        && task_state(tx, sibling)?.is_some_and(|(s, _, _)| {
                            matches!(
                                s,
                                TaskState::Ready
                                    | TaskState::Leased
                                    | TaskState::Running
                                    | TaskState::Blocked
                            )
                        })
                    {
                        waiting = true;
                    }
                }
                if waiting || unconsumed(tx, run_id, task)? {
                    continue;
                }
                let (verdict, mut commit) = super::retry::repair_in(
                    tx,
                    &RepairRequest {
                        run_id: run_id.to_owned(),
                        task_id: task.to_owned(),
                        repair: Repair::Reverify {
                            root: format!("refreeze/{task}"),
                        },
                        limit: super::retry::REVERIFY_LIMIT,
                        detail: format!(
                            "candidate {} is {candidate}; its content is to be frozen anew",
                            round.candidate
                        ),
                        action_id: Some(child_id(&round.id, &format!("refreeze/{task}"))),
                    },
                )?;
                if matches!(verdict, RepairVerdict::Granted { .. }) {
                    commit.cas.push(StateCas {
                        id: task.to_owned(),
                        expected_revision: revision,
                        change: Change::Task(TaskState::Ready),
                    });
                    commit.events.push(event(
                        child_id(&round.id, &format!("superseded/{task}")),
                        "task.superseded",
                        json!({"task_id": task, "round_id": round.id, "candidate_id": round.candidate,
                            "candidate_state": candidate,
                            "detail": "the candidate is no longer valid for other reasons; when the content is \
                                unchanged a new commit is needed (an empty one will do)"}),
                    ));
                }
                writer::write(tx, &commit, ctx)?;
            }
        }
    }
    // The next round of each inconclusive one, on the same candidate with the same scope.
    for (previous, sources) in reverified {
        let Some(round) = all.iter().find(|r| r.id == previous) else {
            continue;
        };
        let opened = super::verification::reverify(
            tx,
            run_id,
            round.candidate.as_str(),
            &round.id,
            &round.chain_root,
            sources,
            ctx,
        )?;
        done.opened.push(opened);
    }
    // (4) The candidates' barriers.
    let candidates: BTreeSet<&str> = all.iter().map(|r| r.candidate.as_str()).collect();
    for candidate in candidates {
        if candidate_state(tx, candidate)?.as_deref() != Some("VERIFYING") {
            continue;
        }
        let rounds: Vec<&Round> = all.iter().filter(|r| r.candidate == candidate).collect();
        let participants: BTreeSet<&str> = rounds
            .iter()
            .flat_map(|r| r.sources.iter().map(String::as_str))
            .collect();
        let mut joinable = false;
        for task in participants {
            let Some((state, _, _)) = task_state(tx, task)? else {
                continue;
            };
            if state == TaskState::Cancelled {
                continue;
            }
            let at_passable = current(all, task).is_some_and(|r| {
                r.candidate == candidate
                    && verdicts.get(&r.id).map(String::as_str) == Some("passable")
            });
            let all_closed = all
                .iter()
                .filter(|r| r.sources.iter().any(|s| s == task))
                .all(|r| verdicts.contains_key(&r.id));
            if state != TaskState::Verifying
                || !at_passable
                || !all_closed
                || unconsumed(tx, run_id, task)?
            {
                joinable = false;
                break;
            }
            joinable = true;
        }
        if !joinable {
            continue;
        }
        let id = child_id(candidate, &format!("join/{}", rounds.len()));
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
                params![id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if !exists {
            writer::write(
                tx,
                &Commit {
                    events: vec![event(
                        id,
                        "verification.candidate_ready_for_join",
                        json!({"candidate_id": candidate, "rounds": rounds.iter().map(|r| r.id.clone()).collect::<Vec<_>>()}),
                    )],
                    ..Commit::default()
                },
                ctx,
            )?;
            iteration(tx, (project_id, run_id), candidate, "passable", ctx)?;
            done.joinable.push(candidate.to_owned());
        }
    }
    Ok(())
}

/// Candidate `candidate`'s one iteration, at its first conclusive result (decision 11, M2T16D-362, -657): a round closed
/// `repair` or `invalidated`, or its join point; `iteration.completed` and an `iterations` row, the run's iteration
/// usage counted (ADR-020). Nothing when it was counted already.
fn iteration(
    tx: &Transaction<'_>,
    (project_id, run_id): (&str, &str),
    candidate: &str,
    verdict: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let id = child_id(candidate, "iteration");
    let counted: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
            params![id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if counted {
        return Ok(());
    }
    let number: i64 = tx
        .query_row(
            "SELECT coalesce(max(number), 0) + 1 FROM iterations WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO iterations(iteration_id, run_id, number) VALUES (?1, ?2, ?3)",
        params![
            child_id(run_id, &format!("iteration/{number}")),
            run_id,
            number
        ],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "UPDATE budgets SET used_value = used_value + 1 WHERE scope_kind = 'run' AND scope_id = ?1 \
         AND kind = 'iterations'",
        params![run_id],
    )
    .map_err(sqlite_error)?;
    writer::write(
        tx,
        &Commit {
            events: vec![NewEvent {
                project_id: project_id.to_owned(),
                event_id: id,
                run_id: Some(run_id.to_owned()),
                kind: "iteration.completed".to_owned(),
                payload: json!({"iteration": number, "candidate_id": candidate, "verdict": verdict}),
            }],
            ..Commit::default()
        },
        ctx,
    )
    .map(drop)
}

/// How long transient recovery failures of a candidate must last, and how many in a row, before it is judged
/// unrecoverable (docs/adr/ADR-028.md decision 11, M2T16D-1251).
const TRANSIENT_FAILURES: usize = 3;
const TRANSIENT_SPAN_MS: u128 = 10 * 60 * 1000;

/// One recovery of a candidate's pending records: how many it recorded, or its failure (permanent, and why).
pub type RecoveryOutcome = Result<usize, (bool, String)>;

/// Record one recovery of candidate `candidate`'s pending check records at `now` (decision 11): a failure is
/// `candidate.reconcile_failed{attempt, kind, error}`, a success after failures `candidate.reconcile_recovered` (the
/// count starts again). Judged unrecoverable — a permanent failure at once, transient ones at least 3 in a row over at
/// least 10 minutes — in this transaction: a VERIFYING candidate STALE (its prior state the condition), any other kept;
/// the persistent fact `candidate.stale{evidence_unrecoverable}` and `integrity_violation` either way; its pending
/// facts kept for audit, never tried again (-1252).
pub(super) fn record_recovery(
    conn: &mut rusqlite::Connection,
    candidate: &str,
    outcome: &RecoveryOutcome,
    now: std::time::SystemTime,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let (run_id, project_id): (String, String) = tx
        .query_row(
            "SELECT c.run_id, r.project_id FROM candidates c JOIN runs r ON r.run_id = c.run_id WHERE c.candidate_id = ?1",
            params![candidate],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let of = |kind: &str| -> Result<Vec<(i64, Value)>, PersistError> {
        let mut statement = tx
            .prepare(
                "SELECT seq, payload_json FROM events WHERE kind = ?1 AND json_extract(payload_json, '$.candidate_id') = ?2 \
                 ORDER BY seq",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![kind, candidate], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?;
        rows.map(|row| {
            let (seq, payload) = row.map_err(sqlite_error)?;
            Ok((seq, serde_json::from_str(&payload).unwrap_or(Value::Null)))
        })
        .collect()
    };
    let recovered = of("candidate.reconcile_recovered")?;
    let failed = of("candidate.reconcile_failed")?;
    let since = recovered.last().map_or(0, |(seq, _)| *seq);
    let run: Vec<&(i64, Value)> = failed.iter().filter(|(seq, _)| *seq > since).collect();
    let millis = |t: std::time::SystemTime| {
        t.duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    };
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        project_id: project_id.clone(),
        event_id,
        run_id: Some(run_id.clone()),
        kind: kind.to_owned(),
        payload,
    };
    let mut events = Vec::new();
    match outcome {
        Ok(recorded) => {
            if !run.is_empty() {
                events.push(event(
                    child_id(candidate, &format!("reconcile_recovered/{}", recovered.len() + 1)),
                    "candidate.reconcile_recovered",
                    json!({"candidate_id": candidate, "recorded": recorded, "after_failures": run.len()}),
                ));
            }
        }
        Err((permanent, error)) => {
            let attempt = run.len() + 1;
            events.push(event(
                child_id(candidate, &format!("reconcile_failed/{}", failed.len() + 1)),
                "candidate.reconcile_failed",
                json!({"candidate_id": candidate, "attempt": attempt,
                    "kind": if *permanent { "permanent" } else { "transient" }, "error": error,
                    "at_ms": millis(now).to_string()}),
            ));
            let first = run
                .first()
                .and_then(|(_, p)| p["at_ms"].as_str().and_then(|s| s.parse::<u128>().ok()))
                .unwrap_or_else(|| millis(now));
            let judged = *permanent
                || (attempt >= TRANSIENT_FAILURES
                    && millis(now).saturating_sub(first) >= TRANSIENT_SPAN_MS);
            let stale: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM events WHERE kind = 'candidate.stale' \
                     AND json_extract(payload_json, '$.candidate_id') = ?1)",
                    params![candidate],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            if judged && !stale {
                tx.execute(
                    "UPDATE candidates SET state = 'STALE' WHERE candidate_id = ?1 AND state = 'VERIFYING'",
                    params![candidate],
                )
                .map_err(sqlite_error)?;
                events.push(event(
                    child_id(candidate, "stale"),
                    "candidate.stale",
                    json!({"candidate_id": candidate, "reason": "evidence_unrecoverable", "error": error}),
                ));
                events.push(event(
                    child_id(candidate, "unrecoverable"),
                    "integrity_violation",
                    json!({"candidate_id": candidate, "reason": "evidence_unrecoverable", "error": error}),
                ));
            }
        }
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
