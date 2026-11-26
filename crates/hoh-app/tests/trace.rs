//! REQ-032 (requirement tracing) through the product host and freeze driver; no live agent runs.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;
mod world;

use hoh_app::host::Host;
use hoh_app::verify::freeze::{Inputs, freeze};
use hoh_core::specindex::{self, Index, Source};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use world::{R, World};

const PREFIX: &str =
    "# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n";

/// HoH's own repository, where `scripts/product/traceability.py` lives.
fn hoh_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Requirement `id` with one AC per check in `checks`, numbered from 01.
fn req(id: &str, checks: &[&str]) -> String {
    let mut text = format!("## REQ-{id} Requirement {id}\n");
    for (n, check) in checks.iter().enumerate() {
        let n = n + 1;
        text.push_str(&format!(
            "### AC-{id}-{n:02}\ngiven: fixture {id}\nwhen: operation {n}\nthen: result {n}\ncheck: {check}\n"
        ));
    }
    text
}

/// The AC-032-04 project: two Markdown files, 2 REQs, 3 ACs whose check lines are unit, unit and integration, the
/// generated index, a check plan listing unit and integration; no `contracts/` directory. Committed.
fn project(world: &World) -> R<Index> {
    let files = vec![
        Source {
            path: "docs/spec/a.md".into(),
            bytes: format!("{PREFIX}{}", req("A", &["unit", "unit"])).into_bytes(),
        },
        Source {
            path: "docs/spec/b.md".into(),
            bytes: format!("{PREFIX}{}", req("B", &["integration"])).into_bytes(),
        },
    ];
    write_project(world, files)
}

/// A command of `kind` for run `run` at its current revision.
async fn command(host: &Host, run: &str, key: &str, kind: &str, payload: Value) -> R<Value> {
    let (_, body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    Ok(
        json!({"schema_version":"1.0", "command_id":format!("cmd-{key}"), "idempotency_key":key,
        "expected_revision":body["revision"], "run_id":run, "kind":kind, "payload":payload}),
    )
}

/// A run with `index` approved by the user (ApproveSpec).
async fn approved(world: &World, index: &Index) -> R<(Host, String)> {
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "tracing").await?;
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
    Ok((host, run))
}

/// The run's candidate frozen by the product: its id and its exported manifest.
async fn frozen(world: &World, host: &Host, run: &str) -> R<(String, Value)> {
    let frozen = freeze(
        host.store(),
        &world.project(),
        &world.project().join(".hoh"),
        run,
        &Inputs {
            spec: specindex::PATH.into(),
            environment: "environment.json".into(),
            test_plan: "tests/check-plan.json".into(),
        },
    )
    .await?;
    let hoh_core::persistence::Frozen::New { candidate_id, .. } = frozen else {
        return Err("a new candidate expected".into());
    };
    let path = world.query(&format!(
        "SELECT source_path FROM candidates WHERE candidate_id='{candidate_id}'"
    ))?;
    let manifest: Value = serde_json::from_slice(&std::fs::read(world.project().join(path))?)?;
    Ok((candidate_id, manifest))
}

/// The tracing World of `scripts/product/traceability.py` on the project and the candidate manifest at `candidate`
/// (profile `project`): its TESTs with their checks, each TEST's status without members, and every path Python opened
/// meanwhile (an audit hook on `open`).
const WORLD_PY: &str = r#"import json, sys
from pathlib import Path
opened = []
sys.addaudithook(lambda event, args: opened.append(str(args[0])) if event == 'open' and args and isinstance(args[0], (str, bytes, Path)) else None)
hoh, project, candidate = Path(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3])
sys.path.insert(0, str(hoh / 'scripts/product'))
import traceability
manifest = json.loads(candidate.read_text(encoding='utf-8'))
world = traceability.World(project, manifest, 'project')
tests = world.tests()
found = traceability.members(world)
result = traceability.report(world, found, [])
print(json.dumps({'tests': {t: v.check for t, v in tests.items()},
                  'status': {t: v['status'] for t, v in result['tests'].items()},
                  'opened': opened}))
"#;

