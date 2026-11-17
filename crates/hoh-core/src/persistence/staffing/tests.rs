//! Staffing's store side against docs/adr/ADR-030.md decisions 1, 4 and 5 (the test plan's "store" bullet and
//! `staffing_ids_of_a_long_run_are_distinct`): the bindings read for the runner, each judgment's writes and events in
//! one transaction, the condition on the judged binding, the pending changes while an attempt is in flight, and the
//! staffing series' ids.

use crate::config::{Budget, Config};
use crate::persistence::{
    Evidence, Intent, NewEvent, Options, RunTarget, StaffChange, StaffWrite, Store, SubmitCommand,
    selftest,
};
use crate::protocol::BlockedReason;
use crate::staffing::{AgentIdentity, BindingState, Cause, PendingChange, Session, Staffed};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::PathBuf;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    let store = Store::open(&path, options)?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-14T00:00:00Z');",
    )?;
    Ok((store, path))
}

/// StartRun of run `run_id`, as the host submits it: every role of the current revision bound at epoch 1, no session.
fn start(run_id: &str, budget: Budget) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{}", run_id.len()),
        idempotency_key: format!("k-{}", run_id.len()),
        kind: "StartRun".to_owned(),
        run_id: run_id.to_owned(),
        expected_revision: 0,
        payload: json!({"goal": "g", "mode": "AUTO", "interactive": false}),
        payload_sha256: format!("{:064}", run_id.len()),
        target: RunTarget::Start {
            mode: "AUTO".to_owned(),
            budget,
            policy: "balanced".to_owned(),
        },
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{}", run_id.len()),
            run_id: Some(run_id.to_owned()),
            kind: "run.created".to_owned(),
            payload: json!({"to": "CREATED"}),
        },
        outbox: None,
    }
}

/// The project's ConfigureTeam at team revision `expected` with `bindings`.
fn team_change(key: &str, expected: i64, bindings: Value) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{key}"),
        idempotency_key: format!("k-{key}"),
        kind: "ConfigureTeam".to_owned(),
        run_id: String::new(),
        expected_revision: expected,
        payload: json!({"bindings": bindings, "apply_at": "checkpoint"}),
        payload_sha256: format!("{:064}", key.len() + 100),
        target: RunTarget::Team(crate::persistence::TeamChange {
            bindings: Some(bindings),
            scheduler: None,
            root: None,
            file: None,
        }),
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{key}"),
            run_id: None,
            kind: "team.configured".to_owned(),
            payload: json!({"revision": expected + 1}),
        },
        outbox: None,
    }
}

fn exec(path: &PathBuf, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

/// The rows `sql` reads, each column as JSON.
fn values(path: &PathBuf, sql: &str) -> R<Vec<Vec<Value>>> {
    use rusqlite::types::ValueRef;
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let width = statement.column_count();
    let rows = statement
        .query_map([], |r| {
            (0..width)
                .map(|i| {
                    Ok(match r.get_ref(i)? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(n) => json!(n),
                        ValueRef::Real(x) => json!(x),
                        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
                        ValueRef::Blob(b) => json!(b),
                    })
                })
                .collect()
        })?
        .collect::<Result<Vec<Vec<Value>>, _>>()?;
    Ok(rows)
}

/// The one value `sql` reads.
fn value(path: &PathBuf, sql: &str) -> R<Value> {
    values(path, sql)?
        .pop()
        .and_then(|mut row| row.pop())
        .ok_or_else(|| format!("no row: {sql}").into())
}

/// `(event_id, kind, payload)` of run `run_id`'s events of `kinds`, in seq order.
fn events(path: &PathBuf, run_id: &str, kinds: &[&str]) -> R<Vec<(String, String, Value)>> {
    let list = kinds
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ");
    values(
        path,
        &format!(
            "SELECT event_id, kind, payload_json FROM events WHERE run_id = '{run_id}' AND kind IN ({list}) \
             ORDER BY seq"
        ),
    )?
    .into_iter()
    .map(|row| {
        Ok((
            row[0].as_str().unwrap_or_default().to_owned(),
            row[1].as_str().unwrap_or_default().to_owned(),
            serde_json::from_str(row[2].as_str().unwrap_or("null"))?,
        ))
    })
    .collect()
}

/// The staffing events (decision 5's four kinds).
const STAFFING: [&str; 4] = [
    "team.binding_blocked",
    "team.binding_unblocked",
    "team.binding_change_pending",
    "team.binding_change_withdrawn",
];

/// Run `run_id`'s binding of `role`: agent, model, role_epoch, config_revision, session_id, state, blocked_reason,
/// missing_json.
fn binding(path: &PathBuf, run_id: &str, role: &str) -> R<Vec<Value>> {
    values(
        path,
        &format!(
            "SELECT agent, model, role_epoch, config_revision, session_id, state, blocked_reason, missing_json \
             FROM role_bindings WHERE run_id = '{run_id}' AND role = '{role}'"
        ),
    )?
    .pop()
    .ok_or_else(|| format!("no binding of {role}").into())
}

/// An identity whose four fields name `tag`.
fn identity(tag: &str) -> AgentIdentity {
    AgentIdentity {
        launcher_path: format!("/opt/{tag}/bin/agent"),
        launcher_sha256: "1".repeat(64),
        implementation_path: format!("/opt/{tag}/lib/agent"),
        implementation_sha256: "2".repeat(64),
    }
}

/// An evidence object as the host registers it (its bytes are not read by the store).
fn evidence(n: u8) -> Evidence {
    Evidence {
        sha256: format!("{n:x}").repeat(64),
        bytes: 100 + u64::from(n),
    }
}

fn change(run_id: &str, role: &str, epoch: i64, agent: &str, write: StaffWrite) -> StaffChange {
    StaffChange {
        run_id: run_id.to_owned(),
        role: role.to_owned(),
        role_epoch: epoch,
        agent: agent.to_owned(),
        write,
    }
}

/// A grant of a session recording [`caps`].
fn grant(agent_version: &str, tag: &str) -> StaffWrite {
    StaffWrite::Grant {
        agent_version: agent_version.to_owned(),
        capabilities: caps(),
        identity: identity(tag),
    }
}

fn block(reason: BlockedReason, missing: &[&str], n: u8) -> StaffWrite {
    StaffWrite::Block {
        reason,
        missing: missing.iter().map(|m| (*m).to_owned()).collect(),
        evidence: evidence(n),
    }
}

/// The team: qa pi falling back to claude, the developer claude with model m-dev; revision 1. Run R1 started under it,
/// on spec s1 and RUNNING.
async fn team_and_run(store: &Store, path: &PathBuf) -> R {
    store
        .submit_command(team_change(
            "fallbacks",
            0,
            json!({"qa": {"agent": "pi", "fallback": ["claude"]}, "developer": {"agent": "claude", "model": "m-dev"}}),
        ))
        .await?;
    // The project's specification first: StartRun adopts it (docs/adr/ADR-031.md decision 1).
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);",
    )?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    exec(
        path,
        "UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R1';",
    )
}

/// Task T0 of `role` with attempt A0 in `state` (in flight: PREPARED, STARTING or RUNNING) in session `session`.
fn in_flight(path: &PathBuf, role: &str, state: &str, session: Option<&str>) -> R {
    let session = session.map_or("NULL".to_owned(), |s| format!("'{s}'"));
    exec(
        path,
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
               ('T0', 'R1', 's1', 'RUNNING', 2, '{role}', 0, 1, 1);
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, agent,
               started_at) VALUES ('A0', 'T0', {session}, '{state}', 1, 1, 1, 'claude', '2026-11-14T00:00:00Z');"
        ),
    )
}

/// A0 ends and T0 with it.
fn a0_ends(path: &PathBuf) -> R {
    exec(
        path,
        "UPDATE attempts SET state = 'SETTLED', ended_at = '2026-11-14T00:01:00Z' WHERE attempt_id = 'A0';
         UPDATE tasks SET state = 'IMPLEMENTED', revision = 3 WHERE task_id = 'T0';",
    )
}

/// Whether run R1's `role` would be dispatched now (`decide::load_binding`'s `active`).
fn active(path: &PathBuf, role: &str) -> R<bool> {
    let mut conn = Connection::open(path)?;
    let tx = conn.transaction()?;
    Ok(crate::persistence::decide::load_binding(&tx, "R1", role)?
        .ok_or("no binding")?
        .active)
}

/// Run R1's `role` as the team view shows it: its missing lines and pending change.
fn shown(path: &PathBuf, role: &str) -> R<(Vec<String>, Option<Value>)> {
    let view = crate::persistence::team::view(&Connection::open(path)?, "R1", "p1")?;
    let member = view
        .members
        .into_iter()
        .find(|m| m.role == role)
        .ok_or("no member")?;
    Ok((member.missing, member.pending))
}

/// The staffing read of run R1's `role`.
async fn staffed(store: &Store, role: &str) -> R<Staffed> {
    store
        .runner_waiting("p1")
        .await?
        .staffing
        .into_iter()
        .find(|s| s.run_id == "R1" && s.role == role)
        .ok_or_else(|| format!("{role} is not staffed").into())
}

