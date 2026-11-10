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

use super::cgroup::{AttemptGroup, Tracking};
use super::processes::{Outside, Proof};
use super::{HostHooks, ReconcileSettings, http::Shared, reconcile};
use hoh_agents::identity;
use hoh_core::decision::Context;
use hoh_core::delivery::Clock;
use hoh_core::persistence::{
    Commit, DispatchNotice, Ending, NewEvent, PersistError, SessionStart, UnfinishedRow,
};
use hoh_core::reconcile::{
    Disposition, Effect, EffectState, Process, Unfinished, dispose, work_key,
};
use hoh_core::retry::{FailureClass, SplitMix64};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, watch};

/// What an executor is given for one attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Work {
    /// The project's id (unique: made once, at its first host's start).
    pub project_id: String,
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub role: String,
    /// The key of the attempt's own work effect: the run marker its process leaves (ADR-019 decision 6).
    pub effect_key: String,
    /// What it runs as: its binding's agent, model and session, the role's prompt, the run's goal and the task's title
    /// (docs/adr/ADR-022.md decisions 9 to 12; test executors need none of it).
    pub assignment: hoh_core::persistence::Assignment,
}

impl Work {
    /// The value of [`super::ATTEMPT_ENV`] for this attempt: its project's id and its own, so no other project's
    /// attempt of the same id shares it.
    pub fn marker(&self) -> String {
        marker(&self.project_id, &self.attempt_id)
    }
}

/// The marker of attempt `attempt_id` of project `project_id` (see [`Work::marker`]).
pub(super) fn marker(project_id: &str, attempt_id: &str) -> String {
    format!("{project_id}/{attempt_id}")
}

/// How an attempt's process ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    pub detail: String,
    /// Whether the executor saw the leader's process group empty: it sent the group a SIGKILL while the leader was an
    /// unreaped zombie (which holds the group's number, so the signal reaches only this group), reaped the leader,
    /// then waited until no process was left in the group (`kill(-pgid, 0)` gives ESRCH; the members hold the number
    /// meanwhile). The attempt ends, and its slot goes, only if so (M2T05R-101, -303).
    pub group_gone: bool,
    /// How it failed, when it did (ADR-020 decision 1): a 429/503 with its Retry-After, other transient failures,
    /// missing authentication or an incompatible CLI. Unused on success.
    pub class: FailureClass,
    /// What the agent's turn showed beyond its verdict (docs/adr/ADR-022.md decision 12); test executors give none.
    pub observed: hoh_core::persistence::Observed,
    /// The turn's verdict was UNKNOWN_EFFECT (it ended after starting an effect whose outcome it never reported): the
    /// attempt ends UNKNOWN_EFFECT, its task untouched and nothing run again, as a reconcile ends one (ADR-019 decision
    /// 5, M2T07R-200); its work effect is left STARTED, never recorded as a result.
    pub unknown_effect: bool,
}

/// The host's source of the retries' jitter draws (ADR-020 decision 2): one SplitMix64 for the reconcile and the
/// runner, seeded once at the start.
pub(super) struct Jitter(Mutex<SplitMix64>);

impl Jitter {
    pub(super) fn new(seed: u64) -> Jitter {
        Jitter(Mutex::new(SplitMix64::new(seed)))
    }

    /// The next uniform 32-bit draw.
    pub(super) fn draw(&self) -> u32 {
        self.0.lock().map_or(0, |mut g| g.next_u32())
    }
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

/// Why an executor started no process for a work (docs/adr/ADR-022.md decision 12): the attempt's failure reason (a
/// reason code for the product's, such as PROMPT_UNAVAILABLE or BLOCKED_CAPABILITY), how it failed, and what was found,
/// kept in an `attempt.start_refused` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub reason: String,
    pub class: FailureClass,
    pub detail: Option<String>,
}

/// A refusal nothing classified (a test executor's text): transient.
impl From<String> for Refusal {
    fn from(detail: String) -> Refusal {
        Refusal {
            reason: format!("the process did not start: {detail}"),
            class: FailureClass::Transient { retry_after: None },
            detail: None,
        }
    }
}

