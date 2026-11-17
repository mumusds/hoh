//! TEST-021-06 (AC-021-06, docs/adr/ADR-032.md's test plan): the Commander's turns launch in the read-only workspace
//! profile through the product executor, on the M2-T13 test world's fakes (their help and schema the recordings, or
//! a [`world::Variant`] of them): (a) claude, codex App Server and pi as the Commander, woken by need_decision twice,
//! the second resuming its session; its decisions are its final message's hoh-decisions block, a line written to its
//! attempt's messages.jsonl is ignored; (b) an agent whose help or schema lacks one read-only option is no Commander
//! and still a Developer; (c) codex's host check of its sandbox (unconfined, unavailable, unknown, recovered; lost
//! after an assignment); (d) codex on exec, from the start or degraded in the run, is no Commander.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::Host;
use pty::R;
use serde_json::{Value, json};
use std::time::Duration;
use world::World;

/// How long a test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(120);

/// The YOLO options no Commander launch carries (AC-021-06 (a)).
const YOLO: [&str; 3] = [
    "--dangerously-skip-permissions",
    "--dangerously-bypass-approvals-and-sandbox",
    "danger-full-access",
];

/// `world`'s committed project with its specification's rows, its agents registered and its in-process product host
/// started; a run started on it, which adopted the specification: the host and the run's id.
async fn adopted_run(world: &World, key: &str) -> R<(Host, String)> {
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    let host = world.start_host().await?;
    let started = async {
        world.seed_spec()?;
        world::start_run(&host, key, "完成fixture目标").await
    }
    .await;
    match started {
        Ok(run) => Ok((host, run)),
        Err(e) => {
            host.shutdown().await;
            Err(e)
        }
    }
}

/// A need_decision of fingerprint `fp` in run `run`.
fn need_decision(world: &World, run: &str, fp: &str) -> R {
    world.exec(&format!(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES \
           (:p, 'nd-{fp}', '{run}', 'need_decision', '{{\"fingerprint\": \"{fp}\"}}', '2026-11-17T00:00:00Z');"
    ))
}