#[tokio::test]
async fn the_staffing_read_has_every_binding_with_its_order_session_identity_and_flight() -> R {
    // Decision 1: every binding of a run not ended with what judging it needs; the order is the revision's agent then
    // its fallbacks (decision 3), the session with the identity its team.session_bound records (decision 4), live as
    // the checkpoint's notion; a session with no identity in its record is stale.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    let qa = staffed(&store, "qa").await?;
    assert_eq!(
        qa,
        Staffed {
            run_id: "R1".to_owned(),
            role: "qa".to_owned(),
            role_epoch: 1,
            agent: "pi".to_owned(),
            model: None,
            preferred: "pi".to_owned(),
            preferred_model: None,
            state: BindingState::Active,
            blocked_reason: None,
            missing: Vec::new(),
            session: None,
            live: false,
            order: vec!["pi".to_owned(), "claude".to_owned()],
            required: [
                "command_execution".to_owned(),
                "structured_events".to_owned()
            ]
            .into(),
            excluded: Vec::new(),
            pending: None,
        }
    );
    assert!(
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                grant("2.1.318", "a")
            ))
            .await?
    );
    in_flight(&path, "developer", "RUNNING", None)?;
    let developer = staffed(&store, "developer").await?;
    assert_eq!(
        (
            developer.model.as_deref(),
            developer.preferred_model.as_deref(),
            &developer.order,
            developer.live
        ),
        (
            Some("m-dev"),
            Some("m-dev"),
            &vec!["claude".to_owned()],
            true
        )
    );
    assert_eq!(
        developer.session,
        Some(Session {
            agent_version: "2.1.318".to_owned(),
            capabilities: caps(),
            identity: Some(identity("a")),
        })
    );
    assert_eq!(
        staffed(&store, "reviewer").await?.required,
        ["resume".to_owned(), "structured_events".to_owned()].into(),
        "the reviewer's contract needs resume"
    );
    // A session granted before identities were recorded (its row only, no team.session_bound): no identity, so stale.
    exec(
        &path,
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('old', 'codex', 'commander', '0.166.0', '{\"structured_events\": true}');
         UPDATE role_bindings SET session_id = 'old' WHERE run_id = 'R1' AND role = 'commander';",
    )?;
    let commander = staffed(&store, "commander").await?;
    assert_eq!(
        commander.session.as_ref().map(|s| s.identity.clone()),
        Some(None)
    );
    // A hand-written binding of a revision not recorded: its order is its agent alone.
    exec(
        &path,
        "UPDATE role_bindings SET config_revision = 99, agent = 'codex' WHERE run_id = 'R1' AND role = 'cto';",
    )?;
    let cto = staffed(&store, "cto").await?;
    assert_eq!(
        (cto.order, cto.preferred),
        (vec!["codex".to_owned()], "codex".to_owned())
    );
    // A removed role and a role whose change waits for its checkpoint are not staffing's.
    exec(
        &path,
        "UPDATE role_bindings SET config_revision = 1 WHERE run_id = 'R1' AND role = 'cto';",
    )?;
    store
        .submit_command(team_change(
            "away",
            1,
            json!({"researcher": null, "cto": {"agent": "pi"}}),
        ))
        .await?;
    store
        .team_checkpoint("R1", crate::persistence::Availability::default())
        .await?;
    let roles: Vec<String> = store
        .runner_waiting("p1")
        .await?
        .staffing
        .into_iter()
        .map(|s| s.role)
        .collect();
    assert_eq!(
        roles,
        ["commander", "cto", "developer", "qa", "reviewer"],
        "the removed researcher is left out"
    );
    in_flight_of_cto(&path)?;
    store
        .submit_command(team_change(
            "cto-again",
            2,
            json!({"cto": {"agent": "codex"}}),
        ))
        .await?;
    let roles: Vec<String> = store
        .runner_waiting("p1")
        .await?
        .staffing
        .into_iter()
        .map(|s| s.role)
        .collect();
    assert_eq!(
        roles,
        ["commander", "developer", "qa", "reviewer"],
        "the cto's change waits for its checkpoint"
    );
    Ok(())
}

/// A cto attempt in flight, so its change waits for its checkpoint.
fn in_flight_of_cto(path: &PathBuf) -> R {
    exec(
        path,
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('TC', 'R1', 's1', 'RUNNING', 2, 'cto', 0, 9, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, agent, started_at)
           VALUES ('AC', 'TC', 'RUNNING', 2, 1, 2, 'pi', '2026-11-14T00:00:00Z');",
    )
}

#[tokio::test]
async fn a_grant_records_the_whole_identity_and_ends_a_vacancy_only() -> R {
    // Decisions 4, 5 and 7: a Grant's team.session_bound carries the whole AgentIdentity; a binding BLOCKED for a
    // reason of V is granted ACTIVE; one BLOCKED otherwise, or that has its session, is not.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    assert!(
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                block(BlockedReason::BlockedAuth, &["claude:BLOCKED_AUTH"], 1)
            ))
            .await?
    );
    assert!(
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                grant("2.1.318", "a")
            ))
            .await?
    );
    let session = crate::persistence::session_id("R1", "developer", 1);
    assert_eq!(
        binding(&path, "R1", "developer")?[4..8],
        [json!(session), json!("ACTIVE"), Value::Null, json!("[]")]
    );
    let bound = events(&path, "R1", &["team.session_bound"])?;
    assert_eq!(bound.len(), 1);
    assert_eq!(bound[0].0, crate::ids::child_id(&session, "bound"));
    assert_eq!(
        bound[0].2["identity"],
        json!({"launcher_path": "/opt/a/bin/agent", "launcher_sha256": "1".repeat(64),
               "implementation_path": "/opt/a/lib/agent", "implementation_sha256": "2".repeat(64)})
    );
    assert!(
        !store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                grant("2.1.318", "a")
            ))
            .await?,
        "a binding with its session is granted no other"
    );
    exec(
        &path,
        "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = 'NEED_DECISION' \
         WHERE run_id = 'R1' AND role = 'cto';",
    )?;
    assert!(
        !store
            .staff(change("R1", "cto", 1, "claude", grant("2.1.318", "a")))
            .await?,
        "NEED_DECISION is no vacancy"
    );
    for reason in crate::staffing::VACANCY {
        exec(
            &path,
            &format!(
                "UPDATE role_bindings SET state = 'BLOCKED', blocked_reason = '{}', session_id = NULL \
                 WHERE run_id = 'R1' AND role = 'reviewer';",
                reason.as_str()
            ),
        )?;
        exec(
            &path,
            "DELETE FROM events WHERE kind = 'team.session_bound' AND json_extract(payload_json, '$.role') = 'reviewer';",
        )?;
        assert!(
            store
                .staff(change("R1", "reviewer", 1, "codex", grant("0.166.0", "c")))
                .await?,
            "{reason:?}"
        );
        assert_eq!(
            binding(&path, "R1", "reviewer")?[5],
            json!("ACTIVE"),
            "{reason:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_block_is_written_on_entering_it_or_when_its_reason_or_missing_changes() -> R {
    // Decision 5: Block sets BLOCKED, its reason and missing (the session kept), registers its evidence object and
    // writes team.binding_blocked{cause: staffing} numbered in the epoch's staffing series; the same block again writes
    // nothing; another missing is a new event. The team view shows the missing lines.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    let lines = ["pi:not_installed", "claude:not_installed"];
    assert!(
        store
            .staff(change(
                "R1",
                "qa",
                1,
                "pi",
                block(BlockedReason::AgentUnavailable, &lines, 1)
            ))
            .await?
    );
    assert_eq!(
        binding(&path, "R1", "qa")?[5..8],
        [
            json!("BLOCKED"),
            json!("AGENT_UNAVAILABLE"),
            json!(json!(lines).to_string())
        ]
    );
    let stem = crate::ids::series("R1", "staffing/qa/1");
    let artifact = format!(
        "{}:{}",
        "1".repeat(64),
        crate::objects::relative_path(&"1".repeat(64))
    );
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(
        written,
        vec![(
            crate::ids::member(&stem, 1),
            "team.binding_blocked".to_owned(),
            json!({"run_id": "R1", "role": "qa", "role_epoch": 1, "agent": "pi", "reason": "AGENT_UNAVAILABLE",
                   "missing": lines, "evaluated": lines, "probe_artifact_id": artifact,
                   "probe_sha256": "1".repeat(64), "cause": "staffing"})
        )]
    );
    assert_eq!(
        value(
            &path,
            &format!("SELECT bytes FROM artifacts WHERE artifact_id = '{artifact}'")
        )?,
        json!(101)
    );
    assert!(
        !store
            .staff(change(
                "R1",
                "qa",
                1,
                "pi",
                block(BlockedReason::AgentUnavailable, &lines, 2)
            ))
            .await?,
        "the same block again"
    );
    assert_eq!(events(&path, "R1", &STAFFING)?.len(), 1);
    assert_eq!(
        value(&path, "SELECT count(*) FROM artifacts")?,
        json!(1),
        "no evidence registered for a write that did not happen"
    );
    let other = ["pi:not_installed", "claude:BLOCKED_AUTH"];
    assert!(
        store
            .staff(change(
                "R1",
                "qa",
                1,
                "pi",
                block(BlockedReason::AgentUnavailable, &other, 3)
            ))
            .await?
    );
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(written.len(), 2);
    assert_eq!(written[1].0, crate::ids::member(&stem, 2));
    assert_eq!(
        shown(&path, "qa")?,
        (other.map(str::to_owned).to_vec(), None)
    );
    Ok(())
}

#[tokio::test]
async fn an_unblock_makes_the_binding_active_with_its_session() -> R {
    // Decision 5: Unblock sets ACTIVE, no reason and no missing, with team.binding_unblocked; on an ACTIVE binding with
    // nothing pending it writes nothing.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            block(BlockedReason::BlockedAuth, &["claude:BLOCKED_AUTH"], 1),
        ))
        .await?;
    assert!(!active(&path, "developer")?);
    assert!(
        store
            .staff(change("R1", "developer", 1, "claude", StaffWrite::Unblock))
            .await?
    );
    let session = crate::persistence::session_id("R1", "developer", 1);
    assert_eq!(
        binding(&path, "R1", "developer")?[4..8],
        [json!(session), json!("ACTIVE"), Value::Null, json!("[]")]
    );
    assert!(active(&path, "developer")?);
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(
        written.iter().map(|e| e.1.as_str()).collect::<Vec<_>>(),
        ["team.binding_blocked", "team.binding_unblocked"]
    );
    assert_eq!(
        written[1].2,
        json!({"run_id": "R1", "role": "developer", "role_epoch": 1, "agent": "claude",
               "reason": "BLOCKED_AUTH"})
    );
    assert!(
        !store
            .staff(change("R1", "developer", 1, "claude", StaffWrite::Unblock))
            .await?,
        "nothing to unblock"
    );
    Ok(())
}

