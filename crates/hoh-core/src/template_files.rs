//! Publish the prepared documents without replacing user files (ADR-040).
use crate::{
    prompt::{Error, unavailable},
    templates::{Inputs, Templates},
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use serde::Serialize;
use std::{io::Write, path::Path};

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub profile: String,
    pub source: String,
}
impl Selection {
    /// Older snapshots predate document selection and use the unchanged full default.
    pub fn from_snapshot(snapshot: &serde_json::Value) -> Result<Self, Error> {
        let project = &snapshot["project"];
        let result = if project.is_null() {
            Self {
                profile: "full".into(),
                source: "default".into(),
            }
        } else {
            Self {
                profile: project["document_profile"]
                    .as_str()
                    .ok_or_else(|| unavailable("snapshot is missing document_profile"))?
                    .into(),
                source: project["document_profile_source"]
                    .as_str()
                    .ok_or_else(|| unavailable("snapshot is missing document_profile_source"))?
                    .into(),
            }
        };
        result.validate()?;
        Ok(result)
    }
    fn validate(&self) -> Result<(), Error> {
        if !["lite", "standard", "full"].contains(&self.profile.as_str())
            || !["project", "default"].contains(&self.source.as_str())
        {
            return Err(unavailable("invalid document profile or source"));
        }
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub profile: String,
    pub source: String,
    pub created: Vec<String>,
    pub skipped: Vec<String>,
}

/// Validate the complete set before publishing. On I/O failure, already published complete files remain;
/// a retry preserves them. The caller owns the Attempt lease and effect (the S6 drafting hook).
pub fn instantiate(
    root: &Path,
    templates: &Templates,
    selection: &Selection,
    inputs: &Inputs,
) -> Result<Outcome, Error> {
    selection.validate()?;
    let files = templates.render(&selection.profile, inputs)?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let root = fs::open(root, flags, Mode::empty()).map_err(unavailable)?;
    let mut outcome = Outcome {
        profile: selection.profile.clone(),
        source: selection.source.clone(),
        created: Vec::new(),
        skipped: Vec::new(),
    };
    for (path, text) in files {
        let result =
            publish(&root, &path, &text).map_err(|e| unavailable(format!("{path}: {e}")))?;
        if result {
            outcome.created.push(path);
        } else {
            outcome.skipped.push(path);
        }
    }
    Ok(outcome)
}
fn present(dir: &std::os::fd::OwnedFd, name: &str) -> std::io::Result<bool> {
    match fs::statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile => Ok(true),
        Ok(_) => Err(std::io::Error::other("document is not a regular file")),
        Err(rustix::io::Errno::NOENT) => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn publish(root: &std::os::fd::OwnedFd, path: &str, text: &str) -> std::io::Result<bool> {
    let mut parts: Vec<_> = path.split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err(std::io::Error::other("invalid document path"));
    }
    let name = parts
        .pop()
        .ok_or_else(|| std::io::Error::other("empty document path"))?;
    let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = fs::openat(root, ".", flags, Mode::empty())?;
    for part in parts {
        match fs::mkdirat(&dir, part, Mode::from_raw_mode(0o755)) {
            Ok(()) => fs::fsync(&dir)?,
            Err(rustix::io::Errno::EXIST) => (),
            Err(e) => return Err(e.into()),
        }
        dir = fs::openat(&dir, part, flags, Mode::empty())?;
    }
    if present(&dir, name)? {
        fs::fsync(&dir)?;
        return Ok(false);
    }
    // No directory entry exists until the complete, synced inode is published.
    // Unsupported filesystems fail closed; a named fallback would leave crash debris.
    let fd = fs::openat(
        &dir,
        ".",
        OFlags::WRONLY | OFlags::TMPFILE | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o644),
    )
    .map_err(|e| {
        std::io::Error::new(
            std::io::Error::from(e).kind(),
            format!("anonymous template staging (O_TMPFILE) unavailable: {e}"),
        )
    })?;
    let mut file = std::fs::File::from(fd);
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    let created = match fs::linkat(&file, "", &dir, name, AtFlags::EMPTY_PATH) {
        Ok(()) => true,
        Err(rustix::io::Errno::EXIST) => {
            present(&dir, name)?;
            false
        }
        Err(e) => return Err(e.into()),
    };
    fs::fsync(&dir)?;
    Ok(created)
}
