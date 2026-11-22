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
    Attempts,
    Tokens,
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
            Stop::Attempts => "max_attempts",
            Stop::Tokens => "max_tokens",
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

/// The share of a limit from which a run's budget is at risk (REQ-042 behaviour 9 and decision 6, CR-025): an initial
/// value, a warning and no guarantee.
pub const RISK_THRESHOLD: f64 = 0.8;

/// A budget past [`RISK_THRESHOLD`] of its limit whose stop does not hold yet (REQ-042 behaviour 9, CR-025).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Risk {
    /// The stop it comes before, named as `run.incomplete` names it.
    pub kind: Stop,
    /// What was judged: what is used plus what is reserved; for the deadline, the wall-clock seconds since the run was
    /// created.
    pub used: f64,
    /// The limit, in the same unit; for the deadline, the seconds from the run's creation to it.
    pub limit: f64,
}

impl Risk {
    /// Its unit: iterations, seconds or usd.
    pub fn unit(&self) -> &'static str {
        match self.kind {
            Stop::Iterations => "iterations",
            Stop::Attempts => "attempts",
            Stop::Tokens => "tokens",
            Stop::Cost => "usd",
            Stop::Duration | Stop::Deadline => "seconds",
        }
    }
}

/// The risks of `budget` at the wall-clock time `wall`, for a run created at `created_at` (REQ-042 behaviour 9,
/// CR-025), in the order of [`stop`]: each kind with a limit set that has reached [`RISK_THRESHOLD`] of it by its stop's
/// own measure, its stop not holding yet. The iterations, the duration and a cost whose price is known by what is used
/// plus what is reserved (a cost whose price is unknown cannot be enforced, so it has none); the deadline by the wall
/// clock, the seconds since the run's creation against the span from it to deadline_at, not judged when that span is
/// not positive (a wall clock behind the creation has no time passed).
pub fn risks(budget: &Budget, created_at: SystemTime, wall: SystemTime) -> Vec<Risk> {
    let mut out = Vec::new();
    if let Some(deadline) = budget.deadline_at
        && wall < deadline
        && let Ok(span) = deadline.duration_since(created_at)
        && !span.is_zero()
    {
        let (used, limit) = (
            wall.duration_since(created_at)
                .map_or(0.0, |d| d.as_secs_f64()),
            span.as_secs_f64(),
        );
        if used >= RISK_THRESHOLD * limit {
            out.push(Risk {
                kind: Stop::Deadline,
                used,
                limit,
            });
        }
    }
    let of = |kind, ledger: Option<Ledger>| {
        let ledger = ledger.filter(|l| !l.used_up())?;
        let (used, limit) = (ledger.used + ledger.reserved, ledger.limit?);
        (used >= RISK_THRESHOLD * limit).then_some(Risk { kind, used, limit })
    };
    out.extend(of(Stop::Iterations, budget.iterations));
    out.extend(of(Stop::Duration, budget.wall_seconds));
    out.extend(of(
        Stop::Cost,
        budget.usd.filter(|(_, known)| *known).map(|(l, _)| l),
    ));
    out
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

    fn risk(kind: Stop, used: f64, limit: f64) -> Risk {
        Risk { kind, used, limit }
    }

    #[test]
    fn a_budget_is_at_risk_from_80_percent_of_its_limit_until_its_stop() {
        // REQ-042 behaviour 9 (CR-025): each kind with a limit, by its stop's measure (used plus reserved): 79 % none,
        // 80 % one, and none once its stop holds (the stop says so); a cost whose price is unknown and an unset limit
        // have none.
        let (created, wall) = (UNIX_EPOCH, UNIX_EPOCH + Duration::from_secs(1));
        let none: [Risk; 0] = [];
        let iterations = |used, reserved| Budget {
            iterations: Some(ledger(Some(100.0), used, reserved)),
            ..Budget::default()
        };
        assert_eq!(risks(&iterations(79.0, 0.0), created, wall), none);
        assert_eq!(
            risks(&iterations(80.0, 0.0), created, wall),
            [risk(Stop::Iterations, 80.0, 100.0)]
        );
        assert_eq!(
            risks(&iterations(60.0, 20.0), created, wall),
            [risk(Stop::Iterations, 80.0, 100.0)],
            "what is reserved counts"
        );
        assert_eq!(
            risks(&iterations(99.0, 1.0), created, wall),
            none,
            "its stop holds"
        );
        let duration = |used| Budget {
            wall_seconds: Some(ledger(Some(3_600.0), used, 0.0)),
            ..Budget::default()
        };
        assert_eq!(risks(&duration(2_879.0), created, wall), none);
        assert_eq!(
            risks(&duration(2_880.0), created, wall),
            [risk(Stop::Duration, 2_880.0, 3_600.0)]
        );
        let cost = |used, known| Budget {
            usd: Some((ledger(Some(5.0), used, 0.0), known)),
            ..Budget::default()
        };
        assert_eq!(risks(&cost(3.9, true), created, wall), none);
        assert_eq!(
            risks(&cost(4.0, true), created, wall),
            [risk(Stop::Cost, 4.0, 5.0)]
        );
        assert_eq!(
            risks(&cost(4.5, false), created, wall),
            none,
            "an unknown price"
        );
        let unset = Budget {
            iterations: Some(ledger(None, 1e9, 0.0)),
            ..Budget::default()
        };
        assert_eq!(risks(&unset, created, wall), none);
        assert_eq!(risks(&Budget::default(), created, wall), none);
        assert_eq!(
            [Stop::Iterations, Stop::Duration, Stop::Cost, Stop::Deadline]
                .map(|kind| risk(kind, 0.0, 1.0).unit()),
            ["iterations", "seconds", "usd", "seconds"]
        );
    }

    #[test]
    fn the_deadline_is_at_risk_by_the_wall_clock_over_the_runs_span() {
        // REQ-042 behaviour 9 (CR-025): the seconds since the run's creation against its span to deadline_at, from
        // 80 % until the deadline's stop; a wall clock behind the creation has no time passed; a span that is not
        // positive is not judged. Several kinds come in the order of the stops.
        let created = UNIX_EPOCH + Duration::from_secs(1_000);
        let s = Duration::from_secs;
        let none: [Risk; 0] = [];
        let budget = Budget {
            deadline_at: Some(created + s(100)),
            ..Budget::default()
        };
        assert_eq!(risks(&budget, created, created + s(79)), none);
        assert_eq!(
            risks(&budget, created, created + s(80)),
            [risk(Stop::Deadline, 80.0, 100.0)]
        );
        assert_eq!(
            risks(&budget, created, created + Duration::from_millis(99_999)),
            [risk(Stop::Deadline, 99.999, 100.0)]
        );
        assert_eq!(
            risks(&budget, created, created + s(100)),
            none,
            "its stop holds"
        );
        assert_eq!(
            risks(&budget, created, created - s(10)),
            none,
            "the wall clock behind"
        );
        let at_creation = Budget {
            deadline_at: Some(created),
            ..Budget::default()
        };
        assert_eq!(risks(&at_creation, created, created - s(1)), none);
        let before = Budget {
            deadline_at: Some(created - s(50)),
            ..Budget::default()
        };
        assert_eq!(risks(&before, created, created - s(60)), none);
        let all = Budget {
            deadline_at: Some(created + s(100)),
            iterations: Some(ledger(Some(100.0), 80.0, 0.0)),
            wall_seconds: Some(ledger(Some(3_600.0), 2_880.0, 0.0)),
            usd: Some((ledger(Some(5.0), 4.0, 0.0), true)),
        };
        assert_eq!(
            risks(&all, created, created + s(80)),
            [
                risk(Stop::Deadline, 80.0, 100.0),
                risk(Stop::Iterations, 80.0, 100.0),
                risk(Stop::Duration, 2_880.0, 3_600.0),
                risk(Stop::Cost, 4.0, 5.0)
            ]
        );
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
