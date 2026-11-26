//! Included by product integration; protocol fakes drive the actual runner and writer, never live agents.
use super::*;
const SECRET: &str = "M4T09_NATIVE_FAULT_SECRET_c17d40b2";
fn public_fixture(mode: &str) -> R<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../hoh-agents/src/compat/fixtures/{mode}/tool.jsonl"
    ));
    let mut out = String::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let mut frame: Value = serde_json::from_str(line)?;
        let output = format!("Authorization: Bearer {SECRET}\nhoh\n").repeat(2000);
        if frame["type"] == "user"
            && let Some(parts) = frame["message"]["content"].as_array_mut()
        {
            for part in parts {
                if part["type"] == "tool_result" {
                    part["content"] = json!(output);
                }
            }
        }
        if frame["method"] == "item/completed"
            && frame["params"]["item"]["type"] == "commandExecution"
        {
            frame["params"]["item"]["aggregatedOutput"] = json!(output);
        }
        if frame["type"] == "item.completed" && frame["item"]["type"] == "command_execution" {
            frame["item"]["aggregated_output"] = json!(output);
        }
        if frame["type"] == "tool_execution_end" {
            frame["result"]["content"] = json!([{"type":"text","text":output}]);
        }
        frame["env"] = json!({"TOKEN":SECRET});
        frame["authorization"] = json!(SECRET);
        frame["system_prompt"] = json!(SECRET);
        frame["thinking"] = json!(SECRET);
        out.push_str(&format!("{frame}\n"));
    }
    Ok(out)
}
fn install_public_fixtures(world: &World) -> R {
    let claude = public_fixture("claude")?;
    let file = world.bin().join("claude.d/public.jsonl");
    std::fs::write(file, &claude)?;
    world.script(
        "claude",
        &world
            .claude_body(None)?
            .replace("$f/success/stdout.jsonl", "$d/public.jsonl"),
    )?;
    std::fs::write(
        world.bin().join("codex.d/frames.jsonl"),
        public_fixture("codex")?,
    )?;
    std::fs::write(
        world.bin().join("codex.d/requests.json"),
        json!({"0":"initialize","1":"thread/start","2":"turn/start"}).to_string(),
    )?;
    let exec = world.bin().join("codex.d/public-exec.jsonl");
    std::fs::write(&exec, public_fixture("codex-exec")?)?;
    let codex = std::fs::read_to_string(world.bin().join("codex"))?
        .replace("$f/exec/stdout.jsonl", "$d/public-exec.jsonl");
    std::fs::write(world.bin().join("codex"), codex)?;
    std::fs::write(world.bin().join("pi.d/frames.jsonl"), public_fixture("pi")?)?;
    std::fs::write(
        world.bin().join("pi.d/commands.json"),
        json!({"hoh-state-1":"get_state","hoh-prompt-2":"prompt"}).to_string(),
    )?;
    Ok(())
}
#[tokio::test]
async fn test_011_05_native_recordings_reach_public_storage_through_product_runner() -> R {
    for mode in ["claude", "codex", "codex-exec", "pi"] {
        eprintln!("public native mode: {mode}");
        let world = World::new()?;
        world.claude(None)?;
        world.codex("gpt-6-astra")?;
        world.pi()?;
        install_public_fixtures(&world)?;
        world.bind_installed()?;
        let registry = world.registry();
        if mode == "codex-exec" {
            let ready = registry.look(hoh_agents::compat::Agent::Codex)?;
            registry.exec_degraded(hoh_agents::compat::Agent::Codex, ready.entry.identity());
        }
        let host = start_when_free(world.config_with(
            hoh_core::config::Config::default(),
            registry,
            None,
            None,
        ))
        .await?;
        let outcome=async {
            let project=world.project();
            let role=match mode {"claude"=>"developer","codex"=>"reviewer","codex-exec"=>"developer",_=>"qa"};
            if mode=="codex-exec"{configure(&host,"exec-binding",team_revision(&project)?,json!({"developer":{"agent":"codex"},"commander":{"agent":"claude"}})).await?;}
            let run=started(&host,&project,"public").await?;
            ready(&project,&run,&[("PUBLIC",role)])?;
            if let Err(e)=until(&project,"SELECT state FROM attempts WHERE attempt_id='PUBLIC/attempt-1'","SETTLED",Duration::from_secs(30)).await {return Err(format!("{mode}: {e}; task={}; binding={}",query(&project,"SELECT state||'/'||coalesce(blocked_reason,'') FROM tasks WHERE task_id='PUBLIC'")?,query(&project,&format!("SELECT state||'/'||coalesce(blocked_reason,'') FROM role_bindings WHERE run_id='{run}' AND role='{role}'"))?).into());}
            let raw=query(&project,"SELECT json_group_array(json(payload_json)) FROM events WHERE kind LIKE 'public.%' AND json_extract(payload_json,'$.attempt_id')='PUBLIC/attempt-1'")?;
            let messages:Vec<Value>=serde_json::from_str(&raw)?;
            assert!(!messages.is_empty(),"{mode}: real runner must persist native public messages");
            assert!(!raw.contains(SECRET),"{mode}: forbidden native fields must not reach public storage");
            assert!(messages.iter().any(|p|p["text"]=="HOH_SUITE_OK"),"{mode}: original assistant text");
            for p in &messages {assert_eq!(p["role"],role);assert_eq!(p["role_epoch"],1);assert!(p["session_id"].is_string());assert_eq!(p["config_revision"],if mode=="codex-exec"{2}else{1});}
            let start:Value=serde_json::from_str(&query(&project,"SELECT payload_json FROM events WHERE kind='public.tool_started' AND json_extract(payload_json,'$.attempt_id')='PUBLIC/attempt-1' ORDER BY seq LIMIT 1")?)?;
            let end:Value=serde_json::from_str(&query(&project,"SELECT payload_json FROM events WHERE kind='public.tool_completed' AND json_extract(payload_json,'$.attempt_id')='PUBLIC/attempt-1' ORDER BY seq LIMIT 1")?)?;
            assert_eq!(start["block_id"],end["block_id"]);assert!(end.get("text").is_none(),"large tool body is an object");assert_eq!(end["redacted"],true);
            let refs=end["refs"].as_array().ok_or("public result references")?;let mut expanded=Vec::new();for reference in refs {expanded.extend(std::fs::read(project.join(reference["path"].as_str().ok_or("public object path")?))?);}
            let expanded=String::from_utf8(expanded)?;assert!(expanded.contains("hoh"));assert!(!expanded.contains(SECRET),"{mode}: public objects exclude credentials");
            assert!(messages.iter().any(|p|p["usage"]["input_tokens"].is_number()),"{mode}: explicit usage");
            assert_ne!(query(&project,"SELECT count(*) FROM public_native_parts WHERE attempt_id='PUBLIC/attempt-1'")?,"0");
            R::Ok(())
        }.await;
        host.shutdown().await;
        world.stop_fakes()?;
        outcome?;
    }
    Ok(())
}

