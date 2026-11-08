//! The cancel of one attempt's processes (REQ-043 behaviour 3-5 and failure modes 1-2, docs/adr/ADR-021.md decision
//! 8c): the native interrupt first, SIGTERM to the group after its grace, SIGKILL after the next, SIGKILL again while
//! the group still lives. Only a look that found the group empty ends it; a timeout is not a cancel. Pure: the host
//! looks at the processes and sends the signals through a [`ProcessControl`], on its clock.

use crate::delivery::Clock;
use std::time::{Duration, SystemTime};

/// What a look at the attempt's processes found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    /// Its group is still its own and alive.
    Held,
    /// Its group is empty: nothing of it runs.
    Empty,
    /// Its group can no longer be told apart: no signal may be sent to it (ADR-019 decision 8).
    Lost,
}

/// A step of the escalation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The agent's own interrupt (its executor's cancel).
    Native,
    Term,
    Kill,
}

impl Signal {
    /// As its event names it (`cancel.<name>`).
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Native => "native_cancel",
            Signal::Term => "sigterm",
            Signal::Kill => "sigkill",
        }
    }
}

/// The graces between the steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Graces {
    /// After the native interrupt, before SIGTERM.
    pub native: Duration,
    /// After SIGTERM, before SIGKILL.
    pub term: Duration,
    /// Between two SIGKILLs while the group lives; also how long a pass waits after its SIGKILL.
    pub kill_every: Duration,
}

/// REQ-043 decision 1 (initial values, configurable once M2-T07 reads `cancel_*_grace_seconds`).
pub const GRACES: Graces = Graces {
    native: Duration::from_secs(10),
    term: Duration::from_secs(5),
    kill_every: Duration::from_secs(5),
};

/// When each step was sent, across passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    pub native_at: Option<SystemTime>,
    pub term_at: Option<SystemTime>,
    pub kill_at: Option<SystemTime>,
}

/// What to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Next {
    Send(Signal),
    /// Look again, at the latest at this time.
    Wait(SystemTime),
    /// The look ended it.
    Ended(Look),
}

/// What to do at `now` after a look found `look`; `native` says whether the attempt has a native interrupt.
pub fn next(
    progress: &Progress,
    now: SystemTime,
    look: Look,
    graces: &Graces,
    native: bool,
) -> Next {
    if look != Look::Held {
        return Next::Ended(look);
    }
    let due = |at: Option<SystemTime>, grace: Duration| match at {
        Some(at) if now < at + grace => Next::Wait(at + grace),
        _ => Next::Send(Signal::Kill),
    };
    match (progress.native_at, progress.term_at, progress.kill_at) {
        (None, None, None) if native => Next::Send(Signal::Native),
        (native_at, None, None) => match native_at {
            Some(at) if now < at + graces.native => Next::Wait(at + graces.native),
            _ => Next::Send(Signal::Term),
        },
        (_, term_at, None) => due(term_at, graces.term),
        (_, _, kill_at) => due(kill_at, graces.kill_every),
    }
}

/// Looks at and signals one attempt's processes.
pub trait ProcessControl {
    fn look(&mut self) -> Look;
    /// Send `signal`; `false` when it was not sent (the group no longer held, so no signal may go to its number).
    fn send(&mut self, signal: Signal) -> bool;
}

/// How a pass ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassEnd {
    Empty,
    Lost,
    /// Its group still lives a grace after this pass's SIGKILL: the run stays cancelling, another pass follows.
    Unreaped,
}

/// One pass: what was sent when, how it ended, and the progress the next pass starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pass {
    pub sent: Vec<(Signal, SystemTime)>,
    pub end: PassEnd,
    pub progress: Progress,
}