#[tokio::test]
async fn repeated_blocks_and_unblocks_in_one_epoch_have_distinct_ids() -> R {
    // M2T15D-202: each staffing event of one (run, role, epoch) is the next member of its series.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    for n in 0..3 {
        assert!(
            store
                .staff(change(
                    "R1",
                    "developer",
                    1,
                    "claude",
                    block(BlockedReason::BlockedAuth, &["claude:BLOCKED_AUTH"], n + 1),
                ))
                .await?
        );
        assert!(
            store
                .staff(change("R1", "developer", 1, "claude", StaffWrite::Unblock))
                .await?
        );
    }
    let stem = crate::ids::series("R1", "staffing/developer/1");
    let ids: Vec<String> = events(&path, "R1", &STAFFING)?
        .into_iter()
        .map(|e| e.0)
        .collect();
    assert_eq!(
        ids,
        (1..=6)
            .map(|n| crate::ids::member(&stem, n))
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn staffing_ids_of_a_long_run_are_distinct() -> R {
    // M2T15D-202 (the test plan's "长 id 的计数"): a 160-character run id writes 3 staffing events in one (role, epoch):
    // their ids differ, each is a member of the series, counted 1, 2, 3, and no unique key fails.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let run = "R".repeat(crate::ids::MAX_ID);
    store
        .submit_command(start(&run, Config::default().budget))
        .await?;
    let stem = crate::ids::series(&run, "staffing/qa/1");
    let mut seen = Vec::new();
    for (n, missing) in ["pi:not_installed", "pi:probe_failed", "pi:unsupported"]
        .into_iter()
        .enumerate()
    {
        assert!(
            store
                .staff(change(
                    &run,
                    "qa",
                    1,
                    "pi",
                    block(
                        BlockedReason::AgentUnavailable,
                        &[missing],
                        u8::try_from(n)? + 1
                    ),
                ))
                .await?
        );
        let count = {
            let conn = Connection::open(&path)?;
            crate::persistence::series::count(&conn, &stem)?
        };
        assert_eq!(count, u64::try_from(n)? + 1);
        seen = events(&path, &run, &STAFFING)?
            .into_iter()
            .map(|e| e.0)
            .collect();
    }
    assert_eq!(seen.len(), 3);
    assert_eq!(
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        3
    );
    for (n, id) in seen.iter().enumerate() {
        assert!(id.len() <= crate::ids::MAX_ID, "{id}");
        assert_eq!(*id, crate::ids::member(&stem, u64::try_from(n)? + 1));
    }
    Ok(())
}

#[tokio::test]
async fn a_judgment_of_a_binding_that_moved_on_writes_nothing() -> R {
    // Decision 5: the write is conditioned on the run, role, epoch and agent judged; another epoch or agent is a no-op.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    let before = value(&path, "SELECT count(*) FROM events")?;
    for (epoch, agent) in [(2, "pi"), (1, "claude")] {
        for write in [
            block(BlockedReason::AgentUnavailable, &["pi:not_installed"], 1),
            StaffWrite::Rebind,
            StaffWrite::Switch {
                to: "claude".to_owned(),
                model: None,
                reason: BlockedReason::AgentUnavailable,
                line: "pi:not_installed".to_owned(),
                evaluated: vec!["pi:not_installed".to_owned(), "claude:usable".to_owned()],
                evidence: evidence(1),
            },
            grant("1.5.0", "p"),
        ] {
            assert!(
                !store.staff(change("R1", "qa", epoch, agent, write)).await?,
                "{epoch} {agent}"
            );
        }
    }
    assert_eq!(value(&path, "SELECT count(*) FROM events")?, before);
    assert_eq!(
        binding(&path, "R1", "qa")?,
        [
            json!("pi"),
            Value::Null,
            json!(1),
            json!(1),
            Value::Null,
            json!("ACTIVE"),
            Value::Null,
            json!("[]")
        ]
    );
    Ok(())
}

#[tokio::test]
async fn a_switch_with_no_attempt_in_flight_rebinds_the_role_in_one_transaction() -> R {
    // Decision 5 (REQ-019 behaviour 2, K2): the blocked event of the epoch, the binding to the new agent at epoch+1 with
    // no session and its config_revision unchanged, team.binding_applied{cause: fallback}, team.binding_fallback, the
    // handover package and its session.start; no configuration change is read from it (team::pending false, the run
    // not behind, no pending in the team view). Then a Grant at the new epoch.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    let evaluated = vec!["pi:not_installed".to_owned(), "claude:usable".to_owned()];
    assert!(
        store
            .staff(change(
                "R1",
                "qa",
                1,
                "pi",
                StaffWrite::Switch {
                    to: "claude".to_owned(),
                    model: None,
                    reason: BlockedReason::AgentUnavailable,
                    line: "pi:not_installed".to_owned(),
                    evaluated: evaluated.clone(),
                    evidence: evidence(1),
                }
            ))
            .await?
    );
    assert_eq!(
        binding(&path, "R1", "qa")?,
        [
            json!("claude"),
            Value::Null,
            json!(2),
            json!(1),
            Value::Null,
            json!("ACTIVE"),
            Value::Null,
            json!("[]")
        ]
    );
    let artifact = format!(
        "{}:{}",
        "1".repeat(64),
        crate::objects::relative_path(&"1".repeat(64))
    );
    let written = events(
        &path,
        "R1",
        &[
            "team.binding_blocked",
            "team.binding_applied",
            "team.binding_fallback",
            "handoff.prepared",
            "team.binding_change_pending",
        ],
    )?;
    assert_eq!(
        written
            .iter()
            .map(|e| (e.0.as_str(), e.1.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                crate::ids::member(&crate::ids::series("R1", "staffing/qa/1"), 1).as_str(),
                "team.binding_blocked"
            ),
            (
                crate::ids::child_id("R1", "team/qa/2").as_str(),
                "team.binding_applied"
            ),
            (
                crate::ids::child_id("R1", "fallback/qa/2").as_str(),
                "team.binding_fallback"
            ),
            (
                crate::ids::child_id("R1", "handoff/qa/2").as_str(),
                "handoff.prepared"
            ),
        ]
    );
    assert_eq!(
        written[0].2,
        json!({"run_id": "R1", "role": "qa", "role_epoch": 1, "agent": "pi", "reason": "AGENT_UNAVAILABLE",
               "missing": ["pi:not_installed"], "evaluated": evaluated, "probe_artifact_id": artifact,
               "probe_sha256": "1".repeat(64), "cause": "staffing"})
    );
    assert_eq!(
        written[1].2,
        json!({"run_id": "R1", "role": "qa", "config_revision": 1, "cause": "fallback",
               "from": {"agent": "pi", "model": null, "role_epoch": 1, "session_id": null},
               "to": {"agent": "claude", "model": null, "role_epoch": 2}})
    );
    assert_eq!(
        written[2].2,
        json!({"run_id": "R1", "role": "qa", "from": "pi", "to": "claude", "evaluated": evaluated,
               "chosen": "claude", "probe_artifact_id": artifact, "probe_sha256": "1".repeat(64)})
    );
    // The seqs of one commit: nothing else between them.
    let seqs = values(
        &path,
        "SELECT seq FROM events WHERE kind IN ('team.binding_blocked', 'team.binding_applied', \
         'team.binding_fallback', 'handoff.prepared') ORDER BY seq",
    )?;
    let first = seqs[0][0].as_i64().ok_or("no seq")?;
    assert_eq!(
        seqs.iter()
            .map(|r| r[0].as_i64().unwrap_or(0) - first)
            .collect::<Vec<_>>(),
        [0, 1, 2, 3]
    );
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT recipient_role || '/' || role_epoch || '/' || json_extract(payload_json, '$.kind') FROM outbox \
                 WHERE message_id = '{}'",
                crate::ids::child_id("R1", "session-start/qa/2")
            )
        )?,
        json!("qa/2/session.start")
    );
    {
        let conn = Connection::open(&path)?;
        assert!(!crate::persistence::team::pending(&conn, "R1", "qa")?);
    }
    let waiting = store.runner_waiting("p1").await?;
    assert!(waiting.team_behind.is_empty(), "{:?}", waiting.team_behind);
    assert_eq!(shown(&path, "qa")?, (Vec::new(), None));
    let qa = staffed(&store, "qa").await?;
    assert_eq!(
        (
            qa.agent.as_str(),
            qa.role_epoch,
            qa.order.clone(),
            qa.preferred.as_str()
        ),
        (
            "claude",
            2,
            vec!["pi".to_owned(), "claude".to_owned()],
            "pi"
        )
    );
    assert!(
        store
            .staff(change("R1", "qa", 2, "claude", grant("2.1.318", "a")))
            .await?
    );
    assert_eq!(
        binding(&path, "R1", "qa")?[4],
        json!(crate::persistence::session_id("R1", "qa", 2))
    );
    Ok(())
}

#[tokio::test]
async fn a_switch_takes_the_model_it_is_given() -> R {
    // Decision 4's model rule (Q8): a switch to the configured agent carries the configured model (the judgment gives
    // it), to any other agent none; the store writes the model of the judgment.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    exec(
        &path,
        "UPDATE role_bindings SET agent = 'codex', model = 'm-other' WHERE run_id = 'R1' AND role = 'developer';",
    )?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "codex",
            StaffWrite::Switch {
                to: "claude".to_owned(),
                model: Some("m-dev".to_owned()),
                reason: BlockedReason::AgentUnavailable,
                line: "codex:not_installed".to_owned(),
                evaluated: vec!["claude:usable".to_owned()],
                evidence: evidence(1),
            },
        ))
        .await?;
    assert_eq!(
        binding(&path, "R1", "developer")?[0..3],
        [json!("claude"), json!("m-dev"), json!(2)]
    );
    Ok(())
}

