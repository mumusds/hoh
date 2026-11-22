//! A specification review's report taken in (REQ-031 behaviour 4, CR-026, docs/adr/ADR-035.md): the single intake entry
//! that checks the report and its native attempt record against each other, against their `spec_review.opened` and
//! the attempt taking them in, against `sessions`, and for the reviewer's independence of the revision's drafters and
//! the Commander (CR-029, through `verification::identity`); every reason is collected, and the outcome is one
//! `spec_review.reported` event, CURRENT or REJECTED. No `independent_reports` row, no round, no candidate.

use super::runner::ArtifactRow;
use super::writer::{self, Commit, Intent, NewEvent};
use super::{PersistError, ReportState, sqlite_error};
use crate::ids::child_id;
use crate::verification::identity::{self, Identities};
use crate::verification::spec_report::{self, Binding, codes};
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// The event that opens a review.
pub(super) const OPENED: &str = "spec_review.opened";
/// The event that records a review's report taken in.
pub(super) const REPORTED: &str = "spec_review.reported";
/// An object the host kept (`objects::put`) for an intake: its artifact and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptObject {
    pub artifact: ArtifactRow,
    pub bytes: Vec<u8>,
}

/// A specification review's report to take in: the review and the attempt taking it in, its report and native
/// attempt record as the host built and kept them (none when there is none: no hoh-report block, no record), its raw
/// session log's artifact and the digest the host recomputed from that log's bytes. The host builds; only this entry
/// checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecReviewIntake {
    pub review_id: String,
    pub attempt_id: String,
    pub report: Option<KeptObject>,
    pub native: Option<KeptObject>,
    pub raw: ArtifactRow,
    pub raw_sha256: String,
}

/// What an intake recorded: CURRENT, or REJECTED with its reasons (`<code>: <detail>`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecReviewTaken {
    pub state: ReportState,
    pub reasons: Vec<String>,
}

/// What `spec_review.opened` bound the review to.
struct Opened {
    project_id: String,
    run_id: String,
    spec_id: String,
    task_id: String,
    binding: Binding,
}

/// Take `intake` in, in a transaction of its own ([`super::Store::intake_spec_review`]).
pub(super) fn record(
    conn: &mut rusqlite::Connection,
    intake: &SpecReviewIntake,
    ctx: &writer::Ctx<'_>,
) -> Result<SpecReviewTaken, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let taken = intake_in(&tx, intake, ctx)?;
    writer::commit(tx, ctx)?;
    Ok(taken)
}

