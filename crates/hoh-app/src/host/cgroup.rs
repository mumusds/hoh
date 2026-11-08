//! Attempts' cgroups, part of the termination proof when the host runs in a delegated cgroup v2 (docs/adr/ADR-021.md
//! decision 8d v7, CR-011, CR-012): each attempt's leader is moved into a cgroup of its own before its start gate opens,
//! so what it starts stays there through fork, setsid, threads and changes of its environment, though a same-user
//! process can move itself out (M2T06R-400); `cgroup.kill` ends the subtree's members at once and `populated 0` in
//! `cgroup.events` proves none is left in it. The proof also needs the process group empty and the marker scan's
//! proof for what left (control.rs, reconcile.rs, runner.rs).

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
    /// below it and removed again; else why not (host.json records it, M2T06R-557).
    pub(super) fn detect() -> Result<Delegated, String> {
        let text = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|e| format!("/proc/self/cgroup cannot be read: {e}"))?;
        let path = text
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .ok_or("it is in no cgroup v2 hierarchy")?;
        let base = Path::new(MOUNT).join(path.trim().trim_start_matches('/'));
        let probe = base.join(format!("hoh-probe-{}", std::process::id()));
        std::fs::create_dir(&probe).map_err(|e| {
            format!(
                "its own cgroup {} is not delegated to it (no cgroup can be made below it: {e})",
                base.display()
            )
        })?;
        // cgroup.kill (Linux 5.14): without it a member that ignores SIGTERM could not be stopped (M2T06R-457).
        let kills = probe.join("cgroup.kill").exists();
        std::fs::remove_dir(&probe).map_err(|e| {
            format!(
                "its probe cgroup {} cannot be removed: {e}",
                probe.display()
            )
        })?;
        if kills {
            Ok(Delegated { base })
        } else {
            Err("its cgroups have no cgroup.kill (Linux 5.14 or later is needed)".to_owned())
        }
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

/// What an attempt's cgroup was when it was made, so a later look knows it sees that cgroup and not whatever is at its
/// path (a mount over it, another cgroup namespace's view, a directory made there later): the boot it was made in and
/// the device and inode of each directory from the mount's root down to it (decision 8d v7, M2T06R-900).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Witness {
    boot: String,
    chain: Vec<(u64, u64)>,
}

impl Witness {
    /// Of the directory `path` at or below `root`, now.
    fn of(root: &Path, path: &Path) -> std::io::Result<Witness> {
        use std::os::unix::fs::MetadataExt as _;
        let rel = path.strip_prefix(root).map_err(std::io::Error::other)?;
        let mut at = root.to_path_buf();
        let meta = std::fs::symlink_metadata(&at)?;
        let mut chain = vec![(meta.dev(), meta.ino())];
        for part in rel.components() {
            at.push(part);
            let meta = std::fs::symlink_metadata(&at)?;
            chain.push((meta.dev(), meta.ino()));
        }
        Ok(Witness {
            boot: hoh_agents::identity::boot_id()?,
            chain,
        })
    }
}

/// One attempt's cgroup.
#[derive(Debug, Clone)]
pub(super) struct AttemptGroup {
    path: PathBuf,
    /// Where its witness's chain starts: the cgroup mount (a test's own directory in tests).
    root: PathBuf,
    witness: Option<Witness>,
}

/// What a directory on a witnessed cgroup's way is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Seen {
    /// The one recorded.
    Recorded,
    /// Another of the same file system: the recorded one was removed.
    Replaced,
    /// Of another file system (a mount over the path): nothing is told.
    Other,
}

/// Flags to open a directory of a recorded chain: never through a symbolic link.
fn directory() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

impl AttemptGroup {
    /// The cgroup at `path` with no record of what it was: nothing it shows proves anything (M2T06R-900).
    #[cfg(test)]
    pub(super) fn at(path: &Path) -> AttemptGroup {
        AttemptGroup {
            path: path.to_owned(),
            root: PathBuf::from(MOUNT),
            witness: None,
        }
    }

