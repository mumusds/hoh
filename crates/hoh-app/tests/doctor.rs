//! `hoh doctor` without `--live` (REQ-006, ADR-015) against programmable fake CLIs in a PATH sandbox:
//! - TEST-006-01: the five-row table (a CLI missing, an old version lacking what HoH needs, compatible but not logged in,
//!   compatible and logged in, a status command that hangs), as fields and exit codes.
//! - TEST-006-02: canary credentials in the environment and in isolated credential files, echoed by the status
//!   commands. They appear in no output or written file, and strace sees no open of those files.
//!
//! The fakes answer the probe from the recorded help texts and schema, and their status commands as the real CLIs do
//! (ADR-015 decision 3). The compatible fakes' hashes are registered in a PASS compat manifest under the project's
//! `.hoh/`.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::compat::{Agent, CompatManifest, SuiteStatus};
use hoh_agents::probe::{ProbeEnv, probe};
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol")
}

/// How a fake's status command answers.
#[derive(Clone, Copy, PartialEq)]
enum Status {
    LoggedIn,
    NotLoggedIn,
    /// Sleeps 60 s, with a child in its group.
    Hangs,
    /// Exits as soon as a helper it started is in a session of its own (pid in `$HOME/helper.pid`), leaving
    /// `$HOME/launcher.exited`. The helper holds both pipes: it closes stdout at 12 s and gives the logged-in answer on
    /// stderr at 28 s, past doctor's 20 s limit.
    Escapes,
}

struct Sandbox {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    bin: tempfile::TempDir,
    /// doctor's TMPDIR: the scratch directory `--live` keeps goes here and is removed with the sandbox, also when a
    /// test fails.
    tmp: tempfile::TempDir,
    /// Printed by every status command (a key-like string the doctor must never show).
    canary: Option<String>,
}

