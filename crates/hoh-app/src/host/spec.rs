//! The host owns specification files; the core only receives bytes (ADR-033).

use hoh_core::specindex::{IndexError, Source};
use std::path::Path;

pub(crate) fn files(root: &Path) -> Result<hoh_core::persistence::SpecFiles, IndexError> {
    let sources = sources(root)?;
    let path = hoh_core::specindex::PATH;
    let tracked = super::control::git(root, &["ls-files", "-z", "--", path])
        .map_err(|e| IndexError::at(path, 1, e))?;
    let tracked_index = if tracked.is_empty() {
        None
    } else {
        match std::fs::symlink_metadata(root.join(path)) {
            Ok(meta) if meta.is_file() => Some(
                std::fs::read(root.join(path))
                    .map_err(|e| IndexError::at(path, 1, e.to_string()))?,
            ),
            Ok(_) => {
                return Err(IndexError::at(
                    path,
                    1,
                    "index must be a regular file, not a link",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(IndexError::at(path, 1, e.to_string())),
        }
    };
    Ok(hoh_core::persistence::SpecFiles {
        sources,
        tracked_index,
    })
}

/// The direct Markdown children of docs/spec, without following directory or file links.
pub(crate) fn sources(root: &Path) -> Result<Vec<Source>, IndexError> {
    for path in ["docs", "docs/spec"] {
        let meta = std::fs::symlink_metadata(root.join(path))
            .map_err(|e| IndexError::at(path, 1, e.to_string()))?;
        if !meta.is_dir() {
            return Err(IndexError::at(path, 1, "expected a directory, not a link"));
        }
    }
    let mut sources = Vec::new();
    for entry in std::fs::read_dir(root.join("docs/spec"))
        .map_err(|e| IndexError::at("docs/spec", 1, e.to_string()))?
    {
        let entry = entry.map_err(|e| IndexError::at("docs/spec", 1, e.to_string()))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| IndexError::at("docs/spec", 1, "non-UTF-8 filename"))?;
        if !name.ends_with(".md") {
            continue;
        }
        let path = format!("docs/spec/{name}");
        let kind = entry
            .file_type()
            .map_err(|e| IndexError::at(&path, 1, e.to_string()))?;
        if !kind.is_file() {
            return Err(IndexError::at(
                &path,
                1,
                "expected a regular Markdown file, not a link",
            ));
        }
        let bytes =
            std::fs::read(entry.path()).map_err(|e| IndexError::at(&path, 1, e.to_string()))?;
        sources.push(Source { path, bytes });
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(sources)
}
