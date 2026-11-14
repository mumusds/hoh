//! The registry against a fake agent CLI on a PATH of its own.

use super::*;
use hoh_agents::compat::{self, CompatManifest, Suite, SuiteStatus};
use hoh_core::staffing::{Compat, Fact, FileStamp, Usable};
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
    // identity; an agent not on PATH, or not an agent, is unavailable; what was seen is read without looking again,
    // an agent never looked at is unknown until the wanted ones are refreshed.
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
    let after = runs(dir.path());
    assert!(registry.look(Agent::Codex).is_err());
    assert!(matches!(registry.seen(Agent::Codex), Some(Err(_))));
    assert_eq!(registry.seen(Agent::Pi), None);
    let named: BTreeSet<String> = [
        "claude".to_owned(),
        "codex".to_owned(),
        "gpt".to_owned(),
        "pi".to_owned(),
    ]
    .into();
    let known = registry.availability();
    assert_eq!(known.available, Some(["claude".to_owned()].into()));
    assert_eq!(
        known.unavailable.keys().cloned().collect::<Vec<_>>(),
        ["codex".to_owned()]
    );
    assert!(known.unknown("pi"), "pi was not looked at yet");
    registry.want(&named);
    registry.refresh();
    assert_eq!(runs(dir.path()), after, "looked at unchanged, probed again");
    let known = registry.availability();
    assert!(
        known.unavailable.contains_key("pi") && !known.unknown("pi"),
        "{known:?}"
    );
    assert!(known.unavailable["codex"].contains("PATH"), "{known:?}");
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

