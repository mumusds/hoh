//! Everything the host process starts and must account for at its stop (docs/adr/ADR-031.md, implementation record,
//! implementation review round 4: the design correction of the process lifetime at the stop). The host's served tasks
//! (HTTP and its connections, the runner, pause and cancel) run in the host's [`Begun`] scope; every task or blocking
//! step they start goes through [`spawn`], [`spawn_finishing`] or [`spawn_blocking`], and is counted while it runs.
//! The host's Begun keeps the host's shared state, and every counted task and step keeps the Begun: the store, whose
//! writer holds the project's lock until its last handle is gone (ADR-016 decision 12), cannot close while any of them
//! runs, whatever the stop waited for. [`Begun::stop`], once
//! the served tasks have ended, aborts every counted task [`spawn`] started (an aborted future writes nothing after its
//! abort; what it began on a blocking thread is still counted), lets the finishing tasks finish (a check's run, which
//! its owner's drop cancels, and its record; the commit of pending check records), and waits until nothing counted
//! runs, a bound at most: then nothing the host started writes the project. A blocking step runs with the scope set
//! for its thread, so what it starts is counted too. Outside a scope (the client, a test without a host) the three are
//! tokio's own. A scan test keeps every spawn of the host's code routed here or allowed with its reason.

use std::cell::RefCell;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::task::{AbortHandle, JoinHandle};

/// What one host started and still runs.
#[derive(Debug, Default)]
pub(crate) struct Begun {
    /// How many counted tasks and blocking steps run.
    running: AtomicUsize,
    /// The counted tasks [`spawn`] started, aborted at the stop.
    abortable: Mutex<Vec<AbortHandle>>,
    /// What this keeps alive as long as the host or any counted task or step holds it ([`Begun::keeping`]).
    _keeps: Option<Box<dyn std::any::Any + Send + Sync>>,
}

tokio::task_local! {
    static BEGUN: Arc<Begun>;
}

thread_local! {
    /// The scope of the blocking step this thread runs, if any.
    static ON_THREAD: RefCell<Option<Arc<Begun>>> = const { RefCell::new(None) };
}

/// One counted task or step, while it lives.
struct Running(Arc<Begun>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A blocking step's scope set on its thread, given back when it ends (the thread is the pool's).
struct OnThread(Option<Arc<Begun>>);

impl OnThread {
    fn set(begun: Option<Arc<Begun>>) -> OnThread {
        OnThread(ON_THREAD.with(|t| t.replace(begun)))
    }
}

impl Drop for OnThread {
    fn drop(&mut self) {
        let before = self.0.take();
        ON_THREAD.with(|t| *t.borrow_mut() = before);
    }
}

/// The scope here: the task's, else the blocking step's on this thread.
fn current() -> Option<Arc<Begun>> {
    BEGUN
        .try_with(Arc::clone)
        .ok()
        .or_else(|| ON_THREAD.with(|t| t.borrow().clone()))
}

impl Begun {
    /// A host's: `keeps` (its shared state, with the store) lives until the host and every counted task and step have
    /// let go of this.
    pub(crate) fn keeping(keeps: Box<dyn std::any::Any + Send + Sync>) -> Begun {
        Begun {
            _keeps: Some(keeps),
            ..Begun::default()
        }
    }

    fn enter(self: &Arc<Self>) -> Running {
        self.running.fetch_add(1, Ordering::SeqCst);
        Running(self.clone())
    }

    /// How many counted tasks and steps run.
    pub(crate) fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// `task` with this as its scope: what it starts through this module is counted here.
    pub(crate) async fn scope<F: Future>(self: Arc<Self>, task: F) -> F::Output {
        BEGUN.scope(self, task).await
    }

