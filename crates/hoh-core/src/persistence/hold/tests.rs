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
    // In the command's own transaction, its run is the command's even while stopped: an event of it in G is written,
    // and the run, moved by the command (E3, M2T13R-02), ends it with its result.
    let dir = tempfile::tempdir()?;
    let (store, path) = stopped_world(&dir, RunState::Incomplete).await?;
    drop(store);
    let mut conn = Connection::open(&path)?;
    let ctx = Ctx::quiet();
    let tx = writer::begin(&mut conn, &ctx)?;
    super::command(&tx, "R")?;
    let current = revision(&path)?;
    writer::write(
        &tx,
        &Commit {
            cas: vec![StateCas {
                id: "R".to_owned(),
                expected_revision: current,
                change: Change::Touch(writer::Entity::Run),
            }],
            events: vec![event(
                "commanded",
                Some("R"),
                "test.input",
                Intent::Progress,
            )],
            ..Commit::default()
        },
        &ctx,
    )?;
    writer::commit(tx, &ctx)?;
    let all = events(&path)?;
    assert_eq!(
        all.iter()
            .rev()
            .take(2)
            .map(|e| e.3.as_str())
            .collect::<Vec<_>>(),
        ["run.result", "test.input"]
    );
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

#[test]
fn a_failure_is_never_a_stopped_runs_held_or_run_stopped() {
    // Rule 2 (c): the recorders of failures take only a Failure, which neither form of a stopped run's refusal makes.
    for stopped in [
        PersistError::Held {
            run: "R".to_owned(),
            state: RunState::Completed,
        },
        PersistError::RunStopped {
            run: "R".to_owned(),
            state: RunState::Incomplete,
        },
    ] {
        assert!(
            matches!(
                super::Failure::try_from(stopped),
                Err(PersistError::Held { .. } | PersistError::RunStopped { .. })
            ),
            "a stopped run's refusal is no failure"
        );
    }
    let failure = super::Failure::try_from(PersistError::Storage("disk".to_owned()));
    assert_eq!(
        failure.map(|f| f.to_string()).ok().as_deref(),
        Some("storage error: disk")
    );
}

/// R's candidates: C1 superseded by C2's freeze (history), C3 VERIFYING under an open round (a live obligation).
const CANDIDATES: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 1), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'VERIFYING', '2026-11-15T00:00:00Z'),
  ('C2', 'R', printf('%040d', 2), printf('%040d', 2), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'FROZEN', '2026-11-15T00:00:00Z'),
  ('C3', 'R', printf('%040d', 3), printf('%040d', 3), printf('%064d', 1), printf('%064d', 2), printf('%064d', 3),
   '/repo', 'VERIFYING', '2026-11-15T00:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'C1/round', 'R', 'verification.round_opened', '{\"round_id\": \"rd1\", \"candidate_id\": \"C1\"}',
   '2026-11-15T00:00:00Z'),
  ('p1', 'C2/frozen', 'R', 'candidate.frozen', '{\"candidate_id\": \"C2\", \"superseded\": [\"C1\"]}',
   '2026-11-15T00:00:00Z'),
  ('p1', 'C3/round', 'R', 'verification.round_opened', '{\"round_id\": \"rd3\", \"candidate_id\": \"C3\"}',
   '2026-11-15T00:00:00Z');
";

#[tokio::test]
async fn a_stopped_runs_history_is_recovered_as_records_and_its_live_obligation_waits() -> R {
    // (5)–(7) of the test plan, as far as the two predicates go: on a stopped run, a superseded candidate's recovery is
    // history — wanted at the host's start, written as Records (project-scoped), its failure only
    // candidate.reconcile_failed — while a live obligation is neither due nor wanted, and its write would be Held.
    for state in [RunState::Completed, RunState::Incomplete] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        let (from, through) = path_to(state);
        world(&path, from)?;
        Connection::open(&path)?.execute_batch(CANDIDATES)?;
        for (n, to) in through.into_iter().enumerate() {
            enter(&store, &path, to, &format!("move-{n}")).await?;
        }
        let reader = store.reader()?;
        assert!(reader.start_recovery_wanted("C1")?, "{state:?}");
        assert!(!reader.start_recovery_wanted("C3")?, "{state:?}");
        assert!(!reader.recovery_due("C1")?);
        assert!(!reader.recovery_due("C3")?);
        let now = std::time::SystemTime::now();
        store
            .record_start_recovery("C1", Err((true, "lost".to_owned())), now)
            .await?;
        store.record_recovery("C1", Ok(1), now).await?;
        let written: Vec<(Option<String>, String, Value)> = events(&path)?
            .into_iter()
            .filter(|e| e.4["candidate_id"] == "C1" && e.3 != "verification.round_opened")
            .map(|e| (e.2, e.3, e.4["run_id"].clone()))
            .collect();
        assert_eq!(
            written,
            [
                (None, "candidate.reconcile_failed".to_owned(), json!("R")),
                (None, "candidate.reconcile_recovered".to_owned(), json!("R"))
            ],
            "{state:?}"
        );
        assert_eq!(stopped(&path)?, Some(state));
        // The live obligation's recovery, were it tried, is the run's progress: Held.
        let refused = store
            .record_recovery("C3", Err((false, "x".to_owned())), now)
            .await;
        assert!(
            matches!(refused, Err(PersistError::Held { .. })),
            "{state:?}: {refused:?}"
        );
        // It is due again once the run leaves its stop (INCOMPLETE takes a CancelRun).
        if state == RunState::Incomplete {
            store.submit_command(cancel(revision(&path)?)).await?;
            assert!(store.reader()?.recovery_due("C3")?);
        }
    }
    Ok(())
}

