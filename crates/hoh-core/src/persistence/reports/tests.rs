//! A verifier's report taken in against a real database (docs/adr/ADR-029.md decisions 7 and 8).

use super::*;
use crate::persistence::completion::tests::{
    R, completion, consistent, freeze, freeze_of, fresh_objects, open, result_of, second_writer,
    texts, verifier_completion, verifier_running, verifiers, verifying,
};
use crate::persistence::{
    Store, VerifierCompleted, VerifierOutcome, VerifierReport, WriterCompletion,
};
use rusqlite::Connection;
use std::path::Path;

/// The two READY verifier tasks of the run dispatched and started: (role, attempt) of each.
async fn start_verifiers(store: &Store, path: &Path) -> R<Vec<(String, String)>> {
    let mut started = Vec::new();
    for _ in 0..2 {
        let d = store
            .schedule_round("R", verifiers())
            .await?
            .dispatched
            .ok_or("nothing dispatched")?;
        let role = if d.task_id.ends_with("/qa") {
            "qa"
        } else {
            "reviewer"
        };
        Connection::open(path)?.execute_batch(&format!(
            "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = '{0}';
             UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{1}';",
            d.attempt_id, d.task_id
        ))?;
        started.push((role.to_owned(), d.attempt_id));
    }
    Ok(started)
}

/// Settle verifier attempt `attempt` of `role` with a report `status` consistent with it.
async fn settle(
    store: &Store,
    path: &Path,
    attempt: &str,
    role: &str,
    status: &str,
) -> R<VerifierCompleted> {
    let report = consistent(path, attempt, (role, status), fresh_objects(path)?)?;
    Ok(store
        .complete_verifier_attempt(verifier_completion(
            attempt,
            VerifierOutcome::Settle {
                report: Box::new(report),
            },
        ))
        .await?)
}

/// The rows of `independent_reports` as JSON objects, in insertion order.
fn rows(path: &Path) -> R<Vec<Value>> {
    texts(
        path,
        "SELECT json_object('report_id', report_id, 'run_id', run_id, 'candidate_id', candidate_id, 'role', role, \
         'session_id', session_id, 'spec_sha256', spec_sha256, 'status', status, 'state', state, 'reject_reason', \
         reject_reason, 'attempt_id', attempt_id, 'report_artifact_id', report_artifact_id, \
         'raw_session_artifact_id', raw_session_artifact_id) FROM independent_reports ORDER BY rowid",
    )?
    .iter()
    .map(|r| Ok(serde_json::from_str(r)?))
    .collect()
}

/// The payloads of the events of kind `kind`, in order.
fn events(path: &Path, kind: &str) -> R<Vec<Value>> {
    texts(
        path,
        &format!("SELECT payload_json FROM events WHERE kind = '{kind}' ORDER BY seq"),
    )?
    .iter()
    .map(|p| Ok(serde_json::from_str(p)?))
    .collect()
}

/// The codes of `reasons` (`CODE: detail`).
fn codes(reasons: &[String]) -> Vec<&str> {
    reasons
        .iter()
        .map(|r| r.split_once(':').map_or(r.as_str(), |(code, _)| code))
        .collect()
}

/// C1's round 1 with its qa verifier's attempt RUNNING and the report of that attempt consistent with it: the store,
/// the database, the attempt and the intake.
async fn qa_intake(dir: &Path) -> R<(Store, std::path::PathBuf, String, ReportIntake)> {
    let (store, path) = open(dir)?;
    let (_, _, attempt) = verifier_running(&store, &path).await?;
    let intake =
        consistent(&path, &attempt, ("qa", "PASS"), fresh_objects(&path)?)?.intake(&attempt);
    Ok((store, path, attempt, intake))
}

