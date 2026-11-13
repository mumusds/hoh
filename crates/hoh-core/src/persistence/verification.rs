//! Verification rounds (docs/adr/ADR-028.md decision 9): a round is a candidate, the development tasks it verifies
//! (its sources) and its number on that candidate. It is opened only inside a writer's completion commit (or a
//! reverification, decision 11), with its two handoff messages and its two Runtime verifier tasks in the same
//! transaction.

use super::decide::load_binding;
use super::writer::{NewEvent, NewOutbox};
use super::{PersistError, sqlite_error};
use crate::handoff::{self, Recipient, Verification};
use crate::ids::child_id;
use crate::protocol::Candidate;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

/// The kind of the event that opens a round.
pub(super) const ROUND_OPENED: &str = "verification.round_opened";

/// The kind of the event that closes a round (decision 11).
pub(super) const ROUND_CLOSED: &str = "verification.round_closed";

/// The writer tasks of run `run_id` an open round holds (decision 13, REQ-037 decision 1): a round not closed whose
/// candidate has an Issue not closed (OPEN or REOPENED), and the task has a requirement with a criterion in that
/// round's scope under the run's specification. Not given the write lease until the round closes.
pub(super) fn held_by_open_rounds(
    conn: &rusqlite::Connection,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = conn
        .prepare(
            "WITH open AS (SELECT e.payload_json AS p FROM events e WHERE e.kind = ?2 AND e.run_id = ?1 \
               AND NOT EXISTS (SELECT 1 FROM events c WHERE c.kind = ?3 AND c.run_id = ?1 \
                 AND json_extract(c.payload_json, '$.round_id') = json_extract(e.payload_json, '$.round_id')) \
               AND EXISTS (SELECT 1 FROM issues i WHERE i.state IN ('OPEN', 'REOPENED') \
                 AND i.candidate_id = json_extract(e.payload_json, '$.candidate_id'))), \
             scope AS (SELECT DISTINCT a.value AS ac_id FROM open o, json_each(o.p, '$.acceptance_ids') a) \
             SELECT DISTINCT t.task_id FROM tasks t JOIN runs r ON r.run_id = t.run_id \
             JOIN task_requirements tr ON tr.task_id = t.task_id AND tr.spec_id = r.spec_id \
             JOIN acceptance_criteria ac ON ac.requirement_id = tr.requirement_id AND ac.spec_id = tr.spec_id \
             WHERE t.run_id = ?1 AND t.writer = 1 AND ac.ac_id IN (SELECT ac_id FROM scope)",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id, ROUND_OPENED, ROUND_CLOSED], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// A development task a round verifies, at its revision once VERIFYING (the revision its handoffs expect).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Source {
    pub task_id: String,
    pub revision: i64,
}

/// What a round is opened with.
pub(super) struct Opening<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub spec_id: &'a str,
    pub spec_sha256: &'a str,
    pub spec_revision: u64,
    pub candidate: &'a Candidate,
    /// Its sources; at least one.
    pub sources: Vec<Source>,
    /// The developer attempt whose completion opens it.
    pub attempt_id: &'a str,
    /// The event its handoffs follow.
    pub cause: &'a str,
    /// The index, in the caller's commit, of the event its outbox rows hang on.
    pub outbox_event: usize,
    /// A reverification's round: the inconclusive round it follows and the first round of their chain (decision 12).
    pub follows: Option<(&'a str, &'a str)>,
}

/// A round opened: its id, its `verification.round_opened` event and its handoffs, for the caller's commit (its two
/// verifier tasks are already written in the transaction).
pub(super) struct Opened {
    pub round_id: String,
    pub event: NewEvent,
    pub outbox: Vec<NewOutbox>,
    pub handoffs: Vec<String>,
}

