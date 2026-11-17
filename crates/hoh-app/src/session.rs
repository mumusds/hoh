//! `hoh attach [run_id]` (REQ-005 behaviours 1, 3, 5, 7 and failure modes 1-3; docs/adr/ADR-021.md decisions 5, 6;
//! docs/adr/ADR-031.md decision 12): an attach picks its run (never the newest of several on its own), then shows its
//! result as of now (json) or follows it, and leaving only unsubscribes: nothing is sent.

use crate::cli::Output;
use crate::client::{self, Failure, HostInfo};
use crate::follow::{End, Follower, Replay, Watch};
use crate::signals::State;
use serde_json::Value;
use std::path::Path;

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

/// The run `run_id` names, read while the client looks at `state` (docs/adr/ADR-031.md, 设计修正（客户端信号）, rule
/// 3), or why not: a signal's exit code, 64 when there is none (said on stderr), the host's failure (said).
pub fn run_of(state: &State, host: &HostInfo, run_id: &str) -> Result<Value, u8> {
    let read = {
        let (host, run_id) = (host.clone(), run_id.to_owned());
        state.wait(move || client::get_run(&host, &run_id))?
    };
    match read {
        Ok(Some(run)) => Ok(run),
        Ok(None) => {
            say!("hoh: RUN_NOT_FOUND: 没有 Run {run_id}");
            Err(64)
        }
        Err(failure) => Err(fail(&failure)),
    }
}

/// The project's host ([`client::ensure`], which may start one and wait for it), waited for while the client looks at
/// `state` (rule 3): a signal ends the wait with its exit code, and a host already started goes on in the background,
/// in its own session (REQ-001 failure mode 3). A host that cannot be had is said, with its code.
pub fn host(state: &State, project: &Path) -> Result<HostInfo, u8> {
    let dir = project.to_path_buf();
    match state.wait(move || client::ensure(&dir))? {
        Ok(host) => Ok(host),
        Err(failure) => Err(fail(&failure)),
    }
}

/// `hoh attach [run_id]`: its signal state registered before anything else, and its exit consulting it (the design
/// correction, rules 1 and 2): a signal before the attach is said with “没有附着”.
pub fn attach(project: &Path, run_id: Option<&str>, output: Output) -> u8 {
    let state = match crate::signals::register() {
        Ok(state) => state,
        Err(code) => return code,
    };
    let code = attach_with(state, project, run_id, output);
    state.exit(code, "没有附着")
}

fn attach_with(state: &State, project: &Path, run_id: Option<&str>, output: Output) -> u8 {
    // The Workspace with a terminal on both sides and text (ADR-021 decision 10).
    let shell = output == Output::Text && terminal();
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
    let host = match host(state, project) {
        Ok(host) => host,
        Err(code) => return code,
    };
    let run = match run_id {
        Some(id) => match run_of(state, &host, id) {
            Ok(run) => run,
            Err(code) => return code,
        },
        None => {
            // Every page: one run not ended on a later page makes two (M2T06R-05).
            let listed = {
                let host = host.clone();
                state.wait(move || client::runs(&host))
            };
            let all = match listed {
                Ok(Ok(all)) => all,
                Ok(Err(failure)) => return fail(&failure),
                Err(code) => return code,
            };
            let open: Vec<&Value> = all
                .iter()
                .filter(|r| attachable(r["state"].as_str().unwrap_or_default()))
                .collect();
            match open.as_slice() {
                [one] => match run_of(state, &host, one["run_id"].as_str().unwrap_or_default()) {
                    Ok(run) => run,
                    Err(code) => return code,
                },
                [] => {
                    say!("hoh: 没有可附着的 Run");
                    return 64;
                }
                several => {
                    // With a terminal, the TUI's selector: the user picks one (docs/adr/ADR-031.md decision 12).
                    if shell {
                        return crate::tui::pick(project, host, &all, state);
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
    if shell {
        return crate::tui::attach(project, host, &run, state);
    }
    if output == Output::Json {
        // The run's result as of now, from the host's own function (decision 12): a snapshot, so 0.
        let snapshot = &run["projection"]["result"];
        if !snapshot.is_object() {
            return fail(&Failure::unreachable(
                "the host gave no result of the run (projection.result)",
            ));
        }
        // A signal that came while the run was read ends the attach before its snapshot (CLT-101; rule 2).
        if let Some(code) = state.received() {
            return code;
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
    state.note_run(&run_id);
    let stderr = crate::forward::Stderr::start(project);
    let follower = Follower {
        project,
        run_id: &run_id,
        output,
        state,
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
    let code = crate::follow::conclude(&outcome, output, &run_id, &stderr, state);
    stderr.finish();
    code
}
