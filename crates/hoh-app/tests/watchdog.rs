//! The inactivity watchdog's lifetime in the runner (docs/adr/ADR-022.md decision 19, REQ-042 behaviour 8): a look is
//! an operation of its attempt, polled beside its end and never in its way; once the attempt has ended, nothing of a
//! look still in flight is recorded (WD-01).

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{
    Executor, Gate, Host, HostConfig, HostHooks, Outcome, Refusal, RunnerConfig, Started, Watchdog,
    Work,
};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use std::io::Write as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    });
    config
}

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

/// The first column of the first row `sql` gives, as text ("" for none or NULL).
fn query(project: &Path, sql: &str) -> R<String> {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; r=sqlite3.connect(sys.argv[1], timeout=10).execute(sys.argv[2]).fetchone(); \
             print('' if r is None or r[0] is None else r[0])",
        ])
        .arg(db(project))
        .arg(sql)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_owned())
}

/// Run `sql` (with `:p` the host's project id) on the project's database.
fn seed(project: &Path, sql: &str) -> R {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); \
             p=c.execute('SELECT project_id FROM projects').fetchone()[0]; \
             c.executescript(sys.argv[2].replace(':p', repr(p))); c.commit()",
        ])
        .arg(db(project))
        .arg(sql)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(())
}

/// Run R RUNNING on spec s2, its developer bound; task T1 READY for the developer (a reader).
const WORLD: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', :p, printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', :p, 's2', 'RUNNING', 'AUTO', 7, '2026-11-07T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true, \"resume\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-07T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T1', 'R', 's2', 'READY', 1, 'developer', 0, 1, 1);
";

/// An executor whose turn is `sh` reading two lines (its gate's `go`, then `stop`, which ends it), and whose output
/// look blocks until released: a look held on its blocking thread.
#[derive(Debug, Default)]
struct HeldLook {
    looking: AtomicBool,
    release: AtomicBool,
    ended: Arc<AtomicBool>,
    input: Arc<Mutex<Option<ChildStdin>>>,
}

impl Executor for HeldLook {
    fn deliver(&self, _: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = tx.send(Ok(()));
        rx
    }

    fn start(&self, work: &Work) -> Result<Started, Refusal> {
        let mut child = Command::new("sh")
            .args(["-c", "read -r go; read -r stop"])
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| Refusal::from(e.to_string()))?;
        let pid = child.id();
        if let Ok(mut input) = self.input.lock() {
            *input = child.stdin.take();
        }
        let input = self.input.clone();
        let gate = Gate::new(move || {
            let mut input = input.lock().map_err(|e| e.to_string())?;
            let stdin = input.as_mut().ok_or("no stdin")?;
            stdin.write_all(b"go\n").map_err(|e| e.to_string())
        });
        let (tx, rx) = tokio::sync::oneshot::channel();
        let ended = self.ended.clone();
        std::thread::spawn(move || {
            let reaped = child.wait().is_ok();
            let _ = tx.send(Outcome {
                success: reaped,
                detail: "its sh ended".to_owned(),
                group_gone: true,
                class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                observed: Default::default(),
                unknown_effect: false,
                retry_at: None,
            });
            ended.store(true, Ordering::SeqCst);
        });
        Ok(Started {
            pid,
            gate,
            ended: rx,
            prompt: None,
            ran: None,
        })
    }

    fn output_bytes(&self, _: &Work) -> Option<u64> {
        self.looking.store(true, Ordering::SeqCst);
        while !self.release.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        Some(0)
    }
}

