//! Resource pin publication shares the StartRun/checkpoint transaction. No schema changes (ADR-034).
use super::{
    PersistError, sqlite_error,
    writer::{Intent, NewEvent},
};
use crate::resource_bundle::{Bundle, Pin};
use rusqlite::{Connection, OptionalExtension, params};

pub(super) fn latest(
    conn: &Connection,
    run: &str,
    role: &str,
    version: Option<&str>,
) -> Result<Option<Pin>, PersistError> {
    let text: Option<String> = conn.query_row(
        "SELECT payload_json FROM events WHERE run_id = ?1 AND kind = 'resource.pinned' AND json_extract(payload_json, '$.role') = ?2 AND (?3 IS NULL OR json_extract(payload_json, '$.resource_bundle_version') = ?3) ORDER BY seq DESC LIMIT 1",
        params![run, role, version], |r| r.get(0)).optional().map_err(sqlite_error)?;
    text.map(|s| {
        serde_json::from_str(&s).map_err(|e| PersistError::IntegrityViolation(e.to_string()))
    })
    .transpose()
}

pub(super) fn events(
    conn: &Connection,
    project: &str,
    run: &str,
    roles: impl IntoIterator<Item = String>,
    bundle: &Bundle,
) -> Result<Vec<NewEvent>, PersistError> {
    // Objects are relative to the store, not an untrusted repo_path row. The host always opens <project>/.hoh/state.db.
    let db = conn.path().ok_or_else(|| {
        PersistError::Storage("resource pins need a file-backed store".to_owned())
    })?;
    let state = std::path::Path::new(db)
        .parent()
        .ok_or_else(|| PersistError::Storage("store has no parent".to_owned()))?;
    let root = if state.file_name().is_some_and(|n| n == ".hoh") {
        state.parent().unwrap_or(state)
    } else {
        state
    };
    let mut pin = None;
    let mut events = Vec::new();
    for role in roles {
        let old = latest(conn, run, &role, None)?;
        if old
            .as_ref()
            .is_some_and(|p| p.resource_bundle_version == bundle.version())
        {
            continue;
        }
        let saved = match &pin {
            Some(p) => p,
            None => pin.insert(bundle.store_io(root).map_err(|e| {
                // This is the failed object write's own error, before any SQLite rollback. No room probe or
                // message parsing is needed; other causes (including a read-only file system) retain their detail.
                if super::room::none(&e) {
                    PersistError::Full(e.to_string())
                } else {
                    PersistError::Storage(format!("object store disk I/O error: {e}"))
                }
            })?),
        };
        let previous_seq: i64 = conn.query_row("SELECT coalesce(max(seq), 0) FROM events WHERE run_id = ?1 AND kind = 'resource.pinned' AND json_extract(payload_json, '$.role') = ?2", params![run,role],|r|r.get(0)).map_err(sqlite_error)?;
        events.push(NewEvent {
            intent: Intent::Progress, project_id: project.to_owned(), run_id: Some(run.to_owned()),
            event_id: crate::ids::child_id(run, &format!("resource/{}/{}/after-{previous_seq}", crate::layout::component(&role), saved.resource_bundle_version)),
            kind: "resource.pinned".to_owned(),
            payload: serde_json::json!({"role": role, "resource_bundle_version": saved.resource_bundle_version, "bundle_sha256": saved.bundle_sha256}),
        });
    }
    Ok(events)
}

#[cfg(test)]
mod tests;
