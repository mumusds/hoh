//! Decision 3's table from a writer's bridge, each row by the outcome it gives the store (docs/adr/ADR-028.md).

use super::*;
use std::process::Command;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn identity() -> Identity {
    Identity {
        run_id: "R".to_owned(),
        task_id: "D".to_owned(),
        attempt_id: "A".to_owned(),
        role: "developer".to_owned(),
        role_epoch: 1,
    }
}

/// A TaskResult line of attempt A with `status`, its fields overridden by `with`.
fn line(status: &str, with: serde_json::Value) -> String {
    let mut result = json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "D", "attempt_id": "A", "role": "developer",
        "role_epoch": 1, "status": status, "summary": format!("{status} summary"), "candidate_id": null,
        "evidence": [], "issues": [], "next_action": "verify"
    });
    if let (Some(result), Some(with)) = (result.as_object_mut(), with.as_object()) {
        for (k, v) in with {
            result.insert(k.clone(), v.clone());
        }
    }
    result.to_string()
}

fn ending(lines: &[String], truncated: bool) -> Ending {
    let mut ending = Ending {
        success: true,
        detail: "turn".to_owned(),
        class: hoh_core::retry::FailureClass::Transient { retry_after: None },
        at: std::time::UNIX_EPOCH,
        draw: 0,
        observed: Default::default(),
        unknown_effect: false,
    };
    ending.observed.messages = lines.to_vec();
    ending.observed.messages_truncated = truncated;
    ending
}

fn reasons(outcome: &WriterOutcome) -> String {
    match outcome {
        WriterOutcome::Correct { reasons } => format!("correct: {}", reasons.join("; ")),
        WriterOutcome::Decide { reason, detail } => format!("decide {reason}: {detail}"),
        WriterOutcome::Freeze { job, result, .. } => {
            format!("freeze {} for {:?}", job.sealed.candidate_id, result.status)
        }
    }
}

fn git(dir: &Path, args: &[&str]) -> R {
    let out = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(())
}

