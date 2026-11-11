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

/// The CANCEL turn: SIGINT and SIGTERM blocked before `claude.d/cancel-ready` is made and taken with sigtimedwait, each
/// written to `claude.d/signals` with the nanosecond it came; a blocked signal stays pending, so none is lost in a
/// gap of a shell's own handling (the bash trap this replaces is the stand-in whose SIGINT was not recorded in CI run
/// 42775489697, cause unproven; ADR-022 decision 10, convention v2). It prints the recorded turn's first frame, exits
/// 143 on SIGTERM and 0 after 30 s.
const CANCEL_STAND_IN: &str = "import os, signal, sys, time
d = sys.argv[1]
wanted = [signal.SIGINT, signal.SIGTERM]
signal.pthread_sigmask(signal.SIG_BLOCK, wanted)
sys.stdout.write(open(sys.argv[2]).readline())
sys.stdout.flush()
open(os.path.join(d, \"cancel-ready\"), \"w\").close()
end = time.monotonic() + 30
for info in iter(lambda: signal.sigtimedwait(wanted, max(0.0, end - time.monotonic())), None): f = open(os.path.join(d, \"signals\"), \"a\"); f.write((\"INT\" if info.si_signo == signal.SIGINT else \"TERM\") + \" \" + str(time.time_ns()) + chr(10)); f.close(); info.si_signo == signal.SIGTERM and sys.exit(143)
";

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
        // The tools the fakes use, and nothing else from the system's PATH: an agent installed there is never found
        // (M2T07R-259).
        let tools = dir.path().join("tools");
        std::fs::create_dir_all(&tools)?;
        for tool in [
            "cat", "cp", "date", "env", "head", "mkdir", "python3", "seq", "sleep", "timeout", "tr",
        ] {
            let found = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .ok_or(format!("no {tool}"))?;
            std::os::unix::fs::symlink(found, tools.join(tool))?;
        }
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
        self.config_of(hoh_core::config::Config::default())
    }

    /// The product's host as [`World::config`], with the effective configuration `effective`.
    fn config_of(&self, effective: hoh_core::config::Config) -> HostConfig {
        let path = format!(
            "{}:{}",
            self.bin().display(),
            self.dir.path().join("tools").display()
        );
        // pi's settings from a directory of the world's own, never the user's ~/.pi/agent (M2T17R-59).
        let registry = hoh_app::host::agents::Registry::with_pi_dir(
            path.into(),
            Some(self.dir.path().join("pi-agent")),
        );
        let mut config = HostConfig::product_with(
            &self.project(),
            effective,
            env!("CARGO_BIN_EXE_hoh").into(),
            std::sync::Arc::new(registry),
        );
        config.port = 0;
        config.store.wal_probe = Some(probe());
        // Commander sessions' own directories in the world's, never the user's state directory (ADR-025 decision 3).
        if let Some(runner) = config.runner.as_mut() {
            runner.context.commander_dir = Some(self.commander_dir());
        }
        config
    }

    /// Where this world's Commander sessions get their own directories.
    fn commander_dir(&self) -> PathBuf {
        self.dir.path().join("state/hoh/commander")
    }

    /// Bind every built-in role whose agent this world did not install to one it did (claude, then codex, then pi), in
    /// the project's `.hoh/config.toml` (appended; a new run reads its team from it): no binding is blocked, so nothing
    /// wakes a Commander the world cannot run and its runs are not BLOCKED for it (docs/adr/ADR-025.md decision 5). For
    /// the tests whose subject is the executor, not the team or its supervision; call it after installing the fakes
    /// and after writing the project's own file.
    fn bind_installed(&self) -> R {
        use hoh_core::config::Agent;
        use std::io::Write as _;
        let installed: Vec<Agent> = [Agent::Claude, Agent::Codex, Agent::Pi]
            .into_iter()
            .filter(|a| self.bin().join(a.command()).exists())
            .collect();
        let Some(first) = installed.first() else {
            return Ok(());
        };
        let file = self.project().join(".hoh/config.toml");
        std::fs::create_dir_all(self.project().join(".hoh"))?;
        let mut text = if file.exists() {
            String::new()
        } else {
            "config_version = \"1.0\"\n".to_owned()
        };
        for (role, binding) in hoh_core::config::Config::default().team {
            if !installed.contains(&binding.agent) {
                text.push_str(&format!("[team.{role}]\nagent = \"{}\"\n", first.as_str()));
            }
        }
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(file)?
            .write_all(text.as_bytes())?;
        Ok(())
    }

    fn script(&self, name: &str, body: &str) -> R {
        let path = self.bin().join(name);
        std::fs::write(&path, format!("#!/bin/bash\n{body}\n"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        std::fs::create_dir_all(self.bin().join(format!("{name}.d")))?;
        Ok(())
    }

    /// A fake `claude` 2.1.318: the probe's answers from the recording (`--version` 3 s late while `claude.d/slow`
    /// exists, 120 s with its pid in `claude.d/probe-pid` while `claude.d/hang` does); a turn reads its prompt, then replays the recorded `success` turn (once `hold` exists, if given), or, for
    /// an attempt whose marker names HUGE, its first frame and then 8 MiB with no LF, or, for one whose marker names
    /// CANCEL, [`CANCEL_STAND_IN`].
    fn claude(&self, hold: Option<&Path>) -> R {
        let wait = hold.map_or(String::new(), |h| {
            format!("while [ ! -e '{}' ]; do sleep 0.05; done\n", h.display())
        });
        self.script(
            "claude",
            &format!(
                "f='{PROTOCOL}/claude/2.1.318'; d='{bin}/claude.d'\n\
                 case \"$1\" in --version) [ -e \"$d/hang\" ] && {{ echo $$ > \"$d/probe-pid\"; sleep 120; }}; [ -e \"$d/slow\" ] && sleep 3; echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"; env > \"$d/env.$$\"; cat > \"$d/stdin.$$\"\n\
                 case \"$HOH_ATTEMPT_ID\" in\n\
                  *HUGE*) head -n 1 \"$f/success/stdout.jsonl\"; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
                  *CANCEL*) exec python3 -c '{CANCEL_STAND_IN}' \"$d\" \"$f/success/stdout.jsonl\";;\n\
                 esac\n{wait}cat \"$f/success/stdout.jsonl\"\n",
                bin = self.bin().display()
            ),
        )
    }

    /// A fake `codex` 0.166.0: the probe's answers (helps, App Server schema) from the recording (while
    /// `codex.d/hang-schema` exists, generating the schema waits 120 s, its pid in `codex.d/schema-pid` and its child's
    /// in `codex.d/schema-member`); `app-server`
    /// replays the recorded `app-server-turn`, its model given as `model`; for an attempt whose marker names HUGE, the
    /// frames before `turn/completed` and then 8 MiB with no LF; for one whose marker names UNKNOWN, an effect (a line
    /// in `codex.d/effects`), the frames up to a started command, and its end.
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
        // A turn that starts a command and ends before reporting it: what it did is unknown (M2T07R-200).
        let started = frames
            .iter()
            .position(|f| {
                f["method"] == "item/started" && f["params"]["item"]["type"] == "agentMessage"
            })
            .ok_or("no agentMessage item/started")?;
        let mut unknown = frames[..started].to_vec();
        let mut command = frames[started].clone();
        command["params"]["item"] = json!({"type": "commandExecution", "id": "cmd-1", "command": "touch effect",
            "status": "inProgress"});
        unknown.push(command);
        std::fs::write(d.join("unknown.jsonl"), lines(&unknown))?;
        // A first start cut short once the thread is started, before any turn (M2T17R-300): the agent has given its
        // session id, no input reached it.
        let thread_start = requests
            .iter()
            .find(|(_, method)| method.as_str() == Some("thread/start"))
            .and_then(|(id, _)| id.parse::<i64>().ok())
            .ok_or("no thread/start request")?;
        let answered = frames
            .iter()
            .position(|f| f["id"].as_i64() == Some(thread_start) && f.get("result").is_some())
            .ok_or("no thread/start answer")?;
        std::fs::write(d.join("incomplete.jsonl"), lines(&frames[..=answered]))?;
        // An App Server that does not know initialize: the turn degrades to exec (decision 15).
        std::fs::write(
            d.join("degrade.jsonl"),
            lines(&[json!({"id": 0, "error": {"code": -32601, "message": "Method not found"}})]),
        )?;
        std::fs::write(d.join("requests.json"), Value::Object(requests).to_string())?;
        self.script(
            "codex",
            &format!(
                "f='{PROTOCOL}/codex/0.166.0'; d='{d}'\n\
                 case \"$1\" in\n\
                  --version) echo 'codex-cli 0.166.0'; exit 0;;\n\
                  --help) cat \"$f/help.txt\"; exit 0;;\n\
                  exec) [ \"$2\" = --help ] && {{ cat \"$f/help-exec.txt\"; exit 0; }}\n\
                   [ \"$2\" = resume ] && exit 2\n\
                   echo $$ > \"$d/pid.$$\"; printf '%s\\n' \"$@\" > \"$d/exec-argv.$$\"; cat > /dev/null\n\
                   cat \"$f/exec/stdout.jsonl\"; exit 0;;\n\
                  app-server) case \"$2\" in\n\
                    --help) cat \"$f/help-app-server.txt\"; exit 0;;\n\
                    generate-json-schema) [ -e \"$d/hang-schema\" ] && {{ sleep 120 & echo $! > \"$d/schema-member\"; echo $$ > \"$d/schema-pid\"; wait; }}\n\
                     [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R \"$f/app-server-schema/.\" \"$4/\"; exit 0;;\n\
                   esac\n\
                   echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"\n\
                   case \"$HOH_ATTEMPT_ID\" in\n\
                    *HUGE*) frames=\"$d/huge.jsonl\"; opt=;;\n\
                    *UNKNOWN*) frames=\"$d/unknown.jsonl\"; opt=--exit-when-done; echo \"$HOH_ATTEMPT_ID\" >> \"$d/effects\";;\n\
                    *DEGRADE*) frames=\"$d/degrade.jsonl\"; opt=;;\n\
                    *INCOMPLETE/attempt-1) frames=\"$d/incomplete.jsonl\"; opt=--exit-when-done;;\n\
                    *) frames=\"$d/frames.jsonl\"; opt=;;\n\
                   esac\n\
                   exec timeout 60 python3 '{APP_SERVER}' \"$frames\" \"$d/requests.json\" \"$d/log.$$\" $opt;;\n\
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
        // A first start that ends once get_state answered, before any prompt (M2T17R-300, -350, -450): pi has given
        // its session id and, as pi 1.5.0 persists a session only once it has a message, written no session file.
        let first = std::fs::read_to_string(format!("{case}/stdout.jsonl"))?
            .lines()
            .next()
            .ok_or("no get_state answer")?
            .to_owned();
        std::fs::write(
            d.join("early-exit.jsonl"),
            format!(
                "{}\n{first}\n{}\n",
                json!({"hoh_lazy_session": true}),
                json!({"hoh_exit": 1})
            ),
        )?;
        self.script(
            "pi",
            &format!(
                "f='{PROTOCOL}/pi/1.5.0'; d='{d}'\n\
                 case \"$1\" in --version) echo '1.5.0'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"\n\
                 case \"$HOH_ATTEMPT_ID\" in\n\
                  *HUGE*) printf '%s\\n' '{{\"type\":\"agent_start\"}}'; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
                  *EARLY-EXIT/attempt-1) exec timeout 60 python3 '{PI_RPC}' \"$d/early-exit.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\";;\n\
                 esac\n\
                 exec timeout 60 python3 '{PI_RPC}' \"$d/frames.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\"\n",
                d = d.display()
            ),
        )
    }

    /// Every fake turn process gone: the ones still there are killed (nothing a test started outlives it). A number is
    /// killed only while its command line names this world's fakes (each fake's does, the ones that exec too), so a
    /// number another process took since is never killed (CIR-056's class).
    fn stop_fakes(&self) -> R {
        let bin = self.bin().display().to_string();
        for name in ["claude", "codex", "pi"] {
            let d = self.bin().join(format!("{name}.d"));
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if let Some(pid) = file.strip_prefix("pid.")
                    && std::fs::read(format!("/proc/{pid}/cmdline"))
                        .is_ok_and(|c| String::from_utf8_lossy(&c).contains(&bin))
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

/// The project's current team revision (1 once a project file is loaded, e.g. by [`World::bind_installed`]).
fn team_revision(project: &Path) -> R<i64> {
    Ok(query(
        project,
        "SELECT coalesce(max(revision), 0) FROM team_revisions",
    )?
    .parse()?)
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
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        if agent != "claude" {
            configure(&host, "dev", team_revision(&project)?, json!({"developer": {"agent": agent}})).await?;
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
        // A few ticks with A1 running: nothing changes for it, and the run shows the change pending, its developer still
        // claude at epoch 1 (M2T07R-253).
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
        let (status, snapshot) = exchange(&host, "GET", &format!("/v1/runs/{run_id}"), None).await?;
        assert_eq!(status, 200, "{snapshot}");
        let developer = snapshot["projection"]["team"]
            .as_array()
            .and_then(|team| team.iter().find(|m| m["role"] == "developer"))
            .cloned()
            .ok_or(format!("no developer: {snapshot}"))?;
        assert_eq!(
            (&developer["agent"], &developer["role_epoch"]),
            (&json!("claude"), &json!(1)),
            "{developer}"
        );
        assert_eq!(
            (&developer["pending"]["agent"], &developer["pending"]["model"]),
            (&json!("codex"), &json!("m-test")),
            "{developer}"
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
        // The new binding took effect after A1 ended, not while it ran.
        let seq = |event_id: &str| -> R<i64> {
            Ok(query(&project, &format!("SELECT seq FROM events WHERE event_id = '{event_id}'"))?.parse()?)
        };
        let (a1_ended, applied) = (
            seq("T1/attempt-1/SETTLED")?,
            seq(&format!("{run_id}/team/developer/2"))?,
        );
        assert!(applied > a1_ended, "applied at {applied}, A1 ended at {a1_ended}");
        keep(
            "test-018-02",
            &json!({"team_revision_before": r, "accepted": accepted, "while_a1_ran": developer, "a1": a1,
                    "a1_ended_seq": a1_ended, "binding_applied_seq": applied, "a2": a2}),
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
    // session (sessions.role cto and developer); the developer's agent process is killed while its turn runs (M2T07R-259);
    // the developer's task runs again and a third task, the developer's, runs in the same session as its first,
    // resumed; the team revision and the cto's session are unchanged.
    let world = World::new()?;
    let hold = world.dir.path().join("release");
    world.claude(Some(&hold))?;
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let revision = query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?;
        ready(&project, &run_id, &[("T-cto", "cto"), ("T-dev", "developer")])?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            "SELECT count(*) FROM attempts WHERE attempt_id IN ('T-cto/attempt-1', 'T-dev/attempt-1') \
             AND state = 'RUNNING' AND pid IS NOT NULL",
            "2",
            limit,
        )
        .await?;
        let pid = query(&project, "SELECT pid FROM attempts WHERE attempt_id = 'T-dev/attempt-1'")?;
        // The pid is recorded once the turn started, before the fake's first lines ran: it is killed only once it
        // wrote what it was started with (its env, with the attempt's marker, comes before its stdin).
        let wrote = world.bin().join(format!("claude.d/stdin.{pid}"));
        let end = Instant::now() + limit;
        while !wrote.exists() {
            if Instant::now() > end {
                return Err(format!("the developer's turn never wrote {}", wrote.display()).into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let killed = Command::new("kill").args(["-KILL", &pid]).status()?;
        assert!(killed.success(), "kill {pid}");
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-dev/attempt-1'",
            "FAILED",
            limit,
        )
        .await?;
        std::fs::write(&hold, "")?;
        until(&project, &ended("T-cto/attempt-1"), "SETTLED/", limit).await?;
        until(&project, &ended("T-dev/attempt-2"), "SETTLED/", limit).await?;
        let cto = attempt_row(&project, "T-cto/attempt-1")?;
        let dev = attempt_row(&project, "T-dev/attempt-1")?;
        assert_eq!(attempt_row(&project, "T-dev/attempt-2")?["session_id"], dev["session_id"]);
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
        // The developer's killed agent process is gone.
        assert!(hoh_agents::identity::reaped(pid.parse()?)?, "{pid}");
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
        assert_eq!(markers.len(), 4, "{markers:?}");
        for attempt in [
            "T-cto/attempt-1",
            "T-dev/attempt-1",
            "T-dev/attempt-2",
            "T-dev2/attempt-1",
        ] {
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
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        configure(&host, "dev", team_revision(&project)?, json!({"developer": {"agent": "pi"}})).await?;
        let limit = Duration::from_secs(30);
        until(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'team.binding_refused'",
            "1",
            limit,
        )
        .await?;
        // Refused with why (REQ-019 failure mode 3, M2T07R-260).
        let detail = query(
            &project,
            "SELECT json_extract(payload_json, '$.detail') FROM events WHERE kind = 'team.binding_refused'",
        )?;
        assert!(detail.contains("PATH"), "{detail}");
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
    world.bind_installed()?;
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

#[tokio::test]
async fn once_a_turn_ended_the_executor_no_longer_holds_it_and_a_cancel_reaches_nothing() -> R {
    // M2T07R-553 (docs/adr/ADR-022.md decision 17): a turn's end releases the executor's hold on it, so a later native
    // cancel of its attempt (a settle's, a CancelRun's) answers that it reached nothing.
    let world = World::new()?;
    world.claude(None)?;
    let config = world.config();
    let executor = config
        .runner
        .as_ref()
        .ok_or("the product's host has a runner")?
        .executor
        .clone();
    world.bind_installed()?;
    let host = start_when_free(config).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T-END", "developer")])?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-END/attempt-1'",
            "SETTLED",
            Duration::from_secs(30),
        )
        .await?;
        let work = hoh_app::host::Work {
            project_id: host.project_id.clone(),
            run_id,
            task_id: "T-END".to_owned(),
            attempt_id: "T-END/attempt-1".to_owned(),
            role: "developer".to_owned(),
            effect_key: String::new(),
            assignment: hoh_core::persistence::Assignment::default(),
        };
        let end = Instant::now() + Duration::from_secs(5);
        while executor.cancel(&work) && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        R::Ok(executor.cancel(&work))
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    assert!(!outcome?, "the ended turn is still held");
    Ok(())
}

#[tokio::test]
async fn a_turn_that_ends_after_starting_a_command_ends_unknown_effect_and_is_not_run_again() -> R {
    // M2T07R-200 (ADR-019 decision 5, ADR-022 decision 12): codex starts a command and its turn ends with no report of
    // it: the attempt ends UNKNOWN_EFFECT, its task stays, no attempt runs it again (the effect happened once), and a
    // diagnostic names its effects.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        configure(&host, "dev", 0, json!({"developer": {"agent": "codex"}})).await?;
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T-UNKNOWN", "developer")])?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-UNKNOWN/attempt-1'",
            "UNKNOWN_EFFECT",
            Duration::from_secs(30),
        )
        .await?;
        // Ticks go by: nothing runs it again.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-UNKNOWN'"
            )?,
            "1"
        );
        assert_eq!(
            std::fs::read_to_string(world.bin().join("codex.d/effects"))?
                .lines()
                .count(),
            1,
            "the effect happened once"
        );
        assert_eq!(
            query(
                &project,
                "SELECT state FROM tasks WHERE task_id = 'T-UNKNOWN'"
            )?,
            "RUNNING"
        );
        let diagnostic: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'runner.diagnostic' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-UNKNOWN/attempt-1'",
        )?)?;
        assert_eq!(diagnostic["code"], "UNKNOWN_EFFECT", "{diagnostic}");
        assert!(
            diagnostic["effect_keys"]
                .as_array()
                .is_some_and(|k| !k.is_empty()),
            "{diagnostic}"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// `git` in `dir` (a test's own repository), its stdout.
fn git(dir: &Path, args: &[&str]) -> R<String> {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[tokio::test]
async fn a_writer_the_host_stops_for_an_oversize_frame_lets_its_lease_go_with_the_workspace_recorded()
-> R {
    // M2T17FR-50, -53 (REQ-019 behaviour 6, docs/adr/ADR-024.md decision 9 after round 1): HoH itself stops a writer's
    // claude turn that sends a frame over the limit; the attempt ends FAILED (PROTOCOL_FRAME_TOO_LARGE), by the
    // runner's own end, with the workspace's record in the same transaction: the record holds the project's git work
    // tree with its uncommitted change (its diff hash); the next writer is granted only after it. That the record is
    // taken after the stop is the settle's host test's (its agent's SIGTERM write is in the record, M2T17FR-153).
    let world = World::new()?;
    world.claude(None)?;
    let project = world.project();
    git(&project, &["init", "-q"])?;
    std::fs::write(project.join(".gitignore"), ".hoh/\n")?;
    std::fs::write(project.join("a.txt"), "one\n")?;
    git(&project, &["add", ".gitignore", "a.txt"])?;
    git(&project, &["commit", "-q", "-m", "first"])?;
    std::fs::write(project.join("a.txt"), "two\n")?;
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[])?;
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T-HUGE', '{run_id}', 's1', 'READY', 1, 'developer', 1, 1, 1);"
            ),
        )?;
        until(
            &project,
            "SELECT count(*) FROM attempts WHERE attempt_id = 'T-HUGE/attempt-1'",
            "1",
            Duration::from_secs(30),
        )
        .await?;
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T-NEXT', '{run_id}', 's1', 'READY', 1, 'developer', 1, 2, 1);"
            ),
        )?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &ended("T-HUGE/attempt-1"),
            "FAILED/PROTOCOL_FRAME_TOO_LARGE",
            limit,
        )
        .await?;
        until(
            &project,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             AND json_extract(payload_json, '$.task_id') = 'T-NEXT'",
            "1",
            limit,
        )
        .await?;
        let record: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'attempt.workspace' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-HUGE/attempt-1'",
        )?)?;
        assert_eq!(record["to"], "FAILED", "{record}");
        let workspace = &record["workspace"];
        assert_eq!(
            workspace["head"],
            json!(git(&project, &["rev-parse", "HEAD"])?.trim()),
            "{record}"
        );
        let diff = std::process::Command::new("git")
            .args(["diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD"])
            .current_dir(&project)
            .output()?
            .stdout;
        let digest = <sha2::Sha256 as sha2::Digest>::digest(&diff);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(workspace["diff_sha256"], json!(hex), "{record}");
        let at = |sql: &str| -> R<i64> { Ok(query(&project, sql)?.parse()?) };
        let recorded = at("SELECT seq FROM events WHERE kind = 'attempt.workspace' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-HUGE/attempt-1'")?;
        let ended_at = at("SELECT seq FROM events WHERE kind = 'attempt.moved' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-HUGE/attempt-1' \
             AND json_extract(payload_json, '$.to') = 'FAILED'")?;
        let granted = at("SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             AND json_extract(payload_json, '$.task_id') = 'T-NEXT'")?;
        assert_eq!(recorded + 1, ended_at, "the record is in its end's transaction");
        assert!(recorded < granted, "the next writer was granted before the record");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_writers_turn_that_ends_unknown_effect_lets_its_lease_go_with_the_workspace_recorded() -> R
{
    // REQ-019 behaviour 6 (docs/adr/ADR-024.md decision 9, M2T17R-52), the runner's own end: a writer's codex turn
    // starts a command and ends with no report of it. Its end is UNKNOWN_EFFECT, taken by the runner (no reconcile),
    // with the workspace's record in the same transaction; the next writer is granted only after it. The project is
    // no git work tree: the record says so and counts (reading A5).
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        configure(&host, "dev", 0, json!({"developer": {"agent": "codex"}})).await?;
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[])?;
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T-UNKNOWN', '{run_id}', 's1', 'READY', 1, 'developer', 1, 1, 1);"
            ),
        )?;
        // Another writer task, once the first one holds the writer slot: it waits for it.
        until(
            &project,
            "SELECT count(*) FROM attempts WHERE attempt_id = 'T-UNKNOWN/attempt-1'",
            "1",
            Duration::from_secs(30),
        )
        .await?;
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T-NEXT', '{run_id}', 's1', 'READY', 1, 'developer', 1, 2, 1);"
            ),
        )?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-UNKNOWN/attempt-1'",
            "UNKNOWN_EFFECT",
            Duration::from_secs(30),
        )
        .await?;
        until(
            &project,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             AND json_extract(payload_json, '$.task_id') = 'T-NEXT'",
            "1",
            Duration::from_secs(30),
        )
        .await?;
        let record: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'attempt.workspace' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-UNKNOWN/attempt-1'",
        )?)?;
        assert_eq!(record["to"], "UNKNOWN_EFFECT", "{record}");
        assert!(record["workspace"]["error"].is_string(), "{record}");
        let at = |sql: &str| -> R<i64> { Ok(query(&project, sql)?.parse()?) };
        let recorded = at("SELECT seq FROM events WHERE kind = 'attempt.workspace' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-UNKNOWN/attempt-1'")?;
        let ended = at("SELECT seq FROM events WHERE kind = 'attempt.moved' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-UNKNOWN/attempt-1' \
             AND json_extract(payload_json, '$.to') = 'UNKNOWN_EFFECT'")?;
        let granted = at("SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             AND json_extract(payload_json, '$.task_id') = 'T-NEXT'")?;
        assert_eq!(recorded + 1, ended, "the record is in its end's transaction");
        assert!(recorded < granted, "the next writer was granted before the record");
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM events WHERE kind = 'reconcile.attempt' \
                 AND json_extract(payload_json, '$.attempt_id') = 'T-UNKNOWN/attempt-1'"
            )?,
            "0",
            "ended by a reconcile, not by the runner"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn an_app_server_that_cannot_start_a_turn_degrades_that_codex_to_exec() -> R {
    // M2T07R-201 (REQ-024 failure mode 4, ADR-022 decision 15): the App Server answers initialize with "method not
    // found": the attempt fails CAPABILITY_UNSUPPORTED with one adapter.degraded event and its facts recorded; the
    // task's next attempt runs on exec and settles.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        configure(
            &host,
            "dev",
            team_revision(&project)?,
            json!({"developer": {"agent": "codex"}}),
        )
        .await?;
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T-DEGRADE", "developer")])?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &ended("T-DEGRADE/attempt-1"),
            "FAILED/CAPABILITY_UNSUPPORTED",
            limit,
        )
        .await?;
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM events WHERE kind = 'adapter.degraded' \
                 AND event_id = 'T-DEGRADE/attempt-1/degraded'"
            )?,
            "1"
        );
        assert!(
            !query(
                &project,
                "SELECT raw_log_path FROM attempts WHERE attempt_id = 'T-DEGRADE/attempt-1'"
            )?
            .is_empty(),
            "the failed attempt's facts were recorded"
        );
        until(&project, &ended("T-DEGRADE/attempt-2"), "SETTLED/", limit).await?;
        let exec_turns = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("exec-argv."))
            .count();
        assert_eq!(exec_turns, 1, "the second attempt ran on exec");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_task_dispatched_before_the_first_look_at_its_agent_waits_for_it() -> R {
    // M2T07R-250 (ADR-022 decisions 10 and 12): after a restart a binding already has its session, so its task is
    // dispatched before the new host's registry has looked at its agent (a slow probe here): the executor takes the work
    // once the agent was looked at, and the attempt settles; none fails for an agent not looked at yet.
    let world = World::new()?;
    world.claude(None)?;
    let project = world.project();
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let run_id = match async {
        let run_id = started(&host, "s1").await?;
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role = 'developer' \
                 AND session_id IS NOT NULL"
            ),
            "1",
            Duration::from_secs(30),
        )
        .await?;
        R::Ok(run_id)
    }
    .await
    {
        Ok(run_id) => run_id,
        Err(e) => {
            host.shutdown().await;
            return Err(e);
        }
    };
    host.shutdown().await;
    std::fs::write(world.bin().join("claude.d/slow"), "")?;
    // A task waits for the next host, whose first look at claude takes 3 s.
    ready(&project, &run_id, &[("T-AFTER", "developer")])?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        until(
            &project,
            &ended("T-AFTER/attempt-1"),
            "SETTLED/",
            Duration::from_secs(60),
        )
        .await?;
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-AFTER'"
            )?,
            "1"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_hosts_stop_ends_the_probe_it_has_running() -> R {
    // M2T07R-202 (ADR-022 decision 10): a probe command that hangs is ended when the host stops (its group killed, its
    // leader reaped), and the stop does not wait for its own timeout: nothing the registry started outlives the host.
    let world = World::new()?;
    world.claude(None)?;
    std::fs::write(world.bin().join("claude.d/hang"), "")?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let pid_file = world.bin().join("claude.d/probe-pid");
    let outcome = async {
        started(&host, "s1").await?;
        let end = Instant::now() + Duration::from_secs(30);
        // Its number and start time while it hangs, so that the end's look and kill are of this process alone and
        // never of one that took its number since (CIR-056; convention v2).
        let pid: u32 = loop {
            if let Ok(pid) = std::fs::read_to_string(&pid_file)
                .unwrap_or_default()
                .trim()
                .parse()
            {
                break pid;
            }
            if Instant::now() > end {
                return Err("the probe never ran".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        let start = hoh_agents::identity::start_id(pid)?
            .ok_or_else(|| format!("the probe's command {pid} ended before the stop"))?;
        R::Ok((pid, start))
    }
    .await;
    let stopping = Instant::now();
    host.shutdown().await;
    let took = stopping.elapsed();
    let (pid, start) = outcome?;
    // Ended by identity::ended: a look that cannot tell fails the test (convention v2; CIR-051).
    let alive = !hoh_agents::identity::ended(pid, Some(&start))?;
    world.stop_fakes()?;
    if alive {
        let _ = hoh_agents::identity::signal_if_same(pid, &start, rustix::process::Signal::KILL);
    }
    assert!(!alive, "the probe's command {pid} outlived the host");
    assert!(took < Duration::from_secs(10), "the stop took {took:?}");
    let _ = project;
    Ok(())
}

#[test]
fn the_configured_cancel_graces_reach_every_step_of_the_products_host() {
    // M2T07R-350 (docs/adr/ADR-022.md decision 17): the product's host takes execution's cancel graces for CancelRun's
    // ladder and for a lease-expiry settle, not only for the adapters' own escalation.
    let mut effective = hoh_core::config::Config::default();
    effective.execution.cancel_native_grace_seconds = 3;
    effective.execution.cancel_term_grace_seconds = 4;
    let config = HostConfig::product(Path::new("/p"), effective, "hoh".into(), "".into());
    let (native, term) = (Duration::from_secs(3), Duration::from_secs(4));
    assert_eq!(
        (config.reconcile.native_grace, config.reconcile.term_grace),
        (native, term)
    );
    assert_eq!((config.cancel.native, config.cancel.term), (native, term));
}

/// The id of `pid`'s thread named `name`, if it has one.
fn thread_named(pid: u32, name: &str) -> Option<i32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .ok()?
        .filter_map(Result::ok)
        .find(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|comm| comm.trim_end() == name)
        })
        .and_then(|task| task.file_name().to_str()?.parse().ok())
}

