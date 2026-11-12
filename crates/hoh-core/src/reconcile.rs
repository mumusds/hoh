//! The dispositions of the reconcile at a host's start (REQ-041 b1–b2, b6; docs/adr/ADR-019.md decision 5). Pure:
//! from what the host found about one unfinished attempt (its process, its effects and what was observed of them) to
//! what becomes of it. Never a blind replay: an attempt is delivered again or ended for another only when it is
//! confirmed that it began no effect; an effect that cannot be confirmed makes it UNKNOWN_EFFECT.

use crate::lifecycle::AttemptState;
use crate::protocol::TaskState;

/// The states of an effect (`effects.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectState {
    Prepared,
    Started,
    Observed,
    Committed,
    Unknown,
}

impl EffectState {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectState::Prepared => "PREPARED",
            EffectState::Started => "STARTED",
            EffectState::Observed => "OBSERVED",
            EffectState::Committed => "COMMITTED",
            EffectState::Unknown => "UNKNOWN",
        }
    }

    pub fn parse(text: &str) -> Option<EffectState> {
        Some(match text {
            "PREPARED" => EffectState::Prepared,
            "STARTED" => EffectState::Started,
            "OBSERVED" => EffectState::Observed,
            "COMMITTED" => EffectState::Committed,
            "UNKNOWN" => EffectState::Unknown,
            _ => return None,
        })
    }
}

/// Where the reconcile looks for a STARTED effect (REQ-041 b6: by its run marker or external ref, never by doing it
/// again): a port. The product's [`NoObserver`] can ask nothing, so every STARTED effect stays unconfirmed (UNKNOWN);
/// a forge or the runner's markers answer through other implementations.
pub trait Observer: Send + Sync + std::fmt::Debug {
    fn observe(&self, attempt_id: &str, effect_key: &str) -> Observation;
}

/// The product's observer until one exists for the effect's kind: it cannot ask, so nothing is confirmed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoObserver;

impl Observer for NoObserver {
    fn observe(&self, _attempt_id: &str, _effect_key: &str) -> Observation {
        Observation::Unavailable {
            detail: "no observer can look for this effect".to_owned(),
        }
    }
}

/// What was found of a STARTED effect where it would show (a run marker, an external ref; REQ-041 b6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// It happened: what it made.
    Found { external_ref: String },
    /// The place it would show answered that it is not there.
    Absent,
    /// The place it would show could not be asked.
    Unavailable { detail: String },
}

/// One effect of the attempt and, for a STARTED one, what was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effect {
    pub key: String,
    pub state: EffectState,
    pub observation: Option<Observation>,
}

/// What the host found about the attempt's process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Process {
    /// PREPARED: no process was started, none is looked for.
    NotStarted,
    /// Not there, or the pid names another process (`basis`, e.g. "start_time_differs"): no signal was sent.
    Gone { basis: String },
    /// Ours and alive: stopped by these steps; `reaped` whether it and its group are gone.
    Stopped { steps: Vec<String>, reaped: bool },
    /// Whether anything of it still runs cannot be told (its leader gone with members left in its group, or a
    /// RUNNING attempt with no process recorded): no signal was sent, and its slot stays held (M2T05R-01, -02).
    Uncertain { basis: String },
}

/// One unfinished attempt (PREPARED, STARTING or RUNNING) with its task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unfinished {
    pub attempt_id: String,
    pub state: AttemptState,
    pub task_state: TaskState,
    pub process: Process,
    pub effects: Vec<Effect>,
    /// A lease it holds has expired: a PREPARED attempt can no longer start (a writer start needs an unexpired lease).
    pub lease_expired: bool,
    /// The outcome recorded when its work was observed (`effect.observed`: success or failure), if it was.
    pub work_outcome: Option<bool>,
    /// Its task holds the write lease's role (`tasks.writer`): its success is its completion (docs/adr/ADR-028.md
    /// decision 4), never the generic settle.
    pub writer: bool,
}

/// What an effect becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectMove {
    pub key: String,
    pub from: EffectState,
    pub to: EffectState,
    pub external_ref: Option<String>,
}

