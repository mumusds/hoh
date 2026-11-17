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
            ..Options::new(crate::spawn::Direct::shared())
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

/// The replacement of the manifest fails at `step` the first time it reaches it: the test's fault hook (CR-006; the
/// product's does nothing). After `Step::Renamed` that is the sync of the manifest's directory.
#[derive(Debug)]
struct FailingManifestStep {
    step: crate::compat_manifest::Step,
    failed: AtomicBool,
}

impl FaultHooks for FailingManifestStep {
    fn manifest_at(&self, step: crate::compat_manifest::Step) -> std::io::Result<()> {
        if step == self.step && !self.failed.swap(true, Ordering::SeqCst) {
            Err(std::io::Error::other("injected"))
        } else {
            Ok(())
        }
    }
}

/// `entry()` with another implementation hash.
fn entry_of(implementation: char) -> Value {
    let mut entry = entry();
    entry["implementation_sha256"] = json!(hex(implementation));
    entry
}

fn subject_of(agent: &str, implementation: char) -> SuiteSubject {
    SuiteSubject {
        agent: agent.to_owned(),
        cli_version: "codex-cli 0.166.0".to_owned(),
        identity: crate::staffing::AgentIdentity {
            launcher_path: "/opt/bin/codex".to_owned(),
            launcher_sha256: hex('a'),
            implementation_path: "/opt/lib/codex".to_owned(),
            implementation_sha256: hex(implementation),
        },
    }
}