#[tokio::test]
async fn change_pending_is_written_once_while_the_attempt_runs() -> R {
    // M2T15D-55 (K2): with an attempt of the role in flight, a switch blocks the binding (once) and writes one
    // team.binding_change_pending{cause: fallback, to} however many times it is judged; the role is not dispatched and
    // the team view shows the pending change; once the attempt ended the switch applies.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    in_flight(
        &path,
        "developer",
        "RUNNING",
        Some(&crate::persistence::session_id("R1", "developer", 1)),
    )?;
    let switch = || {
        change(
            "R1",
            "developer",
            1,
            "claude",
            StaffWrite::Switch {
                to: "codex".to_owned(),
                model: None,
                reason: BlockedReason::CompatUnverified,
                line: "claude:COMPAT_UNVERIFIED".to_owned(),
                evaluated: vec!["claude:COMPAT_UNVERIFIED".to_owned()],
                evidence: evidence(1),
            },
        )
    };
    for n in 0..5 {
        let wrote = store.staff(switch()).await?;
        assert_eq!(wrote, n == 0, "judged {} times", n + 1);
    }
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(
        written
            .iter()
            .map(|e| (e.1.as_str(), e.2["cause"].clone()))
            .collect::<Vec<_>>(),
        [
            ("team.binding_blocked", json!("staffing")),
            ("team.binding_change_pending", json!("fallback"))
        ]
    );
    let artifact = format!(
        "{}:{}",
        "1".repeat(64),
        crate::objects::relative_path(&"1".repeat(64))
    );
    assert_eq!(
        written[1].2,
        json!({"run_id": "R1", "role": "developer", "role_epoch": 1, "agent": "claude", "cause": "fallback",
               "to": "codex", "model": null, "evaluated": ["claude:COMPAT_UNVERIFIED"],
               "probe_artifact_id": artifact, "probe_sha256": "1".repeat(64)})
    );
    assert_eq!(
        binding(&path, "R1", "developer")?[0..8],
        [
            json!("claude"),
            json!("m-dev"),
            json!(1),
            json!(1),
            json!(crate::persistence::session_id("R1", "developer", 1)),
            json!("BLOCKED"),
            json!("COMPAT_UNVERIFIED"),
            json!("[\"claude:COMPAT_UNVERIFIED\"]")
        ]
    );
    assert!(!active(&path, "developer")?);
    assert_eq!(
        shown(&path, "developer")?.1,
        Some(
            json!({"cause": "fallback", "agent": "codex", "model": null, "config_revision": 1, "refused": null})
        )
    );
    assert_eq!(
        staffed(&store, "developer").await?.pending,
        Some(PendingChange {
            cause: Cause::Fallback,
            to: "codex".to_owned()
        })
    );
    a0_ends(&path)?;
    assert!(store.staff(switch()).await?);
    assert_eq!(
        binding(&path, "R1", "developer")?[0..3],
        [json!("codex"), Value::Null, json!(2)]
    );
    assert_eq!(
        events(&path, "R1", &STAFFING)?.len(),
        2,
        "already BLOCKED for the reason: no second blocked event"
    );
    assert_eq!(events(&path, "R1", &["team.binding_fallback"])?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_rebind_waits_for_the_attempt_in_flight_and_is_withdrawn_once_the_session_is_fresh() -> R
{
    // Decision 5: a rebind with an attempt of the role in flight writes only team.binding_change_pending{cause: rebind}
    // (once), and the role is not dispatched meanwhile (load_binding's active false); the session fresh again, the
    // Unblock writes team.binding_change_withdrawn and the role is dispatched again. A supervisor attempt counts.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    assert!(active(&path, "developer")?);
    in_flight(&path, "developer", "PREPARED", None)?;
    for n in 0..3 {
        assert_eq!(
            store
                .staff(change("R1", "developer", 1, "claude", StaffWrite::Rebind))
                .await?,
            n == 0
        );
    }
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(
        written
            .iter()
            .map(|e| (e.1.as_str(), e.2["cause"].clone(), e.2["to"].clone()))
            .collect::<Vec<_>>(),
        [(
            "team.binding_change_pending",
            json!("rebind"),
            json!("claude")
        )]
    );
    assert_eq!(
        binding(&path, "R1", "developer")?[2..6],
        [
            json!(1),
            json!(1),
            json!(crate::persistence::session_id("R1", "developer", 1)),
            json!("ACTIVE")
        ]
    );
    assert!(!active(&path, "developer")?, "the rebind is pending");
    assert_eq!(
        shown(&path, "developer")?.1.map(|p| p["cause"].clone()),
        Some(json!("rebind"))
    );
    assert!(
        store
            .staff(change("R1", "developer", 1, "claude", StaffWrite::Unblock))
            .await?
    );
    let written = events(&path, "R1", &STAFFING)?;
    assert_eq!(
        written.iter().map(|e| e.1.as_str()).collect::<Vec<_>>(),
        [
            "team.binding_change_pending",
            "team.binding_change_withdrawn"
        ]
    );
    assert_eq!(
        written[1].2,
        json!({"run_id": "R1", "role": "developer", "role_epoch": 1, "agent": "claude", "cause": "rebind",
               "to": "claude"})
    );
    assert!(active(&path, "developer")?);
    assert_eq!(shown(&path, "developer")?.1, None);
    // Pending again: a new change_pending after the withdrawal.
    assert!(
        store
            .staff(change("R1", "developer", 1, "claude", StaffWrite::Rebind))
            .await?
    );
    assert_eq!(events(&path, "R1", &STAFFING)?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn a_rebind_with_no_attempt_in_flight_is_the_next_epoch_of_the_same_agent_and_model() -> R {
    // Decision 5: the same agent and model at epoch+1 with no session, team.binding_applied{cause: rebind}, the handover
    // and its session.start; no blocked and no fallback event; the config_revision unchanged. Its Grant then records
    // the new identity.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    assert!(
        store
            .staff(change("R1", "developer", 1, "claude", StaffWrite::Rebind))
            .await?
    );
    assert_eq!(
        binding(&path, "R1", "developer")?,
        [
            json!("claude"),
            json!("m-dev"),
            json!(2),
            json!(1),
            Value::Null,
            json!("ACTIVE"),
            Value::Null,
            json!("[]")
        ]
    );
    let applied = events(&path, "R1", &["team.binding_applied"])?;
    assert_eq!(
        applied,
        vec![(
            crate::ids::child_id("R1", "team/developer/2"),
            "team.binding_applied".to_owned(),
            json!({"run_id": "R1", "role": "developer", "config_revision": 1, "cause": "rebind",
                   "from": {"agent": "claude", "model": "m-dev", "role_epoch": 1,
                            "session_id": crate::persistence::session_id("R1", "developer", 1)},
                   "to": {"agent": "claude", "model": "m-dev", "role_epoch": 2}})
        )]
    );
    assert_eq!(events(&path, "R1", &["handoff.prepared"])?.len(), 1);
    assert!(events(&path, "R1", &STAFFING)?.is_empty());
    assert!(events(&path, "R1", &["team.binding_fallback"])?.is_empty());
    assert!(
        store
            .staff(change(
                "R1",
                "developer",
                2,
                "claude",
                grant("2.1.325", "b")
            ))
            .await?
    );
    let session = crate::persistence::session_id("R1", "developer", 2);
    assert_eq!(
        staffed(&store, "developer").await?.session,
        Some(Session {
            agent_version: "2.1.325".to_owned(),
            capabilities: caps(),
            identity: Some(identity("b")),
        })
    );
    assert_eq!(binding(&path, "R1", "developer")?[4], json!(session));
    Ok(())
}

#[tokio::test]
async fn an_assignment_carries_its_sessions_capabilities_and_identity() -> R {
    // Decision 9 (d): the assignment of an attempt names its session's capabilities and the identity it was granted
    // under (none for a session whose record has none).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "a"),
        ))
        .await?;
    let session = crate::persistence::session_id("R1", "developer", 1);
    exec(
        &path,
        &format!(
            "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
               ('old', 'claude', 'developer', '1', '{{\"structured_events\": true}}');
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
               ('T0', 'R1', 's1', 'RUNNING', 2, 'developer', 0, 1, 1), ('T1', 'R1', 's1', 'RUNNING', 2, 'developer', 0, 2, 1);
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, agent)
               VALUES ('A0', 'T0', '{session}', 'PREPARED', 1, 1, 1, 'claude'),
                      ('A1', 'T1', 'old', 'PREPARED', 1, 1, 1, 'claude');"
        ),
    )?;
    let conn = Connection::open(&path)?;
    let a0 = crate::persistence::team::assignment(&conn, "p1", "R1", "T0", "A0", "developer")?;
    assert_eq!(
        (a0.session_capabilities, a0.session_identity),
        (Some(caps()), Some(identity("a")))
    );
    let a1 = crate::persistence::team::assignment(&conn, "p1", "R1", "T1", "A1", "developer")?;
    assert_eq!(
        (a1.session_capabilities, a1.session_identity),
        (Some(json!({"structured_events": true})), None)
    );
    Ok(())
}

// ---- Turn failures (docs/adr/ADR-030.md decision 6, slice S4) ----

use crate::decision::Context;
use crate::persistence::Ending;
use crate::retry::FailureClass;
use crate::staffing::{Compat, Fact, Facts, FileStamp, Judgment, Snapshot, Stamp, Usable};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// The session capabilities every grant of these tests records. Command execution is among them: claude 2.1.318,
/// codex 0.166.0 and pi 1.5.0 each have it under HoH's options as their probes measure it from the recorded help and
/// schema, and the developer and qa require it (docs/adr/ADR-030.md decision 12).
fn caps() -> Value {
    // Every role's needs, the commander's read-only workspace (docs/adr/ADR-032.md decision 4) included.
    json!({"structured_events": true, "resume": true, "command_execution": true, "read_only_workspace": true})
}

/// A usable, verified fact of an agent at `version` with `identity` whose session would record [`caps`].
fn usable(version: &str, identity: AgentIdentity) -> Fact {
    let stamp = FileStamp {
        dev: 1,
        ino: 1,
        ctime_sec: 0,
        ctime_nsec: 0,
    };
    Fact::Usable(Box::new(Usable {
        version: version.to_owned(),
        identity,
        stamp: Stamp {
            launcher: stamp,
            implementation: stamp,
        },
        generation: (1, 1),
        session: caps(),
        granted: crate::decision::granted_capabilities(&caps()),
        compat: Compat::Verified,
    }))
}

fn facts(items: &[(&str, Fact)]) -> Facts {
    items
        .iter()
        .map(|(agent, fact)| ((*agent).to_owned(), fact.clone()))
        .collect()
}

/// The team at revision 1: the developer `agent` falling back to `fallback`; run R1 RUNNING under it on spec s1.
async fn developer_team(store: &Store, path: &PathBuf, agent: &str, fallback: &[&str]) -> R {
    store
        .submit_command(team_change(
            "developer",
            0,
            json!({"developer": {"agent": agent, "fallback": fallback}}),
        ))
        .await?;
    // The project's specification first: StartRun adopts it (docs/adr/ADR-031.md decision 1).
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);",
    )?;
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    exec(
        path,
        "UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R1';",
    )
}

/// Task `task` of `role` RUNNING with attempt `attempt` RUNNING in `session` of `agent` at `epoch` (revision 1).
fn running(path: &PathBuf, task: &str, attempt: &str, role: &str, agent: &str, epoch: i64) -> R {
    let session = crate::persistence::session_id("R1", role, epoch);
    exec(
        path,
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
               ('{task}', 'R1', 's1', 'RUNNING', 2, '{role}', 0, (SELECT count(*) + 1 FROM tasks), 1);
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, agent,
               started_at) VALUES ('{attempt}', '{task}', '{session}', 'RUNNING', {epoch}, 1, 1, '{agent}',
               '2026-11-14T00:00:00Z');"
        ),
    )
}

/// A failed ending of `class` at `at`.
fn failed(class: FailureClass, at: SystemTime) -> Ending {
    Ending {
        success: false,
        detail: format!("{class:?}"),
        class,
        at,
        draw: 0,
        observed: crate::persistence::Observed::default(),
        unknown_effect: false,
    }
}

fn t0() -> R<SystemTime> {
    crate::time::parse_rfc3339("2026-11-14T00:00:00Z").ok_or_else(|| "a time".into())
}

/// What `judgment` of `staffed` under `facts` writes, as the host's staffing writes it (docs/adr/ADR-030.md decision
/// 5; `host::agents::staff`): the evidence object is the store test's own.
fn write_of(staffed: &Staffed, judgment: Judgment, facts: &Facts, n: u8) -> Option<StaffWrite> {
    Some(match judgment {
        Judgment::Nothing => return None,
        Judgment::Grant => match facts.get(&staffed.agent) {
            Some(Fact::Usable(u)) => StaffWrite::Grant {
                agent_version: u.version.clone(),
                capabilities: u.session.clone(),
                identity: u.identity.clone(),
            },
            _ => return None,
        },
        Judgment::Unblock => StaffWrite::Unblock,
        Judgment::Rebind { .. } => StaffWrite::Rebind,
        Judgment::Block { reason, missing } => StaffWrite::Block {
            reason,
            missing,
            evidence: evidence(n),
        },
        Judgment::Switch {
            to,
            model,
            evaluated,
        } => {
            let v = crate::staffing::verdict(
                &staffed.agent,
                &staffed.required,
                facts,
                &staffed.excluded,
            );
            StaffWrite::Switch {
                to,
                model,
                reason: v.reason()?,
                line: v.line(&staffed.agent),
                evaluated,
                evidence: evidence(n),
            }
        }
    })
}

