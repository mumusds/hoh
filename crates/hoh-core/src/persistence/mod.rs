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
mod blockers;
mod candidates;
mod closing;
pub use closing::{Reconciled, RecoveryOutcome};
pub use context::ReworkContext;
pub use issues::Traced;
mod commands;
mod completion;
mod context;
mod control;
mod decide;
mod evidence;
mod handover;
mod inputs;
mod issues;
mod join;
mod leases;
mod outbox;
mod outputs;
mod plan;
mod reconcile;
mod reports;
mod results;
mod retry;
mod rounds;
mod runner;
mod snapshot;
mod supervise;
mod team;
mod verification;
mod wakes;
pub use candidates::{CandidateRow, Freeze, Frozen, RunAcceptance};
pub use completion::{
    SelfTests, VerifierCompleted, VerifierCompletion, VerifierOutcome, VerifierReport,
    WriterCompleted, WriterCompletion, WriterOutcome,
};
pub use control::{CancelPass, Finished, PauseStep};
pub use evidence::{AcceptanceKey, AcceptanceRow, Recording};
pub use handover::{SessionStart, package_bytes};
pub use leases::{Grant, LEASE_TTL, LeaseView};
pub use outbox::{ConsumeEffect, Consumed, OutboxRow, Settlement, backoff};
pub use plan::Imported;
pub use reconcile::{EffectRow, UnfinishedRow};
pub use reports::{IntakeState, ReportIntake, ReportState};
pub use results::{TaskResultOutcome, VERIFIERS};
pub use retry::{Failure, Paced, Repair, RepairRequest, RepairVerdict};
pub use rounds::{Dispatched, RoundOutcome};
pub use runner::{
    ArtifactRow, DispatchNotice, Ending, Observed, Packing, PromptRecord, RUNNER, Waiting,
};
pub use snapshot::{
    AcShown, AcceptanceItem, AcceptancePage, AcceptanceView, CandidateShown, CheckShown, IssueItem,
    IssuesPage, RowShown,
};
pub use supervise::{Fired, Supervised};
pub use team::{
    Assignment, Availability, FileTeam, Handoff, SessionGrant, TeamChange, TeamConfig, TeamMember,
    TeamView, Unbound, session_id,
};
pub use verification::{ChangeRequest, Deferred, VerifierRequest};
pub use wakes::{Allocated, SUPERVISION_TURN, Stop, Supervision};
pub mod location;
pub mod selftest;
mod writer;

pub use attempts::{
    AttemptFacts, AttemptMove, AttemptRow, CancelLatch, CancelReceipt, NewAttempt, Session,
};
pub use commands::{
    CommandOutcome, EventRow, HostStart, RunRow, RunTarget, RunsPage, SubmitCommand,
};
pub use decide::{Decided, Fenced};
pub use inputs::{InputMove, InputRow, NewInput};
pub use writer::{Change, Claimed, Commit, Committed, Entity, NewEvent, NewOutbox, StateCas};

use crate::protocol::{ErrorCode, RejectionReason};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
use std::sync::Arc;
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
    /// Fault injection points; the product's [`NoFaults`] does nothing (CR-006).
    pub faults: Arc<dyn FaultHooks>,
    /// The clock the writer stamps each commit with (docs/adr/ADR-025.md decision 2: the supervisor's debounce and
    /// latency run from commit instants): the host's clock; `None`, the system clock.
    pub clock: Option<StoreClock>,
}

/// A clock for [`Options::clock`].
#[derive(Clone)]
pub struct StoreClock(pub Arc<dyn crate::delivery::Clock>);

impl std::fmt::Debug for StoreClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreClock")
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_millis(5000),
            queue: 64,
            wal_probe: None,
            faults: Arc::new(NoFaults),
            clock: None,
        }
    }
}

/// Fault injection points (REQ-036, CR-006, approved by the project owner): the product's implementation,
/// [`NoFaults`], does nothing; the implementations that make faults are test code, passed in process through
/// [`Options::faults`]. No product entry point passes another, and no environment variable, argument or file does.
pub trait FaultHooks: Send + Sync + std::fmt::Debug {
    /// Before a write transaction commits: an error fails the commit with it, and the transaction rolls back.
    fn before_commit(&self) -> Result<(), PersistError> {
        Ok(())
    }

    /// Before each WAL self-test step: an `Err` fails that step with its text.
    fn before_wal_step(&self, _step: selftest::Step) -> Result<(), String> {
        Ok(())
    }

    /// At `point` of a request of the writer named `job` ("Decide", "Commit", …; ADR-019 decision 1): a test's hook
    /// may end the process here (AC-036-02).
    fn at(&self, _job: &'static str, _point: WritePoint) {}
}

/// A point of a write request (REQ-036, ADR-019 decision 1), in order: the transaction began (its host fence
/// checked), its state changes are written, its events, its outbox messages; then it committed and its answer is
/// not yet sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePoint {
    Begun,
    StateWritten,
    EventsWritten,
    OutboxWritten,
    Committed,
}

/// The product's fault hooks: none.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFaults;

