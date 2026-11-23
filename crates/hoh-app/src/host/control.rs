//! Pause and cancel on the host (REQ-043, docs/adr/ADR-021.md decisions 7 and 8): a loop that runs whether or not
//! the host has an executor. A pausing run is moved to PAUSED at its safe point; a cancelling run gets a task of its
//! own that stops what this host runs for it, escalates over its attempts' process groups pass after pass, and once
//! nothing of it runs records the workspace and cancels it. Neither touches the Commander (REQ-043 behaviour 7).

use super::cgroup::{AttemptGroup, Tracking};
use super::http::Shared;
use super::processes::{Outside, Proof};
use super::runner::{Entry, Executor, Work};
use hoh_agents::identity::{self, Presence};
use hoh_core::cancel::{Canceller, Look, ProcessControl, Processes, Signal};
use hoh_core::delivery::Clock;
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptRow, Finished, PersistError};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

/// The attempts this host runs (the runner's, shared so a cancel can stop them first).
pub(super) type Inflight = Arc<Mutex<BTreeMap<String, Entry>>>;

/// How often the loop looks again when nothing woke it.
const TICK: Duration = Duration::from_millis(200);

/// How often a cancel pass looks at a group at least (a group seen held must be looked at again within
/// `identity::HELD_FOR` after its leader's reap to stay signalable).
const LOOK_EVERY: Duration = Duration::from_millis(10);

/// The pause between two cancel steps of a run whose group still lives or was lost track of.
const STEP_AGAIN: Duration = Duration::from_secs(1);

/// What the loop and its cancel tasks share.
struct Common {
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    project: PathBuf,
    canceller: Canceller,
    cancelling: Mutex<BTreeSet<String>>,
    /// The runs whose baseline tests this host runs now, each with its stop: set by the run's cancel or pause, the
    /// command is killed with its group (docs/adr/ADR-039.md decision 8, DC-T07 (2)).
    testing: Mutex<BTreeMap<String, Arc<AtomicBool>>>,
    /// Set when the host stops: a baseline test command running then is killed with its group.
    stopping: Arc<AtomicBool>,
}

/// How long a recorded baseline test group a settle killed may take to empty.
const BASELINE_KILL_GRACE: Duration = Duration::from_secs(5);

/// The owner of a run's baseline test commands on this host (DC-T07): it records their processes through the store
/// from the blocking thread that runs them.
struct BaselineOwner {
    common: Arc<Common>,
    run: String,
    stop: Arc<AtomicBool>,
    runtime: tokio::runtime::Handle,
}

impl super::discovery::TestOwner for BaselineOwner {
    fn stopping(&self) -> bool {
        self.common.stopping.load(Ordering::SeqCst) || self.stop.load(Ordering::SeqCst)
    }

    fn started(
        &self,
        index: usize,
        nonce: &str,
        command: &str,
        identity: &identity::Identity,
        log: &str,
    ) -> Result<Option<String>, String> {
        let payload = json!({"index": index, "nonce": nonce, "command": command, "pid": identity.pid,
            "boot_id": identity.boot_id, "process_start_id": identity.start_id, "log": log});
        self.runtime
            .block_on(
                self.common
                    .shared
                    .store
                    .baseline_process_started(&self.run, payload),
            )
            .map_err(|e| format!("{command}: its process could not be recorded: {e}"))
    }

    fn ended(&self, process: &str) {
        if let Err(e) = self
            .runtime
            .block_on(self.common.shared.store.baseline_process_ended(
                &self.run,
                process,
                json!({"settled_by": "host"}),
            ))
        {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: the end of baseline process {process}: {e}"
            );
        }
    }
}

/// Settle the project's recorded baseline test processes not known to have ended, but those of the runs this host
/// tests now (docs/adr/ADR-039.md decision 8, DC-T07 (3)): one whose whole recorded identity still matches is killed
/// with its group (SIGKILL) and its group must be seen empty; one gone is recorded ended; one whose presence cannot be
/// told (its leader gone with members left, an incomplete record, a failed look) gets no signal and stays open. Its
/// log is kept as it is.
async fn settle_baseline(common: &Arc<Common>) {
    use std::io::Write as _;
    let project = common.shared.project_id.clone();
    let unsettled = match super::http::read(&common.shared, move |reader| {
        reader.unsettled_baseline_processes(&project)
    })
    .await
    {
        Ok(unsettled) => unsettled,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "hoh host: baseline processes: {e}");
            return;
        }
    };
    let testing: BTreeSet<String> = common
        .testing
        .lock()
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();
    for process in unsettled {
        if testing.contains(&process.run_id) {
            continue;
        }
        let (pid, boot, start) = (
            process.pid,
            process.boot_id.clone(),
            process.process_start_id.clone(),
        );
        let seen = super::begun::spawn_blocking(move || {
            match identity::presence(pid, boot.as_deref(), start.as_deref()) {
                Ok(Presence::Ours(found)) => {
                    found.kill();
                    (found.wait(BASELINE_KILL_GRACE) == identity::Group::Empty)
                        .then(|| json!({"ours": "killed"}))
                }
                Ok(Presence::Gone(gone)) => Some(json!({"gone": gone.basis()})),
                Ok(Presence::Leaderless(_) | Presence::Unverifiable) | Err(_) => None,
            }
        })
        .await;
        let Ok(Some(seen)) = seen else {
            continue;
        };
        if let Err(e) = common
            .shared
            .store
            .baseline_process_ended(
                &process.run_id,
                &process.process_id,
                json!({"settled_by": "reconcile", "seen": seen, "partial": true, "log": process.log}),
            )
            .await
        {
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: the end of baseline process {}: {e}",
                process.process_id
            );
        }
    }
}

/// Ask this host's baseline test commands of run `run`, if any, to stop (its cancel or pause, DC-T07 (2)).
fn stop_baseline(common: &Common, run: &str) {
    if let Ok(testing) = common.testing.lock()
        && let Some(stop) = testing.get(run)
    {
        stop.store(true, Ordering::SeqCst);
    }
}

/// Run `run`'s confirmed baseline test `commands` off the async threads, owned by this host until `stop` (its cancel or
/// pause) or the host's stop, and record them; a failure is said and the next tick tries again while the run owns
/// them (docs/adr/ADR-039.md decision 8, DC-T07).
async fn baseline_tests(
    common: &Arc<Common>,
    run: &str,
    commands: Vec<String>,
    stop: Arc<AtomicBool>,
) {
    use std::io::Write as _;
    let project = common.project.clone();
    let log_dir = PathBuf::from(".hoh/baseline").join(run.replace('%', "%25").replace('/', "%2F"));
    let owner = BaselineOwner {
        common: common.clone(),
        run: run.to_owned(),
        stop,
        runtime: tokio::runtime::Handle::current(),
    };
    let ran = super::begun::spawn_blocking(move || {
        super::discovery::run_tests(&project, &log_dir, &commands, &owner)
    })
    .await;
    match ran {
        Ok(Ok(payload)) => {
            if let Err(e) = common
                .shared
                .store
                .record_baseline_tests(run, payload)
                .await
            {
                let _ = writeln!(
                    std::io::stderr(),
                    "hoh host: baseline tests of run {run}: {e}"
                );
            }
        }
        Ok(Err(e)) => {
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: baseline tests of run {run}: {e}"
            );
        }
        Err(e) => {
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: baseline tests of run {run} stopped: {e}"
            );
        }
    }
}

