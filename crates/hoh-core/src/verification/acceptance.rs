//! The acceptance projection for display and counting, the check-level aggregation over a candidate's history and the
//! normalisation of issue fingerprints (docs/adr/ADR-029.md decisions 2, 4, 9 and 14). Pure; the store, the join and
//! the snapshot call it.

use super::classify::{Checks, Row};
use std::collections::{BTreeMap, BTreeSet};

/// The closed set of display labels (docs/adr/ADR-029.md decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Label {
    Pass,
    Degraded,
    Flaky,
    Fail,
    Unknown,
    Blocked,
    Invalid,
    Stale,
    NotRun,
}

impl Label {
    /// The label as `acceptance_results.status` spells it.
    pub fn code(self) -> &'static str {
        match self {
            Label::Pass => "PASS",
            Label::Degraded => "DEGRADED",
            Label::Flaky => "FLAKY",
            Label::Fail => "FAIL",
            Label::Unknown => "UNKNOWN",
            Label::Blocked => "BLOCKED",
            Label::Invalid => "INVALID",
            Label::Stale => "STALE",
            Label::NotRun => "NOT_RUN",
        }
    }

    /// Whether the label satisfies its AC: PASS, or compliant DEGRADED on a degradable requirement.
    pub fn satisfied(self) -> bool {
        matches!(self, Label::Pass | Label::Degraded)
    }
}

/// Why a label is what it is, when the rows alone do not say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// A verifier's completed report claimed the AC without any observation (REQ-053 failure mode 2).
    Insufficient,
    /// The candidate is INVALID or vetoed (it has an INVALID evidence).
    CandidateInvalid,
    /// The candidate is STALE.
    CandidateStale,
}

/// What the candidate's state lets the rows say (docs/adr/ADR-029.md decision 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Standing {
    /// FROZEN, VERIFYING, ACCEPTED or REJECTED, and not vetoed: the rows decide.
    Usable,
    /// INVALID, or with an INVALID evidence: no AC is satisfied.
    Invalid,
    /// STALE: no AC is satisfied.
    Stale,
}

/// One acceptance row as the projection needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowFact {
    pub status: Row,
    /// The row's evidence's check.
    pub check_id: Option<String>,
    /// The agents of the row's evidence's live sessions (rule 16); none when it has none.
    pub live_agents: Option<BTreeSet<String>>,
}

impl RowFact {
    /// A row of `status` with nothing else known.
    pub fn of(status: Row) -> RowFact {
        RowFact {
            status,
            check_id: None,
            live_agents: None,
        }
    }
}

/// The rows of one (spec, candidate, AC), whether its requirement may degrade, and whether a verifier's completed
/// report claimed it without any observation (docs/adr/ADR-029.md decision 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    pub rows: Vec<RowFact>,
    pub degradable: bool,
    pub unobserved_claim: bool,
}

/// A release profile's live agents per check (principle one's rule 16; docs/adr/ADR-029.md decision 14).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub live_agents: BTreeMap<String, BTreeSet<String>>,
}

/// A display label and its reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub label: Label,
    pub why: Option<Why>,
}

/// The display projection of observation `o` on a candidate of standing `standing`, under `profile` when given
/// (docs/adr/ADR-029.md decision 2).
pub fn display(standing: Standing, o: &Observation, profile: Option<&Profile>) -> Shown {
    let of = |label, why| Shown { label, why };
    match standing {
        Standing::Invalid => return of(Label::Invalid, Some(Why::CandidateInvalid)),
        Standing::Stale => return of(Label::Stale, Some(Why::CandidateStale)),
        Standing::Usable => {}
    }
    // Rule 16 under a profile: a PASS row of a check the profile names whose evidence lacks one of its live agents
    // concludes nothing (decision 14); DEGRADED rows are untouched.
    let kept = o.rows.iter().filter(|r| {
        let wanted = r
            .check_id
            .as_ref()
            .and_then(|c| profile.and_then(|p| p.live_agents.get(c)));
        !(r.status == Row::Pass
            && wanted
                .is_some_and(|w| !r.live_agents.as_ref().is_some_and(|have| w.is_subset(have))))
    });
    // A DEGRADED row of a requirement that may not degrade is a FAIL row.
    let rows: Vec<Row> = kept
        .map(|r| match r.status {
            Row::Degraded if !o.degradable => Row::Fail,
            s => s,
        })
        .collect();
    let has = |r: Row| rows.contains(&r);
    let good = has(Row::Pass) || has(Row::Degraded);
    let label = if has(Row::Flaky)
        || (good && (has(Row::Fail) || has(Row::Unknown) || has(Row::Blocked)))
    {
        Label::Flaky
    } else if has(Row::Fail) {
        Label::Fail
    } else if has(Row::Unknown) {
        Label::Unknown
    } else if has(Row::Blocked) {
        Label::Blocked
    } else if has(Row::Invalid) {
        Label::Invalid
    } else if has(Row::Stale) {
        Label::Stale
    } else if rows.is_empty() && o.unobserved_claim {
        return of(Label::Invalid, Some(Why::Insufficient));
    } else if rows.is_empty() || has(Row::NotRun) {
        Label::NotRun
    } else if has(Row::Degraded) {
        Label::Degraded
    } else {
        Label::Pass
    };
    of(label, None)
}

