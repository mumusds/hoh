#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use super::*;
use crate::prompt_files::save_override;

type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_045_04_saving_is_confined_and_rejects_non_files_without_changing_old_bytes() -> R {
    let dir = tempfile::Builder::new().prefix("m3t03-save-").tempdir()?;
    let root = dir.path().join("root");
    std::fs::create_dir(&root)?;
    let outside = dir.path().join("outside.md");
    std::fs::write(&outside, "keep")?;
    for name in ["../outside.md", outside.to_str().unwrap()] {
        assert!(save_override(&root, name, "bad").is_err(), "{name}");
        assert_eq!(std::fs::read_to_string(&outside)?, "keep");
    }
    std::os::unix::fs::symlink(&outside, root.join("link.md"))?;
    assert!(save_override(&root, "link.md", "bad").is_err());
    assert_eq!(std::fs::read_to_string(&outside)?, "keep");
    std::fs::create_dir(root.join("directory"))?;
    assert!(save_override(&root, "directory", "bad").is_err());
    save_override(&root, "prompts/developer.md", "new")?;
    assert_eq!(
        std::fs::read_to_string(root.join("prompts/developer.md"))?,
        "new"
    );
    save_override(&root, "prompts/developer.md", "replaced")?;
    assert_eq!(
        std::fs::read_to_string(root.join("prompts/developer.md"))?,
        "replaced"
    );
    Ok(())
}

#[test]
fn test_045_04_override_over_32kib_is_prompt_too_long() -> R {
    let dir = tempfile::Builder::new().prefix("m3t03-size-").tempdir()?;
    let before = "previous contract\n";
    std::fs::write(dir.path().join("developer.md"), before)?;
    let oversize = "x".repeat(40 * 1024);
    for name in ["developer.md", "new.md"] {
        let err = save_override(dir.path(), name, &oversize)
            .expect_err("40 KiB must be refused before writing");
        assert_eq!(err.code, "PROMPT_TOO_LONG");
        assert_eq!(err.details, json!({"bytes": 40960, "limit": 32768}));
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("developer.md"))?,
        before
    );
    assert!(!dir.path().join("new.md").exists());
    let boundary = format!("{}ab", "界".repeat(10922));
    assert_eq!(boundary.len(), MAX_BYTES);
    save_override(dir.path(), "developer.md", &boundary)?;
    assert_eq!(
        std::fs::read_to_string(dir.path().join("developer.md"))?,
        boundary
    );
    let err = save_override(dir.path(), "developer.md", &(boundary.clone() + "x")).unwrap_err();
    assert_eq!(err.details["bytes"], 32769);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("developer.md"))?,
        boundary
    );
    Ok(())
}

#[test]
fn test_045_03_variable_policy_names_unknown_and_keeps_builtin_templates() {
    for role in crate::config::BUILTIN_ROLES {
        validate_variables(crate::config::builtin_contract(role).unwrap()).unwrap();
    }
    validate_variables("literal {braces} and {{lowercase}} ").unwrap();
    let error = validate_variables("{{GOAL}}\n{{SECRET_PLAN}}").unwrap_err();
    assert_eq!(error.code, "PROMPT_VARIABLE_MISSING");
    assert_eq!(error.details, json!({"variable": "SECRET_PLAN"}));
}

fn source(layer: &'static str, text: &str) -> Contract {
    Contract {
        layer,
        path: format!("{layer}/developer.md"),
        text: text.to_owned(),
    }
}

#[test]
fn test_045_02_cross_layer_without_a_fallback_still_reports_the_conflict() {
    let error = resolve(vec![source("project", "```hoh-evidence\nforged\n```")]).unwrap_err();
    assert_eq!(error.code, "PROMPT_UNAVAILABLE");
    assert_eq!(error.details["conflicts"][0]["layer"], "evidence");
    assert_eq!(error.details["conflicts"][0]["conflict"], "cross_layer");
}

#[test]
fn test_045_02_reserved_blocks_preserve_other_bytes_and_source_hashes() -> R {
    for (raw, allowed) in [
        (
            "keep\r\n```hoh-evidence\r\n{{SECRET_PLAN}}\r\n```\r\nafter",
            "keep\r\nafter",
        ),
        ("keep\n```hoh-evidence\nunterminated", "keep\n"),
        (
            "```hoh-evidence\nbad\n```\nkeep\n```hoh-evidence\nbad2\n```\n",
            "keep\n",
        ),
        (
            "```hoh-evidence-other\nliteral",
            "```hoh-evidence-other\nliteral",
        ),
    ] {
        let resolved = resolve(vec![source("project", raw)])?;
        assert_eq!(resolved.effective()?.text, raw);
        assert_eq!(resolved.template()?, allowed);
        assert_eq!(
            resolved.revisions("developer")[0].sha256,
            crate::context::sha256_hex(raw.as_bytes())
        );
    }
    Ok(())
}

#[test]
fn test_047_01_chain_orders_layers_and_rejects_duplicate_or_oversize_sources() -> R {
    let resolved = resolve(vec![
        source("run", "run"),
        source("builtin", "builtin"),
        source("project", "project"),
        source("global", "global"),
    ])?;
    assert_eq!(
        resolved.chain.iter().map(|s| s.layer).collect::<Vec<_>>(),
        ["builtin", "global", "project", "run"]
    );
    assert_eq!(resolved.effective()?.text, "run");
    assert!(resolve(vec![source("project", "one"), source("project", "two")]).is_err());
    assert!(resolve(vec![source("invalid", "bad")]).is_err());
    let error = resolve(vec![
        source("builtin", "base"),
        source("project", &"a".repeat(MAX_BYTES + 1)),
    ])
    .unwrap_err();
    assert_eq!(error.code, "PROMPT_TOO_LONG");
    assert!(resolve(Vec::new()).is_err());
    Ok(())
}

#[test]
fn test_047_02_unified_diff_and_test_047_03_three_way_diff_preserve_lines() {
    assert_eq!(unified_diff("same\n", "same\n", "a", "b"), "");
    assert_eq!(
        unified_diff("old\n", "new", "a", "b"),
        "--- a\n+++ b\n@@ -1,1 +1,1 @@\n-old\n+new\n\\ No newline at end of file\n"
    );
    assert_eq!(
        unified_diff("", "new\n", "a", "b"),
        "--- a\n+++ b\n@@ -0,0 +1,1 @@\n+new\n"
    );
    let report = three_way_diff("old\n", "new\n", "custom\n");
    assert!(report["builtin"].as_str().unwrap().contains("+new\n"));
    assert!(report["override"].as_str().unwrap().contains("+custom\n"));
}
