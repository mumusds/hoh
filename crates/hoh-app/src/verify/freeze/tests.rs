use super::*;
use std::process::Command;

type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_034_04_index_drift_refuses_freeze() -> R {
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
    let mut tampered = index.value().clone();
    tampered["requirements"][0]["acceptance"][0]["then"] = "manual edit".into();
    std::fs::write(
        root.join(hoh_core::specindex::PATH),
        serde_json::to_vec_pretty(&tampered)?,
    )?;
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &["commit", "-qm", "drift"],
    ] {
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
    }
    let result = prepare_job(
        root,
        &root.join(".hoh"),
        "R",
        &Inputs {
            spec: "docs/spec/index.json".into(),
            environment: "environment.json".into(),
            test_plan: "tests/check-plan.json".into(),
        },
    );
    assert!(
        result.is_err(),
        "index drift must refuse freeze before producing a candidate"
    );
    let error = result.err().ok_or("freeze succeeded")?.to_string();
    assert!(
        error.contains("SPEC_INVALID") && error.contains("docs/spec/index.json"),
        "{error}"
    );
    assert!(
        !root.join(".hoh/objects").exists(),
        "refused freeze stores no candidate object"
    );
    let before = std::fs::read(root.join(hoh_core::specindex::PATH))?;
    assert_eq!(
        index.drift(Some(&before)),
        Some(serde_json::json!({"path": hoh_core::specindex::PATH}))
    );
    std::fs::write(root.join(hoh_core::specindex::PATH), index.bytes())?;
    let out = Command::new("git")
        .current_dir(root)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-qam",
            "regenerate",
        ])
        .output()?;
    assert!(out.status.success());
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
