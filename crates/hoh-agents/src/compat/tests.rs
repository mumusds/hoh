//! `compat::verdict` against docs/adr/ADR-030.md decision 9 ("一个谓词") and the whole-identity comparison
//! (M2T15D-104): which manifest entry verifies a probed agent, and what its suite status then means.

use super::*;
use hoh_core::lifecycle::AttemptState;
use hoh_core::staffing::{AgentIdentity, Compat};

/// The suite set the table's matching entries were recorded under (a stand-in for this build's digest).
fn recorded_set() -> String {
    "5".repeat(64)
}

/// A probe's entry for codex (suite not run, as `probe` gives it).
fn probed() -> AgentEntry {
    AgentEntry {
        cli_version: "codex-cli 0.166.0".to_owned(),
        launcher_path: "/opt/codex/bin/codex".to_owned(),
        launcher_sha256: "1".repeat(64),
        implementation_path: "/opt/codex/vendor/codex".to_owned(),
        implementation_sha256: "2".repeat(64),
        transport: Transport::AppServer,
        help_sha256: "3".repeat(64),
        schema_sha256: Some("4".repeat(64)),
        capabilities: BTreeMap::new(),
        suite: Suite {
            status: SuiteStatus::NotRun,
            fixture_set_sha256: "e".repeat(64),
            run_at: None,
            cases: None,
        },
    }
}

/// The manifest's entry for the probed agent with `status`, run at `run_at`, under the table's suite set.
fn recorded(status: SuiteStatus, run_at: Option<Option<String>>) -> AgentEntry {
    let mut entry = probed();
    entry.suite = Suite {
        status,
        fixture_set_sha256: recorded_set(),
        run_at,
        cases: Some(vec![SuiteCase {
            case: "success".to_owned(),
            status,
        }]),
    };
    entry
}

fn manifest(entry: AgentEntry) -> CompatManifest {
    CompatManifest::new([(Agent::Codex, entry)].into())
}

fn is_pending(compat: &Compat) -> bool {
    matches!(compat, Compat::Pending(_))
}

#[test]
fn an_entry_verifies_only_the_whole_probed_identity_and_hashes_of_this_suite_set() {
    let probe = probed();
    let set = recorded_set();
    let pass = manifest(recorded(
        SuiteStatus::Pass,
        Some(Some("2026-11-14T00:00:00Z".into())),
    ));
    assert_eq!(
        verdict(Some(&pass), "codex", &probe, &set),
        Compat::Verified,
        "the matching PASS entry"
    );
    assert!(
        is_pending(&verdict(None, "codex", &probe, &set)),
        "no manifest"
    );
    assert!(
        is_pending(&verdict(Some(&pass), "claude", &probe, &set)),
        "no entry for the agent"
    );
    assert!(
        is_pending(&verdict(Some(&pass), "gpt", &probe, &set)),
        "not an agent"
    );
    // Each compared item differing alone: pending, whatever the suite said (decision 9, M2T15D-52, -102, -104).
    type Change = fn(&mut AgentEntry);
    let changes: [(&str, Change); 8] = [
        ("launcher_path", |e| {
            e.launcher_path = "/usr/bin/codex".into()
        }),
        ("launcher_sha256", |e| e.launcher_sha256 = "6".repeat(64)),
        ("implementation_path", |e| {
            e.implementation_path = "/opt/codex/vendor/codex-copy".into();
        }),
        ("implementation_sha256", |e| {
            e.implementation_sha256 = "7".repeat(64);
        }),
        ("help_sha256", |e| e.help_sha256 = "8".repeat(64)),
        ("schema_sha256", |e| e.schema_sha256 = None),
        ("cli_version", |e| {
            e.cli_version = "codex-cli 0.167.0".into()
        }),
        ("fixture_set_sha256", |e| {
            e.suite.fixture_set_sha256 = "9".repeat(64);
        }),
    ];
    for (item, change) in changes {
        let mut entry = recorded(SuiteStatus::Pass, Some(Some("2026-11-14T00:00:00Z".into())));
        change(&mut entry);
        let compat = verdict(Some(&manifest(entry)), "codex", &probe, &set);
        assert!(is_pending(&compat), "{item} differs: {compat:?}");
    }
    // The suite's status of a matching entry.
    let failed = manifest(recorded(
        SuiteStatus::Fail,
        Some(Some("2026-11-14T00:00:00Z".into())),
    ));
    assert!(matches!(
        verdict(Some(&failed), "codex", &probe, &set),
        Compat::Failed(_)
    ));
    let undecided = manifest(recorded(
        SuiteStatus::NotRun,
        Some(Some("2026-11-14T00:00:00Z".into())),
    ));
    assert!(matches!(
        verdict(Some(&undecided), "codex", &probe, &set),
        Compat::Undecided(_)
    ));
    for run_at in [None, Some(None)] {
        let probed_only = manifest(recorded(SuiteStatus::NotRun, run_at.clone()));
        assert!(
            is_pending(&verdict(Some(&probed_only), "codex", &probe, &set)),
            "NOT_RUN without run_at ({run_at:?}): only probed"
        );
    }
}

