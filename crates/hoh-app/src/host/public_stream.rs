//! Public storage runs beside process control. A queue reservation lasts through commit, including DB_BUSY.
use super::{
    http::Shared,
    runner::{Executor, Work},
};
use hoh_core::persistence::PersistError;
use std::{sync::Arc, time::Duration};
pub(super) struct Drain {
    task: Option<tokio::task::JoinHandle<Result<(), &'static str>>>,
    executor: Arc<dyn Executor>,
    work: Work,
}
impl Drain {
    pub(super) fn start(shared: Arc<Shared>, executor: Arc<dyn Executor>, work: Work) -> Self {
        let task = executor.take_public_stream(&work).map(|mut receiver| {
            let attempt = work.attempt_id.clone();
            let epoch = work.assignment.role_epoch;
            let project = work.project_id.clone();
            let run = work.run_id.clone();
            super::begun::spawn(async move {
                while let Some(pending) = receiver.recv().await {
                    let mut blocked = None;
                    let mut since = None;
                    let mut persisted = false;
                    loop {
                        match shared
                            .store
                            .record_public(&attempt, epoch, pending.part.clone())
                            .await
                        {
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
                            Err(PersistError::StorageLowSpace {
                                available_bytes,
                                required_bytes,
                                min_free_bytes,
                            }) => {
                                drop(blocked.take());
                                let block = hoh_core::ids::child_id(
                                    &attempt,
                                    &format!(
                                        "public/{}/{}",
                                        pending.part.native_stream, pending.part.block_key
                                    ),
                                );
                                let event = hoh_core::persistence::NewEvent {
                                    intent: hoh_core::persistence::Intent::Record,
                                    project_id: project.clone(),
                                    run_id: Some(run.clone()),
                                    event_id: hoh_core::ids::child_id(
                                        &attempt,
                                        &format!(
                                            "public/gap/{}/{}/{}/{}",
                                            pending.part.native_stream,
                                            pending.part.received_offset,
                                            pending.part.native_item_id,
                                            pending.part.part
                                        ),
                                    ),
                                    kind: "public.gap".into(),
                                    payload: serde_json::json!({
                                        "block_id": block,
                                        "attempt_id": attempt,
                                        "role_epoch": epoch,
                                        "native_stream": pending.part.native_stream,
                                        "native_item_id": pending.part.native_item_id,
                                        "received_offset": pending.part.received_offset,
                                        "part": pending.part.part,
                                        "reason": "storage_low_space",
                                        "available_bytes": available_bytes,
                                        "required_bytes": required_bytes,
                                        "min_free_bytes": min_free_bytes,
                                        "dropped_bytes": pending.part.text.len(),
                                        "complete": false,
                                    }),
                                };
                                commit_best_effort(&shared.store, event, "storage_low_space_gap")
                                    .await;
                                break;
                            }
                            Err(err) => {
                                drop(blocked.take());
                                let block = hoh_core::ids::child_id(
                                    &attempt,
                                    &format!(
                                        "public/{}/{}",
                                        pending.part.native_stream, pending.part.block_key
                                    ),
                                );
                                let event = hoh_core::persistence::NewEvent {
                                    intent: hoh_core::persistence::Intent::Record,
                                    project_id: project.clone(),
                                    run_id: Some(run.clone()),
                                    event_id: hoh_core::ids::child_id(
                                        &attempt,
                                        &format!(
                                            "public/gap/{}/{}/{}/{}",
                                            pending.part.native_stream,
                                            pending.part.received_offset,
                                            pending.part.native_item_id,
                                            pending.part.part
                                        ),
                                    ),
                                    kind: "public.gap".into(),
                                    payload: serde_json::json!({
                                        "block_id": block,
                                        "attempt_id": attempt,
                                        "role_epoch": epoch,
                                        "native_stream": pending.part.native_stream,
                                        "native_item_id": pending.part.native_item_id,
                                        "received_offset": pending.part.received_offset,
                                        "part": pending.part.part,
                                        "reason": "public_storage_error",
                                        "error": err.to_string(),
                                        "dropped_bytes": pending.part.text.len(),
                                        "complete": false,
                                    }),
                                };
                                commit_best_effort(&shared.store, event, "public_storage_error_gap")
                                    .await;
                                break;
                            }
                        }
                    }
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
                                    pending.part.native_stream,
                                    pending.part.received_offset,
                                    pending.part.native_item_id,
                                    pending.part.part
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
            executor,
            work,
        }
    }
    pub(super) async fn finish(mut self) -> Option<&'static str> {
        if let Some(task) = self.task.as_mut() {
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

/// Best-effort diagnostics and inline gaps never end the drain task (DC-M4T09a).
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
}
