//! The compat suite's embedded synthetic fixtures (docs/adr/ADR-030.md decision 10, "嵌入的合成 fixture"): for each
//! suite (an adapter transport with a replay of its own: claude, codex's App Server, codex exec, pi) the six cases of
//! REQ-027 behaviour 3 as the agent's stdout, each case's expected replay ([`crate::adapter::Replayed`]) in the
//! suite's `expected.json`, and the launch-variant table (`variants.json`). A case's line
//! [`crate::adapter::REPLAY_CANCEL`] is no frame of the agent's: it is the host's cancel at that point. The cases are
//! synthesized from the frames the adapters know, never copied from a recording: the recordings
//! (tests/fixtures/protocol) stay in the tests ([`leaks`]).

use crate::adapter::Replayed;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The suites: each adapter transport whose frames are judged by a replay of its own.
pub const SUITES: [&str; 4] = ["claude", "codex", "codex-exec", "pi"];

/// The six cases of REQ-027 behaviour 3.
pub const CASES: [&str; 6] = [
    "success",
    "failure",
    "cancel",
    "compaction",
    "tool",
    "unknown_fields",
];

/// Every embedded file, by its path under `compat/fixtures/`.
pub const FILES: &[(&str, &[u8])] = &[
    (
        "variants.json",
        include_bytes!("fixtures/variants.json") as &[u8],
    ),
    (
        "claude/expected.json",
        include_bytes!("fixtures/claude/expected.json") as &[u8],
    ),
    (
        "claude/success.jsonl",
        include_bytes!("fixtures/claude/success.jsonl") as &[u8],
    ),
    (
        "claude/failure.jsonl",
        include_bytes!("fixtures/claude/failure.jsonl") as &[u8],
    ),
    (
        "claude/cancel.jsonl",
        include_bytes!("fixtures/claude/cancel.jsonl") as &[u8],
    ),
    (
        "claude/compaction.jsonl",
        include_bytes!("fixtures/claude/compaction.jsonl") as &[u8],
    ),
    (
        "claude/tool.jsonl",
        include_bytes!("fixtures/claude/tool.jsonl") as &[u8],
    ),
    (
        "claude/unknown_fields.jsonl",
        include_bytes!("fixtures/claude/unknown_fields.jsonl") as &[u8],
    ),
    (
        "codex/expected.json",
        include_bytes!("fixtures/codex/expected.json") as &[u8],
    ),
    (
        "codex/success.jsonl",
        include_bytes!("fixtures/codex/success.jsonl") as &[u8],
    ),
    (
        "codex/failure.jsonl",
        include_bytes!("fixtures/codex/failure.jsonl") as &[u8],
    ),
    (
        "codex/cancel.jsonl",
        include_bytes!("fixtures/codex/cancel.jsonl") as &[u8],
    ),
    (
        "codex/compaction.jsonl",
        include_bytes!("fixtures/codex/compaction.jsonl") as &[u8],
    ),
    (
        "codex/tool.jsonl",
        include_bytes!("fixtures/codex/tool.jsonl") as &[u8],
    ),
    (
        "codex/unknown_fields.jsonl",
        include_bytes!("fixtures/codex/unknown_fields.jsonl") as &[u8],
    ),
    (
        "codex-exec/expected.json",
        include_bytes!("fixtures/codex-exec/expected.json") as &[u8],
    ),
    (
        "codex-exec/success.jsonl",
        include_bytes!("fixtures/codex-exec/success.jsonl") as &[u8],
    ),
    (
        "codex-exec/failure.jsonl",
        include_bytes!("fixtures/codex-exec/failure.jsonl") as &[u8],
    ),
    (
        "codex-exec/cancel.jsonl",
        include_bytes!("fixtures/codex-exec/cancel.jsonl") as &[u8],
    ),
    (
        "codex-exec/compaction.jsonl",
        include_bytes!("fixtures/codex-exec/compaction.jsonl") as &[u8],
    ),
    (
        "codex-exec/tool.jsonl",
        include_bytes!("fixtures/codex-exec/tool.jsonl") as &[u8],
    ),
    (
        "codex-exec/unknown_fields.jsonl",
        include_bytes!("fixtures/codex-exec/unknown_fields.jsonl") as &[u8],
    ),
    (
        "pi/expected.json",
        include_bytes!("fixtures/pi/expected.json") as &[u8],
    ),
    (
        "pi/success.jsonl",
        include_bytes!("fixtures/pi/success.jsonl") as &[u8],
    ),
    (
        "pi/failure.jsonl",
        include_bytes!("fixtures/pi/failure.jsonl") as &[u8],
    ),
    (
        "pi/cancel.jsonl",
        include_bytes!("fixtures/pi/cancel.jsonl") as &[u8],
    ),
    (
        "pi/compaction.jsonl",
        include_bytes!("fixtures/pi/compaction.jsonl") as &[u8],
    ),
    (
        "pi/tool.jsonl",
        include_bytes!("fixtures/pi/tool.jsonl") as &[u8],
    ),
    (
        "pi/unknown_fields.jsonl",
        include_bytes!("fixtures/pi/unknown_fields.jsonl") as &[u8],
    ),
];

