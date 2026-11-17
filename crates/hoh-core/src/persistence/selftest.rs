//! WAL self-test (REQ-062, CR-002): on a file system not known to be network or shared, the host proves at start that
//! SQLite's WAL mode works there before it creates the real database. A temporary database in the target directory
//! is switched to WAL and written; then a second **process** (two connections in one process would share SQLite's
//! in-process lock and wal-index state and prove nothing, M2T01R-02) reads it, sees a later write of ours and we see
//! one of its (the `-shm` wal-index is really shared), and each side is refused the write lock while the other holds
//! it (file locks really work). Finally the WAL is checkpointed. Any failure is `DB_WAL_UNSUPPORTED`, but for a
//! directory that refuses the temporary database's file (not writable: [`PersistError::NotWritable`]; full) and a file
//! system that is full at any step (`DB_FULL`), which are not WAL's (docs/09 fault matrix item 7); the temporary files
//! are removed in every case.
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

/// How many names the self-test tries for its database before it gives up (F7-03).
const NAME_ATTEMPTS: u32 = 8;

/// The files SQLite keeps beside a database.
const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];

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
    let pid = std::process::id();
    let names =
        (0..NAME_ATTEMPTS).map(|n| dir.join(format!(".hoh-wal-selftest-{pid}-{nanos}-{n}.db")));
    run_named(dir, names, probe, inject)
}

/// The self-test with its database at the first of `names` that this call can claim ([`claim`]). Only the files this
/// call created are removed after: the database it claimed and SQLite's companions of it, which did not exist when it
/// claimed the name (F7-03).
pub(super) fn run_named(
    dir: &Path,
    names: impl IntoIterator<Item = PathBuf>,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), PersistError> {
    let db = claim(dir, names)?;
    let result = steps(&db, probe, inject);
    for suffix in std::iter::once("").chain(COMPANIONS) {
        let _ = std::fs::remove_file(with_suffix(&db, suffix));
    }
    result
}

/// Create the self-test's database file at the first name that is free: neither the file nor a companion of it
/// exists. A name that exists is another creator's and is left to it: the next name is tried, up to the last, and
/// the self-test fails at its open step when none is free.
///
/// The file is made before SQLite opens it, so that a directory refusing it for want of permission or room says so by
/// the OS's error (SQLite says only that it cannot open the file); SQLite opens the empty file as a new database.
fn claim(dir: &Path, names: impl IntoIterator<Item = PathBuf>) -> Result<PathBuf, PersistError> {
    let mut taken = None;
    for db in names {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&db)
        {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                taken = Some(db);
                continue;
            }
            Err(e) => return Err(not_created(dir, &db, &e)),
        }
        if COMPANIONS
            .iter()
            .any(|suffix| std::fs::symlink_metadata(with_suffix(&db, suffix)).is_ok())
        {
            let _ = std::fs::remove_file(&db);
            taken = Some(db);
            continue;
        }
        return Ok(db);
    }
    Err(PersistError::WalUnsupported {
        step: Step::Open.name(),
        detail: match taken {
            Some(db) => format!(
                "every name tried for the self-test database exists already, the last {}",
                db.display()
            ),
            None => "no name to try for the self-test database".to_owned(),
        },
    })
}

/// The self-test's database file could not be created at `db` in `dir`, for `e` (not that the name exists): a
/// directory that refuses a new file is NotWritable, one without room DB_FULL, and anything else the open step's
/// failure.
fn not_created(dir: &Path, db: &Path, e: &std::io::Error) -> PersistError {
    use std::io::ErrorKind::{PermissionDenied, QuotaExceeded, ReadOnlyFilesystem, StorageFull};
    match e.kind() {
        PermissionDenied | ReadOnlyFilesystem => PersistError::NotWritable {
            path: dir.to_path_buf(),
            detail: e.to_string(),
        },
        StorageFull | QuotaExceeded => {
            PersistError::Full(format!("{} has no room: {e}", dir.display()))
        }
        _ => PersistError::WalUnsupported {
            step: Step::Open.name(),
            detail: format!("cannot create {}: {e}", db.display()),
        },
    }
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

    fn expect(&mut self, command: &str, expected: &str) -> Result<(), ProbeError> {
        let answer = self.ask(command).map_err(ProbeError::Other)?;
        if answer == expected {
            Ok(())
        } else {
            Err(ProbeError::of(command, &answer, || {
                format!("probe answered `{answer}` to `{command}`, expected `{expected}`")
            }))
        }
    }
}

