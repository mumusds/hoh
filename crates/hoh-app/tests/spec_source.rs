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
