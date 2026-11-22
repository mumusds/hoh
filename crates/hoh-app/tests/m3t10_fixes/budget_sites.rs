//! DC-T10 call-site inventory, checked against all product persistence sources.
use super::*;
#[test]
fn test_023_01_dc_budget_call_sites_use_one_view() -> R {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hoh-core/src/persistence");
    let mut stack = vec![root.clone()];
    let mut loads = std::collections::BTreeMap::new();
    let mut answers = std::collections::BTreeMap::new();
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
    Ok(())
}
