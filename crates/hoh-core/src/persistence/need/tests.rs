//! docs/adr/ADR-031.md correction A's guard against a real database: the typed reasons (22 task-level pairs, 2
//! run-level), the only builder of a blocking `need_decision` refusing a variant that does not fit its task, and the
//! scan that keeps every `need_decision` written here (its listed readers aside). The compile-time half of the mutation
//! (`VerifierReason::LocalRepairsExhausted` does not exist) is need.rs's `compile_fail` doc test.

use super::*;
use crate::persistence::retry::{Repair, RepairRequest};
use crate::persistence::{Options, PersistError, Store, selftest};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R RUNNING with development task D (VERIFYING, a source of round R1 on candidate C), its verifier tasks Q (qa)
/// and V (reviewer), and development task W that no round names.
const ROUND: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-15T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-15T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('D', 'R', 's1', 'VERIFYING', 2, 'developer', 1, 1),
  ('W', 'R', 's1', 'READY', 0, 'developer', 1, 2),
  ('Q', 'R', 's1', 'RUNNING', 1, 'qa', 0, 3),
  ('V', 'R', 's1', 'READY', 0, 'reviewer', 0, 4);
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'R1/opened', 'R', 'verification.round_opened',
   '{\"round_id\": \"R1\", \"candidate_id\": \"C\", \"number\": 1, \"source_task_ids\": [\"D\"],
     \"tasks\": {\"qa\": \"Q\", \"reviewer\": \"V\"}}', '2026-11-15T00:00:00Z');";

fn events(path: &Path) -> R<i64> {
    Ok(Connection::open(path)?.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?)
}

fn need(task_id: &str, reason: TaskReason) -> TaskNeed<'_> {
    TaskNeed {
        project_id: "p1",
        run_id: "R",
        task_id,
        event_id: format!("{task_id}/need"),
        reason,
        fields: json!({"detail": "d"}),
    }
}

#[test]
fn the_typed_reasons_are_the_writers_twenty_three_pairs_and_three_run_reasons() {
    // Correction A's guard (1): work 12 (dirty_workspace: docs/adr/ADR-039.md decision 7), source 3, verifier 8; run
    // no_spec, stalled and baseline_tests. Each name parses back to its
    // variant, and a reason only some subjects have has a variant only in their enums.
    assert_eq!(
        (
            WorkReason::ALL.len(),
            SourceReason::ALL.len(),
            VerifierReason::ALL.len(),
            TaskReason::all().len(),
            RunReason::ALL.len()
        ),
        (12, 3, 8, 23, 6)
    );
    // Each ALL holds every variant once, in its position (a variant left out of ALL has a position past its end).
    let positions = |all: Vec<usize>| all.iter().enumerate().all(|(i, p)| i == *p);
    assert!(positions(
        WorkReason::ALL.iter().map(|r| r.position()).collect()
    ));
    assert!(positions(
        SourceReason::ALL.iter().map(|r| r.position()).collect()
    ));
    assert!(positions(
        VerifierReason::ALL.iter().map(|r| r.position()).collect()
    ));
    assert!(positions(
        RunReason::ALL.iter().map(|r| r.position()).collect()
    ));
    assert_eq!(
        (
            WorkReason::DirtyWorkspace.position(),
            SourceReason::Flaky.position(),
            VerifierReason::ReportRejected.position(),
            RunReason::BuildReadiness.position()
        ),
        (11, 2, 7, 5),
        "the last variant of each closes its ALL"
    );
    for reason in TaskReason::all() {
        let verifier = matches!(reason, TaskReason::Verifier(_));
        assert_eq!(
            TaskReason::of(verifier, reason.as_str()),
            Some(reason),
            "{reason:?}"
        );
    }
    for reason in RunReason::ALL {
        assert_eq!(RunReason::parse(reason.as_str()), Some(*reason));
    }
    assert_eq!(VerifierReason::parse("local_repairs_exhausted"), None);
    assert_eq!(VerifierReason::parse("flaky"), None);
    assert_eq!(WorkReason::parse("flaky"), None);
    assert_eq!(WorkReason::parse("reverify_exhausted"), None);
    assert_eq!(TaskReason::of(false, "repair_refused"), None);
    assert_eq!(TaskReason::of(true, "join_gate_refused"), None);
}

