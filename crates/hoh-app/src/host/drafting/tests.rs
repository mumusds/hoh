use super::*;
use std::process::Command;
type R<T = ()> = Result<T, Box<dyn std::error::Error>>;
fn git(root: &Path, args: &[&str]) -> R {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(())
}
fn repo() -> R<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix("m3t04-").tempdir()?;
    git(dir.path(), &["init", "-q"])?;
    Ok(dir)
}
fn docs(root: &Path) -> R {
    docs_with_commit(root, &["commit", "-q", "-m", "draft"])
}
fn docs_with_commit(root: &Path, commit: &[&str]) -> R {
    std::fs::create_dir_all(root.join("docs/spec"))?;
    let source = hoh_core::specindex::Source {
        path: "docs/spec/main.md".into(),
        bytes: b"# users\n".to_vec(),
    };
    let source = hoh_core::specindex::Source {
        bytes: "# 用户\n# 范围\n# 非目标\n# 决策\n# 开放问题\n# 需求\n"
            .as_bytes()
            .to_vec(),
        ..source
    };
    std::fs::write(root.join(&source.path), &source.bytes)?;
    let index = hoh_core::specindex::generate_index(&[source])?;
    std::fs::write(root.join(hoh_core::specindex::PATH), index.bytes())?;
    git(root, &["add", "docs/spec"])?;
    git(root, commit)
}
#[test]
fn test_008_02_draft_completion_requires_committed_canonical_index() -> R {
    let dir = repo()?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    std::fs::write(root.join("docs/spec/index.json"), "{}")?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Correct { .. }
    ));
    git(root, &["add", "docs"])?;
    git(root, &["commit", "-q", "-m", "bad index"])?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Correct { .. }
    ));
    Ok(())
}
#[test]
fn test_008_04_committed_deleted_ignored_and_non_utf8_product_paths_are_preserved() -> R {
    for kind in ["committed", "deleted", "ignored", "non-utf8"] {
        let dir = repo()?;
        let root = dir.path();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n")?;
        std::fs::write(root.join("product.txt"), "original")?;
        git(root, &["add", "."])?;
        git(root, &["commit", "-q", "-m", "base"])?;
        let start = snapshot(root);
        docs(root)?;
        let expected = match kind {
            "committed" => {
                std::fs::write(root.join("product.txt"), "changed")?;
                git(root, &["add", "product.txt"])?;
                git(root, &["commit", "-q", "-m", "bad"])?;
                "product.txt".to_owned()
            }
            "deleted" => {
                std::fs::remove_file(root.join("product.txt"))?;
                git(root, &["add", "-u"])?;
                git(root, &["commit", "-q", "-m", "deleted"])?;
                "product.txt".to_owned()
            }
            "ignored" => {
                std::fs::write(root.join("ignored.txt"), "ignored")?;
                "ignored.txt".to_owned()
            }
            _ => {
                let path = b"bad-\xff";
                std::fs::write(
                    root.join(std::ffi::OsString::from_vec(path.to_vec())),
                    "new",
                )?;
                hex(path)
            }
        };
        let WriterOutcome::DraftBlocked { paths, .. } = checked(root, &start)? else {
            return Err(format!("{kind} allowed").into());
        };
        assert!(paths.contains(&expected), "{kind}: {paths:?}");
        if kind == "committed" {
            assert_eq!(
                std::fs::read_to_string(root.join("product.txt"))?,
                "changed"
            );
        }
    }
    Ok(())
}
#[test]
fn test_008_04_uncertain_start_and_allowed_directory_links_fail_closed() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::write(root.join("existing.rs"), "user work")?;
    git(root, &["add", "existing.rs"])?;
    git(root, &["commit", "-q", "-m", "base"])?;
    std::fs::write(root.join("existing.rs"), "dirty user work")?;
    let start = snapshot(root);
    docs(root)?;
    assert!(
        checked(root, &start)
            .unwrap_err()
            .contains("ownership uncertain")
    );
    assert!(root.join("existing.rs").exists());
    // A fresh observation after committing the user's initial file establishes a bounded Git starting point.
    git(root, &["add", "existing.rs"])?;
    git(root, &["commit", "-q", "-m", "base"])?;
    let start = snapshot(root);
    std::os::unix::fs::symlink("../existing.rs", root.join("docs/link"))?;
    assert!(
        checked(root, &start)
            .unwrap_err()
            .contains("link in draft path")
    );
    Ok(())
}

#[test]
fn test_008_02_m3l100_ignored_artifacts_do_not_block_drafting() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), "target/\n.env\n")?;
    git(root, &["add", ".gitignore"])?;
    git(root, &["commit", "-q", "-m", "ignore artifacts"])?;
    std::fs::create_dir(root.join("target"))?;
    std::fs::write(root.join("target/a.o"), "artifact")?;
    std::fs::write(root.join(".env"), "existing environment")?;
    let start = snapshot(root);
    assert_eq!(start["draft_dirty"], json!([]));
    docs(root)?;
    std::fs::write(root.join("target/b.o"), "another artifact")?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    Ok(())
}

