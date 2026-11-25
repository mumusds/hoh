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
    let helper = Direct::plain().helper(&mut command, HelperStderr::Null)?;
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
    let helper = Direct::plain().helper(&mut command, HelperStderr::Null)?;
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
    let helper = Direct::plain().helper(&mut command, HelperStderr::Null)?;
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
    let host = Direct::plain().host(&mut command)?;
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

#[test]
fn a_helper_whose_pidfd_cannot_be_had_has_its_group_killed_and_is_reaped() -> R {
    // docs/adr/ADR-031.md, 设计修正 2, A2, the wrapping failure (CLT-700; verification 4): the spawn succeeded but the
    // pidfd cannot be opened. The child is ended as the adapters' supervisor has always ended it: its process group is
    // killed first (a member in it goes too), then its leader, and the leader is reaped within END_WAIT, its pid and
    // group id fixed until then because it is not yet reaped.
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!(
        "sleep 30 < /dev/null > /dev/null 2>&1 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; exec sleep 30",
        member.display()
    ));
    let leader = std::sync::Mutex::new(None);
    let member_pid = std::sync::Mutex::new(None);
    let refused = Helper::spawn_by(
        &mut command,
        &|pid| {
            *leader.lock().unwrap_or_else(PoisonError::into_inner) = Some(pid.as_raw_pid());
            *member_pid.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(written_pid(&member).map_err(|e| io::Error::other(e.to_string()))?);
            Err(io::Error::from_raw_os_error(24))
        },
        END_WAIT,
    );
    assert_eq!(refused.err().and_then(|e| e.raw_os_error()), Some(24));
    let member = member_pid
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .ok_or("the member's pid")?;
    let leader = leader
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner)
        .ok_or("the leader's pid")?;
    let member_gone = ends(member);
    if !member_gone {
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(member).ok_or("pid")?,
            Signal::KILL,
        );
    }
    assert!(member_gone, "the member outlived the wrapping failure");
    // Reaped by the spawn's error path: this test process, its parent, sees no child of that number any more.
    assert!(
        !std::path::Path::new(&format!("/proc/{leader}")).exists(),
        "the leader was not reaped"
    );
    Ok(())
}

#[test]
fn a_helpers_stderr_is_its_policys_never_the_spawners_own() -> R {
    // docs/adr/ADR-031.md, 设计修正 2, round 3 (CLT-1400): a helper's stderr is set by the policy every start names, before
    // the spawn, whatever the command was left with: null, a pipe of its own, or a file it is given. Never the
    // spawner's own fd 2, which in a client only its writer writes (C).
    // fd 2's own link, its path joined at run time (a literal naming it would name whatever file fd 2 is now).
    let ours = std::fs::read_link(std::path::Path::new("/proc/self/fd").join("2"))?;
    let dir = tempfile::tempdir()?;
    let file = dir.path().join("helper.stderr");
    for (policy, want) in [
        (
            HelperStderr::Null,
            Some(std::path::PathBuf::from("/dev/null")),
        ),
        (HelperStderr::Piped, None),
        (
            HelperStderr::File(std::fs::File::create(&file)?),
            Some(file.clone()),
        ),
    ] {
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("readlink /proc/self/fd/2")
            .stdout(std::process::Stdio::piped());
        let mut helper = Direct::plain().helper(&mut command, policy)?;
        let mut seen = String::new();
        std::io::Read::read_to_string(helper.stdout.as_mut().ok_or("stdout")?, &mut seen)?;
        let seen = std::path::PathBuf::from(seen.trim());
        assert!(helper.shared.end(Duration::from_secs(5)).is_some());
        match want {
            Some(want) => assert_eq!(seen, want),
            None => {
                assert!(seen.to_string_lossy().starts_with("pipe:"), "{seen:?}");
                assert!(
                    helper.stderr.is_some(),
                    "a piped helper hands its stderr over"
                );
            }
        }
        if !ours.starts_with("/dev/null") {
            assert_ne!(seen, ours, "the helper has the spawner's own stderr");
        }
    }
    Ok(())
}

/// Wait at most 5 s until no process has number `pid`: reaped by whoever is its parent (this test process).
fn reaped(pid: i32) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

