//! The reconcile at a host's start (REQ-041 b1–b3; docs/adr/ADR-019.md decision 5). After the host epoch is raised
//! and before anything is served, each unfinished attempt of the project is looked at: its recorded process is found
//! only if pid, boot and start time all match (else no signal goes anywhere), an old host's live process is stopped
//! (SIGTERM, then SIGKILL to its group) and must be gone, each STARTED effect is looked for where it would show, and
//! the attempt's disposition is written with what was found, between `reconcile.started` and `reconcile.completed`.

use hoh_agents::identity::{self, StopStep};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{PersistError, Store, UnfinishedRow};
use hoh_core::reconcile::{self, Disposition, Effect, EffectState, Observer, Process, Unfinished};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// How the reconcile stops an old host's process and where it looks for effects.
#[derive(Clone, Debug)]
pub struct ReconcileSettings {
    /// After SIGTERM to the group, how long before SIGKILL (docs/13: 5 s).
    pub term_grace: Duration,
    /// After SIGKILL, how long for the leader to exit and its group to empty.
    pub kill_grace: Duration,
    /// Where STARTED effects are looked for; the product's [`reconcile::NoObserver`] confirms nothing.
    pub observer: Arc<dyn Observer>,
}

impl Default for ReconcileSettings {
    fn default() -> Self {
        ReconcileSettings {
            term_grace: Duration::from_secs(5),
            kill_grace: Duration::from_secs(5),
            observer: Arc::new(reconcile::NoObserver),
        }
    }
}

/// What was found of `row`'s process, and done to it (blocking: run off the async threads).
fn process_of(row: &UnfinishedRow, settings: &ReconcileSettings) -> Result<Process, PersistError> {
    if row.state == AttemptState::Prepared {
        return Ok(Process::NotStarted);
    }
    let found = identity::find(
        row.pid,
        row.boot_id.as_deref(),
        row.process_start_id.as_deref(),
    )
    .map_err(|e| {
        PersistError::Storage(format!(
            "cannot look for the process of {}: {e}",
            row.attempt_id
        ))
    })?;
    Ok(match found {
        Err(gone) => Process::Gone {
            basis: gone.basis().to_owned(),
        },
        Ok(ours) => {
            let stopped = ours.stop(settings.term_grace, settings.kill_grace);
            Process::Stopped {
                steps: stopped
                    .steps
                    .iter()
                    .map(|s| {
                        match s {
                            StopStep::Term => "sigterm",
                            StopStep::Kill => "sigkill",
                            StopStep::Reaped => "reaped",
                        }
                        .to_owned()
                    })
                    .collect(),
                reaped: stopped.reaped,
            }
        }
    })
}

/// Reconcile the project's unfinished attempts; done before the host serves anything.
pub(super) async fn run(
    store: &Store,
    project_id: &str,
    host_epoch: i64,
    settings: &ReconcileSettings,
) -> Result<(), PersistError> {
    let rows = store
        .reconcile_scan(project_id, std::time::SystemTime::now())
        .await?;
    store
        .reconcile_started(project_id, host_epoch, rows.len())
        .await?;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    for row in rows {
        let (process, effects) = {
            let row = row.clone();
            let settings = settings.clone();
            tokio::task::spawn_blocking(move || -> Result<_, PersistError> {
                let process = process_of(&row, &settings)?;
                let effects: Vec<Effect> = row
                    .effects
                    .iter()
                    .map(|e| Effect {
                        key: e.key.clone(),
                        state: e.state,
                        observation: (e.state == EffectState::Started)
                            .then(|| settings.observer.observe(&row.attempt_id, &e.key)),
                    })
                    .collect();
                Ok((process, effects))
            })
            .await
            .map_err(|e| PersistError::Storage(format!("the reconcile's look-up ended: {e}")))??
        };
        let disposition = reconcile::dispose(&Unfinished {
            attempt_id: row.attempt_id.clone(),
            state: row.state,
            task_state: row.task_state,
            process: process.clone(),
            effects,
            lease_expired: row.lease_expired,
        });
        *counts
            .entry(match disposition {
                Disposition::Redeliver => "redeliver",
                Disposition::Unreaped => "unreaped",
                Disposition::Ended { .. } => "ended",
                Disposition::Completed { .. } => "completed",
                Disposition::UnknownEffect { .. } => "unknown_effect",
            })
            .or_default() += 1;
        store
            .reconcile_apply(row, disposition, process, "reconcile")
            .await?;
    }
    store
        .reconcile_completed(project_id, host_epoch, json!(counts))
        .await?;
    Ok(())
}
