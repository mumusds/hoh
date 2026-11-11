//! The content-addressed object directory (docs/04-architecture.md; ADR-026 decision 6): `.hoh/objects/sha256/<first
//! two hex digits>/<sha256>`, each object its bytes under their own digest. Written whole and linked into place, so a
//! place only ever holds a whole object; read back only when the bytes still match their name. And the resource
//! bundle version every packed prompt records (decision 7).

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Every resource built into the binary that a packed prompt can depend on, by its path in the source tree: the
/// built-in role contracts and the schemas the output contracts come from.
pub const RESOURCES: [(&str, &str); 9] = [
    (
        "resources/prompts/commander.md",
        include_str!("../../../resources/prompts/commander.md"),
    ),
    (
        "resources/prompts/cto.md",
        include_str!("../../../resources/prompts/cto.md"),
    ),
    (
        "resources/prompts/developer.md",
        include_str!("../../../resources/prompts/developer.md"),
    ),
    (
        "resources/prompts/qa.md",
        include_str!("../../../resources/prompts/qa.md"),
    ),
    (
        "resources/prompts/researcher.md",
        include_str!("../../../resources/prompts/researcher.md"),
    ),
    (
        "resources/prompts/reviewer.md",
        include_str!("../../../resources/prompts/reviewer.md"),
    ),
    (
        "contracts/protocol.schema.json",
        include_str!("../../../contracts/protocol.schema.json"),
    ),
    (
        "contracts/independent-report.schema.json",
        include_str!("../../../contracts/independent-report.schema.json"),
    ),
    (
        "contracts/acceptance-observation.schema.json",
        include_str!("../../../contracts/acceptance-observation.schema.json"),
    ),
];

/// Where the object named `sha256` lives under the project at `project`; only a lower-case sha256 names one.
pub fn object_path(project: &Path, sha256: &str) -> Result<PathBuf, String> {
    if sha256.len() != 64
        || !sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(format!("{sha256:?} is not a lower-case sha256"));
    }
    Ok(project
        .join(".hoh/objects/sha256")
        .join(&sha256[..2])
        .join(sha256))
}

/// The counter of staged names: each staged file has its own name, this process's id and the next count, created
/// only if absent (a name a dead process of the same id left is skipped, never reused).
static STAGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Keeps `bytes` as an object and returns its sha256: written whole to a file of its own, synced, then linked to its
/// place, which fails if anything is there already; the directory synced so the link outlives a power loss. The same
/// bytes there already are taken; other bytes there (a damaged object) are replaced by this write's synced copy, which
/// its name says is right, then checked again (docs/adr/ADR-026.md decision 6).
pub fn put(project: &Path, bytes: &[u8]) -> Result<String, String> {
    put_with(project, bytes, &|dir| std::fs::File::open(dir)?.sync_all())
}

/// The project's `.hoh/`, made with its `.gitignore` before an object goes under it (docs/adr/ADR-027.md decision 3).
fn state_dir(project: &Path) -> Result<(), String> {
    let top = project.join(".hoh");
    crate::state_dir::ensure(&top).map_err(|e| format!("{}: {e}", top.display()))
}