/// Drive `control` from `progress` until a look ends it or the group outlives this pass's SIGKILL by its grace.
/// Between steps it looks at least every `poll` (`wait` sleeps, or advances a test's clock).
pub fn pass(
    control: &mut dyn ProcessControl,
    clock: &dyn Clock,
    wait: &mut dyn FnMut(Duration),
    graces: &Graces,
    native: bool,
    mut progress: Progress,
    poll: Duration,
) -> Pass {
    let mut sent = Vec::new();
    loop {
        let look = control.look();
        let now = clock.now();
        match next(&progress, now, look, graces, native) {
            Next::Ended(Look::Empty) => {
                return Pass {
                    sent,
                    end: PassEnd::Empty,
                    progress,
                };
            }
            Next::Ended(_) => {
                return Pass {
                    sent,
                    end: PassEnd::Lost,
                    progress,
                };
            }
            Next::Send(Signal::Kill) if sent.iter().any(|(s, _)| *s == Signal::Kill) => {
                return Pass {
                    sent,
                    end: PassEnd::Unreaped,
                    progress,
                };
            }
            Next::Send(signal) => {
                if control.send(signal) {
                    sent.push((signal, now));
                    match signal {
                        Signal::Native => progress.native_at = Some(now),
                        Signal::Term => progress.term_at = Some(now),
                        Signal::Kill => progress.kill_at = Some(now),
                    }
                }
            }
            Next::Wait(until) => wait(until.duration_since(now).unwrap_or_default().min(poll)),
        }
    }
}

/// What the host gives the cancel of a run's processes (docs/adr/ADR-021.md decision 8).
pub trait Processes: Send + Sync {
    /// The control of `attempt`'s processes and whether it has a native interrupt; `None` when it has none to cancel.
    /// Called before each pass, once nothing else runs the attempt (the host stopped its runner task first).
    fn control(
        &self,
        attempt: &crate::persistence::AttemptRow,
    ) -> Option<(Box<dyn ProcessControl + Send>, bool)>;
    /// The workspace's record, taken before the run is CANCELLED (ADR-021 decision 8g); called on a blocking thread.
    fn workspace(&self) -> serde_json::Value;
}

/// The cancels in progress: where each attempt's escalation stands between passes.
#[derive(Debug)]
pub struct Canceller {
    progress: std::sync::Mutex<std::collections::BTreeMap<String, Progress>>,
    /// The attempts whose last pass found their group lost track of: another such pass that sent nothing is not
    /// recorded again (one record per finding).
    lost: std::sync::Mutex<std::collections::BTreeSet<String>>,
    pub graces: Graces,
    /// How often a pass looks at least.
    pub poll: Duration,
}

impl Canceller {
    pub fn new(graces: Graces, poll: Duration) -> Canceller {
        Canceller {
            progress: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            lost: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            graces,
            poll,
        }
    }

