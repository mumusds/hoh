use super::*;
use std::process::Command;

use hoh_core::persistence::{
    Intent, NewEvent, Options, RunTarget, SpecAction, SpecRequest, SubmitCommand,
};
use hoh_core::protocol::{ApproveSpec, ProposeSpecChange, SpecDecision};
use serde_json::{Value, json};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn git(root: &Path, args: &[&str]) -> R {
    let out = Command::new("git")
        .current_dir(root)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
        .args(args)
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}

// SQLite observations remain in an explicit test-only module (AC-062-05).
#[cfg(test)]
mod db {
    use super::*;
    pub(super) fn rows(root: &Path, sql: &str) -> R<Value> {
        let out = Command::new("python3").args(["-c",
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); c.row_factory=sqlite3.Row; print(json.dumps([dict(r) for r in c.execute(sys.argv[2])]))"])
        .arg(root.join(".hoh/state.db")).arg(sql).output()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(serde_json::from_slice(&out.stdout)?)
    }
}
use db::rows;

fn approved_rows(root: &Path) -> R<Vec<Value>> {
    ["specs", "requirements", "acceptance_criteria"]
        .iter()
        .map(|table| rows(root, &format!("SELECT * FROM {table} ORDER BY rowid")))
        .collect()
}

fn command(key: &str, revision: i64, target: RunTarget) -> SubmitCommand {
    SubmitCommand {
        command_id: key.into(),
        idempotency_key: key.into(),
        kind: match &target {
            RunTarget::Start { .. } => "StartRun",
            RunTarget::Specification(request) => match request.action {
                SpecAction::Approve(_) => "ApproveSpec",
                SpecAction::Propose(_) => "ProposeSpecChange",
            },
            _ => unreachable!("fixture only starts, approves and proposes"),
        }
        .into(),
        run_id: "R".into(),
        expected_revision: revision,
        payload: json!({"key":key}),
        payload_sha256: hoh_core::specindex::digest(key.as_bytes()),
        target,
        event: NewEvent {
            project_id: "P".into(),
            event_id: format!("event-{key}"),
            run_id: Some("R".into()),
            kind: "run.created".into(),
            payload: json!({"goal":"specification"}),
            intent: Intent::Progress,
        },
        outbox: None,
    }
}

fn inputs() -> Inputs {
    Inputs {
        spec: hoh_core::specindex::PATH.into(),
        environment: "environment.json".into(),
        test_plan: "tests/check-plan.json".into(),
    }
}

#[tokio::test]
async fn test_034_04_index_drift_refuses_freeze() -> R {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    for (path, bytes) in [
        (".gitignore", ".hoh/\n"),
        (
            "docs/spec/a.md",
            "# 用户\n无\n# 范围\n无\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A Work\n### AC-A-01\ngiven: a\nwhen: b\nthen: c\ncheck: unit\n",
        ),
        (
            "docs/spec/index.json",
            "{\"requirements\":[],\"tampered\":true}\n",
        ),
        ("environment.json", "{}\n"),
        ("tests/check-plan.json", "{\"checks\":{}}\n"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, bytes)?;
    }
    let index = hoh_core::specindex::generate_index(&crate::host::spec::sources(root)?)?;
    std::fs::write(root.join(hoh_core::specindex::PATH), index.bytes())?;
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &["commit", "-qm", "specification"],
    ] {
        git(root, args)?;
    }
    let store = Store::open(
        &root.join(".hoh/state.db"),
        Options {
            wal_probe: Some(hoh_core::persistence::selftest::ProbeCommand {
                program: std::env::current_exe()?,
                args: [
                    "verify::freeze::tests::wal_probe_helper",
                    "--exact",
                    "--ignored",
                    "--nocapture",
                    "--test-threads=1",
                ]
                .map(Into::into)
                .to_vec(),
                env: vec![],
                answer_timeout: std::time::Duration::from_secs(10),
            }),
            ..Options::new(hoh_core::spawn::Direct::shared())
        },
    )?;
    store
        .ensure_project("P", root.to_str().ok_or("non-UTF-8 root")?)
        .await?;
    store
        .submit_command(command(
            "start",
            0,
            RunTarget::Start {
                mode: "PLAN".into(),
                budget: hoh_core::config::Config::default().budget,
                policy: "balanced".into(),
                assumptions: Default::default(),
                unattended: false,
                mode_cycle: hoh_core::config::Config::default().ui.mode_cycle,
            },
        ))
        .await?;
    let revision = store.reader()?.snapshot("R")?.ok_or("no run")?.0.revision;
    store
        .submit_command(command(
            "approve",
            revision,
            RunTarget::Specification(Box::new(SpecRequest {
                origin: hoh_core::origin::Origin::User,
                action: SpecAction::Approve(ApproveSpec {
                    spec_sha256: index.sha256(),
                    decision: SpecDecision::ApproveOnly,
                    note: None,
                }),
                files: crate::host::spec::files(root),
            })),
        ))
        .await?;
    let before = approved_rows(root)?;
    assert_eq!(before[0][0]["revision"], 1);
    assert_eq!(before[0][0]["sha256"], index.sha256());
    let mut tampered = index.value().clone();
    tampered["requirements"][0]["acceptance"][0]["then"] = "manual edit".into();
    std::fs::write(
        root.join(hoh_core::specindex::PATH),
        serde_json::to_vec_pretty(&tampered)?,
    )?;
    git(root, &["commit", "-qam", "manual index edit"])?;
    // Objects present before the refused freeze (the Run's resource bundle pin writes some, M3-T03): the refusal adds none.
    let objects_before = object_files(root)?;
    let result = freeze(&store, root, &root.join(".hoh"), "R", &inputs()).await;
    assert!(
        result.is_err(),
        "index drift must refuse freeze before producing a candidate"
    );
    let error = result.err().ok_or("freeze succeeded")?.to_string();
    assert!(
        error.contains("SPEC_INVALID") && error.contains("docs/spec/index.json"),
        "{error}"
    );
    assert_eq!(
        object_files(root)?,
        objects_before,
        "refused freeze stores no candidate object"
    );
    assert_eq!(rows(root, "SELECT * FROM candidates")?, json!([]));
    let snapshot = crate::host::spec::files(root)?;
    let revision = store.reader()?.snapshot("R")?.ok_or("no run")?.0.revision;
    let proposal = command(
        "regenerate",
        revision,
        RunTarget::Specification(Box::new(SpecRequest {
            origin: hoh_core::origin::Origin::User,
            action: SpecAction::Propose(ProposeSpecChange {
                spec_sha256: index.sha256(),
                reason: "repair manual index edit".into(),
            }),
            files: Ok(snapshot.clone()),
        })),
    );
    store.submit_command(proposal.clone()).await?;
    crate::host::spec::publish(root, &snapshot)?;
    assert_eq!(
        std::fs::read(root.join(hoh_core::specindex::PATH))?,
        index.bytes(),
        "product publication must restore canonical bytes"
    );
    let drift = rows(
        root,
        "SELECT payload_json FROM events WHERE kind='spec.generated_drift'",
    )?;
    assert_eq!(
        drift.as_array().ok_or("no drift array")?.len(),
        1,
        "one persistent drift event"
    );
    let payload: Value = serde_json::from_str(
        drift[0]["payload_json"]
            .as_str()
            .ok_or("no drift payload")?,
    )?;
    assert_eq!(payload["path"], hoh_core::specindex::PATH);
    assert_eq!(payload["spec_revision"], 1);
    assert_eq!(approved_rows(root)?, before);
    store.submit_command(proposal).await?;
    crate::host::spec::publish(root, &snapshot)?;
    assert_eq!(
        rows(
            root,
            "SELECT payload_json FROM events WHERE kind='spec.generated_drift'"
        )?,
        drift
    );
    assert_eq!(approved_rows(root)?, before);
    assert_eq!(
        rows(root, "SELECT * FROM change_requests")?,
        json!([]),
        "repairing the approved digest must not propose a CR"
    );
    git(root, &["commit", "-qam", "regenerate"])?;
    let job = prepare_job(
        root,
        &root.join(".hoh"),
        "R",
        &Inputs {
            spec: hoh_core::specindex::PATH.into(),
            environment: "environment.json".into(),
            test_plan: "tests/check-plan.json".into(),
        },
    )?;
    assert_eq!(job.sealed.manifest.spec.sha256, index.sha256());
    assert_eq!(
        std::fs::read(root.join(hoh_core::specindex::PATH))?,
        index.bytes()
    );
    Ok(())
}