impl FaultHooks for NoFaults {}

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
    /// An interrupted end of the writer lease's holder (to UNKNOWN_EFFECT, or from RUNNING to any end but SETTLED)
    /// without the workspace's record (`WORKSPACE_RECORD_REQUIRED`): its lease goes only with the record (REQ-019 behaviour 6,
    /// docs/adr/ADR-024.md decision 9); nothing was written, the slot stays held. The caller takes the record and ends
    /// it again with it.
    WorkspaceRecordRequired(String),
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
    /// A scheduler change for a run that exists: a run keeps the parameters of its snapshot (`RUN_CONFIG_FROZEN`,
    /// CR-007); nothing was written.
    RunConfigFrozen(String),
    /// A team change the configuration's rules refuse (`INVALID_TEAM_CONFIG`, REQ-018): the key path, why, and the
    /// team revision that stays current; nothing was written.
    InvalidTeamConfig {
        key_path: String,
        message: String,
        current_revision: i64,
    },
    /// A proposal that cannot be recorded (its payload violates `$defs.DecisionPayload`, or a number is out of
    /// range); nothing was written.
    InvalidProposal(String),
    /// A decision kind that is not applied yet (docs/adr/ADR-011.md §3.6); nothing was written.
    Unsupported(&'static str),
    /// The database stayed locked for busy_timeout, or the writer queue stayed full that long (`DB_BUSY`,
    /// retryable); nothing was written.
    Busy(String),
    /// The database's file system is full or over quota, so the transaction could not commit (`DB_FULL`,
    /// retryable); nothing was written.
    Full(String),
    /// The idempotency key was accepted before with another payload (`KEY_REUSE`); nothing was written.
    KeyReuse(String),
    /// A change or removal of a row of an append-only table (`acceptance_results`, `evidence`) was refused by the
    /// database (docs/adr/ADR-029.md decision 6, REQ-053 failure mode 4); nothing was written.
    AppendOnly(String),
    /// The lease a dispatch needs is held: unexpired, or its holder attempt has not ended (`LEASE_UNAVAILABLE`,
    /// CR-007); nothing was written.
    LeaseUnavailable(String),
    /// A lease compare-and-set found another epoch: someone else was granted it meanwhile (`STALE_LEASE_EPOCH`);
    /// nothing was written.
    StaleLeaseEpoch(String),
    /// A plan was refused (REQ-038: SCORE_OUT_OF_RANGE, DAG_DUPLICATE_TASK, DAG_MISSING_DEPENDENCY, DAG_CYCLE, or a
    /// schema failure); nothing of it was imported.
    Plan(crate::dag::PlanError),
    /// The writer actor is no longer running.
    WriterStopped,
    /// A scratch-only write ([`Store::scratch_task`]) on a database that is not empty; nothing was written.
    NotScratch(String),
    /// The candidate's content was frozen by another run (`candidate_id` is its primary key, a row belongs to one run):
    /// a new commit is needed to freeze it for this run (REQ-052 decision 1 within one run, CR-017); nothing was
    /// written.
    CandidateOfAnotherRun {
        candidate_id: String,
        run_id: String,
    },
    /// Evidence the store does not take: its candidate is not frozen here, or its producer is not a qa or reviewer
    /// attempt of the candidate's run (docs/adr/ADR-027.md principle two); nothing was written.
    EvidenceRefused(String),
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
            PersistError::Full(_) => Some(ErrorCode::DbFull),
            PersistError::RunNotFound(_) => Some(ErrorCode::RunNotFound),
            PersistError::InvalidTeamConfig { .. } => Some(ErrorCode::InvalidTeamConfig),
            PersistError::RunConfigFrozen(_) => Some(ErrorCode::RunConfigFrozen),
            PersistError::Plan(plan) => plan.code(),
            _ => None,
        }
    }

    /// The rejection reason, where one exists.
    pub fn rejection(&self) -> Option<RejectionReason> {
        match self {
            PersistError::StaleRevision { .. } => Some(RejectionReason::StaleRevision),
            PersistError::IllegalTransition { .. } => Some(RejectionReason::IllegalTransition),
            PersistError::StaleState { .. } => Some(RejectionReason::StaleRevision),
            PersistError::KeyReuse(_) => Some(RejectionReason::KeyReuse),
            PersistError::LeaseUnavailable(_) => Some(RejectionReason::LeaseUnavailable),
            PersistError::StaleLeaseEpoch(_) => Some(RejectionReason::StaleLeaseEpoch),
            _ => None,
        }
    }

    /// Whether the same request may succeed later unchanged.
    pub fn retryable(&self) -> bool {
        matches!(self, PersistError::Busy(_) | PersistError::Full(_))
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
            PersistError::AppendOnly(table) => {
                write!(f, "APPEND_ONLY: {table} rows are never changed or removed")
            }
            PersistError::WorkspaceRecordRequired(attempt) => write!(
                f,
                "WORKSPACE_RECORD_REQUIRED: the interrupted writer {attempt} ends only with its workspace record"
            ),
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
            PersistError::InvalidTeamConfig {
                key_path, message, ..
            } => write!(f, "INVALID_TEAM_CONFIG: {key_path}: {message}"),
            PersistError::RunConfigFrozen(id) => write!(f, "RUN_CONFIG_FROZEN: {id}"),
            PersistError::InvalidProposal(detail) => write!(f, "invalid proposal: {detail}"),
            PersistError::Unsupported(kind) => write!(f, "{kind} decisions are not applied yet"),
            PersistError::Busy(detail) => write!(f, "DB_BUSY: {detail}"),
            PersistError::Full(detail) => write!(f, "DB_FULL: {detail}"),
            PersistError::KeyReuse(key) => write!(
                f,
                "KEY_REUSE: idempotency key {key} was accepted with another payload"
            ),
            PersistError::LeaseUnavailable(detail) => write!(f, "LEASE_UNAVAILABLE: {detail}"),
            PersistError::StaleLeaseEpoch(detail) => write!(f, "STALE_LEASE_EPOCH: {detail}"),
            PersistError::Plan(plan) => match plan.code() {
                Some(code) => write!(f, "plan refused: {code:?} {}", plan.details()),
                None => write!(f, "plan refused: {}", plan.details()),
            },
            PersistError::WriterStopped => write!(f, "the database writer is not running"),
            PersistError::NotScratch(why) => write!(f, "not a scratch database: {why}"),
            PersistError::EvidenceRefused(why) => write!(f, "evidence refused: {why}"),
            PersistError::CandidateOfAnotherRun {
                candidate_id,
                run_id,
            } => write!(
                f,
                "candidate {candidate_id} is content run {run_id} froze: make a new commit to freeze it for this run"
            ),
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
        Some(rusqlite::ErrorCode::DiskFull) => PersistError::Full(e.to_string()),
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

/// Install, on writer connection `conn`, the guard of the append-only tables (docs/adr/ADR-029.md decision 6, J1):
/// TEMP triggers that refuse UPDATE and DELETE of `acceptance_results` and `evidence` (only in this connection, so the
/// locked schema is unchanged), and recursive triggers so a REPLACE's delete is refused too.
pub(crate) fn guard_append_only(conn: &Connection) -> Result<(), PersistError> {
    conn.pragma_update(None, "recursive_triggers", true)
        .map_err(sqlite_error)?;
    for table in APPEND_ONLY_TABLES {
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM main.sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if !exists {
            continue;
        }
        for (verb, suffix) in [("UPDATE", "u"), ("DELETE", "d")] {
            conn.execute_batch(&format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS {table}_append_only_{suffix} BEFORE {verb} ON main.{table} \
                 BEGIN SELECT RAISE(ABORT, '{APPEND_ONLY}: {table}'); END;"
            ))
            .map_err(sqlite_error)?;
        }
    }
    Ok(())
}

/// The tables whose rows are never changed or removed (docs/adr/ADR-029.md decision 6).
const APPEND_ONLY_TABLES: [&str; 2] = ["acceptance_results", "evidence"];

/// The message of the append-only guard's refusal.
pub(crate) const APPEND_ONLY: &str = "APPEND_ONLY";

/// `error` as [`PersistError::AppendOnly`] when it is the append-only guard's refusal, else as [`sqlite_error`].
pub(crate) fn append_only_error(error: rusqlite::Error) -> PersistError {
    let text = error.to_string();
    match text.split_once(&format!("{APPEND_ONLY}: ")) {
        Some((_, table)) => PersistError::AppendOnly(table.trim().to_owned()),
        None => sqlite_error(error),
    }
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

/// A read-only connection to `path`.
fn open_reader(path: &Path, options: &Options) -> Result<Reader, PersistError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let conn = Connection::open_with_flags(path, flags).map_err(sqlite_error)?;
    configure(&conn, options, false)?;
    Ok(Reader { conn })
}

/// What a check's finishing task holds so that it judges and records its run even when its caller has gone
/// (docs/adr/ADR-027.md decision 6, M2T10R-03): readers and the writer's evidence request, nothing more. It keeps the
/// writer running until it is dropped.
#[derive(Debug, Clone)]
pub struct EvidenceHandle {
    path: PathBuf,
    options: Options,
    writer: writer::Writer,
}

impl EvidenceHandle {
    /// A read-only connection, as [`Store::reader`].
    pub fn reader(&self) -> Result<Reader, PersistError> {
        open_reader(&self.path, &self.options)
    }

    /// As [`Store::record_evidence`].
    pub async fn record_evidence(&self, recording: Recording) -> Result<(), PersistError> {
        self.writer.record_evidence(recording).await
    }
}