#[test]
fn test_008_02_m3l100_greenfield_preserves_existing_untracked_files() -> R {
    for change in ["unchanged", "modified", "deleted", "committed"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t04-greenfield-")
            .tempdir()?;
        let root = dir.path();
        std::fs::write(root.join("README.txt"), "user file")?;
        git(root, &["init", "-q"])?;
        let start = snapshot(root);
        docs(root)?;
        match change {
            "modified" => std::fs::write(root.join("README.txt"), "overwritten")?,
            "deleted" => std::fs::remove_file(root.join("README.txt"))?,
            "committed" => {
                git(root, &["add", "README.txt"])?;
                git(root, &["commit", "-q", "-m", "outside boundary"])?;
            }
            _ => {}
        }
        let result = checked(root, &start)?;
        if change == "unchanged" {
            assert!(matches!(result, WriterOutcome::Draft { .. }));
            assert_eq!(
                std::fs::read_to_string(root.join("README.txt"))?,
                "user file"
            );
        } else {
            assert!(
                matches!(result, WriterOutcome::DraftBlocked { paths, .. } if paths == ["README.txt"]),
                "{change}"
            );
        }
    }
    Ok(())
}

#[test]
fn test_008_04_m3l300_add_all_counts_unchanged_product_files_as_committed_writes() -> R {
    for existing_commit in [false, true] {
        let dir = repo()?;
        let root = dir.path();
        if existing_commit {
            git(root, &["commit", "--allow-empty", "-q", "-m", "base"])?;
        }
        std::fs::create_dir(root.join("src"))?;
        for path in [".env", "README.txt", "src/brownfield.rs"] {
            std::fs::write(root.join(path), "existing user content")?;
        }
        let start = snapshot(root);
        git(root, &["add", "-A"])?;
        docs(root)?;
        let result = checked(root, &start)?;
        assert!(
            matches!(&result, WriterOutcome::DraftBlocked { paths, .. }
                if paths == &[".env", "README.txt", "src/brownfield.rs"]),
            "existing_commit={existing_commit}: {result:?}"
        );
        for path in [".env", "README.txt", "src/brownfield.rs"] {
            assert_eq!(
                std::fs::read_to_string(root.join(path))?,
                "existing user content"
            );
        }
        git(root, &["cat-file", "-e", "HEAD:.env"])?;
    }
    Ok(())
}

#[test]
fn test_008_02_s8_git_init_observation_requires_the_exact_local_repository() -> R {
    use hoh_core::reconcile::Observation;
    let root = tempfile::Builder::new()
        .prefix("m3t04-init-observer-")
        .tempdir()?;
    let key = hoh_core::ids::child_id("A", "git-init");
    assert!(observe_init(root.path(), "A", "other").is_none());
    assert_eq!(
        observe_init(root.path(), "A", &key),
        Some(Observation::Absent)
    );
    std::fs::create_dir(root.path().join(".git"))?;
    assert!(matches!(
        observe_init(root.path(), "A", &key),
        Some(Observation::Unavailable { .. })
    ));
    git(root.path(), &["init", "-q"])?;
    assert_eq!(
        observe_init(root.path(), "A", &key),
        Some(Observation::Found {
            external_ref: ".git".into()
        })
    );
    let linked = tempfile::Builder::new()
        .prefix("m3t04-linked-init-")
        .tempdir()?;
    std::os::unix::fs::symlink(root.path().join(".git"), linked.path().join(".git"))?;
    assert!(matches!(
        observe_init(linked.path(), "A", &key),
        Some(Observation::Unavailable { .. })
    ));
    Ok(())
}

#[test]
fn test_008_02_m3l250_unchanged_untracked_docs_need_no_commit() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::create_dir(root.join("docs"))?;
    std::fs::write(root.join("docs/user-notes.txt"), "user notes")?;
    let start = snapshot(root);
    // Commit only the draft, leaving the existing document untracked.
    let notes = root.join("docs/user-notes.txt");
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    std::fs::write(&notes, "draft changed these notes")?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Correct { .. }
    ));
    Ok(())
}

fn predirty_doc(staged: bool) -> R<tempfile::TempDir> {
    let dir = repo()?;
    std::fs::create_dir_all(dir.path().join("docs"))?;
    std::fs::write(dir.path().join("docs/notes.md"), "committed user notes")?;
    git(dir.path(), &["add", "docs/notes.md"])?;
    git(dir.path(), &["commit", "-qm", "user baseline"])?;
    std::fs::write(dir.path().join("docs/notes.md"), "uncommitted user notes")?;
    if staged {
        git(dir.path(), &["add", "docs/notes.md"])?;
    }
    Ok(dir)
}

