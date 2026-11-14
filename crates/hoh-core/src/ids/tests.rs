use super::*;

/// The families numbered by series in docs/adr/ADR-030.md (decisions 5 and 10).
const FAMILIES: [&str; 3] = [
    "staffing/developer/1",
    "compat-suite-opened/1",
    "compat-suite-ended/1",
];

const NUMBERS: [u64; 7] = [1, 9, 10, 99, 100, 1_000_000_000, u64::MAX];

/// Parents of 1..=160 bytes: ASCII, and text with multi-byte characters (a cut must fall on a character boundary).
fn parents() -> Vec<String> {
    let mut parents = Vec::new();
    for len in 1..=MAX_ID {
        parents.push("p".repeat(len));
        let mut mixed = String::new();
        while mixed.len() < len {
            mixed.push(if mixed.len().is_multiple_of(3) {
                'é'
            } else {
                'r'
            });
        }
        parents.push(mixed);
    }
    parents
}

#[test]
fn series_members_keep_their_stem() {
    // M2T15D-202: every member is within MAX_ID, starts with its stem and `/`, and distinct numbers give distinct ids,
    // whatever the parent's length; numbering by `child_id(p, "…/<n>")` fails at n = 1 for a 160-character parent.
    for parent in parents() {
        for family in FAMILIES {
            let stem = series(&parent, family);
            let mut seen = std::collections::BTreeSet::new();
            for n in NUMBERS {
                let id = member(&stem, n);
                assert!(
                    id.len() <= MAX_ID && id.chars().count() <= MAX_ID,
                    "{parent:?} {family} {n}: {} bytes",
                    id.len()
                );
                assert!(
                    id.starts_with(&format!("{stem}/")),
                    "{parent:?} {family} {n}: {id:?} does not start with {stem:?}/"
                );
                assert_eq!(&id[stem.len() + 1..], n.to_string(), "{id:?}");
                assert!(seen.insert(id), "{parent:?} {family} {n}: repeated");
            }
        }
    }
}

#[test]
fn a_short_series_is_its_parent_and_family() {
    assert_eq!(series("run-1", "staffing/qa/2"), "run-1/staffing/qa/2");
    assert_eq!(member("run-1/staffing/qa/2", 3), "run-1/staffing/qa/2/3");
    // Shortened to leave room for any number, and the same input gives the same stem.
    let long = "r".repeat(MAX_ID);
    let stem = series(&long, "compat-suite-opened/1");
    assert!(stem.len() <= MAX_ID - 21, "{}", stem.len());
    assert_eq!(stem, series(&long, "compat-suite-opened/1"));
    assert_ne!(stem, series(&long, "compat-suite-ended/1"));
}
