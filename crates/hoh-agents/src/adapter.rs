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

/// The sha256 of a raw log, read in fixed-size pieces: a turn's logs are not bounded by the frame limit or the stderr
/// tail, so the whole file is never held in memory (M1R-05).
pub(crate) fn sha256_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut piece = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => hasher.update(&piece[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher
        .finalize()
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
/// [`DECLARATION_INDENT`] spaces, is one or more names (`-x` or `-xy`, `--name`, comma-separated, a name may carry a
/// placeholder as `=<value>`, `=VALUE`, `[=<value>]` or `[=VALUE]`) and then only value placeholders (`<value>`,
/// `[value]`, `VALUE`, each maybe ending in `...`), up to two spaces or a tab before its description, or the end of
/// the line. Anything else is text: a continuation line, a list item `- x`, a mention like
/// `--output-format=stream-json)` or `--mode[=rpc]` (M1R-03, M1T07R-18).
fn declared(line: &str) -> Option<Vec<&str>> {
    declaration(line).map(|(names, _)| names)
}

/// The names a declaration line declares (see [`declared`]) and its description: the rest of the line after them.
fn declaration(line: &str) -> Option<(Vec<&str>, &str)> {
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
    // A name comes first or after a comma; a name without one ends the names (M1T07R-26).
    let mut name_next = true;
    for word in text[..end].split(' ') {
        let (word, comma) = match word.strip_suffix(',') {
            Some(word) => (word, true),
            None => (word, false),
        };
        if name_next && let Some(name) = flag_name(word) {
            names.push(name);
            name_next = comma;
        } else if name_next || !is_placeholder(word) || comma {
            return None;
        }
    }
    (!names.is_empty() && !name_next).then_some((names, &text[end..]))
}

/// The name of an option word (`-x`, `-xy` starting with a letter, `--name`, `--name=<value>`, `--name[=VALUE]`), or
/// `None`; what follows `=` or `[=` must be a placeholder, not a value (M1R-03).
fn flag_name(word: &str) -> Option<&str> {
    let cut = word.find(['=', '[']).unwrap_or(word.len());
    let (name, rest) = word.split_at(cut);
    let value = rest.is_empty()
        || rest
            .strip_prefix('=')
            .or_else(|| rest.strip_prefix("[=").and_then(|r| r.strip_suffix(']')))
            .is_some_and(names_a_value);
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
    (named && value).then_some(name)
}

/// A placeholder that names a value: `<value>` or an upper-case `VALUE` (a letter first, so `0`, `123` and `_` are
/// literals; M1R-03).
fn names_a_value(word: &str) -> bool {
    (word.len() > 2 && word.starts_with('<') && word.ends_with('>'))
        || (word.starts_with(|c: char| c.is_ascii_uppercase())
            && word
                .chars()
                .all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit()))
}

/// A value placeholder: `<value>`, `[value]` or an upper-case `VALUE`, maybe ending in `...`.
fn is_placeholder(word: &str) -> bool {
    let word = word.strip_suffix("...").unwrap_or(word);
    names_a_value(word) || (word.len() > 2 && word.starts_with('[') && word.ends_with(']'))
}

/// Whether `help` (a probed `--help` text) declares the option `flag`. A flag only named in other text is not offered
/// (M1R-03).
pub fn offers(help: &str, flag: &str) -> bool {
    help.lines()
        .filter_map(declared)
        .any(|names| names.contains(&flag))
}

/// What `help` says about the option `flag`, over every declaration of it: each declaration's description and the
/// lines that follow it up to the next declaration or a blank line; `None` if `help` does not declare it. A value
/// counts only where the help says it, never as the declaration's placeholder (`--mode <rpc>`; M1T07R-19, M1R-03).
pub fn option_text(help: &str, flag: &str) -> Option<String> {
    let lines: Vec<&str> = help.lines().collect();
    let mut text: Option<Vec<&str>> = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some((names, description)) = declaration(line)
            && names.contains(&flag)
        {
            let text = text.get_or_insert_with(Vec::new);
            text.push(description);
            text.extend(
                lines[i + 1..]
                    .iter()
                    .take_while(|l| !l.trim().is_empty() && declared(l).is_none()),
            );
        }
    }
    text.map(|text| text.join("\n"))
}

/// Whether `word` is a value listed in `text`: a token between white space, once what only wraps or follows a value is
/// taken off ([`listed_value`]). So `stream-json-v2` is not `stream-json`, `grpc` is not `rpc`, and a longer value
/// (`stdio:///socket`, `stdio://.invalid`, `stdio://[::1]`, `rpc(v2)`, `--output-format=stream-json`) is not the value
/// (M1T07R-17, -27, -30).
pub fn has_word(text: &str, word: &str) -> bool {
    !word.is_empty()
        && text
            .split_whitespace()
            .any(|token| listed_value(token) == word)
}

