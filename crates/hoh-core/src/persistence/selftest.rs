//! WAL self-test (REQ-062, CR-002): on a file system not known to be network or shared, the host proves at start that
//! SQLite's WAL mode works there before it creates the real database. A temporary database in the target directory
//! is switched to WAL and written; then a second **process** (two connections in one process would share SQLite's
//! in-process lock and wal-index state and prove nothing, M2T01R-02) reads it, sees a later write of ours and we see
//! one of its (the `-shm` wal-index is really shared), and each side is refused the write lock while the other holds
//! it (file locks really work). Finally the WAL is checkpointed. Any failure is `DB_WAL_UNSUPPORTED`; the temporary
//! files are removed in every case.
//!
//! The second process is started from a [`ProbeCommand`]: in the product the `hoh` binary itself with the hidden
//! argument [`PROBE_ARG`], which runs [`probe_main`]. It gets the database path in [`PROBE_DB_ENV`] and answers one
//! line per command on stdout after the marker `hoh-wal-probe: ` (other output is ignored).

use super::PersistError;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Hidden argument of the `hoh` binary that runs [`probe_main`].
pub const PROBE_ARG: &str = "__wal-probe";
/// Environment variable with the probe's database path.
pub const PROBE_DB_ENV: &str = "HOH_WAL_PROBE_DB";
const REPLY: &str = "hoh-wal-probe: ";

/// How to start the probe process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    /// Longest wait for one answer.
    pub answer_timeout: Duration,
}

impl ProbeCommand {
    /// This executable with [`PROBE_ARG`]: how the `hoh` binary runs its own probe.
    pub fn current_exe() -> std::io::Result<Self> {
        Ok(Self {
            program: std::env::current_exe()?,
            args: vec![PROBE_ARG.into()],
            env: Vec::new(),
            answer_timeout: Duration::from_secs(10),
        })
    }
}

/// The self-test's steps, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Open,
    EnableWal,
    Write,
    SharedMemory,
    Locking,
    Checkpoint,
}

impl Step {
    fn name(self) -> &'static str {
        match self {
            Step::Open => "open",
            Step::EnableWal => "enable WAL",
            Step::Write => "write",
            Step::SharedMemory => "shared wal-index between processes",
            Step::Locking => "write locking between processes",
            Step::Checkpoint => "checkpoint",
        }
    }
}

/// Run the self-test in `dir` (which must exist), with `probe` as the second process.
pub fn wal_self_test(dir: &Path, probe: &ProbeCommand) -> Result<(), PersistError> {
    run(dir, probe, &|_| Ok(()))
}

/// The self-test with `inject` called before each step; an `Err` from it fails that step (fault injection).
pub(super) fn run(
    dir: &Path,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), PersistError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let db = dir.join(format!(
        ".hoh-wal-selftest-{}-{nanos}.db",
        std::process::id()
    ));
    let result = steps(&db, probe, inject);
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let _ = std::fs::remove_file(with_suffix(&db, suffix));
    }
    result
}

