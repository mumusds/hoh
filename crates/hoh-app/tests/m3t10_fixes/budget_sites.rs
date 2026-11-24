//! DC-T10 call-site inventory, checked against all product persistence sources.
use super::*;
fn sources() -> R<Vec<(String, String)>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../hoh-core/src/persistence");
    let mut stack = vec![root.clone()];
    let mut sources = Vec::new();
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
                sources.push((
                    path.strip_prefix(&root)?.to_string_lossy().into_owned(),
                    std::fs::read_to_string(path)?,
                ));
            }
        }
    }
    Ok(sources)
}

// Mask comments and literals only for finding item boundaries; SQL literals remain in the audited text.
fn masked(source: &str, literals: bool) -> Vec<u8> {
    let bytes = source.as_bytes();
    let mut out = bytes.to_vec();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let mut comment = false;
        if bytes[i..].starts_with(b"//") {
            comment = true;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            comment = true;
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r' && {
            let mut quote = i + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            bytes.get(quote) == Some(&b'"')
        } {
            let mut quote = i + 1;
            while bytes.get(quote) == Some(&b'#') {
                quote += 1;
            }
            let hashes = quote - i - 1;
            i = quote + 1;
            while i < bytes.len() {
                if bytes[i] == b'"'
                    && bytes
                        .get(i + 1..i + 1 + hashes)
                        .is_some_and(|s| s.iter().all(|b| *b == b'#'))
                {
                    i += 1 + hashes;
                    break;
                }
                i += 1;
            }
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' => i = (i + 2).min(bytes.len()),
                    b'"' => {
                        i += 1;
                        break;
                    }
                    _ => i += 1,
                }
            }
        } else if bytes[i] == b'\'' && bytes.get(i + 1) == Some(&b'\\') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\'' {
                i += 1;
            }
            i = (i + 1).min(bytes.len());
        } else if bytes[i] == b'\''
            && source[i + 1..]
                .chars()
                .next()
                .is_some_and(|c| bytes.get(i + 1 + c.len_utf8()) == Some(&b'\''))
        {
            i += 2 + source[i + 1..].chars().next().map_or(0, char::len_utf8);
        } else {
            i += 1;
            continue;
        }
        if comment || literals {
            out[start..i].fill(b' ');
        }
    }
    out
}

fn product(source: &str) -> R<String> {
    let code = masked(source, true);
    let mut kept = masked(source, false);
    let mut at = 0;
    while at < code.len() {
        if code[at] != b'#' {
            at += 1;
            continue;
        }
        let Some(end) = code[at..]
            .iter()
            .position(|b| *b == b']')
            .map(|n| at + n + 1)
        else {
            break;
        };
        let attr: Vec<u8> = code[at..end]
            .iter()
            .copied()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        if attr != b"#[cfg(test)]" {
            at += 1;
            continue;
        }
        let start = at;
        at = end;
        let mut nesting = Vec::new();
        while at < code.len() {
            let b = code[at];
            at += 1;
            match b {
                b'(' | b'[' | b'{' => nesting.push(b),
                b')' | b']' | b'}' => {
                    let open = nesting.pop().ok_or("unbalanced test item")?;
                    assert!(
                        matches!((open, b), (b'(', b')') | (b'[', b']') | (b'{', b'}')),
                        "balanced test item"
                    );
                    if b == b'}' && nesting.is_empty() {
                        break;
                    }
                }
                b';' if nesting.is_empty() => break,
                _ => {}
            }
        }
        assert!(nesting.is_empty(), "unclosed test item");
        kept[start..at].fill(b' ');
    }
    Ok(String::from_utf8(kept)?)
}

