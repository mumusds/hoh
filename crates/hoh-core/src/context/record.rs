//! A packed prompt's record (docs/adr/ADR-026.md decision 7): the objects its inputs are kept as, its manifest, and
//! its `context.packed` event; and repacking from a manifest. Pure: the caller writes and reads the objects.

use super::{
    Class, Entry, FORMAT, Kind, Mandatory, PackError, Packed, Params, pack_with, policy, sha256_hex,
};
use serde_json::{Value, json};

/// The role contract a prompt was packed into: its layer (`builtin` or `project`), its path and its text.
#[derive(Debug, Clone, Copy)]
pub struct Contract<'a> {
    pub layer: &'a str,
    pub path: &'a str,
    pub text: &'a str,
}

/// An object to keep: its bytes, its type, what it is (`template`, `lead`, `entry`, `summary`, `index`, `manifest`) and
/// the logical path it stands for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Object {
    pub bytes: Vec<u8>,
    pub mime: &'static str,
    pub kind: &'static str,
    pub name: String,
}

impl Object {
    pub fn sha256(&self) -> String {
        sha256_hex(&self.bytes)
    }
}

/// A prompt packed and recorded: the packing, every object to keep (the manifest last), the manifest's sha256 and the
/// payload of its `context.packed` event.
#[derive(Debug, Clone)]
pub struct Recorded {
    pub packed: Packed,
    pub objects: Vec<Object>,
    pub manifest_sha256: String,
    pub event: Value,
}

const TEXT: &str = "text/plain; charset=utf-8";
const JSON: &str = "application/json";

/// Packs `entries` into `contract` with `mandatory` under `params`, and records it: every input an object, the
/// manifest naming them all with every value packing read, and the event with the inputs list (never empty: the
/// template and the manifest are in it), the policy, the counts and `resource_bundle_version`.
pub fn record(
    contract: Contract<'_>,
    mandatory: &Mandatory,
    package: Option<&Value>,
    entries: &[Entry],
    params: Params,
    resource_bundle_version: &str,
) -> Result<Recorded, PackError> {
    let packed = pack_with(
        contract.text,
        mandatory,
        entries,
        params.budget_tokens,
        params.shares,
    )?;
    let mut objects = vec![Object {
        bytes: contract.text.as_bytes().to_vec(),
        mime: TEXT,
        kind: "template",
        name: contract.path.to_owned(),
    }];
    let mut inputs = vec![
        json!({"kind": "template", "path": contract.path, "sha256": objects[0].sha256(),
        "layer": contract.layer}),
    ];
    if let Some(package) = package {
        let object = Object {
            bytes: package.to_string().into_bytes(),
            mime: JSON,
            kind: "lead",
            name: "first-input".to_owned(),
        };
        inputs.push(json!({"kind": "lead", "path": "first-input", "sha256": object.sha256()}));
        objects.push(object);
    }
    let mut kept = Vec::new();
    for entry in entries {
        let text = Object {
            bytes: entry.text.as_bytes().to_vec(),
            mime: TEXT,
            kind: "entry",
            name: entry.path.clone(),
        };
        let summary = entry.summary.as_ref().map(|s| Object {
            bytes: s.as_bytes().to_vec(),
            mime: TEXT,
            kind: "summary",
            name: entry.path.clone(),
        });
        kept.push(json!({"class": entry.class, "kind": entry.kind, "path": entry.path, "sha256": text.sha256(),
            "summary_sha256": summary.as_ref().map(Object::sha256), "relevance": entry.relevance,
            "recency": entry.recency, "causal": entry.causal}));
        objects.push(text);
        objects.extend(summary);
    }
    for input in &packed.inputs {
        inputs.push(json!({"kind": "entry", "path": input.path, "sha256": input.sha256, "class": input.class,
            "disposition": input.disposition, "tokens": input.tokens}));
    }
    objects.push(Object {
        bytes: packed.index_object.clone(),
        mime: JSON,
        kind: "index",
        name: "index".to_owned(),
    });
    let mut policy = policy();
    policy["shares"] = json!(params.shares);
    let manifest = json!({
        "format": FORMAT,
        "resource_bundle_version": resource_bundle_version,
        "template": {"layer": contract.layer, "path": contract.path, "sha256": sha256_hex(contract.text.as_bytes())},
        "mandatory": {"goal": mandatory.goal, "project": mandatory.project, "task": mandatory.task,
            "evidence": mandatory.evidence, "lead": mandatory.lead, "schemas": mandatory.schemas},
        "entries": kept,
        "budget_tokens": params.budget_tokens,
        "shares": params.shares,
        "policy": policy,
    });
    let manifest = Object {
        bytes: manifest.to_string().into_bytes(),
        mime: JSON,
        kind: "manifest",
        name: "manifest".to_owned(),
    };
    let manifest_sha256 = manifest.sha256();
    inputs.insert(
        if package.is_some() { 2 } else { 1 },
        json!({"kind": "manifest", "path": "manifest", "sha256": manifest_sha256}),
    );
    objects.push(manifest);
    let event = json!({
        "prompt_sha256": packed.sha256,
        "resource_bundle_version": resource_bundle_version,
        "format": FORMAT,
        "manifest": manifest_sha256,
        "index": packed.index_sha256,
        "budget_tokens": params.budget_tokens,
        "estimated": true,
        "policy": policy,
        "inputs": inputs,
        "layers": packed.stats.layers,
        "stats": packed.stats,
    });
    Ok(Recorded {
        packed,
        objects,
        manifest_sha256,
        event,
    })
}

