//! The commander's handover against a real store (REQ-019; docs/adr/ADR-024.md): the old Commander's late decision is
//! rejected for its epoch (TEST-019-01) and the new Commander's dispatches of work already running or accepted are
//! rejected by the state checks, nothing run twice (TEST-019-03).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::config::Config;
use hoh_core::decision::{Context, Proposal};
use hoh_core::persistence::selftest::{self, ProbeCommand};
use hoh_core::persistence::{
    Availability, Decided, Fenced, NewEvent, Options, RunTarget, Store, SubmitCommand, TeamChange,
};
use hoh_core::protocol::Message;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The WAL self-test's second process: this binary, only this function.
#[test]
#[ignore = "run by the WAL self-test as its probe"]
fn wal_probe_helper() {
    if std::env::var_os(selftest::PROBE_DB_ENV).is_some() {
        std::process::exit(selftest::probe_main());
    }
}

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let probe = ProbeCommand {
        program: std::env::current_exe()?,
        args: [
            "wal_probe_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(OsString::from)
        .to_vec(),
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    };
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(probe),
            ..Options::default()
        },
    )?;
    exec(
        &path,
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-10T00:00:00Z');",
    )?;
    Ok((store, path))
}

/// `script` run by python3 on database `db` with `stdin` (the store's own connections are the product's; a test reads
/// and seeds through another client, as delivery.rs does).
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn exec(path: &Path, sql: &str) -> R {
    python(
        path,
        "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],timeout=30)\nc.execute('PRAGMA foreign_keys=ON')\n\
         c.executescript(sys.stdin.read())\nc.commit()",
        sql,
    )?;
    Ok(())
}

/// The one value `sql` reads, as text (NULL for none).
fn one(path: &Path, sql: &str) -> R<String> {
    let out = python(
        path,
        "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],timeout=30)\nv=c.execute(sys.stdin.read()).fetchone()[0]\n\
         print('NULL' if v is None else v)",
        sql,
    )?;
    Ok(out.trim_end_matches('\n').to_owned())
}

/// Run R started (goal "G"), RUNNING on spec s1, its commander codex at epoch 1 (session sc); developer task T RUNNING
/// at revision 5 with its one attempt A RUNNING; task T2 ACCEPTED at revision 3; message M ACKED after one delivery.
async fn world(store: &Store, path: &Path) -> R {
    store
        .submit_command(SubmitCommand {
            command_id: "c-start".to_owned(),
            idempotency_key: "k-start".to_owned(),
            kind: "StartRun".to_owned(),
            run_id: "R".to_owned(),
            expected_revision: 0,
            payload: json!({"goal": "G", "mode": "AUTO", "interactive": false}),
            payload_sha256: format!("{:064}", 7),
            target: RunTarget::Start {
                mode: "AUTO".to_owned(),
                budget: Config::default().budget,
            },
            event: NewEvent {
                project_id: "p1".to_owned(),
                event_id: "e-start".to_owned(),
                run_id: Some("R".to_owned()),
                kind: "run.created".to_owned(),
                payload: json!({"to": "CREATED"}),
            },
            outbox: None,
        })
        .await?;
    exec(
        path,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = 'R';
         INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc', 'codex', 'commander', '1', '{\"structured_events\": true}'),
           ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
         UPDATE role_bindings SET session_id = 'sc' WHERE run_id = 'R' AND role = 'commander';
         UPDATE role_bindings SET session_id = 'sd' WHERE run_id = 'R' AND role = 'developer';
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T', 'R', 's1', 'RUNNING', 5, 'developer', 0, 1, 1),
           ('T2', 'R', 's1', 'ACCEPTED', 3, 'developer', 0, 2, 1);
         INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision, agent,
           started_at) VALUES ('A', 'T', 'sd', 'RUNNING', 1, 1, 0, 'claude', '2026-11-10T00:00:00Z');
         INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, payload_json, state, attempt_count) VALUES
           ('M', (SELECT min(seq) FROM events), 'R', 'developer', '{}', 'ACKED', 1);",
    )
}

