//! TEST-001-03 (REQ-001): without a terminal on both stdin and stdout, bare `hoh` does not open the TUI. It exits 64
//! with a `-p` hint on stderr and nothing on stdout, does not read stdin, and leaves the project, its `.hoh/` database
//! included, untouched; no host is started. The fixture is an empty Git project with a store (no run), as in
//! AC-001-02; the configuration file of that AC has no parser before M2-T07 and is not part of it.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod pty;

use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{Options, Store};
use pty::{Pty, R, exit_within, processes_in, tree};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// An empty Git project with `.hoh/state.db` made by the store (a project row, no run).
fn fixture(project: &Path) -> R {
    if !Command::new("git")
        .args(["init", "-q"])
        .current_dir(project)
        .status()?
        .success()
    {
        return Err("git init failed".into());
    }
    let db = project.join(".hoh/state.db");
    let store = Store::open(
        &db,
        Options {
            wal_probe: Some(ProbeCommand {
                program: env!("CARGO_BIN_EXE_hoh").into(),
                args: vec![PROBE_ARG.into()],
                env: Vec::new(),
                answer_timeout: Duration::from_secs(10),
            }),
            ..Options::default()
        },
    )?;
    drop(store);
    let seed = "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-05T00:00:00Z');";
    let status = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit(); c.close()",
        ])
        .arg(&db)
        .arg(seed)
        .status()?;
    if !status.success() {
        return Err("seeding the store failed".into());
    }
    Ok(())
}

/// The row count of every table of the store, read with Python's sqlite3 (test code).
fn rows(project: &Path) -> R<String> {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); t=[r[0] for r in c.execute(\"SELECT name FROM sqlite_master WHERE type='table' ORDER BY name\")]; print({n: c.execute(f'SELECT count(*) FROM \"{n}\"').fetchone()[0] for n in t})",
        ])
        .arg(project.join(".hoh/state.db"))
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[test]
fn test_001_03_without_a_terminal_bare_hoh_is_a_usage_error() -> R {
    let home = tempfile::tempdir()?;
    let project = tempfile::tempdir()?;
    let project = project.path().canonicalize()?;
    fixture(&project)?;
    let files = tempfile::tempdir()?;
    let tree_before = tree(&project)?;
    let rows_before = rows(&project)?;

    // 1. No controlling terminal (a new session without one), stdin /dev/null, stdout and stderr files.
    let (out, err) = (files.path().join("out"), files.path().join("err"));
    let mut child = Command::new("setsid")
        .arg(env!("CARGO_BIN_EXE_hoh"))
        .current_dir(&project)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(File::create(&out)?)
        .stderr(File::create(&err)?)
        .spawn()?;
    let status = exit_within(&mut child, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(64), "no terminal at all");
    assert_eq!(std::fs::read(&out)?, b"", "stdout");
    let stderr = std::fs::read_to_string(&err)?;
    assert!(stderr.contains("hoh -p"), "the -p hint: {stderr:?}");

    // 2. In a PTY (its controlling terminal, stdin and stderr) with only stdout redirected to a file.
    let pty = Pty::new(30, 100)?;
    let out = files.path().join("out2");
    let mut child = Command::new("setsid")
        .arg("--ctty")
        .arg(env!("CARGO_BIN_EXE_hoh"))
        .current_dir(&project)
        .env_clear()
        .env("HOME", home.path())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("TERM", "xterm-256color")
        .stdin(pty.stdio()?)
        .stdout(File::create(&out)?)
        .stderr(pty.stdio()?)
        .spawn()?;
    let status = exit_within(&mut child, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(64), "stdout redirected");
    assert_eq!(std::fs::read(&out)?, b"", "stdout");
    pty.wait_for("hoh -p", 0, Duration::from_secs(5))?;
    let shown = String::from_utf8_lossy(&pty.output()).into_owned();
    assert!(
        !shown.contains("\u{1b}[?1049h"),
        "the alternate screen was entered: {shown:?}"
    );

    // Nothing written (the database rows included) and nothing left running in the project: no host.
    assert_eq!(rows(&project)?, rows_before, "store rows");
    assert!(tree(&project)? == tree_before, "the project changed");
    assert_eq!(
        processes_in(&project),
        Vec::<u32>::new(),
        "processes in the project"
    );
    Ok(())
}
