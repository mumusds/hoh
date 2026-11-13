//! Issues against a real database (docs/adr/ADR-029.md decisions 9 and 10, J4, J6): findings and acceptance failures
//! as Issues, confirmations delivered and recorded, observation facts, and the state materialised by `settle_all`.

use super::*;
use crate::persistence::completion::tests::{
    R, completion, consistent, evidence_judged, evidence_of, freeze, freeze_of, fresh_objects,
    open, pass_recording, result_of, second_writer, texts, verifier_completion, verifiers,
};
use crate::persistence::{Reconciled, Store, VerifierCompleted, VerifierOutcome, WriterCompletion};
use std::path::{Path, PathBuf};

/// A finding as a report gives it: id, severity, summary and the paths of its evidence.
pub(in crate::persistence) fn finding(
    id: &str,
    severity: &str,
    summary: &str,
    paths: &[&str],
) -> Value {
    json!({"id": id, "severity": severity, "summary": summary,
        "evidence": paths.iter().map(|p| json!({"path": p, "sha256": "a".repeat(64)})).collect::<Vec<_>>()})
}

/// A confirmation entry of a verifier's TaskResult `issues`.
pub(in crate::persistence) fn confirm(issue: &str, status: &str, observed: &str) -> Value {
    json!({"issue_id": issue, "state": status, "observed": observed})
}

/// The payloads of the events of kind `kind`, in order.
pub(in crate::persistence) fn events(path: &Path, kind: &str) -> R<Vec<Value>> {
    texts(
        path,
        &format!("SELECT payload_json FROM events WHERE kind = '{kind}' ORDER BY seq"),
    )?
    .iter()
    .map(|p| Ok(serde_json::from_str(p)?))
    .collect()
}

/// The `issues` rows as JSON objects, in insertion order.
fn issue_rows(path: &Path) -> R<Vec<Value>> {
    texts(
        path,
        "SELECT json_object('issue_id', issue_id, 'candidate_id', candidate_id, 'severity', severity, 'state', state, \
         'fingerprint', fingerprint, 'description', description, 'report_id', report_id, 'finding_id', finding_id) \
         FROM issues ORDER BY rowid",
    )?
    .iter()
    .map(|r| Ok(serde_json::from_str(r)?))
    .collect()
}

pub(in crate::persistence) fn state_of(path: &Path, issue: &str) -> R<String> {
    Ok(texts(
        path,
        &format!("SELECT state FROM issues WHERE issue_id = '{issue}'"),
    )?
    .pop()
    .ok_or("no such issue")?)
}

/// D's attempt `n` (0 the world's A) completes freezing tree `tree`: the candidate.
pub(in crate::persistence) async fn freeze_d(
    store: &Store,
    path: &Path,
    n: usize,
    tree: char,
) -> R<String> {
    let attempt = if n == 0 {
        "A".to_owned()
    } else {
        format!("A{n}")
    };
    if n > 0 {
        Connection::open(path)?.execute_batch(&format!(
            "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'D';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
               started_at) VALUES ('{attempt}', 'D', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:0{n}Z');"
        ))?;
    }
    let job = freeze_of(tree);
    let candidate = job.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: attempt.clone(),
            ..completion(freeze(job, result_of("D", &attempt)?))
        })
        .await?;
    Ok(candidate)
}

/// Round `round`'s two verifier tasks dispatched and started (with any other READY verifier task), each attempt's
/// delivery recorded as the executor records it from the store's assignment (`context.packed` `confirm_ids`), unless
/// `forged` names other ids for its role: (qa attempt, reviewer attempt).
pub(in crate::persistence) async fn start(
    store: &Store,
    path: &Path,
    round: &str,
    forged: &[(&str, &[&str])],
) -> R<(String, String)> {
    let started = dispatch(store, path, forged).await?;
    let of = |role: &str| {
        started
            .get(&child_id(round, &format!("task/{role}")))
            .cloned()
            .ok_or(format!("{round}'s {role} was not dispatched"))
    };
    Ok((of("qa")?, of("reviewer")?))
}

/// Every verifier task the scheduler dispatches now, started and its delivery recorded as [`start`] does: task ->
/// attempt.
async fn dispatch(
    store: &Store,
    path: &Path,
    forged: &[(&str, &[&str])],
) -> R<BTreeMap<String, String>> {
    let mut started = BTreeMap::new();
    while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
        let role = if d.task_id.ends_with("/qa") {
            "qa"
        } else {
            "reviewer"
        };
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!(
            "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = '{0}';
             UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{1}';",
            d.attempt_id, d.task_id
        ))?;
        let assigned = crate::persistence::team::assignment(
            &conn,
            "p1",
            "R",
            &d.task_id,
            &d.attempt_id,
            role,
        )?;
        let mut ids = assigned
            .context
            .and_then(|f| f.confirm_ids)
            .ok_or("a verifier's facts carry its confirm_ids")?;
        if let Some((_, forged)) = forged.iter().find(|(r, _)| *r == role) {
            ids = forged.iter().map(|i| (*i).to_owned()).collect();
        }
        conn.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
             'context.packed', json_object('attempt_id', ?2, 'confirm_ids', json(?3)), '2026-11-12T00:00:09Z')",
            params![
                child_id(&d.attempt_id, "context.packed"),
                d.attempt_id,
                json!(ids).to_string()
            ],
        )?;
        started.insert(d.task_id.clone(), d.attempt_id.clone());
    }
    Ok(started)
}

/// The confirm_ids attempt `attempt` was delivered.
pub(in crate::persistence) fn delivered_to(path: &Path, attempt: &str) -> R<Vec<String>> {
    Ok(delivered(&Connection::open(path)?, attempt)?
        .into_iter()
        .collect())
}

/// Settle verifier attempt `attempt` of `role` with a report `status` consistent with it, its `findings`, and its
/// TaskResult `issues`.
pub(in crate::persistence) async fn settle(
    store: &Store,
    path: &Path,
    attempt: &str,
    (role, status): (&str, &str),
    findings: &[Value],
    issues: &[Value],
) -> R<VerifierCompleted> {
    let mut report = consistent(path, attempt, (role, status), fresh_objects(path)?)?;
    report.report_json["findings"] = json!(findings);
    report.findings = findings
        .iter()
        .filter_map(|f| f["summary"].as_str().map(str::to_owned))
        .collect();
    report.confirmations = issues.to_vec();
    Ok(store
        .complete_verifier_attempt(verifier_completion(
            attempt,
            VerifierOutcome::Settle {
                report: Box::new(report),
            },
        ))
        .await?)
}

/// QA attempt `qa`'s PASS evidence of AC-1-01 on `candidate` (its row written at its round's close).
pub(in crate::persistence) fn pass(path: &Path, qa: &str, candidate: &str, record: &str) -> R {
    evidence_of(
        path,
        (qa, candidate, record),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )
}

pub(in crate::persistence) async fn reconcile(store: &Store) -> R<Reconciled> {
    Ok(store
        .reconcile_verification("R", Default::default())
        .await?)
}

/// The failure items of round `round`, `task:item`.
pub(in crate::persistence) fn items(path: &Path, round: &str) -> R<Vec<String>> {
    texts(
        path,
        &format!(
            "SELECT json_extract(i.value, '$.task_id') || ':' || json_extract(i.value, '$.item') FROM events e, \
             json_each(e.payload_json, '$.items') i WHERE e.kind = 'verification.failures' \
             AND json_extract(e.payload_json, '$.round_id') = '{round}' ORDER BY 1"
        ),
    )
}

/// Round `round`'s close: its payload.
pub(in crate::persistence) fn closed(path: &Path, round: &str) -> R<Value> {
    Ok(serde_json::from_str(
        &texts(
            path,
            &format!(
                "SELECT payload_json FROM events WHERE kind = 'verification.round_closed' \
                 AND json_extract(payload_json, '$.round_id') = '{round}'"
            ),
        )?
        .pop()
        .ok_or(format!("{round} is not closed"))?,
    )?)
}

pub(in crate::persistence) const X_SUMMARY: &str =
    "[缺陷] src/a.rs:12 — 越界访问；影响：崩溃；修复方向：检查长度";

/// A run whose C1 round failed on the reviewer's critical finding X (and the QA's PASS): D reworked and frozen again
/// as C2, whose round's verifiers are started with the deliveries the store gives (or `forged`).
struct Delivered {
    _dir: tempfile::TempDir,
    store: Store,
    path: PathBuf,
    c1: String,
    c2: String,
    r2: String,
    x: String,
    fingerprint: String,
    qa: String,
    reviewer: String,
}

async fn x_delivered(severity: &str, forged: &[(&str, &[&str])]) -> R<Delivered> {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    // A critical or high finding fails the round; a medium one does not, so QA's FAIL fails it then.
    let qa_status = if grave(severity) { "PASS" } else { "FAIL" };
    settle(&store, &path, &qa, ("qa", qa_status), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &[finding("F1", severity, X_SUMMARY, &["src/a.rs"])],
        &[],
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "repair".to_owned())]);
    assert_eq!(done.reworked, ["D"]);
    let x = child_id(&child_id(&reviewer, "report"), "finding/F1");
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let r2 = child_id(&c2, "round/1");
    let (qa, reviewer) = start(&store, &path, &r2, forged).await?;
    Ok(Delivered {
        _dir: dir,
        store,
        path,
        c1,
        c2,
        r2,
        fingerprint: acceptance::fingerprint(severity, X_SUMMARY, &["src/a.rs".to_owned()]),
        x,
        qa,
        reviewer,
    })
}

