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

async fn input(
    host: &hoh_app::host::Host,
    world: &world::World,
    run: &str,
    key: &str,
    kind: &str,
    payload: serde_json::Value,
) -> R<serde_json::Value> {
    for _ in 0..20 {
        let revision: i64 = world
            .query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?
            .parse()?;
        let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,"expected_revision":revision,"kind":kind,"run_id":run,"payload":payload});
        let (status, reply) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
        if status == 409 && reply["code"] == "STALE_REVISION" {
            continue;
        }
        assert_eq!(status, 202, "{reply}");
        return Ok(reply);
    }
    Err("run kept changing".into())
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_02_first_draft_completes_then_all_three_user_entrances_approve() -> R {
    for kind in ["ApproveSpec", "AnswerQuestion", "SendInput"] {
        let world = world::World::named("m3t04-draft-approval")?;
        world.configure(&[], "")?;
        let host = world
            .team_host(world::Team::drafting(world.project()), None)
            .await?;
        let result: R=async{
   let run=world::start_run(&host,"start","规格起草").await?;world.staff(&run)?;
   let second=world::start_run(&host,"second","等待规格").await?;world.staff(&second)?;
   if kind == "SendInput" {
    let mut cli=world.command(&["-p","draft","--resume",&run]);
    let output=tokio::task::spawn_blocking(move ||cli.output()).await??;
    assert_eq!(output.status.code(),Some(3),"{}",String::from_utf8_lossy(&output.stderr));
   } else {
   input(&host,&world,&run,"draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   }
   world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(20)).await?;
   assert_eq!(world.query("SELECT count(*) FROM candidates")?,"0");
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");
   assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,"IMPLEMENTED");
   assert_eq!(world.query(&format!("SELECT state||'/'||resume_state FROM runs WHERE run_id='{run}'"))?,"BLOCKED/SPECIFYING");
   let q:serde_json::Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval' ORDER BY seq DESC LIMIT 1")?)?;
   let payload=match kind{
    "ApproveSpec"=>json!({"spec_sha256":q["spec_sha256"],"decision":"APPROVE_ONLY"}),
    "AnswerQuestion"=>json!({"question_id":q["question_id"],"choice_id":"approve","answer":"approve"}),
    _=>json!({"text":"approve","target_role":null,"input_kind":"REPLY"}),
   };
   if kind == "SendInput" {
    let mut cli=world.command(&["-p","approve","--resume",&run,"--detach"]);
    let output=tokio::task::spawn_blocking(move ||cli.output()).await??;
    assert_eq!(output.status.code(),Some(0),"{}",String::from_utf8_lossy(&output.stderr));
   } else { input(&host,&world,&run,"approval",kind,payload).await?; }
   assert_eq!(world.query("SELECT count(*) FROM specs WHERE revision=1")?,"1");
   assert_eq!(world.query("SELECT state||'/'||coalesce(spec_id,'NULL') FROM tasks WHERE kind='spec_draft'")?,"ACCEPTED/NULL");
   assert_eq!(world.query("SELECT count(*) FROM commands WHERE kind='ApproveSpec'")?,"1");
   input(&host,&world,&second,"adopt-draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,"1");
   assert_eq!(world.query(&format!("SELECT count(*) FROM runs WHERE run_id='{second}' AND spec_id IS NOT NULL"))?,"1");
   assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.drafting'")?,"1");
   Ok(())
  }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}

async fn marked_request(
    host: &hoh_app::host::Host,
    body: &serde_json::Value,
) -> R<(u16, serde_json::Value)> {
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        host.port,
        host.token,
        body.to_string().len(),
        body
    );
    let port = host.port.to_string();
    let output = tokio::task::spawn_blocking(move || {
        world::approval::python(
            r#"
import socket,sys
s=socket.create_connection(('127.0.0.1',int(sys.argv[1])),15)
s.sendall(sys.argv[2].encode())
while True:
 data=s.recv(65536)
 if not data: break
 sys.stdout.buffer.write(data)
"#,
            &[("HOH_ATTEMPT_ID", "another-project/attempt")],
        )
        .args([port, request])
        .output()
    })
    .await??;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = String::from_utf8(output.stdout)?;
    let (header, body) = response.split_once("\r\n\r\n").ok_or("response")?;
    Ok((
        header.split_whitespace().nth(1).ok_or("status")?.parse()?,
        serde_json::from_str(body)?,
    ))
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_008_05_answer_and_reply_refuse_attempt_origin_before_application_and_replay() -> R {
    for kind in ["AnswerQuestion", "SendInput"] {
        let world = world::World::named("m3t04-draft-origin")?;
        world.configure(&[], "")?;
        let host = world
            .team_host(world::Team::drafting(world.project()), None)
            .await?;
        let result:R=async{
   let run=world::start_run(&host,"start","draft authority").await?;world.staff(&run)?;
   input(&host,&world,&run,"draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
   world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",Duration::from_secs(20)).await?;
   let q:serde_json::Value=serde_json::from_str(&world.query("SELECT payload_json FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval'")?)?;
   let payload=if kind=="AnswerQuestion"{json!({"question_id":q["question_id"],"choice_id":"approve","answer":"approve"})}else{json!({"text":"approve","target_role":null,"input_kind":"REPLY"})};
   let revision:i64=world.query("SELECT revision FROM runs")?.parse()?;
   let body=json!({"schema_version":"1.0","command_id":"approve","idempotency_key":"approve","run_id":run,"expected_revision":revision,"kind":kind,"payload":payload});
   for _ in 0..2{
    let (status,rejected)=marked_request(&host,&body).await?;
    assert_eq!(status,403,"{rejected}");assert_eq!(rejected["code"],"SPEC_APPROVAL_NOT_USER");assert_eq!(rejected["details"]["reason"],"marker");
    assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");assert_eq!(world.query("SELECT count(*) FROM commands WHERE command_id='approve'")?,"0");
    assert_eq!(world.query("SELECT revision FROM runs")?,revision.to_string());
   }
   assert_eq!(world::exchange(&host,"POST","/v1/commands",Some(&body)).await?.0,202);
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");
   assert_eq!(marked_request(&host,&body).await?.0,403,"origin guard precedes the user's accepted replay");
   assert_eq!(world.query("SELECT count(*) FROM specs")?,"1");Ok(())
  }.await;
        host.shutdown().await;
        result?;
    }
    Ok(())
}
