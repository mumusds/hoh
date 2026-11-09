//! Installing HoH (REQ-005, docs/adr/ADR-023.md): the binary's `__unit` entry and the installer's file layer, in a
//! temporary HOME with no user manager reachable (an empty XDG_RUNTIME_DIR). The unit's behaviour under systemd is
//! tested under a private user manager (`systemd.rs`), and the whole release bundle by TEST-005-06 (`deployment.rs`).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use std::path::Path;
use std::process::{Command, Output};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `hoh <args>` with only `vars` in its environment, in `dir`.
fn hoh(args: &[&str], vars: &[(&str, &Path)], dir: &Path) -> R<Output> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hoh"));
    command.args(args).current_dir(dir).env_clear();
    command.env("PATH", "/usr/bin:/bin");
    for (key, value) in vars {
        command.env(key, value);
    }
    Ok(command.output()?)
}

#[test]
fn the_unit_entry_refuses_a_wrong_command_line_with_64_and_is_in_no_help() -> R {
    let home = tempfile::tempdir()?;
    let vars = [("HOME", home.path())];
    for args in [
        &["__unit"][..],
        &["__unit", "x"],
        &["__unit", "template", "x"],
        &["__unit", "enable"],
        &["__unit", "enable", "a", "b"],
    ] {
        let out = hoh(args, &vars, home.path())?;
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    let missing = home.path().join("missing");
    let out = hoh(
        &["__unit", "enable", missing.to_str().ok_or("path")?],
        &vars,
        home.path(),
    )?;
    assert_eq!(out.status.code(), Some(64));
    for args in [&["--help"][..], &["help", "doctor"], &["attach", "--help"]] {
        let out = hoh(args, &vars, home.path())?;
        assert!(
            !String::from_utf8(out.stdout)?.contains("__unit"),
            "{args:?}"
        );
    }
    Ok(())
}

#[test]
fn the_unit_entry_prints_the_template_for_its_home_or_refuses_one_systemd_cannot_use() -> R {
    let root = tempfile::tempdir()?;
    let home = root.path().join("a home %1");
    std::fs::create_dir(&home)?;
    let out = hoh(&["__unit", "template"], &[("HOME", &home)], root.path())?;
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout)?;
    assert_eq!(text, hoh_app::unit::template(&home)?);
    let exec = format!(
        "ExecStart=\"{}/a home %%1/.local/bin/hoh\" __host ${{HOH_PROJECT}}",
        root.path().display()
    );
    assert!(text.lines().any(|line| line == exec), "{text}");
    let quoted = root.path().join("a\"b");
    let out = hoh(&["__unit", "template"], &[("HOME", &quoted)], root.path())?;
    assert_eq!(out.status.code(), Some(78));
    assert!(out.stdout.is_empty());
    let out = hoh(&["__unit", "template"], &[], root.path())?;
    assert_eq!(out.status.code(), Some(78), "HOME unset");
    Ok(())
}

#[test]
fn enabling_a_project_with_no_user_manager_to_ask_is_refused_with_69_and_writes_nothing() -> R {
    let root = tempfile::tempdir()?;
    let (home, runtime, project) = (
        root.path().join("h"),
        root.path().join("r"),
        root.path().join("p"),
    );
    for dir in [&home, &runtime, &project] {
        std::fs::create_dir(dir)?;
    }
    let vars = [
        ("HOME", home.as_path()),
        ("XDG_RUNTIME_DIR", runtime.as_path()),
    ];
    let out = hoh(
        &["__unit", "enable", project.to_str().ok_or("path")?],
        &vars,
        root.path(),
    )?;
    assert_eq!(
        out.status.code(),
        Some(69),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8(out.stderr)?.contains("does not answer"),
        "names why"
    );
    for dir in [&home, &runtime, &project] {
        assert_eq!(std::fs::read_dir(dir)?.count(), 0, "{}", dir.display());
    }
    Ok(())
}