/// The commander's change from codex to pi, applied at the run's checkpoint.
async fn hand_over(store: &Store, path: &Path) -> R {
    let bindings = json!({"commander": {"agent": "pi"}});
    store
        .submit_command(SubmitCommand {
            command_id: "c-pi".to_owned(),
            idempotency_key: "k-pi".to_owned(),
            kind: "ConfigureTeam".to_owned(),
            run_id: String::new(),
            expected_revision: 0,
            payload: json!({"bindings": bindings, "apply_at": "checkpoint"}),
            payload_sha256: format!("{:064}", 8),
            target: RunTarget::Team(TeamChange {
                bindings: Some(bindings.clone()),
                scheduler: None,
                root: None,
                file: None,
            }),
            event: NewEvent {
                project_id: "p1".to_owned(),
                event_id: "e-pi".to_owned(),
                run_id: None,
                kind: "team.configured".to_owned(),
                payload: json!({"revision": 1}),
            },
            outbox: None,
        })
        .await?;
    store.team_checkpoint("R", Availability::default()).await?;
    assert_eq!(
        one(
            path,
            "SELECT agent || '/' || role_epoch FROM role_bindings WHERE run_id = 'R' AND role = 'commander'"
        )?,
        "pi/2"
    );
    Ok(())
}

/// A decision of the Commander at `epoch`, expecting revision `revision`.
fn decision(id: &str, epoch: u64, revision: u64, payload: Value) -> R<Proposal> {
    let message: Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": epoch, "correlation_id": "c",
        "causation_id": null, "idempotency_key": format!("k-{id}"), "expected_revision": revision,
        "spec_sha256": format!("{:064}", 1), "candidate_id": null, "kind": "decision", "payload": payload
    }))?;
    Ok(Proposal::from_message(&message)?)
}

#[tokio::test]
async fn test_019_01_a_late_decision_of_the_old_commander_is_rejected_stale_role_epoch() -> R {
    // AC-019-01: commander codex at epoch 1, T RUNNING at revision 5. Once the change to pi is applied (pi, epoch 2),
    // the old Codex's decision {role_epoch 1, expected_revision 5} is REJECTED with STALE_ROLE_EPOCH, after the
    // handover in the event order; T stays at revision 5 and the decision adds no outbox row. (decide's epoch fence,
    // ADR-011, predates M2-T17: the test holds it across the handover; a mutant removing it fails here.)
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&store, &path).await?;
    hand_over(&store, &path).await?;
    let outbox = one(&path, "SELECT count(*) FROM outbox")?;
    let late = decision(
        "old-1",
        1,
        5,
        json!({"kind": "transition_task", "task_id": "T", "to_state": "READY"}),
    )?;
    match store.decide(late, Context::default()).await? {
        Decided::Rejected { reason, .. } => {
            assert_eq!(serde_json::to_value(reason)?, json!("STALE_ROLE_EPOCH"));
        }
        other => return Err(format!("the late decision was not rejected: {other:?}").into()),
    }
    assert_eq!(
        one(
            &path,
            "SELECT state || '/' || reason_code FROM decisions WHERE decision_id = 'old-1'"
        )?,
        "REJECTED/STALE_ROLE_EPOCH"
    );
    assert_eq!(
        one(&path, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        "5"
    );
    assert_eq!(
        one(&path, "SELECT count(*) FROM outbox")?,
        outbox,
        "no outbox row"
    );
    let applied: i64 = one(
        &path,
        "SELECT seq FROM events WHERE kind = 'team.binding_applied' AND json_extract(payload_json, '$.role') = 'commander'",
    )?
    .parse()?;
    let rejected: i64 = one(
        &path,
        "SELECT seq FROM events WHERE kind = 'decision.rejected' AND json_extract(payload_json, '$.decision_id') = 'old-1'",
    )?
    .parse()?;
    assert!(
        applied < rejected,
        "handover {applied}, rejection {rejected}"
    );
    Ok(())
}

