use super::*;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The pid a shell wrote to `file` (written to a temporary name, then renamed), waited for at most 10 s.
fn written_pid(file: &std::path::Path) -> R<i32> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(text) = std::fs::read_to_string(file) {
            return Ok(text.trim().parse()?);
        }
        if Instant::now() > end {
            return Err(format!("{} never written", file.display()).into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Whether process `pid` has ended: gone from /proc, or a zombie there (its exit is done, whoever reaps it).
fn ended(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state == "Z" || state == "X"),
    }
}

/// Wait at most 5 s for process `pid` to end.
fn ends(pid: i32) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while !ended(pid) {
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

#[test]
fn a_helper_that_exits_by_itself_has_its_group_cleaned_before_it_is_reaped() -> R {
    // docs/adr/ADR-031.md, 设计修正 2, A2 (CLT-600): the leader exits 0 by itself while a member of its group lives on.
    // Seeing the exit does not consume it, so the group's id is still the leader's and the member is killed by it;
    // only then is the leader reaped, its status published, and numeric group signals refused.
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!(
        "sleep 30 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; exit 0",
        member.display()
    ));
    let helper = Direct.helper(&mut command)?;
    let member = written_pid(&member)?;
    let shared = &helper.shared;
    assert!(
        shared.exited_within(Duration::from_secs(5)),
        "the leader did not exit"
    );
    assert_eq!(shared.observe()?.and_then(|s| s.code()), Some(0));
    assert!(!shared.reaped(), "seeing the exit reaped it");
    assert!(!ended(member), "the member ended by itself");
    assert!(
        shared.signal_group(Signal::KILL),
        "the group was not signalled"
    );
    assert!(ends(member), "the member outlived the group's kill");
    assert_eq!(shared.reap()?.and_then(|s| s.code()), Some(0));
    assert!(shared.reaped());
    assert!(
        !shared.signal_group(Signal::KILL),
        "a reaped leader's group was signalled by number"
    );
    Ok(())
}

#[test]
fn a_helper_is_reaped_once_and_its_status_is_shared() -> R {
    // A2: only `reap` consumes the status; observing before it and asking after it read the same published status.
    let mut command = Command::new("sh");
    command.arg("-c").arg("exit 3");
    let helper = Direct.helper(&mut command)?;
    let shared = helper.shared.clone();
    assert!(shared.exited_within(Duration::from_secs(5)));
    assert_eq!(shared.observe()?.and_then(|s| s.code()), Some(3));
    assert_eq!(shared.observe()?.and_then(|s| s.code()), Some(3));
    assert_eq!(shared.reap()?.and_then(|s| s.code()), Some(3));
    assert_eq!(shared.reap()?.and_then(|s| s.code()), Some(3));
    assert_eq!(shared.observe()?.and_then(|s| s.code()), Some(3));
    Ok(())
}

#[test]
fn ending_a_helper_kills_it_and_its_group_and_reaps_it() -> R {
    // A2: a helper's end kills its leader and its group and reaps it within its limit.
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!(
        "sleep 30 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; exec sleep 30",
        member.display()
    ));
    let helper = Direct.helper(&mut command)?;
    let member = written_pid(&member)?;
    let status = helper.shared.end(END_WAIT).ok_or("not reaped")?;
    assert_eq!(status.signal(), Some(9), "{status:?}");
    assert!(ends(member), "a member of its group lived on");
    Ok(())
}

#[test]
fn a_host_can_only_be_waited_for_and_dropping_it_leaves_it_running() -> R {
    // A (CLT-650): a host comes back wait-only; dropping it signals nothing and the host lives on.
    let mut command = Command::new("sleep");
    command.arg("30");
    let host = Direct.host(&mut command)?;
    let raw = i32::try_from(host.id())?;
    let pidfd = pidfd_open(Pid::from_raw(raw).ok_or("no pid")?, PidfdFlags::empty())?;
    drop(host);
    std::thread::sleep(Duration::from_millis(50));
    assert!(!ended(raw), "dropping the host's object ended it");
    pidfd_send_signal(&pidfd, Signal::KILL)?;
    // Reaped by the test, its parent, so nothing is left behind.
    let _ = waitid(WaitId::PidFd(pidfd.as_fd()), WaitIdOptions::EXITED);
    Ok(())
}