/// An open project database: the writer actor and the means to open readers.
#[derive(Debug)]
pub struct Store {
    path: PathBuf,
    options: Options,
    writer: writer::Writer,
    /// The highest committed event seq.
    watermark: tokio::sync::watch::Receiver<i64>,
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
        selftest::run(&dir, &probe, &|step| options.faults.before_wal_step(step))?;
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
        guard_append_only(&conn)?;
        let (committed, watermark) = tokio::sync::watch::channel(commands::max_seq(&conn)?);
        let writer = writer::Writer::spawn(conn, &options, committed)?;
        Ok(Store {
            path: path.to_path_buf(),
            options,
            writer,
            watermark,
        })
    }

    /// Commit `commit` in one IMMEDIATE transaction on the writer: all of it or nothing.
    pub async fn commit(&self, commit: Commit) -> Result<Committed, PersistError> {
        self.writer.commit(commit).await
    }

    /// The project's unfinished attempts (PREPARED, STARTING, RUNNING) with their tasks, process records and
    /// effects as of `now`, for the reconcile at a host's start and the runner's lease expiry (REQ-041 b1, ADR-019
    /// decisions 5 and 7).
    pub async fn reconcile_scan(
        &self,
        project_id: &str,
        now: std::time::SystemTime,
    ) -> Result<Vec<UnfinishedRow>, PersistError> {
        match self
            .writer
            .reconcile(reconcile::ReconcileJob::Scan(project_id.to_owned(), now))
            .await?
        {
            reconcile::ReconcileDone::Scanned(rows) => Ok(rows),
            reconcile::ReconcileDone::Event(_) => Err(PersistError::IntegrityViolation(
                "a scan answered with an event".to_owned(),
            )),
        }
    }

    /// The `reconcile.started` event of host epoch `host_epoch`; its seq.
    pub async fn reconcile_started(
        &self,
        project_id: &str,
        host_epoch: i64,
        unfinished: usize,
    ) -> Result<i64, PersistError> {
        self.reconcile_event(reconcile::ReconcileJob::Started {
            project_id: project_id.to_owned(),
            host_epoch,
            unfinished,
        })
        .await
    }

    /// One attempt's disposition, with what was found of its process and why (`reconcile` at a host's start,
    /// `lease_expired` when the runner found its lease expired), at `at` by the caller's clock, in one transaction;
    /// a failed attempt's task is decided by the retry policy with `draw` (ADR-020 decision 3). The seq of its last
    /// event.
    pub async fn reconcile_apply(
        &self,
        row: UnfinishedRow,
        disposition: crate::reconcile::Disposition,
        process: crate::reconcile::Process,
        cause: &'static str,
        at: std::time::SystemTime,
        draw: u32,
    ) -> Result<i64, PersistError> {
        self.reconcile_event(reconcile::ReconcileJob::Apply(Box::new(reconcile::Apply {
            row,
            disposition,
            process,
            cause,
            at,
            draw,
        })))
        .await
    }

    /// The `reconcile.completed` event of host epoch `host_epoch` with what was done; its seq.
    pub async fn reconcile_completed(
        &self,
        project_id: &str,
        host_epoch: i64,
        summary: serde_json::Value,
    ) -> Result<i64, PersistError> {
        self.reconcile_event(reconcile::ReconcileJob::Completed {
            project_id: project_id.to_owned(),
            host_epoch,
            summary,
        })
        .await
    }

    /// What is waiting for the host runner in `project_id` (ADR-019 decision 8).
    pub async fn runner_waiting(&self, project_id: &str) -> Result<Waiting, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Waiting(project_id.to_owned()))
            .await?
        {
            runner::RunnerDone::Waiting(waiting) => Ok(waiting),
            _ => Err(PersistError::IntegrityViolation(
                "a runner read answered with a write".to_owned(),
            )),
        }
    }

    /// The session.start notices PENDING in project `project_id`'s runs not ended, with their bindings now (the
    /// runner's look after its checkpoints and binder, docs/adr/ADR-024.md decision 4).
    pub async fn session_starts(
        &self,
        project_id: &str,
    ) -> Result<Vec<SessionStart>, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::SessionStarts(project_id.to_owned()))
            .await?
        {
            runner::RunnerDone::SessionStarts(starts) => Ok(starts),
            _ => Err(PersistError::IntegrityViolation(
                "a runner read answered with something else".to_owned(),
            )),
        }
    }

    /// The runner took `notice`: its receipt, the notice ACKED, the attempt STARTING with its work effect PREPARED;
    /// false when it was taken already or its attempt is no longer PREPARED.
    pub async fn runner_received(
        &self,
        notice: DispatchNotice,
        at: std::time::SystemTime,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Received(Box::new(notice), at))
            .await
    }

    /// Effect `key` of `attempt_id` moves `from` -> `to` with its event; false when it is no longer `from`.
    pub async fn runner_effect(
        &self,
        attempt_id: &str,
        key: &str,
        from: crate::reconcile::EffectState,
        to: crate::reconcile::EffectState,
        detail: serde_json::Value,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Effect {
            attempt_id: attempt_id.to_owned(),
            key: key.to_owned(),
            from,
            to,
            detail,
        })
        .await
    }

    /// `attempt_id` STARTING -> RUNNING with its process facts, its task RUNNING.
    pub async fn runner_running(
        &self,
        attempt_id: &str,
        facts: AttemptFacts,
        tracking: serde_json::Value,
    ) -> Result<bool, PersistError> {
        self.runner_running_with(attempt_id, facts, tracking, None)
            .await
    }

    /// [`Store::runner_running`] with the prompt the attempt is about to be sent, recorded in the same commit, before
    /// its start gate opens (docs/adr/ADR-026.md decision 7).
    pub async fn runner_running_with(
        &self,
        attempt_id: &str,
        facts: AttemptFacts,
        tracking: serde_json::Value,
        prompt: Option<PromptRecord>,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Running {
            attempt_id: attempt_id.to_owned(),
            facts: Box::new(facts),
            tracking,
            prompt: prompt.map(Box::new),
        })
        .await
    }

    /// `attempt_id` RUNNING -> SETTLED (its task IMPLEMENTED) or FAILED, its task then decided by the retry policy
    /// (READY at its next_retry_at, or BLOCKED; ADR-020 decision 3).
    pub async fn runner_finished(
        &self,
        attempt_id: &str,
        ending: Ending,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Finished {
            attempt_id: attempt_id.to_owned(),
            ending: Box::new(ending),
        })
        .await
    }

    /// One more repair of a task, or, past its limit, its refusal: nothing dispatched, a `need_decision` event and the
    /// task BLOCKED for a decision (REQ-042 behaviour 5; ADR-020 decision 10).
    pub async fn request_repair(
        &self,
        request: RepairRequest,
    ) -> Result<RepairVerdict, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Repair(Box::new(request)))
            .await?
        {
            runner::RunnerDone::Repair(verdict) => Ok(verdict),
            _ => Err(PersistError::IntegrityViolation(
                "a repair request answered with something else".to_owned(),
            )),
        }
    }

    /// How many repairs of `repair` run `run_id` was granted (the persisted count).
    pub async fn repair_count(&self, run_id: &str, repair: &Repair) -> Result<u32, PersistError> {
        self.reader()?.repair_count(run_id, repair)
    }

    /// One step of run `run_id`'s supervisor at `context.now()` (REQ-021, docs/adr/ADR-025.md decision 2): its events
    /// after its cursor taken in, a writer job per batch until it has caught up (so a cancel or a pause waits one batch
    /// at most, v13), then the triggers due fired as its state allows.
    pub async fn supervise(
        &self,
        run_id: &str,
        context: crate::decision::Context,
    ) -> Result<Supervised, PersistError> {
        loop {
            match self
                .writer
                .runner(runner::RunnerJob::Supervise {
                    run_id: run_id.to_owned(),
                    context: Box::new(context.clone()),
                })
                .await?
            {
                runner::RunnerDone::Supervised(Some(done)) => {
                    // Then its outputs, a job per batch until none is left (v16).
                    while self
                        .runner_step(runner::RunnerJob::Dispose {
                            run_id: run_id.to_owned(),
                            upto: done.cursor,
                            context: Box::new(context.clone()),
                        })
                        .await?
                    {}
                    return Ok(done);
                }
                runner::RunnerDone::Supervised(None) => {}
                _ => {
                    return Err(PersistError::IntegrityViolation(
                        "a supervisor step answered with something else".to_owned(),
                    ));
                }
            }
        }
    }

    /// One owed wake of project `project_id` given a Commander turn at `context.now()`, if the slot is free and a
    /// run admits one (docs/adr/ADR-025.md decision 3): its supervision task, PREPARED attempt, lease and dispatch.
    pub async fn allocate_supervision(
        &self,
        project_id: &str,
        context: crate::decision::Context,
    ) -> Result<Option<Allocated>, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Allocate {
                project_id: project_id.to_owned(),
                context: Box::new(context),
            })
            .await?
        {
            runner::RunnerDone::Allocated(done) => Ok(done),
            _ => Err(PersistError::IntegrityViolation(
                "an allocation answered with something else".to_owned(),
            )),
        }
    }

    /// The supervision turns of project `project_id` the host must stop at `context.now()` (docs/adr/ADR-025.md
    /// decision 3): those a preempt names, and those in flight longer than a turn's limit.
    pub async fn supervision_stops(
        &self,
        project_id: &str,
        context: crate::decision::Context,
    ) -> Result<Vec<Stop>, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Stops {
                project_id: project_id.to_owned(),
                context: Box::new(context),
            })
            .await?
        {
            runner::RunnerDone::Stops(stops) => Ok(stops),
            _ => Err(PersistError::IntegrityViolation(
                "a read of the turns to stop answered otherwise".to_owned(),
            )),
        }
    }

    /// Run `run_id`'s supervisor step failed with `error` (docs/adr/ADR-025.md v15 decision 5): recorded as
    /// `supervisor.step_failed` unless the run already has one with this error (once per run and error); whether it was
    /// written.
    pub async fn supervision_failed(
        &self,
        run_id: &str,
        error: &str,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::StepFailed(
            run_id.to_owned(),
            error.to_owned(),
        ))
        .await
    }

    /// Withdraw supervision turn `attempt_id` while it is PREPARED (docs/adr/ADR-025.md decision 3); whether it was.
    pub async fn withdraw_turn(
        &self,
        attempt_id: &str,
        at: std::time::SystemTime,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::WithdrawTurn(attempt_id.to_owned(), at))
            .await
    }

    /// One look at run `run_id` as a whole at `context.now()` (ADR-020 decisions 4, 5 and 7; docs/adr/ADR-025.md
    /// decision 5): woken from WAITING_EXTERNAL when a retry is due; BLOCKED while blocker claims are open (a cost
    /// limit it cannot enforce, a task its session cannot run, nothing able to move on), back once they are released;
    /// INCOMPLETE when its budget stops it; WAITING_EXTERNAL when only retries are left.
    pub async fn pace_run(
        &self,
        run_id: &str,
        context: crate::decision::Context,
    ) -> Result<Paced, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Pace {
                run_id: run_id.to_owned(),
                context: Box::new(context),
            })
            .await?
        {
            runner::RunnerDone::Paced(paced) => Ok(paced),
            _ => Err(PersistError::IntegrityViolation(
                "a pace answered with something else".to_owned(),
            )),
        }
    }

    /// Run `run_id`'s team checkpoint: the roles the project's current revision changes and that have no live attempt
    /// take their new binding (docs/adr/ADR-022.md decision 8); whether anything changed.
    pub async fn team_checkpoint(
        &self,
        run_id: &str,
        availability: impl Into<Availability>,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Checkpoint(
            run_id.to_owned(),
            availability.into(),
        ))
        .await
    }

    /// Grant the session of a binding (docs/adr/ADR-022.md decision 9); whether the binding was still the one granted.
    pub async fn bind_session(&self, grant: SessionGrant) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::BindSession(Box::new(grant)))
            .await
    }

    /// Block a binding whose agent is unavailable (decision 9), saying why; whether it was still waiting.
    pub async fn block_binding(
        &self,
        unbound: Unbound,
        detail: String,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::BlockBinding(Box::new(unbound), detail))
            .await
    }

    /// A runtime notice taken (ACKED); false when it was not PENDING.
    pub async fn ack_notice(&self, message_id: &str) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::Ack(message_id.to_owned()))
            .await
    }

    /// Settle session.start notice `message_id` as delivered to `session_id`, which must be its role's session at the
    /// notice's own epoch (docs/adr/ADR-024.md decision 4); whether it was PENDING and so settled.
    pub async fn deliver_session_start(
        &self,
        message_id: &str,
        session_id: &str,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::SessionStart(
            message_id.to_owned(),
            Some(session_id.to_owned()),
        ))
        .await
    }

    /// Record once that the executor did not take session.start notice `message_id`, with `detail` (its answer): one
    /// handoff.undelivered event while the notice is PENDING (docs/adr/ADR-024.md decision 4, M2T17R-150); whether it
    /// was written now.
    pub async fn session_start_undelivered(
        &self,
        message_id: &str,
        detail: &str,
    ) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::SessionStartUndelivered(
            message_id.to_owned(),
            detail.to_owned(),
        ))
        .await
    }

    /// Settle session.start notice `message_id` as superseded, its role's binding being past its epoch (decision 4);
    /// whether it was PENDING and so settled.
    pub async fn supersede_session_start(&self, message_id: &str) -> Result<bool, PersistError> {
        self.runner_step(runner::RunnerJob::SessionStart(message_id.to_owned(), None))
            .await
    }

    /// Renew the unexpired leases `attempts` hold, at `now` for [`LEASE_TTL`]; how many were renewed.
    pub async fn renew_leases(
        &self,
        attempts: Vec<String>,
        now: std::time::SystemTime,
    ) -> Result<usize, PersistError> {
        match self
            .writer
            .runner(runner::RunnerJob::Renew(attempts, now))
            .await?
        {
            runner::RunnerDone::Renewed(n) => Ok(n),
            _ => Err(PersistError::IntegrityViolation(
                "a renewal answered with something else".to_owned(),
            )),
        }
    }

    async fn runner_step(&self, job: runner::RunnerJob) -> Result<bool, PersistError> {
        match self.writer.runner(job).await? {
            runner::RunnerDone::Applied(applied) => Ok(applied),
            _ => Err(PersistError::IntegrityViolation(
                "a runner write answered with a read".to_owned(),
            )),
        }
    }

    /// The attempts of run `run_id` that start or run, read in the writer's order (a cancel's step, ADR-021
    /// decision 8).
    pub async fn running_attempts(&self, run_id: &str) -> Result<Vec<AttemptRow>, PersistError> {
        match self
            .writer
            .control(control::ControlJob::Running(run_id.to_owned()))
            .await?
        {
            control::ControlDone::Running(rows) => Ok(rows),
            _ => Err(PersistError::IntegrityViolation(
                "a read of running attempts answered otherwise".to_owned(),
            )),
        }
    }

    /// A pausing run's step (REQ-043 behaviour 1, docs/adr/ADR-021.md decision 7): its dispatches not yet taken are
    /// withdrawn, and it is PAUSED once none of its attempts starts or runs.
    pub async fn pause_step(
        &self,
        run_id: &str,
        at: std::time::SystemTime,
    ) -> Result<PauseStep, PersistError> {
        match self
            .writer
            .control(control::ControlJob::PauseStep {
                run_id: run_id.to_owned(),
                at,
            })
            .await?
        {
            control::ControlDone::Paused(step) => Ok(step),
            _ => Err(PersistError::IntegrityViolation(
                "a pause step answered otherwise".to_owned(),
            )),
        }
    }

    /// Record one cancel pass over an attempt's processes (REQ-043, docs/adr/ADR-021.md decision 8c): its steps and
    /// how it ended, together; a pass that found the group empty ends the attempt CANCELLED.
    pub async fn cancel_pass(&self, pass: CancelPass) -> Result<i64, PersistError> {
        match self
            .writer
            .control(control::ControlJob::CancelPass(pass))
            .await?
        {
            control::ControlDone::Recorded(seq) => Ok(seq),
            _ => Err(PersistError::IntegrityViolation(
                "a cancel pass answered otherwise".to_owned(),
            )),
        }
    }

    /// The run's last cancel transaction once none of its attempts starts or runs (ADR-021 decision 8f), with its
    /// workspace record.
    pub async fn finish_cancel(
        &self,
        run_id: &str,
        workspace: serde_json::Value,
        at: std::time::SystemTime,
    ) -> Result<Finished, PersistError> {
        match self
            .writer
            .control(control::ControlJob::FinishCancel {
                run_id: run_id.to_owned(),
                workspace,
                at,
            })
            .await?
        {
            control::ControlDone::Finished(finished) => Ok(finished),
            _ => Err(PersistError::IntegrityViolation(
                "a cancel finish answered otherwise".to_owned(),
            )),
        }
    }

    async fn reconcile_event(&self, job: reconcile::ReconcileJob) -> Result<i64, PersistError> {
        match self.writer.reconcile(job).await? {
            reconcile::ReconcileDone::Event(seq) => Ok(seq),
            reconcile::ReconcileDone::Scanned(_) => Err(PersistError::IntegrityViolation(
                "a reconcile write answered with rows".to_owned(),
            )),
        }
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

    /// Pass a Commander's steer, query or other message (not a decision) through its sender's fence when it enters
    /// (docs/adr/ADR-024.md decision 6): one of an epoch the commander's binding has passed is only history, a decision
    /// REJECTED STALE_ROLE_EPOCH; one of the current epoch passes with nothing written.
    pub async fn commander_message(
        &self,
        message: &crate::protocol::Message,
    ) -> Result<Fenced, PersistError> {
        self.writer.fence(message.clone()).await
    }

    /// Change acceptance row `key` to `status`: refused, always (docs/adr/ADR-029.md decision 6, REQ-053 failure mode
    /// 4). The statement is issued; the database's append-only guard refuses it and nothing changes.
    pub async fn update_acceptance_result(
        &self,
        key: AcceptanceKey,
        status: &str,
    ) -> Result<(), PersistError> {
        self.writer.amend(key, status.to_owned()).await
    }

    /// Record a check's evidence in one transaction (docs/adr/ADR-027.md decisions 7 to 9): the evidence row and its
    /// acceptance rows, its objects, `evidence.recorded`, `integrity_violation` when the source changed, and the
    /// candidate made INVALID when the evidence is.
    pub async fn record_evidence(&self, recording: Recording) -> Result<(), PersistError> {
        self.writer.record_evidence(recording).await
    }

    /// Freeze a candidate for its run in one transaction (docs/adr/ADR-027.md decision 5): a new FROZEN row whose run's
    /// earlier current candidates go STALE, its export and inputs registered and a `candidate.frozen` event; the same
    /// content again in the run returns its id and state; another run's content is refused.
    pub async fn freeze_candidate(&self, freeze: Freeze) -> Result<Frozen, PersistError> {
        self.writer.freeze_candidate(freeze).await
    }

    /// A successful writer attempt's completion in one commit (docs/adr/ADR-028.md decisions 3 and 4): freeze, accept
    /// and settle; or a protocol correction; or a decision. Nothing written when the attempt already ended.
    pub async fn complete_writer_attempt(
        &self,
        completion: WriterCompletion,
    ) -> Result<WriterCompleted, PersistError> {
        self.writer.complete_writer_attempt(completion).await
    }

    /// Clear task `task_id`'s standing repair refusal by decision `decision_id` (docs/adr/ADR-028.md decision 12; M2-T13's
    /// AnswerQuestion calls it in its transaction, CR-018).
    pub async fn clear_repair_refusal(
        &self,
        task_id: &str,
        decision_id: &str,
    ) -> Result<(), PersistError> {
        let (project_id, run_id) = self
            .reader()?
            .task_run(task_id)?
            .ok_or_else(|| PersistError::IntegrityViolation(format!("no task {task_id}")))?;
        self.commit(Commit {
            events: vec![NewEvent {
                project_id,
                event_id: crate::ids::child_id(task_id, &format!("refusal_cleared/{decision_id}")),
                run_id: Some(run_id),
                kind: "repair.refusal_cleared".to_owned(),
                payload: serde_json::json!({"task_id": task_id, "decision_id": decision_id}),
            }],
            ..Commit::default()
        })
        .await
        .map(drop)
    }

    /// Record one recovery of candidate `candidate_id`'s pending check records at `now` (docs/adr/ADR-028.md decision
    /// 11): how many it recorded, or its failure (permanent, and why).
    pub async fn record_recovery(
        &self,
        candidate_id: &str,
        outcome: RecoveryOutcome,
        now: std::time::SystemTime,
    ) -> Result<(), PersistError> {
        self.writer
            .record_recovery(candidate_id.to_owned(), outcome, now, false)
            .await
    }

    /// Record candidate `candidate_id`'s recovery at the host's start (docs/adr/ADR-028.md decision 16): counted as a
    /// tick's; a failure also wakes the Commander unless the candidate was superseded, whose failure is never judged.
    pub async fn record_start_recovery(
        &self,
        candidate_id: &str,
        outcome: RecoveryOutcome,
        now: std::time::SystemTime,
    ) -> Result<(), PersistError> {
        self.writer
            .record_recovery(candidate_id.to_owned(), outcome, now, true)
            .await
    }

    /// Run `run_id`'s verification reconcile (docs/adr/ADR-028.md decision 11): one transaction, idempotent; `pending`
    /// holds the candidates with check records still to be recorded (`.hoh/pending/`).
    pub async fn reconcile_verification(
        &self,
        run_id: &str,
        pending: std::collections::BTreeSet<String>,
    ) -> Result<Reconciled, PersistError> {
        self.writer
            .reconcile_verification(run_id.to_owned(), pending)
            .await
    }

    /// Complete a successful verifier attempt from its bridge's result in one commit (docs/adr/ADR-028.md decision 11).
    pub async fn complete_verifier_attempt(
        &self,
        completion: VerifierCompletion,
    ) -> Result<VerifierCompleted, PersistError> {
        self.writer.complete_verifier_attempt(completion).await
    }

    /// Take a verifier's report in, in a transaction of its own (docs/adr/ADR-029.md decision 7): judged and kept as an
    /// `independent_reports` row CURRENT, STALE or REJECTED (`report.rejected`), or no row when its session is unknown;
    /// no task or attempt moves. A verifier's completion takes its report in within its own commit.
    pub async fn record_report(&self, intake: ReportIntake) -> Result<IntakeState, PersistError> {
        self.writer.record_report(intake).await
    }

    /// Answer a verifier's request through its bridge (docs/adr/ADR-028.md decision 13): a change request refused with
    /// an OPEN Issue on the candidate (its id returned), any other message deferred.
    pub async fn verifier_request(
        &self,
        request: VerifierRequest,
    ) -> Result<Option<String>, PersistError> {
        self.writer.verifier_request(request).await
    }

    /// Validate a plan (`$defs.PlanProposal`) for `run_id` and import it whole, or refuse it with nothing written
    /// (REQ-038, AC-038-03).
    pub async fn import_plan(
        &self,
        run_id: &str,
        plan: serde_json::Value,
    ) -> Result<Imported, PersistError> {
        self.writer.import_plan(run_id.to_owned(), plan).await
    }

    /// One scheduling round of `run_id` (REQ-038): the best eligible task whose slot is free is leased to a new
    /// PREPARED attempt; nothing is written when none is.
    pub async fn schedule_round(
        &self,
        run_id: &str,
        context: crate::decision::Context,
    ) -> Result<RoundOutcome, PersistError> {
        self.writer.schedule_round(run_id.to_owned(), context).await
    }

    /// Take in the Developer's TaskResult (ADR-018 decision 5): `candidate_ready` and one verification handoff each
    /// for qa and reviewer, or one rejection event; the same result again replays.
    pub async fn accept_task_result(
        &self,
        result: crate::protocol::TaskResult,
    ) -> Result<TaskResultOutcome, PersistError> {
        self.writer
            .outbox(writer::OutboxJob::TaskResult(Box::new(result)))
            .await
            .and_then(|done| {
                done.task_result
                    .ok_or_else(|| PersistError::Storage("no outcome".to_owned()))
            })
    }

    /// Take the next outbox message due at `now` for one of `roles` and start its delivery; rows whose deliveries
    /// are spent go to DEAD_LETTER on the way (ADR-018 decision 4).
    pub async fn claim_outbox(
        &self,
        now: std::time::SystemTime,
        roles: Vec<String>,
        max_attempts: i64,
    ) -> Result<Option<Claimed>, PersistError> {
        self.writer
            .outbox(writer::OutboxJob::Claim(now, roles, max_attempts))
            .await
            .map(|done| done.rows.into_iter().next())
    }

    /// Settle delivery `generation` (the row's `attempt_count` when it was taken) of `message_id` (ADR-018 decision
    /// 4); a failure of an earlier generation is ignored.
    pub async fn settle_outbox(
        &self,
        message_id: &str,
        generation: i64,
        settlement: Settlement,
        max_attempts: i64,
    ) -> Result<(), PersistError> {
        self.writer
            .outbox(writer::OutboxJob::Settle(
                message_id.to_owned(),
                generation,
                settlement,
                max_attempts,
            ))
            .await
            .map(drop)
    }

    /// Consume `message` as `consumer` once behind its fences, with `effect` if it passes (ADR-018 decisions 1-2).
    pub async fn consume(
        &self,
        consumer: &str,
        message: crate::protocol::Message,
        effect: ConsumeEffect,
        now: std::time::SystemTime,
    ) -> Result<Consumed, PersistError> {
        self.writer
            .outbox(writer::OutboxJob::Consume(Box::new((
                consumer.to_owned(),
                message,
                effect,
                now,
            ))))
            .await
            .and_then(|done| {
                done.consumed
                    .ok_or_else(|| PersistError::Storage("no answer".to_owned()))
            })
    }

    /// Record that `consumer` got `message_id`, which is no valid message (ADR-018 decision 3); the event's seq.
    pub async fn record_invalid_message(
        &self,
        consumer: &str,
        message_id: &str,
        run_id: Option<&str>,
        violations: &str,
    ) -> Result<i64, PersistError> {
        self.writer
            .outbox(writer::OutboxJob::Invalid(Box::new((
                consumer.to_owned(),
                message_id.to_owned(),
                run_id.map(str::to_owned),
                violations.to_owned(),
            ))))
            .await
            .and_then(|done| {
                done.event_seq
                    .ok_or_else(|| PersistError::Storage("no event".to_owned()))
            })
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

    /// For a scratch database only (`hoh doctor --live`, ADR-015): a project, a spec, a RUNNING run `run_id` and a
    /// LEASED task `task_id` to hang attempts on, in one transaction. Refused ([`PersistError::NotScratch`], nothing
    /// written) if the database has any run, so it never stands in for the command path of a real project.
    pub async fn scratch_task(
        &self,
        project_id: &str,
        repo_path: &str,
        run_id: &str,
        task_id: &str,
    ) -> Result<(), PersistError> {
        self.writer
            .scratch_task([
                project_id.to_owned(),
                repo_path.to_owned(),
                run_id.to_owned(),
                task_id.to_owned(),
            ])
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

    /// Apply a command through the host API: idempotent by its key (REQ-022, REQ-041).
    pub async fn submit_command(
        &self,
        command: SubmitCommand,
    ) -> Result<CommandOutcome, PersistError> {
        self.writer.command(command).await
    }

    /// Start a host on this database for the project at `repo_path` (`project_id` if it is new): integrity check
    /// and the next host epoch (REQ-041).
    pub async fn begin_host(
        &self,
        project_id: &str,
        repo_path: &str,
    ) -> Result<HostStart, PersistError> {
        self.writer
            .begin_host(project_id.to_owned(), repo_path.to_owned())
            .await
    }

    /// The writer connection's settings.
    pub async fn writer_pragmas(&self) -> Result<Pragmas, PersistError> {
        self.writer.pragmas().await
    }

    /// Hand `held` to the writer thread: it is dropped only when the writer has ended, after every write is done
    /// and the connection is closed (a host's project lock, ADR-016 decision 12). The writer ends once every handle
    /// of this store is gone. A writer that panics leaks `held` instead: it stays held until the process ends.
    pub async fn keep_until_closed(
        &self,
        held: Box<dyn std::any::Any + Send>,
    ) -> Result<(), PersistError> {
        self.writer.keep(held).await
    }

    /// Resolves once the writer thread has ended: true when it finished, after this store and every other handle were
    /// dropped (what it kept is then dropped); false when it ended early by a panic (what it kept is then leaked,
    /// still held).
    pub fn writer_ended(&self) -> impl std::future::Future<Output = bool> + Send + 'static {
        self.writer.ended()
    }

    /// Whether the writer still takes requests; false once its thread has ended. During a panic's unwinding the
    /// request that panicked is answered (the writer stopped) a moment before this turns false: wait for
    /// [`Store::writer_ended`] to know.
    pub fn writer_alive(&self) -> bool {
        self.writer.alive()
    }

    /// The highest committed event seq, updated after each commit (REQ-036: subscribers learn of an event only after
    /// its transaction committed).
    pub fn watermark(&self) -> tokio::sync::watch::Receiver<i64> {
        self.watermark.clone()
    }

    /// A new read-only connection with verified settings.
    pub fn reader(&self) -> Result<Reader, PersistError> {
        open_reader(&self.path, &self.options)
    }

    /// A handle that outlives this borrow, for a check's finishing task (docs/adr/ADR-027.md decision 6).
    pub fn evidence_handle(&self) -> EvidenceHandle {
        EvidenceHandle {
            path: self.path.clone(),
            options: self.options.clone(),
            writer: self.writer.clone(),
        }
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

    /// The cost of run `run_id` and whether it is known (CR-009: `RunResult.cost_usd`/`cost_status`).
    pub fn run_cost(
        &self,
        run_id: &str,
    ) -> Result<(Option<f64>, crate::protocol::CostStatus), PersistError> {
        retry::run_cost(&self.conn, run_id)
    }

    /// How many repairs of `repair` run `run_id` was granted.
    pub fn repair_count(&self, run_id: &str, repair: &Repair) -> Result<u32, PersistError> {
        retry::repair_count(&self.conn, run_id, repair)
    }

    /// What a verification turn's context is packed from, given its handover package (docs/adr/ADR-026.md decisions 3
    /// and 10): the collector M2-T16's QA and Reviewer dispatch packs with.
    /// The candidate `candidate_id`'s row (docs/adr/ADR-027.md decision 6: what a check is checked against).
    pub fn candidate(&self, candidate_id: &str) -> Result<Option<CandidateRow>, PersistError> {
        candidates::candidate(&self.conn, candidate_id)
    }

    /// The outcome of the evidence `evidence_id` (a record id), if it is recorded (docs/adr/ADR-027.md decision 8).
    pub fn evidence_outcome(&self, evidence_id: &str) -> Result<Option<String>, PersistError> {
        candidates::evidence_outcome(&self.conn, evidence_id)
    }

    /// The run `run_id`'s specification and acceptance criteria (docs/adr/ADR-027.md decision 9).
    pub fn run_acceptance(&self, run_id: &str) -> Result<Option<RunAcceptance>, PersistError> {
        candidates::acceptance(&self.conn, run_id)
    }

    /// The snapshot of run `run_id`'s acceptance on its latest candidate (docs/adr/ADR-029.md decision 5), read in one
    /// transaction; none when there is no such run.
    pub fn acceptance(&self, run_id: &str) -> Result<Option<AcceptanceView>, PersistError> {
        snapshot::acceptance(&self.conn, run_id)
    }

    /// A page of run `run_id`'s acceptance projection, newest candidate first (openapi `listAcceptance`): at most
    /// `limit` items after the cursor `after` a previous page gave; none when there is no such run.
    pub fn acceptance_page(
        &self,
        run_id: &str,
        limit: usize,
        after: Option<(i64, String)>,
    ) -> Result<Option<AcceptancePage>, PersistError> {
        snapshot::acceptance_page(&self.conn, run_id, limit, after)
    }

    /// A page of run `run_id`'s Issues, newest first, each state evaluated now (openapi `listIssues`): at most `limit`
    /// older than the cursor `before`; none when there is no such run.
    pub fn issues_page(
        &self,
        run_id: &str,
        limit: usize,
        before: Option<i64>,
    ) -> Result<Option<IssuesPage>, PersistError> {
        snapshot::issues_page(&self.conn, run_id, limit, before)
    }

    /// Issue `issue_id` traced to its report and raw session (docs/adr/ADR-029.md decision 15); none when there is no
    /// such Issue or it is no finding's.
    pub fn issue_chain(
        &self,
        issue_id: &str,
    ) -> Result<Option<crate::verification::report::IssueChain>, PersistError> {
        snapshot::issue_chain(&self.conn, issue_id)
    }

    /// The attempt `attempt_id`'s run and task role: who may produce evidence (docs/adr/ADR-027.md principle two).
    pub fn producer(
        &self,
        attempt_id: &str,
    ) -> Result<Option<crate::verification::Producer>, PersistError> {
        candidates::producer(&self.conn, attempt_id)
    }

    /// A verifier's context from its package and its round's sources (docs/adr/ADR-028.md decision 14).
    pub fn verification_context(
        &self,
        package: &crate::protocol::ContextHandoff,
        sources: &[String],
    ) -> Result<crate::context::Facts, PersistError> {
        context::verification(&self.conn, package, sources)
    }

    /// Task `task_id`'s rework context (docs/adr/ADR-028.md decisions 12 and 14): the round whose failures a rework of
    /// it took, its rejected candidate, those failures and the candidate's open Issues; none if none was taken.
    pub fn rework_context(&self, task_id: &str) -> Result<Option<ReworkContext>, PersistError> {
        context::rework(&self.conn, task_id)
    }

    /// The candidates whose last recovery failed and none succeeded since, not judged unrecoverable: held until one
    /// does, so each tick tries them again whether or not they still have pending records (docs/adr/ADR-028.md
    /// decisions 11 and 16).
    pub fn unrecovered_candidates(
        &self,
    ) -> Result<std::collections::BTreeSet<String>, PersistError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT json_extract(f.payload_json, '$.candidate_id') FROM events f \
                 WHERE f.kind = 'candidate.reconcile_failed' \
                 AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
                   AND json_extract(r.payload_json, '$.candidate_id') = json_extract(f.payload_json, '$.candidate_id')), 0) \
                 AND NOT EXISTS(SELECT 1 FROM events s WHERE s.run_id = f.run_id AND s.kind = 'candidate.stale' \
                   AND json_extract(s.payload_json, '$.reason') = 'evidence_unrecoverable' \
                   AND json_extract(s.payload_json, '$.candidate_id') = json_extract(f.payload_json, '$.candidate_id'))",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map([], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)
    }

    /// Whether candidate `candidate_id`'s records are recovered at the host's start (docs/adr/ADR-028.md decision 16):
    /// it is frozen here and was not judged unrecoverable (its pending facts are kept for audit only, -1252).
    pub fn start_recovery_wanted(&self, candidate_id: &str) -> Result<bool, PersistError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM candidates WHERE candidate_id = ?1) \
                 AND NOT EXISTS(SELECT 1 FROM events WHERE kind = 'candidate.stale' \
                   AND run_id = (SELECT run_id FROM candidates WHERE candidate_id = ?1) \
                   AND json_extract(payload_json, '$.reason') = 'evidence_unrecoverable' \
                   AND json_extract(payload_json, '$.candidate_id') = ?1)",
                rusqlite::params![candidate_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)
    }

    /// Run `run_id`'s wait-for view (docs/adr/ADR-028.md decision 13): one edge, sender role to recipient role, per
    /// unfinished request between roles (a query or steer PENDING or DELIVERING in the outbox). A refused or deferred
    /// request is never queued, so it makes no edge.
    pub fn wait_for(&self, run_id: &str) -> Result<Vec<(String, String)>, PersistError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT json_extract(payload_json, '$.sender_role'), recipient_role FROM outbox \
                 WHERE run_id = ?1 AND state IN ('PENDING', 'DELIVERING') \
                 AND json_extract(payload_json, '$.kind') IN ('query', 'steer') \
                 AND json_extract(payload_json, '$.sender_role') IS NOT NULL ORDER BY 1, 2",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(rusqlite::params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)
    }

    /// When candidate `candidate_id`'s recovery may be tried again (docs/adr/ADR-028.md decision 11, M2T16D-1251):
    /// after its last failure, none succeeding since, by the failure's backoff (2 s doubling per failure in a row, at
    /// most 300 s); none when no failure stands.
    pub fn recovery_not_before(
        &self,
        candidate_id: &str,
    ) -> Result<Option<std::time::SystemTime>, PersistError> {
        let last: Option<(Option<i64>, Option<String>)> = self
            .conn
            .query_row(
                "SELECT json_extract(f.payload_json, '$.attempt'), json_extract(f.payload_json, '$.at_ms') FROM events f \
                 WHERE f.kind = 'candidate.reconcile_failed' AND json_extract(f.payload_json, '$.candidate_id') = ?1 \
                 AND f.seq > coalesce((SELECT max(r.seq) FROM events r WHERE r.kind = 'candidate.reconcile_recovered' \
                   AND json_extract(r.payload_json, '$.candidate_id') = ?1), 0) ORDER BY f.seq DESC LIMIT 1",
                rusqlite::params![candidate_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(sqlite_error(e)),
            })?;
        let Some((attempt, at_ms)) = last else {
            return Ok(None);
        };
        let at = at_ms.and_then(|ms| ms.parse::<u64>().ok()).unwrap_or(0);
        let doublings = u32::try_from(attempt.unwrap_or(1).saturating_sub(1))
            .unwrap_or(u32::MAX)
            .min(16);
        let backoff = (2u64 << doublings).min(300);
        Ok(Some(
            std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(at)
                + std::time::Duration::from_secs(backoff),
        ))
    }

    /// Verifier task `task_id`'s round in run `run_id` and the candidate it verifies (docs/adr/ADR-028.md decision 9);
    /// none for any other task.
    pub fn verifier_round(
        &self,
        run_id: &str,
        task_id: &str,
    ) -> Result<Option<(String, String)>, PersistError> {
        self.conn
            .query_row(
                "SELECT json_extract(payload_json, '$.round_id'), json_extract(payload_json, '$.candidate_id') \
                 FROM events WHERE kind = 'verification.round_opened' AND run_id = ?1 \
                 AND ?2 IN (json_extract(payload_json, '$.tasks.qa'), json_extract(payload_json, '$.tasks.reviewer'))",
                rusqlite::params![run_id, task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(sqlite_error(e)),
            })
    }

    /// The cancelled runs of project `project_id` with a verification round not closed: their cancel ended before the
    /// rounds' verifiers were settled, and the verification reconcile still closes them (docs/adr/ADR-028.md decision
    /// 11, M2T16R-53).
    pub fn cancelled_with_open_rounds(
        &self,
        project_id: &str,
    ) -> Result<Vec<String>, PersistError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT DISTINCT r.run_id FROM runs r JOIN events o ON o.run_id = r.run_id \
                 AND o.kind = 'verification.round_opened' WHERE r.project_id = ?1 AND r.state = 'CANCELLED' \
                 AND NOT EXISTS(SELECT 1 FROM events c WHERE c.run_id = r.run_id AND c.kind = 'verification.round_closed' \
                   AND json_extract(c.payload_json, '$.round_id') = json_extract(o.payload_json, '$.round_id')) \
                 ORDER BY r.run_id",
            )
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map(rusqlite::params![project_id], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)
    }

    /// The attempts still live (PREPARED, STARTING or RUNNING): whose run directories and requests are kept.
    pub fn live_attempts(&self) -> Result<Vec<String>, PersistError> {
        let mut statement = self
            .conn
            .prepare("SELECT attempt_id FROM attempts WHERE state IN ('PREPARED', 'STARTING', 'RUNNING')")
            .map_err(sqlite_error)?;
        let rows = statement
            .query_map([], |r| r.get(0))
            .map_err(sqlite_error)?;
        rows.collect::<Result<_, _>>().map_err(sqlite_error)
    }

    /// Whether candidate `candidate_id`'s pending check records are to be recovered now (docs/adr/ADR-028.md decision
    /// 11): it has a verification round not closed, it is VERIFYING, REJECTED or INVALID, and no persistent fact says it
    /// was superseded (a later freeze listed it, or `candidate.stale`, an unrecoverable one's included).
    pub fn recovery_due(&self, candidate_id: &str) -> Result<bool, PersistError> {
        self.conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM candidates c WHERE c.candidate_id = ?1 \
                   AND c.state IN ('VERIFYING', 'REJECTED', 'INVALID')) \
                 AND EXISTS(SELECT 1 FROM events o WHERE o.kind = 'verification.round_opened' \
                   AND json_extract(o.payload_json, '$.candidate_id') = ?1 AND NOT EXISTS(SELECT 1 FROM events x \
                     WHERE x.kind = 'verification.round_closed' \
                     AND json_extract(x.payload_json, '$.round_id') = json_extract(o.payload_json, '$.round_id'))) \
                 AND NOT EXISTS(SELECT 1 FROM events s WHERE s.kind = 'candidate.stale' \
                   AND s.run_id = (SELECT run_id FROM candidates WHERE candidate_id = ?1) \
                   AND json_extract(s.payload_json, '$.candidate_id') = ?1) \
                 AND NOT EXISTS(SELECT 1 FROM events f, json_each(f.payload_json, '$.superseded') v \
                   WHERE f.kind = 'candidate.frozen' AND f.run_id = (SELECT run_id FROM candidates \
                   WHERE candidate_id = ?1) AND v.value = ?1)",
                rusqlite::params![candidate_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)
    }

    /// Task `task_id`'s project and run, if it exists.
    pub fn task_run(&self, task_id: &str) -> Result<Option<(String, String)>, PersistError> {
        self.conn
            .query_row(
                "SELECT r.project_id, t.run_id FROM tasks t JOIN runs r ON r.run_id = t.run_id WHERE t.task_id = ?1",
                rusqlite::params![task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                e => Err(e),
            })
            .map_err(sqlite_error)
    }

    /// The attempt `attempt_id`, if it exists.
    pub fn attempt(&self, attempt_id: &str) -> Result<Option<AttemptRow>, PersistError> {
        attempts::read_attempt(&self.conn, attempt_id)
    }

    /// The attempts of run `run_id` that start or run, oldest first (what a cancel must stop, ADR-021 decision 8).
    pub fn running_attempts(&self, run_id: &str) -> Result<Vec<AttemptRow>, PersistError> {
        control::running_attempts(&self.conn, run_id)
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

    /// The project's leases as of `now`, by kind (REQ-037).
    pub fn leases(
        &self,
        project_id: &str,
        now: std::time::SystemTime,
    ) -> Result<Vec<LeaseView>, PersistError> {
        leases::held(&self.conn, project_id, now)
    }

    /// The run `run_id` with the highest event seq, read together.
    pub fn snapshot(&self, run_id: &str) -> Result<Option<(RunRow, i64)>, PersistError> {
        commands::snapshot(&self.conn, run_id)
    }

    /// The project's current team revision (0 before any team transaction, docs/adr/ADR-022.md decision 3).
    pub fn team_revision(&self, project_id: &str) -> Result<i64, PersistError> {
        team::revision(&self.conn, project_id)
    }

    /// Whether project `project_id` has the event `event_id`.
    pub fn event_exists(&self, project_id: &str, event_id: &str) -> Result<bool, PersistError> {
        self.conn
            .query_row(
                "SELECT count(*) FROM events WHERE project_id = ?1 AND event_id = ?2",
                rusqlite::params![project_id, event_id],
                |r| r.get::<_, i64>(0),
            )
            .map(|n| n > 0)
            .map_err(sqlite_error)
    }

    /// The source of the project's latest team revision made from its configuration file or the built-in defaults
    /// (docs/adr/ADR-022.md decision 6), none before any.
    pub fn team_file_source(
        &self,
        project_id: &str,
    ) -> Result<Option<serde_json::Value>, PersistError> {
        team::file_source(&self.conn, project_id)
    }

    /// The run `run_id`, its team (docs/adr/ADR-022.md decision 18) and the highest event seq, read together.
    pub fn run_view(&self, run_id: &str) -> Result<Option<(RunRow, TeamView, i64)>, PersistError> {
        commands::run_view(&self.conn, run_id)
    }

    /// [`Reader::run_view`] with the run's acceptance snapshot (getRun's projection, docs/adr/ADR-029.md decision 5),
    /// read together.
    pub fn run_projection(
        &self,
        run_id: &str,
    ) -> Result<Option<(RunRow, TeamView, AcceptanceView, i64)>, PersistError> {
        commands::run_projection(&self.conn, run_id)
    }

    /// A page of the project's runs, newest first (openapi `listRuns`; ADR-021 decision 6): the runs with their
    /// creation time, the next page's cursor if there may be one, and the watermark.
    pub fn list_runs(
        &self,
        state: Option<&str>,
        limit: usize,
        before: Option<i64>,
    ) -> Result<RunsPage, PersistError> {
        commands::list_runs(&self.conn, state, limit, before)
    }

    /// The payload of the latest event of `kind` about attempt `attempt_id`, if any (an attempt's tracking, ADR-021
    /// decision 8d).
    pub fn attempt_event(
        &self,
        kind: &str,
        attempt_id: &str,
    ) -> Result<Option<serde_json::Value>, PersistError> {
        commands::attempt_event(&self.conn, kind, attempt_id)
    }

    /// At most `limit` events after seq `after`, in order.
    pub fn events_after(&self, after: i64, limit: usize) -> Result<Vec<EventRow>, PersistError> {
        commands::events_after(&self.conn, after, limit)
    }

    /// The highest event seq.
    pub fn max_seq(&self) -> Result<i64, PersistError> {
        commands::max_seq(&self.conn)
    }

    /// Whether this connection refuses writes, as a reader must (REQ-062).
    pub fn refuses_writes(&self) -> bool {
        commands::refuses_writes(&self.conn)
    }
}

#[cfg(test)]
mod tests;
