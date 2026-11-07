//! The host runner (docs/adr/ADR-019.md decision 8), the minimal loop that turns dispatches into work: it starts
//! after the reconcile, so every dispatch it makes or takes comes after `reconcile.completed`. Each tick it settles
//! attempts whose lease expired, renews the leases of its attempts, runs scheduling rounds for the working runs (a
//! round that selects nothing writes nothing), takes the run.started notices, and hands every dispatch notice whose
//! attempt is still PREPARED to the [`Executor`]:
//!
//! deliver (the executor's answer is the ACK) → receipt, attempt STARTING, work effect PREPARED → the process starts
//! held at its start gate → attempt RUNNING with its identity and work effect STARTED, in one transaction → the gate
//! opens → the process ends → work effect OBSERVED with the outcome, then COMMITTED → attempt SETTLED and task
//! IMPLEMENTED (or FAILED and READY).
//!
//! A crash anywhere leaves what the reconcile needs: a dispatch not yet received stays PENDING with its attempt
//! PREPARED (delivered again); no recorded identity means the gate never opened and the agent never ran; a STARTED
//! effect is looked for, never done again. The product has no executor yet (agents are bound to roles with M2-T07),
//! so the product host runs no runner.

use super::{HostHooks, ReconcileSettings, http::Shared, reconcile};
use hoh_agents::identity::{self, Presence};
use hoh_core::decision::Context;
use hoh_core::delivery::Clock;
use hoh_core::persistence::{Commit, DispatchNotice, NewEvent, PersistError, UnfinishedRow};
use hoh_core::reconcile::{
    Disposition, Effect, EffectState, Process, Unfinished, dispose, work_key,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

/// What an executor is given for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    /// The key of the attempt's own work effect: the run marker its process leaves (ADR-019 decision 6).
    pub effect_key: String,
}

/// How an attempt's process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    pub detail: String,
}

/// A started process's start gate: until it is opened the process runs nothing of the agent; dropped unopened, the
/// process ends without running it (M2T05R-01, -52).
pub struct Gate(Box<dyn FnOnce() -> Result<(), String> + Send>);

impl Gate {
    pub fn new(open: impl FnOnce() -> Result<(), String> + Send + 'static) -> Gate {
        Gate(Box::new(open))
    }

    fn open(self) -> Result<(), String> {
        (self.0)()
    }
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Gate")
    }
}

/// A process an executor started: its pid (the leader of its own process group), held at its start gate, and how it
/// ended (a dropped sender means the executor lost it: its end is not known).
#[derive(Debug)]
pub struct Started {
    pub pid: u32,
    pub gate: Gate,
    pub ended: oneshot::Receiver<Outcome>,
}

/// Where the runner's work runs (a port: the agents of the roles come with M2-T07; tests pass their own).
pub trait Executor: Send + Sync + std::fmt::Debug {
    /// Hand `work` over; the answer says the executor has it (the ACK).
    fn deliver(&self, work: &Work) -> oneshot::Receiver<Result<(), String>>;
    /// Start `work`'s process held at its start gate: nothing of the agent may run before [`Started::gate`] opens.
    fn start(&self, work: &Work) -> Result<Started, String>;
    /// Ask `work`'s process to stop the agent's own way (its native cancel), before any signal: the first step when its
    /// lease expired (AC-041-02).
    fn cancel(&self, _work: &Work) {}
}

/// The runner's fault injection points (CR-006): the product's hooks do nothing. In order, for one attempt: before
/// its dispatch is handed over, handed over but not answered, answered but its receipt not written, its process
/// started at its gate but its identity not recorded, its work effect STARTED and the process running (not yet
/// OBSERVED), its work effect COMMITTED but its task not yet moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerPoint {
    BeforeSend,
    AfterSend,
    AfterAck,
    AfterSpawn,
    AfterEffectStarted,
    AfterEffectCommitted,
}

/// A host's runner: where its work runs and what its rounds judge by (the roles' required capabilities, the reader
/// slots; the team configuration sets them with M2-T07).
#[derive(Clone, Debug)]
pub struct RunnerConfig {
    pub executor: Arc<dyn Executor>,
    pub context: Context,
}

/// How often the runner looks again when nothing woke it.
const TICK: Duration = Duration::from_millis(200);

/// How often the leases of the attempts it runs are renewed (REQ-041: every 30 s, for 120 s).
const RENEW: Duration = Duration::from_secs(30);

