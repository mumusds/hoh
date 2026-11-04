//! A raw log that cannot be written whole fails the turn (M1T03R-04). In a test binary of its own: the file size
//! limit it sets applies to the whole process.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::claude::{self, Launch, Turn};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::CancelLatch;
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};

/// SIGXFSZ on Linux (x86_64 and aarch64).
const SIGXFSZ: i32 = 25;

const HELP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/claude/2.1.318/help.txt"
);

#[tokio::test]
async fn a_raw_log_that_cannot_be_written_whole_fails_the_turn() -> R {
    // SIGXFSZ would end this process; with a handler, a write past the limit fails (EFBIG) instead.
    let _xfsz = signal(SignalKind::from_raw(SIGXFSZ))?;
    let world = World::new()?;
    let fake = world.dir.path().join("claude");
    std::fs::write(
        &fake,
        "#!/bin/bash\ncat > /dev/null\n\
         echo '{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sid-1\",\"model\":\"m\"}'\n\
         head -c 8388608 /dev/zero >&2\n\
         echo '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"ok\"}'\nexit 0\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let attempt = world.attempt_in("A1", "S1", "2.1.318").await?;
    let turn = Turn {
        launch: Launch {
            program: fake,
            help: std::fs::read_to_string(HELP)?,
            cwd: world.dir.path().to_path_buf(),
            yolo: true,
            partial: false,
            resume: None,
            root: false,
        },
        prompt: b"hello".to_vec(),
        logs: world.dir.path().join("logs"),
        escalation: Turn::default_escalation(),
        deadline: Some(Duration::from_secs(60)),
    };
    // stderr.log stops at 4 MiB of the 8 MiB written; nothing else this process writes meanwhile comes near it.
    let before = getrlimit(Resource::Fsize);
    setrlimit(
        Resource::Fsize,
        Rlimit {
            current: Some(4 << 20),
            maximum: before.maximum,
        },
    )?;
    let outcome = claude::run(&world.store, &attempt, &turn, &CancelLatch::new()).await;
    setrlimit(Resource::Fsize, before)?;
    let outcome = outcome?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("RAW_LOG_INCOMPLETE"))
    );
    assert_eq!(std::fs::metadata(&outcome.stderr)?.len(), 4 << 20);
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.failure_reason.as_deref()),
        (AttemptState::Failed, Some("RAW_LOG_INCOMPLETE"))
    );
    let diagnostic = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .find(|(k, p)| k == "adapter.diagnostic" && p["reason"] == "RAW_LOG_INCOMPLETE")
        .ok_or("no diagnostic event")?;
    assert!(
        diagnostic.1["stderr"]["error"].is_string(),
        "{diagnostic:?}"
    );
    Ok(())
}
