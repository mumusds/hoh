//! REQ-028 (project discovery) through the product host; no live agent runs and no project command runs unconfirmed.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::Host;
use hoh_core::specindex::{self, Index, Source};
use serde_json::{Value, json};
use std::path::Path;
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
    // Its revision's CURRENT PASS independent review, which readiness needs before READY (REQ-031 behaviour 3,
    // docs/adr/ADR-035.md decision 4): no reviewer runs here.
    world.review_spec(&world.query(&format!("SELECT spec_id FROM runs WHERE run_id='{run}'"))?)?;
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
    // The detected tests are not run unconfirmed: the user is asked (REQ-028 behaviour 3); here they decline.
    answer_tests(&world, &host, &run, "skip").await?;
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

/// The Commander's create_tasks of `tasks` (task id, requirement ids, paths) on run `run`.
fn plan(tasks: &[(&str, &[&str], &[&str])]) -> Value {
    let tasks: Vec<Value> = tasks
        .iter()
        .map(|(id, requirements, paths)| {
            json!({"task_id": id, "title": id, "role": "developer", "writer": true, "depends_on": [],
                "requirement_ids": requirements, "paths": paths})
        })
        .collect();
    json!({"kind": "create_tasks", "plan": {"tasks": tasks}})
}

#[tokio::test]
async fn test_028_03_empty_req_rejected_and_dirty_blocks() -> R {
    let world = World::new()?;
    let index = brownfield(&world)?;
    let (host, run) = approved(&world, &index).await?;
    // F: the user's uncommitted change.
    let f = world.project().join("src/lib.rs");
    std::fs::write(&f, b"pub fn answer() -> u32 { 41 }\n")?;
    let digest = || -> R<String> { Ok(world::sha256_hex(&std::fs::read(&f)?)) };
    let original = digest()?;
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
    answer_tests(&world, &host, &run, "skip").await?;
    let specified = decide(
        &world,
        &host,
        &run,
        "specify",
        world::transition("SPECIFYING"),
    )
    .await?;
    assert!(
        matches!(specified, hoh_core::persistence::Decided::Applied { .. }),
        "{specified:?}"
    );
    // P1: T (REQ-A, its scope holding F) and U (no requirement: a refactoring of an existing module): refused whole.
    let before = (
        world.rows("SELECT * FROM tasks ORDER BY task_id")?,
        world.rows("SELECT * FROM task_requirements ORDER BY task_id")?,
    );
    let p1 = decide(
        &world,
        &host,
        &run,
        "p1",
        plan(&[("T", &["REQ-A"], &["src"]), ("U", &[], &["src/old"])]),
    )
    .await;
    let refusal = format!("{p1:?}");
    assert!(p1.is_err(), "P1 must be refused: {refusal}");
    assert!(
        refusal.contains("task U") && refusal.contains("requirement"),
        "the refusal names U's missing requirement: {refusal}"
    );
    assert_eq!(
        (
            world.rows("SELECT * FROM tasks ORDER BY task_id")?,
            world.rows("SELECT * FROM task_requirements ORDER BY task_id")?,
        ),
        before,
        "nothing of P1 imported"
    );
    // P2: only T, accepted and imported.
    let p2 = decide(
        &world,
        &host,
        &run,
        "p2",
        plan(&[("T", &["REQ-A"], &["src"])]),
    )
    .await?;
    assert!(
        matches!(p2, hoh_core::persistence::Decided::Applied { .. }),
        "{p2:?}"
    );
    assert_eq!(
        world.query(&format!(
            "SELECT group_concat(task_id) FROM tasks WHERE run_id='{run}'"
        ))?,
        "T"
    );
    for (id, to) in [("ready", "READY"), ("running", "RUNNING")] {
        let moved = decide(&world, &host, &run, id, world::transition(to)).await?;
        assert!(
            matches!(moved, hoh_core::persistence::Decided::Applied { .. }),
            "{moved:?}"
        );
    }
    // The scheduling round: T's scope holds F, so it is held for the user, not dispatched.
    let outcome = host
        .store()
        .schedule_round(&run, hoh_core::decision::Context::default())
        .await?;
    assert_eq!(outcome.dispatched, None, "{outcome:?}");
    assert_eq!(
        world.query(
            "SELECT state || '/' || coalesce(blocked_reason, '') FROM tasks WHERE task_id='T'"
        )?,
        "BLOCKED/NEED_DECISION"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM attempts WHERE task_id='T'")?,
        "0"
    );
    let need: Value = serde_json::from_str(&world.query(
        "SELECT payload_json FROM events WHERE kind='need_decision' AND json_extract(payload_json,'$.task_id')='T'",
    )?)?;
    assert_eq!(need["reason"], "dirty_workspace", "{need}");
    assert_eq!(need["paths"], json!(["src/lib.rs"]), "{need}");
    // The question the user sees lists F.
    let (_, body) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let questions = body["projection"]["questions"].to_string();
    assert!(
        questions.contains("dirty_workspace") && questions.contains("src/lib.rs"),
        "{questions}"
    );
    // Another round dispatches nothing of T either.
    let again = host
        .store()
        .schedule_round(&run, hoh_core::decision::Context::default())
        .await?;
    assert_eq!(again.dispatched, None, "{again:?}");
    // F was never touched: its digest, no stash, its change still uncommitted.
    assert_eq!(digest()?, original);
    assert_eq!(world.git(&["stash", "list"])?, "");
    assert_eq!(
        world.git(&["status", "--porcelain", "--", "src/lib.rs"])?,
        " M src/lib.rs\n"
    );
    host.shutdown().await;
    Ok(())
}