/// Staffing's judgment of run R1's `role` under `facts`, written: the judgment, and whether anything was written.
async fn judge_and_write(store: &Store, role: &str, facts: &Facts, n: u8) -> R<(Judgment, bool)> {
    let staffed = staffed(store, role).await?;
    let judgment = crate::staffing::judge(&staffed, facts, &staffed.excluded);
    let wrote = match write_of(&staffed, judgment.clone(), facts, n) {
        Some(write) => {
            store
                .staff(change(
                    "R1",
                    role,
                    staffed.role_epoch,
                    &staffed.agent,
                    write,
                ))
                .await?
        }
        None => false,
    };
    Ok((judgment, wrote))
}

/// The developer's context of these tests at `now`: what its role requires, and the tick's snapshot `snapshot`.
fn developer_context(now: SystemTime, snapshot: Option<Snapshot>) -> Context {
    Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events".to_owned()].into_iter().collect(),
        )]
        .into(),
        now: Some(now),
        staffing: snapshot.map(Arc::new),
        ..Context::default()
    }
}

fn snapshot(facts: Facts) -> Snapshot {
    Snapshot {
        facts,
        ..Snapshot::default()
    }
}

/// Dispatch run R1's due tasks at `now` (the pace, then the rounds), each attempt taken by the runner (STARTING); the
/// attempts dispatched with their agents.
async fn dispatch(store: &Store, context: Context) -> R<Vec<(String, Option<String>)>> {
    store.pace_run("R1", context.clone()).await?;
    let mut dispatched = Vec::new();
    while let Some(d) = store
        .schedule_round("R1", context.clone())
        .await?
        .dispatched
    {
        let notice = store
            .runner_waiting("p1")
            .await?
            .dispatches
            .into_iter()
            .find(|n| n.attempt_id == d.attempt_id)
            .ok_or("no dispatch notice")?;
        let agent = notice.assignment.agent.clone();
        assert!(store.runner_received(notice, context.now()).await?);
        dispatched.push((d.attempt_id, agent));
    }
    Ok(dispatched)
}

/// Save `value` as `name` in the check's evidence directory, when there is one.
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn test_020_05_a_503_waits_and_a_lost_login_falls_back_to_codex() -> R {
    // AC-020-05 (decision 6, REQ-020 behaviour 7): two stores, a FakeClock from t0. (a) A 503 with Retry-After 30 s
    // waits (READY at t0+30 s, the run WAITING_EXTERNAL) and staffing changes nothing under a snapshot with both agents
    // usable and verified: codex is chosen 0 times. (b) A turn's BLOCKED_AUTH blocks the task at once (no retry) and
    // the binding (cause turn); under a snapshot where claude's login is missing, staffing falls back to codex (epoch
    // 2, model NULL, evaluated [claude:BLOCKED_AUTH, codex:usable]); once codex has its session and its first input, the
    // pace puts the task back READY and a round dispatches it to codex.
    let t0 = t0()?;
    let mut kept = serde_json::Map::new();
    for part in ["a", "b"] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        developer_team(&store, &path, "claude", &["codex"]).await?;
        assert!(
            store
                .staff(change(
                    "R1",
                    "developer",
                    1,
                    "claude",
                    StaffWrite::Grant {
                        agent_version: "2.1.318".to_owned(),
                        capabilities: caps(),
                        identity: identity("claude"),
                    }
                ))
                .await?
        );
        exec(
            &path,
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
             VALUES ('T1', 'R1', 's1', 'READY', 1, 'developer', 0, 1, 1);",
        )?;
        let both = facts(&[
            ("claude", usable("2.1.318", identity("claude"))),
            ("codex", usable("codex-cli 0.166.0", identity("codex"))),
        ]);
        let dispatched =
            dispatch(&store, developer_context(t0, Some(snapshot(both.clone())))).await?;
        let [(attempt, agent)] = dispatched.as_slice() else {
            return Err(format!("{part}: dispatched {dispatched:?}").into());
        };
        assert_eq!(agent.as_deref(), Some("claude"));
        if part == "a" {
            let class = FailureClass::Transient {
                retry_after: Some(Duration::from_secs(30)),
            };
            assert!(store.runner_finished(attempt, failed(class, t0)).await?);
            assert_eq!(
                value(
                    &path,
                    "SELECT state || ' ' || next_retry_at FROM tasks WHERE task_id = 'T1'"
                )?,
                json!("READY 2026-11-14T00:00:30Z")
            );
            store
                .pace_run("R1", developer_context(t0, Some(snapshot(both.clone()))))
                .await?;
            assert_eq!(
                value(&path, "SELECT state FROM runs WHERE run_id = 'R1'")?,
                json!("WAITING_EXTERNAL")
            );
            let (judgment, wrote) = judge_and_write(&store, "developer", &both, 1).await?;
            assert_eq!((judgment, wrote), (Judgment::Nothing, false));
            assert!(events(&path, "R1", &["team.binding_fallback"])?.is_empty());
            assert_eq!(
                value(&path, "SELECT count(*) FROM attempts WHERE agent = 'codex'")?,
                json!(0),
                "codex chosen"
            );
            kept.insert(
                "a".to_owned(),
                json!({
                    "retry": events(&path, "R1", &["task.retry_scheduled"])?.pop().map(|e| e.2),
                    "binding": binding(&path, "R1", "developer")?,
                    "staffing_events": events(&path, "R1", &STAFFING)?.len(),
                }),
            );
            continue;
        }
        assert!(
            store
                .runner_finished(attempt, failed(FailureClass::Auth, t0))
                .await?
        );
        assert_eq!(
            values(
                &path,
                "SELECT state, blocked_reason, next_retry_at FROM tasks WHERE task_id = 'T1'"
            )?,
            [vec![json!("BLOCKED"), json!("BLOCKED_AUTH"), Value::Null]]
        );
        assert!(events(&path, "R1", &["task.retry_scheduled"])?.is_empty());
        assert_eq!(
            binding(&path, "R1", "developer")?[2..8],
            [
                json!(1),
                json!(1),
                json!(crate::persistence::session_id("R1", "developer", 1)),
                json!("BLOCKED"),
                json!("BLOCKED_AUTH"),
                json!("[\"claude:BLOCKED_AUTH\"]")
            ]
        );
        let blocked = events(&path, "R1", &["team.binding_blocked"])?;
        assert_eq!(blocked.len(), 1);
        assert_eq!(
            blocked[0].0,
            crate::ids::member(&crate::ids::series("R1", "staffing/developer/1"), 1)
        );
        assert_eq!(
            blocked[0].2,
            json!({"run_id": "R1", "role": "developer", "role_epoch": 1, "agent": "claude",
                   "reason": "BLOCKED_AUTH", "missing": ["claude:BLOCKED_AUTH"],
                   "evaluated": ["claude:BLOCKED_AUTH"], "cause": "turn", "attempt_id": attempt})
        );
        let lost = facts(&[
            (
                "claude",
                Fact::LoginMissing(format!("turn {attempt} failed BLOCKED_AUTH")),
            ),
            ("codex", usable("codex-cli 0.166.0", identity("codex"))),
        ]);
        let (judgment, wrote) = judge_and_write(&store, "developer", &lost, 2).await?;
        assert!(wrote);
        assert_eq!(
            judgment,
            Judgment::Switch {
                to: "codex".to_owned(),
                model: None,
                evaluated: vec!["claude:BLOCKED_AUTH".to_owned(), "codex:usable".to_owned()],
            }
        );
        assert_eq!(
            binding(&path, "R1", "developer")?[0..3],
            [json!("codex"), Value::Null, json!(2)]
        );
        let fallback = events(&path, "R1", &["team.binding_fallback"])?;
        assert_eq!(fallback.len(), 1);
        assert_eq!(
            (
                fallback[0].2["evaluated"].clone(),
                fallback[0].2["chosen"].clone()
            ),
            (
                json!(["claude:BLOCKED_AUTH", "codex:usable"]),
                json!("codex")
            )
        );
        // Grant, then the first input delivered: the pace puts T1 back READY, a round dispatches it to codex.
        let (judgment, wrote) = judge_and_write(&store, "developer", &lost, 3).await?;
        assert_eq!((judgment, wrote), (Judgment::Grant, true));
        let session = crate::persistence::session_id("R1", "developer", 2);
        let starts = store.session_starts("p1").await?;
        let start = starts
            .iter()
            .find(|s| s.role == "developer" && s.role_epoch == 2)
            .ok_or("no session.start of epoch 2")?;
        assert!(
            store
                .deliver_session_start(&start.message_id, &session)
                .await?
        );
        let later = t0 + Duration::from_secs(1);
        let dispatched = dispatch(
            &store,
            developer_context(later, Some(snapshot(lost.clone()))),
        )
        .await?;
        assert_eq!(
            events(&path, "R1", &["task.unblocked"])?
                .iter()
                .map(|e| e.2.clone())
                .collect::<Vec<_>>(),
            [
                json!({"task_id": "T1", "reason": "BLOCKED_AUTH", "role": "developer", "role_epoch": 2})
            ]
        );
        let [(second, agent)] = dispatched.as_slice() else {
            return Err(format!("dispatched {dispatched:?}").into());
        };
        assert_eq!(agent.as_deref(), Some("codex"));
        assert_eq!(
            value(
                &path,
                &format!("SELECT session_id FROM attempts WHERE attempt_id = '{second}'")
            )?,
            json!(session)
        );
        kept.insert(
            "b".to_owned(),
            json!({
                "selection": fallback[0].2,
                "task": values(&path, "SELECT state, blocked_reason, attempt_count, next_retry_at FROM tasks")?,
                "events": events(&path, "R1", &["task.blocked", "team.binding_blocked", "team.binding_fallback",
                    "team.binding_applied", "team.session_bound", "task.unblocked"])?
                    .into_iter().map(|e| json!([e.0, e.1, e.2])).collect::<Vec<_>>(),
            }),
        );
    }
    keep("test_020_05.json", &Value::Object(kept))
}