fn audit(sources: &[(String, String)]) -> R {
    let mut loads = std::collections::BTreeMap::new();
    let mut answers = std::collections::BTreeMap::new();
    let mut task_stops = std::collections::BTreeMap::new();
    let mut consult_stops = std::collections::BTreeMap::new();
    let mut legacy = Vec::new();
    for (name, source) in sources {
        let text = product(source)?;
        for old in [
            "load_budget(",
            "task_budget_stop(",
            "budget_and_stop(",
            "exclude_unused_holds(",
            "retry::budget_stop(",
        ] {
            if text.contains(old) {
                legacy.push((name.to_owned(), old));
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
                let statement: String = statement.chars().filter(|c| !c.is_whitespace()).collect();
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
            "consult/bounds/mod.rs",
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
            task_stops.insert(name.to_owned(), n);
        }
        let n = text.matches(".consult_stop(").count();
        if n > 0 {
            consult_stops.insert(name.to_owned(), n);
        }
        let n = text.matches("budget_view::load(").count();
        if n > 0 {
            loads.insert(name.to_owned(), n);
        }
        let n = text.matches(".any_stop(").count();
        if n > 0 {
            answers.insert(name.to_owned(), n);
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
            ("drafting.rs", 1), // Draft dispatch reads the same transactional view (M3-T04).
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
        expected(&[
            ("admission.rs", 1), // The draft verdict consumes that view without a private rule.
            ("budget_view.rs", 1),
            ("consult/returned.rs", 1),
            ("decide.rs", 1),
            ("rounds.rs", 1)
        ])
    );
    assert_eq!(consult_stops, expected(&[("budget_view.rs", 1)]));
    Ok(())
}

#[test]
fn test_023_01_dc_budget_call_sites_use_one_view() -> R {
    audit(&sources()?)
}

fn injected_after_first_test(file: &str, end: &str) -> R {
    let mut sources = sources()?;
    let (_, source) = sources
        .iter_mut()
        .find(|(name, _)| name == file)
        .ok_or("source")?;
    let marker = source.find("#[cfg(test)]").ok_or("test marker")?;
    let at = marker + source[marker..].find(end).ok_or("test item end")? + end.len();
    source.insert_str(at, "\nfn private_budget_rule() { let sql = \"SELECT used_value + reserved_value >= limit_value FROM budgets\"; }\n");
    let refusal = std::panic::catch_unwind(|| audit(&sources));
    let error = refusal
        .err()
        .ok_or("the budget pin must reject product code after the first test item")?;
    let diagnostic = error
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| error.downcast_ref::<&str>().copied())
        .ok_or("panic message")?;
    assert!(
        diagnostic.contains(&format!("private budget access in {file}")),
        "{diagnostic}"
    );
    Ok(())
}

#[test]
fn test_023_01_dc3_pin_checks_writer_after_first_test_item() -> R {
    injected_after_first_test("writer.rs", "fn nothing() {}")
}

#[test]
fn test_023_01_dc3_pin_checks_inputs_after_first_test_item() -> R {
    injected_after_first_test("inputs.rs", "mod tests;")
}

#[test]
fn test_023_01_dc3_pin_removes_only_annotated_items() -> R {
    let source = r####"fn before() {}
#[cfg(test)]
mod inline { fn test() { let text = r###"} #[cfg(test)] {"###; /* } */ } }
fn after_module() {}
#[cfg(test)]
mod external;
fn after_external() {}
impl Example {
    #[cfg(test)]
    fn helper<'a>(&'a self) { let text = "\"}"; let ch = '}'; }
    fn after_helper() {}
}
#[cfg(test)]
const HELPER: &str = "}";
fn after_const() {}
"####;
    let kept = product(source)?;
    for name in [
        "before",
        "after_module",
        "after_external",
        "after_helper",
        "after_const",
    ] {
        assert!(kept.contains(name), "product item {name} missing: {kept}");
    }
    for name in ["mod inline", "mod external", "fn helper", "const HELPER"] {
        assert!(!kept.contains(name), "test item {name} retained: {kept}");
    }
    Ok(())
}

// Every task state SQL writer is enumerated as a statement, not as a file-wide exemption.
fn consult_writers(sources: &[(String, String)]) -> R {
    let mut writes = Vec::new();
    let mut gateway = false;
    let mut commit_audit = false;
    for (name, source) in sources {
        let text = product(source)?;
        let compact: String = text
            .chars()
            .filter(|c| !c.is_whitespace() && *c != '\\')
            .collect();
        for part in compact.split('"') {
            let sql = part.to_ascii_lowercase();
            if sql.contains("update")
                && (sql.contains("tasksset") || sql.contains("{table}set"))
                && sql.contains("state=")
            {
                writes.push((name.clone(), part.to_owned()));
            }
        }
        if name == "writer.rs" {
            gateway = compact.contains("super::consult::move_task(tx,cas,to,ctx)?");
            commit_audit = source.contains("super::questions::check_invariants(tx)?")
                && source.matches("check_test_invariants(&tx)?").count() == 2;
        }
    }
    writes.sort();
    let mut expected = vec![
        ("consult/returned.rs".to_owned(), "UPDATEtasksSETstate=?1,revision=revision+1,blocked_reason=NULL,next_retry_at=NULLWHEREtask_id=?2ANDrevision=?3".to_owned()),
        ("consult/state.rs".to_owned(), "UPDATEtasksSETstate=?1,revision=revision+1WHEREtask_id=?2ANDrevision=?3AND?1IN('PENDING','READY','LEASED','RUNNING','BLOCKED')".to_owned()),
        ("writer.rs".to_owned(), "UPDATEtasksSETstate=?1,revision=revision+1WHEREtask_id=?2ANDrevision=?3ANDstate=?4ANDNOTEXISTS(SELECT1FROMeventsWHEREkind='task.consult'ANDjson_extract(payload_json,'$.task_id')=?2)".to_owned()),
        ("writer.rs".to_owned(), "UPDATErunsSETstate=?1,revision=revision+1,resume_state=?2WHERErun_id=?3ANDrevision=?4ANDstate=?5".to_owned()),
        ("writer.rs".to_owned(), "UPDATErunsSETstate=?1,revision=revision+1,resume_state=?2,blocked_reason=NULL,blocked_reasons_json='[]'WHERErun_id=?3ANDrevision=?4ANDstate=?5".to_owned()),
    ];
    // The dynamic table Touch statement changes only revision; Run writes cannot address a child.
    expected.retain(|(_, sql)| sql.starts_with("UPDATEtasks"));
    expected.sort();
    assert_eq!(writes, expected, "consult child state writers changed");
    assert!(
        gateway,
        "task CAS must delegate every consult move to its lifecycle"
    );
    assert!(
        commit_audit,
        "all persistence test commits must check the invariants"
    );
    Ok(())
}
#[test]
fn test_023_01_dc4_consult_state_writer_pin() -> R {
    consult_writers(&sources()?)
}
#[test]
fn test_023_01_dc4_injected_state_writer_is_refused() -> R {
    for file in ["writer.rs", "inputs.rs", "consult/returned.rs"] {
        let mut all = sources()?;
        let (_, source) = all
            .iter_mut()
            .find(|(name, _)| name == file)
            .ok_or("source")?;
        source.push_str("\nfn bypass() { tx.execute(\"UPDATE tasks SET title='bypass',state='READY' WHERE task_id=?1\", params![task]); }\n");
        let failure = std::panic::catch_unwind(|| consult_writers(&all));
        assert!(
            failure.is_err(),
            "unowned child state write in {file} escaped"
        );
    }
    Ok(())
}
