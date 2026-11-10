//! M1-T03 Claude adapter against a fake `claude` that replays recorded stream-json (TEST-025-01..03). The adapter's
//! results are checked in the store it wrote: attempts, sessions and events.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod common;

use common::{R, World, probed};
use hoh_agents::claude::{self, AttemptRef, Launch, Outcome, PARTIAL_FLAG, Turn, YOLO_FLAG};
use hoh_agents::identity::{group_empty, group_left};
use hoh_agents::process::{Escalation, Signal, Target};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{CancelLatch, CancelReceipt};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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
    fn script(&self, name: &str, body: &str) -> R<PathBuf>;
    fn turn(&self, program: &Path, logs: &str) -> R<Turn>;
    async fn run(&self, attempt: &AttemptRef, turn: &Turn) -> R<Outcome>;
    async fn attempt(&self, id: &str) -> R<AttemptRef>;
}

impl Fakes for World {
    async fn attempt(&self, id: &str) -> R<AttemptRef> {
        self.attempt_in("claude", id, "S1", "2.1.318").await
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

    /// A fake `claude` running `body` with bash.
    fn script(&self, name: &str, body: &str) -> R<PathBuf> {
        let d = self.dir.path().join(name);
        std::fs::create_dir_all(&d)?;
        let script = d.join("claude");
        std::fs::write(&script, format!("#!/bin/bash\n{body}\n"))?;
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
                model: None,
                root: false,
                // What `probe()` finds for the recorded 2.1.318 help.
                capabilities: probed(&[
                    ("structured_events", true),
                    ("partial_messages", true),
                    ("resume", true),
                ]),
            },
            prompt: b"Use the Bash tool to run `echo hoh`, then reply.".to_vec(),
            logs: self.dir.path().join(logs),
            escalation: Turn::default_escalation(),
            deadline: Some(Duration::from_secs(60)),
        })
    }

    async fn run(&self, attempt: &AttemptRef, turn: &Turn) -> R<Outcome> {
        Ok(claude::run(&self.store, attempt, turn, &CancelLatch::new()).await?)
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
/// The contents of the files next to `fake` whose names start with `prefix`.
fn files(fake: &Path, prefix: &str) -> R<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fake.parent().ok_or("no dir")?)? {
        let path = entry?.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(prefix))
        {
            out.push(std::fs::read(path)?);
        }
    }
    Ok(out)
}

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
    // Both turns got the prompt whole on stdin.
    let stdins = files(&fake, "stdin.")?;
    assert_eq!(stdins.len(), 2);
    assert!(stdins.iter().all(|s| *s == turn.prompt), "{stdins:?}");
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

/// Each non-JSON line is its own diagnostic event; the result frame still decides the end.
/// The session row records what the probe found, never more than this claude's help offers: a probe that found no
/// resume makes a session without it although the help has `--resume`, and partial messages the help does not offer
/// are no capability whatever the probe said (M1R-01, REQ-027); no steer and no dialog whatever was probed.
#[tokio::test]
async fn the_session_has_the_capabilities_the_probe_found() -> R {
    let world = World::new()?;
    let fake = world.fake("probed", &fixture("tool-success")?, b"", 0)?;
    let mut turn = world.turn(&fake, "logs/A1")?;
    turn.launch.capabilities = probed(&[
        ("structured_events", true),
        ("partial_messages", true),
        ("resume", false),
        ("mid_turn_steer", true),
        ("ui_dialogs", true),
        ("token_usage", true),
        ("custom_tools", true),
    ]);
    turn.launch.help = turn.launch.help.replace(PARTIAL_FLAG, "");
    let attempt = world.attempt("A1").await?;
    let outcome = world.run(&attempt, &turn).await?;
    assert_eq!(outcome.state, AttemptState::Settled);
    let caps = world
        .store
        .reader()?
        .session("S1")?
        .ok_or("no session")?
        .capabilities;
    assert_eq!(
        (
            &caps["structured_events"],
            &caps["resume"],
            &caps["partial_messages"]
        ),
        (&json!(true), &json!(false), &json!(false)),
        "{caps}"
    );
    // One prompt on stdin, then stdin closes: nothing reaches a running turn, and nobody answers a dialog
    // (M1T04R-12).
    assert_eq!(
        (&caps["mid_turn_steer"], &caps["ui_dialogs"]),
        (&json!(false), &json!(false)),
        "{caps}"
    );
    // It records the result's token usage, so the probe decides that one; it offers no custom tools (M1T04R-15, -17).
    assert_eq!(
        (&caps["token_usage"], &caps["custom_tools"]),
        (&json!(true), &json!(false)),
        "{caps}"
    );
    assert_eq!(
        hoh_core::decision::granted_capabilities(&caps),
        ["structured_events".to_owned(), "token_usage".to_owned()]
            .into_iter()
            .collect()
    );
    Ok(())
}

