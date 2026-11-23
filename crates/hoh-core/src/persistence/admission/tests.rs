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
            ("hoh-core/src/persistence/drafting.rs", 1),
            ("hoh-core/src/persistence/wakes.rs", 1),
        ])
    );
    assert!(body(text(DECIDE)?, "decide_in")?.contains("admission::admit_one("));
    let draft = body(text("hoh-core/src/persistence/drafting.rs")?, "dispatch")?;
    assert!(
        draft.find("admission::admit(").ok_or("draft admission")?
            < draft.find("decide::lease_task(").ok_or("draft lease")?
    );
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
/// for the user's decision (`dirty_workspace`, asked once) and dispatches none of it. Asked, W is still held: only the
/// answer releases it (`test_028_03_asking_is_no_consent_until_the_user_answers`).
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
    // Asked once: asking is no consent, the admission still holds W until the user answers (M3N-052).
    assert!(super::admit_one(&conn, "R", "W", super::By::Commander)?.is_err());
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

/// A store at `dir` holding [`DIRTY_WORLD`] with W's path scope `scope`, and a connection to it.
fn dirty_store(
    dir: &Path,
    scope: &str,
) -> Result<(crate::persistence::Store, rusqlite::Connection), Box<dyn std::error::Error>> {
    use crate::persistence::{Options, Store, selftest};
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let conn = rusqlite::Connection::open(&path)?;
    let world = DIRTY_WORLD.replace(
        "json_object('task_id', 'W', 'paths', json_array('src'))",
        &format!("json_object('task_id', 'W', 'paths', json_array('{scope}'))"),
    );
    assert!(
        scope == "src" || world != DIRTY_WORLD,
        "W's scope is replaced"
    );
    conn.execute_batch(&world)?;
    Ok((store, conn))
}

fn dirty_context() -> crate::decision::Context {
    crate::decision::Context {
        required_capabilities: ["developer"]
            .map(|role| (role.to_owned(), Default::default()))
            .into(),
        now: Some(std::time::SystemTime::now()),
        ..crate::decision::Context::default()
    }
}

/// A direct creation of an attempt `id` of `task`.
async fn create_direct(
    store: &crate::persistence::Store,
    id: &str,
    task: &str,
) -> Result<(), crate::persistence::PersistError> {
    store
        .create_attempt(crate::persistence::NewAttempt {
            attempt_id: id.to_owned(),
            task_id: task.to_owned(),
            session_id: None,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 1,
            agent: Some("claude".to_owned()),
        })
        .await
}

fn attempts_of(conn: &rusqlite::Connection, task: &str) -> Result<i64, rusqlite::Error> {
    conn.query_row(
        "SELECT count(*) FROM attempts WHERE task_id = ?1",
        [task],
        |r| r.get(0),
    )
}

