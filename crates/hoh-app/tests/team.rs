//! The team on an in-process host (REQ-018, docs/adr/ADR-022.md): a started run's bindings in its snapshot and its
//! team page, and ConfigureTeam.

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config
}

/// Start a host in a project another host of this process just let go (as tests/host.rs does: a lock just released
/// can still be held for an instant by a parallel test's child).
async fn start_when_free(config: HostConfig) -> Result<Host, hoh_app::host::StartError> {
    let end = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && std::time::Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

/// The first column of the first row `sql` gives, as text ("" for none or NULL).
fn query(project: &Path, sql: &str) -> R<String> {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone(); \
             print('' if r is None or r[0] is None else r[0])",
        ])
        .arg(db(project))
        .arg(sql)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// One HTTP exchange with the host over its TCP port: the status and the JSON body.
async fn exchange(host: &Host, method: &str, path: &str, body: Option<&Value>) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n",
        port = host.port,
        token = host.token,
    );
    if !body.is_empty() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// A StartRun body with idempotency key `key`.
fn start_run(key: &str) -> Value {
    json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": 0, "kind": "StartRun", "run_id": null,
        "payload": {"goal": "g", "mode": "AUTO", "interactive": false},
    })
}

/// The six built-in roles in the snapshot's order, with their default agents.
const DEFAULT_TEAM: [(&str, &str); 6] = [
    ("commander", "codex"),
    ("cto", "claude"),
    ("developer", "claude"),
    ("qa", "pi"),
    ("researcher", "pi"),
    ("reviewer", "codex"),
];

#[tokio::test]
async fn a_started_run_shows_its_team_in_its_snapshot_and_its_team_page() -> R {
    // ADR-022 decisions 7 and 18: StartRun binds the default team at epoch 1, team revision 0; GET /v1/runs/{id} lists
    // it in the projection, in the role order, and GET /v1/runs/{id}/team is the same list as a page.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let (status, accepted) =
        exchange(&host, "POST", "/v1/commands", Some(&start_run("s1"))).await?;
    assert_eq!(status, 202, "{accepted}");
    let run_id = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    let (status, run) = exchange(&host, "GET", &format!("/v1/runs/{run_id}"), None).await?;
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["projection"]["team_revision"], 0);
    let team = run["projection"]["team"].as_array().ok_or("no team")?;
    let shown: Vec<(&str, &str)> = team
        .iter()
        .filter_map(|m| Some((m["role"].as_str()?, m["agent"].as_str()?)))
        .collect();
    assert_eq!(shown, DEFAULT_TEAM.to_vec());
    for member in team {
        assert_eq!(member["role_epoch"], 1, "{member}");
        assert_eq!(member["config_revision"], 0, "{member}");
        assert_eq!(member["state"], "ACTIVE", "{member}");
        assert_eq!(member["model"], Value::Null, "{member}");
        assert_eq!(member["pending"], Value::Null, "{member}");
    }
    let (status, page) = exchange(&host, "GET", &format!("/v1/runs/{run_id}/team"), None).await?;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["items"], run["projection"]["team"]);
    assert_eq!(page["next_cursor"], Value::Null);
    assert!(page["watermark"].as_i64().is_some_and(|w| w > 0), "{page}");
    let (status, missing) = exchange(&host, "GET", "/v1/runs/no-such-run/team", None).await?;
    assert_eq!(
        (status, missing["code"].as_str()),
        (404, Some("RUN_NOT_FOUND"))
    );
    assert_eq!(
        query(
            dir.path(),
            &format!("SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}'")
        )?,
        "6"
    );
    host.shutdown().await;
    Ok(())
}

/// A ConfigureTeam body with idempotency key `key`.
fn configure(key: &str, run_id: Option<&str>, expected: i64, payload: Value) -> Value {
    json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": expected, "kind": "ConfigureTeam", "run_id": run_id, "payload": payload,
    })
}

/// A started run's id.
async fn started(host: &Host, key: &str) -> R<String> {
    let (status, accepted) = exchange(host, "POST", "/v1/commands", Some(&start_run(key))).await?;
    assert_eq!(status, 202, "{accepted}");
    Ok(accepted["run_id"].as_str().ok_or("no run_id")?.to_owned())
}