/// The loop of `shared`'s project until `stop`.
pub(super) async fn run(
    shared: Arc<Shared>,
    clock: Arc<dyn Clock>,
    executor: Option<Arc<dyn Executor>>,
    inflight: Inflight,
    project: PathBuf,
    graces: hoh_core::cancel::Graces,
    mut stop: watch::Receiver<bool>,
) {
    let mut watermark = shared.store.watermark();
    let common = Arc::new(Common {
        shared,
        clock,
        executor,
        inflight,
        project,
        canceller: Canceller::new(graces, LOOK_EVERY),
        cancelling: Mutex::new(BTreeSet::new()),
        testing: Mutex::new(BTreeMap::new()),
        stopping: Arc::new(AtomicBool::new(false)),
    });
    // A recorded baseline test group an earlier host left is settled before anything else touches the baseline
    // (DC-T07 (3)); each tick settles again what could not be told then.
    settle_baseline(&common).await;
    let mut tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    loop {
        if *stop.borrow() {
            break;
        }
        if let Err(e) = tick(&common, &mut tasks).await {
            use std::io::Write as _;
            let _ = writeln!(std::io::stderr(), "hoh host: pause and cancel: {e}");
        }
        tasks.retain(|t| !t.is_finished());
        tokio::select! {
            _ = watermark.changed() => {}
            () = tokio::time::sleep(TICK) => {}
            _ = stop.changed() => break,
        }
    }
    // The host stops: a cancel in progress is left to the next host (its reconcile stops the processes and its loop
    // finishes the cancel); a baseline test command running is killed with its group, its tests unrecorded (the next
    // host runs them again).
    common.stopping.store(true, Ordering::SeqCst);
    for task in tasks {
        task.abort();
        let _ = task.await;
    }
}

async fn tick(
    common: &Arc<Common>,
    tasks: &mut Vec<tokio::task::JoinHandle<()>>,
) -> Result<(), super::runner::TickError> {
    let store = &common.shared.store;
    let waiting = super::runner::no_run(store.runner_waiting(&common.shared.project_id).await)?;
    // Each pausing run's step is its own item: a run stopped meanwhile is Held, skipped, and the others go on
    // (docs/adr/ADR-031.md correction D rule 2 (b), M2T13R-01).
    let mut held = super::runner::Held::default();
    for run in &waiting.mode_changes {
        held.item(store.mode_step(run).await)?;
    }
    // A recorded baseline test group of no command this host runs: settled (DC-T07 (3)).
    settle_baseline(common).await;
    // A pausing run's baseline test command is stopped first: the run is PAUSED only once its group is gone (the store
    // counts its record until its end is recorded, DC-T07 (2)).
    for run in &waiting.pausing {
        stop_baseline(common, run);
        held.item(store.pause_step(run, common.clock.now()).await)?;
    }
    // Each run in DISCOVERING without its baseline: the project's read-only discovery, then its run.baseline
    // (docs/adr/ADR-039.md decision 4). A failed look is said and tried again on the next tick.
    for run in &waiting.discovering {
        let project = common.project.clone();
        match super::begun::spawn_blocking(move || super::discovery::discover(&project)).await {
            Ok(Ok(payload)) => {
                held.item(store.record_baseline(run, payload).await)?;
            }
            Ok(Err(e)) => {
                use std::io::Write as _;
                let _ = writeln!(std::io::stderr(), "hoh host: discovery of run {run}: {e}");
            }
            Err(e) => {
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "hoh host: discovery of run {run} stopped: {e}"
                );
            }
        }
    }
    // Each run whose user confirmed the baseline's test run: its test commands once, in a task of its own (they may
    // take long; the loop goes on), recorded as run.baseline_tests (docs/adr/ADR-039.md decision 8).
    for (run, commands) in waiting.baseline_tests {
        let stop = Arc::new(AtomicBool::new(false));
        let new = common
            .testing
            .lock()
            .map(|mut t| match t.entry(run.clone()) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(stop.clone());
                    true
                }
                std::collections::btree_map::Entry::Occupied(_) => false,
            })
            .unwrap_or(false);
        if new {
            let common = common.clone();
            tasks.push(super::begun::spawn(async move {
                baseline_tests(&common, &run, commands, stop).await;
                if let Ok(mut t) = common.testing.lock() {
                    t.remove(&run);
                }
            }));
        }
    }
    let mut runs: BTreeSet<String> = waiting.cancelling.into_iter().collect();
    let project = common.shared.project_id.clone();
    runs.extend(
        super::runner::no_run(
            super::http::read(&common.shared, move |reader| {
                reader.requested_cancellations(&project)
            })
            .await,
        )?
        .into_iter()
        .map(|(run, _)| run),
    );
    for run in runs {
        let new = common
            .cancelling
            .lock()
            .map(|mut c| c.insert(run.clone()))
            .unwrap_or(false);
        if new {
            let common = common.clone();
            tasks.push(super::begun::spawn(async move {
                cancel(&common, &run).await;
                if let Ok(mut c) = common.cancelling.lock() {
                    c.remove(&run);
                }
            }));
        }
    }
    Ok(())
}

/// One cancellation owner per run, shared by explicit run cancellation and attempt-only requests.
/// Only an explicit CANCEL_REQUESTED run may use the run-level canceller (ADR-033 Q10).
async fn cancel(common: &Arc<Common>, run_id: &str) {
    let mut works: BTreeMap<String, Work> = BTreeMap::new();
    let pins: Pins = Arc::default();
    loop {
        let requested = async {
            let waiting = common
                .shared
                .store
                .runner_waiting(&common.shared.project_id)
                .await?;
            let whole_run = waiting.cancelling.iter().any(|run| run == run_id);
            let project = common.shared.project_id.clone();
            let ids: BTreeSet<String> = super::http::read(&common.shared, move |reader| {
                reader.requested_cancellations(&project)
            })
            .await?
            .into_iter()
            .filter(|(run, _)| run == run_id)
            .map(|(_, attempt)| attempt)
            .collect();
            Ok::<_, PersistError>((whole_run, ids))
        }
        .await;
        let (whole_run, ids) = match requested {
            Ok(requested) => requested,
            Err(e) => {
                use std::io::Write as _;
                let _ = writeln!(
                    std::io::stderr(),
                    "hoh host: reading cancellation requests for {run_id}: {e}"
                );
                return;
            }
        };
        if !whole_run && ids.is_empty() {
            return;
        }
        // The run's baseline test command stops first: the run is CANCELLED only once its group is gone (the store
        // counts its record until its end is recorded, DC-T07 (2)).
        if whole_run {
            stop_baseline(common, run_id);
        }
        // What this host runs for the run stops first, so the cancel alone ends its attempts (one writer, ADR-021
        // decision 8c); a native interrupt goes to the work it was given.
        let entries: Vec<(String, Entry)> = common
            .inflight
            .lock()
            .map(|mut inflight| {
                let ids: Vec<String> = inflight
                    .iter()
                    .filter(|(id, e)| e.work.run_id == run_id && (whole_run || ids.contains(*id)))
                    .map(|(id, _)| id.clone())
                    .collect();
                ids.into_iter()
                    .filter_map(|id| inflight.remove(&id).map(|e| (id, e)))
                    .collect()
            })
            .unwrap_or_default();
        for (id, entry) in entries {
            if let Some(task) = entry.task {
                task.abort();
                let _ = task.await;
            }
            works.insert(id, entry.work);
        }
        let step = async {
            // How each attempt still running is tracked, as its start recorded.
            let mut trackings = BTreeMap::new();
            let attempts = if whole_run {
                common.shared.store.running_attempts(run_id).await?
            } else {
                super::http::read(&common.shared, move |reader| {
                    ids.iter()
                        .filter_map(|id| reader.attempt(id).transpose())
                        .collect::<Result<Vec<_>, _>>()
                })
                .await?
            };
            for attempt in &attempts {
                let id = attempt.attempt_id.clone();
                let event = super::http::read(&common.shared, move |reader| {
                    reader.attempt_event("attempt.tracking", &id)
                })
                .await?;
                trackings.insert(attempt.attempt_id.clone(), Tracking::of(event.as_ref()));
            }
            let processes: Arc<dyn Processes> = Arc::new(HostProcesses {
                works: works.clone(),
                executor: common.executor.clone(),
                project: common.project.clone(),
                project_id: common.shared.project_id.clone(),
                pins: pins.clone(),
                trackings,
            });
            if !whole_run {
                for attempt in &attempts {
                    if attempt.cancel_requested
                        && matches!(
                            attempt.state,
                            AttemptState::Prepared | AttemptState::Starting | AttemptState::Running
                        )
                    {
                        common
                            .canceller
                            .step_attempt(
                                &common.shared.store,
                                attempt,
                                processes.clone(),
                                common.clock.clone(),
                                Arc::new(std::thread::sleep),
                            )
                            .await?;
                    }
                }
                return Ok(Finished::Running(attempts.len()));
            }
            common
                .canceller
                .step(
                    &common.shared.store,
                    run_id,
                    processes,
                    common.clock.clone(),
                    Arc::new(std::thread::sleep),
                )
                .await
        };
        match step.await {
            // A round waiting for its candidate's pending records: the runner's recovery settles them, the next step
            // finishes (docs/adr/ADR-031.md decision 4).
            Ok(Finished::Running(_) | Finished::Recovering) => tokio::time::sleep(STEP_AGAIN).await,
            Ok(Finished::Cancelled | Finished::NotCancelling) => return,
            Err(e) => {
                use std::io::Write as _;
                let _ = writeln!(std::io::stderr(), "hoh host: the cancel of {run_id}: {e}");
                tokio::time::sleep(STEP_AGAIN).await;
            }
        }
    }
}

