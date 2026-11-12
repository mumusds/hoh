//! A writer attempt's completion (docs/adr/ADR-028.md decisions 3 and 4): its bridge read against its identity, its
//! workspace prepared for a freeze while the write lease is still held, and the store's one commit
//! ([`Store::complete_writer_attempt`]). The runner's own end and a reconcile of an attempt whose work was observed
//! done take the same path; neither settles a writer's success the generic way (which would leave its task
//! IMPLEMENTED).

use crate::verify::freeze::{self, FreezeError, Inputs};
use hoh_core::bridge::{self, Identity};
use hoh_core::persistence::{
    Ending, Observed, PersistError, Store, UnfinishedRow, WriterCompleted, WriterCompletion,
    WriterOutcome,
};
use hoh_core::protocol::TaskStatus;
use hoh_core::retry::FailureClass;
use serde_json::json;
use std::path::Path;

/// The freeze's inputs by the project layout's convention until the specification's own location exists (decision 4;
/// M3-T02 replaces them, CR-018): relative to the checkout.
pub(super) fn inputs() -> Inputs {
    Inputs {
        spec: "contracts/requirements.json".into(),
        environment: ".hoh/environment.json".into(),
        test_plan: "tests/check-plan.json".into(),
    }
}

/// What a successful writer attempt's bridge decides (decision 3's table), its refused lines added to `ending`'s events
/// as `bridge.rejected` (decision 2). A COMPLETED result has the project's workspace prepared for its freeze: the
/// environment description first (decision 5). Blocking (git, files): run it off the async threads.
pub(super) fn outcome(project: &Path, identity: &Identity, ending: &mut Ending) -> WriterOutcome {
    let parsed = bridge::parse(&ending.observed.messages, identity);
    for refused in &parsed.refused {
        ending.observed.events.push((
            format!("bridge/rejected/{}", refused.line),
            "bridge.rejected".to_owned(),
            json!({"attempt_id": identity.attempt_id, "line": refused.line, "line_sha256": refused.sha256,
                "reason": refused.reason}),
        ));
    }
    let correct = |reason: String| WriterOutcome::Correct {
        reasons: vec![reason],
    };
    // Cut at its limit, its last TaskResult may be lost: not trusted (M2T16D-456).
    if ending.observed.messages_truncated {
        return correct(format!(
            "its bridge was read only up to its limit ({} lines, {} bytes): write one TaskResult as its last line",
            super::product::BRIDGE_LINES,
            super::product::BRIDGE_BYTES
        ));
    }
    let Some(result) = parsed.task_result else {
        return correct(if parsed.result_invalid {
            let why = parsed
                .refused
                .last()
                .map_or(String::new(), |r| r.reason.clone());
            format!("its last TaskResult was refused: {why}")
        } else {
            "it wrote no TaskResult to its bridge".to_owned()
        });
    };
    let decide = |reason: &str| WriterOutcome::Decide {
        reason: reason.to_owned(),
        detail:
            json!({"status": result.status, "summary": result.summary, "issues": result.issues})
                .to_string(),
    };
    match result.status {
        TaskStatus::Failed => return decide("agent_failed"),
        TaskStatus::Blocked => return decide("agent_blocked"),
        TaskStatus::Incomplete => return decide("agent_incomplete"),
        TaskStatus::Completed => {}
    }
    let prepared = crate::verify::environment::ensure(project)
        .map_err(FreezeError::Io)
        .and_then(|_| {
            freeze::prepare_job(project, &project.join(".hoh"), &identity.run_id, &inputs())
        });
    match prepared {
        Ok(job) => WriterOutcome::Freeze {
            job: Box::new(job),
            result: Box::new(result),
        },
        Err(FreezeError::Refused(reasons)) => WriterOutcome::Correct {
            reasons: reasons
                .into_iter()
                .map(|r| format!("its freeze was refused: {r}"))
                .collect(),
        },
        // The host could not read or write what the freeze needs (git, the object store): not the agent's to correct,
        // so no correction is counted; the task waits for a decision (decision 3 lists only the agent's outputs).
        Err(e) => WriterOutcome::Decide {
            reason: "freeze_failed".to_owned(),
            detail: e.to_string(),
        },
    }
}

/// Complete writer attempt `identity.attempt_id`, which ended successfully with its processes proven gone: its outcome
/// decided off the async threads, then one commit carrying the workspace's record when the store asks for it (an
/// interrupted end, REQ-019 behaviour 6).
pub(super) async fn complete(
    store: &Store,
    project: &Path,
    identity: Identity,
    ending: Ending,
) -> Result<WriterCompleted, PersistError> {
    let held = project.to_path_buf();
    let attempt_id = identity.attempt_id.clone();
    let (outcome, ending) = tokio::task::spawn_blocking(move || {
        let mut ending = ending;
        let outcome = outcome(&held, &identity, &mut ending);
        (outcome, ending)
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the completion of a writer ended: {e}")))?;
    super::control::recorded_end(project, move |workspace| {
        let mut ending = ending.clone();
        ending.observed.workspace = workspace;
        store.complete_writer_attempt(WriterCompletion {
            attempt_id: attempt_id.clone(),
            ending,
            outcome: outcome.clone(),
        })
    })
    .await
}

/// Complete writer attempt `row`, which a reconcile found with its work observed done and its process proven gone
/// (decision 4's recovery, [`hoh_core::reconcile::Disposition::Complete`]): its bridge read again from its attempt
/// directory, then the same path as the runner's own end, its end taken as its work recorded it (else `at`).
pub(super) async fn recover(
    store: &Store,
    project: &Path,
    row: &UnfinishedRow,
    cause: &str,
    at: std::time::SystemTime,
    draw: u32,
) -> Result<WriterCompleted, PersistError> {
    let identity = Identity {
        run_id: row.run_id.clone(),
        task_id: row.task_id.clone(),
        attempt_id: row.attempt_id.clone(),
        role: row.role.clone(),
        role_epoch: u64::try_from(row.role_epoch).map_err(|_| {
            PersistError::IntegrityViolation(format!(
                "attempt {}: role epoch {}",
                row.attempt_id, row.role_epoch
            ))
        })?,
    };
    let bridge = super::product::bridge_of(project, &row.attempt_id);
    let (messages, truncated) =
        tokio::task::spawn_blocking(move || super::product::read_bridge(&bridge))
            .await
            .map_err(|e| PersistError::Storage(format!("the read of a bridge ended: {e}")))?;
    let mut observed = Observed::default();
    (observed.messages, observed.messages_truncated) = (messages, truncated);
    let ending = Ending {
        success: true,
        detail: format!("its work was observed done; completed by a reconcile ({cause})"),
        class: row
            .work_class
            .unwrap_or(FailureClass::Transient { retry_after: None }),
        at: row.work_ended_at.unwrap_or(at),
        draw,
        observed,
        unknown_effect: false,
    };
    complete(store, project, identity, ending).await
}

#[cfg(test)]
mod tests;