// M2T13R-02: whether a run is stopped is read without its history, and equals the ADR's definition read from it.

/// The ADR's own definition of a stopped run, read from R's whole history: in G, and R's latest event the run.result of
/// that state (docs/adr/ADR-031.md correction D). The reference [`super::stopped`]'s indexed reading must equal.
fn stopped_by_history(path: &Path) -> R<bool> {
    Ok(Connection::open(path)?.query_row(
        "SELECT EXISTS(SELECT 1 FROM runs r WHERE r.run_id = 'R' \
           AND r.state IN ('COMPLETED', 'FAILED', 'CANCELLED', 'INCOMPLETE') \
           AND (SELECT e.kind || '/' || coalesce(json_extract(e.payload_json, '$.state'), '') FROM events e \
                WHERE e.run_id = 'R' ORDER BY e.seq DESC LIMIT 1) = 'run.result/' || r.state)",
        [],
        |r| r.get(0),
    )?)
}

/// After `step`: the store's reading of R's stop equals the ADR's.
fn same(path: &Path, step: &str) -> R {
    assert_eq!(
        stopped(path)?.is_some(),
        stopped_by_history(path)?,
        "{step}: {:?}",
        events(path)?.iter().rev().take(3).collect::<Vec<_>>()
    );
    Ok(())
}

/// R moved by a compare-and-set that changes nothing but its revision, with no event (`Touch`).
async fn touch(store: &Store, path: &Path) -> R<Result<(), PersistError>> {
    Ok(store
        .commit(Commit {
            cas: vec![StateCas {
                id: "R".to_owned(),
                expected_revision: revision(path)?,
                change: Change::Touch(writer::Entity::Run),
            }],
            ..Commit::default()
        })
        .await
        .map(drop))
}

/// A ResumeRun of R at `revision`.
fn resume(revision: i64) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-resume-{revision}"),
        idempotency_key: format!("k-resume-{revision}"),
        kind: "ResumeRun".to_owned(),
        target: RunTarget::Resume,
        event: event(
            &format!("e-resume-{revision}"),
            Some("R"),
            "run.resumed",
            Intent::Progress,
        ),
        ..cancel(revision)
    }
}

