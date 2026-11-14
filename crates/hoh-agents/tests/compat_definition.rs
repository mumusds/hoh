//! The compat suite's definition (docs/adr/ADR-030.md decision 10, "套件集摘要"): `compat::DEFINITION_SHA256` covers
//! every file under this crate's src/ (the request builders, the suite, the validator, the replays, the embedded
//! fixtures), its Cargo.toml, the frame registries the replays judge by and the digest of the workspace's Cargo.lock
//! (src/compat/definition/cargo-lock.sha256), each embedded as it is on disk. A file added under src/ without an entry
//! in src/compat/definition.rs fails here, and so does a Cargo.lock whose digest file was not refreshed. Refresh it,
//! from the workspace root, with:
//! `sha256sum Cargo.lock | cut -d' ' -f1 > crates/hoh-agents/src/compat/definition/cargo-lock.sha256`

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::compat::{DEFINITION_FILES, DEFINITION_SHA256, definition_files};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

type R = Result<(), Box<dyn std::error::Error>>;

/// The lock digest file, by its path in the workspace.
const LOCK_DIGEST: &str = "crates/hoh-agents/src/compat/definition/cargo-lock.sha256";

/// The refresh command of the lock digest file, run from the workspace root.
const REFRESH: &str = "sha256sum Cargo.lock | cut -d' ' -f1 > crates/hoh-agents/src/compat/definition/cargo-lock.sha256";

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sha256(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Every file under `dir`, recursively, by its path relative to `base` joined with `/`.
fn files_under(base: &Path, dir: &Path, out: &mut BTreeSet<String>) -> R {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files_under(base, &entry.path(), out)?;
        } else {
            let path = entry.path();
            let name = path
                .strip_prefix(base)?
                .components()
                .map(|c| {
                    c.as_os_str()
                        .to_str()
                        .ok_or("a file name that is not UTF-8")
                })
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            out.insert(name);
        }
    }
    Ok(())
}

#[test]
fn the_definition_embeds_every_file_under_src_as_it_is_on_disk() -> R {
    let workspace = workspace();
    let files = definition_files();
    let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
    // The listed names, sorted and unique, are DEFINITION_FILES; a name has no space or line break (it is one
    // `<name> <digest>` line of the digest).
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(names, sorted, "definition files listed twice or unsorted");
    assert_eq!(*DEFINITION_FILES, sorted);
    for name in &names {
        assert!(!name.contains([' ', '\n', ';']), "{name:?}");
    }
    // Every file under src/ on disk has an entry, and every entry under src/ is a file there.
    let mut on_disk = BTreeSet::new();
    files_under(
        &workspace,
        &workspace.join("crates/hoh-agents/src"),
        &mut on_disk,
    )?;
    let listed: BTreeSet<String> = names
        .iter()
        .filter(|name| name.starts_with("crates/hoh-agents/src/"))
        .map(|name| (*name).to_owned())
        .collect();
    let missing: Vec<&String> = on_disk.difference(&listed).collect();
    assert!(
        missing.is_empty(),
        "files under crates/hoh-agents/src without an entry in src/compat/definition.rs (or, for a fixture, \
         src/compat/fixtures.rs): {missing:?}"
    );
    let gone: Vec<&String> = listed.difference(&on_disk).collect();
    assert!(gone.is_empty(), "entries without a file: {gone:?}");
    // Beyond src/: the crate's manifest and the frame registries the replays judge by, and nothing else.
    let others: BTreeSet<&str> = names
        .iter()
        .copied()
        .filter(|name| !name.starts_with("crates/hoh-agents/src/"))
        .collect();
    assert_eq!(
        others,
        BTreeSet::from([
            "crates/hoh-agents/Cargo.toml",
            "tests/fixtures/protocol/claude/2.1.318/known-frames.json",
            "tests/fixtures/protocol/codex/0.166.0/known-frames.json",
            "tests/fixtures/protocol/pi/1.5.0/known-frames.json",
        ])
    );
    assert!(names.contains(&LOCK_DIGEST), "{LOCK_DIGEST}");
    // Each entry's bytes are the file's, and the digest is the tree digest of them: sha256 over the sorted lines
    // `<name> <sha256>\n`.
    let mut lines = Vec::new();
    for (name, bytes) in &files {
        let disk = std::fs::read(workspace.join(name))?;
        assert!(
            *bytes == disk.as_slice(),
            "{name}: the embedded bytes differ from the file"
        );
        lines.push(format!("{name} {}\n", sha256(&disk)));
    }
    lines.sort();
    assert_eq!(*DEFINITION_SHA256, sha256(lines.concat().as_bytes()));
    Ok(())
}

#[test]
fn the_lock_digest_is_the_workspace_lock() -> R {
    // The definition covers the dependencies through the digest of the workspace's Cargo.lock.
    let workspace = workspace();
    let lock = std::fs::read(workspace.join("Cargo.lock"))?;
    let recorded = std::fs::read_to_string(workspace.join(LOCK_DIGEST))?;
    assert_eq!(
        recorded,
        format!("{}\n", sha256(&lock)),
        "{LOCK_DIGEST} is not the digest of the workspace's Cargo.lock; refresh it from the workspace root: {REFRESH}"
    );
    assert!(DEFINITION_FILES.iter().any(|f| f == LOCK_DIGEST));
    Ok(())
}
