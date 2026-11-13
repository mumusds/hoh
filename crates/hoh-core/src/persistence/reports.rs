//! A verifier's report taken in (docs/adr/ADR-029.md decisions 7 and 8): judged against its attempt, its round, its
//! sessions and its native attempt, and kept as an `independent_reports` row CURRENT, STALE or REJECTED in the
//! caller's transaction; a CURRENT row made STALE once its candidate is superseded.

use super::runner::{ArtifactRow, artifact_id};
use super::writer::{self, Commit, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::verification::classify::Report;
use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// The native attempt statuses a report may come from: contracts/release-policy.json `accepted_attempt_statuses`
/// (locked; a test checks that they agree).
pub(super) const ACCEPTED_ATTEMPT_STATUSES: [&str; 2] = ["NATIVE_TURN_COMPLETED", "COMPLETED"];

/// A report to take in, as the host checked it (decision 7): its attempt, the report, its three objects, and its
/// native attempt's session ids and status (the store reads no object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportIntake {
    pub attempt_id: String,
    pub report: Value,
    pub report_artifact: ArtifactRow,
    pub raw_artifact: ArtifactRow,
    pub native_artifact: ArtifactRow,
    pub native_session_ids: Vec<String>,
    pub native_status: String,
}

/// A report row's state (`independent_reports.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportState {
    Current,
    Stale,
    Rejected,
}

impl ReportState {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportState::Current => "CURRENT",
            ReportState::Stale => "STALE",
            ReportState::Rejected => "REJECTED",
        }
    }

    fn parse(state: &str) -> Result<ReportState, PersistError> {
        match state {
            "CURRENT" => Ok(ReportState::Current),
            "STALE" => Ok(ReportState::Stale),
            "REJECTED" => Ok(ReportState::Rejected),
            other => Err(PersistError::IntegrityViolation(format!(
                "unknown report state {other:?} in the database"
            ))),
        }
    }
}

/// What an intake did: its report's id (none when its session is not in `sessions`: no row), its state and, REJECTED,
/// its reasons (`CODE: detail`, in decision 7's order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntakeState {
    pub report_id: Option<String>,
    pub state: ReportState,
    pub reasons: Vec<String>,
}

