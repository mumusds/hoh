//! The reconcile at a host's start (REQ-041 b1–b3; docs/adr/ADR-019.md decision 5). After the host epoch is raised
//! and before anything is served, each unfinished attempt of the project is looked at: its recorded process is found
//! only if pid, boot and start time all match (else no signal goes anywhere), an old host's live process is stopped
//! (SIGTERM, then SIGKILL to its group) and its group must be seen empty, each STARTED effect is looked for where it
//! would show, and
//! the attempt's disposition is written with what was found, between `reconcile.started` and `reconcile.completed`.

use super::cgroup::Tracking;
use super::processes::{Outside, Proof, both};
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
    /// After the native cancel of an attempt this host runs, how long before SIGTERM (REQ-043 decision 1: 10 s;
    /// `execution.cancel_native_grace_seconds`).
    pub native_grace: Duration,
    /// After SIGTERM to the group, how long before SIGKILL (docs/13: 5 s; `execution.cancel_term_grace_seconds`).
    pub term_grace: Duration,
    /// After SIGKILL, how long for its group to empty.
    pub kill_grace: Duration,
    /// Where STARTED effects are looked for; the product's [`reconcile::NoObserver`] confirms nothing.
    pub observer: Arc<dyn Observer>,
    /// How long the runner leaves a holder it could not confirm gone before it looks again (60 s).
    pub retry: Duration,
}

impl ReconcileSettings {
    /// How long a settle waits before its SIGTERM: the native cancel's grace when it sent one, else the SIGTERM grace
    /// as before there was a native cancel (docs/adr/ADR-022.md decision 17, M2T07R-353).
    pub(super) fn first_wait(&self, native_cancel_sent: bool) -> Duration {
        if native_cancel_sent {
            self.native_grace
        } else {
            self.term_grace
        }
    }
}

