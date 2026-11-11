use super::*;
use crate::retry::SplitMix64;
use serde_json::{Value, json};

type R = Result<(), Box<dyn std::error::Error>>;

/// The built-in developer contract: four placeholders in the order GOAL, TASK, PROJECT, EVIDENCE.
const DEVELOPER: &str = include_str!("../../../../resources/prompts/developer.md");
/// The built-in commander contract: GOAL, EVIDENCE, TASK, PROJECT.
const COMMANDER: &str = include_str!("../../../../resources/prompts/commander.md");

fn keep(name: &str, value: &Value) {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let _ = std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value).unwrap_or_default(),
        );
    }
}

fn mandatory() -> Mandatory {
    Mandatory {
        goal: "把 HoH 的上下文装箱做完".to_owned(),
        project: "spec_sha256: 5f1c0d2e9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b9c8d7e6f5a4b3c2d1\nspec revision: 3".to_owned(),
        task: "任务：T1 装箱（developer，写者）\n验收标准：\n- AC-1：强制项永不裁剪。\n- AC-2：份额是硬上限。\n候选：CAND-1 commit 0123abc\n输出 Schema：{\"type\":\"object\"}".to_owned(),
        evidence: String::new(),
        lead: String::new(),
        schemas: output_schemas("developer").unwrap_or_default(),
    }
}

/// `scores` = (relevance, recency, causal), the ranking key's order.
fn entry(class: Class, path: &str, bytes: usize, scores: (u64, u64, u64)) -> Entry {
    // Distinct content per path, ASCII so the byte count is exact.
    let seed = format!("{path}:");
    let text: String = seed.chars().cycle().take(bytes).collect();
    Entry {
        class,
        kind: Kind::Plain,
        path: path.to_owned(),
        text,
        summary: None,
        relevance: scores.0,
        recency: scores.1,
        causal: scores.2,
    }
}

fn with_summary(mut e: Entry, bytes: usize) -> Entry {
    let seed = format!("summary of {}:", e.path);
    e.summary = Some(seed.chars().cycle().take(bytes).collect());
    e
}

/// Every piece of a template outside its placeholders, as the packer must keep it byte for byte.
fn contract_pieces(template: &str) -> Vec<&str> {
    let mut pieces = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        pieces.push(&rest[..start]);
        let end = rest[start..]
            .find("}}")
            .map_or(rest.len(), |e| start + e + 2);
        rest = &rest[end..];
    }
    pieces.push(rest);
    pieces.into_iter().filter(|p| !p.is_empty()).collect()
}

