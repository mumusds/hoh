//! `hoh doctor` (REQ-006; ADR-015): what is installed, whether HoH can drive it, and what is known about each login,
//! without a model call unless `--live` is given. It never reads credential files or credential variables and never
//! shows a status command's output, only its conclusion. Every external command has a 20 s limit and runs in its own
//! process group; the agents and tools are probed in parallel. The exit code is 3 if a required item is not installed,
//! incompatible or has no login, else 0, which does not mean logged in.

mod assess;
mod live;
mod report;
#[cfg(test)]
mod tests;

use hoh_agents::compat::{Agent, CompatManifest};
use hoh_agents::probe::{self, ProbeEnv, RunLog};
use report::{AgentItem, Auth, HohDir, Plugin, Protocol, Report, Runtime, Summary, Tool};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// The limit of each external command (REQ-006).
const TIMEOUT: Duration = Duration::from_secs(20);

/// The project's plugin set (REQ-048..050).
const PLUGINS: &str = include_str!("../../../config/plugins.json");

/// The stated limits of an attempt's termination proof, in every mode (REQ-043 decision 3, CR-012; the owner accepted
/// them on 2026-11-08).
const TRACKING_LIMITS: &str = "终结证明只覆盖 Attempt 自己的进程：它的 cgroup（受委托时）、进程组和继承的环境标记。\
同一用户的进程把工作交给 Attempt 之外早已运行的服务（systemd --user、HoH 之外启动的 tmux server、cron、ssh localhost），\
或迁出 cgroup、脱离进程组并隐藏标记时，无法归属，可能在 Attempt 结束、写者租约释放后继续写（REQ-043 决定 3，CR-012）";

/// The compat manifest of the project, if there is a readable one (ADR-015 decision 2); a warning otherwise.
fn manifest(project: &Path) -> (Option<CompatManifest>, Option<String>) {
    let path = project.join(".hoh/compat-manifest.json");
    match std::fs::read(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(e) => (
            None,
            Some(format!(
                "compat manifest 无法读取（{}）：{e}",
                path.display()
            )),
        ),
        Ok(bytes) => match serde_json::from_slice::<CompatManifest>(&bytes) {
            Ok(m) => (Some(m), None),
            Err(e) => (
                None,
                Some(format!(
                    "compat manifest 无法解析（{}）：{e}",
                    path.display()
                )),
            ),
        },
    }
}

/// `<tool> --version`'s first line; `minimum` (major, minor) for node.
fn tool(name: &str, env: &ProbeEnv, minimum: Option<(u64, u64)>) -> Tool {
    let Some(path) = probe::find_on_path(name, &env.path) else {
        return Tool {
            installed: false,
            path: None,
            version: None,
            protocol: Protocol::Unknown,
            detail: Some(format!("{name} 未安装")),
        };
    };
    // Only a version known to be too old is incompatible; one that cannot be read is unknown (M1T07R-03).
    let (version, unread) = match probe::run_status(&path, &["--version"], env) {
        Ok(out) if out.status.success() => (
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .next()
                .map(str::to_owned),
            format!("{name} --version 没有给出版本"),
        ),
        Ok(out) => (None, format!("{name} --version 以 {} 结束", out.status)),
        Err(e) => (
            None,
            assess::unfinished(&format!("{name} --version"), &e)
                .unwrap_or_else(|| format!("{name} --version 无法运行：{e}")),
        ),
    };
    let (protocol, detail) = match (minimum, version.as_deref()) {
        (_, None) => (Protocol::Unknown, Some(unread)),
        (None, Some(_)) => (Protocol::Compatible, None),
        (Some((major, minor)), Some(v)) => {
            let mut parts = v
                .trim_start_matches('v')
                .split('.')
                .map(|p| p.parse::<u64>().ok());
            match (parts.next().flatten(), parts.next().flatten()) {
                (Some(a), Some(b)) if (a, b) >= (major, minor) => (
                    Protocol::Compatible,
                    Some(format!("需要 ≥ {major}.{minor}")),
                ),
                (Some(_), Some(_)) => (
                    Protocol::Incompatible,
                    Some(format!("需要 ≥ {major}.{minor}（pi 需要）")),
                ),
                _ => (Protocol::Unknown, Some(format!("无法从 {v:?} 读出版本"))),
            }
        }
    };
    Tool {
        installed: true,
        path: Some(path.display().to_string()),
        version,
        protocol,
        detail,
    }
}