#[tokio::test]
async fn test_054_04_findings_become_traceable_issues() -> R {
    // AC-054-04 at the store (REQ-054 behaviour 5; docs/adr/ADR-029.md decision 9, J4): the reviewer's FAIL report with
    // F1 critical (evidence in src/a.rs) and F2 low, QA PASS, the check PASS. Its settle registers one Issue per
    // finding on C1 (severity, fingerprint with its locator, finding id, its report), `issue.opened{source: finding}`
    // without a round, and present facts; the critical one fails the round (C1 REJECTED), the low one is no item.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    let f1 = "[缺陷] src/a.rs:12 — 越界访问";
    let f2 = "[建议] src/b.rs:3 — 命名";
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &[
            finding("F1", "critical", f1, &["src/a.rs"]),
            finding("F2", "low", f2, &["src/b.rs"]),
        ],
        &[],
    )
    .await?;
    let report = child_id(&reviewer, "report");
    let rows = issue_rows(&path)?;
    let want = |id: &str, severity: &str, summary: &str, located: &str| {
        json!({"issue_id": child_id(&report, &format!("finding/{id}")), "candidate_id": c1, "severity": severity,
            "state": "OPEN", "fingerprint": acceptance::fingerprint(severity, summary, &[located.to_owned()]),
            "description": summary, "report_id": report, "finding_id": id})
    };
    assert_eq!(
        rows,
        [
            want("F1", "critical", f1, "src/a.rs"),
            want("F2", "low", f2, "src/b.rs")
        ]
    );
    // Each Issue's report is the reviewer's row, CURRENT.
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT state || ':' || role FROM independent_reports WHERE report_id = '{report}'"
            )
        )?,
        ["CURRENT:reviewer"]
    );
    let opened = events(&path, "issue.opened")?;
    assert_eq!(opened.len(), 2);
    for (event, row) in opened.iter().zip(&rows) {
        assert_eq!(event["source"], "finding");
        assert_eq!(event["issue_id"], row["issue_id"]);
        assert_eq!(event["report_id"], json!(report));
        assert_eq!(event["attempt_id"], json!(reviewer));
        assert!(event.get("round_id").is_none(), "{event}");
    }
    let observations = events(&path, OBSERVATION)?;
    let facts: BTreeSet<String> = observations
        .iter()
        .map(|o| {
            format!(
                "{}:{}:{}",
                o["polarity"], o["fingerprint"], o["candidate_id"]
            )
        })
        .collect();
    let want_facts: BTreeSet<String> = rows
        .iter()
        .map(|r| format!("{}:{}:{}", json!("present"), r["fingerprint"], json!(c1)))
        .collect();
    assert_eq!(facts, want_facts);
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "repair".to_owned())]);
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["REJECTED"]);
    let mut summaries = [f1, f2];
    summaries.sort_unstable();
    assert_eq!(
        items(&path, &r1)?,
        [
            format!(
                "D:issue:{}",
                rows[0]["fingerprint"].as_str().unwrap_or_default()
            ),
            format!("D:report-fail:reviewer:{}", summaries.join(" | "))
        ]
    );
    println!(
        "{}",
        json!({"issues": rows, "issue.opened": opened, "issue.observation": observations,
            "round_closed": closed(&path, &r1)?})
    );
    // Only a low finding, both reports PASS, the check PASS: passable (low never fails a round) and joined: C1
    // ACCEPTED, its Issue OPEN (docs/adr/ADR-029.md decision 11).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "PASS"),
        &[finding("F2", "low", f2, &["src/b.rs"])],
        &[],
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    assert_eq!(done.joinable, std::slice::from_ref(&c1));
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["ACCEPTED"]);
    assert_eq!(
        texts(&path, "SELECT severity || ':' || state FROM issues")?,
        ["low:OPEN"]
    );
    assert!(items(&path, &r1)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_critical_finding_on_a_superseded_candidate_still_fails_its_source() -> R {
    // CR-018's product path (docs/adr/ADR-029.md decision 9, ADR-028 -801): C1's reviewer reports a critical finding
    // (its Issue registered), then E's freeze of C2 supersedes C1 and takes D into C2's round; C1's round closes
    // superseded and D gets the Issue's `issue:<fingerprint>` item (reworked). Again with the report arriving after the
    // supersession (STALE): the same. C2's verifiers are not asked to confirm the Issue while C1's round is open
    // (M2T18D-04).
    for late in [false, true] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        let c1 = freeze_d(&store, &path, 0, 'b').await?;
        let r1 = child_id(&c1, "round/1");
        let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
        pass(&path, &qa, &c1, "rec-1")?;
        settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
        let critical = [finding("F1", "critical", X_SUMMARY, &["src/a.rs"])];
        if !late {
            settle(
                &store,
                &path,
                &reviewer,
                ("reviewer", "FAIL"),
                &critical,
                &[],
            )
            .await?;
        }
        second_writer(&path, "E", "AE")?;
        let job = freeze_of('c');
        let c2 = job.sealed.candidate_id.clone();
        store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: "AE".to_owned(),
                ..completion(freeze(job, result_of("E", "AE")?))
            })
            .await?;
        if late {
            settle(
                &store,
                &path,
                &reviewer,
                ("reviewer", "FAIL"),
                &critical,
                &[],
            )
            .await?;
        }
        // C1's round is not closed yet (no reconcile): its Issue is not C2's verifiers' to confirm.
        let r2 = child_id(&c2, "round/1");
        let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
        assert!(delivered_to(&path, &qa2)?.is_empty(), "late {late}");
        assert!(delivered_to(&path, &reviewer2)?.is_empty(), "late {late}");
        // Taken in CURRENT before the supersession (then made STALE by the freeze), or STALE after it.
        assert_eq!(
            texts(
                &path,
                &format!(
                    "SELECT json_extract(payload_json, '$.state') FROM events WHERE kind = 'verification.report' \
                     AND json_extract(payload_json, '$.attempt_id') = '{reviewer}'"
                )
            )?,
            [if late { "STALE" } else { "CURRENT" }],
            "late {late}"
        );
        let fingerprint = acceptance::fingerprint("critical", X_SUMMARY, &["src/a.rs".to_owned()]);
        assert_eq!(
            texts(
                &path,
                "SELECT fingerprint || ':' || candidate_id FROM issues"
            )?,
            [format!("{fingerprint}:{c1}")]
        );
        let done = reconcile(&store).await?;
        assert!(
            done.closed.contains(&(r1.clone(), "superseded".to_owned())),
            "late {late}: {done:?}"
        );
        assert_eq!(
            items(&path, &r1)?,
            [format!("D:issue:{fingerprint}")],
            "late {late}"
        );
        // Taken by its owner: reworked (D was VERIFYING in C2's round).
        assert_eq!(
            texts(
                &path,
                &format!(
                    "SELECT json_extract(payload_json, '$.by') FROM events WHERE kind = 'verification.failures_consumed' \
                     AND json_extract(payload_json, '$.round_id') = '{r1}' AND json_extract(payload_json, '$.task_id') = 'D'"
                )
            )?,
            ["rework"],
            "late {late}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_rejected_reports_findings_open_no_issue_and_write_no_fact_and_record_report_takes_a_current_ones()
-> R {
    // J4, M2T18D-452: `Store::record_report` takes a CURRENT report's findings as Issues with their present facts, in its
    // own transaction; a REJECTED report's findings (and confirmations) are untrusted: no Issue, no fact.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (qa, _) = start(&store, &path, &child_id(&c1, "round/1"), &[]).await?;
    let findings = json!([finding("F1", "high", "AC-1-01 FAIL：越界", &[])]);
    let mut rejected = consistent(&path, &qa, ("qa", "FAIL"), fresh_objects(&path)?)?;
    rejected.report_json["findings"] = findings.clone();
    rejected.report_json["candidate_id"] = json!(format!("CAND-{}", "0".repeat(64)));
    let done = store.record_report(rejected.intake(&qa)).await?;
    assert_eq!(done.state, crate::persistence::ReportState::Rejected);
    assert!(issue_rows(&path)?.is_empty());
    assert!(events(&path, OBSERVATION)?.is_empty());
    // Another attempt of the same task, its report consistent: CURRENT, its finding an Issue and a present fact.
    Connection::open(&path)?.execute_batch(&format!(
        "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
           SELECT '{qa}-2', task_id, session_id, 'RUNNING', 1, 1, 1 FROM attempts WHERE attempt_id = '{qa}';"
    ))?;
    let second = format!("{qa}-2");
    let mut current = consistent(&path, &second, ("qa", "FAIL"), fresh_objects(&path)?)?;
    current.report_json["findings"] = findings;
    let done = store.record_report(current.intake(&second)).await?;
    assert_eq!(done.state, crate::persistence::ReportState::Current);
    let report = child_id(&second, "report");
    assert_eq!(
        texts(&path, "SELECT issue_id || ':' || severity FROM issues")?,
        [format!("{}:high", child_id(&report, "finding/F1"))]
    );
    assert_eq!(events(&path, OBSERVATION)?.len(), 1);
    // Taken in again: nothing new.
    let again = store.record_report(current.intake(&second)).await?;
    assert_eq!(again.state, crate::persistence::ReportState::Current);
    assert_eq!(issue_rows(&path)?.len(), 1);
    assert_eq!(events(&path, OBSERVATION)?.len(), 1);
    assert_eq!(events(&path, "issue.opened")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_failed_close_opens_one_acceptance_issue_per_failing_test_shared_by_rounds_of_one_candidate()
-> R {
    // Decision 9 (REQ-053 behaviour 5, failure mode 1; M2T18D-55, -157, -253; J4): a round closed failed on
    // `ac-fail:<AC>:<test>` opens one high Issue per item on its candidate, no report, `issue.opened{source:
    // acceptance}` without a round; QA's FAIL without a finding still has it. E's round on the same candidate failing the
    // same AC and test has the same one Issue.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    second_writer(&path, "E", "AE")?;
    Connection::open(&path)?.execute(
        "INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('E', 'REQ-1', 's1')",
        [],
    )?;
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(freeze_of('b'), result_of("E", "AE")?))
        })
        .await?;
    let r2 = child_id(&c1, "round/2");
    // One session per role: a round's verifiers run once the other round's are done.
    for (round, record) in [(&r1, "rec-1"), (&r2, "rec-2")] {
        let (qa, rev) = start(&store, &path, round, &[]).await?;
        let (qa, rev) = (&qa, &rev);
        evidence_judged(
            &path,
            (qa, &c1, record),
            ("unit", "FAIL"),
            &[("AC-1-01", "TEST-1-01", "FAIL", vec!["test_1_01_bounds"])],
            &[("TEST-1-01", "test_1_01_bounds", "FAIL")],
        )?;
        settle(&store, &path, qa, ("qa", "FAIL"), &[], &[]).await?;
        settle(&store, &path, rev, ("reviewer", "PASS"), &[], &[]).await?;
    }
    let done = reconcile(&store).await?;
    assert_eq!(
        done.closed,
        [
            (r1.clone(), "repair".to_owned()),
            (r2.clone(), "repair".to_owned())
        ]
    );
    let issue = child_id(&c1, "acceptance/AC-1-01/test_1_01_bounds");
    let rows = issue_rows(&path)?;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0]["issue_id"], json!(issue));
    assert_eq!(rows[0]["severity"], "high");
    assert_eq!(rows[0]["state"], "OPEN");
    assert_eq!(rows[0]["candidate_id"], json!(c1));
    assert_eq!(rows[0]["report_id"], Value::Null);
    assert_eq!(rows[0]["finding_id"], Value::Null);
    assert_eq!(
        rows[0]["fingerprint"],
        json!(acceptance::fingerprint(
            "high",
            "ac-fail AC-1-01 test_1_01_bounds",
            &[]
        ))
    );
    let description = rows[0]["description"].as_str().unwrap_or_default();
    assert!(
        description.contains("AC-1-01")
            && description.contains(&c1)
            && description.contains("test_1_01_bounds")
            && description.contains("rec-1"),
        "{description}"
    );
    let opened = events(&path, "issue.opened")?;
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0]["source"], "acceptance");
    assert!(opened[0].get("round_id").is_none());
    assert!(opened[0].get("report_id").is_none());
    agrees(&path)?;
    Ok(())
}

