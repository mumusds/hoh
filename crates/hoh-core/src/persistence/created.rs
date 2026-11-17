//! Files this call made (docs/adr/ADR-016.md, the note's round 3 rule, F7-200): a file the WAL self-test or the room
//! probe may remove is removed only through the token its own `create_new` left, never by name alone. A name this call
//! did not create is never removed, however it was checked before.

use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

/// A file this call made with create_new: its path and the (dev, inode) it had then.
#[derive(Debug)]
#[must_use = "a file this call made is removed through its token"]
pub(super) struct Created {
    path: PathBuf,
    id: (u64, u64),
}

/// A file of this call's that could not be removed: its exact path and the OS's reason.
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

impl Created {
    /// Make `path` with create_new (an existing file is another's: AlreadyExists), and the open file.
    pub(super) fn new(path: &Path) -> io::Result<(Created, std::fs::File)> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let meta = file.metadata()?;
        let made = Created {
            path: path.to_owned(),
            id: (meta.dev(), meta.ino()),
        };
        Ok((made, file))
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    /// Remove the file if its path still is the file this call made (lstat; the window between it and the unlink
    /// stays: there is no unlink by inode). A path that is gone, or is another file now (another creator's, or one
    /// SQLite made anew), is left as it is.
    pub(super) fn remove(self) -> Result<(), Leftover> {
        let gone = |e: &io::Error| e.kind() == io::ErrorKind::NotFound;
        match std::fs::symlink_metadata(&self.path) {
            Err(e) if gone(&e) => Ok(()),
            Ok(meta) if (meta.dev(), meta.ino()) != self.id => Ok(()),
            Ok(_) => match std::fs::remove_file(&self.path) {
                Err(e) if !gone(&e) => Err(Leftover {
                    path: self.path,
                    error: e,
                }),
                _ => Ok(()),
            },
            Err(e) => Err(Leftover {
                path: self.path,
                error: e,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_file_is_removed_only_while_its_path_is_still_that_file()
    -> Result<(), Box<dyn std::error::Error>> {
        // F7-200: removed through its token; gone, or replaced by another file of the same name, it is left as it is.
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("made");
        let (made, _) = Created::new(&path)?;
        made.remove().map_err(|left| left.to_string())?;
        assert!(!path.exists());

        let (made, file) = Created::new(&path)?;
        drop(file);
        std::fs::remove_file(&path)?;
        std::fs::write(&path, "another's")?;
        made.remove().map_err(|left| left.to_string())?;
        assert_eq!(std::fs::read_to_string(&path)?, "another's");

        let taken = Created::new(&path).map(|_| ()).map_err(|e| e.kind());
        assert_eq!(taken, Err(io::ErrorKind::AlreadyExists));
        assert_eq!(std::fs::read_to_string(&path)?, "another's");

        let (made, _) = Created::new(&dir.path().join("gone"))?;
        std::fs::remove_file(dir.path().join("gone"))?;
        made.remove().map_err(|left| left.to_string())?;
        Ok(())
    }
}