/// [`put`] with `sync_dir` syncing a directory: the object's own directory once its link is made (or found), then
/// each parent up to `.hoh`, whose entries name the directories a first object made.
fn put_with(
    project: &Path,
    bytes: &[u8],
    sync_dir: &dyn Fn(&Path) -> std::io::Result<()>,
) -> Result<String, String> {
    use std::io::Write as _;
    use std::sync::atomic::Ordering;
    let sha256 = sha256_hex(bytes);
    let path = object_path(project, &sha256)?;
    let dir = path.parent().ok_or("no directory")?;
    state_dir(project)?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut tries = 0;
    let (staged, mut file) = loop {
        let staged = dir.join(format!(
            ".{sha256}.{}.{}",
            std::process::id(),
            STAGED.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::File::create_new(&staged) {
            Ok(file) => break (staged, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 1_000 => tries += 1,
            Err(e) => return Err(format!("{}: {e}", staged.display())),
        }
    };
    let placed = (|| -> Result<(), String> {
        let at = |e: std::io::Error| format!("{}: {e}", staged.display());
        file.write_all(bytes).map_err(at)?;
        file.sync_all().map_err(at)?;
        match std::fs::hard_link(&staged, &path) {
            Ok(()) => Ok(()),
            // Made by another write of the same bytes: taken if its bytes match its name. Damaged: its name says
            // what it must hold, so this write's own staged copy (whole and synced) is renamed over it, then read
            // back.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !holds(&path, &sha256).unwrap_or(false) {
                    std::fs::rename(&staged, &path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if !holds(&path, &sha256)? {
                        return Err(format!(
                            "{}: its bytes no longer match its name",
                            path.display()
                        ));
                    }
                }
                Ok(())
            }
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    })();
    drop(file);
    let _ = std::fs::remove_file(&staged);
    placed?;
    let top = project.join(".hoh");
    let mut at = Some(dir);
    while let Some(d) = at {
        sync_dir(d).map_err(|e| format!("{}: {e}", d.display()))?;
        at = if d == top { None } else { d.parent() };
    }
    Ok(sha256)
}

/// Keeps the file `file` as an object, copied and hashed in pieces (a raw log is not bounded), and returns its sha256
/// and size: copied to a staged file of its own under `.hoh/objects/staging`, synced, then linked to its place as
/// [`put`] does (the same bytes there already taken; a damaged object replaced by them).
pub fn put_file(project: &Path, file: &Path) -> Result<(String, u64), String> {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::Ordering;
    let staging = project.join(".hoh/objects/staging");
    state_dir(project)?;
    std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let mut tries = 0;
    let (staged, mut out) = loop {
        let staged = staging.join(format!(
            ".{}.{}",
            std::process::id(),
            STAGED.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::File::create_new(&staged) {
            Ok(out) => break (staged, out),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 1_000 => tries += 1,
            Err(e) => return Err(format!("{}: {e}", staged.display())),
        }
    };
    let copied = (|| -> Result<(String, u64), String> {
        let at = |e: std::io::Error| format!("{}: {e}", file.display());
        let mut input = std::fs::File::open(file).map_err(at)?;
        let mut digest = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let n = input.read(&mut buffer).map_err(at)?;
            if n == 0 {
                break;
            }
            digest.update(&buffer[..n]);
            out.write_all(&buffer[..n])
                .map_err(|e| format!("{}: {e}", staged.display()))?;
            size += n as u64;
        }
        out.sync_all()
            .map_err(|e| format!("{}: {e}", staged.display()))?;
        let sha: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let path = object_path(project, &sha)?;
        let dir = path.parent().ok_or("no directory")?;
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        match std::fs::hard_link(&staged, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !holds(&path, &sha).unwrap_or(false) {
                    std::fs::rename(&staged, &path)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                    if !holds(&path, &sha)? {
                        return Err(format!(
                            "{}: its bytes no longer match its name",
                            path.display()
                        ));
                    }
                }
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
        let top = project.join(".hoh");
        let mut at = Some(dir);
        while let Some(d) = at {
            std::fs::File::open(d)
                .and_then(|f| f.sync_all())
                .map_err(|e| format!("{}: {e}", d.display()))?;
            at = if d == top { None } else { d.parent() };
        }
        Ok((sha, size))
    })();
    drop(out);
    let _ = std::fs::remove_file(&staged);
    copied
}

/// The sha256 of the file `file` and its size, read in fixed-size pieces (M2T09R-02: a raw log is not bounded, so it is
/// never read whole to be checked).
pub fn hash_file(file: &Path) -> Result<(String, u64), String> {
    use std::io::Read as _;
    let at = |e: std::io::Error| format!("{}: {e}", file.display());
    let mut input = std::fs::File::open(file).map_err(at)?;
    let mut digest = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let n = input.read(&mut buffer).map_err(at)?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
        size += n as u64;
    }
    Ok((
        digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        size,
    ))
}

/// Whether the file at `path` holds bytes whose sha256 is `sha256`, read in pieces.
fn holds(path: &Path, sha256: &str) -> Result<bool, String> {
    Ok(hash_file(path)?.0 == sha256)
}

/// The object named `sha256`, if its bytes still match its name.
pub fn get(project: &Path, sha256: &str) -> Result<Vec<u8>, String> {
    let path = object_path(project, sha256)?;
    let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    if sha256_hex(&bytes) != sha256 {
        return Err(format!(
            "{}: its bytes no longer match its name",
            path.display()
        ));
    }
    Ok(bytes)
}

/// Repacks the prompt whose manifest is the object `manifest_sha256` in the project at `project`, from its objects only
/// and under this binary's resource bundle version (docs/adr/ADR-026.md decision 7): a damaged object fails it.
pub fn repack(project: &Path, manifest_sha256: &str) -> Result<crate::context::Packed, String> {
    let manifest = get(project, manifest_sha256)?;
    crate::context::repack(&manifest, &resource_bundle_version(), &|sha| {
        get(project, sha)
    })
}

/// The project-relative path of the object named `sha256` (what `artifacts` rows and references name).
pub fn relative_path(sha256: &str) -> String {
    format!(
        ".hoh/objects/sha256/{}/{sha256}",
        &sha256[..2.min(sha256.len())]
    )
}

/// `<crate version>+<digest of every resource>`: the same binary gives the same version, and a binary whose
/// resources differ in any byte gives another.
pub fn resource_bundle_version() -> String {
    format!(
        "{}+{}",
        env!("CARGO_PKG_VERSION"),
        digest_of(&with_format(&RESOURCES, crate::context::FORMAT))
    )
}

/// The resources and the packing format, which the digest covers as one more line.
fn with_format<'a>(resources: &[(&'a str, &'a str)], format: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut all = resources.to_vec();
    all.push(("context::FORMAT", format));
    all
}

/// The sha256 of the resources' `<path> <sha256>` lines, sorted by path.
fn digest_of(resources: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = resources
        .iter()
        .map(|(path, text)| format!("{path} {}\n", sha256_hex(text.as_bytes())))
        .collect();
    lines.sort();
    sha256_hex(lines.concat().as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
