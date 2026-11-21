//! A candidate's recovery facts (docs/adr/ADR-028.md decisions 11 and 16; docs/adr/ADR-031.md's record, review rounds 1
//! and 2, M2T13R-03, -101, -102): their kinds, their ids and every read of them — by their ids, never by a scan of
//! the project's events.
//!
//! `closing::record_recovery` takes their ids from here; specification changes use [`spec_changed`]. Each fact is
//! named by its candidate: the failures
//! [`failure`] `child_id(c, "reconcile_failed/<n>")` and the recoveries [`recovery`]
//! `child_id(c, "reconcile_recovered/<m>")`, each numbered from 1 with no gap (the next is the count of those before
//! it, plus one, so a later one has the higher seq), and the judgment [`judgment`] `child_id(c, "stale")`. A fact
//! written as a Record of a stopped run keeps its id (its `run_id` null, its run in the payload), so one look-up by id
//! finds it whichever way it was written, and no other run's facts are read. The scan test
//! `the_recovery_facts_are_named_only_by_their_module` (hold/tests.rs) keeps their kinds out of every other product
//! file, so no reader or writer goes round these ids.

use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use rusqlite::{Connection, OptionalExtension, Statement, StatementStatus, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The kinds of a candidate's recovery facts — a failure, a recovery after failures, the judgment — for
/// `hold::RECORD_KINDS` (the kinds written as Records), and only for it.
pub(super) const KINDS: [&str; 3] = [
    "candidate.reconcile_failed",
    "candidate.reconcile_recovered",
    "candidate.stale",
];

/// A recovery failure's `attempt` and `at_ms`, as its event has them.
pub(crate) type Failed = (Option<i64>, Option<String>);

/// The SQLite VM steps of the statements a read ran: a test's measure of its work.
#[derive(Debug, Default)]
pub(crate) struct Work(pub(crate) i64);

impl Work {
    /// Add `statement`'s steps since its last count.
    pub(super) fn count(&mut self, statement: &Statement<'_>) {
        self.0 += i64::from(statement.get_status(StatementStatus::VmStep));
        statement.reset_status(StatementStatus::VmStep);
    }
}

/// The numbered families of facts: failures and recoveries.
#[derive(Debug, Clone, Copy)]
enum Family {
    Failures,
    Recoveries,
}

impl Family {
    fn kind(self) -> &'static str {
        match self {
            Family::Failures => KINDS[0],
            Family::Recoveries => KINDS[1],
        }
    }

    fn id(self, candidate: &str, n: u64) -> String {
        let name = match self {
            Family::Failures => "reconcile_failed",
            Family::Recoveries => "reconcile_recovered",
        };
        child_id(candidate, &format!("{name}/{n}"))
    }
}

/// The id and kind of candidate `candidate`'s `n`-th recovery failure (from 1).
pub(super) fn failure(candidate: &str, n: u64) -> (String, &'static str) {
    (Family::Failures.id(candidate, n), Family::Failures.kind())
}

/// The id and kind of candidate `candidate`'s `m`-th recovery after failures (from 1).
pub(super) fn recovery(candidate: &str, m: u64) -> (String, &'static str) {
    (
        Family::Recoveries.id(candidate, m),
        Family::Recoveries.kind(),
    )
}

/// The id and kind of candidate `candidate`'s one judgment: unrecoverable evidence or a changed specification.
pub(super) fn judgment(candidate: &str) -> (String, &'static str) {
    (child_id(candidate, "stale"), KINDS[2])
}

