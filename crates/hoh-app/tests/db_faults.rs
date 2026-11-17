//! docs/09's fault matrix, item 7 ("磁盘满/权限只读/SQLite busy/迁移失败；保留可诊断状态而不是半提交"; online backup is
//! REQ-063, M5): the product binary and its client in a project of their own, with nothing of the user's (an empty
//! PATH of agents, a home and runtime directory of the test's; no user manager answers, so the client starts its host
//! directly). Each fault leaves the database as it was (compared by its logical dump) and says what happened where the
//! user looks: the client's stderr and exit code.
//!
//! The tests marked `ignore` mount a file system of their own (a small tmpfs, an ext4 image that turns read-only)
//! with `sudo -n`; they run only when asked (`--ignored`) and fail, never pass, where `sudo -n` does not work.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml): the tests read the database with python3.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use std::io::{BufRead as _, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const HOH: &str = env!("CARGO_BIN_EXE_hoh");

/// How long one client command may take.
const LIMIT: Duration = Duration::from_secs(60);

/// A git project with a home and a runtime directory of its own, in `base` (a temporary directory, or a mount).
struct Project {
    root: PathBuf,
    _dir: Option<tempfile::TempDir>,
}

impl Project {
    fn new() -> R<Project> {
        let dir = tempfile::tempdir()?;
        let mut project = Project::in_dir(dir.path())?;
        project._dir = Some(dir);
        Ok(project)
    }

    fn in_dir(base: &Path) -> R<Project> {
        let root = base.to_path_buf();
        std::fs::create_dir_all(root.join("p"))?;
        std::fs::create_dir_all(root.join("home/run"))?;
        let git = Command::new("git")
            .args(["init", "-q"])
            .arg(root.join("p"))
            .status()?;
        assert!(git.success(), "git init");
        Ok(Project { root, _dir: None })
    }

    fn path(&self) -> PathBuf {
        self.root.join("p")
    }

    fn hoh_dir(&self) -> PathBuf {
        self.path().join(".hoh")
    }

    fn db(&self) -> PathBuf {
        self.hoh_dir().join("state.db")
    }

