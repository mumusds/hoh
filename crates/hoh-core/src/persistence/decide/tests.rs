//! TEST-035-01 and TEST-035-04 against a real store, plus the APPLIED path: a FakeCommander sends decision messages,
//! the store judges them, and the tables are compared before and after.

use super::*;
use crate::decision::{Context, Proposal};
use crate::ids::MAX_ID;
use crate::persistence::{Options, Store, selftest};
use crate::protocol::Message;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type R = Result<(), Box<dyn std::error::Error>>;

fn sha(n: u32) -> String {
    format!("{n:064}")
}

fn open(dir: &tempfile::TempDir) -> Result<(Store, PathBuf), PersistError> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// AC-035-01's world: run R (RUNNING, revision 7, commander epoch 3, spec s2 = revision 2); T1 READY after T0
/// (ACCEPTED); T2 PENDING after T3 (RUNNING); T4 READY costing 3; T5 a qa task whose bound qa session lacks
/// command_exec. Specification s1 (revision 1) is older.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1), ('s2', 'p1', printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's2', 'RUNNING', 'AUTO', 7, '2026-11-04T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}'),
  ('sq', 'pi', 'qa', '1', '{\"structured_events\": true, \"command_exec\": false}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-04T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-04T00:00:00Z'),
  ('R', 'qa', 'pi', 'sq', 1, 1, '2026-11-04T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
  ('T0', 'R', 's2', 'ACCEPTED', 5, 'developer', 1, 1),
  ('T1', 'R', 's2', 'READY', 2, 'developer', 2, 1),
  ('T2', 'R', 's2', 'PENDING', 1, 'developer', 3, 1),
  ('T3', 'R', 's2', 'RUNNING', 3, 'developer', 4, 1),
  ('T4', 'R', 's2', 'READY', 1, 'developer', 5, 3),
  ('T5', 'R', 's2', 'READY', 1, 'qa', 6, 1),
  ('T6', 'R', 's1', 'READY', 1, 'developer', 7, 1);
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('T1', 'T0'), ('T2', 'T3');
";

fn seed(path: &Path, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

/// Role contracts (REQ-020's initial values) and a budget of 1 that can still be reserved.
fn context() -> Context {
    let caps = |names: &[&str]| names.iter().map(|s| (*s).to_owned()).collect();
    Context {
        required_capabilities: BTreeMap::from([
            (
                "developer".to_owned(),
                caps(&["structured_events", "command_exec"]),
            ),
            (
                "qa".to_owned(),
                caps(&["structured_events", "command_exec"]),
            ),
        ]),
        budget_headroom: Some(1),
        ..Context::default()
    }
}

/// Sends decision messages as the Commander of run R would, about `candidate` when it names one.
struct FakeCommander {
    sent: usize,
    candidate: Option<&'static str>,
}

impl FakeCommander {
    fn decide(&mut self, epoch: u64, revision: u64, payload: Value) -> Proposal {
        self.sent += 1;
        let message: Message = serde_json::from_value(json!({
            "schema_version": "1.0", "message_id": format!("d{}", self.sent), "run_id": "R", "task_id": "-",
            "attempt_id": "-", "sender_role": "commander", "recipient_role": "runtime", "role_epoch": epoch,
            "correlation_id": "c", "causation_id": null, "idempotency_key": format!("k{}", self.sent),
            "expected_revision": revision, "spec_sha256": sha(2), "candidate_id": self.candidate, "kind": "decision",
            "payload": payload
        }))
        .expect("a message");
        Proposal::from_message(&message).expect("a schema-valid proposal")
    }
}

type Rows = Vec<Vec<Value>>;

/// Every row of `sql`, each column as JSON.
fn rows(path: &Path, sql: &str) -> Result<Rows, Box<dyn std::error::Error>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let columns = statement.column_count();
    let out = statement
        .query_map([], |r| {
            (0..columns)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        rusqlite::types::ValueRef::Null => Value::Null,
                        rusqlite::types::ValueRef::Integer(n) => json!(n),
                        rusqlite::types::ValueRef::Real(x) => json!(x),
                        rusqlite::types::ValueRef::Text(t) | rusqlite::types::ValueRef::Blob(t) => {
                            json!(String::from_utf8_lossy(t))
                        }
                    })
                })
                .collect()
        })?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// The state AC-035-01 compares: tasks and runs (state, revision), outbox and attempts.
