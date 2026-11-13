//! Whether an Issue is resolved (docs/adr/ADR-029.md decision 10, J6): a function of the persistent observation facts,
//! the acceptance rows and the vetoes (a candidate with an INVALID evidence), never of a candidate's state or of the
//! order the facts were written in; and the one state change that materialises it. Pure: the store's `settle_all`
//! materialises it in every transaction that writes such a fact, and a deciding reader evaluates it directly.

use super::classify::{self, Row};
use std::collections::{BTreeMap, BTreeSet};

/// An observation's polarity: the Issue still present, or fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Polarity {
    Present,
    Absent,
}

impl Polarity {
    /// As `issue.observation` spells it.
    pub fn code(self) -> &'static str {
        match self {
            Polarity::Present => "present",
            Polarity::Absent => "absent",
        }
    }
}

/// One observation fact on a candidate (`issue.observation`, or a change request's `issue.opened` read as present).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    pub id: String,
    pub fingerprint: String,
    pub polarity: Polarity,
    pub candidate: String,
    /// The Issues it names: an absent fact resolves only these.
    pub issue_ids: BTreeSet<String>,
}

/// One acceptance row as the function reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcRow {
    pub ac: String,
    pub candidate: String,
    pub status: Row,
    /// A FAIL row's failing tests: its `failing`, else the TEST its reason names.
    pub failing: BTreeSet<String>,
    /// Under its candidate's own specification (the rows `satisfied` projects).
    pub own_spec: bool,
    /// Its requirement may degrade.
    pub degradable: bool,
}

/// What resolves an Issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subject {
    /// A finding's or a change request's: the observation facts of its fingerprint.
    Observed { fingerprint: String },
    /// An acceptance failure's (AC `ac`, its failing test when the item named one): the rows.
    Acceptance { ac: String, test: Option<String> },
}

/// An Issue as the function reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Issue {
    pub id: String,
    pub candidate: String,
    pub subject: Subject,
}

/// The persistent facts of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct World {
    /// Each candidate's freeze order (its first `candidate.frozen`); a candidate not in it is later than none.
    pub order: BTreeMap<String, i64>,
    /// The candidates with an INVALID evidence.
    pub vetoed: BTreeSet<String>,
    pub facts: Vec<Fact>,
    pub rows: Vec<AcRow>,
}

/// Whether an Issue is resolved, and on what: the fact or the rows it rests on, or what keeps it unresolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judged {
    Resolved { basis: String },
    Unresolved { basis: Option<String> },
}

impl Judged {
    pub fn resolved(&self) -> bool {
        matches!(self, Judged::Resolved { .. })
    }
}

impl World {
    /// Candidate `a` was frozen after candidate `b` (both known to the run's freezes).
    fn after(&self, a: &str, b: &str) -> bool {
        matches!((self.order.get(a), self.order.get(b)), (Some(x), Some(y)) if x > y)
    }

    /// Candidate `a` was frozen no earlier than candidate `b`: the same candidate, or a later one.
    fn at_or_after(&self, a: &str, b: &str) -> bool {
        a == b || self.after(a, b)
    }

    /// The latest of `candidates` by freeze, ties (and unknown candidates) by id.
    fn latest<'a>(&self, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
        candidates.max_by_key(|c| (self.order.get(*c).copied(), *c))
    }
}

/// Whether `issue` is resolved in `world` (decision 10's state function, v8): a finding's or a change request's Issue
/// by an absent fact naming it on a later candidate that is not vetoed, with no present fact of its fingerprint on that
/// candidate or a later one (present first on one candidate); an acceptance failure's by the rows of a later candidate
/// that is not vetoed and satisfies its AC (`classify::project`), with no FAIL row of its test on that candidate or a
/// later one. The basis is the latest such fact or candidate; unresolved, what keeps it so (the latest).
pub fn judge(issue: &Issue, world: &World) -> Judged {
    match &issue.subject {
        Subject::Observed { fingerprint } => observed(issue, fingerprint, world),
        Subject::Acceptance { ac, test } => accepted(issue, ac, test.as_deref(), world),
    }
}

