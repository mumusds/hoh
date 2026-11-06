//! `hoh doctor --live` (ADR-015 decision 9): one minimal native turn per installed agent, through its own adapter, in
//! a scratch directory with a scratch store, never the project's database. It is a model call and may cost money.
//! SETTLED with the token in the reply is verified, BLOCKED_AUTH is missing, anything else is unknown. Each agent's
//! item gets the attempt as stored (mode=live, the native session id), and the raw logs stay in the scratch
//! directory, whose path is reported.

use super::report::{AgentItem, Auth};
use hoh_agents::adapter::AttemptRef;
use hoh_agents::clock::SystemClock;
use hoh_agents::compat::Agent;
use hoh_agents::probe::{self, ProbeEnv};
use hoh_agents::{claude, codex, pi};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{CancelLatch, NewAttempt, Options, Store};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// What the reply must contain.
const TOKEN: &str = "HOH-DOCTOR-OK";
const PROMPT: &str = "Reply with exactly HOH-DOCTOR-OK and nothing else. Do not use any tool.";
/// The longest a live turn may take.
const DEADLINE: Duration = Duration::from_secs(180);

/// The end of one live turn.
struct Ended {
    state: AttemptState,
    failure: Option<String>,
    native_session: Option<String>,
    reply: Option<String>,
}

