//! A run's frozen candidates (REQ-052 behaviours 1 to 3; decision 1 as CR-017 scopes it to one run;
//! docs/adr/ADR-027.md decisions 4 and 5): one row per content identity, owned by one run.

use super::runner::ArtifactRow;
use super::{PersistError, sqlite_error};
use crate::verification::Sealed;
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::json;

/// A candidate to freeze for `run_id`: its sealed manifest, its export kept as an object (the row's `source_path`),
/// and the bytes of its specification, environment and test plan kept as objects (decision 4).
#[derive(Debug, Clone, PartialEq)]
pub struct Freeze {
    pub run_id: String,
    pub sealed: Sealed,
    pub export: ArtifactRow,
    pub inputs: Vec<ArtifactRow>,
}

/// What a freeze did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frozen {
    /// A new FROZEN row; the run's earlier current candidates (FROZEN, VERIFYING, ACCEPTED) made STALE.
    New {
        candidate_id: String,
        superseded: Vec<String>,
    },
    /// The same content frozen before in this run: its id and its state as they are; nothing written, nothing
    /// revived (REQ-052 decision 1 within one run, CR-017).
    Existing { candidate_id: String, state: String },
}

/// The states of a run's current candidate: a newer freeze makes them STALE.
const CURRENT: &str = "('FROZEN', 'VERIFYING', 'ACCEPTED')";

/// Freeze `job` in one transaction: refused with nothing written when the run does not exist, the content belongs to
/// another run, or the specification, environment or test plan bytes are not among the objects it registers.
pub(super) fn freeze(
    conn: &mut Connection,
    job: &Freeze,
    ctx: &super::writer::Ctx<'_>,
) -> Result<Frozen, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let frozen = freeze_in(&tx, job)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(frozen)
}

/// [`freeze`] inside the caller's transaction (docs/adr/ADR-028.md decision 4: a writer attempt's completion freezes,
/// accepts and settles in one commit). The caller commits; an error leaves the transaction to be rolled back.
pub(super) fn freeze_in(tx: &Transaction<'_>, job: &Freeze) -> Result<Frozen, PersistError> {
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![job.run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| PersistError::RunNotFound(job.run_id.clone()))?;
    let id = &job.sealed.candidate_id;
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT run_id, state FROM candidates WHERE candidate_id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match existing {
        Some((run_id, state)) if run_id == job.run_id => {
            return Ok(Frozen::Existing {
                candidate_id: id.clone(),
                state,
            });
        }
        Some((run_id, _)) => {
            return Err(PersistError::CandidateOfAnotherRun {
                candidate_id: id.clone(),
                run_id,
            });
        }
        None => {}
    }
    let m = &job.sealed.manifest;
    let kept: Vec<&ArtifactRow> = std::iter::once(&job.export).chain(&job.inputs).collect();
    for (what, sha256) in [
        ("specification", &m.spec.sha256),
        ("environment", &m.environment.sha256),
        ("test plan", &m.test_plan.sha256),
    ] {
        if !kept.iter().any(|a| &a.sha256 == sha256) {
            return Err(PersistError::IntegrityViolation(format!(
                "candidate {id}: its {what} ({sha256}) is not kept as an object"
            )));
        }
    }
    let rows: Vec<ArtifactRow> = kept.into_iter().cloned().collect();
    super::runner::register(tx, &rows)?;
    let superseded: Vec<String> = {
        let mut statement = tx
            .prepare(&format!(
                "SELECT candidate_id FROM candidates WHERE run_id = ?1 AND state IN {CURRENT} ORDER BY candidate_id"
            ))
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![job.run_id], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)?
    };
    tx.execute(
        &format!("UPDATE candidates SET state = 'STALE' WHERE run_id = ?1 AND state IN {CURRENT}"),
        params![job.run_id],
    )
    .map_err(sqlite_error)?;
    let now = crate::time::now_rfc3339();
    tx.execute(
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, \
         test_plan_sha256, source_path, state, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'FROZEN', ?9)",
        params![
            id,
            job.run_id,
            m.git_commit,
            m.git_tree,
            m.spec.sha256,
            m.environment.sha256,
            m.test_plan.sha256,
            job.export.path,
            now
        ],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            project_id,
            crate::ids::child_id(id, "frozen"),
            job.run_id,
            "candidate.frozen",
            json!({"candidate_id": id, "superseded": superseded, "source_path": job.export.path}).to_string(),
            now
        ],
    )
    .map_err(sqlite_error)?;
    // Every other candidate of the run is now superseded for its reports (docs/adr/ADR-029.md decision 8): their
    // CURRENT reports STALE in this transaction.
    let others: Vec<String> = {
        let mut statement = tx
            .prepare("SELECT candidate_id FROM candidates WHERE run_id = ?1 AND candidate_id <> ?2 ORDER BY candidate_id")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(params![job.run_id, id], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)?
    };
    super::reports::stale_reports(tx, &job.run_id, &others, "superseded", id)?;
    Ok(Frozen::New {
        candidate_id: id.clone(),
        superseded,
    })
}