#[tokio::test]
async fn test_018_04_refused_team_changes_keep_the_revision_and_every_role_epoch() -> R {
    // AC-018-04: a running project at team revision r (a host with its runner, so checkpoints happen); five
    // ConfigureTeams are refused: an unknown agent, removing qa, a shared reviewer session and apply_at=now with 400
    // and the key path, a stale expected_revision with 409 and current_revision=r; after them, and after the runner's
    // ticks, the snapshot's team and revision are the same, no role epoch moved and no binding was applied
    // (M2T07R-56).
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Idle),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    exec(
        dir.path(),
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 's1', project_id, printf('%064d', 1), 1 \
             FROM projects; UPDATE runs SET state = 'RUNNING', spec_id = 's1', revision = revision + 1 \
             WHERE run_id = '{run_id}';"
        ),
    )?;
    let checkpoint = |bindings: Value| json!({"bindings": bindings, "apply_at": "checkpoint"});
    // r = 1, applied to the run by its checkpoint, so that r-1 is still a revision.
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "ok",
            None,
            0,
            checkpoint(json!({"cto": {"model": "m-cto"}})),
        )),
    )
    .await?;
    assert_eq!(
        (status, body["revision"].as_i64()),
        (202, Some(1)),
        "{body}"
    );
    let r = 1;
    let applied_count = || {
        query(
            dir.path(),
            "SELECT count(*) FROM events WHERE kind = 'team.binding_applied'",
        )
    };
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while query(
        dir.path(),
        &format!("SELECT config_revision FROM runs WHERE run_id = '{run_id}'"),
    )? != "1"
    {
        assert!(
            std::time::Instant::now() < end,
            "the run never reached revision 1"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let applied = applied_count()?;
    let run_path = format!("/v1/runs/{run_id}");
    let (_, before) = exchange(&host, "GET", &run_path, None).await?;
    let mut exchanges = Vec::new();
    for (key, run, expected, payload, key_path) in [
        (
            "agent",
            Some(run_id.as_str()),
            r,
            checkpoint(json!({"qa": {"agent": "unknown"}})),
            "bindings.qa.agent",
        ),
        (
            "delete",
            None,
            r,
            checkpoint(json!({"qa": null})),
            "bindings.qa",
        ),
        (
            "shared",
            Some(run_id.as_str()),
            r,
            checkpoint(json!({"reviewer": {"independent_session": false}})),
            "bindings.reviewer.independent_session",
        ),
        (
            "now",
            None,
            r,
            json!({"bindings": {"developer": {"agent": "codex"}}, "apply_at": "now"}),
            "apply_at",
        ),
    ] {
        let request = configure(key, run, expected, payload);
        let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&request)).await?;
        assert_eq!(status, 400, "{key}: {body}");
        assert_eq!(body["code"], "INVALID_TEAM_CONFIG", "{key}: {body}");
        assert_eq!(body["details"]["key_path"], key_path, "{key}: {body}");
        assert_eq!(body["current_revision"], r, "{key}: {body}");
        assert_eq!(body["retryable"], false, "{key}: {body}");
        assert!(
            body["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{key}: {body}"
        );
        exchanges.push(json!({"request": request, "status": status, "response": body}));
    }
    let stale = configure(
        "stale",
        None,
        r - 1,
        checkpoint(json!({"developer": {"agent": "codex"}})),
    );
    let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&stale)).await?;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "STALE_REVISION", "{body}");
    assert_eq!(body["current_revision"], r, "{body}");
    assert_eq!(body["retryable"], false, "{body}");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
    exchanges.push(json!({"request": stale, "status": status, "response": body}));
    // Several ticks of the runner (200 ms each) after the five commands.
    tokio::time::sleep(Duration::from_millis(800)).await;
    let (_, after) = exchange(&host, "GET", &run_path, None).await?;
    assert_eq!(after["projection"]["team"], before["projection"]["team"]);
    assert_eq!(after["projection"]["team_revision"], r);
    assert_eq!(
        query(
            dir.path(),
            "SELECT max(revision) || '/' || count(*) FROM team_revisions"
        )?,
        format!("{r}/{}", r + 1)
    );
    assert_eq!(applied_count()?, applied, "a binding was applied");
    assert_eq!(
        query(
            dir.path(),
            "SELECT count(*) FROM commands WHERE kind = 'ConfigureTeam'"
        )?,
        "1",
        "a refused command is not recorded"
    );
    keep(
        "test-018-04.json",
        &json!({"exchanges": exchanges, "snapshot_before": before, "snapshot_after": after,
                "team_revisions": query(dir.path(), "SELECT json_group_array(json_object('revision', revision, 'state', state)) FROM team_revisions")?,
                "binding_applied_events": applied}),
    )?;
    host.shutdown().await;
    Ok(())
}

/// The evidence a check keeps (HOH_EVIDENCE_DIR, set by run_check).
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