fn with_suffix(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The probe process; killed and reaped when dropped.
struct Probe {
    process: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    answers: mpsc::Receiver<String>,
    timeout: Duration,
}

impl Probe {
    fn start(command: &ProbeCommand, db: &Path) -> std::io::Result<Probe> {
        // Its own process group, so that whatever it starts goes with it (M2T01R-11).
        let mut process = std::process::Command::new(&command.program)
            .process_group(0)
            .args(&command.args)
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .env(PROBE_DB_ENV, db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let missing = || std::io::Error::other("probe stdio was not piped");
        let stdin = process.stdin.take().ok_or_else(missing)?;
        let stdout = process.stdout.take().ok_or_else(missing)?;
        let (tx, answers) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                // The prefix need not start the line (a test harness may print before it on the same line).
                if let Some(at) = line.find(REPLY)
                    && tx.send(line[at + REPLY.len()..].to_owned()).is_err()
                {
                    break;
                }
            }
        });
        Ok(Probe {
            process,
            stdin: Some(stdin),
            answers,
            timeout: command.answer_timeout,
        })
    }

    fn ask(&mut self, command: &str) -> Result<String, String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| format!("cannot send `{command}`: the probe's stdin is closed"))?;
        writeln!(stdin, "{command}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("cannot send `{command}` to the probe: {e}"))?;
        self.answers
            .recv_timeout(self.timeout)
            .map_err(|_| format!("the probe did not answer `{command}`"))
    }

    fn expect(&mut self, command: &str, expected: &str) -> Result<(), String> {
        let answer = self.ask(command)?;
        if answer == expected {
            Ok(())
        } else {
            Err(format!(
                "probe answered `{answer}` to `{command}`, expected `{expected}`"
            ))
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // The group id stays valid until the leader is reaped, so the group is killed first. Its members holding
        // the stdout pipe die with it, which ends the reading thread. The leader is also killed on its own and its
        // stdin closed, so the wait below cannot hang even if the group kill failed.
        if let Some(pid) = rustix::process::Pid::from_raw(self.process.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.process.kill();
        drop(self.stdin.take());
        let _ = self.process.wait();
    }
}

fn count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
}

fn steps(
    db: &Path,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), PersistError> {
    let failed = |step: Step, detail: String| PersistError::WalUnsupported {
        step: step.name(),
        detail,
    };
    let at = |step: Step| inject(step).map_err(|d| failed(step, d));
    let sql = |step: Step| move |e: rusqlite::Error| failed(step, e.to_string());

    at(Step::Open)?;
    let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let ours = Connection::open_with_flags(db, flags).map_err(sql(Step::Open))?;

    at(Step::EnableWal)?;
    let mode: String = ours
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .map_err(sql(Step::EnableWal))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(failed(Step::EnableWal, format!("journal_mode is {mode}")));
    }

    at(Step::Write)?;
    ours.execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (1);")
        .map_err(sql(Step::Write))?;

    // The rows are only in the WAL: the other process finds them through the wal-index, and must see a write we make
    // after it opened, which only a shared (not private or per-process) wal-index shows.
    at(Step::SharedMemory)?;
    let shared = |d: String| failed(Step::SharedMemory, d);
    let mut other =
        Probe::start(probe, db).map_err(|e| shared(format!("cannot start the probe: {e}")))?;
    other.expect("open", "ok").map_err(shared)?;
    other.expect("count", "count 1").map_err(shared)?;
    ours.execute("INSERT INTO t VALUES (2)", [])
        .map_err(sql(Step::SharedMemory))?;
    other.expect("count", "count 2").map_err(shared)?;
    if !with_suffix(db, "-shm").exists() {
        return Err(shared("no -shm file".to_owned()));
    }

    // Each process must be refused the write lock while the other holds it, and see the other's commit.
    at(Step::Locking)?;
    let locking = |d: String| failed(Step::Locking, d);
    ours.busy_timeout(Duration::ZERO)
        .map_err(sql(Step::Locking))?;
    ours.execute_batch("BEGIN IMMEDIATE")
        .map_err(sql(Step::Locking))?;
    let theirs = other.ask("try-lock");
    ours.execute_batch("ROLLBACK").map_err(sql(Step::Locking))?;
    match theirs.map_err(locking)?.as_str() {
        "busy" => {}
        answer => {
            return Err(locking(format!(
                "the probe got the write lock we held ({answer})"
            )));
        }
    }
    other.expect("hold", "held").map_err(locking)?;
    match ours.execute_batch("BEGIN IMMEDIATE") {
        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {}
        Err(e) => return Err(locking(e.to_string())),
        Ok(()) => {
            let _ = ours.execute_batch("ROLLBACK");
            return Err(locking("we got the write lock the probe held".to_owned()));
        }
    }
    other
        .expect("insert-commit", "committed")
        .map_err(locking)?;
    let seen = count(&ours).map_err(sql(Step::Locking))?;
    if seen != 3 {
        return Err(locking(format!(
            "we read {seen} rows after the probe's commit, expected 3"
        )));
    }
    other.expect("quit", "bye").map_err(locking)?;
    drop(other);

    at(Step::Checkpoint)?;
    let (busy, log, done): (i64, i64, i64) = ours
        .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sql(Step::Checkpoint))?;
    let wal_len = std::fs::metadata(with_suffix(db, "-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    if busy != 0 || log != done || wal_len != 0 {
        return Err(failed(
            Step::Checkpoint,
            format!("busy={busy} log={log} checkpointed={done} wal bytes={wal_len}"),
        ));
    }
    Ok(())
}

