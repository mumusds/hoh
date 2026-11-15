//! The project's compat manifest file, `.hoh/compat-manifest.json` (ADR-015 decision 2; docs/adr/ADR-030.md decision
//! 10, K6): only the host writes it, by publishing a compat suite's entry over the manifest it holds; [`replaced`]
//! builds the new whole manifest and checks it against the locked schema, [`replace`] puts it in place and says
//! whether it did: an error only when the old manifest is still the one in place (M2T15R-04).

use serde_json::Value;
use std::path::{Path, PathBuf};

/// The manifest's file name in the project's `.hoh/`.
pub const FILE: &str = "compat-manifest.json";

/// Where a project's compat manifest is: `<root>/.hoh/compat-manifest.json`.
pub fn path(root: &Path) -> PathBuf {
    root.join(".hoh").join(FILE)
}

/// A temporary file a publication writes the whole new manifest to before renaming it over [`path`], `tag` making it
/// the writer's own: `.hoh/.compat-manifest.json.<tag>.tmp`. The host deletes those a crash left when it opens the
/// manifest ([`is_temp`]).
pub fn temp(root: &Path, tag: &str) -> PathBuf {
    root.join(".hoh").join(format!(".{FILE}.{tag}.tmp"))
}

/// Whether `name` (a file name in `.hoh/`) is one of [`temp`]'s.
pub fn is_temp(name: &str) -> bool {
    name.strip_prefix(&format!(".{FILE}."))
        .and_then(|rest| rest.strip_suffix(".tmp"))
        .is_some_and(|tag| !tag.is_empty())
}

/// The manifest and the temporary file a publication writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Files {
    pub path: PathBuf,
    pub temp: PathBuf,
}

impl Files {
    /// The manifest of the project at `root`, and the temporary file of a publication tagged `tag`.
    pub fn of(root: &Path, tag: &str) -> Files {
        Files {
            path: path(root),
            temp: temp(root, tag),
        }
    }
}

/// The manifest `base` (none: an empty one) with `agent`'s entry replaced by `entry` and the others kept as they are,
/// generated at `generated_at`; refused unless the whole is valid against the locked schema
/// (contracts/compat-manifest.schema.json), which also refuses an agent it does not name.
pub fn replaced(
    base: Option<&Value>,
    agent: &str,
    entry: &Value,
    generated_at: &str,
) -> Result<Value, String> {
    let mut manifest = match base {
        Some(base) => base.clone(),
        None => serde_json::json!({"schema_version": "1.0", "agents": {}}),
    };
    let Some(agents) = manifest.get_mut("agents").and_then(Value::as_object_mut) else {
        return Err("the manifest it replaces has no agents object".to_owned());
    };
    agents.insert(agent.to_owned(), entry.clone());
    manifest["generated_at"] = Value::from(generated_at);
    crate::schema::compat_manifest()
        .validate(&manifest)
        .map_err(|violations| {
            let said: Vec<String> = violations
                .iter()
                .take(3)
                .map(|v| format!("{} {}", v.instance_path, v.message))
                .collect();
            format!("the new manifest is not schema-valid: {}", said.join("; "))
        })?;
    Ok(manifest)
}

/// The steps of [`replace`], at which a test fails it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The temporary file holds the new bytes.
    Written,
    /// The temporary file is synced.
    Synced,
    /// The temporary file was renamed over the manifest.
    Renamed,
}

/// A replacement done: the new manifest was renamed over the old one, so it is the one every reader of the file sees.
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Replaced {
    /// Its directory synced too.
    Synced,
    /// Its directory not synced (why): the rename may not outlive a crash of the system, after which the old manifest
    /// is back.
    Unsynced(String),
}

/// Put `bytes` in place as the manifest of `files`: what was done once they are in place; an error (why) when they are
/// not, the old manifest (or none) being in place as it was.
pub fn replace(files: &Files, bytes: &[u8]) -> Result<Replaced, String> {
    replace_at(files, bytes, &|_| Ok(()))
}

/// [`replace`], `at` called after each [`Step`] (an error there fails it as an I/O error would). The bytes go to the
/// temporary file in the manifest's own directory, which is synced, renamed over the manifest and its directory
/// synced: until the rename the old manifest is whole, after it the new one is. A failure before the rename removes
/// the temporary file and is the error; one after it (the directory's sync) leaves the new manifest in place and is
/// [`Replaced::Unsynced`].
pub fn replace_at(
    files: &Files,
    bytes: &[u8],
    at: &dyn Fn(Step) -> std::io::Result<()>,
) -> Result<Replaced, String> {
    use std::io::Write as _;
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&files.temp)?;
        file.write_all(bytes)?;
        at(Step::Written)?;
        file.sync_all()?;
        at(Step::Synced)?;
        std::fs::rename(&files.temp, &files.path)
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&files.temp);
        return Err(format!("{} was not replaced: {e}", files.path.display()));
    }
    let synced = (|| {
        at(Step::Renamed)?;
        let dir = files.path.parent().unwrap_or(Path::new("/"));
        std::fs::File::open(dir)?.sync_all()
    })();
    Ok(match synced {
        Ok(()) => Replaced::Synced,
        Err(e) => Replaced::Unsynced(format!(
            "{} was replaced but its directory was not synced: {e}",
            files.path.display()
        )),
    })
}

#[cfg(test)]
mod tests;