#[tokio::test]
async fn an_accepted_team_change_is_the_next_revision_pending_in_running_runs_and_replays() -> R {
    // ADR-022 decisions 3, 4 and 8: an accepted ConfigureTeam is the project's revision r+1 at once (202 with that
    // revision), recorded with its source and a team.configured event; a run started before shows the change as pending
    // until its checkpoint; a replay returns the same body, the same key with another payload is KEY_REUSE.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let run_id = started(&host, "s1").await?;
    let body = configure(
        "dev",
        None,
        0,
        json!({"bindings": {"developer": {"agent": "codex", "model": "m-test"},
            "auditor": {"agent": "pi", "prompt": "builtin:qa"}}, "apply_at": "checkpoint"}),
    );
    let (status, accepted) = exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{accepted}");
    assert_eq!(
        accepted,
        json!({"command_id": "c-dev", "run_id": null, "revision": 1, "status": "ACCEPTED"})
    );
    let (status, replayed) = exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!((status, &replayed), (202, &accepted));
    let mut other = body.clone();
    other["payload"]["bindings"]["developer"]["model"] = json!("m-other");
    let (status, reused) = exchange(&host, "POST", "/v1/commands", Some(&other)).await?;
    assert_eq!((status, reused["code"].as_str()), (409, Some("KEY_REUSE")));
    let source: Value = serde_json::from_str(&query(
        dir.path(),
        "SELECT json_extract(config_json, '$.source') FROM team_revisions WHERE revision = 1 AND state = 'APPLIED'",
    )?)?;
    assert_eq!(source, json!({"kind": "command", "command_id": "c-dev"}));
    assert_eq!(
        query(
            dir.path(),
            "SELECT json_extract(payload_json, '$.revision') FROM events WHERE kind = 'team.configured' AND run_id IS NULL"
        )?,
        "1"
    );
    let (_, run) = exchange(&host, "GET", &format!("/v1/runs/{run_id}"), None).await?;
    assert_eq!(run["projection"]["team_revision"], 1);
    let team = run["projection"]["team"].as_array().ok_or("no team")?;
    let member = |role: &str| team.iter().find(|m| m["role"] == role).cloned();
    let developer = member("developer").ok_or("no developer")?;
    assert_eq!(
        developer["agent"], "claude",
        "the running run keeps its binding: {developer}"
    );
    assert_eq!(developer["role_epoch"], 1);
    assert_eq!(
        developer["pending"],
        json!({"agent": "codex", "model": "m-test", "config_revision": 1, "refused": null})
    );
    let auditor = member("auditor").ok_or("no auditor")?;
    assert_eq!(auditor["state"], "UNBOUND", "{auditor}");
    assert_eq!(member("qa").ok_or("no qa")?["pending"], Value::Null);
    assert_eq!(
        team.last().map(|m| m["role"].clone()),
        Some(json!("auditor"))
    );
    // A run started now binds revision 1.
    let later = started(&host, "s2").await?;
    assert_eq!(
        query(
            dir.path(),
            &format!(
                "SELECT agent || '/' || model || '/' || config_revision FROM role_bindings \
                 WHERE run_id = '{later}' AND role = 'developer'"
            )
        )?,
        "codex/m-test/1"
    );
    assert_eq!(
        query(
            dir.path(),
            &format!("SELECT count(*) FROM role_bindings WHERE run_id = '{later}'")
        )?,
        "7"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn of_two_changes_at_one_revision_one_is_accepted_and_the_other_sees_the_new_revision() -> R {
    // ADR-022 decision 3 (AC-019-04's rule): concurrent ConfigureTeams at revision r: one 202 at r+1, the other 409
    // with current_revision r+1 read in its own transaction.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let change = |key: &str, model: &str| {
        configure(
            key,
            None,
            0,
            json!({"bindings": {"cto": {"model": model}}, "apply_at": "checkpoint"}),
        )
    };
    let (first, second) = (change("a", "m-a"), change("b", "m-b"));
    let (a, b) = tokio::join!(
        exchange(&host, "POST", "/v1/commands", Some(&first)),
        exchange(&host, "POST", "/v1/commands", Some(&second))
    );
    let mut replies = [a?, b?];
    replies.sort_by_key(|(status, _)| *status);
    assert_eq!(replies[0].0, 202, "{replies:?}");
    assert_eq!(replies[0].1["revision"], 1);
    assert_eq!(replies[1].0, 409, "{replies:?}");
    assert_eq!(replies[1].1["code"], "STALE_REVISION");
    assert_eq!(replies[1].1["current_revision"], 1);
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_scheduler_change_for_the_project_is_snapshot_by_runs_started_later() -> R {
    // CR-007 S3 and ADR-022 decision 4: a project ConfigureTeam with `scheduler` is revision r+1; a run started later
    // snapshots it; a run that exists keeps its snapshot (409 RUN_CONFIG_FROZEN, nothing written).
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let first = started(&host, "s1").await?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "rounds",
            None,
            0,
            json!({"scheduler": {"max_wait_rounds": 5}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(
        (status, body["revision"].as_i64()),
        (202, Some(1)),
        "{body}"
    );
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "frozen",
            Some(&first),
            1,
            json!({"scheduler": {"max_wait_rounds": 6}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("RUN_CONFIG_FROZEN")),
        "{body}"
    );
    let rounds = |run: &str| {
        query(
            dir.path(),
            &format!(
                "SELECT json_extract(config_snapshot_json, '$.scheduler.max_wait_rounds') FROM runs WHERE run_id = '{run}'"
            ),
        )
    };
    assert_eq!(rounds(&first)?, "20");
    let later = started(&host, "s2").await?;
    assert_eq!(rounds(&later)?, "5");
    assert_eq!(
        query(
            dir.path(),
            &format!(
                "SELECT json_extract(config_snapshot_json, '$.scheduler.weights.user_value') FROM runs WHERE run_id = '{later}'"
            )
        )?,
        "4"
    );
    assert_eq!(
        query(dir.path(), "SELECT max(revision) FROM team_revisions")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_team_change_with_an_unknown_key_or_a_bad_role_name_is_refused_with_its_key_path() -> R {
    // REQ-018 decision 3 and 6: the same rules as the configuration file, the key path under `bindings`.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    for (key, bindings, key_path) in [
        ("modle", json!({"qa": {"modle": "x"}}), "bindings.qa.modle"),
        ("shape", json!({"qa": "pi"}), "bindings.qa"),
        (
            "name",
            json!({"Bad Name": {"agent": "pi", "prompt": "a.md"}}),
            "bindings.Bad Name",
        ),
        (
            "custom",
            json!({"auditor": {"agent": "pi"}}),
            "bindings.auditor.prompt",
        ),
        (
            "fallback",
            json!({"qa": {"fallback": ["pi", "gpt"]}}),
            "bindings.qa.fallback[1]",
        ),
    ] {
        let (status, body) = exchange(
            &host,
            "POST",
            "/v1/commands",
            Some(&configure(
                key,
                None,
                0,
                json!({"bindings": bindings, "apply_at": "checkpoint"}),
            )),
        )
        .await?;
        assert_eq!(status, 400, "{key}: {body}");
        assert_eq!(body["details"]["key_path"], key_path, "{key}: {body}");
    }
    assert_eq!(
        query(dir.path(), "SELECT count(*) FROM team_revisions")?,
        "0"
    );
    host.shutdown().await;
    Ok(())
}

/// An executor that is never given work (no task exists): only the runner's ticks run.
#[derive(Debug)]
struct Idle;

impl hoh_app::host::Executor for Idle {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        tokio::sync::oneshot::channel().1
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        Err("no work is expected".to_owned().into())
    }
}

/// Run `sql` on the project's database.
fn exec(project: &Path, sql: &str) -> R {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); c.executescript(sys.argv[2]); c.commit()",
        ])
        .arg(db(project))
        .arg(sql)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(())
}

/// Run `run_id` on a spec of its own: a binding change hands its role over with a package naming the run's spec, and
/// a run with none (StartRun writes none yet) has every change refused NO_SPEC (docs/adr/ADR-024.md decisions 1, 10).
fn on_a_spec(project: &Path, run_id: &str) -> R {
    exec(
        project,
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 'spec-{run_id}', project_id, \
             printf('%064d', 1), 1 FROM projects; UPDATE runs SET spec_id = 'spec-{run_id}' WHERE run_id = '{run_id}';"
        ),
    )
}

