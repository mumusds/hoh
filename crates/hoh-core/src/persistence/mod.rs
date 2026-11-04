//! Persistence (docs/04-architecture.md, docs/05-data-api.md, REQ-062): one SQLite database per project
//! (`.hoh/state.db`) in WAL mode, written only by a single writer actor on a dedicated thread through typed requests;
//! every other connection is read-only. This module is the only place in the product that writes SQL (AC-062-05).
//!
//! Opening a [`Store`] checks, in this order, before any database file exists: the location is not on a network or
//! shared file system ([`location`], `DB_ON_NETWORK_FS`); WAL works in the directory ([`selftest`],
//! `DB_WAL_UNSUPPORTED`). Then the writer connection is opened, its PRAGMAs set and verified (journal_mode=WAL,
//! foreign_keys=1, busy_timeout, synchronous=FULL; any mismatch refuses the store), and the schema migrated
//! (`user_version`; a database from a newer binary is refused).

pub mod location;
pub mod selftest;
mod writer;

pub use writer::{Commit, Committed, Entity, NewEvent, NewOutbox, StateCas};

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
}

impl Default for Options {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_millis(5000),
            queue: 64,
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
            _ => None,
        }
    }

    /// The rejection reason, where one exists.
    pub fn rejection(&self) -> Option<RejectionReason> {
        matches!(self, PersistError::StaleRevision { .. }).then_some(RejectionReason::StaleRevision)
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
            PersistError::Busy(detail) => write!(f, "DB_BUSY: {detail}"),
            PersistError::WriterStopped => write!(f, "the database writer is not running"),
            PersistError::Storage(detail) => write!(f, "storage error: {detail}"),
        }
    }
}

impl std::error::Error for PersistError {}

/// Map a SQLite error: constraint failures are integrity violations, lock timeouts are busy.
pub(crate) fn sqlite_error(e: rusqlite::Error) -> PersistError {
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
pub fn pragmas(conn: &Connection) -> Result<Pragmas, PersistError> {
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
pub fn verify_pragmas(conn: &Connection, busy_timeout: Duration) -> Result<(), PersistError> {
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

/// Schema migrations in order: `(user_version after it, SQL)`. Each script runs its own transaction and sets
/// `user_version` (as the contract schema does).
pub(crate) const MIGRATIONS: &[(i64, &str)] =
    &[(1, include_str!("../../../../contracts/sql/001_initial.sql"))];

/// Bring the database to the latest schema version; refuse one newer than this binary.
pub(crate) fn migrate(conn: &Connection, migrations: &[(i64, &str)]) -> Result<i64, PersistError> {
    let latest = migrations.last().map_or(0, |m| m.0);
    let mut version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(sqlite_error)?;
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
        if let Err(e) = conn.execute_batch(sql) {
            if !conn.is_autocommit() {
                let _ = conn.execute_batch("ROLLBACK");
            }
            return Err(failed(e.to_string()));
        }
        version = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .map_err(sqlite_error)?;
        if version != target {
            return Err(failed(format!("user_version is {version} afterwards")));
        }
    }
    Ok(version)
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
        let dir = path.parent().unwrap_or(Path::new("."));
        let resolved =
            location::existing_ancestor(dir).map_err(|e| PersistError::Storage(e.to_string()))?;
        location::check(&resolved, mountinfo).map_err(|fstype| PersistError::OnNetworkFs {
            path: resolved.clone(),
            fstype,
        })?;
        std::fs::create_dir_all(dir).map_err(|e| PersistError::Storage(e.to_string()))?;
        selftest::wal_self_test(dir)?;
        Self::open_checked(path, options, MIGRATIONS)
    }

    fn open_checked(
        path: &Path,
        options: Options,
        migrations: &[(i64, &str)],
    ) -> Result<Store, PersistError> {
        let conn = Connection::open(path).map_err(sqlite_error)?;
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
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
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
    /// Run a query on the connection; writes fail because it is read-only.
    pub fn with<T>(
        &self,
        f: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> Result<T, PersistError> {
        f(&self.conn).map_err(sqlite_error)
    }

    pub fn pragmas(&self) -> Result<Pragmas, PersistError> {
        pragmas(&self.conn)
    }
}

#[cfg(test)]
mod tests;