#[tokio::test]
async fn every_line_that_is_not_json_is_its_own_diagnostic_event() -> R {
    let world = World::new()?;
    let success = fixture("success")?;
    let lines: Vec<&[u8]> = success.split_inclusive(|b| *b == b'\n').collect();
    let mut stdout = lines[..1].concat();
    stdout.extend_from_slice(b"first line that is not JSON\n");
    stdout.extend(lines[1..2].concat());
    stdout.extend_from_slice(b"second line that is not JSON\n");
    stdout.extend(lines[2..].concat());
    let fake = world.fake("two-junk", &stdout, b"", 0)?;
    let attempt = world.attempt("A1").await?;
    let outcome = world
        .run(&attempt, &world.turn(&fake, "logs/two-junk")?)
        .await?;
    assert_eq!(
        (outcome.state, outcome.diagnostics),
        (AttemptState::Settled, 2)
    );
    let heads: Vec<String> = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .filter(|(k, _)| k == "adapter.diagnostic")
        .map(|(_, p)| p["head"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        heads,
        [
            "first line that is not JSON",
            "second line that is not JSON"
        ]
    );
    Ok(())
}

/// A fake `claude` that ignores SIGINT and SIGTERM (logging them), leaves a grandchild in its group, prints its init
/// frame, says it is ready and waits; its signal log and its ready file. The grandchild starts before the signals are
/// blocked, so a SIGTERM to the group ends it; the leader takes them with sigtimedwait, so none is lost in a gap of a
/// shell's own handling, and logs each by a write of its own (the bash traps this replaces forked a `date` there;
/// ADR-022 decision 10, convention v2).
fn stubborn(world: &World) -> R<(PathBuf, PathBuf, PathBuf)> {
    let d = world.dir.path().join("stubborn");
    std::fs::create_dir_all(&d)?;
    let (log, ready) = (d.join("signals.log"), d.join("ready"));
    let script = world.script(
        "stubborn",
        &format!(
            "exec python3 -c '\
import signal, subprocess, sys
log, ready = sys.argv[1], sys.argv[2]
subprocess.Popen([\"sleep\", \"600\"])
wanted = [signal.SIGINT, signal.SIGTERM]
signal.pthread_sigmask(signal.SIG_BLOCK, wanted)
print(sys.argv[3], flush=True)
open(ready, \"w\").close()
for info in iter(lambda: signal.sigtimedwait(wanted, 600), None): f = open(log, \"a\"); f.write((\"INT\" if info.si_signo == signal.SIGINT else \"TERM\") + chr(10)); f.close()
' '{}' '{}' '{INIT}'",
            log.display(),
            ready.display()
        ),
    )?;
    Ok((script, log, ready))
}

/// Cancel attempt `id` once its agent says it is ready (`ready`, its own event: convention v2); the cancel time, the pid, whether its group is empty by `mark` after the
/// cancel (the kernel's look-up, zombies counted: ADR-022 decision 10, convention v2; CIR-001, -052) and, for the
/// message only, a scan of its members: both start 100 ms before the mark and must end by it, so they never see a
/// later state (M1T03R-15).
async fn cancel_and_sample(
    world: &World,
    id: &str,
    cancel: &CancelLatch,
    mark: Duration,
    ready: &Path,
) -> R<(Instant, u32, bool, Result<Vec<u32>, String>)> {
    let pid = running_pid(world, id).await?;
    let end = Instant::now() + Duration::from_secs(10);
    while !ready.exists() {
        if Instant::now() > end {
            return Err("the agent never said it was ready".into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let at = Instant::now();
    assert_eq!(cancel.request(), CancelReceipt::Accepted);
    tokio::time::sleep_until((at + mark - Duration::from_millis(100)).into()).await;
    let empty = group_empty(pid);
    let left = group_left(pid, 0).map_err(|e| e.to_string());
    let ended = at.elapsed();
    if ended > mark {
        return Err(format!("the look ended {ended:?} after the cancel").into());
    }
    Ok((at, pid, empty, left))
}

#[tokio::test]
async fn test_025_03_cancel_escalates_to_the_group_and_ends_cancelled() -> R {
    let world = World::new()?;
    let (script, log, ready) = stubborn(&world)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&script, "logs/A1")?;
    turn.escalation = quick();
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &attempt, &turn, &cancel);
    // The group is sampled by 3 s after the cancel (AC-025-03), zombies included.
    let sample = cancel_and_sample(&world, "A1", &cancel, Duration::from_secs(3), &ready);
    let (outcome, sample) = tokio::join!(running, sample);
    let (outcome, (at, pid, empty, left)) = (outcome?, sample?);
    assert!(
        empty,
        "the group is not empty 3 s after the cancel: {left:?}"
    );
    assert!(at.elapsed() >= Duration::from_millis(2900));
    assert_eq!(outcome.state, AttemptState::Cancelled);
    assert_eq!(outcome.pid, Some(pid));
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
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );
    Ok(())
}

/// The 3 s sample of TEST-025-03 catches a slower stop: with graces of 2 s each the group is still there at 3 s.
#[tokio::test]
async fn the_three_second_sample_catches_a_slow_stop() -> R {
    let world = World::new()?;
    let (script, _, ready) = stubborn(&world)?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&script, "logs/A1")?;
    turn.escalation = Escalation {
        native_grace: Duration::from_secs(2),
        term_grace: Duration::from_secs(2),
        ..quick()
    };
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &attempt, &turn, &cancel);
    let sample = cancel_and_sample(&world, "A1", &cancel, Duration::from_secs(3), &ready);
    let (outcome, sample) = tokio::join!(running, sample);
    let (outcome, (_, pid, empty, left)) = (outcome?, sample?);
    assert!(!empty, "the group was gone before its kill: {left:?}");
    assert_eq!(outcome.state, AttemptState::Cancelled);
    assert!(group_empty(pid));
    Ok(())
}

const INIT: &str = r#"{"type":"system","subtype":"init","session_id":"sid-1","model":"m"}"#;
const SUCCESS: &str =
    r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"sid-1"}"#;

/// Short graces for the tests that stop a fake.
fn quick() -> Escalation {
    Escalation {
        native: Some(Signal::Interrupt),
        native_grace: Duration::from_secs(1),
        term_grace: Duration::from_secs(1),
        ..Turn::default_escalation()
    }
}

/// Wait until attempt `id` is RUNNING; its pid.
async fn running_pid(world: &World, id: &str) -> R<u32> {
    for _ in 0..200 {
        if let Some(a) = world.store.reader()?.attempt(id)?
            && a.state == AttemptState::Running
        {
            // What it recorded names its live process (REQ-041 b3, ADR-019 decision 4).
            let found = hoh_agents::identity::find(
                a.pid,
                a.boot_id.as_deref(),
                a.process_start_id.as_deref(),
            )?;
            assert!(found.is_ok(), "{id}: {found:?}");
            return Ok(u32::try_from(a.pid.ok_or("no pid")?)?);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(format!("{id} never ran").into())
}

/// Another connection holding the store's write lock for `hold` (Python's sqlite3, test code); `ready` is created once
/// the lock is held.
fn hold_store(world: &World, ready: &Path, hold: Duration) -> R<std::process::Child> {
    Ok(std::process::Command::new("python3")
        .args([
            "-c",
            "import pathlib,sqlite3,sys,time; c=sqlite3.connect(sys.argv[1], isolation_level=None); \
             c.execute('BEGIN IMMEDIATE'); pathlib.Path(sys.argv[2]).touch(); time.sleep(float(sys.argv[3])); \
             c.execute('COMMIT')",
        ])
        .arg(world.dir.path().join(".hoh/state.db"))
        .arg(ready)
        .arg(hold.as_secs_f64().to_string())
        .spawn()?)
}

/// A child that never reads its prompt neither holds up a cancel nor gets a success (M1T03R-02).
#[tokio::test]
async fn a_prompt_that_is_never_read_does_not_hold_up_cancel_or_count_as_delivered() -> R {
    let world = World::new()?;
    let stuck = world.script(
        "stuck",
        &format!("echo '{INIT}'\nwhile true; do sleep 0.1; done"),
    )?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&stuck, "logs/stuck")?;
    turn.prompt = vec![b'p'; 2 << 20];
    turn.escalation = quick();
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &attempt, &turn, &cancel);
    let trigger = async {
        running_pid(&world, "A1").await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(cancel.request(), CancelReceipt::Accepted);
        Ok::<Instant, Box<dyn std::error::Error>>(Instant::now())
    };
    let (outcome, at) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(running, trigger)
    })
    .await?;
    let (outcome, at) = (outcome?, at?);
    assert!(at.elapsed() <= Duration::from_secs(3), "{:?}", at.elapsed());
    assert_eq!(outcome.state, AttemptState::Cancelled);
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );

    // The same child printing a success and exiting: its prompt was not delivered, so it is no success.
    let quitter = world.script(
        "quitter",
        &format!("echo '{INIT}'\necho '{SUCCESS}'\nexit 0"),
    )?;
    let attempt = world.attempt("A2").await?;
    let mut turn = world.turn(&quitter, "logs/quitter")?;
    turn.prompt = vec![b'p'; 2 << 20];
    let outcome = world.run(&attempt, &turn).await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("PROMPT_NOT_DELIVERED"))
    );
    Ok(())
}