#[tokio::test]
async fn two_candidates_excluded_by_model_failures_switch_twice_then_block() -> R {
    // Decision 6 (K5): each switch needs a failed turn first; the developer claude falling back to codex then pi, each
    // agent's turn failing MODEL_UNAVAILABLE: two switches, then BLOCKED(MODEL_UNAVAILABLE) with every agent's line,
    // and no further switch however often it is judged again.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    developer_team(&store, &path, "claude", &["codex", "pi"]).await?;
    let all = facts(&[
        ("claude", usable("2.1.318", identity("claude"))),
        ("codex", usable("codex-cli 0.166.0", identity("codex"))),
        ("pi", usable("1.5.0", identity("pi"))),
    ]);
    let t0 = t0()?;
    for (n, agent) in ["claude", "codex", "pi"].into_iter().enumerate() {
        let epoch = i64::try_from(n)? + 1;
        let (judgment, wrote) = judge_and_write(&store, "developer", &all, 10).await?;
        assert_eq!((judgment, wrote), (Judgment::Grant, true), "{agent}");
        let (task, attempt) = (format!("T{n}"), format!("A{n}"));
        running(&path, &task, &attempt, "developer", agent, epoch)?;
        assert!(
            store
                .runner_finished(&attempt, failed(FailureClass::Model, t0))
                .await?
        );
        assert_eq!(
            binding(&path, "R1", "developer")?[5..8],
            [
                json!("BLOCKED"),
                json!("MODEL_UNAVAILABLE"),
                json!(format!("[\"{agent}:MODEL_UNAVAILABLE\"]"))
            ],
            "{agent}"
        );
        let (judgment, _) = judge_and_write(&store, "developer", &all, 11).await?;
        match agent {
            "claude" => {
                assert!(matches!(judgment, Judgment::Switch { ref to, .. } if to == "codex"))
            }
            "codex" => assert!(matches!(judgment, Judgment::Switch { ref to, .. } if to == "pi")),
            _ => assert_eq!(
                judgment,
                Judgment::Block {
                    reason: BlockedReason::ModelUnavailable,
                    missing: vec![
                        "claude:MODEL_UNAVAILABLE".to_owned(),
                        "codex:MODEL_UNAVAILABLE".to_owned(),
                        "pi:MODEL_UNAVAILABLE".to_owned()
                    ],
                }
            ),
        }
    }
    assert_eq!(events(&path, "R1", &["team.binding_fallback"])?.len(), 2);
    assert_eq!(
        binding(&path, "R1", "developer")?[..8],
        [
            json!("pi"),
            Value::Null,
            json!(3),
            json!(1),
            json!(crate::persistence::session_id("R1", "developer", 3)),
            json!("BLOCKED"),
            json!("MODEL_UNAVAILABLE"),
            json!(
                "[\"claude:MODEL_UNAVAILABLE\",\"codex:MODEL_UNAVAILABLE\",\"pi:MODEL_UNAVAILABLE\"]"
            )
        ]
    );
    for n in 0..3 {
        let (_, wrote) = judge_and_write(&store, "developer", &all, 20 + n).await?;
        assert!(!wrote, "judged again {n}");
    }
    assert_eq!(events(&path, "R1", &["team.binding_fallback"])?.len(), 2);
    assert_eq!(events(&path, "R1", &["team.binding_excluded"])?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn an_exclusion_does_not_apply_to_an_identity_of_the_same_version_and_another_hash() -> R {
    // Decision 6 (K8): an exclusion record applies only to its version and whole identity; claude at the same version
    // with another implementation hash is usable: its binding is rebound (its session was another identity's), not
    // switched.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    developer_team(&store, &path, "claude", &["codex"]).await?;
    let first = facts(&[
        ("claude", usable("2.1.318", identity("claude"))),
        ("codex", usable("codex-cli 0.166.0", identity("codex"))),
    ]);
    judge_and_write(&store, "developer", &first, 1).await?;
    running(&path, "T0", "A0", "developer", "claude", 1)?;
    assert!(
        store
            .runner_finished("A0", failed(FailureClass::Capability, t0()?))
            .await?
    );
    let excluded = events(&path, "R1", &["team.binding_excluded"])?;
    assert_eq!(excluded.len(), 1);
    assert_eq!(excluded[0].2["reason"], json!("BLOCKED_CAPABILITY"));
    let rebuilt = AgentIdentity {
        implementation_sha256: "3".repeat(64),
        ..identity("claude")
    };
    let second = facts(&[
        ("claude", usable("2.1.318", rebuilt.clone())),
        ("codex", usable("codex-cli 0.166.0", identity("codex"))),
    ]);
    let staffed = staffed(&store, "developer").await?;
    assert_eq!(staffed.excluded.len(), 1);
    assert_eq!(
        crate::staffing::verdict("claude", &staffed.required, &second, &staffed.excluded),
        crate::staffing::Verdict::Usable
    );
    assert_eq!(
        crate::staffing::verdict("claude", &staffed.required, &first, &staffed.excluded),
        crate::staffing::Verdict::Excluded(crate::staffing::Exclusion::BlockedCapability)
    );
    let (judgment, wrote) = judge_and_write(&store, "developer", &second, 2).await?;
    assert_eq!((judgment, wrote), (Judgment::Rebind { model: None }, true));
    assert_eq!(
        binding(&path, "R1", "developer")?[0..3],
        [json!("claude"), Value::Null, json!(2)]
    );
    assert!(events(&path, "R1", &["team.binding_fallback"])?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_supervisor_attempts_capability_or_model_failure_blocks_the_commander_and_is_excluded()
-> R {
    // M2T15D-05 (decision 6): a supervision turn's Capability or Model failure blocks the commander binding (cause
    // turn) and writes its exclusion record in the transaction that cancels the supervision task and records
    // supervisor.turn_failed.
    for (class, reason) in [
        (FailureClass::Model, "MODEL_UNAVAILABLE"),
        (FailureClass::Capability, "BLOCKED_CAPABILITY"),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        store
            .submit_command(team_change(
                "commander",
                0,
                json!({"commander": {"agent": "pi", "fallback": ["claude"]}}),
            ))
            .await?;
        // The project's specification first: StartRun adopts it (docs/adr/ADR-031.md decision 1).
        exec(
            &path,
            "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);",
        )?;
        store
            .submit_command(start("R1", Config::default().budget))
            .await?;
        exec(
            &path,
            "UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R1';",
        )?;
        assert!(
            store
                .staff(change(
                    "R1",
                    "commander",
                    1,
                    "pi",
                    StaffWrite::Grant {
                        agent_version: "1.5.0".to_owned(),
                        capabilities: caps(),
                        identity: identity("pi"),
                    }
                ))
                .await?
        );
        running(&path, "S1", "SA1", "commander", "pi", 1)?;
        assert!(store.runner_finished("SA1", failed(class, t0()?)).await?);
        assert_eq!(
            value(&path, "SELECT state FROM tasks WHERE task_id = 'S1'")?,
            json!("CANCELLED")
        );
        assert_eq!(events(&path, "R1", &["supervisor.turn_failed"])?.len(), 1);
        assert_eq!(
            binding(&path, "R1", "commander")?[5..8],
            [
                json!("BLOCKED"),
                json!(reason),
                json!(format!("[\"pi:{reason}\"]"))
            ]
        );
        let blocked = events(&path, "R1", &["team.binding_blocked"])?;
        assert_eq!(
            (blocked.len(), blocked[0].2["cause"].clone()),
            (1, json!("turn"))
        );
        let excluded = events(&path, "R1", &["team.binding_excluded"])?;
        assert_eq!(excluded.len(), 1);
        assert_eq!(excluded[0].0, crate::ids::child_id("SA1", "excluded"));
        assert_eq!(
            excluded[0].2,
            json!({"run_id": "R1", "role": "commander", "config_revision": 1, "agent": "pi",
                   "agent_version": "1.5.0", "identity": identity("pi"), "reason": reason, "attempt_id": "SA1"})
        );
        // The commander's fallback is evaluated with it: staffing switches to claude.
        let both = facts(&[
            ("pi", usable("1.5.0", identity("pi"))),
            ("claude", usable("2.1.318", identity("claude"))),
        ]);
        let (judgment, wrote) = judge_and_write(&store, "commander", &both, 1).await?;
        assert!(wrote);
        assert!(
            matches!(judgment, Judgment::Switch { ref to, .. } if to == "claude"),
            "{judgment:?}"
        );
    }
    Ok(())
}

/// Run R1's developer pi falling back to claude, its session granted under `identity` (none: a team.session_bound
/// written before identities, M2T15D-103), attempt A1 of task T1 RUNNING with its work OBSERVED as failed by `class`.
async fn observed_failure(
    store: &Store,
    path: &PathBuf,
    identity: Option<AgentIdentity>,
    class: FailureClass,
) -> R {
    developer_team(store, path, "pi", &["claude"]).await?;
    assert!(
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "pi",
                StaffWrite::Grant {
                    agent_version: "1.5.0".to_owned(),
                    capabilities: caps(),
                    identity: self::identity("pi"),
                }
            ))
            .await?
    );
    if identity.is_none() {
        exec(
            path,
            "UPDATE events SET payload_json = json_remove(payload_json, '$.identity') WHERE kind = 'team.session_bound';",
        )?;
    }
    running(path, "T1", "A1", "developer", "pi", 1)?;
    let key = crate::reconcile::work_key("A1");
    let class = class.to_json();
    exec(
        path,
        &format!(
            "INSERT INTO effects(effect_key, attempt_id, state) VALUES ('{key}', 'A1', 'OBSERVED');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', '{key}/effect.observed', 'R1', 'effect.observed',
                json_object('effect_key', '{key}', 'detail', json_object('success', json('false'), 'detail',
                  'Model not found', 'class', '{}', 'retry_after_ms', NULL, 'ended_at', '2026-11-14T00:00:05Z')),
                '2026-11-14T00:00:05Z');",
            class["class"].as_str().unwrap_or_default()
        ),
    )
}

/// The reconcile of a new host (decision 6's crash recovery): A1's process gone, disposed of by its recorded class.
async fn reconcile_a1(store: &Store) -> R<usize> {
    use crate::reconcile::{Effect, Process, Unfinished, dispose};
    let at = t0()? + Duration::from_secs(60);
    let rows = store.reconcile_scan("p1", at).await?;
    for row in &rows {
        let process = Process::Gone {
            basis: "no_process".to_owned(),
        };
        let disposition = dispose(&Unfinished {
            attempt_id: row.attempt_id.clone(),
            state: row.state,
            task_state: row.task_state,
            process: process.clone(),
            effects: row
                .effects
                .iter()
                .map(|e| Effect {
                    key: e.key.clone(),
                    state: e.state,
                    observation: None,
                })
                .collect(),
            lease_expired: row.lease_expired,
            work_outcome: row.work_outcome,
            writer: row.writer,
            verifier: row.verifier,
        });
        store
            .reconcile_apply(row.clone(), disposition, process, "reconcile", at, 0)
            .await?;
    }
    Ok(rows.len())
}

/// Every event's id, kind and payload text of run R1, in seq order.
fn all_events(path: &PathBuf) -> R<Vec<Vec<Value>>> {
    values(
        path,
        "SELECT event_id, kind, payload_json FROM events WHERE run_id = 'R1' ORDER BY seq",
    )
}

#[tokio::test]
async fn a_failure_recovered_after_observed_is_excluded_like_a_settled_one() -> R {
    // M2T15D-103 (decision 6, K8): the same prefix in two stores, A1's Model failure OBSERVED. Store 1 is recovered by
    // a new host's reconcile (fail_task with the recorded class), store 2 settled by its own host (runner_finished):
    // both block the binding (cause turn) and write the same exclusion record, byte for byte, its version and identity
    // the session's; under the same snapshot both switch to claude once. A second reconcile of store 1 writes nothing.
    let mut stores = Vec::new();
    for recovered in [true, false] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        observed_failure(&store, &path, Some(identity("pi")), FailureClass::Model).await?;
        if recovered {
            assert_eq!(reconcile_a1(&store).await?, 1);
        } else {
            assert!(
                store
                    .runner_finished(
                        "A1",
                        failed(FailureClass::Model, t0()? + Duration::from_secs(5))
                    )
                    .await?
            );
        }
        assert_eq!(
            binding(&path, "R1", "developer")?[5..8],
            [
                json!("BLOCKED"),
                json!("MODEL_UNAVAILABLE"),
                json!("[\"pi:MODEL_UNAVAILABLE\"]")
            ]
        );
        assert_eq!(
            events(&path, "R1", &["team.binding_blocked"])?[0].2["cause"],
            json!("turn")
        );
        let excluded = values(
            &path,
            "SELECT event_id, payload_json FROM events WHERE kind = 'team.binding_excluded'",
        )?;
        assert_eq!(excluded.len(), 1);
        assert_eq!(
            excluded[0][0],
            json!(crate::ids::child_id("A1", "excluded"))
        );
        let payload: Value = serde_json::from_str(excluded[0][1].as_str().ok_or("text")?)?;
        assert_eq!(
            (
                payload["agent_version"].clone(),
                payload["identity"].clone()
            ),
            (json!("1.5.0"), serde_json::to_value(identity("pi"))?)
        );
        let both = facts(&[
            ("pi", usable("1.5.0", identity("pi"))),
            ("claude", usable("2.1.318", identity("claude"))),
        ]);
        let (judgment, wrote) = judge_and_write(&store, "developer", &both, 1).await?;
        assert!(wrote);
        assert!(matches!(judgment, Judgment::Switch { ref to, .. } if to == "claude"));
        assert_eq!(
            binding(&path, "R1", "developer")?[0..3],
            [json!("claude"), Value::Null, json!(2)]
        );
        assert_eq!(events(&path, "R1", &["team.binding_fallback"])?.len(), 1);
        stores.push((dir, store, path, excluded[0][1].clone()));
    }
    assert_eq!(stores[0].3, stores[1].3, "the two records differ");
    let (_, store, path, _) = &stores[0];
    let before = all_events(path)?;
    assert_eq!(reconcile_a1(store).await?, 0, "a second reconcile found A1");
    assert_eq!(all_events(path)?, before);
    Ok(())
}