/// How long a holder whose process could not be confirmed gone is left before it is looked at again.
const RETRY: Duration = Duration::from_secs(60);

/// An attempt the runner runs: what it was given and its task.
struct Entry {
    work: Work,
    task: Option<tokio::task::JoinHandle<()>>,
}

/// What the attempts' tasks and the loop share.
struct Common {
    shared: Arc<Shared>,
    executor: Arc<dyn Executor>,
    settings: ReconcileSettings,
    clock: Arc<dyn Clock>,
    host_epoch: i64,
    /// The number of the next settle pass (event ids of this host: `<cause>/<host epoch>/<pass>/…`).
    passes: AtomicU64,
    inflight: Mutex<BTreeMap<String, Entry>>,
}

/// The runner of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    config: RunnerConfig,
    clock: Arc<dyn Clock>,
    settings: ReconcileSettings,
    host_epoch: i64,
    mut stop: watch::Receiver<bool>,
) {
    let mut watermark = shared.store.watermark();
    let common = Arc::new(Common {
        shared,
        executor: config.executor.clone(),
        settings,
        clock,
        host_epoch,
        passes: AtomicU64::new(0),
        inflight: Mutex::new(BTreeMap::new()),
    });
    let mut renewed = Instant::now();
    let mut held_back: BTreeMap<String, Instant> = BTreeMap::new();
    loop {
        if *stop.borrow() {
            break;
        }
        if let Err(e) = tick(&common, &config.context, &mut renewed, &mut held_back).await {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: the runner: {e}");
        }
        tokio::select! {
            _ = watermark.changed() => {}
            () = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => break,
        }
    }
    // The host stops: the attempts in flight are left as they are (their processes run on); the next host's reconcile
    // settles them (pausing and cancelling them is M2-T06's).
    let tasks: Vec<_> = common
        .inflight
        .lock()
        .map(|mut f| f.values_mut().filter_map(|e| e.task.take()).collect())
        .unwrap_or_default();
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
}

async fn tick(
    common: &Arc<Common>,
    context: &Context,
    renewed: &mut Instant,
    held_back: &mut BTreeMap<String, Instant>,
) -> Result<(), PersistError> {
    let store = &common.shared.store;
    let project_id = common.shared.project_id.clone();
    let now = common.clock.now();
    // Expired leases first, so the rounds below can grant their slots again (ADR-019 decision 7).
    for row in store.reconcile_scan(&project_id, now).await? {
        if !row.lease_expired
            || held_back
                .get(&row.attempt_id)
                .is_some_and(|at| at.elapsed() < RETRY)
        {
            continue;
        }
        let id = row.attempt_id.clone();
        let entry = common.inflight.lock().ok().and_then(|mut f| f.remove(&id));
        let disposition = settle(common, &id, entry, "lease_expired").await?;
        if disposition == Some(Disposition::Unreaped) {
            held_back.insert(id, Instant::now());
        } else {
            held_back.remove(&id);
        }
    }
    if renewed.elapsed() >= RENEW {
        let running: Vec<String> = common
            .inflight
            .lock()
            .map(|f| f.keys().cloned().collect())
            .unwrap_or_default();
        store.renew_leases(running, now).await?;
        *renewed = Instant::now();
    }
    let waiting = store.runner_waiting(&project_id).await?;
    let context = Context {
        now: Some(now),
        ..context.clone()
    };
    for run in &waiting.runs {
        while store
            .schedule_round(run, context.clone())
            .await?
            .dispatched
            .is_some()
        {}
    }
    for (message_id, _) in &waiting.started {
        store.ack_notice(message_id).await?;
    }
    // Dispatches made by these rounds are taken on the next tick (their commit wakes the loop).
    for notice in waiting.dispatches {
        let id = notice.attempt_id.clone();
        let work = Work {
            run_id: notice.run_id.clone(),
            task_id: notice.task_id.clone(),
            attempt_id: id.clone(),
            role: notice.role.clone(),
            effect_key: work_key(&id),
        };
        let Ok(mut inflight) = common.inflight.lock() else {
            continue;
        };
        if inflight.contains_key(&id) {
            continue;
        }
        let task = tokio::spawn({
            let common = common.clone();
            let work = work.clone();
            async move {
                if let Err(e) = attempt(&common, work, notice).await {
                    use std::io::Write as _;
                    let _ = writeln!(std::io::stderr(), "hoh host: an attempt of the runner: {e}");
                }
            }
        });
        inflight.insert(
            id,
            Entry {
                work,
                task: Some(task),
            },
        );
    }
    Ok(())
}

