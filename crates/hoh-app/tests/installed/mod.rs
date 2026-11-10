//! An installed HoH for tests (docs/adr/ADR-023.md decision 10): a release bundle staged from this build as
//! `scripts/product/bundle.sh` lays it out, and a private systemd user manager in a delegated transient scope of the
//! user's own manager, with a temporary HOME and XDG directories, so that installing, enabling, starting and killing a
//! host's unit touch nothing of the user's. A missing precondition (cgroup v2, the user's manager, lingering, the
//! systemd binary) fails the test with its reason: it is never skipped (docs/09).

#![allow(dead_code)] // each test binary uses a part of it

use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The repository's root.
pub fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Make the executable `path` from `sources` (files, concatenated) by `cat` in a process of its own, never through a
/// file this process opens for writing: such a file is inherited by whatever child another test thread forks
/// meanwhile, and executing it fails with ETXTBSY until that child has exec'd.
pub fn executable(path: &Path, sources: &[&Path]) -> R {
    let status = Command::new("sh")
        .args([
            "-c",
            "out=$1; shift; cat \"$@\" > \"$out\" && chmod 755 \"$out\"",
            "sh",
        ])
        .arg(path)
        .args(sources)
        .status()?;
    if !status.success() {
        return Err(format!("cannot make {}", path.display()).into());
    }
    Ok(())
}

/// A script at `path` with `body`, made by [`executable`] from a file that is never executed.
pub fn script(path: &Path, body: &str) -> R {
    let mut part = path.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    std::fs::write(&part, body)?;
    executable(path, &[&part])?;
    std::fs::remove_file(part)?;
    Ok(())
}

/// Stage a bundle at `<at>/hoh` as bundle.sh does: this build's binary (with `extra` bytes after it, still runnable,
/// another version by its digest), the repository's install.sh, LICENSE, THIRD_PARTY_NOTICES.md and resources/.
pub fn stage_bundle(at: &Path, extra: &[u8]) -> R<PathBuf> {
    let hoh = at.join("hoh");
    std::fs::create_dir_all(hoh.join("bin"))?;
    let tail = at.join("tail");
    std::fs::write(&tail, extra)?;
    executable(
        &hoh.join("bin/hoh"),
        &[Path::new(env!("CARGO_BIN_EXE_hoh")), &tail],
    )?;
    std::fs::remove_file(tail)?;
    executable(
        &hoh.join("install.sh"),
        &[&repository().join("scripts/install.sh")],
    )?;
    for file in ["LICENSE", "THIRD_PARTY_NOTICES.md"] {
        std::fs::copy(repository().join(file), hoh.join(file))?;
    }
    let status = Command::new("cp")
        .arg("-R")
        .arg(repository().join("resources"))
        .arg(hoh.join("resources"))
        .status()?;
    if !status.success() {
        return Err("cannot copy resources/".into());
    }
    Ok(hoh)
}

/// This user's name: `$USER`, else by `id -un`.
pub fn user() -> R<String> {
    if let Ok(name) = std::env::var("USER")
        && !name.is_empty()
    {
        return Ok(name);
    }
    let out = Command::new("id").arg("-un").output()?;
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// `systemctl --user <args>` against the user's own manager, in this process's environment.
pub fn real_systemctl(args: &[&str]) -> R<Output> {
    Ok(Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?)
}

/// The units of the user's own manager whose names match `pattern`, as `systemctl list-units --all` lists them.
pub fn real_units(pattern: &str) -> R<Vec<String>> {
    let out = real_systemctl(&["list-units", "--all", "--plain", "--no-legend", pattern])?;
    Ok(String::from_utf8(out.stdout)?
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect())
}

/// Directories a test made unreadable, given back their permissions (0755) when it is dropped, whatever ended the test:
/// declared after what owns their temporary root, so it is dropped first and the root can then be removed (M2T14R-904).
pub struct Restore(pub Vec<PathBuf>);

impl Drop for Restore {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt as _;
        for path in &self.0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        }
    }
}

/// A private `systemd --user` in a scope of its own, under a short temporary root (a socket path has 107 bytes):
/// `h/` the HOME with its XDG directories, `r/` the runtime directory, `bin/` the test's stand-ins.
pub struct Manager {
    root: PathBuf,
    /// The transient scope of the user's manager it runs in.
    pub scope: String,
    /// The scope's cgroup, from the manager's own ControlGroup.
    cgroup: PathBuf,
    child: Option<Child>,
    pub user: String,
}

