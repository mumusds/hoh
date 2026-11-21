//! REQ-034 through the product host and freeze driver; no live agent runs.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

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
    for (row, scenario) in criteria.iter().zip(
        index.value()["scenarios"]
            .as_array()
            .ok_or("no scenarios")?,
    ) {
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
        "1"
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
