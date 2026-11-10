//! From probe results, the compat manifest and native status commands to one agent's item (ADR-015 decisions 2 and
//! 3). Status output is only read for its conclusion and never shown.

use super::report::{AgentItem, Auth, Protocol};
use hoh_agents::compat::{Agent, AgentEntry, CompatManifest, SuiteStatus, Transport};
use hoh_agents::login::{self, Login};
use hoh_agents::probe::{self, Captured, ProbeEnv, ProbeError};

use std::path::Path;

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

/// The conclusion of a native status command (ADR-015 decision 3); `None` when it says nothing clear.
pub fn conclusion(agent: Agent, out: &Captured) -> Option<Auth> {
    login::conclusion(agent, out).map(|found| match found {
        Login::LoggedIn => Auth::NotVerified,
        Login::Missing => Auth::Missing,
    })
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
pub(super) fn auth(agent: Agent, launcher: &Path, env: &ProbeEnv) -> (Auth, Option<String>) {
    match login::status(agent, launcher, env) {
        Ok((out, provider)) => match conclusion(agent, &out) {
            Some(Auth::Missing) => (Auth::Missing, Some(login::hint(agent, provider.as_deref()))),
            Some(found) => (found, None),
            None => (
                Auth::Unknown,
                Some("状态命令的输出无法判断登录状态".to_owned()),
            ),
        },
        Err(None) => (Auth::Unknown, Some(login::NO_PI_PROVIDER.to_owned())),
        Err(Some(e)) => (
            Auth::Unknown,
            Some(unfinished("状态命令", &e).unwrap_or_else(|| "状态命令无法运行".to_owned())),
        ),
    }
}

/// The item of an installed agent whose probe was refused or failed, with what was found before it: the version and
/// the implementation's hash (REQ-006; M1QA-08).
fn not_probed(
    agent: Agent,
    env: &ProbeEnv,
    identity: Option<probe::Identity>,
    e: &ProbeError,
) -> AgentItem {
    let (protocol, detail, remedy) = match e {
        ProbeError::Unsupported { detail, .. } => (
            Protocol::Incompatible,
            format!("缺少 HoH 需要的接口：{detail}"),
            format!("升级 {} 到受支持的版本", agent.command()),
        ),
        ProbeError::TimedOut { .. } | ProbeError::NotStarted { .. } => (
            Protocol::Unknown,
            unfinished("探测命令", e).unwrap_or_default(),
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
    item.path = probe::find_on_path(agent.command(), &env.path).map(|p| p.display().to_string());
    if let Some(identity) = identity {
        item.version = Some(identity.cli_version);
        item.binary_sha256 = Some(identity.implementation_sha256);
    }
    item.protocol = protocol;
    item.protocol_detail = Some(detail);
    item
}

/// One agent's item, without `--live`.
pub fn agent(agent: Agent, env: &ProbeEnv, manifest: Option<&CompatManifest>) -> AgentItem {
    let identity = match probe::identify(agent, env) {
        Ok(identity) => identity,
        Err(ProbeError::NotFound { .. }) => return AgentItem::missing(install(agent).to_owned()),
        Err(e) => return not_probed(agent, env, None, &e),
    };
    let entry = match probe::probe_identified(agent, identity.clone(), env) {
        Ok(entry) => entry,
        Err(e) => return not_probed(agent, env, Some(identity), &e),
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
