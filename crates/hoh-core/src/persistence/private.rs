//! A private directory (docs/adr/ADR-016.md, the note's rounds 4 to 6): made by this call with create_dir under an
//! unpredictable name, mode 0700, in the directory whose file system it is to use, then opened at once with
//! O_DIRECTORY|O_NOFOLLOW and held open for its whole life. The WAL self-test and the room probe keep all their files in
//! one. Everything this code makes or removes in it goes through that handle (openat, unlinkat; a symlink in it is
//! unlinked, never followed; a subdirectory that is a mount point, by another device or as the root of a mount, is not
//! entered and is told), so neither a symlink in it, a detected mount in it, nor the directory
//! swapped for another or for a symlink leads this code outside it (F7-400, F7-502); the directory itself is removed by
//! its name only while that name is still the directory held.
//!
//! The residual, not defended against: SQLite opens, creates and deletes its files in it by path, so a directory
//! swapped while the self-test runs may have SQLite create, overwrite or delete `selftest.db`, `-wal`, `-shm` and `-journal`
//! in the swapped-in target (F7-503); and a same-user process that tampers with it otherwise (a swap in the instants
//! between mkdir and open, between lstat and rmdir, or a mount made inside between the check and the descent).

use rustix::fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags};
use std::ffi::{CString, OsStr};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// A directory this call made, held open; removed with what is in it by [`Private::remove`].
#[derive(Debug)]
#[must_use = "a private directory is removed by Private::remove"]
pub(super) struct Private {
    path: PathBuf,
    dir: OwnedFd,
    made: rustix::fs::Stat,
}

/// Why [`Private::make_first`] gave no directory: the path it was about, the reason, and whether this call made that
/// directory (made, then not usable: it is left as it is, and told as a [`Leftover`], F7-501).
#[derive(Debug)]
pub(super) struct NotMade {
    pub(super) path: PathBuf,
    pub(super) error: io::Error,
    pub(super) made: bool,
}

impl NotMade {
    /// The directory this call made and left, if it made one.
    pub(super) fn leftover(&self) -> Option<Leftover> {
        self.made.then(|| Leftover {
            path: self.path.clone(),
            error: io::Error::new(self.error.kind(), self.error.to_string()),
        })
    }
}

/// A path of a private directory that could not be removed: its exact path and the reason.
#[derive(Debug)]
pub(super) struct Leftover {
    pub(super) path: PathBuf,
    pub(super) error: io::Error,
}

impl std::fmt::Display for Leftover {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is left, not removed: {}",
            self.path.display(),
            self.error
        )
    }
}

/// `count` names `<base><pid>-<16 random hex digits>` (getrandom): for [`Private::make_first`].
pub(super) fn names(base: &Path, count: usize) -> io::Result<Vec<PathBuf>> {
    (0..count)
        .map(|_| {
            let mut bytes = [0u8; 8];
            let mut filled = 0;
            while filled < bytes.len() {
                filled += rustix::rand::getrandom(
                    &mut bytes[filled..],
                    rustix::rand::GetRandomFlags::empty(),
                )?;
            }
            let mut name = base.as_os_str().to_owned();
            name.push(format!(
                "{}-{:016x}",
                std::process::id(),
                u64::from_le_bytes(bytes)
            ));
            Ok(PathBuf::from(name))
        })
        .collect()
}

/// The directory flags every directory of ours is opened with: never through a symlink.
fn directory() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

impl Private {
    /// Make the first of `names` that does not exist (create_dir: exclusive; mode 0700) and open it at once. A name
    /// that exists is another's and is skipped. The error ([`NotMade`]): the path and the reason one was refused, or
    /// could not be used once made (it is then left as it is, and told), or the last name with AlreadyExists when every
    /// one exists.
    pub(super) fn make_first(names: impl IntoIterator<Item = PathBuf>) -> Result<Private, NotMade> {
        let mut taken = NotMade {
            path: PathBuf::new(),
            error: io::Error::from(io::ErrorKind::AlreadyExists),
            made: false,
        };
        for path in names {
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Private::opened(&path).map_err(|error| NotMade {
                        path,
                        error,
                        made: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    taken = NotMade {
                        path,
                        error,
                        made: false,
                    };
                }
                Err(error) => {
                    return Err(NotMade {
                        path,
                        error,
                        made: false,
                    });
                }
            }
        }
        Err(taken)
    }