// A child entry point for the real SQLite WAL self-test, not an acceptance test.
#[test]
#[ignore = "invoked by the store WAL probe in test_034_04"]
fn wal_probe_helper() {
    std::process::exit(hoh_core::persistence::selftest::probe_main());
}

fn repository_with_ignored_markdown(index_includes_ignored: bool) -> R<tempfile::TempDir> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    std::fs::create_dir_all(root.join("docs/spec"))?;
    std::fs::create_dir_all(root.join("tests"))?;
    std::fs::write(root.join(".gitignore"), ".hoh/\n/docs/spec/extra.md\n")?;
    std::fs::write(root.join("environment.json"), "{}\n")?;
    std::fs::write(root.join("tests/check-plan.json"), "{\"checks\":{}}\n")?;
    let markdown = "# 用户\n# 范围\n# 非目标\n# 决策\n# 开放问题\n# 需求\n";
    std::fs::write(root.join("docs/spec/a.md"), markdown)?;
    std::fs::write(root.join("docs/spec/extra.md"), markdown)?;
    let mut sources = crate::host::spec::sources(root)?;
    if !index_includes_ignored {
        sources.retain(|s| s.path == "docs/spec/a.md");
    }
    let index = hoh_core::specindex::generate_index(&sources)?;
    std::fs::write(root.join(hoh_core::specindex::PATH), index.bytes())?;
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &["commit", "-qm", "committed sources"],
    ] {
        git(root, args)?;
    }
    Ok(dir)
}

