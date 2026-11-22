//! REQ-034 through the product host and freeze driver; no live agent runs.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod spec_source {
    use super::*;
    mod inputs;
    mod origin;
    mod tasks;
}

mod pty;
mod world;

use hoh_app::host::Host;
use hoh_app::verify::freeze::{Inputs, freeze};
use hoh_core::specindex::{self, Index, Source};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt as _;
use world::{R, World};

const PREFIX: &str =
    "# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n";

fn req(id: &str, count: usize) -> String {
    let mut text = format!("## REQ-{id} Requirement {id}\n");
    for n in 1..=count {
        text.push_str(&format!("### AC-{id}-{n:02}\ngiven: fixture {id}\nwhen: operation {n}\nthen: result {n}\ncheck: check-{id}-{n}\n"));
    }
    text
}

fn sources() -> Vec<Source> {
    vec![
        Source {
            path: "docs/spec/a.md".into(),
            bytes: format!("{PREFIX}{}{}", req("A", 2), req("B", 3)).into_bytes(),
        },
        Source {
            path: "docs/spec/b.md".into(),
            bytes: format!("{PREFIX}{}", req("C", 2)).into_bytes(),
        },
    ]
}

fn repository(world: &World) -> R<Index> {
    let files = sources();
    let index = specindex::generate_index(&files)?;
    for file in &files {
        let path = world.project().join(&file.path);
        std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
        std::fs::write(path, &file.bytes)?;
    }
    for (path, bytes) in [
        (specindex::PATH, index.bytes()),
        (
            ".gitignore",
            b".hoh/\n/contracts/requirements.json\n".as_slice(),
        ),
        (
            "tests/check-plan.json",
            b"{\"schema_version\":\"1.0\",\"plan_id\":\"fixture\",\"checks\":{}}\n".as_slice(),
        ),
        ("environment.json", b"{}\n".as_slice()),
    ] {
        let file = world.project().join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, bytes)?;
    }
    world.git(&["init", "-q"])?;
    world.git(&["add", "."])?;
    world.git(&["commit", "-qm", "specification"])?;
    Ok(index)
}

async fn command(host: &Host, run: &str, key: &str, kind: &str, payload: Value) -> R<Value> {
    let (_, run_body) = world::exchange(host, "GET", &format!("/v1/runs/{run}"), None).await?;
    Ok(
        json!({"schema_version":"1.0", "command_id":format!("cmd-{key}"), "idempotency_key":key,
        "expected_revision":run_body["revision"], "run_id":run, "kind":kind, "payload":payload}),
    )
}