#[tokio::test]
async fn a_runs_stop_read_by_its_current_results_id_is_the_adrs_on_every_path() -> R {
    // M2T13R-02: `stopped` reads R's latest run.result by the (run_id, kind, seq) index and its id's revision, not R's
    // whole history. It equals the ADR's definition (latest event the result of the state) because every transaction
    // that leaves R in G with its revision moved ends with R's result at that revision, and every event of a stopped R
    // is a command's, which moves it. Walked here on every path a run's events take around its stops.
    // Each state of G and the stay states, entered by a commit; then a Record, Progress, a Touch with no event.
    for state in [
        RunState::Completed,
        RunState::Failed,
        RunState::Cancelled,
        RunState::Incomplete,
        RunState::Paused,
        RunState::Blocked,
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = stopped_world(&dir, state).await?;
        same(&path, &format!("{state:?} entered"))?;
        store
            .commit(Commit {
                events: vec![event("rec", Some("R"), "runner.diagnostic", Intent::Record)],
                ..Commit::default()
            })
            .await?;
        same(&path, &format!("{state:?} a record"))?;
        let _held = store
            .commit(Commit {
                events: vec![event("prog", Some("R"), "test.progress", Intent::Progress)],
                ..Commit::default()
            })
            .await;
        same(&path, &format!("{state:?} progress"))?;
        touch(&store, &path).await??;
        same(&path, &format!("{state:?} touched"))?;
        let _held = store
            .commit(Commit {
                events: vec![event("prog2", Some("R"), "test.progress", Intent::Progress)],
                ..Commit::default()
            })
            .await;
        same(&path, &format!("{state:?} progress after the touch"))?;
    }
    // INCOMPLETE: a command that keeps R in G moves it and ends with its result; one that does not is refused.
    let dir = tempfile::tempdir()?;
    let (store, path) = stopped_world(&dir, RunState::Incomplete).await?;
    drop(store);
    let ctx = Ctx::quiet();
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
        "{refused:?}"
    );
    same(&path, "a command that does not move its stopped run")?;
    let tx = writer::begin(&mut conn, &ctx)?;
    super::command(&tx, "R")?;
    let current = revision(&path)?;
    writer::write(
        &tx,
        &Commit {
            cas: vec![StateCas {
                id: "R".to_owned(),
                expected_revision: current,
                change: Change::Touch(writer::Entity::Run),
            }],
            events: vec![event(
                "commanded",
                Some("R"),
                "test.input",
                Intent::Progress,
            )],
            ..Commit::default()
        },
        &ctx,
    )?;
    writer::commit(tx, &ctx)?;
    same(&path, "a command that moves its stopped run")?;
    assert_eq!(stopped(&path)?, Some(RunState::Incomplete));
    drop(conn);
    // INCOMPLETE left by ResumeRun and entered again; left by CancelRun.
    let (store, _) = open(&dir)?;
    store.submit_command(resume(revision(&path)?)).await?;
    same(&path, "resumed")?;
    enter(&store, &path, RunState::Incomplete, "again").await?;
    same(&path, "INCOMPLETE again")?;
    assert_eq!(stopped(&path)?, Some(RunState::Incomplete));
    store.submit_command(cancel(revision(&path)?)).await?;
    same(&path, "cancel requested")?;
    // A run SQL put into G with no result, and its progress.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, "INCOMPLETE")?;
    same(&path, "seeded")?;
    store
        .commit(Commit {
            events: vec![event("after", Some("R"), "test.progress", Intent::Progress)],
            ..Commit::default()
        })
        .await?;
    same(&path, "seeded, then progress")?;
    Ok(())
}

#[tokio::test]
async fn whether_a_run_is_stopped_is_read_without_its_history() -> R {
    // M2T13R-02: the stop's reading does the same work for a run with 1 000 events as for one with 100 000 (SQLite's
    // VM steps of its statement), stopped or not.
    let mut steps = Vec::new();
    for history in [1_000, 100_000] {
        for stopped_now in [true, false] {
            let dir = tempfile::tempdir()?;
            let (store, path) = stopped_world(&dir, RunState::Incomplete).await?;
            drop(store);
            let conn = Connection::open(&path)?;
            // R's history before its stop: `history` events of a few kinds, then its result again last.
            conn.execute_batch(&format!(
                "CREATE TEMP TABLE result AS SELECT * FROM events WHERE kind = 'run.result';
                 DELETE FROM events WHERE kind = 'run.result';
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {history})
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT 'p1', 'h' || i, 'R', 'k' || (i % 7), '{{}}', '2026-11-15T00:00:00Z' FROM n;
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, event_id, run_id, kind, payload_json, created_at FROM temp.result;"
            ))?;
            if !stopped_now {
                conn.execute_batch(
                    "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
                       ('p1', 'later', 'R', 'k1', '{}', '2026-11-15T00:00:00Z');
                     UPDATE runs SET revision = revision + 1 WHERE run_id = 'R';",
                )?;
            }
            let mut statement = conn.prepare(&format!(
                "SELECT state FROM main.runs WHERE run_id = ?1 AND {}",
                super::stopped_sql("?1")
            ))?;
            let found = statement.exists(params!["R"])?;
            assert_eq!(found, stopped_now, "{history}");
            steps.push((
                history,
                stopped_now,
                statement.get_status(rusqlite::StatementStatus::VmStep),
            ));
        }
    }
    for stopped_now in [true, false] {
        let of = |history: i64| {
            steps
                .iter()
                .find(|(h, s, _)| *h == history && *s == stopped_now)
                .map(|(.., n)| *n)
                .unwrap_or(i32::MAX)
        };
        assert!(
            of(100_000) <= of(1_000) + 10,
            "stopped {stopped_now}: {steps:?}"
        );
    }
    Ok(())
}

