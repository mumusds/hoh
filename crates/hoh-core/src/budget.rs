//! A run's budget stops (REQ-042 behaviour 6 and 7, docs/adr/ADR-020.md decision 7): no new dispatch once a limit is
//! used up (what is used plus what is reserved) or the deadline has come; a cost limit whose price is unknown cannot
//! be enforced, so it blocks before any dispatch (BUDGET_UNENFORCEABLE). Pure: the store reads the budgets rows.

use std::time::SystemTime;

/// One `budgets` row of a run: its limit (`None`: no limit), what is used and what is reserved.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Ledger {
    pub limit: Option<f64>,
    pub used: f64,
    pub reserved: f64,
}

impl Ledger {
    /// Whether it is used up: used plus reserved has reached the limit.
    pub fn used_up(&self) -> bool {
        self.limit
            .is_some_and(|limit| self.used + self.reserved >= limit)
    }
}

/// A run's budget: its rows by kind and its deadline.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Budget {
    /// `max_iterations`.
    pub iterations: Option<Ledger>,
    /// `max_duration_hours`, in seconds.
    pub wall_seconds: Option<Ledger>,
    /// `max_usd`, and whether the price of the models used is known (`budgets.price_known`).
    pub usd: Option<(Ledger, bool)>,
    /// `deadline_at`.
    pub deadline_at: Option<SystemTime>,
}

/// Why a run dispatches nothing more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Deadline,
    Iterations,
    Duration,
    Cost,
}

impl Stop {
    /// As the `run.incomplete` event names it.
    pub fn as_str(self) -> &'static str {
        match self {
            Stop::Deadline => "deadline_at",
            Stop::Iterations => "max_iterations",
            Stop::Duration => "max_duration_hours",
            Stop::Cost => "max_usd",
        }
    }
}

/// The first stop that holds at `now`, if any: the deadline, then the iterations, the duration and a cost whose price
/// is known.
pub fn stop(budget: &Budget, now: SystemTime) -> Option<Stop> {
    if budget.deadline_at.is_some_and(|deadline| now >= deadline) {
        return Some(Stop::Deadline);
    }
    if budget.iterations.is_some_and(|l| l.used_up()) {
        return Some(Stop::Iterations);
    }
    if budget.wall_seconds.is_some_and(|l| l.used_up()) {
        return Some(Stop::Duration);
    }
    if budget
        .usd
        .is_some_and(|(l, price_known)| price_known && l.used_up())
    {
        return Some(Stop::Cost);
    }
    None
}

/// Whether a cost limit is set while the price is unknown: it cannot be enforced (CR-002, BUDGET_UNENFORCEABLE).
pub fn unenforceable(budget: &Budget) -> bool {
    budget
        .usd
        .is_some_and(|(l, price_known)| l.limit.is_some() && !price_known)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn ledger(limit: Option<f64>, used: f64, reserved: f64) -> Ledger {
        Ledger {
            limit,
            used,
            reserved,
        }
    }

    #[test]
    fn max_iterations_stops_dispatch_once_used_and_reserved_reach_it() {
        // REQ-042 behaviour 6 (M2-T19 done_when: a unit test of the max_iterations stop).
        let now = UNIX_EPOCH + Duration::from_secs(100);
        let with = |used, reserved| Budget {
            iterations: Some(ledger(Some(2.0), used, reserved)),
            ..Budget::default()
        };
        assert_eq!(stop(&with(0.0, 0.0), now), None);
        assert_eq!(stop(&with(1.0, 0.0), now), None);
        assert_eq!(stop(&with(1.0, 1.0), now), Some(Stop::Iterations));
        assert_eq!(stop(&with(2.0, 0.0), now), Some(Stop::Iterations));
        assert_eq!(stop(&with(3.0, 0.0), now), Some(Stop::Iterations));
        // No limit, no stop.
        let unlimited = Budget {
            iterations: Some(ledger(None, 1e9, 0.0)),
            ..Budget::default()
        };
        assert_eq!(stop(&unlimited, now), None);
        assert_eq!(stop(&Budget::default(), now), None);
    }

    #[test]
    fn deadline_at_stops_dispatch_from_its_instant_on() {
        // REQ-042 behaviour 6 and failure mode 4 (M2-T19 done_when: a unit test of the deadline_at stop).
        let deadline = UNIX_EPOCH + Duration::from_secs(1_000);
        let budget = Budget {
            deadline_at: Some(deadline),
            ..Budget::default()
        };
        assert_eq!(stop(&budget, deadline - Duration::from_millis(1)), None);
        assert_eq!(stop(&budget, deadline), Some(Stop::Deadline));
        assert_eq!(
            stop(&budget, deadline + Duration::from_secs(1)),
            Some(Stop::Deadline)
        );
        // The deadline is named first when several stops hold.
        let all = Budget {
            iterations: Some(ledger(Some(1.0), 1.0, 0.0)),
            wall_seconds: Some(ledger(Some(10.0), 10.0, 0.0)),
            deadline_at: Some(deadline),
            ..Budget::default()
        };
        assert_eq!(stop(&all, deadline), Some(Stop::Deadline));
    }

    #[test]
    fn the_duration_and_a_known_cost_stop_too() {
        let now = UNIX_EPOCH;
        let wall = Budget {
            wall_seconds: Some(ledger(Some(3_600.0), 3_599.0, 0.0)),
            ..Budget::default()
        };
        assert_eq!(stop(&wall, now), None);
        let wall = Budget {
            wall_seconds: Some(ledger(Some(3_600.0), 3_600.0, 0.0)),
            ..Budget::default()
        };
        assert_eq!(stop(&wall, now), Some(Stop::Duration));
        let cost = |known| Budget {
            usd: Some((ledger(Some(5.0), 5.0, 0.0), known)),
            ..Budget::default()
        };
        assert_eq!(stop(&cost(true), now), Some(Stop::Cost));
        // An unknown price is no stop: it blocks before any dispatch instead.
        assert_eq!(stop(&cost(false), now), None);
    }

    #[test]
    fn a_cost_limit_with_an_unknown_price_cannot_be_enforced() {
        // CR-002, AC-042-06: Rc (max_usd = 5, price unknown) is unenforceable; Rd (no max_usd) is not.
        let rc = Budget {
            usd: Some((ledger(Some(5.0), 0.0, 0.0), false)),
            ..Budget::default()
        };
        assert!(unenforceable(&rc));
        let rd = Budget {
            usd: Some((ledger(None, 0.0, 0.0), false)),
            ..Budget::default()
        };
        assert!(!unenforceable(&rd));
        assert!(!unenforceable(&Budget::default()));
        let priced = Budget {
            usd: Some((ledger(Some(5.0), 0.0, 0.0), true)),
            ..Budget::default()
        };
        assert!(!unenforceable(&priced));
    }
}