/// Run `run`'s open question `runtime:baseline_tests` (REQ-028 behaviour 3), once asked: its id.
async fn tests_question(world: &World, host: &Host, run: &str) -> R<String> {
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='question.asked' \
                 AND json_extract(payload_json,'$.reason')='baseline_tests'"
            ),
            "1",
            Duration::from_secs(20),
        )
        .await?;
    let (_, body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let question = body["projection"]["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|q| q["reason"] == "baseline_tests")
        .ok_or_else(|| format!("no baseline_tests question: {body}"))?;
    assert_eq!(body["state"], "BLOCKED", "{body}");
    Ok(question["question_id"]
        .as_str()
        .ok_or("no question id")?
        .to_owned())
}

/// The user's answer `choice` to run `run`'s baseline tests question.
async fn answer_tests(world: &World, host: &Host, run: &str, choice: &str) -> R {
    let question = tests_question(world, host, run).await?;
    let body = command(
        host,
        run,
        &format!("answer-{choice}"),
        "AnswerQuestion",
        json!({"question_id": question, "answer": choice, "choice_id": choice}),
    )
    .await?;
    let (status, response) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{response}");
    Ok(())
}

#[tokio::test]
async fn test_028_02_confirmed_test_run_excludes_baseline_failures() -> R {
    let world = World::new()?;
    let index = brownfield(&world)?;
    // One test that already fails, and a passing one that counts each run of the project's tests outside it.
    let runs = world.root().join("test-runs.log");
    std::fs::write(
        world.project().join("tests/answer.rs"),
        format!(
            "#[test]\nfn answer() {{\n    use std::io::Write as _;\n    let mut f = std::fs::OpenOptions::new().create(true).append(true).open({:?}).unwrap();\n    writeln!(f, \"ran\").unwrap();\n    assert_eq!(fixture::answer(), 42);\n}}\n\n#[test]\nfn fails() {{\n    panic!(\"known before HoH\");\n}}\n",
            runs.display().to_string()
        ),
    )?;
    std::fs::write(
        world.project().join(".gitignore"),
        b".hoh/\n/target/\nCargo.lock\n",
    )?;
    world.git(&["add", "."])?;
    world.git(&["commit", "-qm", "a known failure"])?;
    let (host, run) = approved(&world, &index).await?;
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
    // Asked, not run: no project command before the user confirms.
    let question = tests_question(&world, &host, &run).await?;
    assert_eq!(question, format!("{run}/baseline_tests/question"));
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !runs.exists(),
        "a project command ran before the user confirmed"
    );
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
        ))?,
        "0"
    );
    answer_tests(&world, &host, &run, "run").await?;
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
            ),
            "1",
            Duration::from_secs(240),
        )
        .await?;
    let tested: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
    ))?)?;
    let commands = tested["commands"].as_array().ok_or("no commands")?;
    assert_eq!(commands.len(), 1, "{tested}");
    assert_eq!(commands[0]["command"], "cargo test", "{tested}");
    assert_eq!(commands[0]["status"], "fail", "{tested}");
    assert_eq!(commands[0]["failed_tests"], json!(["fails"]), "{tested}");
    assert_eq!(commands[0]["incomplete"], json!(false), "{tested}");
    // Its process was recorded before it ran and its end once its group was gone (DC-T07 (1), (2)).
    let log = commands[0]["log"].as_str().ok_or("no log")?;
    assert!(world.project().join(log).is_file(), "{tested}");
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_process' \
             AND json_extract(payload_json,'$.log')='{log}'"
        ))?,
        "1"
    );
    assert!(
        host.store()
            .reader()?
            .unsettled_baseline_processes(&world.query("SELECT project_id FROM projects")?)?
            .is_empty()
    );
    // Run once.
    assert_eq!(std::fs::read_to_string(&runs)?, "ran\n");
    // A later candidate on which the same test still fails, and one more: only the new one is HoH's regression.
    let record_dir = world.project().join(".hoh/evidence/fixture/unit/R1");
    std::fs::create_dir_all(record_dir.join("artifacts"))?;
    let lines = "{\"schema_version\":\"1.0\",\"test_id\":\"TEST-A-01\",\"name\":\"fails\",\"status\":\"FAIL\"}\n\
                 {\"schema_version\":\"1.0\",\"test_id\":\"TEST-A-01\",\"name\":\"answer\",\"status\":\"PASS\"}\n\
                 {\"schema_version\":\"1.0\",\"test_id\":\"TEST-A-01\",\"name\":\"newly_broken\",\"status\":\"FAIL\"}\n";
    std::fs::write(record_dir.join("artifacts/test-results.jsonl"), lines)?;
    let results = ".hoh/evidence/fixture/unit/R1/artifacts/test-results.jsonl";
    let record = json!({"artifacts": [{"name": "test-results.jsonl", "path": results,
        "sha256": world::sha256_hex(lines.as_bytes())}]});
    std::fs::write(record_dir.join("execution.json"), record.to_string())?;
    let candidate = format!("CAND-{}", "c".repeat(64));
    world.exec(&format!(
        "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
           SELECT 'T',run_id,spec_id,'IMPLEMENTED','developer',1,1 FROM runs WHERE run_id='{run}';
         INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
           VALUES('observed','T','SETTLED',1,1,0);
         INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,created_at)
           VALUES('{candidate}','{run}','{h}','{h}','{d}','{d}','{d}','fixture','2026-11-22T00:00:00Z');
         INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime)
           VALUES('record','{d}','.hoh/evidence/fixture/unit/R1/execution.json',0,'application/json');
         INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at)
           VALUES('e1','{candidate}','observed','record','unit','check','FAIL','mock','2026-11-22T00:00:00Z');",
        h = "1".repeat(40),
        d = "0".repeat(64)
    ))?;
    let (_, body) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
    assert_eq!(
        body["projection"]["regression"],
        json!({"candidate_id": candidate, "baseline_complete": true, "baseline_failed": ["fails"],
            "candidate_failed": ["fails", "newly_broken"], "introduced": ["newly_broken"],
            "not_in_baseline": ["newly_broken"]}),
        "{body}"
    );
    host.shutdown().await;
    Ok(())
}

