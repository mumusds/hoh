//! Attempts' cgroups, the termination proof when the host runs in a delegated cgroup v2 (docs/adr/ADR-021.md decision
//! 8d v5, CR-011): each attempt's leader is moved into a cgroup of its own before its start gate opens, so everything
//! it starts stays there whatever it does (fork, setsid, threads, its environment); `cgroup.kill` ends them all at
//! once and `populated 0` in `cgroup.events` proves none is left.

use super::processes::Proof;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Where cgroup v2 is mounted.
const MOUNT: &str = "/sys/fs/cgroup";

/// How often a stop looks again.
const LOOK_EVERY: Duration = Duration::from_millis(10);

/// The host's own cgroup, delegated: it may make cgroups below it and move its children into them.
#[derive(Debug, Clone)]
pub(super) struct Delegated {
    base: PathBuf,
}

impl Delegated {
    /// The cgroup this process is in, if it is delegated to it: cgroup v2 (`0::<path>`), and a cgroup can be made
    /// below it and removed again.
    pub(super) fn detect() -> Option<Delegated> {
        let text = std::fs::read_to_string("/proc/self/cgroup").ok()?;
        let path = text.lines().find_map(|line| line.strip_prefix("0::"))?;
        let base = Path::new(MOUNT).join(path.trim().trim_start_matches('/'));
        let probe = base.join(format!("hoh-probe-{}", std::process::id()));
        std::fs::create_dir(&probe).ok()?;
        std::fs::remove_dir(&probe).ok()?;
        Some(Delegated { base })
    }

    /// The cgroup of the attempt with `marker` (`<project id>/<attempt id>`): a name of hex digits, unique per
    /// project and attempt.
    pub(super) fn attempt(&self, marker: &str) -> PathBuf {
        let digest: String = Sha256::digest(marker.as_bytes())
            .iter()
            .take(8)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.base.join(format!("attempt-{digest}"))
    }
}

/// One attempt's cgroup.
#[derive(Debug, Clone)]
pub(super) struct AttemptGroup {
    path: PathBuf,
}

