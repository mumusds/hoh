//! A review's commit (docs/adr/ADR-035.md decision 5) against real repositories.

use super::*;
use hoh_core::specindex::{self, Source};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn git(dir: &Path, args: &[&str]) -> R<String> {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// `markdown` as docs/spec/a.md with its index (`index` bytes when given, else the generated ones), committed: the
/// commit and the generated index's digest.
fn commit(dir: &Path, markdown: &str, index: Option<&[u8]>) -> R<(String, String)> {
    let source = Source {
        path: "docs/spec/a.md".into(),
        bytes: markdown.as_bytes().to_vec(),
    };
    let generated = specindex::generate_index(std::slice::from_ref(&source))?;
    std::fs::create_dir_all(dir.join("docs/spec"))?;
    std::fs::write(dir.join(&source.path), &source.bytes)?;
    std::fs::write(
        dir.join(specindex::PATH),
        index.unwrap_or(generated.bytes()),
    )?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "-qm", "spec"])?;
    Ok((git(dir, &["rev-parse", "HEAD"])?, generated.sha256()))
}

fn spec(then: &str) -> String {
    format!(
        "# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A 标题\n### AC-A-01\n\
         given: g\nwhen: w\nthen: {then}\ncheck: unit\n"
    )
}

#[test]
fn a_revision_is_reviewed_at_the_newest_commit_holding_its_sources_and_index() -> R {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    assert_eq!(
        review_commit(root, &"0".repeat(64))?,
        Err("none".to_owned())
    );
    git(root, &["init", "-q"])?;
    assert_eq!(
        review_commit(root, &"0".repeat(64))?,
        Err("none".to_owned())
    );
    let (first, one) = commit(root, &spec("one"), None)?;
    let (second, two) = commit(root, &spec("two"), None)?;
    // A commit that changes no specification file is not looked at; its HEAD is the one a wait names.
    std::fs::write(root.join("README"), "x")?;
    git(root, &["add", "README"])?;
    git(root, &["commit", "-qm", "other"])?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    assert_eq!(review_commit(root, &one)?, Ok(first));
    assert_eq!(review_commit(root, &two)?, Ok(second.clone()));
    assert_eq!(review_commit(root, &"0".repeat(64))?, Err(head));
    // Markdown of revision three committed with a stale index.json is not its commit.
    let stale = std::fs::read(root.join(specindex::PATH))?;
    let (_, three) = commit(root, &spec("three"), Some(&stale))?;
    let head = git(root, &["rev-parse", "HEAD"])?;
    assert_eq!(review_commit(root, &three)?, Err(head));
    assert_eq!(review_commit(root, &two)?, Ok(second));
    Ok(())
}
