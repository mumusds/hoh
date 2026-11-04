//! The command surface of the `hoh` binary (REQ-003): help, version and usage errors, run as a user would, in a
//! temporary HOME and an empty project directory with COLUMNS=80 and NO_COLOR=1.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const GOLDEN: &str = include_str!("golden/help.txt");

struct Env {
    home: tempfile::TempDir,
    project: tempfile::TempDir,
}

impl Env {
    fn new() -> R<Env> {
        Ok(Env {
            home: tempfile::tempdir()?,
            project: tempfile::tempdir()?,
        })
    }

    fn hoh(&self, args: &[&str]) -> R<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_hoh"))
            .args(args)
            .current_dir(self.project.path())
            .env_clear()
            .env("HOME", self.home.path())
            .env("COLUMNS", "80")
            .env("NO_COLOR", "1")
            .output()?)
    }
}

/// Every path under `dir` with its contents, sorted: equal before and after means nothing was written.
fn tree(dir: &Path) -> R<Vec<(PathBuf, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(&d)? {
            let path = entry?.path();
            if path.is_dir() {
                todo.push(path.clone());
                out.push((path, Vec::new()));
            } else {
                let bytes = std::fs::read(&path)?;
                out.push((path, bytes));
            }
        }
    }
    out.sort();
    Ok(out)
}

