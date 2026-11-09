//! The host's systemd user unit (REQ-005 behaviour 6 and decision 4, CR-011, docs/adr/ADR-023.md decisions 1 to 4),
//! under a private user manager (`installed`): what the installed template says, an instance enabled for a project,
//! a host the unit starts in its delegated cgroup and restarts after a crash, the exits it does not restart, and the
//! uninstall stopping a running instance.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod installed;

use installed::{Manager, R};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// A manager with this build installed in its home from a staged bundle; `projects` enabled.
fn installed(projects: &[&Path]) -> R<Manager> {
    let manager = Manager::start()?;
    let bundle = installed::stage_bundle(&manager.root().join("bundle"), b"")?;
    let mut args = Vec::new();
    for project in projects {
        args.push("--project");
        args.push(project.to_str().ok_or("path")?);
    }
    let out = manager.install(&bundle, &args)?;
    if out.status.code() != Some(0) {
        return Err(format!("install.sh {args:?}: {}", text(&out)).into());
    }
    Ok(manager)
}

/// A Git project with one commit at `<root>/p` (made before the install, which needs it to exist).
fn project(root: &Path) -> R<PathBuf> {
    let at = root.join("p");
    std::fs::create_dir_all(&at)?;
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&at)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root)
            .status()?;
        if !status.success() {
            return Err(format!("git {args:?} failed").into());
        }
    }
    Ok(std::fs::canonicalize(at)?)
}

fn unit_of(project: &Path) -> String {
    hoh_app::unit::unit_name(&hoh_app::unit::instance(project))
}

/// The project's host.json, if there is one to read.
fn host_json(project: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(project.join(".hoh/host.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
}

/// Wait until the unit's main process is the project's ready host (host.json names it); its pid.
fn ready_under_the_unit(manager: &mut Manager, unit: &str, project: &Path) -> R<u32> {
    let mut pid = 0;
    manager.wait_until(
        "the host its unit started to be ready",
        Duration::from_secs(30),
        |m| {
            pid = m.property(unit, "MainPID")?.parse().unwrap_or(0);
            Ok(pid != 0
                && host_json(project).and_then(|h| h["pid"].as_u64()) == Some(u64::from(pid)))
        },
    )?;
    Ok(pid)
}

#[test]
fn the_private_manager_runs_in_a_scope_of_its_own_and_leaves_nothing_behind() -> R {
    let manager = Manager::start()?;
    let scope = format!("{}.scope", manager.scope);
    let root = manager.root().to_path_buf();
    assert!(
        installed::real_units(&scope)?.contains(&scope),
        "{scope} while it runs"
    );
    drop(manager);
    assert!(
        !installed::real_units(&scope)?.contains(&scope),
        "{scope} left"
    );
    assert!(!root.exists(), "{} left", root.display());
    Ok(())
}

#[test]
fn the_installed_template_loads_with_the_settings_of_req_005() -> R {
    let manager = installed(&[])?;
    let unit = unit_of(Path::new("/nowhere"));
    let shown = manager.show(
        &unit,
        "LoadState,FragmentPath,Type,ExecStart,EnvironmentFiles,Restart,RestartUSec,StartLimitBurst,\
         StartLimitIntervalUSec,RestartPreventExitStatus,Delegate,KillMode,TimeoutStopUSec",
    )?;
    let get = |key: &str| shown.get(key).map(String::as_str).unwrap_or_default();
    assert_eq!(get("LoadState"), "loaded");
    assert_eq!(
        Path::new(get("FragmentPath")),
        manager.data().join("systemd/user/hoh-host@.service")
    );
    assert_eq!(get("Type"), "exec");
    assert!(
        hoh_app::unit::runs(get("ExecStart"), &manager.home().join(".local/bin/hoh")),
        "{}",
        get("ExecStart")
    );
    assert_eq!(
        hoh_app::unit::environment_file(get("EnvironmentFiles")),
        Some(manager.state().join(format!(
            "hoh/hosts/{}.env",
            hoh_app::unit::instance(Path::new("/nowhere"))
        )))
    );
    assert_eq!(get("Restart"), "on-failure");
    assert_eq!(get("RestartUSec"), "5s");
    assert_eq!(get("StartLimitBurst"), "5");
    assert_eq!(get("StartLimitIntervalUSec"), "5min");
    assert_eq!(get("RestartPreventExitStatus"), "64 69 78");
    assert_eq!(get("Delegate"), "yes");
    assert_eq!(get("KillMode"), "control-group");
    assert_eq!(get("TimeoutStopUSec"), "15s");
    Ok(())
}

#[test]
fn an_install_with_a_project_enables_its_instance_and_writes_its_file() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let enabled = manager.systemctl(&["is-enabled", &unit])?;
    assert_eq!(String::from_utf8(enabled.stdout)?.trim(), "enabled");
    let file = manager.state().join(format!(
        "hoh/hosts/{}.env",
        hoh_app::unit::instance(&project)
    ));
    assert_eq!(
        std::fs::metadata(&file)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(file.parent().ok_or("dir")?)?
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        hoh_app::unit::project_of(&std::fs::read_to_string(&file)?),
        Some(project.clone())
    );
    // Nothing was started: enabling is for the manager's next start.
    assert_eq!(manager.property(&unit, "ActiveState")?, "inactive");
    assert!(!project.join(".hoh").exists());
    Ok(())
}

#[test]
fn a_host_its_unit_starts_runs_in_the_units_delegated_cgroup_and_is_restarted_after_a_kill() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let out = manager.systemctl(&["start", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let first = ready_under_the_unit(&mut manager, &unit, &project)?;
    let info = host_json(&project).ok_or("host.json")?;
    // CR-011: under its unit (Delegate=yes) the host tracks its attempts by cgroups of their own.
    assert_eq!(info["process_tracking"], "cgroup", "{info}");
    let cgroup = std::fs::read_to_string(format!("/proc/{first}/cgroup"))?;
    assert!(cgroup.trim_end().ends_with(&format!("/{unit}")), "{cgroup}");
    let epoch = info["host_epoch"].as_i64().ok_or("host_epoch")?;
    assert_eq!(manager.property(&unit, "NRestarts")?, "0");
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(first)?).ok_or("pid")?,
        rustix::process::Signal::KILL,
    )?;
    let mut second = 0;
    manager.wait_until(
        "the unit to restart its host",
        Duration::from_secs(30),
        |m| {
            second = m.property(&unit, "MainPID")?.parse().unwrap_or(0);
            Ok(second != 0
                && second != first
                && m.property(&unit, "NRestarts")? == "1"
                && host_json(&project).and_then(|h| h["pid"].as_u64()) == Some(u64::from(second)))
        },
    )?;
    let info = host_json(&project).ok_or("host.json")?;
    assert_eq!(info["host_epoch"].as_i64(), Some(epoch + 1), "{info}");
    assert_eq!(info["process_tracking"], "cgroup", "{info}");
    assert_eq!(manager.property(&unit, "ActiveState")?, "active");
    Ok(())
}

