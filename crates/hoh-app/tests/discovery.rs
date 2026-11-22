//! REQ-028 (project discovery) through the product host; no live agent runs and no project command runs unconfirmed.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::Host;
use hoh_core::specindex::{self, Index, Source};
use serde_json::{Value, json};
use std::time::Duration;
use world::{R, World};

const SPEC: &str = "# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n\
## REQ-A Requirement A\n### AC-A-01\ngiven: fixture\nwhen: operation\nthen: result\ncheck: unit\n";

/// A brownfield repository: code, a test, a README, docs/, a Cargo manifest, the approved specification's sources
/// and index; one remote; committed. Its index.
fn brownfield(world: &World) -> R<Index> {
    let source = Source {
        path: "docs/spec/spec.md".into(),
        bytes: SPEC.as_bytes().to_vec(),
    };
    let index = specindex::generate_index(std::slice::from_ref(&source))?;
    for (path, bytes) in [
        (source.path.as_str(), source.bytes.clone()),
        (specindex::PATH, index.bytes().to_vec()),
        (
            "Cargo.toml",
            b"[package]\nname = \"fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n".to_vec(),
        ),
        ("src/lib.rs", b"pub fn answer() -> u32 { 42 }\n".to_vec()),
        (
            "tests/answer.rs",
            b"#[test]\nfn answer() { assert_eq!(fixture::answer(), 42); }\n".to_vec(),
        ),
        ("README.md", b"# Fixture\n".to_vec()),
        ("docs/guide.md", b"# Guide\n".to_vec()),
        (".gitignore", b".hoh/\n".to_vec()),
    ] {
        let file = world.project().join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, bytes)?;
    }
    world.git(&["init", "-q"])?;
    world.git(&["add", "."])?;
    world.git(&["commit", "-qm", "brownfield"])?;
    world.git(&[
        "remote",
        "add",
        "origin",
        "https://example.invalid/fixture.git",
    ])?;
    Ok(index)
}

/// A command of `kind` for run `run` at its current revision.
async fn command(host: &Host, run: &str, key: &str, kind: &str, payload: Value) -> R<Value> {
    let (_, body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    Ok(
        json!({"schema_version":"1.0", "command_id":format!("cmd-{key}"), "idempotency_key":key,
        "expected_revision":body["revision"], "run_id":run, "kind":kind, "payload":payload}),
    )
}

/// A run with `index` approved by the user, its Commander bound.
async fn approved(world: &World, index: &Index) -> R<(Host, String)> {
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "discovery").await?;
    let body = command(
        &host,
        &run,
        "approve",
        "ApproveSpec",
        json!({"spec_sha256": index.sha256(), "decision": "APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{response}");
    // A new run of the project adopts its approved specification (CREATED).
    let run = world::start_run(&host, "brownfield", "discovery").await?;
    world.staff(&run)?;
    Ok((host, run))
}

/// The Commander's decision `payload` on run `run`, at its current revision.
async fn decide(
    world: &World,
    host: &Host,
    run: &str,
    id: &str,
    payload: Value,
) -> R<hoh_core::persistence::Decided> {
    let state = world.rows(&format!("SELECT r.revision,s.sha256,b.role_epoch FROM runs r JOIN specs s ON s.spec_id=r.spec_id JOIN role_bindings b ON b.run_id=r.run_id AND b.role='commander' WHERE r.run_id='{run}'"))?;
    let proposal = hoh_core::decision::Proposal {
        decision_id: id.into(),
        run_id: run.into(),
        expected_revision: state[0]["revision"].as_u64().ok_or("revision")?,
        role_epoch: state[0]["role_epoch"].as_u64().ok_or("epoch")?,
        spec_sha256: state[0]["sha256"].as_str().ok_or("digest")?.into(),
        candidate_id: None,
        causation_id: None,
        payload: hoh_core::protocol::decode(&payload)?,
    };
    Ok(host
        .store()
        .decide(proposal, hoh_core::decision::Context::default())
        .await?)
}

/// What discovery must leave as it was: `git status --porcelain`, the digests of the dirty and untracked files, the
/// commit count, `git remote -v` and the stash list.
fn observed(world: &World, files: &[&str]) -> R<Value> {
    let digests: Vec<String> = files
        .iter()
        .map(|f| Ok(world::sha256_hex(&std::fs::read(world.project().join(f))?)))
        .collect::<R<_>>()?;
    Ok(json!({
        "status": world.git(&["status", "--porcelain"])?,
        "digests": digests,
        "commits": world.git(&["rev-list", "--count", "HEAD"])?,
        "remotes": world.git(&["remote", "-v"])?,
        "stash": world.git(&["stash", "list"])?,
    }))
}

#[tokio::test]
async fn test_028_01_baseline_is_read_only() -> R {
    let world = World::new()?;
    let index = brownfield(&world)?;
    let (host, run) = approved(&world, &index).await?;
    // The user's uncommitted work: two modified tracked files and one untracked file.
    std::fs::write(
        world.project().join("src/lib.rs"),
        b"pub fn answer() -> u32 { 41 }\n",
    )?;
    std::fs::write(world.project().join("README.md"), b"# Fixture, edited\n")?;
    std::fs::write(world.project().join("notes.txt"), b"todo\n")?;
    let files = ["README.md", "src/lib.rs", "notes.txt"];
    let before = observed(&world, &files)?;
    let head = world.git(&["rev-parse", "HEAD"])?.trim().to_owned();
    let tree = world.git(&["rev-parse", "HEAD^{tree}"])?.trim().to_owned();
    assert_eq!(
        world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?,
        "CREATED"
    );
    // Entering DISCOVERING (the Commander's transition_run until M3-T04's Runtime step moves it).
    let entered = decide(
        &world,
        &host,
        &run,
        "discover",
        world::transition("DISCOVERING"),
    )
    .await?;
    assert!(
        matches!(entered, hoh_core::persistence::Decided::Applied { .. }),
        "{entered:?}"
    );
    world
        .until(
            &format!("SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline'"),
            "1",
            Duration::from_secs(20),
        )
        .await?;
    let baseline: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline'"
    ))?)?;
    let sha =
        |f: &str| -> R<String> { Ok(world::sha256_hex(&std::fs::read(world.project().join(f))?)) };
    assert_eq!(
        baseline,
        json!({
            "run_id": run,
            "head": head,
            "tree": tree,
            "dirty": [{"path": "README.md", "sha256": sha("README.md")?}, {"path": "src/lib.rs", "sha256": sha("src/lib.rs")?}],
            "untracked": [{"path": "notes.txt", "sha256": sha("notes.txt")?}],
            "build_commands": ["cargo build"],
            "test_commands": ["cargo test"],
            "docs": ["README.md", "docs/"],
        }),
        "{baseline}"
    );
    // Read only: nothing of the repository changed; no write task was made.
    assert_eq!(observed(&world, &files)?, before);
    assert_eq!(
        world.query(&format!("SELECT count(*) FROM tasks WHERE run_id='{run}'"))?,
        "0"
    );
    assert_eq!(
        world.query(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{run}'"))?,
        "0"
    );
    // One baseline per run, whatever the later ticks.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline'"
        ))?,
        "1"
    );
    // Leaving DISCOVERING after the baseline is a transition like any other.
    let left = decide(
        &world,
        &host,
        &run,
        "specify",
        world::transition("SPECIFYING"),
    )
    .await?;
    assert!(
        matches!(left, hoh_core::persistence::Decided::Applied { .. }),
        "{left:?}"
    );
    host.shutdown().await;
    Ok(())
}
