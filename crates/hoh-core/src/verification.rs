//! Frozen candidates and evidence validity (REQ-052, docs/adr/ADR-027.md): the candidate manifest and its canonical
//! identity, byte-equal to tools/candidate.py's, so a product export passes the bootstrap tools and the release gate.
//! Pure: no I/O.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// A file of the candidate by its path relative to the repository and the sha256 of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRef {
    pub path: String,
    pub sha256: String,
}

/// What a candidate binds before it is sealed: the commit and its tree, the specification, environment and test plan
/// files, and every tracked file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub git_commit: String,
    pub git_tree: String,
    pub spec: FileRef,
    pub environment: FileRef,
    pub test_plan: FileRef,
    pub source_files: Vec<FileRef>,
}

/// A sealed candidate: its manifest and its identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub candidate_id: String,
    pub manifest: Manifest,
}

impl Manifest {
    /// Sealed with its identity (decision 2): its source files sorted by path.
    pub fn seal(mut self) -> Sealed {
        self.source_files.sort_by(|a, b| a.path.cmp(&b.path));
        let candidate_id = identity(&body(&self));
        Sealed {
            candidate_id,
            manifest: self,
        }
    }
}

/// The export without its id: what the identity hashes.
fn body(m: &Manifest) -> Value {
    json!({
        "schema_version": "1.0",
        "git_commit": m.git_commit,
        "git_tree": m.git_tree,
        "spec": m.spec,
        "environment": m.environment,
        "test_plan": m.test_plan,
        "spec_sha256": m.spec.sha256,
        "environment_sha256": m.environment.sha256,
        "test_plan_sha256": m.test_plan.sha256,
        "source_files": m.source_files,
    })
}

/// Whether `value` has candidate.schema.json's shape: the in-crate validator cannot read the schema's `^(?!/)` path
/// pattern (no lookaround), so the export's shape is checked here, and an integration test validates exports with
/// Python's jsonschema against the locked schema itself.
fn shape(value: &Value) -> Result<(), String> {
    let object = value.as_object().ok_or("a candidate export is an object")?;
    let keys = [
        "schema_version",
        "candidate_id",
        "git_commit",
        "git_tree",
        "spec_sha256",
        "environment_sha256",
        "test_plan_sha256",
        "spec",
        "environment",
        "test_plan",
        "source_files",
    ];
    if object.len() != keys.len() || keys.iter().any(|k| !object.contains_key(*k)) {
        return Err(format!(
            "a candidate export has exactly the fields {keys:?}"
        ));
    }
    if value["schema_version"] != "1.0" {
        return Err("schema_version is not 1.0".to_owned());
    }
    let hex = |v: &Value, lengths: &[usize]| {
        v.as_str().is_some_and(|s| {
            lengths.contains(&s.len())
                && s.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    };
    let id = value["candidate_id"].as_str().unwrap_or_default();
    if !id
        .strip_prefix("CAND-")
        .is_some_and(|h| hex(&json!(h), &[64]))
    {
        return Err(format!("{id:?} is not a candidate id"));
    }
    for key in ["git_commit", "git_tree"] {
        if !hex(&value[key], &[40, 64]) {
            return Err(format!("{key} is not a git object id"));
        }
    }
    for key in ["spec_sha256", "environment_sha256", "test_plan_sha256"] {
        if !hex(&value[key], &[64]) {
            return Err(format!("{key} is not a sha256"));
        }
    }
    let file = |v: &Value| {
        v.as_object().is_some_and(|o| o.len() == 2)
            && v["path"]
                .as_str()
                .is_some_and(|p| !p.is_empty() && !p.starts_with('/'))
            && hex(&v["sha256"], &[64])
    };
    for key in ["spec", "environment", "test_plan"] {
        if !file(&value[key]) {
            return Err(format!("{key} is not a file reference"));
        }
    }
    let files = value["source_files"]
        .as_array()
        .ok_or("source_files is not a list")?;
    if files.is_empty() || !files.iter().all(file) {
        return Err("source_files is empty or holds what is not a file reference".to_owned());
    }
    Ok(())
}

/// `CAND-` and the sha256 of `body`'s canonical JSON: keys sorted (serde_json's maps are ordered), no whitespace,
/// non-ASCII as it is: what tools/candidate.py's `json.dumps(sort_keys=True, separators=(',',':'),
/// ensure_ascii=False)` writes.
pub fn identity(body: &Value) -> String {
    let mut body = body.clone();
    if let Some(object) = body.as_object_mut() {
        object.remove("candidate_id");
    }
    let canonical = body.to_string();
    let digest: String = Sha256::digest(canonical.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("CAND-{digest}")
}

impl Sealed {
    /// As candidate.schema.json exports it.
    pub fn to_value(&self) -> Value {
        let mut value = body(&self.manifest);
        if let Some(object) = value.as_object_mut() {
            object.insert("candidate_id".to_owned(), json!(self.candidate_id));
        }
        value
    }

    /// One read back from its export, refused when it is not a candidate export, its digests disagree with its files,
    /// or its id does not match its content.
    pub fn from_value(value: &Value) -> Result<Sealed, String> {
        shape(value)?;
        let field = |k: &str| -> Result<FileRef, String> {
            serde_json::from_value(value[k].clone()).map_err(|e| format!("{k}: {e}"))
        };
        let manifest = Manifest {
            git_commit: value["git_commit"].as_str().unwrap_or_default().to_owned(),
            git_tree: value["git_tree"].as_str().unwrap_or_default().to_owned(),
            spec: field("spec")?,
            environment: field("environment")?,
            test_plan: field("test_plan")?,
            source_files: serde_json::from_value(value["source_files"].clone())
                .map_err(|e| e.to_string())?,
        };
        for key in ["spec", "environment", "test_plan"] {
            if value[key]["sha256"] != value[format!("{key}_sha256")] {
                return Err(format!("candidate {key} hash mismatch"));
            }
        }
        let candidate_id = value["candidate_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if candidate_id != identity(value) {
            return Err("candidate identity mismatch".to_owned());
        }
        Ok(Sealed {
            candidate_id,
            manifest,
        })
    }
}

#[cfg(test)]
mod tests;
