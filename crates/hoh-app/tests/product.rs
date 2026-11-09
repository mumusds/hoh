//! The product executor in a host (docs/adr/ADR-022.md decisions 9 to 17): the roles' agents, here fake CLIs answering
//! the probe from their recordings and replaying recorded turns, run through `hoh __gate` (TEST-027-03 in the host,
//! TEST-018-02, -03, -05).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const PROTOCOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");
const APP_SERVER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hoh-agents/tests/fakes/app_server.py"
);
const PI_RPC: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../hoh-agents/tests/fakes/pi_rpc.py"
);

/// A line of 8 MiB with no LF (AC-027-03).
const HUGE: usize = 8 << 20;

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

/// A test project with fake agent CLIs of its own: the project, the fakes' directory, and its temporary root.
struct World {
    dir: tempfile::TempDir,
}

impl World {
    fn new() -> R<World> {
        let dir = tempfile::tempdir()?;
        std::fs::create_dir_all(dir.path().join("project"))?;
        std::fs::create_dir_all(dir.path().join("bin"))?;
        Ok(World { dir })
    }

    fn project(&self) -> PathBuf {
        self.dir.path().join("project")
    }

    fn bin(&self) -> PathBuf {
        self.dir.path().join("bin")
    }

    /// The product's host of the project, its agents found only among the fakes (never an agent installed on this
    /// machine).
    fn config(&self) -> HostConfig {
        let path = format!("{}:/usr/bin:/bin", self.bin().display());
        let mut config = HostConfig::product(
            &self.project(),
            hoh_core::config::Config::default(),
            env!("CARGO_BIN_EXE_hoh").into(),
            path.into(),
        );
        config.port = 0;
        config.store.wal_probe = Some(probe());
        config
    }