fn python_world(world: &World, manifest: &Value) -> R<Value> {
    let file = world.root().join("candidate.json");
    std::fs::write(&file, serde_json::to_vec(manifest)?)?;
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg(WORLD_PY)
        .arg(hoh_root())
        .arg(world.project())
        .arg(&file)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "traceability World: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The product's trace export of run `run` (`GET /v1/runs/{run}`'s `projection.trace`).
async fn export(host: &Host, run: &str) -> R<Value> {
    let (status, body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    assert_eq!(status, 200, "{body}");
    Ok(body["projection"]["trace"].clone())
}

#[tokio::test]
async fn test_032_04_scenarios_from_spec_not_contracts() -> R {
    let world = World::new()?;
    let index = project(&world)?;
    let contracts = world.project().join("contracts");
    assert!(!contracts.exists(), "the fixture project has no contracts/");
    let (host, run) = approved(&world, &index).await?;
    let (candidate, manifest) = frozen(&world, &host, &run).await?;
    // The candidate's spec is the single FileRef to the index.
    assert_eq!(
        manifest["spec"],
        json!({"path": specindex::PATH, "sha256": index.sha256()})
    );
    let scenarios: Vec<(String, String)> = index.value()["scenarios"]
        .as_array()
        .ok_or("no scenarios")?
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap_or_default().to_owned(),
                s["check_id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    let expected = vec![
        ("TEST-A-01".to_owned(), "unit".to_owned()),
        ("TEST-A-02".to_owned(), "unit".to_owned()),
        ("TEST-B-01".to_owned(), "integration".to_owned()),
    ];
    assert_eq!(scenarios, expected, "the index's scenarios");
    let stored: Vec<(String, String)> = world
        .rows("SELECT ac_id, check_id FROM acceptance_criteria ORDER BY ac_id")?
        .iter()
        .map(|r| {
            (
                r["ac_id"]
                    .as_str()
                    .unwrap_or_default()
                    .replacen("AC-", "TEST-", 1),
                r["check_id"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(stored, expected, "acceptance_criteria.check_id");

    // The tracing World of the traceability check, profile `project`.
    let python = python_world(&world, &manifest)?;
    let tests: Vec<(String, String)> = python["tests"]
        .as_object()
        .ok_or("no World tests")?
        .iter()
        .map(|(t, c)| (t.clone(), c.as_str().unwrap_or("<none>").to_owned()))
        .collect();
    assert_eq!(tests, expected, "World.tests()");
    // No member evidence: each TEST is MISSING, without a result and not an error (AC-032-04 as CR-035 corrected it:
    // ADR-027 decision 10 names a TEST whose scenario check has no member at all MISSING; NOT_RUN needs a member).
    assert_eq!(
        python["status"],
        json!({"TEST-A-01": "MISSING", "TEST-A-02": "MISSING", "TEST-B-01": "MISSING"}),
        "no member evidence: no result, not an error"
    );
    let opened: Vec<&str> = python["opened"]
        .as_array()
        .ok_or("no opens")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let prefix = contracts.to_string_lossy().into_owned();
    assert!(
        opened.iter().all(|p| !p.starts_with(&prefix)),
        "the World opened a path under the project's contracts/: {opened:?}"
    );
    assert!(
        opened.iter().any(|p| p.ends_with(specindex::PATH)),
        "the World read the candidate's spec file: {opened:?}"
    );

    // The product's export of the same candidate, served by a real `hoh __host` child traced with strace so every
    // file-open/stat syscall made by the product during export is audited for `<project>/contracts/` (M3QA-015).
    host.shutdown().await;
    let strace_log = world.root().join("product-export.strace");
    let strace_err = world.root().join("product-export.strace.stderr");
    let strace_bin = std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join("strace"))
                .find(|p| p.is_file())
        })
        .unwrap_or_else(|| PathBuf::from("/usr/bin/strace"));
    let mut tracer = {
        use std::os::unix::process::CommandExt as _;
        pty::Hoh(
            std::process::Command::new(strace_bin)
                .args(["-f", "-e", "trace=%file", "-s", "4096", "-o"])
                .arg(&strace_log)
                .arg(world::HOH)
                .arg("__host")
                .arg(world.project())
                .envs(world.env())
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::fs::File::create(&strace_err)?)
                .spawn()?,
        )
    };
    let own_pid = i32::try_from(std::process::id())?;
    let host_json_path = world.project().join(".hoh/host.json");
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let (port, token, host_pid): (u16, String, i32) = loop {
        if let Some(status) = tracer.0.try_wait()? {
            return Err(format!(
                "strace exited before host became ready ({status}): {}",
                std::fs::read_to_string(&strace_err).unwrap_or_default()
            )
            .into());
        }
        if let Ok(bytes) = std::fs::read(&host_json_path)
            && let Ok(v) = serde_json::from_slice::<Value>(&bytes)
            && let (Some(port), Some(token), Some(pid)) =
                (v["port"].as_u64(), v["token"].as_str(), v["pid"].as_u64())
            && pid != u64::from(std::process::id())
            && std::net::TcpStream::connect(("127.0.0.1", port as u16)).is_ok()
        {
            break (port as u16, token.to_owned(), i32::try_from(pid)?);
        }
        if std::time::Instant::now() >= end {
            return Err(format!(
                "traced hoh __host did not become ready: {}",
                std::fs::read_to_string(&strace_err).unwrap_or_default()
            )
            .into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    };
    assert_ne!(
        host_pid, own_pid,
        "host.json must name the traced child host, not this test process"
    );
    let trace: Value = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        use std::io::{Read as _, Write as _};
        let mut stream =
            std::net::TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .map_err(|e| e.to_string())?;
        let req = format!(
            "GET /v1/runs/{run} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-HoH-Token: {token}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;
        let resp = String::from_utf8(raw).map_err(|e| e.to_string())?;
        let (head, body) = resp.split_once("\r\n\r\n").ok_or("no HTTP body")?;
        if !head.starts_with("HTTP/1.1 200") {
            return Err(format!("{head}: {body}"));
        }
        let parsed: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
        Ok(parsed["projection"]["trace"].clone())
    })
    .await?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let host_raw = rustix::process::Pid::from_raw(host_pid).ok_or("host pid")?;
    let _ = rustix::process::kill_process(host_raw, rustix::process::Signal::TERM);
    let term_end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if tracer.0.try_wait()?.is_some() {
            break;
        }
        if std::time::Instant::now() >= term_end {
            if let Ok(raw) = i32::try_from(tracer.0.id())
                && let Some(pgid) = rustix::process::Pid::from_raw(raw)
            {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
            let _ = rustix::process::kill_process(host_raw, rustix::process::Signal::KILL);
            let _ = tracer.0.wait();
            return Err("traced hoh __host did not exit after SIGTERM within 15s".into());
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let traced_calls = std::fs::read_to_string(&strace_log)?;
    assert!(
        traced_calls.contains(&index.sha256()),
        "strace must observe the product reading the frozen candidate spec object {}: {}",
        index.sha256(),
        &traced_calls[..traced_calls.len().min(2000)]
    );
    let contracts_hits: Vec<&str> = traced_calls
        .lines()
        .filter(|line| {
            line.contains(&prefix)
                || line.contains("\"contracts/")
                || line.contains("\"contracts\"")
                || line.contains("/contracts\"")
        })
        .collect();
    assert!(
        contracts_hits.is_empty(),
        "product host opened a path under project contracts/: {contracts_hits:?}"
    );
    assert_eq!(trace["candidate_id"], json!(candidate), "{trace}");
    assert_eq!(trace["spec_source"], "candidate", "{trace}");
    let product: Vec<(String, String)> = trace["tests"]
        .as_array()
        .ok_or("no exported tests")?
        .iter()
        .map(|t| {
            (
                t["test_id"].as_str().unwrap_or_default().to_owned(),
                t["check_id"].as_str().unwrap_or("<none>").to_owned(),
            )
        })
        .collect();
    assert_eq!(product, tests, "the product export and the World agree");
    assert!(!contracts.exists(), "nothing made the project's contracts/");
    Ok(())
}

/// The AC-032-01/-02 project: REQ-A (AC-A-01 unit, AC-A-02 integration) and REQ-B (AC-B-01, AC-B-02 unit), the
/// generated index, a check plan; committed.
fn chain_project(world: &World) -> R<Index> {
    let files = vec![Source {
        path: "docs/spec/spec.md".into(),
        bytes: format!(
            "{PREFIX}{}{}",
            req("A", &["unit", "integration"]),
            req("B", &["unit", "unit"])
        )
        .into_bytes(),
    }];
    write_project(world, files)
}

/// `files`, their index, a check plan and an environment, committed in a new repository.
fn write_project(world: &World, files: Vec<Source>) -> R<Index> {
    let index = specindex::generate_index(&files)?;
    let check = |name: &str| {
        json!({"argv": ["sh", "-c", format!("echo {name}")], "mode": "product", "timeout_seconds": 60,
            "artifacts": ["test-results.jsonl"]})
    };
    let plan = json!({"schema_version": "1.0", "plan_id": "trace-fixture",
        "checks": {"unit": check("unit"), "integration": check("integration")}});
    let mut written: Vec<(String, Vec<u8>)> =
        files.into_iter().map(|f| (f.path, f.bytes)).collect();
    written.extend([
        (specindex::PATH.to_owned(), index.bytes().to_vec()),
        (".gitignore".to_owned(), b".hoh/\n".to_vec()),
        (
            "tests/check-plan.json".to_owned(),
            format!("{}\n", serde_json::to_string_pretty(&plan)?).into_bytes(),
        ),
        ("environment.json".to_owned(), b"{}\n".to_vec()),
    ]);
    for (path, bytes) in written {
        let file = world.project().join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, bytes)?;
    }
    world.git(&["init", "-q"])?;
    world.git(&["add", "."])?;
    world.git(&["commit", "-qm", "specification"])?;
    Ok(index)
}

/// Run `run`'s three tasks (T1: REQ-A, T2: REQ-B, T3: both) and, on `candidate`, one PASS row per AC with its own
/// evidence of the AC's check: a persisted fixture of a verified candidate; no verification process or agent runs.
fn verified(world: &World, run: &str, candidate: &str) -> R {
    verified_with(world, run, candidate, Fault::None)
}

/// A fault of TEST-032-01's fixture, written as the fixture is persisted (inserts only: evidence and acceptance
/// results are append-only, docs/adr/ADR-029.md decision 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fault {
    None,
    /// AC-A-01's PASS cites evidence that is not there.
    MissingEvidence,
    /// AC-B-01's PASS is on an older candidate `CAND-old`, STALE.
    StaleCandidate,
    /// The candidate's row is missing.
    MissingCandidate,
}

/// [`verified`] with `fault`.
fn verified_with(world: &World, run: &str, candidate: &str, fault: Fault) -> R {
    for (seq, (task, requirements)) in [
        ("T1", vec!["REQ-A"]),
        ("T2", vec!["REQ-B"]),
        ("T3", vec!["REQ-A", "REQ-B"]),
    ]
    .into_iter()
    .enumerate()
    {
        world.exec(&format!(
            "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
             SELECT '{task}',run_id,spec_id,'ACCEPTED','developer',1,{seq} FROM runs WHERE run_id='{run}';"
        ))?;
        for r in requirements {
            world.exec(&format!(
                "INSERT INTO task_requirements(task_id,requirement_id,spec_id)
                 SELECT '{task}','{r}',spec_id FROM runs WHERE run_id='{run}';"
            ))?;
        }
    }
    if fault == Fault::StaleCandidate {
        world.exec(&format!(
            "INSERT INTO candidates(candidate_id,run_id,git_commit,git_tree,spec_sha256,environment_sha256,
               test_plan_sha256,source_path,state,created_at)
             SELECT 'CAND-old',run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,
               'STALE',created_at FROM candidates WHERE candidate_id='{candidate}';"
        ))?;
    }
    let skipped = if fault == Fault::MissingEvidence {
        "AND a.ac_id <> 'AC-A-01'"
    } else {
        ""
    };
    let result_candidate = if fault == Fault::StaleCandidate {
        format!("CASE a.ac_id WHEN 'AC-B-01' THEN 'CAND-old' ELSE '{candidate}' END")
    } else {
        format!("'{candidate}'")
    };
    world.exec(&format!(
        "INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
            VALUES('observed','T1','SETTLED',1,1,0);
         INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES('observed','{hash}','fixture',0,'application/json');
         INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at)
         SELECT 'evidence-'||a.ac_id,'{candidate}','observed','observed',a.check_id,'check','PASS','mock',
            '2026-11-22T00:00:00Z'
         FROM acceptance_criteria a JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}' {skipped}
         ORDER BY a.ac_id;
         INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status)
         SELECT a.ac_id,a.spec_id,{result_candidate},'evidence-'||a.ac_id,'PASS' FROM acceptance_criteria a
         JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}' ORDER BY a.ac_id;",
        hash = "0".repeat(64)
    ))?;
    if fault == Fault::MissingCandidate {
        world.exec(&format!(
            "DELETE FROM candidates WHERE candidate_id='{candidate}';"
        ))?;
    }
    Ok(())
}

