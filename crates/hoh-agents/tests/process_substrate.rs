//! M1-T02 process substrate: orphans, backpressure, oversized frames and children that ignore SIGTERM
//! (docs/13-adapters.md "进程与终结").

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::frame::{Frame, HEAD_BYTES};
use hoh_agents::process::{
    Cause, Config, Escalation, Event, ExitReport, Process, Signal, Target, Termination,
    group_members,
};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

type R = Result<(), Box<dyn std::error::Error>>;

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script);
    command
}

fn alive(pid: u32) -> bool {
    // A zombie still has a /proc entry; only a reaped process is gone.
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Read events until the exit report, with an overall bound.
async fn until_exit(
    p: &mut Process,
    bound: Duration,
) -> Result<(Vec<Frame>, ExitReport), Box<dyn std::error::Error>> {
    let mut frames = Vec::new();
    let collect = async {
        while let Some(event) = p.next().await {
            match event {
                Event::Frame(f) => frames.push(f),
                Event::Exit(report) => return Some(*report),
            }
        }
        None
    };
    let report = tokio::time::timeout(bound, collect)
        .await?
        .ok_or("events ended without an exit report")?;
    Ok((frames, report))
}

async fn first_line(p: &mut Process) -> Result<String, Box<dyn std::error::Error>> {
    match tokio::time::timeout(Duration::from_secs(10), p.next()).await? {
        Some(Event::Frame(Frame::Line(l))) => Ok(String::from_utf8(l)?),
        other => Err(format!("expected a frame, got {other:?}").into()),
    }
}

#[tokio::test]
async fn orphans_left_in_the_group_are_killed_when_the_leader_exits() -> R {
    // The grandchild keeps stdout open: without the group kill the stream would never end.
    let mut p = Process::spawn(sh("sleep 600 & echo \"$!\"; exit 3"), Config::default())?;
    let pgid = p.pid();
    let grandchild: u32 = first_line(&mut p).await?.parse()?;
    let (_, report) = until_exit(&mut p, Duration::from_secs(10)).await?;
    assert_eq!(report.status, Some(Termination::Code(3)));
    assert_eq!(report.cause, Cause::Exited);
    assert!(report.steps.is_empty(), "{:?}", report.steps);
    assert_eq!(report.orphans, 1);
    assert!(report.group_reaped);
    assert!(report.stdout.eof);
    assert!(!alive(grandchild), "grandchild {grandchild} survived");
    assert!(group_members(pgid).is_empty());
    Ok(())
}

#[tokio::test]
async fn a_child_ignoring_int_and_term_is_killed_after_the_graces() -> R {
    // Both the leader and its same-group grandchild ignore SIGINT and SIGTERM (ignored dispositions survive exec).
    let script = "trap '' INT TERM; sleep 600 & echo \"$!\"; while :; do sleep 1; done";
    let mut p = Process::spawn(sh(script), Config::default())?;
    let pgid = p.pid();
    let grandchild: u32 = first_line(&mut p).await?.parse()?;
    let grace = Duration::from_secs(1);
    let cancelled = Instant::now();
    p.cancel(Escalation {
        native: Some(Signal::Interrupt),
        native_grace: grace,
        term_grace: grace,
        kill_grace: Duration::from_secs(5),
    });
    let (_, report) = until_exit(&mut p, Duration::from_secs(10)).await?;
    let elapsed = cancelled.elapsed();
    let sent: Vec<_> = report.steps.iter().map(|s| (s.target, s.signal)).collect();
    assert_eq!(
        sent,
        vec![
            (Target::Leader, Signal::Interrupt),
            (Target::Group, Signal::Terminate),
            (Target::Group, Signal::Kill)
        ]
    );
    for pair in report.steps.windows(2) {
        assert!(pair[1].at - pair[0].at >= grace, "{:?}", report.steps);
    }
    assert!(elapsed < Duration::from_secs(4), "took {elapsed:?}");
    assert_eq!(report.status, Some(Termination::Signal(9)));
    assert_eq!(report.cause, Cause::Cancelled);
    assert!(report.group_reaped);
    assert!(!alive(grandchild), "grandchild {grandchild} survived");
    assert!(group_members(pgid).is_empty());
    Ok(())
}

#[tokio::test]
async fn an_oversized_frame_is_refused_with_a_bounded_buffer() -> R {
    // Two frames, then 8 MiB without LF. After the refusal the rest is drained, so the child finishes on its own.
    let script = "printf 'a\\nb\\n'; head -c 8388608 /dev/zero | tr '\\0' x; exit 0";
    let normal = Process::spawn(sh("sleep 0.5; echo fine"), Config::default());
    let mut p = Process::spawn(sh(script), Config::default())?;
    let mut normal = normal?;
    let (frames, report) = until_exit(&mut p, Duration::from_secs(20)).await?;
    let config = Config::default();
    let line = |s: &str| Frame::Line(s.as_bytes().to_vec());
    assert_eq!(frames.get(..2), Some(&[line("a"), line("b")][..]));
    let Some(Frame::TooLarge(refused)) = frames.get(2) else {
        return Err(format!("expected a refused frame, got {:?}", frames.get(2)).into());
    };
    assert_eq!(frames.len(), 3, "nothing is framed after the refusal");
    assert!(refused.length > config.frame_limit);
    assert!(refused.length <= config.frame_limit + config.read_chunk);
    assert_eq!(refused.head, vec![b'x'; HEAD_BYTES]);
    let digest: String = Sha256::digest(vec![b'x'; refused.length])
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(refused.prefix_sha256, digest);
    assert!(report.stdout.max_buffered <= config.frame_limit + config.read_chunk);
    assert_eq!(report.stdout.bytes, 4 + (8 << 20));
    assert!(report.stdout.eof);
    assert_eq!(report.status, Some(Termination::Code(0)));
    // Another process supervised at the same time is unaffected.
    let (frames, report) = until_exit(&mut normal, Duration::from_secs(10)).await?;
    assert_eq!(frames, vec![line("fine")]);
    assert_eq!(report.status, Some(Termination::Code(0)));
    Ok(())
}

#[tokio::test]
async fn a_slow_consumer_blocks_the_writer_while_stderr_keeps_draining() -> R {
    let dir = tempfile::tempdir()?;
    let marker = |name: &str| dir.path().join(name);
    let script = format!(
        "head -c 4194304 /dev/zero >&2; touch '{e}'; \
         awk 'BEGIN {{ for (i = 1; i <= 20000; i++) printf \"%d %0100d\\n\", i, 0 }}'; touch '{o}'",
        e = marker("stderr-done").display(),
        o = marker("stdout-done").display()
    );
    let config = Config {
        queue: 16,
        stderr_tail: 1024,
        ..Config::default()
    };
    let mut p = Process::spawn(sh(&script), config)?;
    // 4 MiB of stderr nobody consumes must not stall the child...
    let wait_for = Instant::now() + Duration::from_secs(10);
    while !marker("stderr-done").exists() && Instant::now() < wait_for {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(marker("stderr-done").exists(), "stderr was not drained");
    // ...while 2 MB of frames nobody reads leave it blocked: memory stays bounded by the queue and one read.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        !marker("stdout-done").exists(),
        "the writer was not held back"
    );
    let (frames, report) = until_exit(&mut p, Duration::from_secs(20)).await?;
    assert_eq!(frames.len(), 20000);
    for (i, frame) in frames.iter().enumerate() {
        let Frame::Line(l) = frame else {
            return Err(format!("frame {i} is {frame:?}").into());
        };
        assert!(
            l.starts_with(format!("{} ", i + 1).as_bytes()),
            "frame {i} out of order"
        );
    }
    assert!(marker("stdout-done").exists());
    assert_eq!(report.status, Some(Termination::Code(0)));
    assert_eq!(report.stderr.bytes, 4 << 20);
    assert_eq!(report.stderr_tail.len(), 1024);
    assert!(
        report.stdout.max_buffered <= Config::default().frame_limit + Config::default().read_chunk
    );
    Ok(())
}

#[tokio::test]
async fn the_wall_deadline_escalates_without_a_cancel() -> R {
    let config = Config {
        deadline: Some(Duration::from_millis(300)),
        escalation: Escalation {
            native: None,
            native_grace: Duration::ZERO,
            term_grace: Duration::from_secs(2),
            kill_grace: Duration::from_secs(2),
        },
        ..Config::default()
    };
    let started = Instant::now();
    let mut command = Command::new("sleep");
    command.arg("600");
    let mut p = Process::spawn(command, config)?;
    let (_, report) = until_exit(&mut p, Duration::from_secs(10)).await?;
    assert_eq!(report.cause, Cause::DeadlineExceeded);
    assert_eq!(report.status, Some(Termination::Signal(15)));
    let sent: Vec<_> = report.steps.iter().map(|s| (s.target, s.signal)).collect();
    assert_eq!(sent, vec![(Target::Group, Signal::Terminate)]);
    assert!(report.steps[0].at - started >= Duration::from_millis(300));
    assert!(report.group_reaped);
    Ok(())
}

#[tokio::test]
async fn a_descendant_that_left_the_group_cannot_hold_up_the_exit() -> R {
    // `setsid` puts the sleeper in its own session; it keeps stdout open after the group is gone. The leader waits
    // until the sleeper has left its group: before that, the group kill at the leader's exit rightly takes it too.
    let config = Config {
        drain_grace: Duration::from_millis(300),
        ..Config::default()
    };
    let script = "setsid sleep 600 & pid=$!; \
                  while [ \"$(cut -d' ' -f5 /proc/$pid/stat)\" = \"$$\" ]; do sleep 0.01; done; echo \"$pid\"; exit 0";
    let mut p = Process::spawn(sh(script), config)?;
    let escaped: u32 = first_line(&mut p).await?.parse()?;
    let started = Instant::now();
    let outcome = until_exit(&mut p, Duration::from_secs(10)).await;
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(escaped as i32).ok_or("bad pid")?,
        rustix::process::Signal::KILL,
    );
    let (_, report) = outcome?;
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(report.status, Some(Termination::Code(0)));
    assert!(report.group_reaped);
    assert!(!report.stdout.eof, "stdout should have been abandoned");
    Ok(())
}

