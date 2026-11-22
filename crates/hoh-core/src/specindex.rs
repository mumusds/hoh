//! The pure Markdown specification index (REQ-034; ADR-033).

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

mod impact;
mod parse;
pub use impact::impact;

pub const PATH: &str = "docs/spec/index.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// An open question of the specification (docs/adr/ADR-035.md decision 1): its id (in the one id set of REQs and
/// ACs), its text, its blocking mark and where it is written. Not part of index.json: the files' digests cover it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub id: String,
    pub text: String,
    pub blocking: bool,
    pub path: String,
    pub line: usize,
}

/// A decision of the specification and its source (`question_id`, `input_id` or `event_id`, REQ-030 behaviour 2),
/// with where it is written. Not part of index.json.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub text: String,
    pub source_kind: String,
    pub source_id: String,
    pub path: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Index {
    value: Value,
    bytes: Vec<u8>,
    questions: Vec<Question>,
    decisions: Vec<Decision>,
}

impl Index {
    /// The open questions, by path and line.
    pub fn questions(&self) -> &[Question] {
        &self.questions
    }
    /// The decisions, by path and line.
    pub fn decisions(&self) -> &[Decision] {
        &self.decisions
    }
    pub fn value(&self) -> &Value {
        &self.value
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn sha256(&self) -> String {
        digest(&self.bytes)
    }

    /// A byte-difference diagnostic. The store excludes known generated digests before recording manual drift.
    /// A missing file is creation, not drift.
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
    let mut questions = Vec::new();
    let mut decisions = Vec::new();
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
        questions.extend(parsed.questions);
        decisions.extend(parsed.decisions);
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
    Ok(Index {
        bytes,
        value,
        questions,
        decisions,
    })
}

#[cfg(test)]
mod tests;
