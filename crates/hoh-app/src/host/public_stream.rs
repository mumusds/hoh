//! Public storage runs beside process control. A queue reservation lasts through commit, including DB_BUSY.
use super::{
    http::Shared,
    runner::{Executor, Work},
};
use hoh_core::persistence::PersistError;
use std::{sync::Arc, time::Duration};
pub(super) struct Drain {
    task: Option<tokio::task::JoinHandle<Result<(), &'static str>>>,
    loss: hoh_agents::public_stream::Loss,
    executor: Arc<dyn Executor>,
    work: Work,
}
/// What the process-end gap says: why the public stream ended early, and what of it was lost (M4T09R-301).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndGap {
    pub(super) reason: &'static str,
    pub(super) loss: hoh_agents::public_stream::LossSummary,
}
impl EndGap {
    pub(super) fn payload(&self, attempt: &str, epoch: i64) -> serde_json::Value {
        serde_json::json!({
            "attempt_id": attempt,
            "role_epoch": epoch,
            "reason": self.reason,
            "first_dropped_offset": self.loss.first_offset,
            "dropped_parts": self.loss.dropped,
            "unrecorded_parts": self.loss.unrecorded,
            "dropped_bytes": self.loss.bytes,
            "frames_after_close": self.loss.frames_after_close,
            "complete": false,
        })
    }
}
/// Pauses before the second and third attempt at a dropped body's gap: all three take well under one quiet period
/// (M4T09R-300).
const GAP_RETRY: [Duration; 2] = [Duration::from_millis(50), Duration::from_millis(100)];
impl Drain {
    pub(super) fn start(shared: Arc<Shared>, executor: Arc<dyn Executor>, work: Work) -> Self {
        let mut loss = hoh_agents::public_stream::Loss::default();
        let task = executor.take_public_stream(&work).map(|mut receiver| {
            loss = receiver.loss.clone();
            let attempt = work.attempt_id.clone();
            let epoch = work.assignment.role_epoch;
            let project = work.project_id.clone();
            let run = work.run_id.clone();
            super::begun::spawn(async move {
                while let Some(pending) = receiver.recv().await {
                    let part = &pending.part;
                    let mut blocked = None;
                    let mut since = None;
                    let mut persisted = false;
                    loop {
                        match shared.store.record_public(&attempt, epoch, part.clone()).await {
                            Ok(_) => {
                                persisted = true;
                                break;
                            }
                            Err(PersistError::Busy(_)) => {
                                if blocked.is_none() {
                                    blocked = Some(receiver.backpressure.blocked());
                                    since = Some(std::time::Instant::now());
                                }
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                            // Low space, a full state DB and any other refusal drop this body: storage must not stall
                            // the agent. Its gap takes the body's place (DC-M4T09a, M4T09R-300).
                            Err(err) => {
                                drop(blocked.take());
                                let (reason, details) = match &err {
                                    PersistError::StorageLowSpace {
                                        available_bytes,
                                        required_bytes,
                                        min_free_bytes,
                                    } => (
                                        "storage_low_space",
                                        serde_json::json!({
                                            "available_bytes": available_bytes,
                                            "required_bytes": required_bytes,
                                            "min_free_bytes": min_free_bytes,
                                        }),
                                    ),
                                    other => (
                                        "public_storage_error",
                                        serde_json::json!({"error": other.to_string()}),
                                    ),
                                };
                                let recorded =
                                    record_gap(&shared.store, &attempt, epoch, part, reason, details)
                                        .await;
                                receiver.loss.dropped(part, recorded);
                                break;
                            }
                        }
                    }
                    receiver.loss.settled();
                    drop(blocked);
                    if persisted && let Some(since) = since {
                        let event = hoh_core::persistence::NewEvent {
                            intent: hoh_core::persistence::Intent::Record,
                            project_id: project.clone(),
                            run_id: Some(run.clone()),
                            event_id: hoh_core::ids::child_id(
                                &attempt,
                                &format!(
                                    "public/backpressure/{}/{}/{}/{}",
                                    part.native_stream,
                                    part.received_offset,
                                    part.native_item_id,
                                    part.part
                                ),
                            ),
                            kind: "runner.diagnostic".into(),
                            payload: serde_json::json!({
                                "attempt_id": attempt,
                                "role_epoch": epoch,
                                "reason": "public_storage_backpressure",
                                "blocked_ms": u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX),
                            }),
                        };
                        commit_best_effort(&shared.store, event, "public_storage_backpressure")
                            .await;
                    }
                    drop(pending);
                }
                Ok(())
            })
        });
        Self {
            task,
            loss,
            executor,
            work,
        }
    }
    /// The process-end gap, if the stream ended early or lost output that no gap of its own records.
    pub(super) async fn finish(mut self) -> Option<EndGap> {
        let reason = if let Some(task) = self.task.as_mut() {
            // Local storage cannot hold up process end, cancellation or the runner's budget accounting indefinitely.
            match tokio::time::timeout(Duration::from_secs(1), &mut *task).await {
                Ok(Ok(Ok(()))) => None,
                Ok(Ok(Err(reason))) => Some(reason),
                Ok(Err(_)) => Some("public_drain_lost"),
                Err(_) => {
                    task.abort();
                    let _ = task.await;
                    Some("public_storage_interrupted_at_process_end")
                }
            }
        } else {
            None
        };
        let loss = self.loss.summary();
        match reason {
            Some(reason) => Some(EndGap { reason, loss }),
            None if loss.needs_gap() => Some(EndGap {
                reason: "public_output_lost",
                loss,
            }),
            None => None,
        }
    }
}
impl Drop for Drain {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        self.executor.release_public_stream(&self.work);
    }
}

