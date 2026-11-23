#![forbid(unsafe_code)]
#![forbid(clippy::disallowed_methods)]
//! Template rendering and file publication, real filesystem and config, no agents or network.
use hoh_core::{
    config,
    resource_bundle::Bundle,
    template_files::{self, Selection},
    templates::{self, Inputs},
};
type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_029_02_default_profile_source_and_reprofile_preserves_edited_files() -> R {
    for explicit in [false, true] {
        let dir = tempfile::Builder::new().prefix("m3t08-docs-").tempdir()?;
        let root = dir.path();
        std::fs::create_dir_all(root.join(".hoh"))?;
        if explicit {
            std::fs::write(
                root.join(config::FILE),
                "config_version = \"1.0\"\n[project]\ndocument_profile = \"lite\"\n",
            )?;
        }
        let cfg = config::load(root)?;
        let selection = Selection::from_snapshot(&serde_json::json!({"project": cfg.project}))?;
        assert_eq!(selection.profile, if explicit { "lite" } else { "full" });
        assert_eq!(
            selection.source,
            if explicit { "project" } else { "default" }
        );
        let templates = templates::load(root, None, "run", &Bundle::current())?;
        let inputs = Inputs {
            goal: "Build a local tool".into(),
            owners: "developer".into(),
        };
        let first = template_files::instantiate(root, &templates, &selection, &inputs)?;
        assert_eq!(first.profile, selection.profile);
        assert_eq!(first.source, selection.source);
        assert_eq!(first.created.len(), if explicit { 4 } else { 17 });
        assert!(first.skipped.is_empty());
        for file in &first.created {
            assert!(!std::fs::read_to_string(root.join(file))?.contains("{{"));
        }
        let edited = root.join("docs/feature.md");
        std::fs::write(
            &edited,
            "User edited this, with {{literal}} and no metadata.\n",
        )?;
        let digest = hoh_core::specindex::digest(&std::fs::read(&edited)?);
        let full = Selection {
            profile: "full".into(),
            source: "project".into(),
        };
        let second = template_files::instantiate(root, &templates, &full, &inputs)?;
        assert_eq!(second.created.len(), if explicit { 13 } else { 0 });
        assert_eq!(second.skipped.len(), first.created.len());
        assert_eq!(
            hoh_core::specindex::digest(&std::fs::read(&edited)?),
            digest
        );
        let retry = template_files::instantiate(root, &templates, &full, &inputs)?;
        assert!(retry.created.is_empty());
        assert_eq!(retry.skipped.len(), 17);
    }
    Ok(())
}

#[test]
fn test_029_02_invalid_render_or_link_cannot_overwrite_or_escape() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-doc-errors-")
        .tempdir()?;
    let root = dir.path().join("project");
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&root)?;
    std::fs::create_dir_all(&outside)?;
    let templates = templates::load(&root, None, "run", &Bundle::current())?;
    let full = Selection {
        profile: "full".into(),
        source: "default".into(),
    };
    let bad = Inputs {
        goal: "{{UNRESOLVED}}".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(&root, &templates, &full, &bad).is_err());
    assert!(!root.join("docs").exists());
    std::os::unix::fs::symlink(&outside, root.join("docs"))?;
    let good = Inputs {
        goal: "Goal".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(&root, &templates, &full, &good).is_err());
    assert_eq!(std::fs::read_dir(&outside)?.count(), 0);
    assert!(Selection::from_snapshot(&serde_json::json!({"project":{"document_profile":"invalid","document_profile_source":"project"}})).is_err());
    Ok(())
}

#[test]
fn test_029_02_retry_after_publication_error_keeps_completed_files() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-doc-retry-")
        .tempdir()?;
    let root = dir.path();
    std::fs::create_dir_all(root.join("docs/ux.md"))?;
    let templates = templates::load(root, None, "run", &Bundle::current())?;
    let selection = Selection {
        profile: "full".into(),
        source: "project".into(),
    };
    let inputs = Inputs {
        goal: "Goal".into(),
        owners: "developer".into(),
    };
    assert!(template_files::instantiate(root, &templates, &selection, &inputs).is_err());
    let before = std::fs::read(root.join("docs/acceptance.md"))?;
    std::fs::remove_dir(root.join("docs/ux.md"))?;
    let retried = template_files::instantiate(root, &templates, &selection, &inputs)?;
    assert!(retried.created.contains(&"docs/ux.md".into()));
    assert!(retried.skipped.contains(&"docs/acceptance.md".into()));
    assert_eq!(std::fs::read(root.join("docs/acceptance.md"))?, before);
    Ok(())
}