/// What becomes of the attempt and its task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// PREPARED: its dispatch was never received, so it stays as it is and its dispatch is delivered again.
    Redeliver,
    /// Its process could not be reaped: nothing moves, its lease stays held (no new writer grant) and a diagnostic
    /// says so (REQ-041 failure mode 5).
    Unreaped,
    /// Its work is not confirmed done and every effect it began was found done (or it began none): it ends (`to`) and
    /// its task is READY for another attempt, whose effects find by their markers what this one made (a side effect's
    /// key is stable per logical effect; the work's key is per attempt).
    Ended {
        to: AttemptState,
        task_to: TaskState,
        effects: Vec<EffectMove>,
    },
    /// A writer's work was observed done and its task is RUNNING: its effects committed and nothing else; the host then
    /// completes it as the runner would have (its bridge read, its freeze prepared, one commit with its settle;
    /// docs/adr/ADR-028.md decision 4), so its attempt stays RUNNING with its write lease until that commit.
    Complete { effects: Vec<EffectMove> },
    /// Its work was observed and is committed: SETTLED, its task IMPLEMENTED when it was RUNNING.
    Completed {
        effects: Vec<EffectMove>,
        task_to: Option<TaskState>,
    },
    /// An effect that cannot be confirmed: UNKNOWN_EFFECT, its task left where it is (no new attempt), a diagnostic
    /// with the effect keys (REQ-041 failure mode 1).
    UnknownEffect {
        effects: Vec<EffectMove>,
        unknown: Vec<String>,
    },
}

/// The key of an attempt's own work: the effect the runner records around the agent's execution (ADR-019 decision 6).
pub fn work_key(attempt_id: &str) -> String {
    crate::ids::child_id(attempt_id, "work")
}