/// Where the runner's work runs (a port: the agents of the roles come with M2-T07; tests pass their own).
pub trait Executor: Send + Sync + std::fmt::Debug {
    /// The product's agent registry, when this executor runs the product's agents: the runner then binds sessions,
    /// holds back changes to unavailable agents and asks of each role what its contract needs (docs/adr/ADR-022.md
    /// decisions 8 to 10). Test executors have none.
    fn registry(&self) -> Option<Arc<super::agents::Registry>> {
        None
    }

    /// Hand `work` over; the answer says the executor has it (the ACK).
    fn deliver(&self, work: &Work) -> oneshot::Receiver<Result<(), String>>;
    /// Start `work`'s process held at its start gate: nothing of the agent may run before [`Started::gate`] opens.
    /// The leader's environment has [`super::ATTEMPT_ENV`] set to [`Work::marker`], which its descendants inherit: a
    /// cancel finds by it the attempt's processes that left its group (docs/adr/ADR-021.md decision 8d).
    /// When the leader ends, the executor SIGKILLs its process group while the leader is an unreaped zombie, reaps
    /// it, waits for the group to be empty and reports that in [`Outcome::group_gone`] (the hoh-agents substrate's
    /// order: group SIGKILL, reap, `group_gone`).
    fn start(&self, work: &Work) -> Result<Started, Refusal>;
    /// Give a role's new holder its first input, the checkpoint package of `start` (docs/adr/ADR-024.md decision 4),
    /// for `start`'s session: the answer says the executor has it. It may be asked again for the same notice (the
    /// answer lost in a crash) and takes it again without harm. The default takes none, so the notice waits.
    fn open_session(&self, _start: &SessionStart) -> oneshot::Receiver<Result<(), String>> {
        let (sent, answer) = oneshot::channel();
        let _ = sent.send(Err("this executor opens no sessions".to_owned()));
        answer
    }

