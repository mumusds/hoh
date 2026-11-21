//! The project's `.hoh/` directory, HoH's own state, which git always ignores (docs/adr/ADR-027.md decision 3). Every
//! place that makes it calls [`ensure`]: the directory is made, and when it has no `.gitignore` one holding `*` is
//! written. That file is inside `.hoh/` itself, so it anchors to this one directory only (in a linked worktree or a
//! submodule as well), changes no tracked file and touches nothing under `.git`. One already there, whether the user
//! edited it or not, is left as it is; the freeze guard names each HoH path it no longer keeps out.

use std::io::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// What `.hoh/.gitignore` holds when HoH writes it.
pub const IGNORE_ALL: &str = "*\n";

/// Names this process's staged copies apart.
static STAGED: AtomicU64 = AtomicU64::new(0);

/// Makes `dir` (a project's `.hoh/`) and, if it has none, its `.gitignore` holding [`IGNORE_ALL`]. The file is written
/// whole to a name of its own, synced, then linked to `.gitignore`: the name never shows a part of it, and a file put
/// there meanwhile by another process or the user is kept. An optional diagnostic says why an existing owned
/// directory's mode could not be tightened; it never prevents access. The host reports it through its own log,
/// and input history through its notice; this shared library does not write to the client's stderr (ADR-031).
pub fn ensure(dir: &Path) -> std::io::Result<Option<String>> {
    // A newly visible directory never grants access beyond its owner. Legacy directory hardening is best effort:
    // a read-only mount must still reach the original host.lock diagnostic (ADR-016, CLN2-55).
    create_private(dir)?;
    let diagnostic = tighten(dir, rustix::process::geteuid().as_raw())
        .err()
        .map(|e| format!("cannot tighten {}: {e}", dir.display()));
    let target = dir.join(".gitignore");
    match std::fs::symlink_metadata(&target) {
        Ok(_) => return Ok(diagnostic),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let staged = dir.join(format!(
        ".gitignore.{}.{}",
        std::process::id(),
        STAGED.fetch_add(1, Ordering::Relaxed)
    ));
    let placed = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(IGNORE_ALL.as_bytes())?;
        file.sync_all()?;
        match std::fs::hard_link(&staged, &target) {
            Ok(()) => std::fs::File::open(dir)?.sync_all(),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            Err(e) => Err(e),
        }
    })();
    let _ = std::fs::remove_file(&staged);
    placed.map(|()| diagnostic)
}

/// Tighten only a real directory owned by `uid`. Hold a no-follow descriptor through the metadata check and chmod:
/// replacing the path with a symlink cannot redirect the mode change to its target.
fn tighten(dir: &Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
    let metadata = std::fs::symlink_metadata(dir)?;
    if !metadata.is_dir() || metadata.uid() != uid {
        return Ok(());
    }
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::NOFOLLOW).bits() as i32)
        .open(dir)?;
    let metadata = directory.metadata()?;
    if metadata.uid() != uid {
        return Ok(());
    }
    let mode = metadata.permissions().mode();
    let private = mode & 0o700;
    // Remove access, never add owner permissions that the user (or umask) deliberately withheld.
    if mode & 0o7777 != private {
        directory.set_permissions(std::fs::Permissions::from_mode(private))?;
    }
    Ok(())
}

/// The mkdir itself has a private mode; checked separately before any later chmod or state write.
fn create_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(test)]
mod tests;