#[test]
fn a_helper_let_go_before_it_was_reaped_is_ended_and_reaped() -> R {
    // docs/adr/ADR-031.md, 设计修正 2, A2, the handle's bounded drop (CLT-1257): the last owner of a helper lets it go
    // while it runs; the drop kills it with its group and reaps it (or leaves it to a reaper thread of its own), so no
    // zombie of it stays in this process. Before, nothing ended it: it ran on and, once it exited, stayed a zombie.
    let helper = Direct::plain().helper(Command::new("sleep").arg("0.3"), HelperStderr::Null)?;
    let pid = i32::try_from(helper.shared.pid())?;
    drop(helper);
    let gone = reaped(pid);
    if !gone {
        let _ = rustix::process::waitpid(
            rustix::process::Pid::from_raw(pid),
            rustix::process::WaitOptions::NOHANG,
        );
    }
    assert!(
        gone,
        "a helper let go of stayed in the process table (a zombie, or still running)"
    );
    Ok(())
}

#[test]
fn a_child_the_wrapping_failure_did_not_reap_in_time_is_reaped_by_a_thread_of_its_own() -> R {
    // A2, the wrapping failure (CLT-1258): the pidfd cannot be had and the child is not reaped within the bound (here
    // none); it is handed to a reaper thread of its own, never dropped as a zombie of this process.
    let mut child_pid = None;
    let pid_seen = std::sync::Mutex::new(None);
    let refused = Helper::spawn_by(
        Command::new("sleep")
            .arg("30")
            .stderr(std::process::Stdio::null()),
        &|pid| {
            *pid_seen.lock().unwrap_or_else(PoisonError::into_inner) = Some(pid.as_raw_pid());
            Err(io::Error::from_raw_os_error(24))
        },
        Duration::ZERO,
    );
    assert!(refused.is_err());
    child_pid.replace(
        pid_seen
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
            .ok_or("pid")?,
    );
    let pid = child_pid.ok_or("pid")?;
    let gone = reaped(pid);
    if !gone {
        let _ = rustix::process::waitpid(
            rustix::process::Pid::from_raw(pid),
            rustix::process::WaitOptions::NOHANG,
        );
    }
    assert!(
        gone,
        "the child the wrapping failure let go of stayed a zombie"
    );
    Ok(())
}

#[test]
fn a_leader_handed_to_the_reaper_thread_is_reaped_when_it_exits_and_its_handle_signals_no_more() -> R
{
    // A2, the handle's drop past its bound (CLT-1257, CLN4-53): a leader that has not exited is handed to a reaper
    // thread of its own, which sees it exit, cleans its group and reaps it; the handle counts as reaped from the hand-
    // over on, so it never signals the group by number again (its id may be another's once the thread reaped it). The
    // thread signals the group only after the exit: a leader that ends its own work writes its mark first.
    let dir = tempfile::tempdir()?;
    let mark = dir.path().join("ended");
    let script = format!("sleep 0.3; : > '{}'", mark.display());
    let helper =
        Direct::plain().helper(Command::new("sh").args(["-c", &script]), HelperStderr::Null)?;
    let pid = i32::try_from(helper.shared.pid())?;
    assert!(helper.shared.hand_over(), "no reaper thread");
    assert!(
        helper.shared.reaped(),
        "the handle still counts its leader as its own"
    );
    assert!(
        !helper.shared.signal_group(Signal::KILL),
        "the handed-over group was signalled by number"
    );
    let gone = reaped(pid);
    if !gone {
        let _ = rustix::process::waitpid(
            rustix::process::Pid::from_raw(pid),
            rustix::process::WaitOptions::NOHANG,
        );
    }
    assert!(
        gone,
        "the reaper thread did not reap the leader once it exited"
    );
    assert!(
        mark.exists(),
        "the reaper thread ended the leader before it exited"
    );
    drop(helper);
    Ok(())
}

/// A leader held on stdin until the test releases it, with a member that outlives its exit.
fn leader_with_member(dir: &std::path::Path) -> R<(Child, OwnedFd, i32)> {
    let member_file = dir.join("member");
    let mut command = Command::new("sh");
    command
        .args(["-c", "sleep 30 < /dev/null & echo $! > \"$1.tmp\"; mv \"$1.tmp\" \"$1\"; read release; exit 0", "reaper-test"])
        .arg(&member_file)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    let child = command.spawn()?;
    let member = written_pid(&member_file)?;
    let held = pidfd_open(
        Pid::from_raw(member).ok_or("member pid")?,
        PidfdFlags::empty(),
    )?;
    Ok((child, held, member))
}

