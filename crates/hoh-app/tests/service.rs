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

/// A manager with this build installed in its home from a staged bundle, and a Git project under the manager's root
/// (so its teardown ends whatever a test left running for it), enabled when `enable`.
fn installed(enable: bool) -> R<(Manager, PathBuf)> {
    let manager = Manager::start()?;
    let project = project(manager.root())?;
    let bundle = installed::stage_bundle(&manager.root().join("bundle"), b"")?;
    let mut args = Vec::new();
    if enable {
        args.push("--project");
        args.push(project.to_str().ok_or("path")?);
    }
    let out = manager.install(&bundle, &args)?;
    if out.status.code() != Some(0) {
        return Err(format!("install.sh {args:?}: {}", text(&out)).into());
    }
    Ok((manager, project))
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
    let (manager, _) = installed(false)?;
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
    // The instance's file is the installation's (ADR-023 design correction 1), and the manager reads exactly it.
    let files = manager.systemctl(&["show", "-p", "EnvironmentFiles", &unit])?;
    assert_eq!(
        String::from_utf8(files.stdout)?,
        format!(
            "EnvironmentFiles={}/hoh/hosts/{}.env (ignore_errors=no)\n",
            manager.data().display(),
            hoh_app::unit::instance(Path::new("/nowhere"))
        )
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
    let (manager, project) = installed(true)?;
    let unit = unit_of(&project);
    let enabled = manager.systemctl(&["is-enabled", &unit])?;
    assert_eq!(String::from_utf8(enabled.stdout)?.trim(), "enabled");
    let file = hoh_app::unit::instance_file(
        &manager.data().join("hoh"),
        &hoh_app::unit::instance(&project),
    );
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
    let (mut manager, project) = installed(true)?;
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
    let (mut manager, project) = installed(true)?;
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
    let (mut manager, project) = installed(true)?;
    let unit = unit_of(&project);
    std::fs::remove_dir_all(&project)?;
    not_restarted(&mut manager, &unit, "64")
}

#[test]
fn the_uninstall_stops_and_disables_a_running_instance_and_keeps_its_project() -> R {
    let (mut manager, project) = installed(true)?;
    let unit = unit_of(&project);
    let out = manager.systemctl(&["start", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let pid = ready_under_the_unit(&mut manager, &unit, &project)?;
    // ADR-023 decision 11's supplementary check: the database's rows and every file but the host's own runtime files.
    let rows = dump(&project)?;
    let files = kept(&project)?;
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
        manager.data().join("hoh/hosts"),
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
    // The project's data stays (AC-005-06): the same rows, the same files (the host's stop removes only its own).
    assert_eq!(dump(&project)?, rows);
    assert_eq!(kept(&project)?, files);
    Ok(())
}

/// The SHA-256 of `project`'s database's rows (Python's iterdump, read-only), whatever its WAL holds.
fn dump(project: &Path) -> R<String> {
    let out = std::process::Command::new("python3")
        .args([
            "-c",
            "import hashlib,sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro', uri=True); \
             print(hashlib.sha256('\\n'.join(c.iterdump()).encode()).hexdigest())",
        ])
        .arg(project.join(".hoh/state.db"))
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Every file under `project`'s .hoh/ but the database (compared by [`dump`]) and the host's own runtime files, with
/// its size and digest.
fn kept(project: &Path) -> R<Vec<(String, u64, String)>> {
    let dir = project.join(".hoh");
    let mut out = Vec::new();
    let mut todo = vec![dir.clone()];
    while let Some(at) = todo.pop() {
        for item in std::fs::read_dir(&at)? {
            let path = item?.path();
            let name = path.strip_prefix(&dir)?.display().to_string();
            if name.starts_with("state.db") || name.starts_with("host.") {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                todo.push(path);
                out.push((name, 0, String::new()));
            } else if meta.is_file() {
                out.push((name, meta.len(), installed::sha256(&std::fs::read(&path)?)));
            }
        }
    }
    out.sort();
    Ok(out)
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
    let (manager, project) = installed(true)?;
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
    let (mut manager, project) = installed(true)?;
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
    let (manager, project) = installed(true)?;
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
    let (mut manager, project) = installed(true)?;
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
    let (manager, project) = installed(true)?;
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
    let (manager, project) = installed(true)?;
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

#[test]
fn the_client_writes_its_project_and_path_for_the_unit_also_when_no_instance_was_enabled() -> R {
    // ADR-023 decisions 2 and 6: installed without --project, so no file names the project; the client writes the
    // instance's file (the project, its own PATH) before it starts the unit, and the host gets exactly those.
    let (manager, project) = installed(false)?;
    let unit = unit_of(&project);
    let extra = manager.root().join("extra bin");
    std::fs::create_dir(&extra)?;
    let path = format!(
        "{}:{}:{}:/usr/bin:/bin",
        extra.display(),
        manager.bin().display(),
        manager.home().join(".local/bin").display()
    );
    let out = manager
        .command(manager.home().join(".local/bin/hoh"))
        .env("PATH", &path)
        .args(["-p", "x", "--detach", "--output", "json"])
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let main: u32 = manager.property(&unit, "MainPID")?.parse()?;
    assert_ne!(main, 0);
    let environ = std::fs::read(format!("/proc/{main}/environ"))?;
    let vars: Vec<&[u8]> = environ.split(|b| *b == 0).collect();
    let var = |name: &str| {
        vars.iter()
            .find_map(|v| v.strip_prefix(format!("{name}=").as_bytes()))
            .map(|v| String::from_utf8_lossy(v).into_owned())
    };
    assert_eq!(var("PATH").as_deref(), Some(path.as_str()));
    assert_eq!(var("HOH_PROJECT").as_deref(), project.to_str());
    let file = hoh_app::unit::instance_file(
        &manager.data().join("hoh"),
        &hoh_app::unit::instance(&project),
    );
    assert_eq!(
        hoh_app::unit::project_of(&std::fs::read_to_string(file)?),
        Some(project.clone())
    );
    // Started, not enabled: the install enabled nothing.
    let enabled = manager.systemctl(&["is-enabled", &unit])?;
    assert_ne!(String::from_utf8(enabled.stdout)?.trim(), "enabled");
    Ok(())
}

/// A drop-in for the template with `body`, read by the manager.
fn dropin(manager: &Manager, name: &str, body: &str) -> R {
    let dir = manager
        .home()
        .join(".config/systemd/user/hoh-host@.service.d");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(name), body)?;
    let out = manager.systemctl(&["daemon-reload"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(())
}

/// The environment of process `pid`, as NAME=value strings.
fn environ(pid: u32) -> R<Vec<String>> {
    Ok(std::fs::read(format!("/proc/{pid}/environ"))?
        .split(|b| *b == 0)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .collect())
}

#[test]
fn a_file_a_drop_in_adds_is_read_by_the_host_and_never_written_by_hoh() -> R {
    // M2T14R-02: a user's drop-in adds an environment file (credentials, a proxy); the client writes only the
    // installation's own file, the host gets both.
    let (manager, project) = installed(false)?;
    let unit = unit_of(&project);
    let user = manager.home().join(".config/agent.env");
    std::fs::write(&user, "AGENT_SENTINEL=preserve-me\n")?;
    dropin(
        &manager,
        "agent.conf",
        &format!("[Service]\nEnvironmentFile={}\n", user.display()),
    )?;
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(
        std::fs::read_to_string(&user)?,
        "AGENT_SENTINEL=preserve-me\n"
    );
    let main: u32 = manager.property(&unit, "MainPID")?.parse()?;
    let vars = environ(main)?;
    assert!(
        vars.iter().any(|v| v == "AGENT_SENTINEL=preserve-me"),
        "{vars:?}"
    );
    assert!(
        vars.iter()
            .any(|v| v == &format!("HOH_PROJECT={}", project.display())),
        "{vars:?}"
    );
    assert_eq!(
        host_json(&project).ok_or("host.json")?["supervisor"],
        unit.as_str()
    );
    Ok(())
}

#[test]
fn a_drop_in_that_replaces_the_installations_file_keeps_hoh_from_the_unit_and_from_writing_it() -> R
{
    // M2T14R-02: with the installation's file replaced by the user's, `__unit enable` refuses and the client starts
    // its host another way, saying why; the user's file is never written.
    let (manager, project) = installed(false)?;
    let unit = unit_of(&project);
    let user = manager.home().join(".config/agent.env");
    std::fs::write(&user, "AGENT_SENTINEL=preserve-me\n")?;
    dropin(
        &manager,
        "agent.conf",
        &format!(
            "[Service]\nEnvironmentFile=\nEnvironmentFile={}\n",
            user.display()
        ),
    )?;
    let bundle = manager.root().join("bundle/hoh");
    let out = manager.install(&bundle, &["--project", project.to_str().ok_or("path")?])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(String::from_utf8(out.stderr)?.contains("does not read"));
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let info = host_json(&project).ok_or("host.json")?;
    assert!(info["supervisor"].is_null(), "{info}");
    assert!(
        info["supervision_reason"]
            .as_str()
            .is_some_and(|why| why.contains("does not read")),
        "{info}"
    );
    assert_eq!(manager.property(&unit, "ActiveState")?, "inactive");
    assert_eq!(
        std::fs::read_to_string(&user)?,
        "AGENT_SENTINEL=preserve-me\n"
    );
    assert!(!manager.data().join("hoh/hosts").exists());
    Ok(())
}

#[test]
fn the_uninstall_removes_nothing_a_drop_in_points_at() -> R {
    // M2T14R-01: a drop-in points the instance's environment file into the project's .hoh/; the uninstall removes only
    // what the installer wrote, never a directory taken from the manager's configuration.
    let (manager, project) = installed(true)?;
    let hoh = project.join(".hoh");
    std::fs::create_dir_all(&hoh)?;
    std::fs::write(hoh.join("state.db"), "sentinel")?;
    std::fs::write(hoh.join("host.env"), "HOH_PROJECT=/elsewhere\n")?;
    dropin(
        &manager,
        "elsewhere.conf",
        &format!(
            "[Service]\nEnvironmentFile=\nEnvironmentFile={}/host.env\n",
            hoh.display()
        ),
    )?;
    let bundle = manager.root().join("bundle/hoh");
    let out = manager.install(&bundle, &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    assert_eq!(std::fs::read_to_string(hoh.join("state.db"))?, "sentinel");
    assert_eq!(
        std::fs::read_to_string(hoh.join("host.env"))?,
        "HOH_PROJECT=/elsewhere\n"
    );
    assert!(!manager.data().join("hoh").exists());
    assert!(!manager.home().join(".local/bin/hoh").exists());
    Ok(())
}

#[test]
fn the_uninstall_removes_nothing_while_an_instance_did_not_stop_or_a_host_runs_the_installation()
-> R {
    // M2T14R-03: a disable that fails (a stand-in systemctl refuses it), then a host started directly from the
    // installed binary: each time the uninstall ends with 69 and removes nothing; once nothing runs, it completes.
    let (mut manager, project) = installed(true)?;
    let unit = unit_of(&project);
    let out = manager.systemctl(&["start", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let pid = ready_under_the_unit(&mut manager, &unit, &project)?;
    let bundle = manager.root().join("bundle/hoh");
    let installed_paths = [
        manager.home().join(".local/bin/hoh"),
        manager.data().join("hoh/current"),
        manager.data().join("systemd/user/hoh-host@.service"),
        hoh_app::unit::instance_file(
            &manager.data().join("hoh"),
            &hoh_app::unit::instance(&project),
        ),
    ];
    let stand_in = manager.bin().join("systemctl");
    installed::script(
        &stand_in,
        "#!/bin/sh\ncase \" $* \" in *\" disable \"*) echo 'refused' >&2; exit 1;; esac\nexec /usr/bin/systemctl \"$@\"\n",
    )?;
    let out = manager.install(&bundle, &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(
        !String::from_utf8(out.stdout.clone())?.contains("stopped and disabled"),
        "{}",
        text(&out)
    );
    for path in &installed_paths {
        assert!(path.exists(), "{} removed", path.display());
    }
    assert_eq!(manager.property(&unit, "MainPID")?, pid.to_string());
    let exe = std::fs::read_link(format!("/proc/{pid}/exe"))?;
    assert!(
        !exe.to_string_lossy().ends_with(" (deleted)"),
        "{}",
        exe.display()
    );
    std::fs::remove_file(&stand_in)?;
    // Stopped now, but a host started directly from the installation runs.
    let out = manager.systemctl(&["stop", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let mut direct = manager
        .command(manager.data().join("hoh/current/bin/hoh"))
        .arg("__host")
        .arg(&project)
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let direct_pid = direct.id();
    manager.wait_until("the direct host", Duration::from_secs(30), |_| {
        Ok(host_json(&project).and_then(|h| h["pid"].as_u64()) == Some(u64::from(direct_pid)))
    })?;
    let out = manager.install(&bundle, &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(
        String::from_utf8(out.stderr.clone())?.contains(&format!("pid {direct_pid}")),
        "{}",
        text(&out)
    );
    for path in &installed_paths[..3] {
        assert!(path.exists(), "{} removed", path.display());
    }
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(i32::try_from(direct_pid)?).ok_or("pid")?,
        rustix::process::Signal::TERM,
    )?;
    let _ = direct.wait();
    let out = manager.install(&bundle, &["--uninstall"])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    for path in &installed_paths {
        assert!(
            !path.exists() && !path.is_symlink(),
            "{} left",
            path.display()
        );
    }
    assert!(project.join(".hoh/state.db").is_file());
    Ok(())
}

#[test]
fn a_start_still_running_at_the_clients_deadline_is_reported_as_such_and_goes_on() -> R {
    // M2T14R-52: after a crash the manager first stops what the previous host left (up to TimeoutStopSec, 15 s); a
    // start that has not finished by the client's 10 s is not a refusal. Here a drop-in makes the start take 12 s.
    let (mut manager, project) = installed(true)?;
    let unit = unit_of(&project);
    dropin(
        &manager,
        "slow.conf",
        "[Service]\nExecStartPre=/bin/sleep 12\n",
    )?;
    let started = std::time::Instant::now();
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    let took = started.elapsed();
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(took < Duration::from_secs(12), "{took:?}");
    let stderr = String::from_utf8(out.stderr)?;
    assert!(stderr.contains("did not finish starting"), "{stderr}");
    assert!(!stderr.contains(" failed (Result="), "{stderr}");
    // The start went on in the manager: the host is up once it is done.
    ready_under_the_unit(&mut manager, &unit, &project)?;
    Ok(())
}

#[test]
fn a_host_its_unit_ends_with_a_refusal_ends_the_client_with_78_and_the_hosts_line() -> R {
    // M2T14R-53 (ADR-023 decision 6, M2T06R-60): a host that refuses its database (here newer than it knows: 78, not
    // restarted) ends the client with 78 and its line, as a host started directly does; a client that judged the
    // unit's end without its exit status would say 69.
    let (mut manager, project) = installed(true)?;
    let unit = unit_of(&project);
    let out = manager.systemctl(&["start", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    ready_under_the_unit(&mut manager, &unit, &project)?;
    let out = manager.systemctl(&["stop", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let set = std::process::Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('PRAGMA user_version = 999'); c.commit()",
        ])
        .arg(project.join(".hoh/state.db"))
        .status()?;
    assert!(set.success());
    let out = hoh_in(
        &manager,
        &project,
        None,
        &["-p", "x", "--detach", "--output", "json"],
    )?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
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
    assert_eq!(manager.property(&unit, "ExecMainStatus")?, "78");
    assert_eq!(manager.property(&unit, "NRestarts")?, "0");
    Ok(())
}