/// `intake` taken in on its own; it must be REJECTED: its reasons, after checking its row and its `report.rejected`.
async fn rejected(store: &Store, path: &Path, intake: ReportIntake) -> R<Vec<String>> {
    let done = store.record_report(intake).await?;
    assert_eq!(done.state, ReportState::Rejected, "{done:?}");
    let row = rows(path)?.pop().ok_or("no row")?;
    assert_eq!(row["state"], "REJECTED");
    assert_eq!(row["reject_reason"], json!(done.reasons.join("; ")));
    assert_eq!(
        events(path, "report.rejected")?,
        [
            json!({"report_id": done.report_id, "attempt_id": row["attempt_id"], "reasons": done.reasons,
            "report": {"path": row["report_artifact_id"].as_str().and_then(|a| a.split_once(':'))
                .map(|(_, p)| p), "sha256": row["report_artifact_id"].as_str()
                .and_then(|a| a.split_once(':')).map(|(s, _)| s)}})
        ]
    );
    Ok(done.reasons)
}

#[test]
fn the_accepted_native_statuses_are_the_release_policys() -> R {
    // Decision 7: the store's set is a constant read with the code, the locked policy's own (contracts are not read at
    // run time); this keeps the two the same.
    let policy: Value =
        serde_json::from_str(include_str!("../../../../../contracts/release-policy.json"))?;
    assert_eq!(
        policy["accepted_attempt_statuses"],
        json!(ACCEPTED_ATTEMPT_STATUSES)
    );
    Ok(())
}

#[tokio::test]
async fn test_054_01_reports_in_conflicting_sessions_are_rejected() -> R {
    // AC-054-01 (REQ-054 behaviour 3, failure mode 1; decisions 7 and 7a): S_cmd (codex, the commander's binding),
    // S_dev (claude, the source task's developer attempt), S_qa (pi, the qa binding) and S_rev (codex, the reviewer's
    // binding). The reviewer task has four attempts, one in each; each takes in a report built under the locked schema.
    // Three are REJECTED for a session conflict, each with its report.rejected; the one in S_rev is CURRENT; S_rev's
    // session is a reviewer's; no rejected report has an Issue.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('S_cmd', 'codex', 'commander', '1', '{}'), ('S_dev', 'claude', 'developer', '1', '{}'),
           ('S_qa', 'pi', 'qa', '1', '{}'), ('S_rev', 'codex', 'reviewer', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'codex', 'S_cmd', 1, 1, '2026-11-12T00:00:00Z');
         UPDATE role_bindings SET session_id = 'S_qa' WHERE run_id = 'R' AND role = 'qa';
         UPDATE role_bindings SET session_id = 'S_rev' WHERE run_id = 'R' AND role = 'reviewer';
         UPDATE attempts SET session_id = 'S_dev' WHERE attempt_id = 'A';",
    )?;
    let round = verifying(&store).await?;
    let reviewer = child_id(&round, "task/reviewer");
    let mut states = Vec::new();
    for session in ["S_cmd", "S_dev", "S_qa", "S_rev"] {
        let attempt = child_id(&reviewer, &format!("attempt-{session}"));
        Connection::open(&path)?.execute(
            "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision) \
             VALUES (?1, ?2, ?3, 'SETTLED', 1, 1, 1)",
            rusqlite::params![attempt, reviewer, session],
        )?;
        let intake = consistent(&path, &attempt, ("reviewer", "PASS"), fresh_objects(&path)?)?
            .intake(&attempt);
        let done = store.record_report(intake).await?;
        states.push((session, done));
    }
    let reports = rows(&path)?;
    let sessions = texts(
        &path,
        "SELECT json_object('session_id', session_id, 'agent', agent, 'role', role) FROM sessions \
         ORDER BY session_id",
    )?;
    let rejections = events(&path, "report.rejected")?;
    println!(
        "{}",
        json!({"independent_reports": reports, "sessions": sessions.iter()
            .map(|s| serde_json::from_str::<Value>(s).unwrap_or_default()).collect::<Vec<_>>(),
            "report.rejected": rejections})
    );
    for (session, done) in &states {
        let row = reports
            .iter()
            .find(|r| r["session_id"] == json!(session))
            .ok_or("no row")?;
        if *session == "S_rev" {
            assert_eq!(done.state, ReportState::Current, "{done:?}");
            assert_eq!(row["state"], "CURRENT");
            assert_eq!(row["reject_reason"], Value::Null);
        } else {
            assert_eq!(done.state, ReportState::Rejected, "{session}: {done:?}");
            assert_eq!(row["state"], "REJECTED", "{session}");
            assert!(
                row["reject_reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("session conflict")),
                "{session}: {row}"
            );
            assert_eq!(
                rejections
                    .iter()
                    .filter(|e| e["report_id"] == row["report_id"])
                    .count(),
                1,
                "{session}"
            );
        }
    }
    assert_eq!(
        reports.iter().filter(|r| r["state"] == "REJECTED").count(),
        3
    );
    assert_eq!(rejections.len(), 3);
    assert_eq!(
        texts(
            &path,
            "SELECT role FROM sessions WHERE session_id = 'S_rev'"
        )?,
        ["reviewer"]
    );
    assert_eq!(texts(&path, "SELECT issue_id FROM issues")?.len(), 0);
    Ok(())
}

