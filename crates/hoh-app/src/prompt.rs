//! `hoh -p <text>` (REQ-002, REQ-004, REQ-005; docs/adr/ADR-031.md decisions 9-11): one StartRun, or with `--resume`
//! one SendInput to an existing run (and a ResumeRun after it when the run is INCOMPLETE); then, in the foreground,
//! the run's events until the first `run.result` after the client's last accepted command, whose `exit_code` the
//! client exits with. It never opens, reads or polls stdin and never changes the terminal; a signal only ends the
//! client's wait (no command is sent, E8).

use crate::cli::Output;
use crate::client::{self, Failure, HostInfo};
use crate::exit::{ClientEvent, client_code};
use crate::follow::{End, Follower, Outcome, Replay, TERMINAL, Watch};
use crate::forward::Stderr;
use crate::session::{fail, signals};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// `hoh -p <text> [--resume <run>] [--detach] [--output …]` in `project`.
pub fn run(project: &Path, text: &str, output: Output, detach: bool, resume: Option<&str>) -> u8 {
    let got = signals();
    // Before any connection: an invalid configuration starts nothing (REQ-001 failure mode 4, REQ-004 failure mode 4).
    let config = match hoh_core::config::load(project) {
        Ok(config) => config,
        Err(e) => {
            say!("hoh: 配置无效，没有连接或启动 host：{e}");
            return client_code(ClientEvent::Config);
        }
    };
    match resume {
        None => start(
            project,
            text,
            output,
            detach,
            &config.project.default_mode,
            &got,
        ),
        Some(run_id) => answer(project, text, output, detach, run_id, &got),
    }
}

/// The exit code of a signal that came before anything was accepted, said as such.
fn early(got: &AtomicUsize, what: &str) -> Option<u8> {
    let signal = got.load(Ordering::SeqCst);
    (signal != 0).then(|| {
        say!("hoh: 收到信号，{what}");
        u8::try_from(signal).unwrap_or(1)
    })
}

/// Print an accepted command for `--detach`: its 202 body (json) or its run (text).
fn detached(accepted: &Value, output: Output) -> u8 {
    let run_id = accepted["run_id"].as_str().unwrap_or_default();
    let text = match output {
        Output::Json => match crate::output::json_line(accepted) {
            Ok(line) => format!("{line}\n"),
            Err(e) => {
                say!("hoh: 受理结果无法序列化：{e}");
                return 1;
            }
        },
        Output::Text | Output::StreamJson => format!("{run_id}\n"),
    };
    if let Err(e) = crate::follow::emit(&text) {
        say!("hoh: stdout 无法写入（{e}）；命令已受理，Run {run_id} 在后台运行");
        return client_code(ClientEvent::TerminalIo);
    }
    if output == Output::Text {
        say!("hoh: 已脱离，Run 在后台运行；用 hoh attach {run_id} 重新附着");
    }
    0
}