/// A token without what only wraps or follows a whole value: openers `( [ {` before it; one sentence-final `.`, then
/// closers `) ] }` and list punctuation `,` `;` after it; then one pair of quotes around it (`"x"`, `'x'`, `` `x` ``).
/// Brackets, quotes or a `.` inside a value are part of it, and so is a second `.` (`stdio://..`; M1T07R-27).
fn listed_value(token: &str) -> &str {
    let value = token.trim_start_matches(['(', '[', '{']);
    let value = value
        .strip_suffix('.')
        .unwrap_or(value)
        .trim_end_matches([')', ']', '}', ',', ';']);
    ['"', '\'', '`']
        .into_iter()
        .find_map(|quote| {
            value
                .strip_prefix(quote)
                .and_then(|v| v.strip_suffix(quote))
        })
        .unwrap_or(value)
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

    /// A value in the declaration's own syntax is no value: `=…` and `[=…]` hold a placeholder, not a literal, and an
    /// option's values are what its description says, never its placeholder (M1R-03).
    #[test]
    fn a_value_is_read_from_the_description_only() {
        for mention in [
            "      --output-format[=stream-json]",
            "      --mode[=rpc]",
            "      --json[=no]    not a real flag",
            "  --mode=rpc  Output mode",
        ] {
            assert_eq!(declared(mention), None, "{mention}");
        }
        for (line, name) in [
            ("  --color[=<WHEN>]  Colorize", "--color"),
            ("  --color[=WHEN]", "--color"),
            ("  --out=<FILE>  Write it", "--out"),
            ("  -r, --resume [value]  Resume", "--resume"),
        ] {
            assert!(
                declared(line).is_some_and(|names| names.contains(&name)),
                "{line}"
            );
        }
        // `[rpc]` and `[stream-json]` hold whole tokens: only the description-only rule keeps them out.
        let help = "Options:\n  --mode [rpc]  Output mode: text or json\n  --output-format [stream-json]\n  --level LEVEL  One of: rpc\n";
        let value = |help: &str, flag: &str, word: &str| {
            option_text(help, flag).map(|text| has_word(&text, word))
        };
        assert_eq!(value(help, "--mode", "rpc"), Some(false));
        assert_eq!(value(help, "--output-format", "stream-json"), Some(false));
        assert_eq!(value(help, "--level", "rpc"), Some(true));
        // The recorded helps still offer the values the adapters need.
        let claude = include_str!("../../../tests/fixtures/protocol/claude/2.1.318/help.txt");
        let pi = include_str!("../../../tests/fixtures/protocol/pi/1.5.0/help.txt");
        assert_eq!(value(claude, "--output-format", "stream-json"), Some(true));
        assert_eq!(value(pi, "--mode", "rpc"), Some(true));
    }

    /// A placeholder names a value: a number or `_` is a literal; and two names of one declaration are separated by
    /// a comma, so a description that names two options is not a declaration of them (M1R-03, M1T07R-26).
    #[test]
    fn literals_and_option_lists_are_no_declarations() {
        for line in [
            "  --json=0",
            "  --json[=0]",
            "  --json[=_]",
            "  --json=123",
            "  --json 0",
            "      --other --json  mentioned alongside another option",
            "  -p --print",
            "  --json,",
            "  -c, <FILE>",
        ] {
            assert_eq!(declared(line), None, "{line}");
        }
        for (line, names) in [
            ("  -p, --print  Print", vec!["-p", "--print"]),
            ("  --no-tools, -nt", vec!["--no-tools", "-nt"]),
            ("  -c, --config <key=value>", vec!["-c", "--config"]),
            (
                "  -p, --profile <CONFIG_PROFILE_V2>",
                vec!["-p", "--profile"],
            ),
            ("  --level LEVEL2  One", vec!["--level"]),
        ] {
            assert_eq!(declared(line), Some(names), "{line}");
        }
    }

    /// A value is a whole token of the text, whatever it is made of: `stdio://` as codex lists it, not `stdio://x`
    /// or `pipe://` (M1T07R-27); words as before.
    #[test]
    fn a_value_is_a_whole_token() {
        let listen = "Transport endpoint URL. Supported values: `stdio://` (default), `unix://`";
        assert!(has_word(listen, "stdio://") && has_word(listen, "unix://"));
        assert!(!has_word(
            "Supported values: `stdio://x`, `pipe://`",
            "stdio://"
        ));
        assert!(has_word("text, json, or rpc", "rpc") && !has_word("grpc", "rpc"));
        assert!(
            !has_word("stream-json-v2", "stream-json") && has_word("(stream-json)", "stream-json")
        );
        // A longer URI is another value (M1T07R-27, round 2); a value may end a sentence.
        for longer in [
            "Supported values: `stdio:///socket`",
            "Supported values: stdio://?mode=pipe",
            "Supported values: `stdio://.invalid`",
        ] {
            assert!(!has_word(longer, "stdio://"), "{longer}");
        }
        assert!(has_word("Supported values: unix://, stdio://.", "stdio://"));
        assert!(has_word("Output mode: text, json or rpc.", "rpc"));
        // Part of another token is no value: a file name, another option's syntax.
        assert!(!has_word(
            "Output mode: text or json (see docs/rpc.md)",
            "rpc"
        ));
        assert!(!has_word(
            "Implies --output-format=stream-json",
            "stream-json"
        ));
        // Brackets and quotes only wrap a whole value; inside it they are part of it (M1T07R-30).
        for longer in [
            "Supported values: stdio://[::1]",
            "Supported values: stdio://(socket)",
            "Supported values: stdio://.(invalid)",
            "Supported values: stdio://{mem}",
        ] {
            assert!(!has_word(longer, "stdio://"), "{longer}");
        }
        assert!(!has_word("Output format: stream-json[v2]", "stream-json"));
        assert!(!has_word("Output mode: rpc(v2)", "rpc"));
        assert!(!has_word("Output format: \"stream-json", "stream-json"));
        for listed in [
            "(choices: \"text\", \"json\", \"stream-json\")",
            "[default: stdio://]",
            "(`stdio://`).",
        ] {
            assert!(
                has_word(listed, "stream-json") || has_word(listed, "stdio://"),
                "{listed}"
            );
        }
        // One sentence-final period only: `stdio://..` is a longer value (M1T07R-27, round 4).
        for longer in ["stdio://..", "stdio://...", "stdio://.)"] {
            assert!(
                !has_word(&format!("Supported values: {longer}"), "stdio://"),
                "{longer}"
            );
        }
        assert!(!has_word("Output format: stream-json..", "stream-json"));
        assert!(!has_word("Output mode: rpc...", "rpc"));
        assert!(has_word("Supported values: (stdio://).", "stdio://"));
    }

    /// A subcommand is a name in the command column of the `Commands:` section, not a wrapped description and not a
    /// later section (M1T07R-20).
    #[test]
    fn commands_are_read_from_their_column() {
        assert!(lists_command(HELP, "help") && lists_command(HELP, "review"));
        assert!(!lists_command(HELP, "resume"));
    }
}