    fn script(&self, name: &str, body: &str) -> R {
        let path = self.bin().join(name);
        std::fs::write(&path, format!("#!/bin/bash\n{body}\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::create_dir_all(self.bin().join(format!("{name}.d")))?;
        Ok(())
    }

    /// A fake `claude` 2.1.318: the probe's answers from the recording; a turn reads its prompt, then replays the
    /// recorded `success` turn (once `hold` exists, if given), or, for an attempt whose marker names HUGE, its first
    /// frame and then 8 MiB with no LF.
    fn claude(&self, hold: Option<&Path>) -> R {
        let wait = hold.map_or(String::new(), |h| {
            format!("while [ ! -e '{}' ]; do sleep 0.05; done\n", h.display())
        });
        self.script(
            "claude",
            &format!(
                "f='{PROTOCOL}/claude/2.1.318'; d='{bin}/claude.d'\n\
                 case \"$1\" in --version) echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"; env > \"$d/env.$$\"; cat > \"$d/stdin.$$\"\n\
                 case \"$HOH_ATTEMPT_ID\" in\n\
                  *HUGE*) head -n 1 \"$f/success/stdout.jsonl\"; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
                 esac\n{wait}cat \"$f/success/stdout.jsonl\"\n",
                bin = self.bin().display()
            ),
        )
    }

    /// A fake `codex` 0.166.0: the probe's answers (helps, App Server schema) from the recording; `app-server`
    /// replays the recorded `app-server-turn`, its model given as `model`, or, for an attempt whose marker names HUGE,
    /// the frames before `turn/completed` and then 8 MiB with no LF.
    fn codex(&self, model: &str) -> R {
        let d = self.bin().join("codex.d");
        std::fs::create_dir_all(&d)?;
        let case = format!("{PROTOCOL}/codex/0.166.0/app-server-turn");
        let frames: Vec<Value> = std::fs::read_to_string(format!("{case}/stdout.jsonl"))?
            .lines()
            .map(|l| serde_json::from_str(&l.replace("gpt-6-astra", model)))
            .collect::<Result<_, _>>()?;
        let mut requests = serde_json::Map::new();
        for line in std::fs::read_to_string(format!("{case}/stdin.txt"))?.lines() {
            let Ok(m) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(method)) = (m["id"].as_i64(), m["method"].as_str()) {
                requests.insert(id.to_string(), json!(method));
            }
        }
        let lines =
            |frames: &[Value]| -> String { frames.iter().map(|f| format!("{f}\n")).collect() };
        std::fs::write(d.join("frames.jsonl"), lines(&frames))?;
        let end = frames
            .iter()
            .position(|f| f["method"] == "turn/completed")
            .ok_or("no turn/completed")?;
        let mut huge = frames[..end].to_vec();
        huge.push(json!({"hoh_raw": {"byte": "x", "count": HUGE}}));
        std::fs::write(d.join("huge.jsonl"), lines(&huge))?;
        std::fs::write(d.join("requests.json"), Value::Object(requests).to_string())?;
        self.script(
            "codex",
            &format!(
                "f='{PROTOCOL}/codex/0.166.0'; d='{d}'\n\
                 case \"$1\" in\n\
                  --version) echo 'codex-cli 0.166.0'; exit 0;;\n\
                  --help) cat \"$f/help.txt\"; exit 0;;\n\
                  exec) [ \"$2\" = --help ] && {{ cat \"$f/help-exec.txt\"; exit 0; }}; exit 2;;\n\
                  app-server) case \"$2\" in\n\
                    --help) cat \"$f/help-app-server.txt\"; exit 0;;\n\
                    generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R \"$f/app-server-schema/.\" \"$4/\"; exit 0;;\n\
                   esac\n\
                   echo $$ > \"$d/pid.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"\n\
                   case \"$HOH_ATTEMPT_ID\" in *HUGE*) frames=\"$d/huge.jsonl\";; *) frames=\"$d/frames.jsonl\";; esac\n\
                   exec timeout 60 python3 '{APP_SERVER}' \"$frames\" \"$d/requests.json\" \"$d/log.$$\";;\n\
                 esac\nexit 2\n",
                d = d.display()
            ),
        )
    }

    /// A fake `pi` 1.5.0: the probe's answers from the recording; a turn replays the recorded `rpc-prompt`, or, for an
    /// attempt whose marker names HUGE, one record and then 8 MiB with no LF.
    fn pi(&self) -> R {
        let d = self.bin().join("pi.d");
        std::fs::create_dir_all(&d)?;
        let case = format!("{PROTOCOL}/pi/1.5.0/rpc-prompt");
        std::fs::copy(format!("{case}/stdout.jsonl"), d.join("frames.jsonl"))?;
        let mut commands = serde_json::Map::new();
        for line in std::fs::read_to_string(format!("{case}/stdin.txt"))?.lines() {
            let Ok(c) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let (Some(id), Some(kind)) = (c["id"].as_str(), c["type"].as_str()) {
                commands.insert(id.to_owned(), json!(kind));
            }
        }
        std::fs::write(d.join("commands.json"), Value::Object(commands).to_string())?;
        self.script(
            "pi",
            &format!(
                "f='{PROTOCOL}/pi/1.5.0'; d='{d}'\n\
                 case \"$1\" in --version) echo '1.5.0'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"\n\
                 case \"$HOH_ATTEMPT_ID\" in\n\
                  *HUGE*) printf '%s\\n' '{{\"type\":\"agent_start\"}}'; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
                 esac\n\
                 exec timeout 60 python3 '{PI_RPC}' \"$d/frames.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\"\n",
                d = d.display()
            ),
        )
    }

    /// Every fake turn process gone: the ones still there are killed (nothing a test started outlives it).
    fn stop_fakes(&self) -> R {
        for name in ["claude", "codex", "pi"] {
            let d = self.bin().join(format!("{name}.d"));
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(pid) = file.strip_prefix("pid.")
                    && Path::new(&format!("/proc/{pid}")).exists()
                {
                    let _ = Command::new("kill").args(["-KILL", pid]).status();
                }
            }
        }
        Ok(())
    }
}

async fn start_when_free(config: HostConfig) -> Result<Host, hoh_app::host::StartError> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && Instant::now() < end => {
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
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1], timeout=10).execute(sys.argv[2]).fetchone(); \
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

