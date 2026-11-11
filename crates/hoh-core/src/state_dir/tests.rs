use super::*;
use std::path::Path;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `git` in `dir` (a test's own repository, no user configuration): its exit status and stdout.
fn git(dir: &Path, args: &[&str]) -> R<(bool, String)> {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()?;
    Ok((
        out.status.success(),
        String::from_utf8(out.stdout)? + &String::from_utf8_lossy(&out.stderr),
    ))
}

fn ok(dir: &Path, args: &[&str]) -> R<String> {
    let (success, out) = git(dir, args)?;
    if !success {
        return Err(format!("git {args:?}: {out}").into());
    }
    Ok(out)
}

/// The files HoH writes under `.hoh/`, one of each kind (docs/adr/ADR-027.md decision 3).
fn write_state(hoh: &Path) -> R {
    for path in [
        "state.db",
        "state.db-wal",
        "objects/sha256/ab/abcd",
        "evidence/CAND-1/unit/r1/stdout.log",
        "attempts/a1/stdout.jsonl",
        "sessions/s1/notes.md",
        "first-inputs/i1.json",
        "host.json",
        "host.log",
        "input-history.jsonl",
    ] {
        let file = hoh.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, b"x")?;
    }
    Ok(())
}

/// A repository with one commit, so a linked worktree can be added.
fn repository() -> R<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    ok(dir.path(), &["init", "-q"])?;
    std::fs::write(dir.path().join("README"), b"r\n")?;
    ok(dir.path(), &["add", "README"])?;
    ok(dir.path(), &["commit", "-q", "-m", "c"])?;
    Ok(dir)
}

#[test]
fn a_new_state_dir_and_all_hoh_writes_there_are_ignored_by_git() -> R {
    let repo = repository()?;
    let exclude = std::fs::read(repo.path().join(".git/info/exclude")).ok();
    let hoh = repo.path().join(".hoh");
    ensure(&hoh)?;
    assert_eq!(std::fs::read_to_string(hoh.join(".gitignore"))?, "*\n");
    write_state(&hoh)?;
    assert_eq!(
        ok(
            repo.path(),
            &["status", "--porcelain", "--untracked-files=all"]
        )?,
        ""
    );
    assert!(git(repo.path(), &["check-ignore", "-q", ".hoh/state.db"])?.0);
    assert!(
        git(
            repo.path(),
            &["check-ignore", "-q", ".hoh/objects/sha256/ab/abcd"]
        )?
        .0
    );
    // Nothing of the user's repository is changed: no tracked file, nothing under `.git`.
    assert_eq!(
        std::fs::read(repo.path().join(".git/info/exclude")).ok(),
        exclude
    );
    assert!(!repo.path().join(".gitignore").exists());
    // Only the one file: no staged copy is left.
    let mut names: Vec<String> = std::fs::read_dir(&hoh)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".gitignore"))
        .collect();
    names.sort();
    assert_eq!(names, vec![".gitignore".to_owned()]);
    Ok(())
}

#[test]
fn a_state_dir_in_a_linked_worktree_is_ignored_there() -> R {
    let repo = repository()?;
    let linked = repo.path().join("linked");
    ok(
        repo.path(),
        &["worktree", "add", "-q", "--detach", "linked"],
    )?;
    ensure(&linked.join(".hoh"))?;
    write_state(&linked.join(".hoh"))?;
    assert_eq!(
        ok(&linked, &["status", "--porcelain", "--untracked-files=all"])?,
        ""
    );
    Ok(())
}

#[test]
fn a_gitignore_already_in_the_state_dir_is_left_as_it_is() -> R {
    let repo = repository()?;
    let hoh = repo.path().join(".hoh");
    std::fs::create_dir_all(&hoh)?;
    // The user's own: it keeps out only the database, and stays so.
    std::fs::write(hoh.join(".gitignore"), "state.db*\n.gitignore\n")?;
    ensure(&hoh)?;
    ensure(&hoh)?;
    assert_eq!(
        std::fs::read_to_string(hoh.join(".gitignore"))?,
        "state.db*\n.gitignore\n"
    );
    write_state(&hoh)?;
    assert!(git(repo.path(), &["check-ignore", "-q", ".hoh/state.db"])?.0);
    assert!(!git(repo.path(), &["check-ignore", "-q", ".hoh/host.json"])?.0);
    Ok(())
}

#[test]
fn ensuring_twice_or_from_many_threads_writes_one_whole_file() -> R {
    let dir = tempfile::tempdir()?;
    let hoh = dir.path().join("deep/.hoh");
    std::thread::scope(|s| {
        let handles: Vec<_> = (0..8).map(|_| s.spawn(|| ensure(&hoh))).collect();
        handles.into_iter().try_for_each(|h| {
            h.join()
                .map_err(|_| "a thread panicked")?
                .map_err(|e| e.to_string())
        })
    })?;
    ensure(&hoh)?;
    assert_eq!(std::fs::read_to_string(hoh.join(".gitignore"))?, IGNORE_ALL);
    let names: Vec<String> = std::fs::read_dir(&hoh)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![".gitignore".to_owned()]);
    Ok(())
}