#[tokio::test]
async fn test_034_01_approve_freezes_index_json() -> R {
    assert!(
        !rustix::process::getuid().is_root(),
        "Q7 requires a non-root test process"
    );
    let world = World::new()?;
    let index = repository(&world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "specification source").await?;
    let body = command(
        &host,
        &run,
        "approve",
        "ApproveSpec",
        json!({"spec_sha256": index.sha256(), "decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, accepted) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(
        status, 202,
        "first specification approval must be supported: {accepted}"
    );
    let specs = world.rows("SELECT spec_id,sha256,revision FROM specs")?;
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0]["revision"], 1);
    assert_eq!(specs[0]["sha256"], index.sha256());
    assert_eq!(
        world.rows("SELECT spec_id FROM runs")?[0]["spec_id"],
        specs[0]["spec_id"]
    );
    let requirements = world.rows("SELECT requirement_id,title,release_class,required_release FROM requirements ORDER BY requirement_id")?;
    assert_eq!(requirements.len(), 3);
    for (row, expected) in requirements.iter().zip(
        index.value()["requirements"]
            .as_array()
            .ok_or("no requirements")?,
    ) {
        assert_eq!(row["requirement_id"], expected["id"]);
        assert_eq!(row["title"], expected["title"]);
        assert_eq!(row["release_class"], expected["release_class"]);
        assert_eq!(row["required_release"], 1);
    }
    let criteria = world.rows(
        "SELECT ac_id,requirement_id,body,check_id FROM acceptance_criteria ORDER BY ac_id",
    )?;
    assert_eq!(criteria.len(), 7);
    // Read the expected mapping from the authored Markdown, independently of the product index.
    let mut markdown_checks = std::collections::BTreeMap::new();
    for source in sources() {
        let mut acceptance = None;
        let markdown = std::fs::read_to_string(world.project().join(&source.path))?;
        for line in markdown.lines() {
            if let Some(id) = line.strip_prefix("### ") {
                acceptance = Some(id.to_owned());
            } else if let Some(check) = line.strip_prefix("check: ") {
                let id = acceptance.take().ok_or("check without AC")?;
                assert!(markdown_checks.insert(id, check.to_owned()).is_none());
            }
        }
    }
    assert_eq!(markdown_checks.len(), 7);
    let scenarios = index.value()["scenarios"]
        .as_array()
        .ok_or("no scenarios")?;
    assert_eq!(scenarios.len(), 7, "exactly one scenario per authored AC");
    for scenario in scenarios {
        let ac = scenario["acceptance_id"].as_str().ok_or("no scenario AC")?;
        assert_eq!(
            scenario["check_id"], markdown_checks[ac],
            "scenario check must match Markdown for {ac}"
        );
    }
    for row in &criteria {
        let ac_id = row["ac_id"].as_str().ok_or("no row AC")?;
        assert_eq!(
            row["check_id"], markdown_checks[ac_id],
            "stored check must match Markdown for {ac_id}"
        );
        let scenario = scenarios
            .iter()
            .find(|s| s["acceptance_id"] == row["ac_id"])
            .ok_or("missing scenario")?;
        assert_eq!(row["ac_id"], scenario["acceptance_id"]);
        assert_eq!(row["requirement_id"], scenario["requirement_id"]);
        assert_eq!(row["check_id"], scenario["check_id"]);
        let ac: Value = serde_json::from_str(row["body"].as_str().ok_or("no AC body")?)?;
        assert_eq!(ac["id"], row["ac_id"]);
        assert!(ac["given"].as_str().is_some() && ac["then"].as_str().is_some());
    }
    let events = world.rows("SELECT payload_json FROM events WHERE kind='spec.changed'")?;
    assert_eq!(events.len(), 1);
    let event: Value =
        serde_json::from_str(events[0]["payload_json"].as_str().ok_or("no payload")?)?;
    assert_eq!(event["cause"], "approved");
    assert_eq!(event["spec_revision"], 1);
    assert_eq!(event["from"], Value::Null);
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
    let replay = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(replay, (202, accepted));
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");

    // The old conventional path cannot be opened, but the product's index-based freeze succeeds.
    let obsolete = world.project().join("contracts/requirements.json");
    std::fs::create_dir_all(obsolete.parent().ok_or("no parent")?)?;
    std::fs::write(&obsolete, b"obsolete")?;
    std::fs::set_permissions(&obsolete, std::fs::Permissions::from_mode(0o000))?;
    assert!(std::fs::read(&obsolete).is_err());
    let frozen = freeze(
        host.store(),
        &world.project(),
        &world.project().join(".hoh"),
        &run,
        &Inputs {
            spec: specindex::PATH.into(),
            environment: "environment.json".into(),
            test_plan: "tests/check-plan.json".into(),
        },
    )
    .await?;
    let candidates = world.rows("SELECT candidate_id,spec_sha256,source_path FROM candidates")?;
    assert_eq!(candidates[0]["spec_sha256"], index.sha256());
    let hoh_core::persistence::Frozen::New { candidate_id, .. } = frozen else {
        return Err("expected new candidate".into());
    };
    assert_eq!(candidates[0]["candidate_id"], candidate_id);
    let manifest: Value = serde_json::from_slice(&std::fs::read(
        world
            .project()
            .join(candidates[0]["source_path"].as_str().ok_or("no export")?),
    )?)?;
    assert_eq!(
        manifest["spec"],
        json!({"path":specindex::PATH,"sha256":index.sha256()})
    );
    assert_eq!(manifest["spec_sha256"], index.sha256());
    assert_eq!(
        index.value()["files"].as_array().ok_or("no files")?.len(),
        2
    );
    host.shutdown().await;
    Ok(())
}

