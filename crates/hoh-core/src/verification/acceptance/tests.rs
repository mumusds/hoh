use super::*;
use crate::verification::classify::{Ac, project};

const ALL: [Row; 9] = [
    Row::Pass,
    Row::Degraded,
    Row::Fail,
    Row::Blocked,
    Row::Flaky,
    Row::NotRun,
    Row::Invalid,
    Row::Unknown,
    Row::Stale,
];

fn observed(rows: &[Row], degradable: bool, unobserved_claim: bool) -> Observation {
    Observation {
        rows: rows.iter().copied().map(RowFact::of).collect(),
        degradable,
        unobserved_claim,
    }
}

fn label(rows: &[Row]) -> Label {
    display(Standing::Usable, &observed(rows, false, false), None).label
}

#[test]
fn test_053_02_the_eight_sequences_project_as_specified() {
    // AC-053-02 (docs/adr/ADR-029.md decisions 2 and 3): the eight sequences of one (spec, candidate, AC), the last an
    // empty-observation PASS claim, project as specified, and only [PASS] projects as PASS.
    use Row::*;
    let cases: [(&[Row], bool, Label); 8] = [
        (&[Fail, Pass], false, Label::Flaky),
        (&[Unknown, Pass], false, Label::Flaky),
        (&[Fail], false, Label::Fail),
        (&[Unknown], false, Label::Unknown),
        (&[], false, Label::NotRun),
        (&[NotRun], false, Label::NotRun),
        (&[Pass], false, Label::Pass),
        (&[], true, Label::Invalid),
    ];
    let mut table = Vec::new();
    for (rows, claim, want) in cases {
        let shown = display(Standing::Usable, &observed(rows, false, claim), None);
        assert_eq!(shown.label, want, "{rows:?} claim {claim}");
        table.push(serde_json::json!({
            "rows": rows.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>(),
            "unobserved_claim": claim,
            "label": shown.label.code(),
            "why": shown.why.map(|w| format!("{w:?}")),
        }));
    }
    let passes = table.iter().filter(|r| r["label"] == "PASS").count();
    assert_eq!(passes, 1, "only [PASS] is PASS");
    assert_eq!(
        display(Standing::Usable, &observed(&[], false, true), None).why,
        Some(Why::Insufficient)
    );
    println!("{}", serde_json::Value::Array(table));
}

/// Every multiset of at most three rows.
fn multisets() -> Vec<Vec<Row>> {
    let mut out = vec![Vec::new()];
    for (a, first) in ALL.iter().enumerate() {
        out.push(vec![*first]);
        for (b, second) in ALL.iter().enumerate().skip(a) {
            out.push(vec![*first, *second]);
            for third in ALL.iter().skip(b) {
                out.push(vec![*first, *second, *third]);
            }
        }
    }
    out
}

#[test]
fn the_display_and_the_decision_projection_satisfy_the_same_acs() {
    // J2 (docs/adr/ADR-029.md decision 2, CR-018, ADR-028 -956): for every multiset, degradability and claim, on a
    // usable candidate without a profile, the display label is satisfied exactly when the decision projection is
    // Satisfied; and the label depends on the multiset, not the order of the rows.
    for rows in multisets() {
        for degradable in [false, true] {
            for claim in [false, true] {
                let shown = display(Standing::Usable, &observed(&rows, degradable, claim), None);
                assert_eq!(
                    shown.label.satisfied(),
                    project(&rows, degradable) == Ac::Satisfied,
                    "{rows:?} degradable {degradable} claim {claim}: {shown:?}"
                );
                let mut reversed = rows.clone();
                reversed.reverse();
                assert_eq!(
                    display(
                        Standing::Usable,
                        &observed(&reversed, degradable, claim),
                        None
                    ),
                    shown,
                    "{rows:?} order"
                );
            }
        }
    }
}

#[test]
fn the_label_order_is_flaky_fail_unknown_blocked_invalid_stale_not_run_degraded_pass() {
    // docs/adr/ADR-029.md decision 2: the first that holds; a DEGRADED row of a requirement that may not degrade is a
    // FAIL row.
    use Row::*;
    assert_eq!(
        label(&[Blocked, Degraded]),
        Label::Fail,
        "DEGRADED not degradable is FAIL"
    );
    assert_eq!(label(&[Flaky]), Label::Flaky);
    assert_eq!(label(&[Blocked, Pass]), Label::Flaky);
    assert_eq!(label(&[Fail, Unknown]), Label::Fail);
    assert_eq!(label(&[Unknown, Blocked]), Label::Unknown);
    assert_eq!(label(&[Blocked, Invalid]), Label::Blocked);
    assert_eq!(label(&[Invalid, Pass]), Label::Invalid);
    assert_eq!(label(&[Stale, Pass]), Label::Stale);
    assert_eq!(label(&[NotRun, Pass]), Label::NotRun);
    assert_eq!(
        display(
            Standing::Usable,
            &observed(&[Pass, Degraded], true, false),
            None
        )
        .label,
        Label::Degraded
    );
    assert_eq!(label(&[Pass, Pass]), Label::Pass);
    // A claim never masks a row: a later PASS is PASS (decision 3).
    assert_eq!(
        display(Standing::Usable, &observed(&[Pass], false, true), None).label,
        Label::Pass
    );
}

