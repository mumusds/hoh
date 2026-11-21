//! `hoh doctor` without `--live` (REQ-006, ADR-015) against programmable fake CLIs in a PATH sandbox:
//! - TEST-006-01: the five-row table (a CLI missing, an old version lacking what HoH needs, compatible but not logged in,
//!   compatible and logged in, a status command that hangs), as fields and exit codes.
//! - TEST-006-02: canary credentials in the environment and in isolated credential files, echoed by the status
//!   commands. They appear in no output or written file, and strace sees no open of those files.
//!
//! The fakes answer the probe from the recorded help texts and schema, and their status commands as the real CLIs do
//! (ADR-015 decision 3). The compatible fakes are registered in the project's `.hoh/` compat manifest with the PASS
//! entries this build's compat suite earns on them.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::compat::{Agent, SuiteStatus};
use serde_json::Value;
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

mod prerequisites;

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
    /// Exits as soon as a helper it started is in a session of its own (its own stat line, so its pid and start time,
    /// in `$HOME/helper.pid`), leaving `$HOME/launcher.exited`. The helper holds both pipes: it closes stdout at 12 s
    /// and gives the logged-in answer on stderr at 28 s, past doctor's 20 s limit.
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
    /// Where every shell stand-in records the process group it runs in as it starts (`started`), outside the
    /// directories a test watches for writes (CIR-004).
    groups: tempfile::TempDir,
}

