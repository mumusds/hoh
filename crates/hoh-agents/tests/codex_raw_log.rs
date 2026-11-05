//! An App Server raw log that cannot be written whole fails the attempt even when the handshake is refused: in
//! ADR-012 §3 an incomplete raw log comes before a refused protocol request, so codex is not tried again on exec
//! (M1R-02). In a test binary of its own: the file size limit it sets applies to the whole process.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::codex::{self, Launch, Mode, Turn};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::CancelLatch;
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use std::time::Duration;
use tokio::signal::unix::{SignalKind, signal};

/// SIGXFSZ on Linux (x86_64 and aarch64).
const SIGXFSZ: i32 = 25;

#[tokio::test]
async fn a_raw_log_cut_short_during_a_refused_handshake_fails_the_attempt() -> R {
    // SIGXFSZ would end this process; with a handler, a write past the limit fails (EFBIG) instead.
    let _xfsz = signal(SignalKind::from_raw(SIGXFSZ))?;
    let world = World::new()?;
    let fake = world.dir.path().join("codex");
    // app-server refuses initialize after 8 MiB on stderr; exec would settle.
    std::fs::write(
        &fake,
        "#!/bin/bash\nif [ \"$1\" = app-server ]; then\n\
         head -c 8388608 /dev/zero >&2\n\
         read -r line\n\
         echo '{\"id\":0,\"error\":{\"code\":-32601,\"message\":\"Method not found: initialize\"}}'\n\
         cat > /dev/null\nexit 0\nfi\n\
         cat > /dev/null\n\
         echo '{\"type\":\"thread.started\",\"thread_id\":\"t-1\"}'\n\
         echo '{\"type\":\"turn.started\"}'\n\
         echo '{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}'\nexit 0\n",
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
    let attempt = world.attempt_in("codex", "A1", "S1", "0.166.0").await?;
    let turn = Turn {
        launch: Launch {
            program: fake.clone(),
            app_server_help: Some("--listen".to_owned()),
            exec_help: "--json --dangerously-bypass-approvals-and-sandbox".to_owned(),
            exec_resume_help: None,
            cwd: world.dir.path().to_path_buf(),
            yolo: false,
            resume: None,
            model: None,
            known: Default::default(),
        },
        prompt: "hello".to_owned(),
        logs: world.dir.path().join("logs"),
        escalation: Turn::default_escalation(),
        handshake_timeout: Duration::from_secs(10),
        deadline: Some(Duration::from_secs(60)),
    };
    // app-server.stderr.log stops at 4 MiB of the 8 MiB written; nothing else this process writes comes near it.
    let before = getrlimit(Resource::Fsize);
    setrlimit(
        Resource::Fsize,
        Rlimit {
            current: Some(4 << 20),
            maximum: before.maximum,
        },
    )?;
    let outcome = codex::run(
        &world.store,
        &attempt,
        &turn,
        &CancelLatch::new(),
        tokio::sync::mpsc::channel(1).1,
    )
    .await;
    setrlimit(Resource::Fsize, before)?;
    let outcome = outcome?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref(), outcome.mode),
        (
            AttemptState::Failed,
            Some("RAW_LOG_INCOMPLETE"),
            Mode::AppServer
        )
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.failure_reason.as_deref()),
        (AttemptState::Failed, Some("RAW_LOG_INCOMPLETE"))
    );
    let degraded = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .filter(|(k, _)| k == "adapter.degraded")
        .count();
    assert_eq!(degraded, 0, "codex was not tried again on exec");
    Ok(())
}
