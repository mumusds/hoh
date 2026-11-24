//! AC-047-03's single-binary fixture: v1 seeded by the product API; the current binary is v2. It covers what the real
//! upgrade (`resource_upgrade`, same check) cannot reach: a v2 host running an attempt still pinned to v1 (a PREPARED
//! barrier holds the checkpoint).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
use crate::world::{self, World};
use hoh_core::{
    lifecycle::AttemptState,
    persistence::{AttemptFacts, AttemptMove, NewAttempt, Options, SessionGrant, Store},
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
    old.retain_files(|path, _| !path.starts_with("resources/skills/"));
    old.insert_file(
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
        // Grant v1's session from the isolated agent's actual probe, as staffing does. A synthetic empty session
        // would be stale on restart and correctly refused as COMPAT_UNVERIFIED before prompt packing.
        let registry=world.registry();
        assert!(registry.open_manifest(&project).is_none());
        registry.look(hoh_agents::compat::Agent::Claude)?;
        let Some(hoh_core::staffing::Fact::Usable(agent))=registry.fact(hoh_agents::compat::Agent::Claude) else {
            return Err("fixture Claude was not usable".into());
        };
        store.bind_session(SessionGrant {run_id:run.clone(),role:"developer".to_owned(),role_epoch:1,
            agent:"claude".to_owned(),agent_version:agent.version,capabilities:agent.session,identity:agent.identity}).await?;
        world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,created_seq,cost_estimate)
            VALUES ('T-old','{run}','{spec}','READY','developer',2,1), ('T-new','{run}','{spec}','READY','developer',3,1);"))?;
        let context=hoh_core::decision::Context {
            required_capabilities: [("developer".to_owned(),Default::default())].into(),
            ..Default::default()
        };
        let dispatched=store.schedule_round(&run,context).await?.dispatched.ok_or("v1 did not dispatch")?;
        assert_eq!(dispatched.task_id,"T-old");
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-old/attempt-1'")?,old.version());
        Ok(run)
    }.await;
    host.shutdown().await;
    let run = seeded?;
    // Both old attempts were prepared by v1. The v2 host finishes the queued dispatch, but must not lease T-new
    // while either old attempt holds the checkpoint (CR-026 C14 / M3G-50).
    let host = world.start_host().await?;
    let outcome: R = async {
        world.until("SELECT count(*) FROM events WHERE event_id='T-old/attempt-1/context.packed'","1",Duration::from_secs(45)).await?;
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-old/attempt-1'")?,old.version());
        let packed:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-old/attempt-1/context.packed'")?)?;
        assert_eq!(packed["skills"], serde_json::json!([]));
        assert_eq!(packed["skill_registry"]["builtin"], "missing");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE event_id='T-old/attempt-1/context.packed'")?, "1");
        let first=&packed["override_chain"][0];
        assert_eq!(first["layer"],"builtin");
        assert_eq!(first["sha256"],crate::installed::sha256(b"OLD BUILTIN DEVELOPER\n"));
        let drift:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-old/attempt-1/resource.drift'")?)?;
        let report:Value=serde_json::from_slice(&hoh_core::objects::get(&project,drift["reports"][0]["sha256"].as_str().ok_or("no report")?)?)?;
        crate::resource_upgrade::assert_three_way_diff(&report["diff"],"OLD BUILTIN DEVELOPER\n",hoh_core::config::builtin_contract("developer").unwrap(),"PROJECT OVERRIDE\n")?;
        world.until("SELECT state FROM attempts WHERE attempt_id='T-old/attempt-1'","SETTLED",Duration::from_secs(45)).await?;
        assert_eq!(world.query("SELECT count(*) FROM attempts WHERE task_id='T-new'")?,"0");
        assert_eq!(world.query("SELECT state FROM tasks WHERE task_id='T-new'")?,"READY");
        let store=Store::open(&world.db(),Options {wal_probe:Some(world::probe()),..Options::new(hoh_core::spawn::Direct::shared())})?;
        store.move_attempt(AttemptMove {attempt_id:"A-hold".to_owned(),from:AttemptState::Prepared,to:AttemptState::Cancelled,facts:AttemptFacts::default()}).await?;
        // The already-ready task resumes after the checkpoint without another task or user action.
        world.until(&format!("SELECT json_extract(payload_json,'$.resource_bundle_version') FROM events WHERE kind='resource.pinned' AND run_id='{run}' AND json_extract(payload_json,'$.role')='developer' ORDER BY seq DESC LIMIT 1"),&Bundle::current().version(),Duration::from_secs(20)).await?;
        world.until("SELECT count(*) FROM events WHERE event_id='T-new/attempt-1/context.packed'","1",Duration::from_secs(45)).await?;
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-new/attempt-1'")?,Bundle::current().version());
        let packed:Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE event_id='T-new/attempt-1/context.packed'")?)?;
        assert_eq!(packed["override_chain"][0]["sha256"],crate::installed::sha256(hoh_core::config::builtin_contract("developer").unwrap().as_bytes()));
        assert_eq!(world.query("SELECT resource_bundle_version FROM attempts WHERE attempt_id='T-old/attempt-1'")?,old.version());
        Ok(())
    }.await;
    if outcome.is_err() {
        eprintln!("resource deployment attempts: {:?}", world.query("SELECT json_group_array(json_object('id',attempt_id,'state',state,'reason',failure_reason)) FROM attempts"));
        eprintln!("resource deployment refusals: {:?}", world.query("SELECT json_group_array(json_object('kind',kind,'payload',json(payload_json))) FROM events WHERE kind='attempt.start_refused'"));
    }
    host.shutdown().await;
    outcome
}
