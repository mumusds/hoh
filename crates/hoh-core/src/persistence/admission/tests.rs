//! The dispatch admission's coverage (docs/adr/ADR-035.md decision 12, M3J-100): the places that create an attempt,
//! found by searching the workspace's production sources, are exactly the ones the design lists, and each goes
//! through the admission. A new path that creates an attempt fails here until it is listed and admitted. The
//! behaviour of each path under an approval between the readiness step and a dispatch is tested in
//! `spec_review/open/tests.rs` (`test_031_04_every_attempt_creating_path_refuses_*`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type R = Result<(), Box<dyn std::error::Error>>;

/// Every production `.rs` file of the workspace's crates (test files and test directories left out), relative to
/// `crates/`, with its text.
fn sources() -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("no crates directory")?
        .to_path_buf();
    let mut found = Vec::new();
    let mut stack: Vec<PathBuf> = std::fs::read_dir(&crates)?
        .map(|e| e.map(|e| e.path().join("src")))
        .collect::<Result<_, _>>()?;
    while let Some(dir) = stack.pop() {
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if path.is_dir() {
                if name != "tests" {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") && name != "tests.rs" {
                let relative = path.strip_prefix(&crates)?.to_string_lossy().into_owned();
                found.push((relative, std::fs::read_to_string(&path)?));
            }
        }
    }
    found.sort();
    Ok(found)
}

/// How often each file contains `needle` (`text` is lowercased with whitespace collapsed when `sql`).
fn occurrences(sources: &[(String, String)], needle: &str, sql: bool) -> BTreeMap<String, usize> {
    sources
        .iter()
        .filter_map(|(path, text)| {
            let n = if sql {
                text.to_lowercase()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .matches(needle)
                    .count()
            } else {
                text.matches(needle).count()
            };
            (n > 0).then(|| (path.clone(), n))
        })
        .collect()
}

fn listed(entries: &[(&str, usize)]) -> BTreeMap<String, usize> {
    entries.iter().map(|(p, n)| ((*p).to_owned(), *n)).collect()
}

/// The body of `fn <name>(` in `text`, up to the next top-level item.
fn body<'a>(text: &'a str, name: &str) -> Result<&'a str, Box<dyn std::error::Error>> {
    let start = text
        .find(&format!("fn {name}("))
        .ok_or_else(|| format!("no fn {name}"))?;
    let rest = &text[start..];
    let end = rest.find("\n}\n").ok_or("unterminated fn")?;
    Ok(&rest[..end])
}

#[test]
fn test_031_04_every_attempt_creating_path_goes_through_the_admission() -> R {
    let sources = sources()?;
    let text = |path: &str| -> Result<&str, Box<dyn std::error::Error>> {
        sources
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t.as_str())
            .ok_or_else(|| format!("no {path}").into())
    };
    const ATTEMPTS: &str = "hoh-core/src/persistence/attempts.rs";
    const DECIDE: &str = "hoh-core/src/persistence/decide.rs";
    // The one SQL statement that creates an attempt row, in `attempts::insert_prepared`.
    assert_eq!(
        occurrences(&sources, "into attempts", true),
        listed(&[(ATTEMPTS, 1)]),
        "an attempt row is inserted only by attempts::insert_prepared"
    );
    assert!(body(text(ATTEMPTS)?, "insert_prepared")?.contains("INSERT INTO attempts"));
    // insert_prepared: defined, called by attempts::create and by decide::lease_task only.
    assert_eq!(
        occurrences(&sources, "insert_prepared(", false),
        listed(&[(ATTEMPTS, 2), (DECIDE, 1)])
    );
    assert!(body(text(DECIDE)?, "lease_task")?.contains("attempts::insert_prepared("));
    let create = body(text(ATTEMPTS)?, "create")?;
    let admitted = create
        .find("admission::admit_one(")
        .ok_or("attempts::create does not call the admission")?;
    assert!(
        admitted < create.find("insert_prepared(").ok_or("no insert")?,
        "attempts::create admits before it inserts"
    );
    // lease_task: defined and called by the scheduling round, the Commander's dispatch decision and the supervision
    // turn; the first two hold an admission from `admit`, the last one from `supervision` (its task the Commander's).
    assert_eq!(
        occurrences(&sources, "lease_task(", false),
        listed(&[
            (DECIDE, 2),
            ("hoh-core/src/persistence/rounds.rs", 1),
            ("hoh-core/src/persistence/wakes.rs", 1),
        ])
    );
    assert!(body(text(DECIDE)?, "decide_in")?.contains("admission::admit_one("));
    assert!(
        body(text("hoh-core/src/persistence/rounds.rs")?, "round")?.contains("admission::admit(")
    );
    assert!(text("hoh-core/src/persistence/wakes.rs")?.contains("admission::supervision("));
    assert!(body(text(DECIDE)?, "lease_task")?.contains("request.admission.task_id()"));
    // attempts::create: called by the attempt job (Store::create_attempt) and a consumer's effect only.
    assert_eq!(
        occurrences(&sources, "attempts::create(", false),
        listed(&[("hoh-core/src/persistence/outbox.rs", 1)])
    );
    assert_eq!(
        occurrences(&sources, "=> create(", false),
        listed(&[(ATTEMPTS, 1)])
    );
    assert_eq!(
        occurrences(&sources, "AttemptJob::Create(", false),
        listed(&[(ATTEMPTS, 1), ("hoh-core/src/persistence/mod.rs", 1)])
    );
    Ok(())
}