impl AttemptGroup {
    /// The cgroup at `path` (made by this host or an earlier one).
    pub(super) fn at(path: &Path) -> AttemptGroup {
        AttemptGroup {
            path: path.to_owned(),
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Make the cgroup at `path` and move `pid` (the leader, held at its start gate) into it.
    pub(super) fn adopt(path: &Path, pid: u32) -> std::io::Result<AttemptGroup> {
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        std::fs::write(path.join("cgroup.procs"), pid.to_string())?;
        Ok(AttemptGroup::at(path))
    }

    /// Whether any process is in it: `Ok(false)` once it is empty or gone; an error when that cannot be read.
    pub(super) fn populated(&self) -> Result<bool, String> {
        match std::fs::read_to_string(self.path.join("cgroup.events")) {
            Ok(events) => events
                .lines()
                .find_map(|line| line.strip_prefix("populated "))
                .map(|v| v.trim() == "1")
                .ok_or_else(|| format!("{}: no populated line", self.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", self.path.display())),
        }
    }

    /// SIGTERM to each process in it, each pinned by a pidfd and checked to be in this cgroup still (a pid that ended
    /// and was reused is never signalled); whether any got it.
    pub(super) fn terminate(&self) -> bool {
        let Ok(procs) = std::fs::read_to_string(self.path.join("cgroup.procs")) else {
            return false;
        };
        let wanted = self.relative();
        let mut sent = false;
        for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
            let Some(raw) = rustix::process::Pid::from_raw(pid) else {
                continue;
            };
            let Ok(pidfd) = rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty())
            else {
                continue;
            };
            let here = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).is_ok_and(|c| {
                c.lines()
                    .any(|l| l.strip_prefix("0::").map(str::trim) == Some(wanted.as_str()))
            });
            if here {
                sent |= rustix::process::pidfd_send_signal(&pidfd, rustix::process::Signal::TERM)
                    .is_ok();
            }
        }
        sent
    }

    /// SIGKILL to every process in it at once, by the kernel (`cgroup.kill`); whether it was asked.
    pub(super) fn kill(&self) -> bool {
        std::fs::write(self.path.join("cgroup.kill"), "1").is_ok()
    }

    /// Remove it once it is empty (a leftover empty directory is harmless).
    pub(super) fn remove(&self) {
        let _ = std::fs::remove_dir(&self.path);
    }

    /// Its path as `/proc/<pid>/cgroup` names it.
    fn relative(&self) -> String {
        let rel = self.path.strip_prefix(MOUNT).unwrap_or(&self.path);
        format!("/{}", rel.display())
    }

    /// Where the proof stands: none left (Done), some left (Pending), or not readable (Incomplete).
    pub(super) fn proof(&self) -> Proof {
        match self.populated() {
            Ok(false) => Proof::Done,
            Ok(true) => Proof::Pending,
            Err(_) => Proof::Incomplete,
        }
    }

    /// Stop what is left in it as a group is stopped (SIGTERM, `term_grace`, `cgroup.kill`, `kill_grace`), the
    /// attempt's group already stopped or gone (blocking): the steps taken and where the proof ended. It is removed
    /// once proved empty.
    pub(super) fn stop(&self, term_grace: Duration, kill_grace: Duration) -> (Vec<String>, Proof) {
        let mut steps = Vec::new();
        let mut proof = self.proof();
        if proof == Proof::Pending {
            if self.terminate() {
                steps.push("cgroup_sigterm".to_owned());
            }
            proof = self.wait(term_grace);
        }
        if proof == Proof::Pending {
            if self.kill() {
                steps.push("cgroup_kill".to_owned());
            }
            proof = self.wait(kill_grace);
        }
        match proof {
            Proof::Done => {
                if !steps.is_empty() {
                    steps.push("cgroup_reaped".to_owned());
                }
                self.remove();
            }
            Proof::Incomplete => steps.push("cgroup_unreadable".to_owned()),
            Proof::Pending => {}
        }
        (steps, proof)
    }

    /// Look until it is empty, unreadable, or `grace` passed.
    fn wait(&self, grace: Duration) -> Proof {
        let end = Instant::now() + grace;
        loop {
            let proof = self.proof();
            if proof != Proof::Pending || Instant::now() >= end {
                return proof;
            }
            std::thread::sleep(LOOK_EVERY);
        }
    }
}

/// How an attempt's processes are tracked (decision 8d v5): by its cgroup, or (degraded) by the /proc scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tracking {
    Cgroup(PathBuf),
    /// Why there is no cgroup.
    Proc(String),
}

impl Tracking {
    /// As recorded in its `attempt.tracking` event.
    pub(super) fn json(&self, attempt_id: &str) -> serde_json::Value {
        match self {
            Tracking::Cgroup(path) => serde_json::json!({
                "attempt_id": attempt_id, "tracking": "cgroup", "cgroup": path.to_string_lossy(),
            }),
            Tracking::Proc(reason) => serde_json::json!({
                "attempt_id": attempt_id, "tracking": "proc", "reason": reason,
            }),
        }
    }

    /// From its `attempt.tracking` event: none recorded (an attempt of an earlier host version, or one whose start
    /// recorded none) is tracked by the /proc scan.
    pub(super) fn of(event: Option<&serde_json::Value>) -> Tracking {
        match event {
            Some(e) if e["tracking"] == "cgroup" => e["cgroup"].as_str().map_or_else(
                || Tracking::Proc("no cgroup path recorded".to_owned()),
                |p| Tracking::Cgroup(PathBuf::from(p)),
            ),
            Some(e) => Tracking::Proc(e["reason"].as_str().unwrap_or("recorded").to_owned()),
            None => Tracking::Proc("no tracking recorded".to_owned()),
        }
    }

    /// Its cgroup, if it has one.
    pub(super) fn cgroup(&self) -> Option<AttemptGroup> {
        match self {
            Tracking::Cgroup(path) => Some(AttemptGroup::at(path)),
            Tracking::Proc(_) => None,
        }
    }
}