impl Sandbox {
    fn new() -> R<Sandbox> {
        let s = Sandbox {
            home: tempfile::tempdir()?,
            project: tempfile::tempdir()?,
            bin: tempfile::tempdir()?,
            tmp: tempfile::tempdir()?,
            canary: None,
            groups: tempfile::tempdir()?,
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

    /// Write a stand-in. A shell one records its process group first, read from its own stat with shell built-ins
    /// in a subshell (its `$$` is the script's): each command doctor starts leads a group of its own, and what it
    /// leaves running is found by that group, whatever its command lines say (CIR-004).
    fn write(&self, name: &str, script: &str) -> R {
        let path = self.bin.path().join(name);
        let script = match script.strip_prefix("#!/bin/sh\n") {
            Some(body) => format!(
                "#!/bin/sh\n( read -r s < /proc/$$/stat; set -- ${{s##*\") \"}}; echo \"$3\" >> '{}' )\n{body}",
                self.groups.path().join("started").display()
            ),
            None => script.to_owned(),
        };
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
                "setsid sh -s <<'HELPER' &\nread -r s < /proc/$$/stat; printf '%s\\n' \"$s\" > \"{home}/helper.tmp\"; mv \"{home}/helper.tmp\" \"{home}/helper.pid\"\n\
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
                 *) exit 2;;\n esac;;\n login) if [ \"$2\" = status ]; then\n{answer}fi;;\n \
                 sandbox) mode=$(cat '{mode}' 2>/dev/null)\n  case \"$mode\" in\n   confined) cat \"$5/token\"; exit 1;;\n   \
                 unconfined) shift 6; exec \"$@\";;\n   unavailable) echo 'bwrap: No permissions to create a new namespace' >&2; exit 1;;\n   \
                 *) echo \"error: unrecognized subcommand 'sandbox'\" >&2; exit 2;;\n  esac;;\n *) exit 2;;\nesac\n",
                f = f.display(),
                exec = exec_help.display(),
                schema = schema.display(),
                mode = self.sandbox_mode().display()
            ),
        )
    }

    /// The file whose word fake codex's `sandbox` answers by (docs/adr/ADR-032.md decision 4): `confined`,
    /// `unconfined`, `unavailable`, anything else (a usage error: unknown).
    fn sandbox_mode(&self) -> PathBuf {
        self.groups.path().join("sandbox-mode")
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

    /// Register the installed fakes of `agents` as compatible in the project's compat manifest: the entry this build's
    /// compat suite earns on each, published as a host publishes it (`hoh_app::compat::register`), which must be a PASS
    /// (the fakes serve the recorded helps and schema; docs/adr/ADR-030.md decision 10, never a forged PASS).
    fn register(&self, agents: &[Agent]) -> R {
        for agent in agents {
            let ran = hoh_app::compat::register(
                self.project.path(),
                *agent,
                std::ffi::OsStr::new(&self.path()),
            )?;
            assert_eq!(
                ran.entry.suite.status,
                SuiteStatus::Pass,
                "{agent:?}: {:#?}",
                ran.cases
            );
        }
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

impl Sandbox {
    /// What is left of the process groups the stand-ins started in, once doctor has ended: empty when the kernel finds
    /// no process in any of them (docs/adr/ADR-022.md decision 10, v2: a group is empty only by the kernel's look-up;
    /// CIR-004). A member a scan finds that has not ended (`identity::ended`) is left at once, without waiting: a scan
    /// proves presence, and a command doctor left running that would end by itself must not pass by outwaiting it
    /// (CIR-153). Only a group whose members found are all ended (zombies its reaper has not reaped yet) or dying (a
    /// SIGKILL pending, or already in exit: [`dying`]; the probe SIGKILLs a group and waits for its leader only, CIR-255)
    /// is looked at again, until 10 s have passed in all. No record at all is an error. A number reused since can only
    /// fail the test, never pass it.
    fn groups_left(&self) -> R<Vec<String>> {
        let recorded = std::fs::read_to_string(self.groups.path().join("started"))?;
        let mut groups = std::collections::BTreeSet::new();
        for line in recorded.lines() {
            groups.insert(
                line.parse::<u32>()
                    .map_err(|e| format!("a recorded group {line:?}: {e}"))?,
            );
        }
        if groups.is_empty() {
            return Err("no stand-in recorded its group".into());
        }
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let mut reaping = Vec::new();
            for group in &groups {
                if hoh_agents::identity::group_empty(*group) {
                    continue;
                }
                let running: Vec<u32> = hoh_agents::identity::group_left(*group, 0)?
                    .into_iter()
                    .filter(|m| !matches!(hoh_agents::identity::ended(*m, None), Ok(true)))
                    .filter(|m| !matches!(dying(*m), Ok(true)))
                    .collect();
                if !running.is_empty() {
                    return Ok(vec![format!("group {group}: {running:?} running")]);
                }
                reaping.push(format!("group {group}: not emptied within 10 s"));
            }
            if reaping.is_empty() || Instant::now() >= until {
                return Ok(reaping);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Whether process `pid` is on its way out though not ended yet: a SIGKILL pending for it (its thread's or its group's,
/// `/proc/<pid>/status` SigPnd and ShdPnd, signal 9's bit) or its exit begun (`PF_EXITING`, 0x4, in its stat's flags).
/// A look that fails is an error, so the caller counts it as running.
fn dying(pid: u32) -> R<bool> {
    const SIGKILL_BIT: u64 = 1 << 8;
    const PF_EXITING: u32 = 0x4;
    let status = std::fs::read_to_string(format!("/proc/{pid}/status"))?;
    for line in status.lines() {
        if let Some(mask) = line
            .strip_prefix("SigPnd:")
            .or_else(|| line.strip_prefix("ShdPnd:"))
            && u64::from_str_radix(mask.trim(), 16)? & SIGKILL_BIT != 0
        {
            return Ok(true);
        }
    }
    let stat = std::fs::read(format!("/proc/{pid}/stat"))?;
    let flags: u32 = hoh_agents::identity::after_command_name(&stat)
        .and_then(|fields| fields.split_whitespace().nth(6))
        .and_then(|flags| flags.parse().ok())
        .ok_or_else(|| format!("/proc/{pid}/stat has no flags"))?;
    Ok(flags & PF_EXITING != 0)
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
                let left = s.groups_left()?;
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
    // The helper left codex's group, out of doctor's reach: stop its group here, only while the helper that wrote
    // its stat line is still the process of that number (ADR-022 decision 10: identity::find holds it by a pidfd and
    // checks its start time; Found::kill signals the group only right after a look that shows it held).
    if let Ok(stat) = std::fs::read(s.home.path().join("helper.pid")) {
        let pid: i64 = String::from_utf8_lossy(&stat)
            .split_whitespace()
            .next()
            .ok_or("an empty stat line")?
            .parse()?;
        let start = hoh_agents::identity::after_command_name(&stat)
            .and_then(|fields| fields.split_whitespace().nth(19))
            .ok_or("the helper's stat line has no start time")?;
        let boot = hoh_agents::identity::boot_id()?;
        if let Ok(found) = hoh_agents::identity::find(Some(pid), Some(&boot), Some(start))? {
            found.kill();
        }
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
    let left = s.groups_left()?;
    assert!(
        left.is_empty(),
        "a hung command's group is still there: {left:?}"
    );
    Ok(())
}

#[test]
fn a_signal_ends_doctor_with_its_code_and_leaves_no_probe_behind() -> R {
    // ADR-031's design correction, addendum rule 5 (CLT-200): doctor registers the client's signal state; SIGTERM
    // while a probe runs (a codex whose `--version` sleeps) ends it with 143, no report on stdout, the reason said once
    // on stderr, and the probe, in a process group of its own, has ended by the time doctor has. Before, doctor died by
    // the default disposition and its probe ran on, orphaned, with no one to bound it.
    let s = Sandbox::new()?;
    let pid_file = s.home.path().join("probe.pid");
    s.write(
        "codex",
        &format!(
            "#!/bin/sh\necho $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let mut doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let outcome = (|| -> R<(bool, Duration)> {
        let end = Instant::now() + Duration::from_secs(20);
        let pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                break text.trim().parse()?;
            }
            if Instant::now() > end {
                return Err("codex --version never started".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let probe = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).ok_or("no pid")?,
            rustix::process::PidfdFlags::empty(),
        )?;
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&doctor),
            rustix::process::Signal::TERM,
        )?;
        let at = Instant::now();
        while doctor.try_wait()?.is_none() {
            if at.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let took = at.elapsed();
        let gone = {
            use rustix::event::{PollFd, PollFlags, Timespec};
            let mut fds = [PollFd::new(&probe, PollFlags::IN)];
            let none = Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            rustix::event::poll(&mut fds, Some(&none))? > 0
        };
        if !gone {
            let _ = rustix::process::pidfd_send_signal(&probe, rustix::process::Signal::KILL);
        }
        Ok((gone, took))
    })();
    // Whatever happened, doctor is ended and waited for here (CLT-252).
    let _ = doctor.kill();
    let out = doctor.wait_with_output()?;
    let (gone, took) = outcome?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{:?}: {said}", out.status);
    assert!(gone, "codex --version outlived doctor");
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(said.matches("收到信号").count(), 1, "{said}");
    Ok(())
}

#[test]
fn a_signal_while_the_wal_self_test_probe_runs_leaves_it_ended() -> R {
    // ADR-031, 设计修正 2, A (CLT-300): `doctor --live` opens a scratch store, whose WAL self-test starts its second
    // process (`hoh __wal-probe`) like any other helper of the client. SIGTERM while that probe runs (held at its start
    // by the debug hold point) ends doctor with 143, and the probe has ended by the time doctor has. Before, the probe
    // was started outside the client's stop and ran on after doctor ended.
    let s = Sandbox::new()?;
    let holds = s.home.path().join("holds");
    std::fs::create_dir_all(&holds)?;
    std::fs::write(holds.join("wal-probe"), "")?;
    let mut doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--live", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .env("HOH_DEBUG_HOLD_DIR", &holds)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut probe = None;
    let outcome = (|| -> R<(bool, Duration)> {
        let end = Instant::now() + Duration::from_secs(60);
        let reached = holds.join("wal-probe.reached");
        let pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&reached) {
                break text.trim().parse()?;
            }
            if let Some(status) = doctor.try_wait()? {
                return Err(format!("doctor ended before its WAL probe started: {status}").into());
            }
            if Instant::now() > end {
                return Err("the WAL probe never started".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let held = probe.insert(rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).ok_or("no pid")?,
            rustix::process::PidfdFlags::empty(),
        )?);
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&doctor),
            rustix::process::Signal::TERM,
        )?;
        let at = Instant::now();
        while doctor.try_wait()?.is_none() && at.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let took = at.elapsed();
        Ok((pidfd_ended(held)?, took))
    })();
    // Whatever happened, doctor is ended and waited for, the hold opened and the probe killed if it lives on.
    let _ = doctor.kill();
    let out = doctor.wait_with_output()?;
    let _ = std::fs::remove_file(holds.join("wal-probe"));
    if let Some(probe) = &probe
        && !pidfd_ended(probe).unwrap_or(true)
    {
        let _ = rustix::process::pidfd_send_signal(probe, rustix::process::Signal::KILL);
    }
    let (gone, took) = outcome?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{:?}: {said}", out.status);
    assert!(gone, "the WAL self-test's probe outlived doctor");
    assert!(took < Duration::from_secs(4), "{took:?}");
    // Verification 5 (CLT-1000): everything confirmed ended, so the exit says nothing unconfirmed.
    assert_eq!(said, "hoh: 收到信号，doctor 没有输出报告\n");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    Ok(())
}

#[test]
fn a_signal_during_a_live_turn_leaves_no_process_its_agent_set_apart() -> R {
    // ADR-031, 设计修正 2, B (CLT-302; verification 5): `doctor --live` drives an exec-only codex whose turn starts a
    // child in a session of its own (`setsid`, out of the agent's process group) and then never answers. SIGTERM ends
    // doctor with 143, and that child, found as doctor's descendant and killed by pidfd, has ended by the time doctor
    // has. Before, killing the agent's group left it running, orphaned.
    let s = Sandbox::new()?;
    let f = fixtures().join("codex/0.166.0");
    let child_file = s.home.path().join("apart.pid");
    s.write(
        "codex",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) grep -v '^  app-server ' '{f}/help.txt';;\n \
             exec) if [ \"$2\" = --help ]; then cat '{f}/help-exec.txt'; elif [ \"$2\" = resume ]; then printf 'Usage: codex exec resume\\n      --json\\n      --dangerously-bypass-approvals-and-sandbox\\n  -m, --model <MODEL>\\n'; \
             else setsid sh -c 'echo $$ > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 60' '{c}' < /dev/null > /dev/null 2>&1 & sleep 60; fi;;\n \
             login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
            f = f.display(),
            c = child_file.display()
        ),
    )?;
    let mut doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--live", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let mut apart = None;
    let outcome = (|| -> R<(bool, Duration)> {
        let end = Instant::now() + Duration::from_secs(60);
        let pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&child_file) {
                break text.trim().parse()?;
            }
            if let Some(status) = doctor.try_wait()? {
                return Err(
                    format!("doctor ended before its live turn's child started: {status}").into(),
                );
            }
            if Instant::now() > end {
                return Err("the live turn's child never started".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let held = apart.insert(rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).ok_or("no pid")?,
            rustix::process::PidfdFlags::empty(),
        )?);
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&doctor),
            rustix::process::Signal::TERM,
        )?;
        let at = Instant::now();
        while doctor.try_wait()?.is_none() && at.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let took = at.elapsed();
        Ok((pidfd_ended(held)?, took))
    })();
    // Whatever happened, doctor is ended and waited for, and the child killed if it lives on.
    let _ = doctor.kill();
    let out = doctor.wait_with_output()?;
    if let Some(apart) = &apart
        && !pidfd_ended(apart).unwrap_or(true)
    {
        let _ = rustix::process::pidfd_send_signal(apart, rustix::process::Signal::KILL);
    }
    let (gone, took) = outcome?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{:?}: {said}", out.status);
    assert!(
        gone,
        "the child the live turn's agent set apart outlived doctor"
    );
    assert!(took < Duration::from_secs(4), "{took:?}");
    // Verification 5 (CLT-1000): everything confirmed ended, so the exit says nothing unconfirmed.
    assert_eq!(said, "hoh: 收到信号，doctor 没有输出报告\n");
    Ok(())
}