/// A supervision admission is the Commander's own turn only: no work task is leased under one.
#[tokio::test]
async fn a_supervision_admission_is_refused_to_a_work_task() -> R {
    use crate::persistence::{Options, Store, selftest};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let _store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-22T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%.64c', '1'), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
           ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-22T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
           ('W', 'R', 's1', 'READY', 0, 'developer', 1, 1), ('C', 'R', 's1', 'LEASED', 0, 'commander', 0, 2);",
    )?;
    assert!(matches!(
        super::supervision(&conn, "W"),
        Err(crate::persistence::PersistError::IntegrityViolation(_))
    ));
    assert_eq!(super::supervision(&conn, "C")?.task_id(), "C");
    Ok(())
}

/// Run R RUNNING on revision 1 (s1, its CURRENT PASS review), its Commander and developer bound; planned tasks of
/// REQ-A, all READY: writer W (scope `src`), reader Rd (scope `src`), writer W2 (scope `docs`); the run's baseline
/// lists `src/lib.rs` as uncommitted.
const DIRTY_WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-22T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%.64c', '1'), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-22T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, native_id, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', 'N-cmd', '1', '{}'), ('sd', 'claude', 'developer', NULL, '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-22T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('W', 'R', 's1', 'READY', 0, 'developer', 1, 1), ('Rd', 'R', 's1', 'READY', 0, 'developer', 0, 2),
  ('W2', 'R', 's1', 'READY', 0, 'developer', 1, 3);
INSERT INTO requirements(requirement_id, spec_id, title, required_release, release_class) VALUES
  ('REQ-A', 's1', 'A', 1, 'required');
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-A-01', 'REQ-A', 's1', json_object('id', 'AC-A-01', 'given', 'g', 'when', 'w', 'then', 't',
   'test_id', 'TEST-A-01'), 'unit');
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES
  ('W', 'REQ-A', 's1'), ('Rd', 'REQ-A', 's1'), ('W2', 'REQ-A', 's1');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'plan', 'R', 'plan.imported', json_object('tasks', json_array(
     json_object('task_id', 'W', 'paths', json_array('src')),
     json_object('task_id', 'Rd', 'paths', json_array('src')),
     json_object('task_id', 'W2', 'paths', json_array('docs')))), '2026-11-22T00:00:00Z'),
  ('p1', 'pass', 'R', 'spec_review.reported', json_object('spec_id', 's1', 'spec_revision', 1,
   'state', 'CURRENT', 'status', 'PASS', 'findings', json_array()), '2026-11-22T00:00:00Z'),
  ('p1', 'R/baseline', 'R', 'run.baseline', json_object('head', NULL, 'tree', NULL,
   'dirty', json_array(json_object('path', 'src/lib.rs', 'sha256', NULL)), 'untracked', json_array(),
   'build_commands', json_array('unknown'), 'test_commands', json_array('unknown'), 'docs', json_array()),
   '2026-11-22T00:00:00Z');
";

/// The Commander of [`DIRTY_WORLD`] proposing to dispatch `task` to the developer at the run's current revision.
fn dirty_dispatch(
    conn: &rusqlite::Connection,
    id: &str,
    task: &str,
) -> Result<crate::decision::Proposal, Box<dyn std::error::Error>> {
    let (revision, sha): (i64, String) = conn.query_row(
        "SELECT r.revision, s.sha256 FROM runs r JOIN specs s ON s.spec_id = r.spec_id",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let message: crate::protocol::Message = serde_json::from_value(serde_json::json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 1, "correlation_id": "c",
        "causation_id": null, "idempotency_key": id, "expected_revision": revision, "spec_sha256": sha,
        "candidate_id": null, "kind": "decision",
        "payload": {"kind": "dispatch", "task_id": task, "role": "developer"}
    }))?;
    Ok(crate::decision::Proposal::from_message(&message)?)
}