/// The three plugins and whether `pi list --no-approve` lists them (ADR-015 decision 6).
fn plugins(pi: Option<&str>, env: &ProbeEnv) -> Vec<Plugin> {
    let listed = pi.and_then(|launcher| {
        probe::run_status(Path::new(launcher), &["list", "--no-approve"], env)
            .ok()
            .filter(|out| out.status.success())
            .map(|out| {
                format!(
                    "{}\n{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                )
            })
    });
    let config: Value = serde_json::from_str(PLUGINS).unwrap_or(Value::Null);
    config["plugins"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let package = p["package"].as_str().unwrap_or_default().to_owned();
            Plugin {
                id: p["id"].as_str().unwrap_or_default().to_owned(),
                listed: listed
                    .as_ref()
                    .map(|text| !package.is_empty() && text.contains(&package)),
                package,
                version: p["version"].as_str().map(str::to_owned),
                runtime: "unknown",
            }
        })
        .collect()
}

/// The project's `.hoh/`: a directory that can be written, or nothing yet in a project that can be (M1T07R-06). A
/// file, or a link that leads nowhere or to a file, in its place cannot hold the project's state.
fn hoh_dir(project: &Path) -> HohDir {
    let path = project.join(".hoh");
    let writable = |dir: &Path| rustix::fs::access(dir, rustix::fs::Access::WRITE_OK).is_ok();
    let (exists, ok, detail) = match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (false, writable(project), None),
        Err(e) => (true, false, Some(format!("无法查看：{e}"))),
        Ok(_) => match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => (true, writable(&path), None),
            Ok(_) => (
                true,
                false,
                Some("这里是文件，不是目录；移走它后 HoH 才能保存状态".to_owned()),
            ),
            Err(_) => (
                true,
                false,
                Some("这里是指向不存在位置的符号链接；移走它后 HoH 才能保存状态".to_owned()),
            ),
        },
    };
    HohDir {
        path: path.display().to_string(),
        exists,
        writable: ok,
        detail,
    }
}

/// The summary and exit code (REQ-006): 3 if a required item is not installed, is incompatible or has no login.
fn summary(
    agents: &BTreeMap<&'static str, AgentItem>,
    runtime: &Runtime,
    mut warnings: Vec<String>,
) -> Summary {
    let mut blocking = Vec::new();
    let mut unverified = Vec::new();
    for (name, item) in agents {
        if !item.installed {
            blocking.push(format!("{name} 未安装"));
        }
        if item.protocol == Protocol::Incompatible {
            blocking.push(format!("{name} protocol=incompatible"));
        }
        if item.auth_check == Auth::Missing {
            blocking.push(format!("{name} 未登录"));
        }
        if item.installed
            && (matches!(item.auth_check, Auth::Unknown | Auth::NotVerified)
                || item.protocol == Protocol::Unknown)
        {
            unverified.push((*name).to_owned());
        }
    }
    for (name, t) in [("git", &runtime.git), ("node", &runtime.node)] {
        if !t.installed {
            blocking.push(format!("{name} 未安装"));
        } else if t.protocol == Protocol::Incompatible {
            blocking.push(format!(
                "{name} protocol=incompatible（{}）",
                t.detail.as_deref().unwrap_or("版本不符")
            ));
        } else if t.protocol == Protocol::Unknown {
            warnings.push(format!(
                "{name} 的版本未能确认：{}",
                t.detail.as_deref().unwrap_or("原因未知")
            ));
        }
    }
    if !runtime.project_hoh.writable {
        warnings.push(format!(
            "{} 不可写：HoH 无法保存本项目的状态{}",
            runtime.project_hoh.path,
            runtime
                .project_hoh
                .detail
                .as_deref()
                .map(|d| format!("（{d}）"))
                .unwrap_or_default()
        ));
    }
    if runtime.pi_plugins.iter().any(|p| p.listed == Some(false)) {
        warnings.push(
            "有 Pi 插件未被 pi list --no-approve 列出（项目本地安装不在其中）；插件可降级，不影响退出码".to_owned(),
        );
    }
    let exit_code = if blocking.is_empty() { 0 } else { 3 };
    Summary {
        blocking,
        unverified,
        warnings,
        exit_code,
    }
}