    /// `hoh args…` in the project with the test's environment only, to its end (killed after [`LIMIT`]).
    fn hoh(&self, args: &[&str]) -> R<Output> {
        let home = self.root.join("home");
        let child = Command::new(HOH)
            .args(args)
            .current_dir(self.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("LANG", "C.UTF-8")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let end = Instant::now() + LIMIT;
        let mut child = child;
        while child.try_wait()?.is_none() {
            if Instant::now() > end {
                let _ = child.kill();
                return Err(format!("hoh {args:?} did not end in {LIMIT:?}").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(child.wait_with_output()?)
    }

    /// A new run by `hoh -p <goal> --detach`: its host started if none runs. Its stdout and stderr when it fails.
    fn submit(&self, goal: &str) -> R<Output> {
        self.hoh(&["-p", goal, "--detach", "--output", "json"])
    }

    /// [`Project::submit`] that must be accepted.
    fn accepted(&self, goal: &str) -> R {
        let out = self.submit(goal)?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        Ok(())
    }

    /// The pid of the host its `host.json` names, while that process is this project's host (a file left behind
    /// may name a pid since reused).
    fn host_pid(&self) -> Option<i32> {
        let text = std::fs::read_to_string(self.hoh_dir().join("host.json")).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let pid = i32::try_from(value["pid"].as_i64()?).ok()?;
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let wanted = format!("__host\0{}\0", self.path().display());
        String::from_utf8_lossy(&cmdline)
            .contains(&wanted)
            .then_some(pid)
    }

    /// Stop the project's host (SIGTERM) and wait until it is gone.
    fn stop_host(&self) -> R {
        let Some(pid) = self.host_pid() else {
            return Ok(());
        };
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
        let end = Instant::now() + Duration::from_secs(30);
        while Path::new(&format!("/proc/{pid}")).exists() {
            if Instant::now() > end {
                return Err(format!("host {pid} did not stop").into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    /// The first column of the first row `sql` gives on the database opened read-only, as text.
    fn sql(&self, sql: &str) -> R<String> {
        python(
            "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro', uri=True, timeout=10); \
             r=c.execute(sys.argv[2]).fetchone(); print('' if r is None or r[0] is None else r[0])",
            &[self.db().as_os_str(), sql.as_ref()],
        )
    }

    /// The database's content: its user_version and its logical dump (a file's bytes change with WAL mode alone).
    fn dump(&self) -> R<String> {
        python(
            "import sqlite3,sys; c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro', uri=True, timeout=10); \
             print(c.execute('PRAGMA user_version').fetchone()[0]); print('\\n'.join(c.iterdump()))",
            &[self.db().as_os_str()],
        )
    }
}

impl Drop for Project {
    /// A host the test's commands started is killed however the test ends.
    fn drop(&mut self) {
        if let Some(pid) = self.host_pid().and_then(rustix::process::Pid::from_raw) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

fn python(code: &str, args: &[&std::ffi::OsStr]) -> R<String> {
    let out = Command::new("python3")
        .arg("-c")
        .arg(code)
        .args(args)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Another process's write lock on a database (`BEGIN IMMEDIATE`), held until released.
struct Lock(Child);

impl Lock {
    fn take(db: &Path) -> R<Lock> {
        let mut child = Command::new("python3")
            .arg("-c")
            .arg(
                "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], isolation_level=None, timeout=10); \
                 c.execute('BEGIN IMMEDIATE'); print('locked', flush=True); sys.stdin.read(); c.execute('ROLLBACK')",
            )
            .arg(db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
        assert_eq!(line.trim(), "locked");
        Ok(Lock(child))
    }

    fn release(mut self) -> R {
        drop(self.0.stdin.take());
        assert!(self.0.wait()?.success(), "the lock's holder failed");
        Ok(())
    }
}

/// A project whose host made its database, stopped: the database as it is, and its content.
fn made() -> R<(Project, String)> {
    let project = Project::new()?;
    project.accepted("first")?;
    project.stop_host()?;
    let dump = project.dump()?;
    Ok((project, dump))
}

#[test]
fn busy_a_host_started_under_another_processs_write_lock_refuses_within_its_busy_timeout_and_changes_nothing()
-> R {
    // REQ-062 (busy_timeout 5000 ms): the start's own write (its host epoch) waits the busy timeout, then the host
    // ends saying DB_BUSY and the client says so; nothing of the start is written, and the next start goes on.
    let (project, before) = made()?;
    let lock = Lock::take(&project.db())?;
    let started = Instant::now();
    let out = project.submit("second")?;
    let waited = started.elapsed();
    lock.release()?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("DB_BUSY"), "{stderr}");
    assert!(
        waited >= Duration::from_secs(5) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
    assert_eq!(project.dump()?, before, "the refused start wrote");
    project.accepted("third")?;
    assert_eq!(project.sql("SELECT count(*) FROM runs")?, "2");
    project.stop_host()
}

#[test]
fn migration_a_database_of_a_newer_binary_is_refused_with_78_naming_both_versions_and_left_as_it_was()
-> R {
    let (project, _) = made()?;
    python(
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('PRAGMA user_version = 7'); c.commit()",
        &[project.db().as_os_str()],
    )?;
    let before = project.dump()?;
    for _ in 0..2 {
        let out = project.submit("second")?;
        assert_eq!(out.status.code(), Some(78), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("database schema version 7 is newer than this binary supports (1)"),
            "{stderr}"
        );
        assert_eq!(project.dump()?, before);
        assert!(!project.hoh_dir().join("host.json").exists());
    }
    Ok(())
}

#[test]
fn migration_a_migration_that_fails_is_rolled_back_and_the_client_says_which_and_why_on_one_line()
-> R {
    // A database the schema cannot be applied to (a table of its own named as one of the schema's): migration 1 fails
    // in its one transaction, the database keeps what it had, and the reason reaches the client, which shows the last
    // line the host wrote.
    let project = Project::new()?;
    std::fs::create_dir_all(project.hoh_dir())?;
    python(
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('CREATE TABLE runs(x)'); \
         c.execute('INSERT INTO runs VALUES (42)'); c.commit()",
        &[project.db().as_os_str()],
    )?;
    let before = project.dump()?;
    let out = project.submit("first")?;
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("migration 1 failed and was rolled back: table runs already exists"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("CREATE "),
        "the schema's script is not the reason: {stderr}"
    );
    assert_eq!(project.dump()?, before, "the failed migration left changes");
    Ok(())
}

/// The test's user is not root (root ignores file modes, so nothing would be read-only).
fn not_root() -> R {
    if rustix::process::geteuid().is_root() {
        return Err("run as a user other than root: root ignores file modes".into());
    }
    Ok(())
}

#[test]
fn read_only_a_database_made_read_only_under_its_host_is_still_written_and_the_next_start_refuses_saying_so()
-> R {
    // A mode change takes effect at the next open: the running host holds its database open for writing, so its
    // commands are still taken; the next host cannot write it, says so, and writes nothing.
    not_root()?;
    let project = Project::new()?;
    project.accepted("first")?;
    let files: Vec<PathBuf> = ["state.db", "state.db-wal", "state.db-shm"]
        .iter()
        .map(|f| project.hoh_dir().join(f))
        .filter(|f| f.exists())
        .collect();
    for file in &files {
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444))?;
    }
    project.accepted("second")?;
    project.stop_host()?;
    std::fs::set_permissions(project.db(), std::fs::Permissions::from_mode(0o444))?;
    let before = project.dump()?;
    let out = project.submit("third")?;
    for file in &files {
        if file.exists() {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644))?;
        }
    }
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("attempt to write a readonly database"),
        "{stderr}"
    );
    assert_eq!(project.dump()?, before);
    assert_eq!(project.sql("SELECT count(*) FROM runs")?, "2");
    project.accepted("fourth")?;
    project.stop_host()
}

#[test]
fn read_only_a_hoh_directory_the_host_cannot_write_is_named_as_such_not_taken_for_wal() -> R {
    // The WAL self-test runs in .hoh; when .hoh refuses a new file, the reason is the directory, with the OS error,
    // not DB_WAL_UNSUPPORTED (which says the file system cannot do WAL). Still 78: the environment is not usable.
    not_root()?;
    let (project, before) = made()?;
    let dir = project.hoh_dir();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555))?;
    let out = project.submit("second");
    let left: Vec<String> = std::fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".hoh-"))
        .collect();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755))?;
    let out = out?;
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(".hoh is not writable") && stderr.contains("Permission denied"),
        "{stderr}"
    );
    assert!(!stderr.contains("DB_WAL_UNSUPPORTED"), "{stderr}");
    assert!(left.is_empty(), "{left:?}");
    assert_eq!(project.dump()?, before);
    project.accepted("third")?;
    project.stop_host()
}

