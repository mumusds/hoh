//! Product host with scripted agents; these diagnostics are not acceptance or live-agent evidence.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
mod pty;
mod world;
use pty::R;
use serde_json::json;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_first_draft_uses_runtime_writer_and_git_init_effect() -> R {
    let world = world::World::named("m3t04-drafting")?;
    world.configure(&[], "")?;
    let team = world::Team::working(|_| Vec::new(), Duration::from_secs(60));
    let host = world.team_host(team, None).await?;
    let outcome=async {
        let run=world::start_run(&host,"draft-start","起草规格").await?;
        world.staff(&run)?;
        assert_eq!(world.query("SELECT count(*) FROM tasks")?,"0");
        let revision:i64=world.query("SELECT revision FROM runs")?.parse()?;
        let body=json!({"schema_version":"1.0","command_id":"draft","idempotency_key":"draft",
            "expected_revision":revision,"kind":"SendInput","run_id":run,
            "payload":{"text":"draft","target_role":null,"input_kind":"REPLY"}});
        let (status,result)=world::exchange(&host,"POST","/v1/commands",Some(&body)).await?;
        assert_eq!(status,202,"{result}");
        world.until("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft' AND a.state='RUNNING'","1",Duration::from_secs(15)).await?;
        assert_eq!(world.query("SELECT state||'/'||mode||'/'||coalesce(spec_id,'NULL') FROM runs")?,"SPECIFYING/PLAN/NULL");
        assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft' AND spec_id IS NULL AND role='cto' AND writer=1")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM task_requirements")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.drafting'")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='schedule.round'")?,"0");
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state='COMMITTED'")?,"1");
        assert_eq!(world.query("SELECT count(*) FROM leases WHERE kind='writer'")?,"1");
        assert!(world.project().join(".git").is_dir());
        assert!(!std::fs::read_to_string(world.project().join(".git/config"))?.contains("[remote"));
        let phases=world.rows("SELECT json_extract(payload_json,'$.to') AS state FROM events WHERE kind='run.transitioned'")?;
        assert_eq!(phases,vec![json!({"state":"DISCOVERING"}),json!({"state":"SPECIFYING"})]);
        let second=world::start_run(&host,"draft-second","另一起草").await?;
        world.staff(&second)?;
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{second}'"))?.parse()?;
        let request=json!({"schema_version":"1.0","command_id":"draft-second-answer","idempotency_key":"draft-second-answer",
            "expected_revision":revision,"kind":"SendInput","run_id":second,
            "payload":{"text":"draft","target_role":null,"input_kind":"REPLY"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&request)).await?.0,202);
        world.until("SELECT count(*) FROM tasks WHERE kind='spec_draft'","2",Duration::from_secs(15)).await?;
        assert_eq!(world.query(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{second}'"))?,"0","one project writer");
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse()?;
        let cancel=json!({"schema_version":"1.0","command_id":"cancel","idempotency_key":"cancel",
            "expected_revision":revision,"kind":"CancelRun","run_id":run,"payload":{"reason":"test"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&cancel)).await?.0,202);
        world.until(&format!("SELECT state FROM runs WHERE run_id='{run}'"),"CANCELLED",Duration::from_secs(15)).await?;
        assert_eq!(world.query(&format!("SELECT state FROM tasks WHERE kind='spec_draft' AND run_id='{run}'"))?,"CANCELLED");
        assert!(world.project().join(".git").exists(),"cancellation preserves the workspace");
        world.until(&format!("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.run_id='{second}' AND a.state='RUNNING'"),"1",Duration::from_secs(15)).await?;
        assert_eq!(world.query("SELECT count(*) FROM effects WHERE state='COMMITTED'")?,"1","existing repository is not initialized again");
        let revision:i64=world.query(&format!("SELECT revision FROM runs WHERE run_id='{second}'"))?.parse()?;
        let cancel=json!({"schema_version":"1.0","command_id":"cancel-second","idempotency_key":"cancel-second",
            "expected_revision":revision,"kind":"CancelRun","run_id":second,"payload":{"reason":"test"}});
        assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&cancel)).await?.0,202);
        world.until(&format!("SELECT state FROM runs WHERE run_id='{second}'"),"CANCELLED",Duration::from_secs(15)).await?;
        Ok(())
    }.await;
    host.shutdown().await;
    outcome
}