/// Repacks the prompt the manifest `manifest` (its bytes) records, reading the objects it names through `get`, under
/// the resource bundle version `current`: a manifest of another version is refused, never repacked as this one.
pub fn repack(
    manifest: &[u8],
    current: &str,
    get: &dyn Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Packed, String> {
    let m: Value = serde_json::from_slice(manifest).map_err(|e| format!("the manifest: {e}"))?;
    let version = m["resource_bundle_version"].as_str().unwrap_or_default();
    if version != current || m["format"] != json!(FORMAT) {
        return Err(format!(
            "the manifest was packed under {version} ({}), this is {current} ({FORMAT})",
            m["format"]
        ));
    }
    let text = |sha: &Value| -> Result<String, String> {
        let sha = sha.as_str().ok_or("an object without its sha256")?;
        String::from_utf8(get(sha)?).map_err(|e| format!("{sha}: {e}"))
    };
    let template = text(&m["template"]["sha256"])?;
    let field = |k: &str| m["mandatory"][k].as_str().unwrap_or_default().to_owned();
    let mandatory = Mandatory {
        goal: field("goal"),
        project: field("project"),
        task: field("task"),
        evidence: field("evidence"),
        lead: field("lead"),
        schemas: field("schemas"),
    };
    let mut entries = Vec::new();
    for e in m["entries"].as_array().cloned().unwrap_or_default() {
        let class: Class = serde_json::from_value(e["class"].clone()).map_err(|e| e.to_string())?;
        let kind: Kind = serde_json::from_value(e["kind"].clone()).map_err(|e| e.to_string())?;
        entries.push(Entry {
            class,
            kind,
            path: e["path"].as_str().unwrap_or_default().to_owned(),
            text: text(&e["sha256"])?,
            summary: if e["summary_sha256"].is_null() {
                None
            } else {
                Some(text(&e["summary_sha256"])?)
            },
            relevance: e["relevance"].as_u64().unwrap_or(0),
            recency: e["recency"].as_u64().unwrap_or(0),
            causal: e["causal"].as_u64().unwrap_or(0),
        });
    }
    let shares: Vec<u64> = m["shares"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default();
    let shares: [u64; 3] = shares
        .try_into()
        .map_err(|_| "the manifest's shares".to_owned())?;
    let budget = m["budget_tokens"].as_u64().ok_or("the manifest's budget")?;
    pack_with(&template, &mandatory, &entries, budget, shares).map_err(|e| e.to_string())
}