/// A file system of the test's own, mounted with `sudo -n` and unmounted however the test ends.
struct Mount {
    point: PathBuf,
    /// The loop image's directory, for an ext4 one.
    _work: tempfile::TempDir,
}

impl Mount {
    fn sudo(args: &[&str]) -> R {
        let out = Command::new("sudo").arg("-n").args(args).output()?;
        if !out.status.success() {
            return Err(format!(
                "sudo -n {args:?}: {} (this test needs sudo -n)",
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        Ok(())
    }

    /// A tmpfs of `size` owned by the test's user.
    fn tmpfs(size: &str) -> R<Mount> {
        let work = tempfile::tempdir()?;
        let point = work.path().join("m");
        std::fs::create_dir(&point)?;
        let options = format!(
            "size={size},uid={},gid={},mode=0755",
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw()
        );
        Mount::sudo(&[
            "mount",
            "-t",
            "tmpfs",
            "-o",
            &options,
            "tmpfs",
            &point.display().to_string(),
        ])?;
        Ok(Mount { point, _work: work })
    }

    /// An ext4 image mounted with errors=remount-ro, owned by the test's user; its loop device's name.
    fn ext4() -> R<(Mount, String)> {
        let work = tempfile::tempdir()?;
        let image = work.path().join("fs.img");
        let point = work.path().join("m");
        std::fs::create_dir(&point)?;
        std::fs::File::create(&image)?.set_len(64 << 20)?;
        let made = Command::new("/usr/sbin/mkfs.ext4")
            .args(["-q", "-F"])
            .arg(&image)
            .status()?;
        assert!(made.success(), "mkfs.ext4");
        let (image_s, point_s) = (image.display().to_string(), point.display().to_string());
        Mount::sudo(&["mount", "-o", "loop,errors=remount-ro", &image_s, &point_s])?;
        let mount = Mount { point, _work: work };
        let owner = format!(
            "{}:{}",
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw()
        );
        Mount::sudo(&["chown", &owner, &point_s])?;
        let source = Command::new("findmnt")
            .args(["-n", "-o", "SOURCE"])
            .arg(&mount.point)
            .output()?;
        let device = String::from_utf8(source.stdout)?
            .trim()
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_owned();
        Ok((mount, device))
    }

    /// Fill it: a file grown until the file system says it is full.
    fn fill(&self) -> R<PathBuf> {
        let filler = self.point.join("filler");
        let mut file = std::fs::File::create(&filler)?;
        let block = vec![0u8; 4096];
        loop {
            match file.write_all(&block) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(28) => return Ok(filler),
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for Mount {
    fn drop(&mut self) {
        let _ = Command::new("sudo")
            .args(["-n", "umount"])
            .arg(&self.point)
            .status();
    }
}

#[test]
#[ignore = "mounts a tmpfs with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
fn full_a_full_file_system_is_db_full_and_retryable_writes_nothing_and_writes_resume_with_room() -> R
{
    let mount = Mount::tmpfs("16m")?;
    let project = Project::in_dir(&mount.point)?;
    let outcome = (|| -> R {
        project.accepted("first")?;
        let runs = project.sql("SELECT count(*) FROM runs")?;
        let filler = mount.fill()?;
        let out = project.submit("second")?;
        assert_ne!(out.status.code(), Some(0), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("DB_FULL"), "{stderr}");
        assert_eq!(project.sql("SELECT count(*) FROM runs")?, runs);
        std::fs::remove_file(filler)?;
        project.accepted("third")?;
        assert_eq!(project.sql("PRAGMA integrity_check")?, "ok");
        project.stop_host()
    })();
    let _ = project.stop_host();
    outcome
}

#[test]
#[ignore = "mounts a tmpfs with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
fn full_a_host_started_on_a_full_file_system_says_db_full_not_wal() -> R {
    let mount = Mount::tmpfs("16m")?;
    let project = Project::in_dir(&mount.point)?;
    let outcome = (|| -> R {
        project.accepted("first")?;
        project.stop_host()?;
        let before = project.dump()?;
        let filler = mount.fill()?;
        let out = project.submit("second")?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert_eq!(out.status.code(), Some(69), "{}", text(&out));
        assert!(stderr.contains("DB_FULL"), "{stderr}");
        assert!(!stderr.contains("DB_WAL_UNSUPPORTED"), "{stderr}");
        assert_eq!(project.dump()?, before);
        std::fs::remove_file(filler)?;
        project.accepted("third")?;
        project.stop_host()
    })();
    let _ = project.stop_host();
    outcome
}

#[test]
#[ignore = "mounts an ext4 image with sudo -n and makes it fail read-only (docs/09 fault matrix item 7); run with --ignored"]
fn read_only_a_file_system_that_turns_read_only_under_its_host_fails_commands_with_their_cause() -> R
{
    // ext4 with errors=remount-ro, its error triggered: the open database's writes fail (SQLite: disk I/O error). The
    // command fails whole and the client is told why (the host's log is on the same file system, so it says nothing);
    // the host goes on answering and stops cleanly; the next host is refused.
    let (mount, device) = Mount::ext4()?;
    let project = Project::in_dir(&mount.point)?;
    let outcome = (|| -> R {
        project.accepted("first")?;
        let before = project.dump()?;
        Mount::sudo(&[
            "sh",
            "-c",
            &format!("echo 1 > /sys/fs/ext4/{device}/trigger_fs_error"),
        ])?;
        let out = project.submit("second")?;
        assert_ne!(out.status.code(), Some(0), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("disk I/O error"), "{stderr}");
        assert_eq!(project.dump()?, before);
        assert_eq!(project.sql("PRAGMA quick_check")?, "ok");
        project.stop_host()?;
        let out = project.submit("third")?;
        assert_eq!(out.status.code(), Some(69), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("Read-only file system"), "{stderr}");
        Ok(())
    })();
    let _ = project.stop_host();
    outcome
}