/// A cancel that arrives while the adapter is busy, with the result and the exit already queued, still cancels
/// (M1T03R-03): the store is held by another connection while the fake prints its result and exits.
#[tokio::test]
async fn a_cancel_queued_with_the_exit_still_cancels() -> R {
    let world = World::new()?;
    let go = world.dir.path().join("go");
    let fake = world.script(
        "racer",
        &format!(
            "cat > /dev/null\nwhile [ ! -e '{}' ]; do sleep 0.02; done\necho '{INIT}'\necho '{SUCCESS}'\nexit 0",
            go.display()
        ),
    )?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/racer")?;
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &attempt, &turn, &cancel);
    let trigger = async {
        let pid = running_pid(&world, "A1").await?;
        // Hold the database while the fake speaks and exits; the adapter blocks on the init frame's write.
        let mut holder = hold_store(&world, &go, Duration::from_millis(1500))?;
        for _ in 0..200 {
            if hoh_agents::identity::reaped(pid)? {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(cancel.request(), CancelReceipt::Accepted);
        let held = holder.wait()?;
        Ok::<bool, Box<dyn std::error::Error>>(held.success())
    };
    let (outcome, held) = tokio::join!(running, trigger);
    assert!(held?, "the store was not held");
    let outcome = outcome?;
    assert_eq!(outcome.state, AttemptState::Cancelled);
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );
    Ok(())
}