    /// The directory just made at `path`, opened: it must still be a directory (O_NOFOLLOW: not a symlink) and empty,
    /// else something was put in it, or in its place, since it was made. (Its owner is not compared: a file system that
    /// maps owners, FAT's `uid=`, shows another, F7-500.)
    fn opened(path: &Path) -> io::Result<Private> {
        let dir = rustix::fs::open(path, directory(), Mode::empty())?;
        let made = rustix::fs::fstat(&dir)?;
        Self::from_opened(path, dir, made)
    }

    fn from_opened(path: &Path, dir: OwnedFd, made: rustix::fs::Stat) -> io::Result<Private> {
        let empty = Dir::read_from(&dir)?
            .map(|entry| entry.map(|e| matches!(e.file_name().to_bytes(), b"." | b"..")))
            .collect::<Result<Vec<bool>, _>>()?
            .into_iter()
            .all(|dot| dot);
        if !empty {
            return Err(io::Error::other(
                "not empty when opened: another process put something in it, or another directory in its place",
            ));
        }
        Ok(Private {
            path: path.to_owned(),
            dir,
            made,
        })
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Make the file `name` in it (O_EXCL, O_NOFOLLOW, mode 0600), through its handle.
    pub(super) fn create(&self, name: &str) -> io::Result<std::fs::File> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = rustix::fs::openat(&self.dir, name, flags, Mode::from_raw_mode(0o600))?;
        Ok(std::fs::File::from(fd))
    }

    /// Remove every entry in it through its handle (a directory: its entries, then it; a symlink: unlinked, never
    /// followed), then it, by its name, while that name is still this directory. Every path that stays is told, with
    /// the reason (F7-302, F7-400).
    pub(super) fn remove(self) -> Vec<Leftover> {
        let mut left = Vec::new();
        empty(&self.dir, &self.path, &self.made, &mut left, &mount_root);
        let still = rustix::fs::statat(CWD, &self.path, AtFlags::SYMLINK_NOFOLLOW)
            .is_ok_and(|now| (now.st_dev, now.st_ino) == (self.made.st_dev, self.made.st_ino));
        if !still {
            left.push(Leftover {
                path: self.path,
                error: io::Error::other(
                    "the name is no longer the directory this call made (that one was emptied where it is now)",
                ),
            });
        } else if let Err(e) = rustix::fs::unlinkat(CWD, &self.path, AtFlags::REMOVEDIR) {
            left.push(Leftover {
                path: self.path,
                error: e.into(),
            });
        }
        left
    }
}

/// Whether the entry `name` of `dir` is the root of a mount, where the kernel says (STATX_ATTR_MOUNT_ROOT).
fn mount_root(dir: &OwnedFd, name: &std::ffi::CStr) -> bool {
    use rustix::fs::{StatxAttributes, StatxFlags};
    rustix::fs::statx(
        dir,
        name,
        AtFlags::NO_AUTOMOUNT | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::empty(),
    )
    .is_ok_and(|s| {
        s.stx_attributes_mask.contains(StatxAttributes::MOUNT_ROOT)
            && s.stx_attributes.contains(StatxAttributes::MOUNT_ROOT)
    })
}

