//! A writer attempt's completion against a real database (docs/adr/ADR-028.md decisions 3 and 4).

use super::*;
use crate::persistence::{
    ArtifactRow, ChangeRequest, Deferred, Options, Reconciled, Store, VerifierReport,
    VerifierRequest, selftest,
};
use crate::verification::{FileRef, Manifest};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Run R on specification s1 (whose sha256 is the freeze's spec object), developer task D RUNNING with attempt A holding
/// the write lease, its requirement REQ-1 with one criterion, and the qa and reviewer roles bound.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-12T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%.64c', '1'), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-12T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-1', 's1', 'r', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES ('AC-1-01', 'REQ-1', 's1', 'b', 'unit');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{}'), ('sq', 'pi', 'qa', '1', '{}'), ('sr', 'codex', 'reviewer', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'qa', 'pi', 'sq', 1, 1, '2026-11-12T00:00:00Z'),
  ('R', 'reviewer', 'codex', 'sr', 1, 1, '2026-11-12T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('D', 'R', 's1', 'RUNNING', 4, 'developer', 1, 1);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-1', 's1');
INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
  ('A', 'D', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:01Z');
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
";

fn open(dir: &Path) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    let store = Store::open(&path, options)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    Ok((store, path))
}

fn texts(path: &Path, sql: &str) -> R<Vec<String>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

/// Whether the writer lease is released: it expires at the attempt's end (leases::release_held_by).
fn released(path: &Path) -> R<&'static str> {
    let expires = texts(path, "SELECT expires_at FROM leases WHERE kind = 'writer'")?;
    let ended = crate::time::rfc3339_utc(ending().at);
    Ok(if expires == [ended] {
        "the write lease expires at the attempt's end"
    } else {
        "the write lease is still held"
    })
}

fn object(n: u8, kind: &str, name: &str) -> ArtifactRow {
    let sha256: String = format!("{n:x}").repeat(64).chars().take(64).collect();
    ArtifactRow {
        path: format!(".hoh/objects/sha256/{}/{sha256}", &sha256[..2]),
        sha256,
        bytes: 10 + u64::from(n),
        mime: "application/json".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

/// A freeze of tree `tree` for run R: spec, environment and test plan kept as objects 1, 2 and 3, the export as 9.
fn freeze_of(tree: char) -> Freeze {
    let file = |path: &str, n: u8| FileRef {
        path: path.to_owned(),
        sha256: object(n, "", "").sha256,
    };
    let sealed = Manifest {
        git_commit: "a".repeat(40),
        git_tree: tree.to_string().repeat(40),
        spec: file("contracts/requirements.json", 1),
        environment: file(".hoh/environment.json", 2),
        test_plan: file("tests/check-plan.json", 3),
        source_files: vec![file("src/lib.rs", 4)],
    }
    .seal();
    Freeze {
        run_id: "R".to_owned(),
        sealed,
        export: object(9, "candidate", "candidate.json"),
        inputs: vec![
            object(1, "spec", "contracts/requirements.json"),
            object(2, "environment", ".hoh/environment.json"),
            object(3, "test_plan", "tests/check-plan.json"),
        ],
    }
}

fn result(candidate_id: Option<&str>) -> R<TaskResult> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "D", "attempt_id": "A", "role": "developer",
        "role_epoch": 1, "status": "COMPLETED", "summary": "done", "candidate_id": candidate_id, "evidence": [],
        "issues": [], "next_action": "verify"
    }))?)
}

fn ending() -> Ending {
    Ending {
        success: true,
        detail: "turn".to_owned(),
        class: crate::retry::FailureClass::Transient { retry_after: None },
        at: std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000),
        draw: 0,
        observed: Default::default(),
        unknown_effect: false,
    }
}

fn completion(outcome: WriterOutcome) -> WriterCompletion {
    WriterCompletion {
        attempt_id: "A".to_owned(),
        ending: ending(),
        outcome,
    }
}

fn freeze(job: Freeze, result: TaskResult) -> WriterOutcome {
    WriterOutcome::Freeze {
        job: Box::new(job),
        result: Box::new(result),
        self_tests: SelfTests::default(),
    }
}

#[tokio::test]
async fn a_completed_result_freezes_accepts_and_settles_in_one_commit() -> R {
    // Decision 4: the candidate row, its acceptance (task and candidate VERIFYING, candidate_ready and the two handoffs
    // in one event's transaction) and the attempt's settle (SETTLED, the write lease released) commit together.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of('b');
    let id = job.sealed.candidate_id.clone();
    let done = store
        .complete_writer_attempt(completion(freeze(job, result(None)?)))
        .await?;
    let WriterCompleted::Verifying {
        candidate_id,
        handoffs,
    } = done
    else {
        return Err(format!("{done:?}").into());
    };
    assert_eq!((candidate_id.as_str(), handoffs.len()), (id.as_str(), 2));
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["VERIFYING"]);
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["VERIFYING"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
        ["SETTLED"]
    );
    assert_eq!(
        released(&path)?,
        "the write lease expires at the attempt's end"
    );
    let seqs = texts(
        &path,
        "SELECT DISTINCT CAST(event_seq AS TEXT) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'handoff'",
    )?;
    assert_eq!(
        seqs.len(),
        1,
        "both handoffs in candidate_ready's transaction"
    );
    // Asked again (a reconcile after the commit): nothing more.
    let again = store
        .complete_writer_attempt(completion(freeze(freeze_of('b'), result(None)?)))
        .await?;
    assert_eq!(again, WriterCompleted::AlreadyEnded);
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'candidate_ready'"
        )?
        .len(),
        1
    );
    Ok(())
}

#[tokio::test]
async fn a_completions_self_test_records_are_registered_as_its_attempts_and_referenced_by_its_handoffs()
-> R {
    // docs/adr/ADR-028.md decision 14: the records the host kept are registered in the completion's commit, an
    // `attempt.artifacts` of their own (kind self_test) with the items only referenced; the round's handoffs reference
    // them as the developer attempt's products.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let kept = object(7, "self_test", ".hoh/self-tests/A/report.txt");
    let outcome = WriterOutcome::Freeze {
        job: Box::new(freeze_of('c')),
        result: Box::new(result(None)?),
        self_tests: SelfTests {
            items: vec![kept.clone()],
            failed: vec![
                json!({"name": ".hoh/self-tests/A/gone.txt", "sha256": "e".repeat(64),
                "error": "its bytes hash to another digest"}),
            ],
        },
    };
    let done = store.complete_writer_attempt(completion(outcome)).await?;
    assert!(
        matches!(done, WriterCompleted::Verifying { .. }),
        "{done:?}"
    );
    let event = texts(
        &path,
        "SELECT payload_json FROM events WHERE event_id = 'A/artifacts/self_tests' AND kind = 'attempt.artifacts'",
    )?;
    let [event] = event.as_slice() else {
        return Err(format!("{event:?}").into());
    };
    let event: serde_json::Value = serde_json::from_str(event)?;
    assert_eq!(event["attempt_id"], "A");
    assert_eq!(event["items"][0]["kind"], "self_test");
    assert_eq!(event["items"][0]["sha256"], kept.sha256.as_str());
    assert_eq!(event["failed"][0]["name"], ".hoh/self-tests/A/gone.txt");
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT path FROM artifacts WHERE sha256 = '{}'",
                kept.sha256
            )
        )?,
        std::slice::from_ref(&kept.path)
    );
    for package in texts(
        &path,
        "SELECT payload_json FROM outbox WHERE json_extract(payload_json, '$.kind') = 'handoff'",
    )? {
        let package: serde_json::Value = serde_json::from_str(&package)?;
        let references = &package["payload"]["references"];
        assert!(
            references
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r["sha256"] == kept.sha256.as_str())),
            "{references}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_output_to_correct_is_a_counted_correction_until_its_limit_then_a_decision() -> R {
    // Decision 3: a correction settles the attempt and sends the task back to READY, counted by Repair::Protocol keyed
    // by the chain's first attempt; past max_protocol_repairs the task is BLOCKED for a decision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let correct = || WriterOutcome::Correct {
        reasons: vec!["no TaskResult in the bridge".to_owned()],
    };
    let done = store.complete_writer_attempt(completion(correct())).await?;
    assert_eq!(
        done,
        WriterCompleted::Corrected {
            verdict: RepairVerdict::Granted { count: 1 }
        }
    );
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["READY"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
        ["SETTLED"]
    );
    assert_eq!(
        released(&path)?,
        "the write lease expires at the attempt's end"
    );
    // Two more attempts of the chain, each completing with an output to correct.
    for (n, attempt) in [(2, "A2"), (3, "A3")] {
        Connection::open(&path)?.execute_batch(&format!(
            "UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'D';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
               VALUES ('{attempt}', 'D', 'sd', 'RUNNING', 1, 1, 1);"
        ))?;
        let done = store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: attempt.to_owned(),
                ..completion(correct())
            })
            .await?;
        let want = if n == 2 {
            WriterCompleted::Corrected {
                verdict: RepairVerdict::Granted { count: 2 },
            }
        } else {
            WriterCompleted::Corrected {
                verdict: RepairVerdict::Refused { count: 2 },
            }
        };
        assert_eq!(done, want, "attempt {attempt}");
    }
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.root') FROM events WHERE kind = 'task.protocol_correction'"
        )?,
        ["A", "A"],
        "one chain, keyed by its first attempt"
    );
    Ok(())
}

