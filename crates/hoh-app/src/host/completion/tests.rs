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
        WriterOutcome::Freeze { job, result } => {
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
    for (path, text) in [
        ("src/lib.rs", "pub fn a() {}\n"),
        ("contracts/requirements.json", "{\"requirements\": []}\n"),
        ("tests/check-plan.json", "{\"checks\": {}}\n"),
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
    let WriterOutcome::Freeze { job, result } = outcome(dir.path(), &identity(), &mut ending)
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
            "contracts/requirements.json",
            ".hoh/environment.json",
            "tests/check-plan.json"
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
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn changed() {}\n")?;
    let mut ending = ending(&[line("COMPLETED", json!({}))], false);
    let got = reasons(&outcome(dir.path(), &identity(), &mut ending));
    assert!(
        got.starts_with("correct: its freeze was refused: src/lib.rs is not committed"),
        "{got}"
    );
    let plain = tempfile::tempdir()?;
    let got = reasons(&outcome(plain.path(), &identity(), &mut ending));
    assert!(got.starts_with("decide freeze_failed: "), "{got}");
    Ok(())
}