// M2T13R-03 (the runner's unrecovered candidates): one indexed rule for the runner's tick and the stall detector's (d).

/// The rule as it was written before (one scan of the history per failure): the reference of the indexed statement.
fn unrecovered_by_history(path: &Path) -> R<std::collections::BTreeSet<String>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(&format!(
        "SELECT DISTINCT json_extract(f.payload_json, '$.candidate_id') FROM events f \
         WHERE f.kind = 'candidate.reconcile_failed' \
         AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
           AND json_extract(r.payload_json, '$.candidate_id') = json_extract(f.payload_json, '$.candidate_id')), 0) \
         AND NOT EXISTS(SELECT 1 FROM events s WHERE s.kind = 'candidate.stale' AND {} \
           AND json_extract(s.payload_json, '$.reason') = 'evidence_unrecoverable' \
           AND json_extract(s.payload_json, '$.candidate_id') = json_extract(f.payload_json, '$.candidate_id'))",
        super::of_run(
            "s",
            "(SELECT run_id FROM candidates WHERE candidate_id = json_extract(f.payload_json, '$.candidate_id'))"
        )
    ))?;
    let rows = statement.query_map([], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `verification::live_obligation` as it was written before (the project's history read by kind): the reference.
fn live_obligation_by_history(conn: &Connection, candidate: &str) -> R<bool> {
    Ok(conn.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM candidates c WHERE c.candidate_id = ?1 \
               AND c.state IN ('VERIFYING', 'REJECTED', 'INVALID')) \
             AND EXISTS(SELECT 1 FROM events o WHERE o.kind = 'verification.round_opened' \
               AND json_extract(o.payload_json, '$.candidate_id') = ?1 AND NOT EXISTS(SELECT 1 FROM events x \
                 WHERE x.kind = 'verification.round_closed' \
                 AND json_extract(x.payload_json, '$.round_id') = json_extract(o.payload_json, '$.round_id'))) \
             AND NOT EXISTS(SELECT 1 FROM events s WHERE s.kind = 'candidate.stale' \
               AND {} AND json_extract(s.payload_json, '$.candidate_id') = ?1) \
             AND NOT EXISTS(SELECT 1 FROM events f, json_each(f.payload_json, '$.superseded') v \
               WHERE f.kind = 'candidate.frozen' AND f.run_id = (SELECT run_id FROM candidates \
               WHERE candidate_id = ?1) AND v.value = ?1)",
            super::of_run("s", "(SELECT run_id FROM candidates WHERE candidate_id = ?1)")
        ),
        params![candidate],
        |r| r.get(0),
    )?)
}

/// `Reader::recovery_not_before`'s read as it was written before: the reference.
fn last_failure_by_history(
    conn: &Connection,
    candidate: &str,
) -> R<Option<crate::persistence::Failed>> {
    let mut statement = conn.prepare(
        "SELECT json_extract(f.payload_json, '$.attempt'), json_extract(f.payload_json, '$.at_ms') FROM events f \
         WHERE f.kind = 'candidate.reconcile_failed' AND json_extract(f.payload_json, '$.candidate_id') = ?1 \
         AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
           AND json_extract(r.payload_json, '$.candidate_id') = ?1), 0) ORDER BY f.seq DESC LIMIT 1",
    )?;
    let mut rows = statement.query_map(params![candidate], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.next().transpose()?)
}