#[tokio::test]
async fn an_outcome_that_needs_a_decision_blocks_the_task_and_settles_the_attempt() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let done = store
        .complete_writer_attempt(completion(WriterOutcome::Decide {
            reason: "task_result_blocked".to_owned(),
            detail: "no credentials for the registry".to_owned(),
        }))
        .await?;
    assert_eq!(
        done,
        WriterCompleted::Blocked {
            reason: "task_result_blocked".to_owned()
        }
    );
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
        ["SETTLED"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision'"
        )?,
        ["task_result_blocked"]
    );
    assert_eq!(
        released(&path)?,
        "the write lease expires at the attempt's end"
    );
    Ok(())
}

#[tokio::test]
async fn content_already_judged_or_another_candidate_id_is_corrected_not_frozen_anew() -> R {
    // Decision 3: same content whose candidate is STALE (or INVALID, REJECTED, ACCEPTED) is not revived; an agent's
    // candidate id other than the freeze's is refused; both corrections, nothing frozen or accepted.
    for case in ["stale", "other id"] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        let job = freeze_of('c');
        let id = job.sealed.candidate_id.clone();
        let given = if case == "stale" {
            Connection::open(&path)?.execute(
                "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
                   test_plan_sha256, source_path, state, created_at) VALUES (?1, 'R', printf('%.40c', 'a'),
                   printf('%.40c', 'c'), printf('%.64c', '1'), printf('%.64c', '2'), printf('%.64c', '3'), 'p', 'STALE',
                   '2026-11-12T00:00:00Z')",
                [&id],
            )?;
            None
        } else {
            Some("CAND-someone-elses")
        };
        let done = store
            .complete_writer_attempt(completion(freeze(job, result(given)?)))
            .await?;
        assert!(
            matches!(
                done,
                WriterCompleted::Corrected {
                    verdict: RepairVerdict::Granted { count: 1 }
                }
            ),
            "{case}: {done:?}"
        );
        assert_eq!(
            texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
            ["READY"],
            "{case}"
        );
        assert!(
            texts(
                &path,
                "SELECT kind FROM events WHERE kind = 'candidate_ready'"
            )?
            .is_empty(),
            "{case}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_task_without_criteria_or_a_run_without_its_verifiers_is_decided_before_any_freeze() -> R
{
    // Decision 3 (M2T16D-353, -1055): accepting would be refused whole (no AC to verify; no qa or reviewer bound), so
    // the completion decides first: the task BLOCKED for a decision, the attempt settled and its write lease released,
    // nothing frozen and no other candidate made STALE.
    for (case, setup, reason) in [
        (
            "no criteria",
            "DELETE FROM task_requirements WHERE task_id = 'D';",
            "no_acceptance_criteria",
        ),
        (
            "no reviewer",
            "DELETE FROM role_bindings WHERE role = 'reviewer';",
            "verification_team_missing",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        Connection::open(&path)?.execute_batch(&format!(
            "{setup}
             INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
               test_plan_sha256, source_path, state, created_at) VALUES ('C0', 'R', printf('%.40c', 'a'),
               printf('%.40c', 'd'), printf('%.64c', '1'), printf('%.64c', '2'), printf('%.64c', '3'), 'p', 'VERIFYING',
               '2026-11-12T00:00:00Z');"
        ))?;
        let done = store
            .complete_writer_attempt(completion(freeze(freeze_of('e'), result(None)?)))
            .await?;
        assert_eq!(
            done,
            WriterCompleted::Blocked {
                reason: reason.to_owned()
            },
            "{case}"
        );
        assert_eq!(
            texts(&path, "SELECT candidate_id || ':' || state FROM candidates")?,
            ["C0:VERIFYING"],
            "{case}: nothing frozen, nothing made STALE"
        );
        assert_eq!(
            texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
            ["SETTLED"],
            "{case}"
        );
        assert_eq!(
            released(&path)?,
            "the write lease expires at the attempt's end",
            "{case}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_cancelled_attempt_is_neither_frozen_nor_judged_its_cancel_takes_precedence() -> R {
    // ADR-021 decision 8e: a cancel takes precedence over how the work ended. A run being cancelled freezes nothing,
    // accepts nothing and counts no correction: the attempt ends CANCELLED and its task is the cancel's to move.
    for outcome in [
        freeze(freeze_of('b'), result(None)?),
        WriterOutcome::Correct {
            reasons: vec!["no TaskResult".to_owned()],
        },
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        Connection::open(&path)?
            .execute_batch("UPDATE attempts SET cancel_requested = 1 WHERE attempt_id = 'A';")?;
        // An interrupted writer's end carries its workspace's record (REQ-019 behaviour 6).
        let mut job = completion(outcome);
        job.ending.observed.workspace = Some(json!({"head": "a".repeat(40)}));
        let done = store.complete_writer_attempt(job).await?;
        assert_eq!(done, WriterCompleted::Cancelled);
        assert_eq!(
            texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
            ["CANCELLED"]
        );
        assert_eq!(
            texts(&path, "SELECT candidate_id FROM candidates")?.len(),
            0
        );
        assert_eq!(
            texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
            ["RUNNING"]
        );
        assert_eq!(
            texts(
                &path,
                "SELECT kind FROM events WHERE kind IN ('candidate_ready', 'task.protocol_correction') OR kind LIKE 'repair.%'"
            )?
            .len(),
            0
        );
    }
    Ok(())
}

#[tokio::test]
async fn only_a_completion_handed_to_verification_records_its_task_implemented() -> R {
    // ADR-025 decision 3's shared rule (task.implemented, on which intensive supervision wakes) under ADR-028 decision
    // 3: a writer's settle that sends its task back for a correction or blocks it for a decision implemented nothing.
    let implemented = |path: &Path| -> R<usize> {
        Ok(texts(
            path,
            "SELECT event_id FROM events WHERE kind = 'task.implemented'",
        )?
        .len())
    };
    for (outcome, want) in [
        (freeze(freeze_of('b'), result(None)?), 1),
        (
            WriterOutcome::Correct {
                reasons: vec!["no TaskResult".to_owned()],
            },
            0,
        ),
        (
            WriterOutcome::Decide {
                reason: "agent_incomplete".to_owned(),
                detail: "{}".to_owned(),
            },
            0,
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(dir.path())?;
        let done = store.complete_writer_attempt(completion(outcome)).await?;
        assert_eq!(implemented(&path)?, want, "{done:?}");
    }
    Ok(())
}

/// Developer task `task` of run R RUNNING with attempt `attempt`, its requirement REQ-2 (criterion AC-2-01).
fn second_writer(path: &Path, task: &str, attempt: &str) -> R {
    Connection::open(path)?.execute_batch(&format!(
        "INSERT OR IGNORE INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-2', 's1', 'r2', 1);
         INSERT OR IGNORE INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-2-01', 'REQ-2', 's1', 'b2', 'unit');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('{task}', 'R', 's1', 'RUNNING', 2, 'developer', 1, (SELECT max(created_seq) + 1 FROM tasks));
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{task}', 'REQ-2', 's1');
         INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, started_at)
           VALUES ('{attempt}', '{task}', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:02Z');"
    ))?;
    Ok(())
}

/// [`result`] of task `task`'s attempt `attempt`.
fn result_of(task: &str, attempt: &str) -> R<TaskResult> {
    let mut result = result(None)?;
    (result.task_id, result.attempt_id) = (task.to_owned(), attempt.to_owned());
    Ok(result)
}

/// The rounds opened, in order: (round id, candidate, number, source tasks, criteria, verifier tasks, handoffs).
fn rounds(path: &Path) -> R<Vec<serde_json::Value>> {
    texts(
        path,
        "SELECT payload_json FROM events WHERE kind = 'verification.round_opened' ORDER BY seq",
    )?
    .iter()
    .map(|p| Ok(serde_json::from_str(p)?))
    .collect()
}

#[tokio::test]
async fn a_completion_opens_its_candidates_first_round_with_its_two_runtime_verifier_tasks() -> R {
    // Decision 9: the completion's commit opens round 1 on the candidate: its source task, its criteria, two verifier
    // tasks (qa and reviewer: readers, layer 0, READY, no task_requirements) and the two handoffs, whose ids are the
    // round's, in candidate_ready's transaction.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of('b');
    let c1 = job.sealed.candidate_id.clone();
    let done = store
        .complete_writer_attempt(completion(freeze(job, result(None)?)))
        .await?;
    let round = crate::ids::child_id(&c1, "round/1");
    let id = |suffix: &str| crate::ids::child_id(&round, suffix);
    assert_eq!(
        done,
        WriterCompleted::Verifying {
            candidate_id: c1.clone(),
            handoffs: vec![id("verify/qa"), id("verify/reviewer")],
        }
    );
    assert_eq!(
        rounds(&path)?,
        [json!({
            "round_id": round, "candidate_id": c1, "number": 1, "source_task_ids": ["D"],
            "acceptance_ids": ["AC-1-01"], "tasks": {"qa": id("task/qa"), "reviewer": id("task/reviewer")},
            "handoffs": [id("verify/qa"), id("verify/reviewer")],
        })]
    );
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT task_id || ':' || role || ':' || writer || ':' || layer || ':' || state || ':' || spec_id \
                 FROM tasks WHERE task_id IN ('{}', '{}') ORDER BY role",
                id("task/qa"),
                id("task/reviewer")
            )
        )?,
        [
            format!("{}:qa:0:0:READY:s1", id("task/qa")),
            format!("{}:reviewer:0:0:READY:s1", id("task/reviewer"))
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT task_id FROM task_requirements WHERE task_id != 'D'"
        )?
        .len(),
        0
    );
    assert_eq!(
        texts(
            &path,
            "SELECT message_id FROM outbox WHERE event_seq = (SELECT seq FROM events WHERE kind = 'candidate_ready') \
             ORDER BY message_id"
        )?,
        [id("verify/qa"), id("verify/reviewer")]
    );
    Ok(())
}

#[tokio::test]
async fn a_new_candidate_takes_the_superseded_candidates_verifying_sources_into_its_round() -> R {
    // Decision 9's supersession: D is VERIFYING in C1's round 1 when E's completion freezes C2 (C1 STALE). C2's round 1
    // has both sources (one pair of handoffs, AC-053-04), the union of their criteria, D not run again and not moved.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let first = freeze_of('b');
    let c1 = first.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(completion(freeze(first, result(None)?)))
        .await?;
    let d = texts(
        &path,
        "SELECT state || ':' || revision FROM tasks WHERE task_id = 'D'",
    )?;
    second_writer(&path, "E", "AE")?;
    let second = freeze_of('c');
    let c2 = second.sealed.candidate_id.clone();
    let done = store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(second, result_of("E", "AE")?))
        })
        .await?;
    assert!(
        matches!(done, WriterCompleted::Verifying { .. }),
        "{done:?}"
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM candidates WHERE candidate_id = '{c1}'")
        )?,
        ["STALE"]
    );
    let opened = rounds(&path)?;
    assert_eq!(opened.len(), 2);
    assert_eq!(opened[1]["candidate_id"], json!(c2));
    assert_eq!(opened[1]["number"], json!(1));
    assert_eq!(opened[1]["source_task_ids"], json!(["D", "E"]));
    assert_eq!(opened[1]["acceptance_ids"], json!(["AC-1-01", "AC-2-01"]));
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || revision FROM tasks WHERE task_id = 'D'"
        )?,
        d,
        "D is neither run again nor moved"
    );
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'E'")?,
        ["VERIFYING"]
    );
    // One pair of handoffs for the round, enveloped by its first source (M2T16D-659).
    let round = crate::ids::child_id(&c2, "round/1");
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT json_extract(payload_json, '$.task_id') FROM outbox WHERE message_id LIKE '{round}/%' \
                 ORDER BY message_id"
            )
        )?,
        ["D", "D"]
    );
    Ok(())
}

