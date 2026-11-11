//! The recovery check (REQ-036, REQ-041; docs/adr/ADR-019.md): kill -9 matrices and the reconcile at a host's start.
//! Each host runs in a process of its own (this test binary's `recovery_host` entry); the faults are made by the
//! test's hook implementations, passed in process (CR-006 F2/F3): the product's hooks do nothing.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig};
use hoh_core::decision::{Context, Proposal};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{Commit, FaultHooks, NewEvent, WritePoint};
use hoh_core::protocol::Message;
use serde_json::{Value, json};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

mod installed;

const CHILD_PROJECT: &str = "HOH_TEST_RECOVERY_PROJECT";
/// `<job>:<point>`: the child kills itself when its writer reaches that point of such a request.
const CHILD_KILL: &str = "HOH_TEST_RECOVERY_KILL";
/// A decision Message (JSON) the child submits once its host has started.
const CHILD_DECIDE: &str = "HOH_TEST_RECOVERY_DECIDE";
/// The reconcile's stop graces in milliseconds (SIGTERM to SIGKILL, SIGKILL to gone).
const CHILD_GRACE_MS: &str = "HOH_TEST_RECOVERY_GRACE_MS";
/// A JSON file `{"available": {marker: ref}, "unavailable": [marker]}`: the FakeRemote the reconcile looks in.
const CHILD_REMOTE: &str = "HOH_TEST_RECOVERY_REMOTE";
/// A directory with the FakeAgent's counter file and markers: the child runs a runner with the FakeExecutor and looks
/// for work effects by their markers.
const CHILD_WORK: &str = "HOH_TEST_RECOVERY_WORK";
/// A runner point: the child kills itself when an attempt reaches it.
const CHILD_RUNNER_KILL: &str = "HOH_TEST_RECOVERY_RUNNER_KILL";
/// The child runs a runner whose every attempt fails with this class: `auth`, or `retry_after:<seconds>` (a 429).
const CHILD_FAIL_CLASS: &str = "HOH_TEST_RECOVERY_FAIL_CLASS";

/// A file holding the child host's time in milliseconds since the epoch, read at each look: its clock (CR-006 F2: a
/// clock test code injects; the product uses `MonotonicClock`, ADR-020 decision 4).
const CHILD_CLOCK: &str = "HOH_TEST_RECOVERY_CLOCK";

/// A directory of fake agent CLIs: the child runs a runner whose executor has the product's registry over that PATH
/// alone (its binder grants sessions) and records each first input it is given (docs/adr/ADR-024.md decision 4).
const CHILD_AGENTS: &str = "HOH_TEST_RECOVERY_AGENTS";
/// The child kills itself at the commit of the checkpoint that handed the commander over (its session.start notice
/// written), before the binder can give the new Commander its session (AC-019-05).
const CHILD_KILL_AT_HANDOVER: &str = "HOH_TEST_RECOVERY_KILL_AT_HANDOVER";
/// A file: once its host serves, the child sends the old Codex Commander's three epoch-1 messages (two decisions, a
/// steer) and writes what each got, one line each.
const CHILD_OLD_COMMANDER: &str = "HOH_TEST_RECOVERY_OLD_COMMANDER";

/// The child's exit code when it submitted its decision and was not killed.
const NOT_KILLED: i32 = 77;

/// The product binary's WAL self-test probe.
fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config
}

/// Ends its own process with SIGKILL when the writer reaches `point` of a `job` request (CR-006 F2: a SIGKILL the
/// process sends itself, made by test code).
#[derive(Debug)]
struct KillAt {
    job: String,
    point: WritePoint,
}

impl FaultHooks for KillAt {
    fn at(&self, job: &'static str, point: WritePoint) {
        if job == self.job && point == self.point {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            // SIGKILL cannot be caught; nothing after it runs.
            loop {
                std::thread::park();
            }
        }
    }
}

/// A local fake forge (AC-041-05): pull requests by run marker (the effect key), some markers unreachable. Every query
/// is appended to `<file>.calls` as `query <marker>`. The reconcile is given only this read-only port (an `Observer` has
/// no way to create anything), so its never creating a pull request holds by construction (M2T05R-56).
#[derive(Debug)]
struct FakeRemote(PathBuf);

impl FakeRemote {
    fn log(&self, line: &str) {
        use std::io::Write as _;
        let mut calls = self.0.clone().into_os_string();
        calls.push(".calls");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(calls)
        {
            let _ = writeln!(file, "{line}");
        }
    }
}

impl hoh_core::reconcile::Observer for FakeRemote {
    fn observe(&self, _attempt_id: &str, marker: &str) -> hoh_core::reconcile::Observation {
        use hoh_core::reconcile::Observation;
        self.log(&format!("query {marker}"));
        let remote: Value = std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        if remote["unavailable"]
            .as_array()
            .is_some_and(|m| m.iter().any(|x| x == marker))
        {
            return Observation::Unavailable {
                detail: "the forge did not answer".to_owned(),
            };
        }
        match remote["available"][marker].as_str() {
            Some(pr) => Observation::Found {
                external_ref: pr.to_owned(),
            },
            None => Observation::Absent,
        }
    }
}

/// The FakeAgent (AC-041-01): waits at its start gate (a line `go` on stdin; anything else, EOF included, ends it
/// without running), then appends its effect key to the counter file when it starts executing, works a moment, and
/// leaves its marker (the key, `/` as `_`) when it is done.
/// With a `fail-once` file in its directory, each task's first run fails (exit 1) after it counted itself.
const FAKE_AGENT: &str = "read -r go; [ \"$go\" = go ] || exit 0; printf '%s\\n' \"$1\" >> \"$2/counter\"; \
                          if [ -e \"$2/fail-once\" ] && [ ! -e \"$2/failed-$4\" ]; then : > \"$2/failed-$4\"; exit 1; fi; \
                          sleep 0.2; : > \"$2/markers/$3\"";

/// The start gate of a child whose stdin is piped: `go` opens it; dropped unopened, the pipe closes and the child ends.
fn gate(child: &mut std::process::Child) -> hoh_app::host::Gate {
    let stdin = child.stdin.take();
    hoh_app::host::Gate::new(move || {
        use std::io::Write as _;
        let mut stdin = stdin.ok_or("no stdin")?;
        stdin.write_all(b"go\n").map_err(|e| e.to_string())
    })
}

fn marker_name(key: &str) -> String {
    key.replace('/', "_")
}

/// Runs each attempt's work as a FakeAgent, the leader of its own process group (as the substrate starts agents).
#[derive(Debug)]
struct FakeExecutor(PathBuf);

impl hoh_app::host::Executor for FakeExecutor {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sh")
            .args(["-c", FAKE_AGENT, "fake-agent", &work.effect_key])
            .arg(&self.0)
            .arg(marker_name(&work.effect_key))
            .arg(&work.task_id)
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let gate = gate(&mut child);
        let ended = reaping(child);
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }
}

/// The executor's end of an attempt's process, as `Executor::start` requires (the hoh-agents substrate's order,
/// M2T05R-303): the leader's exit seen without reaping it, a SIGKILL to its group while the zombie holds the group's
/// number, the leader reaped, then up to 5 s for the group to be empty (`kill(-pgid, 0)`); `group_gone` says whether
/// it was.
fn reaping(child: std::process::Child) -> tokio::sync::oneshot::Receiver<hoh_app::host::Outcome> {
    reaping_after(child, Duration::ZERO)
}

/// [`reaping`] by a slow executor: `delay` between the leader's exit and the SIGKILL to its group.
fn reaping_after(
    child: std::process::Child,
    delay: Duration,
) -> tokio::sync::oneshot::Receiver<hoh_app::host::Outcome> {
    reaping_with(child, delay, None)
}

/// [`reaping_after`], the outcome a failure of `failed`'s class when it is given.
fn reaping_with(
    mut child: std::process::Child,
    delay: Duration,
    failed: Option<hoh_core::retry::FailureClass>,
) -> tokio::sync::oneshot::Receiver<hoh_app::host::Outcome> {
    let (end, ended) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let pid = child.id();
        if let Some(raw) = i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::waitid(
                rustix::process::WaitId::Pid(raw),
                rustix::process::WaitIdOptions::EXITED | rustix::process::WaitIdOptions::NOWAIT,
            );
        }
        std::thread::sleep(delay);
        kill_group(pid);
        let status = child.wait();
        let until = Instant::now() + Duration::from_secs(5);
        let mut group_gone = hoh_agents::identity::group_empty(pid);
        while !group_gone && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
            group_gone = hoh_agents::identity::group_empty(pid);
        }
        let _ = end.send(hoh_app::host::Outcome {
            success: failed.is_none()
                && status.as_ref().is_ok_and(std::process::ExitStatus::success),
            detail: format!("{status:?}"),
            group_gone,
            class: failed.unwrap_or(hoh_core::retry::FailureClass::Transient { retry_after: None }),
            observed: Default::default(),
            unknown_effect: false,
        });
    });
    ended
}

/// Runs each attempt's work as a short process at its gate whose outcome is a failure of `.0`'s class (a 429 with
/// its Retry-After, a missing authentication).
#[derive(Debug)]
struct FailingExecutor(hoh_core::retry::FailureClass);

impl hoh_app::host::Executor for FailingExecutor {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sh")
            .args(["-c", "read -r go; exit 3"])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let gate = gate(&mut child);
        let ended = reaping_with(child, Duration::ZERO, Some(self.0));
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }
}

/// What is in process group `pgid` now, leader included, as the evidence records it (M2T05R-360): a whole scan of
/// /proc (whose failure is shown, never read as empty) and the kernel's look-up of the group.
fn group_now(pgid: u32) -> Value {
    json!({
        "left": hoh_agents::identity::group_left(pgid, 0).map_err(|e| e.to_string()),
        "empty": hoh_agents::identity::group_empty(pgid),
    })
}

/// [`group_now`] of a group with nothing left in it.
fn nothing_left() -> Value {
    json!({"left": {"Ok": []}, "empty": true})
}

/// Looks for an attempt's work by the FakeAgent's marker.
#[derive(Debug)]
struct Markers(PathBuf);

impl hoh_core::reconcile::Observer for Markers {
    fn observe(&self, _attempt_id: &str, key: &str) -> hoh_core::reconcile::Observation {
        let marker = self.0.join("markers").join(marker_name(key));
        if marker.exists() {
            hoh_core::reconcile::Observation::Found {
                external_ref: marker.to_string_lossy().into_owned(),
            }
        } else {
            hoh_core::reconcile::Observation::Absent
        }
    }
}

/// Ends its own process with SIGKILL when an attempt reaches `0` (CR-006 F2).
#[derive(Debug)]
struct KillAtRunner(hoh_app::host::RunnerPoint);

impl hoh_app::host::HostHooks for KillAtRunner {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == self.0 {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            loop {
                std::thread::park();
            }
        }
    }
}

fn runner_point(name: &str) -> Option<hoh_app::host::RunnerPoint> {
    use hoh_app::host::RunnerPoint;
    Some(match name {
        "BeforeSend" => RunnerPoint::BeforeSend,
        "AfterSpawn" => RunnerPoint::AfterSpawn,
        "AfterSend" => RunnerPoint::AfterSend,
        "AfterAck" => RunnerPoint::AfterAck,
        "AfterEffectStarted" => RunnerPoint::AfterEffectStarted,
        "AfterEffectCommitted" => RunnerPoint::AfterEffectCommitted,
        _ => return None,
    })
}

fn point_named(name: &str) -> Option<WritePoint> {
    Some(match name {
        "Begun" => WritePoint::Begun,
        "StateWritten" => WritePoint::StateWritten,
        "EventsWritten" => WritePoint::EventsWritten,
        "OutboxWritten" => WritePoint::OutboxWritten,
        "Committed" => WritePoint::Committed,
        _ => return None,
    })
}

/// SIGKILL at the commit of a checkpoint once the database holds a session.start notice: the commander's handover
/// committed, nothing after it run (AC-019-05). Other checkpoints (an empty one commits nothing, an agent not looked at
/// yet holds the change) pass.
#[derive(Debug)]
struct KillAtHandover(PathBuf);

impl FaultHooks for KillAtHandover {
    fn at(&self, job: &'static str, point: WritePoint) {
        if job != "Checkpoint" || point != WritePoint::Committed {
            return;
        }
        let notices = query(
            &self.0,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'session.start'",
        );
        if notices.is_ok_and(|n| n != "0") {
            let _ = rustix::process::kill_process(
                rustix::process::getpid(),
                rustix::process::Signal::KILL,
            );
            loop {
                std::thread::park();
            }
        }
    }
}

/// An executor with the product's registry and no work (no task is dispatched): it records each first input it is
/// given, one line `<message id> <session id>` in its file, and takes it.
#[derive(Debug)]
struct Handovers {
    registry: Arc<hoh_app::host::agents::Registry>,
    log: PathBuf,
}

impl hoh_app::host::Executor for Handovers {
    fn registry(&self) -> Option<Arc<hoh_app::host::agents::Registry>> {
        Some(self.registry.clone())
    }

    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        tokio::sync::oneshot::channel().1
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        Err("no work is expected".to_owned().into())
    }

    fn open_session(
        &self,
        start: &hoh_core::persistence::SessionStart,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        use std::io::Write as _;
        let (sent, answer) = tokio::sync::oneshot::channel();
        let line = format!(
            "{} {}\n",
            start.message_id,
            start.session_id.as_deref().unwrap_or("-")
        );
        let kept = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .map_err(|e| e.to_string());
        let _ = sent.send(kept);
        answer
    }
}

/// The old Codex Commander's messages after the handover (AC-019-05): two decisions and a steer, all at epoch 1.
fn old_commander_messages() -> R<Vec<Message>> {
    let message = |id: &str, kind: &str, recipient: &str, payload: Value| -> R<Message> {
        Ok(serde_json::from_value(json!({
            "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "T", "attempt_id": "-",
            "sender_role": "commander", "recipient_role": recipient, "role_epoch": 1, "correlation_id": "c",
            "causation_id": null, "idempotency_key": format!("k-{id}"), "expected_revision": 1,
            "spec_sha256": sha(1), "candidate_id": null, "kind": kind, "payload": payload
        }))?)
    };
    Ok(vec![
        message(
            "old-dispatch",
            "decision",
            "runtime",
            json!({"kind": "dispatch", "task_id": "T", "role": "developer"}),
        )?,
        message(
            "old-transition",
            "decision",
            "runtime",
            json!({"kind": "transition_task", "task_id": "T", "to_state": "READY"}),
        )?,
        message(
            "old-steer",
            "steer",
            "developer",
            json!({"text": "rebase first"}),
        )?,
    ])
}

