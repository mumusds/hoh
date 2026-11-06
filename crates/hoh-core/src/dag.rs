//! Plan validation before import (REQ-038): the scores' range, the schema, duplicate task ids, missing dependencies
//! and cycles, in that order; the whole plan is refused on the first failure, so nothing of it is imported. O(V+E):
//! Kahn's algorithm over a sorted ready set, so the resulting order (and any cycle named) does not depend on the
//! order of the plan's array.

use crate::protocol::{self, ErrorCode, PlanProposal, PlannedTask};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Why a plan is refused.
#[derive(Debug, Clone, PartialEq)]
pub enum PlanError {
    /// The plan breaks its schema in a way none of the codes below names (an API 400).
    Invalid(Vec<String>),
    /// A score outside 0..=5, or a cost outside 1..=5 (refused, never clamped).
    ScoreOutOfRange {
        task_id: String,
        field: String,
        value: Value,
    },
    /// A task id given twice, or already a task of the run (CR-007).
    DuplicateTask { task_id: String },
    /// A dependency that is neither in the plan nor a task of the run; the first such task, its missing ids sorted.
    MissingDependency {
        task_id: String,
        missing: Vec<String>,
    },
    /// The tasks of one cycle, in dependency order, starting at the smallest id.
    Cycle { cycle: Vec<String> },
}

impl PlanError {
    /// The error code (`None` for a plain schema failure).
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            PlanError::Invalid(_) => None,
            PlanError::ScoreOutOfRange { .. } => Some(ErrorCode::ScoreOutOfRange),
            PlanError::DuplicateTask { .. } => Some(ErrorCode::DagDuplicateTask),
            PlanError::MissingDependency { .. } => Some(ErrorCode::DagMissingDependency),
            PlanError::Cycle { .. } => Some(ErrorCode::DagCycle),
        }
    }

    /// The machine-readable details of an API error body.
    pub fn details(&self) -> Value {
        match self {
            PlanError::Invalid(violations) => json!({ "violations": violations }),
            PlanError::ScoreOutOfRange {
                task_id,
                field,
                value,
            } => json!({ "task_id": task_id, "field": field, "value": value }),
            PlanError::DuplicateTask { task_id } => json!({ "task_id": task_id }),
            PlanError::MissingDependency { task_id, missing } => {
                json!({ "task_id": task_id, "missing": missing })
            }
            PlanError::Cycle { cycle } => json!({ "cycle": cycle }),
        }
    }
}

/// A plan that may be imported: its tasks in their canonical topological order (dependencies first, ties by id).
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub tasks: Vec<PlannedTask>,
}

/// The scored members and their ranges.
const RANGES: [(&str, i32, i32); 5] = [
    ("user_value", 0, 5),
    ("unblock_value", 0, 5),
    ("risk_reduction", 0, 5),
    ("information_gain", 0, 5),
    ("cost_estimate", 1, 5),
];

/// An integral number outside its range (JSON Schema counts `7.0` and `1e1` as integers). Anything else is left to
/// the schema.
fn out_of_range(value: &Value, low: i32, high: i32) -> bool {
    let Some(number) = value.as_f64() else {
        return false;
    };
    if !number.is_finite() || number.fract() != 0.0 {
        return false;
    }
    number < f64::from(low) || number > f64::from(high)
}

/// The first score out of range, in the plan's array order (the plan is refused as a whole, so the order only picks
/// which one is named).
fn check_scores(raw: &Value) -> Result<(), PlanError> {
    let Some(tasks) = raw.get("tasks").and_then(Value::as_array) else {
        return Ok(());
    };
    for task in tasks {
        for (field, low, high) in RANGES {
            if let Some(value) = task.get(field)
                && out_of_range(value, low, high)
            {
                return Err(PlanError::ScoreOutOfRange {
                    task_id: task
                        .get("task_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    field: field.to_owned(),
                    value: value.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Validate `raw` (a `PlanProposal`) for a run whose tasks are `existing`.
pub fn validate(raw: &Value, existing: &BTreeSet<String>) -> Result<Plan, PlanError> {
    check_scores(raw)?;
    let proposal: PlanProposal =
        protocol::decode(raw).map_err(|e| PlanError::Invalid(vec![e.to_string()]))?;
    let mut ids = BTreeSet::new();
    let mut duplicates = BTreeSet::new();
    for task in &proposal.tasks {
        if !ids.insert(task.task_id.as_str()) || existing.contains(&task.task_id) {
            duplicates.insert(task.task_id.as_str());
        }
    }
    if let Some(task_id) = duplicates.first() {
        return Err(PlanError::DuplicateTask {
            task_id: (*task_id).to_owned(),
        });
    }
    // Dependencies within the plan, by task id; a dependency on a task of the run is already satisfied here.
    let mut inside: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    let mut missing: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for task in &proposal.tasks {
        let deps = inside.entry(task.task_id.as_str()).or_default();
        for dep in &task.depends_on {
            if ids.contains(dep.as_str()) {
                deps.insert(dep.as_str());
            } else if !existing.contains(dep) {
                missing
                    .entry(task.task_id.as_str())
                    .or_default()
                    .insert(dep.as_str());
            }
        }
    }
    if let Some((task_id, deps)) = missing.first_key_value() {
        return Err(PlanError::MissingDependency {
            task_id: (*task_id).to_owned(),
            missing: deps.iter().map(|d| (*d).to_owned()).collect(),
        });
    }
    let order = kahn(&inside)?;
    let mut by_id: BTreeMap<&str, &PlannedTask> = proposal
        .tasks
        .iter()
        .map(|t| (t.task_id.as_str(), t))
        .collect();
    let tasks = order
        .iter()
        .filter_map(|id| by_id.remove(id))
        .cloned()
        .collect();
    Ok(Plan { tasks })
}

/// Kahn's algorithm with a sorted ready set; a cycle when tasks are left.
fn kahn<'a>(deps: &BTreeMap<&'a str, BTreeSet<&'a str>>) -> Result<Vec<&'a str>, PlanError> {
    let mut waiting: BTreeMap<&str, usize> = deps.iter().map(|(id, d)| (*id, d.len())).collect();
    let mut dependents: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, d) in deps {
        for dep in d {
            dependents.entry(dep).or_default().push(id);
        }
    }
    let mut ready: BTreeSet<&str> = waiting
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut order = Vec::with_capacity(deps.len());
    while let Some(id) = ready.pop_first() {
        order.push(id);
        waiting.remove(id);
        for next in dependents.get(id).into_iter().flatten() {
            if let Some(n) = waiting.get_mut(next) {
                *n -= 1;
                if *n == 0 {
                    ready.insert(next);
                }
            }
        }
    }
    if waiting.is_empty() {
        return Ok(order);
    }
    // Every task left has a dependency left: from the smallest, follow the smallest one until a task repeats.
    let left: BTreeSet<&str> = waiting.keys().copied().collect();
    let mut path: Vec<&str> = Vec::new();
    let mut at = left.first().copied().unwrap_or_default();
    while !path.contains(&at) {
        path.push(at);
        at = deps
            .get(at)
            .and_then(|d| d.iter().find(|dep| left.contains(*dep)))
            .copied()
            .unwrap_or(at);
    }
    let start = path.iter().position(|id| *id == at).unwrap_or(0);
    Err(PlanError::Cycle {
        cycle: path[start..].iter().map(|id| (*id).to_owned()).collect(),
    })
}

#[cfg(test)]
mod tests;
