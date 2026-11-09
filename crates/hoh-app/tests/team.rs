//! The team on an in-process host (REQ-018, docs/adr/ADR-022.md): a started run's bindings in its snapshot and its
//! team page, and ConfigureTeam.

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
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
    // AC-018-04: with team revision r, five ConfigureTeams are refused: an unknown agent, removing qa, a shared
    // reviewer session and apply_at=now with 400 and the key path, a stale expected_revision with 409 and
    // current_revision=r; afterwards the revision is still r, no role epoch moved and no binding was applied.
    let dir = tempfile::tempdir()?;
    let host = start_when_free(config(dir.path())).await?;
    let run_id = started(&host, "s1").await?;
    // r = 1: one accepted change first, so that r-1 is still a revision.
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "ok",
            None,
            0,
            json!({"bindings": {"cto": {"model": "m-cto"}}, "apply_at": "checkpoint"}),
        )),
    )
    .await?;
    assert_eq!(
        (status, body["revision"].as_i64()),
        (202, Some(1)),
        "{body}"
    );
    let r = 1;
    let epochs = || {
        query(
            dir.path(),
            &format!(
                "SELECT group_concat(role || ':' || role_epoch, ',') FROM role_bindings WHERE run_id = '{run_id}'"
            ),
        )
    };
    let before = epochs()?;
    let checkpoint = |bindings: Value| json!({"bindings": bindings, "apply_at": "checkpoint"});
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
        let (status, body) = exchange(
            &host,
            "POST",
            "/v1/commands",
            Some(&configure(key, run, expected, payload)),
        )
        .await?;
        assert_eq!(status, 400, "{key}: {body}");
        assert_eq!(body["code"], "INVALID_TEAM_CONFIG", "{key}: {body}");
        assert_eq!(body["details"]["key_path"], key_path, "{key}: {body}");
        assert_eq!(body["current_revision"], r, "{key}: {body}");
        assert_eq!(body["retryable"], false, "{key}: {body}");
        assert!(
            body["message"].as_str().is_some_and(|m| !m.is_empty()),
            "{key}: {body}"
        );
    }
    let (status, body) = exchange(
        &host,
        "POST",
        "/v1/commands",
        Some(&configure(
            "stale",
            None,
            r - 1,
            checkpoint(json!({"developer": {"agent": "codex"}})),
        )),
    )
    .await?;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["code"], "STALE_REVISION", "{body}");
    assert_eq!(body["current_revision"], r, "{body}");
    assert_eq!(body["retryable"], false, "{body}");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
    assert_eq!(
        query(
            dir.path(),
            "SELECT max(revision) || '/' || count(*) FROM team_revisions"
        )?,
        format!("{r}/{}", r + 1)
    );
    assert_eq!(epochs()?, before);
    assert_eq!(
        query(
            dir.path(),
            "SELECT count(*) FROM events WHERE kind = 'team.binding_applied'"
        )?,
        "0"
    );
    assert_eq!(
        query(
            dir.path(),
            "SELECT count(*) FROM commands WHERE kind = 'ConfigureTeam'"
        )?,
        "1",
        "a refused command is not recorded"
    );
    host.shutdown().await;
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
        json!({"agent": "codex", "model": "m-test", "config_revision": 1})
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