/// The child entry: a host built in the test target. With [`CHILD_DECIDE`] it starts, submits the decision and is
/// expected to die at its kill point; without, it runs as `hoh_app::host::run` until SIGTERM.
#[test]
#[ignore = "run by the recovery tests in a process of their own"]
fn recovery_host() {
    let Some(project) = std::env::var_os(CHILD_PROJECT) else {
        return;
    };
    let mut config = config(Path::new(&project));
    if let Ok(kill) = std::env::var(CHILD_KILL) {
        let (job, point) = kill.split_once(':').expect("<job>:<point>");
        config.store.faults = Arc::new(KillAt {
            job: job.to_owned(),
            point: point_named(point).expect("a write point"),
        });
    }
    if let Some(ms) = std::env::var(CHILD_GRACE_MS)
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
    {
        config.reconcile.native_grace = Duration::from_millis(ms);
        config.reconcile.term_grace = Duration::from_millis(ms);
        config.reconcile.kill_grace = Duration::from_millis(ms.max(2000));
    }
    if let Some(remote) = std::env::var_os(CHILD_REMOTE) {
        config.reconcile.observer = Arc::new(FakeRemote(PathBuf::from(remote)));
    }
    if let Some(work) = std::env::var_os(CHILD_WORK) {
        let developer = ["structured_events", "command_exec"].map(str::to_owned);
        config.runner = Some(hoh_app::host::RunnerConfig {
            executor: Arc::new(FakeExecutor(PathBuf::from(&work))),
            context: Context {
                required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                    .into(),
                ..Context::default()
            },
        });
        config.reconcile.observer = Arc::new(Markers(PathBuf::from(work)));
    }
    if let Ok(class) = std::env::var(CHILD_FAIL_CLASS) {
        let class = match class.strip_prefix("retry_after:") {
            Some(secs) => hoh_core::retry::FailureClass::Transient {
                retry_after: Some(Duration::from_secs(secs.parse().expect("seconds"))),
            },
            None => hoh_core::retry::FailureClass::Auth,
        };
        let developer = ["structured_events", "command_exec"].map(str::to_owned);
        config.runner = Some(hoh_app::host::RunnerConfig {
            executor: Arc::new(FailingExecutor(class)),
            context: Context {
                required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                    .into(),
                ..Context::default()
            },
        });
    }
    if let Some(bin) = std::env::var_os(CHILD_AGENTS) {
        let pi_dir = PathBuf::from(&bin).join("pi-agent");
        let registry = Arc::new(hoh_app::host::agents::Registry::with_pi_dir(
            bin.clone(),
            Some(pi_dir),
        ));
        config.runner = Some(hoh_app::host::RunnerConfig {
            executor: Arc::new(Handovers {
                registry,
                log: PathBuf::from(&bin).join("first-inputs.log"),
            }),
            context: Context::default(),
        });
    }
    if std::env::var_os(CHILD_KILL_AT_HANDOVER).is_some() {
        config.store.faults = Arc::new(KillAtHandover(db(Path::new(&project))));
    }
    if let Some(clock) = std::env::var_os(CHILD_CLOCK) {
        config.clock = hoh_app::host::HostClock(Arc::new(FileClock(PathBuf::from(clock))));
    }
    let runner_kill = std::env::var(CHILD_RUNNER_KILL).ok();
    if let Some(point) = &runner_kill {
        config.hooks = Arc::new(KillAtRunner(runner_point(point).expect("a runner point")));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    if let Ok(decision) = std::env::var(CHILD_DECIDE) {
        let code = runtime.block_on(async {
            let host = Host::start(config).await.expect("the host starts");
            let message: Message = serde_json::from_str(&decision).expect("a decision message");
            let proposal = Proposal::from_message(&message).expect("a proposal");
            let decided = host.store().decide(proposal, Context::default()).await;
            if runner_kill.is_some() {
                // The runner reaches its kill point while this waits.
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            eprintln!("decided without being killed: {decided:?}");
            NOT_KILLED
        });
        std::process::exit(code);
    }
    if let Some(file) = std::env::var_os(CHILD_OLD_COMMANDER) {
        runtime.block_on(async {
            use std::io::Write as _;
            let host = Host::start(config).await.expect("the host starts");
            let mut lines = String::new();
            for message in old_commander_messages().expect("the old Commander's messages") {
                let got = if message.kind == hoh_core::protocol::MessageKind::Decision {
                    let proposal = Proposal::from_message(&message).expect("a proposal");
                    match host.store().decide(proposal, Context::default()).await {
                        Ok(hoh_core::persistence::Decided::Rejected { reason, .. }) => {
                            format!(
                                "rejected {}",
                                serde_json::to_value(reason).unwrap_or_default()
                            )
                        }
                        other => format!("not rejected {other:?}"),
                    }
                } else {
                    match host.store().commander_message(&message).await {
                        Ok(hoh_core::persistence::Fenced::Rejected { reason, .. }) => {
                            format!(
                                "rejected {}",
                                serde_json::to_value(reason).unwrap_or_default()
                            )
                        }
                        other => format!("not rejected {other:?}"),
                    }
                };
                lines.push_str(&format!("{} {got}\n", message.message_id));
            }
            let mut out = std::fs::File::create(&file).expect("the outcome file");
            out.write_all(lines.as_bytes()).expect("written");
            // The host serves on until the test stops it.
            tokio::time::sleep(Duration::from_secs(120)).await;
        });
        std::process::exit(0);
    }
    let code = runtime.block_on(hoh_app::host::run(config));
    std::process::exit(i32::from(code));
}

/// The time the test writes to a file (milliseconds since the epoch), the system's while the file cannot be read.
#[derive(Debug)]
struct FileClock(PathBuf);

impl hoh_core::delivery::Clock for FileClock {
    fn now(&self) -> std::time::SystemTime {
        std::fs::read_to_string(&self.0)
            .ok()
            .and_then(|ms| ms.trim().parse::<u64>().ok())
            .map_or_else(std::time::SystemTime::now, |ms| {
                std::time::UNIX_EPOCH + Duration::from_millis(ms)
            })
    }
}

/// Set the time a [`FileClock`] reads.
fn set_clock(file: &Path, at: std::time::SystemTime) -> R {
    let ms = at.duration_since(std::time::UNIX_EPOCH)?.as_millis();
    std::fs::write(file, ms.to_string())?;
    Ok(())
}

/// Start a host child with `env` and wait until it serves (its own host.json).
fn start_child(project: &Path, env: &[(&str, &std::ffi::OsStr)]) -> R<ChildGuard> {
    let mut command = child(project)?;
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = ChildGuard(command.spawn()?);
    let own = u64::from(guard.0.id());
    let end = Instant::now() + Duration::from_secs(30);
    while announced(project) != Some(own) {
        if let Some(status) = guard.0.try_wait()? {
            return Err(format!("the host ended before it served: {status:?}").into());
        }
        if Instant::now() > end {
            return Err("the host did not start".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(guard)
}

/// A child process that is always ended and reaped, whatever the test does.
struct ChildGuard(std::process::Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn child(project: &Path) -> R<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "recovery_host",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_PROJECT, project)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    Ok(command)
}

/// Wait for `child` to end, at most `limit`; its status and stderr.
fn wait(mut child: ChildGuard, limit: Duration) -> R<(std::process::ExitStatus, String)> {
    let end = Instant::now() + limit;
    loop {
        if let Some(status) = child.0.try_wait()? {
            let mut stderr = String::new();
            if let Some(mut pipe) = child.0.stderr.take() {
                use std::io::Read as _;
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((status, stderr));
        }
        if Instant::now() > end {
            return Err("the host process did not end in time".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The pid in the project's host.json, if there is one (a host killed by SIGKILL leaves its file behind).
fn announced(project: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(project.join(".hoh/host.json")).ok()?;
    serde_json::from_str::<Value>(&text).ok()?["pid"].as_u64()
}

/// Start a host (no faults, no command), wait until it serves (its own host.json, not one a killed host left), stop
/// it with SIGTERM: its exit code and stderr.
fn serve_and_stop(project: &Path) -> R<(i32, String)> {
    serve_and_stop_with(project, &[])
}

/// [`serve_and_stop`] with these child variables.
fn serve_and_stop_with(project: &Path, env: &[(&str, &std::ffi::OsStr)]) -> R<(i32, String)> {
    let mut command = child(project)?;
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = ChildGuard(command.spawn()?);
    let own = u64::from(guard.0.id());
    let end = Instant::now() + Duration::from_secs(30);
    while announced(project) != Some(own) {
        if let Some(status) = guard.0.try_wait()? {
            return Err(format!("the host ended before it served: {status:?}").into());
        }
        if Instant::now() > end {
            return Err("the host did not start".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(guard.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let (status, stderr) = wait(guard, Duration::from_secs(30))?;
    Ok((
        status
            .code()
            .ok_or_else(|| format!("ended by a signal ({status:?}): {stderr}"))?,
        stderr,
    ))
}

/// Run `script` (Python, sqlite3 module) against `db`; its stdout. Test code: the product never writes outside
/// persistence.
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

/// Write the evidence file `name` when the check gives a directory for it.
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

fn sha(n: u32) -> String {
    format!("{n:064}")
}

/// AC-036-02's world in the host's project: run R READY at revision 7 (spec s2, the Commander bound at epoch 3), its
/// task T1 READY for the developer.
const READY_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'READY', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's2', 'READY', 1, 'developer', 1, 1, 1);
";

/// Seed `sql` (with `:p` the host's project id) into the project's database.
fn seed(project: &Path, sql: &str) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); \
                  p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
                  c.executescript(sys.stdin.read().replace(':p', repr(p))); c.commit(); c.close()";
    python(&db(project), script, sql).map(drop)
}

/// The Commander's decision to start run R: READY -> RUNNING at revision 7.
fn start_run() -> Value {
    json!({
        "schema_version": "1.0", "message_id": "d-start", "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 3, "correlation_id": "c",
        "causation_id": null, "idempotency_key": "k-start", "expected_revision": 7, "spec_sha256": sha(2),
        "candidate_id": null, "kind": "decision",
        "payload": {"kind": "transition_run", "to_state": "RUNNING"}
    })
}

/// What TEST-036-02 reads after the restart: the run, the transition's events and outbox rows, the integrity check
/// and the outbox rows whose event is missing.
const READ_TRANSITION: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
run = c.execute("SELECT state, revision FROM runs WHERE run_id = 'R'").fetchone()
events = [r[0] for r in c.execute(
    "SELECT seq FROM events WHERE kind = 'decision.applied' AND json_extract(payload_json, '$.decision_id') = 'd-start'")]
outbox = [r[0] for r in c.execute(
    "SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.decision_id') = 'd-start'")]
decisions = c.execute("SELECT count(*) FROM decisions WHERE decision_id = 'd-start'").fetchone()[0]
orphans = c.execute("SELECT count(*) FROM outbox WHERE event_seq NOT IN (SELECT seq FROM events)").fetchone()[0]
check = [r[0] for r in c.execute("PRAGMA integrity_check")]
print(json.dumps({"run": list(run), "events": events, "outbox": outbox, "decisions": decisions,
                  "orphans": orphans, "integrity": check}))
"#;

/// Copy the database files of `from` to `to` (the files that exist).
fn copy_db(from: &Path, to: &Path) -> R {
    std::fs::create_dir_all(to)?;
    for suffix in ["", "-wal", "-shm"] {
        let name = format!("state.db{suffix}");
        let _ = std::fs::remove_file(to.join(&name));
        if from.join(&name).exists() {
            std::fs::copy(from.join(&name), to.join(&name))?;
        }
    }
    Ok(())
}

/// AC-036-02: a host killed (SIGKILL by itself) at each of the five points of the transaction that moves run R
/// READY -> RUNNING and queues its notice, 20 times each, then restarted without any command: before the commit the
/// run is still READY at its revision with none of the transition's events or outbox rows; after it, RUNNING with
/// exactly one of each, the outbox row pointing at the event; every integrity check is ok and no outbox row points
/// at a missing event.
#[test]
fn test_036_02_a_transition_killed_at_any_point_is_all_or_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    // The fixture: the host makes the database (its project), the world goes in, the host's files are kept.
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, READY_WORLD)?;
    let template = dir.path().join("template");
    copy_db(&project.join(".hoh"), &template)?;
    let decision = start_run().to_string();
    let points = [
        "Begun",
        "StateWritten",
        "EventsWritten",
        "OutboxWritten",
        "Committed",
    ];
    let mut runs = Vec::new();
    for point in points {
        for round in 0..20 {
            copy_db(&template, &project.join(".hoh"))?;
            let killed = ChildGuard(
                child(&project)?
                    .env(CHILD_KILL, format!("Decide:{point}"))
                    .env(CHILD_DECIDE, &decision)
                    .spawn()?,
            );
            let (status, stderr) = wait(killed, Duration::from_secs(60))?;
            assert_eq!(
                status.signal(),
                Some(9),
                "{point} #{round}: not killed at its point ({status:?}): {stderr}"
            );
            let (code, stderr) = serve_and_stop(&project)?;
            assert_eq!(code, 0, "{point} #{round}: the restarted host: {stderr}");
            let read: Value = serde_json::from_str(&python(&db(&project), READ_TRANSITION, "")?)?;
            let committed = point == "Committed";
            let expected = if committed {
                (json!(["RUNNING", 8]), 1, 1)
            } else {
                (json!(["READY", 7]), 0, 0)
            };
            assert_eq!(
                (
                    &read["run"],
                    read["events"].as_array().map_or(usize::MAX, Vec::len),
                    read["outbox"].as_array().map_or(usize::MAX, Vec::len)
                ),
                (&expected.0, expected.1, expected.2),
                "{point} #{round}: {read}"
            );
            if committed {
                assert_eq!(read["outbox"][0], read["events"][0], "{point} #{round}");
            }
            assert_eq!(
                read["decisions"],
                json!(i32::from(committed)),
                "{point} #{round}"
            );
            assert_eq!(read["orphans"], json!(0), "{point} #{round}");
            assert_eq!(read["integrity"], json!(["ok"]), "{point} #{round}");
            runs.push(json!({"point": point, "round": round, "signal": 9, "read": read}));
        }
    }
    keep(
        "test-036-02.json",
        &json!({"runs": runs, "count": runs.len()}),
    )?;
    Ok(())
}

/// A helper process unrelated to any attempt that records every catchable signal it gets, one line each, in `log`,
/// returned once it says its traps are set (a builtin's write of its pid to `<log>.ready`): a signal before them would
/// end it unrecorded, and an empty log would then pass (CIR-151).
fn signal_counter(log: &Path) -> R<ChildGuard> {
    let ready = log.with_extension("ready");
    let script = format!(
        "for s in TERM INT HUP USR1 USR2 QUIT; do trap \"echo $s >> '{}'\" $s; done; echo $$ > '{}'; \
         while :; do sleep 0.05; done",
        log.display(),
        ready.display()
    );
    let helper = ChildGuard(
        Command::new("sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?,
    );
    trapped_at(&ready)?;
    Ok(helper)
}

/// The pid a trap stand-in wrote to `ready` once its traps were set, waited for (at most 10 s; convention v2: a
/// child's state read only after an event it makes).
fn trapped_at(ready: &Path) -> R<u32> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(pid) = std::fs::read_to_string(ready)
            .unwrap_or_default()
            .trim()
            .parse()
        {
            return Ok(pid);
        }
        if Instant::now() > end {
            return Err(format!("{} never said its traps were set", ready.display()).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// What reconcile events and attempts AC-041-03 and AC-041-05 read.
const READ_RECONCILE: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
attempts = {r[0]: r[1] for r in c.execute("SELECT attempt_id, state FROM attempts")}
events = [{"seq": r[0], "kind": r[1], "payload": json.loads(r[2])} for r in c.execute(
    "SELECT seq, kind, payload_json FROM events WHERE kind LIKE 'reconcile.%' ORDER BY seq")]
effects = {r[0]: {"state": r[1], "external_ref": r[2]} for r in c.execute(
    "SELECT effect_key, state, external_ref FROM effects")}
tasks = {r[0]: r[1] for r in c.execute("SELECT task_id, state FROM tasks")}
print(json.dumps({"attempts": attempts, "events": events, "effects": effects, "tasks": tasks}))
"#;

/// AC-036-02's world with run R RUNNING and two running tasks.
const RUNNING_WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T2', 'R', 's2', 'RUNNING', 3, 'developer', 0, 1, 1), ('T3', 'R', 's2', 'RUNNING', 3, 'developer', 0, 2, 1);
";

/// AC-041-03: attempt A2 records the pid of a live, unrelated process with this boot but another start time, A3 the
/// same pid with its start time but another boot. The restarted host's reconcile finds neither is ours, sends no
/// signal (the helper lives and logged none), records the basis in the reconcile events, and ends both (not RUNNING).
#[test]
fn test_041_03_a_reused_pid_or_another_boot_is_not_signalled() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let log = dir.path().join("signals.log");
    let helper = signal_counter(&log)?;
    let pid = helper.0.id();
    let me = hoh_agents::identity::of(pid)?.ok_or("the helper is not running")?;
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('A2', 'T2', 'RUNNING', 1, 1, 1, {pid}, '{boot}', '{wrong_start}', '2026-11-07T00:00:00Z'),
               ('A3', 'T3', 'RUNNING', 1, 1, 1, {pid}, '{other_boot}', '{start}', '2026-11-07T00:00:00Z');",
            boot = me.boot_id,
            wrong_start = me.start_id.clone() + "1",
            other_boot = "00000000-0000-0000-0000-000000000000",
            start = me.start_id,
        ),
    )?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    // The helper lives on, the same process, and got no signal: alive by the parent's own view of its child (a
    // zombie of the same start time would be a helper killed by SIGKILL, which no trap records; CIR-150, -200).
    assert!(
        !hoh_agents::identity::child_ended(pid)?,
        "the helper was killed"
    );
    assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "");
    for (attempt, basis) in [("A2", "start_time_differs"), ("A3", "boot_id_differs")] {
        let state = read["attempts"][attempt].as_str().unwrap_or_default();
        assert!(
            ["SETTLED", "FAILED", "UNKNOWN_EFFECT"].contains(&state),
            "{attempt}: {state}"
        );
        let event = read["events"]
            .as_array()
            .and_then(|events| {
                events.iter().find(|e| {
                    e["kind"] == "reconcile.attempt" && e["payload"]["attempt_id"] == attempt
                })
            })
            .ok_or("no reconcile event")?;
        assert_eq!(
            (
                &event["payload"]["process"]["found"],
                &event["payload"]["process"]["basis"],
                &event["payload"]["process"]["signals"]
            ),
            (&json!("gone"), &json!(basis), &json!(0)),
            "{attempt}: {event}"
        );
    }
    let kinds: Vec<&str> = read["events"]
        .as_array()
        .ok_or("no events")?
        .iter()
        .filter_map(|e| e["kind"].as_str())
        .collect();
    assert_eq!(kinds.first(), Some(&"reconcile.started"));
    assert_eq!(kinds.last(), Some(&"reconcile.completed"));
    keep(
        "test-041-03.json",
        &json!({"helper": {"pid": pid, "boot_id": me.boot_id, "start_id": me.start_id, "signals": 0}, "read": read}),
    )?;
    drop(helper);
    Ok(())
}

/// An old host's agent still running when the new host starts (its record names it: pid, boot and start time): the
/// reconcile stops it (SIGTERM to its group, which it ignores, then SIGKILL) before it disposes of the attempt.
#[test]
fn an_old_hosts_live_agent_is_stopped_before_its_attempt_is_disposed_of() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let log = dir.path().join("signals.log");
    // An orphan leading its own group, as an old host's agent is after a kill -9 (the substrate starts each agent as
    // a group leader; its parent is gone, so another process reaps it).
    let script = format!(
        "setsid sh -c 'trap \"echo TERM >> {}\" TERM; while :; do sleep 0.05; done' </dev/null >/dev/null 2>&1 & echo $!",
        log.display()
    );
    let out = Command::new("sh").args(["-c", &script]).output()?;
    let pid: u32 = String::from_utf8_lossy(&out.stdout).trim().parse()?;
    // Gone whatever happens below (M2T05R-61).
    let _guard = GroupGuard(pid);
    let end = Instant::now() + Duration::from_secs(5);
    // Until its trap is set: its loop's first sleep (a member besides the leader) comes after it.
    while hoh_agents::identity::group_left(pid, pid).map_or(0, |l| l.len()) < 1
        && Instant::now() < end
    {
        std::thread::sleep(Duration::from_millis(10));
    }
    let me = hoh_agents::identity::of(pid)?.ok_or("the agent is not running")?;
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('A2', 'T2', 'RUNNING', 1, 1, 1, {pid}, '{boot}', '{start}', '2026-11-07T00:00:00Z');",
            boot = me.boot_id,
            start = me.start_id,
        ),
    )?;
    let (code, stderr) = serve_and_stop_with(&project, &[(CHILD_GRACE_MS, "300".as_ref())])?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    assert_eq!(std::fs::read_to_string(&log)?.trim(), "TERM");
    let event = read["events"]
        .as_array()
        .and_then(|events| events.iter().find(|e| e["kind"] == "reconcile.attempt"))
        .ok_or("no reconcile event")?;
    assert_eq!(
        event["payload"]["process"],
        json!({"found": "ours", "steps": ["sigterm", "sigkill", "reaped"], "reaped": true})
    );
    assert_eq!(read["attempts"]["A2"], json!("FAILED"));
    assert_eq!(read["tasks"]["T2"], json!("READY"));
    assert_eq!(group_now(pid), nothing_left());
    Ok(())
}

/// AC-041-05: effect E (a pull request) STARTED and the FakeRemote already has one with its marker; E2 STARTED and
/// the FakeRemote cannot be asked. The reconcile commits E with the existing PR as its external ref, makes E2
/// UNKNOWN and its attempt UNKNOWN_EFFECT with a diagnostic naming E2, and never creates anything.
#[test]
fn test_041_05_a_started_effect_is_found_or_unknown_never_done_again() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('AE', 'T2', 'RUNNING', 1, 1, 1, {gone}, '{boot}', '1', '2026-11-07T00:00:00Z'),
               ('AE2', 'T3', 'RUNNING', 1, 1, 1, {gone}, '{boot}', '1', '2026-11-07T00:00:00Z');
             INSERT INTO effects(effect_key, attempt_id, state) VALUES ('pr:R/T2', 'AE', 'STARTED'), ('pr:R/T3', 'AE2', 'STARTED');",
            boot = hoh_agents::identity::boot_id()?,
            // A pid no process has (pid_max is at most 2^22): the record of agents long gone.
            gone = 1 << 23,
        ),
    )?;
    let remote = dir.path().join("forge.json");
    std::fs::write(
        &remote,
        json!({"available": {"pr:R/T2": "https://forge.invalid/pr/17"}, "unavailable": ["pr:R/T3"]}).to_string(),
    )?;
    let (code, stderr) = serve_and_stop_with(&project, &[(CHILD_REMOTE, remote.as_os_str())])?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(&db(&project), READ_RECONCILE, "")?)?;
    let e = &read["effects"]["pr:R/T2"];
    assert!(
        ["OBSERVED", "COMMITTED"].contains(&e["state"].as_str().unwrap_or_default()),
        "{e}"
    );
    assert_eq!(e["external_ref"], json!("https://forge.invalid/pr/17"));
    assert_eq!(read["effects"]["pr:R/T3"]["state"], json!("UNKNOWN"));
    assert_eq!(read["attempts"]["AE2"], json!("UNKNOWN_EFFECT"));
    // No new attempt for its task, which stays where it was.
    assert_eq!(read["tasks"]["T3"], json!("RUNNING"));
    let diagnostic = read["events"]
        .as_array()
        .and_then(|events| {
            events.iter().find(|e| {
                e["kind"] == "reconcile.diagnostic"
                    && e["payload"]["effect_keys"]
                        .as_array()
                        .is_some_and(|k| k.iter().any(|x| x == "pr:R/T3"))
            })
        })
        .ok_or("no diagnostic naming E2")?;
    assert_eq!(diagnostic["payload"]["code"], json!("UNKNOWN_EFFECT"));
    // Exactly one query per STARTED effect, and nothing else (no create: the port has none).
    let calls = std::fs::read_to_string(dir.path().join("forge.json.calls"))?;
    let mut lines: Vec<&str> = calls.lines().collect();
    lines.sort_unstable();
    assert_eq!(lines, ["query pr:R/T2", "query pr:R/T3"]);
    keep(
        "test-041-05.json",
        &json!({"read": read, "forge_calls": lines}),
    )?;
    Ok(())
}

