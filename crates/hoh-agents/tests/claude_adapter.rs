//! M1-T03 Claude adapter against a fake `claude` that replays recorded stream-json (TEST-025-01..03). The adapter's
//! results are checked in the store it wrote: attempts, sessions and events.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World};
use hoh_agents::claude::{self, AttemptRef, Launch, Outcome, PARTIAL_FLAG, Turn, YOLO_FLAG};
use hoh_agents::process::{Escalation, Signal, Target, group_members};
use hoh_core::lifecycle::AttemptState;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::watch;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/protocol/claude/2.1.318"
);

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Fakes and turns for the fixture tests.
trait Fakes {
    fn fake(&self, name: &str, stdout: &[u8], stderr: &[u8], exit: i32) -> R<PathBuf>;
    fn turn(&self, program: &Path, logs: &str) -> R<Turn>;
    async fn run(&self, attempt: &AttemptRef, turn: &Turn) -> R<Outcome>;
    async fn attempt(&self, id: &str) -> R<AttemptRef>;
}

impl Fakes for World {
    async fn attempt(&self, id: &str) -> R<AttemptRef> {
        self.attempt_in(id, "S1", "2.1.318").await
    }

    /// A fake `claude` replaying `stdout` line by line with `stderr` interleaved in as many chunks, then exiting with
    /// `exit`; it records its argv (one file per run) and reads its stdin.
    fn fake(&self, name: &str, stdout: &[u8], stderr: &[u8], exit: i32) -> R<PathBuf> {
        let d = self.dir.path().join(name);
        std::fs::create_dir_all(&d)?;
        let lines: Vec<&[u8]> = stdout.split_inclusive(|b| *b == b'\n').collect();
        let chunk = stderr.len().div_ceil(lines.len().max(1)).max(1);
        for (i, line) in lines.iter().enumerate() {
            std::fs::write(d.join(format!("out.{i:04}")), line)?;
            let part = stderr
                .get(i * chunk..((i + 1) * chunk).min(stderr.len()))
                .unwrap_or_default();
            std::fs::write(d.join(format!("err.{i:04}")), part)?;
        }
        let script = d.join("claude");
        std::fs::write(
            &script,
            format!(
                "#!/bin/bash\nd='{}'\nprintf '%s\\n' \"$@\" > \"$d/argv.$$\"\ncat > \"$d/stdin.$$\"\n\
                 for f in \"$d\"/out.*; do cat \"$f\"; cat \"$d/err.${{f##*.}}\" >&2; done\nexit {exit}\n",
                d.display()
            ),
        )?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok(script)
    }

    fn turn(&self, program: &Path, logs: &str) -> R<Turn> {
        Ok(Turn {
            launch: Launch {
                program: program.to_path_buf(),
                help: std::fs::read_to_string(format!("{FIXTURES}/help.txt"))?,
                cwd: self.dir.path().to_path_buf(),
                yolo: true,
                partial: false,
                resume: None,
                root: false,
            },
            prompt: b"Use the Bash tool to run `echo hoh`, then reply.".to_vec(),
            logs: self.dir.path().join(logs),
            escalation: Turn::default_escalation(),
            deadline: Some(Duration::from_secs(60)),
        })
    }

    async fn run(&self, attempt: &AttemptRef, turn: &Turn) -> R<Outcome> {
        let (_cancel, quiet) = watch::channel(false);
        Ok(claude::run(&self.store, attempt, turn, quiet).await?)
    }
}

fn fixture(case: &str) -> R<Vec<u8>> {
    Ok(std::fs::read(format!("{FIXTURES}/{case}/stdout.jsonl"))?)
}

/// The init frame's (session_id, model).
fn init(stdout: &[u8]) -> R<(String, String)> {
    for line in stdout.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
        let frame: serde_json::Value = serde_json::from_slice(line)?;
        if frame["type"] == "system" && frame["subtype"] == "init" {
            return Ok((
                frame["session_id"]
                    .as_str()
                    .ok_or("no session id")?
                    .to_owned(),
                frame["model"].as_str().ok_or("no model")?.to_owned(),
            ));
        }
    }
    Err("no init frame".into())
}

/// The argv the fake was started with, for each run.
fn argvs(fake: &Path) -> R<Vec<Vec<String>>> {
    let dir = fake.parent().ok_or("no dir")?;
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("argv."))
        {
            out.push(
                std::fs::read_to_string(path)?
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            );
        }
    }
    Ok(out)
}

