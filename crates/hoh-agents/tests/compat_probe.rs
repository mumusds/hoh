//! AC-027-01 / TEST-027-01: probe() on three groups of fake binaries yields schema-valid compat manifest entries with
//! the known hashes, the version text verbatim, capabilities from the probed help/schema, and identical results twice.

use hoh_agents::compat::{Agent, AgentEntry, CompatManifest, SuiteStatus, Transport};
use hoh_agents::probe::{ProbeEnv, probe, tree_sha256};
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
