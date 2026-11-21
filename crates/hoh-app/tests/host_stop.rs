//! The in-process product host's stop (docs/adr/ADR-016.md decision 12; ADR-019): once `Host::shutdown` has returned,
//! nothing of the host writes the project any more. A turn its executor started runs on after it (an agent outlives
//! its host), but what the host would have done at that turn's end (its raw logs kept as objects) is not done by a host
//! that has stopped: the next host's reconcile settles the attempt. Found by the M2-T13 test world: a world whose
//! developer turn ended while the world was being removed was left behind (World::drop's report: a new
//! `.hoh/objects/staging/.<pid>.<n>` of the test's own process).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use pty::R;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use world::World;

/// What an agent process writes in its attempt's directory (its bridge, its run-check requests) and the copies of its
/// output that the one exception appends to (docs/adr/ADR-031.md, implementation review round 4, correction 1): an
/// agent outlives its host, so these may still change after the stop; nothing else of the project may.
fn agents(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned());
    path.components().any(|c| c.as_os_str() == "requests")
        || name.is_some_and(|n| {
            [
                "messages.jsonl",
                "stdout.jsonl",
                "stderr.log",
                "app-server.stdout.jsonl",
                "app-server.stderr.log",
                "exec.stdout.jsonl",
                "exec.stderr.log",
            ]
            .contains(&n.as_str())
        })
}

/// What appeared under `dir` between `before` and now, leaving out what agents may still write ([`agents`]).
fn new_since(dir: &Path, before: &[PathBuf]) -> Vec<PathBuf> {
    paths(dir)
        .into_iter()
        .filter(|p| !before.contains(p) && !agents(p))
        .collect()
}

