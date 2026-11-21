use super::*;

const SHA2: &str = "2222222222222222222222222222222222222222222222222222222222222222";
const SHA1: &str = "1111111111111111111111111111111111111111111111111111111111111111";

fn criterion(id: &str) -> Criterion {
    Criterion {
        id: id.to_owned(),
        then: Some("可观察的结果".to_owned()),
        check: Some("unit".to_owned()),
    }
}

fn requirement(id: &str, criteria: Vec<Criterion>) -> Requirement {
    Requirement {
        id: id.to_owned(),
        required: true,
        criteria,
    }
}

fn task(id: &str, requirements: &[&str], depends_on: &[&str]) -> PlanTask {
    PlanTask {
        task_id: id.to_owned(),
        requirement_ids: requirements.iter().map(|r| (*r).to_owned()).collect(),
        depends_on: depends_on.iter().map(|d| (*d).to_owned()).collect(),
    }
}

/// Revision 2: REQ-A and REQ-B, each with one observable criterion; a non-blocking open question.
fn complete() -> Revision {
    Revision {
        revision: 2,
        sha256: SHA2.to_owned(),
        requirements: vec![
            requirement("REQ-A", vec![criterion("AC-A-01")]),
            requirement("REQ-B", vec![criterion("AC-B-01")]),
        ],
        questions: vec![OpenQuestion {
            id: "Q-1".to_owned(),
            blocking: false,
        }],
    }
}

fn plan() -> Vec<PlanTask> {
    vec![
        task("T-A", &["REQ-A"], &[]),
        task("T-B", &["REQ-B"], &["T-A"]),
    ]
}

/// The drafter (codex, native N-draft) and the Commander (claude, native N-cmd).
fn authors() -> Identities {
    let mut ids = Identities::default();
    ids.add_hoh("S-draft");
    ids.add_native("codex", "N-draft");
    ids.add_hoh("S-cmd");
    ids.add_native("claude", "N-cmd");
    ids
}

/// A reviewer in HoH session `session` of `agent`, whose native attempt record's session_ids are `record` (its own
/// HoH id among them, excluded as REQ-031 behaviour 4 says).
fn reviewer(session: &str, agent: &str, record: &[&str]) -> Identities {
    let mut ids = Identities::default();
    ids.add_hoh(session);
    for id in record.iter().filter(|id| **id != session) {
        ids.claim(agent, id);
    }
    ids
}

fn pass(revision: i64, findings: &[(&str, Severity)], who: Identities) -> Review {
    Review {
        spec_revision: revision,
        spec_sha256: if revision == 2 { SHA2 } else { SHA1 }.to_owned(),
        state: ReviewState::Current,
        status: ReviewStatus::Pass,
        findings: findings
            .iter()
            .map(|(id, severity)| Finding {
                id: (*id).to_owned(),
                severity: *severity,
            })
            .collect(),
        reviewer: who,
    }
}

fn independent() -> Identities {
    reviewer("S-rev", "claude", &["S-rev", "N-rev"])
}

fn reviewed(latest: Option<Review>) -> ReviewView {
    ReviewView {
        latest,
        authors: authors(),
    }
}

fn independent_pass() -> ReviewView {
    reviewed(Some(pass(2, &[], independent())))
}

/// (code, ids, detail) of each reason.
fn summary(readiness: &Readiness) -> Vec<(&'static str, Vec<String>, Option<&'static str>)> {
    readiness
        .reasons()
        .iter()
        .map(|r| {
            (
                r.code.as_str(),
                r.ids.clone(),
                r.detail.map(ReviewDetail::as_str),
            )
        })
        .collect()
}

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn test_031_01_readiness_reasons_per_fixture() {
    // AC-031-01: five fixtures, each reviewed by an independent session's PASS of the same revision.
    let review = independent_pass();
    let mut missing_ac = complete();
    missing_ac.requirements[1].criteria.clear();
    let mut unobservable = complete();
    unobservable.requirements[1].criteria[0].then = None;
    let cyclic = vec![
        task("T-A", &["REQ-A"], &["T-B"]),
        task("T-B", &["REQ-B"], &["T-A"]),
    ];
    let mut undecided = complete();
    undecided.questions.push(OpenQuestion {
        id: "Q-2".to_owned(),
        blocking: true,
    });
    let fixtures: [(&str, Revision, Vec<PlanTask>, Readiness); 5] = [
        ("complete", complete(), plan(), Readiness::Ready),
        (
            "missing_ac",
            missing_ac,
            plan(),
            Readiness::NotReady(vec![Reason {
                code: Code::MissingAc,
                ids: ids(&["REQ-B"]),
                detail: None,
            }]),
        ),
        (
            "unobservable",
            unobservable,
            plan(),
            Readiness::NotReady(vec![Reason {
                code: Code::Unobservable,
                ids: ids(&["AC-B-01"]),
                detail: None,
            }]),
        ),
        (
            "DAG_CYCLE",
            complete(),
            cyclic,
            Readiness::NotReady(vec![Reason {
                code: Code::DagCycle,
                ids: ids(&["T-A", "T-B"]),
                detail: None,
            }]),
        ),
        (
            "undecided",
            undecided,
            plan(),
            Readiness::NotReady(vec![Reason {
                code: Code::Undecided,
                ids: ids(&["Q-2"]),
                detail: None,
            }]),
        ),
    ];
    for (name, revision, plan, expected) in fixtures {
        assert_eq!(judge_run(&revision, &plan, &review), expected, "{name}");
    }
}

