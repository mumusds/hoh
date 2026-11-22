//! The product's requirement trace (REQ-032; docs/adr/ADR-039.md decisions 1 and 2): the chain REQ→AC→Task→
//! candidate→TEST→evidence→result projected from the store's rows and the candidate's spec file, with what is broken,
//! orphaned and stale. Pure: no store, no file, no git; `persistence` reads the rows and the spec object.
//! `scripts/product/traceability.py` (`checks_of`, `chain`) is its mirror and must keep the same rules.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

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

/// A task of the run: its id and the requirements it references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task_id: String,
    pub requirement_ids: Vec<String>,
}

/// A candidate of the run: its id and state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateRow {
    pub candidate_id: String,
    pub state: String,
}

/// An evidence row on one of the run's candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceRow {
    pub evidence_id: String,
    pub candidate_id: String,
    pub check_id: String,
    pub outcome: String,
}

/// An `acceptance_results` row on one of the run's candidates, with its specification's revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRow {
    pub ac_id: String,
    pub spec_revision: i64,
    pub candidate_id: String,
    pub evidence_id: String,
    pub status: String,
}

/// What the export is projected from.
#[derive(Debug, Clone, PartialEq)]
pub struct Input {
    pub run_id: String,
    pub spec_revision: Option<i64>,
    pub spec_source: SpecSource,
    /// The spec file (an index: `requirements`, `scenarios`).
    pub spec: Value,
    /// Why the spec file could not be read (it is then null), if it could not.
    pub spec_error: Option<String>,
    /// The run's current candidate: its latest, bound to the current revision's digest.
    pub candidate_id: Option<String>,
    /// The run's tasks, candidates, evidence and results, in their rows' order.
    pub tasks: Vec<TaskRow>,
    pub candidates: Vec<CandidateRow>,
    pub evidence: Vec<EvidenceRow>,
    pub results: Vec<ResultRow>,
}

/// Whether `result` is history: under another revision than `revision`, or on a STALE candidate (REQ-032 behaviour
/// 5: every acceptance node of an old revision, not only the affected chains).
fn stale(result: &ResultRow, revision: Option<i64>, states: &BTreeMap<&str, &str>) -> bool {
    Some(result.spec_revision) != revision
        || states.get(result.candidate_id.as_str()) == Some(&"STALE")
}

/// The export of `input` (docs/adr/ADR-039.md decision 2): the chain's nodes with their links, and what is broken,
/// orphaned and stale.
pub fn export(input: &Input) -> Value {
    let revision = input.spec_revision;
    let tests = tests(&input.spec);
    let mut acceptance = Vec::new();
    let mut requirements = Vec::new();
    for requirement in input.spec["requirements"].as_array().into_iter().flatten() {
        let id = requirement["id"].as_str().unwrap_or_default();
        let mut ac_ids = Vec::new();
        for ac in requirement["acceptance"].as_array().into_iter().flatten() {
            let ac_id = ac["id"].as_str().unwrap_or_default();
            ac_ids.push(ac_id);
            let statuses: Vec<&str> = input
                .results
                .iter()
                .filter(|r| {
                    r.ac_id == ac_id
                        && Some(r.spec_revision) == revision
                        && input.candidate_id.as_deref() == Some(r.candidate_id.as_str())
                })
                .map(|r| r.status.as_str())
                .collect();
            acceptance.push(
                json!({"ac_id": ac_id, "requirement_id": id, "test_id": ac["test_id"],
                "spec_revision": revision, "statuses": statuses}),
            );
        }
        let task_ids: Vec<&str> = input
            .tasks
            .iter()
            .filter(|t| t.requirement_ids.iter().any(|r| r == id))
            .map(|t| t.task_id.as_str())
            .collect();
        requirements.push(json!({"requirement_id": id, "ac_ids": ac_ids, "task_ids": task_ids}));
    }
    let states: BTreeMap<&str, &str> = input
        .candidates
        .iter()
        .map(|c| (c.candidate_id.as_str(), c.state.as_str()))
        .collect();
    let task_ids: Vec<&str> = input.tasks.iter().map(|t| t.task_id.as_str()).collect();
    let evidence_ids: BTreeSet<&str> = input
        .evidence
        .iter()
        .map(|e| e.evidence_id.as_str())
        .collect();
    let mut broken = Vec::new();
    for a in &acceptance {
        if a["test_id"].as_str().is_none() {
            broken.push(json!({"node": "acceptance", "id": a["ac_id"], "missing": "test"}));
        }
    }
    for t in &tests {
        if t.check_id.is_none() {
            broken.push(json!({"node": "test", "id": t.test_id, "missing": "check"}));
        }
        let evidenced = input.results.iter().any(|r| {
            r.ac_id == t.ac_id
                && Some(r.spec_revision) == revision
                && evidence_ids.contains(r.evidence_id.as_str())
        });
        if !evidenced {
            broken.push(json!({"node": "test", "id": t.test_id, "missing": "evidence"}));
        }
    }
    for e in &input.evidence {
        if !states.contains_key(e.candidate_id.as_str()) {
            broken.push(json!({"node": "evidence", "id": e.evidence_id, "missing": "candidate"}));
        }
    }
    for t in &input.tasks {
        if t.requirement_ids.is_empty() {
            broken.push(json!({"node": "task", "id": t.task_id, "missing": "requirement"}));
        }
    }
    let mut orphan = Vec::new();
    let mut stale_nodes = Vec::new();
    let mut results = Vec::new();
    for r in &input.results {
        let old = stale(r, revision, &states);
        let node = json!({"ac_id": r.ac_id, "spec_revision": r.spec_revision, "candidate_id": r.candidate_id,
            "evidence_id": r.evidence_id, "status": r.status});
        if matches!(r.status.as_str(), "PASS" | "DEGRADED")
            && (input.candidate_id.as_deref() != Some(r.candidate_id.as_str())
                || Some(r.spec_revision) != revision)
        {
            orphan.push(node.clone());
        }
        if old {
            stale_nodes.push(node.clone());
        }
        let mut node = node;
        node["stale"] = json!(old);
        results.push(node);
    }
    json!({
        "run_id": input.run_id,
        "spec_revision": revision,
        "spec_source": input.spec_source.as_str(),
        "spec_error": input.spec_error,
        "candidate_id": input.candidate_id,
        "requirements": requirements,
        "acceptance": acceptance,
        "tasks": input.tasks.iter().map(|t| json!({"task_id": t.task_id, "requirement_ids": t.requirement_ids}))
            .collect::<Vec<_>>(),
        "candidates": input.candidates.iter().map(|c| json!({"candidate_id": c.candidate_id, "state": c.state,
            "current": input.candidate_id.as_deref() == Some(c.candidate_id.as_str()), "task_ids": task_ids}))
            .collect::<Vec<_>>(),
        "tests": tests.iter().map(|t| json!({"test_id": t.test_id, "ac_id": t.ac_id, "check_id": t.check_id}))
            .collect::<Vec<_>>(),
        "evidence": input.evidence.iter().map(|e| json!({"evidence_id": e.evidence_id, "candidate_id": e.candidate_id,
            "check_id": e.check_id, "outcome": e.outcome})).collect::<Vec<_>>(),
        "results": results,
        "broken": broken,
        "orphan": orphan,
        "stale": stale_nodes,
    })
}

#[cfg(test)]
mod tests;