/// D's C1 round failing AC-1-01 on test_1_01_bounds (its acceptance Issue opened), then C2 frozen: (store, path, C2's
/// round and its QA attempt, C2, the Issue).
async fn acceptance_failed(
    dir: &Path,
    degradable: bool,
) -> R<(Store, PathBuf, String, String, String, String)> {
    let (store, path) = open(dir)?;
    if degradable {
        Connection::open(&path)?.execute(
            "UPDATE requirements SET release_class = 'degradable' WHERE requirement_id = 'REQ-1'",
            [],
        )?;
    }
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let (qa, reviewer) = start(&store, &path, &child_id(&c1, "round/1"), &[]).await?;
    evidence_judged(
        &path,
        (&qa, &c1, "rec-1"),
        ("unit", "FAIL"),
        &[("AC-1-01", "TEST-1-01", "FAIL", vec!["test_1_01_bounds"])],
        &[("TEST-1-01", "test_1_01_bounds", "FAIL")],
    )?;
    settle(&store, &path, &qa, ("qa", "FAIL"), &[], &[]).await?;
    settle(&store, &path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    reconcile(&store).await?;
    let issue = child_id(&c1, "acceptance/AC-1-01/test_1_01_bounds");
    assert_eq!(state_of(&path, &issue)?, "OPEN");
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let r2 = child_id(&c2, "round/1");
    let (qa2, _) = start(&store, &path, &r2, &[]).await?;
    Ok((store, path, r2, qa2, c2, issue))
}

#[tokio::test]
async fn an_acceptance_issue_is_resolved_by_a_later_candidates_compliant_degraded_rows() -> R {
    // Decision 10 (M2T18D-250): C1's acceptance Issue on AC-1-01, whose requirement may degrade; C2's round writes a
    // compliant DEGRADED row: at its close `settle_all` makes the Issue RESOLVED on those rows. Acceptance Issues are
    // never delivered for confirmation (M2T18D-654).
    let dir = tempfile::tempdir()?;
    let (store, path, r2, qa2, c2, issue) = acceptance_failed(dir.path(), true).await?;
    assert!(delivered_to(&path, &qa2)?.is_empty());
    evidence_of(
        &path,
        (&qa2, &c2, "rec-2"),
        ("unit", "DEGRADED"),
        &[("AC-1-01", "DEGRADED")],
        true,
    )?;
    let reviewer2 = child_id(&r2, "task/reviewer/attempt-1");
    settle(&store, &path, &qa2, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer2, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2, "passable".to_owned())]);
    assert_eq!(state_of(&path, &issue)?, "RESOLVED");
    assert_eq!(
        events(&path, "issue.resolved")?,
        [json!({"issue_id": issue, "basis": format!("rows/{c2}/AC-1-01")})]
    );
    Ok(())
}

#[tokio::test]
async fn an_acceptance_issue_is_resolved_by_rows_an_earlier_inconclusive_round_of_the_candidate_wrote()
-> R {
    // M2T18D-354: C2's round writes AC-1-01's PASS row but is inconclusive (the reviewer UNKNOWN): the rows resolve C1's
    // acceptance Issue whatever the round's verdict; the next round (reverify) keeps it RESOLVED.
    let dir = tempfile::tempdir()?;
    let (store, path, r2, qa2, c2, issue) = acceptance_failed(dir.path(), false).await?;
    pass(&path, &qa2, &c2, "rec-2")?;
    let reviewer2 = child_id(&r2, "task/reviewer/attempt-1");
    settle(&store, &path, &qa2, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer2, ("reviewer", "UNKNOWN"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2, "reverify".to_owned())]);
    assert_eq!(state_of(&path, &issue)?, "RESOLVED");
    let r3 = child_id(&c2, "round/2");
    assert_eq!(done.opened, std::slice::from_ref(&r3));
    let (qa3, reviewer3) = start(&store, &path, &r3, &[]).await?;
    pass(&path, &qa3, &c2, "rec-3")?;
    settle(&store, &path, &qa3, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer3, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r3, "passable".to_owned())]);
    assert_eq!(state_of(&path, &issue)?, "RESOLVED");
    assert_eq!(events(&path, "issue.resolved")?.len(), 1);
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn an_issue_both_verifiers_were_delivered_and_one_confirmed_fixed_is_resolved_and_reopens_when_it_recurs()
-> R {
    // Decision 10: C1's critical X is delivered to both of C2's verifiers (mandatory scope, `confirm_ids`); QA confirms
    // it RESOLVED in its TaskResult issues (kept in its `verification.report`), the reviewer says nothing of it: an
    // absent fact on C2 at the round's close, X RESOLVED, the round passable (not unconfirmed). Then E's freeze of C3
    // (D taken in) and C3's reviewer finds the same defect again: a present fact on C3, X REOPENED in that settle, with a
    // new Issue of its own.
    let d = x_delivered("critical", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    assert_eq!(delivered_to(path, &d.qa)?, std::slice::from_ref(&d.x));
    assert_eq!(delivered_to(path, &d.reviewer)?, std::slice::from_ref(&d.x));
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[confirm(&d.x, "RESOLVED", "重放越界输入，已拒绝")],
    )
    .await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let reported = events(path, "verification.report")?;
    let qa_report = reported
        .iter()
        .find(|r| r["attempt_id"] == json!(d.qa))
        .ok_or("no QA report")?;
    assert_eq!(
        qa_report["confirmations"],
        json!([confirm(&d.x, "RESOLVED", "重放越界输入，已拒绝")])
    );
    assert_eq!(state_of(path, &d.x)?, "OPEN", "not at one report's settle");
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "passable".to_owned())]);
    assert_eq!(closed(path, &d.r2)?["inputs"]["unconfirmed"], json!(false));
    let absent = events(path, OBSERVATION)?
        .into_iter()
        .filter(|o| o["polarity"] == "absent")
        .collect::<Vec<_>>();
    let observation = child_id(&d.r2, &format!("observation/absent/{}", d.fingerprint));
    assert_eq!(
        absent,
        [
            json!({"observation_id": observation, "fingerprint": d.fingerprint, "polarity": "absent",
            "candidate_id": d.c2, "issue_ids": [d.x], "sources": [format!("confirmation:{}", d.qa)],
            "round_id": d.r2})
        ]
    );
    assert_eq!(state_of(path, &d.x)?, "RESOLVED");
    assert_eq!(
        events(path, "issue.resolved")?,
        [json!({"issue_id": d.x, "basis": observation})]
    );
    // C3, and the same defect found on it.
    second_writer(path, "E", "AE")?;
    let job = freeze_of('d');
    let c3 = job.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(job, result_of("E", "AE")?))
        })
        .await?;
    let r3 = child_id(&c3, "round/1");
    let (_, reviewer3) = start(store, path, &r3, &[]).await?;
    assert!(delivered_to(path, &reviewer3)?.is_empty(), "X is RESOLVED");
    settle(
        store,
        path,
        &reviewer3,
        ("reviewer", "FAIL"),
        &[finding("F9", "critical", X_SUMMARY, &["src/a.rs"])],
        &[],
    )
    .await?;
    assert_eq!(state_of(path, &d.x)?, "REOPENED");
    let reopened = events(path, "issue.reopened")?;
    assert_eq!(reopened.len(), 1);
    assert_eq!(reopened[0]["issue_id"], json!(d.x));
    assert_eq!(reopened[0]["undoes"], json!(observation));
    assert_eq!(
        reopened[0]["basis"],
        json!(child_id(
            &reviewer3,
            &format!("observation/present/{}", d.fingerprint)
        ))
    );
    assert_eq!(
        state_of(
            path,
            &child_id(&child_id(&reviewer3, "report"), "finding/F9")
        )?,
        "OPEN"
    );
    // A later veto is its own transaction's business too: an INVALID evidence on C2 changes nothing more (X is not
    // RESOLVED); on the first scenario it would have reopened X (see the veto test).
    let _ = d.c1;
    agrees(path)?;
    Ok(())
}

