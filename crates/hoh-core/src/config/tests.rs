use super::*;
use std::path::Path;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// config/hoh.example.toml as committed.
fn example() -> R<String> {
    Ok(std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/hoh.example.toml"),
    )?)
}

/// A config file of `body` after the version line.
fn with(body: &str) -> String {
    format!("config_version = \"1.0\"\n{body}")
}

/// The error parsing `text` gives; a panic when it parses.
fn refused(text: &str) -> ConfigError {
    match parse(text) {
        Ok(config) => panic!("accepted: {config:?}\n{text}"),
        Err(error) => error,
    }
}

#[test]
fn test_018_01_no_config_and_the_example_give_the_same_effective_config() -> R {
    // AC-018-01: an empty project directory and one holding a verbatim copy of config/hoh.example.toml load to the same
    // effective configuration, field by field: the six default roles and the non-team defaults.
    let empty = tempfile::tempdir()?;
    let a = load(empty.path())?;
    let copied = tempfile::tempdir()?;
    std::fs::create_dir_all(copied.path().join(".hoh"))?;
    let text = example()?;
    std::fs::write(copied.path().join(FILE), &text)?;
    assert_eq!(std::fs::read_to_string(copied.path().join(FILE))?, text);
    let b = load(copied.path())?;
    assert_eq!(a, b);
    let agents: Vec<(&str, Agent)> = a.team.iter().map(|(n, r)| (n.as_str(), r.agent)).collect();
    assert_eq!(
        agents,
        vec![
            ("commander", Agent::Codex),
            ("cto", Agent::Claude),
            ("developer", Agent::Claude),
            ("qa", Agent::Pi),
            ("researcher", Agent::Pi),
            ("reviewer", Agent::Codex),
        ]
    );
    assert!(a.team["reviewer"].independent_session);
    assert_eq!(a.execution.writer_slots, 1);
    assert_eq!(a.execution.reader_slots, 2);
    assert!(a.execution.reserve_supervisor_slot);
    assert_eq!(a.supervision.policy, "balanced");
    assert_eq!(a.budget.max_local_repairs, 3);
    assert_eq!(a.budget.max_protocol_repairs, 2);
    assert_eq!(a.budget.max_usd, None, "no USD default");
    assert_eq!(a, Config::default());
    // The scenario's observations (TEST-018-01): both effective configurations and the example's sha256 (M2T07R-56).
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let out = std::process::Command::new("sha256sum")
            .arg(copied.path().join(FILE))
            .output()?;
        let sha = String::from_utf8(out.stdout)?
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_owned();
        std::fs::write(
            Path::new(&dir).join("test-018-01.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "no_config": a, "example": b, "example_sha256": sha,
            }))?,
        )?;
    }
    Ok(())
}

