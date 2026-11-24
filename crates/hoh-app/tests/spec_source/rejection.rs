use super::*;

// M3B-06 / AC-034-02: a refusal targets the proposed digest even after its files are reverted. Approval and
// proposal still fence the workspace digest; approval also still requires the tracked canonical index.
#[tokio::test]
async fn test_034_02_reject_proposed_change_after_workspace_revert_or_untracking() -> R {
    for reverted in [true, false] {
        let world = World::new()?;
        let original = repository(&world)?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "start", "reject a proposed specification").await?;
        submit_spec(&host, &run, "first", "ApproveSpec", &original).await?;
        let proposed = revise_a(&world)?;
        submit_spec(&host, &run, "propose", "ProposeSpecChange", &proposed).await?;
        if reverted {
            world.git(&["restore", "--", "docs/spec"])?;
        } else {
            world.git(&["rm", "--cached", "--", specindex::PATH])?;
        }
        let before = snapshot(&world)?;
        let files_before = std::fs::read(world.project().join(specindex::PATH))?;
        for decision in ["APPROVE_ONLY", "APPROVE_AND_BUILD"] {
            let body = command(
                &host,
                &run,
                decision,
                "ApproveSpec",
                json!({"spec_sha256":proposed.sha256(),"decision":decision}),
            )
            .await?;
            let (status, response) =
                world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
            assert_eq!(status, 409, "{response}");
            assert_eq!(response["code"], "SPEC_CONFLICT");
            assert_eq!(snapshot(&world)?, before);
        }
        if reverted {
            let body = command(
                &host,
                &run,
                "stale-proposal",
                "ProposeSpecChange",
                json!({"spec_sha256":proposed.sha256(),"reason":"still requires current bytes"}),
            )
            .await?;
            let (status, response) =
                world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
            assert_eq!(status, 409, "{response}");
            assert_eq!(response["code"], "SPEC_CONFLICT");
            assert_eq!(snapshot(&world)?, before);
        }
        let body = command(
            &host,
            &run,
            "reject",
            "ApproveSpec",
            json!({"spec_sha256":proposed.sha256(),"decision":"REJECT","note":"withdrawn"}),
        )
        .await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(
            status, 202,
            "REJECT must not require current workspace bytes (reverted={reverted}): {response}"
        );
        assert_eq!(
            world.query("SELECT state FROM change_requests")?,
            "REJECTED"
        );
        assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
        assert_eq!(world.query("SELECT sha256 FROM specs")?, original.sha256());
        assert_eq!(
            world.query(
                "SELECT count(*) FROM runs r JOIN specs s USING(spec_id) WHERE s.revision=1"
            )?,
            "1"
        );
        assert_eq!(
            std::fs::read(world.project().join(specindex::PATH))?,
            files_before
        );
        let rejected = snapshot(&world)?;
        let (status, replay) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{replay}");
        assert_eq!(snapshot(&world)?, rejected);
        host.shutdown().await;
    }
    Ok(())
}
