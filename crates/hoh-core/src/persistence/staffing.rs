//! Staffing's store side (docs/adr/ADR-030.md decisions 1, 4 and 5): the bindings it judges, read for the runner with
//! what judging them needs, and each judgment written in one IMMEDIATE transaction, conditioned on the binding being
//! the one judged (run, role, epoch and agent) and with the role's attempts in flight read again inside it.
//!
//! The staffing events of one binding's epoch are numbered in their own series, `ids::member(ids::series(run,
//! "staffing/<role>/<epoch>"), n)` with n the series' count plus one (M2T15D-202); the latest by seq is the epoch's
//! staffing state.

use super::team::TeamConfig;
use super::writer::{self, Commit, Ctx, Intent, NewEvent, NewOutbox};
use super::{PersistError, sqlite_error};
use crate::ids::{child_id, member, series};
use crate::protocol::BlockedReason;
use crate::retry::FailureClass;
use crate::staffing::{
    AgentIdentity, BindingState, Cause, Excluded, Exclusion as Excludes, PendingChange, Session,
    Staffed,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A role left without a usable agent (decision 5): the only staffing event that wakes the Commander.
pub const BINDING_BLOCKED: &str = "team.binding_blocked";
/// A blocked binding's agent is usable again with its session.
pub const BINDING_UNBLOCKED: &str = "team.binding_unblocked";
/// A switch or a rebind waits for the role's attempt in flight.
pub const CHANGE_PENDING: &str = "team.binding_change_pending";
/// A pending change the judgment no longer makes.
pub const CHANGE_WITHDRAWN: &str = "team.binding_change_withdrawn";
/// A role bound to another agent of its order.
pub const BINDING_FALLBACK: &str = "team.binding_fallback";
/// A turn's failure excluded an agent's version and identity from a role (decision 6; written by the settle).
pub const BINDING_EXCLUDED: &str = "team.binding_excluded";
/// A turn's Capability or Model failure excluded nothing: its session has no identity (decision 6, M2T15D-103).
pub const EXCLUSION_SKIPPED: &str = "team.exclusion_skipped";
/// A task of a vacant role back to READY (decision 7).
pub const TASK_UNBLOCKED: &str = "task.unblocked";

/// The evidence object of a Block or a Switch (decision 5): what the host wrote with `objects::put` before, by its
/// digest and size; the transaction registers it as an artifact and the events name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub sha256: String,
    pub bytes: u64,
}

/// What a judgment writes (decision 5).
#[derive(Debug, Clone, PartialEq)]
pub enum StaffWrite {
    /// A session for the binding, with what its agent's fact says: BLOCKED becomes ACTIVE.
    Grant {
        agent_version: String,
        capabilities: Value,
        identity: AgentIdentity,
    },
    /// The bound agent is usable and its session fresh: ACTIVE, any change pending withdrawn.
    Unblock,
    /// The same agent and model at the next epoch with a new session; pending while an attempt is in flight.
    Rebind,
    /// No agent of the order can fill the role (or the bound one's probe failed): BLOCKED for `reason`, `missing` the
    /// lines evaluated.
    Block {
        reason: BlockedReason,
        missing: Vec<String>,
        evidence: Evidence,
    },
    /// Bind `to` with `model`, the bound agent being permanently unusable for `reason` (its verdict's line `line`);
    /// pending while an attempt is in flight.
    Switch {
        to: String,
        model: Option<String>,
        reason: BlockedReason,
        line: String,
        evaluated: Vec<String>,
        evidence: Evidence,
    },
}

/// A judgment of run `run_id`'s binding of `role` at `role_epoch` to `agent` (decision 5): written only while the
/// binding is still that one.
#[derive(Debug, Clone, PartialEq)]
pub struct StaffChange {
    pub run_id: String,
    pub role: String,
    pub role_epoch: i64,
    pub agent: String,
    pub write: StaffWrite,
}

/// One binding row as the staffing read takes it.
struct Row {
    run_id: String,
    role: String,
    agent: String,
    model: Option<String>,
    role_epoch: i64,
    config_revision: i64,
    state: String,
    blocked_reason: Option<String>,
    missing: String,
    session_id: Option<String>,
    agent_version: Option<String>,
    capabilities: Option<String>,
}