/// Every path under `dir` (none when it does not exist), sorted.
fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path.clone());
            }
            out.push(path);
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_that_ends_after_its_host_stopped_has_nothing_written_for_it() -> R {
    // The product's world: a run whose Developer works 3 s; the host stops while that turn is in flight.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.developer(&world::Developer {
        seconds: 3.0,
        ..world::Developer::default()
    })?;
    world.register()?;
    let host = world.start_host().await?;
    let started = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "s", "完成fixture目标").await?;
        let attempt = format!("{run}-develop/attempt-1");
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'"),
                "RUNNING",
                Duration::from_secs(60),
            )
            .await?;
        // Its turn has begun its work (the fake Developer logged its start).
        let end = Instant::now() + Duration::from_secs(30);
        while !world
            .log("calls.jsonl")?
            .iter()
            .any(|c| c["attempt_id"] == attempt.as_str())
        {
            if Instant::now() > end {
                return Err("the developer turn never started".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        R::Ok(attempt)
    }
    .await;
    host.shutdown().await;
    let attempt = started?;
    let objects = world.project().join(".hoh/objects");
    let at_stop = paths(&objects);
    // The turn ends on its own (its agent outlives the host), then a while for anything that would follow it.
    let end = Instant::now() + Duration::from_secs(30);
    while !world
        .log("turns.jsonl")?
        .iter()
        .any(|t| t["attempt_id"] == attempt.as_str() && t["event"] == "end")
    {
        if Instant::now() > end {
            return Err("the developer turn did not end".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = paths(&objects);
    let new: Vec<&PathBuf> = later.iter().filter(|p| !at_stop.contains(p)).collect();
    assert!(
        new.is_empty(),
        "written into {} after the host stopped: {new:?}",
        objects.display()
    );
    Ok(())
}

/// Holds every writer attempt's completion step (on its blocking thread, before its freeze) until released.
#[derive(Debug, Default)]
struct HoldCompletion {
    held: std::sync::atomic::AtomicBool,
    release: std::sync::atomic::AtomicBool,
}

impl hoh_app::host::HostHooks for HoldCompletion {
    fn completing(&self, _attempt_id: &str) {
        use std::sync::atomic::Ordering::SeqCst;
        self.held.store(true, SeqCst);
        // Never longer than a minute, whatever the test does.
        let end = Instant::now() + Duration::from_secs(60);
        while !self.release.load(SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_an_attempt_began_on_a_blocking_thread_ends_before_its_host_stop_returns() -> R {
    // The product's world: a run whose Developer completes at once; its completion (the freeze of its candidate, on a
    // blocking thread) is held while the host stops. The stop aborts the attempt's task; the step it began still runs.
    // The stop returns only once it has ended, and nothing is written into the project after it returned.
    use std::sync::atomic::Ordering::SeqCst;
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.register()?;
    let hold = std::sync::Arc::new(HoldCompletion::default());
    let mut config = world.host_config()?;
    config.hooks = hold.clone();
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        let end = Instant::now() + Duration::from_secs(60);
        while !hold.held.load(SeqCst) {
            if Instant::now() > end {
                return Err("no completion was held".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        R::Ok(())
    }
    .await;
    let stopping = tokio::spawn(host.shutdown());
    tokio::time::sleep(Duration::from_secs(1)).await;
    let returned_early = stopping.is_finished();
    hold.release.store(true, SeqCst);
    stopping.await?;
    started?;
    let hoh = world.project().join(".hoh");
    let at_return = paths(&hoh);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = paths(&hoh);
    let new: Vec<&PathBuf> = later.iter().filter(|p| !at_return.contains(p)).collect();
    assert!(
        !returned_early && new.is_empty(),
        "the stop returned while the step ran: {returned_early}; written into {} after it returned: {new:?}",
        hoh.display()
    );
    Ok(())
}

/// Holds `point`'s step (on its blocking thread) once it is reached, until released, a minute at most.
#[derive(Debug, Default)]
struct Hold {
    held: std::sync::atomic::AtomicBool,
    release: std::sync::atomic::AtomicBool,
}

impl Hold {
    fn hold(&self) {
        use std::sync::atomic::Ordering::SeqCst;
        self.held.store(true, SeqCst);
        let end = Instant::now() + Duration::from_secs(60);
        while !self.release.load(SeqCst) && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    async fn reached(&self, limit: Duration) -> R {
        let end = Instant::now() + limit;
        while !self.held.load(std::sync::atomic::Ordering::SeqCst) {
            if Instant::now() > end {
                return Err("the held step was never reached".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Ok(())
    }
}

/// Holds the commit of a candidate's pending check records that a verifier's check runs first.
#[derive(Debug, Default)]
struct HoldReconcile(std::sync::Arc<Hold>);

impl hoh_app::host::HostHooks for HoldReconcile {
    fn reconciling(&self, _candidate_id: &str) {
        self.0.hold();
    }
}

/// The host stops while `hold`'s step is held; `release` after `after`: whether the stop had returned by then, and
/// what appeared under the project's `.hoh` after it returned (in 2 s, agents' own writes left out).
async fn stop_while_held(
    world: &World,
    host: hoh_app::host::Host,
    hold: &Hold,
    after: Duration,
) -> R<(bool, bool, Vec<PathBuf>)> {
    use std::sync::atomic::Ordering::SeqCst;
    let stopping = tokio::spawn(host.shutdown());
    tokio::time::sleep(after).await;
    let returned_early = stopping.is_finished();
    hold.release.store(true, SeqCst);
    let clean = stopping.await?;
    let hoh = world.project().join(".hoh");
    let at_return = paths(&hoh);
    tokio::time::sleep(Duration::from_secs(2)).await;
    Ok((returned_early, clean, new_since(&hoh, &at_return)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_commit_of_pending_check_records_at_the_stop_ends_before_the_stop_returns() -> R {
    // M2T13R-350: QA runs the planned check; its run first commits the candidate's pending records in a task of its
    // own (check::reconcile, M2T10R-201), held here while the host stops. That task finishes what it records, so the
    // stop waits for it, and nothing appears in the project after the stop returned.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.verifier(
        "qa",
        &world::Verifier {
            checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
            ..world::Verifier::default()
        },
    )?;
    world.register()?;
    let hold = std::sync::Arc::new(HoldReconcile::default());
    let mut config = world.host_config()?;
    config.hooks = hold.clone();
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        hold.0.reached(Duration::from_secs(90)).await
    }
    .await;
    let (returned_early, clean, new) =
        stop_while_held(&world, host, &hold.0, Duration::from_secs(1)).await?;
    started?;
    assert!(
        !returned_early && clean && new.is_empty(),
        "returned while the commit was held: {returned_early}; clean: {clean}; new after the return: {new:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_turns_logs_being_kept_at_the_stop_are_kept_before_the_stop_returns() -> R {
    // M2T13R-351: the Developer's turn has ended and its raw logs are being kept as objects (held here) when the host
    // stops, past the executor's own 10 s for its turn watchers: the stop still waits for the copy, and nothing
    // appears in the project after it returned.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.register()?;
    let hold = std::sync::Arc::new(Hold::default());
    let mut config = world.host_config()?;
    let runner = config.runner.as_mut().ok_or("no runner")?;
    let registry = runner.executor.registry().ok_or("no registry")?;
    let held = hold.clone();
    runner.executor = std::sync::Arc::new(
        hoh_app::host::product::ProductExecutor::new(
            registry,
            world.project(),
            world::HOH.into(),
            (config.reconcile.native_grace, config.reconcile.term_grace),
        )
        .with_keeping(std::sync::Arc::new(move |attempt: &str| {
            if attempt.ends_with("-develop/attempt-1") {
                held.hold();
            }
        })),
    );
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        hold.reached(Duration::from_secs(90)).await
    }
    .await;
    let (returned_early, clean, new) =
        stop_while_held(&world, host, &hold, Duration::from_secs(12)).await?;
    started?;
    assert!(
        !returned_early && clean && new.is_empty(),
        "returned while the logs were kept: {returned_early}; clean: {clean}; new after the return: {new:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_running_at_the_stop_is_cancelled_and_still_recorded_before_the_stop_returns() -> R
{
    // M2T13R-352: QA's check runs a command of two minutes when the host stops. The stop aborts the attempt's task, so
    // the check's run is cancelled (TERM, then KILL after 5 s); its finishing task still writes its record (ADR-027
    // decision 6), and the stop returns once that is done, long before the command would have ended, cleanly, with
    // nothing appearing in the project after it.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    let plan = world.project().join("tests/check-plan.json");
    let mut checks: serde_json::Value = serde_json::from_slice(&std::fs::read(&plan)?)?;
    checks["checks"]["slow"] = serde_json::json!({"mode": "product", "timeout_seconds": 300,
        "argv": ["sh", "-c", "echo started > \"$HOH_EVIDENCE_DIR/started\"; exec sleep 120"]});
    std::fs::write(
        &plan,
        format!("{}\n", serde_json::to_string_pretty(&checks)?),
    )?;
    world.git(&["commit", "-qam", "a slow check"])?;
    world.verifier(
        "qa",
        &world::Verifier {
            checks: vec!["slow".to_owned()],
            ..world::Verifier::default()
        },
    )?;
    world.register()?;
    let host = world.start_host().await?;
    let evidence = world.project().join(".hoh/evidence");
    let running = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        let end = Instant::now() + Duration::from_secs(90);
        loop {
            if let Some(started) = paths(&evidence)
                .into_iter()
                .find(|p| p.ends_with("artifacts/started"))
            {
                return R::Ok(started);
            }
            if Instant::now() > end {
                return Err("the slow check never started".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
    .await;
    let begun = Instant::now();
    let clean = host.shutdown().await;
    let took = begun.elapsed();
    let started = running?;
    let hoh = world.project().join(".hoh");
    let at_return = paths(&hoh);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let new = new_since(&hoh, &at_return);
    let record = started
        .parent()
        .and_then(Path::parent)
        .ok_or("no record directory")?;
    assert!(clean, "the stop was not clean");
    assert!(took < Duration::from_secs(25), "the stop took {took:?}");
    assert!(
        record.join("execution.json").is_file(),
        "the cancelled check's record was not written: {:?}",
        paths(record)
    );
    assert!(new.is_empty(), "new after the return: {new:?}");
    Ok(())
}

/// Whether the project's lock is held (docs/adr/ADR-016.md decision 12: an exclusive `flock` on its `.hoh` directory):
/// one taken through a description of this test's own is refused while the old host keeps it.
fn locked(world: &World) -> R<bool> {
    let dir = std::fs::File::open(world.project().join(".hoh"))?;
    match rustix::fs::flock(&dir, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => {
            rustix::fs::flock(&dir, rustix::fs::FlockOperation::Unlock)?;
            Ok(false)
        }
        Err(rustix::io::Errno::WOULDBLOCK) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

/// What a stop past its bound showed: its status and how long it took, whether the project was locked when it returned
/// and still a moment before the held step was released, whether it was let go once the step ended (15 s at most), and
/// what appeared under `.hoh` in the 2 s after that (agents' own writes left out).
#[derive(Debug)]
struct PastTheBound {
    clean: bool,
    took: Duration,
    locked_at_return: bool,
    locked_while_held: bool,
    let_go: bool,
    new_after_let_go: Vec<PathBuf>,
}

/// Stop `host` while a step is held, released (`release`) `held_for` after the stop began.
async fn stop_past_the_bound(
    world: &World,
    host: hoh_app::host::Host,
    release: std::sync::Arc<Hold>,
    held_for: Duration,
) -> R<PastTheBound> {
    use std::sync::atomic::Ordering::SeqCst;
    let began = Instant::now();
    let releasing = {
        let release = release.clone();
        std::thread::spawn(move || {
            std::thread::sleep(held_for);
            release.release.store(true, SeqCst);
        })
    };
    let clean = host.shutdown().await;
    let took = began.elapsed();
    let locked_at_return = locked(world)?;
    tokio::time::sleep(
        (began + held_for - Duration::from_secs(1)).saturating_duration_since(Instant::now()),
    )
    .await;
    let locked_while_held = locked(world)? && !release.release.load(SeqCst);
    releasing.join().map_err(|_| "the releaser panicked")?;
    let end = Instant::now() + Duration::from_secs(15);
    let let_go = loop {
        if !locked(world)? {
            break true;
        }
        if Instant::now() > end {
            break false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let hoh = world.project().join(".hoh");
    let at_let_go = paths(&hoh);
    tokio::time::sleep(Duration::from_secs(2)).await;
    Ok(PastTheBound {
        clean,
        took,
        locked_at_return,
        locked_while_held,
        let_go,
        new_after_let_go: new_since(&hoh, &at_let_go),
    })
}

/// Holds every writer attempt's completion step, as [`HoldCompletion`] does, with [`Hold`]'s release.
#[derive(Debug, Default)]
struct HoldCompleting(std::sync::Arc<Hold>);

impl hoh_app::host::HostHooks for HoldCompleting {
    fn completing(&self, _attempt_id: &str) {
        self.0.hold();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_step_past_the_stops_bound_keeps_the_project_locked_and_the_stop_is_not_clean() -> R {
    // M2T13R-352, M2T13R-300 (docs/adr/ADR-016.md decision 12): a writer attempt's completion step (on its blocking
    // thread, holding no handle of the store) is held past the stop's bound (2 s here). The stop returns at the bound
    // and is not clean; the project stays locked while the step runs, and is let go only once it ended.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.register()?;
    let hold = std::sync::Arc::new(Hold::default());
    let mut config = world.host_config()?;
    config.hooks = std::sync::Arc::new(HoldCompleting(hold.clone()));
    config.stop_bound = Duration::from_secs(2);
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        hold.reached(Duration::from_secs(90)).await
    }
    .await;
    let stopped = stop_past_the_bound(&world, host, hold, Duration::from_secs(6)).await?;
    started?;
    assert!(
        !stopped.clean
            && stopped.took >= Duration::from_secs(2)
            && stopped.took < Duration::from_secs(5)
            && stopped.locked_at_return
            && stopped.locked_while_held
            && stopped.let_go
            && stopped.new_after_let_go.is_empty(),
        "{stopped:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn logs_kept_past_both_bounds_keep_the_project_locked_and_the_stop_is_not_clean() -> R {
    // M2T13R-300 (docs/adr/ADR-016.md decision 12): a finished turn's raw logs are being kept as objects (held here,
    // a step holding no handle of the store) past the executor's 10 s for its turn watchers, which then aborts the
    // watcher, and past the stop's bound (2 s here). The stop is not clean and returns before the copy ended; the
    // project stays locked while it runs, and is let go only once it ended.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.register()?;
    let hold = std::sync::Arc::new(Hold::default());
    let mut config = world.host_config()?;
    config.stop_bound = Duration::from_secs(2);
    let runner = config.runner.as_mut().ok_or("no runner")?;
    let registry = runner.executor.registry().ok_or("no registry")?;
    let held = hold.clone();
    runner.executor = std::sync::Arc::new(
        hoh_app::host::product::ProductExecutor::new(
            registry,
            world.project(),
            world::HOH.into(),
            (config.reconcile.native_grace, config.reconcile.term_grace),
        )
        .with_keeping(std::sync::Arc::new(move |attempt: &str| {
            if attempt.ends_with("-develop/attempt-1") {
                held.hold();
            }
        })),
    );
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        hold.reached(Duration::from_secs(90)).await
    }
    .await;
    let stopped = stop_past_the_bound(&world, host, hold, Duration::from_secs(20)).await?;
    started?;
    assert!(
        !stopped.clean
            && stopped.took < Duration::from_secs(18)
            && stopped.locked_at_return
            && stopped.locked_while_held
            && stopped.let_go
            && stopped.new_after_let_go.is_empty(),
        "{stopped:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stop_past_its_bound_returns_at_it_even_when_what_still_runs_holds_the_store() -> R {
    // M2T13R-352: what still runs past the bound is a finishing task that holds the store (the commit of pending check
    // records, held here 8 s; the bound 2 s): the stop does not wait for the store's writer, which cannot end before
    // it does; it returns at its bound, is not clean, and the project stays locked until the task ended.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    // Its [assumptions] lists the pinned Commander's basis: the run StartRun makes is unattended (REQ-030).
    world.configure(&[], "")?;
    world.verifier(
        "qa",
        &world::Verifier {
            checks: vec![world::fixture_spec()[0].criteria[0].check_id()],
            ..world::Verifier::default()
        },
    )?;
    world.register()?;
    let hold = std::sync::Arc::new(Hold::default());
    let mut config = world.host_config()?;
    config.hooks = std::sync::Arc::new(HoldReconcile(hold.clone()));
    config.stop_bound = Duration::from_secs(2);
    let host = hoh_app::host::Host::start(config).await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, "s", "完成fixture目标").await?;
        hold.reached(Duration::from_secs(90)).await
    }
    .await;
    let stopped = stop_past_the_bound(&world, host, hold, Duration::from_secs(8)).await?;
    started?;
    assert!(
        !stopped.clean
            && stopped.took < Duration::from_secs(6)
            && stopped.locked_at_return
            && stopped.locked_while_held
            && stopped.let_go
            && stopped.new_after_let_go.is_empty(),
        "{stopped:?}"
    );
    Ok(())
}