#[tokio::test]
async fn dropping_the_process_kills_its_group() -> R {
    let config = Config {
        escalation: Escalation {
            native: None,
            native_grace: Duration::ZERO,
            term_grace: Duration::from_millis(200),
            kill_grace: Duration::from_secs(2),
        },
        ..Config::default()
    };
    let mut p = Process::spawn(sh("trap '' TERM; sleep 600 & echo \"$!\"; wait"), config)?;
    let pgid = p.pid();
    let grandchild: u32 = first_line(&mut p).await?.parse()?;
    drop(p);
    let until = Instant::now() + Duration::from_secs(5);
    while (alive(grandchild) || !group_members(pgid).is_empty()) && Instant::now() < until {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive(grandchild), "grandchild {grandchild} survived");
    assert!(!alive(pgid), "leader {pgid} was not reaped");
    Ok(())
}

#[test]
fn shutting_down_the_runtime_kills_the_group() -> R {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (pgid, grandchild) = runtime.block_on(async {
        let mut p = Process::spawn(sh("sleep 600 & echo \"$!\"; wait"), Config::default())?;
        let grandchild: u32 = first_line(&mut p).await?.parse()?;
        // Keep the process alive past block_on: only the runtime going away ends it.
        let pgid = p.pid();
        std::mem::forget(p);
        Ok::<_, Box<dyn std::error::Error>>((pgid, grandchild))
    })?;
    drop(runtime);
    let until = Instant::now() + Duration::from_secs(5);
    while alive(grandchild) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive(grandchild), "grandchild {grandchild} survived");
    assert!(!alive(pgid), "leader {pgid} was not reaped");
    Ok(())
}

