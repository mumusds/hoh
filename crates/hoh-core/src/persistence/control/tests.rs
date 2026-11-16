//! TEST-043-05 and the cancel's store writes against a real store: a fake process group, a FakeClock.

use crate::cancel::{Canceller, GRACES, Look, ProcessControl, Processes, Signal};
use crate::delivery::Clock;
use crate::lifecycle::{self, Lifecycle as _};
use crate::persistence::{
    AttemptRow, CommandOutcome, Finished, Intent, NewEvent, NewOutbox, Options, RunTarget, Store,
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
            intent: Intent::Progress,
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
            Finished::Recovering => return Err("no record is pending here".into()),
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

/// [`Fake`]'s processes, whose SIGTERM leaves part of the attempt's cgroup to the SIGKILL step (CGW-50).
struct Unwalked(Fake);

/// A control whose every SIGTERM may have missed some of the attempt's processes.
struct Partial(Box<dyn ProcessControl + Send>);

impl ProcessControl for Partial {
    fn look(&mut self) -> Look {
        self.0.look()
    }
    fn send(&mut self, signal: Signal) -> bool {
        self.0.send(signal)
    }
    fn sigterm_incomplete(&mut self) -> bool {
        true
    }
}

impl Processes for Unwalked {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        self.0.control(attempt).map(|(group, native)| {
            let partial: Box<dyn ProcessControl + Send> = Box::new(Partial(group));
            (partial, native)
        })
    }
    fn workspace(&self) -> Value {
        self.0.workspace()
    }
}

