//! `hoh -p '<goal>' --detach` and `hoh attach [run_id]` (REQ-005 behaviours 1, 3, 5, 7 and failure modes 1-3;
//! docs/adr/ADR-021.md decisions 5, 6): a detach prints its run only once the StartRun is committed; an attach picks
//! its run (never the newest of several on its own), then follows it, and leaving only unsubscribes: nothing is sent.

use crate::cli::Output;
use crate::client::{self, Failure, HostInfo};
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The exit code for a run that ended in `state`, if it ended (REQ-004).
fn ended(state: &str) -> Option<u8> {
    match state {
        "COMPLETED" => Some(0),
        "FAILED" => Some(1),
        "CANCELLED" => Some(5),
        _ => None,
    }
}

fn fail(failure: &Failure) -> u8 {
    say!("hoh: {}: {}", failure.code, failure.message);
    failure.exit
}

fn print(text: &str) {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

/// `hoh -p '<goal>' --detach`: the run's id (text) or the accepted command (json, the 202 body) on stdout, printed only
/// after the StartRun committed; on any failure stdout stays empty and stderr names the code (AC-005-05).
pub fn detach(project: &Path, goal: &str, output: Output) -> u8 {
    let host = match client::ensure(project) {
        Ok(host) => host,
        Err(failure) => return fail(&failure),
    };
    let command = json!({
        "expected_revision": 0,
        "kind": "StartRun",
        "run_id": null,
        "payload": {"goal": goal, "mode": "AUTO", "interactive": false},
    });
    match client::submit(project, &host, command) {
        Ok(accepted) => {
            let run_id = accepted["run_id"].as_str().unwrap_or_default().to_owned();
            if output == Output::Json {
                print(&format!("{accepted}\n"));
            } else {
                print(&format!("{run_id}\n"));
                say!("hoh: 已脱离，Run 在后台运行；用 hoh attach {run_id} 重新附着");
            }
            0
        }
        Err(failure) => fail(&failure),
    }
}

/// The signal that ended an attach, as an exit code, or 0.
fn signals() -> Arc<AtomicUsize> {
    let got = Arc::new(AtomicUsize::new(0));
    for (signal, code) in [
        (signal_hook::consts::SIGINT, 130),
        (signal_hook::consts::SIGTERM, 143),
        (signal_hook::consts::SIGHUP, 129),
    ] {
        let got = got.clone();
        // A handler that only stores: async-signal-safe.
        let _ = signal_hook::flag::register_usize(signal, got, code);
    }
    got
}

/// The run `run_id` names, or why not.
fn run_of(host: &HostInfo, run_id: &str) -> Result<Value, u8> {
    match client::call(host, "GET", &format!("/v1/runs/{run_id}"), None) {
        Ok(reply) if reply.status == 200 => Ok(reply.json()),
        Ok(reply) if reply.status == 404 => {
            say!("hoh: RUN_NOT_FOUND: 没有 Run {run_id}");
            Err(64)
        }
        Ok(reply) => {
            let error = reply.json();
            Err(fail(&Failure::unreachable(format!(
                "the host answered {}: {}",
                reply.status,
                error["message"].as_str().unwrap_or_default()
            ))))
        }
        Err(e) => Err(fail(&Failure::unreachable(format!(
            "the host did not answer: {e}"
        )))),
    }
}

/// `hoh attach [run_id]`.
pub fn attach(project: &Path, run_id: Option<&str>, output: Output) -> u8 {
    let got = signals();
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
    let host = match client::ensure(project) {
        Ok(host) => host,
        Err(failure) => return fail(&failure),
    };
    let run = match run_id {
        Some(id) => match run_of(&host, id) {
            Ok(run) => run,
            Err(code) => return code,
        },
        None => {
            let page = match client::call(&host, "GET", "/v1/runs?limit=500", None) {
                Ok(reply) if reply.status == 200 => reply.json(),
                Ok(reply) => {
                    return fail(&Failure::unreachable(format!(
                        "the host answered {}",
                        reply.status
                    )));
                }
                Err(e) => {
                    return fail(&Failure::unreachable(format!(
                        "the host did not answer: {e}"
                    )));
                }
            };
            let open: Vec<&Value> = page["items"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter(|r| ended(r["state"].as_str().unwrap_or_default()).is_none())
                        .collect()
                })
                .unwrap_or_default();
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
    {
        use std::io::IsTerminal as _;
        if output == Output::Text
            && std::io::stdin().is_terminal()
            && std::io::stdout().is_terminal()
        {
            return crate::tui::attach(project, host, &run);
        }
    }
    if output == Output::Json {
        let mut snapshot = run.clone();
        snapshot["label"] = run["projection"]["label"].clone();
        snapshot["last_event_seq"] = run["watermark"].clone();
        print(&format!("{snapshot}\n"));
        return 0;
    }
    follow(project, host, &run_id, output, &got)
}

/// Follow `run_id`'s events from the first, then live, until the run ends or a signal comes (which only unsubscribes:
/// no command, no signal to any agent). stream-json: one event per line on stdout; text: one line per event on
/// stderr, the run's end on stdout.
fn follow(
    project: &Path,
    mut host: HostInfo,
    run_id: &str,
    output: Output,
    got: &Arc<AtomicUsize>,
) -> u8 {
    let mut last = 0i64;
    let mut stream: Option<client::Events> = None;
    let mut checked = std::time::Instant::now();
    loop {
        let signal = got.load(Ordering::SeqCst);
        if signal != 0 {
            return u8::try_from(signal).unwrap_or(1);
        }
        let events = match stream.as_mut() {
            Some(events) => events,
            None => match client::Events::open(&host, run_id, last) {
                Ok(events) => stream.insert(events),
                Err(_) => {
                    // The host went: the next one is found or started, and the stream goes on after `last`.
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    match client::ensure(project) {
                        Ok(again) => host = again,
                        Err(failure) => return fail(&failure),
                    }
                    continue;
                }
            },
        };
        match events.next() {
            Ok(Some(event)) => {
                last = event["seq"].as_i64().unwrap_or(last).max(last);
                let kind = event["kind"].as_str().unwrap_or_default().to_owned();
                if output == Output::StreamJson {
                    print(&format!("{event}\n"));
                } else {
                    say!("#{last} {kind}");
                }
                if kind.starts_with("run.") {
                    let state = run_of(&host, run_id)
                        .ok()
                        .and_then(|r| r["state"].as_str().map(str::to_owned))
                        .unwrap_or_default();
                    if let Some(code) = ended(&state) {
                        if output == Output::Text {
                            print(&format!("{run_id} {state}\n"));
                        }
                        return code;
                    }
                }
            }
            Ok(None) => {
                // Nothing new for a while: a run that had already ended when attached (or ended with no run event)
                // ends the attach once its events are shown.
                if checked.elapsed() >= std::time::Duration::from_secs(1) {
                    checked = std::time::Instant::now();
                    let state = run_of(&host, run_id)
                        .ok()
                        .and_then(|r| r["state"].as_str().map(str::to_owned))
                        .unwrap_or_default();
                    if let Some(code) = ended(&state) {
                        if output == Output::Text {
                            print(&format!("{run_id} {state}\n"));
                        }
                        return code;
                    }
                }
            }
            Err(_) => stream = None,
        }
    }
}
