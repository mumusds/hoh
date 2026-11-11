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
/// there meanwhile by another process or the user is kept.
pub fn ensure(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let target = dir.join(".gitignore");
    match std::fs::symlink_metadata(&target) {
        Ok(_) => return Ok(()),
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
    placed
}

#[cfg(test)]
mod tests;