/// A project whose only detected test command is `make test`: its recipe appends its shell's pid to `marker`, says one
/// line, waits until `release` exists, then reports one failing test. Committed.
fn make_project(world: &World, marker: &Path, release: &Path) -> R<Index> {
    let index = brownfield(world)?;
    std::fs::remove_file(world.project().join("Cargo.toml"))?;
    std::fs::write(
        world.project().join("Makefile"),
        format!(
            "test:\n\t@sh -c 'echo $$$$ >> {}; echo partial; while [ ! -f {} ]; do sleep 0.1; done; \
             echo \"test known ... FAILED\"; exit 1'\n",
            marker.display(),
            release.display()
        ),
    )?;
    world.git(&["add", "-A"])?;
    world.git(&["commit", "-qm", "make test"])?;
    Ok(index)
}

/// The pids `marker` lists, one per run of the test command.
fn started(marker: &Path) -> Vec<u32> {
    std::fs::read_to_string(marker)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Wait (at most 60 s) until `marker` lists `n` runs: the last one's process group.
async fn nth_started(marker: &Path, n: usize) -> R<u32> {
    let end = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let pids = started(marker);
        if pids.len() >= n {
            let pid = pids[n - 1];
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
            let (_, rest) = stat.rsplit_once(") ").ok_or("no stat")?;
            return Ok(rest.split_whitespace().nth(2).ok_or("no pgrp")?.parse()?);
        }
        if std::time::Instant::now() > end {
            return Err(format!("the test command did not start {n} times: {pids:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// SIGKILLs a process group when dropped, however the test ends (cleanup only).
struct GroupGuard(u32);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pgid) = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
        }
    }
}

/// Run `run`'s `kind` control command (PauseRun, ResumeRun, CancelRun), accepted.
async fn control(host: &Host, run: &str, kind: &str) -> R {
    let body = command(host, run, kind, kind, json!({"reason": kind})).await?;
    let (status, response) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{kind}: {response}");
    Ok(())
}

/// Wait (at most `limit`) until run `run` is in `state`; whether group `pgid` was empty when it first was.
async fn group_empty_at(
    world: &World,
    run: &str,
    state: &str,
    pgid: u32,
    limit: Duration,
) -> R<bool> {
    let end = std::time::Instant::now() + limit;
    loop {
        let now = world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?;
        if now == state {
            return Ok(hoh_agents::identity::group_empty(pgid));
        }
        if std::time::Instant::now() > end {
            return Err(format!("run {run} is {now}, not {state}").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The user's confirmed baseline test run of a fresh `make test` project, its run in DISCOVERING: the host, the run.
async fn confirmed(world: &World, marker: &Path, release: &Path) -> R<(Host, String)> {
    let index = make_project(world, marker, release)?;
    let (host, run) = approved(world, &index).await?;
    let entered = decide(
        world,
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
    answer_tests(world, &host, &run, "run").await?;
    Ok((host, run))
}

fn count(world: &World, run: &str, kind: &str) -> R<String> {
    world.query(&format!(
        "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='{kind}'"
    ))
}

#[tokio::test]
async fn test_028_02_a_cancel_ends_the_baseline_test_command_before_the_run_is_cancelled() -> R {
    // M3N-003 (docs/adr/ADR-039.md decision 8, DC-T07 (2)): CancelRun while the confirmed `make test` runs is not
    // CANCELLED until the command's process group is gone; nothing of the interrupted run is recorded.
    let world = World::new()?;
    let (marker, release) = (world.root().join("started"), world.root().join("release"));
    let (host, run) = confirmed(&world, &marker, &release).await?;
    let pgid = nth_started(&marker, 1).await?;
    let _guard = GroupGuard(pgid);
    control(&host, &run, "CancelRun").await?;
    assert!(
        group_empty_at(&world, &run, "CANCELLED", pgid, Duration::from_secs(60)).await?,
        "the run is CANCELLED while its baseline test command's group {pgid} lives"
    );
    assert_eq!(count(&world, &run, "run.baseline_tests")?, "0");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_pause_ends_the_baseline_test_command_and_the_resumed_run_reruns_it() -> R {
    // M3N-051 (DC-T07 (2), (3), (4)): PauseRun while the command runs is not PAUSED until its group is gone; the paused
    // run starts nothing; resumed, the command runs again with a new log, the interrupted run's log kept.
    let world = World::new()?;
    let (marker, release) = (world.root().join("started"), world.root().join("release"));
    let (host, run) = confirmed(&world, &marker, &release).await?;
    let pgid = nth_started(&marker, 1).await?;
    let _guard = GroupGuard(pgid);
    control(&host, &run, "PauseRun").await?;
    assert!(
        group_empty_at(&world, &run, "PAUSED", pgid, Duration::from_secs(60)).await?,
        "the run is PAUSED while its baseline test command's group {pgid} lives"
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(started(&marker).len(), 1, "a paused run started its tests");
    assert_eq!(count(&world, &run, "run.baseline_tests")?, "0");
    let first: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_process' ORDER BY seq LIMIT 1"
    ))?)?;
    let first_log = world.project().join(first["log"].as_str().ok_or("no log")?);
    std::fs::write(&release, b"")?;
    control(&host, &run, "ResumeRun").await?;
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
            ),
            "1",
            Duration::from_secs(60),
        )
        .await?;
    assert_eq!(started(&marker).len(), 2);
    let tested: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
    ))?)?;
    assert_eq!(
        tested["commands"][0]["failed_tests"],
        json!(["known"]),
        "{tested}"
    );
    assert_eq!(
        tested["commands"][0]["incomplete"],
        json!(false),
        "{tested}"
    );
    let second = world
        .project()
        .join(tested["commands"][0]["log"].as_str().ok_or("no log")?);
    assert_ne!(second, first_log);
    assert_eq!(
        std::fs::read_to_string(&first_log)?,
        "partial\n",
        "the interrupted run's log is kept"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_new_host_kills_the_group_a_killed_host_left_and_keeps_its_log() -> R {
    // M3N-050 (DC-T07 (1), (3)): the product records its baseline test process before the command can run. A host
    // SIGKILLed while it ran left that group running, not its child: here the state such a host leaves, a live group
    // in a process group of its own, its identity recorded as the product records it, its partial log. The next host
    // kills and reaps it before it reruns the command, keeps that log and writes the rerun's to a new one.
    let world = World::new()?;
    let (marker, release) = (world.root().join("started"), world.root().join("release"));
    let (host, run) = confirmed(&world, &marker, &release).await?;
    let pgid = nth_started(&marker, 1).await?;
    let _first = GroupGuard(pgid);
    let recorded: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_process'"
    ))?)?;
    assert_eq!(recorded["pid"], json!(pgid), "{recorded}");
    assert!(
        recorded["boot_id"].is_string() && recorded["process_start_id"].is_string(),
        "{recorded}"
    );
    host.shutdown().await;
    // What a host killed at this point leaves: the group alive, recorded, not ended.
    let log = format!(".hoh/baseline/{run}/0-crashed.log");
    let out = std::fs::File::create(world.project().join(&log))?;
    let mut left = {
        use std::os::unix::process::CommandExt as _;
        std::process::Command::new("sh")
            .args([
                "-c",
                "echo $$ >> \"$1\"; echo partial; while :; do sleep 0.1; done",
                "left",
            ])
            .arg(&marker)
            .current_dir(world.project())
            .stdin(std::process::Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .process_group(0)
            .spawn()?
    };
    let left_pid = left.id();
    let _left = GroupGuard(left_pid);
    nth_started(&marker, 2).await?;
    let identity = hoh_agents::identity::of(left_pid)?.ok_or("no identity")?;
    world.exec(&format!(
        "INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at)
         VALUES(:p,'{run}/baseline/process/0/crashed','{run}','run.baseline_process',
           json_object('run_id','{run}','index',0,'nonce','crashed','command','make test','pid',{pid},
             'boot_id','{boot}','process_start_id','{start}','log','{log}'),'2026-11-22T00:00:00Z');",
        pid = identity.pid,
        boot = identity.boot_id,
        start = identity.start_id,
    ))?;
    let reaped = std::thread::spawn(move || left.wait());
    let host = world.start_plain_host(None).await?;
    // Killed and reaped before the rerun starts; its log kept as it was.
    let status = tokio::task::spawn_blocking(move || reaped.join())
        .await?
        .map_err(|_| "the reaper panicked")??;
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "{status:?}"
    );
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_process_ended' \
                 AND json_extract(payload_json,'$.process')='{run}/baseline/process/0/crashed'"
            ),
            "1",
            Duration::from_secs(20),
        )
        .await?;
    let settled: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_process_ended' \
         AND json_extract(payload_json,'$.process')='{run}/baseline/process/0/crashed'"
    ))?)?;
    assert_eq!(settled["settled_by"], "reconcile", "{settled}");
    assert_eq!(settled["partial"], true, "{settled}");
    assert_eq!(
        std::fs::read_to_string(world.project().join(&log))?,
        "partial\n"
    );
    std::fs::write(&release, b"")?;
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
            ),
            "1",
            Duration::from_secs(60),
        )
        .await?;
    assert_eq!(started(&marker).len(), 3, "the rerun ran once");
    let order: Vec<Value> = world.rows(&format!(
        "SELECT kind, json_extract(payload_json,'$.process') AS process, json_extract(payload_json,'$.log') AS log
         FROM events WHERE run_id='{run}' AND kind LIKE 'run.baseline_process%' ORDER BY seq"
    ))?;
    let ended = order
        .iter()
        .position(|e| e["process"] == json!(format!("{run}/baseline/process/0/crashed")))
        .ok_or("not ended")?;
    let rerun = order
        .iter()
        .rposition(|e| e["kind"] == "run.baseline_process")
        .ok_or("no rerun")?;
    assert!(
        ended < rerun,
        "the group was settled before the rerun started: {order:?}"
    );
    assert_ne!(order[rerun]["log"], json!(log));
    assert_eq!(
        std::fs::read_to_string(world.project().join(&log))?,
        "partial\n"
    );
    host.shutdown().await;
    Ok(())
}