#[test]
fn test_031_01_missing_check_and_blank_then_are_unobservable() {
    let review = independent_pass();
    let mut revision = complete();
    revision.requirements[0].criteria[0].check = None;
    revision.requirements[1].criteria[0].then = Some("  ".to_owned());
    assert_eq!(
        summary(&judge_run(&revision, &plan(), &review)),
        [("unobservable", ids(&["AC-A-01", "AC-B-01"]), None)]
    );
}

#[test]
fn test_031_01_degradable_requirement_without_criteria_is_not_missing_ac() {
    // missing_ac is about required requirements only.
    let review = independent_pass();
    let mut revision = complete();
    revision.requirements[1].criteria.clear();
    revision.requirements[1].required = false;
    assert_eq!(judge_run(&revision, &plan(), &review), Readiness::Ready);
}

#[test]
fn test_031_01_missing_dependency_names_the_missing_task() {
    let review = independent_pass();
    let plan = vec![
        task("T-A", &["REQ-A"], &["T-GONE"]),
        task("T-B", &["REQ-B"], &[]),
    ];
    assert_eq!(
        summary(&judge_run(&complete(), &plan, &review)),
        [("DAG_MISSING_DEPENDENCY", ids(&["T-GONE"]), None)]
    );
    // Only the task whose closure holds it is affected.
    assert!(!judge_task(&complete(), &plan, &review, "T-A").is_ready());
    assert_eq!(
        judge_task(&complete(), &plan, &review, "T-B"),
        Readiness::Ready
    );
}

#[test]
fn test_031_01_task_readiness_reads_only_its_dependency_closure() {
    // REQ-031 behaviour 5: a task's readiness is its requirements and those of its dependency closure.
    let review = independent_pass();
    let mut revision = complete();
    revision.requirements.push(requirement("REQ-C", Vec::new()));
    let tasks = vec![
        task("T-A", &["REQ-A"], &[]),
        task("T-B", &["REQ-B"], &["T-A"]),
        task("T-C", &["REQ-C"], &[]),
        task("T-D", &["REQ-A"], &["T-C"]),
        task("T-X", &["REQ-A"], &["T-Y"]),
        task("T-Y", &["REQ-A"], &["T-X"]),
        task("T-Z", &["REQ-UNKNOWN"], &[]),
    ];
    let judged = |id: &str| summary(&judge_task(&revision, &tasks, &review, id));
    assert_eq!(judged("T-A"), []);
    assert_eq!(judged("T-B"), []);
    assert_eq!(judged("T-C"), [("missing_ac", ids(&["REQ-C"]), None)]);
    assert_eq!(judged("T-D"), [("missing_ac", ids(&["REQ-C"]), None)]);
    assert_eq!(judged("T-X"), [("DAG_CYCLE", ids(&["T-X", "T-Y"]), None)]);
    // A requirement the revision does not have has no criterion in it.
    assert_eq!(judged("T-Z"), [("missing_ac", ids(&["REQ-UNKNOWN"]), None)]);
    // The run sees all of them, one reason per code.
    assert_eq!(
        summary(&judge_run(&revision, &tasks, &review)),
        [
            ("missing_ac", ids(&["REQ-C", "REQ-UNKNOWN"]), None),
            ("DAG_CYCLE", ids(&["T-X", "T-Y"]), None),
        ]
    );
    // Open questions and the review are about the whole revision: every task.
    let mut undecided = complete();
    undecided.questions[0].blocking = true;
    for id in ["T-A", "T-B"] {
        assert_eq!(
            summary(&judge_task(&undecided, &plan(), &reviewed(None), id)),
            [
                ("undecided", ids(&["Q-1"]), None),
                ("spec_review", Vec::new(), Some("missing")),
            ]
        );
    }
}

