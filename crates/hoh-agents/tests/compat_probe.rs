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

/// A well-formed `ClientNotification.json` with what every App Server turn sends (M1R-04).
const CLIENT_NOTIFICATIONS: &str = "{\"oneOf\":[{\"type\":\"object\",\"required\":[\"method\"],\"properties\":{\"method\":{\"type\":\"string\",\"enum\":[\"initialized\"]}}}]}";

/// Whether the probe refused the schema output itself: a failure that names `file`.
fn schema_refused(result: &Result<AgentEntry, ProbeError>, file: &str) -> bool {
    matches!(result, Err(ProbeError::Failed { detail, .. }) if detail.contains(file))
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
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
         app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
         *) exit 2;;\n esac;;\n *) echo \"unknown command $1\" >&2; exit 2;;\nesac\n",
        f = codex_fix.display()
    );
    let old_codex = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.100.0';;\n --help) grep -v '^  app-server ' '{f}/help.txt';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
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

    // Help and schema hashes over every help probed (codex: `--help`, whose Commands column names the transports, then
    // `exec --help` and `app-server --help`); transports.
    let top_help = std::fs::read_to_string(codex_fix.join("help.txt"))?;
    let old_top_help: String = top_help
        .lines()
        .filter(|l| !l.starts_with("  app-server "))
        .map(|l| format!("{l}\n"))
        .collect();
    assert_ne!(old_top_help.len(), top_help.len());
    let exec_help = std::fs::read(codex_fix.join("help-exec.txt"))?;
    let app_help = std::fs::read(codex_fix.join("help-app-server.txt"))?;
    assert_eq!(
        new.help_sha256,
        sha256(&[top_help.into_bytes(), exec_help.clone(), app_help].concat())
    );
    assert_eq!(
        old.help_sha256,
        sha256(&[old_top_help.into_bytes(), exec_help].concat())
    );
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

