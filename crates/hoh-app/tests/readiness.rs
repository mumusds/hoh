//! The Readiness Gate on the product's tick (REQ-031 behaviour 5, docs/adr/ADR-035.md decision 8). TEST-031-02
//! (AC-031-02): an in-process host whose runner is the scripted Team, which takes work; the approved revision holds a
//! complete requirement and a required one without a criterion, its specification review PASS.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod pty;
mod world;

use pty::R;
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use world::World;

/// How long a test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(30);

/// Requirement group A (REQ-A, complete) and B (REQ-B1, required, no criterion).
fn spec() -> Vec<world::Requirement> {
    vec![
        world::Requirement {
            id: "REQ-A".to_owned(),
            title: "需求组 A".to_owned(),
            criteria: vec![world::Criterion {
                id: "AC-A-01".to_owned(),
                body: "A 完成".to_owned(),
                test_id: "TEST-A-01".to_owned(),
                passes: true,
            }],
        },
        world::Requirement {
            id: "REQ-B1".to_owned(),
            title: "需求组 B".to_owned(),
            criteria: Vec::new(),
        },
    ]
}

/// The Commander plans TA (REQ-A) and TB (REQ-B1) at its first turn and decides nothing else.
fn planning_commander() -> std::sync::Arc<world::Team> {
    world::Team::working(
        |turn| match turn.state.as_str() {
            "CREATED" => {
                let mut decided = world::steps("CREATED", "SPECIFYING");
                let task = |id: &str, requirement: &str| {
                    json!({"task_id": format!("{}-{id}", turn.run_id), "title": id, "role": "developer",
                           "writer": true, "depends_on": [], "requirement_ids": [requirement]})
                };
                decided.push(json!({"kind": "create_tasks", "plan": {"tasks": [
                    task("TA", "REQ-A"), task("TB", "REQ-B1")]}}));
                decided
            }
            _ => Vec::new(),
        },
        Duration::from_secs(60),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_031_02_unready_task_blocks_alone() -> R {
    let world = World::new()?;
    world.configure(&[], "")?;
    world.commit_project(&spec())?;
    let team = planning_commander();
    let host = world.team_host(team.clone(), None).await?;
    let outcome = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "readiness", "做 A 与 B").await?;
        world.staff(&run)?;
        let (ta, tb) = (format!("{run}-TA"), format!("{run}-TB"));
        // The plan imported in SPECIFYING: the run cannot be READY (REQ-B1 has no criterion).
        world
            .until(
                &format!("SELECT count(*) FROM tasks WHERE run_id = '{run}' AND role = 'developer'"),
                "2",
                WAIT,
            )
            .await?;
        // Given: the run at work (RUNNING), as one that started before REQ-B1 was approved without a criterion.
        world.exec(&format!(
            "UPDATE runs SET state = 'RUNNING', revision = revision + 1 WHERE run_id = '{run}'"
        ))?;
        // Rounds run: TA is dispatched; three more ticks' worth of rounds go by.
        world
            .until(
                &format!("SELECT count(*) > 0 FROM attempts WHERE task_id = '{ta}'"),
                "1",
                WAIT,
            )
            .await?;
        let ticks = Instant::now();
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(ticks.elapsed() >= Duration::from_secs(2));
        assert_eq!(
            world.query(&format!(
                "SELECT state || ' ' || coalesce(blocked_reason, '-') FROM tasks WHERE task_id = '{tb}'"
            ))?,
            "BLOCKED READINESS_NOT_MET"
        );
        let blocked: Vec<Value> = world
            .rows(&format!(
                "SELECT payload_json FROM events WHERE kind = 'task.blocked' \
                 AND json_extract(payload_json, '$.task_id') = '{tb}'"
            ))?
            .iter()
            .map(|r| serde_json::from_str(r["payload_json"].as_str().unwrap_or("null")))
            .collect::<Result<_, _>>()?;
        assert_eq!(blocked.len(), 1, "{blocked:?}");
        assert_eq!(
            blocked[0]["details"]["reasons"],
            json!([{"reason": "missing_ac", "ids": ["REQ-B1"]}]),
            "{blocked:?}"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM attempts WHERE task_id = '{tb}'"
            ))?,
            "0"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT state || ' ' || blocked_reasons_json FROM runs WHERE run_id = '{run}'"
            ))?,
            "RUNNING []"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE run_id = '{run}' AND kind = 'run.blocked'"
            ))?,
            "0"
        );
        if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&dir).join("test-031-02.json"),
                serde_json::to_vec_pretty(&json!({"run": run, "blocked": blocked}))?,
            )?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}
