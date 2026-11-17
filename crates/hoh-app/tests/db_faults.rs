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
        self.hoh_with(args, &[])
    }

    /// [`Project::hoh`] with `env` added (the client passes it on to the host it starts, and the host to its probe).
    fn hoh_with(&self, args: &[&str], env: &[(&str, std::ffi::OsString)]) -> R<Output> {
        let home = self.root.join("home");
        let child = Command::new(HOH)
            .args(args)
            .current_dir(self.path())
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", home.join("run"))
            .env("LANG", "C.UTF-8")
            .envs(env.iter().map(|(k, v)| (k, v)))
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

    /// [`Project::dump`] once the running host has gone quiet: the same for a second (its tick is 200 ms), so that
    /// what its last command set going (the supervisor's cursor, say) is in it and not taken for a fault's write.
    fn quiet_dump(&self) -> R<String> {
        const QUIET: Duration = Duration::from_secs(1);
        let end = Instant::now() + Duration::from_secs(20);
        let mut last = self.dump()?;
        let mut since = Instant::now();
        while since.elapsed() < QUIET {
            if Instant::now() > end {
                return Err("the host did not go quiet in 20 s".into());
            }
            std::thread::sleep(Duration::from_millis(100));
            let now = self.dump()?;
            if now != last {
                last = now;
                since = Instant::now();
            }
        }
        Ok(last)
    }
}

