//! `hoh` binary (REQ-003, REQ-004): parses the command line, answers help and version, and refuses a usage error with
//! exit 64 before touching anything (no host, no `.hoh/`, no database, stdout empty). Bare `hoh` opens the TUI shell
//! (`tui`, M1-T08). The other commands arrive with later tasks; until then each reports NOT_IMPLEMENTED on stderr and
//! exits 3, so no script can mistake the binary for a working product.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

/// A line on stderr, whatever becomes of it: `eprintln!` panics when the write fails (a hung-up terminal, a full
/// device), which would turn the exit code into 101 (M1QA-01). Defined before the modules, so they can use it.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($arg)*);
    }};
}

mod cli;
mod client;
mod doctor;
mod exit;
mod help;
mod session;
mod tui;

use cli::Invocation;
use exit::{ClientEvent, Ending};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Internal mode: the second process of the WAL self-test (REQ-062); not a user command and in no help text.
    if args.first().map(OsString::as_os_str)
        == Some(hoh_core::persistence::selftest::PROBE_ARG.as_ref())
    {
        std::process::exit(hoh_core::persistence::selftest::probe_main());
    }
    // Internal mode: the start gate an agent is started through (docs/adr/ADR-022.md decision 13); not a user command and
    // in no help text.
    if args.first().map(OsString::as_os_str) == Some("__gate".as_ref()) {
        std::process::exit(hoh_app::gate::main(&args[1..]));
    }
    // Internal mode: the host a client starts (docs/adr/ADR-021.md decision 1); not a user command and in no help text.
    if args.first().map(OsString::as_os_str) == Some("__host".as_ref()) {
        std::process::exit(i32::from(host(&args[1..])));
    }
    std::process::exit(i32::from(run(&args)));
}

/// `hoh __host <project>`: its own session before anything else (no terminal, so no shell's hang-up reaches it),
/// stdin and stdout from its starter (`/dev/null`), stderr to its starter until it is ready, then to `.hoh/host.log`.
fn host(args: &[OsString]) -> u8 {
    let [project] = args else {
        return usage_code();
    };
    let project = std::path::PathBuf::from(project);
    if !project.is_absolute() || !project.is_dir() {
        return usage_code();
    }
    if rustix::process::getsid(None).ok() != Some(rustix::process::getpid())
        && let Err(e) = rustix::process::setsid()
    {
        say!(
            "hoh host: no session of its own ({e}); a hang-up of its starter's terminal is still ignored"
        );
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            say!("hoh host: no runtime: {e}");
            return 1;
        }
    };
    // The product's host: its runner runs the roles' agents found on its PATH, started through this binary's start gate
    // (docs/adr/ADR-022.md decision 17).
    let effective = match hoh_core::config::load(&project) {
        Ok(effective) => effective,
        Err(e) => {
            say!("hoh host: the project's configuration is not used: {e}");
            hoh_core::config::Config::default()
        }
    };
    let gate = std::env::current_exe().unwrap_or_else(|_| "hoh".into());
    let path = std::env::var_os("PATH").unwrap_or_default();
    let config = hoh_app::host::HostConfig::product(&project, effective, gate, path);
    let log = project.join(".hoh/host.log");
    let code = runtime.block_on(hoh_app::host::run_ready(config, move || {
        use std::os::unix::fs::OpenOptionsExt as _;
        if let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&log)
        {
            let _ = rustix::stdio::dup2_stderr(&file);
        }
    }));
    // The runner ended its registry's probe before it stopped (docs/adr/ADR-022.md decision 10); whatever blocking work
    // is left gets a bound, not a wait without end.
    runtime.shutdown_timeout(std::time::Duration::from_secs(2));
    code
}

/// Exit code of a usage error.
fn usage_code() -> u8 {
    exit::exit_code(Ending::Client(ClientEvent::Usage)).unwrap_or(64)
}

