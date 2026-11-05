//! AC-027-01 / TEST-027-01: probe() on three groups of fake binaries yields schema-valid compat manifest entries with
//! the known hashes, the version text verbatim, capabilities from the probed help/schema, and identical results twice.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::compat::{Agent, AgentEntry, CompatManifest, SuiteStatus, Transport};
use hoh_agents::probe::{ProbeEnv, ProbeError, RunLog, probe, run_status, tree_sha256};
use hoh_core::schema;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

type R = Result<(), Box<dyn std::error::Error>>;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol")
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write an executable fake CLI named `name` into its own directory and return (dir, script path).
fn fake(
    root: &Path,
    group: &str,
    name: &str,
    script: &str,
) -> Result<(PathBuf, PathBuf), Box<dyn std::error::Error>> {
    let dir = root.join(group);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    std::fs::write(&path, script)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))?;
    Ok((dir, path))
}

fn env_for(dir: &Path) -> ProbeEnv {
    ProbeEnv {
        path: format!("{}:/usr/bin:/bin", dir.display()).into(),
        fixture_root: Some(fixtures()),
        timeout: Duration::from_secs(30),
        log: None,
        deadline: None,
    }
}

fn probe_twice(agent: Agent, dir: &Path) -> Result<AgentEntry, Box<dyn std::error::Error>> {
    let first = probe(agent, &env_for(dir))?;
    let second = probe(agent, &env_for(dir))?;
    assert_eq!(
        first, second,
        "{agent:?}: two probes of the same input differ"
    );
    Ok(first)
}

#[test]
fn test_027_01_probe_three_fake_binaries_into_a_valid_manifest() -> R {
    let tmp = tempfile::tempdir()?;
    let codex_fix = fixtures().join("codex/0.166.0");
    let claude_help = std::fs::read_to_string(fixtures().join("claude/2.1.317/help.txt"))?;
    let old_claude_help: String = claude_help
        .lines()
        .filter(|l| !l.contains("--include-partial-messages"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(
        old_claude_help.len(),
        claude_help.len(),
        "the recorded help must mention --include-partial-messages"
    );
    let old_help_file = tmp.path().join("old-claude-help.txt");
    std::fs::write(&old_help_file, &old_claude_help)?;

    let new_codex = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
         app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
         *) exit 2;;\n esac;;\n *) echo \"unknown command $1\" >&2; exit 2;;\nesac\n",
        f = codex_fix.display()
    );
    let old_codex = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.100.0';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
         *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        f = codex_fix.display()
    );
    let old_claude = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.0.0 (Claude Code)';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
        old_help_file.display()
    );
    let (new_dir, new_path) = fake(tmp.path(), "codex-0.166", "codex", &new_codex)?;
    let (old_dir, old_path) = fake(tmp.path(), "codex-old", "codex", &old_codex)?;
    let (claude_dir, claude_path) = fake(tmp.path(), "claude-old", "claude", &old_claude)?;

    let new = probe_twice(Agent::Codex, &new_dir)?;
    let old = probe_twice(Agent::Codex, &old_dir)?;
    let claude = probe_twice(Agent::Claude, &claude_dir)?;

    // Launcher and implementation hashes equal the known values (a fake binary is its own implementation).
    for (entry, path, script) in [
        (&new, &new_path, &new_codex),
        (&old, &old_path, &old_codex),
        (&claude, &claude_path, &old_claude),
    ] {
        assert_eq!(entry.launcher_path, path.display().to_string());
        assert_eq!(entry.implementation_path, entry.launcher_path);
        assert_eq!(entry.launcher_sha256, sha256(script.as_bytes()));
        assert_eq!(entry.implementation_sha256, entry.launcher_sha256);
        assert_eq!(entry.suite.status, SuiteStatus::NotRun);
    }
    assert_eq!(
        (
            new.cli_version.as_str(),
            old.cli_version.as_str(),
            claude.cli_version.as_str()
        ),
        (
            "codex-cli 0.166.0",
            "codex-cli 0.100.0",
            "2.0.0 (Claude Code)"
        )
    );

    // Help and schema hashes; transports.
    let exec_help = std::fs::read(codex_fix.join("help-exec.txt"))?;
    let app_help = std::fs::read(codex_fix.join("help-app-server.txt"))?;
    assert_eq!(
        new.help_sha256,
        sha256(&[exec_help.clone(), app_help].concat())
    );
    assert_eq!(old.help_sha256, sha256(&exec_help));
    assert_eq!(claude.help_sha256, sha256(old_claude_help.as_bytes()));
    assert_eq!(
        new.schema_sha256.as_deref(),
        Some(tree_sha256(&codex_fix.join("app-server-schema"))?.as_str())
    );
    assert_eq!(
        (
            old.schema_sha256.as_deref(),
            claude.schema_sha256.as_deref()
        ),
        (None, None)
    );
    assert_eq!(
        (new.transport, old.transport, claude.transport),
        (Transport::AppServer, Transport::Exec, Transport::StreamJson)
    );

    // Capabilities come from what was probed, not from the brand.
    assert_eq!(new.capabilities.get("mid_turn_steer"), Some(&Some(true)));
    assert_eq!(old.capabilities.get("mid_turn_steer"), Some(&Some(false)));
    assert_ne!(new.capabilities, old.capabilities);
    assert_eq!(
        claude.capabilities.get("partial_messages"),
        Some(&Some(false))
    );
    assert_eq!(
        claude.capabilities.get("structured_events"),
        Some(&Some(true))
    );

    // The fixture set hash is the tree hash of the recorded fixtures for that version, when there are any.
    assert_eq!(new.suite.fixture_set_sha256, tree_sha256(&codex_fix)?);

    // The manifest validates against contracts/compat-manifest.schema.json.
    let mut agents = BTreeMap::new();
    agents.insert(Agent::Codex, new);
    agents.insert(Agent::Claude, claude);
    let manifest = CompatManifest::new(agents);
    let value = serde_json::to_value(&manifest)?;
    if let Err(errors) = schema::compat_manifest().validate(&value) {
        return Err(format!("manifest invalid: {errors:?}").into());
    }
    let mut with_old = manifest.clone();
    with_old.agents.insert(Agent::Codex, old);
    if let Err(errors) = schema::compat_manifest().validate(&serde_json::to_value(&with_old)?) {
        return Err(format!("manifest with the old codex invalid: {errors:?}").into());
    }
    Ok(())
}

