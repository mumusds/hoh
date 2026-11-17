//! TEST-021-07's read-only judge (docs/adr/ADR-032.md decision 8, design corrections 5 to 7), offline: on the committed
//! S0 excerpts, and on every variant tests/fixtures/commander-readonly/judge_check.py generates (`--emit`), each
//! judged on the product adapters' replay and held to its frozen expected class (M2T20D-851): a "non-PASS" variant
//! never passes, a "same" variant keeps PASS.

// No database access outside hoh-core::persistence (AC-062-05), and no unsafe code.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod readonly;

use readonly::{Agent, Case, R, Verdict, round_verdict};
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn excerpts() -> PathBuf {
    root().join("tests/fixtures/commander-readonly/excerpts")
}

fn control_of(agent: &str) -> &'static str {
    match agent {
        "codex" => "app-control",
        _ => "control",
    }
}

#[test]
fn the_read_only_judge_on_the_s0_excerpts() -> R {
    // ADR-032 decision 8: the six read-only rounds of decision 3's profile PASS; pi with discovered extensions (ro-new,
    // ro-resumed) and without its session directory made first (ro-no-extensions) FAIL on the snapshot.
    let dir = excerpts();
    let mut verdicts = Vec::new();
    for (agent, name, want) in [
        ("claude", "ro-new", Verdict::Pass),
        ("claude", "ro-resumed", Verdict::Pass),
        ("codex", "app-ro-new", Verdict::Pass),
        ("codex", "app-ro-resumed", Verdict::Pass),
        ("pi", "noext-new", Verdict::Pass),
        ("pi", "noext-resumed", Verdict::Pass),
        ("pi", "ro-new", Verdict::Fail),
        ("pi", "ro-resumed", Verdict::Fail),
        ("pi", "ro-no-extensions", Verdict::Fail),
    ] {
        let case = Case::excerpt(&dir, agent, name)?;
        let control = Case::excerpt(&dir, agent, control_of(agent))?;
        let (verdict, why) = round_verdict(&case, Some(&control));
        verdicts.push(format!("{agent}/{name}: {verdict:?} {why:?}"));
        assert_eq!(verdict, want, "{agent}/{name}: {why:?}");
        // No control is no pass.
        assert_ne!(
            round_verdict(&case, None).0,
            Verdict::Pass,
            "{agent}/{name}"
        );
    }
    // The evidence rows the rounds rest on.
    let codex = Case::excerpt(&dir, "codex", "app-ro-new")?;
    assert_eq!(readonly::e3_codex_availability(&codex), "conform");
    assert!(
        readonly::e5_shell_cells(&codex)
            .values()
            .all(|v| *v == "refused")
    );
    assert!(
        readonly::e6_patch_cells(&codex)
            .values()
            .all(|v| *v == "refused")
    );
    let control = Case::excerpt(&dir, "codex", "app-control")?;
    assert_eq!(readonly::e3_codex_availability(&control), "nonconform");
    assert!(
        readonly::e5_shell_cells(&control)
            .values()
            .all(|v| *v == "success")
    );
    let plus_write = Case::excerpt(&dir, "claude", "ro-plus-write")?;
    assert!(
        readonly::e2_claude_tool_results(&plus_write)
            .values()
            .any(|v| *v == "refused")
    );
    let pi_write = Case::excerpt(&dir, "pi", "ro-plus-write")?;
    assert!(
        readonly::e9_pi_tool_results(&pi_write)
            .iter()
            .any(|(k, v)| k.starts_with("write ") && *v == "success")
    );
    println!("{}", verdicts.join("\n"));
    Ok(())
}

#[test]
fn the_read_only_judge_on_every_generated_variant() -> R {
    // The acceptance fixtures (design correction 7): judge_check.py --emit writes every generated variant of the six
    // positive rounds with its inputs and its frozen expected class; this judge, on the adapters' replay, must give
    // each a verdict of that class.
    let out = tempfile::tempdir()?;
    let status = std::process::Command::new("python3")
        .arg("-I")
        .arg(root().join("tests/fixtures/commander-readonly/judge_check.py"))
        .args(["--root", "/nonexistent", "--json"])
        .arg(out.path().join("report.json"))
        .arg("--emit")
        .arg(out.path().join("variants"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()?;
    assert!(status.success(), "judge_check.py --emit: {status}");
    let mut judged = 0usize;
    let mut differ_from_prototype = 0usize;
    let mut wrong = Vec::new();
    for agent_dir in std::fs::read_dir(out.path().join("variants"))? {
        let agent_dir = agent_dir?.path();
        let agent = Agent::of(&agent_dir.file_name().unwrap_or_default().to_string_lossy());
        for round in std::fs::read_dir(&agent_dir)? {
            let round = round?.path();
            let mut variants: Vec<PathBuf> = std::fs::read_dir(&round)?
                .map(|e| e.map(|e| e.path()))
                .collect::<Result<_, _>>()?;
            variants.sort();
            for v in variants {
                let expect: serde_json::Value =
                    serde_json::from_str(&std::fs::read_to_string(v.join("expect.json"))?)?;
                let case = Case::emitted(&v, agent, "")?.ok_or("a variant without its round")?;
                let control = Case::emitted(&v, agent, "control.")?;
                let (verdict, why) = round_verdict(&case, control.as_ref());
                judged += 1;
                let ok = match expect["expect"].as_str() {
                    Some("non-PASS") => verdict != Verdict::Pass,
                    Some("same") => verdict == Verdict::Pass,
                    other => panic!("{}: unknown expectation {other:?}", v.display()),
                };
                let prototype = match expect["prototype_verdict"].as_str() {
                    Some("PASS") => Verdict::Pass,
                    Some("FAIL") => Verdict::Fail,
                    _ => Verdict::Insufficient,
                };
                if prototype != verdict {
                    differ_from_prototype += 1;
                }
                if !ok {
                    wrong.push(format!(
                        "{} [{} / {}] expect {} got {verdict:?}: {why:?}",
                        v.strip_prefix(out.path()).unwrap_or(&v).display(),
                        expect["kind"],
                        expect["label"],
                        expect["expect"]
                    ));
                }
            }
        }
    }
    println!(
        "{judged} variants judged; {differ_from_prototype} differ from the prototype's verdict class"
    );
    assert!(judged > 1000, "{judged} variants");
    assert!(
        wrong.is_empty(),
        "{} of {judged} variants off their class:\n{}",
        wrong.len(),
        wrong
            .iter()
            .take(30)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(())
}
