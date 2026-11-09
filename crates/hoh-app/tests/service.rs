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

/// `hoh <args>` as the user runs it in `project`: the installed command, or `program` when given.
fn hoh_in(manager: &Manager, project: &Path, program: Option<&Path>, args: &[&str]) -> R<Output> {
    let installed = manager.home().join(".local/bin/hoh");
    Ok(manager
        .command(program.unwrap_or(&installed))
        .args(args)
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .output()?)
}

/// The `hoh __host` processes of this user whose project is `project`.
fn hosts_of(project: &Path) -> R<Vec<u32>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Ok(pid) = entry?.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|b| *b == 0).collect();
        if args.get(1) == Some(&&b"__host"[..])
            && args.get(2) == Some(&project.as_os_str().as_encoded_bytes())
        {
            out.push(pid);
        }
    }
    Ok(out)
}

#[test]
fn the_installed_client_starts_its_host_through_the_unit() -> R {
    // ADR-023 decision 6: the installed hoh starts the host as its unit, so systemd restarts it after a crash.
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let run: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    assert!(run["run_id"].is_string(), "{run}");
    let main: u64 = manager.property(&unit, "MainPID")?.parse()?;
    let info = host_json(&project).ok_or("host.json")?;
    assert_eq!(info["pid"].as_u64(), Some(main), "{info}");
    assert_eq!(info["supervisor"], unit.as_str(), "{info}");
    assert_eq!(info["process_tracking"], "cgroup", "{info}");
    assert_eq!(hosts_of(&project)?, [u32::try_from(main)?]);
    // It runs where a host started directly runs: in its project (decision 7).
    assert_eq!(std::fs::read_link(format!("/proc/{main}/cwd"))?, project);
    // hoh doctor reports it (decision 7).
    let out = hoh_in(&manager, &project, None, &["doctor", "--output", "json"])?;
    let report: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let host = &report["runtime"]["host"];
    assert_eq!(host["state"], "running", "{host}");
    assert_eq!(host["supervisor"], unit.as_str(), "{host}");
    Ok(())
}

#[test]
fn concurrent_attaches_through_the_unit_start_one_host_once() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let run: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    let run_id = run["run_id"].as_str().ok_or("run_id")?.to_owned();
    let epoch = host_json(&project).ok_or("host.json")?["host_epoch"]
        .as_i64()
        .ok_or("host_epoch")?;
    let out = manager.systemctl(&["stop", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    manager.wait_until("the host to stop", Duration::from_secs(20), |_| {
        Ok(hosts_of(&project)?.is_empty())
    })?;
    let installed = manager.home().join(".local/bin/hoh");
    let clients: Vec<_> = (0..3)
        .map(|_| {
            manager
                .command(&installed)
                .args(["attach", &run_id, "--output", "json"])
                .current_dir(&project)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
        })
        .collect::<Result<_, _>>()?;
    for client in clients {
        let out = client.wait_with_output()?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    }
    let main: u64 = manager.property(&unit, "MainPID")?.parse()?;
    let info = host_json(&project).ok_or("host.json")?;
    assert_eq!(info["pid"].as_u64(), Some(main), "{info}");
    assert_eq!(info["host_epoch"].as_i64(), Some(epoch + 1), "{info}");
    assert_eq!(hosts_of(&project)?, [u32::try_from(main)?]);
    assert_eq!(manager.property(&unit, "NRestarts")?, "0");
    Ok(())
}

#[test]
fn a_hoh_that_is_not_the_installed_one_starts_its_host_as_before_and_says_why() -> R {
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let manager = installed(&[&project])?;
    let unit = unit_of(&project);
    let out = hoh_in(
        &manager,
        &project,
        Some(Path::new(env!("CARGO_BIN_EXE_hoh"))),
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let info = host_json(&project).ok_or("host.json")?;
    assert!(info["supervisor"].is_null(), "{info}");
    assert!(
        info["supervision_reason"]
            .as_str()
            .is_some_and(|why| why.contains("not the installed hoh")),
        "{info}"
    );
    assert_eq!(manager.property(&unit, "ActiveState")?, "inactive");
    let pids = hosts_of(&project)?;
    assert_eq!(pids.len(), 1, "{pids:?}");
    for pid in pids {
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(pid)?).ok_or("pid")?,
            rustix::process::Signal::TERM,
        );
    }
    let mut manager = manager;
    manager.wait_until("the direct host to stop", Duration::from_secs(20), |_| {
        Ok(hosts_of(&project)?.is_empty())
    })
}

