//! The compat suite's definition digest (docs/adr/ADR-030.md decision 10, "套件集摘要"): the tree digest, at compile
//! time, of this crate's `src/**` (the request builders, the suite, the probe's validator, the adapters' replays, the
//! embedded fixtures), its Cargo.toml, the workspace's Cargo.lock and the frame registries the adapters build in from
//! tests/fixtures/protocol (`src/known.rs`): sha256 over the sorted lines `<path in the workspace> <sha256>\n`, the
//! probe's tree digest (`probe::members_sha256`). It reaches the crate as `HOH_AGENTS_DEFINITION_SHA256`, the files as
//! `HOH_AGENTS_DEFINITION_FILES` (joined by `;`), so a build that changes how requests are built, checked or replayed
//! never accepts a PASS of another.

use sha2::{Digest, Sha256};
use std::error::Error;
use std::path::{Path, PathBuf};

/// The frame registries src/known.rs builds in.
const REGISTRIES: [&str; 3] = [
    "tests/fixtures/protocol/claude/2.1.318/known-frames.json",
    "tests/fixtures/protocol/codex/0.166.0/known-frames.json",
    "tests/fixtures/protocol/pi/1.5.0/known-frames.json",
];

/// Every file under `dir`, recursively.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Box<dyn Error>> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            files_under(&entry.path(), out)?;
        } else if kind.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let manifest =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").ok_or("no CARGO_MANIFEST_DIR")?);
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("the crate is not two levels under the workspace")?
        .to_path_buf();
    let src = manifest.join("src");
    let mut files = Vec::new();
    files_under(&src, &mut files)?;
    files.push(manifest.join("Cargo.toml"));
    files.push(workspace.join("Cargo.lock"));
    files.extend(REGISTRIES.iter().map(|r| workspace.join(r)));
    let mut lines = Vec::new();
    let mut names = Vec::new();
    for file in &files {
        let name = file
            .strip_prefix(&workspace)?
            .components()
            .map(|c| {
                c.as_os_str()
                    .to_str()
                    .ok_or("a file name that is not UTF-8")
            })
            .collect::<Result<Vec<_>, _>>()?
            .join("/");
        if name.contains([';', '\n', ' ']) {
            return Err(format!(
                "{name:?}: a definition file's name has `;`, a space or a line break"
            )
            .into());
        }
        let digest: String = Sha256::digest(std::fs::read(file)?)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        lines.push(format!("{name} {digest}\n"));
        names.push(name);
        println!("cargo::rerun-if-changed={}", file.display());
    }
    // A file added under src/ changes the directory.
    println!("cargo::rerun-if-changed={}", src.display());
    lines.sort();
    names.sort();
    let digest: String = Sha256::digest(lines.concat().as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    println!("cargo::rustc-env=HOH_AGENTS_DEFINITION_SHA256={digest}");
    println!(
        "cargo::rustc-env=HOH_AGENTS_DEFINITION_FILES={}",
        names.join(";")
    );
    Ok(())
}
