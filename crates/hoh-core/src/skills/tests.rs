use super::*;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

fn builtin() -> Value {
    serde_json::from_str(include_str!("../../../../resources/skills/registry.json")).unwrap()
}

#[test]
fn test_046_03_registry_validation_rejects_cycle_missing_entry_unknown_phase_and_scheduler() -> R {
    let bundle = crate::resource_bundle::Bundle::current();
    let good = Registry::parse(&builtin().to_string())?;
    good.validate(|path| bundle.text(path).map(str::to_owned))?;
    assert_eq!(good.skills.len(), 13);
    for (kind, expected) in [
        ("cycle", "cycle"),
        ("entry", "missing"),
        ("phase", "invented"),
        ("scheduler", "not allowed"),
        ("dependency", "absent"),
        ("output", "TaskResult"),
        ("field", "description"),
    ] {
        let mut value = builtin();
        match kind {
            "cycle" => {
                value["skills"][0]["dependencies"] = json!(["decision-map"]);
            }
            "entry" => {
                value["skills"][0]["entry"] = json!("resources/skills/missing/SKILL.md");
            }
            "phase" => {
                value["skills"][0]["phases"] = json!(["invented"]);
                value["known_phases"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!("invented"));
            }
            "scheduler" => {
                value["skills"][0]["scheduler"] = json!(true);
            }
            "dependency" => {
                value["skills"][0]["dependencies"] = json!(["absent"]);
            }
            "output" => {
                value["skills"][0]["output_contract"] = json!("CommanderDecision");
            }
            "field" => {
                value["skills"][0]["description"] = json!("");
            }
            _ => unreachable!(),
        }
        let result = Registry::parse(&value.to_string())
            .and_then(|r| r.validate(|path| bundle.text(path).map(str::to_owned)));
        assert!(result.is_err(), "{kind} must be rejected");
        let reason = result.unwrap_err();
        assert!(reason.to_string().contains(expected), "{kind}: {reason}");
        if kind == "cycle" {
            assert!(
                reason.to_string().contains("requirements")
                    && reason.to_string().contains("decision-map")
            );
        }
    }
    Ok(())
}

#[test]
fn bundle_includes_all_skills_templates_and_catalogs() -> R {
    let bundle = crate::resource_bundle::Bundle::current();
    let registry = Registry::parse(bundle.text("resources/skills/registry.json")?)?;
    registry.validate(|path| bundle.text(path).map(str::to_owned))?;
    let profiles: Value = serde_json::from_str(bundle.text("resources/templates/profiles.json")?)?;
    for id in profiles["profiles"]["full"].as_array().unwrap() {
        assert!(
            !bundle
                .text(&format!("resources/templates/{}.md", id.as_str().unwrap()))?
                .is_empty()
        );
    }
    Ok(())
}

#[test]
fn four_layers_validate_before_becoming_effective() -> R {
    let dir = tempfile::Builder::new().prefix("m3t08-skills-").tempdir()?;
    let project = dir.path().join("project");
    let global = dir.path().join("global");
    std::fs::create_dir_all(&project)?;
    let bundle = crate::resource_bundle::Bundle::current();
    let roots = [
        global.clone(),
        project.join(".hoh"),
        project.join(".hoh/runs/run-1"),
    ];
    for (index, root) in roots.iter().enumerate() {
        std::fs::create_dir_all(root.join("skills/tdd"))?;
        std::fs::write(
            root.join("skills/tdd/SKILL.md"),
            format!("Layer {index} TDD body"),
        )?;
        let mut registry = builtin();
        registry["skills"] = json!([registry["skills"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == "tdd")
            .unwrap()
            .clone()]);
        registry["skills"][0]["version"] = json!(format!("1.{index}.0"));
        std::fs::write(root.join("skills/registry.json"), registry.to_string())?;
        let loaded = load(&project, Some(&global), "run-1", &bundle)?;
        let tdd = &loaded["tdd"];
        assert_eq!(tdd.source.text, format!("Layer {index} TDD body"));
        assert_eq!(tdd.source.layer, ["global", "project", "run"][index]);
        assert_eq!(tdd.skill.version, format!("1.{index}.0"));
        assert_eq!(loaded.len(), 13);
    }
    let before = load(&project, Some(&global), "run-1", &bundle)?;
    for (index, root) in roots.iter().enumerate() {
        let path = root.join("skills/registry.json");
        let saved = std::fs::read_to_string(&path)?;
        let mut bad: Value = serde_json::from_str(&saved)?;
        bad["skills"][0]["bypass_gates"] = json!(true);
        std::fs::write(&path, bad.to_string())?;
        let result = load_with_diagnostics(&project, Some(&global), "run-1", &bundle)?;
        assert_eq!(result.rejected.len(), 1);
        assert_eq!(
            result.rejected[0].layer,
            ["global", "project", "run"][index]
        );
        assert!(result.rejected[0].reason.contains("not allowed"));
        assert_eq!(
            result.skills["tdd"].source.text,
            format!("Layer {} TDD body", if index == 2 { 1 } else { 2 })
        );
        std::fs::write(&path, saved)?;
        assert_eq!(load(&project, Some(&global), "run-1", &bundle)?, before);
    }
    Ok(())
}