async fn approved(world: &World) -> R<(Host, String, Index)> {
    let index = repository(world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "start", "specification source").await?;
    let body = command(
        &host,
        &run,
        "approve",
        "ApproveSpec",
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{response}");
    Ok((host, run, index))
}

fn snapshot(world: &World) -> R<Vec<Vec<Value>>> {
    [
        "specs",
        "requirements",
        "acceptance_criteria",
        "runs",
        "commands",
        "change_requests",
        "events",
    ]
    .iter()
    .map(|table| world.rows(&format!("SELECT * FROM {table} ORDER BY rowid")))
    .collect()
}

fn changed(world: &World) -> R<Index> {
    let mut files = sources();
    files[0].bytes.extend_from_slice(b"\n");
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    Ok(specindex::generate_index(&files)?)
}

#[tokio::test]
async fn test_034_02_approve_digest_conflict() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let next = changed(&world)?;
    assert_ne!(first.sha256(), next.sha256());
    std::fs::write(world.project().join(specindex::PATH), next.bytes())?;
    for (key, expected, tracked) in [
        ("old-digest", first.sha256(), next.bytes().to_vec()),
        (
            "tracked-drift",
            next.sha256(),
            b"{\"manual\":true}\n".to_vec(),
        ),
    ] {
        std::fs::write(world.project().join(specindex::PATH), &tracked)?;
        let before = snapshot(&world)?;
        let body = command(
            &host,
            &run,
            key,
            "ApproveSpec",
            json!({"spec_sha256":expected,"decision":"APPROVE_ONLY"}),
        )
        .await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(
            status, 409,
            "digest mismatch must be a SPEC_CONFLICT: {response}"
        );
        assert_eq!(response["code"], "SPEC_CONFLICT");
        assert_eq!(response["details"]["expected"], expected);
        assert_eq!(response["details"]["actual"], next.sha256());
        if key == "tracked-drift" {
            assert_eq!(response["details"]["tracked"], specindex::digest(&tracked));
        } else {
            assert!(response["details"].get("tracked").is_none());
        }
        assert_eq!(
            snapshot(&world)?,
            before,
            "a conflicting approval writes no database row"
        );
        assert_eq!(
            std::fs::read(world.project().join(specindex::PATH))?,
            tracked
        );
        assert_eq!(
            std::fs::read(world.project().join("docs/spec/a.md"))?,
            [sources()[0].bytes.clone(), b"\n".to_vec()].concat()
        );
    }
    std::fs::write(world.project().join(specindex::PATH), next.bytes())?;
    world.git(&["rm", "--cached", "--", specindex::PATH])?;
    let before = snapshot(&world)?;
    let body = command(
        &host,
        &run,
        "untracked",
        "ApproveSpec",
        json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 409, "{response}");
    assert_eq!(
        response["details"],
        json!({"expected":next.sha256(),"actual":next.sha256(),"tracked":null})
    );
    assert_eq!(snapshot(&world)?, before);
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_03_invalid_markdown_keeps_revision() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}';"))?;
    world.staff(&run)?;
    for n in 1..=3 {
        world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) SELECT 'T{n}',run_id,spec_id,'READY','researcher',0,{n} FROM runs WHERE run_id='{run}';
            INSERT INTO task_requirements(task_id,requirement_id,spec_id) SELECT 'T{n}','REQ-A',spec_id FROM runs WHERE run_id='{run}';"))?;
    }
    let invalid =
        String::from_utf8(sources()[0].bytes.clone())?.replacen("then: result 1\n", "", 1);
    std::fs::write(world.project().join("docs/spec/a.md"), invalid.as_bytes())?;
    let body = command(
        &host,
        &run,
        "bad-proposal",
        "ProposeSpecChange",
        json!({"spec_sha256":first.sha256(),"reason":"correct the specification"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 422, "{response}");
    assert_eq!(
        response["code"], "SPEC_INVALID",
        "invalid Markdown must be diagnosed"
    );
    assert_eq!(response["details"]["path"], "docs/spec/a.md");
    assert!(response["details"]["line"].as_u64().is_some_and(|n| n > 0));
    assert!(
        response["details"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("then"))
    );
    let failures = world.rows("SELECT payload_json FROM events WHERE kind='spec.index_failed'")?;
    assert_eq!(failures.len(), 1);
    let failure: Value =
        serde_json::from_str(failures[0]["payload_json"].as_str().ok_or("no payload")?)?;
    assert_eq!(failure["path"], response["details"]["path"]);
    assert_eq!(failure["line"], response["details"]["line"]);
    assert_eq!(failure["error"], response["details"]["error"]);
    assert_eq!(failure["spec_revision"], 1);
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "0");
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        first.bytes()
    );
    let context = hoh_core::decision::Context {
        required_capabilities: [("researcher".to_owned(), Default::default())].into(),
        reader_slots: Some(3),
        ..Default::default()
    };
    for n in 1..=3 {
        let round = host.store().schedule_round(&run, context.clone()).await?;
        assert_eq!(
            round
                .dispatched
                .ok_or_else(|| format!("round {n}: no task dispatched under revision 1"))?
                .task_id,
            format!("T{n}")
        );
        let revisions = world.rows("SELECT s.revision FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN specs s ON s.spec_id=t.spec_id")?;
        assert_eq!(revisions.len(), n);
        assert!(revisions.iter().all(|r| r["revision"] == 1));
        assert_eq!(
            world.query("SELECT s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id")?,
            "1"
        );
    }
    let next = changed(&world)?;
    let propose = command(
        &host,
        &run,
        "fixed-proposal",
        "ProposeSpecChange",
        json!({"spec_sha256":next.sha256(),"reason":"fixed Markdown"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&propose)).await?;
    assert_eq!(status, 202, "{response}");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
    assert_eq!(
        world.query("SELECT state FROM change_requests")?,
        "PROPOSED"
    );
    let approve = command(
        &host,
        &run,
        "fixed-approval",
        "ApproveSpec",
        json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&approve)).await?;
    assert_eq!(status, 202, "{response}");
    assert_eq!(world.query("SELECT max(revision) FROM specs")?, "2");
    assert_eq!(
        world.query("SELECT sha256 FROM specs WHERE revision=2")?,
        next.sha256()
    );
    assert_eq!(
        world.query("SELECT s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id")?,
        "2"
    );
    assert_eq!(world.query("SELECT cr.state || '/' || s.revision FROM change_requests cr JOIN specs s ON s.spec_id=cr.result_spec_id")?, "APPLIED/2");
    assert_eq!(world.query("SELECT count(*) FROM acceptance_criteria a JOIN specs s ON s.spec_id=a.spec_id WHERE s.revision=2")?, "7");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_03_invalid_approval_records_only_its_diagnostic() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let invalid =
        String::from_utf8(sources()[0].bytes.clone())?.replacen("then: result 1\n", "", 1);
    std::fs::write(world.project().join("docs/spec/a.md"), invalid)?;
    let before = snapshot(&world)?;
    let body = command(
        &host,
        &run,
        "bad-approval",
        "ApproveSpec",
        json!({"spec_sha256":first.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 422, "{response}");
    assert_eq!(response["code"], "SPEC_INVALID");
    assert_eq!(response["details"]["line"], 16);
    assert_eq!(
        &snapshot(&world)?[..6],
        &before[..6],
        "invalid approval must preserve every non-event row"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='spec.index_failed'")?,
        "1"
    );
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        first.bytes()
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_03_proposal_replay_finishes_interrupted_index_publication() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let mut files = sources();
    files[0].bytes = String::from_utf8(files[0].bytes.clone())?
        .replacen("then: result 1", "then: amended result", 1)
        .into_bytes();
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    let next = specindex::generate_index(&files)?;
    let pending = world.project().join(".hoh/pending");
    std::fs::create_dir_all(&pending)?;
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o500))?;
    let body = command(
        &host,
        &run,
        "publish-retry",
        "ProposeSpecChange",
        json!({"spec_sha256":next.sha256(),"reason":"amended result"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o700))?;
    assert_eq!(status, 503, "{response}");
    assert!(
        response["message"]
            .as_str()
            .is_some_and(|text| text.contains("proposal accepted; index publication failed"))
    );
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        first.bytes()
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
    let cr = world.rows("SELECT impact_json FROM change_requests")?;
    let impact: Value = serde_json::from_str(cr[0]["impact_json"].as_str().ok_or("no impact")?)?;
    assert_eq!(
        impact,
        json!({"requirements":["REQ-A"],"acceptance":["AC-A-01","AC-A-02"],"tests":["TEST-A-01","TEST-A-02"],"tasks":[]})
    );
    let before = snapshot(&world)?;
    host.shutdown().await;
    let host = world.start_plain_host(None).await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{response}");
    assert_eq!(
        std::fs::read(world.project().join(specindex::PATH))?,
        next.bytes()
    );
    // Host restart records its own events, but replay creates neither CR nor specification nor another drift.
    let after = snapshot(&world)?;
    assert_eq!(&after[..3], &before[..3]);
    assert_eq!(after[5], before[5]);
    assert_eq!(
        world.query("SELECT count(*) FROM events WHERE kind='spec.generated_drift'")?,
        "0"
    );
    let approve = command(
        &host,
        &run,
        "after-publication",
        "ApproveSpec",
        json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&approve)).await?;
    assert_eq!(status, 202, "{response}");
    assert_eq!(world.query("SELECT max(revision) FROM specs")?, "2");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_02_cr_base_conflict_is_read_only() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let mut proposals = Vec::new();
    for suffix in ["A", "B"] {
        let mut files = sources();
        // Change prose, with no guessed impact edge from its filename.
        files[0].bytes = String::from_utf8(sources()[0].bytes.clone())?
            .replacen("用户\n# 范围", &format!("用户{suffix}\n# 范围"), 1)
            .into_bytes();
        std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
        let next = specindex::generate_index(&files)?;
        let body = command(
            &host,
            &run,
            &format!("propose-{suffix}"),
            "ProposeSpecChange",
            json!({"spec_sha256":next.sha256(),"reason":suffix}),
        )
        .await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{response}");
        proposals.push((files, next));
    }
    let body = command(
        &host,
        &run,
        "approve-B",
        "ApproveSpec",
        json!({"spec_sha256":proposals[1].1.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body))
            .await?
            .0,
        202
    );
    let (files, index) = &proposals[0];
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    std::fs::write(world.project().join(specindex::PATH), index.bytes())?;
    let before = snapshot(&world)?;
    let body = command(
        &host,
        &run,
        "approve-old-base",
        "ApproveSpec",
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 409, "{response}");
    assert_eq!(response["code"], "SPEC_CONFLICT");
    assert_ne!(
        response["details"]["expected_base"],
        response["details"]["actual_base"]
    );
    assert_eq!(snapshot(&world)?, before);
    assert_eq!(
        world.query("SELECT count(*) FROM change_requests WHERE state='PROPOSED'")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

async fn submit_spec(host: &Host, run: &str, key: &str, kind: &str, index: &Index) -> R<Value> {
    let payload = if kind == "ProposeSpecChange" {
        json!({"spec_sha256":index.sha256(),"reason":"revise acceptance"})
    } else {
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"})
    };
    let body = command(host, run, key, kind, payload).await?;
    let (status, response) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{kind}: {response}");
    Ok(body)
}

fn revise_a(world: &World) -> R<Index> {
    let mut files = sources();
    files[0].bytes = String::from_utf8(files[0].bytes.clone())?
        .replacen("then: result 2", "then: result X", 1)
        .into_bytes();
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    Ok(specindex::generate_index(&files)?)
}

fn tasks(world: &World, run: &str) -> R {
    for (seq, req) in ["A", "B", "C"].into_iter().enumerate() {
        world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
            SELECT 'T{req}',run_id,spec_id,'ACCEPTED','developer',1,{seq} FROM runs WHERE run_id='{run}';
            INSERT INTO task_requirements(task_id,requirement_id,spec_id)
            SELECT 'T{req}','REQ-{req}',spec_id FROM runs WHERE run_id='{run}';"))?;
    }
    Ok(())
}

fn event_payloads(world: &World, kind: &str) -> R<Vec<Value>> {
    world
        .rows(&format!(
            "SELECT payload_json FROM events WHERE kind='{kind}' ORDER BY seq"
        ))?
        .iter()
        .map(|r| {
            Ok(serde_json::from_str(
                r["payload_json"].as_str().ok_or("no payload")?,
            )?)
        })
        .collect()
}

async fn decide(
    world: &World,
    host: &Host,
    run: &str,
    id: &str,
    candidate: Option<String>,
    payload: Value,
) -> R<hoh_core::persistence::Decided> {
    let state = world.rows(&format!("SELECT r.revision,s.sha256,b.role_epoch FROM runs r JOIN specs s ON s.spec_id=r.spec_id JOIN role_bindings b ON b.run_id=r.run_id AND b.role='commander' WHERE r.run_id='{run}'"))?;
    let proposal = hoh_core::decision::Proposal {
        decision_id: id.into(),
        run_id: run.into(),
        expected_revision: state[0]["revision"].as_u64().ok_or("revision")?,
        role_epoch: state[0]["role_epoch"].as_u64().ok_or("epoch")?,
        spec_sha256: state[0]["sha256"].as_str().ok_or("digest")?.into(),
        candidate_id: candidate,
        causation_id: None,
        payload: hoh_core::protocol::decode(&payload)?,
    };
    Ok(host
        .store()
        .decide(proposal, hoh_core::decision::Context::default())
        .await?)
}

#[tokio::test]
async fn test_033_01_change_request_closure_and_approve() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    tasks(&world, &run)?;
    let other = world::start_run(&host, "other", "same project").await?;
    let ended = world::start_run(&host, "ended", "historical run").await?;
    world.exec(&format!(
        "UPDATE runs SET state='COMPLETED' WHERE run_id='{ended}'"
    ))?;
    let before_runs = world.rows("SELECT run_id,spec_id FROM runs ORDER BY run_id")?;
    let before_tasks = world.rows("SELECT * FROM tasks ORDER BY task_id")?;
    let next = revise_a(&world)?;
    let proposal = submit_spec(&host, &run, "closure", "ProposeSpecChange", &next).await?;
    let crs = world.rows("SELECT * FROM change_requests")?;
    assert_eq!(crs.len(), 1);
    assert_eq!(crs[0]["state"], "PROPOSED");
    assert_eq!(
        crs[0]["base_spec_id"],
        before_runs
            .iter()
            .find(|r| r["run_id"] == run)
            .ok_or("run")?["spec_id"]
    );
    let impact: Value = serde_json::from_str(crs[0]["impact_json"].as_str().ok_or("impact")?)?;
    assert_eq!(
        impact,
        json!({"requirements":["REQ-A"],"acceptance":["AC-A-01","AC-A-02"],"tests":["TEST-A-01","TEST-A-02"],"tasks":["TA"]})
    );
    assert_eq!(
        world.rows("SELECT run_id,spec_id FROM runs ORDER BY run_id")?,
        before_runs
    );
    assert_eq!(
        world.rows("SELECT * FROM tasks ORDER BY task_id")?,
        before_tasks
    );
    assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&proposal))
            .await?
            .0,
        202
    );
    assert_eq!(world.query("SELECT count(*) FROM change_requests")?, "1");
    let approval = submit_spec(&host, &run, "closure-approval", "ApproveSpec", &next).await?;
    let current = world.query("SELECT spec_id FROM specs WHERE revision=2")?;
    assert_ne!(first.sha256(), next.sha256());
    assert_eq!(world.query("SELECT state FROM change_requests")?, "APPLIED");
    assert_eq!(
        world.query("SELECT result_spec_id FROM change_requests")?,
        current
    );
    for id in [&run, &other] {
        assert_eq!(
            world.query(&format!("SELECT spec_id FROM runs WHERE run_id='{id}'"))?,
            current
        );
    }
    assert_eq!(world.query(&format!("SELECT s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id WHERE r.run_id='{ended}'"))?, "1");
    let changed_events: Vec<_> = event_payloads(&world, "spec.changed")?
        .into_iter()
        .filter(|e| e["cause"] == "changed")
        .collect();
    assert_eq!(changed_events.len(), 2);
    for e in changed_events {
        assert_eq!(e["from"], 1);
        assert_eq!(e["to"], 2);
        assert_eq!(e["spec_revision"], 2);
        assert_eq!(e["cr_id"], crs[0]["cr_id"]);
    }
    let after = snapshot(&world)?;
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&approval))
            .await?
            .0,
        202
    );
    assert_eq!(snapshot(&world)?, after);
    // Q3 also includes non-terminal runs still on a revision older than this CR's base.
    let older = world::start_run(&host, "older", "older specification").await?;
    world.exec(&format!("UPDATE runs SET spec_id=(SELECT spec_id FROM specs WHERE revision=1) WHERE run_id='{older}'"))?;
    let third = changed(&world)?;
    submit_spec(&host, &run, "third-proposal", "ProposeSpecChange", &third).await?;
    submit_spec(&host, &run, "third-approval", "ApproveSpec", &third).await?;
    for id in [&run, &other, &older] {
        assert_eq!(world.query(&format!("SELECT s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id WHERE r.run_id='{id}'"))?, "3");
    }
    assert_eq!(world.query(&format!("SELECT s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id WHERE r.run_id='{ended}'"))?, "1");
    host.shutdown().await;
    Ok(())
}