    fn abort_all(&self) {
        let tasks = std::mem::take(
            &mut *self
                .abortable
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for task in tasks {
            task.abort();
        }
    }

    /// The host's stop, once its served tasks have ended: every counted task [`spawn`] started is aborted (and any one
    /// started meanwhile), then nothing counted may run any more; waited for `bound` at most: how many still run.
    pub(crate) async fn stop(&self, bound: Duration) -> usize {
        let end = Instant::now() + bound;
        loop {
            self.abort_all();
            let running = self.running();
            if running == 0 || Instant::now() >= end {
                return running;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// `step` on a blocking thread (`tokio::task::spawn_blocking`), counted while it runs and with the scope set on its
/// thread, when it is started in a scope.
pub(crate) fn spawn_blocking<F, T>(step: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let begun = current();
    let running = begun.as_ref().map(Begun::enter);
    tokio::task::spawn_blocking(move || {
        let _running = running;
        let _scope = OnThread::set(begun);
        step()
    })
}

/// `task` in the scope here, if any, for a task whose starter joins it itself (an HTTP connection, which the server
/// ends after its grace and joins before it returns): what it starts is counted, it is not.
pub(crate) fn scoped<F: Future>(task: F) -> impl Future<Output = F::Output> {
    let begun = current();
    async move {
        match begun {
            Some(begun) => BEGUN.scope(begun, task).await,
            None => task.await,
        }
    }
}

/// `task` spawned (`tokio::spawn`) in the same scope, counted while it runs and aborted at the host's stop, when it is
/// started in a scope.
pub(crate) fn spawn<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let Some(begun) = current() else {
        return tokio::spawn(task);
    };
    let handle = counted(begun.clone(), task);
    let mut abortable = begun
        .abortable
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    abortable.retain(|t| !t.is_finished());
    abortable.push(handle.abort_handle());
    handle
}

/// `task` spawned as [`spawn`] does, but never aborted at the stop: a task that finishes what it records once its owner
/// is gone (a check's run, which its owner's drop cancels, and its record; the commit of pending check records), whose
/// waits are its own child, the file system and the store's writer, never anything only a running host gives.
pub(crate) fn spawn_finishing<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match current() {
        Some(begun) => counted(begun, task),
        None => tokio::spawn(task),
    }
}

fn counted<F>(begun: Arc<Begun>, task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let running = begun.enter();
    tokio::spawn(BEGUN.scope(begun, async move {
        let _running = running;
        task.await
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_step_begun_in_a_scope_is_counted_until_it_ends_even_once_its_task_is_aborted() {
        let begun = Arc::new(Begun::default());
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (entered, step_began) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(begun.clone().scope(async move {
            let step = spawn_blocking(move || {
                let _ = entered.send(());
                let _ = gate.recv();
            });
            let _ = step.await;
        }));
        step_began.await.expect("the step began");
        assert_eq!(begun.running(), 1);
        task.abort();
        let _ = task.await;
        // Its task is gone; the step still runs, and is still counted: the stop waits for it.
        assert_eq!(begun.stop(Duration::from_millis(100)).await, 1);
        go.send(()).expect("the step waits");
        assert_eq!(begun.stop(Duration::from_secs(5)).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_stop_aborts_a_counted_task_and_what_a_step_started_but_lets_a_finishing_one_finish()
     {
        let begun = Arc::new(Begun::default());
        let (_never, forever) = tokio::sync::oneshot::channel::<()>();
        let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let (_kept, held) = tokio::sync::oneshot::channel::<()>();
        begun
            .clone()
            .scope(async move {
                // A task that would wait for ever.
                drop(spawn(async move {
                    let _ = forever.await;
                }));
                // One a blocking step starts: counted under the step's scope, aborted as well.
                spawn_blocking(move || {
                    drop(spawn(async move {
                        let _ = held.await;
                    }));
                })
                .await
                .expect("the step ran");
                // A finishing task: never aborted, waited for.
                drop(spawn_finishing(async move {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    done.store(true, Ordering::SeqCst);
                }));
            })
            .await;
        assert_eq!(begun.running(), 3);
        let started = Instant::now();
        assert_eq!(begun.stop(Duration::from_secs(5)).await, 0);
        assert!(
            finished.load(Ordering::SeqCst),
            "the finishing task was cut short"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_lock_poisoned_by_a_panic_loses_neither_a_count_nor_an_abort() {
        let begun = Arc::new(Begun::default());
        let poisoner = begun.clone();
        let _ = std::thread::spawn(move || {
            let _held = poisoner.abortable.lock();
            panic!("poisoned");
        })
        .join();
        assert!(begun.abortable.is_poisoned());
        let (_never, forever) = tokio::sync::oneshot::channel::<()>();
        begun
            .clone()
            .scope(async move {
                drop(spawn(async move {
                    let _ = forever.await;
                }));
            })
            .await;
        assert_eq!(
            begun.running(),
            1,
            "a task begun after the poisoning is not counted"
        );
        assert_eq!(
            begun.stop(Duration::from_secs(2)).await,
            0,
            "a task begun after the poisoning is not aborted"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_a_begun_keeps_lives_until_its_last_counted_step_ended_whatever_the_stop_waited_for()
     {
        /// Says when it is dropped.
        struct Kept(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Kept {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let begun = Arc::new(Begun::keeping(Box::new(Kept(dropped.clone()))));
        let (go, gate) = std::sync::mpsc::channel::<()>();
        let (entered, step_began) = tokio::sync::oneshot::channel::<()>();
        let (_never, forever) = tokio::sync::oneshot::channel::<()>();
        begun
            .clone()
            .scope(async move {
                // A step that holds nothing of what is kept, and a task that would wait for ever.
                drop(spawn_blocking(move || {
                    let _ = entered.send(());
                    let _ = gate.recv();
                }));
                drop(spawn(async move {
                    let _ = forever.await;
                }));
                // A task an HTTP server would join itself: in the scope, not counted.
                scoped(async { assert!(current().is_some()) }).await;
            })
            .await;
        step_began.await.expect("the step began");
        // The stop gives up on the step past its bound, and the host lets go of its Begun: what it keeps stays.
        assert_eq!(begun.stop(Duration::from_millis(100)).await, 1);
        drop(begun);
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !dropped.load(Ordering::SeqCst),
            "let go while a counted step ran"
        );
        go.send(()).expect("the step waits");
        let end = Instant::now() + Duration::from_secs(5);
        while !dropped.load(Ordering::SeqCst) {
            assert!(Instant::now() < end, "kept after its last step ended");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn outside_a_scope_the_three_are_tokios_own() {
        let begun = Arc::new(Begun::default());
        spawn_blocking(|| ()).await.expect("ran");
        spawn(async {}).await.expect("ran");
        spawn_finishing(async {}).await.expect("ran");
        assert_eq!(begun.running(), 0);
        // A blocking step's scope does not outlive it on its thread.
        let begun2 = begun.clone();
        begun2
            .scope(async {
                spawn_blocking(|| ()).await.expect("ran");
            })
            .await;
        for _ in 0..8 {
            spawn_blocking(|| assert!(current().is_none(), "a scope left on a pool thread"))
                .await
                .expect("ran");
        }
        assert_eq!(begun.stop(Duration::from_secs(1)).await, 0);
    }
}

#[cfg(test)]
mod scan {
    //! docs/adr/ADR-031.md, implementation review round 4, correction 2: every spawn the host process runs goes through
    //! [`super`] or is on [`ALLOWED`] with its reason; any other is this test's failure, not a review's finding.

    use std::path::{Path, PathBuf};

    /// The spawns that do not go through `host::begun`: (file under `crates/`, a fragment of its line, how many such
    /// lines, why).
    const ALLOWED: &[(&str, &str, usize, &str)] = &[
        (
            "hoh-app/src/host/mod.rs",
            "http::serve(",
            2,
            "a served task (HTTP): Host::shutdown joins it; it runs in the host's Begun scope",
        ),
        (
            "hoh-app/src/host/mod.rs",
            "runner::run(",
            1,
            "a served task (the runner): Host::shutdown joins it; it runs in the host's Begun scope",
        ),
        (
            "hoh-app/src/host/mod.rs",
            "control::run(",
            1,
            "a served task (pause and cancel): Host::shutdown joins it; it runs in the host's Begun scope",
        ),
        (
            "hoh-app/src/host/mod.rs",
            "tokio::spawn(async move {",
            1,
            "run_ready's SIGHUP logger: writes stderr only, lives as long as the host process",
        ),
        (
            "hoh-app/src/host/http.rs",
            "connections.spawn(super::begun::scoped(",
            1,
            "an HTTP connection: serve joins it (ended after the grace) before it returns, and Host::shutdown joins \
             serve; it runs in the host's Begun scope (begun::scoped), so what a request starts is counted",
        ),
        (
            "hoh-app/src/host/suites.rs",
            "let run = self.spawn(agent, &ready, registry);",
            1,
            "a method of the same file (a suite's run), whose own spawns are scanned",
        ),
        (
            "hoh-app/src/host/control.rs",
            "let reader = std::thread::spawn(",
            1,
            "reads a git command's stdout; its caller joins it before returning",
        ),
        (
            "hoh-core/src/persistence/writer.rs",
            "std::thread::Builder::new()",
            1,
            "the store's writer thread: it ends once every handle of the store is gone (the host's Begun keeps one \
             while any counted task or step runs) and lets the project's lock go then",
        ),
        (
            "hoh-core/src/persistence/writer.rs",
            ".spawn(move || {",
            1,
            "the same writer thread's Builder chain",
        ),
        (
            "hoh-core/src/persistence/selftest.rs",
            "std::thread::spawn(",
            1,
            "reads the WAL self-test probe's stdout at the store's opening (the host's start); writes nothing",
        ),
        (
            "hoh-core/src/cancel.rs",
            "tokio::task::spawn_blocking(",
            3,
            "a cancel's pass (looks and signals), its workspace record and its pending candidates: reads and \
             signals, never writes the project",
        ),
        (
            "hoh-agents/src/process.rs",
            "tokio::spawn(read_stdout(",
            1,
            "the one exception (correction 1): a process's stdout copy, appended to the file made at its spawn \
             until its pipe closes",
        ),
        (
            "hoh-agents/src/process.rs",
            "tokio::spawn(drain_stderr(",
            1,
            "the one exception (correction 1): a process's stderr copy, appended to the file made at its spawn \
             until its pipe closes",
        ),
        (
            "hoh-agents/src/process.rs",
            "tokio::spawn(supervise(",
            1,
            "reaps a process's leader and waits for its copies; writes nothing",
        ),
        (
            "hoh-agents/src/claude.rs",
            "let ended = tokio::spawn(",
            1,
            "a claude turn's driver: writes only the agent's stdin",
        ),
        (
            "hoh-agents/src/codex.rs",
            "let ended = tokio::spawn(",
            1,
            "a codex turn's driver: writes only the agent's stdin",
        ),
        (
            "hoh-agents/src/pi.rs",
            "let ended = tokio::spawn(",
            1,
            "a pi turn's driver: writes only the agent's stdin",
        ),
        (
            "hoh-agents/src/codex/app_server.rs",
            "let mut writer = tokio::spawn(",
            1,
            "the codex App Server's stdin writer",
        ),
        (
            "hoh-agents/src/pi.rs",
            "let mut writer = tokio::spawn(",
            1,
            "pi's RPC stdin writer",
        ),
        (
            "hoh-agents/src/probe.rs",
            "std::thread::Builder::new()",
            2,
            "a probe's schema worker (writes only its scratch in the system temp directory) and the reaper of a \
             probe's child (writes nothing)",
        ),
        (
            "hoh-agents/src/probe.rs",
            ".spawn(work)",
            1,
            "the schema worker's Builder chain",
        ),
        (
            "hoh-agents/src/probe.rs",
            ".spawn(move || {",
            1,
            "the child reaper's Builder chain",
        ),
    ];

    /// What starts a task or a thread, besides any `.spawn(` with an argument ([`spawns`]).
    const SPAWNS: [&str; 5] = [
        "tokio::spawn(",
        "task::spawn(",
        "task::spawn_blocking(",
        "thread::spawn(",
        "thread::Builder::new(",
    ];

    /// Whether `code` starts a task or a thread: one of [`SPAWNS`], or a `.spawn(` with an argument (a `JoinSet`'s, a
    /// scoped thread's, a thread `Builder`'s; a process's `.spawn()` takes none).
    fn spawns(code: &str) -> bool {
        SPAWNS.iter().any(|s| code.contains(s))
            || code
                .match_indices(".spawn(")
                .any(|(at, _)| !code[at + ".spawn(".len()..].starts_with(')'))
    }

    /// The production source files of `dir`: no `tests.rs`, nothing under a `tests` directory, not this router.
    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a scanned tree").flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "tests" {
                    sources(&path, out);
                }
            } else if name.ends_with(".rs")
                && name != "tests.rs"
                && !path.ends_with("host/begun.rs")
            {
                out.push(path);
            }
        }
    }

    /// `source` without its inline test module (`#[cfg(test)]` then `mod <name> {`, to the end).
    fn production(source: &str) -> &str {
        let mut at = 0;
        while let Some(found) = source[at..].find("\n#[cfg(test)]\nmod ") {
            let start = at + found;
            let rest = &source[start + "\n#[cfg(test)]\nmod ".len()..];
            if rest
                .split('\n')
                .next()
                .is_some_and(|l| l.trim_end().ends_with('{'))
            {
                return &source[..start];
            }
            at = start + 1;
        }
        source
    }

    #[test]
    fn every_spawn_the_host_runs_goes_through_begun_or_is_allowed_with_its_reason() {
        let crates = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crates directory");
        let mut files = Vec::new();
        for tree in [
            "hoh-app/src/host",
            "hoh-app/src/verify",
            "hoh-agents/src",
            "hoh-core/src",
        ] {
            sources(&crates.join(tree), &mut files);
        }
        files.sort();
        assert!(files.len() > 40, "the scan lost its files: {files:?}");
        let mut used = vec![0usize; ALLOWED.len()];
        let mut unrouted = Vec::new();
        for file in &files {
            let name = file
                .strip_prefix(crates)
                .expect("under crates")
                .to_string_lossy()
                .into_owned();
            let source = std::fs::read_to_string(file).expect("a source file");
            for (n, line) in production(&source).lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") || !spawns(code) {
                    continue;
                }
                match ALLOWED
                    .iter()
                    .position(|(path, fragment, _, _)| *path == name && code.contains(fragment))
                {
                    Some(i) => used[i] += 1,
                    None => unrouted.push(format!("{name}:{}: {code}", n + 1)),
                }
            }
        }
        let stale: Vec<String> = ALLOWED
            .iter()
            .zip(&used)
            .filter(|((_, _, count, _), used)| *count != **used)
            .map(|((path, fragment, count, _), used)| {
                format!("{path} `{fragment}`: {count} allowed, {used} found")
            })
            .collect();
        assert!(
            unrouted.is_empty() && stale.is_empty(),
            "spawns not through host::begun nor allowed:\n{}\nallowlist entries that do not match:\n{}",
            unrouted.join("\n"),
            stale.join("\n")
        );
    }
}