#[tokio::test]
async fn a_recovered_failure_of_a_session_without_identity_blocks_and_rebinds() -> R {
    // M2T15D-103: a session granted before identities were recorded: the recovered Model failure blocks the binding,
    // writes team.exclusion_skipped instead of an exclusion record; staffing then rebinds pi (its session stale), no
    // fallback, and the new session carries its identity.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    observed_failure(&store, &path, None, FailureClass::Model).await?;
    assert_eq!(reconcile_a1(&store).await?, 1);
    assert_eq!(
        binding(&path, "R1", "developer")?[5..7],
        [json!("BLOCKED"), json!("MODEL_UNAVAILABLE")]
    );
    assert!(events(&path, "R1", &["team.binding_excluded"])?.is_empty());
    let skipped = events(&path, "R1", &["team.exclusion_skipped"])?;
    assert_eq!(
        skipped
            .iter()
            .map(|e| (e.0.clone(), e.2.clone()))
            .collect::<Vec<_>>(),
        [(
            crate::ids::child_id("A1", "exclusion_skipped"),
            json!({"attempt_id": "A1", "session_id": crate::persistence::session_id("R1", "developer", 1),
                   "reason": "the session has no identity"})
        )]
    );
    let both = facts(&[
        ("pi", usable("1.5.0", identity("pi"))),
        ("claude", usable("2.1.318", identity("claude"))),
    ]);
    let (judgment, wrote) = judge_and_write(&store, "developer", &both, 1).await?;
    assert_eq!((judgment, wrote), (Judgment::Rebind { model: None }, true));
    assert!(events(&path, "R1", &["team.binding_fallback"])?.is_empty());
    let (judgment, wrote) = judge_and_write(&store, "developer", &both, 2).await?;
    assert_eq!((judgment, wrote), (Judgment::Grant, true));
    let bound = events(&path, "R1", &["team.session_bound"])?;
    assert_eq!(
        bound.last().map(|e| e.2["identity"].clone()),
        Some(serde_json::to_value(identity("pi"))?)
    );
    Ok(())
}

// ---- A vacant role's tasks: marking and recovery in the pace (docs/adr/ADR-030.md decision 7, slice S5) ----

/// The context of the pace and the rounds at `now`: every role of [`team_and_run`]'s team requiring structured events
/// (the reviewer resume too), and the tick's snapshot `snapshot`.
fn team_context(now: SystemTime, snapshot: Option<Snapshot>) -> Context {
    let needs = |extra: &[&str]| {
        std::iter::once("structured_events")
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect()
    };
    Context {
        required_capabilities: [
            ("developer".to_owned(), needs(&[])),
            ("qa".to_owned(), needs(&[])),
            ("reviewer".to_owned(), needs(&["resume"])),
        ]
        .into(),
        now: Some(now),
        staffing: snapshot.map(Arc::new),
        ..Context::default()
    }
}

/// Task `task` of `role` in `state` (BLOCKED for `reason`, if any), never attempted.
fn task(path: &PathBuf, task: &str, role: &str, state: &str, reason: Option<&str>) -> R {
    let reason = reason.map_or("NULL".to_owned(), |r| format!("'{r}'"));
    exec(
        path,
        &format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate, \
               blocked_reason) VALUES ('{task}', 'R1', 's1', '{state}', 1, '{role}', 0, \
               (SELECT count(*) + 1 FROM tasks), 1, {reason});"
        ),
    )
}

/// Task `task`'s state, blocked reason, attempt count and next retry, and its attempts.
fn task_row(path: &PathBuf, task: &str) -> R<Vec<Value>> {
    let mut row = values(
        path,
        &format!(
            "SELECT state, blocked_reason, attempt_count, next_retry_at, \
             (SELECT count(*) FROM attempts WHERE task_id = '{task}') FROM tasks WHERE task_id = '{task}'"
        ),
    )?;
    row.pop().ok_or_else(|| format!("no task {task}").into())
}

#[tokio::test]
async fn every_vacancy_reason_marks_a_due_task_and_recovers_it_once_dispatchable() -> R {
    // Decision 7 (K4): for each reason of V, a due task of a role whose binding is BLOCKED for it is marked BLOCKED
    // for it (task.blocked{cause: vacancy, missing}, R2's id), with no attempt, its attempt_count and next_retry_at
    // untouched and cleared; the run, with nothing else to move on, is BLOCKED for it. Once the binding is ACTIVE with
    // its session fresh and verified in the snapshot, the pace puts the task back READY (task.unblocked{task_id,
    // reason, role, role_epoch}, blocked_reason and next_retry_at cleared before the CAS) and the run works again; a
    // NEED_DECISION task beside it is never recovered.
    let t0 = t0()?;
    for reason in crate::staffing::VACANCY {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        team_and_run(&store, &path).await?;
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                grant("2.1.318", "claude"),
            ))
            .await?;
        let line = format!("claude:{}", reason.as_str());
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                StaffWrite::Block {
                    reason,
                    missing: vec![line.clone()],
                    evidence: evidence(1),
                },
            ))
            .await?;
        task(&path, "T1", "developer", "READY", None)?;
        task(&path, "N1", "developer", "BLOCKED", Some("NEED_DECISION"))?;
        exec(
            &path,
            "UPDATE tasks SET attempt_count = 2, next_retry_at = '2026-11-13T00:00:00Z' WHERE task_id = 'T1';",
        )?;
        let verified = facts(&[("claude", usable("2.1.318", identity("claude")))]);
        let paced = store
            .pace_run("R1", team_context(t0, Some(snapshot(verified.clone()))))
            .await?;
        assert_eq!(paced.vacant, ["T1"], "{reason:?}");
        assert_eq!(
            task_row(&path, "T1")?,
            [
                json!("BLOCKED"),
                json!(reason.as_str()),
                json!(2),
                Value::Null,
                json!(0)
            ],
            "{reason:?}"
        );
        let blocked = events(&path, "R1", &["task.blocked"])?;
        assert_eq!(
            blocked
                .iter()
                .map(|e| (e.0.clone(), e.2.clone()))
                .collect::<Vec<_>>(),
            [(
                crate::ids::child_id("T1", "blocked/1"),
                json!({"task_id": "T1", "reason": reason.as_str(), "missing": [line], "cause": "vacancy"})
            )],
            "{reason:?}"
        );
        assert_eq!(
            value(&path, "SELECT state FROM runs WHERE run_id = 'R1'")?,
            json!("BLOCKED"),
            "{reason:?}"
        );
        // Still vacant: nothing is recovered.
        let again = store
            .pace_run("R1", team_context(t0, Some(snapshot(verified.clone()))))
            .await?;
        assert!(again.recovered.is_empty(), "{reason:?}");
        assert!(
            store
                .staff(change("R1", "developer", 1, "claude", StaffWrite::Unblock))
                .await?
        );
        let paced = store
            .pace_run("R1", team_context(t0, Some(snapshot(verified.clone()))))
            .await?;
        assert_eq!(paced.recovered, ["T1"], "{reason:?}");
        assert_eq!(
            task_row(&path, "T1")?,
            [json!("READY"), Value::Null, json!(2), Value::Null, json!(0)],
            "{reason:?}"
        );
        let unblocked = events(&path, "R1", &["task.unblocked"])?;
        assert_eq!(
            unblocked
                .iter()
                .map(|e| (e.0.clone(), e.2.clone()))
                .collect::<Vec<_>>(),
            [(
                crate::ids::child_id("T1", "unblocked/2"),
                json!({"task_id": "T1", "reason": reason.as_str(), "role": "developer", "role_epoch": 1})
            )],
            "{reason:?}"
        );
        assert_eq!(
            task_row(&path, "N1")?[0..2],
            [json!("BLOCKED"), json!("NEED_DECISION")],
            "{reason:?}"
        );
        assert_ne!(
            value(&path, "SELECT state FROM runs WHERE run_id = 'R1'")?,
            json!("BLOCKED"),
            "{reason:?}"
        );
        let dispatched = dispatch_r1(&store, team_context(t0, Some(snapshot(verified)))).await?;
        assert_eq!(dispatched.len(), 1, "{reason:?}");
    }
    Ok(())
}

/// Run R1's rounds at `context` until nothing more is dispatched: the attempts dispatched.
async fn dispatch_r1(store: &Store, context: Context) -> R<Vec<String>> {
    let mut dispatched = Vec::new();
    while let Some(d) = store
        .schedule_round("R1", context.clone())
        .await?
        .dispatched
    {
        dispatched.push(d.attempt_id);
    }
    Ok(dispatched)
}