/// The recorded help without the declaration of `flag` (the option line naming it, indented as declarations are);
/// other options' text that mentions the flag stays.
fn without_declaration(help: &str, flag: &str) -> String {
    help.lines()
        .filter(|l| {
            let t = l.trim_start();
            let declares = l.len() - t.len() <= 8
                && t.starts_with('-')
                && t.split("  ")
                    .next()
                    .unwrap_or(t)
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .any(|w| w.split(['=', '[']).next() == Some(flag));
            !declares
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

/// How an adapter would start a turn with this help: `true` for a turn with every option this test sets (YOLO,
/// resume, partial messages, provider, model), `false` for the plainest turn; the argv, or why it was blocked.
type Start = Box<dyn Fn(&str, bool) -> Result<Vec<String>, String>>;

/// An adapter under test: its name, a recorded help, how it starts a turn, and the probe's rule for it.
type Adapter = (&'static str, String, Start, fn(&str) -> Option<String>);

/// The options in an argv: words starting with `-`, other than `-` (stdin).
fn passed_options(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| a.starts_with('-') && a.as_str() != "-")
        .cloned()
        .collect()
}

/// Every option an adapter passes is declared by the help it was given, and the plainest turn is blocked exactly when
/// the probe refuses the CLI: with an option's declaration gone from the recorded help, a turn that would pass it is
/// blocked or does without it, and the plainest turn is blocked if and only if the probe's rule refuses that help.
/// Likewise a value the help lists for an option the plainest turn passes (claude's `stream-json`, pi's `rpc`, the App
/// Server's `stdio://`): without it the turn does not pass it, and is blocked if and only if the probe refuses. So the
/// probe neither accepts a CLI its adapter cannot start nor refuses one it can (M1R-03, M1T07R-27).
#[test]
fn every_option_an_adapter_passes_is_declared_and_the_probe_refuses_exactly_what_cannot_start() -> R
{
    use hoh_agents::adapter::{has_word, offers, option_text};
    use hoh_agents::{claude, codex, pi};
    let help = |path: &str| std::fs::read_to_string(fixtures().join(path));
    let claude_start: Start = Box::new(|help, full| {
        claude::argv(&claude::Launch {
            program: "claude".into(),
            help: help.to_owned(),
            cwd: "/".into(),
            yolo: full,
            partial: full,
            resume: full.then(|| "hoh-id".to_owned()),
            root: false,
            capabilities: Default::default(),
        })
        .map(|argv| argv.args)
        .map_err(|blocked| blocked.0)
    });
    let pi_start: Start = Box::new(|help, full| {
        pi::args(&pi::Launch {
            program: "pi".into(),
            help: help.to_owned(),
            cwd: "/".into(),
            session_dir: "/hoh-dir".into(),
            session_id: "hoh-id".to_owned(),
            resume: full,
            provider: full.then(|| "provider".to_owned()),
            model: full.then(|| "model".to_owned()),
            settled_event: false,
            capabilities: Default::default(),
        })
        .map_err(|blocked| blocked.0)
    });
    let exec_start: Start = Box::new(|help, full| {
        codex::exec_args(&codex::Launch {
            program: "codex".into(),
            app_server_help: None,
            exec_help: help.to_owned(),
            exec_resume_help: None,
            cwd: "/".into(),
            yolo: full,
            resume: None,
            model: full.then(|| "model".to_owned()),
            known: Default::default(),
            requests: Default::default(),
            client_notifications: Default::default(),
            capabilities: Default::default(),
        })
        .map_err(|blocked| blocked.0)
    });
    let app_server_start: Start =
        Box::new(|help, _| codex::app_server_args(help).map_err(|blocked| blocked.0));
    let cases: [Adapter; 4] = [
        (
            "claude",
            help("claude/2.1.318/help.txt")?,
            claude_start,
            claude::interface_missing,
        ),
        (
            "pi",
            help("pi/1.5.0/help.txt")?,
            pi_start,
            pi::interface_missing,
        ),
        (
            "codex exec",
            help("codex/0.166.0/help-exec.txt")?,
            exec_start,
            codex::exec_interface_missing,
        ),
        (
            "codex app-server",
            help("codex/0.166.0/help-app-server.txt")?,
            app_server_start,
            codex::app_server_launch_missing,
        ),
    ];
    let (mut values, mut blocking) = (Vec::new(), Vec::new());
    for (agent, help, start, interface_missing) in cases {
        assert_eq!(interface_missing(&help), None, "{agent}");
        let every = start(&help, false)?;
        let all = start(&help, true)?;
        for option in passed_options(&all) {
            let without = without_declaration(&help, &option);
            assert!(
                !offers(&without, &option),
                "{agent}: {option} is still declared"
            );
            if let Ok(args) = start(&without, true) {
                assert!(
                    !args.contains(&option),
                    "{agent} passes {option}, which its help does not declare"
                );
            }
            assert_eq!(
                start(&without, false).is_err(),
                interface_missing(&without).is_some(),
                "{agent} without {option}: the plainest turn blocked, the probe refusing"
            );
            if every.contains(&option) && start(&without, false).is_err() {
                blocking.push(format!("{agent} {option}"));
            }
        }
        for pair in every.windows(2) {
            let (option, value) = (&pair[0], &pair[1]);
            if option.starts_with('-')
                && option_text(&help, option).is_some_and(|text| has_word(&text, value))
            {
                let without = help.replace(value.as_str(), &format!("{value}-gone"));
                let plainest = start(&without, false);
                assert!(
                    !plainest.as_ref().is_ok_and(|args| args
                        .windows(2)
                        .any(|p| p[0] == *option && p[1] == *value)),
                    "{agent} passes {option} {value}, which its help does not list"
                );
                assert_eq!(
                    plainest.is_err(),
                    interface_missing(&without).is_some(),
                    "{agent} without {option} {value}: the plainest turn blocked, the probe refusing"
                );
                values.push(format!("{option} {value}"));
            }
        }
    }
    assert_eq!(
        values,
        [
            "--output-format stream-json",
            "--mode rpc",
            "--listen stdio://"
        ]
    );
    // The options without which nothing starts: those the probe requires.
    assert_eq!(
        blocking,
        [
            "claude -p",
            "claude --output-format",
            "claude --verbose",
            "pi --mode",
            "pi --session-dir",
            "pi --session-id",
            "codex exec --json"
        ]
    );
    Ok(())
}

/// A CLI its adapter cannot start is refused by the probe, by the adapter's own rule (M1R-03, M1T07R-14): claude
/// without `-p`, `--verbose` or `--output-format`, pi without `--mode`, `--session-id` or `--session-dir`. Only the
/// option's declaration is removed: a flag named in another option's text is not offered (claude's help mentions
/// `--output-format=stream-json` in two others).
#[test]
fn a_cli_without_what_its_adapter_needs_is_unsupported() -> R {
    let tmp = tempfile::tempdir()?;
    for (agent, fixture, version, flag) in [
        (
            Agent::Claude,
            "claude/2.1.318/help.txt",
            "2.1.318 (Claude Code)",
            "--output-format",
        ),
        (
            Agent::Claude,
            "claude/2.1.318/help.txt",
            "2.1.318 (Claude Code)",
            "-p",
        ),
        (
            Agent::Claude,
            "claude/2.1.318/help.txt",
            "2.1.318 (Claude Code)",
            "--verbose",
        ),
        (Agent::Pi, "pi/1.5.0/help.txt", "1.5.0", "--mode"),
        (Agent::Pi, "pi/1.5.0/help.txt", "1.5.0", "--session-id"),
        (Agent::Pi, "pi/1.5.0/help.txt", "1.5.0", "--session-dir"),
    ] {
        let recorded = std::fs::read_to_string(fixtures().join(fixture))?;
        let help = without_declaration(&recorded, flag);
        assert_eq!(
            recorded.lines().count() - help.lines().count(),
            1,
            "one declaration of {flag}"
        );
        let group = format!("{}{flag}", agent.command());
        let help_file = tmp.path().join(format!("{group}.txt"));
        std::fs::write(&help_file, &help)?;
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '{version}';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
            help_file.display()
        );
        let (dir, _) = fake(tmp.path(), &group, agent.command(), &script)?;
        match probe(agent, &env_for(&dir)) {
            Err(ProbeError::Unsupported { detail, .. }) => {
                assert!(detail.contains(flag), "{group}: {detail}");
            }
            other => return Err(format!("{group}: not refused: {other:?}").into()),
        }
    }
    // A value counts only in its option's own text: claude's --output-format offering text and json only (stream-json
    // left in --input-format's text), pi's --mode without rpc (rpc left elsewhere in the help).
    let claude = std::fs::read_to_string(fixtures().join("claude/2.1.318/help.txt"))?;
    let at = claude
        .find("  --output-format <format>")
        .ok_or("no --output-format block")?;
    let end = claude[at..]
        .find("  --permission-mode")
        .ok_or("no next option")?
        + at;
    let no_stream = format!(
        "{}{}{}",
        &claude[..at],
        claude[at..end].replace("stream-json", "ndjson"),
        &claude[end..]
    );
    assert!(
        no_stream.contains("stream-json"),
        "stream-json stays in other options' text"
    );
    let pi = std::fs::read_to_string(fixtures().join("pi/1.5.0/help.txt"))?;
    let no_rpc = pi.replace(
        "Output mode: text (default), json, or rpc",
        "Output mode: text (default) or json",
    );
    assert_ne!(no_rpc, pi);
    let no_rpc = format!("{no_rpc}\n  --rpc-socket <path>            Socket for rpc clients\n");
    for (agent, version, help, why) in [
        (
            Agent::Claude,
            "2.1.318 (Claude Code)",
            no_stream.clone(),
            "--output-format stream-json",
        ),
        (Agent::Pi, "1.5.0", no_rpc.clone(), "--mode rpc"),
    ] {
        let group = format!("{}-values", agent.command());
        let help_file = tmp.path().join(format!("{group}.txt"));
        std::fs::write(&help_file, &help)?;
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '{version}';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
            help_file.display()
        );
        let (dir, _) = fake(tmp.path(), &group, agent.command(), &script)?;
        match probe(agent, &env_for(&dir)) {
            Err(ProbeError::Unsupported { detail, .. }) => {
                assert!(detail.contains(why), "{group}: {detail}")
            }
            other => return Err(format!("{group}: not refused: {other:?}").into()),
        }
    }
    // A declaration is a declaration: claude's `--output-format` declaration gone, a continuation line indented six
    // spaces that starts with `--output-format=stream-json)` does not declare it (M1R-03); a value is a whole word:
    // `stream-json-v2` is not stream-json, `grpc` is not rpc (M1T07R-17).
    let without = without_declaration(&claude, "--output-format")
        + "  --other <x>                           Other\n      --output-format=stream-json)\n";
    let v2 = claude.replace("\"stream-json\"", "\"stream-json-v2\"");
    let grpc = pi.replace(
        "Output mode: text (default), json, or rpc",
        "Output mode: text (default), json, or grpc",
    );
    for (agent, version, help, why, group) in [
        (
            Agent::Claude,
            "2.1.318 (Claude Code)",
            without,
            "--output-format",
            "claude-continuation",
        ),
        (
            Agent::Claude,
            "2.1.318 (Claude Code)",
            v2,
            "--output-format stream-json",
            "claude-v2",
        ),
        (Agent::Pi, "1.5.0", grpc, "--mode rpc", "pi-grpc"),
    ] {
        let help_file = tmp.path().join(format!("{group}.txt"));
        std::fs::write(&help_file, &help)?;
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '{version}';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
            help_file.display()
        );
        let (dir, _) = fake(tmp.path(), group, agent.command(), &script)?;
        match probe(agent, &env_for(&dir)) {
            Err(ProbeError::Unsupported { detail, .. }) => {
                assert!(detail.contains(why), "{group}: {detail}")
            }
            other => return Err(format!("{group}: not refused: {other:?}").into()),
        }
    }
    // A usable CLI stays usable: rpc listed in a second declaration of pi's --mode, claude's values as a list.
    let second = pi.replace(
        "Output mode: text (default), json, or rpc",
        "Output mode: text (default) or json",
    ) + "  --mode <mode>                  Also: rpc\n";
    // (no_stream: claude's help with stream-json gone from --output-format's own text.)
    let listed = no_stream.replace(
        "  --output-format <format>              Output format (only works with --print):\n",
        "  --output-format <format>              Output format (only works with --print):\n      - stream-json\n",
    );
    assert_ne!(listed, no_stream);
    for (agent, version, help, group) in [
        (Agent::Pi, "1.5.0", second, "pi-second-mode"),
        (
            Agent::Claude,
            "2.1.318 (Claude Code)",
            listed,
            "claude-listed",
        ),
    ] {
        let help_file = tmp.path().join(format!("{group}.txt"));
        std::fs::write(&help_file, &help)?;
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '{version}';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
            help_file.display()
        );
        let (dir, _) = fake(tmp.path(), group, agent.command(), &script)?;
        probe(agent, &env_for(&dir)).map_err(|e| format!("{group}: refused: {e}"))?;
    }
    // A flag is a whole word: a codex without an App Server whose exec help offers `--json-lines` but not `--json`.
    let exec_help = std::fs::read_to_string(fixtures().join("codex/0.166.0/help-exec.txt"))?
        .replace("--json", "--json-lines");
    let help_file = tmp.path().join("codex-exec-help.txt");
    std::fs::write(&help_file, exec_help)?;
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.100.0';;\n --help) grep -v '^  app-server ' '{top}';;\n exec) [ \"$2\" = --help ] && cat '{}';;\n \
         *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        help_file.display(),
        top = fixtures().join("codex/0.166.0/help.txt").display()
    );
    let (dir, _) = fake(tmp.path(), "codex-json-lines", "codex", &script)?;
    match probe(Agent::Codex, &env_for(&dir)) {
        Err(ProbeError::Unsupported { detail, .. }) => {
            assert!(detail.contains("--json"), "{detail}")
        }
        other => return Err(format!("codex --json-lines: not refused: {other:?}").into()),
    }
    Ok(())
}

