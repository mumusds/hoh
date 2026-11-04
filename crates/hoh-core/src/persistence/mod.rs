//! Persistence (docs/04-architecture.md, docs/05-data-api.md, REQ-062): one SQLite database per project
//! (`.hoh/state.db`) in WAL mode, written only by a single writer actor on a dedicated thread through typed requests;
//! every other connection is read-only. This module is the only place in the product that writes SQL (AC-062-05).
//!
//! Opening a [`Store`] checks, in this order, before any database file exists: the location is not on a network or
//! shared file system ([`location`], `DB_ON_NETWORK_FS`); WAL works in the directory ([`selftest`],
//! `DB_WAL_UNSUPPORTED`). Then the writer connection is opened, its PRAGMAs set and verified (journal_mode=WAL,
//! foreign_keys=1, busy_timeout, synchronous=FULL; any mismatch refuses the store), and the schema migrated
//! (`user_version`; a database from a newer binary is refused).

// The one module allowed to call SQLite (clippy.toml `disallowed-methods`, AC-062-05).
#![allow(clippy::disallowed_methods)]

mod attempts;
mod decide;
mod inputs;
pub mod location;
pub mod selftest;
mod writer;

pub use attempts::{
    AttemptFacts, AttemptMove, AttemptRow, CancelLatch, CancelReceipt, NewAttempt, Session,
};
pub use decide::Decided;
pub use inputs::{InputMove, InputRow, NewInput};
pub use writer::{Change, Commit, Committed, Entity, NewEvent, NewOutbox, StateCas};

use crate::protocol::{ErrorCode, RejectionReason};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Initial values of REQ-062.
#[derive(Debug, Clone)]
pub struct Options {
    /// SQLite busy_timeout, and the longest a caller waits for room in the writer queue.
    pub busy_timeout: Duration,
    /// Requests waiting for the writer before callers are held back.
    pub queue: usize,
    /// The second process of the WAL self-test; `None` is this executable with
    /// [`selftest::PROBE_ARG`] (the `hoh` binary).
    pub wal_probe: Option<selftest::ProbeCommand>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_millis(5000),
            queue: 64,
            wal_probe: None,
        }
    }
}

/// Why a persistence operation failed.
#[derive(Debug)]
pub enum PersistError {
    /// The database location is on a network or shared file system (`DB_ON_NETWORK_FS`).
    OnNetworkFs { path: PathBuf, fstype: String },
    /// WAL does not work in the database directory (`DB_WAL_UNSUPPORTED`).
    WalUnsupported { step: &'static str, detail: String },
    /// A connection setting differs from the required value; the store refuses writes.
    Pragma {
        name: &'static str,
        expected: String,
        actual: String,
    },
    /// The database was written by a newer binary (`user_version` above the latest migration).
    SchemaTooNew { found: i64, supported: i64 },
    /// A migration failed and was rolled back.
    Migration { version: i64, detail: String },
    /// A FOREIGN KEY, CHECK, UNIQUE or NOT NULL constraint failed; nothing of the transaction was written.
    IntegrityViolation(String),
    /// A compare-and-set found another revision; nothing of the transaction was written (`STALE_REVISION`).
    StaleRevision {
        entity: Entity,
        id: String,
        expected: i64,
        actual: Option<i64>,
    },
    /// The move is not allowed (`ILLEGAL_TRANSITION`, ADR-011): not in the transition table, not to the saved
    /// resume state, or past unresolved blockers; nothing of the transaction was written.
    IllegalTransition {
        entity: Entity,
        id: String,
        from: String,
        to: String,
        why: String,
    },
    /// A move found another state than the one it expected (an attempt changed meanwhile); nothing was written.
    StaleState {
        entity: Entity,
        id: String,
        expected: String,
        actual: Option<String>,
    },
    /// The run does not exist (`RUN_NOT_FOUND`); nothing was written.
    RunNotFound(String),
    /// A proposal that cannot be recorded (its payload violates `$defs.DecisionPayload`, or a number is out of
    /// range); nothing was written.
    InvalidProposal(String),
    /// A decision kind that is not applied yet (docs/adr/ADR-011.md §3.6); nothing was written.
    Unsupported(&'static str),
    /// The database stayed locked for busy_timeout, or the writer queue stayed full that long (`DB_BUSY`,
    /// retryable); nothing was written.
    Busy(String),
    /// The writer actor is no longer running.
    WriterStopped,
    /// Any other SQLite or I/O failure.
    Storage(String),
}

impl PersistError {
    /// The contract code, where one exists.
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            PersistError::OnNetworkFs { .. } => Some(ErrorCode::DbOnNetworkFs),
            PersistError::WalUnsupported { .. } => Some(ErrorCode::DbWalUnsupported),
            PersistError::Busy(_) => Some(ErrorCode::DbBusy),
            PersistError::RunNotFound(_) => Some(ErrorCode::RunNotFound),
            _ => None,
        }
    }