#[tokio::test]
async fn test_025_01_a_recorded_tool_turn_replays_byte_exact_and_resumes_by_session_id() -> R {
    let world = World::new()?;
    let stdout = fixture("tool-success")?;
    // 2 MiB of stderr, interleaved with the frames.
    let stderr: Vec<u8> = (0..)
        .map(|i| format!("stderr line {i}\n"))
        .flat_map(String::into_bytes)
        .take(2 << 20)
        .collect();
    let fake = world.fake("tool", &stdout, &stderr, 0)?;
    let (session, model) = init(&stdout)?;

    let first = world.attempt("A1").await?;
    let outcome = world.run(&first, &world.turn(&fake, "logs/A1")?).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Settled, None)
    );
    assert_eq!(outcome.stdout_sha256, hex(&stdout));
    assert_eq!(outcome.stderr_sha256, hex(&stderr));
    assert_eq!(std::fs::read(&outcome.stdout)?, stdout);
    let reader = world.store.reader()?;
    let row = reader.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(row.state, AttemptState::Settled);
    assert_eq!(row.resolved_model.as_deref(), Some(model.as_str()));
    assert_eq!(row.session_id.as_deref(), Some("S1"));
    assert!(row.usage.is_some() && row.cost_usd.is_some());
    let s = reader.session("S1")?.ok_or("no session")?;
    assert_eq!(s.native_id.as_deref(), Some(session.as_str()));

    // The second turn resumes the session the first one reported.
    let second = world.attempt("A2").await?;
    let mut turn = world.turn(&fake, "logs/A2")?;
    turn.launch.resume = outcome.native_session.clone();
    let resumed = world.run(&second, &turn).await?;
    assert_eq!(resumed.state, AttemptState::Settled);
    let at = resumed
        .args
        .iter()
        .position(|a| a == "--resume")
        .ok_or("no --resume")?;
    assert_eq!(resumed.args.get(at + 1), Some(&session));
    for args in argvs(&fake)? {
        assert!(
            !args.windows(2).any(|w| w == ["--permission-mode", "auto"]),
            "{args:?}"
        );
        assert_eq!(
            &args[..4],
            ["-p", "--output-format", "stream-json", "--verbose"]
        );
    }
    assert!(
        argvs(&fake)?
            .iter()
            .any(|a| a.windows(2).any(|w| w == ["--resume", session.as_str()]))
    );
    assert_eq!(
        reader.session("S1")?.and_then(|s| s.native_id),
        Some(session)
    );
    Ok(())
}