/// Entry point of the probe process (`hoh __wal-probe`): serve commands on stdin for the database in
/// [`PROBE_DB_ENV`]. Returns the exit code.
pub fn probe_main() -> i32 {
    let Some(db) = std::env::var_os(PROBE_DB_ENV) else {
        eprintln!("{PROBE_ARG}: {PROBE_DB_ENV} is not set");
        return 64;
    };
    match probe_child(
        Path::new(&db),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        &NormalProbe,
    ) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{PROBE_ARG}: {e}");
            1
        }
    }
}

/// How the probe process acts at each point where a broken file system would show. The product's is
/// [`NormalProbe`], which only uses these defaults; the misbehaving ones that stand in for broken file systems are
/// test code (CR-006).
pub(super) trait ProbeBehaviour {
    /// Open the database.
    fn open(&self, db: &Path) -> rusqlite::Result<Connection> {
        Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_WRITE)
    }

    /// The row count to answer, given the count read now and the first one answered.
    fn count(&self, now: i64, _first: i64) -> i64 {
        now
    }

    /// An answer that stands for the probe's own to `command`, if any.
    fn answer(&self, _command: &str, _conn: &Connection) -> Option<String> {
        None
    }
}

/// The product's probe: the protocol as it is.
pub(super) struct NormalProbe;

impl ProbeBehaviour for NormalProbe {}

/// The probe's side of the protocol.
pub(super) fn probe_child(
    db: &Path,
    input: impl BufRead,
    mut output: impl Write,
    behaviour: &dyn ProbeBehaviour,
) -> Result<(), String> {
    let mut conn: Option<Connection> = None;
    let mut first_count = None;
    for line in input.lines() {
        let line = line.map_err(|e| e.to_string())?;
        let command = line.trim();
        let answer = if command == "open" {
            let opened = behaviour
                .open(db)
                .and_then(|c| c.busy_timeout(Duration::ZERO).map(|()| c));
            match opened {
                Ok(c) => {
                    conn = Some(c);
                    "ok".to_owned()
                }
                Err(e) => format!("error {e}"),
            }
        } else if command == "quit" {
            writeln!(output, "{REPLY}bye").map_err(|e| e.to_string())?;
            return output.flush().map_err(|e| e.to_string());
        } else {
            let Some(c) = conn.as_ref() else {
                return Err(format!("`{command}` before `open`"));
            };
            if let Some(answer) = behaviour.answer(command, c) {
                answer
            } else {
                match command {
                    "count" => match count(c) {
                        Ok(n) => {
                            let first = *first_count.get_or_insert(n);
                            format!("count {}", behaviour.count(n, first))
                        }
                        Err(e) => format!("error {e}"),
                    },
                    "try-lock" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => {
                            let _ = c.execute_batch("ROLLBACK");
                            "acquired".to_owned()
                        }
                        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                            "busy".to_owned()
                        }
                        Err(e) => format!("error {e}"),
                    },
                    "hold" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => "held".to_owned(),
                        Err(e) => format!("error {e}"),
                    },
                    "insert-commit" => match c.execute_batch("INSERT INTO t VALUES (3); COMMIT;") {
                        Ok(()) => "committed".to_owned(),
                        Err(e) => format!("error {e}"),
                    },
                    other => format!("error unknown command `{other}`"),
                }
            }
        };
        writeln!(output, "{REPLY}{answer}")
            .and_then(|()| output.flush())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The probe for tests: this test binary running only `probe_helper`, with `fault` passed in the environment.