fn unchanged_predirty_doc(staged: bool) -> R {
    let dir = predirty_doc(staged)?;
    let root = dir.path();
    let start = snapshot(root);
    // Commit only the draft, preserving the user's staged delta as well as its working bytes.
    docs_with_commit(
        root,
        &["commit", "-q", "-m", "draft", "--only", "--", "docs/spec"],
    )?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    assert_eq!(
        std::fs::read_to_string(root.join("docs/notes.md"))?,
        "uncommitted user notes"
    );
    assert_eq!(
        observe_git(root, &["show", "HEAD:docs/notes.md"], OBSERVATION_LIMIT)?,
        b"committed user notes"
    );
    let changed = git_paths(root, &["diff", "--cached", "--name-only", "-z", "HEAD"])?;
    assert_eq!(changed.contains(b"docs/notes.md".as_slice()), staged);
    assert!(dirty(root, true)?.contains(b"docs/notes.md".as_slice()));
    Ok(())
}

#[test]
fn test_008_02_m3l502_unchanged_unstaged_tracked_docs_need_no_commit() -> R {
    unchanged_predirty_doc(false)
}

#[test]
fn test_008_02_m3l502_unchanged_staged_tracked_docs_need_no_commit() -> R {
    unchanged_predirty_doc(true)
}

#[test]
fn test_008_02_m3l502_preexisting_tracked_deletion_needs_no_commit() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    std::fs::remove_file(root.join("docs/notes.md"))?;
    let start = snapshot(root);
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    assert!(!root.join("docs/notes.md").exists());
    assert_eq!(
        observe_git(root, &["show", "HEAD:docs/notes.md"], OBSERVATION_LIMIT)?,
        b"committed user notes"
    );
    Ok(())
}

fn assert_predirty_ownership_uncertain(root: &Path, start: &Value) -> R {
    let error = checked(root, start).unwrap_err();
    assert!(
        error.contains("ownership uncertain") && error.contains("docs/notes.md"),
        "{error}"
    );
    // The completion adapter must classify it as observation_failed (empty paths), not a
    // correctable must-commit refusal. It runs the write boundary before validating a result.
    let identity = hoh_core::bridge::Identity {
        run_id: "R".into(),
        task_id: "D".into(),
        attempt_id: "A".into(),
        role: "cto".into(),
        role_epoch: 1,
    };
    let mut ending = Ending {
        success: true,
        detail: "scripted draft".into(),
        class: hoh_core::retry::FailureClass::Decision,
        at: std::time::SystemTime::now(),
        draw: 0,
        observed: Default::default(),
        unknown_effect: false,
    };
    assert!(
        matches!(outcome(root, &identity, &mut ending, start), WriterOutcome::DraftBlocked { paths, .. } if paths.is_empty())
    );
    Ok(())
}

