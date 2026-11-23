//! Built-in skill registration and per-Attempt overrides (ADR-040).
use crate::{
    config::Contract,
    prompt::{Error, unavailable},
    resource_bundle::Bundle,
};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub const PHASES: [&str; 11] = [
    "discover",
    "specify",
    "readiness",
    "plan",
    "implement",
    "verify",
    "review",
    "repair",
    "recover",
    "release",
    "change",
];

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Skill {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    pub roles: Vec<String>,
    pub phases: Vec<String>,
    pub interaction: String,
    pub entry: String,
    pub dependencies: Vec<String>,
    pub output_contract: String,
    pub license: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub schema_version: String,
    pub known_phases: Vec<String>,
    pub skills: Vec<Skill>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub skill: Skill,
    pub source: Contract,
    pub registry_source: Contract,
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn capabilities(value: &serde_json::Value) -> Result<(), Error> {
    match value {
        serde_json::Value::Object(map) => {
            for (key, value) in map {
                if ["scheduler", "dispatch", "bypass_gates", "auto_merge"].contains(&key.as_str()) {
                    return Err(unavailable(format!("capability {key} is not allowed")));
                }
                capabilities(value)?;
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                capabilities(item)?;
            }
        }
        _ => (),
    }
    Ok(())
}

impl Registry {
    pub fn parse(text: &str) -> Result<Self, Error> {
        let value: serde_json::Value = serde_json::from_str(text).map_err(unavailable)?;
        capabilities(&value)?;
        let registry: Self = serde_json::from_value(value).map_err(unavailable)?;
        registry.validate_fields()?;
        Ok(registry)
    }

    fn validate_fields(&self) -> Result<(), Error> {
        if self.schema_version != "1.0" {
            return Err(unavailable("unsupported registry schema_version"));
        }
        for phase in &self.known_phases {
            if !PHASES.contains(&phase.as_str()) {
                return Err(unavailable(format!("unknown phase {phase}")));
            }
        }
        let mut ids = BTreeSet::new();
        for skill in &self.skills {
            if !valid_id(&skill.id) || !ids.insert(&skill.id) {
                return Err(unavailable(format!(
                    "invalid or duplicate skill id {}",
                    skill.id
                )));
            }
            for (name, value) in [
                ("version", &skill.version),
                ("name", &skill.name),
                ("description", &skill.description),
                ("interaction", &skill.interaction),
                ("entry", &skill.entry),
                ("license", &skill.license),
            ] {
                if value.trim().is_empty() || value.contains(['\n', '\r']) {
                    return Err(unavailable(format!(
                        "{}: {name} must be a nonempty single line",
                        skill.id
                    )));
                }
            }
            if skill.roles.is_empty()
                || skill.roles.iter().any(|role| !valid_id(role))
                || skill.phases.is_empty()
            {
                return Err(unavailable(format!(
                    "{}: roles and phases are required",
                    skill.id
                )));
            }
            if skill.output_contract != "TaskResult" {
                return Err(unavailable(format!(
                    "{}: output_contract must be TaskResult",
                    skill.id
                )));
            }
            for phase in &skill.phases {
                if !PHASES.contains(&phase.as_str()) || !self.known_phases.contains(phase) {
                    return Err(unavailable(format!("{}: unknown phase {phase}", skill.id)));
                }
            }
            let relative = skill
                .entry
                .strip_prefix("resources/skills/")
                .ok_or_else(|| {
                    unavailable(format!(
                        "{}: entry must be under resources/skills",
                        skill.entry
                    ))
                })?;
            if relative.is_empty()
                || relative
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
                || relative.contains('\\')
            {
                return Err(unavailable(format!("{}: invalid entry path", skill.entry)));
            }
        }
        Ok(())
    }

    pub fn validate(
        &self,
        mut read: impl FnMut(&str) -> Result<String, String>,
    ) -> Result<(), Error> {
        self.validate_fields()?;
        let graph: BTreeMap<_, _> = self.skills.iter().map(|s| (s.id.as_str(), s)).collect();
        fn visit<'a>(
            id: &'a str,
            graph: &BTreeMap<&'a str, &'a Skill>,
            done: &mut BTreeSet<&'a str>,
            stack: &mut Vec<&'a str>,
        ) -> Result<(), Error> {
            if let Some(start) = stack.iter().position(|node| *node == id) {
                return Err(unavailable(format!(
                    "dependency cycle: {} -> {id}",
                    stack[start..].join(" -> ")
                )));
            }
            if done.contains(id) {
                return Ok(());
            }
            let skill = graph
                .get(id)
                .ok_or_else(|| unavailable(format!("missing dependency {id}")))?;
            stack.push(id);
            for dependency in &skill.dependencies {
                visit(dependency, graph, done, stack)?;
            }
            stack.pop();
            done.insert(id);
            Ok(())
        }
        let mut done = BTreeSet::new();
        for skill in &self.skills {
            visit(&skill.id, &graph, &mut done, &mut Vec::new())?;
            let text =
                read(&skill.entry).map_err(|e| unavailable(format!("{}: {e}", skill.entry)))?;
            if text.trim().is_empty() {
                return Err(unavailable(format!("{}: empty entry", skill.entry)));
            }
            crate::prompt::validate_size(&text)?;
        }
        Ok(())
    }
}

