//! The binary must never pretend to be a working product before the CLI exists (M1-T06).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use std::process::Command;

#[test]
fn unimplemented_binary_reports_not_implemented_and_exits_3() {
    let output = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .output()
        .expect("hoh binary should start");
    assert_eq!(
        output.status.code(),
        Some(3),
        "exit code 3 means NOT_IMPLEMENTED/BLOCKED"
    );
    assert!(output.stdout.is_empty(), "stdout is reserved for results");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("NOT_IMPLEMENTED"), "stderr was: {stderr}");
}
