//! The product's requirement trace (REQ-032; docs/adr/ADR-039.md decisions 1 and 2): the chain REQ→AC→Task→
//! candidate→TEST→evidence→result projected from the store's rows and the candidate's spec file, with what is broken,
//! orphaned and stale. Pure: no store, no file, no git; `persistence` reads the rows and the spec object.
//! `scripts/product/traceability.py` (`checks_of`, `chain`) is its mirror and must keep the same rules.

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// TEST id → scenario check of spec file `spec` (REQ-032 behaviour 6, failure mode 3): `None` when it has no
/// `scenarios` key (HoH's own `contracts/requirements.json` layout, which only the repository's traceability tool
/// reads with `contracts/acceptance-scenarios.json`; the product never opens a project's `contracts/`). An entry
/// without a text `id` and `check_id` names nothing.
pub fn checks(spec: &Value) -> Option<BTreeMap<String, String>> {
    let scenarios = spec.get("scenarios")?;
    Some(
        scenarios
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| {
                Some((
                    s.get("id")?.as_str()?.to_owned(),
                    s.get("check_id")?.as_str()?.to_owned(),
                ))
            })
            .collect(),
    )
}

/// A TEST of the spec: its AC, requirement and scenario check (none: broken, missing check).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestNode {
    pub test_id: String,
    pub ac_id: String,
    pub requirement_id: String,
    pub check_id: Option<String>,
}

/// Every TEST of spec file `spec`, in the order of its requirements and their ACs, each with its check by [`checks`]:
/// a TEST the scenarios do not name, or a spec without them, has none.
pub fn tests(spec: &Value) -> Vec<TestNode> {
    let checks = checks(spec).unwrap_or_default();
    let mut out = Vec::new();
    for requirement in spec["requirements"].as_array().into_iter().flatten() {
        let requirement_id = requirement["id"].as_str().unwrap_or_default();
        for ac in requirement["acceptance"].as_array().into_iter().flatten() {
            let Some(test_id) = ac["test_id"].as_str() else {
                continue;
            };
            out.push(TestNode {
                test_id: test_id.to_owned(),
                ac_id: ac["id"].as_str().unwrap_or_default().to_owned(),
                requirement_id: requirement_id.to_owned(),
                check_id: checks.get(test_id).cloned(),
            });
        }
    }
    out
}

/// Where the export's spec file came from: the current candidate's frozen spec object, or the run's revision rebuilt
/// from the rows its approval wrote (no current candidate, or its object unreadable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecSource {
    Candidate,
    Revision,
}

impl SpecSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecSource::Candidate => "candidate",
            SpecSource::Revision => "revision",
        }
    }
}

/// What the export is projected from.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub run_id: String,
    pub spec_revision: Option<i64>,
    pub spec_source: SpecSource,
    /// The spec file (an index: `requirements`, `scenarios`).
    pub spec: Value,
    /// The run's current candidate: its latest, bound to the current revision's digest.
    pub candidate_id: Option<String>,
}

/// The export of `input` (docs/adr/ADR-039.md decision 2).
pub fn export(input: &Input) -> Value {
    let tests: Vec<Value> = tests(&input.spec)
        .into_iter()
        .map(|t| json!({"test_id": t.test_id, "ac_id": t.ac_id, "check_id": t.check_id}))
        .collect();
    json!({
        "run_id": input.run_id,
        "spec_revision": input.spec_revision,
        "spec_source": input.spec_source.as_str(),
        "candidate_id": input.candidate_id,
        "tests": tests,
    })
}

#[cfg(test)]
mod tests;