/// `closing::held_by_recovery` as it was written before (the project's history read by kind for each round): the
/// reference.
fn held_by_recovery_by_history(
    conn: &Connection,
    run: &str,
) -> R<std::collections::BTreeSet<String>> {
    let mut statement = conn.prepare(
        "SELECT t.value FROM events o, json_each(o.payload_json, '$.tasks') t \
         WHERE o.kind = ?1 AND o.run_id = ?2 AND EXISTS(SELECT 1 FROM events f \
           WHERE f.kind = 'candidate.reconcile_failed' \
           AND json_extract(f.payload_json, '$.candidate_id') = json_extract(o.payload_json, '$.candidate_id') \
           AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
             AND json_extract(r.payload_json, '$.candidate_id') = json_extract(o.payload_json, '$.candidate_id')), 0))",
    )?;
    let rows = statement.query_map(params!["verification.round_opened", run], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Rounds of C1 and C3 that name their verifier tasks (CANDIDATES' rounds name none).
const ROUNDS_WITH_TASKS: &str = "
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'C1/round-b', 'R', 'verification.round_opened',
   '{\"round_id\": \"rd1b\", \"candidate_id\": \"C1\", \"tasks\": {\"qa\": \"Q-C1\", \"reviewer\": \"V-C1\"}}', 'x'),
  ('p1', 'C3/round-b', 'R', 'verification.round_opened',
   '{\"round_id\": \"rd3b\", \"candidate_id\": \"C3\", \"tasks\": {\"qa\": \"Q-C3\", \"reviewer\": \"V-C3\"}}', 'x');
";

/// After `step`: the indexed reads of the recovery give what the rules they replace give — the unrecovered set for
/// every run and each run's own candidates' part of it, each candidate's live obligation and last failure, and each
/// run's verifier tasks held by a recovery.
fn same_recovery_reads(path: &Path, step: &str) -> R<std::collections::BTreeSet<String>> {
    let conn = Connection::open(path)?;
    let runs: Vec<String> = conn
        .prepare("SELECT run_id FROM runs")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for run in &runs {
        assert_eq!(
            super::super::closing::held_by_recovery(&conn, run)?,
            held_by_recovery_by_history(&conn, run)?,
            "{step}: verifiers of {run} held by a recovery"
        );
    }
    let candidates: Vec<String> = conn
        .prepare("SELECT candidate_id FROM candidates")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for candidate in &candidates {
        assert_eq!(
            super::super::verification::live_obligation(&conn, candidate)?,
            live_obligation_by_history(&conn, candidate)?,
            "{step}: live obligation of {candidate}"
        );
        assert_eq!(
            crate::persistence::last_failure(&conn, candidate)?,
            last_failure_by_history(&conn, candidate)?,
            "{step}: last failure of {candidate}"
        );
    }
    same_unrecovered(path, step)
}

/// After `step`: the indexed statement gives the reference's set for every run, and its own candidates' part of it for
/// each run.
fn same_unrecovered(path: &Path, step: &str) -> R<std::collections::BTreeSet<String>> {
    let reference = unrecovered_by_history(path)?;
    let conn = Connection::open(path)?;
    assert_eq!(
        crate::persistence::unrecovered_candidates(&conn, None)?,
        reference,
        "{step}"
    );
    let runs: Vec<String> = conn
        .prepare("SELECT run_id FROM runs")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for run in runs {
        let of_run: std::collections::BTreeSet<String> = conn
            .prepare("SELECT candidate_id FROM candidates WHERE run_id = ?1")?
            .query_map(params![run], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        assert_eq!(
            crate::persistence::unrecovered_candidates(&conn, Some(&run))?,
            reference.intersection(&of_run).cloned().collect(),
            "{step}: run {run}"
        );
    }
    Ok(reference)
}

/// `n` minutes after the epoch.
fn minutes(n: u64) -> std::time::SystemTime {
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000 + 60 * n)
}

#[tokio::test]
async fn the_recovery_reads_by_index_are_the_rules_they_replace() -> R {
    // M2T13R-03: the indexed reads of the recovery and the rules they replace give the same answers — the unrecovered
    // candidates for every run and for each run's own, each candidate's live obligation and last failure — on the worlds
    // the tests of recovery use and on a long history.
    // Recoveries of a working run's candidates: failures, a recovery, failures again, judged unrecoverable; a
    // permanent failure; a superseded candidate's failure.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&path, "RUNNING")?;
    Connection::open(&path)?.execute_batch(&format!("{CANDIDATES}{ROUNDS_WITH_TASKS}"))?;
    same_recovery_reads(&path, "none")?;
    let transient = || Err((false, "x".to_owned()));
    let steps: Vec<(&str, crate::persistence::RecoveryOutcome, u64)> = vec![
        ("C1", transient(), 0),
        ("C3", transient(), 1),
        ("C1", Ok(1), 2),
        ("C1", transient(), 3),
        ("C2", transient(), 4),
        ("C1", transient(), 8),
        ("C1", transient(), 20),
        ("C3", Err((true, "damaged".to_owned())), 21),
        ("C2", Ok(0), 22),
    ];
    let mut seen = 0;
    for (n, (candidate, outcome, at)) in steps.into_iter().enumerate() {
        store
            .record_recovery(candidate, outcome, minutes(at))
            .await?;
        seen = seen.max(same_recovery_reads(&path, &format!("step {n} {candidate}"))?.len());
    }
    assert!(seen > 0, "a candidate was unrecovered on the way");
    // A stopped run's recoveries, written as Records project-scoped (run_id null, the run in the payload).
    for state in [RunState::Completed, RunState::Incomplete] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        let (from, through) = path_to(state);
        world(&path, from)?;
        Connection::open(&path)?.execute_batch(&format!("{CANDIDATES}{ROUNDS_WITH_TASKS}"))?;
        for (n, to) in through.into_iter().enumerate() {
            enter(&store, &path, to, &format!("move-{n}")).await?;
        }
        store
            .record_start_recovery("C1", Err((false, "lost".to_owned())), minutes(0))
            .await?;
        assert_eq!(
            same_recovery_reads(&path, &format!("{state:?} failed"))?.len(),
            1
        );
        store.record_recovery("C1", Ok(1), minutes(1)).await?;
        assert_eq!(
            same_recovery_reads(&path, &format!("{state:?} recovered"))?.len(),
            0
        );
    }
    // The stall detector's worlds (stall/tests.rs (10)): one judged unrecoverable, one backing off.
    let dir = tempfile::tempdir()?;
    let (_store, path) = open(&dir)?;
    world(&path, "RUNNING")?;
    Connection::open(&path)?.execute_batch(&format!(
        "{CANDIDATES}{ROUNDS_WITH_TASKS}
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'C1/failed', 'R', 'candidate.reconcile_failed', '{{\"candidate_id\": \"C1\"}}', 'x'),
           ('p1', 'C1/stale', 'R', 'candidate.stale', '{{\"candidate_id\": \"C1\", \"reason\": \"evidence_unrecoverable\"}}', 'x'),
           ('p1', 'C3/failed', 'R', 'candidate.reconcile_failed',
            '{{\"candidate_id\": \"C3\", \"recovery_not_before\": \"2100-01-01T00:00:00Z\"}}', 'x'),
           ('p1', 'C2/stale', 'R', 'candidate.stale', '{{\"candidate_id\": \"C2\", \"reason\": \"superseded\"}}', 'x'),
           ('p1', 'C2/failed', 'R', 'candidate.reconcile_failed', '{{\"candidate_id\": \"C2\"}}', 'x'),
           ('p1', 'C3/closed', 'R', 'verification.round_closed', '{{\"round_id\": \"rd3\"}}', 'x');"
    ))?;
    assert_eq!(same_recovery_reads(&path, "the detector's world")?.len(), 2);
    // A long history: two runs, 3 000 recovery facts of either run or project-scoped, among 20 000 other events.
    let dir = tempfile::tempdir()?;
    let (_store, path) = open(&dir)?;
    world(&path, "RUNNING")?;
    Connection::open(&path)?.execute_batch(&format!(
        "{CANDIDATES}{ROUNDS_WITH_TASKS}
         INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
           ('Q', 'p1', 'RUNNING', 'AUTO', 1, '2026-11-15T00:00:00Z');
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40)
         INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at)
           SELECT 'Q' || i, 'Q', printf('%040d', i), printf('%040d', i), printf('%064d', 1), printf('%064d', 2),
             printf('%064d', 3), '/repo',
             CASE i % 5 WHEN 0 THEN 'REJECTED' WHEN 1 THEN 'INVALID' WHEN 2 THEN 'FROZEN' ELSE 'VERIFYING' END,
             'x' FROM n;
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 23000)
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
           SELECT 'p1', 'e' || i,
             CASE WHEN i % 23 < 3 AND (i * 7) % 11 = 0 THEN NULL WHEN i % 2 = 0 THEN 'Q' ELSE 'R' END,
             CASE i % 23 WHEN 0 THEN 'candidate.reconcile_failed' WHEN 1 THEN 'candidate.reconcile_recovered'
               WHEN 2 THEN 'candidate.stale' WHEN 3 THEN 'verification.round_opened'
               WHEN 4 THEN 'verification.round_closed' WHEN 5 THEN 'candidate.frozen' ELSE 'k' || (i % 5) END,
             json_object('candidate_id', CASE WHEN i % 23 = 2 AND i % 2 = 0 THEN 'Q' || ((i * 13) % 10 + 1)
                 WHEN i % 23 = 2 THEN 'C1' WHEN i % 2 = 0 THEN 'Q' || ((i * 13) % 40 + 1)
                 ELSE 'C' || ((i * 13) % 3 + 1) END,
               'run_id', CASE WHEN i % 2 = 0 THEN 'Q' ELSE 'R' END,
               'round_id', CASE WHEN i % 23 = 4 THEN 'rd' || ((i * 7) % 105) ELSE 'rd' || (i % 211) END,
               'tasks', json_object('qa', 'q' || i, 'reviewer', 'v' || i),
               'superseded', json_array(CASE WHEN i % 2 = 0 THEN 'Q' || ((i * 17) % 10 + 11) ELSE 'C2' END),
               'reason', CASE WHEN i % 2 = 0 AND (i * 5) % 211 = 2 THEN 'evidence_unrecoverable' ELSE 'superseded' END),
             'x' FROM n;"
    ))?;
    let found = same_recovery_reads(&path, "a long history")?;
    let judged: i64 = Connection::open(&path)?.query_row(
        "SELECT count(DISTINCT json_extract(payload_json, '$.candidate_id')) FROM events \
         WHERE kind = 'candidate.stale' AND json_extract(payload_json, '$.reason') = 'evidence_unrecoverable'",
        [],
        |r| r.get(0),
    )?;
    assert!(
        found.iter().any(|c| c.starts_with('Q'))
            && found.iter().any(|c| c.starts_with('C'))
            && judged > 0,
        "the long history has unrecovered candidates of both runs, and judged ones: {found:?}, {judged}"
    );
    // And candidates with a live obligation and without, with a last failure and without.
    let conn = Connection::open(&path)?;
    let (mut live, mut failing) = (std::collections::BTreeSet::new(), 0);
    let all: Vec<String> = conn
        .prepare("SELECT candidate_id FROM candidates")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for candidate in &all {
        live.insert(super::super::verification::live_obligation(
            &conn, candidate,
        )?);
        failing += usize::from(crate::persistence::last_failure(&conn, candidate)?.is_some());
    }
    assert!(
        live.len() == 2 && failing > 0 && failing < all.len(),
        "{live:?} {failing} of {}",
        all.len()
    );
    // And verifiers held by a recovery in both runs, not all of them.
    for run in ["R", "Q"] {
        let held = super::super::closing::held_by_recovery(&conn, run)?;
        let named: i64 = conn.query_row(
            "SELECT count(DISTINCT t.value) FROM events o, json_each(o.payload_json, '$.tasks') t \
             WHERE o.kind = 'verification.round_opened' AND o.run_id = ?1",
            params![run],
            |r| r.get(0),
        )?;
        assert!(
            !held.is_empty() && i64::try_from(held.len())? < named,
            "{run}: {} of {named}",
            held.len()
        );
    }
    Ok(())
}