/// REQ-028 behaviour 4 (AC-028-03; docs/adr/ADR-039.md decision 7): the dispatch admission holds a write task whose
/// path scope holds a file the run's baseline lists as uncommitted, on every path that creates a work attempt. `admit`
/// refuses W with those files (readers and writers outside the files pass); the Commander's dispatch of W is REJECTED
/// ILLEGAL_TRANSITION with `details.paths`, W left READY; a direct creation is refused; the scheduling round blocks W
/// for the user's decision (`dirty_workspace`, asked once) and dispatches none of it. Once asked, the answer decides:
/// the admission no longer holds W.
#[tokio::test]
async fn test_028_03_the_admission_holds_a_write_task_on_dirty_files_on_every_dispatch_path() -> R {
    use crate::persistence::{Decided, NewAttempt, Options, PersistError, Store, selftest};
    use serde_json::json;
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let conn = rusqlite::Connection::open(&path)?;
    conn.execute_batch(DIRTY_WORLD)?;
    let held = json!({"paths": ["src/lib.rs"]});
    // The admission itself, for the round and the Commander alike.
    for by in [super::By::Round, super::By::Commander, super::By::Direct] {
        let verdicts = super::admit(&conn, "R", &["W", "Rd", "W2"], by)?;
        let refusal = verdicts["W"]
            .as_ref()
            .err()
            .ok_or(format!("{by:?}: W admitted"))?;
        let rejection = refusal.rejection("W");
        assert_eq!(
            rejection.reason,
            crate::protocol::RejectionReason::IllegalTransition,
            "{by:?}"
        );
        assert_eq!(rejection.details.as_ref(), Some(&held), "{by:?}");
        assert!(
            rejection.detail.contains("src/lib.rs"),
            "{by:?}: {}",
            rejection.detail
        );
        assert!(verdicts["Rd"].is_ok(), "{by:?}: a reader is not held");
        assert!(
            verdicts["W2"].is_ok(),
            "{by:?}: a writer outside the files is not held"
        );
    }
    let context = || crate::decision::Context {
        required_capabilities: ["developer"]
            .map(|role| (role.to_owned(), Default::default()))
            .into(),
        now: Some(std::time::SystemTime::now()),
        ..crate::decision::Context::default()
    };
    // The Commander's dispatch: REJECTED, W untouched and not asked about (the round asks).
    let decided = store
        .decide(dirty_dispatch(&conn, "d-W", "W")?, context())
        .await?;
    let Decided::Rejected { reason, .. } = decided else {
        return Err(format!("the Commander's dispatch of W: {decided:?}").into());
    };
    assert_eq!(reason, crate::protocol::RejectionReason::IllegalTransition);
    let rejected: String = conn.query_row(
        "SELECT payload_json FROM events WHERE kind = 'decision.rejected' ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let rejected: serde_json::Value = serde_json::from_str(&rejected)?;
    assert_eq!(rejected["details"], held, "{rejected}");
    let state = |task: &str| -> Result<String, rusqlite::Error> {
        conn.query_row(
            "SELECT state || '/' || coalesce(blocked_reason, '') FROM tasks WHERE task_id = ?1",
            [task],
            |r| r.get(0),
        )
    };
    assert_eq!(state("W")?, "READY/");
    // A direct creation: refused, nothing written.
    let created = store
        .create_attempt(NewAttempt {
            attempt_id: "A-direct".to_owned(),
            task_id: "W".to_owned(),
            session_id: None,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 1,
            agent: Some("claude".to_owned()),
        })
        .await;
    let Err(PersistError::IllegalTransition { details, .. }) = created else {
        return Err(format!("a direct creation of W: {created:?}").into());
    };
    assert_eq!(details.as_deref(), Some(&held));
    // The scheduling round: W BLOCKED for the user's decision with the files, no attempt of it.
    let outcome = store.schedule_round("R", context()).await?;
    assert_ne!(outcome.dispatched.map(|d| d.task_id).as_deref(), Some("W"));
    assert_eq!(state("W")?, "BLOCKED/NEED_DECISION");
    let need: String = conn.query_row(
        "SELECT payload_json FROM events WHERE kind = 'need_decision' \
         AND json_extract(payload_json, '$.task_id') = 'W'",
        [],
        |r| r.get(0),
    )?;
    let need: serde_json::Value = serde_json::from_str(&need)?;
    assert_eq!(
        (&need["reason"], &need["paths"]),
        (&json!("dirty_workspace"), &held["paths"]),
        "{need}"
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM attempts WHERE task_id = 'W'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        0
    );
    // Asked once: the user's answer decides, the admission no longer holds W.
    assert!(super::admit_one(&conn, "R", "W", super::By::Commander)?.is_ok());
    Ok(())
}

/// The dirty-file hold is one of the admission's reasons, judged nowhere else (docs/adr/ADR-039.md decision 7, ADR-035
/// decision 12): the run's uncommitted files are read by the admission only, so no dispatch path keeps a private check.
#[test]
fn test_028_03_the_dirty_file_hold_is_judged_in_the_admission_only() -> R {
    let sources = sources()?;
    assert_eq!(
        occurrences(&sources, "baseline::uncommitted(", false),
        listed(&[("hoh-core/src/persistence/admission.rs", 1)])
    );
    assert_eq!(
        occurrences(&sources, "baseline::touched(", false),
        listed(&[("hoh-core/src/persistence/admission.rs", 1)])
    );
    Ok(())
}