#[test]
fn every_key_of_the_key_table_is_read_and_the_example_lists_them_all() -> R {
    // REQ-018 decision 6 (CR-003): the example lists every key, optional ones as comments; each read value lands in
    // its field.
    let text = example()?;
    for key in [
        "default_mode",
        "document_profile",
        "profile",
        "writer_slots",
        "reader_slots",
        "reserve_supervisor_slot",
        "cancel_native_grace_seconds",
        "cancel_term_grace_seconds",
        "max_delivery_attempts",
        "writer_queue_capacity",
        "max_wait_rounds",
        "user_value",
        "unblock_value",
        "risk_reduction",
        "information_gain",
        "agent",
        "# model",
        "# fallback",
        "# prompt",
        "independent_session",
        "policy",
        "max_duration_hours",
        "max_iterations",
        "max_local_repairs",
        "max_protocol_repairs",
        "# max_usd",
        "# deadline_at",
        "view",
        "statusline",
        "mouse",
        "host",
        "port",
    ] {
        assert!(text.contains(&format!("{key} =")), "{key}");
    }
    let config = parse(&with(
        r#"
[project]
default_mode = "PLAN"
document_profile = "lite"
[execution]
profile = "yolo-vm"
writer_slots = 1
reader_slots = 3
reserve_supervisor_slot = false
cancel_native_grace_seconds = 7
cancel_term_grace_seconds = 4
[delivery]
max_delivery_attempts = 6
[persistence]
writer_queue_capacity = 128
[scheduler]
max_wait_rounds = 9
[scheduler.weights]
user_value = 1
unblock_value = 2
risk_reduction = 0
information_gain = 10
[team.developer]
agent = "codex"
model = "m-test"
fallback = ["claude", "pi"]
prompt = ".hoh/prompts/developer.md"
[supervision]
policy = "intensive"
[budget]
max_duration_hours = 1.5
max_iterations = 7
max_local_repairs = 0
max_protocol_repairs = 1
max_usd = 20.0
deadline_at = "2026-12-31T00:00:00Z"
[ui]
view = "team"
statusline = "standard"
mouse = false
[web]
host = "::1"
port = 9000
"#,
    ))?;
    assert_eq!(config.project.default_mode, "PLAN");
    assert_eq!(config.project.document_profile, "lite");
    assert_eq!(config.execution.reader_slots, 3);
    assert!(!config.execution.reserve_supervisor_slot);
    assert_eq!(config.execution.cancel_native_grace_seconds, 7);
    assert_eq!(config.execution.cancel_term_grace_seconds, 4);
    assert_eq!(config.delivery.max_delivery_attempts, 6);
    assert_eq!(config.persistence.writer_queue_capacity, 128);
    assert_eq!(config.scheduler.max_wait_rounds, 9);
    assert_eq!(
        config.scheduler.weights,
        crate::schedule::Weights {
            user_value: 1,
            unblock_value: 2,
            risk_reduction: 0,
            information_gain: 10
        }
    );
    let developer = &config.team["developer"];
    assert_eq!(developer.agent, Agent::Codex);
    assert_eq!(developer.model.as_deref(), Some("m-test"));
    assert_eq!(developer.fallback, vec![Agent::Claude, Agent::Pi]);
    assert_eq!(
        developer.prompt.as_deref(),
        Some(".hoh/prompts/developer.md")
    );
    assert_eq!(
        config.team["cto"],
        Config::default().team["cto"],
        "others kept"
    );
    assert_eq!(config.supervision.policy, "intensive");
    assert_eq!(config.budget.max_duration_hours, 1.5);
    assert_eq!(config.budget.max_iterations, 7);
    assert_eq!(config.budget.max_local_repairs, 0);
    assert_eq!(config.budget.max_protocol_repairs, 1);
    assert_eq!(config.budget.max_usd, Some(20.0));
    assert_eq!(
        config.budget.deadline_at.as_deref(),
        Some("2026-12-31T00:00:00Z")
    );
    assert_eq!(config.ui.view, "team");
    assert!(!config.ui.mouse);
    assert_eq!(config.web.host, "::1");
    assert_eq!(config.web.port, 9000);
    Ok(())
}

#[test]
fn an_unknown_key_anywhere_is_refused_with_its_key_path() {
    // REQ-018 decision 3 and 6: unknown keys are never ignored, at any depth.
    for (body, key) in [
        ("colour = 1", "colour"),
        ("[execution]\nwriter_slot = 1", "execution.writer_slot"),
        ("[scheduler.weights]\nspeed = 1", "scheduler.weights.speed"),
        ("[team.qa]\nagent = \"pi\"\nmodle = \"x\"", "team.qa.modle"),
        ("[plugins]\nx = 1", "plugins"),
        ("[budget]\nmax_eur = 1.0", "budget.max_eur"),
    ] {
        let error = refused(&with(body));
        assert_eq!(error.key.as_deref(), Some(key), "{body}");
        assert!(error.message.contains("unknown key"), "{error}");
        assert!(error.line.is_some(), "{error}");
    }
}

