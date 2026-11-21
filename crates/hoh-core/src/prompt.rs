//! Role contract overrides (REQ-045, REQ-047, ADR-034). Pure policy, shared by host and clients.

use crate::config::Contract;
use serde_json::{Value, json};

pub const MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: &'static str,
    pub details: Value,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.details)
    }
}
impl std::error::Error for Error {}

pub fn unavailable(message: impl std::fmt::Display) -> Error {
    Error {
        code: "PROMPT_UNAVAILABLE",
        details: json!({"message": message.to_string()}),
    }
}

pub fn validate_size(text: &str) -> Result<(), Error> {
    if text.len() > MAX_BYTES {
        return Err(Error {
            code: "PROMPT_TOO_LONG",
            details: json!({"bytes": text.len(), "limit": MAX_BYTES}),
        });
    }
    Ok(())
}

pub fn validate_variables(text: &str) -> Result<(), Error> {
    let mut from = 0;
    while let Some(offset) = text[from..].find("{{") {
        let start = from + offset;
        let Some(len) = text[start + 2..].find("}}") else {
            break;
        };
        let name = &text[start + 2..start + 2 + len];
        if !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
        {
            if ![
                "GOAL",
                "PROJECT_CONTEXT",
                "TASK_CONTEXT",
                "EVIDENCE_CONTEXT",
            ]
            .contains(&name)
            {
                return Err(Error {
                    code: "PROMPT_VARIABLE_MISSING",
                    details: json!({"variable": name}),
                });
            }
            from = start + 2 + len + 2;
        } else {
            from = start + 1;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub chain: Vec<Contract>,
    pub conflicts: Vec<Value>,
}

impl Resolved {
    pub fn effective(&self) -> Result<&Contract, Error> {
        self.chain
            .last()
            .ok_or_else(|| unavailable("no role contract"))
    }
}

pub fn resolve(mut sources: Vec<Contract>) -> Result<Resolved, Error> {
    let layers = ["builtin", "global", "project", "run"];
    for source in &sources {
        if !layers.contains(&source.layer) {
            return Err(unavailable("unknown override layer"));
        }
        validate_size(&source.text)?;
    }
    sources.sort_by_key(|s| layers.iter().position(|layer| *layer == s.layer));
    if sources
        .windows(2)
        .any(|pair| pair[0].layer == pair[1].layer)
    {
        return Err(unavailable("duplicate override layer"));
    }
    let resolved = Resolved {
        chain: sources,
        conflicts: Vec::new(),
    };
    validate_variables(&resolved.effective()?.text)?;
    Ok(resolved)
}

/// A deterministic, whole-file unified hunk. Linear space/time even for completely different contracts.
pub fn unified_diff(old: &str, new: &str, old_name: &str, new_name: &str) -> String {
    if old == new {
        return String::new();
    }
    let old_lines = old.split_inclusive('\n').count();
    let new_lines = new.split_inclusive('\n').count();
    let mut diff = format!(
        "--- {old_name}\n+++ {new_name}\n@@ -{},{old_lines} +{},{new_lines} @@\n",
        usize::from(old_lines != 0),
        usize::from(new_lines != 0)
    );
    for (prefix, text) in [('-', old), ('+', new)] {
        for line in text.split_inclusive('\n') {
            diff.push(prefix);
            diff.push_str(line);
            if !line.ends_with('\n') {
                diff.push_str("\n\\ No newline at end of file\n");
            }
        }
    }
    diff
}

pub fn three_way_diff(old: &str, new: &str, custom: &str) -> Value {
    json!({"builtin": unified_diff(old, new, "old-builtin", "new-builtin"),
           "override": unified_diff(old, custom, "old-builtin", "override")})
}

#[cfg(test)]
mod tests;