fn tables(path: &Path) -> Result<[Rows; 4], Box<dyn std::error::Error>> {
    Ok([
        rows(
            path,
            "SELECT task_id, state, revision FROM tasks ORDER BY task_id",
        )?,
        rows(
            path,
            "SELECT run_id, state, revision, resume_state FROM runs ORDER BY run_id",
        )?,
        rows(path, "SELECT * FROM outbox ORDER BY message_id")?,
        rows(path, "SELECT * FROM attempts ORDER BY attempt_id")?,
    ])
}

fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn test_035_01_six_invalid_proposals_are_rejected_without_changing_state() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    let before = tables(&path)?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let proposals = [
        commander.decide(
            3,
            7,
            json!({"kind": "dispatch", "task_id": "T2", "role": "developer"}),
        ),
        commander.decide(
            2,
            7,
            json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
        ),
        commander.decide(
            3,
            6,
            json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
        ),
        commander.decide(
            3,
            7,
            json!({"kind": "dispatch", "task_id": "T4", "role": "developer"}),
        ),
        commander.decide(
            3,
            7,
            json!({"kind": "dispatch", "task_id": "T5", "role": "qa"}),
        ),
        commander.decide(
            3,
            7,
            json!({"kind": "transition_task", "task_id": "T1", "to_state": "ACCEPTED"}),
        ),
    ];
    let expected = [
        RejectionReason::DependencyNotAccepted,
        RejectionReason::StaleRoleEpoch,
        RejectionReason::StaleRevision,
        RejectionReason::BudgetNotReservable,
        RejectionReason::RoleCapabilityMissing,
        RejectionReason::IllegalTransition,
    ];
    let mut seqs = Vec::new();
    for (proposal, want) in proposals.into_iter().zip(expected) {
        let id = proposal.decision_id.clone();
        match store.decide(proposal, context()).await? {
            Decided::Rejected {
                reason, event_seq, ..
            } => {
                assert_eq!(reason, want, "{id}");
                seqs.push(event_seq);
            }
            applied => return Err(format!("{id} was applied: {applied:?}").into()),
        }
    }
    let decisions = rows(
        &path,
        "SELECT decision_id, state, reason_code, role_epoch, expected_revision, payload_json FROM decisions ORDER BY rowid",
    )?;
    let events = rows(
        &path,
        "SELECT seq, event_id, kind, payload_json FROM events ORDER BY seq",
    )?;
    let after = tables(&path)?;
    keep(
        "test-035-01.json",
        &json!({"decisions": decisions, "events": events, "before": before, "after": after}),
    )?;
    let reasons: Vec<Value> = decisions.iter().map(|d| d[2].clone()).collect();
    assert_eq!(
        reasons,
        [
            "DEPENDENCY_NOT_ACCEPTED",
            "STALE_ROLE_EPOCH",
            "STALE_REVISION",
            "BUDGET_NOT_RESERVABLE",
            "ROLE_CAPABILITY_MISSING",
            "ILLEGAL_TRANSITION"
        ]
        .map(|r| json!(r))
    );
    assert!(decisions.iter().all(|d| d[1] == "REJECTED"));
    // Exactly one rejection event per decision, naming it, and no other event.
    assert_eq!(events.len(), 6);
    for (i, (event, decision)) in events.iter().zip(&decisions).enumerate() {
        assert_eq!(event[0], json!(seqs[i]));
        assert_eq!(event[2], "decision.rejected");
        let payload: Value = serde_json::from_str(event[3].as_str().unwrap_or_default())?;
        assert_eq!(payload["decision_id"], decision[0]);
        assert_eq!(payload["reason_code"], decision[2]);
    }
    // tasks/runs state and revision unchanged; no outbox or attempt rows.
    assert_eq!(after, before);
    assert!(after[2].is_empty() && after[3].is_empty());
    Ok(())
}

