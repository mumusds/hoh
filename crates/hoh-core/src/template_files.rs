//! Publish the prepared documents without replacing user files (ADR-040).
use crate::{
    prompt::{Error, unavailable},
    templates::{Inputs, Templates},
};
use rustix::fs::{self, AtFlags, FileType, Mode, OFlags};
use serde::Serialize;
use std::{
    io::Write,
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    path::Path,
};

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
    publish_with(root, path, text, |old, source, dir, name, flags| {
        fs::linkat(old, source, dir, name, flags)
    })
}

fn publish_with(
    root: &std::os::fd::OwnedFd,
    path: &str,
    text: &str,
    link: impl FnOnce(BorrowedFd<'_>, &str, BorrowedFd<'_>, &str, AtFlags) -> rustix::io::Result<()>,
) -> std::io::Result<bool> {
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
    // Linux before 6.10 requires CAP_DAC_READ_SEARCH for AT_EMPTY_PATH.
    // Follow the proc fd link instead, as documented for unprivileged O_TMPFILE publication.
    let source = format!("/proc/self/fd/{}", file.as_raw_fd());
    let created = match link(fs::CWD, &source, dir.as_fd(), name, AtFlags::SYMLINK_FOLLOW) {
        Ok(()) => true,
        Err(rustix::io::Errno::EXIST) => {
            present(&dir, name)?;
            false
        }
        Err(e) => {
            return Err(std::io::Error::new(
                std::io::Error::from(e).kind(),
                format!("anonymous template publication via /proc/self/fd unavailable: {e}"),
            ));
        }
    };
    fs::fsync(&dir)?;
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn publication_uses_proc_fd_without_empty_path_capability() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::open(
            temp.path(),
            OFlags::RDONLY | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        let created = publish_with(
            &root,
            "doc.md",
            "complete document",
            |old, source, dir, name, flags| {
                assert_eq!(old.as_raw_fd(), fs::CWD.as_raw_fd());
                assert!(source.starts_with("/proc/self/fd/"), "{source}");
                assert_eq!(flags, AtFlags::SYMLINK_FOLLOW);
                assert_eq!(
                    std::fs::read_to_string(source).unwrap(),
                    "complete document"
                );
                fs::linkat(old, source, dir, name, flags)
            },
        )?;
        assert!(created);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("doc.md"))?,
            "complete document"
        );
        assert_eq!(std::fs::read_dir(temp.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn unavailable_proc_publication_fails_with_reason_and_no_named_file() -> std::io::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::open(
            temp.path(),
            OFlags::RDONLY | OFlags::DIRECTORY,
            Mode::empty(),
        )?;
        let error = publish_with(&root, "doc.md", "complete document", |_, _, _, _, _| {
            Err(rustix::io::Errno::NOENT)
        })
        .unwrap_err();
        assert!(error.to_string().contains("/proc/self/fd"), "{error}");
        assert_eq!(std::fs::read_dir(temp.path())?.count(), 0);
        Ok(())
    }
}