/// FNV-1a over the tree, for the record.
fn tree_hash(tree: &[(PathBuf, Vec<u8>)]) -> String {
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for (path, bytes) in tree {
        for b in path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .chain([0u8].iter())
            .chain(bytes)
        {
            h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// Processes running the hoh binary under test with `home` as HOME: anything this test's invocations started (a host
/// would inherit it), and nothing the tests running beside it started.
fn hoh_processes(home: &Path) -> R<BTreeSet<u32>> {
    let binary = std::fs::canonicalize(env!("CARGO_BIN_EXE_hoh"))?;
    let marker = [b"HOME=".as_slice(), home.as_os_str().as_encoded_bytes()].concat();
    Ok(std::fs::read_dir("/proc")
        .map(|entries| {
            entries
                .filter_map(|e| {
                    let e = e.ok()?;
                    let pid: u32 = e.file_name().to_str()?.parse().ok()?;
                    let ours = std::fs::read_link(e.path().join("exe")).ok()? == binary
                        && std::fs::read(e.path().join("environ"))
                            .ok()?
                            .split(|b| *b == 0)
                            .any(|v| v == marker);
                    ours.then_some(pid)
                })
                .collect()
        })
        .unwrap_or_default())
}

/// The first word of each entry line of the section titled `title` in a help text.
fn section(help: &str, title: &str) -> BTreeSet<String> {
    help.lines()
        .skip_while(|l| *l != title)
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect()
}

fn keep(name: &str, text: &str) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(Path::new(&dir).join(name), text)?;
    }
    Ok(())
}

#[test]
fn test_003_01_help_and_version_show_exactly_the_command_surface() -> R {
    let env = Env::new()?;
    let before = tree(env.project.path())?;
    let mut record = String::new();
    let mut helps = Vec::new();
    for args in [
        &["--help"][..],
        &["help"],
        &["attach", "--help"],
        &["web", "--help"],
        &["doctor", "--help"],
        &["--version"],
    ] {
        let out = env.hoh(args)?;
        let stdout = String::from_utf8(out.stdout.clone())?;
        record.push_str(&format!(
            "$ hoh {}\nexit {:?}, stderr {} bytes\n{stdout}\n",
            args.join(" "),
            out.status.code(),
            out.stderr.len()
        ));
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(
            out.stderr.is_empty(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        if args == ["--version"] {
            let first = stdout.lines().next().unwrap_or_default();
            let semver = first.strip_prefix("hoh ").unwrap_or_default();
            let parts: Vec<&str> = semver.split('.').collect();
            assert!(
                parts.len() == 3
                    && parts
                        .iter()
                        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
                "version line {first:?}"
            );
        } else {
            helps.push(stdout);
        }
    }
    let top = &helps[0];
    assert_eq!(
        top.as_bytes(),
        GOLDEN.as_bytes(),
        "hoh --help differs from tests/golden/help.txt"
    );
    assert_eq!(&helps[1], top, "hoh help and hoh --help differ");
    let subcommands = section(top, "子命令：");
    assert_eq!(
        subcommands,
        ["attach", "web", "doctor", "help"].map(String::from).into()
    );
    let options = section(top, "选项：");
    assert_eq!(
        options,
        [
            "-p",
            "--output",
            "--detach",
            "--project",
            "--resume",
            "--help",
            "--version"
        ]
        .map(String::from)
        .into()
    );
    for help in &helps {
        for word in ["daemon", "plan", "mode", "run"] {
            assert!(!section(help, "子命令：").contains(word), "a {word} entry");
            assert!(
                !help
                    .lines()
                    .any(|l| l.split_whitespace().next() == Some(word)),
                "a {word} entry"
            );
        }
        assert!(
            !help.contains("daemon") && !help.contains("__"),
            "an internal entry is shown"
        );
    }
    let after = tree(env.project.path())?;
    record.push_str(&format!(
        "project tree {} -> {}\n",
        tree_hash(&before),
        tree_hash(&after)
    ));
    keep("test-003-01.txt", &record)?;
    assert_eq!(after, before, "the project directory changed");
    Ok(())
}

#[test]
fn test_003_02_undefined_commands_and_combinations_are_usage_errors_without_side_effects() -> R {
    let env = Env::new()?;
    let before = tree(env.project.path())?;
    let processes = hoh_processes(env.home.path())?;
    let mut record = String::new();
    for args in [
        &["plan"][..],
        &["mode", "plan"],
        &["run"],
        &["resume"],
        &["init"],
        &["做个X"],
        &["--resume", "r1"],
        &["--detach"],
        &["--project", "/nonexistent", "-p", "x"],
    ] {
        let out = env.hoh(args)?;
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        record.push_str(&format!(
            "$ hoh {}\nexit {:?}, stdout {} bytes\n{stderr}\n",
            args.join(" "),
            out.status.code(),
            out.stdout.len()
        ));
        assert_eq!(out.status.code(), Some(64), "{args:?}: {stderr}");
        assert!(out.stdout.is_empty(), "{args:?}: stdout {:?}", out.stdout);
        assert!(stderr.contains("usage"), "{args:?}: {stderr}");
        if args[0] == "plan" || args[0] == "mode" {
            assert!(stderr.contains("/plan"), "{args:?}: {stderr}");
        }
        assert_eq!(
            tree(env.project.path())?,
            before,
            "{args:?} changed the project directory"
        );
        assert!(!env.project.path().join(".hoh").exists());
        assert_eq!(
            hoh_processes(env.home.path())?,
            processes,
            "{args:?} left a hoh process"
        );
    }
    assert!(!Path::new("/nonexistent").exists());
    record.push_str(&format!(
        "project tree {}\n",
        tree_hash(&tree(env.project.path())?)
    ));
    keep("test-003-02.txt", &record)?;
    Ok(())
}

#[test]
fn valid_commands_that_are_not_built_yet_say_so_and_exit_3() -> R {
    let env = Env::new()?;
    for args in [&[][..], &["-p", "x"], &["attach"], &["web"], &["doctor"]] {
        let out = env.hoh(args)?;
        assert_eq!(
            out.status.code(),
            Some(3),
            "{args:?}: exit 3 means NOT_IMPLEMENTED/BLOCKED"
        );
        assert!(out.stdout.is_empty(), "stdout is reserved for results");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("NOT_IMPLEMENTED"), "{args:?}: {stderr}");
    }
    assert!(!env.project.path().join(".hoh").exists());
    Ok(())
}