/// What each class put in the developer contract's prompt, read from the prompt itself: the code after the task
/// layer's items up to the project heading, the knowledge after the project layer's items up to the evidence heading,
/// the evidence from the evidence layer's start (its items empty here) up to the index pointer.
fn regions<'a>(prompt: &'a str, m: &Mandatory) -> Result<[&'a str; 3], String> {
    let after = |value: &str| {
        prompt
            .find(value)
            .map(|at| at + value.len())
            .ok_or(format!("{value:.40} missing"))
    };
    let until = |from: usize, mark: &str| {
        prompt[from..]
            .find(mark)
            .map(|at| &prompt[from..from + at])
            .ok_or(format!("{mark} missing"))
    };
    // The task layer's items end with the role's schemas.
    let items_end = if m.schemas.is_empty() {
        &m.task
    } else {
        &m.schemas
    };
    let code = until(after(items_end)?, "\n\n### 项目上下文")?;
    let knowledge = until(after(&m.project)?, "\n\n### 证据上下文")?;
    let evidence = until(
        after("\n\n### 证据上下文\n")? + m.evidence.len(),
        "\n\n引用索引",
    )?;
    Ok([code, evidence, knowledge])
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

#[test]
fn the_estimate_is_a_third_of_the_bytes_rounded_up_and_subadditive() {
    assert_eq!(estimate(""), 0);
    assert_eq!(estimate("a"), 1);
    assert_eq!(estimate("abc"), 1);
    assert_eq!(estimate("abcd"), 2);
    // Three bytes per CJK character: one token each.
    assert_eq!(estimate("上下文"), 3);
    let mut rng = SplitMix64::new(0x0039_0000);
    for _ in 0..1_000 {
        let a = "x".repeat((rng.next_u32() % 50) as usize);
        let b = "装".repeat((rng.next_u32() % 50) as usize);
        assert!(estimate(&a) + estimate(&b) >= estimate(&format!("{a}{b}")));
        assert!(estimate(&format!("{a}{b}")) >= estimate(&a));
    }
}

#[test]
fn shares_are_floors_of_45_35_20_of_what_the_mandatory_items_leave() {
    assert_eq!(shares(6_000), [2_700, 2_100, 1_200]);
    assert_eq!(shares(1), [0, 0, 0]);
    assert_eq!(shares(99), [44, 34, 19]);
    assert_eq!(shares(0), [0, 0, 0]);
    // No overflow at the extreme.
    let [c, e, k] = shares(u64::MAX);
    assert!(u128::from(c) + u128::from(e) + u128::from(k) <= u128::from(u64::MAX));
    assert!(
        c > u64::MAX / 3 && e > u64::MAX / 3 && k > u64::MAX / 6,
        "a share wrapped"
    );
}

#[test]
fn test_039_01_shares_are_hard_caps_and_every_omitted_entry_is_indexed() -> R {
    // The mandatory items padded so that, with the developer contract, they estimate at exactly 4 000.
    let mut m = mandatory();
    let base = required(DEVELOPER, &m)?;
    assert!(
        base < 4_000,
        "the fixture's mandatory items already exceed 4 000: {base}"
    );
    m.task.push_str(&"p".repeat((4_000 - base) as usize * 3));
    while required(DEVELOPER, &m)? > 4_000 {
        m.task.pop();
    }
    assert_eq!(required(DEVELOPER, &m)?, 4_000, "the fixture's M");
    // Code 12 000, evidence 9 000, knowledge 6 000 estimated tokens of candidates, several entries each.
    let entries = vec![
        entry(Class::Code, "src/a.rs", 4_500, (90, 10, 50)),
        with_summary(entry(Class::Code, "src/b.rs", 4_500, (80, 90, 10)), 300),
        entry(Class::Code, "src/c.rs", 9_000, (70, 0, 0)),
        entry(Class::Code, "src/d.rs", 17_970, (60, 0, 0)),
        // Small enough to fit what is left, but after one that did not: a reference (failure mode 2).
        entry(Class::Code, "src/tiny.rs", 30, (10, 0, 0)),
        entry(Class::Evidence, "logs/short.txt", 1_500, (99, 0, 0)),
        with_summary(
            entry(Class::Evidence, "reports/qa.json", 9_000, (95, 50, 50)),
            600,
        ),
        entry(Class::Evidence, "logs/build.txt", 9_000, (50, 0, 0)),
        entry(Class::Evidence, "logs/test.txt", 7_500, (40, 0, 0)),
        entry(Class::Knowledge, "docs/07.md", 9_000, (50, 0, 0)),
        Entry {
            kind: Kind::Memory,
            ..entry(Class::Knowledge, "memory/1.md", 900, (60, 0, 0))
        },
        entry(Class::Knowledge, "docs/15.md", 8_100, (40, 0, 0)),
    ];
    let candidate = |c: Class| -> u64 {
        entries
            .iter()
            .filter(|e| e.class == c)
            .map(|e| estimate(&e.text))
            .sum()
    };
    assert_eq!(
        (
            candidate(Class::Code),
            candidate(Class::Evidence),
            candidate(Class::Knowledge)
        ),
        (12_000, 9_000, 6_000),
        "the fixture's candidate sizes"
    );
    let packed = pack(DEVELOPER, &m, &entries, 10_000)?;
    let disposition = |p: &str| {
        packed
            .inputs
            .iter()
            .find(|i| i.path == p)
            .map(|i| i.disposition)
    };
    assert_eq!(disposition("src/tiny.rs"), Some(Disposition::Reference));
    // Mandatory items byte for byte: every piece of the contract and every mandatory value.
    for piece in contract_pieces(DEVELOPER) {
        assert!(
            packed.prompt.contains(piece),
            "contract piece missing: {piece:?}"
        );
    }
    for value in [&m.goal, &m.project, &m.task] {
        assert!(
            packed.prompt.contains(value.as_str()),
            "mandatory value missing"
        );
    }
    // Hard caps per class, the total within the budget, every count estimated.
    assert_eq!(packed.stats.mandatory, 4_000);
    assert_eq!(packed.stats.shares, [2_700, 2_100, 1_200]);
    assert!(packed.stats.code <= 2_700, "code {}", packed.stats.code);
    assert!(
        packed.stats.evidence <= 2_100,
        "evidence {}",
        packed.stats.evidence
    );
    assert!(
        packed.stats.knowledge <= 1_200,
        "knowledge {}",
        packed.stats.knowledge
    );
    assert!(packed.stats.total <= 10_000, "total {}", packed.stats.total);
    assert_eq!(packed.stats.total, estimate(&packed.prompt));
    assert!(packed.stats.estimated);
    // The same caps read from the prompt itself, not from what the packer says it did.
    let [code, evidence, knowledge] = regions(&packed.prompt, &m)?;
    assert!(
        estimate(code) <= 2_700,
        "code in the prompt {}",
        estimate(code)
    );
    assert!(
        estimate(evidence) <= 2_100,
        "evidence in the prompt {}",
        estimate(evidence)
    );
    assert!(
        estimate(knowledge) <= 1_200,
        "knowledge in the prompt {}",
        estimate(knowledge)
    );
    assert!(estimate(&packed.prompt) <= 10_000);
    // Omitted entries counted from the prompt: those whose full text is not in it.
    let omitted = entries
        .iter()
        .filter(|e| !packed.prompt.contains(&e.text))
        .count();
    assert_eq!(
        omitted,
        packed.index.len(),
        "omitted entries vs index items"
    );
    assert!(omitted > 0 && omitted < entries.len());
    // Each class placed something (the fixture is not vacuous) and left something out.
    for class in [Class::Code, Class::Evidence, Class::Knowledge] {
        let of = |d: Disposition| {
            packed
                .inputs
                .iter()
                .filter(|i| i.class == class && i.disposition == d)
                .count()
        };
        assert!(
            of(Disposition::Full) + of(Disposition::Summary) > 0,
            "{class:?} placed nothing"
        );
        assert!(
            of(Disposition::Reference) + of(Disposition::Summary) > 0,
            "{class:?} omitted nothing"
        );
    }
    // Every entry not in full has its index item with path and sha256 of its full text, and nothing else has one.
    let omitted: Vec<&Input> = packed
        .inputs
        .iter()
        .filter(|i| i.disposition != Disposition::Full)
        .collect();
    assert_eq!(omitted.len(), packed.index.len());
    for (input, item) in omitted.iter().zip(&packed.index) {
        assert_eq!((&input.path, &input.sha256), (&item.path, &item.sha256));
        assert!(is_sha256(&item.sha256));
        let e = entries
            .iter()
            .find(|e| e.path == item.path)
            .ok_or("index item of no entry")?;
        assert_eq!(
            item.sha256,
            sha256_hex(e.text.as_bytes()),
            "the index retrieves the full text"
        );
        if item.disposition == Disposition::Reference {
            assert!(
                !packed.prompt.contains(&e.text),
                "a reference-only entry's text is in the prompt"
            );
        }
    }
    // The index object is what the prompt's pointer names.
    assert!(packed.prompt.contains(&packed.index_sha256));
    assert_eq!(packed.index_sha256, sha256_hex(&packed.index_object));
    keep(
        "test_039_01-prompt.txt.json",
        &json!({"sha256": packed.sha256, "prompt": packed.prompt}),
    );
    keep(
        "test_039_01-layers.json",
        &serde_json::to_value(&packed.stats)?,
    );
    keep(
        "test_039_01-index.json",
        &serde_json::from_slice::<Value>(&packed.index_object)?,
    );
    Ok(())
}

#[test]
fn test_039_02_mandatory_items_survive_every_budget_and_below_m_nothing_is_packed() -> R {
    const SEED: u64 = 0x0039_0002;
    let m = mandatory();
    let entries = vec![
        entry(Class::Code, "src/a.rs", 30_000, (90, 0, 0)),
        with_summary(entry(Class::Code, "src/b.rs", 12_000, (50, 0, 0)), 900),
        entry(Class::Evidence, "logs/a.txt", 24_000, (90, 0, 0)),
        with_summary(entry(Class::Evidence, "logs/b.txt", 6_000, (10, 0, 0)), 300),
        entry(Class::Knowledge, "docs/a.md", 18_000, (90, 0, 0)),
        entry(Class::Knowledge, "docs/b.md", 600, (10, 0, 0)),
    ];
    let big_m = required(DEVELOPER, &m)?;
    let mut rng = SplitMix64::new(SEED);
    let mut budgets = vec![big_m - 1, big_m];
    while budgets.len() < 200 {
        budgets.push(big_m - 1 + u64::from(rng.next_u32()) % 20_002);
    }
    let mut cases = Vec::new();
    let mut failures = 0;
    for budget in budgets {
        match pack(DEVELOPER, &m, &entries, budget) {
            Err(PackError::TooSmall {
                required,
                available,
            }) => {
                let ok = budget < big_m && required == big_m && available == budget;
                failures += usize::from(!ok);
                cases.push(
                    json!({"budget": budget, "result": "CONTEXT_BUDGET_TOO_SMALL", "ok": ok}),
                );
            }
            Err(other) => return Err(format!("budget {budget}: {other:?}").into()),
            Ok(packed) => {
                let complete = [&m.goal, &m.project, &m.task]
                    .iter()
                    .all(|v| packed.prompt.contains(v.as_str()))
                    && contract_pieces(DEVELOPER)
                        .iter()
                        .all(|p| packed.prompt.contains(p));
                let within = packed.stats.total <= budget && estimate(&packed.prompt) <= budget;
                let ok = budget >= big_m && complete && within;
                failures += usize::from(!ok);
                cases.push(
                    json!({"budget": budget, "result": "packed", "total": packed.stats.total,
                    "sha256": packed.sha256, "ok": ok}),
                );
            }
        }
    }
    keep(
        "test_039_02-cases.json",
        &json!({"seed": SEED, "mandatory": big_m, "cases": cases, "failures": failures}),
    );
    assert_eq!(cases[0]["result"], "CONTEXT_BUDGET_TOO_SMALL");
    assert_eq!(cases[1]["result"], "packed");
    assert_eq!(failures, 0, "{cases:#?}");
    Ok(())
}

#[test]
fn entries_rank_by_relevance_then_recency_then_causal_then_path_and_sha256() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    // Each entry costs the same; the code share fits exactly two.
    let mk = |path: &str, s: (u64, u64, u64)| entry(Class::Code, path, 90, s);
    let entries = vec![
        mk("e", (50, 0, 100)),
        mk("d", (50, 10, 0)),
        mk("c", (60, 0, 0)),
        mk("f", (50, 0, 7)),
        mk("b", (50, 10, 0)),
        mk("a", (50, 0, 100)),
    ];
    let cost = estimate(&block(&entries[0], Disposition::Full));
    let packed = pack(
        DEVELOPER,
        &m,
        &entries,
        big_m + (2 * cost * 100).div_ceil(45),
    )?;
    let order: Vec<&str> = packed.inputs.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(order, ["c", "b", "d", "a", "e", "f"]);
    let full: Vec<&str> = packed
        .inputs
        .iter()
        .filter(|i| i.disposition == Disposition::Full)
        .map(|i| i.path.as_str())
        .collect();
    assert_eq!(full, ["c", "b"]);
    Ok(())
}

