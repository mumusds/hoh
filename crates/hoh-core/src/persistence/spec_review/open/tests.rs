//! A specification review opened, dispatched and ended against a real store (REQ-031 behaviour 3 and 4,
//! docs/adr/ADR-035.md decisions 5 to 7): each ordering of the run's states, the review's task and its intake is its
//! own deterministic step here; the host's commit look-up, checkout and report construction are tested in hoh-app.

use crate::decision::Context;
use crate::persistence::{Options, Store, selftest};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Project p1, revision 1 (s1) adopted by run R in SPECIFYING; the Commander bound in session sc, the reviewer in
/// session sr (claude, its native session N-rev), the developer in sd with planned task TP READY.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-22T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%.64c', '1'), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'SPECIFYING', 'AUTO', 1, '2026-11-22T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, native_id, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', 'N-cmd', '1', '{}'), ('sr', 'claude', 'reviewer', 'N-rev', '1', '{}'),
  ('sd', 'claude', 'developer', NULL, '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-22T00:00:00Z'),
  ('R', 'reviewer', 'claude', 'sr', 1, 1, '2026-11-22T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-22T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('TP', 'R', 's1', 'READY', 0, 'developer', 1, 1);
";

const COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";

fn world(dir: &Path) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    Ok((store, path))
}

fn context() -> Context {
    Context {
        required_capabilities: ["reviewer", "developer"]
            .map(|role| (role.to_owned(), Default::default()))
            .into(),
        now: Some(SystemTime::now()),
        ..Context::default()
    }
}

fn text(path: &Path, sql: &str) -> R<String> {
    Ok(Connection::open(path)?.query_row(sql, [], |r| r.get::<_, String>(0))?)
}

fn exec(path: &Path, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

/// The payloads of the events of `kind`, in order.
fn events(path: &Path, kind: &str) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    let mut statement =
        conn.prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY seq")?;
    let rows = statement
        .query_map([kind], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows
        .iter()
        .map(|t| serde_json::from_str(t))
        .collect::<Result<_, _>>()?)
}

/// A review is due once per revision: opened with its reader task, not again while it is in progress, again (a new
/// review id) once its task ended without a CURRENT report, never once the revision has one; never in a stopped run;
/// a wait for the revision's commit is recorded once per HEAD.
#[tokio::test]
async fn a_review_is_opened_once_per_revision_until_a_current_report() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    let reader = store.reader()?;
    let due = reader.spec_review_due("R")?.ok_or("not due")?;
    assert_eq!((due.spec_id.as_str(), due.spec_revision), ("s1", 1));
    // Waiting for its commit: once per HEAD, nothing else written.
    assert!(store.spec_review_waiting("R", "s1", "h1").await?);
    assert!(!store.spec_review_waiting("R", "s1", "h1").await?);
    assert!(reader.spec_review_waited("s1", "h1")?);
    assert!(!reader.spec_review_waited("s1", "h2")?);
    assert_eq!(
        events(&path, "spec_review.waiting")?,
        [
            json!({"spec_id": "s1", "spec_revision": 1, "head": "h1", "reason": "spec_not_committed"})
        ]
    );
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let opened = events(&path, "spec_review.opened")?;
    assert_eq!(opened.len(), 1);
    let (review, task) = (
        opened[0]["review_id"].as_str().ok_or("review")?.to_owned(),
        opened[0]["task_id"].as_str().ok_or("task")?.to_owned(),
    );
    assert_eq!(
        opened[0],
        json!({"review_id": review, "run_id": "R", "spec_id": "s1", "spec_revision": 1,
               "spec_sha256": "1".repeat(64), "git_commit": COMMIT, "task_id": task})
    );
    assert_eq!(
        text(
            &path,
            &format!(
                "SELECT state || ' ' || role || ' ' || writer || ' ' || layer || ' ' || spec_id || ' ' || \
                 (SELECT count(*) FROM task_requirements WHERE task_id = '{task}') FROM tasks WHERE task_id = '{task}'"
            )
        )?,
        "READY reviewer 0 0 s1 0"
    );
    assert_eq!(
        reader.spec_review_work(&task)?,
        Some(crate::persistence::ReviewWork {
            review_id: review.clone(),
            spec_revision: 1,
            spec_sha256: "1".repeat(64),
            git_commit: COMMIT.to_owned()
        })
    );
    // In progress: not due, not opened again.
    assert_eq!(reader.spec_review_due("R")?, None);
    assert!(!store.open_spec_review("R", "s1", COMMIT).await?);
    // Its task ended with no report: due again, a new review.
    exec(
        &path,
        &format!("UPDATE tasks SET state = 'CANCELLED' WHERE task_id = '{task}'"),
    )?;
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let again = events(&path, "spec_review.opened")?;
    assert_eq!(again.len(), 2);
    assert_ne!(again[1]["review_id"], json!(review));
    // A CURRENT report of the revision: never due again.
    exec(
        &path,
        "UPDATE tasks SET state = 'ACCEPTED' WHERE role = 'reviewer';
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'rep', 'R',
           'spec_review.reported', json_object('spec_id', 's1', 'state', 'CURRENT'), '2026-11-22T00:00:00Z');",
    )?;
    assert_eq!(reader.spec_review_due("R")?, None);
    exec(
        &path,
        "DELETE FROM events WHERE event_id = 'rep'; UPDATE runs SET state = 'PAUSED', resume_state = 'SPECIFYING';",
    )?;
    assert_eq!(
        reader.spec_review_due("R")?,
        None,
        "a paused run opens none"
    );
    assert!(!store.spec_review_waiting("R", "s1", "h3").await?);
    Ok(())
}