#[tokio::test]
async fn a_hosts_runner_applies_a_team_change_at_the_roles_checkpoint() -> R {
    // ADR-022 decision 8: each tick of the runner applies the checkpoint of every run not ended that is behind the
    // project's revision, before any pace or round: a change to a role with no live attempt is bound at the next epoch.
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Idle),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    on_a_spec(dir.path(), &run_id)?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "dev",
            None,
            0,
            json!({"bindings": {"developer": {"agent": "codex"}}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(status, 202, "{body}");
    // The run is still CREATED: a run not ended that is behind the revision is checkpointed whatever its state
    // (M2T07R-02, -54).
    let binding = format!(
        "SELECT agent || '/' || role_epoch || '/' || config_revision FROM role_bindings \
         WHERE run_id = '{run_id}' AND role = 'developer'"
    );
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while query(dir.path(), &binding)? != "codex/2/1" {
        assert!(
            std::time::Instant::now() < end,
            "not applied: {}",
            query(dir.path(), &binding)?
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_hosts_runner_leaves_an_ended_run_at_its_revision() -> R {
    // M2T07R-154 (ADR-022 decision 8): only runs not ended are checkpointed; an ended run keeps its bindings and writes
    // no event, while a run not ended at the same ticks takes the change.
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Idle),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let mut ended = Vec::new();
    for (key, state) in [
        ("c", "COMPLETED"),
        ("x", "CANCELLED"),
        ("f", "FAILED"),
        ("i", "INCOMPLETE"),
    ] {
        let run_id = started(&host, key).await?;
        exec(
            dir.path(),
            &format!("UPDATE runs SET state = '{state}' WHERE run_id = '{run_id}';"),
        )?;
        ended.push(run_id);
    }
    let open = started(&host, "open").await?;
    on_a_spec(dir.path(), &open)?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "dev",
            None,
            0,
            json!({"bindings": {"developer": {"agent": "codex"}}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(status, 202, "{body}");
    let binding = |run_id: &str| {
        format!(
            "SELECT agent || '/' || role_epoch || '/' || config_revision FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        )
    };
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while query(dir.path(), &binding(&open))? != "codex/2/1" {
        assert!(
            std::time::Instant::now() < end,
            "the open run is not checkpointed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // A few more ticks.
    tokio::time::sleep(Duration::from_millis(500)).await;
    for run_id in &ended {
        assert_eq!(
            query(dir.path(), &binding(run_id))?,
            "claude/1/0",
            "{run_id}"
        );
        assert_eq!(
            query(
                dir.path(),
                &format!(
                    "SELECT config_revision || '/' || (SELECT count(*) FROM events WHERE run_id = '{run_id}' \
                     AND kind LIKE 'team.%') FROM runs WHERE run_id = '{run_id}'"
                )
            )?,
            "0/0",
            "{run_id}"
        );
    }
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_reused_key_is_key_reuse_before_any_frozen_scheduler_and_its_revision_is_the_teams() -> R
{
    // M2T07R-04, -05 (ADR-022 decision 5): the key is looked up first; a team command's current_revision is the team
    // revision, with or without a run.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let run_id = started(&host, "s1").await?;
    let checkpoint = |bindings: Value| json!({"bindings": bindings, "apply_at": "checkpoint"});
    let first = configure("k1", None, 0, checkpoint(json!({"cto": {"model": "a"}})));
    assert_eq!(
        exchange(&host, "POST", "/v1/commands", Some(&first))
            .await?
            .0,
        202
    );
    let with_run = configure(
        "k2",
        Some(&run_id),
        1,
        checkpoint(json!({"cto": {"model": "b"}})),
    );
    assert_eq!(
        exchange(&host, "POST", "/v1/commands", Some(&with_run))
            .await?
            .0,
        202
    );
    for (mut reused, what) in [(first, "no run"), (with_run, "a run")] {
        reused["payload"]["scheduler"] = json!({"max_wait_rounds": 5});
        let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&reused)).await?;
        assert_eq!(
            (status, body["code"].as_str()),
            (409, Some("KEY_REUSE")),
            "{what}: {body}"
        );
        assert_eq!(body["current_revision"], 2, "{what}: {body}");
    }
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn the_team_page_follows_limit_and_cursor() -> R {
    // M2T07R-06, -57 (openapi listTeam): pages of `limit` roles in the role order, the next one after `cursor`;
    // a limit or cursor it cannot read is 404 as for GET /v1/runs.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let run_id = started(&host, "s1").await?;
    let mut roles = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            None => format!("/v1/runs/{run_id}/team?limit=4"),
            Some(c) => format!("/v1/runs/{run_id}/team?limit=4&cursor={c}"),
        };
        let (status, page) = exchange(&host, "GET", &path, None).await?;
        assert_eq!(status, 200, "{page}");
        let items = page["items"].as_array().ok_or("no items")?;
        assert!(items.len() <= 4);
        roles.extend(
            items
                .iter()
                .filter_map(|m| m["role"].as_str().map(str::to_owned)),
        );
        match page["next_cursor"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(
        roles,
        DEFAULT_TEAM.map(|(role, _)| role.to_owned()).to_vec()
    );
    for query in ["limit=0", "limit=501", "limit=x", "cursor=nobody"] {
        let (status, _) = exchange(
            &host,
            "GET",
            &format!("/v1/runs/{run_id}/team?{query}"),
            None,
        )
        .await?;
        assert_eq!(status, 404, "{query}");
    }
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_team_change_is_checked_against_the_project_and_its_envelope_stays_a_bad_request() -> R {
    // M2T07R-53: a prompt override file must exist inside the project; M2T07R-55 (4): a run that does not exist is
    // RUN_NOT_FOUND with nothing written; M2T07R-58: a violation outside the payload is BAD_REQUEST.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let auditor = |prompt: &str| json!({"bindings": {"auditor": {"agent": "pi", "prompt": prompt}}, "apply_at": "checkpoint"});
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "missing",
            None,
            0,
            auditor(".hoh/prompts/auditor.md"),
        )),
    )
    .await?;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["details"]["key_path"], "bindings.auditor.prompt");
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "nowhere",
            Some("no-such-run"),
            0,
            auditor("builtin:qa"),
        )),
    )
    .await?;
    assert_eq!(
        (status, body["code"].as_str()),
        (409, Some("RUN_NOT_FOUND")),
        "{body}"
    );
    let mut envelope = configure("env", None, 0, auditor("builtin:qa"));
    envelope["expected_revision"] = json!(-1);
    let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&envelope)).await?;
    assert_eq!(
        (status, body["code"].as_str()),
        (400, Some("BAD_REQUEST")),
        "{body}"
    );
    assert_eq!(
        query(dir.path(), "SELECT count(*) FROM team_revisions")?,
        "0"
    );
    std::fs::create_dir_all(dir.path().join(".hoh/prompts"))?;
    // M2T07R-101: a link that leads outside the project is refused, though its file exists.
    let outside = tempfile::tempdir()?;
    std::fs::write(outside.path().join("outside.md"), "Outside.")?;
    std::os::unix::fs::symlink(
        outside.path().join("outside.md"),
        dir.path().join(".hoh/prompts/escape.md"),
    )?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "escape",
            None,
            0,
            auditor(".hoh/prompts/escape.md"),
        )),
    )
    .await?;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["details"]["key_path"], "bindings.auditor.prompt");
    std::fs::write(dir.path().join(".hoh/prompts/auditor.md"), "Audit.")?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "present",
            None,
            0,
            auditor(".hoh/prompts/auditor.md"),
        )),
    )
    .await?;
    assert_eq!(status, 202, "{body}");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_payload_missing_a_member_or_of_another_type_is_an_invalid_team_config() -> R {
    // M2T07R-100, -151 (ADR-022 decision 5): a violation of the payload itself is the team's too, not only one inside
    // it: 400 INVALID_TEAM_CONFIG with the team's revision, the missing member's name as its key path, else `payload`.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let first = configure(
        "first",
        None,
        0,
        json!({"bindings": {"cto": {"model": "a"}}, "apply_at": "checkpoint"}),
    );
    assert_eq!(
        exchange(&host, "POST", "/v1/commands", Some(&first))
            .await?
            .0,
        202
    );
    for (key, payload, key_path) in [
        (
            "no-apply-at",
            json!({"bindings": {"developer": {"model": "x"}}}),
            "apply_at",
        ),
        ("nothing", json!({"apply_at": "checkpoint"}), "payload"),
        ("array", json!([]), "payload"),
    ] {
        let (status, body) = exchange(
            &host,
            "POST",
            "/v1/commands",
            Some(&configure(key, None, 1, payload)),
        )
        .await?;
        assert_eq!(
            (status, body["code"].as_str()),
            (400, Some("INVALID_TEAM_CONFIG")),
            "{key}: {body}"
        );
        assert_eq!(body["details"]["key_path"], key_path, "{key}: {body}");
        assert_eq!(body["current_revision"], 1, "{key}: {body}");
    }
    assert_eq!(
        query(dir.path(), "SELECT max(revision) FROM team_revisions")?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// An executor that is never given work but has the product's registry: the runner binds sessions with it.
#[derive(Debug)]
struct Registered(std::sync::Arc<hoh_app::host::agents::Registry>);

impl hoh_app::host::Executor for Registered {
    fn registry(&self) -> Option<std::sync::Arc<hoh_app::host::agents::Registry>> {
        Some(self.0.clone())
    }

    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        tokio::sync::oneshot::channel().1
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        Err("no work is expected".to_owned().into())
    }
}

