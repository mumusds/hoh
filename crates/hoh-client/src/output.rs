//! What a client writes on stdout (REQ-004; docs/adr/ADR-031.md decision 10): every JSON line through one serializer,
//! and the one line of `--output text`.

use serde_json::Value;

/// `value` as one line of JSON, without its LF: serde_json's text with U+2028 and U+2029 written `\u2028` and
/// `\u2029` (REQ-004 behaviour 5). serde_json writes those two characters only inside strings, so the text is replaced
/// as it stands; any other character, a line feed inside a string included (`\n`), is serde_json's.
pub fn json_line<T: serde::Serialize + ?Sized>(value: &T) -> serde_json::Result<String> {
    Ok(serde_json::to_string(value)?
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

/// The one stdout line of `--output text` for run result `result` (without its LF): `<run_id> <state>`, then its
/// blocked reason when BLOCKED, or the requirements not accepted when INCOMPLETE.
pub fn text_line(result: &Value) -> String {
    let state = result["state"].as_str().unwrap_or_default();
    let mut line = format!("{} {state}", result["run_id"].as_str().unwrap_or_default());
    match state {
        "BLOCKED" => {
            if let Some(reason) = result["blocked_reason"].as_str() {
                line.push(' ');
                line.push_str(reason);
            }
        }
        "INCOMPLETE" => {
            let remaining: Vec<&str> = result["remaining_requirements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            line.push_str(" 剩余 ");
            line.push_str(&remaining.join(","));
        }
        _ => {}
    }
    line
}

#[cfg(test)]
mod tests;
