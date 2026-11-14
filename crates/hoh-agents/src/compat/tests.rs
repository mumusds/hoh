//! `compat::verdict` against docs/adr/ADR-030.md decision 9 ("一个谓词") and the whole-identity comparison
//! (M2T15D-104): which manifest entry verifies a probed agent, and what its suite status then means.

use super::*;
use hoh_core::staffing::{AgentIdentity, Compat};

/// The suite set the table's matching entries were recorded under (a stand-in for this build's digest).
fn suite_set() -> String {
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
        fixture_set_sha256: suite_set(),
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
    let set = suite_set();
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
        &suite_set()
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
    // `SUITE_SET_SHA256` (decision 9, M2T15D-102); while that is the empty placeholder it matches no manifest the schema
    // accepts (64 hex), so no agent is verified: the conservative direction.
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
        SUITE_SET_SHA256
    )));
}
