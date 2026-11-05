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

/// The option names a line declares, or `None` for a line that declares no option. A declaration, indented at most
/// [`DECLARATION_INDENT`] spaces, is one or more names (`-x` or `-xy`, `--name`, comma-separated, a name may carry `=<value>` or
/// `[=value]`) and then only value placeholders (`<value>`, `[value]`, `VALUE`, each maybe ending in `...`), up to two
/// spaces or a tab before its description, or the end of the line. Anything else is text: a continuation line, a list
/// item `- x`, a mention like `--output-format=stream-json)` (M1R-03, M1T07R-18).
fn declared(line: &str) -> Option<Vec<&str>> {
    let text = line.trim_start_matches(' ');
    if line.len() - text.len() > DECLARATION_INDENT || !text.starts_with('-') {
        return None;
    }
    let end = [text.find("  "), text.find('\t')]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(text.len());
    let mut names = Vec::new();
    let mut placeholders = false;
    for word in text[..end].split(' ') {
        let word = word.strip_suffix(',').unwrap_or(word);
        if !placeholders && let Some(name) = flag_name(word) {
            names.push(name);
        } else if is_placeholder(word) && !names.is_empty() {
            placeholders = true;
        } else {
            return None;
        }
    }
    (!names.is_empty()).then_some(names)
}

/// The name of an option word (`-x`, `-xy` starting with a letter, `--name`, `--name=<value>`, `--name[=value]`), or `None`.
fn flag_name(word: &str) -> Option<&str> {
    let cut = word.find(['=', '[']).unwrap_or(word.len());
    let (name, rest) = word.split_at(cut);
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let named = match (name.strip_prefix("--"), name.strip_prefix('-')) {
        (Some(long), _) => {
            long.starts_with(|c: char| c.is_ascii_alphanumeric())
                && long.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        }
        // A short name starts with a letter and may have several letters or digits (pi: `--no-tools, -nt`;
        // M1T07R-22); `-123` is a number, not an option (M1T07R-24).
        (None, Some(short)) => {
            short.starts_with(|c: char| c.is_ascii_alphabetic())
                && short.chars().all(|c| c.is_ascii_alphanumeric())
        }
        (None, None) => false,
    };
    (named && name.starts_with('-') && (rest.is_empty() || is_placeholder(rest))).then_some(name)
}

/// A value placeholder: `<value>`, `[value]` or an upper-case `VALUE`, maybe ending in `...`.
fn is_placeholder(word: &str) -> bool {
    let word = word.strip_suffix("...").unwrap_or(word);
    (word.len() > 2 && word.starts_with('<') && word.ends_with('>'))
        || (word.len() > 2 && word.starts_with('[') && word.ends_with(']'))
        || (!word.is_empty()
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))
}

/// Whether `help` (a probed `--help` text) declares the option `flag`. A flag only named in other text is not offered
/// (M1R-03).
pub fn offers(help: &str, flag: &str) -> bool {
    help.lines()
        .filter_map(declared)
        .any(|names| names.contains(&flag))
}

/// The text of the option `flag`, over every declaration of it: each declaration line and the lines that follow it
/// up to the next declaration or a blank line; `None` if `help` does not declare it (M1T07R-19).
pub fn option_text(help: &str, flag: &str) -> Option<String> {
    let lines: Vec<&str> = help.lines().collect();
    let mut text = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if declared(line).is_some_and(|names| names.contains(&flag)) {
            text.push(*line);
            text.extend(
                lines[i + 1..]
                    .iter()
                    .take_while(|l| !l.trim().is_empty() && declared(l).is_none()),
            );
        }
    }
    (!text.is_empty()).then(|| text.join("\n"))
}

/// Whether `word` is a whole word of `text`: words are runs of letters, digits, `-` and `_` (`stream-json-v2` is not
/// `stream-json`, `grpc` is not `rpc`; M1T07R-17).
pub fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .any(|w| w == word)
}

