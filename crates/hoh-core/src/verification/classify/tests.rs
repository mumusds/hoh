use super::*;

#[test]
fn an_acs_projection_covers_every_acceptance_status() {
    // docs/adr/ADR-028.md decision 11 (M2T16D-401, -451, -452): every history of rows has one projection.
    use Row::*;
    let cases: [(&[Row], bool, Ac); 19] = [
        (&[], false, Ac::Undetermined),
        (&[Pass], false, Ac::Satisfied),
        (&[Pass, Pass], false, Ac::Satisfied),
        (&[Degraded], true, Ac::Satisfied),
        (&[Pass, Degraded], true, Ac::Satisfied),
        (&[Degraded], false, Ac::Failed),
        (&[Fail], false, Ac::Failed),
        (&[Fail, Pass], false, Ac::Failed),
        (&[Pass, Fail], false, Ac::Failed),
        (&[Flaky], false, Ac::Flaky),
        (&[Unknown, Pass], false, Ac::Flaky),
        (&[Blocked, Pass], false, Ac::Flaky),
        (&[NotRun], false, Ac::Undetermined),
        (&[Unknown], false, Ac::Undetermined),
        (&[Blocked, Stale], false, Ac::Undetermined),
        (&[Invalid], false, Ac::Undetermined),
        // Any row beside PASS but compliant DEGRADED stays on the candidate and leaves the AC unsatisfiable there
        // (ADR-027 principle one, M2T16D-803).
        (&[NotRun, Pass], false, Ac::Flaky),
        (&[Stale, Pass], false, Ac::Flaky),
        // An empty observation's INVALID(INSUFFICIENT) stays on this candidate: like FLAKY, a later PASS cannot clear it.
        (&[Invalid, Pass], false, Ac::Flaky),
    ];
    for (rows, degradable, want) in cases {
        assert_eq!(
            project(rows, degradable),
            want,
            "{rows:?} degradable {degradable}"
        );
    }
}

#[test]
fn of_ac_054_05s_ninety_six_combinations_only_one_is_passable() {
    // AC-054-05's dimensions mapped (docs/adr/ADR-028.md decision 11): QA and Review are the current round's reports,
    // the deterministic checks one of three, the coverage every AC satisfied or one undetermined.
    let reports = [Report::Pass, Report::Fail, Report::Unknown, Report::NotRun];
    let checks = [Checks::Passed, Checks::Failed, Checks::Inconclusive];
    let mut passable = Vec::new();
    for qa in reports {
        for review in reports {
            for c in checks {
                for full in [true, false] {
                    let acs = if full {
                        vec![Ac::Satisfied, Ac::Satisfied]
                    } else {
                        vec![Ac::Satisfied, Ac::Undetermined]
                    };
                    let input = Input {
                        qa,
                        review,
                        checks: c,
                        acs,
                        failing_issues: false,
                        outside_failed: false,
                    };
                    let class = classify(&input);
                    if class == Class::Passable {
                        passable.push((qa, review, c, full));
                    }
                    // A FAIL anywhere fails; otherwise nothing short of all-pass is a failure (UNKNOWN never reworks).
                    let any_fail =
                        qa == Report::Fail || review == Report::Fail || c == Checks::Failed;
                    assert_eq!(class == Class::Failed, any_fail, "{input:?}");
                }
            }
        }
    }
    assert_eq!(
        passable,
        [(Report::Pass, Report::Pass, Checks::Passed, true)]
    );
}

#[test]
fn failed_comes_before_flaky_before_inconclusive_and_an_empty_scope_never_passes() {
    let all = |acs: Vec<Ac>, failing_issues: bool| Input {
        qa: Report::Pass,
        review: Report::Pass,
        checks: Checks::Passed,
        acs,
        failing_issues,
        outside_failed: false,
    };
    assert_eq!(classify(&all(vec![Ac::Satisfied], false)), Class::Passable);
    assert_eq!(classify(&all(vec![Ac::Satisfied], true)), Class::Failed);
    // An AC outside the scope failed by the round's evidence fails it, its own scope all satisfied (correction 6).
    let outside = Input {
        outside_failed: true,
        ..all(vec![Ac::Satisfied], false)
    };
    assert_eq!(classify(&outside), Class::Failed);
    assert_eq!(
        classify(&all(vec![Ac::Flaky, Ac::Failed], false)),
        Class::Failed
    );
    assert_eq!(
        classify(&all(vec![Ac::Flaky, Ac::Undetermined], false)),
        Class::Flaky
    );
    assert_eq!(
        classify(&all(vec![Ac::Undetermined], false)),
        Class::Inconclusive
    );
    assert_eq!(classify(&all(Vec::new(), false)), Class::Inconclusive);
    let unknown = Input {
        qa: Report::Unknown,
        ..all(vec![Ac::Satisfied], false)
    };
    assert_eq!(classify(&unknown), Class::Inconclusive);
}