/// Resolve atomically: a rejected layer returns an error and never publishes a partially changed set.
pub fn load(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    bundle: &Bundle,
) -> Result<BTreeMap<String, Resolved>, Error> {
    let source = Contract {
        layer: "builtin",
        path: "resources/skills/registry.json".into(),
        text: bundle
            .text("resources/skills/registry.json")
            .map_err(unavailable)?
            .into(),
    };
    let registry = Registry::parse(&source.text)?;
    registry.validate(|path| bundle.text(path).map(str::to_owned))?;
    let mut selected: BTreeMap<String, Resolved> = registry
        .skills
        .iter()
        .map(|skill| {
            let body = Contract {
                layer: "builtin",
                path: skill.entry.clone(),
                text: bundle.text(&skill.entry).map_err(unavailable)?.into(),
            };
            Ok((
                skill.id.clone(),
                Resolved {
                    skill: skill.clone(),
                    source: body,
                    registry_source: source.clone(),
                },
            ))
        })
        .collect::<Result<_, Error>>()?;
    for layer in crate::prompt_files::override_layers(root, global, run)? {
        let mut next = selected.clone();
        if let Some(source) = layer.read("skills/registry.json")? {
            let patch = Registry::parse(&source.text)?;
            for skill in patch.skills {
                let old = next
                    .get(&skill.id)
                    .filter(|s| s.skill.entry == skill.entry)
                    .map(|s| s.source.clone());
                let body = layer
                    .read(skill.entry.trim_start_matches("resources/"))?
                    .or(old)
                    .ok_or_else(|| unavailable(format!("{}: missing entry", skill.entry)))?;
                next.insert(
                    skill.id.clone(),
                    Resolved {
                        skill,
                        source: body,
                        registry_source: source.clone(),
                    },
                );
            }
        }
        for resolved in next.values_mut() {
            if let Some(body) = layer.read(resolved.skill.entry.trim_start_matches("resources/"))? {
                resolved.source = body;
            }
        }
        let merged = Registry {
            schema_version: "1.0".into(),
            known_phases: PHASES.iter().map(|p| (*p).into()).collect(),
            skills: next.values().map(|s| s.skill.clone()).collect(),
        };
        merged.validate(|path| {
            next.values()
                .find(|s| s.skill.entry == path)
                .map(|s| s.source.text.clone())
                .ok_or_else(|| format!("{path}: missing entry"))
        })?;
        selected = next;
    }
    Ok(selected)
}

mod turn;
pub use turn::{Source, for_turn, phase, with_dependencies};

#[cfg(test)]
mod tests;
