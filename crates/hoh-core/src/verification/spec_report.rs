//! A specification review's report and native attempt record (REQ-031 behaviour 4, CR-026, docs/adr/ADR-035.md): built
//! by the host only from a COMPLETED TaskResult, both under their locked schemas; and the checks of their bytes that
//! need no database, every reason collected (none stops the others), which the store's single intake entry
//! (`persistence::spec_review`) completes with the sessions'. Pure.

use super::report::ObjectRef;
use serde_json::{Value, json};

/// What a review's report and record are bound to: its `spec_review.opened` and the attempt that takes it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub review_id: String,
    pub attempt_id: String,
    pub spec_revision: i64,
    pub spec_sha256: String,
    pub git_commit: String,
}

/// The reason codes of a REJECTED specification review report (`<code>: <detail>`).
pub mod codes {
    pub const REPORT_MISSING: &str = "report_missing";
    pub const REPORT_INVALID: &str = "report_invalid";
    pub const NATIVE_ATTEMPT_MISSING: &str = "native_attempt_missing";
    pub const NATIVE_ATTEMPT_INVALID: &str = "native_attempt_invalid";
    pub const RAW_SESSION_SHA256_MISMATCH: &str = "raw_session_sha256_mismatch";
    pub const NATIVE_ATTEMPT_SHA256_MISMATCH: &str = "native_attempt_sha256_mismatch";
    pub const SESSION_NOT_IN_NATIVE_ATTEMPT: &str = "session_not_in_native_attempt";
    pub const ROLE_MISMATCH: &str = "role_mismatch";
    pub const REVIEW_ID_MISMATCH: &str = "review_id_mismatch";
    pub const ATTEMPT_ID_MISMATCH: &str = "attempt_id_mismatch";
    pub const STDOUT_SHA256_MISMATCH: &str = "stdout_sha256_mismatch";
    pub const DUPLICATE_FINDING_ID: &str = "duplicate_finding_id";
    pub const ATTEMPT_NOT_OF_REVIEW: &str = "attempt_not_of_review";
    pub const SESSION_UNKNOWN: &str = "session_unknown";
    pub const SESSION_MISMATCH: &str = "session_mismatch";
    pub const SESSION_ROLE_MISMATCH: &str = "session_role_mismatch";
    pub const NOT_INDEPENDENT: &str = "not_independent";
}

/// The keys a report, its record and its `spec_review.opened` must agree on; a mismatch is `<key>_mismatch`.
const REVISION_KEYS: [&str; 3] = ["spec_revision", "spec_sha256", "git_commit"];

/// The Runtime's export of a review's native attempt (contracts/spec-review-attempt.schema.json): `session_ids` are its
/// HoH session and its native session(s), `stdout_sha256` the turn's raw stdout log's digest. Only from a COMPLETED
/// TaskResult (its status is COMPLETED).
pub fn native(
    binding: &Binding,
    session_ids: &[String],
    stdout_sha256: &str,
) -> Result<Value, String> {
    let record = json!({
        "record_type": "runtime_spec_review_export", "role": "reviewer", "status": "COMPLETED",
        "session_ids": session_ids, "review_id": binding.review_id, "attempt_id": binding.attempt_id,
        "spec_revision": binding.spec_revision, "spec_sha256": binding.spec_sha256, "git_commit": binding.git_commit,
        "stdout_sha256": stdout_sha256,
    });
    conforms(
        crate::schema::spec_review_attempt(),
        &record,
        "native attempt record",
    )?;
    Ok(record)
}

/// The report of `body` (a `report::block(final_text, "reviewer")`) by HoH session `session_id`, bound to `binding`'s
/// revision and naming its raw session and native attempt objects (contracts/spec-review-report.schema.json).
pub fn build(
    body: &Value,
    session_id: &str,
    binding: &Binding,
    raw_session: &ObjectRef,
    native_attempt: &ObjectRef,
) -> Result<Value, String> {
    let reference = |r: &ObjectRef| json!({"path": r.path, "sha256": r.sha256});
    let mut report = json!({
        "schema_version": "1.0", "role": "reviewer", "session_id": session_id,
        "spec_revision": binding.spec_revision, "spec_sha256": binding.spec_sha256, "git_commit": binding.git_commit,
        "status": body["status"], "scope": body["scope"], "findings": body["findings"],
        "raw_session": reference(raw_session), "native_attempt": reference(native_attempt),
    });
    // A block that names no findings has none, not null.
    if let Some(object) = report.as_object_mut() {
        object.retain(|_, v| !v.is_null());
    }
    conforms(
        crate::schema::spec_review_report(),
        &report,
        "specification review report",
    )?;
    Ok(report)
}

/// What [`check`] read and found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The report, when it is valid under its schema.
    pub report: Option<Value>,
    /// The native attempt record, when it is valid under its schema.
    pub native: Option<Value>,
    /// Every reason found, `<code>: <detail>`, in a fixed order.
    pub reasons: Vec<String>,
}

