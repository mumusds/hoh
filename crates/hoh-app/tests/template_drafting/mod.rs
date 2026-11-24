//! S6 preparation diagnostics: real host/Git, scripted agents, never live acceptance.
use crate::{input, world};
use serde_json::{Value, json};
use std::time::Duration;
type R = Result<(), Box<dyn std::error::Error>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn template_publication_failure_keeps_unknown_effect_and_never_starts_agent() -> R {
    let world = world::World::named("m3t08-template-failure")?;
    world.configure(&[], "")?;
    std::fs::create_dir_all(world.project().join("docs/spec"))?;
    // Sorted publication has already created several documents when it encounters this
    // non-file target. Nothing may overwrite it or silently retry the partial effect.
    std::fs::create_dir(world.project().join("docs/prd.md"))?;
    let host = world
        .team_host(
            world::Team::working(|_| Vec::new(), Duration::from_secs(60)),
            None,
        )
        .await?;
    let outcome: R = async {
        let run = world::start_run(&host, "start", "failed templates").await?;
        world.staff(&run)?;
        input(
            &host,
            &world,
            &run,
            "draft",
            "SendInput",
            json!({"text":"draft","target_role":null,"input_kind":"REPLY"}),
        )
        .await?;
        world
            .until(
                "SELECT count(*) FROM attempts WHERE state='UNKNOWN_EFFECT'",
                "1",
                Duration::from_secs(15),
            )
            .await?;
        assert!(
            world.project().join("docs/acceptance.md").is_file(),
            "keep complete files published before the error"
        );
        assert!(world.project().join("docs/prd.md").is_dir());
        assert!(
            world
                .query("SELECT failure_reason FROM attempts")?
                .contains("document is not a regular file")
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?,
            "0"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='attempt.workspace_start'")?,
            "0"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM effects WHERE state='UNKNOWN'")?,
            "1"
        );
        assert_eq!(
            world.query("SELECT count(*) FROM tasks WHERE next_retry_at IS NOT NULL")?,
            "0"
        );
        Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn templates_are_an_attempt_effect_before_the_drafter_and_keep_user_files() -> R {
    for profile in [None, Some("lite")] {
        let world = world::World::named("m3t08-template-hook")?;
        world.configure(
            &[],
            &profile
                .map(|p| format!("[project]\ndocument_profile=\"{p}\"\n"))
                .unwrap_or_default(),
        )?;
        std::fs::create_dir_all(world.project().join("docs"))?;
        std::fs::write(world.project().join("docs/feature.md"), "user document")?;
        let host = world
            .team_host(
                world::Team::working(|_| Vec::new(), Duration::from_secs(60)),
                None,
            )
            .await?;
        let outcome: R = async {
            let run = world::start_run(&host, "start", "离线起草").await?;
            world.staff(&run)?;
            input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
            world.until("SELECT count(*) FROM attempts WHERE state='RUNNING'", "1", Duration::from_secs(15)).await?;
            assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?, "1");
            let event: Value = serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='templates.instantiated'")?)?;
            assert_eq!(event["profile"], profile.unwrap_or("full"));
            assert_eq!(event["source"], if profile.is_some() { "project" } else { "default" });
            assert_eq!(event["skipped"], json!(["docs/feature.md"]));
            let bundle = hoh_core::resource_bundle::Bundle::current();
            let templates = hoh_core::templates::load(&world.project(), None, &run, &bundle)?;
            let expected = templates.render(profile.unwrap_or("full"), &hoh_core::templates::Inputs { goal: "离线起草".into(), owners: "cto".into() })?;
            for (path, content) in &expected {
                assert_eq!(std::fs::read_to_string(world.project().join(path))?, if path == "docs/feature.md" { "user document" } else { content });
            }
            let attempt = event["attempt_id"].as_str().ok_or("attempt id")?;
            let key = hoh_core::ids::child_id(attempt, "templates");
            assert_eq!(world.query(&format!("SELECT state FROM effects WHERE effect_key='{key}'"))?, "COMMITTED");
            assert_eq!(world.query("SELECT (SELECT seq FROM events WHERE kind='templates.instantiated') < (SELECT seq FROM events WHERE kind='attempt.workspace_start')")?, "1");
            let start = host.store().reader()?.draft_start(attempt)?.ok_or("start")?;
            let dirty = start["draft_dirty"].as_array().ok_or("dirty")?;
            let path_hex = |p: &str| format!("hex:{}", p.bytes().map(|b| format!("{b:02x}")).collect::<String>());
            assert!(dirty.contains(&json!(path_hex("docs/feature.md"))), "user file keeps its original ownership");
            for path in expected.keys().filter(|p| p.as_str() != "docs/feature.md") {
                assert!(!dirty.contains(&json!(path_hex(path))), "Runtime template is not user pre-dirty: {path}");
            }
            Ok(())
        }.await;
        host.shutdown().await;
        outcome?;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn templates_survive_restart_without_reinstantiation_or_user_overwrite() -> R {
    let world = world::World::named("m3t08-template-restart")?;
    world.configure(&[], "")?;
    let host = world
        .team_host(world::Team::drafting(world.project()), None)
        .await?;
    let prepared: Result<String, Box<dyn std::error::Error>> = async {
        let run = world::start_run(&host, "start", "template restart").await?;
        world.staff(&run)?;
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'", "1", Duration::from_secs(20)).await?;
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?, "1");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind IN ('plan.write_violation','plan.write_observation_failed')")?, "0");
        Ok(run)
    }.await;
    host.shutdown().await;
    let run = prepared?;
    std::fs::write(
        world.project().join("docs/prd.md"),
        "user edits after draft",
    )?;
    std::fs::remove_file(world.project().join("docs/ux.md"))?;
    let host = world
        .team_host(world::Team::drafting(world.project()), None)
        .await?;
    let outcome: R = async {
        input(
            &host,
            &world,
            &run,
            "reject",
            "SendInput",
            json!({"text":"please revise","target_role":null,"input_kind":"REPLY"}),
        )
        .await?;
        world
            .until(
                "SELECT count(*) FROM events WHERE kind='spec.draft_completed'",
                "2",
                Duration::from_secs(20),
            )
            .await?;
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?,
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(world.project().join("docs/prd.md"))?,
            "user edits after draft"
        );
        assert!(
            !world.project().join("docs/ux.md").exists(),
            "a committed preparation is not run again"
        );
        Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}
