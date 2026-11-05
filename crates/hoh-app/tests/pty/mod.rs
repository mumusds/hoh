//! A pseudo-terminal for running `hoh` as a user would (test code). The master side is read on a thread into a
//! buffer; the test keeps the slave open so that `stty -g` can read the terminal's attributes before and after.
//! `setsid --ctty` makes the PTY the controlling terminal of the spawned `hoh` (same pid: a child of the test is not
//! a process group leader, so setsid does not fork).

#![allow(dead_code)] // each test binary uses a part of it

use rustix::fd::OwnedFd;
use rustix::fs::{Mode, OFlags};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub struct Pty {
    master: std::fs::File,
    slave: OwnedFd,
    output: Arc<Mutex<Vec<u8>>>,
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
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            // Ends with EIO once every slave descriptor is closed.
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                if let Ok(mut out) = sink.lock() {
                    out.extend_from_slice(&buf[..n]);
                }
            }
        });
        Ok(Pty {
            master,
            slave,
            output,
        })
    }

    /// The slave side, for a child's stdin, stdout or stderr.
    pub fn stdio(&self) -> R<Stdio> {
        Ok(Stdio::from(self.slave.try_clone()?))
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
        self.master.write_all(bytes)?;
        self.master.flush()?;
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
) -> R<Child> {
    let mut cmd = Command::new("setsid");
    cmd.arg("--ctty")
        .arg(env!("CARGO_BIN_EXE_hoh"))
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
    Ok(cmd.spawn()?)
}

/// The exit status of `child` within `timeout`; killed and an error otherwise.
pub fn exit_within(child: &mut Child, timeout: Duration) -> R<ExitStatus> {
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
pub fn signal(child: &Child, signal: rustix::process::Signal) -> R {
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
