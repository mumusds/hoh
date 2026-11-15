//! The compat suites' admission, publication and end (docs/adr/ADR-030.md decision 10 "何时运行与互斥", K6, K7): each
//! one IMMEDIATE transaction on the writer, so an admission and a writer's grant are serialized by SQLite's write lock.
//!
//! A suite is open from its first deferral or admission (`compat.suite_opened`, member n of the host epoch's opened
//! series) to its end (`compat.suite_ended`, member n of the ended series); while one is open the writer slot is
//! granted to no new holder ([`super::leases::suite_open`]), and another identity's admission is `Busy`. The epoch's
//! counting closes what a host that stopped or crashed left open.

use super::leases::{self, OpenSuite};
use super::writer::{self, Commit, Ctx, NewEvent};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::time::rfc3339_utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::time::SystemTime;

/// A suite deferred because the writer slot is held (once per holder and lease epoch).
pub const SUITE_DEFERRED: &str = "compat.suite_deferred";
/// A suite opened: from now on the writer slot is granted to no new holder until it ends.
pub const SUITE_OPENED: &str = "compat.suite_opened";
/// A suite admitted to run: the writer slot is held by no one.
pub const SUITE_STARTED: &str = "compat.suite_started";
/// A suite ended: by its publication (its status), or by the host (the statuses below).
pub const SUITE_ENDED: &str = "compat.suite_ended";
/// A suite's result published in the manifest.
pub const SUITE_COMPLETED: &str = "compat.suite_completed";

/// The host ended a suite that ran past its limit (`SUITE_LIMIT`).
pub const TIMED_OUT: &str = "timed_out";
/// The host stopped with the suite open.
pub const ABANDONED: &str = "abandoned";
/// The identity the suite was opened for is no longer wanted or no longer the agent's.
pub const WITHDRAWN: &str = "withdrawn";
/// The run earned nothing to publish: its files changed while it ran, or its own probe differed or failed.
pub const DISCARDED: &str = "discarded";
/// The new manifest could not be built (schema) or put in place: the old one is in place.
pub const UNPUBLISHED: &str = "unpublished";

/// Why a deferred suite waits, for whoever reads the deferral (AC-027-04's maintenance window).
const DEFERRED_DETAIL: &str = "维护窗口：写租约释放后自动运行兼容套件";

/// What a compat suite is run for: an agent's version line and identity, as the registry's look found them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuiteSubject {
    pub agent: String,
    pub cli_version: String,
    pub identity: crate::staffing::AgentIdentity,
}

/// What an admission decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Another identity's suite is open in this host epoch (the one named): nothing was written; ask again once it
    /// ended (M2T15D-151).
    Busy(String),
    /// The writer slot is held: the suite is open (so no new writer is granted) and not started; ask again once the
    /// holder ended.
    Deferred(String),
    /// Admitted: run it now, then publish or end it.
    Started(String),
}

/// A suite's result to publish over the manifest the host holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Publication {
    pub project_id: String,
    pub suite_id: String,
    pub agent: String,
    /// The manifest entry the suite earned.
    pub entry: Value,
    /// The manifest the host holds (none: empty); only a publication writes it, and only one suite is open, so it is the
    /// current one.
    pub base: Option<Value>,
    pub files: crate::compat_manifest::Files,
    /// When it is published: the new manifest's `generated_at`.
    pub now: SystemTime,
}

/// What a publication did.
#[derive(Debug, Clone, PartialEq)]
pub enum Published {
    /// The manifest was replaced and the suite ended: this is the manifest now (also when only its directory's sync
    /// failed, which the suite's end says).
    Replaced(Value),
    /// The suite was not open and started (ended by the host, or of an older host epoch): nothing was written.
    Closed,
    /// The new manifest could not be built or put in place (not renamed over the old one): the old one is in place and
    /// the suite ended `unpublished`.
    Unpublished(String),
    /// The manifest was replaced but the transaction did not commit: the suite is still open (the writer slot stays
    /// closed) until the host ends it.
    Uncommitted { manifest: Value, error: String },
}

/// One request of the suites.
#[derive(Debug)]
pub(super) enum SuiteJob {
    Begin {
        project_id: String,
        subject: SuiteSubject,
        now: SystemTime,
    },
    Publish(Publication),
    End {
        project_id: String,
        suite_id: String,
        status: String,
        error: Option<String>,
    },
}