/// The review task is the only candidate in SPECIFYING and READY (the run's state never moved by it), dispatched with
/// origin spec_review; none is dispatched on a BLOCKED run; a reviewer bound in the Commander's session (or sharing
/// its native identity) is refused once, nothing leased, until a rebinding lets it go.
#[tokio::test]
async fn a_review_task_is_dispatched_in_the_working_states_in_an_independent_session() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let task = text(&path, "SELECT task_id FROM tasks WHERE role = 'reviewer'")?;
    // BLOCKED: not dispatched (no new one is, REQ-031 behaviour 3), its run's state restored after.
    exec(
        &path,
        "UPDATE runs SET state = 'BLOCKED', resume_state = 'SPECIFYING', blocked_reason = 'NEED_DECISION', \
         blocked_reasons_json = '[\"NEED_DECISION\"]'",
    )?;
    assert_eq!(store.schedule_round("R", context()).await?.dispatched, None);
    // Not independent: the reviewer bound in a session sharing the Commander's native identity.
    exec(
        &path,
        "UPDATE runs SET state = 'SPECIFYING', resume_state = NULL, blocked_reason = NULL, blocked_reasons_json = '[]';
         INSERT INTO sessions(session_id, agent, role, native_id, agent_version, capabilities_json) VALUES
           ('sx', 'codex', 'reviewer', 'N-cmd', '1', '{}');
         UPDATE role_bindings SET session_id = 'sx' WHERE role = 'reviewer';",
    )?;
    for _ in 0..2 {
        assert_eq!(store.schedule_round("R", context()).await?.dispatched, None);
    }
    let refused = events(&path, "spec_review.dispatch_refused")?;
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(refused[0]["task_id"], json!(task));
    assert_eq!(refused[0]["reason"], "not_independent");
    assert_eq!(text(&path, "SELECT count(*) || '' FROM attempts")?, "0");
    // Rebound to an independent session: dispatched in SPECIFYING, the planned task left, the run not moved.
    exec(
        &path,
        "UPDATE role_bindings SET session_id = 'sr' WHERE role = 'reviewer';",
    )?;
    let dispatched = store
        .schedule_round("R", context())
        .await?
        .dispatched
        .ok_or("not dispatched")?;
    assert_eq!(dispatched.task_id, task);
    assert_eq!(dispatched.lease.kind, "reader-1");
    assert_eq!(
        text(
            &path,
            "SELECT json_extract(payload_json, '$.origin') FROM outbox WHERE json_extract(payload_json, '$.kind') = \
             'dispatch'"
        )?,
        "spec_review"
    );
    assert_eq!(
        store.schedule_round("R", context()).await?.dispatched,
        None,
        "the planned task waits for RUNNING"
    );
    assert_eq!(text(&path, "SELECT state FROM runs")?, "SPECIFYING");
    assert_eq!(
        text(&path, "SELECT state FROM tasks WHERE task_id = 'TP'")?,
        "READY"
    );
    Ok(())
}