#[test]
fn a_process_a_probe_set_apart_has_ended_when_doctor_ends_normally() -> R {
    // ADR-031, 设计修正 2, B: the stop runs at every exit, not only a signal's. `git --version` answers at once and
    // leaves a child in a session of its own, orphaned when the probe ends: doctor, its subreaper, finds it under
    // itself and kills it before it ends normally. Without the subreaper it would go to init, unfound; without the
    // stop at a normal exit it would be left running.
    let s = Sandbox::new()?;
    let child_file = s.home.path().join("apart.pid");
    s.write(
        "git",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) setsid sh -c 'echo $$ > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 60' '{c}' \
             < /dev/null > /dev/null 2>&1 & while [ ! -f '{c}' ]; do sleep 0.01; done; echo 'git version 2.43.0';;\n -C) mkdir -p \"$2/.git\";;\n *) exit 1;;\nesac\n",
            c = child_file.display()
        ),
    )?;
    let (out, _) = s.doctor_in(&[], &["--output", "json"], &[])?;
    let said = String::from_utf8_lossy(&out.stderr);
    let pid: i32 = std::fs::read_to_string(&child_file)
        .map_err(|e| format!("the probe's child never started ({e}): {said}"))?
        .trim()
        .parse()?;
    let alive = match rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(pid).ok_or("no pid")?,
        rustix::process::PidfdFlags::empty(),
    ) {
        Ok(child) => {
            let alive = !pidfd_ended(&child)?;
            if alive {
                let _ = rustix::process::pidfd_send_signal(&child, rustix::process::Signal::KILL);
            }
            alive
        }
        Err(_) => false,
    };
    assert!(
        !alive,
        "the child a probe set apart outlived doctor: {said}"
    );
    let report: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(report["runtime"]["git"]["installed"], true, "{report}");
    Ok(())
}