#[test]
fn probe_fails_cleanly_when_the_cli_is_missing() -> R {
    let tmp = tempfile::tempdir()?;
    let env = ProbeEnv {
        path: tmp.path().as_os_str().to_owned(),
        fixture_root: None,
        timeout: Duration::from_secs(5),
        log: None,
        deadline: None,
    };
    assert!(probe(Agent::Pi, &env).is_err());
    Ok(())
}

#[test]
fn probe_is_bounded_by_its_timeout() -> R {
    let tmp = tempfile::tempdir()?;
    let (dir, _) = fake(tmp.path(), "slow", "claude", "#!/bin/sh\nexec sleep 30\n")?;
    let env = ProbeEnv {
        path: format!("{}:/usr/bin:/bin", dir.display()).into(),
        fixture_root: None,
        timeout: Duration::from_millis(500),
        log: None,
        deadline: None,
    };
    let started = std::time::Instant::now();
    assert!(probe(Agent::Claude, &env).is_err());
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "probe took {:?}",
        started.elapsed()
    );
    Ok(())
}

#[test]
fn probe_leaves_no_descendant_behind() -> R {
    // M1T01R-03: a probe command that starts a background child must not leave it running.
    let tmp = tempfile::tempdir()?;
    let pidfile = tmp.path().join("bg.pid");
    let help = tmp.path().join("help.txt");
    std::fs::write(&help, "--output-format stream-json --resume\n")?;
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) sleep 60 >/dev/null 2>&1 & echo $! > '{}'; echo '2.0.0 (Claude Code)';;\n --help) cat '{}';;\nesac\n",
        pidfile.display(),
        help.display()
    );
    let (dir, _) = fake(tmp.path(), "bg", "claude", &script)?;
    probe(Agent::Claude, &env_for(&dir))?;
    let pid = std::fs::read_to_string(&pidfile)?.trim().to_owned();
    std::thread::sleep(Duration::from_millis(200));
    let alive = std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| {
            !s.rsplit(')')
                .next()
                .unwrap_or_default()
                .trim_start()
                .starts_with('Z')
        })
        .unwrap_or(false);
    if alive {
        let _ = std::process::Command::new("kill")
            .args(["-9", &pid])
            .status();
    }
    assert!(!alive, "background child {pid} outlived the probe");
    Ok(())
}