/// Remove every entry of the directory open as `dir` (its path for the telling: `at`), through it; a subdirectory on
/// another device than the private directory (`made`), or the root of a mount, is not entered and is told (F7-502).
fn empty(
    dir: &OwnedFd,
    at: &Path,
    made: &rustix::fs::Stat,
    left: &mut Vec<Leftover>,
    is_mount: &impl Fn(&OwnedFd, &std::ffi::CStr) -> bool,
) {
    let entries: Vec<(CString, FileType)> = match Dir::read_from(dir) {
        Ok(entries) => {
            let mut named = Vec::new();
            for entry in entries {
                match entry {
                    Ok(entry) if !matches!(entry.file_name().to_bytes(), b"." | b"..") => {
                        named.push((entry.file_name().to_owned(), entry.file_type()));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        left.push(Leftover {
                            path: at.to_owned(),
                            error: e.into(),
                        });
                        break;
                    }
                }
            }
            named
        }
        Err(e) => {
            left.push(Leftover {
                path: at.to_owned(),
                error: e.into(),
            });
            return;
        }
    };
    for (name, kind) in entries {
        let path = at.join(OsStr::from_bytes(name.to_bytes()));
        let kind = match kind {
            FileType::Unknown => {
                rustix::fs::statat(dir, name.as_c_str(), AtFlags::SYMLINK_NOFOLLOW)
                    .map_or(FileType::Unknown, |s| FileType::from_raw_mode(s.st_mode))
            }
            kind => kind,
        };
        let removed = if kind == FileType::Directory {
            if is_mount(dir, name.as_c_str()) {
                left.push(Leftover {
                    path,
                    error: io::Error::other("a mount point: not entered"),
                });
                continue;
            }
            match rustix::fs::openat(dir, name.as_c_str(), directory(), Mode::empty()) {
                Ok(sub) => {
                    let same_device = rustix::fs::fstat(&sub).and_then(|s| {
                        if s.st_dev == made.st_dev {
                            Ok(())
                        } else {
                            Err(rustix::io::Errno::XDEV)
                        }
                    });
                    if let Err(error) = same_device {
                        left.push(Leftover {
                            path,
                            error: io::Error::other(format!("not entered: {error}")),
                        });
                        continue;
                    }
                    empty(&sub, &path, made, left, is_mount);
                    rustix::fs::unlinkat(dir, name.as_c_str(), AtFlags::REMOVEDIR)
                }
                Err(e) => Err(e),
            }
        } else {
            rustix::fs::unlinkat(dir, name.as_c_str(), AtFlags::empty())
        };
        if let Err(e) = removed {
            left.push(Leftover {
                path,
                error: e.into(),
            });
        }
    }
}