#[tokio::test]
async fn an_issue_a_verifier_confirms_open_is_still_present_and_fails_the_round_with_its_item_and_rework_context()
-> R {
    // Decision 10 (-150, -153, -201): C2's QA confirms X OPEN (with a medium finding of its own) and reports PASS, the
    // reviewer PASS: X is still present, the round failed with `issue:<X's fingerprint>` for D, C2 REJECTED, X not
    // resolved; D's rework context names X across the run with its report, finding and what QA observed.
    let d = x_delivered("critical", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[finding("Q1", "medium", "AC-1-01 PASS：日志冗长", &[])],
        &[confirm(&d.x, "OPEN", "长度 0 的输入仍越界")],
    )
    .await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "repair".to_owned())]);
    assert_eq!(items(path, &d.r2)?, [format!("D:issue:{}", d.fingerprint)]);
    assert_eq!(
        closed(path, &d.r2)?["inputs"]["failing_issues"],
        json!(true)
    );
    assert_eq!(
        texts(
            path,
            &format!(
                "SELECT state FROM candidates WHERE candidate_id = '{}'",
                d.c2
            )
        )?,
        ["REJECTED"]
    );
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    let rework = store.reader()?.rework_context("D")?.ok_or("no rework")?;
    let traced = rework
        .traced
        .iter()
        .find(|t| t.issue_id == d.x)
        .ok_or("X is not traced")?;
    assert_eq!(traced.fingerprint, d.fingerprint);
    assert_eq!(traced.description, X_SUMMARY);
    assert_eq!(traced.finding_id.as_deref(), Some("F1"));
    assert_eq!(traced.candidate_id.as_deref(), Some(d.c1.as_str()));
    assert!(traced.report_id.is_some());
    assert_eq!(traced.observed, ["长度 0 的输入仍越界"]);
    // And in the Developer's next prompt.
    let facts =
        crate::persistence::context::facts(&Connection::open(path)?, "R", "D", "A-next", None)?;
    assert!(
        facts
            .entries
            .iter()
            .any(|e| e.text.contains(&d.x) && e.text.contains("长度 0 的输入仍越界")),
        "{:?}",
        facts.entries
    );
    agrees(path)?;
    Ok(())
}

#[tokio::test]
async fn an_issue_reported_again_by_its_id_even_as_low_fails_the_round_whatever_the_other_side_confirmed()
-> R {
    // M2T18D-500, -551: the reviewer reports a low finding whose id is X's Issue id, QA confirms X RESOLVED: still
    // present by X's fingerprint, so the round fails on X and X is not resolved; the low finding is an Issue of its own.
    let d = x_delivered("critical", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[confirm(&d.x, "RESOLVED", "看起来已修复")],
    )
    .await?;
    settle(
        store,
        path,
        &d.reviewer,
        ("reviewer", "PASS"),
        &[finding(&d.x, "low", "仍有一处未检查长度", &[])],
        &[],
    )
    .await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "repair".to_owned())]);
    assert!(items(path, &d.r2)?.contains(&format!("D:issue:{}", d.fingerprint)));
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    assert!(
        events(path, OBSERVATION)?
            .iter()
            .all(|o| o["polarity"] == "present"),
        "no absent fact"
    );
    assert_eq!(
        state_of(
            path,
            &child_id(
                &child_id(&d.reviewer, "report"),
                &format!("finding/{}", d.x)
            )
        )?,
        "OPEN"
    );
    Ok(())
}

#[tokio::test]
async fn an_issue_neither_verifier_confirmed_leaves_the_round_inconclusive_until_its_chain_is_spent()
-> R {
    // Decision 10 (-150, -153, -254): both delivered X, neither wrote a confirmation (nor reported it): unconfirmed, the
    // round inconclusive (recorded in its inputs), verified again with X delivered again; the chain spent, D waits for a
    // decision; X stays OPEN and C2 is never accepted.
    let d = x_delivered("critical", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    let (mut qa, mut reviewer, mut round) = (d.qa.clone(), d.reviewer.clone(), d.r2.clone());
    for n in 0..3 {
        assert_eq!(
            delivered_to(path, &qa)?,
            std::slice::from_ref(&d.x),
            "round {n}"
        );
        pass(path, &qa, &d.c2, &format!("rec-chain-{n}"))?;
        settle(store, path, &qa, ("qa", "PASS"), &[], &[]).await?;
        settle(store, path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
        let done = reconcile(store).await?;
        assert_eq!(done.closed, [(round.clone(), "reverify".to_owned())]);
        assert_eq!(closed(path, &round)?["inputs"]["unconfirmed"], json!(true));
        if n == 2 {
            assert!(done.opened.is_empty());
            break;
        }
        round = child_id(&d.c2, &format!("round/{}", n + 2));
        assert_eq!(done.opened, std::slice::from_ref(&round));
        (qa, reviewer) = start(store, path, &round, &[]).await?;
    }
    assert_eq!(
        texts(
            path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    assert_eq!(
        texts(
            path,
            &format!(
                "SELECT state FROM candidates WHERE candidate_id = '{}'",
                d.c2
            )
        )?,
        ["VERIFYING"]
    );
    Ok(())
}

#[tokio::test]
async fn an_issue_confirmed_fixed_in_a_round_inconclusive_for_another_reason_is_resolved() -> R {
    // M2T18D-603: the absent fact does not depend on the round's verdict: no check ran (inconclusive), X delivered to
    // both, QA confirmed it RESOLVED, nobody reported it: X RESOLVED.
    let d = x_delivered("high", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[confirm(&d.x, "RESOLVED", "复现脚本不再失败")],
    )
    .await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "reverify".to_owned())]);
    assert_eq!(closed(path, &d.r2)?["inputs"]["unconfirmed"], json!(false));
    assert_eq!(state_of(path, &d.x)?, "RESOLVED");
    Ok(())
}

#[tokio::test]
async fn an_issue_delivered_to_one_verifier_only_is_neither_resolved_nor_holding_the_round() -> R {
    // M2T18D-254: X delivered to QA only (the reviewer's delivery without it): QA's RESOLVED is no absent fact (the two
    // deliveries do not intersect), and X does not make the round unpassable.
    let d = x_delivered("critical", &[("reviewer", &[])]).await?;
    let (store, path) = (&d.store, &d.path);
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[confirm(&d.x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "passable".to_owned())]);
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    assert!(
        events(path, OBSERVATION)?
            .iter()
            .all(|o| o["polarity"] == "present")
    );
    Ok(())
}

#[tokio::test]
async fn a_confirmation_of_an_issue_not_delivered_or_malformed_is_dropped_with_a_diagnostic() -> R {
    // Decision 10 (M2T18D-02, -156): both deliveries forged without X: QA's RESOLVED of X and a malformed entry are
    // dropped (`verification.confirmations_dropped`), the result still settles, its report keeps no confirmation, X is
    // not resolved.
    let d = x_delivered("critical", &[("qa", &[]), ("reviewer", &[])]).await?;
    let (store, path) = (&d.store, &d.path);
    pass(path, &d.qa, &d.c2, "rec-2")?;
    let done = settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[
            confirm(&d.x, "RESOLVED", "已修复"),
            json!({"severity": "high", "summary": "not a confirmation"}),
        ],
    )
    .await?;
    assert_eq!(done, VerifierCompleted::Settled);
    let dropped = events(path, "verification.confirmations_dropped")?;
    assert_eq!(dropped.len(), 1);
    assert_eq!(
        dropped[0]["dropped"]
            .as_array()
            .map(|d| d.iter().map(|e| e["reason"].clone()).collect::<Vec<_>>()),
        Some(vec![
            json!("not delivered to this attempt"),
            json!("no issue_id")
        ])
    );
    let reported = events(path, "verification.report")?;
    assert_eq!(
        reported.last().map(|r| r["confirmations"].clone()),
        Some(json!([]))
    );
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "passable".to_owned())]);
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    Ok(())
}

#[tokio::test]
async fn a_medium_issue_left_unconfirmed_does_not_hold_the_round() -> R {
    // Decision 10: only critical and high entries enter the classification; a medium X neither confirmed nor reported is
    // still OPEN after a passable round.
    let d = x_delivered("medium", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    assert_eq!(delivered_to(path, &d.qa)?, std::slice::from_ref(&d.x));
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(store, path, &d.qa, ("qa", "PASS"), &[], &[]).await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(d.r2.clone(), "passable".to_owned())]);
    assert_eq!(closed(path, &d.r2)?["inputs"]["unconfirmed"], json!(false));
    assert_eq!(state_of(path, &d.x)?, "OPEN");
    Ok(())
}