#[test]
fn a_probe_whose_leader_ends_first_leaves_no_member_of_its_group_while_doctor_runs() -> R {
    // ADR-031, 设计修正 2, A2 (CLT-600; verification 4, doctor's half): `git --version` answers and ends, leaving a
    // member of its process group running. The probe's handle sees the leader's exit, kills what is left of the group
    // while the zombie leader still holds its number, and only then reaps it: the member is gone while doctor still
    // runs, not only at its exit. `node --version`, probed alongside, waits for the member and looks at it for 2 s.
    let s = Sandbox::new()?;
    let member = s.home.path().join("member.pid");
    let seen = s.home.path().join("member.seen");
    s.write(
        "git",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) sleep 60 < /dev/null > /dev/null 2>&1 & echo $! > '{m}.tmp' && mv '{m}.tmp' '{m}'; \
             echo 'git version 2.43.0';;\n -C) mkdir -p \"$2/.git\";;\n *) exit 1;;\nesac\n",
            m = member.display()
        ),
    )?;
    s.write(
        "node",
        &format!(
            "#!/bin/sh\nalive() {{ s=$(awk '/^State:/{{print $2}}' /proc/$1/status 2>/dev/null); [ -n \"$s\" ] && [ \"$s\" != Z ]; }}\n\
             while [ ! -f '{m}' ]; do sleep 0.01; done\nm=$(cat '{m}'); i=0\n\
             while alive $m && [ $i -lt 200 ]; do sleep 0.01; i=$((i+1)); done\n\
             if alive $m; then echo alive > '{r}'; else echo gone > '{r}'; fi\necho v22.19.0\n",
            m = member.display(),
            r = seen.display()
        ),
    )?;
    let (out, report, _) = s.doctor()?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        std::fs::read_to_string(&seen)
            .map_err(|e| format!("node never looked ({e}): {said}"))?
            .trim(),
        "gone",
        "a member of the probe's group outlived its probe: {said}"
    );
    assert_eq!(report["runtime"]["git"]["installed"], true, "{report}");
    Ok(())
}

#[test]
fn doctors_stop_ends_more_descendants_than_it_has_descriptors_for() -> R {
    // ADR-031, 设计修正 2, corrections after its implementation review, 3 (CLT-1200): `git --version` sets 120 sleepers
    // apart (each in a session of its own, so only doctor's sweep reaches them once git ends) and doctor runs with 64
    // file descriptors at most, fewer than it would hold pidfds for them all. Its stop at the end must still end every
    // one, or say it could not: a pidfd it cannot open makes a scan incomplete, never clean, and the pidfds of those
    // that ended are let go. Before, a failed pidfd_open was skipped, the scan counted clean, and sleepers lived on.
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let pids = s.home.path().join("sleepers");
    s.write(
        "git",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) i=0; while [ $i -lt 120 ]; do setsid sh -c 'read -r s < /proc/$$/stat; \
             echo \"$s\" >> \"$0\"; exec sleep 60' '{p}' < /dev/null > /dev/null 2>&1 & i=$((i+1)); done; \
             while [ \"$(wc -l < '{p}' 2>/dev/null)\" != 120 ]; do sleep 0.01; done; echo 'git version 2.43.0';;\n \
             -C) mkdir -p \"$2/.git\";;\n *) exit 1;;\nesac\n",
            p = pids.display()
        ),
    )?;
    let out = Command::new("sh")
        .args(["-c", "ulimit -n 64 && exec \"$0\" \"$@\""])
        .arg(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .output()?;
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    // Every sleeper by its stat line (pid and start time), so a number taken again is never mistaken for it.
    let text = std::fs::read_to_string(&pids)?;
    let mut alive = Vec::new();
    for line in text.lines() {
        let pid: u32 = line.split_whitespace().next().ok_or("pid")?.parse()?;
        let start = hoh_agents::identity::after_command_name(line.as_bytes())
            .and_then(|rest| rest.split_whitespace().nth(19))
            .ok_or("start")?
            .to_owned();
        if !hoh_agents::identity::ended(pid, Some(&start))? {
            alive.push((pid, start));
        }
    }
    for (pid, start) in &alive {
        let _ = hoh_agents::identity::signal_if_same(*pid, start, rustix::process::Signal::KILL);
    }
    assert_eq!(text.lines().count(), 120, "{said}");
    assert!(
        alive.is_empty() || said.contains("没能"),
        "{} sleepers outlived doctor and it said nothing: {said}",
        alive.len()
    );
    assert!(
        alive.is_empty(),
        "{} sleepers outlived doctor: {said}",
        alive.len()
    );
    Ok(())
}

#[test]
fn doctors_stop_ends_a_descendant_whose_leader_exited_while_a_thread_runs() -> R {
    // ADR-031, 设计修正 2, corrections after its second implementation review, 6 (CLT-1300): `git --version` sets apart
    // a process (a session of its own) whose main thread calls pthread_exit while a worker thread runs on: /proc shows
    // the leader Z, yet the thread group lives. doctor's stop at its end must end it (its pidfd only readable once every
    // thread is gone), or say it could not. Before, the sweep took the Z for proof and left it running, saying nothing.
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let stat = s.home.path().join("threaded");
    let script = format!(
        "import ctypes, os, threading, time\n\
         threading.Thread(target=time.sleep, args=(60,)).start()\n\
         line = open('/proc/self/stat').read().strip()\n\
         open('{0}.tmp', 'w').write(line)\n\
         os.rename('{0}.tmp', '{0}')\n\
         ctypes.CDLL(None).pthread_exit(None)\n",
        stat.display()
    );
    let program = s.home.path().join("threaded.py");
    std::fs::write(&program, script)?;
    s.write(
        "git",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) setsid python3 '{p}' < /dev/null > /dev/null 2>&1 & \
             while [ ! -f '{s}' ]; do sleep 0.01; done; \
             i=0; while [ $i -lt 200 ] && [ \"$(awk '{{print $3}}' /proc/$(cut -d' ' -f1 '{s}')/stat 2>/dev/null)\" != Z ]; do sleep 0.01; i=$((i+1)); done; \
             echo 'git version 2.43.0';;\n -C) mkdir -p \"$2/.git\";;\n *) exit 1;;\nesac\n",
            p = program.display(),
            s = stat.display()
        ),
    )?;
    let mut path = s.path();
    path.push_str(":/usr/local/bin");
    let out = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", &path)
        .output()?;
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let line = std::fs::read_to_string(&stat)
        .map_err(|e| format!("the threaded process never started ({e}): {said}"))?;
    let pid: u32 = line.split_whitespace().next().ok_or("pid")?.parse()?;
    let start = hoh_agents::identity::after_command_name(line.as_bytes())
        .and_then(|rest| rest.split_whitespace().nth(19))
        .ok_or("start")?
        .to_owned();
    let alive = !hoh_agents::identity::ended(pid, Some(&start))?;
    if alive {
        let _ = hoh_agents::identity::signal_if_same(pid, &start, rustix::process::Signal::KILL);
    }
    assert!(
        !alive || said.contains("没能"),
        "a descendant whose leader is Z while a thread runs outlived doctor, and it said nothing: {said}"
    );
    assert!(
        !alive,
        "a descendant whose leader is Z while a thread runs outlived doctor: {said}"
    );
    Ok(())
}

