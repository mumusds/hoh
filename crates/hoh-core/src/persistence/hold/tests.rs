//! docs/adr/ADR-031.md correction D against a real store: the only events insert and its write intents (test plan
//! "停住之后的写入口" (1)), Held in the writer's answer (2), the backstop trigger and the scans that keep every events
//! insert in the insert function and every by-run read of a Record kind on `hold::of_run`.

use crate::persistence::writer::{self, Ctx};
use crate::persistence::{
    Change, Commit, Intent, NewEvent, NewOutbox, Options, PersistError, RunTarget, StateCas, Store,
    SubmitCommand, selftest,
};
use crate::protocol::RunState;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R of project p1 in `state`, revision 1, with no event yet.
fn world(path: &Path, state: &str) -> R {
    Connection::open(path)?.execute_batch(&format!(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-15T00:00:00Z');
         INSERT INTO runs(run_id, project_id, state, mode, revision, created_at, blocked_reason) VALUES
           ('R', 'p1', '{state}', 'AUTO', 1, '2026-11-15T00:00:00Z',
            CASE WHEN '{state}' = 'BLOCKED' THEN 'NEED_DECISION' END);"
    ))?;
    Ok(())
}

fn event(id: &str, run: Option<&str>, kind: &str, intent: Intent) -> NewEvent {
    NewEvent {
        project_id: "p1".to_owned(),
        event_id: id.to_owned(),
        run_id: run.map(str::to_owned),
        kind: kind.to_owned(),
        payload: json!({"n": id}),
        intent,
    }
}

/// R's revision now.
fn revision(path: &Path) -> R<i64> {
    Ok(Connection::open(path)?.query_row(
        "SELECT revision FROM runs WHERE run_id = 'R'",
        [],
        |r| r.get(0),
    )?)
}

/// R moved to `to` by a commit with one event of its own: its stop, published by its run.result.
async fn enter(store: &Store, path: &Path, to: RunState, id: &str) -> R {
    store
        .commit(Commit {
            cas: vec![StateCas {
                id: "R".to_owned(),
                expected_revision: revision(path)?,
                change: Change::Run(to),
            }],
            events: vec![event(id, Some("R"), "test.moved", Intent::Progress)],
            ..Commit::default()
        })
        .await?;
    Ok(())
}

/// An event as stored: (seq, event_id, run_id, kind, payload).
type Row = (i64, String, Option<String>, String, Value);

