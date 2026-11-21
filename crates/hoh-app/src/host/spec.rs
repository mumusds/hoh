//! The host owns specification files; the core only receives bytes (ADR-033).

use hoh_core::specindex::{IndexError, Source};
use std::path::Path;

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
