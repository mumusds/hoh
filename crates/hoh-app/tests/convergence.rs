//! Specification convergence (REQ-030, docs/adr/ADR-035.md): what an unattended run may assume. TEST-030-02
//! (AC-030-02): `hoh -p` against the world's in-process host whose runner is the scripted Team; the run assumes only
//! the defaults its configuration lists under `[assumptions]`, and a configuration listing a reserved class is refused
//! before anything is started.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod pty;
mod world;

use pty::R;
use serde_json::{Value, json};
use std::process::Stdio;
use std::time::{Duration, Instant};
use world::World;

/// How long a test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(30);

/// The product-direction question the scripted Commander of scenario A asks.
const DIRECTION: &str = "这个工具面向个人记账还是小团队共享账本？";

/// The run StartRun made for `goal`, once there is one.
fn run_of_goal(world: &World, goal: &str) -> R<String> {
    let sql = format!(
        "SELECT run_id FROM commands WHERE kind = 'StartRun' AND json_extract(payload_json, '$.goal') = '{goal}'"
    );
    let end = Instant::now() + WAIT;
    loop {
        let run = world.query(&sql)?;
        if !run.is_empty() {
            return Ok(run);
        }
        if Instant::now() > end {
            return Err(format!("no run for {goal}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `payload_json` column of the rows of `sql`, parsed.
fn payloads(world: &World, sql: &str) -> R<Vec<Value>> {
    world
        .rows(sql)?
        .into_iter()
        .map(|row| {
            Ok(serde_json::from_str(
                row["payload_json"].as_str().ok_or("no payload")?,
            )?)
        })
        .collect()
}

/// Scenario A's Commander: its turn at CREATED moves the run to SPECIFYING and, filling in the specification, records
/// the assumption assumptions.language, then one citing assumptions.product_direction (which the Runtime rejects), then
/// asks a person the product direction; every other turn submits nothing.
fn assuming_commander() -> std::sync::Arc<world::Team> {
    world::Team::new(|turn| match turn.state.as_str() {
        "CREATED" => {
            let mut decided = world::steps("CREATED", "SPECIFYING");
            decided.push(
                json!({"kind": "record_assumption", "statement": "实现语言为 Rust",
                "basis": "assumptions.language"}),
            );
            decided.push(world::rejected(json!({"kind": "record_assumption",
                "statement": "产品方向：个人记账", "basis": "assumptions.product_direction"})));
            decided.push(world::ask("q-direction", DIRECTION));
            decided
        }
        _ => Vec::new(),
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_030_02_unattended_assumes_only_listed_keys() -> R {
    // AC-030-02, project A: approved revision 1 (no language nor product-direction decision), [assumptions] lists
    // language = "Rust"; `hoh -p` runs until it stops.
    let world = World::new()?;
    world.configure(&[], "[assumptions]\nlanguage = \"Rust\"\n")?;
    world.commit_project(&world::fixture_spec())?;
    let team = assuming_commander();
    let host = world.team_host(team.clone(), None).await?;
    let outcome = tokio::task::block_in_place(|| -> R {
        world.seed_spec()?;
        let goal = "做一个记账工具";
        let child = world
            .command(&["-p", goal, "--output", "json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let run = run_of_goal(&world, goal)?;
        world.staff(&run)?;
        let out = world::finish(child, Duration::from_secs(60))?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // -p exits 3 and lists the product-direction question.
        assert_eq!(out.status.code(), Some(3), "{stderr}");
        assert!(stderr.contains(DIRECTION), "{stderr}");
        let result: Value = serde_json::from_slice(&out.stdout)?;
        assert_eq!(result["state"], "BLOCKED", "{result}");
        assert_eq!(result["blocked_reason"], "NEED_DECISION", "{result}");
        // The run is unattended, its snapshot listing language only.
        assert_eq!(
            world.query(&format!(
                "SELECT json_extract(config_snapshot_json, '$.unattended') || ' ' || \
                 json(json_extract(config_snapshot_json, '$.assumptions')) FROM runs WHERE run_id = '{run}'"
            ))?,
            r#"1 {"language":"Rust"}"#
        );
        // The language is recorded as an assumption whose basis holds assumptions.language: the only one.
        let applied = payloads(
            &world,
            &format!(
                "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'decision.applied' \
                 AND json_extract(payload_json, '$.kind') = 'record_assumption'"
            ),
        )?;
        assert_eq!(applied.len(), 1, "{applied:?}");
        assert!(
            applied[0]["payload"]["basis"]
                .as_str()
                .is_some_and(|b| b.contains("assumptions.language")),
            "{applied:?}"
        );
        // The product direction's record_assumption is REJECTED, its details naming product_direction as a key not
        // listed in [assumptions].
        let rejected = payloads(
            &world,
            &format!(
                "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'decision.rejected' \
                 AND json_extract(payload_json, '$.kind') = 'record_assumption'"
            ),
        )?;
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(
            rejected[0]["reason_code"], "ASSUMPTION_NOT_LISTED",
            "{rejected:?}"
        );
        assert_eq!(
            rejected[0]["details"],
            json!({"basis": "assumptions.product_direction", "key": "product_direction",
                "listed": ["language"]}),
            "{rejected:?}"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT group_concat(state || ':' || coalesce(reason_code, '-'), ',') FROM decisions \
                 WHERE run_id = '{run}' AND json_extract(payload_json, '$.kind') = 'record_assumption'"
            ))?,
            "APPLIED:-,REJECTED:ASSUMPTION_NOT_LISTED"
        );
        // The run is BLOCKED (NEED_DECISION) with the product-direction question persisted.
        assert_eq!(
            world.query(&format!(
                "SELECT state || ' ' || blocked_reasons_json FROM runs WHERE run_id = '{run}'"
            ))?,
            r#"BLOCKED ["NEED_DECISION"]"#
        );
        let asked = payloads(
            &world,
            &format!(
                "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'question.asked'"
            ),
        )?;
        assert_eq!(
            asked
                .iter()
                .map(|a| a["question"].clone())
                .collect::<Vec<_>>(),
            [json!(DIRECTION)],
            "{asked:?}"
        );
        // No new approved revision.
        assert_eq!(world.query("SELECT max(revision) FROM specs")?, "1");
        assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
        if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&dir).join("test-030-02-a.json"),
                serde_json::to_vec_pretty(&json!({
                    "exit": out.status.code(), "result": result, "stderr": stderr,
                    "applied": applied, "rejected": rejected, "asked": asked,
                }))?,
            )?;
        }
        Ok(())
    });
    host.shutdown().await;
    outcome?;
    assert!(team.refused().is_empty(), "{team:?}");
    Ok(())
}

#[test]
fn test_030_02_reserved_assumption_keys_refuse_config() -> R {
    // AC-030-02, projects B, C and D: [assumptions] lists product_direction, scope or non_goals; `hoh -p` exits with the
    // configuration error (78) naming the key path, before any host, database, assumption or dispatch.
    let mut seen = Vec::new();
    for name in ["product_direction", "scope", "non_goals"] {
        let world = World::new()?;
        world.configure(&world::TEAM, &format!("[assumptions]\n{name} = \"x\"\n"))?;
        let out = world.hoh(&["-p", "做一个记账工具"])?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(78), "{name}: {stderr}");
        assert!(out.stdout.is_empty(), "{name}");
        let key = format!("assumptions.{name}");
        assert!(
            stderr.contains("config.toml") && stderr.contains(&key),
            "{name}: {stderr}"
        );
        assert!(world.hosts()?.is_empty(), "{name} started a host");
        assert!(!world.db().exists(), "{name}: a database, so maybe a run");
        seen.push(json!({"key": key, "exit": out.status.code(), "stderr": stderr}));
    }
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join("test-030-02-bcd.json"),
            serde_json::to_vec_pretty(&seen)?,
        )?;
    }
    Ok(())
}
