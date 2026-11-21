//! TEST-017-02: an installed Codex through the product host, plus the deterministic unsupported-agent companion.
//! Ignored outside adapter-live. A socket held by this test keeps the native foreground tool running until the
//! host has durably acknowledged turn steering. No fixed delay establishes the ordering.
use super::*;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

fn verdict(out: &Path, status: &str, detail: &str, sessions: &[Value], observations: Value) -> R {
    std::fs::write(
        out.join("steer-live-verdict.json"),
        serde_json::to_vec_pretty(&json!({
            "status":status,"detail":detail,"sessions":sessions,"observations":observations,
        }))?,
    )?;
    Ok(())
}

async fn input_state(host: &Host, id: &str, expected: hoh_core::lifecycle::InputState) -> R {
    let mut changed = host.store().watermark();
    tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            if let Some(row) = host.store().reader()?.input(id)? {
                if row.state == expected {
                    return R::Ok(());
                }
                if row.state == hoh_core::lifecycle::InputState::Rejected {
                    return Err(format!("input rejected: {:?}", row.payload).into());
                }
            }
            changed.changed().await?;
        }
    })
    .await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: installed Codex; run only through adapter-live"]
async fn test_017_02_steer_boundaries_live() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("no HOH_LIVE_OUT")?);
    let program = PathBuf::from(std::env::var_os("HOH_LIVE_CODEX").ok_or("no HOH_LIVE_CODEX")?);
    if !Command::new(&program)
        .args(["login", "status"])
        .output()?
        .status
        .success()
    {
        return verdict(&out, "BLOCKED", "Codex is not logged in", &[], json!({}));
    }
    // This companion exercises the product's next-Attempt prompt, receipt and application through a fake agent
    // that lacks steering. Its output stays explicitly product, never a native session claim.
    let companion = tokio::task::spawn_blocking(|| {
        Command::new("cargo")
            .args([
                "test",
                "-p",
                "hoh-app",
                "--locked",
                "--test",
                "control",
                "test_017_02_host_selects_turn_or_next_attempt_with_explicit_barriers",
                "--",
                "--exact",
                "--nocapture",
            ])
            .output()
    })
    .await??;
    std::fs::write(
        out.join("next-attempt-product.stdout.log"),
        &companion.stdout,
    )?;
    std::fs::write(
        out.join("next-attempt-product.stderr.log"),
        &companion.stderr,
    )?;
    if !companion.status.success()
        || !String::from_utf8_lossy(&companion.stdout).contains(
            "test test_017_02_host_selects_turn_or_next_attempt_with_explicit_barriers ... ok",
        )
    {
        verdict(
            &out,
            "FAIL",
            "product fallback companion failed",
            &[],
            json!({"companion_exit":companion.status.code()}),
        )?;
        return Err("product fallback companion failed".into());
    }
    let world = World::new()?;
    world.claude(None)?; // inert recorded Commander; only the developer is native Codex
    std::os::unix::fs::symlink(&program, world.bin().join("codex"))?;
    let path = format!("{}:{}", world.bin().display(), std::env::var("PATH")?);
    let mut effective = hoh_core::config::Config::default();
    let mut config = "config_version = \"1.0\"\n".to_owned();
    for (role, binding) in &mut effective.team {
        binding.agent = if role == "developer" {
            hoh_core::config::Agent::Codex
        } else {
            hoh_core::config::Agent::Claude
        };
        binding.model = None;
        binding.fallback.clear();
        config.push_str(&format!(
            "[team.{role}]\nagent = \"{}\"\nfallback = []\n",
            binding.agent.as_str()
        ));
    }
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(project.join(".hoh/config.toml"), config)?;
    git(&project, &["init", "-q"])?;
    let installed = hoh_app::compat::register(
        &project,
        hoh_agents::compat::Agent::Codex,
        std::ffi::OsStr::new(&path),
    )?;
    std::fs::write(
        out.join("codex-install.json"),
        serde_json::to_vec_pretty(&installed.entry)?,
    )?;
    if installed.entry.suite.status != hoh_agents::compat::SuiteStatus::Pass {
        return verdict(
            &out,
            "BLOCKED",
            "installed Codex compatibility suite did not pass",
            &[],
            json!({}),
        );
    }
    if installed.entry.capabilities.get("mid_turn_steer") != Some(&Some(true)) {
        return verdict(
            &out,
            "BLOCKED",
            "installed Codex has no mid_turn_steer",
            &[],
            json!({}),
        );
    }
    hoh_app::compat::register(
        &project,
        hoh_agents::compat::Agent::Claude,
        std::ffi::OsStr::new(&path),
    )?;
    let registry = Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
        path.into(),
        Some(world.dir.path().join("pi-agent")),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let script = project.join("steer_barrier.py");
    std::fs::write(
        &script,
        format!(
            "import socket\nwith socket.create_connection(('127.0.0.1', {}), 180) as s:\n s.sendall(b'R')\n assert s.recv(1) == b'G'\nprint('barrier released')\n",
            listener.local_addr()?.port()
        ),
    )?;
    let host = Host::start(world.config_with(effective, registry, None, None)).await?;
    let mut sessions = Vec::new();
    let mut observations = json!({"fallback_mode":"product","native_mode":"live","companion_exit":companion.status.code()});
    let result=async {
        let run=started(&host,&project,"live-steer").await?;
        let title=format!("只读诊断任务：首先且仅执行一次 `python3 {}`，保持前台等待此命令完成，不取消它。随后回答 STEERED。不修改仓库、规格或任务，不派发其他工作。",script.display());
        let plan=json!({"run_id":run,"tasks":[{"task_id":"T-LIVE-STEER","title":title}]}).to_string().replace('\'',"''");
        exec(&project,&format!("INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) SELECT project_id,'live-steer-plan','{run}','plan.imported','{plan}',strftime('%Y-%m-%dT%H:%M:%SZ','now') FROM runs WHERE run_id='{run}';"))?;
        ready(&project,&run,&[("T-LIVE-STEER","developer")])?;
        let (mut barrier,_)=tokio::time::timeout(Duration::from_secs(240),listener.accept()).await??;
        let mut ready=[0];
        tokio::time::timeout(Duration::from_secs(30),barrier.read_exact(&mut ready)).await??;
        if ready!=*b"R" {return Err("native tool did not enter the barrier".into());}
        let (status,reply)=common::fresh(json!(query(&project,&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?.parse::<i64>()?),|revision| {
            let body=json!({"schema_version":"1.0","command_id":"live-steer-input","idempotency_key":"live-steer-input","kind":"SendInput","run_id":run,"expected_revision":revision,"payload":{"input_kind":"STEER","target_role":"developer","text":"收到此引导后继续等待前台命令正常完成，然后只回答 STEERED。不要取消命令。"}});
            let host=&host;async move {exchange(host,"POST","/v1/commands",Some(&body)).await}
        }).await?;
        if status!=202 {return Err(format!("SendInput {status}: {reply}").into());}
        let id=hoh_core::ids::child_id("live-steer-input","input");
        input_state(&host,&id,hoh_core::lifecycle::InputState::Delivered).await?;
        let attempt="T-LIVE-STEER/attempt-1";
        let logs=hoh_app::host::product::attempt_logs(&project,attempt);
        let before=std::fs::read(logs.join("app-server.stdout.jsonl"))?;
        std::fs::write(out.join("before-tool-release.jsonl"),&before)?;
        let frames=before.split(|b|*b==b'\n').filter(|b|!b.is_empty()).map(serde_json::from_slice::<Value>).collect::<Result<Vec<_>,_>>()?;
        // The native turn/steer response proves the request reached Codex before this still-held turn completed.
        if frames.iter().any(|v|v["method"]=="turn/completed") || frames.iter().filter(|v|v["result"]["turnId"].is_string()).count()!=1 {
            return Err("no unique native steer ACK before turn completion".into());
        }
        barrier.write_all(b"G").await?;
        barrier.shutdown().await?;
        input_state(&host,&id,hoh_core::lifecycle::InputState::Applied).await?;
        let final_log=std::fs::read(logs.join("app-server.stdout.jsonl"))?;
        std::fs::write(out.join("completed-turn.jsonl"),&final_log)?;
        if !final_log.starts_with(&before) {return Err("raw turn log changed its prefix".into());}
        let events=host.store().reader()?.events(&run)?.into_iter().filter(|(k,v)|k=="input.delivered" && v["input_id"]==id).collect::<Vec<_>>();
        if events.len()!=1 || events[0].1["boundary"]!="turn" {return Err(format!("wrong boundary: {events:?}").into());}
        let native=query(&project,"SELECT json_object('agent',s.agent,'cli_version',s.agent_version,'session_id',s.native_id,'resolved_model',coalesce(a.resolved_model,'unknown')) FROM attempts a JOIN sessions s USING(session_id) WHERE a.attempt_id='T-LIVE-STEER/attempt-1'")?;
        let native:Value=serde_json::from_str(&native)?;
        if native["session_id"].as_str().is_none_or(str::is_empty) {return Err("no native session evidence".into());}
        sessions.push(native);
        observations["delivered"]=json!(events);
        observations["raw_before_sha256"]=json!(file_sha256(&out.join("before-tool-release.jsonl"))?);
        observations["raw_complete_sha256"]=json!(file_sha256(&out.join("completed-turn.jsonl"))?);
        R::Ok(())
    }.await;
    // The host owns and drains its exact gated native processes even after a barrier or assertion failure.
    host.shutdown().await;
    let blocked_auth = query(
        &project,
        "SELECT count(*) FROM attempts WHERE failure_reason='BLOCKED_AUTH'",
    )? != "0";
    match result {
        Ok(()) => verdict(
            &out,
            "PASS",
            "native Codex steer ACK while foreground tool held; product next-Attempt companion passed",
            &sessions,
            observations,
        ),
        Err(error) => {
            verdict(
                &out,
                if blocked_auth { "BLOCKED" } else { "FAIL" },
                &error.to_string(),
                &sessions,
                observations,
            )?;
            if blocked_auth { Ok(()) } else { Err(error) }
        }
    }
}