impl SuiteJob {
    /// The request's name, as the fault hooks see it.
    pub(super) fn name(&self) -> &'static str {
        match self {
            SuiteJob::Begin { .. } => "BeginSuite",
            SuiteJob::Publish(_) => "PublishSuite",
            SuiteJob::End { .. } => "EndSuite",
        }
    }
}

/// What a request of the suites gave.
#[derive(Debug)]
pub(super) enum SuiteDone {
    Admitted(Admission),
    Published(Published),
    Ended(bool),
}

pub(super) fn run(
    conn: &mut Connection,
    job: SuiteJob,
    ctx: &Ctx<'_>,
) -> Result<SuiteDone, PersistError> {
    Ok(match job {
        SuiteJob::Begin {
            project_id,
            subject,
            now,
        } => SuiteDone::Admitted(begin(conn, &project_id, &subject, now, ctx)?),
        SuiteJob::Publish(publication) => SuiteDone::Published(publish(conn, &publication, ctx)?),
        SuiteJob::End {
            project_id,
            suite_id,
            status,
            error,
        } => SuiteDone::Ended(end(conn, &project_id, &suite_id, &status, error, ctx)?),
    })
}

fn event(project_id: &str, event_id: String, kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        project_id: project_id.to_owned(),
        event_id,
        run_id: None,
        kind: kind.to_owned(),
        payload,
    }
}

fn exists(tx: &Connection, event_id: &str) -> Result<bool, PersistError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

fn host_epoch(tx: &Connection, project_id: &str) -> Result<i64, PersistError> {
    tx.query_row(
        "SELECT host_epoch FROM projects WHERE project_id = ?1",
        params![project_id],
        |r| r.get(0),
    )
    .optional()
    .map_err(sqlite_error)?
    .ok_or_else(|| PersistError::IntegrityViolation(format!("no project {project_id}")))
}

/// Admit `subject`'s suite at `now` (decision 10 "准入"): `Busy` with nothing written while another identity's suite
/// is open; else, the writer slot held by the `effective` predicate (unexpired, or its holder not ended; never "the
/// row exists") → `compat.suite_deferred` once per (agent, implementation, holder, lease epoch) and `Deferred`;
/// not held → `compat.suite_started` and `Started`. Either way the suite is opened first if it is not yet.
pub(super) fn begin(
    conn: &mut Connection,
    project_id: &str,
    subject: &SuiteSubject,
    now: SystemTime,
    ctx: &Ctx<'_>,
) -> Result<Admission, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let implementation = &subject.identity.implementation_sha256;
    let open = leases::suite_open(&tx, project_id)?;
    if let Some(open) = &open
        && (open.agent != subject.agent || open.implementation_sha256 != *implementation)
    {
        // Dropped: the transaction rolls back, nothing was written.
        return Ok(Admission::Busy(open.suite_id.clone()));
    }
    let holder = leases::writer_holder(&tx, project_id, &rfc3339_utc(now))?;
    let mut events = Vec::new();
    if let Some((attempt, lease_epoch)) = &holder {
        let id = child_id(
            project_id,
            &format!(
                "compat-suite-deferred/{}/{implementation}/{attempt}/{lease_epoch}",
                subject.agent
            ),
        );
        if !exists(&tx, &id)? {
            events.push(event(
                project_id,
                id,
                SUITE_DEFERRED,
                json!({
                    "agent": subject.agent,
                    "cli_version": subject.cli_version,
                    "implementation_sha256": implementation,
                    "lease_attempt_id": attempt,
                    "lease_epoch": lease_epoch,
                    "reason": "write_lease_held",
                    "detail": DEFERRED_DETAIL,
                }),
            ));
        }
    }
    let suite_id = match &open {
        Some(open) => open.suite_id.clone(),
        None => {
            let epoch = host_epoch(&tx, project_id)?;
            let (opened, _) = leases::suite_series(project_id, epoch);
            let suite_id = crate::ids::member(&opened, super::series::count(&tx, &opened)? + 1);
            events.push(event(
                project_id,
                suite_id.clone(),
                SUITE_OPENED,
                json!({
                    "suite_id": suite_id,
                    "agent": subject.agent,
                    "implementation_sha256": implementation,
                    "host_epoch": epoch,
                }),
            ));
            suite_id
        }
    };
    if holder.is_none() && !open.as_ref().is_some_and(|o| o.started) {
        events.push(event(
            project_id,
            leases::suite_started_id(&suite_id),
            SUITE_STARTED,
            json!({"suite_id": suite_id}),
        ));
    }
    if !events.is_empty() {
        writer::write(
            &tx,
            &Commit {
                events,
                ..Commit::default()
            },
            ctx,
        )?;
    }
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(match holder {
        Some(_) => Admission::Deferred(suite_id),
        None => Admission::Started(suite_id),
    })
}

