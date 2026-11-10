//! `hoh __unit`: what the installer asks of the binary (docs/adr/ADR-023.md decision 5). `template` prints the unit
//! template for this user's home; `check <home> <root>...` says whether the unit can name that home's command and
//! each root, by the template's own rule (design correction 9); `enable <project>` writes the environment file of the
//! project's instance where the user's systemd manager reads it, and enables the instance. Not a user command and in
//! no help text.

use crate::exit::{ClientEvent, Ending, exit_code};
use hoh_app::unit;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one `systemctl` may take; it runs in a process group of its own, ended at this limit.
const SYSTEMCTL_LIMIT: Duration = Duration::from_secs(10);

pub fn main(args: &[OsString]) -> u8 {
    match args {
        [what] if what == "template" => template(),
        [what, project] if what == "enable" => enable(Path::new(project)),
        [what, home, roots @ ..] if what == "check" && !roots.is_empty() => check(home, roots),
        _ => fail(
            ClientEvent::Usage,
            "usage: hoh __unit template | hoh __unit check <home> <root>... | hoh __unit enable <project directory>",
        ),
    }
}

fn fail(event: ClientEvent, message: impl std::fmt::Display) -> u8 {
    say!("hoh __unit: {message}");
    exit_code(Ending::Client(event)).unwrap_or(1)
}

/// Print `text` on stdout; a failed write is a failure (the installer reads it).
fn print(text: &str) -> u8 {
    use std::io::Write as _;
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => 0,
        Err(e) => fail(
            ClientEvent::HostUnavailable,
            format!("cannot write stdout: {e}"),
        ),
    }
}

/// The root of the installation this binary is in (ADR-023 design correction 1), or why it is not an installed hoh.
fn root() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot find this program: {e}"))?;
    unit::installation_root(&exe).ok_or_else(|| {
        format!(
            "{} is not an installed hoh (<root>/versions/<version>/bin/hoh)",
            exe.display()
        )
    })
}

fn template() -> u8 {
    let Some(home) = std::env::var_os("HOME") else {
        return fail(ClientEvent::Config, "HOME is not set");
    };
    let root = match root() {
        Ok(root) => root,
        Err(why) => return fail(ClientEvent::Config, why),
    };
    match unit::template(Path::new(&home), &root) {
        Ok(text) => print(&text),
        Err(why) => fail(ClientEvent::Config, why),
    }
}

/// Whether the template can be made for `home` with each of `roots` (design correction 9, M2T14R-601): the one rule
/// for what the unit can name, asked by the installer before it writes anything of the installation; 78 and why when
/// it cannot.
fn check(home: &OsStr, roots: &[OsString]) -> u8 {
    for root in roots {
        if let Err(why) = unit::template(Path::new(home), Path::new(root)) {
            return fail(ClientEvent::Config, why);
        }
    }
    0
}

/// `name` in a directory of `path`.
fn on_path(name: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

/// `systemctl --user <args>`, bounded; its stdout, or what it said when it failed.
fn systemctl(program: &Path, args: &[&str], path: &OsStr) -> Result<String, String> {
    let env = hoh_agents::probe::ProbeEnv {
        path: path.to_owned(),
        fixture_root: None,
        timeout: SYSTEMCTL_LIMIT,
        log: None,
        deadline: None,
        cancel: None,
    };
    let mut argv = vec!["--user"];
    argv.extend_from_slice(args);
    let out = hoh_agents::probe::run_status(program, &argv, &env).map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "systemctl --user {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn enable(given: &Path) -> u8 {
    let project = match std::fs::canonicalize(given) {
        Ok(project) if project.is_dir() => project,
        _ => {
            return fail(
                ClientEvent::Usage,
                format!("{} is not a directory", given.display()),
            );
        }
    };
    // Enabled without lingering, the instance would stop with the user's last session (REQ-005 behaviour 2).
    if let Err(why) = crate::client::manager_that_lingers() {
        return fail(ClientEvent::HostUnavailable, why);
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    let Some(systemctl_program) = on_path("systemctl", &path) else {
        return fail(ClientEvent::HostUnavailable, "systemctl is not on PATH");
    };
    let name = unit::unit_name(&unit::instance(&project));
    // The instance's file is this installation's (design correction 1): the manager must read it; HoH writes no other.
    let root = match root() {
        Ok(root) => root,
        Err(why) => return fail(ClientEvent::Config, why),
    };
    let file = unit::instance_file(&root, &unit::instance(&project));
    let (shown, listed) = match systemctl(
        &systemctl_program,
        &["show", "-p", "LoadState,EnvironmentFiles", &name],
        &path,
    ) {
        Ok(text) => (
            unit::show(&text),
            unit::lists_environment_file(&text, &file),
        ),
        Err(why) => return fail(ClientEvent::HostUnavailable, why),
    };
    let load = shown.get("LoadState").map(String::as_str).unwrap_or("");
    if load != "loaded" {
        return fail(
            ClientEvent::HostUnavailable,
            format!(
                "the user's systemd manager does not load {name} (LoadState={load}): is {} installed where it reads \
                 units?",
                unit::TEMPLATE
            ),
        );
    }
    if !listed {
        return fail(
            ClientEvent::HostUnavailable,
            format!(
                "{name} does not read {} (a drop-in replaced its EnvironmentFile=?): nothing written",
                file.display()
            ),
        );
    }
    let text = match unit::environment(&project, &path) {
        Ok(text) => text,
        Err(why) => return fail(ClientEvent::Config, why),
    };
    match unit::write_environment(&file, &project, &text) {
        Ok(()) => {}
        Err(e @ unit::WriteError::Collision(_)) => {
            return fail(ClientEvent::Config, format!("{}: {e}", project.display()));
        }
        Err(e) => return fail(ClientEvent::HostUnavailable, e),
    }
    if let Err(why) = systemctl(&systemctl_program, &["enable", "--quiet", &name], &path) {
        return fail(ClientEvent::HostUnavailable, why);
    }
    print(&format!("{name} {}\n", project.display()))
}
