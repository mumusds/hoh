//! Override filesystem operations. Policy is in `prompt`; no host or database dependency.

use crate::prompt::{self, Error};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SAVES: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
mod tests;

/// The global HoH configuration root. Reading this does not create or modify a user directory.
pub fn global_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("hoh"))
}

/// Read each Attempt's directory chain afresh. A configured project file replaces the conventional project file.
pub fn load(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    role: &str,
    configured: Option<&str>,
) -> Result<prompt::Resolved, Error> {
    use crate::config::{self, Contract};
    let builtin = configured
        .and_then(|p| p.strip_prefix(config::BUILTIN_PROMPT))
        .unwrap_or(role);
    let mut sources = Vec::new();
    if let Some(text) = config::builtin_contract(builtin) {
        sources.push(Contract {
            layer: "builtin",
            path: format!("resources/prompts/{builtin}.md"),
            text: text.to_owned(),
        });
    } else if configured.is_none()
        || configured.is_some_and(|p| p.starts_with(config::BUILTIN_PROMPT))
    {
        return Err(prompt::unavailable(format!(
            "{builtin} has no built-in role contract"
        )));
    }
    let role = crate::layout::component(role);
    let run = crate::layout::component(run);
    if [role.as_str(), run.as_str()]
        .iter()
        .any(|s| s.is_empty() || *s == "." || *s == "..")
    {
        return Err(prompt::unavailable("invalid role or run path component"));
    }
    if let Some(global) = global {
        let relative = format!("prompts/{role}.md");
        if let Some(text) = read_optional(global, &relative)? {
            sources.push(Contract {
                layer: "global",
                path: global.join(relative).display().to_string(),
                text,
            });
        }
    }
    let explicit = configured.filter(|p| !p.starts_with(config::BUILTIN_PROMPT));
    let conventional = format!(".hoh/prompts/{role}.md");
    let project = explicit.unwrap_or(&conventional);
    match read_optional(root, project)? {
        Some(text) => sources.push(Contract {
            layer: "project",
            path: project.to_owned(),
            text,
        }),
        None if explicit.is_some() => {
            return Err(prompt::unavailable(format!(
                "{project}: configured contract is missing"
            )));
        }
        None => (),
    }
    let relative = format!(".hoh/runs/{run}/prompts/{role}.md");
    if let Some(text) = read_optional(root, &relative)? {
        sources.push(Contract {
            layer: "run",
            path: relative,
            text,
        });
    }
    prompt::resolve(sources)
}

/// Missing is the only condition that means an absent layer. Check existing ancestors, including links, first.
fn optional_file(root: &Path, relative: &str) -> Result<Option<PathBuf>, Error> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(prompt::unavailable(
            "override path must be relative without ..",
        ));
    }
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(prompt::unavailable(e)),
        Ok(_) => (),
    }
    let root = root.canonicalize().map_err(prompt::unavailable)?;
    let mut path = root.clone();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(prompt::unavailable(e)),
            Ok(_) => (),
        }
        if !path
            .canonicalize()
            .map_err(prompt::unavailable)?
            .starts_with(&root)
        {
            return Err(prompt::unavailable(format!(
                "{} leads outside root",
                path.display()
            )));
        }
    }
    if !std::fs::metadata(&path)
        .map_err(prompt::unavailable)?
        .is_file()
    {
        return Err(prompt::unavailable(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(Some(path))
}

fn read_optional(root: &Path, relative: &str) -> Result<Option<String>, Error> {
    optional_file(root, relative)?
        .map(|path| {
            std::fs::read_to_string(&path)
                .map_err(|e| prompt::unavailable(format!("{}: {e}", path.display())))
        })
        .transpose()
}

/// References only: document text is never interpreted as evidence or template variables.
pub fn project_references(root: &Path) -> Result<serde_json::Value, Error> {
    use serde_json::json;
    let reference = |relative: &str| -> Result<Option<serde_json::Value>, Error> {
        optional_file(root, relative)?
            .map(|path| {
                let bytes = std::fs::read(&path).map_err(prompt::unavailable)?;
                Ok(json!({"path": relative, "sha256": crate::context::sha256_hex(&bytes)}))
            })
            .transpose()
    };
    let dir = root.join("docs/adr");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(prompt::unavailable(e)),
    };
    let mut paths = Vec::new();
    if let Some(entries) = entries {
        for entry in entries {
            let entry = entry.map_err(prompt::unavailable)?;
            if entry.path().extension().is_some_and(|e| e == "md") {
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| prompt::unavailable("ADR path is not UTF-8"))?;
                paths.push(format!("docs/adr/{name}"));
            }
        }
    }
    paths.sort();
    let mut adrs = Vec::new();
    for path in paths {
        adrs.push(reference(&path)?.ok_or_else(|| {
            prompt::unavailable(format!("{path} disappeared while collecting references"))
        })?);
    }
    Ok(json!({"adrs": adrs, "conventions": reference(".hoh/conventions.md")?}))
}

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

/// Restore only the conventional project override owned by this product. No glob, recursive removal or configured
/// arbitrary file deletion. Directory handles and NOFOLLOW prevent a swapped ancestor from redirecting the unlink.
pub fn restore_project(root: &Path, role: &str, configured: Option<&str>) -> Result<bool, Error> {
    use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
    if configured.is_some_and(|p| !p.starts_with(crate::config::BUILTIN_PROMPT)) {
        return Err(prompt::unavailable(
            "配置指定了文件；请先移除 prompt 配置再恢复约定路径",
        ));
    }
    if crate::config::builtin_contract(
        configured
            .and_then(|p| p.strip_prefix(crate::config::BUILTIN_PROMPT))
            .unwrap_or(role),
    )
    .is_none()
    {
        return Err(prompt::unavailable("该职位没有内置默认契约"));
    }
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = fs::open(root, flags, Mode::empty()).map_err(prompt::unavailable)?;
    for name in [".hoh", "prompts"] {
        dir = match fs::openat(&dir, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(rustix::io::Errno::NOENT) => return Ok(false),
            Err(e) => return Err(prompt::unavailable(e)),
        };
    }
    let name = format!("{}.md", crate::layout::component(role));
    let stat = match fs::statat(&dir, &name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(rustix::io::Errno::NOENT) => return Ok(false),
        Err(e) => return Err(prompt::unavailable(e)),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(prompt::unavailable("覆盖不是普通文件，未删除"));
    }
    fs::unlinkat(&dir, &name, AtFlags::empty()).map_err(prompt::unavailable)?;
    fs::fsync(&dir).map_err(prompt::unavailable)?;
    Ok(true)
}
