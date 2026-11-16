//! What the runner's attempt tasks begin that their abort at the host's stop does not end: the steps they run on a
//! blocking thread (a freeze, a completion's outcome, a run directory's preparation, its post-check, a request's
//! answer, a check's record) and the tasks they spawn to finish one (a check's run and record). The host's stop aborts
//! its attempt tasks (their processes run on), and an aborted task no longer waits for such a step, which runs to its
//! end and writes the project. Each is counted while it runs, under the attempt task's [`Begun`] (a task-local scope
//! the runner opens for each attempt task); after aborting them the runner's stop waits until none runs ([`drained`]),
//! so nothing of an attempt writes the project after `Host::shutdown` has returned. Outside an attempt task's scope
//! [`spawn_blocking`] and [`spawn`] are tokio's own.
//!
//! [`drained`]: Begun::drained

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

/// The steps begun under one host's attempt tasks and still running.
#[derive(Debug, Default)]
pub(crate) struct Begun {
    running: Mutex<usize>,
}

tokio::task_local! {
    static BEGUN: Arc<Begun>;
}

/// One step counted while it lives.
struct Running(Arc<Begun>);

impl Drop for Running {
    fn drop(&mut self) {
        if let Ok(mut n) = self.0.running.lock() {
            *n = n.saturating_sub(1);
        }
    }
}

impl Begun {
    fn enter(self: &Arc<Self>) -> Running {
        if let Ok(mut n) = self.running.lock() {
            *n += 1;
        }
        Running(self.clone())
    }

    /// How many steps still run.
    pub(crate) fn running(&self) -> usize {
        self.running.lock().map_or(0, |n| *n)
    }

    /// `task` with this as its scope: what it begins through [`spawn_blocking`] and [`spawn`] is counted here.
    pub(crate) async fn scope<F: Future>(self: Arc<Self>, task: F) -> F::Output {
        BEGUN.scope(self, task).await
    }

    /// Wait until no step runs, `bound` at most: how many still run then.
    pub(crate) async fn drained(&self, bound: Duration) -> usize {
        let end = Instant::now() + bound;
        loop {
            let running = self.running();
            if running == 0 || Instant::now() >= end {
                return running;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// `step` on a blocking thread (`tokio::task::spawn_blocking`), counted while it runs when it is begun in an attempt
/// task's scope.
pub(crate) fn spawn_blocking<F, T>(step: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    let running = BEGUN.try_with(Begun::enter).ok();
    tokio::task::spawn_blocking(move || {
        let _running = running;
        step()
    })
}

/// `task` spawned (`tokio::spawn`), counted while it runs and in the same scope (what it begins is counted too) when
/// it is spawned in an attempt task's scope.
pub(crate) fn spawn<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match BEGUN.try_with(Arc::clone) {
        Ok(begun) => {
            let running = begun.enter();
            tokio::spawn(BEGUN.scope(begun, async move {
                let _running = running;
                task.await
            }))
        }
        Err(_) => tokio::spawn(task),
    }
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
        // Its task is gone; the step still runs, and is still counted.
        assert_eq!(begun.drained(Duration::from_millis(100)).await, 1);
        go.send(()).expect("the step waits");
        assert_eq!(begun.drained(Duration::from_secs(5)).await, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_spawned_task_and_what_it_begins_are_counted_and_nothing_outside_a_scope() {
        let begun = Arc::new(Begun::default());
        let (go, gate) = tokio::sync::oneshot::channel::<()>();
        let (sent, spawned) = std::sync::mpsc::channel();
        begun
            .clone()
            .scope(async move {
                let _ = sent.send(spawn(async move {
                    let _ = gate.await;
                    spawn_blocking(|| ()).await
                }));
            })
            .await;
        let spawned = spawned.recv().expect("spawned");
        assert_eq!(begun.running(), 1);
        let _ = go.send(());
        let _ = spawned.await;
        assert_eq!(begun.drained(Duration::from_secs(5)).await, 0);
        // Outside any scope: tokio's own, counted nowhere.
        spawn_blocking(|| ()).await.expect("ran");
        assert_eq!(begun.running(), 0);
    }
}
