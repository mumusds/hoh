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

/// The scope question the scripted Commander of TEST-030-01 asks, and its choice the user takes.
const SCOPE: &str = "记账工具的范围：个人记账还是小团队共享账本？";

/// The world's specification Markdown with `decisions` under `# 决策` (each entry two lines), every other heading
/// written or 无, and REQ-901 as `world::fixture_spec` has it.
fn markdown(decisions: &str) -> String {
    format!(
        "# 用户\n记账的个人用户\n# 范围\n个人记账\n# 非目标\n无\n# 决策\n{decisions}# 开放问题\n无\n# 需求\n\
         ## REQ-901 完成 fixture 目标\n### AC-901-01\ngiven: fixture\nwhen: checked\nthen: fixture 目标已完成\n\
         check: check-ac-901-01\n"
    )
}

/// A control command of run `run` at its current revision, through the host: its status and body.
async fn submit(
    host: &hoh_app::host::Host,
    run: &str,
    key: &str,
    kind: &str,
    payload: Value,
) -> R<(u16, Value)> {
    let (_, got) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let body = json!({"schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": got["revision"], "run_id": run, "kind": kind, "payload": payload});
    world::exchange(host, "POST", "/v1/commands", Some(&body)).await
}

/// The project's revision 1 approved through the host on a run of its own (`hoh` blocks a run started before any
/// approved specification, then adopts it): the committed `docs/spec` of `commit_project`, digest `sha256`.
async fn approve_first(host: &hoh_app::host::Host, sha256: &str) -> R<String> {
    let first = world::start_run(host, "first", "批准修订 1").await?;
    let (status, body) = submit(
        host,
        &first,
        "approve-1",
        "ApproveSpec",
        json!({"spec_sha256": sha256, "decision": "APPROVE_ONLY"}),
    )
    .await?;
    if status != 202 {
        return Err(format!("ApproveSpec 1: {status} {body}").into());
    }
    Ok(first)
}