/// The leftovers, to follow the rest of a message: `<path> is left, not removed: <error>; …`.
pub(super) fn told(left: &[Leftover]) -> String {
    left.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mapped_owner_does_not_refuse_an_empty_directory() -> Result<(), Box<dyn std::error::Error>>
    {
        // F7-500, AC-062-06: only the reported uid is injected; the directory and its cleanup are real.
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("private");
        std::fs::create_dir(&path)?;
        let dir = rustix::fs::open(&path, directory(), Mode::empty())?;
        let mut made = rustix::fs::fstat(&dir)?;
        made.st_uid = rustix::process::geteuid().as_raw().wrapping_add(1);
        let private = Private::from_opened(&path, dir, made)?;
        drop(private.create("room")?);
        assert!(private.remove().is_empty());
        assert!(!path.exists());

        std::fs::create_dir(&path)?;
        std::fs::write(path.join("foreign"), "kept")?;
        assert!(Private::opened(&path).is_err(), "nonempty is still refused");
        assert_eq!(std::fs::read_to_string(path.join("foreign"))?, "kept");
        Ok(())
    }

    #[test]
    fn cleanup_keeps_and_reports_a_subdirectory_on_another_device()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-502: simulate a different device without mounting; exercise the actual recursive cleanup.
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("private");
        let mut private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        let sub = path.join("other-device");
        std::fs::create_dir(&sub)?;
        std::fs::write(sub.join("sentinel"), "kept")?;
        private.made.st_dev ^= 1;
        let left = private.remove();
        assert_eq!(std::fs::read_to_string(sub.join("sentinel"))?, "kept");
        for p in [&sub, &path] {
            assert!(told(&left).contains(&format!("{} is left, not removed", p.display())));
        }
        assert!(left[0].error.to_string().contains("not entered"));
        Ok(())
    }

    #[test]
    fn cleanup_keeps_and_reports_a_mount_root_on_the_same_device()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-502: inject only the mount-root observation; real files and recursion, no sudo or mount.
        let parent = tempfile::tempdir()?;
        let path = parent.path().join("private");
        let private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        let sub = path.join("ordinary/mount");
        std::fs::create_dir_all(&sub)?;
        std::fs::write(sub.join("sentinel"), "kept")?;
        std::fs::write(path.join("ordinary/ours"), "remove")?;
        let mut left = Vec::new();
        empty(&private.dir, &path, &private.made, &mut left, &|_, name| {
            name.to_bytes() == b"mount"
        });
        assert_eq!(std::fs::read_to_string(sub.join("sentinel"))?, "kept");
        assert!(!path.join("ordinary/ours").exists());
        for p in [&sub, &path.join("ordinary")] {
            assert!(told(&left).contains(&format!("{} is left, not removed", p.display())));
        }
        assert!(
            left[0]
                .error
                .to_string()
                .contains("a mount point: not entered")
        );
        Ok(())
    }

    #[test]
    fn mount_root_observation_recognizes_root_but_not_an_ordinary_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        // Read-only observation of existing mounts; no mount changes are made.
        let root = rustix::fs::open("/", directory(), Mode::empty())?;
        assert!(mount_root(&root, c"."));
        let parent = tempfile::tempdir()?;
        std::fs::create_dir(parent.path().join("ordinary"))?;
        let dir = rustix::fs::open(parent.path(), directory(), Mode::empty())?;
        assert!(!mount_root(&dir, c"ordinary"));
        Ok(())
    }

    #[test]
    fn a_private_directory_is_made_once_and_removed_whole_and_what_stays_is_told()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-300..302: made exclusively (a name that exists is skipped), 0700, removed with what is in it; a path that
        // cannot be removed is told with its exact path, and so is every directory that therefore stays.
        use std::os::unix::fs::PermissionsExt as _;
        let parent = tempfile::tempdir()?;
        let base = parent.path().join("p-");
        let taken = parent.path().join("p-taken");
        std::fs::write(&taken, "another's")?;
        let [first, second] = [names(&base, 1)?, names(&base, 1)?].map(|mut n| n.remove(0));
        assert_ne!(first, second);
        let private = Private::make_first([taken.clone(), first.clone()]).map_err(|n| n.error)?;
        assert_eq!(private.path(), first);
        assert_eq!(
            std::fs::metadata(&first)?.permissions().mode() & 0o777,
            0o700
        );
        std::fs::create_dir(first.join("sub"))?;
        std::fs::write(first.join("sub/file"), "x")?;
        std::fs::write(first.join("file"), "x")?;
        assert!(private.remove().is_empty());
        assert!(!first.exists());
        assert_eq!(std::fs::read_to_string(&taken)?, "another's");

        let all_taken = Private::make_first([taken.clone()])
            .map(|_| ())
            .map_err(|n| (n.path, n.error.kind()));
        assert_eq!(
            all_taken,
            Err((taken.clone(), io::ErrorKind::AlreadyExists))
        );

        if !rustix::process::geteuid().is_root() {
            let private = Private::make_first([second.clone()]).map_err(|n| n.error)?;
            std::fs::create_dir(second.join("sub"))?;
            std::fs::write(second.join("sub/file"), "x")?;
            std::fs::set_permissions(second.join("sub"), std::fs::Permissions::from_mode(0o500))?;
            let left: Vec<PathBuf> = private.remove().into_iter().map(|l| l.path).collect();
            std::fs::set_permissions(second.join("sub"), std::fs::Permissions::from_mode(0o700))?;
            assert_eq!(
                left,
                [second.join("sub/file"), second.join("sub"), second.clone()]
            );
        }
        Ok(())
    }

    #[test]
    fn its_cleanup_never_follows_a_symlink_out_of_it() -> Result<(), Box<dyn std::error::Error>> {
        // F7-400: a symlink in it is unlinked, not followed; swapped for a symlink to another directory after it was
        // made, it is emptied where it really is and its name is left and told; the other directory keeps its files.
        let parent = tempfile::tempdir()?;
        let outside = parent.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("sentinel"), "kept")?;

        let path = names(&parent.path().join("p-"), 1)?.remove(0);
        let private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        std::os::unix::fs::symlink(&outside, path.join("link"))?;
        drop(private.create("room")?);
        assert!(private.remove().is_empty());
        assert!(!path.exists());
        assert_eq!(std::fs::read_to_string(outside.join("sentinel"))?, "kept");

        let private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        drop(private.create("room")?);
        let moved = parent.path().join("moved");
        std::fs::rename(&path, &moved)?;
        std::os::unix::fs::symlink(&outside, &path)?;
        let left = private.remove();
        assert_eq!(
            left.iter().map(|l| l.path.as_path()).collect::<Vec<_>>(),
            [path.as_path()]
        );
        assert_eq!(std::fs::read_to_string(outside.join("sentinel"))?, "kept");
        assert_eq!(std::fs::read_dir(&moved)?.count(), 0, "emptied where it is");
        std::fs::remove_file(&path)?;
        std::fs::remove_dir(&moved)?;

        // Swapped for another's empty directory: that one is not removed either.
        let private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        std::fs::rename(&path, &moved)?;
        std::fs::create_dir(&path)?;
        assert_eq!(private.remove().len(), 1);
        assert!(path.is_dir(), "another's directory stays");
        std::fs::remove_dir(&path)?;
        std::fs::remove_dir(&moved)?;

        let other = parent.path().join("other");
        std::os::unix::fs::symlink(&outside, &other)?;
        let refused = Private::make_first([other.clone()])
            .map(|_| ())
            .map_err(|n| (n.path, n.error.kind()));
        assert_eq!(refused, Err((other, io::ErrorKind::AlreadyExists)));
        Ok(())
    }

    #[test]
    #[ignore = "mounts with sudo -n (docs/09 fault matrix item 7, F7-502); run with --ignored"]
    fn its_cleanup_does_not_enter_a_mount_inside_it() -> Result<(), Box<dyn std::error::Error>> {
        // F7-502: a directory mounted inside it (a bind mount of another directory of the same file system, a tmpfs)
        // is not entered, as `rm --one-file-system` does: what is in it stays, and it is told.
        let sudo = |args: &[&str]| -> Result<(), String> {
            let out = std::process::Command::new("sudo")
                .arg("-n")
                .args(args)
                .output()
                .map_err(|e| format!("sudo: {e}"))?;
            if out.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "sudo -n {args:?}: {} (this test needs sudo -n)",
                    String::from_utf8_lossy(&out.stderr).trim()
                ))
            }
        };
        let parent = tempfile::tempdir()?;
        let outside = parent.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("sentinel"), "kept")?;
        let path = names(&parent.path().join("p-"), 1)?.remove(0);
        let private = Private::make_first([path.clone()]).map_err(|n| n.error)?;
        let (bind, fs) = (path.join("bind"), path.join("tmpfs"));
        std::fs::create_dir(&bind)?;
        std::fs::create_dir(&fs)?;
        let text = |p: &Path| p.display().to_string();
        sudo(&["mount", "--bind", &text(&outside), &text(&bind)])?;
        let mounted = sudo(&[
            "mount",
            "-t",
            "tmpfs",
            "-o",
            "size=1m,mode=0777",
            "tmpfs",
            &text(&fs),
        ]);
        let written = mounted.clone().map_err(|e| e.into()).and_then(|()| {
            std::fs::write(fs.join("inside"), "kept").map_err(Box::<dyn std::error::Error>::from)
        });
        let left: Vec<PathBuf> = private.remove().into_iter().map(|l| l.path).collect();
        let inside = std::fs::read_to_string(fs.join("inside")).ok();
        let unmounted = [
            sudo(&["umount", &text(&bind)]),
            sudo(&["umount", &text(&fs)]),
        ];
        mounted?;
        written?;
        for result in unmounted {
            result?;
        }
        assert_eq!(std::fs::read_to_string(outside.join("sentinel"))?, "kept");
        assert_eq!(inside.as_deref(), Some("kept"));
        let mut told = left;
        told.sort();
        let mut want = vec![bind, fs, path];
        want.sort();
        assert_eq!(told, want);
        Ok(())
    }
}
