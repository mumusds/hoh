//! A verifier's request bridge (docs/adr/ADR-028.md decisions 7 and 13): two POSIX sh helpers in its attempt
//! directory, `run-check <check_id>` and `ask <message.json>`, each writing a numbered request under `requests/` and
//! waiting for its response; the host takes the requests of each running verifier in order, one at a time, while its
//! process runs. Not a `hoh` subcommand (CONTRACT 3): a file bridge of the running attempt.

use hoh_core::persistence::{ChangeRequest, Deferred, Store, VerifierRequest};
use hoh_core::protocol::{Message, MessageKind};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// At most this many requests of one attempt are taken (decision 7's initial value).
pub(super) const MAX_REQUESTS: u64 = 64;

/// How long `ask` waits for its answer, in seconds.
const ASK_WAIT: u64 = 60;

/// How long `run-check` waits for its answer, in seconds, under frozen plan `plan` (decision 7): its longest check's
/// timeout (an hour for a check naming none, as the driver runs it), the stop's grace `grace`, and a minute for the
/// answer to be written.
pub(super) fn check_wait(plan: &Value, grace: std::time::Duration) -> u64 {
    let longest = plan["checks"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, c)| c["timeout_seconds"].as_f64().unwrap_or(3600.0))
        .fold(0.0_f64, f64::max);
    // A plan of no check still answers within the default.
    let longest = if longest > 0.0 {
        longest.ceil() as u64
    } else {
        3600
    };
    longest + grace.as_secs() + 60
}

/// A verifier's helpers and the directory its requests go to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Helpers {
    pub requests: PathBuf,
    pub run_check: PathBuf,
    pub ask: PathBuf,
}