/// Every event.
fn events(path: &Path) -> R<Vec<Row>> {
    let conn = Connection::open(path)?;
    let mut statement =
        conn.prepare("SELECT seq, event_id, run_id, kind, payload_json FROM events ORDER BY seq")?;
    let rows = statement.query_map([], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or(Value::Null),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// R's latest event's kind and payload state.
fn latest(path: &Path) -> R<(String, Option<String>)> {
    Ok(Connection::open(path)?.query_row(
        "SELECT kind, json_extract(payload_json, '$.state') FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

fn stopped(path: &Path) -> R<Option<RunState>> {
    Ok(super::stopped(&Connection::open(path)?, "R")?)
}

/// The legal way into each state of G (and the stay states): from where, through where.
fn path_to(state: RunState) -> (&'static str, Vec<RunState>) {
    match state {
        RunState::Cancelled => ("RUNNING", vec![RunState::CancelRequested, state]),
        RunState::Paused => ("RUNNING", vec![RunState::PauseRequested, state]),
        _ => ("VERIFYING", vec![state]),
    }
}

/// R stopped in `state` (published), in a fresh store.
async fn stopped_world(dir: &tempfile::TempDir, state: RunState) -> R<(Store, PathBuf)> {
    let (store, path) = open(dir)?;
    let (from, through) = path_to(state);
    world(&path, from)?;
    if state == RunState::Blocked {
        // A BLOCKED run has its reason recorded before it moves (blockers.rs).
        Connection::open(&path)?
            .execute_batch("UPDATE runs SET blocked_reason = 'NEED_DECISION' WHERE run_id = 'R'")?;
    }
    for (n, to) in through.into_iter().enumerate() {
        enter(&store, &path, to, &format!("move-{n}")).await?;
    }
    use crate::lifecycle::Lifecycle as _;
    assert_eq!(
        latest(&path)?,
        ("run.result".to_owned(), Some(state.as_str().to_owned()))
    );
    Ok((store, path))
}

const G: [RunState; 4] = [
    RunState::Completed,
    RunState::Failed,
    RunState::Cancelled,
    RunState::Incomplete,
];

#[tokio::test]
async fn a_stopped_runs_progress_is_held_with_nothing_of_its_transaction_written() -> R {
    // (1) and (2): Progress of a run stopped in G — an event and an outbox row in one commit — is refused by the insert
    // function; the transaction rolls back and the caller gets Held, the run and its state named.
    for state in G {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        assert_eq!(stopped(&path)?, Some(state));
        let before = events(&path)?;
        let refused = store
            .commit(Commit {
                events: vec![event(
                    "progress",
                    Some("R"),
                    "test.progress",
                    Intent::Progress,
                )],
                outbox: vec![NewOutbox {
                    message_id: "m1".to_owned(),
                    event: 0,
                    run_id: Some("R".to_owned()),
                    recipient_role: "commander".to_owned(),
                    role_epoch: 1,
                    payload: json!({}),
                }],
                ..Commit::default()
            })
            .await;
        match refused {
            Err(PersistError::Held { run, state: held }) => {
                assert_eq!((run.as_str(), held), ("R", state));
            }
            other => panic!("{state:?}: {other:?}"),
        }
        assert_eq!(events(&path)?, before, "{state:?}");
        let outbox: i64 =
            Connection::open(&path)?.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))?;
        assert_eq!(outbox, 0, "{state:?}");
        // The project's events are not the run's: written as they are.
        store
            .commit(Commit {
                events: vec![event("project", None, "test.project", Intent::Progress)],
                ..Commit::default()
            })
            .await?;
        assert_eq!(stopped(&path)?, Some(state));
    }
    Ok(())
}

#[tokio::test]
async fn a_stopped_runs_records_are_written_project_scoped_and_it_stays_stopped() -> R {
    // (1): every kind of RECORD_KINDS as a Record of a stopped run: run_id null, the run in the payload, the id kept;
    // the run's latest event is still its run.result. A Record of another kind, or naming another run, is refused.
    for state in G {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        for kind in super::RECORD_KINDS {
            let id = format!("record/{kind}");
            store
                .commit(Commit {
                    events: vec![event(&id, Some("R"), kind, Intent::Record)],
                    ..Commit::default()
                })
                .await?;
            let written = events(&path)?
                .into_iter()
                .find(|(_, event_id, ..)| *event_id == id)
                .ok_or("not written")?;
            assert_eq!(written.2, None, "{kind}");
            assert_eq!(written.3, kind);
            assert_eq!(written.4, json!({"n": id, "run_id": "R"}), "{kind}");
            assert_eq!(stopped(&path)?, Some(state), "{kind}");
        }
        let before = events(&path)?;
        let other_kind = store
            .commit(Commit {
                events: vec![event("x", Some("R"), "test.progress", Intent::Record)],
                ..Commit::default()
            })
            .await;
        assert!(
            matches!(other_kind, Err(PersistError::IntegrityViolation(_))),
            "{other_kind:?}"
        );
        let mut naming = event("y", Some("R"), "runner.diagnostic", Intent::Record);
        naming.payload = json!({"run_id": "OTHER"});
        let other_run = store
            .commit(Commit {
                events: vec![naming],
                ..Commit::default()
            })
            .await;
        assert!(
            matches!(other_run, Err(PersistError::IntegrityViolation(_))),
            "{other_run:?}"
        );
        assert_eq!(events(&path)?, before);
    }
    Ok(())
}

#[tokio::test]
async fn records_of_a_run_not_stopped_stay_its_own() -> R {
    // A Record is a run's event like any other while the run has not stopped.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, "RUNNING")?;
    store
        .commit(Commit {
            events: vec![event("d", Some("R"), "runner.diagnostic", Intent::Record)],
            ..Commit::default()
        })
        .await?;
    let written = events(&path)?;
    assert_eq!(written[0].2.as_deref(), Some("R"));
    assert_eq!(written[0].4, json!({"n": "d"}));
    Ok(())
}

/// A CancelRun of R at `revision`.
fn cancel(revision: i64) -> SubmitCommand {
    SubmitCommand {
        command_id: "c-cancel".to_owned(),
        idempotency_key: "k-cancel".to_owned(),
        kind: "CancelRun".to_owned(),
        run_id: "R".to_owned(),
        expected_revision: revision,
        payload: json!({}),
        payload_sha256: format!("{:064}", 9),
        target: RunTarget::To(RunState::CancelRequested),
        event: event(
            "e-cancel",
            Some("R"),
            "run.cancel_requested",
            Intent::Progress,
        ),
        outbox: None,
    }
}

#[tokio::test]
async fn a_command_of_a_stopped_incomplete_run_is_written_and_none_is_accepted_for_an_ended_one()
-> R {
    // (1): a command accepted for a stopped INCOMPLETE run writes the run's events as they are (CancelRun leaves G by
    // its compare-and-set; a command that keeps the run in G is the command's too, `temp.command_run`).
    let dir = tempfile::tempdir()?;
    let (store, path) = stopped_world(&dir, RunState::Incomplete).await?;
    store.submit_command(cancel(revision(&path)?)).await?;
    let last = events(&path)?.pop().ok_or("no event")?;
    assert_eq!(
        (last.1.as_str(), last.2.as_deref()),
        ("e-cancel", Some("R"))
    );
    // In the command's own transaction, its run is the command's even while stopped: an event of it in G is written.
    let dir = tempfile::tempdir()?;
    let (store, path) = stopped_world(&dir, RunState::Incomplete).await?;
    drop(store);
    let mut conn = Connection::open(&path)?;
    let ctx = Ctx::quiet();
    let tx = writer::begin(&mut conn, &ctx)?;
    super::command(&tx, "R")?;
    writer::insert_event(
        &tx,
        &event("commanded", Some("R"), "test.input", Intent::Progress),
    )?;
    writer::commit(tx, &ctx)?;
    assert_eq!(latest(&path)?.0, "test.input");
    // A command's run that has ended is an IntegrityViolation at the commit, and nothing of it is written.
    for state in [RunState::Completed, RunState::Failed, RunState::Cancelled] {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        drop(store);
        let before = events(&path)?;
        let mut conn = Connection::open(&path)?;
        let tx = writer::begin(&mut conn, &ctx)?;
        super::command(&tx, "R")?;
        writer::insert_event(
            &tx,
            &event("commanded", Some("R"), "test.input", Intent::Progress),
        )?;
        let refused = writer::commit(tx, &ctx);
        assert!(
            matches!(refused, Err(PersistError::IntegrityViolation(_))),
            "{state:?}: {refused:?}"
        );
        assert_eq!(events(&path)?, before);
    }
    Ok(())
}

#[tokio::test]
async fn the_commit_that_stops_and_a_direct_move_from_a_stay_state_are_not_held() -> R {
    // (1): the commit entering G writes its own events (its compare-and-set comes first, and the run's latest event is
    // not yet its result); a stay state with its result published moves straight to FAILED with an event, its
    // result then FAILED's; after it the run is stopped and its progress Held.
    for state in [RunState::Blocked, RunState::Paused, RunState::Incomplete] {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        enter(&store, &path, RunState::Failed, "to-failed").await?;
        let all = events(&path)?;
        let n = all.len();
        assert_eq!(all[n - 2].1, "to-failed", "{state:?}");
        assert_eq!(
            (all[n - 1].3.as_str(), all[n - 1].4["state"].as_str()),
            ("run.result", Some("FAILED")),
            "{state:?}"
        );
        let refused = store
            .commit(Commit {
                events: vec![event("late", Some("R"), "test.progress", Intent::Progress)],
                ..Commit::default()
            })
            .await;
        assert!(
            matches!(refused, Err(PersistError::Held { .. })),
            "{state:?}: {refused:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn paused_blocked_and_runs_with_no_result_are_not_stopped() -> R {
    // G has no PAUSED or BLOCKED: their published results hold nothing back (the ninth draft's scope). A run that
    // stopped before run.result existed, or one SQL put into G, has no result: not stopped either.
    for state in [RunState::Paused, RunState::Blocked] {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        assert_eq!(stopped(&path)?, None);
        store
            .commit(Commit {
                events: vec![event("after", Some("R"), "test.progress", Intent::Progress)],
                ..Commit::default()
            })
            .await?;
        assert_eq!(latest(&path)?.0, "test.progress");
    }
    for state in ["COMPLETED", "FAILED", "CANCELLED", "INCOMPLETE"] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        world(&path, state)?;
        assert_eq!(stopped(&path)?, None);
        store
            .commit(Commit {
                events: vec![event("after", Some("R"), "test.progress", Intent::Progress)],
                ..Commit::default()
            })
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn raw_sql_around_the_insert_function_meets_the_backstop_trigger() -> R {
    // The writer connection's TEMP trigger refuses an event of a stopped run that is not its result nor a command's:
    // SQL that bypasses the insert function (which the scan below also forbids).
    let dir = tempfile::tempdir()?;
    let (store, path) = stopped_world(&dir, RunState::Completed).await?;
    drop(store);
    let conn = Connection::open(&path)?;
    super::install(&conn)?;
    let insert = |id: &str, kind: &str| {
        conn.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
             VALUES ('p1', ?1, 'R', ?2, '{}', '2026-11-15T00:00:00Z')",
            params![id, kind],
        )
    };
    let refused = insert("raw", "test.progress");
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.to_string().contains(super::RUN_STOPPED)),
        "{refused:?}"
    );
    // A command's run is let through.
    conn.execute("INSERT INTO temp.command_run(run_id) VALUES ('R')", [])?;
    insert("commanded", "test.input")?;
    Ok(())
}

/// The workspace's product sources (hoh-core and hoh-app), test files and inline test modules left out: (path, text).
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
                // Code only: comments that name the SQL are not SQL.
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

/// The body of function `name` in `text` (from its `fn` to the matching close brace).
fn body<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let start = text.find(&format!("fn {name}("))?;
    let open = start + text[start..].find('{')?;
    let mut depth = 0;
    for (i, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

#[test]
fn every_events_insert_is_the_insert_function() -> R {
    // (1), scan: the SQL `INSERT INTO events` appears in the product only in writer::insert's body, the one
    // insert_event calls (the six direct inserts of before go through it).
    let mut found = Vec::new();
    for (path, text) in product_sources()? {
        let allowed = (path.ends_with("persistence/writer.rs"))
            .then(|| body(&text, "insert"))
            .flatten();
        let outside = match allowed {
            Some(inside) => text.replacen(inside, "", 1),
            None => text.clone(),
        };
        let n = outside.matches("INSERT INTO events").count();
        if n > 0 {
            found.push(format!("{}: {n}", path.display()));
        }
    }
    assert_eq!(found, Vec::<String>::new());
    // The scan sees one where it is.
    let writer = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src/persistence/writer.rs"),
    )?;
    assert_eq!(
        body(&writer, "insert").map(|b| b.matches("INSERT INTO events").count()),
        Some(1)
    );
    Ok(())
}

/// The Rust string literals of `text` (with their line continuations), roughly: enough for SQL.
fn literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    let mut in_comment = false;
    let mut prev = ' ';
    while let Some(c) = chars.next() {
        if in_comment {
            if c == '\n' {
                in_comment = false;
            }
            prev = c;
            continue;
        }
        if c == '/' && chars.peek() == Some(&'/') {
            in_comment = true;
            continue;
        }
        if c == '\'' && prev != '\\' {
            // A char literal: '"' is not a string.
            let ahead: String = chars.clone().take(3).collect();
            if ahead.starts_with("\"'") {
                chars.next();
                chars.next();
                prev = '\'';
                continue;
            }
        }
        if c == '"' {
            let mut s = String::new();
            let mut escaped = false;
            for d in chars.by_ref() {
                if escaped {
                    escaped = false;
                    s.push(d);
                    continue;
                }
                match d {
                    '\\' => escaped = true,
                    '"' => break,
                    _ => s.push(d),
                }
            }
            out.push(s);
            prev = '"';
            continue;
        }
        prev = c;
    }
    out
}

/// The SQL literals of `text` that read a Record kind by run without `hold::of_run`: a `<alias>.kind = '<kind>'` (or
/// `kind = '<kind>'`) of a kind in RECORD_KINDS whose alias is matched by `run_id =` in the same literal.
fn record_reads_by_run(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for literal in literals(text) {
        for kind in super::RECORD_KINDS {
            let quoted = format!("kind = '{kind}'");
            let mut rest = literal.as_str();
            while let Some(at) = rest.find(&quoted) {
                let before = &rest[..at];
                let alias: String = before
                    .chars()
                    .rev()
                    .skip_while(|c| *c == '.')
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect();
                let dotted = before.ends_with('.');
                let needle = if dotted {
                    format!("{alias}.run_id =")
                } else {
                    "run_id =".to_owned()
                };
                let by_run = if dotted {
                    literal.contains(&needle)
                } else {
                    // Unqualified: a `run_id =` not preceded by an alias's dot.
                    literal
                        .match_indices(&needle)
                        .any(|(i, _)| !literal[..i].ends_with('.'))
                };
                if by_run {
                    found.push(format!("{kind}: {}", literal.trim()));
                }
                rest = &rest[at + quoted.len()..];
            }
        }
    }
    found
}

#[test]
fn every_read_of_a_record_kind_by_run_goes_through_of_run() -> R {
    // (1), scan: in persistence, an SQL literal naming a Record kind and selecting it by `run_id =` reads only the
    // run's own rows and misses its project-scoped Records: such reads go through `hold::of_run`.
    let mut found = Vec::new();
    for (path, text) in product_sources()? {
        if !path.to_string_lossy().contains("persistence") {
            continue;
        }
        for hit in record_reads_by_run(&text) {
            found.push(format!("{}: {hit}", path.display()));
        }
    }
    assert_eq!(found, Vec::<String>::new());
    // The scan itself: a read by run, by alias, and the ones it lets be.
    assert_eq!(
        record_reads_by_run(
            r#""SELECT 1 FROM events WHERE run_id = ?1 AND kind = 'candidate.stale'""#
        )
        .len(),
        1
    );
    assert_eq!(
        record_reads_by_run(
            r#""SELECT 1 FROM events s WHERE s.run_id = ?1 AND s.kind = 'issue.resolved'""#
        )
        .len(),
        1
    );
    assert!(
        record_reads_by_run(
            r#""SELECT 1 FROM events o, events f WHERE o.run_id = ?1 AND f.kind = 'candidate.reconcile_failed'""#
        )
        .is_empty()
    );
    assert!(
        record_reads_by_run(
            r#"&format!("SELECT 1 FROM events s WHERE {} AND s.kind = 'candidate.stale'", of_run("s", "?1"))"#
        )
        .is_empty()
    );
    Ok(())
}