#[tokio::test]
async fn test_054_02_a_report_on_a_superseded_candidate_is_stale() -> R {
    // AC-054-02 at the store (REQ-054 behaviour 4, failure mode 2; decision 8): C1's verifiers run when E's completion
    // freezes C2 (D taken into C2's round). Their reports on C1 arrive afterwards: their rows are STALE (and kept), C1's
    // round closes superseded, and C2 has exactly one reviewer handoff.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    let c1 = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    let started = start_verifiers(&store, &path).await?;
    second_writer(&path, "E", "AE")?;
    let second = freeze_of('c');
    let c2 = second.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(second, result_of("E", "AE")?))
        })
        .await?;
    for (role, attempt) in &started {
        assert_eq!(
            settle(&store, &path, attempt, role, "PASS").await?,
            VerifierCompleted::Settled
        );
    }
    let stale = events(&path, "report.stale")?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    let reports = rows(&path)?;
    let handoffs = texts(
        &path,
        &format!(
            "SELECT message_id FROM outbox WHERE recipient_role = 'reviewer' AND json_extract(payload_json, '$.kind') \
             = 'handoff' AND json_extract(payload_json, '$.candidate_id') = '{c2}'"
        ),
    )?;
    println!(
        "{}",
        json!({"independent_reports": reports, "report.stale": stale, "closed": done.closed,
            "c2_reviewer_handoffs": handoffs})
    );
    assert_eq!(
        reports
            .iter()
            .map(|r| format!(
                "{}:{}:{}",
                r["role"].as_str().unwrap_or_default(),
                r["state"].as_str().unwrap_or_default(),
                r["candidate_id"].as_str().unwrap_or_default()
            ))
            .collect::<Vec<_>>(),
        started
            .iter()
            .map(|(role, _)| format!("{role}:STALE:{c1}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        stale
            .iter()
            .map(|e| (e["report_ids"].clone(), e["reason"].clone()))
            .collect::<Vec<_>>(),
        reports
            .iter()
            .map(|r| (json!([r["report_id"]]), json!("superseded")))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.state') FROM events WHERE kind = 'verification.report' ORDER BY seq"
        )?,
        ["STALE", "STALE"]
    );
    assert_eq!(done.closed, [(round, "superseded".to_owned())]);
    assert_eq!(handoffs.len(), 1, "{handoffs:?}");
    // C2's review read before its reviewer completes (decision 5's `report_status`): NOT_RUN, C1's STALE PASS counting
    // for nothing.
    let snapshot = store.reader()?.acceptance("R")?.ok_or("no run R")?;
    assert_eq!(
        (
            snapshot.candidate.map(|c| c.candidate_id),
            snapshot.review,
            snapshot.qa
        ),
        (
            Some(c2.clone()),
            crate::verification::classify::Report::NotRun,
            crate::verification::classify::Report::NotRun
        )
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM independent_reports WHERE candidate_id = '{c1}'")
        )?,
        ["STALE", "STALE"],
        "C1's reports are kept"
    );
    Ok(())
}

