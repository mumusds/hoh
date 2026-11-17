//! WAL self-test (REQ-062, CR-002): on a file system not known to be network or shared, the host proves at start that
//! SQLite's WAL mode works there before it creates the real database. A temporary database in the target directory
//! is switched to WAL and written; then a second **process** (two connections in one process would share SQLite's
//! in-process lock and wal-index state and prove nothing, M2T01R-02) reads it, sees a later write of ours and we see
//! one of its (the `-shm` wal-index is really shared), and each side is refused the write lock while the other holds
//! it (file locks really work). Finally the WAL is checkpointed. Any failure is `DB_WAL_UNSUPPORTED`, but for a
//! directory that refuses the self-test's private directory (not writable: [`PersistError::NotWritable`]; full) and a
//! file system that is full at a step before the checkpoint (`DB_FULL`), which are not WAL's (docs/09 fault matrix item
//! 7). Everything lives in a private directory of the call's own, removed with its files in every case
//! (docs/adr/ADR-016.md, the note's round 4).
//!
//! The second process is started from a [`ProbeCommand`]: in the product the `hoh` binary itself with the hidden
//! argument [`PROBE_ARG`], which runs [`probe_main`]. It gets the database path in [`PROBE_DB_ENV`] and answers one
//! line per command on stdout after the marker `hoh-wal-probe: ` (other output is ignored).

use super::PersistError;
use super::private::{Private, names, told};
use rusqlite::{Connection, ErrorCode, OpenFlags};
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

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

/// How many names the self-test tries for its private directory before it gives up (F7-03).
const NAME_ATTEMPTS: usize = 8;

/// The database's name in the self-test's private directory.
const DB_NAME: &str = "selftest.db";

/// The self-test with `inject` called before each step; an `Err` from it fails that step (fault injection).
pub(super) fn run(
    dir: &Path,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), PersistError> {
    let names = names(&dir.join(".hoh-wal-selftest-"), NAME_ATTEMPTS).map_err(|e| {
        PersistError::WalUnsupported {
            step: Step::Open.name(),
            detail: format!("no random name for the self-test's directory: {e}"),
        }
    })?;
    run_named(dir, names, probe, inject)
}

/// The self-test in the first of `names` it can make as its private directory ([`Private`]; a name that exists is
/// another's and is skipped; with none left, the open step fails). The database and every file of the self-test live
/// there, and the directory is removed with them after: nothing outside it is touched (docs/adr/ADR-016.md, the note's
/// round 4). A path that stays is told with the self-test's own failure, whose kind it keeps; a self-test that passed
/// fails at its cleanup, naming them (F7-202, F7-302).
pub(super) fn run_named(
    dir: &Path,
    names: impl IntoIterator<Item = PathBuf>,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), PersistError> {
    let private = Private::make_first(names).map_err(|(path, e)| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            PersistError::WalUnsupported {
                step: Step::Open.name(),
                detail: format!(
                    "every name tried for the self-test's directory exists already, the last {}",
                    path.display()
                ),
            }
        } else {
            not_created(dir, &path, &e)
        }
    })?;
    let result = steps(&private.path().join(DB_NAME), probe, inject);
    let left = private.remove();
    let cleanup = (!left.is_empty()).then(|| told(&left));
    match (result, cleanup) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(left)) => Err(PersistError::WalUnsupported {
            step: "cleanup",
            detail: left,
        }),
        (Err(failed), left) => Err(failed.with_cleanup(left)),
    }
}

/// A failure of the self-test's steps: a full file system ([`PersistError::Full`]) or WAL's
/// ([`PersistError::WalUnsupported`]) at a step. Only these, so that a leftover is told with each (F7-352).
#[derive(Debug)]
enum Failed {
    Full(String),
    Wal { step: &'static str, detail: String },
}

impl Failed {
    fn wal(step: Step, detail: String) -> Failed {
        Failed::Wal {
            step: step.name(),
            detail,
        }
    }