fn stdout_of(launcher: &Path, args: &[&str], env: &ProbeEnv) -> Result<String, String> {
    let out = probe::run_status(launcher, args, env).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "{} {} exited with {}",
            launcher.display(),
            args.join(" "),
            out.status
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A live turn ready to run: everything asked of the CLI before the attempt exists, so a turn that cannot be prepared
/// leaves no attempt behind (M1T07R-04).
enum Prepared {
    Claude(claude::Turn),
    Codex(codex::Turn),
    Pi(pi::Turn),
}

/// The turn of `agent`; `capabilities` are what its probe found, for the session row (REQ-027).
fn prepare(
    agent: Agent,
    launcher: &Path,
    work: &Path,
    logs: PathBuf,
    env: &ProbeEnv,
    capabilities: &hoh_agents::capabilities::Probed,
) -> Result<Prepared, String> {
    Ok(match agent {
        Agent::Claude => Prepared::Claude(claude::Turn {
            launch: claude::Launch {
                program: launcher.to_path_buf(),
                help: stdout_of(launcher, &["--help"], env)?,
                cwd: work.to_path_buf(),
                yolo: false,
                partial: false,
                resume: None,
                root: rustix::process::geteuid().is_root(),
                capabilities: capabilities.clone(),
            },
            prompt: PROMPT.as_bytes().to_vec(),
            logs,
            escalation: claude::Turn::default_escalation(),
            deadline: Some(DEADLINE),
        }),
        Agent::Codex => {
            // The transports `codex --help` lists, read as the probe reads them (M1R-03); app-server's schema names the
            // methods of both directions, and codex::run uses it only if they have everything a turn uses, as the probe
            // decides (M1T07R-15, M1R-04).
            let top = stdout_of(launcher, &["--help"], env)?;
            let listed = |name: &str| hoh_agents::adapter::lists_command(&top, name);
            let app_server_help = match listed("app-server") {
                true => Some(stdout_of(launcher, &["app-server", "--help"], env)?),
                false => None,
            };
            // An App Server that cannot start on stdio is not used whatever its schema says, so its schema is not asked
            // for; codex::run then takes exec, saying why (the probe's order, M1T07R-27, -29).
            let startable = app_server_help
                .as_deref()
                .filter(|help| codex::app_server_launch_missing(help).is_none());
            let (known, requests, client_notifications) = match startable {
                None => Default::default(),
                Some(_) => {
                    // The probe's validated reader: a schema the probe refuses is refused here too (M1T07R-31).
                    let schema = probe::codex_schema(launcher, env).map_err(|e| e.to_string())?;
                    (
                        schema.notifications,
                        schema.requests,
                        schema.client_notifications,
                    )
                }
            };
            // Without an exec subcommand the exec fallback has no help, so it cannot start (codex::exec_args).
            let (exec_help, exec_resume_help) = match listed("exec") {
                true => (
                    stdout_of(launcher, &["exec", "--help"], env)?,
                    stdout_of(launcher, &["exec", "resume", "--help"], env).ok(),
                ),
                false => (String::new(), None),
            };
            Prepared::Codex(codex::Turn {
                launch: codex::Launch {
                    program: launcher.to_path_buf(),
                    app_server_help,
                    exec_help,
                    exec_resume_help,
                    cwd: work.to_path_buf(),
                    yolo: false,
                    resume: None,
                    model: None,
                    known,
                    requests,
                    client_notifications,
                    capabilities: capabilities.clone(),
                },
                prompt: PROMPT.to_owned(),
                logs,
                escalation: codex::Turn::default_escalation(),
                handshake_timeout: Duration::from_secs(60),
                deadline: Some(DEADLINE),
            })
        }
        Agent::Pi => Prepared::Pi(pi::Turn {
            launch: pi::Launch {
                program: launcher.to_path_buf(),
                help: stdout_of(launcher, &["--help"], env)?,
                cwd: work.to_path_buf(),
                session_dir: work.join("..").join("pi-sessions"),
                session_id: std::fs::read_to_string("/proc/sys/kernel/random/uuid")
                    .map(|s| s.trim().to_owned())
                    .map_err(|e| e.to_string())?,
                resume: false,
                provider: None,
                model: None,
                settled_event: true,
                capabilities: capabilities.clone(),
            },
            prompt: PROMPT.to_owned(),
            input_id: None,
            logs,
            escalation: pi::Turn::default_escalation(),
            deadline: Some(DEADLINE),
            clock: Arc::new(SystemClock),
            events: None,
        }),
    })
}

async fn run(store: &Store, attempt: &AttemptRef, prepared: &Prepared) -> Result<Ended, String> {
    let latch = CancelLatch::new();
    Ok(match prepared {
        Prepared::Claude(turn) => {
            let o = claude::run(store, attempt, turn, &latch)
                .await
                .map_err(|e| e.to_string())?;
            Ended {
                state: o.state,
                failure: o.failure,
                native_session: o.native_session,
                reply: o.result.and_then(|r| r.text),
            }
        }
        Prepared::Codex(turn) => {
            let (_steer, steers) = tokio::sync::mpsc::channel(1);
            let o = codex::run(store, attempt, turn, &latch, steers)
                .await
                .map_err(|e| e.to_string())?;
            Ended {
                state: o.state,
                failure: o.failure,
                native_session: o.native_session,
                reply: o.reply,
            }
        }
        Prepared::Pi(turn) => {
            let o = pi::run(store, attempt, turn, &latch)
                .await
                .map_err(|e| e.to_string())?;
            Ended {
                state: o.state,
                failure: o.failure,
                native_session: o.native_session,
                reply: o.reply,
            }
        }
    })
}

/// One agent's live turn: its auth conclusion and its record.
async fn one(
    agent: Agent,
    store: &Store,
    item: &AgentItem,
    scratch: &Path,
    env: &ProbeEnv,
) -> (Auth, Value) {
    let name = agent.command();
    let attempt_id = format!("doctor-{name}");
    let launcher = PathBuf::from(item.path.clone().unwrap_or_default());
    let work = scratch.join(name).join("work");
    let logs = scratch.join(name).join("logs");
    let attempt = AttemptRef {
        attempt_id: attempt_id.clone(),
        session_id: format!("doctor-{name}-session"),
        role: "doctor".to_owned(),
        agent_version: item.version.clone().unwrap_or_default(),
    };
    let ended = async {
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        // The CLIs expect a repository; its `git init` is bounded, logged and group-killed like every other command
        // (M1T07R-10), and past the preparation limit it is not started.
        let git = probe::find_on_path("git", &env.path).ok_or("git is not installed")?;
        let work_arg = work.display().to_string();
        stdout_of(&git, &["-C", &work_arg, "init", "-q"], env)?;
        let prepared = prepare(
            agent,
            &launcher,
            &work,
            logs.clone(),
            env,
            &item.capabilities,
        )?;
        store
            .create_attempt(NewAttempt {
                attempt_id: attempt_id.clone(),
                task_id: "doctor".to_owned(),
                session_id: None,
                role_epoch: 1,
                host_epoch: 1,
                config_revision: 0,
                agent: Some(name.to_owned()),
            })
            .await
            .map_err(|e| e.to_string())?;
        run(store, &attempt, &prepared).await
    }
    .await;
    let reader = store.reader().ok();
    let row = reader
        .as_ref()
        .and_then(|r| r.attempt(&attempt_id).ok().flatten());
    // The session row's capabilities: what the probe found, narrowed by the transport the turn used (REQ-027).
    let session_capabilities = reader
        .as_ref()
        .and_then(|r| r.session(&attempt.session_id).ok().flatten())
        .map(|s| s.capabilities);
    let stored = row.map(|r| {
        json!({"state": r.state, "failure_reason": r.failure_reason, "started_at": r.started_at, "ended_at": r.ended_at,
               "resolved_model": r.resolved_model, "pid": r.pid, "session_capabilities": session_capabilities})
    });
    match ended {
        Ok(e) => {
            let reply_ok = e.reply.as_deref().is_some_and(|r| r.contains(TOKEN));
            let auth = auth_of(e.state, e.failure.as_deref(), e.reply.as_deref());
            (
                auth,
                json!({"mode": "live", "attempt_id": attempt_id, "native_session_id": e.native_session,
                       "state": e.state, "failure_reason": e.failure, "reply_ok": reply_ok, "stored": stored,
                       "logs": logs.display().to_string()}),
            )
        }
        Err(why) => (
            Auth::Unknown,
            json!({"mode": "live", "attempt_id": attempt_id, "error": why, "stored": stored,
                   "logs": logs.display().to_string()}),
        ),
    }
}

/// The conclusion of a live turn: verified only when it settled with the token in its reply; missing when the
/// agent refused for want of a login; unknown otherwise.
pub fn auth_of(state: AttemptState, failure: Option<&str>, reply: Option<&str>) -> Auth {
    match (state, failure) {
        (AttemptState::Settled, _) if reply.is_some_and(|r| r.contains(TOKEN)) => Auth::Verified,
        (_, Some("BLOCKED_AUTH")) => Auth::Missing,
        _ => Auth::Unknown,
    }
}

/// The scratch directory of this run: under the system temporary directory, kept for its raw logs.
fn scratch() -> std::io::Result<PathBuf> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("hoh-doctor-live-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Live-check every installed agent HoH can drive; the items are updated in place. The scratch directory, or why
/// there is none.
pub fn check(
    items: &mut std::collections::BTreeMap<&'static str, AgentItem>,
    env: &ProbeEnv,
) -> Result<PathBuf, String> {
    let dir = scratch().map_err(|e| format!("无法创建临时目录：{e}"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("无法启动运行时：{e}"))?;
    runtime.block_on(async {
        let store = Store::open(&dir.join(".hoh/state.db"), Options::default())
            .map_err(|e| e.to_string())?;
        store
            .scratch_task("doctor", &dir.display().to_string(), "doctor", "doctor")
            .await
            .map_err(|e| e.to_string())?;
        let ready = |name: &str| {
            items
                .get(name)
                .filter(|i| i.installed && i.protocol != super::report::Protocol::Incompatible)
                .cloned()
        };
        let (codex, claude, pi) = (ready("codex"), ready("claude"), ready("pi"));
        let skip = || async { None::<(Auth, Value)> };
        let (c, cl, p) = tokio::join!(
            async {
                match &codex {
                    Some(i) => Some(one(Agent::Codex, &store, i, &dir, env).await),
                    None => skip().await,
                }
            },
            async {
                match &claude {
                    Some(i) => Some(one(Agent::Claude, &store, i, &dir, env).await),
                    None => skip().await,
                }
            },
            async {
                match &pi {
                    Some(i) => Some(one(Agent::Pi, &store, i, &dir, env).await),
                    None => skip().await,
                }
            },
        );
        for (name, result) in [("codex", c), ("claude", cl), ("pi", p)] {
            if let (Some(item), Some((auth, record))) = (items.get_mut(name), result) {
                item.auth_check = auth;
                item.live = Some(record);
                item.remedy = match auth {
                    Auth::Verified => None,
                    Auth::Missing => Some(format!("最小原生调用报告未登录：请先登录 {name}")),
                    _ => Some("最小原生调用没有成功：见 live 记录与原始日志".to_owned()),
                };
            }
        }
        Ok::<(), String>(())
    })?;
    Ok(dir)
}