#[cfg(test)]
pub(super) fn test_probe(fault: Option<&str>) -> ProbeCommand {
    ProbeCommand {
        program: std::env::current_exe().unwrap_or_default(),
        args: [
            "persistence::selftest::tests::probe_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(OsString::from)
        .to_vec(),
        env: fault
            .map(|f| vec![(OsString::from(tests::FAULT_ENV), OsString::from(f))])
            .unwrap_or_default(),
        answer_timeout: Duration::from_secs(10),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) const FAULT_ENV: &str = "HOH_WAL_PROBE_TEST_FAULT";

    /// Opens without file locking (`nolock=1`).
    struct NoLock;

    impl ProbeBehaviour for NoLock {
        fn open(&self, db: &Path) -> rusqlite::Result<Connection> {
            Connection::open_with_flags(
                format!("file:{}?nolock=1", uri_path(db)),
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_URI,
            )
        }
    }

    /// Keeps answering the first row count, as with a wal-index that is not shared.
    struct StaleCounts;

    impl ProbeBehaviour for StaleCounts {
        fn count(&self, _now: i64, first: i64) -> i64 {
            first
        }
    }

    /// Claims the write lock without taking it, as with file locks that do nothing.
    struct FakeLock;

    impl ProbeBehaviour for FakeLock {
        fn answer(&self, command: &str, conn: &Connection) -> Option<String> {
            match command {
                "try-lock" => Some("acquired".to_owned()),
                "hold" => Some("held".to_owned()),
                "insert-commit" => Some(match conn.execute_batch("INSERT INTO t VALUES (3);") {
                    Ok(()) => "committed".to_owned(),
                    Err(e) => format!("error {e}"),
                }),
                _ => None,
            }
        }
    }

    /// Gets past our write lock but takes its own for real, as with locks seen in one direction only.
    struct OneWayLock;

    impl ProbeBehaviour for OneWayLock {
        fn answer(&self, command: &str, _conn: &Connection) -> Option<String> {
            (command == "try-lock").then(|| "acquired".to_owned())
        }
    }

    /// A path for a `file:` URI: `%`, `?` and `#` percent-encoded.
    fn uri_path(path: &Path) -> String {
        let mut out = String::new();
        for c in path.to_string_lossy().chars() {
            match c {
                '%' => out.push_str("%25"),
                '?' => out.push_str("%3f"),
                '#' => out.push_str("%23"),
                c => out.push(c),
            }
        }
        out
    }

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The probe process of these tests; does nothing unless started by them.
    #[test]
    #[ignore = "helper process of the WAL self-test tests"]
    fn probe_helper() -> Result<(), String> {
        let Some(db) = std::env::var_os(PROBE_DB_ENV) else {
            return Ok(());
        };
        let behaviour: &dyn ProbeBehaviour = match std::env::var(FAULT_ENV).as_deref() {
            Ok("nolock") => &NoLock,
            Ok("stale") => &StaleCounts,
            Ok("fake-lock") => &FakeLock,
            Ok("one-way-lock") => &OneWayLock,
            _ => &NormalProbe,
        };
        probe_child(
            Path::new(&db),
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            behaviour,
        )
    }

    /// The product's probe answers as the protocol says: its count as read, no answer of its own (CR-006).
    #[test]
    fn the_product_probe_follows_the_protocol() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("p.db");
        // The self-test makes the database; the probe only opens it.
        drop(Connection::open(&db)?);
        let conn = NormalProbe.open(&db)?;
        assert_eq!(NormalProbe.count(5, 3), 5);
        for command in ["try-lock", "hold", "insert-commit", "count"] {
            assert_eq!(NormalProbe.answer(command, &conn), None, "{command}");
        }
        Ok(())
    }

    #[test]
    fn the_self_test_passes_on_a_local_directory_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        wal_self_test(dir.path(), &test_probe(None))?;
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn a_probe_without_working_locks_or_shared_memory_fails_the_test()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T01R-02: the second process sees through what one process cannot.
        // `nolock` is a real connection without file locks (SQLite then cannot share the wal-index either);
        // `fake-lock` claims locks it does not take, `one-way-lock` ignores ours only, and `stale` answers like a
        // private wal-index.
        for (fault, steps) in [
            ("nolock", [Step::SharedMemory, Step::Locking].as_slice()),
            ("fake-lock", &[Step::Locking]),
            ("one-way-lock", &[Step::Locking]),
            ("stale", &[Step::SharedMemory]),
        ] {
            let dir = tempfile::tempdir()?;
            match wal_self_test(dir.path(), &test_probe(Some(fault))) {
                Err(PersistError::WalUnsupported { step: name, detail }) => {
                    assert!(
                        steps.iter().any(|s| s.name() == name),
                        "{fault}: failed at {name}: {detail}"
                    );
                }
                other => return Err(format!("{fault}: {other:?}").into()),
            }
            assert_eq!(leftovers(dir.path()), Vec::<String>::new(), "{fault}");
        }
        Ok(())
    }

    #[test]
    fn a_probe_that_cannot_start_or_does_not_answer_fails_the_test()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let missing = ProbeCommand {
            program: dir.path().join("no-such-program"),
            args: Vec::new(),
            env: Vec::new(),
            answer_timeout: Duration::from_millis(300),
        };
        let silent = ProbeCommand {
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), "cat > /dev/null".into()],
            ..missing.clone()
        };
        for probe in [missing, silent] {
            match wal_self_test(dir.path(), &probe) {
                Err(PersistError::WalUnsupported { step, .. }) => {
                    assert_eq!(step, Step::SharedMemory.name())
                }
                other => return Err(format!("{probe:?}: {other:?}").into()),
            }
        }
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn a_probe_that_hangs_is_killed_with_everything_it_started()
    -> Result<(), Box<dyn std::error::Error>> {
        // M2T01R-11: the probe starts a child that keeps its stdout, then never answers.
        let dir = tempfile::tempdir()?;
        let pidfile = dir.path().join("child.pid");
        let probe = ProbeCommand {
            program: PathBuf::from("/bin/sh"),
            args: vec![
                "-c".into(),
                format!(
                    "sleep 600 & echo $! > '{}'; cat > /dev/null",
                    pidfile.display()
                )
                .into(),
            ],
            env: Vec::new(),
            answer_timeout: Duration::from_millis(300),
        };
        let work = dir.path().join("work");
        std::fs::create_dir(&work)?;
        assert!(wal_self_test(&work, &probe).is_err());
        let pid: u32 = std::fs::read_to_string(&pidfile)?.trim().parse()?;
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while Path::new(&format!("/proc/{pid}")).exists() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "the probe's child {pid} survived"
        );
        Ok(())
    }

    #[test]
    fn a_failing_step_is_wal_unsupported_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        // TEST-062-06 needs the host (shm mapping and checkpoint faults); each step's failure is covered here.
        for step in [
            Step::Open,
            Step::EnableWal,
            Step::Write,
            Step::SharedMemory,
            Step::Locking,
            Step::Checkpoint,
        ] {
            let dir = tempfile::tempdir()?;
            let fault = |s: Step| {
                if s == step {
                    Err("injected".to_owned())
                } else {
                    Ok(())
                }
            };
            match run(dir.path(), &test_probe(None), &fault) {
                Err(PersistError::WalUnsupported { step: name, detail }) => {
                    assert_eq!(name, step.name());
                    assert_eq!(detail, "injected");
                }
                other => return Err(format!("{step:?}: {other:?}").into()),
            }
            assert_eq!(leftovers(dir.path()), Vec::<String>::new(), "{step:?}");
        }
        Ok(())
    }

    #[test]
    fn a_directory_that_cannot_hold_a_database_fails_the_open_step()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let missing = dir.path().join("missing");
        match wal_self_test(&missing, &test_probe(None)) {
            Err(PersistError::WalUnsupported { step, .. }) => assert_eq!(step, "open"),
            other => return Err(format!("{other:?}").into()),
        }
        Ok(())
    }
}
