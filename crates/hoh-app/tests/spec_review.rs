//! The independent specification review on the product path (REQ-031 behaviour 3, 4 and 7; docs/adr/ADR-035.md).
//! TEST-031-04 (AC-031-04): in-process hosts whose runner is the scripted Team, which plays the Commander and the
//! reviewer of each specification review (a fake reviewer: never reported as live); revisions are drafted by the test
//! (drafting Tasks are M3-T04's), committed, proposed and approved through the host.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;
mod pty;
mod world;

use hoh_app::host::Host;
use pty::R;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use world::{Verdict, World};

/// How long a test waits for the product to get where it looks.
const WAIT: Duration = Duration::from_secs(60);

/// The specification Markdown: requirement `id` with `criteria` criteria whose `then` is `then`.
fn markdown(id: &str, criteria: usize, then: &str) -> String {
    let mut text = format!(
        "# 用户\n记账用户\n# 范围\n记账\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## {id} 记一笔账\n"
    );
    let n = id.trim_start_matches("REQ-");
    for k in 1..=criteria {
        text.push_str(&format!(
            "### AC-{n}-{k:02}\ngiven: 空账本\nwhen: 记一笔\nthen: {then}\ncheck: check-{k}\n"
        ));
    }
    text
}

/// The project's `docs/spec/spec.md` written as `text` and committed with its generated index: its digest.
fn commit_spec(world: &World, text: &str) -> R<String> {
    let source = hoh_core::specindex::Source {
        path: "docs/spec/spec.md".to_owned(),
        bytes: text.as_bytes().to_vec(),
    };
    let index = hoh_core::specindex::generate_index(std::slice::from_ref(&source))?;
    std::fs::create_dir_all(world.project().join("docs/spec"))?;
    for entry in std::fs::read_dir(world.project().join("docs/spec"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "md") {
            std::fs::remove_file(path)?;
        }
    }
    std::fs::write(world.project().join(&source.path), &source.bytes)?;
    std::fs::write(
        world.project().join(hoh_core::specindex::PATH),
        index.bytes(),
    )?;
    world.git(&["add", "-A", "docs/spec"])?;
    world.git(&["commit", "-qm", "specification"])?;
    Ok(index.sha256())
}

/// A command of run `run` at its current revision: its status and body.
async fn submit(host: &Host, run: &str, key: &str, kind: &str, payload: Value) -> R<(u16, Value)> {
    let (_, got) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let body = json!({"schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": got["revision"], "run_id": run, "kind": kind, "payload": payload});
    world::exchange(host, "POST", "/v1/commands", Some(&body)).await
}

/// Revision `sha256` proposed (a CR on an approved base) and approved on run `run`.
async fn approve_change(host: &Host, run: &str, key: &str, sha256: &str) -> R {
    let (status, body) = submit(
        host,
        run,
        &format!("{key}-propose"),
        "ProposeSpecChange",
        json!({"reason": key, "spec_sha256": sha256}),
    )
    .await?;
    if status != 202 {
        return Err(format!("ProposeSpecChange {key}: {status} {body}").into());
    }
    let (status, body) = submit(
        host,
        run,
        &format!("{key}-approve"),
        "ApproveSpec",
        json!({"spec_sha256": sha256, "decision": "APPROVE_ONLY"}),
    )
    .await?;
    if status != 202 {
        return Err(format!("ApproveSpec {key}: {status} {body}").into());
    }
    Ok(())
}

/// Revision 1 (`sha256`) approved through the host on a run of its own, and reviewed (the world's PASS fixture: the
/// test is about later revisions); its spec_id.
async fn approve_first(world: &World, host: &Host, sha256: &str) -> R<String> {
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
    let spec = world.query("SELECT spec_id FROM specs WHERE revision = 1")?;
    world.review_spec(&spec)?;
    Ok(spec)
}

