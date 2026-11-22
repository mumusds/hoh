//! Project discovery's pure parts (REQ-028; docs/adr/ADR-039.md decisions 4, 7 and 8): what `git status` says is
//! uncommitted, which build and test commands the project's conventional files name, which documents it has, whether a
//! task's path scope meets an uncommitted file, and which failing tests a baseline already had. No git, no file, no
//! process: the host reads and runs, these decide.

use std::collections::BTreeSet;

/// The paths `git status --porcelain=v1 -z --untracked-files=all` lists: tracked ones with a change (staged or not;
/// a rename's new path), and untracked ones, each sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub dirty: Vec<String>,
    pub untracked: Vec<String>,
}

/// [`Status`] of the NUL-separated porcelain v1 output `out` (`XY path`, a rename or copy followed by its old path as
/// the next entry; ignored entries `!!` are not listed).
pub fn status(out: &[u8]) -> Status {
    let mut dirty = BTreeSet::new();
    let mut untracked = BTreeSet::new();
    let mut entries = out.split(|b| *b == 0).filter(|e| !e.is_empty());
    while let Some(entry) = entries.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = (
            &entry[..2],
            String::from_utf8_lossy(&entry[3..]).into_owned(),
        );
        match code {
            b"??" => {
                untracked.insert(path);
            }
            b"!!" => {}
            _ => {
                if code.contains(&b'R') || code.contains(&b'C') {
                    // The old path of a rename or copy: not a file of the work tree under this name.
                    entries.next();
                }
                dirty.insert(path);
            }
        }
    }
    Status {
        dirty: dirty.into_iter().collect(),
        untracked: untracked.into_iter().collect(),
    }
}

/// The text of the project's conventional files at its root (none: absent or not a regular file).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conventions {
    pub cargo_toml: Option<String>,
    pub package_json: Option<String>,
    pub makefile: Option<String>,
    pub pyproject_toml: Option<String>,
}

/// The marker of a side with no command detected (C10).
pub const UNKNOWN: &str = "unknown";

/// The build and test commands `files` name by convention (docs/adr/ADR-039.md decision 4), every convention found,
/// in this order: Cargo, npm, Make, pytest; a side with none is `["unknown"]`.
pub fn commands(files: &Conventions) -> (Vec<String>, Vec<String>) {
    let mut build = Vec::new();
    let mut test = Vec::new();
    if files.cargo_toml.is_some() {
        build.push("cargo build".to_owned());
        test.push("cargo test".to_owned());
    }
    if let Some(scripts) = files
        .package_json
        .as_deref()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
        .and_then(|v| v.get("scripts").cloned())
    {
        if scripts
            .get("build")
            .is_some_and(serde_json::Value::is_string)
        {
            build.push("npm run build".to_owned());
        }
        if scripts
            .get("test")
            .is_some_and(serde_json::Value::is_string)
        {
            test.push("npm test".to_owned());
        }
    }
    if let Some(makefile) = &files.makefile {
        let targets: BTreeSet<&str> = makefile
            .lines()
            .filter(|l| !l.starts_with(['\t', ' ', '#', '.']))
            .filter_map(|l| l.split_once(':').map(|(t, _)| t))
            .flat_map(str::split_whitespace)
            .collect();
        if targets.contains("build") {
            build.push("make build".to_owned());
        } else if targets.contains("all") {
            build.push("make".to_owned());
        }
        if targets.contains("test") {
            test.push("make test".to_owned());
        }
    }
    if files.pyproject_toml.as_deref().is_some_and(|t| {
        t.lines()
            .any(|l| l.trim_start().starts_with("[tool.pytest"))
    }) {
        test.push("pytest".to_owned());
    }
    for side in [&mut build, &mut test] {
        if side.is_empty() {
            side.push(UNKNOWN.to_owned());
        }
    }
    (build, test)
}

/// The project's documents by common location (REQ-028 behaviour 6): its root's `README*` files (`root` lists the root's
/// regular files) and those of the directories `docs/`, `docs/adr/` and `adr/` that exist (`dirs`), sorted.
pub fn docs(root: &[String], dirs: &[&str]) -> Vec<String> {
    let mut out: BTreeSet<String> = root
        .iter()
        .filter(|f| f.starts_with("README"))
        .cloned()
        .collect();
    out.extend(dirs.iter().map(|d| format!("{}/", d.trim_end_matches('/'))));
    out.into_iter().collect()
}

#[cfg(test)]
mod tests;