#[tokio::test]
async fn test_025_02_failed_turns_and_missing_flags() -> R {
    let world = World::new()?;
    let success = fixture("success")?;
    let lines: Vec<&[u8]> = success.split_inclusive(|b| *b == b'\n').collect();
    let result_at = lines
        .iter()
        .position(|l| {
            serde_json::from_slice::<serde_json::Value>(l).is_ok_and(|f| f["type"] == "result")
        })
        .ok_or("no result frame")?;
    let with_result = |edit: &dyn Fn(&mut serde_json::Value)| -> R<Vec<u8>> {
        let mut out = Vec::new();
        for (i, l) in lines.iter().enumerate() {
            if i == result_at {
                let mut f: serde_json::Value = serde_json::from_slice(l)?;
                edit(&mut f);
                out.extend(serde_json::to_vec(&f)?);
                out.push(b'\n');
            } else {
                out.extend_from_slice(l);
            }
        }
        Ok(out)
    };
    let no_result: Vec<u8> = lines
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != result_at)
        .flat_map(|(_, l)| l.to_vec())
        .collect();
    let mut with_junk = lines[..1].concat();
    with_junk.extend_from_slice(b"this line is not JSON\n");
    with_junk.extend(lines[1..].concat());
    let help = std::fs::read_to_string(format!("{FIXTURES}/help.txt"))?;
    assert!(
        help.contains(PARTIAL_FLAG) && help.contains(YOLO_FLAG),
        "the recorded help offers both flags"
    );

    // (name, stdout, exit, edit of the launch, expected state and reason)
    type Row<'a> = (
        &'a str,
        Vec<u8>,
        i32,
        Box<dyn Fn(&mut Launch)>,
        AttemptState,
        Option<&'a str>,
    );
    let no_partial = help.replace(PARTIAL_FLAG, "");
    let no_yolo = help.replace(YOLO_FLAG, "");
    let rows: Vec<Row> = vec![
        (
            "max-turns",
            fixture("max-turns-error")?,
            1,
            Box::new(|_| {}),
            AttemptState::Failed,
            Some("error_max_turns"),
        ),
        (
            "is-error",
            with_result(&|f| f["is_error"] = true.into())?,
            0,
            Box::new(|_| {}),
            AttemptState::Failed,
            Some("is_error"),
        ),
        (
            "eof",
            no_result,
            0,
            Box::new(|_| {}),
            AttemptState::Failed,
            Some("PROTOCOL_INCOMPLETE"),
        ),
        (
            "exit-1",
            success.clone(),
            1,
            Box::new(|_| {}),
            AttemptState::Failed,
            Some("exit code 1"),
        ),
        (
            "junk",
            with_junk.clone(),
            0,
            Box::new(|_| {}),
            AttemptState::Settled,
            None,
        ),
        (
            "no-partial",
            success.clone(),
            0,
            Box::new(move |l| {
                l.partial = true;
                l.help = no_partial.clone();
            }),
            AttemptState::Settled,
            None,
        ),
        (
            "no-yolo",
            success.clone(),
            0,
            Box::new(move |l| l.help = no_yolo.clone()),
            AttemptState::Failed,
            Some("BLOCKED_CAPABILITY"),
        ),
    ];
    for (i, (name, stdout, exit, edit, state, reason)) in rows.into_iter().enumerate() {
        let fake = world.fake(name, &stdout, b"", exit)?;
        let attempt = world.attempt(&format!("A{i}")).await?;
        let mut turn = world.turn(&fake, &format!("logs/{name}"))?;
        edit(&mut turn.launch);
        let outcome = world.run(&attempt, &turn).await?;
        assert_eq!(
            (outcome.state, outcome.failure.as_deref()),
            (state, reason),
            "{name}"
        );
        let row = world
            .store
            .reader()?
            .attempt(&attempt.attempt_id)?
            .ok_or("no attempt")?;
        assert_eq!(
            (row.state, row.failure_reason.as_deref()),
            (state, reason),
            "{name} in the store"
        );
        assert!(
            !outcome
                .args
                .windows(2)
                .any(|w| w == ["--permission-mode", "auto"]),
            "{name}"
        );
        match name {
            "junk" => {
                assert_eq!(outcome.diagnostics, 1);
                assert_eq!(
                    std::fs::read(&outcome.stdout)?,
                    with_junk,
                    "the raw line is kept"
                );
                let diagnostics = world
                    .store
                    .reader()?
                    .events("R")?
                    .into_iter()
                    .filter(|(k, p)| {
                        k == "adapter.diagnostic" && p.to_string().contains("not JSON")
                    })
                    .count();
                assert_eq!(diagnostics, 1);
            }
            "no-partial" => {
                assert!(!outcome.args.iter().any(|a| a == PARTIAL_FLAG));
                let caps = world
                    .store
                    .reader()?
                    .session("S1")?
                    .ok_or("no session")?
                    .capabilities;
                assert_eq!(caps["partial_messages"], false);
            }
            "no-yolo" => assert!(argvs(&fake)?.is_empty(), "no process may start"),
            _ => {}
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_025_03_cancel_escalates_to_the_group_and_ends_cancelled() -> R {
    let world = World::new()?;
    let d = world.dir.path().join("stubborn");
    std::fs::create_dir_all(&d)?;
    let log = d.join("signals.log");
    let script = d.join("claude");
    // Ignores SIGINT and SIGTERM (logging them), leaves a grandchild in its group, prints its init frame and waits.
    std::fs::write(
        &script,
        format!(
            "#!/bin/bash\nlog='{}'\ntrap 'echo INT $(date +%s.%N) >> \"$log\"' INT\ntrap 'echo TERM $(date +%s.%N) >> \"$log\"' TERM\n\
             sleep 600 &\necho '{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"sid-1\",\"model\":\"m\"}}'\n\
             while true; do sleep 0.1; done\n",
            log.display()
        ),
    )?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&script, "logs/A1")?;
    turn.escalation = Escalation {
        native: Some(Signal::Interrupt),
        native_grace: Duration::from_secs(1),
        term_grace: Duration::from_secs(1),
        ..Turn::default_escalation()
    };
    let (cancel, cancelled) = watch::channel(false);
    let store = &world.store;
    let running = claude::run(store, &attempt, &turn, cancelled);
    let trigger = async {
        // Cancel once the process is running and has spoken.
        for _ in 0..100 {
            if store
                .reader()?
                .attempt("A1")?
                .is_some_and(|a| a.state == AttemptState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let at = Instant::now();
        cancel.send(true)?;
        Ok::<Instant, Box<dyn std::error::Error>>(at)
    };
    let (outcome, at) = tokio::join!(running, trigger);
    let (outcome, at) = (outcome?, at?);
    assert!(
        at.elapsed() <= Duration::from_secs(3) + Duration::from_secs(2),
        "took {:?}",
        at.elapsed()
    );
    assert_eq!(outcome.state, AttemptState::Cancelled);
    let order: Vec<(Signal, Target)> = outcome.steps.iter().map(|s| (s.signal, s.target)).collect();
    assert_eq!(
        order,
        [
            (Signal::Interrupt, Target::Leader),
            (Signal::Terminate, Target::Group),
            (Signal::Kill, Target::Group)
        ]
    );
    for pair in outcome.steps.windows(2) {
        assert!(
            pair[1].at.duration_since(pair[0].at) >= Duration::from_secs(1),
            "{:?}",
            outcome.steps
        );
    }
    let seen: Vec<String> = std::fs::read_to_string(&log)?
        .lines()
        .filter_map(|l| l.split(' ').next().map(str::to_owned))
        .collect();
    assert_eq!(seen, ["INT", "TERM"]);
    let pid = outcome.pid.ok_or("no pid")?;
    assert!(
        group_members(pid).is_empty(),
        "the group still has {:?}",
        group_members(pid)
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );
    Ok(())
}
