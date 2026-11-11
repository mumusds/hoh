//! The Commander's wakes (docs/adr/ADR-025.md decision 3), read from a run's `supervisor.*` events: a fired trigger
//! owes the Commander one turn. A wake is its first `supervisor.woken` (its id is that trigger's id); a later trigger
//! of its key woken while it is owed and not in flight joins it (`merged_into`), adding its tasks and raising its
//! severity. A wake is owed until a turn of it SETTLES or it is superseded; a turn of it is in flight from its
//! `supervisor.allocated` until its attempt ends.

use super::{PersistError, sqlite_error};
use crate::lifecycle::{AttemptState, Lifecycle as _};
use crate::supervise::Severity;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;
use std::collections::BTreeSet;

pub(super) const WOKEN: &str = "supervisor.woken";
pub(super) const ALLOCATED: &str = "supervisor.allocated";
pub(super) const REARMED: &str = "supervisor.rearmed";
pub(super) const SUPERSEDED: &str = "supervisor.superseded";

/// One turn given to a wake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Turn {
    pub generation: i64,
    pub attempt_id: String,
    /// Its attempt's state now (`None`: no such attempt).
    pub state: Option<AttemptState>,
    /// Its `supervisor.rearmed` is written.
    pub rearmed: bool,
}

impl Turn {
    fn ended(&self) -> bool {
        self.state.is_none_or(AttemptState::is_terminal)
    }
}

/// A wake as its events tell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Wake {
    pub id: String,
    pub fingerprint: String,
    pub spec_id: Option<String>,
    pub severity: Severity,
    /// When it was first woken (milliseconds UTC) and that event's seq.
    pub woken_at: String,
    pub woken_seq: i64,
    /// The tasks its events name, over every trigger merged into it.
    pub task_ids: BTreeSet<String>,
    pub turns: Vec<Turn>,
    pub superseded: bool,
}

impl Wake {
    /// A turn of it SETTLED: it is answered.
    pub fn settled(&self) -> bool {
        self.turns
            .last()
            .is_some_and(|t| t.state == Some(AttemptState::Settled))
    }

    /// A turn of it has not ended.
    pub fn in_flight(&self) -> bool {
        self.turns.last().is_some_and(|t| !t.ended())
    }

    /// Owed a turn: not answered, not superseded, none in flight.
    pub fn owed(&self) -> bool {
        !self.superseded && !self.settled() && !self.in_flight()
    }
}

fn text(payload: &Value, field: &str) -> Option<String> {
    payload[field].as_str().map(str::to_owned)
}

/// Run `run_id`'s wakes, in the order they were first woken.
pub(super) fn of_run(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<Wake>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT seq, kind, payload_json FROM events WHERE run_id = ?1 AND kind IN (?2, ?3, ?4, ?5) ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, String, String)> = statement
        .query_map(
            params![run_id, WOKEN, ALLOCATED, REARMED, SUPERSEDED],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut wakes: Vec<Wake> = Vec::new();
    for (seq, kind, payload) in rows {
        let payload: Value = serde_json::from_str(&payload).map_err(|e| {
            PersistError::IntegrityViolation(format!("supervisor event {seq} of {run_id}: {e}"))
        })?;
        let named = text(&payload, "wake");
        let wake = |wakes: &mut Vec<Wake>| -> Option<usize> {
            named
                .as_deref()
                .and_then(|id| wakes.iter().position(|w| w.id == id))
        };
        match kind.as_str() {
            WOKEN => {
                let severity = payload["severity"]
                    .as_str()
                    .and_then(Severity::named)
                    .unwrap_or(Severity::Normal);
                let tasks = payload["task_ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(|v| v.as_str().map(str::to_owned))
                            .collect::<BTreeSet<_>>()
                    })
                    .unwrap_or_default();
                if let Some(i) = wake(&mut wakes).filter(|_| payload["merged_into"].is_string()) {
                    wakes[i].severity = wakes[i].severity.max(severity);
                    wakes[i].task_ids.extend(tasks);
                } else {
                    wakes.push(Wake {
                        id: named
                            .or_else(|| text(&payload, "trigger_id"))
                            .unwrap_or_default(),
                        fingerprint: text(&payload["dedup_key"], "fingerprint").unwrap_or_default(),
                        spec_id: text(&payload["dedup_key"], "spec_id"),
                        severity,
                        woken_at: text(&payload, "woken_at").unwrap_or_default(),
                        woken_seq: seq,
                        task_ids: tasks,
                        turns: Vec::new(),
                        superseded: false,
                    });
                }
            }
            ALLOCATED => {
                if let (Some(i), Some(attempt_id)) =
                    (wake(&mut wakes), text(&payload, "attempt_id"))
                {
                    wakes[i].turns.push(Turn {
                        generation: payload["generation"].as_i64().unwrap_or(0),
                        attempt_id,
                        state: None,
                        rearmed: false,
                    });
                }
            }
            REARMED => {
                if let (Some(i), Some(attempt_id)) =
                    (wake(&mut wakes), text(&payload, "attempt_id"))
                    && let Some(turn) = wakes[i]
                        .turns
                        .iter_mut()
                        .find(|t| t.attempt_id == attempt_id)
                {
                    turn.rearmed = true;
                }
            }
            _ => {
                if let Some(i) = wake(&mut wakes) {
                    wakes[i].superseded = true;
                }
            }
        }
    }
    for wake in &mut wakes {
        for turn in &mut wake.turns {
            let state: Option<String> = tx
                .query_row(
                    "SELECT state FROM attempts WHERE attempt_id = ?1",
                    params![turn.attempt_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            turn.state = state
                .as_deref()
                .map(super::writer::parse::<AttemptState>)
                .transpose()?;
        }
    }
    Ok(wakes)
}