    /// The rejection reason, where one exists.
    pub fn rejection(&self) -> Option<RejectionReason> {
        match self {
            PersistError::StaleRevision { .. } => Some(RejectionReason::StaleRevision),
            PersistError::IllegalTransition { .. } => Some(RejectionReason::IllegalTransition),
            PersistError::StaleState { .. } => Some(RejectionReason::StaleRevision),
            _ => None,
        }
    }

    /// Whether the same request may succeed later unchanged.
    pub fn retryable(&self) -> bool {
        matches!(self, PersistError::Busy(_))
    }
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PersistError::OnNetworkFs { path, fstype } => {
                write!(f, "DB_ON_NETWORK_FS: {} is on {fstype}", path.display())
            }
            PersistError::WalUnsupported { step, detail } => {
                write!(
                    f,
                    "DB_WAL_UNSUPPORTED: WAL self-test failed at {step}: {detail}"
                )
            }
            PersistError::Pragma {
                name,
                expected,
                actual,
            } => write!(
                f,
                "connection setting {name} is {actual}, expected {expected}"
            ),
            PersistError::SchemaTooNew { found, supported } => write!(
                f,
                "database schema version {found} is newer than this binary supports ({supported})"
            ),
            PersistError::Migration { version, detail } => {
                write!(
                    f,
                    "migration {version} failed and was rolled back: {detail}"
                )
            }
            PersistError::IntegrityViolation(detail) => write!(f, "IntegrityViolation: {detail}"),
            PersistError::StaleRevision {
                entity,
                id,
                expected,
                actual,
            } => write!(
                f,
                "STALE_REVISION: {entity:?} {id} expected revision {expected}, found {actual:?}"
            ),
            PersistError::IllegalTransition {
                entity,
                id,
                from,
                to,
                why,
            } => write!(
                f,
                "ILLEGAL_TRANSITION: {entity:?} {id} {from} -> {to}: {why}"
            ),
            PersistError::StaleState {
                entity,
                id,
                expected,
                actual,
            } => write!(
                f,
                "{entity:?} {id} expected state {expected}, found {actual:?}"
            ),
            PersistError::RunNotFound(id) => write!(f, "RUN_NOT_FOUND: {id}"),
            PersistError::InvalidProposal(detail) => write!(f, "invalid proposal: {detail}"),
            PersistError::Unsupported(kind) => write!(f, "{kind} decisions are not applied yet"),
            PersistError::Busy(detail) => write!(f, "DB_BUSY: {detail}"),
            PersistError::WriterStopped => write!(f, "the database writer is not running"),
            PersistError::Storage(detail) => write!(f, "storage error: {detail}"),
        }
    }
}

impl std::error::Error for PersistError {}

