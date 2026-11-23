//! Host I/O for Runtime-owned drafting; no approvals and no second agent loop (ADR-038).
use hoh_core::persistence::Store;
use hoh_core::reconcile::EffectState;
use std::path::Path;

/// The T04 read-only discovery seam. T07 owns the full run.baseline and its leave guard.
pub(super) fn discover(
    project: &Path,
    run: &str,
    hooks: &dyn super::HostHooks,
) -> Result<(), String> {
    // Actually observe the directory without following its entries or creating anything.
    for entry in std::fs::read_dir(project).map_err(|e| e.to_string())? {
        entry
            .map_err(|e| e.to_string())?
            .file_type()
            .map_err(|e| e.to_string())?;
    }
    hooks.draft_discovered(project, run)
}

pub(super) async fn prepare(
    store: &Store,
    project: &Path,
    work: &super::runner::Work,
) -> Result<(), String> {
    let root = project.to_path_buf();
    let existing =
        super::begun::spawn_blocking(move || match std::fs::symlink_metadata(root.join(".git")) {
            Ok(_) => super::control::git(&root, &["rev-parse", "--show-toplevel"]).map(|_| true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        })
        .await
        .map_err(|e| e.to_string())??;
    if existing {
        return Ok(());
    }
    let id = &work.attempt_id;
    if !store
        .prepare_draft_init(id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("draft git-init preparation no longer admissible; reconcile its effect".into());
    }
    let key = hoh_core::ids::child_id(id, "git-init");
    let transition = async |from, to, detail| -> Result<(), String> {
        if store
            .runner_effect(id, &key, from, to, detail)
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(())
        } else {
            Err("draft git-init effect changed; reconcile before continuing".into())
        }
    };
    transition(
        EffectState::Prepared,
        EffectState::Started,
        serde_json::json!({"operation":"git init","remote":null}),
    )
    .await?;
    let root = project.to_path_buf();
    let marker = work.marker();
    super::begun::spawn_blocking(move || {
        super::control::as_attempt(&marker, || super::control::git(&root, &["init", "--quiet"]))
    })
    .await
    .map_err(|e| e.to_string())??;
    transition(
        EffectState::Started,
        EffectState::Observed,
        serde_json::json!({"git_dir":".git","remote":null}),
    )
    .await?;
    transition(
        EffectState::Observed,
        EffectState::Committed,
        serde_json::json!({}),
    )
    .await?;
    Ok(())
}

/// Read-only observer for Runtime's own git-init effect. Other effects retain their configured observer.
pub(super) fn observe_init(
    root: &Path,
    attempt: &str,
    key: &str,
) -> Option<hoh_core::reconcile::Observation> {
    use hoh_core::reconcile::Observation;
    if key != hoh_core::ids::child_id(attempt, "git-init") {
        return None;
    }
    let found = || -> Result<Observation, String> {
        let path = root.join(".git");
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Observation::Absent),
            Err(e) => return Err(e.to_string()),
            Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                return Err("git-init observation is not a local .git directory".into());
            }
            Ok(_) => (),
        }
        let actual = super::control::git(root, &["rev-parse", "--absolute-git-dir"])?;
        let actual = std::str::from_utf8(&actual)
            .map_err(|e| e.to_string())?
            .trim();
        if std::fs::canonicalize(actual).map_err(|e| e.to_string())?
            != std::fs::canonicalize(&path).map_err(|e| e.to_string())?
        {
            return Err("git-init resolves outside its recorded project".into());
        }
        if super::control::git(root, &["rev-parse", "--is-inside-work-tree"])? != b"true\n" {
            return Err("git-init has no working tree".into());
        }
        Ok(Observation::Found {
            external_ref: ".git".into(),
        })
    };
    Some(found().unwrap_or_else(|detail| Observation::Unavailable { detail }))
}

use hoh_core::persistence::{Ending, WriterOutcome};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::ffi::OsStringExt;