impl Drop for Project {
    /// A host the test's commands started is killed however the test ends, and waited for (a while): a test on a
    /// mount of its own unmounts it next, which a host still there would keep busy.
    fn drop(&mut self) {
        let Some(pid) = self.host_pid() else {
            return;
        };
        if let Some(pid) = rustix::process::Pid::from_raw(pid) {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
        let end = Instant::now() + Duration::from_secs(10);
        while Path::new(&format!("/proc/{pid}")).exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
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

/// tests/support/io_fault.c, built with `cc` into `dir`: an LD_PRELOAD shim that fails I/O on the WAL self-test's
/// files (after Codex's injector, fault7 review round 2). A test artefact: never built into or shipped with the product.
fn io_fault_shim(dir: &Path) -> R<PathBuf> {
    let so = dir.join("io_fault.so");
    let out = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-Wall", "-Werror", "-o"])
        .arg(&so)
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/io_fault.c"
        ))
        .arg("-ldl")
        .output()?;
    if !out.status.success() {
        return Err(format!("cc io_fault.c: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(so)
}

/// One I/O fault of [`io_fault_shim`] on the WAL self-test's files at a host's start, and what the client must say.
struct IoFault {
    why: &'static str,
    /// F7_IO_OP, F7_IO_SUFFIX, F7_IO_ERRNO.
    op: &'static str,
    file: &'static str,
    errno: i32,
    /// The file system stays full or over quota after the first fault (F7_IO_STATE).
    sticky: bool,
    /// The first fault in the probe process only (F7_IO_PROBE).
    in_probe: bool,
    /// The next read after it is interrupted (EINTR) and retried by SQLite (F7_CLEANUP_ERRNO).
    interrupted_read: bool,
    /// The first fault at the nth matching call (F7_IO_NTH).
    nth: u32,
    exit: i32,
    says: &'static [&'static str],
}

impl IoFault {
    /// A made project's next start under the fault: the client's exit and words, nothing written, nothing left. What
    /// differs, as text.
    fn check(&self) -> R<Vec<String>> {
        let (project, before) = made()?;
        let mut env = vec![
            ("LD_PRELOAD", io_fault_shim(&project.root)?.into_os_string()),
            ("F7_IO_OP", self.op.into()),
            ("F7_IO_SUFFIX", self.file.into()),
            ("F7_IO_ERRNO", self.errno.to_string().into()),
        ];
        if self.sticky {
            env.push(("F7_IO_STATE", project.root.join("io-fault-state").into()));
        }
        if self.in_probe {
            env.push(("F7_IO_PROBE", "1".into()));
        }
        if self.interrupted_read {
            env.push(("F7_CLEANUP_ERRNO", "4".into()));
        }
        env.push(("F7_IO_NTH", self.nth.to_string().into()));
        let out = project.hoh_with(&["-p", "second", "--detach", "--output", "json"], &env)?;
        let stderr = String::from_utf8_lossy(&out.stderr);
        let mut wrong = Vec::new();
        if out.status.code() != Some(self.exit) {
            wrong.push(format!("exit {:?}, not {}", out.status.code(), self.exit));
        }
        for words in self.says {
            if !stderr.contains(words) {
                wrong.push(format!("no `{words}`"));
            }
        }
        if project.dump()? != before {
            wrong.push("the database changed".to_owned());
        }
        let left: Vec<String> = std::fs::read_dir(project.hoh_dir())?
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".hoh-wal-selftest-"))
            .collect();
        if !left.is_empty() {
            wrong.push(format!("left {left:?}"));
        }
        if !wrong.is_empty() {
            wrong.push(format!("stderr: {}", stderr.trim()));
        }
        Ok(wrong)
    }
}

/// Every fault checked; all that differs from what each must give, case by case.
fn check_all(faults: &[IoFault]) -> R {
    let mut failed = Vec::new();
    for fault in faults {
        let wrong = fault.check()?;
        if !wrong.is_empty() {
            failed.push(format!("{}:\n  {}", fault.why, wrong.join("\n  ")));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("\n").into())
    }
}

#[test]
fn full_a_full_disk_or_quota_is_db_full_however_sqlites_own_rollback_leaves_errno() -> R {
    // F7-100: SQLite does rollback I/O inside the call that failed, so the errno read after it returns may be a later
    // call's (an interrupted read, EINTR). The cause is observed directly instead: a write-family I/O error makes the
    // self-test try 64 KiB in the store's directory, and a file system that stays full or over quota refuses it.
    let quota = 122;
    let no_space = 28;
    check_all(&[
        IoFault {
            why: "the WAL's fsync over quota, then an interrupted rollback read",
            op: "sync",
            file: "-wal",
            errno: quota,
            sticky: true,
            in_probe: false,
            interrupted_read: true,
            nth: 1,
            exit: 69,
            says: &["DB_FULL", "Disk quota exceeded"],
        },
        IoFault {
            why: "the WAL's fsync without space, then an interrupted rollback read",
            op: "sync",
            file: "-wal",
            errno: no_space,
            sticky: true,
            in_probe: false,
            interrupted_read: true,
            nth: 1,
            exit: 69,
            says: &["DB_FULL", "No space left on device"],
        },
        IoFault {
            why: "the probe's WAL fsync over quota, then an interrupted rollback read",
            op: "sync",
            file: "-wal",
            errno: quota,
            sticky: true,
            in_probe: true,
            interrupted_read: true,
            nth: 1,
            exit: 69,
            says: &["DB_FULL", "the probe's", "Disk quota exceeded"],
        },
        IoFault {
            why: "one fsync over quota, the directory with room after it",
            op: "sync",
            file: "-wal",
            errno: quota,
            sticky: false,
            in_probe: false,
            interrupted_read: true,
            nth: 1,
            exit: 78,
            says: &["DB_WAL_UNSUPPORTED", "fsync", "not for want of room"],
        },
    ])
}

/// The WAL self-test's files in `.hoh` (`.hoh-wal-selftest-…`), by name, with their bytes.
fn selftest_files(project: &Project) -> R<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(project.hoh_dir())? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(".hoh-wal-selftest-") {
            files.push((name, std::fs::read(entry.path())?));
        }
    }
    files.sort();
    Ok(files)
}