#[tokio::test]
async fn the_same_content_under_verification_gets_a_new_round_for_the_completing_task_only() -> R {
    // Decision 3's row (M2T16D-101): E's completion freezes the content of C1, which is VERIFYING: no new candidate,
    // round 2 on C1 with E alone; D stays in round 1.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let first = freeze_of('b');
    let c1 = first.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(completion(freeze(first, result(None)?)))
        .await?;
    second_writer(&path, "E", "AE")?;
    let done = store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(freeze_of('b'), result_of("E", "AE")?))
        })
        .await?;
    let round = crate::ids::child_id(&c1, "round/2");
    assert_eq!(
        done,
        WriterCompleted::Verifying {
            candidate_id: c1.clone(),
            handoffs: vec![
                crate::ids::child_id(&round, "verify/qa"),
                crate::ids::child_id(&round, "verify/reviewer")
            ],
        }
    );
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["VERIFYING"]);
    let opened = rounds(&path)?;
    assert_eq!(
        opened
            .iter()
            .map(|r| (r["number"].clone(), r["source_task_ids"].clone()))
            .collect::<Vec<_>>(),
        [(json!(1), json!(["D"])), (json!(2), json!(["E"]))]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'E'")?,
        ["VERIFYING"]
    );
    Ok(())
}

/// The scheduling round's context for run R: qa and reviewer need no capability, at the completion's time.
fn verifiers() -> crate::decision::Context {
    crate::decision::Context {
        required_capabilities: [
            ("qa".to_owned(), Default::default()),
            ("reviewer".to_owned(), Default::default()),
        ]
        .into(),
        now: Some(ending().at),
        ..crate::decision::Context::default()
    }
}

/// D completed into C1's round 1; the round's id.
async fn verifying(store: &Store) -> R<String> {
    let job = freeze_of('b');
    let c1 = job.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(completion(freeze(job, result(None)?)))
        .await?;
    Ok(crate::ids::child_id(&c1, "round/1"))
}

#[tokio::test]
async fn a_rounds_verifier_tasks_are_dispatched_by_the_runtime_even_on_a_blocked_run() -> R {
    // Decision 10: a runtime-class dispatch with origin "verification", taken on a BLOCKED run too (the Commander
    // unavailable), each naming its round and its current handoff.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE runs SET state = 'BLOCKED', blocked_reason = 'COMMANDER_UNAVAILABLE' WHERE run_id = 'R';",
    )?;
    let mut dispatched = Vec::new();
    while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
        dispatched.push(d.task_id);
    }
    let id = |suffix: &str| crate::ids::child_id(&round, suffix);
    assert_eq!(dispatched, [id("task/qa"), id("task/reviewer")]);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.origin') || ':' || json_extract(payload_json, '$.round_id') || ':' || \
             json_extract(payload_json, '$.handoff') FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             ORDER BY rowid"
        )?,
        [
            format!("verification:{round}:{}", id("verify/qa")),
            format!("verification:{round}:{}", id("verify/reviewer"))
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_handoff_of_an_older_epoch_is_dead_lettered_and_issued_again_at_the_current_one() -> R {
    // Decision 9 (REQ-022 behaviour 8, M2T16D-204): qa was rebound after the handoff was written. Its dispatch refuses
    // the old message (DEAD_LETTER, STALE_ROLE_EPOCH, nothing else changed) and issues it again at the current epoch,
    // the package the same but for its epoch; the dispatch and its attempt use the new one.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE role_bindings SET role_epoch = 3 WHERE run_id = 'R' AND role = 'qa';",
    )?;
    let d = store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .ok_or("nothing dispatched")?;
    let old = crate::ids::child_id(&round, "verify/qa");
    let new = crate::ids::child_id(&old, "e3");
    assert_eq!(d.task_id, crate::ids::child_id(&round, "task/qa"));
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT message_id || ':' || state || ':' || role_epoch || ':' || \
                 json_extract(payload_json, '$.role_epoch') || ':' || json_extract(payload_json, '$.payload.role_epoch') \
                 FROM outbox WHERE message_id IN ('{old}', '{new}') ORDER BY rowid"
            )
        )?,
        [
            format!("{old}:DEAD_LETTER:1:1:1"),
            format!("{new}:PENDING:3:3:3")
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'outbox.dead_letter'"
        )?,
        ["STALE_ROLE_EPOCH"]
    );
    let packages = texts(
        &path,
        &format!(
            "SELECT json_remove(json_extract(payload_json, '$.payload'), '$.role_epoch') FROM outbox \
             WHERE message_id IN ('{old}', '{new}')"
        ),
    )?;
    assert_eq!(
        packages[0], packages[1],
        "the package the same but for its epoch"
    );
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT json_extract(payload_json, '$.handoff') FROM outbox WHERE message_id = '{}'",
                crate::ids::child_id(&d.attempt_id, "dispatch")
            )
        )?,
        [new]
    );
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT CAST(role_epoch AS TEXT) FROM attempts WHERE attempt_id = '{}'",
                d.attempt_id
            )
        )?,
        ["3"]
    );
    Ok(())
}