#[test]
fn a_unit_at_its_start_limit_fails_the_client_with_69_and_nothing_else_starts_a_host() -> R {
    // ADR-023 decision 6: the start limit is a brake on a crash loop; the client does not go round it. A `.hoh` that
    // is a file makes every host fail before it is ready; a drop-in shortens the loop (2 starts, 0.1 s apart).
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let mut manager = installed(&[&project])?;
    let unit = unit_of(&project);
    std::fs::write(project.join(".hoh"), "not a directory")?;
    let dropin = manager
        .home()
        .join(".config/systemd/user/hoh-host@.service.d");
    std::fs::create_dir_all(&dropin)?;
    std::fs::write(
        dropin.join("limit.conf"),
        "[Unit]\nStartLimitBurst=2\n[Service]\nRestartSec=100ms\n",
    )?;
    let out = manager.systemctl(&["daemon-reload"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let _ = manager.systemctl(&["start", &unit])?;
    // The loop ends at the limit with the unit failed (its Result the last exit's: systemd 259 says exit-code).
    manager.wait_until(
        "the restart loop to stop at the start limit",
        Duration::from_secs(30),
        |m| {
            let shown = m.show(&unit, "ActiveState,NRestarts")?;
            Ok(
                shown.get("ActiveState").map(String::as_str) == Some("failed")
                    && shown.get("NRestarts").and_then(|n| n.parse::<u32>().ok()) >= Some(1),
            )
        },
    )?;
    let said = manager.runtime().join(format!(
        "hoh-host@{}.stderr",
        hoh_app::unit::instance(&project)
    ));
    let before = std::fs::read_to_string(&said)?;
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
    let stderr = String::from_utf8(out.stderr)?;
    assert!(
        stderr.contains(&format!("systemctl --user status {unit}")),
        "{stderr}"
    );
    // systemd refused the start: no host ran (none wrote its stderr), and none was started another way.
    assert_eq!(std::fs::read_to_string(&said)?, before, "{stderr}");
    assert!(
        hosts_of(&project)?.is_empty(),
        "a host was started some other way"
    );
    assert!(project.join(".hoh").is_file());
    Ok(())
}

#[test]
fn a_host_that_keeps_failing_under_its_unit_fails_the_client_with_its_last_line() -> R {
    // A `.hoh` that is a file: every host the unit starts fails before it is ready; the unit restarts it; the client
    // follows until its deadline and then says what the host said.
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let manager = installed(&[&project])?;
    std::fs::write(project.join(".hoh"), "not a directory")?;
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_ne!(out.status.code(), Some(0), "{}", text(&out));
    assert!(out.stdout.is_empty(), "{}", text(&out));
    let said = std::fs::read_to_string(manager.runtime().join(format!(
        "hoh-host@{}.stderr",
        hoh_app::unit::instance(&project)
    )))?;
    let last = said
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .ok_or("the host said nothing")?;
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains(last), "client: {stderr}\nhost: {said}");
    Ok(())
}

#[test]
fn a_host_that_fails_under_its_unit_with_no_restart_coming_fails_the_client_at_once() -> R {
    // ADR-023 decision 6: the unit failed and nothing will restart it (here a drop-in says Restart=no): the client
    // judges the host's end then, by its exit and its last line, not at its deadline.
    let root = tempfile::tempdir()?;
    let project = project(root.path())?;
    let manager = installed(&[&project])?;
    std::fs::write(project.join(".hoh"), "not a directory")?;
    let dropin = manager
        .home()
        .join(".config/systemd/user/hoh-host@.service.d");
    std::fs::create_dir_all(&dropin)?;
    std::fs::write(dropin.join("once.conf"), "[Service]\nRestart=no\n")?;
    let out = manager.systemctl(&["daemon-reload"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let started = std::time::Instant::now();
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    let took = started.elapsed();
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(took < Duration::from_secs(5), "{took:?}: {}", text(&out));
    let said = std::fs::read_to_string(manager.runtime().join(format!(
        "hoh-host@{}.stderr",
        hoh_app::unit::instance(&project)
    )))?;
    let last = said
        .lines()
        .rfind(|l| !l.trim().is_empty())
        .ok_or("the host said nothing")?;
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains(last), "client: {stderr}\nhost: {said}");
    assert!(!stderr.contains("did not become ready"), "{stderr}");
    Ok(())
}