#[tokio::test]
async fn a_waiting_runs_verifier_is_unblocked_in_the_same_pace() -> R {
    // M2T15D-54 (decision 7's position): run R1 WAITING_EXTERNAL (another task's retry in the future), its qa task
    // BLOCKED_AUTH; qa's binding recovered, its session fresh and verified in the snapshot: the one pace writes
    // task.unblocked, the task READY and due, and the run works again.
    let t0 = t0()?;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change("R1", "qa", 1, "pi", grant("1.5.0", "pi")))
        .await?;
    task(&path, "V1", "qa", "BLOCKED", Some("BLOCKED_AUTH"))?;
    task(&path, "T2", "developer", "READY", None)?;
    exec(
        &path,
        "UPDATE tasks SET next_retry_at = '2026-11-14T01:00:00Z', attempt_count = 1 WHERE task_id = 'T2';
         UPDATE runs SET state = 'WAITING_EXTERNAL', resume_state = 'RUNNING' WHERE run_id = 'R1';",
    )?;
    let verified = facts(&[("pi", usable("1.5.0", identity("pi")))]);
    let paced = store
        .pace_run("R1", team_context(t0, Some(snapshot(verified))))
        .await?;
    assert_eq!(paced.recovered, ["V1"]);
    assert!(paced.woke, "{paced:?}");
    assert_eq!(
        task_row(&path, "V1")?,
        [json!("READY"), Value::Null, json!(0), Value::Null, json!(0)]
    );
    assert_eq!(
        events(&path, "R1", &["task.unblocked"])?[0].2,
        json!({"task_id": "V1", "reason": "BLOCKED_AUTH", "role": "qa", "role_epoch": 1})
    );
    assert_eq!(
        value(&path, "SELECT state FROM runs WHERE run_id = 'R1'")?,
        json!("RUNNING")
    );
    Ok(())
}

#[tokio::test]
async fn compat_unverified_is_recovered_only_by_a_verified_fresh_session_in_the_snapshot() -> R {
    // M2T15D-51 (decision 7): a task BLOCKED(COMPAT_UNVERIFIED) of a role whose binding is ACTIVE with its session stays
    // BLOCKED while the snapshot lacks the agent, its fact is not verified, the session's version or identity is not
    // the fact's, or there is no snapshot at all (no registry, no gate); a verified fact the session is fresh for
    // recovers it.
    let t0 = t0()?;
    let pending = match usable("2.1.318", identity("claude")) {
        Fact::Usable(mut u) => {
            u.compat = Compat::Pending("not run".to_owned());
            Fact::Usable(u)
        }
        other => other,
    };
    let cases: [(&str, Option<Snapshot>, bool); 6] = [
        (
            "the snapshot lacks claude",
            Some(snapshot(Facts::new())),
            false,
        ),
        (
            "claude is not verified",
            Some(snapshot(facts(&[("claude", pending)]))),
            false,
        ),
        (
            "another version",
            Some(snapshot(facts(&[(
                "claude",
                usable("2.1.325", identity("claude")),
            )]))),
            false,
        ),
        (
            "another identity",
            Some(snapshot(facts(&[(
                "claude",
                usable(
                    "2.1.318",
                    AgentIdentity {
                        implementation_path: "/opt/claude/versions/B".to_owned(),
                        ..identity("claude")
                    },
                ),
            )]))),
            false,
        ),
        ("no snapshot", None, false),
        (
            "verified and fresh",
            Some(snapshot(facts(&[(
                "claude",
                usable("2.1.318", identity("claude")),
            )]))),
            true,
        ),
    ];
    for (case, snapshot, recovered) in cases {
        let dir = tempfile::tempdir()?;
        let (store, path) = open(&dir)?;
        team_and_run(&store, &path).await?;
        store
            .staff(change(
                "R1",
                "developer",
                1,
                "claude",
                grant("2.1.318", "claude"),
            ))
            .await?;
        task(
            &path,
            "T1",
            "developer",
            "BLOCKED",
            Some("COMPAT_UNVERIFIED"),
        )?;
        let paced = store.pace_run("R1", team_context(t0, snapshot)).await?;
        assert_eq!(paced.recovered.len(), usize::from(recovered), "{case}");
        assert_eq!(
            task_row(&path, "T1")?[0],
            json!(if recovered { "READY" } else { "BLOCKED" }),
            "{case}"
        );
        assert_eq!(
            events(&path, "R1", &["task.unblocked"])?.len(),
            usize::from(recovered),
            "{case}"
        );
    }
    // Without a snapshot the other reasons of V are judged by the binding and the session's capabilities alone.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    store
        .staff(change(
            "R1",
            "developer",
            1,
            "claude",
            grant("2.1.318", "claude"),
        ))
        .await?;
    task(&path, "T1", "developer", "BLOCKED", Some("BLOCKED_AUTH"))?;
    let paced = store.pace_run("R1", team_context(t0, None)).await?;
    assert_eq!(paced.recovered, ["T1"]);
    Ok(())
}

#[tokio::test]
async fn a_blocked_capability_verifier_is_dispatched_once_its_rebound_session_has_the_capability()
-> R {
    // CR-018 (docs/cr/CR-018-verification-hand-overs.md:30; decisions 4 and 7): the reviewer's session lacks resume:
    // staffing blocks the binding BLOCKED_CAPABILITY and the pace marks its due verification task. codex upgraded (its
    // session capabilities with resume, another version and identity, verified): Rebind at epoch 2, Grant, the first
    // input delivered, and the pace puts the task back READY for a round to dispatch; a NEED_DECISION task stays BLOCKED
    // and the failure items attached to the reworked task are still its rework context after the recovery.
    let t0 = t0()?;
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    team_and_run(&store, &path).await?;
    let old = json!({"structured_events": true});
    let codex = |version: &str, tag: &str, session: &Value| match usable(version, identity(tag)) {
        Fact::Usable(mut u) => {
            u.session = session.clone();
            u.granted = crate::decision::granted_capabilities(session);
            Fact::Usable(u)
        }
        other => other,
    };
    let before = facts(&[("codex", codex("codex-cli 0.166.0", "codex-a", &old))]);
    // A session granted before (its capabilities what codex's session said then).
    assert!(
        store
            .staff(change(
                "R1",
                "reviewer",
                1,
                "codex",
                StaffWrite::Grant {
                    agent_version: "codex-cli 0.166.0".to_owned(),
                    capabilities: old.clone(),
                    identity: identity("codex-a"),
                }
            ))
            .await?
    );
    let (judgment, wrote) = judge_and_write(&store, "reviewer", &before, 2).await?;
    assert_eq!(
        (judgment, wrote),
        (
            Judgment::Block {
                reason: BlockedReason::BlockedCapability,
                missing: vec!["codex:CAPABILITY_UNSUPPORTED:resume".to_owned()],
            },
            true
        )
    );
    task(&path, "V1", "reviewer", "READY", None)?;
    task(&path, "N1", "reviewer", "BLOCKED", Some("NEED_DECISION"))?;
    // V1's rework context: the failure items a rework took from a round on candidate C1.
    exec(
        &path,
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, created_at) VALUES ('C1', 'R1', printf('%040d', 1), printf('%040d', 2),
           printf('%064d', 1), printf('%064d', 2), printf('%064d', 3), '/repo', '2026-11-14T00:00:00Z');
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
           ('p1', 'round-1/opened', 'R1', 'verification.round_opened',
            '{\"round_id\": \"round-1\", \"candidate_id\": \"C1\", \"source_task_ids\": [\"V1\"]}', '2026-11-14T00:00:00Z'),
           ('p1', 'round-1/consumed', 'R1', 'verification.failures_consumed',
            '{\"round_id\": \"round-1\", \"task_id\": \"V1\", \"by\": \"rework\", \"items\": [\"check:unit\"]}',
            '2026-11-14T00:00:00Z');",
    )?;
    let rework = |path: &PathBuf| -> R<Option<crate::persistence::ReworkContext>> {
        let conn = Connection::open(path)?;
        Ok(crate::persistence::context::rework(&conn, "V1")?)
    };
    let attached = rework(&path)?;
    assert!(attached.is_some(), "no rework context seeded");
    let paced = store
        .pace_run("R1", team_context(t0, Some(snapshot(before.clone()))))
        .await?;
    assert_eq!(paced.vacant, ["V1"]);
    assert_eq!(
        task_row(&path, "V1")?[0..2],
        [json!("BLOCKED"), json!("BLOCKED_CAPABILITY")]
    );
    let upgraded = facts(&[("codex", usable("codex-cli 0.167.0", identity("codex-b")))]);
    let (judgment, wrote) = judge_and_write(&store, "reviewer", &upgraded, 3).await?;
    assert_eq!((judgment, wrote), (Judgment::Rebind { model: None }, true));
    let paced = store
        .pace_run("R1", team_context(t0, Some(snapshot(upgraded.clone()))))
        .await?;
    assert!(
        paced.recovered.is_empty(),
        "recovered before its new session"
    );
    let (judgment, wrote) = judge_and_write(&store, "reviewer", &upgraded, 4).await?;
    assert_eq!((judgment, wrote), (Judgment::Grant, true));
    let paced = store
        .pace_run("R1", team_context(t0, Some(snapshot(upgraded.clone()))))
        .await?;
    assert!(
        paced.recovered.is_empty(),
        "recovered before its first input"
    );
    let session = crate::persistence::session_id("R1", "reviewer", 2);
    let starts = store.session_starts("p1").await?;
    let start = starts
        .iter()
        .find(|s| s.role == "reviewer" && s.role_epoch == 2)
        .ok_or("no session.start of epoch 2")?;
    assert!(
        store
            .deliver_session_start(&start.message_id, &session)
            .await?
    );
    let paced = store
        .pace_run("R1", team_context(t0, Some(snapshot(upgraded.clone()))))
        .await?;
    assert_eq!(paced.recovered, ["V1"]);
    assert_eq!(
        events(&path, "R1", &["task.unblocked"])?[0].2,
        json!({"task_id": "V1", "reason": "BLOCKED_CAPABILITY", "role": "reviewer", "role_epoch": 2})
    );
    assert_eq!(
        task_row(&path, "N1")?[0..2],
        [json!("BLOCKED"), json!("NEED_DECISION")]
    );
    assert_eq!(rework(&path)?, attached);
    let dispatched = dispatch_r1(&store, team_context(t0, Some(snapshot(upgraded)))).await?;
    assert_eq!(dispatched.len(), 1);
    assert_eq!(
        value(
            &path,
            &format!(
                "SELECT task_id || ' ' || session_id FROM attempts WHERE attempt_id = '{}'",
                dispatched[0]
            )
        )?,
        json!(format!("V1 {session}"))
    );
    Ok(())
}
