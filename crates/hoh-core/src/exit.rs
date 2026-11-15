//! A run's exit code (REQ-004's table) for the run states that end a client: the code a foreground client exits with
//! and the `exit_code` of a run's `run.result` (docs/adr/ADR-031.md decision 3). The client's own codes (usage, host,
//! signals) stay with the client.

use crate::protocol::RunState;

/// The exit code of a run in `state`; `None` for a state that does not end a client (it keeps waiting). Only
/// COMPLETED is 0.
pub fn run_code(state: RunState) -> Option<u8> {
    match state {
        RunState::Completed => Some(0),
        RunState::Failed => Some(1),
        RunState::Blocked => Some(3),
        RunState::Incomplete => Some(4),
        RunState::Cancelled => Some(5),
        RunState::Paused => Some(6),
        RunState::Created
        | RunState::Discovering
        | RunState::Specifying
        | RunState::Ready
        | RunState::Running
        | RunState::Verifying
        | RunState::PauseRequested
        | RunState::WaitingExternal
        | RunState::CancelRequested => None,
    }
}

#[cfg(test)]
mod tests;