#[test]
fn full_a_room_file_another_creator_made_after_the_claim_is_left_to_it() -> R {
    // F7-200: the room probe's `-room` name is made by another creator after the self-test claimed its database's
    // names (here when the WAL's fsync fails); the probe's create_new finds it, so it is not the self-test's: room is
    // not known, and the file stays, its bytes as they were.
    let (project, before) = made()?;
    let env = [
        ("LD_PRELOAD", io_fault_shim(&project.root)?.into_os_string()),
        ("F7_IO_OP", "sync".into()),
        ("F7_IO_SUFFIX", "-wal".into()),
        ("F7_IO_ERRNO", "122".into()),
        ("F7_IO_PLANT", "-room".into()),
    ];
    let out = project.hoh_with(&["-p", "second", "--detach", "--output", "json"], &env)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    let left = selftest_files(&project)?;
    assert_eq!(
        left.iter()
            .map(|(name, bytes)| (name.ends_with(".db-room"), bytes.as_slice()))
            .collect::<Vec<_>>(),
        [(true, b"FOREIGN-CREATOR-DATA\n".as_slice())],
        "the other creator's file: {}",
        text(&out)
    );
    assert_eq!(out.status.code(), Some(78), "{}", text(&out));
    assert!(
        stderr.contains("room not known") && stderr.contains("File exists"),
        "{stderr}"
    );
    assert_eq!(project.dump()?, before);
    Ok(())
}

#[test]
fn full_a_checkpoint_that_fails_writing_is_the_wals_failure_not_probed() -> R {
    // F7-201: AC-062-06 makes a failed checkpoint DB_WAL_UNSUPPORTED; a real I/O error of the checkpoint (here over a
    // quota that stays) is not probed for room, and still says which I/O failed and SQLite's extended code.
    let checkpoint = |why, op, file, nth, says| IoFault {
        why,
        op,
        file,
        errno: 122,
        sticky: true,
        in_probe: false,
        interrupted_read: false,
        nth,
        exit: 78,
        says,
    };
    check_all(&[
        checkpoint(
            "the checkpoint's truncate of the WAL over quota",
            "truncate",
            "-wal",
            1,
            &[
                "DB_WAL_UNSUPPORTED",
                "at checkpoint",
                "a truncate failed, SQLite extended code 1546",
            ],
        ),
        checkpoint(
            "the checkpoint's fsync of the database over quota",
            "sync",
            ".db",
            2,
            &[
                "DB_WAL_UNSUPPORTED",
                "at checkpoint",
                "an fsync failed, SQLite extended code 1034",
            ],
        ),
    ])
}

#[test]
fn full_the_commit_of_the_switch_to_wal_is_not_lost() -> R {
    // F7-101: `PRAGMA journal_mode=WAL` commits on the self-test's new database (its journal, then its header); its
    // failure came after the first row and was lost, and the host later said "no -shm file".
    check_all(&[
        IoFault {
            why: "the first write of the database without space",
            op: "pwrite",
            file: ".db",
            errno: 28,
            sticky: false,
            in_probe: false,
            interrupted_read: false,
            nth: 1,
            exit: 69,
            says: &["DB_FULL", "enable WAL", "database or disk is full"],
        },
        IoFault {
            why: "the journal's fsync over quota, and it stays so",
            op: "sync",
            file: "-journal",
            errno: 122,
            sticky: true,
            in_probe: false,
            interrupted_read: false,
            nth: 1,
            exit: 69,
            says: &["DB_FULL", "enable WAL", "Disk quota exceeded"],
        },
        IoFault {
            why: "one fsync of the journal over quota, the directory with room after it",
            op: "sync",
            file: "-journal",
            errno: 122,
            sticky: false,
            in_probe: false,
            interrupted_read: false,
            nth: 1,
            exit: 78,
            says: &[
                "DB_WAL_UNSUPPORTED",
                "enable WAL",
                "fsync",
                "not for want of room",
            ],
        },
    ])
}

