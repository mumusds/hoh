//! A private directory (docs/adr/ADR-016.md, the note's round 4 correction): made by this call with create_dir under an
//! unpredictable name, mode 0700, in the directory whose file system it is to use. The WAL self-test and the room probe
//! keep all their files in one, so that removing it removes nothing outside it, and nothing another creator made in the
//! shared directory. A same-user process that deliberately writes into it, renames or replaces it is the stated
//! residual.

use std::io;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};

/// A directory this call made, removed with what is in it by [`Private::remove`].
#[derive(Debug)]
#[must_use = "a private directory is removed by Private::remove"]
pub(super) struct Private {
    path: PathBuf,
}

/// A path of a private directory that could not be removed: its exact path and the OS's reason.
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

impl Private {
    /// Make the first of `names` that does not exist (create_dir: exclusive; mode 0700). A name that exists is
    /// another's and is skipped. The error: the path and the OS error that refused one, or the last name with
    /// AlreadyExists when every one exists.
    pub(super) fn make_first(
        names: impl IntoIterator<Item = PathBuf>,
    ) -> Result<Private, (PathBuf, io::Error)> {
        let mut taken = (
            PathBuf::new(),
            io::Error::from(io::ErrorKind::AlreadyExists),
        );
        for path in names {
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => return Ok(Private { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => taken = (path, e),
                Err(e) => return Err((path, e)),
            }
        }
        Err(taken)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Remove every entry in it (a directory with what is in it), then it: every path that stays is told, with the
    /// OS's reason (F7-302).
    pub(super) fn remove(self) -> Vec<Leftover> {
        let mut left = Vec::new();
        remove_tree(&self.path, &mut left);
        left
    }
}

fn remove_tree(dir: &Path, left: &mut Vec<Leftover>) {
    match std::fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) => {
                        let path = entry.path();
                        if entry.file_type().is_ok_and(|t| t.is_dir()) {
                            remove_tree(&path, left);
                        } else if let Err(error) = std::fs::remove_file(&path) {
                            left.push(Leftover { path, error });
                        }
                    }
                    Err(error) => left.push(Leftover {
                        path: dir.to_owned(),
                        error,
                    }),
                }
            }
        }
        Err(error) => left.push(Leftover {
            path: dir.to_owned(),
            error,
        }),
    }
    if let Err(error) = std::fs::remove_dir(dir) {
        left.push(Leftover {
            path: dir.to_owned(),
            error,
        });
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
        let private = Private::make_first([taken.clone(), first.clone()]).map_err(|(_, e)| e)?;
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
            .map_err(|(path, e)| (path, e.kind()));
        assert_eq!(
            all_taken,
            Err((taken.clone(), io::ErrorKind::AlreadyExists))
        );

        if !rustix::process::geteuid().is_root() {
            let private = Private::make_first([second.clone()]).map_err(|(_, e)| e)?;
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
}