#[tokio::test]
async fn the_runner_binds_each_role_to_a_session_of_its_available_agent_or_blocks_it() -> R {
    // docs/adr/ADR-022.md decision 9: with only claude on the host's PATH, the claude roles of a new run get a session
    // per role at epoch 1 (the agent's version and capabilities recorded), the others are blocked AGENT_UNAVAILABLE.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let help = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/claude/2.1.318/help.txt"
    );
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{help}';;\n *) exit 2;;\nesac\n"
        ),
    )?;
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let mut config = config(&project);
    let registry = std::sync::Arc::new(hoh_app::host::agents::Registry::new(
        format!("{}:/usr/bin:/bin", bin.display()).into(),
    ));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Registered(registry)),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    let states = format!(
        "SELECT group_concat(role || ':' || state || ':' || coalesce(session_id, '-'), ',') FROM \
         (SELECT * FROM role_bindings WHERE run_id = '{run_id}' ORDER BY role)"
    );
    let end = std::time::Instant::now() + Duration::from_secs(20);
    let settled = |s: &str| !s.contains(":ACTIVE:-");
    let mut seen = query(&project, &states)?;
    while !settled(&seen) {
        assert!(std::time::Instant::now() < end, "not bound: {seen}");
        tokio::time::sleep(Duration::from_millis(50)).await;
        seen = query(&project, &states)?;
    }
    let session = |role: &str| format!("{run_id}/session/{role}/1");
    assert_eq!(
        seen,
        format!(
            "commander:BLOCKED:-,cto:ACTIVE:{},developer:ACTIVE:{},qa:BLOCKED:-,researcher:BLOCKED:-,reviewer:BLOCKED:-",
            session("cto"),
            session("developer")
        )
    );
    assert_eq!(
        query(
            &project,
            &format!(
                "SELECT agent_version || '/' || json_extract(capabilities_json, '$.structured_events') FROM sessions WHERE session_id = '{}'",
                session("developer")
            )
        )?,
        "2.1.318 (Claude Code)/1"
    );
    assert_eq!(
        query(
            &project,
            &format!(
                "SELECT blocked_reason FROM role_bindings WHERE run_id = '{run_id}' AND role = 'qa'"
            )
        )?,
        "AGENT_UNAVAILABLE"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_configuration_file_becomes_a_team_revision_at_the_hosts_start_and_at_each_start_run() -> R
{
    // ADR-022 decision 6 (REQ-018 behaviour 4): the file is read at the host's start and at each StartRun; when it
    // differs from the latest revision made from it (by its sha256), its whole team becomes the next revision, with the
    // file as its source; unchanged, nothing is written, and a change made through the API since stands; a new run's
    // budget is the file's; without a file, the built-in defaults.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    let file = dir.path().join(".hoh/config.toml");
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"claude\"\nmodel = \"m-file\"\n\
         [team.auditor]\nagent = \"pi\"\nprompt = \"builtin:qa\"\n",
    )?;
    let host = start_when_free(config(dir.path())).await?;
    let source = |revision: i64| {
        query(
            dir.path(),
            &format!(
                "SELECT json_extract(config_json, '$.source.kind') || '/' || \
                 coalesce(json_extract(config_json, '$.source.sha256'), '') FROM team_revisions WHERE revision = {revision}"
            ),
        )
    };
    let sha = |text: &str| {
        let digest = <Sha256 as sha2::Digest>::digest(text.as_bytes());
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let first = std::fs::read_to_string(&file)?;
    assert_eq!(source(1)?, format!("file/{}", sha(&first)));
    let run = started(&host, "s1").await?;
    assert_eq!(
        query(
            dir.path(),
            &format!("SELECT agent FROM role_bindings WHERE run_id = '{run}' AND role = 'auditor'")
        )?,
        "pi"
    );
    let model = |run: &str| {
        query(
            dir.path(),
            &format!(
                "SELECT model FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"
            ),
        )
    };
    assert_eq!(model(&run)?, "m-file");
    assert_eq!(
        query(dir.path(), "SELECT max(revision) FROM team_revisions")?,
        "1"
    );
    // A change through the API stands while the file is unchanged.
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "api",
            None,
            1,
            json!({"bindings": {"developer": {"model": "m-api"}}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(status, 202, "{body}");
    let run = started(&host, "s2").await?;
    assert_eq!(model(&run)?, "m-api");
    // The file changes: the next StartRun makes it the next revision.
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"claude\"\nmodel = \"m-2\"\n[budget]\nmax_iterations = 7\n",
    )?;
    let run = started(&host, "s3").await?;
    assert_eq!(model(&run)?, "m-2");
    // The file's team replaces the team: the auditor it no longer has is not bound.
    assert_eq!(
        query(
            dir.path(),
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run}' AND role = 'auditor'"
            )
        )?,
        "0"
    );
    // A new run's budget is the file's at its start too.
    assert_eq!(
        query(
            dir.path(),
            &format!(
                "SELECT CAST(limit_value AS INTEGER) FROM budgets WHERE scope_id = '{run}' AND kind = 'iterations'"
            )
        )?,
        "7"
    );
    assert_eq!(
        source(3)?,
        format!("file/{}", sha(&std::fs::read_to_string(&file)?))
    );
    // Removed: the built-in defaults.
    std::fs::remove_file(&file)?;
    let run = started(&host, "s4").await?;
    assert_eq!(model(&run)?, "");
    assert_eq!(source(4)?, "builtin/");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn an_invalid_configuration_file_refuses_new_runs_and_keeps_the_revision() -> R {
    // REQ-018 failure mode 5 (ADR-022 decision 6): the host starts and serves; StartRun is 400 INVALID_TEAM_CONFIG with
    // the file's path, the key path, line and column; a config.invalid event is written once for that file; the team
    // revision made from the file before it became invalid stays, not the built-in defaults (M2T07R-259); no run is
    // made.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    std::fs::write(
        dir.path().join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.developer]\nagent = \"claude\"\nmodel = \"m-ok\"\n",
    )?;
    let host = start_when_free(config(dir.path())).await?;
    host.shutdown().await;
    std::fs::write(
        dir.path().join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.qa]\nmodle = \"x\"\n",
    )?;
    let host = start_when_free(config(dir.path())).await?;
    for key in ["s1", "s2"] {
        let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&start_run(key))).await?;
        assert_eq!(
            (status, body["code"].as_str()),
            (400, Some("INVALID_TEAM_CONFIG")),
            "{body}"
        );
        assert_eq!(body["details"]["key_path"], "team.qa.modle", "{body}");
        assert_eq!(body["details"]["line"], 3, "{body}");
        assert_eq!(body["details"]["column"], 1, "{body}");
        assert!(
            body["details"]["path"]
                .as_str()
                .is_some_and(|p| p.ends_with(".hoh/config.toml")),
            "{body}"
        );
    }
    assert_eq!(query(dir.path(), "SELECT count(*) FROM runs")?, "0");
    assert_eq!(
        query(
            dir.path(),
            "SELECT count(*) FROM events WHERE kind = 'config.invalid'"
        )?,
        "1"
    );
    assert_eq!(
        query(
            dir.path(),
            "SELECT coalesce(max(revision), 0) FROM team_revisions"
        )?,
        "1"
    );
    assert_eq!(
        query(
            dir.path(),
            "SELECT json_extract(config_json, '$.team.developer.model') FROM team_revisions WHERE revision = 1"
        )?,
        "m-ok"
    );
    let (status, _) = exchange(&host, "GET", "/v1/runs", None).await?;
    assert_eq!(status, 200);
    host.shutdown().await;
    Ok(())
}

