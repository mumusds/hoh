//! The doctor's conclusions from status output and the compat manifest, and its JSON line (ADR-015).

use super::assess::{conclusion, protocol};
use super::report::{Auth, Protocol};
use hoh_agents::compat::{Agent, AgentEntry, CompatManifest, Suite, SuiteStatus, Transport};
use hoh_agents::probe::Captured;
use std::collections::BTreeMap;
use std::os::unix::process::ExitStatusExt;

fn out(code: i32, stdout: &str, stderr: &str) -> Captured {
    Captured {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn status_commands_say_logged_in_or_not_and_nothing_else_counts() {
    let canary = "token sk-CANARY-0123\n";
    for (agent, answer, want) in [
        (
            Agent::Codex,
            out(0, "", "Logged in using ChatGPT\n"),
            Some(Auth::NotVerified),
        ),
        (
            Agent::Codex,
            out(0, canary, "Logged in using an API key - sk-***\n"),
            Some(Auth::NotVerified),
        ),
        (
            Agent::Codex,
            out(1, "", "Not logged in\n"),
            Some(Auth::Missing),
        ),
        (
            Agent::Codex,
            out(1, "", "Error loading configuration\n"),
            None,
        ),
        (Agent::Codex, out(1, "", "Logged in using ChatGPT\n"), None),
        (
            Agent::Claude,
            out(
                0,
                "{\n  \"loggedIn\": true,\n  \"authMethod\": \"claude.ai\"\n}\n",
                "",
            ),
            Some(Auth::NotVerified),
        ),
        (
            Agent::Claude,
            out(1, "{\"loggedIn\": false}", ""),
            Some(Auth::Missing),
        ),
        (
            Agent::Claude,
            out(0, &format!("{canary}{{\"loggedIn\": true}}\n"), ""),
            Some(Auth::NotVerified),
        ),
        (Agent::Claude, out(0, "logged in", ""), None),
        // A positive answer counts only from a command that succeeded (M1T07R-12).
        (Agent::Claude, out(1, "{\"loggedIn\": true}", ""), None),
        (
            Agent::Pi,
            out(1, "{\"status\":\"ready\",\"provider\":\"p\"}\n", ""),
            None,
        ),
        (Agent::Claude, out(0, "{\"loggedIn\": \"yes\"}", ""), None),
        (
            Agent::Pi,
            out(0, "{\"status\":\"ready\",\"provider\":\"p\"}\n", ""),
            Some(Auth::NotVerified),
        ),
        (
            Agent::Pi,
            out(
                1,
                "{\"status\":\"not_ready\",\"provider\":\"p\",\"reason\":\"credentials_not_configured\"}\n",
                "",
            ),
            Some(Auth::Missing),
        ),
        (
            Agent::Pi,
            out(
                1,
                "{\"status\":\"not_ready\",\"reason\":\"provider_not_found\"}\n",
                "",
            ),
            None,
        ),
        (Agent::Pi, out(1, "", "boom"), None),
    ] {
        assert_eq!(
            conclusion(agent, &answer),
            want,
            "{agent:?} {:?}",
            String::from_utf8_lossy(&answer.stdout)
        );
    }
}

fn entry(transport: Transport) -> AgentEntry {
    AgentEntry {
        cli_version: "x 1".to_owned(),
        launcher_path: "/bin/x".to_owned(),
        launcher_sha256: "a".repeat(64),
        implementation_path: "/bin/x".to_owned(),
        implementation_sha256: "b".repeat(64),
        transport,
        help_sha256: "c".repeat(64),
        schema_sha256: Some("d".repeat(64)),
        capabilities: BTreeMap::new(),
        suite: Suite {
            status: SuiteStatus::NotRun,
            fixture_set_sha256: "f".repeat(64),
            run_at: None,
            cases: None,
        },
    }
}

#[test]
fn only_a_registered_passing_version_is_compatible() {
    let installed = entry(Transport::AppServer);
    assert_eq!(
        protocol(&installed, None, Agent::Codex).0,
        Protocol::Unknown,
        "no manifest"
    );
    let mut known = installed.clone();
    known.suite.status = SuiteStatus::Pass;
    let manifest =
        |e: &AgentEntry| CompatManifest::new(BTreeMap::from([(Agent::Codex, e.clone())]));
    assert_eq!(
        protocol(&installed, Some(&manifest(&known)), Agent::Codex).0,
        Protocol::Compatible
    );
    assert_eq!(
        protocol(&installed, Some(&manifest(&known)), Agent::Claude).0,
        Protocol::Unknown,
        "another agent"
    );
    for change in [
        |e: &mut AgentEntry| e.implementation_sha256 = "e".repeat(64),
        |e: &mut AgentEntry| e.help_sha256 = "e".repeat(64),
        |e: &mut AgentEntry| e.schema_sha256 = None,
        |e: &mut AgentEntry| e.suite.status = SuiteStatus::Fail,
        |e: &mut AgentEntry| e.suite.status = SuiteStatus::NotRun,
    ] {
        let mut other = known.clone();
        change(&mut other);
        assert_eq!(
            protocol(&installed, Some(&manifest(&other)), Agent::Codex).0,
            Protocol::Unknown
        );
    }
    let exec = entry(Transport::Exec);
    let mut known_exec = exec.clone();
    known_exec.suite.status = SuiteStatus::Pass;
    assert_eq!(
        protocol(&exec, Some(&manifest(&known_exec)), Agent::Codex).0,
        Protocol::Degraded
    );
}

#[test]
fn the_json_form_is_one_line_with_line_separators_escaped() {
    use super::report::{HohDir, Report, Runtime, Summary, Tool, json};
    let none = Tool {
        installed: false,
        path: None,
        version: None,
        protocol: Protocol::Unknown,
        detail: Some("a\u{2028}b\u{2029}c".to_owned()),
    };
    let report = Report {
        schema_version: "1.0",
        mode: "default",
        agents: BTreeMap::new(),
        runtime: Runtime {
            git: none.clone(),
            node: none,
            pi_plugins: Vec::new(),
            project_hoh: HohDir {
                path: "/p/.hoh".to_owned(),
                exists: false,
                writable: true,
                detail: None,
            },
            host: serde_json::json!({"state": "not_running"}),
        },
        summary: Summary {
            blocking: Vec::new(),
            unverified: Vec::new(),
            warnings: Vec::new(),
            exit_code: 0,
        },
        probes: Vec::new(),
    };
    let line = json(&report);
    assert!(line.ends_with('\n') && line.matches('\n').count() == 1);
    assert!(!line.contains('\u{2028}') && !line.contains('\u{2029}'));
    assert!(line.contains("a\\u2028b\\u2029c"));
    let back: serde_json::Value = serde_json::from_str(&line).expect("json");
    assert_eq!(back["runtime"]["git"]["detail"], "a\u{2028}b\u{2029}c");
}

#[test]
fn a_live_turn_is_verified_only_when_it_settled_with_the_token() {
    use super::live::auth_of;
    use hoh_core::lifecycle::AttemptState::*;
    assert_eq!(
        auth_of(Settled, None, Some("HOH-DOCTOR-OK")),
        Auth::Verified
    );
    assert_eq!(
        auth_of(Settled, None, Some("ok")),
        Auth::Unknown,
        "settled without the token"
    );
    assert_eq!(auth_of(Settled, None, None), Auth::Unknown);
    assert_eq!(auth_of(Failed, Some("BLOCKED_AUTH"), None), Auth::Missing);
    assert_eq!(
        auth_of(Failed, Some("PERMANENT_EXTERNAL"), Some("HOH-DOCTOR-OK")),
        Auth::Unknown
    );
    assert_eq!(
        auth_of(Cancelled, None, Some("HOH-DOCTOR-OK")),
        Auth::Unknown
    );
}

/// `doctor --live` drives a Pi version whose recorded traffic has `agent_settled` by that event, any other by the
/// `get_state` check, so a Pi without the event is verified rather than left to its deadline (M1R-06).
#[test]
fn live_pi_relies_on_agent_settled_only_for_a_recorded_version()
-> Result<(), Box<dyn std::error::Error>> {
    use super::live::{Prepared, prepare};
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir()?;
    let help = tmp.path().join("help.txt");
    std::fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/protocol/pi/1.5.0/help.txt"
        ),
        &help,
    )?;
    let launcher = tmp.path().join("pi");
    std::fs::write(&launcher, format!("#!/bin/sh\ncat '{}'\n", help.display()))?;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
    let env = hoh_agents::probe::ProbeEnv {
        path: std::env::var_os("PATH").unwrap_or_default(),
        fixture_root: None,
        timeout: std::time::Duration::from_secs(5),
        log: None,
        deadline: None,
        cancel: None,
    };
    for (version, settled) in [(Some("1.5.0"), true), (Some("1.6.0"), false), (None, false)] {
        let prepared = prepare(
            Agent::Pi,
            &launcher,
            tmp.path(),
            tmp.path().join("logs"),
            &env,
            &BTreeMap::new(),
            version,
        )?;
        let Prepared::Pi(turn) = prepared else {
            return Err("not a pi turn".into());
        };
        assert_eq!(turn.launch.settled_event, settled, "{version:?}");
    }
    Ok(())
}
