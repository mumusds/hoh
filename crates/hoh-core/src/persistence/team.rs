//! Team revisions and a new run's bindings (REQ-018, docs/adr/ADR-022.md decisions 3 and 7).
//!
//! Team revisions are the project's: revision 0 is the built-in defaults, written by the first team transaction that
//! needs it; an accepted change is APPLIED at once and is the project's current team. A run binds every role of the
//! current revision when it starts, at role epoch 1; the checkpoint carries later revisions into it.

use super::{PersistError, sqlite_error};
use crate::config::{Budget, Config, Role};
use crate::protocol::{SchedulerSettings, SchedulerWeights};
use crate::schedule::Params;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A team revision's configuration, as `team_revisions.config_json` holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TeamConfig {
    /// By role name.
    pub team: BTreeMap<String, Role>,
    /// The scheduler settings runs started under this revision snapshot (CR-007 S3); a member left out is its default.
    pub scheduler: SchedulerSettings,
    /// Where it came from: `{"kind": "builtin"}`, a command, or the configuration file and its sha256.
    pub source: serde_json::Value,
}

impl TeamConfig {
    /// Revision 0: the built-in defaults.
    pub fn builtin() -> TeamConfig {
        TeamConfig {
            team: Config::default().team,
            scheduler: SchedulerSettings::default(),
            source: serde_json::json!({"kind": "builtin"}),
        }
    }
}

/// The project's current team revision and its configuration; revision 0 is written first when there is none.
pub(super) fn current(
    tx: &Connection,
    project_id: &str,
) -> Result<(i64, TeamConfig), PersistError> {
    let row: Option<(i64, String)> = tx
        .query_row(
            "SELECT revision, config_json FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED' \
             ORDER BY revision DESC LIMIT 1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    if let Some((revision, json)) = row {
        let config = serde_json::from_str(&json).map_err(|e| {
            PersistError::IntegrityViolation(format!(
                "team revision {revision} of {project_id}: {e}"
            ))
        })?;
        return Ok((revision, config));
    }
    let config = TeamConfig::builtin();
    let json = serde_json::to_string(&config)
        .map_err(|e| PersistError::IntegrityViolation(format!("the built-in team: {e}")))?;
    let now = crate::time::now_rfc3339();
    tx.execute(
        "INSERT INTO team_revisions(project_id, revision, config_json, state, created_at, applied_at) \
         VALUES (?1, 0, ?2, 'APPLIED', ?3, ?3)",
        params![project_id, json, now],
    )
    .map_err(sqlite_error)?;
    Ok((0, config))
}

