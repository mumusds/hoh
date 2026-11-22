//! Deterministic resource/checkpoint orderings. Real SQLite transactions; no Agent or timing sleeps.
use super::*;
use crate::config::Config;
use crate::persistence::{
    Availability, NewAttempt, Options, RunTarget, Store, SubmitCommand, selftest,
};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn options(bundle: Bundle) -> Options {
    Options {
        resource_bundle: Arc::new(bundle),
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    }
}
fn old() -> Bundle {
    let mut b = Bundle::current();
    b.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "old developer contract\n".to_owned(),
    );
    b
}
async fn setup() -> R<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new().prefix("m3t03-order-").tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(&path, options(old()))?;
    Connection::open(&path)?.execute_batch("INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p', '/unused', 'now');
      INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s', 'p', printf('%064d', 1), 1);")?;
    store
        .submit_command(SubmitCommand {
            command_id: "start".to_owned(),
            idempotency_key: "start".to_owned(),
            kind: "StartRun".to_owned(),
            run_id: "R".to_owned(),
            expected_revision: 0,
            payload: json!({"goal":"resource order", "mode":"AUTO", "interactive":false}),
            payload_sha256: "1".repeat(64),
            target: RunTarget::Start {
                mode: "AUTO".to_owned(),
                budget: Config::default().budget,
                policy: "balanced".to_owned(),
                assumptions: Default::default(),
                unattended: true,
            },
            event: NewEvent {
                intent: Intent::Progress,
                project_id: "p".to_owned(),
                run_id: Some("R".to_owned()),
                event_id: "created".to_owned(),
                kind: "run.created".to_owned(),
                payload: json!({"goal":"resource order"}),
            },
            outbox: None,
        })
        .await?;
    Connection::open(&path)?.execute_batch("UPDATE runs SET state = 'RUNNING' WHERE run_id = 'R';
        INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('T', 'R', 's', 'LEASED', 'developer', 1);")?;
    drop(store);
    Ok((dir, path))
}
fn attempt(id: &str) -> NewAttempt {
    NewAttempt {
        attempt_id: id.to_owned(),
        task_id: "T".to_owned(),
        session_id: None,
        role_epoch: 1,
        host_epoch: 0,
        config_revision: 0,
        agent: Some("claude".to_owned()),
    }
}
fn pin(path: &Path, role: &str) -> R<Pin> {
    Ok(latest(&Connection::open(path)?, "R", role, None)?.ok_or("no pin")?)
}
fn assigned(path: &Path, id: &str) -> R<Option<String>> {
    Ok(Connection::open(path)?.query_row(
        "SELECT resource_bundle_version FROM attempts WHERE attempt_id = ?1",
        [id],
        |r| r.get(0),
    )?)
}

// AC-047-03 / M3G-50: a steady supply of ready tasks must drain, rather than extend, the old bundle's attempts.
#[tokio::test]
async fn test_047_03_overlapping_attempts_pause_dispatch_until_resource_checkpoint() -> R {
    for state in ["PREPARED", "STARTING", "RUNNING"] {
        let (_dir, path) = setup().await?;
        let store = Store::open(&path, options(old()))?;
        Connection::open(&path)?.execute_batch(
            "UPDATE runs SET spec_id='s' WHERE run_id='R';
             INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json)
               VALUES ('sd', 'claude', 'developer', '1', '{\"structured_events\":true}');
             UPDATE role_bindings SET session_id='sd' WHERE run_id='R' AND role='developer';
             INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq, cost_estimate)
               VALUES ('overlap', 'R', 's', 'LEASED', 'developer', 2, 1),
                      ('next', 'R', 's', 'READY', 'developer', 3, 1);",
        )?;
        store.create_attempt(attempt("A")).await?;
        store
            .create_attempt(NewAttempt {
                task_id: "overlap".to_owned(),
                ..attempt("B")
            })
            .await?;
        Connection::open(&path)?.execute("UPDATE attempts SET state=?1", [state])?;
        drop(store);
        let store = Store::open(&path, options(Bundle::current()))?;
        let context = crate::decision::Context {
            required_capabilities: [(
                "developer".to_owned(),
                ["structured_events".to_owned()].into(),
            )]
            .into(),
            reader_slots: Some(4),
            ..Default::default()
        };
        let proposal = crate::decision::Proposal {
            decision_id: "dispatch-next".to_owned(),
            run_id: "R".to_owned(),
            role_epoch: 1,
            expected_revision: 0,
            spec_sha256: format!("{:064}", 1),
            candidate_id: None,
            causation_id: None,
            payload: crate::protocol::decode(
                &json!({"kind":"dispatch", "task_id":"next", "role":"developer"}),
            )?,
        };
        assert!(
            matches!(
                store.decide(proposal, context.clone()).await?,
                crate::persistence::Decided::Rejected {
                    reason: crate::protocol::RejectionReason::RoleBindingPending,
                    ..
                }
            ),
            "Commander dispatch must honor resource lag too"
        );
        // Other roles may upgrade independently; the developer still has two overlapping attempts.
        assert!(store.team_checkpoint("R", Availability::default()).await?);
        drop(store);
        let store = Store::open(&path, options(Bundle::current()))?;
        for ended in [None, Some("A")] {
            if let Some(id) = ended {
                Connection::open(&path)?.execute(
                    "UPDATE attempts SET state='CANCELLED' WHERE attempt_id=?1",
                    [id],
                )?;
            }
            for _ in 0..3 {
                assert!(
                    store
                        .schedule_round("R", context.clone())
                        .await?
                        .dispatched
                        .is_none(),
                    "{state}: resource lag must pause new dispatch while B remains in flight"
                );
                assert!(
                    !store
                        .runner_waiting("p")
                        .await?
                        .staffing
                        .iter()
                        .any(|b| b.role == "developer"),
                    "staffing must honor the same pending predicate"
                );
                assert_eq!(
                    pin(&path, "developer")?.resource_bundle_version,
                    old().version()
                );
            }
        }
        Connection::open(&path)?.execute(
            "UPDATE attempts SET state='FAILED' WHERE attempt_id='B'",
            [],
        )?;
        assert_eq!(store.runner_waiting("p").await?.team_behind, ["R"]);
        assert!(store.team_checkpoint("R", Availability::default()).await?);
        let dispatched = store
            .schedule_round("R", context)
            .await?
            .dispatched
            .ok_or("dispatch did not resume")?;
        assert_eq!(dispatched.task_id, "next");
        assert_eq!(
            assigned(&path, &dispatched.attempt_id)?,
            Some(Bundle::current().version())
        );
        assert_eq!(assigned(&path, "A")?, Some(old().version()));
        assert_eq!(assigned(&path, "B")?, Some(old().version()));
    }
    Ok(())
}

