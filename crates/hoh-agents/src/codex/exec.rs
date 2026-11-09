//! One turn over `codex exec --json`, when the App Server is not there (see the module docs of [`super`]). Its event
//! names start with `exec-`, so they never collide with the App Server's of the same attempt (M1T04R-03).

use super::{
    Ending, Mode, Outcome, Steer, SteerReceipt, Turn, failure_of, hard_stop, hashes,
    interrupt_first, is_text_item, verdict,
};
use crate::adapter::{AdapterError, AttemptRef, Recorder, Stop, finish, now, step};
use crate::frame::Frame;
use crate::known;
use crate::process::{Cause, Config, Event, Process};
use hoh_core::lifecycle::AttemptState;
use hoh_core::persistence::{AttemptFacts, CancelLatch, Store};
use serde_json::{Value, json};
use std::process::Command;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

/// The raw log files of an exec turn.
pub(super) fn logs(turn: &Turn) -> (std::path::PathBuf, std::path::PathBuf) {
    (
        turn.logs.join("exec.stdout.jsonl"),
        turn.logs.join("exec.stderr.log"),
    )
}

/// The process settings of an exec turn: interrupt first, the turn's deadline.
pub(super) fn config(turn: &Turn) -> Config {
    let (stdout, stderr) = logs(turn);
    Config {
        escalation: interrupt_first(turn.escalation),
        deadline: turn.deadline,
        stdout_copy: Some(stdout),
        stderr_copy: Some(stderr),
        ..Config::default()
    }
}

pub(super) async fn run(
    store: &Store,
    attempt: &AttemptRef,
    turn: &Turn,
    args: Vec<String>,
    degraded: String,
    cancel: &CancelLatch,
    steers: &mut mpsc::Receiver<Steer>,
) -> Result<Outcome, AdapterError> {
    use AttemptState::*;
    let (stdout, stderr) = logs(turn);
    let mut command = Command::new(&turn.launch.program);
    command.args(&args).current_dir(&turn.launch.cwd);
    let mut process = match Process::spawn(command, config(turn)) {
        Ok(p) => p,
        Err(e) => {
            finish(
                store,
                attempt,
                Starting,
                Failed,
                AttemptFacts {
                    ended_at: now(),
                    failure_reason: Some(format!("spawn: {e}")),
                    ..AttemptFacts::default()
                },
                cancel,
            )
            .await?;
            return Err(e.into());
        }
    };
    let pid = process.pid();
    step(
        store,
        attempt,
        Starting,
        Running,
        crate::identity::running_pid(pid),
    )
    .await?;
    let capabilities = super::session(attempt, turn, false, None).capabilities;
    let mut recorder = Recorder::Store {
        store,
        attempt,
        agent: "codex",
        capabilities: &capabilities,
    };
    let driven = drive(
        &mut recorder,
        attempt,
        turn,
        &mut process,
        turn.prompt.as_bytes().to_vec(),
        Stop::Latch(cancel),
        steers,
    )
    .await?;
    let state = finish(
        store,
        attempt,
        Running,
        driven.state,
        AttemptFacts {
            ended_at: now(),
            failure_reason: driven.failure.clone(),
            cancel_requested: Some(driven.cancelled),
            usage: driven.usage.clone(),
            ..AttemptFacts::default()
        },
        cancel,
    )
    .await?;
    let (stdout_sha256, stderr_sha256) = hashes(&stdout, &stderr)?;
    let exit = driven.report.as_ref().and_then(|r| r.status);
    Ok(Outcome {
        failure: if state == Cancelled {
            None
        } else {
            driven.failure
        },
        state,
        mode: Mode::Exec,
        args,
        native_session: driven.thread_id,
        turn_id: None,
        model: None,
        status: driven.status,
        reply: driven.reply,
        exit,
        stdout,
        stderr,
        stdout_sha256,
        stderr_sha256,
        items: driven.items,
        unknown_events: driven.unknown,
        degraded: Some(degraded),
        pid: Some(pid),
        max_buffered: driven.report.as_ref().map(|r| r.stdout.max_buffered),
        steps: driven.report.map(|r| r.steps).unwrap_or_default(),
    })
}