/// Take `intake` in, in the caller's transaction: its objects registered, every check run, and one `spec_review.reported`
/// (`child_id(attempt, "spec-review/reported")`). Replayed with the same review, objects and recomputed log digest,
/// nothing is written and its outcome is returned; with any of them changed, `KeyReuse`. A review that was never opened,
/// an attempt that does not exist, or an object whose bytes are not its artifact's are the caller's integrity errors,
/// not reasons. A raw artifact whose digest is not the recomputed one is a reason and is not registered.
pub(super) fn intake_in(
    tx: &Transaction<'_>,
    intake: &SpecReviewIntake,
    ctx: &writer::Ctx<'_>,
) -> Result<SpecReviewTaken, PersistError> {
    for kept in [&intake.report, &intake.native].into_iter().flatten() {
        let digest = crate::context::sha256_hex(&kept.bytes);
        if digest != kept.artifact.sha256 {
            return Err(PersistError::IntegrityViolation(format!(
                "the object {} of review {} hashes to {digest}, not to its artifact's {}",
                kept.artifact.path, intake.review_id, kept.artifact.sha256
            )));
        }
    }
    let opened = opened(tx, &intake.review_id)?;
    let (task_id, attempt_session): (String, Option<String>) = tx
        .query_row(
            "SELECT task_id, session_id FROM attempts WHERE attempt_id = ?1",
            params![intake.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| {
            PersistError::IntegrityViolation(format!("no attempt {}", intake.attempt_id))
        })?;
    let event_id = child_id(&intake.attempt_id, "spec-review/reported");
    let reference = |a: &ArtifactRow| json!({"path": a.path, "sha256": a.sha256});
    // What a replay must repeat: the review, the three objects and the digest recomputed from the turn log.
    let identity = json!({
        "review_id": intake.review_id,
        "report": intake.report.as_ref().map(|k| reference(&k.artifact)),
        "raw_session": reference(&intake.raw),
        "native_attempt": intake.native.as_ref().map(|k| reference(&k.artifact)),
        "raw_sha256": intake.raw_sha256,
    });
    if let Some(done) = replayed(tx, &event_id, &identity)? {
        return Ok(done);
    }
    let raw_is_its_log = intake.raw.sha256 == intake.raw_sha256;
    let mut artifacts = Vec::new();
    if raw_is_its_log {
        artifacts.push(intake.raw.clone());
    }
    artifacts.extend(
        [&intake.report, &intake.native]
            .into_iter()
            .flatten()
            .map(|k| k.artifact.clone()),
    );
    super::runner::register(tx, &artifacts)?;
    let mut expected = opened.binding.clone();
    expected.attempt_id = intake.attempt_id.clone();
    let checked = spec_report::check(
        intake.report.as_ref().map(|k| k.bytes.as_slice()),
        intake.native.as_ref().map(|k| k.bytes.as_slice()),
        &intake.raw_sha256,
        &expected,
    );
    let mut reasons = checked.reasons.clone();
    // The objects the report names are the ones given and registered, and the raw artifact is the turn log's.
    if let Some(report) = &checked.report {
        let given = [
            (
                "raw_session",
                codes::RAW_SESSION_REF_MISMATCH,
                Some(&identity["raw_session"]),
            ),
            (
                "native_attempt",
                codes::NATIVE_ATTEMPT_REF_MISMATCH,
                intake.native.as_ref().map(|_| &identity["native_attempt"]),
            ),
        ];
        for (key, code, given) in given {
            if let Some(given) = given.filter(|given| report[key] != **given) {
                reasons.push(format!(
                    "{code}: the report names {key} {}, the object given is {given}",
                    report[key]
                ));
            }
        }
    }
    if !raw_is_its_log {
        reasons.push(format!(
            "{}: the raw session artifact names sha256 {}, its log's bytes hash to {}",
            codes::RAW_SESSION_ARTIFACT_MISMATCH,
            intake.raw.sha256,
            intake.raw_sha256
        ));
    }
    if task_id != opened.task_id {
        reasons.push(format!(
            "{}: attempt {} is of task {task_id}, review {} of task {}",
            codes::ATTEMPT_NOT_OF_REVIEW,
            intake.attempt_id,
            intake.review_id,
            opened.task_id
        ));
    }
    let session = checked
        .report
        .as_ref()
        .and_then(|r| r["session_id"].as_str())
        .map(str::to_owned);
    if let Some(session) = &session {
        reasons.extend(session_reasons(
            tx,
            &opened,
            session,
            attempt_session.as_deref(),
            checked.native.as_ref(),
        )?);
    }
    let state = if reasons.is_empty() {
        ReportState::Current
    } else {
        ReportState::Rejected
    };
    let mut payload = json!({
        "review_id": intake.review_id, "spec_id": opened.spec_id, "spec_revision": opened.binding.spec_revision,
        "status": checked.report.as_ref().map(|r| r["status"].clone()), "state": state.as_str(),
        "reasons": reasons, "session_id": session, "attempt_id": intake.attempt_id,
        // What readiness judges a PASS by (REQ-031 behaviour 3): each finding's id and severity.
        "findings": checked.report.as_ref().map(|r| r["findings"].as_array().into_iter().flatten()
            .map(|f| json!({"id": f["id"], "severity": f["severity"]})).collect::<Vec<_>>()),
    });
    for key in REPLAY_KEYS.iter().filter(|key| **key != "review_id") {
        payload[*key] = identity[*key].clone();
    }
    writer::write(
        tx,
        &Commit {
            events: vec![NewEvent {
                intent: Intent::Progress,
                project_id: opened.project_id.clone(),
                event_id,
                run_id: Some(opened.run_id.clone()),
                kind: REPORTED.to_owned(),
                payload,
            }],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(SpecReviewTaken { state, reasons })
}

/// The `spec_review.opened` of `review_id`.
fn opened(tx: &Transaction<'_>, review_id: &str) -> Result<Opened, PersistError> {
    let (project_id, run_id, payload): (String, Option<String>, String) = tx
        .query_row(
            "SELECT project_id, run_id, payload_json FROM events WHERE kind = ?1 \
             AND json_extract(payload_json, '$.review_id') = ?2 ORDER BY seq LIMIT 1",
            params![OPENED, review_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| {
            PersistError::IntegrityViolation(format!("no review {review_id} was opened"))
        })?;
    let p: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("{OPENED} of {review_id}: {e}")))?;
    let text = |key: &str| {
        p[key].as_str().map(str::to_owned).ok_or_else(|| {
            PersistError::IntegrityViolation(format!("{OPENED} of {review_id} has no {key}"))
        })
    };
    Ok(Opened {
        project_id,
        run_id: run_id.map_or_else(|| text("run_id"), Ok)?,
        spec_id: text("spec_id")?,
        task_id: text("task_id")?,
        binding: Binding {
            review_id: review_id.to_owned(),
            attempt_id: String::new(),
            spec_revision: p["spec_revision"].as_i64().ok_or_else(|| {
                PersistError::IntegrityViolation(format!(
                    "{OPENED} of {review_id} has no spec_revision"
                ))
            })?,
            spec_sha256: text("spec_sha256")?,
            git_commit: text("git_commit")?,
        },
    })
}

/// The reasons about the report's session `session`: not in `sessions`; not the attempt's; not a reviewer's; sharing an
/// identity with the revision's drafters or the Commander (CR-029).
fn session_reasons(
    tx: &Transaction<'_>,
    opened: &Opened,
    session: &str,
    attempt_session: Option<&str>,
    native: Option<&Value>,
) -> Result<Vec<String>, PersistError> {
    let known: Option<(String, String)> = tx
        .query_row(
            "SELECT agent, role FROM sessions WHERE session_id = ?1",
            params![session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((agent, role)) = known else {
        return Ok(vec![format!(
            "{}: the report's session {session} is not in sessions",
            codes::SESSION_UNKNOWN
        )]);
    };
    let mut reasons = Vec::new();
    if attempt_session != Some(session) {
        reasons.push(format!(
            "{}: the report names session {session}, its attempt ran in {}",
            codes::SESSION_MISMATCH,
            attempt_session.unwrap_or("none")
        ));
    }
    if role != "reviewer" {
        reasons.push(format!(
            "{}: session {session} is a {role}'s, not a reviewer's",
            codes::SESSION_ROLE_MISMATCH
        ));
    }
    // The reviewer: its HoH session, its record's session_ids but its own (of no known kind) and its stored native
    // identities.
    let mut reviewer = identities_of(tx, &BTreeSet::from([session.to_owned()]))?;
    for id in native
        .and_then(|n| n["session_ids"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| *id != session)
    {
        reviewer.claim(&agent, id);
    }
    let authors = identities_of(
        tx,
        &authors(
            tx,
            &opened.run_id,
            &opened.project_id,
            &opened.binding.spec_sha256,
        )?,
    )?;
    let shared = identity::shared(&reviewer, &authors);
    if !shared.is_empty() {
        let named: Vec<String> = shared.iter().map(ToString::to_string).collect();
        reasons.push(format!(
            "{}: the reviewer shares {} with the revision's drafters or the Commander",
            codes::NOT_INDEPENDENT,
            named.join(", ")
        ));
    }
    Ok(reasons)
}

/// The sessions the reviewer must be independent of (CR-026 correction A, docs/adr/ADR-035.md decision 6): for the
/// run's Commander and, for each Task `spec.drafting` names with the revision's digest in the project, for that Task's
/// run and role, the role's current binding, every Attempt of the role's Tasks in that run and the role's sessions
/// of the run's `session.native` records. Their identities are taken by [`identities_of`].
pub(super) fn authors(
    tx: &rusqlite::Connection,
    run_id: &str,
    project_id: &str,
    spec_sha256: &str,
) -> Result<BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "WITH roles(run_id, role) AS (\
               SELECT ?1, 'commander' \
               UNION SELECT t.run_id, t.role FROM tasks t WHERE t.task_id IN (\
                 SELECT json_extract(payload_json, '$.task_id') FROM events WHERE project_id = ?2 \
                   AND kind = 'spec.drafting' AND json_extract(payload_json, '$.spec_sha256') = ?3)) \
             SELECT b.session_id FROM role_bindings b JOIN roles r ON r.run_id = b.run_id AND r.role = b.role \
               WHERE b.session_id IS NOT NULL \
             UNION SELECT a.session_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
               JOIN roles r ON r.run_id = t.run_id AND r.role = t.role WHERE a.session_id IS NOT NULL \
             UNION SELECT s.session_id FROM events e JOIN sessions s \
               ON s.session_id = json_extract(e.payload_json, '$.session_id') \
               JOIN roles r ON r.run_id = e.run_id AND r.role = s.role WHERE e.kind = 'session.native'",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id, project_id, spec_sha256], |r| r.get(0))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The identities of `sessions`: each HoH id, and each (agent, native id) its row and its `session.native` records
/// hold.
pub(super) fn identities_of(
    tx: &rusqlite::Connection,
    sessions: &BTreeSet<String>,
) -> Result<Identities, PersistError> {
    let mut ids = Identities::default();
    let mut statement = tx
        .prepare(
            "SELECT s.agent, s.native_id FROM sessions s WHERE s.session_id = ?1 AND s.native_id IS NOT NULL \
             UNION SELECT s.agent, json_extract(e.payload_json, '$.native_id') FROM events e JOIN sessions s \
               ON s.session_id = json_extract(e.payload_json, '$.session_id') \
               WHERE s.session_id = ?1 AND e.kind = 'session.native' \
                 AND json_extract(e.payload_json, '$.native_id') IS NOT NULL",
        )
        .map_err(sqlite_error)?;
    for session in sessions {
        ids.add_hoh(session);
        let natives = statement
            .query_map(params![session], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(sqlite_error)?;
        for native in natives {
            let (agent, id) = native.map_err(sqlite_error)?;
            ids.add_native(&agent, &id);
        }
    }
    Ok(ids)
}

/// The keys of a `spec_review.reported` a replay must repeat.
const REPLAY_KEYS: [&str; 5] = [
    "review_id",
    "report",
    "raw_session",
    "native_attempt",
    "raw_sha256",
];

/// The outcome of `event_id` taken in before with the same `identity` (its [`REPLAY_KEYS`]); `KeyReuse` with another;
/// none when it was not.
fn replayed(
    tx: &Transaction<'_>,
    event_id: &str,
    identity: &Value,
) -> Result<Option<SpecReviewTaken>, PersistError> {
    let payload: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![event_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(payload) = payload else {
        return Ok(None);
    };
    let p: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("{event_id}: {e}")))?;
    if REPLAY_KEYS.iter().any(|key| p[*key] != identity[*key]) {
        return Err(PersistError::KeyReuse(format!(
            "the review report of {event_id} was taken in before with another review, other objects or another \
             log digest"
        )));
    }
    let reasons: Vec<String> = p["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_owned))
        .collect();
    Ok(Some(SpecReviewTaken {
        state: if p["state"] == "CURRENT" {
            ReportState::Current
        } else {
            ReportState::Rejected
        },
        reasons,
    }))
}

#[cfg(test)]
mod tests;