/// AC-035-04's world: run R VERIFYING on s2 with requirement REQ-1 (two criteria) and candidate C1 frozen on it,
/// without evidence or acceptance results.
const VERIFYING: &str = "
UPDATE runs SET state = 'VERIFYING' WHERE run_id = 'R';
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-1', 's2', 'r', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-1-01', 'REQ-1', 's2', 'b', 'unit'), ('AC-1-02', 'REQ-1', 's2', 'b', 'unit');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 2), printf('%064d', 2), printf('%064d', 3), printf('%064d', 4),
   '/repo', 'VERIFYING', '2026-11-04T00:00:00Z');
";

fn complete() -> Value {
    json!({"kind": "transition_run", "to_state": "COMPLETED", "accepted_requirements": ["REQ-1"],
           "reason": "all ACs passed; mark REQ accepted and complete the run"})
}

#[tokio::test]
async fn test_035_04_completion_without_evidence_is_rejected() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    seed(&path, VERIFYING)?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: Some("C1"),
    };
    let decided = store
        .decide(commander.decide(3, 7, complete()), context())
        .await?;
    let Decided::Rejected { reason, .. } = decided else {
        return Err(format!("applied: {decided:?}").into());
    };
    assert_eq!(reason, RejectionReason::EvidenceGateUnmet);
    let decisions = rows(
        &path,
        "SELECT decision_id, state, reason_code FROM decisions",
    )?;
    let runs = rows(&path, "SELECT run_id, state, revision FROM runs")?;
    let results = rows(&path, "SELECT count(*) FROM acceptance_results")?;
    keep(
        "test-035-04.json",
        &json!({"decisions": decisions, "runs": runs, "acceptance_results": results}),
    )?;
    assert_eq!(
        decisions,
        [vec![
            json!("d1"),
            json!("REJECTED"),
            json!("EVIDENCE_GATE_UNMET")
        ]]
    );
    assert_eq!(runs, [vec![json!("R"), json!("VERIFYING"), json!(7)]]);
    assert_eq!(results, [vec![json!(0)]]);
    Ok(())
}

/// Evidence for C1: a PASS result for `criteria` (each backed by PASS evidence) and, optionally, one more result.
fn evidence(criteria: &[&str], extra: &str) -> String {
    let mut sql = String::from(
        "INSERT INTO artifacts(artifact_id, sha256, path, bytes, mime) VALUES ('a1', printf('%064d', 9), '/e', 1, 'text/plain');
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('A1', 'T3', 'SETTLED', 1, 1, 1);
         INSERT INTO evidence(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode, created_at)
           VALUES ('E1', 'C1', 'A1', 'a1', 'unit', 'test', 'PASS', 'product', '2026-11-04T00:00:00Z'),
                  ('E2', 'C1', 'A1', 'a1', 'unit', 'test', 'FAIL', 'product', '2026-11-04T00:00:00Z'),
                  ('E3', 'C1', 'A1', 'a1', 'unit', 'test', 'PASS', 'product', '2026-11-04T00:00:00Z'),
                  ('E4', 'C1', 'A1', 'a1', 'unrelated-check', 'test', 'PASS', 'product', '2026-11-04T00:00:00Z');",
    );
    for ac in criteria {
        sql.push_str(&format!(
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status) VALUES ('{ac}', 's2', 'C1', 'E1', 'PASS');"
        ));
    }
    sql.push_str(extra);
    sql
}