#[test]
fn an_invalid_or_stale_candidate_satisfies_nothing() {
    // docs/adr/ADR-029.md decision 2 (R0): the standing decides before the rows.
    let rows = observed(&[Row::Pass], false, false);
    assert_eq!(
        display(Standing::Invalid, &rows, None),
        Shown {
            label: Label::Invalid,
            why: Some(Why::CandidateInvalid)
        }
    );
    assert_eq!(
        display(Standing::Stale, &rows, None),
        Shown {
            label: Label::Stale,
            why: Some(Why::CandidateStale)
        }
    );
}

#[test]
fn rule_16_drops_only_pass_rows_without_the_profiles_live_agents() {
    // docs/adr/ADR-029.md decision 14 (-155): with a profile requiring claude on adapter-live, a PASS row of that check
    // whose evidence has no claude session concludes nothing; a DEGRADED row is untouched; without a profile nothing
    // changes; a profile only makes the projection stricter.
    let profile = Profile {
        live_agents: [(
            "adapter-live".to_owned(),
            ["claude".to_owned()].into_iter().collect(),
        )]
        .into_iter()
        .collect(),
    };
    let row = |status: Row, agents: &[&str]| RowFact {
        status,
        check_id: Some("adapter-live".to_owned()),
        live_agents: Some(agents.iter().map(|a| (*a).to_owned()).collect()),
    };
    let of = |rows: Vec<RowFact>, degradable: bool| Observation {
        rows,
        degradable,
        unobserved_claim: false,
    };
    let missing = of(vec![row(Row::Pass, &["codex"])], false);
    assert_eq!(
        display(Standing::Usable, &missing, Some(&profile)).label,
        Label::NotRun
    );
    assert_eq!(display(Standing::Usable, &missing, None).label, Label::Pass);
    let present = of(vec![row(Row::Pass, &["claude", "codex"])], false);
    assert_eq!(
        display(Standing::Usable, &present, Some(&profile)).label,
        Label::Pass
    );
    let degraded = of(vec![row(Row::Degraded, &[])], true);
    assert_eq!(
        display(Standing::Usable, &degraded, Some(&profile)).label,
        Label::Degraded
    );
    for rows in multisets() {
        for degradable in [false, true] {
            let o = Observation {
                rows: rows.iter().map(|s| row(*s, &[])).collect(),
                degradable,
                unobserved_claim: false,
            };
            if display(Standing::Usable, &o, Some(&profile))
                .label
                .satisfied()
            {
                assert!(
                    display(Standing::Usable, &o, None).label.satisfied(),
                    "{rows:?}"
                );
            }
        }
    }
}

fn run(check: &str, outcome: &str, reason: Option<&str>, fail_lines: bool) -> CheckRun {
    CheckRun {
        check_id: check.to_owned(),
        outcome: outcome.to_owned(),
        reason: reason.map(str::to_owned),
        fail_lines,
    }
}

#[test]
fn a_checks_label_aggregates_its_history() {
    // docs/adr/ADR-029.md decision 4 (ADR-027:170, -51): INVALID first; a conclusive FAIL (a reason outside the
    // inconclusive set, none, or FAIL result lines) beside PASS/DEGRADED is FLAKY; then FAIL, DEGRADED, PASS, BLOCKED;
    // inconclusive FAILs alone are UNKNOWN; nothing is NOT_RUN. A rerun after a timeout that passes is PASS.
    let label = |runs: &[CheckRun]| check_label(&runs.iter().collect::<Vec<_>>());
    assert_eq!(label(&[]), Label::NotRun);
    assert_eq!(
        label(&[
            run("u", "PASS", None, false),
            run("u", "INVALID", None, false)
        ]),
        Label::Invalid
    );
    assert_eq!(
        label(&[
            run("u", "FAIL", Some("TIMED_OUT"), false),
            run("u", "PASS", None, false)
        ]),
        Label::Pass
    );
    assert_eq!(
        label(&[
            run("u", "FAIL", Some("TIMED_OUT"), true),
            run("u", "PASS", None, false)
        ]),
        Label::Flaky,
        "FAIL lines before a timeout are a conclusive FAIL"
    );
    assert_eq!(
        label(&[run("u", "FAIL", None, false), run("u", "PASS", None, false)]),
        Label::Flaky
    );
    assert_eq!(label(&[run("u", "FAIL", None, false)]), Label::Fail);
    assert_eq!(
        label(&[
            run("u", "PASS", None, false),
            run("u", "DEGRADED", None, false)
        ]),
        Label::Degraded
    );
    assert_eq!(label(&[run("u", "BLOCKED", None, false)]), Label::Blocked);
    assert_eq!(
        label(&[run("u", "FAIL", Some("INTERRUPTED"), false)]),
        Label::Unknown
    );
    // The label depends on the set of runs, not their order.
    let mixed = [
        run("u", "FAIL", Some("NOT_STARTED"), false),
        run("u", "BLOCKED", None, false),
        run("u", "PASS", None, false),
    ];
    for i in 0..mixed.len() {
        let mut rotated = mixed.to_vec();
        rotated.rotate_left(i);
        assert_eq!(label(&rotated), Label::Pass, "{rotated:?}");
    }
}

