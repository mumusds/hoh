//! A check's evidence against a real database (docs/adr/ADR-027.md decisions 5, 7 to 9, principle two).

use super::*;
use crate::persistence::{Freeze, Frozen, Options, Store, selftest};
use crate::verification::{FileRef, Manifest, NamedRef, Runner, Status};
use rusqlite::Connection;
use serde_json::json;
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Runs R and S on spec s1 (REQ-001 required, its AC-001-01 scenario-checked by unit); R's qa task Q and developer task
/// D, S's qa task Q2, each with an attempt.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-11T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-11T00:00:00Z'),
  ('S', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-11T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-001', 's1', 'one', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-001-01', 'REQ-001', 's1', 'it works', 'unit');
INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES
  ('Q', 'R', 's1', 'RUNNING', 'qa', 1), ('D', 'R', 's1', 'RUNNING', 'developer', 2),
  ('Q2', 'S', 's1', 'RUNNING', 'qa', 1);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
  ('Q/attempt-1', 'Q', 'RUNNING', 1, 1, 1), ('D/attempt-1', 'D', 'RUNNING', 1, 1, 1),
  ('Q2/attempt-1', 'Q2', 'RUNNING', 1, 1, 1);
";

fn object(n: u8, kind: &str, name: &str) -> ArtifactRow {
    let sha256: String = format!("{n:x}").repeat(64).chars().take(64).collect();
    ArtifactRow {
        path: format!(".hoh/objects/sha256/{}/{sha256}", &sha256[..2]),
        sha256,
        bytes: 10 + u64::from(n),
        mime: "application/octet-stream".to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

/// A store holding WORLD and candidate C frozen for run R.
async fn world(dir: &Path) -> R<(Store, PathBuf, String)> {
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::default()
        },
    )?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let file = |p: &str, n: u8| FileRef {
        path: p.to_owned(),
        sha256: object(n, "", "").sha256,
    };
    let sealed = Manifest {
        git_commit: "a".repeat(40),
        git_tree: "b".repeat(40),
        spec: file("contracts/requirements.json", 1),
        environment: file("environment.json", 2),
        test_plan: file("tests/check-plan.json", 3),
        source_files: vec![file("src/lib.rs", 4)],
    }
    .seal();
    let id = sealed.candidate_id.clone();
    let frozen = store
        .freeze_candidate(Freeze {
            run_id: "R".to_owned(),
            sealed,
            export: object(9, "candidate", "candidate.json"),
            inputs: vec![
                object(1, "spec", "contracts/requirements.json"),
                object(2, "environment", "environment.json"),
                object(3, "test_plan", "tests/check-plan.json"),
            ],
        })
        .await?;
    assert!(matches!(frozen, Frozen::New { .. }));
    Ok((store, path, id))
}

/// A unit check's record `record_id` on `candidate` by `producer`, that passed.
fn recording(record_id: &str, candidate: &str, producer: &str) -> Recording {
    let dir = format!(".hoh/evidence/{candidate}/unit/{record_id}");
    let named = |name: &str, n: u8| {
        let o = object(n, "", "");
        FileRef {
            path: format!("{dir}/{name}"),
            sha256: o.sha256,
        }
    };
    let record = Record {
        schema_version: "1.1".to_owned(),
        record_type: "command_execution".to_owned(),
        record_id: record_id.to_owned(),
        check_id: "unit".to_owned(),
        candidate_id: candidate.to_owned(),
        spec_sha256: object(1, "", "").sha256,
        mode: "product".to_owned(),
        command: vec!["bash".to_owned(), "qa.sh".to_owned()],
        cwd: ".".to_owned(),
        started_at: "2026-11-11T10:00:00Z".to_owned(),
        finished_at: "2026-11-11T10:00:05Z".to_owned(),
        duration_seconds: 5.0,
        exit_code: 0,
        timed_out: false,
        stdout: named("stdout.log", 5),
        stderr: named("stderr.log", 6),
        candidate_manifest: FileRef {
            path: object(9, "", "").path,
            sha256: object(9, "", "").sha256,
        },
        source_unchanged: true,
        environment_sha256: object(2, "", "").sha256,
        test_plan_sha256: object(3, "", "").sha256,
        artifacts: vec![NamedRef {
            name: "test-results.jsonl".to_owned(),
            path: format!("{dir}/artifacts/test-results.jsonl"),
            sha256: object(7, "", "").sha256,
        }],
        missing_artifacts: Vec::new(),
        producer_attempt_id: producer.to_owned(),
        runner: Runner {
            tool: "hoh".to_owned(),
            euid: 1000,
            boot_id: None,
        },
    };
    Recording {
        record,
        record_object: object(8, "execution", "execution.json"),
        objects: vec![
            object(5, "stdout", "stdout.log"),
            object(6, "stderr", "stderr.log"),
            object(7, "artifact", "test-results.jsonl"),
        ],
        outcome: Outcome {
            status: Status::Pass,
            reason: None,
        },
        reasons: Vec::new(),
        tree_before: json!({"head": "a", "tree": "b"}),
        tree_after: json!({"head": "a", "tree": "b"}),
        observations: vec![json!({"test_id": "TEST-001-01", "status": "PASS"})],
        rows: vec![AcceptanceRow {
            ac_id: "AC-001-01".to_owned(),
            spec_id: "s1".to_owned(),
            status: "PASS",
            reason: "TEST-001-01 case: PASS".to_owned(),
        }],
        integrity: Vec::new(),
    }
}