#[test]
fn test_031_01_reasons_serialise_as_task_blocked_details() {
    let reason = Reason {
        code: Code::SpecReview,
        ids: ids(&["F-1"]),
        detail: Some(ReviewDetail::BlockingFindings),
    };
    assert_eq!(
        reason.json(),
        json!({"reason": "spec_review", "ids": ["F-1"], "detail": "blocking_findings"})
    );
    let reason = Reason {
        code: Code::MissingAc,
        ids: ids(&["REQ-B1"]),
        detail: None,
    };
    assert_eq!(
        reason.json(),
        json!({"reason": "missing_ac", "ids": ["REQ-B1"]})
    );
}

/// A review record of AC-031-05: its name, the latest report (none for none) and the `spec_review` detail and finding
/// ids it is judged to (none for READY).
type Record = (
    &'static str,
    Option<Review>,
    Option<(&'static str, Vec<String>)>,
);

#[test]
fn test_031_05_spec_review_records_judged() {
    // AC-031-05: revision 2 is complete; eleven review records, each judged once.
    let mut fail = pass(2, &[], independent());
    fail.status = ReviewStatus::Fail;
    let mut unknown = pass(2, &[], independent());
    unknown.status = ReviewStatus::Unknown;
    let records: [Record; 11] = [
        ("independent PASS", Some(pass(2, &[], independent())), None),
        ("FAIL", Some(fail), Some(("fail", Vec::new()))),
        ("UNKNOWN", Some(unknown), Some(("unknown", Vec::new()))),
        ("no record", None, Some(("missing", Vec::new()))),
        (
            "the drafting session's PASS",
            Some(pass(
                2,
                &[],
                reviewer("S-draft", "codex", &["S-draft", "N-draft"]),
            )),
            Some(("not_independent", Vec::new())),
        ),
        (
            "a PASS of revision 1",
            Some(pass(1, &[], independent())),
            Some(("stale_revision", Vec::new())),
        ),
        (
            "a PASS with a high finding",
            Some(pass(
                2,
                &[("F-7", Severity::High), ("F-8", Severity::Low)],
                independent(),
            )),
            Some(("blocking_findings", ids(&["F-7"]))),
        ),
        (
            "a PASS with a critical finding",
            Some(pass(
                2,
                &[("F-9", Severity::Medium), ("F-10", Severity::Critical)],
                independent(),
            )),
            Some(("blocking_findings", ids(&["F-10"]))),
        ),
        (
            "a PASS with medium and low findings",
            Some(pass(
                2,
                &[("F-11", Severity::Medium), ("F-12", Severity::Low)],
                independent(),
            )),
            None,
        ),
        (
            "another HoH session sharing the drafter's (agent, native id)",
            Some(pass(
                2,
                &[],
                reviewer("S-other", "codex", &["S-other", "N-draft"]),
            )),
            Some(("not_independent", Vec::new())),
        ),
        (
            "the drafter's native id string under another agent",
            Some(pass(
                2,
                &[],
                reviewer("S-third", "claude", &["S-third", "N-draft"]),
            )),
            None,
        ),
    ];
    for (name, latest, expected) in records {
        let judged = judge_run(&complete(), &plan(), &reviewed(latest));
        match expected {
            None => assert_eq!(judged, Readiness::Ready, "{name}"),
            Some((detail, finding_ids)) => assert_eq!(
                summary(&judged),
                [("spec_review", finding_ids, Some(detail))],
                "{name}"
            ),
        }
    }
}

#[test]
fn test_031_05_rejected_and_mismatched_reports_do_not_satisfy() {
    // REQ-031 behaviour 3: REJECTED, or a report of another digest of the same revision number, is no PASS.
    let mut rejected = pass(2, &[], independent());
    rejected.state = ReviewState::Rejected {
        reasons: vec!["git_commit_mismatch: the native attempt names another commit".to_owned()],
    };
    let mut not_independent = pass(2, &[], independent());
    not_independent.state = ReviewState::Rejected {
        reasons: vec!["not_independent: shares HoH session S-cmd".to_owned()],
    };
    let mut other_digest = pass(2, &[], independent());
    other_digest.spec_sha256 = SHA1.to_owned();
    // A Commander's session reviewing is no more independent than a drafter's.
    let commander = pass(2, &[], reviewer("S-x", "claude", &["S-x", "N-cmd"]));
    for (name, review, detail) in [
        ("rejected", rejected, "rejected"),
        (
            "rejected as not independent",
            not_independent,
            "not_independent",
        ),
        ("another digest", other_digest, "stale_revision"),
        (
            "the Commander's native session",
            commander,
            "not_independent",
        ),
    ] {
        assert_eq!(
            judge_review(&complete(), &reviewed(Some(review))).map(|r| r.detail),
            Some(Some(match detail {
                "rejected" => ReviewDetail::Rejected,
                "not_independent" => ReviewDetail::NotIndependent,
                _ => ReviewDetail::StaleRevision,
            })),
            "{name}"
        );
    }
}