/// Persisted accepted candidate fixture; no verification process or live agent runs.
async fn accepted_candidate(world: &World, host: &Host, run: &str) -> R<String> {
    tasks(world, run)?;
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
        return Err("new candidate expected".into());
    };
    world.exec(&format!("UPDATE candidates SET state='ACCEPTED' WHERE candidate_id='{candidate_id}';
        INSERT INTO attempts(attempt_id,task_id,state,role_epoch,host_epoch,config_revision) VALUES('observed','TA','SETTLED',1,1,0);
        INSERT INTO artifacts(artifact_id,sha256,path,bytes,mime) VALUES('observed','{hash}','fixture',0,'application/json');
        INSERT INTO evidence(evidence_id,candidate_id,attempt_id,artifact_id,check_id,kind,outcome,mode,created_at)
        SELECT 'observed-'||a.ac_id,'{candidate_id}','observed','observed',a.check_id,'check','PASS','mock','2026-11-21T00:00:00Z'
        FROM acceptance_criteria a JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}';
        INSERT INTO acceptance_results(ac_id,spec_id,candidate_id,evidence_id,status)
        SELECT a.ac_id,a.spec_id,'{candidate_id}','observed-'||a.ac_id,'PASS' FROM acceptance_criteria a JOIN runs r ON r.spec_id=a.spec_id WHERE r.run_id='{run}';
        UPDATE runs SET state='VERIFYING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}';",hash="0".repeat(64)))?;
    world.staff(run)?;
    Ok(candidate_id)
}