/// Take report `intake` in, in the caller's transaction (decision 7's store side): its objects registered, then its
/// row (`report_id` = `child_id(attempt, "report")`): REJECTED with its reasons in decision 7's fixed order and
/// `report.rejected`, else STALE (`report.stale`) or CURRENT by decision 8; no row when its session is not in
/// `sessions` (`report.rejected{report_id: null}`, SESSION_UNKNOWN). Replayed with the same content (its report and raw
/// session objects), nothing is written and its state is returned; with other content, `KeyReuse`.
pub(super) fn intake_in(
    tx: &Transaction<'_>,
    intake: &ReportIntake,
    ctx: &writer::Ctx<'_>,
) -> Result<IntakeState, PersistError> {
    crate::verification::report::conforms(&intake.report).map_err(|e| {
        PersistError::IntegrityViolation(format!(
            "the report of attempt {}: {e}",
            intake.attempt_id
        ))
    })?;
    let report_id = child_id(&intake.attempt_id, "report");
    let (run_id, project_id, task_id, task_role, session): (
        String,
        String,
        String,
        String,
        Option<String>,
    ) = tx
        .query_row(
            "SELECT t.run_id, r.project_id, t.task_id, t.role, a.session_id FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id JOIN runs r ON r.run_id = t.run_id WHERE a.attempt_id = ?1",
            params![intake.attempt_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(sqlite_error)?
        .ok_or_else(|| PersistError::IntegrityViolation(format!("no attempt {}", intake.attempt_id)))?;
    if let Some(done) = replayed(tx, &report_id, intake)? {
        return Ok(done);
    }
    let (round_id, _) =
        super::verification::round_of_task(tx, &run_id, &task_id)?.ok_or_else(|| {
            PersistError::IntegrityViolation(format!("{task_id} is no round's verifier"))
        })?;
    let candidate: String = tx
        .query_row(
            "SELECT json_extract(payload_json, '$.candidate_id') FROM events WHERE kind = ?1 AND run_id = ?2 \
             AND json_extract(payload_json, '$.round_id') = ?3",
            params![super::verification::ROUND_OPENED, run_id, round_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    super::runner::register(
        tx,
        &[
            intake.report_artifact.clone(),
            intake.native_artifact.clone(),
            intake.raw_artifact.clone(),
        ],
    )?;
    let text = |key: &str| intake.report[key].as_str().unwrap_or_default().to_owned();
    let (role, reported_session) = (text("role"), text("session_id"));
    let reasons = judge(
        tx,
        intake,
        &Judged {
            run_id: &run_id,
            round_id: &round_id,
            candidate: &candidate,
            task_role: &task_role,
            session: session.as_deref(),
        },
    )?;
    let event = |name: &str, kind: &str, payload: Value| NewEvent {
        project_id: project_id.clone(),
        event_id: child_id(&report_id, name),
        run_id: Some(run_id.clone()),
        kind: kind.to_owned(),
        payload,
    };
    let reference =
        json!({"path": intake.report_artifact.path, "sha256": intake.report_artifact.sha256});
    // A session not in `sessions` cannot be a row's (its foreign key): only its rejection (decision 7).
    if reasons.iter().any(|r| r.starts_with(SESSION_UNKNOWN)) {
        writer::write(
            tx,
            &Commit {
                events: vec![event(
                    "rejected",
                    "report.rejected",
                    json!({"report_id": null, "attempt_id": intake.attempt_id, "reasons": reasons,
                        "report": reference}),
                )],
                ..Commit::default()
            },
            ctx,
        )?;
        return Ok(IntakeState {
            report_id: None,
            state: ReportState::Rejected,
            reasons,
        });
    }
    let (state, events) = if !reasons.is_empty() {
        (
            ReportState::Rejected,
            vec![event(
                "rejected",
                "report.rejected",
                json!({"report_id": report_id, "attempt_id": intake.attempt_id, "reasons": reasons,
                    "report": reference}),
            )],
        )
    } else if let Some(reason) = stale_reason(tx, &run_id, &candidate)? {
        (
            ReportState::Stale,
            vec![event(
                "stale",
                "report.stale",
                json!({"report_ids": [report_id], "candidate_id": candidate, "reason": reason, "by": "intake"}),
            )],
        )
    } else {
        (ReportState::Current, Vec::new())
    };
    // A rejected report on another candidate is kept with its round's (the foreign key; the report's is in its reason,
    // M2T18D-853).
    tx.execute(
        "INSERT INTO independent_reports(report_id, run_id, candidate_id, role, session_id, spec_sha256, status, \
         state, reject_reason, attempt_id, report_artifact_id, raw_session_artifact_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            report_id,
            run_id,
            candidate,
            role,
            reported_session,
            text("spec_sha256"),
            text("status"),
            state.as_str(),
            (!reasons.is_empty()).then(|| reasons.join("; ")),
            intake.attempt_id,
            artifact_id(&intake.report_artifact.sha256, &intake.report_artifact.path),
            artifact_id(&intake.raw_artifact.sha256, &intake.raw_artifact.path),
            crate::time::now_rfc3339()
        ],
    )
    .map_err(sqlite_error)?;
    writer::write(
        tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(IntakeState {
        report_id: Some(report_id),
        state,
        reasons,
    })
}

/// The code of a report whose session is not in `sessions`.
const SESSION_UNKNOWN: &str = "SESSION_UNKNOWN";

/// The prefix of the detail of a reason that is a session conflict (decision 7).
const CONFLICT: &str = "session conflict:";

/// What a report is judged against.
struct Judged<'a> {
    run_id: &'a str,
    round_id: &'a str,
    /// The candidate of the attempt's round.
    candidate: &'a str,
    /// The role of the attempt's task.
    task_role: &'a str,
    /// The attempt's HoH session.
    session: Option<&'a str>,
}

/// Why report `intake` is rejected, in decision 7's fixed order (`CODE: detail`); none when it is not. A session not in
/// `sessions` is only `SESSION_UNKNOWN`.
fn judge(
    tx: &Transaction<'_>,
    intake: &ReportIntake,
    on: &Judged<'_>,
) -> Result<Vec<String>, PersistError> {
    let text = |key: &str| intake.report[key].as_str().unwrap_or_default();
    let (role, session) = (text("role"), text("session_id"));
    let session_role: Option<String> = tx
        .query_row(
            "SELECT role FROM sessions WHERE session_id = ?1",
            params![session],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(session_role) = session_role else {
        return Ok(vec![format!(
            "{SESSION_UNKNOWN}: the report's session {session} is not in sessions"
        )]);
    };
    let mut reasons = Vec::new();
    if role != on.task_role {
        reasons.push(format!(
            "REPORT_ROLE_MISMATCH: the report is a {role}'s, its attempt's task a {}'s",
            on.task_role
        ));
    }
    if Some(session) != on.session {
        reasons.push(format!(
            "REPORT_SESSION_MISMATCH: the report names session {session}, its attempt ran in {}",
            on.session.unwrap_or("none")
        ));
    }
    if session_role != role {
        reasons.push(format!(
            "SESSION_ROLE_MISMATCH: {CONFLICT} session {session} is a {session_role}'s, not a {role}'s"
        ));
    }
    if super::verification::conflicting_sessions(tx, on.run_id, on.round_id, on.task_role)?
        .contains(session)
    {
        reasons.push(format!(
            "SESSION_NOT_INDEPENDENT: {CONFLICT} session {session} is a source attempt's, the commander's, the \
             developer's or the other verifier's"
        ));
    }
    if text("candidate_id") != on.candidate {
        reasons.push(format!(
            "REPORT_CANDIDATE_MISMATCH: the report names candidate {}, its round verifies {}",
            text("candidate_id"),
            on.candidate
        ));
    }
    let spec: Option<String> = tx
        .query_row(
            "SELECT spec_sha256 FROM candidates WHERE candidate_id = ?1",
            params![on.candidate],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    if spec.as_deref() != Some(text("spec_sha256")) {
        reasons.push(format!(
            "REPORT_SPEC_MISMATCH: the report names specification {}, candidate {} is of {}",
            text("spec_sha256"),
            on.candidate,
            spec.as_deref().unwrap_or("none")
        ));
    }
    let named = |key: &str, given: &ArtifactRow| {
        intake.report[key] == json!({"path": given.path, "sha256": given.sha256})
    };
    if !named("raw_session", &intake.raw_artifact)
        || !named("native_attempt", &intake.native_artifact)
    {
        reasons.push(format!(
            "REPORT_OBJECTS_MISMATCH: the report names raw_session {} and native_attempt {}, the objects given are \
             {} and {}",
            intake.report["raw_session"],
            intake.report["native_attempt"],
            json!({"path": intake.raw_artifact.path, "sha256": intake.raw_artifact.sha256}),
            json!({"path": intake.native_artifact.path, "sha256": intake.native_artifact.sha256})
        ));
    }
    if !ACCEPTED_ATTEMPT_STATUSES.contains(&intake.native_status.as_str()) {
        reasons.push(format!(
            "NATIVE_ATTEMPT_INCOMPLETE: its native attempt is {}, not one of {ACCEPTED_ATTEMPT_STATUSES:?}",
            intake.native_status
        ));
    }
    let developers = developer_sessions(tx, on.run_id, on.candidate)?;
    let shared: Vec<&String> = intake
        .native_session_ids
        .iter()
        .filter(|s| developers.contains(*s))
        .collect();
    if !shared.is_empty() {
        reasons.push(format!(
            "NATIVE_SESSION_NOT_INDEPENDENT: {CONFLICT} its native attempt's sessions {shared:?} are a source \
             attempt's"
        ));
    }
    Ok(reasons)
}

/// The HoH and native sessions of every attempt of candidate `candidate`'s source tasks (the sources of its rounds)
/// in run `run_id`: their `attempts.session_id`, those sessions' `native_id`, and the native sessions their turns
/// recorded (`session.native`).
fn developer_sessions(
    tx: &Transaction<'_>,
    run_id: &str,
    candidate: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "WITH sources AS (SELECT DISTINCT s.value AS task_id FROM events e, \
               json_each(e.payload_json, '$.source_task_ids') s WHERE e.kind = ?1 AND e.run_id = ?2 \
               AND json_extract(e.payload_json, '$.candidate_id') = ?3), \
             tried AS (SELECT a.attempt_id, a.session_id FROM attempts a WHERE a.task_id IN (SELECT task_id FROM sources)) \
             SELECT session_id FROM tried WHERE session_id IS NOT NULL \
             UNION SELECT s.native_id FROM sessions s WHERE s.session_id IN (SELECT session_id FROM tried) \
               AND s.native_id IS NOT NULL \
             UNION SELECT json_extract(n.payload_json, '$.native_id') FROM events n WHERE n.kind = 'session.native' \
               AND json_extract(n.payload_json, '$.attempt_id') IN (SELECT attempt_id FROM tried) \
               AND json_extract(n.payload_json, '$.native_id') IS NOT NULL",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(
            params![super::verification::ROUND_OPENED, run_id, candidate],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// Report `report_id` taken in before: its state when `intake` has the same content (its report and raw session
/// objects), `KeyReuse` when not; none when it was not.
fn replayed(
    tx: &Transaction<'_>,
    report_id: &str,
    intake: &ReportIntake,
) -> Result<Option<IntakeState>, PersistError> {
    let reuse = || {
        PersistError::KeyReuse(format!(
            "report {report_id} was taken in before with other content"
        ))
    };
    let rejection: Option<String> = tx
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![child_id(report_id, "rejected")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let rejection: Value = rejection
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or(Value::Null);
    let reasons: Vec<String> = rejection["reasons"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| r.as_str().map(str::to_owned))
        .collect();
    let row: Option<(String, String, String)> = tx
        .query_row(
            "SELECT report_artifact_id, raw_session_artifact_id, state FROM independent_reports WHERE report_id = ?1",
            params![report_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    match row {
        Some((report, raw, state)) => {
            if report != artifact_id(&intake.report_artifact.sha256, &intake.report_artifact.path)
                || raw != artifact_id(&intake.raw_artifact.sha256, &intake.raw_artifact.path)
            {
                return Err(reuse());
            }
            Ok(Some(IntakeState {
                report_id: Some(report_id.to_owned()),
                state: ReportState::parse(&state)?,
                reasons,
            }))
        }
        // Taken in with its session unknown: its rejection alone.
        None if !rejection.is_null() => {
            if rejection["report"]
                != json!({"path": intake.report_artifact.path, "sha256": intake.report_artifact.sha256})
            {
                return Err(reuse());
            }
            Ok(Some(IntakeState {
                report_id: None,
                state: ReportState::Rejected,
                reasons,
            }))
        }
        None => Ok(None),
    }
}

/// Why candidate `candidate` of run `run_id` no longer stands for its reports (decision 8): `superseded` when another
/// candidate of the run was frozen after it (a `candidate.frozen` later than its own), `candidate_stale` when a
/// `candidate.stale` names it; none when neither.
fn stale_reason(
    tx: &Transaction<'_>,
    run_id: &str,
    candidate: &str,
) -> Result<Option<&'static str>, PersistError> {
    let (superseded, stale): (bool, bool) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM events f WHERE f.kind = 'candidate.frozen' AND f.run_id = ?1 \
               AND json_extract(f.payload_json, '$.candidate_id') <> ?2 \
               AND f.seq > coalesce((SELECT max(o.seq) FROM events o WHERE o.kind = 'candidate.frozen' \
                 AND o.run_id = ?1 AND json_extract(o.payload_json, '$.candidate_id') = ?2), 0)), \
             EXISTS(SELECT 1 FROM events s WHERE s.run_id = ?1 AND s.kind = 'candidate.stale' \
               AND json_extract(s.payload_json, '$.candidate_id') = ?2)",
            params![run_id, candidate],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    Ok(if superseded {
        Some("superseded")
    } else if stale {
        Some("candidate_stale")
    } else {
        None
    })
}

/// The report status of `role` over rounds whose verifier attempts are `rounds` (each in its order; docs/adr/ADR-029.md
/// decision 5's `report_status`, the join's and the snapshot's): each round's latest CURRENT report of the role (never a
/// STALE or REJECTED one), merged FAIL > UNKNOWN > a round without one (NOT_RUN) > PASS; NOT_RUN for no round. With it,
/// the findings' summaries of its FAIL reports (their `verification.report` events), sorted.
pub(super) fn report_status(
    tx: &Transaction<'_>,
    rounds: &[Vec<String>],
    role: &str,
) -> Result<(Report, Vec<String>), PersistError> {
    let mut statuses = Vec::new();
    let mut findings = std::collections::BTreeSet::new();
    for attempts in rounds {
        let mut latest: Option<(String, Option<String>)> = None;
        for attempt in attempts {
            let found: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT r.status, (SELECT json_extract(e.payload_json, '$.findings') FROM events e \
                       WHERE e.run_id = r.run_id AND e.kind = 'verification.report' \
                       AND json_extract(e.payload_json, '$.attempt_id') = r.attempt_id \
                       ORDER BY e.seq DESC LIMIT 1) \
                     FROM independent_reports r WHERE r.attempt_id = ?1 AND r.role = ?2 AND r.state = 'CURRENT' \
                     ORDER BY r.rowid DESC LIMIT 1",
                    params![attempt, role],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            if found.is_some() {
                latest = found;
            }
        }
        let status = match latest {
            None => Report::NotRun,
            Some((status, summaries)) => {
                let status = match status.as_str() {
                    "PASS" => Report::Pass,
                    "FAIL" => Report::Fail,
                    "UNKNOWN" => Report::Unknown,
                    _ => Report::NotRun,
                };
                if status == Report::Fail {
                    findings.extend(
                        summaries
                            .and_then(|f| serde_json::from_str::<Vec<String>>(&f).ok())
                            .unwrap_or_default(),
                    );
                }
                status
            }
        };
        statuses.push(status);
    }
    let merged = if statuses.contains(&Report::Fail) {
        Report::Fail
    } else if statuses.contains(&Report::Unknown) {
        Report::Unknown
    } else if statuses.is_empty() || statuses.contains(&Report::NotRun) {
        Report::NotRun
    } else {
        Report::Pass
    };
    Ok((merged, findings.into_iter().collect()))
}

/// [`intake_in`] in a transaction of its own ([`super::Store::record_report`]): only the report's intake (its row, its
/// events, its findings' Issues and observation facts, materialised by `settle_all`); no task or attempt moves.
pub(super) fn record(
    conn: &mut rusqlite::Connection,
    intake: &ReportIntake,
    ctx: &writer::Ctx<'_>,
) -> Result<IntakeState, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let done = intake_in(&tx, intake, ctx)?;
    // Its findings as Issues and its observations as facts, as in a verifier's settle (docs/adr/ADR-029.md decisions
    // 9 and 10); no TaskResult here, so no confirmation.
    if let (Some(report_id), ReportState::Current | ReportState::Stale) =
        (&done.report_id, done.state)
    {
        super::issues::take_report(&tx, &intake.attempt_id, report_id, &intake.report, &[])?;
        let run_id: String = tx
            .query_row(
                "SELECT run_id FROM independent_reports WHERE report_id = ?1",
                params![report_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        super::issues::settle_all(&tx, &run_id)?;
    }
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

/// Make the CURRENT reports of run `run_id` on `candidates` STALE (decision 8) with one `report.stale{report_ids,
/// reason, by}`; nothing when none is CURRENT. Written in the caller's transaction as the freeze writes its own events
/// (it has no writer context). The reports made STALE, by id.
pub(super) fn stale_reports(
    tx: &Transaction<'_>,
    run_id: &str,
    candidates: &[String],
    reason: &str,
    by: &str,
) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT report_id FROM independent_reports WHERE run_id = ?1 AND state = 'CURRENT' \
             AND candidate_id IN (SELECT value FROM json_each(?2)) ORDER BY report_id",
        )
        .map_err(sqlite_error)?;
    let flipped: Vec<String> = statement
        .query_map(params![run_id, json!(candidates).to_string()], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let Some(first) = flipped.first() else {
        return Ok(flipped);
    };
    tx.execute(
        "UPDATE independent_reports SET state = 'STALE' WHERE state = 'CURRENT' \
         AND report_id IN (SELECT value FROM json_each(?1))",
        params![json!(flipped).to_string()],
    )
    .map_err(sqlite_error)?;
    // Each report goes STALE once, so the first of those made STALE names the event.
    tx.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
         SELECT project_id, ?1, run_id, 'report.stale', ?2, ?3 FROM runs WHERE run_id = ?4",
        params![
            child_id(first, "made-stale"),
            json!({"report_ids": flipped, "reason": reason, "by": by}).to_string(),
            crate::time::now_rfc3339(),
            run_id
        ],
    )
    .map_err(sqlite_error)?;
    Ok(flipped)
}

/// Every CURRENT report of run `run_id` whose candidate no longer stands made STALE (decision 8's sweep, at the start of
/// each verification reconcile, so no writer of `candidate.stale` needs to remember it): `superseded` first, then
/// `candidate_stale`, by `reconcile`. The reports made STALE.
pub(super) fn sweep(tx: &Transaction<'_>, run_id: &str) -> Result<Vec<String>, PersistError> {
    let mut statement = tx
        .prepare(
            "SELECT DISTINCT candidate_id FROM independent_reports WHERE run_id = ?1 AND state = 'CURRENT' \
             ORDER BY candidate_id",
        )
        .map_err(sqlite_error)?;
    let candidates: Vec<String> = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut by_reason: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    for candidate in candidates {
        if let Some(reason) = stale_reason(tx, run_id, &candidate)? {
            by_reason.entry(reason).or_default().push(candidate);
        }
    }
    let mut flipped = Vec::new();
    for reason in ["superseded", "candidate_stale"] {
        if let Some(candidates) = by_reason.get(reason) {
            flipped.extend(stale_reports(tx, run_id, candidates, reason, "reconcile")?);
        }
    }
    Ok(flipped)
}

#[cfg(test)]
mod tests;
