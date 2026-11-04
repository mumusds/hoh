//! The product schema validator agrees with Python jsonschema on the shared corpus
//! (tests/fixtures/schema/corpus.json; the bundle test confirms each recorded verdict with Python).

// No SQLite outside hoh-core::persistence (AC-062-05); see clippy.toml.
#![forbid(clippy::disallowed_methods)]

use hoh_core::schema;
use serde_json::Value;

fn corpus() -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/schema/corpus.json"
    ))?;
    let doc: Value = serde_json::from_str(&text)?;
    Ok(doc["cases"].as_array().cloned().unwrap_or_default())
}

#[test]
fn validator_matches_every_corpus_verdict() -> Result<(), Box<dyn std::error::Error>> {
    let cases = corpus()?;
    assert!(cases.len() >= 90, "corpus too small: {}", cases.len());
    let mut wrong = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let doc = match case["schema"].as_str() {
            Some("protocol") => schema::protocol(),
            Some("compat-manifest") => schema::compat_manifest(),
            other => return Err(format!("case {i}: unknown schema {other:?}").into()),
        };
        let def = case["def"].as_str().ok_or("def")?;
        let verdict = if def == "root" {
            doc.validate(&case["instance"])
        } else {
            doc.validate_def(def, &case["instance"])?
        };
        if verdict.is_ok() != case["valid"].as_bool().ok_or("valid")? {
            wrong.push(format!(
                "case {i} ({}): expected valid={}, got {:?}",
                case["note"], case["valid"], verdict
            ));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}

#[test]
fn violations_name_the_instance_location() -> Result<(), Box<dyn std::error::Error>> {
    let cases = corpus()?;
    let case = cases
        .iter()
        .find(|c| c["note"] == "minimum: role_epoch")
        .ok_or("case")?;
    let errors = schema::protocol()
        .validate_def("Message", &case["instance"])?
        .err()
        .ok_or("expected violations")?;
    assert!(
        errors.iter().any(|v| v.instance_path == "/role_epoch"),
        "{errors:?}"
    );
    Ok(())
}

#[test]
fn keyword_semantics_match_the_keyword_corpus() -> Result<(), Box<dyn std::error::Error>> {
    // M1T01R-04/05/09/10: exact numbers, RFC 3339 date-times, ECMA-262 `$`, integral-float keyword values.
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/schema/keywords.json"
    ))?;
    let doc: Value = serde_json::from_str(&text)?;
    let cases = doc["cases"].as_array().ok_or("cases")?;
    assert!(cases.len() >= 35);
    let mut wrong = Vec::new();
    for case in cases {
        let schema = schema::Schema::new(case["schema"].clone())?;
        let valid = schema.validate(&case["instance"]).is_ok();
        if valid != case["valid"].as_bool().ok_or("valid")? {
            wrong.push(format!("{}: got valid={valid}", case["note"]));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}
