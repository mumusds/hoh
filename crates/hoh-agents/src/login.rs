//! An agent's login as its own status command tells it, without a model call (REQ-006, ADR-015 decision 3): what
//! `hoh doctor` reports and what the host's registry asks before a role is changed to that agent (REQ-019 failure
//! mode 3, docs/adr/ADR-024.md decision 7). Status output is only read for its conclusion and never shown.

use crate::compat::Agent;
use crate::probe::{self, Captured, ProbeEnv, ProbeError};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// What a status command concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Login {
    /// It says logged in (not verified with a model call).
    LoggedIn,
    /// It says not logged in.
    Missing,
}

/// What logging in takes (HoH never logs in itself).
pub fn hint(agent: Agent, provider: Option<&str>) -> String {
    match agent {
        Agent::Codex => "运行 codex login 登录".to_owned(),
        Agent::Claude => "运行 claude auth login 登录".to_owned(),
        Agent::Pi => format!(
            "为 pi 的 provider {} 配置凭证（pi 中使用 /login，或设置该 provider 的 API key），再用 pi auth check 确认",
            provider.unwrap_or("（未设置默认 provider）")
        ),
    }
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
pub fn conclusion(agent: Agent, out: &Captured) -> Option<Login> {
    match agent {
        Agent::Codex => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if text.contains("Not logged in") {
                Some(Login::Missing)
            } else if out.status.success()
                && text
                    .lines()
                    .any(|l| l.trim_start().starts_with("Logged in"))
            {
                Some(Login::LoggedIn)
            } else {
                None
            }
        }
        // A positive answer counts only from a command that succeeded; "not logged in" comes with a failing exit
        // (M1T07R-12).
        Agent::Claude => match json_object(&out.stdout)?.get("loggedIn")?.as_bool()? {
            true if out.status.success() => Some(Login::LoggedIn),
            true => None,
            false => Some(Login::Missing),
        },
        Agent::Pi => {
            let answer = json_object(&out.stdout)?;
            match (answer["status"].as_str()?, answer["reason"].as_str()) {
                ("ready", _) if out.status.success() => Some(Login::LoggedIn),
                ("not_ready", Some("credentials_not_configured")) => Some(Login::Missing),
                _ => None,
            }
        }
    }
}

/// Where pi keeps its settings: `$PI_CODING_AGENT_DIR`, else `~/.pi/agent`.
pub fn pi_agent_dir() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(".pi/agent")))
}

/// pi's default provider, from its settings in `dir` (`settings.json`, not its credentials).
pub fn pi_provider(dir: &Path) -> Option<String> {
    let settings: Value =
        serde_json::from_slice(&std::fs::read(dir.join("settings.json")).ok()?).ok()?;
    settings["defaultProvider"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
}

/// Why a status command could not be run for `agent`: pi without a default provider has none that needs no model call.
pub const NO_PI_PROVIDER: &str = "pi 没有默认 provider，无法在不调用模型的情况下检查；运行 pi auth check --provider <provider> 或 hoh doctor --live";

/// Run `agent`'s status command (`launcher`, under `env`'s limits and stop): its output, the provider it checked (pi,
/// from the settings in `pi_dir`), or `Err(None)` when there is no command to run (pi without a default provider),
/// `Err(Some(e))` when it failed.
pub fn status(
    agent: Agent,
    launcher: &Path,
    env: &ProbeEnv,
    pi_dir: Option<&Path>,
) -> Result<(Captured, Option<String>), Option<ProbeError>> {
    let provider = if agent == Agent::Pi {
        pi_dir.and_then(pi_provider)
    } else {
        None
    };
    let args: Vec<&str> = match agent {
        Agent::Codex => vec!["login", "status"],
        Agent::Claude => vec!["auth", "status", "--json"],
        Agent::Pi => match provider.as_deref() {
            Some(p) => vec!["auth", "check", "--provider", p, "--json", "--no-refresh"],
            None => return Err(None),
        },
    };
    probe::run_status(launcher, &args, env)
        .map(|out| (out, provider.clone()))
        .map_err(Some)
}

/// What logging in takes when `agent`'s status command says it is not logged in; `None` when it says it is, says
/// nothing clear, or could not be run (the registry then lets a change go ahead: only a clear "not logged in" refuses).
pub fn missing(
    agent: Agent,
    launcher: &Path,
    env: &ProbeEnv,
    pi_dir: Option<&Path>,
) -> Option<String> {
    let (out, provider) = status(agent, launcher, env, pi_dir).ok()?;
    (conclusion(agent, &out)? == Login::Missing).then(|| hint(agent, provider.as_deref()))
}