impl Sandbox {
    fn new() -> R<Sandbox> {
        let s = Sandbox {
            home: tempfile::tempdir()?,
            project: tempfile::tempdir()?,
            bin: tempfile::tempdir()?,
            tmp: tempfile::tempdir()?,
            canary: None,
        };
        s.write(
            "git",
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'git version 2.43.0';;\n -C) mkdir -p \"$2/.git\";;\n *) exit 1;;\nesac\n",
        )?;
        s.write(
            "node",
            "#!/bin/sh\n[ \"$1\" = --version ] && echo 'v22.20.0'\n",
        )?;
        // pi's default provider, as pi keeps it in its settings (not its credentials).
        let pi_dir = s.home.path().join(".pi/agent");
        std::fs::create_dir_all(&pi_dir)?;
        std::fs::write(
            pi_dir.join("settings.json"),
            r#"{"defaultProvider": "fake-provider", "defaultModel": "m"}"#,
        )?;
        Ok(s)
    }

    fn write(&self, name: &str, script: &str) -> R {
        let path = self.bin.path().join(name);
        std::fs::write(&path, script)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
        Ok(())
    }

    fn remove(&self, name: &str) -> R {
        std::fs::remove_file(self.bin.path().join(name))?;
        Ok(())
    }

    fn echo(&self) -> String {
        self.canary
            .as_deref()
            .map(|c| format!("echo 'token {c}'\n"))
            .unwrap_or_default()
    }

    /// A status answer in shell: `logged`/`not` lines for the two states, or a hang.
    fn status(&self, status: Status, logged: &str, not: &str) -> String {
        match status {
            Status::LoggedIn => format!("{}{logged}", self.echo()),
            Status::NotLoggedIn => format!("{}{not}", self.echo()),
            Status::Hangs => "sleep 60 & sleep 60\n".to_owned(),
            Status::Escapes => format!(
                "setsid sh -s <<'HELPER' &\necho $$ > \"{home}/helper.tmp\"; mv \"{home}/helper.tmp\" \"{home}/helper.pid\"\n\
                 sleep 12\nexec >&-\nsleep 16\n{logged}HELPER\nwhile [ ! -s '{home}/helper.pid' ]; do sleep 0.01; done\n\
                 : > '{home}/launcher.exited'\nexit 0\n",
                home = self.home.path().display()
            ),
        }
    }

    fn codex(&self, status: Status) -> R {
        let f = fixtures().join("codex/0.166.0");
        self.codex_with(
            status,
            &f.join("app-server-schema"),
            &f.join("help-exec.txt"),
        )
    }

    /// The recorded codex with the App Server schema in `schema` and `exec --help` from `exec_help`.
    fn codex_with(&self, status: Status, schema: &Path, exec_help: &Path) -> R {
        let f = fixtures().join("codex/0.166.0");
        let answer = self.status(
            status,
            "echo 'Logged in using ChatGPT' >&2\nexit 0\n",
            "echo 'Not logged in' >&2\nexit 1\n",
        );
        self.write(
            "codex",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
                 exec) [ \"$2\" = --help ] && cat '{exec}';;\n \
                 app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{schema}/.' \"$4/\";;\n  \
                 *) exit 2;;\n esac;;\n login) if [ \"$2\" = status ]; then\n{answer}fi;;\n *) exit 2;;\nesac\n",
                f = f.display(),
                exec = exec_help.display(),
                schema = schema.display()
            ),
        )
    }

    /// A codex that has neither app-server nor `exec --json`.
    fn old_codex(&self) -> R {
        self.write(
            "codex",
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.1.0';;\n \
             --help) printf 'Usage: codex [OPTIONS] <COMMAND>\\n\\nCommands:\\n  exec  Run Codex non-interactively\\n';;\n \
             exec) [ \"$2\" = --help ] && echo 'Usage: codex exec [PROMPT]';;\n \
             *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        )
    }

    fn claude(&self, status: Status) -> R {
        self.claude_with_help(status, &fixtures().join("claude/2.1.318/help.txt"))
    }

    /// A claude whose `--help` is the file `help`.
    fn claude_with_help(&self, status: Status, help: &Path) -> R {
        let answer = self.status(
            status,
            "echo '{\"loggedIn\": true, \"authMethod\": \"claude.ai\"}'\nexit 0\n",
            "echo '{\"loggedIn\": false, \"authMethod\": \"none\"}'\nexit 1\n",
        );
        self.write(
            "claude",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{}';;\n \
                 auth) if [ \"$2\" = status ]; then\n{answer}fi;;\n *) exit 2;;\nesac\n",
                help.display()
            ),
        )
    }

    fn pi(&self, status: Status) -> R {
        self.pi_listing(status, "echo 'No packages installed.'")
    }

    /// A pi whose `list` runs `list` (shell).
    fn pi_listing(&self, status: Status, list: &str) -> R {
        self.pi_with(status, list, &fixtures().join("pi/1.5.0/help.txt"))
    }

    /// A pi whose `list` runs `list` (shell) and whose `--help` is the file `help`.
    fn pi_with(&self, status: Status, list: &str, help: &Path) -> R {
        let answer = self.status(
            status,
            "echo '{\"status\":\"ready\",\"provider\":\"fake-provider\",\"authType\":\"api_key\"}'\nexit 0\n",
            "echo '{\"status\":\"not_ready\",\"provider\":\"fake-provider\",\"reason\":\"credentials_not_configured\"}'\nexit 1\n",
        );
        self.write(
            "pi",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo '1.5.0';;\n --help) cat '{}';;\n \
                 auth) if [ \"$2\" = check ]; then\n{answer}fi;;\n list) {list};;\n *) exit 2;;\nesac\n",
                help.display()
            ),
        )
    }

    fn path(&self) -> String {
        format!("{}:/usr/bin:/bin", self.bin.path().display())
    }

    /// Register the installed fakes of `agents` as compatible (suite PASS) in the project's compat manifest.
    fn register(&self, agents: &[Agent]) -> R {
        let env = ProbeEnv {
            path: self.path().into(),
            fixture_root: Some(fixtures()),
            timeout: Duration::from_secs(30),
            log: None,
            deadline: None,
        };
        let mut entries = BTreeMap::new();
        for agent in agents {
            let mut entry = probe(*agent, &env)?;
            entry.suite.status = SuiteStatus::Pass;
            entries.insert(*agent, entry);
        }
        let dir = self.project.path().join(".hoh");
        std::fs::create_dir_all(&dir)?;
        std::fs::write(
            dir.join("compat-manifest.json"),
            serde_json::to_vec_pretty(&CompatManifest::new(entries))?,
        )?;
        Ok(())
    }

    fn doctor_in(
        &self,
        wrapper: &[&str],
        args: &[&str],
        env: &[(&str, &str)],
    ) -> R<(Output, Duration)> {
        let hoh = env!("CARGO_BIN_EXE_hoh");
        let mut cmd = match wrapper.split_first() {
            Some((program, rest)) => {
                let mut c = Command::new(program);
                c.args(rest).arg(hoh);
                c
            }
            None => Command::new(hoh),
        };
        cmd.arg("doctor")
            .args(args)
            .current_dir(self.project.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("TMPDIR", self.tmp.path())
            .env("PATH", self.path())
            .env("NO_COLOR", "1");
        for (k, v) in env {
            cmd.env(k, v);
        }
        let started = Instant::now();
        let out = cmd.output()?;
        Ok((out, started.elapsed()))
    }

    fn doctor(&self) -> R<(Output, Value, Duration)> {
        let (out, took) = self.doctor_in(&[], &["--output", "json"], &[])?;
        let report: Value = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&out.stdout)))?;
        Ok((out, report, took))
    }
}

