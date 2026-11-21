//! Stopped runs (docs/adr/ADR-031.md design correction D, E17): once a run has stopped in G = T ∪ {INCOMPLETE} and
//! published that stop, no progress of it is written any more. The rule lives in the only events insert
//! ([`super::writer::insert_event`]) and in the queries that pick work, never in a list of writers:
//!
//! - a run R is **stopped** ([`stopped`]) when runs.state(R) is in G and R's latest event is the `run.result` whose
//!   payload state is runs.state(R): the stop is published. Keyed so that the commit entering G (its compare-and-set
//!   comes before its events) and the commit leaving it (ResumeRun, CancelRun) are not held by an older result; read
//!   by R's current revision rather than its history (M2T13R-02);
//! - on a stopped run an event of a command accepted for it (`temp.command_run`) is written as it is, a Record (a
//!   kind of [`RECORD_KINDS`]: audit, history, delivery bookkeeping, diagnostics) is written project-scoped, and
//!   anything else (Progress) is refused with `RunStopped`, which the writer actor's answer turns into
//!   [`PersistError::Held`];
//! - the queries that pick work leave stopped runs out ([`not_held`]), so a run is Held only in the tick of its stop;
//! - project-scoped records are read by run through [`of_run`].

use super::{PersistError, sqlite_error};
use crate::protocol::RunState;
use rusqlite::{Connection, OptionalExtension, params};

/// G, as SQL: the stopping states entered only when quiet (docs/adr/ADR-031.md correction D, its scope).
pub(super) const G: &str = "'COMPLETED', 'FAILED', 'CANCELLED', 'INCOMPLETE'";

