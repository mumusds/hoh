//! `hoh` binary (REQ-003, REQ-004): parses the command line, answers help and version, and refuses a usage error with
//! exit 64 before touching anything (no host, no `.hoh/`, no database, stdout empty). The commands themselves arrive
//! with later tasks; until then each reports NOT_IMPLEMENTED on stderr and exits 3, so no script can mistake the
//! binary for a working product.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod cli;
mod exit;
mod help;

use cli::Invocation;
use exit::{ClientEvent, Ending};
use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // Internal mode: the second process of the WAL self-test (REQ-062); not a user command and in no help text.
    if args.first().map(OsString::as_os_str)
        == Some(hoh_core::persistence::selftest::PROBE_ARG.as_ref())
    {
        std::process::exit(hoh_core::persistence::selftest::probe_main());
    }
    std::process::exit(i32::from(run(&args)));
}

/// Exit code of a usage error.
fn usage_code() -> u8 {
    exit::exit_code(Ending::Client(ClientEvent::Usage)).unwrap_or(64)
}

fn refuse(message: &str) -> u8 {
    eprintln!("hoh: {message}\n{}", help::SYNOPSIS);
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
        | Invocation::Doctor { project, .. } => project.as_deref(),
        Invocation::Help(_) | Invocation::Version => None,
    }
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
        Invocation::Help(topic) => return print(help::text(topic)),
        Invocation::Version => return print(&help::version()),
        Invocation::Workspace { .. } => "TUI 工作区（hoh）",
        Invocation::Prompt { .. } => "非交互运行（hoh -p）",
        Invocation::Attach { .. } => "hoh attach",
        Invocation::Web { .. } => "hoh web",
        Invocation::Doctor { .. } => "hoh doctor",
    };
    eprintln!(
        "hoh {}: NOT_IMPLEMENTED: {what} 尚未实现（见 docs/08-development-plan.md）",
        env!("CARGO_PKG_VERSION")
    );
    3
}
