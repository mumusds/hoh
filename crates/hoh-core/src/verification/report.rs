//! A verifier's independent report (docs/adr/ADR-028.md decision 15, AC-054-03): its ```hoh-report block read from its
//! final assistant message by the same rule as tools/agent_task.py's `--report-out` (the last block; a JSON object of
//! role, status, scope and findings only), completed by the Runtime with what the agent can neither know nor forge
//! (its HoH session, the candidate, the specification, its raw session and native attempt objects) and checked against
//! contracts/independent-report.schema.json. Pure.

use serde_json::{Value, json};

/// A path and digest a report names (`raw_session`, `native_attempt`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
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
    conforms(&report)?;
    Ok(report)
}

/// Whether `report` is valid under contracts/independent-report.schema.json; why not, at its first violation.
pub fn conforms(report: &Value) -> Result<(), String> {
    crate::schema::independent_report()
        .validate(report)
        .map_err(|violations| {
            let first = violations.first();
            format!(
                "the independent report is invalid at {}: {}",
                first.map_or("/", |v| v.instance_path.as_str()),
                first.map_or("", |v| v.message.as_str())
            )
        })
}

/// What the host's check of a report read from its objects (docs/adr/ADR-029.md decision 7): the report, and its
/// native attempt's session ids and status, which the store judges (it reads no object).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    pub report: Value,
    pub session_ids: Vec<String>,
    pub status: String,
}

/// The host's check of a report before its commit (docs/adr/ADR-029.md decision 7): `report` and `native` are the bytes
/// of its report and native attempt objects, `raw_sha256` the digest recomputed from its raw session object's bytes.
/// The report is valid under the locked schema; its raw_session and native_attempt references name those digests; the
/// native attempt is valid under its schema, its session_ids hold the report's session, its candidate and role are the
/// report's and its stdout is the raw session; the report's finding ids are unique. Why not, at the first that fails:
/// an output to correct. Whether the native sessions are another role's needs the store's sessions (its side). Pure.
pub fn verify(report: &[u8], raw_sha256: &str, native: &[u8]) -> Result<Checked, String> {
    let parsed: Value = serde_json::from_slice(report)
        .map_err(|e| format!("the independent report is not JSON: {e}"))?;
    conforms(&parsed)?;
    if parsed["raw_session"]["sha256"] != json!(raw_sha256) {
        return Err(format!(
            "the report's raw_session names {}, its raw session object's bytes hash to {raw_sha256}",
            parsed["raw_session"]["sha256"]
        ));
    }
    let native_sha256 = crate::context::sha256_hex(native);
    if parsed["native_attempt"]["sha256"] != json!(native_sha256) {
        return Err(format!(
            "the report's native_attempt names {}, its native attempt object's bytes hash to {native_sha256}",
            parsed["native_attempt"]["sha256"]
        ));
    }
    let export: Value = serde_json::from_slice(native)
        .map_err(|e| format!("the native attempt export is not JSON: {e}"))?;
    native_conforms(&export)?;
    let session_ids: Vec<String> = export["session_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect();
    let session = parsed["session_id"].as_str().unwrap_or_default();
    if !session_ids.iter().any(|s| s == session) {
        return Err(format!(
            "the native attempt's session_ids {session_ids:?} do not hold the report's session {session}"
        ));
    }
    for key in ["candidate_id", "role"] {
        if export[key] != parsed[key] {
            return Err(format!(
                "the native attempt's {key} {} is not the report's {}",
                export[key], parsed[key]
            ));
        }
    }
    if export["stdout_sha256"] != json!(raw_sha256) {
        return Err(format!(
            "the native attempt's stdout_sha256 {} is not its raw session's {raw_sha256}",
            export["stdout_sha256"]
        ));
    }
    let mut ids = std::collections::BTreeSet::new();
    for finding in parsed["findings"].as_array().into_iter().flatten() {
        let id = finding["id"].as_str().unwrap_or_default();
        if !ids.insert(id) {
            return Err(format!(
                "the report names finding id {id} more than once: each finding needs an id of its own"
            ));
        }
    }
    Ok(Checked {
        status: export["status"].as_str().unwrap_or_default().to_owned(),
        session_ids,
        report: parsed,
    })
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
    native_conforms(&export)?;
    Ok(export)
}

/// Whether `export` is valid under contracts/native-attempt.schema.json; why not, at its first violation.
fn native_conforms(export: &Value) -> Result<(), String> {
    crate::schema::native_attempt()
        .validate(export)
        .map_err(|violations| {
            format!(
                "the native attempt export is invalid: {}",
                violations.first().map_or("", |v| v.message.as_str())
            )
        })
}

/// A finding's Issue traced as the store registered it (docs/adr/ADR-029.md decision 15, AC-054-04): the Issue, its
/// report's row with the report object it names, and the raw session and native attempt objects registered with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct IssueChain {
    pub issue: ChainIssue,
    pub report: ChainReport,
    /// The report row's raw session artifact.
    pub raw: ObjectRef,
    /// The native attempt artifact its settle registered (its `verification.report`); none when it was taken in
    /// without one (a report recorded on its own).
    pub native: Option<ObjectRef>,
}

