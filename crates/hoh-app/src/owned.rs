//! The client's one start gate (docs/adr/ADR-031.md, 设计修正 2, A): every process a client or doctor starts goes
//! through it, as a helper or as a host. A start takes a ticket under the gate's lock (refused once the stop began),
//! spawns without the lock, and hands off under the lock again: a helper is registered (and killed at once if the stop
//! began meanwhile), a host recorded and never signalled. The in-flight counts drop only at the end of the hand-off,
//! which a ticket's drop (a panic's unwinding included) also runs, so each start is counted down exactly once.
//!
//! The stop refuses new tickets, kills every registered helper's group and leader, waits within its bound for the
//! helper starts in flight to end, reads the registry again (the late registrations), and reaps through the handles,
//! as A2 orders: see the exit, clean the group, reap. In doctor, which adopts its descendants, it then kills every
//! descendant still alive ([`descendants`], B). What it could not confirm it reports, never claims.

use hoh_core::spawn::{Direct, Helper, Shared, Spawner, WaitOnly};
use rustix::process::Signal;
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The gate's state, under its lock (no lock is held across a spawn or a wait).
#[derive(Debug, Default)]
struct State {
    stopping: bool,
    helpers_in_flight: usize,
    hosts_in_flight: usize,
    helpers: Vec<Arc<Shared>>,
    hosts: Vec<u32>,
    /// Whether the stop also kills every descendant of this process (doctor, B).
    sweeps: bool,
}

/// The start gate of a client process.
#[derive(Debug, Default)]
pub struct Gate {
    state: Mutex<State>,
}

/// What a stop found: whether helper starts were still in flight at its bound, the registered helpers it could not
/// confirm ended (their pids), and the hosts handed off or still starting (never signalled).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub helpers_in_flight: usize,
    pub unconfirmed: Vec<u32>,
    pub hosts: Vec<u32>,
    pub hosts_in_flight: usize,
}

/// What a ticket is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Helper,
    Host,
}

/// Where a start is.
#[derive(Debug)]
enum Stage {
    NotSpawned,
    SpawnedHelper(Arc<Shared>),
    SpawnedHost(u32),
    HandedOff,
}

/// A start's ticket: the one owner of its in-flight count.
#[derive(Debug)]
struct Ticket<'a> {
    gate: &'a Gate,
    kind: Kind,
    stage: Stage,
}

impl Gate {
    fn locked(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A ticket for a start of `kind`, or why not: refused once the stop began.
    fn take(&self, kind: Kind) -> std::io::Result<Ticket<'_>> {
        let mut state = self.locked();
        if state.stopping {
            return Err(std::io::Error::other(
                "the client is ending (a signal came): nothing more is started",
            ));
        }
        match kind {
            Kind::Helper => state.helpers_in_flight += 1,
            Kind::Host => state.hosts_in_flight += 1,
        }
        Ok(Ticket {
            gate: self,
            kind,
            stage: Stage::NotSpawned,
        })
    }

    /// Make this process its descendants' subreaper, and have the stop kill every descendant it finds (doctor's, B):
    /// a process a helper set apart and orphaned is then re-parented here, not to init. Without the subreaper (the
    /// call refused) the stop still finds every descendant whose parents live.
    pub fn adopt_descendants(&self) -> std::io::Result<()> {
        self.locked().sweeps = true;
        Ok(rustix::process::set_child_subreaper(Some(
            rustix::process::getpid(),
        ))?)
    }

