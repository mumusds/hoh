//! A pseudo-terminal for running `hoh` as a user would (test code). The master side is read on a thread into a
//! buffer; the test keeps the slave open so that `stty -g` can read the terminal's attributes before and after.
//! `setsid --ctty` makes the PTY the controlling terminal of the spawned `hoh` (same pid: a child of the test is not
//! a process group leader, so setsid does not fork).

#![allow(dead_code)] // each test binary uses a part of it

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fd::OwnedFd;
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Action, Winsize, tcflow, tcsetwinsize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub struct Pty {
    /// `None` once the terminal was hung up.
    master: Option<std::fs::File>,
    /// `None` once dropped: the reader then ends with EIO when the program's copies are closed too.
    slave: Option<OwnedFd>,
    output: Arc<Mutex<Vec<u8>>>,
    /// Asks the reader to close its copy of the master.
    stop: Arc<AtomicBool>,
    /// How many of the reader's polls found nothing to read (see [`Pty::drained`]).
    quiet: Arc<std::sync::atomic::AtomicU64>,
    reader: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Pty {
    /// Close the slave and collect the reader (it ends once no slave is open; given up after 2 s).
    fn drop(&mut self) {
        self.slave = None;
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let started = Instant::now();
            while !reader.is_finished() && started.elapsed() < Duration::from_secs(2) {
                std::thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

/// A started `hoh`: killed and reaped when dropped if it is still running, so a test that fails early leaves no
/// process behind (M1T08R-04).
pub struct Hoh(pub Child);

impl Drop for Hoh {
    fn drop(&mut self) {
        if let Ok(None) = self.0.try_wait() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

impl Pty {
    pub fn new(rows: u16, cols: u16) -> R<Pty> {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
        grantpt(&master)?;
        unlockpt(&master)?;
        let name = ptsname(&master, Vec::new())?;
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        tcsetwinsize(
            &slave,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        let master = std::fs::File::from(master);
        let mut reader = master.try_clone()?;
        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = output.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let quiet = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let quieted = quiet.clone();
        let reader = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            let tick = Timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            };
            // Ends with EIO once every slave descriptor is closed, or when asked to stop: its copy of the master
            // must close too for the terminal to hang up.
            while !stopped.load(Ordering::SeqCst) {
                let mut fds = [PollFd::new(&reader, PollFlags::IN)];
                match rustix::event::poll(&mut fds, Some(&tick)) {
                    Ok(0) => {
                        quieted.fetch_add(1, Ordering::SeqCst);
                        continue;
                    }
                    Err(rustix::io::Errno::INTR) => continue,
                    Ok(_) => {}
                    Err(_) => break,
                }
                match reader.read(&mut buf) {
                    Ok(n @ 1..) => {
                        if let Ok(mut out) = sink.lock() {
                            out.extend_from_slice(&buf[..n]);
                        }
                    }
                    _ => break,
                }
            }
        });
        Ok(Pty {
            master: Some(master),
            slave: Some(slave),
            output,
            stop,
            quiet,
            reader: Some(reader),
        })
    }

    /// Wait (at most `timeout`) until everything written to the terminal before this call is in [`Pty::output`]: the
    /// reader appends what it reads on its own thread, so right after a program exits its last bytes can still be
    /// unread (two TUI tests failed so under load: "the alternate screen was not left"; ADR-022 decision 10, v2). Once
    /// nothing more is written (its writer gone), a poll that starts after this call and finds nothing to read shows
    /// that all of it was read and appended (the reader appends each read before it polls again); the second quiet
    /// poll counted from here is one that started after this call. A reader that has ended has read all it will.
    pub fn drained(&self, timeout: Duration) -> R {
        let from = self.quiet.load(Ordering::SeqCst);
        let end = Instant::now() + timeout;
        while self.quiet.load(Ordering::SeqCst) < from + 2
            && !self
                .reader
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
        {
            if Instant::now() >= end {
                return Err(
                    format!("the terminal's output was not read within {timeout:?}").into(),
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(())
    }

    /// Hang the terminal up as closing its window does: every descriptor of the master is closed (the reader's
    /// copy first), so the kernel hangs up the slave and sends SIGHUP to the session it is the controlling terminal
    /// of. The test's own slave stays open, hung up.
    pub fn hang_up(&mut self) -> R {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            reader.join().map_err(|_| "the PTY reader panicked")?;
        }
        self.master = None;
        Ok(())
    }

    /// The slave side, for a child's stdin, stdout or stderr.
    pub fn stdio(&self) -> R<Stdio> {
        let slave = self.slave.as_ref().ok_or("the PTY is closed")?;
        Ok(Stdio::from(slave.try_clone()?))
    }

    /// `stty -g` of the terminal.
    pub fn stty(&self) -> R<String> {
        let out = Command::new("stty")
            .arg("-g")
            .stdin(self.stdio()?)
            .output()?;
        if !out.status.success() {
            return Err(format!("stty -g: {}", String::from_utf8_lossy(&out.stderr)).into());
        }
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    /// What is waiting to be read on the terminal (typed and not read by any program), within `timeout`.
    pub fn unread_input(&self, timeout: Duration) -> R<Vec<u8>> {
        let slave = self.slave.as_ref().ok_or("the PTY is closed")?;
        let wait = Timespec {
            tv_sec: i64::try_from(timeout.as_secs())?,
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let mut fds = [PollFd::new(slave, PollFlags::IN)];
        if rustix::event::poll(&mut fds, Some(&wait))? == 0 {
            return Ok(Vec::new());
        }
        let mut buf = [0u8; 4096];
        let n = rustix::io::read(slave, &mut buf)?;
        Ok(buf[..n].to_vec())
    }

    /// The terminal's foreground process group, asked of its master (on the slave, tcgetpgrp(3) answers only the
    /// processes whose controlling terminal it is).
    pub fn foreground(&self) -> R<u32> {
        let master = self.master.as_ref().ok_or("the terminal was hung up")?;
        Ok(u32::try_from(
            rustix::termios::tcgetpgrp(master)?.as_raw_pid(),
        )?)
    }

    /// Resize the terminal as its window would be: the kernel then sends SIGWINCH to its foreground process group.
    pub fn resize(&self, rows: u16, cols: u16) -> R {
        let slave = self.slave.as_ref().ok_or("the PTY is closed")?;
        tcsetwinsize(
            slave,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        Ok(())
    }

    /// Stop or restart the terminal's output, as Ctrl+S and Ctrl+Q or a stalled link do: a program writing to it
    /// meanwhile is held in write(2).
    pub fn hold_output(&self, held: bool) -> R {
        let slave = self.slave.as_ref().ok_or("the PTY is closed")?;
        tcflow(slave, if held { Action::OOff } else { Action::OOn })?;
        Ok(())
    }

    /// Wait until the terminal's input queue holds `bytes` bytes typed and not yet read.
    fn wait_for_queued(&self, bytes: u64, timeout: Duration) -> R {
        let slave = self.slave.as_ref().ok_or("the PTY is closed")?;
        let end = Instant::now() + timeout;
        loop {
            let queued = rustix::io::ioctl_fionread(slave)?;
            if queued == bytes {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(
                    format!("{queued} bytes queued, not {bytes}, after {timeout:?}").into(),
                );
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Type `bytes` and wait until `hoh` has read them. A write to the master reaches the input queue later, so an
    /// empty queue right after it proves nothing (M1T08R-33): `hoh` is stopped while they are typed, they are seen
    /// waiting in the queue, and once it continues they are seen gone; nothing else reads this terminal. Anything typed
    /// before must have been read already. Under a tracer (`strace -f`) a stopped process shows `t`, not `T`, and is
    /// not taken for stopped: the handshake then fails, so run the PTY tests untraced (M1T08R-37).
    pub fn send_and_wait_until_read(&mut self, hoh: &Hoh, bytes: &[u8], timeout: Duration) -> R {
        signal(hoh, rustix::process::Signal::STOP)?;
        let continued = (|| {
            wait_for_state(hoh, 'T', timeout)?;
            self.send(bytes)?;
            self.wait_for_queued(u64::try_from(bytes.len())?, timeout)
        })();
        signal(hoh, rustix::process::Signal::CONT)?;
        continued?;
        self.wait_for_queued(0, timeout)
    }

    /// Type `bytes` as the user would.
    pub fn send(&mut self, bytes: &[u8]) -> R {
        let master = self.master.as_mut().ok_or("the terminal was hung up")?;
        master.write_all(bytes)?;
        master.flush()?;
        Ok(())
    }

    /// Everything the program wrote so far.
    pub fn output(&self) -> Vec<u8> {
        self.output.lock().map(|o| o.clone()).unwrap_or_default()
    }

    /// Wait until `text` has been drawn after byte `from` of the output (escape sequences and whitespace ignored, as
    /// a full-screen program skips unchanged cells); the output length at that point.
    pub fn wait_for(&self, text: &str, from: usize, timeout: Duration) -> R<usize> {
        let want = squash(text.as_bytes());
        let started = Instant::now();
        loop {
            let out = self.output();
            if squash(&out[from.min(out.len())..]).contains(&want) {
                return Ok(out.len());
            }
            if started.elapsed() > timeout {
                return Err(format!(
                    "{text:?} not drawn within {timeout:?}; screen text: {:?}",
                    squash(&out[from.min(out.len())..])
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

/// Text without escape sequences (CSI, OSC and two-byte ones) and whitespace.
pub fn squash(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_whitespace() && !c.is_control() {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A snapshot helper: the screen a terminal of the given rows and columns shows after the bytes.
pub type Screen = dyn Fn(&[u8], u16, u16) -> R<String>;

/// What a terminal of `rows` x `cols` shows after `bytes`, as [`screen`] gives it, once [`erase_check`] found that
/// [`screen`] shows nothing that was erased or overwritten (docs/adr/ADR-031.md decision 14): a snapshot helper that
/// fails its counter-example is refused, never used, and there is no fallback to [`squash`].
pub fn snapshot(bytes: &[u8], rows: u16, cols: u16) -> R<String> {
    erase_check(&screen)?;
    screen(bytes, rows, cols)
}

/// Wait (at most `timeout`) until the screen of `pty` (a terminal of `rows` x `cols`) as [`snapshot`] gives it for all
/// its output so far satisfies `want`; that screen.
pub fn screen_until(
    pty: &Pty,
    rows: u16,
    cols: u16,
    timeout: Duration,
    want: impl Fn(&str) -> bool,
) -> R<String> {
    let end = Instant::now() + timeout;
    loop {
        let screen = snapshot(&pty.output(), rows, cols)?;
        if want(&screen) {
            return Ok(screen);
        }
        if Instant::now() > end {
            return Err(
                format!("the screen was not as wanted within {timeout:?}:\n{screen}").into(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The screen a terminal of `rows` x `cols` shows after `bytes`: each row's text without its trailing blanks, the rows
/// joined by LF, trailing empty rows dropped.
pub fn screen(bytes: &[u8], rows: u16, cols: u16) -> R<String> {
    let size = |n: u16| {
        std::num::NonZeroU16::new(n).ok_or("a terminal has at least one row and one column")
    };
    let mut parser = vt100::Parser::new(size(rows)?, size(cols)?, 0);
    parser.process(bytes);
    let rows: Vec<String> = parser
        .screen()
        .contents()
        .split('\n')
        .map(|row| row.trim_end().to_owned())
        .collect();
    let used = rows
        .iter()
        .rposition(|r| !r.is_empty())
        .map_or(0, |i| i + 1);
    Ok(rows[..used].join("\n"))
}

/// The erase counter-examples (docs/adr/ADR-031.md decision 14) `screen` must pass: a project name written and the
/// screen then cleared (`ESC[2J`), a line written and then erased (`ESC[2K`), a word overwritten in place after a
/// carriage return and the rest of its line erased (`ESC[K`), and a run id drawn on the alternate screen that was
/// left. None of what was erased may show, and what was written after it must.
pub fn erase_check(screen: &Screen) -> R {
    let cases: [(&[u8], &[&str], &[&str]); 4] = [
        (
            b"project-erased\r\n\x1b[2J\x1b[Hshown",
            &["project-erased"],
            &["shown"],
        ),
        (b"line-erased\x1b[2K\rkept", &["line-erased"], &["kept"]),
        (
            b"PAUSED-overwritten\rRUNNING\x1b[K",
            &["PAUSED", "overwritten"],
            &["RUNNING"],
        ),
        (
            b"main\x1b[?1049h\x1b[Hrun-on-alternate\x1b[?1049l",
            &["run-on-alternate"],
            &["main"],
        ),
    ];
    for (bytes, gone, shown) in cases {
        let text = screen(bytes, 24, 80)?;
        if let Some(left) = gone.iter().find(|g| text.contains(**g)) {
            return Err(format!(
                "the snapshot helper shows {left:?}, which {:?} erased or overwrote: {text:?}",
                String::from_utf8_lossy(bytes)
            )
            .into());
        }
        if let Some(missing) = shown.iter().find(|s| !text.contains(**s)) {
            return Err(format!(
                "the snapshot helper does not show {missing:?}, which {:?} left on the screen: {text:?}",
                String::from_utf8_lossy(bytes)
            )
            .into());
        }
    }
    Ok(())
}

/// `hoh args…` in `project` with the PTY as its controlling terminal and stdio, in an environment isolated in `home`
/// ([`isolate`]).
pub fn spawn_in(
    pty: &Pty,
    project: &Path,
    home: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> R<Hoh> {
    spawn(pty, true, project, home, args, env)
}

/// As [`spawn_in`], but in a session of its own without a controlling terminal: the PTY is only its stdio, so a
/// hang-up sends it no SIGHUP.
pub fn spawn_without_ctty_in(pty: &Pty, project: &Path, home: &Path) -> R<Hoh> {
    spawn(pty, false, project, home, &[], &[])
}

/// What a stand-in agent answers to every command (attach.rs's fake agents): a version, and a help that offers nothing
/// HoH drives an agent by, so a host's registry finds it unsupported for good and never runs it again.
const STAND_IN: &str = "#!/bin/sh\necho 'stand-in agent 0.0.0 (hoh PTY tests)'\n";

/// `command`'s environment cleared and isolated in `home`: HOME `home`; XDG_RUNTIME_DIR `<home>/run` (0700, where no
/// user manager answers, so a host is started directly); PATH `<home>/agents` (stand-ins for codex, claude and pi, so
/// no agent installed on this machine is ever run, by `hoh` or by a host it starts) then /usr/bin:/bin.
pub fn isolate(command: &mut Command, home: &Path) -> R {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let (agents, runtime) = (home.join("agents"), home.join("run"));
    std::fs::create_dir_all(&agents)?;
    std::fs::create_dir_all(&runtime)?;
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700))?;
    // Written once: a stand-in's file never changes after (the stamps a host's look compares).
    for agent in ["codex", "claude", "pi"] {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o755)
            .open(agents.join(agent))
        {
            Ok(mut file) => file.write_all(STAND_IN.as_bytes())?,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let mut path = agents.into_os_string();
    path.push(":/usr/bin:/bin");
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_RUNTIME_DIR", runtime)
        .env("PATH", path);
    Ok(())
}

/// The longest Unix socket path (`sockaddr_un`), as the host checks it before it binds.
const SOCKET_PATH_MAX: usize = 107;

/// `project`'s host socket `.hoh/host.sock` fits [`SOCKET_PATH_MAX`]; else the host's own words for it, before anything
/// starts (M3C-01: with a long TMPDIR the host refuses to start and the test sees only a Workspace that never connects).
pub fn socket_fits(project: &Path) -> R {
    let socket = project.join(".hoh/host.sock");
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(format!(
            "the socket path {} is longer than {SOCKET_PATH_MAX} bytes (a shorter TMPDIR is needed)",
            socket.display()
        )
        .into());
    }
    Ok(())
}

fn spawn(
    pty: &Pty,
    ctty: bool,
    project: &Path,
    home: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> R<Hoh> {
    socket_fits(project)?;
    let mut cmd = Command::new("setsid");
    isolate(&mut cmd, home)?;
    if ctty {
        cmd.arg("--ctty");
    }
    cmd.arg(env!("CARGO_BIN_EXE_hoh"))
        .args(args)
        .current_dir(project)
        .env("TERM", "xterm-256color")
        .stdin(pty.stdio()?)
        .stdout(pty.stdio()?)
        .stderr(pty.stdio()?);
    for (k, v) in env {
        cmd.env(k, v);
    }
    Ok(Hoh(cmd.spawn()?))
}

/// The exit status of `child` within `timeout`; killed and an error otherwise.
pub fn exit_within(Hoh(child): &mut Hoh, timeout: Duration) -> R<ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("hoh did not exit within {timeout:?}").into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Wait until `hoh` is in `state` (`/proc/<pid>/stat`: `T` stopped).
fn wait_for_state(Hoh(child): &Hoh, state: char, timeout: Duration) -> R {
    let path = format!("/proc/{}/stat", child.id());
    let end = Instant::now() + timeout;
    loop {
        let stat = std::fs::read_to_string(&path)?;
        // The state follows the command name, which is in parentheses and may hold anything.
        let now = stat
            .rsplit_once(')')
            .and_then(|(_, rest)| rest.trim_start().chars().next());
        if now == Some(state) {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!("hoh not in state {state} within {timeout:?} ({now:?})").into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Wait until `hoh` (single-threaded) is inside write(2), as its `/proc/<pid>/syscall` says.
pub fn in_write(Hoh(child): &Hoh, timeout: Duration) -> R {
    #[cfg(target_arch = "x86_64")]
    const WRITE: &str = "1";
    #[cfg(target_arch = "aarch64")]
    const WRITE: &str = "64";
    let path = format!("/proc/{}/syscall", child.id());
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if std::fs::read_to_string(&path)?.split(' ').next() == Some(WRITE) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("hoh was not seen in write(2) within {timeout:?}").into())
}

/// Send `signal` to the process `child`.
pub fn signal(Hoh(child): &Hoh, signal: rustix::process::Signal) -> R {
    let pid = rustix::process::Pid::from_child(child);
    rustix::process::kill_process(pid, signal)?;
    Ok(())
}

/// Processes other than this one whose working directory is `dir` or below it.
///
/// A listing of /proc that fails is an error, not "none" (docs/adr/ADR-022.md decision 10, v2; CIR-004). A process
/// whose working directory cannot be read has gone or is another user's, so it is none of this test's.
pub fn processes_in(dir: &Path) -> R<Vec<u32>> {
    let me = std::process::id();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let Some(pid) = entry?
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if pid != me
            && std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir))
        {
            found.push(pid);
        }
    }
    Ok(found)
}

/// Every path under `dir` with its contents, sorted: equal before and after means nothing was written.
pub fn tree(dir: &Path) -> R<Vec<(PathBuf, Vec<u8>)>> {
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

/// The position of `needle` in `hay` at or after `from`.
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    hay.get(from..)?
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|i| i + from)
}