/// Moves the team on, once armed, each time the host is about to submit the configuration file's revision: every try
/// is overtaken.
#[derive(Debug)]
struct MovesTheTeamOn {
    project: PathBuf,
    armed: std::sync::atomic::AtomicBool,
    moved: std::sync::atomic::AtomicUsize,
}

impl hoh_app::host::HostHooks for MovesTheTeamOn {
    fn before_file_revision(&self) {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.moved.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = exec(
                &self.project,
                "INSERT INTO team_revisions(project_id, revision, config_json, state, created_at) \
                 SELECT project_id, revision + 1, config_json, 'APPLIED', created_at FROM team_revisions \
                 ORDER BY revision DESC LIMIT 1;",
            );
        }
    }
}

#[tokio::test]
async fn a_start_run_whose_configuration_file_is_overtaken_every_time_is_refused_as_stale() -> R {
    // M2T07R-351, -454 (docs/adr/ADR-022.md decision 6): when the team moves on before each of the three tries to make
    // the file's revision current, StartRun is refused 409 STALE_REVISION (retryable) and makes no run; it is not
    // taken as applied.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    let file = dir.path().join(".hoh/config.toml");
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"claude\"\n",
    )?;
    let hooks = std::sync::Arc::new(MovesTheTeamOn {
        project: dir.path().to_path_buf(),
        armed: std::sync::atomic::AtomicBool::new(false),
        moved: std::sync::atomic::AtomicUsize::new(0),
    });
    let mut host_config = config(dir.path());
    host_config.hooks = hooks.clone();
    let host = start_when_free(host_config).await?;
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\n",
    )?;
    hooks.armed.store(true, std::sync::atomic::Ordering::SeqCst);
    let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&start_run("s1"))).await?;
    hooks
        .armed
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], json!("STALE_REVISION"), "{body}");
    assert_eq!(body["retryable"], json!(true), "{body}");
    assert_eq!(hooks.moved.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(query(dir.path(), "SELECT count(*) FROM runs")?, "0");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_start_run_whose_configuration_file_cannot_be_applied_is_refused_not_started_on_the_old_team()
-> R {
    // M2T07R-204 (ADR-022 decision 6): when the file's revision cannot be written, StartRun does not go on with the
    // previous team: it is refused as the store's failure is, and makes no run; once the file is applied, a run starts
    // on it.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    let file = dir.path().join(".hoh/config.toml");
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"claude\"\n",
    )?;
    let host = start_when_free(config(dir.path())).await?;
    exec(
        dir.path(),
        "CREATE TRIGGER refuse_revisions BEFORE INSERT ON team_revisions BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    )?;
    std::fs::write(
        &file,
        "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\n",
    )?;
    let (status, body) = exchange(&host, "POST", "/v1/commands", Some(&start_run("s1"))).await?;
    assert_ne!(status, 202, "{body}");
    assert!(body["code"].is_string(), "{body}");
    assert_eq!(query(dir.path(), "SELECT count(*) FROM runs")?, "0");
    exec(dir.path(), "DROP TRIGGER refuse_revisions;")?;
    let run = started(&host, "s2").await?;
    assert_eq!(
        query(
            dir.path(),
            &format!(
                "SELECT agent FROM role_bindings WHERE run_id = '{run}' AND role = 'developer'"
            )
        )?,
        "codex"
    );
    host.shutdown().await;
    Ok(())
}

/// One blocking POST of `body` to the host at `port` with `token`, from a thread of its own: its status and body.
fn post_blocking(port: u16, token: &str, body: &Value) -> R<(u16, Value)> {
    use std::io::{Read as _, Write as _};
    let body = body.to_string();
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.write_all(request.as_bytes())?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_019_04_two_concurrent_team_changes_one_202_one_409_one_epoch_step() -> R {
    // AC-019-04 (ADR-022 decision 3, ADR-024): a run on a spec with its runner; at team revision r two clients, two OS
    // threads released together, submit ConfigureTeam at expected_revision r, the commander to each of the two agents
    // it is not: exactly one 202, the other 409 with current_revision r+1; the binding becomes the accepted one and
    // the commander's epoch moves by exactly one. 50 rounds.
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Idle),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    exec(
        dir.path(),
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 's1', project_id, printf('%064d', 1), 1 \
             FROM projects; UPDATE runs SET state = 'RUNNING', spec_id = 's1', revision = revision + 1 \
             WHERE run_id = '{run_id}';"
        ),
    )?;
    let binding = format!(
        "SELECT agent || '/' || role_epoch FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander'"
    );
    let (port, token) = (host.port, host.token.clone());
    let agents = ["codex", "pi", "claude"];
    for round in 0..50_i64 {
        let now = query(dir.path(), &binding)?;
        let (agent, epoch) = now.split_once('/').ok_or("no binding")?;
        let epoch: i64 = epoch.parse()?;
        let r: i64 = query(
            dir.path(),
            "SELECT coalesce(max(revision), 0) FROM team_revisions",
        )?
        .parse()?;
        assert_eq!(r, round, "round {round}");
        let targets: Vec<&str> = agents.iter().copied().filter(|a| *a != agent).collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let threads: Vec<_> = targets
            .iter()
            .map(|target| {
                let (barrier, token) = (barrier.clone(), token.clone());
                let body = configure(
                    &format!("r{round}-{target}"),
                    None,
                    r,
                    json!({"bindings": {"commander": {"agent": target}}, "apply_at": "checkpoint"}),
                );
                let target = (*target).to_owned();
                std::thread::spawn(move || {
                    barrier.wait();
                    post_blocking(port, &token, &body)
                        .map(|reply| (target, reply))
                        .map_err(|e| e.to_string())
                })
            })
            .collect();
        let mut replies = Vec::new();
        for thread in threads {
            replies.push(thread.join().map_err(|_| "a client panicked")??);
        }
        replies.sort_by_key(|(_, (status, _))| *status);
        let [(accepted, (ok, body)), (_, (stale, refused))] = &replies[..] else {
            return Err("two replies expected".into());
        };
        assert_eq!((*ok, *stale), (202, 409), "round {round}: {replies:?}");
        assert_eq!(body["revision"], r + 1, "round {round}");
        assert_eq!(refused["code"], "STALE_REVISION", "round {round}");
        assert_eq!(refused["current_revision"], r + 1, "round {round}");
        // Applied at the run's checkpoint: the accepted agent, the epoch one step on, and never two.
        let want = format!("{accepted}/{}", epoch + 1);
        let end = std::time::Instant::now() + Duration::from_secs(20);
        let mut seen = query(dir.path(), &binding)?;
        while seen != want {
            let moved: i64 = seen.split_once('/').map_or(Ok(0), |(_, e)| e.parse())?;
            assert!(
                moved <= epoch + 1,
                "round {round}: {seen}, the epoch moved twice"
            );
            assert!(
                std::time::Instant::now() < end,
                "round {round}: {seen}, not {want}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
            seen = query(dir.path(), &binding)?;
        }
    }
    assert_eq!(
        query(dir.path(), &binding)?
            .split_once('/')
            .map(|(_, e)| e.to_owned()),
        Some("51".to_owned())
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn a_change_to_a_logged_out_agent_is_refused_blocked_auth_and_the_old_binding_works_on() -> R
{
    // REQ-019 failure mode 3 (docs/adr/ADR-024.md decision 7): claude is installed but its status command says it is
    // not logged in; a run on a spec, its commander codex at epoch 1. The commander's change to claude is refused at
    // its checkpoint (team.binding_refused, reason BLOCKED_AUTH, what to do in its detail): the commander keeps codex
    // and epoch 1, ticks later as well, with one refusal.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let help = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/claude/2.1.318/help.txt"
    );
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n --version*) echo '2.1.318 (Claude Code)';;\n --help*) /bin/cat '{help}';;\n \
             'auth status') echo '{{\"loggedIn\": false}}'; exit 1;;\n *) exit 2;;\nesac\n"
        ),
    )?;
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o755))?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let mut config = config(&project);
    let registry = std::sync::Arc::new(hoh_app::host::agents::Registry::new(
        bin.as_os_str().to_owned(),
    ));
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: std::sync::Arc::new(Registered(registry)),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    exec(
        &project,
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 's1', project_id, printf('%064d', 1), 1 \
             FROM projects; UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';"
        ),
    )?;
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "to-claude",
            None,
            0,
            json!({"bindings": {"commander": {"agent": "claude"}}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(status, 202, "{body}");
    let refused = "SELECT coalesce(group_concat(json_extract(payload_json, '$.reason') || '|' || \
                   json_extract(payload_json, '$.detail'), ','), '') FROM events WHERE kind = 'team.binding_refused'";
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while query(&project, refused)?.is_empty() {
        assert!(std::time::Instant::now() < end, "never refused");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        query(&project, refused)?,
        "BLOCKED_AUTH|运行 claude auth login 登录"
    );
    assert_eq!(
        query(
            &project,
            &format!(
                "SELECT agent || '/' || role_epoch FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander'"
            )
        )?,
        "codex/1"
    );
    host.shutdown().await;
    Ok(())
}

/// An executor with the product's registry and no work that holds back each first input while its hold file is
/// there: every offer is one line of its log, `<message id> <session id> <the commander's epoch in the database at the
/// offer> <held|took>`, so an offer of a notice its binding has already passed is seen as it is made.
#[derive(Debug)]
struct HeldFirstInputs {
    registry: std::sync::Arc<hoh_app::host::agents::Registry>,
    project: PathBuf,
    run_id: std::sync::Mutex<String>,
    hold: PathBuf,
    log: PathBuf,
}

impl hoh_app::host::Executor for HeldFirstInputs {
    fn registry(&self) -> Option<std::sync::Arc<hoh_app::host::agents::Registry>> {
        Some(self.registry.clone())
    }

    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        tokio::sync::oneshot::channel().1
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        Err("no work is expected".to_owned().into())
    }

    fn open_session(
        &self,
        start: &hoh_core::persistence::SessionStart,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        use std::io::Write as _;
        let (sent, answer) = tokio::sync::oneshot::channel();
        let run_id = self.run_id.lock().map(|r| r.clone()).unwrap_or_default();
        let epoch = query(
            &self.project,
            &format!("SELECT role_epoch FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander'"),
        )
        .unwrap_or_else(|e| format!("unread:{e}"));
        let held = self.hold.exists();
        let line = format!(
            "{} {} {epoch} {}\n",
            start.message_id,
            start.session_id.as_deref().unwrap_or("-"),
            if held { "held" } else { "took" }
        );
        let logged = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| e.to_string());
        let _ = sent.send(if held {
            Err("held by the test".to_owned())
        } else {
            logged
        });
        answer
    }
}