#[tokio::test]
async fn an_invalid_evidence_on_the_confirming_candidate_reopens_the_issue_in_its_own_transaction()
-> R {
    // M2T18D-601, -700, -752 analogue: X RESOLVED on C2's absent fact; an INVALID evidence later recorded on C2 vetoes
    // it: `record_evidence`'s own transaction makes X REOPENED (no reconcile in between).
    let d = x_delivered("critical", &[]).await?;
    let (store, path) = (&d.store, &d.path);
    pass(path, &d.qa, &d.c2, "rec-2")?;
    settle(
        store,
        path,
        &d.qa,
        ("qa", "PASS"),
        &[],
        &[confirm(&d.x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(store, path, &d.reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    reconcile(store).await?;
    assert_eq!(state_of(path, &d.x)?, "RESOLVED");
    let mut vetoed = pass_recording("rec-veto", &d.c2, &d.qa);
    vetoed.outcome.status = crate::verification::Status::Invalid;
    vetoed.reasons = vec!["SOURCE_CHANGED"];
    vetoed.rows.clear();
    store.record_evidence(vetoed).await?;
    assert_eq!(state_of(path, &d.x)?, "REOPENED");
    assert_eq!(
        events(path, "issue.reopened")?
            .iter()
            .map(|e| e["basis"].clone())
            .collect::<Vec<_>>(),
        [json!(format!("vetoed/{}", d.c2))]
    );
    agrees(path)?;
    Ok(())
}

#[tokio::test]
async fn a_change_request_of_the_same_fingerprint_on_the_confirming_candidate_keeps_the_issue_unresolved()
-> R {
    // M2T18D-701, -800: on C2, QA confirms X fixed and also asks for a change of the same text and severity (X's
    // fingerprint, no locator): the change request's `issue.opened` is a present fact on C2, read directly, so X is
    // not resolved and the round fails on it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    let text = "解析器在空输入上越界";
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &[finding("F1", "high", text, &[])],
        &[],
    )
    .await?;
    reconcile(&store).await?;
    let x = child_id(&child_id(&reviewer, "report"), "finding/F1");
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let r2 = child_id(&c2, "round/1");
    let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
    assert_eq!(delivered_to(&path, &qa2)?, std::slice::from_ref(&x));
    store
        .verifier_request(crate::persistence::VerifierRequest::RejectChange(
            crate::persistence::ChangeRequest {
                run_id: "R".to_owned(),
                attempt_id: qa2.clone(),
                message_id: "m1".to_owned(),
                round_id: r2.clone(),
                candidate_id: c2.clone(),
                severity: "high".to_owned(),
                text: text.to_owned(),
            },
        ))
        .await?;
    pass(&path, &qa2, &c2, "rec-2")?;
    settle(
        &store,
        &path,
        &qa2,
        ("qa", "PASS"),
        &[],
        &[confirm(&x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(
        &store,
        &path,
        &reviewer2,
        ("reviewer", "PASS"),
        &[],
        &[confirm(&x, "RESOLVED", "已修复")],
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2.clone(), "repair".to_owned())]);
    assert_eq!(state_of(&path, &x)?, "OPEN");
    // The confirmation's absent fact is written (the round's own reports did not report X), the change request wins on
    // C2.
    assert!(
        events(&path, OBSERVATION)?
            .iter()
            .any(|o| o["polarity"] == "absent"),
        "the absent fact is written"
    );
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn fifty_one_entries_are_delivered_fifty_with_their_description_critical_first_and_the_rest_by_id()
-> R {
    // Decision 10 (M2T18D-156, -200, -251): 51 Issues of 51 fingerprints on C1 (the first a low one): all 51 in C2's
    // verifiers' mandatory scope, the 50 critical ones with their description and in `confirm_ids`, the low one by id
    // only; confirmed fixed by QA, the 50 RESOLVED and the low one's confirmation dropped; the round passable (the low
    // one was delivered to neither); C2's next delivery would hold the low one alone. Two Issues of one fingerprint are
    // one entry, and confirming it resolves both.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    let mut findings = vec![finding("F00", "low", "低优先级：注释", &[])];
    findings.extend((1..=50).map(|n| {
        finding(
            &format!("F{n:02}"),
            "critical",
            &format!("缺陷 {n}：边界 {n}"),
            &[],
        )
    }));
    // The same defect again under another id: one more Issue of F01's fingerprint.
    findings.push(finding("G01", "critical", "缺陷 1：边界 1", &[]));
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &findings,
        &[],
    )
    .await?;
    reconcile(&store).await?;
    let report = child_id(&reviewer, "report");
    let id = |f: &str| child_id(&report, &format!("finding/{f}"));
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let r2 = child_id(&c2, "round/1");
    let delivery = deliver(&Connection::open(&path)?, "R", &r2, &["D".to_owned()])?;
    let all = delivery.lines.join("\n");
    for f in &findings {
        let f = f["id"].as_str().unwrap_or_default();
        assert!(all.contains(&id(f)), "{f} is not in the scope");
    }
    assert_eq!(
        delivery.confirm_ids.len(),
        51,
        "50 entries, F01's of two Issues"
    );
    assert!(!delivery.confirm_ids.contains(&id("F00")));
    assert!(delivery.confirm_ids.contains(&id("G01")));
    let listed = delivery
        .lines
        .iter()
        .find(|l| l.contains(&id("F00")))
        .ok_or("F00 is not listed")?;
    assert!(!listed.contains("注释"), "by id only: {listed}");
    let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
    assert_eq!(delivered_to(&path, &qa2)?.len(), 51);
    // The scope is the verifier's mandatory scope, never trimmed.
    let assigned = crate::persistence::team::assignment(
        &Connection::open(&path)?,
        "p1",
        "R",
        &child_id(&r2, "task/qa"),
        &qa2,
        "qa",
    )?;
    let scope = assigned.context.ok_or("no facts")?.scope.join("\n");
    assert!(scope.contains(&id("F00")) && scope.contains(&id("F50")));
    pass(&path, &qa2, &c2, "rec-2")?;
    let mut confirmations: Vec<Value> = (1..=50)
        .map(|n| confirm(&id(&format!("F{n:02}")), "RESOLVED", "已修复"))
        .collect();
    confirmations.push(confirm(&id("F00"), "RESOLVED", "已修复"));
    settle(&store, &path, &qa2, ("qa", "PASS"), &[], &confirmations).await?;
    settle(&store, &path, &reviewer2, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2.clone(), "passable".to_owned())]);
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || CAST(count(*) AS TEXT) FROM issues GROUP BY state ORDER BY state"
        )?,
        ["OPEN:1", "RESOLVED:51"]
    );
    assert_eq!(state_of(&path, &id("F00"))?, "OPEN");
    assert_eq!(state_of(&path, &id("G01"))?, "RESOLVED");
    let next = deliver(&Connection::open(&path)?, "R", &r2, &["D".to_owned()])?;
    assert_eq!(next.confirm_ids, [id("F00")]);
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn a_change_requests_issue_confirmed_fixed_after_its_rework_is_resolved() -> R {
    // Decision 10 (RESOLVED/REOPENED, M2T18D-50): C1's QA asks for a change (high): its Issue fails the round and D is
    // reworked; on C2 both verifiers are delivered it, the reviewer confirms it RESOLVED: RESOLVED at the passable
    // close (its `issue.opened` is a present fact on C1 only, earlier than C2's absent one).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    let (qa, reviewer) = start(&store, &path, &r1, &[]).await?;
    let issue = store
        .verifier_request(crate::persistence::VerifierRequest::RejectChange(
            crate::persistence::ChangeRequest {
                run_id: "R".to_owned(),
                attempt_id: qa.clone(),
                message_id: "m1".to_owned(),
                round_id: r1.clone(),
                candidate_id: c1.clone(),
                severity: "high".to_owned(),
                text: "空输入应返回错误".to_owned(),
            },
        ))
        .await?
        .ok_or("no issue")?;
    pass(&path, &qa, &c1, "rec-1")?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(&store, &path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
    assert_eq!(
        reconcile(&store).await?.closed,
        [(r1.clone(), "repair".to_owned())]
    );
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let r2 = child_id(&c2, "round/1");
    let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
    assert_eq!(delivered_to(&path, &qa2)?, std::slice::from_ref(&issue));
    pass(&path, &qa2, &c2, "rec-2")?;
    settle(&store, &path, &qa2, ("qa", "PASS"), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer2,
        ("reviewer", "PASS"),
        &[],
        &[confirm(&issue, "RESOLVED", "空输入返回 InvalidInput")],
    )
    .await?;
    assert_eq!(
        reconcile(&store).await?.closed,
        [(r2, "passable".to_owned())]
    );
    assert_eq!(state_of(&path, &issue)?, "RESOLVED");
    Ok(())
}

/// The materialised state of every Issue of run R agrees with the state function evaluated now (J6: after each
/// transaction).
pub(in crate::persistence) fn agrees(path: &Path) -> R {
    for (row, judged) in judged(&Connection::open(path)?, "R")? {
        assert_eq!(
            row.state == "RESOLVED",
            judged.resolved(),
            "{} is {} but judged {judged:?}",
            row.issue.id,
            row.state
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_issue_one_round_resolved_is_reopened_by_a_sibling_rounds_open_confirmation_on_the_same_candidate()
-> R {
    // Decision 10 (M2T18D-300, -351, J6): X (critical) was found by C2's round whose sources were D and E. On C3, D's
    // round R3 and E's round R4 are both delivered X before either closes. R3 closing first (passable, X confirmed
    // fixed: RESOLVED), R4's QA then confirms X OPEN: its settle makes X REOPENED (present on C3 wins over C3's absent)
    // and R4 fails on X. R4 settling before R3 closes: R3 does not resolve X (still present) and fails on it too. Either
    // way X ends unresolved and in the confirm set of what comes next, and after every transaction the materialised
    // states agree with the function.
    for r3_closes_first in [true, false] {
        siblings(r3_closes_first).await?;
    }
    Ok(())
}

async fn siblings(r3_closes_first: bool) -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    freeze_d(&store, &path, 0, 'b').await?;
    second_writer(&path, "E", "AE")?;
    let job = freeze_of('c');
    let c2 = job.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(job, result_of("E", "AE")?))
        })
        .await?;
    // C1's round, its verifiers never started, closes superseded.
    reconcile(&store).await?;
    let r2 = child_id(&c2, "round/1");
    let (qa, reviewer) = start(&store, &path, &r2, &[]).await?;
    evidence_of(
        &path,
        (&qa, &c2, "rec-2"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS"), ("AC-2-01", "PASS")],
        true,
    )?;
    settle(&store, &path, &qa, ("qa", "PASS"), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer,
        ("reviewer", "FAIL"),
        &[finding("F1", "critical", X_SUMMARY, &["src/a.rs"])],
        &[],
    )
    .await?;
    assert_eq!(
        reconcile(&store).await?.closed,
        [(r2.clone(), "repair".to_owned())]
    );
    let x = child_id(&child_id(&reviewer, "report"), "finding/F1");
    // D freezes C3 (R3); E, reworked too, completes the same content (R4 on C3).
    let c3 = freeze_d(&store, &path, 1, 'd').await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'E';
         INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, started_at)
           VALUES ('AE2', 'E', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:05Z');",
    )?;
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE2".to_owned(),
            ..completion(freeze(freeze_of('d'), result_of("E", "AE2")?))
        })
        .await?;
    let (r3, r4) = (child_id(&c3, "round/1"), child_id(&c3, "round/2"));
    let (qa3, reviewer3) = start(&store, &path, &r3, &[]).await?;
    assert_eq!(delivered_to(&path, &qa3)?, std::slice::from_ref(&x));
    pass(&path, &qa3, &c3, "rec-3")?;
    settle(
        &store,
        &path,
        &qa3,
        ("qa", "PASS"),
        &[],
        &[confirm(&x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(&store, &path, &reviewer3, ("reviewer", "PASS"), &[], &[]).await?;
    agrees(&path)?;
    // R4's verifiers are delivered X before R3 closes.
    let (qa4, reviewer4) = start(&store, &path, &r4, &[]).await?;
    assert_eq!(delivered_to(&path, &qa4)?, std::slice::from_ref(&x));
    if r3_closes_first {
        let done = reconcile(&store).await?;
        assert_eq!(done.closed, [(r3.clone(), "passable".to_owned())]);
        assert_eq!(state_of(&path, &x)?, "RESOLVED");
        agrees(&path)?;
    }
    evidence_of(
        &path,
        (&qa4, &c3, "rec-4"),
        ("unit", "PASS"),
        &[("AC-2-01", "PASS")],
        true,
    )?;
    settle(
        &store,
        &path,
        &qa4,
        ("qa", "PASS"),
        &[],
        &[confirm(&x, "OPEN", "空输入仍越界")],
    )
    .await?;
    let unresolved = if r3_closes_first { "REOPENED" } else { "OPEN" };
    assert_eq!(state_of(&path, &x)?, unresolved, "at R4's QA settle");
    agrees(&path)?;
    settle(&store, &path, &reviewer4, ("reviewer", "PASS"), &[], &[]).await?;
    agrees(&path)?;
    let done = reconcile(&store).await?;
    let fingerprint = acceptance::fingerprint("critical", X_SUMMARY, &["src/a.rs".to_owned()]);
    if r3_closes_first {
        assert_eq!(done.closed, [(r4.clone(), "repair".to_owned())]);
    } else {
        // R4's present fact exists when R3 closes: R3 does not resolve X and fails on it.
        assert_eq!(
            done.closed,
            [
                (r3.clone(), "repair".to_owned()),
                (r4.clone(), "repair".to_owned())
            ]
        );
        assert!(items(&path, &r3)?.contains(&format!("D:issue:{fingerprint}")));
    }
    assert_eq!(items(&path, &r4)?, [format!("E:issue:{fingerprint}")]);
    assert_eq!(state_of(&path, &x)?, unresolved);
    agrees(&path)?;
    let next = confirm_set(
        &Connection::open(&path)?,
        "R",
        &["D".to_owned()],
        "next",
        i64::MAX,
    )?;
    assert!(next.iter().any(|i| i.issue_id == x), "{next:?}");
    Ok(())
}

// The operation-level interleaving model of decision 10's state function (docs/adr/ADR-029.md test plan, "状态函数";
// M2T18D-502, -550, -600, -601, -602, -650, -651, -755, -850; M2T18R-51): two sibling rounds on one candidate, two
// fingerprints and one AC, driven by the store's own transactions (verifier settles, round closes, a freeze that
// supersedes, a run cancel's close, a veto midway, evidence recorded after its round closed, a change request) in every
// order a family of scripts allows. After each transaction the materialised Issue states agree with the state function
// evaluated directly; at the end each Issue is resolved alike in every order it exists in, as the design says.

/// Y's words: a high finding without a locator, so a change request in the same words has Y's fingerprint.
const Y_TEXT: &str = "解析器在空输入上越界";

/// The test AC-1-01 fails on (A's, and A3's).
const T: &str = "test_1_01_bounds";

/// One operation of a script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Op {
    /// R3's QA records its planned check (AC-1-01 PASS) and settles PASS, confirming X and Y fixed.
    Qa3,
    /// R3's reviewer settles PASS, confirming X and Y fixed.
    Reviewer3,
    /// R4's verifiers are dispatched (each role's one session is free once R3's settled), both delivered X and Y.
    Start4,
    /// R4's QA records its planned check (AC-2-01 PASS; AC-1-01, outside R4's scope, FAIL on T when the family says so)
    /// and settles, confirming X still OPEN and Y fixed.
    Qa4,
    /// R4's reviewer settles PASS, confirming Y fixed.
    Reviewer4,
    /// A verification reconcile: the rounds whose verifiers are done close, whatever their verdict.
    Close,
    /// R4's reviewer records a vetoed planned check (INVALID) on C3: C3 INVALID under verification, or vetoed though
    /// already REJECTED.
    Veto,
    /// Writer F completes C4: C3 superseded when still under verification, so its rounds close superseded.
    Supersede,
    /// The run's cancel requested (as its canceller records it): the rounds then close superseded.
    Cancel,
    /// R4's QA asks for a change in Y's words: a change request's Issue on C3, present for Y's fingerprint.
    ChangeRequest,
    /// R2's QA (its round long closed) records AC-1-01 PASS on C3: rows at once, on a candidate vetoed or REJECTED.
    LatePassC3,
    /// R2's QA records AC-1-01 FAIL of T on C3, a later candidate than A's.
    LateFailC3,
    /// R2's QA records AC-1-01 PASS on C4.
    LatePassC4,
}

/// The orders every script keeps: R4's verifiers are dispatched once R3's of their role settled (one session per role)
/// and before any reconcile, so each is delivered X and Y whatever comes next.
const DISPATCH: &[(Op, Op)] = &[
    (Op::Qa3, Op::Start4),
    (Op::Reviewer3, Op::Start4),
    (Op::Start4, Op::Qa4),
    (Op::Start4, Op::Reviewer4),
    (Op::Start4, Op::Close),
];

/// R3's QA settles before its reviewer (the families beyond the first vary what comes after).
const R3_FIRST: &[(Op, Op)] = &[(Op::Qa3, Op::Reviewer3)];

/// A family of scripts: its operations (one `Close` per reconcile; one more ends every script), which must come before
/// which, whether R4's QA fails AC-1-01 outside R4's scope, each Issue's final resolution as the design gives it, and
/// what at least one of its orders must reach.
struct Family<'a> {
    name: &'static str,
    ops: &'a [Op],
    before: &'static [&'static [(Op, Op)]],
    out_of_scope_fail: bool,
    resolved: &'static [(&'static str, bool)],
    reaches: &'static [&'static str],
}

