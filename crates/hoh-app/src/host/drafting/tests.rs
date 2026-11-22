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
    git(root, &["add", "docs"])?;
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