/// The project's current team revision, read only (0 before any team transaction).
pub(super) fn revision(conn: &Connection, project_id: &str) -> Result<i64, PersistError> {
    conn.query_row(
        "SELECT coalesce(max(revision), 0) FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED'",
        params![project_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// In StartRun's transaction: bind every role of the current revision to run `run_id` at epoch 1, snapshot the
/// scheduler (CR-007 S2) and write the run's budget and deadline (ADR-020 decision 7).
pub(super) fn start_run(
    tx: &Connection,
    run_id: &str,
    project_id: &str,
    budget: &Budget,
) -> Result<(), PersistError> {
    let (revision, config) = current(tx, project_id)?;
    let now = crate::time::now_rfc3339();
    for (role, binding) in &config.team {
        tx.execute(
            "INSERT INTO role_bindings(run_id, role, agent, model, role_epoch, config_revision, state, bound_at) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5, 'ACTIVE', ?6)",
            params![
                run_id,
                role,
                binding.agent.as_str(),
                binding.model,
                revision,
                now
            ],
        )
        .map_err(sqlite_error)?;
    }
    let snapshot = serde_json::json!({
        "scheduler": settings(&Params::from_settings(Some(&config.scheduler))),
    });
    tx.execute(
        "UPDATE runs SET config_revision = ?2, config_snapshot_json = ?3, deadline_at = ?4 WHERE run_id = ?1",
        params![run_id, revision, snapshot.to_string(), budget.deadline_at],
    )
    .map_err(sqlite_error)?;
    let mut limits = vec![
        ("iterations", f64::from(budget.max_iterations), true),
        ("wall_seconds", budget.max_duration_hours * 3600.0, true),
    ];
    // No price table: every model's price is unknown (CR-002), so a cost cap cannot be enforced yet.
    if let Some(usd) = budget.max_usd {
        limits.push(("usd", usd, false));
    }
    for (kind, limit, price_known) in limits {
        tx.execute(
            "INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, price_known) VALUES ('run', ?1, ?2, ?3, ?4)",
            params![run_id, kind, limit, price_known],
        )
        .map_err(sqlite_error)?;
    }
    Ok(())
}

/// A ConfigureTeam's change (decision 4): a merge patch over the team and scheduler settings for later runs.
#[derive(Debug, Clone, PartialEq)]
pub struct TeamChange {
    pub bindings: Option<serde_json::Value>,
    pub scheduler: Option<SchedulerSettings>,
    /// The project's root, against which the prompt override files the bindings name are checked
    /// (docs/adr/ADR-022.md decision 11); none: not checked.
    pub root: Option<std::path::PathBuf>,
}

/// In the command's transaction: the team revision `command` makes over the current one (decisions 3 and 4): stale
/// unless `expected_revision` is the current revision, refused unless the patched team passes the configuration's
/// rules; otherwise revision r+1, its source the command, for [`record`] to write once the command's row is.
pub(super) fn configure(
    tx: &Connection,
    command: &super::commands::SubmitCommand,
    change: &TeamChange,
) -> Result<NewRevision, PersistError> {
    let project_id = &command.event.project_id;
    let (current, config) = current(tx, project_id)?;
    if command.expected_revision != current {
        return Err(PersistError::StaleRevision {
            entity: super::writer::Entity::Team,
            id: project_id.clone(),
            expected: command.expected_revision,
            actual: Some(current),
        });
    }
    let team = match &change.bindings {
        Some(bindings) => crate::config::patch_team(&config.team, bindings).map_err(|e| {
            PersistError::InvalidTeamConfig {
                key_path: e.key.unwrap_or_else(|| "bindings".to_owned()),
                message: e.message,
                current_revision: current,
            }
        })?,
        None => config.team,
    };
    // The prompt files this change names, checked once its key and revision were (M2T07R-53).
    if let (Some(root), Some(serde_json::Value::Object(roles))) = (&change.root, &change.bindings) {
        for (name, keys) in roles {
            if let Some(prompt) = keys.get("prompt").and_then(serde_json::Value::as_str) {
                crate::config::check_prompt(root, &format!("bindings.{name}.prompt"), prompt)
                    .map_err(|e| PersistError::InvalidTeamConfig {
                        key_path: e.key.unwrap_or_default(),
                        message: e.message,
                        current_revision: current,
                    })?;
            }
        }
    }
    let mut scheduler = config.scheduler;
    if let Some(change) = &change.scheduler {
        if let Some(rounds) = change.max_wait_rounds {
            scheduler.max_wait_rounds = Some(rounds);
        }
        if let Some(weights) = &change.weights {
            let into = scheduler
                .weights
                .get_or_insert_with(SchedulerWeights::default);
            for (value, slot) in [
                (weights.user_value, &mut into.user_value),
                (weights.unblock_value, &mut into.unblock_value),
                (weights.risk_reduction, &mut into.risk_reduction),
                (weights.information_gain, &mut into.information_gain),
            ] {
                if value.is_some() {
                    *slot = value;
                }
            }
        }
    }
    let next = TeamConfig {
        team,
        scheduler,
        source: serde_json::json!({"kind": "command", "command_id": command.command_id}),
    };
    Ok(NewRevision {
        revision: current + 1,
        config_json: serde_json::to_string(&next)
            .map_err(|e| PersistError::IntegrityViolation(format!("team revision: {e}")))?,
    })
}

/// A team revision a command makes, written after the command's row (it references it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NewRevision {
    pub(super) revision: i64,
    config_json: String,
}

/// Write `revision`, applied now, made by command `command_id` of project `project_id`.
pub(super) fn record(
    tx: &Connection,
    project_id: &str,
    command_id: &str,
    revision: &NewRevision,
) -> Result<(), PersistError> {
    let now = crate::time::now_rfc3339();
    tx.execute(
        "INSERT INTO team_revisions(project_id, revision, config_json, state, command_id, created_at, applied_at) \
         VALUES (?1, ?2, ?3, 'APPLIED', ?4, ?5, ?5)",
        params![project_id, revision.revision, revision.config_json, command_id, now],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

/// The project's latest applied revision and its configuration, read only.
fn latest(conn: &Connection, project_id: &str) -> Result<Option<(i64, TeamConfig)>, PersistError> {
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT revision, config_json FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED' \
             ORDER BY revision DESC LIMIT 1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    row.map(|(revision, json)| Ok((revision, parsed(project_id, revision, &json)?)))
        .transpose()
}

/// The configuration of revision `revision`, if it is recorded.
fn at_revision(
    conn: &Connection,
    project_id: &str,
    revision: i64,
) -> Result<Option<TeamConfig>, PersistError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT config_json FROM team_revisions WHERE project_id = ?1 AND revision = ?2",
            params![project_id, revision],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    json.map(|json| parsed(project_id, revision, &json))
        .transpose()
}

fn parsed(project_id: &str, revision: i64, json: &str) -> Result<TeamConfig, PersistError> {
    serde_json::from_str(json).map_err(|e| {
        PersistError::IntegrityViolation(format!("team revision {revision} of {project_id}: {e}"))
    })
}

/// A run's binding of one role.
#[derive(Debug, Clone)]
struct Bound {
    role: String,
    agent: String,
    model: Option<String>,
    role_epoch: i64,
    config_revision: i64,
    session_id: Option<String>,
}

fn bound(conn: &Connection, run_id: &str, role: Option<&str>) -> Result<Vec<Bound>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT role, agent, model, role_epoch, config_revision, session_id \
             FROM role_bindings WHERE run_id = ?1 AND (?2 IS NULL OR role = ?2)",
        )
        .map_err(sqlite_error)?;
    statement
        .query_map(params![run_id, role], |r| {
            Ok(Bound {
                role: r.get(0)?,
                agent: r.get(1)?,
                model: r.get(2)?,
                role_epoch: r.get(3)?,
                config_revision: r.get(4)?,
                session_id: r.get(5)?,
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)
}

/// A role as the revision its binding was made at configured it.
enum Before {
    Configured(Role),
    /// That revision has no such role: it was removed at it.
    Absent,
    /// That revision is not recorded (a binding written by hand): only its agent and model are known.
    Unrecorded,
}

fn before(conn: &Connection, project_id: &str, binding: &Bound) -> Result<Before, PersistError> {
    Ok(
        match at_revision(conn, project_id, binding.config_revision)? {
            Some(config) => match config.team.get(&binding.role) {
                Some(role) => Before::Configured(role.clone()),
                None => Before::Absent,
            },
            None => Before::Unrecorded,
        },
    )
}

/// Whether revision `wanted`'s team changes `binding` (decision 8, second version): only the two configurations are
/// compared, the role in the revision the binding was made at and in `wanted` (agent, model, fallback, prompt, session
/// independence, and whether it is there at all); the binding's availability (BLOCKED_AUTH, AGENT_UNAVAILABLE) is
/// never read as configuration (M2T07R-01, -51).
fn changes(
    conn: &Connection,
    project_id: &str,
    binding: &Bound,
    wanted: &TeamConfig,
) -> Result<bool, PersistError> {
    let target = wanted.team.get(&binding.role);
    Ok(match (before(conn, project_id, binding)?, target) {
        (Before::Configured(was), Some(now)) => &was != now,
        (Before::Configured(_), None) | (Before::Absent, Some(_)) => true,
        (Before::Absent, None) => false,
        (Before::Unrecorded, Some(now)) => {
            binding.agent != now.agent.as_str() || binding.model != now.model
        }
        (Before::Unrecorded, None) => true,
    })
}

/// Why a role a revision removed is BLOCKED (`$defs.BlockedReason`).
const REMOVED: &str = "AGENT_UNAVAILABLE";

/// Whether run `run_id`'s binding of `role` waits for its checkpoint (decision 8): the project's current revision
/// changes it. Such a role is not dispatched, so no attempt is leased under the binding being replaced.
pub(super) fn pending(conn: &Connection, run_id: &str, role: &str) -> Result<bool, PersistError> {
    let project_id: Option<String> = conn
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(project_id) = project_id else {
        return Ok(false);
    };
    let Some(binding) = bound(conn, run_id, Some(role))?.pop() else {
        return Ok(false);
    };
    let Some((revision, wanted)) = latest(conn, &project_id)? else {
        return Ok(false);
    };
    if binding.config_revision >= revision {
        return Ok(false);
    }
    changes(conn, &project_id, &binding, &wanted)
}

/// Run `run_id`'s checkpoint (decision 8, second version), in one transaction. A role the project's current revision
/// does not change moves to it as it is, live attempt or not. A changed role with an attempt PREPARED, STARTING or
/// RUNNING waits; one with none takes its new binding at epoch+1 with no session yet (a removed role BLOCKED, a new role
/// bound at epoch 1) and a team.binding_applied event, unless its new agent is among `unavailable` (the registry's,
/// probed outside the writer): then the old binding and epoch stay and one team.binding_refused event is written for
/// the role and revision (REQ-019 decision 2). Once every role is at the revision, so is the run. Whether anything
/// changed.
pub(super) fn checkpoint(
    conn: &mut Connection,
    run_id: &str,
    unavailable: &std::collections::BTreeSet<String>,
    ctx: &super::writer::Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let run: Option<(String, i64)> = tx
        .query_row(
            "SELECT project_id, config_revision FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project_id, run_revision)) = run else {
        return Ok(false);
    };
    let Some((revision, wanted)) = latest(&tx, &project_id)? else {
        return Ok(false);
    };
    let live: std::collections::BTreeSet<String> = {
        let mut statement = tx
            .prepare(
                "SELECT DISTINCT t.role FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
                 WHERE t.run_id = ?1 AND a.state IN ('PREPARED', 'STARTING', 'RUNNING')",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    let now = crate::time::now_rfc3339();
    let bindings = bound(&tx, run_id, None)?;
    let mut events = Vec::new();
    let mut changed = false;
    let mut behind = false;
    let applied = |role: &str, epoch: i64, from: serde_json::Value, to: serde_json::Value| {
        super::writer::NewEvent {
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(run_id, &format!("team/{role}/{epoch}")),
            run_id: Some(run_id.to_owned()),
            kind: "team.binding_applied".to_owned(),
            payload: serde_json::json!({
                "run_id": run_id, "role": role, "config_revision": revision, "from": from, "to": to,
            }),
        }
    };
    for binding in bindings.iter().filter(|b| b.config_revision < revision) {
        if !changes(&tx, &project_id, binding, &wanted)? {
            changed = true;
            tx.execute(
                "UPDATE role_bindings SET config_revision = ?3 WHERE run_id = ?1 AND role = ?2",
                params![run_id, binding.role, revision],
            )
            .map_err(sqlite_error)?;
            continue;
        }
        if live.contains(&binding.role) {
            behind = true;
            continue;
        }
        if let Some(role) = wanted.team.get(&binding.role)
            && unavailable.contains(role.agent.as_str())
        {
            behind = true;
            let event_id =
                crate::ids::child_id(run_id, &format!("team-refused/{}/{revision}", binding.role));
            let written: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM events WHERE event_id = ?1",
                    params![event_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            if written.is_none() {
                changed = true;
                events.push(super::writer::NewEvent {
                    project_id: project_id.clone(),
                    event_id,
                    run_id: Some(run_id.to_owned()),
                    kind: "team.binding_refused".to_owned(),
                    payload: serde_json::json!({
                        "run_id": run_id, "role": binding.role, "config_revision": revision,
                        "agent": role.agent.as_str(), "reason": "AGENT_UNAVAILABLE",
                        "kept": {"agent": binding.agent, "model": binding.model, "role_epoch": binding.role_epoch},
                    }),
                });
            }
            continue;
        }
        changed = true;
        let epoch = binding.role_epoch + 1;
        let from = serde_json::json!({
            "agent": binding.agent, "model": binding.model, "role_epoch": binding.role_epoch,
            "session_id": binding.session_id,
        });
        let to = match wanted.team.get(&binding.role) {
            Some(role) => {
                tx.execute(
                    "UPDATE role_bindings SET agent = ?3, model = ?4, role_epoch = ?5, config_revision = ?6, \
                     session_id = NULL, state = 'ACTIVE', blocked_reason = NULL, missing_json = '[]', bound_at = ?7 \
                     WHERE run_id = ?1 AND role = ?2",
                    params![
                        run_id,
                        binding.role,
                        role.agent.as_str(),
                        role.model,
                        epoch,
                        revision,
                        now
                    ],
                )
                .map_err(sqlite_error)?;
                serde_json::json!({"agent": role.agent.as_str(), "model": role.model, "role_epoch": epoch})
            }
            None => {
                tx.execute(
                    "UPDATE role_bindings SET role_epoch = ?3, config_revision = ?4, session_id = NULL, \
                     state = 'BLOCKED', blocked_reason = ?5, bound_at = ?6 WHERE run_id = ?1 AND role = ?2",
                    params![run_id, binding.role, epoch, revision, REMOVED, now],
                )
                .map_err(sqlite_error)?;
                serde_json::Value::Null
            }
        };
        events.push(applied(&binding.role, epoch, from, to));
    }
    for (name, role) in &wanted.team {
        if bindings.iter().any(|b| &b.role == name) {
            continue;
        }
        changed = true;
        tx.execute(
            "INSERT INTO role_bindings(run_id, role, agent, model, role_epoch, config_revision, state, bound_at) \
             VALUES (?1, ?2, ?3, ?4, 1, ?5, 'ACTIVE', ?6)",
            params![run_id, name, role.agent.as_str(), role.model, revision, now],
        )
        .map_err(sqlite_error)?;
        let to =
            serde_json::json!({"agent": role.agent.as_str(), "model": role.model, "role_epoch": 1});
        events.push(applied(name, 1, serde_json::Value::Null, to));
    }
    if !behind && run_revision < revision {
        changed = true;
        tx.execute(
            "UPDATE runs SET config_revision = ?2 WHERE run_id = ?1",
            params![run_id, revision],
        )
        .map_err(sqlite_error)?;
    }
    if !changed {
        // Nothing to write: the transaction ends without a commit.
        return Ok(false);
    }
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events,
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// One role of a run's team, as the snapshot and the team page show it (decision 18).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamMember {
    pub role: String,
    /// `None` for a role the project's current revision adds that the run has not bound yet.
    pub agent: Option<String>,
    pub model: Option<String>,
    pub role_epoch: i64,
    pub config_revision: i64,
    pub session_id: Option<String>,
    pub state: String,
    pub blocked_reason: Option<String>,
    /// What the project's current revision changes for this role, applied at its next checkpoint (decision 8):
    /// `{"agent", "model", "config_revision"}`, `agent` null when the role is removed.
    pub pending: Option<serde_json::Value>,
    /// The role was removed from the team (its row stays, BLOCKED, so an old epoch stays fenced): not an availability
    /// block.
    pub removed: bool,
}

/// A run's team: the project's current team revision and the run's roles, in the role order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamView {
    pub team_revision: i64,
    pub members: Vec<TeamMember>,
}

/// The order roles are shown in: the built-in roles in their order, then the custom roles by name.
fn shown_order(role: &str) -> (usize, String) {
    let at = crate::config::BUILTIN_ROLES
        .iter()
        .position(|r| *r == role)
        .unwrap_or(crate::config::BUILTIN_ROLES.len());
    (at, role.to_owned())
}

/// The team of run `run_id` of project `project_id` (read only: with no revision written yet, revision 0 is the
/// built-in defaults).
pub(super) fn view(
    conn: &Connection,
    run_id: &str,
    project_id: &str,
) -> Result<TeamView, PersistError> {
    let latest: Option<(i64, String)> = conn
        .query_row(
            "SELECT revision, config_json FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED' \
             ORDER BY revision DESC LIMIT 1",
            params![project_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (team_revision, config) = match latest {
        Some((revision, json)) => (
            revision,
            serde_json::from_str::<TeamConfig>(&json).map_err(|e| {
                PersistError::IntegrityViolation(format!(
                    "team revision {revision} of {project_id}: {e}"
                ))
            })?,
        ),
        None => (0, TeamConfig::builtin()),
    };
    let mut statement = conn
        .prepare(
            "SELECT role, agent, model, role_epoch, config_revision, session_id, state, blocked_reason \
             FROM role_bindings WHERE run_id = ?1",
        )
        .map_err(sqlite_error)?;
    let mut members = statement
        .query_map(params![run_id], |r| {
            Ok(TeamMember {
                role: r.get(0)?,
                agent: Some(r.get(1)?),
                model: r.get(2)?,
                role_epoch: r.get(3)?,
                config_revision: r.get(4)?,
                session_id: r.get(5)?,
                state: r.get(6)?,
                blocked_reason: r.get(7)?,
                pending: None,
                removed: false,
            })
        })
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?;
    for member in &mut members {
        let binding = Bound {
            role: member.role.clone(),
            agent: member.agent.clone().unwrap_or_default(),
            model: member.model.clone(),
            role_epoch: member.role_epoch,
            config_revision: member.config_revision,
            session_id: member.session_id.clone(),
        };
        member.removed = matches!(before(conn, project_id, &binding)?, Before::Absent);
        if member.config_revision >= team_revision {
            continue;
        }
        let wanted = config.team.get(&member.role);
        if changes(conn, project_id, &binding, &config)? {
            member.pending = Some(serde_json::json!({
                "agent": wanted.map(|role| role.agent.as_str()),
                "model": wanted.and_then(|role| role.model.clone()),
                "config_revision": team_revision,
            }));
        }
    }
    for (name, role) in &config.team {
        if members.iter().all(|m| &m.role != name) {
            members.push(TeamMember {
                role: name.clone(),
                agent: None,
                model: None,
                role_epoch: 0,
                config_revision: team_revision,
                session_id: None,
                state: "UNBOUND".to_owned(),
                blocked_reason: None,
                removed: false,
                pending: Some(serde_json::json!({
                    "agent": role.agent.as_str(),
                    "model": role.model,
                    "config_revision": team_revision,
                })),
            });
        }
    }
    members.sort_by_key(|m| shown_order(&m.role));
    Ok(TeamView {
        team_revision,
        members,
    })
}

/// Every member of `params`, as a snapshot records them.
fn settings(params: &Params) -> SchedulerSettings {
    let narrow = |weight: u32| u8::try_from(weight).ok();
    SchedulerSettings {
        weights: Some(SchedulerWeights {
            user_value: narrow(params.weights.user_value),
            unblock_value: narrow(params.weights.unblock_value),
            risk_reduction: narrow(params.weights.risk_reduction),
            information_gain: narrow(params.weights.information_gain),
        }),
        max_wait_rounds: Some(params.max_wait_rounds),
    }
}

#[cfg(test)]
mod tests;