#[test]
fn codex_schema_output_must_be_a_schema() -> R {
    // M1T01R-08: a schema command that exits 0 but writes nothing usable is a failure, not an empty schema.
    let tmp = tempfile::tempdir()?;
    let codex_fix = fixtures().join("codex/0.166.0");
    for (name, generate) in [
        ("empty", "mkdir -p \"$4\""),
        (
            "garbage",
            "mkdir -p \"$4\" && echo nope > \"$4/ClientRequest.json\"",
        ),
    ] {
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) {generate};; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        assert!(
            probe(Agent::Codex, &env_for(&dir)).is_err(),
            "{name} schema output accepted"
        );
    }
    Ok(())
}

#[test]
fn codex_capabilities_come_from_the_schema_method_lists() -> R {
    // M1T01R-15: JSON that is not a protocol schema is a failure; capabilities come from the request/notification
    // method lists, not from strings found anywhere in the output.
    let tmp = tempfile::tempdir()?;
    let codex_fix = fixtures().join("codex/0.166.0");
    let write = |files: &str| {
        format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) mkdir -p \"$4\" && cd \"$4\" && {files};; esac;;\nesac\n",
            f = codex_fix.display()
        )
    };
    let methods = |list: &[&str]| {
        let items: Vec<String> = list
            .iter()
            .map(|m| {
                format!(
                    "{{\"type\":\"object\",\"required\":[\"method\"],\"properties\":{{\"method\":{{\"type\":\"string\",\"enum\":[\"{m}\"]}}}}}}"
                )
            })
            .collect();
        format!("{{\"oneOf\":[{}]}}", items.join(","))
    };
    for (name, files) in [
        ("null", "echo null > ClientRequest.json && echo null > ServerNotification.json".to_owned()),
        ("array", "echo '[\"turn/steer\",\"thread/resume\"]' > ClientRequest.json && echo '{}' > ServerNotification.json".to_owned()),
        ("no-notifications", format!("echo '{}' > ClientRequest.json", methods(&["turn/steer"]))),
    ] {
        let (dir, _) = fake(tmp.path(), name, "codex", &write(&files))?;
        assert!(probe(Agent::Codex, &env_for(&dir)).is_err(), "{name} schema output accepted");
    }
    // A minimal well-formed schema: steer requests exist, resume and deltas do not; a stray string elsewhere is ignored.
    let files = format!(
        "echo '{}' > ClientRequest.json && echo '{}' > ServerNotification.json && echo '\"thread/resume\"' > Other.json",
        methods(&["initialize", "turn/steer"]),
        methods(&["turn/completed"])
    );
    let (dir, _) = fake(tmp.path(), "minimal", "codex", &write(&files))?;
    let entry = probe(Agent::Codex, &env_for(&dir))?;
    assert_eq!(entry.capabilities.get("mid_turn_steer"), Some(&Some(true)));
    assert_eq!(entry.capabilities.get("resume"), Some(&Some(false)));
    assert_eq!(
        entry.capabilities.get("partial_messages"),
        Some(&Some(false))
    );
    Ok(())
}

#[test]
fn malformed_method_branches_are_refused() -> R {
    // M1T01R-18: a method only counts if the branch naming it is a well-formed object schema with a string method.
    let tmp = tempfile::tempdir()?;
    let codex_fix = fixtures().join("codex/0.166.0");
    let notifications = "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/completed\"]}}}]}";
    for (name, requests) in [
        (
            "type-number",
            "{\"oneOf\":[{\"type\":123,\"properties\":{\"method\":{\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "method-number",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"number\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "enum-mixed",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"enum\":[\"turn/steer\",7]}}}]}",
        ),
    ] {
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) mkdir -p \"$4\" && cd \"$4\" && \
             echo '{requests}' > ClientRequest.json && echo '{notifications}' > ServerNotification.json;; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        assert!(
            probe(Agent::Codex, &env_for(&dir)).is_err(),
            "{name} accepted"
        );
    }
    Ok(())
}