/// Every order of the multiset `ops` in which each `(a, b)` of `before` has every `a` before any `b`.
fn orders(ops: &[Op], before: &[(Op, Op)]) -> Vec<Vec<Op>> {
    fn extend(
        left: &mut BTreeMap<Op, usize>,
        before: &[(Op, Op)],
        prefix: &mut Vec<Op>,
        out: &mut Vec<Vec<Op>>,
    ) {
        if left.values().all(|n| *n == 0) {
            out.push(prefix.clone());
            return;
        }
        let ready: Vec<Op> = left
            .iter()
            .filter(|(op, n)| {
                **n > 0
                    && before
                        .iter()
                        .all(|(a, b)| b != *op || left.get(a).copied().unwrap_or(0) == 0)
            })
            .map(|(op, _)| *op)
            .collect();
        for op in ready {
            *left.entry(op).or_default() -= 1;
            prefix.push(op);
            extend(left, before, prefix, out);
            prefix.pop();
            *left.entry(op).or_default() += 1;
        }
    }
    let mut left = BTreeMap::new();
    for op in ops {
        *left.entry(*op).or_default() += 1;
    }
    let mut out = Vec::new();
    extend(&mut left, before, &mut Vec::new(), &mut out);
    out
}

/// A check record of `producer` on `candidate`: outcome `status`, its rows `(AC, status)` (a FAIL failing its AC's
/// test) and its result lines.
fn recording(
    record: &str,
    (candidate, producer): (&str, &str),
    status: crate::verification::Status,
    rows: &[(&str, &'static str)],
) -> crate::persistence::Recording {
    let test = |ac: &str| match ac {
        "AC-1-01" => ("TEST-1-01", T),
        _ => ("TEST-2-01", "test_2_01_ok"),
    };
    let mut r = pass_recording(record, candidate, producer);
    r.outcome.status = status;
    if status == crate::verification::Status::Invalid {
        r.reasons = vec!["SOURCE_CHANGED"];
    }
    r.observations = rows
        .iter()
        .map(|(ac, status)| {
            json!({"schema_version": "1.0", "test_id": test(ac).0, "name": test(ac).1, "status": status})
        })
        .collect();
    r.rows = rows
        .iter()
        .map(|(ac, status)| crate::persistence::AcceptanceRow {
            ac_id: (*ac).to_owned(),
            spec_id: "s1".to_owned(),
            status,
            reason: format!("{} in check unit", test(ac).0),
            failing: if *status == "FAIL" {
                vec![test(ac).1.to_owned()]
            } else {
                Vec::new()
            },
        })
        .collect();
    r
}

/// Each Issue of run R that disagrees with the state function evaluated now.
fn disagreements(path: &Path) -> R<Vec<String>> {
    Ok(judged(&Connection::open(path)?, "R")?
        .into_iter()
        .filter(|(row, judged)| (row.state == "RESOLVED") != judged.resolved())
        .map(|(row, judged)| format!("{} is {} but judged {judged:?}", row.issue.id, row.state))
        .collect())
}

/// One script's world: C2's round R2 failed on X (critical, located), Y (high, no locator) and AC-1-01's FAIL on T (A);
/// D froze C3 (its round R3) and E the same content (R4), both delivered X and Y; R3's verifiers started; writer F
/// running.
struct Model {
    _dir: tempfile::TempDir,
    store: Store,
    path: PathBuf,
    c3: String,
    c4: String,
    r4: String,
    qa2: String,
    qa3: String,
    reviewer3: String,
    qa4: Option<String>,
    reviewer4: Option<String>,
    /// Issue id -> its name in the model.
    names: BTreeMap<String, String>,
    out_of_scope_fail: bool,
    /// The transactions so far, for a failure's message.
    trace: Vec<String>,
    /// What this script reached (a family's `reaches`).
    reached: BTreeSet<String>,
}

impl Model {
    /// The world every script starts from.
    async fn world() -> R<Model> {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        freeze_d(&store, &path, 0, 'b').await?;
        second_writer(&path, "E", "AE")?;
        let job = freeze_of('c');
        let c2 = job.sealed.candidate_id.clone();
        store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: "AE".to_owned(),
                ..completion(freeze(job, result_of("E", "AE")?))
            })
            .await?;
        reconcile(&store).await?;
        let r2 = child_id(&c2, "round/1");
        let (qa2, reviewer2) = start(&store, &path, &r2, &[]).await?;
        evidence_judged(
            &path,
            (&qa2, &c2, "rec-2"),
            ("unit", "FAIL"),
            &[
                ("AC-1-01", "TEST-1-01", "FAIL", vec![T]),
                ("AC-2-01", "TEST-2-01", "PASS", vec![]),
            ],
            &[
                ("TEST-1-01", T, "FAIL"),
                ("TEST-2-01", "test_2_01_ok", "PASS"),
            ],
        )?;
        settle(&store, &path, &qa2, ("qa", "FAIL"), &[], &[]).await?;
        settle(
            &store,
            &path,
            &reviewer2,
            ("reviewer", "FAIL"),
            &[
                finding("F1", "critical", X_SUMMARY, &["src/a.rs"]),
                finding("F2", "high", Y_TEXT, &[]),
            ],
            &[],
        )
        .await?;
        assert_eq!(
            reconcile(&store).await?.closed,
            [(r2.clone(), "repair".to_owned())]
        );
        let report = child_id(&reviewer2, "report");
        let names: BTreeMap<String, String> = [
            (child_id(&report, "finding/F1"), "X"),
            (child_id(&report, "finding/F2"), "Y"),
            (child_id(&c2, &format!("acceptance/AC-1-01/{T}")), "A"),
        ]
        .into_iter()
        .map(|(id, name)| (id, name.to_owned()))
        .collect();
        assert_eq!(
            texts(&path, "SELECT issue_id FROM issues ORDER BY issue_id")?,
            names.keys().cloned().collect::<Vec<_>>()
        );
        // D freezes C3 (R3); E, reworked too, completes the same content (R4 on C3); F, a third writer, runs.
        let c3 = freeze_d(&store, &path, 1, 'd').await?;
        Connection::open(&path)?.execute_batch(
            "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'E';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
               started_at) VALUES ('AE2', 'E', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:05Z');",
        )?;
        store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: "AE2".to_owned(),
                ..completion(freeze(freeze_of('d'), result_of("E", "AE2")?))
            })
            .await?;
        second_writer(&path, "F", "AF")?;
        let (r3, r4) = (child_id(&c3, "round/1"), child_id(&c3, "round/2"));
        let started = dispatch(&store, &path, &[]).await?;
        let task = |round: &str, role: &str| child_id(round, &format!("task/{role}"));
        assert_eq!(
            started.keys().cloned().collect::<BTreeSet<_>>(),
            [task(&r3, "qa"), task(&r3, "reviewer")].into(),
            "one session per role: R4's verifiers wait"
        );
        let (qa3, reviewer3) = (
            started[&task(&r3, "qa")].clone(),
            started[&task(&r3, "reviewer")].clone(),
        );
        let model = Model {
            _dir: dir,
            store,
            path,
            c3,
            c4: freeze_of('e').sealed.candidate_id,
            r4,
            qa2,
            qa3,
            reviewer3,
            qa4: None,
            reviewer4: None,
            names,
            out_of_scope_fail: false,
            trace: vec!["world".to_owned()],
            reached: BTreeSet::new(),
        };
        for attempt in [&model.qa3, &model.reviewer3] {
            model.delivered_x_and_y(attempt)?;
        }
        model.check()?;
        Ok(model)
    }

    /// A script's own copy of world `self` (its database copied and opened as a store), R4's QA failing AC-1-01 when
    /// `out_of_scope_fail`.
    fn copy(&self, out_of_scope_fail: bool) -> R<Model> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join(".hoh/state.db");
        std::fs::create_dir_all(dir.path().join(".hoh"))?;
        Connection::open(&self.path)?.execute(
            "VACUUM INTO ?1",
            [path.to_str().ok_or("a temporary path is UTF-8")?],
        )?;
        let store = Store::open(
            &path,
            crate::persistence::Options {
                wal_probe: Some(crate::persistence::selftest::test_probe(None)),
                ..crate::persistence::Options::default()
            },
        )?;
        Ok(Model {
            _dir: dir,
            store,
            path,
            c3: self.c3.clone(),
            c4: self.c4.clone(),
            r4: self.r4.clone(),
            qa2: self.qa2.clone(),
            qa3: self.qa3.clone(),
            reviewer3: self.reviewer3.clone(),
            qa4: None,
            reviewer4: None,
            names: self.names.clone(),
            out_of_scope_fail,
            trace: self.trace.clone(),
            reached: BTreeSet::new(),
        })
    }

    fn state_of_c3(&self) -> R<String> {
        Ok(texts(
            &self.path,
            &format!(
                "SELECT state FROM candidates WHERE candidate_id = '{}'",
                self.c3
            ),
        )?
        .remove(0))
    }

    fn id(&self, name: &str) -> String {
        self.names
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(id, _)| id.clone())
            .unwrap_or_default()
    }

    fn delivered_x_and_y(&self, attempt: &str) -> R {
        let mut want = vec![self.id("X"), self.id("Y")];
        want.sort();
        assert_eq!(delivered_to(&self.path, attempt)?, want, "{attempt}");
        Ok(())
    }

    /// After a transaction: the materialised states agree with the function (J6).
    fn check(&self) -> R {
        let wrong = disagreements(&self.path)?;
        assert!(
            wrong.is_empty(),
            "after {}: {wrong:?}",
            self.trace.join(" > ")
        );
        Ok(())
    }

    fn done(&mut self, what: String) -> R {
        self.trace.push(what);
        self.check()
    }

    async fn record(&mut self, record: crate::persistence::Recording) -> R {
        let what = format!("record {}", record.record.record_id);
        self.store.record_evidence(record).await?;
        self.done(what)
    }

    async fn settle(
        &mut self,
        attempt: &str,
        (role, status): (&str, &str),
        confirmations: &[(&str, &str)],
    ) -> R {
        let issues: Vec<Value> = confirmations
            .iter()
            .map(|(name, state)| confirm(&self.id(name), state, "复核"))
            .collect();
        settle(
            &self.store,
            &self.path,
            attempt,
            (role, status),
            &[],
            &issues,
        )
        .await?;
        self.done(format!("settle {attempt}"))
    }

    async fn close(&mut self) -> R {
        let before: BTreeSet<String> = texts(&self.path, "SELECT issue_id FROM issues")?
            .into_iter()
            .collect();
        let done = reconcile(&self.store).await?;
        let frozen_c4 = !texts(
            &self.path,
            &format!(
                "SELECT state FROM candidates WHERE candidate_id = '{}'",
                self.c4
            ),
        )?
        .is_empty();
        let a3 = child_id(&self.c3, &format!("acceptance/AC-1-01/{T}"));
        for id in texts(&self.path, "SELECT issue_id FROM issues")? {
            if before.contains(&id) {
                continue;
            }
            assert_eq!(id, a3, "the only Issue a close opens here");
            self.names.insert(id, "A3".to_owned());
            self.reached.insert(format!(
                "A3 opened {}",
                if frozen_c4 { "after C4" } else { "before C4" }
            ));
        }
        let cancelled = texts(&self.path, "SELECT state FROM runs")? == ["CANCEL_REQUESTED"];
        for (round, verdict) in &done.closed {
            let name = match round.strip_prefix(&self.c3) {
                Some("/round/1") => "R3",
                Some("/round/2") => "R4",
                _ => round,
            };
            let by = if cancelled { " by the cancel" } else { "" };
            self.reached.insert(format!("{name} closed {verdict}{by}"));
        }
        self.done(format!("close {:?}", done.closed))
    }

    async fn apply(&mut self, op: Op) -> R {
        use crate::verification::Status;
        let qa4 = || self.qa4.clone().ok_or("R4's QA is not dispatched");
        match op {
            Op::Qa3 => {
                let qa3 = self.qa3.clone();
                self.record(recording(
                    "qa3-check",
                    (&self.c3, &qa3),
                    Status::Pass,
                    &[("AC-1-01", "PASS")],
                ))
                .await?;
                self.settle(
                    &qa3,
                    ("qa", "PASS"),
                    &[("X", "RESOLVED"), ("Y", "RESOLVED")],
                )
                .await
            }
            Op::Reviewer3 => {
                let reviewer3 = self.reviewer3.clone();
                self.settle(
                    &reviewer3,
                    ("reviewer", "PASS"),
                    &[("X", "RESOLVED"), ("Y", "RESOLVED")],
                )
                .await
            }
            Op::Start4 => {
                let started = dispatch(&self.store, &self.path, &[]).await?;
                let task = |role: &str| child_id(&self.r4, &format!("task/{role}"));
                assert_eq!(
                    started.keys().cloned().collect::<BTreeSet<_>>(),
                    [task("qa"), task("reviewer")].into(),
                    "after {}",
                    self.trace.join(" > ")
                );
                let (qa4, reviewer4) = (
                    started[&task("qa")].clone(),
                    started[&task("reviewer")].clone(),
                );
                self.delivered_x_and_y(&qa4)?;
                self.delivered_x_and_y(&reviewer4)?;
                (self.qa4, self.reviewer4) = (Some(qa4), Some(reviewer4));
                self.done("dispatch R4".to_owned())
            }
            Op::Qa4 => {
                let qa4 = qa4()?;
                let mut rows = vec![("AC-2-01", "PASS")];
                if self.out_of_scope_fail {
                    rows.push(("AC-1-01", "FAIL"));
                }
                let status = if self.out_of_scope_fail {
                    Status::Fail
                } else {
                    Status::Pass
                };
                self.record(recording("qa4-check", (&self.c3, &qa4), status, &rows))
                    .await?;
                self.settle(
                    &qa4,
                    ("qa", status.code()),
                    &[("X", "OPEN"), ("Y", "RESOLVED")],
                )
                .await
            }
            Op::Reviewer4 => {
                let reviewer4 = self
                    .reviewer4
                    .clone()
                    .ok_or("R4's reviewer is not dispatched")?;
                self.settle(&reviewer4, ("reviewer", "PASS"), &[("Y", "RESOLVED")])
                    .await
            }
            Op::Close => self.close().await,
            Op::Veto => {
                let reviewer4 = self
                    .reviewer4
                    .clone()
                    .ok_or("R4's reviewer is not dispatched")?;
                let state = self.state_of_c3()?;
                self.reached.insert(format!("veto on {state}"));
                self.record(recording(
                    "veto",
                    (&self.c3, &reviewer4),
                    Status::Invalid,
                    &[],
                ))
                .await
            }
            Op::Supersede => {
                self.store
                    .complete_writer_attempt(WriterCompletion {
                        attempt_id: "AF".to_owned(),
                        ..completion(freeze(freeze_of('e'), result_of("F", "AF")?))
                    })
                    .await?;
                self.done("C4 frozen".to_owned())
            }
            Op::Cancel => {
                Connection::open(&self.path)?.execute(
                    "UPDATE runs SET state = 'CANCEL_REQUESTED', revision = revision + 1 WHERE run_id = 'R'",
                    [],
                )?;
                self.done("cancel requested".to_owned())
            }
            Op::ChangeRequest => {
                let qa4 = qa4()?;
                let issue = self
                    .store
                    .verifier_request(crate::persistence::VerifierRequest::RejectChange(
                        crate::persistence::ChangeRequest {
                            run_id: "R".to_owned(),
                            attempt_id: qa4.clone(),
                            message_id: "m-change".to_owned(),
                            round_id: self.r4.clone(),
                            candidate_id: self.c3.clone(),
                            severity: "high".to_owned(),
                            text: Y_TEXT.to_owned(),
                        },
                    ))
                    .await?
                    .ok_or("a change request opens an Issue")?;
                self.names.insert(issue, "CR".to_owned());
                self.done("change request".to_owned())
            }
            Op::LatePassC3 | Op::LateFailC3 | Op::LatePassC4 => {
                if op == Op::LatePassC3 {
                    let state = self.state_of_c3()?;
                    self.reached.insert(format!("late PASS on {state}"));
                }
                let (record, candidate, status, row) = match op {
                    Op::LatePassC3 => ("late-pass-c3", self.c3.clone(), Status::Pass, "PASS"),
                    Op::LateFailC3 => ("late-fail-c3", self.c3.clone(), Status::Fail, "FAIL"),
                    _ => ("late-pass-c4", self.c4.clone(), Status::Pass, "PASS"),
                };
                let qa2 = self.qa2.clone();
                self.record(recording(
                    record,
                    (&candidate, &qa2),
                    status,
                    &[("AC-1-01", row)],
                ))
                .await
            }
        }
    }

    /// Each named Issue's final state: resolved or not.
    fn resolved(&self) -> R<BTreeMap<String, bool>> {
        let mut out = BTreeMap::new();
        for (id, name) in &self.names {
            out.insert(name.clone(), state_of(&self.path, id)? == "RESOLVED");
        }
        Ok(out)
    }
}