#[tokio::test]
async fn a_report_whose_role_is_not_its_tasks_is_rejected_report_role_mismatch_first() -> R {
    // Decision 7: the reasons in their fixed order; the row's reject_reason is `CODE: detail` joined by "; ".
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    intake.report["role"] = json!("reviewer");
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(
        codes(&reasons),
        ["REPORT_ROLE_MISMATCH", "SESSION_ROLE_MISMATCH"]
    );
    assert!(
        reasons[1].starts_with("SESSION_ROLE_MISMATCH: session conflict:"),
        "{reasons:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_report_in_a_session_other_than_its_attempts_is_rejected_report_session_mismatch() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sq2', 'pi', 'qa', '1', '{}');",
    )?;
    intake.report["session_id"] = json!("sq2");
    intake.native_session_ids = vec!["sq2".to_owned()];
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["REPORT_SESSION_MISMATCH"]);
    Ok(())
}

#[tokio::test]
async fn a_report_in_a_session_of_another_role_is_rejected_session_role_mismatch() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path, attempt, mut intake) = qa_intake(dir.path()).await?;
    Connection::open(&path)?.execute_batch(&format!(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sx', 'pi', 'researcher', '1', '{{}}');
         UPDATE attempts SET session_id = 'sx' WHERE attempt_id = '{attempt}';"
    ))?;
    intake.report["session_id"] = json!("sx");
    intake.native_session_ids = vec!["sx".to_owned()];
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["SESSION_ROLE_MISMATCH"]);
    assert!(
        reasons[0].starts_with("SESSION_ROLE_MISMATCH: session conflict:"),
        "{reasons:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_report_in_a_conflicting_session_is_rejected_session_not_independent() -> R {
    // Decision 7a: the commander's binding is one of the sessions a verifier may not use.
    let dir = tempfile::tempdir()?;
    let (store, path, attempt, mut intake) = qa_intake(dir.path()).await?;
    Connection::open(&path)?.execute_batch(&format!(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sq2', 'pi', 'qa', '1', '{{}}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'pi', 'sq2', 1, 1, '2026-11-12T00:00:00Z');
         UPDATE attempts SET session_id = 'sq2' WHERE attempt_id = '{attempt}';"
    ))?;
    intake.report["session_id"] = json!("sq2");
    intake.native_session_ids = vec!["sq2".to_owned()];
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["SESSION_NOT_INDEPENDENT"]);
    assert!(
        reasons[0].starts_with("SESSION_NOT_INDEPENDENT: session conflict:"),
        "{reasons:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_report_on_another_candidate_is_rejected_and_its_row_keeps_the_rounds() -> R {
    // Decision 7 (M2T18D-853): the rejected row stores the round's candidate (a foreign key), the report's in the
    // reason's detail.
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    let c1 = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    let other = format!("CAND-{}", "e".repeat(64));
    intake.report["candidate_id"] = json!(other);
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["REPORT_CANDIDATE_MISMATCH"]);
    assert!(reasons[0].contains(&other), "{reasons:?}");
    assert_eq!(rows(&path)?[0]["candidate_id"], json!(c1));
    Ok(())
}

#[tokio::test]
async fn a_report_under_another_specification_is_rejected_report_spec_mismatch() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    intake.report["spec_sha256"] = json!("2".repeat(64));
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["REPORT_SPEC_MISMATCH"]);
    Ok(())
}

#[tokio::test]
async fn a_report_naming_other_objects_than_those_given_is_rejected_report_objects_mismatch() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    intake.raw_artifact.path = ".hoh/attempts/elsewhere/stdout.jsonl".to_owned();
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["REPORT_OBJECTS_MISMATCH"]);
    Ok(())
}

