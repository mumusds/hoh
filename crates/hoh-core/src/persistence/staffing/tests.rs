//! Staffing's store side against docs/adr/ADR-030.md decisions 1, 4 and 5 (the test plan's "store" bullet and
//! `staffing_ids_of_a_long_run_are_distinct`): the bindings read for the runner, each judgment's writes and events in
//! one transaction, the condition on the judged binding, the pending changes while an attempt is in flight, and the
//! staffing series' ids.

use crate::config::{Budget, Config};
use crate::persistence::{
    Evidence, NewEvent, Options, RunTarget, StaffChange, StaffWrite, Store, SubmitCommand, selftest,
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

fn grant(agent_version: &str, tag: &str) -> StaffWrite {
    StaffWrite::Grant {
        agent_version: agent_version.to_owned(),
        capabilities: json!({"structured_events": true, "resume": true}),
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
    store
        .submit_command(start("R1", Config::default().budget))
        .await?;
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R1';",
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
            required: ["structured_events".to_owned()].into(),
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
            capabilities: json!({"structured_events": true, "resume": true}),
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
            capabilities: json!({"structured_events": true, "resume": true}),
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
        (
            Some(json!({"structured_events": true, "resume": true})),
            Some(identity("a"))
        )
    );
    let a1 = crate::persistence::team::assignment(&conn, "p1", "R1", "T1", "A1", "developer")?;
    assert_eq!(
        (a1.session_capabilities, a1.session_identity),
        (Some(json!({"structured_events": true})), None)
    );
    Ok(())
}
