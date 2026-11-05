//! From probe results, the compat manifest and native status commands to one agent's item (ADR-015 decisions 2 and
//! 3). Status output is only read for its conclusion and never shown.

use super::report::{AgentItem, Auth, Protocol};
use hoh_agents::compat::{Agent, AgentEntry, CompatManifest, SuiteStatus, Transport};
use hoh_agents::probe::{self, Captured, ProbeEnv, ProbeError};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// What installing the CLI takes.
fn install(agent: Agent) -> &'static str {
    match agent {
        Agent::Codex => "安装 Codex CLI 后重试：npm install -g @openai/codex",
        Agent::Claude => {
            "安装 Claude Code 后重试：下载 https://claude.ai/install.sh 并运行 bash install.sh stable"
        }
        Agent::Pi => "安装 pi 后重试：npm install -g @earendil-works/pi-coding-agent@1.5.0",
    }
}

/// What logging in takes (doctor never logs in itself).
fn login(agent: Agent, provider: Option<&str>) -> String {
    match agent {
        Agent::Codex => "运行 codex login 登录".to_owned(),
        Agent::Claude => "运行 claude auth login 登录".to_owned(),
        Agent::Pi => format!(
            "为 pi 的 provider {} 配置凭证（pi 中使用 /login，或设置该 provider 的 API key），再用 pi auth check 确认",
            provider.unwrap_or("（未设置默认 provider）")
        ),
    }
}

/// The protocol of a probed CLI against the compat manifest: compatible (or degraded, exec only) only if the
/// implementation, help and schema hashes match an entry whose suite passed.
pub fn protocol(
    entry: &AgentEntry,
    manifest: Option<&CompatManifest>,
    agent: Agent,
) -> (Protocol, Option<String>) {
    let Some(known) = manifest.and_then(|m| m.agents.get(&agent)) else {
        return (
            Protocol::Unknown,
            Some("compat manifest 中没有这一项：该版本未经兼容套件验证".to_owned()),
        );
    };
    let same = known.implementation_sha256 == entry.implementation_sha256
        && known.help_sha256 == entry.help_sha256
        && known.schema_sha256 == entry.schema_sha256;
    if !same {
        return (
            Protocol::Unknown,
            Some("已安装版本的哈希与 compat manifest 不符：该版本未经兼容套件验证".to_owned()),
        );
    }
    if known.suite.status != SuiteStatus::Pass {
        return (
            Protocol::Unknown,
            Some("compat manifest 中该版本的兼容套件未通过".to_owned()),
        );
    }
    if entry.transport == Transport::Exec {
        return (
            Protocol::Degraded,
            Some("只有 exec 可用：没有 app-server（不能中途 steer）".to_owned()),
        );
    }
    (Protocol::Compatible, None)
}

/// A JSON object in a status command's output: all of it, or from the first line that starts with `{`.
fn json_object(stdout: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(stdout);
    let whole = serde_json::from_str::<Value>(text.trim())
        .ok()
        .filter(Value::is_object);
    whole.or_else(|| {
        let start = text
            .match_indices('\n')
            .map(|(i, _)| i + 1)
            .chain(std::iter::once(0))
            .filter(|i| text[*i..].starts_with('{'))
            .min()?;
        serde_json::from_str::<Value>(text[start..].trim())
            .ok()
            .filter(Value::is_object)
    })
}

/// The conclusion of a native status command (ADR-015 decision 3); `None` when it says nothing clear.
pub fn conclusion(agent: Agent, out: &Captured) -> Option<Auth> {
    match agent {
        Agent::Codex => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if text.contains("Not logged in") {
                Some(Auth::Missing)
            } else if out.status.success()
                && text
                    .lines()
                    .any(|l| l.trim_start().starts_with("Logged in"))
            {
                Some(Auth::NotVerified)
            } else {
                None
            }
        }
        // A positive answer counts only from a command that succeeded; "not logged in" comes with a failing exit
        // (M1T07R-12).
        Agent::Claude => match json_object(&out.stdout)?.get("loggedIn")?.as_bool()? {
            true if out.status.success() => Some(Auth::NotVerified),
            true => None,
            false => Some(Auth::Missing),
        },
        Agent::Pi => {
            let answer = json_object(&out.stdout)?;
            match (answer["status"].as_str()?, answer["reason"].as_str()) {
                ("ready", _) if out.status.success() => Some(Auth::NotVerified),
                ("not_ready", Some("credentials_not_configured")) => Some(Auth::Missing),
                _ => None,
            }
        }
    }
}

