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
    let processes: Arc<dyn Processes> = Arc::new(Fake {
        clock: clock.clone(),
        state: state.clone(),
    });
    let reader = store.reader()?;
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    let mut samples = Vec::new();
    loop {
        let step = canceller
            .step(&store, "R", processes.clone(), clock.clone(), wait.clone())
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

#[tokio::test]
async fn a_cancel_is_not_finished_while_an_attempt_still_runs_however_it_is_asked() -> R {
    // The final transaction runs only once nothing runs (docs/adr/ADR-021.md decision 8f): a FinishCancel that
    // finds A1 still RUNNING, whoever read the attempts before it, changes nothing and answers Running.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    store.submit_command(cancel_run(1)).await?;
    let before = count(&path, "SELECT count(*) FROM events")?;
    let finished = store
        .finish_cancel("R", json!({"workspace_sha256": "fake"}), SystemTime::now())
        .await?;
    assert_eq!(finished, Finished::Running(1));
    let states: (String, String, String) = Connection::open(&path)?.query_row(
        "SELECT r.state, a.state, t.state FROM runs r, attempts a, tasks t \
         WHERE r.run_id = 'R' AND a.attempt_id = 'A1' AND t.task_id = 'T2'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert_eq!(
        states,
        (
            "CANCEL_REQUESTED".to_owned(),
            "RUNNING".to_owned(),
            "READY".to_owned()
        )
    );
    assert_eq!(count(&path, "SELECT count(*) FROM events")?, before);
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
async fn a_resume_with_blockers_claimed_returns_to_blocked_and_keeps_its_resume_state() -> R {
    // docs/adr/ADR-025.md decision 5: a run paused while BLOCKED, its claims still open, is BLOCKED again on ResumeRun
    // (not refused), its resume state kept for when the claims are released.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE attempts SET state = 'FAILED', ended_at = '2026-11-07T00:00:05Z' WHERE attempt_id = 'A1';
         UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING', revision = 4, blocked_reason = 'NEED_DECISION',
           blocked_reasons_json = '[\"NEED_DECISION\"]' WHERE run_id = 'R';
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'claim-1', 'R', 'run.blocker_claimed', '{{\"reason\": \"NEED_DECISION\", \"source\": \"pace\"}}',
            '2026-11-07T00:00:00Z');"
    ))?;
    let resumed = store
        .submit_command(command("ResumeRun", "resume", 4, RunTarget::Resume))
        .await?;
    assert!(
        matches!(resumed, CommandOutcome::Accepted { revision: 5, .. }),
        "{resumed:?}"
    );
    assert_eq!(
        run_state(&path)?,
        ("BLOCKED".to_owned(), 5, Some("RUNNING".to_owned()))
    );
    let reasons: String = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json, '$.reasons') FROM events WHERE kind = 'run.blocked'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(reasons, "[\"NEED_DECISION\"]");
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
                budget: crate::config::Config::default().budget,
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