/// Whether run `run` is in G now (stopped or not).
pub(super) fn in_g(conn: &Connection, run: &str) -> Result<bool, PersistError> {
    conn.query_row(
        &format!("SELECT EXISTS(SELECT 1 FROM main.runs WHERE run_id = ?1 AND state IN ({G}))"),
        params![run],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The kinds an event may be written as a Record (docs/adr/ADR-031.md correction D, rule 1): the history of evidence
/// and integrity, the outbox's bookkeeping and the diagnostics. A Record of a stopped run is written project-scoped.
pub(super) const RECORD_KINDS: [&str; 18] = [
    "input.rejected",
    // (i) evidence and integrity history: a check's evidence, its Issues, a candidate's recovery.
    "evidence.recorded",
    "integrity_violation",
    "issue.resolved",
    "issue.reopened",
    super::recovery::KINDS[0],
    super::recovery::KINDS[1],
    super::recovery::KINDS[2],
    // (ii) delivery bookkeeping.
    "outbox.dead_letter",
    "outbox.rejected",
    "outbox.delivered_late",
    "message.invalid",
    // (iii) diagnostics.
    "reconcile.diagnostic",
    "runner.diagnostic",
    "runner.outside",
    "runner.unreaped",
    "cancel.diagnostic",
    "supervisor.step_failed",
];

/// SQL: whether the run named by the SQL expression `run` is stopped — in G, its latest event the `run.result` of that
/// state — read without the run's history (M2T13R-02): its latest run.result (the `(run_id, kind, seq)` index's last
/// entry) is the one of its current revision (its id, `child_id(run, "result/<revision>")`, ends with that suffix
/// however its run part was shortened) and of its current state. The same, because every transaction that leaves a
/// run in G with its revision moved ends with its run.result at that revision (`writer::write` registers a Touch of a
/// run in G as it does a move into E), and every event of a stopped run is a command's, whose run is moved (a
/// command's run in G must end with its result, `writer::finish`). Its aliases are its own, so it can be put into any
/// query, once or more.
fn stopped_sql(run: &str) -> String {
    format!(
        "EXISTS(SELECT 1 FROM main.runs hold_r WHERE hold_r.run_id = {run} AND hold_r.state IN ({G}) \
         AND EXISTS(SELECT 1 FROM main.events hold_e WHERE hold_e.seq = (SELECT max(hold_m.seq) FROM main.events \
             hold_m WHERE hold_m.run_id = hold_r.run_id AND hold_m.kind = 'run.result') \
           AND substr(hold_e.event_id, -length('/result/' || hold_r.revision)) = '/result/' || hold_r.revision \
           AND json_extract(hold_e.payload_json, '$.state') = hold_r.state))"
    )
}

/// SQL: the run named by the SQL expression `run_col` is not stopped (the ADR's `NOT_HELD`), ANDed into every query
/// that picks a run's work, so no stopped run is picked again after the tick of its stop.
pub(super) fn not_held(run_col: &str) -> String {
    format!("NOT {}", stopped_sql(run_col))
}

/// SQL: event row `alias` is run `run`'s (an SQL expression): its own, or a Record of it written project-scoped after
/// it stopped (`run_id` null, the run in the payload). Every query of a [`RECORD_KINDS`] kind by run goes through it.
pub(super) fn of_run(alias: &str, run: &str) -> String {
    format!(
        "({alias}.run_id = {run} OR ({alias}.run_id IS NULL AND json_extract({alias}.payload_json, '$.run_id') = {run}))"
    )
}

/// The state run `run` stopped in, when it is stopped (none for a run not stopped, or not there).
pub(super) fn stopped(conn: &Connection, run: &str) -> Result<Option<RunState>, PersistError> {
    let state: Option<String> = conn
        .query_row(
            &format!(
                "SELECT state FROM main.runs WHERE run_id = ?1 AND {}",
                stopped_sql("?1")
            ),
            params![run],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    state
        .as_deref()
        .map(super::writer::parse::<RunState>)
        .transpose()
}

/// Run `run`'s progress may be written now: it is not [`stopped`], else `RunStopped` (answered Held). The cheap look at
/// the start of a run's job, which then does none of its work; never what correctness rests on: a job that does not
/// look is refused by the insert all the same.
pub(super) fn writable(conn: &Connection, run: &str) -> Result<(), PersistError> {
    match stopped(conn, run)? {
        None => Ok(()),
        Some(state) => Err(PersistError::RunStopped {
            run: run.to_owned(),
            state,
        }),
    }
}

/// Register run `run` as the run of the command this transaction accepts (`temp.command_run`): its events are the
/// command's, written as they are even while it is stopped. Rolled back with the transaction, emptied by its commit.
pub(super) fn command(conn: &Connection, run: &str) -> Result<(), PersistError> {
    conn.execute(
        "INSERT OR IGNORE INTO temp.command_run(run_id) VALUES (?1)",
        params![run],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// Whether run `run` is the run of a command this transaction accepts.
pub(super) fn commanded(conn: &Connection, run: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM temp.command_run WHERE run_id = ?1)",
        params![run],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The message of the backstop trigger's refusal.
pub(super) const RUN_STOPPED: &str = "RUN_STOPPED";

/// Install, on writer connection `conn`, the backstops of the insert function and of the compare-and-set's gate
/// (correction D; M2T13R-100): TEMP triggers that refuse an event of a stopped run that is not its `run.result` nor a
/// command's, and a move of a stopped run's revision or state that is not its command's (judged on the run before the
/// update). The writer decides first (the insert function writes a Record project-scoped; `compare_and_set` refuses
/// the move), so on the product's paths they never fire; they stop raw SQL that bypasses them.
pub(crate) fn install(conn: &Connection) -> Result<(), PersistError> {
    super::writer::temp_tables(conn)?;
    conn.execute_batch(&format!(
        "CREATE TEMP TRIGGER IF NOT EXISTS events_of_a_stopped_run BEFORE INSERT ON main.events \
         WHEN NEW.run_id IS NOT NULL AND NEW.kind <> 'run.result' \
           AND NOT EXISTS(SELECT 1 FROM temp.command_run c WHERE c.run_id = NEW.run_id) \
           AND {} \
         BEGIN SELECT RAISE(ABORT, '{RUN_STOPPED}'); END; \
         CREATE TEMP TRIGGER IF NOT EXISTS moves_of_a_stopped_run BEFORE UPDATE OF revision, state ON main.runs \
         WHEN NOT EXISTS(SELECT 1 FROM temp.command_run c WHERE c.run_id = OLD.run_id) \
           AND {} \
         BEGIN SELECT RAISE(ABORT, '{RUN_STOPPED}'); END;",
        stopped_sql("NEW.run_id"),
        stopped_sql("OLD.run_id")
    ))
    .map_err(sqlite_error)
}

/// A store call's failure that a failure's recorder may count (docs/adr/ADR-031.md correction D, rule 2 (c)): never a
/// stopped run's [`PersistError::Held`] or [`PersistError::RunStopped`], which are no failure (nothing is counted, the
/// obligation stays).
#[derive(Debug)]
pub struct Failure(PersistError);

impl TryFrom<PersistError> for Failure {
    type Error = PersistError;

    fn try_from(error: PersistError) -> Result<Self, Self::Error> {
        match error {
            PersistError::RunStopped { .. } | PersistError::Held { .. } => Err(error),
            error => Ok(Failure(error)),
        }
    }
}

impl Failure {
    /// The store's error it is.
    pub fn error(&self) -> &PersistError {
        &self.0
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[cfg(test)]
mod tests;