#[test]
fn test_008_04_m3l601_replaced_predirty_index_blob_has_uncertain_owner() -> R {
    let dir = predirty_doc(true)?;
    let root = dir.path();
    std::fs::write(root.join("docs/notes.md"), "user worktree B")?;
    let start = snapshot(root);
    git(root, &["add", "docs"])?;
    docs_with_commit(
        root,
        &["commit", "-qm", "draft", "--only", "--", "docs/spec"],
    )?;
    assert_eq!(
        observe_git(root, &["show", ":docs/notes.md"], OBSERVATION_LIMIT)?,
        b"user worktree B"
    );
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l601_changed_predirty_index_mode_has_uncertain_owner() -> R {
    let dir = predirty_doc(true)?;
    let root = dir.path();
    let start = snapshot(root);
    git(root, &["update-index", "--chmod=+x", "docs/notes.md"])?;
    docs_with_commit(
        root,
        &["commit", "-qm", "draft", "--only", "--", "docs/spec"],
    )?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l601_removed_predirty_index_entry_has_uncertain_owner() -> R {
    let dir = predirty_doc(true)?;
    let root = dir.path();
    let start = snapshot(root);
    git(root, &["update-index", "--force-remove", "docs/notes.md"])?;
    docs_with_commit(
        root,
        &["commit", "-qm", "draft", "--only", "--", "docs/spec"],
    )?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l601_missing_predirty_index_snapshot_has_uncertain_owner() -> R {
    let dir = predirty_doc(true)?;
    let root = dir.path();
    let mut start = snapshot(root);
    start
        .as_object_mut()
        .ok_or("snapshot")?
        .remove("draft_index");
    docs_with_commit(
        root,
        &["commit", "-qm", "draft", "--only", "--", "docs/spec"],
    )?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l502_changed_predirty_tracked_doc_has_uncertain_owner() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    std::fs::write(root.join("docs/notes.md"), "changed during draft")?;
    assert_predirty_ownership_uncertain(root, &start)?;
    assert_eq!(
        std::fs::read_to_string(root.join("docs/notes.md"))?,
        "changed during draft"
    );
    Ok(())
}

#[test]
fn test_008_04_m3l502_committed_change_of_predirty_doc_has_uncertain_owner() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    std::fs::write(
        root.join("docs/notes.md"),
        "changed and committed during draft",
    )?;
    git(root, &["add", "docs/notes.md"])?;
    git(root, &["commit", "-qm", "changed user notes"])?;
    let end = head(root)?;
    assert_predirty_ownership_uncertain(root, &start)?;
    assert_eq!(head(root)?, end, "the user's commit is retained");
    Ok(())
}

#[test]
fn test_008_04_m3l502_committing_unchanged_user_delta_has_uncertain_owner() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    git(root, &["add", "docs/notes.md"])?;
    git(root, &["commit", "-qm", "user delta included in draft"])?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l502_deleted_predirty_tracked_doc_has_uncertain_owner() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    std::fs::remove_file(root.join("docs/notes.md"))?;
    assert_predirty_ownership_uncertain(root, &start)?;
    assert!(!root.join("docs/notes.md").exists());
    Ok(())
}

#[test]
fn test_008_04_m3l502_mode_changed_predirty_tracked_doc_has_uncertain_owner() -> R {
    use std::os::unix::fs::PermissionsExt;
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let start = snapshot(root);
    docs(root)?;
    let path = root.join("docs/notes.md");
    let mut permissions = std::fs::metadata(&path)?.permissions();
    permissions.set_mode(permissions.mode() ^ 0o100);
    std::fs::set_permissions(&path, permissions)?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_04_m3l502_old_predirty_start_without_digests_is_uncertain() -> R {
    let dir = predirty_doc(false)?;
    let root = dir.path();
    let mut start = snapshot(root);
    start
        .as_object_mut()
        .ok_or("snapshot object")?
        .remove("draft_tracked");
    docs(root)?;
    assert_predirty_ownership_uncertain(root, &start)
}

#[test]
fn test_008_02_m3l203_untracked_observation_is_bounded_and_folded() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::create_dir(root.join("vendor"))?;
    for n in 0..128 {
        std::fs::write(root.join(format!("vendor/file-{n}")), "existing")?;
    }
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    assert!(
        serde_json::to_vec(&start)?.len() < 4096,
        "large trees must be folded"
    );
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    assert!(
        super::super::control::git_bounded(root, &["ls-files", "--others", "-z"], 8)
            .unwrap_err()
            .contains("limit")
    );
    std::fs::write(root.join(".git/info/exclude"), "vendor/hidden\n")?;
    std::fs::write(root.join("vendor/hidden"), "new ignored product file")?;
    assert!(
        matches!(checked(root, &start)?, WriterOutcome::DraftBlocked { paths, .. } if paths.contains(&"vendor/hidden".to_owned()))
    );
    std::fs::write(root.join(".git/info/exclude"), "")?;
    std::fs::write(root.join("vendor/file-7"), "changed")?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::DraftBlocked { .. }
    ));
    for n in 128..4097 {
        std::fs::write(root.join(format!("vendor/file-{n}")), "existing")?;
    }
    let failed = snapshot(root);
    assert!(
        failed["draft_error"]
            .as_str()
            .is_some_and(|s| s.contains("limit")),
        "{failed}"
    );
    assert!(checked(root, &failed).is_err());
    assert!(
        checked(root, &start).is_err(),
        "end observation must enforce the same cap"
    );
    Ok(())
}

#[test]
#[ignore = "subprocess WAL probe for the boundary read test"]
fn draft_boundary_probe() {
    assert_eq!(hoh_core::persistence::selftest::probe_main(), 0);
}