#[test]
fn once_an_entry_fits_neither_in_full_nor_as_a_summary_every_later_one_is_a_reference() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let entries = vec![
        entry(Class::Evidence, "first", 300, (90, 0, 0)),
        entry(Class::Evidence, "too-big", 30_000, (80, 0, 0)),
        entry(Class::Evidence, "small-after", 3, (70, 0, 0)),
    ];
    // Evidence share of about 1 000 tokens: "first" fits, "too-big" does not, "small-after" would.
    let packed = pack(DEVELOPER, &m, &entries, big_m + 1_000 * 100 / 35 + 1)?;
    let disposition = |p: &str| {
        packed
            .inputs
            .iter()
            .find(|i| i.path == p)
            .map(|i| i.disposition)
    };
    assert_eq!(disposition("first"), Some(Disposition::Full));
    assert_eq!(disposition("too-big"), Some(Disposition::Reference));
    assert_eq!(disposition("small-after"), Some(Disposition::Reference));
    Ok(())
}

#[test]
fn an_entry_too_big_in_full_goes_in_as_its_own_summary_and_the_prefix_goes_on() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let entries = vec![
        with_summary(entry(Class::Knowledge, "big", 30_000, (90, 0, 0)), 60),
        entry(Class::Knowledge, "next", 60, (80, 0, 0)),
    ];
    let packed = pack(DEVELOPER, &m, &entries, big_m + 500 * 100 / 20)?;
    let summary = entries[0].summary.as_deref().ok_or("summary")?;
    assert!(packed.prompt.contains(summary));
    assert!(!packed.prompt.contains(&entries[0].text));
    assert_eq!(packed.inputs[0].disposition, Disposition::Summary);
    assert_eq!(packed.inputs[1].disposition, Disposition::Full);
    assert_eq!(packed.index.len(), 1);
    assert_eq!(packed.index[0].disposition, Disposition::Summary);
    Ok(())
}