#[test]
fn phase_mapping_and_role_filtering_are_explicit() -> R {
    for (role, kind, writer, supervision, rework, expected) in [
        ("developer", "work", true, false, false, Some("implement")),
        ("developer", "work", true, false, true, Some("repair")),
        ("qa", "work", false, false, false, Some("verify")),
        ("reviewer", "work", false, false, false, Some("review")),
        ("commander", "work", false, true, false, Some("plan")),
        ("cto", "spec_draft", true, false, false, Some("specify")),
        ("unknown", "work", false, false, false, None),
    ] {
        assert_eq!(phase(role, kind, writer, supervision, rework), expected);
    }
    let root = tempfile::Builder::new().prefix("m3t08-phase-").tempdir()?;
    let loaded = load(
        root.path(),
        None,
        "run",
        &crate::resource_bundle::Bundle::current(),
    )?;
    let matched = for_turn(&loaded, "developer", Some("implement"));
    assert_eq!(
        matched
            .iter()
            .map(|s| s.skill.id.as_str())
            .collect::<Vec<_>>(),
        ["domain-modeling", "implementation", "tdd"]
    );
    assert_eq!(
        with_dependencies(&loaded, &matched)?
            .iter()
            .map(|s| s.skill.id.as_str())
            .collect::<Vec<_>>(),
        ["domain-modeling", "implementation", "requirements", "tdd"]
    );
    assert!(for_turn(&loaded, "developer", None).is_empty());
    assert!(for_turn(&loaded, "unknown", Some("implement")).is_empty());
    Ok(())
}

#[test]
fn old_pin_without_registry_has_no_builtin_skills() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-old-pin-")
        .tempdir()?;
    let mut old = Bundle::current();
    old.retain_files(|path, _| !path.starts_with("resources/skills/"));
    let pin = old.store(dir.path())?;
    let restored = pin.load(dir.path())?;
    assert_eq!(restored.version(), old.version());
    assert!(load(dir.path(), None, "old-run", &restored)?.is_empty());
    Ok(())
}

#[test]
fn unknown_extra_fields_are_rejected() -> R {
    for (key, value) in [
        ("capabilities", json!(["scheduler"])),
        ("registers", json!("scheduler")),
        ("unexpected", json!({"nested":"dispatch"})),
    ] {
        let mut registry = builtin();
        registry["skills"][0][key] = value;
        let rejected = Registry::parse(&registry.to_string());
        assert!(rejected.is_err(), "{key} must not be silently accepted");
        assert!(rejected.unwrap_err().to_string().contains(key));
    }
    Ok(())
}

#[test]
fn global_skill_sources_use_relative_identifiers() -> R {
    let dir = tempfile::Builder::new().prefix("m3t08-global-").tempdir()?;
    let global = dir.path().join("global");
    std::fs::create_dir_all(global.join("skills/tdd"))?;
    std::fs::write(global.join("skills/tdd/SKILL.md"), "Global TDD")?;
    std::fs::write(global.join("skills/registry.json"), builtin().to_string())?;
    let loaded = load(dir.path(), Some(&global), "run", &Bundle::current())?;
    assert_eq!(loaded["tdd"].source.path, "global:skills/tdd/SKILL.md");
    assert_eq!(
        loaded["tdd"].registry_source.path,
        "global:skills/registry.json"
    );
    assert!(
        !loaded["tdd"]
            .index_line()
            .contains(&dir.path().display().to_string())
    );
    assert_eq!(
        Source::new(&loaded["tdd"], "developer", true).path,
        "global:skills/tdd/SKILL.md"
    );
    Ok(())
}