/// The product's processes for a cancel: an attempt's group found by its recorded identity (ADR-019 decision 4) and
/// the processes of the attempt found outside it, its native interrupt through the executor that runs it, and the
/// project's workspace.
struct HostProcesses {
    works: BTreeMap<String, Work>,
    executor: Option<Arc<dyn Executor>>,
    project: PathBuf,
    project_id: String,
    pins: Pins,
    /// How each running attempt is tracked, as its start recorded (decision 8d v5 to v7).
    trackings: BTreeMap<String, Tracking>,
}

impl Processes for HostProcesses {
    fn control(&self, attempt: &AttemptRow) -> Option<(Box<dyn ProcessControl + Send>, bool)> {
        // Recorded by a host that saw another world: no signal, no proof (M2T06R-900).
        if matches!(
            self.trackings.get(&attempt.attempt_id),
            Some(Tracking::Foreign(_))
        ) {
            return Some((Box::new(Fixed(Look::Lost)), false));
        }
        let presence = identity::presence(
            attempt.pid,
            attempt.boot_id.as_deref(),
            attempt.process_start_id.as_deref(),
        );
        let native = match (&self.executor, self.works.get(&attempt.attempt_id)) {
            (Some(executor), Some(work)) => Some((executor.clone(), work.clone())),
            _ => None,
        };
        let has_native = native.is_some();
        let cgroup = self
            .trackings
            .get(&attempt.attempt_id)
            .and_then(Tracking::cgroup);
        let outside = {
            let Ok(mut pins) = self.pins.lock() else {
                return Some((Box::new(Fixed(Look::Lost)), false));
            };
            pins.entry(attempt.attempt_id.clone())
                .or_insert_with(|| {
                    Arc::new(Mutex::new(
                        Outside::new(&super::runner::marker(
                            &self.project_id,
                            &attempt.attempt_id,
                        ))
                        .born(attempt.process_start_id.as_deref())
                        .by_cgroup(cgroup.is_some()),
                    ))
                })
                .clone()
        };
        let control = |group: Watch| {
            if let Ok(mut outside) = outside.lock() {
                if matches!(group, Watch::Found(_)) {
                    outside.set_leader(attempt.pid, attempt.process_start_id.as_deref());
                } else {
                    outside.set_leader(None, None);
                }
            }
            AttemptControl {
                group,
                native: native.clone(),
                outside: outside.clone(),
                cgroup: cgroup.clone(),
                term_incomplete: false,
            }
        };
        // The recorded pid, which is the group's number while anything of the group is left.
        let number = attempt.pid.and_then(|p| u32::try_from(p).ok());
        Some(match presence {
            Ok(Presence::Ours(found)) => (Box::new(control(Watch::Found(found))), has_native),
            // Running with no process recorded: nothing can be looked at (as the reconcile, M2T05R-100).
            Ok(Presence::Gone(identity::Gone::NoPid)) if attempt.state == AttemptState::Running => {
                (Box::new(Fixed(Look::Lost)), false)
            }
            // Its group is gone (or no process was recorded: held at its start gate, which closed when its task
            // stopped): the finding proves it; what of it left the group is still looked for.
            Ok(Presence::Gone(_)) => (Box::new(control(Watch::Gone)), false),
            // Its group cannot be told apart, or its leader not inspected or confirmed: no signal may go to the group
            // (ADR-019 decision 8). With a cgroup, its members are stopped by their membership, and the group must be
            // seen gone by its number (decision 8d v6, M2T06R-401; v7, M2T06R-500: never taken for gone); without one
            // it stays cancelling while its number is in use, as pid or as group (below, M2T06R-600, -700).
            Ok(Presence::Leaderless(left)) if cgroup.is_some() => {
                (Box::new(control(Watch::Number(left.pgid))), false)
            }
            _ if cgroup.is_some() => match number {
                Some(pgid) => (Box::new(control(Watch::Number(pgid))), false),
                None => (Box::new(Fixed(Look::Lost)), false),
            },
            // Without a cgroup: Lost while its group number is in use; once no process has it, the rest of the proof
            // (decision 8d v7, M2T06R-600).
            _ => match number {
                Some(pgid) if identity::number_free(pgid) => {
                    (Box::new(control(Watch::Number(pgid))), false)
                }
                _ => (Box::new(Fixed(Look::Lost)), false),
            },
        })
    }

    fn workspace(&self) -> Value {
        workspace(&self.project)
    }

    fn pending_candidates(&self) -> std::collections::BTreeSet<String> {
        crate::verify::check::pending_candidates(&self.project.join(".hoh"))
    }
}

/// A look at an attempt with its group seen `group` (Held or Empty): while the group lives the processes outside it are
/// kept up with (every [`super::processes::RESCAN`]) and no scan counts for the proof; once it is empty every look
/// scans, and only the termination proof makes it Empty (decision 8d, M2T06R-252, -253).
fn look_at(group: Look, outside: &mut Outside) -> Look {
    if group == Look::Held {
        let _ = outside.keep_up();
        outside.group_alive();
        return Look::Held;
    }
    match outside.scan() {
        Proof::Done => Look::Empty,
        Proof::Pending | Proof::Incomplete => Look::Held,
    }
}

