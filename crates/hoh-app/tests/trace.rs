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
    // No member evidence: each TEST is reported without a result, not an error. ADR-027 decision 10 names a TEST
    // whose scenario check has no member at all MISSING (NOT_RUN needs a member of its check); docs/adr/ADR-039.md
    // records this reading of AC-032-04's "NOT_RUN".
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

    // The product's export of the same candidate: TEST→check item for item the same as the World's.
    let trace = export(&host, &run).await?;
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
    host.shutdown().await;
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
    world.exec(&format!(
        "INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision)
            VALUES('observed','T1','SETTLED',1,1,0);
         INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES('observed','{hash}','fixture',0,'application/json');
         INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at)
         SELECT 'evidence-'||a.ac_id,'{candidate}','observed','observed',a.check_id,'check','PASS','mock',
            '2026-11-22T00:00:00Z'
         FROM acceptance_criteria a JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}' ORDER BY a.ac_id;
         INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status)
         SELECT a.ac_id,a.spec_id,'{candidate}','evidence-'||a.ac_id,'PASS' FROM acceptance_criteria a
         JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}' ORDER BY a.ac_id;",
        hash = "0".repeat(64)
    ))?;
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

/// The product's trace export for TEST-032-01 (docs/adr/ADR-039.md decision 3): what
/// `tests/bundle/fixtures/trace-export.json` must be, its run and candidate ids replaced by fixed placeholders.
const TRACE_FIXTURE: &str = "tests/bundle/fixtures/trace-export.json";

#[tokio::test]
async fn trace_export_fixture_is_the_products_current_export() -> R {
    let world = World::new()?;
    let index = chain_project(&world)?;
    let (host, run) = approved(&world, &index).await?;
    let (candidate, _) = frozen(&world, &host, &run).await?;
    verified(&world, &run, &candidate)?;
    let trace = export(&host, &run).await?;
    host.shutdown().await;
    let text = format!("{}\n", serde_json::to_string_pretty(&trace)?)
        .replace(&run, "RUN")
        .replace(&candidate, "CANDIDATE");
    let fixture = hoh_root().join(TRACE_FIXTURE);
    if std::env::var_os("HOH_BLESS_TRACE").is_some() {
        std::fs::write(&fixture, &text)?;
    }
    let committed = std::fs::read_to_string(&fixture).unwrap_or_default();
    assert!(
        committed == text,
        "{TRACE_FIXTURE} is not the product's current export; rerun with HOH_BLESS_TRACE=1 and review the diff:\n{text}"
    );
    Ok(())
}