/// An App Server whose schema lacks anything a turn uses (a request or notification HoH sends on every turn, or a
/// notification it needs to end the turn and read its reply) is no App Server for HoH: the probe takes exec when
/// `codex exec` can run a turn, and refuses the CLI when it cannot either (M1T07R-15, M1R-04). Each method is taken
/// out of its file by renaming it, so the file stays a well-formed schema.
#[test]
fn an_app_server_without_any_method_a_turn_uses_falls_back_to_exec_or_is_unsupported() -> R {
    use hoh_agents::codex::{
        APP_SERVER_CLIENT_NOTIFICATIONS, APP_SERVER_NOTIFICATIONS, APP_SERVER_REQUESTS,
    };
    // The interface of ADR-015 decision 2; codex_adapter checks that the adapter sends nothing else (M1R-04).
    assert_eq!(
        APP_SERVER_REQUESTS,
        ["initialize", "thread/start", "turn/start", "turn/interrupt"]
    );
    assert_eq!(APP_SERVER_CLIENT_NOTIFICATIONS, ["initialized"]);
    assert_eq!(
        APP_SERVER_NOTIFICATIONS,
        ["turn/completed", "item/started", "item/completed"]
    );
    let tmp = tempfile::tempdir()?;
    let f = fixtures().join("codex/0.166.0");
    let lines_help = tmp.path().join("exec-json-lines.txt");
    std::fs::write(
        &lines_help,
        std::fs::read_to_string(f.join("help-exec.txt"))?.replace("--json", "--json-lines"),
    )?;
    for (file, methods) in [
        ("ClientRequest.json", APP_SERVER_REQUESTS),
        ("ClientNotification.json", APP_SERVER_CLIENT_NOTIFICATIONS),
        ("ServerNotification.json", APP_SERVER_NOTIFICATIONS),
    ] {
        for method in methods {
            let group = method.replace('/', "-");
            let schema = tmp.path().join(format!("schema-{group}"));
            std::fs::create_dir(&schema)?;
            let copied = std::process::Command::new("cp")
                .arg("-R")
                .arg(f.join("app-server-schema/."))
                .arg(&schema)
                .status()?;
            assert!(copied.success());
            let mut value: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(schema.join(file))?)?;
            let mut renamed = 0;
            for variant in value["oneOf"].as_array_mut().ok_or("no oneOf")? {
                for m in variant["properties"]["method"]["enum"]
                    .as_array_mut()
                    .into_iter()
                    .flatten()
                {
                    if m == method {
                        *m = serde_json::json!(format!("{method}-gone"));
                        renamed += 1;
                    }
                }
            }
            assert_eq!(renamed, 1, "{file} lists {method} once");
            std::fs::write(schema.join(file), serde_json::to_vec_pretty(&value)?)?;
            for (exec_help, usable) in
                [(f.join("help-exec.txt"), true), (lines_help.clone(), false)]
            {
                let script = format!(
                    "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
                     exec) [ \"$2\" = --help ] && cat '{e}';;\n \
                     app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{s}/.' \"$4/\";;\n  \
                     *) exit 2;;\n esac;;\n *) exit 2;;\nesac\n",
                    e = exec_help.display(),
                    f = f.display(),
                    s = schema.display()
                );
                let (dir, _) = fake(tmp.path(), &format!("{group}-{usable}"), "codex", &script)?;
                match (probe(Agent::Codex, &env_for(&dir)), usable) {
                    (Ok(entry), true) => {
                        assert_eq!(entry.transport, Transport::Exec, "without {method}");
                        assert_eq!(entry.capabilities.get("mid_turn_steer"), Some(&Some(false)));
                    }
                    (Err(ProbeError::Unsupported { detail, .. }), false) => {
                        assert!(
                            detail.contains(method) && detail.contains("--json"),
                            "{detail}"
                        );
                    }
                    (other, _) => return Err(format!("without {method}: {other:?}").into()),
                }
            }
        }
    }
    Ok(())
}

