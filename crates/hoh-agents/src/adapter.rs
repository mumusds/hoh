//! What the native adapters share (docs/13-adapters.md): the attempt a turn runs as, its errors, and the moves of
//! that attempt through the store (every end through its cancel latch, M1T03R-10).

use hoh_core::lifecycle::{AttemptState, InputState, Lifecycle};
use hoh_core::persistence::{
    AttemptFacts, AttemptMove, CancelLatch, InputMove, PersistError, Store,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The attempt a turn runs as, and its session.
#[derive(Debug, Clone)]
pub struct AttemptRef {
    pub attempt_id: String,
    /// HoH's session id (`sessions.session_id`).
    pub session_id: String,
    pub role: String,
    pub agent_version: String,
}

/// Why a turn could not be recorded or run.
#[derive(Debug)]
pub enum AdapterError {
    Store(PersistError),
    Io(std::io::Error),
}

impl From<PersistError> for AdapterError {
    fn from(e: PersistError) -> Self {
        AdapterError::Store(e)
    }
}

impl From<std::io::Error> for AdapterError {
    fn from(e: std::io::Error) -> Self {
        AdapterError::Io(e)
    }
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::Store(e) => write!(f, "store: {e}"),
            AdapterError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for AdapterError {}

pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

pub(crate) fn now() -> Option<String> {
    Some(hoh_core::time::now_rfc3339())
}

pub(crate) async fn step(
    store: &Store,
    attempt: &AttemptRef,
    from: AttemptState,
    to: AttemptState,
    facts: AttemptFacts,
) -> Result<(), PersistError> {
    store
        .move_attempt(AttemptMove {
            attempt_id: attempt.attempt_id.clone(),
            from,
            to,
            facts,
        })
        .await
}

/// End the attempt through the latch: the state written (CANCELLED if a cancel was accepted first).
pub(crate) async fn finish(
    store: &Store,
    attempt: &AttemptRef,
    from: AttemptState,
    to: AttemptState,
    facts: AttemptFacts,
    cancel: &CancelLatch,
) -> Result<AttemptState, PersistError> {
    store
        .end_attempt(
            AttemptMove {
                attempt_id: attempt.attempt_id.clone(),
                from,
                to,
                facts,
            },
            cancel,
        )
        .await
}

/// The deepest indentation of an option's declaration in the recorded helps (clap: 2 or 6 spaces); descriptions and
/// their continuation lines are indented further.
const DECLARATION_INDENT: usize = 8;

/// The declaration of an option line (its names and value placeholders, up to the two spaces before its
/// description), or `None` for a line that declares no option.
fn declaration(line: &str) -> Option<&str> {
    let text = line.trim_start();
    (line.len() - text.len() <= DECLARATION_INDENT && text.starts_with('-'))
        .then(|| text.split("  ").next().unwrap_or(text))
}

/// Whether `help` (a probed `--help` text) declares the option `flag` as a word of its own. A flag only named in
/// another option's text is not offered (M1R-03).
pub fn offers(help: &str, flag: &str) -> bool {
    help.lines().filter_map(declaration).any(|declared| {
        declared
            .split(|c: char| c.is_whitespace() || c == ',')
            .any(|word| word.split(['=', '[']).next() == Some(flag))
    })
}

/// The text of the option `flag`: its declaration line and the description lines that follow it, up to the next
/// declaration or a blank line; `None` if `help` does not declare it.
pub fn option_text(help: &str, flag: &str) -> Option<String> {
    let mut lines = help.lines().skip_while(|line| !offers(line, flag));
    let first = lines.next()?;
    let rest = lines.take_while(|line| !line.trim().is_empty() && declaration(line).is_none());
    Some(
        std::iter::once(first)
            .chain(rest)
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Whether `help` lists the subcommand `name` under its `Commands:` heading.
pub fn lists_command(help: &str, name: &str) -> bool {
    help.lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case("commands:"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .any(|line| line.split_whitespace().next() == Some(name))
}

/// Numbers the diagnostics of input moves that did not happen, so two of one attempt never share an event id.
static NOT_MOVED: AtomicUsize = AtomicUsize::new(1);

/// Move an input a turn delivers; true if moved. An input someone else has moved on (stale) or a move the table refuses is
/// recorded and does not stop the turn (M1T05R-01); other store errors do.
pub(crate) async fn soft_move(
    store: &Store,
    attempt_id: &str,
    input_id: &str,
    from: InputState,
    to: InputState,
    reason: Option<String>,
) -> Result<bool, PersistError> {
    let moved = store
        .move_input(InputMove {
            input_id: input_id.to_owned(),
            from,
            to,
            reason,
        })
        .await;
    match moved {
        Ok(()) => Ok(true),
        Err(e @ (PersistError::StaleState { .. } | PersistError::IllegalTransition { .. })) => {
            store
                .attempt_event(
                    attempt_id,
                    &format!("input-not-moved-{}", NOT_MOVED.fetch_add(1, Ordering::Relaxed)),
                    "adapter.diagnostic",
                    json!({"attempt_id": attempt_id, "reason": "input not moved", "input_id": input_id,
                           "to": to.as_str(), "error": e.to_string()}),
                )
                .await?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}