/// Whether `help` lists the subcommand `name` in the command column of its `Commands:` section (the least indented
/// lines of that section; wrapped descriptions are indented further), which ends at a blank line or the next
/// heading (M1T07R-20).
pub fn lists_command(help: &str, name: &str) -> bool {
    let section: Vec<&str> = help
        .lines()
        .skip_while(|line| !line.trim().eq_ignore_ascii_case("commands:"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty() && line.starts_with([' ', '\t']))
        .collect();
    let indent = |line: &&str| line.len() - line.trim_start().len();
    let column = section.iter().map(indent).min();
    section
        .iter()
        .any(|line| Some(indent(line)) == column && line.split_whitespace().next() == Some(name))
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

#[cfg(test)]
mod help_tests {
    use super::*;

    const HELP: &str = "Usage: tool [options]\n\nOptions:\n  -p, --print                 Print and exit\n  --verbose\tOverride verbose mode (see --quiet)\n  --output-format <format>    Output format: \"text\", \"json\" or\n                              \"stream-json\" (with --print)\n                              --quiet\n  -i, --image <FILE>...\n          Images to attach; works with\n      --output-format=stream-json)\n  --mode <mode>               Output mode: text or json\n      - text\n      - json\n  --mode <mode>               Also: rpc\n  --level <LEVEL>             One of: stream-json-v2, grpc\n\nCommands:\n  help    Print help\n          resume a session through another tool\n  review  Review code\nOther:\n  resume  Not a command\n";

    /// A flag counts only where an option line declares it: not in a continuation line, a description, a list item or
    /// after a tab (M1R-03).
    #[test]
    fn only_declarations_offer_a_flag() {
        for flag in [
            "-p",
            "--print",
            "--verbose",
            "--output-format",
            "-i",
            "--image",
            "--mode",
            "--level",
        ] {
            assert!(offers(HELP, flag), "{flag}");
        }
        for flag in ["--quiet", "--with", "-", "--print)"] {
            assert!(!offers(HELP, flag), "{flag}");
        }
        // The real --output-format declaration gone: its mention in --image's continuation line does not count.
        let without = HELP.replace(
            "  --output-format <format>    Output format: \"text\", \"json\" or\n",
            "",
        );
        assert!(!offers(&without, "--output-format"));
    }

    /// An option's text is the text of every declaration of it, list items included; a value is a whole word of it
    /// (M1T07R-17..19).
    #[test]
    fn values_are_whole_words_of_every_declaration_of_the_option() {
        let words = |flag: &str| option_text(HELP, flag).map(|t| has_word(&t, "stream-json"));
        assert_eq!(words("--output-format"), Some(true));
        assert_eq!(
            words("--level"),
            Some(false),
            "stream-json-v2 is not stream-json"
        );
        assert!(
            option_text(HELP, "--level").is_some_and(|t| !has_word(&t, "rpc")),
            "grpc is not rpc"
        );
        let mode = option_text(HELP, "--mode").unwrap_or_default();
        assert!(has_word(&mode, "json") && has_word(&mode, "rpc"), "{mode}");
        assert!(option_text(HELP, "--quiet").is_none());
    }

    /// Every declaration of the recorded pi 1.5.0 help is one, all eight multi-letter short aliases included, and
    /// none is read as the previous option's text (M1T07R-22, -25).
    #[test]
    fn the_recorded_pi_declarations_are_all_declarations() {
        let help = include_str!("../../../tests/fixtures/protocol/pi/1.5.0/help.txt");
        for (long, short) in [
            ("--no-tools", "-nt"),
            ("--no-builtin-tools", "-nbt"),
            ("--exclude-tools", "-xt"),
            ("--no-extensions", "-ne"),
            ("--no-skills", "-ns"),
            ("--no-prompt-templates", "-np"),
            ("--no-context-files", "-nc"),
            ("--no-approve", "-na"),
        ] {
            assert!(offers(help, long) && offers(help, short), "{long}, {short}");
        }
        for (option, own, other) in [
            ("--models", "fuzzy matching", "--no-tools"),
            ("--tools", "mcp__", "--exclude-tools"),
        ] {
            let text =
                option_text(help, option).unwrap_or_else(|| panic!("{option} is not declared"));
            assert!(text.contains(own), "{option} lost its own text: {text}");
            assert!(
                !text.contains(other),
                "{option} took {other}'s text: {text}"
            );
        }
    }

    /// Text that is no declaration is refused without fail: a word of any script, a negative number (M1T07R-23, -24).
    #[test]
    fn other_text_is_no_declaration() {
        assert_eq!(declared("  --other 中文说明"), None);
        assert_eq!(declared("  -中文"), None);
        assert_eq!(declared("      -123"), None);
        assert_eq!(declared("  -1"), None);
        assert_eq!(
            declared("  -nt, --no-tools"),
            Some(vec!["-nt", "--no-tools"])
        );
    }

    /// A subcommand is a name in the command column of the `Commands:` section, not a wrapped description and not a
    /// later section (M1T07R-20).
    #[test]
    fn commands_are_read_from_their_column() {
        assert!(lists_command(HELP, "help") && lists_command(HELP, "review"));
        assert!(!lists_command(HELP, "resume"));
    }
}