#[test]
fn a_budget_exactly_m_packs_no_entry_and_indexes_them_all() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let entries = vec![
        entry(Class::Code, "a", 3, (1, 1, 1)),
        entry(Class::Knowledge, "b", 3, (1, 1, 1)),
    ];
    let packed = pack(DEVELOPER, &m, &entries, big_m)?;
    assert_eq!(packed.stats.total, big_m);
    assert_eq!(packed.index.len(), 2);
    assert!(
        packed
            .inputs
            .iter()
            .all(|i| i.disposition == Disposition::Reference)
    );
    Ok(())
}

#[test]
fn the_same_inputs_in_any_order_pack_to_the_same_bytes() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let mut entries = vec![
        entry(Class::Code, "a", 900, (5, 5, 5)),
        entry(Class::Code, "b", 900, (5, 5, 5)),
        with_summary(entry(Class::Evidence, "c", 3_000, (5, 5, 5)), 90),
        entry(Class::Knowledge, "d", 300, (5, 5, 5)),
    ];
    let one = pack(DEVELOPER, &m, &entries, big_m + 1_500)?;
    entries.reverse();
    let two = pack(DEVELOPER, &m, &entries, big_m + 1_500)?;
    assert_eq!(one.prompt, two.prompt);
    assert_eq!(one.sha256, two.sha256);
    assert_eq!(one.index_object, two.index_object);
    assert_eq!(one.sha256, sha256_hex(one.prompt.as_bytes()));
    Ok(())
}

#[test]
fn a_memory_entry_is_labelled_as_experience_not_acceptance_fact() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let memory = Entry {
        kind: Kind::Memory,
        ..entry(Class::Knowledge, "memory/lesson.md", 60, (9, 9, 9))
    };
    let plain = entry(Class::Knowledge, "docs/x.md", 60, (1, 1, 1));
    let packed = pack(DEVELOPER, &m, &[memory, plain], big_m + 2_000)?;
    let line = |path: &str| {
        packed
            .prompt
            .lines()
            .find(|l| l.starts_with("#### ") && l.contains(path))
            .map(str::to_owned)
    };
    let memory_line = line("memory/lesson.md").ok_or("memory entry not placed")?;
    assert!(memory_line.contains(MEMORY_LABEL), "{memory_line}");
    let plain_line = line("docs/x.md").ok_or("plain entry not placed")?;
    assert!(!plain_line.contains(MEMORY_LABEL), "{plain_line}");
    Ok(())
}

#[test]
fn layers_fill_the_templates_own_placeholders_in_its_own_order() -> R {
    let m = Mandatory {
        evidence: "证据强制项".to_owned(),
        ..mandatory()
    };
    let entries = vec![
        entry(Class::Code, "code.rs", 30, (1, 1, 1)),
        entry(Class::Evidence, "evidence.log", 30, (1, 1, 1)),
        entry(Class::Knowledge, "knowledge.md", 30, (1, 1, 1)),
    ];
    for template in [DEVELOPER, COMMANDER] {
        let big_m = required(template, &m)?;
        let packed = pack(template, &m, &entries, big_m + 2_000)?;
        assert!(!packed.prompt.contains("{{"), "a placeholder left");
        let at = |s: &str| packed.prompt.find(s).ok_or_else(|| format!("{s} missing"));
        let (goal, task, project, evidence) = (
            at(&m.goal)?,
            at(&m.task)?,
            at(&m.project)?,
            at("证据强制项")?,
        );
        // Each class lands in its layer, after that layer's mandatory items.
        assert!(
            at("code.rs")? > task
                && at("knowledge.md")? > project
                && at("evidence.log")? > evidence
        );
        let (g, t, p, e) = (
            template.find("{{GOAL}}").ok_or("GOAL")?,
            template.find("{{TASK_CONTEXT}}").ok_or("TASK")?,
            template.find("{{PROJECT_CONTEXT}}").ok_or("PROJECT")?,
            template.find("{{EVIDENCE_CONTEXT}}").ok_or("EVIDENCE")?,
        );
        let mut want = [("g", g), ("t", t), ("p", p), ("e", e)];
        want.sort_by_key(|(_, at)| *at);
        let mut got = [("g", goal), ("t", task), ("p", project), ("e", evidence)];
        got.sort_by_key(|(_, at)| *at);
        assert_eq!(want.map(|(n, _)| n), got.map(|(n, _)| n));
    }
    Ok(())
}