#[test]
fn an_invalid_value_is_refused_with_its_key_path() {
    // REQ-018: illegal values are never ignored nor replaced by a default; failure mode 1 (an agent that is not a
    // registered adapter) and 3 (reviewer.independent_session=false).
    for (body, key) in [
        ("[team.qa]\nagent = \"unknown\"", "team.qa.agent"),
        ("[team.qa]\nagent = 1", "team.qa.agent"),
        (
            "[team.developer]\nfallback = [\"claude\", \"gpt\"]",
            "team.developer.fallback[1]",
        ),
        (
            "[team.developer]\nfallback = \"claude\"",
            "team.developer.fallback",
        ),
        (
            "[team.developer]\nfallback = [\"pi\", \"pi\"]",
            "team.developer.fallback[1]",
        ),
        ("[team.developer]\nmodel = 3", "team.developer.model"),
        (
            "[team.reviewer]\nindependent_session = false",
            "team.reviewer.independent_session",
        ),
        ("[team]\nqa = 1", "team.qa"),
        ("[execution]\nwriter_slots = 2", "execution.writer_slots"),
        ("[execution]\nreader_slots = 0", "execution.reader_slots"),
        ("[execution]\nreader_slots = 2.0", "execution.reader_slots"),
        (
            "[execution]\nreserve_supervisor_slot = \"yes\"",
            "execution.reserve_supervisor_slot",
        ),
        ("[execution]\nprofile = \"bare\"", "execution.profile"),
        (
            "[execution]\ncancel_term_grace_seconds = -1",
            "execution.cancel_term_grace_seconds",
        ),
        ("[project]\ndefault_mode = \"auto\"", "project.default_mode"),
        (
            "[project]\ndocument_profile = \"huge\"",
            "project.document_profile",
        ),
        (
            "[delivery]\nmax_delivery_attempts = 0",
            "delivery.max_delivery_attempts",
        ),
        (
            "[persistence]\nwriter_queue_capacity = 0",
            "persistence.writer_queue_capacity",
        ),
        (
            "[scheduler]\nmax_wait_rounds = 0",
            "scheduler.max_wait_rounds",
        ),
        (
            "[scheduler.weights]\nuser_value = 11",
            "scheduler.weights.user_value",
        ),
        ("[supervision]\npolicy = \"lazy\"", "supervision.policy"),
        (
            "[budget]\nmax_duration_hours = 0",
            "budget.max_duration_hours",
        ),
        ("[budget]\nmax_iterations = 0", "budget.max_iterations"),
        ("[budget]\nmax_usd = -1.0", "budget.max_usd"),
        ("[budget]\ndeadline_at = \"tomorrow\"", "budget.deadline_at"),
        ("[ui]\nview = \"grid\"", "ui.view"),
        ("[ui]\nmouse = 1", "ui.mouse"),
        ("[web]\nhost = \"0.0.0.0\"", "web.host"),
        ("[web]\nport = 0", "web.port"),
        ("[web]\nport = 70000", "web.port"),
        ("execution = 1", "execution"),
    ] {
        let error = refused(&with(body));
        assert_eq!(error.key.as_deref(), Some(key), "{body}: {error}");
        assert!(error.line.is_some(), "{error}");
    }
}

#[test]
fn a_custom_role_needs_an_agent_and_a_prompt_or_a_built_in_contract() -> R {
    // REQ-018 decision 5: a custom role references a built-in role contract or gives a prompt override file.
    let config = parse(&with(
        "[team.security_reviewer]\nagent = \"codex\"\nprompt = \".hoh/prompts/security.md\"",
    ))?;
    let role = &config.team["security_reviewer"];
    assert_eq!(role.agent, Agent::Codex);
    assert_eq!(role.prompt.as_deref(), Some(".hoh/prompts/security.md"));
    assert!(!role.independent_session);
    assert_eq!(config.team.len(), 7);
    let config = parse(&with(
        "[team.second_reviewer]\nagent = \"claude\"\nprompt = \"builtin:reviewer\"",
    ))?;
    assert_eq!(
        config.team["second_reviewer"].prompt.as_deref(),
        Some("builtin:reviewer")
    );
    for (body, key) in [
        (
            "[team.auditor]\nmodel = \"x\"\nprompt = \"a.md\"",
            "team.auditor.agent",
        ),
        ("[team.auditor]\nagent = \"pi\"", "team.auditor.prompt"),
        (
            "[team.auditor]\nagent = \"pi\"\nprompt = \"builtin:auditor\"",
            "team.auditor.prompt",
        ),
        (
            "[team.auditor]\nagent = \"pi\"\nprompt = \"\"",
            "team.auditor.prompt",
        ),
        (
            "[team.\"Bad Name\"]\nagent = \"pi\"\nprompt = \"a.md\"",
            "team.Bad Name",
        ),
    ] {
        assert_eq!(refused(&with(body)).key.as_deref(), Some(key), "{body}");
    }
    Ok(())
}