/// The embedded file `name` (its path under `compat/fixtures/`).
pub fn file(name: &str) -> Option<&'static [u8]> {
    FILES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, bytes)| *bytes)
}

/// The embedded stdout of `case` for `suite`.
pub fn case(suite: &str, case: &str) -> Option<&'static [u8]> {
    file(&format!("{suite}/{case}.jsonl"))
}

/// The expected replay of each of `suite`'s cases, by case; none when the suite has no readable `expected.json`.
pub fn expected(suite: &str) -> BTreeMap<String, Replayed> {
    file(&format!("{suite}/expected.json"))
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default()
}

/// A launch variant: the product's launch of a new or a resumed session, with or without a model, in a profile: HoH's
/// YOLO launch unless the table says otherwise (the read-only variants, docs/adr/ADR-032.md decision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Variant {
    pub resume: bool,
    pub model: bool,
    #[serde(default = "yolo")]
    pub profile: crate::Profile,
}

fn yolo() -> crate::Profile {
    crate::Profile::Yolo
}

/// The launch-variant table (`variants.json`): the variants by name, and the variants of each suite's cases.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Variants {
    variants: BTreeMap<String, Variant>,
    suites: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

impl Variants {
    /// The variants of `suite`'s `case`, by name (none for a suite or case the table does not have).
    pub fn of(&self, suite: &str, case: &str) -> &[String] {
        self.suites
            .get(suite)
            .and_then(|cases| cases.get(case))
            .map_or(&[], Vec::as_slice)
    }

    /// The variant named `name`.
    pub fn variant(&self, name: &str) -> Option<Variant> {
        self.variants.get(name).copied()
    }
}

static VARIANTS: LazyLock<Variants> = LazyLock::new(|| {
    file("variants.json")
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .unwrap_or_default()
});

/// The embedded launch-variant table (empty if it could not be read; a unit test reads every suite's cases in it).
pub fn variants() -> &'static Variants {
    &VARIANTS
}

/// What in `bytes` looks like a recording machine's path or a credential (docs/adr/ADR-030.md decision 10): a home
/// directory (`/home/`, `/Users/`), a private key, a bearer token, a JSON key naming a token, an API key's or a JWT's
/// shape, or a long run of letters and digits; empty when nothing does.
pub fn leaks(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut found = Vec::new();
    for marker in [
        "/home/",
        "/Users/",
        "-----BEGIN",
        "Bearer ",
        "\"access_token\"",
        "\"refresh_token\"",
        "\"id_token\"",
        "\"api_key\"",
        "\"apiKey\"",
        "\"password\"",
        "\"secret\"",
    ] {
        if text.contains(marker) {
            found.push(format!("contains {marker:?}"));
        }
    }
    let token = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
    for word in text.split(|c: char| !token(c)).filter(|w| !w.is_empty()) {
        let prefixed = [
            "sk-",
            "ghp_",
            "gho_",
            "ghs_",
            "github_pat_",
            "xoxb-",
            "xoxp-",
            "AKIA",
            "AIza",
            "eyJ",
        ]
        .iter()
        .any(|prefix| word.starts_with(prefix) && word.len() >= prefix.len() + 16);
        // A run of 32 or more letters and digits (with `_` or `-`) that has both: a key, a hash or an id of the
        // recording's, never a word of a synthesized frame.
        let long_run = word.split('.').any(|run| {
            run.len() >= 32
                && run.chars().any(|c| c.is_ascii_digit())
                && run.chars().any(|c| c.is_ascii_alphabetic())
        });
        if prefixed || long_run {
            found.push(format!("contains {word:?}"));
        }
    }
    found
}