#[tokio::test]
async fn completion_is_applied_only_when_one_candidate_passed_every_criterion() -> R {
    // (criteria with PASS results, extra rows, the rejection or None when applied)
    let cases = [
        (
            vec!["AC-1-01"],
            "",
            Some(RejectionReason::EvidenceGateUnmet),
        ),
        (
            vec!["AC-1-01", "AC-1-02"],
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status) VALUES ('AC-1-02', 's2', 'C1', 'E3', 'FLAKY');",
            Some(RejectionReason::EvidenceGateUnmet),
        ),
        (
            vec!["AC-1-01"],
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status) VALUES ('AC-1-02', 's2', 'C1', 'E2', 'PASS');",
            Some(RejectionReason::EvidenceGateUnmet),
        ),
        (
            vec!["AC-1-01", "AC-1-02"],
            "UPDATE candidates SET state = 'STALE';",
            Some(RejectionReason::CandidateMismatch),
        ),
        // In use but not yet under verification: its evidence does not count yet.
        (
            vec!["AC-1-01", "AC-1-02"],
            "UPDATE candidates SET state = 'FROZEN';",
            Some(RejectionReason::EvidenceGateUnmet),
        ),
        // Passing evidence from another check than the criterion's (M2T02R-03).
        (
            vec!["AC-1-01"],
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status) VALUES ('AC-1-02', 's2', 'C1', 'E4', 'PASS');",
            Some(RejectionReason::EvidenceGateUnmet),
        ),
        (vec!["AC-1-01", "AC-1-02"], "", None),
    ];
    for (i, (criteria, extra, rejection)) in cases.into_iter().enumerate() {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        seed(&path, WORLD)?;
        seed(&path, VERIFYING)?;
        seed(&path, &evidence(&criteria, extra))?;
        let mut commander = FakeCommander {
            sent: 0,
            candidate: Some("C1"),
        };
        let decided = store
            .decide(commander.decide(3, 7, complete()), context())
            .await?;
        let state = rows(&path, "SELECT state, revision FROM runs")?;
        match (rejection, decided) {
            (
                None,
                Decided::Applied {
                    run_revision: 8, ..
                },
            ) => {
                assert_eq!(state, [vec![json!("COMPLETED"), json!(8)]]);
            }
            (Some(want), Decided::Rejected { reason, .. }) if reason == want => {
                assert_eq!(state, [vec![json!("VERIFYING"), json!(7)]], "case {i}")
            }
            (_, other) => return Err(format!("case {i}: {other:?}").into()),
        }
    }
    Ok(())
}

#[tokio::test]
async fn acceptance_is_judged_on_the_named_current_candidate_only() -> R {
    // M2T02R-01: C1 passed everything and is ACCEPTED, C2 is newer and has no evidence. While both are in use the
    // current candidate is unknown; once C1 is STALE, completing on C2 needs C2's evidence and C1's cannot be named.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    seed(&path, VERIFYING)?;
    seed(&path, &evidence(&["AC-1-01", "AC-1-02"], ""))?;
    seed(
        &path,
        "UPDATE candidates SET state = 'ACCEPTED';
         INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at) VALUES
           ('C2', 'R', printf('%040d', 5), printf('%040d', 6), printf('%064d', 2), printf('%064d', 3),
            printf('%064d', 4), '/repo', 'VERIFYING', '2026-11-04T00:00:01Z');",
    )?;
    let mut sent = 0;
    let mut verdict = |candidate: Option<&'static str>| {
        sent += 1;
        let mut commander = FakeCommander { sent, candidate };
        let proposal = commander.decide(3, 7, complete());
        let store = &store;
        async move {
            match store.decide(proposal, context()).await {
                Ok(Decided::Rejected { reason, .. }) => Ok(reason),
                other => Err(format!("{candidate:?}: {other:?}")),
            }
        }
    };
    assert_eq!(
        verdict(Some("C2")).await?,
        RejectionReason::CandidateMismatch
    );
    assert_eq!(
        verdict(Some("C1")).await?,
        RejectionReason::CandidateMismatch
    );
    assert_eq!(verdict(None).await?, RejectionReason::EvidenceGateUnmet);
    seed(
        &path,
        "UPDATE candidates SET state = 'STALE' WHERE candidate_id = 'C1';",
    )?;
    assert_eq!(
        verdict(Some("C2")).await?,
        RejectionReason::EvidenceGateUnmet
    );
    assert_eq!(
        verdict(Some("C1")).await?,
        RejectionReason::CandidateMismatch
    );
    assert_eq!(
        rows(&path, "SELECT state, revision FROM runs")?,
        [vec![json!("VERIFYING"), json!(7)]]
    );
    Ok(())
}