#[test]
fn the_unrecovered_candidates_are_read_without_the_history() -> R {
    // M2T13R-03: the statement's work (SQLite's VM steps) is the same with 1 000 and 100 000 other events in the
    // history, for every run and for one run, with the same 30 recovery facts; the rule it replaces grows with them.
    let mut steps: Vec<(usize, bool, i32)> = Vec::new();
    for history in [1_000, 100_000] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        drop(store);
        world(&path, "RUNNING")?;
        let conn = Connection::open(&path)?;
        conn.execute_batch(&format!(
            "{CANDIDATES}
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {history})
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'h' || i, 'R', 'k' || (i % 7), '{{}}', 'x' FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30)
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'f' || i, 'R', CASE i % 3 WHEN 0 THEN 'candidate.reconcile_recovered'
                 ELSE 'candidate.reconcile_failed' END, json_object('candidate_id', 'C' || (i % 3 + 1)), 'x' FROM n;"
        ))?;
        for by_run in [false, true] {
            let mut statement = conn.prepare(&crate::persistence::unrecovered_sql(by_run))?;
            let found: Vec<String> = if by_run {
                statement
                    .query_map(params!["R"], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            } else {
                statement
                    .query_map([], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            assert!(!found.is_empty(), "{history} {by_run}");
            steps.push((
                history,
                by_run,
                statement.get_status(rusqlite::StatementStatus::VmStep),
            ));
        }
    }
    for by_run in [false, true] {
        let of = |history: usize| {
            steps
                .iter()
                .find(|(h, b, _)| *h == history && *b == by_run)
                .map_or(i32::MAX, |(.., n)| *n)
        };
        assert!(of(100_000) <= of(1_000) + 50, "by run {by_run}: {steps:?}");
    }
    Ok(())
}

#[test]
fn a_candidates_live_obligation_and_last_failure_are_read_without_the_history() -> R {
    // M2T13R-03: what the runner's tick reads for each candidate it may recover — `recovery_due`'s live obligation and
    // `recovery_not_before`'s last failure — does the same work (SQLite's VM steps) with 1 000 and 100 000 other events
    // in the history; the reads they replace grow with them.
    let mut steps: Vec<(usize, &str, i32)> = Vec::new();
    for history in [1_000, 100_000] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        drop(store);
        world(&path, "RUNNING")?;
        let conn = Connection::open(&path)?;
        conn.execute_batch(&format!(
            "{CANDIDATES}
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {history})
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'h' || i, 'R', 'k' || (i % 7), '{{}}', 'x' FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30)
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'f' || i, 'R', CASE i % 3 WHEN 0 THEN 'candidate.reconcile_recovered'
                 ELSE 'candidate.reconcile_failed' END,
                 json_object('candidate_id', 'C' || (i % 3 + 1), 'attempt', i, 'at_ms', '0'), 'x' FROM n;"
        ))?;
        for (name, sql) in [
            (
                "live obligation",
                super::super::verification::live_obligation_sql(),
            ),
            ("last failure", crate::persistence::last_failure_sql()),
        ] {
            let mut statement = conn.prepare(&sql)?;
            let found = statement
                .query_map(params!["C3"], |r| r.get_ref(0).map(drop))?
                .count();
            assert_eq!(found, 1, "{history} {name}");
            steps.push((
                history,
                name,
                statement.get_status(rusqlite::StatementStatus::VmStep),
            ));
        }
        assert!(super::super::verification::live_obligation(&conn, "C3")?);
    }
    for name in ["live obligation", "last failure"] {
        let of = |history: usize| {
            steps
                .iter()
                .find(|(h, n, _)| *h == history && *n == name)
                .map_or(i32::MAX, |(.., s)| *s)
        };
        assert!(of(100_000) <= of(1_000) + 50, "{name}: {steps:?}");
    }
    Ok(())
}

