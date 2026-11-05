//! A session's capabilities (`sessions.capabilities_json`, REQ-027): what the probe found for the CLI (its compat
//! manifest entry), and only where the adapter implements the capability on the transport a turn used. They are never
//! set from the agent's brand: a dispatch that needs one the session does not have is refused
//! (`ROLE_CAPABILITY_MISSING`, M1R-01).

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Probed capabilities by name; `None` is unknown (`AgentEntry::capabilities`).
pub type Probed = BTreeMap<String, Option<bool>>;

/// The session row's capabilities. `supported` lists what the adapter implements on this turn's transport, true where
/// the CLI offers it there: each of those is what the probe found (left out if unknown); every other capability, probed
/// or not, is false. A capability a compat suite finds but the adapter does not handle is never granted (M1T04R-15).
pub fn of_session(probed: &Probed, supported: &[(&str, bool)]) -> Value {
    let mut caps = Map::new();
    let names = crate::probe::CAPABILITIES
        .iter()
        .copied()
        .chain(probed.keys().map(String::as_str));
    for name in names {
        let offered = supported.iter().any(|(n, ok)| *n == name && *ok);
        let value = if offered {
            probed.get(name).copied().flatten()
        } else {
            Some(false)
        };
        if let Some(v) = value {
            caps.insert(name.to_owned(), Value::Bool(v));
        }
    }
    Value::Object(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_what_the_adapter_supports_and_the_probe_found() {
        let probed: Probed = [
            ("resume".to_owned(), Some(true)),
            ("mid_turn_steer".to_owned(), Some(true)),
            ("token_usage".to_owned(), Some(false)),
            ("images".to_owned(), None),
            ("x_future".to_owned(), Some(true)),
        ]
        .into_iter()
        .collect();
        let caps = of_session(
            &probed,
            &[
                ("resume", true),
                ("mid_turn_steer", false),
                ("token_usage", true),
                ("images", true),
            ],
        );
        // Supported and found: the probe's value; supported but unknown: left out; anything else: false.
        assert_eq!(caps["resume"], json!(true));
        assert_eq!(caps["token_usage"], json!(false));
        assert!(caps.get("images").is_none(), "{caps}");
        for no in [
            "mid_turn_steer",
            "x_future",
            "structured_events",
            "partial_messages",
            "custom_tools",
            "browser",
            "ui_dialogs",
        ] {
            assert_eq!(caps[no], json!(false), "{no}: {caps}");
        }
        assert!(
            of_session(&Probed::new(), &[])
                .as_object()
                .is_some_and(|m| m.values().all(|v| *v == json!(false)))
        );
    }
}