/// A review task its finished attempt left IMPLEMENTED ends ACCEPTED with its report's intake, CURRENT or REJECTED
/// alike (decision 7).
#[tokio::test]
async fn an_intake_ends_its_review_task_accepted() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let (review, task) = (
        text(
            &path,
            "SELECT json_extract(payload_json, '$.review_id') FROM events WHERE kind = 'spec_review.opened'",
        )?,
        text(&path, "SELECT task_id FROM tasks WHERE role = 'reviewer'")?,
    );
    exec(
        &path,
        &format!(
            "UPDATE tasks SET state = 'IMPLEMENTED', revision = 3 WHERE task_id = '{task}';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
               VALUES ('AR', '{task}', 'sr', 'SETTLED', 1, 1, 1);"
        ),
    )?;
    let raw = b"{}\n";
    let sha256 = crate::specindex::digest(raw);
    let taken = store
        .intake_spec_review(crate::persistence::SpecReviewIntake {
            review_id: review,
            attempt_id: "AR".to_owned(),
            report: None,
            native: None,
            raw: crate::persistence::ArtifactRow {
                path: ".hoh/attempts/AR/stdout.jsonl".to_owned(),
                sha256: sha256.clone(),
                bytes: raw.len() as u64,
                mime: "application/jsonl".to_owned(),
                kind: "raw_log".to_owned(),
                name: "stdout.jsonl".to_owned(),
            },
            raw_sha256: sha256,
        })
        .await?;
    assert_eq!(taken.state, crate::persistence::ReportState::Rejected);
    assert_eq!(
        text(
            &path,
            &format!("SELECT state || ' ' || revision FROM tasks WHERE task_id = '{task}'")
        )?,
        "ACCEPTED 5"
    );
    // Its revision is due again: the report was REJECTED.
    assert!(store.reader()?.spec_review_due("R")?.is_some());
    Ok(())
}

/// Once `counting`, fails the commit of the write that reaches the `nth` EventsWritten point of the runner's writer
/// jobs (the review end's own job, or the generic runner job it was): what a kill inside that write leaves, the
/// transaction rolled back.
#[derive(Debug)]
struct FailAtEvents {
    nth: u32,
    counting: std::sync::atomic::AtomicBool,
    seen: std::sync::atomic::AtomicU32,
    armed: std::sync::atomic::AtomicBool,
}

impl crate::persistence::FaultHooks for FailAtEvents {
    fn at(&self, job: &'static str, point: crate::persistence::WritePoint) {
        use std::sync::atomic::Ordering;
        if self.counting.load(Ordering::SeqCst)
            && ["Runner", "FinishedReview"].contains(&job)
            && point == crate::persistence::WritePoint::EventsWritten
            && self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.nth
        {
            self.armed.store(true, Ordering::SeqCst);
        }
    }

    fn before_commit(&self) -> Result<(), crate::persistence::PersistError> {
        if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(crate::persistence::PersistError::Full(
                "injected: the review end's write failed".to_owned(),
            ));
        }
        Ok(())
    }
}

/// [`world`] with `faults` in the store.
fn world_with(
    dir: &Path,
    faults: std::sync::Arc<dyn crate::persistence::FaultHooks>,
) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            faults,
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    Ok((store, path))
}

/// The review opened in `store` with its task RUNNING and its attempt AR RUNNING in the reviewer's session (its work
/// effect OBSERVED, done); its review id and task id.
async fn running_review(store: &Store, path: &Path) -> R<(String, String)> {
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let (review, task) = (
        text(
            path,
            "SELECT json_extract(payload_json, '$.review_id') FROM events WHERE kind = 'spec_review.opened'",
        )?,
        text(path, "SELECT task_id FROM tasks WHERE role = 'reviewer'")?,
    );
    exec(
        path,
        &format!(
            "UPDATE tasks SET state = 'RUNNING', revision = 2 WHERE task_id = '{task}';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
               started_at) VALUES ('AR', '{task}', 'sr', 'RUNNING', 1, 1, 1, '2026-11-22T00:00:01Z');
             INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{}', 'AR', 'OBSERVED');",
            crate::reconcile::work_key("AR")
        ),
    )?;
    Ok((review, task))
}

/// A review attempt's successful end, as the runner hands it.
fn ended(unknown_effect: bool) -> crate::persistence::Ending {
    crate::persistence::Ending {
        success: true,
        detail: "reviewed".to_owned(),
        class: crate::retry::FailureClass::Transient { retry_after: None },
        at: SystemTime::now(),
        draw: 0,
        observed: crate::persistence::Observed::default(),
        unknown_effect,
    }
}

