//! S6 preparation diagnostics: real host/Git, scripted agents, never live acceptance.
use crate::{input, world};
use serde_json::{Value, json};
use std::time::Duration;
type R = Result<(), Box<dyn std::error::Error>>;

// M3O-700 / AC-029-03: only pre-approval drafting instantiates templates.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_029_03_revision_draft_does_not_recreate_deleted_templates() -> R {
    let world = world::World::named("m3t08-revision-templates")?;
    world.configure(&[], "")?;
    let host = world
        .team_host(world::Team::drafting(world.project()), None)
        .await?;
    let outcome: R = async {
        let run = world::start_run(&host, "start", "revision templates").await?;
        world.staff(&run)?;
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'", "1", Duration::from_secs(20)).await?;
        let q: Value = serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval' ORDER BY seq DESC LIMIT 1")?)?;
        input(&host, &world, &run, "approve", "ApproveSpec", json!({"spec_sha256":q["spec_sha256"],"decision":"APPROVE_ONLY"})).await?;
        assert_eq!(world.query("SELECT count(*) FROM specs WHERE revision=1")?, "1");
        assert_eq!(world.query("SELECT state||'/'||coalesce(spec_id,'NULL') FROM tasks WHERE kind='spec_draft'")?, "ACCEPTED/NULL");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?, "1");
        for path in ["docs/ux.md", "docs/spec/spec.md"] {
            assert!(world.project().join(path).is_file());
            std::fs::remove_file(world.project().join(path))?;
        }
        // The user commits the tracked skeleton deletion before requesting a revision.
        // Its deletion must not become the drafter's pre-dirty write-set violation.
        world.git(&["add", "-u", "docs/spec"])?;
        world.git(&["commit", "-qm", "remove unused skeleton"])?;
        input(&host, &world, &run, "revise", "SendInput", json!({"text":"revise the specification","target_role":null,"input_kind":"STEER"})).await?;
        world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'", "2", Duration::from_secs(20)).await?;
        assert_eq!(world.query("SELECT json_extract(payload_json,'$.generation') FROM events WHERE kind='spec.draft_requested' ORDER BY seq DESC LIMIT 1")?, "2");
        assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft' AND spec_id IS NOT NULL")?, "1");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?, "1", "revision drafting must not instantiate templates again");
        for path in ["docs/ux.md", "docs/spec/spec.md"] {
            assert!(!world.project().join(path).exists(), "revision draft recreated {path}");
        }
        Ok(())
    }.await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_029_03_m3o701_symlink_target_fails_before_template_effect() -> R {
    non_regular_target(true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_029_03_m3o701_directory_target_fails_before_template_effect() -> R {
    non_regular_target(false).await
}

async fn non_regular_target(symlink: bool) -> R {
    let world = world::World::named("m3t08-template-preflight")?;
    world.configure(&[], "")?;
    world.git(&["init", "-q"])?;
    std::fs::create_dir_all(world.project().join("docs"))?;
    let target = world.project().join("docs/user-guide.md");
    let outside = world.home().join("user-document.md");
    std::fs::write(&outside, "user document")?;
    if symlink {
        std::os::unix::fs::symlink(&outside, &target)?;
    } else {
        std::fs::create_dir(&target)?;
    }
    let host = world
        .team_host(
            world::Team::working(|_| Vec::new(), Duration::from_secs(60)),
            None,
        )
        .await?;
    let outcome: R = async {
        let run = world::start_run(&host, "start", "invalid template target").await?;
        world.staff(&run)?;
        input(&host, &world, &run, "draft", "SendInput", json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
        world.until("SELECT count(*)>0 FROM attempts WHERE ended_at IS NOT NULL", "1", Duration::from_secs(15)).await?;
        assert!(!world.project().join("docs/acceptance.md").exists(), "preflight must reject every invalid target before publishing any document");
        assert!(!world.project().join("docs/spec").exists(), "preflight must not create directories");
        let reason = world.query("SELECT failure_reason FROM attempts ORDER BY rowid LIMIT 1")?;
        assert!(reason.contains("docs/user-guide.md") && reason.contains("document is not a regular file"), "{reason}");
        assert_eq!(world.query("SELECT count(*) FROM attempts WHERE state='UNKNOWN_EFFECT'")?, "0");
        assert_eq!(world.query("SELECT state FROM attempts ORDER BY rowid LIMIT 1")?, "FAILED");
        let attempt = world.query("SELECT attempt_id FROM attempts ORDER BY rowid LIMIT 1")?;
        let key = hoh_core::ids::child_id(&attempt, "templates");
        assert_eq!(world.query(&format!("SELECT count(*) FROM effects WHERE effect_key='{key}'"))?, "0", "even PREPARED must follow preflight");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind IN ('templates.instantiated','attempt.workspace_start')")?, "0");
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state<>'PREPARED'")?, "0");
        let meta = std::fs::symlink_metadata(&target)?;
        assert_eq!(meta.file_type().is_symlink(), symlink);
        assert_eq!(meta.is_dir(), !symlink);
        if symlink {
            assert_eq!(std::fs::read_link(&target)?, outside);
        }
        assert_eq!(std::fs::read_to_string(&outside)?, "user document");
        Ok(())
    }.await;
    host.shutdown().await;
    outcome
}

// A conflict arriving after the read-only preflight is a real publication failure.
// Use the existing committed-write fault seam; no product hook or timing race is needed.
#[derive(Debug)]
struct ConflictAfterTemplateStarted {
    world: std::sync::Arc<world::World>,
    fired: std::sync::atomic::AtomicBool,
}

impl hoh_core::persistence::FaultHooks for ConflictAfterTemplateStarted {
    fn at(&self, job: &'static str, point: hoh_core::persistence::WritePoint) {
        use std::sync::atomic::Ordering::SeqCst;
        if job != "Runner"
            || point != hoh_core::persistence::WritePoint::Committed
            || self.fired.load(SeqCst)
        {
            return;
        }
        let attempt = self
            .world
            .query("SELECT attempt_id FROM attempts WHERE state='STARTING'")
            .unwrap_or_else(|e| panic!("read the attempt at the fault seam: {e}"));
        if attempt.is_empty() {
            return;
        }
        let key = hoh_core::ids::child_id(&attempt, "templates");
        if self
            .world
            .query(&format!(
                "SELECT state FROM effects WHERE effect_key='{key}'"
            ))
            .unwrap_or_else(|e| panic!("read the template effect at the fault seam: {e}"))
            == "STARTED"
        {
            assert!(!self.world.project().join("docs/acceptance.md").exists());
            std::fs::create_dir(self.world.project().join("docs/prd.md"))
                .unwrap_or_else(|e| panic!("inject the target conflict after STARTED: {e}"));
            self.fired.store(true, SeqCst);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn template_publication_failure_keeps_unknown_effect_and_never_starts_agent() -> R {
    let world = std::sync::Arc::new(world::World::named("m3t08-template-failure")?);
    world.configure(&[], "")?;
    world.git(&["init", "-q"])?;
    std::fs::create_dir_all(world.project().join("docs/spec"))?;
    // Sorted publication has already created several documents when it encounters this
    // late non-file target. Keep every original UNKNOWN_EFFECT assertion for this window.
    let fault = std::sync::Arc::new(ConflictAfterTemplateStarted {
        world: world.clone(),
        fired: std::sync::atomic::AtomicBool::new(false),
    });
    let mut config = hoh_app::host::HostConfig::new(&world.project());
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.store.faults = fault.clone();
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: world::Team::working(|_| Vec::new(), Duration::from_secs(60)),
        context: hoh_core::decision::Context {
            required_capabilities: world::TEAM
                .iter()
                .map(|(role, _)| ((*role).into(), Default::default()))
                .collect(),
            ..Default::default()
        },
    });
    let host = hoh_app::host::Host::start(config).await?;
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
        assert!(fault.fired.load(std::sync::atomic::Ordering::SeqCst));
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