#[tokio::test]
async fn test_022_05_an_empty_handoff_found_at_its_dispatch_is_refused_and_creates_no_attempt() -> R
{
    // AC-022-05's fourth message under decision 9 (implementation correction 1): a verification handoff with no
    // acceptance criteria (only a tampered store has one) is refused at its verifier task's dispatch: message.invalid,
    // the message DEAD_LETTER, no lease and no attempt, the verifier task BLOCKED for a decision with
    // integrity_violation; the verified task does not move.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    let qa = crate::ids::child_id(&round, "verify/qa");
    let task = crate::ids::child_id(&round, "task/qa");
    Connection::open(&path)?.execute(
        "UPDATE outbox SET payload_json = json_set(payload_json, '$.payload.acceptance_ids', json('[]')) \
         WHERE message_id = ?1",
        [&qa],
    )?;
    let d = texts(
        &path,
        "SELECT state || ':' || revision FROM tasks WHERE task_id = 'D'",
    )?;
    let round_outcome = store.schedule_round("R", verifiers()).await?;
    assert!(
        round_outcome
            .dispatched
            .as_ref()
            .is_none_or(|d| d.task_id != task),
        "{round_outcome:?}"
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM outbox WHERE message_id = '{qa}'")
        )?,
        ["DEAD_LETTER"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.message_id') FROM events WHERE kind = 'message.invalid'"
        )?,
        std::slice::from_ref(&qa)
    );
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
            &format!("SELECT attempt_id FROM attempts WHERE task_id = '{task}'")
        )?
        .len(),
        0
    );
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'integrity_violation'"
        )?
        .len(),
        1
    );
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || revision FROM tasks WHERE task_id = 'D'"
        )?,
        d
    );
    Ok(())
}

#[tokio::test]
async fn a_verifier_bound_to_a_source_attempts_session_is_refused_as_not_independent() -> R {
    // Decision 10 (REQ-036 failure mode 5, M2T16D-361): a forged binding gives the reviewer the developer's session,
    // which D's attempt used: its dispatch is refused SESSION_NOT_INDEPENDENT, the verifier task BLOCKED for a decision
    // with integrity_violation, no attempt.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE role_bindings SET session_id = 'sd' WHERE run_id = 'R' AND role = 'reviewer';",
    )?;
    while store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .is_some()
    {}
    let task = crate::ids::child_id(&round, "task/reviewer");
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
            &format!("SELECT attempt_id FROM attempts WHERE task_id = '{task}'")
        )?
        .len(),
        0
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'integrity_violation'"
        )?,
        ["SESSION_NOT_INDEPENDENT"]
    );
    // The qa verifier is dispatched as usual.
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT CAST(count(*) AS TEXT) FROM attempts WHERE task_id = '{}'",
                crate::ids::child_id(&round, "task/qa")
            )
        )?,
        ["1"]
    );
    Ok(())
}

#[tokio::test]
async fn a_verifiers_request_to_change_the_candidate_is_refused_with_an_issue_and_others_deferred()
-> R {
    // docs/adr/ADR-028.md decision 13 (REQ-037 behaviour 6, failure mode 5): while the round is open, a verifier's
    // request that the candidate under verification be changed is refused (request.rejected, ILLEGAL_TRANSITION) with
    // an OPEN Issue on the candidate (its fingerprint sha256(severity, normalised text), a severity outside the four
    // being medium) and issue.opened, nothing queued; asked again, the same answer and nothing more. Any other request
    // is deferred (request.deferred), no Issue.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    let outbox = texts(&path, "SELECT message_id FROM outbox")?;
    let attempt = crate::ids::child_id(&round, "task/qa/attempt-1");
    let change = |message: &str, severity: &str| {
        VerifierRequest::RejectChange(ChangeRequest {
            run_id: "R".to_owned(),
            attempt_id: attempt.clone(),
            message_id: message.to_owned(),
            round_id: round.clone(),
            candidate_id: candidate.clone(),
            severity: severity.to_owned(),
            text: "Fix   the parser\n before release ".to_owned(),
        })
    };
    let issue = store.verifier_request(change("m1", "high")).await?;
    assert_eq!(issue, Some(crate::ids::child_id(&attempt, "request/m1")));
    assert_eq!(
        store.verifier_request(change("m1", "high")).await?,
        issue,
        "asked again"
    );
    store.verifier_request(change("m2", "urgent")).await?;
    let fingerprint = |severity: &str| {
        crate::context::sha256_hex(format!("{severity}\nfix the parser before release").as_bytes())
    };
    assert_eq!(
        texts(
            &path,
            "SELECT issue_id || ':' || candidate_id || ':' || severity || ':' || state || ':' || fingerprint \
             FROM issues ORDER BY issue_id"
        )?,
        [
            format!(
                "{}:{candidate}:high:OPEN:{}",
                crate::ids::child_id(&attempt, "request/m1"),
                fingerprint("high")
            ),
            format!(
                "{}:{candidate}:medium:OPEN:{}",
                crate::ids::child_id(&attempt, "request/m2"),
                fingerprint("medium")
            ),
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason_code') || ':' || json_extract(payload_json, '$.round_id') \
             FROM events WHERE kind = 'request.rejected'"
        )?,
        [
            format!("ILLEGAL_TRANSITION:{round}"),
            format!("ILLEGAL_TRANSITION:{round}")
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.issue_id') || ':' || json_extract(payload_json, '$.severity') \
             FROM events WHERE kind = 'issue.opened' ORDER BY seq"
        )?,
        [
            format!("{}:high", crate::ids::child_id(&attempt, "request/m1")),
            format!("{}:medium", crate::ids::child_id(&attempt, "request/m2"))
        ]
    );
    assert_eq!(texts(&path, "SELECT message_id FROM outbox")?, outbox);
    let deferred = store
        .verifier_request(VerifierRequest::Defer(Deferred {
            run_id: "R".to_owned(),
            attempt_id: attempt.clone(),
            message_id: "m3".to_owned(),
            recipient_role: "developer".to_owned(),
        }))
        .await?;
    assert_eq!(deferred, None);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.message_id') || ':' || json_extract(payload_json, '$.recipient_role') \
             FROM events WHERE kind = 'request.deferred'"
        )?,
        ["m3:developer"]
    );
    assert_eq!(texts(&path, "SELECT issue_id FROM issues")?.len(), 2);
    // The wait-for view (decision 13): refused and deferred requests are never queued, so no edge; a query from qa to
    // the developer still PENDING is one (the positive control), an ACKED one is not.
    assert_eq!(
        store.reader()?.wait_for("R")?,
        Vec::<(String, String)>::new()
    );
    Connection::open(&path)?.execute_batch(
        "INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
         SELECT 'q-open', max(seq), 'R', 'developer', 1, \
           json_object('kind', 'query', 'sender_role', 'qa', 'recipient_role', 'developer'), 'PENDING' FROM events;
         INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
         SELECT 'q-done', max(seq), 'R', 'qa', 1, \
           json_object('kind', 'steer', 'sender_role', 'reviewer', 'recipient_role', 'qa'), 'ACKED' FROM events;",
    )?;
    assert_eq!(
        store.reader()?.wait_for("R")?,
        [("qa".to_owned(), "developer".to_owned())]
    );
    Ok(())
}