/// M3N-001 (docs/adr/ADR-039.md, round 1 corrections): a write task whose scope names the dirty file in another
/// spelling (`./src`, `.` the project root, `src//lib.rs`) is held on every dispatch path, as `src` is.
#[tokio::test]
async fn test_028_03_every_dispatch_path_holds_a_scope_in_any_spelling() -> R {
    use crate::persistence::{Decided, PersistError};
    for scope in ["./src", ".", "src//lib.rs", "src/./lib.rs/"] {
        let dir = tempfile::tempdir()?;
        let (store, conn) = dirty_store(dir.path(), scope)?;
        for by in [super::By::Round, super::By::Commander, super::By::Direct] {
            let verdict = super::admit_one(&conn, "R", "W", by)?;
            assert!(
                matches!(verdict, Err(super::Refusal::DirtyWorkspace { ref paths }) if paths == &["src/lib.rs"]),
                "{scope} {by:?}: {verdict:?}"
            );
        }
        let decided = store
            .decide(dirty_dispatch(&conn, "d-W", "W")?, dirty_context())
            .await?;
        assert!(
            matches!(decided, Decided::Rejected { .. }),
            "{scope}: the Commander's dispatch {decided:?}"
        );
        let created = create_direct(&store, "A-direct", "W").await;
        assert!(
            matches!(created, Err(PersistError::IllegalTransition { .. })),
            "{scope}: a direct creation {created:?}"
        );
        store.schedule_round("R", dirty_context()).await?;
        let state: String = conn.query_row(
            "SELECT state || '/' || coalesce(blocked_reason, '') FROM tasks WHERE task_id = 'W'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(state, "BLOCKED/NEED_DECISION", "{scope}");
        assert_eq!(attempts_of(&conn, "W")?, 0, "{scope}");
    }
    Ok(())
}

/// The user's AnswerQuestion of run R's question `question` with choice `choice`.
async fn answer_question(
    store: &crate::persistence::Store,
    conn: &rusqlite::Connection,
    id: &str,
    question: &str,
    choice: &str,
) -> R {
    use crate::persistence::writer::{Intent, NewEvent};
    use crate::persistence::{Input, RunTarget, SubmitCommand};
    let revision: i64 =
        conn.query_row("SELECT revision FROM runs WHERE run_id = 'R'", [], |r| {
            r.get(0)
        })?;
    store
        .submit_command(SubmitCommand {
            command_id: id.to_owned(),
            idempotency_key: format!("k-{id}"),
            kind: "AnswerQuestion".to_owned(),
            run_id: "R".to_owned(),
            expected_revision: revision,
            payload: serde_json::json!({"question_id": question, "choice_id": choice}),
            payload_sha256: crate::context::sha256_hex(id.as_bytes()),
            target: RunTarget::Input(Input::Answer {
                question_id: question.to_owned(),
                answer: choice.to_owned(),
                choice_id: Some(choice.to_owned()),
            }),
            event: NewEvent {
                intent: Intent::Progress,
                project_id: "p1".to_owned(),
                event_id: format!("e-{id}"),
                run_id: Some("R".to_owned()),
                kind: "question.answer_accepted".to_owned(),
                payload: serde_json::json!({"command_id": id, "question_id": question}),
            },
            outbox: None,
        })
        .await?;
    Ok(())
}

/// M3N-002, M3N-052 (docs/adr/ADR-039.md, round 1 corrections): asking is no consent. After the round asked about
/// W's dirty files and before the user answers, every attempt-creating path refuses W and writes nothing (a direct
/// creation included, W being BLOCKED); only the user's answer `continue` releases the hold.
#[tokio::test]
async fn test_028_03_asking_is_no_consent_until_the_user_answers() -> R {
    use crate::persistence::PersistError;
    let dir = tempfile::tempdir()?;
    let (store, conn) = dirty_store(dir.path(), "src")?;
    store.schedule_round("R", dirty_context()).await?;
    let question: String = conn.query_row(
        "SELECT event_id FROM events WHERE kind = 'need_decision' AND json_extract(payload_json, '$.task_id') = 'W'",
        [],
        |r| r.get(0),
    )?;
    // Asked, not answered: refused on every path, nothing written by a direct creation.
    for by in [super::By::Round, super::By::Commander, super::By::Direct] {
        let verdict = super::admit_one(&conn, "R", "W", by)?;
        assert!(
            matches!(verdict, Err(super::Refusal::DirtyWorkspace { .. })),
            "{by:?} before the answer: {verdict:?}"
        );
    }
    let seq: i64 = conn.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
    let created = create_direct(&store, "A-early", "W").await;
    assert!(
        matches!(created, Err(PersistError::IllegalTransition { .. })),
        "a direct creation before the answer: {created:?}"
    );
    assert_eq!(attempts_of(&conn, "W")?, 0);
    assert_eq!(
        conn.query_row("SELECT max(seq) FROM events", [], |r| r.get::<_, i64>(0))?,
        seq,
        "a refused direct creation writes nothing"
    );
    // The round asks once: a second round neither asks again nor dispatches W.
    store.schedule_round("R", dirty_context()).await?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM events WHERE kind = 'need_decision' \
             AND json_extract(payload_json, '$.task_id') = 'W'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    assert_eq!(attempts_of(&conn, "W")?, 0);
    // The answer continue: W READY, the hold released for every path.
    answer_question(&store, &conn, "c-continue", &question, "continue").await?;
    let state: String = conn.query_row("SELECT state FROM tasks WHERE task_id = 'W'", [], |r| {
        r.get(0)
    })?;
    assert_eq!(state, "READY");
    for by in [super::By::Round, super::By::Commander, super::By::Direct] {
        assert!(
            super::admit_one(&conn, "R", "W", by)?.is_ok(),
            "{by:?} after the answer"
        );
    }
    create_direct(&store, "A-after", "W").await?;
    assert_eq!(attempts_of(&conn, "W")?, 1);
    Ok(())
}

/// M3N-052: a direct creation of an attempt of a BLOCKED task is refused and writes nothing, whatever blocked it.
#[tokio::test]
async fn a_direct_creation_of_a_blocked_task_is_refused() -> R {
    use crate::persistence::PersistError;
    let dir = tempfile::tempdir()?;
    let (store, conn) = dirty_store(dir.path(), "docs/none")?;
    conn.execute_batch(
        "UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' WHERE task_id = 'W2'",
    )?;
    let created = create_direct(&store, "A-blocked", "W2").await;
    assert!(
        matches!(created, Err(PersistError::IllegalTransition { .. })),
        "{created:?}"
    );
    assert_eq!(attempts_of(&conn, "W2")?, 0);
    Ok(())
}

#[test]
fn test_008_04_plan_blocks_every_ordinary_writer_dispatch_path() -> R {
    let conn = rusqlite::Connection::open_in_memory()?;
    super::super::migrate(&conn, super::super::MIGRATIONS)?;
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now');
      INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
      INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES('r','p','s','RUNNING','PLAN','now');
      INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) VALUES('w','r','s','READY','developer',1,1),('q','r','s','READY','qa',0,2);")?;
    for by in [super::By::Round, super::By::Commander, super::By::Direct] {
        assert!(
            super::admit_one(&conn, "r", "w", by)?.is_err(),
            "PLAN writer admitted by {by:?}"
        );
        assert!(
            super::admit_one(&conn, "r", "q", by)?.is_ok(),
            "read-only work blocked by PLAN"
        );
    }
    Ok(())
}

