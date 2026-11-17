//! The host's check of codex's own read-only sandbox (docs/adr/ADR-032.md decision 4, "本机沙箱检查"), against fake
//! codex launchers that act out each outcome the recordings and Codex's reviews saw: the token read and the write
//! refused, the write done, the command never run (bwrap's `unshare` refused), the profile name refused, no answer.

use super::*;
use std::time::Duration;

fn env(timeout: Duration) -> ProbeEnv {
    ProbeEnv {
        path: OsString::from("/usr/bin:/bin"),
        fixture_root: None,
        timeout,
        log: None,
        deadline: None,
        cancel: None,
    }
}

/// A fake `codex` running `body` (after it writes its argv, CODEX_HOME and HOME to `seen`).
fn fake(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("codex");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{seen}/argv'\nprintf '%s\\n%s\\n' \"$CODEX_HOME\" \"$HOME\" > '{seen}/env'\n{body}\n",
            seen = dir.display()
        ),
    )
    .expect("write the fake");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

#[test]
fn each_outcome_of_the_check() {
    let dir = tempfile_dir();
    let quick = env(Duration::from_secs(20));
    for (case, body, want) in [
        // $5 is the ws directory (sandbox -P :read-only -C <ws> -- /bin/sh -c <script>).
        (
            "confined",
            "cat \"$5/token\"; echo \"/bin/sh: 1: cannot create $5/probe: Read-only file system\" >&2; exit 1",
            "confined",
        ),
        ("unconfined", "shift 6; exec \"$@\"", "unconfined"),
        (
            "unavailable",
            "echo 'bwrap: unshare user ns: Operation not permitted' >&2; exit 1",
            "unavailable",
        ),
        (
            "unknown profile",
            "echo \"Error: default_permissions refers to unknown built-in profile \\`$3\\`\" >&2; exit 1",
            "unknown",
        ),
    ] {
        let launcher = fake(&dir, body);
        let check = codex_sandbox_check(&launcher, &quick);
        assert_eq!(check.name(), want, "{case}: {check:?}");
        assert_eq!(
            check.withholds(),
            matches!(want, "unconfined" | "unavailable"),
            "{case}"
        );
        // The argv is the check's own, and codex ran with a scratch CODEX_HOME and HOME, removed afterwards.
        let argv = std::fs::read_to_string(dir.join("argv")).expect("argv");
        let argv: Vec<&str> = argv.lines().collect();
        assert_eq!(
            argv[..4],
            ["sandbox", "-P", SANDBOX_PROFILE, "-C"],
            "{case}"
        );
        assert_eq!(argv[5..8], ["--", "/bin/sh", "-c"], "{case}");
        let seen = std::fs::read_to_string(dir.join("env")).expect("env");
        let (codex_home, home) = seen.split_once('\n').expect("two lines");
        let scratch = Path::new(argv[4]).parent().expect("the scratch directory");
        assert_eq!(Path::new(codex_home), scratch.join("codex"), "{case}");
        assert_eq!(Path::new(home.trim()), scratch.join("home"), "{case}");
        assert!(
            !scratch.exists(),
            "{case}: the scratch directory is removed"
        );
    }
    // No answer within the limit: unknown, never an agent failure.
    let launcher = fake(&dir, "sleep 30");
    let check = codex_sandbox_check(&launcher, &env(Duration::from_millis(500)));
    assert_eq!(check.name(), "unknown", "{check:?}");
    assert!(!check.withholds());
    // A launcher that is not there: unknown.
    let check = codex_sandbox_check(&dir.join("missing"), &quick);
    assert_eq!(check.name(), "unknown", "{check:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A fresh directory of this test's own (the crate has no tempfile at run time).
fn tempfile_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let dir = std::env::temp_dir().join(format!(
        "hoh-sandbox-check-test-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}