/// The checks of a review's objects that need no database: `report` and `native` are the bytes of its report and its
/// native attempt record (none when there is none), `raw_sha256` the digest recomputed from its raw session log's
/// bytes, `expected` what `spec_review.opened` and the attempt taking it in bind it to.
pub fn check(
    report: Option<&[u8]>,
    native: Option<&[u8]>,
    raw_sha256: &str,
    expected: &Binding,
) -> Checked {
    let mut reasons = Vec::new();
    let mut reason = |code: &str, detail: String| reasons.push(format!("{code}: {detail}"));
    let parsed = |bytes: &[u8], schema: &crate::schema::Schema, what: &str| {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("the {what} is not JSON: {e}"))?;
        conforms(schema, &value, what)?;
        Ok::<Value, String>(value)
    };
    let report_value = match report {
        None => {
            reason(
                codes::REPORT_MISSING,
                "the final assistant message has no valid ```hoh-report block".to_owned(),
            );
            None
        }
        Some(bytes) => parsed(
            bytes,
            crate::schema::spec_review_report(),
            "specification review report",
        )
        .map_err(|e| reason(codes::REPORT_INVALID, e))
        .ok(),
    };
    let native_value = match native {
        None => {
            reason(
                codes::NATIVE_ATTEMPT_MISSING,
                "no native attempt record was given with the report".to_owned(),
            );
            None
        }
        Some(bytes) => parsed(
            bytes,
            crate::schema::spec_review_attempt(),
            "native attempt record",
        )
        .map_err(|e| reason(codes::NATIVE_ATTEMPT_INVALID, e))
        .ok(),
    };
    if let Some(r) = &report_value {
        if r["raw_session"]["sha256"] != json!(raw_sha256) {
            reason(
                codes::RAW_SESSION_SHA256_MISMATCH,
                format!(
                    "the report's raw_session names {}, its raw session log's bytes hash to {raw_sha256}",
                    r["raw_session"]["sha256"]
                ),
            );
        }
        if let Some(bytes) = native {
            let digest = crate::context::sha256_hex(bytes);
            if r["native_attempt"]["sha256"] != json!(digest) {
                reason(
                    codes::NATIVE_ATTEMPT_SHA256_MISMATCH,
                    format!(
                        "the report's native_attempt names {}, the native attempt record's bytes hash to {digest}",
                        r["native_attempt"]["sha256"]
                    ),
                );
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        let twice: std::collections::BTreeSet<&str> = r["findings"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| f["id"].as_str())
            .filter(|id| !ids.insert(*id))
            .collect();
        if !twice.is_empty() {
            reason(
                codes::DUPLICATE_FINDING_ID,
                format!("the report names finding ids {twice:?} more than once"),
            );
        }
    }
    if let (Some(r), Some(n)) = (&report_value, &native_value) {
        let held = n["session_ids"]
            .as_array()
            .is_some_and(|ids| ids.contains(&r["session_id"]));
        if !held {
            reason(
                codes::SESSION_NOT_IN_NATIVE_ATTEMPT,
                format!(
                    "the native attempt record's session_ids {} do not hold the report's session {}",
                    n["session_ids"], r["session_id"]
                ),
            );
        }
        if n["role"] != r["role"] {
            reason(
                codes::ROLE_MISMATCH,
                format!(
                    "the native attempt record is a {}'s, the report a {}'s",
                    n["role"], r["role"]
                ),
            );
        }
    }
    if let Some(n) = &native_value {
        for (key, code, want) in [
            ("review_id", codes::REVIEW_ID_MISMATCH, &expected.review_id),
            (
                "attempt_id",
                codes::ATTEMPT_ID_MISMATCH,
                &expected.attempt_id,
            ),
        ] {
            if n[key] != json!(want) {
                reason(
                    code,
                    format!(
                        "the native attempt record names {key} {}, the review is taken in as {want}",
                        n[key]
                    ),
                );
            }
        }
        if n["stdout_sha256"] != json!(raw_sha256) {
            reason(
                codes::STDOUT_SHA256_MISMATCH,
                format!(
                    "the native attempt record's stdout_sha256 {} is not its raw session log's {raw_sha256}",
                    n["stdout_sha256"]
                ),
            );
        }
    }
    let opened = json!({"spec_revision": expected.spec_revision, "spec_sha256": expected.spec_sha256,
        "git_commit": expected.git_commit});
    for key in REVISION_KEYS {
        let differs = |v: &Option<Value>| v.as_ref().is_some_and(|v| v[key] != opened[key]);
        if differs(&report_value) || differs(&native_value) {
            let named = |v: &Option<Value>| v.as_ref().map_or(Value::Null, |v| v[key].clone());
            reason(
                &format!("{key}_mismatch"),
                format!(
                    "the report names {}, the native attempt record {}, spec_review.opened {}",
                    named(&report_value),
                    named(&native_value),
                    opened[key]
                ),
            );
        }
    }
    Checked {
        report: report_value,
        native: native_value,
        reasons,
    }
}

/// Whether `value` is valid under `schema`; why not, at its first violation.
fn conforms(schema: &crate::schema::Schema, value: &Value, what: &str) -> Result<(), String> {
    schema.validate(value).map_err(|violations| {
        let first = violations.first();
        format!(
            "the {what} is invalid at {}: {}",
            first.map_or("/", |v| v.instance_path.as_str()),
            first.map_or("", |v| v.message.as_str())
        )
    })
}

#[cfg(test)]
mod tests;