    /// The store's error, the cleanup's leftovers, if any, after its reason.
    fn with_cleanup(self, left: Option<String>) -> PersistError {
        let left = left
            .map(|left| format!("; cleanup: {left}"))
            .unwrap_or_default();
        match self {
            Failed::Full(detail) => PersistError::Full(format!("{detail}{left}")),
            Failed::Wal { step, detail } => PersistError::WalUnsupported {
                step,
                detail: format!("{detail}{left}"),
            },
        }
    }
}

/// The self-test's private directory could not be made at `path` in `dir`, for `e` (not that the name exists): a
/// directory that refuses a new entry is NotWritable, one without room DB_FULL, and anything else the open step's
/// failure.
fn not_created(dir: &Path, path: &Path, e: &std::io::Error) -> PersistError {
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
            detail: format!("cannot create {}: {e}", path.display()),
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

    /// Judged at `step` by the same rule as our own failed calls ([`judge`]), the room seen beside `db`.
    fn judged(self, step: Step, db: &Path) -> Failed {
        match self {
            ProbeError::Sql { command, failure } => {
                let detail = format!("the probe's `{command}` failed: {}", failure.message);
                judge(step, &failure, detail, || room(db))
            }
            ProbeError::Other(detail) => Failed::wal(step, detail),
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

/// Whether the directory of `db`, the self-test's private one, has room now ([`super::room::probe`]).
fn room(db: &Path) -> super::room::Seen {
    super::room::probe(&with_suffix(db, "-room-"))
}

/// The rule for a failed SQLite call of either process at `step`, said by `detail` (ADR-016's note, round 2): no errno
/// read after SQLite returns, which may be a later call's. A failed checkpoint is WAL's (AC-062-06), not probed, its
/// message naming the I/O and SQLite's extended code. SQLITE_FULL is SQLite's own attribution: DB_FULL. A write or
/// growth that failed ([`super::room::growing_write`]) is DB_FULL when `room`, seen right away, is refused for want of space or
/// quota; otherwise, and for any other failure, it is WAL's failure at `step` (REQ-062 behaviour 7), saying what
/// failed and what the room probe saw.
fn judge(
    step: Step,
    failure: &SqlFailure,
    detail: String,
    room: impl FnOnce() -> super::room::Seen,
) -> Failed {
    let wal = |detail: String| Failed::wal(step, detail);
    let Some(extended) = failure.extended else {
        return wal(detail);
    };
    // A failed checkpoint is WAL's (AC-062-06), whatever its I/O: not probed, said with what failed (F7-201).
    if step == Step::Checkpoint {
        let what = super::room::growing_write(extended)
            .map(|what| format!("{what} failed, "))
            .unwrap_or_default();
        return wal(format!(
            "{detail} ({what}SQLite extended code {extended}); a checkpoint's failure is WAL's, not probed for room"
        ));
    }
    if extended & 0xff == rusqlite::ffi::SQLITE_FULL {
        return Failed::Full(format!("WAL self-test at {}: {detail}", step.name()));
    }
    let Some(what) = super::room::growing_write(extended) else {
        return wal(detail);
    };
    let what = format!("{what} failed, SQLite extended code {extended}");
    let kib = super::room::BYTES >> 10;
    let seen = room();
    let left = seen.left_told();
    match seen.written {
        Err(e) if super::room::none(&e) => Failed::Full(format!(
            "WAL self-test at {}: {detail} ({what}); its directory has no room: a {kib} KiB write and fsync \
             failed: {e}{left}",
            step.name()
        )),
        Ok(()) => wal(format!(
            "{detail} ({what}); not for want of room: its directory took a {kib} KiB write and fsync{left}"
        )),
        Err(e) => wal(format!(
            "{detail} ({what}); room not known: a {kib} KiB write and fsync in its directory failed: {e}{left}"
        )),
    }
}

/// A failed SQLite call of either process, as [`judge`] takes it: its extended result code (none for an error that is
/// not SQLite's) and SQLite's message. The probe sends it in its error answer ([`SqlFailure::answer`]) and we read it
/// back ([`SqlFailure::parse`]), so one rule judges both processes' calls (F7-02).
#[derive(Debug, PartialEq)]
pub(super) struct SqlFailure {
    extended: Option<i32>,
    message: String,
}

impl SqlFailure {
    fn of(e: &rusqlite::Error) -> SqlFailure {
        SqlFailure {
            extended: e.sqlite_extended_error_code(),
            message: e.to_string(),
        }
    }

    /// The probe's answer: `error code=<primary> extended=<extended> <message>`, `-` for a code it does not have; the
    /// message on one line.
    fn answer(&self) -> String {
        let value = |v: Option<i32>| v.map_or_else(|| "-".to_owned(), |n| n.to_string());
        format!(
            "error code={} extended={} {}",
            value(self.extended.map(|x| x & 0xff)),
            value(self.extended),
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
        let (extended, message) = rest.split_once(' ')?;
        let code = value(code, "code=")?;
        let extended = value(extended, "extended=")?;
        if code != extended.map(|x| x & 0xff) {
            return None;
        }
        Some(SqlFailure {
            extended,
            message: message.to_owned(),
        })
    }
}

fn count(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT count(*) FROM t", [], |r| r.get(0))
}

fn steps(
    db: &Path,
    probe: &ProbeCommand,
    inject: &dyn Fn(Step) -> Result<(), String>,
) -> Result<(), Failed> {
    let failed = Failed::wal;
    let at = |step: Step| inject(step).map_err(|d| failed(step, d));
    // A full file system is not WAL's failure ([`judge`]): the room beside the database is seen at once.
    let sql = |step: Step| {
        move |e: rusqlite::Error| judge(step, &SqlFailure::of(&e), e.to_string(), || room(db))
    };

    at(Step::Open)?;
    let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let ours = Connection::open_with_flags(db, flags).map_err(sql(Step::Open))?;

    at(Step::EnableWal)?;
    // Run to its end: on the new database it commits (F7-101).
    let mode: String = super::first_row_to_end(&ours, "PRAGMA journal_mode=WAL", |r| r.get(0))
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
    let from_probe = |step: Step| move |e: ProbeError| e.judged(step, db);
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
    // SQLite grows the -shm it shares to 32 KiB.
    if std::fs::metadata(with_suffix(db, "-shm")).map_or(true, |m| m.len() == 0) {
        return Err(shared("no -shm file in use (missing or empty)".to_owned()));
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
            .judged(Step::Locking, db));
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
    let (busy, log, done): (i64, i64, i64) =
        super::first_row_to_end(&ours, "PRAGMA wal_checkpoint(TRUNCATE)", |r| {
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
        // As the parent opens it: no symlink on the way (SQLite refuses one, SQLITE_CANTOPEN_SYMLINK).
        Connection::open_with_flags(
            db,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
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

/// The probe's answer for a failed SQLite call ([`SqlFailure::answer`]).
pub(super) fn error_answer(e: &rusqlite::Error) -> String {
    SqlFailure::of(e).answer()
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
                Err(e) => error_answer(&e),
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
                        Err(e) => error_answer(&e),
                    },
                    "try-lock" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => {
                            let _ = c.execute_batch("ROLLBACK");
                            "acquired".to_owned()
                        }
                        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {
                            "busy".to_owned()
                        }
                        Err(e) => error_answer(&e),
                    },
                    "hold" => match c.execute_batch("BEGIN IMMEDIATE") {
                        Ok(()) => "held".to_owned(),
                        Err(e) => error_answer(&e),
                    },
                    "insert-commit" => match c.execute_batch("INSERT INTO t VALUES (3); COMMIT;") {
                        Ok(()) => "committed".to_owned(),
                        Err(e) => error_answer(&e),
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
                    Err(e) => error_answer(&e),
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
                    Err(e) => error_answer(&e),
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
        // F7-03, round 4: the private directory's name exists already (another creator's file or directory): it is
        // left as it is and the next name is made; with every name taken the open step fails and deletes nothing.
        let dir = tempfile::tempdir()?;
        let names: Vec<PathBuf> = (0..3)
            .map(|n| dir.path().join(format!("selftest-{n}")))
            .collect();
        let foreign = "owned by another creator\n";
        std::fs::write(&names[0], foreign)?;
        run_named(dir.path(), names.clone(), &test_probe(None), &|_| Ok(()))?;
        assert_eq!(std::fs::read_to_string(&names[0])?, foreign);
        assert_eq!(leftovers(dir.path()), ["selftest-0"]);

        std::fs::create_dir(&names[1])?;
        std::fs::write(names[1].join("selftest.db-journal"), foreign)?;
        std::fs::write(&names[2], foreign)?;
        match run_named(dir.path(), names.clone(), &test_probe(None), &|_| Ok(())) {
            Err(PersistError::WalUnsupported { step, detail }) => {
                assert_eq!(step, Step::Open.name(), "{detail}");
                assert!(detail.contains("selftest-2"), "{detail}");
            }
            other => return Err(format!("{other:?}").into()),
        }
        let mut left = leftovers(dir.path());
        left.sort();
        assert_eq!(left, ["selftest-0", "selftest-1", "selftest-2"]);
        for kept in [&names[0], &names[1].join("selftest.db-journal"), &names[2]] {
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
    fn the_probes_error_answer_carries_its_codes_and_is_judged_by_the_same_rule()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-02, F7-100: the protocol carries the failed call's code and extended code, no errno (one read after
        // SQLite returns may be a later call's), and the parent judges the probe's failure by the rule for its own
        // ([`judge`]), the room seen in the store's directory.
        let shm = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_IOERR_SHMSIZE),
            Some("disk I/O error\nat -shm".to_owned()),
        );
        let answer = error_answer(&shm);
        assert_eq!(answer, "error code=10 extended=4874 disk I/O error at -shm");
        assert_eq!(
            SqlFailure::parse(&answer),
            Some(SqlFailure {
                extended: Some(rusqlite::ffi::SQLITE_IOERR_SHMSIZE),
                message: "disk I/O error at -shm".to_owned(),
            })
        );
        let not_sqlites = error_answer(&rusqlite::Error::InvalidQuery);
        assert_eq!(
            SqlFailure::parse(&not_sqlites).map(|f| f.extended),
            Some(None)
        );
        for text in [
            "error database is locked",
            "error code=13 extended=14 inconsistent codes",
            "error code=13 extended=x not a number",
            "busy",
        ] {
            assert_eq!(SqlFailure::parse(text), None, "{text}");
        }

        let dir = tempfile::tempdir()?;
        let db = dir.path().join("selftest.db");
        for (answer, full) in [
            ("error code=13 extended=13 database or disk is full", true),
            ("error code=10 extended=4874 disk I/O error", false),
            ("error code=- extended=- not sqlite's", false),
        ] {
            let judged =
                ProbeError::of("insert-commit", answer, String::new).judged(Step::Locking, &db);
            assert_eq!(
                matches!(judged, Failed::Full(_)),
                full,
                "{answer}: {judged:?}"
            );
        }
        let text = ProbeError::of("hold", "held twice", || "said otherwise".to_owned())
            .judged(Step::Locking, &db);
        assert!(
            matches!(&text, Failed::Wal { detail, .. } if detail == "said otherwise"),
            "{text:?}"
        );
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
        Ok(())
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
    fn a_checkpoint_that_failed_is_the_wals_failure_whatever_its_io() {
        // F7-201: AC-062-06; not probed, its message naming the I/O and SQLite's extended code.
        use rusqlite::ffi::*;
        for (extended, says) in [
            (
                SQLITE_IOERR_FSYNC,
                "(an fsync failed, SQLite extended code 1034)",
            ),
            (
                SQLITE_IOERR_TRUNCATE,
                "(a truncate failed, SQLite extended code 1546)",
            ),
            (SQLITE_FULL, "(SQLite extended code 13)"),
            (SQLITE_IOERR_SHMMAP, "(SQLite extended code 5386)"),
        ] {
            let failure = SqlFailure {
                extended: Some(extended),
                message: "disk I/O error".to_owned(),
            };
            let judged = judge(
                Step::Checkpoint,
                &failure,
                "disk I/O error".to_owned(),
                || panic!("probed at the checkpoint"),
            );
            match &judged {
                Failed::Wal { step, detail } => {
                    assert_eq!(*step, Step::Checkpoint.name());
                    assert!(detail.contains(says), "{detail}");
                }
                other => panic!("{extended}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_failed_write_is_db_full_only_when_its_directory_has_no_room_now() {
        // F7-100, F7-302: SQLITE_FULL is DB_FULL, unprobed; a write, fsync, truncate, -shm growth or an open that
        // creates a file that failed is DB_FULL only when the room probe is refused for space or quota, and otherwise
        // WAL's, saying what failed and what the probe saw; any other code (the plain I/O error, a read, the -shm's
        // mapping, an open of a directory) is WAL's, unprobed.
        use rusqlite::ffi::*;
        use std::io::{Error, ErrorKind};
        let nospc = || Err(Error::from(ErrorKind::StorageFull));
        let dquot = || Err(Error::from(ErrorKind::QuotaExceeded));
        let eio = || Err(Error::other("I/O error"));
        let ok = || Ok(());
        type Room = fn() -> std::io::Result<()>;
        for (extended, room, want, probed) in [
            (Some(SQLITE_FULL), ok as Room, "full", false),
            (Some(SQLITE_IOERR_SHMSIZE), nospc, "full", true),
            (Some(SQLITE_IOERR_WRITE), dquot, "full", true),
            (Some(SQLITE_IOERR_FSYNC), dquot, "full", true),
            (Some(SQLITE_IOERR_TRUNCATE), nospc, "full", true),
            (Some(SQLITE_IOERR_FSYNC), ok, "not for want of room", true),
            (Some(SQLITE_IOERR_WRITE), eio, "room not known", true),
            (Some(SQLITE_IOERR_SHMMAP), nospc, "wal", false),
            (Some(SQLITE_IOERR), nospc, "wal", false),
            (Some(SQLITE_IOERR_READ), nospc, "wal", false),
            (Some(SQLITE_CANTOPEN), nospc, "full", true),
            (Some(SQLITE_CANTOPEN_ISDIR), nospc, "wal", false),
            (None, nospc, "wal", false),
        ] {
            let failure = SqlFailure {
                extended,
                message: "disk I/O error".to_owned(),
            };
            let seen = std::cell::Cell::new(false);
            let judged = judge(Step::Write, &failure, "disk I/O error".to_owned(), || {
                seen.set(true);
                super::super::room::Seen {
                    written: room(),
                    left: Vec::new(),
                }
            });
            assert_eq!(seen.get(), probed, "{extended:?}: {judged:?}");
            let got = match &judged {
                Failed::Full(_) => "full",
                Failed::Wal { detail, .. } if detail.contains("not for want of room") => {
                    "not for want of room"
                }
                Failed::Wal { detail, .. } if detail.contains("room not known") => "room not known",
                Failed::Wal { detail, .. } => {
                    assert_eq!(detail, "disk I/O error");
                    "wal"
                }
            };
            assert_eq!(got, want, "{extended:?}: {judged:?}");
            if probed {
                let x = extended.unwrap_or_default();
                assert!(
                    format!("{judged:?}").contains(&format!("SQLite extended code {x}")),
                    "{judged:?}"
                );
            }
        }
    }

    #[test]
    fn the_room_probe_writes_and_syncs_in_a_private_directory_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-100, round 4: the room probe writes in a private directory of its own beside the database, removed
        // with its file after; another's file of any name there stays as it is.
        let dir = tempfile::tempdir()?;
        let db = dir.path().join("selftest.db");
        let seen = room(&db);
        seen.written?;
        assert!(seen.left.is_empty(), "{:?}", seen.left);
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
        std::fs::write(with_suffix(&db, "-room"), "another's")?;
        std::fs::write(with_suffix(&db, "-room-"), "another's")?;
        let seen = room(&db);
        seen.written?;
        assert!(seen.left.is_empty(), "{:?}", seen.left);
        let mut left = leftovers(dir.path());
        left.sort();
        assert_eq!(left, ["selftest.db-room", "selftest.db-room-"]);
        Ok(())
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
