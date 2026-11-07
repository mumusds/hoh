//! The host runner (docs/adr/ADR-019.md decision 8), the minimal loop that turns dispatches into work: it starts
//! after the reconcile, so every dispatch it makes or takes comes after `reconcile.completed`. Each tick it runs
//! scheduling rounds for the working runs (a round that selects nothing writes nothing), takes the run.started
//! notices, and hands every dispatch notice whose attempt is still PREPARED to the [`Executor`]:
//!
//! deliver (the executor's answer is the ACK) → receipt, attempt STARTING, work effect PREPARED → work effect STARTED
//! (write-ahead) → the process starts → attempt RUNNING with its identity → the process ends → work effect OBSERVED,
//! then COMMITTED → attempt SETTLED and task IMPLEMENTED (or FAILED and READY).
//!
//! A crash anywhere leaves what the reconcile needs: a dispatch not yet received stays PENDING with its attempt
//! PREPARED (delivered again); a receipt without a STARTED effect proves the work never began; a STARTED effect is
//! looked for, never done again. The product has no executor yet (agents are bound to roles with M2-T07), so the
//! product host runs no runner.

use super::{HostHooks, http::Shared};
use hoh_core::decision::Context;
use hoh_core::persistence::{DispatchNotice, PersistError};
use hoh_core::reconcile::{EffectState, work_key};
use serde_json::json;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

/// A process an executor started: its pid (the leader of its own process group) and how it ended.
#[derive(Debug)]
pub struct Started {
    pub pid: u32,
    pub ended: oneshot::Receiver<Outcome>,
}

/// Where the runner's work runs (a port: the agents of the roles come with M2-T07; tests pass their own).
pub trait Executor: Send + Sync + std::fmt::Debug {
    /// Hand `work` over; the answer says the executor has it (the ACK).
    fn deliver(&self, work: &Work) -> oneshot::Receiver<Result<(), String>>;
    /// Start `work`'s process; called once its work effect is STARTED.
    fn start(&self, work: &Work) -> Result<Started, String>;
}

/// The runner's fault injection points (CR-006): the product's hooks do nothing. In order, for one attempt: before
/// its dispatch is handed over, handed over but not answered, answered but its receipt not written, its work effect
/// STARTED and the process running (not yet OBSERVED), its work effect COMMITTED but its task not yet moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerPoint {
    BeforeSend,
    AfterSend,
    AfterAck,
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

/// The runner of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    config: RunnerConfig,
    mut stop: watch::Receiver<bool>,
) {
    let inflight: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let mut attempts = tokio::task::JoinSet::new();
    let mut watermark = shared.store.watermark();
    loop {
        if *stop.borrow() {
            break;
        }
        while attempts.try_join_next().is_some() {}
        if let Err(e) = tick(&shared, &config, &inflight, &mut attempts).await {
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
    attempts.abort_all();
    while attempts.join_next().await.is_some() {}
}

async fn tick(
    shared: &Arc<Shared>,
    config: &RunnerConfig,
    inflight: &Arc<Mutex<BTreeSet<String>>>,
    attempts: &mut tokio::task::JoinSet<()>,
) -> Result<(), PersistError> {
    let store = &shared.store;
    let waiting = store.runner_waiting(&shared.project_id).await?;
    for run in &waiting.runs {
        while store
            .schedule_round(run, config.context.clone())
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
        let fresh = inflight
            .lock()
            .map(|mut set| set.insert(notice.attempt_id.clone()))
            .unwrap_or(false);
        if !fresh {
            continue;
        }
        let shared = shared.clone();
        let executor = config.executor.clone();
        let inflight = inflight.clone();
        attempts.spawn(async move {
            let id = notice.attempt_id.clone();
            if let Err(e) = attempt(&shared, executor.as_ref(), shared.hooks.as_ref(), notice).await
            {
                use std::io::Write as _;
                let _ = writeln!(std::io::stderr(), "hoh host: attempt {id}: {e}");
            }
            if let Ok(mut set) = inflight.lock() {
                set.remove(&id);
            }
        });
    }
    Ok(())
}

/// One attempt from its dispatch to its end.
async fn attempt(
    shared: &Shared,
    executor: &dyn Executor,
    hooks: &dyn HostHooks,
    notice: DispatchNotice,
) -> Result<(), PersistError> {
    let store = &shared.store;
    let key = work_key(&notice.attempt_id);
    let work = Work {
        run_id: notice.run_id.clone(),
        task_id: notice.task_id.clone(),
        attempt_id: notice.attempt_id.clone(),
        role: notice.role.clone(),
        effect_key: key.clone(),
    };
    hooks.runner(RunnerPoint::BeforeSend);
    let ack = executor.deliver(&work);
    hooks.runner(RunnerPoint::AfterSend);
    if !matches!(ack.await, Ok(Ok(()))) {
        // Not taken: the notice stays PENDING and its attempt PREPARED, so it is delivered again.
        return Ok(());
    }
    hooks.runner(RunnerPoint::AfterAck);
    if !store.runner_received(notice.clone()).await? {
        return Ok(());
    }
    let id = &notice.attempt_id;
    // A step that no longer applies means the attempt was disposed of meanwhile (its lease expired): stop there.
    if !store
        .runner_effect(
            id,
            &key,
            EffectState::Prepared,
            EffectState::Started,
            json!({}),
        )
        .await?
    {
        return Ok(());
    }
    let outcome = match executor.start(&work) {
        Ok(started) => {
            store
                .runner_running(id, hoh_agents::identity::running(started.pid))
                .await?;
            hooks.runner(RunnerPoint::AfterEffectStarted);
            started.ended.await.unwrap_or_else(|_| Outcome {
                success: false,
                detail: "the executor lost the process".to_owned(),
            })
        }
        Err(detail) => Outcome {
            success: false,
            detail: format!("the process did not start: {detail}"),
        },
    };
    let observed = json!({"success": outcome.success, "detail": outcome.detail});
    if !store
        .runner_effect(
            id,
            &key,
            EffectState::Started,
            EffectState::Observed,
            observed,
        )
        .await?
        || !store
            .runner_effect(
                id,
                &key,
                EffectState::Observed,
                EffectState::Committed,
                json!({}),
            )
            .await?
    {
        return Ok(());
    }
    hooks.runner(RunnerPoint::AfterEffectCommitted);
    store
        .runner_finished(id, outcome.success, &outcome.detail)
        .await?;
    Ok(())
}
