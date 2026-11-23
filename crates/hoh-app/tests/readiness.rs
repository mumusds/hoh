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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_031_02_unready_task_blocks_alone() -> R {
    let world = World::new()?;
    world.configure(&[], "")?;
    world.commit_project(&spec())?;
    // AC-031-02 starts with an already-running plan. A live SPECIFYING prologue with a missing AC now correctly
    // starts AUTO drafting, so establish the given state before enabling the runner.
    world.init_store().await?;
    let spec_id = world.seed_spec()?;
    let setup = world.start_plain_host(None).await?;
    let run = world::start_run(&setup, "readiness", "做 A 与 B").await?;
    world.staff(&run)?;
    let (ta, tb) = (format!("{run}-TA"), format!("{run}-TB"));
    world.exec(&format!(
        "UPDATE runs SET state='RUNNING',revision=revision+1 WHERE run_id='{run}';
         INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) VALUES
           ('{ta}','{run}','{spec_id}','READY','developer',1,1),('{tb}','{run}','{spec_id}','READY','developer',1,2);
         INSERT INTO task_requirements(task_id,requirement_id,spec_id) VALUES
           ('{ta}','REQ-A','{spec_id}'),('{tb}','REQ-B1','{spec_id}');
         INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES
           (:p,'{run}','readiness-plan','plan.imported',json_object('run_id','{run}','tasks',json_array(
             json_object('task_id','{ta}','paths',json_array('src/a')),
             json_object('task_id','{tb}','paths',json_array('src/b')))),'2026-11-23T00:00:00Z');"
    ))?;
    setup.shutdown().await;
    let host = world
        .team_host(
            world::Team::working(|_| Vec::new(), Duration::from_secs(60)),
            None,
        )
        .await?;
    let outcome = async {
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