/// A new run: StartRun with the text as its goal, the configured mode, not interactive (REQ-002 behaviour 1).
fn start(
    project: &Path,
    text: &str,
    output: Output,
    detach: bool,
    mode: &str,
    got: &AtomicUsize,
) -> u8 {
    let host = match client::ensure(project) {
        Ok(host) => host,
        Err(failure) => return fail(&failure),
    };
    if let Some(code) = early(got, "没有提交命令") {
        return code;
    }
    let command = json!({
        "expected_revision": 0,
        "kind": "StartRun",
        "run_id": null,
        "payload": {"goal": text, "mode": mode, "interactive": false},
    });
    let accepted = match client::submit(project, &host, command) {
        Ok(accepted) => accepted,
        Err(failure) => return fail(&failure),
    };
    if detach {
        return detached(&accepted, output);
    }
    let run_id = accepted["run_id"].as_str().unwrap_or_default().to_owned();
    let command_id = accepted["command_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    // A new run's first event is its acceptance: nothing replays (only stream-json shows the mark at all).
    let replay = Replay::Before {
        command_id: command_id.clone(),
    };
    let stderr = Stderr::start(project);
    let follower = Follower {
        project,
        run_id: &run_id,
        output,
        signals: got,
        stderr: &stderr,
        quiet: false,
    };
    let mut watch = Watch::new(End::Accepted { command_id }, replay, 0);
    let outcome = follower.follow(host, &mut watch);
    let code = crate::follow::conclude(&outcome, output, &run_id, &stderr);
    if let Outcome::Result(result) = &outcome {
        questions(&client::running(project), &run_id, result, &stderr);
    }
    stderr.finish();
    code
}

/// Said when a run in a terminal state is asked to take input: it cannot be reopened (REQ-002 failure mode 3; for
/// FAILED and CANCELLED, decision 9's reading of it).
fn ended(run_id: &str, state: &str) -> u8 {
    say!("hoh: Run {run_id} 已结束（{state}），不能重开；请用 hoh -p '<文本>' 新建 Run");
    client_code(ClientEvent::Usage)
}

/// Submit the command `build` makes for the run's revision, reading the run again and submitting anew while the host
/// says the revision is stale (`client::fresh`, decision 9). A run found ended meanwhile is 64.
fn fresh(
    project: &Path,
    host: &HostInfo,
    run_id: &str,
    revision: i64,
    build: impl Fn(i64) -> Value,
) -> Result<Value, Failure> {
    client::fresh(
        |command| client::submit(project, host, command),
        || client::get_run(host, run_id),
        run_id,
        revision,
        build,
    )
}

/// Whether `failure` is a ResumeRun refused because the INCOMPLETE run's stop still holds (decision 16's
/// `details: {state: INCOMPLETE, stop}`, the stop named): the input stays accepted and the run INCOMPLETE. A 409 that
/// names INCOMPLETE but no stop is another refusal, with its own exit (M2T13R-50).
pub(crate) fn still_stopped(failure: &Failure) -> bool {
    failure.status == Some(409)
        && failure.code == "ILLEGAL_TRANSITION"
        && failure.details["state"] == "INCOMPLETE"
        && failure.details["stop"].is_string()
}

/// The stop a refused ResumeRun names (decision 16's `details.stop`): the budget item or the deadline still exhausted.
pub(crate) fn stop_of(failure: &Failure) -> String {
    match &failure.details["stop"] {
        Value::String(stop) => stop.clone(),
        Value::Null => "未说明".to_owned(),
        other => other.to_string(),
    }
}

/// What a ResumeRun refused by a stop still holding is said with: the stop, and that no entry raises a run's budget
/// or deadline in this version (decision 9).
fn refused_resume(failure: &Failure) -> String {
    let stop = stop_of(failure);
    format!(
        "hoh: Run 未恢复：使它停下的预算或期限仍耗尽（{stop}）；本版本没有提高一个 Run 的预算或期限的入口，\
         请以 hoh -p '<文本>' 新建 Run 继续剩余需求"
    )
}

/// `hoh -p <text> --resume <run_id>`: the text as SendInput{REPLY} to the run (an answer to its one open question,
/// else queued as STEER, decision 7), then ResumeRun when it was INCOMPLETE; followed from the client's last accepted
/// command (E7).
fn answer(
    project: &Path,
    text: &str,
    output: Output,
    detach: bool,
    run_id: &str,
    got: &AtomicUsize,
) -> u8 {
    // No database: no run, and nothing is started for none.
    if !project.join(".hoh/state.db").exists() {
        say!("hoh: RUN_NOT_FOUND: 没有 Run {run_id}");
        return client_code(ClientEvent::Usage);
    }
    let host = match client::ensure(project) {
        Ok(host) => host,
        Err(failure) => return fail(&failure),
    };
    if let Some(code) = early(got, "没有提交命令") {
        return code;
    }
    let run = match crate::session::run_of(&host, run_id) {
        Ok(run) => run,
        Err(code) => return code,
    };
    // 64 only from the state read, never from a message (M2T13D-54).
    let state = run["state"].as_str().unwrap_or_default().to_owned();
    if TERMINAL.contains(&state.as_str()) {
        return ended(run_id, &state);
    }
    let watermark = run["watermark"].as_i64().unwrap_or_default();
    let input = match fresh(
        project,
        &host,
        run_id,
        run["revision"].as_i64().unwrap_or_default(),
        |revision| {
            json!({
                "expected_revision": revision,
                "kind": "SendInput",
                "run_id": run_id,
                "payload": {"text": text, "target_role": null, "input_kind": "REPLY"},
            })
        },
    ) {
        Ok(accepted) => accepted,
        Err(failure) => return fail(&failure),
    };
    // An INCOMPLETE run is resumed after its input (decision 16): answered first, so a claim the answer releases does
    // not make the resume land in BLOCKED.
    let mut last = input.clone();
    let mut stopped: Option<Failure> = None;
    // A signal after the input was accepted sends nothing more (E8).
    if let Some(code) = early(
        got,
        &format!(
            "输入已受理（命令 {}），没有再提交命令；用 hoh attach {run_id} 附着",
            input["command_id"].as_str().unwrap_or_default()
        ),
    ) {
        return code;
    }
    if state == "INCOMPLETE" {
        match fresh(
            project,
            &host,
            run_id,
            input["revision"].as_i64().unwrap_or_default(),
            |revision| {
                json!({
                    "expected_revision": revision,
                    "kind": "ResumeRun",
                    "run_id": run_id,
                    "payload": {"reason": "hoh -p --resume"},
                })
            },
        ) {
            Ok(accepted) => last = accepted,
            Err(failure) if still_stopped(&failure) => stopped = Some(failure),
            Err(failure) => {
                say!(
                    "hoh: 输入已受理（命令 {}），但 ResumeRun 被拒",
                    input["command_id"].as_str().unwrap_or_default()
                );
                return fail(&failure);
            }
        }
    }
    if detach {
        if let Some(failure) = &stopped {
            say!(
                "hoh: 输入已受理（命令 {}）；ResumeRun 被拒：{}",
                input["command_id"].as_str().unwrap_or_default(),
                failure.message
            );
            say!("{}", refused_resume(failure));
        }
        return detached(&last, output);
    }
    // stream-json shows every event of the run, those before the first accepted command's own marked as a replay of
    // history (CR-022); json and text start at the watermark the run was read at, before the acceptance event.
    let after = if output == Output::StreamJson {
        0
    } else {
        watermark
    };
    let replay = Replay::Before {
        command_id: input["command_id"].as_str().unwrap_or_default().to_owned(),
    };
    let stderr = Stderr::start(project);
    let follower = Follower {
        project,
        run_id,
        output,
        signals: got,
        stderr: &stderr,
        quiet: false,
    };
    let mut watch = Watch::new(
        End::Accepted {
            command_id: last["command_id"].as_str().unwrap_or_default().to_owned(),
        },
        replay,
        after,
    );
    let outcome = follower.follow(host, &mut watch);
    // Where the input went (decision 9): the acceptance event of the SendInput says whether it answered a question.
    if let Some(event) = watch.first() {
        match event["payload"]["question_id"].as_str() {
            Some(question) => stderr.say(&format!("hoh: 已答复问题 {question}")),
            None => stderr.say("hoh: 输入已排队，待 M3-T04 投递"),
        }
    }
    let code = crate::follow::conclude(&outcome, output, run_id, &stderr);
    if let Outcome::Result(result) = &outcome {
        if let Some(failure) = &stopped {
            stderr.say(&refused_resume(failure));
            let remaining: Vec<&str> = result["remaining_requirements"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            stderr.say(&format!("hoh: 剩余需求：{}", remaining.join(", ")));
        }
        questions(&client::running(project), run_id, result, &stderr);
    }
    stderr.finish();
    code
}

/// A run that stopped BLOCKED on NEED_DECISION: its open questions (getRun's `projection.questions`) and how to answer
/// one (decision 9).
fn questions(host: &Option<HostInfo>, run_id: &str, result: &Value, stderr: &Stderr) {
    if result["state"] != "BLOCKED" || result["blocked_reason"] != "NEED_DECISION" {
        return;
    }
    let run = host
        .as_ref()
        .and_then(|host| client::get_run(host, run_id).ok().flatten());
    match run.as_ref().map(|r| &r["projection"]["questions"]) {
        Some(Value::Array(questions)) => {
            stderr.say(&format!(
                "hoh: Run {run_id} 有 {} 个待答问题：",
                questions.len()
            ));
            for question in questions {
                stderr.say(&format!(
                    "  {}  {}",
                    question["question_id"].as_str().unwrap_or_default(),
                    question["text"].as_str().unwrap_or_default()
                ));
                for choice in question["choices"].as_array().into_iter().flatten() {
                    stderr.say(&format!(
                        "    - {}：{}",
                        choice["choice_id"].as_str().unwrap_or_default(),
                        choice["label"].as_str().unwrap_or_default()
                    ));
                }
            }
        }
        _ => stderr.say(&format!(
            "hoh: host 没有给出 Run {run_id} 的待答问题（getRun 的 projection.questions）"
        )),
    }
    stderr.say(&format!("hoh: 答复：hoh -p '<答复>' --resume {run_id}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ResumeRun's 409 ILLEGAL_TRANSITION with `details`.
    fn refused(details: Value) -> Failure {
        Failure {
            code: "ILLEGAL_TRANSITION".to_owned(),
            message: "refused".to_owned(),
            exit: 3,
            status: Some(409),
            details,
        }
    }

    #[test]
    fn only_a_refusal_naming_the_stop_that_still_holds_takes_the_stop_path() {
        // docs/adr/ADR-031.md decisions 9 and 16, M2T13R-50: a ResumeRun refused because the INCOMPLETE run's stop still
        // holds names it (`details: {state: INCOMPLETE, stop}`); only then is the input left accepted and the run
        // INCOMPLETE (its result followed, exit 4). A 409 that names INCOMPLETE without a stop is another refusal: its
        // own exit, not the stop's.
        assert!(still_stopped(&refused(
            json!({"state": "INCOMPLETE", "stop": "max_iterations"})
        )));
        for details in [
            json!({"state": "INCOMPLETE"}),
            json!({"state": "INCOMPLETE", "stop": null}),
            json!({"state": "INCOMPLETE", "stop": 3}),
            json!({"state": "BLOCKED", "stop": "max_iterations"}),
            Value::Null,
        ] {
            assert!(!still_stopped(&refused(details.clone())), "{details}");
        }
        let mut other = refused(json!({"state": "INCOMPLETE", "stop": "deadline_at"}));
        other.code = "STALE_REVISION".to_owned();
        assert!(!still_stopped(&other));
    }
}
