//! HCP types round-trip every valid corpus document byte-for-byte as JSON values, reject every invalid one with an
//! error (never a panic), keep unknown event kinds and payload fields, and list exactly the schema's enum values.

use hoh_core::protocol::{
    self, AllVariants, BlockedReason, Candidate, CommandKind, ContextHandoff, DecisionPayload,
    ErrorCode, Event, HandoffKind, InputKind, Message, MessageKind, Mode, PlanProposal,
    RejectionReason, RunState, SpecDecision, TaskResult, TaskState, TaskStatus,
};
use hoh_core::protocol::{ArtifactRef, Command};
use hoh_core::schema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

type R = Result<(), Box<dyn std::error::Error>>;

fn corpus() -> Result<Vec<Value>, Box<dyn std::error::Error>> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/schema/corpus.json"
    ))?;
    let doc: Value = serde_json::from_str(&text)?;
    Ok(doc["cases"].as_array().cloned().unwrap_or_default())
}

fn check<T: protocol::SchemaDef + Serialize + DeserializeOwned>(
    case: &Value,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let instance = &case["instance"];
    let decoded = protocol::decode::<T>(instance);
    if case["valid"] == true {
        let value = protocol::encode(&decoded.map_err(|e| format!("{}: {e}", case["note"]))?)?;
        return Ok((value != *instance).then(|| {
            format!(
                "{}: round trip changed it\n{value}\n{instance}",
                case["note"]
            )
        }));
    }
    Ok(decoded
        .is_ok()
        .then(|| format!("{}: invalid instance decoded", case["note"])))
}

#[test]
fn every_corpus_document_round_trips_or_is_rejected() -> R {
    let mut wrong = Vec::new();
    let mut seen = 0;
    for case in corpus()?.iter().filter(|c| c["schema"] == "protocol") {
        let outcome = match case["def"].as_str().unwrap_or_default() {
            "Message" => check::<Message>(case)?,
            "ContextHandoff" => check::<ContextHandoff>(case)?,
            "Event" => check::<Event>(case)?,
            "Command" => check::<Command>(case)?,
            "TaskResult" => check::<TaskResult>(case)?,
            "DecisionPayload" => check::<DecisionPayload>(case)?,
            "PlanProposal" => check::<PlanProposal>(case)?,
            "ArtifactRef" => check::<ArtifactRef>(case)?,
            "Candidate" => check::<Candidate>(case)?,
            _ => continue,
        };
        seen += 1;
        wrong.extend(outcome);
    }
    assert!(seen >= 80, "only {seen} typed cases");
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    Ok(())
}

#[test]
fn unknown_event_kinds_and_payload_fields_are_kept() -> R {
    let raw = serde_json::json!({"schema_version": "1.0", "project_id": "p", "seq": 7, "event_id": "e", "run_id": null,
        "attempt_id": null, "at": "2026-11-04T00:00:00Z", "kind": "vendor.future_frame",
        "payload": {"never_seen": {"nested": [1, "x"]}}});
    let event: Event = protocol::decode(&raw)?;
    assert_eq!(event.kind, "vendor.future_frame");
    assert_eq!(event.payload["never_seen"]["nested"][1], "x");
    assert_eq!(protocol::encode(&event)?, raw);
    Ok(())
}

#[test]
fn arbitrary_input_never_panics() -> R {
    let samples = [
        "null",
        "[]",
        "{}",
        "1",
        "\"x\"",
        "{\"kind\":null}",
        "{\"schema_version\":\"1.0\",\"kind\":\"dispatch\",\"payload\":7}",
        "{\"kind\":\"create_tasks\",\"plan\":{\"tasks\":[{}]}}",
        "{\"seq\":1e400}",
        "{\"seq\":-1}",
    ];
    for text in samples {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        assert!(protocol::decode::<Message>(&value).is_err());
        assert!(protocol::decode::<Event>(&value).is_err());
        assert!(protocol::decode::<DecisionPayload>(&value).is_err());
    }
    Ok(())
}

fn schema_enum(pointer: &str) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let values = schema::protocol()
        .document()
        .pointer(pointer)
        .and_then(Value::as_array)
        .ok_or_else(|| format!("no enum at {pointer}"))?;
    Ok(values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect())
}

fn rust_values<T: AllVariants + Serialize>() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    T::ALL
        .iter()
        .map(|v| {
            Ok(serde_json::to_value(v)?
                .as_str()
                .ok_or("not a string")?
                .to_owned())
        })
        .collect()
}