#[tokio::test]
async fn a_pause_waits_for_its_safe_point_and_withdraws_dispatches_not_yet_taken() -> R {
    // REQ-043 behaviour 1, failure mode 3 (docs/adr/ADR-021.md decision 7): A1 runs on, uninterrupted, and holds the
    // pause; T2's dispatch not yet taken is withdrawn (not counted); once A1 ended the run is PAUSED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET state = 'LEASED', revision = 2 WHERE task_id = 'T2';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
           ('A2', 'T2', 'PREPARED', 1, 1, 1);
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'A2/dispatch', 'R', 'task.dispatched', '{{}}', '2026-11-07T00:00:00Z');
         INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, payload_json, state) VALUES
           ('m-A2', (SELECT seq FROM events WHERE event_id = 'A2/dispatch'), 'R', 'developer',
            '{{\"notice\": \"runtime\", \"kind\": \"dispatch\", \"attempt_id\": \"A2\", \"task_id\": \"T2\"}}', 'PENDING');"
    ))?;
    store
        .submit_command(SubmitCommand {
            outbox: Some(NewOutbox {
                payload: json!({"notice": "runtime", "kind": "run.control", "control": "PAUSE_REQUESTED", "run_id": "R"}),
                ..cancel_run(1).outbox.ok_or("no outbox")?
            }),
            ..command("PauseRun", "pause", 1, RunTarget::To(RunState::PauseRequested))
        })
        .await?;
    let at = parse_rfc3339("2026-11-07T00:00:10Z").ok_or("time")?;
    assert_eq!(
        store.pause_step("R", at).await?,
        crate::persistence::PauseStep::Running(1)
    );
    let after: (String, String, String, String, String) = Connection::open(&path)?.query_row(
        "SELECT r.state, a1.state, a2.state, t2.state, o.state FROM runs r, attempts a1, attempts a2, tasks t2, outbox o \
         WHERE r.run_id = 'R' AND a1.attempt_id = 'A1' AND a2.attempt_id = 'A2' AND t2.task_id = 'T2' \
         AND o.message_id = 'm-A2'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    assert_eq!(
        after,
        (
            "PAUSE_REQUESTED".to_owned(),
            "RUNNING".to_owned(),
            "CANCELLED".to_owned(),
            "READY".to_owned(),
            "ACKED".to_owned()
        )
    );
    assert_eq!(
        count(
            &path,
            "SELECT attempt_count FROM tasks WHERE task_id = 'T2'"
        )?,
        0
    );
    Connection::open(&path)?
        .execute_batch("UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-07T00:00:12Z' WHERE attempt_id = 'A1';")?;
    assert_eq!(
        store.pause_step("R", at).await?,
        crate::persistence::PauseStep::Paused
    );
    assert_eq!(
        run_state(&path)?,
        ("PAUSED".to_owned(), 3, Some("RUNNING".to_owned()))
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'run.paused'"
        )?,
        1
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM outbox WHERE state = 'PENDING' AND json_extract(payload_json, '$.kind') = 'run.control'"
        )?,
        0
    );
    assert_eq!(
        store.pause_step("R", at).await?,
        crate::persistence::PauseStep::NotPausing
    );
    Ok(())
}

/// Processes whose group cannot be told apart: no signal may go (ADR-019 decision 8).
struct LostTrack;

impl ProcessControl for LostTrack {
    fn look(&mut self) -> Look {
        Look::Lost
    }
    fn send(&mut self, _signal: Signal) -> bool {
        false
    }
}

impl Processes for LostTrack {
    fn control(&self, _attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        Some((Box::new(LostTrack), false))
    }
    fn workspace(&self) -> Value {
        json!({})
    }
}

#[tokio::test]
async fn a_group_lost_track_of_keeps_the_run_cancelling_and_is_recorded_once() -> R {
    // ADR-021 decision 8c: a lost group gets no signal and the run stays CANCEL_REQUESTED; the same finding is not
    // written again at every step.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    store.submit_command(cancel_run(1)).await?;
    let clock: Arc<dyn Clock> = Arc::new(FakeClock(Mutex::new(Duration::ZERO)));
    let wait: Arc<dyn Fn(Duration) + Send + Sync> = Arc::new(|_| {});
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    for _ in 0..3 {
        let step = canceller
            .step(
                &store,
                "R",
                Arc::new(LostTrack),
                clock.clone(),
                wait.clone(),
            )
            .await?;
        assert_eq!(step, Finished::Running(1));
    }
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'cancel.lost'"
        )?,
        1
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM events WHERE kind LIKE 'cancel.sig%'"
        )?,
        0
    );
    assert_eq!(run_state(&path)?.0, "CANCEL_REQUESTED");
    Ok(())
}