#[tokio::test]
async fn an_applied_dispatch_leases_the_task_queues_its_message_and_fences_later_proposals() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let decided = store
        .decide(
            commander.decide(
                3,
                7,
                json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
            ),
            context(),
        )
        .await?;
    let Decided::Applied {
        run_revision: 8,
        event_seqs,
    } = decided
    else {
        return Err(format!("{decided:?}").into());
    };
    assert_eq!(
        rows(
            &path,
            "SELECT state, revision FROM tasks WHERE task_id = 'T1'"
        )?,
        [vec![json!("LEASED"), json!(3)]]
    );
    assert_eq!(rows(&path, "SELECT revision FROM runs")?, [vec![json!(8)]]);
    assert_eq!(
        rows(
            &path,
            "SELECT decision_id, state, reason_code FROM decisions"
        )?,
        [vec![json!("d1"), json!("APPLIED"), Value::Null]]
    );
    let events = rows(&path, "SELECT seq, event_id, kind FROM events")?;
    assert_eq!(
        events,
        [vec![
            json!(event_seqs[0]),
            json!("d1/applied"),
            json!("decision.applied")
        ]]
    );
    let outbox = rows(
        &path,
        "SELECT message_id, event_seq, recipient_role, role_epoch, state, payload_json FROM outbox",
    )?;
    assert_eq!(outbox.len(), 1);
    assert_eq!(
        outbox[0][..5],
        [
            json!("d1/dispatch"),
            json!(event_seqs[0]),
            json!("developer"),
            json!(2),
            json!("PENDING")
        ]
    );
    // AC-035-02: the dispatch's PREPARED attempt and its lease are in the same transaction (no event of their own);
    // T1 is not a writer task, so it holds the first reader slot.
    assert_eq!(
        rows(
            &path,
            "SELECT attempt_id, task_id, state, role_epoch FROM attempts"
        )?,
        [vec![
            json!("d1/attempt"),
            json!("T1"),
            json!("PREPARED"),
            json!(2)
        ]]
    );
    assert_eq!(
        rows(&path, "SELECT kind, attempt_id, epoch FROM leases")?,
        [vec![json!("reader-1"), json!("d1/attempt"), json!(1)]]
    );
    // The next proposal decided on revision 7 is now stale, and nothing changes for it.
    let before = tables(&path)?;
    let stale = store
        .decide(
            commander.decide(
                3,
                7,
                json!({"kind": "record_assumption", "statement": "s", "basis": "b"}),
            ),
            context(),
        )
        .await?;
    assert!(matches!(
        stale,
        Decided::Rejected {
            reason: RejectionReason::StaleRevision,
            ..
        }
    ));
    assert_eq!(tables(&path)?, before);
    Ok(())
}