/// Every item of the paged list at `path` (`limit` 1 per page, so every cursor is exercised).
async fn all_pages(host: &Host, path: &str) -> R<Vec<Value>> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let url = match &cursor {
            Some(c) => format!("{path}?limit=1&cursor={c}"),
            None => format!("{path}?limit=1"),
        };
        let (status, page) = world::exchange(host, "GET", &url, None).await?;
        assert_eq!(status, 200, "{url}: {page}");
        items.extend(page["items"].as_array().ok_or("no items")?.iter().cloned());
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(items.len() < 100, "pagination must advance");
    }
    Ok(items)
}

#[tokio::test]
async fn test_032_02_acceptance_and_tasks_match_db() -> R {
    let world = World::new()?;
    let index = chain_project(&world)?;
    let (host, run) = approved(&world, &index).await?;
    let (candidate, _) = frozen(&world, &host, &run).await?;
    verified(&world, &run, &candidate)?;
    // Each AC's candidate, TEST and evidence references through the API, all pages.
    let api: Vec<Value> = all_pages(&host, &format!("/v1/runs/{run}/acceptance"))
        .await?
        .iter()
        .map(|i| {
            json!({"candidate_id": i["candidate_id"], "ac_id": i["ac_id"], "test_id": i["test_id"],
                "evidence_ids": i["evidence_ids"]})
        })
        .collect();
    assert_eq!(api.len(), 4, "{api:?}");
    // The same straight from the store.
    let db: Vec<Value> = world
        .rows(&format!(
            "SELECT r.candidate_id, a.ac_id, json_extract(a.body,'$.test_id') AS test_id,
                json_group_array(r.evidence_id) AS evidence_ids
             FROM acceptance_criteria a JOIN runs u ON u.spec_id=a.spec_id
             JOIN acceptance_results r ON r.ac_id=a.ac_id AND r.spec_id=a.spec_id
             WHERE u.run_id='{run}' GROUP BY r.candidate_id, a.ac_id ORDER BY a.ac_id"
        ))?
        .into_iter()
        .map(|mut r| {
            r["evidence_ids"] = serde_json::from_str(r["evidence_ids"].as_str().unwrap_or("[]"))
                .unwrap_or(Value::Null);
            r
        })
        .collect();
    assert_eq!(api, db, "acceptance: API and store");
    assert!(api.iter().all(|i| i["test_id"].is_string()), "{api:?}");
    // Each task's requirements through the API, all pages, and straight from the store.
    let tasks: Vec<Value> = all_pages(&host, &format!("/v1/runs/{run}/tasks"))
        .await?
        .iter()
        .map(|t| json!({"task_id": t["task_id"], "requirement_ids": t["requirement_ids"]}))
        .collect();
    let db: Vec<Value> = world
        .rows(&format!(
            "SELECT t.task_id, (SELECT json_group_array(requirement_id) FROM
                (SELECT requirement_id FROM task_requirements q WHERE q.task_id=t.task_id ORDER BY requirement_id))
                AS requirement_ids
             FROM tasks t WHERE t.run_id='{run}' ORDER BY t.created_seq, t.task_id"
        ))?
        .into_iter()
        .map(|mut r| {
            r["requirement_ids"] =
                serde_json::from_str(r["requirement_ids"].as_str().unwrap_or("[]"))
                    .unwrap_or(Value::Null);
            r
        })
        .collect();
    assert_eq!(tasks.len(), 3);
    assert_eq!(tasks, db, "tasks: API and store");
    host.shutdown().await;
    Ok(())
}