/// Everything, with the live checks if `live`.
fn examine(project: &Path, live: bool) -> Report {
    let log: RunLog = RunLog::default();
    let env = ProbeEnv {
        path: std::env::var_os("PATH").unwrap_or_default(),
        fixture_root: None,
        timeout: TIMEOUT,
        log: Some(log.clone()),
        // One limit for all commands, also those run one after another (M1T07R-02).
        deadline: Some(Instant::now() + TIMEOUT),
    };
    let (manifest, warning) = manifest(project);
    let mut warnings: Vec<String> = warning.into_iter().collect();
    let (mut agents, git, node, pi_plugins) = std::thread::scope(|scope| {
        let probe_agent = |agent: Agent| {
            let env = &env;
            let manifest = manifest.as_ref();
            scope.spawn(move || assess::agent(agent, env, manifest))
        };
        let codex = probe_agent(Agent::Codex);
        let claude = probe_agent(Agent::Claude);
        let pi = probe_agent(Agent::Pi);
        // Alongside pi's probe, not after it (M1T07R-02).
        let pi_plugins = scope.spawn(|| {
            let launcher = probe::find_on_path("pi", &env.path);
            plugins(launcher.as_deref().and_then(Path::to_str), &env)
        });
        let git = scope.spawn(|| tool("git", &env, None));
        let node = scope.spawn(|| tool("node", &env, Some((22, 19))));
        let lost = |what: &str| AgentItem::missing(format!("{what} 的探测线程异常结束"));
        let pi = pi.join().unwrap_or_else(|_| lost("pi"));
        let plugins = pi_plugins.join().unwrap_or_default();
        let gone = |name: &str| Tool {
            installed: false,
            path: None,
            version: None,
            protocol: Protocol::Unknown,
            detail: Some(format!("{name} 的探测线程异常结束")),
        };
        (
            BTreeMap::from([
                ("codex", codex.join().unwrap_or_else(|_| lost("codex"))),
                ("claude", claude.join().unwrap_or_else(|_| lost("claude"))),
                ("pi", pi),
            ]),
            git.join().unwrap_or_else(|_| gone("git")),
            node.join().unwrap_or_else(|_| gone("node")),
            plugins,
        )
    });
    if live {
        // The live turns' preparation gets a limit of its own: the default probes may have used up theirs
        // (M1T07R-07). Each turn itself is bounded by its adapter's deadline.
        let live_env = ProbeEnv {
            deadline: Some(Instant::now() + TIMEOUT),
            ..env.clone()
        };
        match live::check(&mut agents, &live_env) {
            Ok(dir) => warnings.push(format!(
                "live 检查的原始日志与临时 store 保留在 {}",
                dir.display()
            )),
            Err(why) => warnings.push(format!("live 检查没有完成：{why}")),
        }
    }
    // How the project's host tracks its attempts' processes (docs/adr/ADR-021.md decision 8d, CR-011): a running
    // host's own record; with none running, what the check before a start finds, not verified (M2T06R-402).
    let running = crate::client::running(project).and_then(|_| {
        std::fs::read_to_string(project.join(".hoh/host.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    });
    let tracking = match running {
        Some(record) => {
            let mode = record["process_tracking"]
                .as_str()
                .unwrap_or("unknown")
                .to_owned();
            let why = record["process_tracking_reason"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            if mode != "cgroup" {
                warnings.push(format!(
                    "进程跟踪降级：运行中的 host {why}；它按 /proc 尽力跟踪 Attempt 的组外进程（tracking: proc，对蓄意躲避的进程不成立，CR-011）"
                ));
            }
            json!({"state": "running", "process_tracking": mode, "process_tracking_detail": why})
        }
        None => match crate::client::delegated_scope() {
            Ok(_) => json!({"process_tracking": "unverified",
                "process_tracking_detail": "没有运行中的 host；用户级 systemd 管理器可达且有 systemd-run，下次启动的 host 预计在受委托的 cgroup 中运行（未经验证）"}),
            Err(why) => {
                warnings.push(format!(
                    "进程跟踪将降级：{why}；下次启动的 host 将按 /proc 尽力跟踪 Attempt 的组外进程（tracking: proc，对蓄意躲避的进程不成立，CR-011）"
                ));
                json!({"process_tracking": "proc", "process_tracking_detail": why})
            }
        },
    };
    let mut host = json!({"state": "not_running", "detail": "没有运行中的 host",
        "process_tracking_limits": TRACKING_LIMITS});
    if let (Some(host), Some(tracking)) = (host.as_object_mut(), tracking.as_object()) {
        host.extend(tracking.clone());
    }
    let runtime = Runtime {
        git,
        node,
        pi_plugins,
        project_hoh: hoh_dir(project),
        host,
    };
    let summary = summary(&agents, &runtime, warnings);
    let probes = log.lock().map(|runs| runs.clone()).unwrap_or_default();
    Report {
        schema_version: "1.0",
        mode: if live { "live" } else { "default" },
        agents,
        runtime,
        summary,
        probes,
    }
}

/// Run doctor for `project`; the exit code.
pub fn run(project: &Path, live: bool, json_output: bool) -> u8 {
    let report = examine(project, live);
    let text = if json_output {
        report::json(&report)
    } else {
        report::text(&report)
    };
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
    report.summary.exit_code
}
