//! The pure Markdown specification index (REQ-034; ADR-033).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

mod impact;
mod parse;
pub use impact::impact;

pub const PATH: &str = "docs/spec/index.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexError {
    pub path: String,
    pub line: usize,
    pub error: String,
}

impl IndexError {
    pub fn at(path: &str, line: usize, error: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            line,
            error: error.into(),
        }
    }
}

impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.path, self.line, self.error)
    }
}

impl std::error::Error for IndexError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    value: Value,
    bytes: Vec<u8>,
}

impl Index {
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> String {
        digest(&self.bytes)
    }

    /// The diagnostic payload for explicit regeneration; a missing file is creation, not drift.
    pub fn drift(&self, existing: Option<&[u8]>) -> Option<Value> {
        existing
            .filter(|bytes| *bytes != self.bytes)
            .map(|_| json!({"path": PATH}))
    }

    /// Freezing only checks: it never repairs a committed generated file.
    pub fn verify(&self, tracked: &[u8]) -> Result<(), IndexError> {
        if self.bytes != tracked {
            return Err(IndexError::at(
                PATH,
                1,
                "index.json differs from the Markdown-generated canonical bytes",
            ));
        }
        Ok(())
    }
}

pub fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Parse all source bytes and return one canonical index. No database, filesystem or clock is consulted.
pub fn generate_index(sources: &[Source]) -> Result<Index, IndexError> {
    if sources.is_empty() {
        return Err(IndexError::at(
            "docs/spec",
            1,
            "no Markdown specification files",
        ));
    }
    let mut sources: Vec<_> = sources.iter().collect();
    sources.sort_by(|a, b| a.path.cmp(&b.path));
    let mut paths = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut files = Vec::new();
    let mut requirements = Vec::new();
    let mut scenarios = Vec::new();
    for source in sources {
        let name = source.path.strip_prefix("docs/spec/").unwrap_or("");
        if !name.ends_with(".md")
            || name.contains('/')
            || name.contains('\\')
            || !paths.insert(&source.path)
        {
            return Err(IndexError::at(
                &source.path,
                1,
                "expected a unique docs/spec/*.md path",
            ));
        }
        let parsed = parse::document(source, &mut ids)?;
        files.push(json!({"path": source.path, "sha256": digest(&source.bytes)}));
        requirements.extend(parsed.requirements);
        scenarios.extend(parsed.scenarios);
    }
    let sort =
        |items: &mut Vec<Value>| items.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    sort(&mut requirements);
    sort(&mut scenarios);
    let value = json!({"files": files, "generated": "生成，勿改", "requirements": requirements, "scenarios": scenarios});
    // Value's maps are BTreeMaps (serde_json without preserve_order), including every nested object.
    let mut bytes =
        serde_json::to_vec_pretty(&value).map_err(|e| IndexError::at(PATH, 1, e.to_string()))?;
    bytes.push(b'\n');
    Ok(Index { bytes, value })
}

#[cfg(test)]
mod tests;