/// Its intake without a report (REJECTED, report_missing) whose raw log is `raw`.
fn bare_intake(review: &str, raw: &[u8]) -> crate::persistence::SpecReviewIntake {
    let sha256 = crate::specindex::digest(raw);
    crate::persistence::SpecReviewIntake {
        review_id: review.to_owned(),
        attempt_id: "AR".to_owned(),
        report: None,
        native: None,
        raw: crate::persistence::ArtifactRow {
            path: ".hoh/attempts/AR/stdout.jsonl".to_owned(),
            sha256: sha256.clone(),
            bytes: raw.len() as u64,
            mime: "application/jsonl".to_owned(),
            kind: "raw_log".to_owned(),
            name: "stdout.jsonl".to_owned(),
        },
        raw_sha256: sha256,
    }
}

/// The attempt's, its task's state and the review events' count: `<attempt> <task> <reported> <ended> <opened>`.
fn ledger(path: &Path, task: &str) -> R<String> {
    text(
        path,
        &format!(
            "SELECT (SELECT state FROM attempts WHERE attempt_id = 'AR') || ' ' || \
             (SELECT state FROM tasks WHERE task_id = '{task}') || ' ' || \
             (SELECT count(*) FROM events WHERE kind = 'spec_review.reported') || ' ' || \
             (SELECT count(*) FROM events WHERE kind = 'spec_review.ended') || ' ' || \
             (SELECT count(*) FROM events WHERE kind = 'spec_review.opened')"
        ),
    )
}

/// A kill at each write of the review end's job (the attempt's move and end, then its report taken in): nothing of the
/// end is left, so the attempt is still RUNNING for the reconcile, or all of it is; never its task IMPLEMENTED without
/// a report (REQ-031 behaviour 3).
#[tokio::test]
async fn test_031_04_a_review_end_and_its_intake_commit_together() -> R {
    let mut killed = 0;
    for nth in 1..=20 {
        let dir = tempfile::tempdir()?;
        let faults = std::sync::Arc::new(FailAtEvents {
            nth,
            counting: false.into(),
            seen: 0.into(),
            armed: false.into(),
        });
        let (store, path) = world_with(dir.path(), faults.clone())?;
        let (review, task) = running_review(&store, &path).await?;
        faults
            .counting
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ended_at = store
            .finish_spec_review("AR", ended(false), Some(bare_intake(&review, b"{}\n")))
            .await;
        if ended_at.is_ok() {
            assert_eq!(ledger(&path, &task)?, "SETTLED ACCEPTED 1 0 1", "{nth}");
            break;
        }
        killed += 1;
        assert_eq!(
            ledger(&path, &task)?,
            "RUNNING RUNNING 0 0 1",
            "write {nth}"
        );
        // Ended again: the end and its report together.
        assert!(
            store
                .finish_spec_review("AR", ended(false), Some(bare_intake(&review, b"{}\n")))
                .await?
        );
        assert_eq!(ledger(&path, &task)?, "SETTLED ACCEPTED 1 0 1", "{nth}");
    }
    // The end's own writes and the report's.
    assert!(killed >= 2, "{killed}");
    Ok(())
}

/// A review attempt a reconcile settles (its work observed done, its final message lost with its host): the review
/// task ends without a report (`spec_review.ended`, once), and the revision is reviewed again under a new review id,
/// once (REQ-031 behaviour 3).
#[tokio::test]
async fn test_031_04_a_review_settled_by_a_reconcile_ends_and_is_opened_again_once() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    let (review, task) = running_review(&store, &path).await?;
    let rows = store.reconcile_scan("p1", SystemTime::now()).await?;
    let row = rows
        .into_iter()
        .find(|r| r.attempt_id == "AR")
        .ok_or("AR not unfinished")?;
    let disposition = crate::reconcile::dispose(&crate::reconcile::Unfinished {
        attempt_id: row.attempt_id.clone(),
        state: row.state,
        task_state: row.task_state,
        process: crate::reconcile::Process::Gone {
            basis: "no_process".to_owned(),
        },
        effects: row
            .effects
            .iter()
            .map(|e| crate::reconcile::Effect {
                key: e.key.clone(),
                state: e.state,
                observation: None,
            })
            .collect(),
        lease_expired: row.lease_expired,
        work_outcome: Some(true),
        writer: row.writer,
        verifier: row.verifier,
    });
    assert!(
        matches!(disposition, crate::reconcile::Disposition::Completed { .. }),
        "{disposition:?}"
    );
    store
        .reconcile_apply(
            row,
            disposition,
            crate::reconcile::Process::Gone {
                basis: "no_process".to_owned(),
            },
            "reconcile",
            SystemTime::now(),
            0,
        )
        .await?;
    assert_eq!(ledger(&path, &task)?, "SETTLED ACCEPTED 0 1 1");
    let ended = events(&path, "spec_review.ended")?;
    assert_eq!(ended[0]["review_id"], json!(review));
    assert_eq!(ended[0]["task_id"], json!(task));
    assert_eq!(ended[0]["attempt_id"], "AR");
    assert_eq!(ended[0]["spec_revision"], 1);
    assert!(
        ended[0]["reason"]
            .as_str()
            .is_some_and(|r| r.starts_with("report_not_collected: ")),
        "{ended:?}"
    );
    // Reviewed again, once: a new review id, then in progress.
    let reader = store.reader()?;
    assert!(reader.spec_review_due("R")?.is_some());
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    assert!(!store.open_spec_review("R", "s1", COMMIT).await?);
    let opened = events(&path, "spec_review.opened")?;
    assert_eq!(opened.len(), 2);
    assert_ne!(opened[1]["review_id"], json!(review));
    Ok(())
}

