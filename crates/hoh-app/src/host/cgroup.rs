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
        // cgroup.kill (Linux 5.14): without it a member that ignores SIGTERM could not be stopped (M2T06R-457).
        let kills = probe.join("cgroup.kill").exists();
        std::fs::remove_dir(&probe).ok()?;
        kills.then_some(Delegated { base })
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

/// Whether a recorded path has the form this host gives an attempt's cgroup: below the cgroup v2 mount, named
/// `attempt-<16 hex digits>` (a record that is not is not trusted with a kill, M2T06R-458).
fn recorded_form(path: &Path) -> bool {
    path.starts_with(MOUNT)
        && path.is_absolute()
        && !path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("attempt-"))
            .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
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
            Some(e) if e["tracking"] == "cgroup" => e["cgroup"]
                .as_str()
                .filter(|p| recorded_form(Path::new(p)))
                .map_or_else(
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    /// A cgroup of a test's own (a directory with the files a cgroup has), `populated` as given, no members.
    fn fake(populated: u8) -> R<tempfile::TempDir> {
        let dir = tempfile::tempdir()?;
        std::fs::write(
            dir.path().join("cgroup.events"),
            format!("populated {populated}\nfrozen 0\n"),
        )?;
        std::fs::write(dir.path().join("cgroup.procs"), b"")?;
        std::fs::write(dir.path().join("cgroup.kill"), b"")?;
        Ok(dir)
    }

    #[test]
    fn a_cgroup_still_populated_after_its_kill_proves_nothing() -> R {
        // M2T06R-451: SIGKILL asked of the kernel, the grace waited, still populated: Pending, no reaped step, kept.
        let dir = fake(1)?;
        let group = AttemptGroup::at(dir.path());
        let (steps, proof) = group.stop(Duration::from_millis(30), Duration::from_millis(30));
        assert_eq!(
            (steps, proof),
            (vec!["cgroup_kill".to_owned()], Proof::Pending)
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cgroup.kill"))?,
            "1"
        );
        assert!(dir.path().exists());
        Ok(())
    }

    #[test]
    fn a_cgroup_whose_events_cannot_be_read_proves_nothing() -> R {
        // M2T06R-451: cgroup.events unreadable: Incomplete, said so among the steps.
        let dir = fake(0)?;
        std::fs::set_permissions(
            dir.path().join("cgroup.events"),
            std::fs::Permissions::from_mode(0o000),
        )?;
        let group = AttemptGroup::at(dir.path());
        assert_eq!(group.proof(), Proof::Incomplete);
        let (steps, proof) = group.stop(Duration::from_millis(30), Duration::from_millis(30));
        std::fs::set_permissions(
            dir.path().join("cgroup.events"),
            std::fs::Permissions::from_mode(0o600),
        )?;
        assert_eq!(
            (steps, proof),
            (vec!["cgroup_unreadable".to_owned()], Proof::Incomplete)
        );
        Ok(())
    }

    #[test]
    fn an_empty_cgroup_is_proved_at_once_and_a_gone_one_too() -> R {
        let dir = fake(0)?;
        assert_eq!(
            AttemptGroup::at(dir.path()).stop(Duration::ZERO, Duration::ZERO),
            (Vec::new(), Proof::Done)
        );
        assert_eq!(
            AttemptGroup::at(&dir.path().join("gone")).proof(),
            Proof::Done
        );
        Ok(())
    }

    #[test]
    fn only_a_recorded_path_of_the_hosts_form_is_trusted() {
        // M2T06R-458: below the cgroup mount, named attempt-<16 hex>; anything else is tracked by the /proc scan.
        let event = |path: &str| serde_json::json!({"tracking": "cgroup", "cgroup": path});
        let good = "/sys/fs/cgroup/user.slice/x.scope/attempt-0123456789abcdef";
        assert_eq!(
            Tracking::of(Some(&event(good))),
            Tracking::Cgroup(PathBuf::from(good))
        );
        for bad in [
            "/tmp/attempt-0123456789abcdef",
            "/sys/fs/cgroup/user.slice/x.scope",
            "/sys/fs/cgroup/../etc/attempt-0123456789abcdef",
            "/sys/fs/cgroup/x/attempt-0123456789abcdeg",
            "sys/fs/cgroup/x/attempt-0123456789abcdef",
        ] {
            assert!(
                matches!(Tracking::of(Some(&event(bad))), Tracking::Proc(_)),
                "{bad}"
            );
        }
    }
}