    /// Stop: refuse new tickets, kill every registered helper (its group, then its leader), wait at most `bound` for
    /// the helper starts in flight to end, read the registry again, and reap what exited within the rest of the bound,
    /// seeing each exit and cleaning its group first; with descendants adopted, kill every one still alive and scan
    /// again within the bound. Hosts are never signalled.
    pub fn stop(&self, bound: Duration) -> Stopped {
        let end = Instant::now() + bound;
        let first = {
            let mut state = self.locked();
            state.stopping = true;
            state.helpers.clone()
        };
        for helper in &first {
            helper.signal_group(Signal::KILL);
            helper.kill();
        }
        // The starts in flight end: each hands off (and, the stop having begun, kills what it started) or fails.
        let in_flight = loop {
            let left = self.locked().helpers_in_flight;
            if left == 0 || Instant::now() >= end {
                break left;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let (helpers, hosts, hosts_in_flight, sweeps) = {
            let state = self.locked();
            (
                state.helpers.clone(),
                state.hosts.clone(),
                state.hosts_in_flight,
                state.sweeps,
            )
        };
        let mut unconfirmed = Vec::new();
        for helper in &helpers {
            helper.signal_group(Signal::KILL);
            helper.kill();
            let left = end.saturating_duration_since(Instant::now());
            let reaped = helper.reaped()
                || (helper.exited_within(left) && {
                    helper.signal_group(Signal::KILL);
                    matches!(helper.reap(), Ok(Some(_)))
                });
            if !reaped {
                unconfirmed.push(helper.pid());
            }
        }
        if sweeps {
            for pid in descendants::sweep(std::process::id(), end) {
                if !unconfirmed.contains(&pid) {
                    unconfirmed.push(pid);
                }
            }
        }
        Stopped {
            helpers_in_flight: in_flight,
            unconfirmed,
            hosts,
            hosts_in_flight,
        }
    }

    /// Hand a helper start off: register it (and kill it at once if the stop began), and count the start down.
    fn hand_off_helper(&self, shared: &Arc<Shared>) {
        let mut state = self.locked();
        state.helpers.retain(|h| !h.reaped());
        state.helpers.push(shared.clone());
        if state.stopping {
            // Registry lock, then the handle's lock: the one order (A2).
            shared.signal_group(Signal::KILL);
            shared.kill();
        }
        state.helpers_in_flight = state.helpers_in_flight.saturating_sub(1);
    }

    /// Hand a host start off: record it, never signal it, and count the start down.
    fn hand_off_host(&self, pid: Option<u32>) {
        let mut state = self.locked();
        if let Some(pid) = pid {
            state.hosts.push(pid);
        }
        state.hosts_in_flight = state.hosts_in_flight.saturating_sub(1);
    }
}

impl Ticket<'_> {
    /// The end of the hand-off: the only place a ticket's count goes down.
    fn finish(&mut self) {
        match std::mem::replace(&mut self.stage, Stage::HandedOff) {
            Stage::HandedOff => {}
            Stage::NotSpawned => match self.kind {
                Kind::Helper => self.gate.hand_off_none(Kind::Helper),
                Kind::Host => self.gate.hand_off_host(None),
            },
            Stage::SpawnedHelper(shared) => self.gate.hand_off_helper(&shared),
            Stage::SpawnedHost(pid) => self.gate.hand_off_host(Some(pid)),
        }
    }
}

impl Drop for Ticket<'_> {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Gate {
    /// A start that spawned nothing counts down.
    fn hand_off_none(&self, kind: Kind) {
        let mut state = self.locked();
        match kind {
            Kind::Helper => state.helpers_in_flight = state.helpers_in_flight.saturating_sub(1),
            Kind::Host => state.hosts_in_flight = state.hosts_in_flight.saturating_sub(1),
        }
    }

    /// [`Spawner::helper`] with `spawn` doing the spawn (a test's seam).
    fn helper_by(
        &self,
        command: &mut Command,
        spawn: &dyn Fn(&mut Command) -> std::io::Result<Helper>,
    ) -> std::io::Result<Helper> {
        let mut ticket = self.take(Kind::Helper)?;
        let helper = spawn(command)?;
        ticket.stage = Stage::SpawnedHelper(helper.shared.clone());
        ticket.finish();
        Ok(helper)
    }

    /// [`Spawner::host`] with `spawn` doing the spawn (a test's seam).
    fn host_by(
        &self,
        command: &mut Command,
        spawn: &dyn Fn(&mut Command) -> std::io::Result<WaitOnly>,
    ) -> std::io::Result<WaitOnly> {
        let mut ticket = self.take(Kind::Host)?;
        let host = spawn(command)?;
        ticket.stage = Stage::SpawnedHost(host.id());
        ticket.finish();
        Ok(host)
    }
}

impl Spawner for Gate {
    fn helper(&self, command: &mut Command) -> std::io::Result<Helper> {
        self.helper_by(command, &|command| Direct::plain().helper(command))
    }

    fn host(&self, command: &mut Command) -> std::io::Result<WaitOnly> {
        self.host_by(command, &|command| Direct::plain().host(command))
    }
}

mod descendants;

#[cfg(test)]
mod tests;
