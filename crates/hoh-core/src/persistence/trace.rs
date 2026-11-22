//! The rows of a run's requirement trace (REQ-032; docs/adr/ADR-039.md decision 2), read inside the caller's read
//! transaction and projected by the pure `crate::trace`. The spec file is the current candidate's frozen spec object
//! (`.hoh/objects`, beside the store), else the run's revision rebuilt from the rows its approval wrote.

use super::{PersistError, sqlite_error};
use crate::trace::{Input, SpecSource};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::path::PathBuf;

/// The project root of the store at `conn`: the parent of its `.hoh/` (the host always opens `<project>/.hoh/…`).
fn project_root(conn: &Connection) -> Option<PathBuf> {
    let db = std::path::Path::new(conn.path()?);
    let state = db.parent()?;
    Some(if state.file_name().is_some_and(|n| n == ".hoh") {
        state.parent().unwrap_or(state).to_owned()
    } else {
        state.to_owned()
    })
}

/// Run `run_id`'s trace export inside the caller's read transaction; none when there is no such run.
pub(super) fn export(conn: &Connection, run_id: &str) -> Result<Option<Value>, PersistError> {
    let run: Option<(Option<String>, Option<i64>, Option<String>)> = conn
        .query_row(
            "SELECT r.spec_id, s.revision, s.sha256 FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id \
             WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((spec_id, spec_revision, digest)) = run else {
        return Ok(None);
    };
    // The run's latest candidate (its last candidate.frozen, as the acceptance projection orders them) is current
    // when it binds the current revision's digest.
    let latest: Option<(String, String)> = conn
        .query_row(
            "SELECT c.candidate_id, c.spec_sha256, coalesce((SELECT max(e.seq) FROM events e WHERE e.run_id = c.run_id \
               AND e.kind = 'candidate.frozen' AND json_extract(e.payload_json, '$.candidate_id') = c.candidate_id), 0) \
               AS frozen FROM candidates c WHERE c.run_id = ?1 ORDER BY frozen DESC, c.rowid DESC LIMIT 1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let current = latest.filter(|(_, sha)| digest.as_deref() == Some(sha.as_str()));
    let object = current.as_ref().and_then(|(_, sha)| {
        let bytes = crate::objects::get(&project_root(conn)?, sha).ok()?;
        serde_json::from_slice::<Value>(&bytes).ok()
    });
    let (spec_source, spec) = match (object, &spec_id) {
        (Some(spec), _) => (SpecSource::Candidate, spec),
        (None, Some(spec_id)) => (SpecSource::Revision, super::spec::indexed(conn, spec_id)?),
        (None, None) => (SpecSource::Revision, Value::Null),
    };
    Ok(Some(crate::trace::export(&Input {
        run_id: run_id.to_owned(),
        spec_revision,
        spec_source,
        spec,
        candidate_id: current.map(|(id, _)| id),
    })))
}