/// A dropped body's gap, in its place and with its attempt's identity (M4T09R-201), tried a bounded number of times
/// (M4T09R-300). Whether it was written.
async fn record_gap(
    store: &hoh_core::persistence::Store,
    attempt: &str,
    epoch: i64,
    part: &hoh_core::public_output::PublicPart,
    reason: &str,
    details: serde_json::Value,
) -> bool {
    let mut place = part.clone();
    place.text = String::new();
    let drop = hoh_core::public_output::PublicDrop {
        part: place,
        dropped_bytes: part.text.len() as u64,
        reason: reason.to_owned(),
        details: match details {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        },
    };
    let mut last = None;
    for pause in std::iter::once(None).chain(GAP_RETRY.map(Some)) {
        if let Some(pause) = pause {
            tokio::time::sleep(pause).await;
        }
        match store.record_public_gap(attempt, epoch, drop.clone()).await {
            Ok(_) => return true,
            Err(err) => last = Some(err),
        }
    }
    if let Some(err) = last {
        eprintln!(
            "public stream {reason} gap not recorded after {} attempts: {err}",
            GAP_RETRY.len() + 1
        );
    }
    false
}

/// Best-effort diagnostics never end the drain task (DC-M4T09a).
async fn commit_best_effort(
    store: &hoh_core::persistence::Store,
    event: hoh_core::persistence::NewEvent,
    label: &str,
) {
    if let Err(err) = store
        .commit(hoh_core::persistence::Commit {
            events: vec![event],
            ..Default::default()
        })
        .await
    {
        eprintln!("public stream {label} skipped: {err}");
    }
}

#[derive(Debug, Default)]
pub(super) struct BackfillBackoff {
    errors: u32,
    last_error: Option<String>,
}

impl BackfillBackoff {
    const BASE: Duration = Duration::from_millis(10);
    const CAP: Duration = Duration::from_secs(5);

    pub(super) fn on_ok(&mut self) -> Duration {
        self.errors = 0;
        self.last_error = None;
        Self::BASE
    }