/// Invalidate every usable candidate of a run moving to a changed specification (ADR-033 Q4).
/// The approval owns this transaction. An existing judgment is immutable; it does not prevent the state update.
pub(super) fn spec_changed(
    tx: &Transaction<'_>,
    run: &str,
    revision: i64,
    template: &super::writer::NewEvent,
) -> Result<Vec<super::writer::NewEvent>, PersistError> {
    let candidates = tx
        .prepare("SELECT candidate_id FROM candidates WHERE run_id=?1 AND state IN ('FROZEN','VERIFYING','ACCEPTED') ORDER BY candidate_id")
        .map_err(sqlite_error)?
        .query_map(params![run], |r| r.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    let mut events = Vec::new();
    for candidate in candidates {
        tx.execute("UPDATE candidates SET state='STALE' WHERE candidate_id=?1 AND state IN ('FROZEN','VERIFYING','ACCEPTED')", params![candidate])
            .map_err(sqlite_error)?;
        let (id, kind) = judgment(&candidate);
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE event_id=?1)",
                params![id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if !exists {
            let mut event = template.clone();
            event.event_id = id;
            event.run_id = Some(run.to_owned());
            event.kind = kind.to_owned();
            event.payload =
                json!({"candidate_id":candidate,"reason":"spec_changed","spec_revision":revision});
            events.push(event);
        }
    }
    Ok(events)
}

/// One recovery fact: its seq, the run it is of (its own, or the one its Record names) and its payload.
#[derive(Debug, Clone)]
struct Fact {
    seq: i64,
    run: Option<String>,
    payload: Value,
}

/// Candidate `candidate`'s fact of id `id` and kind `kind`, if there is one: one look-up by the unique id.
fn fact(
    conn: &Connection,
    candidate: &str,
    (id, kind): (String, &str),
    work: &mut Work,
) -> Result<Option<Fact>, PersistError> {
    let mut statement = conn
        .prepare_cached(
            "SELECT seq, coalesce(run_id, json_extract(payload_json, '$.run_id')), payload_json FROM main.events \
             WHERE event_id = ?1 AND kind = ?2 AND json_extract(payload_json, '$.candidate_id') = ?3",
        )
        .map_err(sqlite_error)?;
    let found: Option<(i64, Option<String>, String)> = statement
        .query_row(params![id, kind, candidate], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()
        .map_err(sqlite_error)?;
    work.count(&statement);
    Ok(found.map(|(seq, run, payload)| Fact {
        seq,
        run,
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
    }))
}

/// Candidate `candidate`'s `n`-th fact of `family`.
fn at(
    conn: &Connection,
    candidate: &str,
    family: Family,
    n: u64,
    work: &mut Work,
) -> Result<Option<Fact>, PersistError> {
    fact(
        conn,
        candidate,
        (family.id(candidate, n), family.kind()),
        work,
    )
}

/// How many facts of `family` candidate `candidate` has: the last n there is (numbered from 1 with no gap), found by
/// doubling and halving — O(log n) look-ups.
fn count(
    conn: &Connection,
    candidate: &str,
    family: Family,
    work: &mut Work,
) -> Result<u64, PersistError> {
    if at(conn, candidate, family, 1, work)?.is_none() {
        return Ok(0);
    }
    // (low there, high not.)
    let (mut low, mut high) = (1_u64, 2_u64);
    while at(conn, candidate, family, high, work)?.is_some() {
        low = high;
        high = high.saturating_mul(2);
    }
    while high - low > 1 {
        let mid = low + (high - low) / 2;
        if at(conn, candidate, family, mid, work)?.is_some() {
            low = mid;
        } else {
            high = mid;
        }
    }
    Ok(low)
}

/// The last of candidate `candidate`'s facts of `family` that `wanted` takes: down from the last one.
fn last(
    conn: &Connection,
    candidate: &str,
    family: Family,
    wanted: &dyn Fn(&Fact) -> bool,
    work: &mut Work,
) -> Result<Option<Fact>, PersistError> {
    let mut n = count(conn, candidate, family, work)?;
    while n >= 1 {
        if let Some(found) = at(conn, candidate, family, n, work)?
            && wanted(&found)
        {
            return Ok(Some(found));
        }
        n -= 1;
    }
    Ok(None)
}

/// Candidate `candidate`'s recovery facts that `wanted` takes: its last failure, its last recovery and its judgment.
struct Facts {
    failure: Option<Fact>,
    recovery: Option<Fact>,
    judgment: Option<Fact>,
}

impl Facts {
    fn read(
        conn: &Connection,
        candidate: &str,
        wanted: &dyn Fn(&Fact) -> bool,
        work: &mut Work,
    ) -> Result<Facts, PersistError> {
        Ok(Facts {
            failure: last(conn, candidate, Family::Failures, wanted, work)?,
            recovery: last(conn, candidate, Family::Recoveries, wanted, work)?,
            judgment: fact(conn, candidate, judgment(candidate), work)?,
        })
    }

    /// Its last failure, when no recovery came after it.
    fn failing(&self) -> Option<&Fact> {
        self.failure
            .as_ref()
            .filter(|f| f.seq > self.recovery.as_ref().map_or(0, |r| r.seq))
    }

    /// Judged on run `run` (`hold::of_run`'s rule: of the run, or a Record naming it), for `reason` when one is given.
    fn judged(&self, run: &str, reason: Option<&str>) -> bool {
        self.judgment.as_ref().is_some_and(|j| {
            j.run.as_deref() == Some(run)
                && reason.is_none_or(|reason| j.payload["reason"] == reason)
        })
    }
}

/// The candidates whose last recovery failed and none succeeded since, not judged unrecoverable on their run: of every
/// run (`run` none; any run's facts count, as the rule always read them) or of run `run` (its facts only). It
/// enumerates the candidates — the `candidates` rows of the run, or of all runs — and reads each by a bounded number
/// of look-ups by id (O(log) in its own facts): its work is bounded by the candidates, not by the project's events.
pub(crate) fn unrecovered(
    conn: &Connection,
    run: Option<&str>,
    work: &mut Work,
) -> Result<BTreeSet<String>, PersistError> {
    // A run's through its index (`candidates_run`), every run's in one pass.
    let mut statement = conn
        .prepare_cached(match run {
            Some(_) => "SELECT candidate_id, run_id FROM main.candidates WHERE run_id = ?1",
            None => "SELECT candidate_id, run_id FROM main.candidates WHERE ?1 IS NULL",
        })
        .map_err(sqlite_error)?;
    let candidates: Vec<(String, String)> = statement
        .query_map(params![run], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    work.count(&statement);
    let wanted = |f: &Fact| run.is_none_or(|run| f.run.as_deref() == Some(run));
    let mut found = BTreeSet::new();
    for (candidate, of) in candidates {
        let facts = Facts::read(conn, &candidate, &wanted, work)?;
        if facts.failing().is_some() && !facts.judged(&of, Some("evidence_unrecoverable")) {
            found.insert(candidate);
        }
    }
    Ok(found)
}

/// Candidate `candidate`'s last recovery failure since its last recovery, if any (any run's facts, as the rule always
/// read them): its `attempt` and `at_ms`, `Reader::recovery_not_before`'s backoff.
pub(crate) fn last_failure(
    conn: &Connection,
    candidate: &str,
    work: &mut Work,
) -> Result<Option<Failed>, PersistError> {
    let facts = Facts::read(conn, candidate, &|_| true, work)?;
    Ok(facts.failing().map(|f| {
        (
            f.payload["attempt"].as_i64(),
            f.payload["at_ms"].as_str().map(str::to_owned),
        )
    }))
}

/// Whether candidate `candidate` is failing: its last recovery failed and none succeeded since (any run's facts, as
/// `closing::held_by_recovery`'s rule always read them).
pub(crate) fn failing(
    conn: &Connection,
    candidate: &str,
    work: &mut Work,
) -> Result<bool, PersistError> {
    Ok(Facts::read(conn, candidate, &|_| true, work)?
        .failing()
        .is_some())
}

/// Whether candidate `candidate` was judged on run `run` (`hold::of_run`'s rule: the judgment of the run, or a Record
/// naming it), for `reason` when one is given (any reason when none).
pub(crate) fn judged(
    conn: &Connection,
    candidate: &str,
    run: &str,
    reason: Option<&str>,
    work: &mut Work,
) -> Result<bool, PersistError> {
    let judgment = fact(conn, candidate, judgment(candidate), work)?;
    Ok(Facts {
        failure: None,
        recovery: None,
        judgment,
    }
    .judged(run, reason))
}

/// Candidate `candidate`'s failures and recoveries so far, as its writer counts them (any run's facts).
pub(super) struct Streak {
    /// How many failures it has (the next is numbered one more).
    pub(super) failures: u64,
    /// How many recoveries it has (the next is numbered one more).
    pub(super) recoveries: u64,
    /// The payloads of its failures since its last recovery, oldest first: the run of failures a judgment weighs.
    pub(super) since: Vec<Value>,
}

/// Candidate `candidate`'s [`Streak`]: its counts by doubling and halving, then its failures down from the last one
/// while they come after its last recovery — look-ups bounded by its own facts.
pub(super) fn streak(
    conn: &Connection,
    candidate: &str,
    work: &mut Work,
) -> Result<Streak, PersistError> {
    let failures = count(conn, candidate, Family::Failures, work)?;
    let recoveries = count(conn, candidate, Family::Recoveries, work)?;
    let after = match recoveries {
        0 => 0,
        m => at(conn, candidate, Family::Recoveries, m, work)?.map_or(0, |r| r.seq),
    };
    let mut since = Vec::new();
    let mut n = failures;
    while n >= 1 {
        match at(conn, candidate, Family::Failures, n, work)? {
            Some(f) if f.seq > after => since.push(f.payload),
            _ => break,
        }
        n -= 1;
    }
    since.reverse();
    Ok(Streak {
        failures,
        recoveries,
        since,
    })
}