/// Every process whose command line mentions `dir`.
fn processes_mentioning(dir: &Path) -> Vec<String> {
    let needle = dir.display().to_string();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .chars()
                .all(|c| c.is_ascii_digit())
        })
        .filter_map(|e| std::fs::read(e.path().join("cmdline")).ok())
        .map(|c| String::from_utf8_lossy(&c).replace('\0', " "))
        .filter(|c| c.contains(&needle))
        .collect()
}

/// An installed CLI its adapter cannot start is incompatible and blocks (exit 3), for every agent and not only codex
/// (REQ-006; M1R-03, M1T07R-14): claude without `-p`, `--verbose` or `--output-format`, pi without `--session-id` or
/// `--session-dir`. Only the option's declaration is removed (a flag named in another option's text is not offered).
/// The probe refuses such a CLI, so no compat manifest entry can make it compatible.
#[test]
fn a_cli_without_what_its_adapter_needs_is_incompatible() -> R {
    for (agent, fixture, flag) in [
        ("claude", "claude/2.1.318/help.txt", "--output-format"),
        ("claude", "claude/2.1.318/help.txt", "-p"),
        ("claude", "claude/2.1.318/help.txt", "--verbose"),
        ("pi", "pi/1.5.0/help.txt", "--session-id"),
        ("pi", "pi/1.5.0/help.txt", "--session-dir"),
    ] {
        let s = Sandbox::new()?;
        s.codex(Status::LoggedIn)?;
        let recorded = std::fs::read_to_string(fixtures().join(fixture))?;
        // Without the line that declares the option, indented as declarations are.
        let help: String = recorded
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !(l.len() - t.len() <= 8
                    && t.starts_with('-')
                    && t.split("  ")
                        .next()
                        .unwrap_or(t)
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .any(|w| w.split(['=', '[']).next() == Some(flag)))
            })
            .map(|l| format!("{l}\n"))
            .collect();
        assert_eq!(recorded.lines().count() - help.lines().count(), 1, "{flag}");
        let help_file = s.home.path().join("help.txt");
        std::fs::write(&help_file, help)?;
        if agent == "claude" {
            s.claude_with_help(Status::LoggedIn, &help_file)?;
            s.pi(Status::LoggedIn)?;
        } else {
            s.claude(Status::LoggedIn)?;
            s.pi_with(
                Status::LoggedIn,
                "echo 'No packages installed.'",
                &help_file,
            )?;
        }
        let (out, report, _) = s.doctor()?;
        let item = &report["agents"][agent];
        assert_eq!(item["protocol"], "incompatible", "{agent} {flag}: {item}");
        identified(&s, agent, item)?;
        assert!(
            item["protocol_detail"]
                .as_str()
                .is_some_and(|d| d.contains(flag)),
            "{agent} {flag}: {item}"
        );
        assert_eq!(out.status.code(), Some(3), "{agent} {flag}");
    }
    Ok(())
}

/// An incompatible item still names what doctor found: the `--version` line and the implementation's sha256 (REQ-006,
/// ADR-015 decision 8; M1QA-08). The fakes are their own implementations.
fn identified(s: &Sandbox, agent: &str, item: &Value) -> R {
    let version = Command::new(s.bin.path().join(agent))
        .arg("--version")
        .output()?
        .stdout;
    let version = String::from_utf8(version)?;
    assert_eq!(item["version"], version.trim_end(), "{agent}: {item}");
    let sum = Command::new("sha256sum")
        .arg(s.bin.path().join(agent))
        .output()?
        .stdout;
    let sum = String::from_utf8(sum)?;
    assert_eq!(
        item["binary_sha256"].as_str(),
        sum.split_whitespace().next(),
        "{agent}: {item}"
    );
    Ok(())
}