    pub(super) fn on_err(&mut self, err: &str) -> (Duration, bool) {
        let log = self.last_error.as_deref() != Some(err);
        if log {
            self.last_error = Some(err.to_owned());
        }
        let factor = 1u32 << self.errors.min(10);
        self.errors = self.errors.saturating_add(1);
        (Self::BASE.saturating_mul(factor).min(Self::CAP), log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hoh_core::persistence::Assignment;
    use tokio::sync::oneshot;
    #[derive(Debug)]
    struct Idle;
    impl Executor for Idle {
        fn deliver(&self, _: &Work) -> oneshot::Receiver<Result<(), String>> {
            oneshot::channel().1
        }
        fn start(
            &self,
            _: &Work,
        ) -> Result<super::super::runner::Started, super::super::runner::Refusal> {
            panic!("no process in drain cancellation test")
        }
    }
    fn work(prompt: Option<&str>) -> Work {
        Work {
            project_id: "p1".to_owned(),
            run_id: "R1".to_owned(),
            task_id: "T1".to_owned(),
            attempt_id: "R1/T1/a1".to_owned(),
            role: "developer".to_owned(),
            effect_key: "k".to_owned(),
            assignment: Assignment {
                spec_draft: false,
                inputs: Vec::new(),
                context_seq: 0,
                resource_bundle: None,
                agent: Some("claude".to_owned()),
                model: None,
                role_epoch: 1,
                config_revision: 0,
                session_id: Some("R1/session/developer/1".to_owned()),
                native_session: None,
                agent_version: Some("2.1.318".to_owned()),
                session_capabilities: None,
                session_identity: None,
                prompt: prompt.map(str::to_owned),
                goal: Some("Ship it".to_owned()),
                title: Some("Write the parser".to_owned()),
                first_input: false,
                supervision: None,
                context: Some(hoh_core::context::Facts {
                    goal: Some("Ship it".to_owned()),
                    scope: vec!["任务：T1 Write the parser（职位 developer）".to_owned()],
                    ..hoh_core::context::Facts::default()
                }),
                writer: true,
                handoff: None,
            },
            run_dir: None,
            helpers: None,
        }
    }

    #[tokio::test]
    async fn test_011_05_cancel_during_finish_aborts_the_public_writer() {
        let (ready, started) = oneshot::channel();
        let (dropped, gone) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let _owned = dropped;
            let _ = ready.send(());
            std::future::pending::<()>().await;
            Ok(())
        });
        let cleanup = task.abort_handle();
        started.await.expect("task started");
        let drain = Drain {
            task: Some(task),
            loss: hoh_agents::public_stream::Loss::default(),
            executor: Arc::new(Idle),
            work: work(None),
        };
        assert!(
            tokio::time::timeout(Duration::from_millis(20), drain.finish())
                .await
                .is_err()
        );
        let stopped = tokio::time::timeout(Duration::from_millis(200), gone).await;
        cleanup.abort();
        assert!(
            stopped.is_ok(),
            "cancelled finish must not detach a writer task"
        );
    }

    #[test]
    fn test_013_05_backfill_backoff_is_exponential_capped_and_logs_distinct_errors_once() {
        let mut b = BackfillBackoff::default();
        let (d1, log1) = b.on_err("disk full");
        let (d2, log2) = b.on_err("disk full");
        let (d3, log3) = b.on_err("disk full");
        assert!(log1, "first occurrence of an error is logged");
        assert!(!log2, "repeated identical error must not spam host.log");
        assert!(!log3, "repeated identical error must not spam host.log");
        assert!(
            d2 > d1,
            "backoff must grow exponentially: d1={d1:?} d2={d2:?}"
        );
        assert!(
            d3 > d2,
            "backoff must continue growing: d2={d2:?} d3={d3:?}"
        );
        let (d_new, log_new) = b.on_err("busy");
        assert!(log_new, "distinct error message must be logged once");
        assert!(d_new >= d3);
        let mut last = d_new;
        for _ in 0..30 {
            let (d, _) = b.on_err("busy");
            last = d;
        }
        assert!(
            last <= Duration::from_secs(5),
            "backoff must be capped at <=5s, got {last:?}"
        );
        assert!(
            last >= Duration::from_secs(1),
            "capped backoff must reach at least 1s, got {last:?}"
        );
        let ok_delay = b.on_ok();
        assert_eq!(ok_delay, Duration::from_millis(10));
        let (d_reset, log_after_ok) = b.on_err("busy");
        assert!(log_after_ok, "error after recovery logs again once");
        assert_eq!(d_reset, d1, "delay resets after Ok");
    }
}