#[tokio::test]
async fn test_019_03_the_new_commanders_dispatches_of_running_or_accepted_tasks_are_rejected() -> R
{
    // AC-019-03: before the handover T has one RUNNING attempt, M is ACKED and T2 ACCEPTED. The new Commander's
    // dispatches of T and T2 (its own epoch, their current revisions) are REJECTED with their reason; T keeps its one
    // attempt, T2 runs nothing, M's delivery count stays 1. (These state checks predate M2-T17: the test holds them
    // across the handover.)
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&store, &path).await?;
    hand_over(&store, &path).await?;
    let attempts = "SELECT (SELECT count(*) FROM attempts WHERE task_id = 'T') || '/' || \
                    (SELECT count(*) FROM attempts WHERE task_id = 'T2')";
    // A decision expects the run's revision (as decide's own tests send it), its current one: only the tasks' states
    // can refuse these dispatches.
    let run: u64 = one(&path, "SELECT revision FROM runs WHERE run_id = 'R'")?.parse()?;
    for (id, task) in [("new-1", "T"), ("new-2", "T2")] {
        let dispatch = decision(
            id,
            2,
            run,
            json!({"kind": "dispatch", "task_id": task, "role": "developer"}),
        )?;
        match store.decide(dispatch, Context::default()).await? {
            Decided::Rejected { reason, .. } => {
                assert_eq!(
                    serde_json::to_value(reason)?,
                    json!("ILLEGAL_TRANSITION"),
                    "{id}"
                );
            }
            other => return Err(format!("{id} was not rejected: {other:?}").into()),
        }
        assert_eq!(one(&path, attempts)?, "1/0", "after {id}");
    }
    assert_eq!(
        one(
            &path,
            "SELECT group_concat(state || ':' || coalesce(reason_code, '-'), ',') FROM \
             (SELECT * FROM decisions WHERE decision_id IN ('new-1', 'new-2') ORDER BY decision_id)"
        )?,
        "REJECTED:ILLEGAL_TRANSITION,REJECTED:ILLEGAL_TRANSITION"
    );
    assert_eq!(
        one(
            &path,
            "SELECT state || '/' || attempt_count FROM outbox WHERE message_id = 'M'"
        )?,
        "ACKED/1"
    );
    assert_eq!(
        one(
            &path,
            "SELECT state || '/' || revision FROM tasks WHERE task_id IN ('T', 'T2') ORDER BY task_id LIMIT 1"
        )?,
        "RUNNING/5"
    );
    assert_eq!(
        one(&path, "SELECT state FROM tasks WHERE task_id = 'T2'")?,
        "ACCEPTED"
    );
    Ok(())
}

/// A message of the Commander at `epoch` to `recipient`, of `kind` (not a decision).
fn commander_message(id: &str, epoch: u64, recipient: &str, kind: &str) -> R<Message> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "T", "attempt_id": "A",
        "sender_role": "commander", "recipient_role": recipient, "role_epoch": epoch, "correlation_id": "c",
        "causation_id": null, "idempotency_key": format!("k-{id}"), "expected_revision": 5,
        "spec_sha256": format!("{:064}", 1), "candidate_id": null, "kind": kind,
        "payload": {"text": "stop and rebase"}
    }))?)
}

#[tokio::test]
async fn an_old_commanders_steer_is_rejected_by_its_own_epoch_and_changes_nothing() -> R {
    // REQ-019 behaviour 3 (docs/adr/ADR-024.md decision 6): a Commander's steer or message passes the sender's fence
    // when it enters, as a decision does in decide: after the handover the old Codex's steer to the developer (epoch
    // 1) is only history, a decisions row REJECTED STALE_ROLE_EPOCH with its decision.rejected event; no task or run
    // changes, no outbox row. The new Commander's (epoch 2) passes the fence with nothing written.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    world(&store, &path).await?;
    hand_over(&store, &path).await?;
    let before = |path: &Path| -> R<String> {
        one(
            path,
            "SELECT (SELECT count(*) FROM outbox) || '/' || (SELECT group_concat(task_id || revision) FROM tasks) \
             || '/' || (SELECT revision FROM runs WHERE run_id = 'R')",
        )
    };
    let unchanged = before(&path)?;
    let old = commander_message("old-steer", 1, "developer", "steer")?;
    match store.commander_message(&old).await? {
        Fenced::Rejected { reason, .. } => {
            assert_eq!(serde_json::to_value(reason)?, json!("STALE_ROLE_EPOCH"));
        }
        Fenced::Current => return Err("the old Commander's steer passed".into()),
    }
    assert_eq!(
        one(
            &path,
            "SELECT state || '/' || reason_code || '/' || role_epoch FROM decisions WHERE decision_id = 'old-steer'"
        )?,
        "REJECTED/STALE_ROLE_EPOCH/1"
    );
    assert_eq!(
        one(
            &path,
            "SELECT count(*) FROM events WHERE kind = 'decision.rejected' \
             AND json_extract(payload_json, '$.decision_id') = 'old-steer'"
        )?,
        "1"
    );
    assert_eq!(before(&path)?, unchanged);
    let decisions = one(&path, "SELECT count(*) FROM decisions")?;
    let new = commander_message("new-steer", 2, "developer", "steer")?;
    assert!(matches!(
        store.commander_message(&new).await?,
        Fenced::Current
    ));
    assert_eq!(one(&path, "SELECT count(*) FROM decisions")?, decisions);
    assert_eq!(before(&path)?, unchanged);
    // Only the Commander's own messages pass this fence; a decision goes through decide.
    assert!(
        store
            .commander_message(&commander_message("not-mine", 2, "developer", "decision")?)
            .await
            .is_err()
    );
    Ok(())
}