/// What went wrong on the probe's side: its own failed SQLite call, carried by the protocol ([`SqlFailure`]), or
/// anything else, as text.
enum ProbeError {
    Sql {
        command: String,
        failure: SqlFailure,
    },
    Other(String),
}

impl ProbeError {
    /// The probe's `answer` to `command`, which was not the one expected; `otherwise` says why when it is not a failed
    /// SQLite call.
    fn of(command: &str, answer: &str, otherwise: impl FnOnce() -> String) -> ProbeError {
        match SqlFailure::parse(answer) {
            Some(failure) => ProbeError::Sql {
                command: command.to_owned(),
                failure,
            },
            None => ProbeError::Other(otherwise()),
        }
    }

    /// Judged at `step` by the same rule as our own failed calls ([`SqlFailure::judged`]).
    fn judged(self, step: Step) -> PersistError {
        match self {
            ProbeError::Sql { command, failure } => {
                let detail = format!("the probe's `{command}` failed: {}", failure.message);
                failure.judged(step, detail)
            }
            ProbeError::Other(detail) => PersistError::WalUnsupported {
                step: step.name(),
                detail,
            },
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

/// The OS error of the call that just failed: rusqlite exposes no `sqlite3_system_errno`, so it is read right after the
/// failing call returns, before anything else runs (a failing call sets it; the calls in between succeed and keep it).
fn errno() -> Option<i32> {
    std::io::Error::last_os_error().raw_os_error()
}

/// Whether a failed SQLite call of the self-test, by its extended result code and the OS error of the failing call, is
/// the file system out of room (DB_FULL) and not WAL's failure: SQLITE_FULL whatever its errno; an I/O error of a call
/// that grows or writes a store file (the -shm wal-index, a write, a truncate, a sync) only with ENOSPC or EDQUOT as its
/// OS error. Any other I/O error stays WAL's: an I/O error alone is never taken for a full file system (F7-01). The
/// probe's errors are judged by the same rule (F7-02).
pub(super) fn out_of_room(extended: i32, errno: Option<i32>) -> bool {
    use rusqlite::ffi;
    let room = [rustix::io::Errno::NOSPC, rustix::io::Errno::DQUOT].map(|e| e.raw_os_error());
    extended & 0xff == ffi::SQLITE_FULL
        || (matches!(
            extended,
            ffi::SQLITE_IOERR_SHMSIZE
                | ffi::SQLITE_IOERR_WRITE
                | ffi::SQLITE_IOERR_TRUNCATE
                | ffi::SQLITE_IOERR_FSYNC
        ) && errno.is_some_and(|e| room.contains(&e)))
}

/// A failed SQLite call of either process, as [`out_of_room`] judges it: its extended result code (none for an error
/// that is not SQLite's), the OS error of the failing call and SQLite's message. The probe sends it in its error answer
/// ([`SqlFailure::answer`]) and we read it back ([`SqlFailure::parse`]), so one rule judges both processes' calls
/// (F7-02).
#[derive(Debug, PartialEq)]
pub(super) struct SqlFailure {
    extended: Option<i32>,
    errno: Option<i32>,
    message: String,
}

impl SqlFailure {
    /// `e`, which just failed, with the OS error read right after it ([`errno`]).
    fn of(e: &rusqlite::Error, errno: Option<i32>) -> SqlFailure {
        SqlFailure {
            extended: e.sqlite_extended_error_code(),
            errno,
            message: e.to_string(),
        }
    }

    /// The probe's answer: `error code=<primary> extended=<extended> errno=<OS error> <message>`, `-` for a value it
    /// does not have; the message on one line.
    fn answer(&self) -> String {
        let value = |v: Option<i32>| v.map_or_else(|| "-".to_owned(), |n| n.to_string());
        format!(
            "error code={} extended={} errno={} {}",
            value(self.extended.map(|x| x & 0xff)),
            value(self.extended),
            value(self.errno),
            self.message.replace(['\r', '\n'], " ")
        )
    }

    /// An answer made by [`SqlFailure::answer`], else `None`.
    fn parse(answer: &str) -> Option<SqlFailure> {
        let value = |field: &str, key: &str| -> Option<Option<i32>> {
            match field.strip_prefix(key)? {
                "-" => Some(None),
                n => n.parse().ok().map(Some),
            }
        };
        let rest = answer.strip_prefix("error ")?;
        let (code, rest) = rest.split_once(' ')?;
        let (extended, rest) = rest.split_once(' ')?;
        let (errno, message) = rest.split_once(' ')?;
        let code = value(code, "code=")?;
        let extended = value(extended, "extended=")?;
        if code != extended.map(|x| x & 0xff) {
            return None;
        }
        Some(SqlFailure {
            extended,
            errno: value(errno, "errno=")?,
            message: message.to_owned(),
        })
    }

    /// The failure at `step`, said by `detail`: DB_FULL when [`out_of_room`], else WAL's failure at that step.
    fn judged(&self, step: Step, detail: String) -> PersistError {
        match self.extended {
            Some(extended) if out_of_room(extended, self.errno) => {
                let os = self
                    .errno
                    .map(|n| format!(" ({})", std::io::Error::from_raw_os_error(n)))
                    .unwrap_or_default();
                PersistError::Full(format!("WAL self-test at {}: {detail}{os}", step.name()))
            }
            _ => PersistError::WalUnsupported {
                step: step.name(),
                detail,
            },
        }
    }
}

/// One of our own failed SQLite calls at `step`, with the OS error of the failing call.
fn judged(step: Step, e: &rusqlite::Error, errno: Option<i32>) -> PersistError {
    SqlFailure::of(e, errno).judged(step, e.to_string())
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
    // A full file system is not WAL's failure ([`out_of_room`]); the OS error is read first, before anything else.
    let sql = |step: Step| {
        move |e: rusqlite::Error| {
            let errno = errno();
            judged(step, &e, errno)
        }
    };

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
    let from_probe = |step: Step| move |e: ProbeError| e.judged(step);
    let mut other =
        Probe::start(probe, db).map_err(|e| shared(format!("cannot start the probe: {e}")))?;
    other
        .expect("open", "ok")
        .map_err(from_probe(Step::SharedMemory))?;
    other
        .expect("count", "count 1")
        .map_err(from_probe(Step::SharedMemory))?;
    ours.execute("INSERT INTO t VALUES (2)", [])
        .map_err(sql(Step::SharedMemory))?;
    other
        .expect("count", "count 2")
        .map_err(from_probe(Step::SharedMemory))?;
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
            return Err(ProbeError::of("try-lock", answer, || {
                format!("the probe got the write lock we held ({answer})")
            })
            .judged(Step::Locking));
        }
    }
    other
        .expect("hold", "held")
        .map_err(from_probe(Step::Locking))?;
    match ours.execute_batch("BEGIN IMMEDIATE") {
        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {}
        Err(e) => return Err(sql(Step::Locking)(e)),
        Ok(()) => {
            let _ = ours.execute_batch("ROLLBACK");
            return Err(locking("we got the write lock the probe held".to_owned()));
        }
    }
    other
        .expect("insert-commit", "committed")
        .map_err(from_probe(Step::Locking))?;
    let seen = count(&ours).map_err(sql(Step::Locking))?;
    if seen != 3 {
        return Err(locking(format!(
            "we read {seen} rows after the probe's commit, expected 3"
        )));
    }
    other
        .expect("quit", "bye")
        .map_err(from_probe(Step::Locking))?;
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

/// The probe's answer for a failed SQLite call, with the OS error of the failing call ([`SqlFailure::answer`]).
pub(super) fn error_answer(e: &rusqlite::Error, errno: Option<i32>) -> String {
    SqlFailure::of(e, errno).answer()
}

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
                Err(e) => {
                    let errno = errno();
                    error_answer(&e, errno)
                }
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
                        Err(e) => {
                            let errno = errno();
                            error_answer(&e, errno)
                        }
                    },
                    "try-lock" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => {
                            let _ = c.execute_batch("ROLLBACK");
                            "acquired".to_owned()
                        }
                        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                            "busy".to_owned()
                        }
                        Err(e) => {
                            let errno = errno();
                            error_answer(&e, errno)
                        }
                    },
                    "hold" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => "held".to_owned(),
                        Err(e) => {
                            let errno = errno();
                            error_answer(&e, errno)
                        }
                    },
                    "insert-commit" => match c.execute_batch("INSERT INTO t VALUES (3); COMMIT;") {
                        Ok(()) => "committed".to_owned(),
                        Err(e) => {
                            let errno = errno();
                            error_answer(&e, errno)
                        }
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
                    Err(e) => {
                        let errno = errno();
                        error_answer(&e, errno)
                    }
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

    /// Its insert-commit hits a real SQLITE_FULL: the database may not grow (`max_page_count` at its page count) and
    /// the row needs pages. Its answer is the product probe's ([`error_answer`]).
    struct PageLimit;

    impl ProbeBehaviour for PageLimit {
        fn answer(&self, command: &str, conn: &Connection) -> Option<String> {
            (command == "insert-commit").then(|| {
                let full = conn
                    .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))
                    .and_then(|pages| {
                        conn.execute_batch(&format!("PRAGMA max_page_count = {pages};"))
                    })
                    .and_then(|()| {
                        conn.execute_batch("INSERT INTO t VALUES (zeroblob(1048576)); COMMIT;")
                    });
                match full {
                    Ok(()) => "committed".to_owned(),
                    Err(e) => {
                        let errno = errno();
                        error_answer(&e, errno)
                    }
                }
            })
        }
    }

    /// Fills the database's file system just before its insert-commit, which the product probe then runs (F7-02: the
    /// file system fills between the probe's steps).
    struct FillBeforeCommit;

    impl ProbeBehaviour for FillBeforeCommit {
        fn answer(&self, command: &str, _conn: &Connection) -> Option<String> {
            if command == "insert-commit"
                && let Some(dir) = std::env::var_os(PROBE_DB_ENV)
                    .as_deref()
                    .and_then(|db| Path::new(db).parent().map(Path::to_path_buf))
            {
                let _ = fill(&dir.join("filler"));
            }
            None
        }
    }

    /// Grow `filler` until its file system says it is full.
    fn fill(filler: &Path) -> std::io::Result<()> {
        let mut file = std::fs::File::create(filler)?;
        let block = vec![0u8; 4096];
        loop {
            match file.write_all(&block) {
                Ok(()) => {}
                Err(e) if e.raw_os_error() == Some(rustix::io::Errno::NOSPC.raw_os_error()) => {
                    return Ok(());
                }
                Err(e) => return Err(e),
            }
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
            Ok("page-limit") => &PageLimit,
            Ok("fill-before-commit") => &FillBeforeCommit,
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
        // Reaped only when /proc has no entry for it; any other failed look is an error, never "reaped" (ADR-022
        // decision 10, convention v2; this crate cannot use hoh_agents::identity::reaped).
        let reaped = || match std::fs::symlink_metadata(format!("/proc/{pid}")) {
            Ok(_) => Ok(false),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(true),
            Err(e) => Err(e),
        };
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !reaped()? && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(reaped()?, "the probe's child {pid} survived");
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
    fn a_name_another_creator_holds_is_left_to_it_and_another_is_tried()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-03: a database name that exists already (another creator's file, or a companion of that name) is not
        // this call's: the self-test takes the next name and removes only what it created.
        let dir = tempfile::tempdir()?;
        let names: Vec<PathBuf> = (0..3)
            .map(|n| dir.path().join(format!("selftest-{n}.db")))
            .collect();
        let foreign = "owned by another creator\n";
        std::fs::write(&names[0], foreign)?;
        let first = run_named(dir.path(), names.clone(), &test_probe(None), &|_| Ok(()));
        assert_eq!(
            std::fs::read_to_string(&names[0]).ok().as_deref(),
            Some(foreign),
            "{first:?}"
        );
        first?;
        assert_eq!(leftovers(dir.path()), ["selftest-0.db"]);

        // A free name whose -wal exists is taken too: neither its -wal nor any file of ours stays.
        std::fs::write(with_suffix(&names[1], "-wal"), foreign)?;
        run_named(dir.path(), names.clone(), &test_probe(None), &|_| Ok(()))?;
        assert_eq!(
            std::fs::read_to_string(with_suffix(&names[1], "-wal"))?,
            foreign
        );
        let mut left = leftovers(dir.path());
        left.sort();
        assert_eq!(left, ["selftest-0.db", "selftest-1.db-wal"]);

        // Every name taken: the self-test fails at open, and deletes none of them.
        std::fs::write(&names[2], foreign)?;
        match run_named(dir.path(), names.clone(), &test_probe(None), &|_| Ok(())) {
            Err(PersistError::WalUnsupported { step, detail }) => {
                assert_eq!(step, Step::Open.name(), "{detail}");
                assert!(detail.contains("selftest-2.db"), "{detail}");
            }
            other => return Err(format!("{other:?}").into()),
        }
        let mut left = leftovers(dir.path());
        left.sort();
        assert_eq!(
            left,
            ["selftest-0.db", "selftest-1.db-wal", "selftest-2.db"]
        );
        for kept in [&names[0], &with_suffix(&names[1], "-wal"), &names[2]] {
            assert_eq!(
                std::fs::read_to_string(kept)?,
                foreign,
                "{}",
                kept.display()
            );
        }
        Ok(())
    }

    #[test]
    fn the_probes_full_database_is_db_full_not_wal() -> Result<(), Box<dyn std::error::Error>> {
        // F7-02: a SQLITE_FULL in the probe process (its database may not grow) is judged as the self-test's own: DB_FULL,
        // retryable, not WAL's failure at the locking step.
        let dir = tempfile::tempdir()?;
        match wal_self_test(dir.path(), &test_probe(Some("page-limit"))) {
            Err(e @ PersistError::Full(_)) => {
                assert!(e.retryable(), "{e}");
                assert!(
                    e.to_string()
                        .contains("the probe's `insert-commit` failed: database or disk is full"),
                    "{e}"
                );
            }
            other => return Err(format!("{other:?}").into()),
        }
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn the_probes_error_answer_carries_its_codes_and_errno_and_is_judged_by_the_same_rule() {
        // F7-02: the protocol carries the failed call's code, extended code and OS error, and the parent judges the
        // probe's failure with the rule it uses for its own (out_of_room).
        let shm = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_SHMSIZE),
            Some("disk I/O error\nat -shm".to_owned()),
        );
        let answer = error_answer(&shm, Some(rustix::io::Errno::NOSPC.raw_os_error()));
        assert_eq!(
            answer,
            "error code=10 extended=4874 errno=28 disk I/O error at -shm"
        );
        let failure = SqlFailure::parse(&answer);
        assert_eq!(
            failure,
            Some(SqlFailure {
                extended: Some(rusqlite::ffi::SQLITE_IOERR_SHMSIZE),
                errno: Some(28),
                message: "disk I/O error at -shm".to_owned(),
            })
        );
        let not_sqlites = error_answer(&rusqlite::Error::InvalidQuery, None);
        assert_eq!(
            SqlFailure::parse(&not_sqlites).map(|f| (f.extended, f.errno)),
            Some((None, None))
        );
        for text in [
            "error database is locked",
            "error code=13 extended=14 errno=- inconsistent codes",
            "error code=13 extended=13 errno=x not a number",
            "busy",
        ] {
            assert_eq!(SqlFailure::parse(text), None, "{text}");
        }

        for (answer, full) in [
            ("error code=10 extended=4874 errno=28 disk I/O error", true),
            ("error code=10 extended=4874 errno=5 disk I/O error", false),
            (
                "error code=13 extended=13 errno=- database or disk is full",
                true,
            ),
            ("error code=- extended=- errno=28 not sqlite's", false),
        ] {
            let judged = ProbeError::of("insert-commit", answer, String::new).judged(Step::Locking);
            assert_eq!(
                matches!(judged, PersistError::Full(_)),
                full,
                "{answer}: {judged:?}"
            );
        }
        let text = ProbeError::of("hold", "held twice", || "said otherwise".to_owned())
            .judged(Step::Locking);
        assert!(
            matches!(&text, PersistError::WalUnsupported { detail, .. } if detail == "said otherwise"),
            "{text:?}"
        );
    }

    /// A tmpfs of the test's own (`sudo -n`): unmounted by [`Tmpfs::unmount`], whose failure fails the test; dropped
    /// mounted, it is unmounted, lazily if need be, and its path told if it stays.
    struct Tmpfs {
        point: PathBuf,
        mounted: bool,
        _work: tempfile::TempDir,
    }

    impl Tmpfs {
        fn sudo(args: &[&str]) -> Result<(), String> {
            let out = std::process::Command::new("sudo")
                .arg("-n")
                .args(args)
                .output()
                .map_err(|e| format!("sudo: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "sudo -n {args:?}: {} (this test needs sudo -n)",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        }

        fn new(size: &str) -> Result<Tmpfs, Box<dyn std::error::Error>> {
            let work = tempfile::tempdir()?;
            let point = work.path().join("m");
            std::fs::create_dir(&point)?;
            let options = format!(
                "size={size},uid={},gid={},mode=0755",
                rustix::process::getuid().as_raw(),
                rustix::process::getgid().as_raw()
            );
            let path = point.display().to_string();
            Tmpfs::sudo(&["mount", "-t", "tmpfs", "-o", &options, "tmpfs", &path])?;
            Ok(Tmpfs {
                point,
                mounted: true,
                _work: work,
            })
        }

        fn unmount(mut self) -> Result<(), String> {
            let path = self.point.display().to_string();
            Tmpfs::sudo(&["umount", &path])?;
            self.mounted = false;
            Ok(())
        }
    }

    impl Drop for Tmpfs {
        fn drop(&mut self) {
            if !self.mounted {
                return;
            }
            let path = self.point.display().to_string();
            if Tmpfs::sudo(&["umount", &path]).is_err()
                && Tmpfs::sudo(&["umount", "-l", &path]).is_err()
            {
                eprintln!("left mounted: {path}");
            }
        }
    }

    #[test]
    #[ignore = "mounts a tmpfs with sudo -n (docs/09 fault matrix item 7); run with --ignored"]
    fn a_file_system_that_fills_before_the_probes_commit_is_db_full()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-02 on a real file system: the probe's insert-commit finds it full (Codex's case: SQLITE_FULL in the
        // probe was DB_WAL_UNSUPPORTED, not retryable).
        let mount = Tmpfs::new("4m")?;
        let result = wal_self_test(&mount.point, &test_probe(Some("fill-before-commit")));
        let _ = std::fs::remove_file(mount.point.join("filler"));
        let left = leftovers(&mount.point);
        mount.unmount()?;
        match result {
            Err(e @ PersistError::Full(_)) => {
                assert!(e.retryable(), "{e}");
                assert!(
                    e.to_string().contains("the probe's `insert-commit` failed"),
                    "{e}"
                );
            }
            other => return Err(format!("{other:?}").into()),
        }
        assert_eq!(left, Vec::<String>::new());
        Ok(())
    }

    #[test]
    fn only_sqlite_full_or_a_growing_write_out_of_room_is_db_full() {
        // F7-01: SQLITE_FULL whatever its errno; the -shm growth, a write, a truncate or a sync only with ENOSPC or
        // EDQUOT; never another I/O error, the plain one, a read, or a growing call that failed for another reason.
        use rusqlite::ffi::*;
        use rustix::io::Errno;
        let (nospc, dquot, eio) = (
            Some(Errno::NOSPC.raw_os_error()),
            Some(Errno::DQUOT.raw_os_error()),
            Some(Errno::IO.raw_os_error()),
        );
        for (extended, errno, full) in [
            (SQLITE_FULL, None, true),
            (SQLITE_FULL, eio, true),
            (SQLITE_IOERR_SHMSIZE, nospc, true),
            (SQLITE_IOERR_SHMSIZE, dquot, true),
            (SQLITE_IOERR_WRITE, nospc, true),
            (SQLITE_IOERR_TRUNCATE, nospc, true),
            (SQLITE_IOERR_FSYNC, dquot, true),
            (SQLITE_IOERR_SHMSIZE, eio, false),
            (SQLITE_IOERR_SHMSIZE, None, false),
            (SQLITE_IOERR, nospc, false),
            (SQLITE_IOERR_READ, nospc, false),
            (SQLITE_IOERR_SHMMAP, nospc, false),
            (SQLITE_CANTOPEN, nospc, false),
        ] {
            assert_eq!(out_of_room(extended, errno), full, "{extended} {errno:?}");
        }
    }

    #[test]
    fn a_database_file_that_cannot_be_created_is_judged_by_the_os_error()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-55: no room (ENOSPC, EDQUOT) is DB_FULL; a directory that refuses a new file (EACCES, EPERM, EROFS) is
        // NotWritable, neither WAL's nor full; anything else (a missing directory, no file descriptor left) stays the
        // open step's failure, as before the file was made first.
        use rustix::io::Errno;
        let dir = Path::new("/srv/store");
        let db = dir.join(".hoh-wal-selftest-1-2-0.db");
        for (errno, want) in [
            (Errno::NOSPC, "full"),
            (Errno::DQUOT, "full"),
            (Errno::ACCESS, "not writable"),
            (Errno::PERM, "not writable"),
            (Errno::ROFS, "not writable"),
            (Errno::NOENT, "open"),
            (Errno::MFILE, "open"),
        ] {
            let e = std::io::Error::from_raw_os_error(errno.raw_os_error());
            let got = match not_created(dir, &db, &e) {
                PersistError::Full(detail) => {
                    assert!(detail.contains(&e.to_string()), "{detail}");
                    "full"
                }
                PersistError::NotWritable { path, detail } => {
                    assert_eq!((path.as_path(), detail), (dir, e.to_string()));
                    "not writable"
                }
                PersistError::WalUnsupported { step, detail } => {
                    assert_eq!(step, Step::Open.name(), "{detail}");
                    assert!(detail.contains(&db.display().to_string()), "{detail}");
                    "open"
                }
                other => return Err(format!("{e}: {other:?}").into()),
            };
            assert_eq!(got, want, "{e}");
        }

        // For real: a directory its user may not write (when that user is not root, whom permissions do not stop).
        if !rustix::process::geteuid().is_root() {
            use std::os::unix::fs::PermissionsExt as _;
            let dir = tempfile::tempdir()?;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555))?;
            let result = wal_self_test(dir.path(), &test_probe(None));
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755))?;
            match result {
                Err(PersistError::NotWritable { path, .. }) => assert_eq!(path, dir.path()),
                other => return Err(format!("{other:?}").into()),
            }
            assert_eq!(leftovers(dir.path()), Vec::<String>::new());
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