#[test]
fn a_writer_found_interrupted_at_a_restart_ends_with_the_workspace_it_left_recorded() -> R {
    // REQ-019 behaviour 6 (docs/adr/ADR-024.md decision 9, M2T17R-52), the startup reconcile's path: writer task T2's
    // attempt AW is RUNNING and holds the writer lease, its agent long gone and its effect STARTED and not found. The
    // restarted host's reconcile ends it UNKNOWN_EFFECT with the workspace's record in the same transaction, before
    // the reconcile completes; its lease is then free. The project is a git work tree with an uncommitted change.
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    git_in(&project, &["init", "-q"])?;
    std::fs::write(project.join(".gitignore"), ".hoh/\n")?;
    std::fs::write(project.join("a.txt"), "one\n")?;
    git_in(&project, &["add", ".gitignore", "a.txt"])?;
    git_in(
        &project,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "first",
        ],
    )?;
    std::fs::write(project.join("a.txt"), "two\n")?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(
        &project,
        &format!(
            "{RUNNING_WORLD}
             UPDATE tasks SET writer = 1 WHERE task_id = 'T2';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('AW', 'T2', 'RUNNING', 1, 1, 1, {gone}, '{boot}', '1', '2026-11-07T00:00:00Z');
             INSERT INTO effects(effect_key, attempt_id, state) VALUES ('pr:R/T2', 'AW', 'STARTED');
             INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES
               (:p, 'writer', 'AW', 1, '2999-01-01T00:00:00Z');",
            boot = hoh_agents::identity::boot_id()?,
            gone = 1 << 23,
        ),
    )?;
    let remote = dir.path().join("forge.json");
    std::fs::write(
        &remote,
        json!({"available": {}, "unavailable": ["pr:R/T2"]}).to_string(),
    )?;
    let (code, stderr) = serve_and_stop_with(&project, &[(CHILD_REMOTE, remote.as_os_str())])?;
    assert_eq!(code, 0, "the restarted host: {stderr}");
    let read: Value = serde_json::from_str(&python(
        &db(&project),
        r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
one = lambda sql: (c.execute(sql).fetchone() or [None])[0]
print(json.dumps({
    "state": one("SELECT state FROM attempts WHERE attempt_id = 'AW'"),
    "records": [[r[0], json.loads(r[1])] for r in c.execute(
        "SELECT seq, payload_json FROM events WHERE kind = 'attempt.workspace' AND json_extract(payload_json, '$.attempt_id') = 'AW'")],
    "ended": one("SELECT seq FROM events WHERE kind = 'attempt.moved' AND json_extract(payload_json, '$.attempt_id') = 'AW' AND json_extract(payload_json, '$.to') = 'UNKNOWN_EFFECT'"),
    "completed": one("SELECT max(seq) FROM events WHERE kind = 'reconcile.completed'"),
    "expires_at": one("SELECT expires_at FROM leases WHERE kind = 'writer'"),
}))
"#,
        "",
    )?)?;
    assert_eq!(read["state"], "UNKNOWN_EFFECT", "{read}");
    let records = read["records"].as_array().ok_or("no records")?;
    assert_eq!(records.len(), 1, "{read}");
    let (seq, record) = (records[0][0].as_i64(), &records[0][1]);
    assert_eq!(seq.map(|s| s + 1), read["ended"].as_i64(), "{read}");
    assert!(seq < read["completed"].as_i64(), "{read}");
    assert!(
        read["expires_at"].as_str() < Some("2999"),
        "the lease was not let go: {read}"
    );
    let workspace = &record["workspace"];
    assert_eq!(
        workspace["head"],
        json!(git_in(&project, &["rev-parse", "HEAD"])?.trim()),
        "{record}"
    );
    let diff = Command::new("git")
        .args(["diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD"])
        .current_dir(&project)
        .output()?
        .stdout;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&diff);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(workspace["diff_sha256"], json!(hex), "{record}");
    Ok(())
}

/// Start a host with these child variables, wait until it serves, then until `done` holds (at most `limit`), stop it
/// with SIGTERM: its exit code, its stderr, and whether `done` held.
fn serve_until(
    project: &Path,
    env: &[(&str, &std::ffi::OsStr)],
    limit: Duration,
    done: impl Fn() -> bool,
) -> R<(i32, String, bool)> {
    let mut command = child(project)?;
    for (name, value) in env {
        command.env(name, value);
    }
    let mut guard = ChildGuard(command.spawn()?);
    let own = u64::from(guard.0.id());
    let end = Instant::now() + Duration::from_secs(30);
    while announced(project) != Some(own) {
        if let Some(status) = guard.0.try_wait()? {
            return Err(format!("the host ended before it served: {status:?}").into());
        }
        if Instant::now() > end {
            return Err("the host did not start".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let end = Instant::now() + limit;
    let mut held = done();
    while !held && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        held = done();
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(guard.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let (status, stderr) = wait(guard, Duration::from_secs(30))?;
    Ok((
        status
            .code()
            .ok_or_else(|| format!("ended by a signal ({status:?}): {stderr}"))?,
        stderr,
        held,
    ))
}

/// AC-041-01's world: run R READY (revision 7, the Commander at epoch 3) with five tasks for the developer, none a
/// writer (two run at a time, `execution.reader_slots`).
const FIVE_TASKS: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'READY', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's2', 'READY', 1, 'developer', 0, 1, 1), ('T2', 'R', 's2', 'READY', 1, 'developer', 0, 2, 1),
  ('T3', 'R', 's2', 'READY', 1, 'developer', 0, 3, 1), ('T4', 'R', 's2', 'READY', 1, 'developer', 0, 4, 1),
  ('T5', 'R', 's2', 'READY', 1, 'developer', 0, 5, 1);
";

/// Every task IMPLEMENTED, or left with its attempt UNKNOWN_EFFECT: the run's next stable point.
const STABLE: &str = "SELECT count(*) FROM tasks t WHERE t.state != 'IMPLEMENTED' AND NOT EXISTS \
                      (SELECT 1 FROM attempts a WHERE a.task_id = t.task_id AND a.state = 'UNKNOWN_EFFECT')";

/// What TEST-041-01 reads after a restart.
const READ_RUN: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
attempts = [{"id": r[0], "task": r[1], "state": r[2], "host_epoch": r[3]} for r in c.execute(
    "SELECT attempt_id, task_id, state, host_epoch FROM attempts ORDER BY rowid")]
reconcile = [{"seq": r[0], "kind": r[1], "at": r[2], "payload": json.loads(r[3])} for r in c.execute(
    "SELECT seq, kind, created_at, payload_json FROM events WHERE kind LIKE 'reconcile.%' ORDER BY seq")]
dispatches = [r[0] for r in c.execute(
    "SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' ORDER BY event_seq")]
tasks = {r[0]: r[1] for r in c.execute("SELECT task_id, state FROM tasks")}
print(json.dumps({"attempts": attempts, "reconcile": reconcile, "dispatches": dispatches, "tasks": tasks,
                  "host_epoch": c.execute("SELECT max(host_epoch) FROM projects").fetchone()[0]}))
"#;

/// AC-041-01: a host killed (SIGKILL by itself) at each of five points of an attempt the runner runs (before the
/// dispatch is sent; sent, not answered; answered, its receipt not written; its work effect STARTED, not OBSERVED; its
/// work effect COMMITTED, its task not moved), ten times each, and restarted at once. Each run ends at its next
/// stable point with no effect key counted twice, every attempt of the killed host SETTLED, CANCELLED or
/// UNKNOWN_EFFECT, a task given another attempt only after its earlier one was judged never started, no new attempt
/// for a task whose attempt is UNKNOWN_EFFECT, the reconcile completed before the restarted host's first dispatch,
/// and the reconcile started within 30 s of the kill.
#[test]
fn test_041_01_kill_at_each_point_of_a_dispatch_never_runs_an_effect_twice() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, FIVE_TASKS)?;
    let template = dir.path().join("template");
    copy_db(&project.join(".hoh"), &template)?;
    let decision = start_run().to_string();
    let points = [
        "BeforeSend",
        "AfterSend",
        "AfterAck",
        "AfterEffectStarted",
        "AfterEffectCommitted",
    ];
    let mut runs = Vec::new();
    for point in points {
        for round in 0..10 {
            copy_db(&template, &project.join(".hoh"))?;
            let work = dir.path().join(format!("work-{point}-{round}"));
            std::fs::create_dir_all(work.join("markers"))?;
            let killed = ChildGuard(
                child(&project)?
                    .env(CHILD_RUNNER_KILL, point)
                    .env(CHILD_WORK, &work)
                    .env(CHILD_DECIDE, &decision)
                    .spawn()?,
            );
            let (status, stderr) = wait(killed, Duration::from_secs(90))?;
            let t0 = std::time::SystemTime::now();
            assert_eq!(
                status.signal(),
                Some(9),
                "{point} #{round}: not killed at its point ({status:?}): {stderr}"
            );
            let db = db(&project);
            let restart = std::time::SystemTime::now();
            let (code, stderr, stable) = serve_until(
                &project,
                &[(CHILD_WORK, work.as_os_str())],
                Duration::from_secs(60),
                || {
                    python(&db, &format!("import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute({STABLE:?}).fetchone()[0])"), "")
                    .is_ok_and(|n| n.trim() == "0")
                },
            )?;
            assert_eq!(code, 0, "{point} #{round}: the restarted host: {stderr}");
            assert!(
                stable,
                "{point} #{round}: the run did not reach a stable point: {stderr}"
            );
            let read: Value = serde_json::from_str(&python(&db, READ_RUN, "")?)?;
            let counted = std::fs::read_to_string(work.join("counter")).unwrap_or_default();
            let mut keys: Vec<&str> = counted.lines().collect();
            keys.sort_unstable();
            let before = keys.len();
            keys.dedup();
            assert_eq!(
                keys.len(),
                before,
                "{point} #{round}: an effect ran twice: {counted}"
            );
            // The killed host's epoch is the one before the restarted host's.
            let killed_epoch = read["host_epoch"].as_i64().ok_or("no epoch")? - 1;
            let attempts = read["attempts"].as_array().ok_or("no attempts")?;
            let restart_reconcile: Vec<&Value> = read["reconcile"]
                .as_array()
                .ok_or("no reconcile")?
                .iter()
                .collect();
            // The restarted host's reconcile (its epoch), not any other.
            let epoch = read["host_epoch"].as_i64();
            let started = restart_reconcile
                .iter()
                .find(|e| {
                    e["kind"] == "reconcile.started" && e["payload"]["host_epoch"].as_i64() == epoch
                })
                .ok_or("no reconcile.started of the restarted host")?;
            let completed = restart_reconcile
                .iter()
                .find(|e| {
                    e["kind"] == "reconcile.completed"
                        && e["payload"]["host_epoch"].as_i64() == epoch
                })
                .ok_or("no reconcile.completed of the restarted host")?;
            let dispositions: std::collections::BTreeMap<&str, &str> = restart_reconcile
                .iter()
                .filter(|e| {
                    e["kind"] == "reconcile.attempt" && e["seq"].as_i64() > started["seq"].as_i64()
                })
                .filter_map(|e| {
                    Some((
                        e["payload"]["attempt_id"].as_str()?,
                        e["payload"]["disposition"].as_str()?,
                    ))
                })
                .collect();
            for a in attempts
                .iter()
                .filter(|a| a["host_epoch"].as_i64() == Some(killed_epoch))
            {
                let state = a["state"].as_str().unwrap_or_default();
                assert!(
                    ["SETTLED", "CANCELLED", "UNKNOWN_EFFECT"].contains(&state),
                    "{point} #{round}: old attempt {a}"
                );
            }
            for task in ["T1", "T2", "T3", "T4", "T5"] {
                let of: Vec<&Value> = attempts.iter().filter(|a| a["task"] == task).collect();
                for pair in of.windows(2) {
                    let earlier = pair[0]["id"].as_str().unwrap_or_default();
                    assert_ne!(
                        pair[0]["state"], "UNKNOWN_EFFECT",
                        "{point} #{round}: {task} got another attempt"
                    );
                    assert_eq!(
                        (
                            pair[0]["state"].as_str(),
                            dispositions.get(earlier).copied()
                        ),
                        (Some("CANCELLED"), Some("ended")),
                        "{point} #{round}: {task} got another attempt without a never-started judgement"
                    );
                }
            }
            let after: Vec<i64> = read["dispatches"]
                .as_array()
                .ok_or("no dispatches")?
                .iter()
                .filter_map(Value::as_i64)
                .filter(|seq| Some(*seq) > started["seq"].as_i64())
                .collect();
            if let Some(first) = after.first() {
                assert!(
                    completed["seq"].as_i64() < Some(*first),
                    "{point} #{round}: a dispatch before the reconcile completed"
                );
            }
            let ms = |t: std::time::SystemTime| {
                t.duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            };
            let (killed_ms, restart_ms) = (ms(t0), ms(restart));
            let reconcile_ms = started["payload"]["at_ms"].as_u64().ok_or("no at_ms")?;
            assert!(
                restart_ms - killed_ms <= 1000,
                "{point} #{round}: restarted after {} ms",
                restart_ms - killed_ms
            );
            // Not before the kill, and within 30 s of it.
            assert!(
                reconcile_ms >= killed_ms,
                "{point} #{round}: reconcile before the kill"
            );
            let delay = Duration::from_millis(reconcile_ms - killed_ms);
            assert!(
                delay <= Duration::from_secs(30),
                "{point} #{round}: reconcile after {delay:?}"
            );
            runs.push(json!({
                "point": point, "round": round, "signal": 9, "counted": counted.lines().count(),
                "keys": keys.len(), "killed_ms": killed_ms, "restart_ms": restart_ms, "reconcile_ms": reconcile_ms,
                "reconcile_after_ms": delay.as_millis(), "dispositions": dispositions,
                "attempts": attempts, "tasks": read["tasks"],
            }));
        }
    }
    keep(
        "test-041-01.json",
        &json!({"runs": runs, "count": runs.len()}),
    )?;
    Ok(())
}

/// A clock the test moves forward (CR-006 F2: a clock swapped by test code): the system time plus a jump the test
/// adds, so it never runs behind the system time an attempt's end is recorded at.
#[derive(Debug, Default)]
struct FakeClock(std::sync::Mutex<Duration>);

impl FakeClock {
    /// Jump `by` ahead.
    fn advance(&self, by: Duration) {
        if let Ok(mut ahead) = self.0.lock() {
            *ahead += by;
        }
    }
}

impl hoh_core::delivery::Clock for FakeClock {
    fn now(&self) -> std::time::SystemTime {
        std::time::SystemTime::now() + self.0.lock().map(|d| *d).unwrap_or_default()
    }
}

/// Starts each attempt's work as an agent that ignores SIGTERM (and passes the ignore to its children) and never
/// ends; its native cancel is recorded, and ignored too. Every group it started is killed when it is dropped.
#[derive(Debug)]
struct StuckExecutor {
    cancels: PathBuf,
    started: std::sync::Mutex<Vec<u32>>,
    /// Whether it holds the turns it started, so a native cancel reaches them (an executor that lost one does not).
    holds: bool,
}

impl hoh_app::host::Executor for StuckExecutor {
    fn deliver(
        &self,
        _work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (answer, ack) = tokio::sync::oneshot::channel();
        let _ = answer.send(Ok(()));
        ack
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let (child, pid, gate) = self.spawn()?;
        let ended = reaping(child);
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }

    fn cancel(&self, work: &hoh_app::host::Work) -> bool {
        use std::io::Write as _;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cancels)
        {
            let _ = writeln!(file, "{}", work.attempt_id);
        }
        // When, for the graces between the steps (M2T07R-254).
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.cancels.with_extension("at"))
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let _ = writeln!(file, "{now}");
        }
        self.holds
    }
}

impl StuckExecutor {
    /// Its agent's process at its gate.
    fn spawn(&self) -> Result<(std::process::Child, u32, hoh_app::host::Gate), String> {
        use std::os::unix::process::CommandExt as _;
        // A member that lives on in its group and ignores SIGTERM too (M2T05R-152): only a SIGKILL to the whole group
        // ends it. The leader outlives SIGTERM as well and marks the first one by a write of its own, a builtin: a
        // `date` it forked there could still be running at the SIGKILL that follows the term grace (300 ms), leaving
        // the file made and empty (seen in CI, reproduced with a slow date); its time is the file's mtime (M2T07R-254).
        // Once its trap is set it says so in `<cancels>.ready`, a builtin's write too: a SIGTERM before it is ignored
        // and marks nothing, so a test that reads the mark sends none before (`trapped`; convention v2).
        let terms = self.cancels.with_extension("term");
        let ready = self.cancels.with_extension("ready");
        let mut child = Command::new("sh")
            .args([
                "-c",
                &format!(
                    "trap '' TERM; read -r go; [ \"$go\" = go ] || exit 0; sleep 1000 & \
                     trap 'echo term >> {}; trap \"\" TERM' TERM; echo $$ >> {}; while :; do sleep 0.05; done",
                    terms.display(),
                    ready.display()
                ),
            ])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        let gate = gate(&mut child);
        if let Ok(mut started) = self.started.lock() {
            started.push(pid);
        }
        Ok((child, pid, gate))
    }
}

