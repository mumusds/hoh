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
    // A listing of /proc that fails is an error, not "none" (docs/adr/ADR-022.md decision 10, v2; CIR-004). A process
    // whose executable or environment cannot be read has gone or is another user's, so it is none of this test's.
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir("/proc")? {
        let e = entry?;
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let ours = std::fs::read_link(e.path().join("exe")).is_ok_and(|exe| exe == binary)
            && std::fs::read(e.path().join("environ"))
                .is_ok_and(|environ| environ.split(|b| *b == 0).any(|v| v == marker));
        if ours {
            found.insert(pid);
        }
    }
    Ok(found)
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
            // ADR-023 decision 8: the build's commit, or `unknown` (a test build is given none).
            let second = stdout.lines().nth(1).unwrap_or_default();
            assert!(
                second == "commit unknown" || second.starts_with("commit "),
                "commit line {second:?}"
            );
            assert_eq!(stdout.lines().count(), 3, "{stdout}");
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
    // Bare `hoh` is built (M1-T08): without a terminal it is a usage error, tests/tui.rs (TEST-001-03); `hoh doctor`
    // is built (M1-T07), tests/doctor.rs (TEST-006-01/02); `hoh attach` and `hoh -p --detach` are built (M2-T06),
    // tests/attach.rs and the test below. A foreground `hoh -p` comes with M2-T13.
    for args in [&["-p", "x"][..], &["web"]] {
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

#[test]
fn attach_in_a_project_with_no_runs_is_a_usage_error_that_starts_and_makes_nothing() -> R {
    // REQ-005 failure mode 2 (docs/adr/ADR-021.md decision 6): with no database there is no run; nothing is started
    // or created for none, with or without a run id.
    let env = Env::new()?;
    for (args, says) in [
        (&["attach"][..], "没有可附着的 Run"),
        (&["attach", "r1"], "RUN_NOT_FOUND"),
    ] {
        let out = env.hoh(args)?;
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(says), "{args:?}: {stderr}");
    }
    assert!(!env.project.path().join(".hoh").exists());
    Ok(())
}

#[test]
fn help_does_not_skip_the_project_check_or_the_version_rule() -> R {
    // M1T06R-03/04: these exit 64 like the forms without --help.
    let env = Env::new()?;
    for args in [
        &["--project", "/nonexistent", "-p", "x", "--help"][..],
        &["--project", "/nonexistent", "--help"],
        &["help", "--version"],
        &["help", "attach", "--version"],
    ] {
        let out = env.hoh(args)?;
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    assert!(!Path::new("/nonexistent").exists());
    Ok(())
}

/// A message that cannot be written does not change the exit code: with stderr on a full device, a usage error still
/// exits 64 and a command not built yet still exits 3, not 101 from a panic while printing (M1QA-01).
#[test]
fn the_exit_code_holds_when_stderr_cannot_be_written() -> R {
    let env = Env::new()?;
    for (args, code) in [(&["--no-such-flag"][..], 64), (&["web"], 3)] {
        let out = Command::new(env!("CARGO_BIN_EXE_hoh"))
            .args(args)
            .current_dir(env.project.path())
            .env_clear()
            .env("HOME", env.home.path())
            .stderr(std::fs::OpenOptions::new().write(true).open("/dev/full")?)
            .output()?;
        assert_eq!(out.status.code(), Some(code), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
    }
    Ok(())
}
