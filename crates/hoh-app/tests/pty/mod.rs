//! A pseudo-terminal for running `hoh` as a user would (test code). The master side is read on a thread into a
//! buffer; the test keeps the slave open so that `stty -g` can read the terminal's attributes before and after.
//! `setsid --ctty` makes the PTY the controlling terminal of the spawned `hoh` (same pid: a child of the test is not
//! a process group leader, so setsid does not fork).

#![allow(dead_code)] // each test binary uses a part of it

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fd::OwnedFd;
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
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
                    Ok(0) | Err(rustix::io::Errno::INTR) => continue,
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
            reader: Some(reader),
        })
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

/// `hoh args…` in `project` with the PTY as its controlling terminal and stdio, in a clean environment.
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

fn spawn(
    pty: &Pty,
    ctty: bool,
    project: &Path,
    home: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> R<Hoh> {
    let mut cmd = Command::new("setsid");
    if ctty {
        cmd.arg("--ctty");
    }
    cmd.arg(env!("CARGO_BIN_EXE_hoh"))
        .args(args)
        .current_dir(project)
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
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

/// Send `signal` to the process `child`.
pub fn signal(Hoh(child): &Hoh, signal: rustix::process::Signal) -> R {
    let pid = rustix::process::Pid::from_child(child);
    rustix::process::kill_process(pid, signal)?;
    Ok(())
}

/// Processes other than this one whose working directory is `dir` or below it.
pub fn processes_in(dir: &Path) -> Vec<u32> {
    let me = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != me)
        .filter(|pid| {
            std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(dir))
        })
        .collect()
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
