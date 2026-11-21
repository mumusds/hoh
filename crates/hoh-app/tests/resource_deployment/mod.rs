//! AC-047-03's adopted single-binary fixture: v1 seeded by the product API; the current binary is v2.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
use crate::world::{self, World};
use hoh_core::{
    lifecycle::AttemptState,
    persistence::{AttemptFacts, AttemptMove, NewAttempt, Options, Store},
    resource_bundle::Bundle,
};
use serde_json::Value;
use std::{sync::Arc, time::Duration};
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[tokio::test]
async fn test_047_03_bundle_pinned_until_checkpoint() -> R {
    let world = World::named("m3t03-bundle")?;
    world.commander(&world::Commander {
        tasks: Vec::new(),
        ..Default::default()
    })?;
    world.register()?;
    world.commit_project(&world::fixture_spec())?;
    world.init_store().await?;
    let spec = world.seed_spec()?;
    let project = world.project();
    let mut old = Bundle::current();
    old.files.insert(
        "resources/prompts/developer.md".to_owned(),
        "OLD BUILTIN DEVELOPER\n".to_owned(),
    );
    hoh_core::prompt_files::save_override_based_on(
        &project,
        ".hoh/prompts/developer.md",
        "PROJECT OVERRIDE\n",
        "developer",
        &old,
    )?;
    let mut config = world.host_config()?;
    config.runner = None;
    config.store.resource_bundle = Arc::new(old.clone());
    let host = world.start_host_with(config).await?;
    let seeded: R<String> = async {
        let run=world::start_run(&host,"pinned-v1","resource upgrade").await?;
        world.exec(&format!("UPDATE runs SET state='RUNNING' WHERE run_id='{run}';
            INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('T-hold','{run}','{spec}','LEASED','developer',1);"))?;
        let store=Store::open(&world.db(),Options {resource_bundle:Arc::new(old.clone()),wal_probe:Some(world::probe()),..Options::new(hoh_core::spawn::Direct::shared())})?;
        store.create_attempt(NewAttempt {attempt_id:"A-hold".to_owned(),task_id:"T-hold".to_owned(),session_id:None,role_epoch:1,host_epoch:1,config_revision:0,agent:Some("claude".to_owned())}).await?;
        Ok(run)
    }.await;
    host.shutdown().await;
    let run = seeded?;
    // The v2 host recovers a PREPARED attempt. That durable barrier keeps this role on v1 while another attempt runs.
    let host = world.start_host().await?;
    let outcome: R = async {
        let add=|task:&str| world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq,cost_estimate) VALUES ('{task}','{run}','{spec}','READY','developer',(SELECT max(created_seq)+1 FROM tasks),1);"));
        add("T-old")?;
        world.until("SELECT count(*) FROM events WHERE event_id='T-old/attempt-1/context.packed'","1",Duration::from_secs(45)).await?;
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-old/attempt-1'")?,old.version());
        let packed:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-old/attempt-1/context.packed'")?)?;
        let first=&packed["override_chain"][0];
        assert_eq!(first["layer"],"builtin");
        assert_eq!(first["sha256"],crate::installed::sha256(b"OLD BUILTIN DEVELOPER\n"));
        let drift:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-old/attempt-1/resource.drift'")?)?;
        let report:Value=serde_json::from_slice(&hoh_core::objects::get(&project,drift["reports"][0]["sha256"].as_str().ok_or("no report")?)?)?;
        assert_eq!(report["diff"],hoh_core::prompt::three_way_diff("OLD BUILTIN DEVELOPER\n",hoh_core::config::builtin_contract("developer").unwrap(),"PROJECT OVERRIDE\n"));
        world.until("SELECT state FROM attempts WHERE attempt_id='T-old/attempt-1'","SETTLED",Duration::from_secs(45)).await?;
        let store=Store::open(&world.db(),Options {wal_probe:Some(world::probe()),..Options::new(hoh_core::spawn::Direct::shared())})?;
        store.move_attempt(AttemptMove {attempt_id:"A-hold".to_owned(),from:AttemptState::Prepared,to:AttemptState::Cancelled,facts:AttemptFacts::default()}).await?;
        // Observe the actual checkpoint event, not elapsed time, before adding the next attempt.
        world.until(&format!("SELECT json_extract(payload_json,'$.resource_bundle_version') FROM events WHERE kind='resource.pinned' AND run_id='{run}' AND json_extract(payload_json,'$.role')='developer' ORDER BY seq DESC LIMIT 1"),&Bundle::current().version(),Duration::from_secs(20)).await?;
        add("T-new")?;
        world.until("SELECT count(*) FROM events WHERE event_id='T-new/attempt-1/context.packed'","1",Duration::from_secs(45)).await?;
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-new/attempt-1'")?,Bundle::current().version());
        let packed:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-new/attempt-1/context.packed'")?)?;
        assert_eq!(packed["override_chain"][0]["sha256"],crate::installed::sha256(hoh_core::config::builtin_contract("developer").unwrap().as_bytes()));
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-old/attempt-1'")?,old.version());
        Ok(())
    }.await;
    host.shutdown().await;
    outcome
}
