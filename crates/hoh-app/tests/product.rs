//! The product executor in a host (docs/adr/ADR-022.md decisions 9 to 17): the roles' agents, here fake CLIs answering
//! the probe from their recordings and replaying recorded turns, run through `hoh __gate` (TEST-027-03 in the host,
//! TEST-018-02, -03, -05).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

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

/// The provider-fault recordings (tests/fixtures/protocol/_provider-faults, record_faults.py).
const FAULTS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/_provider-faults"
);

/// A line of 8 MiB with no LF (AC-027-03).
const HUGE: usize = 8 << 20;

/// A fake agent's TaskResult from nothing but its product prompt's identity header (docs/adr/ADR-028.md decision 2):
/// the prompt as it came on stdin (`argv[1]`), plain or as JSON lines whose strings hold it; a TaskResult of status
/// `argv[2]` appended to the bridge file the header names. While the file `argv[3]` exists, its bytes are first written
/// as a self-test record in the directory the header names and named in the result's evidence (decision 14).
const RESULT_FAKE: &str = r#"import hashlib, json, os, re, sys
raw = open(sys.argv[1], encoding="utf-8").read()
texts = [raw]
for line in raw.splitlines():
    try:
        stack = [json.loads(line)]
    except ValueError:
        continue
    while stack:
        x = stack.pop()
        if isinstance(x, str):
            texts.append(x)
        elif isinstance(x, dict):
            stack.extend(x.values())
        elif isinstance(x, list):
            stack.extend(x)
text = "\n".join(texts)
field = lambda key: re.search("- " + key + "：(.+)", text).group(1).strip()
bridge = re.search("- 输出文件（[^）]*）：(.+)", text).group(1).strip()
result = {"schema_version": "1.0", "run_id": field("run_id"), "task_id": field("task_id"),
          "attempt_id": field("attempt_id"), "role": field("role"), "role_epoch": int(field("role_epoch")),
          "status": sys.argv[2], "summary": "done by the fake", "candidate_id": None, "evidence": [], "issues": [],
          "next_action": "verify"}
tests = re.search("- 自测记录目录（[^）]*）：(.+)", text)
if len(sys.argv) > 3 and os.path.exists(sys.argv[3]) and tests:
    record = os.path.join(tests.group(1).strip(), "report.txt")
    os.makedirs(os.path.dirname(record), exist_ok=True)
    data = open(sys.argv[3], "rb").read()
    open(record, "wb").write(data)
    result["evidence"].append({"path": record, "sha256": hashlib.sha256(data).hexdigest()})
with open(bridge, "a", encoding="utf-8") as f:
    f.write(json.dumps(result) + "\n")
"#;

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

/// The WATCH turn (docs/adr/ADR-022.md decision 19): a "tool" (`sleep 600`, its pid `argv[3]`) runs from before the
/// recorded turn's first frame; once `claude.d/watch-tool-end` exists it is killed and reaped and `claude.d/watch-alone`
/// is made; then nothing runs and nothing is printed until `claude.d/watch-speak` exists; then its second frame, and the
/// rest of the turn once `claude.d/watch-end` exists.
const WATCH_STAND_IN: &str = "import os, signal, sys, time
d, frames, tool = sys.argv[1], sys.argv[2], int(sys.argv[3])
def wait(name):
    while not os.path.exists(os.path.join(d, name)):
        time.sleep(0.02)
