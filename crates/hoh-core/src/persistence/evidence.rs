//! A check's evidence on a frozen candidate (REQ-052 behaviours 5 to 8; docs/adr/ADR-027.md principles one and two,
//! decisions 7 to 9): one transaction writes the evidence row, its acceptance rows, its objects, its producer event,
//! the integrity event when the source changed, and the candidate's veto.

use super::runner::{ArtifactRow, artifact_id, register};
use super::{PersistError, sqlite_error};
use crate::verification::{Outcome, Record, Status};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

/// One acceptance row: an AC of the run's spec, the status the evidence concludes on it, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceRow {
    pub ac_id: String,
    pub spec_id: String,
    pub status: &'static str,
    pub reason: String,
    /// A FAIL row's failing result lines' names, sorted (docs/adr/ADR-028.md implementation correction 5).
    pub failing: Vec<String>,
}

/// The key of an acceptance row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceKey {
    pub ac_id: String,
    pub spec_id: String,
    pub candidate_id: String,
    pub evidence_id: String,
}

/// Change acceptance row `key` to `status` (docs/adr/ADR-029.md decision 6): the UPDATE is issued and the append-only
/// guard refuses it; the transaction rolls back and the refusal is [`PersistError::AppendOnly`].
pub(super) fn amend(
    conn: &mut rusqlite::Connection,
    key: &AcceptanceKey,
    status: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let changed = tx
        .execute(
            "UPDATE acceptance_results SET status = ?5 WHERE ac_id = ?1 AND spec_id = ?2 AND candidate_id = ?3 \
             AND evidence_id = ?4",
            params![
                key.ac_id,
                key.spec_id,
                key.candidate_id,
                key.evidence_id,
                status
            ],
        )
        .map_err(super::append_only_error)?;
    // The guard refuses every change; reaching here means it is missing, and nothing may change all the same.
    drop(tx);
    Err(PersistError::IntegrityViolation(format!(
        "the append-only guard of acceptance_results is missing ({changed} rows matched; rolled back)"
    )))
}

/// What a check's driver hands the store after the command ran and its record was judged.
#[derive(Debug, Clone, PartialEq)]
pub struct Recording {
    pub record: Record,
    /// The record itself kept as an object (the evidence row's artifact).
    pub record_object: ArtifactRow,
    /// Its stdout, stderr and declared artifacts kept as objects.
    pub objects: Vec<ArtifactRow>,
    pub outcome: Outcome,
    /// The validity reasons (`Reason::code`), empty when valid.
    pub reasons: Vec<&'static str>,
    pub tree_before: Value,
    pub tree_after: Value,
    /// The clocks the run was measured by: its wall-clock finish and its monotonic seconds (the record's finish is its
    /// start plus those seconds, docs/adr/ADR-027.md decision 6).
    pub clock: Value,
    pub observations: Vec<Value>,
    pub rows: Vec<AcceptanceRow>,
    /// The post-check's reasons when the source or the candidate changed during the check (decision 7).
    pub integrity: Vec<String>,
}

