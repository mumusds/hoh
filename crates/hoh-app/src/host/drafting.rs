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