#[tokio::test]
async fn a_writer_whose_criteria_an_open_round_with_an_open_issue_covers_waits_for_the_round_to_close()
-> R {
    // docs/adr/ADR-028.md decision 13 (REQ-037 decision 1, M2T16D-458, -554, -602): C1's round 1 is open with an OPEN
    // Issue on C1 (a refused change request). Writer W, whose requirement REQ-1 has a criterion of the round, is not
    // selected by a scheduling round (it stays READY) and a Commander's dispatch of it is ILLEGAL_TRANSITION; writer V,
    // whose criteria the round does not cover, is dispatched. Once the round is closed, W is dispatched too.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    store
        .verifier_request(VerifierRequest::RejectChange(ChangeRequest {
            run_id: "R".to_owned(),
            attempt_id: crate::ids::child_id(&round, "task/qa/attempt-1"),
            message_id: "m1".to_owned(),
            round_id: round.clone(),
            candidate_id: candidate,
            severity: "high".to_owned(),
            text: "change the parser".to_owned(),
        }))
        .await?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-2', 's1', 'r2', 1);
         INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-2-01', 'REQ-2', 's1', 'b2', 'unit');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('W', 'R', 's1', 'READY', 1, 'developer', 1, 10), ('V', 'R', 's1', 'READY', 1, 'developer', 1, 11);
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('W', 'REQ-1', 's1'), ('V', 'REQ-2', 's1');
         UPDATE tasks SET state = 'CANCELLED' WHERE role IN ('qa', 'reviewer');
         INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES ('sc', 'codex', 'commander', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-12T00:00:00Z');",
    )?;
    let mut context = verifiers();
    context
        .required_capabilities
        .insert("developer".to_owned(), Default::default());
    let first = store
        .schedule_round("R", context.clone())
        .await?
        .dispatched
        .ok_or("nothing dispatched")?;
    assert_eq!(first.task_id, "V");
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'W'")?,
        ["READY"]
    );
    // The Commander's dispatch of W (V's lease released first, so only the hold refuses it).
    Connection::open(&path)?.execute_batch(
        "UPDATE leases SET expires_at = '2000-01-01T00:00:00Z' WHERE kind = 'writer';
         UPDATE attempts SET state = 'CANCELLED' WHERE task_id = 'V';
         UPDATE tasks SET state = 'CANCELLED' WHERE task_id = 'V';",
    )?;
    let revision: i64 = Connection::open(&path)?.query_row(
        "SELECT revision FROM runs WHERE run_id = 'R'",
        [],
        |r| r.get(0),
    )?;
    let message: crate::protocol::Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": "d1", "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 1, "correlation_id": "c",
        "causation_id": null, "idempotency_key": "k1", "expected_revision": revision,
        "spec_sha256": "1".repeat(64), "candidate_id": null, "kind": "decision",
        "payload": {"kind": "dispatch", "task_id": "W", "role": "developer"}
    }))?;
    let proposal =
        crate::decision::Proposal::from_message(&message).map_err(|e| format!("{e:?}"))?;
    let decided = store.decide(proposal, context.clone()).await?;
    let crate::persistence::Decided::Rejected { reason, .. } = decided else {
        return Err(format!("{decided:?}").into());
    };
    assert_eq!(reason, crate::protocol::RejectionReason::IllegalTransition);
    // The round closed: W is dispatched.
    Connection::open(&path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', ?1, 'R', 'verification.round_closed', json_object('round_id', ?2), '2026-11-12T00:00:09Z')",
        [crate::ids::child_id(&round, "closed"), round.clone()],
    )?;
    let next = store
        .schedule_round("R", context)
        .await?
        .dispatched
        .ok_or("W was not dispatched")?;
    assert_eq!(next.task_id, "W");
    Ok(())
}

/// C1's round 1 with its qa verifier's attempt RUNNING (dispatched, then started): the round, the task, the attempt.
async fn verifier_running(store: &Store, path: &Path) -> R<(String, String, String)> {
    let round = verifying(store).await?;
    let d = store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .ok_or("nothing dispatched")?;
    Connection::open(path)?.execute(
        "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = ?1",
        [&d.attempt_id],
    )?;
    Connection::open(path)?.execute(
        "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = ?1",
        [&d.task_id],
    )?;
    Ok((round, d.task_id, d.attempt_id))
}

fn verifier_completion(attempt: &str, outcome: VerifierOutcome) -> VerifierCompletion {
    VerifierCompletion {
        attempt_id: attempt.to_owned(),
        ending: ending(),
        outcome,
    }
}

#[tokio::test]
async fn a_verifiers_completed_result_settles_it_and_its_task_is_done() -> R {
    // Decision 11 (verifier outputs): COMPLETED settles the attempt and its task IMPLEMENTED, the result recorded.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, task, attempt) = verifier_running(&store, &path).await?;
    let done = store
        .complete_verifier_attempt(verifier_completion(
            &attempt,
            VerifierOutcome::Settle {
                report: Box::new(VerifierReport {
                    role: "qa".to_owned(),
                    status: "PASS".to_owned(),
                    report: object(7, "independent_report", "report.json"),
                    native_attempt: object(8, "native_attempt", "attempt.json"),
                }),
            },
        ))
        .await?;
    assert_eq!(done, VerifierCompleted::Settled);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.role') || ':' || json_extract(payload_json, '$.status') || ':' || \
             json_extract(payload_json, '$.report.sha256') FROM events WHERE kind = 'verification.report'"
        )?,
        [format!("qa:PASS:{}", object(7, "", "").sha256)]
    );
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT path FROM artifacts WHERE sha256 = '{}'",
                object(8, "", "").sha256
            )
        )?
        .len(),
        1
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM tasks WHERE task_id = '{task}'")
        )?,
        ["IMPLEMENTED"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.result_status') || ':' || json_extract(payload_json, '$.round_id') \
             FROM events WHERE kind = 'verification.result'"
        )?,
        [format!("COMPLETED:{round}")]
    );
    Ok(())
}

#[tokio::test]
async fn a_verifiers_failed_result_is_verified_again_in_its_round_until_the_reverify_limit() -> R {
    // Decision 11/12 (M2T16D-260, -362): a legal FAILED, BLOCKED or INCOMPLETE result sends the verifier task back to
    // READY within its round, counted by Repair::Reverify keyed by the round's chain (REVERIFY_LIMIT = 2); past it the
    // task is BLOCKED for a decision. A missing result is a protocol correction instead.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, task, first) = verifier_running(&store, &path).await?;
    let reverify = |status: &str| VerifierOutcome::Reverify {
        status: status.to_owned(),
        reason: format!("the verifier said {status}"),
    };
    let mut verdicts = Vec::new();
    for (n, status) in ["FAILED", "INCOMPLETE", "BLOCKED"].into_iter().enumerate() {
        let attempt = if n == 0 {
            first.clone()
        } else {
            let attempt = format!("{task}/attempt-{}", n + 1);
            Connection::open(&path)?.execute_batch(&format!(
                "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{task}';
                 INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
                   VALUES ('{attempt}', '{task}', 'sq', 'RUNNING', 1, 1, 1);"
            ))?;
            attempt
        };
        let done = store
            .complete_verifier_attempt(verifier_completion(&attempt, reverify(status)))
            .await?;
        verdicts.push(done);
        let state = texts(
            &path,
            &format!("SELECT state FROM tasks WHERE task_id = '{task}'"),
        )?;
        assert_eq!(state, [if n < 2 { "READY" } else { "BLOCKED" }], "{status}");
    }
    assert_eq!(
        verdicts,
        [
            VerifierCompleted::Reverified {
                verdict: RepairVerdict::Granted { count: 1 }
            },
            VerifierCompleted::Reverified {
                verdict: RepairVerdict::Granted { count: 2 }
            },
            VerifierCompleted::Reverified {
                verdict: RepairVerdict::Refused { count: 2 }
            },
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.round_root') FROM events WHERE kind = 'repair.reverify'"
        )?,
        [round.clone(), round.clone()]
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT blocked_reason FROM tasks WHERE task_id = '{task}'")
        )?,
        ["NEED_DECISION"]
    );
    Ok(())
}