fn check_after_exit_reaper(wrapping_failure: bool) -> R {
    use std::io::Write as _;
    let dir = tempfile::Builder::new().prefix("CLN4-151-").tempdir()?;
    let (mut child, member_held, member) = leader_with_member(dir.path())?;
    let pid = Pid::from_child(&child);
    let mut release = child.stdin.take().ok_or("leader stdin")?;
    let helper = if wrapping_failure {
        // The wrapping failure's actual reaper body, after the initial bounded kill/wait has expired.
        // Enter here with a live leader to deterministically expose the after-exit group kill, independent of
        // whether the kernel happened to process the initial SIGKILL before the zero-bound try_wait.
        std::thread::spawn(move || reap_child_later(child));
        None
    } else {
        let pidfd = pidfd_open(pid, PidfdFlags::empty())?;
        let helper = Helper::own(&mut child, pid, pidfd);
        assert!(helper.shared.hand_over(), "no handle reaper thread");
        Some(helper)
    };
    // No kill before the leader exits. Its stdin handshake holds that boundary without a timing race.
    assert!(!ended(member), "member killed before leader exit");
    release.write_all(b"go\n")?;
    drop(release);
    let leader_reaped = reaped(pid.as_raw_pid());
    let member_ended = ends(member);
    // Exact kernel identity: even a failing mutation must leave no sleeper behind or target a reused pid.
    let _ = pidfd_send_signal(&member_held, Signal::KILL);
    assert!(ends(member), "test cleanup left its member running");
    drop(helper);
    assert!(leader_reaped, "reaper left its leader behind");
    assert!(member_ended, "reaper let a group member outlive the leader");
    Ok(())
}

#[test]
fn the_handle_reaper_ends_members_after_the_leader_exits() -> R {
    check_after_exit_reaper(false)
}

#[test]
fn the_wrapping_failure_reaper_ends_members_after_the_leader_exits() -> R {
    check_after_exit_reaper(true)
}

/// Whether a helper started now holds descriptor `fd`: its exit status of `test -e /proc/self/fd/<fd>`.
fn holds(fd: i32) -> R<bool> {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!("test -e /proc/self/fd/{fd}"));
    let helper = Direct::plain().helper(&mut command, HelperStderr::Null)?;
    if !helper.shared.exited_within(Duration::from_secs(10)) {
        return Err("the helper did not exit".into());
    }
    Ok(helper.shared.reap()?.and_then(|s| s.code()) == Some(0))
}

#[test]
fn only_the_start_inside_inheriting_inherits_its_descriptor() -> R {
    // CR-038 (M3LS-01): `inheriting` leaves a copy of its descriptor (3 or above) inheritable for the start inside it
    // only. Another thread's start waits until it has returned and its copy is closed, so it never holds it; neither
    // does a later start.
    let file = std::fs::File::open("/dev/null")?;
    let (go, started) = std::sync::mpsc::channel();
    let other = std::thread::scope(|scope| -> R<(i32, bool, bool)> {
        let other = scope.spawn(move || -> Result<(i32, bool), String> {
            let fd: i32 = started.recv().map_err(|e| e.to_string())?;
            holds(fd).map(|held| (fd, held)).map_err(|e| e.to_string())
        });
        let inside = inheriting(file.as_fd(), |fd| -> R<bool> {
            go.send(fd)?;
            // The other thread's start would run now if nothing held it back.
            std::thread::sleep(Duration::from_millis(300));
            holds(fd)
        })??;
        let (fd, held) = other.join().map_err(|_| "the other start panicked")??;
        Ok((fd, inside, held))
    })?;
    let (fd, inside, concurrent) = other;
    assert!(fd >= 3, "{fd}");
    assert!(
        inside,
        "the start inside `inheriting` did not inherit descriptor {fd}"
    );
    assert!(!concurrent, "a concurrent start inherited descriptor {fd}");
    assert!(!holds(fd)?, "a later start inherited descriptor {fd}");
    Ok(())
}