#[test]
fn a_hosts_stop_kills_the_schema_command_however_late_its_reader_runs() -> R {
    // M2T07R-300 (docs/adr/ADR-022.md decision 10 v4): the product's `hoh __host` stops (SIGTERM) while its registry
    // probes Codex, the schema command running and the thread that reads it held by ptrace, so that it cannot run
    // before the host exits: the stop itself killed the command and its group, so nothing the host started outlives it.
    use nix::sys::{ptrace, signal, wait};
    use nix::unistd::Pid;
    let world = World::new()?;
    world.codex("m-test")?;
    let d = world.bin().join("codex.d");
    std::fs::write(d.join("hang-schema"), "")?;
    let (home, tmp) = (world.dir.path().join("home"), world.dir.path().join("tmp"));
    std::fs::create_dir_all(&home)?;
    std::fs::create_dir_all(&tmp)?;
    let path = format!(
        "{}:{}",
        world.bin().display(),
        world.dir.path().join("tools").display()
    );
    let mut host = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .arg("__host")
        .arg(world.project())
        .env_clear()
        .env("PATH", &path)
        .env("HOME", &home)
        .env("TMPDIR", &tmp)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let host_pid = host.id();
    let pids = [d.join("schema-pid"), d.join("schema-member")];
    let mut started: Vec<(u32, String)> = Vec::new();
    let mut traced = None;
    let outcome = (|| -> R<(i32, std::time::Duration)> {
        let info = world.project().join(".hoh/host.json");
        let end = Instant::now() + Duration::from_secs(30);
        let info: Value = loop {
            if let Ok(text) = std::fs::read_to_string(&info)
                && let Ok(info) = serde_json::from_str::<Value>(&text)
            {
                break info;
            }
            if Instant::now() > end {
                return Err("the host never announced itself".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let port = info["port"].as_u64().ok_or("no port")?;
        let token = info["token"].as_str().ok_or("no token")?;
        let body = json!({
            "schema_version": "1.0", "command_id": "c-s1", "idempotency_key": "s1",
            "expected_revision": 0, "kind": "StartRun", "run_id": null,
            "payload": {"goal": "Ship the parser", "mode": "AUTO", "interactive": false},
        })
        .to_string();
        let mut stream = std::net::TcpStream::connect(("127.0.0.1", u16::try_from(port)?))?;
        use std::io::{Read as _, Write as _};
        write!(
            stream,
            "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n\
             Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )?;
        let mut answer = String::new();
        stream.read_to_string(&mut answer)?;
        if !answer.starts_with("HTTP/1.1 202") {
            return Err(format!("StartRun: {answer}").into());
        }
        while !pids.iter().all(|p| p.exists()) {
            if Instant::now() > end {
                return Err("the schema was never generated".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // The schema commands live now (each hangs 120 s): their numbers and start times name them for the end's
        // check, so a number taken again is never taken for them (ADR-022 decision 10, convention v2; M2T07R-1151).
        for p in &pids {
            let pid: u32 = loop {
                if let Ok(pid) = std::fs::read_to_string(p)?.trim().parse() {
                    break pid;
                }
                if Instant::now() > end {
                    return Err(format!("{} never named a process", p.display()).into());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            // One gone already (it should hang 120 s) fails the test: without its start time a number taken again
            // would be read as it and killed (CIR-056).
            let start = hoh_agents::identity::start_id(pid)?
                .ok_or_else(|| format!("schema command {pid} ended before the stop"))?;
            started.push((pid, start));
        }
        let reader = thread_named(host_pid, "hoh-schema").ok_or("no schema reader thread")?;
        let tid = Pid::from_raw(reader);
        ptrace::seize(tid, ptrace::Options::empty())?;
        traced = Some(tid);
        ptrace::interrupt(tid)?;
        wait::waitpid(tid, Some(wait::WaitPidFlag::__WALL))?;
        let stopping = Instant::now();
        signal::kill(
            Pid::from_raw(i32::try_from(host_pid)?),
            signal::Signal::SIGTERM,
        )?;
        // The held thread dies with its process; its tracer (this test) reaps it, then the host itself is reaped.
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match wait::waitpid(
                tid,
                Some(wait::WaitPidFlag::__WALL | wait::WaitPidFlag::WNOHANG),
            ) {
                Ok(wait::WaitStatus::Exited(..) | wait::WaitStatus::Signaled(..)) | Err(_) => break,
                Ok(_) => {}
            }
            if Instant::now() > end {
                return Err("the host did not exit".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        traced = None;
        let status = loop {
            if let Some(status) = host.try_wait()? {
                break status;
            }
            if Instant::now() > end {
                return Err("the host was not reaped".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        Ok((status.code().unwrap_or(-1), stopping.elapsed()))
    })();
    if let Some(tid) = traced {
        let _ = ptrace::detach(tid, None);
    }
    if !matches!(host.try_wait(), Ok(Some(_))) {
        let _ = host.kill();
        let _ = host.wait();
    }
    // Each command's end by identity::ended with its start time: a look that cannot tell is said, never taken for an
    // end, and only a process seen to be the same one is killed.
    let look = |started: &[(u32, String)]| {
        let (mut alive, mut untold) = (Vec::new(), Vec::new());
        for (pid, start) in started {
            match hoh_agents::identity::ended(*pid, Some(start)) {
                Ok(true) => {}
                Ok(false) => alive.push(*pid),
                Err(e) => untold.push(format!("{pid}: {e}")),
            }
        }
        (alive, untold)
    };
    let until = Instant::now() + Duration::from_secs(2);
    let (mut alive, mut untold) = look(&started);
    while !(alive.is_empty() && untold.is_empty()) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
        (alive, untold) = look(&started);
    }
    // Only a process still of its start time is killed (CIR-103, -152).
    for (pid, start) in started.iter().filter(|(pid, _)| alive.contains(pid)) {
        let _ = hoh_agents::identity::signal_if_same(*pid, start, rustix::process::Signal::KILL);
    }
    let (code, took) = outcome?;
    assert_eq!(started.len(), 2, "{started:?}");
    assert!(
        untold.is_empty(),
        "whether the schema commands ended cannot be told: {untold:?}"
    );
    assert!(
        alive.is_empty(),
        "the schema command outlived the host: {alive:?}"
    );
    assert_eq!(code, 0, "the host's exit");
    assert!(took < Duration::from_secs(15), "the stop took {took:?}");
    Ok(())
}

#[tokio::test]
async fn a_cancelled_run_stops_its_agent_by_its_native_cancel_then_sigterm_after_the_configured_grace()
-> R {
    // M2T07R-254, -258 (REQ-043, ADR-022 decisions 12, 14 and 17): CancelRun reaches the product executor's turn: its
    // native cancel first (SIGINT for claude), then, after execution.cancel_native_grace_seconds (1 s here, not the
    // default 10 s), SIGTERM; the run ends CANCELLED.
    let world = World::new()?;
    world.claude(None)?;
    let mut effective = hoh_core::config::Config::default();
    effective.execution.cancel_native_grace_seconds = 1;
    effective.execution.cancel_term_grace_seconds = 1;
    world.bind_installed()?;
    let host = start_when_free(world.config_of(effective)).await?;
    let project = world.project();
    let signals = world.bin().join("claude.d/signals");
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[("T-CANCEL", "developer")])?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-CANCEL/attempt-1'",
            "RUNNING",
            Duration::from_secs(30),
        )
        .await?;
        // The cancel only once the stand-in says it takes signals (its own event; convention v2).
        let ready = world.bin().join("claude.d/cancel-ready");
        let end = Instant::now() + Duration::from_secs(30);
        while !ready.exists() {
            if Instant::now() > end {
                return Err("the agent never said it takes signals".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let revision = query(
            &project,
            &format!("SELECT revision FROM runs WHERE run_id = '{run_id}'"),
        )?;
        let body = json!({
            "schema_version": "1.0", "command_id": "c-cancel", "idempotency_key": "cancel",
            "expected_revision": revision.parse::<i64>()?, "kind": "CancelRun", "run_id": run_id,
            "payload": {"reason": "stop"},
        });
        let (status, accepted) = exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
        assert_eq!(status, 202, "{accepted}");
        until(
            &project,
            &format!("SELECT state FROM runs WHERE run_id = '{run_id}'"),
            "CANCELLED",
            Duration::from_secs(30),
        )
        .await?;
        let seen = std::fs::read_to_string(&signals)?;
        let steps: Vec<(&str, u128)> = seen
            .lines()
            .filter_map(|l| l.split_once(' '))
            .filter_map(|(name, at)| at.parse().ok().map(|at| (name, at)))
            .collect();
        assert_eq!(
            steps.iter().map(|s| s.0).collect::<Vec<_>>(),
            ["INT", "TERM"],
            "{seen}"
        );
        let gap = Duration::from_nanos(u64::try_from(steps[1].1 - steps[0].1)?);
        assert!(
            gap >= Duration::from_millis(900) && gap < Duration::from_secs(8),
            "SIGTERM {gap:?} after the native cancel"
        );
        keep(
            "cancel-native-then-term",
            &json!({"signals": seen, "gap_ms": gap.as_millis()}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_019_02_the_new_commanders_first_input_is_the_checkpoint_package() -> R {
    // AC-019-02 (docs/adr/ADR-024.md decisions 1, 2 and 4): run R has goal G, spec revision 3, two APPLIED decisions,
    // an OPEN and a REOPENED issue, one QUEUED input and candidate C, and its old Commander's raw log holds a unique
    // sentinel, as do an event and the payloads of an APPLIED decision, the QUEUED input and the unacked message the
    // package names by id (M2T17R-58: their contents, as the log's, must not reach it). The commander changes from codex to pi; once applied, the first input the product keeps for the new
    // Commander's session is a ContextHandoff valid against protocol.schema.json with goal G, revision 3's digest and
    // candidate C, whose id lists are the database's; the sentinel appears in it 0 times.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        let commander = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'commander'"
        );
        let old_session = format!("{run_id}/session/commander/1");
        until(&project, &commander, &format!("codex/1/{old_session}"), limit).await?;
        // No runtime notice left for the runner (run.started taken), so the outbox holds only what the fixture adds.
        until(
            &project,
            "SELECT count(*) FROM outbox WHERE state <> 'ACKED' AND json_extract(payload_json, '$.notice') = 'runtime'",
            "0",
            limit,
        )
        .await?;
        let sentinel = "SENTINEL-019-02-7f3a9c";
        let raw = project.join(".hoh/sessions/old-commander-raw.jsonl");
        std::fs::create_dir_all(raw.parent().ok_or("no dir")?)?;
        std::fs::write(&raw, format!("{{\"type\":\"message\",\"text\":\"{sentinel}\"}}\n"))?;
        exec(
            &project,
            &format!(
                "INSERT INTO specs(spec_id, project_id, sha256, revision) \
                   SELECT 's3', project_id, printf('%064d', 3), 3 FROM runs WHERE run_id = '{run_id}';
                 UPDATE runs SET spec_id = 's3' WHERE run_id = '{run_id}';
                 UPDATE sessions SET native_path = '{raw}' WHERE session_id = '{old_session}';
                 INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, payload_json, state) VALUES
                   ('D1', '{run_id}', 1, 1, '{{\"note\": \"{sentinel}\"}}', 'APPLIED'),
                   ('D2', '{run_id}', 1, 1, '{{}}', 'APPLIED'), ('D3', '{run_id}', 1, 1, '{{}}', 'REJECTED');
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'e-sentinel', '{run_id}', 'commander.said', '{{\"text\": \"{sentinel}\"}}',
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';
                 INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
                   test_plan_sha256, source_path, state, created_at) VALUES
                   ('C', '{run_id}', printf('%040d', 1), printf('%040d', 2), printf('%064d', 3), printf('%064d', 4),
                    printf('%064d', 5), '/repo', 'FROZEN', '2026-11-10T00:00:00Z');
                 INSERT INTO issues(issue_id, run_id, fingerprint, severity, state, description) VALUES
                   ('I1', '{run_id}', 'f1', 'high', 'OPEN', 'd'), ('I2', '{run_id}', 'f2', 'low', 'REOPENED', 'd'),
                   ('I3', '{run_id}', 'f3', 'low', 'RESOLVED', 'd');
                 INSERT INTO input_queue(input_id, run_id, kind, target_role, state, payload_json) VALUES
                   ('Q1', '{run_id}', 'STEER', 'developer', 'QUEUED', '{{\"text\": \"{sentinel}\"}}'),
                   ('Q2', '{run_id}', 'ASK', NULL, 'DELIVERED', '{{}}');
                 INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, payload_json, state) VALUES
                   ('M1', (SELECT min(seq) FROM events), '{run_id}', 'developer', '{{\"text\": \"{sentinel}\"}}',
                    'PENDING'),
                   ('M2', (SELECT min(seq) FROM events), '{run_id}', 'qa', '{{}}', 'ACKED');",
                raw = raw.display()
            ),
        )?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-pi", revision, json!({"commander": {"agent": "pi"}})).await?;
        let new_session = format!("{run_id}/session/commander/2");
        until(&project, &commander, &format!("pi/2/{new_session}"), limit).await?;
        let kept = hoh_app::host::product::first_input_path(&project, &new_session);
        let end = Instant::now() + limit;
        while !kept.exists() {
            assert!(Instant::now() < end, "no first input at {}", kept.display());
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let text = std::fs::read_to_string(&kept)?;
        assert_eq!(text.matches(sentinel).count(), 0, "the old session's text: {text}");
        let input: Value = serde_json::from_str(&text)?;
        let package: hoh_core::protocol::ContextHandoff =
            hoh_core::protocol::decode(&input["package"])?;
        assert_eq!(package.goal, "Ship the parser");
        assert_eq!(
            (package.spec_revision, package.spec_sha256.as_deref()),
            (Some(3), Some(format!("{:064}", 3).as_str()))
        );
        assert_eq!(package.candidate.map(|c| c.id).as_deref(), Some("C"));
        assert_eq!((package.role.as_str(), package.role_epoch), ("commander", 2));
        let ids = |sql: String| -> R<Vec<String>> {
            let joined = query(&project, &format!("SELECT coalesce(group_concat(id, ','), '') FROM ({sql} ORDER BY 1)"))?;
            Ok(joined.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect())
        };
        let state = package.state.ok_or("no state")?;
        assert_eq!(
            state.applied_decision_ids,
            ids(format!("SELECT decision_id AS id FROM decisions WHERE run_id = '{run_id}' AND state = 'APPLIED'"))?
        );
        assert_eq!(
            state.open_issue_ids,
            ids(format!(
                "SELECT issue_id AS id FROM issues WHERE run_id = '{run_id}' AND state IN ('OPEN', 'REOPENED')"
            ))?
        );
        assert_eq!(
            state.queued_input_ids,
            ids(format!("SELECT input_id AS id FROM input_queue WHERE run_id = '{run_id}' AND state = 'QUEUED'"))?
        );
        assert_eq!(
            state.unacked_message_ids,
            ids(format!(
                "SELECT message_id AS id FROM outbox WHERE run_id = '{run_id}' AND state <> 'ACKED' \
                 AND message_id <> '{}'",
                input["message_id"].as_str().ok_or("no message id")?
            ))?
        );
        assert_eq!(
            (
                state.applied_decision_ids.len(),
                state.open_issue_ids.len(),
                state.queued_input_ids.len(),
                state.unacked_message_ids
            ),
            (2, 2, 1, vec!["M1".to_owned()]),
            "the fixture's own"
        );
        // The notice settled once the product kept it.
        until(
            &project,
            &format!(
                "SELECT o.state || '/' || (SELECT count(*) FROM events WHERE kind = 'handoff.delivered') FROM outbox o \
                 WHERE o.message_id = '{}'",
                input["message_id"].as_str().ok_or("no message id")?
            ),
            "ACKED/1",
            limit,
        )
        .await?;
        // What the new Commander is actually sent (docs/adr/ADR-025.md decision 10, M2T17R-54): woken by a
        // need_decision, its first turn begins with the role's contract and then the kept package, whole; the sentinel
        // reaches it 0 times.
        exec(
            &project,
            &format!(
                "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
                   SELECT project_id, 'nd-019-02', '{run_id}', 'need_decision', '{{\"fingerprint\": \"fp-019-02\"}}', \
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';"
            ),
        )?;
        until(
            &project,
            "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'commander' \
             AND a.state = 'SETTLED'",
            "1",
            limit,
        )
        .await?;
        let mut prompts = Vec::new();
        for entry in std::fs::read_dir(world.bin().join("pi.d"))? {
            let received = entry?.path().join("received.jsonl");
            if let Ok(text) = std::fs::read_to_string(&received) {
                for line in text.lines() {
                    let command: Value = serde_json::from_str(line).unwrap_or(Value::Null);
                    if command["type"] == "prompt" {
                        prompts.push(command["message"].as_str().unwrap_or_default().to_owned());
                    }
                }
            }
        }
        let [sent] = prompts.as_slice() else {
            return Err(format!("the pi Commander was sent {} prompts", prompts.len()).into());
        };
        let (contract, handover, wake) = (
            sent.find("Commander").ok_or("no role contract")?,
            sent.find("# 交接包").ok_or("no handover package")?,
            sent.find("# 本次唤醒").ok_or("no wake")?,
        );
        assert!(contract < handover && handover < wake, "{sent}");
        assert!(sent.contains(&format!("{:064}", 3)), "the package names revision 3's digest: {sent}");
        assert_eq!(sent.matches(sentinel).count(), 0, "{sent}");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_new_developers_first_turn_starts_from_its_handover_package() -> R {
    // REQ-019 b2, b4 (docs/adr/ADR-024.md decisions 1 and 4, M2T17R-03): the developer changes from claude to codex in
    // a run on a spec; its handover writes a session.start notice whose package the product keeps for the new session,
    // and the new developer's first turn is sent its role's contract, then that package whole, then the task's
    // header; the attempt's prompt sha256 is what codex was sent.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-codex", revision, json!({"developer": {"agent": "codex"}})).await?;
        let session = format!("{run_id}/session/developer/2");
        until(&project, &developer, &format!("codex/2/{session}"), limit).await?;
        until(
            &project,
            &format!(
                "SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"
            ),
            "ACKED",
            limit,
        )
        .await?;
        let kept: Value = serde_json::from_str(&std::fs::read_to_string(
            hoh_app::host::product::first_input_path(&project, &session),
        )?)?;
        let package = &kept["package"];
        assert_eq!(
            (&package["role"], &package["role_epoch"], &package["output_contract"]),
            (&json!("developer"), &json!(2), &json!("TaskResult"))
        );
        ready(&project, &run_id, &[("T-dev", "developer")])?;
        until(&project, &ended("T-dev/attempt-1"), "SETTLED/", limit).await?;
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
        let text = &sent[0];
        let block = text
            .split("```json\n")
            .nth(1)
            .and_then(|rest| rest.split("\n```").next())
            .ok_or(format!("no package in the first turn: {text}"))?;
        assert_eq!(&serde_json::from_str::<Value>(block)?, package);
        let (package_at, task_at) = (
            text.find("```json").ok_or("no package")?,
            text.find("T-dev").ok_or("no task")?,
        );
        assert!(package_at > 0 && package_at < task_at, "{text}");
        let sha = |bytes: &[u8]| -> String {
            <sha2::Sha256 as sha2::Digest>::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(
            query(
                &project,
                "SELECT prompt_sha256 FROM attempts WHERE attempt_id = 'T-dev/attempt-1'"
            )?,
            sha(text.as_bytes())
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_new_holders_first_turn_without_its_kept_input_fails_first_input_unavailable() -> R {
    // docs/adr/ADR-024.md decision 4 (M2T17R-151, -152, -251): the developer's session got its first input, then the
    // kept file is unreadable, then gone: its first turn is not started without its package (FIRST_INPUT_UNAVAILABLE,
    // nothing sent); once the input is back, the retry's turn starts from it.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-codex", revision, json!({"developer": {"agent": "codex"}})).await?;
        let session = format!("{run_id}/session/developer/2");
        until(
            &project,
            &format!("SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"),
            "ACKED",
            limit,
        )
        .await?;
        let kept = hoh_app::host::product::first_input_path(&project, &session);
        let whole = std::fs::read(&kept)?;
        // Unreadable, then gone: each attempt of the first turn fails FIRST_INPUT_UNAVAILABLE and sends nothing; the
        // input back, the retry starts from it (M2T17R-251; the first input owed until a turn of the session completed, -200, -300).
        std::fs::write(&kept, b"{")?;
        ready(&project, &run_id, &[("T-dev", "developer")])?;
        until(&project, &ended("T-dev/attempt-1"), "FAILED/FIRST_INPUT_UNAVAILABLE", limit).await?;
        std::fs::remove_file(&kept)?;
        until(&project, &ended("T-dev/attempt-2"), "FAILED/FIRST_INPUT_UNAVAILABLE", limit).await?;
        std::fs::write(&kept, &whole)?;
        until(&project, &ended("T-dev/attempt-3"), "SETTLED/", limit).await?;
        let sent: Vec<String> = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|m| m["method"] == "turn/start")
            .filter_map(|m| m["params"]["input"][0]["text"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(sent.len(), 1, "only the third attempt's turn was sent: {sent:?}");
        assert!(sent[0].contains("```json"), "the retry's turn has no package: {}", sent[0]);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_new_holders_later_turns_in_its_session_carry_no_package() -> R {
    // docs/adr/ADR-024.md decision 4 (M2T17R-151, -152): the developer changes from claude to pi; the first turn of
    // its new session carries the package, the session's next turn, once the first completed, does not (the store
    // says the first input is given; a session that cannot resume is covered by
    // a_first_input_stays_owed_until_a_turn_of_its_session_completed).
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-pi", revision, json!({"developer": {"agent": "pi"}})).await?;
        until(
            &project,
            &format!("SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"),
            "ACKED",
            limit,
        )
        .await?;
        for task in ["T-dev", "T-dev2"] {
            ready(&project, &run_id, &[(task, "developer")])?;
            until(&project, &ended(&format!("{task}/attempt-1")), "SETTLED/", limit).await?;
        }
        let prompts: Vec<String> = std::fs::read_dir(world.bin().join("pi.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|c| c["type"] == "prompt")
            .filter_map(|c| c["message"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(prompts.len(), 2, "{prompts:?}");
        let turn = |task: &str| {
            prompts
                .iter()
                .find(|p| p.contains(&format!("任务：{task}（")))
                .cloned()
                .ok_or(format!("no turn of {task}: {prompts:?}"))
        };
        assert!(turn("T-dev")?.contains("```json"), "the first turn has no package");
        assert!(!turn("T-dev2")?.contains("```json"), "a later turn carries the package");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_first_turn_cut_short_after_the_agent_gave_its_session_id_leaves_the_package_owed() -> R {
    // M2T17R-300, -451 (docs/adr/ADR-024.md decision 4): the new developer's (codex) first attempt is cut short after
    // the App Server answered thread/start with its thread id, before any turn: no input reached the agent, so its
    // retry, the first turn codex gets, starts from the package, on a thread of its own: a thread no turn completed is
    // not resumed (it may never have been persisted).
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-codex", revision, json!({"developer": {"agent": "codex"}})).await?;
        until(
            &project,
            &format!("SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"),
            "ACKED",
            limit,
        )
        .await?;
        ready(&project, &run_id, &[("T-INCOMPLETE", "developer")])?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-INCOMPLETE/attempt-1' AND state IN ('FAILED', 'UNKNOWN_EFFECT')",
            "FAILED",
            limit,
        )
        .await?;
        until(&project, &ended("T-INCOMPLETE/attempt-2"), "SETTLED/", limit).await?;
        let sent: Vec<String> = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|m| m["method"] == "turn/start")
            .filter_map(|m| m["params"]["input"][0]["text"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(sent.len(), 1, "only the retry's turn was sent: {sent:?}");
        assert!(sent[0].contains("```json"), "the first turn codex got has no package: {}", sent[0]);
        let resumes = std::fs::read_dir(world.bin().join("codex.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter(|line| line.contains("\"thread/resume\""))
            .count();
        assert_eq!(resumes, 0, "a thread no turn completed was resumed");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_first_pi_start_that_ends_before_its_prompt_leaves_the_package_owed() -> R {
    // M2T17R-300, -350, -450 (docs/adr/ADR-024.md decision 4): the new developer's (pi) first process answers get_state,
    // which gives its session id, and ends before any prompt, writing no session file (as pi 1.5.0); no input reached
    // pi, so its retry, not resuming the session no turn completed (which would be refused SESSION_NOT_FOUND for good),
    // starts pi afresh with the session's id, and its prompt, the first pi takes, starts from the package.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        let revision: i64 =
            query(&project, "SELECT coalesce(max(revision), 0) FROM team_revisions")?.parse()?;
        configure(&host, "to-pi", revision, json!({"developer": {"agent": "pi"}})).await?;
        until(
            &project,
            &format!("SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"),
            "ACKED",
            limit,
        )
        .await?;
        ready(&project, &run_id, &[("T-EARLY-EXIT", "developer")])?;
        until(
            &project,
            "SELECT state FROM attempts WHERE attempt_id = 'T-EARLY-EXIT/attempt-1' AND state IN ('FAILED', 'UNKNOWN_EFFECT')",
            "FAILED",
            limit,
        )
        .await?;
        until(&project, &ended("T-EARLY-EXIT/attempt-2"), "SETTLED/", limit).await?;
        let prompts: Vec<String> = std::fs::read_dir(world.bin().join("pi.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
            .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
            .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .filter(|c| c["type"] == "prompt")
            .filter_map(|c| c["message"].as_str().map(str::to_owned))
            .collect();
        assert_eq!(prompts.len(), 1, "only the retry's prompt was taken: {prompts:?}");
        assert!(prompts[0].contains("```json"), "the first prompt pi took has no package: {}", prompts[0]);
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// A Commander turn of `agent`'s, woken by a need_decision in a world with only `agent` installed: the directory its
/// process ran in is its session's own, under the world's Commander directory and outside the project
/// (docs/adr/ADR-025.md decision 3, -551, -652, -1057).
async fn a_commander_turn_runs_in_its_sessions_own_directory(agent: &str) -> R {
    let world = World::new()?;
    match agent {
        "claude" => world.claude(None)?,
        "codex" => world.codex("gpt-6-astra")?,
        _ => world.pi()?,
    }
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role = 'commander' \
                 AND session_id IS NOT NULL"
            ),
            "1",
            limit,
        )
        .await?;
        ready(&project, &run_id, &[])?;
        exec(
            &project,
            &format!(
                "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
                   SELECT project_id, 'nd-cwd', '{run_id}', 'need_decision', '{{\"fingerprint\": \"fp-cwd\"}}', \
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';"
            ),
        )?;
        until(
            &project,
            "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'commander' \
             AND a.state IN ('SETTLED', 'FAILED')",
            "1",
            limit,
        )
        .await?;
        let workdir = query(
            &project,
            "SELECT json_extract(payload_json, '$.path') FROM events WHERE kind = 'commander.workdir'",
        )?;
        let project_digest = query(&project, "SELECT project_id FROM projects")?;
        assert!(
            workdir.starts_with(&world.commander_dir().display().to_string()),
            "{workdir}"
        );
        assert!(!workdir.contains(&project_digest), "the project's id is digested: {workdir}");
        let canonical = std::fs::canonicalize(&workdir)?;
        assert!(!canonical.starts_with(std::fs::canonicalize(&project)?));
        let mut ran_in = Vec::new();
        for entry in std::fs::read_dir(world.bin().join(format!("{agent}.d")))? {
            let path = entry?.path();
            if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("cwd."))
            {
                ran_in.push(std::fs::read_to_string(&path)?.trim().to_owned());
            }
        }
        assert!(
            ran_in.iter().any(|d| Path::new(d) == canonical),
            "{agent} ran in {ran_in:?}, not {}",
            canonical.display()
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_claude_commander_turn_runs_in_its_sessions_own_directory() -> R {
    a_commander_turn_runs_in_its_sessions_own_directory("claude").await
}

#[tokio::test]
async fn a_codex_commander_turn_runs_in_its_sessions_own_directory() -> R {
    a_commander_turn_runs_in_its_sessions_own_directory("codex").await
}

#[tokio::test]
async fn a_pi_commander_turn_runs_in_its_sessions_own_directory() -> R {
    a_commander_turn_runs_in_its_sessions_own_directory("pi").await
}

/// The texts codex was sent in its turns (turn/start's input), oldest first.
fn codex_prompts(world: &World) -> R<Vec<String>> {
    Ok(std::fs::read_dir(world.bin().join("codex.d"))?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("log."))
        .filter_map(|e| std::fs::read_to_string(e.path().join("received.jsonl")).ok())
        .flat_map(|log| log.lines().map(str::to_owned).collect::<Vec<_>>())
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|m| m["method"] == "turn/start")
        .filter_map(|m| m["params"]["input"][0]["text"].as_str().map(str::to_owned))
        .collect())
}

/// Developer task `task` of run `run_id` READY with two acceptance criteria and `earlier` failed attempts
/// (`<task>/attempt-1` on), each with a failure reason of `reason_bytes` bytes, in one transaction: its first turn is
/// `<task>/attempt-<earlier + 1>`.
fn task_with_acceptance(
    project: &Path,
    run_id: &str,
    task: &str,
    earlier: usize,
    reason_bytes: usize,
) -> R {
    let mut sql = format!(
        "BEGIN;
         INSERT OR IGNORE INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-X', 's1', 'x', 1);
         INSERT OR IGNORE INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-X-01', 'REQ-X', 's1', '装箱后强制项逐字节出现', 'unit'),
           ('AC-X-02', 'REQ-X', 's1', '份额是硬上限 {{{{GOAL}}}}', 'unit');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate)
           VALUES ('{task}', '{run_id}', 's1', 'READY', 1, 'developer', 0,
           (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{task}', 'REQ-X', 's1');"
    );
    for n in 1..=earlier {
        sql.push_str(&format!(
            "INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, failure_reason, \
             ended_at) VALUES ('{task}/attempt-{n}', '{task}', 'FAILED', 1, 1, 1, \
             '{n}:' || replace(hex(zeroblob({reason_bytes})), '00', 'r'), '2026-11-07T00:0{n}:00Z');"
        ));
    }
    sql.push_str("COMMIT;");
    exec(project, &sql)
}

#[tokio::test]
async fn test_039_04_repacking_from_an_attempts_record_gives_the_prompt_it_was_sent() -> R {
    // AC-039-04 (docs/adr/ADR-026.md decision 7): attempt A ran with its prompt recorded (its sha256, the resource
    // bundle version, its inputs list and its truncation policy); repacked from that record and the same bundle, the
    // prompt's sha256 is A's, whatever changed meanwhile in the live state, its contract file or its logs. A's evidence
    // went in as summaries (its earlier attempts' reasons are too long for the evidence share) in ranked order.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh/prompts"))?;
    std::fs::write(
        project.join(".hoh/prompts/dev.md"),
        "你是开发者。\n\n{{GOAL}}\n\n{{TASK_CONTEXT}}\n\n{{PROJECT_CONTEXT}}\n\n{{EVIDENCE_CONTEXT}}\n",
    )?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\nprompt = \".hoh/prompts/dev.md\"\n",
    )?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        ready(&project, &run_id, &[])?;
        task_with_acceptance(&project, &run_id, "T-a", 3, 40_000)?;
        until(&project, &ended("T-a/attempt-4"), "SETTLED/", Duration::from_secs(60)).await?;
        let row: Value = serde_json::from_str(&query(
            &project,
            "SELECT json_object('prompt_sha256', prompt_sha256, 'resource_bundle_version', resource_bundle_version) \
             FROM attempts WHERE attempt_id = 'T-a/attempt-4'",
        )?)?;
        let packed: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'context.packed' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-a/attempt-4'",
        )?)?;
        // The four fields, none empty.
        let sha = row["prompt_sha256"].as_str().ok_or("no prompt sha256")?.to_owned();
        let version = row["resource_bundle_version"].as_str().ok_or("no bundle version")?;
        assert_eq!(sha.len(), 64);
        assert_eq!(version, hoh_core::objects::resource_bundle_version());
        let inputs = packed["inputs"].as_array().ok_or("no inputs")?;
        assert!(!inputs.is_empty());
        assert_eq!(packed["policy"]["rule"], "prefix");
        assert!(packed["policy"]["estimator"].is_string());
        // What codex was sent is what was recorded.
        let sent = codex_prompts(&world)?;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let digest = |bytes: &[u8]| -> String {
            <sha2::Sha256 as sha2::Digest>::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
        };
        assert_eq!(digest(sent[0].as_bytes()), sha);
        assert!(sent[0].contains("份额是硬上限 {{GOAL}}"), "an AC naming a placeholder is text");
        // Its evidence went in as summaries, ranked newest first.
        let summaries: Vec<&str> = inputs
            .iter()
            .filter(|i| i["kind"] == "entry" && i["disposition"] == "summary")
            .filter_map(|i| i["path"].as_str())
            .collect();
        assert_eq!(summaries, ["attempts/T-a/attempt-3", "attempts/T-a/attempt-2", "attempts/T-a/attempt-1"]);
        // Everything it was packed from changes; the repack reads its objects only.
        std::fs::write(project.join(".hoh/prompts/dev.md"), "另一个契约。\n")?;
        exec(
            &project,
            "UPDATE acceptance_criteria SET body = 'changed'; DELETE FROM attempts WHERE task_id = 'T-a' AND attempt_id <> 'T-a/attempt-4'; \
             UPDATE runs SET config_snapshot_json = '{}';",
        )?;
        let _ = std::fs::remove_dir_all(project.join(".hoh/attempts"));
        let manifest = packed["manifest"].as_str().ok_or("no manifest")?;
        let again = hoh_core::objects::repack(&project, manifest)?;
        assert_eq!(again.sha256, sha);
        keep(
            "test-039-04",
            &json!({"attempt": row, "context_packed": packed, "repacked_sha256": again.sha256}),
        )?;
        // A damaged object fails the repack; it is never repacked into something else.
        let entry = inputs
            .iter()
            .find(|i| i["kind"] == "entry")
            .and_then(|i| i["sha256"].as_str())
            .ok_or("no entry")?;
        let object = project.join(hoh_core::objects::relative_path(entry));
        std::fs::write(&object, b"damaged")?;
        assert!(hoh_core::objects::repack(&project, manifest).is_err());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_turn_whose_mandatory_items_exceed_its_runs_budget_is_refused_and_its_task_waits_for_a_decision()
-> R {
    // REQ-039 failure mode 1 (docs/adr/ADR-026.md decision 9): the run's budget (its snapshot) below what the task's
    // mandatory items need: the attempt fails CONTEXT_BUDGET_TOO_SMALL before anything runs, with both numbers; its
    // task is BLOCKED for a decision with a need_decision event; no prompt is recorded.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\n",
    )?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, "s1").await?;
        exec(
            &project,
            &format!(
                "UPDATE runs SET config_snapshot_json = json_set(config_snapshot_json, '$.context', \
                 json('{{\"budget_tokens\": 100, \"shares\": [45, 35, 20]}}')) WHERE run_id = '{run_id}';"
            ),
        )?;
        ready(&project, &run_id, &[("T-b", "developer")])?;
        until(&project, &ended("T-b/attempt-1"), "FAILED/CONTEXT_BUDGET_TOO_SMALL", Duration::from_secs(60)).await?;
        let refused: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'attempt.start_refused' AND event_id = 'T-b/attempt-1/start-refused'",
        )?)?;
        assert_eq!(refused["details"]["available"], 100, "{refused}");
        assert!(refused["details"]["required"].as_u64().is_some_and(|r| r > 100), "{refused}");
        assert_eq!(refused["details"]["estimated"], true);
        assert_eq!(
            query(&project, "SELECT state || '/' || blocked_reason FROM tasks WHERE task_id = 'T-b'")?,
            "BLOCKED/NEED_DECISION"
        );
        assert_eq!(
            query(
                &project,
                "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision' \
                 AND json_extract(payload_json, '$.task_id') = 'T-b'"
            )?,
            "refused_deterministically"
        );
        assert_eq!(
            query(&project, "SELECT coalesce(prompt_sha256, '') || '/' || coalesce(pid, '') FROM attempts WHERE attempt_id = 'T-b/attempt-1'")?,
            "/",
            "nothing sent, no process"
        );
        assert!(codex_prompts(&world)?.is_empty());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}