/// The attempt of the Commander batch the fake wrote for the wake of need_decision `fp`, once there is one.
async fn woken_for(world: &World, fp: &str) -> R<String> {
    let end = std::time::Instant::now() + WAIT;
    loop {
        if let Some(batch) = world
            .log("commander.jsonl")?
            .into_iter()
            .find(|b| b["fingerprint"] == json!(fp))
        {
            return Ok(batch["attempt_id"].as_str().ok_or("no attempt")?.to_owned());
        }
        if std::time::Instant::now() > end {
            return Err(
                format!("no Commander turn for {fp}: {:?}", world.log("turns.jsonl")).into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The Commander's attempts of run `run`, in order.
fn commander_attempts(world: &World, run: &str) -> R<Vec<String>> {
    Ok(world
        .rows(&format!(
            "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.run_id = '{run}' AND t.role = 'commander' ORDER BY a.rowid"
        ))?
        .into_iter()
        .filter_map(|r| r["attempt_id"].as_str().map(str::to_owned))
        .collect())
}

/// The launch Commander attempt `attempt` recorded with its prompt (its `context.packed` event's `read_only`): the
/// profile and, for codex, this host's check of its sandbox with its reason (docs/adr/ADR-032.md decision 4).
fn read_only_record(world: &World, attempt: &str) -> R<Value> {
    let rows = world.rows(&format!(
        "SELECT payload_json FROM events WHERE kind = 'context.packed' \
         AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
    ))?;
    let payload: Value = serde_json::from_str(
        rows.first()
            .and_then(|r| r["payload_json"].as_str())
            .ok_or(format!("no context.packed of {attempt}"))?,
    )?;
    Ok(payload["read_only"].clone())
}

/// The read-only argv of REQ-021 behaviour 9 for `agent`'s turn (claude: resuming `resume`, without partial messages,
/// which a Commander's turn does not want; pi: its session's directory and id), without a model.
fn read_only_argv(agent: &str, resume: Option<&str>, pi: Option<(&str, &str)>) -> Vec<String> {
    let mut argv: Vec<String> = match agent {
        "claude" => [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--tools",
            "Read,Grep,Glob",
            "--allowedTools",
            "Read,Grep,Glob",
            "--permission-mode",
            "dontAsk",
            "--strict-mcp-config",
        ]
        .iter()
        .map(|a| (*a).to_owned())
        .collect(),
        _ => {
            let (dir, id) = pi.unwrap_or_default();
            [
                "--mode",
                "rpc",
                "--session-dir",
                dir,
                "--session-id",
                id,
                "--tools",
                "read,grep,find,ls",
                "--no-mcp",
                "--no-extensions",
            ]
            .iter()
            .map(|a| (*a).to_owned())
            .collect()
        }
    };
    if let Some(id) = resume {
        argv.extend(["--resume".to_owned(), id.to_owned()]);
    }
    argv
}

/// AC-021-06 (a) on `agent`: the Commander is `agent`, the Developer claude. The pinned Commander moves the run to
/// RUNNING (its first turn, a new session); once the Developer works, a need_decision fp-ro-1 wakes it and its final message's block has a
/// record_assumption, while it also writes another valid decision of its nonce to the attempt's messages.jsonl; a
/// need_decision fp-ro-2 wakes it again. Every Commander turn launches exactly as REQ-021 behaviour 9's line (the
/// first new, the others resuming its session), with no YOLO option; the Developer's carries its YOLO option; the
/// block's decision is APPLIED; the file's line is no output and is recorded once as supervisor.bridge_ignored.
async fn read_only_launches(agent: &str) -> R {
    let world = World::new()?;
    let mut team = world::TEAM.to_vec();
    team[0] = ("commander", agent);
    world.configure(&team, "")?;
    let statement = "只读档下的假设：块里的决策才登记";
    world.commander(&world::Commander {
        statement: statement.to_owned(),
        assume_on: Some("fp-ro-1".to_owned()),
        bridge_also: true,
        ..world::Commander::default()
    })?;
    // The Developer works on past the test: while it does, nothing else moves the run, so the Commander's decision
    // names the run's revision.
    world.developer(&world::Developer {
        seconds: 600.0,
        commit: false,
        ..world::Developer::default()
    })?;
    let (host, run) = adopted_run(&world, "ro-a").await?;
    let outcome = async {
        world
            .until(
                &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
                "RUNNING",
                WAIT,
            )
            .await?;
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{run}-develop/attempt-1'"),
                "RUNNING",
                WAIT,
            )
            .await?;
        need_decision(&world, &run, "fp-ro-1")?;
        let first = woken_for(&world, "fp-ro-1").await?;
        world
            .until(
                &format!(
                    "SELECT state FROM decisions WHERE run_id = '{run}' \
                     AND json_extract(payload_json, '$.kind') = 'record_assumption'"
                ),
                "APPLIED",
                WAIT,
            )
            .await?;
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{first}'"),
                "SETTLED",
                WAIT,
            )
            .await?;
        need_decision(&world, &run, "fp-ro-2")?;
        let second = woken_for(&world, "fp-ro-2").await?;
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{second}'"),
                "SETTLED",
                WAIT,
            )
            .await?;
        let attempts = commander_attempts(&world, &run)?;
        assert!(attempts.len() >= 3, "{attempts:?}");
        assert!(attempts.contains(&first) && attempts.contains(&second));
        // The Commander's session: one, every turn of it.
        let session = world.rows(&format!(
            "SELECT s.session_id, s.native_id FROM role_bindings b JOIN sessions s ON s.session_id = b.session_id \
             WHERE b.run_id = '{run}' AND b.role = 'commander'"
        ))?;
        let native = session[0]["native_id"].as_str().ok_or("no native id")?.to_owned();
        let turns = world.log("turns.jsonl")?;
        let started_for = |attempt: &str| -> R<Value> {
            Ok(turns
                .iter()
                .find(|t| t["event"] == "start" && t["attempt_id"] == json!(attempt))
                .cloned()
                .ok_or(format!("no turn of {attempt}: {turns:?}"))?)
        };
        let mut launches = Vec::new();
        for (i, attempt) in attempts.iter().enumerate() {
            let turn = started_for(attempt)?;
            let argv: Vec<String> = serde_json::from_value(turn["argv"].clone())?;
            for yolo in YOLO {
                assert!(!argv.iter().any(|a| a.contains(yolo)), "{attempt}: {argv:?}");
            }
            match agent {
                "claude" => {
                    let resume = (i > 0).then_some(native.as_str());
                    assert_eq!(argv, read_only_argv("claude", resume, None), "{attempt}");
                }
                "pi" => {
                    let dir = argv
                        .iter()
                        .position(|a| a == "--session-dir")
                        .and_then(|at| argv.get(at + 1))
                        .ok_or("no --session-dir")?;
                    assert_eq!(
                        argv,
                        read_only_argv("pi", None, Some((dir, &native))),
                        "{attempt}"
                    );
                }
                _ => {}
            }
            let record = read_only_record(&world, attempt)?;
            assert_eq!(record["profile"], json!("read_only"), "{attempt}: {record}");
            let check = if agent == "codex" { json!("confined") } else { Value::Null };
            assert_eq!(record["host_check"]["check"], check, "{attempt}: {record}");
            launches.push(json!({"attempt": attempt, "argv": argv}));
        }
        if agent == "pi" {
            let dirs: std::collections::BTreeSet<String> =
                launches.iter().map(|l| l["argv"][3].to_string()).collect();
            assert_eq!(dirs.len(), 1, "{launches:?}");
        }
        if agent == "codex" {
            // Each Commander turn's App Server: thread/start for the first, thread/resume of the session's thread for
            // the others, with exactly the read-only parameters.
            let requests = world.log("codex-requests.jsonl")?;
            for (i, attempt) in attempts.iter().enumerate() {
                let thread: Vec<&Value> = requests
                    .iter()
                    .filter(|r| {
                        r["marker"]
                            .as_str()
                            .is_some_and(|m| m.ends_with(&format!("/{attempt}")))
                            && r["params"].is_object()
                    })
                    .collect();
                assert_eq!(thread.len(), 1, "{attempt}: {requests:?}");
                let want = if i == 0 {
                    ("thread/start", json!({"approvalPolicy": "never", "sandbox": "read-only"}))
                } else {
                    (
                        "thread/resume",
                        json!({"approvalPolicy": "never", "sandbox": "read-only", "threadId": native}),
                    )
                };
                assert_eq!(
                    (thread[0]["method"].as_str(), &thread[0]["params"]),
                    (Some(want.0), &want.1),
                    "{attempt}"
                );
                assert!(!thread[0].to_string().contains("danger-full-access"));
                launches.push(json!({"attempt": attempt, "thread": thread[0]}));
            }
        }
        // The Developer's turns launch with the YOLO option.
        let developer: Vec<&Value> = turns
            .iter()
            .filter(|t| t["event"] == "start" && t["role"] == "developer")
            .collect();
        assert!(!developer.is_empty(), "{turns:?}");
        for turn in &developer {
            assert!(
                turn["argv"]
                    .as_array()
                    .is_some_and(|a| a.contains(&json!("--dangerously-skip-permissions"))),
                "{turn}"
            );
        }
        // The block's record_assumption is applied; the line in the file is never an output, and is recorded once.
        let decisions = world.rows(&format!(
            "SELECT decision_id, state, json_extract(payload_json, '$.statement') AS statement FROM decisions \
             WHERE run_id = '{run}' AND json_extract(payload_json, '$.kind') = 'record_assumption'"
        ))?;
        assert_eq!(
            decisions
                .iter()
                .map(|d| (d["state"].clone(), d["statement"].clone()))
                .collect::<Vec<_>>(),
            [(json!("APPLIED"), json!(statement))],
            "{decisions:?}"
        );
        assert!(
            decisions[0]["decision_id"]
                .as_str()
                .is_some_and(|d| d.starts_with(&first)),
            "{decisions:?}"
        );
        let outputs = world.rows(&format!(
            "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'supervisor.output'"
        ))?;
        assert!(!outputs.is_empty());
        assert!(
            outputs
                .iter()
                .all(|o| !o["payload_json"].as_str().unwrap_or_default().contains("-bridge")),
            "{outputs:?}"
        );
        let ignored = world.rows(&format!(
            "SELECT json_extract(payload_json, '$.attempt_id') AS attempt FROM events \
             WHERE run_id = '{run}' AND kind = 'supervisor.bridge_ignored'"
        ))?;
        assert_eq!(ignored, [json!({"attempt": first})], "{ignored:?}");
        eprintln!("{agent}: {}", serde_json::to_string(&launches)?);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_a_claude_commander_launches_and_resumes_read_only() -> R {
    read_only_launches("claude").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_a_codex_commander_launches_and_resumes_read_only() -> R {
    read_only_launches("codex").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_a_pi_commander_launches_and_resumes_read_only() -> R {
    read_only_launches("pi").await
}

/// The world's team with `commander` the Commander and `developer` the Developer, every other role as
/// [`world::TEAM`] has it.
fn team_with(
    commander: &'static str,
    developer: &'static str,
) -> Vec<(&'static str, &'static str)> {
    let mut team = world::TEAM.to_vec();
    team[0] = ("commander", commander);
    team[2] = ("developer", developer);
    team
}

/// The world's team with `agent` both the Commander and the Developer.
fn team_of(agent: &'static str) -> Vec<(&'static str, &'static str)> {
    team_with(agent, agent)
}

/// An agent other than `agent` that can be the Commander in its worlds.
fn other_than(agent: &str) -> &'static str {
    match agent {
        "claude" => "codex",
        _ => "claude",
    }
}

/// Run `run` of the world's committed specification (its rows seeded), RUNNING, bound as StartRun binds `team` at the
/// project's current team revision with no session yet (staffing grants them), and its developer task `task` READY
/// for REQ-901: no wake is owed to its Commander before staffing judges it.
fn seeded_run(world: &World, run: &str, task: &str, team: &[(&str, &str)]) -> R {
    let revision = "(SELECT max(revision) FROM team_revisions WHERE state = 'APPLIED')";
    let bindings: Vec<String> = team
        .iter()
        .map(|(role, agent)| {
            format!("('{run}', '{role}', '{agent}', 1, {revision}, '2026-11-17T00:00:00Z')")
        })
        .collect();
    world.exec(&format!(
        "INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, config_revision, created_at) VALUES \
           ('{run}', :p, 'spec-world', 'RUNNING', 'AUTO', 1, {revision}, '2026-11-17T00:00:00Z');
         INSERT INTO role_bindings(run_id, role, agent, role_epoch, config_revision, bound_at) VALUES {};
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES \
           ('{task}', '{run}', 'spec-world', 'READY', 1, 'developer', 1, \
            (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{task}', 'REQ-901', 'spec-world');",
        bindings.join(", ")
    ))
}

/// The commander binding of run `run`, once it is BLOCKED with the missing line `<agent>:CAPABILITY_UNSUPPORTED:
/// read_only_workspace` (and only it), and the probe evidence its team.binding_blocked recorded for `agent`.
async fn commander_blocked(world: &World, run: &str, agent: &str) -> R<(Value, Value)> {
    let line = format!("{agent}:CAPABILITY_UNSUPPORTED:read_only_workspace");
    world
        .until(
            &format!(
                "SELECT state || ' ' || blocked_reason || ' ' || missing_json FROM role_bindings \
                 WHERE run_id = '{run}' AND role = 'commander'"
            ),
            &format!("BLOCKED BLOCKED_CAPABILITY [\"{line}\"]"),
            WAIT,
        )
        .await?;
    let binding = world
        .rows(&format!(
            "SELECT agent, role_epoch, state, blocked_reason, missing_json FROM role_bindings \
             WHERE run_id = '{run}' AND role = 'commander'"
        ))?
        .remove(0);
    let blocked = world.rows(&format!(
        "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'team.binding_blocked' \
         AND json_extract(payload_json, '$.role') = 'commander' ORDER BY seq DESC LIMIT 1"
    ))?;
    let payload: Value = serde_json::from_str(
        blocked[0]["payload_json"]
            .as_str()
            .ok_or("no team.binding_blocked")?,
    )?;
    assert_eq!(payload["missing"], json!([line]), "{payload}");
    let sha256 = payload["probe_sha256"]
        .as_str()
        .ok_or("no probe evidence")?;
    let probe: Value = serde_json::from_slice(&hoh_core::objects::get(&world.project(), sha256)?)?;
    let evidence = probe
        .as_array()
        .and_then(|items| items.iter().find(|i| i["agent"] == agent))
        .cloned()
        .ok_or(format!("no {agent} in the evidence {probe}"))?;
    assert_eq!(evidence["line"], json!(line), "{evidence}");
    Ok((binding, evidence))
}

/// Run R, where `agent` is the Commander and the Developer: its commander binding BLOCKED for the read-only profile
/// (the evidence `agent`'s), its developer binding staffed all the same, and the run BLOCKED(COMMANDER_UNAVAILABLE)
/// since the block itself wakes the Commander (docs/adr/ADR-025.md decision 5), so no turn of R starts; no Commander
/// attempt or turn: the evidence, and the developer binding's session.
async fn no_commander_in_r(world: &World, agent: &str) -> R<(Value, String)> {
    let (binding, evidence) = commander_blocked(world, "R", agent).await?;
    assert_eq!(binding["agent"], json!(agent));
    world
        .until(
            "SELECT state || ' ' || (session_id IS NOT NULL) FROM role_bindings WHERE run_id = 'R' \
             AND role = 'developer'",
            "ACTIVE 1",
            WAIT,
        )
        .await?;
    world
        .until(
            "SELECT state || ' ' || (instr(blocked_reasons_json, 'COMMANDER_UNAVAILABLE') > 0) FROM runs \
             WHERE run_id = 'R'",
            "BLOCKED 1",
            WAIT,
        )
        .await?;
    assert!(commander_attempts(world, "R")?.is_empty());
    let turns = world.log("turns.jsonl")?;
    assert!(!turns.iter().any(|t| t["role"] == "commander"), "{turns:?}");
    Ok((evidence, developer_session(world, "R")?))
}

/// Developer task `task`'s first attempt that SETTLED, once one did (its own attempts, not its verification round's):
/// its attempt id, its session's id and its agent.
async fn developer_settled(world: &World, task: &str) -> R<(String, String, String)> {
    let settled = format!(
        "SELECT attempt_id, session_id, agent FROM attempts WHERE task_id = '{task}' \
         AND attempt_id GLOB '{task}/attempt-*' AND state = 'SETTLED' ORDER BY rowid"
    );
    world
        .until(&format!("SELECT count(*) > 0 FROM ({settled})"), "1", WAIT)
        .await
        .map_err(|e| {
            format!(
                "{e}; attempts: {:?}; turns: {:?}; events: {:?}",
                world.rows("SELECT attempt_id, state, failure_reason FROM attempts"),
                world.log("turns.jsonl"),
                world.rows("SELECT seq, run_id, kind, substr(payload_json, 1, 300) AS payload FROM events ORDER BY seq")
            )
        })?;
    let settled = world.rows(&settled)?.remove(0);
    let text = |key: &str| -> R<String> {
        Ok(settled[key].as_str().ok_or(format!("no {key}"))?.to_owned())
    };
    Ok((text("attempt_id")?, text("session_id")?, text("agent")?))
}

/// `agent`'s entry in the project's compat manifest: its suite's status and its read_only case's.
fn manifest_statuses(world: &World, agent: &str) -> R<(Value, Value)> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(
        world.project().join(".hoh/compat-manifest.json"),
    )?)?;
    let suite = &manifest["agents"][agent]["suite"];
    let read_only = suite["cases"]
        .as_array()
        .and_then(|cases| cases.iter().find(|c| c["case"] == "read_only"))
        .map(|c| c["status"].clone())
        .ok_or(format!("no read_only case for {agent}: {suite}"))?;
    Ok((suite["status"].clone(), read_only))
}

/// The developer binding's session of run `run`.
fn developer_session(world: &World, run: &str) -> R<String> {
    world.query(&format!(
        "SELECT session_id FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"
    ))
}

/// AC-021-06 (b): `variant` takes one read-only option out of `agent`'s help or schema; the project's team has
/// `agent` both the Commander and the Developer, with no fallback. The compat suite passes and its read_only case
/// fails. In run R (bound as the team is) the commander binding is BLOCKED with `<agent>:CAPABILITY_UNSUPPORTED:
/// read_only_workspace` and no Commander attempt starts; the Developer binding is staffed with `agent` all the same,
/// but R is BLOCKED(COMMANDER_UNAVAILABLE) by its Commander's block, so `agent`'s Developer turn is shown in run S,
/// whose Commander is another agent: it starts and SETTLES.
async fn missing_option(variant: world::Variant, agent: &'static str) -> R {
    let world = World::new()?;
    world.variant(variant)?;
    world.configure(&team_of(agent), "")?;
    world.developer(&world::Developer {
        commit: false,
        ..world::Developer::default()
    })?;
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    assert_eq!(
        manifest_statuses(&world, agent)?,
        (json!("PASS"), json!("FAIL"))
    );
    let host = world.start_host().await?;
    let outcome = async {
        world.seed_spec()?;
        seeded_run(&world, "R", "D", &team_of(agent))?;
        seeded_run(&world, "S", "E", &team_with(other_than(agent), agent))?;
        let (evidence, _) = no_commander_in_r(&world, agent).await?;
        assert_eq!(
            evidence["probe"]["capabilities"]["read_only_workspace"],
            json!(false),
            "{evidence}"
        );
        let (_, session, ran_on) = developer_settled(&world, "E").await?;
        assert_eq!(
            (session, ran_on.as_str()),
            (developer_session(&world, "S")?, agent)
        );
        assert!(commander_attempts(&world, "R")?.is_empty());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_b_claude_without_strict_mcp_config_is_no_commander_and_still_a_developer() -> R
{
    missing_option(world::Variant::ClaudeWithoutStrictMcpConfig, "claude").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_b_pi_without_no_mcp_is_no_commander_and_still_a_developer() -> R {
    missing_option(world::Variant::PiWithoutNoMcp, "pi").await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_b_codex_without_a_read_only_sandbox_is_no_commander_and_still_a_developer() -> R
{
    missing_option(world::Variant::CodexWithoutReadOnlySandbox, "codex").await
}

/// A registry of `world`'s agents whose host check of codex's sandbox runs again at every look while it is not
/// confined (the product's: at most every 60 s).
fn quick_registry(world: &World) -> std::sync::Arc<hoh_app::host::agents::Registry> {
    std::sync::Arc::new(world.registry().with_host_check_every(Duration::ZERO))
}

/// AC-021-06 (c) with codex's host check `mode` (unconfined or unavailable) from the start; the project's team has
/// codex the Commander and the Developer. In run R the commander binding is BLOCKED with
/// `codex:CAPABILITY_UNSUPPORTED:read_only_workspace`, its evidence naming the check's conclusion, and no Commander
/// attempt starts; codex's read_only case still PASSES and a codex session records the profile (the host's fact only
/// staffs); codex's Developer turn SETTLES in run S, whose Commander is claude. With `recover`, the check is then made
/// confined: at the next refresh R's binding is unblocked, a Commander attempt of R starts and SETTLES, and R's
/// Developer turn SETTLES in the session its binding had all along.
async fn host_check_blocks(mode: &str, recover: bool) -> R {
    let world = World::new()?;
    world.configure(&team_of("codex"), "")?;
    world.developer(&world::Developer {
        commit: false,
        ..world::Developer::default()
    })?;
    world.sandbox(mode)?;
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    assert_eq!(
        manifest_statuses(&world, "codex")?,
        (json!("PASS"), json!("PASS"))
    );
    let registry = quick_registry(&world);
    let host = world
        .start_host_with(world.host_config_with(registry, None)?)
        .await?;
    let outcome = async {
        world.seed_spec()?;
        seeded_run(&world, "R", "D", &team_of("codex"))?;
        seeded_run(&world, "S", "E", &team_with("claude", "codex"))?;
        let (evidence, session) = no_commander_in_r(&world, "codex").await?;
        assert_eq!(evidence["probe"]["host_check"]["check"], json!(mode), "{evidence}");
        assert_eq!(
            evidence["probe"]["capabilities"]["read_only_workspace"],
            json!(true),
            "{evidence}"
        );
        let (_, used, ran_on) = developer_settled(&world, "E").await?;
        assert_eq!((used.as_str(), ran_on.as_str()), (developer_session(&world, "S")?.as_str(), "codex"));
        for session in [&session, &used] {
            let capabilities: Value = serde_json::from_str(&world.query(&format!(
                "SELECT capabilities_json FROM sessions WHERE session_id = '{session}'"
            ))?)?;
            assert_eq!(capabilities["read_only_workspace"], json!(true), "{capabilities}");
        }
        if recover {
            world.sandbox("confined")?;
            world
                .until(
                    "SELECT count(*) > 0 FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                     WHERE t.run_id = 'R' AND t.role = 'commander' AND a.state = 'SETTLED'",
                    "1",
                    WAIT,
                )
                .await?;
            // Unblocked by its session's grant (docs/adr/ADR-030.md decision 7: a vacancy's block ends with it).
            assert_eq!(
                world.query(
                    "SELECT state || ' ' || (session_id IS NOT NULL) FROM role_bindings \
                     WHERE run_id = 'R' AND role = 'commander'"
                )?,
                "ACTIVE 1"
            );
            assert_eq!(
                world.query(
                    "SELECT count(*) FROM events WHERE run_id = 'R' AND kind = 'team.session_bound' \
                     AND json_extract(payload_json, '$.role') = 'commander' AND seq > (SELECT max(seq) FROM events \
                     WHERE run_id = 'R' AND kind = 'team.binding_blocked')"
                )?,
                "1"
            );
            let (_, used, _) = developer_settled(&world, "D").await?;
            assert_eq!(used, session);
            assert_eq!(developer_session(&world, "R")?, session);
        } else {
            tokio::time::sleep(Duration::from_secs(2)).await;
            assert!(commander_attempts(&world, "R")?.is_empty());
        }
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind = 'team.binding_excluded'")?,
            "0"
        );
        assert_eq!(
            manifest_statuses(&world, "codex")?,
            (json!("PASS"), json!("PASS"))
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_c_an_unconfined_host_check_blocks_only_the_codex_commander() -> R {
    host_check_blocks("unconfined", false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_c_an_unavailable_host_check_blocks_the_codex_commander_until_it_is_confined()
-> R {
    host_check_blocks("unavailable", true).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_c_an_unknown_host_check_keeps_codex_the_commander() -> R {
    // AC-021-06 (c), unknown: codex is the Commander as usual (the pinned script moves the run to RUNNING) and the
    // Developer; the registry's evidence of codex records unknown with its reason.
    let world = World::new()?;
    world.configure(&team_of("codex"), "")?;
    world.developer(&world::Developer {
        commit: false,
        ..world::Developer::default()
    })?;
    world.sandbox("unknown")?;
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    let registry = quick_registry(&world);
    let host = world
        .start_host_with(world.host_config_with(registry.clone(), None)?)
        .await?;
    let outcome = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "ro-c-unknown", "完成fixture目标").await?;
        world
            .until(
                &format!("SELECT state FROM runs WHERE run_id = '{run}'"),
                "RUNNING",
                WAIT,
            )
            .await?;
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{run}-develop/attempt-1'"),
                "SETTLED",
                WAIT,
            )
            .await?;
        let commanders = commander_attempts(&world, &run)?;
        assert!(!commanders.is_empty());
        let turns = world.log("turns.jsonl")?;
        assert!(
            turns
                .iter()
                .any(|t| t["role"] == "commander" && t["agent"] == "codex-app-server"),
            "{turns:?}"
        );
        // The evidence of the unknown check, kept with each Commander attempt's own record: its context.packed
        // names the profile it launched in and this host's check, with its reason.
        for attempt in &commanders {
            let launch = read_only_record(&world, attempt)?;
            assert_eq!(launch["profile"], json!("read_only"), "{attempt}: {launch}");
            assert_eq!(
                launch["host_check"]["check"],
                json!("unknown"),
                "{attempt}: {launch}"
            );
            assert!(
                launch["host_check"]["reason"]
                    .as_str()
                    .is_some_and(|r| r.contains("sandbox")),
                "{attempt}: {launch}"
            );
        }
        let snapshot = registry.snapshot(0);
        assert_eq!(
            snapshot.probes["codex"]["host_check"]["check"],
            json!("unknown")
        );
        // Not confined: looked at again while the run went on.
        let calls = std::fs::read_to_string(world.fakes().join("sandbox-calls"))?;
        assert!(calls.lines().count() >= 2, "{calls}");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_c_a_check_lost_after_the_assignment_refuses_the_turn_unavailable_then_recovers()
-> R {
    // AC-021-06 (c), -201: the check is unknown when the Commander's first turn is assigned; between the assignment and
    // its prepare a refresh finds it unavailable. prepare refuses the turn AGENT_UNAVAILABLE (a host fact), no
    // team.binding_excluded is written and staffing then BLOCKS the binding; once the check is confined, the next
    // refresh unblocks it and a Commander attempt of the same codex version is dispatched again and SETTLES. The
    // Developer's session is the same throughout.
    let world = World::new()?;
    world.configure(&team_of("codex"), "")?;
    world.developer(&world::Developer {
        commit: false,
        ..world::Developer::default()
    })?;
    world.sandbox("unknown")?;
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    let registry = quick_registry(&world);
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hook: hoh_app::host::product::PreparingSeen = {
        let (registry, fired, mode) = (
            registry.clone(),
            fired.clone(),
            world.fakes().join("sandbox"),
        );
        std::sync::Arc::new(move |role: &str, _attempt: &str| {
            if role == "commander" && !fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                let staged = mode.with_extension("lost");
                if std::fs::write(&staged, "unavailable").is_ok() {
                    let _ = std::fs::rename(&staged, &mode);
                }
                let _ = registry.look(hoh_agents::compat::Agent::Codex);
            }
        })
    };
    let host = world
        .start_host_with(world.host_config_with(registry.clone(), Some(hook))?)
        .await?;
    let outcome = async {
        world.seed_spec()?;
        let run = world::start_run(&host, "ro-c-lost", "完成fixture目标").await?;
        world
            .until(
                &format!("SELECT session_id IS NOT NULL FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"),
                "1",
                WAIT,
            )
            .await?;
        let session = world.query(&format!(
            "SELECT session_id FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"
        ))?;
        let first = format!("{run}/trigger/");
        world
            .until(
                &format!(
                    "SELECT a.state || ' ' || a.failure_reason FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                     WHERE t.run_id = '{run}' AND t.role = 'commander' ORDER BY a.rowid LIMIT 1"
                ),
                "FAILED AGENT_UNAVAILABLE",
                WAIT,
            )
            .await
            .map_err(|e| {
                format!(
                    "{e}; attempts: {:?}; events: {:?}",
                    world.rows("SELECT attempt_id, state, failure_reason FROM attempts"),
                    world.rows(&format!(
                        "SELECT seq, kind, substr(payload_json, 1, 400) AS payload FROM events \
                         WHERE run_id = '{run}' ORDER BY seq"
                    ))
                )
            })?;
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
        let (_, evidence) = commander_blocked(&world, &run, "codex").await?;
        assert_eq!(evidence["probe"]["host_check"]["check"], json!("unavailable"), "{evidence}");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind = 'team.binding_excluded'")?,
            "0"
        );
        let version = world.query(&format!(
            "SELECT s.agent_version FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             JOIN sessions s ON s.session_id = a.session_id \
             WHERE t.run_id = '{run}' AND t.role = 'commander' ORDER BY a.rowid LIMIT 1"
        ))?;
        world.sandbox("confined")?;
        world
            .until(
                &format!(
                    "SELECT count(*) > 0 FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                     WHERE t.run_id = '{run}' AND t.role = 'commander' AND a.state = 'SETTLED'"
                ),
                "1",
                WAIT,
            )
            .await?;
        assert_eq!(
            world.query(&format!(
                "SELECT group_concat(DISTINCT s.agent_version) FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 JOIN sessions s ON s.session_id = a.session_id WHERE t.run_id = '{run}' AND t.role = 'commander'"
            ))?,
            version
        );
        assert!(commander_attempts(&world, &run)?.iter().all(|a| a.starts_with(&first)));
        // The attempt dispatched again keeps the check its prepare saw: confined.
        let settled = world.query(&format!(
            "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.run_id = '{run}' AND t.role = 'commander' AND a.state = 'SETTLED' ORDER BY a.rowid LIMIT 1"
        ))?;
        let record = read_only_record(&world, &settled)?;
        assert_eq!(record["host_check"]["check"], json!("confined"), "{settled}: {record}");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind = 'team.binding_excluded'")?,
            "0"
        );
        world
            .until(
                &format!("SELECT state FROM attempts WHERE attempt_id = '{run}-develop/attempt-1'"),
                "SETTLED",
                WAIT,
            )
            .await?;
        assert_eq!(
            world.query(&format!(
                "SELECT session_id FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"
            ))?,
            session
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

/// AC-021-06 (d): codex on exec is never the Commander; the project's team has codex the Commander and the
/// Developer. With `variant` (exec only from the start: the read_only case FAILS, the suite PASSES), run R's commander
/// binding is BLOCKED with `codex:CAPABILITY_UNSUPPORTED:read_only_workspace` and codex's Developer turn SETTLES on
/// exec in run S, whose Commander is claude. Without one, codex is degraded in run R: its App Server answers the
/// Developer's initialize "method not found", the Developer's task SETTLES on exec, and from then on the commander
/// binding is BLOCKED for the read-only profile (its evidence says the App Server degraded) and no Commander turn
/// starts, on exec or at all.
async fn on_exec(variant: Option<world::Variant>) -> R {
    let world = World::new()?;
    if let Some(variant) = variant {
        world.variant(variant)?;
    }
    world.configure(&team_of("codex"), "")?;
    world.developer(&world::Developer {
        commit: false,
        ..world::Developer::default()
    })?;
    if variant.is_none() {
        world.degrade(&["/D/attempt-1"])?;
    }
    world.commit_project(&world::fixture_spec())?;
    world.register()?;
    let read_only = if variant.is_some() { "FAIL" } else { "PASS" };
    assert_eq!(
        manifest_statuses(&world, "codex")?,
        (json!("PASS"), json!(read_only))
    );
    let host = world.start_host().await?;
    let outcome = async {
        world.seed_spec()?;
        seeded_run(&world, "R", "D", &team_of("codex"))?;
        if variant.is_some() {
            seeded_run(&world, "S", "E", &team_with("claude", "codex"))?;
            no_commander_in_r(&world, "codex").await?;
            let (_, session, ran_on) = developer_settled(&world, "E").await?;
            assert_eq!(
                (session, ran_on.as_str()),
                (developer_session(&world, "S")?, "codex")
            );
        } else {
            let (_, session, _) = developer_settled(&world, "D").await?;
            assert_eq!(session, developer_session(&world, "R")?);
            let (_, evidence) = commander_blocked(&world, "R", "codex").await?;
            assert_eq!(
                evidence["probe"]["exec_degraded"],
                json!(true),
                "{evidence}"
            );
            need_decision(&world, "R", "fp-exec")?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            assert!(commander_attempts(&world, "R")?.is_empty());
        }
        let turns = world.log("turns.jsonl")?;
        assert!(!turns.iter().any(|t| t["role"] == "commander"), "{turns:?}");
        assert!(
            turns
                .iter()
                .any(|t| t["role"] == "developer" && t["agent"] == "codex-exec"),
            "{turns:?}"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_d_codex_on_exec_only_is_no_commander_and_still_a_developer() -> R {
    on_exec(Some(world::Variant::CodexExecOnly)).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_021_06_d_codex_degraded_to_exec_in_the_run_is_no_commander_after() -> R {
    on_exec(None).await
}