#[tokio::test]
async fn a_report_whose_native_attempt_did_not_complete_is_rejected_native_attempt_incomplete() -> R
{
    // Decision 7 (-400, -450): an export SETTLED (the status before M2-T18) is not one the release policy accepts.
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    intake.native_status = "SETTLED".to_owned();
    let reasons = rejected(&store, &path, intake).await?;
    assert_eq!(codes(&reasons), ["NATIVE_ATTEMPT_INCOMPLETE"]);
    Ok(())
}

#[tokio::test]
async fn a_report_whose_native_sessions_are_a_developers_is_rejected_native_session_not_independent()
-> R {
    // Decision 7 (docs/10 §5, -162, -256): the native attempt's sessions meet a source attempt's native session, as
    // its session row names it or as its turn's session.native recorded it.
    for how in ["session row", "session.native"] {
        let dir = tempfile::tempdir()?;
        let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
        let sql = if how == "session row" {
            "UPDATE sessions SET native_id = 'native-dev' WHERE session_id = 'sd';"
        } else {
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', \
             'A/session.native', 'R', 'session.native', \
             '{\"session_id\": \"sd\", \"native_id\": \"native-dev\", \"attempt_id\": \"A\"}', '2026-11-12T00:00:02Z');"
        };
        Connection::open(&path)?.execute_batch(sql)?;
        intake.native_session_ids.push("native-dev".to_owned());
        let reasons = rejected(&store, &path, intake).await?;
        assert_eq!(codes(&reasons), ["NATIVE_SESSION_NOT_INDEPENDENT"], "{how}");
        assert!(
            reasons[0].starts_with("NATIVE_SESSION_NOT_INDEPENDENT: session conflict:"),
            "{how}: {reasons:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_report_taken_in_again_is_a_no_op_and_with_other_content_a_key_reuse() -> R {
    // Decision 7: report_id = child_id(attempt, "report"); the same content again writes nothing and gives its state,
    // other content is KeyReuse.
    let dir = tempfile::tempdir()?;
    let (store, path, attempt, intake) = qa_intake(dir.path()).await?;
    let first = store.record_report(intake.clone()).await?;
    assert_eq!(
        first,
        IntakeState {
            report_id: Some(child_id(&attempt, "report")),
            state: ReportState::Current,
            reasons: Vec::new(),
        }
    );
    let before = texts(&path, "SELECT CAST(max(seq) AS TEXT) FROM events")?;
    assert_eq!(store.record_report(intake.clone()).await?, first);
    assert_eq!(
        texts(&path, "SELECT CAST(max(seq) AS TEXT) FROM events")?,
        before
    );
    assert_eq!(rows(&path)?.len(), 1);
    let mut other = intake.clone();
    other.report_artifact = fresh_objects(&path)?[0].clone();
    let again = store.record_report(other).await;
    assert!(
        matches!(again, Err(crate::persistence::PersistError::KeyReuse(_))),
        "{again:?}"
    );
    assert_eq!(rows(&path)?.len(), 1);
    // A REJECTED one again: the same reasons, no second report.rejected.
    let dir = tempfile::tempdir()?;
    let (store, path, _, mut intake) = qa_intake(dir.path()).await?;
    intake.native_status = "SETTLED".to_owned();
    let first = store.record_report(intake.clone()).await?;
    assert_eq!(store.record_report(intake).await?, first);
    assert_eq!(events(&path, "report.rejected")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_report_whose_session_is_unknown_has_no_row_only_its_rejection() -> R {
    // Decision 7: the session is not in `sessions` (the foreign key would refuse the row): no row, only
    // report.rejected{report_id: null, reasons: ["SESSION_UNKNOWN: ..."]}; taken in again, nothing more.
    let dir = tempfile::tempdir()?;
    let (store, path, attempt, mut intake) = qa_intake(dir.path()).await?;
    intake.report["session_id"] = json!("nobody");
    intake.native_session_ids = vec!["nobody".to_owned()];
    let done = store.record_report(intake.clone()).await?;
    assert_eq!(done.report_id, None);
    assert_eq!(done.state, ReportState::Rejected);
    assert_eq!(codes(&done.reasons), ["SESSION_UNKNOWN"]);
    assert_eq!(rows(&path)?.len(), 0);
    let rejections = events(&path, "report.rejected")?;
    assert_eq!(rejections.len(), 1);
    assert_eq!(rejections[0]["report_id"], Value::Null);
    assert_eq!(rejections[0]["attempt_id"], json!(attempt));
    assert_eq!(rejections[0]["reasons"], json!(done.reasons));
    assert_eq!(store.record_report(intake).await?, done);
    assert_eq!(events(&path, "report.rejected")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_rejected_report_settles_its_attempt_and_blocks_its_verifier_for_a_decision() -> R {
    // Decision 7's exit (as SESSION_NOT_INDEPENDENT at the dispatch, ADR-028 decision 10): the attempt SETTLED, the
    // verifier task BLOCKED (NEED_DECISION) with integrity_violation{attempt_id, report_id, reasons} and
    // need_decision{reason: report_rejected} in the same commit, no Issue, and the round does not close.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, task, attempt) = verifier_running(&store, &path).await?;
    let mut report: VerifierReport =
        consistent(&path, &attempt, ("qa", "FAIL"), fresh_objects(&path)?)?;
    report.report_json["session_id"] = json!("sr");
    let done = store
        .complete_verifier_attempt(verifier_completion(
            &attempt,
            VerifierOutcome::Settle {
                report: Box::new(report),
            },
        ))
        .await?;
    let VerifierCompleted::ReportRejected { reasons } = done else {
        return Err(format!("not rejected: {done:?}").into());
    };
    assert_eq!(
        codes(&reasons),
        [
            "REPORT_SESSION_MISMATCH",
            "SESSION_ROLE_MISMATCH",
            "SESSION_NOT_INDEPENDENT"
        ]
    );
    let report_id = child_id(&attempt, "report");
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'")
        )?,
        ["SETTLED"]
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = '{task}'")
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    let integrity = events(&path, "integrity_violation")?;
    assert_eq!(integrity.len(), 1);
    assert_eq!(integrity[0]["reason"], "report_rejected");
    assert_eq!(integrity[0]["attempt_id"], json!(attempt));
    assert_eq!(integrity[0]["report_id"], json!(report_id));
    assert_eq!(integrity[0]["reasons"], json!(reasons));
    assert_eq!(
        events(&path, "need_decision")?
            .iter()
            .map(|e| (e["task_id"].clone(), e["reason"].clone()))
            .collect::<Vec<_>>(),
        [(json!(task), json!("report_rejected"))]
    );
    assert_eq!(events(&path, "report.rejected")?.len(), 1);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.state') || ':' || json_extract(payload_json, '$.report_id') FROM \
             events WHERE kind = 'verification.report'"
        )?,
        [format!("REJECTED:{report_id}")]
    );
    assert_eq!(rows(&path)?[0]["state"], "REJECTED");
    assert_eq!(texts(&path, "SELECT issue_id FROM issues")?.len(), 0);
    // The reviewer settles; the round does not close (its qa task is not done).
    let d = store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .ok_or("the reviewer was not dispatched")?;
    Connection::open(&path)?.execute_batch(&format!(
        "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = '{0}';
         UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{1}';",
        d.attempt_id, d.task_id
    ))?;
    assert_eq!(
        settle(&store, &path, &d.attempt_id, "reviewer", "PASS").await?,
        VerifierCompleted::Settled
    );
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert!(done.closed.is_empty(), "{done:?}");
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT event_id FROM events WHERE kind = 'verification.round_closed' AND \
                 json_extract(payload_json, '$.round_id') = '{round}'"
            )
        )?
        .len(),
        0
    );
    Ok(())
}

#[tokio::test]
async fn a_report_in_an_unknown_session_settles_its_attempt_and_blocks_its_verifier() -> R {
    // Decision 7: SESSION_UNKNOWN takes the same exit, with no row (its verification.report names none).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (_, task, attempt) = verifier_running(&store, &path).await?;
    let mut report = consistent(&path, &attempt, ("qa", "PASS"), fresh_objects(&path)?)?;
    report.report_json["session_id"] = json!("nobody");
    let done = store
        .complete_verifier_attempt(verifier_completion(
            &attempt,
            VerifierOutcome::Settle {
                report: Box::new(report),
            },
        ))
        .await?;
    assert!(
        matches!(&done, VerifierCompleted::ReportRejected { reasons } if codes(reasons) == ["SESSION_UNKNOWN"]),
        "{done:?}"
    );
    assert_eq!(rows(&path)?.len(), 0);
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = '{task}'")
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'")
        )?,
        ["SETTLED"]
    );
    let report = events(&path, "verification.report")?;
    assert_eq!(
        (report[0]["state"].clone(), report[0]["report_id"].clone()),
        (json!("REJECTED"), Value::Null)
    );
    Ok(())
}