#[test]
fn an_entry_differing_only_in_its_implementation_path_is_pending() {
    // M2T15D-104: the same version line and hashes under another implementation path are another identity.
    let mut entry = recorded(SuiteStatus::Pass, Some(Some("2026-11-14T00:00:00Z".into())));
    entry.implementation_path = "/opt/codex/vendor/elsewhere/codex".to_owned();
    let probe = probed();
    assert_eq!(
        (
            &entry.implementation_sha256,
            &entry.cli_version,
            &entry.help_sha256
        ),
        (
            &probe.implementation_sha256,
            &probe.cli_version,
            &probe.help_sha256
        )
    );
    assert_ne!(entry.identity(), probe.identity());
    assert!(is_pending(&verdict(
        Some(&manifest(entry)),
        "codex",
        &probe,
        &recorded_set()
    )));
}

#[test]
fn an_entrys_identity_is_its_four_identity_fields() {
    assert_eq!(
        probed().identity(),
        AgentIdentity {
            launcher_path: "/opt/codex/bin/codex".to_owned(),
            launcher_sha256: "1".repeat(64),
            implementation_path: "/opt/codex/vendor/codex".to_owned(),
            implementation_sha256: "2".repeat(64),
        }
    );
}

#[test]
fn a_pass_of_another_suite_set_is_not_verified_by_this_build() {
    // A schema-valid PASS that matches the probe in everything but its suite set is pending under this build's
    // `SUITE_SET_SHA256` (decision 9, M2T15D-102).
    let entry = recorded(SuiteStatus::Pass, Some(Some("2026-11-14T00:00:00Z".into())));
    let document = manifest(entry);
    assert!(
        hoh_core::schema::compat_manifest()
            .validate(&serde_json::to_value(&document).unwrap_or_default())
            .is_ok()
    );
    assert!(is_pending(&verdict(
        Some(&document),
        "codex",
        &probed(),
        &SUITE_SET_SHA256
    )));
}

/// Each suite's replay of `bytes`, at the recorded version its embedded cases were synthesized after.
fn replay(suite: &str, bytes: &[u8]) -> crate::adapter::Replayed {
    match suite {
        "claude" => crate::claude::replay(bytes, "2.1.318 (Claude Code)"),
        "codex" => crate::codex::replay(bytes, "codex-cli 0.166.0"),
        "codex-exec" => crate::codex::replay_exec(bytes, "codex-cli 0.166.0"),
        "pi" => crate::pi::replay(bytes, "1.5.0"),
        other => panic!("no suite {other}"),
    }
}

fn case_bytes(suite: &str, case: &str) -> &'static [u8] {
    fixtures::case(suite, case).unwrap_or_else(|| panic!("no embedded {suite}/{case}"))
}