fn hex(path: &[u8]) -> String {
    format!(
        "hex:{}",
        path.iter().map(|b| format!("{b:02x}")).collect::<String>()
    )
}
fn named(path: &[u8]) -> String {
    match std::str::from_utf8(path) {
        Ok(text) if !text.starts_with("hex:") => text.to_owned(),
        _ => hex(path),
    }
}
fn allowed(path: &[u8]) -> bool {
    path.starts_with(b"docs/") || path.starts_with(b".hoh/")
}
fn paths(bytes: Vec<u8>) -> BTreeSet<Vec<u8>> {
    bytes
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}
fn head(root: &Path) -> Result<Option<String>, String> {
    // An unborn repository is distinguishable from an unreadable/invalid one.
    super::control::git(root, &["rev-parse", "--git-dir"])?;
    match super::control::git(root, &["rev-parse", "--verify", "HEAD^{commit}"]) {
        Ok(bytes) => Ok(Some(
            String::from_utf8(bytes)
                .map_err(|e| e.to_string())?
                .trim()
                .into(),
        )),
        Err(e) => {
            let symbolic = super::control::git(root, &["symbolic-ref", "--quiet", "HEAD"])?;
            let name = std::str::from_utf8(&symbolic)
                .map_err(|e| e.to_string())?
                .trim();
            let refs = super::control::git(root, &["for-each-ref", "--format=%(refname)", name])?;
            if refs.is_empty() { Ok(None) } else { Err(e) }
        }
    }
}
fn dirty(root: &Path, committed: bool) -> Result<BTreeSet<Vec<u8>>, String> {
    let mut all = untracked(root)?;
    if committed {
        all.extend(paths(super::control::git(
            root,
            &["diff", "--name-only", "--no-renames", "-z", "HEAD", "--"],
        )?));
        all.extend(paths(super::control::git(
            root,
            &[
                "diff",
                "--cached",
                "--name-only",
                "--no-renames",
                "-z",
                "HEAD",
                "--",
            ],
        )?));
    } else {
        all.extend(paths(super::control::git(
            root,
            &["ls-files", "--cached", "-z"],
        )?));
    }
    Ok(all)
}

fn untracked(root: &Path) -> Result<BTreeSet<Vec<u8>>, String> {
    Ok(paths(super::control::git(
        root,
        &["ls-files", "--others", "--exclude-standard", "-z"],
    )?))
}

// Collapse pre-existing ignored trees (target/, node_modules/, .hoh/) rather than logging every artifact.
// Their contents are outside this Git observation boundary; newly appearing ignored roots still get checked.
fn ignored_roots(root: &Path) -> Result<BTreeSet<Vec<u8>>, String> {
    Ok(paths(super::control::git(
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
    )?))
}

fn fingerprint(root: &Path, path: &[u8]) -> Result<Value, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    no_link(root, path)?;
    let at = root.join(std::ffi::OsString::from_vec(path.to_vec()));
    let mut file = match std::fs::File::open(&at) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(e) => return Err(e.to_string()),
    };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err(format!("not a regular draft path: {}", at.display()));
    }
    let mut digest = Sha256::new();
    let mut buf = [0; 65536];
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }
    use std::os::unix::fs::PermissionsExt;
    Ok(
        json!({"sha256": digest.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>(), "mode": meta.permissions().mode()}),
    )
}

/// Saved before opening the process gate. Errors are durable, not mistaken for an empty write set.
pub(super) fn snapshot(root: &Path) -> Value {
    let observe = || -> Result<Value, String> {
        let h = head(root)?;
        let d = dirty(root, h.is_some())?;
        let u = untracked(root)?;
        let fingerprints = u
            .iter()
            .map(|p| Ok((hex(p), fingerprint(root, p)?)))
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        Ok(
            json!({"head":h,"draft_dirty":d.iter().map(|p|hex(p)).collect::<Vec<_>>(),
            "draft_untracked":fingerprints,
            "draft_ignored_roots":ignored_roots(root)?.iter().map(|p|hex(p)).collect::<Vec<_>>(),
            "draft_outside":d.difference(&u).filter(|p|!allowed(p)).map(|p|named(p)).collect::<Vec<_>>()}),
        )
    };
    observe().unwrap_or_else(|error| json!({"draft_error":error}))
}

fn decoded(path: &str) -> Result<Vec<u8>, String> {
    let bytes = path
        .strip_prefix("hex:")
        .ok_or("invalid draft path encoding")?
        .as_bytes();
    bytes
        .chunks(2)
        .map(|pair| {
            if pair.len() != 2 {
                return Err("invalid draft path encoding".into());
            }
            let text = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
            u8::from_str_radix(text, 16).map_err(|e| e.to_string())
        })
        .collect()
}