/// A codex HoH cannot drive is incompatible and blocks (exit 3), as REQ-006 says of one with neither app-server nor
/// `exec --json` (M1R-03, M1R-04): one whose `codex --help` lists neither subcommand, and one whose App Server schema
/// has no `initialized` notification while its exec has no `--json`. The item names the version and hash doctor
/// found (M1QA-08).
#[test]
fn a_codex_without_what_its_adapter_needs_is_incompatible() -> R {
    let f = fixtures().join("codex/0.166.0");
    for (row, missing) in [
        ("only-login", ["exec", "app-server"]),
        ("no-initialized", ["initialized", "--json"]),
    ] {
        let s = Sandbox::new()?;
        s.claude(Status::LoggedIn)?;
        s.pi(Status::LoggedIn)?;
        if row == "only-login" {
            s.write(
                "codex",
                &format!(
                    "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n \
                     --help) grep -v -e '^  exec ' -e '^  app-server ' '{f}/help.txt';;\n \
                     login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
                    f = f.display()
                ),
            )?;
        } else {
            let schema = s.home.path().join("schema");
            std::fs::create_dir(&schema)?;
            assert!(
                Command::new("cp")
                    .arg("-R")
                    .arg(f.join("app-server-schema/."))
                    .arg(&schema)
                    .status()?
                    .success()
            );
            let file = schema.join("ClientNotification.json");
            let text = std::fs::read_to_string(&file)?;
            assert!(text.contains("\"initialized\""));
            std::fs::write(
                &file,
                text.replace("\"initialized\"", "\"initialized-gone\""),
            )?;
            let exec_help = s.home.path().join("exec-help.txt");
            std::fs::write(
                &exec_help,
                std::fs::read_to_string(f.join("help-exec.txt"))?.replace("--json", "--json-lines"),
            )?;
            s.codex_with(Status::LoggedIn, &schema, &exec_help)?;
        }
        let (out, report, _) = s.doctor()?;
        let item = &report["agents"]["codex"];
        assert_eq!(item["protocol"], "incompatible", "{row}: {item}");
        let detail = item["protocol_detail"].as_str().unwrap_or_default();
        assert!(missing.iter().all(|m| detail.contains(m)), "{row}: {item}");
        assert_eq!(out.status.code(), Some(3), "{row}");
        identified(&s, "codex", item)?;
    }
    Ok(())
}

#[test]
fn test_006_01_the_five_rows_of_installed_protocol_and_auth() -> R {
    let mut rows = Vec::new();
    for row in 1..=5 {
        let s = Sandbox::new()?;
        s.claude(Status::LoggedIn)?;
        s.pi(Status::LoggedIn)?;
        match row {
            1 => s.codex(Status::LoggedIn).and_then(|()| s.remove("codex"))?,
            2 => s.old_codex()?,
            3 => s.codex(Status::NotLoggedIn)?,
            4 => s.codex(Status::LoggedIn)?,
            _ => s.codex(Status::Hangs)?,
        }
        let registered: &[Agent] = if row <= 2 {
            &[Agent::Claude, Agent::Pi]
        } else {
            &[Agent::Codex, Agent::Claude, Agent::Pi]
        };
        s.register(registered)?;
        let (out, report, took) = s.doctor()?;
        let codex = &report["agents"]["codex"];
        rows.push((row, out.status.code(), codex.clone(), took));
        // No item is ever verified without --live.
        for agent in ["codex", "claude", "pi"] {
            assert_ne!(
                report["agents"][agent]["auth_check"], "verified",
                "row {row}: {agent}"
            );
        }
        assert_eq!(report["mode"], "default");
        assert_eq!(
            report["summary"]["exit_code"].as_i64(),
            out.status.code().map(i64::from)
        );
        match row {
            1 => {
                assert_eq!(
                    (codex["installed"].as_bool(), codex["auth_check"].as_str()),
                    (Some(false), Some("unknown"))
                );
                assert_eq!(codex["protocol"], "unknown");
                assert!(
                    codex["remedy"].as_str().is_some_and(|r| !r.is_empty()),
                    "an install action"
                );
            }
            2 => {
                assert_eq!(codex["protocol"], "incompatible");
                let detail = codex["protocol_detail"].as_str().unwrap_or_default();
                assert!(
                    detail.contains("app-server") && detail.contains("--json"),
                    "the missing names: {detail}"
                );
            }
            3 => {
                assert_eq!(codex["auth_check"], "missing");
                assert!(
                    codex["remedy"]
                        .as_str()
                        .is_some_and(|r| r.contains("codex login")),
                    "{codex}"
                );
            }
            4 => {
                assert_eq!(
                    (codex["protocol"].as_str(), codex["auth_check"].as_str()),
                    (Some("compatible"), Some("not_verified"))
                );
                assert!(
                    report["summary"]["unverified"]
                        .as_array()
                        .is_some_and(|u| u.iter().any(|v| v == "codex"))
                );
                assert_eq!(codex["version"], "codex-cli 0.166.0");
            }
            _ => {
                assert_eq!(codex["auth_check"], "unknown");
                assert!(
                    report["summary"]["unverified"]
                        .as_array()
                        .is_some_and(|u| u.iter().any(|v| v == "codex"))
                );
                assert!(took <= Duration::from_secs(25), "doctor took {took:?}");
                let left = processes_mentioning(s.bin.path());
                assert!(
                    left.is_empty(),
                    "the hung status command's group is still there: {left:?}"
                );
                let probes = report["probes"].as_array().ok_or("no probes")?;
                assert!(
                    probes.iter().any(|p| p["timed_out"] == true
                        && p["argv"]
                            .as_array()
                            .is_some_and(|a| a.iter().any(|x| x == "login"))),
                    "{probes:?}"
                );
            }
        }
    }
    let codes: Vec<Option<i32>> = rows.iter().map(|r| r.1).collect();
    assert_eq!(
        codes,
        [Some(3), Some(3), Some(3), Some(0), Some(0)],
        "{rows:#?}"
    );
    Ok(())
}