    /// One step of cancelling `run_id`: a pass over each of its attempts that starts or runs, recorded as it ended;
    /// once none does, the run's last transaction with its workspace record. A timeout is not a cancel: the run stays
    /// CANCEL_REQUESTED while any group lives (REQ-043 behaviour 5, failure mode 2).
    pub async fn step(
        &self,
        store: &crate::persistence::Store,
        run_id: &str,
        processes: std::sync::Arc<dyn Processes>,
        clock: std::sync::Arc<dyn Clock>,
        wait: std::sync::Arc<dyn Fn(Duration) + Send + Sync>,
    ) -> Result<crate::persistence::Finished, crate::persistence::PersistError> {
        for attempt in store.running_attempts(run_id).await? {
            let Some((mut control, native)) = processes.control(&attempt) else {
                continue;
            };
            let from = self
                .progress
                .lock()
                .map(|p| p.get(&attempt.attempt_id).copied().unwrap_or_default())
                .unwrap_or_default();
            let (graces, poll, at) = (self.graces, self.poll, clock.clone());
            let wait = wait.clone();
            let done = tokio::task::spawn_blocking(move || {
                let mut wait = |d: Duration| wait(d);
                pass(
                    control.as_mut(),
                    at.as_ref(),
                    &mut wait,
                    &graces,
                    native,
                    from,
                    poll,
                )
            })
            .await
            .map_err(|e| {
                crate::persistence::PersistError::IntegrityViolation(format!(
                    "a cancel pass failed: {e}"
                ))
            })?;
            if let Ok(mut progress) = self.progress.lock() {
                if done.end == PassEnd::Unreaped {
                    progress.insert(attempt.attempt_id.clone(), done.progress);
                } else {
                    progress.remove(&attempt.attempt_id);
                }
            }
            let repeated = self.lost.lock().is_ok_and(|mut lost| {
                if done.end == PassEnd::Lost {
                    !lost.insert(attempt.attempt_id.clone()) && done.sent.is_empty()
                } else {
                    lost.remove(&attempt.attempt_id);
                    false
                }
            });
            if repeated {
                continue;
            }
            store
                .cancel_pass(crate::persistence::CancelPass {
                    attempt_id: attempt.attempt_id,
                    sent: done.sent,
                    end: done.end,
                    at: clock.now(),
                })
                .await?;
        }
        if !store.running_attempts(run_id).await?.is_empty() {
            return Ok(crate::persistence::Finished::Running(1));
        }
        // The record reads git and files: off the async workers (M2T06R-06).
        let workspace = tokio::task::spawn_blocking(move || processes.workspace())
            .await
            .map_err(|e| {
                crate::persistence::PersistError::IntegrityViolation(format!(
                    "the workspace record failed: {e}"
                ))
            })?;
        store.finish_cancel(run_id, workspace, clock.now()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::UNIX_EPOCH;

    #[derive(Default)]
    struct FakeClock(Mutex<Duration>);

    impl FakeClock {
        fn advance(&self, by: Duration) {
            if let Ok(mut at) = self.0.lock() {
                *at += by;
            }
        }
        fn secs(&self) -> u64 {
            self.0.lock().map(|d| d.as_secs()).unwrap_or(0)
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + self.0.lock().map(|d| *d).unwrap_or_default()
        }
    }

    /// A group that lives until `empty_at` (seconds on the clock), and what it was sent.
    struct Group<'a> {
        clock: &'a FakeClock,
        empty_at: u64,
        lost: bool,
        sent: Vec<(Signal, u64)>,
    }

    impl ProcessControl for Group<'_> {
        fn look(&mut self) -> Look {
            if self.clock.secs() >= self.empty_at {
                Look::Empty
            } else if self.lost {
                Look::Lost
            } else {
                Look::Held
            }
        }
        fn send(&mut self, signal: Signal) -> bool {
            self.sent.push((signal, self.clock.secs()));
            true
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn the_escalation_waits_each_grace_before_the_next_signal() {
        let held = Look::Held;
        let none = Progress::default();
        assert_eq!(
            next(&none, at(0), held, &GRACES, true),
            Next::Send(Signal::Native)
        );
        // Without a native interrupt (no executor) SIGTERM comes first.
        assert_eq!(
            next(&none, at(0), held, &GRACES, false),
            Next::Send(Signal::Term)
        );
        let native = Progress {
            native_at: Some(at(0)),
            ..none
        };
        assert_eq!(
            next(&native, at(9), held, &GRACES, true),
            Next::Wait(at(10))
        );
        assert_eq!(
            next(&native, at(10), held, &GRACES, true),
            Next::Send(Signal::Term)
        );
        let term = Progress {
            term_at: Some(at(10)),
            ..native
        };
        assert_eq!(next(&term, at(14), held, &GRACES, true), Next::Wait(at(15)));
        assert_eq!(
            next(&term, at(15), held, &GRACES, true),
            Next::Send(Signal::Kill)
        );
        let kill = Progress {
            kill_at: Some(at(15)),
            ..term
        };
        assert_eq!(next(&kill, at(19), held, &GRACES, true), Next::Wait(at(20)));
        assert_eq!(
            next(&kill, at(20), held, &GRACES, true),
            Next::Send(Signal::Kill)
        );
        // Only a look ends it.
        assert_eq!(
            next(&kill, at(16), Look::Empty, &GRACES, true),
            Next::Ended(Look::Empty)
        );
        assert_eq!(
            next(&none, at(0), Look::Lost, &GRACES, true),
            Next::Ended(Look::Lost)
        );
    }

    #[test]
    fn a_group_that_outlives_sigkill_keeps_the_cancel_going_pass_after_pass_until_it_is_empty() {
        // REQ-043 failure mode 2: a group still alive after SIGKILL is no cancel; each pass ends a grace after its
        // SIGKILL and the next sends it again, until a look finds the group empty.
        let clock = FakeClock::default();
        let mut group = Group {
            clock: &clock,
            empty_at: 45,
            lost: false,
            sent: Vec::new(),
        };
        let mut wait = |d: Duration| clock.advance(d);
        let poll = Duration::from_millis(10);
        let first = pass(
            &mut group,
            &clock,
            &mut wait,
            &GRACES,
            true,
            Progress::default(),
            poll,
        );
        assert_eq!(first.end, PassEnd::Unreaped);
        assert_eq!(
            first.sent,
            vec![
                (Signal::Native, at(0)),
                (Signal::Term, at(10)),
                (Signal::Kill, at(15))
            ]
        );
        assert_eq!(clock.secs(), 20);
        let mut progress = first.progress;
        let mut passes = Vec::new();
        loop {
            let next = pass(&mut group, &clock, &mut wait, &GRACES, true, progress, poll);
            progress = next.progress;
            passes.push((next.end, next.sent.clone()));
            if next.end != PassEnd::Unreaped {
                break;
            }
        }
        // SIGKILL again at 20, 25, 30, 35 (each pass unreaped 5 s later) and 40, after which the group is empty at 45.
        assert_eq!(passes.len(), 5, "{passes:?}");
        assert_eq!(passes[0], (PassEnd::Unreaped, vec![(Signal::Kill, at(20))]));
        assert_eq!(passes[4], (PassEnd::Empty, vec![(Signal::Kill, at(40))]));
        assert_eq!(group.sent.len(), 3 + 5);
        // Looked at least every poll interval: the end is found within one of it.
        assert!(clock.0.lock().map(|d| *d).unwrap_or_default() < Duration::from_secs(45) + poll);
    }

    #[test]
    fn a_lost_group_gets_no_further_signal_and_ends_the_pass() {
        let clock = FakeClock::default();
        let mut group = Group {
            clock: &clock,
            empty_at: u64::MAX,
            lost: true,
            sent: Vec::new(),
        };
        let mut wait = |d: Duration| clock.advance(d);
        let lost = pass(
            &mut group,
            &clock,
            &mut wait,
            &GRACES,
            true,
            Progress::default(),
            Duration::from_millis(10),
        );
        assert_eq!(
            (lost.end, lost.sent.len(), group.sent.len()),
            (PassEnd::Lost, 0, 0)
        );
    }

    #[test]
    fn a_signal_that_could_not_be_sent_is_not_recorded_and_is_decided_again() {
        struct Refusing(u32);
        impl ProcessControl for Refusing {
            fn look(&mut self) -> Look {
                if self.0 >= 2 { Look::Empty } else { Look::Held }
            }
            fn send(&mut self, _signal: Signal) -> bool {
                self.0 += 1;
                false
            }
        }
        let clock = FakeClock::default();
        let mut wait = |d: Duration| clock.advance(d);
        let ended = pass(
            &mut Refusing(0),
            &clock,
            &mut wait,
            &GRACES,
            true,
            Progress::default(),
            Duration::from_millis(10),
        );
        assert_eq!(
            (ended.end, ended.sent, ended.progress),
            (PassEnd::Empty, Vec::new(), Progress::default())
        );
    }
}