#[tokio::test]
async fn test_008_04_m3l202_boundary_read_error_is_an_observation_failure() -> R {
    use hoh_core::persistence::{Options, selftest::ProbeCommand};
    let dir = repo()?;
    let store = Store::open(
        &dir.path().join(".hoh/state.db"),
        Options {
            wal_probe: Some(ProbeCommand {
                program: std::env::current_exe()?,
                args: [
                    "--exact",
                    "host::drafting::tests::draft_boundary_probe",
                    "--ignored",
                    "--nocapture",
                ]
                .map(Into::into)
                .to_vec(),
                env: vec![],
                answer_timeout: std::time::Duration::from_secs(10),
            }),
            ..Options::new(hoh_core::spawn::Direct::shared())
        },
    )?;
    // Missing identity makes the durable boundary read fail, independently of Git.
    let result = boundary(&store, dir.path(), "missing-attempt").await;
    assert!(
        matches!(result, Some((_, ref paths)) if paths.is_empty()),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn test_008_02_m3l203_content_limit_is_a_small_failed_observation() -> R {
    let dir = repo()?;
    let start = snapshot(dir.path());
    std::fs::File::create(dir.path().join("large-user-file"))?.set_len(CONTENT_LIMIT + 1)?;
    let failed = snapshot(dir.path());
    assert!(
        failed["draft_error"]
            .as_str()
            .is_some_and(|s| s.contains("limit"))
    );
    assert!(serde_json::to_vec(&failed)?.len() < 1024);
    assert!(checked(dir.path(), &failed).is_err());
    // New product paths are refused without reading their contents.
    assert!(matches!(
        checked(dir.path(), &start)?,
        WriterOutcome::DraftBlocked { .. }
    ));
    Ok(())
}

#[test]
fn test_008_04_m3l300_reverted_committed_product_write_remains_in_attempt_window() -> R {
    for unborn in [false, true] {
        let dir = repo()?;
        let root = dir.path();
        if !unborn {
            std::fs::write(root.join("product.txt"), "original")?;
            git(root, &["add", "product.txt"])?;
            git(root, &["commit", "-q", "-m", "base"])?;
        }
        let start = snapshot(root);
        std::fs::write(root.join("product.txt"), "changed")?;
        git(root, &["add", "product.txt"])?;
        git(root, &["commit", "-q", "-m", "product write"])?;
        if unborn {
            std::fs::remove_file(root.join("product.txt"))?;
        } else {
            std::fs::write(root.join("product.txt"), "original")?;
        }
        git(root, &["add", "-u"])?;
        git(root, &["commit", "-q", "-m", "restore product"])?;
        docs(root)?;
        let before = head(root)?;
        assert!(
            matches!(checked(root, &start)?, WriterOutcome::DraftBlocked { paths, .. } if paths == ["product.txt"]),
            "committed product write disappeared, unborn={unborn}"
        );
        assert_eq!(head(root)?, before);
        assert_eq!(root.join("product.txt").exists(), !unborn);
        if !unborn {
            assert_eq!(
                std::fs::read_to_string(root.join("product.txt"))?,
                "original"
            );
        }
    }
    Ok(())
}

#[test]
fn test_008_02_m3l301_large_tracked_repository_does_not_consume_untracked_budget() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::create_dir(root.join("src"))?;
    for n in 0..30_000 {
        std::fs::write(
            root.join(format!("src/existing-brownfield-source-file-{n:05}.c")),
            "",
        )?;
    }
    git(root, &["add", "src"])?;
    git(root, &["commit", "-q", "-m", "large existing project"])?;
    assert!(super::super::control::git(root, &["ls-files", "-z"])?.len() > OBSERVATION_LIMIT);
    // A literal pathspec must preserve wildcard and non-UTF-8 directory names, too.
    let prefix = b"vendor[raw]-\xff";
    let vendor = root.join(std::ffi::OsString::from_vec(prefix.to_vec()));
    std::fs::create_dir(&vendor)?;
    for n in 0..FOLD_AT {
        std::fs::write(vendor.join(format!("file-{n}")), "user data")?;
    }
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    assert_eq!(
        start["draft_untracked"].as_object().ok_or("digests")?.len(),
        1
    );
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    std::fs::write(vendor.join("file-0"), "changed")?;
    let expected = named(&[prefix.as_slice(), b"/"].concat());
    assert!(
        matches!(checked(root, &start)?, WriterOutcome::DraftBlocked { paths, .. } if paths == [expected])
    );
    for n in 0..FOLD_AT {
        std::fs::write(root.join(format!("src/untracked-{n}.c")), "user source")?;
    }
    let mixed = snapshot(root);
    assert!(mixed.get("draft_error").is_none(), "{mixed}");
    assert!(matches!(
        checked(root, &mixed)?,
        WriterOutcome::Draft { .. }
    ));
    Ok(())
}

#[test]
fn test_008_02_m3l301_in_tree_ignored_build_products_do_not_consume_untracked_budget() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), "*.o\n")?;
    std::fs::create_dir(root.join("src"))?;
    std::fs::write(root.join("src/brownfield.c"), "source")?;
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "-m", "in-tree build project"])?;
    for n in 0..4100 {
        std::fs::write(root.join(format!("src/build-{n}.o")), "artifact")?;
    }
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    assert_eq!(
        start["draft_ignored_roots"]
            .as_array()
            .ok_or("ignored roots")?
            .len(),
        4100
    );
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    std::fs::write(root.join("src/new.o"), "new artifact")?;
    assert!(
        matches!(checked(root, &start)?, WriterOutcome::DraftBlocked { paths, .. } if paths == ["src/new.o"])
    );
    Ok(())
}

#[test]
fn test_008_02_m3l400_changed_initial_document_committed_or_deleted_is_complete() -> R {
    for unborn in [false, true] {
        for deleted in [false, true] {
            let dir = repo()?;
            let root = dir.path();
            if !unborn {
                docs(root)?;
            }
            std::fs::create_dir_all(root.join("docs"))?;
            std::fs::write(root.join("docs/requirements.md"), "user draft")?;
            let start = snapshot(root);
            if deleted {
                std::fs::remove_file(root.join("docs/requirements.md"))?;
            } else {
                std::fs::write(root.join("docs/requirements.md"), "finished draft")?;
                git(root, &["add", "docs/requirements.md"])?;
            }
            if unborn {
                docs(root)?;
            } else if !deleted {
                git(root, &["commit", "-q", "-m", "finish document"])?;
            }
            let result = checked(root, &start)?;
            assert!(
                matches!(result, WriterOutcome::Draft { .. }),
                "unborn={unborn}, deleted={deleted}: {result:?}"
            );
            assert_eq!(root.join("docs/requirements.md").exists(), !deleted);
        }
    }
    Ok(())
}

