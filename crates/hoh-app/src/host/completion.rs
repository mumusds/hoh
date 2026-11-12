//! A writer attempt's completion (docs/adr/ADR-028.md decisions 3 and 4): its bridge read against its identity, its
//! workspace prepared for a freeze while the write lease is still held, and the store's one commit
//! ([`Store::complete_writer_attempt`]). The runner's own end and a reconcile of an attempt whose work was observed
//! done take the same path; neither settles a writer's success the generic way (which would leave its task
//! IMPLEMENTED).

use crate::verify::freeze::{self, FreezeError, Inputs};
use hoh_core::bridge::{self, Identity};
use hoh_core::persistence::{
    ArtifactRow, Ending, Observed, PersistError, SelfTests, Store, UnfinishedRow,
    VerifierCompletion, VerifierOutcome, WriterCompleted, WriterCompletion, WriterOutcome,
};
use hoh_core::protocol::TaskStatus;
use hoh_core::retry::FailureClass;
use serde_json::json;
use std::path::Path;

/// The freeze's inputs by the project layout's convention until the specification's own location exists (decision 4;
/// M3-T02 replaces them, CR-018): relative to the checkout.
pub(crate) fn inputs() -> Inputs {
    Inputs {
        spec: "contracts/requirements.json".into(),
        environment: ".hoh/environment.json".into(),
        test_plan: "tests/check-plan.json".into(),
    }
}

/// What a successful writer attempt's bridge decides (decision 3's table), its refused lines added to `ending`'s events
/// as `bridge.rejected` (decision 2). A COMPLETED result has the project's workspace prepared for its freeze: the
/// environment description first (decision 5). Blocking (git, files): run it off the async threads.
pub(super) fn outcome(project: &Path, identity: &Identity, ending: &mut Ending) -> WriterOutcome {
    let result = match result_of(identity, ending) {
        Ok(result) => result,
        Err(reason) => {
            return WriterOutcome::Correct {
                reasons: vec![reason],
            };
        }
    };
    let decide = |reason: &str| WriterOutcome::Decide {
        reason: reason.to_owned(),
        detail:
            json!({"status": result.status, "summary": result.summary, "issues": result.issues})
                .to_string(),
    };
    match result.status {
        TaskStatus::Failed => return decide("agent_failed"),
        TaskStatus::Blocked => return decide("agent_blocked"),
        TaskStatus::Incomplete => return decide("agent_incomplete"),
        TaskStatus::Completed => {}
    }
    let self_tests = self_tests(project, &identity.attempt_id, &result.evidence, ending);
    let prepared = crate::verify::environment::ensure(project)
        .map_err(FreezeError::Io)
        .and_then(|_| {
            freeze::prepare_job(project, &project.join(".hoh"), &identity.run_id, &inputs())
        });
    match prepared {
        Ok(job) => WriterOutcome::Freeze {
            job: Box::new(job),
            result: Box::new(result),
            self_tests,
        },
        Err(FreezeError::Refused(reasons)) => WriterOutcome::Correct {
            reasons: reasons
                .into_iter()
                .map(|r| format!("its freeze was refused: {r}"))
                .collect(),
        },
        // The host could not read or write what the freeze needs (git, the object store): not the agent's to correct,
        // so no correction is counted; the task waits for a decision (decision 3 lists only the agent's outputs).
        Err(e) => WriterOutcome::Decide {
            reason: "freeze_failed".to_owned(),
            detail: e.to_string(),
        },
    }
}

/// Where attempt `attempt_id`'s self-test records go, relative to the project (docs/adr/ADR-028.md decision 14).
pub(crate) fn self_tests_dir(attempt_id: &str) -> String {
    format!(".hoh/self-tests/{}/", super::product::component(attempt_id))
}