#[test]
fn native_session_body_must_match_and_stay_inside_project() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-native-path-")
        .tempdir()?;
    let root = dir.path().join("project");
    std::fs::create_dir(&root)?;
    let skills = load(&root, None, "run", &Bundle::current())?;
    let tdd = &skills["tdd"];
    assert!(native_path(&root, "session", tdd)?.is_none());
    let path = root.join(".hoh/sessions/session/skills/tdd");
    std::fs::create_dir_all(&path)?;
    std::fs::write(path.join("SKILL.md"), "stale body")?;
    assert!(native_path(&root, "session", tdd)?.is_none());
    std::fs::write(path.join("SKILL.md"), &tdd.source.text)?;
    assert_eq!(native_path(&root, "session", tdd)?, Some(path.clone()));
    std::fs::remove_file(path.join("SKILL.md"))?;
    let outside = dir.path().join("outside.md");
    std::fs::write(&outside, &tdd.source.text)?;
    std::os::unix::fs::symlink(&outside, path.join("SKILL.md"))?;
    assert!(
        native_path(&root, "session", tdd)
            .unwrap_err()
            .to_string()
            .contains("outside root")
    );
    Ok(())
}

#[test]
fn native_skill_requires_loadable_pi_frontmatter() -> R {
    let dir = tempfile::Builder::new()
        .prefix("m3t08-native-metadata-")
        .tempdir()?;
    let mut tdd = load(dir.path(), None, "run", &Bundle::current())?["tdd"].clone();
    let path = dir.path().join(".hoh/sessions/session/skills/tdd");
    std::fs::create_dir_all(&path)?;
    for (body, accepted) in [
        ("Plain Skill body", false),
        ("---\nname: tdd\n---\nBody", false),
        ("---\ndescription: ''\n---\nBody", false),
        ("---\ndescription: 42\n---\nBody", false),
        ("---\ndescription: [unterminated\n---\nBody", false),
        ("---\ndescription: Test with TDD\n---\nBody", true),
        (
            "---\ndescription: Hidden\ndisable-model-invocation: true\n---\nBody",
            false,
        ),
        (
            "---\ndescription: Visible\ndisable-model-invocation: false\n---\nBody",
            true,
        ),
        ("---\ndescription: >\n  Test with\n  TDD\n---\nBody", true),
        (
            "\u{feff}---\r\ndescription: 'Test with TDD'\r\n---\r\nBody",
            true,
        ),
    ] {
        tdd.source.text = body.into();
        std::fs::write(path.join("SKILL.md"), body)?;
        assert_eq!(
            native_path(dir.path(), "session", &tdd)?.is_some(),
            accepted,
            "{body:?}"
        );
    }
    Ok(())
}

fn registry_io_failure(case: &str, expected: &str) -> R {
    use std::os::unix::fs::{PermissionsExt, symlink};
    for layer in ["global", "project", "run"] {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("project");
        let global = dir.path().join("global");
        std::fs::create_dir_all(&root)?;
        let base = match layer {
            "global" => global.join("skills"),
            "project" => root.join(".hoh/skills"),
            _ => root.join(".hoh/runs/run/skills"),
        };
        std::fs::create_dir_all(&base)?;
        let registry = base.join("registry.json");
        match case {
            "outside" => {
                let outside = dir.path().join("outside.json");
                std::fs::write(&outside, builtin().to_string())?;
                symlink(outside, &registry)?;
            }
            "directory" => std::fs::create_dir(&registry)?,
            "unreadable" => {
                std::fs::write(&registry, builtin().to_string())?;
                std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o000))?;
            }
            _ => unreachable!(),
        }
        let result = load_with_diagnostics(&root, Some(&global), "run", &Bundle::current());
        assert!(
            result.is_err(),
            "{layer} {case} must fail explicitly, not reject and continue"
        );
        assert!(
            result.unwrap_err().to_string().contains(expected),
            "{layer} {case}"
        );
    }
    Ok(())
}

#[test]
fn registry_outside_root_fails_explicitly() -> R {
    registry_io_failure("outside", "outside root")
}
#[test]
fn registry_non_regular_fails_explicitly() -> R {
    registry_io_failure("directory", "not a regular file")
}
#[test]
fn registry_unreadable_fails_explicitly() -> R {
    registry_io_failure("unreadable", "Permission denied")
}

#[test]
fn malformed_and_oversized_registration_retain_valid_layers() -> R {
    for bad in [
        "invalid json".to_owned(),
        format!("{}{}", " ".repeat(crate::prompt::MAX_BYTES), builtin()),
    ] {
        let dir = tempfile::tempdir()?;
        let base = dir.path().join(".hoh/skills");
        std::fs::create_dir_all(&base)?;
        std::fs::write(base.join("registry.json"), &bad)?;
        let loaded = load_with_diagnostics(dir.path(), None, "run", &Bundle::current())?;
        assert_eq!(loaded.skills.len(), 13);
        assert_eq!(
            loaded.rejected.len(),
            1,
            "registration error must be recorded"
        );
        assert_eq!(loaded.skills["tdd"].source.layer, "builtin");
    }
    Ok(())
}
