//! TEST-043-05 and the cancel's store writes against a real store: a fake process group, a FakeClock.

use crate::cancel::{Canceller, GRACES, Look, ProcessControl, Processes, Signal};
use crate::delivery::Clock;
use crate::lifecycle;
use crate::persistence::{
    AttemptRow, CommandOutcome, Finished, NewEvent, NewOutbox, Options, RunTarget, Store,
    SubmitCommand, selftest,
};
use crate::protocol::RunState;
use crate::time::parse_rfc3339;
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// Run R (RUNNING) with T1 RUNNING and its attempt A1 RUNNING since 00:00:00, its work STARTED.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's1', 'RUNNING', 3, 'developer', 1, 1),
  ('T2', 'R', 's1', 'READY', 1, 'developer', 2, 1);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, started_at) VALUES
  ('A1', 'T1', 'RUNNING', 1, 1, 1, 4242, '2026-11-07T00:00:00Z');
INSERT INTO effects(effect_key, attempt_id, state) VALUES ('A1/work', 'A1', 'STARTED');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 2), printf('%064d', 3), printf('%064d', 4), printf('%064d', 5),
   '/repo', 'VERIFYING', '2026-11-07T00:00:00Z');
";

/// The CancelRun the host submits for R at `revision` (docs/adr/ADR-021.md decision 8b).
fn cancel_run(revision: i64) -> SubmitCommand {
    SubmitCommand {
        command_id: "c-cancel".to_owned(),
        idempotency_key: "k-cancel".to_owned(),
        kind: "CancelRun".to_owned(),
        run_id: "R".to_owned(),
        expected_revision: revision,
        payload: json!({"reason": "test"}),
        payload_sha256: format!("{:064}", 9),
        target: RunTarget::To(RunState::CancelRequested),
        event: NewEvent {
            project_id: "p1".to_owned(),
            event_id: "e-cancel".to_owned(),
            run_id: Some("R".to_owned()),
            kind: "run.cancel_requested".to_owned(),
            payload: json!({"command_id": "c-cancel", "state": "CANCEL_REQUESTED"}),
        },
        outbox: Some(NewOutbox {
            message_id: "m-cancel".to_owned(),
            event: 0,
            run_id: Some("R".to_owned()),
            recipient_role: "commander".to_owned(),
            role_epoch: 1,
            payload: json!({"notice": "runtime", "kind": "run.control", "control": "CANCEL_REQUESTED", "run_id": "R"}),
        }),
    }
}

/// Time on the test's say: from 00:00:00, advanced by what the cancel waits.
#[derive(Debug)]
struct FakeClock(Mutex<Duration>);