/// An App Server whose `--listen` does not list `stdio://` cannot be started on stdio: the probe takes exec when
/// `codex exec` can run a turn, and refuses the CLI when it cannot either (M1T07R-27).
#[test]
fn an_app_server_that_cannot_listen_on_stdio_falls_back_to_exec_or_is_unsupported() -> R {
    let tmp = tempfile::tempdir()?;
    let f = fixtures().join("codex/0.166.0");
    let lines_help = tmp.path().join("exec-json-lines.txt");
    std::fs::write(
        &lines_help,
        std::fs::read_to_string(f.join("help-exec.txt"))?.replace("--json", "--json-lines"),
    )?;
    // Another endpoint, or a longer URI that only starts like `stdio://` (M1T07R-27).
    for (n, other) in ["pipe://", "stdio:///socket"].into_iter().enumerate() {
        let app_help = tmp.path().join(format!("app-server-{n}.txt"));
        std::fs::write(
            &app_help,
            std::fs::read_to_string(f.join("help-app-server.txt"))?.replace("stdio://", other),
        )?;
        for (exec_help, usable) in [(f.join("help-exec.txt"), true), (lines_help.clone(), false)] {
            let script = format!(
                "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
                 exec) [ \"$2\" = --help ] && cat '{e}';;\n \
                 app-server) case \"$2\" in\n  --help) cat '{a}';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
                 *) exit 2;;\n esac;;\n *) exit 2;;\nesac\n",
                e = exec_help.display(),
                a = app_help.display(),
                f = f.display()
            );
            let (dir, _) = fake(
                tmp.path(),
                &format!("listen-{n}-{usable}"),
                "codex",
                &script,
            )?;
            match (probe(Agent::Codex, &env_for(&dir)), usable) {
                (Ok(entry), true) => assert_eq!(entry.transport, Transport::Exec, "{other}"),
                (Err(ProbeError::Unsupported { detail, .. }), false) => {
                    assert!(
                        detail.contains("stdio://") && detail.contains("--json"),
                        "{other}: {detail}"
                    )
                }
                (result, _) => {
                    return Err(format!("{other}, usable exec {usable}: {result:?}").into());
                }
            }
        }
    }
    Ok(())
}