    /// The cgroup at `path`, below `root`, as it is now (the root is the cgroup mount, but a test's own directory in
    /// tests).
    pub(super) fn witnessed(root: &Path, path: &Path) -> std::io::Result<AttemptGroup> {
        Ok(AttemptGroup {
            path: path.to_owned(),
            root: root.to_owned(),
            witness: Some(Witness::of(root, path)?),
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Its witness, as its tracking records it.
    pub(super) fn witness(&self) -> Option<&Witness> {
        self.witness.as_ref()
    }

    /// As if it had been made in another boot (tests).
    #[cfg(test)]
    fn another_boot(&mut self) {
        if let Some(w) = self.witness.as_mut() {
            w.boot = "another-boot".to_owned();
        }
    }

    /// Make the cgroup at `path` and move `pid` (the leader, held at its start gate) into it.
    pub(super) fn adopt(path: &Path, pid: u32) -> std::io::Result<AttemptGroup> {
        match std::fs::create_dir(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let group = AttemptGroup::witnessed(Path::new(MOUNT), path)?;
        group.write("cgroup.procs", &pid.to_string())?;
        Ok(group)
    }

    /// Its directory, opened only through the directories its witness recorded, each checked by its device and inode:
    /// `Some` when it is there; `None` when one of them is absent from its checked parent (a cgroup can be removed
    /// only when empty, and whatever was below it with it); an error when anything else is found, or there is no
    /// witness (M2T06R-900).
    fn open_checked(&self) -> Result<Option<rustix::fd::OwnedFd>, String> {
        let witness = self
            .witness
            .as_ref()
            .ok_or_else(|| format!("{}: no record of what it was", self.path.display()))?;
        let parts: Vec<&std::ffi::OsStr> = self
            .path
            .strip_prefix(&self.root)
            .map_err(|e| format!("{}: {e}", self.path.display()))?
            .components()
            .map(|c| c.as_os_str())
            .collect();
        if parts.len() + 1 != witness.chain.len() {
            return Err(format!(
                "{}: its record does not fit its path",
                self.path.display()
            ));
        }
        // The recorded directory itself, another directory of the same file system in its place, or something else.
        let seen = |fd: &rustix::fd::OwnedFd, at: usize| -> Result<Seen, String> {
            let stat =
                rustix::fs::fstat(fd).map_err(|e| format!("{}: {e}", self.path.display()))?;
            let (dev, ino) = witness.chain[at];
            Ok(if (stat.st_dev, stat.st_ino) == (dev, ino) {
                Seen::Recorded
            } else if stat.st_dev == dev {
                Seen::Replaced
            } else {
                Seen::Other
            })
        };
        let other = || {
            format!(
                "{}: a directory on its way is on another file system than the one recorded",
                self.path.display()
            )
        };
        let mut dir = rustix::fs::open(&self.root, directory(), rustix::fs::Mode::empty())
            .map_err(|e| format!("{}: {e}", self.root.display()))?;
        if seen(&dir, 0)? != Seen::Recorded {
            return Err(format!(
                "{}: not the cgroup mount recorded",
                self.root.display()
            ));
        }
        for (i, part) in parts.iter().enumerate() {
            match rustix::fs::openat(&dir, *part, directory(), rustix::fs::Mode::empty()) {
                Ok(next) => match seen(&next, i + 1)? {
                    Seen::Recorded => dir = next,
                    // Another cgroup of the same hierarchy at its path: the recorded one was removed, which a
                    // cgroup can be only when nothing is in it or below it (cgroupfs never gives an inode number
                    // twice in a boot, and cgroup v2 has no rename): gone (M2T06R-1001). Its successor is not it.
                    Seen::Replaced => return Ok(None),
                    Seen::Other => return Err(other()),
                },
                Err(rustix::io::Errno::NOENT) => return Ok(None),
                Err(e) => return Err(format!("{}: {e}", self.path.display())),
            }
        }
        Ok(Some(dir))
    }

    /// Write `text` to its file `name`, through its checked directory, never creating it.
    fn write(&self, name: &str, text: &str) -> std::io::Result<()> {
        use std::io::Write as _;
        let dir = self
            .open_checked()
            .map_err(std::io::Error::other)?
            .ok_or_else(|| std::io::Error::other("it is gone"))?;
        let fd = rustix::fs::openat(
            &dir,
            name,
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )?;
        std::fs::File::from(fd).write_all(text.as_bytes())
    }

    /// Whether any process is in it: `Ok(false)` once it is empty, removed, or of another boot; an error when that
    /// cannot be told, its events read only through its checked directory (M2T06R-900).
    pub(super) fn populated(&self) -> Result<bool, String> {
        use std::io::Read as _;
        if let Some(witness) = &self.witness
            && hoh_agents::identity::boot_id().is_ok_and(|now| now != witness.boot)
        {
            return Ok(false);
        }
        let Some(dir) = self.open_checked()? else {
            return Ok(false);
        };
        let fd = rustix::fs::openat(
            &dir,
            "cgroup.events",
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|e| format!("{}: cgroup.events: {e}", self.path.display()))?;
        let mut events = String::new();
        std::fs::File::from(fd)
            .read_to_string(&mut events)
            .map_err(|e| format!("{}: cgroup.events: {e}", self.path.display()))?;
        events
            .lines()
            .find_map(|line| line.strip_prefix("populated "))
            .map(|v| v.trim() == "1")
            .ok_or_else(|| format!("{}: no populated line", self.path.display()))
    }

    /// It and every cgroup below it, parents first: `populated` and `cgroup.kill` cover the whole subtree, and so must
    /// SIGTERM (decision 8d v7, M2T06R-501). A directory that cannot be read is skipped (its members still get
    /// `cgroup.kill`, and `populated` still counts them).
    fn subtree(&self) -> Vec<PathBuf> {
        let mut found = vec![self.path.clone()];
        let mut at = 0;
        while at < found.len() {
            if let Ok(entries) = std::fs::read_dir(&found[at]) {
                found.extend(
                    entries
                        .filter_map(Result::ok)
                        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                        .map(|e| e.path()),
                );
            }
            at += 1;
        }
        found
    }

    /// Its checked directory and every directory below it, opened one from another (never through a path that may
    /// name another cgroup by then).
    fn checked_subtree(&self) -> Vec<rustix::fd::OwnedFd> {
        let Ok(Some(top)) = self.open_checked() else {
            return Vec::new();
        };
        let mut found = vec![top];
        let mut at = 0;
        while at < found.len() {
            let names: Vec<std::ffi::CString> = rustix::fs::Dir::read_from(&found[at])
                .map(|dir| {
                    dir.filter_map(Result::ok)
                        .filter(|e| e.file_type() == rustix::fs::FileType::Directory)
                        .map(|e| e.file_name().to_owned())
                        .filter(|n| n.as_bytes() != b"." && n.as_bytes() != b"..")
                        .collect()
                })
                .unwrap_or_default();
            for name in names {
                if let Ok(below) = rustix::fs::openat(
                    &found[at],
                    name.as_c_str(),
                    directory(),
                    rustix::fs::Mode::empty(),
                ) {
                    found.push(below);
                }
            }
            at += 1;
        }
        found
    }

    /// SIGTERM to each process in it or in a cgroup below it, each pinned by a pidfd and checked to be in this subtree
    /// still (a pid that ended and was reused is never signalled); whether any got it. Only the recorded cgroup, read
    /// through its checked directories: a cgroup made later at its path holds another's processes (M2T06R-1000).
    pub(super) fn terminate(&self) -> bool {
        use std::io::Read as _;
        use std::os::unix::ffi::OsStrExt as _;
        let wanted = self.relative();
        let wanted = wanted.as_os_str().as_bytes();
        let mut sent = false;
        for cgroup in self.checked_subtree() {
            let Ok(fd) = rustix::fs::openat(
                &cgroup,
                "cgroup.procs",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            ) else {
                continue;
            };
            let mut procs = String::new();
            if std::fs::File::from(fd).read_to_string(&mut procs).is_err() {
                continue;
            }
            for pid in procs.lines().filter_map(|l| l.trim().parse::<i32>().ok()) {
                let Some(raw) = rustix::process::Pid::from_raw(pid) else {
                    continue;
                };
                let Ok(pidfd) =
                    rustix::process::pidfd_open(raw, rustix::process::PidfdFlags::empty())
                else {
                    continue;
                };
                // Its cgroup as bytes (a cgroup's name need not be UTF-8): this one or one below it.
                let here = std::fs::read(format!("/proc/{pid}/cgroup")).is_ok_and(|c| {
                    c.split(|&b| b == b'\n').any(|l| {
                        l.strip_prefix(b"0::").is_some_and(|at| {
                            at == wanted
                                || at.strip_prefix(wanted).is_some_and(|r| r.starts_with(b"/"))
                        })
                    })
                });
                if here {
                    sent |=
                        rustix::process::pidfd_send_signal(&pidfd, rustix::process::Signal::TERM)
                            .is_ok();
                }
            }
        }
        sent
    }

    /// SIGKILL to every process in it at once, by the kernel (`cgroup.kill`, through its checked directory); whether it
    /// was asked.
    pub(super) fn kill(&self) -> bool {
        self.write("cgroup.kill", "1").is_ok()
    }

    /// Remove it once it is empty, the cgroups below it first (a leftover empty directory is harmless).
    pub(super) fn remove(&self) {
        for cgroup in self.subtree().iter().rev() {
            let _ = std::fs::remove_dir(cgroup);
        }
    }

    /// Its path as `/proc/<pid>/cgroup` names it.
    fn relative(&self) -> PathBuf {
        let rel = self.path.strip_prefix(MOUNT).unwrap_or(&self.path);
        Path::new("/").join(rel)
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

/// How an attempt's processes are tracked (decision 8d v5 to v7): by its cgroup with the marker scan, or (degraded) by
/// the /proc scan alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Tracking {
    /// Its cgroup, and what it was when it was made (`None` when its record has none: nothing it shows proves
    /// anything).
    Cgroup {
        path: PathBuf,
        witness: Option<Witness>,
    },
    /// Why there is no cgroup.
    Proc(String),
    /// Recorded by a host that saw another world (other pid, mount, cgroup or user namespaces): nothing this host sees of
    /// the attempt's processes, cgroup or records is theirs, so nothing is proved (M2T06R-900); why.
    Foreign(String),
}

/// The namespaces this process sees the system through (`/proc/self/ns/{pid,mnt,cgroup,user}`), as recorded with each
/// attempt's tracking: a later host that sees another world proves nothing of it (decision 8d v7, M2T06R-900).
pub(super) fn world() -> Option<serde_json::Value> {
    let mut seen = serde_json::Map::new();
    for name in ["pid", "mnt", "cgroup", "user"] {
        let link = std::fs::read_link(format!("/proc/self/ns/{name}")).ok()?;
        seen.insert(name.to_owned(), serde_json::json!(link.to_string_lossy()));
    }
    Some(serde_json::Value::Object(seen))
}

impl Tracking {
    /// As recorded in its `attempt.tracking` event.
    pub(super) fn json(&self, attempt_id: &str) -> serde_json::Value {
        match self {
            Tracking::Cgroup { path, witness } => serde_json::json!({
                "attempt_id": attempt_id, "tracking": "cgroup", "cgroup": path.to_string_lossy(),
                "boot_id": witness.as_ref().map(|w| w.boot.clone()),
                "inodes": witness.as_ref().map(|w| w.chain.iter().map(|(d, i)| [*d, *i]).collect::<Vec<_>>()),
                "world": world(),
            }),
            Tracking::Proc(reason) => serde_json::json!({
                "attempt_id": attempt_id, "tracking": "proc", "reason": reason, "world": world(),
            }),
            Tracking::Foreign(why) => serde_json::json!({
                "attempt_id": attempt_id, "tracking": "foreign", "reason": why,
            }),
        }
    }

    /// From its `attempt.tracking` event: none recorded (an attempt of an earlier host version, or one whose start
    /// recorded none) is tracked by the /proc scan.
    pub(super) fn of(event: Option<&serde_json::Value>) -> Tracking {
        Tracking::seen_from(event, world())
    }

    /// [`Tracking::of`] by a host that sees `now`: a record made in another world is foreign.
    fn seen_from(event: Option<&serde_json::Value>, now: Option<serde_json::Value>) -> Tracking {
        if let Some(recorded) = event.map(|e| &e["world"]).filter(|w| !w.is_null())
            && now.as_ref() != Some(recorded)
        {
            return Tracking::Foreign(format!(
                "recorded seeing {recorded}, this host sees {}",
                now.unwrap_or(serde_json::Value::Null)
            ));
        }
        match event {
            Some(e) if e["tracking"] == "cgroup" => e["cgroup"]
                .as_str()
                .filter(|p| recorded_form(Path::new(p)))
                .map_or_else(
                    || Tracking::Proc("no cgroup path recorded".to_owned()),
                    |p| Tracking::Cgroup {
                        path: PathBuf::from(p),
                        witness: witness_of(e),
                    },
                ),
            Some(e) => Tracking::Proc(e["reason"].as_str().unwrap_or("recorded").to_owned()),
            None => Tracking::Proc("no tracking recorded".to_owned()),
        }
    }

    /// Its cgroup, if it has one.
    pub(super) fn cgroup(&self) -> Option<AttemptGroup> {
        match self {
            Tracking::Cgroup { path, witness } => Some(AttemptGroup {
                path: path.clone(),
                root: PathBuf::from(MOUNT),
                witness: witness.clone(),
            }),
            Tracking::Proc(_) | Tracking::Foreign(_) => None,
        }
    }

    /// The tracking of a cgroup just made.
    pub(super) fn of_group(group: &AttemptGroup) -> Tracking {
        Tracking::Cgroup {
            path: group.path().to_owned(),
            witness: group.witness().cloned(),
        }
    }
}

/// The witness an `attempt.tracking` event records, if a whole one.
fn witness_of(event: &serde_json::Value) -> Option<Witness> {
    let boot = event["boot_id"].as_str()?.to_owned();
    let chain = event["inodes"]
        .as_array()?
        .iter()
        .map(|pair| Some((pair.get(0)?.as_u64()?, pair.get(1)?.as_u64()?)))
        .collect::<Option<Vec<_>>>()?;
    (!chain.is_empty()).then_some(Witness { boot, chain })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn a_cgroups_subtree_is_every_cgroup_below_it_and_goes_bottom_up() -> R {
        // M2T06R-651 (decision 8d v7): SIGTERM goes to every cgroup below the attempt's, however deep, and a proved
        // cgroup is removed with all of them, the deepest first.
        let root = tempfile::tempdir()?;
        let at = root.path().join("attempt-0123456789abcdef");
        let deepest = at.join("a").join("b").join("c");
        std::fs::create_dir_all(&deepest)?;
        std::fs::create_dir_all(at.join("d"))?;
        let group = AttemptGroup::witnessed(root.path(), &at)?;
        let mut found = group.subtree();
        assert_eq!(found.first(), Some(&at), "the cgroup itself first");
        found.sort();
        let mut wanted = vec![
            at.clone(),
            at.join("a"),
            at.join("a").join("b"),
            deepest.clone(),
            at.join("d"),
        ];
        wanted.sort();
        assert_eq!(found, wanted);
        group.remove();
        assert!(
            !at.exists(),
            "left: {:?}",
            std::fs::read_dir(root.path())?.count()
        );
        Ok(())
    }

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
        let group = AttemptGroup::witnessed(dir.path(), dir.path())?;
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
        let group = AttemptGroup::witnessed(dir.path(), dir.path())?;
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
    fn an_empty_cgroup_is_proved_at_once_and_a_removed_one_too() -> R {
        let dir = fake(0)?;
        assert_eq!(
            AttemptGroup::witnessed(dir.path(), dir.path())?.stop(Duration::ZERO, Duration::ZERO),
            (Vec::new(), Proof::Done)
        );
        // M2T06R-900: removed means absent from its verified parent.
        let root = tempfile::tempdir()?;
        let at = root.path().join("attempt-0123456789abcdef");
        std::fs::create_dir(&at)?;
        let group = AttemptGroup::witnessed(root.path(), &at)?;
        std::fs::remove_dir(&at)?;
        assert_eq!(group.proof(), Proof::Done);
        Ok(())
    }

    #[test]
    fn a_cgroup_is_judged_only_through_the_directories_it_was_recorded_in() -> R {
        // M2T06R-900, -1001 (decision 8d v7): what is at a recorded path is not taken for the recorded cgroup.
        // Another directory of the same file system in its place, or in its parent's, means the recorded one was
        // removed (a cgroup goes only when empty; cgroupfs gives no inode number twice in a boot): gone, and the
        // successor is not read (here it says populated 1). A directory of another file system (a mount over the
        // path) tells nothing; nor does a path with no record of what it was.
        let root = tempfile::tempdir()?;
        let parent = root.path().join("host.scope");
        let at = parent.join("attempt-0123456789abcdef");
        std::fs::create_dir_all(&at)?;
        std::fs::write(at.join("cgroup.events"), b"populated 1\n")?;
        let group = AttemptGroup::witnessed(root.path(), &at)?;
        assert_eq!(
            group.proof(),
            Proof::Pending,
            "the recorded cgroup, populated"
        );
        // On another file system: the recorded device is not the one found.
        let mut elsewhere = group.clone();
        if let Some(w) = elsewhere.witness.as_mut()
            && let Some(last) = w.chain.last_mut()
        {
            last.0 ^= 1;
        }
        assert_eq!(elsewhere.proof(), Proof::Incomplete, "another file system");
        // Another directory in its place, populated: the recorded one is gone, its successor not read. The old one
        // is kept (moved away) so the new one cannot get its inode number back, as a file system other than cgroupfs
        // may give a freed number again (seen in CI).
        std::fs::rename(&at, root.path().join("moved"))?;
        std::fs::create_dir(&at)?;
        std::fs::write(at.join("cgroup.events"), b"populated 1\n")?;
        assert_eq!(group.proof(), Proof::Done, "replaced at its path");
        // Its parent replaced, a populated successor below it: gone too.
        std::fs::rename(&parent, root.path().join("old-parent"))?;
        std::fs::create_dir_all(&at)?;
        std::fs::write(at.join("cgroup.events"), b"populated 1\n")?;
        assert_eq!(group.proof(), Proof::Done, "its parent replaced");
        // No record of what it was: an absence proves nothing.
        assert_eq!(
            AttemptGroup::at(&at.join("gone")).proof(),
            Proof::Incomplete
        );
        Ok(())
    }

    #[test]
    fn a_cgroups_tracking_records_what_it_was_and_a_later_host_reads_it_back() -> R {
        // M2T06R-900: the attempt.tracking event carries the witness, so a host after a restart judges the same way.
        let root = tempfile::tempdir()?;
        let at = root.path().join("attempt-0123456789abcdef");
        std::fs::create_dir(&at)?;
        let group = AttemptGroup::witnessed(root.path(), &at)?;
        let tracking = Tracking::of_group(&group);
        let event = tracking.json("p/A1");
        assert_eq!(event["inodes"].as_array().map(Vec::len), Some(2), "{event}");
        assert_eq!(witness_of(&event).as_ref(), group.witness());
        Ok(())
    }

    #[test]
    fn a_record_made_in_another_world_is_foreign() {
        // M2T06R-900 (decision 8d v7): a host started in other namespaces (a new mount namespace over /proc or the
        // cgroup mount, another pid namespace) sees another world; what it sees proves nothing of an attempt recorded
        // in this one. A record without a world (none is written so) is read as before.
        let now = world();
        assert!(now.is_some(), "this process's namespaces");
        let recorded = Tracking::Proc("p".to_owned()).json("p/A1");
        assert_eq!(
            Tracking::seen_from(Some(&recorded), now.clone()),
            Tracking::Proc("p".to_owned())
        );
        let elsewhere = serde_json::json!({"pid": "pid:[1]", "mnt": "mnt:[1]", "cgroup": "cgroup:[1]", "user": "user:[1]"});
        assert!(matches!(
            Tracking::seen_from(Some(&recorded), Some(elsewhere)),
            Tracking::Foreign(_)
        ));
        assert!(matches!(
            Tracking::seen_from(Some(&recorded), None),
            Tracking::Foreign(_)
        ));
        let mut legacy = recorded.clone();
        legacy["world"] = serde_json::Value::Null;
        assert_eq!(
            Tracking::seen_from(Some(&legacy), None),
            Tracking::Proc("p".to_owned())
        );
    }

    /// Set in this test binary when it runs inside a delegated scope.
    const IN_SCOPE: &str = "HOH_LIB_TEST_IN_DELEGATED_SCOPE";

    /// Run this binary's ignored test `name` inside a delegated scope of the user's systemd manager; it must pass.
    fn in_delegated_scope(name: &str) -> R {
        let out = std::process::Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "-p",
                "Delegate=yes",
                "--quiet",
                "--collect",
                "--expand-environment=no",
                "--",
            ])
            .arg(std::env::current_exe()?)
            .args([
                name,
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(IN_SCOPE, "1")
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() || !stdout.contains("1 passed") {
            return Err(format!(
                "{name} in a delegated scope: {}\n{stdout}\n{}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            )
            .into());
        }
        Ok(())
    }

    #[test]
    fn a_cgroup_made_again_where_an_attempts_was_is_not_the_attempts() -> R {
        in_delegated_scope("host::cgroup::tests::replaced_cgroup_inside")
    }

    #[test]
    #[ignore = "runs inside a delegated scope, started by a_cgroup_made_again_where_an_attempts_was_is_not_the_attempts"]
    fn replaced_cgroup_inside() -> R {
        // M2T06R-1000, -1001 on real cgroups: an attempt's cgroup, recorded, then removed and made again at its path
        // with another process in it. That process gets no SIGTERM (it is not the attempt's), and the recorded cgroup
        // is proved gone; so it is when its parent was removed and made again too.
        use std::os::unix::process::CommandExt as _;
        if std::env::var_os(IN_SCOPE).is_none() {
            return Ok(());
        }
        let delegated = Delegated::detect()?;
        let scope = delegated
            .base
            .join(format!("hoh-replaced-{}", std::process::id()));
        let at = scope.join("attempt-0123456789abcdef");
        std::fs::create_dir_all(&at)?;
        let group = AttemptGroup::witnessed(Path::new(MOUNT), &at)?;
        std::fs::remove_dir(&at)?;
        std::fs::create_dir(&at)?;
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()?;
        std::fs::write(at.join("cgroup.procs"), other.id().to_string())?;
        let signalled = group.terminate();
        let lived = other.try_wait()?.is_none();
        let replaced = group.proof();
        let _ = other.kill();
        let _ = other.wait();
        std::fs::remove_dir(&at)?;
        std::fs::remove_dir(&scope)?;
        std::fs::create_dir_all(&at)?;
        let parent_replaced = group.proof();
        std::fs::remove_dir(&at)?;
        std::fs::remove_dir(&scope)?;
        assert!(
            !signalled && lived,
            "a process of the cgroup made later got SIGTERM"
        );
        assert_eq!(replaced, Proof::Done, "replaced at its path");
        assert_eq!(parent_replaced, Proof::Done, "its parent replaced");
        Ok(())
    }

    #[test]
    fn a_cgroup_of_another_boot_is_gone() -> R {
        // M2T06R-900: whatever is at its path now, nothing of a cgroup made before this boot is left.
        let root = tempfile::tempdir()?;
        let at = root.path().join("attempt-0123456789abcdef");
        std::fs::create_dir(&at)?;
        std::fs::write(at.join("cgroup.events"), b"populated 1\n")?;
        let mut group = AttemptGroup::witnessed(root.path(), &at)?;
        group.another_boot();
        assert_eq!(group.proof(), Proof::Done);
        Ok(())
    }

    #[test]
    fn only_a_recorded_path_of_the_hosts_form_is_trusted() {
        // M2T06R-458: below the cgroup mount, named attempt-<16 hex>; anything else is tracked by the /proc scan.
        let event = |path: &str| serde_json::json!({"tracking": "cgroup", "cgroup": path});
        let good = "/sys/fs/cgroup/user.slice/x.scope/attempt-0123456789abcdef";
        assert_eq!(
            Tracking::of(Some(&event(good))),
            Tracking::Cgroup {
                path: PathBuf::from(good),
                witness: None
            }
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
