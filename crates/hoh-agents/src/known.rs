//! The known frames of each supported CLI version (docs/13 "已知帧登记", M1-T07): the registries in
//! `tests/fixtures/protocol/<agent>/<version>/known-frames.json`, gathered from the recordings and the CLI's own
//! documentation, schema or executable, and built into the adapters. A frame whose key is not in its registry is kept
//! as one `adapter.unknown_event` with the frame as received and the sha256 of its bytes (AC-027-02). A known frame
//! the adapter does not act on is ignored without an event.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// The keys of one registry.
#[derive(Debug, Default)]
pub struct Registry {
    keys: BTreeSet<String>,
}

impl Registry {
    /// The `key` of every entry of the lists at `paths` in a registry file.
    fn parse(text: &str, paths: &[&[&str]]) -> Registry {
        let doc: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let mut keys = BTreeSet::new();
        for path in paths {
            let mut list = &doc;
            for step in *path {
                list = &list[*step];
            }
            keys.extend(
                list.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|entry| entry["key"].as_str().map(str::to_owned)),
            );
        }
        Registry { keys }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.keys.contains(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.keys.iter()
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

static CLAUDE: LazyLock<Registry> = LazyLock::new(|| {
    Registry::parse(
        include_str!("../../../tests/fixtures/protocol/claude/2.1.318/known-frames.json"),
        &[&["frames"]],
    )
});
static PI: LazyLock<Registry> = LazyLock::new(|| {
    Registry::parse(
        include_str!("../../../tests/fixtures/protocol/pi/1.5.0/known-frames.json"),
        &[&["frames"]],
    )
});
static CODEX: &str =
    include_str!("../../../tests/fixtures/protocol/codex/0.166.0/known-frames.json");
static CODEX_APP_SERVER: LazyLock<Registry> = LazyLock::new(|| {
    Registry::parse(
        CODEX,
        &[
            &["app_server", "notifications"],
            &["app_server", "requests"],
        ],
    )
});
static CODEX_EXEC: LazyLock<Registry> = LazyLock::new(|| Registry::parse(CODEX, &[&["exec"]]));

/// claude 2.1.318, `--output-format stream-json` records.
pub fn claude() -> &'static Registry {
    &CLAUDE
}

/// pi 1.5.0, `--mode rpc` stdout records.
pub fn pi() -> &'static Registry {
    &PI
}

/// codex 0.166.0 app-server: server notification and server request methods.
pub fn codex_app_server() -> &'static Registry {
    &CODEX_APP_SERVER
}

/// codex 0.166.0 `exec --json` event types.
pub fn codex_exec() -> &'static Registry {
    &CODEX_EXEC
}

/// The registry key of a claude record: its type, with the subtype for system and result records.
pub fn claude_key(frame: &Value) -> Option<String> {
    let kind = frame["type"].as_str()?;
    match (kind, frame["subtype"].as_str()) {
        ("system" | "result", Some(subtype)) => Some(format!("{kind}/{subtype}")),
        _ => Some(kind.to_owned()),
    }
}

/// The registry key of a pi record: its type.
pub fn pi_key(frame: &Value) -> Option<String> {
    frame["type"].as_str().map(str::to_owned)
}

/// sha256 (lowercase hex) of a frame's bytes.
pub fn frame_sha256(line: &[u8]) -> String {
    Sha256::digest(line)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether a frame with `key` (none: no string type) is in `registry`.
pub fn is_known(registry: &Registry, key: Option<&str>) -> bool {
    key.is_some_and(|k| registry.contains(k))
}

/// The payload of an `adapter.unknown_event`: the frame as received, its key and the sha256 of its bytes.
pub fn unknown_event(attempt_id: &str, key: Option<&str>, line: &[u8], frame: &Value) -> Value {
    json!({"attempt_id": attempt_id, "key": key, "frame_sha256": frame_sha256(line), "frame": frame})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registries_are_built_in_and_keyed_as_recorded() {
        assert_eq!(claude().len(), 92);
        assert_eq!(pi().len(), 26);
        assert_eq!(codex_app_server().len(), 95);
        assert_eq!(codex_exec().len(), 8);
        for key in [
            "system/init",
            "system/task_started",
            "result/success",
            "assistant",
            "rate_limit_event",
        ] {
            assert!(claude().contains(key), "{key}");
        }
        for key in [
            "agent_settled",
            "response",
            "extension_ui_request",
            "compaction_end",
        ] {
            assert!(pi().contains(key), "{key}");
        }
        assert!(codex_app_server().contains("remoteControl/status/changed"));
        assert!(codex_app_server().contains("item/commandExecution/requestApproval"));
        assert_eq!(
            claude_key(&json!({"type": "system", "subtype": "init"})).as_deref(),
            Some("system/init")
        );
        assert_eq!(
            claude_key(&json!({"type": "assistant", "subtype": "x"})).as_deref(),
            Some("assistant")
        );
        assert_eq!(claude_key(&json!({"subtype": "init"})), None);
        assert!(
            !is_known(claude(), None),
            "a record without a type is unknown"
        );
        assert!(!is_known(claude(), Some("system/hoh_future")));
    }
}