/// codex's transports are subcommands, read from the Commands column of `codex --help`: a codex that lists neither
/// `exec` nor `app-server` cannot be driven, whatever those words do when run; one that lists them but cannot show
/// their help is a failed probe, not a missing interface (M1R-03).
#[test]
fn a_codex_without_its_subcommands_is_unsupported() -> R {
    let tmp = tempfile::tempdir()?;
    let f = fixtures().join("codex/0.166.0");
    let only_login = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n \
         --help) grep -v -e '^  exec ' -e '^  app-server ' '{f}/help.txt';;\n login) exit 0;;\n \
         exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
         *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        f = f.display()
    );
    let (dir, _) = fake(tmp.path(), "only-login", "codex", &only_login)?;
    match probe(Agent::Codex, &env_for(&dir)) {
        Err(ProbeError::Unsupported { detail, .. }) => {
            assert!(
                detail.contains("exec") && detail.contains("app-server"),
                "{detail}"
            )
        }
        other => return Err(format!("a codex with only login: {other:?}").into()),
    }
    // A listed subcommand whose help fails, the other one working: a failed probe, not the other transport (M1R-03).
    for (group, fails) in [("exec-fails", "exec"), ("app-server-fails", "app-server")] {
        let script = format!(
            "#!/bin/sh\n[ \"$1 $2\" = '{fails} --help' ] && {{ echo 'crashed' >&2; exit 1; }}\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n \
             exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in\n  --help) cat '{f}/help-app-server.txt';;\n  generate-json-schema) [ \"$3\" = --out ] && mkdir -p \"$4\" && cp -R '{f}/app-server-schema/.' \"$4/\";;\n  \
             *) exit 2;;\n esac;;\n *) exit 2;;\nesac\n",
            f = f.display()
        );
        let (dir, _) = fake(tmp.path(), group, "codex", &script)?;
        match probe(Agent::Codex, &env_for(&dir)) {
            Err(ProbeError::Failed { argv, .. }) => {
                assert_eq!(argv[1..], [fails, "--help"], "{group}: {argv:?}")
            }
            other => return Err(format!("{group}: {other:?}").into()),
        }
    }
    Ok(())
}

