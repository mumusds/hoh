//! A session's capabilities (`sessions.capabilities_json`, REQ-027): what the probe found for the CLI (its compat
//! manifest entry), never more than the transport a turn actually used offers. They are never set from the agent's
//! brand: a dispatch that needs one the CLI does not have is refused (`ROLE_CAPABILITY_MISSING`, M1R-01).

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Probed capabilities by name; `None` is unknown (`AgentEntry::capabilities`).
pub type Probed = BTreeMap<String, Option<bool>>;

/// The session row's capabilities: each one the probe found, false where `limits` says the turn's transport cannot
/// have it; one the probe left unknown is left out unless a limit rules it out.
pub fn of_session(probed: &Probed, limits: &[(&str, bool)]) -> Value {
    let mut caps: Map<String, Value> = probed
        .iter()
        .filter_map(|(name, value)| value.map(|v| (name.clone(), Value::Bool(v))))
        .collect();
    for (name, allowed) in limits {
        if !allowed {
            caps.insert((*name).to_owned(), Value::Bool(false));
        }
    }
    Value::Object(caps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_probe_decides_and_a_limit_only_takes_away() {
        let probed: Probed = [
            ("resume".to_owned(), Some(true)),
            ("mid_turn_steer".to_owned(), Some(true)),
            ("token_usage".to_owned(), Some(false)),
            ("images".to_owned(), None),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            of_session(
                &probed,
                &[
                    ("mid_turn_steer", false),
                    ("token_usage", true),
                    ("ui_dialogs", false),
                    ("images", true)
                ]
            ),
            json!({"resume": true, "mid_turn_steer": false, "token_usage": false, "ui_dialogs": false})
        );
        assert_eq!(of_session(&Probed::new(), &[]), json!({}));
    }
}
