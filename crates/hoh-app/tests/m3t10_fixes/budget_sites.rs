//! DC-T10 call-site inventory, checked against all product persistence sources.
use super::*;
#[test]
fn test_023_01_dc_budget_call_sites_use_one_view() -> R {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hoh-core/src/persistence");
    let mut stack = vec![root.clone()];
    let mut loads = std::collections::BTreeMap::new();
    let mut answers = std::collections::BTreeMap::new();
    let mut task_stops = std::collections::BTreeMap::new();
    let mut consult_stops = std::collections::BTreeMap::new();
    let mut legacy = Vec::new();
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "tests") {
                    stack.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                let text = std::fs::read_to_string(&path)?;
                let text = text
                    .split("#[cfg(test)]")
                    .next()
                    .unwrap_or_default()
                    .lines()
                    .filter(|l| !l.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                let name = path.strip_prefix(&root)?.to_string_lossy().into_owned();
                for old in [
                    "load_budget(",
                    "task_budget_stop(",
                    "budget_and_stop(",
                    "exclude_unused_holds(",
                    "retry::budget_stop(",
                ] {
                    if text.contains(old) {
                        legacy.push((name.clone(), old));
                    }
                }
                // Wrapping calls or SQL across lines must not evade this inventory.
                let mut text: String = text
                    .chars()
                    .filter(|c| !c.is_whitespace() && *c != '\\')
                    .collect();
                for forbidden in ["budget::stop(", "budget::unenforceable("] {
                    assert!(
                        !text.contains(forbidden),
                        "private budget rule in {name}: {forbidden}"
                    );
                }
                // Exact consumption writes are not admission rules. These exceptions permit only the
                // statements, never another read/calculation in either file.
                for (file, statement) in [
                    (
                        "retry.rs",
                        "UPDATE budgets SET used_value = used_value + ?1 WHERE scope_kind = 'run' AND scope_id = ?2 AND kind = 'wall_seconds'",
                    ),
                    (
                        "closing.rs",
                        "UPDATE budgets SET used_value = used_value + 1 WHERE scope_kind = 'run' AND scope_id = ?1 AND kind = 'iterations'",
                    ),
                ] {
                    if name == file {
                        let statement: String =
                            statement.chars().filter(|c| !c.is_whitespace()).collect();
                        assert_eq!(
                            text.matches(&statement).count(),
                            1,
                            "consumption writer changed: {name}"
                        );
                        text = text.replacen(&statement, "", 1);
                    }
                }
                if ![
                    "budget_view.rs",
                    "consult/children.rs",
                    "consult/bounds.rs",
                    "consult/listing.rs",
                    "team.rs",
                ]
                .contains(&name.as_str())
                {
                    for forbidden in [
                        "limit_value",
                        "used_value",
                        "reserved_value",
                        "select*frombudgets",
                    ] {
                        assert!(
                            !text.to_ascii_lowercase().contains(forbidden),
                            "private budget access in {name}: {forbidden}"
                        );
                    }
                }
                let n = text.matches(".task_stop(").count();
                if n > 0 {
                    task_stops.insert(name.clone(), n);
                }
                let n = text.matches(".consult_stop(").count();
                if n > 0 {
                    consult_stops.insert(name.clone(), n);
                }
                let n = text.matches("budget_view::load(").count();
                if n > 0 {
                    loads.insert(name.clone(), n);
                }
                let n = text.matches(".any_stop(").count();
                if n > 0 {
                    answers.insert(name, n);
                }
            }
        }
    }
    assert!(
        legacy.is_empty(),
        "per-site budget calculations remain: {legacy:?}"
    );
    let expected = |rows: &[(&str, usize)]| {
        rows.iter()
            .map(|(p, n)| ((*p).to_owned(), *n))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(
        loads,
        expected(&[
            ("commands.rs", 1),
            ("consult/returned.rs", 1),
            ("decide.rs", 1),
            ("retry.rs", 1),
            ("rounds.rs", 1),
            ("stall.rs", 1),
            ("wakes.rs", 2)
        ])
    );
    assert_eq!(
        answers,
        expected(&[
            ("commands.rs", 1),
            ("retry.rs", 1),
            ("stall.rs", 1),
            ("wakes.rs", 2)
        ])
    );
    assert_eq!(
        task_stops,
        expected(&[("budget_view.rs", 1), ("decide.rs", 1), ("rounds.rs", 1)])
    );
    assert_eq!(
        consult_stops,
        expected(&[("budget_view.rs", 1), ("consult/returned.rs", 1)])
    );
    Ok(())
}