/// pi's default provider, from its settings (not its credentials): `$PI_CODING_AGENT_DIR/settings.json`, else
/// `~/.pi/agent/settings.json`.
pub fn pi_provider() -> Option<String> {
    let dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".pi/agent")))?;
    let settings: Value =
        serde_json::from_slice(&std::fs::read(dir.join("settings.json")).ok()?).ok()?;
    settings["defaultProvider"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
}

/// Why `what` gave no answer, if it timed out or was not started: its own limit, the time left of doctor's total limit,
/// or none of it left (M1T07R-08).
pub fn unfinished(what: &str, e: &ProbeError) -> Option<String> {
    match e {
        ProbeError::TimedOut {
            after,
            cut_by_deadline: false,
            ..
        } => Some(format!("{what} {after:?} 内没有结束，已终止其进程组")),
        ProbeError::TimedOut { after, .. } => Some(format!(
            "doctor 的总时限只剩 {after:?}，{what}没有在其中结束，已终止其进程组"
        )),
        ProbeError::NotStarted { .. } => Some(format!("doctor 的总时限已用完，{what}没有运行")),
        _ => None,
    }
}

/// Run the agent's status command; the conclusion and, if it is not clear, why.
fn auth(agent: Agent, launcher: &Path, env: &ProbeEnv) -> (Auth, Option<String>) {
    let provider = (agent == Agent::Pi).then(pi_provider).flatten();
    let args: Vec<&str> = match agent {
        Agent::Codex => vec!["login", "status"],
        Agent::Claude => vec!["auth", "status", "--json"],
        Agent::Pi => match provider.as_deref() {
            Some(p) => vec!["auth", "check", "--provider", p, "--json", "--no-refresh"],
            None => {
                return (
                    Auth::Unknown,
                    Some("pi 没有默认 provider，无法在不调用模型的情况下检查；运行 pi auth check --provider <provider> 或 hoh doctor --live".to_owned()),
                );
            }
        },
    };
    match probe::run_status(launcher, &args, env) {
        Ok(out) => match conclusion(agent, &out) {
            Some(Auth::Missing) => (Auth::Missing, Some(login(agent, provider.as_deref()))),
            Some(found) => (found, None),
            None => (
                Auth::Unknown,
                Some("状态命令的输出无法判断登录状态".to_owned()),
            ),
        },
        Err(e) => (
            Auth::Unknown,
            Some(unfinished("状态命令", &e).unwrap_or_else(|| "状态命令无法运行".to_owned())),
        ),
    }
}

/// One agent's item, without `--live`.
pub fn agent(agent: Agent, env: &ProbeEnv, manifest: Option<&CompatManifest>) -> AgentItem {
    let launcher = probe::find_on_path(agent.command(), &env.path);
    let entry = match probe::probe(agent, env) {
        Ok(entry) => entry,
        Err(ProbeError::NotFound { .. }) => return AgentItem::missing(install(agent).to_owned()),
        Err(e) => {
            let (protocol, detail, remedy) = match &e {
                ProbeError::Unsupported { detail, .. } => (
                    Protocol::Incompatible,
                    format!("缺少 HoH 需要的接口：{detail}"),
                    format!("升级 {} 到受支持的版本", agent.command()),
                ),
                ProbeError::TimedOut { .. } | ProbeError::NotStarted { .. } => (
                    Protocol::Unknown,
                    unfinished("探测命令", &e).unwrap_or_default(),
                    "检查该 CLI 能否正常启动后重试".to_owned(),
                ),
                _ => (
                    Protocol::Unknown,
                    format!("探测失败：{e}"),
                    "检查该 CLI 能否正常运行后重试".to_owned(),
                ),
            };
            let mut item = AgentItem::missing(remedy);
            item.installed = true;
            item.path = launcher.map(|p| p.display().to_string());
            item.protocol = protocol;
            item.protocol_detail = Some(detail);
            return item;
        }
    };
    let (protocol, protocol_detail) = protocol(&entry, manifest, agent);
    let (auth_check, auth_note) = auth(agent, Path::new(&entry.launcher_path), env);
    let remedy = match (protocol, auth_note) {
        (_, Some(note)) => Some(note),
        (Protocol::Unknown, None) => {
            Some("在没有写任务时运行兼容套件，登记该版本后再用于新 attempt".to_owned())
        }
        _ => None,
    };
    AgentItem {
        installed: true,
        path: Some(entry.launcher_path),
        version: Some(entry.cli_version),
        binary_sha256: Some(entry.implementation_sha256),
        protocol,
        protocol_detail,
        auth_check,
        capabilities: entry.capabilities,
        remedy,
        live: None,
    }
}