/// A started run's id.
async fn started(host: &Host, key: &str) -> R<String> {
    let body = json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": 0, "kind": "StartRun", "run_id": null,
        "payload": {"goal": "Ship the parser", "mode": "AUTO", "interactive": false},
    });
    let (status, accepted) = exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{accepted}");
    Ok(accepted["run_id"].as_str().ok_or("no run_id")?.to_owned())
}

/// Submit ConfigureTeam `bindings` at `expected`; its accepted body.
async fn configure(host: &Host, key: &str, expected: i64, bindings: Value) -> R<Value> {
    let body = json!({
        "schema_version": "1.0", "command_id": format!("c-{key}"), "idempotency_key": key,
        "expected_revision": expected, "kind": "ConfigureTeam", "run_id": null,
        "payload": {"bindings": bindings, "apply_at": "checkpoint"},
    });
    let (status, accepted) = exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "{accepted}");
    Ok(accepted)
}

/// Run `run_id` RUNNING on a spec, with `tasks` READY: (task id, role), readers.
fn ready(project: &Path, run_id: &str, tasks: &[(&str, &str)]) -> R {
    let mut sql = format!(
        "INSERT OR IGNORE INTO specs(spec_id, project_id, sha256, revision) \
         SELECT 's1', project_id, printf('%064d', 1), 1 FROM runs WHERE run_id = '{run_id}';
         UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';"
    );
    for (task, role) in tasks {
        sql.push_str(&format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
             VALUES ('{task}', '{run_id}', 's1', 'READY', 1, '{role}', 0, \
             (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);"
        ));
    }
    exec(project, &sql)
}

