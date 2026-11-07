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

use super::{HostHooks, ReconcileSettings, http::Shared};
use hoh_agents::identity::{self, StopStep};
use hoh_core::decision::Context;
use hoh_core::delivery::Clock;
use hoh_core::persistence::{Commit, DispatchNotice, NewEvent, PersistError, UnfinishedRow};
use hoh_core::reconcile::{self, Disposition, Effect, EffectState, Process, Unfinished, work_key};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Ask `work`'s process to stop the agent's own way (its native cancel), before any signal: the first step when its
    /// lease expired (AC-041-02).
    fn cancel(&self, _work: &Work) {}
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

/// How often the leases of the attempts it runs are renewed (REQ-041: every 30 s, for 120 s).
const RENEW: Duration = Duration::from_secs(30);

/// The attempts the runner runs: what each was given and how to stop its task.
#[derive(Default)]
struct Inflight(BTreeMap<String, (Work, Option<tokio::task::AbortHandle>)>);

/// What the runner keeps between ticks.
struct Runner {
    shared: Arc<Shared>,
    config: RunnerConfig,
    clock: Arc<dyn Clock>,
    settings: ReconcileSettings,
    inflight: Arc<Mutex<Inflight>>,
    attempts: tokio::task::JoinSet<()>,
    renewed: Instant,
    /// Lease holders whose process outlived their stop: left blocked, with one diagnostic, until the next host.
    given_up: BTreeSet<String>,
}