#[test]
fn an_empty_model_is_the_native_default() -> R {
    // REQ-018: an empty model means the agent's own default (the attempt records the resolved one).
    let config = parse(&with("[team.cto]\nmodel = \"\""))?;
    assert_eq!(config.team["cto"].model, None);
    Ok(())
}

#[test]
fn a_version_other_than_1_0_or_none_is_refused() {
    // REQ-018 failure mode 5: config_version must be "1.0".
    for (text, key) in [
        ("config_version = \"2.0\"\n", "config_version"),
        ("config_version = 1.0\n", "config_version"),
        ("[ui]\nmouse = true\n", "config_version"),
    ] {
        let error = refused(text);
        assert_eq!(error.key.as_deref(), Some(key), "{text}");
    }
}

#[test]
fn a_file_that_does_not_parse_is_refused_with_its_line_and_column() -> R {
    // REQ-018 failure mode 5 and REQ-001: the host reports the file and where it fails; it never falls back to the
    // built-in defaults.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh"))?;
    std::fs::write(
        dir.path().join(FILE),
        "config_version = \"1.0\"\n[ui]\nmouse = tru\n",
    )?;
    let error = match load(dir.path()) {
        Ok(config) => panic!("accepted: {config:?}"),
        Err(error) => error,
    };
    assert_eq!(error.file.as_deref(), Some(dir.path().join(FILE).as_path()));
    assert_eq!(error.line, Some(3));
    assert!(error.column.is_some());
    let shown = error.to_string();
    assert!(
        shown.starts_with(&format!("{}:3:", dir.path().join(FILE).display())),
        "{shown}"
    );
    std::fs::write(
        dir.path().join(FILE),
        "config_version = \"1.0\"\n[ui]\n\nmous = true\n",
    )?;
    let error = load(dir.path()).err().ok_or("accepted")?;
    assert_eq!(
        (error.key.as_deref(), error.line),
        (Some("ui.mous"), Some(4))
    );
    assert!(error.to_string().contains("ui.mous"), "{error}");
    Ok(())
}

#[test]
fn a_config_that_is_not_a_regular_file_is_refused() -> R {
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(FILE))?;
    let error = load(dir.path()).err().ok_or("accepted")?;
    assert_eq!(error.file.as_deref(), Some(dir.path().join(FILE).as_path()));
    Ok(())
}

#[test]
fn a_team_patch_merges_keys_removes_roles_and_resets_keys_to_their_defaults() -> R {
    // ADR-022 decision 4: ConfigureTeam's bindings are an RFC 7396 merge patch over the team.
    let base = Config::default().team;
    let team = patch_team(
        &base,
        &serde_json::json!({
            "developer": {"agent": "codex", "model": "m-test", "fallback": ["claude"]},
            "cto": null,
            "auditor": {"agent": "pi", "prompt": "builtin:qa"},
        }),
    )?;
    assert_eq!(team["developer"].agent, Agent::Codex);
    assert_eq!(team["developer"].model.as_deref(), Some("m-test"));
    assert_eq!(team["developer"].fallback, vec![Agent::Claude]);
    assert!(!team.contains_key("cto"), "removed");
    assert_eq!(team["auditor"].prompt.as_deref(), Some("builtin:qa"));
    assert_eq!(team["qa"], base["qa"], "untouched");
    let reset = patch_team(
        &team,
        &serde_json::json!({"developer": {"agent": null, "model": null, "fallback": null}}),
    )?;
    assert_eq!(
        reset["developer"], base["developer"],
        "reset to the default"
    );
    assert!(reset["reviewer"].independent_session);
    let reset = patch_team(
        &base,
        &serde_json::json!({"reviewer": {"independent_session": null}}),
    )?;
    assert!(
        reset["reviewer"].independent_session,
        "the reviewer's default"
    );
    Ok(())
}

