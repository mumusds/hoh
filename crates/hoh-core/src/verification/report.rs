//! A verifier's independent report (docs/adr/ADR-028.md decision 15, AC-054-03): its ```hoh-report block read from its
//! final assistant message by the same rule as tools/agent_task.py's `--report-out` (the last block; a JSON object of
//! role, status, scope and findings only), completed by the Runtime with what the agent can neither know nor forge
//! (its HoH session, the candidate, the specification, its raw session and native attempt objects) and checked against
//! contracts/independent-report.schema.json. Pure.

use serde_json::{Value, json};

/// A path and digest a report names (`raw_session`, `native_attempt`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRef {
    pub path: String,
    pub sha256: String,
}

/// The report block of `final_text`: the last ```hoh-report block, a JSON object with only role, status, scope and
/// findings, its role (when given) `role`.
pub fn block(final_text: &str, role: &str) -> Result<Value, String> {
    static FENCE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let fence = FENCE.get_or_init(|| {
        regex::Regex::new(r"(?s)```hoh-report[ \t]*\n(.*?)\n```").unwrap_or_else(|_| unreachable!())
    });
    let last = fence
        .captures_iter(final_text)
        .last()
        .and_then(|c| c.get(1))
        .ok_or("the final assistant message has no ```hoh-report block")?;
    let body: Value = serde_json::from_str(last.as_str())
        .map_err(|e| format!("the hoh-report block is not JSON: {e}"))?;
    let object = body
        .as_object()
        .ok_or("the hoh-report block must be a JSON object")?;
    let extra: Vec<&str> = object
        .keys()
        .map(String::as_str)
        .filter(|k| !["role", "status", "scope", "findings"].contains(k))
        .collect();
    if !extra.is_empty() {
        return Err(format!(
            "the hoh-report block may hold only role, status, scope and findings; got {}",
            extra.join(", ")
        ));
    }
    if let Some(given) = object.get("role").filter(|r| **r != json!(role)) {
        return Err(format!(
            "the hoh-report role {given} is not the attempt's {role}"
        ));
    }
    Ok(body)
}

/// The report of `body` (a [`block`]) by `role` in HoH session `session_id`, on `candidate_id` under `spec_sha256`,
/// naming its raw session and native attempt objects; valid under the locked schema.
pub fn build(
    body: &Value,
    role: &str,
    session_id: &str,
    (candidate_id, spec_sha256): (&str, &str),
    raw_session: &ObjectRef,
    native_attempt: &ObjectRef,
) -> Result<Value, String> {
    let reference = |r: &ObjectRef| json!({"path": r.path, "sha256": r.sha256});
    let report = drop_nulls(json!({
        "schema_version": "1.1", "role": role, "session_id": session_id, "candidate_id": candidate_id,
        "spec_sha256": spec_sha256, "status": body["status"], "scope": body["scope"], "findings": body["findings"],
        "raw_session": reference(raw_session), "native_attempt": reference(native_attempt),
    }));
    crate::schema::independent_report()
        .validate(&report)
        .map_err(|violations| {
            let first = violations.first();
            format!(
                "the independent report is invalid at {}: {}",
                first.map_or("/", |v| v.instance_path.as_str()),
                first.map_or("", |v| v.message.as_str())
            )
        })?;
    Ok(report)
}

/// `value` without its top-level null members (a block that names no findings has none, not null).
fn drop_nulls(mut value: Value) -> Value {
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, v| !v.is_null());
    }
    value
}

/// The Runtime's export of a verifier's native attempt (contracts/native-attempt.schema.json).
pub fn native_attempt(
    role: &str,
    status: &str,
    session_ids: &[String],
    candidate_id: &str,
    stdout_sha256: &str,
) -> Result<Value, String> {
    let export = json!({
        "record_type": "runtime_attempt_export", "role": role, "status": status, "session_ids": session_ids,
        "candidate_id": candidate_id, "stdout_sha256": stdout_sha256,
    });
    crate::schema::native_attempt()
        .validate(&export)
        .map_err(|violations| {
            format!(
                "the native attempt export is invalid: {}",
                violations.first().map_or("", |v| v.message.as_str())
            )
        })?;
    Ok(export)
}

#[cfg(test)]
mod tests;