#[tokio::test]
async fn task_decisions_follow_the_commanders_boundary_and_the_table() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let mut revision = 7;
    // (task, target, expected reason or None when applied)
    let cases = [
        (
            "T3",
            "IMPLEMENTED",
            Some(RejectionReason::IllegalTransition),
        ),
        ("T2", "READY", Some(RejectionReason::DependencyNotAccepted)),
        ("T2", "CANCELLED", None),
        ("T6", "PENDING", None),
        ("T9", "CANCELLED", Some(RejectionReason::IllegalTransition)),
        ("T0", "READY", Some(RejectionReason::IllegalTransition)),
    ];
    for (task, to, want) in cases {
        let proposal = commander.decide(
            3,
            revision,
            json!({"kind": "transition_task", "task_id": task, "to_state": to}),
        );
        match (store.decide(proposal, context()).await?, want) {
            (Decided::Applied { run_revision, .. }, None) => {
                revision += 1;
                assert_eq!(run_revision, i64::try_from(revision)?);
                assert_eq!(
                    rows(
                        &path,
                        &format!("SELECT state FROM tasks WHERE task_id = '{task}'")
                    )?,
                    [vec![json!(to)]]
                );
            }
            (Decided::Rejected { reason, .. }, Some(want)) => {
                assert_eq!(reason, want, "{task} -> {to}")
            }
            (other, _) => return Err(format!("{task} -> {to}: {other:?}").into()),
        }
    }
    // A blocked binding cannot take a dispatch, whatever its session can do.
    Connection::open(&path)?.execute_batch(
        "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH' WHERE role = 'developer'",
    )?;
    let blocked = commander.decide(
        3,
        revision,
        json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
    );
    assert!(matches!(
        store.decide(blocked, context()).await?,
        Decided::Rejected {
            reason: RejectionReason::RoleCapabilityMissing,
            ..
        }
    ));
    // Dispatching an old-specification task is refused.
    let old = commander.decide(
        3,
        revision,
        json!({"kind": "dispatch", "task_id": "T6", "role": "developer"}),
    );
    assert!(matches!(
        store.decide(old, context()).await?,
        Decided::Rejected {
            reason: RejectionReason::SpecMismatch,
            ..
        }
    ));
    Ok(())
}

#[tokio::test]
async fn proposals_that_cannot_be_recorded_write_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    let before = (
        tables(&path)?,
        rows(&path, "SELECT count(*) FROM decisions")?,
        rows(&path, "SELECT count(*) FROM events")?,
    );
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    // Kinds applied by later tasks.
    let ask = commander.decide(
        3,
        7,
        json!({"kind": "request_user_decision", "question_id": "q", "question": "?", "requires_user": true}),
    );
    assert!(matches!(
        store.decide(ask, context()).await,
        Err(PersistError::Unsupported("request_user_decision"))
    ));
    // A payload built in code is held to the schema too.
    let mut empty = commander.decide(
        3,
        7,
        json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
    );
    empty.payload = crate::protocol::DecisionPayload::Dispatch {
        task_id: String::new(),
        role: "developer".to_owned(),
        note: None,
    };
    assert!(matches!(
        store.decide(empty, context()).await,
        Err(PersistError::InvalidProposal(_))
    ));
    let mut unknown = commander.decide(
        3,
        7,
        json!({"kind": "record_assumption", "statement": "s", "basis": "b"}),
    );
    unknown.run_id = "no-such-run".to_owned();
    let Err(e) = store.decide(unknown, context()).await else {
        return Err("an unknown run was accepted".into());
    };
    assert_eq!(
        e.error_code(),
        Some(crate::protocol::ErrorCode::RunNotFound)
    );
    let after = (
        tables(&path)?,
        rows(&path, "SELECT count(*) FROM decisions")?,
        rows(&path, "SELECT count(*) FROM events")?,
    );
    assert_eq!(after, before);
    // The same decision id twice with other content: a key reuse, refused whole (docs/adr/ADR-025.md decision 6: no
    // longer an integrity violation).
    let first = commander.decide(
        3,
        7,
        json!({"kind": "record_assumption", "statement": "s", "basis": "b"}),
    );
    let mut again = first.clone();
    again.expected_revision = 8;
    store.decide(first, context()).await?;
    let once = (tables(&path)?, rows(&path, "SELECT count(*) FROM events")?);
    assert_eq!(
        store.decide(again, context()).await?,
        crate::persistence::Decided::KeyReuse
    );
    assert_eq!(
        (tables(&path)?, rows(&path, "SELECT count(*) FROM events")?),
        once
    );
    Ok(())
}