/// `text` written as the project's specification Markdown: its index's digest.
fn draft(world: &World, text: &str) -> R<String> {
    let source = hoh_core::specindex::Source {
        path: "docs/spec/fixture.md".to_owned(),
        bytes: text.as_bytes().to_vec(),
    };
    let index = hoh_core::specindex::generate_index(std::slice::from_ref(&source))?;
    std::fs::write(world.project().join(&source.path), &source.bytes)?;
    Ok(index.sha256())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_030_01_answered_scope_decision_cites_question() -> R {
    // AC-030-01: an interactive run whose scripted Commander asks a scope question with options; the user answers one
    // option; the drafted specification (the test drafts it: drafting Tasks are M3-T04's) cites the persisted
    // question id and goes through ProposeSpecChange and ApproveSpec.
    let world = World::new()?;
    world.configure(&[], "")?;
    let first_sha = world.commit_project(&world::fixture_spec())?;
    let asked = std::sync::atomic::AtomicBool::new(false);
    let team = world::Team::new(move |turn| {
        if turn.state == "CREATED" && !asked.swap(true, std::sync::atomic::Ordering::SeqCst) {
            vec![
                json!({"kind": "request_user_decision", "question_id": "q-scope", "question": SCOPE,
                "choices": [{"choice_id": "personal", "label": "个人记账"},
                            {"choice_id": "team", "label": "小团队共享账本"}],
                "requires_user": true}),
            ]
        } else {
            Vec::new()
        }
    });
    let host = world.team_host(team.clone(), None).await?;
    let outcome = async {
        approve_first(&host, &first_sha).await?;
        let start = json!({"schema_version": "1.0", "command_id": "c-start", "idempotency_key": "start",
            "expected_revision": 0, "kind": "StartRun", "run_id": null,
            "payload": {"goal": "做一个记账工具", "mode": "AUTO", "interactive": true}});
        let (status, started) = world::exchange(&host, "POST", "/v1/commands", Some(&start)).await?;
        assert_eq!(status, 202, "{started}");
        let run = started["run_id"].as_str().ok_or("no run")?.to_owned();
        world.staff(&run)?;
        // The question persisted: its id is the Runtime's (the decision's), not the Commander's q-scope.
        world
            .until(
                &format!(
                    "SELECT count(*) FROM events WHERE run_id = '{run}' AND kind = 'question.asked'"
                ),
                "1",
                WAIT,
            )
            .await?;
        let question = world.query(&format!(
            "SELECT json_extract(payload_json, '$.question_id') FROM events WHERE run_id = '{run}' \
             AND kind = 'question.asked'"
        ))?;
        assert_ne!(question, "q-scope");
        let (status, answered) = submit(
            &host,
            &run,
            "answer",
            "AnswerQuestion",
            json!({"question_id": question, "answer": "个人记账", "choice_id": "personal"}),
        )
        .await?;
        assert_eq!(status, 202, "{answered}");
        // The drafted revision 2: its decision cites the answer's question_id.
        let text = markdown(&format!("- 范围限于个人记账\nsource: question_id={question}\n"));
        let sha = draft(&world, &text)?;
        let (status, proposed) = submit(
            &host,
            &run,
            "propose",
            "ProposeSpecChange",
            json!({"reason": "范围的答复", "spec_sha256": sha}),
        )
        .await?;
        assert_eq!(status, 202, "{proposed}");
        world.git(&["add", "docs/spec"])?;
        world.git(&["commit", "-qm", "specification revision 2"])?;
        let (status, approved) = submit(
            &host,
            &run,
            "approve-2",
            "ApproveSpec",
            json!({"spec_sha256": sha, "decision": "APPROVE_ONLY"}),
        )
        .await?;
        assert_eq!(status, 202, "{approved}");
        assert_eq!(
            world.query("SELECT revision || ' ' || sha256 FROM specs ORDER BY revision DESC LIMIT 1")?,
            format!("2 {sha}")
        );
        // Each of the five headings has content or 无 (the grammar guarantees it: asserted on the approved bytes).
        let approved_text = std::fs::read_to_string(world.project().join("docs/spec/fixture.md"))?;
        let lines: Vec<&str> = approved_text.lines().collect();
        for heading in ["# 用户", "# 范围", "# 非目标", "# 决策", "# 开放问题"] {
            let at = lines.iter().position(|l| *l == heading).ok_or(heading)?;
            let next = lines.get(at + 1).copied().unwrap_or_default();
            assert!(!next.is_empty() && !next.starts_with('#'), "{heading}: {next:?}");
        }
        // The DB holds the AnswerQuestion and its question.answered for that question.
        assert_eq!(
            world.query(&format!(
                "SELECT c.kind FROM events e JOIN commands c ON c.command_id = json_extract(e.payload_json, \
                 '$.command_id') WHERE e.kind = 'question.answered' \
                 AND json_extract(e.payload_json, '$.question_id') = '{question}'"
            ))?,
            "AnswerQuestion"
        );
        if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&dir).join("test-030-01.json"),
                serde_json::to_vec_pretty(&json!({"run": run, "question_id": question,
                    "proposed": proposed, "approved": approved, "markdown": text}))?,
            )?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_030_03_unsourced_user_decision_spec_invalid() -> R {
    // AC-030-03: the proposed revision states a decision as the user's whose question_id has no answer in the DB (an
    // assumption, in fact): 422 SPEC_INVALID listing the entry and its missing source; no CR, no new revision.
    let world = World::new()?;
    world.configure(&[], "")?;
    let first_sha = world.commit_project(&world::fixture_spec())?;
    let host = world.start_plain_host(None).await?;
    let outcome = async {
        let run = approve_first(&host, &first_sha).await?;
        let text = markdown("- 用户选择了个人记账\nsource: question_id=q-assumed\n");
        let sha = draft(&world, &text)?;
        let (status, refused) = submit(
            &host,
            &run,
            "propose",
            "ProposeSpecChange",
            json!({"reason": "范围", "spec_sha256": sha}),
        )
        .await?;
        assert_eq!(status, 422, "{refused}");
        assert_eq!(refused["code"], "SPEC_INVALID", "{refused}");
        assert_eq!(
            refused["details"],
            json!({"unsourced": [{"path": "docs/spec/fixture.md", "line": 8, "text": "用户选择了个人记账",
                "source": {"kind": "question_id", "id": "q-assumed"}, "missing": "question.answered"}]}),
            "{refused}"
        );
        assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
        assert_eq!(world.query("SELECT max(revision) FROM specs")?, "1");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind IN ('spec.proposed', 'spec.index_failed')")?,
            "0"
        );
        // Approving the same bytes is refused the same way (the approval checks the sources too).
        world.git(&["add", "docs/spec"])?;
        let index = hoh_core::specindex::generate_index(&[hoh_core::specindex::Source {
            path: "docs/spec/fixture.md".to_owned(),
            bytes: text.as_bytes().to_vec(),
        }])?;
        std::fs::write(world.project().join(hoh_core::specindex::PATH), index.bytes())?;
        world.git(&["add", "docs/spec"])?;
        world.git(&["commit", "-qm", "unsourced revision"])?;
        let (status, again) = submit(
            &host,
            &run,
            "approve-2",
            "ApproveSpec",
            json!({"spec_sha256": sha, "decision": "APPROVE_ONLY"}),
        )
        .await?;
        assert_eq!(status, 422, "{again}");
        assert_eq!(again["details"], refused["details"], "{again}");
        assert_eq!(world.query("SELECT max(revision) FROM specs")?, "1");
        if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&dir).join("test-030-03.json"),
                serde_json::to_vec_pretty(&json!({"refused": refused, "approval_refused": again}))?,
            )?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}