/// What an exec turn showed, and the adapter's verdict.
pub(super) struct Driven {
    pub(super) state: AttemptState,
    pub(super) failure: Option<String>,
    pub(super) cancelled: bool,
    pub(super) thread_id: Option<String>,
    pub(super) usage: Option<Value>,
    pub(super) status: Option<String>,
    pub(super) reply: Option<String>,
    pub(super) items: usize,
    pub(super) unknown: usize,
    pub(super) report: Option<crate::process::ExitReport>,
}

/// Write `input` and read one exec turn's output until its process ended, recording what it shows; a cancel from
/// `stop` interrupts it (the direct mode then escalates as the turn's plan says; the runner's sends SIGINT only).
pub(super) async fn drive(
    recorder: &mut Recorder<'_>,
    attempt: &AttemptRef,
    turn: &Turn,
    process: &mut Process,
    input: Vec<u8>,
    stop: Stop<'_>,
    steers: &mut mpsc::Receiver<Steer>,
) -> Result<Driven, AdapterError> {
    use AttemptState::*;
    let id = &attempt.attempt_id;
    let cancel_now = |process: &Process| {
        if stop.is_native() {
            process.signal_leader(crate::process::Signal::Interrupt);
        } else {
            process.cancel(interrupt_first(turn.escalation));
        }
    };
    let pipe = process.take_stdin();
    let mut feed = std::pin::pin!(async {
        let mut pipe = pipe.ok_or_else(|| std::io::Error::other("no stdin pipe"))?;
        pipe.write_all(&input).await?;
        pipe.shutdown().await
    });
    let mut fed: Option<std::io::Result<()>> = None;
    let mut cancelled = stop.requested_now();
    if cancelled {
        cancel_now(process);
    }
    let (mut thread_id, mut usage) = (None::<String>, None::<Value>);
    let (mut status, mut error, mut reply) = (None::<String>, None::<Value>, None::<String>);
    let (mut too_large, mut tool_started, mut auth_hint) = (false, false, false);
    let (mut items, mut unknown, mut diagnostics, mut errors) = (0, 0, 0, 0);
    let mut steers_open = true;
    let report = loop {
        let event = tokio::select! {
            biased;
            () = stop.requested(), if !cancelled => {
                cancelled = true;
                cancel_now(process);
                continue;
            }
            written = &mut feed, if fed.is_none() => {
                fed = Some(written);
                continue;
            }
            steer = steers.recv(), if steers_open => {
                match steer {
                    // exec takes no input mid-turn: the input stays QUEUED for a later turn.
                    Some(s) => { let _ = s.reply.send(SteerReceipt::CapabilityUnsupported); }
                    None => steers_open = false,
                }
                continue;
            }
            event = process.next() => event,
        };
        let line = match event {
            Some(Event::Frame(Frame::Line(bytes) | Frame::Partial(bytes))) => bytes,
            Some(Event::Frame(Frame::TooLarge(refused))) => {
                too_large = true;
                process.cancel(hard_stop(turn.escalation));
                recorder
                    .event(
                        "exec-frame-too-large",
                        "adapter.diagnostic",
                        json!({"attempt_id": id, "reason": "PROTOCOL_FRAME_TOO_LARGE",
                               "length": refused.length, "prefix_sha256": refused.prefix_sha256,
                               "head": String::from_utf8_lossy(&refused.head)}),
                    )
                    .await?;
                continue;
            }
            Some(Event::Exit(report)) => break Some(*report),
            None => break None,
        };
        let Some(frame) = serde_json::from_slice::<Value>(&line)
            .ok()
            .filter(Value::is_object)
        else {
            diagnostics += 1;
            recorder
                .event(
                    &format!("exec-diagnostic-{diagnostics}"),
                    "adapter.diagnostic",
                    json!({"attempt_id": id, "reason": "stdout line is not a JSON object", "bytes": line.len(),
                           "head": String::from_utf8_lossy(&line[..line.len().min(256)])}),
                )
                .await?;
            continue;
        };
        let kind = frame["type"].as_str().unwrap_or_default();
        match kind {
            "thread.started" => {
                thread_id = frame["thread_id"].as_str().map(str::to_owned);
                if let Some(thread) = &thread_id {
                    recorder.native_session(thread).await?;
                }
            }
            "item.started" | "item.completed" => {
                items += 1;
                let item = &frame["item"];
                let item_type = item["type"].as_str().unwrap_or_default();
                recorder
                    .event(
                        &format!("exec-item-{items}"),
                        "adapter.item",
                        json!({"attempt_id": id, "method": kind, "threadId": thread_id, "turnId": null,
                               "itemId": item["id"], "itemType": item_type}),
                    )
                    .await?;
                if kind == "item.completed" && item_type == "agent_message" {
                    reply = item["text"].as_str().map(str::to_owned);
                }
                if kind == "item.started" && !is_text_item(item_type) {
                    tool_started = true;
                }
            }
            "turn.completed" => {
                status = Some("completed".to_owned());
                usage = frame.get("usage").filter(|u| u.is_object()).cloned();
            }
            "turn.failed" => {
                status = Some("failed".to_owned());
                error = Some(frame["error"].clone()).filter(|e| !e.is_null());
            }
            "error" => {
                errors += 1;
                let message = frame["message"].as_str().unwrap_or_default();
                auth_hint |= message.contains("unexpected status 401");
                recorder
                    .event(
                        &format!("exec-error-{errors}"),
                        "adapter.error",
                        json!({"attempt_id": id, "error": {"message": message}}),
                    )
                    .await?;
            }
            // exec's event types (`--json`, the version's registry); any other is kept as an `adapter.unknown_event`.
            _ if known::codex_exec().contains(kind) => {}
            _ => {
                unknown += 1;
                recorder
                    .event(
                        &format!("exec-unknown-{unknown}"),
                        "adapter.unknown_event",
                        json!({"attempt_id": id, "method": kind, "key": kind,
                               "frame_sha256": known::frame_sha256(&line), "frame": frame}),
                    )
                    .await?;
            }
        }
    };
    let logs_incomplete = report.as_ref().is_none_or(|r| {
        r.stdout.error.is_some() || r.stderr.error.is_some() || !r.stdout.eof || !r.stderr.eof
    });
    if logs_incomplete && let Some(r) = &report {
        recorder
            .event(
                "exec-raw-log-incomplete",
                "adapter.diagnostic",
                json!({"attempt_id": id, "reason": "RAW_LOG_INCOMPLETE",
                       "stdout": {"eof": r.stdout.eof, "error": r.stdout.error},
                       "stderr": {"eof": r.stderr.eof, "error": r.stderr.error}}),
            )
            .await?;
    }
    let exit = report.as_ref().and_then(|r| r.status);
    let (state, failure) = match verdict(&Ending {
        cancelled,
        too_large,
        deadline: report
            .as_ref()
            .is_some_and(|r| r.cause == Cause::DeadlineExceeded),
        logs_incomplete,
        failure: None,
        status: status.as_deref(),
        error: error.as_ref(),
        auth_hint,
        tool_started,
        exit_matters: true,
        exit,
    }) {
        // A prompt not written whole is no success of that prompt.
        (Settled, _) if !matches!(fed, Some(Ok(()))) => {
            (Failed, Some("PROMPT_NOT_DELIVERED".to_owned()))
        }
        judged => judged,
    };
    if status.as_deref() == Some("failed") {
        let reason = failure_of(error.as_ref(), auth_hint);
        recorder
            .event(
                "exec-turn-failed",
                "adapter.turn_failed",
                json!({"attempt_id": id, "status": status, "error": error,
                       "blocked_reason": (reason == "BLOCKED_AUTH").then_some("BLOCKED_AUTH"),
                       "retry": if reason == "TRANSIENT_EXTERNAL" { "transient" } else { "none" }}),
            )
            .await?;
    }
    Ok(Driven {
        state,
        failure,
        cancelled,
        thread_id,
        usage,
        status,
        reply,
        items,
        unknown,
        report,
    })
}