#[test]
fn test_006_02_credentials_are_never_read_or_shown() -> R {
    let canary = format!(
        "sk-CANARY-{}",
        std::fs::read_to_string("/proc/sys/kernel/random/uuid")?
            .trim()
            .replace('-', "")
    );
    let mut s = Sandbox::new()?;
    s.canary = Some(canary.clone());
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    s.register(&[Agent::Codex, Agent::Claude, Agent::Pi])?;
    let codex_auth = s.home.path().join(".codex/auth.json");
    let claude_auth = s.home.path().join(".claude/.credentials.json");
    for file in [&codex_auth, &claude_auth] {
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(
            file,
            format!("{{\"OPENAI_API_KEY\": \"{canary}\", \"token\": \"{canary}\"}}"),
        )?;
    }
    let files = |dirs: &[&Path]| -> R<BTreeMap<PathBuf, Vec<u8>>> {
        let mut out = BTreeMap::new();
        for d in dirs {
            let mut todo = vec![d.to_path_buf()];
            while let Some(dir) = todo.pop() {
                for e in std::fs::read_dir(&dir)? {
                    let p = e?.path();
                    if p.is_dir() {
                        todo.push(p);
                    } else {
                        out.insert(p.clone(), std::fs::read(&p)?);
                    }
                }
            }
        }
        Ok(out)
    };
    let watched = [s.home.path(), s.project.path()];
    let traces = tempfile::tempdir()?;
    let env = [
        ("OPENAI_API_KEY", canary.as_str()),
        ("ANTHROPIC_API_KEY", canary.as_str()),
    ];
    for output in ["json", "text"] {
        let before = files(&watched)?;
        let trace = traces.path().join(format!("{output}.strace"));
        let trace_arg = trace.display().to_string();
        let (out, _) = s.doctor_in(
            &["strace", "-f", "-e", "trace=open,openat", "-o", &trace_arg],
            &["--output", output],
            &env,
        )?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{output}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        for (name, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
            assert!(
                !String::from_utf8_lossy(bytes).contains(&canary),
                "{output}: the canary in {name}"
            );
        }
        let after = files(&watched)?;
        for (path, bytes) in &after {
            if before.get(path) != Some(bytes) && path != &codex_auth && path != &claude_auth {
                assert!(
                    !String::from_utf8_lossy(bytes).contains(&canary),
                    "{output}: the canary in {}",
                    path.display()
                );
            }
        }
        let log = std::fs::read_to_string(&trace)?;
        assert!(log.contains("openat"), "strace recorded nothing");
        for file in [&codex_auth, &claude_auth] {
            assert!(
                !log.contains(&file.display().to_string()),
                "{output}: {} was opened",
                file.display()
            );
        }
        if output == "json" {
            let report: Value = serde_json::from_slice(&out.stdout)?;
            for agent in ["codex", "claude", "pi"] {
                assert_eq!(
                    report["agents"][agent]["auth_check"], "not_verified",
                    "{agent}"
                );
            }
        }
    }
    Ok(())
}