#[tokio::test]
async fn test_047_03_busy_resource_lag_does_not_request_checkpoint_every_tick() -> R {
    let (_dir, path) = setup().await?;
    let store = Store::open(&path, options(old()))?;
    store.create_attempt(attempt("A")).await?;
    drop(store);
    let store = Store::open(&path, options(Bundle::current()))?;
    assert_eq!(store.runner_waiting("p").await?.team_behind, ["R"]);
    assert!(store.team_checkpoint("R", Availability::default()).await?);
    for _ in 0..3 {
        assert!(
            store.runner_waiting("p").await?.team_behind.is_empty(),
            "only the busy developer is behind; no checkpoint can advance it"
        );
    }
    Connection::open(&path)?.execute(
        "UPDATE attempts SET state='CANCELLED' WHERE attempt_id='A'",
        [],
    )?;
    assert_eq!(store.runner_waiting("p").await?.team_behind, ["R"]);
    assert!(store.team_checkpoint("R", Availability::default()).await?);
    assert!(store.runner_waiting("p").await?.team_behind.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_047_03_dispatch_before_checkpoint_keeps_old_until_all_attempts_end() -> R {
    for state in ["PREPARED", "STARTING", "RUNNING"] {
        let (_dir, path) = setup().await?;
        let store = Store::open(&path, options(Bundle::current()))?;
        store.create_attempt(attempt("A")).await?;
        assert_eq!(
            assigned(&path, "A")?,
            Some(old().version()),
            "dispatch must bind before spawn"
        );
        Connection::open(&path)?.execute(
            "UPDATE attempts SET state = ?1 WHERE attempt_id = 'A'",
            [state],
        )?;
        store.team_checkpoint("R", Availability::default()).await?;
        assert_eq!(
            pin(&path, "developer")?.resource_bundle_version,
            old().version(),
            "{state}"
        );
        assert_eq!(
            pin(&path, "reviewer")?.resource_bundle_version,
            Bundle::current().version(),
            "other roles advance independently"
        );
        store.create_attempt(attempt("B")).await?;
        Connection::open(&path)?
            .execute_batch("UPDATE attempts SET state='CANCELLED' WHERE attempt_id='A';")?;
        store.team_checkpoint("R", Availability::default()).await?;
        assert_eq!(
            pin(&path, "developer")?.resource_bundle_version,
            old().version(),
            "B still prepared"
        );
        Connection::open(&path)?
            .execute_batch("UPDATE attempts SET state='FAILED' WHERE attempt_id='B';")?;
        store.team_checkpoint("R", Availability::default()).await?;
        assert_eq!(
            pin(&path, "developer")?.resource_bundle_version,
            Bundle::current().version()
        );
        assert_eq!(
            assigned(&path, "A")?,
            Some(old().version()),
            "completed attempt never retagged"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_047_03_checkpoint_before_dispatch_and_restart_are_idempotent() -> R {
    let (dir, path) = setup().await?;
    let store = Store::open(&path, options(Bundle::current()))?;
    assert_eq!(
        store.runner_waiting("p").await?.team_behind,
        ["R"],
        "resource-only upgrade wakes checkpoint"
    );
    assert!(store.team_checkpoint("R", Availability::default()).await?);
    assert!(!store.team_checkpoint("R", Availability::default()).await?);
    store.create_attempt(attempt("A")).await?;
    assert_eq!(assigned(&path, "A")?, Some(Bundle::current().version()));
    drop(store);
    let store = Store::open(&path, options(Bundle::current()))?;
    assert!(!store.team_checkpoint("R", Availability::default()).await?);
    let p = pin(&path, "developer")?;
    assert_eq!(p.load(dir.path())?, Bundle::current());
    let count:i64 = Connection::open(&path)?.query_row("SELECT count(*) FROM events WHERE kind='resource.pinned' AND json_extract(payload_json,'$.role')='developer'", [], |r|r.get(0))?;
    assert_eq!(count, 2);
    Ok(())
}

#[derive(Debug, Default)]
struct Gate {
    reached: std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>,
    released: std::sync::Mutex<bool>,
    wake: std::sync::Condvar,
    fail: std::sync::atomic::AtomicBool,
    checkpoint: std::sync::atomic::AtomicBool,
}
impl Gate {
    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.wake.notify_all();
    }
}
impl crate::persistence::FaultHooks for Gate {
    fn before_job(&self, job: &'static str) {
        self.checkpoint
            .store(job == "Checkpoint", std::sync::atomic::Ordering::SeqCst);
    }
    fn at(&self, job: &'static str, point: crate::persistence::WritePoint) {
        if job == "Checkpoint"
            && point == crate::persistence::WritePoint::EventsWritten
            && let Some(sender) = self.reached.lock().unwrap().take()
        {
            sender.send(()).unwrap();
            let mut released = self.released.lock().unwrap();
            while !*released {
                released = self.wake.wait(released).unwrap();
            }
        }
    }
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.fail.load(std::sync::atomic::Ordering::SeqCst)
            && self.checkpoint.load(std::sync::atomic::Ordering::SeqCst)
        {
            Err(PersistError::Storage(
                "injected checkpoint failure".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}
struct Release(Arc<Gate>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.release();
    }
}

async fn race(fail: bool) -> R {
    let (_dir, path) = setup().await?;
    let gate = Arc::new(Gate::default());
    let (sent, heard) = std::sync::mpsc::channel();
    *gate.reached.lock().unwrap() = Some(sent);
    gate.fail.store(fail, std::sync::atomic::Ordering::SeqCst);
    let mut opts = options(Bundle::current());
    opts.faults = gate.clone();
    let store = Arc::new(Store::open(&path, opts)?);
    let checkpoint = tokio::spawn({
        let store = store.clone();
        async move { store.team_checkpoint("R", Availability::default()).await }
    });
    let _release = Release(gate.clone());
    tokio::task::spawn_blocking(move || heard.recv_timeout(std::time::Duration::from_secs(10)))
        .await??;
    // Reader observes v1 while the checkpoint holds the writer transaction after writing the v2 events.
    assert_eq!(
        pin(&path, "developer")?.resource_bundle_version,
        old().version()
    );
    let mut dispatch = Box::pin(store.create_attempt(attempt("A")));
    std::future::poll_fn(|cx| {
        use std::future::Future;
        assert!(
            dispatch.as_mut().poll(cx).is_pending(),
            "dispatch cannot commit through the checkpoint transaction"
        );
        std::task::Poll::Ready(())
    })
    .await;
    gate.release();
    assert_eq!(checkpoint.await?.is_err(), fail);
    gate.fail.store(false, std::sync::atomic::Ordering::SeqCst);
    dispatch.await?;
    let want = if fail {
        old().version()
    } else {
        Bundle::current().version()
    };
    assert_eq!(assigned(&path, "A")?, Some(want.clone()));
    assert_eq!(pin(&path, "developer")?.resource_bundle_version, want);
    Ok(())
}
#[tokio::test]
async fn test_047_03_checkpoint_transaction_serializes_dispatch() -> R {
    race(false).await
}
#[tokio::test]
async fn test_047_03_checkpoint_failure_rolls_back_before_dispatch() -> R {
    race(true).await
}

#[tokio::test]
async fn test_047_03_return_to_prior_bundle_appends_a_new_pin() -> R {
    let (_dir, path) = setup().await?;
    let store = Store::open(&path, options(Bundle::current()))?;
    store.team_checkpoint("R", Availability::default()).await?;
    drop(store);
    let store = Store::open(&path, options(old()))?;
    store.team_checkpoint("R", Availability::default()).await?;
    assert_eq!(
        pin(&path, "developer")?.resource_bundle_version,
        old().version(),
        "returning to a prior installed version must not reuse the old event seq"
    );
    assert!(!store.team_checkpoint("R", Availability::default()).await?);
    Ok(())
}

#[tokio::test]
async fn test_047_03_old_host_epoch_cannot_repin_after_replacement() -> R {
    let (_dir, path) = setup().await?;
    let first = Store::open(&path, options(old()))?;
    first.begin_host("p", "/unused").await?;
    let next = Store::open(&path, options(Bundle::current()))?;
    next.begin_host("p", "/unused").await?;
    next.team_checkpoint("R", Availability::default()).await?;
    assert!(
        first
            .team_checkpoint("R", Availability::default())
            .await
            .is_err()
    );
    assert_eq!(
        pin(&path, "developer")?.resource_bundle_version,
        Bundle::current().version()
    );
    Ok(())
}