#[test]
fn a_team_patch_is_checked_by_the_files_rules_with_key_paths_under_bindings() {
    // REQ-018 failure modes 1 and 3, decisions 3 to 5.
    let base = Config::default().team;
    for (patch, key) in [
        (
            serde_json::json!({"qa": {"agent": "unknown"}}),
            "bindings.qa.agent",
        ),
        (serde_json::json!({"qa": null}), "bindings.qa"),
        (serde_json::json!({"commander": null}), "bindings.commander"),
        (
            serde_json::json!({"reviewer": {"independent_session": false}}),
            "bindings.reviewer.independent_session",
        ),
        (
            serde_json::json!({"qa": {"modle": "x"}}),
            "bindings.qa.modle",
        ),
        (serde_json::json!({"qa": "pi"}), "bindings.qa"),
        (
            serde_json::json!({"auditor": {"agent": "pi"}}),
            "bindings.auditor.prompt",
        ),
        (
            serde_json::json!({"auditor": {"prompt": "a.md"}}),
            "bindings.auditor.agent",
        ),
        (
            serde_json::json!({"Bad": {"agent": "pi", "prompt": "a.md"}}),
            "bindings.Bad",
        ),
        (
            serde_json::json!({"qa": {"fallback": ["pi", "pi"]}}),
            "bindings.qa.fallback[1]",
        ),
        (serde_json::json!({"qa": {"model": 1}}), "bindings.qa.model"),
        (serde_json::json!(["qa"]), "bindings"),
    ] {
        match patch_team(&base, &patch) {
            Ok(team) => panic!("{patch} accepted: {team:?}"),
            Err(error) => {
                assert_eq!(error.key.as_deref(), Some(key), "{patch}: {error}");
                assert_eq!((error.line, error.column), (None, None), "{patch}");
            }
        }
    }
    let optional = patch_team(&base, &serde_json::json!({"cto": null, "researcher": null}));
    assert!(
        optional.is_ok(),
        "cto and researcher are not required: {optional:?}"
    );
}

#[test]
fn a_prompt_override_must_be_a_readable_file_inside_the_project() -> R {
    // M2T07R-53 (REQ-018 decision 5, ADR-022 decision 11): a prompt override names a readable regular file inside the
    // project, or a built-in role contract; anything else is refused with its key path.
    let dir = tempfile::tempdir()?;
    std::fs::create_dir_all(dir.path().join(".hoh/prompts"))?;
    let with_prompt = |prompt: &str| -> R {
        std::fs::write(
            dir.path().join(FILE),
            format!(
                "config_version = \"1.0\"\n[team.auditor]\nagent = \"pi\"\nprompt = \"{prompt}\"\n"
            ),
        )?;
        Ok(())
    };
    for prompt in [
        ".hoh/prompts/auditor.md",
        "/etc/hostname",
        "../outside.md",
        ".hoh/prompts",
    ] {
        with_prompt(prompt)?;
        let error = load(dir.path()).err().ok_or(format!("{prompt} accepted"))?;
        assert_eq!(
            error.key.as_deref(),
            Some("team.auditor.prompt"),
            "{prompt}: {error}"
        );
    }
    std::fs::write(dir.path().join(".hoh/prompts/auditor.md"), "Audit.")?;
    with_prompt(".hoh/prompts/auditor.md")?;
    assert_eq!(
        load(dir.path())?.team["auditor"].prompt.as_deref(),
        Some(".hoh/prompts/auditor.md")
    );
    with_prompt("builtin:qa")?;
    assert!(load(dir.path()).is_ok());
    Ok(())
}

#[test]
fn a_fallback_to_the_roles_own_agent_is_refused() {
    // M2T07R-60: falling back to the agent the role already has is no fallback.
    for (body, key) in [
        (
            "[team.qa]\nfallback = [\"claude\", \"pi\"]",
            "team.qa.fallback[1]",
        ),
        (
            "[team.qa]\nagent = \"codex\"\nfallback = [\"codex\"]",
            "team.qa.fallback[0]",
        ),
    ] {
        assert_eq!(refused(&with(body)).key.as_deref(), Some(key), "{body}");
    }
    let error = patch_team(
        &Config::default().team,
        &serde_json::json!({"qa": {"fallback": ["pi"]}}),
    )
    .err();
    assert_eq!(
        error.and_then(|e| e.key).as_deref(),
        Some("bindings.qa.fallback[0]")
    );
}
