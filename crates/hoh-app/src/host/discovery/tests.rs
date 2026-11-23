use super::*;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A test's owner: it records every process (unless `refuse`), stops when `stop` says so, and keeps what it saw.
struct Owner<'a> {
    stop: &'a dyn Fn() -> bool,
    refuse: bool,
    started: Mutex<Vec<(u32, String)>>,
    ended: Mutex<Vec<String>>,
}

impl<'a> Owner<'a> {
    fn new(stop: &'a dyn Fn() -> bool) -> Owner<'a> {
        Owner {
            stop,
            refuse: false,
            started: Mutex::default(),
            ended: Mutex::default(),
        }
    }
}

impl TestOwner for Owner<'_> {
    fn stopping(&self) -> bool {
        (self.stop)()
    }

    fn started(
        &self,
        index: usize,
        _nonce: &str,
        _command: &str,
        identity: &hoh_agents::identity::Identity,
        log: &str,
    ) -> Result<Option<String>, String> {
        if self.refuse {
            return Ok(None);
        }
        self.started
            .lock()
            .expect("started")
            .push((identity.pid, log.to_owned()));
        Ok(Some(format!("process-{index}")))
    }

    fn ended(&self, process: &str) {
        self.ended.lock().expect("ended").push(process.to_owned());
    }
}

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
    let owner = Owner::new(&|| false);
    let ran = run_tests(
        project,
        Path::new("logs"),
        &["sleep 30 & echo $! > left.pid; echo 'test a ... FAILED'; echo 'test b ... ok'; exit 101".to_owned()],
        &owner,
    )
    .expect("ran");
    let log = owner.started.lock().expect("started")[0].1.clone();
    assert!(log.starts_with("logs/0-") && log.ends_with(".log"), "{log}");
    assert_eq!(
        ran,
        json!({"commands": [{"command": "sleep 30 & echo $! > left.pid; echo 'test a ... FAILED'; echo 'test b ... ok'; exit 101",
            "exit_code": 101, "timed_out": false, "incomplete": false, "status": "fail", "failed_tests": ["a"],
            "log": log}]})
    );
    // Recorded before it ran, its end recorded once its group was seen empty (docs/adr/ADR-039.md, DC-T07 (1), (2)).
    assert_eq!(*owner.ended.lock().expect("ended"), ["process-0"]);
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
        Path::new("logs2"),
        &["true".to_owned()],
        &Owner::new(&|| false),
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
    let stop = || start.elapsed() > Duration::from_millis(500);
    let owner = Owner::new(&stop);
    let stopped = run_tests(
        project,
        Path::new("logs"),
        &["sleep 30 & echo $! > left.pid; sleep 30".to_owned()],
        &owner,
    );
    assert!(
        stopped.as_ref().is_err_and(|e| e.contains("stopped")),
        "{stopped:?}"
    );
    assert_eq!(*owner.ended.lock().expect("ended"), ["process-0"]);
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

#[test]
fn a_command_whose_record_the_run_refuses_runs_nothing() {
    // DC-T07 (1), (4): the gate opens only after the record; a run that no longer owns its tests records none, and the
    // command behind the closed gate never runs.
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path();
    let stop = || false;
    let owner = Owner {
        refuse: true,
        ..Owner::new(&stop)
    };
    let refused = run_tests(
        project,
        Path::new("logs"),
        &["touch ran".to_owned()],
        &owner,
    );
    assert!(
        refused
            .as_ref()
            .is_err_and(|e| e.contains("no longer owns")),
        "{refused:?}"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !project.join("ran").exists(),
        "the command ran behind a closed gate"
    );
}

#[test]
fn a_rerun_never_truncates_an_earlier_log() {
    // DC-T07 (3): each start writes a new log file.
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path();
    let owner = Owner::new(&|| false);
    for _ in 0..2 {
        run_tests(
            project,
            Path::new("logs"),
            &["echo once".to_owned()],
            &owner,
        )
        .expect("ran");
    }
    let logs: Vec<String> = owner
        .started
        .lock()
        .expect("started")
        .iter()
        .map(|(_, l)| l.clone())
        .collect();
    assert_eq!(logs.len(), 2);
    assert_ne!(logs[0], logs[1]);
    for log in &logs {
        assert_eq!(
            std::fs::read_to_string(project.join(log)).expect("log"),
            "once\n"
        );
    }
}

#[test]
fn a_test_command_ended_by_a_signal_is_incomplete() {
    // M3N-101 (docs/adr/ADR-039.md, round 2): a real command killed by a signal, the command itself (its leader) or a
    // process its shell reports as killed (128 + the signal), leaves its failed tests incomplete.
    let dir = tempfile::tempdir().expect("dir");
    let project = dir.path();
    for command in [
        "echo 'test a ... FAILED'; kill -KILL $$",
        "sh -c 'echo \"test a ... FAILED\"; kill -KILL $$'",
    ] {
        let ran = run_tests(
            project,
            Path::new("logs"),
            &[command.to_owned()],
            &Owner::new(&|| false),
        )
        .expect("ran");
        let ran = &ran["commands"][0];
        assert_eq!(
            (
                &ran["incomplete"],
                &ran["timed_out"],
                &ran["status"],
                &ran["failed_tests"]
            ),
            (&json!(true), &json!(false), &json!("fail"), &json!(["a"])),
            "{command}: {ran}"
        );
    }
}
