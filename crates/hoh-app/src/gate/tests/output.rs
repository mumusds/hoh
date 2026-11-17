//! A helper's bounded wait for its child's output. The same bytes are in two files, `tests/common/output.rs` for the
//! integration tests and `src/gate/tests/output.rs` for the crate's unit tests, and a unit test keeps them so: an
//! integration test reaches only the crate's public interface, and the persistence scan (AC-062-05) refuses a
//! `#[path]` or `include!` in `src/` (CIRF-53).

use std::os::fd::AsFd as _;
use std::time::{Duration, Instant};

/// How long its pipes may stay open once the child ended: a process it left may hold them.
const HELD_AFTER_END: Duration = Duration::from_secs(5);

/// `child`'s output once it has ended and its pipes closed, waited for at most `limit`; its pipes may stay open
/// [`HELD_AFTER_END`] more after its end. Its stdout and stderr are read as they come on this thread, the pipes
/// non-blocking and polled (no reader thread), so a full pipe cannot hold it up and nothing is left reading when this
/// returns: the read ends are closed on every path. Past either bound the child, this test's own and not yet reaped, is
/// killed and reaped, and that is an error naming what it wrote to stderr so far. A helper's wait has a bound of its
/// own, so a hung child fails its test instead of holding it until the suite's bound (ADR-022 decision 10; CIR-502,
/// CIRF-04).
pub fn output_within(
    mut child: std::process::Child,
    limit: Duration,
) -> Result<std::process::Output, Box<dyn std::error::Error>> {
    let (mut out, mut err) = (child.stdout.take(), child.stderr.take());
    if let Some(pipe) = &out {
        rustix::io::ioctl_fionbio(pipe.as_fd(), true)?;
    }
    if let Some(pipe) = &err {
        rustix::io::ioctl_fionbio(pipe.as_fd(), true)?;
    }
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut end = Instant::now() + limit;
    let mut status = None;
    // When it ended, and whether the bound in force is then the time its pipes may stay open after that.
    let mut ended: Option<(Instant, bool)> = None;
    loop {
        drain(&mut out, &mut stdout)?;
        drain(&mut err, &mut stderr)?;
        if status.is_none() {
            status = child.try_wait()?;
            if status.is_some() {
                let now = Instant::now();
                ended = Some((now, now + HELD_AFTER_END < end));
                end = end.min(now + HELD_AFTER_END);
            }
        }
        if let Some(status) = status
            && out.is_none()
            && err.is_none()
        {
            return Ok(std::process::Output {
                status,
                stdout,
                stderr,
            });
        }
        let now = Instant::now();
        if now >= end {
            let _ = child.kill();
            let _ = child.wait();
            // The bound that ran out, and how long the output was held open after the end (CIRF-100).
            let what = match ended {
                Some((at, true)) => format!(
                    "its output was still held open {:?} after it ended, {HELD_AFTER_END:?} allowed",
                    at.elapsed()
                ),
                Some((at, false)) => format!(
                    "its output was still held open {:?} after it ended, when its limit of {limit:?} ran out",
                    at.elapsed()
                ),
                None => format!("still running after {limit:?}, so killed"),
            };
            return Err(format!("{what}; its stderr: {}", String::from_utf8_lossy(&stderr)).into());
        }
        let wait = rustix::event::Timespec::try_from((end - now).min(Duration::from_millis(10)))?;
        let mut fds = Vec::new();
        if let Some(pipe) = &out {
            fds.push(rustix::event::PollFd::new(
                pipe,
                rustix::event::PollFlags::IN,
            ));
        }
        if let Some(pipe) = &err {
            fds.push(rustix::event::PollFd::new(
                pipe,
                rustix::event::PollFlags::IN,
            ));
        }
        if fds.is_empty() {
            std::thread::sleep(Duration::from_millis(10));
        } else {
            match rustix::event::poll(&mut fds, Some(&wait)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Read what `pipe` holds now into `into`, without waiting; at its end of file it is closed (`None`).
fn drain(pipe: &mut Option<impl std::io::Read>, into: &mut Vec<u8>) -> std::io::Result<()> {
    let Some(reading) = pipe.as_mut() else {
        return Ok(());
    };
    let mut buffer = [0u8; 8192];
    loop {
        match reading.read(&mut buffer) {
            Ok(0) => {
                *pipe = None;
                return Ok(());
            }
            Ok(n) => into.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}
