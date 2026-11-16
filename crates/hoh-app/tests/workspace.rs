//! The Workspace with a host (docs/adr/ADR-031.md decision 13, slice S4's supporting tests; test plan "顶栏的
//! baseline"): list_runs' goal and accepted candidate. The world's plain host has no runner: nothing works or wakes.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use pty::R;
use serde_json::{Value, json};
use world::World;

/// A git commit or tree of 40 hex digits made of `c`.
fn hex40(c: char) -> String {
    c.to_string().repeat(40)
}

/// The runs, candidates and events of the baseline tests: C1 and C2 COMPLETED with ACCEPTED candidates (C1's later
/// one accepted at a smaller seq than C2's), C3 COMPLETED with none accepted, R RUNNING with an ACCEPTED candidate
/// accepted last of all; C1 has a StartRun with its goal.
const BASELINE_WORLD: &str = "
INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
  ('C1', :p, 'COMPLETED', 'AUTO', 3, '2026-11-01T00:00:00Z'),
  ('C2', :p, 'COMPLETED', 'AUTO', 3, '2026-11-02T00:00:00Z'),
  ('C3', :p, 'COMPLETED', 'AUTO', 3, '2026-11-03T00:00:00Z'),
  ('R', :p, 'RUNNING', 'AUTO', 2, '2026-11-04T00:00:00Z');
INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, expected_revision,
  result_json, accepted_at) VALUES
  ('k1', 'C1', 'k1', 'StartRun', '{\"goal\":\"目标一\",\"mode\":\"AUTO\",\"interactive\":false}', printf('%064d', 1), 0,
   NULL, '2026-11-01T00:00:00Z');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('c1a', 'C1', <a>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-01T01:00:00Z'),
  ('c1b', 'C1', <b>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-01T02:00:00Z'),
  ('c1r', 'C1', <f>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'REJECTED', '2026-11-01T03:00:00Z'),
  ('c2a', 'C2', <c>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-02T01:00:00Z'),
  ('c3r', 'C3', <e>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'REJECTED', '2026-11-03T01:00:00Z'),
  ('ra', 'R', <d>, <t>, printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/x', 'ACCEPTED', '2026-11-04T01:00:00Z');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  (:p, 'e1', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1a\",\"join_id\":\"j1\"}', '2026-11-01T01:00:00Z'),
  (:p, 'e2', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1b\",\"join_id\":\"j2\"}', '2026-11-01T02:00:00Z'),
  (:p, 'e3', 'C2', 'candidate.accepted', '{\"candidate_id\":\"c2a\",\"join_id\":\"j3\"}', '2026-11-02T01:00:00Z'),
  (:p, 'e4', 'C1', 'candidate.accepted', '{\"candidate_id\":\"c1r\",\"join_id\":\"j4\"}', '2026-11-01T03:00:00Z'),
  (:p, 'e5', 'R', 'candidate.accepted', '{\"candidate_id\":\"ra\",\"join_id\":\"j5\"}', '2026-11-04T01:00:00Z');
";

/// [`BASELINE_WORLD`] in the world's store.
fn seed_baseline(world: &World) -> R {
    let mut sql = BASELINE_WORLD.to_owned();
    for (name, c) in [
        ("<a>", 'a'),
        ("<b>", 'b'),
        ("<c>", 'c'),
        ("<d>", 'd'),
        ("<e>", 'e'),
        ("<f>", 'f'),
        ("<t>", '7'),
    ] {
        sql = sql.replace(name, &format!("'{}'", hex40(c)));
    }
    world.exec(&sql)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_runs_gives_each_runs_goal_and_a_completed_runs_latest_accepted_candidate() -> R {
    // Decision 13 (the top bar's baseline): list_runs' items carry the run's goal (its StartRun's) and, for a
    // COMPLETED run only, `accepted` — its ACCEPTED candidate with the greatest `candidate.accepted` seq, or null.
    let world = World::new()?;
    let host = world.start_plain_host(None).await?;
    seed_baseline(&world)?;
    let seq = |event: &str| -> R<i64> {
        Ok(world
            .query(&format!(
                "SELECT seq FROM events WHERE event_id = '{event}'"
            ))?
            .parse()?)
    };
    let (status, page) = world::exchange(&host, "GET", "/v1/runs", None).await?;
    assert_eq!(status, 200, "{page}");
    let item = |run: &str| -> R<Value> {
        Ok(page["items"]
            .as_array()
            .ok_or("no items")?
            .iter()
            .find(|i| i["run_id"] == run)
            .ok_or(format!("no item of {run}: {page}"))?
            .clone())
    };
    assert_eq!(
        item("C1")?["accepted"],
        json!({"candidate_id": "c1b", "git_commit": hex40('b'), "seq": seq("e2")?}),
        "C1's ACCEPTED candidate accepted last; a REJECTED one's later event does not count"
    );
    assert_eq!(
        item("C2")?["accepted"],
        json!({"candidate_id": "c2a", "git_commit": hex40('c'), "seq": seq("e3")?})
    );
    let c3 = item("C3")?;
    assert_eq!(
        c3.get("accepted"),
        Some(&Value::Null),
        "a COMPLETED run with no ACCEPTED candidate: null"
    );
    let r = item("R")?;
    assert_eq!(
        r.get("accepted"),
        None,
        "a run that is not COMPLETED carries none, whatever its candidates: {r}"
    );
    assert_eq!(item("C1")?["goal"], "目标一");
    for run in ["C2", "C3", "R"] {
        assert_eq!(item(run)?.get("goal"), Some(&Value::Null), "{run}");
    }
    host.shutdown().await;
    Ok(())
}