impl StuckExecutor {
    /// Wait (at most 30 s) until an agent it started has set its SIGTERM trap.
    fn trapped(&self) -> R {
        let ready = self.cancels.with_extension("ready");
        let end = Instant::now() + Duration::from_secs(30);
        while !std::fs::read_to_string(&ready).is_ok_and(|r| r.ends_with('\n')) {
            if Instant::now() > end {
                return Err("the agent never set its SIGTERM trap".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

impl Drop for StuckExecutor {
    fn drop(&mut self) {
        for pid in self.started.lock().map(|p| p.clone()).unwrap_or_default() {
            if let Some(pgid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
            }
        }
    }
}

/// Records, inside each round that dispatches (its outbox written, before its commit), which processes are left in
/// the group of the process it watches (M2T05R-51: the group scanned at the grant itself).
#[derive(Debug, Default)]
struct GrantProbe {
    pid: std::sync::Mutex<Option<u32>>,
    seen: std::sync::Mutex<Vec<Value>>,
}

impl FaultHooks for GrantProbe {
    fn at(&self, job: &'static str, point: WritePoint) {
        if job == "Round" && point == WritePoint::OutboxWritten {
            let pid = self.pid.lock().ok().and_then(|p| *p);
            if let (Some(pid), Ok(mut seen)) = (pid, self.seen.lock()) {
                seen.push(group_now(pid));
            }
        }
    }
}

/// AC-041-02's world: run R RUNNING with two writer tasks READY after T0, whose attempt A0 settled holding the
/// writer lease at epoch 3 (so T1's attempt gets epoch 4, the next grant 5).
const WRITERS: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T0', 'R', 's2', 'ACCEPTED', 5, 'developer', 1, 1, 1), ('T1', 'R', 's2', 'READY', 1, 'developer', 1, 2, 1),
  ('T2', 'R', 's2', 'READY', 1, 'developer', 1, 3, 1);
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, ended_at) VALUES
  ('A0', 'T0', 'SETTLED', 2, 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES (:p, 'writer', 'A0', 3, '2026-11-07T00:00:00Z');
";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settle_whose_native_cancel_reached_no_turn_waits_only_the_sigterm_grace() -> R {
    // M2T07R-453 (docs/adr/ADR-022.md decision 17): an expired lease whose executor no longer holds the turn (its
    // native cancel reaches nothing) is settled with SIGTERM after the SIGTERM grace, not the longer native one, and
    // its step says no native cancel was sent.
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(StuckExecutor {
        cancels: dir.path().join("cancels"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: false,
    });
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.reconcile.native_grace = Duration::from_millis(1500);
    config.reconcile.term_grace = Duration::from_millis(300);
    config.reconcile.kill_grace = Duration::from_secs(5);
    let developer = ["structured_events", "command_exec"].map(str::to_owned);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: Context {
            required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                .into(),
            ..Context::default()
        },
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT a.state || ':' || l.epoch FROM attempts a JOIN leases l ON l.attempt_id = a.attempt_id \
         WHERE a.task_id = 'T1' AND l.kind = 'writer'",
        "RUNNING:4",
        Duration::from_secs(30)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    // The reaper's SIGTERM only once the agent marks it (convention v2).
    executor.trapped()?;
    clock.advance(Duration::from_secs(300));
    assert!(wait_for(
        &db,
        &format!(
            "SELECT count(*) FROM events WHERE kind = 'lease.reaped' AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
        ),
        "1",
        Duration::from_secs(30)
    )?);
    let sent = query(
        &db,
        &format!(
            "SELECT json_extract(payload_json, '$.sent') FROM events WHERE kind = 'lease.native_cancel' \
             AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
        ),
    )?;
    let first = |file: &str| -> R<u128> {
        Ok(std::fs::read_to_string(dir.path().join(file))?
            .lines()
            .next()
            .ok_or(format!("nothing in {file}"))?
            .trim()
            .parse()?)
    };
    let gap = term_at(&dir.path().join("cancels.term"))?.saturating_sub(first("cancels.at")?);
    host.shutdown().await;
    drop(executor);
    assert_eq!(sent, "0", "the step says a native cancel was sent");
    assert!(
        gap < 1_200_000_000,
        "SIGTERM {gap} ns after a native cancel that reached nothing"
    );
    Ok(())
}

/// AC-041-02: writer lease epoch 4 held by A1, whose agent's process group runs and ignores SIGTERM; the clock is
/// moved past its expiry. The runner inspects, sends the native cancel, SIGTERM, SIGKILL, confirms the reap, and only
/// then is epoch 5 granted, with no process of A1's group left; A1's late TaskResult is STALE_LEASE_EPOCH with the
/// task's revision unchanged and one history event; a write of the old host epoch changes 0 rows and is an
/// IntegrityViolation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_041_02_an_expired_lease_is_granted_again_only_after_its_holder_is_reaped() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(StuckExecutor {
        cancels: dir.path().join("cancels"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: true,
    });
    let grant_probe = Arc::new(GrantProbe::default());
    let mut config = config(&project);
    config.store.faults = grant_probe.clone();
    config.clock = hoh_app::host::HostClock(clock.clone());
    // The native cancel's grace is the configured one, not SIGTERM's (M2T07R-254).
    config.reconcile.native_grace = Duration::from_millis(1500);
    config.reconcile.term_grace = Duration::from_millis(300);
    config.reconcile.kill_grace = Duration::from_secs(5);
    let developer = ["structured_events", "command_exec"].map(str::to_owned);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: Context {
            required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                .into(),
            ..Context::default()
        },
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    let scalar = |sql: &str| -> R<String> {
        Ok(python(
            &db,
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
            sql,
        )?
        .trim()
        .to_owned())
    };
    let until = |sql: &str, want: &str| -> R<bool> {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            if scalar(sql)? == want {
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(false)
    };
    // A1: T1's attempt, holding writer epoch 4, running its agent.
    assert!(until(
        "SELECT a.state || ':' || l.epoch FROM attempts a JOIN leases l ON l.attempt_id = a.attempt_id \
         WHERE a.task_id = 'T1' AND l.kind = 'writer'",
        "RUNNING:4"
    )?);
    let a1 = scalar("SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    let pid: u32 = scalar(&format!(
        "SELECT pid FROM attempts WHERE attempt_id = '{a1}'"
    ))?
    .parse()?;
    let t1_revision = scalar("SELECT revision FROM tasks WHERE task_id = 'T1'")?;
    if let Ok(mut watched) = grant_probe.pid.lock() {
        *watched = Some(pid);
    }
    executor.trapped()?;
    clock.advance(Duration::from_secs(300));
    // The next grant: epoch 5, after A1's chain.
    assert!(until(
        "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
         AND json_extract(payload_json, '$.lease.epoch') = 5",
        "1"
    )?);
    let left = group_now(pid);
    let read: Value = serde_json::from_str(&python(
        &db,
        &format!(
            r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
steps = [[r[0], r[1]] for r in c.execute(
    "SELECT seq, kind FROM events WHERE kind LIKE 'lease.%' AND json_extract(payload_json, '$.attempt_id') = '{a1}' ORDER BY seq")]
grant = c.execute("SELECT event_seq, json_extract(payload_json, '$.attempt_id') FROM outbox WHERE json_extract(payload_json, '$.lease.epoch') = 5").fetchone()
disposed = [json.loads(r[0]) for r in c.execute(
    "SELECT payload_json FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.attempt_id') = '{a1}'")]
print(json.dumps({{"steps": steps, "grant": list(grant), "disposed": disposed,
                  "a1": c.execute("SELECT state FROM attempts WHERE attempt_id = '{a1}'").fetchone()[0]}}))
"#
        ),
        "",
    )?)?;
    let kinds: Vec<&str> = read["steps"]
        .as_array()
        .ok_or("no steps")?
        .iter()
        .filter_map(|s| s[1].as_str())
        .collect();
    assert_eq!(
        kinds,
        [
            "lease.inspected",
            "lease.native_cancel",
            "lease.sigterm",
            "lease.sigkill",
            "lease.reaped"
        ],
        "{read}"
    );
    let last_step = read["steps"][4][0].as_i64().ok_or("no reap")?;
    assert!(read["grant"][0].as_i64() > Some(last_step), "{read}");
    assert_ne!(read["grant"][1], json!(a1));
    assert_eq!(left, nothing_left(), "A1's group");
    // At the grant itself, inside its transaction: no process of A1's group left (the only dispatching round since).
    let at_grant = grant_probe
        .seen
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();
    assert_eq!(at_grant, [nothing_left()], "{at_grant:?}");
    assert_eq!(
        std::fs::read_to_string(dir.path().join("cancels"))?.trim(),
        a1
    );
    // SIGTERM came no sooner than the native cancel's grace after it.
    let first = |file: &str| -> R<u128> {
        Ok(std::fs::read_to_string(dir.path().join(file))?
            .lines()
            .next()
            .ok_or(format!("nothing in {file}"))?
            .trim()
            .parse()?)
    };
    let gap = term_at(&dir.path().join("cancels.term"))?.saturating_sub(first("cancels.at")?);
    assert!(
        gap >= 1_400_000_000,
        "SIGTERM {gap} ns after the native cancel"
    );
    assert_eq!(read["disposed"][0]["cause"], json!("lease_expired"));
    // A1's late result: refused, the task's revision unchanged, one history event.
    let late: hoh_core::protocol::TaskResult = serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T1", "attempt_id": a1, "role": "developer",
        "role_epoch": 2, "status": "COMPLETED", "summary": "late", "candidate_id": null, "evidence": [],
        "issues": [], "next_action": "verify"
    }))?;
    let outcome = host.store().accept_task_result(late).await?;
    assert!(
        matches!(
            outcome,
            hoh_core::persistence::TaskResultOutcome::Rejected {
                reason: hoh_core::protocol::RejectionReason::StaleLeaseEpoch,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(
        scalar("SELECT revision FROM tasks WHERE task_id = 'T1'")?,
        t1_revision
    );
    assert_eq!(
        scalar(&format!(
            "SELECT count(*) FROM events WHERE kind = 'task_result.rejected' AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
        ))?,
        "1"
    );
    // A newer host began on the database: the old host's next write changes nothing.
    let newer = hoh_core::persistence::Store::open(
        &db,
        hoh_core::persistence::Options {
            wal_probe: Some(probe()),
            ..hoh_core::persistence::Options::default()
        },
    )?;
    let repo = std::fs::canonicalize(&project)?;
    newer.begin_host("unused", &repo.to_string_lossy()).await?;
    let events = scalar("SELECT count(*) FROM events")?;
    let refused = host
        .store()
        .commit(Commit {
            events: vec![NewEvent {
                project_id: host.project_id.clone(),
                event_id: "old-host-write".to_owned(),
                run_id: Some("R".to_owned()),
                kind: "test.write".to_owned(),
                payload: json!({}),
            }],
            ..Commit::default()
        })
        .await
        .expect_err("fenced");
    assert!(
        matches!(&refused, hoh_core::persistence::PersistError::IntegrityViolation(m) if m.contains("0 rows")),
        "{refused:?}"
    );
    assert_eq!(scalar("SELECT count(*) FROM events")?, events);
    keep(
        "test-041-02.json",
        &json!({"a1": a1, "pid": pid, "group_at_grant": at_grant, "group_left_after_grant": left, "read": read,
                "late_result": format!("{outcome:?}"),
                "old_host_write": refused.to_string()}),
    )?;
    drop(newer);
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// `git` in `dir`, its stdout.
fn git_in(dir: &Path, args: &[&str]) -> R<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_writer_is_granted_again_only_once_the_workspace_it_left_is_recorded() -> R {
    // REQ-019 behaviour 6 (docs/adr/ADR-024.md decision 9, M2T17R-52), the settle's path: AC-041-02's world in a git
    // work tree, its agent appending to a tracked file at its SIGTERM. Its lease expires, it is reaped and its effect
    // is unknown: it ends UNKNOWN_EFFECT with the workspace's record, taken after the stop, in the same transaction
    // as its end and its lease's release, before the next writer's grant.
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    git_in(&project, &["init", "-q"])?;
    std::fs::write(
        project.join(".gitignore"),
        ".hoh/\nwork\nwork.at\nwork.ready\n",
    )?;
    std::fs::write(project.join("work.term"), "")?;
    git_in(&project, &["add", ".gitignore", "work.term"])?;
    git_in(
        &project,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "-m",
            "first",
        ],
    )?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    // Its SIGTERM mark goes to `work.term`, tracked: what the agent did at its stop is in the workspace's diff.
    let executor = Arc::new(StuckExecutor {
        cancels: project.join("work"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: true,
    });
    let mut config = config(&project);
    config.clock = hoh_app::host::HostClock(clock.clone());
    config.reconcile.native_grace = Duration::from_millis(300);
    config.reconcile.term_grace = Duration::from_millis(300);
    config.reconcile.kill_grace = Duration::from_secs(5);
    let developer = ["structured_events", "command_exec"].map(str::to_owned);
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: executor.clone(),
        context: Context {
            required_capabilities: [("developer".to_owned(), developer.into_iter().collect())]
                .into(),
            ..Context::default()
        },
    });
    let host = Host::start(config).await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT a.state || ':' || l.epoch FROM attempts a JOIN leases l ON l.attempt_id = a.attempt_id \
         WHERE a.task_id = 'T1' AND l.kind = 'writer'",
        "RUNNING:4",
        Duration::from_secs(30)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    executor.trapped()?;
    clock.advance(Duration::from_secs(300));
    assert!(wait_for(
        &db,
        "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch' \
         AND json_extract(payload_json, '$.lease.epoch') = 5",
        "1",
        Duration::from_secs(30)
    )?);
    host.shutdown().await;
    drop(executor);
    let read: Value = serde_json::from_str(&python(
        &db,
        &format!(
            r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1])
record = c.execute("SELECT seq, payload_json FROM events WHERE kind = 'attempt.workspace' AND json_extract(payload_json, '$.attempt_id') = '{a1}'").fetchall()
ended = c.execute("SELECT seq FROM events WHERE kind = 'attempt.moved' AND json_extract(payload_json, '$.attempt_id') = '{a1}' AND json_extract(payload_json, '$.to') = 'UNKNOWN_EFFECT'").fetchone()
grant = c.execute("SELECT event_seq FROM outbox WHERE json_extract(payload_json, '$.lease.epoch') = 5").fetchone()
print(json.dumps({{"records": [[r[0], json.loads(r[1])] for r in record], "ended": ended[0] if ended else None,
                  "grant": grant[0] if grant else None,
                  "a1": c.execute("SELECT state FROM attempts WHERE attempt_id = '{a1}'").fetchone()[0]}}))
"#
        ),
        "",
    )?)?;
    assert_eq!(read["a1"], "UNKNOWN_EFFECT", "{read}");
    let records = read["records"].as_array().ok_or("no records")?;
    assert_eq!(records.len(), 1, "{read}");
    let (seq, record) = (records[0][0].as_i64(), &records[0][1]);
    // In the end's own transaction (the end's event right after it), before the next writer's grant.
    assert_eq!(seq.map(|s| s + 1), read["ended"].as_i64(), "{read}");
    assert!(seq < read["grant"].as_i64(), "{read}");
    assert_eq!(record["to"], "UNKNOWN_EFFECT", "{record}");
    let workspace = &record["workspace"];
    assert_eq!(
        workspace["head"],
        json!(git_in(&project, &["rev-parse", "HEAD"])?.trim()),
        "{record}"
    );
    assert_eq!(
        workspace["head_tree"],
        json!(git_in(&project, &["rev-parse", "HEAD^{tree}"])?.trim()),
        "{record}"
    );
    // Taken after the stop: the agent's write at its SIGTERM is in it.
    assert_eq!(
        std::fs::read_to_string(project.join("work.term"))?,
        "term\n"
    );
    let diff = Command::new("git")
        .args(["diff", "--binary", "--no-ext-diff", "--no-textconv", "HEAD"])
        .current_dir(&project)
        .output()?
        .stdout;
    let digest = <sha2::Sha256 as sha2::Digest>::digest(&diff);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(workspace["diff_sha256"], json!(hex), "{record}");
    Ok(())
}

/// The developer's required capabilities, for a runner's rounds.
fn developer_context() -> Context {
    let developer = ["structured_events", "command_exec"].map(str::to_owned);
    Context {
        required_capabilities: [("developer".to_owned(), developer.into_iter().collect())].into(),
        ..Context::default()
    }
}

/// A host of this process (CR-006 F3) for `project`: a runner on `executor`, the fake clock, short stop graces, and
/// `faults` as its store's fault hooks.
async fn runner_host(
    project: &Path,
    executor: Arc<dyn hoh_app::host::Executor>,
    clock: Arc<FakeClock>,
    faults: Option<Arc<dyn FaultHooks>>,
) -> R<Host> {
    runner_host_with(
        project,
        executor,
        clock,
        Setup {
            faults,
            ..Setup::default()
        },
    )
    .await
}

/// What a [`runner_host_with`] adds to a runner host.
#[derive(Default)]
struct Setup {
    faults: Option<Arc<dyn FaultHooks>>,
    observer: Option<Arc<dyn hoh_core::reconcile::Observer>>,
    retry: Option<Duration>,
    hooks: Option<Arc<dyn hoh_app::host::HostHooks>>,
}

/// [`runner_host`] with what `setup` adds.
async fn runner_host_with(
    project: &Path,
    executor: Arc<dyn hoh_app::host::Executor>,
    clock: Arc<FakeClock>,
    setup: Setup,
) -> R<Host> {
    let mut config = config(project);
    if let Some(observer) = setup.observer {
        config.reconcile.observer = observer;
    }
    if let Some(retry) = setup.retry {
        config.reconcile.retry = retry;
    }
    if let Some(hooks) = setup.hooks {
        config.hooks = hooks;
    }
    config.clock = hoh_app::host::HostClock(clock);
    config.reconcile.native_grace = Duration::from_millis(300);
    config.reconcile.term_grace = Duration::from_millis(300);
    config.reconcile.kill_grace = Duration::from_secs(5);
    if let Some(faults) = setup.faults {
        config.store.faults = faults;
    }
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor,
        context: developer_context(),
    });
    Ok(Host::start(config).await?)
}

/// The first column of the first row of `sql` ("" for none or NULL), read outside the product.
fn query(db: &Path, sql: &str) -> R<String> {
    Ok(python(
        db,
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); print('' if r is None or r[0] is None else r[0])",
        sql,
    )?
    .trim()
    .to_owned())
}

/// Wait until `sql` reads `want`, at most `limit`.
fn wait_for(db: &Path, sql: &str, want: &str, limit: Duration) -> R<bool> {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if query(db, sql)? == want {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(false)
}

/// Kill whatever is left in process group `pgid` (test cleanup).
fn kill_group(pgid: u32) {
    if let Some(pgid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pgid, rustix::process::Signal::KILL);
    }
}

/// Kills a process group when dropped, however the test ends.
struct GroupGuard(u32);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        kill_group(self.0);
    }
}

/// M2T05R-02: a recorded leader that exited and left a member in its group (which logs every catchable signal). Its
/// lease expired and another writer task waits. Neither the reconcile at the start nor the runner's expiry can tell
/// whether the member is the attempt's: no signal, the attempt and its slot stay, no writer lease is granted, and a
/// diagnostic says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leaderless_group_keeps_its_slot_and_gets_no_signal() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let log = dir.path().join("signals.log");
    let member = dir.path().join("member.sh");
    std::fs::write(
        &member,
        format!(
            "for s in TERM INT HUP; do trap \"echo $s >> {}\" $s; done\necho $$ > {}\nwhile :; do sleep 0.05; done\n",
            log.display(),
            log.with_extension("ready").display()
        ),
    )?;
    let out = Command::new("sh")
        .args([
            "-c",
            &format!(
                "setsid sh -c 'sh {} >/dev/null 2>&1 & echo $$; exit 0' </dev/null 2>/dev/null & wait",
                member.display()
            ),
        ])
        .output()?;
    let leader: u32 = String::from_utf8_lossy(&out.stdout).trim().parse()?;
    let _guard = GroupGuard(leader);
    // The member, once its traps are set, with its start time for the end's look (CIR-150, -151).
    let member_pid = trapped_at(&log.with_extension("ready"))?;
    let member_start =
        hoh_agents::identity::start_id(member_pid)?.ok_or("the member is not running")?;
    let end = Instant::now() + Duration::from_secs(5);
    while hoh_agents::identity::start_id(leader)?.is_some() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!hoh_agents::identity::group_empty(leader));
    seed(
        &project,
        &format!(
            "{WRITERS}
             UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'T1';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at) VALUES
               ('A1', 'T1', 'RUNNING', 2, 1, 1, {leader}, '{boot}', '1', '2026-11-07T00:00:00Z');
             UPDATE leases SET attempt_id = 'A1', epoch = 4 WHERE kind = 'writer';",
            boot = hoh_agents::identity::boot_id()?,
        ),
    )?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let clock = Arc::new(FakeClock::default());
    // Looked at again every 200 ms: an unchanged finding is not written again (M2T05R-157, -253).
    let host = runner_host_with(
        &project,
        Arc::new(FakeExecutor(work.clone())),
        clock,
        Setup {
            retry: Some(Duration::from_millis(200)),
            ..Setup::default()
        },
    )
    .await?;
    let db = db(&project);
    // The reconcile's diagnostic, then a few runner ticks with the lease expired.
    assert!(wait_for(
        &db,
        "SELECT count(*) > 0 FROM events WHERE kind = 'reconcile.diagnostic' AND json_extract(payload_json, '$.attempt_id') = 'A1'",
        "1",
        Duration::from_secs(10)
    )?);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
        "RUNNING"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T2'")?,
        "0"
    );
    assert_eq!(
        query(
            &db,
            "SELECT attempt_id || ':' || epoch FROM leases WHERE kind = 'writer'"
        )?,
        "A1:4"
    );
    // Every pass (the reconcile's, then the runner's every RETRY) found it uncertain, and none moved it.
    assert_eq!(
        query(
            &db,
            "SELECT group_concat(DISTINCT json_extract(payload_json, '$.process.found') || '/' || json_extract(payload_json, '$.disposition')) \
             FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.attempt_id') = 'A1'",
        )?,
        "uncertain/unreaped"
    );
    assert!(!hoh_agents::identity::group_empty(leader));
    assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "");
    assert!(
        !hoh_agents::identity::ended(member_pid, Some(&member_start))?,
        "the member was killed"
    );
    // One settle written for its expired lease over many retries.
    assert_eq!(
        query(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'lease.inspected' AND json_extract(payload_json, '$.attempt_id') = 'A1'"
        )?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// Runs each attempt's work as a gated FakeAgent whose start does not return before the file `0` exists (M2T05R-03,
/// -153: a latch the test opens, not a fixed time).
#[derive(Debug)]
struct SlowStart(PathBuf, FakeExecutor);

impl hoh_app::host::Executor for SlowStart {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.1.deliver(work)
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let end = Instant::now() + Duration::from_secs(30);
        while !self.0.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.1.start(work)
    }
}