/// One shared limit for every command (M1T07R-02): a pi whose status hangs and whose `list` is slow, and a node whose
/// `--version` hangs, still let doctor end within the limit + 5 s. A tool that does not answer is unknown, not
/// incompatible, so it blocks nothing (M1T07R-03). A codex whose status answer comes past the limit through a pipe a
/// helper holds open is unknown too, not logged in (M1T07R-13).
#[test]
fn slow_and_hung_commands_share_one_limit_and_stay_unknown() -> R {
    let s = Sandbox::new()?;
    s.codex(Status::Escapes)?;
    s.claude(Status::LoggedIn)?;
    s.pi_listing(Status::Hangs, "sleep 6; echo 'No packages installed.'")?;
    s.write("node", "#!/bin/sh\nsleep 60\n")?;
    s.register(&[Agent::Codex, Agent::Pi])?;
    // A claude whose probe commands, run one after another, take 14 s each: 28 s without one shared limit.
    let help = fixtures().join("claude/2.1.318/help.txt");
    s.write(
        "claude",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) sleep 14; echo '2.1.318 (Claude Code)';;\n --help) sleep 14; cat '{}';;\n \
             auth) echo '{{\"loggedIn\": true}}';;\n *) exit 2;;\nesac\n",
            help.display()
        ),
    )?;
    let (out, report, took) = s.doctor()?;
    // The helper left codex's group, out of doctor's reach: stop it here.
    if let Ok(pid) = std::fs::read_to_string(s.home.path().join("helper.pid")) {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", pid.trim())])
            .status();
    }
    assert!(took <= Duration::from_secs(25), "doctor took {took:?}");
    assert!(
        s.home.path().join("launcher.exited").exists(),
        "codex's status launcher did not exit"
    );
    let codex = &report["agents"]["codex"];
    assert_eq!(codex["auth_check"], "unknown", "{codex}");
    assert!(
        codex["remedy"]
            .as_str()
            .is_some_and(|r| r.contains("总时限")),
        "{codex}"
    );
    assert_eq!(
        report["agents"]["claude"]["protocol"], "unknown",
        "{}",
        report["agents"]["claude"]
    );
    // Cut short by the shared limit, and said so (M1T07R-08).
    let detail = report["agents"]["claude"]["protocol_detail"]
        .as_str()
        .unwrap_or_default();
    assert!(detail.contains("总时限"), "{detail}");
    assert_eq!(report["agents"]["pi"]["auth_check"], "unknown");
    let node = &report["runtime"]["node"];
    assert_eq!(
        (node["installed"].as_bool(), node["protocol"].as_str()),
        (Some(true), Some("unknown")),
        "{node}"
    );
    assert!(
        node["detail"].as_str().is_some_and(|d| !d.is_empty()),
        "{node}"
    );
    assert_eq!(out.status.code(), Some(0), "{}", report["summary"]);
    assert!(
        processes_mentioning(s.bin.path()).is_empty(),
        "a hung command is still there"
    );
    Ok(())
}

/// The text form carries what the JSON carries for each agent: path, binary sha256, protocol, auth and capabilities
/// (M1T07R-05).
#[test]
fn the_text_form_shows_each_agents_fields() -> R {
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    s.register(&[Agent::Codex, Agent::Claude, Agent::Pi])?;
    let (_, report, _) = s.doctor()?;
    let (out, _) = s.doctor_in(&[], &["--output", "text"], &[])?;
    let text = String::from_utf8(out.stdout)?;
    for agent in ["codex", "claude", "pi"] {
        let item = &report["agents"][agent];
        for field in ["path", "binary_sha256", "version", "protocol", "auth_check"] {
            let value = item[field].as_str().ok_or(format!("{agent}.{field}"))?;
            assert!(
                text.contains(value),
                "{agent}.{field} {value:?} not in the text:\n{text}"
            );
        }
        for (capability, _) in item["capabilities"].as_object().ok_or("capabilities")? {
            assert!(
                text.contains(capability.as_str()),
                "{agent}: capability {capability} not in the text"
            );
        }
    }
    Ok(())
}