fn refuse(message: &str) -> u8 {
    say!("hoh: {message}\n{}", help::SYNOPSIS);
    usage_code()
}

/// Print a result; a closed stdout (e.g. `| head -1`) is not an error of the command.
fn print(text: &str) -> u8 {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
    0
}

fn project(invocation: &Invocation) -> Option<&Path> {
    match invocation {
        Invocation::Workspace { project }
        | Invocation::Prompt { project, .. }
        | Invocation::Attach { project, .. }
        | Invocation::Web { project }
        | Invocation::Doctor { project, .. }
        | Invocation::Help { project, .. } => project.as_deref(),
        Invocation::Version => None,
    }
}

/// Bare `hoh`: the TUI only with a terminal on both stdin and stdout (REQ-001); otherwise a usage error that reads
/// nothing and writes only stderr.
fn workspace(project: Option<&Path>) -> u8 {
    if !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal()) {
        return refuse(
            "hoh 只在 stdin 与 stdout 都是终端时打开 TUI 工作区；非交互运行请用 hoh -p '<文本>'",
        );
    }
    let dir = match project {
        Some(dir) => dir.to_path_buf(),
        None => match std::env::current_dir() {
            Ok(dir) => dir,
            Err(e) => return refuse(&format!("无法确定当前目录：{e}")),
        },
    };
    tui::run(&dir.canonicalize().unwrap_or(dir))
}

/// The project directory: `--project`, or the current one, canonical.
fn here(project: Option<&Path>) -> Result<std::path::PathBuf, u8> {
    let dir = match project.map_or_else(std::env::current_dir, |p| Ok(p.to_path_buf())) {
        Ok(dir) => dir,
        Err(e) => return Err(refuse(&format!("无法确定当前目录：{e}"))),
    };
    Ok(dir.canonicalize().unwrap_or(dir))
}

fn run(args: &[OsString]) -> u8 {
    let invocation = match cli::parse(args) {
        Ok(invocation) => invocation,
        Err(cli::UsageError(message)) => return refuse(&message),
    };
    // `--project` names an existing directory; nothing is ever created for it.
    if let Some(dir) = project(&invocation)
        && !dir.is_dir()
    {
        return refuse(&format!("--project 不是已有的目录：{}", dir.display()));
    }
    let what = match invocation {
        Invocation::Help { topic, .. } => return print(help::text(topic)),
        Invocation::Version => return print(&help::version()),
        Invocation::Workspace { project } => return workspace(project.as_deref()),
        Invocation::Prompt {
            text,
            output,
            detach: true,
            project,
            ..
        } => {
            if output == cli::Output::StreamJson {
                return refuse(
                    "--detach 只输出 run_id（text）或受理结果（json），不能与 --output stream-json 同用",
                );
            }
            return match here(project.as_deref()) {
                Ok(dir) => session::detach(&dir, &text, output),
                Err(code) => code,
            };
        }
        Invocation::Prompt { .. } => "非交互运行（hoh -p）",
        Invocation::Attach {
            run_id,
            output,
            project,
        } => {
            return match here(project.as_deref()) {
                Ok(dir) => session::attach(&dir, run_id.as_deref(), output),
                Err(code) => code,
            };
        }
        Invocation::Web { .. } => "hoh web",
        Invocation::Doctor {
            live,
            output,
            project,
        } => {
            if output == cli::Output::StreamJson {
                return refuse("hoh doctor 只支持 --output text 或 --output json");
            }
            let dir = match project.map_or_else(std::env::current_dir, Ok) {
                Ok(dir) => dir,
                Err(e) => return refuse(&format!("无法确定当前目录：{e}")),
            };
            return doctor::run(
                &dir.canonicalize().unwrap_or(dir),
                live,
                output == cli::Output::Json,
            );
        }
    };
    say!(
        "hoh {}: NOT_IMPLEMENTED: {what} 尚未实现（见 docs/08-development-plan.md）",
        env!("CARGO_PKG_VERSION")
    );
    3
}
