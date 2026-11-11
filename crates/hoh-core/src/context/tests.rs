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
    }
}

fn entry(class: Class, path: &str, bytes: usize, scores: (u8, u8, u8)) -> Entry {
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
        causal: scores.1,
        recency: scores.2,
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
fn entries_rank_by_relevance_then_causal_then_recency_then_path_and_sha256() -> R {
    let m = mandatory();
    let big_m = required(DEVELOPER, &m)?;
    // Each entry costs about 40 tokens; the code share fits exactly two.
    let mk = |path: &str, s: (u8, u8, u8)| entry(Class::Code, path, 90, s);
    let entries = vec![
        mk("e", (50, 0, 100)),
        mk("d", (50, 10, 0)),
        mk("c", (60, 0, 0)),
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
    assert_eq!(order, ["c", "b", "d", "a", "e"]);
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
    assert_eq!(p["shares"], json!([45, 35, 20]));
    assert_eq!(p["rule"], "prefix");
    assert_eq!(
        p["order"],
        json!([
            "relevance desc",
            "causal desc",
            "recency desc",
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