#[cfg(test)]
mod hash_tests {
    use super::*;

    /// Set for the child process [`a_raw_log_larger_than_the_memory_allowed_is_hashed`] starts.
    const BOUNDED_CHILD: &str = "HOH_TEST_BOUNDED_HASH_FILE";

    /// A raw log is hashed in fixed-size pieces: a process allowed 64 MiB of data hashes a 256 MiB log, so a large log
    /// never turns a finished turn into an I/O error (M1R-05).
    #[test]
    fn a_raw_log_larger_than_the_memory_allowed_is_hashed() -> Result<(), Box<dyn std::error::Error>>
    {
        let tmp = tempfile::tempdir()?;
        let log = tmp.path().join("stderr.log");
        std::fs::File::create(&log)?.set_len(256 << 20)?;
        // The same bytes hashed here, piece by piece, without the function under test.
        let mut expected = Sha256::new();
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..256 {
            expected.update(&zeros);
        }
        let expected: String = expected
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let out = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "adapter::hash_tests::bounded_hash_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(BOUNDED_CHILD, &log)
            .output()?;
        let said = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "{said}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(said.contains(&format!("hashed {expected}")), "{said}");
        Ok(())
    }

    #[test]
    #[ignore = "run by a_raw_log_larger_than_the_memory_allowed_is_hashed in a process of its own"]
    fn bounded_hash_child() -> Result<(), Box<dyn std::error::Error>> {
        let Some(log) = std::env::var_os(BOUNDED_CHILD) else {
            return Ok(());
        };
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        let old = getrlimit(Resource::Data);
        setrlimit(
            Resource::Data,
            Rlimit {
                current: Some(64 << 20),
                maximum: old.maximum,
            },
        )?;
        println!("hashed {}", sha256_file(Path::new(&log))?);
        Ok(())
    }
}