/// Open the candidate's next round in `tx`: its number, the union of its sources' criteria under the run's
/// specification (never empty), the handoff to each verifier at its current epoch (enveloped by the first source,
/// M2T16D-659) and the two verifier tasks: `child_id(round, "task/<role>")`, readers in layer 0, READY, with no
/// task_requirements (their scope is the handoff's acceptance_ids, M2T16D-258). Runtime's: decision 10.
pub(super) fn open_in(tx: &Transaction<'_>, o: &Opening<'_>) -> Result<Opened, PersistError> {
    let mut sources = o.sources.clone();
    sources.sort_by(|a, b| a.task_id.cmp(&b.task_id));
    sources.dedup_by(|a, b| a.task_id == b.task_id);
    let first = sources.first().ok_or_else(|| {
        PersistError::IntegrityViolation("a round with no source task".to_owned())
    })?;
    let opened: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE kind = ?1 AND run_id = ?2 \
             AND json_extract(payload_json, '$.candidate_id') = ?3",
            params![ROUND_OPENED, o.run_id, o.candidate.id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let number = opened + 1;
    let round_id = child_id(&o.candidate.id, &format!("round/{number}"));
    let mut acceptance_ids = Vec::new();
    for source in &sources {
        acceptance_ids.extend(criteria(tx, &source.task_id, o.spec_id)?);
    }
    acceptance_ids.sort();
    acceptance_ids.dedup();
    let mut recipients = Vec::with_capacity(super::results::VERIFIERS.len());
    for role in super::results::VERIFIERS {
        let binding = load_binding(tx, o.run_id, role)?.ok_or_else(|| {
            PersistError::IntegrityViolation(format!("run {} has no {role} bound", o.run_id))
        })?;
        recipients.push(Recipient {
            role: role.to_owned(),
            role_epoch: binding.role_epoch,
        });
    }
    let verification = Verification {
        round_id: round_id.clone(),
        run_id: o.run_id.to_owned(),
        task_id: first.task_id.clone(),
        attempt_id: o.attempt_id.to_owned(),
        candidate: o.candidate.clone(),
        spec_sha256: o.spec_sha256.to_owned(),
        spec_revision: o.spec_revision,
        acceptance_ids: acceptance_ids.clone(),
        goal: super::results::goal(tx, o.run_id, &first.task_id, &o.candidate.id)?,
        task_revision: u64::try_from(first.revision)
            .map_err(|_| PersistError::IntegrityViolation("task revision".to_owned()))?,
        cause: o.cause.to_owned(),
        // What the developer's attempt produced, by reference only (docs/adr/ADR-026.md decision 10).
        references: super::context::references(
            tx,
            &[o.attempt_id.to_owned()],
            super::results::REFERENCES,
        )?,
        budget_tokens: super::context::budget(tx, o.run_id)?,
    };
    // No acceptance criteria, nothing to verify against: refused whole (REQ-022, REQ-053 behaviour 8).
    let messages = handoff::verification(&verification, &recipients).ok_or_else(|| {
        PersistError::IntegrityViolation(format!(
            "round {round_id} has no acceptance criteria under the run's specification"
        ))
    })?;
    let mut tasks = serde_json::Map::new();
    for role in super::results::VERIFIERS {
        let task_id = child_id(&round_id, &format!("task/{role}"));
        tx.execute(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, layer) \
             VALUES (?1, ?2, ?3, 'READY', 0, ?4, 0, \
             (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks WHERE run_id = ?2), 0)",
            params![task_id, o.run_id, o.spec_id, role],
        )
        .map_err(sqlite_error)?;
        tasks.insert(role.to_owned(), json!(task_id));
    }
    let handoffs: Vec<String> = messages.iter().map(|m| m.message_id.clone()).collect();
    let outbox = messages
        .iter()
        .map(|m| NewOutbox::hcp(o.outbox_event, m))
        .collect::<Result<Vec<_>, _>>()?;
    let event = NewEvent {
        project_id: o.project_id.to_owned(),
        event_id: child_id(&round_id, "opened"),
        run_id: Some(o.run_id.to_owned()),
        kind: ROUND_OPENED.to_owned(),
        payload: json!({
            "round_id": round_id,
            "candidate_id": o.candidate.id,
            "number": number,
            "source_task_ids": sources.iter().map(|s| s.task_id.clone()).collect::<Vec<_>>(),
            "acceptance_ids": acceptance_ids,
            "tasks": tasks,
            "handoffs": handoffs,
        }),
    };
    let mut event = event;
    if let (Some((previous, root)), Some(payload)) = (o.follows, event.payload.as_object_mut()) {
        payload.insert("reverify_of".to_owned(), json!(previous));
        payload.insert("chain_root".to_owned(), json!(root));
    }
    Ok(Opened {
        round_id,
        event,
        outbox,
        handoffs,
    })
}