/// A review attempt whose end is not a settle takes no report in: cancelled (its task the cancel's to move) or ended
/// with an effect not confirmed (its task left as a read task's is); a settled one handed no intake, or one its intake
/// refuses as inconsistent, ends its review without a report, once (REQ-031 behaviour 3).
#[tokio::test]
async fn test_031_04_only_a_settled_review_attempt_takes_its_report_in() -> R {
    for case in ["cancelled", "unknown_effect", "no_intake", "refused"] {
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        let (review, task) = running_review(&store, &path).await?;
        if case == "cancelled" {
            exec(
                &path,
                "UPDATE attempts SET cancel_requested = 1 WHERE attempt_id = 'AR'",
            )?;
        }
        let mut intake = Some(bare_intake(&review, b"{}\n"));
        if case == "no_intake" {
            intake = None;
        }
        if case == "refused" {
            // Another review's id: the intake's integrity error, not a reason.
            intake = Some(bare_intake("no-such-review", b"{}\n"));
        }
        store
            .finish_spec_review("AR", ended(case == "unknown_effect"), intake)
            .await?;
        let want = match case {
            "cancelled" => "CANCELLED RUNNING 0 0 1",
            "unknown_effect" => "UNKNOWN_EFFECT RUNNING 0 0 1",
            _ => "SETTLED ACCEPTED 0 1 1",
        };
        assert_eq!(ledger(&path, &task)?, want, "{case}");
        let due = store.reader()?.spec_review_due("R")?;
        assert_eq!(due.is_some(), want.contains("ACCEPTED"), "{case}");
        if case == "refused" {
            let ended = events(&path, "spec_review.ended")?;
            assert!(
                ended[0]["reason"]
                    .as_str()
                    .is_some_and(|r| r.starts_with("intake_refused: ")),
                "{ended:?}"
            );
        }
    }
    Ok(())
}

/// [`WORLD`] with TP planned for REQ-A of revision 1, which has a CURRENT PASS review report: TP and the run are
/// ready.
const PLANNED: &str = "
INSERT INTO requirements(requirement_id, spec_id, title, required_release, release_class) VALUES
  ('REQ-A', 's1', 'A', 1, 'required');
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-A-01', 'REQ-A', 's1', json_object('id', 'AC-A-01', 'given', 'g', 'when', 'w', 'then', 'original',
   'test_id', 'TEST-A-01'), 'unit');
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('TP', 'REQ-A', 's1');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'plan', 'R', 'plan.imported', json_object('tasks', json_array(json_object('task_id', 'TP'))),
   '2026-11-22T00:00:00Z'),
  ('p1', 'old-pass', 'R', 'spec_review.reported', json_object('spec_id', 's1', 'spec_revision', 1,
   'state', 'CURRENT', 'status', 'PASS', 'findings', json_array()), '2026-11-22T00:00:00Z');
";

