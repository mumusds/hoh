//! Approved specifications and their derived rows (REQ-034; ADR-033).
//! The host supplies bytes; this module owns their one approval transaction.

use super::commands::SubmitCommand;
use super::writer::{self, Change, Commit, Ctx, Entity, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{ApproveSpec, RunState, SpecDecision};
use crate::specindex::{self, Index, IndexError, Source};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

/// A workspace snapshot read by the host. A tracked index is its working-tree bytes, never a generated substitute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecFiles {
    pub sources: Vec<Source>,
    pub tracked_index: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecApproval {
    pub approval: ApproveSpec,
    pub files: Result<SpecFiles, IndexError>,
}

pub(super) fn approve(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    input: &SpecApproval,
    ctx: &Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    let run: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT state,revision,project_id FROM runs WHERE run_id=?1",
            params![command.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, project)) = run else {
        return Err(PersistError::RunNotFound(command.run_id.clone()));
    };
    if writer::parse::<RunState>(&state)?.is_terminal() {
        return Err(PersistError::BadRequest(
            "an ended run cannot approve specifications".into(),
        ));
    }
    if project != command.event.project_id {
        return Err(PersistError::BadRequest(
            "run belongs to another project".into(),
        ));
    }
    if revision != command.expected_revision {
        return Err(PersistError::StaleRevision {
            entity: Entity::Run,
            id: command.run_id.clone(),
            expected: command.expected_revision,
            actual: Some(revision),
        });
    }
    let files = input
        .files
        .as_ref()
        .map_err(|e| PersistError::BadRequest(e.to_string()))?;
    let index = specindex::generate_index(&files.sources)
        .map_err(|e| PersistError::BadRequest(e.to_string()))?;
    if index.sha256() != input.approval.spec_sha256
        || files.tracked_index.as_deref() != Some(index.bytes())
    {
        return Err(PersistError::BadRequest(
            "specification digest conflicts with the generated or tracked index".into(),
        ));
    }
    let latest: Option<String> = tx
        .query_row(
            "SELECT spec_id FROM specs WHERE project_id=?1 ORDER BY revision DESC LIMIT 1",
            params![project],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if latest.is_some() {
        return Err(PersistError::BadRequest(
            "a later specification revision requires a proposed change".into(),
        ));
    }
    if input.approval.decision == SpecDecision::Reject {
        return Err(PersistError::BadRequest(
            "there is no proposed change to reject".into(),
        ));
    }
    let id = child_id(&command.command_id, "spec");
    insert(tx, &project, &id, 1, &index)?;
    tx.execute(
        "UPDATE runs SET spec_id=?1 WHERE run_id=?2",
        params![id, command.run_id],
    )
    .map_err(sqlite_error)?;
    let mut event = command.event.clone();
    event.kind = "spec.changed".to_owned();
    event.payload = json!({"command_id": command.command_id, "run_id": command.run_id,
        "spec_id": id, "spec_revision": 1, "spec_sha256": index.sha256(), "cause": "approved", "from": null});
    let done = writer::write(
        tx,
        &Commit {
            cas: vec![StateCas {
                id: command.run_id.clone(),
                expected_revision: revision,
                change: Change::Touch(Entity::Run),
            }],
            events: vec![event],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok((done.revisions[0], done.event_seqs[0]))
}

fn insert(
    tx: &Transaction<'_>,
    project: &str,
    id: &str,
    revision: i64,
    index: &Index,
) -> Result<(), PersistError> {
    tx.execute(
        "INSERT INTO specs(spec_id,project_id,sha256,revision) VALUES(?1,?2,?3,?4)",
        params![id, project, index.sha256(), revision],
    )
    .map_err(sqlite_error)?;
    // Only the pure parser constructs Index; these arrays and strings are guaranteed by its grammar.
    for requirement in index.value()["requirements"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let text = |key: &str| requirement[key].as_str().unwrap_or_default();
        tx.execute("INSERT INTO requirements(requirement_id,spec_id,title,required_release,release_class) VALUES(?1,?2,?3,1,?4)",
            params![text("id"), id, text("title"), text("release_class")]).map_err(sqlite_error)?;
        for ac in requirement["acceptance"].as_array().into_iter().flatten() {
            let ac_id = ac["id"].as_str().unwrap_or_default();
            let check = index.value()["scenarios"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["acceptance_id"] == ac["id"])
                .and_then(|s| s["check_id"].as_str())
                .ok_or_else(|| {
                    PersistError::IntegrityViolation(format!("missing scenario for {ac_id}"))
                })?;
            tx.execute("INSERT INTO acceptance_criteria(ac_id,requirement_id,spec_id,body,check_id) VALUES(?1,?2,?3,?4,?5)",
                params![ac_id,text("id"),id,ac.to_string(),check]).map_err(sqlite_error)?;
        }
    }
    Ok(())
}