#[tokio::test]
async fn a_cancels_sigterm_that_missed_part_of_the_cgroup_is_recorded_and_sigkill_follows() -> R {
    // CGW-50 (docs/adr/ADR-021.md decision 8d): on the cancel ladder, a SIGTERM whose cgroup walk left part of the
    // subtree (not read, or its budget spent) is recorded as a cancel.diagnostic CGROUP_SIGTERM_INCOMPLETE with the
    // pass that sent it, once; SIGKILL follows at 15 all the same and the run is CANCELLED once the group exits.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    store.submit_command(cancel_run(1)).await?;
    let clock = Arc::new(FakeClock(Mutex::new(Duration::ZERO)));
    let advance = clock.clone();
    let wait: Arc<dyn Fn(Duration) + Send + Sync> = Arc::new(move |d| {
        if let Ok(mut at) = advance.0.lock() {
            *at += d;
        }
    });
    let state = Arc::new(Mutex::new((None, Vec::new())));
    let processes: Arc<dyn Processes> = Arc::new(Unwalked(Fake {
        clock: clock.clone(),
        state: state.clone(),
    }));
    let canceller = Canceller::new(GRACES, Duration::from_secs(1));
    let mut steps = 0;
    while canceller
        .step(&store, "R", processes.clone(), clock.clone(), wait.clone())
        .await?
        != Finished::Cancelled
    {
        steps += 1;
        if steps > 20 {
            return Err("never ended".into());
        }
    }
    let killed = state.lock().map(|s| s.0).unwrap_or(None);
    assert_eq!(killed, Some(15), "SIGKILL at 15");
    let kinds: Vec<(String, Option<String>)> = {
        let conn = Connection::open(&path)?;
        let mut statement = conn.prepare(
            "SELECT kind, json_extract(payload_json, '$.code') FROM events WHERE (kind IN ('cancel.sigterm', \
             'cancel.diagnostic', 'run.cancelled') OR (kind = 'cancel.sigkill' AND seq = (SELECT min(seq) FROM events \
             WHERE kind = 'cancel.sigkill'))) AND json_extract(payload_json, '$.code') IS NOT 'UNKNOWN_EFFECT' \
             ORDER BY seq",
        )?;
        statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?
    };
    let kind = |k: &str, code: Option<&str>| (k.to_owned(), code.map(str::to_owned));
    assert_eq!(
        kinds,
        [
            kind("cancel.sigterm", None),
            kind("cancel.sigkill", None),
            kind("cancel.diagnostic", Some("CGROUP_SIGTERM_INCOMPLETE")),
            kind("run.cancelled", None),
        ]
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
        .finish_cancel(
            "R",
            json!({"workspace_sha256": "fake"}),
            std::collections::BTreeSet::new(),
            SystemTime::now(),
        )
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
            intent: Intent::Progress,
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
async fn a_resume_records_the_runs_reconcile_before_it_works_again_and_only_from_a_stop_state() -> R
{
    // REQ-043 behaviour 2 (docs/adr/ADR-021.md decision 7), REQ-036 decision 2 (docs/adr/ADR-031.md decision 16): a run
    // in a stop state (PAUSED, INCOMPLETE, WAITING_EXTERNAL, BLOCKED; here PAUSED) resumes to the state it stopped in,
    // its reconcile recorded first, and no retrigger with it; a run in no stop state (RUNNING) does not resume.
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

/// A store whose clock is `clock`: the host's, which a ResumeRun's stop is judged by (docs/adr/ADR-031.md decision
/// 16), with `faults`.
fn open_at(
    dir: &tempfile::TempDir,
    clock: Arc<FakeClock>,
    faults: Arc<dyn crate::persistence::FaultHooks>,
) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        clock: Some(crate::persistence::StoreClock(clock)),
        faults,
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

/// The clock at 00:00:00 plus `secs`.
fn clock_at(secs: u64) -> Arc<FakeClock> {
    Arc::new(FakeClock(Mutex::new(Duration::from_secs(secs))))
}

/// T1's A1 ended: nothing of R runs.
const QUIET: &str = "UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-07T00:00:05Z' WHERE attempt_id = 'A1';";

/// R moved by the store's writer from `revision` to `to`, in one commit: a stop's run.result is published by it, as a
/// Commander's transition_run publishes one (no budget stop: decision 16's Commander-decided INCOMPLETE).
async fn move_r(store: &Store, revision: i64, to: RunState) -> R {
    store
        .commit(crate::persistence::Commit {
            cas: vec![crate::persistence::StateCas {
                id: "R".to_owned(),
                expected_revision: revision,
                change: crate::persistence::Change::Run(to),
            }],
            ..crate::persistence::Commit::default()
        })
        .await?;
    Ok(())
}

/// What a refused command leaves as it was: R's row, every event and every command.
fn written(path: &Path) -> R<(String, i64, Option<String>, i64, i64, i64)> {
    let (state, revision, resume) = run_state(path)?;
    Ok((
        state,
        revision,
        resume,
        count(path, "SELECT coalesce(max(seq), 0) FROM events")?,
        count(path, "SELECT count(*) FROM events")?,
        count(path, "SELECT count(*) FROM commands")?,
    ))
}

/// A ResumeRun refused as ILLEGAL_TRANSITION: the state it names as `from`, why, and its details.
fn refused(
    result: Result<CommandOutcome, crate::persistence::PersistError>,
) -> R<(String, String, Option<Value>)> {
    match result {
        Err(crate::persistence::PersistError::IllegalTransition {
            from, why, details, ..
        }) => Ok((from, why, details.map(|d| *d))),
        other => Err(format!("not refused as ILLEGAL_TRANSITION: {other:?}").into()),
    }
}

/// R's events after seq `after`: kind and payload.
fn events_after(path: &Path, after: i64) -> R<Vec<(String, Value)>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT kind, payload_json FROM events WHERE run_id = 'R' AND seq > ?1 ORDER BY seq",
    )?;
    let rows = statement
        .query_map([after], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(kind, payload)| Ok((kind, serde_json::from_str(&payload)?)))
        .collect()
}

/// The points each request of the writer reached, by job, in order.
#[derive(Debug, Default)]
struct Points(Mutex<Vec<(&'static str, crate::persistence::WritePoint)>>);

impl crate::persistence::FaultHooks for Points {
    fn at(&self, job: &'static str, point: crate::persistence::WritePoint) {
        if let Ok(mut points) = self.0.lock() {
            points.push((job, point));
        }
    }
}

#[tokio::test]
async fn a_resume_from_paused_or_a_commander_decided_incomplete_moves_first_and_returns_to_its_resume_state()
-> R {
    // docs/adr/ADR-031.md decision 16 (REQ-036 decision 2, CR-023): neither stop has a stop of its own (PAUSED holds
    // only by the user's request; an INCOMPLETE the Commander decided names no budget item), so ResumeRun is accepted:
    // its commit moves R first (its compare-and-set before its events), then writes `reconcile.run{from}` and the
    // accepted event, as R's events though the INCOMPLETE was published; no retrigger (the stall detector carries on,
    // E12), no run.result; R is in its resume state.
    for from in [RunState::Paused, RunState::Incomplete] {
        let dir = tempfile::tempdir()?;
        let points = Arc::new(Points::default());
        let (store, path) = open_at(&dir, clock_at(0), points.clone())?;
        Connection::open(&path)?.execute_batch(&format!("{WORLD}{QUIET}"))?;
        if from == RunState::Paused {
            move_r(&store, 1, RunState::PauseRequested).await?;
            move_r(&store, 2, RunState::Paused).await?;
        } else {
            move_r(&store, 1, RunState::Incomplete).await?;
        }
        let revision = run_state(&path)?.1;
        let latest: String = Connection::open(&path)?.query_row(
            "SELECT kind || '/' || json_extract(payload_json, '$.state') FROM events WHERE run_id = 'R' \
             ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(latest, format!("run.result/{}", from.as_str()), "published");
        let before = count(&path, "SELECT coalesce(max(seq), 0) FROM events")?;
        if let Ok(mut p) = points.0.lock() {
            p.clear();
        }
        let resumed = store
            .submit_command(command("ResumeRun", "resume", revision, RunTarget::Resume))
            .await?;
        assert!(
            matches!(resumed, CommandOutcome::Accepted { revision: r, .. } if r == revision + 1),
            "{from:?}: {resumed:?}"
        );
        assert_eq!(
            run_state(&path)?,
            ("RUNNING".to_owned(), revision + 1, None),
            "{from:?}"
        );
        let events = events_after(&path, before)?;
        let kinds: Vec<&str> = events.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(kinds, ["reconcile.run", "run.resumerun"], "{from:?}");
        assert_eq!(events[0].1["from"], from.as_str(), "{from:?}");
        assert_eq!(
            count(
                &path,
                &format!(
                    "SELECT count(*) FROM events WHERE seq > {before} AND (run_id IS NULL OR run_id <> 'R')"
                )
            )?,
            0,
            "{from:?}: nothing else written"
        );
        let command: Vec<crate::persistence::WritePoint> = points
            .0
            .lock()
            .map_err(|_| "points")?
            .iter()
            .filter(|(job, _)| *job == "Command")
            .map(|(_, p)| *p)
            .collect();
        let at = |p| command.iter().position(|q| *q == p);
        let (Some(moved), Some(written)) = (
            at(crate::persistence::WritePoint::StateWritten),
            at(crate::persistence::WritePoint::EventsWritten),
        ) else {
            return Err(format!("{from:?}: the command's points: {command:?}").into());
        };
        assert!(
            moved < written,
            "{from:?}: the move comes before the events: {command:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_resume_of_an_incomplete_run_with_blockers_claimed_is_blocked_until_they_are_released()
-> R {
    // docs/adr/ADR-031.md decision 16 and its edge INCOMPLETE -> BLOCKED: R BLOCKED by a claim, then INCOMPLETE (a
    // stop entered from BLOCKED keeps the claims and the resume state). ResumeRun: R BLOCKED, its events
    // reconcile.run{from: INCOMPLETE}, the accepted event, run.blocked with the claimed reasons and run.result{BLOCKED}
    // last; its resume state RUNNING kept. Once the claim is released (the pace's own, as nothing stalls), R goes back
    // to RUNNING.
    let clock = clock_at(0);
    let dir = tempfile::tempdir()?;
    let (store, path) = open_at(&dir, clock.clone(), Arc::new(crate::persistence::NoFaults))?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}{QUIET}
         UPDATE runs SET state = 'BLOCKED', resume_state = 'RUNNING', blocked_reason = 'NEED_DECISION',
           blocked_reasons_json = '[\"NEED_DECISION\"]' WHERE run_id = 'R';
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'claim-1', 'R', 'run.blocker_claimed', '{{\"reason\": \"NEED_DECISION\", \"source\": \"pace\"}}',
            '2026-11-07T00:00:00Z');"
    ))?;
    move_r(&store, 1, RunState::Incomplete).await?;
    assert_eq!(
        run_state(&path)?,
        ("INCOMPLETE".to_owned(), 2, Some("RUNNING".to_owned()))
    );
    let before = count(&path, "SELECT coalesce(max(seq), 0) FROM events")?;
    let resumed = store
        .submit_command(command("ResumeRun", "resume", 2, RunTarget::Resume))
        .await?;
    assert!(
        matches!(resumed, CommandOutcome::Accepted { revision: 3, .. }),
        "{resumed:?}"
    );
    assert_eq!(
        run_state(&path)?,
        ("BLOCKED".to_owned(), 3, Some("RUNNING".to_owned()))
    );
    let events = events_after(&path, before)?;
    let kinds: Vec<&str> = events.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "reconcile.run",
            "run.resumerun",
            "run.blocked",
            "run.result"
        ]
    );
    assert_eq!(events[0].1["from"], "INCOMPLETE");
    assert_eq!(events[2].1["reasons"], json!(["NEED_DECISION"]));
    let result =
        crate::persistence::results::assert_ends_with_result(&path, before, "R", "BLOCKED")?;
    assert_eq!(result["blocked_reason"], "NEED_DECISION");
    let context = crate::decision::Context {
        now: Some(clock.now()),
        wall: Some(clock.now()),
        ..crate::decision::Context::default()
    };
    let paced = store.pace_run("R", context).await?;
    assert!(paced.unblocked, "{paced:?}");
    assert_eq!(run_state(&path)?, ("RUNNING".to_owned(), 4, None));
    Ok(())
}

#[tokio::test]
async fn a_resume_of_an_incomplete_run_whose_budget_stop_still_holds_is_refused_and_writes_nothing()
-> R {
    // docs/adr/ADR-031.md decision 16: INCOMPLETE's stop is the pace's own predicate, budget::stop of the run's budget
    // at the host's clock. Iterations used up, or the deadline passed on the FakeClock: 409 ILLEGAL_TRANSITION with
    // details {state: INCOMPLETE, stop} (the stop as run.incomplete names it); R's row, the events and the commands
    // unchanged. The same run before its deadline (the clock earlier) has no stop: accepted, back to RUNNING.
    for (variant, seed, secs, stop) in [
        (
            "iterations",
            "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES ('run', 'R', 'iterations', 3, 3);",
            0,
            Some("max_iterations"),
        ),
        (
            "deadline passed",
            "UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';",
            61,
            Some("deadline_at"),
        ),
        (
            "deadline to come",
            "UPDATE runs SET deadline_at = '2026-11-07T00:01:00Z' WHERE run_id = 'R';",
            30,
            None,
        ),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open_at(&dir, clock_at(secs), Arc::new(crate::persistence::NoFaults))?;
        Connection::open(&path)?.execute_batch(&format!("{WORLD}{QUIET}{seed}"))?;
        move_r(&store, 1, RunState::Incomplete).await?;
        let before = written(&path)?;
        let result = store
            .submit_command(command("ResumeRun", "resume", 2, RunTarget::Resume))
            .await;
        match stop {
            Some(stop) => {
                let (from, _, details) = refused(result)?;
                assert_eq!(from, "INCOMPLETE", "{variant}");
                assert_eq!(
                    details,
                    Some(json!({"state": "INCOMPLETE", "stop": stop})),
                    "{variant}"
                );
                assert_eq!(written(&path)?, before, "{variant}: nothing written");
            }
            None => {
                assert!(
                    matches!(result, Ok(CommandOutcome::Accepted { revision: 3, .. })),
                    "{variant}: {result:?}"
                );
                assert_eq!(run_state(&path)?, ("RUNNING".to_owned(), 3, None));
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn a_resume_of_a_waiting_run_is_refused_until_its_earliest_retry_is_due() -> R {
    // docs/adr/ADR-031.md decision 16: WAITING_EXTERNAL's stop is the pace's own (no task due and one waiting for its
    // next_retry_at, REQ-042 behaviour 2). T2 waits until 00:02:00, T3 until 00:05:00: at 00:00:00 ResumeRun is 409
    // ILLEGAL_TRANSITION with details {state: WAITING_EXTERNAL, next_retry_at: the earliest}, nothing written; with the
    // FakeClock past it, accepted and back to RUNNING.
    let clock = clock_at(0);
    let dir = tempfile::tempdir()?;
    let (store, path) = open_at(&dir, clock.clone(), Arc::new(crate::persistence::NoFaults))?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}{QUIET}
         UPDATE runs SET state = 'WAITING_EXTERNAL', resume_state = 'RUNNING', revision = 4 WHERE run_id = 'R';
         UPDATE tasks SET next_retry_at = '2026-11-07T00:02:00Z' WHERE task_id = 'T2';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate, next_retry_at)
           VALUES ('T3', 'R', 's1', 'READY', 1, 'developer', 3, 1, '2026-11-07T00:05:00Z');"
    ))?;
    let before = written(&path)?;
    let (from, _, details) = refused(
        store
            .submit_command(command("ResumeRun", "resume-early", 4, RunTarget::Resume))
            .await,
    )?;
    assert_eq!(from, "WAITING_EXTERNAL");
    assert_eq!(
        details,
        Some(json!({"state": "WAITING_EXTERNAL", "next_retry_at": "2026-11-07T00:02:00Z"}))
    );
    assert_eq!(written(&path)?, before, "nothing written");
    if let Ok(mut at) = clock.0.lock() {
        *at = Duration::from_secs(121);
    }
    let resumed = store
        .submit_command(command("ResumeRun", "resume", 4, RunTarget::Resume))
        .await?;
    assert!(
        matches!(resumed, CommandOutcome::Accepted { revision: 5, .. }),
        "{resumed:?}"
    );
    assert_eq!(run_state(&path)?, ("RUNNING".to_owned(), 5, None));
    let from: String = Connection::open(&path)?.query_row(
        "SELECT json_extract(payload_json, '$.from') FROM events WHERE kind = 'reconcile.run'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(from, "WAITING_EXTERNAL");
    Ok(())
}

#[tokio::test]
async fn a_resume_of_a_blocked_run_or_one_with_attempts_unfinished_or_in_another_state_is_refused()
-> R {
    // docs/adr/ADR-031.md decision 16: BLOCKED's stop is its blocker set, never empty in BLOCKED: 409 with details
    // {state: BLOCKED, reasons}. A stop state with an attempt not finished is refused by the reconcile first, its
    // `from` that state (not PAUSED). Any other state (RUNNING, COMPLETED) keeps its 409 with details {state}. Nothing is
    // written by any of them.
    let blocked = "UPDATE runs SET blocked_reason = 'BLOCKED_AUTH', \
                   blocked_reasons_json = '[\"BLOCKED_AUTH\", \"NEED_DECISION\"]' WHERE run_id = 'R';";
    let cases = [
        (
            "BLOCKED",
            QUIET,
            Some(json!({"state": "BLOCKED", "reasons": ["BLOCKED_AUTH", "NEED_DECISION"]})),
        ),
        ("PAUSED", "", None),
        ("INCOMPLETE", "", None),
        ("WAITING_EXTERNAL", "", None),
        ("BLOCKED", "", None),
        ("RUNNING", QUIET, Some(json!({"state": "RUNNING"}))),
        ("COMPLETED", QUIET, Some(json!({"state": "COMPLETED"}))),
    ];
    for (state, quiet, want) in cases {
        let dir = tempfile::tempdir()?;
        let (store, path) = open_at(&dir, clock_at(0), Arc::new(crate::persistence::NoFaults))?;
        let resume = if state == "RUNNING" || state == "COMPLETED" {
            "NULL"
        } else {
            "'RUNNING'"
        };
        Connection::open(&path)?.execute_batch(&format!(
            "{WORLD}{quiet}{}
             UPDATE runs SET state = '{state}', resume_state = {resume}, revision = 4 WHERE run_id = 'R';",
            if state == "BLOCKED" { blocked } else { "" }
        ))?;
        let before = written(&path)?;
        let (from, why, details) = refused(
            store
                .submit_command(command("ResumeRun", "resume", 4, RunTarget::Resume))
                .await,
        )?;
        let case = format!(
            "{state} {}",
            if quiet.is_empty() {
                "A1 running"
            } else {
                "quiet"
            }
        );
        assert_eq!(from, state, "{case}");
        assert_eq!(details, want, "{case}");
        if quiet.is_empty() {
            assert!(why.contains("A1"), "{case}: {why}");
        }
        assert_eq!(written(&path)?, before, "{case}: nothing written");
    }
    Ok(())
}

#[tokio::test]
async fn a_started_run_is_created_with_its_exact_accepted_body_and_replayed_byte_for_byte() -> R {
    // REQ-005 behaviour 1, decision 5 (docs/adr/ADR-021.md decision 5): StartRun creates the run the host named,
    // CREATED at revision 0, in the command's transaction, and stores the 202 body a replay returns. The project has an
    // approved specification, which the run adopts (docs/adr/ADR-031.md decision 1; with none it is BLOCKED at
    // revision 1, persistence/adoption/tests.rs).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-07T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);",
    )?;
    let start = |key: &str, revision: i64| SubmitCommand {
        run_id: "R-new".to_owned(),
        event: NewEvent {
            intent: Intent::Progress,
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
                policy: "balanced".to_owned(),
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
            .finish_cancel(
                "R",
                json!({"workspace_sha256": "fake"}),
                std::collections::BTreeSet::new(),
                SystemTime::now(),
            )
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
            term_incomplete: false,
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
            term_incomplete: false,
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

#[tokio::test]
async fn a_pause_a_cancel_and_a_resume_into_blocked_each_end_their_commit_with_a_run_result() -> R {
    // docs/adr/ADR-031.md decision 3, the control paths: pause_step to PAUSED, finish_cancel to CANCELLED, and a
    // ResumeRun with blockers claimed to BLOCKED each end their transaction with the run's run.result.
    let ended = "UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-07T00:00:05Z' WHERE attempt_id = 'A1';";
    let max = |path: &Path| count(path, "SELECT coalesce(max(seq), 0) FROM events");
    // PAUSED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!("{WORLD}{ended}"))?;
    store
        .submit_command(command(
            "PauseRun",
            "pause",
            1,
            RunTarget::To(RunState::PauseRequested),
        ))
        .await?;
    let before = max(&path)?;
    let at = parse_rfc3339("2026-11-07T00:00:10Z").ok_or("time")?;
    assert_eq!(
        store.pause_step("R", at).await?,
        crate::persistence::PauseStep::Paused
    );
    let paused =
        crate::persistence::results::assert_ends_with_result(&path, before, "R", "PAUSED")?;
    assert_eq!(paused["exit_code"], 6);
    // CANCELLED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!("{WORLD}{ended}"))?;
    store.submit_command(cancel_run(1)).await?;
    let before = max(&path)?;
    let finished = store
        .finish_cancel(
            "R",
            json!({"workspace_sha256": "fake"}),
            std::collections::BTreeSet::new(),
            SystemTime::now(),
        )
        .await?;
    assert_eq!(finished, Finished::Cancelled);
    let cancelled =
        crate::persistence::results::assert_ends_with_result(&path, before, "R", "CANCELLED")?;
    assert_eq!(cancelled["exit_code"], 5);
    // A resume with blockers claimed: BLOCKED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!(
        "{WORLD}{ended}
         UPDATE runs SET state = 'PAUSED', resume_state = 'RUNNING', revision = 4, blocked_reason = 'NEED_DECISION',
           blocked_reasons_json = '[\"NEED_DECISION\"]' WHERE run_id = 'R';"
    ))?;
    let before = max(&path)?;
    store
        .submit_command(command("ResumeRun", "resume", 4, RunTarget::Resume))
        .await?;
    let blocked =
        crate::persistence::results::assert_ends_with_result(&path, before, "R", "BLOCKED")?;
    assert_eq!(
        (&blocked["exit_code"], &blocked["blocked_reason"]),
        (&json!(3), &json!("NEED_DECISION"))
    );
    Ok(())
}

/// R cancelling with nothing running: T1's A1 ended; round rd1 open on C1 with its verifier tasks Q and V, whose
/// attempts ended CANCELLED, and their handoffs DELIVERED.
const OPEN_ROUND: &str = "
UPDATE attempts SET state = 'CANCELLED', ended_at = '2026-11-07T00:00:05Z' WHERE attempt_id = 'A1';
UPDATE runs SET state = 'CANCEL_REQUESTED', revision = 2, resume_state = 'RUNNING' WHERE run_id = 'R';
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, created_seq, cost_estimate) VALUES
  ('Q', 'R', 's1', 'RUNNING', 2, 'qa', 3, 1), ('V', 'R', 's1', 'RUNNING', 2, 'reviewer', 4, 1);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at, ended_at) VALUES
  ('QA1', 'Q', 'CANCELLED', 1, 1, 1, '2026-11-07T00:00:01Z', '2026-11-07T00:00:05Z'),
  ('RV1', 'V', 'CANCELLED', 1, 1, 1, '2026-11-07T00:00:01Z', '2026-11-07T00:00:05Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'rd1/opened', 'R', 'verification.round_opened',
   '{\"round_id\": \"rd1\", \"candidate_id\": \"C1\", \"tasks\": {\"qa\": \"Q\", \"reviewer\": \"V\"}, \"source_task_ids\": [\"T1\"]}',
   '2026-11-07T00:00:00Z');
