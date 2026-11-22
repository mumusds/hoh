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

/// How many commits back a review's commit is looked for (docs/adr/ADR-035.md decision 5).
const REVIEW_DEPTH: &str = "100";

/// The commit of the project at `root` whose specification is the approved revision of digest `sha256`
/// (docs/adr/ADR-035.md decision 5): along `git rev-list HEAD -- docs/spec`, newest first, at most 100 back, the first
/// whose `docs/spec/*.md` regenerate an index of that digest and whose committed `index.json` holds those very
/// bytes. `Ok(Err(head))`: none is (the HEAD looked from, `none` without one). Blocking.
pub(crate) fn review_commit(root: &Path, sha256: &str) -> Result<Result<String, String>, String> {
    let git = |args: &[&str]| super::control::git(root, args);
    let Ok(head) = git(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]) else {
        return Ok(Err("none".to_owned()));
    };
    let head = String::from_utf8_lossy(&head).trim().to_owned();
    let listed = git(&[
        "rev-list",
        "--max-count",
        REVIEW_DEPTH,
        &head,
        "--",
        "docs/spec",
    ])?;
    for commit in String::from_utf8_lossy(&listed).lines() {
        let names = git(&["ls-tree", "-z", "--name-only", commit, "docs/spec/"])?;
        let mut sources = Vec::new();
        for path in names.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            let Some(name) = path.strip_prefix("docs/spec/") else {
                continue;
            };
            if !name.ends_with(".md") || name.contains('/') {
                continue;
            }
            let bytes = git(&["cat-file", "blob", &format!("{commit}:{path}")])?;
            sources.push(Source { path, bytes });
        }
        let Ok(index) = hoh_core::specindex::generate_index(&sources) else {
            continue;
        };
        if index.sha256() != sha256 {
            continue;
        }
        let committed = git(&[
            "cat-file",
            "blob",
            &format!("{commit}:{}", hoh_core::specindex::PATH),
        ]);
        if committed.is_ok_and(|bytes| bytes == index.bytes()) {
            return Ok(Ok(commit.to_owned()));
        }
    }
    Ok(Err(head))
}

#[cfg(test)]
mod tests;

/// Read the specification from exactly this commit, never substituted working-tree bytes.
pub(crate) fn committed(
    root: &Path,
    commit: &str,
) -> Result<hoh_core::persistence::SpecFiles, String> {
    let rows = super::control::git(root, &["ls-tree", "-r", "-z", commit, "--", "docs/spec/"])?;
    let mut sources = Vec::new();
    for row in rows.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let tab = row
            .iter()
            .position(|b| *b == b'\t')
            .ok_or("invalid Git tree row")?;
        let path = std::str::from_utf8(&row[tab + 1..]).map_err(|e| e.to_string())?;
        let name = path
            .strip_prefix("docs/spec/")
            .ok_or("unexpected specification path")?;
        if name.contains('/') || !name.ends_with(".md") {
            continue;
        }
        if !row.starts_with(b"100644 ") && !row.starts_with(b"100755 ") {
            return Err(format!("committed Markdown is not a regular file: {path}"));
        }
        let bytes = super::control::git(root, &["cat-file", "blob", &format!("{commit}:{path}")])?;
        sources.push(Source {
            path: path.into(),
            bytes,
        });
    }
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let index = super::control::git(
        root,
        &[
            "cat-file",
            "blob",
            &format!("{commit}:{}", hoh_core::specindex::PATH),
        ],
    )?;
    Ok(hoh_core::persistence::SpecFiles {
        sources,
        existing_index: Some(index.clone()),
        tracked_index: Some(index),
    })
}
