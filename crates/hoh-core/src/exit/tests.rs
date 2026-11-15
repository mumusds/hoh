//! REQ-004's table for the run states: the stopping states E have their codes, every other state has none.

use super::*;

#[test]
fn the_stopping_states_have_the_table_s_codes_and_no_other_state_has_one() {
    let names = crate::persistence::allowed_values("runs", "state").expect("runs.state CHECK");
    assert_eq!(names.len(), 15);
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
            _ => None,
        };
        assert_eq!(run_code(state), expected, "{name}");
    }
}
