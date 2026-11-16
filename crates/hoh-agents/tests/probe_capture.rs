//! What reading a probe command's output keeps once the command is over (M1T07R-47). This binary holds this one test
//! so that the process's file descriptors and threads are its own to count.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::probe::{ProbeEnv, ProbeError, run_status};
use std::path::Path;
use std::time::Duration;

fn count(dir: &str) -> usize {
    std::fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

/// A helper that leaves the command's process group and keeps its output open makes each command time out, and
/// once it has, nothing of it is left in the probe: no file descriptor, no thread still reading.
#[test]
fn an_escaped_helper_holding_the_output_leaves_nothing_in_the_probe()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let pids = tmp.path().join("pids");
    let env = ProbeEnv {
        path: std::env::var_os("PATH").unwrap_or_default(),
        fixture_root: None,
        timeout: Duration::from_millis(300),
        log: None,
        deadline: None,
        cancel: None,
    };
    // The helper inherits stdout and stderr and outlives the command in a session of its own. It appends its own stat
    // line (a builtin read, while it surely runs), so the test ends it and never a process given its number later
    // (ADR-022 decision 10).
    let script = format!(
        "setsid sh -c 'read -r s < /proc/$$/stat; printf \"%s\\n\" \"$s\" >> {pids}; exec sleep 30' & sleep 0.05; exit 0",
        pids = pids.display()
    );
    let before = (count("/proc/self/fd"), count("/proc/self/task"));
    let mut results = Vec::new();
    for _ in 0..5 {
        results.push(run_status(Path::new("/bin/sh"), &["-c", &script], &env));
    }
    let after = (count("/proc/self/fd"), count("/proc/self/task"));
    for stat in std::fs::read(&pids)
        .unwrap_or_default()
        .split(|&b| b == b'\n')
    {
        let pid = String::from_utf8_lossy(stat)
            .split_whitespace()
            .next()
            .and_then(|pid| pid.parse::<u32>().ok());
        let start = hoh_agents::identity::after_command_name(stat)
            .and_then(|fields| fields.split_whitespace().nth(19));
        if let (Some(pid), Some(start)) = (pid, start) {
            let _ = hoh_agents::identity::signal_if_same(pid, start, rustix::process::Signal::KILL);
        }
    }
    for result in &results {
        assert!(
            matches!(result, Err(ProbeError::TimedOut { .. })),
            "{result:?}"
        );
    }
    assert_eq!(
        before, after,
        "file descriptors and threads (before, after)"
    );
    Ok(())
}