#[test]
fn every_suite_embeds_the_six_cases_their_results_and_launch_variants() {
    // docs/adr/ADR-030.md decision 10, "嵌入的合成 fixture"; REQ-027 behaviour 3: six cases per adapter.
    let mut cases = fixtures::CASES.map(str::to_owned).to_vec();
    cases.sort();
    let table = fixtures::variants();
    for suite in fixtures::SUITES {
        let expected = fixtures::expected(suite);
        assert_eq!(
            expected.keys().cloned().collect::<Vec<_>>(),
            cases,
            "{suite}"
        );
        let mut resumed = (false, false);
        let mut model = (false, false);
        for case in fixtures::CASES {
            assert!(!case_bytes(suite, case).is_empty(), "{suite}/{case}");
            let variants = table.of(suite, case);
            assert!(!variants.is_empty(), "{suite}/{case} has launch variants");
            for name in variants {
                let variant = table.variant(name).expect("a variant the table defines");
                resumed = (resumed.0 || !variant.resume, resumed.1 || variant.resume);
                model = (model.0 || !variant.model, model.1 || variant.model);
            }
        }
        // The product's YOLO launch new and resumed, with and without a model.
        assert_eq!((resumed, model), ((true, true), (true, true)), "{suite}");
    }
    // Every embedded file is a case, a suite's expected results or the variant table.
    for (name, _) in fixtures::FILES {
        let known = *name == "variants.json"
            || fixtures::SUITES.iter().any(|suite| {
                *name == format!("{suite}/expected.json")
                    || fixtures::CASES
                        .iter()
                        .any(|case| *name == format!("{suite}/{case}.jsonl"))
            });
        assert!(known, "{name}");
    }
    assert_eq!(fixtures::FILES.len(), fixtures::SUITES.len() * 7 + 1);
}

#[test]
fn each_adapter_replays_its_embedded_cases_as_expected() {
    // docs/adr/ADR-030.md decision 10 (the third check of a case): the adapter's own judging over the embedded frames
    // gives the case's expected.json: final state, failure code and `adapter.unknown_event` count.
    for suite in fixtures::SUITES {
        let expected = fixtures::expected(suite);
        for case in fixtures::CASES {
            assert_eq!(
                replay(suite, case_bytes(suite, case)),
                expected[case],
                "{suite}/{case}"
            );
        }
    }
}

#[test]
fn a_replay_judges_the_frames_it_is_given() {
    // The replay is the adapters' judging, not a lookup: the host's cancel taken out of claude's cancel case leaves
    // claude's own end (FAILED error_during_execution); each unknown_fields case without its unknown frame keeps its end
    // and has no unknown event (AC-027-02's control); a stream cut before its end is incomplete.
    let without = |bytes: &[u8], pattern: &str| -> Vec<u8> {
        let text = std::str::from_utf8(bytes).expect("utf-8");
        let kept: Vec<&str> = text.lines().filter(|l| !l.contains(pattern)).collect();
        (kept.join("\n") + "\n").into_bytes()
    };
    let uncancelled = replay(
        "claude",
        &without(case_bytes("claude", "cancel"), "hoh_replay"),
    );
    assert_eq!(uncancelled.state, AttemptState::Failed);
    assert_eq!(
        uncancelled.failure.as_deref(),
        Some("error_during_execution")
    );
    for (suite, unknown) in [
        ("claude", "hoh_future"),
        ("codex", "hoh/futureNotification"),
        ("codex-exec", "hoh.future"),
        ("pi", "hoh_future"),
    ] {
        let full = replay(suite, case_bytes(suite, "unknown_fields"));
        let control = replay(
            suite,
            &without(case_bytes(suite, "unknown_fields"), unknown),
        );
        assert_eq!(full.unknown_events, 1, "{suite}");
        assert_eq!(control.unknown_events, 0, "{suite}");
        assert_eq!(
            (&full.state, &full.failure),
            (&control.state, &control.failure),
            "{suite}"
        );
        let success = case_bytes(suite, "success");
        let cut = &success[..success.len() / 2];
        let cut = &cut[..=cut.iter().rposition(|b| *b == b'\n').expect("a whole line")];
        let incomplete = replay(suite, cut);
        assert_eq!(incomplete.state, AttemptState::Failed, "{suite}");
        assert_eq!(
            incomplete.failure.as_deref(),
            Some("PROTOCOL_INCOMPLETE"),
            "{suite}"
        );
    }
}

#[test]
fn the_codex_tool_case_starts_a_command() {
    // docs/adr/ADR-030.md decision 12 measures command_execution with it.
    let started = std::str::from_utf8(case_bytes("codex", "tool"))
        .expect("utf-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .any(|frame| {
            frame["method"] == "item/started"
                && frame["params"]["item"]["type"] == "commandExecution"
        });
    assert!(started);
}