/// Opens the latch `0` when a settle begins (CR-006: a test's hook at a no-op point; M2T05R-256).
#[derive(Debug)]
struct OpenAtSettle(PathBuf);

impl hoh_app::host::HostHooks for OpenAtSettle {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == hoh_app::host::RunnerPoint::Settling {
            let _ = std::fs::write(&self.0, "");
        }
    }
}

/// M2T05R-03: A1's lease expires while its start is still in progress. The settle waits for A1's task, so the process
/// that start made never passes its gate (A1's work never runs) before T2 is granted the writer lease.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settle_waits_for_a_start_still_in_progress() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let latch = dir.path().join("latch");
    let clock = Arc::new(FakeClock::default());
    let host = runner_host_with(
        &project,
        Arc::new(SlowStart(latch.clone(), FakeExecutor(work.clone()))),
        clock.clone(),
        Setup {
            hooks: Some(Arc::new(OpenAtSettle(latch.clone()))),
            ..Setup::default()
        },
    )
    .await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "STARTING",
        Duration::from_secs(10)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    clock.advance(Duration::from_secs(300));
    // A1's start is held until its settle begins (the hook opens the latch then).
    assert!(wait_for(
        &db,
        "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.lease.epoch') = 5",
        "1",
        Duration::from_secs(20)
    )?);
    // T2's agent runs; A1's never did.
    assert!(wait_for(
        &db,
        "SELECT state FROM tasks WHERE task_id = 'T2'",
        "IMPLEMENTED",
        Duration::from_secs(20)
    )?);
    let counted = std::fs::read_to_string(work.join("counter")).unwrap_or_default();
    assert!(!counted.contains(&a1), "{a1} ran: {counted}");
    let state = query(
        &db,
        &format!("SELECT state FROM attempts WHERE attempt_id = '{a1}'"),
    )?;
    assert!(
        ["UNKNOWN_EFFECT", "CANCELLED"].contains(&state.as_str()),
        "{state}"
    );
    host.shutdown().await;
    Ok(())
}

/// Starts each attempt's work as a gated agent that never ends, then loses track of it: its completion sender is
/// dropped at once (it is still reaped here, so it leaves no zombie).
#[derive(Debug)]
struct Losing(StuckExecutor);

impl hoh_app::host::Executor for Losing {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.0.deliver(work)
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let started = self.0.start(work)?;
        let (_lost, ended) = tokio::sync::oneshot::channel();
        Ok(hoh_app::host::Started { ended, ..started })
    }
}

/// M2T05R-04: the executor loses A1's process. The runner does not take that as a failure to run again: it settles
/// A1 (inspect, stop, reap) and, its work not confirmed, makes it UNKNOWN_EFFECT; T1 gets no new attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_completion_is_settled_never_run_again() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(Losing(StuckExecutor {
        cancels: dir.path().join("cancels"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: true,
    }));
    let host = runner_host(&project, executor.clone(), clock, None).await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "UNKNOWN_EFFECT",
        Duration::from_secs(20)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    let pid: u32 = query(
        &db,
        &format!("SELECT pid FROM attempts WHERE attempt_id = '{a1}'"),
    )?
    .parse()?;
    assert_eq!(group_now(pid), nothing_left());
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T1'")?,
        "1"
    );
    assert_eq!(
        query(
            &db,
            &format!(
                "SELECT count(*) FROM events WHERE kind = 'runner.inspected' AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
            )
        )?,
        "1"
    );
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// Fails the commit of the `nth` plain Commit request (counted from 1) after it is armed (M2T05R-06: a settle whose
/// steps cannot be written).
#[derive(Debug, Default)]
struct FailNthCommit {
    armed: std::sync::atomic::AtomicBool,
    seen: std::sync::atomic::AtomicU32,
    nth: u32,
    fail: std::sync::atomic::AtomicBool,
}

impl FaultHooks for FailNthCommit {
    fn at(&self, job: &'static str, point: WritePoint) {
        use std::sync::atomic::Ordering;
        if job == "Commit"
            && point == WritePoint::OutboxWritten
            && self.armed.load(Ordering::SeqCst)
        {
            let n = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
            if n == self.nth {
                self.fail.store(true, Ordering::SeqCst);
            }
        }
    }

    fn before_commit(&self) -> Result<(), hoh_core::persistence::PersistError> {
        if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(hoh_core::persistence::PersistError::Full(
                "injected: a settle step cut short".to_owned(),
            ));
        }
        Ok(())
    }
}

/// M2T05R-06, -452, -552, -651: A1's lease expires with the fault armed for the `nth` plain commit after that. nth 1:
/// the settle's pass's one commit fails, so that pass leaves no record (its stop was done) and the next pass, with
/// event ids of its own, finds A1's process gone (two commits in all). nth 2: a second commit, which a pass would
/// make only if it split its steps, so the fault never fires: the pass is recorded whole by exactly one commit.
/// Either way the chain ends and T2 is granted epoch 5.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_settle_cut_short_is_done_again_by_another_pass() -> R {
    for nth in 1..=2 {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{stderr}");
        seed(&project, WRITERS)?;
        let clock = Arc::new(FakeClock::default());
        let executor = Arc::new(StuckExecutor {
            cancels: dir.path().join("cancels"),
            started: std::sync::Mutex::new(Vec::new()),
            holds: true,
        });
        let faults = Arc::new(FailNthCommit {
            nth,
            ..FailNthCommit::default()
        });
        let host = runner_host(
            &project,
            executor.clone(),
            clock.clone(),
            Some(faults.clone()),
        )
        .await?;
        let db = db(&project);
        assert!(wait_for(
            &db,
            "SELECT state FROM attempts WHERE task_id = 'T1'",
            "RUNNING",
            Duration::from_secs(10)
        )?);
        let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
        faults
            .armed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        clock.advance(Duration::from_secs(300));
        assert!(wait_for(
            &db,
            "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.lease.epoch') = 5",
            "1",
            Duration::from_secs(20)
        )?);
        let steps = query(
            &db,
            &format!(
                "SELECT group_concat(kind, ',') FROM (SELECT kind FROM events WHERE kind LIKE 'lease.%' AND json_extract(payload_json, '$.attempt_id') = '{a1}' ORDER BY seq)"
            ),
        )?;
        let found = query(
            &db,
            &format!(
                "SELECT group_concat(json_extract(payload_json, '$.found'), ';') FROM events WHERE kind = 'lease.inspected' AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
            ),
        )?;
        // The plain commits made since the arming: one per pass.
        let commits = faults.seen.load(std::sync::atomic::Ordering::SeqCst);
        if nth == 1 {
            assert_eq!(commits, 2, "nth={nth}");
            assert_eq!(steps, "lease.inspected,lease.native_cancel", "nth={nth}");
            assert_eq!(found, r#"{"gone":"no_process"}"#, "nth={nth}");
        } else {
            assert_eq!(commits, 1, "nth={nth}");
            assert_eq!(
                steps,
                "lease.inspected,lease.native_cancel,lease.sigterm,lease.sigkill,lease.reaped",
                "nth={nth}"
            );
            assert_eq!(found, "ours", "nth={nth}");
        }
        host.shutdown().await;
        drop(executor);
    }
    Ok(())
}

/// Kill a host at `point` of the runner `rounds` times (the FakeAgent failing each task once when `fail_once`),
/// restart it at once and run to the next stable point; the state read after each run (READ_RUN) and its counter.
fn kill_and_restart(point: &str, rounds: u32, fail_once: bool) -> R<Vec<(Value, String)>> {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, FIVE_TASKS)?;
    let template = dir.path().join("template");
    copy_db(&project.join(".hoh"), &template)?;
    let decision = start_run().to_string();
    let mut runs = Vec::new();
    for round in 0..rounds {
        copy_db(&template, &project.join(".hoh"))?;
        let work = dir.path().join(format!("work-{round}"));
        std::fs::create_dir_all(work.join("markers"))?;
        if fail_once {
            std::fs::write(work.join("fail-once"), "")?;
        }
        let killed = ChildGuard(
            child(&project)?
                .env(CHILD_RUNNER_KILL, point)
                .env(CHILD_WORK, &work)
                .env(CHILD_DECIDE, &decision)
                .spawn()?,
        );
        let (status, stderr) = wait(killed, Duration::from_secs(90))?;
        assert_eq!(
            status.signal(),
            Some(9),
            "{point} #{round}: {status:?}: {stderr}"
        );
        let db = db(&project);
        let (code, stderr, stable) = serve_until(
            &project,
            &[(CHILD_WORK, work.as_os_str())],
            Duration::from_secs(60),
            || {
                python(&db, &format!("import sqlite3,sys; print(sqlite3.connect(sys.argv[1]).execute({STABLE:?}).fetchone()[0])"), "")
                    .is_ok_and(|n| n.trim() == "0")
            },
        )?;
        assert_eq!(code, 0, "{point} #{round}: {stderr}");
        assert!(stable, "{point} #{round}: no stable point: {stderr}");
        let read: Value = serde_json::from_str(&python(&db, READ_RUN, "")?)?;
        runs.push((
            read,
            std::fs::read_to_string(work.join("counter")).unwrap_or_default(),
        ));
    }
    Ok(runs)
}

/// M2T05R-01, -52: a host killed after it started an agent's process (held at its start gate) but before it recorded
/// the process's identity. The gate never opens, so the agent never runs; the restarted host ends that attempt as never
/// executed (CANCELLED) and the task runs once in another attempt. No effect key is counted twice, and none of an
/// attempt that never passed its gate.
#[test]
fn a_host_killed_between_spawn_and_identity_never_runs_the_agent() -> R {
    for (round, (read, counted)) in kill_and_restart("AfterSpawn", 5, false)?
        .into_iter()
        .enumerate()
    {
        let killed_epoch = read["host_epoch"].as_i64().ok_or("no epoch")? - 1;
        let cancelled: Vec<&str> = read["attempts"]
            .as_array()
            .ok_or("no attempts")?
            .iter()
            .filter(|a| a["host_epoch"].as_i64() == Some(killed_epoch) && a["state"] == "CANCELLED")
            .filter_map(|a| a["id"].as_str())
            .collect();
        assert!(!cancelled.is_empty(), "#{round}: {read}");
        for id in cancelled {
            assert!(!counted.contains(id), "#{round}: {id} ran: {counted}");
        }
        let mut keys: Vec<&str> = counted.lines().collect();
        let all = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all, "#{round}: {counted}");
        assert!(
            read["tasks"]
                .as_object()
                .is_some_and(|t| t.values().all(|s| s == "IMPLEMENTED")),
            "#{round}: {read}"
        );
    }
    Ok(())
}

/// M2T05R-05, -53: the work of the running attempts fails and the failure is committed; the host is killed before the
/// attempts end. The restarted host disposes of them by the recorded outcome: FAILED, their tasks READY (not SETTLED
/// and IMPLEMENTED); each task then runs again and is done.
#[test]
fn a_failure_committed_before_a_crash_stays_a_failure() -> R {
    for (round, (read, _)) in kill_and_restart("AfterEffectCommitted", 3, true)?
        .into_iter()
        .enumerate()
    {
        let killed_epoch = read["host_epoch"].as_i64().ok_or("no epoch")? - 1;
        let old: Vec<&Value> = read["attempts"]
            .as_array()
            .ok_or("no attempts")?
            .iter()
            .filter(|a| a["host_epoch"].as_i64() == Some(killed_epoch))
            .collect();
        // Every first run fails: none of the killed host's attempts may settle.
        assert!(
            old.iter().all(|a| a["state"] != "SETTLED"),
            "#{round}: {read}"
        );
        // Each attempt the reconcile ended moved by its recorded state (M2T05R-550): from RUNNING, where its work's
        // failure was committed, to FAILED; from STARTING, where its work never started (one can be STARTING when
        // the host is killed), to CANCELLED. Anything else, or an attempt left in another state, is a defect; the
        // attempt killed after its failure was committed is among the first.
        let mut failed = 0;
        for event in read["reconcile"]
            .as_array()
            .ok_or("no reconcile")?
            .iter()
            .filter(|e| e["kind"] == "reconcile.attempt" && e["payload"]["disposition"] == "ended")
        {
            let id = event["payload"]["attempt_id"]
                .as_str()
                .ok_or("no attempt id")?;
            let moved = (
                event["payload"]["attempt"]["from"].as_str(),
                event["payload"]["attempt"]["to"].as_str(),
            );
            match moved {
                (Some("RUNNING"), Some("FAILED")) => failed += 1,
                (Some("STARTING"), Some("CANCELLED")) => {}
                other => return Err(format!("#{round}: {id} moved {other:?}: {read}").into()),
            }
            assert!(
                old.iter()
                    .any(|a| a["id"] == id && a["state"].as_str() == moved.1),
                "#{round}: {id}: {read}"
            );
        }
        assert!(failed >= 1, "#{round}: {read}");
        // Each task is done in a later attempt, but one left with an attempt UNKNOWN_EFFECT (killed mid-run).
        let unknown: Vec<&str> = read["attempts"]
            .as_array()
            .ok_or("no attempts")?
            .iter()
            .filter(|a| a["state"] == "UNKNOWN_EFFECT")
            .filter_map(|a| a["task"].as_str())
            .collect();
        for (task, state) in read["tasks"].as_object().ok_or("no tasks")? {
            assert!(
                state == "IMPLEMENTED" || unknown.contains(&task.as_str()),
                "#{round}: {task} {state}"
            );
        }
    }
    Ok(())
}

/// M2T05R-01: a RUNNING attempt whose process was never recorded (only possible outside the gated runner) cannot be
/// shown gone: the reconcile keeps it and its writer slot (no signal; a diagnostic) rather than let another writer in.
#[test]
fn a_running_attempt_with_no_process_record_keeps_its_slot() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(
        &project,
        &format!(
            "{WRITERS}
             UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'T1';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, started_at) VALUES
               ('A1', 'T1', 'RUNNING', 2, 1, 1, '2026-11-07T00:00:00Z');
             UPDATE leases SET attempt_id = 'A1', epoch = 4, expires_at = '2999-01-01T00:00:00Z' WHERE kind = 'writer';"
        ),
    )?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let db = db(&project);
    assert_eq!(
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
        "RUNNING"
    );
    assert_eq!(
        query(
            &db,
            "SELECT attempt_id || ':' || epoch FROM leases WHERE kind = 'writer'"
        )?,
        "A1:4"
    );
    assert_eq!(
        query(
            &db,
            "SELECT json_extract(payload_json, '$.process.found') || '/' || json_extract(payload_json, '$.process.basis') \
             FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.attempt_id') = 'A1'"
        )?,
        "uncertain/running_without_a_process_record"
    );
    Ok(())
}

/// Runs each attempt's work as a gated agent whose leader leaves a member in its group that ignores SIGTERM and lives
/// on, then ends at once with success (M2T05R-101). It arms `probe` with each process it starts (M2T05R-251); a
/// `negligent` one reaps the leader at once without emptying the group (and says so, M2T05R-250). Every group it
/// started is killed when it is dropped (M2T05R-252).
#[derive(Debug)]
struct LeavesAMember {
    inner: FakeExecutor,
    probe: Option<Arc<GrantProbe>>,
    negligent: bool,
    started: std::sync::Mutex<Vec<u32>>,
}

impl hoh_app::host::Executor for LeavesAMember {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.inner.deliver(work)
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        use std::os::unix::process::CommandExt as _;
        let mut child = Command::new("sh")
            .args([
                "-c",
                "read -r go; [ \"$go\" = go ] || exit 0; sh -c 'trap \"\" TERM; exec sleep 1000' </dev/null >/dev/null 2>&1 & exit 0",
            ])
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id();
        if let Ok(mut started) = self.started.lock() {
            started.push(pid);
        }
        if let Some(probe) = &self.probe
            && let Ok(mut watched) = probe.pid.lock()
            && watched.is_none()
        {
            *watched = Some(pid);
        }
        let gate = gate(&mut child);
        let ended = if self.negligent {
            let (end, ended) = tokio::sync::oneshot::channel();
            std::thread::spawn(move || {
                let status = child.wait();
                let _ = end.send(hoh_app::host::Outcome {
                    success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                    detail: format!("{status:?}"),
                    group_gone: hoh_agents::identity::group_empty(pid),
                    class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                    observed: Default::default(),
                    unknown_effect: false,
                });
            });
            ended
        } else {
            reaping(child)
        };
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }
}

impl Drop for LeavesAMember {
    fn drop(&mut self) {
        for pid in self.started.lock().map(|p| p.clone()).unwrap_or_default() {
            kill_group(pid);
        }
    }
}

