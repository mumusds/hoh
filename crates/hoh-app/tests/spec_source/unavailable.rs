use super::*;

// M3B-07 / AC-034-03: real filesystem faults in an isolated product host, without live agents. Restore the exact
// permission before any assertion so the same command can prove recovery and the fixture remains removable.
async fn unreadable_source(path: &str) -> R {
    assert!(!rustix::process::getuid().is_root());
    for kind in ["ApproveSpec", "ProposeSpecChange"] {
        let world = World::new()?;
        let original = repository(&world)?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "start", "source read recovery").await?;
        let index = if kind == "ProposeSpecChange" {
            submit_spec(&host, &run, "first", "ApproveSpec", &original).await?;
            revise_a(&world)?
        } else {
            original
        };
        let payload = if kind == "ProposeSpecChange" {
            json!({"spec_sha256":index.sha256(),"reason":"source recovery"})
        } else {
            json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"})
        };
        let body = command(&host, &run, "retry-same-command", kind, payload).await?;
        let before = snapshot(&world)?;
        let file = world.project().join(path);
        let permissions = std::fs::metadata(&file)?.permissions();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000))?;
        let refused = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await;
        std::fs::set_permissions(&file, permissions)?;
        let (status, response) = refused?;
        assert_eq!(status, 503, "{kind}, unreadable {path}: {response}");
        assert_eq!(response["code"], "SPEC_SOURCE_UNAVAILABLE");
        assert_eq!(response["retryable"], true);
        assert_eq!(response["current_revision"], body["expected_revision"]);
        assert_eq!(
            response.as_object().ok_or("error object")?.len(),
            4,
            "DB_BUSY Error shape"
        );
        assert_eq!(
            snapshot(&world)?,
            before,
            "no accepted command, index failure event or partial specification"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
            "0"
        );
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(
            status, 202,
            "same command recovers after {path}: {response}"
        );
        assert_eq!(
            world.query(
                "SELECT count(*) FROM commands WHERE idempotency_key='retry-same-command'"
            )?,
            "1"
        );
        let after = snapshot(&world)?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{response}");
        assert_eq!(snapshot(&world)?, after);
        host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_034_03_m3b07_markdown_read_unavailable_is_retryable() -> R {
    unreadable_source("docs/spec/a.md").await
}

#[tokio::test]
async fn test_034_03_m3b07_index_read_unavailable_is_retryable() -> R {
    unreadable_source(specindex::PATH).await
}

#[tokio::test]
async fn test_034_03_m3b07_git_ls_files_unavailable_is_retryable() -> R {
    unreadable_source(".git/index").await
}

#[tokio::test]
async fn test_034_03_m3b07_directory_read_unavailable_is_retryable() -> R {
    unreadable_source("docs/spec").await
}

#[tokio::test]
async fn test_034_03_m3b07_invalid_index_shape_stays_nonretryable() -> R {
    let world = World::new()?;
    let index = repository(&world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "invalid source shape").await?;
    let path = world.project().join(specindex::PATH);
    std::fs::remove_file(&path)?;
    std::fs::create_dir(&path)?;
    let body = command(
        &host,
        &run,
        "invalid",
        "ApproveSpec",
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 422, "{response}");
    assert_eq!(response["code"], "SPEC_INVALID");
    assert_eq!(response["retryable"], false);
    assert_eq!(response["details"]["path"], specindex::PATH);
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "0");
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// The first specification-source read panics, so its join fails; later reads run (M3LE-02).
#[derive(Debug)]
struct FailSpecSourceRead(std::sync::atomic::AtomicBool);

impl hoh_app::host::HostHooks for FailSpecSourceRead {
    fn fail_spec_source_read(&self) -> bool {
        self.0.swap(false, std::sync::atomic::Ordering::SeqCst)
    }
}

async fn host_with_spec_read_fault(
    world: &World,
    fail: std::sync::Arc<FailSpecSourceRead>,
) -> R<Host> {
    let mut config = hoh_app::host::HostConfig::new(&world.project());
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.hooks = fail;
    Ok(Host::start(config).await?)
}

// Unmapped on purpose: a `test_NNN_MM` name would be recorded by traceability (M3FK-01).
#[tokio::test]
async fn spec_source_join_failure_answers_spec_source_unavailable() -> R {
    for kind in ["ApproveSpec", "ProposeSpecChange"] {
        let world = World::new()?;
        let original = repository(&world)?;
        let fail = std::sync::Arc::new(FailSpecSourceRead(std::sync::atomic::AtomicBool::new(
            false,
        )));
        let host = host_with_spec_read_fault(&world, std::sync::Arc::clone(&fail)).await?;
        let run = world::start_run(&host, "start", "spec source join failure").await?;
        let index = if kind == "ProposeSpecChange" {
            submit_spec(&host, &run, "first", "ApproveSpec", &original).await?;
            revise_a(&world)?
        } else {
            original
        };
        let payload = if kind == "ProposeSpecChange" {
            json!({"spec_sha256":index.sha256(),"reason":"join failure"})
        } else {
            json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"})
        };
        let body = command(&host, &run, "retry-same-command", kind, payload).await?;
        let before = snapshot(&world)?;
        fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 503, "{kind}, join failure: {response}");
        assert_eq!(
            response["code"], "SPEC_SOURCE_UNAVAILABLE",
            "{kind}, join failure: {response}"
        );
        assert_eq!(response["retryable"], true, "{kind}: {response}");
        assert!(
            response["message"].as_str().is_some_and(|message| {
                message.starts_with(
                    "specification source could not be read; nothing was written: specification read stopped:",
                )
            }),
            "{kind}: {response}"
        );
        assert_eq!(response["current_revision"], body["expected_revision"]);
        assert_eq!(
            response.as_object().ok_or("error object")?.len(),
            4,
            "DB_BUSY Error shape: {response}"
        );
        assert_eq!(
            snapshot(&world)?,
            before,
            "no accepted command, index failure event or partial specification"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
            "0"
        );
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(
            status, 202,
            "same command recovers after the join failure: {response}"
        );
        assert_eq!(
            world.query(
                "SELECT count(*) FROM commands WHERE idempotency_key='retry-same-command'"
            )?,
            "1"
        );
        let after = snapshot(&world)?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{response}");
        assert_eq!(snapshot(&world)?, after);
        host.shutdown().await;
    }
    Ok(())
}

