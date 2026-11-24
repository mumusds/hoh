//! From probe results, the compat manifest and native status commands to one agent's item (ADR-015 decisions 2 and
//! 3). Status output is only read for its conclusion and never shown.

use super::report::{AgentItem, Auth, Protocol, ReadOnly, ReadOnlyParts};
use hoh_agents::compat::{self, Agent, AgentEntry, CompatManifest, SuiteStatus, Transport};
use hoh_agents::login::{self, Login};
use hoh_agents::probe::{self, Captured, ProbeEnv, ProbeError};
use hoh_core::staffing::Compat;

use std::path::Path;

/// What installing the CLI takes.
fn install(agent: Agent) -> &'static str {
    match agent {
        Agent::Codex => "安装 Codex CLI 后重试：npm install -g @openai/codex",
        Agent::Claude => {
            "安装 Claude Code 后重试：下载 https://claude.ai/install.sh 并运行 bash install.sh stable"
        }
        // Recorded suite traffic is 1.5.0; the installed CLI recorded in ADR-040 is 1.6.0 (PI16-B2).
        Agent::Pi => {
            "安装 pi 后重试：npm install -g @earendil-works/pi-coding-agent@1.6.0（兼容套件录制为 1.5.0）"
        }
    }
}

/// The protocol of a probed CLI against the compat manifest, judged by `compat::verdict` as the gate and staffing judge
/// it (docs/adr/ADR-030.md decision 9): compatible (or degraded, exec only) only if the manifest's entry for the agent
/// has the probe's whole identity, version and help and schema hashes and records a PASS of this build's compat suite;
/// anything else is unknown, the detail saying why.
pub fn protocol(
    entry: &AgentEntry,
    manifest: Option<&CompatManifest>,
    agent: Agent,
) -> (Protocol, Option<String>) {
    let why = match compat::verdict(manifest, agent.as_str(), entry, &compat::SUITE_SET_SHA256) {
        Compat::Verified if entry.transport == Transport::Exec => {
            return (
                Protocol::Degraded,
                Some("只有 exec 可用：没有 app-server（不能中途 steer）".to_owned()),
            );
        }
        Compat::Verified => return (Protocol::Compatible, None),
        Compat::Pending(why) => format!("该版本未经本构建的兼容套件验证（{why}）"),
        Compat::Failed(why) => format!("该版本的兼容套件未通过（{why}）"),
        Compat::Undecided(why) => format!("兼容套件无法判定该版本（{why}）"),
    };
    (Protocol::Unknown, Some(why))
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
    match login::status(agent, launcher, env, login::pi_agent_dir().as_deref()) {
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
/// the implementation's hash (REQ-006; M1QA-08), or the version alone when a hash was not done within doctor's limit.
fn not_probed(
    agent: Agent,
    env: &ProbeEnv,
    identity: Option<probe::Identity>,
    version: Option<String>,
    e: &ProbeError,
) -> AgentItem {
    let (protocol, detail, remedy) = match e {
        // A file of its identity not hashed within doctor's limit (package G2, docs/adr/ADR-015.md note): said as
        // such, never as a command that did not end.
        _ if let Some(path) = e.hashed() => (
            Protocol::Unknown,
            match e {
                ProbeError::TimedOut { after, .. } => format!(
                    "doctor 的总时限只剩 {:.1} s，{path} 的 sha256 没有在其中算完（见 hashes）；未能与 compat manifest 核对",
                    after.as_secs_f64()
                ),
                _ => format!(
                    "doctor 的总时限已用完，{path} 的 sha256 没有计算（见 hashes）；未能与 compat manifest 核对"
                ),
            },
            "在机器较空闲时重试：doctor 要在总时限内算完该 CLI 的 sha256".to_owned(),
        ),
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
    } else {
        item.version = version;
    }
    item.protocol = protocol;
    item.protocol_detail = Some(detail);
    item
}

/// One agent's item, without `--live`.
pub fn agent(agent: Agent, env: &ProbeEnv, manifest: Option<&CompatManifest>) -> AgentItem {
    let identity = match probe::identify_versioned(agent, env) {
        Ok(identity) => identity,
        Err((_, ProbeError::NotFound { .. })) => {
            return AgentItem::missing(install(agent).to_owned());
        }
        Err((version, e)) => return not_probed(agent, env, None, version, &e),
    };
    let entry = match probe::probe_identified(agent, identity.clone(), env) {
        Ok(entry) => entry,
        Err(e) => return not_probed(agent, env, Some(identity), None, &e),
    };
    let (protocol, protocol_detail) = protocol(&entry, manifest, agent);
    let (auth_check, auth_note) = auth(agent, Path::new(&entry.launcher_path), env);
    let remedy = remedy(protocol, auth_note);
    let read_only = read_only(agent, &entry, env, manifest);
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
        read_only: Some(read_only),
        live: None,
    }
}

/// What doctor tells the user to do for an agent: the login's note, when its login is not known to work, and the compat
/// suite's, when its protocol is unknown (M1QA-09).
pub(super) fn remedy(protocol: Protocol, auth_note: Option<String>) -> Option<String> {
    match (protocol, auth_note) {
        (Protocol::Unknown, Some(note)) => Some(format!("{note}；{SUITE_NOTE}")),
        (_, Some(note)) => Some(note),
        (Protocol::Unknown, None) => Some(SUITE_NOTE.to_owned()),
        _ => None,
    }
}

#[cfg(test)]
mod install_hint {
    use super::install;
    use hoh_agents::compat::Agent;

    #[test]
    fn pi_install_hint_names_recorded_1_5_0_and_installed_1_6_0() {
        // PI16-B2: the recorded suite is pi 1.5.0 and the installed CLI is 1.6.0 (ADR-040).
        let hint = install(Agent::Pi);
        assert!(hint.contains("@earendil-works/pi-coding-agent"), "{hint}");
        assert!(hint.contains("1.5.0"), "{hint}");
        assert!(hint.contains("1.6.0"), "{hint}");
    }
}

/// The compat suite's note for an agent whose protocol is unknown.
const SUITE_NOTE: &str = "host 在没有写租约时自动运行兼容套件；运行中的写任务结束后即登记该版本";

/// The Commander's read-only workspace profile on `agent`, probed as `entry` (docs/adr/ADR-032.md decisions 4 and 7):
/// the parts the registry judges it by, each by the registry's own function — the probe rule (the entry's), the
/// read-only builder on the tested help (`claude::read_only_missing`, `pi::read_only_missing`; codex's builds on its
/// App Server only), the compat manifest's read_only case for this version (`hoh_agents::compat::read_only_case`) and, for codex on
/// its App Server, this host's check of its sandbox (`probe::codex_sandbox_check`) — and `workspace` their Kleene
/// conjunction, as `granted` has it.
pub(super) fn read_only(
    agent: Agent,
    entry: &AgentEntry,
    env: &ProbeEnv,
    manifest: Option<&CompatManifest>,
) -> ReadOnly {
    const NAME: &str = "read_only_workspace";
    let launcher = Path::new(&entry.launcher_path);
    let probe = entry.capabilities.get(NAME).copied().flatten();
    let help = || hoh_agents::launch::stdout_of(launcher, &["--help"], env).ok();
    let builder = match agent {
        Agent::Codex => Some(entry.transport == Transport::AppServer),
        Agent::Claude => help().map(|h| hoh_agents::claude::read_only_missing(&h).is_none()),
        Agent::Pi => help().map(|h| hoh_agents::pi::read_only_missing(&h).is_none()),
    };
    let suite_case = hoh_agents::compat::read_only_case(
        manifest,
        agent,
        entry,
        compat::SUITE_SET_SHA256.as_str(),
    );
    let check = (agent == Agent::Codex && entry.transport == Transport::AppServer)
        .then(|| probe::codex_sandbox_check(launcher, env));
    let case_holds = match suite_case {
        Some(SuiteStatus::Pass) => Some(true),
        Some(SuiteStatus::Fail) => Some(false),
        Some(SuiteStatus::NotRun) | None => None,
    };
    let host = check.as_ref().map(|c| !c.withholds());
    let parts = [probe, builder, case_holds, host.or(Some(true))];
    let workspace = if parts.contains(&Some(false)) {
        Some(false)
    } else if parts.iter().all(Option::is_some) {
        Some(true)
    } else {
        None
    };
    let mut detail = String::new();
    if probe == Some(false) || builder == Some(false) {
        detail.push_str(match (agent, entry.transport) {
            (Agent::Codex, Transport::Exec) => {
                "只有 exec 可用：Commander 不经 codex exec 启动，codex 不被选为 Commander。"
            }
            _ => {
                "被测 help/schema 不提供只读档的选项：不被选为 Commander（没有全权限的 fallback）。"
            }
        });
    } else if case_holds == Some(false) {
        detail.push_str("兼容清单的 read_only 用例未通过：不被选为 Commander；其他职位照常可用。");
    } else if case_holds.is_none() {
        detail.push_str("兼容套件尚未验证该版本的只读档：结论未知，验证前不被选为 Commander。");
    }
    if let Some(check) = &check {
        detail.push_str(&match check {
            probe::SandboxCheck::Confined => "本机能建立 codex 的只读沙箱。".to_owned(),
            probe::SandboxCheck::Unconfined => {
                "本机检查观察到沙箱下不受限的写入：codex 不被选为 Commander。".to_owned()
            }
            probe::SandboxCheck::Unavailable(why) => format!(
                "本机无法建立 codex 的沙箱（{why}）：codex 不被选为 Commander；主机修好后 host 在下一次刷新时恢复。"
            ),
            probe::SandboxCheck::Unknown(why) => format!(
                "本机检查的结论为 unknown（{why}）：这台主机上没有沙箱是否建立的本机证据，codex 仍可当 Commander，host 每 60 秒重测（ADR-032 余项 R2）。"
            ),
        });
    }
    detail.push_str(match agent {
        Agent::Codex => {
            "只读由 codex 自己的操作系统沙箱强制：Commander 的线程以 sandbox read-only、approvalPolicy never 启动。\
             本机检查只报告这台主机能否建立 codex 的默认只读沙箱，不是 Commander 的线程本身。\
             用户自己配置的 MCP 服务器、钩子与账号启用的应用连接器在沙箱之外运行，不在保证之内（ADR-032 余项 R1）。\
             App Server 在 host 运行中降级为 exec 后，codex 在该 host 重启或换版本前不当 Commander；doctor 看不到运行中 host 的这一事实。"
        }
        Agent::Claude => {
            "只读只靠 Claude Code 进程内的工具集与权限判定（--tools Read,Grep,Glob、--allowedTools、--permission-mode dontAsk、\
             --strict-mcp-config），没有操作系统级屏障；用户设置里的钩子与插件照常运行（ADR-032 余项 R4）。工具名 Read、Grep、Glob 不在 help 里，\
             由发布检查实测（ADR-032 余项 R6）。"
        }
        Agent::Pi => {
            "只读只靠 pi 进程内按工具名的允许清单（--tools read,grep,find,ls、--no-mcp），没有操作系统级屏障（ADR-032 余项 R3）。\
             Commander 的 pi 会话不加载任何扩展（--no-extensions）：v1 的三个 Pi 插件服务于其他 Pi 职位，不进 Commander 的会话。"
        }
    });
    detail.push_str("同一用户的其他进程仍能写项目（ADR-032 余项 R7）。");
    ReadOnly {
        workspace,
        enforced_by: match agent {
            Agent::Codex => "os_sandbox",
            Agent::Claude | Agent::Pi => "tool_allowlist",
        },
        parts: ReadOnlyParts {
            probe,
            builder,
            suite_case,
            host_check: check
                .as_ref()
                .map(|c| serde_json::json!({"check": c.name(), "reason": c.reason()})),
        },
        check: check.as_ref().map(probe::SandboxCheck::name),
        detail,
    }
}
