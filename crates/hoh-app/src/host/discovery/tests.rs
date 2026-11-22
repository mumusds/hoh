use super::*;
use std::time::{Duration, Instant};

/// Whether process `pid` is gone (or a zombie no one waits for yet).
fn gone(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z')),
    }
}

fn wait_gone(pid: u32) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn a_test_command_is_judged_by_its_exit_and_leaves_nothing_of_its_group_running() {
    // docs/adr/ADR-039.md decision 8: what the command left in its group ends with it.
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path();
    let ran = run_tests(
        project,
        &project.join("logs"),
        &["sleep 30 & echo $! > left.pid; echo 'test a ... FAILED'; echo 'test b ... ok'; exit 101".to_owned()],
        &|| false,
    )
    .expect("ran");
    assert_eq!(
        ran,
        json!({"commands": [{"command": "sleep 30 & echo $! > left.pid; echo 'test a ... FAILED'; echo 'test b ... ok'; exit 101",
            "exit_code": 101, "timed_out": false, "status": "fail", "failed_tests": ["a"]}]})
    );
    let left: u32 = std::fs::read_to_string(project.join("left.pid"))
        .expect("pid")
        .trim()
        .parse()
        .expect("a pid");
    assert!(
        wait_gone(left),
        "the command's background process {left} still runs"
    );
    let passed = run_tests(
        project,
        &project.join("logs2"),
        &["true".to_owned()],
        &|| false,
    )
    .expect("ran");
    assert_eq!(passed["commands"][0]["status"], "pass");
    assert_eq!(passed["commands"][0]["failed_tests"], json!([]));
}

#[test]
fn the_hosts_stop_kills_a_running_test_command_with_its_group_and_records_nothing() {
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path();
    let start = Instant::now();
    let stopped = run_tests(
        project,
        &project.join("logs"),
        &["sleep 30 & echo $! > left.pid; sleep 30".to_owned()],
        &|| start.elapsed() > Duration::from_millis(500),
    );
    assert!(
        stopped
            .as_ref()
            .is_err_and(|e| e.contains("the host stops")),
        "{stopped:?}"
    );
    assert!(start.elapsed() < Duration::from_secs(10));
    let left: u32 = std::fs::read_to_string(project.join("left.pid"))
        .expect("pid")
        .trim()
        .parse()
        .expect("a pid");
    assert!(
        wait_gone(left),
        "the group's other process {left} still runs"
    );
}
