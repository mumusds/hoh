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
}

struct Sandbox {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
    bin: tempfile::TempDir,
    /// Printed by every status command (a key-like string the doctor must never show).
    canary: Option<String>,
}

impl Sandbox {
    fn new() -> R<Sandbox> {
        let s = Sandbox {
            home: tempfile::tempdir()?,
            project: tempfile::tempdir()?,
            bin: tempfile::tempdir()?,
            canary: None,
        };
        s.write(
            "git",
            "#!/bin/sh\n[ \"$1\" = --version ] && echo 'git version 2.43.0'\n",
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
        }
    }

    fn codex(&self, status: Status) -> R {
        let f = fixtures().join("codex/0.166.0");
        let answer = self.status(
            status,
            "echo 'Logged in using ChatGPT' >&2\nexit 0\n",
            "echo 'Not logged in' >&2\nexit 1\n",
        );
        self.write(
            "codex",
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
                 app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
                 *) exit 2;;\n esac;;\n login) if [ \"$2\" = status ]; then\n{answer}fi;;\n *) exit 2;;\nesac\n",
                f = f.display()
            ),
        )
    }

    /// A codex that has neither app-server nor `exec --json`.
    fn old_codex(&self) -> R {
        self.write(
            "codex",
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.1.0';;\n exec) [ \"$2\" = --help ] && echo 'Usage: codex exec [PROMPT]';;\n \
             *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        )
    }

    fn claude(&self, status: Status) -> R {
        let help = fixtures().join("claude/2.1.318/help.txt");
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
        let help = fixtures().join("pi/1.5.0/help.txt");
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
/// incompatible, so it blocks nothing (M1T07R-03).
#[test]
fn slow_and_hung_commands_share_one_limit_and_stay_unknown() -> R {
    let s = Sandbox::new()?;
    s.codex(Status::LoggedIn)?;
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
    assert!(took <= Duration::from_secs(25), "doctor took {took:?}");
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
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n \
             exec) if [ \"$2\" = --help ]; then cat '{f}/help-exec.txt'; elif [ \"$2\" = resume ]; then echo 'Usage: codex exec resume --json'; \
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
    // The scratch directory doctor kept for the logs.
    if let Some(scratch) = live["logs"]
        .as_str()
        .and_then(|logs| Path::new(logs).parent())
        .and_then(Path::parent)
    {
        let _ = std::fs::remove_dir_all(scratch);
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