#[test]
fn a_contract_without_placeholders_comes_first_verbatim_and_the_layers_follow_in_a_fixed_order() -> R
{
    let contract = "你是安全审查员。\n只看安全问题。\n";
    let m = mandatory();
    let big_m = required(contract, &m)?;
    let packed = pack(contract, &m, &[], big_m)?;
    assert!(packed.prompt.starts_with(contract.trim_end()));
    let at = |s: &str| packed.prompt.find(s).ok_or_else(|| format!("{s} missing"));
    assert!(at(&m.goal)? < at(&m.project)? && at(&m.project)? < at(&m.task)?);
    assert!(at(&m.task)? < at(&packed.index_sha256)?);
    Ok(())
}

#[test]
fn a_template_with_some_unknown_or_repeated_placeholders_is_refused() {
    let partial = "x {{GOAL}} y {{TASK_CONTEXT}}";
    let unknown = "{{GOAL}} {{TASK_CONTEXT}} {{PROJECT_CONTEXT}} {{EVIDENCE_CONTEXT}} {{OTHER}}";
    let repeated = "{{GOAL}} {{GOAL}} {{TASK_CONTEXT}} {{PROJECT_CONTEXT}} {{EVIDENCE_CONTEXT}}";
    for template in [partial, unknown, repeated] {
        assert!(
            matches!(
                pack(template, &mandatory(), &[], 1_000_000),
                Err(PackError::Template(_))
            ),
            "{template}"
        );
    }
}

#[test]
fn a_blank_goal_project_or_task_layer_is_refused_and_values_are_never_substituted_again() -> R {
    for blank in ["goal", "project", "task"] {
        let mut m = mandatory();
        match blank {
            "goal" => m.goal = " \n".to_owned(),
            "project" => m.project = String::new(),
            _ => m.task = "\t".to_owned(),
        }
        assert!(
            matches!(
                pack(DEVELOPER, &m, &[], 1_000_000),
                Err(PackError::EmptyLayer(_))
            ),
            "{blank}"
        );
    }
    // A value naming a placeholder is text, not a second substitution.
    let m = Mandatory {
        goal: "写出 {{TASK_CONTEXT}} 字样".to_owned(),
        ..mandatory()
    };
    let packed = pack(DEVELOPER, &m, &[], 1_000_000)?;
    assert!(packed.prompt.contains("写出 {{TASK_CONTEXT}} 字样"));
    assert_eq!(packed.prompt.matches(&m.task).count(), 1);
    Ok(())
}

#[test]
fn the_policy_names_the_estimator_the_shares_the_order_and_the_rule() {
    let p = policy();
    assert_eq!(p["estimator"], "utf8-bytes/3");
    assert_eq!(p["format"], FORMAT);
    assert_eq!(p["shares"], json!([45, 35, 20]));
    assert_eq!(p["rule"], "prefix");
    assert_eq!(
        p["order"],
        json!([
            "relevance desc",
            "recency desc",
            "causal desc",
            "path asc",
            "sha256 asc"
        ])
    );
}

#[test]
fn every_built_in_contract_keeps_each_piece_and_takes_its_four_layers_once() -> R {
    let m = Mandatory {
        evidence: "证据强制项".to_owned(),
        ..mandatory()
    };
    for role in [
        "commander",
        "cto",
        "developer",
        "qa",
        "researcher",
        "reviewer",
    ] {
        let template = crate::config::builtin_contract(role).ok_or(role)?;
        let packed = pack(template, &m, &[], required(template, &m)?)?;
        for piece in contract_pieces(template) {
            assert!(
                packed.prompt.contains(piece),
                "{role}: contract piece missing: {piece:?}"
            );
        }
        for value in [&m.goal, &m.project, &m.task, &"证据强制项".to_owned()] {
            assert_eq!(
                packed.prompt.matches(value.as_str()).count(),
                1,
                "{role}: {value}"
            );
        }
        assert!(!packed.prompt.contains("{{"), "{role}: a placeholder left");
        assert_eq!(packed.stats.total, packed.stats.mandatory, "{role}");
    }
    Ok(())
}

#[test]
fn one_class_left_unused_lends_nothing_to_another() -> R {
    // REQ-039 decision 1: hard caps, no borrowing.
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let entries = vec![
        entry(Class::Code, "small.rs", 3, (1, 1, 1)),
        entry(Class::Evidence, "e1", 600, (9, 0, 0)),
        entry(Class::Evidence, "e2", 600, (8, 0, 0)),
        entry(Class::Evidence, "e3", 600, (7, 0, 0)),
    ];
    let budget = big_m + 1_000;
    let packed = pack(DEVELOPER, &m, &entries, budget)?;
    let [code, evidence, _] = regions(&packed.prompt, &m)?;
    assert!(
        estimate(code) < packed.stats.shares[0] / 2,
        "the code share is mostly unused"
    );
    assert!(
        estimate(evidence) <= packed.stats.shares[1],
        "evidence {}",
        estimate(evidence)
    );
    assert!(
        packed
            .inputs
            .iter()
            .any(|i| i.class == Class::Evidence && i.disposition == Disposition::Reference),
        "the evidence that did not fit its own share stays a reference"
    );
    Ok(())
}