#[test]
fn test_008_04_m3l401_signed_commits_have_only_raw_paths_under_user_config() -> R {
    let dir = repo()?;
    let root = dir.path();
    let keys = tempfile::Builder::new()
        .prefix("m3t04-signing-")
        .tempdir()?;
    let key = keys.path().join("key");
    let made = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .output()?;
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let signers = keys.path().join("allowed");
    std::fs::write(
        &signers,
        format!(
            "t@example.invalid {}",
            std::fs::read_to_string(key.with_extension("pub"))?
        ),
    )?;
    for (name, value) in [
        ("gpg.format", "ssh"),
        ("commit.gpgsign", "true"),
        ("log.showSignature", "true"),
        ("color.ui", "always"),
        ("log.diffMerges", "separate"),
        ("diff.renames", "copies"),
        ("core.pager", "false"),
    ] {
        git(root, &["config", name, value])?;
    }
    git(
        root,
        &["config", "user.signingkey", key.to_str().ok_or("key path")?],
    )?;
    git(
        root,
        &[
            "config",
            "gpg.ssh.allowedSignersFile",
            signers.to_str().ok_or("signers path")?,
        ],
    )?;
    let start = snapshot(root);
    docs(root)?;
    let result = checked(root, &start)?;
    assert!(matches!(result, WriterOutcome::Draft { .. }), "{result:?}");
    Ok(())
}

#[test]
fn test_008_02_m3l402_large_ignored_inventory_has_bounded_snapshot() -> R {
    let dir = repo()?;
    let root = dir.path();
    std::fs::write(root.join(".gitignore"), "*.o\n*.cmd\n")?;
    for group in 0..20 {
        let sub = root.join(format!("drivers/subsystem-{group:02}/component"));
        std::fs::create_dir_all(&sub)?;
        std::fs::write(sub.join("main.c"), "source")?;
        for n in 0..800 {
            std::fs::write(sub.join(format!("build-artifact-{n:04}.o")), "")?;
        }
    }
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "-m", "in-tree build"])?;
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    assert!(
        serde_json::to_vec(&start)?.len() < 16 * 1024,
        "ignored artifacts expanded the persisted snapshot"
    );
    docs(root)?;
    assert!(matches!(
        checked(root, &start)?,
        WriterOutcome::Draft { .. }
    ));
    std::fs::write(root.join("drivers/subsystem-00/component/new.o"), "new")?;
    assert!(
        checked(root, &start).is_err(),
        "a changed large inventory must not be silently excluded"
    );
    Ok(())
}

#[test]
fn test_008_02_m3l403_old_git_is_rejected_before_observation() -> R {
    use std::os::unix::fs::PermissionsExt;
    if let Some(root) = std::env::var_os("HOH_M3L403_PROBE") {
        let start = snapshot(Path::new(&root));
        assert!(
            start["draft_error"]
                .as_str()
                .is_some_and(|s| s.contains("Git 2.38")),
            "{start}"
        );
        return Ok(());
    }
    let dir = repo()?;
    let bin = tempfile::Builder::new()
        .prefix("m3t04-git-version-")
        .tempdir()?;
    let real = Command::new("sh").args(["-c", "command -v git"]).output()?;
    assert!(real.status.success());
    let proxy = bin.path().join("git");
    std::fs::write(
        &proxy,
        "#!/bin/sh\nfor arg in \"$@\"; do\n if [ \"$arg\" = --version ]; then echo 'git version 2.37.9'; exit 0; fi\ndone\nexec \"$HOH_M3L403_REAL_GIT\" \"$@\"\n",
    )?;
    std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o700))?;
    let out = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "host::drafting::tests::test_008_02_m3l403_old_git_is_rejected_before_observation",
            "--nocapture",
        ])
        .env("HOH_M3L403_PROBE", dir.path())
        .env(
            "HOH_M3L403_REAL_GIT",
            String::from_utf8(real.stdout)?.trim(),
        )
        .env("PATH", bin.path())
        .output()?;
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

