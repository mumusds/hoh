//! M3-T03: real product host and store, isolated fake native CLIs; no live agents.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use super::*;

#[tokio::test]
async fn test_045_03_unknown_variable_refuses_start() -> R {
    for (task, text, reason) in [
        ("T-secret", "{{SECRET_PLAN}}", "PROMPT_VARIABLE_MISSING"),
        ("T-partial", "{{GOAL}}", "PROMPT_UNAVAILABLE"),
    ] {
        let world = World::new()?;
        world.codex("gpt-6-astra")?;
        let project = world.project();
        std::fs::create_dir_all(project.join(".hoh/prompts"))?;
        let file = project.join(".hoh/prompts/dev.md");
        std::fs::write(&file, text)?;
        std::fs::write(
            project.join(".hoh/config.toml"),
            "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\nprompt = \".hoh/prompts/dev.md\"\n",
        )?;
        let gates = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = gates.clone();
        let config = world.config_with(
            hoh_core::config::Config::default(),
            world.registry(),
            Some(std::sync::Arc::new(move |attempt, _key| {
                captured.lock().unwrap().push(attempt.to_owned())
            })),
            None,
        );
        let host = start_when_free(config).await?;
        let outcome: R = async {
            let run_id = started(&host, &project, "prompt-variable").await?;

            std::fs::write(&file, text)?;
            ready(&project, &run_id, &[(task, "developer")])?;
            let attempt = format!("{task}/attempt-1");
            until(
                &project,
                &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'"),
                "FAILED",
                Duration::from_secs(30),
            )
            .await?;
            assert_eq!(
                query(
                    &project,
                    &format!("SELECT failure_reason FROM attempts WHERE attempt_id = '{attempt}'")
                )?,
                reason
            );
            let refused: Value = serde_json::from_str(&query(
                &project,
                &format!(
                    "SELECT payload_json FROM events WHERE event_id = '{attempt}/start-refused'"
                ),
            )?)?;
            if task == "T-secret" {
                assert_eq!(refused["details"]["variable"], "SECRET_PLAN", "{refused}");
            }
            assert_eq!(
                query(
                    &project,
                    &format!(
                        "SELECT count(*) FROM attempts WHERE task_id = '{task}' AND pid IS NOT NULL"
                    )
                )?,
                "0"
            );
            assert_eq!(
                query(
                    &project,
                    &format!("SELECT count(*) FROM attempts WHERE task_id = '{task}'")
                )?,
                "1"
            );
            until(
                &project,
                &format!("SELECT state FROM tasks WHERE task_id = '{task}'"),
                "BLOCKED",
                Duration::from_secs(30),
            )
            .await?;
            assert!(
                !gates.lock().unwrap().contains(&attempt),
                "no start gate was even created"
            );
            assert!(codex_prompts_of(&world, |a| a == attempt)?.is_empty());
            Ok(())
        }
        .await;
        host.shutdown().await;
        world.stop_fakes()?;
        outcome?;
    }
    Ok(())
}