/// `.hoh` taken by a file or a dangling link cannot hold the project's state: not writable, with why (M1T07R-06).
#[test]
fn a_hoh_that_is_not_a_directory_is_reported_unusable() -> R {
    for kind in ["file", "dangling link"] {
        let s = Sandbox::new()?;
        s.codex(Status::LoggedIn)?;
        s.claude(Status::LoggedIn)?;
        s.pi(Status::LoggedIn)?;
        let hoh = s.project.path().join(".hoh");
        match kind {
            "file" => std::fs::write(&hoh, "x")?,
            _ => std::os::unix::fs::symlink(s.project.path().join("gone"), &hoh)?,
        }
        let (_, report, _) = s.doctor()?;
        let dir = &report["runtime"]["project_hoh"];
        assert_eq!(
            (dir["exists"].as_bool(), dir["writable"].as_bool()),
            (Some(true), Some(false)),
            "{kind}: {dir}"
        );
        assert!(
            dir["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "{kind}: {dir}"
        );
        assert!(
            report["summary"]["warnings"].as_array().is_some_and(|w| w
                .iter()
                .any(|x| x.as_str().is_some_and(|x| x.contains(".hoh")))),
            "{kind}: {}",
            report["summary"]
        );
    }
    Ok(())
}

/// `--live` drives a codex that only has exec through exec (M1T07R-04): no app-server schema is asked for, the turn
/// runs and its attempt ends. The recorded reply has no token, so the login stays unknown; no model is called.
#[test]
fn live_drives_an_exec_only_codex_through_exec() -> R {
    let s = Sandbox::new()?;
    let f = fixtures().join("codex/0.166.0");
    s.write(
        "codex",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) grep -v '^  app-server ' '{f}/help.txt';;\n \
             exec) if [ \"$2\" = --help ]; then cat '{f}/help-exec.txt'; elif [ \"$2\" = resume ]; then printf 'Usage: codex exec resume\\n      --json\\n      --dangerously-bypass-approvals-and-sandbox\\n  -m, --model <MODEL>\\n'; \
             else cat > /dev/null; cat '{f}/exec/stdout.jsonl'; fi;;\n \
             login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
            f = f.display()
        ),
    )?;
    // A default-mode command that uses up the shared limit leaves --live its own time (M1T07R-07).
    s.write("node", "#!/bin/sh\nsleep 60\n")?;
    let (out, _) = s.doctor_in(&[], &["--live", "--output", "json"], &[])?;
    let report: Value = serde_json::from_slice(&out.stdout)?;
    let live = &report["agents"]["codex"]["live"];
    assert!(live["error"].is_null(), "the live turn did not run: {live}");
    assert_eq!(live["state"], "SETTLED", "{live}");
    assert_eq!(live["stored"]["state"], "SETTLED", "{live}");
    // The session row records what the probe found, narrowed to exec: no steer, no deltas (REQ-027, M1R-01).
    let probed = &report["agents"]["codex"]["capabilities"];
    let session = &live["stored"]["session_capabilities"];
    assert_eq!(probed["structured_events"], true, "{probed}");
    for name in [
        "structured_events",
        "resume",
        "mid_turn_steer",
        "partial_messages",
    ] {
        assert_eq!(session[name], probed[name], "{name}: {session} vs {probed}");
    }
    assert_eq!(session["mid_turn_steer"], false, "{session}");
    assert_eq!(
        report["agents"]["codex"]["auth_check"], "unknown",
        "the recorded reply has no token"
    );
    let argv: Vec<String> = report["probes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| p["argv"].to_string())
        .collect();
    assert!(
        !argv.iter().any(|a| a.contains("generate-json-schema")),
        "{argv:?}"
    );
    // The scratch directory doctor kept for the logs is in the sandbox's TMPDIR, removed with it.
    assert!(
        live["logs"]
            .as_str()
            .is_some_and(|logs| Path::new(logs).starts_with(s.tmp.path())),
        "{live}"
    );
    Ok(())
}

