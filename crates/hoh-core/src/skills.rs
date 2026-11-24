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
            // No extension fields have defined semantics. Reject claims, including capabilities expressed as values.
            if let Some(key) = skill.extra.keys().next() {
                return Err(unavailable(format!(
                    "{}: field {key} is not allowed",
                    skill.id
                )));
            }
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Rejected {
    pub layer: String,
    pub reason: String,
}
#[derive(Debug, Clone)]
pub struct Loaded {
    pub skills: BTreeMap<String, Resolved>,
    pub builtin_registry: bool,
    pub rejected: Vec<Rejected>,
}

pub fn load(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    bundle: &Bundle,
) -> Result<BTreeMap<String, Resolved>, Error> {
    Ok(load_with_diagnostics(root, global, run, bundle)?.skills)
}

/// Reject each invalid layer atomically, retaining the valid set for this Attempt and a visible reason.
pub fn load_with_diagnostics(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    bundle: &Bundle,
) -> Result<Loaded, Error> {
    let mut selected = BTreeMap::new();
    let builtin = bundle.files().get("resources/skills/registry.json");
    if let Some(text) = builtin {
        let source = Contract {
            layer: "builtin",
            path: "resources/skills/registry.json".into(),
            text: text.clone(),
        };
        let registry = Registry::parse(&source.text)?;
        registry.validate(|path| bundle.text(path).map(str::to_owned))?;
        for skill in registry.skills {
            let body = Contract {
                layer: "builtin",
                path: skill.entry.clone(),
                text: bundle.text(&skill.entry).map_err(unavailable)?.into(),
            };
            selected.insert(
                skill.id.clone(),
                Resolved {
                    skill,
                    source: body,
                    registry_source: source.clone(),
                },
            );
        }
    }
    let mut rejected = Vec::new();
    for layer in crate::prompt_files::override_layers(root, global, run)? {
        match apply_layer(&selected, &layer) {
            Ok(next) => selected = next,
            Err(LayerError::File(error)) => return Err(error),
            Err(LayerError::Registration(error)) => rejected.push(Rejected {
                layer: layer.layer.into(),
                reason: error.to_string(),
            }),
        }
    }
    Ok(Loaded {
        skills: selected,
        builtin_registry: builtin.is_some(),
        rejected,
    })
}

enum LayerError {
    File(Error),
    Registration(Error),
}
impl From<Error> for LayerError {
    fn from(error: Error) -> Self {
        Self::Registration(error)
    }
}

fn apply_layer(
    selected: &BTreeMap<String, Resolved>,
    layer: &crate::prompt_files::OverrideLayer,
) -> Result<BTreeMap<String, Resolved>, LayerError> {
    let read = |path: &str| -> Result<Option<Contract>, LayerError> {
        Ok(layer
            .read(path)
            .map_err(LayerError::File)?
            .map(|mut source| {
                if source.layer == "global" {
                    source.path = format!("global:{path}");
                }
                source
            }))
    };
    let mut next = selected.clone();
    if let Some(source) = read("skills/registry.json")? {
        crate::prompt::validate_size(&source.text)?;
        let patch = Registry::parse(&source.text)?;
        for skill in patch.skills {
            let old = next
                .get(&skill.id)
                .filter(|s| s.skill.entry == skill.entry)
                .map(|s| s.source.clone());
            let body = read(skill.entry.trim_start_matches("resources/"))?
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
        if let Some(body) = read(resolved.skill.entry.trim_start_matches("resources/"))? {
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
    Ok(next)
}

mod turn;
pub use turn::{Source, for_turn, phase, with_dependencies};

/// Only an identical, loadable session-local body can replace the selected body in HoH's prompt.
/// The shared reader rejects paths escaping the project and non-regular entries.
pub fn native_path(
    root: &Path,
    session: &str,
    skill: &Resolved,
) -> Result<Option<std::path::PathBuf>, Error> {
    let relative = format!(
        ".hoh/sessions/{}/skills/{}",
        crate::layout::component(session),
        skill.skill.id
    );
    let body = crate::prompt_files::read_optional(root, &format!("{relative}/SKILL.md"))?;
    Ok(body
        .filter(|body| *body == skill.source.text && pi_frontmatter(body))
        .map(|_| root.join(relative)))
}

/// Reopen the selected native copy just before spawn, rejecting links and changed bytes.
/// The caller falls back to the already selected source, never the changed session copy.
pub fn recheck_native(root: &Path, relative: &Path, expected: &str) -> Result<(), Error> {
    use rustix::fs::{self, Mode, OFlags};
    use std::io::Read;
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut dir = fs::open(root, flags | OFlags::DIRECTORY, Mode::empty()).map_err(unavailable)?;
    for part in relative.components() {
        let std::path::Component::Normal(part) = part else {
            return Err(unavailable("native Skill path must be relative without .."));
        };
        dir = fs::openat(&dir, part, flags | OFlags::DIRECTORY, Mode::empty())
            .map_err(unavailable)?;
    }
    let fd = fs::openat(&dir, "SKILL.md", flags | OFlags::NONBLOCK, Mode::empty())
        .map_err(unavailable)?;
    let file = std::fs::File::from(fd);
    if !file.metadata().map_err(unavailable)?.is_file() {
        return Err(unavailable("native Skill is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(crate::prompt::MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unavailable)?;
    if crate::context::sha256_hex(&bytes) != expected {
        return Err(unavailable("native Skill sha256 changed before spawn"));
    }
    Ok(())
}

// Pi 1.6.0 normalizes BOM/newlines, parses YAML between the first two fences,
// and ignores a Skill unless its description is a nonempty string.
fn pi_frontmatter(body: &str) -> bool {
    let normalized = body
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return false;
    };
    let Some(end) = rest.find("\n---") else {
        return false;
    };
    #[derive(Deserialize)]
    struct Header {
        description: serde_json::Value,
        #[serde(default, rename = "disable-model-invocation")]
        disable_model_invocation: serde_json::Value,
    }
    let Ok(value) = serde_saphyr::from_str::<Header>(&rest[..end]) else {
        return false;
    };
    value.disable_model_invocation != serde_json::Value::Bool(true)
        && value
            .description
            .as_str()
            .is_some_and(|description| !description.trim().is_empty())
}

#[cfg(test)]
mod tests;