#[test]
fn every_byte_is_counted_whatever_the_sizes_and_budgets() -> R {
    // Random entries (ASCII and three-byte text, summaries or none) and budgets from M up: the whole prompt and each
    // class read back from it stay within the budget and the shares (separators included).
    const SEED: u64 = 0x0039_0055;
    let mut rng = SplitMix64::new(SEED);
    let m = Mandatory {
        evidence: "证据".to_owned(),
        ..mandatory()
    };
    let big_m = required(DEVELOPER, &m)?;
    for case in 0..300 {
        let mut entries = Vec::new();
        for n in 0..(rng.next_u32() % 8) {
            let class =
                [Class::Code, Class::Evidence, Class::Knowledge][(rng.next_u32() % 3) as usize];
            let bytes = (rng.next_u32() % 1_500) as usize;
            let mut e = entry(
                class,
                &format!("c{case}/{n}"),
                bytes,
                (u64::from(rng.next_u32() % 5), 0, 0),
            );
            if rng.next_u32().is_multiple_of(2) {
                e.text = "装".repeat(bytes / 3 + 1);
            }
            if rng.next_u32().is_multiple_of(3) {
                e = with_summary(e, (rng.next_u32() % 200) as usize);
            }
            entries.push(e);
        }
        let budget = big_m + u64::from(rng.next_u32() % 3_000);
        let packed = pack(DEVELOPER, &m, &entries, budget)?;
        assert!(
            estimate(&packed.prompt) <= budget,
            "case {case}: {} > {budget}",
            estimate(&packed.prompt)
        );
        let [code, evidence, knowledge] = regions(&packed.prompt, &m)?;
        for (n, region) in [code, evidence, knowledge].iter().enumerate() {
            assert!(
                estimate(region) <= packed.stats.shares[n],
                "case {case} class {n}"
            );
        }
    }
    Ok(())
}

#[test]
fn a_lead_goes_first_in_the_first_placeholders_value_before_any_entry() -> R {
    let lead = "交接包：首个输入".to_owned();
    let m = Mandatory {
        lead: lead.clone(),
        evidence: "证据强制项".to_owned(),
        ..mandatory()
    };
    let entries = vec![
        entry(Class::Code, "code.rs", 30, (1, 1, 1)),
        entry(Class::Evidence, "evidence.log", 30, (1, 1, 1)),
        entry(Class::Knowledge, "knowledge.md", 30, (1, 1, 1)),
    ];
    // The built-in contracts begin with GOAL: the lead follows the goal and precedes every other context, the
    // commander's evidence included.
    for template in [DEVELOPER, COMMANDER] {
        let packed = pack(template, &m, &entries, required(template, &m)? + 2_000)?;
        let at = |s: &str| packed.prompt.find(s).ok_or_else(|| format!("{s} missing"));
        let lead_at = at(&lead)?;
        assert!(at(&m.goal)? < lead_at);
        for later in [
            m.project.as_str(),
            m.task.as_str(),
            "证据强制项",
            "code.rs",
            "evidence.log",
            "knowledge.md",
        ] {
            assert!(lead_at < at(later)?, "{later} before the lead");
        }
        assert_eq!(packed.prompt.matches(&lead).count(), 1);
    }
    // A template that begins with EVIDENCE: the lead after that layer's items, before its entries.
    let evidence_first =
        "前言\n{{EVIDENCE_CONTEXT}}\n---\n{{GOAL}}\n{{TASK_CONTEXT}}\n{{PROJECT_CONTEXT}}\n";
    let packed = pack(
        evidence_first,
        &m,
        &entries,
        required(evidence_first, &m)? + 2_000,
    )?;
    let at = |s: &str| packed.prompt.find(s).ok_or_else(|| format!("{s} missing"));
    assert!(at("证据强制项")? < at(&lead)? && at(&lead)? < at("evidence.log")?);
    assert!(at(&lead)? < at(&m.goal)?);
    Ok(())
}

#[test]
fn the_layers_are_counted_as_req_039_names_them() -> R {
    let m = mandatory();
    let packed = pack(DEVELOPER, &m, &[], required(DEVELOPER, &m)?)?;
    let l = packed.stats.layers;
    let contract: String = contract_pieces(DEVELOPER).concat();
    assert!(!m.schemas.is_empty());
    assert_eq!(
        l.role_contract,
        estimate(&format!("{contract}{}", m.schemas))
    );
    assert_eq!(l.task, estimate(&format!("{}{}", m.goal, m.task)));
    assert_eq!(l.project, estimate(&m.project));
    assert!(l.evidence > 0, "the index pointer");
    Ok(())
}

#[test]
fn the_golden_prompts_change_only_with_the_format() -> R {
    // docs/adr/ADR-026.md decision 7 (M2T09D-160): a change to what this code renders must change FORMAT (and so the
    // resource bundle version), or an old prompt would repack differently under the same version. The cases cover
    // every rendering branch: placeholders in their own order and none, the lead, a summary, the memory label, control
    // characters in a path, an empty and a non-empty evidence item, a reference only.
    let in_place = "契约\n### 目标\n{{GOAL}}\n### 证据\n{{EVIDENCE_CONTEXT}}\n### 任务\n{{TASK_CONTEXT}}\n### 项目\n{{PROJECT_CONTEXT}}\n";
    let none = "只有契约正文。\n\n";
    let entries = vec![
        with_summary(entry(Class::Code, "a\n\tb.rs", 9_000, (9, 1, 1)), 30),
        entry(Class::Evidence, "b.log", 60, (5, 2, 0)),
        Entry {
            kind: Kind::Memory,
            ..entry(Class::Knowledge, "m.md", 60, (5, 0, 2))
        },
        entry(Class::Knowledge, "big.md", 90_000, (9, 0, 0)),
    ];
    let mut shas = Vec::new();
    for (template, lead, evidence) in [(in_place, "lead", "ev"), (none, "", ""), (in_place, "", "")]
    {
        let m = Mandatory {
            lead: lead.to_owned(),
            evidence: evidence.to_owned(),
            // Its own schemas text: the golden depends on this code only, not on a schema file's text.
            schemas: "schemas".to_owned(),
            ..mandatory()
        };
        let packed = pack(template, &m, &entries, required(template, &m)? + 3_000)?;
        let dispositions: Vec<Disposition> = packed.inputs.iter().map(|i| i.disposition).collect();
        assert!(
            dispositions.contains(&Disposition::Summary)
                && dispositions.contains(&Disposition::Reference)
        );
        shas.push(packed.sha256);
    }
    let golden: [(&str, [&str; 3]); 1] = [("hoh-context/1", GOLDEN_1)];
    let want = golden
        .iter()
        .find(|(f, _)| *f == FORMAT)
        .map(|(_, s)| s.map(str::to_owned).to_vec());
    assert_eq!(
        Some(shas),
        want,
        "the rendering changed: before the first release update GOLDEN_1 under review; after it, bump context::FORMAT \
         and add its golden"
    );
    Ok(())
}