    /// Ask `work`'s process to stop the agent's own way (its native cancel), before any signal: the first step when its
    /// lease expired (AC-041-02). It only asks and returns at once: the settle looks at the group around it
    /// (M2T05R-452). Whether it asked a turn it still holds: only then does the settle wait the native grace, and its
    /// step says the cancel was sent (docs/adr/ADR-022.md decision 17, M2T07R-453).
    fn cancel(&self, _work: &Work) -> bool {
        false
    }
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
    /// A settle of an attempt this host runs begins (before its task is stopped).
    Settling,
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

/// How long a tick waits for the executor to take a new holder's first input; past it the notice waits for the next.
const OPEN_SESSION: Duration = Duration::from_secs(10);

/// How long the host's stop waits for a probe running to end: its command's polling (10 ms) and its leader's reaping
/// (1 s at most).
const PROBE_STOP: Duration = Duration::from_secs(5);

/// How often the leases of the attempts it runs are renewed (REQ-041: every 30 s, for 120 s).
const RENEW: Duration = Duration::from_secs(30);

/// An attempt the runner runs: what it was given and its task.
pub(super) struct Entry {
    pub(super) work: Work,
    pub(super) task: Option<tokio::task::JoinHandle<()>>,
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
    jitter: Arc<Jitter>,
    inflight: super::control::Inflight,
    /// The session.start notices whose delivery failure was already said (once each).
    undelivered: std::sync::Mutex<std::collections::BTreeSet<String>>,
}

impl Common {
    /// A failure nothing classified (a process that did not start, an identity that could not be read): transient,
    /// at the runner's clock (ADR-020 decision 1).
    fn failed(&self, detail: String) -> Ending {
        Ending {
            success: false,
            detail,
            class: FailureClass::Transient { retry_after: None },
            at: self.clock.now(),
            draw: self.jitter.draw(),
            observed: hoh_core::persistence::Observed::default(),
            unknown_effect: false,
        }
    }
}

/// What the host gives its runner besides its project and configuration.
pub(super) struct Setup {
    pub(super) clock: Arc<dyn Clock>,
    pub(super) settings: ReconcileSettings,
    pub(super) host_epoch: i64,
    pub(super) jitter: Arc<Jitter>,
    /// Shared with the pause and cancel loop, which stops a cancelled run's attempts first.
    pub(super) inflight: super::control::Inflight,
}

/// The runner of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    config: RunnerConfig,
    setup: Setup,
    mut stop: watch::Receiver<bool>,
) {
    let Setup {
        clock,
        settings,
        host_epoch,
        jitter,
        inflight,
    } = setup;
    let mut watermark = shared.store.watermark();
    let common = Arc::new(Common {
        shared,
        executor: config.executor.clone(),
        settings,
        clock,
        host_epoch,
        passes: AtomicU64::new(0),
        jitter,
        inflight,
        undelivered: std::sync::Mutex::default(),
    });
    let registry = config.executor.registry();
    let watch = registry
        .clone()
        .map(|registry| tokio::spawn(registry_watch(registry, stop.clone())));
    let mut renewed = Instant::now();
    let mut held_back: BTreeMap<String, (Instant, String)> = BTreeMap::new();
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
    // The registry's probes are stopped: their commands are killed by the stop or by their own looks at it, so none
    // outlives the host but what a probing thread that is not run leaves (docs/adr/ADR-022.md decision 10 v6,
    // M2T07R-202, -853); its loop is waited for, within a bound.
    if let Some(registry) = &registry {
        registry.stop();
    }
    if let Some(mut watch) = watch
        && tokio::time::timeout(PROBE_STOP, &mut watch).await.is_err()
    {
        watch.abort();
    }
    // The host stops: the attempts in flight are left as they are (their processes run on); the next host's reconcile
    // settles them.
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

/// The registry's own loop (docs/adr/ADR-022.md decision 10): every tick it looks at the agents the runner wants, off
/// the async workers; a probe may take its while, but neither a tick nor the host's stop waits for it.
async fn registry_watch(registry: Arc<super::agents::Registry>, mut stop: watch::Receiver<bool>) {
    loop {
        let looking = registry.clone();
        // Not left behind on a stop: the registry's stop ends the probe, and the host waits for it (M2T07R-202).
        let _ = tokio::task::spawn_blocking(move || looking.refresh()).await;
        if *stop.borrow() {
            break;
        }
        tokio::select! {
            () = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => break,
        }
    }
}

async fn tick(
    common: &Arc<Common>,
    context: &Context,
    renewed: &mut Instant,
    held_back: &mut BTreeMap<String, (Instant, String)>,
) -> Result<(), PersistError> {
    let store = &common.shared.store;
    let project_id = common.shared.project_id.clone();
    let now = common.clock.now();
    // Expired leases first, so the rounds below can grant their slots again (ADR-019 decision 7).
    for row in store.reconcile_scan(&project_id, now).await? {
        let waiting = held_back
            .get(&row.attempt_id)
            .is_some_and(|(at, _)| at.elapsed() < common.settings.retry);
        // An attempt being cancelled is the cancel's to end (docs/adr/ADR-021.md decision 8c).
        if !row.lease_expired || waiting || row.cancel_requested {
            continue;
        }
        let id = row.attempt_id.clone();
        let entry = common.inflight.lock().ok().and_then(|mut f| f.remove(&id));
        let last = held_back.get(&id).map(|(_, seen)| seen.clone());
        match settle(common, &id, entry, "lease_expired", last.as_deref()).await? {
            Some((Disposition::Unreaped, seen)) => {
                held_back.insert(id, (Instant::now(), seen));
            }
            _ => {
                held_back.remove(&id);
            }
        }
    }
    if renewed.elapsed() >= RENEW {
        // Only attempts whose task still runs: one that ended (or failed) keeps no lease alive (M2T05R-102, -150).
        let running: Vec<String> = common
            .inflight
            .lock()
            .map(|f| {
                f.iter()
                    .filter(|(_, e)| e.task.as_ref().is_some_and(|t| !t.is_finished()))
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        store.renew_leases(running, now).await?;
        *renewed = Instant::now();
    }
    let waiting = store.runner_waiting(&project_id).await?;
    // The product's agents as the registry last saw them: it looks at the ones the team and the unbound bindings name
    // in its own task (registry_watch), so no probe holds up a tick (docs/adr/ADR-022.md decision 10).
    let registry = common.executor.registry();
    let availability = match &registry {
        Some(registry) => {
            let named: std::collections::BTreeSet<String> = waiting
                .agents
                .iter()
                .cloned()
                .chain(waiting.unbound.iter().map(|u| u.agent.clone()))
                .collect();
            registry.want(&named);
            registry.availability()
        }
        None => hoh_core::persistence::Availability::default(),
    };
    // The team changes whose checkpoint has come, in every run not ended that is behind the project's revision
    // (decision 8), before any pace or round: a change to an agent unavailable is refused, one to an agent not looked
    // at yet waits.
    for run in &waiting.team_behind {
        store.team_checkpoint(run, availability.clone()).await?;
    }
    // Sessions for the bindings waiting for one, and what each role of the team needs (decisions 9 and 10).
    let mut context = context.clone();
    if let Some(registry) = &registry {
        super::agents::bind(
            store,
            registry,
            waiting.unbound.clone(),
            &common.shared.root,
        )
        .await?;
        context.required_capabilities = waiting
            .roles
            .iter()
            .map(|(role, contract)| (role.clone(), super::agents::required(contract.as_deref())))
            .collect();
    }
    // The new holders' first inputs (docs/adr/ADR-024.md decision 4): a notice its binding has passed is superseded,
    // never delivered; one of the binding's own epoch goes to its session once the binder granted it, and is settled
    // when the executor has it (at least once: a crash before the settle gives it again).
    for start in &waiting.session_starts {
        if start.current_epoch > start.role_epoch {
            store.supersede_session_start(&start.message_id).await?;
            continue;
        }
        let Some(session) = &start.session_id else {
            continue;
        };
        let answer = tokio::time::timeout(OPEN_SESSION, common.executor.open_session(start)).await;
        let failure = match answer {
            Ok(Ok(Ok(()))) => {
                store
                    .deliver_session_start(&start.message_id, session)
                    .await?;
                continue;
            }
            Ok(Ok(Err(e))) => e,
            Ok(Err(_)) => "the executor dropped the request".to_owned(),
            Err(_) => format!("no answer within {OPEN_SESSION:?}"),
        };
        if common
            .undelivered
            .lock()
            .is_ok_and(|mut said| said.insert(start.message_id.clone()))
        {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: the {} session {session} did not take its first input ({}): {failure}",
                start.role,
                start.message_id
            );
        }
    }
    // The time taken afresh for each pace and each round, never one from before an awaited step (a settle, the
    // writer's queue): a round after the deadline dispatches nothing (M2T19R-04); with the team's capabilities.
    let at = || Context {
        now: Some(common.clock.now()),
        wall: Some(common.clock.wall()),
        ..context.clone()
    };
    for run in &waiting.runs {
        // The run as a whole first: woken when a retry is due, parked, blocked or stopped (ADR-020 decisions 4, 5, 7).
        store.pace_run(run, at()).await?;
        while store.schedule_round(run, at()).await?.dispatched.is_some() {}
    }
    for (message_id, _) in &waiting.started {
        store.ack_notice(message_id).await?;
    }
    // Dispatches made by these rounds are taken on the next tick (their commit wakes the loop).
    for notice in waiting.dispatches {
        let id = notice.attempt_id.clone();
        let work = Work {
            project_id: common.shared.project_id.clone(),
            run_id: notice.run_id.clone(),
            task_id: notice.task_id.clone(),
            attempt_id: id.clone(),
            role: notice.role.clone(),
            effect_key: work_key(&id),
            assignment: notice.assignment.clone(),
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
            let id = id.clone();
            async move {
                let result = attempt(&common, work.clone(), notice).await;
                // An error is settled at once, its entry still there so no tick takes it meanwhile (M2T05R-260);
                // however it ends, its entry then goes (no lease is renewed for it any more; a settle that failed is
                // left to its lease's expiry, M2T05R-102, -150).
                if let Err(e) = result {
                    use std::io::Write as _;
                    let _ = writeln!(std::io::stderr(), "hoh host: an attempt of the runner: {e}");
                    let entry = Entry { work, task: None };
                    if let Err(e) = settle(&common, &id, Some(entry), "runner_error", None).await {
                        let _ = writeln!(std::io::stderr(), "hoh host: the settle of {id}: {e}");
                    }
                }
                if let Ok(mut f) = common.inflight.lock() {
                    f.remove(&id);
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
    hooks.runner(RunnerPoint::BeforeSend);
    let ack = common.executor.deliver(&work);
    hooks.runner(RunnerPoint::AfterSend);
    if !matches!(ack.await, Ok(Ok(()))) {
        // Not taken: the notice stays PENDING and its attempt PREPARED, so it is delivered again.
        return Ok(());
    }
    hooks.runner(RunnerPoint::AfterAck);
    if !store
        .runner_received(notice.clone(), common.clock.now())
        .await?
    {
        return Ok(());
    }
    let key = work.effect_key.clone();
    let started = match common.executor.start(&work) {
        Ok(started) => started,
        Err(refusal) => {
            // Nothing ran: its work stays PREPARED, the attempt fails from STARTING by the refusal's class (its task
            // READY again for a transient one).
            let mut ending = common.failed(refusal.reason.clone());
            ending.class = refusal.class;
            if let Some(detail) = refusal.detail {
                ending.observed.events.push((
                    "start-refused".to_owned(),
                    "attempt.start_refused".to_owned(),
                    serde_json::json!({"attempt_id": id, "reason": refusal.reason, "detail": detail}),
                ));
            }
            store.runner_finished(&id, ending).await?;
            return Ok(());
        }
    };
    // Its whole identity: without it the gate stays shut (dropped below) and the agent never runs (M2T05R-100). Its
    // cgroup too, while it is held at the gate (nothing of the agent runs yet): what it starts stays in it unless it
    // moves itself out, which the marker scan covers (docs/adr/ADR-021.md decision 8d v5 to v7, CR-011, CR-012);
    // without a delegated cgroup it is tracked by the /proc scan.
    let pid = started.pid;
    let cgroups = common.shared.cgroups.clone();
    let marker = work.marker();
    let (who, tracking) = tokio::task::spawn_blocking(move || {
        let tracking = match cgroups {
            Some(delegated) => match AttemptGroup::adopt(&delegated.attempt(&marker), pid) {
                Ok(group) => Tracking::of_group(&group),
                Err(e) => Tracking::Proc(format!("its cgroup could not be made or joined: {e}")),
            },
            None => Tracking::Proc("the host has no delegated cgroup".to_owned()),
        };
        (identity::of(pid).ok().flatten(), tracking)
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the identity of {id}'s process: {e}")))?;
    let Some(who) = who else {
        drop(started);
        store
            .runner_finished(
                &id,
                common.failed("the identity of its process could not be read".to_owned()),
            )
            .await?;
        return Ok(());
    };
    hooks.runner(RunnerPoint::AfterSpawn);
    // Its tracking is recorded with its identity, in the same commit.
    if !store
        .runner_running(&id, identity::running(&who), tracking.json(&id))
        .await?
    {
        // Disposed of meanwhile: the gate is dropped unopened, so the process ends without running anything.
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
            None,
        )
        .await?;
        use std::io::Write as _;
        let _ = writeln!(
            std::io::stderr(),
            "hoh host: the start gate of {id}: {detail}"
        );
        return Ok(());
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
            None,
        )
        .await?;
        return Ok(());
    };
    // The class and the time it ended go with the outcome, so a host that crashes after OBSERVED retries it as it would
    // have, from the time it ended (ADR-020 decision 1, M2T19R-03).
    let ended = common.clock.now();
    let unknown = outcome.unknown_effect;
    let mut observed = json!({
        "success": outcome.success,
        "detail": outcome.detail,
        "ended_at": hoh_core::time::rfc3339_utc(ended),
    });
    if let (Some(observed), Value::Object(class)) =
        (observed.as_object_mut(), outcome.class.to_json())
    {
        observed.extend(class);
    }
    // An unknown effect has no result to record: its work effect stays STARTED, so a crash from here on settles it
    // UNKNOWN_EFFECT too.
    if !unknown
        && !store
            .runner_effect(
                &id,
                &key,
                EffectState::Started,
                EffectState::Observed,
                observed,
            )
            .await?
    {
        return Ok(());
    }
    // Its leader ended: the attempt ends, and its slot with it, only if the executor emptied its group first
    // (M2T05R-101, -202); else it stays RUNNING with its slot held, and a diagnostic says why.
    if !outcome.group_gone {
        store
            .commit(Commit {
                events: vec![NewEvent {
                    project_id: common.shared.project_id.clone(),
                    event_id: hoh_core::ids::child_id(&id, "runner/unreaped"),
                    run_id: Some(work.run_id.clone()),
                    kind: "runner.unreaped".to_owned(),
                    payload: json!({"attempt_id": id, "detail":
                        "its leader ended but its process group was not emptied; the attempt and its slot stay"}),
                }],
                ..Commit::default()
            })
            .await?;
        return Ok(());
    }
    // Its group is empty: its processes outside the group are stopped too, and it ends, its slot with it, only with the
    // termination proof (docs/adr/ADR-021.md decision 8d, M2T06R-100); else it stays RUNNING like an unreaped group.
    let marker = work.marker();
    let (term, kill) = (common.settings.term_grace, common.settings.kill_grace);
    let root = common.shared.hooks.proc_root();
    let group = tracking.cgroup();
    let (steps, proof) = tokio::task::spawn_blocking(move || {
        let mut outside = Outside::new(&marker).with_root(root);
        match group {
            // The cgroup, then what may have left it with its marker (decision 8d v6, M2T06R-400).
            Some(group) => {
                let (mut steps, in_cgroup) = group.stop(term, kill);
                let (left, outside) = outside.stop(term, kill);
                steps.extend(left);
                (steps, super::processes::both(in_cgroup, outside))
            }
            None => outside.stop(term, kill),
        }
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the stop outside the group of {id} ended: {e}")))?;
    if !steps.is_empty() || proof != Proof::Done {
        let (name, detail) = if proof == Proof::Done {
            (
                "runner/outside",
                "processes of it outside its group were stopped",
            )
        } else {
            (
                "runner/unreaped",
                "its group was emptied but processes of it outside the group live on or could not be read; the \
                 attempt and its slot stay",
            )
        };
        store
            .commit(Commit {
                events: vec![NewEvent {
                    project_id: common.shared.project_id.clone(),
                    event_id: hoh_core::ids::child_id(&id, name),
                    run_id: Some(work.run_id.clone()),
                    kind: name.replace('/', "."),
                    payload: json!({"attempt_id": id, "steps": steps, "detail": detail}),
                }],
                ..Commit::default()
            })
            .await?;
        if proof != Proof::Done {
            return Ok(());
        }
    }
    if !unknown
        && !store
            .runner_effect(
                &id,
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
        .runner_finished(
            &id,
            Ending {
                success: outcome.success,
                detail: outcome.detail.clone(),
                class: outcome.class,
                at: ended,
                draw: common.jitter.draw(),
                observed: outcome.observed.clone(),
                unknown_effect: unknown,
            },
        )
        .await?;
    Ok(())
}

/// One step event of a settle. A pass's steps are committed together, in their order (AC-041-02), after its stop
/// (M2T05R-452): a pass whose commit fails leaves no record, and its id is the pass's own, so the next pass does not
/// collide with it (M2T05R-06).
fn step(row: &UnfinishedRow, prefix: &str, pass: &str, name: &str, payload: Value) -> NewEvent {
    NewEvent {
        project_id: row.project_id.clone(),
        event_id: hoh_core::ids::child_id(&row.attempt_id, &format!("{pass}/{name}")),
        run_id: Some(row.run_id.clone()),
        kind: format!("{prefix}.{name}"),
        payload,
    }
}

/// Settle `id`, which this host can no longer trust to end by itself: its lease expired (`lease_expired`), its
/// executor lost its process (`executor_lost`), or its task failed (`runner_error`). Never judged ended by that alone
/// (REQ-041 b4): its own task here is aborted and waited for first (so nothing else of this host writes for it,
/// M2T05R-03), its row read again, its process inspected by its record, its native cancel sent, SIGTERM and SIGKILL
/// to its group and its group seen empty (all in one go, the step events then committed together in their order);
/// then its disposition as at a reconcile,
/// which lets its lease go only once it is ended. A process that may still run (outlived its stop, or cannot be told
/// apart) moves nothing and keeps its slot; when that finding is `last`'s again, nothing is written again
/// (M2T05R-157). The disposition applied, if any, with the finding.
async fn settle(
    common: &Common,
    id: &str,
    entry: Option<Entry>,
    cause: &'static str,
    last: Option<&str>,
) -> Result<Option<(Disposition, String)>, PersistError> {
    common.shared.hooks.runner(RunnerPoint::Settling);
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
    // From the inspection to the end of the stop in one go, with no write in between, so its group is looked at
    // throughout (M2T05R-452); the steps are written after it, in their order.
    let scanned = row.clone();
    let settings = common.settings.clone();
    let attempt = id.to_owned();
    let tracking = Tracking::of(
        super::http::read(&common.shared, move |reader| {
            reader.attempt_event("attempt.tracking", &attempt)
        })
        .await?
        .as_ref(),
    );
    let executor = common.executor.clone();
    let repeated = last.map(str::to_owned);
    let stop = tokio::task::spawn_blocking(move || {
        // A record of another world is one finding, whatever it names here: written once, not at every retry
        // (M2T06R-1054); the end keeps it uncertain.
        let seen = if matches!(tracking, Tracking::Foreign(_)) {
            reconcile::Seen::Uncertain("recorded_in_another_world".to_owned())
        } else {
            reconcile::inspect(&scanned)
        };
        let found = seen.json();
        let finding = found.to_string();
        if matches!(seen, reconcile::Seen::Uncertain(_))
            && repeated.as_deref() == Some(finding.as_str())
        {
            return None;
        }
        let sent = work.as_ref().is_some_and(|work| executor.cancel(work));
        // The native cancel's grace first, when one reached a turn: a group that emptied by it gets no signal
        // (M2T07R-254, -353, -453).
        seen.grace(settings.first_wait(sent));
        Some((
            found,
            finding,
            sent,
            reconcile::end(seen, &scanned, &tracking, &settings),
        ))
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the stop of {id} ended: {e}")))?;
    let Some((found, finding, cancelled, process)) = stop else {
        return Ok(Some((
            Disposition::Unreaped,
            last.unwrap_or_default().to_owned(),
        )));
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
    let mut events = vec![
        step(
            &row,
            prefix,
            &pass,
            "inspected",
            json!({"attempt_id": id, "cause": cause, "lease_epoch": row.lease_epoch, "found": found}),
        ),
        step(
            &row,
            prefix,
            &pass,
            "native_cancel",
            json!({"attempt_id": id, "sent": cancelled}),
        ),
    ];
    if let Process::Stopped { steps, .. } = &process {
        for name in steps {
            events.push(step(&row, prefix, &pass, name, json!({"attempt_id": id})));
        }
    }
    store
        .commit(Commit {
            events,
            ..Commit::default()
        })
        .await?;
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
        lease_expired: cause == "lease_expired" || row.lease_expired,
        work_outcome: row.work_outcome,
    });
    store
        .reconcile_apply(
            row,
            disposition.clone(),
            process,
            cause,
            common.clock.now(),
            common.jitter.draw(),
        )
        .await?;
    Ok(Some((disposition, finding)))
}