fn texts(path: &Path, sql: &str) -> R<Vec<String>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

#[tokio::test]
async fn a_passing_evidence_writes_its_row_its_acceptance_rows_its_objects_and_its_producer_event()
-> R {
    let dir = tempfile::tempdir()?;
    let (store, path, c) = world(dir.path()).await?;
    let job = recording("r1", &c, "Q/attempt-1");
    store.record_evidence(job.clone()).await?;
    assert_eq!(
        texts(
            &path,
            "SELECT json_array(evidence_id, candidate_id, attempt_id, artifact_id, check_id, kind, outcome, mode) \
             FROM evidence"
        )?,
        vec![
            json!([
                "r1",
                c,
                "Q/attempt-1",
                format!("{}:{}", job.record_object.sha256, job.record_object.path),
                "unit",
                "check",
                "PASS",
                "product"
            ])
            .to_string()
        ]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT json_array(ac_id, spec_id, candidate_id, evidence_id, status, reason) FROM acceptance_results"
        )?,
        vec![json!(["AC-001-01", "s1", c, "r1", "PASS", "TEST-001-01 case: PASS"]).to_string()]
    );
    // The record and its outputs are registered objects.
    let registered = texts(&path, "SELECT path FROM artifacts")?;
    for o in std::iter::once(&job.record_object).chain(&job.objects) {
        assert!(registered.contains(&o.path), "{} registered", o.name);
    }
    // One evidence.recorded event, the producer's registration of this record (decision 8).
    let events = texts(
        &path,
        "SELECT json_array(event_id, run_id, json(payload_json)) FROM events WHERE kind = 'evidence.recorded'",
    )?;
    assert_eq!(events.len(), 1);
    let event: Value = serde_json::from_str(&events[0])?;
    assert_eq!(event[0], "r1/evidence");
    assert_eq!(event[1], "R");
    let payload = &event[2];
    assert_eq!(payload["evidence_id"], "r1");
    assert_eq!(payload["record_id"], "r1");
    assert_eq!(payload["attempt_id"], "Q/attempt-1");
    assert_eq!(payload["check_id"], "unit");
    assert_eq!(payload["outcome"], "PASS");
    assert_eq!(payload["tree_before"], job.tree_before);
    assert_eq!(payload["observations"], json!(job.observations));
    assert_eq!(payload["reasons"], json!([]));
    assert_eq!(payload["artifacts"].as_array().map(Vec::len), Some(4));
    // Nothing else: the candidate is as it was, no integrity event.
    assert_eq!(
        texts(&path, "SELECT state FROM candidates WHERE run_id = 'R'")?,
        ["FROZEN"]
    );
    assert!(
        texts(
            &path,
            "SELECT event_id FROM events WHERE kind = 'integrity_violation'"
        )?
        .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn an_invalid_evidence_vetoes_its_candidate_for_good_and_a_changed_source_raises_integrity()
-> R {
    let dir = tempfile::tempdir()?;
    let (store, path, c) = world(dir.path()).await?;
    let mut job = recording("r1", &c, "Q/attempt-1");
    job.record.source_unchanged = false;
    job.outcome = Outcome {
        status: Status::Invalid,
        reason: None,
    };
    job.reasons = vec!["SOURCE_CHANGED"];
    job.tree_after = json!({"head": "a", "tree": "b", "diff": "c"});
    job.rows = vec![AcceptanceRow {
        ac_id: "AC-001-01".to_owned(),
        spec_id: "s1".to_owned(),
        status: "INVALID",
        reason: "SOURCE_CHANGED".to_owned(),
    }];
    job.integrity = vec!["source changed: src/lib.rs".to_owned()];
    store.record_evidence(job.clone()).await?;
    assert_eq!(texts(&path, "SELECT outcome FROM evidence")?, ["INVALID"]);
    assert_eq!(
        texts(&path, "SELECT status FROM acceptance_results")?,
        ["INVALID"]
    );
    assert_eq!(
        texts(&path, "SELECT state FROM candidates WHERE run_id = 'R'")?,
        ["INVALID"]
    );
    let integrity = texts(
        &path,
        "SELECT json_array(event_id, json(payload_json)) FROM events WHERE kind = 'integrity_violation'",
    )?;
    assert_eq!(integrity.len(), 1);
    let event: Value = serde_json::from_str(&integrity[0])?;
    assert_eq!(
        event,
        json!(["r1/integrity", {"candidate_id": c, "check_id": "unit", "record_id": "r1",
               "reasons": ["source changed: src/lib.rs"], "tree_before": job.tree_before,
               "tree_after": job.tree_after}])
    );
    // A later passing evidence does not revive it: INVALID is terminal.
    store
        .record_evidence(recording("r2", &c, "Q/attempt-1"))
        .await?;
    assert_eq!(
        texts(&path, "SELECT state FROM candidates WHERE run_id = 'R'")?,
        ["INVALID"]
    );
    Ok(())
}

#[tokio::test]
async fn evidence_from_no_qa_or_reviewer_attempt_of_the_candidates_run_is_refused_and_writes_nothing()
-> R {
    // Principle two, checked again at insert (the pre-check checks first, M2T10D-252, -256).
    let dir = tempfile::tempdir()?;
    let (store, path, c) = world(dir.path()).await?;
    for (producer, named) in [
        ("D/attempt-1", "PRODUCER_NOT_INDEPENDENT"),
        ("Q2/attempt-1", "PRODUCER_MISMATCH"),
        ("nobody", "PRODUCER_MISSING"),
    ] {
        let refused = store.record_evidence(recording("r1", &c, producer)).await;
        let Err(e) = refused else {
            return Err(format!("{producer}: recorded").into());
        };
        assert!(e.to_string().contains(named), "{producer}: {e}");
    }
    let unknown = store
        .record_evidence(recording("r1", "CAND-unknown", "Q/attempt-1"))
        .await;
    assert!(unknown.is_err(), "{unknown:?}");
    assert!(texts(&path, "SELECT evidence_id FROM evidence")?.is_empty());
    assert!(
        texts(
            &path,
            "SELECT event_id FROM events WHERE kind <> 'candidate.frozen'"
        )?
        .is_empty()
    );
    Ok(())
}

#[tokio::test]
async fn a_handover_references_an_attempts_check_outputs_with_its_turn_artifacts_deduplicated_and_limited()
-> R {
    // docs/adr/ADR-027.md decision 8 (M2T10D-102, -161, -259; ADR-026 decision 10's note): the reference reader merges
    // an attempt's attempt.artifacts with each of its evidence.recorded, in event order, without duplicates, up to the
    // limit.
    let dir = tempfile::tempdir()?;
    let (store, path, c) = world(dir.path()).await?;
    store
        .record_evidence(recording("r1", &c, "Q/attempt-1"))
        .await?;
    let mut second = recording("r2", &c, "Q/attempt-1");
    second.record_object = object(10, "execution", "execution.json");
    second.objects.push(object(11, "artifact", "report.json"));
    store.record_evidence(second.clone()).await?;
    // The turn's own artifacts, written at its end, name one object the checks named too.
    let turn = json!({"items": [object(5, "stdout", "stdout.log").json(), object(12, "raw_log", "stdout.jsonl").json()]});
    Connection::open(&path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
         VALUES ('p1', 'Q/attempt-1/artifacts', 'R', 'attempt.artifacts', ?1, '2026-11-11T11:00:00Z')",
        [turn.to_string()],
    )?;
    let conn = Connection::open(&path)?;
    let attempts = ["Q/attempt-1".to_owned()];
    let all = crate::persistence::context::references(&conn, &attempts, 100)?;
    let paths: Vec<String> = all.iter().map(|r| r.path.clone()).collect();
    // r1's record and outputs, then r2's (its stdout and stderr are r1's objects: once), then the turn's raw log.
    let expected: Vec<String> = [8, 5, 6, 7, 10, 11, 12]
        .iter()
        .map(|n| object(*n, "", "").path)
        .collect();
    assert_eq!(paths, expected);
    let limited = crate::persistence::context::references(&conn, &attempts, 3)?;
    assert_eq!(limited.len(), 3);
    Ok(())
}