/// Run every order of family `f`, each from its own copy of the world: each transaction checked, each Issue resolved
/// alike in every order it exists in and as the family says, every `reaches` reached by some order.
async fn run_family(f: &Family<'_>) -> R {
    let before: Vec<(Op, Op)> = std::iter::once(DISPATCH)
        .chain(f.before.iter().copied())
        .flatten()
        .copied()
        .collect();
    let all = orders(f.ops, &before);
    let world = Model::world().await?;
    // Issue name -> resolved -> the orders that ended so.
    let mut finals: BTreeMap<String, BTreeMap<bool, Vec<usize>>> = BTreeMap::new();
    let mut reached = BTreeSet::new();
    let mut reopened = 0;
    for (n, order) in all.iter().enumerate() {
        let mut model = world.copy(f.out_of_scope_fail)?;
        for op in order {
            model.apply(*op).await?;
        }
        model.close().await?;
        for (name, resolved) in model.resolved()? {
            finals
                .entry(name)
                .or_default()
                .entry(resolved)
                .or_default()
                .push(n);
        }
        reopened += events(&model.path, "issue.reopened")?.len();
        reached.extend(model.reached);
    }
    for (name, by) in &finals {
        assert_eq!(
            by.len(),
            1,
            "{}: {name} ends resolved in some orders and not in others: {:?}",
            f.name,
            by.iter()
                .map(|(r, orders)| (
                    r,
                    orders.iter().take(3).map(|n| &all[*n]).collect::<Vec<_>>()
                ))
                .collect::<Vec<_>>()
        );
    }
    // Every Issue but A3 (opened only by a close that classified R4) exists in every order.
    for (name, by) in finals.iter().filter(|(name, _)| *name != "A3") {
        assert_eq!(
            by.values().map(Vec::len).sum::<usize>(),
            all.len(),
            "{}: {name} exists in every order",
            f.name
        );
    }
    for (name, want) in f.resolved {
        assert_eq!(
            finals.get(*name).map(|by| by.contains_key(want)),
            Some(true),
            "{}: {name} ends resolved = {want}: {finals:?}",
            f.name
        );
    }
    for what in f.reaches {
        assert!(
            reached.contains(*what) || (*what == "reopened" && reopened > 0),
            "{}: no order reaches {what}: {reached:?}",
            f.name
        );
    }
    println!(
        "{}: {} orders, {reopened} reopenings, reached {reached:?}",
        f.name,
        all.len()
    );
    Ok(())
}