#[tokio::test]
async fn a_cancelled_attempt_ends_cancelled_however_its_runner_or_a_reconcile_ends_it() -> R {
    // ADR-021 decision 8e: an attempt of a cancelled run ends CANCELLED, not counted and with no retry, whether its
    // runner saw its process fail or a later host's reconcile disposes of it.
    use crate::reconcile::{Disposition, Process};
    for by_reconcile in [false, true] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(WORLD)?;
        store.submit_command(cancel_run(1)).await?;
        let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
        if by_reconcile {
            let rows = store.reconcile_scan("p1", at).await?;
            let [row] = rows.as_slice() else {
                return Err(format!("unfinished: {rows:?}").into());
            };
            assert!(row.cancel_requested);
            store
                .reconcile_apply(
                    row.clone(),
                    Disposition::Ended {
                        to: crate::lifecycle::AttemptState::Failed,
                        task_to: crate::protocol::TaskState::Ready,
                        effects: Vec::new(),
                    },
                    Process::Gone {
                        basis: "no_process".to_owned(),
                    },
                    "reconcile",
                    at,
                    0,
                )
                .await?;
        } else {
            store
                .runner_finished(
                    "A1",
                    crate::persistence::Ending {
                        success: false,
                        detail: "exit 1".to_owned(),
                        class: crate::retry::FailureClass::Transient { retry_after: None },
                        at,
                        draw: 0,
                        observed: Default::default(),
                        unknown_effect: false,
                    },
                )
                .await?;
        }
        let row: (String, String, i64, Option<String>) = Connection::open(&path)?.query_row(
            "SELECT a.state, t.state, t.attempt_count, t.next_retry_at FROM attempts a, tasks t \
             WHERE a.attempt_id = 'A1' AND t.task_id = 'T1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        assert_eq!(
            row,
            ("CANCELLED".to_owned(), "RUNNING".to_owned(), 0, None),
            "by the reconcile: {by_reconcile}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_cancel_committed_between_a_reconciles_scan_and_its_apply_still_takes_precedence() -> R {
    // M2T06R-02: the scan saw A1 not cancelled, the CancelRun commits, then the apply (a settle ending it FAILED):
    // the apply reads the cancel in its own transaction, so A1 ends CANCELLED, not counted, with no retry.
    use crate::reconcile::{Disposition, Process};
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
    let rows = store.reconcile_scan("p1", at).await?;
    let [row] = rows.as_slice() else {
        return Err(format!("unfinished: {rows:?}").into());
    };
    assert!(!row.cancel_requested);
    assert!(matches!(
        store.submit_command(cancel_run(1)).await?,
        CommandOutcome::Accepted { .. }
    ));
    store
        .reconcile_apply(
            row.clone(),
            Disposition::Ended {
                to: crate::lifecycle::AttemptState::Failed,
                task_to: crate::protocol::TaskState::Ready,
                effects: Vec::new(),
            },
            Process::Gone {
                basis: "no_process".to_owned(),
            },
            "lease_expired",
            at,
            0,
        )
        .await?;
    let row: (String, String, i64, Option<String>) = Connection::open(&path)?.query_row(
        "SELECT a.state, t.state, t.attempt_count, t.next_retry_at FROM attempts a, tasks t \
         WHERE a.attempt_id = 'A1' AND t.task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(row, ("CANCELLED".to_owned(), "RUNNING".to_owned(), 0, None));
    assert_eq!(count(&path, "SELECT count(*) FROM attempts")?, 1);
    Ok(())
}

#[tokio::test]
async fn a_cancelled_attempt_whose_effect_is_unknown_ends_the_same_however_it_ends() -> R {
    // M2T07R-301 (ADR-021 decision 8e, docs/adr/ADR-022.md decision 12): a CancelRun committed while A1 runs, then A1
    // ends with its work effect started and never reported, by its runner or by a later host's reconcile: either way
    // A1 is CANCELLED, its effect UNKNOWN and named in a diagnostic, its task untouched, nothing counted or retried;
    // the cancel's last transaction then ends the run as always.
    use crate::reconcile::{Disposition, EffectMove, Process};
    use crate::retry::FailureClass;
    for by_reconcile in [false, true] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(WORLD)?;
        store.submit_command(cancel_run(1)).await?;
        let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
        let diagnostic_kind = if by_reconcile {
            let rows = store.reconcile_scan("p1", at).await?;
            let [row] = rows.as_slice() else {
                return Err(format!("unfinished: {rows:?}").into());
            };
            store
                .reconcile_apply(
                    row.clone(),
                    Disposition::UnknownEffect {
                        effects: vec![EffectMove {
                            key: "A1/work".to_owned(),
                            from: crate::reconcile::EffectState::Started,
                            to: crate::reconcile::EffectState::Unknown,
                            external_ref: None,
                        }],
                        unknown: vec!["A1/work".to_owned()],
                    },
                    Process::Gone {
                        basis: "no_process".to_owned(),
                    },
                    "reconcile",
                    at,
                    0,
                )
                .await?;
            "reconcile.diagnostic"
        } else {
            store
                .runner_finished(
                    "A1",
                    crate::persistence::Ending {
                        success: false,
                        detail: "PROTOCOL_INCOMPLETE".to_owned(),
                        class: FailureClass::Transient { retry_after: None },
                        at,
                        draw: 0,
                        observed: Default::default(),
                        unknown_effect: true,
                    },
                )
                .await?;
            "runner.diagnostic"
        };
        let conn = Connection::open(&path)?;
        let row: (String, String, String, i64, Option<String>) = conn.query_row(
            "SELECT a.state, e.state, t.state, t.attempt_count, t.next_retry_at FROM attempts a, effects e, tasks t \
             WHERE a.attempt_id = 'A1' AND e.effect_key = 'A1/work' AND t.task_id = 'T1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        assert_eq!(
            row,
            (
                "CANCELLED".to_owned(),
                "UNKNOWN".to_owned(),
                "RUNNING".to_owned(),
                0,
                None
            ),
            "by the reconcile: {by_reconcile}"
        );
        let keys: String = conn.query_row(
            "SELECT json_extract(payload_json, '$.effect_keys') FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.code') = 'UNKNOWN_EFFECT' AND json_extract(payload_json, '$.attempt_id') = 'A1'",
            [diagnostic_kind],
            |r| r.get(0),
        )?;
        assert_eq!(keys, r#"["A1/work"]"#, "by the reconcile: {by_reconcile}");
        store
            .finish_cancel("R", json!({"workspace_sha256": "fake"}), SystemTime::now())
            .await?;
        let ended: (String, String, String, String) = conn.query_row(
            "SELECT r.state, t.state, a.state, e.state FROM runs r, tasks t, attempts a, effects e \
             WHERE r.run_id = 'R' AND t.task_id = 'T1' AND a.attempt_id = 'A1' AND e.effect_key = 'A1/work'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;
        assert_eq!(
            ended,
            (
                "CANCELLED".to_owned(),
                "CANCELLED".to_owned(),
                "CANCELLED".to_owned(),
                "UNKNOWN".to_owned()
            ),
            "by the reconcile: {by_reconcile}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn an_unknown_effect_ends_the_same_by_its_runner_or_a_reconcile() -> R {
    // M2T07R-451 (docs/adr/ADR-022.md decision 12 v3, AC-041-05): with no cancel, A1 ends with its work effect started
    // and never reported, by its runner or by a later host's reconcile: either way A1 is UNKNOWN_EFFECT, its effect
    // UNKNOWN and named in a diagnostic, its task untouched, nothing counted or retried.
    use crate::reconcile::{Disposition, EffectMove, Process};
    use crate::retry::FailureClass;
    for by_reconcile in [false, true] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        Connection::open(&path)?.execute_batch(WORLD)?;
        let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
        let diagnostic_kind = if by_reconcile {
            let rows = store.reconcile_scan("p1", at).await?;
            let [row] = rows.as_slice() else {
                return Err(format!("unfinished: {rows:?}").into());
            };
            store
                .reconcile_apply(
                    row.clone(),
                    Disposition::UnknownEffect {
                        effects: vec![EffectMove {
                            key: "A1/work".to_owned(),
                            from: crate::reconcile::EffectState::Started,
                            to: crate::reconcile::EffectState::Unknown,
                            external_ref: None,
                        }],
                        unknown: vec!["A1/work".to_owned()],
                    },
                    Process::Gone {
                        basis: "no_process".to_owned(),
                    },
                    "reconcile",
                    at,
                    0,
                )
                .await?;
            "reconcile.diagnostic"
        } else {
            store
                .runner_finished(
                    "A1",
                    crate::persistence::Ending {
                        success: false,
                        detail: "PROTOCOL_INCOMPLETE".to_owned(),
                        class: FailureClass::Transient { retry_after: None },
                        at,
                        draw: 0,
                        observed: Default::default(),
                        unknown_effect: true,
                    },
                )
                .await?;
            "runner.diagnostic"
        };
        let conn = Connection::open(&path)?;
        let row: (String, String, String, i64, Option<String>) = conn.query_row(
            "SELECT a.state, e.state, t.state, t.attempt_count, t.next_retry_at FROM attempts a, effects e, tasks t \
             WHERE a.attempt_id = 'A1' AND e.effect_key = 'A1/work' AND t.task_id = 'T1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        assert_eq!(
            row,
            (
                "UNKNOWN_EFFECT".to_owned(),
                "UNKNOWN".to_owned(),
                "RUNNING".to_owned(),
                0,
                None
            ),
            "by the reconcile: {by_reconcile}"
        );
        let keys: String = conn.query_row(
            "SELECT json_extract(payload_json, '$.effect_keys') FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.code') = 'UNKNOWN_EFFECT' AND json_extract(payload_json, '$.attempt_id') = 'A1'",
            [diagnostic_kind],
            |r| r.get(0),
        )?;
        assert_eq!(keys, r#"["A1/work"]"#, "by the reconcile: {by_reconcile}");
    }
    Ok(())
}

#[tokio::test]
async fn a_runners_end_that_leaves_an_effect_started_is_an_unknown_effect_however_it_is_reported()
-> R {
    // M2T07R-451 (docs/adr/ADR-022.md decision 12 v3): the store reads the effects, not only the executor's word: an
    // end reported as a plain failure with A1's work effect still STARTED (never observed) is UNKNOWN_EFFECT, its
    // effect UNKNOWN, nothing counted or retried, as a reconcile would end it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
    store
        .runner_finished(
            "A1",
            crate::persistence::Ending {
                success: false,
                detail: "exit 1".to_owned(),
                class: crate::retry::FailureClass::Transient { retry_after: None },
                at,
                draw: 0,
                observed: Default::default(),
                unknown_effect: false,
            },
        )
        .await?;
    let row: (String, String, String, i64, Option<String>) = Connection::open(&path)?.query_row(
        "SELECT a.state, e.state, t.state, t.attempt_count, t.next_retry_at FROM attempts a, effects e, tasks t \
         WHERE a.attempt_id = 'A1' AND e.effect_key = 'A1/work' AND t.task_id = 'T1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    assert_eq!(
        row,
        (
            "UNKNOWN_EFFECT".to_owned(),
            "UNKNOWN".to_owned(),
            "RUNNING".to_owned(),
            0,
            None
        )
    );
    Ok(())
}

#[tokio::test]
async fn a_cancels_pass_that_ends_an_attempt_names_the_effects_it_left_unknown() -> R {
    // M2T07R-552 (docs/adr/ADR-022.md decision 12 v3): the cancel's own pass ends A1 CANCELLED with its work effect,
    // started and never observed, UNKNOWN, and names it in a diagnostic, as the runner's end and a reconcile do.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    store.submit_command(cancel_run(1)).await?;
    let at = parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?;
    store
        .cancel_pass(super::CancelPass {
            attempt_id: "A1".to_owned(),
            sent: Vec::new(),
            end: crate::cancel::PassEnd::Empty,
            at,
            workspace: None,
        })
        .await?;
    let conn = Connection::open(&path)?;
    let row: (String, String) = conn.query_row(
        "SELECT a.state, e.state FROM attempts a, effects e WHERE a.attempt_id = 'A1' AND e.effect_key = 'A1/work'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(row, ("CANCELLED".to_owned(), "UNKNOWN".to_owned()));
    let keys: String = conn.query_row(
        "SELECT json_extract(payload_json, '$.effect_keys') FROM events WHERE kind = 'cancel.diagnostic' \
         AND json_extract(payload_json, '$.code') = 'UNKNOWN_EFFECT' AND json_extract(payload_json, '$.attempt_id') = 'A1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(keys, r#"["A1/work"]"#);
    Ok(())
}

/// [`Fake`]'s group (A1's, and `also`'s, which ends with it), whose workspace records are numbered: each look at the
/// workspace counted.
struct Counted {
    fake: Fake,
    also: Option<&'static str>,
    records: Arc<Mutex<u32>>,
}

impl Processes for Counted {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        if Some(attempt.attempt_id.as_str()) == self.also {
            let group: Box<dyn ProcessControl + Send> = Box::new(Group {
                clock: self.fake.clock.clone(),
                exits_after: 30,
                state: self.fake.state.clone(),
            });
            return Some((group, true));
        }
        self.fake.control(attempt)
    }
    fn workspace(&self) -> Value {
        let n = self
            .records
            .lock()
            .map(|mut n| {
                *n += 1;
                *n
            })
            .unwrap_or(0);
        json!({"workspace_sha256": format!("record-{n}")})
    }
}

#[tokio::test]
async fn a_cancelled_writer_lets_its_lease_go_with_its_workspace_record_which_the_run_reuses() -> R
{
    // REQ-019 behaviour 6 (docs/adr/ADR-024.md decision 9, M2T17R-52), the cancel's path: A1 is the writer lease's
    // holder. A pass that found its group empty without the record is refused: A1 stays RUNNING, its lease held.
    // The cancel's step takes the record once the group is empty and ends A1 CANCELLED with it, after its reap's
    // event; that pass is the step's last, so the run's record is the same one, taken once.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET writer = 1 WHERE task_id IN ('T1', 'T2');
         INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
           ('p1', 'writer', 'A1', 1, '2026-11-07T00:02:00Z');"
    ))?;
    store.submit_command(cancel_run(1)).await?;
    let refused = store
        .cancel_pass(super::CancelPass {
            attempt_id: "A1".to_owned(),
            sent: Vec::new(),
            end: crate::cancel::PassEnd::Empty,
            at: parse_rfc3339("2026-11-07T00:00:05Z").ok_or("time")?,
            workspace: None,
        })
        .await;
    assert!(
        matches!(
            refused,
            Err(crate::persistence::PersistError::WorkspaceRecordRequired(_))
        ),
        "{refused:?}"
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM attempts WHERE attempt_id = 'A1' AND state = 'RUNNING'"
        )?,
        1
    );
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM events WHERE kind IN ('cancel.reaped', 'attempt.workspace')"
        )?,
        0
    );
    let clock = Arc::new(FakeClock(Mutex::new(Duration::ZERO)));
    let advance = clock.clone();
    let wait: Arc<dyn Fn(Duration) + Send + Sync> = Arc::new(move |d| {
        if let Ok(mut at) = advance.0.lock() {
            *at += d;
        }
    });
    let records = Arc::new(Mutex::new(0));
    let processes: Arc<dyn Processes> = Arc::new(Counted {
        fake: Fake {
            clock: clock.clone(),
            state: Arc::new(Mutex::new((None, Vec::new()))),
        },
        also: None,
        records: records.clone(),
    });
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    let mut steps = 0;
    while canceller
        .step(&store, "R", processes.clone(), clock.clone(), wait.clone())
        .await?
        != Finished::Cancelled
    {
        steps += 1;
        if steps > 20 {
            return Err("never cancelled".into());
        }
    }
    let conn = Connection::open(&path)?;
    let (recorded, attempt): (i64, String) = conn.query_row(
        "SELECT seq, payload_json FROM events WHERE kind = 'attempt.workspace' \
         AND json_extract(payload_json, '$.attempt_id') = 'A1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let attempt: Value = serde_json::from_str(&attempt)?;
    assert_eq!(attempt["to"], "CANCELLED", "{attempt}");
    assert_eq!(attempt["workspace"]["workspace_sha256"], "record-1");
    // After the pass's steps (M2T17FR-51): the record follows the stop it was taken after.
    let reaped = count(&path, "SELECT seq FROM events WHERE kind = 'cancel.reaped'")?;
    assert!(recorded > reaped, "{recorded} {reaped}");
    let run: Value = serde_json::from_str(&conn.query_row(
        "SELECT payload_json FROM events WHERE kind = 'cancel.workspace'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    assert_eq!(run["workspace_sha256"], "record-1", "{run}");
    assert_eq!(records.lock().map(|n| *n).unwrap_or(0), 1);
    Ok(())
}

#[tokio::test]
async fn a_run_record_is_taken_again_when_a_pass_came_after_the_writers() -> R {
    // Decision 9 of docs/adr/ADR-024.md: the run's record reuses the writer's only when no pass came after the one
    // that took it. Here reader A3's group ends with A1's, its pass right after A1's in the same step: the run's record
    // is taken again, after both.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}
         UPDATE tasks SET writer = 1 WHERE task_id IN ('T1', 'T2');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
           ('T3', 'R', 's1', 'RUNNING', 3, 'qa', 3, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, started_at) VALUES
           ('A3', 'T3', 'RUNNING', 1, 1, 1, 4343, '2026-11-07T00:00:00Z');
         INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
           ('p1', 'writer', 'A1', 1, '2026-11-07T00:02:00Z'), ('p1', 'reader-1', 'A3', 1, '2026-11-07T00:02:00Z');"
    ))?;
    store.submit_command(cancel_run(1)).await?;
    let clock = Arc::new(FakeClock(Mutex::new(Duration::ZERO)));
    let advance = clock.clone();
    let wait: Arc<dyn Fn(Duration) + Send + Sync> = Arc::new(move |d| {
        if let Ok(mut at) = advance.0.lock() {
            *at += d;
        }
    });
    let records = Arc::new(Mutex::new(0));
    let processes: Arc<dyn Processes> = Arc::new(Counted {
        fake: Fake {
            clock: clock.clone(),
            state: Arc::new(Mutex::new((None, Vec::new()))),
        },
        also: Some("A3"),
        records: records.clone(),
    });
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    let mut steps = 0;
    while canceller
        .step(&store, "R", processes.clone(), clock.clone(), wait.clone())
        .await?
        != Finished::Cancelled
    {
        steps += 1;
        if steps > 20 {
            return Err("never cancelled".into());
        }
    }
    let conn = Connection::open(&path)?;
    let order: Vec<String> = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.attempt_id') FROM events WHERE kind = 'cancel.reaped' ORDER BY seq",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        order,
        ["A1", "A3"],
        "the reader's pass came after the writer's"
    );
    let attempt: Value = serde_json::from_str(&conn.query_row(
        "SELECT payload_json FROM events WHERE kind = 'attempt.workspace'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    assert_eq!(attempt["workspace"]["workspace_sha256"], "record-1");
    let run: Value = serde_json::from_str(&conn.query_row(
        "SELECT payload_json FROM events WHERE kind = 'cancel.workspace'",
        [],
        |r| r.get::<_, String>(0),
    )?)?;
    assert_eq!(run["workspace_sha256"], "record-2", "{run}");
    assert_eq!(records.lock().map(|n| *n).unwrap_or(0), 2);
    Ok(())
}
