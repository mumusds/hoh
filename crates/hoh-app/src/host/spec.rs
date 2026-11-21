//! The host owns specification files; the core only receives bytes (ADR-033).

use hoh_core::specindex::{IndexError, Source};
use std::path::Path;

pub(crate) fn files(root: &Path) -> Result<hoh_core::persistence::SpecFiles, IndexError> {
    let sources = sources(root)?;
    let path = hoh_core::specindex::PATH;
    let tracked = super::control::git(root, &["ls-files", "-z", "--", path])
        .map_err(|e| IndexError::at(path, 1, e))?;
    let existing_index = match std::fs::symlink_metadata(root.join(path)) {
        Ok(meta) if meta.is_file() => Some(
            std::fs::read(root.join(path)).map_err(|e| IndexError::at(path, 1, e.to_string()))?,
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
    };
    Ok(hoh_core::persistence::SpecFiles {
        sources,
        tracked_index: (!tracked.is_empty())
            .then(|| existing_index.clone())
            .flatten(),
        existing_index,
    })
}

/// Publish a successfully proposed index atomically. A replay can finish a publication interrupted after the DB
/// commit; source files or an index edited since the snapshot are never overwritten by that old snapshot.
pub(crate) fn publish(
    root: &Path,
    snapshot: &hoh_core::persistence::SpecFiles,
) -> Result<(), String> {
    use std::io::Write as _;
    let current = files(root).map_err(|e| e.to_string())?;
    if current.sources != snapshot.sources {
        return Err("Markdown changed while proposing the specification".into());
    }
    let index =
        hoh_core::specindex::generate_index(&snapshot.sources).map_err(|e| e.to_string())?;
    if current.existing_index.as_deref() == Some(index.bytes()) {
        return Ok(());
    }
    if current.existing_index != snapshot.existing_index {
        return Err("index.json changed while proposing the specification".into());
    }
    let pending = root.join(".hoh/pending");
    std::fs::create_dir_all(&pending).map_err(|e| e.to_string())?;
    let temp = pending.join(format!("spec-index-{}.json", super::new_id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| -> std::io::Result<()> {
        file.write_all(index.bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, root.join(hoh_core::specindex::PATH))?;
        std::fs::File::open(root.join("docs/spec"))?.sync_all()
    })();
    if let Err(error) = result {
        match std::fs::remove_file(&temp) {
            Ok(()) => {}
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => {}
            Err(cleanup) => {
                return Err(format!(
                    "index publication: {error}; temporary file {}: {cleanup}",
                    temp.display()
                ));
            }
        }
        return Err(format!("index publication: {error}"));
    }
    Ok(())
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