fn no_link(root: &Path, path: &[u8]) -> Result<(), String> {
    let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(path.to_vec()));
    let mut at = root.to_path_buf();
    for part in path.components() {
        let std::path::Component::Normal(part) = part else {
            return Err("non-relative Git path".into());
        };
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(format!("link in draft path {}", at.display()));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn checked(root: &Path, start: &Value) -> Result<WriterOutcome, String> {
    if start.get("draft_dirty").and_then(Value::as_array).is_none() {
        return Err(format!(
            "missing or failed draft start observation: {start}"
        ));
    }
    if start["draft_outside"]
        .as_array()
        .is_none_or(|p| !p.is_empty())
    {
        return Err(format!(
            "product files already dirty at draft start; ownership uncertain: {}",
            start["draft_outside"]
        ));
    }
    let current = head(root)?;
    let mut changed = dirty(root, current.is_some())?;
    let uncommitted = changed.clone();
    let initial = start["draft_untracked"]
        .as_object()
        .ok_or("missing untracked start digests")?;
    for (name, digest) in initial {
        let path = decoded(name)?;
        if fingerprint(root, &path)? == *digest {
            changed.remove(&path);
        } else {
            changed.insert(path);
        }
    }
    let ignored = start["draft_ignored_roots"]
        .as_array()
        .ok_or("missing ignored roots observation")?
        .iter()
        .map(|p| decoded(p.as_str().ok_or("invalid ignored root")?))
        .collect::<Result<BTreeSet<_>, String>>()?;
    changed.extend(ignored_roots(root)?.difference(&ignored).cloned());

    if let Some(current) = &current {
        let committed = if let Some(base) = start["head"].as_str() {
            super::control::git(root, &["merge-base", "--is-ancestor", base, current])?;
            super::control::git(
                root,
                &[
                    "diff",
                    "--name-only",
                    "--no-renames",
                    "-z",
                    base,
                    current,
                    "--",
                ],
            )?
        } else {
            super::control::git(root, &["ls-tree", "-r", "-z", "--name-only", current])?
        };
        changed.extend(paths(committed));
    }
    let outside: Vec<_> = changed
        .iter()
        .filter(|p| !allowed(p))
        .map(|p| named(p))
        .collect();
    if !outside.is_empty() {
        return Ok(WriterOutcome::DraftBlocked {
            detail: "PLAN 起草写入了允许范围之外的路径；文件保留，请用户处理。".into(),
            paths: outside,
        });
    }
    for path in &changed {
        no_link(root, path)?;
    }
    if uncommitted.iter().any(|p| p.starts_with(b"docs/")) || current.is_none() {
        return Ok(WriterOutcome::Correct {
            reasons: vec!["draft Markdown and index must be committed before completion".into()],
        });
    }
    let current = current.ok_or("draft has no committed HEAD")?;
    let files = match super::spec::committed(root, &current) {
        Ok(files) => files,
        Err(reason) => {
            return Ok(WriterOutcome::Correct {
                reasons: vec![reason],
            });
        }
    };
    let index = match hoh_core::specindex::generate_index(&files.sources) {
        Ok(index) => index,
        Err(reason) => {
            return Ok(WriterOutcome::Correct {
                reasons: vec![reason.to_string()],
            });
        }
    };
    if files.tracked_index.as_deref() != Some(index.bytes()) {
        return Ok(WriterOutcome::Correct {
            reasons: vec![
                "committed docs/spec/index.json differs from the committed Markdown".into(),
            ],
        });
    }
    Ok(WriterOutcome::Draft {
        git_commit: current,
        files: Box::new(files),
    })
}

/// A draft never enters product freezing. The result identity is checked by the existing bridge validator.
pub(super) fn outcome(
    root: &Path,
    identity: &hoh_core::bridge::Identity,
    ending: &mut Ending,
    start: &Value,
) -> WriterOutcome {
    // The write boundary applies even when the agent reports BLOCKED or an invalid result.
    let checked = match checked(root, start) {
        Ok(done) => done,
        Err(detail) => {
            return WriterOutcome::DraftBlocked {
                detail,
                paths: vec![],
            };
        }
    };
    if matches!(checked, WriterOutcome::DraftBlocked { .. }) {
        return checked;
    }
    let result = match super::completion::result_of(identity, ending) {
        Ok(r) => r,
        Err(reason) => {
            return WriterOutcome::Correct {
                reasons: vec![reason],
            };
        }
    };
    let reason = match result.status {
        hoh_core::protocol::TaskStatus::Completed => None,
        hoh_core::protocol::TaskStatus::Blocked => {
            Some(hoh_core::persistence::need::WorkReason::AgentBlocked)
        }
        hoh_core::protocol::TaskStatus::Failed => {
            Some(hoh_core::persistence::need::WorkReason::AgentFailed)
        }
        hoh_core::protocol::TaskStatus::Incomplete => {
            Some(hoh_core::persistence::need::WorkReason::AgentIncomplete)
        }
    };
    if let Some(reason) = reason {
        return WriterOutcome::Decide {
            reason,
            detail: result.summary,
        };
    }
    if result.candidate_id.is_some() {
        return WriterOutcome::Correct {
            reasons: vec!["a draft has no candidate; candidate_id must be null".into()],
        };
    }
    checked
}

/// Read once the process has ended, before its durable ending releases the write lease.
pub(super) async fn boundary(
    store: &Store,
    root: &Path,
    attempt: &str,
) -> Result<Option<(String, Vec<String>)>, hoh_core::persistence::PersistError> {
    let start = store.reader()?.draft_start(attempt)?;
    // No process gate ever opened: git init alone cannot have written product files.
    let Some(start) = start.filter(|s| !s.is_null()) else {
        return Ok(None);
    };
    let root = root.to_path_buf();
    super::begun::spawn_blocking(move || match checked(&root, &start) {
        Ok(WriterOutcome::DraftBlocked { detail, paths }) => Some((detail, paths)),
        Err(detail) => Some((detail, vec![])),
        _ => None,
    })
    .await
    .map_err(|e| hoh_core::persistence::PersistError::Storage(e.to_string()))
}

#[cfg(test)]
mod tests;