/// A candidate `CAND-<c>` of run `run` whose only evidence's `test-results.jsonl` fails `failed` (a persisted fixture of
/// a verified candidate; no check runs): its id.
fn failing_candidate(world: &World, run: &str, c: char, failed: &[&str]) -> R<String> {
    let candidate = format!("CAND-{}", c.to_string().repeat(64));
    let dir = format!(".hoh/evidence/fixture/unit/{c}");
    let record_dir = world.project().join(&dir);
    std::fs::create_dir_all(record_dir.join("artifacts"))?;
    let lines: String = failed
        .iter()
        .map(|n| {
            format!(
                "{{\"schema_version\":\"1.0\",\"test_id\":\"TEST-A-01\",\"name\":\"{n}\",\"status\":\"FAIL\"}}\n"
            )
        })
        .collect();
    std::fs::write(record_dir.join("artifacts/test-results.jsonl"), &lines)?;
    let record = json!({"artifacts": [{"name": "test-results.jsonl",
        "path": format!("{dir}/artifacts/test-results.jsonl"), "sha256": world::sha256_hex(lines.as_bytes())}]});
    std::fs::write(record_dir.join("execution.json"), record.to_string())?;
    world.exec(&format!(
        "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
           SELECT 'T{c}',run_id,spec_id,'IMPLEMENTED','developer',1,1 FROM runs WHERE run_id='{run}';
         INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
           VALUES('observed-{c}','T{c}','SETTLED',1,1,0);
         INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,created_at)
           VALUES('{candidate}','{run}','{h}','{h}','{d}','{d}','{d}','fixture','2026-11-22T00:00:00Z');
         INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime)
           VALUES('record-{c}','{d}','{dir}/execution.json',0,'application/json');
         INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at)
           VALUES('e-{c}','{candidate}','observed-{c}','record-{c}','unit','check','FAIL','mock','2026-11-22T00:00:00Z');",
        h = "1".repeat(40),
        d = "0".repeat(64)
    ))?;
    Ok(candidate)
}

