use super::*;
use crate::persistence::{FaultHooks, Options, Store, selftest};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

type R = Result<(), Box<dyn std::error::Error>>;

/// Commits fail while its flag is set: the test's fault hook (the product's does nothing, CR-006).
#[derive(Debug, Default)]
struct FailingCommits(AtomicBool);

impl FaultHooks for FailingCommits {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.load(Ordering::SeqCst) {
            Err(PersistError::Storage("injected".to_owned()))
        } else {
            Ok(())
        }
    }
}

fn hex(c: char) -> String {
    c.to_string().repeat(64)
}

fn entry() -> Value {
    json!({
        "cli_version": "codex-cli 0.166.0", "launcher_path": "/opt/bin/codex", "launcher_sha256": hex('a'),
        "implementation_path": "/opt/lib/codex", "implementation_sha256": hex('b'), "transport": "app-server",
        "help_sha256": hex('c'), "schema_sha256": hex('d'), "capabilities": {},
        "suite": {"status": "PASS", "fixture_set_sha256": hex('e'), "run_at": "2026-11-14T00:00:00Z", "cases": []},
    })
}

#[tokio::test]
async fn a_commit_failing_after_the_rename_keeps_the_suite_open_until_it_is_ended() -> R {
    // Decision 10 "发布": once the manifest was renamed a failed commit leaves the new manifest in place and the suite
    // open, so the writer slot stays closed (only more cautious) until the host's end of it commits.
    let dir = tempfile::tempdir()?;
    let faults = Arc::new(FailingCommits::default());
    let store = Store::open(
        &dir.path().join(".hoh/state.db"),
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            faults: faults.clone(),
            ..Options::default()
        },
    )?;
    let project = store.begin_host("p1", "/repo").await?.project_id;
    let subject = SuiteSubject {
        agent: "codex".to_owned(),
        cli_version: "codex-cli 0.166.0".to_owned(),
        identity: crate::staffing::AgentIdentity {
            launcher_path: "/opt/bin/codex".to_owned(),
            launcher_sha256: hex('a'),
            implementation_path: "/opt/lib/codex".to_owned(),
            implementation_sha256: hex('b'),
        },
    };
    let Admission::Started(suite) = store
        .begin_suite(&project, subject.clone(), SystemTime::now())
        .await?
    else {
        panic!("not started");
    };
    let files = crate::compat_manifest::Files::of(dir.path(), "t");
    faults.0.store(true, Ordering::SeqCst);
    let published = store
        .publish_suite(Publication {
            project_id: project.clone(),
            suite_id: suite.clone(),
            agent: "codex".to_owned(),
            entry: entry(),
            base: None,
            files: files.clone(),
            now: SystemTime::now(),
        })
        .await?;
    let Published::Uncommitted { manifest, error } = published else {
        panic!("not uncommitted: {published:?}");
    };
    assert!(error.contains("injected"), "{error}");
    let on_disk: Value = serde_json::from_slice(&std::fs::read(&files.path)?)?;
    assert_eq!(on_disk, manifest);
    // Still open: an admission of another identity is Busy with it; its end commits once the disk is back.
    let other = SuiteSubject {
        agent: "claude".to_owned(),
        ..subject
    };
    faults.0.store(false, Ordering::SeqCst);
    assert_eq!(
        store
            .begin_suite(&project, other.clone(), SystemTime::now())
            .await?,
        Admission::Busy(suite.clone())
    );
    assert!(store.end_suite(&project, &suite, "PASS", None).await?);
    assert!(matches!(
        store
            .begin_suite(&project, other, SystemTime::now())
            .await?,
        Admission::Started(_)
    ));
    Ok(())
}
