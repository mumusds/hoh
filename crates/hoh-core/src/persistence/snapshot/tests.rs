//! The acceptance snapshot, the two HTTP lists and the issue chain against a real database (docs/adr/ADR-029.md
//! decisions 3, 4, 5, 10 and 15; J2, J7): the latest candidate's labels and counts, a vetoed, INVALID or STALE
//! candidate, the counting set, the QA summary's downgrade, the unobserved claim and the reverification's context, each
//! candidate's projection and the Issues' states evaluated now.

use super::*;
use crate::ids::child_id;
use crate::persistence::Store;
use crate::persistence::completion::tests::{R, evidence_of, evidence_with, open, texts};
use crate::persistence::issues::tests::{
    finding, freeze_d, pass, reconcile, settle, start, state_of,
};
use crate::persistence::join::tests::{attempts_of, two_sources, verify};
use crate::verification::acceptance::{Label, Why};
use crate::verification::classify::Report;
use rusqlite::Connection;
use serde_json::json;
use std::path::Path;

/// Run R's snapshot.
fn snapshot(store: &Store) -> R<AcceptanceView> {
    Ok(store.reader()?.acceptance("R")?.ok_or("no run R")?)
}

/// Each AC of `view`: (AC, label, reason, counted).
fn shown(view: &AcceptanceView) -> Vec<(String, Label, Option<Why>, bool)> {
    view.acs
        .iter()
        .map(|a| (a.ac_id.clone(), a.label, a.why, a.counted))
        .collect()
}

/// `ac` shown with `label`, `why`, counted.
fn ac(ac: &str, label: Label, why: Option<Why>) -> (String, Label, Option<Why>, bool) {
    (ac.to_owned(), label, why, true)
}

/// (passing, accepted, total) of `view`.
fn counts(view: &AcceptanceView) -> (usize, usize, usize) {
    (view.passing, view.accepted, view.total)
}

