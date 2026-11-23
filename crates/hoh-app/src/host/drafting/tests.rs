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
    git(root, &["commit", "-q", "-m", "draft"])
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