/// M3R-001: the first drafting task has no specification, including after its draft is approved.
async fn drafting_task_pages(after_approval: bool) -> R {
    let world = World::new()?;
    let index = project(&world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "trace first drafting task").await?;
    world.exec(&format!(
        "INSERT INTO tasks(task_id,run_id,state,role,writer,created_seq,kind)
         VALUES('draft','{run}','IMPLEMENTED','cto',1,1,'spec_draft');"
    ))?;
    if after_approval {
        let body = command(
            &host,
            &run,
            "approve",
            "ApproveSpec",
            json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
        )
        .await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{response}");
        world.exec(&format!(
            "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
             SELECT 'work-'||requirement_id,'{run}',s.spec_id,'READY','developer',1,2
             FROM requirements q JOIN specs s USING(spec_id);"
        ))?;
    }
    let expected = world.rows(&format!(
        "SELECT task_id,spec_id FROM tasks WHERE run_id='{run}' ORDER BY created_seq,task_id"
    ))?;
    assert_eq!(expected.len(), if after_approval { 3 } else { 1 });
    assert_eq!(expected[0], json!({"task_id":"draft","spec_id":null}));
    assert!(expected.iter().skip(1).all(|t| t["spec_id"].is_string()));
    let path = format!("/v1/runs/{run}/tasks");
    let (status, page) = world::exchange(&host, "GET", &path, None).await?;
    assert_eq!(status, 200, "{page}");
    let paged = all_pages(&host, &path).await?;
    assert_eq!(page["items"], json!(paged));
    let projected: Vec<Value> = paged
        .iter()
        .map(|t| {
            assert!(
                t.get("spec_id").is_some(),
                "spec_id must be explicit, including null"
            );
            json!({"task_id":t["task_id"],"spec_id":t["spec_id"]})
        })
        .collect();
    assert_eq!(
        projected, expected,
        "HTTP pages preserve database identities"
    );
    assert_eq!(paged[0]["requirement_ids"], json!([]));
    assert_eq!(
        world.query("SELECT spec_id IS NULL FROM tasks WHERE task_id='draft'")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_032_02_first_drafting_task_before_approval() -> R {
    drafting_task_pages(false).await
}

#[tokio::test]
async fn test_032_02_first_drafting_task_after_approval_across_pages() -> R {
    drafting_task_pages(true).await
}

/// The product's trace exports for TEST-032-01 (docs/adr/ADR-039.md decision 3 and its round 1 corrections): what
/// `tests/bundle/fixtures/trace-export.json` must be, `{whole, missing_evidence, stale_candidate, missing_candidate}`,
/// its run and candidate ids replaced by fixed placeholders.
const TRACE_FIXTURE: &str = "tests/bundle/fixtures/trace-export.json";

/// The mirror's `chain` (scripts/product/traceability.py) on `export`: its broken, orphan and stale.
fn mirror_chain(export: &Value) -> R<Value> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("python3")
        .arg("-c")
        .arg(
            "import json, sys\nsys.path.insert(0, sys.argv[1] + '/scripts/product')\nimport traceability\n\
             print(json.dumps(traceability.chain(json.load(sys.stdin))))",
        )
        .arg(hoh_root())
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(&serde_json::to_vec(export)?)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(format!(
            "traceability.chain: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The product's current export of TEST-032-01's fixture persisted with `fault` in a store of its own, through the
/// product host, checked item by item against the mirror's chain on the same export; with its run and candidate ids.
async fn export_with(fault: Fault) -> R<(Value, String, String)> {
    let world = World::new()?;
    let index = chain_project(&world)?;
    let (host, run) = approved(&world, &index).await?;
    let (candidate, _) = frozen(&world, &host, &run).await?;
    verified_with(&world, &run, &candidate, fault)?;
    let trace = export(&host, &run).await?;
    host.shutdown().await;
    let mirror = mirror_chain(&trace)?;
    for list in ["broken", "orphan", "stale"] {
        assert_eq!(
            mirror[list], trace[list],
            "{fault:?} {list}: the mirror and the product's export differ"
        );
    }
    Ok((trace, run, candidate))
}

/// REQ-032 / AC-032-01 (TEST-032-01; docs/adr/ADR-039.md decision 3, round 1 corrections M3N-004, M3N-005): the
/// product's current trace export, through the product host, of the fixture whole and over three faults persisted in
/// its store — one TEST's evidence missing, a PASS on a STALE older candidate, the candidate row missing — each
/// compared item by item with the mirror (`traceability.chain`), with the exact entries each fault must report. The
/// four exports are pinned byte for byte in `tests/bundle/fixtures/trace-export.json` (rewritten with
/// `HOH_BLESS_TRACE=1`), which the bundle's `test_032_01` checks against the mirror too.
#[tokio::test]
async fn test_032_01_product_export_over_fault_variants_matches_the_mirror() -> R {
    let mut exports = serde_json::Map::new();
    let mut placeholders = Vec::new();
    for (name, fault) in [
        ("whole", Fault::None),
        ("missing_evidence", Fault::MissingEvidence),
        ("stale_candidate", Fault::StaleCandidate),
        ("missing_candidate", Fault::MissingCandidate),
    ] {
        let (trace, run, candidate) = export_with(fault).await?;
        placeholders.push((run, candidate));
        exports.insert(name.to_owned(), trace);
    }
    let whole = &exports["whole"];
    assert_eq!(
        (&whole["broken"], &whole["orphan"], &whole["stale"]),
        (&json!([]), &json!([]), &json!([])),
        "{whole}"
    );
    assert!(whole["candidate_id"].is_string(), "{whole}");
    // One TEST's evidence missing: that TEST is broken (missing evidence), nothing else.
    let missing_evidence = &exports["missing_evidence"];
    assert_eq!(
        missing_evidence["broken"],
        json!([{"node": "test", "id": "TEST-A-01", "missing": "evidence"}]),
        "{missing_evidence}"
    );
    assert_eq!(
        (&missing_evidence["orphan"], &missing_evidence["stale"]),
        (&json!([]), &json!([]))
    );
    // AC-B-01's PASS on an older STALE candidate: an orphan (not the current candidate) and stale.
    let stale_candidate = &exports["stale_candidate"];
    let rebound = json!([{"ac_id": "AC-B-01", "spec_revision": 1, "candidate_id": "CAND-old",
        "evidence_id": "evidence-AC-B-01", "status": "PASS"}]);
    assert_eq!(
        (
            &stale_candidate["broken"],
            &stale_candidate["orphan"],
            &stale_candidate["stale"]
        ),
        (&json!([]), &rebound, &rebound),
        "{stale_candidate}"
    );
    // The candidate row missing: its evidence and results stay reachable through their producer attempt's task, and
    // each evidence reports its missing candidate (M3N-004).
    let missing_candidate = &exports["missing_candidate"];
    let acs = ["AC-A-01", "AC-A-02", "AC-B-01", "AC-B-02"];
    assert_eq!(
        missing_candidate["broken"],
        json!(
            acs.iter()
                .map(|ac| json!({"node": "evidence", "id": format!("evidence-{ac}"), "missing": "candidate"}))
                .collect::<Vec<_>>()
        ),
        "{missing_candidate}"
    );
    for nodes in ["evidence", "results"] {
        assert_eq!(
            missing_candidate[nodes].as_array().map(Vec::len),
            Some(4),
            "{nodes}: {missing_candidate}"
        );
    }
    assert_eq!(missing_candidate["candidate_id"], Value::Null);
    let mut text = format!("{}\n", serde_json::to_string_pretty(&exports)?);
    for (run, candidate) in &placeholders {
        text = text.replace(run, "RUN").replace(candidate, "CANDIDATE");
    }
    let fixture = hoh_root().join(TRACE_FIXTURE);
    if std::env::var_os("HOH_BLESS_TRACE").is_some() {
        std::fs::write(&fixture, &text)?;
    }
    let committed = std::fs::read_to_string(&fixture).unwrap_or_default();
    assert!(
        committed == text,
        "{TRACE_FIXTURE} is not the product's current export; rerun with HOH_BLESS_TRACE=1 and review the diff:\n{text}"
    );
    // Run by the traceability check (docs/adr/ADR-039.md DC-T07b): the exports, as the product wrote them, for its
    // self-test, each with the check's nonce and the tree they were produced on.
    if let Some(dir) = std::env::var_os("HOH_TRACE_EXPORT_DIR") {
        write_exports(Path::new(&dir), &exports)?;
    }
    Ok(())
}

/// The file each variant's export is written to in the traceability check's evidence directory (its check plan
/// declares them).
const EXPORT_FILES: [(&str, &str); 4] = [
    ("whole", "trace-export-whole.json"),
    ("missing_evidence", "trace-export-missing-evidence.json"),
    ("stale_candidate", "trace-export-stale-candidate.json"),
    ("missing_candidate", "trace-export-missing-candidate.json"),
];

/// Write `exports` into `dir` as `{variant, nonce, git_tree, export}` (DC-T07b (1)): the nonce is the check's
/// `HOH_TRACE_EXPORT_NONCE`, the tree HoH's checkout's `HEAD^{tree}`, refused when the checkout has uncommitted changes.
fn write_exports(dir: &Path, exports: &serde_json::Map<String, Value>) -> R {
    let nonce = std::env::var("HOH_TRACE_EXPORT_NONCE")
        .map_err(|_| "HOH_TRACE_EXPORT_DIR without HOH_TRACE_EXPORT_NONCE")?;
    let git = |args: &[&str]| -> R<String> {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(hoh_root())
            .args(args)
            .output()?;
        if !out.status.success() {
            return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
        Ok(String::from_utf8(out.stdout)?)
    };
    let dirty = git(&["status", "--porcelain=v1", "--untracked-files=normal"])?;
    if !dirty.is_empty() {
        return Err(
            format!("HoH's checkout has uncommitted changes, no export written:\n{dirty}").into(),
        );
    }
    let tree = git(&["rev-parse", "HEAD^{tree}"])?.trim().to_owned();
    for (variant, file) in EXPORT_FILES {
        let doc = json!({"variant": variant, "nonce": nonce, "git_tree": tree, "export": exports[variant]});
        std::fs::write(dir.join(file), serde_json::to_vec_pretty(&doc)?)?;
    }
    Ok(())
}

#[tokio::test]
async fn test_032_03_revision_stales_both_chains() -> R {
    let world = World::new()?;
    let index = chain_project(&world)?;
    let (host, run) = approved(&world, &index).await?;
    let (candidate, _) = frozen(&world, &host, &run).await?;
    verified(&world, &run, &candidate)?;
    let before = export(&host, &run).await?;
    assert_eq!(before["candidate_id"], json!(candidate));
    assert_eq!(
        (&before["broken"], &before["orphan"], &before["stale"]),
        (&json!([]), &json!([]), &json!([])),
        "revision 1's chain is whole: {before}"
    );
    let history = world.rows("SELECT * FROM acceptance_results ORDER BY rowid")?;
    assert_eq!(history.len(), 4);
    // Revision 2 rewrites only REQ-A (AC-A-02's then), proposed and approved through the product.
    let path = world.project().join("docs/spec/spec.md");
    let text = std::fs::read_to_string(&path)?.replacen("then: result 2", "then: result X", 1);
    std::fs::write(&path, &text)?;
    let next = specindex::generate_index(&[Source {
        path: "docs/spec/spec.md".into(),
        bytes: text.into_bytes(),
    }])?;
    assert_ne!(next.sha256(), index.sha256());
    for (key, kind, payload) in [
        (
            "revise",
            "ProposeSpecChange",
            json!({"spec_sha256": next.sha256(), "reason": "revise REQ-A"}),
        ),
        (
            "approve-2",
            "ApproveSpec",
            json!({"spec_sha256": next.sha256(), "decision": "APPROVE_ONLY"}),
        ),
    ] {
        let body = command(&host, &run, key, kind, payload).await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{kind}: {response}");
    }
    let changed =
        world.query("SELECT json_extract(impact_json, '$.requirements') FROM change_requests")?;
    assert_eq!(changed, r#"["REQ-A"]"#, "only REQ-A changed");
    let after = export(&host, &run).await?;
    assert_eq!(after["spec_revision"], 2, "{after}");
    assert_eq!(
        after["candidate_id"],
        Value::Null,
        "no current candidate: {after}"
    );
    // Every acceptance node of revision 1 is stale, REQ-B's unchanged chain as well as REQ-A's.
    let stale: Vec<&str> = after["stale"]
        .as_array()
        .ok_or("no stale")?
        .iter()
        .filter_map(|n| n["ac_id"].as_str())
        .collect();
    assert_eq!(
        stale,
        ["AC-A-01", "AC-A-02", "AC-B-01", "AC-B-02"],
        "{after}"
    );
    let results = after["results"].as_array().ok_or("no results")?;
    assert_eq!(results.len(), 4, "history stays: {after}");
    assert!(
        results.iter().all(|r| r["stale"] == true
            && r["spec_revision"] == 1
            && r["candidate_id"] == json!(candidate)),
        "{after}"
    );
    // No node of revision 2 carries a revision-1 PASS: its acceptance has none, and every revision-1 PASS is an
    // orphan, outside the current progress.
    let acceptance = after["acceptance"].as_array().ok_or("no acceptance")?;
    assert_eq!(acceptance.len(), 4);
    assert!(
        acceptance
            .iter()
            .all(|a| a["spec_revision"] == 2 && a["statuses"] == json!([])),
        "{after}"
    );
    assert_eq!(after["orphan"].as_array().map(Vec::len), Some(4), "{after}");
    assert!(
        after["broken"]
            .as_array()
            .ok_or("no broken")?
            .iter()
            .all(|b| b["node"] == "test" && b["missing"] == "evidence"),
        "revision 2 is not verified yet: {after}"
    );
    // History is queryable: the rows are unchanged and the API lists them as STALE on the candidate.
    assert_eq!(
        world.rows("SELECT * FROM acceptance_results ORDER BY rowid")?,
        history
    );
    let items = all_pages(&host, &format!("/v1/runs/{run}/acceptance")).await?;
    let old: Vec<&Value> = items.iter().filter(|i| i["spec_revision"] == 1).collect();
    assert_eq!(old.len(), 4);
    assert!(
        old.iter()
            .all(|i| i["label"] == "STALE" && i["candidate_id"] == json!(candidate)),
        "{items:?}"
    );
    assert!(
        items
            .iter()
            .filter(|i| i["spec_revision"] == 2)
            .all(|i| i["label"] == "NOT_RUN" && i["candidate_id"].is_null()),
        "{items:?}"
    );
    host.shutdown().await;
    Ok(())
}