/// The Issue of an [`IssueChain`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChainIssue {
    pub issue_id: String,
    pub candidate_id: Option<String>,
    pub severity: String,
    pub fingerprint: String,
    pub finding_id: String,
}

/// The report row of an [`IssueChain`]: its id, state and report object.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ChainReport {
    pub report_id: String,
    pub state: String,
    pub path: String,
    pub sha256: String,
}

/// What [`verify_chain`] found, each true when it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ChainCheck {
    /// The report object's bytes hash to its name.
    pub report_digest: bool,
    /// The report names the Issue's finding.
    pub finding_present: bool,
    /// The report's raw_session is the registered raw artifact, whose bytes hash to its digest.
    pub raw_digest: bool,
    /// The report's native_attempt is the registered native artifact, whose bytes hash to its digest.
    pub native_digest: bool,
    /// The native attempt's session_ids hold the report's session.
    pub session_in_native: bool,
    /// The native attempt's stdout_sha256 is the raw session's digest.
    pub stdout_is_raw: bool,
}

impl ChainCheck {
    /// Every link holds.
    pub fn holds(&self) -> bool {
        self.report_digest
            && self.finding_present
            && self.raw_digest
            && self.native_digest
            && self.session_in_native
            && self.stdout_is_raw
    }
}

/// Check `chain` against the objects of the project at `project` (docs/adr/ADR-029.md decision 15): the report object
/// read by its name (`objects::get` recomputes its digest), its finding, its raw_session and native_attempt references
/// equal to the registered artifacts and their bytes recomputed, the native attempt's session_ids holding the report's
/// session and its stdout_sha256 the raw session's digest. Reads objects; writes nothing.
pub fn verify_chain(project: &std::path::Path, chain: &IssueChain) -> ChainCheck {
    let mut check = ChainCheck::default();
    let report_at = ObjectRef {
        path: chain.report.path.clone(),
        sha256: chain.report.sha256.clone(),
    };
    let Some(report) =
        bytes_of(project, &report_at).and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return check;
    };
    check.report_digest = true;
    check.finding_present = report["findings"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|f| f["id"] == json!(chain.issue.finding_id));
    let named = |r: &ObjectRef| json!({"path": r.path, "sha256": r.sha256});
    check.raw_digest =
        report["raw_session"] == named(&chain.raw) && bytes_of(project, &chain.raw).is_some();
    let native = chain
        .native
        .as_ref()
        .filter(|n| report["native_attempt"] == named(n))
        .and_then(|n| bytes_of(project, n))
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    if let Some(native) = native {
        check.native_digest = true;
        check.session_in_native = report["session_id"].is_string()
            && native["session_ids"]
                .as_array()
                .is_some_and(|ids| ids.contains(&report["session_id"]));
        check.stdout_is_raw =
            check.raw_digest && native["stdout_sha256"] == json!(chain.raw.sha256);
    }
    check
}

/// The bytes of the object `at` names, if they hash to its digest: read by its name when it is kept as an object
/// (`objects::get` recomputes it), else from its path under the project (a raw log kept in place).
fn bytes_of(project: &std::path::Path, at: &ObjectRef) -> Option<Vec<u8>> {
    let bytes = if at.path == crate::objects::relative_path(&at.sha256) {
        crate::objects::get(project, &at.sha256).ok()?
    } else {
        crate::objects::read_regular(&project.join(&at.path)).ok()?
    };
    (crate::context::sha256_hex(&bytes) == at.sha256).then_some(bytes)
}

#[cfg(test)]
mod tests;