/// A refused frame ends the turn instead of waiting for the child (M1T03R-08); no deadline is set.
#[tokio::test]
async fn a_frame_too_large_ends_the_turn() -> R {
    let world = World::new()?;
    let fake = world.script(
        "huge",
        &format!(
            "cat > /dev/null\necho '{INIT}'\nhead -c 1048578 /dev/zero | tr '\\0' a\necho\nwhile true; do sleep 0.1; done"
        ),
    )?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/huge")?;
    turn.escalation = quick();
    turn.deadline = None;
    let started = Instant::now();
    let outcome =
        tokio::time::timeout(Duration::from_secs(10), world.run(&attempt, &turn)).await??;
    assert!(
        started.elapsed() <= Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("PROTOCOL_FRAME_TOO_LARGE"))
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (
            row.state,
            row.failure_reason.as_deref(),
            row.cancel_requested
        ),
        (
            AttemptState::Failed,
            Some("PROTOCOL_FRAME_TOO_LARGE"),
            false
        )
    );
    assert!(group_empty(outcome.pid.ok_or("no pid")?));
    let diagnosis = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .find(|(k, p)| k == "adapter.diagnostic" && p["reason"] == "PROTOCOL_FRAME_TOO_LARGE")
        .map(|(_, p)| p)
        .ok_or("no diagnosis")?;
    assert!(diagnosis["length"].as_u64().is_some_and(|n| n > 1 << 20));
    assert_eq!(diagnosis["prefix_sha256"].as_str().map(str::len), Some(64));
    assert_eq!(diagnosis["head"].as_str().map(str::len), Some(4 << 10));
    Ok(())
}

/// The deadline is a failure even if the interrupted child then reports success (M1T03R-05).
#[tokio::test]
async fn a_deadline_is_a_failure_even_after_a_success_frame() -> R {
    let world = World::new()?;
    let fake = world.script(
        "slow",
        &format!(
            "on_int() {{ echo '{SUCCESS}'; exit 0; }}\ntrap on_int INT\ncat > /dev/null\necho '{INIT}'\nwhile true; do sleep 0.05; done"
        ),
    )?;
    let attempt = world.attempt("A1").await?;
    let mut turn = world.turn(&fake, "logs/slow")?;
    turn.escalation = quick();
    turn.deadline = Some(Duration::from_millis(250));
    let outcome = world.run(&attempt, &turn).await?;
    assert_eq!(
        outcome.steps.first().map(|s| s.signal),
        Some(Signal::Interrupt)
    );
    assert_eq!(
        outcome.result.as_ref().and_then(|r| r.subtype.as_deref()),
        Some("success"),
        "the fake answered the interrupt with a success"
    );
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("DEADLINE_EXCEEDED"))
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (
            row.state,
            row.failure_reason.as_deref(),
            row.cancel_requested
        ),
        (AttemptState::Failed, Some("DEADLINE_EXCEEDED"), false)
    );
    Ok(())
}

/// A turn whose log directory cannot be made ends FAILED in the store rather than staying PREPARED (M1T03R-09).
#[tokio::test]
async fn a_log_directory_that_cannot_be_made_fails_the_attempt() -> R {
    let world = World::new()?;
    let fake = world.fake("plain", SUCCESS.as_bytes(), b"", 0)?;
    let attempt = world.attempt("A1").await?;
    let blocker = world.dir.path().join("not-a-directory");
    std::fs::write(&blocker, b"")?;
    let mut turn = world.turn(&fake, "unused")?;
    turn.logs = blocker.join("logs");
    assert!(world.run(&attempt, &turn).await.is_err());
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(row.state, AttemptState::Failed);
    assert!(
        row.failure_reason
            .as_deref()
            .is_some_and(|r| r.starts_with("LOGS:")),
        "{:?}",
        row.failure_reason
    );
    assert!(row.ended_at.is_some());
    assert!(argvs(&fake)?.is_empty(), "no process may start");
    Ok(())
}