INSERT INTO artifacts(artifact_id, sha256, path, bytes, mime) VALUES
  ('pkg', printf('%064d', 9), 'package.json', 1, 'application/json');
INSERT INTO handoffs(handoff_id, run_id, from_role, to_role, role_epoch, package_artifact_id, state, created_at) VALUES
  ('rd1/verify/qa', 'R', 'runtime', 'qa', 1, 'pkg', 'DELIVERED', '2026-11-07T00:00:00Z'),
  ('rd1/verify/reviewer', 'R', 'runtime', 'reviewer', 1, 'pkg', 'DELIVERED', '2026-11-07T00:00:00Z');
";

#[tokio::test]
async fn a_cancel_closes_its_open_rounds_superseded_in_its_own_last_transaction() -> R {
    // docs/adr/ADR-031.md decision 4 (M2T13D-302): the cancel's last transaction cancels the tasks, closes every open
    // round as superseded and rejects its handoffs, then writes run.cancelled, and its run.result last; nothing of the
    // run is written after it. A round whose candidate's records are still pending, not superseded, holds it back.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    Connection::open(&path)?.execute_batch(&format!("{WORLD}{OPEN_ROUND}"))?;
    let before = count(&path, "SELECT max(seq) FROM events")?;
    // C1's check records still pending and C1 not superseded: the round waits for them, so nothing is written and the
    // run stays cancelling.
    let waiting = store
        .finish_cancel(
            "R",
            json!({"workspace_sha256": "fake"}),
            ["C1".to_owned()].into(),
            SystemTime::now(),
        )
        .await?;
    assert_eq!(waiting, Finished::Recovering);
    assert_eq!(count(&path, "SELECT max(seq) FROM events")?, before);
    assert_eq!(run_state(&path)?.0, "CANCEL_REQUESTED");
    assert_eq!(
        count(
            &path,
            "SELECT count(*) FROM tasks WHERE state = 'CANCELLED'"
        )?,
        0
    );
    // Recorded since: the next cancel step ends it.
    let finished = store
        .finish_cancel(
            "R",
            json!({"workspace_sha256": "fake"}),
            std::collections::BTreeSet::new(),
            SystemTime::now(),
        )
        .await?;
    assert_eq!(finished, Finished::Cancelled);
    let kinds: Vec<(i64, String, Option<String>)> = {
        let conn = Connection::open(&path)?;
        let mut statement = conn.prepare(
            "SELECT seq, kind, json_extract(payload_json, '$.verdict') FROM events WHERE seq > ?1 ORDER BY seq",
        )?;
        statement
            .query_map([before], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?
    };
    let at = |kind: &str| kinds.iter().position(|(_, k, _)| k == kind);
    let (closed, cancelled, result) = (
        at("verification.round_closed"),
        at("run.cancelled"),
        at("run.result"),
    );
    assert!(
        closed.is_some() && closed < cancelled && cancelled < result,
        "{kinds:?}"
    );
    assert_eq!(result, Some(kinds.len() - 1), "{kinds:?}");
    assert_eq!(
        closed.and_then(|i| kinds[i].2.clone()).as_deref(),
        Some("superseded")
    );
    let states: (String, String, String, String) = Connection::open(&path)?.query_row(
        "SELECT (SELECT group_concat(DISTINCT state) FROM tasks WHERE task_id IN ('Q', 'V')), \
         (SELECT group_concat(DISTINCT state) FROM handoffs), (SELECT state FROM runs WHERE run_id = 'R'), \
         (SELECT kind FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    assert_eq!(
        states,
        (
            "CANCELLED".to_owned(),
            "REJECTED".to_owned(),
            "CANCELLED".to_owned(),
            "run.result".to_owned()
        )
    );
    Ok(())
}