#[test]
fn derived_ids_stay_within_the_protocol_limit_and_distinct() {
    assert_eq!(child_id("d1", "applied"), "d1/applied");
    let long_a = "a".repeat(160);
    let mut long_b = "a".repeat(159);
    long_b.push('b');
    let (x, y) = (child_id(&long_a, "rejected"), child_id(&long_b, "rejected"));
    assert!(x.len() <= MAX_ID && y.len() <= MAX_ID, "{x} {y}");
    assert_ne!(x, y);
    assert!(x.ends_with("/rejected"));
    // Multi-byte characters are cut on a boundary.
    let wide = "é".repeat(80);
    assert!(child_id(&wide, "dispatch").len() <= MAX_ID);
    // Any suffix, however long, still gives a bounded id (M1T03R-06): suffixes are caller-supplied event names.
    for (parent, suffix) in [
        ("A1", "x".repeat(160)),
        (long_a.as_str(), "x".repeat(400)),
        (wide.as_str(), "é".repeat(200)),
        ("", String::new()),
    ] {
        let id = child_id(parent, &suffix);
        assert!(
            id.len() <= MAX_ID,
            "{} for a {}-byte suffix",
            id.len(),
            suffix.len()
        );
    }
    assert_ne!(
        child_id("A1", &"x".repeat(160)),
        child_id("A1", &format!("{}y", "x".repeat(159)))
    );
}

/// Every write point of every request, in order (ADR-019 decision 1).
#[derive(Debug, Default)]
struct Points(std::sync::Mutex<Vec<(&'static str, crate::persistence::WritePoint)>>);

impl crate::persistence::FaultHooks for Points {
    fn at(&self, job: &'static str, point: crate::persistence::WritePoint) {
        if let Ok(mut seen) = self.0.lock() {
            seen.push((job, point));
        }
    }
}

#[tokio::test]
async fn a_run_that_starts_running_queues_its_notice_with_its_event_through_every_point() -> R {
    // ADR-019 decisions 1 and 3 (AC-036-02's transition): READY -> RUNNING writes the run's move, one event and one
    // runtime notice that points at it, in one transaction that passes the five points in order.
    use crate::persistence::WritePoint;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let points = std::sync::Arc::new(Points::default());
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            faults: points.clone(),
            ..Options::default()
        },
    )?;
    seed(&path, WORLD)?;
    seed(&path, "UPDATE runs SET state = 'READY' WHERE run_id = 'R';")?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let decided = store
        .decide(
            commander.decide(
                3,
                7,
                json!({"kind": "transition_run", "to_state": "RUNNING"}),
            ),
            context(),
        )
        .await?;
    assert!(matches!(decided, Decided::Applied { .. }), "{decided:?}");
    let seen: Vec<WritePoint> = points
        .0
        .lock()
        .map_err(|_| "poisoned")?
        .iter()
        .filter(|(job, _)| *job == "Decide")
        .map(|(_, point)| *point)
        .collect();
    assert_eq!(
        seen,
        [
            WritePoint::Begun,
            WritePoint::StateWritten,
            WritePoint::EventsWritten,
            WritePoint::OutboxWritten,
            WritePoint::Committed
        ]
    );
    assert_eq!(
        rows(&path, "SELECT state, revision FROM runs WHERE run_id = 'R'")?,
        [[json!("RUNNING"), json!(8)]]
    );
    assert_eq!(
        rows(
            &path,
            "SELECT e.kind, o.recipient_role, o.state, json_extract(o.payload_json, '$.notice'), \
             json_extract(o.payload_json, '$.kind'), json_extract(o.payload_json, '$.run_id') \
             FROM outbox o JOIN events e ON e.seq = o.event_seq"
        )?,
        [[
            json!("decision.applied"),
            json!("runtime"),
            json!("PENDING"),
            json!("runtime"),
            json!("run.started"),
            json!("R")
        ]]
    );
    assert_eq!(rows(&path, "SELECT count(*) FROM events")?, [[json!(1)]]);
    Ok(())
}