/// D's C1 (tree b) verified passable (QA's PASS row of AC-1-01, both reports PASS) and joined: C1 and D ACCEPTED.
async fn accepted_c1(store: &Store, path: &Path) -> R<String> {
    let c1 = freeze_d(store, path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(store, path, &r1, &[]).await?;
    pass(path, &qa, &c1, "rec-1")?;
    settle(store, path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(store, path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    reconcile(store).await?;
    assert_eq!(texts(path, "SELECT state FROM candidates")?, ["ACCEPTED"]);
    Ok(c1)
}

/// The mandatory scope verifier attempt `attempt` of round `round`'s `role` is given.
fn scope_of(path: &Path, round: &str, attempt: &str, role: &str) -> R<Vec<String>> {
    let assigned = crate::persistence::team::assignment(
        &Connection::open(path)?,
        "p1",
        "R",
        &child_id(round, &format!("task/{role}")),
        attempt,
        role,
    )?;
    Ok(assigned.context.ok_or("a verifier has a context")?.scope)
}

/// QA's line of a reverification (docs/adr/ADR-029.md decision 4, -261).
const QA_RERUN: &str = "本轮须重跑范围内每个 AC 的场景检查：轮次只看本轮的证据";

#[tokio::test]
async fn the_snapshot_shows_the_latest_candidate_and_counts_an_ac_accepted_only_once_its_candidate_is()
-> R {
    // Decision 5 (AC-053-04): before any candidate every AC in the scope is NOT_RUN and nothing counts; on C1 under
    // verification QA's PASS while AC-1-01 has no row yet is UNKNOWN (-163) and the check PASS; once the round closes
    // and the join accepts C1, AC-1-01 is PASS from its row, counted passing and accepted.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let before = snapshot(&store)?;
    assert_eq!(before.candidate, None);
    assert_eq!(shown(&before), [ac("AC-1-01", Label::NotRun, None)]);
    assert_eq!(counts(&before), (0, 0, 1));
    assert_eq!((before.qa, before.review), (Report::NotRun, Report::NotRun));
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let verifying = snapshot(&store)?;
    assert_eq!(
        verifying.candidate,
        Some(CandidateShown {
            candidate_id: c1.clone(),
            state: "VERIFYING".to_owned()
        })
    );
    assert_eq!(shown(&verifying), [ac("AC-1-01", Label::NotRun, None)]);
    assert_eq!(
        (verifying.qa, verifying.review),
        (Report::Unknown, Report::Pass)
    );
    assert_eq!(
        verifying.checks,
        [CheckShown {
            check_id: "unit".to_owned(),
            label: Label::Pass
        }]
    );
    assert_eq!(counts(&verifying), (0, 0, 1));
    reconcile(&store).await?;
    let accepted = snapshot(&store)?;
    assert_eq!(
        accepted.candidate.as_ref().map(|c| c.state.as_str()),
        Some("ACCEPTED")
    );
    assert_eq!(shown(&accepted), [ac("AC-1-01", Label::Pass, None)]);
    assert_eq!(accepted.acs[0].requirement_id, "REQ-1");
    assert_eq!(
        accepted.acs[0]
            .rows
            .iter()
            .map(|r| (r.status.as_str(), r.evidence_id.as_str()))
            .collect::<Vec<_>>(),
        [("PASS", "rec-1")]
    );
    assert_eq!(counts(&accepted), (1, 1, 1));
    assert_eq!((accepted.qa, accepted.review), (Report::Pass, Report::Pass));
    assert!(accepted.watermark > verifying.watermark);
    println!(
        "{}",
        json!({"before": before, "verifying": verifying, "accepted": accepted})
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_or_vetoed_candidate_shows_every_ac_invalid_and_counts_none() -> R {
    // Decision 2's standing in the snapshot (J2): an ACCEPTED C1 with its PASS row that then gets an INVALID evidence
    // is vetoed: AC-1-01 INVALID(CANDIDATE_INVALID), passing and accepted 0, its state still ACCEPTED. A candidate whose
    // state is INVALID shows the same.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = accepted_c1(&store, &path).await?;
    let (qa, _) = attempts_of(&path, &child_id(&c1, "round/1"))?;
    evidence_of(
        &path,
        (&qa, &c1, "rec-veto"),
        ("unit", "INVALID"),
        &[],
        false,
    )?;
    let vetoed = snapshot(&store)?;
    assert_eq!(
        vetoed.candidate.as_ref().map(|c| c.state.as_str()),
        Some("ACCEPTED")
    );
    assert_eq!(
        shown(&vetoed),
        [ac("AC-1-01", Label::Invalid, Some(Why::CandidateInvalid))]
    );
    assert_eq!(counts(&vetoed), (0, 0, 1));
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = accepted_c1(&store, &path).await?;
    Connection::open(&path)?.execute(
        "UPDATE candidates SET state = 'INVALID' WHERE candidate_id = ?1",
        [&c1],
    )?;
    let invalid = snapshot(&store)?;
    assert_eq!(
        shown(&invalid),
        [ac("AC-1-01", Label::Invalid, Some(Why::CandidateInvalid))]
    );
    assert_eq!(counts(&invalid), (0, 0, 1));
    Ok(())
}

#[tokio::test]
async fn a_rejected_latest_candidate_vetoed_afterwards_is_invalid_in_the_snapshot_and_the_list() -> R
{
    // -752: C1's round fails on the reviewer's FAIL (QA's PASS row of AC-1-01 written at the close): C1 REJECTED, the
    // latest, AC-1-01 PASS, passing 1 and accepted 0. An INVALID evidence after it (C1 stays REJECTED) vetoes it: the
    // snapshot and `/acceptance` show AC-1-01 INVALID(CANDIDATE_INVALID), passing 0.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer, ("reviewer", "FAIL"), &[], &[]).await?;
    reconcile(&store).await?;
    let rejected = snapshot(&store)?;
    assert_eq!(
        rejected.candidate,
        Some(CandidateShown {
            candidate_id: c1.clone(),
            state: "REJECTED".to_owned()
        })
    );
    assert_eq!(shown(&rejected), [ac("AC-1-01", Label::Pass, None)]);
    assert_eq!(counts(&rejected), (1, 0, 1));
    assert_eq!(rejected.review, Report::Fail);
    evidence_of(
        &path,
        (&qa, &c1, "rec-veto"),
        ("unit", "INVALID"),
        &[],
        false,
    )?;
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["REJECTED"]);
    let vetoed = snapshot(&store)?;
    assert_eq!(
        shown(&vetoed),
        [ac("AC-1-01", Label::Invalid, Some(Why::CandidateInvalid))]
    );
    assert_eq!(counts(&vetoed), (0, 0, 1));
    let page = store
        .reader()?
        .acceptance_page("R", 100, None)?
        .ok_or("no run R")?;
    assert_eq!(
        page.items,
        [AcceptanceItem {
            spec_id: "s1".to_owned(),
            spec_revision: 1,
            candidate_id: c1.clone(),
            candidate_state: "REJECTED".to_owned(),
            ac_id: "AC-1-01".to_owned(),
            label: Label::Invalid,
            why: Some(Why::CandidateInvalid),
            latest: true,
        }]
    );
    assert_eq!(page.next, None);
    Ok(())
}

#[tokio::test]
async fn a_stale_candidate_without_a_successor_shows_every_ac_stale_and_counts_none() -> R {
    // Decision 2's standing: C1 ACCEPTED, then made STALE by `candidate.stale` (a specification change) with no newer
    // candidate: still the latest, AC-1-01 STALE(CANDIDATE_STALE), passing and accepted 0.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = accepted_c1(&store, &path).await?;
    Connection::open(&path)?.execute(
        "UPDATE candidates SET state = 'STALE' WHERE candidate_id = ?1",
        [&c1],
    )?;
    Connection::open(&path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'stale-c1', \
         'R', 'candidate.stale', json_object('candidate_id', ?1, 'reason', 'spec_changed'), '2026-11-12T00:00:09Z')",
        [&c1],
    )?;
    let stale = snapshot(&store)?;
    assert_eq!(
        stale.candidate,
        Some(CandidateShown {
            candidate_id: c1,
            state: "STALE".to_owned()
        })
    );
    assert_eq!(
        shown(&stale),
        [ac("AC-1-01", Label::Stale, Some(Why::CandidateStale))]
    );
    assert_eq!(counts(&stale), (0, 0, 1));
    Ok(())
}

#[tokio::test]
async fn a_verifier_tasks_acceptance_changes_neither_accepted_nor_total() -> R {
    // J7: C1 accepted (1 of 1). A verifier task given a requirement of its own (REQ-3 with AC-3-01, what the explicit
    // filter of verifier tasks guards against) and ACCEPTED: AC-3-01 is not in the scope, the counts unchanged.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = accepted_c1(&store, &path).await?;
    let before = snapshot(&store)?;
    let qa_task = child_id(&child_id(&c1, "round/1"), "task/qa");
    Connection::open(&path)?.execute_batch(&format!(
        "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-3', 's1', 'r3', 1);
         INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-3-01', 'REQ-3', 's1', 'b3', 'unit');
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{qa_task}', 'REQ-3', 's1');
         UPDATE tasks SET state = 'ACCEPTED' WHERE task_id = '{qa_task}';"
    ))?;
    let after = snapshot(&store)?;
    assert_eq!(shown(&after), shown(&before));
    assert_eq!(shown(&after), [ac("AC-1-01", Label::Pass, None)]);
    assert_eq!(counts(&after), (1, 1, 1));
    Ok(())
}

#[tokio::test]
async fn the_snapshots_qa_pass_is_unknown_over_an_empty_scope_or_an_ac_without_rows_and_fail_and_not_run_stay()
-> R {
    // Decision 5 (-163, -203, -255, -352, -852; REQ-053 behaviour 8): no report yet is NOT_RUN; QA's PASS with AC-1-01's
    // row in place is PASS and the reviewer's FAIL is FAIL; the round's scope empty (no product path opens one: a
    // fixture), QA's PASS is UNKNOWN and FAIL stays. QA's FAIL while AC-1-01 has no row stays FAIL.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    let none = snapshot(&store)?;
    assert_eq!((none.qa, none.review), (Report::NotRun, Report::NotRun));
    evidence_of(
        &path,
        (&qa, &c1, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        false,
    )?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer, ("reviewer", "FAIL"), &[], &[]).await?;
    let reported = snapshot(&store)?;
    assert_eq!((reported.qa, reported.review), (Report::Pass, Report::Fail));
    Connection::open(&path)?.execute(
        "UPDATE events SET payload_json = json_set(payload_json, '$.acceptance_ids', json('[]')) \
         WHERE kind = 'verification.round_opened'",
        [],
    )?;
    let empty = snapshot(&store)?;
    assert_eq!((empty.qa, empty.review), (Report::Unknown, Report::Fail));
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (qa, _) = start(&store, &path, &child_id(&c1, "round/1"), &[]).await?;
    settle(&store, &path, &qa, ("qa", "FAIL"), &[], &[]).await?;
    let failed = snapshot(&store)?;
    assert_eq!((failed.qa, failed.review), (Report::Fail, Report::NotRun));
    Ok(())
}

#[tokio::test]
async fn an_unobserved_claim_is_invalid_insufficient_until_a_later_round_observes_the_ac() -> R {
    // Decision 3 (REQ-053 failure mode 2): QA reports PASS on round 1 without any evidence of AC-1-01's scenario check:
    // the round is verified again (no rework) and the snapshot shows AC-1-01 INVALID(INSUFFICIENT), no row written.
    // Round 2's verifiers are told to run `unit` again (decision 4's reverification context; QA alone is told every
    // in-scope AC's scenario check; round 1 had none of it). Its QA's PASS row makes AC-1-01 PASS (J2) and the join
    // accepts C1.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (r1, r2) = (child_id(&c1, "round/1"), child_id(&c1, "round/2"));
    let (qa1, reviewer1) = start(&store, &path, &r1, &[]).await?;
    settle(&store, &path, &qa1, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer1, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "reverify".to_owned())]);
    assert_eq!(done.opened, std::slice::from_ref(&r2));
    let claimed = snapshot(&store)?;
    assert_eq!(
        shown(&claimed),
        [ac("AC-1-01", Label::Invalid, Some(Why::Insufficient))]
    );
    assert_eq!(counts(&claimed), (0, 0, 1));
    assert_eq!(
        texts(
            &path,
            "SELECT CAST(count(*) AS TEXT) FROM acceptance_results"
        )?,
        ["0"]
    );
    let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
    let rerun = "本轮须重跑 AC-1-01 的场景检查 unit：该 AC 在候选上的投影未定";
    let qa_scope = scope_of(&path, &r2, &qa2, "qa")?;
    assert!(
        qa_scope.iter().any(|l| l == rerun) && qa_scope.iter().any(|l| l == QA_RERUN),
        "{qa_scope:?}"
    );
    let review_scope = scope_of(&path, &r2, &reviewer2, "reviewer")?;
    assert!(
        review_scope.iter().any(|l| l == rerun) && !review_scope.iter().any(|l| l == QA_RERUN),
        "{review_scope:?}"
    );
    let first = scope_of(&path, &r1, &qa1, "qa")?;
    assert!(
        !first.iter().any(|l| l.starts_with("本轮须重跑")),
        "{first:?}"
    );
    pass(&path, &qa2, &c1, "rec-2")?;
    settle(&store, &path, &qa2, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer2, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2, "passable".to_owned())]);
    let observed = snapshot(&store)?;
    assert_eq!(shown(&observed), [ac("AC-1-01", Label::Pass, None)]);
    assert_eq!(counts(&observed), (1, 1, 1));
    println!(
        "{}",
        json!({"claimed": claimed, "observed": observed, "qa_scope": qa_scope, "review_scope": review_scope})
    );
    Ok(())
}

