//! The messages the App Server adapter writes to `codex app-server`, built by pure functions (docs/adr/ADR-030.md
//! decision 10, "同一构建函数"): the driver sends exactly what these give, and the compat suite checks the same output
//! against the tested binary's schema and keys its PASS on it. `id` is the JSON-RPC id the driver numbers its requests
//! with (0, 1, ... in the order sent).

use crate::Profile;
use serde_json::{Value, json};

/// A request with its id.
fn request(id: i64, method: &str, params: Value) -> Value {
    json!({"method": method, "id": id, "params": params})
}

/// `initialize`: who the client is.
pub fn initialize(id: i64) -> Value {
    request(
        id,
        "initialize",
        json!({"clientInfo": {"name": "hoh", "version": crate::VERSION}}),
    )
}

/// The `initialized` notification, sent once initialize is answered.
pub fn initialized() -> Value {
    json!({"method": "initialized"})
}

/// The profile and model of a thread: HoH's YOLO profile is `approvalPolicy=never`, `sandbox=danger-full-access`
/// (CR-002); no model leaves codex its own default.
fn thread_params(profile: Profile, model: Option<&str>) -> Value {
    let mut params = match profile {
        Profile::Yolo => json!({"approvalPolicy": "never", "sandbox": "danger-full-access"}),
        Profile::Plain => json!({}),
    };
    if let Some(model) = model {
        params["model"] = json!(model);
    }
    params
}

/// `thread/start` of a new thread.
pub fn thread_start(id: i64, profile: Profile, model: Option<&str>) -> Value {
    request(id, "thread/start", thread_params(profile, model))
}

/// `thread/resume` of the session's thread `thread`.
pub fn thread_resume(id: i64, profile: Profile, model: Option<&str>, thread: &str) -> Value {
    let mut params = thread_params(profile, model);
    params["threadId"] = json!(thread);
    request(id, super::super::RESUME_REQUEST, params)
}

/// `turn/start` of the prompt on `thread` (the thread the thread request's answer named; `null` when it named none).
pub fn turn_start(id: i64, thread: Option<&str>, prompt: &str) -> Value {
    request(
        id,
        "turn/start",
        json!({"threadId": thread, "input": [{"type": "text", "text": prompt}]}),
    )
}

/// `turn/interrupt` of the active turn: the App Server's own cancel.
pub fn turn_interrupt(id: i64, thread: &str, turn: &str) -> Value {
    request(
        id,
        "turn/interrupt",
        json!({"threadId": thread, "turnId": turn}),
    )
}

/// `turn/steer` of `text` into the turn `expected_turn` of `thread`.
pub fn turn_steer(id: i64, thread: Option<&str>, expected_turn: Option<&str>, text: &str) -> Value {
    request(
        id,
        super::super::STEER_REQUEST,
        json!({"threadId": thread, "expectedTurnId": expected_turn,
               "input": [{"type": "text", "text": text}]}),
    )
}