/// An App Server whose schema lacks something a turn uses (`turn/start`, the `initialized` notification), or whose
/// `--listen` does not list `stdio://`, is not used: the probe takes exec, and so does the live turn (M1T07R-15,
/// M1R-04, M1T07R-27).
#[test]
fn live_drives_codex_through_exec_when_its_app_server_lacks_what_a_turn_uses() -> R {
    // (file, what is taken out of it, what takes its place; for the help, the App Server's schema command then fails,
    // as it is never needed: M1T07R-29).
    for (file, method, other) in [
        ("app-server-schema/ClientRequest.json", "turn/start", ""),
        (
            "app-server-schema/ClientNotification.json",
            "initialized",
            "",
        ),
        ("help-app-server.txt", "stdio://", "pipe://"),
        ("help-app-server.txt", "stdio://", "stdio:///socket"),
    ] {
        let s = Sandbox::new()?;
        let f = fixtures().join("codex/0.166.0");
        // A copy of the recording with `method` taken out of `file`.
        let copy = s.home.path().join("recording");
        std::fs::create_dir(&copy)?;
        assert!(
            std::process::Command::new("cp")
                .arg("-R")
                .arg(f.join("."))
                .arg(&copy)
                .status()?
                .success()
        );
        let text = std::fs::read_to_string(copy.join(file))?;
        let changed = match method {
            "stdio://" => text.replace(method, other),
            _ => {
                let quoted = format!("\"{method}\"");
                assert_eq!(
                    text.matches(&quoted).count(),
                    1,
                    "{file} lists {method} once"
                );
                text.replace(&quoted, &format!("\"{method}-gone\""))
            }
        };
        assert_ne!(changed, text);
        std::fs::write(copy.join(file), changed)?;
        let schema = copy.join("app-server-schema");
        s.write(
            "codex",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
                 exec) if [ \"$2\" = --help ]; then cat '{f}/help-exec.txt'; elif [ \"$2\" = resume ]; then printf 'Usage: codex exec resume\\n      --json\\n'; \
                 else cat > /dev/null; cat '{f}/exec/stdout.jsonl'; fi;;\n \
                 app-server) case \"$2\" in\n  --help) cat '{c}/help-app-server.txt';;\n  generate-json-schema) {schema_command};;\n  \
                 *) exit 9;;\n esac;;\n \
                 login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
                f = f.display(),
                c = copy.display(),
                schema_command = match other {
                    "" => format!("[ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{}/.' \"$4/\"", schema.display()),
                    _ => "exit 2".to_owned(),
                }
            ),
        )?;
        let (out, _) = s.doctor_in(&[], &["--live", "--output", "json"], &[])?;
        let report: Value = serde_json::from_slice(&out.stdout)?;
        let live = &report["agents"]["codex"]["live"];
        assert_eq!(live["state"], "SETTLED", "without {method} {other}: {live}");
        let logs = Path::new(live["logs"].as_str().ok_or("no logs")?);
        assert!(
            !logs.join("app-server.stdout.jsonl").exists(),
            "without {method}, the App Server was started: {}",
            logs.display()
        );
    }
    Ok(())
}

/// A tool that is there but gives no version is installed in both forms, with its version unknown (M1T07R-09).
#[test]
fn a_tool_without_a_version_is_installed_in_both_forms() -> R {
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    s.write("node", "#!/bin/sh\nexit 1\n")?;
    let (_, report, _) = s.doctor()?;
    let node = &report["runtime"]["node"];
    assert_eq!(
        (node["installed"].as_bool(), node["protocol"].as_str()),
        (Some(true), Some("unknown")),
        "{node}"
    );
    let (out, _) = s.doctor_in(&[], &["--output", "text"], &[])?;
    let text = String::from_utf8(out.stdout)?;
    let line = text
        .lines()
        .find(|l| l.starts_with("node"))
        .ok_or("no node line")?;
    assert!(
        !line.contains("未安装") && line.contains("版本未知"),
        "{line}"
    );
    Ok(())
}

/// The live phase's `git init` of its scratch work directory is bounded too (M1T07R-10): one that hangs is stopped
/// at the preparation limit with its group, the agent gets no attempt, and doctor ends.
#[test]
fn a_hung_git_init_does_not_hold_up_live() -> R {
    let s = Sandbox::new()?;
    s.write(
        "git",
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'git version 2.43.0';;\n *) sleep 60 & sleep 60;;\nesac\n",
    )?;
    s.codex(Status::LoggedIn)?;
    let (out, took) = s.doctor_in(&[], &["--live", "--output", "json"], &[])?;
    let report: Value = serde_json::from_slice(&out.stdout)?;
    let live = &report["agents"]["codex"]["live"];
    assert!(
        took <= Duration::from_secs(30),
        "doctor --live took {took:?}"
    );
    assert!(
        live["error"].as_str().is_some_and(|e| e.contains("git")),
        "{live}"
    );
    assert!(
        live["stored"].is_null(),
        "no attempt for a turn that could not be prepared: {live}"
    );
    assert!(
        processes_mentioning(s.bin.path()).is_empty(),
        "a hung git init is still there"
    );
    let ran: Vec<String> = report["probes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| p["argv"].to_string())
        .collect();
    assert!(
        ran.iter().any(|a| a.contains("init")),
        "the git init is in the probe log: {ran:?}"
    );
    // The scratch directory doctor kept for the logs is in the sandbox's TMPDIR, removed with it.
    assert!(
        live["logs"]
            .as_str()
            .is_some_and(|logs| Path::new(logs).starts_with(s.tmp.path())),
        "{live}"
    );
    Ok(())
}