#[tokio::test]
async fn a_session_start_passed_by_the_checkpoint_of_the_same_tick_is_never_offered() -> R {
    // docs/adr/ADR-024.md decision 4 (M2T17R-01): the commander goes codex -> pi (epoch 2, its session bound, its first
    // input held back by the executor) -> claude. The tick whose checkpoint applies the second change must not offer
    // the epoch-2 notice to the executor, which a look at the waiting notices taken before that checkpoint would do;
    // the notice is superseded, and the epoch-3 one is the only first input taken.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&bin)?;
    let fixtures = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");
    std::fs::write(
        bin.join("pi"),
        format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '1.5.0';;\n --help) /bin/cat '{fixtures}/pi/1.5.0/help.txt';;\n \
             *) exit 2;;\nesac\n"
        ),
    )?;
    std::fs::write(
        bin.join("claude"),
        format!(
            "#!/bin/sh\ncase \"$1 $2\" in\n --version*) echo '2.1.318 (Claude Code)';;\n --help*) /bin/cat \
             '{fixtures}/claude/2.1.318/help.txt';;\n 'auth status') echo '{{\"loggedIn\": true}}';;\n *) exit 2;;\nesac\n"
        ),
    )?;
    for agent in ["pi", "claude"] {
        std::fs::set_permissions(bin.join(agent), std::fs::Permissions::from_mode(0o755))?;
    }
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let hold = dir.path().join("hold");
    std::fs::write(&hold, b"")?;
    let log = dir.path().join("first-inputs.log");
    let executor = std::sync::Arc::new(HeldFirstInputs {
        registry: std::sync::Arc::new(hoh_app::host::agents::Registry::new(
            bin.as_os_str().to_owned(),
        )),
        project: project.clone(),
        run_id: std::sync::Mutex::new(String::new()),
        hold: hold.clone(),
        log: log.clone(),
    });
    let mut config = config(&project);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: hoh_core::decision::Context::default(),
    });
    let host = start_when_free(config).await?;
    let run_id = started(&host, "s1").await?;
    *executor.run_id.lock().map_err(|e| e.to_string())? = run_id.clone();
    exec(
        &project,
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 's1', project_id, printf('%064d', 1), 1 \
             FROM projects; UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';"
        ),
    )?;
    let commander = format!(
        "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
         WHERE run_id = '{run_id}' AND role = 'commander'"
    );
    let offered = |epoch: u64| -> R<bool> {
        Ok(std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .any(|l| l.contains(&format!("/session-start/commander/{epoch} "))))
    };
    // Waits on the test's runtime, which the host shares: never a blocking sleep.
    async fn until(what: &str, done: &dyn Fn() -> R<bool>, seen: &dyn Fn() -> R<String>) -> R {
        let end = std::time::Instant::now() + Duration::from_secs(30);
        while !done()? {
            if std::time::Instant::now() > end {
                return Err(format!("{what}: {}", seen()?).into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        Ok(())
    }
    let seen = || query(&project, &commander);
    for (key, expected, agent) in [("to-pi", 0, "pi"), ("to-claude", 1, "claude")] {
        let (status, body) = exchange(
            &host,
            "POST",
            "/v1/commands",
            Some(&configure(
                key,
                None,
                expected,
                json!({"bindings": {"commander": {"agent": agent}}, "apply_at": "checkpoint"}),
            )),
        )
        .await?;
        assert_eq!(status, 202, "{body}");
        let epoch = expected as u64 + 2;
        let bound = format!("{agent}/{epoch}/{run_id}/session/commander/{epoch}");
        until(
            &format!("{agent} bound and offered"),
            &|| Ok(query(&project, &commander)? == bound && offered(epoch)?),
            &seen,
        )
        .await?;
    }
    std::fs::remove_file(&hold)?;
    let notice = |epoch: u64| {
        format!(
            "SELECT o.state || ':' || coalesce(json_extract(r.result_json, '$.rejected'), json_extract(r.result_json, \
             '$.session_id')) FROM outbox o LEFT JOIN receipts r ON r.message_id = o.message_id \
             WHERE o.message_id = '{run_id}/session-start/commander/{epoch}'"
        )
    };
    until(
        "the epoch-3 first input taken",
        &|| Ok(query(&project, &notice(3))?.starts_with("ACKED:")),
        &seen,
    )
    .await?;
    host.shutdown().await;
    let lines = std::fs::read_to_string(&log)?;
    for line in lines.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let notice_epoch = fields[0].rsplit('/').next().unwrap_or_default();
        assert_eq!(
            notice_epoch, fields[2],
            "offered a notice its binding had passed: {lines}"
        );
    }
    assert_eq!(
        query(&project, &notice(2))?,
        "ACKED:STALE_ROLE_EPOCH",
        "superseded"
    );
    assert_eq!(
        query(&project, &notice(3))?,
        format!("ACKED:{run_id}/session/commander/3")
    );
    let took: Vec<&str> = lines.lines().filter(|l| l.ends_with(" took")).collect();
    assert_eq!(took.len(), 1, "{lines}");
    assert!(
        took[0].starts_with(&format!("{run_id}/session-start/commander/3 ")),
        "{lines}"
    );
    Ok(())
}