#[tokio::test]
async fn a_verifiers_missing_result_is_a_protocol_correction_and_an_unknown_effect_waits_for_a_decision()
-> R {
    // Decision 11: a missing or invalid result is corrected in the round (Repair::Protocol, task READY). A verifier
    // attempt that ends UNKNOWN_EFFECT is never run again by itself (REQ-041 behaviour 2): its task BLOCKED for a
    // decision with need_decision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (_, task, attempt) = verifier_running(&store, &path).await?;
    let done = store
        .complete_verifier_attempt(verifier_completion(
            &attempt,
            VerifierOutcome::Correct {
                reasons: vec!["it wrote no TaskResult to its bridge".to_owned()],
            },
        ))
        .await?;
    assert_eq!(
        done,
        VerifierCompleted::Corrected {
            verdict: RepairVerdict::Granted { count: 1 }
        }
    );
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM tasks WHERE task_id = '{task}'")
        )?,
        ["READY"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reasons[0]') FROM events WHERE kind = 'verification.output_invalid'"
        )?,
        ["it wrote no TaskResult to its bridge"]
    );
    let second = format!("{task}/attempt-2");
    Connection::open(&path)?.execute_batch(&format!(
        "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{task}';
         INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
           VALUES ('{second}', '{task}', 'sq', 'RUNNING', 1, 1, 1);"
    ))?;
    let mut unknown = ending();
    unknown.unknown_effect = true;
    store.runner_finished(&second, unknown).await?;
    assert_eq!(
        texts(
            &path,
            &format!("SELECT state FROM attempts WHERE attempt_id = '{second}'")
        )?,
        ["UNKNOWN_EFFECT"]
    );
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
            &format!(
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision' \
                 AND json_extract(payload_json, '$.task_id') = '{task}'"
            )
        )?,
        ["verifier_unknown_effect"]
    );
    Ok(())
}

/// C1's round 1 with both verifiers settled, their reports `qa` and `review` (PASS, FAIL or UNKNOWN): the round, the
/// candidate and the two attempts.
async fn verified(
    store: &Store,
    path: &Path,
    qa: &str,
    review: &str,
) -> R<(String, String, [String; 2])> {
    let round = verifying(store).await?;
    let candidate = texts(path, "SELECT candidate_id FROM candidates")?.remove(0);
    let attempts = settle_verifiers(store, path, qa, review).await?;
    Ok((round, candidate, attempts))
}

/// The two READY verifier tasks dispatched, started and settled with reports `qa` and `review`: their attempts.
async fn settle_verifiers(store: &Store, path: &Path, qa: &str, review: &str) -> R<[String; 2]> {
    let mut attempts = Vec::new();
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
        let status = if role == "qa" { qa } else { review };
        Connection::open(path)?.execute_batch(&format!(
            "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = '{0}';
             UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{1}';",
            d.attempt_id, d.task_id
        ))?;
        attempts.push((role.to_owned(), status.to_owned(), d.attempt_id));
    }
    for (role, status, attempt) in &attempts {
        // Report objects of their own: a digest no other object of the test has.
        let unique = |kind: &str, name: &str| -> R<ArtifactRow> {
            let n = texts(path, "SELECT artifact_id FROM artifacts")?.len();
            let sha256 = format!(
                "{:064x}",
                0xfeed_0000 + n + usize::from(kind == "native_attempt")
            );
            Ok(ArtifactRow {
                path: format!(".hoh/objects/sha256/{}/{sha256}", &sha256[..2]),
                sha256,
                bytes: 10,
                mime: "application/json".to_owned(),
                kind: kind.to_owned(),
                name: name.to_owned(),
            })
        };
        store
            .complete_verifier_attempt(verifier_completion(
                attempt,
                VerifierOutcome::Settle {
                    report: Box::new(VerifierReport {
                        role: role.clone(),
                        status: status.clone(),
                        report: unique("independent_report", "report.json")?,
                        native_attempt: unique("native_attempt", "attempt.json")?,
                    }),
                },
            ))
            .await?;
    }
    let of = |role: &str| {
        attempts
            .iter()
            .find(|a| a.0 == role)
            .map(|a| a.2.clone())
            .unwrap_or_default()
    };
    Ok([of("qa"), of("reviewer")])
}

/// A planned check's evidence by `attempt` on `candidate`: its outcome and its acceptance rows (written when its round
/// closes when `deferred`).
fn evidence_of(
    path: &Path,
    (attempt, candidate, record): (&str, &str, &str),
    (check, outcome): (&str, &str),
    rows: &[(&str, &str)],
    deferred: bool,
) -> R {
    let rows_json: Vec<serde_json::Value> = rows
        .iter()
        .map(|(ac, status)| json!({"ac_id": ac, "spec_id": "s1", "status": status, "reason": ""}))
        .collect();
    let conn = Connection::open(path)?;
    conn.execute(
        "INSERT OR IGNORE INTO artifacts(artifact_id, sha256, path, bytes, mime) VALUES (?1, ?2, ?3, 1, 'x')",
        rusqlite::params![format!("{}:{record}", "e".repeat(64)), "e".repeat(64), record],
    )?;
    conn.execute(
        "INSERT INTO evidence(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode, \
         created_at) VALUES (?1, ?2, ?3, ?4, ?5, 'check', ?6, 'product', '2026-11-12T00:00:05Z')",
        rusqlite::params![record, candidate, attempt, format!("{}:{record}", "e".repeat(64)), check, outcome],
    )?;
    conn.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
         'evidence.recorded', ?2, '2026-11-12T00:00:05Z')",
        rusqlite::params![
            format!("{record}/evidence"),
            json!({"evidence_id": record, "attempt_id": attempt, "check_id": check, "outcome": outcome,
                "outcome_reason": null, "reasons": [], "rows": rows_json, "deferred_rows": deferred})
            .to_string()
        ],
    )?;
    if !deferred {
        for (ac, status) in rows {
            conn.execute(
                "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status, reason) \
                 VALUES (?1, 's1', ?2, ?3, ?4, '')",
                rusqlite::params![ac, candidate, record, status],
            )?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_037_02_a_failed_round_closes_after_both_verifiers_and_reworks_its_source() -> R {
    // AC-037-02 at the store (docs/adr/ADR-028.md decisions 11 and 12): QA's report FAIL with a failing check, the
    // reviewer's PASS. Closed only once both are settled: its acceptance rows written then (the FAIL), verdict repair,
    // C1 REJECTED, the failure items registered with their owner D (I6), D reworked (READY, layer 0), the verifier
    // tasks done; asked again, nothing more.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, candidate, [qa, _]) = verified(&store, &path, "FAIL", "PASS").await?;
    evidence_of(
        &path,
        (&qa, &candidate, "rec-1"),
        ("unit", "FAIL"),
        &[("AC-1-01", "FAIL")],
        true,
    )?;
    assert_eq!(
        texts(&path, "SELECT status FROM acceptance_results")?.len(),
        0,
        "no row before the round closes (M2T16D-455)"
    );
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(done.closed, [(round.clone(), "repair".to_owned())]);
    assert_eq!(
        texts(&path, "SELECT status FROM acceptance_results")?,
        ["FAIL"]
    );
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["REJECTED"]);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(i.value, '$.task_id') || ':' || json_extract(i.value, '$.item') FROM events e, \
             json_each(e.payload_json, '$.items') i WHERE e.kind = 'verification.failures' ORDER BY 1"
        )?,
        [
            "D:ac-fail:AC-1-01",
            "D:check-fail:unit:",
            "D:report-fail:qa"
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || layer FROM tasks WHERE task_id = 'D'"
        )?,
        ["READY:0"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT DISTINCT state FROM tasks WHERE role IN ('qa', 'reviewer')"
        )?,
        ["ACCEPTED"]
    );
    // C1's one iteration, on its first conclusive result (decision 11, M2T16D-362).
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.verdict') || ':' || json_extract(payload_json, '$.iteration') \
             FROM events WHERE kind = 'iteration.completed'"
        )?,
        ["repair:1"]
    );
    assert_eq!(
        texts(&path, "SELECT CAST(number AS TEXT) FROM iterations")?,
        ["1"]
    );
    let events = texts(&path, "SELECT CAST(count(*) AS TEXT) FROM events")?;
    let again = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(again, Reconciled::default());
    assert_eq!(
        texts(&path, "SELECT CAST(count(*) AS TEXT) FROM events")?,
        events
    );
    Ok(())
}

#[tokio::test]
async fn a_passable_round_waits_at_its_candidates_barrier_for_the_join() -> R {
    // Decision 11: both reports PASS, the check PASS, every scope criterion PASS: passable; C1 stays VERIFYING, D waits,
    // and the candidate is ready for M2-T18's join (once).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, candidate, [qa, _]) = verified(&store, &path, "PASS", "PASS").await?;
    evidence_of(
        &path,
        (&qa, &candidate, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(done.closed, [(round, "passable".to_owned())]);
    assert_eq!(done.joinable, std::slice::from_ref(&candidate));
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["VERIFYING"]);
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["VERIFYING"]
    );
    let again = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert!(again.joinable.is_empty());
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.verdict') FROM events WHERE kind = 'iteration.completed'"
        )?,
        ["passable"]
    );
    Ok(())
}