/// The recorded not-logged-in run is BLOCKED_AUTH, with the result frame's own subtype kept as an event (REQ-025).
#[tokio::test]
async fn the_recorded_login_failure_is_blocked_auth() -> R {
    let world = World::new()?;
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(format!(
        "{FIXTURES}/unauthenticated/meta.json"
    ))?)?;
    let exit = i32::try_from(meta["exit_code"].as_i64().ok_or("no exit code")?)?;
    let fake = world.fake("unauthenticated", &fixture("unauthenticated")?, b"", exit)?;
    let attempt = world.attempt("A1").await?;
    let outcome = world
        .run(&attempt, &world.turn(&fake, "logs/unauthenticated")?)
        .await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("BLOCKED_AUTH"))
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(row.failure_reason.as_deref(), Some("BLOCKED_AUTH"));
    let results: Vec<serde_json::Value> = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .filter(|(k, _)| k == "adapter.result")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(results.len(), 1);
    assert_eq!(
        (&results[0]["subtype"], &results[0]["is_error"]),
        (&serde_json::json!("success"), &serde_json::json!(true))
    );
    // The same failure without Claude's marker stays an ordinary failure.
    let unmarked: Vec<u8> = fixture("unauthenticated")?
        .split_inclusive(|b| *b == b'\n')
        .flat_map(|l| {
            let mut f: serde_json::Value = serde_json::from_slice(l).unwrap_or_default();
            if let Some(o) = f.as_object_mut() {
                o.remove("error");
            }
            let mut v = serde_json::to_vec(&f).unwrap_or_default();
            v.push(b'\n');
            v
        })
        .collect();
    let fake = world.fake("unmarked", &unmarked, b"", exit)?;
    let attempt = world.attempt("A2").await?;
    let outcome = world
        .run(&attempt, &world.turn(&fake, "logs/unmarked")?)
        .await?;
    assert_eq!(
        (outcome.state, outcome.failure.as_deref()),
        (AttemptState::Failed, Some("is_error"))
    );
    Ok(())
}

