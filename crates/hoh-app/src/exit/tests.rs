//! TEST-004-01: every run state of the stored schema and every client event, against REQ-004's table.

use super::*;

#[test]
fn test_004_01_every_state_and_client_event_maps_as_the_table_says() {
    // The run states as the schema stores them (runs.state CHECK in contracts/sql/001_initial.sql).
    let names = hoh_core::persistence::allowed_values("runs", "state").expect("runs.state CHECK");
    assert_eq!(names.len(), 15);
    let mut rows = Vec::new();
    for name in &names {
        let state: RunState = serde_json::from_value(serde_json::Value::String(name.clone()))
            .unwrap_or_else(|e| panic!("{name} is not a run state: {e}"));
        let expected = match name.as_str() {
            "COMPLETED" => Some(0),
            "FAILED" => Some(1),
            "BLOCKED" => Some(3),
            "INCOMPLETE" => Some(4),
            "CANCELLED" => Some(5),
            "PAUSED" => Some(6),
            "CREATED" | "DISCOVERING" | "SPECIFYING" | "READY" | "RUNNING" | "VERIFYING"
            | "PAUSE_REQUESTED" | "WAITING_EXTERNAL" | "CANCEL_REQUESTED" => None,
            other => panic!("unexpected run state {other}"),
        };
        let actual = exit_code(Ending::Run(state));
        assert_eq!(actual, expected, "{name}");
        rows.push((name.clone(), actual));
    }
    for (event, expected) in [
        (ClientEvent::Usage, 64),
        (ClientEvent::HostUnavailable, 69),
        (ClientEvent::Config, 78),
        (ClientEvent::Sigint, 130),
        (ClientEvent::Sigterm, 143),
    ] {
        let actual = exit_code(Ending::Client(event));
        assert_eq!(actual, Some(expected), "{event:?}");
        rows.push((format!("{event:?}"), actual));
    }
    // 11 inputs have a code, only COMPLETED has 0.
    assert_eq!(rows.iter().filter(|(_, c)| c.is_some()).count(), 11);
    assert_eq!(
        rows.iter()
            .filter(|(_, c)| *c == Some(0))
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>(),
        ["COMPLETED"]
    );
    // The per-input table is an observable of TEST-004-01, kept with the check's evidence under run_check.
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let table: String = rows
            .iter()
            .map(|(n, c)| format!("{n}\t{}\n", c.map_or("none".to_owned(), |c| c.to_string())))
            .collect();
        std::fs::write(std::path::Path::new(&dir).join("exit-codes.tsv"), table)
            .expect("write the table");
    }
}