#[tokio::test]
async fn an_inconclusive_round_is_verified_again_until_its_chain_is_spent() -> R {
    // Decisions 11 and 12 (M2T16D-260, -362): reports PASS but no check ran: inconclusive; the next round on C1 with D
    // and the same scope, counted on round 1's chain; past REVERIFY_LIMIT D is BLOCKED for a decision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, candidate, _) = verified(&store, &path, "PASS", "PASS").await?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(done.closed, [(round.clone(), "reverify".to_owned())]);
    let second = crate::ids::child_id(&candidate, "round/2");
    assert_eq!(done.opened, std::slice::from_ref(&second));
    let rounds = rounds_of(&path)?;
    assert_eq!(rounds[1]["chain_root"], json!(round));
    assert_eq!(rounds[1]["source_task_ids"], json!(["D"]));
    assert_eq!(rounds[1]["acceptance_ids"], json!(["AC-1-01"]));
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["VERIFYING"]
    );
    // Round 2 inconclusive too: round 3, the chain's second; round 3 inconclusive: refused, D BLOCKED.
    settle_verifiers(&store, &path, "PASS", "PASS").await?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(done.opened, [crate::ids::child_id(&candidate, "round/3")]);
    settle_verifiers(&store, &path, "PASS", "PASS").await?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert!(done.opened.is_empty(), "{done:?}");
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision' \
             AND json_extract(payload_json, '$.task_id') = 'D'"
        )?,
        ["reverify_exhausted"]
    );
    Ok(())
}

fn rounds_of(path: &Path) -> R<Vec<serde_json::Value>> {
    rounds(path)
}

#[tokio::test]
async fn a_flaky_criterion_blocks_its_source_for_a_decision() -> R {
    // Decision 11: a criterion with a PASS and a NOT_RUN row on the candidate is flaky (retesting the same candidate
    // cannot satisfy it): D BLOCKED for a decision with need_decision{flaky}.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (round, candidate, [qa, reviewer]) = verified(&store, &path, "PASS", "PASS").await?;
    evidence_of(
        &path,
        (&qa, &candidate, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    evidence_of(
        &path,
        (&reviewer, &candidate, "rec-2"),
        ("unit", "PASS"),
        &[("AC-1-01", "NOT_RUN")],
        true,
    )?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(done.closed, [(round, "flaky".to_owned())]);
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision' \
             AND json_extract(payload_json, '$.task_id') = 'D'"
        )?,
        ["flaky"]
    );
    Ok(())
}

#[tokio::test]
async fn a_superseded_candidates_round_cancels_its_unstarted_verifiers_and_closes_superseded() -> R
{
    // Decisions 9 to 11: E's completion freezes C2 while C1's round 1 has not started (C1 STALE, D moved into C2's
    // round). C1's verifier tasks are cancelled and their handoffs dead-lettered; the round closes superseded and D,
    // whose current round is C2's, does not move.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    second_writer(&path, "E", "AE")?;
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: "AE".to_owned(),
            ..completion(freeze(freeze_of('c'), result_of("E", "AE")?))
        })
        .await?;
    let done = store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(
        done.cancelled,
        [
            crate::ids::child_id(&round, "task/qa"),
            crate::ids::child_id(&round, "task/reviewer")
        ]
    );
    assert_eq!(done.closed, [(round.clone(), "superseded".to_owned())]);
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT DISTINCT state FROM outbox WHERE message_id IN ('{}', '{}')",
                crate::ids::child_id(&round, "verify/qa"),
                crate::ids::child_id(&round, "verify/reviewer")
            )
        )?,
        ["DEAD_LETTER"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["VERIFYING"]
    );
    Ok(())
}

#[tokio::test]
async fn a_round_whose_verifier_is_blocked_and_whose_other_is_done_stalls_its_run() -> R {
    // Decision 10's in-flight rule (M2T16D-202): a VERIFYING source and an IMPLEMENTED verifier are not in flight; with
    // qa BLOCKED_AUTH and the reviewer done, nothing of the run can move on: BLOCKED with BLOCKED_AUTH. A source waiting
    // at a passable candidate's barrier for the join is in flight (M2-T16's transition): its run is not stalled.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    verifying(&store).await?;
    let d = store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .ok_or("nothing dispatched")?;
    // The first dispatched (qa) blocked for authentication; the other settled.
    Connection::open(&path)?.execute(
        "UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH', revision = revision + 1 WHERE task_id = ?1",
        [&d.task_id],
    )?;
    Connection::open(&path)?.execute(
        "UPDATE attempts SET state = 'FAILED' WHERE attempt_id = ?1",
        [&d.attempt_id],
    )?;
    let other = store
        .schedule_round("R", verifiers())
        .await?
        .dispatched
        .ok_or("the other was not dispatched")?;
    Connection::open(&path)?.execute_batch(&format!(
        "UPDATE attempts SET state = 'SETTLED' WHERE attempt_id = '{0}';
         UPDATE tasks SET state = 'IMPLEMENTED', revision = revision + 1 WHERE task_id = '{1}';",
        other.attempt_id, other.task_id
    ))?;
    store.pace_run("R", verifiers()).await?;
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reasons_json FROM runs WHERE run_id = 'R'"
        )?,
        ["BLOCKED:[\"BLOCKED_AUTH\"]"]
    );
    // A passable candidate waiting for its join: in flight.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (_, candidate, [qa, _]) = verified(&store, &path, "PASS", "PASS").await?;
    evidence_of(
        &path,
        (&qa, &candidate, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    store
        .reconcile_verification("R", Default::default())
        .await?;
    store.pace_run("R", verifiers()).await?;
    assert_eq!(
        texts(&path, "SELECT state FROM runs WHERE run_id = 'R'")?,
        ["RUNNING"]
    );
    Ok(())
}

#[tokio::test]
async fn a_repair_refusal_holds_its_task_until_a_decision_clears_it() -> R {
    // Decision 12 (M2T16D-800, -853, -950): a refused repair is a standing admission condition. Writer D READY with a
    // refusal not cleared is BLOCKED by the reconcile and never selected; once Store::clear_repair_refusal records the
    // decision (repair.refusal_cleared) and the decision sends it back to READY, it is dispatched; a writer's completion
    // with a refusal standing freezes nothing.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "UPDATE attempts SET state = 'FAILED' WHERE attempt_id = 'A';
         UPDATE leases SET expires_at = '2000-01-01T00:00:00Z';
         UPDATE tasks SET state = 'READY' WHERE task_id = 'D';
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'nd-1', 'R',
           'need_decision', '{\"task_id\": \"D\", \"reason\": \"local_repairs_exhausted\"}', '2026-11-12T00:00:02Z');",
    )?;
    let mut context = verifiers();
    context
        .required_capabilities
        .insert("developer".to_owned(), Default::default());
    store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reason FROM tasks WHERE task_id = 'D'"
        )?,
        ["BLOCKED:NEED_DECISION"]
    );
    store.clear_repair_refusal("D", "decision-1").await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'READY', blocked_reason = NULL, revision = revision + 1 WHERE task_id = 'D';",
    )?;
    store
        .reconcile_verification("R", Default::default())
        .await?;
    assert_eq!(
        texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        ["READY"]
    );
    let d = store
        .schedule_round("R", context)
        .await?
        .dispatched
        .ok_or("D was not dispatched")?;
    assert_eq!(d.task_id, "D");
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.decision_id') FROM events WHERE kind = 'repair.refusal_cleared'"
        )?,
        ["decision-1"]
    );
    Ok(())
}