/// The self-test records `evidence` names (docs/adr/ADR-028.md decision 14): one that is not a project-relative path
/// under the attempt's own directory, or reaches it through a link, is dropped with a `bridge.rejected` in `ending`'s
/// events (decision 2, -157); a regular file, read without blocking, whose bytes match its digest is kept as an object
/// (kind `self_test`); any other is only referenced. Blocking.
fn self_tests(
    project: &Path,
    attempt_id: &str,
    evidence: &[hoh_core::protocol::ArtifactRef],
    ending: &mut Ending,
) -> SelfTests {
    let dir = self_tests_dir(attempt_id);
    let mut kept = SelfTests::default();
    for (n, item) in evidence.iter().enumerate() {
        if let Err(reason) = admissible(project, &dir, &item.path) {
            ending.observed.events.push((
                format!("bridge/rejected/evidence/{n}"),
                "bridge.rejected".to_owned(),
                json!({"attempt_id": attempt_id, "evidence": item.path, "sha256": item.sha256,
                    "reason": reason}),
            ));
            continue;
        }
        let only_referenced =
            |error: String| json!({"name": item.path, "sha256": item.sha256, "error": error});
        let bytes = match hoh_core::objects::read_regular(&project.join(&item.path)) {
            Ok(bytes) => bytes,
            Err(e) => {
                kept.failed.push(only_referenced(e));
                continue;
            }
        };
        let digest: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if digest != item.sha256 {
            kept.failed.push(only_referenced(format!(
                "its bytes hash to {digest}, not the digest it was named with"
            )));
            continue;
        }
        match hoh_core::objects::put(project, &bytes) {
            Ok(sha256) => kept.items.push(ArtifactRow {
                path: hoh_core::objects::relative_path(&sha256),
                sha256,
                bytes: bytes.len() as u64,
                mime: if std::str::from_utf8(&bytes).is_ok() {
                    "text/plain; charset=utf-8"
                } else {
                    "application/octet-stream"
                }
                .to_owned(),
                kind: "self_test".to_owned(),
                name: item.path.clone(),
            }),
            Err(e) => kept.failed.push(only_referenced(e)),
        }
    }
    kept
}

/// Why `path` is not a self-test record of the directory `dir` (project-relative, ending in `/`), if it is not: an
/// absolute path, a `.` or `..` part, a path elsewhere, or a link on the way from the project's root.
fn admissible(project: &Path, dir: &str, path: &str) -> Result<(), String> {
    use std::path::Component;
    let relative = Path::new(path);
    if relative.is_absolute() {
        return Err("an absolute path".to_owned());
    }
    if relative
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err("a path with a . or .. part".to_owned());
    }
    if !path.starts_with(dir) || path.len() == dir.len() {
        return Err(format!("not a file under {dir}"));
    }
    let mut at = project.to_path_buf();
    for part in relative.components() {
        at.push(part);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a link", at.display()));
        }
    }
    Ok(())
}

/// The TaskResult a work attempt's bridge gives (docs/adr/ADR-028.md decision 2), its refused lines added to `ending`'s
/// events as `bridge.rejected`; or why its output is to be corrected (cut at its limit, missing, or its last TaskResult
/// refused).
fn result_of(
    identity: &Identity,
    ending: &mut Ending,
) -> Result<hoh_core::protocol::TaskResult, String> {
    let parsed = bridge::parse(&ending.observed.messages, identity);
    for refused in &parsed.refused {
        ending.observed.events.push((
            format!("bridge/rejected/{}", refused.line),
            "bridge.rejected".to_owned(),
            json!({"attempt_id": identity.attempt_id, "line": refused.line, "line_sha256": refused.sha256,
                "reason": refused.reason}),
        ));
    }
    // Cut at its limit, its last TaskResult may be lost: not trusted (M2T16D-456).
    if ending.observed.messages_truncated {
        return Err(format!(
            "its bridge was read only up to its limit ({} lines, {} bytes): write one TaskResult as its last line",
            super::product::BRIDGE_LINES,
            super::product::BRIDGE_BYTES
        ));
    }
    parsed.task_result.ok_or_else(|| {
        if parsed.result_invalid {
            let why = parsed
                .refused
                .last()
                .map_or(String::new(), |r| r.reason.clone());
            format!("its last TaskResult was refused: {why}")
        } else {
            "it wrote no TaskResult to its bridge".to_owned()
        }
    })
}

