//! The single transition table of Run, Task and Attempt states (REQ-036, docs/adr/ADR-011.md §1). Every state change
//! the product makes is checked against it: the persistence writer refuses an illegal compare-and-set, and Commander
//! proposals are validated against it before they are applied ([`crate::decision`]).

use crate::protocol::{AllVariants, RejectionReason, RunState, TaskState};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Attempt states (`attempts.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AttemptState {
    Prepared,
    Starting,
    Running,
    Settled,
    Failed,
    Cancelled,
    UnknownEffect,
}

impl AllVariants for AttemptState {
    const ALL: &'static [Self] = &[
        Self::Prepared,
        Self::Starting,
        Self::Running,
        Self::Settled,
        Self::Failed,
        Self::Cancelled,
        Self::UnknownEffect,
    ];
}

/// One state machine of the table.
pub trait Lifecycle: AllVariants + Copy + Eq + Ord + fmt::Debug {
    /// The machine's name in errors and events.
    const MACHINE: &'static str;

    /// The states `self` may move to.
    fn successors(self) -> &'static [Self];

    /// The name stored in the database and used on the wire.
    fn as_str(self) -> &'static str;

    /// The state stored as `name`.
    fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|s| s.as_str() == name)
    }

    /// A state without successors.
    fn is_terminal(self) -> bool {
        self.successors().is_empty()
    }
}

/// A move the table does not allow (`ILLEGAL_TRANSITION`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition<S> {
    pub from: S,
    pub to: S,
}

impl<S> IllegalTransition<S> {
    pub fn reason(&self) -> RejectionReason {
        RejectionReason::IllegalTransition
    }
}

impl<S: Lifecycle> fmt::Display for IllegalTransition<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ILLEGAL_TRANSITION: {} {} -> {} is not in the transition table",
            S::MACHINE,
            self.from.as_str(),
            self.to.as_str()
        )
    }
}

/// Whether the table allows `from` -> `to`.
pub fn check<S: Lifecycle>(from: S, to: S) -> Result<(), IllegalTransition<S>> {
    if from.successors().contains(&to) {
        Ok(())
    } else {
        Err(IllegalTransition { from, to })
    }
}

/// The working run states (W in ADR-011); `runs.resume_state` holds one of them.
pub const WORKING: [RunState; 6] = [
    RunState::Created,
    RunState::Discovering,
    RunState::Specifying,
    RunState::Ready,
    RunState::Running,
    RunState::Verifying,
];

/// Whether `state` is a working run state.
pub fn is_working(state: RunState) -> bool {
    WORKING.contains(&state)
}

/// Check a run move and return `runs.resume_state` after it (ADR-011 §1): leaving a working state for a suspended
/// one saves the state left; moving between suspended states keeps it; resuming must return to it and clears it; a
/// terminal state keeps none.
pub fn resume_state(
    from: RunState,
    to: RunState,
    saved: Option<RunState>,
) -> Result<Option<RunState>, IllegalTransition<RunState>> {
    check(from, to)?;
    Ok(if to.is_terminal() || is_working(to) {
        if is_working(to) && !is_working(from) && saved != Some(to) {
            return Err(IllegalTransition { from, to });
        }
        None
    } else if is_working(from) {
        Some(from)
    } else {
        saved
    })
}

impl Lifecycle for RunState {
    const MACHINE: &'static str = "run";

    fn successors(self) -> &'static [Self] {
        use RunState::*;
        match self {
            Created => &[
                Discovering,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Discovering => &[
                Specifying,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Specifying => &[
                Ready,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Ready => &[
                Running,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Running => &[
                Verifying,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Verifying => &[
                Running,
                Completed,
                PauseRequested,
                WaitingExternal,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            // Resuming returns to `runs.resume_state` (one of the working states).
            WaitingExternal => &[
                Created,
                Discovering,
                Specifying,
                Ready,
                Running,
                Verifying,
                Blocked,
                PauseRequested,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            Blocked => &[
                Created,
                Discovering,
                Specifying,
                Ready,
                Running,
                Verifying,
                PauseRequested,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            PauseRequested => &[Paused, CancelRequested, Failed],
            // BLOCKED again when the pause ends while blockers remain (docs/06: the run returns to resume_state only
            // once the blocker set is empty).
            Paused => &[
                Created,
                Discovering,
                Specifying,
                Ready,
                Running,
                Verifying,
                Blocked,
                CancelRequested,
                Incomplete,
                Failed,
            ],
            CancelRequested => &[Cancelled, Failed],
            Incomplete => &[
                Created,
                Discovering,
                Specifying,
                Ready,
                Running,
                Verifying,
                CancelRequested,
                Failed,
            ],
            Completed | Cancelled | Failed => &[],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Created => "CREATED",
            Self::Discovering => "DISCOVERING",
            Self::Specifying => "SPECIFYING",
            Self::Ready => "READY",
            Self::Running => "RUNNING",
            Self::Verifying => "VERIFYING",
            Self::PauseRequested => "PAUSE_REQUESTED",
            Self::Paused => "PAUSED",
            Self::WaitingExternal => "WAITING_EXTERNAL",
            Self::Blocked => "BLOCKED",
            Self::CancelRequested => "CANCEL_REQUESTED",
            Self::Cancelled => "CANCELLED",
            Self::Incomplete => "INCOMPLETE",
            Self::Failed => "FAILED",
            Self::Completed => "COMPLETED",
        }
    }
}

impl Lifecycle for TaskState {
    const MACHINE: &'static str = "task";

    fn successors(self) -> &'static [Self] {
        use TaskState::*;
        match self {
            Pending => &[Ready, Blocked, Cancelled],
            Ready => &[Leased, Pending, Blocked, Cancelled],
            // READY again: a lease released before the start, a failed attempt awaiting its retry (a new attempt),
            // rework, or a failed verification awaiting the fix.
            Leased => &[Running, Ready, Blocked, Cancelled],
            Running => &[Implemented, Ready, Blocked, Cancelled],
            Implemented => &[Verifying, Ready, Blocked, Cancelled],
            Verifying => &[Accepted, Ready, Blocked, Cancelled],
            Blocked => &[Pending, Ready, Cancelled],
            Accepted | Cancelled => &[],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "PENDING",
            Self::Ready => "READY",
            Self::Leased => "LEASED",
            Self::Running => "RUNNING",
            Self::Implemented => "IMPLEMENTED",
            Self::Verifying => "VERIFYING",
            Self::Accepted => "ACCEPTED",
            Self::Blocked => "BLOCKED",
            Self::Cancelled => "CANCELLED",
        }
    }
}

impl Lifecycle for AttemptState {
    const MACHINE: &'static str = "attempt";

    fn successors(self) -> &'static [Self] {
        use AttemptState::*;
        match self {
            // Nothing external has happened yet, so the outcome is never unknown.
            Prepared => &[Starting, Failed, Cancelled],
            Starting => &[Running, Failed, Cancelled, UnknownEffect],
            Running => &[Settled, Failed, Cancelled, UnknownEffect],
            // Settled by diagnosis or observation; a retry is a new attempt.
            UnknownEffect => &[Settled, Failed, Cancelled],
            Settled | Failed | Cancelled => &[],
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "PREPARED",
            Self::Starting => "STARTING",
            Self::Running => "RUNNING",
            Self::Settled => "SETTLED",
            Self::Failed => "FAILED",
            Self::Cancelled => "CANCELLED",
            Self::UnknownEffect => "UNKNOWN_EFFECT",
        }
    }
}

#[cfg(test)]
mod tests;
