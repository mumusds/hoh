//! Time for an adapter's own timers (the deadline, the native cancel grace), so a test can freeze it: REQ-026
//! (AC-026-02) has no timer decide that an agent is idle, and a frozen clock shows it — a turn that needed one would
//! never end. The process substrate keeps its own (tokio) timers for its escalation graces.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// A timer that fires after `duration`, or never on a frozen clock.
pub type Timer = Pin<Box<dyn Future<Output = ()> + Send>>;

pub trait Clock: Send + Sync + std::fmt::Debug {
    fn sleep(&self, duration: Duration) -> Timer;
}

/// Real time (tokio's).
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn sleep(&self, duration: Duration) -> Timer {
        Box::pin(tokio::time::sleep(duration))
    }
}