/// One `kind = 'check'` evidence of a check on a candidate (docs/adr/ADR-029.md decision 4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub check_id: String,
    /// `evidence.outcome`.
    pub outcome: String,
    /// The outcome's reason (`evidence.recorded`'s `outcome_reason`).
    pub reason: Option<String>,
    /// Whether its observations have a result line of status FAIL.
    pub fail_lines: bool,
}

/// The reasons of a FAIL outcome that conclude nothing (principle one; moved from the round close).
pub const INCONCLUSIVE: [&str; 5] = [
    "TIMED_OUT",
    "NOT_STARTED",
    "INTERRUPTED",
    "INCOMPLETE",
    "PLUGINS_MISSING",
];

/// The label of one check over its runs `runs` on a candidate (docs/adr/ADR-029.md decision 4).
pub fn check_label(runs: &[&CheckRun]) -> Label {
    let any = |outcome: &str| runs.iter().any(|r| r.outcome == outcome);
    let failed = runs.iter().any(|r| conclusive_fail(r));
    if runs.is_empty() {
        Label::NotRun
    } else if any("INVALID") {
        Label::Invalid
    } else if failed && (any("PASS") || any("DEGRADED")) {
        Label::Flaky
    } else if failed {
        Label::Fail
    } else if any("DEGRADED") {
        Label::Degraded
    } else if any("PASS") {
        Label::Pass
    } else if any("BLOCKED") {
        Label::Blocked
    } else {
        Label::Unknown
    }
}

/// Whether run `r` concludes a failure: FAIL result lines whatever its reason (ADR-027:170, -51), or a FAIL outcome
/// whose reason is none or outside the inconclusive set. A vetoed run concludes nothing here (its veto does).
fn conclusive_fail(r: &CheckRun) -> bool {
    r.outcome != "INVALID"
        && (r.fail_lines
            || (r.outcome == "FAIL"
                && !r
                    .reason
                    .as_deref()
                    .is_some_and(|reason| INCONCLUSIVE.contains(&reason))))
}

/// `runs` by check.
fn by_check(runs: &[CheckRun]) -> BTreeMap<&str, Vec<&CheckRun>> {
    let mut out: BTreeMap<&str, Vec<&CheckRun>> = BTreeMap::new();
    for r in runs {
        out.entry(r.check_id.as_str()).or_default().push(r);
    }
    out
}

/// The check dimension of `runs` (every check's runs together): Failed exactly when [`check_failures`] is not empty;
/// Passed when every check is PASS or DEGRADED; otherwise (no runs included) Inconclusive.
pub fn checks(runs: &[CheckRun]) -> Checks {
    let labels: Vec<Label> = by_check(runs).values().map(|r| check_label(r)).collect();
    if labels
        .iter()
        .any(|l| matches!(l, Label::Fail | Label::Flaky))
    {
        Checks::Failed
    } else if !labels.is_empty() && labels.iter().all(|l| l.satisfied()) {
        Checks::Passed
    } else {
        Checks::Inconclusive
    }
}