/// Wait until `sql` gives `want`, for at most `limit`.
async fn until(project: &Path, sql: &str, want: &str, limit: Duration) -> R {
    let end = Instant::now() + limit;
    loop {
        let got = query(project, sql)?;
        if got == want {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{sql}: {got:?}, not {want:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The query of attempt `attempt_id`'s state and failure reason once it ended.
fn ended(attempt_id: &str) -> String {
    format!(
        "SELECT state || '/' || coalesce(failure_reason, '') FROM attempts WHERE attempt_id = '{attempt_id}' \
         AND state IN ('SETTLED', 'FAILED', 'CANCELLED')"
    )
}

/// AC-027-03 in the host for `agent`: the developer bound to it, two developer tasks run side by side; the one whose
/// agent writes its normal frames and then 8 MiB with no LF fails PROTOCOL_FRAME_TOO_LARGE with its diagnosis (length,
/// prefix sha256, first 4 KiB, the reader's maximum within the limit and one read), the other settles, and the host
/// goes on serving.
async fn an_oversized_frame_fails_only_its_attempt(agent: &str) -> R {
    let world = World::new()?;
    match agent {
        "claude" => world.claude(None)?,
        "codex" => world.codex("gpt-6-astra")?,
        _ => world.pi()?,
    }
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        if agent != "claude" {
            configure(&host, "dev", 0, json!({"developer": {"agent": agent}})).await?;
        }
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T-OK", "developer"), ("T-HUGE", "developer")])?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &ended("T-HUGE/attempt-1"),
            "FAILED/PROTOCOL_FRAME_TOO_LARGE",
            limit,
        )
        .await?;
        until(&project, &ended("T-OK/attempt-1"), "SETTLED/", limit).await?;
        // Side by side: the oversized one was running while the other did.
        let overlap = query(
            &project,
            "SELECT count(*) FROM attempts a, attempts b WHERE a.attempt_id = 'T-HUGE/attempt-1' \
             AND b.attempt_id = 'T-OK/attempt-1' AND a.started_at <= b.ended_at AND b.started_at <= a.ended_at",
        )?;
        assert_eq!(overlap, "1", "the two attempts did not overlap");
        let diagnosis: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'adapter.diagnostic' \
             AND event_id LIKE 'T-HUGE/attempt-1/%' AND json_extract(payload_json, '$.reason') = 'PROTOCOL_FRAME_TOO_LARGE'",
        )?)?;
        let ceiling = 1_048_576 + 65_536;
        let length = usize::try_from(diagnosis["length"].as_u64().ok_or("no length")?)?;
        assert!(length > 1 << 20 && length <= ceiling, "{diagnosis}");
        let buffered = diagnosis["reader_max_buffered"]
            .as_u64()
            .ok_or("no reader maximum")?;
        assert!(
            usize::try_from(buffered)? >= length && usize::try_from(buffered)? <= ceiling,
            "{diagnosis}"
        );
        let digest = <sha2::Sha256 as sha2::Digest>::digest(vec![b'x'; length]);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(diagnosis["prefix_sha256"], json!(hex));
        assert_eq!(diagnosis["head"], json!("x".repeat(4096)));
        let logs = query(
            &project,
            "SELECT raw_log_path FROM attempts WHERE attempt_id = 'T-HUGE/attempt-1'",
        )?;
        let kept = std::fs::read_to_string(Path::new(&logs).join("diagnostics.jsonl"))?;
        assert!(kept.contains("PROTOCOL_FRAME_TOO_LARGE"), "{kept}");
        let (status, _) = exchange(&host, "GET", &format!("/v1/runs/{run_id}"), None).await?;
        assert_eq!(status, 200, "the host goes on serving");
        if let Ok(dir) = std::env::var("HOH_EVIDENCE_DIR") {
            let out = Path::new(&dir).join(format!("test-027-03-{agent}.json"));
            std::fs::write(
                out,
                serde_json::to_vec_pretty(&json!({"agent": agent, "diagnosis": diagnosis,
                    "oversized": "FAILED/PROTOCOL_FRAME_TOO_LARGE", "beside": "SETTLED", "host_serving": true}))?,
            )?;
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn test_027_03_claude_an_oversized_frame_in_the_host_fails_only_its_attempt() -> R {
    an_oversized_frame_fails_only_its_attempt("claude").await
}

#[tokio::test]
async fn test_027_03_codex_an_oversized_frame_in_the_host_fails_only_its_attempt() -> R {
    an_oversized_frame_fails_only_its_attempt("codex").await
}

#[tokio::test]
async fn test_027_03_pi_an_oversized_frame_in_the_host_fails_only_its_attempt() -> R {
    an_oversized_frame_fails_only_its_attempt("pi").await
}

/// Keep `value` as this test's evidence, when the check gives a directory for it.
fn keep(name: &str, value: &Value) -> R {
    if let Ok(dir) = std::env::var("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

/// An attempt's agent, resolved model, epoch, config revision and session, as JSON.
fn attempt_row(project: &Path, attempt_id: &str) -> R<Value> {
    Ok(serde_json::from_str(&query(
        project,
        &format!(
            "SELECT json_object('agent', agent, 'resolved_model', resolved_model, 'role_epoch', role_epoch, \
             'config_revision', config_revision, 'session_id', session_id, 'state', state) FROM attempts \
             WHERE attempt_id = '{attempt_id}'"
        ),
    )?)?)
}

#[tokio::test]
async fn test_018_02_a_team_change_waits_for_the_running_attempt_and_the_next_one_has_the_new_binding()
-> R {
    // AC-018-02: developer A1 runs under claude at epoch 1, revision r; ConfigureTeam developer -> codex, m-test at r is
    // accepted; A1 keeps claude and epoch 1; once A1 settled, the next developer attempt A2 runs codex, resolved model
    // m-test, epoch 2, revision r+1.
    let world = World::new()?;
    let hold = world.dir.path().join("release-A1");
    world.claude(Some(&hold))?;
    world.codex("m-test")?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T1", "developer")])?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T1/attempt-1'",
            "RUNNING",
            limit,
        )
        .await?;
        let revision = query(
            &project,
            "SELECT coalesce(max(revision), 0) FROM team_revisions",
        )?;
        let r: i64 = revision.parse()?;
        let accepted = configure(
            &host,
            "dev",
            r,
            json!({"developer": {"agent": "codex", "model": "m-test"}}),
        )
        .await?;
        assert_eq!(accepted["status"], "ACCEPTED", "{accepted}");
        // A few ticks with A1 running: nothing changes for it.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let a1_running = attempt_row(&project, "T1/attempt-1")?;
        assert_eq!(
            (
                &a1_running["agent"],
                &a1_running["role_epoch"],
                &a1_running["state"]
            ),
            (&json!("claude"), &json!(1), &json!("RUNNING"))
        );
        std::fs::write(&hold, "")?;
        until(&project, &ended("T1/attempt-1"), "SETTLED/", limit).await?;
        ready(&project, &run_id, &[("T2", "developer")])?;
        until(&project, &ended("T2/attempt-1"), "SETTLED/", limit).await?;
        let a1 = attempt_row(&project, "T1/attempt-1")?;
        let a2 = attempt_row(&project, "T2/attempt-1")?;
        assert_eq!(
            (&a1["agent"], &a1["role_epoch"], &a1["config_revision"]),
            (&json!("claude"), &json!(1), &json!(r))
        );
        assert_eq!(
            (
                &a2["agent"],
                &a2["resolved_model"],
                &a2["role_epoch"],
                &a2["config_revision"]
            ),
            (&json!("codex"), &json!("m-test"), &json!(2), &json!(r + 1))
        );
        // codex was asked for the binding's model, and reported it.
        let asked: Vec<Value> = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|m| m["method"] == "thread/start")
            .map(|m| m["params"]["model"].clone())
            .collect();
        assert_eq!(asked, [json!("m-test")]);
        keep(
            "test-018-02",
            &json!({"team_revision_before": r, "accepted": accepted, "a1": a1, "a2": a2}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn test_018_03_each_role_has_its_own_session_and_an_agents_exit_changes_nothing() -> R {
    // AC-018-03: cto and developer both bound to claude; a cto task and a developer task each run in their role's own
    // session (sessions.role cto and developer); the developer's agent process has ended; a third task, the
    // developer's, runs in the same session as its first, resumed; the team revision and the cto's session are
    // unchanged.
    let world = World::new()?;
    world.claude(None)?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let revision = query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?;
        ready(&project, &run_id, &[("T-cto", "cto"), ("T-dev", "developer")])?;
        let limit = Duration::from_secs(60);
        until(&project, &ended("T-cto/attempt-1"), "SETTLED/", limit).await?;
        until(&project, &ended("T-dev/attempt-1"), "SETTLED/", limit).await?;
        let cto = attempt_row(&project, "T-cto/attempt-1")?;
        let dev = attempt_row(&project, "T-dev/attempt-1")?;
        assert_ne!(cto["session_id"], dev["session_id"]);
        let role_of = |session: &Value| {
            query(
                &project,
                &format!(
                    "SELECT role FROM sessions WHERE session_id = '{}'",
                    session.as_str().unwrap_or_default()
                ),
            )
        };
        assert_eq!(role_of(&cto["session_id"])?, "cto");
        assert_eq!(role_of(&dev["session_id"])?, "developer");
        // claude was available all along: neither of its roles was ever blocked, even before the registry first
        // looked at it.
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM events WHERE kind = 'team.binding_blocked' \
                 AND json_extract(payload_json, '$.role') IN ('cto', 'developer')"
            )?,
            "0"
        );
        // The developer's agent process is gone.
        let pid = query(&project, "SELECT pid FROM attempts WHERE attempt_id = 'T-dev/attempt-1'")?;
        assert!(!pid.is_empty() && !Path::new(&format!("/proc/{pid}")).exists(), "{pid}");
        ready(&project, &run_id, &[("T-dev2", "developer")])?;
        until(&project, &ended("T-dev2/attempt-1"), "SETTLED/", limit).await?;
        let dev2 = attempt_row(&project, "T-dev2/attempt-1")?;
        assert_eq!(dev2["session_id"], dev["session_id"]);
        assert_eq!(
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?,
            revision
        );
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT session_id FROM role_bindings WHERE run_id = '{run_id}' AND role = 'cto'"
                )
            )?,
            cto["session_id"].as_str().unwrap_or_default()
        );
        let native = query(
            &project,
            &format!(
                "SELECT native_id FROM sessions WHERE session_id = '{}'",
                dev["session_id"].as_str().unwrap_or_default()
            ),
        )?;
        assert!(!native.is_empty(), "the developer's own session was not recorded");
        let resumed = std::fs::read_dir(world.bin().join("claude.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("argv."))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .any(|argv| argv.lines().any(|a| a == native));
        assert!(resumed, "no turn resumed {native}");
        // Every turn was started through the start gate, which gave it its attempt's marker.
        let markers: Vec<String> = std::fs::read_dir(world.bin().join("claude.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("env."))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|env| {
                env.lines()
                    .find_map(|l| l.strip_prefix("HOH_ATTEMPT_ID=").map(str::to_owned))
            })
            .collect();
        assert_eq!(markers.len(), 3, "{markers:?}");
        for attempt in ["T-cto/attempt-1", "T-dev/attempt-1", "T-dev2/attempt-1"] {
            assert!(
                markers.iter().any(|m| m.ends_with(&format!("/{attempt}"))),
                "{attempt}: {markers:?}"
            );
        }
        keep(
            "test-018-03",
            &json!({"cto": cto, "developer": dev, "developer_again": dev2, "native_session": native,
                    "team_revision": revision}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn test_018_05_a_custom_role_from_the_file_runs_its_override_prompt_in_its_own_session() -> R
{
    // AC-018-05: the project's file adds [team.security_reviewer] agent=codex with a prompt override file; once the
    // configuration is loaded, the snapshot's team has security_reviewer at epoch 1; its task's attempt records the
    // override file and its sha256 as its prompt's source (and the sha256 of what was sent); its session is not the
    // reviewer's.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh/prompts"))?;
    let contract = "你是安全审查者。只报告安全问题。\n";
    std::fs::write(project.join(".hoh/prompts/security.md"), contract)?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.security_reviewer]\nagent = \"codex\"\nprompt = \".hoh/prompts/security.md\"\n",
    )?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let (status, snapshot) = exchange(&host, "GET", &format!("/v1/runs/{run_id}"), None).await?;
        assert_eq!(status, 200, "{snapshot}");
        let member = snapshot["projection"]["team"]
            .as_array()
            .and_then(|team| team.iter().find(|m| m["role"] == "security_reviewer"))
            .cloned()
            .ok_or(format!("no security_reviewer: {snapshot}"))?;
        assert_eq!(member["role_epoch"], 1, "{member}");
        ready(&project, &run_id, &[("T-sec", "security_reviewer")])?;
        until(&project, &ended("T-sec/attempt-1"), "SETTLED/", Duration::from_secs(60)).await?;
        let record: Value = serde_json::from_str(&query(
            &project,
            "SELECT json_object('layer', p.layer, 'role', p.role, 'path', p.path, 'content_sha256', p.content_sha256, \
             'sent_sha256', a.prompt_sha256, 'session_id', a.session_id) FROM attempts a \
             JOIN prompt_revisions p ON p.revision_id = a.prompt_revision WHERE a.attempt_id = 'T-sec/attempt-1'",
        )?)?;
        let sha = |bytes: &[u8]| -> String {
            <sha2::Sha256 as sha2::Digest>::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(
            (&record["layer"], &record["role"], &record["path"]),
            (
                &json!("project"),
                &json!("security_reviewer"),
                &json!(".hoh/prompts/security.md")
            )
        );
        assert_eq!(record["content_sha256"], json!(sha(contract.as_bytes())));
        // What codex was sent: the contract, then the task's header; its sha256 is the attempt's.
        let sent: Vec<String> = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|m| m["method"] == "turn/start")
            .filter_map(|m| m["params"]["input"][0]["text"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert!(sent[0].starts_with(contract.trim_end()), "{}", sent[0]);
        assert!(sent[0].contains("T-sec"), "{}", sent[0]);
        assert_eq!(record["sent_sha256"], json!(sha(sent[0].as_bytes())));
        let reviewer = query(
            &project,
            &format!("SELECT session_id FROM role_bindings WHERE run_id = '{run_id}' AND role = 'reviewer'"),
        )?;
        assert!(!reviewer.is_empty(), "the reviewer has no session");
        assert_ne!(record["session_id"], json!(reviewer));
        keep(
            "test-018-05",
            &json!({"team_member": member, "prompt": record, "reviewer_session": reviewer}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_change_to_an_agent_not_installed_is_refused_until_it_is_and_the_old_binding_works_on()
-> R {
    // ADR-022 decision 8 third version and decision 10 (REQ-019 decision 2): each tick the registry looks at the
    // team's agents before the checkpoint; a change to one not installed is refused once, the old binding still runs
    // the role's tasks; once it is installed, the change is applied at the role's next checkpoint.
    let world = World::new()?;
    world.claude(None)?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        configure(&host, "dev", 0, json!({"developer": {"agent": "pi"}})).await?;
        let limit = Duration::from_secs(30);
        until(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'team.binding_refused'",
            "1",
            limit,
        )
        .await?;
        ready(&project, &run_id, &[("T1", "developer")])?;
        until(&project, &ended("T1/attempt-1"), "SETTLED/", limit).await?;
        assert_eq!(
            attempt_row(&project, "T1/attempt-1")?["agent"],
            json!("claude")
        );
        // A few more ticks: still one refusal.
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            query(&project, "SELECT count(*) FROM events WHERE kind = 'team.binding_refused'")?,
            "1"
        );
        world.pi()?;
        until(
            &project,
            &format!(
                "SELECT agent || '/' || role_epoch FROM role_bindings WHERE run_id = '{run_id}' AND role = 'developer'"
            ),
            "pi/2",
            limit,
        )
        .await?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_turn_that_cannot_start_fails_by_its_refusals_reason_and_class() -> R {
    // ADR-022 decisions 11 and 12: a prompt override gone at the start fails the attempt PROMPT_UNAVAILABLE (transient:
    // its task waits for a retry), never another contract in its place; an agent gone from the host since its binding
    // fails it AGENT_UNAVAILABLE (capability: its task is blocked); each with an attempt.start_refused event saying why.
    let world = World::new()?;
    world.claude(None)?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh/prompts"))?;
    std::fs::write(project.join(".hoh/prompts/audit.md"), "Audit.\n")?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.auditor]\nagent = \"claude\"\nprompt = \".hoh/prompts/audit.md\"\n",
    )?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(30);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role IN ('auditor', 'developer') \
                 AND session_id IS NOT NULL"
            ),
            "2",
            limit,
        )
        .await?;
        std::fs::remove_file(project.join(".hoh/prompts/audit.md"))?;
        ready(&project, &run_id, &[("T-a", "auditor")])?;
        until(&project, &ended("T-a/attempt-1"), "FAILED/PROMPT_UNAVAILABLE", limit).await?;
        let refused = |attempt: &str| {
            query(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.detail') FROM events WHERE kind = 'attempt.start_refused' \
                     AND event_id = '{attempt}/start-refused'"
                ),
            )
        };
        assert!(refused("T-a/attempt-1")?.contains("audit.md"), "{}", refused("T-a/attempt-1")?);
        assert_ne!(
            query(&project, "SELECT state FROM tasks WHERE task_id = 'T-a'")?,
            "BLOCKED"
        );
        assert_eq!(
            query(&project, "SELECT count(*) FROM prompt_revisions")?,
            "0",
            "no contract was sent in its place"
        );
        std::fs::remove_file(world.bin().join("claude"))?;
        // A tick looks at the team's agents: claude is gone.
        tokio::time::sleep(Duration::from_millis(600)).await;
        ready(&project, &run_id, &[("T-d", "developer")])?;
        until(&project, &ended("T-d/attempt-1"), "FAILED/AGENT_UNAVAILABLE", limit).await?;
        assert!(!refused("T-d/attempt-1")?.is_empty());
        assert_eq!(
            query(
                &project,
                "SELECT state || '/' || blocked_reason FROM tasks WHERE task_id = 'T-d'"
            )?,
            "BLOCKED/BLOCKED_CAPABILITY"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}