fn observed(issue: &Issue, fingerprint: &str, w: &World) -> Judged {
    let presents: Vec<&Fact> = w
        .facts
        .iter()
        .filter(|f| f.polarity == Polarity::Present && f.fingerprint == fingerprint)
        .collect();
    let naming: Vec<&Fact> = w
        .facts
        .iter()
        .filter(|f| f.polarity == Polarity::Absent && f.issue_ids.contains(&issue.id))
        .filter(|a| w.after(&a.candidate, &issue.candidate))
        .collect();
    let defeaters = |a: &Fact| -> Vec<&Fact> {
        presents
            .iter()
            .copied()
            .filter(|n| w.at_or_after(&n.candidate, &a.candidate))
            .collect()
    };
    let resolving = naming
        .iter()
        .copied()
        .filter(|a| !w.vetoed.contains(&a.candidate) && defeaters(a).is_empty())
        .max_by_key(|a| (w.order.get(&a.candidate).copied(), a.id.clone()));
    if let Some(a) = resolving {
        return Judged::Resolved {
            basis: a.id.clone(),
        };
    }
    // What keeps it unresolved: the latest present fact that defeats an absent one naming it, else the latest veto of
    // a candidate that confirmed it.
    let defeater = naming
        .iter()
        .flat_map(|a| defeaters(a))
        .max_by_key(|n| (w.order.get(&n.candidate).copied(), n.id.clone()));
    let basis = match defeater {
        Some(n) => Some(n.id.clone()),
        None => w
            .latest(
                naming
                    .iter()
                    .filter(|a| w.vetoed.contains(&a.candidate))
                    .map(|a| a.candidate.as_str()),
            )
            .map(|c| format!("vetoed/{c}")),
    };
    Judged::Unresolved { basis }
}

fn accepted(issue: &Issue, ac: &str, test: Option<&str>, w: &World) -> Judged {
    let rows: Vec<&AcRow> = w.rows.iter().filter(|r| r.ac == ac).collect();
    let fails = |c: &str| {
        rows.iter().any(|r| {
            r.candidate == c && r.status == Row::Fail && test.is_none_or(|t| r.failing.contains(t))
        })
    };
    let satisfied = |c: &str| {
        let own: Vec<&&AcRow> = rows
            .iter()
            .filter(|r| r.candidate == c && r.own_spec)
            .collect();
        let statuses: Vec<Row> = own.iter().map(|r| r.status).collect();
        let degradable = own.iter().any(|r| r.degradable);
        !statuses.is_empty() && classify::project(&statuses, degradable) == classify::Ac::Satisfied
    };
    let candidates: BTreeSet<&str> = rows.iter().map(|r| r.candidate.as_str()).collect();
    let failing_later = |c: &str| candidates.iter().any(|f| w.at_or_after(f, c) && fails(f));
    let later: Vec<&str> = candidates
        .iter()
        .copied()
        .filter(|c| w.after(c, &issue.candidate) && satisfied(c))
        .collect();
    if let Some(c) = w.latest(
        later
            .iter()
            .copied()
            .filter(|c| !w.vetoed.contains(*c) && !failing_later(c)),
    ) {
        return Judged::Resolved {
            basis: format!("rows/{c}/{ac}"),
        };
    }
    let basis = match w.latest(
        candidates
            .iter()
            .copied()
            .filter(|f| w.after(f, &issue.candidate) && fails(f)),
    ) {
        Some(f) => Some(format!("fails/{f}/{ac}")),
        None => w
            .latest(later.iter().copied().filter(|c| w.vetoed.contains(*c)))
            .map(|c| format!("vetoed/{c}")),
    };
    Judged::Unresolved { basis }
}

/// The one state change that materialises `judged` on an Issue now in `state` (OPEN, RESOLVED or REOPENED), whose last
/// resolution rested on `last_resolved` (none when it never was).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// RESOLVED or REOPENED.
    pub to: &'static str,
    /// `issue.resolved` or `issue.reopened`.
    pub kind: &'static str,
    /// The suffix of its event id under the Issue: one per (Issue, basis), so a replay writes nothing new.
    pub key: String,
    pub basis: String,
    /// A reopening's: the resolution it undoes.
    pub undoes: Option<String>,
}

/// [`Transition`] of an Issue in `state` judged `judged`; none when its state already says it.
pub fn transition(state: &str, judged: &Judged, last_resolved: Option<&str>) -> Option<Transition> {
    match (state, judged) {
        ("OPEN" | "REOPENED", Judged::Resolved { basis }) => Some(Transition {
            to: "RESOLVED",
            kind: "issue.resolved",
            key: format!("resolved/{basis}"),
            basis: basis.clone(),
            undoes: None,
        }),
        // A resolution once undone never rests on the same fact again (facts, rows and vetoes only grow), so the
        // resolution it undoes names the reopening.
        ("RESOLVED", Judged::Unresolved { basis }) => {
            let undoes = last_resolved.unwrap_or("state").to_owned();
            Some(Transition {
                to: "REOPENED",
                kind: "issue.reopened",
                key: format!("reopened/{undoes}"),
                basis: basis.clone().unwrap_or_else(|| "unknown".to_owned()),
                undoes: Some(undoes),
            })
        }
        _ => None,
    }
}

/// The failing tests of a FAIL row: its `failing`, else the TEST its reason names (`<TEST> in check <check>…`).
pub fn failing_tests(failing: &[String], reason: &str) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = failing.iter().cloned().collect();
    let test = reason.split(" in check").next().unwrap_or_default();
    if names.is_empty() && !test.is_empty() {
        names.insert(test.to_owned());
    }
    names
}

#[cfg(test)]
mod tests;
