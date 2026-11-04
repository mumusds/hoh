//! The exit code table of REQ-004, one place for every client: a run's final state, or what ended the client.
//! Until the clients that wait for runs and hosts arrive (M2-T13), only the usage code is used outside the tests.
#![allow(dead_code)]

use hoh_core::protocol::RunState;

/// Something on the client side that ends an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientEvent {
    /// The command line is invalid.
    Usage,
    /// The configuration is invalid.
    Config,
    /// No host can be reached or started.
    HostUnavailable,
    /// The client detached on SIGINT; the run goes on.
    Sigint,
    /// The client detached on SIGTERM; the run goes on.
    Sigterm,
}

/// What ends an invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    Run(RunState),
    Client(ClientEvent),
}

/// The process exit code for `ending`; `None` for a run state that does not end a client (it keeps waiting).
/// Only COMPLETED is 0.
pub fn exit_code(ending: Ending) -> Option<u8> {
    match ending {
        Ending::Run(state) => match state {
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
        },
        Ending::Client(event) => Some(match event {
            ClientEvent::Usage => 64,
            ClientEvent::HostUnavailable => 69,
            ClientEvent::Config => 78,
            ClientEvent::Sigint => 130,
            ClientEvent::Sigterm => 143,
        }),
    }
}

#[cfg(test)]
mod tests;
