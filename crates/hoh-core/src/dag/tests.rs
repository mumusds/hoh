use super::*;
use crate::protocol::ErrorCode;
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn planned(id: &str, deps: &[&str]) -> Value {
    json!({
        "task_id": id,
        "title": format!("task {id}"),
        "role": "developer",
        "writer": true,
        "depends_on": deps,
        "requirement_ids": ["REQ-038"],
    })
}

fn plan(tasks: Vec<Value>) -> Value {
    json!({ "tasks": tasks })
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

fn order(plan: &Plan) -> Vec<&str> {
    plan.tasks.iter().map(|t| t.task_id.as_str()).collect()
}

#[test]
fn a_cycle_is_refused_and_named() {
    let error = validate(
        &plan(vec![planned("a", &["b"]), planned("b", &["a"])]),
        &none(),
        &none(),
    )
    .expect_err("a cycle was accepted");
    assert_eq!(error.code(), Some(ErrorCode::DagCycle));
    let PlanError::Cycle { cycle } = &error else {
        panic!("{error:?}");
    };
    assert_eq!(cycle, &["a".to_owned(), "b".to_owned()]);
    assert_eq!(error.details(), json!({"cycle": ["a", "b"]}));
}

#[test]
fn a_self_dependency_is_a_cycle_of_one() {
    let error =
        validate(&plan(vec![planned("a", &["a"])]), &none(), &none()).expect_err("accepted");
    assert_eq!(error.details(), json!({"cycle": ["a"]}));
}

#[test]
fn the_cycle_named_does_not_depend_on_the_plan_order() {
    let tasks = vec![
        planned("x", &[]),
        planned("c", &["b", "x"]),
        planned("b", &["a"]),
        planned("a", &["c"]),
        planned("d", &["c"]),
    ];
    let first = validate(&plan(tasks.clone()), &none(), &none()).expect_err("accepted");
    let mut reversed = tasks;
    reversed.reverse();
    let second = validate(&plan(reversed), &none(), &none()).expect_err("accepted");
    assert_eq!(first, second);
    assert_eq!(first.details(), json!({"cycle": ["a", "c", "b"]}));
}

#[test]
fn a_missing_dependency_is_refused_unless_the_run_has_it() {
    let tasks = plan(vec![
        planned("a", &[]),
        planned("b", &["a", "missing", "gone"]),
    ]);
    let error = validate(&tasks, &none(), &none()).expect_err("accepted");
    assert_eq!(error.code(), Some(ErrorCode::DagMissingDependency));
    assert_eq!(
        error.details(),
        json!({"task_id": "b", "missing": ["gone", "missing"]})
    );
    let existing: BTreeSet<String> = ["missing".to_owned(), "gone".to_owned()].into();
    let ok = validate(&tasks, &existing, &none()).expect("refused");
    assert_eq!(order(&ok), ["a", "b"]);
}

#[test]
fn a_duplicate_task_id_is_refused_in_the_plan_and_against_the_run() {
    let error = validate(
        &plan(vec![planned("a", &[]), planned("a", &[])]),
        &none(),
        &none(),
    )
    .expect_err("accepted");
    assert_eq!(error.code(), Some(ErrorCode::DagDuplicateTask));
    assert_eq!(error.details(), json!({"task_id": "a"}));
    let existing: BTreeSet<String> = ["b".to_owned()].into();
    let error = validate(&plan(vec![planned("b", &[])]), &existing, &none()).expect_err("accepted");
    assert_eq!(error.details(), json!({"task_id": "b"}));
    // Another run's task id is taken too, but it is no dependency of this run's tasks.
    let error = validate(&plan(vec![planned("b", &[])]), &none(), &existing).expect_err("accepted");
    assert_eq!(error.details(), json!({"task_id": "b"}));
    let error =
        validate(&plan(vec![planned("c", &["b"])]), &none(), &existing).expect_err("accepted");
    assert_eq!(error.code(), Some(ErrorCode::DagMissingDependency));
}

#[test]
fn scores_out_of_range_are_refused_with_their_field_before_decoding() {
    for (field, value) in [
        ("user_value", json!(7)),
        ("unblock_value", json!(6.0)),
        ("risk_reduction", json!(-1)),
        ("information_gain", json!(1e1)),
        ("cost_estimate", json!(0)),
        ("cost_estimate", json!(6)),
    ] {
        let mut task = planned("a", &[]);
        task[field] = value.clone();
        let error = validate(&plan(vec![task]), &none(), &none()).expect_err("accepted");
        assert_eq!(error.code(), Some(ErrorCode::ScoreOutOfRange), "{field}");
        assert_eq!(
            error.details(),
            json!({"task_id": "a", "field": field, "value": value}),
        );
    }
    let mut task = planned("a", &[]);
    task["user_value"] = json!(5);
    task["cost_estimate"] = json!(1);
    assert!(validate(&plan(vec![task]), &none(), &none()).is_ok());
}

#[test]
fn a_plan_that_breaks_the_schema_otherwise_is_invalid_without_a_dag_code() {
    let mut task = planned("a", &[]);
    task["user_value"] = json!("high");
    let error = validate(&plan(vec![task]), &none(), &none()).expect_err("accepted");
    assert_eq!(error.code(), None);
    assert!(matches!(error, PlanError::Invalid(_)), "{error:?}");
}

#[test]
fn the_order_is_topological_and_canonical() {
    let tasks = vec![
        planned("d", &["b", "c"]),
        planned("c", &["a"]),
        planned("b", &["a"]),
        planned("a", &[]),
        planned("e", &[]),
    ];
    let first = validate(&plan(tasks.clone()), &none(), &none()).expect("refused");
    assert_eq!(order(&first), ["a", "b", "c", "d", "e"]);
    let mut reversed = tasks;
    reversed.reverse();
    let second = validate(&plan(reversed), &none(), &none()).expect("refused");
    assert_eq!(order(&first), order(&second));
}

#[test]
fn a_large_chain_is_linear() {
    let n = 20_000;
    let tasks: Vec<Value> = (0..n)
        .map(|i| {
            let deps: Vec<String> = if i == 0 {
                Vec::new()
            } else {
                vec![format!("t{:05}", i - 1)]
            };
            let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
            planned(&format!("t{i:05}"), &deps)
        })
        .collect();
    let started = std::time::Instant::now();
    let ok = validate(&plan(tasks), &none(), &none()).expect("refused");
    assert_eq!(ok.tasks.len(), n);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn a_cycle_reached_from_outside_starts_at_its_own_smallest_id() {
    // M2T03R-54: from the smallest task left (a, not on the cycle) the walk reaches z -> b -> z; the cycle named is
    // [b, z], rotated to its smallest id.
    let tasks = vec![
        planned("a", &["z"]),
        planned("z", &["b"]),
        planned("b", &["z"]),
    ];
    let error = validate(&plan(tasks), &none(), &none()).expect_err("accepted");
    assert_eq!(error.details(), json!({"cycle": ["b", "z"]}));
}

#[test]
fn a_long_chain_into_a_cycle_is_named_without_a_quadratic_walk() {
    // M2T03R-54: the walk to the cycle is long; naming it must not be quadratic in its length.
    // Long enough that a quadratic walk stands out on any machine (M2T03R-152): it took about four times the
    // acyclic baseline at 20 000 tasks, and the ratio grows with the length.
    let n = 50_000;
    let mut tasks: Vec<Value> = (0..n)
        .map(|i| {
            let dep = format!("t{:05}", i + 1);
            planned(&format!("t{i:05}"), &[dep.as_str()])
        })
        .collect();
    tasks.push(planned(&format!("t{n:05}"), &["t49999"]));
    let acyclic: Vec<Value> = (0..=n)
        .map(|i| {
            let deps: Vec<String> = if i == n {
                Vec::new()
            } else {
                vec![format!("t{:05}", i + 1)]
            };
            let deps: Vec<&str> = deps.iter().map(String::as_str).collect();
            planned(&format!("t{i:05}"), &deps)
        })
        .collect();
    let started = std::time::Instant::now();
    validate(&plan(acyclic), &none(), &none()).expect("refused");
    let baseline = started.elapsed();
    let started = std::time::Instant::now();
    let error = validate(&plan(tasks), &none(), &none()).expect_err("accepted");
    let elapsed = started.elapsed();
    assert_eq!(error.details(), json!({"cycle": ["t49999", "t50000"]}));
    // Compared with an acyclic chain of the same length on the same machine: a linear walk takes about as long.
    assert!(
        elapsed < baseline * 3 + std::time::Duration::from_millis(50),
        "{elapsed:?} against {baseline:?}"
    );
}

#[test]
fn plan_paths_are_validated_and_kept() {
    // CR-033 decision 1 (REQ-039, ADR-026 decision 12): a planned task's optional `paths` are project-relative files
    // or directories, kept with the task; an absolute path, an empty one or one with a `..` component refuses the
    // whole plan (nothing of it is imported), every such path named. No `paths` is the empty scope.
    let mut scoped = planned("a", &[]);
    scoped["paths"] = json!(["crates/hoh-core/src/dag.rs", "docs/", "./a/b", "a..b/c"]);
    let kept = validate(&plan(vec![scoped, planned("b", &["a"])]), &none(), &none())
        .expect("a plan with project-relative paths");
    assert_eq!(
        kept.tasks
            .iter()
            .map(|t| (t.task_id.as_str(), t.paths.clone()))
            .collect::<Vec<_>>(),
        [
            (
                "a",
                vec![
                    "crates/hoh-core/src/dag.rs".to_owned(),
                    "docs/".to_owned(),
                    "./a/b".to_owned(),
                    "a..b/c".to_owned()
                ]
            ),
            ("b", Vec::new()),
        ]
    );
    // Kept through the protocol type's round trip, absent when empty.
    let back = serde_json::to_value(&kept.tasks[0]).expect("serialize");
    assert_eq!(back["paths"][0], "crates/hoh-core/src/dag.rs");
    assert!(
        serde_json::to_value(&kept.tasks[1])
            .expect("serialize")
            .get("paths")
            .is_none()
    );
    for (paths, named) in [
        (json!(["/etc/passwd"]), vec!["paths[0] \"/etc/passwd\""]),
        (json!([".."]), vec!["paths[0] \"..\""]),
        (json!(["src/../../x"]), vec!["paths[0] \"src/../../x\""]),
        (
            json!(["a/..", "/b"]),
            vec!["paths[0] \"a/..\"", "paths[1] \"/b\""],
        ),
    ] {
        let mut task = planned("a", &[]);
        task["paths"] = paths.clone();
        let error = validate(&plan(vec![planned("z", &[]), task]), &none(), &none())
            .expect_err("a path outside the project was accepted");
        assert_eq!(error.code(), None, "{paths}");
        let PlanError::Invalid(violations) = &error else {
            panic!("{paths}: {error:?}");
        };
        assert_eq!(violations.len(), named.len(), "{violations:?}");
        for (violation, named) in violations.iter().zip(named) {
            assert!(
                violation.contains("task a") && violation.contains(named),
                "{paths}: {violation}"
            );
        }
    }
    // An empty path is refused by the schema already (minLength 1), the plan with it.
    let mut task = planned("a", &[]);
    task["paths"] = json!(["src", ""]);
    let error = validate(&plan(vec![planned("z", &[]), task]), &none(), &none())
        .expect_err("an empty path was accepted");
    let PlanError::Invalid(violations) = &error else {
        panic!("{error:?}");
    };
    assert!(
        violations.iter().any(|v| v.contains("/tasks/1/paths/1")),
        "{violations:?}"
    );
}