impl FakeClock {
    fn secs(&self) -> u64 {
        self.0.lock().map(|d| d.as_secs()).unwrap_or(0)
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        parse_rfc3339("2026-11-07T00:00:00Z").unwrap_or(SystemTime::UNIX_EPOCH)
            + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

/// When the group was first SIGKILLed and everything it was sent (seconds on the clock).
type Sent = Arc<Mutex<(Option<u64>, Vec<(Signal, u64)>)>>;

/// A process group that reports itself alive until `exits_after` past its first SIGKILL, and what it was sent.
struct Group {
    clock: Arc<FakeClock>,
    exits_after: u64,
    state: Sent,
}

impl ProcessControl for Group {
    fn look(&mut self) -> Look {
        let killed = self.state.lock().map(|s| s.0).unwrap_or(None);
        match killed {
            Some(at) if self.clock.secs() >= at + self.exits_after => Look::Empty,
            _ => Look::Held,
        }
    }
    fn send(&mut self, signal: Signal) -> bool {
        let now = self.clock.secs();
        if let Ok(mut s) = self.state.lock() {
            if signal == Signal::Kill && s.0.is_none() {
                s.0 = Some(now);
            }
            s.1.push((signal, now));
        }
        true
    }
}

struct Fake {
    clock: Arc<FakeClock>,
    state: Sent,
}

impl Processes for Fake {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        (attempt.attempt_id == "A1").then(|| {
            let group: Box<dyn ProcessControl + Send> = Box::new(Group {
                clock: self.clock.clone(),
                exits_after: 30,
                state: self.state.clone(),
            });
            (group, true)
        })
    }
    fn workspace(&self) -> Value {
        json!({"workspace_sha256": "fake"})
    }
}

fn count(path: &Path, sql: &str) -> R<i64> {
    Ok(Connection::open(path)?.query_row(sql, [], |r| r.get(0))?)
}

#[tokio::test]
async fn test_043_05_a_cancel_stays_cancelling_while_the_group_lives_after_sigkill_and_ends_once_it_exits()
-> R {
    // AC-043-05: a ProcessControl fake still reports its group alive 30 s after SIGKILL. Until it reports the exit
    // the run stays CANCEL_REQUESTED, shown as CANCELLING, with no CANCELLED event; only then is it CANCELLED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let accepted = store.submit_command(cancel_run(1)).await?;
    assert!(
        matches!(accepted, CommandOutcome::Accepted { .. }),
        "{accepted:?}"
    );
    assert_eq!(
        count(
            &path,
            "SELECT cancel_requested FROM attempts WHERE attempt_id = 'A1'"
        )?,
        1
    );
    let clock = Arc::new(FakeClock(Mutex::new(Duration::ZERO)));
    let advance = clock.clone();
    let wait: Arc<dyn Fn(Duration) + Send + Sync> = Arc::new(move |d| {
        if let Ok(mut at) = advance.0.lock() {
            *at += d;
        }
    });
    let state = Arc::new(Mutex::new((None, Vec::new())));
    let processes = Fake {
        clock: clock.clone(),
        state: state.clone(),
    };
    let reader = store.reader()?;
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    let mut samples = Vec::new();
    loop {
        let step = canceller
            .step(
                &store,
                &reader,
                "R",
                &processes,
                clock.clone(),
                wait.clone(),
            )
            .await?;
        match step {
            Finished::Running(_) => {
                let Some((run, _)) = reader.snapshot("R")? else {
                    return Err("no run".into());
                };
                let shown =
                    lifecycle::label(crate::persistence::writer::parse::<RunState>(&run.state)?);
                samples.push((clock.secs(), run.state, shown));
                assert_eq!(
                    count(
                        &path,
                        "SELECT count(*) FROM events WHERE kind = 'run.cancelled'"
                    )?,
                    0
                );
            }
            Finished::Cancelled => break,
            Finished::NotCancelling => return Err("not cancelling".into()),
        }
        if samples.len() > 20 {
            return Err(format!("never ended: {samples:?}").into());
        }
    }
    let killed = state
        .lock()
        .map(|s| s.0)
        .unwrap_or(None)
        .ok_or("no SIGKILL")?;
    assert_eq!(killed, 15, "native at 0, SIGTERM at 10, SIGKILL at 15");
    // Sampled every 5 s from the first SIGKILL's grace on: CANCELLING all the 30 s.
    assert_eq!(
        samples,
        [20, 25, 30, 35, 40]
            .map(|t| (t, "CANCEL_REQUESTED".to_owned(), "CANCELLING"))
            .to_vec()
    );
    assert!(clock.secs() >= killed + 30);
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'run.cancelled'"
        )?,
        1
    );
    let states: (String, String, String, String, String) = Connection::open(&path)?.query_row(
        "SELECT r.state, a.state, t1.state, t2.state, c.state FROM runs r, attempts a, tasks t1, tasks t2, candidates c \
         WHERE r.run_id = 'R' AND a.attempt_id = 'A1' AND t1.task_id = 'T1' AND t2.task_id = 'T2' AND c.candidate_id = 'C1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    assert_eq!(
        states,
        (
            "CANCELLED".to_owned(),
            "CANCELLED".to_owned(),
            "CANCELLED".to_owned(),
            "CANCELLED".to_owned(),
            "VERIFYING".to_owned()
        ),
        "the candidate is not marked verified"
    );
    // The order AC-043-02 observes: requested, interrupt, SIGTERM, SIGKILL, the group reaped, the workspace, CANCELLED.
    let kinds: Vec<String> = {
        let conn = Connection::open(&path)?;
        let mut statement = conn.prepare(
            "SELECT kind FROM events WHERE kind IN ('run.cancel_requested', 'cancel.native_cancel', 'cancel.sigterm', \
             'cancel.reaped', 'cancel.workspace', 'run.cancelled') OR (kind = 'cancel.sigkill' AND seq = \
             (SELECT min(seq) FROM events WHERE kind = 'cancel.sigkill')) ORDER BY seq",
        )?;
        statement
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?
    };
    assert_eq!(
        kinds,
        [
            "run.cancel_requested",
            "cancel.native_cancel",
            "cancel.sigterm",
            "cancel.sigkill",
            "cancel.reaped",
            "cancel.workspace",
            "run.cancelled"
        ]
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM effects WHERE effect_key = 'A1/work' AND state = 'UNKNOWN'"
        )?,
        1
    );
    Ok(())
}

/// A command for R of `kind` to `target` at `revision`, its key `key`.
fn command(kind: &str, key: &str, revision: i64, target: RunTarget) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{key}"),
        idempotency_key: key.to_owned(),
        kind: kind.to_owned(),
        payload_sha256: format!("{:064}", key.len()),
        target,
        event: NewEvent {
            event_id: format!("e-{key}"),
            kind: format!("run.{}", kind.to_lowercase()),
            ..cancel_run(revision).event
        },
        outbox: None,
        ..cancel_run(revision)
    }
}

