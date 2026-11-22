use super::*;
use serde_json::json;

fn spec(scenarios: Option<Value>) -> Value {
    let mut spec = json!({"requirements": [
        {"id": "REQ-A", "acceptance": [{"id": "AC-A-01", "test_id": "TEST-A-01"}, {"id": "AC-A-02", "test_id": "TEST-A-02"}]},
        {"id": "REQ-B", "acceptance": [{"id": "AC-B-01", "test_id": "TEST-B-01"}]},
    ]});
    if let Some(s) = scenarios {
        spec["scenarios"] = s;
    }
    spec
}

#[test]
fn scenarios_name_each_tests_check_and_a_missing_one_has_none() {
    let spec = spec(Some(json!([
        {"id": "TEST-A-01", "check_id": "unit"},
        {"id": "TEST-B-01", "check_id": "integration"},
        {"id": "TEST-A-02"},
    ])));
    let found: Vec<(String, Option<String>)> = tests(&spec)
        .into_iter()
        .map(|t| (t.test_id, t.check_id))
        .collect();
    assert_eq!(
        found,
        [
            ("TEST-A-01".to_owned(), Some("unit".to_owned())),
            ("TEST-A-02".to_owned(), None),
            ("TEST-B-01".to_owned(), Some("integration".to_owned())),
        ]
    );
}

#[test]
fn a_spec_without_scenarios_names_no_check_and_never_falls_back() {
    assert_eq!(checks(&spec(None)), None);
    assert!(tests(&spec(None)).iter().all(|t| t.check_id.is_none()));
    // An empty list is a scenarios key: every TEST is missing its check, none is looked up elsewhere.
    assert_eq!(checks(&spec(Some(json!([])))), Some(BTreeMap::new()));
}