/// The golden prompts' sha256 under FORMAT hoh-context/1.
const GOLDEN_1: [&str; 3] = [
    "fa0674eed4ff17247c20a741cd7c9c9a12cc0cf18350ce9b041a594e0883acd8",
    "29f861e1a3301b776cca9e491fbec6c1c2ea78d27edca5cff7662101362efe04",
    "3e6bb95a336ff03084989764d5340ea40cb7b975521144d676b6740bb2559489",
];

#[test]
fn an_entry_that_fills_its_share_to_the_byte_fits_and_one_byte_more_does_not() -> R {
    // No slack anywhere: the block is exactly three times the code share in bytes, so any byte the packer added
    // without counting it (a separator, a newline) would show in the code read back from the prompt.
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    let share = 500u64;
    let budget = big_m + (share * 100).div_ceil(45);
    assert_eq!(shares(budget - big_m)[0], share);
    let header = block(
        &entry(Class::Code, "exact.rs", 0, (1, 1, 1)),
        Disposition::Full,
    )
    .len();
    let fits = entry(
        Class::Code,
        "exact.rs",
        3 * share as usize - header,
        (1, 1, 1),
    );
    assert_eq!(block(&fits, Disposition::Full).len() as u64, 3 * share);
    let packed = pack(DEVELOPER, &m, std::slice::from_ref(&fits), budget)?;
    assert_eq!(packed.inputs[0].disposition, Disposition::Full);
    let [code, _, _] = regions(&packed.prompt, &m)?;
    assert_eq!(code.len() as u64, 3 * share, "exactly the block's bytes");
    assert_eq!(estimate(code), share);
    let over = entry(
        Class::Code,
        "exact.rs",
        3 * share as usize - header + 1,
        (1, 1, 1),
    );
    let packed = pack(DEVELOPER, &m, &[over], budget)?;
    assert_eq!(packed.inputs[0].disposition, Disposition::Reference);
    Ok(())
}

#[test]
fn each_role_is_given_the_schemas_its_contract_says_it_hands_back_with_every_reference_inside() -> R
{
    // docs/adr/ADR-026.md decision 3 (M2T09D-65c): the output contract sections of resources/prompts/*.md.
    let want: [(&str, &[&str]); 7] = [
        ("commander", &["Message", "DecisionPayload"]),
        ("developer", &["TaskResult"]),
        ("cto", &["TaskResult"]),
        ("researcher", &["TaskResult"]),
        (
            "qa",
            &["TaskResult", "AcceptanceObservation", "IndependentReport"],
        ),
        ("reviewer", &["TaskResult", "IndependentReport"]),
        ("security", &["TaskResult"]),
    ];
    for (role, names) in want {
        let text = output_schemas(role)?;
        let headings: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("#### 输出 Schema："))
            .map(|l| l.split(['（', ' ']).next().unwrap_or(l))
            .collect();
        assert_eq!(headings, names, "{role}");
        // Each block is one JSON object whose every local reference it defines itself.
        for block in text.lines().filter(|l| l.starts_with('{')) {
            let schema: Value = serde_json::from_str(block)?;
            let defs = schema.get("$defs").and_then(Value::as_object);
            for reference in block.split("\"#/$defs/").skip(1) {
                let name = reference.split('"').next().unwrap_or_default();
                assert!(
                    defs.is_some_and(|d| d.contains_key(name)),
                    "{role}: {name} referenced but not defined"
                );
            }
        }
        assert_eq!(text, output_schemas(role)?, "deterministic");
        // The report is only what the agent writes: never a field the bridge fills in.
        if names.contains(&"IndependentReport") {
            let report = text
                .lines()
                .skip_while(|l| !l.contains("IndependentReport"))
                .nth(1)
                .unwrap_or_default();
            let report: Value = serde_json::from_str(report)?;
            let keys: Vec<&str> = report["properties"]
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            assert_eq!(keys, ["findings", "role", "scope", "status"], "{role}");
        }
    }
    Ok(())
}

#[test]
fn the_same_entry_given_twice_with_other_scores_goes_in_once() -> R {
    // docs/adr/ADR-026.md decision 4 (M2T09D-157): one per (class, path, sha256), the higher ranked kept.
    let m = mandatory();
    let low = entry(Class::Evidence, "same.log", 60, (1, 0, 0));
    let high = Entry {
        relevance: 9,
        ..low.clone()
    };
    let between = entry(Class::Evidence, "other.log", 60, (5, 0, 0));
    let packed = pack(
        DEVELOPER,
        &m,
        &[low.clone(), between, high],
        required(DEVELOPER, &m)? + 3_000,
    )?;
    let paths: Vec<&str> = packed.inputs.iter().map(|i| i.path.as_str()).collect();
    assert_eq!(
        paths,
        ["same.log", "other.log"],
        "kept once, at the higher rank"
    );
    assert_eq!(packed.prompt.matches(&low.text).count(), 1);
    Ok(())
}