#[test]
fn a_variant_that_does_not_fit_its_task_is_refused_and_nothing_is_written() -> R {
    // Correction A's runtime mutation: a test-only caller passes a work reason for a verifier task, a source reason for
    // a task no round names, a verifier reason for a development task: IntegrityViolation, nothing written (the
    // transaction rolls back). The fitting ones are built.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    drop(store);
    let mut conn = Connection::open(&path)?;
    conn.execute_batch(ROUND)?;
    let before = events(&path)?;
    for (task, reason) in [
        ("Q", TaskReason::Work(WorkReason::LocalRepairsExhausted)),
        ("V", TaskReason::Work(WorkReason::RetriesExhausted)),
        ("W", TaskReason::Source(SourceReason::Flaky)),
        ("Q", TaskReason::Source(SourceReason::ReverifyExhausted)),
        (
            "D",
            TaskReason::Verifier(VerifierReason::VerifierUnknownEffect),
        ),
        ("W", TaskReason::Verifier(VerifierReason::InvalidHandoff)),
    ] {
        let tx = conn.transaction()?;
        let built = task_event(&tx, need(task, reason));
        assert!(
            matches!(&built, Err(PersistError::IntegrityViolation(_))),
            "{task} {reason:?}: {built:?}"
        );
        drop(tx);
        assert_eq!(events(&path)?, before, "{task} {reason:?}");
    }
    for (task, reason) in [
        ("W", TaskReason::Work(WorkReason::AgentFailed)),
        ("D", TaskReason::Work(WorkReason::RetriesExhausted)),
        ("D", TaskReason::Source(SourceReason::JoinGateRefused)),
        ("Q", TaskReason::Verifier(VerifierReason::RetriesExhausted)),
    ] {
        let built = task_event(&conn, need(task, reason))?;
        assert_eq!(
            (
                built.kind.as_str(),
                built.payload["reason"].as_str(),
                built.payload["task_id"].as_str()
            ),
            (KIND, Some(reason.as_str()), Some(task)),
            "{reason:?}"
        );
        assert_eq!(built.payload["detail"], "d");
    }
    Ok(())
}

#[test]
fn a_verifiers_local_repair_is_refused_and_its_refusals_are_typed_by_its_task() -> R {
    // Repair::exhausted by the task's kind: a local repair's refusal of a verifier is an IntegrityViolation with
    // nothing written; a protocol correction's of a verifier is its question, of a development task the work's; a
    // reverification's of a source its source question.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    drop(store);
    let mut conn = Connection::open(&path)?;
    conn.execute_batch(ROUND)?;
    let ask = |task: &str, repair: Repair| RepairRequest {
        run_id: "R".to_owned(),
        task_id: task.to_owned(),
        repair,
        limit: 0,
        detail: "d".to_owned(),
        action_id: None,
        hold: true,
    };
    let tx = conn.transaction()?;
    let refused = crate::persistence::retry::repair_in(
        &tx,
        &ask(
            "Q",
            Repair::Local {
                fingerprint: "f".to_owned(),
            },
        ),
    );
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    drop(tx);
    for (task, repair, reason) in [
        (
            "Q",
            Repair::Protocol {
                attempt_id: "a".to_owned(),
            },
            "protocol_repairs_exhausted",
        ),
        (
            "W",
            Repair::Protocol {
                attempt_id: "a".to_owned(),
            },
            "protocol_repairs_exhausted",
        ),
        (
            "D",
            Repair::Reverify {
                root: "R1".to_owned(),
            },
            "reverify_exhausted",
        ),
        (
            "V",
            Repair::Reverify {
                root: "R1".to_owned(),
            },
            "reverify_exhausted",
        ),
    ] {
        let (_, commit) = crate::persistence::retry::repair_in(&conn, &ask(task, repair))?;
        let need = commit
            .events
            .iter()
            .find(|e| e.kind == KIND)
            .ok_or("a refusal's question")?;
        assert_eq!(need.payload["reason"], reason, "{task}");
    }
    // W (a task no round names) has no reverification to refuse: its source question does not fit.
    let tx = conn.transaction()?;
    let refused = crate::persistence::retry::repair_in(
        &tx,
        &ask(
            "W",
            Repair::Reverify {
                root: "x".to_owned(),
            },
        ),
    );
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    Ok(())
}

/// The workspace's product sources (hoh-core and hoh-app), test files and inline test modules left out, comments
/// dropped: (path, text).
fn product_sources() -> R<Vec<(PathBuf, String)>> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    let mut stack = vec![crates.join("hoh-core/src"), crates.join("hoh-app/src")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                let text = std::fs::read_to_string(&path)?;
                let product = match text.find("#[cfg(test)]\nmod tests {") {
                    Some(at) => &text[..at],
                    None => &text,
                };
                let product = product
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                found.push((path, product));
            }
        }
    }
    found.sort();
    Ok(found)
}

#[test]
fn every_need_decision_is_written_here_and_read_only_where_listed() -> R {
    // Correction A's guard (4), the scan: the Rust literal "need_decision" appears in the product only in need.rs and
    // in its listed readers (the supervisor's KINDS, the classification, the wake's urgency, the deferred questions'
    // filter and a repair's replayed verdict), once each; a writer that builds the event itself turns this red.
    let allowed = [
        ("hoh-core/src/persistence/need.rs", 1),
        ("hoh-core/src/persistence/supervise.rs", 1),
        ("hoh-core/src/supervise.rs", 1),
        ("hoh-core/src/persistence/wakes.rs", 1),
        ("hoh-core/src/persistence/verification.rs", 1),
        ("hoh-core/src/persistence/retry.rs", 1),
    ];
    let mut found = Vec::new();
    for (path, text) in product_sources()? {
        let n = text.matches("\"need_decision\"").count();
        if n > 0 {
            let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
            let name = path.strip_prefix(&crates).unwrap_or(&path);
            found.push((name.display().to_string(), n));
        }
    }
    let mut want: Vec<(String, usize)> =
        allowed.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect();
    want.sort();
    found.sort();
    assert_eq!(found, want);
    Ok(())
}