#[test]
fn a_runs_verifiers_held_by_a_recovery_are_read_without_the_history() -> R {
    // M2T13R-03: `closing::held_by_recovery`, which each scheduling round of a working run reads, does the same work
    // (SQLite's VM steps) with 1 000 and 100 000 other events in the history; the read it replaces grows with them.
    let mut steps = Vec::new();
    for history in [1_000, 100_000] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        drop(store);
        world(&path, "RUNNING")?;
        let conn = Connection::open(&path)?;
        conn.execute_batch(&format!(
            "{CANDIDATES}{ROUNDS_WITH_TASKS}
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {history})
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'h' || i, 'R', 'k' || (i % 7), '{{}}', 'x' FROM n;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 30)
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
               SELECT 'p1', 'f' || i, 'R', CASE i % 3 WHEN 0 THEN 'candidate.reconcile_recovered'
                 ELSE 'candidate.reconcile_failed' END, json_object('candidate_id', 'C' || (i % 3 + 1)), 'x' FROM n;"
        ))?;
        let mut statement = conn.prepare(&super::super::closing::held_by_recovery_sql())?;
        let held: std::collections::BTreeSet<String> = statement
            .query_map(params!["R"], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        assert_eq!(
            held,
            ["Q-C3", "V-C3"].map(str::to_owned).into_iter().collect(),
            "{history}"
        );
        steps.push((
            history,
            statement.get_status(rusqlite::StatementStatus::VmStep),
        ));
    }
    assert!(steps[1].1 <= steps[0].1 + 50, "{steps:?}");
    Ok(())
}
