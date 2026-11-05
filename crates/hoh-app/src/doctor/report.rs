//! What `hoh doctor` reports, and its two forms: one JSON object on one line, or Chinese text (ADR-015 decision 8).

use hoh_agents::probe::Run;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// Whether HoH can drive a CLI (REQ-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Protocol {
    Compatible,
    /// Only `exec` works (codex without app-server).
    Degraded,
    Incompatible,
    Unknown,
}

/// What is known about a CLI's login (REQ-006).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Auth {
    /// Only from `--live`.
    Verified,
    NotVerified,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentItem {
    pub installed: bool,
    pub path: Option<String>,
    /// The first line of `--version`, verbatim.
    pub version: Option<String>,
    /// The implementation's sha256 (not the launcher's).
    pub binary_sha256: Option<String>,
    pub protocol: Protocol,
    pub protocol_detail: Option<String>,
    pub auth_check: Auth,
    pub capabilities: BTreeMap<String, Option<bool>>,
    pub remedy: Option<String>,
    /// The live check, with `--live`.
    pub live: Option<Value>,
}

impl AgentItem {
    pub fn missing(remedy: String) -> AgentItem {
        AgentItem {
            installed: false,
            path: None,
            version: None,
            binary_sha256: None,
            protocol: Protocol::Unknown,
            protocol_detail: None,
            auth_check: Auth::Unknown,
            capabilities: BTreeMap::new(),
            remedy: Some(remedy),
            live: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tool {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    /// Compatible: a version new enough; incompatible: one known to be too old; unknown: none could be read.
    pub protocol: Protocol,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Plugin {
    pub id: String,
    pub package: String,
    pub version: Option<String>,
    /// Listed by `pi list --no-approve`; `None` when that could not be asked.
    pub listed: Option<bool>,
    /// Not measured yet (REQ-006).
    pub runtime: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HohDir {
    pub path: String,
    pub exists: bool,
    /// It can hold the project's state.
    pub writable: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Runtime {
    pub git: Tool,
    pub node: Tool,
    pub pi_plugins: Vec<Plugin>,
    pub project_hoh: HohDir,
    /// M1 has no host (ADR-015 decision 7).
    pub host: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Required items that make the exit code 3.
    pub blocking: Vec<String>,
    /// Agents whose login or protocol is not known to work.
    pub unverified: Vec<String>,
    pub warnings: Vec<String>,
    pub exit_code: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub schema_version: &'static str,
    pub mode: &'static str,
    pub agents: BTreeMap<&'static str, AgentItem>,
    pub runtime: Runtime,
    pub summary: Summary,
    /// Every command doctor ran.
    pub probes: Vec<Run>,
}

/// One JSON object and one LF; U+2028 and U+2029 escaped (REQ-004).
pub fn json(report: &Report) -> String {
    let text = serde_json::to_string(report).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"));
    format!(
        "{}\n",
        text.replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029")
    )
}

fn word<T: Serialize>(value: T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The same report for people.
pub fn text(report: &Report) -> String {
    let mut out = format!(
        "HoH doctor（{}）\n",
        if report.mode == "live" {
            "live：做了最小原生调用"
        } else {
            "默认：不调用模型；退出码 0 只表示没有阻断项，不表示已认证"
        }
    );
    for (name, item) in &report.agents {
        if item.installed {
            out.push_str(&format!(
                "{name:<7} {}  protocol={}  auth={}\n",
                item.version.as_deref().unwrap_or("版本未知"),
                word(item.protocol),
                word(item.auth_check)
            ));
        } else {
            out.push_str(&format!("{name:<7} 未安装\n"));
        }
        if let Some(path) = &item.path {
            out.push_str(&format!("        path={path}\n"));
        }
        if let Some(sha) = &item.binary_sha256 {
            out.push_str(&format!("        binary_sha256={sha}\n"));
        }
        if !item.capabilities.is_empty() {
            let caps: Vec<String> = item
                .capabilities
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.map_or("unknown", |b| if b { "true" } else { "false" })
                    )
                })
                .collect();
            out.push_str(&format!("        capabilities: {}\n", caps.join(" ")));
        }
        if let Some(live) = &item.live {
            out.push_str(&format!(
                "        live：attempt {} {}，原生会话 {}{}\n",
                live["attempt_id"].as_str().unwrap_or("?"),
                live["state"].as_str().unwrap_or("未开始"),
                live["native_session_id"].as_str().unwrap_or("无"),
                live["error"]
                    .as_str()
                    .map(|e| format!("，错误：{e}"))
                    .unwrap_or_default()
            ));
        }
        if let Some(detail) = &item.protocol_detail {
            out.push_str(&format!("        {detail}\n"));
        }
        if let Some(remedy) = &item.remedy {
            out.push_str(&format!("        处理：{remedy}\n"));
        }
    }
    let r = &report.runtime;
    for (name, tool) in [("git", &r.git), ("node", &r.node)] {
        out.push_str(&format!(
            "{name:<7} {}{}\n",
            match (tool.installed, tool.version.as_deref()) {
                (false, _) => "未安装",
                (true, Some(v)) => v,
                (true, None) => "版本未知",
            },
            tool.detail
                .as_deref()
                .map(|d| format!("（{d}）"))
                .unwrap_or_default()
        ));
    }
    for p in &r.pi_plugins {
        let listed = match p.listed {
            Some(true) => "已安装",
            Some(false) => "pi 未列出",
            None => "无法查询",
        };
        out.push_str(&format!(
            "Pi 插件 {}（{}）：{listed}，运行兼容性 {}\n",
            p.id, p.package, p.runtime
        ));
    }
    out.push_str(&format!(
        ".hoh    {}：{}{}\n",
        r.project_hoh.path,
        if r.project_hoh.writable {
            "可写"
        } else {
            "不可写"
        },
        r.project_hoh
            .detail
            .as_deref()
            .map(|d| format!("（{d}）"))
            .unwrap_or_default()
    ));
    out.push_str("host    未运行（M1 还没有 host）\n");
    let s = &report.summary;
    if s.blocking.is_empty() {
        out.push_str("结论：没有阻断项");
    } else {
        out.push_str(&format!(
            "结论：{} 个阻断项：{}",
            s.blocking.len(),
            s.blocking.join("；")
        ));
    }
    if !s.unverified.is_empty() {
        out.push_str(&format!("；未验证：{}", s.unverified.join("、")));
    }
    out.push('\n');
    for w in &s.warnings {
        out.push_str(&format!("注意：{w}\n"));
    }
    out
}
