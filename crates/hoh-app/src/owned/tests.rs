use super::*;
use std::os::fd::AsFd as _;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

impl Gate {
    /// (helper starts in flight, host starts in flight, registered helpers, handed-off hosts).
    fn counts(&self) -> (usize, usize, usize, usize) {
        let state = self.locked();
        (
            state.helpers_in_flight,
            state.hosts_in_flight,
            state.helpers.len(),
            state.hosts.len(),
        )
    }
}

/// Whether process `pid` has ended: gone from /proc, or a zombie there.
fn ended(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => true,
        Ok(stat) => stat
            .rsplit_once(") ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state == "Z" || state == "X"),
    }
}

/// Wait at most 5 s for process `pid` to end.
fn ends(pid: u32) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while !ended(pid) {
        if Instant::now() > end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    true
}

/// The pid a shell wrote to `file`, waited for at most 10 s.
fn written_pid(file: &std::path::Path) -> R<u32> {
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

/// A shell that leaves a member of its group (its pid written to `member`) and then sleeps.
fn with_member(member: &std::path::Path) -> Command {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!(
        "sleep 30 & echo $! > '{0}.tmp' && mv '{0}.tmp' '{0}'; exec sleep 30",
        member.display()
    ));
    command
}

/// Kill and reap host `host` (the test's own child).
fn reap_host(mut host: WaitOnly) {
    if let Ok(pidfd) = rustix::process::pidfd_open(
        rustix::process::Pid::from_raw(i32::try_from(host.id()).unwrap_or(i32::MAX))
            .unwrap_or(rustix::process::Pid::INIT),
        rustix::process::PidfdFlags::empty(),
    ) {
        let _ = rustix::process::pidfd_send_signal(pidfd.as_fd(), Signal::KILL);
    }
    let _ = host.wait();
}

#[test]
fn a_start_after_the_stop_is_refused_and_starts_nothing() {
    // 设计修正 2, A: once the stop began the gate gives no ticket, for a helper or a host.
    let gate = Gate::default();
    assert_eq!(gate.stop(Duration::from_millis(100)), Stopped::default());
    assert!(gate.helper(&mut Command::new("true")).is_err());
    assert!(gate.host(&mut Command::new("true")).is_err());
    assert_eq!(gate.counts(), (0, 0, 0, 0));
}

#[test]
fn the_stop_kills_every_registered_helper_and_its_group_and_reaps_it() -> R {
    // A and A2: a registered helper's group and leader are killed, then it is reaped through its handle.
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    let gate = Gate::default();
    let helper = gate.helper(&mut with_member(&member))?;
    let member = written_pid(&member)?;
    let stopped = gate.stop(Duration::from_millis(1500));
    assert_eq!(stopped.unconfirmed, Vec::<u32>::new());
    assert_eq!(stopped.helpers_in_flight, 0);
    assert!(helper.shared.reaped(), "not reaped by the stop");
    assert!(ends(member), "a member of its group outlived the stop");
    Ok(())
}

#[test]
fn a_start_in_flight_when_the_stop_begins_is_killed_at_its_hand_off() -> R {
    // A (CLT-301): the stop began between a helper's spawn and its registration; the stop waits for the start in
    // flight, whose hand-off kills it; then the stop reaps it. Nothing is left.
    let dir = tempfile::tempdir()?;
    let member = dir.path().join("member");
    let gate = Arc::new(Gate::default());
    let (spawned, held) = std::sync::mpsc::channel::<()>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let starter = {
        let (gate, member) = (gate.clone(), member.clone());
        std::thread::spawn(move || {
            gate.helper_by(&mut with_member(&member), &|command| {
                let helper = Direct.helper(command);
                let _ = spawned.send(());
                let _ = go.lock().map(|go| go.recv_timeout(Duration::from_secs(10)));
                helper
            })
        })
    };
    held.recv_timeout(Duration::from_secs(10))?;
    let stopper = {
        let gate = gate.clone();
        std::thread::spawn(move || gate.stop(Duration::from_millis(1500)))
    };
    std::thread::sleep(Duration::from_millis(100));
    release.send(())?;
    let stopped = stopper.join().map_err(|_| "the stop panicked")?;
    let helper = starter.join().map_err(|_| "the start panicked")??;
    assert_eq!(stopped.helpers_in_flight, 0, "{stopped:?}");
    assert_eq!(stopped.unconfirmed, Vec::<u32>::new(), "{stopped:?}");
    assert!(helper.shared.reaped());
    assert!(
        ends(written_pid(&member)?),
        "a member of its group lived on"
    );
    assert_eq!(gate.counts().0, 0);
    Ok(())
}