async fn until(what: &str, limit: Duration, mut done: impl FnMut() -> R<bool>) -> R {
    let end = Instant::now() + limit;
    while !done()? {
        if Instant::now() > end {
            return Err(format!("not {what} within {limit:?}").into());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_look_held_on_its_thread_neither_holds_up_its_attempts_end_nor_records_after_it() -> R {
    // WD-01: the watchdog's look (here held on its blocking thread) is in flight when the agent ends. The attempt
    // settles at once, the look's result is dropped, and once the look is let go nothing of it is recorded.
    let dir = tempfile::tempdir()?;
    let project = dir.path();
    let first = Host::start(config(project)).await?;
    assert!(first.shutdown().await);
    seed(project, WORLD)?;
    let held = Arc::new(HeldLook::default());
    let mut config = config(project);
    config.runner = Some(RunnerConfig {
        executor: held.clone(),
        context: hoh_core::decision::Context {
            required_capabilities: [(
                "developer".to_owned(),
                ["structured_events".to_owned(), "command_exec".to_owned()].into(),
            )]
            .into(),
            ..Default::default()
        },
    });
    config.watchdog = Watchdog {
        quiet: Duration::from_millis(50),
        every: Duration::from_millis(100),
    };
    let host = Host::start(config).await?;
    let outcome = async {
        until("looking", Duration::from_secs(30), || {
            Ok(held.looking.load(Ordering::SeqCst))
        })
        .await?;
        if let Some(stdin) = held.input.lock().map_err(|e| e.to_string())?.as_mut() {
            stdin.write_all(b"stop\n")?;
        }
        until("ended", Duration::from_secs(10), || {
            Ok(held.ended.load(Ordering::SeqCst))
        })
        .await?;
        let attempt = "SELECT state FROM attempts WHERE task_id = 'T1'";
        until(
            "settled while the look is held",
            Duration::from_secs(3),
            || Ok(query(project, attempt)? == "SETTLED"),
        )
        .await?;
        held.release.store(true, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            query(
                project,
                "SELECT count(*) FROM events WHERE kind LIKE 'attempt.inactive%'"
            )?,
            "0"
        );
        R::Ok(())
    }
    .await;
    held.release.store(true, Ordering::SeqCst);
    if !held.ended.load(Ordering::SeqCst)
        && let Ok(mut input) = held.input.lock()
        && let Some(stdin) = input.as_mut()
    {
        let _ = stdin.write_all(b"stop\n");
    }
    assert!(host.shutdown().await, "the host's stop was not clean");
    outcome
}

/// A proc tree with no processes and a stable `ns_last_pid`: the look's tools are absent, and the termination proof
/// reaches Done after two quiet scans.
#[derive(Debug)]
struct EmptyProc(PathBuf);

impl HostHooks for EmptyProc {
    fn proc_root(&self) -> Option<PathBuf> {
        Some(self.0.clone())
    }
}

/// Spin until `done`, without yielding to the runtime. A deadline only bounds a missed signal.
fn hold_until(what: &str, done: impl Fn() -> bool) -> R {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        if Instant::now() > end {
            return Err(format!("not {what} within 10s").into());
        }
        std::hint::spin_loop();
    }
    Ok(())
}

/// WD-01, the other order: the sample has finished and the end is already in the oneshot, and the attempt task has
/// not polled since either became ready. `select!` without a bias polls the look first about three times in four,
/// and polling it enqueues `runner_watched` before the end is chosen; the record then commits while the attempt is
/// still RUNNING. The end is listed first so a bias keeps the look from being polled once the end is ready.
fn a_finished_look_records_nothing_when_its_end_is_already_ready() -> R {
    let rt = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()?;
    rt.block_on(async {
        let dir = tempfile::tempdir()?;
        let project = dir.path();
        let proc_root = project.join("empty-proc");
        std::fs::create_dir_all(proc_root.join("sys/kernel"))?;
        std::fs::write(proc_root.join("sys/kernel/ns_last_pid"), "1\n")?;
        let first = Host::start(config(project)).await?;
        assert!(first.shutdown().await);
        seed(project, WORLD)?;
        let held = Arc::new(HeldLook::default());
        let mut config = config(project);
        config.hooks = Arc::new(EmptyProc(proc_root));
        config.runner = Some(RunnerConfig {
            executor: held.clone(),
            context: hoh_core::decision::Context {
                required_capabilities: [(
                    "developer".to_owned(),
                    ["structured_events".to_owned(), "command_exec".to_owned()].into(),
                )]
                .into(),
                ..Default::default()
            },
        });
        config.watchdog = Watchdog {
            quiet: Duration::from_millis(1),
            every: Duration::from_millis(20),
        };
        let host = Host::start(config).await?;
        let outcome = async {
            until("looking", Duration::from_secs(30), || {
                Ok(held.looking.load(Ordering::SeqCst))
            })
            .await?;
            // The only blocking thread is inside the look. The next blocking job runs only after that closure has
            // returned and its join handle has been woken, so `sampled` means the sample is ready to poll.
            let sampled = Arc::new(AtomicBool::new(false));
            let flag = sampled.clone();
            let sentinel = tokio::task::spawn_blocking(move || {
                flag.store(true, Ordering::SeqCst);
            });
            if let Some(stdin) = held.input.lock().map_err(|e| e.to_string())?.as_mut() {
                stdin.write_all(b"stop\n")?;
            }
            // Stay on this worker: the attempt task stays queued while the end is sent and the sample then finishes.
            hold_until("ended", || held.ended.load(Ordering::SeqCst))?;
            held.release.store(true, Ordering::SeqCst);
            hold_until("sample joined", || sampled.load(Ordering::SeqCst))?;
            drop(sentinel);
            until("settled", Duration::from_secs(3), || {
                Ok(query(project, "SELECT state FROM attempts WHERE task_id = 'T1'")? == "SETTLED")
            })
            .await?;
            assert_eq!(
                query(
                    project,
                    "SELECT count(*) FROM events WHERE kind LIKE 'attempt.inactive%'"
                )?,
                "0",
                "a look whose sample had finished was recorded after its end was already ready"
            );
            R::Ok(())
        }
        .await;
        held.release.store(true, Ordering::SeqCst);
        if !held.ended.load(Ordering::SeqCst)
            && let Ok(mut input) = held.input.lock()
            && let Some(stdin) = input.as_mut()
        {
            let _ = stdin.write_all(b"stop\n");
        }
        assert!(host.shutdown().await, "the host's stop was not clean");
        outcome
    })
}

#[test]
fn a_finished_look_records_nothing_when_its_end_is_already_ready_on_one_worker() {
    // Several passes: without a bias each one records about three times in four, so a regression fails the set.
    for pass in 0..8 {
        a_finished_look_records_nothing_when_its_end_is_already_ready()
            .unwrap_or_else(|e| panic!("pass {pass}: {e}"));
    }
}
