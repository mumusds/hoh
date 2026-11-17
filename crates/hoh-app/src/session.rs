//! `hoh attach [run_id]` (REQ-005 behaviours 1, 3, 5, 7 and failure modes 1-3; docs/adr/ADR-021.md decisions 5, 6;
//! docs/adr/ADR-031.md decision 12): an attach picks its run (never the newest of several on its own), then shows its
//! result as of now (json) or follows it, and leaving only unsubscribes: nothing is sent.

use crate::cli::Output;
use crate::client::{self, Failure, HostInfo};
use crate::follow::{End, Follower, Replay, Watch};
use serde_json::Value;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

/// Whether a run in `state` can be attached to: it is not in a terminal state (ADR-021:85; INCOMPLETE can).
fn attachable(state: &str) -> bool {
    !crate::follow::TERMINAL.contains(&state)
}

/// Whether stdin and stdout are both a terminal: the TUI's condition.
fn terminal() -> bool {
    use std::io::IsTerminal as _;
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Say why the client could not do what it was asked (the code, the message and any `details`); its exit code.
pub fn fail(failure: &Failure) -> u8 {
    say!("hoh: {}: {}", failure.code, failure.message);
    if !failure.details.is_null() {
        say!("hoh: details: {}", failure.details);
    }
    failure.exit
}

/// The signal that ended a client, as an exit code, or 0: SIGINT 130, SIGTERM 143, SIGHUP 129, each only stored (it
/// ends the client's wait, never the run).
pub fn signals() -> Arc<AtomicUsize> {
    let got = Arc::new(AtomicUsize::new(0));
    use crate::exit::{ClientEvent, client_code};
    for (signal, event) in [
        (signal_hook::consts::SIGINT, ClientEvent::Sigint),
        (signal_hook::consts::SIGTERM, ClientEvent::Sigterm),
        (signal_hook::consts::SIGHUP, ClientEvent::Sighup),
    ] {
        let got = got.clone();
        // A handler that only stores: async-signal-safe.
        let _ = signal_hook::flag::register_usize(signal, got, usize::from(client_code(event)));
    }
    got
}

/// The run `run_id` names, or why not (64 when there is none, said on stderr).
pub fn run_of(host: &HostInfo, run_id: &str) -> Result<Value, u8> {
    match client::get_run(host, run_id) {
        Ok(Some(run)) => Ok(run),
        Ok(None) => {
            say!("hoh: RUN_NOT_FOUND: 没有 Run {run_id}");
            Err(64)
        }
        Err(failure) => Err(fail(&failure)),
    }
}

/// How often a wait for the host looks at the signal flag (the shell's tick).
const LOOK: Duration = Duration::from_millis(100);

/// The project's host ([`client::ensure`], which may start one and wait for it) on a worker thread while this one looks
/// at `signal` every [`LOOK`]: a signal ends the wait with its exit code, `code(signal)`, and a host the worker already
/// started goes on in the background, in its own session (CLT-01; REQ-001 failure mode 3). A host that cannot be had is
/// said, with its code. Without a worker thread the wait is done here, the signal looked at after it.
fn host_unless_signalled(
    project: &Path,
    signal: &AtomicUsize,
    code: impl Fn(usize) -> u8,
) -> Result<HostInfo, u8> {
    let (send, got) = std::sync::mpsc::channel();
    let dir = project.to_path_buf();
    let worker = std::thread::Builder::new()
        .name("hoh-ensure".to_owned())
        .spawn(move || {
            let _ = send.send(client::ensure(&dir));
        });
    if worker.is_err() {
        let host = client::ensure(project).map_err(|failure| fail(&failure))?;
        return match signal.load(Ordering::SeqCst) {
            0 => Ok(host),
            sig => Err(code(sig)),
        };
    }
    loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 {
            return Err(code(sig));
        }
        match got.recv_timeout(LOOK) {
            Ok(Ok(host)) => return Ok(host),
            Ok(Err(failure)) => return Err(fail(&failure)),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(fail(&Failure::unreachable(
                    "the host could not be found or started",
                )));
            }
        }
    }
}

/// The signal state of an attach, registered before anything else and kept to its end (CLT-01): the shell's flags when
/// it may open the Workspace (a terminal and text), the follower's otherwise.
enum Signals {
    Shell(crate::tui::Flags),
    Follow(Arc<AtomicUsize>),
}