/// What a verifier's report is made from beyond its turn (decision 15).
pub(super) struct Reporting {
    pub project: std::path::PathBuf,
    pub role: String,
    /// Its HoH session (`independent_reports.session_id` references `sessions`, M2T16D-360).
    pub session_id: String,
    pub candidate_id: String,
    pub spec_sha256: String,
}

/// The report of a verifier's turn (decision 15): the ```hoh-report block of its final assistant message, completed
/// with its HoH session, the candidate, the spec, its raw session log (kept as an object at its end) and the native
/// attempt export the Runtime writes; both kept as objects. Why not, when it cannot be made. Blocking.
fn report_of(
    r: &Reporting,
    ending: &Ending,
) -> Result<hoh_core::persistence::VerifierReport, String> {
    use hoh_core::verification::report;
    let text = ending
        .observed
        .final_text
        .as_deref()
        .ok_or("its final assistant message is missing")?;
    let body = report::block(text, &r.role)?;
    let raw = ending
        .observed
        .artifacts
        .iter()
        .find(|a| a.kind.starts_with("raw_log") && a.name.starts_with("stdout"))
        .ok_or("its raw session log was not kept")?;
    let mut sessions = vec![r.session_id.clone()];
    sessions.extend(ending.observed.native_session.clone());
    let keep = |value: &serde_json::Value,
                kind: &str,
                name: &str|
     -> Result<hoh_core::persistence::ArtifactRow, String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        let sha256 = hoh_core::objects::put(&r.project, &bytes)?;
        Ok(hoh_core::persistence::ArtifactRow {
            path: hoh_core::objects::relative_path(&sha256),
            sha256,
            bytes: bytes.len() as u64,
            mime: "application/json".to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
        })
    };
    let native = keep(
        &report::native_attempt(&r.role, "SETTLED", &sessions, &r.candidate_id, &raw.sha256)?,
        "native_attempt",
        "native-attempt.json",
    )?;
    let reference = |a: &hoh_core::persistence::ArtifactRow| report::ObjectRef {
        path: a.path.clone(),
        sha256: a.sha256.clone(),
    };
    let built = report::build(
        &body,
        &r.role,
        &r.session_id,
        (&r.candidate_id, &r.spec_sha256),
        &reference(raw),
        &reference(&native),
    )?;
    Ok(hoh_core::persistence::VerifierReport {
        role: r.role.clone(),
        status: built["status"].as_str().unwrap_or_default().to_owned(),
        report: keep(&built, "independent_report", "independent-report.json")?,
        native_attempt: native,
    })
}