/// A cancel accepted while the end is being committed (the store held by another connection) still ends the attempt
/// CANCELLED; once the end is committed a cancel is refused (M1T03R-10).
#[tokio::test]
async fn a_cancel_while_the_end_is_committed_wins() -> R {
    let world = World::new()?;
    let go = world.dir.path().join("go");
    let fake = world.script(
        "ender",
        &format!(
            "cat > /dev/null\necho '{INIT}'\necho '{SUCCESS}'\nwhile [ ! -e '{}' ]; do sleep 0.02; done\nexit 0",
            go.display()
        ),
    )?;
    let attempt = world.attempt("A1").await?;
    let turn = world.turn(&fake, "logs/ender")?;
    let cancel = CancelLatch::new();
    let running = claude::run(&world.store, &attempt, &turn, &cancel);
    let trigger = async {
        // The result is recorded; then the store is held, the fake exits and the adapter commits its end.
        for _ in 0..400 {
            if world
                .store
                .reader()?
                .events("R")?
                .iter()
                .any(|(k, _)| k == "adapter.result")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let mut holder = hold_store(&world, &go, Duration::from_millis(1500))?;
        while !go.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        let receipt = cancel.request();
        let held = holder.wait()?;
        Ok::<(CancelReceipt, bool), Box<dyn std::error::Error>>((receipt, held.success()))
    };
    let (outcome, trigger) = tokio::join!(running, trigger);
    let (outcome, (receipt, held)) = (outcome?, trigger?);
    assert!(held, "the store was not held");
    assert_eq!(receipt, CancelReceipt::Accepted);
    assert_eq!(
        (outcome.state, outcome.failure),
        (AttemptState::Cancelled, None)
    );
    let row = world.store.reader()?.attempt("A1")?.ok_or("no attempt")?;
    assert_eq!(
        (row.state, row.cancel_requested),
        (AttemptState::Cancelled, true)
    );
    assert_eq!(cancel.request(), CancelReceipt::AlreadyEnded);
    Ok(())
}

/// TEST-027-02 (claude): a recorded turn with one record whose type is not in 2.1.318's known-frame registry
/// (`tests/fixtures/protocol/claude/2.1.318/known-frames.json`) and an `x_future` field in a known record, against the
/// same turn without that record. Expected: no panic; exactly one `adapter.unknown_event`, with the record as received
/// and the sha256 of its bytes; the raw log is the bytes the fake wrote; `x_future` comes back from the raw log as
/// sent; both turns end alike, as returned and as stored.
#[tokio::test]
async fn test_027_02_claude_keeps_an_unregistered_record_once_and_ends_as_without_it() -> R {
    let world = World::new()?;
    let recorded = fixture("success")?;
    let mut lines: Vec<Vec<u8>> = recorded
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    let at = lines
        .iter()
        .position(|l| {
            serde_json::from_slice::<serde_json::Value>(l).is_ok_and(|f| f["type"] == "assistant")
        })
        .ok_or("no assistant record")?;
    let mut known: serde_json::Value = serde_json::from_slice(&lines[at])?;
    known["x_future"] = serde_json::json!({"nested": [1, "二"], "flag": true});
    lines[at] = serde_json::to_vec(&known)?;
    let join = |lines: &[Vec<u8>]| -> Vec<u8> {
        lines
            .iter()
            .flat_map(|l| [l.as_slice(), b"\n"].concat())
            .collect()
    };
    let control = join(&lines);
    let unknown = br#"{"type":"hoh_future_record","detail":{"n":1}}"#.to_vec();
    lines.insert(at, unknown.clone());
    let with = join(&lines);
    let mut ends = Vec::new();
    for (id, name, stdout) in [("A1", "with", &with), ("A2", "control", &control)] {
        let attempt = world.attempt(id).await?;
        let fake = world.fake(name, stdout, b"", 0)?;
        let outcome = world
            .run(&attempt, &world.turn(&fake, &format!("logs/{name}"))?)
            .await?;
        let row = world.store.reader()?.attempt(id)?.ok_or("no attempt")?;
        ends.push((
            (outcome.state, outcome.failure),
            (row.state, row.failure_reason),
        ));
        let raw = std::fs::read(world.dir.path().join(format!("logs/{name}/stdout.jsonl")))?;
        assert_eq!(
            hex(&raw),
            hex(stdout),
            "{name}: the raw log is what the fake wrote"
        );
        let kept = raw
            .split(|b| *b == b'\n')
            .filter_map(|l| serde_json::from_slice::<serde_json::Value>(l).ok())
            .find_map(|f| f.get("x_future").cloned());
        assert_eq!(
            kept.as_ref(),
            Some(&known["x_future"]),
            "{name}: x_future in the raw log"
        );
    }
    let unknowns: Vec<serde_json::Value> = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .filter(|(k, _)| k == "adapter.unknown_event")
        .map(|(_, p)| p)
        .collect();
    assert_eq!(unknowns.len(), 1, "{unknowns:?}");
    assert_eq!(unknowns[0]["attempt_id"], "A1");
    assert_eq!(unknowns[0]["key"], "hoh_future_record");
    assert_eq!(unknowns[0]["frame_sha256"], hex(&unknown));
    assert_eq!(
        unknowns[0]["frame"],
        serde_json::from_slice::<serde_json::Value>(&unknown)?
    );
    assert_eq!(
        ends[0], ends[1],
        "the same end with and without the unknown record"
    );
    Ok(())
}

/// AC-027-03 at the adapter level (CR-008: the in-host test with its TEST id comes with M2-T07; claude): after its normal frames a turn writes one 8 MiB line with no LF while another attempt runs a
/// normal turn beside it on the same store. The reader never holds more than the limit and one read chunk; only the
/// oversized attempt fails (PROTOCOL_FRAME_TOO_LARGE, its diagnosis with the length, the prefix sha256 and the first
/// 4 KiB); the other settles, and the store's writer goes on (the process that runs both, ADR-019 decision 9).
#[tokio::test]
async fn claude_an_8_mib_line_fails_only_its_own_attempt() -> R {
    let world = World::new()?;
    let huge = world.script(
        "huge",
        &format!(
            "cat > /dev/null\necho '{INIT}'\nhead -c 8388608 /dev/zero | tr '\\0' a\nwhile true; do sleep 0.1; done"
        ),
    )?;
    let normal = world.fake("normal", &fixture("tool-success")?, b"", 0)?;
    let oversized = world.attempt_in("claude", "A1", "S1", "2.1.318").await?;
    let beside = world.attempt_in("claude", "A2", "S2", "2.1.318").await?;
    let mut big = world.turn(&huge, "logs/huge")?;
    big.escalation = quick();
    big.deadline = None;
    let small = world.turn(&normal, "logs/normal")?;
    let (big, small) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(world.run(&oversized, &big), world.run(&beside, &small))
    })
    .await?;
    let (big, small) = (big?, small?);
    let limit = 1_048_576 + 65_536;
    assert!(
        big.max_buffered.is_some_and(|n| n <= limit),
        "{:?}",
        big.max_buffered
    );
    assert_eq!(
        (big.state, big.failure.as_deref()),
        (AttemptState::Failed, Some("PROTOCOL_FRAME_TOO_LARGE"))
    );
    let diagnosis = world
        .store
        .reader()?
        .events("R")?
        .into_iter()
        .find(|(k, p)| k == "adapter.diagnostic" && p["reason"] == "PROTOCOL_FRAME_TOO_LARGE")
        .map(|(_, p)| p)
        .ok_or("no diagnosis")?;
    // The reader filled up to the limit before it refused the line, and kept no more than one read beyond it; the
    // diagnosis is of those bytes (M2T05R-59).
    let length = usize::try_from(diagnosis["length"].as_u64().ok_or("no length")?)?;
    assert!(length > 1 << 20 && length <= limit, "{length}");
    assert!(
        big.max_buffered.is_some_and(|n| n >= length),
        "{:?}",
        big.max_buffered
    );
    let digest = <sha2::Sha256 as sha2::Digest>::digest(vec![b'a'; length]);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(diagnosis["prefix_sha256"], serde_json::json!(hex));
    assert_eq!(diagnosis["head"], serde_json::json!("a".repeat(4096)));
    assert_eq!(
        (small.state, small.failure.as_deref()),
        (AttemptState::Settled, None)
    );
    let rows = world.store.reader()?;
    assert_eq!(
        rows.attempt("A2")?.map(|a| a.state),
        Some(AttemptState::Settled)
    );
    assert!(world.store.writer_alive());
    Ok(())
}

