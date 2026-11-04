//! The `hoh` binary serves as the WAL self-test's second process (REQ-062, M2T01R-02).

use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand, wal_self_test};
use std::time::Duration;

#[test]
fn the_hoh_binary_is_the_wal_probe() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let probe = ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    };
    wal_self_test(dir.path(), &probe)?;
    assert_eq!(
        std::fs::read_dir(dir.path())?.count(),
        0,
        "the self-test left files"
    );
    Ok(())
}