/// One attempt from its dispatch to its end.
async fn attempt(
    common: &Arc<Common>,
    work: Work,
    notice: DispatchNotice,
) -> Result<(), PersistError> {
    let store = &common.shared.store;
    let hooks: &dyn HostHooks = common.shared.hooks.as_ref();
    let id = notice.attempt_id.clone();
    let done = |common: &Arc<Common>| {
        if let Ok(mut f) = common.inflight.lock() {
            f.remove(&id);
        }
    };
    hooks.runner(RunnerPoint::BeforeSend);
    let ack = common.executor.deliver(&work);
    hooks.runner(RunnerPoint::AfterSend);
    if !matches!(ack.await, Ok(Ok(()))) {
        // Not taken: the notice stays PENDING and its attempt PREPARED, so it is delivered again.
        done(common);
        return Ok(());
    }
    hooks.runner(RunnerPoint::AfterAck);
    if !store.runner_received(notice.clone()).await? {
        done(common);
        return Ok(());
    }
    let key = work.effect_key.clone();
    let started = match common.executor.start(&work) {
        Ok(started) => started,
        Err(detail) => {
            // Nothing ran: its work stays PREPARED, the attempt fails from STARTING and its task is READY again.
            store
                .runner_finished(&id, false, &format!("the process did not start: {detail}"))
                .await?;
            done(common);
            return Ok(());
        }
    };
    hooks.runner(RunnerPoint::AfterSpawn);
    if !store
        .runner_running(&id, hoh_agents::identity::running(started.pid))
        .await?
    {
        // Disposed of meanwhile: the gate is dropped unopened, so the process ends without running anything.
        done(common);
        return Ok(());
    }
    if let Err(detail) = started.gate.open() {
        // The gate cannot be opened (the process is gone): what happened is the settle's to find out.
        let entry = common.inflight.lock().ok().and_then(|mut f| f.remove(&id));
        settle(
            common,
            &id,
            entry.map(|e| Entry { task: None, ..e }),
            "executor_lost",
        )
        .await?;
        return Err(PersistError::Storage(format!(
            "the start gate of {id}: {detail}"
        )));
    }
    hooks.runner(RunnerPoint::AfterEffectStarted);
    let Ok(outcome) = started.ended.await else {
        // The executor lost the process: its end is not known, so it is settled as an expired lease would be, never
        // taken as a failure to run again (M2T05R-04).
        let entry = common.inflight.lock().ok().and_then(|mut f| f.remove(&id));
        settle(
            common,
            &id,
            entry.map(|e| Entry { task: None, ..e }),
            "executor_lost",
        )
        .await?;
        return Ok(());
    };
    let observed = json!({"success": outcome.success, "detail": outcome.detail});
    if !store
        .runner_effect(
            &id,
            &key,
            EffectState::Started,
            EffectState::Observed,
            observed,
        )
        .await?
        || !store
            .runner_effect(
                &id,
                &key,
                EffectState::Observed,
                EffectState::Committed,
                json!({}),
            )
            .await?
    {
        done(common);
        return Ok(());
    }
    hooks.runner(RunnerPoint::AfterEffectCommitted);
    store
        .runner_finished(&id, outcome.success, &outcome.detail)
        .await?;
    done(common);
    Ok(())
}

/// One step event of a settle, committed on its own so the steps keep their order (AC-041-02); its id is this pass's
/// own, so a pass cut short is followed by another without colliding (M2T05R-06).
async fn step(
    common: &Common,
    row: &UnfinishedRow,
    prefix: &str,
    pass: &str,
    name: &str,
    payload: Value,
) -> Result<(), PersistError> {
    common
        .shared
        .store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: row.project_id.clone(),
                event_id: hoh_core::ids::child_id(&row.attempt_id, &format!("{pass}/{name}")),
                run_id: Some(row.run_id.clone()),
                kind: format!("{prefix}.{name}"),
                payload,
            }],
            ..Commit::default()
        })
        .await
        .map(drop)
}