/// Publish a started suite's result (decision 10 "发布"): in one transaction, only while the suite is still open and
/// started in the current host epoch, the new manifest (the host's with this agent's entry replaced, valid against the
/// locked schema) is written to a temporary file, synced, renamed over the manifest and its directory synced; then the
/// suite's `compat.suite_ended` and `compat.suite_completed`, and the commit. The replacement happens while the suite
/// is open, so no writer's grant commits beside it. Not open → nothing written; not built or not renamed into place →
/// `unpublished`, the old manifest in place; renamed but its directory not synced → published all the same, the end
/// saying so (M2T15R-04: the file every reader sees is the new manifest, so the host must hold it).
pub(super) fn publish(
    conn: &mut Connection,
    publication: &Publication,
    ctx: &Ctx<'_>,
) -> Result<Published, PersistError> {
    let p = publication;
    let tx = writer::begin(conn, ctx)?;
    let open: OpenSuite = match leases::suite_open(&tx, &p.project_id)? {
        Some(open) if open.suite_id == p.suite_id && open.started => open,
        _ => return Ok(Published::Closed),
    };
    let written =
        crate::compat_manifest::replaced(p.base.as_ref(), &p.agent, &p.entry, &rfc3339_utc(p.now))
            .and_then(|manifest| {
                let bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
                let replaced = crate::compat_manifest::replace_at(&p.files, &bytes, &|step| {
                    ctx.manifest_at(step)
                })?;
                Ok((manifest, replaced))
            });
    let (manifest, replaced) = match written {
        Ok(written) => written,
        Err(why) => {
            drop(tx);
            end(
                conn,
                &p.project_id,
                &p.suite_id,
                UNPUBLISHED,
                Some(why.clone()),
                ctx,
            )?;
            return Ok(Published::Unpublished(why));
        }
    };
    let suite = &p.entry["suite"];
    let status = suite["status"].clone();
    let mut ended = json!({"suite_id": p.suite_id, "status": status});
    if let crate::compat_manifest::Replaced::Unsynced(why) = replaced {
        ended["error"] = Value::from(why);
    }
    let events = vec![
        event(&p.project_id, open.ended_id.clone(), SUITE_ENDED, ended),
        event(
            &p.project_id,
            child_id(&p.suite_id, "completed"),
            SUITE_COMPLETED,
            json!({
                "suite_id": p.suite_id,
                "agent": p.agent,
                "status": status,
                "implementation_sha256": p.entry["implementation_sha256"],
                "fixture_set_sha256": suite["fixture_set_sha256"],
                "cases": suite["cases"],
            }),
        ),
    ];
    // Renamed: from here a failure leaves the new manifest in place and the suite open, for the host to end.
    let committed = (move || {
        writer::write(
            &tx,
            &Commit {
                events,
                ..Commit::default()
            },
            ctx,
        )?;
        ctx.before_commit()?;
        tx.commit().map_err(sqlite_error)
    })();
    Ok(match committed {
        Ok(()) => Published::Replaced(manifest),
        Err(e) => Published::Uncommitted {
            manifest,
            error: e.to_string(),
        },
    })
}

/// End `suite_id` with `status` (and why), if it is the suite open in the project's current host epoch; false when it
/// is not (already ended, or of an older epoch).
pub(super) fn end(
    conn: &mut Connection,
    project_id: &str,
    suite_id: &str,
    status: &str,
    error: Option<String>,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let Some(open) = leases::suite_open(&tx, project_id)?.filter(|o| o.suite_id == suite_id) else {
        return Ok(false);
    };
    let mut payload = json!({"suite_id": suite_id, "status": status});
    if let Some(error) = error {
        payload["error"] = Value::from(error);
    }
    writer::write(
        &tx,
        &Commit {
            events: vec![event(project_id, open.ended_id, SUITE_ENDED, payload)],
            ..Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

#[cfg(test)]
mod tests;