/// M2T05R-101, -357: A1's leader ends with success but leaves a member in its group that ignores SIGTERM. An executor
/// keeping the contract (`reaping`) kills that group before it reports, so when T2 is granted the writer lease no
/// process of A1's group is left (looked at inside the granting round). This covers the executor's contract; that
/// the runner ends an attempt only on its report is `a_leader_whose_group_was_not_emptied_keeps_its_slot`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_leader_settles_only_once_its_group_is_gone() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let clock = Arc::new(FakeClock::default());
    let probe = Arc::new(GrantProbe::default());
    let executor = Arc::new(LeavesAMember {
        inner: FakeExecutor(work),
        probe: Some(probe.clone()),
        negligent: false,
        started: std::sync::Mutex::new(Vec::new()),
    });
    let host = runner_host(&project, executor.clone(), clock, Some(probe.clone())).await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "SETTLED",
        Duration::from_secs(20)
    )?);
    assert!(wait_for(
        &db,
        "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        "1",
        Duration::from_secs(10)
    )?);
    let pid: u32 = query(&db, "SELECT pid FROM attempts WHERE task_id = 'T1'")?.parse()?;
    let at_grant = probe.seen.lock().map(|s| s.clone()).unwrap_or_default();
    assert!(
        !at_grant.is_empty() && at_grant.iter().all(|g| *g == nothing_left()),
        "{at_grant:?}"
    );
    assert_eq!(group_now(pid), nothing_left());
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// M2T05R-250: an executor that reaps A1's leader without emptying its group (and says so). A1 does not settle: it
/// stays RUNNING with its writer slot, the runner says why (`runner.unreaped`), and T2 is not granted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_leader_whose_group_was_not_emptied_keeps_its_slot() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let executor = Arc::new(LeavesAMember {
        inner: FakeExecutor(work),
        probe: None,
        negligent: true,
        started: std::sync::Mutex::new(Vec::new()),
    });
    let host = runner_host(
        &project,
        executor.clone(),
        Arc::new(FakeClock::default()),
        None,
    )
    .await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT count(*) FROM events WHERE kind = 'runner.unreaped'",
        "1",
        Duration::from_secs(20)
    )?);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        query(&db, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
        "RUNNING"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T2'")?,
        "0"
    );
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// M2T05R-100, -151: holders the runner cannot confirm gone keep their slot past their lease: one recorded with the
/// pid of a live process but no boot or start time, one RUNNING with no process record. Neither is signalled, and the
/// writer task waiting behind them is not granted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_keeps_the_slot_of_a_holder_it_cannot_confirm_gone() -> R {
    for partial in [true, false] {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{stderr}");
        let log = dir.path().join("signals.log");
        let ready = log.with_extension("ready");
        let script = format!(
            "for s in TERM INT HUP; do trap \"echo $s >> '{}'\" $s; done; echo $$ > '{}'; while :; do sleep 0.05; done",
            log.display(),
            ready.display()
        );
        let live = ChildGuard({
            use std::os::unix::process::CommandExt as _;
            Command::new("sh")
                .args(["-c", &script])
                .process_group(0)
                .stdin(std::process::Stdio::null())
                .spawn()?
        });
        // Its traps set before the host starts (CIR-151).
        trapped_at(&ready)?;
        let pid = if partial {
            live.0.id().to_string()
        } else {
            "NULL".to_owned()
        };
        seed(
            &project,
            &format!(
                "{WRITERS}
                 UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'T1';
                 INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, started_at)
                   VALUES ('A1', 'T1', 'RUNNING', 2, 1, 1, {pid}, '2026-11-07T00:00:00Z');
                 UPDATE leases SET attempt_id = 'A1', epoch = 4 WHERE kind = 'writer';"
            ),
        )?;
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("markers"))?;
        let clock = Arc::new(FakeClock::default());
        let host = runner_host(&project, Arc::new(FakeExecutor(work)), clock, None).await?;
        let db = db(&project);
        assert!(wait_for(
            &db,
            "SELECT count(*) > 0 FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'lease_expired'",
            "1",
            Duration::from_secs(10)
        )?);
        tokio::time::sleep(Duration::from_millis(600)).await;
        let basis = if partial {
            "incomplete_process_record"
        } else {
            "running_without_a_process_record"
        };
        assert_eq!(
            query(
                &db,
                "SELECT group_concat(DISTINCT json_extract(payload_json, '$.cause') || ':' || json_extract(payload_json, '$.process.basis') || ':' || json_extract(payload_json, '$.disposition')) \
                 FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.attempt_id') = 'A1' ORDER BY seq"
            )?
            .split(',')
            .collect::<std::collections::BTreeSet<_>>(),
            [format!("reconcile:{basis}:unreaped"), format!("lease_expired:{basis}:unreaped")]
                .iter()
                .map(String::as_str)
                .collect(),
            "partial={partial}"
        );
        assert_eq!(
            query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
            "RUNNING"
        );
        assert_eq!(
            query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T2'")?,
            "0"
        );
        assert_eq!(std::fs::read_to_string(&log).unwrap_or_default(), "");
        // This process's own child: the parent's view (CIR-200).
        assert!(
            !hoh_agents::identity::child_ended(live.0.id())?,
            "partial={partial}: live was killed"
        );
        host.shutdown().await;
        drop(live);
    }
    Ok(())
}

/// M2T06R-600 (docs/adr/ADR-021.md decision 8d v7): a holder recorded with a pid but no boot or start time keeps its
/// slot while its group number is in use (as above), and gives it up once no process has that number: the settle's
/// repeated finding does not stop it from looking again, and the writer task behind it is granted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_runner_frees_the_slot_of_an_unconfirmed_holder_once_its_group_is_gone() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let live = ChildGuard({
        use std::os::unix::process::CommandExt as _;
        Command::new("sh")
            .args(["-c", "while :; do sleep 0.05; done"])
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .spawn()?
    });
    seed(
        &project,
        &format!(
            "{WRITERS}
             UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'T1';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, started_at)
               VALUES ('A1', 'T1', 'RUNNING', 2, 1, 1, {}, '2026-11-07T00:00:00Z');
             UPDATE leases SET attempt_id = 'A1', epoch = 4 WHERE kind = 'writer';",
            live.0.id()
        ),
    )?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let clock = Arc::new(FakeClock::default());
    // Looked at again every 200 ms; an unchanged finding is not written again (M2T05R-157).
    let host = runner_host_with(
        &project,
        Arc::new(FakeExecutor(work)),
        clock,
        Setup {
            retry: Some(Duration::from_millis(200)),
            ..Setup::default()
        },
    )
    .await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT count(*) > 0 FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'lease_expired'",
        "1",
        Duration::from_secs(10)
    )?);
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert_eq!(
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
        "RUNNING",
        "the precondition: held while its group lives"
    );
    drop(live);
    assert!(
        wait_for(
            &db,
            "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
            "1",
            Duration::from_secs(15)
        )?,
        "the slot stayed held after the holder's group was gone: A1 {}",
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?
    );
    assert_ne!(
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
        "RUNNING"
    );
    host.shutdown().await;
    Ok(())
}

/// M2T06R-1054 (docs/adr/ADR-021.md decision 8d v7): a holder whose tracking a host in other namespaces recorded proves
/// nothing here and keeps its slot; its settle is one finding, written once, not at every retry, whatever its record
/// names here (here a pid no process has: gone, were the record this world's).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_recorded_in_another_world_keeps_its_slot_and_its_settle_is_written_once() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let gone = {
        let mut child = Command::new("true").spawn()?;
        let pid = child.id();
        child.wait()?;
        pid
    };
    seed(
        &project,
        &format!(
            "{WRITERS}
             UPDATE tasks SET state = 'RUNNING' WHERE task_id = 'T1';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, pid, boot_id,
               process_start_id, started_at)
               VALUES ('A1', 'T1', 'RUNNING', 2, 1, 1, {gone}, '{boot}', '1', '2026-11-07T00:00:00Z');
             UPDATE leases SET attempt_id = 'A1', epoch = 4 WHERE kind = 'writer';
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES (:p, 'A1/tracking',
               'R', 'attempt.tracking', '{{\"attempt_id\": \"A1\", \"tracking\": \"proc\", \"reason\": \"r\",
               \"world\": {{\"pid\": \"pid:[1]\", \"mnt\": \"mnt:[1]\", \"cgroup\": \"cgroup:[1]\",
               \"user\": \"user:[1]\"}}}}', '2026-11-07T00:00:00Z');",
            boot = hoh_agents::identity::boot_id()?,
        ),
    )?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let clock = Arc::new(FakeClock::default());
    let host = runner_host_with(
        &project,
        Arc::new(FakeExecutor(work)),
        clock,
        Setup {
            retry: Some(Duration::from_millis(200)),
            ..Setup::default()
        },
    )
    .await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT count(*) > 0 FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'lease_expired'",
        "1",
        Duration::from_secs(10)
    )?);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let settles = query(
        &db,
        "SELECT count(*) FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'lease_expired'",
    )?;
    assert_eq!(
        query(&db, "SELECT state FROM attempts WHERE attempt_id = 'A1'")?,
        "RUNNING"
    );
    assert_eq!(
        query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T2'")?,
        "0"
    );
    assert_eq!(settles, "1", "a settle written at every retry");
    host.shutdown().await;
    Ok(())
}

/// Fails the commit of the `nth` write of the runner (counted at each Runner request's Begun point) once armed.
#[derive(Debug)]
struct FailNthRunnerWrite {
    nth: u32,
    seen: std::sync::atomic::AtomicU32,
    fail: std::sync::atomic::AtomicBool,
}

impl FaultHooks for FailNthRunnerWrite {
    fn at(&self, job: &'static str, point: WritePoint) {
        use std::sync::atomic::Ordering;
        if job == "Runner"
            && point == WritePoint::Begun
            && self.seen.fetch_add(1, Ordering::SeqCst) + 1 == self.nth
        {
            self.fail.store(true, Ordering::SeqCst);
        }
    }

    fn before_commit(&self) -> Result<(), hoh_core::persistence::PersistError> {
        if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
            return Err(hoh_core::persistence::PersistError::Full(
                "injected: a runner write failed".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Holds each settle at its start for `.0`, long enough for the runner to tick meanwhile (M2T05R-358).
#[derive(Debug)]
struct SlowSettle(Duration);

impl hoh_app::host::HostHooks for SlowSettle {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == hoh_app::host::RunnerPoint::Settling {
            std::thread::sleep(self.0);
        }
    }
}

/// M2T05R-102, -150: a write of an attempt's task fails (its receipt; its OBSERVED effect). The task's entry goes
/// with it: the receipt's failure leaves the dispatch to be delivered again; the later one is settled at once with
/// the process it holds (its work found by its marker): either way T1 ends IMPLEMENTED and T2 follows, no lease kept
/// alive for a task that ended. Its entry goes only after that settle: ticks during it do not take the attempt up
/// again (M2T05R-260, -358).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_write_that_fails_does_not_strand_its_attempt() -> R {
    for nth in [1, 3] {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{stderr}");
        seed(&project, WRITERS)?;
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("markers"))?;
        let clock = Arc::new(FakeClock::default());
        let faults = Arc::new(FailNthRunnerWrite {
            nth,
            seen: std::sync::atomic::AtomicU32::new(0),
            fail: std::sync::atomic::AtomicBool::new(false),
        });
        let host = runner_host_with(
            &project,
            Arc::new(FakeExecutor(work.clone())),
            clock,
            Setup {
                faults: Some(faults),
                observer: Some(Arc::new(Markers(work.clone()))),
                hooks: Some(Arc::new(SlowSettle(Duration::from_millis(700)))),
                ..Setup::default()
            },
        )
        .await?;
        let db = db(&project);
        assert!(
            wait_for(
                &db,
                "SELECT group_concat(state ORDER BY task_id) FROM tasks WHERE task_id IN ('T1', 'T2')",
                "IMPLEMENTED,IMPLEMENTED",
                Duration::from_secs(20)
            )?,
            "nth={nth}: {}",
            query(
                &db,
                "SELECT group_concat(task_id || ':' || state) FROM tasks"
            )?
        );
        if nth == 1 {
            // The receipt failed: the dispatch is delivered again to the same attempt, which settles.
            assert_eq!(
                query(
                    &db,
                    "SELECT json_extract(payload_json, '$.disposition') FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'runner_error'"
                )?,
                "redeliver"
            );
            assert_eq!(
                query(
                    &db,
                    "SELECT group_concat(state) FROM attempts WHERE task_id = 'T1'"
                )?,
                "SETTLED"
            );
        }
        if nth == 3 {
            assert_eq!(
                query(
                    &db,
                    "SELECT json_extract(payload_json, '$.disposition') FROM events WHERE kind = 'reconcile.attempt' AND json_extract(payload_json, '$.cause') = 'runner_error'"
                )?,
                "completed"
            );
        }
        let counted = std::fs::read_to_string(work.join("counter")).unwrap_or_default();
        let mut keys: Vec<&str> = counted.lines().collect();
        let all = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), all, "nth={nth}: {counted}");
        host.shutdown().await;
    }
    Ok(())
}

/// Fails its first start, or hands back a gate that cannot open (closing it as it fails); otherwise a FakeExecutor.
#[derive(Debug)]
struct Faulty {
    inner: FakeExecutor,
    start_fails: std::sync::atomic::AtomicBool,
    gate_fails: bool,
    /// Its first start hands back a pid no process has (its identity cannot be read).
    ghost: std::sync::atomic::AtomicBool,
    /// Whether that ghost's gate was ever opened (M2T05R-352).
    ghost_opened: Arc<std::sync::atomic::AtomicBool>,
}

impl hoh_app::host::Executor for Faulty {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.inner.deliver(work)
    }

    fn start(
        &self,
        work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        if self
            .start_fails
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err("injected: no process".to_owned().into());
        }
        if self.ghost.swap(false, std::sync::atomic::Ordering::SeqCst) {
            let (_never, ended) = tokio::sync::oneshot::channel();
            let opened = self.ghost_opened.clone();
            return Ok(hoh_app::host::Started {
                pid: 1 << 23,
                gate: hoh_app::host::Gate::new(move || {
                    opened.store(true, std::sync::atomic::Ordering::SeqCst);
                    Err("no process".to_owned())
                }),
                ended,
                prompt: None,
            });
        }
        let started = self.inner.start(work)?;
        if self.gate_fails {
            let real = started.gate;
            let gate = hoh_app::host::Gate::new(move || {
                drop(real);
                Err("injected: the gate does not open".to_owned())
            });
            return Ok(hoh_app::host::Started { gate, ..started });
        }
        Ok(started)
    }
}

/// M2T05R-156: a start that fails ends its attempt FAILED from STARTING with its work never started, and its task
/// runs again; a gate that cannot open is settled (its process never ran: UNKNOWN_EFFECT, as nothing confirms it).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_start_and_a_gate_that_cannot_open_never_run_the_work() -> R {
    for (gate_fails, ghost) in [(false, false), (true, false), (false, true)] {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{stderr}");
        seed(&project, WRITERS)?;
        let work = dir.path().join("work");
        std::fs::create_dir_all(work.join("markers"))?;
        let clock = Arc::new(FakeClock::default());
        let executor = Arc::new(Faulty {
            inner: FakeExecutor(work.clone()),
            start_fails: std::sync::atomic::AtomicBool::new(!gate_fails && !ghost),
            gate_fails,
            ghost: std::sync::atomic::AtomicBool::new(ghost),
            ghost_opened: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        });
        let host = runner_host(&project, executor.clone(), clock, None).await?;
        let db = db(&project);
        let first = "SELECT a.state || ':' || e.state FROM attempts a JOIN effects e ON e.attempt_id = a.attempt_id \
                     WHERE a.task_id = 'T1' ORDER BY a.rowid LIMIT 1";
        let want = if gate_fails {
            "UNKNOWN_EFFECT:UNKNOWN"
        } else {
            "FAILED:PREPARED"
        };
        assert!(
            wait_for(&db, first, want, Duration::from_secs(20))?,
            "{}",
            query(&db, first)?
        );
        if !gate_fails {
            assert!(wait_for(
                &db,
                "SELECT state FROM tasks WHERE task_id = 'T1'",
                "IMPLEMENTED",
                Duration::from_secs(20)
            )?);
        }
        let counted = std::fs::read_to_string(work.join("counter")).unwrap_or_default();
        let a1 = query(
            &db,
            "SELECT attempt_id FROM attempts WHERE task_id = 'T1' ORDER BY rowid LIMIT 1",
        )?;
        assert!(!counted.contains(&a1), "gate_fails={gate_fails}: {counted}");
        // A process whose identity cannot be read never has its gate opened.
        assert!(
            !executor
                .ghost_opened
                .load(std::sync::atomic::Ordering::SeqCst)
        );
        host.shutdown().await;
    }
    Ok(())
}

/// A stuck agent whose native cancel works: it ends the agent's leader, and its executor, a slow one, kills what is
/// left in the group 100 ms after the leader's exit (M2T05R-259, -354).
#[derive(Debug)]
struct Cancellable(StuckExecutor);

impl hoh_app::host::Executor for Cancellable {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.0.deliver(work)
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let (child, pid, gate) = self.0.spawn()?;
        let ended = reaping_after(child, Duration::from_millis(100));
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }

    fn cancel(&self, work: &hoh_app::host::Work) -> bool {
        let held = self.0.cancel(work);
        for pid in self.0.started.lock().map(|p| p.clone()).unwrap_or_default() {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
        held
    }
}

/// M2T05R-259, -354: a holder whose native cancel ends it within its grace gets no signal: its expiry records the
/// inspection, the native cancel and the reap, with no SIGTERM or SIGKILL step, though its group empties only some
/// time after its leader exits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_ended_by_its_native_cancel_gets_no_signal() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(Cancellable(StuckExecutor {
        cancels: dir.path().join("cancels"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: true,
    }));
    let host = runner_host(&project, executor.clone(), clock.clone(), None).await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    clock.advance(Duration::from_secs(300));
    assert!(wait_for(
        &db,
        &format!(
            "SELECT count(*) FROM events WHERE kind = 'lease.reaped' AND json_extract(payload_json, '$.attempt_id') = '{a1}'"
        ),
        "1",
        Duration::from_secs(20)
    )?);
    assert_eq!(
        query(
            &db,
            &format!(
                "SELECT group_concat(kind, ',') FROM (SELECT kind FROM events WHERE kind LIKE 'lease.%' AND json_extract(payload_json, '$.attempt_id') = '{a1}' ORDER BY seq)"
            )
        )?,
        "lease.inspected,lease.native_cancel,lease.reaped"
    );
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// A stuck agent whose native cancel kills only its leader, and whose executor reaps that leader at once without
/// emptying its group, so the leader is gone while a member that ignores SIGTERM is left (M2T05R-452).
#[derive(Debug)]
struct LeaderOnlyCancel(StuckExecutor);

impl hoh_app::host::Executor for LeaderOnlyCancel {
    fn deliver(
        &self,
        work: &hoh_app::host::Work,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.0.deliver(work)
    }

    fn start(
        &self,
        _work: &hoh_app::host::Work,
    ) -> Result<hoh_app::host::Started, hoh_app::host::Refusal> {
        let (mut child, pid, gate) = self.0.spawn()?;
        let (end, ended) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let status = child.wait();
            let _ = end.send(hoh_app::host::Outcome {
                success: false,
                detail: format!("{status:?}"),
                group_gone: false,
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: Default::default(),
                unknown_effect: false,
            });
        });
        Ok(hoh_app::host::Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }

    fn cancel(&self, work: &hoh_app::host::Work) -> bool {
        let held = self.0.cancel(work);
        for pid in self.0.started.lock().map(|p| p.clone()).unwrap_or_default() {
            if let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
        held
    }
}

/// Makes every plain commit take 200 ms once a settle has begun (longer than `HELD_FOR`).
#[derive(Debug, Default)]
struct SlowWritesWhileSettling(std::sync::atomic::AtomicBool);