/// The processes outside each attempt's group of a cancel, kept for the whole cancel, across its passes.
type Pins = Arc<Mutex<BTreeMap<String, Arc<Mutex<Outside>>>>>;

/// How an attempt's process group is known to be empty (decision 8d v7, M2T06R-500): every way is an observation, none
/// a default.
enum Watch {
    /// Found by its leader's identity: its own look, and it gets the ladder's signals.
    Found(identity::Found),
    /// The identity's finding proves it gone (no pid recorded and never running, the leader gone and its number free,
    /// another boot, a reused pid).
    Gone,
    /// Not to be signalled (its leader gone, not inspected or not confirmed): empty only when the kernel says no process
    /// uses that number, as its pid or as its group ([`identity::number_free`], M2T06R-700, -800).
    Number(u32),
}

/// An attempt's group and its processes outside it (decision 8d): the group counts as empty only with the termination
/// proof (no pinned one alive, two complete scans in a row that found none).
struct AttemptControl {
    group: Watch,
    native: Option<(Arc<dyn Executor>, Work)>,
    outside: Arc<Mutex<Outside>>,
    /// Its cgroup, when it has one: the proof is then the cgroup's and the marker scan's together (decision 8d v6).
    cgroup: Option<AttemptGroup>,
    /// Its last SIGTERM's walk of the cgroup did not reach the whole subtree (CGW-50).
    term_incomplete: bool,
}

/// Whether group number `pgid` may still have a member, now or later: a process in that group, or process `pgid`
/// itself, alive in any group, which can make it again (M2T06R-700). Anything but ESRCH, EPERM included, means in use
/// (M2T06R-558, [`identity::number_free`]).
fn group_lives(pgid: u32) -> bool {
    !identity::number_free(pgid)
}

impl ProcessControl for AttemptControl {
    fn look(&mut self) -> Look {
        // One look at a group watched by its leader's identity.
        let found = match &self.group {
            Watch::Found(found) => Some((found.look(), found.identity.pid)),
            Watch::Gone | Watch::Number(_) => None,
        };
        // Lost track of (its leader reaped, and the group not looked at within HELD_FOR, ADR-019 decision 8): no signal
        // may go to the group from now on. Without a cgroup the look is Lost. With one, the group is watched by its
        // number from this look on, in this pass, as one that cannot be told apart: its members are the cgroup's,
        // stopped by membership (decision 8d v6, M2T06R-401; v7: empty only once the kernel says no process uses the
        // number). F2: the pass ended Lost, and the next began the ladder again.
        if let Some((identity::Group::Lost, pid)) = found
            && self.cgroup.is_some()
        {
            self.group = Watch::Number(pid);
            if let Ok(mut outside) = self.outside.lock() {
                outside.set_leader(None, None);
            }
        }
        let group = match (&self.group, found) {
            (Watch::Found(_), Some((identity::Group::Held, _))) => Look::Held,
            (Watch::Found(_), Some((identity::Group::Empty, _))) => Look::Empty,
            (Watch::Found(_), _) => return Look::Lost,
            (Watch::Gone, _) => Look::Empty,
            (Watch::Number(pgid), _) if group_lives(*pgid) => Look::Held,
            (Watch::Number(_), _) => Look::Empty,
        };
        let Ok(mut outside) = self.outside.lock() else {
            return Look::Held;
        };
        if let Some(cgroup) = &self.cgroup {
            // The group empty, the cgroup empty, and then the marker scan's proof (decision 8d v6, v7).
            let emptied = group == Look::Empty && cgroup.proof() == Proof::Done;
            if !emptied {
                return look_at(Look::Held, &mut outside);
            }
            return match look_at(Look::Empty, &mut outside) {
                Look::Empty => {
                    cgroup.remove();
                    Look::Empty
                }
                other => other,
            };
        }
        look_at(group, &mut outside)
    }

    fn send(&mut self, signal: Signal) -> bool {
        let (sig, group) = match signal {
            Signal::Native => {
                // Sent only when it reached a turn the executor still holds (decision 17, M2T07R-501).
                return match &self.native {
                    Some((executor, work)) => executor.cancel(work),
                    None => false,
                };
            }
            Signal::Term => (
                rustix::process::Signal::TERM,
                match &self.group {
                    Watch::Found(found) => found.terminate(),
                    Watch::Gone | Watch::Number(_) => false,
                },
            ),
            Signal::Kill => (
                rustix::process::Signal::KILL,
                match &self.group {
                    Watch::Found(found) => found.kill(),
                    Watch::Gone | Watch::Number(_) => false,
                },
            ),
        };
        let in_cgroup = match &self.cgroup {
            // SIGTERM to each member; SIGKILL to all of them at once, by the kernel.
            Some(cgroup) if sig == rustix::process::Signal::KILL => cgroup.kill(),
            Some(cgroup) => {
                let termed = cgroup.terminate();
                self.term_incomplete = !termed.whole;
                termed.sent
            }
            None => false,
        };
        // And what the marker scan pinned (outside the cgroup, if it has one).
        let outside = self.outside.lock().is_ok_and(|mut o| o.signal(sig));
        in_cgroup || outside || group
    }

    fn sigterm_incomplete(&mut self) -> bool {
        self.term_incomplete
    }
}

/// A group whose look is known without looking: gone, or lost track of.
struct Fixed(Look);

impl ProcessControl for Fixed {
    fn look(&mut self) -> Look {
        self.0
    }

    fn send(&mut self, _signal: Signal) -> bool {
        false
    }
}

/// How long one git command of the workspace record may take.
const GIT_LIMIT: Duration = Duration::from_secs(5);

/// git's empty tree, the base of a diff in a repository with no commit yet.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Run git read-only in `project` (no optional lock, no fsmonitor, so it writes no index, object or ref) for at most
/// [`GIT_LIMIT`]; its output.
pub(crate) fn git(project: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let (status, out) = git_run(project, args)?;
    if !status.success() {
        return Err(format!("git {}: {status}", args.join(" ")));
    }
    Ok(out)
}

thread_local! {
    /// The attempt whose run directory this thread's git commands make or read, if any (see [`as_attempt`]).
    static ATTEMPT: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with every git command it runs on this thread carrying attempt marker `marker` in its environment
/// (`HOH_ATTEMPT_ID`, as the attempt's own processes do): a run directory's checkout is made while its attempt has no
/// process recorded, so a git a killed host left working there is found by the attempt's marker and stopped by the
/// next start's reconcile before the attempt ends and its directory is removed (docs/adr/ADR-035.md, the review's
/// checkout). The thread's earlier marker is restored after, whatever `f` does.
pub(crate) fn as_attempt<T>(marker: &str, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let earlier = self.0.take();
            ATTEMPT.with(|a| *a.borrow_mut() = earlier);
        }
    }
    let _restore = Restore(ATTEMPT.with(|a| a.borrow_mut().replace(marker.to_owned())));
    f()
}