#[tokio::test]
async fn a_freeze_makes_the_other_candidates_current_reports_stale_in_one_event() -> R {
    // Decision 8: C1's qa report is CURRENT; E's completion freezes C2, which makes it STALE in its transaction with
    // one report.stale{report_ids, reason: superseded, by: C2}. A third freeze, with nothing CURRENT, writes none.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    verifying(&store).await?;
    let started = start_verifiers(&store, &path).await?;
    let (role, attempt) = started.iter().find(|(r, _)| r == "qa").ok_or("no qa")?;
    settle(&store, &path, attempt, role, "PASS").await?;
    assert_eq!(rows(&path)?[0]["state"], "CURRENT");
    assert_eq!(events(&path, "report.stale")?.len(), 0);
    second_writer(&path, "E", "AE")?;
    let second = freeze_of('c');
    let c2 = second.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(second, result_of("E", "AE")?))
        })
        .await?;
    assert_eq!(rows(&path)?[0]["state"], "STALE");
    assert_eq!(
        events(&path, "report.stale")?,
        [json!({"report_ids": [child_id(attempt, "report")], "reason": "superseded", "by": c2})]
    );
    second_writer(&path, "F", "AF")?;
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AF".to_owned(),
            ..completion(freeze(freeze_of('d'), result_of("F", "AF")?))
        })
        .await?;
    assert_eq!(
        texts(
            &path,
            "SELECT state FROM candidates WHERE state = 'FROZEN' OR state = 'VERIFYING'"
        )?
        .len(),
        1
    );
    assert_eq!(events(&path, "report.stale")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_reconcile_makes_stale_a_current_report_whose_candidate_no_longer_stands() -> R {
    // Decision 8's sweep (-60): at the start of each reconcile, whoever wrote the fact. Nothing to flip writes nothing;
    // a candidate.stale naming C1 flips its CURRENT report once (reason candidate_stale, by reconcile); a later freeze
    // that no transaction flipped for (a database of before) flips with reason superseded.
    for (fact, reason) in [
        ("candidate.stale", "candidate_stale"),
        ("candidate.frozen", "superseded"),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        verifying(&store).await?;
        let c1 = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
        let started = start_verifiers(&store, &path).await?;
        let (role, attempt) = started.iter().find(|(r, _)| r == "qa").ok_or("no qa")?;
        settle(&store, &path, attempt, role, "PASS").await?;
        store
            .reconcile_verification("R", Default::default())
            .await?;
        assert_eq!(events(&path, "report.stale")?.len(), 0, "{fact}");
        let named = if fact == "candidate.stale" {
            c1.clone()
        } else {
            format!("CAND-{}", "e".repeat(64))
        };
        Connection::open(&path)?.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
             ?2, ?3, '2026-11-12T00:00:09Z')",
            rusqlite::params![
                format!("fixture/{fact}"),
                fact,
                json!({"candidate_id": named, "superseded": [], "reason": "fixture"}).to_string()
            ],
        )?;
        for _ in 0..2 {
            store
                .reconcile_verification("R", Default::default())
                .await?;
        }
        assert_eq!(rows(&path)?[0]["state"], "STALE", "{fact}");
        assert_eq!(
            events(&path, "report.stale")?,
            [
                json!({"report_ids": [child_id(attempt, "report")], "reason": reason, "by": "reconcile"})
            ],
            "{fact}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_rounds_classification_reads_its_stale_reports_and_never_its_rejected_ones() -> R {
    // Decision 7 (who reads which state; ADR-028 -850): a round reads the last non-REJECTED report of each role among
    // its attempts. QA's FAIL read as STALE fails the round (repair); the same report REJECTED is no report (NOT_RUN):
    // the round is inconclusive (verified again) and has no report-fail item.
    for (state, verdict) in [("STALE", "repair"), ("REJECTED", "reverify")] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        let round = verifying(&store).await?;
        for (role, attempt) in start_verifiers(&store, &path).await? {
            let status = if role == "qa" { "FAIL" } else { "PASS" };
            settle(&store, &path, &attempt, &role, status).await?;
        }
        Connection::open(&path)?.execute(
            "UPDATE independent_reports SET state = ?1, reject_reason = CASE WHEN ?1 = 'REJECTED' \
             THEN 'REPORT_SPEC_MISMATCH: forged' END WHERE role = 'qa'",
            rusqlite::params![state],
        )?;
        let done = store
            .reconcile_verification("R", Default::default())
            .await?;
        assert_eq!(done.closed, [(round, verdict.to_owned())], "{state}");
        let items = texts(
            &path,
            "SELECT json_extract(i.value, '$.item') FROM events e, json_each(e.payload_json, '$.items') i \
             WHERE e.kind = 'verification.failures'",
        )?;
        assert_eq!(
            items.iter().any(|i| i.starts_with("report-fail:qa")),
            state == "STALE",
            "{state}: {items:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_verifier_is_not_dispatched_in_the_developers_session_or_one_the_other_verifier_used() -> R
{
    // Decision 7a: the dispatch applies the intake's conflict set, which adds the developer's binding and the sessions
    // of the round's other verifier task's attempts to ADR-028's (stricter: by construction the product never meets
    // them). Either way the qa verifier task is refused SESSION_NOT_INDEPENDENT, BLOCKED for a decision, no attempt.
    for (how, sql) in [
        (
            "the developer's binding",
            "UPDATE role_bindings SET session_id = 'sq2' WHERE run_id = 'R' AND role = 'developer';",
        ),
        (
            "the other verifier's attempt",
            "INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision) \
             SELECT 'forged', json_extract(payload_json, '$.tasks.reviewer'), 'sq2', 'FAILED', 1, 1, 1 FROM events \
             WHERE kind = 'verification.round_opened';",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        let round = verifying(&store).await?;
        Connection::open(&path)?.execute_batch(&format!(
            "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
               ('sq2', 'pi', 'qa', '1', '{{}}');
             UPDATE role_bindings SET session_id = 'sq2' WHERE run_id = 'R' AND role = 'qa';
             {sql}"
        ))?;
        let qa = child_id(&round, "task/qa");
        while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
            assert_ne!(d.task_id, qa, "{how}");
        }
        assert_eq!(
            texts(
                &path,
                &format!("SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = '{qa}'")
            )?,
            ["BLOCKED:NEED_DECISION"],
            "{how}"
        );
        assert_eq!(
            texts(
                &path,
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'integrity_violation'"
            )?,
            ["SESSION_NOT_INDEPENDENT"],
            "{how}"
        );
    }
    Ok(())
}