/// Every binding of every run of project `project_id` not ended, as staffing judges it (decision 1): its order (the
/// revision's agent then its fallbacks; its agent alone when the revision is not recorded; a bound agent the order
/// lacks first, decision 3), the capabilities its role's contract requires, its session with the identity it was
/// granted under (decision 4), whether the role has an attempt in flight, the exclusion records of its run, role and
/// revision (decision 6) and the change its epoch holds pending. A removed role is left out (it stays BLOCKED), and so
/// is a role whose configuration change waits for its checkpoint (ADR-022 decision 8: the checkpoint's).
pub(super) fn staffed(conn: &Connection, project_id: &str) -> Result<Vec<Staffed>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT b.run_id, b.role, b.agent, b.model, b.role_epoch, b.config_revision, b.state, b.blocked_reason, \
             b.missing_json, b.session_id, s.agent_version, s.capabilities_json FROM role_bindings b \
             JOIN runs r ON r.run_id = b.run_id LEFT JOIN sessions s ON s.session_id = b.session_id \
             WHERE r.project_id = ?1 AND r.state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE') \
             ORDER BY r.created_at, b.run_id, b.role",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<Row> = statement
        .query_map(params![project_id], |r| {
            Ok(Row {
                run_id: r.get(0)?,
                role: r.get(1)?,
                agent: r.get(2)?,
                model: r.get(3)?,
                role_epoch: r.get(4)?,
                config_revision: r.get(5)?,
                state: r.get(6)?,
                blocked_reason: r.get(7)?,
                missing: r.get(8)?,
                session_id: r.get(9)?,
                agent_version: r.get(10)?,
                capabilities: r.get(11)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut revisions: BTreeMap<i64, Option<TeamConfig>> = BTreeMap::new();
    let mut exclusions: BTreeMap<String, Vec<Exclusion>> = BTreeMap::new();
    let mut found = Vec::new();
    for row in rows {
        let config = match revisions.entry(row.config_revision) {
            std::collections::btree_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::btree_map::Entry::Vacant(new) => new.insert(
                super::team::at_revision(conn, project_id, row.config_revision)?,
            ),
        };
        let configured = match config {
            // The revision has no such role: it was removed at it, and stays BLOCKED.
            Some(config) => match config.team.get(&row.role) {
                Some(role) => Some(role.clone()),
                None => continue,
            },
            None => None,
        };
        if super::team::pending(conn, &row.run_id, &row.role)? {
            continue;
        }
        let builtin = crate::config::BUILTIN_ROLES
            .contains(&row.role.as_str())
            .then(|| row.role.clone());
        let (preferred, preferred_model, mut order, contract) = match &configured {
            Some(role) => (
                role.agent.as_str().to_owned(),
                role.model.clone(),
                std::iter::once(&role.agent)
                    .chain(&role.fallback)
                    .map(|a| a.as_str().to_owned())
                    .collect::<Vec<_>>(),
                builtin.or_else(|| {
                    role.prompt
                        .as_deref()
                        .and_then(|p| p.strip_prefix(crate::config::BUILTIN_PROMPT))
                        .map(str::to_owned)
                }),
            ),
            None => (
                row.agent.clone(),
                row.model.clone(),
                vec![row.agent.clone()],
                builtin,
            ),
        };
        if !order.contains(&row.agent) {
            order.insert(0, row.agent.clone());
        }
        let session = match (&row.session_id, row.agent_version, row.capabilities) {
            (Some(id), Some(agent_version), Some(capabilities)) => Some(Session {
                agent_version,
                capabilities: serde_json::from_str(&capabilities).unwrap_or(Value::Null),
                identity: super::team::session_identity(conn, id)?,
            }),
            _ => None,
        };
        let of_run = match exclusions.entry(row.run_id.clone()) {
            std::collections::btree_map::Entry::Occupied(known) => known.into_mut(),
            std::collections::btree_map::Entry::Vacant(new) => {
                new.insert(exclusions_of(conn, &row.run_id)?)
            }
        };
        let excluded = of_run
            .iter()
            .filter(|e| e.role == row.role && e.config_revision == row.config_revision)
            .map(|e| e.record.clone())
            .collect();
        found.push(Staffed {
            live: live(conn, &row.run_id, &row.role)?,
            pending: pending_change(conn, &row.run_id, &row.role, row.role_epoch)?
                .map(|(change, _)| change),
            run_id: row.run_id,
            role: row.role,
            role_epoch: row.role_epoch,
            agent: row.agent,
            model: row.model,
            preferred,
            preferred_model,
            state: if row.state == "BLOCKED" {
                BindingState::Blocked
            } else {
                BindingState::Active
            },
            blocked_reason: row.blocked_reason.as_deref().and_then(BlockedReason::parse),
            missing: serde_json::from_str(&row.missing).unwrap_or_default(),
            session,
            order,
            required: crate::staffing::required(contract.as_deref()),
            excluded,
        });
    }
    Ok(found)
}

/// One `team.binding_excluded` record of a run, with the role and revision it is for.
struct Exclusion {
    role: String,
    config_revision: i64,
    record: Excluded,
}

/// The exclusion records of run `run_id` (decision 6), read by the run and the kind; one without a whole identity or a
/// known reason is no record.
fn exclusions_of(conn: &Connection, run_id: &str) -> Result<Vec<Exclusion>, PersistError> {
    let mut statement = conn
        .prepare("SELECT payload_json FROM events WHERE run_id = ?1 AND kind = ?2 ORDER BY seq")
        .map_err(sqlite_error)?;
    let payloads: Vec<String> = statement
        .query_map(params![run_id, BINDING_EXCLUDED], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(payloads
        .iter()
        .filter_map(|payload| {
            let payload: Value = serde_json::from_str(payload).ok()?;
            Some(Exclusion {
                role: payload["role"].as_str()?.to_owned(),
                config_revision: payload["config_revision"].as_i64()?,
                record: Excluded {
                    agent: payload["agent"].as_str()?.to_owned(),
                    agent_version: payload["agent_version"].as_str()?.to_owned(),
                    identity: AgentIdentity::from_json(&payload["identity"])?,
                    reason: serde_json::from_value(payload["reason"].clone()).ok()?,
                },
            })
        })
        .collect())
}

/// Whether run `run_id`'s `role` has an attempt in flight: PREPARED, STARTING or RUNNING, a supervision turn's too (the
/// checkpoint's notion, ADR-022 decision 8).
fn live(conn: &Connection, run_id: &str, role: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = ?1 \
         AND t.role = ?2 AND a.state IN ('PREPARED', 'STARTING', 'RUNNING'))",
        params![run_id, role],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The stem of the staffing series of run `run_id`'s `role` at `role_epoch` (decision 5, M2T15D-202).
fn stem(run_id: &str, role: &str, role_epoch: i64) -> String {
    series(run_id, &format!("staffing/{role}/{role_epoch}"))
}

/// The change run `run_id`'s `role` holds pending at `role_epoch` (decision 5): the epoch's latest staffing event, when
/// it is a `team.binding_change_pending`, with the model it names.
pub(super) fn pending_change(
    conn: &Connection,
    run_id: &str,
    role: &str,
    role_epoch: i64,
) -> Result<Option<(PendingChange, Option<String>)>, PersistError> {
    let latest = super::series::latest(conn, &stem(run_id, role, role_epoch))?;
    Ok(latest.and_then(|(kind, payload)| pending_of(&kind, &payload)))
}

/// The pending change a staffing event is, if it is a `team.binding_change_pending`.
fn pending_of(kind: &str, payload: &Value) -> Option<(PendingChange, Option<String>)> {
    if kind != CHANGE_PENDING {
        return None;
    }
    Some((
        PendingChange {
            cause: serde_json::from_value(payload["cause"].clone()).ok()?,
            to: payload["to"].as_str()?.to_owned(),
        },
        payload["model"].as_str().map(str::to_owned),
    ))
}

/// The binding a judgment was made for, as it is inside the write's transaction.
struct Judged {
    project_id: String,
    model: Option<String>,
    config_revision: i64,
    blocked: Option<String>,
    missing: Vec<String>,
    session_id: Option<String>,
}

/// The events and notices one staffing write commits, its staffing events numbered on in the epoch's series.
struct Writes<'a> {
    change: &'a StaffChange,
    project_id: String,
    stem: String,
    next: u64,
    events: Vec<NewEvent>,
    outbox: Vec<NewOutbox>,
}

impl Writes<'_> {
    /// The next staffing event of the epoch: `kind` with `payload`'s fields after the binding's own.
    fn staffing(&mut self, kind: &str, payload: Value) {
        let mut body = json!({
            "run_id": self.change.run_id, "role": self.change.role, "role_epoch": self.change.role_epoch,
            "agent": self.change.agent,
        });
        if let (Some(body), Value::Object(more)) = (body.as_object_mut(), payload) {
            body.extend(more);
        }
        self.events.push(NewEvent {
            intent: Intent::Progress,
            project_id: self.project_id.clone(),
            event_id: member(&self.stem, self.next),
            run_id: Some(self.change.run_id.clone()),
            kind: kind.to_owned(),
            payload: body,
        });
        self.next += 1;
    }

    /// Another event of the write.
    fn event(&mut self, event_id: String, kind: &str, payload: Value) {
        self.events.push(NewEvent {
            intent: Intent::Progress,
            project_id: self.project_id.clone(),
            event_id,
            run_id: Some(self.change.run_id.clone()),
            kind: kind.to_owned(),
            payload,
        });
    }
}

/// The artifact id `evidence` is registered under, registered now in the write's transaction.
fn register(tx: &Transaction<'_>, evidence: &Evidence) -> Result<String, PersistError> {
    let row = super::runner::ArtifactRow {
        path: crate::objects::relative_path(&evidence.sha256),
        sha256: evidence.sha256.clone(),
        bytes: evidence.bytes,
        mime: "application/json".to_owned(),
        kind: "staffing_probe".to_owned(),
        name: "probe.json".to_owned(),
    };
    super::runner::register(tx, std::slice::from_ref(&row))?;
    Ok(super::runner::artifact_id(&row.sha256, &row.path))
}

/// The fields of an event naming `evidence` (registered as `artifact_id`).
fn probe(artifact_id: &str, evidence: &Evidence) -> Value {
    json!({"probe_artifact_id": artifact_id, "probe_sha256": evidence.sha256})
}

/// Merge the fields of `more` into `value`.
fn with(mut value: Value, more: Value) -> Value {
    if let (Some(into), Value::Object(more)) = (value.as_object_mut(), more) {
        into.extend(more);
    }
    value
}

/// Write `change` in one transaction (decision 5); whether anything was written. Nothing is written unless the
/// binding is still at the change's epoch and agent; the role's attempts in flight are read inside the transaction. A
/// Grant is `team::bind_session`'s transaction.
pub(super) fn staff(
    conn: &mut Connection,
    change: &StaffChange,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    if let StaffWrite::Grant {
        agent_version,
        capabilities,
        identity,
    } = &change.write
    {
        return super::team::bind_session(
            conn,
            &super::team::SessionGrant {
                run_id: change.run_id.clone(),
                role: change.role.clone(),
                role_epoch: change.role_epoch,
                agent: change.agent.clone(),
                agent_version: agent_version.clone(),
                capabilities: capabilities.clone(),
                identity: identity.clone(),
            },
            ctx,
        );
    }
    let tx = writer::begin(conn, ctx)?;
    let judged: Option<Judged> = tx
        .query_row(
            "SELECT r.project_id, b.model, b.config_revision, b.state, b.blocked_reason, b.missing_json, b.session_id \
             FROM role_bindings b JOIN runs r ON r.run_id = b.run_id \
             WHERE b.run_id = ?1 AND b.role = ?2 AND b.role_epoch = ?3 AND b.agent = ?4",
            params![change.run_id, change.role, change.role_epoch, change.agent],
            |r| {
                let state: String = r.get(3)?;
                Ok(Judged {
                    project_id: r.get(0)?,
                    model: r.get(1)?,
                    config_revision: r.get(2)?,
                    blocked: if state == "BLOCKED" { r.get(4)? } else { None },
                    missing: serde_json::from_str(&r.get::<_, String>(5)?).unwrap_or_default(),
                    session_id: r.get(6)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(judged) = judged else {
        return Ok(false);
    };
    let live = live(&tx, &change.run_id, &change.role)?;
    let stem = stem(&change.run_id, &change.role, change.role_epoch);
    let pending =
        super::series::latest(&tx, &stem)?.and_then(|(kind, payload)| pending_of(&kind, &payload));
    let mut writes = Writes {
        change,
        project_id: judged.project_id.clone(),
        next: super::series::count(&tx, &stem)? + 1,
        stem,
        events: Vec::new(),
        outbox: Vec::new(),
    };
    let withdrawn = |writes: &mut Writes<'_>| {
        if let Some((pending, _)) = &pending {
            writes.staffing(
                CHANGE_WITHDRAWN,
                json!({"cause": pending.cause.as_str(), "to": pending.to}),
            );
        }
    };
    let set = |tx: &Transaction<'_>, state: &str, reason: Option<&str>, missing: &[String]| {
        tx.execute(
            "UPDATE role_bindings SET state = ?4, blocked_reason = ?5, missing_json = ?6 \
             WHERE run_id = ?1 AND role = ?2 AND role_epoch = ?3",
            params![
                change.run_id,
                change.role,
                change.role_epoch,
                state,
                reason,
                json!(missing).to_string()
            ],
        )
        .map_err(sqlite_error)
    };
    match &change.write {
        StaffWrite::Grant { .. } => {}
        StaffWrite::Unblock => {
            if let Some(reason) = &judged.blocked {
                set(&tx, "ACTIVE", None, &[])?;
                writes.staffing(BINDING_UNBLOCKED, json!({"reason": reason}));
            }
            withdrawn(&mut writes);
        }
        StaffWrite::Block {
            reason,
            missing,
            evidence,
        } => {
            // Written on entering BLOCKED or when its reason or missing lines change.
            if judged.blocked.as_deref() != Some(reason.as_str()) || &judged.missing != missing {
                let artifact = register(&tx, evidence)?;
                set(&tx, "BLOCKED", Some(reason.as_str()), missing)?;
                writes.staffing(
                    BINDING_BLOCKED,
                    with(
                        json!({"reason": reason.as_str(), "missing": missing, "evaluated": missing,
                               "cause": "staffing"}),
                        probe(&artifact, evidence),
                    ),
                );
            }
            // A change pending is no longer the judgment.
            withdrawn(&mut writes);
        }
        StaffWrite::Switch {
            to,
            model,
            reason,
            line,
            evaluated,
            evidence,
        } => {
            let artifact = register(&tx, evidence)?;
            // The bound agent's loss, once: not when the binding is BLOCKED for that reason already.
            if judged.blocked.as_deref() != Some(reason.as_str()) {
                if live {
                    set(
                        &tx,
                        "BLOCKED",
                        Some(reason.as_str()),
                        std::slice::from_ref(line),
                    )?;
                }
                writes.staffing(
                    BINDING_BLOCKED,
                    with(
                        json!({"reason": reason.as_str(), "missing": [line], "evaluated": evaluated,
                               "cause": "staffing"}),
                        probe(&artifact, evidence),
                    ),
                );
            }
            if live {
                let same = pending
                    .as_ref()
                    .is_some_and(|(p, _)| p.cause == Cause::Fallback && &p.to == to);
                if !same {
                    writes.staffing(
                        CHANGE_PENDING,
                        with(
                            json!({"cause": Cause::Fallback.as_str(), "to": to, "model": model,
                                   "evaluated": evaluated}),
                            probe(&artifact, evidence),
                        ),
                    );
                }
            } else {
                apply(
                    &tx,
                    &mut writes,
                    &judged,
                    to,
                    model.as_ref(),
                    Cause::Fallback,
                )?;
                let epoch = change.role_epoch + 1;
                writes.event(
                    child_id(&change.run_id, &format!("fallback/{}/{epoch}", change.role)),
                    BINDING_FALLBACK,
                    with(
                        json!({"run_id": change.run_id, "role": change.role, "from": change.agent, "to": to,
                               "evaluated": evaluated, "chosen": to}),
                        probe(&artifact, evidence),
                    ),
                );
                handover(&tx, &mut writes, &judged, to, model.as_ref())?;
            }
        }
        StaffWrite::Rebind => {
            if live {
                let same = pending
                    .as_ref()
                    .is_some_and(|(p, _)| p.cause == Cause::Rebind && p.to == change.agent);
                if !same {
                    writes.staffing(
                        CHANGE_PENDING,
                        json!({"cause": Cause::Rebind.as_str(), "to": change.agent, "model": judged.model}),
                    );
                }
            } else {
                let model = judged.model.clone();
                apply(
                    &tx,
                    &mut writes,
                    &judged,
                    &change.agent,
                    model.as_ref(),
                    Cause::Rebind,
                )?;
                handover(&tx, &mut writes, &judged, &change.agent, model.as_ref())?;
            }
        }
    }
    if writes.events.is_empty() {
        // Nothing to write: the transaction ends without a commit.
        return Ok(false);
    }
    writer::write(
        &tx,
        &Commit {
            events: writes.events,
            outbox: writes.outbox,
            ..Commit::default()
        },
        ctx,
    )?;
    super::writer::commit(tx, ctx)?;
    Ok(true)
}

/// Verifier `role` of run `run_id` moved to its next epoch's session for a SESSION_NOT_INDEPENDENT answer about round
/// `round_id` (docs/adr/ADR-031.md decision 8, M2T13D-03): the staffing's Rebind (`apply` with `Cause::Rebind` — the
/// same agent and model, no session, ACTIVE, `team.binding_applied{cause: rebind}` — and the epoch's handover), whose
/// session `team::session_id(run, role, epoch + 1)` staffing grants next. What to commit, in the answer's transaction;
/// `Err(detail)` when a precondition does not hold: the role has an attempt in flight in the run (staffing would only
/// hold the change pending, and an answer does not queue), or the new session is one the round's admission refuses.
pub(super) fn rebind(
    tx: &Transaction<'_>,
    run_id: &str,
    round_id: &str,
    role: &str,
) -> Result<Result<Commit, String>, PersistError> {
    let row: Option<(String, i64, Judged)> = tx
        .query_row(
            "SELECT b.agent, b.role_epoch, r.project_id, b.model, b.config_revision, b.state, b.blocked_reason, \
             b.missing_json, b.session_id FROM role_bindings b JOIN runs r ON r.run_id = b.run_id \
             WHERE b.run_id = ?1 AND b.role = ?2",
            params![run_id, role],
            |r| {
                let state: String = r.get(5)?;
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    Judged {
                        project_id: r.get(2)?,
                        model: r.get(3)?,
                        config_revision: r.get(4)?,
                        blocked: if state == "BLOCKED" { r.get(6)? } else { None },
                        missing: serde_json::from_str(&r.get::<_, String>(7)?).unwrap_or_default(),
                        session_id: r.get(8)?,
                    },
                ))
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((agent, role_epoch, judged)) = row else {
        return Ok(Err(format!("run {run_id} has no {role} bound")));
    };
    if live(tx, run_id, role)? {
        return Ok(Err(format!(
            "the {role} has an attempt in flight in run {run_id}: its binding cannot move now"
        )));
    }
    let next = super::team::session_id(run_id, role, role_epoch + 1);
    if super::verification::conflicting_sessions(tx, run_id, round_id, role)?.contains(&next) {
        return Ok(Err(format!(
            "the {role}'s next session {next} is one round {round_id} may not use"
        )));
    }
    let change = StaffChange {
        run_id: run_id.to_owned(),
        role: role.to_owned(),
        role_epoch,
        agent: agent.clone(),
        write: StaffWrite::Rebind,
    };
    let stem = stem(run_id, role, role_epoch);
    let mut writes = Writes {
        change: &change,
        project_id: judged.project_id.clone(),
        next: super::series::count(tx, &stem)? + 1,
        stem,
        events: Vec::new(),
        outbox: Vec::new(),
    };
    let model = judged.model.clone();
    apply(
        tx,
        &mut writes,
        &judged,
        &agent,
        model.as_ref(),
        Cause::Rebind,
    )?;
    handover(tx, &mut writes, &judged, &agent, model.as_ref())?;
    Ok(Ok(Commit {
        events: writes.events,
        outbox: writes.outbox,
        ..Commit::default()
    }))
}

/// What a settle reads of a failed attempt: its task's role, its epoch, agent and session, and its configuration
/// revision.
struct TurnRow {
    role: String,
    role_epoch: i64,
    agent: Option<String>,
    session_id: Option<String>,
    config_revision: i64,
}

/// What a turn's failure of `class` writes for its binding (decision 6), in the transaction that settles its attempt
/// `attempt_id` of task `task_id` (the runner's settle or a reconcile, the same input: both read it here). For the
/// Auth, Capability and Model classes, a supervision turn's too (M2T15D-05): the binding of the task's role at the
/// attempt's epoch to the attempt's agent BLOCKED for the class's reason, missing `["<agent>:<CODE>"]`, and its
/// `team.binding_blocked{cause: "turn", attempt_id}` in the epoch's staffing series (on entering BLOCKED or when its
/// reason or missing changes); a binding that is no longer that one is left alone. Capability and Model also write
/// the exclusion record `team.binding_excluded`, its version and identity the attempt's session's (`sessions` and its
/// `team.session_bound`, never the runner's memory, M2T15D-103), once per attempt; a session without an identity writes
/// `team.exclusion_skipped` instead. The binding's columns are written here; the events are for the caller's commit.
pub(super) fn turn_failed(
    tx: &Connection,
    project_id: &str,
    run_id: &str,
    attempt_id: &str,
    class: FailureClass,
) -> Result<Vec<NewEvent>, PersistError> {
    let (reason, excludes) = match class {
        FailureClass::Auth => (BlockedReason::BlockedAuth, None),
        FailureClass::Capability => (
            BlockedReason::BlockedCapability,
            Some(Excludes::BlockedCapability),
        ),
        FailureClass::Model => (
            BlockedReason::ModelUnavailable,
            Some(Excludes::ModelUnavailable),
        ),
        _ => return Ok(Vec::new()),
    };
    let attempt: Option<TurnRow> = tx
        .query_row(
            "SELECT t.role, a.role_epoch, a.agent, a.session_id, a.config_revision FROM attempts a \
             JOIN tasks t ON t.task_id = a.task_id WHERE a.attempt_id = ?1",
            params![attempt_id],
            |r| {
                Ok(TurnRow {
                    role: r.get(0)?,
                    role_epoch: r.get(1)?,
                    agent: r.get(2)?,
                    session_id: r.get(3)?,
                    config_revision: r.get(4)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(TurnRow {
        role,
        role_epoch,
        agent: Some(agent),
        session_id,
        config_revision,
    }) = attempt
    else {
        return Ok(Vec::new());
    };
    let event = |event_id: String, kind: &str, payload: Value| NewEvent {
        intent: Intent::Progress,
        project_id: project_id.to_owned(),
        event_id,
        run_id: Some(run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let mut events = Vec::new();
    let missing = vec![format!("{agent}:{}", reason.as_str())];
    let bound: Option<(String, Option<String>, String)> = tx
        .query_row(
            "SELECT state, blocked_reason, missing_json FROM role_bindings \
             WHERE run_id = ?1 AND role = ?2 AND role_epoch = ?3 AND agent = ?4",
            params![run_id, role, role_epoch, agent],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((state, blocked, held)) = bound {
        let blocked = (state == "BLOCKED").then_some(blocked).flatten();
        let held: Vec<String> = serde_json::from_str(&held).unwrap_or_default();
        if blocked.as_deref() != Some(reason.as_str()) || held != missing {
            tx.execute(
                "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = ?5, missing_json = ?6 \
                 WHERE run_id = ?1 AND role = ?2 AND role_epoch = ?3 AND agent = ?4",
                params![
                    run_id,
                    role,
                    role_epoch,
                    agent,
                    reason.as_str(),
                    json!(missing).to_string()
                ],
            )
            .map_err(sqlite_error)?;
            let stem = stem(run_id, &role, role_epoch);
            let n = super::series::count(tx, &stem)? + 1;
            events.push(event(
                member(&stem, n),
                BINDING_BLOCKED,
                json!({"run_id": run_id, "role": role, "role_epoch": role_epoch, "agent": agent,
                       "reason": reason.as_str(), "missing": missing, "evaluated": missing, "cause": "turn",
                       "attempt_id": attempt_id}),
            ));
        }
    }
    let Some(excludes) = excludes else {
        return Ok(events);
    };
    let exists = |id: &str| -> Result<bool, PersistError> {
        tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
            params![id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)
    };
    let (excluded, skipped) = (
        child_id(attempt_id, "excluded"),
        child_id(attempt_id, "exclusion_skipped"),
    );
    if exists(&excluded)? || exists(&skipped)? {
        return Ok(events);
    }
    let session = match &session_id {
        Some(id) => {
            let version: Option<String> = tx
                .query_row(
                    "SELECT agent_version FROM sessions WHERE session_id = ?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            version.zip(super::team::session_identity(tx, id)?)
        }
        None => None,
    };
    events.push(match session {
        Some((agent_version, identity)) => event(
            excluded,
            BINDING_EXCLUDED,
            json!({"run_id": run_id, "role": role, "config_revision": config_revision, "agent": agent,
                   "agent_version": agent_version, "identity": identity, "reason": excludes,
                   "attempt_id": attempt_id}),
        ),
        None => event(
            skipped,
            EXCLUSION_SKIPPED,
            json!({"attempt_id": attempt_id, "session_id": session_id,
                   "reason": "the session has no identity"}),
        ),
    });
    Ok(events)
}

/// The reason and missing lines of run `run_id`'s binding of `role` while it is BLOCKED for a reason of the vacancy set
/// V (decision 7); none otherwise.
pub(super) fn vacancy(
    tx: &Connection,
    run_id: &str,
    role: &str,
) -> Result<Option<(BlockedReason, Vec<String>)>, PersistError> {
    let row: Option<(Option<String>, String)> = tx
        .query_row(
            "SELECT blocked_reason, missing_json FROM role_bindings \
             WHERE run_id = ?1 AND role = ?2 AND state = 'BLOCKED'",
            params![run_id, role],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(row.and_then(|(reason, missing)| {
        let reason = reason
            .as_deref()
            .and_then(BlockedReason::parse)
            .filter(|r| crate::staffing::VACANCY.contains(r))?;
        Some((reason, serde_json::from_str(&missing).unwrap_or_default()))
    }))
}

/// Whether run `run_id`'s `role` is not staffed yet and staffing waits for a candidate (docs/adr/ADR-030.md decision 4's
/// start-up row, the note of 2026-11-17 under decision 11): its binding ACTIVE and never granted a session, its agent
/// permanently unusable in the tick's snapshot and a later candidate of its order waiting there (not looked at yet, its
/// compat pending, its probe failed), so staffing writes nothing until that candidate is judged. What the registry saw
/// of the bound agent is then not yet the role's. Never without the tick's snapshot (a host with no registry).
pub(super) fn waits_to_staff(
    conn: &Connection,
    run_id: &str,
    role: &str,
    context: &crate::decision::Context,
) -> Result<bool, PersistError> {
    use crate::staffing::{Kind, select, verdict};
    let Some(snapshot) = &context.staffing else {
        return Ok(false);
    };
    let Some(project_id): Option<String> = conn
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
    else {
        return Ok(false);
    };
    let Some(binding) = staffed(conn, &project_id)?
        .into_iter()
        .find(|s| s.run_id == run_id && s.role == role)
    else {
        return Ok(false);
    };
    let facts = &snapshot.facts;
    Ok(binding.state == BindingState::Active
        && binding.session.is_none()
        && verdict(&binding.agent, &binding.required, facts, &binding.excluded).kind()
            == Kind::Permanent
        && select(&binding.order, &binding.required, facts, &binding.excluded)
            .waiting_on
            .is_some())
}

/// Whether a task of run `run_id`'s `role` blocked for `reason` (of V) is dispatchable again (decision 7): the binding
/// active as `decide::load_binding` says (ACTIVE, its session, nothing pending, its first input delivered), its session
/// granting what the role requires, and, with the tick's snapshot, the bound agent's fact usable and verified with the
/// session fresh for it (an agent the snapshot lacks is not verified, M2T15D-51); without one (a host with no
/// registry, and so no gate), never for COMPAT_UNVERIFIED. The binding's epoch when it is.
pub(super) fn dispatchable(
    tx: &Transaction<'_>,
    run_id: &str,
    role: &str,
    context: &crate::decision::Context,
    reason: BlockedReason,
) -> Result<Option<i64>, PersistError> {
    use crate::staffing::{Compat, Fact};
    let Some(binding) = super::decide::load_binding(tx, run_id, role)? else {
        return Ok(None);
    };
    if !binding.active
        || context
            .required_capabilities
            .get(role)
            .is_some_and(|required| !required.is_subset(&binding.capabilities))
    {
        return Ok(None);
    }
    let epoch = i64::try_from(binding.role_epoch).map_err(|_| {
        PersistError::IntegrityViolation(format!("role epoch {}", binding.role_epoch))
    })?;
    let Some(snapshot) = &context.staffing else {
        return Ok((reason != BlockedReason::CompatUnverified).then_some(epoch));
    };
    let bound: Option<(String, String, String, String)> = tx
        .query_row(
            "SELECT b.agent, b.session_id, s.agent_version, s.capabilities_json FROM role_bindings b \
             JOIN sessions s ON s.session_id = b.session_id WHERE b.run_id = ?1 AND b.role = ?2",
            params![run_id, role],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((agent, session_id, agent_version, capabilities)) = bound else {
        return Ok(None);
    };
    let Some(fact @ Fact::Usable(usable)) = snapshot.facts.get(&agent) else {
        return Ok(None);
    };
    let session = Session {
        agent_version,
        capabilities: serde_json::from_str(&capabilities).unwrap_or(Value::Null),
        identity: super::team::session_identity(tx, &session_id)?,
    };
    Ok(
        (usable.compat == Compat::Verified && crate::staffing::fresh(&session, fact))
            .then_some(epoch),
    )
}

/// The binding as `team.binding_applied` records it on either side of a change.
fn sides(
    judged: &Judged,
    change: &StaffChange,
    to: &str,
    model: Option<&String>,
) -> (Value, Value) {
    (
        json!({"agent": change.agent, "model": judged.model, "role_epoch": change.role_epoch,
               "session_id": judged.session_id}),
        json!({"agent": to, "model": model, "role_epoch": change.role_epoch + 1}),
    )
}

/// The binding at its next epoch bound to `to` with `model`, no session and ACTIVE, its config_revision unchanged (a
/// fallback or a rebind is no configuration change, decision 5), with `team.binding_applied{cause}` (the checkpoint's
/// id format: one per epoch).
fn apply(
    tx: &Transaction<'_>,
    writes: &mut Writes<'_>,
    judged: &Judged,
    to: &str,
    model: Option<&String>,
    cause: Cause,
) -> Result<(), PersistError> {
    let change = writes.change;
    let epoch = change.role_epoch + 1;
    tx.execute(
        "UPDATE role_bindings SET agent = ?3, model = ?4, role_epoch = ?5, session_id = NULL, state = 'ACTIVE', \
         blocked_reason = NULL, missing_json = '[]', bound_at = ?6 WHERE run_id = ?1 AND role = ?2",
        params![
            change.run_id,
            change.role,
            to,
            model,
            epoch,
            crate::time::now_rfc3339()
        ],
    )
    .map_err(sqlite_error)?;
    let (from, onto) = sides(judged, change, to, model);
    writes.event(
        child_id(&change.run_id, &format!("team/{}/{epoch}", change.role)),
        "team.binding_applied",
        json!({"run_id": change.run_id, "role": change.role, "config_revision": judged.config_revision,
               "cause": cause.as_str(), "from": from, "to": onto}),
    );
    Ok(())
}

/// The handover of the role to its binding's next epoch (ADR-024 decisions 1 and 2, as the checkpoint's): the
/// handoff.prepared event and the session.start notice with the package.
fn handover(
    tx: &Transaction<'_>,
    writes: &mut Writes<'_>,
    judged: &Judged,
    to: &str,
    model: Option<&String>,
) -> Result<(), PersistError> {
    let change = writes.change;
    let (from, onto) = sides(judged, change, to, model);
    let spec = super::handover::spec(tx, &change.run_id)?;
    let (event, mut notice) = super::handover::prepare(
        tx,
        &writes.project_id,
        &change.run_id,
        &change.role,
        change.role_epoch + 1,
        spec,
        &from,
        &onto,
    )?;
    writes.events.push(event);
    notice.event = writes.events.len() - 1;
    writes.outbox.push(notice);
    Ok(())
}

#[cfg(test)]
mod tests;
