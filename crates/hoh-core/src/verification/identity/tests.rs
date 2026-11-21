use super::*;

fn side(hoh: &[&str], native: &[(&str, &str)]) -> Identities {
    let mut ids = Identities::default();
    for id in hoh {
        ids.add_hoh(id);
    }
    for (agent, id) in native {
        ids.add_native(agent, id);
    }
    ids
}

#[test]
fn equal_hoh_ids_or_equal_agent_native_pairs_are_shared() {
    let a = side(&["S-1", "S-2"], &[("codex", "N-1"), ("claude", "N-2")]);
    let b = side(&["S-2", "S-3"], &[("codex", "N-1"), ("codex", "N-2")]);
    assert_eq!(
        shared(&a, &b),
        [
            Identity::Hoh("S-2".to_owned()),
            Identity::Native {
                agent: "codex".to_owned(),
                id: "N-1".to_owned()
            }
        ]
    );
}

#[test]
fn the_same_native_id_under_another_agent_is_not_shared() {
    // CR-029: a native id is namespaced by its agent.
    let a = side(&["S-1"], &[("claude", "N-1")]);
    let b = side(&["S-2"], &[("codex", "N-1")]);
    assert!(shared(&a, &b).is_empty());
}

#[test]
fn a_claimed_id_meets_a_hoh_id_and_a_native_id_of_its_agent() {
    // A native attempt's session_ids may name another role's HoH session or its native session.
    let mut reviewer = Identities::default();
    reviewer.claim("codex", "S-dev");
    reviewer.claim("codex", "N-dev");
    let others = side(&["S-dev"], &[("codex", "N-dev"), ("claude", "S-dev")]);
    let found: Vec<String> = shared(&reviewer, &others)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(found, ["HoH session S-dev", "codex native session N-dev",]);
    let mut elsewhere = Identities::default();
    elsewhere.claim("claude", "N-dev");
    assert!(shared(&elsewhere, &side(&[], &[("codex", "N-dev")])).is_empty());
}