#[test]
fn the_check_dimension_and_its_failures_come_from_one_judgement() {
    // docs/adr/ADR-029.md decision 4 (-100, -151, -59, -164): Failed exactly when some check is FAIL or FLAKY, and
    // those checks are the failures with their reasons (FAIL_LINES when only the result lines failed it); Passed
    // when every check is PASS or DEGRADED; else (an empty list included) Inconclusive.
    assert_eq!(checks(&[]), Checks::Inconclusive);
    assert_eq!(
        checks(&[
            run("u", "FAIL", Some("TIMED_OUT"), false),
            run("i", "BLOCKED", None, false)
        ]),
        Checks::Inconclusive
    );
    assert_eq!(checks(&[run("u", "PASS", None, false)]), Checks::Passed);
    assert_eq!(
        checks(&[run("u", "PASS", None, false), run("i", "FAIL", None, false)]),
        Checks::Failed
    );
    assert_eq!(
        checks(&[
            run("u", "PASS", None, false),
            run("i", "DEGRADED", None, false)
        ]),
        Checks::Passed
    );
    let runs = [
        run("u", "PASS", None, false),
        run("t", "FAIL", Some("TIMED_OUT"), true),
        run("b", "FAIL", Some("BUILD"), false),
        run("i", "FAIL", Some("INTERRUPTED"), false),
    ];
    assert_eq!(
        check_failures(&runs),
        [
            ("b".to_owned(), "BUILD".to_owned()),
            ("t".to_owned(), "FAIL_LINES".to_owned())
        ]
    );
    assert_eq!(checks(&runs), Checks::Failed);
    assert!(check_failures(&[run("i", "FAIL", Some("INTERRUPTED"), false)]).is_empty());
}

#[test]
fn a_fingerprint_survives_line_numbers_whitespace_case_and_candidates() {
    // REQ-054 decision 2, docs/adr/ADR-029.md decision 9: the same defect on another candidate, at other lines, is the
    // same fingerprint; severity and locator count; a change request's text without line numbers or hex keeps the
    // fingerprint it had.
    let cand = |c: char| format!("CAND-{}", c.to_string().repeat(64));
    let a = fingerprint(
        "high",
        &format!("src/a.rs:12 drops the order on {}", cand('a')),
        &["src/a.rs".to_owned()],
    );
    let b = fingerprint(
        "high",
        &format!("  SRC/a.rs:40-44:3   drops the ORDER on {} ", cand('b')),
        &["src/a.rs".to_owned(), "src/a.rs".to_owned()],
    );
    assert_eq!(a, b);
    let sha = |c: char| c.to_string().repeat(40);
    assert_eq!(
        fingerprint("low", &format!("commit {} broke it", sha('1')), &[]),
        fingerprint("low", &format!("commit {} broke it", sha('2')), &[])
    );
    assert_ne!(
        a,
        fingerprint(
            "critical",
            "src/a.rs:12 drops the order on CAND",
            &["src/a.rs".to_owned()]
        )
    );
    assert_ne!(
        a,
        fingerprint(
            "high",
            "src/a.rs:12 drops the order on CAND",
            &["src/b.rs".to_owned()]
        )
    );
    // Locator paths under .hoh/ (records, attempts) change with every candidate and are left out.
    assert_eq!(
        fingerprint(
            "high",
            "x",
            &["src/a.rs".to_owned(), ".hoh/evidence/C/unit/1/x".to_owned()]
        ),
        fingerprint("high", "x", &["src/a.rs".to_owned()])
    );
    // M2-T16's form: sha256 of the severity and the text with its whitespace collapsed and lowercased.
    for (text, collapsed) in [
        ("refuse the cache rewrite", "refuse the cache rewrite"),
        ("Keep   the API\nstable", "keep the api stable"),
    ] {
        assert_eq!(
            fingerprint("medium", text, &[]),
            crate::context::sha256_hex(format!("medium\n{collapsed}").as_bytes())
        );
    }
}