impl Default for ReconcileSettings {
    fn default() -> Self {
        ReconcileSettings {
            native_grace: Duration::from_secs(10),
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
/// (M2T05R-01, -02, -100, -200), except that an unconfirmed record whose number no process uses, as pid or as group,
/// is gone (decision 8d v7, M2T06R-600, -700). One mapping for the reconcile at a start and the runner's settle
/// (M2T05R-151).
pub(super) fn inspect(row: &UnfinishedRow) -> Seen {
    if row.state == AttemptState::Prepared {
        return Seen::NotStarted;
    }
    seen(
        row.state,
        row.pid.and_then(|p| u32::try_from(p).ok()),
        identity::presence(
            row.pid,
            row.boot_id.as_deref(),
            row.process_start_id.as_deref(),
        ),
    )
}

/// What a presence (or the failure to tell it) of a STARTING or RUNNING attempt's record, its group number `number`
/// (its recorded pid), means. A finding is the same text whenever the same is found, so a retry tells a repeated
/// finding (M2T05R-157, -350). A record that cannot confirm its process gets no signal (ADR-019 decision 8), but once
/// the kernel says no process uses its number, neither as its pid nor as its group, nothing of that group can come
/// back: it is gone, and the proof goes on with the rest (decision 8d v7, M2T06R-600; a changed finding, so a settle
/// looks again). An empty group alone is not enough: process `n`, alive in another group, can make it again (-700).
fn seen(state: AttemptState, number: Option<u32>, found: std::io::Result<Presence>) -> Seen {
    let unconfirmed = |basis: &str, finding: String| match number {
        Some(pgid) if identity::number_free(pgid) => Seen::Gone(format!("{basis}_group_empty")),
        _ => Seen::Uncertain(finding),
    };
    match found {
        Err(e) => unconfirmed("cannot_inspect", format!("cannot_inspect: {e}")),
        Ok(Presence::Ours(found)) => Seen::Ours(found),
        Ok(Presence::Unverifiable) => unconfirmed(
            "incomplete_process_record",
            "incomplete_process_record".to_owned(),
        ),
        Ok(Presence::Leaderless(_)) => Seen::Uncertain("leader_gone_group_not_empty".to_owned()),
        Ok(Presence::Gone(Gone::NoPid)) if state == AttemptState::Running => {
            Seen::Uncertain("running_without_a_process_record".to_owned())
        }
        Ok(Presence::Gone(gone)) => Seen::Gone(gone.basis().to_owned()),
    }
}

/// End what `seen` found of `row`'s processes (blocking): a process of the attempt is stopped with its group and its
/// group must be seen empty (a group already empty gets no signal, M2T05R-259); then its processes outside the group
/// are stopped the same way, the group stopped, gone or never started, and the attempt can end only with the
/// termination proof (docs/adr/ADR-021.md decision 8d, M2T06R-100): else it is `Stopped { reaped: false }`, which
/// leaves it and its slot.
pub(super) fn end(
    seen: Seen,
    row: &UnfinishedRow,
    tracking: &Tracking,
    settings: &ReconcileSettings,
) -> Process {
    // Recorded by a host that saw another world: nothing seen here is the attempt's (M2T06R-900).
    if let Tracking::Foreign(why) = tracking {
        return Process::Uncertain {
            basis: format!("recorded_in_another_world: {why}"),
        };
    }
    let cgroup = tracking.cgroup();
    let mut outside = Outside::new(&super::runner::marker(&row.project_id, &row.attempt_id));
    let (term, kill) = (settings.term_grace, settings.kill_grace);
    let group = match seen {
        // Its group cannot be told apart: no signal may go to it (ADR-019 decision 8). Its cgroup, by membership, is
        // stopped all the same, and its group looked at again (decision 8d v6, M2T06R-401).
        Seen::Uncertain(basis) => {
            let Some(cgroup) = cgroup else {
                return Process::Uncertain { basis };
            };
            let (mut steps, in_cgroup) = cgroup.stop(term, kill);
            let (left, outside) = outside.stop(term, kill);
            steps.extend(left);
            let emptied = matches!(inspect(row), Seen::Gone(_) | Seen::NotStarted);
            return Process::Stopped {
                steps,
                reaped: emptied && both(in_cgroup, outside) == Proof::Done,
            };
        }
        Seen::NotStarted => Process::NotStarted,
        Seen::Ours(found) => {
            // Its descendants are pinned while its leader still ties them to it; then the group is stopped.
            outside.set_leader(row.pid, row.process_start_id.as_deref());
            let _ = outside.scan();
            stopped(&found.stop(term, kill))
        }
        Seen::Gone(basis) => Process::Gone { basis },
    };
    let (steps, proof) = match cgroup {
        // The cgroup, then what may have left it with its marker (decision 8d v6, M2T06R-400).
        Some(cgroup) => {
            let (mut steps, in_cgroup) = cgroup.stop(term, kill);
            let (left, outside) = outside.stop(term, kill);
            steps.extend(left);
            (steps, both(in_cgroup, outside))
        }
        None => outside.stop(term, kill),
    };
    with_outside(group, steps, proof)
}

/// The process as recorded once its processes outside the group were stopped: unchanged when there were none.
fn with_outside(group: Process, outside: Vec<String>, proof: Proof) -> Process {
    match group {
        group if proof == Proof::Done && outside.is_empty() => group,
        Process::Stopped { mut steps, reaped } => {
            steps.extend(outside);
            Process::Stopped {
                steps,
                reaped: reaped && proof == Proof::Done,
            }
        }
        _ => Process::Stopped {
            steps: outside,
            reaped: proof == Proof::Done,
        },
    }
}

/// What was found of `row`'s process at a host's start, and done to it (blocking).
fn process_of(
    row: &UnfinishedRow,
    tracking: &Tracking,
    settings: &ReconcileSettings,
) -> Result<Process, PersistError> {
    Ok(end(inspect(row), row, tracking, settings))
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

/// `row`'s disposition and the process found applied in one transaction (`Store::reconcile_apply`), after its stop:
/// an interrupted end of the writer lease's holder with the workspace's record of `project`, taken when the store asks
/// for it (REQ-019 behaviour 6, docs/adr/ADR-024.md decision 9). A writer whose work was observed done is then
/// completed from its bridge, its attempt RUNNING with its lease until that commit (docs/adr/ADR-028.md decision 4); a
/// crash in between leaves it to the next reconcile, which finds it the same.
pub(super) async fn apply(
    store: &Store,
    project: &std::path::Path,
    row: UnfinishedRow,
    (disposition, process): (Disposition, reconcile::Process),
    cause: &'static str,
    at: std::time::SystemTime,
    draw: u32,
) -> Result<i64, PersistError> {
    let complete = matches!(disposition, Disposition::Complete { .. });
    let found = row.clone();
    let seq = super::control::recorded_end(project, move |workspace| {
        let row = UnfinishedRow {
            workspace,
            ..row.clone()
        };
        store.reconcile_apply(row, disposition.clone(), process.clone(), cause, at, draw)
    })
    .await?;
    if complete {
        super::completion::recover(store, project, &found, cause, at, draw).await?;
    }
    Ok(seq)
}

/// Reconcile the project's unfinished attempts (in `project`); done before the host serves anything.
pub(super) async fn run(
    store: &Store,
    project_id: &str,
    project: &std::path::Path,
    host_epoch: i64,
    settings: &ReconcileSettings,
    clock: &dyn hoh_core::delivery::Clock,
    jitter: &super::runner::Jitter,
) -> Result<(), PersistError> {
    let rows = store.reconcile_scan(project_id, clock.now()).await?;
    store
        .reconcile_started(project_id, host_epoch, rows.len())
        .await?;
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    // How each was tracked: a cgroup an earlier host made is found again by the path its start recorded
    // (docs/adr/ADR-021.md decision 8d v5 to v7).
    let reader = store.reader()?;
    let ids: Vec<String> = rows.iter().map(|r| r.attempt_id.clone()).collect();
    let trackings: BTreeMap<String, Tracking> = tokio::task::spawn_blocking(move || {
        ids.into_iter()
            .map(|id| {
                let event = reader.attempt_event("attempt.tracking", &id)?;
                Ok((id, Tracking::of(event.as_ref())))
            })
            .collect::<Result<_, PersistError>>()
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the reconcile's look-up ended: {e}")))??;
    for row in rows {
        let (process, effects) = {
            let row = row.clone();
            let settings = settings.clone();
            let tracking = trackings
                .get(&row.attempt_id)
                .cloned()
                .unwrap_or_else(|| Tracking::of(None));
            tokio::task::spawn_blocking(move || -> Result<_, PersistError> {
                let process = process_of(&row, &tracking, &settings)?;
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
            writer: row.writer,
        });
        *counts
            .entry(match disposition {
                Disposition::Redeliver => "redeliver",
                Disposition::Unreaped => "unreaped",
                Disposition::Ended { .. } => "ended",
                Disposition::Complete { .. } => "completion",
                Disposition::Completed { .. } => "completed",
                Disposition::UnknownEffect { .. } => "unknown_effect",
            })
            .or_default() += 1;
        apply(
            store,
            project,
            row,
            (disposition, process),
            "reconcile",
            clock.now(),
            jitter.draw(),
        )
        .await?;
    }
    store
        .reconcile_completed(project_id, host_epoch, json!(counts))
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_settle_waits_the_native_grace_only_after_a_native_cancel() {
        // M2T07R-353 (docs/adr/ADR-022.md decision 17): with no native cancel sent (no turn of the attempt held), a
        // settle waits the SIGTERM grace, not the longer native one.
        let settings = super::ReconcileSettings {
            native_grace: super::Duration::from_secs(10),
            term_grace: super::Duration::from_secs(5),
            ..super::ReconcileSettings::default()
        };
        assert_eq!(settings.first_wait(true), super::Duration::from_secs(10));
        assert_eq!(settings.first_wait(false), super::Duration::from_secs(5));
    }

    use super::*;

    #[test]
    fn without_the_proof_the_process_is_recorded_unreaped() {
        // M2T06R-251 (decision 8d): the group's record, then what was done outside it; no proof, no end.
        let stopped = |reaped| Process::Stopped {
            steps: vec!["sigterm".to_owned(), "reaped".to_owned()],
            reaped,
        };
        let outside = vec!["outside_sigterm".to_owned()];
        assert_eq!(
            with_outside(stopped(true), Vec::new(), Proof::Done),
            stopped(true)
        );
        for proof in [Proof::Pending, Proof::Incomplete] {
            assert_eq!(
                with_outside(stopped(true), outside.clone(), proof),
                Process::Stopped {
                    steps: vec![
                        "sigterm".to_owned(),
                        "reaped".to_owned(),
                        "outside_sigterm".to_owned()
                    ],
                    reaped: false
                }
            );
            let gone = Process::Gone {
                basis: "no_process".to_owned(),
            };
            assert_eq!(
                with_outside(gone, outside.clone(), proof),
                Process::Stopped {
                    steps: outside.clone(),
                    reaped: false
                }
            );
        }
        let gone = Process::Gone {
            basis: "no_process".to_owned(),
        };
        assert_eq!(with_outside(gone.clone(), Vec::new(), Proof::Done), gone);
        assert_eq!(
            with_outside(Process::NotStarted, outside.clone(), Proof::Done),
            Process::Stopped {
                steps: outside,
                reaped: true
            }
        );
    }

    /// A group number that is in use: this test process's own group.
    fn living() -> Option<u32> {
        Some(rustix::process::getpgrp().as_raw_pid().unsigned_abs())
    }

    #[test]
    fn an_inspection_that_fails_or_finds_members_without_their_leader_is_uncertain() {
        // M2T05R-356: the mapping the callers rely on; a leaderless group's finding carries no count (M2T05R-350).
        // Each record's group number is in use (M2T06R-600: the next test).
        let failed = seen(
            AttemptState::Running,
            living(),
            Err(std::io::Error::other("no /proc")),
        );
        assert!(matches!(&failed, Seen::Uncertain(b) if b.starts_with("cannot_inspect")));
        for state in [AttemptState::Starting, AttemptState::Running] {
            let left = seen(
                state,
                living(),
                Ok(Presence::Leaderless(identity::Leaderless { pgid: 7 })),
            );
            assert_eq!(
                left.json(),
                json!({"uncertain": "leader_gone_group_not_empty"})
            );
        }
        assert_eq!(
            seen(AttemptState::Running, None, Ok(Presence::Gone(Gone::NoPid))).json(),
            json!({"uncertain": "running_without_a_process_record"})
        );
        assert_eq!(
            seen(
                AttemptState::Starting,
                None,
                Ok(Presence::Gone(Gone::NoPid))
            )
            .json(),
            json!({"gone": "no_pid"})
        );
        assert_eq!(
            seen(AttemptState::Running, living(), Ok(Presence::Unverifiable)).json(),
            json!({"uncertain": "incomplete_process_record"})
        );
    }

    #[test]
    fn an_unconfirmed_record_whose_group_number_is_free_is_gone()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-600 (decision 8d v7): a record that cannot confirm its process (no start time, or an inspection that
        // failed) is uncertain while its number is in use, and gone once the kernel says no process uses it as its
        // pid or as its group (identity::number_free, -700, -800): nothing of that group can come back, so the proof
        // goes on with the rest. With
        // no number recorded nothing changes.
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("true")
            .process_group(0)
            .spawn()?;
        let freed = Some(child.id());
        child.wait()?;
        assert_eq!(
            seen(AttemptState::Running, freed, Ok(Presence::Unverifiable)).json(),
            json!({"gone": "incomplete_process_record_group_empty"})
        );
        assert_eq!(
            seen(
                AttemptState::Running,
                freed,
                Err(std::io::Error::other("no /proc"))
            )
            .json(),
            json!({"gone": "cannot_inspect_group_empty"})
        );
        assert!(matches!(
            seen(
                AttemptState::Running,
                None,
                Err(std::io::Error::other("no /proc"))
            ),
            Seen::Uncertain(_)
        ));
        Ok(())
    }

    #[test]
    fn an_attempt_recorded_in_another_world_is_never_ended() {
        // M2T06R-900: whatever this host finds of it (here nothing: a record whose pid names no process), an attempt
        // recorded by a host that saw other namespaces stays uncertain.
        let row = UnfinishedRow {
            project_id: "p".to_owned(),
            run_id: "R".to_owned(),
            attempt_id: "A1".to_owned(),
            task_id: "T1".to_owned(),
            state: AttemptState::Running,
            task_state: hoh_core::protocol::TaskState::Running,
            task_revision: 1,
            role: "developer".to_owned(),
            writer: false,
            role_epoch: 1,
            pid: None,
            boot_id: None,
            process_start_id: None,
            effects: Vec::new(),
            lease_expired: true,
            lease_epoch: Some(1),
            work_outcome: None,
            work_class: None,
            work_ended_at: None,
            cancel_requested: false,
            workspace: None,
        };
        let process = end(
            Seen::Gone("no_process".to_owned()),
            &row,
            &Tracking::Foreign("elsewhere".to_owned()),
            &ReconcileSettings::default(),
        );
        assert!(
            matches!(&process, Process::Uncertain { basis } if basis.starts_with("recorded_in_another_world")),
            "{process:?}"
        );
    }

    #[test]
    fn an_unconfirmed_record_whose_process_lives_in_another_group_stays_uncertain()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T06R-700: the recorded process lives on in another group, so its own group number is empty, but it can
        // make that group again at any time: uncertain, never gone, until no process has the number as pid or group.
        use std::os::unix::process::CommandExt as _;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(rustix::process::getpgrp().as_raw_pid())
            .spawn()?;
        let n = Some(child.id());
        let found = (
            seen(AttemptState::Running, n, Ok(Presence::Unverifiable)),
            seen(
                AttemptState::Running,
                n,
                Err(std::io::Error::other("no /proc")),
            ),
        );
        drop(child.stdin.take());
        child.wait()?;
        assert!(
            matches!(found.0, Seen::Uncertain(_)),
            "{:?}",
            found.0.json()
        );
        assert!(
            matches!(found.1, Seen::Uncertain(_)),
            "{:?}",
            found.1.json()
        );
        Ok(())
    }
}