/// The number `sql` counts in the world's store (Python's sqlite3, test code).
fn count(world: &World, sql: &str) -> R<i64> {
    let out = std::process::Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute(sys.argv[2]).fetchone()[0])",
        ])
        .arg(world.dir.path().join(".hoh/state.db"))
        .arg(sql)
        .output()?;
    Ok(String::from_utf8(out.stdout)?.trim().parse()?)
}

/// A start gate for the tests (the product's is `hoh __gate`): one line from stdin, read a byte at a time; `go` execs
/// the agent's argv in its place, anything else exits 0 (docs/adr/ADR-022.md decision 13).
fn gate(world: &World, marker: &str) -> R<claude::GateCommand> {
    let script = world.dir.path().join("gate.py");
    std::fs::write(
        &script,
        "import os, sys\nline = b''\nwhile True:\n    b = os.read(0, 1)\n    if not b or b == b'\\n':\n        break\n    line += b\n\
         if line != b'go':\n    sys.exit(0)\nos.execvp(sys.argv[1], sys.argv[1:])\n",
    )?;
    Ok(claude::GateCommand {
        program: "python3".into(),
        args: vec![script.into()],
        env: vec![("HOH_ATTEMPT_ID".into(), marker.into())],
    })
}

/// A fake `claude` that records its argv, its stdin and its marker, then prints `stdout`.
fn recording(world: &World, name: &str, stdout: &str) -> R<PathBuf> {
    let d = world.dir.path().join(name);
    world.script(
        name,
        &format!(
            "d='{}'\nprintf '%s\\n' \"$@\" > \"$d/argv\"\nprintf '%s' \"$HOH_ATTEMPT_ID\" > \"$d/marker\"\n\
             cat > \"$d/stdin\"\nprintf '%s\\n' '{}'\n",
            d.display(),
            stdout.replace('\'', "'\\''")
        ),
    )
}

#[tokio::test]
async fn claude_runner_mode_runs_nothing_before_its_gate_and_reports_facts_without_store_writes()
-> R {
    // docs/adr/ADR-022.md decisions 12 to 14: started held at its gate, nothing of the agent runs; opened, the agent
    // gets its argv, the marker and the prompt (not the gate's line); its facts come back, the store untouched.
    let world = World::new()?;
    let program = recording(&world, "held", &format!("{INIT}\n{SUCCESS}"))?;
    let mut turn = world.turn(&program, "logs-held")?;
    turn.deadline = Some(Duration::from_millis(1));
    let attempt = world.attempt("A1").await?;
    let events = count(&world, "SELECT count(*) FROM events")?;
    let held = claude::start(&attempt, &turn, &gate(&world, "p/A1")?)?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let d = world.dir.path().join("held");
    assert!(
        !d.join("argv").exists(),
        "the agent ran before its gate opened"
    );
    held.open.send(()).map_err(|()| "gate closed")?;
    let facts = tokio::time::timeout(Duration::from_secs(20), held.ended).await??;
    assert_eq!(facts.state, AttemptState::Settled, "{facts:?}");
    assert_eq!(facts.failure, None);
    assert_eq!(facts.native_session.as_deref(), Some("sid-1"));
    assert_eq!(facts.resolved_model.as_deref(), Some("m"));
    assert!(facts.group_reaped, "{facts:?}");
    assert_eq!(
        facts.raw_log_path,
        world.dir.path().join("logs-held").to_string_lossy()
    );
    assert!(
        facts
            .recorded
            .contains(&claude::Recorded::NativeSession("sid-1".to_owned()))
    );
    assert_eq!(std::fs::read(d.join("stdin"))?, turn.prompt);
    assert_eq!(std::fs::read_to_string(d.join("marker"))?, "p/A1");
    assert!(
        std::fs::read_to_string(d.join("argv"))?.starts_with("-p\n--output-format\nstream-json\n")
    );
    assert_eq!(
        count(&world, "SELECT count(*) FROM events")?,
        events,
        "the store was written"
    );
    Ok(())
}

#[tokio::test]
async fn claude_runner_mode_keeps_the_agents_own_cost_estimate_in_the_usage() -> R {
    // docs/adr/ADR-022.md decision 14 (M2T07R-256): the cost Claude reports is its own estimate, kept in the usage as
    // reported_cost_usd, never the run's cost (ADR-020).
    let world = World::new()?;
    let result = r#"{"type":"result","subtype":"success","is_error":false,"result":"ok","session_id":"sid-1","usage":{"input_tokens":3},"total_cost_usd":0.25}"#;
    let program = recording(&world, "costed", &format!("{INIT}\n{result}"))?;
    let turn = world.turn(&program, "logs-costed")?;
    let attempt = world.attempt("A1").await?;
    let held = claude::start(&attempt, &turn, &gate(&world, "p/A1")?)?;
    held.open.send(()).map_err(|()| "gate closed")?;
    let facts = tokio::time::timeout(Duration::from_secs(20), held.ended).await??;
    assert_eq!(facts.state, AttemptState::Settled, "{facts:?}");
    assert_eq!(
        facts.usage,
        Some(serde_json::json!({"input_tokens": 3, "reported_cost_usd": 0.25}))
    );
    Ok(())
}