#[test]
fn test_008_04_m3l451_merge_observes_first_parent_and_actual_product_changes() -> R {
    for changes_product in [false, true] {
        let dir = repo()?;
        let root = dir.path();
        docs(root)?;
        std::fs::write(root.join("product.txt"), "base")?;
        git(root, &["add", "product.txt"])?;
        git(root, &["commit", "-q", "-m", "common base"])?;
        git(root, &["branch", "side"])?;
        git(root, &["checkout", "-q", "-b", "main-line"])?;
        std::fs::write(root.join("main-only.txt"), "already present")?;
        git(root, &["add", "main-only.txt"])?;
        git(root, &["commit", "-q", "-m", "before draft"])?;
        let start = snapshot(root);
        git(root, &["checkout", "-q", "side"])?;
        std::fs::write(root.join("docs/notes.md"), "planning notes")?;
        if changes_product {
            std::fs::write(root.join("product.txt"), "changed")?;
        }
        git(root, &["add", "."])?;
        git(root, &["commit", "-q", "-m", "side draft"])?;
        git(root, &["checkout", "-q", "main-line"])?;
        git(
            root,
            &["merge", "-q", "--no-ff", "-m", "merge draft", "side"],
        )?;
        let result = checked(root, &start)?;
        if changes_product {
            assert!(
                matches!(result, WriterOutcome::DraftBlocked { paths, .. } if paths == ["product.txt"])
            );
        } else {
            assert!(matches!(result, WriterOutcome::Draft { .. }), "{result:?}");
        }
        assert_eq!(
            std::fs::read_to_string(root.join("main-only.txt"))?,
            "already present"
        );
    }
    Ok(())
}

fn staged_initial_untracked(path: &str, folded: bool) -> R<(tempfile::TempDir, Value)> {
    let dir = repo()?;
    let root = dir.path();
    git(root, &["commit", "--allow-empty", "-qm", "base"])?;
    let parent = Path::new(path).parent().ok_or("parent")?;
    std::fs::create_dir_all(root.join(parent))?;
    std::fs::write(root.join(path), "user content")?;
    if folded {
        for n in 1..FOLD_AT {
            std::fs::write(root.join(parent).join(format!("file-{n}")), "user content")?;
        }
    }
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    let observed = if folded { "src/" } else { path };
    assert_eq!(start["draft_dirty"], json!([hex(observed.as_bytes())]));
    git(root, &["add", path])?;
    docs_with_commit(
        root,
        &["commit", "-qm", "draft", "--only", "--", "docs/spec"],
    )?;
    let staged = git_paths(root, &["diff", "--cached", "--name-only", "-z", "HEAD"])?;
    assert_eq!(staged, BTreeSet::from([path.as_bytes().to_vec()]));
    assert_eq!(std::fs::read_to_string(root.join(path))?, "user content");
    Ok((dir, start))
}

#[test]
fn test_008_04_m3l700_staged_initial_untracked_product_is_a_write() -> R {
    let (dir, start) = staged_initial_untracked("src/user.rs", false)?;
    let result = checked(dir.path(), &start)?;
    assert!(
        matches!(&result, WriterOutcome::DraftBlocked { paths, .. } if paths == &["src/user.rs"]),
        "staging the user's source is a product write: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/user.rs"))?,
        "user content"
    );
    Ok(())
}

#[test]
fn test_008_04_m3l700_staged_initial_untracked_doc_must_be_committed() -> R {
    let (dir, start) = staged_initial_untracked("docs/notes.md", false)?;
    let result = checked(dir.path(), &start)?;
    assert!(
        matches!(&result, WriterOutcome::Correct { reasons }
            if reasons == &["draft Markdown and index must be committed before completion"]),
        "staging the user's untracked doc joins the draft's must-commit write set: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("docs/notes.md"))?,
        "user content"
    );
    Ok(())
}

#[test]
fn test_008_04_m3l700_staged_child_of_folded_untracked_prefix_is_a_write() -> R {
    let (dir, start) = staged_initial_untracked("src/user.rs", true)?;
    let result = checked(dir.path(), &start)?;
    assert!(
        matches!(&result, WriterOutcome::DraftBlocked { paths, .. } if paths == &["src/"]),
        "staging a child must not disappear in a folded tree: {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("src/user.rs"))?,
        "user content"
    );
    Ok(())
}

#[test]
fn test_008_04_dc_t04c_missing_untracked_start_index_has_uncertain_owner() -> R {
    let dir = repo()?;
    std::fs::create_dir(dir.path().join("docs"))?;
    std::fs::write(dir.path().join("docs/notes.md"), "user content")?;
    let mut start = snapshot(dir.path());
    start
        .as_object_mut()
        .ok_or("snapshot")?
        .remove("draft_index");
    docs(dir.path())?;
    assert_predirty_ownership_uncertain(dir.path(), &start)
}

#[test]
fn test_008_04_dc_t04c_missing_folded_start_index_has_uncertain_owner() -> R {
    let (dir, mut start) = staged_initial_untracked("src/user.rs", true)?;
    git(dir.path(), &["reset", "-q", "HEAD", "--", "src/user.rs"])?;
    start
        .as_object_mut()
        .ok_or("snapshot")?
        .remove("draft_index");
    let error = checked(dir.path(), &start).unwrap_err();
    assert!(
        error.contains("ownership uncertain") && error.contains("src/"),
        "{error}"
    );
    Ok(())
}

