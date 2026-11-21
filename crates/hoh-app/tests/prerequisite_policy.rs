//! CLT-353: a missing prerequisite cannot silently become a passing test.
mod prerequisites;

type R = Result<(), Box<dyn std::error::Error>>;

#[test]
fn missing_prerequisites_require_the_explicit_opt_out() -> R {
    for value in [None, Some(""), Some("0"), Some("true"), Some("1")] {
        let mut command = std::process::Command::new(std::env::current_exe()?);
        command.args([
            "--ignored",
            "--exact",
            "missing_prerequisite_child",
            "--nocapture",
        ]);
        command.env_remove("HOH_TEST_ALLOW_MISSING_PREREQUISITES");
        if let Some(value) = value {
            command.env("HOH_TEST_ALLOW_MISSING_PREREQUISITES", value);
        }
        let output = command.output()?;
        let stderr = String::from_utf8(output.stderr)?;
        assert_eq!(
            output.status.success(),
            value == Some("1"),
            "{value:?}: {stderr}"
        );
        assert!(
            stderr.contains("synthetic unavailable prerequisite"),
            "{stderr}"
        );
        assert_eq!(stderr.contains("SKIP"), value == Some("1"), "{stderr}");
    }
    Ok(())
}

#[test]
#[ignore = "child of missing_prerequisites_require_the_explicit_opt_out"]
fn missing_prerequisite_child() -> R {
    prerequisites::missing("synthetic unavailable prerequisite")
}
