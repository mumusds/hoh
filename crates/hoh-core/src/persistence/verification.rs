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
use rusqlite::{Transaction, params};
use serde_json::json;

/// The kind of the event that opens a round.
pub(super) const ROUND_OPENED: &str = "verification.round_opened";

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
    Ok(Opened {
        round_id,
        event,
        outbox,
        handoffs,
    })
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
