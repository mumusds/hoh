//! The compat suite's embedded codex frames against the recorded 0.166.0 schema (docs/adr/ADR-030.md decision 10):
//! every notification the App Server adapter judges by ([`crate::codex::APP_SERVER_NOTIFICATIONS`]) in every embedded
//! case is accepted by the recorded `ServerNotification.json`, `params` not opened, by the probe's own validator; else
//! the suite's frame acceptance would refuse the real version.

use super::*;

fn recorded_notifications() -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/codex/0.166.0/app-server-schema/ServerNotification.json"
    );
    serde_json::from_slice(&std::fs::read(path).expect("the recorded schema")).expect("JSON")
}

#[test]
fn the_embedded_codex_notifications_are_accepted_by_the_recorded_schema() {
    let schema = recorded_notifications();
    let mut judged = 0;
    for case in crate::compat::fixtures::CASES {
        let bytes = crate::compat::fixtures::case("codex", case).expect("an embedded case");
        for line in bytes.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let frame: serde_json::Value = serde_json::from_slice(line).expect("a JSON frame");
            let Some(method) = frame["method"].as_str() else {
                continue;
            };
            if frame.get("id").is_some()
                || !crate::codex::APP_SERVER_NOTIFICATIONS.contains(&method)
            {
                continue;
            }
            let work = Work::new(VALIDATION_BUDGET, None);
            assert_eq!(
                frame_valid(Some(&schema), &frame, &work),
                Truth::Yes,
                "{case}: {frame}"
            );
            judged += 1;
        }
    }
    assert!(judged >= 24, "{judged} notifications judged");
    // A control: a turn/completed with a status the schema does not list is refused.
    let unlisted = serde_json::json!({"method": "turn/completed", "params": {"threadId": "t",
        "turn": {"id": "u", "items": [], "status": "settled", "error": null}}});
    let work = Work::new(VALIDATION_BUDGET, None);
    assert_eq!(frame_valid(Some(&schema), &unlisted, &work), Truth::No);
}

#[test]
fn test_024_05_resume_excludes_turns_only_under_a_declared_schema() {
    use crate::compat::suite::{Part, plan};
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/protocol/codex/0.166.0/app-server-schema/ClientRequest.json"
    );
    let original: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).expect("schema")).expect("JSON");
    let plan = plan("codex");
    for declared in [true, false] {
        let mut schema = original.clone();
        if !declared {
            schema["definitions"]["ThreadResumeParams"]["properties"]
                .as_object_mut()
                .expect("properties")
                .remove("excludeTurns");
        }
        let conform = conform::schema(
            &plan,
            conform::Export {
                client_requests: &schema,
                client_notifications: &serde_json::json!({}),
                server_notifications: &serde_json::json!({}),
                responses: &BTreeMap::new(),
            },
            &Work::new(VALIDATION_BUDGET, None),
        );
        let resumed: Vec<_> = conform
            .of("compaction")
            .iter()
            .filter(|f| f.part == Part::Requests && f.detail.starts_with("thread/resume "))
            .collect();
        assert!(!resumed.is_empty());
        assert!(
            resumed
                .iter()
                .all(|f| f.truth == if declared { Truth::Yes } else { Truth::No }),
            "declared={declared}: {resumed:?}"
        );
    }
    for case in &plan.cases {
        for launch in &case.launches {
            for request in launch.built["messages"].as_array().expect("messages") {
                match request["method"].as_str() {
                    Some("thread/resume") => assert_eq!(request["params"]["excludeTurns"], true),
                    Some("thread/start") => {
                        assert!(request["params"].get("excludeTurns").is_none())
                    }
                    _ => {}
                }
            }
        }
    }
}

#[test]
fn test_027_01_m3cx003_exhausted_resume_declaration_is_unknown() {
    use crate::compat::suite::{Part, plan};
    let schema = serde_json::json!({"oneOf":[{"properties":{"method":{"enum":["thread/resume"]},"params":{"properties":{"excludeTurns":{"type":"boolean"}}}}}]});
    for budget in [0, 1] {
        let conform = conform::schema(
            &plan("codex"),
            conform::Export {
                client_requests: &schema,
                client_notifications: &serde_json::json!({}),
                server_notifications: &serde_json::json!({}),
                responses: &BTreeMap::new(),
            },
            &Work::new(budget, None),
        );
        let resumed: Vec<_> = conform
            .of("compaction")
            .iter()
            .filter(|f| f.part == Part::Requests && f.detail.starts_with("thread/resume "))
            .collect();
        assert!(!resumed.is_empty());
        assert!(
            resumed.iter().all(|f| f.truth == Truth::Unknown),
            "budget={budget}: {resumed:?}"
        );
    }
}