/// Map a SQLite error: constraint failures are integrity violations, lock timeouts are busy.
fn sqlite_error(e: rusqlite::Error) -> PersistError {
    match e.sqlite_error_code() {
        Some(rusqlite::ErrorCode::ConstraintViolation) => {
            PersistError::IntegrityViolation(e.to_string())
        }
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            PersistError::Busy(e.to_string())
        }
        _ => PersistError::Storage(e.to_string()),
    }
}

/// Connection settings as read back from a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pragmas {
    pub journal_mode: String,
    pub foreign_keys: i64,
    pub busy_timeout_ms: i64,
    /// 2 = FULL.
    pub synchronous: i64,
}

/// Read a connection's settings.
fn pragmas(conn: &Connection) -> Result<Pragmas, PersistError> {
    let text = |name: &str| conn.pragma_query_value(None, name, |r| r.get::<_, String>(0));
    let int = |name: &str| conn.pragma_query_value(None, name, |r| r.get::<_, i64>(0));
    Ok(Pragmas {
        journal_mode: text("journal_mode")
            .map_err(sqlite_error)?
            .to_ascii_lowercase(),
        foreign_keys: int("foreign_keys").map_err(sqlite_error)?,
        busy_timeout_ms: int("busy_timeout").map_err(sqlite_error)?,
        synchronous: int("synchronous").map_err(sqlite_error)?,
    })
}

/// Check that a connection runs with the required settings; the first mismatch is an error.
fn verify_pragmas(conn: &Connection, busy_timeout: Duration) -> Result<(), PersistError> {
    let got = pragmas(conn)?;
    let expected_busy = i64::try_from(busy_timeout.as_millis()).unwrap_or(i64::MAX);
    let checks: [(&'static str, String, String); 4] = [
        ("journal_mode", "wal".to_owned(), got.journal_mode.clone()),
        ("foreign_keys", "1".to_owned(), got.foreign_keys.to_string()),
        (
            "busy_timeout",
            expected_busy.to_string(),
            got.busy_timeout_ms.to_string(),
        ),
        ("synchronous", "2".to_owned(), got.synchronous.to_string()),
    ];
    for (name, expected, actual) in checks {
        if expected != actual {
            return Err(PersistError::Pragma {
                name,
                expected,
                actual,
            });
        }
    }
    Ok(())
}

/// Apply the per-connection settings (journal_mode is a property of the database and set by the writer only).
fn configure(conn: &Connection, options: &Options, writer: bool) -> Result<(), PersistError> {
    if writer {
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(sqlite_error)?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(PersistError::Pragma {
                name: "journal_mode",
                expected: "wal".to_owned(),
                actual: mode,
            });
        }
    }
    conn.busy_timeout(options.busy_timeout)
        .map_err(sqlite_error)?;
    conn.pragma_update(None, "foreign_keys", true)
        .map_err(sqlite_error)?;
    conn.pragma_update(None, "synchronous", "FULL")
        .map_err(sqlite_error)?;
    verify_pragmas(conn, options.busy_timeout)
}

/// Schema migrations in order: `(user_version after it, SQL)`. A script sets its own `user_version`; the runner owns
/// the transaction, so `BEGIN`/`COMMIT` statements in a script (the contract schema has them) are skipped.
const MIGRATIONS: &[(i64, &str)] =
    &[(1, include_str!("../../../../contracts/sql/001_initial.sql"))];

/// The values `CHECK(<column> IN (...))` allows for `table` in the schema this binary migrates to, in schema order;
/// `None` if the table or the check does not exist. For code that must agree with the stored schema, such as the
/// CLI's exit code for every run state (AC-004-01).
pub fn allowed_values(table: &str, column: &str) -> Option<Vec<String>> {
    let create = format!("CREATE TABLE {table}(");
    let (_, sql) = MIGRATIONS.iter().find(|(_, sql)| sql.contains(&create))?;
    let start = sql.find(&create)?;
    let statement = &sql[start..start + sql[start..].find(");")?];
    let check = format!("CHECK({column} IN (");
    let list = &statement[statement.find(&check)? + check.len()..];
    let list = &list[..list.find(')')?];
    Some(
        list.split(',')
            .map(|v| v.trim().trim_matches('\'').to_owned())
            .collect(),
    )
}

