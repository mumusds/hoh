//! WAL self-test (REQ-062, CR-002): on a file system not known to be network or shared, the host proves at start that
//! SQLite's WAL mode works there before it creates the real database. A temporary database in the target directory
//! is switched to WAL, written, read through a second connection (which needs the `-shm` wal-index to be mapped and
//! shared), checked for cross-connection write locking, and checkpointed; any failure is `DB_WAL_UNSUPPORTED`. The
//! temporary files are removed in every case.

use super::PersistError;
use rusqlite::{Connection, ErrorCode, OpenFlags, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
            Step::SharedMemory => "shared-memory read",
            Step::Locking => "write locking",
            Step::Checkpoint => "checkpoint",
        }
    }
}

/// Run the self-test in `dir` (which must exist).
pub fn wal_self_test(dir: &Path) -> Result<(), PersistError> {
    run(dir, &|_| Ok(()))
}

/// The self-test with `inject` called before each step; an `Err` from it fails that step (fault injection).
pub(crate) fn run(
    dir: &Path,
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
    let result = steps(&db, inject);
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

fn steps(db: &Path, inject: &dyn Fn(Step) -> Result<(), String>) -> Result<(), PersistError> {
    let failed = |step: Step, detail: String| PersistError::WalUnsupported {
        step: step.name(),
        detail,
    };
    let at = |step: Step| inject(step).map_err(|d| failed(step, d));
    let sql = |step: Step| move |e: rusqlite::Error| failed(step, e.to_string());

    at(Step::Open)?;
    let first = Connection::open(db).map_err(sql(Step::Open))?;
    let second_flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;

    at(Step::EnableWal)?;
    let mode: String = first
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .map_err(sql(Step::EnableWal))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(failed(Step::EnableWal, format!("journal_mode is {mode}")));
    }

    at(Step::Write)?;
    first
        .execute_batch("CREATE TABLE t(x INTEGER); INSERT INTO t VALUES (42);")
        .map_err(sql(Step::Write))?;

    // The row is only in the WAL: a second connection sees it only through the shared wal-index (-shm).
    at(Step::SharedMemory)?;
    let second = Connection::open_with_flags(db, second_flags).map_err(sql(Step::SharedMemory))?;
    let seen: i64 = second
        .query_row("SELECT x FROM t", [], |r| r.get(0))
        .map_err(sql(Step::SharedMemory))?;
    if seen != 42 || !with_suffix(db, "-shm").exists() {
        return Err(failed(
            Step::SharedMemory,
            format!(
                "second connection read {seen}; -shm present: {}",
                with_suffix(db, "-shm").exists()
            ),
        ));
    }

    // While one connection holds the write lock, another must be refused it.
    at(Step::Locking)?;
    second
        .busy_timeout(Duration::ZERO)
        .map_err(sql(Step::Locking))?;
    let mut first = first;
    let held = first
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sql(Step::Locking))?;
    let contended = second.execute_batch("BEGIN IMMEDIATE; ROLLBACK;");
    drop(held);
    match contended {
        Err(e) if e.sqlite_error_code() == Some(ErrorCode::DatabaseBusy) => {}
        Err(e) => return Err(failed(Step::Locking, e.to_string())),
        Ok(()) => {
            return Err(failed(
                Step::Locking,
                "a second connection got the write lock while the first held it".to_owned(),
            ));
        }
    }
    drop(second);

    at(Step::Checkpoint)?;
    let (busy, log, done): (i64, i64, i64) = first
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

#[cfg(test)]
mod tests {
    use super::*;

    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok()?.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_self_test_passes_on_a_local_directory_and_leaves_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        wal_self_test(dir.path())?;
        assert_eq!(leftovers(dir.path()), Vec::<String>::new());
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
            match run(dir.path(), &fault) {
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
        match wal_self_test(&missing) {
            Err(PersistError::WalUnsupported { step, .. }) => assert_eq!(step, "open"),
            other => return Err(format!("{other:?}").into()),
        }
        Ok(())
    }
}