/// Probed capabilities follow the adapter's own whole-word rule: a claude whose help offers `--resume-v2` and
/// `--include-partial-messages-v2` offers neither resume nor partial messages (M1T07R-16).
#[test]
fn a_flag_with_a_longer_name_is_not_the_flag() -> R {
    let tmp = tempfile::tempdir()?;
    let help = std::fs::read_to_string(fixtures().join("claude/2.1.318/help.txt"))?
        .replace("--resume", "--resume-v2")
        .replace(
            "--include-partial-messages",
            "--include-partial-messages-v2",
        );
    let help_file = tmp.path().join("claude-v2.txt");
    std::fs::write(&help_file, help)?;
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n --help) cat '{}';;\n *) exit 2;;\nesac\n",
        help_file.display()
    );
    let (dir, _) = fake(tmp.path(), "claude-v2", "claude", &script)?;
    let entry = probe(Agent::Claude, &env_for(&dir))?;
    assert_eq!(entry.capabilities.get("resume"), Some(&Some(false)));
    assert_eq!(
        entry.capabilities.get("partial_messages"),
        Some(&Some(false))
    );
    // codex exec resumes only through a `resume` subcommand: a help whose Commands section lacks it, though its text
    // still says "resume", offers no resume (M1T07R-21).
    let exec_help = std::fs::read_to_string(fixtures().join("codex/0.166.0/help-exec.txt"))?;
    let without: String = exec_help
        .lines()
        .filter(|l| !l.starts_with("  resume "))
        .map(|l| format!("{l}\n"))
        .collect::<String>()
        + "\nA previous session cannot resume here.\n";
    assert!(without.contains("resume") && !without.contains("  resume "));
    let help_file = tmp.path().join("codex-no-resume.txt");
    std::fs::write(&help_file, without)?;
    let script = format!(
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.100.0';;\n --help) grep -v '^  app-server ' '{top}';;\n exec) [ \"$2\" = --help ] && cat '{}';;\n \
         *) echo \"error: unrecognized subcommand '$1'\" >&2; exit 2;;\nesac\n",
        help_file.display(),
        top = fixtures().join("codex/0.166.0/help.txt").display()
    );
    let (dir, _) = fake(tmp.path(), "codex-no-resume", "codex", &script)?;
    let entry = probe(Agent::Codex, &env_for(&dir))?;
    assert_eq!(
        (entry.transport, entry.capabilities.get("resume")),
        (Transport::Exec, Some(&Some(false)))
    );
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
    // A help the claude adapter can drive (M1T07R-14); this test is about the background child.
    std::fs::write(
        &help,
        "  -p, --print\n  --verbose\n  --output-format <format>  stream-json\n  --resume\n",
    )?;
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
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) {generate};; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        let result = probe(Agent::Codex, &env_for(&dir));
        assert!(
            schema_refused(&result, "ClientRequest.json"),
            "{name} schema output accepted: {result:?}"
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
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) cat '{f}/help-exec.txt';;\n \
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
    for (name, files, refused) in [
        ("null", "echo null > ClientRequest.json && echo null > ServerNotification.json".to_owned(), "ClientRequest.json"),
        ("array", "echo '[\"turn/steer\",\"thread/resume\"]' > ClientRequest.json && echo '{}' > ServerNotification.json".to_owned(), "ClientRequest.json"),
        ("no-notifications", format!("echo '{}' > ClientRequest.json && echo '{CLIENT_NOTIFICATIONS}' > ClientNotification.json", methods(&["turn/steer"])), "ServerNotification.json"),
    ] {
        let (dir, _) = fake(tmp.path(), name, "codex", &write(&files))?;
        let result = probe(Agent::Codex, &env_for(&dir));
        assert!(schema_refused(&result, refused), "{name} schema output accepted: {result:?}");
    }
    // A minimal well-formed schema with what every turn uses (M1T07R-15, M1R-04): steer requests exist, resume and
    // deltas do not; a stray string elsewhere is ignored.
    let files = format!(
        "echo '{}' > ClientRequest.json && echo '{}' > ClientNotification.json && echo '{}' > ServerNotification.json && echo '\"thread/resume\"' > Other.json",
        methods(&[hoh_agents::codex::APP_SERVER_REQUESTS, &["turn/steer"]].concat()),
        methods(hoh_agents::codex::APP_SERVER_CLIENT_NOTIFICATIONS),
        methods(hoh_agents::codex::APP_SERVER_NOTIFICATIONS)
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
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) mkdir -p \"$4\" && cd \"$4\" && \
             echo '{requests}' > ClientRequest.json && echo '{CLIENT_NOTIFICATIONS}' > ClientNotification.json && \
             echo '{notifications}' > ServerNotification.json;; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        let result = probe(Agent::Codex, &env_for(&dir));
        assert!(
            schema_refused(&result, "ClientRequest.json"),
            "{name} accepted: {result:?}"
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
            "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) cat '{f}/help-exec.txt';;\n \
             app-server) case \"$2\" in --help) cat '{f}/help-app-server.txt';; generate-json-schema) mkdir -p \"$4\" && cd \"$4\" && \
             echo '{requests}' > ClientRequest.json && echo '{CLIENT_NOTIFICATIONS}' > ClientNotification.json && \
             echo '{notifications}' > ServerNotification.json;; esac;;\nesac\n",
            f = codex_fix.display()
        );
        let (dir, _) = fake(tmp.path(), name, "codex", &script)?;
        if !schema_refused(&probe(Agent::Codex, &env_for(&dir)), "ClientRequest.json") {
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
        "#!/bin/sh\ncase \"$1\" in\n --version) echo 'codex-cli 0.166.0';;\n --help) cat '{f}/help.txt';;\n exec) [ \"$2\" = --help ] && cat '{f}/help-exec.txt';;\n \
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
    for args in [
        &["--help"][..],
        &["app-server", "--help"],
        &["exec", "--help"],
    ] {
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

/// The reads after the command exits end at the command's own limit: a helper that leaves the group and closes stdout
/// within the limit but stderr only after it makes the command time out, and the late answer is not taken. Two waits
/// that each got the time left would end past the limit (M1T07R-13).
#[test]
fn output_held_open_past_the_limit_by_an_escaped_helper_times_out() -> R {
    let tmp = tempfile::tempdir()?;
    let pidfile = tmp.path().join("helper.pid");
    let exited = tmp.path().join("launcher.exited");
    // The launcher exits only once the helper is in a session of its own, else the group kill could take it too; the
    // marker shows the launcher did exit, so the timeout below is the reads', not the wait for the launcher.
    let (dir, path) = fake(
        tmp.path(),
        "escape",
        "codex",
        &format!(
            "#!/bin/sh\nsetsid sh -c 'echo $$ > \"{pid}.tmp\"; mv \"{pid}.tmp\" \"{pid}\"; sleep 1; exec >&-; sleep 1.5; echo late >&2' &\n\
             while [ ! -s '{pid}' ]; do sleep 0.01; done\n: > '{exited}'\nexit 0\n",
            pid = pidfile.display(),
            exited = exited.display()
        ),
    )?;
    let log: RunLog = Default::default();
    let mut env = env_for(&dir);
    env.timeout = Duration::from_secs(2);
    env.log = Some(log.clone());
    let started = std::time::Instant::now();
    let held = run_status(&path, &["login", "status"], &env);
    let took = started.elapsed();
    if let Ok(pid) = std::fs::read_to_string(&pidfile) {
        let _ = std::process::Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", pid.trim())])
            .status();
    }
    assert!(
        matches!(
            held,
            Err(ProbeError::TimedOut {
                cut_by_deadline: false,
                ..
            })
        ),
        "{held:?}"
    );
    assert!(took < Duration::from_millis(2400), "{took:?}");
    assert!(exited.exists(), "the launcher did not exit");
    let runs = log.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(runs.len(), 1);
    assert_eq!((runs[0].exit_code, runs[0].timed_out), (None, true));
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

/// A launcher that cannot be started yet (ETXTBSY: it is still open for writing) is retried only while the shared
/// deadline lasts; past it, it is not started at all, even if it becomes startable later (M1T07R-11).
#[test]
fn a_busy_launcher_is_not_started_past_the_shared_deadline() -> R {
    let tmp = tempfile::tempdir()?;
    let (dir, path) = fake(tmp.path(), "busy", "codex", "#!/bin/sh\necho started\n")?;
    let writer = std::fs::OpenOptions::new().write(true).open(&path)?;
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(800));
        drop(writer);
    });
    let log: RunLog = Default::default();
    let mut env = env_for(&dir);
    env.log = Some(log.clone());
    let started = std::time::Instant::now();
    env.deadline = Some(started + Duration::from_millis(300));
    let result = run_status(&path, &["login", "status"], &env);
    let took = started.elapsed();
    release.join().map_err(|_| "release thread")?;
    assert!(
        matches!(result, Err(ProbeError::NotStarted { .. })),
        "{result:?}"
    );
    assert!(
        took < Duration::from_millis(700),
        "retried past the deadline: {took:?}"
    );
    let runs = log.lock().map_err(|_| "poisoned")?.clone();
    assert_eq!(runs.iter().map(|r| r.started).collect::<Vec<_>>(), [false]);
    Ok(())
}