/// The failing checks of `runs` and each one's reason, sorted by check (docs/adr/ADR-029.md decision 4): the checks
/// labelled FAIL or FLAKY; the reason is its conclusive runs' reasons, sorted and joined by a comma, a run failed only
/// by its result lines giving `FAIL_LINES`. `checks` is Failed exactly when this is not empty.
pub fn check_failures(runs: &[CheckRun]) -> Vec<(String, String)> {
    by_check(runs)
        .into_iter()
        .filter(|(_, r)| matches!(check_label(r), Label::Fail | Label::Flaky))
        .map(|(check, r)| {
            let reasons: BTreeSet<String> = r
                .iter()
                .filter(|run| conclusive_fail(run))
                .map(|run| {
                    let outcome_concludes = run.outcome == "FAIL"
                        && !run
                            .reason
                            .as_deref()
                            .is_some_and(|reason| INCONCLUSIVE.contains(&reason));
                    if outcome_concludes {
                        run.reason.clone().unwrap_or_default()
                    } else {
                        "FAIL_LINES".to_owned()
                    }
                })
                .collect();
            (
                check.to_owned(),
                reasons.into_iter().collect::<Vec<_>>().join(","),
            )
        })
        .collect()
}

/// An issue's fingerprint (REQ-054 decision 2; docs/adr/ADR-029.md decision 9): sha256 of its severity, its text
/// normalised and, when given, its locator (finding evidence paths) normalised.
pub fn fingerprint(severity: &str, text: &str, locator: &[String]) -> String {
    let located: BTreeSet<String> = locator
        .iter()
        .filter(|p| !p.trim_start_matches("./").starts_with(".hoh/"))
        .map(|p| normalise(p))
        .filter(|p| !p.is_empty())
        .collect();
    let mut input = format!("{severity}\n{}", normalise(text));
    if !located.is_empty() {
        input.push('\n');
        input.push_str(&located.into_iter().collect::<Vec<_>>().join("\n"));
    }
    crate::context::sha256_hex(input.as_bytes())
}

/// `text` lowercased, a candidate id `cand-<64 hex>` as `cand`, a standalone 40- or 64-digit hex run as `<sha>`, a
/// line reference after a path-like token (`:12`, `:12-14`, `:12:3`) dropped, whitespace collapsed.
fn normalise(text: &str) -> String {
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let at_start = i == 0 || !word(chars[i - 1]);
        // cand-<64 hex>
        if at_start && chars[i..].starts_with(&['c', 'a', 'n', 'd', '-']) {
            let hex = hex_run(&chars, i + 5);
            if hex == 64 && !chars.get(i + 69).is_some_and(|c| word(*c)) {
                out.push_str("cand");
                i += 69;
                continue;
            }
        }
        if at_start {
            let hex = hex_run(&chars, i);
            if (hex == 40 || hex == 64) && !chars.get(i + hex).is_some_and(|c| word(*c)) {
                out.push_str("<sha>");
                i += hex;
                continue;
            }
        }
        // A line reference after a path-like token: `:digits[-digits][:digits]` not followed by a word character.
        if chars[i] == ':'
            && i > 0
            && path_like(&chars[..i])
            && let Some(end) = line_ref(&chars, i)
        {
            i = end;
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The length of the hex run starting at `i`.
fn hex_run(chars: &[char], i: usize) -> usize {
    chars[i.min(chars.len())..]
        .iter()
        .take_while(|c| c.is_ascii_hexdigit())
        .count()
}

/// Whether the token ending at the end of `before` looks like a path (has a `/` or a `.`).
fn path_like(before: &[char]) -> bool {
    let token: Vec<&char> = before
        .iter()
        .rev()
        .take_while(|c| !c.is_whitespace() && !matches!(c, '(' | '[' | '"' | '\'' | ','))
        .collect();
    !token.is_empty() && token.iter().any(|c| matches!(c, '/' | '.'))
}

/// The end of a line reference `:digits[-digits][:digits]` starting at the colon `i`, if there is one.
fn line_ref(chars: &[char], i: usize) -> Option<usize> {
    let digits = |from: usize| {
        chars[from.min(chars.len())..]
            .iter()
            .take_while(|c| c.is_ascii_digit())
            .count()
    };
    let mut j = i + 1;
    let n = digits(j);
    if n == 0 {
        return None;
    }
    j += n;
    if chars.get(j) == Some(&'-') && digits(j + 1) > 0 {
        j += 1 + digits(j + 1);
    }
    if chars.get(j) == Some(&':') && digits(j + 1) > 0 {
        j += 1 + digits(j + 1);
    }
    if chars
        .get(j)
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_')
    {
        return None;
    }
    Some(j)
}

#[cfg(test)]
mod tests;