/// How long the manager may take to answer, and to end.
const MANAGER_WAIT: Duration = Duration::from_secs(10);

impl Manager {
    pub fn start() -> R<Manager> {
        if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
            return Err("precondition: cgroup v2 is not mounted at /sys/fs/cgroup".into());
        }
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or(
            "precondition: XDG_RUNTIME_DIR is not set (no user manager to run a scope in)",
        )?;
        if !Path::new(&runtime).join("systemd/private").exists() {
            return Err("precondition: the user's systemd manager does not answer".into());
        }
        let user = user()?;
        if !Path::new("/var/lib/systemd/linger").join(&user).is_file() {
            return Err(format!("precondition: user {user} does not linger").into());
        }
        let systemd = Path::new("/usr/lib/systemd/systemd");
        if !systemd.is_file() {
            return Err("precondition: /usr/lib/systemd/systemd is missing".into());
        }
        let root = tempfile::Builder::new()
            .prefix("hohsd.")
            .tempdir_in("/tmp")?
            .keep();
        let scope = format!(
            "hoh-test-{}-{}",
            std::process::id(),
            root.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("hohsd."))
                .unwrap_or("x")
        );
        let mut manager = Manager {
            root,
            scope,
            cgroup: PathBuf::new(),
            child: None,
            user,
        };
        for dir in [
            "h/.config",
            "h/.local/share",
            "h/.local/state",
            "h/.cache",
            "r/systemd/user",
            "bin",
        ] {
            std::fs::create_dir_all(manager.root.join(dir))?;
        }
        std::fs::set_permissions(manager.runtime(), std::fs::Permissions::from_mode(0o700))?;
        // An empty target to start, so the user units enabled system-wide (/etc/systemd/user) do not start in it.
        std::fs::write(
            manager.runtime().join("systemd/user/hoh-test.target"),
            "[Unit]\nDescription=HoH test manager\n",
        )?;
        let log = std::fs::File::create(manager.root.join("manager.log"))?;
        let mut command = Command::new("systemd-run");
        command
            .args([
                "--user",
                "--scope",
                "-p",
                "Delegate=yes",
                "-p",
                "RuntimeMaxSec=900",
            ])
            .arg(format!("--unit={}", manager.scope))
            .args(["--quiet", "--collect", "--", "/usr/bin/env", "-i"]);
        for (key, value) in manager.vars() {
            let mut pair = OsString::from(format!("{key}="));
            pair.push(value);
            command.arg(pair);
        }
        command
            .arg("PATH=/usr/bin:/bin")
            .arg(systemd)
            .args(["--user", "--log-target=console", "--unit=hoh-test.target"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        manager.child = Some(command.spawn()?);
        let socket = manager.runtime().join("systemd/private");
        manager.wait_until("the private manager to answer", MANAGER_WAIT, |m| {
            if let Some(child) = m.child.as_mut()
                && let Some(status) = child.try_wait()?
            {
                return Err(format!(
                    "the private manager ended ({status}): {}",
                    std::fs::read_to_string(m.root.join("manager.log")).unwrap_or_default()
                )
                .into());
            }
            Ok(socket.exists())
        })?;
        // The proof of isolation, before anything is installed: the manager runs in this test's scope.
        let own = manager.systemctl(&["show", "-p", "ControlGroup", "--value"])?;
        let own = String::from_utf8(own.stdout)?.trim().to_owned();
        if !own.ends_with(&format!("/{}.scope", manager.scope)) {
            return Err(format!(
                "the private manager is not in this test's scope {}: {own:?}",
                manager.scope
            )
            .into());
        }
        manager.cgroup = Path::new("/sys/fs/cgroup").join(own.trim_start_matches('/'));
        Ok(manager)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn home(&self) -> PathBuf {
        self.root.join("h")
    }

    pub fn runtime(&self) -> PathBuf {
        self.root.join("r")
    }

    pub fn data(&self) -> PathBuf {
        self.home().join(".local/share")
    }

    pub fn state(&self) -> PathBuf {
        self.home().join(".local/state")
    }

    /// The directory of the test's stand-ins, first on the PATH of what [`Manager::command`] runs.
    pub fn bin(&self) -> PathBuf {
        self.root.join("bin")
    }

    /// The environment of the manager and of everything a test runs against it (but PATH).
    pub fn vars(&self) -> Vec<(&'static str, OsString)> {
        let home = self.home();
        vec![
            ("HOME", home.clone().into()),
            ("XDG_CONFIG_HOME", home.join(".config").into()),
            ("XDG_DATA_HOME", home.join(".local/share").into()),
            ("XDG_STATE_HOME", home.join(".local/state").into()),
            ("XDG_CACHE_HOME", home.join(".cache").into()),
            ("XDG_RUNTIME_DIR", self.runtime().into()),
            ("USER", self.user.clone().into()),
            ("LANG", "C.UTF-8".into()),
        ]
    }

    /// `program` in a clean environment: the manager's variables and PATH = `bin/`, `~/.local/bin`, /usr/bin, /bin.
    /// No D-Bus address and no `r/bus`, so `systemctl --user` can only reach this manager.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command.env_clear().envs(self.vars()).env(
            "PATH",
            format!(
                "{}:{}:/usr/bin:/bin",
                self.bin().display(),
                self.home().join(".local/bin").display()
            ),
        );
        command
    }

    pub fn systemctl(&self, args: &[&str]) -> R<Output> {
        Ok(self
            .command("systemctl")
            .arg("--user")
            .args(args)
            .output()?)
    }

    /// `systemctl --user show -p <props> <unit>`, as properties.
    pub fn show(&self, unit: &str, props: &str) -> R<BTreeMap<String, String>> {
        let out = self.systemctl(&["show", "-p", props, unit])?;
        Ok(hoh_app::unit::show(&String::from_utf8(out.stdout)?))
    }

    /// One property of `unit`.
    pub fn property(&self, unit: &str, prop: &str) -> R<String> {
        Ok(self.show(unit, prop)?.remove(prop).unwrap_or_default())
    }

    /// Run `<bundle>/install.sh <args>` (`bundle` is a staged `hoh/` directory).
    pub fn install(&self, bundle: &Path, args: &[&str]) -> R<Output> {
        Ok(self
            .command("bash")
            .arg(bundle.join("install.sh"))
            .args(args)
            .current_dir(&self.root)
            .output()?)
    }

    /// Wait until `ready` holds, polling every 50 ms, at most `limit`.
    pub fn wait_until(
        &mut self,
        what: &str,
        limit: Duration,
        mut ready: impl FnMut(&mut Manager) -> R<bool>,
    ) -> R {
        let end = Instant::now() + limit;
        loop {
            if ready(self)? {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(format!("{what}: not within {limit:?}").into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The manager's log so far (its stderr: units it started, failed and restarted).
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("manager.log")).unwrap_or_default()
    }
}

impl Manager {
    /// Kill every process of this user whose command line names this manager's root, until none is left (at most
    /// 5 s): a host a test started directly for a project under the root, or one its unit left, whatever became of the
    /// test. Test code: nothing else of the user's names a /tmp/hohsd.* directory.
    fn kill_strays(&self) {
        use std::os::unix::fs::MetadataExt as _;
        let root = self.root.as_os_str().as_encoded_bytes().to_vec();
        let uid = rustix::process::getuid().as_raw();
        let me = std::process::id();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let mut found = false;
            for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
                let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                    continue;
                };
                if pid == me || entry.metadata().map(|m| m.uid()).ok() != Some(uid) {
                    continue;
                }
                let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                    continue;
                };
                if cmdline.windows(root.len()).any(|w| w == root.as_slice())
                    && let Some(pid) = i32::try_from(pid)
                        .ok()
                        .and_then(rustix::process::Pid::from_raw)
                {
                    found = true;
                    let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
                }
            }
            if !found || Instant::now() >= end {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Manager {
    /// Let the manager stop its units and end (at most its wait); then kill whatever is left in its scope's cgroup
    /// (delegated to this user), whether the manager ended or not, and every process naming its root; then remove the
    /// root, whose `r/systemd/inaccessible/*` the manager made unreadable.
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = self.systemctl(&["exit"]);
            let end = Instant::now() + MANAGER_WAIT;
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(50));
            }
            if !self.cgroup.as_os_str().is_empty() {
                let _ = std::fs::write(self.cgroup.join("cgroup.kill"), "1");
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        self.kill_strays();
        let _ = Command::new("chmod")
            .args(["-R", "u+rwX"])
            .arg(&self.root)
            .status();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