fn same<T: AllVariants + Serialize>(pointer: &str) -> R {
    assert_eq!(
        rust_values::<T>()?,
        schema_enum(pointer)?,
        "enum at {pointer}"
    );
    Ok(())
}

#[test]
fn rust_enums_list_exactly_the_schema_values() -> R {
    same::<MessageKind>("/$defs/Message/properties/kind/enum")?;
    same::<HandoffKind>("/$defs/ContextHandoff/properties/handoff_kind/enum")?;
    same::<TaskStatus>("/$defs/TaskResult/properties/status/enum")?;
    same::<CommandKind>("/$defs/Command/properties/kind/enum")?;
    same::<BlockedReason>("/$defs/BlockedReason/enum")?;
    same::<RejectionReason>("/$defs/RejectionReason/enum")?;
    same::<ErrorCode>("/$defs/ErrorCode/enum")?;
    same::<TaskState>("/$defs/DecisionPayload/oneOf/1/properties/to_state/enum")?;
    same::<RunState>("/$defs/DecisionPayload/oneOf/2/properties/to_state/enum")?;
    same::<Mode>("/$defs/Command/allOf/0/then/properties/payload/properties/mode/enum")?;
    same::<Mode>("/$defs/Command/allOf/1/then/properties/payload/properties/mode/enum")?;
    same::<InputKind>("/$defs/Command/allOf/2/then/properties/payload/properties/input_kind/enum")?;
    same::<SpecDecision>(
        "/$defs/Command/allOf/8/then/properties/payload/properties/decision/enum",
    )?;
    Ok(())
}

#[test]
fn command_payload_kinds_follow_the_schema_order() -> R {
    let doc = schema::protocol().document();
    let branches = doc
        .pointer("/$defs/Command/allOf")
        .and_then(Value::as_array)
        .ok_or("allOf")?;
    let kinds: Vec<String> = branches
        .iter()
        .filter_map(|b| {
            b.pointer("/if/properties/kind/const")?
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(kinds, rust_values::<CommandKind>()?);
    let decisions = doc
        .pointer("/$defs/DecisionPayload/oneOf")
        .and_then(Value::as_array)
        .ok_or("oneOf")?;
    let kinds: Vec<String> = decisions
        .iter()
        .filter_map(|b| {
            b.pointer("/properties/kind/const")?
                .as_str()
                .map(str::to_owned)
        })
        .collect();
    assert_eq!(
        kinds,
        DecisionPayload::KINDS
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[test]
fn integral_floats_decode_like_integers() -> R {
    // M1T01R-01: JSON Schema counts 1.0 as an integer, so decode must accept it; encode writes canonical integers.
    let cases = corpus()?;
    let event = cases
        .iter()
        .find(|c| c["def"] == "Event" && c["valid"] == true)
        .ok_or("event")?;
    let mut raw = event["instance"].clone();
    raw["seq"] = serde_json::json!(7.0);
    let decoded: Event = protocol::decode(&raw)?;
    assert_eq!(decoded.seq, 7);
    assert!(schema::json_eq(&protocol::encode(&decoded)?, &raw));
    let message = cases
        .iter()
        .find(|c| c["def"] == "Message" && c["valid"] == true)
        .ok_or("message")?;
    let mut raw = message["instance"].clone();
    raw["role_epoch"] = serde_json::json!(2.0);
    raw["expected_revision"] = serde_json::json!(1e1);
    let decoded: Message = protocol::decode(&raw)?;
    assert_eq!((decoded.role_epoch, decoded.expected_revision), (2, 10));
    let plan = cases
        .iter()
        .find(|c| c["note"] == "plan with scores")
        .ok_or("plan")?;
    let mut raw = plan["instance"].clone();
    raw["tasks"][0]["user_value"] = serde_json::json!(5.0);
    let decoded: PlanProposal = protocol::decode(&raw)?;
    assert_eq!(decoded.tasks[0].user_value, Some(5));
    // Schema-valid but not representable: an error, never a panic or a silent truncation.
    raw["tasks"][0]["user_value"] = serde_json::json!(5);
    let mut event_raw = event["instance"].clone();
    event_raw["seq"] = serde_json::json!(1e20);
    assert!(protocol::decode::<Event>(&event_raw).is_err());
    Ok(())
}
