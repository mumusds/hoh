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

fn input() -> Input {
    let s = |v: &str| v.to_owned();
    Input {
        run_id: s("R"),
        spec_revision: Some(2),
        spec_source: SpecSource::Candidate,
        spec: spec(Some(json!([
            {"id": "TEST-A-01", "check_id": "unit"},
            {"id": "TEST-A-02", "check_id": "unit"},
            {"id": "TEST-B-01", "check_id": "integration"},
        ]))),
        spec_error: None,
        candidate_id: Some(s("C2")),
        tasks: vec![
            TaskRow {
                task_id: s("T1"),
                requirement_ids: vec![s("REQ-A")],
            },
            TaskRow {
                task_id: s("T2"),
                requirement_ids: vec![],
            },
        ],
        candidates: vec![
            CandidateRow {
                candidate_id: s("C1"),
                state: s("STALE"),
            },
            CandidateRow {
                candidate_id: s("C2"),
                state: s("FROZEN"),
            },
        ],
        evidence: vec![
            EvidenceRow {
                evidence_id: s("E1"),
                candidate_id: s("C2"),
                check_id: s("unit"),
                outcome: s("PASS"),
            },
            EvidenceRow {
                evidence_id: s("E2"),
                candidate_id: s("C2"),
                check_id: s("unit"),
                outcome: s("PASS"),
            },
            EvidenceRow {
                evidence_id: s("E9"),
                candidate_id: s("C0"),
                check_id: s("unit"),
                outcome: s("PASS"),
            },
        ],
        results: vec![
            // Current and whole.
            ResultRow {
                ac_id: s("AC-A-01"),
                spec_revision: 2,
                candidate_id: s("C2"),
                evidence_id: s("E1"),
                status: s("PASS"),
            },
            // A PASS bound to an older candidate: orphan, not broken (its evidence is there).
            ResultRow {
                ac_id: s("AC-A-02"),
                spec_revision: 2,
                candidate_id: s("C1"),
                evidence_id: s("E2"),
                status: s("PASS"),
            },
            // History of revision 1: stale and orphan.
            ResultRow {
                ac_id: s("AC-B-01"),
                spec_revision: 1,
                candidate_id: s("C1"),
                evidence_id: s("E9"),
                status: s("PASS"),
            },
        ],
    }
}

#[test]
fn the_export_names_each_broken_link_orphaned_pass_and_stale_node() {
    let e = export(&input());
    assert_eq!(
        e["broken"],
        json!([
            {"node": "test", "id": "TEST-B-01", "missing": "evidence"},
            {"node": "evidence", "id": "E9", "missing": "candidate"},
            {"node": "task", "id": "T2", "missing": "requirement"},
        ])
    );
    let ids = |key: &str| -> Vec<String> {
        e[key]
            .as_array()
            .into_iter()
            .flatten()
            .map(|n| {
                format!(
                    "{}@{}",
                    n["ac_id"].as_str().unwrap_or(""),
                    n["candidate_id"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    assert_eq!(ids("orphan"), ["AC-A-02@C1", "AC-B-01@C1"]);
    // Every result on the STALE candidate or of the old revision, not only the changed chain's.
    assert_eq!(ids("stale"), ["AC-A-02@C1", "AC-B-01@C1"]);
    assert_eq!(e["results"][0]["stale"], false);
    assert_eq!(e["acceptance"][0]["statuses"], json!(["PASS"]));
    assert_eq!(
        e["acceptance"][1]["statuses"],
        json!([]),
        "no PASS carried from another candidate"
    );
    assert_eq!(e["candidates"][1]["current"], true);
    assert_eq!(e["requirements"][0]["task_ids"], json!(["T1"]));
}

#[test]
fn an_ac_without_a_test_is_broken_for_its_missing_test() {
    let mut i = input();
    i.spec["requirements"][1]["acceptance"][0]
        .as_object_mut()
        .map(|a| a.remove("test_id"));
    let e = export(&i);
    assert_eq!(
        e["broken"][0],
        json!({"node": "acceptance", "id": "AC-B-01", "missing": "test"})
    );
}