#[test]
fn a_look_the_hosts_stop_ends_says_nothing_of_its_agent() -> R {
    // M2T07R-352 (docs/adr/ADR-022.md decision 10): a probe the stop ended is not a finding about the agent: the agent
    // stays not looked at (neither available nor unavailable), so no dispatch takes the host's stop for it.
    let dir = tempfile::tempdir()?;
    let script = dir.path().join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho $$ > '{pid}'\nexec sleep 30\n",
            pid = dir.path().join("pid").display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    let registry = Arc::new(Registry::new(
        format!("{}:/usr/bin:/bin", dir.path().display()).into(),
    ));
    let looking = registry.clone();
    let look = std::thread::spawn(move || looking.look(Agent::Claude));
    let end = std::time::Instant::now() + Duration::from_secs(20);
    while !dir.path().join("pid").exists() {
        assert!(std::time::Instant::now() < end, "the probe never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    registry.stop();
    let looked = look.join().map_err(|_| "the look panicked")?;
    assert!(looked.is_err(), "{looked:?}");
    assert_eq!(registry.seen(Agent::Claude), None);
    let known = registry.availability();
    assert!(
        !known.unavailable.contains_key("claude")
            && !known
                .available
                .as_ref()
                .is_some_and(|a| a.contains("claude")),
        "{known:?}"
    );
    Ok(())
}

#[test]
fn a_registry_reads_pis_settings_from_its_own_directory_only() -> R {
    // M2T17R-59 (docs/adr/ADR-024.md decision 7): which provider pi's login is checked for comes from the settings in
    // the registry's pi directory, never from the user's ~/.pi/agent behind a test's back. With a default provider
    // there, its status command runs for it and a clear "not logged in" is recorded; with none, no status command
    // runs and the login is not taken as missing.
    let bin = tempfile::tempdir()?;
    let help = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/pi/1.5.0/help.txt"
    );
    let script = bin.path().join("pi");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{runs}'\ncase \"$1\" in\n --version) echo '1.5.0';;\n --help) cat '{help}';;\n \
             auth) echo '{{\"status\": \"not_ready\", \"reason\": \"credentials_not_configured\"}}'; exit 1;;\n \
             *) exit 2;;\nesac\n",
            runs = bin.path().join("runs").display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    let path: OsString = format!("{}:/usr/bin:/bin", bin.path().display()).into();
    let wanted: BTreeSet<String> = ["pi".to_owned()].into();
    let configured = tempfile::tempdir()?;
    std::fs::write(
        configured.path().join("settings.json"),
        r#"{"defaultProvider": "zz-test-provider"}"#,
    )?;
    let registry = Registry::with_pi_dir(path.clone(), Some(configured.path().to_path_buf()));
    registry.want(&wanted);
    registry.refresh();
    let known = registry.availability();
    assert!(known.auth_missing.contains_key("pi"), "{known:?}");
    assert!(
        std::fs::read_to_string(bin.path().join("runs"))?
            .lines()
            .any(|l| l == "auth check --provider zz-test-provider --json --no-refresh"),
        "pi's status command was not run for the provider of the registry's directory"
    );
    std::fs::remove_file(bin.path().join("runs"))?;
    let empty = tempfile::tempdir()?;
    let registry = Registry::with_pi_dir(path, Some(empty.path().to_path_buf()));
    registry.want(&wanted);
    registry.refresh();
    let known = registry.availability();
    assert!(
        known.auth_missing.is_empty() && known.available == Some(["pi".to_owned()].into()),
        "{known:?}"
    );
    assert!(
        !std::fs::read_to_string(bin.path().join("runs"))?
            .lines()
            .any(|l| l.starts_with("auth")),
        "a status command ran with no provider in the registry's directory"
    );
    Ok(())
}

#[test]
fn each_actual_look_at_an_agent_moves_its_observations_identity_and_a_cached_one_does_not() -> R {
    // docs/adr/ADR-025.md decision 5 (-306, -355): an observation's identity is (host_epoch, n), n the registry's
    // actual looks at the agent (a probe, a PATH that lacks it, a login check); a look answered from the cache is none.
    let dir = tempfile::tempdir()?;
    fake_claude(dir.path(), "first")?;
    let registry = Registry::new(format!("{}:/usr/bin:/bin", dir.path().display()).into());
    assert!(registry.observations(3).is_empty(), "nothing looked at yet");
    registry.look(Agent::Claude)?;
    let first = registry.observations(3)["claude"];
    assert_eq!((first.host_epoch, first.available), (3, true));
    registry.look(Agent::Claude)?;
    assert_eq!(
        registry.observations(3)["claude"].n,
        first.n,
        "a cached look"
    );
    fake_claude(dir.path(), "second, a little longer")?;
    registry.look(Agent::Claude)?;
    assert!(
        registry.observations(3)["claude"].n > first.n,
        "probed again"
    );
    assert!(registry.look(Agent::Codex).is_err());
    let codex = registry.observations(3)["codex"];
    assert!(!codex.available);
    assert!(registry.look(Agent::Codex).is_err());
    assert!(
        registry.observations(3)["codex"].n > codex.n,
        "a PATH that lacks it is looked at each time"
    );
    Ok(())
}

/// The host epoch the registry tests give their snapshots.
const EPOCH: i64 = 7;

/// The suite set the tests' manifests are recorded under (a stand-in for the build's digest, decision 9).
fn suite_set() -> String {
    "5".repeat(64)
}

/// A fake `claude` in `dir` (`note` makes another identity) answering `--version`, `--help` with `help` (a shell
/// command) and `auth status --json`, logged in unless `dir/login` says `out`; each run's first argument is appended to
/// `dir/args`. With `slow_once` its first `--version` sleeps past any probe's limit and every later one answers.
fn claude_script(dir: &Path, note: &str, help: &str, slow_once: bool) -> R {
    let slow = if slow_once {
        format!(
            "if [ \"$1\" = --version ] && [ ! -e '{m}' ]; then : > '{m}'; exec sleep 30; fi\n",
            m = dir.join("slowed").display()
        )
    } else {
        String::new()
    };
    let script = dir.join("claude");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\n# {note}\necho \"$1\" >> '{args}'\n{slow}case \"$1\" in\n --version) echo '2.1.318 (Claude Code)';;\n \
             --help) {help};;\n auth) if [ \"$(cat '{login}' 2>/dev/null)\" = out ]; then \
             echo '{{\"loggedIn\": false}}'; exit 1; fi; echo '{{\"loggedIn\": true}}';;\n *) exit 2;;\nesac\n",
            args = dir.join("args").display(),
            login = dir.join("login").display(),
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn claude(dir: &Path, note: &str) -> R {
    claude_script(dir, note, &format!("cat '{HELP}'"), false)
}

/// How many times the fake was asked for its version: once per probe.
fn versions(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("args"))
        .map(|s| s.lines().filter(|l| *l == "--version").count())
        .unwrap_or(0)
}

fn login(dir: &Path, state: &str) -> R {
    std::fs::write(dir.join("login"), state)?;
    Ok(())
}

fn path_of(dir: &Path) -> OsString {
    format!("{}:/usr/bin:/bin", dir.display()).into()
}

fn wanted(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn usable(fact: Option<Fact>) -> R<Usable> {
    match fact {
        Some(Fact::Usable(usable)) => Ok(*usable),
        other => Err(format!("not usable: {other:?}").into()),
    }
}

/// What a turn of claude prepared now would run as: its fact's identity and the registry's generation now.
fn ran(registry: &Registry) -> R<Ran> {
    let snapshot = registry.snapshot(EPOCH);
    let identity = usable(snapshot.facts.get("claude").cloned())?.identity;
    let seen = snapshot
        .observations
        .get("claude")
        .ok_or("no observation")?;
    Ok(Ran {
        agent: Agent::Claude,
        identity,
        generation: (seen.host_epoch, seen.n),
    })
}

/// `project`'s manifest with claude's entry as the registry's last probe found it, its suite `status` under
/// [`suite_set`].
fn record(project: &Path, registry: &Registry, status: SuiteStatus) -> R {
    let ready = registry.seen(Agent::Claude).ok_or("not seen")??;
    let mut entry = ready.entry.clone();
    entry.suite = Suite {
        status,
        fixture_set_sha256: suite_set(),
        run_at: Some(Some("2026-11-14T00:00:00Z".to_owned())),
        cases: Some(Vec::new()),
    };
    let manifest = CompatManifest::new([(Agent::Claude, entry)].into());
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(
        project.join(".hoh/compat-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
}

fn file_stamp(path: &Path) -> R<FileStamp> {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::metadata(path)?;
    Ok(FileStamp {
        dev: m.dev(),
        ino: m.ino(),
        ctime_sec: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

#[test]
fn a_temporary_probe_failure_is_probed_again_within_auth_every_and_a_permanent_one_is_not() -> R {
    // docs/adr/ADR-030.md decision 2: a look's failure keeps its ProbeError class; a temporary one (here a probe that
    // timed out) is not cached by the files' stamps for good but probed again once AUTH_EVERY has passed, the same
    // files then answering; a permanent one (a CLI HoH cannot drive) stays cached until its files change.
    let every = Duration::from_millis(1500);
    let bin = tempfile::tempdir()?;
    claude_script(bin.path(), "slow", &format!("cat '{HELP}'"), true)?;
    let project = tempfile::tempdir()?;
    let registry =
        Registry::new(path_of(bin.path())).with_timings(Duration::from_millis(500), every);
    assert_eq!(registry.open_manifest(project.path()), None);
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    assert!(
        matches!(registry.seen(Agent::Claude), Some(Err(why)) if why.contains("timed out")),
        "{:?}",
        registry.seen(Agent::Claude)
    );
    assert_eq!(versions(bin.path()), 1);
    registry.refresh();
    assert_eq!(versions(bin.path()), 1, "probed again within AUTH_EVERY");
    std::thread::sleep(every + Duration::from_millis(100));
    registry.refresh();
    assert_eq!(versions(bin.path()), 2, "not probed again after AUTH_EVERY");
    assert!(
        matches!(registry.seen(Agent::Claude), Some(Ok(_))),
        "{:?}",
        registry.seen(Agent::Claude)
    );
    usable(registry.fact(Agent::Claude))?;
    // A permanent failure: probed once, kept while its files stay as they were.
    let other = tempfile::tempdir()?;
    claude_script(other.path(), "no interface", "echo 'usage: claude'", false)?;
    let registry =
        Registry::new(path_of(other.path())).with_timings(Duration::from_millis(500), every);
    registry.open_manifest(project.path());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    assert!(
        matches!(registry.fact(Agent::Claude), Some(Fact::Unsupported(_))),
        "{:?}",
        registry.fact(Agent::Claude)
    );
    std::thread::sleep(every + Duration::from_millis(100));
    registry.refresh();
    assert_eq!(
        versions(other.path()),
        1,
        "a permanent failure probed again"
    );
    Ok(())
}

#[test]
fn a_chmod_alone_moves_the_stamp_and_the_agent_is_probed_again() -> R {
    // Decision 2 (M2T15D-201): a file's stamp has its change time too, so a change of metadata only (a chmod to the
    // same mode) is probed again; the fact's stamp is the files' (dev, ino, ctime) as stat reads them.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "first")?;
    let project = tempfile::tempdir()?;
    let registry = Registry::new(path_of(bin.path()));
    registry.open_manifest(project.path());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    registry.refresh();
    assert_eq!(versions(bin.path()), 1);
    let launcher = bin.path().join("claude");
    let before = file_stamp(&launcher)?;
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while file_stamp(&launcher)? == before {
        assert!(std::time::Instant::now() < end, "the ctime never moved");
        std::thread::sleep(Duration::from_millis(10));
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    }
    registry.refresh();
    assert_eq!(versions(bin.path()), 2, "a chmod was not probed again");
    let fact = usable(registry.fact(Agent::Claude))?;
    assert_eq!(fact.stamp.launcher, file_stamp(&launcher)?);
    // claude's launcher is its implementation.
    assert_eq!(fact.stamp.implementation, fact.stamp.launcher);
    Ok(())
}

#[test]
fn a_turns_auth_failure_latches_login_missing_until_a_visible_recovery() -> R {
    // Decision 2 (M2T15D-07): a turn's BLOCKED_AUTH under the current identity and generation is the agent's fact; a
    // look that only says "logged in" does not clear it, a logged-out then logged-in transition or a new identity does.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "A")?;
    let project = tempfile::tempdir()?;
    let registry =
        Registry::new(path_of(bin.path())).with_timings(Duration::from_secs(20), Duration::ZERO);
    registry.open_manifest(project.path());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    let first = ran(&registry)?;
    assert_eq!(registry.turn_auth_failed(&first, "A1"), None, "not latched");
    let latched = Some(Fact::LoginMissing("turn A1 failed BLOCKED_AUTH".to_owned()));
    assert_eq!(registry.fact(Agent::Claude), latched);
    assert!(
        registry.availability().auth_missing.contains_key("claude"),
        "{:?}",
        registry.availability()
    );
    registry.refresh();
    assert_eq!(
        registry.fact(Agent::Claude),
        latched,
        "a look that only says logged in cleared it"
    );
    login(bin.path(), "out")?;
    registry.refresh();
    assert!(
        matches!(registry.fact(Agent::Claude), Some(Fact::LoginMissing(why)) if !why.starts_with("turn")),
        "{:?}",
        registry.fact(Agent::Claude)
    );
    login(bin.path(), "in")?;
    registry.refresh();
    usable(registry.fact(Agent::Claude))?;
    let second = ran(&registry)?;
    assert_eq!(registry.turn_auth_failed(&second, "A2"), None);
    assert_eq!(
        registry.fact(Agent::Claude),
        Some(Fact::LoginMissing("turn A2 failed BLOCKED_AUTH".to_owned()))
    );
    claude(bin.path(), "B")?;
    registry.refresh();
    let b = usable(registry.fact(Agent::Claude))?;
    assert_ne!(b.identity, second.identity, "the same identity");
    Ok(())
}

#[test]
fn a_late_auth_failure_of_an_older_identity_leaves_the_newer_one_usable() -> R {
    // M2T15D-07 (decision 2, K8): A's turn started at generation g1; B is looked at (g2, another identity) and
    // verified; A's late BLOCKED_AUTH is only a diagnostic, B stays usable and verified.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "A")?;
    let project = tempfile::tempdir()?;
    let registry = Registry::new(path_of(bin.path())).with_suite_set(&suite_set());
    registry.open_manifest(project.path());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    let a = ran(&registry)?;
    claude(bin.path(), "B, another identity")?;
    registry.refresh();
    record(project.path(), &registry, SuiteStatus::Pass)?;
    assert_eq!(registry.open_manifest(project.path()), None);
    let b = usable(registry.fact(Agent::Claude))?;
    assert_eq!(b.compat, Compat::Verified);
    assert_ne!(b.identity, a.identity);
    assert!(
        b.generation > a.generation,
        "{:?} {:?}",
        b.generation,
        a.generation
    );
    let said = registry.turn_auth_failed(&a, "A1");
    assert!(
        said.as_ref()
            .is_some_and(|d| d.kind == TURN_AUTH_IGNORED && d.detail.contains("A1")),
        "{said:?}"
    );
    assert_eq!(usable(registry.fact(Agent::Claude))?, b);
    Ok(())
}

#[test]
fn an_auth_failure_from_before_a_visible_login_does_not_latch() -> R {
    // M2T15D-07: the same identity, but the turn started before a logged-out then logged-in transition: its failure is
    // older than the latest recovery and only said; a turn started after it latches.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "A")?;
    let project = tempfile::tempdir()?;
    let registry =
        Registry::new(path_of(bin.path())).with_timings(Duration::from_secs(20), Duration::ZERO);
    registry.open_manifest(project.path());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    let before = ran(&registry)?;
    login(bin.path(), "out")?;
    registry.refresh();
    login(bin.path(), "in")?;
    registry.refresh();
    let now = usable(registry.fact(Agent::Claude))?;
    assert_eq!(now.identity, before.identity);
    assert!(
        registry
            .turn_auth_failed(&before, "A1")
            .is_some_and(|d| d.kind == TURN_AUTH_IGNORED),
        "latched"
    );
    assert_eq!(usable(registry.fact(Agent::Claude))?, now);
    let after = ran(&registry)?;
    assert_eq!(registry.turn_auth_failed(&after, "A2"), None);
    assert_eq!(
        registry.fact(Agent::Claude),
        Some(Fact::LoginMissing("turn A2 failed BLOCKED_AUTH".to_owned()))
    );
    Ok(())
}

#[test]
fn a_snapshot_reads_each_wanted_agents_fact_and_observation_and_fact_is_its_item() -> R {
    // Decisions 1 and 2 (M2T15D-51, -100): the snapshot has each wanted agent looked at, its fact and observation;
    // `verified` agrees with the fact; `fact(agent)` is the same moment's item; the usable fact carries the probe's
    // identity, the files' stamps, the look's generation and the session capabilities in the project.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "A")?;
    let project = tempfile::tempdir()?;
    let registry = Registry::new(path_of(bin.path())).with_suite_set(&suite_set());
    assert_eq!(registry.open_manifest(project.path()), None);
    registry.want(&wanted(&["claude", "codex"]));
    registry.refresh();
    let snapshot = registry.snapshot(EPOCH);
    assert_eq!(
        snapshot.facts.keys().collect::<Vec<_>>(),
        ["claude", "codex"],
        "{snapshot:?}"
    );
    assert_eq!(
        snapshot.observations.keys().collect::<Vec<_>>(),
        ["claude", "codex"]
    );
    for agent in [Agent::Claude, Agent::Codex] {
        assert_eq!(
            registry.fact(agent).as_ref(),
            snapshot.facts.get(agent.as_str())
        );
    }
    assert!(matches!(
        snapshot.facts["codex"],
        Fact::NotInstalled(ref why) if why.contains("PATH")
    ));
    let codex = snapshot.observations["codex"];
    assert!(!codex.available && !codex.verified);
    let claude = usable(snapshot.facts.get("claude").cloned())?;
    let ready = registry.seen(Agent::Claude).ok_or("not seen")??;
    assert_eq!(claude.version, ready.version);
    assert_eq!(claude.identity, ready.entry.identity());
    assert_eq!(claude.stamp, ready.stamp);
    assert_eq!(claude.generation.0, EPOCH);
    assert!(claude.generation.1 <= snapshot.observations["claude"].n);
    assert_eq!(
        claude.session,
        ready.session_capabilities(Agent::Claude, project.path())?
    );
    assert_eq!(
        claude.granted,
        hoh_core::decision::granted_capabilities(&claude.session)
    );
    assert!(matches!(claude.compat, Compat::Pending(_)));
    let seen = snapshot.observations["claude"];
    assert_eq!(seen.host_epoch, EPOCH);
    assert!(seen.available && seen.logged_in && !seen.verified);
    // Verified once the manifest's entry matches, and not once it records a failure.
    record(project.path(), &registry, SuiteStatus::Pass)?;
    assert_eq!(registry.open_manifest(project.path()), None);
    let snapshot = registry.snapshot(EPOCH);
    assert_eq!(
        usable(snapshot.facts.get("claude").cloned())?.compat,
        Compat::Verified
    );
    assert!(snapshot.observations["claude"].verified);
    assert_eq!(
        registry.fact(Agent::Claude).as_ref(),
        snapshot.facts.get("claude")
    );
    record(project.path(), &registry, SuiteStatus::Fail)?;
    registry.open_manifest(project.path());
    let snapshot = registry.snapshot(EPOCH);
    assert!(matches!(
        usable(snapshot.facts.get("claude").cloned())?.compat,
        Compat::Failed(_)
    ));
    assert!(!snapshot.observations["claude"].verified);
    // An agent not wanted is not in the snapshot, whatever was seen of it.
    registry.want(&wanted(&["codex"]));
    let snapshot = registry.snapshot(EPOCH);
    assert_eq!(snapshot.facts.keys().collect::<Vec<_>>(), ["codex"]);
    Ok(())
}

#[test]
fn a_manifest_missing_unreadable_or_not_schema_valid_is_empty_and_leftovers_go() -> R {
    // Decision 10 ("清单只由 host 写，放在注册表里"): a missing manifest is empty; one that cannot be read or is not
    // schema-valid is empty too, with a `compat.manifest_unreadable` diagnostic; temporary files a crashed publish left
    // in the same directory are deleted, nothing else.
    let bin = tempfile::tempdir()?;
    claude(bin.path(), "A")?;
    let project = tempfile::tempdir()?;
    let hoh = project.path().join(".hoh");
    std::fs::create_dir_all(&hoh)?;
    let leftover = compat::manifest_temp(project.path(), "123-4");
    std::fs::write(&leftover, "{")?;
    std::fs::write(hoh.join("other.tmp"), "kept")?;
    let registry = Registry::new(path_of(bin.path())).with_suite_set(&suite_set());
    assert_eq!(
        registry.open_manifest(project.path()),
        None,
        "a missing manifest"
    );
    assert!(!leftover.exists(), "a leftover temporary manifest was kept");
    assert!(hoh.join("other.tmp").exists());
    registry.want(&wanted(&["claude"]));
    registry.refresh();
    let compat = |registry: &Registry| usable(registry.fact(Agent::Claude)).map(|u| u.compat);
    assert!(matches!(compat(&registry)?, Compat::Pending(_)));
    record(project.path(), &registry, SuiteStatus::Pass)?;
    assert_eq!(registry.open_manifest(project.path()), None);
    assert_eq!(compat(&registry)?, Compat::Verified);
    let manifest = hoh.join("compat-manifest.json");
    let valid = std::fs::read(&manifest)?;
    let mut invalid: serde_json::Value = serde_json::from_slice(&valid)?;
    invalid["agents"]["claude"]["suite"]["fixture_set_sha256"] = serde_json::json!("not hex");
    for (what, bytes) in [
        ("not JSON", b"{".to_vec()),
        ("not schema-valid", serde_json::to_vec(&invalid)?),
    ] {
        std::fs::write(&manifest, &bytes)?;
        let said = registry.open_manifest(project.path());
        assert!(
            said.as_ref()
                .is_some_and(|d| d.kind == "compat.manifest_unreadable"),
            "{what}: {said:?}"
        );
        assert!(matches!(compat(&registry)?, Compat::Pending(_)), "{what}");
    }
    std::fs::write(&manifest, &valid)?;
    assert_eq!(registry.open_manifest(project.path()), None);
    assert_eq!(compat(&registry)?, Compat::Verified);
    std::fs::remove_file(&manifest)?;
    assert_eq!(registry.open_manifest(project.path()), None);
    assert!(matches!(compat(&registry)?, Compat::Pending(_)));
    Ok(())
}