#[tokio::test]
async fn claude_runner_mode_gate_dropped_unopened_never_runs_the_agent() -> R {
    // ADR-019 decision 8, ADR-022 decision 13: a gate that is never opened ends, and the agent never runs.
    let world = World::new()?;
    let program = recording(&world, "dropped", SUCCESS)?;
    let turn = world.turn(&program, "logs-dropped")?;
    let held = claude::start(&world.attempt("A1").await?, &turn, &gate(&world, "p/A1")?)?;
    drop(held.open);
    let facts = tokio::time::timeout(Duration::from_secs(20), held.ended).await??;
    assert_eq!(facts.state, AttemptState::Failed);
    assert_eq!(facts.failure.as_deref(), Some("GATE_CLOSED"));
    assert!(
        !world.dir.path().join("dropped/argv").exists(),
        "the agent ran"
    );
    Ok(())
}

#[tokio::test]
async fn claude_runner_mode_cancel_is_one_sigint_and_nothing_after_it() -> R {
    // ADR-022 decision 14: the host's ladder is authoritative; the adapter's cancel is Claude's native one, SIGINT to
    // the leader, with no SIGTERM or SIGKILL of its own after it.
    let world = World::new()?;
    let d = world.dir.path().join("stubborn");
    // The stand-in blocks SIGINT and SIGTERM before it says it is ready and takes them with sigtimedwait, writing each
    // with its sender: a blocked signal stays pending, so none can be lost in a gap of a shell's own handling (the
    // bash stand-in this replaces lost one in CI, run 42775489697; ADR-022 decision 10, convention v2).
    let program = world.script(
        "stubborn",
        &format!(
            "exec python3 -c '\
import os, signal, sys, time
d = sys.argv[1]
signal.pthread_sigmask(signal.SIG_BLOCK, {{signal.SIGINT, signal.SIGTERM}})
sys.stdin.read()
open(os.path.join(d, \"ready\"), \"w\").write(\"ready\\n\")
end = time.monotonic() + 30
while end > time.monotonic():
    info = signal.sigtimedwait({{signal.SIGINT, signal.SIGTERM}}, end - time.monotonic())
    if info is None:
        break
    name = \"int\" if info.si_signo == signal.SIGINT else \"term\"
    with open(os.path.join(d, \"signals\"), \"a\") as f:
        f.write(name + \" \" + str(info.si_pid) + \"\\n\")
    if info.si_signo == signal.SIGTERM:
        sys.exit(0)
' '{}'",
            d.display()
        ),
    )?;
    let mut turn = world.turn(&program, "logs-stubborn")?;
    turn.escalation = quick();
    let held = claude::start(&world.attempt("A1").await?, &turn, &gate(&world, "p/A1")?)?;
    held.open.send(()).map_err(|()| "gate closed")?;
    let end = Instant::now() + Duration::from_secs(10);
    while !d.join("ready").exists() {
        assert!(Instant::now() < end, "the agent did not start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    held.cancel.notify_one();
    let cancelled = Instant::now();
    // The SIGINT's record, awaited until 3 s after the cancel, the bound the test always had (CIR-053); the
    // stand-in waits 30 s for it, so a missing signal finds it still there to show its status.
    let signals = d.join("signals");
    let end = cancelled + Duration::from_secs(3);
    while std::fs::read_to_string(&signals).map_or(true, |s| s.is_empty()) {
        if Instant::now() > end {
            let status =
                std::fs::read_to_string(format!("/proc/{}/status", held.pid)).unwrap_or_default();
            if let Some(group) = rustix::process::Pid::from_raw(held.pid as i32) {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            return Err(format!(
                "no signal recorded 3 s after the cancel; the agent's status:\n{status}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // Longer than the turn's own graces (1 s and 1 s) from the cancel: nothing but the SIGINT came.
    tokio::time::sleep_until((cancelled + Duration::from_millis(3000)).into()).await;
    let signals = std::fs::read_to_string(&signals)?;
    let alive = !held.ended.is_finished();
    let _ = rustix::process::kill_process_group(
        rustix::process::Pid::from_raw(held.pid as i32).ok_or("pid")?,
        rustix::process::Signal::KILL,
    );
    let facts = tokio::time::timeout(Duration::from_secs(20), held.ended).await??;
    assert_eq!(
        signals,
        format!("int {}\n", std::process::id()),
        "{facts:?}"
    );
    assert!(alive, "the adapter ended the turn itself");
    assert_ne!(facts.state, AttemptState::Settled);
    Ok(())
}