/// A store whose manifest replacements fail at `step` once, its project, and the manifest's files.
async fn failing_at(
    dir: &tempfile::TempDir,
    step: crate::compat_manifest::Step,
) -> Result<(Store, String, crate::compat_manifest::Files), Box<dyn std::error::Error>> {
    let store = Store::open(
        &dir.path().join(".hoh/state.db"),
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            faults: Arc::new(FailingManifestStep {
                step,
                failed: AtomicBool::new(false),
            }),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let project = store.begin_host("p1", "/repo").await?.project_id;
    Ok((
        store,
        project,
        crate::compat_manifest::Files::of(dir.path(), "t"),
    ))
}

/// Admit and publish `agent`'s suite over `held` (the manifest the host holds) as the host does: it holds the manifest
/// a publication gives only when that publication is `Replaced`.
async fn publish_over(
    store: &Store,
    project: &str,
    files: &crate::compat_manifest::Files,
    (agent, implementation): (&str, char),
    held: &mut Option<Value>,
) -> Result<(String, Published), Box<dyn std::error::Error>> {
    let Admission::Started(suite) = store
        .begin_suite(
            project,
            subject_of(agent, implementation),
            SystemTime::now(),
        )
        .await?
    else {
        return Err(format!("{agent}'s suite not started").into());
    };
    let published = store
        .publish_suite(Publication {
            project_id: project.to_owned(),
            suite_id: suite.clone(),
            agent: agent.to_owned(),
            entry: entry_of(implementation),
            base: held.clone(),
            files: files.clone(),
            now: SystemTime::now(),
        })
        .await?;
    if let Published::Replaced(manifest) = &published {
        *held = Some(manifest.clone());
    }
    Ok((suite, published))
}

/// The manifest on disk, if any.
fn on_disk(
    files: &crate::compat_manifest::Files,
) -> Result<Option<Value>, Box<dyn std::error::Error>> {
    match std::fs::read(&files.path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The payloads of the project's events of `kind`, in order.
fn payloads(dir: &tempfile::TempDir, kind: &str) -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let conn = Connection::open(dir.path().join(".hoh/state.db"))?;
    let rows: Vec<String> = conn
        .prepare("SELECT payload_json FROM events WHERE kind = ?1 ORDER BY seq")?
        .query_map([kind], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    rows.iter().map(|p| Ok(serde_json::from_str(p)?)).collect()
}

#[tokio::test]
async fn a_manifest_renamed_whose_directory_was_not_synced_is_published_and_held() -> R {
    // M2T15R-04, decision 10 "发布": the rename put the new manifest in place and only its directory's sync failed. Every
    // reader of the file sees the new manifest, so the publication goes on: the suite ends with its own status (the
    // sync's failure said in the end's `error`) and is completed, and the host holds what was renamed. The next agent's
    // publication, built on the manifest the host holds, keeps the first agent's entry; memory equals disk each step.
    let dir = tempfile::tempdir()?;
    let (store, project, files) = failing_at(&dir, crate::compat_manifest::Step::Renamed).await?;
    let mut held = None;
    let (codex, published) =
        publish_over(&store, &project, &files, ("codex", '1'), &mut held).await?;
    assert!(matches!(published, Published::Replaced(_)), "{published:?}");
    assert_eq!(on_disk(&files)?, held, "memory equals disk");
    let ended = payloads(&dir, SUITE_ENDED)?;
    assert_eq!(ended.len(), 1, "{ended:?}");
    assert_eq!(ended[0]["suite_id"], json!(codex));
    assert_eq!(ended[0]["status"], "PASS");
    assert!(
        ended[0]["error"]
            .as_str()
            .is_some_and(|e| e.contains("not synced") && e.contains("injected")),
        "{ended:?}"
    );
    assert_eq!(payloads(&dir, SUITE_COMPLETED)?.len(), 1);
    // The suite is closed: claude's is admitted, and its publication keeps codex's entry.
    let (_, published) = publish_over(&store, &project, &files, ("claude", '2'), &mut held).await?;
    assert!(matches!(published, Published::Replaced(_)), "{published:?}");
    let disk = on_disk(&files)?;
    assert_eq!(disk, held, "memory equals disk");
    let disk = disk.ok_or("no manifest")?;
    assert_eq!(disk["agents"]["codex"], entry_of('1'), "{disk}");
    assert_eq!(disk["agents"]["claude"], entry_of('2'), "{disk}");
    assert!(!files.temp.exists());
    Ok(())
}

#[tokio::test]
async fn a_manifest_not_renamed_leaves_memory_and_disk_old() -> R {
    // The control: a failure before the rename (the temporary file written or synced) leaves the old manifest in place
    // byte for byte and no temporary file; the suite ends unpublished with why, the host keeps the manifest it held, and
    // the next publication builds on it.
    for step in [
        crate::compat_manifest::Step::Written,
        crate::compat_manifest::Step::Synced,
    ] {
        let dir = tempfile::tempdir()?;
        let (store, project, files) = failing_at(&dir, step).await?;
        let old =
            crate::compat_manifest::replaced(None, "pi", &entry_of('0'), "2026-11-14T00:00:00Z")?;
        let old_bytes = serde_json::to_vec_pretty(&old)?;
        std::fs::write(&files.path, &old_bytes)?;
        let mut held = Some(old.clone());
        let (_, published) =
            publish_over(&store, &project, &files, ("codex", '1'), &mut held).await?;
        assert!(
            matches!(&published, Published::Unpublished(why) if why.contains("was not replaced")),
            "{step:?}: {published:?}"
        );
        assert_eq!(std::fs::read(&files.path)?, old_bytes, "{step:?}");
        assert_eq!(held, Some(old.clone()), "{step:?}");
        assert!(!files.temp.exists(), "{step:?}");
        let ended = payloads(&dir, SUITE_ENDED)?;
        assert_eq!(ended.len(), 1, "{step:?}: {ended:?}");
        assert_eq!(ended[0]["status"], UNPUBLISHED, "{step:?}");
        assert!(payloads(&dir, SUITE_COMPLETED)?.is_empty(), "{step:?}");
        let (_, published) =
            publish_over(&store, &project, &files, ("claude", '2'), &mut held).await?;
        assert!(
            matches!(published, Published::Replaced(_)),
            "{step:?}: {published:?}"
        );
        let disk = on_disk(&files)?;
        assert_eq!(disk, held, "{step:?}: memory equals disk");
        let disk = disk.ok_or("no manifest")?;
        assert_eq!(disk["agents"]["pi"], entry_of('0'), "{step:?}");
        assert_eq!(disk["agents"]["claude"], entry_of('2'), "{step:?}");
        assert!(disk["agents"].get("codex").is_none(), "{step:?}");
    }
    Ok(())
}
