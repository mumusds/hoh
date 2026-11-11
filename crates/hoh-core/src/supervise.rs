//! Commander supervision (REQ-021, docs/adr/ADR-025.md decision 1): which events wake the Commander, the key they
//! join under and when the trigger they make is due, as pure functions. The store keeps the triggers
//! (`persistence::supervise`).

#[cfg(test)]
mod tests;

use serde_json::Value;
use std::time::{Duration, SystemTime};

/// How long a key's trigger waits after its last event (REQ-021 decision 1: trailing debounce).
pub const QUIET: Duration = Duration::from_secs(2);

/// How long after its first event a key's trigger fires at the latest, however its events keep coming (decision 1).
pub const CAP: Duration = Duration::from_secs(10);

/// How much a trigger matters, as `supervisor_triggers.severity` names it; ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Normal,
    High,
    Critical,
    /// An IntegrityViolation-class event: it skips the debounce (REQ-021 behaviour 4, decision 2).
    Integrity,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Normal => "normal",
            Severity::High => "high",
            Severity::Critical => "critical",
            Severity::Integrity => "integrity",
        }
    }

    pub fn named(name: &str) -> Option<Severity> {
        [
            Severity::Normal,
            Severity::High,
            Severity::Critical,
            Severity::Integrity,
        ]
        .into_iter()
        .find(|s| s.as_str() == name)
    }
}

/// `supervision.policy` (REQ-021 behaviour 1, decision 5): which events beyond the important set wake the Commander.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// The important set only.
    Efficient,
    /// Also each iteration's end (the round boundary).
    Balanced,
    /// Also each high-risk task's end (`risk_reduction` of 4 or more).
    Intensive,
}

impl Policy {
    pub fn named(name: &str) -> Option<Policy> {
        match name {
            "efficient" => Some(Policy::Efficient),
            "balanced" => Some(Policy::Balanced),
            "intensive" => Some(Policy::Intensive),
            _ => None,
        }
    }
}

/// The key a run's events join under (REQ-021 behaviour 3): the problem's fingerprint and the spec revision.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key {
    pub fingerprint: String,
    pub spec_revision: Option<i64>,
}

/// An event that wakes the Commander: its key and severity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wake {
    pub key: Key,
    pub severity: Severity,
}

/// The severity of an issue event's own `severity` that wakes the Commander: critical and high only (behaviour 2).
fn issue_severity(payload: &Value) -> Option<Severity> {
    match payload["severity"].as_str() {
        Some("critical") => Some(Severity::Critical),
        Some("high") => Some(Severity::High),
        _ => None,
    }
}

/// Whether event `kind` with `payload` wakes the Commander under `policy`, and under which key and severity
/// (REQ-021 behaviours 1 to 3; the kind names are initial values, decision 4). Any other event is ignored: token
/// deltas, tool output, heartbeats, medium and low issues, routine candidate_ready (the Runtime hands it to QA and
/// Review). The key is the event's `fingerprint` and `spec_revision`, else its kind and subject and the run's current
/// spec revision `spec_revision`.
pub fn classify(
    policy: Policy,
    kind: &str,
    payload: &Value,
    spec_revision: Option<i64>,
) -> Option<Wake> {
    let severity = match kind {
        "integrity_violation" => Severity::Integrity,
        "issue.opened" => issue_severity(payload)?,
        "need_decision"
        | "no_progress"
        | "budget.risk"
        | "team.binding_blocked"
        | "team.binding_refused" => Severity::High,
        "spec.changed" | "milestone_ready" | "release_request" => Severity::Normal,
        "iteration.completed" if policy != Policy::Efficient => Severity::Normal,
        "task.implemented"
            if policy == Policy::Intensive
                && payload["risk_reduction"].as_i64().is_some_and(|r| r >= 4) =>
        {
            Severity::Normal
        }
        _ => return None,
    };
    let fingerprint = match payload["fingerprint"].as_str() {
        Some(named) => named.to_owned(),
        None => {
            let subject = ["task_id", "issue_id", "attempt_id", "role", "candidate_id"]
                .iter()
                .find_map(|field| payload[*field].as_str())
                .unwrap_or("");
            format!("{kind}/{subject}")
        }
    };
    Some(Wake {
        key: Key {
            fingerprint,
            spec_revision: payload["spec_revision"].as_i64().or(spec_revision),
        },
        severity,
    })
}

/// A key's trigger while it collects events: when its first and last events were taken in, its highest severity and
/// its events' seqs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pending {
    pub first_at: SystemTime,
    pub last_at: SystemTime,
    pub severity: Severity,
    pub seqs: Vec<i64>,
}

impl Pending {
    /// A trigger made by event `seq`, taken in at `at`.
    pub fn new(seq: i64, at: SystemTime, severity: Severity) -> Pending {
        Pending {
            first_at: at,
            last_at: at,
            severity,
            seqs: vec![seq],
        }
    }

    /// Event `seq` of the same key, taken in at `at`, joins it (REQ-021 decision 3).
    pub fn join(&mut self, seq: i64, at: SystemTime, severity: Severity) {
        self.seqs.push(seq);
        self.last_at = self.last_at.max(at);
        self.severity = self.severity.max(severity);
    }

    /// When it fires: [`QUIET`] after its last event, at most [`CAP`] after its first; an integrity event at once.
    pub fn due_at(&self) -> SystemTime {
        due_at(self.severity, self.first_at, self.last_at)
    }
}

/// When a trigger of `severity` whose first and last events were taken in at `first` and `last` fires.
pub fn due_at(severity: Severity, first: SystemTime, last: SystemTime) -> SystemTime {
    if severity == Severity::Integrity {
        last
    } else {
        (last + QUIET).min(first + CAP)
    }
}