/// Start the unit of `project` and check that its host ends with `code` and is not started again, past RestartSec.
fn not_restarted(manager: &mut Manager, unit: &str, code: &str) -> R {
    let out = manager.systemctl(&["start", unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    manager.wait_until("the host to end", Duration::from_secs(20), |m| {
        Ok(m.property(unit, "ActiveState")? == "failed")
    })?;
    assert_eq!(manager.property(unit, "ExecMainStatus")?, code);
    // RestartSec is 5 s: a restart would have come by now.
    std::thread::sleep(Duration::from_secs(7));
    let shown = manager.show(unit, "ActiveState,NRestarts,ExecMainStatus")?;
    assert_eq!(
        shown.get("NRestarts").map(String::as_str),
        Some("0"),
        "{shown:?}"
    );
    assert_eq!(
        shown.get("ActiveState").map(String::as_str),
        Some("failed"),
        "{shown:?}"
    );
    Ok(())
}

#[test]
fn a_host_that_finds_its_project_held_by_another_exits_69_and_is_not_restarted() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    // Another host, not the unit's, holds the project (REQ-005 failure mode 4).
    let mut other = manager
        .command(manager.home().join(".local/bin/hoh"))
        .arg("__host")
        .arg(&project)
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let pid = other.id();
    let held = manager.wait_until(
        "the other host to be ready",
        Duration::from_secs(30),
        |_| Ok(host_json(&project).and_then(|h| h["pid"].as_u64()) == Some(u64::from(pid))),
    );
    let outcome = held.and_then(|()| not_restarted(&mut manager, &unit, "69"));
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("pid")?,
        rustix::process::Signal::TERM,
    );
    let _ = other.wait();
    outcome
}

#[test]
fn a_host_whose_project_is_gone_exits_64_and_is_not_restarted() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    std::fs::remove_dir_all(&project)?;
    not_restarted(&mut manager, &unit, "64")
}

#[test]
fn the_uninstall_stops_and_disables_a_running_instance_and_keeps_its_project() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let out = manager.systemctl(&["start", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let pid = ready_under_the_unit(&mut manager, &unit, &project)?;
    let bundle = manager.root().join("bundle/hoh");
    let out = manager.install(&bundle, &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert!(
        String::from_utf8(out.stdout.clone())?.contains(&format!("stopped and disabled {unit}")),
        "{}",
        text(&out)
    );
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "the host still runs"
    );
    assert_eq!(manager.property(&unit, "LoadState")?, "not-found");
    let enabled = manager.systemctl(&["is-enabled", &unit])?;
    assert!(!enabled.status.success(), "{}", text(&enabled));
    for gone in [
        manager.home().join(".local/bin/hoh"),
        manager.data().join("hoh"),
        manager.data().join("systemd/user/hoh-host@.service"),
        manager.state().join("hoh/hosts"),
        manager
            .home()
            .join(format!(".config/systemd/user/default.target.wants/{unit}")),
    ] {
        assert!(
            !gone.exists() && !gone.is_symlink(),
            "{} left",
            gone.display()
        );
    }
    // The project's data stays (AC-005-06).
    assert!(project.join(".hoh/state.db").is_file());
    Ok(())
}
