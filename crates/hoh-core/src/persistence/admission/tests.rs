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
