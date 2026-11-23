//! Embedded document templates and their four-layer overrides (ADR-040).
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

pub const FIELDS: [&str; 7] = [
    "文档ID",
    "标题",
    "版本",
    "状态",
    "负责人",
    "关联需求",
    "来源/假设",
];
const CATEGORIES: [&str; 10] = [
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
];
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub schema_version: String,
    pub profiles: BTreeMap<String, Vec<String>>,
    pub entry_pattern: String,
}
#[derive(Debug, Clone)]
pub struct Templates {
    pub profiles: Profiles,
    pub profiles_source: Contract,
    pub sources: BTreeMap<String, Contract>,
}
#[derive(Debug, Clone)]
pub struct Inputs {
    pub goal: String,
    pub owners: String,
}

fn error(path: &str, line: usize, why: impl std::fmt::Display) -> Error {
    unavailable(format!("{path}: line {line}: {why}"))
}
impl Profiles {
    fn parse(source: &Contract) -> Result<Self, Error> {
        let fail = |why: &str| error(&source.path, 1, why);
        let result: Self =
            serde_json::from_str(&source.text).map_err(|e| error(&source.path, e.line(), e))?;
        if result.schema_version != "1.0" || result.entry_pattern != "resources/templates/{id}.md" {
            return Err(fail("unsupported schema_version or entry_pattern"));
        }
        if result
            .profiles
            .keys()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != BTreeSet::from(["lite", "standard", "full"])
        {
            return Err(fail("profiles must contain lite, standard and full"));
        }
        for ids in result.profiles.values() {
            let mut seen = BTreeSet::new();
            if ids.is_empty()
                || ids
                    .iter()
                    .any(|id| !crate::skills::valid_id(id) || id == "spec" || !seen.insert(id))
            {
                return Err(fail("invalid, duplicate or empty template id list"));
            }
        }
        if CATEGORIES
            .iter()
            .any(|id| !result.profiles["full"].iter().any(|s| s == id))
        {
            return Err(fail("full must contain all ten document categories"));
        }
        for profile in ["lite", "standard"] {
            if result.profiles[profile]
                .iter()
                .any(|id| !result.profiles["full"].contains(id))
            {
                return Err(fail("profile must be a subset of full"));
            }
        }
        Ok(result)
    }
    fn ids(&self) -> BTreeSet<String> {
        self.profiles
            .values()
            .flatten()
            .cloned()
            .chain(["spec".into()])
            .collect()
    }
}