wait(\"watch-tool-end\")
os.kill(tool, signal.SIGKILL)
os.waitpid(tool, 0)
open(os.path.join(d, \"watch-alone\"), \"w\").close()
lines = open(frames).readlines()
wait(\"watch-speak\")
sys.stdout.write(lines[1])
sys.stdout.flush()
wait(\"watch-end\")
sys.stdout.write(\"\".join(lines[2:]))
sys.stdout.flush()
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
        self.config_with(effective, self.registry(), None, None)
    }

    /// The PATH the world's agents are found on: the fakes, then the tools they use.
    fn path(&self) -> String {
        format!(
            "{}:{}",
            self.bin().display(),
            self.dir.path().join("tools").display()
        )
    }

    /// A registry of the world's agents, pi's settings from a directory of the world's own, never the user's
    /// ~/.pi/agent (M2T17R-59).
    fn registry(&self) -> std::sync::Arc<hoh_app::host::agents::Registry> {
        std::sync::Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            self.path().into(),
            Some(self.dir.path().join("pi-agent")),
        ))
    }

    /// The product's host as [`World::config_of`], its agents those of `registry`, with `keys` called with each
    /// attempt's gate key as its executor draws it and `observe` with each diagnostic its turns collect as they do
    /// (docs/adr/ADR-030.md decision 9).
    fn config_with(
        &self,
        effective: hoh_core::config::Config,
        registry: std::sync::Arc<hoh_app::host::agents::Registry>,
        keys: Option<hoh_app::host::product::KeySeen>,
        observe: Option<hoh_app::host::product::DiagnosticSeen>,
    ) -> HostConfig {
        let graces = (
            Duration::from_secs(u64::from(effective.execution.cancel_native_grace_seconds)),
            Duration::from_secs(u64::from(effective.execution.cancel_term_grace_seconds)),
        );
        let mut config = HostConfig::product_with(
            &self.project(),
            effective,
            env!("CARGO_BIN_EXE_hoh").into(),
            registry.clone(),
        );
        if let Some(runner) = config.runner.as_mut()
            && (keys.is_some() || observe.is_some())
        {
            let mut executor = hoh_app::host::product::ProductExecutor::new(
                registry,
                self.project(),
                env!("CARGO_BIN_EXE_hoh").into(),
                graces,
            );
            if let Some(keys) = keys {
                executor = executor.with_keys(keys);
            }
            if let Some(observe) = observe {
                executor = executor.with_observe(observe);
            }
            runner.executor = std::sync::Arc::new(executor);
        }
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
    /// CANCEL, [`CANCEL_STAND_IN`]; for one whose marker names WATCH, [`WATCH_STAND_IN`] with its tool started before its
    /// first frame; one whose marker names RESULT first writes a COMPLETED TaskResult by its prompt's
    /// identity header ([`RESULT_FAKE`]; with the self-test record `claude.d/self-test` while it exists), then prints
    /// `claude.d/big-log` while it exists (its raw log's extra lines); one whose marker names NATIVE-<code> replays the
    /// recorded turn with its `result` frame's subtype `<code>` (COMPAT_UNVERIFIED, MODEL_UNAVAILABLE, BLOCKED_AUTH); one whose
    /// marker names LIMIT or RETRY-AFTER replays `claude.d/limit.jsonl` or `claude.d/retry-after.jsonl` ([`World::faults`]).
    fn claude(&self, hold: Option<&Path>) -> R {
        let body = self.claude_body(hold)?;
        self.script("claude", &body)
    }

    /// [`World::claude`] installed as claude's own installer does: the launcher `bin/claude` a link to the version
    /// file `claude-versions/A`, beside it `claude-versions/B`, the same script with one more line (another identity,
    /// the same version); a turn waits for `hold` as [`World::claude`]'s does.
    fn claude_versions(&self, hold: Option<&Path>) -> R {
        let body = self.claude_body(hold)?;
        let versions = self.dir.path().join("claude-versions");
        std::fs::create_dir_all(&versions)?;
        for (name, text) in [
            ("A", format!("#!/bin/bash\n{body}\n")),
            ("B", format!("#!/bin/bash\n{body}\n# B\n")),
        ] {
            std::fs::write(versions.join(name), text)?;
            std::fs::set_permissions(versions.join(name), std::fs::Permissions::from_mode(0o755))?;
        }
        std::os::unix::fs::symlink(versions.join("A"), self.bin().join("claude"))?;
        Ok(())
    }

    /// The body of [`World::claude`]'s script.
    fn claude_body(&self, hold: Option<&Path>) -> R<String> {
        let wait = hold.map_or(String::new(), |h| {
            format!("while [ ! -e '{}' ]; do sleep 0.05; done\n", h.display())
        });
        std::fs::create_dir_all(self.bin().join("claude.d"))?;
        std::fs::write(self.bin().join("claude.d/result.py"), RESULT_FAKE)?;
        // Turns whose result is claude's own error named like a reserved code (docs/adr/ADR-030.md decision 6).
        let recorded =
            std::fs::read_to_string(format!("{PROTOCOL}/claude/2.1.318/success/stdout.jsonl"))?;
        for code in ["COMPAT_UNVERIFIED", "MODEL_UNAVAILABLE", "BLOCKED_AUTH"] {
            let mut text = String::new();
            for line in recorded.lines() {
                let mut frame: Value = serde_json::from_str(line)?;
                if frame["type"] == "result" {
                    frame["subtype"] = json!(code);
                    frame["is_error"] = json!(true);
                }
                text.push_str(&format!("{frame}\n"));
            }
            std::fs::write(
                self.bin().join(format!("claude.d/native-{code}.jsonl")),
                text,
            )?;
        }
        Ok(format!(
            "f='{PROTOCOL}/claude/2.1.318'; d='{bin}/claude.d'\n\
                 case \"$1\" in --version) [ -e \"$d/hang\" ] && {{ echo $$ > \"$d/probe-pid\"; sleep 120; }}; [ -e \"$d/slow\" ] && sleep 3; echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
                 echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"; env > \"$d/env.$$\"; cat > \"$d/stdin.$$\"\n\
                 case \"$HOH_ATTEMPT_ID\" in\n\
                  *HUGE*) head -n 1 \"$f/success/stdout.jsonl\"; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
                  *CANCEL*) exec python3 -c '{CANCEL_STAND_IN}' \"$d\" \"$f/success/stdout.jsonl\";;\n\
                  *WATCH*) sleep 600 & t=$!; echo $t > \"$d/watch-tool\"; head -n 1 \"$f/success/stdout.jsonl\"; exec python3 -c '{WATCH_STAND_IN}' \"$d\" \"$f/success/stdout.jsonl\" \"$t\";;\n\
                  *RESULT*) python3 \"$d/result.py\" \"$d/stdin.$$\" COMPLETED \"$d/self-test\" || exit 1; [ -f \"$d/big-log\" ] && cat \"$d/big-log\";;\n\
                  *NATIVE-*) code=${{HOH_ATTEMPT_ID#*NATIVE-}}; cat \"$d/native-${{code%%/*}}.jsonl\"; exit 0;;\n\
                  *LIMIT*) cat \"$d/limit.jsonl\"; exit 1;;\n\
                  *RETRY-AFTER*) cat \"$d/retry-after.jsonl\"; exit 1;;\n\
                 esac\n{wait}cat \"$f/success/stdout.jsonl\"\n",
            bin = self.bin().display()
        ))
    }

    /// A fake `codex` 0.166.0: the probe's answers (helps, App Server schema) from the recording (while
    /// `codex.d/hang-schema` exists, generating the schema waits 120 s, its pid in `codex.d/schema-pid` and its child's
    /// in `codex.d/schema-member`); `app-server`
    /// replays the recorded `app-server-turn`, its model given as `model`; for an attempt whose marker names HUGE, the
    /// frames before `turn/completed` and then 8 MiB with no LF; for one whose marker names UNKNOWN, an effect (a line
    /// in `codex.d/effects`), the frames up to a started command, and its end.
    fn codex(&self, model: &str) -> R {
        let body = self.codex_body(model, "A")?;
        self.script("codex", &body)
    }

    /// The body of [`World::codex`]'s script, which appends `<name> <marker> <argv>` to `codex.d/calls` at each run;
    /// for an attempt whose marker names SIXTYFIVE it writes the gate's refusal line on stderr and exits 65 with no
    /// frame; FORGE, the same, its first attempt only after `codex.d/forge.py` (when there) forged records in its
    /// gate's log (a retry forging again would add a `forged.<pid>` of its own and append to that log); NATIVE, the
    /// recorded turn with its `turn/completed` failed by `codexErrorInfo: COMPAT_UNVERIFIED`; LIMIT, `codex.d/limit.jsonl`
    /// ([`World::faults`]). An App Server run writes
    /// its marker to `codex.d/marker.<pid>` (its log is `codex.d/log.<pid>`); for an attempt whose marker names
    /// HOLD-INIT it waits for `codex.d/release` before it answers anything, HOLD-TURN, the first attempt's turn writes its
    /// first frame and then waits for `codex.d/release` (docs/adr/ADR-030.md decision 9's re-checks) and a later
    /// attempt's ends at once with no frame (a writer's turn that wrote no TaskResult is corrected by another attempt,
    /// which then never resumes the thread the fake cannot answer for).
    fn codex_body(&self, model: &str, name: &str) -> R<String> {
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
        // A turn that fails with codex's own error named like a reserved code (docs/adr/ADR-030.md decision 6).
        let mut native = frames.clone();
        native[end]["params"]["turn"]["status"] = json!("failed");
        native[end]["params"]["turn"]["error"] =
            json!({"codexErrorInfo": "COMPAT_UNVERIFIED", "message": "codex's own text"});
        std::fs::write(d.join("native.jsonl"), lines(&native))?;
        // A turn that writes its first frame and waits for the test's release before the rest.
        let turn_start = requests
            .iter()
            .find(|(_, method)| method.as_str() == Some("turn/start"))
            .and_then(|(id, _)| id.parse::<i64>().ok())
            .ok_or("no turn/start request")?;
        let begun = frames
            .iter()
            .position(|f| f["id"].as_i64() == Some(turn_start) && f.get("result").is_some())
            .ok_or("no turn/start answer")?;
        let mut held = frames.clone();
        held.insert(
            begun + 1,
            json!({"hoh_hold": d.join("release").display().to_string()}),
        );
        std::fs::write(d.join("hold-turn.jsonl"), lines(&held))?;
        std::fs::write(d.join("requests.json"), Value::Object(requests).to_string())?;
        Ok(format!(
            "f='{PROTOCOL}/codex/0.166.0'; d='{d}'\n\
                 echo \"{name} $HOH_ATTEMPT_ID $*\" >> \"$d/calls\"\n\
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
                   echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"; echo \"$HOH_ATTEMPT_ID\" > \"$d/marker.$$\"\n\
                   case \"$HOH_ATTEMPT_ID\" in\n\
                    *HUGE*) frames=\"$d/huge.jsonl\"; opt=;;\n\
                    *UNKNOWN*) frames=\"$d/unknown.jsonl\"; opt=--exit-when-done; echo \"$HOH_ATTEMPT_ID\" >> \"$d/effects\";;\n\
                    *DEGRADE*) frames=\"$d/degrade.jsonl\"; opt=;;\n\
                    *INCOMPLETE/attempt-1) frames=\"$d/incomplete.jsonl\"; opt=--exit-when-done;;\n\
                    *SIXTYFIVE*) echo 'hoh gate: COMPAT_UNVERIFIED: implementation_sha256' >&2; exit 65;;\n\
                    *FORGE/attempt-1) [ -f \"$d/forge.py\" ] && python3 \"$d/forge.py\" \"$d/forged.$$\" \"$@\"\n\
                     echo 'hoh gate: COMPAT_UNVERIFIED: implementation_sha256' >&2; exit 65;;\n\
                    *FORGE*) echo 'hoh gate: COMPAT_UNVERIFIED: implementation_sha256' >&2; exit 65;;\n\
                    *NATIVE*) frames=\"$d/native.jsonl\"; opt=;;\n\
                    *LIMIT*) frames=\"$d/limit.jsonl\"; opt=;;\n\
                    *HOLD-INIT*) while [ ! -e \"$d/release\" ]; do sleep 0.05; done; frames=\"$d/frames.jsonl\"; opt=;;\n\
                    *HOLD-TURN/attempt-1) frames=\"$d/hold-turn.jsonl\"; opt=;;\n\
                    *HOLD-TURN/*) exit 3;;\n\
                    *) frames=\"$d/frames.jsonl\"; opt=;;\n\
                   esac\n\
                   exec timeout 60 python3 '{APP_SERVER}' \"$frames\" \"$d/requests.json\" \"$d/log.$$\" $opt;;\n\
                 esac\nexit 2\n",
            d = d.display()
        ))
    }

    /// A fake `pi` 1.5.0: the probe's answers from the recording; a turn replays the recorded `rpc-prompt`, or, for an
    /// attempt whose marker names HUGE, one record and then 8 MiB with no LF.
    fn pi(&self) -> R {
        let body = self.pi_body("A")?;
        self.script("pi", &body)
    }

    /// The body of [`World::pi`]'s script, which appends `<name> <marker> <argv>` to `pi.d/calls` at each run and, for a
    /// turn, its marker to `pi.d/marker.<pid>` (its log is `pi.d/log.<pid>`). For an attempt whose marker names
    /// HOLD-ANSWER its turn waits for `pi.d/release` before it answers anything; CHMOD, it first changes its own mode to
    /// the one it has (docs/adr/ADR-030.md decision 9's re-check before the first input), then makes
    /// `pi.d/chmodded.<attempt-n>` and waits for `pi.d/chmod-go.<attempt-n>` (`<attempt-n>` its attempt id's last part);
    /// LIMIT, `pi.d/limit.jsonl` ([`World::faults`]).
    fn pi_body(&self, name: &str) -> R<String> {
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
        // A turn that waits for the test's release before it answers anything.
        std::fs::write(
            d.join("hold.jsonl"),
            format!(
                "{}\n{}",
                json!({"hoh_hold": d.join("release").display().to_string()}),
                std::fs::read_to_string(d.join("frames.jsonl"))?
            ),
        )?;
        Ok(format!(
            "f='{PROTOCOL}/pi/1.5.0'; d='{d}'\n\
             echo \"{name} $HOH_ATTEMPT_ID $*\" >> \"$d/calls\"\n\
             case \"$1\" in --version) echo '1.5.0'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
             echo $$ > \"$d/pid.$$\"; pwd -P > \"$d/cwd.$$\"; printf '%s\\n' \"$@\" > \"$d/argv.$$\"; echo \"$HOH_ATTEMPT_ID\" > \"$d/marker.$$\"\n\
             case \"$HOH_ATTEMPT_ID\" in\n\
              *HUGE*) printf '%s\\n' '{{\"type\":\"agent_start\"}}'; head -c {HUGE} /dev/zero | tr '\\0' x; sleep 20; exit 0;;\n\
              *EARLY-EXIT/attempt-1) exec timeout 60 python3 '{PI_RPC}' \"$d/early-exit.jsonl\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\";;\n\
              *CHMOD*) python3 -c 'import os, sys; os.chmod(sys.argv[1], os.stat(sys.argv[1]).st_mode & 0o7777)' \"$0\"\n\
               n=\"${{HOH_ATTEMPT_ID##*/}}\"; : > \"$d/chmodded.$n\"; while [ ! -e \"$d/chmod-go.$n\" ]; do sleep 0.05; done;;\n\
             esac\n\
             frames=\"$d/frames.jsonl\"; case \"$HOH_ATTEMPT_ID\" in *HOLD-ANSWER*) frames=\"$d/hold.jsonl\";; *LIMIT*) frames=\"$d/limit.jsonl\";; esac\n\
             exec timeout 60 python3 '{PI_RPC}' \"$frames\" \"$d/commands.json\" \"$d/log.$$\" -- \"$@\"\n",
            d = d.display()
        ))
    }

    /// pi as its installer lays it out (docs/adr/ADR-030.md decision 9, pi's launcher): the launcher `bin/pi` runs the
    /// release `install/current-version` names, each of `versions` a release `install/releases/<version>` whose
    /// `node_modules/.bin/pi` links to [`World::pi_body`]'s script naming itself `<version>`. While `pi.d/hold-look`
    /// exists the launcher's `--version` waits: a look at pi (a probe's first command) is held.
    fn pi_releases(&self, versions: &[&str], current: &str) -> R {
        let root = self.dir.path();
        for version in versions {
            let release = root.join("install/releases").join(version);
            let bundle = release.join("node_modules/pi-coding-agent/dist/bundle");
            std::fs::create_dir_all(&bundle)?;
            let script = bundle.join("cli.js");
            std::fs::write(
                &script,
                format!("#!/bin/bash\n{}\n", self.pi_body(version)?),
            )?;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
            std::fs::create_dir_all(release.join("node_modules/.bin"))?;
            std::os::unix::fs::symlink(
                "../pi-coding-agent/dist/bundle/cli.js",
                release.join("node_modules/.bin/pi"),
            )?;
        }
        std::fs::write(root.join("install/current-version"), format!("{current}\n"))?;
        self.script(
            "pi",
            &format!(
                "root='{root}'\n\
                 [ \"$1\" = --version ] && while [ -e '{hold}' ]; do sleep 0.05; done\n\
                 exec \"$root/install/releases/$(cat \"$root/install/current-version\")/node_modules/.bin/pi\" \"$@\"",
                root = root.display(),
                hold = self.bin().join("pi.d/hold-look").display()
            ),
        )
    }

    /// The fakes' LIMIT and RETRY-AFTER turns from the provider-fault recordings (record_faults.py; M2T07R-256): claude's
    /// subscriber usage limit and codex App Server's, each with its reset moved to `reset` (unix seconds) and codex's
    /// model named `model`; claude's 429 with a Retry-After of 120 s it dropped; pi's 429, which it retried itself and
    /// ended with text only.
    fn faults(&self, reset: u64, model: &str) -> R {
        let moved = |case: &str, header: &str| -> R<String> {
            let text = std::fs::read_to_string(format!("{FAULTS}/{case}/stdout.jsonl"))?;
            let meta: Value = serde_json::from_str(&std::fs::read_to_string(format!(
                "{FAULTS}/{case}/meta.json"
            ))?)?;
            let sent = meta["provider"]["requests"][0]["headers"][header]
                .as_str()
                .ok_or("no reset sent")?;
            let (from, to) = (
                format!("\"resetsAt\":{sent}"),
                format!("\"resetsAt\":{reset}"),
            );
            if !text.contains(&from) {
                return Err(format!("{case}: no {from}").into());
            }
            Ok(text.replace(&from, &to))
        };
        for name in ["claude", "codex", "pi"] {
            std::fs::create_dir_all(self.bin().join(format!("{name}.d")))?;
        }
        std::fs::write(
            self.bin().join("claude.d/limit.jsonl"),
            moved(
                "claude/2.1.325/fault-usage-limit",
                "anthropic-ratelimit-unified-reset",
            )?,
        )?;
        std::fs::copy(
            format!("{FAULTS}/claude/2.1.325/fault-429-retry-after-long/stdout.jsonl"),
            self.bin().join("claude.d/retry-after.jsonl"),
        )?;
        std::fs::write(
            self.bin().join("codex.d/limit.jsonl"),
            moved(
                "codex/0.166.0/app-server-fault-usage-limit",
                "x-codex-primary-reset-at",
            )?
            .replace("gpt-fake-5", model),
        )?;
        std::fs::copy(
            format!("{FAULTS}/pi/1.5.0/rpc-fault-429-retry-after/stdout.jsonl"),
            self.bin().join("pi.d/limit.jsonl"),
        )?;
        Ok(())
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

/// A started run's id. The project has an approved specification first (s1, revision 1, the one [`ready`] names): a
/// run adopts it at its start (docs/adr/ADR-031.md decision 1; with none it would be BLOCKED on runtime:no_spec).
async fn started(host: &Host, project: &Path, key: &str) -> R<String> {
    exec(
        project,
        "INSERT OR IGNORE INTO specs(spec_id, project_id, sha256, revision) \
         SELECT 's1', project_id, printf('%064d', 1), 1 FROM projects;",
    )?;
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
        let run_id = started(&host, &project, "s1").await?;
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

/// REQ-042 behaviour 1 in the host for `agent` (M2T07R-256; owner decision 2026-11-16): the developer bound to it runs
/// task `T-<marker>`, whose turn replays a recorded provider fault. A usage limit whose reset the agent's stream carries
/// (`reset`) fails the attempt transient with that reset as its Retry-After: the task is due exactly then, the wait is
/// recorded (`task.retry_scheduled` with `retry_after_ms` and no jitter bound, `attempt.provider_reset` with the
/// instant, the OBSERVED record with both) and the run waits for it (WAITING_EXTERNAL). A fault whose stream carries
/// none (a Retry-After the CLI consumed, pi's text) gets the first full-jitter bound, 2 s.
async fn a_providers_limit_in_the_host(agent: &str, marker: &str, reset_given: bool) -> R {
    let world = World::new()?;
    let model = "gpt-6-astra";
    match agent {
        "claude" => world.claude(None)?,
        "codex" => world.codex(model)?,
        _ => world.pi()?,
    }
    let reset = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        + 7_200;
    world.faults(reset, model)?;
    world.bind_installed()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        if agent != "claude" {
            configure(&host, "dev", team_revision(&project)?, json!({"developer": {"agent": agent}})).await?;
        }
        let run_id = started(&host, &project, "s1").await?;
        let task = format!("T-{marker}");
        let attempt = format!("{task}/attempt-1");
        ready(&project, &run_id, &[(task.as_str(), "developer")])?;
        let scheduled = format!(
            "SELECT payload_json FROM events WHERE kind = 'task.retry_scheduled' \
             AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
        );
        until(&project, &format!("SELECT count(*) FROM ({scheduled})"), "1", Duration::from_secs(60)).await?;
        let scheduled: Value = serde_json::from_str(&query(&project, &scheduled)?)?;
        let observed: Value = serde_json::from_str(&query(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.detail') FROM events WHERE kind = 'effect.observed' \
                 AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
            ),
        )?)?;
        let provider_reset = query(
            &project,
            &format!(
                "SELECT payload_json FROM events WHERE kind = 'attempt.provider_reset' \
                 AND event_id LIKE '{attempt}/%'"
            ),
        )?;
        let at = hoh_core::time::rfc3339_utc(std::time::UNIX_EPOCH + Duration::from_secs(reset));
        if reset_given {
            assert_eq!(scheduled["next_retry_at"], json!(at), "{scheduled}");
            assert_eq!(scheduled["bound_ms"], Value::Null, "{scheduled}");
            let failed: std::time::SystemTime = hoh_core::time::parse_rfc3339(
                scheduled["failed_at"].as_str().ok_or("no failed_at")?,
            )
            .ok_or("failed_at")?;
            // The wait counts from the end's own time, a part of a second after its whole second `failed_at`.
            let most = u64::try_from(
                (Duration::from_secs(reset) - failed.duration_since(std::time::UNIX_EPOCH)?).as_millis(),
            )?;
            let delay = scheduled["delay_ms"].as_u64().ok_or("no delay")?;
            assert!(delay <= most && delay + 1_000 > most, "{scheduled}");
            assert_eq!(scheduled["failure"]["class"], json!("transient"), "{scheduled}");
            assert_eq!(scheduled["failure"]["retry_after_ms"], json!(delay), "{scheduled}");
            let event: Value = serde_json::from_str(&provider_reset)?;
            assert_eq!(event["retry_at"], json!(at), "{event}");
            assert_eq!(event["retry_after_ms"], scheduled["failure"]["retry_after_ms"], "{event}");
            assert_eq!(observed["retry_at"], json!(at), "{observed}");
            assert_eq!(observed["retry_after_ms"], scheduled["failure"]["retry_after_ms"], "{observed}");
            until(
                &project,
                &format!("SELECT state || '/' || next_retry_at FROM tasks WHERE task_id = '{task}'"),
                &format!("READY/{at}"),
                Duration::from_secs(10),
            )
            .await?;
            until(
                &project,
                &format!("SELECT state FROM runs WHERE run_id = '{run_id}'"),
                "WAITING_EXTERNAL",
                Duration::from_secs(30),
            )
            .await?;
        } else {
            assert_eq!(scheduled["bound_ms"], json!(2_000), "{scheduled}");
            assert_eq!(scheduled["failure"]["retry_after_ms"], Value::Null, "{scheduled}");
            assert_eq!(provider_reset, "", "a reset was recorded");
            assert_eq!(observed.get("retry_at"), None, "{observed}");
        }
        keep(
            &format!("m2t07r-256-{agent}-{marker}"),
            &json!({"agent": agent, "scheduled": scheduled, "observed": observed, "provider_reset": provider_reset}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_claude_usage_limit_waits_for_its_reset() -> R {
    a_providers_limit_in_the_host("claude", "LIMIT", true).await
}

#[tokio::test]
async fn a_codex_usage_limit_waits_for_its_reset() -> R {
    a_providers_limit_in_the_host("codex", "LIMIT", true).await
}

#[tokio::test]
async fn a_claude_429_whose_retry_after_it_dropped_gets_full_jitter() -> R {
    a_providers_limit_in_the_host("claude", "RETRY-AFTER", false).await
}

#[tokio::test]
async fn a_pi_429_it_retried_itself_gets_full_jitter() -> R {
    a_providers_limit_in_the_host("pi", "LIMIT", false).await
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
        let run_id = started(&host, &project, "s1").await?;
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
        // codex was asked for the binding's model, and reported it: every thread/start of the developer's attempts
        // (the binding this test changes), found by their App Server runs' markers. The run's Commander is codex too,
        // and a supervision turn of its asks for the Commander's binding (no model) when a wake starts one meanwhile:
        // the world has no pi, so the qa and researcher bindings are blocked and their team.binding_blocked wakes it
        // (docs/adr/ADR-025.md).
        let developers: Vec<String> = serde_json::from_str(&query(
            &project,
            "SELECT json_group_array(attempt_id) FROM attempts JOIN tasks USING (task_id) WHERE tasks.role = 'developer'",
        )?)?;
        let d = world.bin().join("codex.d");
        let asked: Vec<Value> = std::fs::read_dir(&d)?
            .flatten()
            .filter_map(|e| {
                let pid = e.file_name().to_string_lossy().strip_prefix("marker.")?.to_owned();
                let marker = std::fs::read_to_string(e.path()).ok()?;
                developers
                    .iter()
                    .any(|a| marker.trim().ends_with(&format!("/{a}")))
                    .then(|| d.join(format!("log.{pid}/received.jsonl")))
            })
            .filter_map(|log| std::fs::read_to_string(log).ok())
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
        let run_id = started(&host, &project, "s1").await?;
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
        // Every turn was started through the start gate, which gave it its attempt's marker. The Commander's turns are
        // claude's too in this world: the specification the run adopted at its start wakes it (docs/adr/ADR-031.md
        // decision 1); only the cto's and the developer's turns are counted.
        let commander = query(
            &project,
            "SELECT coalesce(group_concat(a.attempt_id, ' '), '') FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'commander'",
        )?;
        let markers: Vec<String> = std::fs::read_dir(world.bin().join("claude.d"))?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("env."))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .filter_map(|env| {
                env.lines()
                    .find_map(|l| l.strip_prefix("HOH_ATTEMPT_ID=").map(str::to_owned))
            })
            .filter(|m| !commander.split(' ').any(|a| !a.is_empty() && m.ends_with(&format!("/{a}"))))
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
        let run_id = started(&host, &project, "s1").await?;
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
        // What codex was sent for the attempt: the contract, then the task's header; its sha256 is the attempt's.
        let sent = codex_prompts_of(&world, |a| a == "T-sec/attempt-1")?;
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
        let run_id = started(&host, &project, "s1").await?;
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
    // its task waits for a retry), never another contract in its place, with an attempt.start_refused event saying why.
    // An agent gone from the host since its binding: staffing blocks the bindings to it at its next look
    // (docs/adr/ADR-030.md decisions 4 and 5: no fallback, AGENT_UNAVAILABLE with the line that says why), so a task of
    // its role due then starts no attempt: the pace marks it BLOCKED(AGENT_UNAVAILABLE) for its role's vacancy
    // (decision 7: task.blocked{cause: vacancy}, attempt_count untouched).
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
        let run_id = started(&host, &project, "s1").await?;
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
            query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-a' \
                 AND (prompt_revision IS NOT NULL OR prompt_sha256 IS NOT NULL)"
            )?,
            "0",
            "no contract was sent in its place"
        );
        std::fs::remove_file(world.bin().join("claude"))?;
        // The registry looks at the team's agents: claude is gone, and staffing blocks the developer's binding.
        until(
            &project,
            &format!(
                "SELECT state || '/' || blocked_reason || '/' || missing_json FROM role_bindings \
                 WHERE run_id = '{run_id}' AND role = 'developer'"
            ),
            "BLOCKED/AGENT_UNAVAILABLE/[\"claude:not_installed\"]",
            limit,
        )
        .await?;
        // Exactly one block of the developer, by staffing, whose evidence object names claude's look: not found.
        let blocked: Vec<String> = serde_json::from_str(&query(
            &project,
            &format!(
                "SELECT json_group_array(payload_json) FROM events WHERE run_id = '{run_id}' \
                 AND kind = 'team.binding_blocked' AND json_extract(payload_json, '$.role') = 'developer'"
            ),
        )?)?;
        assert_eq!(blocked.len(), 1, "{blocked:?}");
        let blocked: Value = serde_json::from_str(&blocked[0])?;
        assert_eq!(
            (&blocked["cause"], &blocked["reason"], &blocked["missing"]),
            (
                &json!("staffing"),
                &json!("AGENT_UNAVAILABLE"),
                &json!(["claude:not_installed"])
            ),
            "{blocked}"
        );
        let sha256 = blocked["probe_sha256"].as_str().ok_or("no probe_sha256")?;
        assert!(
            blocked["probe_artifact_id"]
                .as_str()
                .is_some_and(|a| a.starts_with(sha256)),
            "{blocked}"
        );
        let evidence: Value = serde_json::from_slice(&hoh_core::objects::get(&project, sha256)?)?;
        let claude = evidence
            .as_array()
            .and_then(|items| items.iter().find(|i| i["agent"] == "claude"))
            .ok_or("no claude in the evidence")?;
        assert_eq!(claude["probe"]["class"], "NotFound", "{evidence}");
        ready(&project, &run_id, &[("T-d", "developer")])?;
        // The next pace marks T-d for its role's vacancy: BLOCKED(AGENT_UNAVAILABLE), no attempt, attempt_count 0.
        until(
            &project,
            "SELECT state || '/' || blocked_reason || '/' || attempt_count || '/' || \
             (SELECT count(*) FROM attempts WHERE task_id = 'T-d') FROM tasks WHERE task_id = 'T-d'",
            "BLOCKED/AGENT_UNAVAILABLE/0/0",
            limit,
        )
        .await?;
        let marked: Value = serde_json::from_str(&query(
            &project,
            "SELECT json_group_array(json(payload_json)) FROM events WHERE kind = 'task.blocked' \
             AND json_extract(payload_json, '$.task_id') = 'T-d'",
        )?)?;
        assert_eq!(
            marked,
            json!([{"task_id": "T-d", "reason": "AGENT_UNAVAILABLE", "missing": ["claude:not_installed"],
                    "cause": "vacancy"}])
        );
        // Ticks go on (200 ms each): the blocked role is dispatched nothing.
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(
            query(
                &project,
                "SELECT state || '/' || attempt_count || '/' || \
                 (SELECT count(*) FROM attempts WHERE task_id = 'T-d') FROM tasks WHERE task_id = 'T-d'"
            )?,
            "BLOCKED/0/0"
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
        let run_id = started(&host, &project, "s1").await?;
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
            run_dir: None,
            helpers: None,
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
        let run_id = started(&host, &project, "s1").await?;
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
async fn a_developers_completed_result_is_frozen_accepted_and_settled_in_one_commit() -> R {
    // docs/adr/ADR-028.md decisions 2 to 4 through the product's executor (TEST-035-03's first half; the verification
    // tasks its handoffs start are S4's): a fake claude writes, from nothing but its prompt's identity header, a
    // COMPLETED TaskResult to the bridge the header names. The writer's completion freezes the committed project (its
    // specification and test plan where the layout puts them, the environment description HoH writes), accepts the
    // result and settles the attempt in one commit: candidate and task VERIFYING, two handoffs (qa, reviewer) in
    // candidate_ready's transaction, the attempt SETTLED and the write lease let go.
    let world = World::new()?;
    world.claude(None)?;
    let project = world.project();
    let spec =
        "{\"requirements\": [{\"id\": \"REQ-001\", \"acceptance\": [{\"id\": \"AC-001-01\"}]}]}\n";
    git(&project, &["init", "-q"])?;
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("contracts/requirements.json", spec),
        ("tests/check-plan.json", "{\"checks\": {}}\n"),
    ] {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(&project, &["add", "."])?;
    git(&project, &["commit", "-q", "-m", "first"])?;
    world.bind_installed()?;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(spec.as_bytes());
    let sha: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        exec(
            &project,
            &format!(
                "INSERT INTO specs(spec_id, project_id, sha256, revision) \
                 SELECT 'sv', project_id, '{sha}', 7 FROM runs WHERE run_id = '{run_id}';
                 UPDATE runs SET state = 'RUNNING', spec_id = 'sv' WHERE run_id = '{run_id}';
                 INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-001', 'sv', 'r', 1);
                 INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
                   ('AC-001-01', 'REQ-001', 'sv', 'it works', 'unit');
                 INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                   VALUES ('T-RESULT', '{run_id}', 'sv', 'READY', 1, 'developer', 1, \
                   (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);
                 INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('T-RESULT', 'REQ-001', 'sv');"
            ),
        )?;
        let limit = Duration::from_secs(60);
        until(&project, &ended("T-RESULT/attempt-1"), "SETTLED/", limit).await?;
        let state = |sql: &str| query(&project, sql);
        assert_eq!(
            state("SELECT group_concat(state) FROM tasks WHERE task_id = 'T-RESULT'")?,
            "VERIFYING"
        );
        let candidate = state("SELECT candidate_id || '/' || state || '/' || spec_sha256 FROM candidates")?;
        assert!(
            candidate.ends_with(&format!("/VERIFYING/{sha}")),
            "{candidate}"
        );
        let id = candidate.split('/').next().ok_or("no candidate")?;
        assert_eq!(
            state(
                "SELECT group_concat(json_extract(payload_json, '$.recipient_role') || ':' || \
                 json_extract(payload_json, '$.correlation_id'), ',') FROM (SELECT * FROM outbox \
                 WHERE json_extract(payload_json, '$.kind') = 'handoff' ORDER BY json_extract(payload_json, '$.recipient_role'))"
            )?,
            format!("qa:{id},reviewer:{id}")
        );
        assert_eq!(
            state(
                "SELECT count(DISTINCT event_seq) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'handoff'"
            )?,
            "1",
            "both handoffs in candidate_ready's transaction"
        );
        assert_eq!(
            state("SELECT count(*) FROM events WHERE kind = 'candidate_ready'")?,
            "1"
        );
        assert_eq!(
            state(
                "SELECT count(*) FROM leases WHERE kind = 'writer' AND attempt_id = 'T-RESULT/attempt-1' \
                 AND expires_at = (SELECT ended_at FROM attempts WHERE attempt_id = 'T-RESULT/attempt-1')"
            )?,
            "1",
            "the write lease expires at the attempt's end"
        );
        assert!(project.join(".hoh/environment.json").is_file());
        // Its start's HEAD, recorded in its RUNNING commit: the base of its changed files (decisions 4, 14).
        assert_eq!(
            state(
                "SELECT json_extract(payload_json, '$.head') FROM events WHERE kind = 'attempt.workspace_start' \
                 AND json_extract(payload_json, '$.attempt_id') = 'T-RESULT/attempt-1' AND seq < (SELECT seq FROM \
                 events WHERE kind = 'attempt.moved' AND json_extract(payload_json, '$.attempt_id') = 'T-RESULT/attempt-1' \
                 AND json_extract(payload_json, '$.to') = 'SETTLED')"
            )?,
            git(&project, &["rev-parse", "HEAD"])?.trim()
        );
        // With the watermark of the context its prompt was packed from (M2T16R-01).
        assert_eq!(
            state(
                "SELECT typeof(json_extract(payload_json, '$.context_seq')) FROM events \
                 WHERE kind = 'attempt.workspace_start' AND json_extract(payload_json, '$.attempt_id') = 'T-RESULT/attempt-1'"
            )?,
            "integer"
        );
        // The round's verifier tasks are dispatched by the Runtime (decision 10); each attempt's start delivers its
        // handoff in its RUNNING commit: the message ACKED, a handoffs row DELIVERED whose package object is the
        // message's package (decision 9).
        let round = format!("{id}/round/1");
        for role in ["qa", "reviewer"] {
            let attempt = format!("{round}/task/{role}/attempt-1");
            until(
                &project,
                &format!("SELECT count(*) FROM attempts WHERE attempt_id = '{attempt}' AND state != 'PREPARED' AND state != 'STARTING'"),
                "1",
                limit,
            )
            .await?;
            let handoff = format!("{round}/verify/{role}");
            assert_eq!(
                state(&format!("SELECT state FROM outbox WHERE message_id = '{handoff}'"))?,
                "ACKED",
                "{role}"
            );
            assert_eq!(
                state(&format!(
                    "SELECT from_role || ':' || to_role || ':' || state FROM handoffs WHERE handoff_id = '{handoff}'"
                ))?,
                format!("runtime:{role}:DELIVERED")
            );
            let object = state(&format!(
                "SELECT a.path FROM handoffs h JOIN artifacts a ON a.artifact_id = h.package_artifact_id \
                 WHERE h.handoff_id = '{handoff}'"
            ))?;
            let package: Value = serde_json::from_slice(&std::fs::read(project.join(&object))?)?;
            let message: Value = serde_json::from_str(&state(&format!(
                "SELECT payload_json FROM outbox WHERE message_id = '{handoff}'"
            ))?)?;
            assert_eq!(package, message["payload"], "{role}");
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The criteria of the verified project's one requirement (AC-A and AC-B of AC-053-03), each body with its own
/// sentinel.
const AC_A: &str = "SENTINEL-AC-A 解析器拒绝空输入";
const AC_B: &str = "SENTINEL-AC-B 解析器保留输入顺序";

/// A committed project whose specification has one requirement of two criteria and whose test plan has no check, the
/// world's roles bound to its installed fakes; the specification's sha256.
fn verified_project(world: &World) -> R<String> {
    let project = world.project();
    let spec = "{\"requirements\": [{\"id\": \"REQ-001\", \"acceptance\": [{\"id\": \"AC-001-01\"}, \
                {\"id\": \"AC-001-02\"}]}]}\n";
    git(&project, &["init", "-q"])?;
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("contracts/requirements.json", spec),
        ("tests/check-plan.json", "{\"checks\": {}}\n"),
    ] {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(&project, &["add", "."])?;
    git(&project, &["commit", "-q", "-m", "first"])?;
    world.bind_installed()?;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(spec.as_bytes());
    Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
}

/// In the started host: developer task T-RESULT of the project's two criteria, completed by the fake claude; once its
/// candidate's round dispatched QA and QA's first turn ended, the prompt that turn was sent (the fake's stdin), the
/// round's QA handoff package and the candidate.
async fn first_qa_prompt(world: &World, host: &Host, spec_sha: &str) -> R<(String, Value, String)> {
    let project = world.project();
    let run_id = started(host, &project, "s1").await?;
    exec(
        &project,
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) \
             SELECT 'sv', project_id, '{spec_sha}', 7 FROM runs WHERE run_id = '{run_id}';
             UPDATE runs SET state = 'RUNNING', spec_id = 'sv' WHERE run_id = '{run_id}';
             INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-001', 'sv', 'r', 1);
             INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
               ('AC-001-01', 'REQ-001', 'sv', '{AC_A}', 'unit'), ('AC-001-02', 'REQ-001', 'sv', '{AC_B}', 'unit');
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
               VALUES ('T-RESULT', '{run_id}', 'sv', 'READY', 1, 'developer', 1, \
               (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);
             INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('T-RESULT', 'REQ-001', 'sv');"
        ),
    )?;
    let limit = Duration::from_secs(60);
    until(&project, &ended("T-RESULT/attempt-1"), "SETTLED/", limit).await?;
    let candidate = query(&project, "SELECT candidate_id FROM candidates")?;
    let round = format!("{candidate}/round/1");
    let qa = format!("{round}/task/qa/attempt-1");
    let end = Instant::now() + limit;
    while query(&project, &ended(&qa))?.is_empty() {
        assert!(Instant::now() < end, "{qa} did not end");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let d = world.bin().join("claude.d");
    let mut prompt = None;
    for entry in std::fs::read_dir(&d)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("env.") else {
            continue;
        };
        if std::fs::read_to_string(d.join(&name))?
            .lines()
            // The marker is the host instance's, then the attempt's id.
            .any(|l| l.starts_with("HOH_ATTEMPT_ID=") && l.ends_with(&format!("/{qa}")))
        {
            prompt = Some(std::fs::read_to_string(d.join(format!("stdin.{pid}")))?);
        }
    }
    let seen: Vec<String> = std::fs::read_dir(&d)?
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("env."))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .flat_map(|t| {
            t.lines()
                .filter(|l| l.starts_with("HOH_"))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect();
    let prompt = prompt.ok_or(format!(
        "no turn of {qa} found among {seen:?}: {}; {}",
        query(&project, &ended(&qa))?,
        query(
            &project,
            &format!(
                "SELECT group_concat(kind || ' ' || payload_json, ' | ') FROM events WHERE kind LIKE 'attempt.start%' \
                 AND json_extract(payload_json, '$.attempt_id') = '{qa}'"
            )
        )?
    ))?;
    let message: Value = serde_json::from_str(&query(
        &project,
        &format!("SELECT payload_json FROM outbox WHERE message_id = '{round}/verify/qa'"),
    )?)?;
    Ok((prompt, message["payload"].clone(), candidate))
}

#[tokio::test]
async fn test_053_03_qa_is_sent_the_in_scope_criteria_before_the_developers_self_test_record_marked_diagnostic()
-> R {
    // AC-053-03 through the product's executor and dispatch (docs/adr/ADR-028.md decision 14, M2T16D-158): the fake
    // Developer writes its self-test record R, holding ALL TESTS PASSED, under .hoh/self-tests/<its attempt>/ and names
    // it in its COMPLETED TaskResult. QA's handoff names AC-A and AC-B and the candidate C; QA's prompt has both
    // criteria's bodies before R's content, which sits in the evidence context layer under its diagnostic label.
    let world = World::new()?;
    world.claude(None)?;
    let sentinel = "ALL TESTS PASSED SENTINEL-053-03";
    std::fs::write(
        world.bin().join("claude.d/self-test"),
        format!("cargo test: 12 passed\n{sentinel}\n"),
    )?;
    let spec_sha = verified_project(&world)?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let (prompt, package, candidate) = first_qa_prompt(&world, &host, &spec_sha).await?;
        assert_eq!(package["acceptance_ids"], json!(["AC-001-01", "AC-001-02"]));
        assert_eq!(package["candidate"]["id"], candidate.as_str());
        assert_eq!(package["role"], "qa");
        let kept = query(
            &world.project(),
            "SELECT json_extract(j.value, '$.kind') || ':' || json_extract(j.value, '$.name') FROM events e, \
             json_each(json_extract(e.payload_json, '$.items')) j WHERE e.event_id = 'T-RESULT/attempt-1/artifacts/self_tests'",
        )?;
        assert_eq!(
            kept,
            "self_test:.hoh/self-tests/T-RESULT%2Fattempt-1/report.txt"
        );
        let at = |what: &str| prompt.find(what).ok_or(format!("{what} is not in the prompt"));
        let r = at(sentinel)?;
        assert_eq!(prompt.matches(sentinel).count(), 1, "{prompt}");
        assert!(at(AC_A)? < r && at(AC_B)? < r, "{prompt}");
        let evidence = at("### 证据上下文")?;
        assert!(evidence < r, "R is in the evidence context layer: {prompt}");
        let block = prompt[..r].rfind("\n#### 〔").ok_or("R has no block")?;
        assert!(block > evidence, "{prompt}");
        let head = &prompt[block..r];
        assert!(head.contains("〔证据〕self-tests/"), "{head}");
        assert!(head.contains("diagnostic"), "R is labelled diagnostic: {head}");
        keep(
            "m2t16-test-053-03",
            &json!({"candidate_id": candidate, "acceptance_ids": package["acceptance_ids"],
                "offsets": {"ac_a": at(AC_A)?, "ac_b": at(AC_B)?, "r": r, "evidence_layer": evidence},
                "block_head": head, "prompt_sha256": <sha2::Sha256 as sha2::Digest>::digest(prompt.as_bytes())
                    .iter().map(|b| format!("{b:02x}")).collect::<String>()}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn test_039_03_qa_is_sent_the_developers_large_raw_log_by_reference_only() -> R {
    // AC-039-03's QA half through the product's dispatch (docs/adr/ADR-028.md decision 14, CR-016): the Developer's
    // raw log of more than 200 KB holds a unique sentinel at its start, middle and end and is registered in its
    // attempt.artifacts (raw_log). The QA handoff's references are ArtifactRefs (path and 64 hex digits, the file there)
    // and include that log; QA's prompt holds the sentinel 0 times, names the log only by its reference and fits the
    // package's budget.
    let world = World::new()?;
    world.claude(None)?;
    let sentinel = "SENTINEL-039-03-qa-7d21";
    let padding = |c: char| c.to_string().repeat(105_000);
    let frame = json!({"type": "system", "subtype": "task_started",
        "description": format!("{sentinel} {} {sentinel} {} {sentinel}", padding('x'), padding('y'))});
    std::fs::write(world.bin().join("claude.d/big-log"), format!("{frame}\n"))?;
    let spec_sha = verified_project(&world)?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let project = world.project();
        let (prompt, package, _) = first_qa_prompt(&world, &host, &spec_sha).await?;
        let logs = query(
            &project,
            "SELECT group_concat(json_extract(j.value, '$.sha256') || ':' || json_extract(j.value, '$.bytes'), ',') \
             FROM events e, json_each(json_extract(e.payload_json, '$.items')) j \
             WHERE e.event_id = 'T-RESULT/attempt-1/artifacts' AND json_extract(j.value, '$.kind') = 'raw_log'",
        )?;
        let log = logs
            .split(',')
            .filter_map(|l| l.split_once(':'))
            .find(|(sha, bytes)| {
                bytes.parse::<usize>().is_ok_and(|b| b >= 200_000)
                    && hoh_core::objects::get(&project, sha)
                        .is_ok_and(|b| String::from_utf8_lossy(&b).matches(sentinel).count() == 3)
            })
            .map(|(sha, _)| sha.to_owned())
            .ok_or(format!("no raw log of 200 KB holding the sentinel: {logs}"))?;
        let package: hoh_core::protocol::ContextHandoff = hoh_core::protocol::decode(&package)?;
        assert!(!package.references.is_empty());
        for r in &package.references {
            assert!(r.sha256.len() == 64 && r.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{r:?}");
            assert!(project.join(&r.path).is_file(), "{} is not there", r.path);
        }
        assert!(package.references.iter().any(|r| r.sha256 == log), "the log is referenced");
        assert_eq!(prompt.matches(sentinel).count(), 0);
        assert!(prompt.contains(&log), "the log by its reference");
        let tokens = hoh_core::context::estimate(&prompt);
        assert!(tokens <= package.budget_tokens, "{tokens} > {}", package.budget_tokens);
        keep(
            "m2t16-test-039-03-qa",
            &json!({"log_sha256": log, "references": package.references.iter().map(|r| json!({"path": r.path,
                "sha256": r.sha256})).collect::<Vec<_>>(), "tokens": tokens, "budget_tokens": package.budget_tokens,
                "sentinel_count": 0}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
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
        let run_id = started(&host, &project, "s1").await?;
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
        let run_id = started(&host, &project, "s1").await?;
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
        let run_id = started(&host, &project, "s1").await?;
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
        // The task's turns on exec, by the attempt each was started for: the Commander's turns are codex's too in this
        // world, on exec as well once codex is degraded, and the specification the run adopted at its start wakes it
        // (docs/adr/ADR-031.md decision 1); only the developer's are counted.
        let calls = std::fs::read_to_string(world.bin().join("codex.d/calls"))?;
        let exec_turns: Vec<&str> = calls
            .lines()
            .filter(|l| {
                let mut words = l.split(' ').skip(1);
                words.next().is_some_and(|a| a.contains("/T-DEGRADE/"))
                    && words.next() == Some("exec")
                    && words.next() != Some("--help")
            })
            .collect();
        assert_eq!(
            exec_turns.len(),
            1,
            "the second attempt ran on exec: {exec_turns:?}"
        );
        assert!(
            exec_turns[0].contains("/T-DEGRADE/attempt-2 "),
            "{exec_turns:?}"
        );
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
        let run_id = started(&host, &project, "s1").await?;
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
        started(&host, &project, "s1").await?;
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
        // Held only once it waits in the probe's loop for its command (blocked in the poll of its outputs, after the
        // spawn returned and the command was put in its slot): what this test claims is that the stop kills a command
        // in its slot however late the thread's own looks come. Its pid files exist as soon as the command runs, while
        // the thread that spawned it may not have returned from the spawn yet (under load it was held in clone3): a
        // thread held there is ADR-022 decision 10 v6's residual (a), the pid known to its starter alone, which this
        // test does not claim.
        let waiting = format!("{} ", nix::libc::SYS_ppoll);
        while !std::fs::read_to_string(format!("/proc/{host_pid}/task/{reader}/syscall"))
            .is_ok_and(|now| now.starts_with(&waiting))
        {
            if Instant::now() > end {
                return Err("the schema reader never waited for its command".into());
            }
            std::thread::sleep(Duration::from_millis(5));
        }
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
        let run_id = started(&host, &project, "s1").await?;
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
        // At the run's revision now, again at the revision the host names while it answers STALE_REVISION: its runner
        // moves the run on meanwhile (common::fresh).
        let revision = query(
            &project,
            &format!("SELECT revision FROM runs WHERE run_id = '{run_id}'"),
        )?;
        let (status, accepted) = common::fresh(json!(revision.parse::<i64>()?), |revision| {
            let body = json!({
                "schema_version": "1.0", "command_id": "c-cancel", "idempotency_key": "cancel",
                "expected_revision": revision, "kind": "CancelRun", "run_id": run_id,
                "payload": {"reason": "stop"},
            });
            let host = &host;
            async move { exchange(host, "POST", "/v1/commands", Some(&body)).await }
        })
        .await?;
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
        // The stand-in confirms that the native cancel came first and SIGTERM after it.
        assert_eq!(
            steps.iter().map(|s| s.0).collect::<Vec<_>>(),
            ["INT", "TERM"],
            "{seen}"
        );
        // The grace is measured between the host's own records of the two steps (each `cancel.<step>` event's at_ms,
        // when the pass sent it), not where the stand-in received them: its receipt lags the host's record by the
        // signal's delivery and its scheduling (ADR-022 decision 10, the measuring point; 3 of 60 runs under load saw
        // 872-894 ms at the stand-in while the host's records were 1002-1003 ms apart).
        let at = |step: &str| -> R<u64> {
            Ok(query(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.at_ms') FROM events WHERE kind = 'cancel.{step}' \
                     AND json_extract(payload_json, '$.attempt_id') = 'T-CANCEL/attempt-1'"
                ),
            )?
            .parse()?)
        };
        let (native, term) = (at("native_cancel")?, at("sigterm")?);
        let gap = Duration::from_millis(term.checked_sub(native).ok_or("SIGTERM recorded before the native cancel")?);
        assert!(
            gap >= Duration::from_millis(900) && gap < Duration::from_secs(8),
            "SIGTERM {gap:?} after the native cancel, by the host's records"
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
        let run_id = started(&host, &project, "s1").await?;
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
        // The new Commander's first turn, by its attempt: the old Commander's turns are codex's, and the
        // specification's adoption may wake it before the change (docs/adr/ADR-031.md decision 1); a later wake may
        // give pi another turn.
        let sent = &first_commander_prompt(&project, &world.bin().join("pi.d"), &run_id, 2, limit).await?;
        let (contract, handover, wake) = (
            sent.find("Commander").ok_or("no role contract")?,
            sent.find("# 交接包").ok_or("no handover package")?,
            sent.find("# 本次唤醒").ok_or("no wake")?,
        );
        assert!(contract < handover && handover < wake, "{sent}");
        // The wake's own evidence too, not only its heading (docs/adr/ADR-026.md decision 3).
        let evidence = sent.find("events/").ok_or("no wake evidence")?;
        assert!(handover < evidence, "{sent}");
        assert!(sent.contains(&format!("{:064}", 3)), "the package names revision 3's digest: {sent}");
        assert_eq!(sent.matches(sentinel).count(), 0, "{sent}");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The one prompt the fake pi under `pi_d` was sent for the first turn of run `run_id`'s Commander at binding epoch
/// `epoch`: that epoch's first Commander attempt by order, whatever its state (a first turn that failed is not skipped
/// for a later one, M2T13R-600), once it ended, which must be SETTLED; each waited for at most `limit`. A later
/// attempt of the epoch (another wake's) is a test's to select by its identity.
async fn first_commander_prompt(
    project: &Path,
    pi_d: &Path,
    run_id: &str,
    epoch: i64,
    limit: Duration,
) -> R<String> {
    let first = format!(
        "SELECT coalesce((SELECT a.attempt_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
         WHERE t.run_id = '{run_id}' AND t.role = 'commander' AND a.role_epoch = {epoch} \
         ORDER BY a.rowid LIMIT 1), '')"
    );
    let end = Instant::now() + limit;
    let (attempt, ended_as) = loop {
        let attempt = query(project, &first)?;
        if !attempt.is_empty() {
            let ended_as = query(project, &ended(&attempt))?;
            if !ended_as.is_empty() {
                break (attempt, ended_as);
            }
        }
        if Instant::now() > end {
            return Err(format!("the new Commander's first turn did not end: {attempt:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    if ended_as != "SETTLED/" {
        return Err(format!("the new Commander's first turn {attempt} ended {ended_as}").into());
    }
    let prompts: Vec<String> = received_by_attempt(pi_d)?
        .into_iter()
        .filter(|(a, command)| *a == attempt && command["type"] == "prompt")
        .filter_map(|(_, command)| command["message"].as_str().map(str::to_owned))
        .collect();
    let [sent] = prompts.as_slice() else {
        return Err(format!(
            "the pi Commander's first turn {attempt} was sent {} prompts",
            prompts.len()
        )
        .into());
    };
    Ok(sent.clone())
}

#[tokio::test]
async fn the_new_commanders_first_turn_is_its_first_attempt_whatever_its_state() -> R {
    // M2T13R-600: the first turn test_019_02 checks is the epoch's first Commander attempt by order, not the first to
    // settle. Here (the reviewer's case, rows written directly) that attempt was sent a prompt with no handover package
    // and FAILED, and a later wake's attempt was sent the whole package and SETTLED: the first turn is the failed one,
    // so the selection refuses it instead of checking the later one.
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("project");
    std::fs::create_dir_all(project.join(".hoh"))?;
    exec(
        &project,
        "CREATE TABLE tasks(task_id TEXT PRIMARY KEY, run_id TEXT, role TEXT);
         CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY, task_id TEXT, role_epoch INTEGER, state TEXT,
           failure_reason TEXT);
         INSERT INTO tasks VALUES ('R/trigger/5/turn/0', 'R', 'commander'), ('R/trigger/5/turn/1', 'R', 'commander');
         INSERT INTO attempts VALUES ('R/trigger/5/turn/0/attempt-1', 'R/trigger/5/turn/0', 2, 'FAILED', 'AGENT_ERROR'),
           ('R/trigger/5/turn/1/attempt-1', 'R/trigger/5/turn/1', 2, 'SETTLED', NULL);",
    )?;
    let pi_d = dir.path().join("pi.d");
    for (pid, attempt, message) in [
        (
            11,
            "R/trigger/5/turn/0/attempt-1",
            "Commander\n# 本次唤醒\nevents/first: missing handover".to_owned(),
        ),
        (
            12,
            "R/trigger/5/turn/1/attempt-1",
            format!("Commander\n# 交接包\n{:064}\n# 本次唤醒\nevents/second", 3),
        ),
    ] {
        std::fs::create_dir_all(pi_d.join(format!("log.{pid}")))?;
        std::fs::write(pi_d.join(format!("marker.{pid}")), format!("P/{attempt}\n"))?;
        std::fs::write(
            pi_d.join(format!("log.{pid}/received.jsonl")),
            format!("{}\n", json!({"type": "prompt", "message": message})),
        )?;
    }
    match first_commander_prompt(&project, &pi_d, "R", 2, Duration::from_secs(1)).await {
        Err(e) => assert!(
            e.to_string().contains("R/trigger/5/turn/0/attempt-1"),
            "{e}"
        ),
        Ok(sent) => return Err(format!("the later turn was taken for the first: {sent}").into()),
    }
    Ok(())
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
        let run_id = started(&host, &project, "s1").await?;
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
        // The task's turns, by its attempts: the Commander's turns are codex's too in this world (the specification's
        // adoption and the stall detector wake it: docs/adr/ADR-031.md decision 1, correction C).
        let sent = codex_prompts_of(&world, |a| a.starts_with("T-dev/"))?;
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(
            codex_prompts_of(&world, |a| a == "T-dev/attempt-1")?,
            sent,
            "the first attempt's"
        );
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
        let run_id = started(&host, &project, "s1").await?;
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
        // The task's turns, by its attempts: the Commander's turns are codex's too in this world (the specification's
        // adoption and the stall detector wake it: docs/adr/ADR-031.md decision 1, correction C).
        let sent = codex_prompts_of(&world, |a| a.starts_with("T-dev/"))?;
        assert_eq!(sent.len(), 1, "only the third attempt's turn was sent: {sent:?}");
        assert_eq!(
            codex_prompts_of(&world, |a| a == "T-dev/attempt-3")?,
            sent,
            "the third attempt's"
        );
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
        let run_id = started(&host, &project, "s1").await?;
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
        let run_id = started(&host, &project, "s1").await?;
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
        // The task's turns, by its attempts: the Commander's turns are codex's too in this world (the specification's
        // adoption and the stall detector wake it: docs/adr/ADR-031.md decision 1, correction C).
        let task = |a: &str| a.starts_with("T-INCOMPLETE/");
        let sent = codex_prompts_of(&world, |a| a == "T-INCOMPLETE/attempt-2")?;
        assert_eq!(sent.len(), 1, "only the retry's turn was sent: {sent:?}");
        assert_eq!(
            codex_prompts_of(&world, task)?.len(),
            1,
            "only the retry's turn was sent"
        );
        assert!(sent[0].contains("```json"), "the first turn codex got has no package: {}", sent[0]);
        let resumes = received_by_attempt(&world.bin().join("codex.d"))?
            .into_iter()
            .filter(|(a, m)| task(a) && m["method"] == "thread/resume")
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
        let run_id = started(&host, &project, "s1").await?;
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
        let run_id = started(&host, &project, "s1").await?;
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

/// What a fake agent's processes under `dir` (`codex.d`, `pi.d`) were sent, by the attempt each ran for: a process's
/// `marker.<pid>` (the attempt's marker, `<project_id>/<attempt_id>`) and the messages of its `log.<pid>/received.jsonl`.
/// A test of an attempt's calls selects that attempt's, whatever else the agent ran meanwhile, such as the Commander's
/// turn its run's adoption of a specification wakes (docs/adr/ADR-031.md decision 1, 实施记录's adoption-wake rule).
fn received_by_attempt(dir: &Path) -> R<Vec<(String, Value)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("log.") else {
            continue;
        };
        let attempt = std::fs::read_to_string(dir.join(format!("marker.{pid}")))
            .ok()
            .and_then(|m| m.trim().split_once('/').map(|(_, a)| a.to_owned()))
            .unwrap_or_default();
        if let Ok(text) = std::fs::read_to_string(entry.path().join("received.jsonl")) {
            for line in text.lines() {
                if let Ok(message) = serde_json::from_str::<Value>(line) {
                    out.push((attempt.clone(), message));
                }
            }
        }
    }
    Ok(out)
}

/// The prompts codex was sent (each turn/start's first input) for the attempts `of` selects.
fn codex_prompts_of(world: &World, of: impl Fn(&str) -> bool) -> R<Vec<String>> {
    Ok(received_by_attempt(&world.bin().join("codex.d"))?
        .into_iter()
        .filter(|(attempt, m)| of(attempt) && m["method"] == "turn/start")
        .filter_map(|(_, m)| m["params"]["input"][0]["text"].as_str().map(str::to_owned))
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
         INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{task}', 'REQ-X', 's1');
         INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256,
           test_plan_sha256, source_path, state, created_at) VALUES
           ('CAND-039', '{run_id}', printf('%040d', 39), printf('%040d', 40), printf('%064d', 1), printf('%064d', 4),
            printf('%064d', 5), '/repo', 'FROZEN', '2026-11-10T00:00:00Z');"
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
        let run_id = started(&host, &project, "s1").await?;
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
        // Its raw logs were kept as objects and registered with it as their producer, with its work's OBSERVED
        // (docs/adr/ADR-026.md decision 10).
        let produced: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'attempt.artifacts' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-a/attempt-4'",
        )?)?;
        let items = produced["items"].as_array().ok_or("no items")?;
        assert!(items.iter().any(|i| i["kind"] == "raw_log"), "{produced}");
        for item in items {
            let path = item["path"].as_str().ok_or("no path")?;
            assert!(project.join(path).is_file(), "{path}");
            let registered = query(
                &project,
                &format!("SELECT count(*) FROM artifacts WHERE sha256 = '{}' AND path = '{path}'", item["sha256"].as_str().unwrap_or_default()),
            )?;
            assert_eq!(registered, "1", "{path}");
        }
        // What codex was sent for the attempt is what was recorded.
        let sent = codex_prompts_of(&world, |a| a == "T-a/attempt-4")?;
        assert_eq!(sent.len(), 1, "{sent:?}");
        let digest = |bytes: &[u8]| -> String {
            <sha2::Sha256 as sha2::Digest>::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
        };
        assert_eq!(digest(sent[0].as_bytes()), sha);
        assert!(sent[0].contains("份额是硬上限 {{GOAL}}"), "an AC naming a placeholder is text");
        assert!(sent[0].contains("CAND-039") && sent[0].contains(&format!("{:040}", 39)), "the candidate");
        // Recorded in the commit that marked it RUNNING, before its gate opened (decision 7, M2T09R-56): with its
        // tracking, before its work's OBSERVED.
        let seq = |sql: &str| -> R<i64> { Ok(query(&project, sql)?.parse()?) };
        let packed_at = seq(
            "SELECT seq FROM events WHERE kind = 'context.packed' AND json_extract(payload_json, '$.attempt_id') = 'T-a/attempt-4'",
        )?;
        let tracking_at = seq("SELECT seq FROM events WHERE event_id = 'T-a/attempt-4/tracking'")?;
        let started_at = seq(
            "SELECT seq FROM events WHERE kind = 'effect.started' AND json_extract(payload_json, '$.attempt_id') = 'T-a/attempt-4'",
        )?;
        // One commit writes them in this order and nothing else between (M2T09R-153): the record, the work effect
        // STARTED, the tracking; then, later, the OBSERVED.
        assert_eq!(
            (started_at - packed_at, tracking_at - started_at),
            (1, 1),
            "{packed_at} {started_at} {tracking_at}"
        );
        let observed_at = seq(
            "SELECT seq FROM events WHERE kind = 'effect.observed' AND json_extract(payload_json, '$.attempt_id') = 'T-a/attempt-4'",
        )?;
        assert!(tracking_at < observed_at, "{tracking_at} {observed_at}");
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
        let run_id = started(&host, &project, "s1").await?;
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
        assert!(codex_prompts_of(&world, |a| a.starts_with("T-b/"))?.is_empty());
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn test_039_03_a_new_commanders_prompt_carries_references_and_none_of_the_runs_large_logs()
-> R {
    // REQ-039 behaviour 5 and failure mode 3 (docs/adr/ADR-026.md decision 10), the Commander half of AC-039-03 (its QA
    // half: test_039_03_qa_is_sent_the_developers_large_raw_log_by_reference_only): a Developer attempt of the run
    // produced a raw log of more than 200 KB holding a unique sentinel, registered in its attempt.artifacts (raw_log);
    // so did the old Commander's session, the log a Commander's handover references (its predecessor's products,
    // decision 10; docs/adr/ADR-028.md, implementation correction 3); an adapter frame and a TaskResult inside a
    // candidate_ready hold the sentinel too. The new Commander's package references the old Commander's log (64 hex
    // digits, the file there); its first prompt holds the sentinel 0 times and fits the package's budget.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("m-test")?;
    world.pi()?;
    let host = start_when_free(world.config()).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        let commander = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'commander'"
        );
        let old_session = format!("{run_id}/session/commander/1");
        until(&project, &commander, &format!("codex/1/{old_session}"), limit).await?;
        let sentinel = "SENTINEL-039-03-c4e1";
        // At its start, in its middle and at its end (M2T09R-53): a head or a tail of it in the prompt would show.
        let log = format!("{sentinel}\n{}\n{sentinel}\n{}\n{sentinel}\n", "x".repeat(105_000), "y".repeat(105_000));
        let sha = hoh_core::objects::put(&project, log.as_bytes())?;
        let item = json!({"artifact_id": format!("{sha}:p"), "sha256": sha,
            "path": hoh_core::objects::relative_path(&sha), "bytes": log.len(), "mime": "text/plain",
            "kind": "raw_log", "name": "stdout.jsonl"});
        // The Developer's own log: other bytes, the same sentinel.
        let developer_log = format!("{sentinel}\n{}\n{sentinel}\n", "d".repeat(210_000));
        let developer_sha = hoh_core::objects::put(&project, developer_log.as_bytes())?;
        let developer_item = json!({"artifact_id": format!("{developer_sha}:p"), "sha256": developer_sha,
            "path": hoh_core::objects::relative_path(&developer_sha), "bytes": developer_log.len(),
            "mime": "text/plain", "kind": "raw_log", "name": "stdout.jsonl"});
        exec(
            &project,
            &format!(
                "INSERT INTO specs(spec_id, project_id, sha256, revision) \
                   SELECT 's3', project_id, printf('%064d', 3), 3 FROM runs WHERE run_id = '{run_id}';
                 UPDATE runs SET spec_id = 's3' WHERE run_id = '{run_id}';
                 INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq)
                   VALUES ('T-old', '{run_id}', 's3', 'CANCELLED', 1, 'commander', 0, 1);
                 INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
                   VALUES ('T-old/attempt-1', 'T-old', '{old_session}', 'SETTLED', 1, 1, 1);
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'T-old/attempt-1/artifacts', '{run_id}', 'attempt.artifacts',
                   json_object('attempt_id', 'T-old/attempt-1', 'items', json_array(json('{item}'))),
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';
                 INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq)
                   VALUES ('T-dev', '{run_id}', 's3', 'IMPLEMENTED', 1, 'developer', 1, 2);
                 INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision)
                   VALUES ('T-dev/attempt-1', 'T-dev', 'SETTLED', 1, 1, 1);
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'T-dev/attempt-1/artifacts', '{run_id}', 'attempt.artifacts',
                   json_object('attempt_id', 'T-dev/attempt-1', 'items', json_array(json('{developer_item}'))),
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'frame-039-03', '{run_id}', 'adapter.unknown_event',
                   '{{\"frame\": \"{sentinel}\"}}', '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'diagnostic-039-03', '{run_id}', 'adapter.diagnostic',
                   '{{\"frame\": \"{sentinel}\"}}', '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';
                 INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
                   SELECT project_id, 'ready-039-03', '{run_id}', 'candidate_ready',
                   '{{\"task_result_json\": \"{sentinel}\"}}', '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';"
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
        let input: Value = serde_json::from_str(&std::fs::read_to_string(&kept)?)?;
        let package: hoh_core::protocol::ContextHandoff = hoh_core::protocol::decode(&input["package"])?;
        assert!(!package.references.is_empty(), "the package references nothing");
        for r in &package.references {
            assert!(r.sha256.len() == 64 && r.sha256.bytes().all(|b| b.is_ascii_hexdigit()), "{r:?}");
            assert!(project.join(&r.path).is_file(), "{} is not there", r.path);
        }
        assert!(package.references.iter().any(|r| r.sha256 == sha), "the old log is referenced");
        exec(
            &project,
            &format!(
                "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
                   SELECT project_id, 'nd-039-03', '{run_id}', 'need_decision', \
                   '{{\"fingerprint\": \"fp-039-03\", \"task_result_json\": \"{sentinel}\", \"detail\": {{\"frame\": \"{sentinel}\"}}}}', \
                   '2026-11-10T00:00:00Z' FROM runs WHERE run_id = '{run_id}';"
            ),
        )?;
        until(
            &project,
            "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.role = 'commander' \
             AND a.state = 'SETTLED' AND a.session_id LIKE '%/commander/2'",
            "1",
            limit,
        )
        .await?;
        let mut prompts = Vec::new();
        for entry in std::fs::read_dir(world.bin().join("pi.d"))? {
            if let Ok(text) = std::fs::read_to_string(entry?.path().join("received.jsonl")) {
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
        assert_eq!(sent.matches(sentinel).count(), 0);
        assert!(sent.contains(&sha), "the reference is in the package it was sent");
        // The turn's own identity, which its answers name (M2T09R-03).
        let turn = query(
            &project,
            "SELECT a.attempt_id || '|' || a.task_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'commander' AND a.session_id LIKE '%/commander/2'",
        )?;
        let (attempt, task) = turn.split_once('|').ok_or("no commander turn")?;
        assert!(sent.contains(&format!("attempt_id：{attempt}")) && sent.contains(&format!("task_id：{task}")), "{sent}");
        let tokens = hoh_core::context::estimate(sent);
        assert!(tokens <= package.budget_tokens, "{tokens} > {}", package.budget_tokens);
        // The package first, then the wake's evidence (docs/adr/ADR-026.md decision 3).
        let (handover, wake_event) = (
            sent.find("交接包（本会话的首个输入）").ok_or("no package")?,
            sent.find("events/").ok_or("no wake event")?,
        );
        assert!(handover < wake_event, "{sent}");
        keep(
            "m2t16-test-039-03-commander",
            &json!({"package": input["package"], "prompt_sha256": <sha2::Sha256 as sha2::Digest>::digest(sent.as_bytes()).iter().map(|b| format!("{b:02x}")).collect::<String>(),
                "tokens": tokens, "sentinel_count": 0}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn a_turns_logs_that_cannot_be_kept_as_objects_are_registered_where_they_are() -> R {
    // M2T09R-01, -151 (docs/adr/ADR-026.md decision 10): the objects' staging place taken by a file, so no raw log can
    // be kept as an object; the turn's logs are registered in place under their own sha256, with their producer, in its
    // work's OBSERVED.
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh/objects"))?;
    std::fs::write(project.join(".hoh/objects/staging"), b"not a directory")?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.developer]\nagent = \"codex\"\n",
    )?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        ready(&project, &run_id, &[("T-l", "developer")])?;
        until(
            &project,
            &ended("T-l/attempt-1"),
            "SETTLED/",
            Duration::from_secs(60),
        )
        .await?;
        let produced: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'attempt.artifacts' \
             AND json_extract(payload_json, '$.attempt_id') = 'T-l/attempt-1'",
        )?)?;
        let items = produced["items"].as_array().ok_or("no items")?;
        assert!(!items.is_empty(), "{produced}");
        for item in items {
            assert_eq!(item["kind"], "raw_log_in_place", "{item}");
            let path = item["path"].as_str().ok_or("no path")?;
            assert!(path.starts_with(".hoh/attempts/"), "{path}");
            let bytes = std::fs::read(project.join(path))?;
            let digest: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            assert_eq!(item["sha256"].as_str(), Some(digest.as_str()), "{path}");
        }
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// Holds each compat suite whose run ended before its publication until `<dir>/release` exists, making `<dir>/held`
/// as it does (docs/adr/ADR-030.md decision 10: the suite stays open meanwhile; the test's hook, CR-006).
#[derive(Debug)]
struct HoldSuite(PathBuf);

impl hoh_app::host::HostHooks for HoldSuite {
    fn suite_ran(&self, _agent: &str) {
        let _ = std::fs::write(self.0.join("held"), "");
        let end = Instant::now() + Duration::from_secs(120);
        // Its directory gone, the test that held it has ended.
        while self.0.exists() && !self.0.join("release").exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = std::fs::write(self.0.join("left"), "");
    }
}

/// Wait until `file` exists, for at most `limit`.
async fn exists(file: &Path, limit: Duration) -> R {
    let end = Instant::now() + limit;
    while !file.exists() {
        if Instant::now() > end {
            return Err(format!("{} never appeared", file.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

/// The project's compat suite events in order, one line each: `seq kind payload`.
fn suite_events(project: &Path) -> R<Vec<(i64, String, Value)>> {
    let rows = query(
        project,
        "SELECT json_group_array(json_array(seq, kind, json(payload_json))) FROM \
         (SELECT * FROM events WHERE kind LIKE 'compat.suite_%' ORDER BY seq)",
    )?;
    let rows: Vec<(i64, String, Value)> = serde_json::from_str(&rows)?;
    Ok(rows)
}

fn kinds(events: &[(i64, String, Value)]) -> Vec<&str> {
    events.iter().map(|(_, kind, _)| kind.as_str()).collect()
}

#[tokio::test]
async fn a_writer_waits_for_a_running_suite() -> R {
    // docs/adr/ADR-030.md decision 10 (M2T15D-04, K7): the host runs the compat suite of pi, which nothing registered,
    // once it looked at it; while the suite is open (held here after its run, before its publication) a writer task
    // due stays READY with no attempt and the writer slot is granted to no one, while a reader task is dispatched;
    // once the suite published, the next round grants the writer. The tasks are the developer's, bound to claude,
    // registered before the host starts: a session is granted only for a verified agent (decisions 4 and 9), so a
    // reader runs while a suite is open only on an agent verified already.
    let world = World::new()?;
    world.claude(None)?;
    world.pi()?;
    // qa and researcher stay pi's, so the registry wants pi and its suite is admitted.
    world.bind_installed()?;
    let project = world.project();
    let path = format!(
        "{}:{}",
        world.bin().display(),
        world.dir.path().join("tools").display()
    );
    hoh_app::compat::register(
        &project,
        hoh_agents::compat::Agent::Claude,
        std::ffi::OsStr::new(&path),
    )?;
    let hold = world.dir.path().join("hold");
    std::fs::create_dir_all(&hold)?;
    let mut config = world.config();
    config.hooks = std::sync::Arc::new(HoldSuite(hold.clone()));
    let host = start_when_free(config).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT group_concat(role, ',') FROM (SELECT role FROM role_bindings WHERE run_id = '{run_id}' \
                     AND agent = 'pi' ORDER BY role)"
                )
            )?,
            "qa,researcher",
            "pi is bound, so wanted"
        );
        ready(&project, &run_id, &[])?;
        exists(&hold.join("held"), Duration::from_secs(60)).await?;
        let events = suite_events(&project)?;
        assert_eq!(
            kinds(&events),
            ["compat.suite_opened", "compat.suite_started"],
            "{events:?}"
        );
        assert_eq!(events[0].2["agent"], "pi");
        let suite = events[0].2["suite_id"].clone();
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T-W', '{run_id}', 's1', 'READY', 1, 'developer', 1, 1, 1);"
            ),
        )?;
        ready(&project, &run_id, &[("T-R", "developer")])?;
        until(
            &project,
            "SELECT count(*) FROM attempts WHERE task_id = 'T-R'",
            "1",
            Duration::from_secs(30),
        )
        .await?;
        // Rounds go on meanwhile (a tick is 200 ms): the writer is still not granted.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            query(
                &project,
                "SELECT state || '/' || (SELECT count(*) FROM attempts WHERE task_id = 'T-W') FROM tasks \
                 WHERE task_id = 'T-W'"
            )?,
            "READY/0"
        );
        assert_eq!(
            query(&project, "SELECT count(*) FROM leases WHERE kind = 'writer'")?,
            "0"
        );
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(project.join(".hoh/compat-manifest.json"))?)?;
        assert!(
            manifest["agents"].get("pi").is_none(),
            "pi's suite is not published yet: {manifest}"
        );
        std::fs::write(hold.join("release"), "")?;
        until(
            &project,
            "SELECT count(*) FROM attempts WHERE task_id = 'T-W'",
            "1",
            Duration::from_secs(30),
        )
        .await?;
        let events = suite_events(&project)?;
        assert_eq!(
            kinds(&events),
            [
                "compat.suite_opened",
                "compat.suite_started",
                "compat.suite_ended",
                "compat.suite_completed"
            ],
            "{events:?}"
        );
        assert_eq!(events[2].2, json!({"suite_id": suite, "status": "PASS"}));
        let granted: i64 = query(
            &project,
            "SELECT min(event_seq) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
             AND json_extract(payload_json, '$.task_id') = 'T-W'",
        )?
        .parse()?;
        assert!(
            events[2].0 < granted,
            "the writer was granted (seq {granted}) before the suite ended ({})",
            events[2].0
        );
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(project.join(".hoh/compat-manifest.json"))?)?;
        assert_eq!(manifest["agents"]["pi"]["suite"]["status"], "PASS");
        assert_eq!(
            manifest["agents"]["pi"]["suite"]["fixture_set_sha256"],
            json!(hoh_agents::compat::SUITE_SET_SHA256.as_str())
        );
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(hold.join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome?;
    exists(&hold.join("left"), Duration::from_secs(10)).await
}

#[tokio::test]
async fn a_stopped_host_abandons_its_suite() -> R {
    // docs/adr/ADR-030.md decision 10 ("有界、停止与崩溃"): the host stops while its suite is open (held after its run,
    // before its publication): the suite is stopped and ended `abandoned`, nothing is published.
    let world = World::new()?;
    world.claude(None)?;
    world.bind_installed()?;
    let project = world.project();
    let hold = world.dir.path().join("hold");
    std::fs::create_dir_all(&hold)?;
    let mut config = world.config();
    config.hooks = std::sync::Arc::new(HoldSuite(hold.clone()));
    // The suite's step, held here past the stop, is counted (docs/adr/ADR-031.md, implementation review round 4): the
    // stop waits for it its bound at most (2 s here) and then is not clean.
    config.stop_bound = Duration::from_secs(2);
    let host = start_when_free(config).await?;
    let held = exists(&hold.join("held"), Duration::from_secs(60)).await;
    let clean = host.shutdown().await;
    std::fs::write(hold.join("release"), "")?;
    exists(&hold.join("left"), Duration::from_secs(10)).await?;
    world.stop_fakes()?;
    held?;
    let events = suite_events(&project)?;
    assert_eq!(
        kinds(&events),
        [
            "compat.suite_opened",
            "compat.suite_started",
            "compat.suite_ended"
        ],
        "{events:?}"
    );
    assert_eq!(
        events[2].2,
        json!({"suite_id": events[0].2["suite_id"], "status": "abandoned"})
    );
    assert!(!project.join(".hoh/compat-manifest.json").exists());
    assert!(!clean, "a stop past its bound said it was clean");
    Ok(())
}

#[tokio::test]
async fn a_hosts_start_holds_an_unreadable_manifest_as_empty_and_says_so() -> R {
    // docs/adr/ADR-030.md decision 10 ("清单只由 host 写，放在注册表里"): the host reads the compat manifest as it starts;
    // one that is not JSON is held as empty, with a `compat.manifest_unreadable` event of its epoch, and the temporary
    // file a publication that crashed left beside it is deleted; claude's suite then publishes a whole manifest.
    let world = World::new()?;
    world.claude(None)?;
    world.bind_installed()?;
    let project = world.project();
    let hoh = project.join(".hoh");
    std::fs::write(hoh.join("compat-manifest.json"), "{")?;
    let leftover = hoh.join(".compat-manifest.json.1.1.tmp");
    std::fs::write(&leftover, "{")?;
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        until(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'compat.suite_completed'",
            "1",
            Duration::from_secs(60),
        )
        .await?;
        let said: Value = serde_json::from_str(&query(
            &project,
            "SELECT payload_json FROM events WHERE kind = 'compat.manifest_unreadable'",
        )?)?;
        assert_eq!(said["host_epoch"], json!(host.host_epoch), "{said}");
        assert!(
            said["detail"]
                .as_str()
                .is_some_and(|d| d.contains("compat-manifest.json")),
            "{said}"
        );
        assert!(!leftover.exists(), "a leftover temporary manifest was kept");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(hoh.join("compat-manifest.json"))?)?;
        assert_eq!(manifest["agents"]["claude"]["suite"]["status"], "PASS");
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The first replacement of the compat manifest that reaches its rename fails to sync the manifest's directory: the
/// test's fault hook (CR-006; the product's does nothing).
#[derive(Debug, Default)]
struct UnsyncedOnce(std::sync::atomic::AtomicBool);

impl hoh_core::persistence::FaultHooks for UnsyncedOnce {
    fn manifest_at(&self, step: hoh_core::compat_manifest::Step) -> std::io::Result<()> {
        if step == hoh_core::compat_manifest::Step::Renamed
            && !self.0.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            Err(std::io::Error::other("injected"))
        } else {
            Ok(())
        }
    }
}

#[tokio::test]
async fn a_manifest_renamed_but_not_synced_is_the_base_of_the_next_publication() -> R {
    // M2T15R-04, docs/adr/ADR-030.md decision 10 "发布": the first suite's publication renames the new manifest into
    // place and only the sync of its directory fails. The host holds what was renamed, so the second agent's
    // publication, built on the manifest the host holds, keeps the first agent's entry instead of writing the second's
    // over the manifest before it.
    let world = World::new()?;
    world.claude(None)?;
    world.pi()?;
    world.bind_installed()?;
    let project = world.project();
    let mut config = world.config();
    config.store.faults = std::sync::Arc::new(UnsyncedOnce::default());
    let host = start_when_free(config).await?;
    let outcome = async {
        until(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'compat.suite_ended'",
            "2",
            Duration::from_secs(90),
        )
        .await?;
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(project.join(".hoh/compat-manifest.json"))?)?;
        let events = suite_events(&project)?;
        let first = events[0].2["agent"].as_str().unwrap_or_default();
        assert!(
            events[2].2["error"]
                .as_str()
                .is_some_and(|e| e.contains("not synced")),
            "{events:?}"
        );
        assert_eq!(
            manifest["agents"][first]["suite"]["status"], "PASS",
            "{first}'s entry was overwritten: {manifest}"
        );
        assert_eq!(
            kinds(&events),
            [
                "compat.suite_opened",
                "compat.suite_started",
                "compat.suite_ended",
                "compat.suite_completed",
                "compat.suite_opened",
                "compat.suite_started",
                "compat.suite_ended",
                "compat.suite_completed"
            ],
            "{events:?}"
        );
        assert_eq!(events[2].2["status"], "PASS", "{events:?}");
        let second = events[4].2["agent"].as_str().unwrap_or_default();
        assert_eq!(
            [first, second]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            ["claude", "pi"].into_iter().collect(),
            "{events:?}"
        );
        assert_eq!(
            events[6].2,
            json!({"suite_id": events[4].2["suite_id"], "status": "PASS"})
        );
        assert_eq!(
            manifest["agents"][second]["suite"]["status"], "PASS",
            "{manifest}"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// An action armed at a runner point: called each time the point is reached until it says it is done.
type Action = Box<dyn FnMut() -> bool + Send>;

/// The runner's points with actions a test arms (CR-006 hooks), and the compat suites held as [`HoldSuite`] holds
/// them when `hold` is set.
#[derive(Default)]
struct Points {
    armed: std::sync::Mutex<Vec<(hoh_app::host::RunnerPoint, Action)>>,
    hold: Option<PathBuf>,
}

impl std::fmt::Debug for Points {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Points")
    }
}

impl Points {
    /// Call `action` at each `point` from now on, until it returns true.
    fn arm(
        &self,
        point: hoh_app::host::RunnerPoint,
        action: impl FnMut() -> bool + Send + 'static,
    ) {
        if let Ok(mut armed) = self.armed.lock() {
            armed.push((point, Box::new(action)));
        }
    }
}

impl hoh_app::host::HostHooks for Points {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        // Taken out while they run (an action may wait), put back unless done.
        let due: Vec<(hoh_app::host::RunnerPoint, Action)> = match self.armed.lock() {
            Ok(mut armed) => {
                let (due, rest) = std::mem::take(&mut *armed)
                    .into_iter()
                    .partition(|(at, _)| *at == point);
                *armed = rest;
                due
            }
            Err(_) => return,
        };
        let kept: Vec<_> = due
            .into_iter()
            .filter_map(|(at, mut action)| (!action()).then_some((at, action)))
            .collect();
        if let Ok(mut armed) = self.armed.lock() {
            armed.extend(kept);
        }
    }

    fn suite_ran(&self, agent: &str) {
        if let Some(hold) = &self.hold {
            HoldSuite(hold.clone()).suite_ran(agent);
        }
    }
}

/// Wait on this thread (a hook's: the tick waits with it) until `done`, for at most 60 s; whether it came. What it
/// waits for is done by the host's other tasks on the same runtime (the registry's own loop, which sleeps on the
/// runtime's timer), so the wait first hands this worker's tasks to another thread (`block_in_place`): a worker that
/// just blocks can leave no thread driving the runtime's timers, and nothing that `done` needs then happens before the
/// wait's own bound (the staffing flake: one run in 517 under load; with one worker, every run).
fn waiting(done: impl Fn() -> bool) -> bool {
    tokio::task::block_in_place(|| {
        let end = Instant::now() + Duration::from_secs(60);
        while !done() {
            if Instant::now() > end {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        true
    })
}

/// The events of `kind` of attempt `attempt_id` (by their id's prefix), their payloads in order.
fn attempt_events(project: &Path, attempt_id: &str, kind: &str) -> R<Vec<Value>> {
    let rows = query(
        project,
        &format!(
            "SELECT json_group_array(json(payload_json)) FROM (SELECT payload_json FROM events WHERE kind = '{kind}' \
             AND event_id LIKE '{attempt_id}/%' ORDER BY seq)"
        ),
    )?;
    Ok(serde_json::from_str(&rows)?)
}

/// The attempts of the Commander's turns allocated to the wake of run `run_id`'s adoption of its specification
/// (docs/adr/ADR-031.md decision 1): the wake the `supervisor.woken` of the trigger whose first event is the run's
/// `spec.changed{cause: adopted}` names.
fn adoption_wake_attempts(project: &Path, run_id: &str) -> R<Vec<String>> {
    let rows = query(
        project,
        &format!(
            "SELECT json_group_array(json_extract(a.payload_json, '$.attempt_id')) FROM events a \
             WHERE a.kind = 'supervisor.allocated' AND a.run_id = '{run_id}' \
             AND json_extract(a.payload_json, '$.wake') = (SELECT json_extract(w.payload_json, '$.wake') \
             FROM events w JOIN supervisor_triggers t ON json_extract(w.payload_json, '$.trigger_id') = t.trigger_id \
             JOIN events s ON s.seq = t.first_seq WHERE w.kind = 'supervisor.woken' AND t.run_id = '{run_id}' \
             AND s.kind = 'spec.changed' AND json_extract(s.payload_json, '$.cause') = 'adopted')"
        ),
    )?;
    Ok(serde_json::from_str(&rows)?)
}

/// The lines of `codex.d/calls` (`<name> <marker> <argv>`) whose marker names `attempt_id`.
fn codex_calls(world: &World, attempt_id: &str) -> Vec<String> {
    std::fs::read_to_string(world.bin().join("codex.d/calls"))
        .unwrap_or_default()
        .lines()
        .filter(|l| {
            l.split(' ')
                .nth(1)
                .is_some_and(|m| m.ends_with(&format!("/{attempt_id}")))
        })
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_whose_agent_went_after_staffing_is_refused_before_it_starts() -> R {
    // docs/adr/ADR-030.md decision 6 (the half of a_turn_that_cannot_start_fails_by_its_refusals_reason_and_class that
    // staffing took over, restored deterministically, S3's obligation A1-2): claude is removed after the tick's
    // staffing kept the developer's binding (its snapshot still had claude) and before the round dispatches T-v; the
    // registry's look found it gone, so prepare refuses AGENT_UNAVAILABLE in the Unavailable class: the attempt FAILED
    // with an attempt.start_refused, nothing started, the task BLOCKED(AGENT_UNAVAILABLE) for its role's vacancy (no
    // longer BLOCKED_CAPABILITY). The Commander is codex's, there all along, so the run is not BLOCKED for it (which
    // would withdraw the dispatch before it starts).
    let world = World::new()?;
    world.claude(None)?;
    world.codex("gpt-6-astra")?;
    world.bind_installed()?;
    let project = world.project();
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Codex,
    ] {
        hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&world.path()))?;
    }
    let registry = world.registry();
    let points = std::sync::Arc::new(Points::default());
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        registry.clone(),
        None,
        None,
    );
    config.hooks = points.clone();
    let host = start_when_free(config).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(30);
        until(
            &project,
            &format!(
                "SELECT group_concat(role || ':' || agent, ',') FROM (SELECT role, agent FROM role_bindings \
                 WHERE run_id = '{run_id}' AND role IN ('commander', 'developer') AND session_id IS NOT NULL \
                 ORDER BY role)"
            ),
            "commander:codex,developer:claude",
            limit,
        )
        .await?;
        // The run is RUNNING before the tick (a tick paces and schedules the runs it read as it began).
        ready(&project, &run_id, &[])?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen, claude, at, run) = (gone.clone(), world.bin().join("claude"), project.clone(), run_id.clone());
        points.arm(hoh_app::host::RunnerPoint::AfterStaffing, move || {
            let _ = ready(&at, &run, &[("T-v", "developer")]);
            let _ = std::fs::remove_file(&claude);
            let found = waiting(|| matches!(registry.seen(hoh_agents::compat::Agent::Claude), Some(Err(_))));
            seen.store(found, std::sync::atomic::Ordering::SeqCst);
            true
        });
        until(&project, &ended("T-v/attempt-1"), "FAILED/AGENT_UNAVAILABLE", limit).await?;
        assert!(gone.load(std::sync::atomic::Ordering::SeqCst), "the registry saw claude gone");
        let refused = attempt_events(&project, "T-v/attempt-1", "attempt.start_refused")?;
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert_eq!(refused[0]["reason"], "AGENT_UNAVAILABLE");
        assert!(
            refused[0]["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{refused:?}"
        );
        until(
            &project,
            "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') FROM tasks \
             WHERE task_id = 'T-v'",
            "BLOCKED/AGENT_UNAVAILABLE/",
            limit,
        )
        .await?;
        // Blocked by its attempt's settle, in the Unavailable class (not by the pace's vacancy marking).
        let blocked = attempt_events(&project, "T-v/attempt-1", "task.blocked")?;
        assert_eq!(blocked.len(), 1, "{blocked:?}");
        assert_eq!(
            (&blocked[0]["reason"], &blocked[0]["failure"]["class"]),
            (&json!("AGENT_UNAVAILABLE"), &json!("unavailable")),
            "{blocked:?}"
        );
        // Nothing started: no gate record, no process of the turn.
        assert!(
            !project.join(".hoh/attempts/T-v%2Fattempt-1/gate.jsonl").exists(),
            "a gate was spawned"
        );
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The keys the world's executor drew, by attempt id.
type Keys = std::sync::Arc<std::sync::Mutex<std::collections::BTreeMap<String, String>>>;

/// A recorder of the keys an executor draws, and the hook that records them.
fn key_recorder() -> (Keys, hoh_app::host::product::KeySeen) {
    let keys: Keys = std::sync::Arc::default();
    let into = keys.clone();
    let hook: hoh_app::host::product::KeySeen =
        std::sync::Arc::new(move |attempt: &str, key: &str| {
            if let Ok(mut keys) = into.lock() {
                keys.insert(attempt.to_owned(), key.to_owned());
            }
        });
    (keys, hook)
}

/// The registry of a test's host.
type Registry = std::sync::Arc<hoh_app::host::agents::Registry>;

/// A world whose every role is the fake codex A, registered (verified) before its host starts, with the hooks of
/// `points` and the keys of its gates recorded; its host's registry.
async fn codex_world(points: &std::sync::Arc<Points>) -> R<(World, Host, Keys, Registry)> {
    codex_world_with(points, None).await
}

/// [`codex_world`], `observe` called with each diagnostic its turns collect as they do.
async fn codex_world_with(
    points: &std::sync::Arc<Points>,
    observe: Option<hoh_app::host::product::DiagnosticSeen>,
) -> R<(World, Host, Keys, Registry)> {
    let world = World::new()?;
    world.codex("gpt-6-astra")?;
    world.bind_installed()?;
    hoh_app::compat::register(
        &world.project(),
        hoh_agents::compat::Agent::Codex,
        std::ffi::OsStr::new(&world.path()),
    )?;
    let (keys, hook) = key_recorder();
    let registry = world.registry();
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        registry.clone(),
        Some(hook),
        observe,
    );
    config.hooks = points.clone();
    let host = start_when_free(config).await?;
    Ok((world, host, keys, registry))
}

/// The fake codex B staged beside the world's codex: the same script naming itself B (so another identity, the same
/// version); renamed to `bin/codex`, it replaces A on the PATH.
fn stage_codex_b(world: &World) -> R<PathBuf> {
    let staged = world.bin().join(".codex-B");
    std::fs::write(
        &staged,
        format!("#!/bin/bash\n{}\n", world.codex_body("gpt-6-astra", "B")?),
    )?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    Ok(staged)
}

/// The lines of an attempt's gate record.
fn gate_records(project: &Path, attempt_id: &str) -> R<Vec<Value>> {
    let path = project
        .join(".hoh/attempts")
        .join(attempt_id.replace('%', "%25").replace('/', "%2F"))
        .join("gate.jsonl");
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_codex_replaced_while_held_at_the_gate_does_not_run() -> R {
    // docs/adr/ADR-030.md decision 9 (-03, -101): A is replaced by B after the attempt's prepare passed and its gate
    // was spawned (RunnerPoint::AfterSpawn), before the gate opens: the gate refuses. Its record has exactly one line,
    // `refused` with the attempt's key, not stamp_only; the attempt FAILED COMPAT_UNVERIFIED with an
    // adapter.diagnostic{GATE_REFUSED}; neither A nor B ran the turn; the task BLOCKED(COMPAT_UNVERIFIED), no retry.
    // Decision 13 (b)'s bound: that is the only refused attempt of the task before B's suite publishes (held here
    // after its run): the registry's facts say "files changed since the last look" until it looked at B, so the pace
    // does not put the task back meanwhile; after the publication, the rebind and the grant, it runs B.
    let hold = tempfile::tempdir()?;
    let points = std::sync::Arc::new(Points {
        hold: Some(hold.path().to_path_buf()),
        ..Points::default()
    });
    let (world, host, keys, _) = codex_world(&points).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role = 'developer' \
                 AND session_id IS NOT NULL"
            ),
            "1",
            limit,
        )
        .await?;
        // At the spawn of T-GATE's attempt (its log directory made by its prepare), A is replaced by B.
        let held = project.join(".hoh/attempts/T-GATE%2Fattempt-1");
        let staged = stage_codex_b(&world)?;
        let codex = world.bin().join("codex");
        let replaced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let said = replaced.clone();
        points.arm(hoh_app::host::RunnerPoint::AfterSpawn, move || {
            if !held.exists() {
                return false;
            }
            said.store(
                std::fs::rename(&staged, &codex).is_ok(),
                std::sync::atomic::Ordering::SeqCst,
            );
            true
        });
        ready(&project, &run_id, &[("T-GATE", "developer")])?;
        until(&project, &ended("T-GATE/attempt-1"), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        assert!(replaced.load(std::sync::atomic::Ordering::SeqCst));
        let key = keys
            .lock()
            .ok()
            .and_then(|k| k.get("T-GATE/attempt-1").cloned())
            .ok_or("no key drawn")?;
        let records = gate_records(&project, "T-GATE/attempt-1")?;
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            (&records[0]["outcome"], &records[0]["key"], &records[0]["stamp_only"]),
            (&json!("refused"), &json!(key), &json!(false)),
            "{records:?}"
        );
        let diagnostics = attempt_events(&project, "T-GATE/attempt-1", "adapter.diagnostic")?;
        assert!(
            diagnostics
                .iter()
                .any(|d| d["reason"] == "GATE_REFUSED" && d["item"] == records[0]["item"]),
            "{diagnostics:?}"
        );
        // Only the gate ran: neither A nor B was called for the turn.
        assert_eq!(codex_calls(&world, "T-GATE/attempt-1"), Vec::<String>::new());
        let task = "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') || '/' || \
                    (SELECT count(*) FROM attempts WHERE task_id = 'T-GATE') FROM tasks WHERE task_id = 'T-GATE'";
        assert_eq!(query(&project, task)?, "BLOCKED/COMPAT_UNVERIFIED//1");
        // B looked at and its suite admitted, held before its publication; ticks go on (200 ms each): the task waits,
        // with its one refused attempt.
        exists(&hold.path().join("held"), limit).await?;
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(query(&project, task)?, "BLOCKED/COMPAT_UNVERIFIED//1");
        std::fs::write(hold.path().join("release"), "")?;
        until(&project, &ended("T-GATE/attempt-2"), "SETTLED/", limit).await?;
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-GATE' AND failure_reason = 'COMPAT_UNVERIFIED'"
            )?,
            "1"
        );
        assert_eq!(codex_calls(&world, "T-GATE/attempt-2").len(), 1);
        keep(
            "gate-replaced",
            &json!({"records": records, "diagnostics": diagnostics,
                    "attempts": [attempt_row(&project, "T-GATE/attempt-1")?,
                                 attempt_row(&project, "T-GATE/attempt-2")?]}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(hold.path().join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pi_whose_current_version_moved_is_refused_once_until_its_look() -> R {
    // M2T15R-01 (docs/adr/ADR-030.md decisions 1, 7 and 13 (b), the CTO's design correction): every role is pi,
    // installed as its installer lays it out with releases A and B, registered (verified) under A. At the spawn of
    // T-MOVED's attempt (RunnerPoint::AfterSpawn, its prepare passed) the registry's next look at pi is held and
    // install/current-version moves to B, A's files kept, so none of the look's stamps changes: the gate resolves B and
    // refuses implementation_path; the attempt FAILED COMPAT_UNVERIFIED, the task BLOCKED(COMPAT_UNVERIFIED) with no
    // retry. While the look stays held (ticks go on, 200 ms each), the registry's facts say "files changed since the
    // last look" (the look's own predicate: B is resolved now), so neither staffing's dispatchable predicate nor the
    // pace puts the task back: still exactly one refused attempt. Once the look is let go, B is probed, its suite
    // publishes, the developer rebinds and the task runs B.
    let world = World::new()?;
    world.pi_releases(&["A", "B"], "A")?;
    world.bind_installed()?;
    let project = world.project();
    hoh_app::compat::register(
        &project,
        hoh_agents::compat::Agent::Pi,
        std::ffi::OsStr::new(&world.path()),
    )?;
    let points = std::sync::Arc::new(Points::default());
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        world.registry(),
        None,
        None,
    );
    config.hooks = points.clone();
    let host = start_when_free(config).await?;
    let hold = world.bin().join("pi.d/hold-look");
    let calls = |attempt_id: &str| -> Vec<String> {
        std::fs::read_to_string(world.bin().join("pi.d/calls"))
            .unwrap_or_default()
            .lines()
            .filter(|l| {
                l.split(' ')
                    .nth(1)
                    .is_some_and(|m| m.ends_with(&format!("/{attempt_id}")))
            })
            .map(str::to_owned)
            .collect()
    };
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role = 'developer' \
                 AND session_id IS NOT NULL"
            ),
            "1",
            limit,
        )
        .await?;
        let held = project.join(".hoh/attempts/T-MOVED%2Fattempt-1");
        let (looks, pointer) = (hold.clone(), world.dir.path().join("install/current-version"));
        let moved = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let said = moved.clone();
        points.arm(hoh_app::host::RunnerPoint::AfterSpawn, move || {
            if !held.exists() {
                return false;
            }
            said.store(
                std::fs::write(&looks, "").is_ok() && std::fs::write(&pointer, "B\n").is_ok(),
                std::sync::atomic::Ordering::SeqCst,
            );
            true
        });
        ready(&project, &run_id, &[("T-MOVED", "developer")])?;
        until(&project, &ended("T-MOVED/attempt-1"), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        assert!(moved.load(std::sync::atomic::Ordering::SeqCst));
        let records = gate_records(&project, "T-MOVED/attempt-1")?;
        assert_eq!(records.len(), 1, "{records:?}");
        assert_eq!(
            (&records[0]["outcome"], &records[0]["item"]),
            (&json!("refused"), &json!("implementation_path")),
            "{records:?}"
        );
        // Only the gate ran: neither A nor B was called for the turn.
        assert_eq!(calls("T-MOVED/attempt-1"), Vec::<String>::new());
        let task = "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') || '/' || \
                    (SELECT count(*) FROM attempts WHERE task_id = 'T-MOVED') FROM tasks WHERE task_id = 'T-MOVED'";
        assert_eq!(query(&project, task)?, "BLOCKED/COMPAT_UNVERIFIED//1");
        // The look at B held, ticks go on: the task waits with its one refused attempt.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(query(&project, task)?, "BLOCKED/COMPAT_UNVERIFIED//1");
        std::fs::remove_file(&hold)?;
        until(&project, &ended("T-MOVED/attempt-2"), "SETTLED/", limit).await?;
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-MOVED' AND failure_reason = 'COMPAT_UNVERIFIED'"
            )?,
            "1"
        );
        let ran = calls("T-MOVED/attempt-2");
        assert!(
            !ran.is_empty() && ran.iter().all(|l| l.starts_with("B ")),
            "{ran:?}"
        );
        keep(
            "pi-current-version-moved",
            &json!({"records": records,
                    "attempts": [attempt_row(&project, "T-MOVED/attempt-1")?,
                                 attempt_row(&project, "T-MOVED/attempt-2")?]}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&hold);
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// What a verified agent that ends without a frame forges (docs/adr/ADR-030.md decision 9, -201): every 64-hex string
/// it can read (its argv, environment, descriptors' targets, its attempt's log directory `argv[2]`) as a refusal line
/// in that directory's gate.jsonl, and each `verified` line there copied as `refused`; all it read in `argv[1]`.
const FORGE: &str = r#"import json, os, re, sys
out, logs = sys.argv[1], sys.argv[2]
seen = [" ".join(sys.argv), json.dumps(dict(os.environ))]
for fd in os.listdir("/proc/self/fd"):
    try:
        seen.append(os.readlink("/proc/self/fd/" + fd))
    except OSError:
        pass
for name in sorted(os.listdir(logs)):
    path = os.path.join(logs, name)
    if os.path.isfile(path):
        seen.append(open(path, encoding="utf-8", errors="replace").read())
text = "\n".join(seen)
open(out, "w").write(text)
gate = os.path.join(logs, "gate.jsonl")
lines = []
for key in sorted(set(re.findall("[0-9a-f]{64}", text))):
    lines.append(json.dumps({"outcome": "refused", "key": key, "code": "COMPAT_UNVERIFIED"}))
for line in open(gate).read().splitlines():
    if '"verified"' in line:
        lines.append(line.replace('"verified"', '"refused"'))
with open(gate, "a") as f:
    for line in lines:
        f.write(line + "\n")
"#;

/// Every file under `dir` (links not followed) whose bytes contain `needle`.
fn files_containing(dir: &Path, needle: &[u8]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file()
                && std::fs::read(entry.path())
                    .is_ok_and(|b| b.windows(needle.len()).any(|w| w == needle))
            {
                found.push(entry.path());
            }
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_verified_cli_exiting_65_without_frames_is_retried_as_its_own_failure() -> R {
    // docs/adr/ADR-030.md decision 9 (-101) and the exits table: a verified codex (its gate verified it and execed it)
    // writes no frame, the gate's own refusal line on stderr and exits 65: its gate's record is `verified` only, so the
    // attempt fails by the adapter's own rule for an agent that ended without frames, transient: retried with a
    // backoff, never BLOCKED(COMPAT_UNVERIFIED); the registry's fact and the manifest are as they were. The forging
    // variant (-201) appends a refusal for every 64-hex string it can read and a copy of the verified line as refused:
    // the same outcome, and the attempt's key is nowhere it wrote, in its log directory or in the workspace.
    let points = std::sync::Arc::new(Points::default());
    let (world, host, keys, registry) = codex_world(&points).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role = 'developer' \
                 AND session_id IS NOT NULL"
            ),
            "1",
            limit,
        )
        .await?;
        let manifest = std::fs::read(project.join(".hoh/compat-manifest.json"))?;
        let fact = registry.fact(hoh_agents::compat::Agent::Codex);
        assert!(
            matches!(&fact, Some(hoh_core::staffing::Fact::Usable(u))
                if u.compat == hoh_core::staffing::Compat::Verified),
            "{fact:?}"
        );
        // The forger is told its attempt's log directory (a fake cannot be given an environment variable of the
        // test's: it gets it as an argument baked into its script).
        let logs = project.join(".hoh/attempts/T-FORGE%2Fattempt-1");
        let d = world.bin().join("codex.d");
        std::fs::write(
            d.join("forge.py"),
            FORGE.replace(
                "out, logs = sys.argv[1], sys.argv[2]",
                &format!("out, logs = sys.argv[1], {:?}", logs.display().to_string()),
            ),
        )?;
        ready(&project, &run_id, &[("T-SIXTYFIVE", "developer"), ("T-FORGE", "developer")])?;
        let mut kept = serde_json::Map::new();
        for task in ["T-SIXTYFIVE", "T-FORGE"] {
            let attempt = format!("{task}/attempt-1");
            until(
                &project,
                &format!(
                    "SELECT state FROM attempts WHERE attempt_id = '{attempt}' \
                     AND state IN ('SETTLED', 'FAILED', 'CANCELLED')"
                ),
                "FAILED",
                limit,
            )
            .await?;
            let failure = query(
                &project,
                &format!("SELECT failure_reason FROM attempts WHERE attempt_id = '{attempt}'"),
            )?;
            assert_ne!(failure, "COMPAT_UNVERIFIED", "{attempt}");
            let records = gate_records(&project, &attempt)?;
            assert_eq!(records[0]["outcome"], "verified", "{attempt}: {records:?}");
            assert!(records[0].get("key").is_none(), "{records:?}");
            assert_eq!(
                records.iter().filter(|r| r["outcome"] == "verified").count(),
                1,
                "{attempt}: {records:?}"
            );
            let retried = attempt_events(&project, &attempt, "task.retry_scheduled")?;
            assert_eq!(retried.len(), 1, "{attempt}: {retried:?}");
            assert_eq!(retried[0]["failure"]["class"], "transient", "{retried:?}");
            assert!(retried[0]["next_retry_at"].is_string(), "{retried:?}");
            assert_eq!(
                query(
                    &project,
                    &format!(
                        "SELECT count(*) FROM events WHERE kind = 'task.blocked' \
                         AND json_extract(payload_json, '$.task_id') = '{task}'"
                    )
                )?,
                "0",
                "{task} was blocked"
            );
            // It ran: the agent's own call is there (the gate let it through).
            assert_eq!(codex_calls(&world, &attempt).len(), 1, "{attempt}");
            kept.insert(attempt.clone(), json!({"failure": failure, "records": records, "retry": retried}));
        }
        // The forger forged: lines it appended are there, none with the attempt's key.
        let records = gate_records(&project, "T-FORGE/attempt-1")?;
        assert!(records.len() > 1, "nothing forged: {records:?}");
        let key = keys
            .lock()
            .ok()
            .and_then(|k| k.get("T-FORGE/attempt-1").cloned())
            .ok_or("no key drawn")?;
        assert!(!records.iter().any(|r| r["key"] == json!(key)), "{records:?}");
        let forged: Vec<PathBuf> = std::fs::read_dir(&d)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("forged.")))
            .collect();
        assert_eq!(forged.len(), 1, "{forged:?}");
        assert_eq!(files_containing(&world.bin(), key.as_bytes()), Vec::<PathBuf>::new());
        assert_eq!(files_containing(&project, key.as_bytes()), Vec::<PathBuf>::new());
        assert_eq!(std::fs::read(project.join(".hoh/compat-manifest.json"))?, manifest);
        assert_eq!(registry.fact(hoh_agents::compat::Agent::Codex), fact);
        keep("sixty-five", &Value::Object(kept))?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_native_error_named_like_a_reserved_code_is_its_own_failure() -> R {
    // docs/adr/ADR-030.md decision 6 (-300): a verified claude's legal `result` frame whose subtype is
    // COMPAT_UNVERIFIED fails its attempt native:COMPAT_UNVERIFIED, transient (retried with a backoff), never blocking
    // the task; MODEL_UNAVAILABLE and BLOCKED_AUTH the same, the binding not blocked and nothing excluded; codex's
    // turn/completed with codexErrorInfo COMPAT_UNVERIFIED the same.
    let world = World::new()?;
    world.claude(None)?;
    world.codex("gpt-6-astra")?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.auditor]\nagent = \"codex\"\nprompt = \"builtin:developer\"\n",
    )?;
    world.bind_installed()?;
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Codex,
    ] {
        hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&world.path()))?;
    }
    let host = start_when_free(world.config()).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        until(
            &project,
            &format!(
                "SELECT count(*) FROM role_bindings WHERE run_id = '{run_id}' AND role IN ('developer', 'auditor') \
                 AND session_id IS NOT NULL"
            ),
            "2",
            limit,
        )
        .await?;
        let cases = [
            ("T-NATIVE-COMPAT_UNVERIFIED", "developer", "native:COMPAT_UNVERIFIED"),
            ("T-NATIVE-MODEL_UNAVAILABLE", "developer", "native:MODEL_UNAVAILABLE"),
            ("T-NATIVE-BLOCKED_AUTH", "developer", "native:BLOCKED_AUTH"),
            ("T-CODEX-NATIVE", "auditor", "native:COMPAT_UNVERIFIED"),
        ];
        let tasks: Vec<(&str, &str)> = cases.iter().map(|(t, r, _)| (*t, *r)).collect();
        ready(&project, &run_id, &tasks)?;
        let mut kept = serde_json::Map::new();
        for (task, role, failure) in cases {
            let attempt = format!("{task}/attempt-1");
            until(&project, &ended(&attempt), &format!("FAILED/{failure}"), limit).await?;
            let retried = attempt_events(&project, &attempt, "task.retry_scheduled")?;
            assert_eq!(retried.len(), 1, "{attempt}: {retried:?}");
            assert_eq!(retried[0]["failure"]["class"], "transient", "{retried:?}");
            assert!(retried[0]["next_retry_at"].is_string(), "{retried:?}");
            assert_eq!(
                query(
                    &project,
                    &format!(
                        "SELECT count(*) FROM events WHERE kind = 'task.blocked' \
                         AND json_extract(payload_json, '$.task_id') = '{task}'"
                    )
                )?,
                "0",
                "{task} was blocked"
            );
            assert_eq!(
                query(
                    &project,
                    &format!(
                        "SELECT state FROM role_bindings WHERE run_id = '{run_id}' AND role = '{role}'"
                    )
                )?,
                "ACTIVE",
                "{role}"
            );
            kept.insert(attempt, json!({"failure": failure, "retry": retried}));
        }
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT count(*) FROM events WHERE run_id = '{run_id}' AND kind IN \
                     ('team.binding_excluded', 'team.binding_blocked')"
                )
            )?,
            "0"
        );
        keep("native-errors", &Value::Object(kept))?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suite_passing_after_the_snapshot_does_not_start_the_new_codex_in_the_old_session() -> R {
    // docs/adr/ADR-030.md decision 9 (d) (M2T15D-100): the developer is bound to codex A (session S_A); B replaces A on
    // the PATH and its suite, admitted, is held before its publication. In the first tick after T2 (a reader task, never
    // BLOCKED) is imported, whose snapshot has B pending, RunnerPoint::AfterStaffing lets the suite publish and waits
    // until the registry's fact is B verified: the same tick's round grants T2's first attempt in S_A, and its prepare
    // refuses it COMPAT_UNVERIFIED (S_A waits for its rebind): B never ran in S_A. Then the rebind, the grant and T2's
    // unblocking: its next attempt runs B in a session whose identity is B's.
    let hold = tempfile::tempdir()?;
    let points = std::sync::Arc::new(Points {
        hold: Some(hold.path().to_path_buf()),
        ..Points::default()
    });
    let (world, host, _, registry) = codex_world(&points).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        let session_of = |role: &str| {
            query(
                &project,
                &format!(
                    "SELECT coalesce(session_id, '') FROM role_bindings WHERE run_id = '{run_id}' AND role = '{role}'"
                ),
            )
        };
        let end = Instant::now() + limit;
        while session_of("developer")?.is_empty() && Instant::now() < end {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let s_a = session_of("developer")?;
        assert!(!s_a.is_empty(), "no session for A");
        ready(&project, &run_id, &[])?;
        let staged = stage_codex_b(&world)?;
        let sha_b = {
            use sha2::Digest as _;
            sha2::Sha256::digest(std::fs::read(&staged)?)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        };
        std::fs::rename(&staged, world.bin().join("codex"))?;
        // B looked at, its suite admitted and held before its publication: B is pending.
        exists(&hold.path().join("held"), limit).await?;
        let codex = hoh_agents::compat::Agent::Codex;
        let pending = registry.fact(codex);
        assert!(
            matches!(&pending, Some(hoh_core::staffing::Fact::Usable(u))
                if u.identity.implementation_sha256 == sha_b
                    && matches!(u.compat, hoh_core::staffing::Compat::Pending(_))),
            "{pending:?}"
        );
        let watermark = std::sync::Arc::new(std::sync::Mutex::new(Value::Null));
        let (mark, at, run, release, facts) = (
            watermark.clone(),
            project.clone(),
            run_id.clone(),
            hold.path().join("release"),
            registry.clone(),
        );
        let wanted = sha_b.clone();
        points.arm(hoh_app::host::RunnerPoint::AfterStaffing, move || {
            let seq = query(&at, "SELECT max(seq) FROM events").unwrap_or_default();
            let snapshot = format!("{:?}", facts.fact(codex));
            let _ = ready(&at, &run, &[("T2", "developer")]);
            let _ = std::fs::write(&release, "");
            let verified = waiting(|| {
                matches!(facts.fact(codex), Some(hoh_core::staffing::Fact::Usable(u))
                    if u.identity.implementation_sha256 == wanted
                        && u.compat == hoh_core::staffing::Compat::Verified)
            });
            if let Ok(mut mark) = mark.lock() {
                *mark = json!({"seq_at_the_hook": seq, "fact_before": snapshot, "verified_after": verified});
            }
            true
        });
        until(&project, &ended("T2/attempt-1"), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        let mark = watermark.lock().map(|m| m.clone()).unwrap_or_default();
        assert_eq!(mark["verified_after"], true, "{mark}");
        let first = attempt_row(&project, "T2/attempt-1")?;
        assert_eq!(
            query(&project, "SELECT session_id FROM attempts WHERE attempt_id = 'T2/attempt-1'")?,
            s_a
        );
        let refused = attempt_events(&project, "T2/attempt-1", "attempt.start_refused")?;
        assert_eq!(refused.len(), 1, "{refused:?}");
        let detail = refused[0]["detail"].as_str().unwrap_or_default();
        assert!(
            detail.contains(&s_a) && detail.contains("waits for its rebind") && detail.contains(&sha_b),
            "{detail}"
        );
        assert_eq!(codex_calls(&world, "T2/attempt-1"), Vec::<String>::new(), "B ran in S_A");
        let dispatched: i64 = query(
            &project,
            "SELECT seq FROM events WHERE kind = 'schedule.round' AND event_id = 'T2/attempt-1/round'",
        )?
        .parse()?;
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT count(*) FROM events WHERE kind = 'task.blocked' \
                     AND json_extract(payload_json, '$.task_id') = 'T2' AND seq < {dispatched}"
                )
            )?,
            "0",
            "T2 was BLOCKED before its first attempt"
        );
        // Rebound, granted, unblocked: the next attempt runs B in a session of B's identity.
        until(&project, &ended("T2/attempt-2"), "SETTLED/", limit).await?;
        let rebound = query(
            &project,
            &format!(
                "SELECT count(*) FROM events WHERE run_id = '{run_id}' AND kind = 'team.binding_applied' \
                 AND json_extract(payload_json, '$.role') = 'developer' \
                 AND json_extract(payload_json, '$.cause') = 'rebind'"
            ),
        )?;
        assert_eq!(rebound, "1");
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM events WHERE kind = 'task.unblocked' \
                 AND json_extract(payload_json, '$.task_id') = 'T2'"
            )?,
            "1"
        );
        let s_b = query(&project, "SELECT session_id FROM attempts WHERE attempt_id = 'T2/attempt-2'")?;
        assert_ne!(s_b, s_a);
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.identity.implementation_sha256') FROM events \
                     WHERE kind = 'team.session_bound' AND json_extract(payload_json, '$.session_id') = '{s_b}'"
                )
            )?,
            sha_b
        );
        let calls = codex_calls(&world, "T2/attempt-2");
        assert!(
            calls.len() == 1 && calls[0].starts_with("B "),
            "{calls:?}"
        );
        keep(
            "suite-after-snapshot",
            &json!({"hook": mark, "attempts": [first, attempt_row(&project, "T2/attempt-2")?],
                    "refused": refused,
                    "b_calls": std::fs::read_to_string(world.bin().join("codex.d/calls"))?
                        .lines().filter(|l| l.starts_with("B ")).collect::<Vec<_>>()}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(hold.path().join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The diagnostics a host's turns collected, as they collected them: (attempt id, diagnostic) in order
/// (docs/adr/ADR-030.md decision 9, M2T15D-401: until a turn ends they are in memory only).
type Observed = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

/// A recorder of the diagnostics an executor's turns collect, and the hook that records them.
fn observed() -> (Observed, hoh_app::host::product::DiagnosticSeen) {
    let seen: Observed = std::sync::Arc::default();
    let into = seen.clone();
    let hook: hoh_app::host::product::DiagnosticSeen =
        std::sync::Arc::new(move |attempt: &str, diagnostic: &Value| {
            if let Ok(mut seen) = into.lock() {
                seen.push((attempt.to_owned(), diagnostic.clone()));
            }
        });
    (seen, hook)
}

/// The diagnostics of `reason` attempt `attempt_id`'s turn collected so far.
fn observed_of(observed: &Observed, attempt_id: &str, reason: &str) -> Vec<Value> {
    observed
        .lock()
        .map(|seen| {
            seen.iter()
                .filter(|(attempt, d)| attempt == attempt_id && d["reason"] == reason)
                .map(|(_, d)| d.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Wait until attempt `attempt_id`'s turn collected `count` diagnostics of `reason`, for at most 60 s; them.
async fn observed_count(
    observed: &Observed,
    attempt_id: &str,
    reason: &str,
    count: usize,
) -> R<Vec<Value>> {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let found = observed_of(observed, attempt_id, reason);
        if found.len() >= count {
            return Ok(found);
        }
        if Instant::now() > end {
            return Err(
                format!("{attempt_id}: {count} {reason} never collected: {found:?}").into(),
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What attempt `attempt_id`'s turn sent its fake `agent` (`<agent>.d/log.<pid>/received.jsonl`, its process found by
/// its `marker.<pid>`): each line's `field` (`type` for pi's commands, `method` for codex's).
fn received(world: &World, agent: &str, attempt_id: &str, field: &str) -> R<Vec<String>> {
    let d = world.bin().join(format!("{agent}.d"));
    let mut lines = Vec::new();
    for entry in std::fs::read_dir(&d)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.strip_prefix("marker.") else {
            continue;
        };
        if !std::fs::read_to_string(entry.path())?
            .trim()
            .ends_with(&format!("/{attempt_id}"))
        {
            continue;
        }
        let log = d.join(format!("log.{pid}/received.jsonl"));
        for line in std::fs::read_to_string(log).unwrap_or_default().lines() {
            let message: Value = serde_json::from_str(line)?;
            lines.push(message[field].as_str().unwrap_or_default().to_owned());
        }
    }
    Ok(lines)
}

/// Wait until attempt `attempt_id`'s gate recorded that it verified its agent and execed it, for at most 60 s.
async fn gate_verified(project: &Path, attempt_id: &str) -> R {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        if gate_records(project, attempt_id)
            .is_ok_and(|records| records.iter().any(|r| r["outcome"] == "verified"))
        {
            return Ok(());
        }
        if Instant::now() > end {
            return Err(format!("{attempt_id}: its gate never verified").into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Lower-case hex of the sha256 of the file at `path`.
fn file_sha256(path: &Path) -> R<String> {
    use sha2::Digest as _;
    Ok(sha2::Sha256::digest(std::fs::read(path)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// The fake pi B staged beside the world's pi: the same script naming itself B (another identity, the same version).
fn stage_pi_b(world: &World) -> R<PathBuf> {
    let staged = world.bin().join(".pi-B");
    std::fs::write(&staged, format!("#!/bin/bash\n{}\n", world.pi_body("B")?))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    Ok(staged)
}

/// Whether HoH session `session_id` of run `run_id`'s `role` at `epoch` still owes its first input (ADR-024 decision
/// 4, as the store judges it): its session.start was delivered to it and no attempt of it SETTLED.
fn first_input_owed(
    project: &Path,
    run_id: &str,
    role: &str,
    epoch: i64,
    session_id: &str,
) -> R<bool> {
    Ok(query(
        project,
        &format!(
            "SELECT EXISTS(SELECT 1 FROM outbox o JOIN receipts r ON r.message_id = o.message_id \
             WHERE o.message_id = '{run_id}/session-start/{role}/{epoch}' AND o.state = 'ACKED' \
             AND json_extract(r.result_json, '$.session_id') = '{session_id}') \
             AND NOT EXISTS(SELECT 1 FROM attempts WHERE session_id = '{session_id}' AND state = 'SETTLED')"
        ),
    )? == "1")
}

/// What a test does to pi's installation while its turn waits before its first answer (docs/adr/ADR-030.md decision
/// 9, "首个输入之前的再核对").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BeforeAnswer {
    /// Another script renamed to pi's implementation path.
    Replaced,
    /// One byte appended to pi's implementation in place: its ctime and hash change, not its inode.
    Appended,
    /// pi's implementation set to the mode it has: only its ctime changes.
    SameMode,
    /// The attempt's gate.jsonl deleted and written anew: no file of pi's changes.
    GateRewritten,
    Unchanged,
}

/// The new developer pi's (claude, then pi at epoch 2 with its handover package) first turn T-HOLD-ANSWER waits before
/// its first answer, after its gate verified it; `change` is made then, and the turn released. Suites are held before
/// their publication, so a new identity of pi stays unverified. Returns what the test asserts on: the world, the suites'
/// hold directory, the host (still running), the run and the diagnostics observed.
async fn pi_held_before_its_first_answer(
    change: BeforeAnswer,
) -> R<(World, tempfile::TempDir, Host, String, Observed)> {
    let world = World::new()?;
    world.claude(None)?;
    world.pi()?;
    world.bind_installed()?;
    let project = world.project();
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Pi,
    ] {
        hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&world.path()))?;
    }
    let hold = tempfile::tempdir()?;
    let points = std::sync::Arc::new(Points {
        hold: Some(hold.path().to_path_buf()),
        ..Points::default()
    });
    let (observed, hook) = observed();
    let registry = world.registry();
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        registry.clone(),
        None,
        Some(hook),
    );
    config.hooks = points;
    let host = start_when_free(config).await?;
    let run = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        ready(&project, &run_id, &[])?;
        let developer = format!(
            "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
             WHERE run_id = '{run_id}' AND role = 'developer'"
        );
        until(&project, &developer, &format!("claude/1/{run_id}/session/developer/1"), limit).await?;
        configure(&host, "to-pi", team_revision(&project)?, json!({"developer": {"agent": "pi"}})).await?;
        until(
            &project,
            &format!("SELECT state FROM outbox WHERE message_id = '{run_id}/session-start/developer/2'"),
            "ACKED",
            limit,
        )
        .await?;
        ready(&project, &run_id, &[("T-HOLD-ANSWER", "developer")])?;
        let attempt = "T-HOLD-ANSWER/attempt-1";
        gate_verified(&project, attempt).await?;
        let pi = world.bin().join("pi");
        match change {
            BeforeAnswer::Replaced => std::fs::rename(stage_pi_b(&world)?, &pi)?,
            BeforeAnswer::Appended => {
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&pi)?
                    .write_all(b"#")?;
            }
            BeforeAnswer::SameMode => {
                let mode = std::fs::metadata(&pi)?.permissions().mode();
                std::fs::set_permissions(&pi, std::fs::Permissions::from_mode(mode))?;
                // Released only once the registry looked at pi again (its fact verified with the new stamps), so the
                // retry this variant claims comes due after that look. A retry due before it (its backoff drawn short,
                // the look slow under load) is refused by prepare ("files changed since the last look") and its task
                // waits for the look: the one refused attempt docs/adr/ADR-030.md allows there (decision 13 (b) and
                // its S6c correction, the registry's facts rereading the stamps), a path of the look's timing that
                // this test does not claim. The turn held here keeps the stamps of the look before the change
                // (`Expect.stamp`).
                let changed = std::fs::metadata(&pi)?;
                let end = Instant::now() + limit;
                loop {
                    use std::os::unix::fs::MetadataExt as _;
                    if let Some(hoh_core::staffing::Fact::Usable(u)) =
                        registry.fact(hoh_agents::compat::Agent::Pi)
                        && u.compat == hoh_core::staffing::Compat::Verified
                        && (u.stamp.implementation.ctime_sec, u.stamp.implementation.ctime_nsec)
                            == (changed.ctime(), changed.ctime_nsec())
                    {
                        break;
                    }
                    if Instant::now() > end {
                        return Err("the registry never looked at pi again".into());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
            BeforeAnswer::GateRewritten => {
                let gate = project.join(".hoh/attempts/T-HOLD-ANSWER%2Fattempt-1/gate.jsonl");
                std::fs::remove_file(&gate)?;
                std::fs::write(
                    &gate,
                    json!({"outcome": "refused", "code": "COMPAT_UNVERIFIED", "item": "stamp"}).to_string()
                        + "\n",
                )?;
            }
            BeforeAnswer::Unchanged => {}
        }
        std::fs::write(world.bin().join("pi.d/release"), "")?;
        R::Ok(run_id)
    }
    .await;
    match run {
        Ok(run_id) => Ok((world, hold, host, run_id, observed)),
        Err(e) => {
            let _ = std::fs::write(hold.path().join("release"), "");
            host.shutdown().await;
            world.stop_fakes()?;
            Err(e)
        }
    }
}

/// The checks of [`pi_held_before_its_first_answer`]'s turn for `change`, then the world's end.
async fn pi_sends_no_prompt_when(change: BeforeAnswer) -> R {
    let (world, hold, host, run_id, observed) = pi_held_before_its_first_answer(change).await?;
    let project = world.project();
    let attempt = "T-HOLD-ANSWER/attempt-1";
    let outcome = async {
        let limit = Duration::from_secs(60);
        let refused = matches!(
            change,
            BeforeAnswer::Replaced | BeforeAnswer::Appended | BeforeAnswer::SameMode
        );
        if !refused {
            until(&project, &ended(attempt), "SETTLED/", limit).await?;
            let sent = received(&world, "pi", attempt, "type")?;
            assert!(sent.iter().any(|t| t == "prompt"), "{change:?}: {sent:?}");
            let diagnostics = attempt_events(&project, attempt, "adapter.diagnostic")?;
            assert!(
                !diagnostics
                    .iter()
                    .any(|d| d["reason"] == "IMPLEMENTATION_CHANGED_BEFORE_INPUT"
                        || d["reason"] == "IMPLEMENTATION_CHANGED_IN_TURN"),
                "{change:?}: {diagnostics:?}"
            );
            return R::Ok(json!({"change": format!("{change:?}"), "sent": sent,
                                "attempt": attempt_row(&project, attempt)?}));
        }
        until(&project, &ended(attempt), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        // Only get_state reached pi: no prompt.
        let sent = received(&world, "pi", attempt, "type")?;
        assert_eq!(sent, ["get_state"], "{change:?}");
        let diagnostics: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
            .into_iter()
            .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_BEFORE_INPUT")
            .collect();
        assert_eq!(diagnostics.len(), 1, "{change:?}: {diagnostics:?}");
        let d = &diagnostics[0];
        assert_eq!(d["file"], "implementation", "{d}");
        assert_eq!(
            d["seen"]["ino"] == d["expected"]["ino"],
            change != BeforeAnswer::Replaced,
            "{change:?}: {d}"
        );
        assert_ne!(d["seen"], d["expected"], "{d}");
        assert_eq!(d["stamp_only"], json!(change == BeforeAnswer::SameMode), "{d}");
        // Collected in the turn's memory first, shown to the test's hook as it was.
        assert_eq!(
            observed_of(&observed, attempt, "IMPLEMENTATION_CHANGED_BEFORE_INPUT").len(),
            1
        );
        let blocked = query(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'task.blocked' \
             AND json_extract(payload_json, '$.task_id') = 'T-HOLD-ANSWER' \
             AND json_extract(payload_json, '$.reason') = 'COMPAT_UNVERIFIED'",
        )?;
        let scheduled = query(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'task.retry_scheduled' \
             AND json_extract(payload_json, '$.task_id') = 'T-HOLD-ANSWER'",
        )?;
        let session = query(
            &project,
            &format!("SELECT session_id FROM attempts WHERE attempt_id = '{attempt}'"),
        )?;
        let owed = first_input_owed(&project, &run_id, "developer", 2, &session)?;
        if change == BeforeAnswer::SameMode {
            // Only the stamps changed: transient, retried; the retry sends its prompt.
            assert_eq!((blocked.as_str(), scheduled.as_str()), ("0", "1"));
            until(&project, &ended("T-HOLD-ANSWER/attempt-2"), "SETTLED/", limit).await?;
            let retried = received(&world, "pi", "T-HOLD-ANSWER/attempt-2", "type")?;
            assert!(retried.iter().any(|t| t == "prompt"), "{retried:?}");
        } else {
            // Another identity: the task waits for its suite (held here) and rebind; the first input is still owed.
            assert_eq!((blocked.as_str(), scheduled.as_str()), ("1", "0"));
            assert_eq!(
                query(
                    &project,
                    "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') \
                     FROM tasks WHERE task_id = 'T-HOLD-ANSWER'"
                )?,
                "BLOCKED/COMPAT_UNVERIFIED/"
            );
            assert!(owed, "{change:?}: the first input of {session} is not owed");
        }
        R::Ok(json!({"change": format!("{change:?}"), "sent": sent, "diagnostic": d,
                     "attempt": attempt_row(&project, attempt)?, "first_input_owed": owed}))
    }
    .await;
    let _ = std::fs::write(hold.path().join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    keep(&format!("pi-before-input-{change:?}"), &outcome?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pi_sends_no_prompt_when_its_script_changed_before_the_first_input() -> R {
    // docs/adr/ADR-030.md decision 9 ("首个输入之前的再核对", the owner's decision of 2026-11-13): the new developer pi's
    // turn waits before it answers get_state; once its gate's `verified` record is there (the test's sync point only),
    // another script is renamed to pi's implementation path and the turn released: pi received get_state only, no
    // prompt; an adapter.diagnostic{IMPLEMENTATION_CHANGED_BEFORE_INPUT} whose expected and seen inodes differ, the
    // attempt FAILED COMPAT_UNVERIFIED, the task BLOCKED(COMPAT_UNVERIFIED), the session's first input still owed.
    pi_sends_no_prompt_when(BeforeAnswer::Replaced).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pi_sends_no_prompt_when_its_script_changed_before_the_first_input_appended_in_place() -> R
{
    // The variant of an in-place append of one byte (its ctime changes, its inode does not, its hash does): the same.
    pi_sends_no_prompt_when(BeforeAnswer::Appended).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pi_sends_no_prompt_when_its_script_changed_before_the_first_input_same_mode_is_transient()
-> R {
    // The variant of a chmod to the same mode (only its ctime changes, its identity does not): no prompt, FAILED
    // COMPAT_UNVERIFIED with stamp_only, the task READY with a retry scheduled (transient), and the retry runs.
    pi_sends_no_prompt_when(BeforeAnswer::SameMode).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pi_sends_no_prompt_when_its_script_changed_before_the_first_input_not_by_its_gate_record()
-> R {
    // The variant of the attempt's gate.jsonl deleted and written anew before pi answers: the re-check reads no record,
    // so the prompt is sent and the turn SETTLED.
    pi_sends_no_prompt_when(BeforeAnswer::GateRewritten).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pi_sends_no_prompt_when_its_script_changed_before_the_first_input_control() -> R {
    // The control: nothing changed, the prompt is sent and the turn SETTLED, no re-check diagnostic.
    pi_sends_no_prompt_when(BeforeAnswer::Unchanged).await
}

/// Wait until the developer of run `run_id` has a session, for at most 60 s; its session and epoch.
async fn developer_session(project: &Path, run_id: &str) -> R<(String, i64)> {
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        let row = query(
            project,
            &format!(
                "SELECT coalesce(session_id, '') || ' ' || role_epoch FROM role_bindings WHERE run_id = '{run_id}' \
                 AND role = 'developer'"
            ),
        )?;
        if let Some((session, epoch)) = row.split_once(' ')
            && !session.is_empty()
        {
            return Ok((session.to_owned(), epoch.parse()?));
        }
        if Instant::now() > end {
            return Err(format!("the developer of {run_id} has no session: {row:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn codex_app_server_sends_no_thread_start_when_its_implementation_changed() -> R {
    // docs/adr/ADR-030.md decision 9 ("首个输入之前的再核对"): codex's App Server answers initialize before HoH sends its
    // first input, thread/start. The fake App Server waits before it answers anything; once the attempt's gate verified
    // A, B is renamed to codex's path and the turn released: stdin held initialize only, no thread/start; an
    // adapter.diagnostic{IMPLEMENTATION_CHANGED_BEFORE_INPUT} whose expected and seen inodes differ; the attempt FAILED
    // COMPAT_UNVERIFIED; the task BLOCKED(COMPAT_UNVERIFIED) while B's suite (held here) has not published.
    let hold = tempfile::tempdir()?;
    let points = std::sync::Arc::new(Points {
        hold: Some(hold.path().to_path_buf()),
        ..Points::default()
    });
    let (observed, hook) = observed();
    let (world, host, _, _) = codex_world_with(&points, Some(hook)).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        developer_session(&project, &run_id).await?;
        ready(&project, &run_id, &[("T-HOLD-INIT", "developer")])?;
        let attempt = "T-HOLD-INIT/attempt-1";
        gate_verified(&project, attempt).await?;
        // Replaced only once A runs (its call is written as its script starts): the fake is a script, executed by its
        // interpreter, which opens it by its path after the gate's check (plan E2), so a rename before that open
        // would run B in the attempt (docs/adr/ADR-030.md decision 9's R2, accepted), not the change before the first
        // input this test claims.
        let end = Instant::now() + limit;
        while codex_calls(&world, attempt).is_empty() {
            if Instant::now() > end {
                return Err(format!("{attempt}: its codex never ran").into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        std::fs::rename(stage_codex_b(&world)?, world.bin().join("codex"))?;
        std::fs::write(world.bin().join("codex.d/release"), "")?;
        until(&project, &ended(attempt), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        let sent = received(&world, "codex", attempt, "method")?;
        assert!(
            sent == ["initialize"] || sent == ["initialize", "initialized"],
            "{sent:?}"
        );
        let diagnostics: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
            .into_iter()
            .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_BEFORE_INPUT")
            .collect();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let d = &diagnostics[0];
        assert_ne!(d["seen"]["ino"], d["expected"]["ino"], "{d}");
        assert_eq!(d["stamp_only"], false, "{d}");
        assert_eq!(
            observed_of(&observed, attempt, "IMPLEMENTATION_CHANGED_BEFORE_INPUT").len(),
            1
        );
        // The turn ran as A (its gate verified A); B was never run for it.
        let calls = codex_calls(&world, attempt);
        assert!(
            calls.len() == 1 && calls[0].starts_with("A "),
            "{calls:?}"
        );
        assert_eq!(
            query(
                &project,
                "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') \
                 FROM tasks WHERE task_id = 'T-HOLD-INIT'"
            )?,
            "BLOCKED/COMPAT_UNVERIFIED/"
        );
        keep(
            "codex-before-input",
            &json!({"sent": sent, "diagnostic": d, "attempt": attempt_row(&project, attempt)?}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(hold.path().join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_codex_replaced_during_its_turn_is_recorded_and_settles() -> R {
    // docs/adr/ADR-030.md decision 9 ("turn 中的再核对（只记录）", the owner's decision of 2026-11-14, M2T15D-401): the
    // fake codex's turn writes its first frame and waits; B (another content) is renamed to the implementation path and
    // the test waits for the executor's hook to report the attempt's IMPLEMENTATION_CHANGED_IN_TURN (a turn's
    // diagnostics are in memory until it ends), then releases it: the turn SETTLED, the task done as usual; once
    // settled, the attempt's events hold exactly one adapter.diagnostic{IMPLEMENTATION_CHANGED_IN_TURN, file:
    // implementation} whose expected and seen inodes differ, kept in its diagnostics.jsonl too; the next task imported
    // has its first attempt refused (COMPAT_UNVERIFIED).
    let hold = tempfile::tempdir()?;
    let points = std::sync::Arc::new(Points {
        hold: Some(hold.path().to_path_buf()),
        ..Points::default()
    });
    let (observed, hook) = observed();
    let (world, host, _, _) = codex_world_with(&points, Some(hook)).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        developer_session(&project, &run_id).await?;
        ready(&project, &run_id, &[("T-HOLD-TURN", "developer")])?;
        let attempt = "T-HOLD-TURN/attempt-1";
        // The turn is under way: turn/start reached the fake, which wrote its first frame and waits.
        let end = Instant::now() + limit;
        while !received(&world, "codex", attempt, "method")?
            .iter()
            .any(|m| m == "turn/start")
        {
            if Instant::now() > end {
                return Err("the turn never started".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        std::fs::rename(stage_codex_b(&world)?, world.bin().join("codex"))?;
        let seen = observed_count(&observed, attempt, "IMPLEMENTATION_CHANGED_IN_TURN", 1).await?;
        // Re-checks go on meanwhile (every 200 ms): the same change is not recorded again.
        tokio::time::sleep(Duration::from_millis(700)).await;
        std::fs::write(world.bin().join("codex.d/release"), "")?;
        until(&project, &ended(attempt), "SETTLED/", limit).await?;
        until(
            &project,
            "SELECT state FROM tasks WHERE task_id = 'T-HOLD-TURN'",
            "IMPLEMENTED",
            limit,
        )
        .await?;
        let diagnostics: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
            .into_iter()
            .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_IN_TURN")
            .collect();
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        let d = &diagnostics[0];
        assert_eq!(d["file"], "implementation", "{d}");
        assert_ne!(d["seen"]["ino"], d["expected"]["ino"], "{d}");
        assert_eq!(
            seen,
            std::slice::from_ref(d),
            "what the hook saw is what was recorded"
        );
        let kept = std::fs::read_to_string(
            project.join(".hoh/attempts/T-HOLD-TURN%2Fattempt-1/diagnostics.jsonl"),
        )?;
        assert_eq!(
            kept.lines()
                .filter(|l| l.contains("IMPLEMENTATION_CHANGED_IN_TURN"))
                .count(),
            1,
            "{kept}"
        );
        let calls = codex_calls(&world, attempt);
        assert!(calls.len() == 1 && calls[0].starts_with("A "), "{calls:?}");
        // A new attempt is refused before B runs.
        ready(&project, &run_id, &[("T-NEXT", "developer")])?;
        until(
            &project,
            &ended("T-NEXT/attempt-1"),
            "FAILED/COMPAT_UNVERIFIED",
            limit,
        )
        .await?;
        assert_eq!(
            codex_calls(&world, "T-NEXT/attempt-1"),
            Vec::<String>::new()
        );
        keep(
            "codex-in-turn",
            &json!({"diagnostic": d, "observed": seen, "attempt": attempt_row(&project, attempt)?,
                    "next": attempt_row(&project, "T-NEXT/attempt-1")?}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(world.bin().join("codex.d/release"), "");
    let _ = std::fs::write(hold.path().join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// A world of claude installed as its installer does ([`World::claude_versions`], its turns waiting for `hold`) as the
/// developer and codex, both registered, its diagnostics observed, suites held in `suites`; its host.
async fn claude_versions_world(hold: &Path, suites: &Path) -> R<(World, Host, Observed)> {
    let world = World::new()?;
    world.claude_versions(Some(hold))?;
    world.codex("gpt-6-astra")?;
    world.bind_installed()?;
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Codex,
    ] {
        hoh_app::compat::register(&world.project(), agent, std::ffi::OsStr::new(&world.path()))?;
    }
    let (observed, hook) = observed();
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        world.registry(),
        None,
        Some(hook),
    );
    config.hooks = std::sync::Arc::new(Points {
        hold: Some(suites.to_path_buf()),
        ..Points::default()
    });
    let host = start_when_free(config).await?;
    Ok((world, host, observed))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_launcher_retargeted_during_its_turn_is_recorded() -> R {
    // docs/adr/ADR-030.md decision 9 ("turn 中的再核对", "记录到什么"): claude's launcher links to versions/A. While its
    // turn runs, the link is retargeted to versions/B (A kept): one diagnostic, file launcher; then A is deleted: a
    // second one, file implementation, seen missing; the same change is never recorded twice, and the turn SETTLED.
    let dir = tempfile::tempdir()?;
    let (hold, suites) = (dir.path().join("hold"), dir.path().join("suites"));
    std::fs::create_dir_all(&suites)?;
    let (world, host, observed) = claude_versions_world(&hold, &suites).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        developer_session(&project, &run_id).await?;
        ready(&project, &run_id, &[("T-RETARGET", "developer")])?;
        let attempt = "T-RETARGET/attempt-1";
        gate_verified(&project, attempt).await?;
        let versions = world.dir.path().join("claude-versions");
        let link = world.bin().join("claude");
        std::os::unix::fs::symlink(versions.join("B"), world.bin().join(".claude-new"))?;
        std::fs::rename(world.bin().join(".claude-new"), &link)?;
        let first = observed_count(&observed, attempt, "IMPLEMENTATION_CHANGED_IN_TURN", 1).await?;
        assert_eq!(first[0]["file"], "launcher", "{first:?}");
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::fs::remove_file(versions.join("A"))?;
        let both = observed_count(&observed, attempt, "IMPLEMENTATION_CHANGED_IN_TURN", 2).await?;
        assert_eq!(
            (&both[1]["file"], &both[1]["seen"]),
            (&json!("implementation"), &json!("missing")),
            "{both:?}"
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
        std::fs::write(&hold, "")?;
        until(&project, &ended(attempt), "SETTLED/", limit).await?;
        let diagnostics: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
            .into_iter()
            .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_IN_TURN")
            .collect();
        assert_eq!(diagnostics, both, "each change once");
        keep(
            "claude-in-turn",
            &json!({"diagnostics": diagnostics, "attempt": attempt_row(&project, attempt)?}),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(&hold, "");
    let _ = std::fs::write(suites.join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_turn_whose_agent_files_did_not_change_records_no_change() -> R {
    // docs/adr/ADR-030.md decision 9's controls ("不记录的"): while a claude turn runs (held for more than a second, so
    // several re-checks ran), a file of its install directory that is in no identity is written (claude-versions/C),
    // and in another turn nothing changes: both SETTLED, neither has an IMPLEMENTATION_CHANGED_IN_TURN.
    let dir = tempfile::tempdir()?;
    let (hold, suites) = (dir.path().join("hold"), dir.path().join("suites"));
    std::fs::create_dir_all(&suites)?;
    let (world, host, observed) = claude_versions_world(&hold, &suites).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        developer_session(&project, &run_id).await?;
        ready(
            &project,
            &run_id,
            &[("T-OTHER-FILE", "developer"), ("T-NOTHING", "developer")],
        )?;
        for attempt in ["T-OTHER-FILE/attempt-1", "T-NOTHING/attempt-1"] {
            gate_verified(&project, attempt).await?;
        }
        std::fs::write(world.dir.path().join("claude-versions/C"), "#!/bin/sh\n")?;
        tokio::time::sleep(Duration::from_millis(1200)).await;
        std::fs::write(&hold, "")?;
        for attempt in ["T-OTHER-FILE/attempt-1", "T-NOTHING/attempt-1"] {
            until(&project, &ended(attempt), "SETTLED/", limit).await?;
            assert_eq!(
                observed_of(&observed, attempt, "IMPLEMENTATION_CHANGED_IN_TURN"),
                Vec::<Value>::new()
            );
            let recorded = attempt_events(&project, attempt, "adapter.diagnostic")?;
            assert!(
                !recorded
                    .iter()
                    .any(|d| d["reason"] == "IMPLEMENTATION_CHANGED_IN_TURN"),
                "{attempt}: {recorded:?}"
            );
        }
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(&hold, "");
    let _ = std::fs::write(suites.join("release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The store's clock moved on by a test (ADR-020's backoffs reached without waiting them out).
#[derive(Debug, Default)]
struct FakeClock(std::sync::Mutex<Duration>);

impl FakeClock {
    fn advance(&self, by: Duration) {
        if let Ok(mut ahead) = self.0.lock() {
            *ahead += by;
        }
    }
}

impl hoh_core::delivery::Clock for FakeClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now() + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_metadata_only_change_before_the_first_input_is_retried_to_the_cap() -> R {
    // docs/adr/ADR-030.md decision 9 ("只有身份戳不符时是瞬时类", "再核对的界限", M2T15D-302): the developer pi sets its
    // own implementation to the mode it has before it answers get_state, in every attempt: no prompt, FAILED
    // COMPAT_UNVERIFIED with stamp_only, transient: READY with a task.retry_scheduled; the store's clock moved to each
    // next_retry_at (once the registry looked at pi's changed stamps again, as it does within a tick or two of a real
    // backoff of seconds; each attempt's pi answers only after that look, so a backoff drawn shorter than the look
    // under load does not bring its retry due before it: that retry would be refused by prepare, "files changed since
    // the last look", a path of the look's timing this test does not claim), the ninth failed attempt (MAX_RETRIES =
    // 8) leaves the task BLOCKED(NEED_DECISION) with one need_decision{reason: retries_exhausted} and no next_retry_at
    // (ADR-020, TEST-042-03's exit); moving the clock on, nothing more is dispatched: exactly nine attempts, all FAILED.
    let world = World::new()?;
    world.claude(None)?;
    world.pi()?;
    let project = world.project();
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/config.toml"),
        "config_version = \"1.0\"\n[team.developer]\nagent = \"pi\"\n",
    )?;
    world.bind_installed()?;
    for agent in [
        hoh_agents::compat::Agent::Claude,
        hoh_agents::compat::Agent::Pi,
    ] {
        hoh_app::compat::register(&project, agent, std::ffi::OsStr::new(&world.path()))?;
    }
    let clock = std::sync::Arc::new(FakeClock::default());
    let registry = world.registry();
    let mut config = world.config_with(
        hoh_core::config::Config::default(),
        registry.clone(),
        None,
        None,
    );
    config.clock = hoh_app::host::HostClock(clock.clone());
    let host = start_when_free(config).await?;
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        developer_session(&project, &run_id).await?;
        ready(&project, &run_id, &[("T-CHMOD", "developer")])?;
        let task = "SELECT state || '/' || coalesce(blocked_reason, '') || '/' || coalesce(next_retry_at, '') FROM tasks \
                    WHERE task_id = 'T-CHMOD'";
        let verified = || {
            matches!(registry.fact(hoh_agents::compat::Agent::Pi), Some(hoh_core::staffing::Fact::Usable(u))
                if u.compat == hoh_core::staffing::Compat::Verified)
        };
        // The registry's fact of pi verified with the stamps pi has now: it looked at pi since its last change.
        let looked = || -> R<bool> {
            use std::os::unix::fs::MetadataExt as _;
            let now = std::fs::metadata(world.bin().join("pi"))?;
            Ok(matches!(registry.fact(hoh_agents::compat::Agent::Pi), Some(hoh_core::staffing::Fact::Usable(u))
                if u.compat == hoh_core::staffing::Compat::Verified
                    && (u.stamp.implementation.ctime_sec, u.stamp.implementation.ctime_nsec)
                        == (now.ctime(), now.ctime_nsec())))
        };
        let d = world.bin().join("pi.d");
        let mut moves = Vec::new();
        let end = Instant::now() + Duration::from_secs(240);
        while query(&project, task)? != "BLOCKED/NEED_DECISION/" {
            if Instant::now() > end {
                return Err(format!("not capped: {}; moves {moves:?}", query(&project, task)?).into());
            }
            // An attempt's pi that changed its mode answers once the registry looked at it again.
            for entry in std::fs::read_dir(&d)?.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if let Some(n) = name.strip_prefix("chmodded.") {
                    let go = d.join(format!("chmod-go.{n}"));
                    if !go.exists() && looked()? {
                        std::fs::write(go, "")?;
                    }
                }
            }
            let busy = query(
                &project,
                "SELECT count(*) FROM attempts WHERE task_id = 'T-CHMOD' \
                 AND state IN ('PREPARED', 'STARTING', 'RUNNING')",
            )?;
            let next = query(
                &project,
                "SELECT coalesce(next_retry_at, '') FROM tasks WHERE task_id = 'T-CHMOD' AND state = 'READY'",
            )?;
            if busy == "0"
                && verified()
                && let Some(next) = hoh_core::time::parse_rfc3339(&next)
                && let Ok(ahead) =
                    next.duration_since(hoh_core::delivery::Clock::now(clock.as_ref()))
            {
                clock.advance(ahead + Duration::from_secs(1));
                moves.push(json!({"to": hoh_core::time::rfc3339_utc(next), "by_s": ahead.as_secs() + 1}));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        clock.advance(Duration::from_secs(3_600));
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(query(&project, task)?, "BLOCKED/NEED_DECISION/");
        let attempts: Vec<(String, String)> = serde_json::from_str::<Vec<Vec<String>>>(&query(
            &project,
            "SELECT json_group_array(json_array(attempt_id, state || '/' || coalesce(failure_reason, ''))) \
             FROM (SELECT * FROM attempts WHERE task_id = 'T-CHMOD' ORDER BY rowid)",
        )?)?
        .into_iter()
        .map(|row| (row[0].clone(), row[1].clone()))
        .collect();
        assert_eq!(attempts.len(), 9, "{attempts:?}");
        for (attempt, end) in &attempts {
            assert_eq!(end, "FAILED/COMPAT_UNVERIFIED", "{attempt}");
            assert_eq!(received(&world, "pi", attempt, "type")?, ["get_state"], "{attempt}");
            let refused: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
                .into_iter()
                .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_BEFORE_INPUT")
                .collect();
            assert!(
                refused.len() == 1 && refused[0]["stamp_only"] == true,
                "{attempt}: {refused:?}"
            );
        }
        let count = |kind: &str, reason: Option<&str>| {
            query(
                &project,
                &format!(
                    "SELECT count(*) FROM events WHERE kind = '{kind}' \
                     AND json_extract(payload_json, '$.task_id') = 'T-CHMOD'{}",
                    reason.map_or(String::new(), |r| format!(
                        " AND json_extract(payload_json, '$.reason') = '{r}'"
                    ))
                ),
            )
        };
        assert_eq!(count("task.retry_scheduled", None)?, "8");
        assert_eq!(count("need_decision", Some("retries_exhausted"))?, "1");
        assert_eq!(count("task.blocked", Some("COMPAT_UNVERIFIED"))?, "0");
        keep(
            "metadata-only-cap",
            &json!({"attempts": attempts, "clock_moves": moves,
                    "task": query(&project, "SELECT json_object('state', state, 'blocked_reason', blocked_reason, \
                                   'attempt_count', attempt_count, 'next_retry_at', next_retry_at) FROM tasks \
                                   WHERE task_id = 'T-CHMOD'")?}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// `hoh doctor --output json` in the world's project, its agents those on the world's PATH, its home the world's own.
fn doctor_json(world: &World) -> R<Value> {
    let home = world.dir.path().join("home");
    std::fs::create_dir_all(&home)?;
    let out = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(world.project())
        .env_clear()
        .env("HOME", &home)
        .env("TMPDIR", world.dir.path())
        .env("PATH", world.path())
        .env("NO_COLOR", "1")
        .output()?;
    serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&out.stdout)).into())
}

/// The project's writer lease row, as JSON (`null` for none).
fn writer_lease(project: &Path) -> R<Value> {
    Ok(serde_json::from_str(&query(
        project,
        "SELECT coalesce((SELECT json_object('attempt_id', attempt_id, 'epoch', epoch, 'expires_at', expires_at) \
         FROM leases WHERE kind = 'writer'), 'null')",
    )?)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_027_04_a_new_codex_waits_for_the_suite_outside_the_write_lease() -> R {
    // AC-027-04 (docs/adr/ADR-030.md decisions 9, 10 and the test plan's TEST-027-04): the developer is bound to codex A,
    // registered. The writer task T1 (here T1-HOLD-TURN: its turn writes its first frame and waits) RUNNING, codex on
    // the PATH is replaced by B (the same script with one more line: another identity, the same version) by a rename, and
    // the reader task T2 imported. T2's first attempt is refused before B runs (FAILED COMPAT_UNVERIFIED,
    // attempt.start_refused), T2 BLOCKED(COMPAT_UNVERIFIED); doctor says codex's protocol is unknown; B's suite is
    // deferred while T1 holds the writer lease (compat.suite_deferred naming T1's attempt, compat.suite_opened, no
    // compat.suite_started), the manifest still A's. T1's turn, A, recorded B's arrival (IMPLEMENTATION_CHANGED_IN_TURN,
    // the re-check only records) and, released, SETTLED: its writer lease row stays, of the epoch it was granted at,
    // expiring at T1's end (released by the settle, not deleted, -01); B's suite starts after T1's settle, no writer is
    // granted from its opening to its PASS, and the manifest's codex is B's, PASS, of this build's suite set and
    // schema-valid. Then (-51) the developer is rebound (epoch + 1), T2 unblocked, and its next attempt runs B in a new
    // session whose granted identity is B's.
    let points = std::sync::Arc::new(Points::default());
    let (observed, hook) = observed();
    let (world, host, _, _) = codex_world_with(&points, Some(hook)).await?;
    let project = world.project();
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        let limit = Duration::from_secs(60);
        let (s1, e1) = developer_session(&project, &run_id).await?;
        let manifest_path = project.join(".hoh/compat-manifest.json");
        let manifest_before: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        let sha_a = file_sha256(&world.bin().join("codex"))?;
        assert_eq!(manifest_before["agents"]["codex"]["implementation_sha256"], json!(sha_a));
        ready(&project, &run_id, &[])?;
        exec(
            &project,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T1-HOLD-TURN', '{run_id}', 's1', 'READY', 1, 'developer', 1, 1, 1);"
            ),
        )?;
        let t1 = "T1-HOLD-TURN/attempt-1";
        let end = Instant::now() + limit;
        while !received(&world, "codex", t1, "method")?
            .iter()
            .any(|m| m == "turn/start")
        {
            if Instant::now() > end {
                return Err("T1's turn never started".into());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let lease_granted = writer_lease(&project)?;
        assert_eq!(lease_granted["attempt_id"], t1, "{lease_granted}");
        let staged = stage_codex_b(&world)?;
        let sha_b = file_sha256(&staged)?;
        std::fs::rename(&staged, world.bin().join("codex"))?;
        observed_count(&observed, t1, "IMPLEMENTATION_CHANGED_IN_TURN", 1).await?;
        ready(&project, &run_id, &[("T2", "developer")])?;
        until(&project, &ended("T2/attempt-1"), "FAILED/COMPAT_UNVERIFIED", limit).await?;
        assert_eq!(
            attempt_events(&project, "T2/attempt-1", "attempt.start_refused")?.len(),
            1
        );
        assert_eq!(
            query(
                &project,
                "SELECT state || '/' || coalesce(blocked_reason, '') FROM tasks WHERE task_id = 'T2'"
            )?,
            "BLOCKED/COMPAT_UNVERIFIED"
        );
        let doctor = doctor_json(&world)?;
        assert_eq!(doctor["agents"]["codex"]["protocol"], "unknown", "{doctor}");
        // B's suite deferred while T1 holds the writer lease.
        let end = Instant::now() + limit;
        let deferred = loop {
            let events = suite_events(&project)?;
            if let Some((_, _, payload)) = events.iter().find(|(_, kind, payload)| {
                kind == "compat.suite_deferred" && payload["implementation_sha256"] == json!(sha_b)
            }) {
                break payload.clone();
            }
            if Instant::now() > end {
                return Err(format!("B's suite was never deferred: {events:?}").into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(
            (&deferred["agent"], &deferred["lease_attempt_id"]),
            (&json!("codex"), &json!(t1)),
            "{deferred}"
        );
        let events = suite_events(&project)?;
        assert!(kinds(&events).contains(&"compat.suite_opened"), "{events:?}");
        assert!(!kinds(&events).contains(&"compat.suite_started"), "{events:?}");
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(&manifest_path)?)?,
            manifest_before
        );
        // B ran no turn: only the looks' and the suite's commands (`--version`, a `--help`, the App Server's schema, the
        // login check), with no attempt's marker. One process of B's may have one, and only with the product's
        // guarantee for it (docs/adr/ADR-031.md 实施记录's adoption-wake rule): the Commander's turn the run's adoption
        // of its specification wakes is a new codex attempt too, and its gate may verify A just before the rename
        // while bash then opens the script by its path, B's — ADR-030 decision 9's R2, which the owner accepted
        // (2026-11-13) together with the re-check before the first input. So that process must be the adoption wake's
        // Commander attempt's, sent `initialize` and nothing else (no `initialized`, `thread/start` or `turn/start`),
        // and the attempt must end FAILED COMPAT_UNVERIFIED with one adapter.diagnostic
        // {IMPLEMENTATION_CHANGED_BEFORE_INPUT}. Every other B call with a marker is a turn of B's.
        let b_calls = || -> R<Vec<String>> {
            Ok(std::fs::read_to_string(world.bin().join("codex.d/calls"))?
                .lines()
                .filter(|l| l.starts_with("B "))
                .map(str::to_owned)
                .collect())
        };
        let adoption = adoption_wake_attempts(&project, &run_id)?;
        let mut adoption_b: Vec<String> = Vec::new();
        for call in b_calls()? {
            let Some(args) = call.strip_prefix("B  ") else {
                let attempt = call
                    .split(' ')
                    .nth(1)
                    .and_then(|marker| marker.split_once('/'))
                    .map(|(_, attempt)| attempt.to_owned())
                    .unwrap_or_default();
                if !adoption.contains(&attempt) || !adoption_b.is_empty() {
                    return Err(format!("a turn of B's: {call} (adoption wake: {adoption:?})").into());
                }
                adoption_b.push(attempt);
                continue;
            };
            assert!(
                args == "--version"
                    || args == "login status"
                    || args.ends_with("--help")
                    || args.starts_with("app-server generate-json-schema"),
                "{call}"
            );
        }
        for attempt in &adoption_b {
            until(&project, &ended(attempt), "FAILED/COMPAT_UNVERIFIED", limit).await?;
            let sent: Vec<Value> = received_by_attempt(&world.bin().join("codex.d"))?
                .into_iter()
                .filter(|(of, _)| of == attempt)
                .map(|(_, message)| message["method"].clone())
                .collect();
            assert_eq!(sent, [json!("initialize")], "{attempt}");
            let changed: Vec<Value> = attempt_events(&project, attempt, "adapter.diagnostic")?
                .into_iter()
                .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_BEFORE_INPUT")
                .collect();
            assert_eq!(changed.len(), 1, "{attempt}: {changed:?}");
        }
        // T1 released: it SETTLED as A, having recorded B's arrival once.
        std::fs::write(world.bin().join("codex.d/release"), "")?;
        until(&project, &ended(t1), "SETTLED/", limit).await?;
        let t1_calls = codex_calls(&world, t1);
        assert!(t1_calls.len() == 1 && t1_calls[0].starts_with("A "), "{t1_calls:?}");
        let in_turn: Vec<Value> = attempt_events(&project, t1, "adapter.diagnostic")?
            .into_iter()
            .filter(|d| d["reason"] == "IMPLEMENTATION_CHANGED_IN_TURN")
            .collect();
        assert_eq!(in_turn.len(), 1, "{in_turn:?}");
        assert_eq!(in_turn[0]["file"], "implementation");
        let lease_released = writer_lease(&project)?;
        let t1_ended = query(&project, &format!("SELECT ended_at FROM attempts WHERE attempt_id = '{t1}'"))?;
        assert_eq!(
            (&lease_released["attempt_id"], &lease_released["epoch"], &lease_released["expires_at"]),
            (&json!(t1), &lease_granted["epoch"], &json!(t1_ended)),
            "{lease_released}"
        );
        // B's suite runs after T1's settle, with no writer granted until its PASS.
        until(
            &project,
            "SELECT count(*) FROM events WHERE kind = 'compat.suite_completed'",
            "1",
            limit,
        )
        .await?;
        let events = suite_events(&project)?;
        let seq_of = |kind: &str| {
            events
                .iter()
                .find(|(_, k, payload)| k == kind && (kind != "compat.suite_ended" || payload["status"] == "PASS"))
                .map(|(seq, _, _)| *seq)
                .ok_or_else(|| format!("no {kind}: {events:?}"))
        };
        let settled: i64 = query(
            &project,
            &format!("SELECT seq FROM events WHERE event_id = '{t1}/SETTLED'"),
        )?
        .parse()?;
        let (opened, started_at, passed) = (
            seq_of("compat.suite_opened")?,
            seq_of("compat.suite_started")?,
            seq_of("compat.suite_ended")?,
        );
        assert!(started_at > settled, "the suite started (seq {started_at}) before T1 settled ({settled})");
        let writer_grants = |within: &str| {
            query(
                &project,
                &format!(
                    "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
                     AND json_extract(payload_json, '$.lease.kind') = 'writer' AND {within}"
                ),
            )
        };
        assert_eq!(
            writer_grants(&format!("json_extract(payload_json, '$.attempt_id') = '{t1}'"))?,
            "1",
            "T1's own grant is a writer dispatch"
        );
        assert_eq!(
            writer_grants(&format!("event_seq > {opened} AND event_seq < {passed}"))?,
            "0",
            "a writer was granted while the suite was open"
        );
        // The writer row now: still T1's released one, or a writer granted after the suite's PASS.
        let lease_after_suite = writer_lease(&project)?;
        if lease_after_suite != lease_released {
            let holder = lease_after_suite["attempt_id"].as_str().unwrap_or_default();
            let granted: i64 = query(
                &project,
                &format!(
                    "SELECT min(event_seq) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch'                      AND json_extract(payload_json, '$.attempt_id') = '{holder}'"
                ),
            )?
            .parse()?;
            assert!(granted > passed, "{lease_after_suite} granted at seq {granted}, the suite passed at {passed}");
        }
        let manifest_after: Value = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
        let codex = &manifest_after["agents"]["codex"];
        assert_eq!(codex["implementation_sha256"], json!(sha_b));
        assert_eq!(codex["suite"]["status"], "PASS");
        assert_eq!(
            codex["suite"]["fixture_set_sha256"],
            json!(hoh_agents::compat::SUITE_SET_SHA256.as_str())
        );
        if let Err(errors) = hoh_core::schema::compat_manifest().validate(&manifest_after) {
            return Err(format!("the manifest is not schema-valid: {errors:?}").into());
        }
        // -51: rebound, T2 unblocked, its next attempt B's in a new session of B's identity.
        until(&project, &ended("T2/attempt-2"), "SETTLED/", limit).await?;
        let rebound = query(
            &project,
            &format!(
                "SELECT json_extract(payload_json, '$.cause') || '/' || json_extract(payload_json, '$.to.role_epoch') \
                 FROM events WHERE kind = 'team.binding_applied' AND event_id = '{run_id}/team/developer/{}'",
                e1 + 1
            ),
        )?;
        assert_eq!(rebound, format!("rebind/{}", e1 + 1));
        assert_eq!(
            query(
                &project,
                "SELECT count(*) FROM events WHERE kind = 'task.unblocked' \
                 AND json_extract(payload_json, '$.task_id') = 'T2'"
            )?,
            "1"
        );
        let t2 = attempt_row(&project, "T2/attempt-2")?;
        assert_ne!(t2["session_id"], json!(s1));
        assert_eq!(t2["role_epoch"], json!(e1 + 1));
        let session = t2["session_id"].as_str().unwrap_or_default();
        assert_eq!(
            query(
                &project,
                &format!(
                    "SELECT json_extract(payload_json, '$.identity.implementation_sha256') FROM events \
                     WHERE kind = 'team.session_bound' AND json_extract(payload_json, '$.session_id') = '{session}'"
                )
            )?,
            sha_b
        );
        let t2_calls = codex_calls(&world, "T2/attempt-2");
        assert!(t2_calls.len() == 1 && t2_calls[0].starts_with("B "), "{t2_calls:?}");
        keep(
            "test-027-04",
            &json!({
                "manifest_before": manifest_before, "manifest_after": manifest_after,
                "attempts": [attempt_row(&project, t1)?, attempt_row(&project, "T2/attempt-1")?, t2],
                "leases": {"granted": lease_granted, "released": lease_released, "after_suite": lease_after_suite},
                "doctor": doctor, "suite_events": events.iter().map(|(seq, kind, payload)| json!([seq, kind, payload]))
                    .collect::<Vec<_>>(),
                "t1_in_turn": in_turn, "b_calls": b_calls()?,
            }),
        )?;
        R::Ok(())
    }
    .await;
    let _ = std::fs::write(world.bin().join("codex.d/release"), "");
    host.shutdown().await;
    world.stop_fakes()?;
    outcome
}

/// The host of `world` with an inactivity watchdog of 2 s, looked at every 100 ms (docs/adr/ADR-022.md decision 19).
fn watched(world: &World) -> HostConfig {
    let mut config = world.config();
    config.watchdog = hoh_app::host::Watchdog {
        quiet: Duration::from_secs(2),
        every: Duration::from_millis(100),
    };
    config
}

/// The query of how many events of `kind` attempt `attempt` has.
fn watchdog_events(attempt: &str, kind: &str) -> String {
    format!(
        "SELECT count(*) FROM events WHERE kind = '{kind}' AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
    )
}

/// The payload of attempt `attempt`'s event of `kind`.
fn watchdog_event(project: &Path, attempt: &str, kind: &str) -> R<Value> {
    Ok(serde_json::from_str(&query(
        project,
        &format!(
            "SELECT payload_json FROM events WHERE kind = '{kind}' \
             AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
        ),
    )?)?)
}

/// Wait until `path` exists, for at most `limit`.
async fn until_file(path: &Path, limit: Duration) -> R {
    let end = Instant::now() + limit;
    while !path.exists() {
        if Instant::now() > end {
            return Err(format!("no {}", path.display()).into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

/// Kill the WATCH turn's tool if it still runs (it is not the fakes' to stop: its command line is `sleep 600`).
fn stop_watch_tool(world: &World) {
    let Ok(pid) = std::fs::read_to_string(world.bin().join("claude.d/watch-tool")) else {
        return;
    };
    let pid = pid.trim();
    if std::fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c == b"sleep\x00600\x00") {
        let _ = Command::new("kill").args(["-KILL", pid]).status();
    }
}

#[tokio::test]
async fn a_silent_turn_is_inactive_once_only_with_no_tool_running_and_its_output_clears_it() -> R {
    // REQ-042 behaviour 8 (docs/adr/ADR-022.md decision 19): silent past the quiet time while its tool runs, nothing;
    // silent with no tool, one attempt.inactive and the attempt runs on; its output then clears it. A residual
    // (decision 19 item 12, WDF-50): a live process of this user started after the agent whose environment cannot be
    // read (PR_SET_DUMPABLE=0, such as the bundle self-test's "hidden" child) counts as a tool however unrelated, so
    // while one runs on this machine the quiet time never completes and this test fails; processes that are only
    // exiting no longer count (the correction of 2026-11-17).
    let world = World::new()?;
    world.claude(None)?;
    world.bind_installed()?;
    let host = start_when_free(watched(&world)).await?;
    let project = world.project();
    let d = world.bin().join("claude.d");
    let attempt = "T-WATCH/attempt-1";
    let outcome = async {
        let run_id = started(&host, &project, "s1").await?;
        ready(&project, &run_id, &[("T-WATCH", "developer")])?;
        until_file(&d.join("watch-tool"), Duration::from_secs(60)).await?;
        tokio::time::sleep(Duration::from_secs(5)).await;
        let inactive = watchdog_events(attempt, "attempt.inactive");
        let cleared = watchdog_events(attempt, "attempt.inactive_cleared");
        assert_eq!(
            query(&project, &inactive)?,
            "0",
            "inactive while its tool ran"
        );
        std::fs::write(d.join("watch-tool-end"), "")?;
        until_file(&d.join("watch-alone"), Duration::from_secs(30)).await?;
        until(&project, &inactive, "1", Duration::from_secs(30)).await?;
        let event = watchdog_event(&project, attempt, "attempt.inactive")?;
        assert_eq!(
            (
                &event["episode"],
                &event["tool_running"],
                &event["quiet_ms"]
            ),
            (&json!(1), &json!(false), &json!(2000)),
            "{event}"
        );
        let since = event["since_ms"].as_u64().ok_or("no since_ms")?;
        // At least the quiet time since the output was last seen, whatever the load (WD-51: a bound from the first
        // frame's time depends on when a look saw it; that the quiet time counts from the tool's last sighting is the
        // Watch's unit test).
        assert!(since >= 2000, "{event}");
        // Once per episode, and nothing is killed: the attempt runs on, its agent alive.
        tokio::time::sleep(Duration::from_secs(5)).await;
        assert_eq!(query(&project, &inactive)?, "1");
        assert_eq!(query(&project, &cleared)?, "0");
        assert_eq!(
            query(
                &project,
                &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'")
            )?,
            "RUNNING"
        );
        // Its output clears it while it runs on, and the turn then ends as it would have.
        std::fs::write(d.join("watch-speak"), "")?;
        until(&project, &cleared, "1", Duration::from_secs(30)).await?;
        assert_eq!(
            query(
                &project,
                &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'")
            )?,
            "RUNNING"
        );
        std::fs::write(d.join("watch-end"), "")?;
        until(
            &project,
            &ended(attempt),
            "SETTLED/",
            Duration::from_secs(60),
        )
        .await?;
        let clear = watchdog_event(&project, attempt, "attempt.inactive_cleared")?;
        assert_eq!(clear["episode"], json!(1), "{clear}");
        assert!(
            clear["inactive_ms"].as_u64().ok_or("no inactive_ms")? >= 5000,
            "{clear}"
        );
        let ids = query(
            &project,
            &format!(
                "SELECT group_concat(event_id, ' ') FROM (SELECT event_id FROM events \
                 WHERE event_id LIKE '{attempt}/watchdog/%' ORDER BY seq)"
            ),
        )?;
        assert_eq!(
            ids,
            format!("{attempt}/watchdog/1/inactive {attempt}/watchdog/1/cleared")
        );
        keep(
            "watchdog-episode",
            &json!({"inactive": event, "cleared": clear, "ids": ids}),
        )?;
        R::Ok(())
    }
    .await;
    host.shutdown().await;
    stop_watch_tool(&world);
    world.stop_fakes()?;
    outcome
}

#[tokio::test]
async fn an_episode_open_when_its_host_stops_is_neither_repeated_nor_cleared_by_the_next_host() -> R
{
    // Decision 19 item 9: the watch lives in its host. The next host's start reconcile ends the attempt (decision 17);
    // it writes no watchdog event for it, the open episode ends with the attempt.
    let world = World::new()?;
    world.claude(None)?;
    world.bind_installed()?;
    let d = world.bin().join("claude.d");
    // Its tool ends at once: the turn is silent with no tool from its first frame on.
    std::fs::write(d.join("watch-tool-end"), "")?;
    let project = world.project();
    let attempt = "T-WATCH/attempt-1";
    let inactive = watchdog_events(attempt, "attempt.inactive");
    let first = start_when_free(watched(&world)).await?;
    let outcome = async {
        let run_id = started(&first, &project, "s1").await?;
        ready(&project, &run_id, &[("T-WATCH", "developer")])?;
        until(&project, &inactive, "1", Duration::from_secs(60)).await
    }
    .await;
    first.shutdown().await;
    let outcome = match outcome {
        Ok(()) => {
            let stopped_at = query(&project, "SELECT max(seq) FROM events")?;
            let second = start_when_free(watched(&world)).await?;
            let outcome = async {
                until(
                    &project,
                    &format!("SELECT state <> 'RUNNING' FROM attempts WHERE attempt_id = '{attempt}'"),
                    "1",
                    Duration::from_secs(60),
                )
                .await?;
                tokio::time::sleep(Duration::from_secs(5)).await;
                assert_eq!(
                    query(
                        &project,
                        &format!("SELECT group_concat(event_id) FROM events WHERE event_id LIKE '{attempt}/watchdog/%'")
                    )?,
                    format!("{attempt}/watchdog/1/inactive")
                );
                assert_eq!(
                    query(
                        &project,
                        &format!(
                            "SELECT count(*) FROM events WHERE seq > {stopped_at} AND kind LIKE 'attempt.inactive%' \
                             AND json_extract(payload_json, '$.attempt_id') = '{attempt}'"
                        )
                    )?,
                    "0"
                );
                R::Ok(())
            }
            .await;
            second.shutdown().await;
            outcome
        }
        Err(e) => Err(e),
    };
    stop_watch_tool(&world);
    world.stop_fakes()?;
    outcome
}