#[tokio::test]
async fn a_commander_dispatch_is_refused_once_the_runs_budget_stops_it() -> R {
    // M2T19R-01, REQ-042 behaviour 6 and 7: a deadline passed, the iterations used up, or a cost limit with an unknown
    // price refuse the decide path's dispatch as they stop the rounds; no attempt is made.
    for (stop, setup) in [
        (
            "deadline_at",
            "UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';",
        ),
        (
            "max_iterations",
            "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES ('run', 'R', 'iterations', 2, 2);",
        ),
        (
            "max_usd",
            "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES ('run', 'R', 'usd', 5, 0);",
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        seed(&path, WORLD)?;
        seed(&path, setup)?;
        let mut commander = FakeCommander {
            sent: 0,
            candidate: None,
        };
        let late = Context {
            now: crate::time::parse_rfc3339("2026-11-07T00:05:00Z"),
            ..context()
        };
        let decided = store
            .decide(
                commander.decide(
                    3,
                    7,
                    json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
                ),
                late,
            )
            .await?;
        let Decided::Rejected { reason, detail, .. } = decided else {
            return Err(format!("{stop}: dispatched: {decided:?}").into());
        };
        assert_eq!(reason, RejectionReason::BudgetNotReservable, "{stop}");
        assert!(detail.contains(stop), "{stop}: {detail}");
        let attempts: i64 =
            Connection::open(&path)?
                .query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?;
        assert_eq!(attempts, 0, "{stop}");
    }
    Ok(())
}

#[tokio::test]
async fn a_commander_dispatch_is_refused_by_the_deadline_on_the_wall_clock() -> R {
    // M2T19R-251: the decide path judges deadline_at by the wall clock, not the timers' clock: with the timers before
    // the deadline and the wall clock past it the dispatch is refused.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    seed(
        &path,
        "UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';",
    )?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let context = Context {
        now: crate::time::parse_rfc3339("2026-11-07T00:00:30Z"),
        wall: crate::time::parse_rfc3339("2026-11-07T00:05:00Z"),
        ..context()
    };
    let decided = store
        .decide(
            commander.decide(
                3,
                7,
                json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
            ),
            context,
        )
        .await?;
    let Decided::Rejected { reason, detail, .. } = decided else {
        return Err(format!("dispatched: {decided:?}").into());
    };
    assert_eq!(reason, RejectionReason::BudgetNotReservable);
    assert!(detail.contains("deadline_at"), "{detail}");
    Ok(())
}

#[tokio::test]
async fn a_commander_dispatch_before_the_tasks_retry_is_refused() -> R {
    // REQ-042 behaviour 2, CR-009: the decide path reads the task's persisted next_retry_at; while it is ahead of
    // the decision's time the dispatch is RETRY_NOT_DUE, at it the dispatch goes.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    seed(&path, WORLD)?;
    seed(
        &path,
        "UPDATE tasks SET next_retry_at = '2026-11-07T00:02:00Z', attempt_count = 1 WHERE task_id = 'T1';",
    )?;
    let mut commander = FakeCommander {
        sent: 0,
        candidate: None,
    };
    let at = |text: &str| crate::time::parse_rfc3339(text).ok_or("not a time");
    let dispatch = json!({"kind": "dispatch", "task_id": "T1", "role": "developer"});
    let early = Context {
        now: Some(at("2026-11-07T00:01:59Z")?),
        ..context()
    };
    let decided = store
        .decide(commander.decide(3, 7, dispatch.clone()), early)
        .await?;
    let Decided::Rejected { reason, .. } = decided else {
        return Err(format!("dispatched before its retry: {decided:?}").into());
    };
    assert_eq!(reason, RejectionReason::RetryNotDue);
    let due = Context {
        now: Some(at("2026-11-07T00:02:00Z")?),
        ..context()
    };
    let decided = store.decide(commander.decide(3, 7, dispatch), due).await?;
    assert!(
        matches!(decided, Decided::Applied { .. }),
        "at its retry: {decided:?}"
    );
    Ok(())
}
