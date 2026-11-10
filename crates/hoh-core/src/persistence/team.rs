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
    /// The project's configuration file as a whole (decision 6): its team and scheduler replace the current ones.
    pub file: Option<FileTeam>,
}

/// The team and scheduler settings of the project's configuration file, or the built-in defaults when there is none
/// (decision 6), already checked by the loader; `source` is `{"kind": "file", "path", "sha256"}` or
/// `{"kind": "builtin"}`.
#[derive(Debug, Clone, PartialEq)]
pub struct FileTeam {
    pub team: BTreeMap<String, Role>,
    pub scheduler: Params,
    pub source: serde_json::Value,
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
    if let Some(file) = &change.file {
        let next = TeamConfig {
            team: file.team.clone(),
            scheduler: settings(&file.scheduler),
            source: file.source.clone(),
        };
        return Ok(NewRevision {
            revision: current + 1,
            config_json: serde_json::to_string(&next)
                .map_err(|e| PersistError::IntegrityViolation(format!("team revision: {e}")))?,
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

/// The source of the project's latest revision made from its configuration file or the built-in defaults (decision 6);
/// none before any revision.
pub(super) fn file_source(
    conn: &Connection,
    project_id: &str,
) -> Result<Option<serde_json::Value>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT revision, config_json FROM team_revisions WHERE project_id = ?1 AND state = 'APPLIED' \
             ORDER BY revision DESC",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(i64, String)> = statement
        .query_map(params![project_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    for (revision, json) in rows {
        let source = parsed(project_id, revision, &json)?.source;
        if matches!(source["kind"].as_str(), Some("file" | "builtin")) {
            return Ok(Some(source));
        }
    }
    Ok(None)
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

/// Why a removed role, or a role whose agent is unavailable, is BLOCKED (`$defs.BlockedReason` has no other code for a
/// removed role; which one it is comes from the configuration, never from this code, decision 8).
const AGENT_UNAVAILABLE: &str = "AGENT_UNAVAILABLE";

/// Why a change to an agent not logged in is refused (`$defs.BlockedReason`).
const BLOCKED_AUTH: &str = "BLOCKED_AUTH";
/// What a refusal for a run without a specification says (docs/adr/ADR-024.md decision 10).
const NO_SPEC_DETAIL: &str = "the run has no specification yet";

/// The event kinds of a checkpoint's verdict on a changed role (decision 8, third version): the change is refused (its
/// new agent unavailable: the old binding works on), or pending again (the agent is there: the role is held until its
/// checkpoint).
const REFUSED: &str = "team.binding_refused";
const PENDING_AGAIN: &str = "team.binding_pending";

/// How many verdicts the checkpoints wrote on run `run_id`'s change of `role` at `revision`, and whether the last one
/// refuses it.
fn verdicts(
    conn: &Connection,
    run_id: &str,
    role: &str,
    revision: i64,
) -> Result<(i64, Option<(String, String)>), PersistError> {
    let filter = "run_id = ?1 AND kind IN (?4, ?5) AND json_extract(payload_json, '$.role') = ?2 \
                  AND json_extract(payload_json, '$.config_revision') = ?3";
    let written: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM events WHERE {filter}"),
            params![run_id, role, revision, REFUSED, PENDING_AGAIN],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    // The last verdict, when it is a refusal: its reason and detail (a new one is written when they change).
    let last: Option<(String, Option<String>, Option<String>)> = conn
        .query_row(
            &format!(
                "SELECT kind, json_extract(payload_json, '$.reason'), json_extract(payload_json, '$.detail') \
                 FROM events WHERE {filter} ORDER BY seq DESC LIMIT 1"
            ),
            params![run_id, role, revision, REFUSED, PENDING_AGAIN],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let refused = last
        .filter(|(kind, _, _)| kind == REFUSED)
        .map(|(_, reason, detail)| (reason.unwrap_or_default(), detail.unwrap_or_default()));
    Ok((written, refused))
}

/// Whether run `run_id`'s binding of `role` waits for its checkpoint (decision 8): the project's current revision
/// changes it and that change is not refused. Such a role is not dispatched, so no attempt is leased under the binding
/// being replaced; a refused change leaves the old binding working, so the role is never vacant (M2T07R-150).
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
    if binding.config_revision >= revision || !changes(conn, &project_id, &binding, &wanted)? {
        return Ok(false);
    }
    Ok(verdicts(conn, run_id, role, revision)?.1.is_none())
}

/// What the host's registry knows of the agents, for a checkpoint (decisions 8 and 10): the agents it found available,
/// and those it found unavailable with why; an agent in neither was not looked at yet (whatever the checkpoint's own
/// read of the team names, M2T07R-255). A host without a registry (`available` none) takes every agent not found
/// unavailable as available.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Availability {
    pub available: Option<std::collections::BTreeSet<String>>,
    pub unavailable: BTreeMap<String, String>,
    /// Agents found installed and usable but not logged in (their auth status says so), with what to do: a change to
    /// one is refused, BLOCKED_AUTH (REQ-019 failure mode 3; docs/adr/ADR-024.md decision 7).
    pub auth_missing: BTreeMap<String, String>,
}

impl Availability {
    /// Whether `agent` was not looked at yet.
    pub fn unknown(&self, agent: &str) -> bool {
        self.available.as_ref().is_some_and(|a| !a.contains(agent))
            && !self.unavailable.contains_key(agent)
    }
}

impl From<std::collections::BTreeSet<String>> for Availability {
    /// `unavailable` found so with no reason given, every other agent available.
    fn from(unavailable: std::collections::BTreeSet<String>) -> Availability {
        Availability {
            available: None,
            unavailable: unavailable
                .into_iter()
                .map(|agent| (agent, "unavailable".to_owned()))
                .collect(),
            auth_missing: BTreeMap::new(),
        }
    }
}

/// Run `run_id`'s checkpoint (decision 8, third version), in one transaction. A role the project's current revision
/// does not change moves to it as it is, live attempt or not. A changed role whose new agent was not looked at yet
/// waits, held, with no verdict. A changed role whose new agent is unavailable (the registry's, probed outside the
/// writer) is refused: the old binding and epoch stay and keep being dispatched (REQ-019 decision 2, M2T07R-150).
/// Otherwise a changed role with an attempt PREPARED, STARTING or RUNNING waits, held from
/// dispatch; one with none takes its new binding at epoch+1 with no session yet (a removed role BLOCKED, a new role
/// bound at epoch 1) and a team.binding_applied event. A refused change is judged again at every checkpoint: each turn
/// of the verdict (refused, or pending again once its agent is there) is one event. Once every role is at the
/// revision, so is the run. Whether anything changed.
pub(super) fn checkpoint(
    conn: &mut Connection,
    run_id: &str,
    availability: &Availability,
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
    let mut outbox = Vec::new();
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
        let (written, refused) = verdicts(&tx, run_id, &binding.role, revision)?;
        let verdict = |kind: &str, payload: serde_json::Value| super::writer::NewEvent {
            project_id: project_id.clone(),
            event_id: crate::ids::child_id(
                run_id,
                &format!("team-verdict/{}/{revision}/{}", binding.role, written + 1),
            ),
            run_id: Some(run_id.to_owned()),
            kind: kind.to_owned(),
            payload,
        };
        let target = wanted.team.get(&binding.role);
        if target.is_some_and(|role| availability.unknown(role.agent.as_str())) {
            behind = true;
            continue;
        }
        // A refusal is written when the change was not refused yet, or for another reason or detail than its last
        // (M2T17R-51: not installed, then not logged in, says what to do now).
        let new_refusal = |reason: &str, detail: &str| {
            refused.as_ref() != Some(&(reason.to_owned(), detail.to_owned()))
        };
        if let Some(role) = target
            && let Some(why) = availability.unavailable.get(role.agent.as_str())
        {
            behind = true;
            if new_refusal("AGENT_UNAVAILABLE", why) {
                changed = true;
                events.push(verdict(
                    REFUSED,
                    serde_json::json!({
                        "run_id": run_id, "role": binding.role, "config_revision": revision,
                        "agent": role.agent.as_str(), "reason": "AGENT_UNAVAILABLE", "detail": why,
                        "kept": {"agent": binding.agent, "model": binding.model, "role_epoch": binding.role_epoch},
                    }),
                ));
            }
            continue;
        }
        // An agent not logged in cannot take the role: refused, the old binding working on, until its auth is there
        // (REQ-019 failure mode 3; ADR-024 decision 7).
        if let Some(role) = target
            && let Some(why) = availability.auth_missing.get(role.agent.as_str())
        {
            behind = true;
            if new_refusal(BLOCKED_AUTH, why) {
                changed = true;
                events.push(verdict(
                    REFUSED,
                    serde_json::json!({
                        "run_id": run_id, "role": binding.role, "config_revision": revision,
                        "agent": role.agent.as_str(), "reason": BLOCKED_AUTH, "detail": why,
                        "kept": {"agent": binding.agent, "model": binding.model, "role_epoch": binding.role_epoch},
                    }),
                ));
            }
            continue;
        }
        // A change to a new holder hands the role over with a package naming the run's spec, whatever the role
        // (ADR-024 decisions 1 and 2, M2T17R-03): with no spec it is refused and the old binding works on until the
        // run has one (decision 10). A role left with no agent has no new holder to hand over to.
        let handover_spec = match target {
            Some(_) => match super::handover::spec(&tx, run_id)? {
                Some(spec) => Some(spec),
                None => {
                    behind = true;
                    if new_refusal(super::handover::NO_SPEC, NO_SPEC_DETAIL) {
                        changed = true;
                        events.push(verdict(
                            REFUSED,
                            serde_json::json!({
                                "run_id": run_id, "role": binding.role, "config_revision": revision,
                                "agent": target.map(|role| role.agent.as_str()),
                                "reason": super::handover::NO_SPEC,
                                "detail": NO_SPEC_DETAIL,
                                "kept": {
                                    "agent": binding.agent, "model": binding.model,
                                    "role_epoch": binding.role_epoch,
                                },
                            }),
                        ));
                    }
                    continue;
                }
            },
            _ => None,
        };
        if live.contains(&binding.role) {
            behind = true;
            if refused.is_some() {
                changed = true;
                events.push(verdict(
                    PENDING_AGAIN,
                    serde_json::json!({
                        "run_id": run_id, "role": binding.role, "config_revision": revision,
                        "agent": target.map(|role| role.agent.as_str()),
                    }),
                ));
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
                    params![run_id, binding.role, epoch, revision, AGENT_UNAVAILABLE, now],
                )
                .map_err(sqlite_error)?;
                serde_json::Value::Null
            }
        };
        events.push(applied(&binding.role, epoch, from.clone(), to.clone()));
        if let Some(spec) = handover_spec {
            let (event, mut notice) = super::handover::prepare(
                &tx,
                &project_id,
                run_id,
                &binding.role,
                epoch,
                spec,
                &from,
                &to,
            )?;
            events.push(event);
            notice.event = events.len() - 1;
            outbox.push(notice);
        }
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
            outbox,
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// A binding that needs its agent looked at by the binder (decision 9): ACTIVE with no session yet, or BLOCKED because
/// its agent was unavailable (not a removed role).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unbound {
    pub run_id: String,
    pub role: String,
    pub role_epoch: i64,
    pub agent: String,
}

/// A session the binder grants: for run `run_id`'s `role` at `role_epoch`, with its agent's version and the
/// capabilities a turn of it will have (decision 9).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionGrant {
    pub run_id: String,
    pub role: String,
    pub role_epoch: i64,
    pub agent: String,
    pub agent_version: String,
    pub capabilities: serde_json::Value,
}

/// What a dispatched attempt runs as (docs/adr/ADR-022.md decisions 9, 11 and 12): its agent, model and session as
/// its binding gave them, the role's prompt in the revision it was bound at, and the run's goal and the task's title
/// for the prompt's header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assignment {
    pub agent: Option<String>,
    pub model: Option<String>,
    pub role_epoch: i64,
    pub config_revision: i64,
    pub session_id: Option<String>,
    /// The agent's own session id, to resume; none when the session's capabilities say it cannot be resumed (decision
    /// 9, M2T07R-257): its next turn starts a new one of its own.
    pub native_session: Option<String>,
    pub agent_version: Option<String>,
    /// A prompt override file (relative to the project) or `builtin:<role>`; none: the role's own built-in contract.
    pub prompt: Option<String>,
    pub goal: Option<String>,
    pub title: Option<String>,
    /// This attempt's turn is the first of a session that was given a first input (its session.start delivered to the
    /// session, and no other attempt of the session started its process): the turn starts from that input
    /// (docs/adr/ADR-024.md decision 4, M2T17R-151, -152). The store says it, not the agent's native session.
    pub first_input: bool,
}

/// The assignment of attempt `attempt_id` of task `task_id` in run `run_id` (project `project_id`), `role`.
pub(super) fn assignment(
    conn: &Connection,
    project_id: &str,
    run_id: &str,
    task_id: &str,
    attempt_id: &str,
    role: &str,
) -> Result<Assignment, PersistError> {
    let row: Option<Assignment> = conn
        .query_row(
            "SELECT a.agent, b.model, a.role_epoch, a.config_revision, a.session_id, \
             CASE WHEN json_extract(s.capabilities_json, '$.resume') = 1 THEN s.native_id END, s.agent_version, \
             (SELECT json_extract(e.payload_json, '$.goal') FROM events e WHERE e.run_id = ?2 AND e.kind = 'run.created' \
              ORDER BY e.seq LIMIT 1), \
             (SELECT json_extract(j.value, '$.title') FROM events e, json_each(json_extract(e.payload_json, '$.tasks')) j \
              WHERE e.run_id = ?2 AND e.kind = 'plan.imported' AND json_extract(j.value, '$.task_id') = ?3 \
              ORDER BY e.seq DESC LIMIT 1), \
             EXISTS(SELECT 1 FROM outbox o JOIN receipts r ON r.message_id = o.message_id WHERE o.state = 'ACKED' \
              AND json_extract(o.payload_json, '$.kind') = 'session.start' \
              AND json_extract(r.result_json, '$.session_id') = a.session_id) \
             AND NOT EXISTS(SELECT 1 FROM attempts p WHERE p.session_id = a.session_id \
              AND p.attempt_id <> a.attempt_id AND p.pid IS NOT NULL) \
             FROM attempts a LEFT JOIN sessions s ON s.session_id = a.session_id \
             LEFT JOIN role_bindings b ON b.run_id = ?2 AND b.role = ?4 WHERE a.attempt_id = ?1",
            params![attempt_id, run_id, task_id, role],
            |r| {
                Ok(Assignment {
                    agent: r.get(0)?,
                    model: r.get(1)?,
                    role_epoch: r.get(2)?,
                    config_revision: r.get(3)?,
                    session_id: r.get(4)?,
                    native_session: r.get(5)?,
                    agent_version: r.get(6)?,
                    prompt: None,
                    goal: r.get(7)?,
                    title: r.get(8)?,
                    first_input: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some(mut assignment) = row else {
        return Ok(Assignment::default());
    };
    assignment.prompt = at_revision(conn, project_id, assignment.config_revision)?
        .and_then(|config| config.team.get(role).and_then(|r| r.prompt.clone()));
    Ok(assignment)
}

/// The id of the session of `role` at `role_epoch` in run `run_id`: one per run, role and epoch.
pub fn session_id(run_id: &str, role: &str, role_epoch: i64) -> String {
    crate::ids::child_id(run_id, &format!("session/{role}/{role_epoch}"))
}

/// The project's current team: each role and the built-in role contract it follows.
pub(super) fn roles(
    conn: &Connection,
    project_id: &str,
) -> Result<Vec<(String, Option<String>)>, PersistError> {
    let team = latest(conn, project_id)?
        .map(|(_, config)| config.team)
        .unwrap_or_else(|| TeamConfig::builtin().team);
    Ok(team
        .into_iter()
        .map(|(name, role)| {
            let contract = if crate::config::BUILTIN_ROLES.contains(&name.as_str()) {
                Some(name.clone())
            } else {
                role.prompt
                    .as_deref()
                    .and_then(|p| p.strip_prefix(crate::config::BUILTIN_PROMPT))
                    .map(str::to_owned)
            };
            (name, contract)
        })
        .collect())
}

/// The agents the runner needs looked at (decision 10): the project's current team's, and those of every binding in a
/// run not ended, so an agent kept by a refused change or bound before a restart is looked at too (M2T07R-250).
pub(super) fn agents(
    conn: &Connection,
    project_id: &str,
) -> Result<std::collections::BTreeSet<String>, PersistError> {
    let team = latest(conn, project_id)?
        .map(|(_, config)| config.team)
        .unwrap_or_else(|| TeamConfig::builtin().team);
    let mut agents: std::collections::BTreeSet<String> = team
        .values()
        .map(|role| role.agent.as_str().to_owned())
        .collect();
    let mut statement = conn
        .prepare(
            "SELECT DISTINCT b.agent FROM role_bindings b JOIN runs r ON r.run_id = b.run_id WHERE r.project_id = ?1 \
             AND r.state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE')",
        )
        .map_err(sqlite_error)?;
    for agent in statement
        .query_map(params![project_id], |r| r.get::<_, String>(0))
        .map_err(sqlite_error)?
    {
        agents.insert(agent.map_err(sqlite_error)?);
    }
    Ok(agents)
}

/// The project's bindings the binder looks at, in runs not ended.
pub(super) fn unbound(conn: &Connection, project_id: &str) -> Result<Vec<Unbound>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT b.run_id, b.role, b.role_epoch, b.agent, b.config_revision, b.session_id FROM role_bindings b \
             JOIN runs r ON r.run_id = b.run_id WHERE r.project_id = ?1 \
             AND r.state NOT IN ('CANCELLED', 'COMPLETED', 'FAILED', 'INCOMPLETE') \
             AND ((b.state = 'ACTIVE' AND b.session_id IS NULL) OR (b.state = 'BLOCKED' AND b.blocked_reason = ?2)) \
             ORDER BY r.created_at, b.run_id, b.role",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(String, String, i64, String, i64, Option<String>)> = statement
        .query_map(params![project_id, AGENT_UNAVAILABLE], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut found = Vec::new();
    for (run_id, role, role_epoch, agent, config_revision, session_id) in rows {
        let binding = Bound {
            role: role.clone(),
            agent: agent.clone(),
            model: None,
            role_epoch,
            config_revision,
            session_id,
        };
        // A removed role stays BLOCKED: it is not the binder's.
        if matches!(before(conn, project_id, &binding)?, Before::Absent) {
            continue;
        }
        found.push(Unbound {
            run_id,
            role,
            role_epoch,
            agent,
        });
    }
    Ok(found)
}

/// In one transaction: the session of `grant`, and the binding at its epoch bound to it (ACTIVE again if its agent was
/// unavailable before), with a team.session_bound event; whether the binding was still the one granted.
pub(super) fn bind_session(
    conn: &mut Connection,
    grant: &SessionGrant,
    ctx: &super::writer::Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let id = session_id(&grant.run_id, &grant.role, grant.role_epoch);
    tx.execute(
        "INSERT OR IGNORE INTO sessions(session_id, agent, role, agent_version, capabilities_json) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            id,
            grant.agent,
            grant.role,
            grant.agent_version,
            grant.capabilities.to_string()
        ],
    )
    .map_err(sqlite_error)?;
    let bound = tx
        .execute(
            "UPDATE role_bindings SET session_id = ?1, state = 'ACTIVE', blocked_reason = NULL, missing_json = '[]' \
             WHERE run_id = ?2 AND role = ?3 AND role_epoch = ?4 AND agent = ?5 \
             AND ((state = 'ACTIVE' AND session_id IS NULL) OR (state = 'BLOCKED' AND blocked_reason = ?6))",
            params![id, grant.run_id, grant.role, grant.role_epoch, grant.agent, AGENT_UNAVAILABLE],
        )
        .map_err(sqlite_error)?;
    if bound == 0 {
        return Ok(false);
    }
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![grant.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events: vec![super::writer::NewEvent {
                project_id,
                event_id: crate::ids::child_id(&id, "bound"),
                run_id: Some(grant.run_id.clone()),
                kind: "team.session_bound".to_owned(),
                payload: serde_json::json!({
                    "run_id": grant.run_id, "role": grant.role, "role_epoch": grant.role_epoch,
                    "session_id": id, "agent": grant.agent, "agent_version": grant.agent_version,
                }),
            }],
            ..super::writer::Commit::default()
        },
        ctx,
    )?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(true)
}

/// In one transaction: run `run_id`'s binding of `role` at `role_epoch`, still waiting for a session, BLOCKED because
/// its agent is unavailable (`$defs.BlockedReason` AGENT_UNAVAILABLE, the agent in missing_json), with a
/// team.binding_blocked event; whether it was still waiting.
pub(super) fn block_binding(
    conn: &mut Connection,
    unbound: &Unbound,
    detail: &str,
    ctx: &super::writer::Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = super::writer::begin(conn, ctx)?;
    let blocked = tx
        .execute(
            "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = ?5, missing_json = json_array(?4) \
             WHERE run_id = ?1 AND role = ?2 AND role_epoch = ?3 AND agent = ?4 AND state = 'ACTIVE' \
             AND session_id IS NULL",
            params![unbound.run_id, unbound.role, unbound.role_epoch, unbound.agent, AGENT_UNAVAILABLE],
        )
        .map_err(sqlite_error)?;
    if blocked == 0 {
        return Ok(false);
    }
    let project_id: String = tx
        .query_row(
            "SELECT project_id FROM runs WHERE run_id = ?1",
            params![unbound.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    super::writer::write(
        &tx,
        &super::writer::Commit {
            events: vec![super::writer::NewEvent {
                project_id,
                event_id: crate::ids::child_id(
                    &unbound.run_id,
                    &format!("blocked/{}/{}", unbound.role, unbound.role_epoch),
                ),
                run_id: Some(unbound.run_id.clone()),
                kind: "team.binding_blocked".to_owned(),
                payload: serde_json::json!({
                    "run_id": unbound.run_id, "role": unbound.role, "role_epoch": unbound.role_epoch,
                    "agent": unbound.agent, "reason": AGENT_UNAVAILABLE, "detail": detail,
                }),
            }],
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
    /// `{"agent", "model", "config_revision", "refused"}`, `agent` null when the role is removed, `refused` the reason
    /// while the change is refused (the old binding works on), else null.
    pub pending: Option<serde_json::Value>,
    /// The role was removed from the team (its row stays, BLOCKED, so an old epoch stays fenced): not an availability
    /// block.
    pub removed: bool,
    /// The role's new holder waits for its first input (its session.start at the binding's epoch not delivered yet):
    /// it is dispatched nothing meanwhile (docs/adr/ADR-024.md decision 4, M2T17R-150).
    pub first_input_pending: bool,
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
                first_input_pending: false,
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
        member.first_input_pending =
            super::handover::first_input_pending(conn, run_id, &member.role, member.role_epoch)?;
        if member.config_revision >= team_revision {
            continue;
        }
        let wanted = config.team.get(&member.role);
        if changes(conn, project_id, &binding, &config)? {
            let refused = verdicts(conn, run_id, &member.role, team_revision)?.1;
            member.pending = Some(serde_json::json!({
                "agent": wanted.map(|role| role.agent.as_str()),
                "model": wanted.and_then(|role| role.model.clone()),
                "config_revision": team_revision,
                // Why it is refused, as its last refusal said (not always AGENT_UNAVAILABLE: M2T17R-51).
                "refused": refused.map(|(reason, _)| reason),
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
                first_input_pending: false,
                pending: Some(serde_json::json!({
                    "agent": role.agent.as_str(),
                    "model": role.model,
                    "config_revision": team_revision,
                    "refused": null,
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