#[tokio::test]
async fn a_scenario_check_that_ran_without_a_row_is_not_run_and_its_check_is_listed_for_the_next_round()
-> R {
    // Decision 3 (ADR-027 -651): QA ran AC-1-01's scenario check, which timed out (evidence, no row), and reported PASS:
    // no unobserved claim, AC-1-01 NOT_RUN, the check UNKNOWN. The round is verified again; its verifiers are told to
    // run `unit` again with its latest reason (decision 4), once.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (r1, r2) = (child_id(&c1, "round/1"), child_id(&c1, "round/2"));
    let (qa1, reviewer1) = start(&store, &path, &r1, &[]).await?;
    evidence_of(&path, (&qa1, &c1, "rec-1"), ("unit", "FAIL"), &[], true)?;
    Connection::open(&path)?.execute(
        "UPDATE events SET payload_json = json_set(payload_json, '$.outcome_reason', 'TIMED_OUT') \
         WHERE event_id = 'rec-1/evidence'",
        [],
    )?;
    settle(&store, &path, &qa1, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer1, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1, "reverify".to_owned())]);
    let ran = snapshot(&store)?;
    assert_eq!(shown(&ran), [ac("AC-1-01", Label::NotRun, None)]);
    assert_eq!(
        ran.checks,
        [CheckShown {
            check_id: "unit".to_owned(),
            label: Label::Unknown
        }]
    );
    let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
    let line = "本轮须重跑检查 unit：它在候选上为 UNKNOWN（最近的原因：TIMED_OUT）";
    for (attempt, role) in [(&qa2, "qa"), (&reviewer2, "reviewer")] {
        let scope = scope_of(&path, &r2, attempt, role)?;
        assert_eq!(
            scope
                .iter()
                .filter(|l| l.contains(" unit"))
                .collect::<Vec<_>>(),
            [line],
            "{role}: {scope:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_cancelled_tasks_own_ac_is_in_none_of_the_three_counts() -> R {
    // -102: on C, D (AC-1-01) and E (AC-2-01) each with its round; D cancelled; E's QA check writes PASS rows of both
    // ACs; both rounds pass and the join (E alone takes part) accepts C: accepted = passing = 1 and total = 1, AC-1-01
    // listed PASS but not counted.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'CANCELLED', revision = revision + 1 WHERE task_id = 'D';",
    )?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-1-01", "AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    verify(
        &store,
        &path,
        (&r1, &c, "rec-1"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    reconcile(&store).await?;
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["ACCEPTED"]);
    let view = snapshot(&store)?;
    assert_eq!(
        shown(&view),
        [
            ("AC-1-01".to_owned(), Label::Pass, None, false),
            ac("AC-2-01", Label::Pass, None)
        ]
    );
    assert_eq!(counts(&view), (1, 1, 1));
    Ok(())
}

#[tokio::test]
async fn the_lists_give_each_candidates_projection_newest_first_and_each_issues_state_evaluated_now()
-> R {
    // Decisions 5 and 10 (the HTTP lists): C1's round fails on AC-1-01 (its acceptance Issue OPEN; the reviewer's low
    // finding an Issue too); D reworked into C2, whose PASS row of AC-1-01 is written straight in (nothing settles: the
    // materialised state stays OPEN). `/acceptance` pages C2 then C1, each with its own label and `latest`; `/issues`
    // pages newest first and shows the acceptance Issue RESOLVED by the state function while its column says OPEN.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (qa, reviewer) = start(&store, &path, &child_id(&c1, "round/1"), &[]).await?;
    evidence_with(
        &path,
        (&qa, &c1, "rec-1"),
        ("unit", "FAIL"),
        &[("AC-1-01", "TEST-001-01", "FAIL")],
        &[("TEST-001-01", "test_001_01_x", "FAIL")],
    )?;
    settle(&store, &path, &qa, ("qa", "FAIL"), &[], &[]).await?;
    let f2 = "[建议] src/b.rs:3 — 命名";
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "PASS"),
        &[finding("F2", "low", f2, &["src/b.rs"])],
        &[],
    )
    .await?;
    reconcile(&store).await?;
    let acceptance_issue = child_id(&c1, "acceptance/AC-1-01/test_001_01_x");
    let finding_issue = child_id(&child_id(&reviewer, "report"), "finding/F2");
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    evidence_of(
        &path,
        ("A1", &c2, "rec-2"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        false,
    )?;
    assert_eq!(state_of(&path, &acceptance_issue)?, "OPEN");
    let reader = store.reader()?;
    let item = |candidate: &str, state: &str, label: Label, latest: bool| AcceptanceItem {
        spec_id: "s1".to_owned(),
        spec_revision: 1,
        candidate_id: candidate.to_owned(),
        candidate_state: state.to_owned(),
        ac_id: "AC-1-01".to_owned(),
        label,
        why: None,
        latest,
    };
    let first = reader.acceptance_page("R", 1, None)?.ok_or("no run R")?;
    assert_eq!(first.items, [item(&c2, "VERIFYING", Label::Pass, true)]);
    let second = reader
        .acceptance_page("R", 1, first.next.clone())?
        .ok_or("no run R")?;
    assert_eq!(second.items, [item(&c1, "REJECTED", Label::Fail, false)]);
    assert_eq!(second.next, None);
    let whole = reader.acceptance_page("R", 500, None)?.ok_or("no run R")?;
    assert_eq!(whole.items, [first.items, second.items].concat());
    assert_eq!(whole.next, None);
    let issues = reader.issues_page("R", 500, None)?.ok_or("no run R")?;
    assert_eq!(
        issues
            .items
            .iter()
            .map(|i| (i.issue_id.as_str(), i.state.as_str()))
            .collect::<Vec<_>>(),
        [
            (acceptance_issue.as_str(), "RESOLVED"),
            (finding_issue.as_str(), "OPEN")
        ]
    );
    assert_eq!(
        (
            &issues.items[1].report_id,
            &issues.items[1].finding_id,
            &issues.items[1].severity,
            &issues.items[1].candidate_id
        ),
        (
            &Some(child_id(&reviewer, "report")),
            &Some("F2".to_owned()),
            &"low".to_owned(),
            &Some(c1.clone())
        )
    );
    assert_eq!(
        (&issues.items[0].report_id, &issues.items[0].finding_id),
        (&None, &None)
    );
    let one = reader.issues_page("R", 1, None)?.ok_or("no run R")?;
    assert_eq!(one.items, issues.items[..1]);
    let rest = reader.issues_page("R", 1, one.next)?.ok_or("no run R")?;
    assert_eq!(rest.items, issues.items[1..]);
    let past = reader.issues_page("R", 1, rest.next)?.ok_or("no run R")?;
    assert!(past.items.is_empty() && past.next.is_none(), "{past:?}");
    assert_eq!(state_of(&path, &acceptance_issue)?, "OPEN");
    // M2T18R-53: candidates without a `candidate.frozen` (imported or older data) share the freeze seq 0; the cursor
    // still names one item, so paging one by one gives every item once.
    drop(reader);
    Connection::open(&path)?.execute("DELETE FROM events WHERE kind = 'candidate.frozen'", [])?;
    let reader = store.reader()?;
    let whole = reader.acceptance_page("R", 500, None)?.ok_or("no run R")?;
    assert_eq!(whole.items.len(), 2, "{whole:?}");
    let mut paged = Vec::new();
    let mut next = None;
    loop {
        let page = reader.acceptance_page("R", 1, next)?.ok_or("no run R")?;
        paged.extend(page.items);
        next = page.next;
        if next.is_none() || paged.len() > 4 {
            break;
        }
    }
    assert_eq!(paged, whole.items);
    assert_eq!(reader.acceptance("NO-RUN")?, None);
    assert_eq!(reader.acceptance_page("NO-RUN", 1, None)?, None);
    assert_eq!(reader.issues_page("NO-RUN", 1, None)?, None);
    Ok(())
}

