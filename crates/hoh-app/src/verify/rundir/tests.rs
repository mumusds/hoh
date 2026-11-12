//! Run directories against a real repository and a real freeze (docs/adr/ADR-028.md decisions 6 and 8).

use super::*;
use std::process::Command;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn run_git(dir: &Path, args: &[&str]) -> R<String> {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// A committed project frozen by the freeze's file side: the project and its candidate.
fn frozen(dir: &Path) -> R<(PathBuf, Sealed)> {
    let project = dir.join("project");
    std::fs::create_dir_all(&project)?;
    run_git(&project, &["init", "-q"])?;
    // The layout's inputs where the freeze takes them, and a source of the project's own.
    let layout = crate::host::completion_inputs();
    for (path, text) in [
        (Path::new("src/work.rs"), "pub fn a() {}\n"),
        (layout.spec.as_path(), "{\"requirements\": []}\n"),
        (layout.test_plan.as_path(), "{\"checks\": {}}\n"),
    ] {
        let file = project.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    run_git(&project, &["add", "."])?;
    run_git(&project, &["commit", "-q", "-m", "work"])?;
    crate::verify::environment::ensure(&project)?;
    let job = crate::verify::freeze::prepare_job(
        &project,
        &project.join(".hoh"),
        "R",
        &crate::host::completion_inputs(),
    )
    .map_err(|e| e.to_string())?;
    Ok((project, job.sealed))
}

#[test]
fn a_run_directory_is_the_candidate_outside_the_project_and_leaves_its_repository_alone() -> R {
    // Decision 6: a shared clone detached at the candidate's commit, its frozen environment in its ignored `.hoh/`,
    // checked whole; the project's refs, index and objects untouched; its pre_tree the candidate's commit and tree.
    let dir = tempfile::tempdir()?;
    let (project, sealed) = frozen(dir.path())?;
    let refs = run_git(&project, &["for-each-ref"])?;
    let index = std::fs::read(project.join(".git/index"))?;
    let run = dir_of(
        &dir.path().join("state/hoh/runs"),
        "p1",
        "C/round/1/task/qa/attempt-1",
    );
    assert!(!run.starts_with(&project));
    let pre_tree = prepare(&project, &run, &sealed).map_err(|e| e.to_string())?;
    assert_eq!(pre_tree["head"], json!(sealed.manifest.git_commit));
    assert_eq!(pre_tree["head_tree"], json!(sealed.manifest.git_tree));
    assert_eq!(
        hoh_core::objects::hash_file(&run.join(&sealed.manifest.environment.path))?.0,
        sealed.manifest.environment.sha256
    );
    assert_eq!(run_git(&run, &["status", "--porcelain"])?, "");
    assert_eq!(run_git(&project, &["for-each-ref"])?, refs);
    assert_eq!(std::fs::read(project.join(".git/index"))?, index);
    // Looked at again unchanged: equal.
    let unchanged = post(&run, &pre_tree, &sealed.manifest);
    assert!(unchanged.equal, "{unchanged:?}");
    // Prepared again (a second turn of the same attempt id cannot reuse what a first one left): fresh.
    std::fs::write(run.join("stray.txt"), "left")?;
    let again = prepare(&project, &run, &sealed).map_err(|e| e.to_string())?;
    assert_eq!(again, pre_tree);
    remove(&run);
    assert!(!run.exists());
    Ok(())
}

#[test]
fn a_changed_run_directory_is_not_equal_and_its_status_says_why() -> R {
    // Decision 8: a tracked file rewritten, a commit made, or a file added: post_tree differs, equal is false and the
    // status shows the change.
    let dir = tempfile::tempdir()?;
    let (project, sealed) = frozen(dir.path())?;
    for change in ["rewrite", "commit", "add"] {
        let run = dir_of(&dir.path().join("runs"), "p1", change);
        let pre_tree = prepare(&project, &run, &sealed).map_err(|e| e.to_string())?;
        match change {
            "rewrite" => std::fs::write(run.join("src/work.rs"), "pub fn b() {}\n")?,
            "commit" => {
                std::fs::write(run.join("src/work.rs"), "pub fn c() {}\n")?;
                run_git(&run, &["commit", "-q", "-am", "verifier's commit"])?;
            }
            _ => std::fs::write(run.join("new.txt"), "x")?,
        }
        let checked = post(&run, &pre_tree, &sealed.manifest);
        assert!(!checked.equal, "{change}: {checked:?}");
        if change != "commit" {
            assert!(!checked.status.is_empty(), "{change}");
        }
    }
    Ok(())
}

#[test]
fn a_candidate_its_project_no_longer_has_is_refused() -> R {
    // Decision 6: the checkout cannot be made the candidate (here its commit is not in the project): nothing starts.
    let dir = tempfile::tempdir()?;
    let (project, mut sealed) = frozen(dir.path())?;
    sealed.manifest.git_commit = "0".repeat(40);
    let refused = prepare(&project, &dir.path().join("runs/x"), &sealed);
    assert!(refused.is_err(), "{refused:?}");
    Ok(())
}

#[test]
fn run_directories_sit_beside_the_commanders_session_directories() {
    let commander = Path::new("/s/hoh/commander");
    assert_eq!(root(Some(commander)), Path::new("/s/hoh/runs"));
    assert_eq!(
        dir_of(Path::new("/s/hoh/runs"), "p", "C/round/1/task/qa/attempt-1"),
        Path::new("/s/hoh/runs/p/C%2Fround%2F1%2Ftask%2Fqa%2Fattempt-1")
    );
}
