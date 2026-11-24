//! `hoh` binary (REQ-003, REQ-004): only a dispatcher. Its internal modes (the WAL self-test's probe, the agent start
//! gate, the installer's `__unit`, the host) run here; everything a user runs (bare `hoh`, `hoh -p`, `hoh attach`,
//! `hoh doctor`, help and version) is the client crate's (`hoh_client::run`), which cannot name host code at all
//! (docs/adr/ADR-031.md, 设计修正 2, the corrections after its second implementation review).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

/// A line on stderr, whatever becomes of it (M1QA-01): through the client crate's `say`, which writes straight to fd 2
/// in a process with no client signal state (every internal mode here). Defined before the modules, so they can use it.
macro_rules! say {
    ($($arg:tt)*) => {
        hoh_client::say(&format!($($arg)*))
    };
}

mod installer;

use hoh_client::exit::{ClientEvent, Ending};
use std::ffi::OsString;

fn main() {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    // An installed hoh holds its installation for its whole run, whatever it was asked; none runs while it is being
    // uninstalled (docs/adr/ADR-023.md design correction 2). Kept open until the process ends.
    let _installation =
        match std::env::current_exe().map(|exe| hoh_app::unit::hold_installation(&exe)) {
            Ok(Err(why)) => {
                say!("hoh: {why}");
                std::process::exit(i32::from(
                    hoh_client::exit::exit_code(Ending::Client(ClientEvent::HostUnavailable))
                        .unwrap_or(69),
                ));
            }
            Ok(Ok(held)) => held,
            Err(_) => None,
        };
    // Internal mode: the second process of the WAL self-test (REQ-062); not a user command and in no help text.
    if args.first().map(OsString::as_os_str)
        == Some(hoh_core::persistence::selftest::PROBE_ARG.as_ref())
    {
        // Where a test holds the probe (debug builds only).
        hoh_client::debug_hold::at("wal-probe");
        std::process::exit(hoh_core::persistence::selftest::probe_main());
    }
    // Internal mode: the start gate an agent is started through (docs/adr/ADR-022.md decision 13); not a user command and
    // in no help text.
    if args.first().map(OsString::as_os_str) == Some("__gate".as_ref()) {
        std::process::exit(hoh_app::gate::main(&args[1..]));
    }
    // Internal mode: the start gate a verifier's check command is started through (docs/adr/ADR-028.md decision 7); not
    // a user command and in no help text.
    if args.first().map(OsString::as_os_str) == Some("__check-gate".as_ref()) {
        std::process::exit(hoh_app::gate::check_main(&args[1..]));
    }
    // Internal mode: what the installer asks of the binary (docs/adr/ADR-023.md decision 5); not a user command and in
    // no help text.
    if args.first().map(OsString::as_os_str) == Some("__unit".as_ref()) {
        std::process::exit(i32::from(installer::main(&args[1..])));
    }
    // Internal mode: the host a client starts (docs/adr/ADR-021.md decision 1); not a user command and in no help text.
    if args.first().map(OsString::as_os_str) == Some("__host".as_ref()) {
        std::process::exit(i32::from(host(&args[1..])));
    }
    std::process::exit(i32::from(hoh_client::run(&args)));
}

/// `hoh __host <project>`: its own session before anything else (no terminal, so no shell's hang-up reaches it),
/// stdin and stdout from its starter (`/dev/null`), stderr to its starter until it is ready, then to `.hoh/host.log`.
fn host(args: &[OsString]) -> u8 {
    let [project] = args else {
        return hoh_client::usage_code();
    };
    let project = std::path::PathBuf::from(project);
    if !project.is_absolute() || !project.is_dir() {
        return hoh_client::usage_code();
    }
    // Where a host a client starts runs, so one its unit starts (in HOME by default) runs there too (ADR-023 decision 7).
    if let Err(e) = std::env::set_current_dir(&project) {
        say!("hoh host: cannot enter {}: {e}", project.display());
        return hoh_client::usage_code();
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