/// Revision 2 of the project (REQ-A's criterion changed) proposed and approved through the store's command entry, as
/// the host submits it: the run adopts it and its tasks migrate to it. Its sha256.
async fn approve_revision_2(store: &Store, path: &Path) -> R<String> {
    use crate::persistence::{
        Intent, NewEvent, RunTarget, SpecAction, SpecFiles, SpecRequest, SubmitCommand,
    };
    let source = crate::specindex::Source {
        path: "docs/spec/spec.md".to_owned(),
        bytes: "# 用户\nu\n# 范围\ns\n# 非目标\nn\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A A\n### AC-A-01\n\
                given: g\nwhen: w\nthen: revised\ncheck: unit\n"
            .as_bytes()
            .to_vec(),
    };
    let index = crate::specindex::generate_index(std::slice::from_ref(&source))?;
    for (id, kind) in [("p2", "ProposeSpecChange"), ("a2", "ApproveSpec")] {
        let (payload, action) = if kind == "ProposeSpecChange" {
            let payload = json!({"spec_sha256": index.sha256(), "reason": "revision 2"});
            let action = SpecAction::Propose(serde_json::from_value(payload.clone())?);
            (payload, action)
        } else {
            let payload = json!({"spec_sha256": index.sha256(), "decision": "APPROVE_ONLY"});
            let action = SpecAction::Approve(serde_json::from_value(payload.clone())?);
            (payload, action)
        };
        let revision: i64 =
            Connection::open(path)?.query_row("SELECT revision FROM runs", [], |r| r.get(0))?;
        let files = SpecFiles {
            sources: vec![source.clone()],
            existing_index: Some(index.bytes().to_vec()),
            tracked_index: Some(index.bytes().to_vec()),
        };
        store
            .submit_command(SubmitCommand {
                command_id: id.into(),
                idempotency_key: id.into(),
                kind: kind.into(),
                run_id: "R".into(),
                expected_revision: revision,
                payload: payload.clone(),
                payload_sha256: crate::specindex::digest(&serde_json::to_vec(&payload)?),
                target: RunTarget::Specification(Box::new(SpecRequest {
                    // The approval comes from the user (M3-T04 S2's origin guard admits only User).
                    origin: crate::origin::Origin::User,
                    action,
                    files: Ok(files),
                })),
                event: NewEvent {
                    intent: Intent::Progress,
                    project_id: "p1".into(),
                    event_id: format!("event-{id}"),
                    run_id: Some("R".into()),
                    kind: "spec.changed".into(),
                    payload: json!({}),
                },
                outbox: None,
            })
            .await?;
    }
    assert_eq!(
        text(
            path,
            "SELECT s.revision || '' FROM runs r JOIN specs s ON s.spec_id = r.spec_id"
        )?,
        "2"
    );
    Ok(index.sha256())
}

/// M3J-01: a revision approved after the tick's readiness step judged the old one ready, and before the rounds,
/// dispatches no planned writer before its own review's PASS (REQ-031 behaviours 3 and 5, AC-031-04): the round
/// judges the task's readiness under the run's current revision in its own transaction. The review task of the new
/// revision goes; TP stays undispatched, and the next readiness step blocks it.
#[tokio::test]
async fn test_031_04_a_revision_approved_between_readiness_and_the_rounds_dispatches_no_writer() -> R
{
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    exec(&path, PLANNED)?;
    exec(&path, "UPDATE runs SET state = 'RUNNING'")?;
    // The tick's readiness step: revision 1 is ready, nothing to change.
    assert!(!store.settle_readiness("R").await?);
    // Approved before the rounds.
    approve_revision_2(&store, &path).await?;
    assert_eq!(
        text(
            &path,
            "SELECT state || ' ' || spec_id FROM tasks WHERE task_id = 'TP'"
        )?,
        format!(
            "READY {}",
            text(&path, "SELECT spec_id FROM runs WHERE run_id = 'R'")?
        )
    );
    let due = store.reader()?.spec_review_due("R")?.ok_or("not due")?;
    assert!(store.open_spec_review("R", &due.spec_id, COMMIT).await?);
    let mut dispatched = Vec::new();
    for _ in 0..3 {
        match store.schedule_round("R", context()).await?.dispatched {
            Some(d) => dispatched.push(d.task_id),
            None => break,
        }
    }
    let review = text(
        &path,
        &format!(
            "SELECT json_extract(payload_json, '$.task_id') FROM events WHERE kind = 'spec_review.opened' \
             AND json_extract(payload_json, '$.spec_id') = '{}'",
            due.spec_id
        ),
    )?;
    assert_eq!(dispatched, [review], "no writer before the review's PASS");
    assert_eq!(
        text(&path, "SELECT state FROM tasks WHERE task_id = 'TP'")?,
        "READY"
    );
    // Positive control: the next step blocks it for the missing review.
    assert!(store.settle_readiness("R").await?);
    assert_eq!(
        text(
            &path,
            "SELECT state || ' ' || blocked_reason FROM tasks WHERE task_id = 'TP'"
        )?,
        "BLOCKED READINESS_NOT_MET"
    );
    Ok(())
}

