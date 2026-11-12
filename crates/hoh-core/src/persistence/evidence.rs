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
    tx.execute(
        "INSERT INTO evidence(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'check', ?6, ?7, ?8)",
        params![
            r.record_id,
            r.candidate_id,
            attempt,
            artifact_id(&job.record_object.sha256, &job.record_object.path),
            r.check_id,
            job.outcome.status.code(),
            r.mode,
            now
        ],
    )
    .map_err(sqlite_error)?;
    for row in &job.rows {
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
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)
}

#[cfg(test)]
mod tests;
