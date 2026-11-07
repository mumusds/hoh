//! The reconcile at a host's start (REQ-041 b1–b3; docs/adr/ADR-019.md decision 5). After the host epoch is raised
//! and before anything is served, each unfinished attempt of the project is looked at: its recorded process is found
//! only if pid, boot and start time all match (else no signal goes anywhere), an old host's live process is stopped
//! (SIGTERM, then SIGKILL to its group) and its group must be seen empty, each STARTED effect is looked for where it
//! would show, and
//! the attempt's disposition is written with what was found, between `reconcile.started` and `reconcile.completed`.

use hoh_agents::identity::{self, Gone, Presence, StopStep};
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
    /// After SIGKILL, how long for its group to empty.
    pub kill_grace: Duration,
    /// Where STARTED effects are looked for; the product's [`reconcile::NoObserver`] confirms nothing.
    pub observer: Arc<dyn Observer>,
    /// How long the runner leaves a holder it could not confirm gone before it looks again (60 s).
    pub retry: Duration,
}

impl Default for ReconcileSettings {
    fn default() -> Self {
        ReconcileSettings {
            term_grace: Duration::from_secs(5),
            kill_grace: Duration::from_secs(5),
            observer: Arc::new(reconcile::NoObserver),
            retry: Duration::from_secs(60),
        }
    }
}

/// What is known of an unfinished attempt's process, before anything is sent to it (ADR-019 decision 8).
pub(super) enum Seen {
    /// PREPARED: no process was started.
    NotStarted,
    /// Found by its whole recorded identity.
    Ours(identity::Found),
    /// Whether anything of it runs cannot be told: no signal, its slot kept.
    Uncertain(String),
    /// Confirmed gone (or never started): why.
    Gone(String),
}

impl Seen {
    /// The inspection as an event records it.
    pub(super) fn json(&self) -> serde_json::Value {
        match self {
            Seen::NotStarted => json!("not_started"),
            Seen::Ours(_) => json!("ours"),
            Seen::Uncertain(basis) => json!({"uncertain": basis}),
            Seen::Gone(basis) => json!({"gone": basis}),
        }
    }

    /// Give a process of the attempt up to `grace` to end by itself (its native cancel), watching its group so that
    /// it stays known (ADR-019 decision 8, design correction 4): over once its group is empty or lost track of, not
    /// only once its leader exits (M2T05R-354).
    pub(super) fn grace(&self, grace: Duration) {
        if let Seen::Ours(found) = self {
            found.wait(grace);
        }
    }
}

/// Inspect `row`'s process (blocking; no signal) by its record: only a whole identity that matches is ours; a record
/// with a pid but no boot or start time, a leader gone with members left in its group (which may be another
/// process's that reused its number), a RUNNING attempt with no record, and an inspection that fails are uncertain
/// (M2T05R-01, -02, -100, -200). One mapping for the reconcile at a start and the runner's settle (M2T05R-151).
pub(super) fn inspect(row: &UnfinishedRow) -> Seen {
    if row.state == AttemptState::Prepared {
        return Seen::NotStarted;
    }
    seen(
        row.state,
        identity::presence(
            row.pid,
            row.boot_id.as_deref(),
            row.process_start_id.as_deref(),
        ),
    )
}

/// What a presence (or the failure to tell it) of a STARTING or RUNNING attempt's record means. A finding is the
/// same text whenever the same is found, so a retry tells a repeated finding (M2T05R-157, -350).
fn seen(state: AttemptState, found: std::io::Result<Presence>) -> Seen {
    match found {
        Err(e) => Seen::Uncertain(format!("cannot_inspect: {e}")),
        Ok(Presence::Ours(found)) => Seen::Ours(found),
        Ok(Presence::Unverifiable) => Seen::Uncertain("incomplete_process_record".to_owned()),
        Ok(Presence::Leaderless(_)) => Seen::Uncertain("leader_gone_group_not_empty".to_owned()),
        Ok(Presence::Gone(Gone::NoPid)) if state == AttemptState::Running => {
            Seen::Uncertain("running_without_a_process_record".to_owned())
        }
        Ok(Presence::Gone(gone)) => Seen::Gone(gone.basis().to_owned()),
    }
}

/// End what `seen` found (blocking): a process of the attempt is stopped with its group and its group must be seen
/// empty (a group already empty gets no signal, M2T05R-259).
pub(super) fn end(seen: Seen, settings: &ReconcileSettings) -> Process {
    match seen {
        Seen::NotStarted => Process::NotStarted,
        Seen::Ours(found) => stopped(&found.stop(settings.term_grace, settings.kill_grace)),
        Seen::Uncertain(basis) => Process::Uncertain { basis },
        Seen::Gone(basis) => Process::Gone { basis },
    }
}

/// What was found of `row`'s process at a host's start, and done to it (blocking).
fn process_of(row: &UnfinishedRow, settings: &ReconcileSettings) -> Result<Process, PersistError> {
    Ok(end(inspect(row), settings))
}

/// A stop as the reconcile records it.
pub(super) fn stopped(stopped: &identity::Stopped) -> Process {
    Process::Stopped {
        steps: stopped
            .steps
            .iter()
            .map(|s| {
                match s {
                    StopStep::Term => "sigterm",
                    StopStep::Kill => "sigkill",
                    StopStep::Reaped => "reaped",
                    StopStep::Lost => "lost_track",
                }
                .to_owned()
            })
            .collect(),
        reaped: stopped.reaped,
    }
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
            work_outcome: row.work_outcome,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inspection_that_fails_or_finds_members_without_their_leader_is_uncertain() {
        // M2T05R-356: the mapping the callers rely on; a leaderless group's finding carries no count (M2T05R-350).
        let failed = seen(
            AttemptState::Running,
            Err(std::io::Error::other("no /proc")),
        );
        assert!(matches!(&failed, Seen::Uncertain(b) if b.starts_with("cannot_inspect")));
        for state in [AttemptState::Starting, AttemptState::Running] {
            let left = seen(
                state,
                Ok(Presence::Leaderless(identity::Leaderless { pgid: 7 })),
            );
            assert_eq!(
                left.json(),
                json!({"uncertain": "leader_gone_group_not_empty"})
            );
        }
        assert_eq!(
            seen(AttemptState::Running, Ok(Presence::Gone(Gone::NoPid))).json(),
            json!({"uncertain": "running_without_a_process_record"})
        );
        assert_eq!(
            seen(AttemptState::Starting, Ok(Presence::Gone(Gone::NoPid))).json(),
            json!({"gone": "no_pid"})
        );
        assert_eq!(
            seen(AttemptState::Running, Ok(Presence::Unverifiable)).json(),
            json!({"uncertain": "incomplete_process_record"})
        );
    }
}