#[test]
fn method_branches_must_use_the_known_schema_form() -> R {
    // M1T01R-23: keywords outside the form the App Server schema uses, or with ill-typed values, reject the file.
    let tmp = tempfile::tempdir()?;
    let codex_fix = fixtures().join("codex/0.166.0");
    let notifications = "{\"oneOf\":[{\"type\":\"object\",\"required\":[\"method\"],\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/completed\"]}}}]}";
    let mut accepted = Vec::new();
    for (name, requests) in [
        (
            "required-number",
            "{\"oneOf\":[{\"type\":\"object\",\"required\":123,\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "method-minlength",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"minLength\":\"bad\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "branch-unknown",
            "{\"oneOf\":[{\"type\":\"object\",\"patternProperties\":{},\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "title-number",
            "{\"oneOf\":[{\"type\":\"object\",\"title\":5,\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        // M1T01R-25: duplicate required names and ill-formed sibling property schemas.
        (
            "required-duplicate",
            "{\"oneOf\":[{\"type\":\"object\",\"required\":[\"method\",\"method\"],\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "params-type-number",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"type\":123}}}]}",
        ),
        (
            "params-unknown-keyword",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"minimum\":\"x\"}}}]}",
        ),
        (
            "params-ref-number",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"$ref\":5}}}]}",
        ),
        (
            "params-anyof-empty",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"anyOf\":[]}}}]}",
        ),
        (
            "params-anyof-bad-member",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"anyOf\":[{\"$ref\":\"#/definitions/P\"},{\"type\":\"nil\"}]}}}]}",
        ),
        // M1T01R-28: serde_json reads 0.9999999999999999 as 1.0; only integer literals count as integers.
        (
            "minlength-lossy",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"minLength\":0.9999999999999999}}}]}",
        ),
        // M1T01R-26/-27: references, the root, definitions and every branch are checked against draft-07.
        (
            "ref-not-uri",
            "{\"definitions\":{\"P\":true},\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"$ref\":\"not a URI\"}}}]}",
        ),
        (
            "ref-percent",
            "{\"definitions\":{\"P\":true},\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"anyOf\":[{\"$ref\":\"#bad%zz\"},{\"type\":\"null\"}]}}}]}",
        ),
        (
            "ref-unresolved",
            "{\"definitions\":{\"P\":true},\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"$ref\":\"#/definitions/Missing\"}}}]}",
        ),
        (
            "root-required-number",
            "{\"required\":123,\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "definition-type-number",
            "{\"definitions\":{\"P\":{\"type\":123}},\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]},\"params\":{\"$ref\":\"#/definitions/P\"}}}]}",
        ),
        (
            "branch-type-number",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}},{\"type\":123}]}",
        ),
        (
            "branch-without-method",
            "{\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}},{\"type\":\"object\"}]}",
        ),
        (
            "root-unknown-keyword",
            "{\"not\":{},\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
        (
            "schema-not-draft-07",
            "{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"oneOf\":[{\"type\":\"object\",\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"turn/steer\"]}}}]}",
        ),
    ] {
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) mkdir -p \"$4\" && cd \"$4\" && \
             echo '{requests}' > ClientRequest.json && echo '{notifications}' > ServerNotification.json;; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        if probe(Agent::Codex, &env_for(&dir)).is_ok() {
            accepted.push(name);
        }
    }
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
    Ok(())
}