impl FaultHooks for SlowWritesWhileSettling {
    fn at(&self, job: &'static str, point: WritePoint) {
        if job == "Commit"
            && point == WritePoint::Begun
            && self.0.load(std::sync::atomic::Ordering::SeqCst)
        {
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

impl hoh_app::host::HostHooks for SlowWritesWhileSettling {
    fn runner(&self, point: hoh_app::host::RunnerPoint) {
        if point == hoh_app::host::RunnerPoint::Settling {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
}

/// M2T05R-452: A1's lease expires; its native cancel kills its leader, which is reaped at once, while a member that
/// ignores SIGTERM stays. Its group is looked at from the inspection to the end of the stop with no write in between,
/// however slow the writes are, so it stays known as A1's: the member gets SIGTERM and SIGKILL, the group is seen
/// empty, and A1 lets its slot go (UNKNOWN_EFFECT: its work cannot be confirmed).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_group_is_watched_through_its_settle_however_slow_the_writes() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    seed(&project, WRITERS)?;
    let clock = Arc::new(FakeClock::default());
    let executor = Arc::new(LeaderOnlyCancel(StuckExecutor {
        cancels: dir.path().join("cancels"),
        started: std::sync::Mutex::new(Vec::new()),
        holds: true,
    }));
    let slow = Arc::new(SlowWritesWhileSettling::default());
    let host = runner_host_with(
        &project,
        executor.clone(),
        clock.clone(),
        Setup {
            faults: Some(slow.clone()),
            hooks: Some(slow),
            ..Setup::default()
        },
    )
    .await?;
    let db = db(&project);
    assert!(wait_for(
        &db,
        "SELECT state FROM attempts WHERE task_id = 'T1'",
        "RUNNING",
        Duration::from_secs(10)
    )?);
    let a1 = query(&db, "SELECT attempt_id FROM attempts WHERE task_id = 'T1'")?;
    let pid: u32 = query(
        &db,
        &format!("SELECT pid FROM attempts WHERE attempt_id = '{a1}'"),
    )?
    .parse()?;
    clock.advance(Duration::from_secs(300));
    assert!(
        wait_for(
            &db,
            &format!("SELECT state FROM attempts WHERE attempt_id = '{a1}'"),
            "UNKNOWN_EFFECT",
            Duration::from_secs(30)
        )?,
        "{}",
        query(
            &db,
            &format!(
                "SELECT group_concat(kind, ',') FROM (SELECT kind FROM events WHERE json_extract(payload_json, '$.attempt_id') = '{a1}' ORDER BY seq)"
            )
        )?
    );
    assert_eq!(
        query(
            &db,
            &format!(
                "SELECT group_concat(kind, ',') FROM (SELECT kind FROM events WHERE kind LIKE 'lease.%' AND json_extract(payload_json, '$.attempt_id') = '{a1}' ORDER BY seq)"
            )
        )?,
        "lease.inspected,lease.native_cancel,lease.sigterm,lease.sigkill,lease.reaped"
    );
    assert_eq!(group_now(pid), nothing_left());
    host.shutdown().await;
    drop(executor);
    Ok(())
}

/// AC-042-02's world at time `t` (whole seconds): run R WAITING_EXTERNAL (to resume RUNNING), its task T1 READY after
/// three failed attempts with its retry at t+200 s; 7 iterations and 1234 s used of the run's budget.
fn waiting_world(t: std::time::SystemTime) -> String {
    let at = |s: u64| hoh_core::time::rfc3339_utc(t + Duration::from_secs(s));
    format!(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, resume_state, created_at) VALUES
           ('R', :p, 's2', 'WAITING_EXTERNAL', 'AUTO', 9, 'RUNNING', '2026-11-07T00:00:00Z');
         INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sd', 'claude', 'developer', '1', '{{\"structured_events\": true, \"command_exec\": true}}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate, attempt_count,
           next_retry_at) VALUES ('T1', 'R', 's2', 'READY', 7, 'developer', 1, 1, 1, 3, '{next}');
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision, ended_at, failure_reason) VALUES
           ('T1/attempt-1', 'T1', 'FAILED', 2, 1, 1, '{e1}', 'HTTP 503'),
           ('T1/attempt-2', 'T1', 'FAILED', 2, 1, 1, '{e2}', 'HTTP 503'),
           ('T1/attempt-3', 'T1', 'FAILED', 2, 1, 1, '{e3}', 'HTTP 429');
         INSERT INTO budgets(scope_kind, scope_id, kind, limit_value, used_value) VALUES
           ('run', 'R', 'iterations', 100, 7), ('run', 'R', 'wall_seconds', 432000, 1234);",
        next = at(200),
        e1 = at(0),
        e2 = at(0),
        e3 = at(0),
    )
}

/// AC-042-02: a WAITING_EXTERNAL run (attempt_count 3, next_retry_at t+200 s, 7 iterations used and its duration
/// persisted) whose host is killed with SIGKILL at t+5 s and restarted at t+10 s keeps all four values; nothing is
/// dispatched before t+200 s, and at t+200 s exactly one dispatch and one new attempt. The child's clock is a file the
/// test writes.
#[test]
fn test_042_02_a_waiting_run_keeps_its_retry_and_budget_across_a_kill_9() -> R {
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    std::fs::create_dir_all(&project)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    let t = std::time::UNIX_EPOCH
        + Duration::from_secs(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
        );
    seed(&project, &waiting_world(t))?;
    let work = dir.path().join("work");
    std::fs::create_dir_all(work.join("markers"))?;
    let clock = dir.path().join("clock");
    let env: [(&str, &std::ffi::OsStr); 2] = [
        (CHILD_WORK, work.as_os_str()),
        (CHILD_CLOCK, clock.as_os_str()),
    ];
    let db = db(&project);
    let snapshot = || -> R<Value> {
        Ok(json!({
            "run": query(&db, "SELECT state FROM runs WHERE run_id = 'R'")?,
            "attempt_count": query(&db, "SELECT attempt_count FROM tasks WHERE task_id = 'T1'")?,
            "next_retry_at": query(&db, "SELECT next_retry_at FROM tasks WHERE task_id = 'T1'")?,
            "iterations_used": query(&db, "SELECT used_value FROM budgets WHERE scope_id = 'R' AND kind = 'iterations'")?,
            "wall_seconds_used": query(&db, "SELECT used_value FROM budgets WHERE scope_id = 'R' AND kind = 'wall_seconds'")?,
        }))
    };
    let counts = || -> R<(String, String)> {
        Ok((
            query(
                &db,
                "SELECT count(*) FROM outbox WHERE json_extract(payload_json, '$.kind') = 'dispatch'",
            )?,
            query(&db, "SELECT count(*) FROM attempts WHERE task_id = 'T1'")?,
        ))
    };
    let mut log = Vec::new();
    // t+5 s: the host waits; it is killed with SIGKILL.
    set_clock(&clock, t + Duration::from_secs(5))?;
    let host = start_child(&project, &env)?;
    std::thread::sleep(Duration::from_millis(800));
    let before = snapshot()?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(host.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::KILL)?;
    let (status, _) = wait(host, Duration::from_secs(30))?;
    assert_eq!(status.signal(), Some(9));
    log.push(json!({"at": "t+5", "killed": true, "counts": counts()?}));
    // t+10 s: restarted; every field is as it was.
    set_clock(&clock, t + Duration::from_secs(10))?;
    let host = start_child(&project, &env)?;
    std::thread::sleep(Duration::from_millis(800));
    let after = snapshot()?;
    assert_eq!(before, after);
    assert_eq!(after["attempt_count"], json!("3"));
    assert_eq!(after["iterations_used"], json!("7.0"));
    assert_eq!(after["run"], json!("WAITING_EXTERNAL"));
    log.push(json!({"at": "t+10", "restarted": true, "counts": counts()?}));
    // t+199 s: nothing dispatched yet.
    set_clock(&clock, t + Duration::from_secs(199))?;
    std::thread::sleep(Duration::from_millis(1_000));
    assert_eq!(counts()?, ("0".to_owned(), "3".to_owned()));
    log.push(json!({"at": "t+199", "counts": counts()?}));
    // t+200 s: one dispatch, one new attempt.
    set_clock(&clock, t + Duration::from_secs(200))?;
    let end = Instant::now() + Duration::from_secs(20);
    while counts()?.1 != "4" && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(counts()?, ("1".to_owned(), "4".to_owned()));
    log.push(json!({"at": "t+200", "counts": counts()?}));
    let pid = rustix::process::Pid::from_raw(i32::try_from(host.0.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let (status, stderr) = wait(host, Duration::from_secs(30))?;
    assert_eq!(status.code(), Some(0), "{stderr}");
    keep(
        "test-042-02.json",
        &json!({
            "t": hoh_core::time::rfc3339_utc(t), "before_kill": before, "after_restart": after, "dispatch_log": log,
            "events": python(&db, "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); print(json.dumps([list(r) for r in c.execute(sys.stdin.read())]))",
                "SELECT seq, kind, json_extract(payload_json, '$.task_id') FROM events WHERE run_id = 'R' ORDER BY seq")?.trim(),
        }),
    )?;
    Ok(())
}

/// M2T19R-51: the host is killed after its attempt's failure was recorded (OBSERVED, then COMMITTED); the host
/// started again ends the attempt by the class and the end time that record kept, as the runner would have: a 429
/// waits its Retry-After from the end, counted; a missing authentication blocks at once, with no retry.
#[test]
fn a_failure_recorded_before_a_kill_is_retried_by_its_class_after_the_restart() -> R {
    for (class, want) in [("retry_after:120", "READY"), ("auth", "BLOCKED")] {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{stderr}");
        seed(
            &project,
            "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
             INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
               ('R', :p, 's2', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
             INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
               ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
             INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
               ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
               ('T1', 'R', 's2', 'READY', 1, 'developer', 1, 1, 1);",
        )?;
        let mut command = child(&project)?;
        command
            .env(CHILD_FAIL_CLASS, class)
            .env(CHILD_RUNNER_KILL, "AfterEffectCommitted");
        let (status, stderr) = wait(ChildGuard(command.spawn()?), Duration::from_secs(30))?;
        assert_eq!(status.signal(), Some(9), "{class}: {stderr}");
        let db = db(&project);
        assert_eq!(
            query(&db, "SELECT state FROM attempts WHERE task_id = 'T1'")?,
            "RUNNING",
            "{class}: killed after COMMITTED, before its end"
        );
        let (code, stderr) = serve_and_stop(&project)?;
        assert_eq!(code, 0, "{class}: {stderr}");
        assert_eq!(
            query(
                &db,
                "SELECT state || ':' || attempt_count FROM tasks WHERE task_id = 'T1'"
            )?,
            format!("{want}:1"),
            "{class}"
        );
        let observed = query(
            &db,
            "SELECT json_extract(payload_json, '$.detail.ended_at') FROM events WHERE kind = 'effect.observed'",
        )?;
        let ended = query(&db, "SELECT ended_at FROM attempts WHERE task_id = 'T1'")?;
        assert_eq!(ended, observed, "{class}: ended when its work did");
        let next = query(&db, "SELECT next_retry_at FROM tasks WHERE task_id = 'T1'")?;
        if want == "READY" {
            let waited = hoh_core::time::parse_rfc3339(&next)
                .zip(hoh_core::time::parse_rfc3339(&ended))
                .and_then(|(n, e)| n.duration_since(e).ok());
            assert_eq!(waited, Some(Duration::from_secs(120)), "{class}");
        } else {
            assert_eq!(next, "", "{class}: no retry");
            assert_eq!(
                query(&db, "SELECT blocked_reason FROM tasks WHERE task_id = 'T1'")?,
                "BLOCKED_AUTH"
            );
        }
    }
    Ok(())
}

/// HTTP over the host's socket with its token, as a client sends it; the status and the body.
fn http(info: &Value, method: &str, path: &str, body: Option<&str>) -> R<(u16, String)> {
    use std::io::{Read as _, Write as _};
    let mut stream =
        std::os::unix::net::UnixStream::connect(info["socket"].as_str().ok_or("no socket")?)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nX-HoH-Token: {}\r\n",
        info["token"].as_str().ok_or("no token")?
    );
    if let Some(body) = body {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(body.unwrap_or_default());
    stream.write_all(request.as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    let status = reply
        .split_whitespace()
        .nth(1)
        .ok_or("no status")?
        .parse()?;
    Ok((
        status,
        reply
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_owned())
            .unwrap_or_default(),
    ))
}

/// The attempts, the effects and the events of `db`, for the record and for failure messages.
const EXPORT: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect("file:" + sys.argv[1] + "?mode=ro", uri=True)
rows = lambda sql: [dict(zip([d[0] for d in q.description], r)) for q in [c.execute(sql)] for r in q.fetchall()]
print(json.dumps({
    "projects": rows("SELECT project_id, host_epoch FROM projects"),
    "runs": rows("SELECT run_id, state, revision FROM runs"),
    "tasks": rows("SELECT task_id, state, role FROM tasks"),
    "attempts": rows("SELECT rowid, attempt_id, task_id, state, host_epoch, pid, started_at, ended_at, failure_reason FROM attempts ORDER BY rowid"),
    "effects": rows("SELECT effect_key, attempt_id, state FROM effects ORDER BY rowid"),
    "events": [dict(e, payload=json.loads(e["payload_json"])) for e in rows("SELECT seq, kind, run_id, created_at, payload_json FROM events ORDER BY seq")],
}))
"#;

fn export(project: &Path) -> R<Value> {
    Ok(serde_json::from_str(&python(&db(project), EXPORT, "")?)?)
}

/// The fake claude for TEST-005-04 (2.1.318 by its recorded probe answers): a turn of T1 writes its completion
/// marker and replays the recorded `success` turn; a turn of T2 replays its first frame and then waits for a file
/// that never appears.
fn unit_claude(bin: &Path) -> R {
    let protocol = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");
    std::fs::create_dir_all(bin.join("claude.d"))?;
    installed::script(
        &bin.join("claude"),
        &format!(
            "#!/bin/bash\nf='{protocol}/claude/2.1.318'; d='{bin}/claude.d'\n\
             case \"$1\" in --version) echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
             cat > \"$d/stdin.$$\"\n\
             case \"$HOH_ATTEMPT_ID\" in\n\
              *T2/attempt*) echo $$ > \"$d/t2.pid\"; head -n 1 \"$f/success/stdout.jsonl\"; while [ ! -e \"$d/never\" ]; do sleep 0.1; done;;\n\
             esac\n\
             echo \"$HOH_ATTEMPT_ID\" >> \"$d/completed\"\n\
             cat \"$f/success/stdout.jsonl\"\n",
            bin = bin.display()
        ),
    )
}

#[test]
fn test_005_04_a_host_killed_under_its_unit_is_restarted_by_systemd_and_reconciles_before_any_write()
-> R {
    // AC-005-04 (docs/adr/ADR-023.md decision 12): the installed hoh, its host run by its systemd user unit (a private
    // user manager in a delegated scope, `installed`); R with T1 SETTLED, its effect COMMITTED, and T2 RUNNING; no
    // client attached. kill -9 of the host's main process, then `hoh attach R`.
    let manager = installed::Manager::start()?;
    let root = manager.root().to_path_buf();
    let mut seen = serde_json::Map::new();
    let outcome = (|| -> R {
        let project = root.join("p");
        std::fs::create_dir(&project)?;
        let git = manager
            .command("git")
            .args(["init", "-q"])
            .current_dir(&project)
            .output()?;
        assert!(git.status.success());
        let project = std::fs::canonicalize(project)?;
        let bundle = installed::stage_bundle(&root.join("bundle"), b"")?;
        let out = manager.install(&bundle, &["--project", project.to_str().ok_or("path")?])?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // The host's PATH (the client writes its own into the unit's file): the fake agent and the tools it and the
        // client use, nothing else, so no agent installed on this machine is found.
        unit_claude(&manager.bin())?;
        // The team on the one agent this machine has: no binding is blocked, so nothing wakes a Commander it cannot run
        // and the run is not BLOCKED for it (docs/adr/ADR-025.md decision 5).
        team_on(&project, "claude")?;
        let tools = root.join("tools");
        std::fs::create_dir(&tools)?;
        for tool in ["cat", "head", "sleep", "python3", "systemctl"] {
            let found = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .ok_or(format!("no {tool}"))?;
            std::os::unix::fs::symlink(found, tools.join(tool))?;
        }
        let path = format!(
            "{}:{}:{}",
            manager.bin().display(),
            tools.display(),
            manager.home().join(".local/bin").display()
        );
        let hoh = |args: &[&str]| {
            manager
                .command(manager.home().join(".local/bin/hoh"))
                .env("PATH", &path)
                .args(args)
                .current_dir(&project)
                .stdin(std::process::Stdio::null())
                .output()
        };
        let out = hoh(&["-p", "x", "--detach", "--output", "json"])?;
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run_id = serde_json::from_slice::<Value>(&out.stdout)?["run_id"]
            .as_str()
            .ok_or("no run_id")?
            .to_owned();
        let unit = hoh_app::unit::unit_name(&hoh_app::unit::instance(&project));
        // T1 and T2 READY for the developer (claude by default), on a spec, the run RUNNING.
        python(
            &db(&project),
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.stdin.read()); c.commit()",
            &format!(
                "INSERT OR IGNORE INTO specs(spec_id, project_id, sha256, revision) \
                 SELECT 's1', project_id, printf('%064d', 1), 1 FROM runs WHERE run_id = '{run_id}';
                 UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run_id}';
                 INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T1', '{run_id}', 's1', 'READY', 1, 'developer', 0, 1, 1), \
                        ('T2', '{run_id}', 's1', 'READY', 1, 'developer', 0, 2, 1);"
            ),
        )?;
        let marker = manager.bin().join("claude.d/completed");
        let mut waited = Ok(());
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let t1 = query(
                &db(&project),
                "SELECT state FROM attempts WHERE task_id = 'T1'",
            )?;
            let effect = query(
                &db(&project),
                "SELECT group_concat(e.state) FROM effects e JOIN attempts a USING(attempt_id) WHERE a.task_id = 'T1'",
            )?;
            let t2 = query(
                &db(&project),
                "SELECT state FROM attempts WHERE task_id = 'T2'",
            )?;
            if t1 == "SETTLED"
                && effect.split(',').any(|e| e == "COMMITTED")
                && t2 == "RUNNING"
                && marker.exists()
                && manager.bin().join("claude.d/t2.pid").exists()
            {
                break;
            }
            if Instant::now() > end {
                waited = Err(format!("T1 {t1:?} ({effect:?}), T2 {t2:?}"));
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if let Err(why) = waited {
            return Err(format!("{why}: {}", export(&project)?).into());
        }
        let info: Value =
            serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
        // CR-011 under the unit: attempts tracked by cgroups of their own.
        assert_eq!(info["process_tracking"], "cgroup", "{info}");
        assert_eq!(info["supervisor"], unit.as_str(), "{info}");
        let before = manager.show(&unit, "MainPID,NRestarts")?;
        let old_pid: i32 = before.get("MainPID").ok_or("MainPID")?.parse()?;
        let restarts: u32 = before.get("NRestarts").ok_or("NRestarts")?.parse()?;
        assert_eq!(Some(i64::from(old_pid)), info["pid"].as_i64(), "{info}");
        let epoch = info["host_epoch"].as_i64().ok_or("host_epoch")?;
        let t1_attempts = query(
            &db(&project),
            "SELECT count(*) FROM attempts WHERE task_id = 'T1'",
        )?;
        let t2_before: i64 = query(
            &db(&project),
            "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        )?
        .parse()?;
        let old_t2 = query(
            &db(&project),
            "SELECT attempt_id FROM attempts WHERE task_id = 'T2'",
        )?;
        seen.insert(
            "before".into(),
            json!({"show": before, "host": info, "db": export(&project)?}),
        );

        // kill -9 of the host's main process.
        let t0 = std::time::SystemTime::now();
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(old_pid).ok_or("pid")?,
            rustix::process::Signal::KILL,
        )?;
        // systemd restarts it (not a client: its start would come before the restart). The first write command goes
        // to the restarted host as soon as it lets any client in (M2T14R-57): its listeners open only after its
        // reconcile (host/mod.rs), and this records that order as a client sees it.
        let end = Instant::now() + Duration::from_secs(60);
        let (info, accepted) = loop {
            let info = std::fs::read_to_string(project.join(".hoh/host.json"))
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .filter(|i| i["pid"].as_i64().is_some_and(|p| p != i64::from(old_pid)));
            if let Some(info) = info {
                let revision = query(
                    &db(&project),
                    &format!("SELECT revision FROM runs WHERE run_id = '{run_id}'"),
                )?;
                let cancel = json!({
                    "schema_version": "1.0", "command_id": "c-cancel-after-restart",
                    "idempotency_key": "cancel-after-restart", "expected_revision": revision.parse::<i64>()?,
                    "kind": "CancelRun", "run_id": run_id, "payload": {"reason": "TEST-005-04"},
                });
                if let Ok((202, accepted)) =
                    http(&info, "POST", "/v1/commands", Some(&cancel.to_string()))
                {
                    break (info, accepted);
                }
            }
            if Instant::now() > end {
                return Err(format!("no write accepted after the kill\n{}", manager.log()).into());
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        seen.insert("cancel".into(), json!(accepted));
        let new_pid = i32::try_from(info["pid"].as_i64().ok_or("pid")?)?;
        // That host is the unit's, restarted by systemd.
        let after = manager.show(&unit, "MainPID,NRestarts,ActiveState,ExecMainStatus")?;
        assert_eq!(
            after.get("MainPID").map(String::as_str),
            Some(new_pid.to_string().as_str()),
            "{after:?}"
        );
        assert_eq!(
            after.get("ActiveState").map(String::as_str),
            Some("active"),
            "{after:?}"
        );
        // `hoh attach R`.
        let out = hoh(&["attach", &run_id, "--output", "json"])?;
        seen.insert("attach".into(), json!({"exit": out.status.code(),
            "stdout": String::from_utf8_lossy(&out.stdout), "stderr": String::from_utf8_lossy(&out.stderr)}));
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let db_after = export(&project)?;
        seen.insert(
            "after".into(),
            json!({"show": after, "host": info, "t0_unix_ms":
            t0.duration_since(std::time::UNIX_EPOCH)?.as_millis(), "db": db_after.clone()}),
        );

        // systemd restarted it: NRestarts + 1, another pid; host_epoch + 1.
        assert_eq!(
            after.get("NRestarts").map(String::as_str),
            Some((restarts + 1).to_string().as_str()),
            "{after:?}"
        );
        assert_ne!(new_pid, old_pid);
        assert_eq!(info["host_epoch"].as_i64(), Some(epoch + 1), "{info}");
        let events = db_after["events"].as_array().ok_or("events")?;
        let of =
            |kind: &str| -> Vec<&Value> { events.iter().filter(|e| e["kind"] == kind).collect() };
        let started = of("reconcile.started")
            .into_iter()
            .find(|e| e["payload"]["host_epoch"].as_i64() == Some(epoch + 1))
            .ok_or("no reconcile.started of the restarted host")?;
        let completed = of("reconcile.completed")
            .into_iter()
            .find(|e| e["payload"]["host_epoch"].as_i64() == Some(epoch + 1))
            .ok_or("no reconcile.completed of the restarted host")?;
        let cancelled = of("run.cancel_requested")
            .into_iter()
            .last()
            .ok_or("no run.cancel_requested")?;
        // The reconcile completed before any write command was accepted.
        assert!(
            completed["seq"].as_i64() < cancelled["seq"].as_i64(),
            "{completed} {cancelled}"
        );
        // T1 gets no new attempt; T2 another one only after the reconcile judged its old one not run.
        assert_eq!(
            query(
                &db(&project),
                "SELECT count(*) FROM attempts WHERE task_id = 'T1'"
            )?,
            t1_attempts
        );
        let t2_after: i64 = query(
            &db(&project),
            "SELECT count(*) FROM attempts WHERE task_id = 'T2'",
        )?
        .parse()?;
        assert!(t2_after <= t2_before + 1, "T2: {t2_before} -> {t2_after}");
        let judged = of("reconcile.attempt")
            .into_iter()
            .find(|e| e["payload"]["attempt_id"].as_str() == Some(old_t2.as_str()))
            .ok_or("no reconcile.attempt for T2's attempt")?;
        if t2_after > t2_before {
            let created = of("attempt.created")
                .into_iter()
                .filter(|e| {
                    e["payload"]["attempt_id"].as_str() != Some(old_t2.as_str())
                        && e["payload"]["task_id"] == "T2"
                })
                .map(|e| e["seq"].as_i64())
                .min()
                .ok_or("T2's new attempt has no attempt.created")?;
            assert!(created > judged["seq"].as_i64(), "{judged}");
            assert_eq!(
                judged["payload"]["process"]["found"], "not_started",
                "{judged}"
            );
        }
        // From the kill to the reconcile's start (its own time in ms): at most 30 s.
        let at = std::time::UNIX_EPOCH
            + Duration::from_millis(started["payload"]["at_ms"].as_u64().ok_or("at_ms")?);
        let delay = at.duration_since(t0).unwrap_or_default();
        seen.insert("kill_to_reconcile_ms".into(), json!(delay.as_millis()));
        assert!(delay <= Duration::from_secs(30), "{delay:?}");
        // The old T2 ended by its cgroup: tracked by one, and judged with its processes gone or stopped.
        let tracking = of("attempt.tracking")
            .into_iter()
            .find(|e| e["payload"]["attempt_id"].as_str() == Some(old_t2.as_str()))
            .ok_or("no attempt.tracking for T2's attempt")?;
        seen.insert("t2".into(), json!({"tracking": tracking, "judged": judged}));
        assert!(
            tracking["payload"]["cgroup"]
                .as_str()
                .is_some_and(|c| c.contains(&format!("/{unit}/attempt-"))),
            "{tracking}"
        );
        // Gone, or found and stopped and reaped: with a cgroup, either needs its cgroup proved empty (ADR-021 8d).
        let process = &judged["payload"]["process"];
        assert!(
            process["found"] == "gone" || (process["found"] == "ours" && process["reaped"] == true),
            "{judged}"
        );
        assert_eq!(started["payload"]["unfinished"], 1, "{started}");
        Ok(())
    })();
    seen.insert("manager_log".into(), json!(manager.log()));
    keep("TEST-005-04.json", &Value::Object(seen))?;
    outcome
}

/// When the stuck agent marked its first SIGTERM: its file's modification time, in ns since the epoch.
fn term_at(file: &Path) -> R<u128> {
    let modified = std::fs::metadata(file)
        .map_err(|e| format!("no SIGTERM marked ({}): {e}", file.display()))?
        .modified()?;
    Ok(modified.duration_since(std::time::UNIX_EPOCH)?.as_nanos())
}

#[test]
fn test_019_05_a_host_killed_after_the_handover_commit_keeps_epoch_2_and_starts_one_session() -> R {
    // AC-019-05 (docs/adr/ADR-024.md decisions 2, 4, 6 and 8): a FakeAgent team (a fake pi on the runner's PATH, no
    // codex or claude); the commander's change from codex to pi commits its handover and the host is killed -9 before
    // the new Commander's session is started. The next host serves (its start reconcile done) within 30 s; the
    // commander stays pi at epoch 2; exactly one pi commander session is bound and its first input given once; the
    // old Codex's three epoch-1 messages (two decisions, a steer) are all rejected STALE_ROLE_EPOCH, and nothing is
    // dispatched.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("project");
    let bin = dir.path().join("bin");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&bin)?;
    let help = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/pi/1.5.0/help.txt"
    );
    std::fs::write(
        bin.join("pi"),
        format!(
            "#!/bin/sh\ncase \"$1\" in\n --version) echo '1.5.0';;\n --help) /bin/cat '{help}';;\n *) exit 2;;\nesac\n"
        ),
    )?;
    std::fs::set_permissions(bin.join("pi"), std::fs::Permissions::from_mode(0o755))?;
    let agents = bin.as_os_str();
    let first = start_child(
        &project,
        &[
            (CHILD_AGENTS, agents),
            (CHILD_KILL_AT_HANDOVER, std::ffi::OsStr::new("1")),
        ],
    )?;
    seed(
        &project,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
           ('R', :p, 's1', 'RUNNING', 'AUTO', 1, '2026-11-10T00:00:00Z');
         INSERT INTO role_bindings(run_id, role, agent, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'codex', 1, 0, '2026-11-10T00:00:00Z'),
           ('R', 'developer', 'claude', 1, 0, '2026-11-10T00:00:00Z'),
           ('R', 'qa', 'pi', 1, 0, '2026-11-10T00:00:00Z'),
           ('R', 'reviewer', 'codex', 1, 0, '2026-11-10T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
           ('T', 'R', 's1', 'ACCEPTED', 1, 'developer', 0, 1, 1);",
    )?;
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    let change = json!({
        "schema_version": "1.0", "command_id": "c-pi", "idempotency_key": "k-pi", "expected_revision": 0,
        "kind": "ConfigureTeam", "run_id": null,
        "payload": {"bindings": {"commander": {"agent": "pi"}}, "apply_at": "checkpoint"},
    });
    let (status, body) = http(&info, "POST", "/v1/commands", Some(&change.to_string()))?;
    assert_eq!(status, 202, "{body}");
    let (status, _) = wait(first, Duration::from_secs(60))?;
    assert_eq!(status.signal(), Some(9), "killed at the handover's commit");
    let commander = "SELECT agent || '/' || role_epoch || '/' || coalesce(session_id, '-') FROM role_bindings \
                     WHERE run_id = 'R' AND role = 'commander'";
    let notice =
        "SELECT state FROM outbox WHERE json_extract(payload_json, '$.kind') = 'session.start'";
    assert_eq!(
        query(&db(&project), commander)?,
        "pi/2/-",
        "committed, no session yet"
    );
    assert_eq!(query(&db(&project), notice)?, "PENDING");
    let log = bin.join("first-inputs.log");
    assert!(!log.exists(), "no first input before the crash");
    // The next host, then the old Codex's messages once it serves.
    let outcomes = dir.path().join("old-commander.txt");
    let restarted = Instant::now();
    let second = start_child(
        &project,
        &[
            (CHILD_AGENTS, agents),
            (CHILD_OLD_COMMANDER, outcomes.as_os_str()),
        ],
    )?;
    let served = restarted.elapsed();
    assert!(
        served <= Duration::from_secs(30),
        "reconciled and serving after {served:?}"
    );
    // AC-019-05's bound runs from the restart to the reconcile's end, the new Commander's session bound and its first
    // input given included (M2T17R-56): one 30 s from the restart, not another 30 s from serving.
    let end = restarted + Duration::from_secs(30);
    let delivered = |project: &Path| -> R<bool> {
        Ok(
            query(&db(project), commander)? == "pi/2/R/session/commander/2"
                && query(&db(project), notice)? == "ACKED"
                && outcomes.exists(),
        )
    };
    while !delivered(&project)? {
        assert!(
            Instant::now() < end,
            "not reconciled 30 s after the restart: {} / {}",
            query(&db(&project), commander)?,
            query(&db(&project), notice)?
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        query(
            &db(&project),
            "SELECT count(*) FROM sessions WHERE role = 'commander' AND agent = 'pi'"
        )?,
        "1"
    );
    assert_eq!(
        std::fs::read_to_string(&log)?.lines().count(),
        1,
        "one first input"
    );
    let got = std::fs::read_to_string(&outcomes)?;
    assert_eq!(
        got,
        "old-dispatch rejected \"STALE_ROLE_EPOCH\"\nold-transition rejected \"STALE_ROLE_EPOCH\"\n\
         old-steer rejected \"STALE_ROLE_EPOCH\"\n"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT group_concat(state || ':' || reason_code, ',') FROM (SELECT * FROM decisions \
             WHERE decision_id IN ('old-dispatch', 'old-transition', 'old-steer') ORDER BY decision_id)"
        )?,
        "REJECTED:STALE_ROLE_EPOCH,REJECTED:STALE_ROLE_EPOCH,REJECTED:STALE_ROLE_EPOCH"
    );
    assert_eq!(
        query(
            &db(&project),
            "SELECT (SELECT count(*) FROM attempts) || '/' || (SELECT count(*) FROM outbox \
             WHERE json_extract(payload_json, '$.kind') = 'dispatch') || '/' || \
             (SELECT state || revision FROM tasks WHERE task_id = 'T')"
        )?,
        "0/0/ACCEPTED1",
        "nothing dispatched, T unchanged"
    );
    assert_eq!(
        query(&db(&project), commander)?,
        "pi/2/R/session/commander/2"
    );
    drop(second);
    Ok(())
}

/// Bind every built-in role whose agent is not `agent` to it, in the project's `.hoh/config.toml` (appended; a new run
/// reads its team from it).
fn team_on(project: &Path, agent: &str) -> R {
    use std::io::Write as _;
    std::fs::create_dir_all(project.join(".hoh"))?;
    let file = project.join(".hoh/config.toml");
    let mut text = if file.exists() {
        String::new()
    } else {
        "config_version = \"1.0\"\n".to_owned()
    };
    for (role, binding) in hoh_core::config::Config::default().team {
        if binding.agent.as_str() != agent {
            text.push_str(&format!("[team.{role}]\nagent = \"{agent}\"\n"));
        }
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file)?
        .write_all(text.as_bytes())?;
    Ok(())
}

/// TEST-021-03's world: run R RUNNING on s1 with its Commander bound; the supervisor took in R's events up to seq 120
/// (its cursor): among them need_decision events of the same key (seq 100) and of another (110), and a Commander
/// output (115) its decision ended (116, APPLIED); seqs 121 to 125 are five need_decision events of one key, committed
/// and not taken in (M2T08R-52: what is up to the cursor would wake, or be disposed of, if taken in again).
fn supervised_world(project: &Path) -> R {
    seed(
        project,
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', :p, printf('%064d', 1), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
           ('R', :p, 's1', 'RUNNING', 'AUTO', 3, '2026-11-07T00:00:00Z');
         INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc', 'codex', 'commander', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-07T00:00:00Z');
         INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, payload_json, state) VALUES
           ('d-before', 'R', 1, 2, '{\"kind\": \"record_assumption\", \"statement\": \"s\", \"basis\": \"b\"}', 'APPLIED');",
    )?;
    seed(
        project,
        "WITH RECURSIVE n(i) AS (SELECT (SELECT coalesce(max(seq), 0) FROM events) + 1 UNION ALL SELECT i + 1 FROM n WHERE i < 120)
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
           SELECT :p, 'filler/' || i, 'R',
             CASE i WHEN 100 THEN 'need_decision' WHEN 110 THEN 'need_decision' WHEN 115 THEN 'supervisor.output'
               WHEN 116 THEN 'decision.applied' ELSE 'token.delta' END,
             CASE i WHEN 100 THEN json_object('task_id', 'T', 'fingerprint', 'fp-T')
               WHEN 110 THEN json_object('task_id', 'U', 'fingerprint', 'fp-other')
               WHEN 115 THEN json_object('output_id', 'R/old/output/0', 'attempt_id', 'R/old', 'line', 0, 'text', '{}')
               WHEN 116 THEN json_object('decision_id', 'd-before', 'output_id', 'R/old/output/0')
               ELSE '{}' END,
             '2026-11-07T00:00:00Z' FROM n;
         INSERT INTO supervisor_cursor(run_id, last_handled_seq) VALUES ('R', 120);
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 5)
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at)
           SELECT :p, 'nd/' || i, 'R', 'need_decision', json_object('task_id', 'T', 'fingerprint', 'fp-T', 'n', i),
             '2026-11-07T00:00:01Z' FROM n;",
    )
}

/// AC-021-03: a host killed with SIGKILL as its supervisor's step begins (cursor 120, seqs 121 to 125 committed and not
/// taken in); the next host takes in only seqs after the cursor: one trigger with seqs 121 to 125, none for a seq up to
/// 120 (the same key's 100 not joined, the other key's 110 no trigger), no new APPLIED decision and the output its
/// decision ended not disposed of again, and the cursor at least 120.
#[test]
fn test_021_03_a_host_killed_in_its_supervisors_step_wakes_once_for_what_it_had_not_taken_in() -> R
{
    let dir = tempfile::tempdir()?;
    let project = dir.path().join("p");
    let work = dir.path().join("work");
    std::fs::create_dir_all(&project)?;
    std::fs::create_dir_all(&work)?;
    let (code, stderr) = serve_and_stop(&project)?;
    assert_eq!(code, 0, "{stderr}");
    supervised_world(&project)?;
    let seqs = python(
        &db(&project),
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); \
         print(json.dumps([r[0] for r in c.execute(\"SELECT seq FROM events WHERE kind = 'need_decision' ORDER BY seq\")]))",
        "",
    )?;
    assert_eq!(seqs.trim(), "[100, 110, 121, 122, 123, 124, 125]");
    // The first host dies by SIGKILL as its supervisor's first step begins.
    let killed = start_child(
        &project,
        &[
            (CHILD_WORK, work.as_os_str()),
            (CHILD_KILL, std::ffi::OsStr::new("Supervise:Begun")),
        ],
    );
    let status = match killed {
        Ok(guard) => wait(guard, Duration::from_secs(30))?.0,
        Err(e) => return Err(format!("the first host: {e}").into()),
    };
    assert_eq!(
        std::os::unix::process::ExitStatusExt::signal(&status),
        Some(9),
        "{status:?}"
    );
    let read = |sql: &str| -> R<String> {
        Ok(python(
            &db(&project),
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1]).execute(sys.stdin.read()).fetchone(); \
             print('' if r is None or r[0] is None else r[0])",
            sql,
        )?
        .trim()
        .to_owned())
    };
    assert_eq!(
        read("SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = 'R'")?,
        "120"
    );
    assert_eq!(read("SELECT count(*) FROM supervisor_triggers")?, "0");
    // The next host.
    let host = start_child(&project, &[(CHILD_WORK, work.as_os_str())])?;
    let end = Instant::now() + Duration::from_secs(30);
    while read("SELECT count(*) FROM events WHERE kind = 'supervisor.woken'")? != "1" {
        if Instant::now() > end {
            return Err("no wake after the restart".into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(host);
    let triggers = read(
        "SELECT json_group_array(json_object('seqs', json(event_seqs_json), 'state', state)) FROM supervisor_triggers",
    )?;
    let triggers: Value = serde_json::from_str(&triggers)?;
    assert_eq!(
        triggers,
        json!([{"seqs": [121, 122, 123, 124, 125], "state": "HANDLED"}])
    );
    assert_eq!(
        read("SELECT count(*) FROM decisions WHERE state = 'APPLIED'")?,
        "1"
    );
    assert_eq!(
        read(
            "SELECT group_concat(seq) FROM events WHERE json_extract(payload_json, '$.output_id') = 'R/old/output/0'"
        )?,
        "115,116",
        "the output its decision ended is not disposed of again"
    );
    let cursor: i64 =
        read("SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = 'R'")?.parse()?;
    assert!(cursor >= 125, "{cursor}");
    keep(
        "test-021-03.json",
        &json!({"triggers": triggers, "cursor": cursor, "need_decision_seqs": [121, 122, 123, 124, 125]}),
    )?;
    Ok(())
}