/// The disposition of `u`.
pub fn dispose(u: &Unfinished) -> Disposition {
    if matches!(
        u.process,
        Process::Stopped { reaped: false, .. } | Process::Uncertain { .. }
    ) {
        return Disposition::Unreaped;
    }
    let mut moves = Vec::new();
    let mut unknown = Vec::new();
    for effect in &u.effects {
        let to = match (effect.state, &effect.observation) {
            (EffectState::Started, Some(Observation::Found { external_ref })) => {
                Some((EffectState::Committed, Some(external_ref.clone())))
            }
            (EffectState::Started, _) => Some((EffectState::Unknown, None)),
            (EffectState::Observed, _) => Some((EffectState::Committed, None)),
            _ => None,
        };
        let after = to.as_ref().map_or(effect.state, |(state, _)| *state);
        if let Some((to, external_ref)) = to {
            moves.push(EffectMove {
                key: effect.key.clone(),
                from: effect.state,
                to,
                external_ref,
            });
        }
        if after == EffectState::Unknown {
            unknown.push(effect.key.clone());
        }
    }
    if !unknown.is_empty() {
        return Disposition::UnknownEffect {
            effects: moves,
            unknown,
        };
    }
    // The attempt's own work, once committed, is disposed of by its recorded outcome (M2T05R-05, -53): done only when
    // it succeeded and the attempt was RUNNING; a failure ends it for another attempt; anything else is not known.
    let work = work_key(&u.attempt_id);
    let committed = u.effects.iter().find(|e| e.key == work).and_then(|e| {
        let found = moves.iter().find(|m| m.key == e.key);
        let after = found.map_or(e.state, |m| m.to);
        (after == EffectState::Committed).then_some(
            // Found where it would show: it was done.
            if found.is_some_and(|m| m.external_ref.is_some()) {
                Some(true)
            } else {
                u.work_outcome
            },
        )
    });
    match committed {
        // A writer's success is its completion, not the generic settle (docs/adr/ADR-028.md decision 4).
        Some(Some(true))
            if u.state == AttemptState::Running
                && u.writer
                && u.task_state == TaskState::Running =>
        {
            return Disposition::Complete { effects: moves };
        }
        Some(Some(true)) if u.state == AttemptState::Running => {
            return Disposition::Completed {
                effects: moves,
                task_to: (u.task_state == TaskState::Running).then_some(TaskState::Implemented),
            };
        }
        Some(Some(false)) => {
            return Disposition::Ended {
                to: AttemptState::Failed,
                task_to: TaskState::Ready,
                effects: moves,
            };
        }
        Some(_) => {
            return Disposition::UnknownEffect {
                effects: moves,
                unknown: vec![work],
            };
        }
        None => {}
    }
    if u.state == AttemptState::Prepared && !u.lease_expired {
        return Disposition::Redeliver;
    }
    Disposition::Ended {
        to: if u.state == AttemptState::Running {
            AttemptState::Failed
        } else {
            AttemptState::Cancelled
        },
        task_to: TaskState::Ready,
        effects: moves,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unfinished(state: AttemptState, process: Process, effects: Vec<Effect>) -> Unfinished {
        Unfinished {
            attempt_id: "A".to_owned(),
            state,
            task_state: if state == AttemptState::Running {
                TaskState::Running
            } else {
                TaskState::Leased
            },
            process,
            effects,
            lease_expired: false,
            work_outcome: Some(true),
            writer: false,
        }
    }

    fn gone() -> Process {
        Process::Gone {
            basis: "no_process".to_owned(),
        }
    }

    fn effect(key: &str, state: EffectState, observation: Option<Observation>) -> Effect {
        Effect {
            key: key.to_owned(),
            state,
            observation,
        }
    }

    #[test]
    fn an_attempt_that_began_no_effect_is_delivered_again_or_ended_for_another() {
        assert_eq!(
            dispose(&unfinished(
                AttemptState::Prepared,
                Process::NotStarted,
                vec![]
            )),
            Disposition::Redeliver
        );
        assert_eq!(
            dispose(&unfinished(AttemptState::Starting, gone(), vec![])),
            Disposition::Ended {
                to: AttemptState::Cancelled,
                task_to: TaskState::Ready,
                effects: vec![]
            }
        );
        let prepared = effect(&work_key("A"), EffectState::Prepared, None);
        assert_eq!(
            dispose(&unfinished(AttemptState::Running, gone(), vec![prepared])),
            Disposition::Ended {
                to: AttemptState::Failed,
                task_to: TaskState::Ready,
                effects: vec![]
            }
        );
    }

    #[test]
    fn a_started_effect_is_committed_only_when_found() {
        let found = effect(
            "pr",
            EffectState::Started,
            Some(Observation::Found {
                external_ref: "PR-7".to_owned(),
            }),
        );
        let unavailable = effect(
            "pr2",
            EffectState::Started,
            Some(Observation::Unavailable {
                detail: "down".to_owned(),
            }),
        );
        let absent = effect("pr3", EffectState::Started, Some(Observation::Absent));
        let d = dispose(&unfinished(
            AttemptState::Running,
            gone(),
            vec![found, unavailable, absent],
        ));
        assert_eq!(
            d,
            Disposition::UnknownEffect {
                effects: vec![
                    EffectMove {
                        key: "pr".to_owned(),
                        from: EffectState::Started,
                        to: EffectState::Committed,
                        external_ref: Some("PR-7".to_owned())
                    },
                    EffectMove {
                        key: "pr2".to_owned(),
                        from: EffectState::Started,
                        to: EffectState::Unknown,
                        external_ref: None
                    },
                    EffectMove {
                        key: "pr3".to_owned(),
                        from: EffectState::Started,
                        to: EffectState::Unknown,
                        external_ref: None
                    }
                ],
                unknown: vec!["pr2".to_owned(), "pr3".to_owned()]
            }
        );
    }

    #[test]
    fn a_found_side_effect_is_committed_and_the_unconfirmed_work_ends_the_attempt() {
        let found = effect(
            "pr",
            EffectState::Started,
            Some(Observation::Found {
                external_ref: "PR-7".to_owned(),
            }),
        );
        assert_eq!(
            dispose(&unfinished(AttemptState::Running, gone(), vec![found])),
            Disposition::Ended {
                to: AttemptState::Failed,
                task_to: TaskState::Ready,
                effects: vec![EffectMove {
                    key: "pr".to_owned(),
                    from: EffectState::Started,
                    to: EffectState::Committed,
                    external_ref: Some("PR-7".to_owned())
                }]
            }
        );
    }

    #[test]
    fn observed_or_found_work_completes_the_attempt_and_its_task() {
        let work = effect(&work_key("A"), EffectState::Observed, None);
        assert_eq!(
            dispose(&unfinished(AttemptState::Running, gone(), vec![work])),
            Disposition::Completed {
                effects: vec![EffectMove {
                    key: work_key("A"),
                    from: EffectState::Observed,
                    to: EffectState::Committed,
                    external_ref: None
                }],
                task_to: Some(TaskState::Implemented)
            }
        );
        let committed = effect(&work_key("A"), EffectState::Committed, None);
        let mut done = unfinished(AttemptState::Running, gone(), vec![committed]);
        done.task_state = TaskState::Implemented;
        assert_eq!(
            dispose(&done),
            Disposition::Completed {
                effects: vec![],
                task_to: None
            }
        );
    }

    #[test]
    fn a_writers_work_observed_done_is_left_to_its_completion_not_the_generic_settle() {
        // docs/adr/ADR-028.md decision 4: a writer whose work succeeded and whose task is RUNNING is completed by the
        // host (its bridge read, its freeze prepared, one commit); the reconcile only commits its effects, so its
        // attempt stays RUNNING with its write lease and its task is never made IMPLEMENTED.
        for state in [EffectState::Observed, EffectState::Committed] {
            let mut writer = unfinished(
                AttemptState::Running,
                gone(),
                vec![effect(&work_key("A"), state, None)],
            );
            writer.writer = true;
            let effects = (state == EffectState::Observed)
                .then(|| EffectMove {
                    key: work_key("A"),
                    from: EffectState::Observed,
                    to: EffectState::Committed,
                    external_ref: None,
                })
                .into_iter()
                .collect();
            assert_eq!(dispose(&writer), Disposition::Complete { effects });
        }
        // Its task no longer RUNNING (a cancel moved it): the generic settle, which leaves it alone.
        let mut moved = unfinished(
            AttemptState::Running,
            gone(),
            vec![effect(&work_key("A"), EffectState::Committed, None)],
        );
        moved.writer = true;
        moved.task_state = TaskState::Cancelled;
        assert_eq!(
            dispose(&moved),
            Disposition::Completed {
                effects: vec![],
                task_to: None
            }
        );
        // A writer's failure ends as any attempt's does.
        let mut failed = unfinished(
            AttemptState::Running,
            gone(),
            vec![effect(&work_key("A"), EffectState::Committed, None)],
        );
        failed.writer = true;
        failed.work_outcome = Some(false);
        assert!(matches!(dispose(&failed), Disposition::Ended { .. }));
    }

    #[test]
    fn a_prepared_attempt_whose_lease_expired_ends_instead_of_waiting_to_start() {
        let mut stale = unfinished(AttemptState::Prepared, Process::NotStarted, vec![]);
        stale.lease_expired = true;
        assert_eq!(
            dispose(&stale),
            Disposition::Ended {
                to: AttemptState::Cancelled,
                task_to: TaskState::Ready,
                effects: vec![]
            }
        );
    }

    #[test]
    fn committed_work_is_disposed_of_by_its_recorded_outcome() {
        // M2T05R-05, -53: a failure recorded with the work ends the attempt for another; a STARTING attempt never
        // settles; committed work with no recorded outcome is not known.
        let committed = || vec![effect(&work_key("A"), EffectState::Committed, None)];
        let mut failed = unfinished(AttemptState::Running, gone(), committed());
        failed.work_outcome = Some(false);
        assert_eq!(
            dispose(&failed),
            Disposition::Ended {
                to: AttemptState::Failed,
                task_to: TaskState::Ready,
                effects: vec![]
            }
        );
        let mut starting = unfinished(AttemptState::Starting, gone(), committed());
        starting.work_outcome = Some(false);
        assert!(matches!(
            dispose(&starting),
            Disposition::Ended {
                to: AttemptState::Failed,
                ..
            }
        ));
        starting.work_outcome = Some(true);
        assert!(matches!(
            dispose(&starting),
            Disposition::UnknownEffect { .. }
        ));
        let mut unrecorded = unfinished(AttemptState::Running, gone(), committed());
        unrecorded.work_outcome = None;
        assert!(matches!(
            dispose(&unrecorded),
            Disposition::UnknownEffect { .. }
        ));
    }

    #[test]
    fn an_uncertain_process_moves_nothing() {
        let uncertain = Process::Uncertain {
            basis: "leader_gone_1_left_in_its_group".to_owned(),
        };
        assert_eq!(
            dispose(&unfinished(AttemptState::Running, uncertain, vec![])),
            Disposition::Unreaped
        );
    }

    #[test]
    fn a_process_that_could_not_be_reaped_moves_nothing() {
        let stuck = Process::Stopped {
            steps: vec!["term".to_owned(), "kill".to_owned()],
            reaped: false,
        };
        assert_eq!(
            dispose(&unfinished(AttemptState::Running, stuck, vec![])),
            Disposition::Unreaped
        );
    }
}
