//! The project's environment description (docs/adr/ADR-028.md decision 5, the hand-over of docs/adr/ADR-027.md decision
//! 4): `.hoh/environment.json`, the environment a candidate is frozen with. Byte-stable: only facts about the
//! environment, no time and nothing random, keys sorted, written only when its content changes, so a host restart does
//! not change its digest and the checks are not refused for it (failure mode 3).

use std::path::{Path, PathBuf};

/// Where it is, relative to the project.
pub const PATH: &str = ".hoh/environment.json";

/// Its bytes for git's `git --version` text `git`.
pub fn describe(git: &str) -> Vec<u8> {
    let value = serde_json::json!({
        "schema_version": "1.0",
        "hoh": {"version": env!("CARGO_PKG_VERSION"), "commit": option_env!("HOH_BUILD_COMMIT").unwrap_or("unknown")},
        "os": {"sysname": std::env::consts::OS, "machine": std::env::consts::ARCH},
        "tools": {"git": git.trim()},
    });
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes
}

/// Make sure `project`'s description says this environment: written (atomically) only when absent or different;
/// `.hoh/` is made ignored first. The path written, or why not.
pub fn ensure(project: &Path) -> Result<PathBuf, String> {
    let state = project.join(".hoh");
    hoh_core::state_dir::ensure(&state).map_err(|e| format!("{}: {e}", state.display()))?;
    let git = crate::host::control::git(project, &["--version"])?;
    let want = describe(&String::from_utf8_lossy(&git));
    let path = project.join(PATH);
    // Read only as a regular file, never waiting on a FIFO or a device someone put there (docs/adr/ADR-027.md decision 8).
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(_) => {
            if hoh_core::objects::read_regular(&path).is_ok_and(|have| have == want) {
                return Ok(path);
            }
        }
    }
    let mut random = [0u8; 8];
    rustix::rand::getrandom(&mut random, rustix::rand::GetRandomFlags::empty())
        .map_err(|e| format!("random staged name: {e}"))?;
    let staged = state.join(format!(
        ".environment.json.{}.tmp",
        random
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    ));
    let written = (|| {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)?;
        file.write_all(&want)?;
        file.sync_all()?;
        std::fs::rename(&staged, &path)?;
        std::fs::File::open(&state)?.sync_all()
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("{}: {e}", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests;