/// [`git`] whatever its exit status: the status and the output (`git check-ignore` answers "not ignored" with 1). git
/// runs in a process group of its own. On every path, its output's reader is joined before this returns, after the
/// group is killed while git is not yet reaped (so its pid still names that group): the pipe's writers are gone then,
/// and the join waits only for what is already in the pipe (M2T13R-450). Only a process git started that left the
/// group and keeps the pipe open could hold it; git's read-only commands here start none that do.
pub(crate) fn git_run(
    project: &Path,
    args: &[&str],
) -> Result<(std::process::ExitStatus, Vec<u8>), String> {
    use rustix::process::{WaitId, WaitIdOptions};
    use std::io::Read as _;
    use std::os::unix::process::CommandExt as _;
    let mut command = std::process::Command::new("git");
    if let Some(marker) = ATTEMPT.with(|a| a.borrow().clone()) {
        command.env(super::processes::ATTEMPT_ENV, marker);
    }
    let mut child = command
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(project)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let git = rustix::process::Pid::from_child(&child);
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout
            .ok_or_else(|| std::io::Error::other("no stdout"))?
            .read_to_end(&mut out)
            .map(|_| out)
    });
    let end = Instant::now() + GIT_LIMIT;
    // Its end is seen without reaping it: its pid, which names its group, stays its own until the wait below.
    let ended = loop {
        match rustix::process::waitid(
            WaitId::Pid(git),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => break Ok(()),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => break Err(format!("git {}: over {GIT_LIMIT:?}", args.join(" "))),
            Err(e) => break Err(format!("git {}: {e}", args.join(" "))),
        }
    };
    // What git started and left in its group ends with it (and git, past the limit); then git is reaped, then the
    // reader joined.
    let _ = rustix::process::kill_process_group(git, rustix::process::Signal::KILL);
    if ended.is_err() {
        let _ = child.kill();
    }
    let status = child.wait();
    let read = reader.join();
    ended?;
    let status = status.map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    let out = read
        .map_err(|_| "git: its output was lost".to_owned())?
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    Ok((status, out))
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// One untracked path's part of the workspace record, read without following a link and without waiting on anything
/// that is not a regular file (M2T06R-06): a regular file is its content's hash, a symbolic link its target's, anything
/// else (a FIFO, a socket, a device, a directory) its type, never opened. The file is opened without following a link
/// and without blocking, and read only if it is still a regular file then. A file not read to its end by `deadline` is
/// its size and modification time instead (M2T06R-102); the flag says whether its content was hashed.
fn untracked_entry(path: &Path, deadline: Instant) -> (String, bool) {
    let entry = untracked_content(path, deadline);
    let hashed = !entry.starts_with("unhashed:");
    (entry, hashed)
}

fn untracked_content(path: &Path, deadline: Instant) -> String {
    use rustix::fs::{FileType, Mode, OFlags};
    use std::io::Read as _;
    let kind = |t: FileType| match t {
        FileType::Fifo => "fifo",
        FileType::Socket => "socket",
        FileType::CharacterDevice => "char_device",
        FileType::BlockDevice => "block_device",
        FileType::Directory => "directory",
        FileType::Symlink => "symlink",
        _ => "other",
    };
    let file_type = match rustix::fs::lstat(path) {
        Ok(stat) => FileType::from_raw_mode(stat.st_mode),
        Err(e) => return format!("unreadable: {e}"),
    };
    match file_type {
        FileType::Symlink => {
            return match std::fs::read_link(path) {
                Ok(target) => format!("symlink:{}", hex(target.as_os_str().as_bytes())),
                Err(e) => format!("unreadable: {e}"),
            };
        }
        FileType::RegularFile => {}
        other => return format!("type:{}", kind(other)),
    }
    let flags =
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC;
    let fd = match rustix::fs::open(path, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(e) => return format!("unreadable: {e}"),
    };
    match rustix::fs::fstat(&fd) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => {}
        Ok(stat) => return format!("type:{}", kind(FileType::from_raw_mode(stat.st_mode))),
        Err(e) => return format!("unreadable: {e}"),
    }
    let unhashed = |stat: &rustix::fs::Stat| {
        format!(
            "unhashed:{}:{}.{}",
            stat.st_size, stat.st_mtime, stat.st_mtime_nsec
        )
    };
    let Ok(meta) = rustix::fs::fstat(&fd) else {
        return "unreadable: fstat".to_owned();
    };
    let mut file = std::fs::File::from(fd);
    let mut content = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        if Instant::now() >= deadline {
            return unhashed(&meta);
        }
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => content.update(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return format!("unreadable: {e}"),
        }
    }
    content
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The workspace's record before CANCELLED (REQ-043 behaviour 4, ADR-021 decision 8g): HEAD and its tree, and the
/// hashes of the diff against HEAD, of the untracked files' names and contents and of the status, and of all of
/// them; read-only, nothing reset, cleaned or removed. A directory that is not a git work tree, or a git that fails,
/// is recorded as such. The untracked files' contents are hashed within [`UNTRACKED_BUDGET`]: the ones left are
/// recorded by size and modification time, and `untracked_unhashed` counts them.
pub(crate) fn workspace(project: &Path) -> Value {
    workspace_within(project, UNTRACKED_BUDGET)
}

/// Run `end`, one commit of an attempt's end whose processes were proven gone, without a workspace record; when the
/// store refuses it as an interrupted end of the writer lease's holder, the workspace's record of `project` is taken
/// on a blocking thread (ADR-021 decision 8g) and `end` runs again with it (REQ-019 behaviour 6,
/// docs/adr/ADR-024.md decision 9). Whether the end is interrupted is known only inside its transaction (a cancel may
/// come meanwhile), so the store decides, not this caller.
pub(super) async fn recorded_end<T, F, Fut>(project: &Path, end: F) -> Result<T, PersistError>
where
    F: Fn(Option<Value>) -> Fut,
    Fut: std::future::Future<Output = Result<T, PersistError>>,
{
    match end(None).await {
        Err(PersistError::WorkspaceRecordRequired(_)) => {
            let project = project.to_path_buf();
            let record = super::begun::spawn_blocking(move || workspace(&project))
                .await
                .map_err(|e| PersistError::Storage(format!("the workspace record ended: {e}")))?;
            end(Some(record)).await
        }
        other => other,
    }
}

/// How long the workspace record may spend on the untracked files' contents (a huge or sparse file must not hold the
/// cancel back, M2T06R-102).
const UNTRACKED_BUDGET: Duration = Duration::from_secs(5);

