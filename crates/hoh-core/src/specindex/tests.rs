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
    // M3B-09: pinned bytes. A serde_json encoding of the same value would
    // follow a map-order change and hide it.
    const CANONICAL: &str = r#"{
  "files": [
    {
      "path": "docs/spec/a.md",
      "sha256": "e3352f139a1305b88a3bdcbe3b9e9e9d4ce53e929d7e8888cf011f1e9c9ed1b9"
    },
    {
      "path": "docs/spec/b.md",
      "sha256": "4d1e8ed3f5f7839ef3d3155045a1ac5edf32a9d6b9b3174d29fde77e022b9c0a"
    }
  ],
  "generated": "生成，勿改",
  "requirements": [
    {
      "acceptance": [
        {
          "given": "前提",
          "id": "AC-A-01",
          "test_id": "TEST-A-01",
          "then": "结果",
          "when": "操作"
        }
      ],
      "id": "REQ-A",
      "release_class": "required",
      "release_targets": [
        "project"
      ],
      "title": "标题"
    },
    {
      "acceptance": [
        {
          "given": "前提",
          "id": "AC-B-01",
          "test_id": "TEST-B-01",
          "then": "结果",
          "when": "操作"
        }
      ],
      "id": "REQ-B",
      "release_class": "degradable",
      "release_targets": [
        "project"
      ],
      "title": "标题"
    }
  ],
  "scenarios": [
    {
      "acceptance_id": "AC-A-01",
      "check_id": "check-A",
      "id": "TEST-A-01",
      "requirement_id": "REQ-A"
    },
    {
      "acceptance_id": "AC-B-01",
      "check_id": "check-B",
      "id": "TEST-B-01",
      "requirement_id": "REQ-B"
    }
  ]
}
"#;
    assert_eq!(index.bytes(), CANONICAL.as_bytes(), "canonical index bytes");
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
        ("# 开放问题\n无", "# 开放问题\n- Q-1 问题\n阻塞：也许"),
        ("# 开放问题\n无", "# 开放问题\n- 问题\n阻塞：否"),
        ("# 开放问题\n无", "# 开放问题\n- Q-1\n阻塞：否"),
        ("# 开放问题\n无", "# 开放问题\n- Q-a.b 问题\n阻塞：否"),
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
        .replace("# 开放问题\n无", "# 开放问题\n- Q-color 颜色\n阻塞：否");
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

/// M3B-08: strip one CR only when it is the CR of a CRLF. Any other CR stays in
/// the line and is rejected, including a file that ends in a bare CR.
#[test]
fn test_034_04_bare_cr_in_a_value_is_rejected() -> R {
    let valid = source("docs/spec/a.md", &requirement("A", ""));
    let text = String::from_utf8(valid.bytes.clone())?;
    let embedded = Source {
        path: valid.path.clone(),
        bytes: text.replace("given: 前提", "given: 前\r提").into_bytes(),
    };
    let error =
        generate_index(std::slice::from_ref(&embedded)).expect_err("bare CR inside a value");
    assert!(
        error.error.contains("a value must be a single line"),
        "{error}"
    );
    assert_eq!(error.path, valid.path);
    assert_eq!(error.line, 14, "{error}");

    let trailing = Source {
        path: valid.path.clone(),
        bytes: format!("{}\r", text.trim_end_matches('\n')).into_bytes(),
    };
    let error = generate_index(std::slice::from_ref(&trailing)).expect_err("trailing bare CR");
    assert!(
        error.error.contains("a value must be a single line"),
        "{error}"
    );
    assert_eq!(error.line, 17, "{error}");

    let doubled = Source {
        path: valid.path.clone(),
        bytes: text
            .replace("given: 前提\n", "given: 前提\r\r\n")
            .into_bytes(),
    };
    let error = generate_index(std::slice::from_ref(&doubled)).expect_err("CR CR LF");
    assert!(
        error.error.contains("a value must be a single line"),
        "{error}"
    );
    assert_eq!(error.line, 14, "{error}");

    let crlf = Source {
        path: valid.path.clone(),
        bytes: text.replace('\n', "\r\n").into_bytes(),
    };
    let index = generate_index(&[crlf])?;
    assert_eq!(
        index.value()["requirements"][0]["acceptance"][0]["given"],
        "前提"
    );
    Ok(())
}

/// Q1 (docs/adr/ADR-035.md decision 1): open questions carry ids from the REQ/AC id set and their blocking mark;
/// decisions carry their source; both are exposed with their source positions and never enter index.json.
#[test]
fn test_031_03_questions_and_decisions_are_exposed_outside_the_index() -> R {
    let text = format!("{PREFIX}{}", requirement("A", ""))
        .replace(
            "# 决策\n无",
            "# 决策\n- 确认范围\nsource: question_id=q-1\n- 默认语言\nsource: event_id=e/2",
        )
        .replace(
            "# 开放问题\n无",
            "# 开放问题\n- Q-color 颜色\n阻塞：否\n\n- Q-db 数据库\n阻塞：是",
        );
    let a = Source {
        path: "docs/spec/a.md".into(),
        bytes: text.as_bytes().to_vec(),
    };
    let index = generate_index(std::slice::from_ref(&a))?;
    assert_eq!(
        index.questions(),
        &[
            Question {
                id: "Q-color".into(),
                text: "颜色".into(),
                blocking: false,
                path: a.path.clone(),
                line: 13
            },
            Question {
                id: "Q-db".into(),
                text: "数据库".into(),
                blocking: true,
                path: a.path.clone(),
                line: 16
            },
        ]
    );
    assert_eq!(
        index.decisions(),
        &[
            Decision {
                text: "确认范围".into(),
                source_kind: "question_id".into(),
                source_id: "q-1".into(),
                path: a.path.clone(),
                line: 8
            },
            Decision {
                text: "默认语言".into(),
                source_kind: "event_id".into(),
                source_id: "e/2".into(),
                path: a.path.clone(),
                line: 10
            },
        ]
    );
    let plain = generate_index(&[source("docs/spec/a.md", &requirement("A", ""))])?;
    assert!(plain.questions().is_empty() && plain.decisions().is_empty());
    let keys: Vec<&String> = index.value().as_object().ok_or("object")?.keys().collect();
    assert_eq!(keys, ["files", "generated", "requirements", "scenarios"]);
    // A question id is in the one id set of REQs and ACs: unique within and across files.
    let clash = text.replace("Q-db", "Q-color");
    let error = generate_index(&[Source {
        path: a.path.clone(),
        bytes: clash.into_bytes(),
    }])
    .expect_err("an id used twice");
    assert!(error.error.contains("duplicate id"), "{error}");
    let b = Source {
        path: "docs/spec/b.md".into(),
        bytes: format!("{PREFIX}{}", requirement("B", ""))
            .replace("# 开放问题\n无", "# 开放问题\n- Q-color 又一个\n阻塞：是")
            .into_bytes(),
    };
    assert!(
        generate_index(&[a, b]).is_err(),
        "a question id used in two files"
    );
    Ok(())
}