/// The helpers of attempt `attempt_id` in the project at `project`, written into its attempt directory (`.hoh/` is
/// ignored, so they change nothing of the candidate) before its process starts, `run-check` waiting `check_wait` seconds
/// at most ([`check_wait`]). Blocking.
pub(super) fn write_helpers(
    project: &Path,
    attempt_id: &str,
    check_wait: u64,
) -> std::io::Result<Helpers> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = super::product::bridge_of(project, attempt_id)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project.join(".hoh/attempts"));
    let requests = dir.join("requests");
    std::fs::create_dir_all(&requests)?;
    let quoted = shell_quote(&requests.to_string_lossy());
    // A request takes the first free number by an atomic mkdir, is written whole and renamed into place, and its
    // answer is waited for; the host takes the numbers in order. The directory is the host's, made before the agent
    // starts: once it is gone (the host removes an attempt's that is no longer live) a helper ends with 2 at its next
    // look, never making it again nor waiting out its wait (M2T18R-54).
    let gone = |name: &str| {
        format!("[ -d \"$d\" ] || {{ echo '{name}: its request directory is gone' >&2; exit 2; }}")
    };
    let claim = |name: &str| {
        let gone = gone(name);
        format!(
            "d={quoted}\n{gone}\nn=1\nwhile ! mkdir \"$d/$n.claim\" 2>/dev/null; do\n  {gone}\n  n=$((n+1))\ndone\n"
        )
    };
    let wait = |limit: u64, name: &str| {
        let gone = gone(name);
        format!(
            "waited=0\nwhile [ ! -f \"$d/$n.response.json\" ]; do\n  {gone}\n  sleep 1; waited=$((waited+1))\n  \
             [ \"$waited\" -ge {limit} ] && {{ echo '{name}: no answer' >&2; exit 2; }}\ndone\n\
             cat \"$d/$n.response.json\"; echo\n"
        )
    };
    let run_check = format!(
        "#!/bin/sh\n# HoH's request helper (docs/adr/ADR-028.md decision 7): run-check <check_id> runs a check of the \
         frozen plan on the candidate and ends with its exit code (2: refused).\n\
         case \"$1\" in ''|*[!A-Za-z0-9._-]*) echo 'run-check: give one check id of the frozen plan' >&2; exit 2;; esac\n\
         {}printf '{{\"kind\":\"check\",\"check_id\":\"%s\"}}\\n' \"$1\" > \"$d/$n.tmp\" && mv \"$d/$n.tmp\" \"$d/$n.json\" || exit 2\n\
         {}code=$(sed -n 's/.*\"exit_code\":\\([0-9][0-9]*\\).*/\\1/p' \"$d/$n.response.json\")\n\
         [ -n \"$code\" ] && exit \"$code\"\nexit 2\n",
        claim("run-check"),
        wait(check_wait, "run-check")
    );
    let ask = format!(
        "#!/bin/sh\n# HoH's request helper (docs/adr/ADR-028.md decisions 7 and 13): ask <message.json> sends one HCP \
         query or steer of this attempt and prints the answer (rejected or deferred).\n\
         [ -f \"$1\" ] || {{ echo 'ask: give the message file' >&2; exit 2; }}\n\
         {}{{ printf '{{\"kind\":\"message\",\"message\":'; cat \"$1\"; printf '}}\\n'; }} > \"$d/$n.tmp\" && \
         mv \"$d/$n.tmp\" \"$d/$n.json\" || exit 2\n{}exit 0\n",
        claim("ask"),
        wait(ASK_WAIT, "ask")
    );
    let helpers = Helpers {
        requests,
        run_check: dir.join("run-check"),
        ask: dir.join("ask"),
    };
    for (path, text) in [(&helpers.run_check, run_check), (&helpers.ask, ask)] {
        std::fs::write(path, text)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(helpers)
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Request `n` of `helpers`, when it is there: its JSON, or why it cannot be read. Blocking.
pub(super) fn next(helpers: &Helpers, n: u64) -> Option<Result<Value, String>> {
    let path = helpers.requests.join(format!("{n}.json"));
    let bytes = hoh_core::objects::read_regular(&path).ok()?;
    Some(serde_json::from_slice(&bytes).map_err(|e| format!("not JSON: {e}")))
}

/// Answer request `n` of `helpers` with `answer`, whole (written aside, then renamed). Blocking.
pub(super) fn respond(helpers: &Helpers, n: u64, answer: &Value) -> std::io::Result<()> {
    let tmp = helpers.requests.join(format!(".{n}.response.tmp"));
    std::fs::write(&tmp, format!("{answer}\n"))?;
    std::fs::rename(&tmp, helpers.requests.join(format!("{n}.response.json")))
}

/// What a verifier's message asks, by decision 13: a change of the candidate under verification (a steer to the
/// developer, or a query whose payload's intent is `change_candidate`) is refused with an Issue; anything else is
/// deferred.
pub(super) fn classify(message: &Message, round_id: &str, candidate_id: &str) -> VerifierRequest {
    let change = (message.kind == MessageKind::Steer && message.recipient_role == "developer")
        || (message.kind == MessageKind::Query
            && message.payload.get("intent") == Some(&json!("change_candidate")));
    if change {
        let text = ["text", "question", "summary"]
            .iter()
            .find_map(|k| message.payload.get(*k).and_then(Value::as_str))
            .map_or_else(
                || Value::Object(message.payload.clone()).to_string(),
                str::to_owned,
            );
        VerifierRequest::RejectChange(ChangeRequest {
            run_id: message.run_id.clone(),
            attempt_id: message.attempt_id.clone(),
            message_id: message.message_id.clone(),
            round_id: round_id.to_owned(),
            candidate_id: candidate_id.to_owned(),
            severity: message
                .payload
                .get("severity")
                .and_then(Value::as_str)
                .unwrap_or("medium")
                .to_owned(),
            text,
        })
    } else {
        VerifierRequest::Defer(Deferred {
            run_id: message.run_id.clone(),
            attempt_id: message.attempt_id.clone(),
            message_id: message.message_id.clone(),
            recipient_role: message.recipient_role.clone(),
        })
    }
}

/// Answer message `message` of a verifier (decision 13) in the store: `{status: rejected, reason_code, issue_id}` or
/// `{status: deferred}`.
pub(super) async fn answer_message(
    store: &Store,
    message: &Message,
    round_id: &str,
    candidate_id: &str,
) -> Value {
    let request = classify(message, round_id, candidate_id);
    let rejected = matches!(request, VerifierRequest::RejectChange(_));
    match store.verifier_request(request).await {
        Ok(issue) if rejected => json!({"status": "rejected", "reason_code": "ILLEGAL_TRANSITION",
            "detail": "the candidate under verification is not changed; an Issue for the next candidate",
            "issue_id": issue}),
        Ok(_) => json!({"status": "deferred",
            "detail": "routing between roles is not available yet; the question is kept for the next decision"}),
        Err(e) => json!({"status": "error", "detail": e.to_string()}),
    }
}

#[cfg(test)]
mod tests;