fn run_state(path: &Path) -> R<(String, i64, Option<String>)> {
    Ok(Connection::open(path)?.query_row(
        "SELECT state, revision, resume_state FROM runs WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

#[tokio::test]
async fn a_resume_records_the_runs_reconcile_before_it_works_again_and_only_from_paused() -> R {
    // REQ-043 behaviour 2 (docs/adr/ADR-021.md decision 7): a paused run resumes to the state it was paused in, its
    // reconcile recorded first; a run not paused does not resume.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE attempts SET state = 'UNKNOWN_EFFECT' WHERE attempt_id = 'A1';
         UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING', revision = 4 WHERE run_id = 'R';"
    ))?;
    let early = store
        .submit_command(command("ResumeRun", "resume-stale", 3, RunTarget::Resume))
        .await;
    assert!(early.is_err(), "{early:?}");
    let resumed = store
        .submit_command(command("ResumeRun", "resume", 4, RunTarget::Resume))
        .await?;
    assert!(
        matches!(resumed, CommandOutcome::Accepted { revision: 5, .. }),
        "{resumed:?}"
    );
    assert_eq!(run_state(&path)?, ("RUNNING".to_owned(), 5, None));
    let (kinds, unknown): (String, String) = Connection::open(&path)?.query_row(
        "SELECT group_concat(kind, ','), (SELECT json_extract(payload_json, '$.unknown_effect') FROM events \
         WHERE kind = 'reconcile.run') FROM (SELECT kind FROM events ORDER BY seq)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(kinds, "reconcile.run,run.resumerun");
    assert_eq!(unknown, "[\"A1\"]");
    // RUNNING now: no resume.
    let again = store
        .submit_command(command("ResumeRun", "resume-again", 5, RunTarget::Resume))
        .await;
    assert!(
        matches!(
            again,
            Err(crate::persistence::PersistError::IllegalTransition { .. })
        ),
        "{again:?}"
    );
    Ok(())
}

#[tokio::test]
async fn a_started_run_is_created_with_its_exact_accepted_body_and_replayed_byte_for_byte() -> R {
    // REQ-005 behaviour 1, decision 5 (docs/adr/ADR-021.md decision 5): StartRun creates the run the host named,
    // CREATED at revision 0, in the command's transaction, and stores the 202 body a replay returns.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');",
    )?;
    let start = |key: &str, revision: i64| SubmitCommand {
        run_id: "R-new".to_owned(),
        event: NewEvent {
            run_id: Some("R-new".to_owned()),
            ..command("StartRun", key, revision, RunTarget::To(RunState::Created)).event
        },
        ..command(
            "StartRun",
            key,
            revision,
            RunTarget::Start {
                mode: "AUTO".to_owned(),
            },
        )
    };
    let stale = store.submit_command(start("start-stale", 1)).await;
    assert!(stale.is_err(), "{stale:?}");
    let CommandOutcome::Accepted {
        result_json,
        revision,
        ..
    } = store.submit_command(start("start", 0)).await?
    else {
        return Err("not accepted".into());
    };
    assert_eq!(revision, 0);
    let body: Value = serde_json::from_str(&result_json)?;
    assert_eq!(
        body,
        json!({"command_id": "c-start", "run_id": "R-new", "revision": 0, "status": "ACCEPTED"})
    );
    let replay = store.submit_command(start("start", 0)).await?;
    assert_eq!(replay, CommandOutcome::Replayed { result_json });
    let row: (String, String, i64) = Connection::open(&path)?.query_row(
        "SELECT state, mode, revision FROM runs WHERE run_id = 'R-new'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(row, ("CREATED".to_owned(), "AUTO".to_owned(), 0));
    assert_eq!(count(&path, "SELECT count(*) FROM runs")?, 1);
    Ok(())
}

#[tokio::test]
async fn a_cancel_wins_over_a_pause_and_a_pause_is_refused_while_cancelling_or_ended() -> R {
    // REQ-043 behaviour 8 and failure mode 4: Cancel during PAUSE_REQUESTED is accepted; Pause during
    // CANCEL_REQUESTED, and Pause or Cancel of a terminal run, are ILLEGAL_TRANSITION with nothing changed.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let pause = |key: &str, revision: i64| {
        command(
            "PauseRun",
            key,
            revision,
            RunTarget::To(RunState::PauseRequested),
        )
    };
    store.submit_command(pause("pause", 1)).await?;
    assert_eq!(run_state(&path)?.0, "PAUSE_REQUESTED");
    store.submit_command(cancel_run(2)).await?;
    assert_eq!(run_state(&path)?.0, "CANCEL_REQUESTED");
    let refused = store.submit_command(pause("pause-2", 3)).await;
    assert!(
        matches!(
            refused,
            Err(crate::persistence::PersistError::IllegalTransition { .. })
        ),
        "{refused:?}"
    );
    Connection::open(&path)?
        .execute_batch("UPDATE runs SET state = 'COMPLETED' WHERE run_id = 'R';")?;
    for (kind, target) in [
        ("PauseRun", RunState::PauseRequested),
        ("CancelRun", RunState::CancelRequested),
    ] {
        let ended = store
            .submit_command(command(
                kind,
                &format!("ended-{kind}"),
                3,
                RunTarget::To(target),
            ))
            .await;
        assert!(
            matches!(
                ended,
                Err(crate::persistence::PersistError::IllegalTransition { .. })
            ),
            "{kind}: {ended:?}"
        );
    }
    assert_eq!(
        run_state(&path)?,
        ("COMPLETED".to_owned(), 3, Some("RUNNING".to_owned()))
    );
    Ok(())
}