/// M3J-02: a review task opened for revision 1 and dispatched after revision 2 was approved (its task migrated with
/// the run) works on revision 1 throughout: its work (target commit, report identity) and its dispatch's context both
/// name revision 1, never the run's current revision.
#[tokio::test]
async fn test_031_04_a_review_dispatched_after_a_newer_approval_keeps_its_own_revision() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    exec(
        &path,
        "UPDATE tasks SET state = 'ACCEPTED' WHERE task_id = 'TP'",
    )?;
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let task = text(&path, "SELECT task_id FROM tasks WHERE role = 'reviewer'")?;
    let new_sha = approve_revision_2(&store, &path).await?;
    let work = store.reader()?.spec_review_work(&task)?.ok_or("no work")?;
    assert_eq!(
        (
            work.spec_revision,
            work.spec_sha256.as_str(),
            work.git_commit.as_str()
        ),
        (1, "1".repeat(64).as_str(), COMMIT)
    );
    let round = store.schedule_round("R", context()).await?;
    assert_eq!(round.dispatched.ok_or("not dispatched")?.task_id, task);
    let waiting = store.runner_waiting("p1").await?;
    let notice = waiting
        .dispatches
        .iter()
        .find(|n| n.task_id == task)
        .ok_or("no dispatch")?;
    let facts = notice.assignment.context.as_ref().ok_or("no context")?;
    assert_eq!(facts.spec, Some(("1".repeat(64), 1)));
    assert_ne!(
        facts.spec.as_ref().map(|s| s.0.as_str()),
        Some(new_sha.as_str())
    );
    Ok(())
}

/// The Commander of [`WORLD`] proposing to dispatch `task` to `role` at the run's current revision and specification.
fn commander_dispatch(
    path: &Path,
    id: &str,
    task: &str,
    role: &str,
) -> R<crate::decision::Proposal> {
    let (revision, sha): (i64, String) = Connection::open(path)?.query_row(
        "SELECT r.revision, s.sha256 FROM runs r JOIN specs s ON s.spec_id = r.spec_id",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let message: crate::protocol::Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 1, "correlation_id": "c",
        "causation_id": null, "idempotency_key": id, "expected_revision": revision, "spec_sha256": sha,
        "candidate_id": null, "kind": "decision",
        "payload": {"kind": "dispatch", "task_id": task, "role": role}
    }))?;
    Ok(crate::decision::Proposal::from_message(&message)?)
}

/// Each path that creates a work attempt, as the design names them (docs/adr/ADR-035.md decision 12).
#[derive(Debug, Clone, Copy)]
enum Path3 {
    Round,
    Commander,
    Direct,
}