/// A candidate's row (contracts/sql/001_initial.sql `candidates`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRow {
    pub candidate_id: String,
    pub run_id: String,
    pub state: String,
    pub source_path: String,
    pub git_commit: String,
    pub git_tree: String,
    pub spec_sha256: String,
    pub environment_sha256: String,
    pub test_plan_sha256: String,
}

/// The candidate `candidate_id`'s row, if there is one.
pub(super) fn candidate(
    conn: &Connection,
    candidate_id: &str,
) -> Result<Option<CandidateRow>, PersistError> {
    conn.query_row(
        "SELECT candidate_id, run_id, state, source_path, git_commit, git_tree, spec_sha256, environment_sha256, \
         test_plan_sha256 FROM candidates WHERE candidate_id = ?1",
        params![candidate_id],
        |r| {
            Ok(CandidateRow {
                candidate_id: r.get(0)?,
                run_id: r.get(1)?,
                state: r.get(2)?,
                source_path: r.get(3)?,
                git_commit: r.get(4)?,
                git_tree: r.get(5)?,
                spec_sha256: r.get(6)?,
                environment_sha256: r.get(7)?,
                test_plan_sha256: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// The attempt `attempt_id`'s run and its task's role (principle two), if the attempt exists.
pub(super) fn producer(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Option<crate::verification::Producer>, PersistError> {
    conn.query_row(
        "SELECT t.run_id, t.role FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE a.attempt_id = ?1",
        params![attempt_id],
        |r| {
            Ok(crate::verification::Producer {
                run_id: r.get(0)?,
                role: r.get(1)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

/// A run's specification as the acceptance rows need it: its id and digest, and each criterion with its scenario check
/// and whether its requirement may degrade (`(ac_id, check_id, degradable)`, by ac_id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAcceptance {
    pub spec_id: String,
    pub spec_sha256: String,
    pub criteria: Vec<(String, String, bool)>,
}

/// The outcome of the evidence `evidence_id`, if it is recorded.
pub(super) fn evidence_outcome(
    conn: &Connection,
    evidence_id: &str,
) -> Result<Option<String>, PersistError> {
    conn.query_row(
        "SELECT outcome FROM evidence WHERE evidence_id = ?1",
        params![evidence_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(sqlite_error)
}

/// The run `run_id`'s specification and criteria, if the run exists and has a specification.
pub(super) fn acceptance(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<RunAcceptance>, PersistError> {
    let spec: Option<(String, String)> = conn
        .query_row(
            "SELECT s.spec_id, s.sha256 FROM runs r JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((spec_id, spec_sha256)) = spec else {
        return Ok(None);
    };
    let mut statement = conn
        .prepare(
            "SELECT a.ac_id, a.check_id, q.release_class = 'degradable' FROM acceptance_criteria a \
             JOIN requirements q ON q.requirement_id = a.requirement_id AND q.spec_id = a.spec_id \
             WHERE a.spec_id = ?1 ORDER BY a.ac_id",
        )
        .map_err(sqlite_error)?;
    let criteria = statement
        .query_map(params![spec_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<(String, String, bool)>, _>>()
        .map_err(sqlite_error)?;
    Ok(Some(RunAcceptance {
        spec_id,
        spec_sha256,
        criteria,
    }))
}

#[cfg(test)]
mod tests;
