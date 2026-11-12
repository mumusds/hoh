//! The one classification of a verification round and of a candidate's join (docs/adr/ADR-028.md decision 11, I5): a
//! total function of AC-054-05's four dimensions (QA, Review, the deterministic checks, the AC coverage) plus the
//! issues that fail a round. Pure; the reconcile and M2-T18's join both call it.

/// A verifier's report on its current round: PASS, FAIL, UNKNOWN, or no valid report (NOT_RUN).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    Pass,
    Fail,
    Unknown,
    NotRun,
}

/// The check-level state of a round's evidence (docs/adr/ADR-027.md principle one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    /// Every check valid and PASS, or a compliant DEGRADED on a check the policy marks degradable.
    Passed,
    /// A conclusive FAIL (its reason not one of the inconclusive set).
    Failed,
    /// Nothing ran, or only inconclusive outcomes (timed out, not started, interrupted, incomplete, plugins missing).
    Inconclusive,
}

/// An `acceptance_results.status` (contracts/sql/001_initial.sql).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Pass,
    Degraded,
    Fail,
    Blocked,
    Flaky,
    NotRun,
    Invalid,
    Unknown,
    Stale,
}

/// One in-scope AC's projection over its rows on the candidate (the same rule as M2-T18's, AC-053-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ac {
    /// Only PASS and, on a degradable requirement, compliant DEGRADED.
    Satisfied,
    /// A FAIL row (with or without PASS), or DEGRADED where the requirement is not degradable.
    Failed,
    /// A FLAKY row, or UNKNOWN, BLOCKED or INVALID with a PASS: retesting the same candidate cannot satisfy it
    /// (REQ-053 decision 1).
    Flaky,
    /// No observation: no row, or only NOT_RUN, UNKNOWN, BLOCKED, STALE or INVALID (an empty observation's
    /// INVALID(INSUFFICIENT); a vetoed evidence's comes with the candidate INVALID, handled before classifying).
    Undetermined,
}

/// The projection of `rows` for an AC whose requirement is degradable or not.
pub fn project(rows: &[Row], degradable: bool) -> Ac {
    let has = |r: Row| rows.contains(&r);
    if has(Row::Fail) || (has(Row::Degraded) && !degradable) {
        return Ac::Failed;
    }
    let passed = has(Row::Pass) || has(Row::Degraded);
    if has(Row::Flaky) || (passed && (has(Row::Unknown) || has(Row::Blocked) || has(Row::Invalid)))
    {
        return Ac::Flaky;
    }
    if passed {
        // PASS and compliant DEGRADED, with only NOT_RUN or STALE beside them: an earlier run that observed nothing
        // does not undo a later one that observed the criterion met.
        return Ac::Satisfied;
    }
    Ac::Undetermined
}

/// What a round, or a candidate's join, is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Failed,
    Flaky,
    Inconclusive,
    Passable,
}

/// The inputs of one classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub qa: Report,
    pub review: Report,
    pub checks: Checks,
    /// Each in-scope AC's projection; an empty scope is never passable (REQ-053 behaviour 8).
    pub acs: Vec<Ac>,
    /// A change request of this round or a critical/high OPEN issue on the candidate (docs/adr/ADR-028.md decision 13).
    pub failing_issues: bool,
}

/// The class of `input`, by priority: failed, flaky, passable (a positive condition), else inconclusive.
pub fn classify(input: &Input) -> Class {
    let failed = input.qa == Report::Fail
        || input.review == Report::Fail
        || input.checks == Checks::Failed
        || input.failing_issues
        || input.acs.contains(&Ac::Failed);
    if failed {
        return Class::Failed;
    }
    if input.acs.contains(&Ac::Flaky) {
        return Class::Flaky;
    }
    let passable = input.qa == Report::Pass
        && input.review == Report::Pass
        && input.checks == Checks::Passed
        && !input.acs.is_empty()
        && input.acs.iter().all(|a| *a == Ac::Satisfied);
    if passable {
        Class::Passable
    } else {
        Class::Inconclusive
    }
}

#[cfg(test)]
mod tests;