/// M3J-100: a revision approved after the tick's readiness step judged the old one ready, and before a dispatch,
/// gives planned writer TP no attempt on any path that creates one while the new revision's review is missing,
/// REJECTED or CURRENT/FAIL (REQ-031 behaviours 3 and 5, docs/adr/ADR-035.md decision 12): the scheduling round
/// leaves it, the Commander's dispatch is REJECTED ILLEGAL_TRANSITION with the readiness reasons, a direct creation is
/// refused. With a CURRENT PASS every path admits it (the positive control).
#[tokio::test]
async fn test_031_04_every_attempt_creating_path_refuses_a_task_unready_after_an_approval_between_settle_and_dispatch()
-> R {
    let reviews = [
        ("missing", None),
        ("rejected", Some(("REJECTED", "null"))),
        ("fail", Some(("CURRENT", "'FAIL'"))),
        ("pass", Some(("CURRENT", "'PASS'"))),
    ];
    for path_kind in [Path3::Round, Path3::Commander, Path3::Direct] {
        for (name, review) in reviews {
            let case = format!("{path_kind:?}/{name}");
            let dir = tempfile::tempdir()?;
            let (store, path) = world(dir.path())?;
            exec(&path, PLANNED)?;
            exec(&path, "UPDATE runs SET state = 'RUNNING'")?;
            // The tick's readiness step: revision 1 is ready, nothing to change.
            assert!(!store.settle_readiness("R").await?, "{case}");
            // Approved before the dispatch: TP migrates to revision 2 and stays READY.
            approve_revision_2(&store, &path).await?;
            let spec = text(&path, "SELECT spec_id FROM runs WHERE run_id = 'R'")?;
            assert_eq!(
                text(
                    &path,
                    "SELECT state || ' ' || spec_id FROM tasks WHERE task_id = 'TP'"
                )?,
                format!("READY {spec}"),
                "{case}"
            );
            if let Some((state, status)) = review {
                exec(
                    &path,
                    &format!(
                        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
                         ('p1', 'new-review', 'R', 'spec_review.reported', json_object('spec_id', '{spec}', \
                         'spec_revision', 2, 'state', '{state}', 'status', {status}, 'reasons', json_array(), \
                         'findings', json_array()), '2026-11-22T00:00:02Z');"
                    ),
                )?;
            }
            let admitted = name == "pass";
            match path_kind {
                Path3::Round => {
                    let dispatched = store.schedule_round("R", context()).await?.dispatched;
                    assert_eq!(
                        dispatched.map(|d| d.task_id),
                        admitted.then(|| "TP".to_owned()),
                        "{case}"
                    );
                }
                Path3::Commander => {
                    let decided = store
                        .decide(
                            commander_dispatch(&path, &format!("d-{name}"), "TP", "developer")?,
                            context(),
                        )
                        .await?;
                    if admitted {
                        assert!(
                            matches!(decided, crate::persistence::Decided::Applied { .. }),
                            "{case}: {decided:?}"
                        );
                    } else {
                        let crate::persistence::Decided::Rejected { reason, .. } = decided else {
                            return Err(format!("{case}: {decided:?}").into());
                        };
                        assert_eq!(
                            reason,
                            crate::protocol::RejectionReason::IllegalTransition,
                            "{case}"
                        );
                        let rejected = events(&path, "decision.rejected")?;
                        let reasons = &rejected.last().ok_or("no rejection")?["details"]["reasons"];
                        assert_eq!(
                            reasons,
                            &json!([{"reason": "spec_review", "ids": [], "detail": name}]),
                            "{case}"
                        );
                    }
                }
                Path3::Direct => {
                    let created = store
                        .create_attempt(crate::persistence::NewAttempt {
                            attempt_id: "A-direct".to_owned(),
                            task_id: "TP".to_owned(),
                            session_id: None,
                            role_epoch: 1,
                            host_epoch: 1,
                            config_revision: 1,
                            agent: Some("claude".to_owned()),
                        })
                        .await;
                    match created {
                        Ok(()) => assert!(admitted, "{case}"),
                        Err(crate::persistence::PersistError::IllegalTransition {
                            details,
                            ..
                        }) => {
                            assert!(!admitted, "{case}");
                            assert_eq!(
                                details.as_deref(),
                                Some(
                                    &json!({"reasons": [{"reason": "spec_review", "ids": [], "detail": name}]})
                                ),
                                "{case}"
                            );
                        }
                        Err(e) => return Err(format!("{case}: {e:?}").into()),
                    }
                }
            }
            assert_eq!(
                text(
                    &path,
                    "SELECT count(*) || '' FROM attempts WHERE task_id = 'TP'"
                )?,
                if admitted { "1" } else { "0" },
                "{case}"
            );
        }
    }
    Ok(())
}

/// A review task is the Runtime's (docs/adr/ADR-035.md decision 12): the Commander's dispatch of it is REJECTED
/// ILLEGAL_TRANSITION and a direct creation refused, both with no attempt, so neither bypasses the round's independence
/// pre-check; the scheduling round dispatches it.
#[tokio::test]
async fn test_031_04_a_review_task_is_dispatched_by_the_round_only() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = world(dir.path())?;
    exec(
        &path,
        "UPDATE tasks SET state = 'ACCEPTED' WHERE task_id = 'TP'",
    )?;
    assert!(store.open_spec_review("R", "s1", COMMIT).await?);
    let task = text(&path, "SELECT task_id FROM tasks WHERE role = 'reviewer'")?;
    let decided = store
        .decide(
            commander_dispatch(&path, "d-review", &task, "reviewer")?,
            context(),
        )
        .await?;
    let crate::persistence::Decided::Rejected { reason, detail, .. } = decided else {
        return Err(format!("{decided:?}").into());
    };
    assert_eq!(
        reason,
        crate::protocol::RejectionReason::IllegalTransition,
        "{detail}"
    );
    let created = store
        .create_attempt(crate::persistence::NewAttempt {
            attempt_id: "A-direct".to_owned(),
            task_id: task.clone(),
            session_id: None,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 1,
            agent: Some("claude".to_owned()),
        })
        .await;
    assert!(
        matches!(
            created,
            Err(crate::persistence::PersistError::IllegalTransition { .. })
        ),
        "{created:?}"
    );
    assert_eq!(
        text(
            &path,
            &format!("SELECT count(*) || '' FROM attempts WHERE task_id = '{task}'")
        )?,
        "0"
    );
    let round = store.schedule_round("R", context()).await?;
    assert_eq!(round.dispatched.ok_or("not dispatched")?.task_id, task);
    Ok(())
}