/// A clean repository with a source, the specification and the test plan committed where the layout puts them.
fn repository() -> R<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    git(root, &["init", "-q"])?;
    // The layout's inputs where the freeze takes them (`inputs()`), and a source of the project's own.
    let layout = inputs();
    for (path, text) in [
        (Path::new("src/work.rs"), "pub fn a() {}\n"),
        (layout.spec.as_path(), "{\"requirements\": []}\n"),
        (layout.test_plan.as_path(), "{\"checks\": {}}\n"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(root, &["add", "."])?;
    git(root, &["commit", "-q", "-m", "work"])?;
    Ok(dir)
}

#[test]
fn no_result_or_one_that_cannot_be_trusted_is_a_correction() -> R {
    // Missing; cut at the bridge's limit even with a valid result read (M2T16D-456); the last TaskResult-shaped line
    // refused although an earlier one was valid (M2T16D-359), the refused line recorded as bridge.rejected.
    let dir = tempfile::tempdir()?;
    let completed = line("COMPLETED", json!({}));
    let other = line("COMPLETED", json!({"attempt_id": "B"}));
    for (lines, truncated, want) in [
        (
            vec![],
            false,
            "correct: it wrote no TaskResult to its bridge",
        ),
        (
            vec![completed.clone()],
            true,
            "correct: its bridge was read only up to its limit",
        ),
        (
            vec![completed.clone(), other],
            false,
            "correct: its last TaskResult was refused",
        ),
    ] {
        let mut ending = ending(&lines, truncated);
        let got = reasons(&outcome(dir.path(), &identity(), &mut ending));
        assert!(got.starts_with(want), "{got}");
        let rejected: Vec<_> = ending
            .observed
            .events
            .iter()
            .filter(|(_, kind, _)| kind == "bridge.rejected")
            .collect();
        assert_eq!(rejected.len(), usize::from(lines.len() == 2), "{got}");
        if let Some((key, _, payload)) = rejected.first() {
            assert_eq!(key, "bridge/rejected/1");
            assert_eq!(payload["attempt_id"], "A");
            assert_eq!(payload["line"], 1);
        }
    }
    // Nothing was prepared for a freeze.
    assert!(!dir.path().join(".hoh").exists());
    Ok(())
}

#[test]
fn a_failed_blocked_or_incomplete_result_needs_a_decision_with_its_summary_and_issues() -> R {
    let dir = tempfile::tempdir()?;
    for (status, reason) in [
        ("FAILED", "agent_failed"),
        ("BLOCKED", "agent_blocked"),
        ("INCOMPLETE", "agent_incomplete"),
    ] {
        let mut ending = ending(&[line(status, json!({}))], false);
        let WriterOutcome::Decide {
            reason: got,
            detail,
        } = outcome(dir.path(), &identity(), &mut ending)
        else {
            return Err(format!("{status}: not a decision").into());
        };
        assert_eq!(got, reason);
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        assert_eq!(detail["summary"], format!("{status} summary"));
        assert_eq!(detail["status"], status);
        assert!(detail["issues"].is_array());
    }
    assert!(!dir.path().join(".hoh").exists());
    Ok(())
}

#[test]
fn a_completed_result_is_frozen_from_the_clean_workspace_with_the_layouts_inputs() -> R {
    // Decision 4: the environment description written first (decision 5), then the freeze's file side of the
    // committed workspace, the specification and test plan where the layout puts them.
    let dir = repository()?;
    let mut ending = ending(&[line("COMPLETED", json!({}))], false);
    let WriterOutcome::Freeze { job, result, .. } = outcome(dir.path(), &identity(), &mut ending)
    else {
        return Err("not a freeze".into());
    };
    assert_eq!(result.status, TaskStatus::Completed);
    assert_eq!(job.run_id, "R");
    let manifest = &job.sealed.manifest;
    assert_eq!(
        (
            manifest.spec.path.as_str(),
            manifest.environment.path.as_str(),
            manifest.test_plan.path.as_str()
        ),
        (
            inputs().spec.to_string_lossy().as_ref(),
            ".hoh/environment.json",
            inputs().test_plan.to_string_lossy().as_ref()
        )
    );
    assert!(dir.path().join(".hoh/environment.json").is_file());
    Ok(())
}

#[test]
fn a_workspace_that_cannot_be_frozen_is_corrected_and_one_the_host_cannot_read_is_decided() -> R {
    // A change left uncommitted: the freeze refuses it, the agent's to correct. No git work tree at all: the host
    // cannot prepare it, not the agent's fault, so nothing is counted against it.
    let dir = repository()?;
    std::fs::write(dir.path().join("src/work.rs"), "pub fn changed() {}\n")?;
    let mut ending = ending(&[line("COMPLETED", json!({}))], false);
    let got = reasons(&outcome(dir.path(), &identity(), &mut ending));
    assert!(
        got.starts_with("correct: its freeze was refused: src/work.rs is not committed"),
        "{got}"
    );
    let plain = tempfile::tempdir()?;
    let got = reasons(&outcome(plain.path(), &identity(), &mut ending));
    assert!(got.starts_with("decide freeze_failed: "), "{got}");
    Ok(())
}

#[test]
fn a_completed_results_self_test_records_are_kept_by_their_digests_the_rest_refused_or_only_referenced()
-> R {
    // docs/adr/ADR-028.md decisions 2 and 14: TaskResult.evidence takes only project-relative paths under the
    // attempt's own .hoh/self-tests/<attempt>/; an absolute path, a .., a link or a path elsewhere is dropped with a
    // bridge.rejected each, never making the result invalid; a file read as a regular file whose bytes match its
    // digest is kept as an object (kind self_test); one that does not match, or is not there, is only referenced.
    let dir = repository()?;
    let tests = dir.path().join(".hoh/self-tests/A");
    hoh_core::state_dir::ensure(&dir.path().join(".hoh"))?;
    std::fs::create_dir_all(&tests)?;
    let report = b"ALL TESTS PASSED\n";
    std::fs::write(tests.join("report.txt"), report)?;
    std::fs::write(tests.join("stale.txt"), b"changed after it was named")?;
    std::os::unix::fs::symlink("report.txt", tests.join("link.txt"))?;
    let digest = |b: &[u8]| -> String {
        <sha2::Sha256 as sha2::Digest>::digest(b)
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect()
    };
    let sha = digest(report);
    let item = |path: &str| json!({"path": path, "sha256": sha});
    let evidence = json!([
        item(".hoh/self-tests/A/report.txt"),
        item(".hoh/self-tests/A/stale.txt"),
        item(".hoh/self-tests/A/missing.txt"),
        item("/etc/passwd"),
        item(".hoh/self-tests/A/../../../src/work.rs"),
        item("src/work.rs"),
        item(".hoh/self-tests/A/link.txt"),
        item(".hoh/self-tests/B/report.txt"),
    ]);
    let mut ending = ending(&[line("COMPLETED", json!({"evidence": evidence}))], false);
    let WriterOutcome::Freeze { self_tests, .. } = outcome(dir.path(), &identity(), &mut ending)
    else {
        return Err("not a freeze".into());
    };
    assert_eq!(self_tests.items.len(), 1, "{self_tests:?}");
    let kept = &self_tests.items[0];
    assert_eq!(
        (kept.kind.as_str(), kept.name.as_str(), kept.sha256.as_str()),
        ("self_test", ".hoh/self-tests/A/report.txt", sha.as_str())
    );
    assert_eq!(hoh_core::objects::get(dir.path(), &sha)?, report);
    let referenced: Vec<&str> = self_tests
        .failed
        .iter()
        .filter_map(|f| f["name"].as_str())
        .collect();
    assert_eq!(
        referenced,
        [
            ".hoh/self-tests/A/stale.txt",
            ".hoh/self-tests/A/missing.txt"
        ]
    );
    let rejected: Vec<String> = ending
        .observed
        .events
        .iter()
        .filter(|(_, kind, _)| kind == "bridge.rejected")
        .filter_map(|(_, _, p)| p["evidence"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        rejected,
        [
            "/etc/passwd",
            ".hoh/self-tests/A/../../../src/work.rs",
            "src/work.rs",
            ".hoh/self-tests/A/link.txt",
            ".hoh/self-tests/B/report.txt"
        ]
    );
    Ok(())
}

#[test]
fn a_verifiers_taskresult_issues_go_with_its_report_for_the_store_to_take_its_confirmations() -> R {
    // docs/adr/ADR-029.md decision 10: a verifier's TaskResult `issues` (its confirmation entries among them) are handed
    // to the store with its report; the store keeps the confirmations of the Issues it was delivered.
    let dir = tempfile::tempdir()?;
    let identity = Identity {
        role: "reviewer".to_owned(),
        ..identity()
    };
    let entry = json!({"issue_id": "I-1", "state": "RESOLVED", "observed": "重放后不再越界"});
    let mut ending = ending(
        &[line(
            "COMPLETED",
            json!({"role": "reviewer", "issues": [entry]}),
        )],
        false,
    );
    ending.observed.final_text = Some(
        "done\n```hoh-report\n{\"role\": \"reviewer\", \"status\": \"PASS\", \"scope\": [\"AC-1-01\"], \"findings\": []}\n```\n"
            .to_owned(),
    );
    let text = "{\"type\": \"result\"}\n";
    let sha256 = hoh_core::objects::put(dir.path(), text.as_bytes())?;
    ending.observed.artifacts = vec![ArtifactRow {
        path: hoh_core::objects::relative_path(&sha256),
        sha256,
        bytes: text.len() as u64,
        mime: "application/x-ndjson".to_owned(),
        kind: "raw_log".to_owned(),
        name: "stdout.jsonl".to_owned(),
    }];
    let reporting = Reporting {
        project: dir.path().to_owned(),
        role: "reviewer".to_owned(),
        session_id: "s-r".to_owned(),
        candidate_id: format!("CAND-{}", "c".repeat(64)),
        spec_sha256: "1".repeat(64),
    };
    match verifier_outcome(&identity, &mut ending, Some(&reporting)) {
        VerifierOutcome::Settle { report } => assert_eq!(report.confirmations, [entry]),
        other => return Err(format!("{other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_verifiers_report_takes_its_session_log_whatever_its_agent_names_it() -> R {
    // docs/adr/ADR-028.md decision 15 (M2T16R-50): the report's raw session is the turn's stdout log, which claude and
    // pi name stdout.jsonl and codex app-server.stdout.jsonl or exec.stdout.jsonl; each settles with that log, never
    // its stderr log.
    let dir = tempfile::tempdir()?;
    for name in [
        "stdout.jsonl",
        "app-server.stdout.jsonl",
        "exec.stdout.jsonl",
    ] {
        let identity = Identity {
            role: "reviewer".to_owned(),
            ..identity()
        };
        let mut ending = ending(&[line("COMPLETED", json!({"role": "reviewer"}))], false);
        ending.observed.final_text = Some(
            "done\n```hoh-report\n{\"role\": \"reviewer\", \"status\": \"PASS\", \"scope\": [\"AC-1-01\"], \"findings\": []}\n```\n"
                .to_owned(),
        );
        // Kept as objects, as the runner keeps them, so the host's check recomputes them (docs/adr/ADR-029.md
        // decision 7).
        let log = |n: &str, text: &str| -> R<ArtifactRow> {
            let sha256 = hoh_core::objects::put(dir.path(), text.as_bytes())?;
            Ok(ArtifactRow {
                path: hoh_core::objects::relative_path(&sha256),
                sha256,
                bytes: text.len() as u64,
                mime: "application/x-ndjson".to_owned(),
                kind: "raw_log".to_owned(),
                name: n.to_owned(),
            })
        };
        let stdout = log(name, "{\"type\": \"result\"}\n")?;
        ending.observed.artifacts = vec![log("stderr.log", "warning\n")?, stdout.clone()];
        let reporting = Reporting {
            project: dir.path().to_owned(),
            role: "reviewer".to_owned(),
            session_id: "s-r".to_owned(),
            candidate_id: format!("CAND-{}", "c".repeat(64)),
            spec_sha256: "1".repeat(64),
        };
        match verifier_outcome(&identity, &mut ending, Some(&reporting)) {
            VerifierOutcome::Settle { report } => {
                let kept: serde_json::Value = serde_json::from_slice(&hoh_core::objects::get(
                    dir.path(),
                    &report.report.sha256,
                )?)?;
                assert_eq!(
                    kept["raw_session"]["sha256"],
                    json!(stdout.sha256),
                    "{name}"
                );
            }
            other => return Err(format!("{name}: {other:?}").into()),
        }
    }
    Ok(())
}

/// A reviewer's successful end in project `dir` whose final message reports `findings`, its stdout log `raw` kept as an
/// object (none when `None`); its reporting in HoH session `s-r` with native session `n-r`.
fn reviewer_end(
    dir: &Path,
    findings: serde_json::Value,
    raw: Option<&str>,
) -> R<(Identity, Ending, Reporting)> {
    let identity = Identity {
        role: "reviewer".to_owned(),
        ..identity()
    };
    let mut ending = ending(&[line("COMPLETED", json!({"role": "reviewer"}))], false);
    ending.observed.final_text = Some(format!(
        "done\n```hoh-report\n{}\n```\n",
        json!({"role": "reviewer", "status": "FAIL", "scope": ["AC-1-01"], "findings": findings})
    ));
    ending.observed.native_session = Some("n-r".to_owned());
    let text = raw.unwrap_or("{\"type\": \"result\"}\n");
    let sha256 = if raw.is_some() {
        hoh_core::objects::put(dir, text.as_bytes())?
    } else {
        "d".repeat(64)
    };
    ending.observed.artifacts = vec![ArtifactRow {
        path: hoh_core::objects::relative_path(&sha256),
        sha256,
        bytes: text.len() as u64,
        mime: "application/x-ndjson".to_owned(),
        kind: "raw_log".to_owned(),
        name: "stdout.jsonl".to_owned(),
    }];
    let reporting = Reporting {
        project: dir.to_owned(),
        role: "reviewer".to_owned(),
        session_id: "s-r".to_owned(),
        candidate_id: format!("CAND-{}", "c".repeat(64)),
        spec_sha256: "1".repeat(64),
    };
    Ok((identity, ending, reporting))
}

#[test]
fn a_verifiers_native_attempt_export_is_completed_and_its_checked_report_goes_to_the_store() -> R {
    // docs/adr/ADR-029.md decision 7 (-400, -450): the export's status is COMPLETED, one of the locked release policy's
    // accepted_attempt_statuses (it was SETTLED); the store is handed the built report, its raw session's object and
    // the export's session ids and status as the host's check read them.
    let dir = tempfile::tempdir()?;
    let finding = json!([{"id": "F1", "severity": "low", "summary": "a typo"}]);
    let (identity, mut ending, reporting) =
        reviewer_end(dir.path(), finding, Some("{\"type\": \"result\"}\n"))?;
    let outcome = verifier_outcome(&identity, &mut ending, Some(&reporting));
    let VerifierOutcome::Settle { report } = outcome else {
        return Err(format!("not settled: {outcome:?}").into());
    };
    let native: serde_json::Value = serde_json::from_slice(&hoh_core::objects::get(
        dir.path(),
        &report.native_attempt.sha256,
    )?)?;
    assert_eq!(native["status"], "COMPLETED");
    assert_eq!(report.native_status, "COMPLETED");
    assert_eq!(report.native_session_ids, ["s-r", "n-r"]);
    assert_eq!(native["session_ids"], json!(report.native_session_ids));
    assert_eq!(&report.raw_session, &ending.observed.artifacts[0]);
    let kept: serde_json::Value =
        serde_json::from_slice(&hoh_core::objects::get(dir.path(), &report.report.sha256)?)?;
    assert_eq!(report.report_json, kept);
    assert_eq!(report.report_json["findings"][0]["id"], "F1");
    Ok(())
}

#[test]
fn a_verifiers_report_with_a_repeated_finding_id_or_no_raw_session_object_is_corrected() -> R {
    // docs/adr/ADR-029.md decision 7 (host side): the host's check refuses it, an output to correct within its round
    // (`verification.output_invalid` and Repair::Protocol); nothing reaches the store's intake.
    let dir = tempfile::tempdir()?;
    let twice = json!([{"id": "F1", "severity": "low", "summary": "a"},
        {"id": "F1", "severity": "high", "summary": "b"}]);
    for (what, findings, raw, why) in [
        ("a finding id twice", twice, Some("{}\n"), "finding id F1"),
        ("no raw session object", json!([]), None, "raw session"),
    ] {
        let (identity, mut ending, reporting) = reviewer_end(dir.path(), findings, raw)?;
        match verifier_outcome(&identity, &mut ending, Some(&reporting)) {
            VerifierOutcome::Correct { reasons } => assert!(
                reasons
                    .iter()
                    .any(|r| r.starts_with("its report: ") && r.contains(why)),
                "{what}: {reasons:?}"
            ),
            other => return Err(format!("{what}: {other:?}").into()),
        }
    }
    Ok(())
}
