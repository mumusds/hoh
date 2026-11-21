//! Prerequisite-dependent tests fail closed; only an explicit CI opt-out permits a diagnostic skip.

pub fn missing(reason: &str) -> Result<(), Box<dyn std::error::Error>> {
    const OPT_OUT: &str = "HOH_TEST_ALLOW_MISSING_PREREQUISITES";
    if std::env::var_os(OPT_OUT).as_deref() != Some(std::ffi::OsStr::new("1")) {
        return Err(format!(
            "missing test prerequisite: {reason}; {OPT_OUT}=1 is an explicit CI opt-out"
        )
        .into());
    }
    use std::io::Write as _;
    writeln!(std::io::stderr(), "SKIP ({OPT_OUT}=1): {reason}")?;
    Ok(())
}
