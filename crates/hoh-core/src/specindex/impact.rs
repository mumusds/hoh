//! Change-request closure over explicit requirement, scenario and task mappings (ADR-033 Q8).

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Exact sorted ids, including both sides of a deletion or replacement. Files alone never imply a changed REQ.
pub fn impact(before: &Value, after: &Value, tasks: &[(String, String)]) -> Value {
    let map = |index: &Value| -> BTreeMap<String, Value> {
        index["requirements"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|req| {
                let id = req["id"].as_str()?;
                let checks: Vec<_> = index["scenarios"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|s| s["requirement_id"] == id)
                    .cloned()
                    .collect();
                Some((id.to_owned(), json!({"requirement":req,"scenarios":checks})))
            })
            .collect()
    };
    let (old, new) = (map(before), map(after));
    let requirements: BTreeSet<_> = old
        .keys()
        .chain(new.keys())
        .filter(|id| old.get(*id) != new.get(*id))
        .cloned()
        .collect();
    let mut acceptance = BTreeSet::new();
    let mut tests = BTreeSet::new();
    for index in [before, after] {
        for scenario in index["scenarios"].as_array().into_iter().flatten() {
            if scenario["requirement_id"]
                .as_str()
                .is_some_and(|id| requirements.contains(id))
            {
                if let Some(id) = scenario["acceptance_id"].as_str() {
                    acceptance.insert(id);
                }
                if let Some(id) = scenario["id"].as_str() {
                    tests.insert(id);
                }
            }
        }
    }
    let tasks: BTreeSet<_> = tasks
        .iter()
        .filter(|(_, req)| requirements.contains(req))
        .map(|(task, _)| task)
        .collect();
    json!({"requirements":requirements,"acceptance":acceptance,"tests":tests,"tasks":tasks})
}
