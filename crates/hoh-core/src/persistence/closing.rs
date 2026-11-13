//! The verification reconcile (docs/adr/ADR-028.md decision 11): level-triggered, one transaction per run each tick
//! and at the host's start, reading only the state as it is now, idempotent. In order: (1) a round whose candidate is
//! no longer under verification (or whose run is being cancelled) has its unsettled verifier tasks cancelled; (2) a
//! round whose two verifier tasks are settled is closed with its verdict (superseded, invalidated, or the class of
//! `verification::classify`), its failures registered with their owners (I6), its candidate REJECTED when failed;
//! (2a) each development task with failure items not yet consumed takes them by its state (rework, attached and
//! counted, appended to its decision, or abandoned); (3) each VERIFYING source task whose current round is closed goes
//! where the exit table says (another round, a decision, waiting for a sibling, or back to be frozen again); (4) a
//! candidate every round of which is closed and every one of whose development tasks waits at a passable round is
//! joined in the same transaction (docs/adr/ADR-029.md decision 11, `join`).

use super::retry::{Repair, RepairRequest, RepairVerdict};
use super::verification::{ROUND_CLOSED, ROUND_OPENED, Source};
use super::writer::{self, Change, Commit, Ctx, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::TaskState;
use crate::verification::acceptance;
use crate::verification::classify::{self, Ac, Class, Report, Row};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The outcome reasons a FAIL is no conclusion with (decision 11's inconclusive set).
/// What one reconcile did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    pub cancelled: Vec<String>,
    /// The rounds closed, with their verdicts.
    pub closed: Vec<(String, String)>,
    pub reworked: Vec<String>,
    pub opened: Vec<String>,
    /// The candidates joined (docs/adr/ADR-029.md decision 11).
    pub joinable: Vec<String>,
}

/// A round as its `verification.round_opened` recorded it.
#[derive(Debug, Clone)]
pub(super) struct Round {
    pub id: String,
    pub candidate: String,
    pub sources: Vec<String>,
    pub scope: Vec<String>,
    pub qa: String,
    pub reviewer: String,
    pub chain_root: String,
    pub seq: i64,
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

pub(super) fn rounds(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<Round>, PersistError> {
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

pub(super) fn task_state(
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

pub(super) fn candidate_state(
    tx: &Transaction<'_>,
    candidate: &str,
) -> Result<Option<String>, PersistError> {
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
        "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'candidate.stale' \
           AND json_extract(payload_json, '$.candidate_id') = ?2) \
         OR EXISTS(SELECT 1 FROM events e, json_each(e.payload_json, '$.superseded') s \
           WHERE e.kind = 'candidate.frozen' AND e.run_id = ?1 AND s.value = ?2)",
        params![run_id, candidate],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The attempts of round `round`'s verifier tasks.
pub(super) fn attempts(tx: &Transaction<'_>, round: &Round) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare("SELECT attempt_id FROM attempts WHERE task_id IN (?1, ?2) ORDER BY rowid")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![round.qa, round.reviewer], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// One planned-check evidence of a round's verifier attempts, or of a candidate.
#[derive(Debug, Clone)]
pub(super) struct Evidence {
    id: String,
    check_id: String,
    outcome: String,
    reason: Option<String>,
    reasons: Vec<String>,
    rows: Vec<Value>,
    deferred: bool,
    /// Whether its observations have a FAIL result line (docs/adr/ADR-029.md decision 4).
    fail_lines: bool,
}

/// The planned-check evidence (kind `check`, never an attempt-level marker record) `filter` (a condition on `ev`,
/// its one parameter `key`) selects, in row order.
fn evidence_where(
    tx: &Transaction<'_>,
    filter: &str,
    key: &str,
) -> Result<Vec<Evidence>, PersistError> {
    let mut statement = tx
        .prepare(&format!(
            "SELECT ev.evidence_id, ev.check_id, ev.outcome, \
             (SELECT payload_json FROM events WHERE event_id = ev.evidence_id || '/evidence') \
             FROM evidence ev WHERE {filter} AND ev.kind = 'check' ORDER BY ev.rowid"
        ))
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![key], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(sqlite_error)?;
    let mut out = Vec::new();
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
            fail_lines: p["observations"]
                .as_array()
                .is_some_and(|o| o.iter().any(|line| line["status"] == json!("FAIL"))),
        });
    }
    Ok(out)
}

/// The planned-check evidence of `attempts`, in their order.
fn evidence(tx: &Transaction<'_>, attempts: &[String]) -> Result<Vec<Evidence>, PersistError> {
    let mut out = Vec::new();
    for attempt in attempts {
        out.extend(evidence_where(tx, "ev.attempt_id = ?1", attempt)?);
    }
    Ok(out)
}

/// Every planned-check evidence of candidate `candidate` (a join reads the candidate's whole history, docs/adr/ADR-029.md
/// decisions 4 and 11).
pub(super) fn candidate_evidence(
    tx: &Transaction<'_>,
    candidate: &str,
) -> Result<Vec<Evidence>, PersistError> {
    evidence_where(tx, "ev.candidate_id = ?1", candidate)
}

/// The status of the latest report of `role` among `attempts` (in their order): its last `independent_reports` row
/// that is CURRENT or STALE, never a REJECTED one (docs/adr/ADR-029.md decision 7; a round reads its STALE reports,
/// ADR-028 -850), and the findings' summaries its attempt's `verification.report` named (decision 15); NOT_RUN without
/// one.
fn report(
    tx: &Transaction<'_>,
    attempts: &[String],
    role: &str,
) -> Result<(Report, Vec<String>), PersistError> {
    let mut latest: Option<(String, Option<String>)> = None;
    for attempt in attempts {
        let found: Option<(String, Option<String>)> = tx
            .query_row(
                "SELECT r.status, (SELECT json_extract(e.payload_json, '$.findings') FROM events e \
                   WHERE e.run_id = r.run_id AND e.kind = 'verification.report' \
                   AND json_extract(e.payload_json, '$.attempt_id') = r.attempt_id \
                   ORDER BY e.seq DESC LIMIT 1) \
                 FROM independent_reports r WHERE r.attempt_id = ?1 AND r.role = ?2 AND r.state <> 'REJECTED' \
                 ORDER BY r.rowid DESC LIMIT 1",
                params![attempt, role],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(sqlite_error)?;
        if found.is_some() {
            latest = found;
        }
    }
    let (status, findings) = latest.unwrap_or_default();
    let mut findings: Vec<String> = findings
        .and_then(|f| serde_json::from_str::<Vec<String>>(&f).ok())
        .unwrap_or_default();
    findings.sort();
    Ok((
        match status.as_str() {
            "PASS" => Report::Pass,
            "FAIL" => Report::Fail,
            "UNKNOWN" => Report::Unknown,
            _ => Report::NotRun,
        },
        findings,
    ))
}

pub(super) fn row_of(status: &str) -> Row {
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

/// The open Issues of round `round` (decision 13's source predicate, M2T16D-1051, -1052): the change requests its
/// verifiers made (`issue.opened` names the round), and the critical or high findings its verifiers' reports registered
/// (report_id -> the report's attempt -> one of the round's verifier tasks); a lower one does not fail a round (-154).
/// (issue id, fingerprint).
pub(super) fn round_issues(
    tx: &Transaction<'_>,
    round: &Round,
) -> Result<Vec<(String, String)>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT i.issue_id, i.fingerprint FROM issues i WHERE i.state IN ('OPEN', 'REOPENED') AND (i.issue_id IN \
             (SELECT json_extract(payload_json, '$.issue_id') FROM events WHERE kind = 'issue.opened' \
              AND json_extract(payload_json, '$.round_id') = ?1) \
             OR (i.severity IN ('critical', 'high') AND i.report_id IN (SELECT r.report_id FROM independent_reports r \
              JOIN attempts a ON a.attempt_id = r.attempt_id WHERE a.task_id IN (?2, ?3)))) ORDER BY i.issue_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![round.id, round.qa, round.reviewer], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The run's project, spec id and state.
pub(super) fn run_of(
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
pub(super) struct Item {
    pub task_id: String,
    pub item: String,
}

impl Item {
    pub(super) fn fingerprint(&self) -> String {
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
    // Reports whose candidate no longer stands, made STALE whoever superseded it (docs/adr/ADR-029.md decision 8).
    super::reports::sweep(&tx, run_id)?;
    // The Issues' states as the facts say, whatever wrote them before (a store older than ADR-029 decision 10's
    // materialisation included).
    super::issues::settle_all(&tx, run_id)?;
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
        // What the round's verifiers were delivered to confirm and what they said of it (docs/adr/ADR-029.md decision
        // 10): its absent facts are written whatever the verdict; its critical and high entries classify it.
        let sides = [
            super::issues::side(&tx, &attempts, "qa")?,
            super::issues::side(&tx, &attempts, "reviewer")?,
        ];
        let entries = super::issues::entries(&tx, run_id, &round.candidate, &sides)?;
        super::issues::observe_absent(&tx, run_id, &round.id, &round.candidate, &sides, &entries)?;
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
                spec_id.as_deref(),
                &attempts,
                &evidence,
                (&issues, &entries),
            )?;
            let class = classify::classify(&input);
            if class == Class::Failed {
                // Each acceptance failure an Issue on the candidate, in this close (docs/adr/ADR-029.md decision 9).
                super::issues::open_acceptance(
                    &tx,
                    run_id,
                    &round.candidate,
                    &super::issues::ac_failures(failed.iter().map(|(_, item)| item.as_str())),
                )?;
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
            (verdict.to_owned(), Some(class), inputs_json(&input))
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
        // Each verifier's last result in the round (decision 15): COMPLETED, FAILED, BLOCKED, INCOMPLETE or missing.
        let mut results = serde_json::Map::new();
        for (role, task) in [("qa", &round.qa), ("reviewer", &round.reviewer)] {
            let status: Option<String> = tx
                .query_row(
                    "SELECT json_extract(payload_json, '$.result_status') FROM events WHERE kind = 'verification.result' \
                     AND json_extract(payload_json, '$.task_id') = ?1 ORDER BY seq DESC LIMIT 1",
                    params![task],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?
                .flatten();
            results.insert(role.to_owned(), json!(status));
        }
        // The evidence whose rows this close published (M2T16R-04).
        let published: Vec<&str> = evidence
            .iter()
            .filter(|e| e.deferred)
            .map(|e| e.id.as_str())
            .collect();
        let mut events = vec![event(
            child_id(&round.id, "closed"),
            ROUND_CLOSED,
            json!({"round_id": round.id, "candidate_id": round.candidate, "verdict": verdict,
                "class": class.map(|c| format!("{c:?}").to_lowercase()), "inputs": inputs,
                "source_task_ids": round.sources, "candidate_state": candidate, "result_status": results,
                "published": published}),
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
        // A superseded round's handoffs never taken are DEAD_LETTER, however its verifier tasks ended (a canceller's
        // move included, decision 9, M2T16R-153).
        if verdict == "superseded" {
            for role in ["qa", "reviewer"] {
                let original = child_id(&round.id, &format!("verify/{role}"));
                let mut statement = tx
                    .prepare(
                        "SELECT message_id FROM outbox WHERE state IN ('PENDING', 'DELIVERING') AND (message_id = ?1 \
                         OR message_id IN (SELECT json_extract(payload_json, '$.to') FROM events \
                           WHERE run_id = ?4 AND kind = 'verification.handoff_reissued' \
                           AND json_extract(payload_json, '$.round_id') = ?2 AND json_extract(payload_json, '$.role') = ?3))",
                    )
                    .map_err(sqlite_error)?;
                let untaken: Vec<String> = statement
                    .query_map(params![original, round.id, role, run_id], |r| r.get(0))
                    .map_err(sqlite_error)?
                    .collect::<Result<_, _>>()
                    .map_err(sqlite_error)?;
                for message in untaken {
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
            }
        }
        // Its handoffs (decision 9): ACCEPTED for a verifier whose result was COMPLETED (its task IMPLEMENTED or done),
        // REJECTED for the other, and for both when the round was superseded.
        for (role, task) in [("qa", &round.qa), ("reviewer", &round.reviewer)] {
            let completed = verdict != "superseded"
                && matches!(
                    task_state(&tx, task)?,
                    Some((
                        TaskState::Implemented | TaskState::Verifying | TaskState::Accepted,
                        _,
                        _
                    ))
                );
            tx.execute(
                "UPDATE handoffs SET state = ?1 WHERE state = 'DELIVERED' AND (handoff_id = ?2 OR handoff_id IN \
                 (SELECT json_extract(payload_json, '$.to') FROM events WHERE kind = 'verification.handoff_reissued' \
                  AND json_extract(payload_json, '$.round_id') = ?3 AND json_extract(payload_json, '$.role') = ?4))",
                params![
                    if completed { "ACCEPTED" } else { "REJECTED" },
                    child_id(&round.id, &format!("verify/{role}")),
                    round.id,
                    role
                ],
            )
            .map_err(sqlite_error)?;
        }
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
        // Its rows, its absent facts and its Issues materialised before the next round is judged (decision 10).
        super::issues::settle_all(&tx, run_id)?;
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
    super::issues::settle_all(&tx, run_id)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

/// Task `task_id`'s acceptance criteria under specification `spec_id`.
pub(super) fn task_criteria(
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

/// A classification input as `verification.round_closed` and `verification.joined` record it.
pub(super) fn inputs_json(input: &classify::Input) -> Value {
    json!({
        "qa": format!("{:?}", input.qa), "review": format!("{:?}", input.review),
        "checks": format!("{:?}", input.checks),
        "acs": input.acs.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>(),
        "failing_issues": input.failing_issues, "outside_failed": input.outside_failed,
        "unconfirmed": input.unconfirmed,
    })
}

/// One acceptance row of an AC on a candidate: its status, evidence and reason, when it was published (a round's
/// deferred rows when that round closed, its `published`, the others with their evidence, M2T16R-04), and whether its
/// requirement may degrade.
pub(super) struct RowAt {
    pub status: String,
    pub evidence: String,
    pub reason: String,
    pub seq: i64,
    pub degradable: bool,
}

/// The rows of AC `ac` on candidate `candidate` under specification `spec_id`.
pub(super) fn ac_rows(
    tx: &Transaction<'_>,
    candidate: &str,
    ac: &str,
    spec_id: Option<&str>,
) -> Result<Vec<RowAt>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT ar.status, ar.evidence_id, \
             coalesce((SELECT c.seq FROM events c, json_each(c.payload_json, '$.published') p \
               WHERE c.kind = 'verification.round_closed' AND p.value = ar.evidence_id), \
               (SELECT seq FROM events WHERE event_id = ar.evidence_id || '/evidence'), 0), \
             (SELECT r.release_class FROM acceptance_criteria c JOIN requirements r ON r.requirement_id = \
              c.requirement_id AND r.spec_id = c.spec_id WHERE c.ac_id = ar.ac_id AND c.spec_id = ar.spec_id), \
             ar.reason \
             FROM acceptance_results ar WHERE ar.candidate_id = ?1 AND ar.ac_id = ?2 AND ar.spec_id = ?3",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![candidate, ac, spec_id], |r| {
            Ok(RowAt {
                status: r.get(0)?,
                evidence: r.get(1)?,
                seq: r.get(2)?,
                degradable: r.get::<_, Option<String>>(3)?.as_deref() == Some("degradable"),
                reason: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// One in-scope AC a verdict judges: its rows that verdict sees, each (status, evidence, reason), and whether its
/// requirement may degrade.
pub(super) struct Scoped {
    pub ac: String,
    pub rows: Vec<(String, String, String)>,
    pub degradable: bool,
}

/// What a verdict judges, a round's close or a candidate's join (docs/adr/ADR-029.md decision 11): the QA's and the
/// reviewer's report (a FAIL one with its findings' summaries), the planned-check evidence, the in-scope ACs, the FAIL
/// rows of ACs outside the scope (the AC, the row as its evidence recorded it, its reason), whether an open Issue
/// fails it, the critical or high confirmation entries still present (their fingerprints), and whether one is
/// unconfirmed.
pub(super) struct Inputs<'a> {
    pub reports: [(Report, Vec<String>); 2],
    pub evidence: &'a [Evidence],
    pub scoped: Vec<Scoped>,
    pub outside: Vec<(String, Value, String)>,
    pub failing_issues: bool,
    pub still: Vec<String>,
    pub unconfirmed: bool,
}

/// A classification input and the failure items its failing inputs give: (the in-scope AC an item belongs to, if any;
/// the item).
pub(super) type Judgement = (classify::Input, Vec<(Option<String>, String)>);

/// The classification input of `inputs` and the failure items each failing input gives, (the in-scope AC it belongs
/// to, if any; the item): one derivation for a round's close and a candidate's join, so that a failed class has at
/// least one item (ADR-028 implementation correction 6; an open Issue's `issue:` items are the caller's, given
/// whatever the verdict).
pub(super) fn judge(tx: &Transaction<'_>, inputs: Inputs<'_>) -> Result<Judgement, PersistError> {
    // Still present: every source's item `issue:<fingerprint>` (decision 10, -150, -153, -254).
    let mut failed: Vec<(Option<String>, String)> = inputs
        .still
        .iter()
        .map(|fingerprint| (None, format!("issue:{fingerprint}")))
        .collect();
    let [(qa, qa_findings), (review, review_findings)] = inputs.reports;
    // A FAIL report's item is its role and its findings' summaries, sorted (decision 12, M2T16R-06).
    for (role, status, findings) in [
        ("qa", qa, qa_findings),
        ("reviewer", review, review_findings),
    ] {
        if status == Report::Fail {
            failed.push((
                None,
                named(&format!("report-fail:{role}"), &findings, " | "),
            ));
        }
    }
    // The check dimension and the check failures from one judgement (docs/adr/ADR-029.md decision 4, M2T18D-100,
    // -151): a check is judged on all its runs.
    let runs: Vec<acceptance::CheckRun> = inputs
        .evidence
        .iter()
        .map(|e| acceptance::CheckRun {
            check_id: e.check_id.clone(),
            outcome: e.outcome.clone(),
            reason: e.reason.clone(),
            fail_lines: e.fail_lines,
        })
        .collect();
    let checks = acceptance::checks(&runs);
    let mut acs = Vec::new();
    for scoped in &inputs.scoped {
        let seen: Vec<Row> = scoped
            .rows
            .iter()
            .map(|(status, ..)| row_of(status))
            .collect();
        let projected = classify::project(&seen, scoped.degradable);
        if projected == Ac::Failed {
            // The item is the AC and its failing tests (decision 12, M2T16R-06): another failure of it is another item.
            let mut tests = BTreeSet::new();
            for (_, evidence_id, reason) in scoped.rows.iter().filter(|r| r.0 == "FAIL") {
                tests.extend(failing_tests(
                    &row_of_evidence(tx, evidence_id, &scoped.ac)?,
                    reason,
                ));
            }
            // One item per failing test (implementation correction 4, M2T16R-100): a test failing every round is the
            // same item whatever fails beside it.
            if tests.is_empty() {
                failed.push((Some(scoped.ac.clone()), format!("ac-fail:{}", scoped.ac)));
            }
            for test in tests {
                failed.push((
                    Some(scoped.ac.clone()),
                    format!("ac-fail:{}:{test}", scoped.ac),
                ));
            }
        }
        acs.push(projected);
    }
    // A FAIL row of an AC outside the scope: every source's (decision 12), and a failing input whatever its check's
    // outcome (implementation correction 6, M2T16R-300).
    for (ac, row, reason) in &inputs.outside {
        let tests = failing_tests(row, reason);
        if tests.is_empty() {
            failed.push((None, format!("ac-fail:{ac}")));
        }
        for test in tests {
            failed.push((None, format!("ac-fail:{ac}:{test}")));
        }
    }
    // A failing check no FAIL row of its evidence locates (a build, say, or FAIL lines of an unmapped TEST): every
    // source's (ADR-028 decision 12, M2T16R-05), from the same judgement as the dimension (ADR-029 decision 4).
    for (check, reason) in acceptance::check_failures(&runs) {
        let located = inputs
            .evidence
            .iter()
            .filter(|e| e.check_id == check)
            .any(|e| e.rows.iter().any(|row| row["status"] == json!("FAIL")));
        if !located {
            failed.push((None, format!("check-fail:{check}:{reason}")));
        }
    }
    Ok((
        classify::Input {
            qa,
            review,
            checks,
            acs,
            failing_issues: inputs.failing_issues || !inputs.still.is_empty(),
            outside_failed: !inputs.outside.is_empty(),
            unconfirmed: inputs.unconfirmed,
        },
        failed,
    ))
}

/// The classification input of `round` and the failure items each failing input gives, from the round's own reports
/// and evidence and the rows of its scope that existed when it opened or are its own (I2, -754).
fn classify_round(
    tx: &Transaction<'_>,
    round: &Round,
    spec_id: Option<&str>,
    attempts: &[String],
    evidence: &[Evidence],
    (issues, entries): (&[(String, String)], &[super::issues::Entry]),
) -> Result<Judgement, PersistError> {
    // The critical and high entries delivered for confirmation (docs/adr/ADR-029.md decision 10, -150, -153, -254):
    // still present is a failing input; delivered to both and neither still present nor confirmed leaves the round
    // unpassable.
    let mut still = Vec::new();
    let mut unconfirmed = false;
    for entry in entries.iter().filter(|e| e.grave()) {
        if entry.still_present {
            still.push(entry.fingerprint.clone());
        } else if entry.unconfirmed() {
            unconfirmed = true;
        }
    }
    // The rows of each in-scope AC on the candidate: those that existed when the round opened and the round's own
    // (a sibling's later rows are its own to judge, I2, -754).
    let own: BTreeSet<&str> = evidence.iter().map(|e| e.id.as_str()).collect();
    let mut scoped = Vec::new();
    for ac in &round.scope {
        let rows = ac_rows(tx, &round.candidate, ac, spec_id)?;
        scoped.push(Scoped {
            ac: ac.clone(),
            degradable: rows.iter().any(|r| r.degradable),
            rows: rows
                .into_iter()
                .filter(|r| own.contains(r.evidence.as_str()) || r.seq < round.seq)
                .map(|r| (r.status, r.evidence, r.reason))
                .collect(),
        });
    }
    // The FAIL rows of ACs outside the scope that the round's own evidence wrote.
    let mut outside = Vec::new();
    for e in evidence {
        for row in &e.rows {
            let ac = row["ac_id"].as_str().unwrap_or_default();
            if row["status"] == json!("FAIL") && !round.scope.iter().any(|s| s == ac) {
                outside.push((
                    ac.to_owned(),
                    row.clone(),
                    row["reason"].as_str().unwrap_or_default().to_owned(),
                ));
            }
        }
    }
    judge(
        tx,
        Inputs {
            reports: [
                report(tx, attempts, "qa")?,
                report(tx, attempts, "reviewer")?,
            ],
            evidence,
            scoped,
            outside,
            failing_issues: !issues.is_empty(),
            still,
            unconfirmed,
        },
    )
}

/// Evidence `evidence_id`'s row on AC `ac` as its event recorded it (`Null` when there is none).
pub(super) fn row_of_evidence(
    tx: &Transaction<'_>,
    evidence_id: &str,
    ac: &str,
) -> Result<Value, PersistError> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![format!("{evidence_id}/evidence")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let payload: Value = payload
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or(Value::Null);
    Ok(payload["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["ac_id"] == json!(ac))
        .cloned()
        .unwrap_or(Value::Null))
}

/// The failing tests of a FAIL row `row`: the failing lines the check driver judged it to have (docs/adr/ADR-028.md
/// implementation correction 5), never chosen again here; the TEST its reason names (`<TEST> in check <check>…`)
/// when it names none (decision 12).
fn failing_tests(row: &Value, reason: &str) -> Vec<String> {
    let mut names: BTreeSet<String> = row["failing"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str().map(str::to_owned))
        .collect();
    let test = reason.split(" in check").next().unwrap_or_default();
    if names.is_empty() && !test.is_empty() {
        names.insert(test.to_owned());
    }
    names.into_iter().collect()
}

/// An item `kind` with what names it (`kind:a<sep>b`), or `kind` alone when nothing does.
fn named(kind: &str, names: &[String], separator: &str) -> String {
    if names.is_empty() {
        kind.to_owned()
    } else {
        format!("{kind}:{}", names.join(separator))
    }
}

/// A (round, task)'s failure items not yet consumed: the round's candidate and each (item, fingerprint).
type Owed = (String, Vec<(String, String)>);

/// (2a) Every development task's failure items not yet consumed, by its state now (decision 11); a failed join's too
/// (docs/adr/ADR-029.md decision 11: its id in the `round_id` field).
pub(super) fn consume(
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
                    // An owner whose attempt runs on is blocked by that attempt's end, never under it (-800).
                    hold: matches!(state, TaskState::Leased | TaskState::Running),
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
        // An Issue of the item's fingerprint taken by the rework (decision 12): recorded, its state the state function's
        // (docs/adr/ADR-029.md decision 10). Those of the round, a change request or a finding of its verifiers' reports;
        // a join's, those of every round on its candidate (decision 11).
        let rounds = failure_rounds(tx, run_id, &round)?;
        for (item, _) in &items {
            let Some(fingerprint) = item.strip_prefix("issue:") else {
                continue;
            };
            let mut statement = tx
                .prepare(
                    "SELECT i.issue_id FROM issues i WHERE i.run_id = ?1 AND i.fingerprint = ?2 AND (i.issue_id IN \
                     (SELECT json_extract(o.payload_json, '$.issue_id') FROM events o WHERE o.run_id = ?1 \
                      AND o.kind = 'issue.opened' AND json_extract(o.payload_json, '$.round_id') IN \
                      (SELECT value FROM json_each(?3))) \
                     OR i.report_id IN (SELECT rep.report_id FROM independent_reports rep \
                      JOIN attempts a ON a.attempt_id = rep.attempt_id JOIN events r ON r.run_id = ?1 \
                      AND r.kind = 'verification.round_opened' \
                      AND json_extract(r.payload_json, '$.round_id') IN (SELECT value FROM json_each(?3)) \
                      AND a.task_id IN (json_extract(r.payload_json, '$.tasks.qa'), \
                        json_extract(r.payload_json, '$.tasks.reviewer')))) ORDER BY i.issue_id",
                )
                .map_err(sqlite_error)?;
            let issues: Vec<String> = statement
                .query_map(
                    params![run_id, fingerprint, json!(rounds).to_string()],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?
                .collect::<Result<_, _>>()
                .map_err(sqlite_error)?;
            for issue in issues {
                let id = child_id(&issue, &format!("consumed/{task}"));
                if exists(tx, &id)? {
                    continue;
                }
                commit.events.push(event(
                    id,
                    "issue.consumed_by_rework",
                    json!({"issue_id": issue, "task_id": task, "round_id": round}),
                ));
            }
        }
        commit.events.push(consumed("rework"));
        writer::write(tx, &commit, ctx)?;
    }
    Ok(())
}

/// Whether event `event_id` was written.
pub(super) fn exists(tx: &Transaction<'_>, event_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The rounds failures registered under `round_id` came from: that round, or every round on the candidate of the join
/// whose id it is (docs/adr/ADR-029.md decision 11, its `verification.joined`).
fn failure_rounds(
    tx: &Transaction<'_>,
    run_id: &str,
    round_id: &str,
) -> Result<Vec<String>, PersistError> {
    let joined: Option<String> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.candidate_id') FROM events WHERE run_id = ?1 \
             AND kind = 'verification.joined' AND json_extract(payload_json, '$.join_id') = ?2 LIMIT 1",
            params![run_id, round_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(candidate) = joined else {
        return Ok(vec![round_id.to_owned()]);
    };
    let mut statement = tx
        .prepare(
            "SELECT json_extract(payload_json, '$.round_id') FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND json_extract(payload_json, '$.candidate_id') = ?3 ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id, ROUND_OPENED, candidate], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// Development task `task`'s current round: the latest that names it as a source.
pub(super) fn current<'a>(all: &'a [Round], task: &str) -> Option<&'a Round> {
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
                let granted = reverify_source(
                    tx,
                    (project_id, run_id),
                    round,
                    (task, revision),
                    Reverification {
                        action: &round.id,
                        join: None,
                        detail: format!("round {} was inconclusive", round.id),
                    },
                    ctx,
                )?;
                if granted {
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
                block_flaky(
                    tx,
                    (project_id, run_id),
                    (task, revision),
                    &round.id,
                    json!({"round_id": round.id}),
                    super::verification::deferred_of(tx, &round.id)?,
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
                        hold: false,
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
    // (4) The candidates' barriers: every round on the candidate closed (one whose sources were all cancelled too,
    // docs/adr/ADR-029.md decision 11, -252), and every development task of them not cancelled VERIFYING at a passable
    // current round on it with nothing left to consume; then its join, in this transaction.
    let candidates: BTreeSet<&str> = all.iter().map(|r| r.candidate.as_str()).collect();
    for candidate in candidates {
        if candidate_state(tx, candidate)?.as_deref() != Some("VERIFYING") {
            continue;
        }
        let rounds: Vec<&Round> = all.iter().filter(|r| r.candidate == candidate).collect();
        if !rounds.iter().all(|r| verdicts.contains_key(&r.id)) {
            continue;
        }
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
        // The barrier's event, numbered by the candidate's rounds, once; a barrier M2-T16 wrote without a join is
        // joined now (J9: the join's own event decides, docs/adr/ADR-029.md decision 11).
        let id = child_id(candidate, &format!("join/{}", rounds.len()));
        if !exists(tx, &id)? {
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
        }
        if super::join::join(tx, run_id, candidate, all, ctx, done)?.is_some() {
            done.joinable.push(candidate.to_owned());
        }
    }
    Ok(())
}

/// How a source is verified again: the action its repair is asked under (the round, or the join's per round), the
/// join when a join asks, and why.
pub(super) struct Reverification<'a> {
    pub action: &'a str,
    pub join: Option<&'a str>,
    pub detail: String,
}

/// Source `task` (VERIFYING at `revision`) of inconclusive round `round` verified again on the same candidate (decision
/// 12; a join's inconclusive class too, docs/adr/ADR-029.md decision 11): `Repair::Reverify` on the round's chain, asked
/// once per action (a sibling source of the round gets the answer replayed with nothing written, and is blocked for the
/// same decision here, M2T16R-03); a refusal is a decision (`reverify_exhausted`) carrying the round's deferred
/// questions. Whether it was granted (the caller opens the next round).
pub(super) fn reverify_source(
    tx: &Transaction<'_>,
    (project_id, run_id): (&str, &str),
    round: &Round,
    (task, revision): (&str, i64),
    how: Reverification<'_>,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let (verdict, mut commit) = super::retry::repair_in(
        tx,
        &RepairRequest {
            run_id: run_id.to_owned(),
            task_id: task.to_owned(),
            repair: Repair::Reverify {
                root: round.chain_root.clone(),
            },
            limit: super::retry::REVERIFY_LIMIT,
            detail: how.detail,
            action_id: Some(how.action.to_owned()),
            hold: false,
        },
    )?;
    let deferred = super::verification::deferred_of(tx, &round.id)?;
    if matches!(verdict, RepairVerdict::Refused { .. }) && commit.events.is_empty() {
        tx.execute(
            "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
            params![task],
        )
        .map_err(sqlite_error)?;
        commit.cas.push(StateCas {
            id: task.to_owned(),
            expected_revision: revision,
            change: Change::Task(TaskState::Blocked),
        });
        let mut payload = json!({"run_id": run_id, "task_id": task, "round_id": round.id,
            "reason": "reverify_exhausted", "round_root": round.chain_root,
            "detail": format!("round {} was inconclusive and its chain is spent", round.id)});
        if let Some(join) = how.join {
            payload["join_id"] = json!(join);
        }
        commit.events.push(NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(how.action, &format!("reverify_exhausted/{task}")),
            run_id: Some(run_id.to_owned()),
            kind: "need_decision".to_owned(),
            payload,
        });
    }
    super::verification::with_deferred(&mut commit, &deferred);
    writer::write(tx, &commit, ctx)?;
    Ok(matches!(verdict, RepairVerdict::Granted { .. }))
}

/// Source `task` (VERIFYING at `revision`) BLOCKED for a decision on a flaky criterion (decision 11; a join's flaky
/// class too, docs/adr/ADR-029.md decision 11): `need_decision{reason: flaky}` keyed under `at` (the round or the join)
/// with `about` (the round, or the candidate and the join) and the deferred questions.
pub(super) fn block_flaky(
    tx: &Transaction<'_>,
    (project_id, run_id): (&str, &str),
    (task, revision): (&str, i64),
    at: &str,
    about: Value,
    deferred: Vec<Value>,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
        params![task],
    )
    .map_err(sqlite_error)?;
    let mut payload = json!({"run_id": run_id, "task_id": task, "reason": "flaky",
        "detail": "an acceptance criterion is flaky on this candidate: retesting it cannot satisfy \
            it; the flaky handling or a new candidate is a decision (REQ-055)",
        "deferred": deferred});
    if let (Some(payload), Some(about)) = (payload.as_object_mut(), about.as_object()) {
        payload.extend(about.clone());
    }
    writer::write(
        tx,
        &Commit {
            cas: vec![StateCas {
                id: task.to_owned(),
                expected_revision: revision,
                change: Change::Task(TaskState::Blocked),
            }],
            events: vec![NewEvent {
                project_id: project_id.to_owned(),
                event_id: child_id(at, &format!("flaky/{task}")),
                run_id: Some(run_id.to_owned()),
                kind: "need_decision".to_owned(),
                payload,
            }],
            ..Commit::default()
        },
        ctx,
    )
    .map(drop)
}

/// Candidate `candidate`'s one iteration, at its first conclusive result (decision 11, M2T16D-362, -657): a round closed
/// `repair` or `invalidated`, or its join passable or failed (docs/adr/ADR-029.md decision 13; an inconclusive or flaky
/// one is none); `iteration.completed` and an `iterations` row, the run's iteration usage counted (ADR-020). Nothing
/// when it was counted already (a barrier M2-T16 counted included).
pub(super) fn iteration(
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

/// The verifier tasks of run `run_id` whose round's candidate is held (docs/adr/ADR-028.md decision 16): its last
/// recovery failed, none succeeded since.
pub(super) fn held_by_recovery(
    tx: &rusqlite::Connection,
    run_id: &str,
) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT t.value FROM events o, json_each(o.payload_json, '$.tasks') t \
             WHERE o.kind = ?1 AND o.run_id = ?2 AND EXISTS(SELECT 1 FROM events f \
               WHERE f.kind = 'candidate.reconcile_failed' \
               AND json_extract(f.payload_json, '$.candidate_id') = json_extract(o.payload_json, '$.candidate_id') \
               AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
                 AND json_extract(r.payload_json, '$.candidate_id') = json_extract(o.payload_json, '$.candidate_id')), 0))",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// Record one recovery of candidate `candidate`'s pending check records at `now` (decision 11): a failure is
/// `candidate.reconcile_failed{attempt, kind, error}`, a success after failures `candidate.reconcile_recovered` (the
/// count starts again). Judged unrecoverable — a permanent failure at once, transient ones at least 3 in a row over at
/// least 10 minutes — in this transaction: a VERIFYING candidate STALE (its prior state the condition), any other kept;
/// the persistent fact `candidate.stale{evidence_unrecoverable}` and `integrity_violation` either way; its pending
/// facts kept for audit, never tried again (-1252). At the host's start (`at_start`, decision 16), a failure of a
/// candidate no persistent fact superseded also wakes the Commander (`integrity_violation`); a superseded candidate's
/// failure is only its `reconcile_failed`, never judged: its results are history (-1201).
pub(super) fn record_recovery(
    conn: &mut rusqlite::Connection,
    candidate: &str,
    outcome: &RecoveryOutcome,
    now: std::time::SystemTime,
    at_start: bool,
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
                    "SELECT EXISTS(SELECT 1 FROM events WHERE run_id = ?2 AND kind = 'candidate.stale' \
                     AND json_extract(payload_json, '$.candidate_id') = ?1)",
                    params![candidate, run_id],
                    |r| r.get(0),
                )
                .map_err(sqlite_error)?;
            // Superseded: a later freeze listed it, or it is STALE; either fact persists (decision 16).
            let superseded = stale
                || tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM events f, json_each(f.payload_json, '$.superseded') v \
                         WHERE f.run_id = ?2 AND f.kind = 'candidate.frozen' AND v.value = ?1)",
                        params![candidate, run_id],
                        |r| r.get(0),
                    )
                    .map_err(sqlite_error)?;
            if at_start && !superseded && !judged {
                events.push(event(
                    child_id(candidate, &format!("reconcile_failed/{}/start", failed.len() + 1)),
                    "integrity_violation",
                    json!({"candidate_id": candidate, "reason": "reconcile_failed_at_start", "error": error}),
                ));
            }
            if judged && !superseded {
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