/// The payloads of `sql`'s `payload_json` column, parsed.
fn payloads(world: &World, sql: &str) -> R<Vec<Value>> {
    world
        .rows(sql)?
        .iter()
        .map(|r| {
            Ok(serde_json::from_str(
                r["payload_json"].as_str().ok_or("no payload")?,
            )?)
        })
        .collect()
}

/// The `spec_review.reported` payloads of revision `revision`, in order.
fn reported(world: &World, revision: i64) -> R<Vec<Value>> {
    payloads(
        world,
        &format!(
            "SELECT payload_json FROM events WHERE kind = 'spec_review.reported' \
             AND json_extract(payload_json, '$.spec_revision') = {revision} \
             AND json_extract(payload_json, '$.review_id') NOT LIKE 'world-review-%' ORDER BY seq"
        ),
    )
}

/// Wait (at most WAIT) until `done` holds.
async fn until(what: &str, mut done: impl FnMut() -> R<bool>) -> R {
    let end = Instant::now() + WAIT;
    while !done()? {
        if Instant::now() > end {
            return Err(format!("{what}: not within {WAIT:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// How the test replaces the native attempt record the host built for one of revision 3's reviews.
#[derive(Debug, Clone, Copy)]
enum Tamper {
    ReviewId,
    AttemptId,
    Stdout,
    SpecSha,
    GitCommit,
    Missing,
}

/// The host's hooks: each native record of a revision-3 review replaced by the next tamper, until there is none.
#[derive(Debug, Default)]
struct Tampering(Mutex<VecDeque<Tamper>>);

impl hoh_app::host::HostHooks for Tampering {
    fn spec_review_native(&self, _attempt_id: &str, mut native: Value) -> Option<Value> {
        if native["spec_revision"] != 3 {
            return Some(native);
        }
        let Some(tamper) = self.0.lock().ok().and_then(|mut q| q.pop_front()) else {
            return Some(native);
        };
        let other = |key: &str, native: &mut Value| {
            native[key] = json!(format!("{}x", native[key].as_str().unwrap_or("")));
        };
        match tamper {
            Tamper::ReviewId => other("review_id", &mut native),
            Tamper::AttemptId => other("attempt_id", &mut native),
            Tamper::Stdout => native["stdout_sha256"] = json!("e".repeat(64)),
            Tamper::SpecSha => native["spec_sha256"] = json!("d".repeat(64)),
            Tamper::GitCommit => native["git_commit"] = json!("f".repeat(40)),
            Tamper::Missing => return None,
        }
        Some(native)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_031_04_spec_review_gates_ready() -> R {
    // Project A: run R in SPECIFYING on revision 1 (REQ-1 without a criterion: missing_ac).
    let world = World::new()?;
    world.configure(&[], "")?;
    world.commit_project(&[])?;
    let first_sha = commit_spec(&world, &markdown("REQ-1", 0, "记下"))?;
    let tampering = Arc::new(Tampering(Mutex::new(VecDeque::from([
        Tamper::ReviewId,
        Tamper::AttemptId,
        Tamper::Stdout,
        Tamper::SpecSha,
        Tamper::GitCommit,
        Tamper::Missing,
    ]))));
    // The reviewer's turns: revision 2's PASS, revision 3's six PASSes the test tampers with, a FAILED, a PASS.
    let turns = Arc::new(AtomicUsize::new(0));
    let counted = turns.clone();
    let team = world::Team::reviewing(
        |turn| match turn.state.as_str() {
            "CREATED" => {
                let mut decided = world::steps("CREATED", "SPECIFYING");
                decided.push(world::rejected(world::transition("READY")));
                decided
            }
            _ => Vec::new(),
        },
        world.project(),
        move |_| {
            if counted.fetch_add(1, Ordering::SeqCst) == 7 {
                Verdict::Failed
            } else {
                Verdict::Pass
            }
        },
    );
    let host = world
        .team_host_hooked(team.clone(), tampering.clone())
        .await?;
    let outcome = async {
        approve_first(&world, &host, &first_sha).await?;
        let run = world::start_run(&host, "r", "做一个记账工具").await?;
        world.staff(&run)?;
        // The Commander's transition_run to READY: REJECTED (ILLEGAL_TRANSITION, details missing_ac); R in SPECIFYING.
        let refused_sql = format!(
            "SELECT payload_json FROM events WHERE run_id = '{run}' AND kind = 'decision.rejected' \
             AND json_extract(payload_json, '$.kind') = 'transition_run'"
        );
        until("the refused transition", || {
            Ok(!payloads(&world, &refused_sql)?.is_empty())
        })
        .await?;
        let refused = payloads(&world, &refused_sql)?;
        assert_eq!(refused[0]["reason_code"], "ILLEGAL_TRANSITION", "{refused:?}");
        assert_eq!(
            refused[0]["details"]["reasons"],
            json!([{"reason": "missing_ac", "ids": ["REQ-1"]}]),
            "{refused:?}"
        );
        let state = || world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"));
        assert_eq!(state()?, "SPECIFYING");
        let commander = world.query(&format!(
            "SELECT session_id FROM role_bindings WHERE run_id = '{run}' AND role = 'commander'"
        ))?;
        let (reports, candidates) = (
            world.query("SELECT count(*) FROM independent_reports")?,
            world.query("SELECT count(*) FROM candidates")?,
        );
        // Revision 2 completes REQ-1's criterion; its review is opened, dispatched and PASSes; then R is READY.
        let second_sha = commit_spec(&world, &markdown("REQ-1", 1, "记下"))?;
        let second_commit = world.git(&["rev-parse", "HEAD"])?.trim().to_owned();
        approve_change(&host, &run, "rev2", &second_sha).await?;
        until("revision 2's report", || {
            Ok(reported(&world, 2)?.iter().any(|r| r["state"] == "CURRENT"))
        })
        .await?;
        until("R READY", || Ok(state()? == "READY")).await?;
        let opened = payloads(
            &world,
            "SELECT payload_json FROM events WHERE kind = 'spec_review.opened' \
             AND json_extract(payload_json, '$.spec_revision') = 2",
        )?;
        assert_eq!(opened.len(), 1, "{opened:?}");
        let opened = &opened[0];
        assert_eq!(opened["git_commit"], json!(second_commit), "{opened}");
        assert_eq!(opened["spec_sha256"], json!(second_sha), "{opened}");
        assert_eq!(opened["run_id"], json!(run), "{opened}");
        let task = opened["task_id"].as_str().ok_or("task")?;
        let attempts = world.rows(&format!(
            "SELECT attempt_id, session_id, state FROM attempts WHERE task_id = '{task}'"
        ))?;
        assert_eq!(attempts.len(), 1, "{attempts:?}");
        assert_ne!(attempts[0]["session_id"], json!(commander));
        let two = reported(&world, 2)?;
        assert_eq!(two.len(), 1, "{two:?}");
        let report = &two[0];
        assert_eq!(
            (&report["status"], &report["state"], &report["review_id"]),
            (&json!("PASS"), &json!("CURRENT"), &opened["review_id"]),
            "{report}"
        );
        for key in ["report", "raw_session", "native_attempt"] {
            let reference = &report[key];
            let bytes = std::fs::read(
                world
                    .project()
                    .join(reference["path"].as_str().ok_or(key)?),
            )?;
            assert_eq!(json!(world::sha256_hex(&bytes)), reference["sha256"], "{key}");
        }
        let native: Value = serde_json::from_slice(&std::fs::read(
            world
                .project()
                .join(report["native_attempt"]["path"].as_str().ok_or("native")?),
        )?)?;
        assert_eq!(native["record_type"], "runtime_spec_review_export");
        for key in ["review_id", "spec_revision", "spec_sha256", "git_commit"] {
            assert_eq!(native[key], opened[key], "{key}");
        }
        assert_eq!(native["attempt_id"], attempts[0]["attempt_id"]);
        assert!(
            native["session_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&report["session_id"])),
            "{native}"
        );
        assert_eq!(native["stdout_sha256"], report["raw_session"]["sha256"]);
        assert_eq!(world.query("SELECT count(*) FROM independent_reports")?, reports);
        assert_eq!(world.query("SELECT count(*) FROM candidates")?, candidates);
        // readiness.passed{spec_revision: 2} after the PASS was taken in, not before.
        let order = world.query(&format!(
            "SELECT (SELECT seq FROM events WHERE kind = 'readiness.passed' AND run_id = '{run}' \
               AND json_extract(payload_json, '$.spec_revision') = 2) > \
             (SELECT seq FROM events WHERE kind = 'spec_review.reported' \
               AND json_extract(payload_json, '$.spec_revision') = 2)"
        ))?;
        assert_eq!(order, "1");
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE kind = 'readiness.passed' AND run_id = '{run}'"
            ))?,
            "1"
        );
        // Revision 3: six tampered PASSes, each REJECTED for its own reason; a FAILED attempt exports nothing; the
        // legal PASS satisfies readiness. Before it, readiness is NOT_READY(spec_review) at every look.
        let third_sha = commit_spec(&world, &markdown("REQ-1", 1, "记下并显示余额"))?;
        approve_change(&host, &run, "rev3", &third_sha).await?;
        // Each look at R's readiness taken before the CURRENT report (checked after the look) counts.
        let mut looks = Vec::new();
        let end = Instant::now() + WAIT;
        loop {
            let (_, got) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
            if reported(&world, 3)?.iter().any(|r| r["state"] == "CURRENT") {
                break;
            }
            looks.push(got["projection"]["readiness"].clone());
            if Instant::now() > end {
                return Err("revision 3's CURRENT report: not within the wait".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!looks.is_empty());
        for look in &looks {
            assert_eq!(look["state"], "NOT_READY", "{looks:?}");
            assert!(
                look["reasons"]
                    .as_array()
                    .is_some_and(|r| r.iter().any(|x| x["reason"] == "spec_review")),
                "{looks:?}"
            );
        }
        let three = reported(&world, 3)?;
        let codes: Vec<String> = three
            .iter()
            .map(|r| {
                r["reasons"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str())
                    .map(|x| x.split(':').next().unwrap_or_default().to_owned())
                    .collect::<Vec<_>>()
                    .join("+")
            })
            .collect();
        let states: Vec<&str> = three.iter().map(|r| r["state"].as_str().unwrap_or("")).collect();
        assert_eq!(
            states,
            ["REJECTED", "REJECTED", "REJECTED", "REJECTED", "REJECTED", "REJECTED", "CURRENT"],
            "{codes:?}"
        );
        for (got, want) in codes.iter().zip([
            "review_id_mismatch",
            "attempt_id_mismatch",
            "stdout_sha256_mismatch",
            "spec_sha256_mismatch",
            "git_commit_mismatch",
            "native_attempt_missing",
        ]) {
            assert!(got.split('+').any(|c| c == want), "{want}: {codes:?}");
        }
        // The FAILED attempt: no report, no record exported.
        let failed = world.rows(
            "SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'reviewer' \
             AND a.state = 'FAILED'",
        )?;
        assert_eq!(failed.len(), 1, "{failed:?}");
        let failed = failed[0]["attempt_id"].as_str().ok_or("failed")?;
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE kind = 'spec_review.reported' \
                 AND json_extract(payload_json, '$.attempt_id') = '{failed}'"
            ))?,
            "0"
        );
        let (_, got) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
        assert_eq!(got["projection"]["readiness"]["state"], "READY", "{got}");
        assert_eq!(turns.load(Ordering::SeqCst), 9);
        if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
            std::fs::write(
                std::path::Path::new(&dir).join("test-031-04-a.json"),
                serde_json::to_vec_pretty(&json!({"refused": refused, "opened": opened,
                    "revision_2": two, "revision_3": three, "failed_attempt": failed,
                    "readiness_looks_before_pass": looks.len()}))?,
            )?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_031_04_a_revision_approved_during_verification_is_reviewed_in_place() -> R {
    // Project B: run R2 on revision 1 in VERIFYING with candidate C under verification; the user approves revision 2's
    // CR. C goes STALE (REQ-033); R2's review of revision 2 is opened after the approval while R2 is VERIFYING or
    // RUNNING, never SPECIFYING; nothing is dispatched for writing under revision 2 before its PASS.
    let world = World::new()?;
    world.configure(&[], "")?;
    world.commit_project(&[])?;
    let first_sha = commit_spec(&world, &markdown("REQ-1", 1, "记下"))?;
    let team = world::Team::reviewing(
        |turn| match turn.state.as_str() {
            "CREATED" => world::steps("CREATED", "SPECIFYING"),
            _ => Vec::new(),
        },
        world.project(),
        |_| Verdict::Pass,
    );
    let host = world
        .team_host_hooked(team.clone(), Arc::new(Tampering::default()))
        .await?;
    let outcome = async {
        approve_first(&world, &host, &first_sha).await?;
        let run = world::start_run(&host, "r2", "做一个记账工具").await?;
        world.staff(&run)?;
        let state = || world.query(&format!("SELECT state FROM runs WHERE run_id = '{run}'"));
        until("R2 READY by its readiness", || Ok(state()? == "READY")).await?;
        // Given: R2 in VERIFYING with candidate C under verification on revision 1.
        world.exec(&format!(
            "UPDATE runs SET state = 'VERIFYING', revision = revision + 1 WHERE run_id = '{run}';
             INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
               test_plan_sha256, source_path, state, created_at) VALUES ('C', '{run}', printf('%.40c', 'a'),
               printf('%.40c', 'b'), '{first_sha}', printf('%.64c', 'e'), printf('%.64c', 'f'), '/src', 'VERIFYING',
               '2026-11-22T00:00:00Z');"
        ))?;
        let second_sha = commit_spec(&world, &markdown("REQ-1", 2, "记下"))?;
        approve_change(&host, &run, "rev2", &second_sha).await?;
        until("revision 2's report", || {
            Ok(reported(&world, 2)?.iter().any(|r| r["state"] == "CURRENT"))
        })
        .await?;
        assert_eq!(
            world.query("SELECT state FROM candidates WHERE candidate_id = 'C'")?,
            "STALE"
        );
        let changed = world.query(&format!(
            "SELECT max(seq) FROM events WHERE run_id = '{run}' AND kind = 'spec.changed'"
        ))?;
        let opened = world.rows(&format!(
            "SELECT seq, payload_json FROM events WHERE kind = 'spec_review.opened' AND run_id = '{run}' \
             AND json_extract(payload_json, '$.spec_revision') = 2"
        ))?;
        assert_eq!(opened.len(), 1, "{opened:?}");
        assert!(
            opened[0]["seq"].as_i64() > changed.parse::<i64>().ok(),
            "opened after the approval: {opened:?}"
        );
        // Never back to SPECIFYING: no move of R2 there since revision 1 was ready, and R2 at work now.
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE run_id = '{run}' AND kind = 'decision.applied' \
                 AND json_extract(payload_json, '$.payload.to_state') = 'SPECIFYING' \
                 AND seq > (SELECT seq FROM events WHERE run_id = '{run}' AND kind = 'readiness.passed')"
            ))?,
            "0"
        );
        let now = state()?;
        assert!(["VERIFYING", "RUNNING"].contains(&now.as_str()), "{now}");
        // Nothing dispatched but the review between the approval and its PASS.
        let reported_seq = world.query(
            "SELECT seq FROM events WHERE kind = 'spec_review.reported' \
             AND json_extract(payload_json, '$.spec_revision') = 2",
        )?;
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events e JOIN tasks t ON t.task_id = json_extract(e.payload_json, '$.task_id') \
                 WHERE e.kind = 'schedule.round' AND e.run_id = '{run}' AND t.role <> 'reviewer' \
                 AND e.seq > {changed} AND e.seq < {reported_seq}"
            ))?,
            "0"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}
