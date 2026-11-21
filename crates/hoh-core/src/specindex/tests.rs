use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};

type R = Result<(), Box<dyn std::error::Error>>;
const PREFIX: &str =
    "# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n";

fn source(path: &str, body: &str) -> Source {
    Source {
        path: path.to_owned(),
        bytes: format!("{PREFIX}{body}").into_bytes(),
    }
}

fn requirement(id: &str, class: &str) -> String {
    format!(
        "## REQ-{id} 标题\n{class}### AC-{id}-01\ngiven: 前提\nwhen: 操作\nthen: 结果\ncheck: check-{id}\n"
    )
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn test_034_04_generator_canonical_bytes_and_source_digest() -> R {
    let a = source("docs/spec/a.md", &requirement("A", ""));
    let b = source("docs/spec/b.md", &requirement("B", "class: degradable\n"));
    let index = generate_index(&[b.clone(), a.clone()])?;
    let expected = json!({
        "generated": "生成，勿改",
        "files": [{"path": a.path, "sha256": sha(&a.bytes)}, {"path": b.path, "sha256": sha(&b.bytes)}],
        "requirements": [
            {"id": "REQ-A", "title": "标题", "release_class": "required", "release_targets": ["project"],
             "acceptance": [{"id": "AC-A-01", "given": "前提", "when": "操作", "then": "结果", "test_id": "TEST-A-01"}]},
            {"id": "REQ-B", "title": "标题", "release_class": "degradable", "release_targets": ["project"],
             "acceptance": [{"id": "AC-B-01", "given": "前提", "when": "操作", "then": "结果", "test_id": "TEST-B-01"}]}
        ],
        "scenarios": [
            {"id": "TEST-A-01", "requirement_id": "REQ-A", "acceptance_id": "AC-A-01", "check_id": "check-A"},
            {"id": "TEST-B-01", "requirement_id": "REQ-B", "acceptance_id": "AC-B-01", "check_id": "check-B"}
        ]
    });
    assert_eq!(
        index.value(),
        &expected,
        "Markdown requirements and scenarios must populate the index"
    );
    let bytes = format!("{}\n", serde_json::to_string_pretty(&expected)?).into_bytes();
    assert_eq!(index.bytes(), bytes);
    assert_eq!(
        index.bytes(),
        generate_index(&[a.clone(), b.clone()])?.bytes()
    );
    let mut changed = a.clone();
    changed.bytes.push(b'\n');
    assert_ne!(
        sha(index.bytes()),
        sha(generate_index(&[changed, b])?.bytes())
    );
    Ok(())
}

#[test]
fn test_034_04_generation_reports_drift_without_mutating_sources() -> R {
    let files = vec![source("docs/spec/a.md", &requirement("A", ""))];
    let before = files.clone();
    let index = generate_index(&files)?;
    assert_eq!(index.drift(None), None);
    assert_eq!(index.drift(Some(index.bytes())), None);
    assert_eq!(
        index.drift(Some(b"manually changed")),
        Some(json!({"path": PATH}))
    );
    assert_eq!(files, before);
    assert_eq!(generate_index(&files)?.bytes(), index.bytes());
    Ok(())
}

#[test]
fn test_034_04_invalid_grammar_reports_source_and_line() -> R {
    let valid = source("docs/spec/a.md", &requirement("A", ""));
    let text = String::from_utf8(valid.bytes.clone())?;
    for (from, to) in [
        ("then: 结果\n", ""),
        ("when: 操作", "whenever: 操作"),
        ("## REQ-A 标题", "## REQ-A"),
        ("AC-A-01", "AC-B-01"),
        ("AC-A-01", "AC-A-1"),
        ("check: check-A", "check: "),
        ("# 用户", "# Users"),
        ("given: 前提", " given: 前提"),
        ("# 决策\n无", "# 决策\n- 决定"),
        ("# 开放问题\n无", "# 开放问题\n- 问题\n阻塞：也许"),
        ("## REQ-A 标题", "## REQ-A 标题\nclass: optional"),
        (
            "## REQ-A 标题",
            "## REQ-A 标题\nclass: required\nclass: required",
        ),
        ("# 决策\n无", "# 决策\n无\n- 决定\nsource: input_id=i1"),
    ] {
        let bad = Source {
            path: valid.path.clone(),
            bytes: text.replace(from, to).into_bytes(),
        };
        let error = generate_index(&[bad]).expect_err(to);
        assert_eq!(error.path, valid.path);
        assert!(error.line >= 1 && !error.error.is_empty(), "{error}");
    }
    for files in [
        vec![],
        vec![valid.clone(), valid.clone()],
        vec![
            valid.clone(),
            source("docs/spec/b.md", &requirement("A", "")),
        ],
        vec![Source {
            path: valid.path.clone(),
            bytes: vec![0xff],
        }],
        vec![source("../a.md", &requirement("A", ""))],
        vec![source("docs/spec/nested/a.md", &requirement("A", ""))],
        vec![source(
            "docs/spec/a.md",
            &(requirement("A", "") + "### AC-A-01\ngiven: g\nwhen: w\nthen: t\ncheck: unit\n"),
        )],
    ] {
        assert!(
            generate_index(&files).is_err(),
            "invalid specification accepted: {files:?}"
        );
    }
    Ok(())
}

#[test]
fn test_034_04_decisions_questions_crlf_and_empty_sections() -> R {
    let text = format!("{PREFIX}{}", requirement("A", ""))
        .replace("# 决策\n无", "# 决策\n- 确认范围\nsource: question_id=q-1")
        .replace("# 开放问题\n无", "# 开放问题\n- 颜色\n阻塞：否");
    let a = Source {
        path: "docs/spec/a.md".into(),
        bytes: text.as_bytes().to_vec(),
    };
    let b = Source {
        bytes: text.replace('\n', "\r\n").trim_end().as_bytes().to_vec(),
        ..a.clone()
    };
    let ai = generate_index(&[a])?;
    let bi = generate_index(&[b])?;
    assert_eq!(ai.value()["requirements"], bi.value()["requirements"]);
    assert_ne!(
        ai.bytes(),
        bi.bytes(),
        "source byte digests cover line endings"
    );
    let empty = Source {
        path: "docs/spec/empty.md".into(),
        bytes: "# 用户\n# 范围\n# 非目标\n# 决策\n# 开放问题\n# 需求\n"
            .as_bytes()
            .to_vec(),
    };
    assert_eq!(generate_index(&[empty])?.value()["requirements"], json!([]));
    Ok(())
}