#[test]
fn full_a_new_stores_switch_to_wal_that_cannot_commit_is_db_full() -> R {
    // F7-101 in the store itself: the writer's `PRAGMA journal_mode=WAL` commits on a new state.db, and its failure
    // (here the database's first write, without space) must reach the start, not be lost after the first row. The
    // next start, with room, makes the store.
    let project = Project::new()?;
    let env = [
        ("LD_PRELOAD", io_fault_shim(&project.root)?.into_os_string()),
        ("F7_IO_MATCH", "/.hoh/state.db".into()),
        ("F7_IO_OP", "pwrite".into()),
        ("F7_IO_SUFFIX", "state.db".into()),
        ("F7_IO_ERRNO", "28".into()),
    ];
    let out = project.hoh_with(&["-p", "first", "--detach", "--output", "json"], &env)?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(69), "{}", text(&out));
    assert!(stderr.contains("DB_FULL"), "{stderr}");
    project.stop_host()?;
    project.accepted("second")?;
    assert_eq!(project.sql("SELECT count(*) FROM runs")?, "1");
    assert_eq!(project.sql("PRAGMA journal_mode")?, "wal");
    project.stop_host()
}

#[test]
fn full_a_commands_commit_that_fails_for_quota_is_db_full_and_writes_nothing() -> R {
    // The store's own commits (docs/09 fault matrix item 7; ADR-016's note, round 3 addition): a command's commit whose
    // WAL fsync fails over quota is DB_FULL, retryable, when the store's directory is seen to have no room right after;
    // with room it stays UNPROCESSABLE, its reason naming the I/O that failed. Nothing is written either way, and once
    // the fault is gone commands are taken.
    for (sticky, says) in [
        (true, ["DB_FULL", "nothing was written"].as_slice()),
        (
            false,
            ["UNPROCESSABLE", "an fsync failed", "not for want of room"].as_slice(),
        ),
    ] {
        let project = Project::new()?;
        let arm = project.root.join("io-fault-arm");
        let state = project.root.join("io-fault-state");
        let mut env = vec![
            ("LD_PRELOAD", io_fault_shim(&project.root)?.into_os_string()),
            ("F7_IO_MATCH", "/.hoh/state.db".into()),
            ("F7_IO_OP", "sync".into()),
            ("F7_IO_SUFFIX", "state.db-wal".into()),
            ("F7_IO_ERRNO", "122".into()),
            ("F7_IO_ARM", arm.clone().into_os_string()),
        ];
        if sticky {
            env.push(("F7_IO_STATE", state.clone().into_os_string()));
        }
        let out = project.hoh_with(&["-p", "first", "--detach", "--output", "json"], &env)?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let before = project.quiet_dump()?;
        std::fs::write(&arm, "")?;
        let out = project.submit("second")?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        let after = project.dump()?;
        std::fs::remove_file(&arm)?;
        let _ = std::fs::remove_file(&state);
        assert_ne!(
            out.status.code(),
            Some(0),
            "sticky {sticky}: {}",
            text(&out)
        );
        for words in says {
            assert!(
                stderr.contains(words),
                "sticky {sticky}: `{words}` not in {stderr}"
            );
        }
        assert_eq!(after, before, "sticky {sticky}");
        project.accepted("third")?;
        assert_eq!(project.sql("SELECT count(*) FROM runs")?, "2");
        project.stop_host()?;
    }
    Ok(())
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

/// A file system of the test's own, mounted with `sudo -n`. A test unmounts it by [`finish`], checked; one that ends
/// otherwise (a failed assertion) leaves it to `Drop`, the fallback.
struct Mount {
    point: PathBuf,
    mounted: bool,
    /// The mount point's directory, and the loop image's for an ext4 one; none for a bind mount over a directory of
    /// the test's.
    work: Option<tempfile::TempDir>,
}

impl Mount {
    fn sudo(args: &[&str]) -> R {
        let out = Command::new("sudo").arg("-n").args(args).output()?;
        if !out.status.success() {
            return Err(format!(
                "sudo -n {args:?}: {} (this test needs sudo -n)",
                String::from_utf8_lossy(&out.stderr).trim()
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
        Ok(Mount {
            point,
            mounted: true,
            work: Some(work),
        })
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
        let mount = Mount {
            point,
            mounted: true,
            work: Some(work),
        };
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

    /// `dir` bind-mounted on itself, read-only: its files open already stay writable, new opens for writing fail.
    /// Read-only from the start (one `mount --bind -o ro`; util-linux 2.39 and later set it before the mount is
    /// attached), so that no file is opened for writing through it in between.
    fn read_only_bind(dir: &Path) -> R<Mount> {
        let path = dir.display().to_string();
        Mount::sudo(&["mount", "--bind", "-o", "ro", &path, &path])?;
        let mount = Mount {
            point: dir.to_path_buf(),
            mounted: true,
            work: None,
        };
        let options = Command::new("findmnt")
            .args(["-n", "-o", "OPTIONS", "--mountpoint", &path])
            .output()?;
        let options = String::from_utf8(options.stdout)?;
        if !options.split(',').any(|o| o == "ro") {
            return Err(format!("{path} is not mounted read-only: {}", options.trim()).into());
        }
        Ok(mount)
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

impl Mount {
    /// Fill it but `room` bytes: the filler grown until the file system is full, then cut back by `room`.
    fn fill_leaving(&self, room: u64) -> R<PathBuf> {
        let filler = self.fill()?;
        let file = std::fs::OpenOptions::new().write(true).open(&filler)?;
        let len = file.metadata()?.len();
        file.set_len(len.saturating_sub(room))?;
        Ok(filler)
    }
}

impl Mount {
    /// Unmount it, checked: its failure is the test's.
    fn unmount(mut self) -> R {
        Mount::sudo(&["umount", &self.point.display().to_string()])?;
        self.mounted = false;
        Ok(())
    }
}

impl Drop for Mount {
    /// The fallback, for a test that ended without unmounting it or failed to: unmount, retried for a while (a host
    /// just killed may hold it), then lazily; a mount that stays is named, and its directory kept.
    fn drop(&mut self) {
        if !self.mounted {
            return;
        }
        let point = self.point.display().to_string();
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if Mount::sudo(&["umount", &point]).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        if Mount::sudo(&["umount", "-l", &point]).is_ok() {
            return;
        }
        if let Some(work) = self.work.as_mut() {
            work.disable_cleanup(true);
        }
        eprintln!("db_faults: {point} is still mounted; unmount it with `sudo umount {point}`");
    }
}

/// The end of a test on a mount of its own, checked: its host stopped and waited for, then the mount unmounted. A host
/// that does not stop or a mount that stays fails the test even when its checks passed (F7-04); when they failed,
/// theirs is the error returned and the cleanup's is told on stderr.
fn finish(outcome: R, project: Project, mount: Mount) -> R {
    let stopped = project.stop_host();
    drop(project);
    let unmounted = mount.unmount();
    if outcome.is_err() {
        for e in [&stopped, &unmounted]
            .into_iter()
            .filter_map(|r| r.as_ref().err())
        {
            eprintln!("db_faults: cleanup after a failed test: {e}");
        }
    }
    outcome.and(stopped).and(unmounted)
}

#[test]
#[ignore = "mounts a tmpfs with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
fn full_a_full_file_system_is_db_full_and_retryable_writes_nothing_and_writes_resume_with_room() -> R
{
    let mount = Mount::tmpfs("16m")?;
    let project = Project::in_dir(&mount.point)?;
    let outcome = (|| -> R {
        project.accepted("first")?;
        let filler = mount.fill()?;
        let before = project.quiet_dump()?;
        let out = project.submit("second")?;
        assert_ne!(out.status.code(), Some(0), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("DB_FULL"), "{stderr}");
        assert_eq!(project.dump()?, before);
        std::fs::remove_file(filler)?;
        project.accepted("third")?;
        assert_eq!(project.sql("PRAGMA integrity_check")?, "ok");
        project.stop_host()
    })();
    finish(outcome, project, mount)
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
    finish(outcome, project, mount)
}

#[test]
#[ignore = "mounts a tmpfs with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
fn full_a_host_started_with_a_few_kib_left_says_db_full_not_wal() -> R {
    // F7-01: with 16 KiB left the WAL self-test can make its files, but SQLite cannot grow the -shm wal-index: the
    // pwrite that grows it fails with ENOSPC, which SQLite reports as SQLITE_IOERR_SHMSIZE ("disk I/O error"). That is
    // the file system out of room, DB_FULL (the client 69), not DB_WAL_UNSUPPORTED (78); nothing is written.
    // F7-100: the same for other amounts left, up to where the self-test passes and the start's first write fails
    // instead; with 32 KiB or less the -shm's growth fails, and the room probe (64 KiB) is refused for want of space.
    let mount = Mount::tmpfs("16m")?;
    let project = Project::in_dir(&mount.point)?;
    let outcome = (|| -> R {
        project.accepted("first")?;
        project.stop_host()?;
        let before = project.dump()?;
        for kib in [8, 16, 24, 32, 40, 56] {
            let filler = mount.fill_leaving(kib << 10)?;
            let out = project.submit("second")?;
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            assert_eq!(
                out.status.code(),
                Some(69),
                "{kib} KiB left: {}",
                text(&out)
            );
            assert!(stderr.contains("DB_FULL"), "{kib} KiB left: {stderr}");
            if kib <= 32 {
                assert!(
                    stderr.contains("No space left on device"),
                    "{kib} KiB left: {stderr}"
                );
            }
            assert!(
                !stderr.contains("DB_WAL_UNSUPPORTED"),
                "{kib} KiB left: {stderr}"
            );
            project.stop_host()?;
            assert_eq!(project.dump()?, before, "{kib} KiB left");
            std::fs::remove_file(filler)?;
        }
        project.accepted("third")?;
        project.stop_host()
    })();
    finish(outcome, project, mount)
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
        let before = project.quiet_dump()?;
        Mount::sudo(&[
            "sh",
            "-c",
            &format!("echo 1 > /sys/fs/ext4/{device}/trigger_fs_error"),
        ])?;
        let out = project.submit("second")?;
        assert_ne!(out.status.code(), Some(0), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("disk I/O error"), "{stderr}");
        // The store's reason, not SQLite's symbols (ADR-016 decision 6).
        assert!(!stderr.contains("SQLITE_"), "{stderr}");
        assert_eq!(project.dump()?, before);
        assert_eq!(project.sql("PRAGMA quick_check")?, "ok");
        project.stop_host()?;
        let out = project.submit("third")?;
        assert_eq!(out.status.code(), Some(69), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("Read-only file system"), "{stderr}");
        Ok(())
    })();
    finish(outcome, project, mount)
}

#[test]
#[ignore = "bind-mounts the project's .hoh read-only with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
fn read_only_a_hoh_directory_bind_mounted_read_only_under_its_host_is_still_written_and_the_next_start_refuses_saying_so()
-> R {
    // F7-52: a read-only bind mount over .hoh takes effect at the next open, as a mode change does: the running host
    // holds its files open for writing, so its commands are still taken; the next host cannot open its lock, says
    // so, and writes nothing.
    let dir = tempfile::tempdir()?;
    let project = Project::in_dir(dir.path())?;
    project.accepted("first")?;
    project.quiet_dump()?;
    let mount = Mount::read_only_bind(&project.hoh_dir())?;
    // Bound again after the mount, so that a failed check drops it (its host killed and waited for) before the mount.
    let project = project;
    let outcome = (|| -> R {
        project.accepted("second")?;
        project.stop_host()?;
        let before = project.dump()?;
        let out = project.submit("third")?;
        assert_eq!(out.status.code(), Some(69), "{}", text(&out));
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("host.lock") && stderr.contains("Read-only file system"),
            "{stderr}"
        );
        assert_eq!(project.dump()?, before);
        assert_eq!(project.sql("SELECT count(*) FROM runs")?, "2");
        Ok(())
    })();
    finish(outcome, project, mount)
}
