//! Per-turn selection and immutable Skill provenance. No scheduling authority.
use super::{Error, Resolved, unavailable};
use crate::context::{self, Class, Entry, Kind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub fn phase(
    role: &str,
    kind: &str,
    writer: bool,
    supervision: bool,
    rework: bool,
) -> Option<&'static str> {
    if supervision {
        return Some("plan");
    }
    match kind {
        "spec_draft" => Some("specify"),
        "work" => match role {
            "developer" if writer => Some(if rework { "repair" } else { "implement" }),
            "qa" => Some("verify"),
            "reviewer" => Some("review"),
            _ => None,
        },
        _ => None,
    }
}
pub fn for_turn<'a>(
    all: &'a BTreeMap<String, Resolved>,
    role: &str,
    phase: Option<&str>,
) -> Vec<&'a Resolved> {
    all.values()
        .filter(|s| {
            s.skill.roles.iter().any(|r| r == role)
                && phase.is_some_and(|p| s.skill.phases.iter().any(|v| v == p))
        })
        .collect()
}
pub fn with_dependencies<'a>(
    all: &'a BTreeMap<String, Resolved>,
    matched: &[&'a Resolved],
) -> Result<Vec<&'a Resolved>, Error> {
    let mut pending: Vec<_> = matched.iter().map(|s| s.skill.id.as_str()).collect();
    let mut selected = BTreeSet::new();
    while let Some(id) = pending.pop() {
        if selected.insert(id) {
            let s = all
                .get(id)
                .ok_or_else(|| unavailable(format!("missing dependency {id}")))?;
            pending.extend(s.skill.dependencies.iter().map(String::as_str));
        }
    }
    selected
        .into_iter()
        .map(|id| {
            all.get(id)
                .ok_or_else(|| unavailable(format!("missing Skill {id}")))
        })
        .collect()
}
impl Resolved {
    pub fn index_line(&self) -> String {
        format!(
            "- {} {} {} {}",
            self.skill.id, self.skill.version, self.skill.description, self.source.path
        )
    }
    pub fn entry(&self, indexed: bool) -> Entry {
        Entry {
            class: Class::Knowledge,
            kind: Kind::Plain,
            path: self.source.path.clone(),
            text: self.source.text.clone(),
            summary: None,
            relevance: if indexed { 100 } else { 90 },
            recency: 0,
            causal: 0,
        }
    }
}

/// Separate from the role-contract chain: the registry object retains the exact version's definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub version: String,
    pub layer: String,
    pub path: String,
    pub sha256: String,
    pub registry_layer: String,
    pub registry_path: String,
    pub registry_sha256: String,
    pub indexed: bool,
    pub revision_id: String,
}
impl Source {
    pub fn new(skill: &Resolved, role: &str, indexed: bool) -> Self {
        let mut source = Self {
            id: skill.skill.id.clone(),
            version: skill.skill.version.clone(),
            layer: skill.source.layer.into(),
            path: skill.source.path.clone(),
            sha256: context::sha256_hex(skill.source.text.as_bytes()),
            registry_layer: skill.registry_source.layer.into(),
            registry_path: skill.registry_source.path.clone(),
            registry_sha256: context::sha256_hex(skill.registry_source.text.as_bytes()),
            indexed,
            revision_id: String::new(),
        };
        source.revision_id = source.identity(role);
        source
    }
    pub fn identity(&self, role: &str) -> String {
        format!(
            "skill/{}",
            context::sha256_hex(
                serde_json::json!([
                    role,
                    self.id,
                    self.version,
                    self.layer,
                    self.path,
                    self.sha256,
                    self.registry_layer,
                    self.registry_path,
                    self.registry_sha256
                ])
                .to_string()
                .as_bytes()
            )
        )
    }
}