// M4L1-002: a spawn_blocking JoinError on ApproveSpec / ProposeSpecChange must be mapped into
// SpecReadError::Unavailable and passed to the store so missing-run, stale-revision, replay and
// Reject keep their own answers instead of 503 SPEC_SOURCE_UNAVAILABLE.
#[tokio::test]
async fn spec_source_join_failure_lets_store_decide_reject_replay_stale_and_missing() -> R {
    let world = World::new()?;
    let original = repository(&world)?;
    let fail = std::sync::Arc::new(FailSpecSourceRead(std::sync::atomic::AtomicBool::new(
        false,
    )));
    let host = host_with_spec_read_fault(&world, std::sync::Arc::clone(&fail)).await?;
    let run = world::start_run(&host, "start", "join error store decisions").await?;

    // 1. Missing run -> 409 RUN_NOT_FOUND (not 503) for both ApproveSpec and ProposeSpecChange.
    for (kind, payload) in [
        (
            "ApproveSpec",
            json!({"spec_sha256": original.sha256(), "decision": "APPROVE_ONLY"}),
        ),
        (
            "ProposeSpecChange",
            json!({"spec_sha256": original.sha256(), "reason": "missing run"}),
        ),
    ] {
        let missing_body = json!({
            "schema_version": "1.0",
            "command_id": format!("missing-{kind}"),
            "idempotency_key": format!("missing-{kind}"),
            "run_id": "no-such-run",
            "expected_revision": 0,
            "kind": kind,
            "payload": payload,
        });
        fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&missing_body)).await?;
        assert_eq!(status, 409, "{kind} missing run: {response}");
        assert_eq!(response["code"], "RUN_NOT_FOUND", "{kind}: {response}");
    }

    // 2. Stale revision -> 409 STALE_REVISION (not 503) for both ApproveSpec and ProposeSpecChange.
    for (kind, payload) in [
        (
            "ApproveSpec",
            json!({"spec_sha256": original.sha256(), "decision": "APPROVE_ONLY"}),
        ),
        (
            "ProposeSpecChange",
            json!({"spec_sha256": original.sha256(), "reason": "stale rev"}),
        ),
    ] {
        let mut stale_body = command(&host, &run, &format!("stale-{kind}"), kind, payload).await?;
        stale_body["expected_revision"] = json!(999);
        fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&stale_body)).await?;
        assert_eq!(status, 409, "{kind} stale revision: {response}");
        assert_eq!(response["code"], "STALE_REVISION", "{kind}: {response}");
    }

    // 3. Replay of an accepted ApproveSpec and ProposeSpecChange -> 202 replay (not 503).
    let approve_body = command(
        &host,
        &run,
        "approve-rev1",
        "ApproveSpec",
        json!({"spec_sha256": original.sha256(), "decision": "APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) =
        world::exchange(&host, "POST", "/v1/commands", Some(&approve_body)).await?;
    assert_eq!(status, 202, "initial approve: {response}");
    let after_approve = snapshot(&world)?;
    fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, response) =
        world::exchange(&host, "POST", "/v1/commands", Some(&approve_body)).await?;
    assert_eq!(
        status, 202,
        "ApproveSpec replay under join error: {response}"
    );
    assert_eq!(snapshot(&world)?, after_approve);

    let revised = revise_a(&world)?;
    let propose_body = command(
        &host,
        &run,
        "propose-rev2",
        "ProposeSpecChange",
        json!({"spec_sha256": revised.sha256(), "reason": "propose for replay and reject"}),
    )
    .await?;
    let (status, response) =
        world::exchange(&host, "POST", "/v1/commands", Some(&propose_body)).await?;
    assert_eq!(status, 202, "initial propose: {response}");
    let after_propose = snapshot(&world)?;
    fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, response) =
        world::exchange(&host, "POST", "/v1/commands", Some(&propose_body)).await?;
    assert_eq!(
        status, 202,
        "ProposeSpecChange replay under join error: {response}"
    );
    assert_eq!(snapshot(&world)?, after_propose);

    // 4. Reject on the pending proposal -> 202 ACCEPTED (not 503), even when the spec-source join panics.
    let reject_body = command(
        &host,
        &run,
        "reject-rev2",
        "ApproveSpec",
        json!({"spec_sha256": revised.sha256(), "decision": "REJECT"}),
    )
    .await?;
    fail.0.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, response) =
        world::exchange(&host, "POST", "/v1/commands", Some(&reject_body)).await?;
    assert_eq!(status, 202, "Reject under join error: {response}");
    assert_eq!(
        world.query("SELECT state FROM change_requests")?,
        "REJECTED"
    );

    host.shutdown().await;
    Ok(())
}
