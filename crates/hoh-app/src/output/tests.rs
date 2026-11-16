//! The client's stdout lines (docs/adr/ADR-031.md decision 10, test plan "客户端").

use super::*;
use serde_json::json;

#[test]
fn a_json_line_escapes_the_line_and_paragraph_separators_and_nothing_else()
-> Result<(), serde_json::Error> {
    let text = "中文 👩‍👩‍👧 a\u{2028}b\u{2029}c\nd \"q\" \\ \u{1b}";
    let line = json_line(&json!({"statement": text}))?;
    assert!(
        !line.contains('\u{2028}') && !line.contains('\u{2029}'),
        "{line}"
    );
    assert!(
        !line
            .as_bytes()
            .windows(3)
            .any(|w| w == [0xE2, 0x80, 0xA8] || w == [0xE2, 0x80, 0xA9])
    );
    assert!(
        line.contains("\\u2028") && line.contains("\\u2029"),
        "{line}"
    );
    assert!(!line.contains('\n') && !line.contains('\u{1b}'), "{line}");
    // Everything else is serde_json's own text: the same once the two escapes are undone.
    let plain = serde_json::to_string(&json!({"statement": text}))?;
    assert_eq!(
        line.replace("\\u2028", "\u{2028}")
            .replace("\\u2029", "\u{2029}"),
        plain
    );
    assert!(line.contains("中文 👩‍👩‍👧"), "{line}");
    let back: Value = serde_json::from_str(&line)?;
    assert_eq!(back["statement"], text);
    // A typed value goes through the same function.
    #[derive(serde::Serialize)]
    struct Typed<'a> {
        s: &'a str,
    }
    assert_eq!(
        json_line(&Typed { s: "x\u{2029}" })?,
        "{\"s\":\"x\\u2029\"}"
    );
    Ok(())
}

#[test]
fn the_text_line_is_the_run_its_state_and_why_it_stopped() {
    let base = json!({"run_id": "R1", "blocked_reason": null, "remaining_requirements": []});
    let with = |state: &str, more: Value| {
        let mut result = base.clone();
        result["state"] = json!(state);
        if let (Some(all), Some(more)) = (result.as_object_mut(), more.as_object()) {
            all.extend(more.clone());
        }
        text_line(&result)
    };
    assert_eq!(with("COMPLETED", json!({})), "R1 COMPLETED");
    assert_eq!(with("PAUSED", json!({})), "R1 PAUSED");
    assert_eq!(
        with("BLOCKED", json!({"blocked_reason": "NEED_DECISION"})),
        "R1 BLOCKED NEED_DECISION"
    );
    assert_eq!(
        with(
            "INCOMPLETE",
            json!({"remaining_requirements": ["REQ-001", "REQ-004"]})
        ),
        "R1 INCOMPLETE 剩余 REQ-001,REQ-004"
    );
    // Nothing remaining is said as nothing (an INCOMPLETE run whose every requirement was accepted).
    assert_eq!(with("INCOMPLETE", json!({})), "R1 INCOMPLETE 剩余 ");
}
