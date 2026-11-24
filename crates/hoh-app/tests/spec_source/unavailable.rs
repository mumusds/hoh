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
