use super::*;
type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_029_01_user_braces_are_literal_values_not_template_residue() -> R {
    let dir = tempfile::Builder::new().prefix("m3t08-braces-").tempdir()?;
    let templates = load(
        dir.path(),
        None,
        "run",
        &crate::resource_bundle::Bundle::current(),
    )?;
    let goal = "Explain {{VARIABLE}} and unmatched {{ or }}";
    let files = templates.render(
        "full",
        &Inputs {
            goal: goal.into(),
            owners: "{{owner}}".into(),
        },
    )?;
    assert!(files["docs/spec/spec.md"].contains(goal));
    assert!(files["docs/feature.md"].contains(&format!("标题：{goal}")));
    assert!(files["docs/feature.md"].contains("负责人：{{owner}}"));
    // Template-owned residue must still fail, even when user values contain braces.
    let mut broken = templates;
    broken
        .sources
        .get_mut("feature")
        .unwrap()
        .text
        .push_str("\nstray }}\n");
    let error = broken
        .render(
            "full",
            &Inputs {
                goal: goal.into(),
                owners: "owner".into(),
            },
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("feature.md") && error.contains("line") && error.contains("placeholder"),
        "{error}"
    );
    Ok(())
}

#[test]
fn test_029_01_indented_goal_headings_are_quoted() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-headings-")
        .tempdir()?;
    let templates = load(
        dir.path(),
        None,
        "run",
        &crate::resource_bundle::Bundle::current(),
    )?;
    let files = templates.render(
        "lite",
        &Inputs {
            goal: "Goal\n  # comment\n\t## detail\n# literal".into(),
            owners: "developer".into(),
        },
    )?;
    assert!(
        files["docs/spec/spec.md"].contains("Goal\n>   # comment\n> \t## detail\n> # literal\n")
    );
    Ok(())
}

#[test]
fn test_029_01_profiles_instantiate_all_templates_without_placeholders() -> R {
    let bundle = crate::resource_bundle::Bundle::current();
    for profile in ["lite", "standard", "full"] {
        let dir = tempfile::Builder::new()
            .prefix("m3t08-templates-")
            .tempdir()?;
        let templates = load(dir.path(), None, "run", &bundle)?;
        let files = templates.render(
            profile,
            &Inputs {
                goal: "Build a local tool".into(),
                owners: "developer".into(),
            },
        )?;
        let ids = &templates.profiles.profiles[profile];
        assert_eq!(files.len(), ids.len() + 1);
        for (path, text) in &files {
            assert!(!text.contains("{{"), "{path}: {text}");
            if path == "docs/spec/spec.md" {
                continue;
            }
            for field in FIELDS {
                assert!(
                    text.lines().any(|l| l.starts_with(&format!("{field}："))),
                    "{path}: missing {field}"
                );
            }
        }
        for id in ids {
            assert!(files.contains_key(&format!("docs/{id}.md")));
        }
        if profile == "full" {
            for id in [
                "product-definition",
                "prd",
                "feature",
                "ux",
                "architecture",
                "development-plan",
                "test-plan",
                "acceptance",
                "deployment",
                "user-guide",
            ] {
                assert!(files.contains_key(&format!("docs/{id}.md")), "missing {id}");
            }
        }
        let spec = &files["docs/spec/spec.md"];
        assert!(spec.starts_with("# 用户\nBuild a local tool\n"));
        for heading in ["用户", "范围", "非目标", "决策", "开放问题", "需求"] {
            assert_eq!(
                spec.lines()
                    .filter(|l| *l == format!("# {heading}"))
                    .count(),
                1
            );
        }
    }
    Ok(())
}

#[test]
fn test_029_01_global_and_run_overrides_validate_every_layer() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-template-layers-")
        .tempdir()?;
    let project = dir.path().join("project");
    let global = dir.path().join("global");
    std::fs::create_dir_all(&project)?;
    let bundle = crate::resource_bundle::Bundle::current();
    let original = bundle.text("resources/templates/feature.md")?;
    for (layer, root) in [
        ("global", global.clone()),
        ("project", project.join(".hoh")),
        ("run", project.join(".hoh/runs/run")),
    ] {
        std::fs::create_dir_all(root.join("templates"))?;
        std::fs::write(
            root.join("templates/feature.md"),
            format!("{original}\n{layer} marker\n"),
        )?;
        let loaded = load(&project, Some(&global), "run", &bundle)?;
        assert_eq!(loaded.sources["feature"].layer, layer);
        let rendered = loaded.render(
            "lite",
            &Inputs {
                goal: "Test".into(),
                owners: "developer".into(),
            },
        )?;
        assert!(rendered["docs/feature.md"].contains(&format!("{layer} marker")));
    }
    let bad = global.join("templates/feature.md");
    std::fs::write(&bad, "# Missing metadata\n{{BROKEN")?;
    let error = load(&project, Some(&global), "run", &bundle)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("feature.md") && error.contains("line"),
        "{error}"
    );
    Ok(())
}

#[test]
fn test_029_01_render_rejects_unclosed_placeholders_and_empty_metadata_with_positions() -> R {
    for (text, expected) in [("{{BROKEN", "placeholder"), ("标题：\n", "标题")] {
        let error = validate("docs/feature.md", text).unwrap_err().to_string();
        assert!(
            error.contains("docs/feature.md") && error.contains("line") && error.contains(expected),
            "{error}"
        );
    }
    Ok(())
}

#[test]
fn test_029_01_invalid_profiles_and_spec_are_rejected_before_publication() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-profiles-")
        .tempdir()?;
    let root = dir.path().join(".hoh/templates");
    std::fs::create_dir_all(&root)?;
    let bundle = crate::resource_bundle::Bundle::current();
    let mut value: serde_json::Value =
        serde_json::from_str(bundle.text("resources/templates/profiles.json")?)?;
    value["profiles"]["lite"] = serde_json::json!(["../escape"]);
    std::fs::write(root.join("profiles.json"), value.to_string())?;
    let error = load(dir.path(), None, "run", &bundle)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("profiles.json") && error.contains("id"),
        "{error}"
    );
    std::fs::write(
        root.join("profiles.json"),
        bundle.text("resources/templates/profiles.json")?,
    )?;
    std::fs::write(root.join("spec.md"), "# 用户\n{{GOAL}}\n# 需求\n")?;
    let error = load(dir.path(), None, "run", &bundle)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("spec.md") && error.contains("line"),
        "{error}"
    );
    Ok(())
}