/// `hoh attach [run_id]`.
pub fn attach(project: &Path, run_id: Option<&str>, output: Output) -> u8 {
    let signals = if output == Output::Text && terminal() {
        match crate::tui::register() {
            Ok(flags) => Signals::Shell(flags),
            Err(code) => return code,
        }
    } else {
        Signals::Follow(signals())
    };
    // No database: no run, and nothing is started for none.
    if !project.join(".hoh/state.db").exists() {
        return match run_id {
            Some(id) => {
                say!("hoh: RUN_NOT_FOUND: 没有 Run {id}");
                64
            }
            None => {
                say!("hoh: 没有可附着的 Run");
                64
            }
        };
    }
    // The start wait answers a signal (CLT-01): the shell's ends as bare `hoh`'s, the terminal untouched; the follower's
    // says so.
    let waited = match &signals {
        Signals::Shell(flags) => {
            host_unless_signalled(project, flags.signal(), crate::tui::Flags::code)
        }
        Signals::Follow(got) => host_unless_signalled(project, got, |code| {
            say!("hoh: 收到信号，没有附着");
            u8::try_from(code).unwrap_or(1)
        }),
    };
    let host = match waited {
        Ok(host) => host,
        Err(code) => return code,
    };
    let run = match run_id {
        Some(id) => match run_of(&host, id) {
            Ok(run) => run,
            Err(code) => return code,
        },
        None => {
            // Every page: one run not ended on a later page makes two (M2T06R-05).
            let all = match client::runs(&host) {
                Ok(all) => all,
                Err(failure) => return fail(&failure),
            };
            let open: Vec<&Value> = all
                .iter()
                .filter(|r| attachable(r["state"].as_str().unwrap_or_default()))
                .collect();
            match open.as_slice() {
                [one] => match run_of(&host, one["run_id"].as_str().unwrap_or_default()) {
                    Ok(run) => run,
                    Err(code) => return code,
                },
                [] => {
                    say!("hoh: 没有可附着的 Run");
                    return 64;
                }
                several => {
                    // With a terminal, the TUI's selector: the user picks one (docs/adr/ADR-031.md decision 12).
                    if let Signals::Shell(flags) = signals {
                        return crate::tui::pick(project, host, &all, flags);
                    }
                    // Never the newest on its own (REQ-003 behaviour 7): the user names one.
                    let list: Vec<String> = several
                        .iter()
                        .map(|r| {
                            format!(
                                "  {}  {}",
                                r["run_id"].as_str().unwrap_or_default(),
                                r["label"].as_str().unwrap_or_default()
                            )
                        })
                        .collect();
                    say!(
                        "hoh: 有 {} 个可附着的 Run，请用 hoh attach <run_id> 指定其一：\n{}",
                        several.len(),
                        list.join("\n")
                    );
                    return 64;
                }
            }
        }
    };
    let run_id = run["run_id"].as_str().unwrap_or_default().to_owned();
    // A terminal on both sides and text: the Workspace bound to the run (ADR-021 decision 10).
    let got = match signals {
        Signals::Shell(flags) => return crate::tui::attach(project, host, &run, flags),
        Signals::Follow(got) => got,
    };
    if output == Output::Json {
        // The run's result as of now, from the host's own function (decision 12): a snapshot, so 0.
        let snapshot = &run["projection"]["result"];
        if !snapshot.is_object() {
            return fail(&Failure::unreachable(
                "the host gave no result of the run (projection.result)",
            ));
        }
        return match crate::output::json_line(snapshot) {
            Ok(line) => match crate::follow::emit(&format!("{line}\n")) {
                Ok(()) => 0,
                Err(e) => {
                    say!("hoh: stdout 无法写入（{e}）");
                    74
                }
            },
            Err(e) => {
                say!("hoh: 结果无法序列化：{e}");
                1
            }
        };
    }
    // Followed from the first event; the history up to the watermark W0 the attach began at is a replay, and the
    // follow ends at a result past W0 or one in a terminal state (decision 12).
    let w0 = run["watermark"].as_i64().unwrap_or_default();
    let stderr = crate::forward::Stderr::start(project);
    let follower = Follower {
        project,
        run_id: &run_id,
        output,
        signals: &got,
        stderr: &stderr,
        quiet: true,
    };
    let replay = if output == Output::StreamJson {
        Replay::UpTo(w0)
    } else {
        Replay::None
    };
    let mut watch = Watch::new(End::Attached { w0 }, replay, 0);
    let outcome = follower.follow(host, &mut watch);
    let code = crate::follow::conclude(&outcome, output, &run_id, &stderr);
    stderr.finish();
    code
}
