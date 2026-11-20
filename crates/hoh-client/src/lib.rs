//! The HoH client (REQ-001..REQ-006): bare `hoh` (the TUI), `hoh -p`, `hoh attach`, `hoh doctor`, help and version, as
//! the `hoh` binary runs them ([`run`]). It parses the command line and refuses a usage error with exit 64 before
//! touching anything (no host, no `.hoh/`, no database, stdout empty). `hoh web` arrives with a later task; until then it
//! reports NOT_IMPLEMENTED on stderr and exits 3, so no script can mistake it for working.
//!
//! A crate of its own that does not depend on the host's crate (`hoh-app`): client code cannot name the host, the
//! verifier, the agent gate or the compat registration at all, and starts processes only through its signal state's
//! one start gate (docs/adr/ADR-031.md, 设计修正 2, the corrections after its second implementation review).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

/// A line on stderr, whatever becomes of it: `eprintln!` panics when the write fails (a hung-up terminal, a full
/// device), which would turn the exit code into 101 (M1QA-01). Once a client's signal state is registered the line goes
/// to its one stderr writer, before that straight to fd 2 (docs/adr/ADR-031.md, 设计修正 2, C). Defined before the
/// modules, so they can use it.
macro_rules! say {
    ($($arg:tt)*) => {
        crate::stderr::say(&format!($($arg)*))
    };
}

mod cli;
mod client;
pub mod debug_hold;
mod doctor;
pub mod exit;
mod follow;
mod forward;
mod help;
mod output;
mod prompt;
mod session;
mod signals;
mod stderr;
mod tui;

use cli::Invocation;
use exit::{ClientEvent, Ending};
use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::path::Path;

pub use client::manager_that_lingers;

/// `text` as a line on stderr: through the client's one stderr writer once its signal state is registered, else (before
/// it, or in a process that has none) straight to fd 2.
pub fn say(text: &str) {
    stderr::say(text);
}

/// Exit code of a usage error.
pub fn usage_code() -> u8 {
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

pub fn run(args: &[OsString]) -> u8 {
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
            detach,
            resume,
            project,
        } => {
            if detach && output == cli::Output::StreamJson {
                return refuse(
                    "--detach 只输出 run_id（text）或受理结果（json），不能与 --output stream-json 同用",
                );
            }
            return match here(project.as_deref()) {
                Ok(dir) => prompt::run(&dir, &text, output, detach, resume.as_deref()),
                Err(code) => code,
            };
        }
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