#[test]
fn the_embedded_fixtures_carry_no_machine_paths_or_credentials() {
    // docs/adr/ADR-030.md decision 10: the recordings stay in the tests; what the product embeds names no recording
    // machine's paths and holds nothing shaped like a credential.
    for (name, bytes) in fixtures::FILES {
        assert_eq!(fixtures::leaks(bytes), Vec::<String>::new(), "{name}");
    }
    // Positive controls: each shape is reported.
    for leaking in [
        r#"{"cwd":"/home/x"}"#,
        r#"{"cwd":"/Users/x/repo"}"#,
        r#"{"key":"sk-proj-abcdefghijklmnop0123456789"}"#,
        r#"{"access_token":"abc"}"#,
        r#"{"auth":"Bearer abc.def"}"#,
        r#"{"jwt":"eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl"}"#,
        r#"{"id":"0123456789abcdef0123456789abcdef01234567"}"#,
        "-----BEGIN PRIVATE KEY-----",
    ] {
        assert!(!fixtures::leaks(leaking.as_bytes()).is_empty(), "{leaking}");
    }
}

/// `members` with the one named `name` replaced by `bytes`.
fn replaced(members: &[(String, Vec<u8>)], name: &str, bytes: Vec<u8>) -> Vec<(String, Vec<u8>)> {
    assert!(members.iter().any(|(n, _)| n == name), "{name}");
    members
        .iter()
        .map(|(n, b)| (n.clone(), if n == name { bytes.clone() } else { b.clone() }))
        .collect()
}

#[test]
fn a_pass_of_another_request_definition_is_not_verified() {
    // M2T15D-102 (docs/adr/ADR-030.md decision 10, "套件集摘要"; K6): the suite set binds how this build builds, checks
    // and replays the requests, not only the fixture files, so a PASS recorded under another request definition is
    // pending for this build even though the agent and the fixtures are the same.
    let fixtures = fixture_members();
    let requests = request_members();
    let s1 = suite_set(&fixtures, &requests, &DEFINITION_SHA256);
    assert_eq!(s1, *SUITE_SET_SHA256);
    assert!(
        s1.len() == 64 && s1.bytes().all(|b| b.is_ascii_hexdigit()),
        "{s1}"
    );
    // A thread/start built with another parameter, everything else the same.
    let name = "requests/codex/success/new";
    let member = &requests.iter().find(|(n, _)| n == name).expect("member").1;
    let mut built: serde_json::Value = serde_json::from_slice(member).expect("canonical JSON");
    let start = built["messages"]
        .as_array_mut()
        .and_then(|m| m.iter_mut().find(|m| m["method"] == "thread/start"))
        .expect("the thread/start the builder gave");
    let mut changed = crate::codex::app_server::requests::thread_start(1, true, None);
    changed["params"]["sandbox"] = serde_json::json!("workspace-write");
    assert_ne!(*start, changed);
    *start = changed;
    let s2 = suite_set(
        &fixtures,
        &replaced(&requests, name, canonical(&built)),
        &DEFINITION_SHA256,
    );
    assert_ne!(s2, s1, "another thread/start");
    let s3 = suite_set(&fixtures, &requests, &"0".repeat(64));
    assert_ne!(s3, s1, "another definition");
    // The same agent, identity, hashes and version, a PASS: verified only under this build's suite set.
    let probe = probed();
    let pass_under = |set: &str| {
        let mut entry = recorded(SuiteStatus::Pass, Some(Some("2026-11-14T00:00:00Z".into())));
        entry.suite.fixture_set_sha256 = set.to_owned();
        manifest(entry)
    };
    assert_eq!(
        verdict(Some(&pass_under(&s1)), "codex", &probe, &SUITE_SET_SHA256),
        Compat::Verified
    );
    for (what, set) in [("request definition", &s2), ("suite definition", &s3)] {
        let document = pass_under(set);
        assert!(
            hoh_core::schema::compat_manifest()
                .validate(&serde_json::to_value(&document).unwrap_or_default())
                .is_ok()
        );
        let compat = verdict(Some(&document), "codex", &probe, &SUITE_SET_SHA256);
        assert!(is_pending(&compat), "another {what}: {compat:?}");
    }
    // The members: every embedded file, each (suite, case, variant)'s requests, the definition.
    let table = fixtures::variants();
    for suite in fixtures::SUITES {
        for case in fixtures::CASES {
            for variant in table.of(suite, case) {
                let name = format!("requests/{suite}/{case}/{variant}");
                assert!(requests.iter().any(|(n, _)| *n == name), "{name}");
            }
        }
    }
    for (name, _) in fixtures::FILES {
        let member = format!("fixtures/{name}");
        assert!(fixtures.iter().any(|(n, _)| *n == member), "{member}");
    }
    // The definition digest covers the builders, the validator, the replays and the embedded fixtures.
    for file in [
        "codex/app_server/requests.rs",
        "codex.rs",
        "claude.rs",
        "pi.rs",
        "adapter.rs",
        "probe.rs",
        "compat/suite.rs",
    ]
    .map(|f| format!("crates/hoh-agents/src/{f}"))
    .into_iter()
    .chain(
        fixtures::FILES
            .iter()
            .map(|(name, _)| format!("crates/hoh-agents/src/compat/fixtures/{name}")),
    ) {
        assert!(DEFINITION_FILES.contains(&file), "{file}");
    }
    // The dependencies through the lock's digest (tests/compat_definition.rs checks it is the workspace lock's).
    for file in [
        "crates/hoh-agents/Cargo.toml",
        "crates/hoh-agents/src/compat/definition/cargo-lock.sha256",
    ] {
        assert!(DEFINITION_FILES.iter().any(|f| f == file), "{file}");
    }
}