#[tokio::test]
async fn a_completion_after_failures_were_attached_to_its_running_task_goes_back_to_rework() -> R {
    // Decision 3's row (decision 11 step 2a, M2T16D-800): failure items attached to D while its attempt ran, after its
    // prompt: its COMPLETED result is not frozen and opens no round; D goes back to READY (layer 0) for the rework its
    // next attempt's context lists (the items were counted when attached).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'A/running',
           'R', 'attempt.moved', '{\"attempt_id\": \"A\", \"to\": \"RUNNING\"}', '2026-11-12T00:00:01Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'g1/attached/D',
           'R', 'verification.failures_attached', '{\"task_id\": \"D\", \"round_id\": \"g1\", \"items\": [\"x\"]}',
           '2026-11-12T00:00:02Z');",
    )?;
    let done = store
        .complete_writer_attempt(completion(freeze(freeze_of('b'), result(None)?)))
        .await?;
    assert_eq!(done, WriterCompleted::Reworked);
    assert_eq!(
        texts(&path, "SELECT candidate_id FROM candidates")?.len(),
        0
    );
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || layer FROM tasks WHERE task_id = 'D'"
        )?,
        ["READY:0"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM attempts WHERE attempt_id = 'A'")?,
        ["SETTLED"]
    );
    Ok(())
}

#[tokio::test]
async fn a_candidates_unrecoverable_evidence_is_judged_permanent_at_once_transient_after_three_and_ten_minutes()
-> R {
    // Decision 11's recovery (M2T16D-1250, -1251): each failed recovery of a candidate's pending records is recorded;
    // a permanent one is judged at once, a transient one only after at least 3 in a row over at least 10 minutes, and a
    // success in between starts the count again. Judged: a VERIFYING candidate STALE with
    // candidate.stale{evidence_unrecoverable} and integrity_violation; an INVALID one keeps its state, the fact written.
    let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    let minutes = |m: u64| t0 + std::time::Duration::from_secs(60 * m);
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    let transient = || Err((false, "EIO".to_owned()));
    store
        .record_recovery(&candidate, transient(), minutes(0))
        .await?;
    store
        .record_recovery(&candidate, transient(), minutes(4))
        .await?;
    store.record_recovery(&candidate, Ok(1), minutes(5)).await?;
    store
        .record_recovery(&candidate, transient(), minutes(6))
        .await?;
    store
        .record_recovery(&candidate, transient(), minutes(9))
        .await?;
    store
        .record_recovery(&candidate, transient(), minutes(15))
        .await?;
    assert_eq!(
        texts(&path, "SELECT state FROM candidates")?,
        ["VERIFYING"],
        "3 in a row but only 9 minutes since the first after the success"
    );
    store
        .record_recovery(&candidate, transient(), minutes(16))
        .await?;
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["STALE"]);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'candidate.stale'"
        )?,
        ["evidence_unrecoverable"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'candidate.reconcile_recovered'"
        )?
        .len(),
        1
    );
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'integrity_violation'"
        )?
        .len(),
        1
    );
    // A permanent fault on an INVALID candidate: judged at once, its state kept.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    Connection::open(&path)?.execute_batch("UPDATE candidates SET state = 'INVALID';")?;
    store
        .record_recovery(&candidate, Err((true, "damaged".to_owned())), minutes(0))
        .await?;
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["INVALID"]);
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'candidate.stale'"
        )?,
        ["evidence_unrecoverable"]
    );
    Ok(())
}

#[tokio::test]
async fn a_failed_recovery_at_the_hosts_start_wakes_the_commander_unless_its_candidate_was_superseded()
-> R {
    // docs/adr/ADR-028.md decision 16 (M2T16D-1150, -1201): at the host's start a candidate's failed recovery is
    // counted as decision 11's and, the candidate not superseded, written with integrity_violation; a superseded
    // candidate's failure is only its reconcile_failed (its results are history), never judged.
    let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    store
        .record_start_recovery(&candidate, Err((false, "EIO".to_owned())), t0)
        .await?;
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.attempt') || ':' || json_extract(payload_json, '$.kind') \
             FROM events WHERE kind = 'candidate.reconcile_failed'"
        )?,
        ["1:transient"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'integrity_violation'"
        )?,
        ["reconcile_failed_at_start"]
    );
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["VERIFYING"]);
    // Superseded since (a persistent fact): a permanent fault is only recorded.
    Connection::open(&path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
         SELECT project_id, 'stale-spec', 'R', 'candidate.stale', \
         json_object('candidate_id', ?1, 'reason', 'spec_changed'), '2026-11-12T00:00:00Z' FROM runs WHERE run_id = 'R'",
        rusqlite::params![candidate],
    )?;
    store
        .record_start_recovery(&candidate, Err((true, "damaged".to_owned())), t0)
        .await?;
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'candidate.reconcile_failed'"
        )?
        .len(),
        2
    );
    assert_eq!(
        texts(
            &path,
            "SELECT kind FROM events WHERE kind = 'integrity_violation'"
        )?
        .len(),
        1,
        "no wake for a superseded candidate"
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'candidate.stale'"
        )?,
        ["spec_changed"],
        "never judged"
    );
    assert_eq!(texts(&path, "SELECT state FROM candidates")?, ["VERIFYING"]);
    Ok(())
}

#[tokio::test]
async fn a_candidate_whose_pending_records_failed_to_recover_has_its_verifiers_held_until_they_do()
-> R {
    // docs/adr/ADR-028.md decision 16: a candidate whose last recovery failed is held — its round's verifier tasks
    // stay READY, not dispatched — until a recovery succeeds.
    let t0 = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000);
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let round = verifying(&store).await?;
    let candidate = texts(&path, "SELECT candidate_id FROM candidates")?.remove(0);
    store
        .record_start_recovery(&candidate, Err((false, "EIO".to_owned())), t0)
        .await?;
    assert_eq!(
        store.schedule_round("R", verifiers()).await?.dispatched,
        None
    );
    let id = |suffix: &str| crate::ids::child_id(&round, suffix);
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT state FROM tasks WHERE task_id IN ('{}', '{}')",
                id("task/qa"),
                id("task/reviewer")
            )
        )?,
        ["READY", "READY"]
    );
    store.record_recovery(&candidate, Ok(1), t0).await?;
    let mut dispatched = Vec::new();
    while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
        dispatched.push(d.task_id);
    }
    assert_eq!(dispatched, [id("task/qa"), id("task/reviewer")]);
    Ok(())
}

#[tokio::test]
async fn a_source_whose_candidates_verifiers_tampered_with_is_frozen_anew_until_the_reverify_limit()
-> R {
    // Decisions 11 and 12 (M2T16D-256, -750, -751): a candidate made INVALID with no planned-check veto in its round
    // (a verifier changed its run directory) closes its round superseded and sends D back to be frozen anew, counted by
    // Repair::Reverify keyed refreeze/D across candidates (never reset by a new one); the third time is past
    // REVERIFY_LIMIT: D waits for a decision.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    for (n, tree) in ['b', 'c', 'd'].into_iter().enumerate() {
        let attempt = if n == 0 {
            "A".to_owned()
        } else {
            format!("A{n}")
        };
        if n > 0 {
            Connection::open(&path)?.execute_batch(&format!(
                "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'D';
                 INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
                   started_at) VALUES ('{attempt}', 'D', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:0{n}Z');"
            ))?;
        }
        let job = freeze_of(tree);
        let candidate = job.sealed.candidate_id.clone();
        let done = store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: attempt.clone(),
                ..completion(freeze(job, result_of("D", &attempt)?))
            })
            .await?;
        assert!(
            matches!(done, WriterCompleted::Verifying { .. }),
            "{n}: {done:?}"
        );
        settle_verifiers(&store, &path, "PASS", "PASS").await?;
        Connection::open(&path)?.execute(
            "UPDATE candidates SET state = 'INVALID' WHERE candidate_id = ?1",
            rusqlite::params![candidate],
        )?;
        let closed = store
            .reconcile_verification("R", Default::default())
            .await?;
        assert_eq!(
            closed
                .closed
                .iter()
                .map(|(_, v)| v.as_str())
                .collect::<Vec<_>>(),
            ["superseded"],
            "{n}"
        );
        let state = texts(&path, "SELECT state FROM tasks WHERE task_id = 'D'")?;
        if n < 2 {
            assert_eq!(state, ["READY"], "{n}: frozen anew");
        } else {
            assert_eq!(state, ["BLOCKED"], "past the limit");
        }
    }
    assert_eq!(
        texts(
            &path,
            "SELECT CAST(json_extract(payload_json, '$.count') AS TEXT) || ':' || json_extract(payload_json, '$.round_root') \
             FROM events WHERE kind = 'repair.reverify' ORDER BY seq"
        )?,
        ["1:refreeze/D", "2:refreeze/D"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision' \
             AND json_extract(payload_json, '$.task_id') = 'D'"
        )?,
        ["reverify_exhausted"]
    );
    Ok(())
}
