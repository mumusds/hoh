//! The registry against a fake agent CLI on a PATH of its own.

use super::*;
use std::os::unix::fs::PermissionsExt as _;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const HELP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/claude/2.1.318/help.txt"
);

/// A fake `claude` in `dir` answering `--version` and `--help` (the recorded help), counting its runs in `dir/runs`.
fn fake_claude(dir: &Path, note: &str) -> R {
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n# {note}\necho x >> '{runs}'\ncase \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n \
             --help) cat '{HELP}';;\n *) exit 2;;\nesac\n",
            runs = dir.join("runs").display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn runs(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("runs"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn an_agent_is_probed_once_until_its_files_change_and_a_missing_one_is_unavailable() -> R {
    // docs/adr/ADR-022.md decision 10: the registry probes lazily and caches by the launcher's and implementation's
    // identity; an agent not on PATH is unavailable.
    let dir = tempfile::tempdir()?;
    fake_claude(dir.path(), "first")?;
    let registry = Registry::new(format!("{}:/usr/bin:/bin", dir.path().display()).into());
    let ready = registry.look(Agent::Claude)?;
    assert_eq!(ready.version, "2.1.318 (Claude Code)");
    assert!(matches!(&ready.facts, launch::Facts::Claude { help } if !help.is_empty()));
    let probed = runs(dir.path());
    assert!(probed > 0);
    assert_eq!(registry.look(Agent::Claude)?, ready);
    assert_eq!(runs(dir.path()), probed, "probed again without a change");
    fake_claude(dir.path(), "second, a little longer")?;
    registry.look(Agent::Claude)?;
    assert!(
        runs(dir.path()) > probed,
        "a changed launcher was not probed again"
    );
    assert!(registry.look(Agent::Codex).is_err());
    assert_eq!(registry.unavailable(), ["codex".to_owned()].into());
    let capabilities = ready.session_capabilities(Agent::Claude, dir.path())?;
    assert_eq!(capabilities["structured_events"], true);
    Ok(())
}

#[test]
fn a_roles_needs_follow_its_contract() {
    // Decision 10 (REQ-020 decision 1, the named part).
    let set = |items: &[&str]| {
        items
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(required(Some("developer")), set(&["structured_events"]));
    assert_eq!(
        required(Some("reviewer")),
        set(&["resume", "structured_events"])
    );
    assert_eq!(
        required(Some("commander")),
        set(&["resume", "structured_events"])
    );
    assert_eq!(required(None), set(&["structured_events"]));
}
