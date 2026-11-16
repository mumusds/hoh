//! The in-process product host's stop (docs/adr/ADR-016.md decision 12; ADR-019): once `Host::shutdown` has returned,
//! nothing of the host writes the project any more. A turn its executor started runs on after it (an agent outlives
//! its host), but what the host would have done at that turn's end (its raw logs kept as objects) is not done by a host
//! that has stopped: the next host's reconcile settles the attempt. Found by the M2-T13 test world: a world whose
//! developer turn ended while the world was being removed was left behind (World::drop's report: a new
//! `.hoh/objects/staging/.<pid>.<n>` of the test's own process).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use pty::R;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use world::World;

/// Every path under `dir` (none when it does not exist), sorted.
fn paths(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                todo.push(path.clone());
            }
            out.push(path);
        }
    }
    out.sort();
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_that_ends_after_its_host_stopped_has_nothing_written_for_it() -> R {
    // The product's world: a run whose Developer works 3 s; the host stops while that turn is in flight.
    let world = World::new()?;
    world.commit_project(&world::fixture_spec())?;
    world.developer(&world::Developer {
        seconds: 3.0,
        ..world::Developer::default()
    })?;
    world.register()?;
    let host = world.start_host().await?;
    let started = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "s", "完成fixture目标").await?;
        let attempt = format!("{run}-develop/attempt-1");
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'"),
                "RUNNING",
                Duration::from_secs(60),
            )
            .await?;
        // Its turn has begun its work (the fake Developer logged its start).
        let end = Instant::now() + Duration::from_secs(30);
        while !world
            .log("calls.jsonl")?
            .iter()
            .any(|c| c["attempt_id"] == attempt.as_str())
        {
            if Instant::now() > end {
                return Err("the developer turn never started".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        R::Ok(attempt)
    }
    .await;
    host.shutdown().await;
    let attempt = started?;
    let objects = world.project().join(".hoh/objects");
    let at_stop = paths(&objects);
    // The turn ends on its own (its agent outlives the host), then a while for anything that would follow it.
    let end = Instant::now() + Duration::from_secs(30);
    while !world
        .log("turns.jsonl")?
        .iter()
        .any(|t| t["attempt_id"] == attempt.as_str() && t["event"] == "end")
    {
        if Instant::now() > end {
            return Err("the developer turn did not end".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let later = paths(&objects);
    let new: Vec<&PathBuf> = later.iter().filter(|p| !at_stop.contains(p)).collect();
    assert!(
        new.is_empty(),
        "written into {} after the host stopped: {new:?}",
        objects.display()
    );
    Ok(())
}