/// The runner of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    config: RunnerConfig,
    clock: Arc<dyn Clock>,
    settings: ReconcileSettings,
    mut stop: watch::Receiver<bool>,
) {
    let mut watermark = shared.store.watermark();
    let mut runner = Runner {
        shared,
        config,
        clock,
        settings,
        inflight: Arc::default(),
        attempts: tokio::task::JoinSet::new(),
        renewed: Instant::now(),
        given_up: BTreeSet::new(),
    };
    loop {
        if *stop.borrow() {
            break;
        }
        while runner.attempts.try_join_next().is_some() {}
        if let Err(e) = runner.tick().await {
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
    runner.attempts.abort_all();
    while runner.attempts.join_next().await.is_some() {}
}

impl Runner {
    async fn tick(&mut self) -> Result<(), PersistError> {
        let now = self.clock.now();
        let project_id = self.shared.project_id.clone();
        // Expired leases first, so the rounds below can grant their slots again (ADR-019 decision 7).
        for row in self.shared.store.reconcile_scan(&project_id, now).await? {
            if row.lease_expired && !self.given_up.contains(&row.attempt_id) {
                self.expire(row).await?;
            }
        }
        if self.renewed.elapsed() >= RENEW {
            let running: Vec<String> = self
                .inflight
                .lock()
                .map(|f| f.0.keys().cloned().collect())
                .unwrap_or_default();
            self.shared.store.renew_leases(running, now).await?;
            self.renewed = Instant::now();
        }
        let store = &self.shared.store;
        let waiting = store.runner_waiting(&project_id).await?;
        let context = Context {
            now: Some(now),
            ..self.config.context.clone()
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
            let work = Work {
                run_id: notice.run_id.clone(),
                task_id: notice.task_id.clone(),
                attempt_id: notice.attempt_id.clone(),
                role: notice.role.clone(),
                effect_key: work_key(&notice.attempt_id),
            };
            let fresh = self
                .inflight
                .lock()
                .map(|mut f| {
                    let fresh = !f.0.contains_key(&notice.attempt_id);
                    if fresh {
                        f.0.insert(notice.attempt_id.clone(), (work.clone(), None));
                    }
                    fresh
                })
                .unwrap_or(false);
            if !fresh {
                continue;
            }
            let shared = self.shared.clone();
            let executor = self.config.executor.clone();
            let inflight = self.inflight.clone();
            let id = notice.attempt_id.clone();
            let handle = self.attempts.spawn(async move {
                let id = notice.attempt_id.clone();
                if let Err(e) = attempt(
                    &shared,
                    executor.as_ref(),
                    shared.hooks.as_ref(),
                    work,
                    notice,
                )
                .await
                {
                    use std::io::Write as _;
                    let _ = writeln!(std::io::stderr(), "hoh host: attempt {id}: {e}");
                }
                if let Ok(mut f) = inflight.lock() {
                    f.0.remove(&id);
                }
            });
            if let Ok(mut f) = self.inflight.lock()
                && let Some(entry) = f.0.get_mut(&id)
            {
                entry.1 = Some(handle);
            }
        }
        Ok(())
    }

    /// One event of a lease's expiry, committed on its own so the steps keep their order (AC-041-02).
    async fn step(
        &self,
        row: &UnfinishedRow,
        name: &str,
        payload: Value,
    ) -> Result<(), PersistError> {
        self.shared
            .store
            .commit(Commit {
                events: vec![NewEvent {
                    project_id: row.project_id.clone(),
                    event_id: hoh_core::ids::child_id(
                        &row.attempt_id,
                        &format!("lease-expired/{}/{name}", row.lease_epoch.unwrap_or(0)),
                    ),
                    run_id: Some(row.run_id.clone()),
                    kind: format!("lease.{name}"),
                    payload,
                }],
                ..Commit::default()
            })
            .await
            .map(drop)
    }

    /// An attempt whose lease expired (REQ-041 b4): never judged dead by the expiry alone. Its own task here stops
    /// first (so nothing else writes for it), then: inspect its process, its native cancel, SIGTERM to its group,
    /// SIGKILL, the reap confirmed; then its disposition as at a reconcile, which lets the lease go only once it is
    /// ended. A process that outlives its stop moves nothing: its lease stays held, no writer lease is granted.
    async fn expire(&mut self, stale: UnfinishedRow) -> Result<(), PersistError> {
        let id = stale.attempt_id.clone();
        let work = self
            .inflight
            .lock()
            .ok()
            .and_then(|f| f.0.get(&id).cloned());
        if let Some((_, Some(handle))) = &work {
            handle.abort();
        }
        // Read it again now that nothing of this host writes for it.
        let now = self.clock.now();
        let Some(row) = self
            .shared
            .store
            .reconcile_scan(&self.shared.project_id, now)
            .await?
            .into_iter()
            .find(|r| r.attempt_id == id)
        else {
            return Ok(());
        };
        let record = (row.pid, row.boot_id.clone(), row.process_start_id.clone());
        let found = tokio::task::spawn_blocking(move || {
            identity::find(record.0, record.1.as_deref(), record.2.as_deref())
        })
        .await
        .map_err(|e| PersistError::Storage(format!("the lease's inspection ended: {e}")))?
        .map_err(|e| PersistError::Storage(format!("cannot look for the process of {id}: {e}")))?;
        self.step(
            &row,
            "inspected",
            json!({"attempt_id": id, "lease_epoch": row.lease_epoch, "found": match &found {
                Ok(_) => json!("ours"), Err(gone) => json!({"gone": gone.basis()}),
            }}),
        )
        .await?;
        let native = work.as_ref().map(|(w, _)| w.clone());
        if let Some(work) = &native {
            self.config.executor.cancel(work);
        }
        self.step(
            &row,
            "native_cancel",
            json!({"attempt_id": id, "sent": native.is_some()}),
        )
        .await?;
        let process = match found {
            Err(gone) => Process::Gone {
                basis: gone.basis().to_owned(),
            },
            Ok(ours) => {
                let (term, kill) = (self.settings.term_grace, self.settings.kill_grace);
                let stopped = tokio::task::spawn_blocking(move || {
                    // The native cancel's grace first: a process that ended by it gets no signal.
                    let end = Instant::now() + term;
                    while !ours.exited() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    if ours.exited()
                        && hoh_agents::process::group_members(ours.identity.pid).is_empty()
                    {
                        identity::Stopped {
                            steps: vec![StopStep::Reaped],
                            reaped: true,
                        }
                    } else {
                        ours.stop(term, kill)
                    }
                })
                .await
                .map_err(|e| PersistError::Storage(format!("the stop of {id} ended: {e}")))?;
                let mut names = Vec::new();
                for step in &stopped.steps {
                    let name = match step {
                        StopStep::Term => "sigterm",
                        StopStep::Kill => "sigkill",
                        StopStep::Reaped => "reaped",
                    };
                    self.step(&row, name, json!({"attempt_id": id})).await?;
                    names.push(name.to_owned());
                }
                Process::Stopped {
                    steps: names,
                    reaped: stopped.reaped,
                }
            }
        };
        let observer = self.settings.observer.clone();
        let effects = row
            .effects
            .iter()
            .map(|e| Effect {
                key: e.key.clone(),
                state: e.state,
                observation: (e.state == EffectState::Started)
                    .then(|| observer.observe(&id, &e.key)),
            })
            .collect();
        let disposition = reconcile::dispose(&Unfinished {
            attempt_id: id.clone(),
            state: row.state,
            task_state: row.task_state,
            process: process.clone(),
            effects,
            lease_expired: true,
        });
        if disposition == Disposition::Unreaped {
            self.given_up.insert(id.clone());
        }
        self.shared
            .store
            .reconcile_apply(row, disposition, process, "lease_expired")
            .await?;
        if let Ok(mut f) = self.inflight.lock() {
            f.0.remove(&id);
        }
        Ok(())
    }
}

/// One attempt from its dispatch to its end.
async fn attempt(
    shared: &Shared,
    executor: &dyn Executor,
    hooks: &dyn HostHooks,
    work: Work,
    notice: DispatchNotice,
) -> Result<(), PersistError> {
    let store = &shared.store;
    let key = work.effect_key.clone();
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