/// Run `run`'s regression projection through the API (`GET /v1/runs/{run}`).
async fn regression(host: &Host, run: &str) -> R<Value> {
    let (status, body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    assert_eq!(status, 200, "{body}");
    Ok(body["projection"]["regression"].clone())
}

#[tokio::test]
async fn test_028_02_no_complete_baseline_until_its_tests_are_recorded() -> R {
    // M3N-102, M3N-150 (docs/adr/ADR-039.md, round 2): after the user's `run`, with a candidate frozen before the
    // baseline's tests are recorded, the regression says the baseline is incomplete and introduces nothing while the
    // command runs and once a pause interrupted it; resumed, the recorded tests complete it.
    let world = World::new()?;
    let (marker, release) = (world.root().join("started"), world.root().join("release"));
    let (host, run) = confirmed(&world, &marker, &release).await?;
    let pgid = nth_started(&marker, 1).await?;
    let _guard = GroupGuard(pgid);
    let candidate = failing_candidate(&world, &run, 'c', &["known", "newly_broken"])?;
    let incomplete = json!({"candidate_id": candidate, "baseline_complete": false, "baseline_failed": [],
        "candidate_failed": ["known", "newly_broken"], "introduced": null,
        "not_in_baseline": ["known", "newly_broken"]});
    assert_eq!(regression(&host, &run).await?, incomplete, "while it runs");
    control(&host, &run, "PauseRun").await?;
    group_empty_at(&world, &run, "PAUSED", pgid, Duration::from_secs(60)).await?;
    assert_eq!(regression(&host, &run).await?, incomplete, "paused");
    std::fs::write(&release, b"")?;
    control(&host, &run, "ResumeRun").await?;
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
            ),
            "1",
            Duration::from_secs(60),
        )
        .await?;
    assert_eq!(
        regression(&host, &run).await?,
        json!({"candidate_id": candidate, "baseline_complete": true, "baseline_failed": ["known"],
            "candidate_failed": ["known", "newly_broken"], "introduced": ["newly_broken"],
            "not_in_baseline": ["newly_broken"]}),
        "resumed and recorded"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_028_02_a_baseline_test_command_killed_by_a_signal_is_incomplete() -> R {
    // M3N-101 (docs/adr/ADR-039.md, round 2): the confirmed `make test` reports one failure, then a signal kills make;
    // its shell reports it killed. Its failed tests are incomplete and the regression introduces nothing.
    let world = World::new()?;
    let index = brownfield(&world)?;
    std::fs::remove_file(world.project().join("Cargo.toml"))?;
    std::fs::write(
        world.project().join("Makefile"),
        "test:\n\t@echo \"test known ... FAILED\"; kill -KILL $$PPID\n",
    )?;
    world.git(&["add", "-A"])?;
    world.git(&["commit", "-qm", "make test killed"])?;
    let (host, run) = approved(&world, &index).await?;
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
    answer_tests(&world, &host, &run, "run").await?;
    world
        .until(
            &format!(
                "SELECT count(*) FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
            ),
            "1",
            Duration::from_secs(60),
        )
        .await?;
    let tested: Value = serde_json::from_str(&world.query(&format!(
        "SELECT payload_json FROM events WHERE run_id='{run}' AND kind='run.baseline_tests'"
    ))?)?;
    let ran = &tested["commands"][0];
    assert_eq!(
        (
            &ran["command"],
            &ran["incomplete"],
            &ran["timed_out"],
            &ran["failed_tests"]
        ),
        (
            &json!("make test"),
            &json!(true),
            &json!(false),
            &json!(["known"])
        ),
        "{tested}"
    );
    let candidate = failing_candidate(&world, &run, 'k', &["known", "other"])?;
    assert_eq!(
        regression(&host, &run).await?,
        json!({"candidate_id": candidate, "baseline_complete": false, "baseline_failed": ["known"],
            "candidate_failed": ["known", "other"], "introduced": null, "not_in_baseline": ["other"]})
    );
    host.shutdown().await;
    Ok(())
}
