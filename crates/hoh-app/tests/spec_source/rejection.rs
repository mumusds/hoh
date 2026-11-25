use super::*;

// M3R-002: all user entrances can send a pending first draft back even after its Markdown becomes invalid.
#[tokio::test]
async fn test_008_02_m3r002_reject_invalid_pending_draft() -> R {
    for entrance in ["ApproveSpec", "AnswerQuestion", "SendInput"] {
        let world = World::new()?;
        let index = repository(&world)?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "start", "reject invalid draft").await?;
        world.exec(&format!(
            "DELETE FROM events;
             UPDATE runs SET state='BLOCKED',resume_state='SPECIFYING',blocked_reason='NEED_DECISION',
                 blocked_reasons_json='[\"NEED_DECISION\"]';
             INSERT INTO tasks(task_id,run_id,kind,role,writer,state,created_seq)
                 VALUES('D','{run}','spec_draft','cto',1,'IMPLEMENTED',1);
             INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at)
                 VALUES(:p,'{run}','claim','run.blocker_claimed',
                 '{{\"source\":\"runtime:spec_approval\",\"reason\":\"NEED_DECISION\"}}','now');
             INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at)
                 VALUES(:p,'{run}','Q','question.asked','{}','now');",
            json!({"reason":"spec_approval","question_id":"Q","task_id":"D",
                "attempt_id":"A","spec_sha256":index.sha256(),"git_commit":"a".repeat(40),
                "base_spec_id":null,"text":"请批准"})
        ))?;
        let broken = b"# Invalid pending draft\n";
        std::fs::write(world.project().join("docs/spec/a.md"), broken)?;
        let index_before = std::fs::read(world.project().join(specindex::PATH))?;
        let before = snapshot(&world)?;
        if entrance == "ApproveSpec" {
            let wrong = command(
                &host,
                &run,
                "wrong-digest",
                entrance,
                json!({"spec_sha256":"0".repeat(64),"decision":"REJECT"}),
            )
            .await?;
            let (status, response) =
                world::exchange(&host, "POST", "/v1/commands", Some(&wrong)).await?;
            assert_eq!(
                status, 409,
                "the persisted question fences the digest: {response}"
            );
            assert_eq!(response["code"], "SPEC_CONFLICT");
            assert_eq!(snapshot(&world)?, before);
        }
        let note = "请修复无效的文档";
        let payload = match entrance {
            "ApproveSpec" => json!({"spec_sha256":index.sha256(),"decision":"REJECT","note":note}),
            "AnswerQuestion" => json!({"question_id":"Q","choice_id":"reject","answer":note}),
            _ => json!({"text":note,"input_kind":"REPLY","target_role":null}),
        };
        let body = command(&host, &run, "reject", entrance, payload).await?;
        let mut stale = body.clone();
        stale["expected_revision"] =
            json!(body["expected_revision"].as_i64().ok_or("revision")? + 1);
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&stale)).await?;
        assert_eq!(status, 409, "{response}");
        assert_eq!(response["code"], "STALE_REVISION");
        assert_eq!(snapshot(&world)?, before);
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(
            status, 202,
            "{entrance} must reject without parsing current Markdown: {response}"
        );
        assert_eq!(
            world.query("SELECT state FROM tasks WHERE task_id='D'")?,
            "READY"
        );
        assert_eq!(world.query("SELECT state FROM runs")?, "SPECIFYING");
        assert_eq!(world.query("SELECT count(*) FROM specs")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
            "0"
        );
        assert_eq!(world.query("SELECT json_extract(payload_json,'$.answer') FROM events WHERE kind='question.answered'")?, note);
        assert_eq!(
            std::fs::read(world.project().join("docs/spec/a.md"))?,
            broken
        );
        assert_eq!(
            std::fs::read(world.project().join(specindex::PATH))?,
            index_before
        );
        let rejected = snapshot(&world)?;
        let (status, replay) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{replay}");
        assert_eq!(snapshot(&world)?, rejected);
        host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_033_01_m3r002_reject_invalid_proposal_keeps_base_guard() -> R {
    for stale_base in [false, true] {
        let world = World::new()?;
        let original = repository(&world)?;
        let host = world.start_plain_host(None).await?;
        let run = world::start_run(&host, "start", "reject invalid proposal").await?;
        submit_spec(&host, &run, "first", "ApproveSpec", &original).await?;
        let proposed = revise_a(&world)?;
        submit_spec(&host, &run, "propose", "ProposeSpecChange", &proposed).await?;
        if stale_base {
            let mut files = sources();
            files[0].bytes.extend_from_slice(b"\n\n");
            std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
            let next = specindex::generate_index(&files)?;
            submit_spec(&host, &run, "propose-next", "ProposeSpecChange", &next).await?;
            submit_spec(&host, &run, "approve-next", "ApproveSpec", &next).await?;
        }
        let cr = world.query("SELECT cr_id FROM change_requests WHERE state='PROPOSED'")?;
        std::fs::write(
            world.project().join("docs/spec/a.md"),
            b"# Invalid proposal\n",
        )?;
        let before = snapshot(&world)?;
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
        if stale_base {
            assert_eq!(status, 409, "{response}");
            assert_eq!(response["code"], "SPEC_CONFLICT");
            assert_ne!(
                response["details"]["expected_base"],
                response["details"]["actual_base"]
            );
            assert_eq!(snapshot(&world)?, before);
        } else {
            assert_eq!(status, 202, "{response}");
        }
        assert_eq!(
            world.query(&format!(
                "SELECT state FROM change_requests WHERE cr_id='{cr}'"
            ))?,
            if stale_base { "PROPOSED" } else { "REJECTED" }
        );
        assert_eq!(
            world.query("SELECT count(*) FROM specs")?,
            if stale_base { "2" } else { "1" }
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
            "0"
        );
        host.shutdown().await;
    }
    Ok(())
}

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
