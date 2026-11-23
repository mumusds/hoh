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
        assert_eq!(
            Registry::parse(&builtin().to_string())?,
            good,
            "failed registration must not mutate the effective set"
        );
    }
    Ok(())
}

#[test]
fn test_046_03_bundle_includes_all_skills_templates_and_catalogs() -> R {
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
fn test_046_03_four_layers_validate_before_becoming_effective() -> R {
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
    for root in roots {
        let path = root.join("skills/registry.json");
        let saved = std::fs::read_to_string(&path)?;
        let mut bad: Value = serde_json::from_str(&saved)?;
        bad["skills"][0]["bypass_gates"] = json!(true);
        std::fs::write(&path, bad.to_string())?;
        let result = load(&project, Some(&global), "run-1", &bundle);
        assert!(
            result.is_err(),
            "invalid lower layer cannot be hidden by later override"
        );
        std::fs::write(&path, saved)?;
        assert_eq!(load(&project, Some(&global), "run-1", &bundle)?, before);
    }
    Ok(())
}