/// R3's and R4's settles, R4's dispatch and two reconciles (closes).
const SIBLINGS: [Op; 7] = [
    Op::Qa3,
    Op::Reviewer3,
    Op::Start4,
    Op::Qa4,
    Op::Reviewer4,
    Op::Close,
    Op::Close,
];

#[tokio::test]
async fn interleaved_sibling_settles_and_closes_agree_with_the_state_function_after_every_transaction()
-> R {
    // M2T18D-300, -351, -502, -550 (J6): R3 confirms X and Y fixed, R4 confirms Y fixed and X still OPEN, in every
    // order of the settles and closes: X resolved at R3's close and reopened by R4's QA, or never resolved (R3 failing
    // on it); Y resolved by the absent facts of either close; A by R3's PASS rows on C3 whatever R3's verdict.
    run_family(&Family {
        name: "siblings",
        ops: &SIBLINGS,
        before: &[],
        out_of_scope_fail: false,
        resolved: &[("X", false), ("Y", true), ("A", true)],
        reaches: &["reopened", "R3 closed passable", "R3 closed repair"],
    })
    .await
}

#[tokio::test]
async fn interleaved_veto_midway_and_a_late_pass_on_the_vetoed_candidate_agree_after_every_transaction()
-> R {
    // M2T18D-601, -602, -651, -755: R4's reviewer records a vetoed check on C3 while C3 is under verification (INVALID)
    // or already REJECTED by R3; R4's QA also fails AC-1-01, outside R4's scope; after its round closed, R2's QA records
    // AC-1-01 PASS on C3 (a PASS row on the vetoed, perhaps REJECTED, candidate). Nothing C3 observed resolves anything.
    let mut ops = SIBLINGS.to_vec();
    ops.extend([Op::Veto, Op::LatePassC3]);
    run_family(&Family {
        name: "veto",
        ops: &ops,
        before: &[
            R3_FIRST,
            &[
                (Op::Start4, Op::Veto),
                (Op::Veto, Op::Reviewer4),
                (Op::Veto, Op::LatePassC3),
            ],
        ],
        out_of_scope_fail: true,
        resolved: &[("X", false), ("Y", false), ("A", false)],
        reaches: &[
            "reopened",
            "veto on VERIFYING",
            "veto on REJECTED",
            "late PASS on REJECTED",
            "R3 closed superseded",
            "R4 closed invalidated",
        ],
    })
    .await
}

#[tokio::test]
async fn interleaved_supersession_and_a_late_acceptance_issue_agree_after_every_transaction() -> R {
    // M2T18D-502, -550, -650, -850: writer F freezes C4 before or after R3 and R4 close (they close superseded while C3
    // is under verification, or classified once R3 rejected it, R4 then opening C3's acceptance Issue A3 after C4's
    // freeze); R4's QA fails AC-1-01 on T outside its scope; after its round closed, R2's QA records AC-1-01 PASS on
    // C4. Y is resolved by C3's absent facts whatever the closes' verdicts; A and A3 by C4's rows.
    let mut ops = SIBLINGS.to_vec();
    ops.extend([Op::Supersede, Op::LatePassC4]);
    run_family(&Family {
        name: "supersession",
        ops: &ops,
        before: &[
            R3_FIRST,
            &[(Op::Start4, Op::Supersede), (Op::Supersede, Op::LatePassC4)],
        ],
        out_of_scope_fail: true,
        resolved: &[("X", false), ("Y", true), ("A", true), ("A3", true)],
        reaches: &[
            "reopened",
            "R3 closed superseded",
            "R4 closed superseded",
            "R4 closed repair",
            "A3 opened after C4",
        ],
    })
    .await
}

#[tokio::test]
async fn interleaved_change_request_and_a_run_cancels_close_agree_after_every_transaction() -> R {
    // M2T18D-701, -800, -850: R4's QA asks for a change in Y's words (its Issue a present fact of Y's fingerprint on
    // C3) before or after R3's close; the run's cancel is requested at any point after R4's dispatch, so the rounds
    // close superseded or classified. Y ends unresolved (resolved by R3's close and reopened by the change request, or
    // never resolved), as does the change request's Issue; A is resolved by R3's PASS rows whatever R3's verdict.
    let mut ops = SIBLINGS.to_vec();
    ops.extend([Op::ChangeRequest, Op::Cancel]);
    run_family(&Family {
        name: "change request and cancel",
        ops: &ops,
        before: &[
            R3_FIRST,
            &[
                (Op::Start4, Op::ChangeRequest),
                (Op::ChangeRequest, Op::Qa4),
                (Op::Start4, Op::Cancel),
            ],
        ],
        out_of_scope_fail: false,
        resolved: &[("X", false), ("Y", false), ("A", true), ("CR", false)],
        reaches: &[
            "reopened",
            "R3 closed superseded by the cancel",
            "R4 closed superseded by the cancel",
            "R3 closed repair",
        ],
    })
    .await
}

#[tokio::test]
async fn interleaved_late_fail_on_a_newer_candidate_agrees_after_every_transaction() -> R {
    // M2T18D-600, -650, -850: after its round closed, R2's QA records AC-1-01 FAIL on T on C3 (a later candidate than
    // A's), before or after R3's PASS rows: A is not resolved, or resolved and reopened.
    let mut ops = SIBLINGS.to_vec();
    ops.push(Op::LateFailC3);
    run_family(&Family {
        name: "late fail",
        ops: &ops,
        before: &[R3_FIRST],
        out_of_scope_fail: false,
        resolved: &[("X", false), ("Y", true), ("A", false)],
        reaches: &["reopened"],
    })
    .await
}