/// What a successful verifier attempt's bridge decides (decision 11): COMPLETED with a valid report settles it; a
/// legal FAILED, BLOCKED or INCOMPLETE verifies again within its round; anything else, a missing or invalid report
/// included, is corrected (decision 15). Blocking (its report's objects).
pub(super) fn verifier_outcome(
    identity: &Identity,
    ending: &mut Ending,
    reporting: Option<&Reporting>,
) -> VerifierOutcome {
    match result_of(identity, ending) {
        Ok(result) if result.status == TaskStatus::Completed => {
            match reporting
                .ok_or_else(|| "its report could not be made here".to_owned())
                .and_then(|r| report_of(r, ending))
            {
                Ok(report) => VerifierOutcome::Settle {
                    report: Box::new(report),
                },
                Err(why) => VerifierOutcome::Correct {
                    reasons: vec![format!("its report: {why}")],
                },
            }
        }
        Ok(result) => VerifierOutcome::Reverify {
            status: serde_json::to_value(result.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            reason: result.summary,
        },
        Err(reason) => VerifierOutcome::Correct {
            reasons: vec![reason],
        },
    }
}

/// Complete verifier attempt `identity.attempt_id`, which ended successfully with its processes proven gone and its
/// run directory looked at: its result decided from its bridge, then one commit (decision 11).
pub(super) async fn complete_verifier(
    store: &Store,
    identity: &Identity,
    ending: Ending,
    reporting: Option<Reporting>,
) -> Result<hoh_core::persistence::VerifierCompleted, PersistError> {
    let held = identity.clone();
    let (outcome, ending) = tokio::task::spawn_blocking(move || {
        let mut ending = ending;
        let outcome = verifier_outcome(&held, &mut ending, reporting.as_ref());
        (outcome, ending)
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the completion of a verifier ended: {e}")))?;
    store
        .complete_verifier_attempt(VerifierCompletion {
            attempt_id: identity.attempt_id.clone(),
            ending,
            outcome,
        })
        .await
}

/// Complete writer attempt `identity.attempt_id`, which ended successfully with its processes proven gone: its outcome
/// decided off the async threads, then one commit carrying the workspace's record when the store asks for it (an
/// interrupted end, REQ-019 behaviour 6).
pub(super) async fn complete(
    store: &Store,
    project: &Path,
    identity: Identity,
    ending: Ending,
) -> Result<WriterCompleted, PersistError> {
    let held = project.to_path_buf();
    let attempt_id = identity.attempt_id.clone();
    let (outcome, ending) = tokio::task::spawn_blocking(move || {
        let mut ending = ending;
        let outcome = outcome(&held, &identity, &mut ending);
        (outcome, ending)
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the completion of a writer ended: {e}")))?;
    super::control::recorded_end(project, move |workspace| {
        let mut ending = ending.clone();
        ending.observed.workspace = workspace;
        store.complete_writer_attempt(WriterCompletion {
            attempt_id: attempt_id.clone(),
            ending,
            outcome: outcome.clone(),
        })
    })
    .await
}

/// Complete writer attempt `row`, which a reconcile found with its work observed done and its process proven gone
/// (decision 4's recovery, [`hoh_core::reconcile::Disposition::Complete`]): its bridge read again from its attempt
/// directory, then the same path as the runner's own end, its end taken as its work recorded it (else `at`).
pub(super) async fn recover(
    store: &Store,
    project: &Path,
    row: &UnfinishedRow,
    cause: &str,
    at: std::time::SystemTime,
    draw: u32,
) -> Result<WriterCompleted, PersistError> {
    let identity = Identity {
        run_id: row.run_id.clone(),
        task_id: row.task_id.clone(),
        attempt_id: row.attempt_id.clone(),
        role: row.role.clone(),
        role_epoch: u64::try_from(row.role_epoch).map_err(|_| {
            PersistError::IntegrityViolation(format!(
                "attempt {}: role epoch {}",
                row.attempt_id, row.role_epoch
            ))
        })?,
    };
    let bridge = super::product::bridge_of(project, &row.attempt_id);
    let (messages, truncated) =
        tokio::task::spawn_blocking(move || super::product::read_bridge(&bridge))
            .await
            .map_err(|e| PersistError::Storage(format!("the read of a bridge ended: {e}")))?;
    let mut observed = Observed::default();
    (observed.messages, observed.messages_truncated) = (messages, truncated);
    let ending = Ending {
        success: true,
        detail: format!("its work was observed done; completed by a reconcile ({cause})"),
        class: row
            .work_class
            .unwrap_or(FailureClass::Transient { retry_after: None }),
        at: row.work_ended_at.unwrap_or(at),
        draw,
        observed,
        unknown_effect: false,
    };
    if row.writer {
        complete(store, project, identity, ending).await
    } else {
        // A verifier's: its result from the same bridge (decision 11); its final message is not kept across a restart,
        // so a COMPLETED result cannot be reported here and is corrected (verified again).
        complete_verifier(store, &identity, ending, None)
            .await
            .map(|_| WriterCompleted::AlreadyEnded)
    }
}

#[cfg(test)]
mod tests;