#[test]
fn a_runs_own_shares_are_the_caps() -> R {
    // docs/adr/ADR-026.md decision 7 (M2T09D-151): pack_with takes the run's shares; more than 100 is refused.
    let m = mandatory();
    let entries = vec![
        entry(Class::Code, "a.rs", 3_000, (1, 0, 0)),
        entry(Class::Knowledge, "k.md", 3_000, (1, 0, 0)),
    ];
    let budget = required(DEVELOPER, &m)? + 2_000;
    let packed = pack_with(DEVELOPER, &m, &entries, budget, [0, 0, 100])?;
    assert_eq!(packed.stats.shares, [0, 0, 2_000]);
    let disposition = |p: &str| {
        packed
            .inputs
            .iter()
            .find(|i| i.path == p)
            .map(|i| i.disposition)
    };
    assert_eq!(disposition("a.rs"), Some(Disposition::Reference));
    assert_eq!(disposition("k.md"), Some(Disposition::Full));
    assert!(matches!(
        pack_with(DEVELOPER, &m, &entries, budget, [50, 50, 1]),
        Err(PackError::Template(_))
    ));
    Ok(())
}

#[test]
fn a_turns_mandatory_items_say_what_is_missing_instead_of_leaving_it_out() -> R {
    let none = Facts::default();
    let m = super::mandatory(&none, "developer", "", None, None)?;
    assert_eq!(m.goal, "Run 目标：（未给出）");
    assert_eq!(m.project, "spec：尚无 spec");
    assert!(
        m.task.contains("（无）") && m.task.contains("尚无候选"),
        "{}",
        m.task
    );
    assert!(m.schemas.contains("TaskResult"));
    let facts = Facts {
        goal: Some("交付 v1".to_owned()),
        question: Some("唤醒 W（high）".to_owned()),
        spec: Some(("f".repeat(64), 3)),
        scope: vec!["T1 装箱（developer，写者）".to_owned()],
        acceptance: vec![
            ("AC-1".to_owned(), "正文一 {{GOAL}}".to_owned()),
            ("AC-2".to_owned(), "正文二".to_owned()),
        ],
        candidate: Some(("CAND-1".to_owned(), "a".repeat(40), "b".repeat(40))),
        header: vec![("nonce".to_owned(), "n-1".to_owned())],
        ..Facts::default()
    };
    let m = super::mandatory(
        &facts,
        "commander",
        "LEAD",
        Some("/p/.hoh/attempts/A/messages.jsonl"),
        Some("/p"),
    )?;
    assert_eq!(
        m.goal,
        "Run 目标：交付 v1\n本次唤醒要回答的问题：唤醒 W（high）"
    );
    assert!(m.project.contains(&"f".repeat(64)) && m.project.contains("revision：3"));
    for want in [
        "- AC-1：正文一 {{GOAL}}",
        "- AC-2：正文二",
        "candidate_id：CAND-1",
        &"a".repeat(40),
        &"b".repeat(40),
        "- nonce：n-1",
        "- 输出文件：/p/.hoh/attempts/A/messages.jsonl",
        "- 项目根：/p",
    ] {
        assert!(m.task.contains(want), "{want} missing from {}", m.task);
    }
    assert_eq!(m.lead, "LEAD");
    assert!(m.schemas.contains("DecisionPayload"));
    // Its acceptance text stays a value: packed whole, never read as a placeholder.
    let packed = pack(COMMANDER, &m, &[], required(COMMANDER, &m)?)?;
    assert!(packed.prompt.contains("- AC-1：正文一 {{GOAL}}"));
    Ok(())
}

#[test]
fn a_first_input_is_the_package_itself_or_a_marked_summary_of_another_shape() -> R {
    // docs/adr/ADR-026.md decision 3, v4 (M2T09D-201): a summary never passes for the package.
    let small = json!({"handoff_kind": "role_change", "state": {"open_issue_ids": ["i1", "i2"], "applied_decision_ids": []}});
    let text = super::lead(&small, ".hoh/objects/sha256/aa/x");
    assert!(text.starts_with("#### 交接包（本会话的首个输入）"));
    let block = |text: &str| -> Result<Value, Box<dyn std::error::Error>> {
        Ok(serde_json::from_str(
            text.split("```json\n")
                .nth(1)
                .and_then(|t| t.split("\n```").next())
                .ok_or("no block")?,
        )?)
    };
    assert_eq!(block(&text)?, small, "within the cap, the package as it is");
    assert!(!text.contains(".hoh/objects"));
    let ids: Vec<String> = (0..10_000).map(|n| format!("m{n:05}")).collect();
    let big = json!({"handoff_kind": "role_change", "state": {"unacked_message_ids": ids, "open_issue_ids": ["i1"]}});
    let text = super::lead(&big, ".hoh/objects/sha256/aa/x");
    assert!(text.len() < 10_000, "bounded: {}", text.len());
    assert!(
        text.starts_with("#### 交接包摘要（不是完整交接包"),
        "{text:.80}"
    );
    assert!(text.contains(".hoh/objects/sha256/aa/x"));
    let shown = block(&text)?;
    assert!(
        shown.get("state").is_none(),
        "no ContextHandoff state: it cannot pass for the package"
    );
    assert_eq!(shown["handoff_kind"], "role_change");
    assert_eq!(
        shown["state_summary"]["unacked_message_ids"]["total"],
        10_000
    );
    assert_eq!(
        shown["state_summary"]["unacked_message_ids"]["first"]
            .as_array()
            .map(Vec::len),
        Some(LEAD_IDS)
    );
    assert_eq!(
        shown["state_summary"]["open_issue_ids"],
        json!({"total": 1, "first": ["i1"]})
    );
    Ok(())
}