/// Record `job` in one transaction; refused with nothing written when its candidate is unknown or its producer is not
/// a qa or reviewer attempt of the candidate's run.
pub(super) fn record(
    conn: &mut Connection,
    job: &Recording,
    ctx: &super::writer::Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let r = &job.record;
    let refused = |why: String| PersistError::EvidenceRefused(why);
    let (run_id, project_id): (String, String) = tx
        .query_row(
            "SELECT c.run_id, ru.project_id FROM candidates c JOIN runs ru ON ru.run_id = c.run_id \
             WHERE c.candidate_id = ?1",
            params![r.candidate_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| refused(format!("candidate {} is not frozen here", r.candidate_id)))?;
    // Principle two, again at insert: a qa or reviewer attempt of the candidate's run.
    let attempt = &r.producer_attempt_id;
    match super::candidates::producer(&tx, attempt)? {
        None => {
            return Err(refused(format!(
                "PRODUCER_MISSING: there is no attempt {attempt}"
            )));
        }
        Some(p) if p.run_id != run_id => {
            return Err(refused(format!(
                "PRODUCER_MISMATCH: attempt {attempt} is of run {}, candidate {} of run {run_id}",
                p.run_id, r.candidate_id
            )));
        }
        Some(p) if p.role != "qa" && p.role != "reviewer" => {
            return Err(refused(format!(
                "PRODUCER_NOT_INDEPENDENT: attempt {attempt} is a {} attempt",
                p.role
            )));
        }
        Some(_) => {}
    }
    // Recorded already (docs/adr/ADR-027.md decision 8, recovery protocol; M2T10R-101, -150): the same record again is
    // nothing to do, whoever commits it second; another record under its id is refused.
    let recorded: Option<String> = tx
        .query_row(
            "SELECT artifact_id FROM evidence WHERE evidence_id = ?1",
            params![r.record_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some(existing) = recorded {
        if existing == artifact_id(&job.record_object.sha256, &job.record_object.path) {
            return Ok(());
        }
        return Err(refused(format!(
            "record {} is recorded already, as another record ({existing})",
            r.record_id
        )));
    }
    let objects: Vec<ArtifactRow> = std::iter::once(&job.record_object)
        .chain(&job.objects)
        .cloned()
        .collect();
    register(&tx, &objects)?;
    let now = crate::time::now_rfc3339();
    // An attempt-level record is the attempt's evidence, never a planned check's (docs/adr/ADR-028.md decision 8).
    let kind = if r.command.first().map(String::as_str) == Some(crate::verification::ATTEMPT_MARKER)
    {
        "attempt"
    } else {
        "check"
    };
    tx.execute(
        "INSERT INTO evidence(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            r.record_id,
            r.candidate_id,
            attempt,
            artifact_id(&job.record_object.sha256, &job.record_object.path),
            r.check_id,
            kind,
            job.outcome.status.code(),
            r.mode,
            now
        ],
    )
    .map_err(sqlite_error)?;
    // A verification round's evidence has its acceptance rows written when the round closes (docs/adr/ADR-028.md
    // decision 11, M2T16D-455, -655); after it closed, at once, PASS and DEGRADED left out of a vetoed candidate or a
    // round with a vetoed record.
    let producer_task: Option<String> = tx
        .query_row(
            "SELECT task_id FROM attempts WHERE attempt_id = ?1",
            params![attempt],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let round = match &producer_task {
        Some(task) => super::verification::round_of_task(&tx, &run_id, task)?,
        None => None,
    };
    let round_closed = match &round {
        Some((round_id, _)) => tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
                params![crate::ids::child_id(round_id, "closed")],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sqlite_error)?,
        None => false,
    };
    let deferred = round.is_some() && !round_closed;
    // Its round had a vetoed planned-check record, whatever its close's verdict (the close's own rule; implementation
    // correction 4, M2T16R-59, -103).
    let invalidated = match &round {
        Some((round_id, _)) if round_closed => tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events o JOIN attempts a ON a.task_id IN \
                 (json_extract(o.payload_json, '$.tasks.qa'), json_extract(o.payload_json, '$.tasks.reviewer')) \
                 JOIN evidence ev ON ev.attempt_id = a.attempt_id \
                 WHERE o.run_id = ?2 AND o.kind = 'verification.round_opened' \
                 AND json_extract(o.payload_json, '$.round_id') = ?1 AND ev.kind = 'check' AND ev.outcome = 'INVALID')",
                params![round_id, run_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(sqlite_error)?,
        _ => false,
    };
    let skip_passes = round_closed
        && (job.outcome.status == Status::Invalid
            || invalidated
            || tx
                .query_row(
                    "SELECT state = 'INVALID' FROM candidates WHERE candidate_id = ?1",
                    params![r.candidate_id],
                    |row| row.get::<_, bool>(0),
                )
                .map_err(sqlite_error)?);
    for row in job
        .rows
        .iter()
        .filter(|row| !deferred && !(skip_passes && matches!(row.status, "PASS" | "DEGRADED")))
    {
        tx.execute(
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status, reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                row.ac_id,
                row.spec_id,
                r.candidate_id,
                r.record_id,
                row.status,
                row.reason
            ],
        )
        .map_err(sqlite_error)?;
    }
    // A vetoed evidence vetoes its candidate (principle one, decision 5): INVALID is terminal.
    if job.outcome.status == Status::Invalid {
        tx.execute(
            "UPDATE candidates SET state = 'INVALID' WHERE candidate_id = ?1 \
             AND state IN ('FROZEN', 'VERIFYING', 'ACCEPTED', 'STALE')",
            params![r.candidate_id],
        )
        .map_err(sqlite_error)?;
    }
    let event = |id: String, kind: &str, payload: Value| {
        tx.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![project_id, id, run_id, kind, payload.to_string(), now],
        )
        .map(|_| ())
        .map_err(sqlite_error)
    };
    event(
        crate::ids::child_id(&r.record_id, "evidence"),
        "evidence.recorded",
        json!({
            "evidence_id": r.record_id, "record_id": r.record_id, "attempt_id": attempt, "check_id": r.check_id,
            "outcome": job.outcome.status.code(), "outcome_reason": job.outcome.reason,
            "tree_before": job.tree_before, "tree_after": job.tree_after, "clock": job.clock,
            "observations": job.observations, "reasons": job.reasons,
            "artifacts": objects.iter().map(ArtifactRow::json).collect::<Vec<_>>(),
            "rows": job.rows.iter().map(|row| json!({"ac_id": row.ac_id, "spec_id": row.spec_id,
                "status": row.status, "reason": row.reason, "failing": row.failing})).collect::<Vec<_>>(),
            "deferred_rows": deferred,
        }),
    )?;
    if !job.integrity.is_empty() {
        event(
            crate::ids::child_id(&r.record_id, "integrity"),
            "integrity_violation",
            json!({
                "candidate_id": r.candidate_id, "check_id": r.check_id, "record_id": r.record_id,
                "reasons": job.integrity, "tree_before": job.tree_before, "tree_after": job.tree_after,
            }),
        )?;
    }
    // Its rows and its veto change what resolves an Issue (docs/adr/ADR-029.md decision 10): materialised before the
    // commit.
    super::issues::settle_all(&tx, &run_id)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)
}

#[cfg(test)]
mod tests;