#[tokio::test]
async fn test_033_03_revision_stales_all_acceptance() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let candidate = accepted_candidate(&world, &host, &run).await?;
    // Exercise the real gate on this candidate, then roll back only the completion write so that
    // the same non-terminal Run can receive the approval below. A rejected gate never hits this trigger.
    world.exec("CREATE TRIGGER completion_probe BEFORE UPDATE OF state ON runs WHEN NEW.state='COMPLETED' BEGIN SELECT RAISE(ABORT,'fixture: completion gate passed'); END;")?;
    let before_completion = decide(
        &world,
        &host,
        &run,
        "complete-before-approval",
        Some(candidate.clone()),
        world::transition("COMPLETED"),
    )
    .await;
    assert!(
        before_completion
            .as_ref()
            .is_err_and(|e| e.to_string().contains("fixture: completion gate passed")),
        "revision 1 must pass the real completion gate: {before_completion:?}"
    );
    world.exec("DROP TRIGGER completion_probe;")?;
    assert_eq!(
        world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?,
        "VERIFYING"
    );
    let (_, before) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
    assert_eq!(before["projection"]["acceptance"]["accepted"], 7);
    let old_rows = world.rows("SELECT * FROM acceptance_results ORDER BY ac_id")?;
    let next = revise_a(&world)?;
    assert_ne!(first.sha256(), next.sha256());
    submit_spec(&host, &run, "stale-proposal", "ProposeSpecChange", &next).await?;
    assert_eq!(world.query("SELECT state FROM candidates")?, "ACCEPTED");
    let approval = submit_spec(&host, &run, "stale-approval", "ApproveSpec", &next).await?;
    assert_eq!(
        world.query("SELECT state FROM candidates")?,
        "STALE",
        "a changed specification must stale its accepted candidate"
    );
    let stale = event_payloads(&world, "candidate.stale")?;
    assert_eq!(stale.len(), 1);
    assert_eq!(stale[0]["candidate_id"], candidate);
    assert_eq!(stale[0]["reason"], "spec_changed");
    assert_eq!(
        world.rows("SELECT * FROM acceptance_results ORDER BY ac_id")?,
        old_rows,
        "history is immutable"
    );
    let (_, after) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
    let summary = &after["projection"]["acceptance"];
    assert_eq!(summary["spec_revision"], 2);
    assert_eq!(summary["accepted"], 0);
    assert_eq!(summary["passing"], 0);
    assert_eq!(
        summary["total"], 7,
        "all current ACs count without migrating accepted tasks"
    );
    let (_, page) =
        world::exchange(&host, "GET", &format!("/v1/runs/{run}/acceptance"), None).await?;
    let items = page["items"].as_array().ok_or("acceptance items")?;
    let current: Vec<_> = items.iter().filter(|a| a["spec_revision"] == 2).collect();
    assert_eq!(current.len(), 7);
    assert!(
        current
            .iter()
            .all(|a| a["label"] == "NOT_RUN" && a["candidate_id"].is_null())
    );
    assert!(current.iter().any(|a| a["ac_id"] == "AC-B-01"));
    let history: Vec<_> = items.iter().filter(|a| a["spec_revision"] == 1).collect();
    assert_eq!(history.len(), 7);
    assert!(
        history
            .iter()
            .all(|a| a["label"] == "STALE" && a["candidate_id"] == candidate)
    );
    // Pagination retains every historical AC exactly once.
    let mut cursor = None;
    let mut paged = Vec::new();
    loop {
        let path = match &cursor {
            Some(c) => format!("/v1/runs/{run}/acceptance?limit=2&cursor={c}"),
            None => format!("/v1/runs/{run}/acceptance?limit=2"),
        };
        let (_, p) = world::exchange(&host, "GET", &path, None).await?;
        paged.extend(p["items"].as_array().ok_or("items")?.iter().cloned());
        cursor = p["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
        assert!(paged.len() < 15, "pagination must advance");
    }
    assert_eq!(&paged, items);
    let result = decide(
        &world,
        &host,
        &run,
        "complete-stale",
        Some(candidate.clone()),
        world::transition("COMPLETED"),
    )
    .await?;
    let hoh_core::persistence::Decided::Rejected { reason, detail, .. } = result else {
        return Err(format!("stale completion must be refused: {result:?}").into());
    };
    assert_eq!(
        reason,
        hoh_core::protocol::RejectionReason::EvidenceGateUnmet,
        "{detail}"
    );
    assert!(
        detail.contains(&candidate) && detail.contains("STALE"),
        "{detail}"
    );
    for ac in &current {
        assert!(
            detail.contains(ac["ac_id"].as_str().ok_or("AC id")?),
            "the new revision's AC must be named: {detail}"
        );
    }
    assert!(!detail.contains("no candidate"), "{detail}");
    let unrelated = decide(
        &world,
        &host,
        &run,
        "complete-unknown-candidate",
        Some(format!("CAND-{}", "f".repeat(64))),
        world::transition("COMPLETED"),
    )
    .await?;
    assert!(
        matches!(
            unrelated,
            hoh_core::persistence::Decided::Rejected {
                reason: hoh_core::protocol::RejectionReason::CandidateMismatch,
                ..
            }
        ),
        "{unrelated:?}"
    );

    assert_ne!(
        world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?,
        "COMPLETED"
    );
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&approval))
            .await?
            .0,
        202
    );
    assert_eq!(event_payloads(&world, "candidate.stale")?.len(), 1);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_03_same_revision_stale_candidate_keeps_candidate_mismatch() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let candidate = accepted_candidate(&world, &host, &run).await?;
    world.exec(&format!(
        "UPDATE candidates SET state='STALE' WHERE candidate_id='{candidate}'"
    ))?;
    let result = decide(
        &world,
        &host,
        &run,
        "same-revision-stale",
        Some(candidate),
        world::transition("COMPLETED"),
    )
    .await?;
    assert!(
        matches!(
            result,
            hoh_core::persistence::Decided::Rejected {
                reason: hoh_core::protocol::RejectionReason::CandidateMismatch,
                ..
            }
        ),
        "{result:?}"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_03_stale_states_preserve_judgments_and_rollback_atomically() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let original = accepted_candidate(&world, &host, &run).await?;
    let ended = world::start_run(&host, "history", "finished").await?;
    world.exec(&format!(
        "UPDATE runs SET state='COMPLETED' WHERE run_id='{ended}'"
    ))?;
    for state in [
        "FROZEN",
        "VERIFYING",
        "REJECTED",
        "INVALID",
        "STALE",
        "ACCEPTED",
    ] {
        world.exec(&format!("INSERT INTO candidates SELECT 'C-{state}',run_id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,'{state}',created_at FROM candidates WHERE candidate_id='{original}'"))?;
    }
    world.exec(&format!("INSERT INTO candidates SELECT 'C-history','{ended}',git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256,source_path,'ACCEPTED',created_at FROM candidates WHERE candidate_id='{original}';
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at)
        SELECT project_id,'{judgment}',run_id,'candidate.stale','{{\"candidate_id\":\"C-ACCEPTED\",\"reason\":\"evidence_unrecoverable\"}}',created_at FROM runs WHERE run_id='{run}';",
        judgment=hoh_core::ids::child_id("C-ACCEPTED","stale")))?;
    let next = revise_a(&world)?;
    submit_spec(&host, &run, "states-propose", "ProposeSpecChange", &next).await?;
    let body = command(
        &host,
        &run,
        "states-approve",
        "ApproveSpec",
        json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let before = snapshot(&world)?;
    let candidates = world.rows("SELECT * FROM candidates ORDER BY candidate_id")?;
    world.exec("CREATE TRIGGER refuse_stale BEFORE INSERT ON events WHEN NEW.kind='candidate.stale' BEGIN SELECT RAISE(ABORT,'fixture refuses stale event'); END;")?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 409, "the injected constraint refusal: {response}");
    assert_eq!(
        snapshot(&world)?,
        before,
        "approval must roll back every derived row and run change"
    );
    assert_eq!(
        world.rows("SELECT * FROM candidates ORDER BY candidate_id")?,
        candidates
    );
    world.exec("DROP TRIGGER refuse_stale;")?;
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body))
            .await?
            .0,
        202
    );
    for state in ["FROZEN", "VERIFYING", "ACCEPTED", "STALE"] {
        assert_eq!(
            world.query(&format!(
                "SELECT state FROM candidates WHERE candidate_id='C-{state}'"
            ))?,
            "STALE"
        );
    }
    for state in ["REJECTED", "INVALID"] {
        assert_eq!(
            world.query(&format!(
                "SELECT state FROM candidates WHERE candidate_id='C-{state}'"
            ))?,
            state
        );
    }
    assert_eq!(
        world.query("SELECT state FROM candidates WHERE candidate_id='C-history'")?,
        "ACCEPTED"
    );
    let judgments = event_payloads(&world, "candidate.stale")?;
    assert_eq!(judgments.len(), 4);
    assert_eq!(
        judgments
            .iter()
            .find(|e| e["candidate_id"] == "C-ACCEPTED")
            .ok_or("prior judgment")?["reason"],
        "evidence_unrecoverable"
    );
    assert_eq!(
        judgments
            .iter()
            .filter(|e| e["reason"] == "spec_changed")
            .count(),
        3
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_04_approval_refuses_legacy_cr_with_base_digest() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    // A CR persisted by the old implementation must not create a duplicate approved revision.
    world.exec("INSERT INTO change_requests(cr_id,project_id,base_spec_id,state,reason,impact_json,created_at)
        SELECT 'legacy-empty',project_id,spec_id,'PROPOSED','legacy repair',json_object('requirements',json('[]'),'acceptance',json('[]'),'tests',json('[]'),'tasks',json('[]')),'2026-11-21T00:00:00Z' FROM specs;
        INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at)
        SELECT 'legacy-proposal',s.project_id,r.run_id,'spec.proposed',
        json_object('cr_id','legacy-empty','spec_sha256',s.sha256),'2026-11-21T00:00:00Z'
        FROM specs s JOIN runs r ON r.spec_id=s.spec_id;")?;
    let before = snapshot(&world)?;
    let body = command(
        &host,
        &run,
        "approve-legacy",
        "ApproveSpec",
        json!({"spec_sha256":first.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let (status, response) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(
        status, 409,
        "a CR whose result equals its base must be refused: {response}"
    );
    assert_eq!(response["code"], "SPEC_CONFLICT");
    assert_eq!(snapshot(&world)?, before);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_034_04_known_generated_digests_are_not_manual_drift() -> R {
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let mut previous = first.clone();
    for n in 1..=3 {
        let mut files = sources();
        files[0].bytes.extend(std::iter::repeat_n(b'\n', n));
        std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
        let next = specindex::generate_index(&files)?;
        // The first is approved; the second is only proposed; the third is an older approved revision.
        if n == 3 {
            std::fs::write(world.project().join(specindex::PATH), first.bytes())?;
        }
        let body = command(
            &host,
            &run,
            &format!("known-{n}"),
            "ProposeSpecChange",
            json!({"spec_sha256":next.sha256(),"reason":"normal Markdown edit"}),
        )
        .await?;
        let (status, response) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{response}");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='spec.generated_drift'")?,
            "0",
            "an approved or proposed generated index is not a manual edit (case {n})"
        );
        assert_eq!(
            std::fs::read(world.project().join(specindex::PATH))?,
            next.bytes()
        );
        assert_ne!(next.sha256(), previous.sha256());
        if n == 2 {
            let body = command(
                &host,
                &run,
                "approve-known",
                "ApproveSpec",
                json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
            )
            .await?;
            let (status, response) =
                world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
            assert_eq!(status, 202, "{response}");
        }
        previous = next;
    }
    host.shutdown().await;
    Ok(())
}

/// REQ-031 behaviour 6 (docs/adr/ADR-035.md decision 1) on the product path: an approval writes its revision's open
/// questions once (`spec.open_questions`), and the run's projection lists them with their id and blocking mark, apart
/// from the answerable `questions`.
#[tokio::test]
async fn an_approved_revision_lists_its_open_questions_in_the_run_projection() -> R {
    let world = World::new()?;
    let questions = "# 开放问题\n- Q-color 主题颜色\n阻塞：否\n- Q-db 存储引擎\n阻塞：是\n";
    let files = vec![Source {
        path: "docs/spec/a.md".into(),
        bytes: format!("{PREFIX}{}", req("A", 1))
            .replace("# 开放问题\n无\n", questions)
            .into_bytes(),
    }];
    let index = specindex::generate_index(&files)?;
    std::fs::create_dir_all(world.project().join("docs/spec"))?;
    std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
    std::fs::write(world.project().join(specindex::PATH), index.bytes())?;
    std::fs::write(world.project().join(".gitignore"), ".hoh/\n")?;
    world.git(&["init", "-q"])?;
    world.git(&["add", "."])?;
    world.git(&["commit", "-qm", "specification"])?;
    let host = world.start_plain_host(None).await?;
    let outcome = async {
        let run = world::start_run(&host, "start", "open questions").await?;
        let body = command(
            &host,
            &run,
            "approve",
            "ApproveSpec",
            json!({"spec_sha256": index.sha256(), "decision":"APPROVE_ONLY"}),
        )
        .await?;
        let (status, accepted) =
            world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{accepted}");
        let events = world.rows(
            "SELECT event_id, payload_json FROM events WHERE kind = 'spec.open_questions'",
        )?;
        assert_eq!(events.len(), 1, "{events:?}");
        let spec_id = world.query("SELECT spec_id FROM specs")?;
        let payload: Value =
            serde_json::from_str(events[0]["payload_json"].as_str().ok_or("payload")?)?;
        assert_eq!(payload["spec_id"], json!(spec_id));
        assert_eq!(payload["spec_revision"], 1);
        assert_eq!(
            payload["questions"],
            json!([
                {"id": "Q-color", "text": "主题颜色", "blocking": false, "path": "docs/spec/a.md", "line": 10},
                {"id": "Q-db", "text": "存储引擎", "blocking": true, "path": "docs/spec/a.md", "line": 12}
            ])
        );
        let (_, got) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
        assert_eq!(
            got["projection"]["open_questions"],
            json!([
                {"id": "Q-color", "text": "主题颜色", "blocking": false, "spec_revision": 1},
                {"id": "Q-db", "text": "存储引擎", "blocking": true, "spec_revision": 1}
            ]),
            "{got}"
        );
        // Not answerable questions: none of them is listed among `questions`.
        assert!(
            got["projection"]["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .all(|q| !q["question_id"].as_str().unwrap_or_default().contains("Q-")),
            "{got}"
        );
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    host.shutdown().await;
    outcome
}