/// Every present layer is validated before the next can override it.
pub fn load(
    root: &Path,
    global: Option<&Path>,
    run: &str,
    bundle: &Bundle,
) -> Result<Templates, Error> {
    let builtin = |path: &str| -> Result<Contract, Error> {
        Ok(Contract {
            layer: "builtin",
            text: bundle.text(path).map_err(unavailable)?.into(),
            path: path.into(),
        })
    };
    let profiles_source = builtin("resources/templates/profiles.json")?;
    let profiles = Profiles::parse(&profiles_source)?;
    let mut sources = BTreeMap::new();
    for id in profiles.ids() {
        sources.insert(
            id.clone(),
            builtin(&format!("resources/templates/{id}.md"))?,
        );
    }
    let mut result = Templates {
        profiles,
        profiles_source,
        sources,
    };
    result.validate_sources()?;
    for layer in crate::prompt_files::override_layers(root, global, run)? {
        if let Some(source) = layer.read("templates/profiles.json")? {
            result.profiles = Profiles::parse(&source)?;
            result.profiles_source = source;
        }
        for id in result.profiles.ids() {
            if let Some(source) = layer.read(&format!("templates/{id}.md"))? {
                result.sources.insert(id, source);
            }
        }
        result.validate_sources()?;
    }
    Ok(result)
}
impl Templates {
    fn validate_sources(&self) -> Result<(), Error> {
        let inputs = Inputs {
            goal: "目标".into(),
            owners: "负责人".into(),
        };
        for id in self.profiles.ids() {
            let source = self
                .sources
                .get(&id)
                .ok_or_else(|| error(&format!("templates/{id}.md"), 1, "missing entry"))?;
            render_one(&id, source, &inputs)?;
        }
        Ok(())
    }
    /// Pure preparation: callers receive a completely validated set before any file is created.
    pub fn render(
        &self,
        profile: &str,
        inputs: &Inputs,
    ) -> Result<BTreeMap<String, String>, Error> {
        let ids = self
            .profiles
            .profiles
            .get(profile)
            .ok_or_else(|| error(&self.profiles_source.path, 1, "unknown profile"))?;
        ids.iter()
            .map(String::as_str)
            .chain(["spec"])
            .map(|id| {
                let source = self
                    .sources
                    .get(id)
                    .ok_or_else(|| error(id, 1, "missing template"))?;
                let path = if id == "spec" {
                    "docs/spec/spec.md".into()
                } else {
                    format!("docs/{id}.md")
                };
                Ok((path, render_one(id, source, inputs)?))
            })
            .collect()
    }
}
fn render_one(id: &str, source: &Contract, inputs: &Inputs) -> Result<String, Error> {
    crate::prompt::validate_size(&source.text).map_err(|e| error(&source.path, 1, e))?;
    if inputs.goal.trim().is_empty() || inputs.owners.trim().is_empty() {
        return Err(error(&source.path, 1, "goal and owners must be nonempty"));
    }
    let goal = inputs.goal.split_whitespace().collect::<Vec<_>>().join(" ");
    let owners = inputs
        .owners
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let spec_goal = inputs
        .goal
        .lines()
        .map(|line| {
            if line.trim_start().starts_with('#') {
                format!("> {line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut output = String::new();
    let mut heading = "正文";
    for (n, line) in source.text.lines().enumerate() {
        if let Some(title) = line.strip_prefix("## ") {
            heading = title;
        }
        let mut rest = line;
        while let Some(start) = rest.find("{{") {
            if rest[..start].contains("}}") {
                return Err(error(&source.path, n + 1, "unresolved placeholder"));
            }
            output.push_str(&rest[..start]);
            let tail = &rest[start + 2..];
            let end = tail
                .find("}}")
                .ok_or_else(|| error(&source.path, n + 1, "unclosed placeholder"))?;
            let token = &tail[..end];
            if token.is_empty()
                || !token
                    .bytes()
                    .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            {
                return Err(error(&source.path, n + 1, "invalid placeholder"));
            }
            let value = match token {
                "GOAL" if id == "spec" => spec_goal.clone(),
                "PROJECT_NAME" => goal.clone(),
                "DOCUMENT_ID" => format!("DOC-{id}"),
                "TITLE" => format!("{goal} · {id}"),
                "REVISION" => "1".into(),
                "OWNERS" => owners.clone(),
                "REQUIREMENT_IDS" => "待起草定义".into(),
                "SOURCES" => "Run 目标；其余来源与假设待核实".into(),
                _ if id != "spec"
                    && !FIELDS
                        .iter()
                        .any(|field| line.starts_with(&format!("{field}："))) =>
                {
                    format!("（待起草补充：{heading}）")
                }
                _ => {
                    return Err(error(
                        &source.path,
                        n + 1,
                        format!("unknown metadata placeholder {token}"),
                    ));
                }
            };
            output.push_str(&value);
            rest = &tail[end + 2..];
        }
        if rest.contains("}}") {
            return Err(error(&source.path, n + 1, "unresolved placeholder"));
        }
        output.push_str(rest);
        output.push('\n');
    }
    if id == "spec" {
        crate::specindex::generate_index(&[crate::specindex::Source {
            path: "docs/spec/spec.md".into(),
            bytes: output.as_bytes().to_vec(),
        }])
        .map_err(|e| error(&source.path, e.line, e.error))?;
    } else {
        validate_metadata(&source.path, &output)?;
    }
    Ok(output)
}
fn placeholders(path: &str, text: &str) -> Result<(), Error> {
    for (n, line) in text.lines().enumerate() {
        if line.contains("{{") || line.contains("}}") {
            return Err(error(path, n + 1, "unresolved placeholder"));
        }
    }
    Ok(())
}
pub fn validate(path: &str, text: &str) -> Result<(), Error> {
    placeholders(path, text)?;
    validate_metadata(path, text)
}
fn validate_metadata(path: &str, text: &str) -> Result<(), Error> {
    // Report empty values at their actual line before reporting a missing field.
    for (n, line) in text.lines().enumerate() {
        for field in FIELDS {
            if line
                .strip_prefix(&format!("{field}："))
                .is_some_and(|s| s.trim().is_empty())
            {
                return Err(error(path, n + 1, format!("empty {field}")));
            }
        }
    }
    for field in FIELDS {
        if text
            .lines()
            .filter(|line| line.starts_with(&format!("{field}：")))
            .count()
            != 1
        {
            return Err(error(path, 1, format!("missing or duplicate {field}")));
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;
