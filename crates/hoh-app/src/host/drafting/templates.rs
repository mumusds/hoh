//! The Runtime's one template effect for a drafting Task (ADR-040 S6).
use hoh_core::{persistence::Store, reconcile::EffectState, templates::Inputs};
use serde_json::{Value, json};
use std::path::Path;

pub(super) async fn prepare(
    store: &Store,
    project: &Path,
    work: &crate::host::runner::Work,
) -> Result<(), String> {
    if store
        .reader()
        .map_err(|e| e.to_string())?
        .draft_templates(&work.attempt_id)
        .map_err(|e| e.to_string())?
        .is_some()
    {
        return Ok(());
    }
    let selection = store
        .reader()
        .map_err(|e| e.to_string())?
        .draft_template_selection(&work.run_id)
        .map_err(|e| e.to_string())?;
    let root = project.to_path_buf();
    let held = work.clone();
    let prepared = crate::host::begun::spawn_blocking(move || {
        let bundle = held.assignment.resource_bundle.as_ref().map_or_else(
            || Ok(hoh_core::resource_bundle::Bundle::current()),
            |pin| pin.load(&root),
        )?;
        let templates = hoh_core::templates::load(
            &root,
            hoh_core::prompt_files::global_root().as_deref(),
            &held.run_id,
            &bundle,
        )
        .map_err(|e| e.to_string())?;
        let inputs = Inputs {
            goal: held.assignment.goal.clone().ok_or("draft has no goal")?,
            owners: held.role.clone(),
        };
        // Validate before recording STARTED; a bad override has produced no external effect.
        templates
            .render(&selection.profile, &inputs)
            .map_err(|e| e.to_string())?;
        Ok::<_, String>((templates, selection, inputs, bundle.version()))
    })
    .await
    .map_err(|e| e.to_string())??;
    let id = &work.attempt_id;
    if !store
        .prepare_draft_templates(id)
        .await
        .map_err(|e| e.to_string())?
    {
        return Err("draft template preparation no longer admissible; reconcile its effect".into());
    }
    let key = hoh_core::ids::child_id(id, "templates");
    let transition = async |from, to, detail| -> Result<(), String> {
        if store
            .runner_effect(id, &key, from, to, detail)
            .await
            .map_err(|e| e.to_string())?
        {
            Ok(())
        } else {
            Err("draft template effect changed; reconcile before continuing".into())
        }
    };
    transition(
        EffectState::Prepared,
        EffectState::Started,
        json!({"operation":"templates.instantiate","resource_bundle_version":prepared.3}),
    )
    .await?;
    let root = project.to_path_buf();
    let detail = crate::host::begun::spawn_blocking(move || {
        let (templates, selection, inputs, version) = prepared;
        let outcome = hoh_core::template_files::instantiate(&root, &templates, &selection, &inputs)
            .map_err(|e| e.to_string())?;
        let mut detail = serde_json::to_value(outcome).map_err(|e| e.to_string())?;
        detail["resource_bundle_version"] = json!(version);
        Ok::<_, String>(detail)
    })
    .await
    .map_err(|e| e.to_string())??;
    // The observation and templates.instantiated are one transaction. A crash after it only
    // commits the effect; a crash before it remains UNKNOWN and preserves all published files.
    transition(EffectState::Started, EffectState::Observed, detail).await?;
    transition(EffectState::Observed, EffectState::Committed, json!({})).await
}

/// Keep Runtime-created files separate from user pre-dirty paths. Both still pass through
/// the same DC-T04c digest/index/commit predicate; staged or edited templates require a commit.
pub(in crate::host) fn attribute(
    start: &mut Value,
    instantiated: Option<&Value>,
) -> Result<(), String> {
    let Some(instantiated) = instantiated else {
        return Ok(());
    };
    let created = instantiated["created"]
        .as_array()
        .ok_or("template observation has no created list")?;
    let mut owned = serde_json::Map::new();
    for path in created {
        let path = path.as_str().ok_or("invalid template path")?;
        let key = super::hex(path.as_bytes());
        if let Some(digest) = start["draft_untracked"]
            .as_object_mut()
            .and_then(|m| m.remove(&key))
        {
            owned.insert(key.clone(), digest);
            start["draft_dirty"]
                .as_array_mut()
                .ok_or("missing draft dirty list")?
                .retain(|p| p.as_str() != Some(&key));
        }
    }
    start["draft_templates"] = Value::Object(owned);
    Ok(())
}
