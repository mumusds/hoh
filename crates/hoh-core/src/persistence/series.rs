//! Counting the members of a series of event ids (docs/adr/ADR-030.md decision 5, M2T15D-202, -352; ADR-025
//! decision 5's rule: a range on the unique index, no scan of payloads).

use super::{PersistError, sqlite_error};
use rusqlite::{Connection, params};

/// How many members the series `stem` has ([`crate::ids::member`]): the event ids in `[<stem>/, <stem>0)` on the
/// unique index (`0` is the byte after `/`) whose rest after `<stem>/` is a non-empty run of digits. Another family
/// numbered under a member (`<stem>/1/…`), a shortened child id (its rest holds `~`), `<stem>/x` and `<stem>/` alone are
/// not members. SQLite's `length` and `substr` count characters, so the rest is cut after as many characters as
/// `<stem>/` has.
pub(super) fn count(tx: &Connection, stem: &str) -> Result<u64, PersistError> {
    let from = format!("{stem}/");
    let below = format!("{stem}0");
    let skip = i64::try_from(from.chars().count()).map_err(|_| {
        PersistError::IntegrityViolation(format!("series stem {stem:?} is too long"))
    })?;
    let n: i64 = tx
        .query_row(
            "SELECT count(*) FROM events WHERE event_id >= ?1 AND event_id < ?2 \
             AND length(event_id) > ?3 AND substr(event_id, ?3 + 1) NOT GLOB '*[^0-9]*'",
            params![from, below, skip],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(u64::try_from(n).unwrap_or(0))
}

#[cfg(test)]
mod tests;