#[test]
fn test_008_04_dc_t04c_unreadable_end_index_has_uncertain_owner() -> R {
    let (dir, start) = staged_initial_untracked("docs/notes.md", false)?;
    std::fs::write(dir.path().join(".git/index"), "broken index")?;
    let error = checked(dir.path(), &start).unwrap_err();
    assert!(error.contains("ownership uncertain"), "{error}");
    Ok(())
}

#[test]
fn test_008_02_dc_t04c_snapshot_records_all_start_dirty_index_entries() -> R {
    let dir = predirty_doc(true)?;
    let root = dir.path();
    std::fs::write(root.join("docs/user.md"), "untracked notes")?;
    std::fs::create_dir(root.join("src"))?;
    for n in 0..FOLD_AT {
        std::fs::write(root.join(format!("src/file-{n}")), "user content")?;
    }
    let start = snapshot(root);
    assert!(start.get("draft_error").is_none(), "{start}");
    assert_eq!(start["draft_index"][hex(b"docs/user.md")], json!([]));
    assert_eq!(start["draft_index"][hex(b"src/")], json!([]));
    let entries = start["draft_index"][hex(b"docs/notes.md")]
        .as_array()
        .ok_or("tracked index entries")?;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]
            .as_str()
            .ok_or("entry")?
            .split_whitespace()
            .count(),
        3
    );
    assert_eq!(start["draft_index"].as_object().ok_or("index")?.len(), 3);
    Ok(())
}

// Pin the complete write-set use inventory as well as the sole exclusion's guard. Any new
// mutation, alias or replacement must fail this pin before it can bypass the shared predicate.
fn untouched_exclusion_is_pinned(source: &str) -> bool {
    let lines: Vec<_> = source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//"))
        .filter(|line| {
            line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == "changed")
        })
        .collect();
    let expected = [
        // Existing diagnostics also contain this token; pin them explicitly so no source
        // line is silently omitted from the inventory by a syntax heuristic.
        "Err(\"draft git-init effect changed; reconcile before continuing\".into())",
        "Err(\"ignored path inventory changed or is missing; cannot attribute draft writes\".into())",
        "let mut changed = end_dirty.clone();",
        "changed.remove(&path);",
        "changed.insert(path);",
        "let uncommitted: BTreeSet<_> = end_dirty.intersection(&changed).cloned().collect();",
        "changed.extend(committed);",
        "changed.extend(newly_ignored);",
        "let outside: Vec<_> = changed",
        "for path in &changed {",
    ];
    let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
    lines == expected
        && compact.contains("ifuntouched_at_start(&path,digest,index,&end_index,&committed,&now)?{changed.remove(&path);}")
}

#[test]
fn test_008_04_dc_t04c_write_set_exclusion_has_one_pinned_predicate() {
    let source = include_str!("../drafting.rs");
    assert!(
        untouched_exclusion_is_pinned(source),
        "every write-set exclusion must go through untouched_at_start"
    );
    for mutation in [
        source.replace("changed.remove(&path);", "changed.clear();"),
        source.replace(
            "changed.remove(&path);",
            "changed.remove(&path); changed.retain(|_| false);",
        ),
        source.replace("if untouched_at_start(", "if bypass_untouched_at_start("),
    ] {
        assert!(!untouched_exclusion_is_pinned(&mutation));
    }
}

#[test]
fn template_owned_paths_use_the_same_digest_index_and_commit_predicate() -> R {
    for action in ["unchanged", "stage", "edit", "commit", "missing_index"] {
        let dir = repo()?;
        let root = dir.path();
        std::fs::create_dir(root.join("docs"))?;
        std::fs::write(root.join("docs/feature.md"), "Runtime template")?;
        let mut start = snapshot(root);
        attribute_templates(&mut start, Some(&json!({"created":["docs/feature.md"]})))?;
        assert_eq!(start["draft_dirty"], json!([]));
        assert_eq!(
            start["draft_templates"].as_object().map(|o| o.len()),
            Some(1)
        );
        docs(root)?;
        match action {
            "stage" => git(root, &["add", "docs/feature.md"])?,
            "edit" => std::fs::write(root.join("docs/feature.md"), "draft edit")?,
            "commit" => {
                git(root, &["add", "docs/feature.md"])?;
                git(root, &["commit", "-qm", "include template"])?;
            }
            "missing_index" => {
                start["draft_index"] = json!({});
            }
            _ => (),
        }
        let checked = checked(root, &start);
        match action {
            "unchanged" | "commit" => {
                assert!(matches!(checked?, WriterOutcome::Draft { .. }), "{action}")
            }
            "stage" | "edit" => assert!(
                matches!(checked?, WriterOutcome::Correct { .. }),
                "{action}"
            ),
            _ => assert!(
                checked.is_err(),
                "a missing Runtime template index cannot establish ownership"
            ),
        }
    }
    Ok(())
}