#[test]
fn doctors_stop_sees_what_a_descendant_forked_as_it_exited() -> R {
    // ADR-031, 设计修正 2, round 3 (CLT-1451): doctor's stop reads its scan's table while a descendant P (set apart by
    // `git --version`) is alive; held there, P forks a daemon-like grandchild in a session of its own and exits, so its
    // pidfd is readable when the scan first looks at it, and the grandchild, re-parented to doctor, is in no table yet.
    // A process is confirmed ended only on a later scan than the one it was first seen in, so that scan sees the
    // grandchild and ends it. Before, P was confirmed at once, the scan was clean, and the grandchild outlived doctor.
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let (go, parent, grand) = (
        s.home.path().join("go"),
        s.home.path().join("parent"),
        s.home.path().join("grand"),
    );
    let forker = format!(
        "read -r st < /proc/$$/stat; echo \"$st\" > '{p}.tmp' && mv '{p}.tmp' '{p}'; \
         while [ ! -f '{go}' ]; do sleep 0.01; done; \
         setsid sh -c 'read -r st < /proc/$$/stat; echo \"$st\" > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 60' '{g}' \
         < /dev/null > /dev/null 2>&1 & \
         while [ ! -f '{g}' ]; do sleep 0.01; done; exit 0",
        p = parent.display(),
        go = go.display(),
        g = grand.display()
    );
    let script = s.home.path().join("forker.sh");
    std::fs::write(&script, forker)?;
    s.write(
        "git",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) setsid sh '{f}' < /dev/null > /dev/null 2>&1 & \
             while [ ! -f '{p}' ]; do sleep 0.01; done; echo 'git version 2.43.0';;\n -C) mkdir -p \"$2/.git\";;\n \
             *) exit 1;;\nesac\n",
            f = script.display(),
            p = parent.display()
        ),
    )?;
    let holds = s.home.path().join("holds");
    std::fs::create_dir_all(&holds)?;
    std::fs::write(holds.join("sweep-scanned"), "")?;
    let doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .env("HOH_DEBUG_HOLD_DIR", &holds)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let stat_of = |file: &Path| -> R<(u32, String)> {
        let line = std::fs::read_to_string(file)?;
        let pid: u32 = line.split_whitespace().next().ok_or("pid")?.parse()?;
        let start = hoh_agents::identity::after_command_name(line.as_bytes())
            .and_then(|rest| rest.split_whitespace().nth(19))
            .ok_or("start")?
            .to_owned();
        Ok((pid, start))
    };
    let outcome = (|| -> R<(u32, String)> {
        let end = Instant::now() + Duration::from_secs(60);
        let wait = |what: &dyn Fn() -> bool, why: &str| -> R {
            while !what() {
                if Instant::now() > end {
                    return Err(why.to_owned().into());
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(())
        };
        wait(
            &|| holds.join("sweep-scanned.reached").exists(),
            "the stop's scan was never held",
        )?;
        let (p, p_start) = stat_of(&parent)?;
        std::fs::write(&go, "")?;
        wait(&|| grand.exists(), "the grandchild never started")?;
        wait(
            &|| hoh_agents::identity::ended(p, Some(&p_start)).unwrap_or(false),
            "the descendant never exited",
        )?;
        std::fs::remove_file(holds.join("sweep-scanned"))?;
        stat_of(&grand)
    })();
    let _ = std::fs::remove_file(holds.join("sweep-scanned"));
    let out = doctor.wait_with_output()?;
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    let (g, g_start) = outcome?;
    let alive = !hoh_agents::identity::ended(g, Some(&g_start))?;
    if alive {
        let _ = hoh_agents::identity::signal_if_same(g, &g_start, rustix::process::Signal::KILL);
    }
    assert!(
        !alive || said.contains("没能"),
        "what a descendant forked as it exited outlived doctor, and it said nothing: {said}"
    );
    assert!(
        !alive,
        "what a descendant forked as it exited outlived doctor: {said}"
    );
    Ok(())
}

#[test]
fn a_signal_after_doctors_report_is_written_says_it_was_written() -> R {
    // ADR-031, 设计修正 2, D (CLT-902, CLT-952, CLT-1050; verification 16): doctor is held once its report is on
    // stdout; SIGTERM then ends it with 143, the whole report stays on stdout, and its exit says the report was
    // written, once, and nothing else (not "没有输出报告", not rule 2's sentence, no positive statement of a stop).
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let holds = s.home.path().join("holds");
    std::fs::create_dir_all(&holds)?;
    std::fs::write(holds.join("doctor-written"), "")?;
    let mut doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .env("HOH_DEBUG_HOLD_DIR", &holds)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let outcome = (|| -> R<Duration> {
        let end = Instant::now() + Duration::from_secs(60);
        let reached = holds.join("doctor-written.reached");
        while !reached.exists() {
            if let Some(status) = doctor.try_wait()? {
                return Err(format!("doctor ended before its report was written: {status}").into());
            }
            if Instant::now() > end {
                return Err("doctor never wrote its report".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&doctor),
            rustix::process::Signal::TERM,
        )?;
        std::fs::remove_file(holds.join("doctor-written"))?;
        let at = Instant::now();
        while doctor.try_wait()?.is_none() && at.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(at.elapsed())
    })();
    // Whatever happened, doctor is ended and waited for here (CLT-252).
    let _ = doctor.kill();
    let _ = std::fs::remove_file(holds.join("doctor-written"));
    let out = doctor.wait_with_output()?;
    let took = outcome?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{:?}: {said}", out.status);
    assert!(took < Duration::from_secs(4), "{took:?}");
    let report: Value = serde_json::from_slice(&out.stdout)?;
    assert_eq!(report["schema_version"], "1.0", "{report}");
    assert_eq!(said, "hoh: 收到信号，报告已经写出\n");
    Ok(())
}

/// Whether the process `pidfd` holds has ended (exited, whoever reaps it).
fn pidfd_ended(pidfd: &std::os::fd::OwnedFd) -> R<bool> {
    use rustix::event::{PollFd, PollFlags, Timespec};
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    let none = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    Ok(rustix::event::poll(&mut fds, Some(&none))? > 0)
}

#[test]
fn a_hash_that_does_not_fit_doctors_limit_is_given_up_said_and_recorded() -> R {
    // Package G2 (REQ-006 behaviour 6; docs/adr/ADR-015.md note): doctor's identity hashing is inside its 20 s limit
    // and in its records. A claude 64 GiB long (sparse: its script exits before the zeros) cannot be hashed in 20 s:
    // doctor still ends in time, says the hash was not done and why, still shows the version, and records the hash
    // with how much it read, beside its commands, not among them. The check before a host's start runs from doctor's
    // start, beside the agents, so the hash does not starve it of its time.
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let help = fixtures().join("claude/2.1.318/help.txt");
    s.write(
        "claude",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{}';;\n \
             auth) echo '{{\"loggedIn\": true}}';;\nesac\nexit\n",
            help.display()
        ),
    )?;
    let claude = s.bin.path().join("claude");
    let size: u64 = 64 << 30;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&claude)?
        .set_len(size)?;
    s.write("systemd-run", "#!/bin/sh\necho 'systemd 255 (255.4-1)'\n")?;
    let manager: Vec<(&str, String)> = ["XDG_RUNTIME_DIR", "USER"]
        .into_iter()
        .filter_map(|k| std::env::var(k).ok().map(|v| (k, v)))
        .collect();
    let pairs: Vec<(&str, &str)> = manager.iter().map(|(k, v)| (*k, v.as_str())).collect();
    // Bounded here too: a doctor that hashed all of it would take minutes.
    let (out, took) = s.doctor_in(&["timeout", "60"], &["--output", "json"], &pairs)?;
    let report: Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        format!(
            "{e} after {took:?} (exit {:?}): {}",
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        )
    })?;
    assert!(took <= Duration::from_secs(25), "doctor took {took:?}");
    let item = &report["agents"]["claude"];
    assert_eq!(item["installed"], true, "{item}");
    assert_eq!(item["version"], "2.1.318 (Claude Code)", "{item}");
    assert_eq!(item["protocol"], "unknown", "{item}");
    assert!(item["binary_sha256"].is_null(), "{item}");
    let detail = item["protocol_detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("sha256") && detail.contains("总时限"),
        "{detail}"
    );
    let hashes = report["hashes"].as_array().ok_or("no hashes")?;
    let cut = hashes
        .iter()
        .find(|h| h["path"] == claude.display().to_string().as_str())
        .ok_or_else(|| format!("claude's hash is not recorded: {hashes:?}"))?;
    assert_eq!(cut["done"], false, "{cut}");
    assert_eq!(cut["size"], size, "{cut}");
    assert!(
        cut["read"].as_u64().is_some_and(|read| read < size),
        "{cut}"
    );
    assert!(
        cut["duration_ms"].as_u64().is_some_and(|ms| ms <= 21_000),
        "{cut}"
    );
    assert!(
        cut["reason"].as_str().is_some_and(|r| r.contains("limit")),
        "{cut}"
    );
    // The others were hashed in full, each recorded with its duration.
    for agent in ["codex", "pi"] {
        let launcher = report["agents"][agent]["path"].as_str().unwrap_or_default();
        assert!(
            hashes
                .iter()
                .any(|h| h["path"] == launcher && h["done"] == true && h["duration_ms"].is_u64()),
            "{agent}: {hashes:?}"
        );
    }
    // probes lists the commands doctor started, and only those (ADR-015 decision 5, AC-006-03).
    assert!(
        report["probes"]
            .as_array()
            .into_iter()
            .flatten()
            .all(|p| p["argv"][0].as_str().is_some_and(|a| a.starts_with('/'))),
        "{}",
        report["probes"]
    );
    // The check before a start was not starved: never "not started" for want of time.
    let host = &report["runtime"]["host"];
    let tracking = host["process_tracking_detail"].as_str().unwrap_or_default();
    assert!(!tracking.contains("not started"), "{host}");
    let user = std::env::var("USER").unwrap_or_default();
    if std::env::var_os("XDG_RUNTIME_DIR")
        .is_some_and(|run| Path::new(&run).join("systemd/private").exists())
        && !user.is_empty()
        && Path::new("/var/lib/systemd/linger").join(&user).is_file()
        && Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
    {
        assert_eq!(host["process_tracking"], "unverified", "{host}");
    }
    Ok(())
}