#[test]
fn a_start_still_in_flight_at_the_bound_is_reported_not_claimed() -> R {
    // A (CLT-400): a start held past the stop's bound is reported as in flight; when it hands off later (the process
    // still alive here) it kills what it started.
    let gate = Arc::new(Gate::default());
    let (spawned, held) = std::sync::mpsc::channel::<u32>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let starter = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            gate.helper_by(Command::new("sleep").arg("30"), &|command| {
                let helper = Direct.helper(command)?;
                let _ = spawned.send(helper.shared.pid());
                let _ = go.lock().map(|go| go.recv_timeout(Duration::from_secs(10)));
                Ok(helper)
            })
        })
    };
    let pid = held.recv_timeout(Duration::from_secs(10))?;
    let stopped = gate.stop(Duration::from_millis(200));
    assert_eq!(stopped.helpers_in_flight, 1, "{stopped:?}");
    assert!(
        !ended(pid),
        "the held start's process ended before its hand-off"
    );
    release.send(())?;
    let helper = starter.join().map_err(|_| "the start panicked")??;
    assert!(
        helper.shared.exited_within(Duration::from_secs(5)),
        "its late hand-off did not kill what it started"
    );
    assert!(helper.shared.end(Duration::from_secs(5)).is_some());
    assert_eq!(gate.counts().0, 0);
    Ok(())
}

#[test]
fn a_ticket_is_counted_down_exactly_once_whatever_the_start_does() -> R {
    // A (CLT-504, CLT-651, CLT-750): a spawn that fails, a spawn that panics before it returns, a spawn that panics
    // after it started its process, and a spawn that succeeds each count their start down exactly once (another
    // start held in flight throughout, so a second count-down would show); only a spawned helper registers.
    let gate = Gate::default();
    let held_helper = gate.take(Kind::Helper)?;
    let held_host = gate.take(Kind::Host)?;
    let failed = gate.helper_by(&mut Command::new("true"), &|_| {
        Err(std::io::Error::other("no"))
    });
    assert!(failed.is_err());
    assert_eq!(gate.counts(), (1, 1, 0, 0));
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        gate.helper_by(&mut Command::new("true"), &|_| {
            panic!("a spawn that panics")
        })
    }));
    assert!(panicked.is_err());
    assert_eq!(gate.counts(), (1, 1, 0, 0));
    let started = Mutex::new(None);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut ticket = gate.take(Kind::Helper)?;
        let helper = Direct.helper(Command::new("sleep").arg("30"))?;
        ticket.stage = Stage::SpawnedHelper(helper.shared.clone());
        if let Ok(mut started) = started.lock() {
            *started = Some(helper.shared.clone());
        }
        panic!("a start that panics before its hand-off");
        #[allow(unreachable_code)]
        std::io::Result::Ok(())
    }));
    assert!(panicked.is_err());
    assert_eq!(
        gate.counts(),
        (1, 1, 1, 0),
        "the unwound start was not registered by its ticket's drop"
    );
    let host_failed = gate.host_by(&mut Command::new("true"), &|_| {
        Err(std::io::Error::other("no"))
    });
    assert!(host_failed.is_err());
    assert_eq!(gate.counts(), (1, 1, 1, 0));
    let helper = gate.helper(Command::new("sleep").arg("30"))?;
    assert_eq!(gate.counts(), (1, 1, 2, 0));
    drop((held_helper, held_host));
    assert_eq!(gate.counts(), (0, 0, 2, 0));
    let stopped = gate.stop(Duration::from_secs(5));
    assert_eq!(stopped.unconfirmed, Vec::<u32>::new());
    assert!(helper.shared.reaped());
    let started = started.lock().map_err(|_| "poisoned")?.take();
    assert!(started.is_some_and(|shared| shared.reaped()));
    Ok(())
}

#[test]
fn a_host_is_handed_off_and_never_signalled_by_the_stop() -> R {
    // A (CLT-502, CLT-650): a host started through the gate is recorded, not registered as a helper; the stop and
    // dropping its object leave it running.
    let gate = Gate::default();
    let host = gate.host(Command::new("sleep").arg("30"))?;
    let pid = host.id();
    let stopped = gate.stop(Duration::from_millis(300));
    assert_eq!(stopped.hosts, vec![pid]);
    assert_eq!(stopped.unconfirmed, Vec::<u32>::new());
    std::thread::sleep(Duration::from_millis(50));
    assert!(!ended(pid), "the stop ended the host");
    reap_host(host);
    Ok(())
}

#[test]
fn a_host_start_in_flight_when_the_stop_begins_completes_and_is_handed_off() -> R {
    // A (S1, CLT-502): a host whose ticket was taken before the stop completes and lives on; the stop counts it as a
    // host start, never as a helper.
    let gate = Arc::new(Gate::default());
    let (taken, held) = std::sync::mpsc::channel::<()>();
    let (release, go) = std::sync::mpsc::channel::<()>();
    let go = Mutex::new(go);
    let starter = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            gate.host_by(Command::new("sleep").arg("30"), &|command| {
                let _ = taken.send(());
                let _ = go.lock().map(|go| go.recv_timeout(Duration::from_secs(10)));
                Direct.host(command)
            })
        })
    };
    held.recv_timeout(Duration::from_secs(10))?;
    let stopped = gate.stop(Duration::from_millis(100));
    assert_eq!(stopped.hosts_in_flight, 1, "{stopped:?}");
    release.send(())?;
    let host = starter.join().map_err(|_| "the start panicked")??;
    let pid = host.id();
    std::thread::sleep(Duration::from_millis(50));
    assert!(!ended(pid), "the host was ended");
    assert_eq!(gate.counts(), (0, 0, 0, 1));
    reap_host(host);
    Ok(())
}