#[test]
fn a_cancel_requested_before_the_exit_is_seen_is_reported_as_cancelled() -> R {
    // M1T02R-01: the cancel is requested while the child runs; the supervisor only gets to run after the child
    // exited, so it sees the exit first. The request must still decide the outcome.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(async {
        let mut p = Process::spawn(sh("sleep 0.1; exit 0"), Config::default())?;
        p.cancel(Escalation {
            native: Some(Signal::Interrupt),
            ..Escalation::default()
        });
        // Block the only runtime thread until the child has exited (a zombie), so the supervisor has not run yet.
        let stat = format!("/proc/{}/stat", p.pid());
        let until = Instant::now() + Duration::from_secs(10);
        while Instant::now() < until {
            let state = std::fs::read_to_string(&stat)?;
            if state[state.rfind(')').ok_or("bad stat")? + 1..]
                .trim_start()
                .starts_with('Z')
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let (_, report) = until_exit(&mut p, Duration::from_secs(10)).await?;
        Ok::<_, Box<dyn std::error::Error>>(report)
    })?;
    assert_eq!(report.cause, Cause::Cancelled);
    assert_eq!(report.status, Some(Termination::Code(0)));
    assert!(report.group_reaped);
    Ok(())
}

#[tokio::test]
async fn the_first_cancel_wins_and_repeats_are_dropped() -> R {
    // M1T02R-03: repeated cancels from any clone keep the first plan and queue nothing.
    let mut p = Process::spawn(
        sh("trap '' INT; echo up; while :; do sleep 1; done"),
        Config::default(),
    )?;
    first_line(&mut p).await?;
    let first = Escalation {
        native: Some(Signal::Interrupt),
        native_grace: Duration::from_millis(100),
        term_grace: Duration::from_secs(2),
        kill_grace: Duration::from_secs(2),
    };
    let control = p.control();
    control.cancel(first);
    let later = Escalation {
        native: None,
        native_grace: Duration::ZERO,
        term_grace: Duration::ZERO,
        kill_grace: Duration::from_secs(2),
    };
    for i in 0..100_000 {
        if i % 2 == 0 {
            p.cancel(later)
        } else {
            control.clone().cancel(later)
        }
    }
    let (_, report) = until_exit(&mut p, Duration::from_secs(10)).await?;
    let sent: Vec<_> = report.steps.iter().map(|s| (s.target, s.signal)).collect();
    assert_eq!(
        sent,
        vec![
            (Target::Leader, Signal::Interrupt),
            (Target::Group, Signal::Terminate)
        ]
    );
    assert_eq!(report.cause, Cause::Cancelled);
    assert_eq!(report.status, Some(Termination::Signal(15)));
    Ok(())
}