fn workspace_within(project: &Path, budget: Duration) -> Value {
    let text = |out: Vec<u8>| String::from_utf8_lossy(&out).trim().to_owned();
    let head = git(project, &["rev-parse", "--verify", "HEAD"])
        .map(text)
        .ok();
    let head_tree = git(project, &["rev-parse", "--verify", "HEAD^{tree}"])
        .map(text)
        .ok();
    let base = head.clone().unwrap_or_else(|| EMPTY_TREE.to_owned());
    let record = (|| -> Result<Value, String> {
        let diff = git(
            project,
            &["diff", "--binary", "--no-ext-diff", "--no-textconv", &base],
        )?;
        let names = git(
            project,
            &["ls-files", "-z", "--others", "--exclude-standard"],
        )?;
        let mut untracked = Sha256::new();
        let deadline = Instant::now() + budget;
        let mut unhashed = 0usize;
        for name in names.split(|b| *b == 0).filter(|n| !n.is_empty()) {
            untracked.update(name);
            untracked.update([0]);
            let path = project.join(std::ffi::OsStr::from_bytes(name));
            let (entry, hashed) = untracked_entry(&path, deadline);
            unhashed += usize::from(!hashed);
            untracked.update(entry.as_bytes());
            untracked.update([0]);
        }
        let untracked: String = untracked
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let status = git(project, &["status", "--porcelain", "-z"])?;
        let (diff, status) = (hex(&diff), hex(&status));
        let all = hex(format!(
            "{}\n{}\n{diff}\n{untracked}\n{status}",
            head.as_deref().unwrap_or(""),
            head_tree.as_deref().unwrap_or("")
        )
        .as_bytes());
        Ok(json!({
            "head": head,
            "head_tree": head_tree,
            "diff_sha256": diff,
            "untracked_sha256": untracked,
            "untracked_unhashed": unhashed,
            "status_sha256": status,
            "workspace_sha256": all,
        }))
    })();
    let mut record =
        record.unwrap_or_else(|e| json!({"head": head, "head_tree": head_tree, "error": e}));
    if let Some(fields) = record.as_object_mut() {
        fields.insert(
            "host_log".to_owned(),
            json!(project.join(".hoh/host.log").to_string_lossy()),
        );
    }
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    fn git_in(dir: &Path, args: &[&str]) -> R {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("git {args:?}: {status}").into())
        }
    }

    /// The processes whose command line is `sleep <seconds>`.
    fn sleeping(seconds: &str) -> R<Vec<i32>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir("/proc")?.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            if std::fs::read(entry.path().join("cmdline"))
                .is_ok_and(|c| c == format!("sleep\0{seconds}\0").as_bytes())
            {
                found.push(pid);
            }
        }
        Ok(found)
    }

    #[test]
    fn a_git_command_past_its_limit_ends_with_what_it_started_and_its_reader_is_joined() -> R {
        // M2T13R-450: git (a shell alias here) starts a process that holds its stdout past the limit. git_run returns
        // at the limit, and nothing git started still runs then: its output's reader saw the pipe close and was
        // joined, on this path as on the others.
        let dir = tempfile::tempdir()?;
        git_in(dir.path(), &["init", "-q"])?;
        let seconds = format!("40.{}", std::process::id());
        let began = Instant::now();
        let ran = git_run(
            dir.path(),
            &["-c", &format!("alias.hang=!sleep {seconds}"), "hang"],
        );
        let took = began.elapsed();
        let left = sleeping(&seconds)?;
        for pid in &left {
            if let Some(pid) = rustix::process::Pid::from_raw(*pid) {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
        assert!(ran.is_err(), "{ran:?}");
        assert!(
            left.is_empty(),
            "what git started still ran when git_run returned: {left:?}"
        );
        assert!(took < GIT_LIMIT + Duration::from_secs(3), "{took:?}");
        Ok(())
    }

    /// A /proc tree of a test's own: `processes` as (pid, environment), none of them real.
    fn tree(processes: &[(u32, &str)]) -> R<tempfile::TempDir> {
        let root = tempfile::tempdir()?;
        std::fs::create_dir_all(root.path().join("sys/kernel"))?;
        std::fs::write(root.path().join("sys/kernel/ns_last_pid"), b"1000\n")?;
        for (pid, env) in processes {
            add(root.path(), *pid, env)?;
        }
        Ok(root)
    }

    fn add(root: &Path, pid: u32, env: &str) -> R {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("stat"),
            format!("{pid} (x) S 1 {pid} {pid} 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 77 0"),
        )?;
        std::fs::write(dir.join("environ"), format!("{env}\0"))?;
        Ok(())
    }

    /// An executor whose native cancel answers `reached`.
    #[derive(Debug)]
    struct Answers(bool);

    impl Executor for Answers {
        fn deliver(&self, _work: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
            tokio::sync::oneshot::channel().1
        }

        fn start(
            &self,
            _work: &Work,
        ) -> Result<super::super::runner::Started, super::super::runner::Refusal> {
            Err("not started".to_owned().into())
        }

        fn cancel(&self, _work: &Work) -> bool {
            self.0
        }
    }

    #[test]
    fn a_cancels_native_step_counts_as_sent_only_when_it_reached_a_turn() {
        // M2T07R-501, -551 (docs/adr/ADR-022.md decision 17, ADR-021 decision 8c): CancelRun's native step is recorded
        // as sent only when the executor's cancel reached a turn it still holds.
        let work = Work {
            project_id: "p1".to_owned(),
            run_id: "R".to_owned(),
            task_id: "T1".to_owned(),
            attempt_id: "A1".to_owned(),
            role: "developer".to_owned(),
            effect_key: "A1/work".to_owned(),
            assignment: hoh_core::persistence::Assignment::default(),
            run_dir: None,
            helpers: None,
        };
        for reached in [true, false] {
            let mut control = AttemptControl {
                group: Watch::Gone,
                native: Some((Arc::new(Answers(reached)), work.clone())),
                outside: Arc::new(Mutex::new(Outside::new("p1/A1"))),
                cgroup: None,
                term_incomplete: false,
            };
            assert_eq!(control.send(Signal::Native), reached, "reached: {reached}");
        }
    }

    #[test]
    fn only_scans_after_the_group_emptied_count_for_the_proof() -> R {
        // M2T06R-252: looks while the group lives keep up with the processes outside it but count nothing; the first
        // look at the empty group is one scan of the two the proof needs.
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        assert_eq!(look_at(Look::Held, &mut outside), Look::Held);
        assert_eq!(look_at(Look::Held, &mut outside), Look::Held);
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Held);
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Empty);
        Ok(())
    }

    #[test]
    fn a_cancels_sigterm_says_when_its_cgroup_walk_missed_part_of_the_subtree() -> R {
        // CGW-50: the cancel's SIGTERM keeps whether its walk of the attempt's cgroup was whole, for the pass to record:
        // a cgroup below that cannot be opened makes it incomplete; once it can be, the next SIGTERM is whole again.
        use std::os::unix::fs::PermissionsExt as _;
        let cgroup = tempfile::tempdir()?;
        let at = cgroup.path().join("attempt-0123456789abcdef");
        std::fs::create_dir_all(at.join("shut"))?;
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut control = AttemptControl {
            group: Watch::Gone,
            native: None,
            outside: Arc::new(Mutex::new(
                Outside::new("p/A1").with_root(Some(root.path().to_owned())),
            )),
            cgroup: Some(AttemptGroup::witnessed(cgroup.path(), &at)?),
            term_incomplete: false,
        };
        std::fs::set_permissions(at.join("shut"), std::fs::Permissions::from_mode(0o000))?;
        control.send(Signal::Term);
        let shut = control.sigterm_incomplete();
        std::fs::set_permissions(at.join("shut"), std::fs::Permissions::from_mode(0o755))?;
        control.send(Signal::Term);
        let open = control.sigterm_incomplete();
        assert_eq!((shut, open), (true, false));
        Ok(())
    }

    #[test]
    fn a_cancels_look_with_a_cgroup_not_proved_empty_holds() -> R {
        // M2T06R-451 (decision 8d v6): the group gone, the attempt's cgroup populated or unreadable: Held; once it is
        // empty, the marker scan's two scans are still needed before Empty.
        use std::os::unix::fs::PermissionsExt as _;
        let cgroup = tempfile::tempdir()?;
        let events = cgroup.path().join("cgroup.events");
        std::fs::write(&events, b"populated 1\n")?;
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut control = AttemptControl {
            group: Watch::Gone,
            native: None,
            outside: Arc::new(Mutex::new(
                Outside::new("p/A1").with_root(Some(root.path().to_owned())),
            )),
            cgroup: Some(AttemptGroup::witnessed(cgroup.path(), cgroup.path())?),
            term_incomplete: false,
        };
        assert_eq!(control.look(), Look::Held, "populated");
        std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o000))?;
        assert_eq!(control.look(), Look::Held, "unreadable");
        std::fs::set_permissions(&events, std::fs::Permissions::from_mode(0o600))?;
        std::fs::write(&events, b"populated 0\n")?;
        assert_eq!(control.look(), Look::Held, "one quiet marker scan");
        assert_eq!(control.look(), Look::Empty);
        Ok(())
    }

    #[test]
    fn the_scan_of_a_look_at_a_living_group_never_counts() -> R {
        // M2T06R-353: a single look while the group lives scans (none before) and must not count; the one look at the
        // empty group after it is the first scan of the two.
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        assert_eq!(look_at(Look::Held, &mut outside), Look::Held);
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Held);
        Ok(())
    }

    #[test]
    fn a_look_at_an_empty_group_scans_however_recent_the_last_scan() -> R {
        // M2T06R-253: right after a scan, a marked process appears; the next look at the empty group scans again (no
        // 100 ms wait) and holds (it cannot be pinned in this tree, so the scan is incomplete).
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut outside = Outside::new("p/A1").with_root(Some(root.path().to_owned()));
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Held);
        add(
            root.path(),
            101,
            &format!("{}=p/A1", super::super::processes::ATTEMPT_ENV),
        )?;
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Held);
        assert_eq!(look_at(Look::Empty, &mut outside), Look::Held);
        Ok(())
    }

    #[test]
    fn a_cancel_keeps_an_attempts_outside_processes_across_its_passes() -> R {
        // M2T06R-257, -352: each pass asks for the attempt's control anew and gets the same pins, signal and count:
        // the first pass's look at its gone group is one quiet scan, the second pass's look the second, which proves it.
        // The attempt is STARTING with no process recorded (held at its gate, which closed): a group proved gone
        // (decision 8d v7; RUNNING with no process recorded proves nothing, the next test).
        let processes = HostProcesses {
            works: BTreeMap::new(),
            executor: None,
            project: PathBuf::from("/nonexistent"),
            project_id: format!("across-passes-{}", std::process::id()),
            pins: Arc::default(),
            trackings: BTreeMap::new(),
        };
        let row = AttemptRow {
            attempt_id: "A1".to_owned(),
            task_id: "T1".to_owned(),
            session_id: None,
            state: hoh_core::lifecycle::AttemptState::Starting,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 1,
            pid: None,
            boot_id: None,
            process_start_id: None,
            started_at: None,
            ended_at: None,
            raw_log_path: None,
            agent: None,
            resolved_model: None,
            failure_reason: None,
            cancel_requested: true,
            usage: None,
            cost_usd: None,
        };
        let (mut first, _) = processes.control(&row).ok_or("no control")?;
        assert_eq!(first.look(), Look::Held, "one quiet scan proves nothing");
        let (mut second, _) = processes.control(&row).ok_or("no control")?;
        assert_eq!(
            second.look(),
            Look::Empty,
            "the second pass counts on the first one's scan"
        );
        Ok(())
    }

    /// An attempt row of a cancel: RUNNING as given, with `pid` and no other identity.
    fn running_row(pid: Option<i64>) -> AttemptRow {
        AttemptRow {
            attempt_id: "A1".to_owned(),
            task_id: "T1".to_owned(),
            session_id: None,
            state: AttemptState::Running,
            role_epoch: 1,
            host_epoch: 1,
            config_revision: 1,
            pid,
            boot_id: None,
            process_start_id: None,
            started_at: None,
            ended_at: None,
            raw_log_path: None,
            agent: None,
            resolved_model: None,
            failure_reason: None,
            cancel_requested: true,
            usage: None,
            cost_usd: None,
        }
    }

    #[test]
    fn a_group_this_user_may_not_signal_lives() -> R {
        // M2T06R-558 (decision 8d v7): kill(-pgid, 0) refused (EPERM: a member with other credentials) means a member
        // is there; only ESRCH means none. A group led by a process of another user (root's daemons) stands in for it.
        let mine = rustix::process::getuid().as_raw();
        let other = std::fs::read_dir("/proc")?
            .filter_map(|e| e.ok()?.file_name().to_str()?.parse::<u32>().ok())
            .filter(|&pid| pid > 1)
            .find(|&pid| {
                let leads = std::fs::read(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|stat| {
                        identity::after_command_name(&stat)?
                            .split_whitespace()
                            .nth(2)?
                            .parse::<u32>()
                            .ok()
                    })
                    == Some(pid);
                let owner = std::fs::read_to_string(format!("/proc/{pid}/status"))
                    .ok()
                    .and_then(|s| {
                        s.lines()
                            .find_map(|l| l.strip_prefix("Uid:"))?
                            .split_whitespace()
                            .next()?
                            .parse::<u32>()
                            .ok()
                    });
                leads && owner.is_some_and(|uid| uid != mine)
            })
            .ok_or("no group of another user to look at")?;
        assert!(group_lives(other), "group {other}");
        // The group half on its own (M2T06R-850): its group refuses the signal (EPERM), so the number is in use (the
        // pid half is pinned by identity's a_number_used_only_as_another_users_pid_is_not_free, M2T06R-951).
        assert!(!identity::group_empty(other), "group {other}");
        assert!(!identity::number_free(other), "number {other}");
        Ok(())
    }

    #[test]
    fn a_cancel_proves_nothing_of_an_attempt_recorded_in_another_world() -> R {
        // M2T06R-900: no signal and no proof for an attempt whose tracking a host in other namespaces recorded, even
        // when its group looks gone from here.
        let processes = HostProcesses {
            works: BTreeMap::new(),
            executor: None,
            project: PathBuf::from("/nonexistent"),
            project_id: format!("foreign-{}", std::process::id()),
            pins: Arc::default(),
            trackings: BTreeMap::from([(
                "A1".to_owned(),
                Tracking::Foreign("elsewhere".to_owned()),
            )]),
        };
        let mut row = running_row(None);
        row.state = AttemptState::Starting;
        for _ in 0..3 {
            let (mut control, _) = processes.control(&row).ok_or("no control")?;
            assert_eq!(control.look(), Look::Lost);
        }
        Ok(())
    }

    #[test]
    fn a_running_attempt_with_no_process_recorded_is_never_taken_for_gone() -> R {
        // M2T06R-500 (decision 8d v7): RUNNING with no pid recorded proves nothing of its group (the reconcile holds it
        // as uncertain too): the cancel's look is Lost, with or without a cgroup, however often it looks.
        let cgroup = tempfile::tempdir()?;
        std::fs::write(cgroup.path().join("cgroup.events"), b"populated 0\n")?;
        for trackings in [
            BTreeMap::new(),
            BTreeMap::from([(
                "A1".to_owned(),
                Tracking::Cgroup {
                    path: cgroup.path().to_owned(),
                    witness: None,
                },
            )]),
        ] {
            let processes = HostProcesses {
                works: BTreeMap::new(),
                executor: None,
                project: PathBuf::from("/nonexistent"),
                project_id: format!("no-pid-{}", std::process::id()),
                pins: Arc::default(),
                trackings,
            };
            for _ in 0..3 {
                let (mut control, _) = processes.control(&running_row(None)).ok_or("no control")?;
                assert_eq!(control.look(), Look::Lost);
            }
        }
        Ok(())
    }

    #[test]
    fn an_unconfirmed_leader_without_a_cgroup_is_lost_while_its_group_lives_and_proved_once_it_is_gone()
    -> R {
        // M2T06R-600 (decision 8d v7): a record without a start time names a process that cannot be confirmed: no
        // signal to its group. Without a cgroup its cancel is Lost while its group number is in use; once no process
        // has it the proof goes on (two quiet scans), instead of Lost for ever.
        use std::os::unix::process::CommandExt as _;
        let processes = HostProcesses {
            works: BTreeMap::new(),
            executor: None,
            project: PathBuf::from("/nonexistent"),
            project_id: format!("unconfirmed-{}", std::process::id()),
            pins: Arc::default(),
            trackings: BTreeMap::new(),
        };
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;
        let row = running_row(Some(i64::from(child.id())));
        let living: Vec<Look> = (0..2)
            .map(|_| processes.control(&row).map(|(mut c, _)| c.look()))
            .collect::<Option<_>>()
            .ok_or("no control")?;
        drop(child.stdin.take());
        child.wait()?;
        assert_eq!(living, vec![Look::Lost; 2], "its group lives");
        let (mut first, _) = processes.control(&row).ok_or("no control")?;
        assert_eq!(first.look(), Look::Held, "one quiet scan");
        let (mut second, _) = processes.control(&row).ok_or("no control")?;
        assert_eq!(second.look(), Look::Empty);
        Ok(())
    }

    #[test]
    fn a_number_whose_process_lives_in_another_group_is_not_free() -> R {
        // M2T06R-700: an unconfirmed leader alive in another group leaves its group number empty but can make it again:
        // without a cgroup the cancel stays Lost, and a group watched by that number holds.
        use std::os::unix::process::CommandExt as _;
        let cgroup = tempfile::tempdir()?;
        std::fs::write(cgroup.path().join("cgroup.events"), b"populated 0\n")?;
        let root = tree(&[(100, "PATH=/bin")])?;
        let processes = HostProcesses {
            works: BTreeMap::new(),
            executor: None,
            project: PathBuf::from("/nonexistent"),
            project_id: format!("elsewhere-{}", std::process::id()),
            pins: Arc::default(),
            trackings: BTreeMap::new(),
        };
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(rustix::process::getpgrp().as_raw_pid())
            .spawn()?;
        let n = child.id();
        let lost = processes
            .control(&running_row(Some(i64::from(n))))
            .map(|(mut c, _)| c.look());
        let mut watched = AttemptControl {
            group: Watch::Number(n),
            native: None,
            outside: Arc::new(Mutex::new(
                Outside::new("p/A1").with_root(Some(root.path().to_owned())),
            )),
            cgroup: Some(AttemptGroup::witnessed(cgroup.path(), cgroup.path())?),
            term_incomplete: false,
        };
        let held: Vec<Look> = (0..3).map(|_| watched.look()).collect();
        drop(child.stdin.take());
        child.wait()?;
        assert_eq!(lost, Some(Look::Lost));
        assert_eq!(held, vec![Look::Held; 3]);
        Ok(())
    }

    #[test]
    fn a_group_watched_by_its_number_is_empty_only_once_the_kernel_says_so() -> R {
        // M2T06R-500 (decision 8d v7): a group that may not be signalled is watched by its number. While a process has
        // that group number the look holds, however empty the cgroup and quiet the scans; once none has, the proof
        // goes on (two quiet scans).
        use std::os::unix::process::CommandExt as _;
        let cgroup = tempfile::tempdir()?;
        std::fs::write(cgroup.path().join("cgroup.events"), b"populated 0\n")?;
        let root = tree(&[(100, "PATH=/bin")])?;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read -r x"])
            .stdin(std::process::Stdio::piped())
            .process_group(0)
            .spawn()?;
        let pgid = child.id();
        let mut control = AttemptControl {
            group: Watch::Number(pgid),
            native: None,
            outside: Arc::new(Mutex::new(
                Outside::new("p/A1").with_root(Some(root.path().to_owned())),
            )),
            cgroup: Some(AttemptGroup::witnessed(cgroup.path(), cgroup.path())?),
            term_incomplete: false,
        };
        let living: Vec<Look> = (0..3).map(|_| control.look()).collect();
        drop(child.stdin.take());
        child.wait()?;
        assert_eq!(living, vec![Look::Held; 3], "its group lives");
        assert_eq!(control.look(), Look::Held, "one quiet scan");
        assert_eq!(control.look(), Look::Empty);
        Ok(())
    }

    /// A deadline no test reaches.
    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    #[test]
    fn a_huge_untracked_file_is_recorded_by_its_size_once_the_budget_is_spent() -> R {
        // M2T06R-102: a 4 GiB sparse untracked file and a small file; with a 300 ms budget the record is made within
        // a few seconds (hashing it all takes far longer in a debug build), the huge file by its size and modification time, and the count says one was not hashed.
        let dir = tempfile::tempdir()?;
        let project = dir.path();
        git_in(project, &["init", "-q"])?;
        // Listed before the huge one (git lists by name), so hashed within the budget; any file after it is not.
        std::fs::write(project.join("a-small.txt"), b"small")?;
        std::fs::File::create(project.join("huge.bin"))?.set_len(4 << 30)?;
        let started = Instant::now();
        let record = workspace_within(project, Duration::from_millis(300));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        assert!(record.get("error").is_none(), "{record}");
        assert_eq!(record["untracked_unhashed"], 1, "{record}");
        let (entry, hashed) = untracked_entry(&project.join("huge.bin"), Instant::now());
        assert!(!hashed);
        assert!(
            entry.starts_with(&format!("unhashed:{}:", 4u64 << 30)),
            "{entry}"
        );
        std::fs::remove_file(project.join("huge.bin"))?;
        assert_eq!(
            workspace_within(project, Duration::from_secs(60))["untracked_unhashed"],
            0
        );
        Ok(())
    }

    #[test]
    fn the_workspace_record_never_waits_on_a_fifo_and_records_a_link_by_its_target() -> R {
        // M2T06R-06: an untracked FIFO with no writer, and an untracked link to it; before, the record opened the
        // link's target and waited there for a writer.
        let dir = tempfile::tempdir()?;
        let project = dir.path();
        git_in(project, &["init", "-q"])?;
        std::fs::write(project.join("plain.txt"), b"content")?;
        rustix::fs::mknodat(
            rustix::fs::CWD,
            project.join("pipe"),
            rustix::fs::FileType::Fifo,
            rustix::fs::Mode::from_raw_mode(0o600),
            0,
        )?;
        std::os::unix::fs::symlink(project.join("pipe"), project.join("link"))?;
        assert_eq!(untracked_entry(&project.join("pipe"), far()).0, "type:fifo");
        let to_pipe = untracked_entry(&project.join("link"), far()).0;
        assert_eq!(
            to_pipe,
            format!(
                "symlink:{}",
                hex(project.join("pipe").as_os_str().as_bytes())
            )
        );
        assert_eq!(
            untracked_entry(&project.join("plain.txt"), far()).0,
            hex(b"content")
        );
        let (sent, got) = std::sync::mpsc::channel();
        let at = project.to_owned();
        std::thread::spawn(move || {
            let _ = sent.send(workspace(&at));
        });
        let record = got.recv_timeout(Duration::from_secs(20))?;
        assert!(record.get("error").is_none(), "{record}");
        assert!(record["untracked_sha256"].is_string(), "{record}");
        // A link to another target is another record.
        std::fs::remove_file(project.join("link"))?;
        std::os::unix::fs::symlink(project.join("plain.txt"), project.join("link"))?;
        assert_ne!(untracked_entry(&project.join("link"), far()).0, to_pipe);
        assert_ne!(
            workspace(project)["untracked_sha256"],
            record["untracked_sha256"]
        );
        Ok(())
    }
}