/// Settle `id`, which this host can no longer trust to end by itself: its lease expired (`lease_expired`), or its
/// executor lost its process (`executor_lost`). Never judged ended by that alone (REQ-041 b4): its own task here is
/// aborted and waited for first (so nothing else of this host writes for it, M2T05R-03), its row read again, its
/// process inspected, its native cancel sent, SIGTERM and SIGKILL to its group and the reap confirmed; then its
/// disposition as at a reconcile, which lets its lease go only once it is ended. A process that may still run
/// (outlived its stop, or cannot be told apart) moves nothing and keeps its slot. The disposition applied, if any.
async fn settle(
    common: &Common,
    id: &str,
    entry: Option<Entry>,
    cause: &'static str,
) -> Result<Option<Disposition>, PersistError> {
    let (work, task) = match entry {
        Some(Entry { work, task }) => (Some(work), task),
        None => (None, None),
    };
    if let Some(task) = task {
        task.abort();
        let _ = task.await;
    }
    let store = &common.shared.store;
    // Read it again now that nothing of this host writes for it (this read queues behind any write its task sent).
    let Some(row) = store
        .reconcile_scan(&common.shared.project_id, common.clock.now())
        .await?
        .into_iter()
        .find(|r| r.attempt_id == id)
    else {
        return Ok(None);
    };
    let prefix = if cause == "lease_expired" {
        "lease"
    } else {
        "runner"
    };
    let pass = format!(
        "{cause}/{}/{}",
        common.host_epoch,
        common.passes.fetch_add(1, Ordering::SeqCst)
    );
    let record = (row.pid, row.boot_id.clone(), row.process_start_id.clone());
    let found = tokio::task::spawn_blocking(move || {
        identity::presence(record.0, record.1.as_deref(), record.2.as_deref())
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the inspection of {id} ended: {e}")))?
    .map_err(|e| PersistError::Storage(format!("cannot look for the process of {id}: {e}")))?;
    let seen = match &found {
        Presence::Ours(_) => json!("ours"),
        Presence::Leaderless(left) => {
            json!({"uncertain": "leader_gone", "members": left.members.len()})
        }
        Presence::Gone(gone) => json!({"gone": gone.basis()}),
    };
    step(
        common,
        &row,
        prefix,
        &pass,
        "inspected",
        json!({"attempt_id": id, "cause": cause, "lease_epoch": row.lease_epoch, "found": seen}),
    )
    .await?;
    if let Some(work) = &work {
        common.executor.cancel(work);
    }
    step(
        common,
        &row,
        prefix,
        &pass,
        "native_cancel",
        json!({"attempt_id": id, "sent": work.is_some()}),
    )
    .await?;
    let process = match found {
        Presence::Gone(identity::Gone::NoPid)
            if row.state == hoh_core::lifecycle::AttemptState::Running =>
        {
            Process::Uncertain {
                basis: "running_without_a_process_record".to_owned(),
            }
        }
        Presence::Gone(gone) => Process::Gone {
            basis: gone.basis().to_owned(),
        },
        Presence::Leaderless(left) => Process::Uncertain {
            basis: format!("leader_gone_{}_left_in_its_group", left.members.len()),
        },
        Presence::Ours(ours) => {
            let (term, kill) = (common.settings.term_grace, common.settings.kill_grace);
            let stopped = tokio::task::spawn_blocking(move || {
                // The native cancel's grace first: a process that ended by it gets no signal.
                let end = Instant::now() + term;
                while !ours.exited() && Instant::now() < end {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if ours.exited() && hoh_agents::process::group_members(ours.identity.pid).is_empty()
                {
                    identity::Stopped {
                        steps: vec![identity::StopStep::Reaped],
                        reaped: true,
                    }
                } else {
                    ours.stop(term, kill)
                }
            })
            .await
            .map_err(|e| PersistError::Storage(format!("the stop of {id} ended: {e}")))?;
            let process = reconcile::stopped(&stopped);
            if let Process::Stopped { steps, .. } = &process {
                for name in steps {
                    step(common, &row, prefix, &pass, name, json!({"attempt_id": id})).await?;
                }
            }
            process
        }
    };
    let observer = common.settings.observer.clone();
    let effects = row
        .effects
        .iter()
        .map(|e| Effect {
            key: e.key.clone(),
            state: e.state,
            observation: (e.state == EffectState::Started).then(|| observer.observe(id, &e.key)),
        })
        .collect();
    let disposition = dispose(&Unfinished {
        attempt_id: id.to_owned(),
        state: row.state,
        task_state: row.task_state,
        process: process.clone(),
        effects,
        lease_expired: true,
        work_outcome: row.work_outcome,
    });
    store
        .reconcile_apply(row, disposition.clone(), process, cause)
        .await?;
    Ok(Some(disposition))
}