#[derive(Debug, Default)]
struct PublicBusy {
    busy: std::sync::atomic::AtomicBool,
    public: std::sync::atomic::AtomicBool,
    seen: std::sync::atomic::AtomicUsize,
}
impl hoh_core::persistence::FaultHooks for PublicBusy {
    fn before_job(&self, job: &'static str) {
        self.public
            .store(job == "Public", std::sync::atomic::Ordering::SeqCst);
    }
    fn before_commit(&self) -> Result<(), hoh_core::persistence::PersistError> {
        if self.public.load(std::sync::atomic::Ordering::SeqCst)
            && self.busy.load(std::sync::atomic::Ordering::SeqCst)
        {
            self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(hoh_core::persistence::PersistError::Busy(
                "public stream fault injection".into(),
            ))
        } else {
            Ok(())
        }
    }
}
#[tokio::test]
async fn test_011_05_db_busy_pauses_inactivity_and_cancel_still_reaches_process() -> R {
    use std::sync::{Arc, atomic::Ordering};
    for cancel_while_busy in [true, false] {
        let world = World::new()?;
        let body = world.claude_body(None)?;
        let script = world.bin().join("claude.d/public-busy.py");
        let emitted = world.bin().join("claude.d/public-emitted");
        std::fs::write(
            &script,
            r#"import json,signal,sys
signal.signal(signal.SIGINT,lambda *_:sys.exit(0))
signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
print(json.dumps({'type':'system','subtype':'init','session_id':'public-busy','model':'fixture'}),flush=True)
for n in range(300):
 print(json.dumps({'type':'assistant','uuid':str(n),'message':{'id':str(n),'content':[{'type':'text','text':'x'*4096}]}}),flush=True)
open(sys.argv[1],'w').close()
signal.pause()
"#,
        )?;
        let body = body.replace(
            "case \"$HOH_ATTEMPT_ID\" in",
            &format!(
                "case \"$HOH_ATTEMPT_ID\" in\n *PUBLIC-BUSY*) exec python3 '{}' '{}' ;;",
                script.display(),
                emitted.display()
            ),
        );
        world.script("claude", &body)?;
        world.bind_installed()?;
        let faults = Arc::new(PublicBusy::default());
        faults.busy.store(true, Ordering::SeqCst);
        let mut config = watched(&world);
        config.watchdog.quiet = Duration::from_millis(300);
        config.watchdog.every = Duration::from_millis(25);
        config.store.faults = faults.clone();
        let host = start_when_free(config).await?;
        let outcome=async {
            let project=world.project();let run=started(&host,&project,"busy").await?;
            ready(&project,&run,&[("PUBLIC-BUSY","developer")])?;
            let until=Instant::now()+Duration::from_secs(10);
            while faults.seen.load(Ordering::SeqCst)==0 {if Instant::now()>until{return Err("no real public persistence attempt".into());}tokio::time::sleep(Duration::from_millis(10)).await;}
            tokio::time::sleep(Duration::from_millis(700)).await;
            assert_eq!(query(&project,&watchdog_events("PUBLIC-BUSY/attempt-1","attempt.inactive"))?,"0","local storage blocking is not agent silence");
            if !cancel_while_busy {
                faults.busy.store(false,Ordering::SeqCst);until_file(&emitted,Duration::from_secs(10)).await?;
                super::until(&project,&watchdog_events("PUBLIC-BUSY/attempt-1","attempt.inactive"),"1",Duration::from_secs(10)).await?;
                assert_ne!(query(&project,"SELECT count(*) FROM events WHERE kind='runner.diagnostic' AND json_extract(payload_json,'$.reason')='public_storage_backpressure'")?,"0","local blocking has a persisted diagnostic");
            }
            let revision=query(&project,&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse::<i64>()?;
            let(status,answer)=common::fresh(json!(revision),|revision|{let body=json!({"schema_version":"1.0","command_id":"cancel-public","idempotency_key":"cancel-public","expected_revision":revision,"kind":"CancelRun","run_id":run,"payload":{"reason":"stop fixture"}});let host=&host;async move{exchange(host,"POST","/v1/commands",Some(&body)).await}}).await?;
            assert_eq!(status,202,"{answer}");
            super::until(&project,&format!("SELECT state FROM runs WHERE run_id='{run}'"),"CANCELLED",Duration::from_secs(15)).await?;
            assert_eq!(query(&project,"SELECT count(*) FROM attempts WHERE task_id='PUBLIC-BUSY' AND state IN ('PREPARED','STARTING','RUNNING')")?,"0");
            R::Ok(())
        }.await;
        faults.busy.store(false, Ordering::SeqCst);
        host.shutdown().await;
        world.stop_fakes()?;
        outcome?;
    }
    Ok(())
}
#[derive(Debug)]
struct PublicCapacity(std::sync::atomic::AtomicU64);
impl hoh_core::persistence::FaultHooks for PublicCapacity {
    fn public_available_bytes(&self) -> Option<u64> {
        Some(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}
#[tokio::test]
async fn test_011_05_http_input_original_objects_idempotency_and_single_low_space_error() -> R {
    let world = World::new()?;
    let capacity = std::sync::Arc::new(PublicCapacity(std::sync::atomic::AtomicU64::new(0)));
    let mut config = world.config();
    config.runner = None;
    config.store.faults = capacity.clone();
    let host = start_when_free(config).await?;
    let result=async {
        let project=world.project();let run=started(&host,&project,"input-public").await?;ready(&project,&run,&[])?;
        let text="original\r\n用户\rtext\n".repeat(2000);let normalized=text.replace("\r\n","\n").replace('\r',"\n");
        let revision=query(&project,&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse::<i64>()?;
        let body=json!({"schema_version":"1.0","command_id":"public-input-one","idempotency_key":"public-input-one","expected_revision":revision,"kind":"SendInput","run_id":run,"payload":{"text":text,"target_role":"commander","input_kind":"ASK"}});
        let before=query(&project,"SELECT count(*) FROM input_queue")?;
        for _ in 0..2 {
            let(status,answer)=exchange(&host,"POST","/v1/commands",Some(&body)).await?;
            assert_eq!(status,503,"{answer}");assert_eq!(answer["code"],"STORAGE_LOW_SPACE");assert_eq!(answer["retryable"],false);assert_eq!(answer["details"]["available_bytes"],0);
            assert_eq!(answer["details"]["required_bytes"],(normalized.len() as u64)+(32<<20));
            assert_eq!(query(&project,"SELECT count(*) FROM input_queue")?,before);
            assert_eq!(query(&project,"SELECT count(*) FROM events WHERE kind='public.message'")?,"0");
            assert_eq!(query(&project,"SELECT count(*) FROM commands WHERE command_id='public-input-one'")?,"0");
        }
        capacity.0.store(u64::MAX,std::sync::atomic::Ordering::SeqCst);
        let(status,first)=exchange(&host,"POST","/v1/commands",Some(&body)).await?;assert_eq!(status,202,"{first}");
        let(status,replay)=exchange(&host,"POST","/v1/commands",Some(&body)).await?;assert_eq!(status,202,"{replay}");
        assert_eq!(query(&project,"SELECT count(*) FROM public_input_blocks")?,"1");
        let mut second=body.clone();second["command_id"]=json!("public-input-two");second["idempotency_key"]=json!("public-input-two");second["expected_revision"]=json!(query(&project,&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse::<i64>()?);
        let(status,answer)=exchange(&host,"POST","/v1/commands",Some(&second)).await?;assert_eq!(status,202,"{answer}");
        assert_eq!(query(&project,"SELECT count(*) FROM public_input_blocks")?,"2");
        assert_eq!(query(&project,"SELECT count(*) FROM public_segments")?,"2");assert_eq!(query(&project,"SELECT count(DISTINCT artifact_id) FROM public_segments")?,"1");
        let path=query(&project,"SELECT a.path FROM artifacts a JOIN public_segments p USING(artifact_id) LIMIT 1")?;
        assert_eq!(std::fs::read(project.join(path))?,normalized.as_bytes());
        let public:Value=serde_json::from_str(&query(&project,"SELECT payload_json FROM events WHERE kind='public.message' ORDER BY seq LIMIT 1")?)?;
        assert!(public.get("text").is_none());assert_eq!(public["speaker"],"user");assert!(public["attempt_id"].is_null());
        R::Ok(())
    }.await;
    host.shutdown().await;
    world.stop_fakes()?;
    result
}

#[derive(Debug, Default)]
struct PublicDiagnosticFault {
    job: std::sync::Mutex<&'static str>,
    public_count: std::sync::atomic::AtomicUsize,
    arm_diag_refusal: std::sync::atomic::AtomicBool,
    refuse_next_commit: std::sync::atomic::AtomicBool,
    diagnostic_refusals: std::sync::atomic::AtomicUsize,
    public_refusals: std::sync::atomic::AtomicUsize,
}
impl hoh_core::persistence::FaultHooks for PublicDiagnosticFault {
    fn before_job(&self, job: &'static str) {
        if let Ok(mut current) = self.job.lock() {
            *current = job;
        }
    }
    fn before_commit(&self) -> Result<(), hoh_core::persistence::PersistError> {
        use std::sync::atomic::Ordering;
        let current = self.job.lock().map(|g| *g).unwrap_or("");
        if current == "Public" {
            let n = self.public_count.fetch_add(1, Ordering::SeqCst);
            // Frame 0 (init, offset 0): n=0 refused -> n=1 ok (optionally refuses its diagnostic Commit).
            // Frame 1 (FIRST_BLOCK, offset 1): n=2 ok.
            // Frame 2 part 0 (PART_ZERO, offset 2): n=3 refused -> n=4 ok.
            // Frame 2 part 1 (tool_use, offset 2): n=5 refused -> n=6 ok.
            if matches!(n, 0 | 3 | 5) {
                self.public_refusals.fetch_add(1, Ordering::SeqCst);
                if n == 0 && self.arm_diag_refusal.load(Ordering::SeqCst) {
                    self.refuse_next_commit.store(true, Ordering::SeqCst);
                }
                return Err(hoh_core::persistence::PersistError::Busy(
                    "injected public DB_BUSY".into(),
                ));
            }
        } else if current == "Commit" && self.refuse_next_commit.swap(false, Ordering::SeqCst) {
            self.diagnostic_refusals.fetch_add(1, Ordering::SeqCst);
            return Err(hoh_core::persistence::PersistError::Busy(
                "injected diagnostic commit DB_BUSY".into(),
            ));
        }
        Ok(())
    }
}

#[tokio::test]
async fn test_011_05_drain_survives_backpressure_diagnostic_busy_and_same_offset_parts() -> R {
    use std::sync::{Arc, atomic::Ordering};
    for refuse_diag_commit in [true, false] {
        let world = World::new()?;
        let body = world.claude_body(None)?;
        let frames = world.bin().join("claude.d/public-diag-fault.jsonl");
        std::fs::write(
            &frames,
            [
                json!({"type":"system","subtype":"init","session_id":"diag-fault","model":"fixture"}),
                json!({"type":"assistant","uuid":"f1","session_id":"diag-fault","message":{"id":"m1","content":[{"type":"text","text":"FIRST_BLOCK"}]}}),
                json!({"type":"assistant","uuid":"f2","session_id":"diag-fault","message":{"id":"m2","content":[{"type":"text","text":"PART_ZERO"},{"type":"tool_use","id":"call-1","name":"Bash","input":{"command":"echo ok"}}]}}),
                json!({"type":"assistant","uuid":"f3","session_id":"diag-fault","message":{"id":"m3","content":[{"type":"text","text":"FINAL_BLOCK"}]}}),
                json!({"type":"result","uuid":"f4","subtype":"success","is_error":false,"result":"FINAL_BLOCK","session_id":"diag-fault","usage":{"input_tokens":10,"output_tokens":5}}),
            ]
            .into_iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
                + "\n",
        )?;
        let body = body.replace("$f/success/stdout.jsonl", &frames.display().to_string());
        world.script("claude", &body)?;
        world.bind_installed()?;
        let faults = Arc::new(PublicDiagnosticFault::default());
        faults
            .arm_diag_refusal
            .store(refuse_diag_commit, Ordering::SeqCst);
        let mut config = world.config();
        config.store.faults = faults.clone();
        let host = start_when_free(config).await?;
        let outcome = async {
            let project = world.project();
            let run = started(&host, &project, "diag-fault").await?;
            ready(&project, &run, &[("DIAG", "developer")])?;
            until(
                &project,
                "SELECT state FROM attempts WHERE attempt_id='DIAG/attempt-1'",
                "SETTLED",
                Duration::from_secs(30),
            )
            .await?;
            assert_eq!(faults.public_refusals.load(Ordering::SeqCst), 3, "refuse_diag_commit={refuse_diag_commit}");
            assert_eq!(
                faults.diagnostic_refusals.load(Ordering::SeqCst),
                usize::from(refuse_diag_commit),
                "refuse_diag_commit={refuse_diag_commit}"
            );
            let raw = query(
                &project,
                "SELECT json_group_array(json(payload_json)) FROM events WHERE kind LIKE 'public.%' AND json_extract(payload_json,'$.attempt_id')='DIAG/attempt-1'",
            )?;
            let messages: Vec<Value> = serde_json::from_str(&raw)?;
            assert!(
                messages.iter().any(|p| p["text"] == "FIRST_BLOCK"),
                "refuse_diag_commit={refuse_diag_commit}: first block persisted: {raw}"
            );
            assert!(
                messages.iter().any(|p| p["text"] == "PART_ZERO"),
                "refuse_diag_commit={refuse_diag_commit}: same-offset part 0 persisted: {raw}"
            );
            assert!(
                messages
                    .iter()
                    .any(|p| p["name"] == "Bash" && p["stream"] == "input"),
                "refuse_diag_commit={refuse_diag_commit}: same-offset part 1 persisted: {raw}"
            );
            assert!(
                messages.iter().any(|p| p["text"] == "FINAL_BLOCK"),
                "refuse_diag_commit={refuse_diag_commit}: drain must stay alive and persist final block: {raw}"
            );
            assert_eq!(
                query(
                    &project,
                    "SELECT count(*) FROM events WHERE kind='public.gap' AND json_extract(payload_json,'$.attempt_id')='DIAG/attempt-1'",
                )?,
                "0",
                "refuse_diag_commit={refuse_diag_commit}: no drain death gap expected"
            );
            R::Ok(())
        }
        .await;
        host.shutdown().await;
        world.stop_fakes()?;
        outcome?;
    }
    Ok(())
}

#[tokio::test]
async fn test_011_05_low_space_body_writes_gap_without_stalling_attempt_or_inactivity() -> R {
    use std::sync::{Arc, atomic::AtomicU64};
    let world = World::new()?;
    let body = world.claude_body(None)?;
    let script = world.bin().join("claude.d/public-lowspace.py");
    let emitted = world.bin().join("claude.d/public-lowspace-emitted");
    std::fs::write(
        &script,
        r#"import json,signal,sys
signal.signal(signal.SIGINT,lambda *_:sys.exit(0))
signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
print(json.dumps({'type':'system','subtype':'init','session_id':'lowspace','model':'fixture'}),flush=True)
print(json.dumps({'type':'assistant','uuid':'big','session_id':'lowspace','message':{'id':'m1','content':[{'type':'text','text':'L'*24576}]}}),flush=True)
print(json.dumps({'type':'assistant','uuid':'small','session_id':'lowspace','message':{'id':'m2','content':[{'type':'text','text':'AFTER_LOW_SPACE'}]}}),flush=True)
open(sys.argv[1],'w').close()
signal.pause()
"#,
    )?;
    let body = body.replace(
        "case \"$HOH_ATTEMPT_ID\" in",
        &format!(
            "case \"$HOH_ATTEMPT_ID\" in\n *LOWSPACE*) exec python3 '{}' '{}' ;;",
            script.display(),
            emitted.display()
        ),
    );
    world.script("claude", &body)?;
    world.bind_installed()?;
    let capacity = Arc::new(PublicCapacity(AtomicU64::new(0)));
    let mut config = watched(&world);
    config.watchdog.quiet = Duration::from_millis(500);
    config.watchdog.every = Duration::from_millis(25);
    config.store.faults = capacity;
    let host = start_when_free(config).await?;
    let outcome = async {
        let project = world.project();
        let run = started(&host, &project, "lowspace").await?;
        ready(&project, &run, &[("LOWSPACE", "developer")])?;
        until_file(&emitted, Duration::from_secs(10)).await?;
        super::until(
            &project,
            "SELECT count(*) FROM events WHERE kind='public.gap' AND json_extract(payload_json,'$.attempt_id')='LOWSPACE/attempt-1' AND json_extract(payload_json,'$.reason')='storage_low_space'",
            "1",
            Duration::from_secs(10),
        )
        .await?;
        super::until(
            &project,
            "SELECT count(*) FROM events WHERE kind='public.message' AND json_extract(payload_json,'$.attempt_id')='LOWSPACE/attempt-1' AND json_extract(payload_json,'$.text')='AFTER_LOW_SPACE'",
            "1",
            Duration::from_secs(10),
        )
        .await?;
        assert_eq!(
            query(&project, "SELECT count(*) FROM public_segments")?,
            "0",
            "low-space body must not write an object"
        );
        super::until(
            &project,
            &watchdog_events("LOWSPACE/attempt-1", "attempt.inactive"),
            "1",
            Duration::from_secs(10),
        )
        .await?;
        let revision = query(&project, &format!("SELECT revision FROM runs WHERE run_id='{run}'"))?
            .parse::<i64>()?;
        let (status, answer) = common::fresh(json!(revision), |revision| {
            let body = json!({"schema_version":"1.0","command_id":"cancel-lowspace","idempotency_key":"cancel-lowspace","expected_revision":revision,"kind":"CancelRun","run_id":run,"payload":{"reason":"stop lowspace"}});
            let host = &host;
            async move { exchange(host, "POST", "/v1/commands", Some(&body)).await }
        })
        .await?;
        assert_eq!(status, 202, "{answer}");
        super::until(
            &project,
            &format!("SELECT state FROM runs WHERE run_id='{run}'"),
            "CANCELLED",
            Duration::from_secs(15),
        )
        .await?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}