#[test]
fn test_009_02_m3l103_only_own_active_draft_holds_product_writes() -> R {
    let conn = rusqlite::Connection::open_in_memory()?;
    super::super::migrate(&conn, super::super::MIGRATIONS)?;
    conn.execute_batch("INSERT INTO projects(project_id,repo_path,created_at) VALUES('p','/repo','now');
      INSERT INTO specs VALUES('s','p',printf('%064d',1),1);
      INSERT INTO runs(run_id,project_id,spec_id,state,mode,created_at) VALUES
      ('r','p','s','RUNNING','BUILD','now'),('other','p','s','RUNNING','PLAN','now');
      INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) VALUES('w','r','s','READY','developer',1,1);
      INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,kind,created_seq) VALUES('d','other','s','IMPLEMENTED','cto',1,'spec_draft',1);")?;
    for state in ["RUNNING", "INCOMPLETE", "FAILED", "COMPLETED", "CANCELLED"] {
        conn.execute("UPDATE runs SET state=?1 WHERE run_id='other'", [state])?;
        for by in [super::By::Round, super::By::Commander, super::By::Direct] {
            assert!(
                super::admit_one(&conn, "r", "w", by)?.is_ok(),
                "unrelated {state} run held {by:?}"
            );
        }
    }
    conn.execute("UPDATE tasks SET run_id='r' WHERE task_id='d'", [])?;
    assert!(matches!(
        super::admit_one(&conn, "r", "w", super::By::Round)?,
        Err(super::Refusal::Mode(
            crate::mode::WriteRefusal::PendingDraft
        ))
    ));
    conn.execute("UPDATE tasks SET state='ACCEPTED' WHERE task_id='d'", [])?;
    conn.execute("INSERT INTO change_requests(cr_id,project_id,base_spec_id,state,reason,impact_json,created_at) VALUES('cr','p','s','PROPOSED','change','{}','now')", [])?;
    assert!(
        super::admit_one(&conn, "r", "w", super::By::Round)?.is_ok(),
        "a proposed CR leaves work on the approved revision unchanged"
    );
    Ok(())
}