#[test]
fn the_request_members_are_what_the_builders_give() {
    // docs/adr/ADR-030.md decision 10: `requests/<suite>/<case>/<variant>` is the canonical JSON of what the request
    // builders give for the variant with the suite's fixed inputs; no builder refuses them.
    let members = request_members();
    let member = |name: &str| -> serde_json::Value {
        let bytes = &members.iter().find(|(n, _)| n == name).expect(name).1;
        assert_eq!(
            *bytes,
            canonical(&serde_json::from_slice(bytes).expect("JSON")),
            "{name}"
        );
        serde_json::from_slice(bytes).expect("JSON")
    };
    use crate::codex::app_server::requests;
    assert_eq!(
        member("requests/codex/success/new-model")["messages"],
        serde_json::json!([
            requests::initialize(0),
            requests::initialized(),
            requests::thread_start(1, true, Some(SUITE_MODEL)),
            requests::turn_start(2, Some(SUITE_SESSION), SUITE_PROMPT),
        ])
    );
    let resumed = member("requests/codex/compaction/resumed");
    assert_eq!(
        resumed["messages"][2],
        requests::thread_resume(1, true, None, SUITE_SESSION)
    );
    assert_eq!(
        member("requests/codex/cancel/new")["messages"][4],
        requests::turn_interrupt(3, SUITE_SESSION, SUITE_TURN)
    );
    let tool = member("requests/codex/tool/new");
    assert_eq!(tool["messages"][4]["method"], "turn/steer");
    assert_eq!(
        tool["answers"]["item/commandExecution/requestApproval"],
        serde_json::json!({"result": {"decision": "decline"}})
    );
    assert_eq!(
        member("requests/claude/compaction/resumed-model")["argv"],
        serde_json::json!([
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            crate::claude::YOLO_FLAG,
            "--resume",
            SUITE_SESSION,
            "--model",
            SUITE_MODEL
        ])
    );
    assert_eq!(
        member("requests/codex-exec/compaction/resumed")["argv"],
        serde_json::json!([
            "exec",
            "resume",
            "--json",
            crate::codex::YOLO_FLAG,
            SUITE_SESSION,
            "-"
        ])
    );
    let pi = member("requests/pi/success/new-model");
    let pi = pi["argv"].as_array().expect("argv");
    assert_eq!(
        pi[..2],
        [serde_json::json!("--mode"), serde_json::json!("rpc")]
    );
    assert!(pi.contains(&serde_json::json!(SUITE_MODEL)));
    for (name, bytes) in &members {
        let value: serde_json::Value = serde_json::from_slice(bytes).expect("JSON");
        assert!(value.get("refused").is_none(), "{name}: {value}");
    }
}
