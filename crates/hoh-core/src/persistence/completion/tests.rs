//! A writer attempt's completion against a real database (docs/adr/ADR-028.md decisions 3 and 4).

use super::*;
use crate::persistence::{ArtifactRow, Options, Store, selftest};
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