#[test]
fn test_034_04_freeze_ignores_markdown_outside_the_commit() -> R {
    let dir = repository_with_ignored_markdown(false)?;
    let root = dir.path();
    let before = prepare_job(root, &root.join(".hoh"), "R", &inputs());
    assert!(
        before.is_ok(),
        "ignored Markdown must not enter freeze: {before:?}"
    );
    let before = before?;
    assert!(
        !before
            .sealed
            .manifest
            .source_files
            .iter()
            .any(|f| f.path == "docs/spec/extra.md")
    );
    // Even invalid ignored bytes must not affect the same committed candidate.
    std::fs::write(root.join("docs/spec/extra.md"), "not a specification")?;
    let after = prepare_job(root, &root.join(".hoh"), "R", &inputs())?;
    assert_eq!(before.sealed.candidate_id, after.sealed.candidate_id);
    std::fs::remove_file(root.join("docs/spec/extra.md"))?;
    let removed = prepare_job(root, &root.join(".hoh"), "R", &inputs())?;
    assert_eq!(before.sealed.candidate_id, removed.sealed.candidate_id);
    Ok(())
}

#[test]
fn test_034_04_freeze_rejects_index_including_ignored_markdown() -> R {
    let dir = repository_with_ignored_markdown(true)?;
    let root = dir.path();
    let result = prepare_job(root, &root.join(".hoh"), "R", &inputs());
    assert!(
        matches!(result, Err(FreezeError::SpecInvalid(_))),
        "an index derived from ignored Markdown must be refused: {result:?}"
    );
    assert!(!root.join(".hoh/objects").exists());
    std::fs::remove_file(root.join("docs/spec/extra.md"))?;
    assert!(matches!(
        prepare_job(root, &root.join(".hoh"), "R", &inputs()),
        Err(FreezeError::SpecInvalid(_))
    ));
    Ok(())
}

/// Every file under the project's object directory (`.hoh/objects`), relative and sorted; empty when it does not exist.
fn object_files(root: &std::path::Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let base = root.join(".hoh/objects");
    let mut out = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            } else {
                out.push(entry.path().strip_prefix(&base)?.display().to_string());
            }
        }
    }
    out.sort();
    Ok(out)
}