/// Every command a probe runs is written down with its argv, exit code, duration and whether it timed out, in the
/// order run (REQ-006: doctor lists them).
#[test]
fn the_run_log_lists_every_command_a_probe_ran() -> R {
    let tmp = tempfile::tempdir()?;
    let f = fixtures().join("codex/0.166.0");
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
         app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
         *) exit 2;;\n esac;;\n *) exit 2;;\nesac\n",
        f = f.display()
    );
    let (dir, path) = fake(tmp.path(), "codex", "codex", &script)?;
    let log: RunLog = Default::default();
    let mut env = env_for(&dir);
    env.log = Some(log.clone());
    probe(Agent::Codex, &env)?;
    let runs = log.lock().map_err(|_| "poisoned")?.clone();
    let argv: Vec<Vec<String>> = runs.iter().map(|r| r.argv.clone()).collect();
    let program = path.display().to_string();
    assert_eq!(
        argv.first(),
        Some(&vec![program.clone(), "--version".to_owned()])
    );
    for args in [&["app-server", "--help"][..], &["exec", "--help"]] {
        let want: Vec<String> = std::iter::once(program.clone())
            .chain(args.iter().map(|a| (*a).to_owned()))
            .collect();
        assert!(argv.contains(&want), "{want:?} in {argv:?}");
    }
    assert!(
        argv.iter().any(|a| a
            .get(1..3)
            .is_some_and(|w| w == ["app-server", "generate-json-schema"])),
        "{argv:?}"
    );
    assert!(
        runs.iter().all(|r| r.exit_code == Some(0) && !r.timed_out),
        "{runs:?}"
    );
    Ok(())
}

/// A status command's output is captured on both streams (codex writes its login status to stderr); one that hangs
/// is stopped at the timeout with its whole group, and the log says so.
#[test]
fn a_status_command_is_captured_on_both_streams_and_bounded() -> R {
    let tmp = tempfile::tempdir()?;
    let (dir, path) = fake(
        tmp.path(),
        "talk",
        "codex",
        "#!/bin/sh\necho out-text\necho err-text >&2\nexit 1\n",
    )?;
    let log: RunLog = Default::default();
    let mut env = env_for(&dir);
    env.log = Some(log.clone());
    let out = run_status(&path, &["login", "status"], &env)?;
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stdout, b"out-text\n");
    assert_eq!(out.stderr, b"err-text\n");
    let pidfile = tmp.path().join("child.pid");
    let (_, slow) = fake(
        tmp.path(),
        "slow",
        "codex",
        &format!(
            "#!/bin/sh\nsleep 60 & echo $! > '{}'\nwait\n",
            pidfile.display()
        ),
    )?;
    env.timeout = Duration::from_millis(500);
    let started = std::time::Instant::now();
    let hung = run_status(&slow, &["login", "status"], &env);
    assert!(matches!(hung, Err(ProbeError::TimedOut { .. })), "{hung:?}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    let child = std::fs::read_to_string(&pidfile)?.trim().to_owned();
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !Path::new(&format!("/proc/{child}")).exists(),
        "the hung command's child {child} is still there"
    );
    let runs = log.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(runs.len(), 2);
    assert_eq!((runs[0].exit_code, runs[0].timed_out), (Some(1), false));
    assert_eq!((runs[1].exit_code, runs[1].timed_out), (None, true));
    assert_eq!(
        runs[1].argv,
        [
            slow.display().to_string(),
            "login".to_owned(),
            "status".to_owned()
        ]
    );
    Ok(())
}

/// With a shared deadline, each command gets only the time left before it, so slow commands one after another end
/// together within the deadline, not one timeout each (REQ-006: doctor ends within its limit + 5 s).
#[test]
fn a_shared_deadline_bounds_commands_run_one_after_another() -> R {
    let tmp = tempfile::tempdir()?;
    let (dir, slow) = fake(tmp.path(), "slow", "codex", "#!/bin/sh\nexec sleep 30\n")?;
    let mut env = env_for(&dir);
    env.timeout = Duration::from_secs(30);
    let started = std::time::Instant::now();
    env.deadline = Some(started + Duration::from_secs(1));
    let log: RunLog = Default::default();
    env.log = Some(log.clone());
    let first = run_status(&slow, &["login", "status"], &env);
    assert!(
        matches!(
            first,
            Err(ProbeError::TimedOut {
                cut_by_deadline: true,
                ..
            })
        ),
        "the first is cut short by the shared deadline: {first:?}"
    );
    // Once the deadline has passed, a command is not started at all, and says so (M1T07R-08).
    for _ in 0..2 {
        let late = run_status(&slow, &["login", "status"], &env);
        assert!(
            matches!(late, Err(ProbeError::NotStarted { .. })),
            "{late:?}"
        );
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let runs = log.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(
        runs.iter()
            .map(|r| (r.started, r.timed_out))
            .collect::<Vec<_>>(),
        [(true, true), (false, false), (false, false)]
    );
    Ok(())
}