/// Bring the database to the latest schema version; refuse one newer than this binary. Each migration runs in one
/// IMMEDIATE transaction that commits only if the script ran without ending it and set `user_version` to its
/// version (M2T01R-05, -09); any failure, COMMIT included, rolls it back (M2T01R-12).
fn migrate(conn: &Connection, migrations: &[(i64, &str)]) -> Result<i64, PersistError> {
    let latest = migrations.last().map_or(0, |m| m.0);
    let mut version = user_version(conn)?;
    if version > latest {
        return Err(PersistError::SchemaTooNew {
            found: version,
            supported: latest,
        });
    }
    let start = version;
    for &(target, sql) in migrations.iter().filter(|m| m.0 > start) {
        let failed = |detail: String| PersistError::Migration {
            version: target,
            detail,
        };
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| failed(e.to_string()))?;
        let ran = run_script(conn, sql)
            .and_then(|()| {
                let after = user_version(conn).map_err(|e| e.to_string())?;
                if after == target {
                    Ok(())
                } else {
                    Err(format!("user_version is {after} afterwards"))
                }
            })
            .and_then(|()| conn.execute_batch("COMMIT").map_err(|e| e.to_string()));
        if let Err(detail) = ran {
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            return Err(failed(detail));
        }
        version = target;
    }
    Ok(version)
}

fn user_version(conn: &Connection) -> Result<i64, PersistError> {
    conn.pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sqlite_error)
}

/// Run a script's statements one by one inside the caller's transaction. Each statement is classified by its first
/// keyword (after whitespace, a byte-order mark, empty statements and comments) before anything runs (M2T01R-15,
/// -20): the script's own `BEGIN`/`COMMIT`/`END` are skipped; `ROLLBACK`, `SAVEPOINT`, `RELEASE`, `VACUUM`, `ATTACH`
/// and `DETACH` fail the script unexecuted (they would leave or cannot run in the transaction). SQLite's read-only
/// flag is a fail-closed net, not the classifier: a read-only statement with any other keyword than a known read or
/// maintenance step (`SELECT`, `WITH`, `VALUES`, `EXPLAIN`, `PRAGMA`, `REINDEX`, `ANALYZE`) fails unexecuted too,
/// since transaction control is read-only. A statement that still ends the transaction fails the script afterwards.
fn run_script(conn: &Connection, sql: &str) -> Result<(), String> {
    use rusqlite::fallible_iterator::FallibleIterator;
    let mut batch = rusqlite::Batch::new(conn, sql);
    while let Some(mut statement) = batch.next().map_err(|e| e.to_string())? {
        let text = statement.expanded_sql().unwrap_or_default();
        let refused = || {
            format!(
                "the statement `{}` is not allowed in a migration",
                text.trim()
            )
        };
        match leading_keyword(&text).as_str() {
            "BEGIN" | "COMMIT" | "END" => continue,
            "ROLLBACK" | "SAVEPOINT" | "RELEASE" | "VACUUM" | "ATTACH" | "DETACH" => {
                return Err(refused());
            }
            "SELECT" | "WITH" | "VALUES" | "EXPLAIN" | "PRAGMA" | "REINDEX" | "ANALYZE" => {}
            _ if statement.readonly() => return Err(refused()),
            _ => {}
        }
        statement.raw_execute().map_err(|e| e.to_string())?;
        if conn.is_autocommit() {
            return Err(format!(
                "the statement `{}` ended the transaction",
                text.trim()
            ));
        }
    }
    Ok(())
}

/// The first keyword of a statement, after whitespace, a byte-order mark, empty statements (`;`) and `--` / `/* */`
/// comments, upper-cased.
fn leading_keyword(sql: &str) -> String {
    let mut rest = sql;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == ';');
        if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, tail)| tail);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, tail)| tail);
        } else {
            break;
        }
    }
    rest.split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_uppercase()
}