#[tokio::test]
async fn an_issues_chain_names_its_report_and_the_raw_session_and_native_attempt_registered_with_it()
-> R {
    // Decision 15 at the store (TEST-054-04's chain; the product path checks the objects): the reviewer's finding F1's
    // Issue traces to its CURRENT report row, the report object, raw session and native attempt artifacts its settle
    // registered. Here the objects are names only, so `verify_chain` finds no report object (a negative control); an
    // acceptance failure's Issue and an unknown id have no chain.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (qa, reviewer) = start(&store, &path, &child_id(&c1, "round/1"), &[]).await?;
    evidence_with(
        &path,
        (&qa, &c1, "rec-1"),
        ("unit", "FAIL"),
        &[("AC-1-01", "TEST-001-01", "FAIL")],
        &[("TEST-001-01", "test_001_01_x", "FAIL")],
    )?;
    settle(&store, &path, &qa, ("qa", "FAIL"), &[], &[]).await?;
    let f1 = "[缺陷] src/a.rs:12 — 越界访问";
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &[finding("F1", "critical", f1, &["src/a.rs"])],
        &[],
    )
    .await?;
    let report = child_id(&reviewer, "report");
    let issue = child_id(&report, "finding/F1");
    let chain = store
        .reader()?
        .issue_chain(&issue)?
        .ok_or("F1's Issue has a chain")?;
    let artifact = |column: &str| -> R<(String, String)> {
        let found = texts(
            &path,
            &format!(
                "SELECT a.path || ' ' || a.sha256 FROM independent_reports r JOIN artifacts a \
                 ON a.artifact_id = r.{column} WHERE r.report_id = '{report}'"
            ),
        )?;
        let (p, s) = found
            .first()
            .and_then(|f| f.split_once(' '))
            .ok_or("no artifact")?;
        Ok((p.to_owned(), s.to_owned()))
    };
    let native: Vec<String> = texts(
        &path,
        &format!(
            "SELECT json_extract(payload_json, '$.native_attempt.path') || ' ' || \
             json_extract(payload_json, '$.native_attempt.sha256') FROM events WHERE kind = 'verification.report' \
             AND json_extract(payload_json, '$.report_id') = '{report}'"
        ),
    )?;
    let (native_path, native_sha256) = native
        .first()
        .and_then(|n| n.split_once(' '))
        .ok_or("no native attempt")?;
    let (report_path, report_sha256) = artifact("report_artifact_id")?;
    let (raw_path, raw_sha256) = artifact("raw_session_artifact_id")?;
    assert_eq!(
        (
            chain.issue.issue_id.as_str(),
            chain.issue.finding_id.as_str(),
            chain.issue.severity.as_str(),
            chain.issue.candidate_id.as_deref()
        ),
        (issue.as_str(), "F1", "critical", Some(c1.as_str()))
    );
    assert_eq!(
        (
            chain.report.report_id.as_str(),
            chain.report.state.as_str(),
            &chain.report.path,
            &chain.report.sha256
        ),
        (report.as_str(), "CURRENT", &report_path, &report_sha256)
    );
    assert_eq!(
        (&chain.raw.path, &chain.raw.sha256),
        (&raw_path, &raw_sha256)
    );
    assert_eq!(
        chain
            .native
            .as_ref()
            .map(|n| (n.path.as_str(), n.sha256.as_str())),
        Some((native_path, native_sha256))
    );
    let check = crate::verification::report::verify_chain(dir.path(), &chain);
    assert!(!check.report_digest && !check.holds(), "{check:?}");
    reconcile(&store).await?;
    let acceptance = texts(&path, "SELECT issue_id FROM issues WHERE report_id IS NULL")?;
    assert_eq!(acceptance.len(), 1, "{acceptance:?}");
    for id in acceptance
        .iter()
        .map(String::as_str)
        .chain(["no-such-issue"])
    {
        assert_eq!(store.reader()?.issue_chain(id)?, None, "{id}");
    }
    Ok(())
}