#[test]
fn a_helper_started_through_the_gate_keeps_the_attempt_id_it_inherits() -> R {
    // A (CLT-802, CLT-954; verification 14): a client running inside an Attempt passes `HOH_ATTEMPT_ID` on to its
    // helpers unchanged, so the outer host's REQ-043 tracking still finds them; the gate neither removes nor replaces
    // it (nor clears the environment).
    use std::io::Read as _;
    let gate = Gate::default();
    let mut helper = gate.helper(
        Command::new("sh")
            .arg("-c")
            .arg("printf %s \"$HOH_ATTEMPT_ID\"")
            .env("HOH_ATTEMPT_ID", "R-1/T-2/A-954")
            .stdout(std::process::Stdio::piped()),
    )?;
    let mut said = String::new();
    helper
        .stdout
        .take()
        .ok_or("no stdout")?
        .read_to_string(&mut said)?;
    assert_eq!(said, "R-1/T-2/A-954");
    assert!(helper.shared.end(Duration::from_secs(5)).is_some());
    Ok(())
}

/// A shell (a helper of its own) that starts a child in a session of its own (`setsid`, its pid written to `apart`)
/// and waits for it.
fn setting_apart(apart: &std::path::Path) -> R<Helper> {
    Ok(Direct.helper(Command::new("sh").arg("-c").arg(format!(
        "setsid sh -c 'echo $$ > \"$0.tmp\" && mv \"$0.tmp\" \"$0\"; exec sleep 30' '{}' \
         < /dev/null > /dev/null 2>&1 & wait",
        apart.display()
    )))?)
}

#[test]
fn the_sweep_kills_a_descendant_that_left_its_group_and_ends_on_a_clean_scan() -> R {
    // B (CLT-302): a process a helper set apart (its own session, so its group's kill does not reach it) is found by
    // the parent links under the root, killed by pidfd, and the sweep ends once a scan finds nothing alive; the root
    // itself is not touched.
    let dir = tempfile::tempdir()?;
    let apart = dir.path().join("apart");
    let root = setting_apart(&apart)?;
    let child = written_pid(&apart)?;
    let at = Instant::now();
    let left = descendants::sweep(root.shared.pid(), at + Duration::from_millis(1500));
    assert_eq!(left, Vec::<u32>::new());
    assert!(
        at.elapsed() < Duration::from_millis(1000),
        "{:?}",
        at.elapsed()
    );
    assert!(ends(child), "the child set apart lived on");
    // The root's wait returned once its child was killed and it ended by itself (exit 0): it was never signalled.
    assert!(root.shared.exited_within(Duration::from_secs(5)));
    assert_eq!(
        root.shared.reap()?.and_then(|status| status.code()),
        Some(0),
        "the root did not end by itself"
    );
    Ok(())
}

#[test]
fn the_sweep_counts_a_zombie_as_ended_and_reports_what_its_bound_left_alive() -> R {
    // B: a descendant that is a zombie (its parent never reaps it) has ended: the scan skips it and ends clean at once,
    // never killing the root. With its bound already come, one pass kills what it finds and reports it, not claimed.
    let root = Direct.helper(
        Command::new("sh")
            .arg("-c")
            .arg("sleep 0.1 & exec sleep 30"),
    )?;
    let end = Instant::now() + Duration::from_secs(5);
    while !std::fs::read_to_string(format!("/proc/{}/task/{0}/children", root.shared.pid()))
        .is_ok_and(|children| {
            children
                .split_whitespace()
                .any(|c| c.parse().is_ok_and(ended))
        })
    {
        assert!(Instant::now() < end, "no zombie child under the root");
        std::thread::sleep(Duration::from_millis(5));
    }
    let at = Instant::now();
    assert_eq!(
        descendants::sweep(root.shared.pid(), at + Duration::from_millis(1500)),
        Vec::<u32>::new()
    );
    assert!(
        at.elapsed() < Duration::from_millis(500),
        "{:?}",
        at.elapsed()
    );
    assert!(!ended(root.shared.pid()), "the sweep touched its root");
    assert!(root.shared.end(Duration::from_secs(5)).is_some());

    let dir = tempfile::tempdir()?;
    let apart = dir.path().join("apart");
    let root = setting_apart(&apart)?;
    let child = written_pid(&apart)?;
    assert_eq!(
        descendants::sweep(root.shared.pid(), Instant::now()),
        vec![child]
    );
    assert!(ends(child), "the child it reported was not killed");
    assert!(root.shared.exited_within(Duration::from_secs(5)));
    let _ = root.shared.reap();
    Ok(())
}
