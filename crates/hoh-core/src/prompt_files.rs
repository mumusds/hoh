//! Override filesystem operations. Policy is in `prompt`; no host or database dependency.

use crate::prompt::{self, Error};
use std::io::Write;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

static SAVES: AtomicU64 = AtomicU64::new(0);

/// Validate before opening any writable file; stage beside the destination and rename only after syncing.
/// A failed staged write is retained at the exact path named in the error, never mistaken for a saved override.
pub fn save_override(root: &Path, relative: &str, text: &str) -> Result<(), Error> {
    prompt::validate_size(text)?;
    prompt::validate_variables(text)?;
    let path = Path::new(relative);
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(prompt::unavailable(
            "override path must be relative without dot components",
        ));
    }
    let root = root.canonicalize().map_err(prompt::unavailable)?;
    let mut target = root.clone();
    for component in path.components() {
        target.push(component);
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(prompt::unavailable(
                    "override path contains a symbolic link",
                ));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(prompt::unavailable(e)),
        }
    }
    if let Ok(meta) = std::fs::metadata(&target)
        && !meta.is_file()
    {
        return Err(prompt::unavailable("override is not a regular file"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| prompt::unavailable("override has no parent"))?;
    std::fs::create_dir_all(parent).map_err(prompt::unavailable)?;
    if !parent
        .canonicalize()
        .map_err(prompt::unavailable)?
        .starts_with(&root)
    {
        return Err(prompt::unavailable("override parent leads outside root"));
    }
    let (staged, mut file) = loop {
        let staged = parent.join(format!(
            ".prompt-save-{}-{}",
            std::process::id(),
            SAVES.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::File::create_new(&staged) {
            Ok(file) => break (staged, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(prompt::unavailable(e)),
        }
    };
    let failure = |e: std::io::Error| prompt::unavailable(format!("{}: {e}", staged.display()));
    file.write_all(text.as_bytes()).map_err(failure)?;
    file.sync_all().map_err(failure)?;
    std::fs::rename(&staged, &target).map_err(failure)?;
    std::fs::File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(prompt::unavailable)
}