#[test]
fn a_signal_while_doctors_start_check_asks_systemd_run_leaves_no_probe_behind() -> R {
    // Package G2 on ADR-031's addendum, rule 5: the check before a host's start runs from doctor's start, beside the
    // agents (docs/adr/ADR-015.md note of package G2), and its probe is the client's helper like any other: SIGTERM
    // while its `systemd-run --version` does not answer (a stand-in that sleeps; a fake runtime directory stands in for
    // the user's manager, so none is contacted) ends doctor with 143, no report, and that probe has ended by the time
    // doctor has.
    let user = std::env::var("USER").unwrap_or_default();
    if user.is_empty()
        || !Path::new("/var/lib/systemd/linger").join(&user).is_file()
        || !Path::new("/sys/fs/cgroup/cgroup.controllers").exists()
    {
        return prerequisites::missing("no linger record for this user, or no cgroup v2, here");
    }
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
    s.claude(Status::LoggedIn)?;
    s.pi(Status::LoggedIn)?;
    let pid_file = s.home.path().join("probe.pid");
    s.write(
        "systemd-run",
        &format!(
            "#!/bin/sh\necho $$ > '{0}.tmp' && mv '{0}.tmp' '{0}'\nexec sleep 30\n",
            pid_file.display()
        ),
    )?;
    let runtime = s.tmp.path().join("run");
    std::fs::create_dir_all(runtime.join("systemd"))?;
    std::fs::File::create(runtime.join("systemd/private"))?;
    let mut doctor = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .args(["doctor", "--output", "json"])
        .current_dir(s.project.path())
        .env_clear()
        .env("HOME", s.home.path())
        .env("TMPDIR", s.tmp.path())
        .env("PATH", s.path())
        .env("XDG_RUNTIME_DIR", &runtime)
        .env("USER", &user)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let outcome = (|| -> R<(bool, Duration)> {
        let end = Instant::now() + Duration::from_secs(20);
        let pid: i32 = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file) {
                break text.trim().parse()?;
            }
            if Instant::now() > end {
                return Err("systemd-run --version never started".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let probe = rustix::process::pidfd_open(
            rustix::process::Pid::from_raw(pid).ok_or("no pid")?,
            rustix::process::PidfdFlags::empty(),
        )?;
        rustix::process::kill_process(
            rustix::process::Pid::from_child(&doctor),
            rustix::process::Signal::TERM,
        )?;
        let at = Instant::now();
        while doctor.try_wait()?.is_none() {
            if at.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let took = at.elapsed();
        let gone = {
            use rustix::event::{PollFd, PollFlags, Timespec};
            let mut fds = [PollFd::new(&probe, PollFlags::IN)];
            let none = Timespec {
                tv_sec: 0,
                tv_nsec: 0,
            };
            rustix::event::poll(&mut fds, Some(&none))? > 0
        };
        if !gone {
            let _ = rustix::process::pidfd_send_signal(&probe, rustix::process::Signal::KILL);
        }
        Ok((gone, took))
    })();
    // Whatever happened, doctor is ended and waited for here.
    let _ = doctor.kill();
    let out = doctor.wait_with_output()?;
    let (gone, took) = outcome?;
    let said = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(143), "{:?}: {said}", out.status);
    assert!(gone, "systemd-run --version outlived doctor");
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(
        out.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
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
        ("help-app-server.txt", "stdio://", "stdio://.."),
        // Listed, but no message for it can be valid (M1T07R-33).
        (
            "app-server-schema/ClientRequest.json",
            "initialize",
            "id-false",
        ),
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
        let changed = match (method, other) {
            ("stdio://", _) => text.replace("`stdio://`", other),
            (_, "id-false") => {
                let mut value: Value = serde_json::from_str(&text)?;
                for branch in value["oneOf"].as_array_mut().into_iter().flatten() {
                    if branch["properties"]["method"]["enum"] == serde_json::json!([method]) {
                        branch["properties"]["id"] = serde_json::json!(false);
                    }
                }
                serde_json::to_string(&value)?
            }
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
                schema_command = match method {
                    "stdio://" => "exit 2".to_owned(),
                    _ => format!("[ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{}/.' \"$4/\"", schema.display()),
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

/// `--live` reads the App Server's schema as the probe does, validated: a schema the probe refuses (a root `not` keyword
/// or a root constraint on `method`, outside the form the App Server schema uses) prepares no attempt and starts no
/// App Server, so no request it may exclude is sent (M1T07R-31, -32).
#[test]
fn live_prepares_nothing_with_a_schema_the_probe_refuses() -> R {
    // A root `not`, and a root `properties.method` that leaves `initialize` out (M1T07R-32).
    for constraint in [
        (
            "not",
            serde_json::json!({"properties": {"method": {"enum": ["initialize"]}}}),
        ),
        (
            "properties",
            serde_json::json!({"method": {"type": "string", "enum": ["thread/start"]}}),
        ),
    ] {
        let s = Sandbox::new()?;
        let f = fixtures().join("codex/0.166.0");
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
        let mut requests: Value =
            serde_json::from_slice(&std::fs::read(schema.join("ClientRequest.json"))?)?;
        requests[constraint.0] = constraint.1;
        std::fs::write(
            schema.join("ClientRequest.json"),
            serde_json::to_vec(&requests)?,
        )?;
        let started = s.home.path().join("app-server-started");
        s.write(
        "codex",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) grep -v '^  exec ' '{f}/help.txt';;\n \
             app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{s}/.' \"$4/\";;\n  \
             *) : > '{started}'; exit 9;;\n esac;;\n \
             login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
            f = f.display(),
            s = schema.display(),
            started = started.display()
        ),
    )?;
        let (out, _) = s.doctor_in(&[], &["--live", "--output", "json"], &[])?;
        let report: Value = serde_json::from_slice(&out.stdout)?;
        let live = &report["agents"]["codex"]["live"];
        assert!(live["stored"].is_null(), "an attempt was prepared: {live}");
        assert!(
            live["error"]
                .as_str()
                .is_some_and(|e| e.contains("ClientRequest.json")),
            "{live}"
        );
        assert!(!started.exists(), "the App Server was started: {live}");
        assert_ne!(report["agents"]["codex"]["auth_check"], "verified");
    }
    Ok(())
}

/// A schema output within its size limits made to parse into huge values (60 MiB of one-key objects in other files,
/// a protocol file padded up to its value limit) keeps doctor within a 1 GiB data limit: it still writes its report
/// (M1T07R-50). Such output used to take gigabytes, and doctor aborted with no report.
#[test]
fn doctor_reports_a_bloated_schema_within_a_memory_limit() -> R {
    let s = Sandbox::new()?;
    let f = fixtures().join("codex/0.166.0");
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
    let mut pad = b"[".to_vec();
    pad.extend(br#"{"":0},"#.repeat(15 * 1024 * 1024 / 7));
    pad.extend(br#"{"":0}]"#);
    for n in 0..4 {
        std::fs::write(schema.join(format!("pad{n}.json")), &pad)?;
    }
    let mut requests: Value =
        serde_json::from_slice(&std::fs::read(schema.join("ClientRequest.json"))?)?;
    requests["definitions"]["Pad"] =
        Value::Array((0..60_000).map(|_| serde_json::json!({"": 0})).collect());
    std::fs::write(
        schema.join("ClientRequest.json"),
        serde_json::to_vec(&requests)?,
    )?;
    s.write(
        "codex",
        &format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
             exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{s}/.' \"$4/\";;\n  \
             *) exit 9;;\n esac;;\n \
             login) echo 'Logged in using ChatGPT' >&2;;\n *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
            f = f.display(),
            s = schema.display()
        ),
    )?;
    let (out, _) = s.doctor_in(
        &["/usr/bin/prlimit", "--data=1073741824", "--"],
        &["--output", "json"],
        &[],
    )?;
    assert!(
        out.status.code().is_some(),
        "doctor was ended by a signal: {:?}; {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let report: Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("{e}: {}", String::from_utf8_lossy(&out.stderr)))?;
    assert!(
        report["agents"]["codex"]["installed"].as_bool() == Some(true),
        "{report}"
    );
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
    let left = s.groups_left()?;
    assert!(
        left.is_empty(),
        "a hung git init's group is still there: {left:?}"
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

/// The Commander's read-only workspace profile on each agent (docs/adr/ADR-032.md decisions 4 and 7): doctor's
/// `read_only` item gives `workspace` and its parts, the same as the host's registry judges them on the same binaries
/// and manifest (whether `granted` has `read_only_workspace`), with how it is enforced and codex's host check; the exit
/// code does not change with it. codex's check confined, unconfined, unavailable and unknown; a claude help without
/// `--strict-mcp-config` (its compat suite passes, its read_only case fails).
#[test]
fn the_read_only_item_is_the_registrys_judgement() -> R {
    let mut seen = Vec::new();
    for (case, mode, claude_help) in [
        ("confined", "confined", None),
        ("unconfined", "unconfined", None),
        ("unavailable", "unavailable", None),
        ("unknown", "unknown", None),
        (
            "claude-without-strict-mcp",
            "confined",
            Some("--strict-mcp-config"),
        ),
    ] {
        let s = Sandbox::new()?;
        s.codex(Status::LoggedIn)?;
        std::fs::write(s.sandbox_mode(), mode)?;
        match claude_help {
            Some(option) => {
                let recorded = std::fs::read_to_string(fixtures().join("claude/2.1.318/help.txt"))?;
                let help: String = recorded
                    .split_inclusive('\n')
                    .filter(|l| !l.contains(option))
                    .collect();
                assert_ne!(help, recorded);
                let path = s.tmp.path().join("claude-help.txt");
                std::fs::write(&path, help)?;
                s.claude_with_help(Status::LoggedIn, &path)?;
            }
            None => s.claude(Status::LoggedIn)?,
        }
        s.pi(Status::LoggedIn)?;
        s.register(&[Agent::Codex, Agent::Claude, Agent::Pi])?;
        let (out, report, _) = s.doctor()?;
        assert_eq!(out.status.code(), Some(0), "{case}: {report}");
        let registry = hoh_app::host::agents::Registry::with_pi_dir(
            s.path().into(),
            Some(s.home.path().join(".pi/agent")),
        );
        registry.open_manifest(s.project.path());
        registry.want(&["codex".to_owned(), "claude".to_owned(), "pi".to_owned()]);
        registry.refresh();
        for (agent, enforced_by) in [
            (Agent::Codex, "os_sandbox"),
            (Agent::Claude, "tool_allowlist"),
            (Agent::Pi, "tool_allowlist"),
        ] {
            let name = agent.as_str();
            let item = &report["agents"][name]["read_only"];
            let granted = match registry.fact(agent) {
                Some(hoh_core::staffing::Fact::Usable(usable)) => {
                    usable.granted.contains("read_only_workspace")
                }
                other => return Err(format!("{case}: {name} is not usable: {other:?}").into()),
            };
            assert_eq!(
                item["workspace"],
                Value::Bool(granted),
                "{case}: {name}: {item}"
            );
            assert_eq!(item["enforced_by"], enforced_by, "{case}: {name}");
            assert!(
                item["detail"].as_str().is_some_and(|d| !d.is_empty()),
                "{case}: {name}: {item}"
            );
            let parts = &item["parts"];
            let refused = case == "claude-without-strict-mcp" && agent == Agent::Claude;
            let want = if refused {
                (Value::Bool(false), Value::Bool(false), Value::from("FAIL"))
            } else {
                (Value::Bool(true), Value::Bool(true), Value::from("PASS"))
            };
            assert_eq!(
                (
                    parts["probe"].clone(),
                    parts["builder"].clone(),
                    parts["suite_case"].clone()
                ),
                want,
                "{case}: {name}: {item}"
            );
            if agent == Agent::Codex {
                assert_eq!(item["check"], mode, "{case}: {item}");
                assert_eq!(parts["host_check"]["check"], mode, "{case}: {item}");
                let snapshot = registry.snapshot(0);
                assert_eq!(
                    snapshot.probes["codex"]["host_check"]["check"], mode,
                    "{case}: the registry's check"
                );
            } else {
                assert!(item["check"].is_null(), "{case}: {name}: {item}");
            }
            seen.push(format!("{case} {name} {}", item["workspace"]));
        }
        // The check is one command of the shape doctor_live.rs's whitelist admits.
        let checks: Vec<Vec<String>> = report["probes"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|run| {
                run["argv"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .skip(1)
                    .filter_map(|a| a.as_str().map(str::to_owned))
                    .collect::<Vec<String>>()
            })
            .filter(|args| args.first().map(String::as_str) == Some("sandbox"))
            .collect();
        assert_eq!(checks.len(), 1, "{case}: {checks:?}");
        let check: Vec<&str> = checks[0].iter().map(String::as_str).collect();
        assert!(
            matches!(
                check.as_slice(),
                [
                    "sandbox",
                    "-P",
                    ":read-only",
                    "-C",
                    _,
                    "--",
                    "/bin/sh",
                    "-c",
                    _,
                    "hoh-sandbox-check",
                    _
                ]
            ),
            "{case}: {check:?}"
        );
        // The text form shows it too.
        let (out, _) = s.doctor_in(&[], &["--output", "text"], &[])?;
        let text = String::from_utf8(out.stdout)?;
        assert!(text.contains("read_only："), "{case}: {text}");
    }
    assert!(seen.contains(&"confined codex true".to_owned()), "{seen:?}");
    for unusable in [
        "unconfined codex false",
        "unavailable codex false",
        "claude-without-strict-mcp claude false",
    ] {
        assert!(seen.contains(&unusable.to_owned()), "{seen:?}");
    }
    assert!(seen.contains(&"unknown codex true".to_owned()), "{seen:?}");
    Ok(())
}