/// Open the next round after inconclusive round `previous` on candidate `candidate_id` for `sources`, the same scope
/// (decision 11's `reverify`), counted on chain `root`: its id.
pub(super) fn reverify(
    tx: &Transaction<'_>,
    run_id: &str,
    candidate_id: &str,
    previous: &str,
    root: &str,
    sources: Vec<Source>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<String, PersistError> {
    let (project_id, spec_id, spec_sha256, spec_revision): (String, String, String, i64) = tx
        .query_row(
            "SELECT r.project_id, r.spec_id, s.sha256, s.revision FROM runs r JOIN specs s ON s.spec_id = r.spec_id \
             WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(sqlite_error)?;
    let (candidate, _) = super::results::load_candidate(tx, run_id, candidate_id)?
        .ok_or_else(|| PersistError::IntegrityViolation(format!("no candidate {candidate_id}")))?;
    let cause = child_id(previous, "closed");
    let opened = open_in(
        tx,
        &Opening {
            project_id: &project_id,
            run_id,
            spec_id: &spec_id,
            spec_sha256: &spec_sha256,
            spec_revision: u64::try_from(spec_revision)
                .map_err(|_| PersistError::IntegrityViolation("spec revision".to_owned()))?,
            candidate: &candidate,
            sources,
            attempt_id: previous,
            cause: &cause,
            outbox_event: 0,
            follows: Some((previous, root)),
        },
    )?;
    super::writer::write(
        tx,
        &super::writer::Commit {
            events: vec![opened.event],
            outbox: opened.outbox,
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    Ok(opened.round_id)
}

/// The VERIFYING development tasks of run `run_id` other than `except` whose current round (the latest that names
/// them) is on a candidate that is no longer under verification (STALE, REJECTED or INVALID): a newly frozen candidate
/// takes them into its round, at their revision (decision 9's supersession; never run again, never moved).
pub(super) fn superseded_sources(
    tx: &Transaction<'_>,
    run_id: &str,
    except: &str,
) -> Result<Vec<Source>, PersistError> {
    let mut statement = tx
        .prepare(
            "WITH named AS (SELECT e.seq, json_extract(e.payload_json, '$.candidate_id') AS candidate_id, \
               s.value AS task_id FROM events e, json_each(e.payload_json, '$.source_task_ids') s \
               WHERE e.kind = ?1 AND e.run_id = ?2), \
             current AS (SELECT n.task_id, n.candidate_id FROM named n \
               WHERE n.seq = (SELECT max(m.seq) FROM named m WHERE m.task_id = n.task_id)) \
             SELECT c.task_id, t.revision FROM current c JOIN tasks t ON t.task_id = c.task_id \
             JOIN candidates k ON k.candidate_id = c.candidate_id \
             WHERE t.state = 'VERIFYING' AND k.state IN ('STALE', 'REJECTED', 'INVALID') AND c.task_id != ?3 \
             ORDER BY c.task_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id, except], |r| {
            Ok(Source {
                task_id: r.get(0)?,
                revision: r.get(1)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// Task `task_id`'s acceptance criteria under specification `spec_id`, sorted.
fn criteria(
    tx: &Transaction<'_>,
    task_id: &str,
    spec_id: &str,
) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT ac.ac_id FROM task_requirements tr JOIN acceptance_criteria ac \
             ON ac.requirement_id = tr.requirement_id AND ac.spec_id = tr.spec_id \
             WHERE tr.task_id = ?1 AND tr.spec_id = ?2 ORDER BY ac.ac_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![task_id, spec_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The kind of the event that issues a round's handoff again at its role's current epoch.
pub(super) const HANDOFF_REISSUED: &str = "verification.handoff_reissued";

/// The round whose verifier task `task_id` is, and the first round of its chain (a reverification's round names it;
/// docs/adr/ADR-028.md decision 12).
pub(super) fn round_of_task(
    tx: &rusqlite::Connection,
    run_id: &str,
    task_id: &str,
) -> Result<Option<(String, String)>, PersistError> {
    tx.query_row(
        "SELECT json_extract(payload_json, '$.round_id'), \
         coalesce(json_extract(payload_json, '$.chain_root'), json_extract(payload_json, '$.round_id')) \
         FROM events WHERE kind = ?1 AND run_id = ?2 \
         AND ?3 IN (json_extract(payload_json, '$.tasks.qa'), json_extract(payload_json, '$.tasks.reviewer'))",
        params![ROUND_OPENED, run_id, task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
    .map_err(sqlite_error)
}

/// A verifier task whose attempt `attempt_id` ended UNKNOWN_EFFECT (decision 11, REQ-041 behaviour 2): BLOCKED for a
/// decision that confirms the effect, never run again by itself; its move (blocked_reason set here) and need_decision.
/// None for any other task, or a verifier task no longer RUNNING (a cancel's).
pub(super) fn unknown_effect(
    tx: &Transaction<'_>,
    (project_id, run_id, task_id): (&str, &str, &str),
    (state, revision): (crate::protocol::TaskState, i64),
    attempt_id: &str,
) -> Result<Option<(super::writer::StateCas, NewEvent)>, PersistError> {
    if state != crate::protocol::TaskState::Running || round_of_task(tx, run_id, task_id)?.is_none()
    {
        return Ok(None);
    }
    tx.execute(
        "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
        params![task_id],
    )
    .map_err(sqlite_error)?;
    Ok(Some((
        super::writer::StateCas {
            id: task_id.to_owned(),
            expected_revision: revision,
            change: super::writer::Change::Task(crate::protocol::TaskState::Blocked),
        },
        NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(attempt_id, "need_decision"),
            run_id: Some(run_id.to_owned()),
            kind: "need_decision".to_owned(),
            payload: json!({"run_id": run_id, "task_id": task_id, "attempt_id": attempt_id,
                "reason": "verifier_unknown_effect",
                "detail": "the verifier's attempt ended with an effect not confirmed; it is not run again until a \
                    decision confirms what it did"}),
        },
    )))
}

/// Run `run_id`'s rounds' source tasks.
pub(super) fn sources(
    tx: &rusqlite::Connection,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT DISTINCT s.value FROM events e, json_each(e.payload_json, '$.source_task_ids') s \
             WHERE e.kind = ?1 AND e.run_id = ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The tasks of run `run_id` with a repair refusal no decision has cleared (docs/adr/ADR-028.md decision 12, M2T16D-800):
/// a `need_decision` of an exhausted repair after the task's last `repair.refusal_cleared`.
pub(super) fn refused(
    tx: &rusqlite::Connection,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT DISTINCT json_extract(n.payload_json, '$.task_id') FROM events n WHERE n.kind = 'need_decision' \
             AND n.run_id = ?1 AND json_extract(n.payload_json, '$.reason') IN \
               ('local_repairs_exhausted', 'protocol_repairs_exhausted', 'reverify_exhausted') \
             AND n.seq > coalesce((SELECT max(c.seq) FROM events c WHERE c.kind = 'repair.refusal_cleared' \
               AND c.run_id = ?1 AND json_extract(c.payload_json, '$.task_id') = json_extract(n.payload_json, '$.task_id')), 0)",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The questions round `round_id`'s verifiers asked that were deferred (`request.deferred` of its verifier tasks'
/// attempts, docs/adr/ADR-028.md decision 13, M2T16D-559): carried by the round's later need_decisions.
pub(super) fn deferred_of(
    tx: &rusqlite::Connection,
    round_id: &str,
) -> Result<Vec<serde_json::Value>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT d.payload_json FROM events d JOIN attempts a ON a.attempt_id = json_extract(d.payload_json, '$.attempt_id') \
             JOIN events o ON o.kind = ?1 AND json_extract(o.payload_json, '$.round_id') = ?2 \
             AND a.task_id IN (json_extract(o.payload_json, '$.tasks.qa'), json_extract(o.payload_json, '$.tasks.reviewer')) \
             WHERE d.kind = 'request.deferred' ORDER BY d.seq",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, round_id], |r| r.get::<_, String>(0))
        .map_err(sqlite_error)?;
    rows.map(|row| {
        let payload = row.map_err(sqlite_error)?;
        Ok(serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null))
    })
    .collect()
}

/// `commit`'s need_decision events with the round's deferred questions `deferred` added (M2T16D-559).
pub(super) fn with_deferred(commit: &mut super::writer::Commit, deferred: &[serde_json::Value]) {
    if deferred.is_empty() {
        return;
    }
    for event in commit
        .events
        .iter_mut()
        .filter(|e| e.kind == "need_decision")
    {
        event.payload["deferred"] = json!(deferred);
    }
}

/// The verifier tasks of run `run_id`'s rounds.
pub(super) fn verifier_tasks(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT t.value FROM events e, json_each(e.payload_json, '$.tasks') t \
             WHERE e.kind = ?1 AND e.run_id = ?2",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// What a verifier task's dispatch may go on with (decisions 9 and 10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Admission {
    /// Dispatch it with this handoff, its round's current one (issued again in the transaction when its epoch was
    /// older than the binding's).
    Go { round_id: String, handoff: String },
    /// Refused, written in the transaction: the task BLOCKED for a decision with `integrity_violation`.
    Refused,
}

/// The admission of verifier task `task_id` (READY at `revision`) of run `run_id`, about to be leased to its role's
/// binding at `role_epoch` with `session`: its round's current handoff, which must decode as a valid Message whose
/// package agrees with its envelope (refused otherwise: `message.invalid`, the message DEAD_LETTER), issued again at the
/// current epoch when older (REQ-022 behaviour 8: the old one DEAD_LETTER, `STALE_ROLE_EPOCH`), and a session of its
/// own: not one a source attempt used, nor the Commander's or the other verifier's (SESSION_NOT_INDEPENDENT, REQ-036
/// failure mode 5). Refusals leave no lease and no attempt (implementation correction 1). `None`: not a verifier task.
pub(super) fn admit(
    tx: &Transaction<'_>,
    ctx: &super::writer::Ctx<'_>,
    project_id: &str,
    run_id: &str,
    (task_id, revision): (&str, i64),
    role_epoch: u64,
    session: Option<&str>,
) -> Result<Option<Admission>, PersistError> {
    let opened: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE kind = ?1 AND run_id = ?2 \
             AND ?3 IN (json_extract(payload_json, '$.tasks.qa'), json_extract(payload_json, '$.tasks.reviewer'))",
            params![ROUND_OPENED, run_id, task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(opened) = opened else {
        return Ok(None);
    };
    let opened: serde_json::Value = serde_json::from_str(&opened)
        .map_err(|e| PersistError::IntegrityViolation(format!("round of {task_id}: {e}")))?;
    let round_id = opened["round_id"].as_str().unwrap_or_default().to_owned();
    let role = super::results::VERIFIERS
        .into_iter()
        .find(|r| opened["tasks"][r] == json!(task_id))
        .ok_or_else(|| PersistError::IntegrityViolation(format!("round {round_id}: {task_id}")))?;
    // Its round's current handoff: the original, or the latest issued again.
    let original = child_id(&round_id, &format!("verify/{role}"));
    let current: Option<String> = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.to') FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.round_id') = ?2 AND json_extract(payload_json, '$.role') = ?3 \
             ORDER BY seq DESC LIMIT 1",
            params![HANDOFF_REISSUED, round_id, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let handoff = current.unwrap_or(original);
    let (state, payload): (String, String) = tx
        .query_row(
            "SELECT state, payload_json FROM outbox WHERE message_id = ?1",
            params![handoff],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let refuse = |reason: &str, detail: String, invalid: Option<&str>| {
        refuse(
            tx,
            ctx,
            &Refusal {
                project_id,
                run_id,
                task_id,
                revision,
                round_id: &round_id,
                cause: "dispatch-refused",
                reason,
                detail,
                invalid: invalid.map(|violations| (handoff.as_str(), role, violations)),
                facts: serde_json::Value::Null,
            },
        )
        .map(|()| Some(Admission::Refused))
    };
    let message = serde_json::from_str::<serde_json::Value>(&payload)
        .map_err(|e| e.to_string())
        .and_then(|v| {
            crate::protocol::decode::<crate::protocol::Message>(&v).map_err(|e| e.to_string())
        })
        .and_then(|m| m.check_payload().map(|()| m));
    let message = match message {
        Ok(m) => m,
        Err(violations) => {
            return refuse(
                "INVALID_HANDOFF",
                format!("handoff {handoff} is not a valid verification handoff"),
                Some(&violations),
            );
        }
    };
    // A session of its own (decision 10; docs/adr/ADR-029.md decision 7a, the predicate the report's intake applies).
    let others = conflicting_sessions(tx, run_id, &round_id, role)?;
    if let Some(session) = session.filter(|s| others.contains(*s)) {
        return refuse(
            "SESSION_NOT_INDEPENDENT",
            format!(
                "the {role} would verify in session {session}, which a source attempt, the Commander, the Developer \
                 or the other verifier uses"
            ),
            None,
        );
    }
    if message.role_epoch == role_epoch {
        return Ok(Some(Admission::Go { round_id, handoff }));
    }
    // Written at an older epoch: refused (DEAD_LETTER, nothing else changed) and issued again at the current one, the
    // package the same but for its epoch (M2T16D-204).
    if matches!(state.as_str(), "PENDING" | "DELIVERING") {
        tx.execute(
            "UPDATE outbox SET state = 'DEAD_LETTER', last_error = 'STALE_ROLE_EPOCH' WHERE message_id = ?1",
            params![handoff],
        )
        .map_err(sqlite_error)?;
    }
    let mut again = message.clone();
    again.message_id = child_id(&handoff, &format!("e{role_epoch}"));
    again.idempotency_key = again.message_id.clone();
    again.role_epoch = role_epoch;
    again
        .payload
        .insert("role_epoch".to_owned(), json!(role_epoch));
    let mut events = vec![NewEvent {
        project_id: project_id.to_owned(),
        event_id: child_id(&again.message_id, "issued"),
        run_id: Some(run_id.to_owned()),
        kind: HANDOFF_REISSUED.to_owned(),
        payload: json!({
            "round_id": round_id, "role": role, "from": handoff, "to": again.message_id, "role_epoch": role_epoch,
        }),
    }];
    if matches!(state.as_str(), "PENDING" | "DELIVERING") {
        events.push(NewEvent {
            project_id: project_id.to_owned(),
            event_id: child_id(&handoff, "dead_letter"),
            run_id: Some(run_id.to_owned()),
            kind: "outbox.dead_letter".to_owned(),
            payload: json!({
                "message_id": handoff, "recipient_role": role, "reason": "STALE_ROLE_EPOCH",
                "role_epoch": message.role_epoch, "current_role_epoch": role_epoch,
            }),
        });
    }
    super::writer::write(
        tx,
        &super::writer::Commit {
            events,
            outbox: vec![NewOutbox::hcp(0, &again)?],
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    Ok(Some(Admission::Go {
        round_id,
        handoff: again.message_id,
    }))
}

/// The sessions verifier role `role` of round `round_id` of run `run_id` may not use (docs/adr/ADR-029.md decision
/// 7a; one predicate for the dispatch and the report's intake): those of every attempt of the round's source tasks,
/// the commander's, the developer's and the other verifier role's bindings now, and those of every attempt of the
/// round's other verifier task.
pub(super) fn conflicting_sessions(
    tx: &rusqlite::Connection,
    run_id: &str,
    round_id: &str,
    role: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let other = super::results::VERIFIERS
        .into_iter()
        .find(|r| *r != role)
        .unwrap_or(role);
    let mut statement = tx
        .prepare(
            "WITH round AS (SELECT payload_json AS p FROM events WHERE kind = ?1 AND run_id = ?2 \
               AND json_extract(payload_json, '$.round_id') = ?3) \
             SELECT a.session_id FROM attempts a, round WHERE a.session_id IS NOT NULL \
               AND (a.task_id IN (SELECT s.value FROM json_each(round.p, '$.source_task_ids') s) \
                 OR a.task_id = json_extract(round.p, '$.tasks.' || ?4)) \
             UNION SELECT b.session_id FROM role_bindings b WHERE b.run_id = ?2 AND b.session_id IS NOT NULL \
               AND b.role IN ('commander', 'developer', ?4)",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![ROUND_OPENED, run_id, round_id, other], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// A verifier task refused: its dispatch, or its report at its intake (docs/adr/ADR-029.md decision 7).
pub(super) struct Refusal<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub task_id: &'a str,
    pub revision: i64,
    pub round_id: &'a str,
    /// What refused it, naming its events with the task's revision: `dispatch-refused` or `report-rejected`.
    pub cause: &'a str,
    pub reason: &'a str,
    pub detail: String,
    /// An invalid handoff: its id, the role it is for, and what is wrong with it.
    pub invalid: Option<(&'a str, &'a str, &'a str)>,
    /// More of what the `integrity_violation` records (an object; `null` for none).
    pub facts: serde_json::Value,
}

/// Write refusal `r`: the task (READY at its dispatch, RUNNING at its report's intake) -> BLOCKED (NEED_DECISION) with
/// `need_decision` and `integrity_violation`; an invalid handoff also `message.invalid` and DEAD_LETTER.
pub(super) fn refuse(
    tx: &Transaction<'_>,
    ctx: &super::writer::Ctx<'_>,
    r: &Refusal<'_>,
) -> Result<(), PersistError> {
    tx.execute(
        "UPDATE tasks SET blocked_reason = 'NEED_DECISION', next_retry_at = NULL WHERE task_id = ?1",
        params![r.task_id],
    )
    .map_err(sqlite_error)?;
    let key = |what: &str| child_id(r.task_id, &format!("{}/{}/{what}", r.cause, r.revision));
    let event = |event_id: String, kind: &str, payload: serde_json::Value| NewEvent {
        project_id: r.project_id.to_owned(),
        event_id,
        run_id: Some(r.run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let mut events = Vec::new();
    if let Some((message_id, role, violations)) = r.invalid {
        tx.execute(
            "UPDATE outbox SET state = 'DEAD_LETTER', last_error = 'INVALID' WHERE message_id = ?1",
            params![message_id],
        )
        .map_err(sqlite_error)?;
        events.push(event(
            child_id(message_id, &format!("{role}/invalid")),
            "message.invalid",
            json!({"message_id": message_id, "consumer": role, "violations": violations}),
        ));
    }
    let mut integrity = json!({"task_id": r.task_id, "round_id": r.round_id, "reason": r.reason, "detail": r.detail});
    for (key, value) in r.facts.as_object().into_iter().flatten() {
        integrity[key] = value.clone();
    }
    events.push(event(key("integrity"), "integrity_violation", integrity));
    events.push(event(
        key("need_decision"),
        "need_decision",
        json!({"run_id": r.run_id, "task_id": r.task_id, "reason": r.reason, "detail": r.detail}),
    ));
    super::writer::write(
        tx,
        &super::writer::Commit {
            cas: vec![super::writer::StateCas {
                id: r.task_id.to_owned(),
                expected_revision: r.revision,
                change: super::writer::Change::Task(crate::protocol::TaskState::Blocked),
            }],
            events,
            ..super::writer::Commit::default()
        },
        ctx,
    )
    .map(drop)
}

/// A verifier's request through its bridge that the Runtime answers itself (docs/adr/ADR-028.md decisions 7 and 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifierRequest {
    /// A request that the candidate under verification be changed, while its round is open: refused with an Issue.
    RejectChange(ChangeRequest),
    /// Any other message: deferred (routing between roles is M3-T04's, CR-018).
    Defer(Deferred),
}

/// A refused change request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeRequest {
    pub run_id: String,
    pub attempt_id: String,
    pub message_id: String,
    pub round_id: String,
    pub candidate_id: String,
    /// As the request gave it; anything but critical, high, medium or low is medium.
    pub severity: String,
    /// The request's text, the Issue's description.
    pub text: String,
}

/// A deferred message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deferred {
    pub run_id: String,
    pub attempt_id: String,
    pub message_id: String,
    pub recipient_role: String,
}

/// Answer `request` in one transaction: the Issue a refused change request opened (the same one when asked again), or
/// none.
pub(super) fn request(
    conn: &mut rusqlite::Connection,
    request: &VerifierRequest,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Option<String>, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let (run_id, attempt_id, message_id) = match request {
        VerifierRequest::RejectChange(r) => (&r.run_id, &r.attempt_id, &r.message_id),
        VerifierRequest::Defer(d) => (&d.run_id, &d.attempt_id, &d.message_id),
    };
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let key = child_id(attempt_id, &format!("request/{message_id}"));
    let event = |event_id: String, kind: &str, payload: serde_json::Value| NewEvent {
        project_id: project_id.clone(),
        event_id,
        run_id: Some(run_id.clone()),
        kind: kind.to_owned(),
        payload,
    };
    let answered: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id IN (?1, ?2))",
            params![child_id(&key, "rejected"), child_id(&key, "deferred")],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let (events, issue) = match request {
        // Answered before (asked again): the same answer, nothing written.
        VerifierRequest::RejectChange(_) if answered => (Vec::new(), Some(key.clone())),
        VerifierRequest::Defer(_) if answered => (Vec::new(), None),
        VerifierRequest::RejectChange(r) => {
            let severity = match r.severity.as_str() {
                s @ ("critical" | "high" | "medium" | "low") => s,
                _ => "medium",
            };
            let fingerprint = crate::verification::issue_fingerprint(severity, &r.text);
            tx.execute(
                "INSERT INTO issues(issue_id, run_id, candidate_id, fingerprint, severity, state, description) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'OPEN', ?6)",
                params![key, r.run_id, r.candidate_id, fingerprint, severity, r.text],
            )
            .map_err(sqlite_error)?;
            (
                vec![
                    event(
                        child_id(&key, "rejected"),
                        "request.rejected",
                        json!({"attempt_id": r.attempt_id, "message_id": r.message_id, "round_id": r.round_id,
                            "reason_code": "ILLEGAL_TRANSITION",
                            "detail": "the candidate under verification is not changed; an Issue for the next candidate"}),
                    ),
                    event(
                        child_id(&key, "issue"),
                        "issue.opened",
                        json!({"issue_id": key, "round_id": r.round_id, "candidate_id": r.candidate_id,
                            "severity": severity, "fingerprint": fingerprint, "attempt_id": r.attempt_id,
                            "source": "change_request"}),
                    ),
                ],
                Some(key.clone()),
            )
        }
        VerifierRequest::Defer(d) => (
            vec![event(
                child_id(&key, "deferred"),
                "request.deferred",
                json!({"attempt_id": d.attempt_id, "message_id": d.message_id, "recipient_role": d.recipient_role}),
            )],
            None,
        ),
    };
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events,
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    // A change request's Issue is a present fact of its fingerprint (docs/adr/ADR-029.md decision 10, M2T18D-701).
    super::issues::settle_all(&tx, run_id)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(issue)
}