/// An open project database: the writer actor and the means to open readers.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    options: Options,
    writer: writer::Writer,
}

impl Store {
    /// Open (creating if needed) the database at `path`, checking its location against `/proc/self/mountinfo`.
    pub fn open(path: &Path, options: Options) -> Result<Store, PersistError> {
        let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| PersistError::Storage(format!("cannot read /proc/self/mountinfo: {e}")))?;
        Self::open_with_mountinfo(path, options, &mountinfo)
    }

    /// [`Store::open`] with the mount table given as text.
    pub fn open_with_mountinfo(
        path: &Path,
        options: Options,
        mountinfo: &str,
    ) -> Result<Store, PersistError> {
        Self::open_inner(path, options, mountinfo, MIGRATIONS, &|| {})
    }

    /// `after_check` runs right after the location check (tests swap the path there).
    fn open_inner(
        path: &Path,
        options: Options,
        mountinfo: &str,
        migrations: &[(i64, &str)],
        after_check: &dyn Fn(),
    ) -> Result<Store, PersistError> {
        // Where the database really goes: links (also a dangling one for the file) and `..` after missing
        // directories resolved, so the check, the self-test and the open all see the same place (M2T01R-01).
        let storage = |e: std::io::Error| PersistError::Storage(e.to_string());
        let network = |path: &Path| {
            let path = path.to_path_buf();
            move |fstype| PersistError::OnNetworkFs { path, fstype }
        };
        let path = location::resolve(path).map_err(storage)?;
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let existing = location::existing_ancestor(&dir).map_err(storage)?;
        location::check(&existing, mountinfo).map_err(network(&existing))?;
        std::fs::create_dir_all(&dir).map_err(storage)?;
        after_check();
        // The resolved path has no links. If one appeared since (M2T01R-08), the directory no longer resolves to
        // itself, and every open below uses SQLITE_OPEN_NOFOLLOW, which refuses a link anywhere in the path before
        // creating anything. Moving a real directory from another file system into place is impossible (rename
        // does not cross file systems); a mount over it needs root. The device of the directory and of the
        // database file are also checked against the mount table.
        if location::resolve(&dir).map_err(storage)? != dir {
            return Err(PersistError::Storage(format!(
                "{} changed while it was being checked",
                dir.display()
            )));
        }
        location::check_device(&dir, mountinfo).map_err(network(&dir))?;
        let probe = match &options.wal_probe {
            Some(probe) => probe.clone(),
            None => selftest::ProbeCommand::current_exe().map_err(storage)?,
        };
        selftest::wal_self_test(&dir, &probe)?;
        let store = Self::open_checked(&path, options, migrations)?;
        location::check_device(&path, mountinfo).map_err(network(&path))?;
        Ok(store)
    }

    fn open_checked(
        path: &Path,
        options: Options,
        migrations: &[(i64, &str)],
    ) -> Result<Store, PersistError> {
        let flags = OpenFlags::default() | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let conn = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
        configure(&conn, &options, true)?;
        migrate(&conn, migrations)?;
        let writer = writer::Writer::spawn(conn, &options)?;
        Ok(Store {
            path: path.to_path_buf(),
            options,
            writer,
        })
    }

    /// Commit `commit` in one IMMEDIATE transaction on the writer: all of it or nothing.
    pub async fn commit(&self, commit: Commit) -> Result<Committed, PersistError> {
        self.writer.commit(commit).await
    }

    /// Record a Commander proposal, judge it against the state it names, and apply or reject it, all in one
    /// transaction (REQ-035, docs/adr/ADR-011.md §3). A kind that is not applied yet is refused before anything is
    /// written.
    pub async fn decide(
        &self,
        proposal: crate::decision::Proposal,
        context: crate::decision::Context,
    ) -> Result<Decided, PersistError> {
        if !proposal.applicable() {
            return Err(PersistError::Unsupported(proposal.kind()));
        }
        self.writer.decide(proposal, context).await
    }

    /// Record a native session, or update the row of a resumed one (never a second row for it).
    pub async fn upsert_session(&self, session: Session) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::Session(session))
            .await
    }

    /// Create an attempt in state PREPARED, with its `attempt.created` event.
    pub async fn create_attempt(&self, attempt: NewAttempt) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::Create(attempt))
            .await
    }

    /// Move an attempt along the transition table, with its `attempt.moved` event.
    pub async fn move_attempt(&self, change: AttemptMove) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::Move(change))
            .await
    }

    /// End an attempt's turn: the terminal or UNKNOWN_EFFECT move `change`, or CANCELLED (with the same facts, no
    /// failure reason) if `latch` accepted a cancel before this commits; the state written. Once it succeeds, `latch`
    /// refuses cancels.
    pub async fn end_attempt(
        &self,
        change: AttemptMove,
        latch: &CancelLatch,
    ) -> Result<crate::lifecycle::AttemptState, PersistError> {
        self.writer.end_attempt(change, latch.clone()).await
    }

    /// Queue an input (QUEUED, with its `input.queued` event).
    pub async fn enqueue_input(&self, input: NewInput) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::Enqueue(input))
            .await
    }

    /// Move an input along the queue (with its `input.moved` event); see [`InputMove`].
    pub async fn move_input(&self, change: InputMove) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::MoveInput(change))
            .await
    }

    /// Append an event of attempt `attempt_id` (kind and payload as given); `name` makes its id unique within the
    /// attempt, so recording the same one twice fails instead of duplicating it.
    pub async fn attempt_event(
        &self,
        attempt_id: &str,
        name: &str,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<(), PersistError> {
        self.writer
            .attempt(attempts::AttemptJob::Event {
                attempt_id: attempt_id.to_owned(),
                name: name.to_owned(),
                kind: kind.to_owned(),
                payload,
            })
            .await
    }

    /// Record the project row if it is not there yet.
    pub async fn ensure_project(
        &self,
        project_id: &str,
        repo_path: &str,
    ) -> Result<(), PersistError> {
        self.writer
            .ensure_project(project_id.to_owned(), repo_path.to_owned())
            .await
    }

    /// A new read-only connection with verified settings.
    pub fn reader(&self) -> Result<Reader, PersistError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let conn = Connection::open_with_flags(&self.path, flags).map_err(sqlite_error)?;
        configure(&conn, &self.options, false)?;
        Ok(Reader { conn })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A read-only connection.
#[derive(Debug)]
pub struct Reader {
    conn: Connection,
}

impl Reader {
    /// Run a query on the connection (tests only); writes fail because it is read-only. No SQLite type leaves the
    /// persistence module (AC-062-05): product queries are typed methods, added with their callers.
    #[cfg(test)]
    fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, PersistError> {
        f(&self.conn).map_err(sqlite_error)
    }

    pub fn pragmas(&self) -> Result<Pragmas, PersistError> {
        pragmas(&self.conn)
    }

    /// The attempt `attempt_id`, if it exists.
    pub fn attempt(&self, attempt_id: &str) -> Result<Option<AttemptRow>, PersistError> {
        attempts::read_attempt(&self.conn, attempt_id)
    }

    /// The session `session_id`, if it exists.
    pub fn session(&self, session_id: &str) -> Result<Option<Session>, PersistError> {
        attempts::read_session(&self.conn, session_id)
    }

    /// (kind, payload) of every event of `run_id`, in seq order.
    pub fn events(&self, run_id: &str) -> Result<Vec<(String, serde_json::Value)>, PersistError> {
        attempts::read_events(&self.conn, run_id)
    }

    /// The input queue row `input_id`.
    pub fn input(&self, input_id: &str) -> Result<Option<InputRow>, PersistError> {
        inputs::read_input(&self.conn, input_id)
    }
}

#[cfg(test)]
mod tests;
