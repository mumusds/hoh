use super::*;
use std::collections::BTreeSet;

fn ids(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn fact(id: &str, polarity: Polarity, fingerprint: &str, candidate: &str, issues: &[&str]) -> Fact {
    Fact {
        id: id.to_owned(),
        fingerprint: fingerprint.to_owned(),
        polarity,
        candidate: candidate.to_owned(),
        issue_ids: ids(issues),
    }
}

fn row(ac: &str, candidate: &str, status: Row, failing: &[&str]) -> AcRow {
    AcRow {
        ac: ac.to_owned(),
        candidate: candidate.to_owned(),
        status,
        failing: ids(failing),
        own_spec: true,
        degradable: false,
    }
}

fn observed(id: &str, candidate: &str, fingerprint: &str) -> Issue {
    Issue {
        id: id.to_owned(),
        candidate: candidate.to_owned(),
        subject: Subject::Observed {
            fingerprint: fingerprint.to_owned(),
        },
    }
}

fn acceptance(id: &str, candidate: &str, ac: &str, test: &str) -> Issue {
    Issue {
        id: id.to_owned(),
        candidate: candidate.to_owned(),
        subject: Subject::Acceptance {
            ac: ac.to_owned(),
            test: Some(test.to_owned()),
        },
    }
}

/// Candidates C1 < C2 < C3 < C4 by their freeze.
fn ordered() -> BTreeMap<String, i64> {
    (1..=4)
        .map(|n| (format!("C{n}"), i64::from(n) * 10))
        .collect()
}

/// One thing a world can gain: a fact, a row, a veto.
#[derive(Debug, Clone)]
enum Item {
    Fact(Fact),
    Row(AcRow),
    Veto(String),
}

fn add(world: &mut World, item: &Item) {
    match item {
        Item::Fact(f) => world.facts.push(f.clone()),
        Item::Row(r) => world.rows.push(r.clone()),
        Item::Veto(c) => {
            world.vetoed.insert(c.clone());
        }
    }
}

/// Every permutation of `items` (Heap's algorithm).
fn permutations(items: &[Item]) -> Vec<Vec<Item>> {
    fn heap(k: usize, items: &mut Vec<Item>, out: &mut Vec<Vec<Item>>) {
        if k <= 1 {
            out.push(items.clone());
            return;
        }
        heap(k - 1, items, out);
        for i in 0..k - 1 {
            if k.is_multiple_of(2) {
                items.swap(i, k - 1);
            } else {
                items.swap(0, k - 1);
            }
            heap(k - 1, items, out);
        }
    }
    let mut out = Vec::new();
    let mut items = items.to_vec();
    let n = items.len();
    heap(n, &mut items, &mut out);
    out
}

/// `items` added one by one in that order to a world of `order` and `issues`, the state of each Issue materialised after
/// each (as `settle_all` does in each transaction): each Issue's final state; no event key is written twice, and after
/// every step the state agrees with the function.
fn materialised(
    order: &BTreeMap<String, i64>,
    issues: &[Issue],
    items: &[Item],
) -> BTreeMap<String, String> {
    let mut world = World {
        order: order.clone(),
        ..World::default()
    };
    let mut states: BTreeMap<String, (String, Option<String>, BTreeSet<String>)> = issues
        .iter()
        .map(|i| (i.id.clone(), ("OPEN".to_owned(), None, BTreeSet::new())))
        .collect();
    for item in items {
        add(&mut world, item);
        for issue in issues {
            let judged = judge(issue, &world);
            let (state, last, keys) = states.get_mut(&issue.id).expect("an issue's state");
            if let Some(t) = transition(state, &judged, last.as_deref()) {
                assert!(
                    keys.insert(t.key.clone()),
                    "{} written twice for {}: {items:?}",
                    t.key,
                    issue.id
                );
                if t.to == "RESOLVED" {
                    *last = Some(t.basis.clone());
                }
                *state = t.to.to_owned();
            }
            assert_eq!(
                *state == "RESOLVED",
                judged.resolved(),
                "{} after {item:?}",
                issue.id
            );
        }
    }
    states.into_iter().map(|(id, (s, _, _))| (id, s)).collect()
}

/// Whether each Issue is resolved, judged on all of `items` at once.
fn direct(
    order: &BTreeMap<String, i64>,
    issues: &[Issue],
    items: &[Item],
) -> BTreeMap<String, bool> {
    let mut world = World {
        order: order.clone(),
        ..World::default()
    };
    for item in items {
        add(&mut world, item);
    }
    issues
        .iter()
        .map(|i| (i.id.clone(), judge(i, &world).resolved()))
        .collect()
}

/// Every order of `items` ends with each Issue resolved exactly when the direct judgement says so (the label REOPENED
/// aside, M2T18D-552): the direct judgement.
fn every_order_agrees(
    order: &BTreeMap<String, i64>,
    issues: &[Issue],
    items: &[Item],
) -> BTreeMap<String, bool> {
    let want = direct(order, issues, items);
    for permutation in permutations(items) {
        let got: BTreeMap<String, bool> = materialised(order, issues, &permutation)
            .into_iter()
            .map(|(id, s)| (id, s == "RESOLVED"))
            .collect();
        assert_eq!(got, want, "{permutation:?}");
    }
    want
}

/// A small deterministic generator (an LCG): no dependency, the same worlds every run.
struct Gen(u64);

impl Gen {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 33) as usize) % n.max(1)
    }
}

#[test]
fn whether_an_issue_is_resolved_is_the_same_in_every_order_of_its_facts_rows_and_vetoes() {
    // docs/adr/ADR-029.md decision 10 and J6 (M2T18D-502, -550, -600, -601, -602, -650, -651, -755): over generated
    // candidate sequences (2 to 4), Issues (two fingerprints, an AC with two tests), observation facts, acceptance rows
    // and vetoes, adding them one by one in every order and materialising after each ends with the direct judgement
    // of the set; no state change is written twice.
    let statuses = [Row::Pass, Row::Fail, Row::Degraded, Row::NotRun];
    let mut resolved_somewhere = 0;
    for seed in 0..400_u64 {
        let mut g = Gen(seed);
        let candidates: Vec<String> = (1..=2 + g.next(3)).map(|n| format!("C{n}")).collect();
        let order: BTreeMap<String, i64> = candidates
            .iter()
            .enumerate()
            .map(|(n, c)| (c.clone(), n as i64 * 10 + 1))
            .collect();
        let pick = |g: &mut Gen| candidates[g.next(candidates.len())].clone();
        let mut issues = Vec::new();
        for n in 0..1 + g.next(2) {
            let fingerprint = ["f1", "f2"][g.next(2)];
            issues.push(observed(&format!("I{n}"), &pick(&mut g), fingerprint));
        }
        issues.push(acceptance(
            "IA",
            &pick(&mut g),
            "AC-A",
            ["t1", "t2"][g.next(2)],
        ));
        let degradable = g.next(2) == 0;
        let mut items = Vec::new();
        for n in 0..3 + g.next(3) {
            match g.next(4) {
                0 | 1 => {
                    let polarity = if g.next(2) == 0 {
                        Polarity::Present
                    } else {
                        Polarity::Absent
                    };
                    let fingerprint = ["f1", "f2"][g.next(2)];
                    let named: Vec<&str> = issues
                        .iter()
                        .filter(|i| {
                            i.subject
                                == Subject::Observed {
                                    fingerprint: fingerprint.to_owned(),
                                }
                        })
                        .filter(|_| g.next(3) > 0)
                        .map(|i| i.id.as_str())
                        .collect();
                    items.push(Item::Fact(fact(
                        &format!("obs-{n}"),
                        polarity,
                        fingerprint,
                        &pick(&mut g),
                        &named,
                    )));
                }
                2 => {
                    let status = statuses[g.next(statuses.len())];
                    let failing: &[&str] = if status == Row::Fail {
                        [&["t1"][..], &["t2"][..], &["t1", "t2"][..]][g.next(3)]
                    } else {
                        &[]
                    };
                    let mut r = row("AC-A", &pick(&mut g), status, failing);
                    r.degradable = degradable;
                    items.push(Item::Row(r));
                }
                _ => items.push(Item::Veto(pick(&mut g))),
            }
        }
        let want = every_order_agrees(&order, &issues, &items);
        resolved_somewhere += want.values().filter(|r| **r).count();
    }
    // The generator reaches resolutions, not only unresolved worlds.
    assert!(resolved_somewhere > 40, "{resolved_somewhere}");
}

#[test]
fn an_absent_fact_on_a_later_candidate_than_a_present_one_resolves_in_every_order_m2t18d_401() {
    // M2T18D-401: X delivered to C2's and C3's positive rounds and to C2's negative one: six orders, all RESOLVED (C3's
    // absent is later than C2's present).
    let issues = [observed("X", "C1", "f")];
    let items = [
        Item::Fact(fact("a2", Polarity::Absent, "f", "C2", &["X"])),
        Item::Fact(fact("n2", Polarity::Present, "f", "C2", &[])),
        Item::Fact(fact("a3", Polarity::Absent, "f", "C3", &["X"])),
    ];
    assert_eq!(permutations(&items).len(), 6);
    let want = every_order_agrees(&ordered(), &issues, &items);
    assert!(want["X"]);
    let mut world = World {
        order: ordered(),
        ..World::default()
    };
    items.iter().for_each(|i| add(&mut world, i));
    assert_eq!(
        judge(&issues[0], &world),
        Judged::Resolved {
            basis: "a3".to_owned()
        }
    );
}

#[test]
fn an_acceptance_issue_failing_again_on_a_later_candidate_stays_unresolved_m2t18d_402() {
    // M2T18D-402, -653: C1's acceptance failure X on test t; the REJECTED C2 later satisfies the AC, C3 fails it on the
    // same test: in both orders X is not resolved. Without C3's failure, C2's rows resolve it (a candidate's state is
    // not read).
    let issues = [acceptance("X", "C1", "AC-A", "t")];
    let items = [
        Item::Row(row("AC-A", "C1", Row::Fail, &["t"])),
        Item::Row(row("AC-A", "C3", Row::Fail, &["t"])),
        Item::Row(row("AC-A", "C2", Row::Pass, &[])),
    ];
    assert!(!every_order_agrees(&ordered(), &issues, &items)["X"]);
    let without = [items[0].clone(), items[2].clone()];
    assert!(every_order_agrees(&ordered(), &issues, &without)["X"]);
    // Another test failing on C3 does not undo it: the Issue is its test's.
    let other = [
        items[0].clone(),
        items[2].clone(),
        Item::Row(row("AC-A", "C3", Row::Fail, &["u"])),
    ];
    assert!(every_order_agrees(&ordered(), &issues, &other)["X"]);
}

#[test]
fn entries_sharing_an_issue_resolve_alike_in_both_orders_and_a_same_fingerprint_finding_beside_them_does_not_m2t18d_451()
 {
    // M2T18D-451 (a): entry {X, X'} in one round, {X'} in another, both confirmed fixed on C2: both resolved in both
    // orders. (b): a finding of the same fingerprint on C2 that was in no entry: present on C2, so neither resolves.
    let issues = [observed("X", "C1", "f"), observed("X2", "C1", "f")];
    let a = [
        Item::Fact(fact("ra", Polarity::Absent, "f", "C2", &["X", "X2"])),
        Item::Fact(fact("rb", Polarity::Absent, "f", "C2", &["X2"])),
    ];
    let want = every_order_agrees(&ordered(), &issues, &a);
    assert_eq!((want["X"], want["X2"]), (true, true));
    let b = [
        a[0].clone(),
        a[1].clone(),
        Item::Fact(fact("new", Polarity::Present, "f", "C2", &["N"])),
    ];
    let want = every_order_agrees(&ordered(), &issues, &b);
    assert_eq!((want["X"], want["X2"]), (false, false));
}

#[test]
fn a_reissued_id_or_a_change_request_on_the_confirming_candidate_keeps_the_issue_unresolved_m2t18d_500_701()
 {
    // M2T18D-500, -551: the Issue's id reported again (a present fact by its fingerprint) beside the other side's
    // RESOLVED (an absent fact) on the same candidate: unresolved. M2T18D-701, -800: a sibling round's change request
    // of the same fingerprint on C2 (its `issue.opened`, read as present) beside C2's confirmation: unresolved, in both
    // orders.
    let issues = [observed("X", "C1", "f")];
    for present in ["reissued", "change-request"] {
        let items = [
            Item::Fact(fact("a2", Polarity::Absent, "f", "C2", &["X"])),
            Item::Fact(fact(present, Polarity::Present, "f", "C2", &[])),
        ];
        assert!(
            !every_order_agrees(&ordered(), &issues, &items)["X"],
            "{present}"
        );
    }
}

#[test]
fn a_veto_of_the_confirming_candidate_undoes_its_resolution_whatever_its_state_m2t18d_752() {
    // M2T18D-601, -752 analogue: confirmed fixed on C2, then an INVALID evidence on C2 (C2 may already be REJECTED):
    // no longer resolved, REOPENED with the veto as its basis; a later confirmation on C3 resolves it again.
    let issue = observed("X", "C1", "f");
    let mut world = World {
        order: ordered(),
        ..World::default()
    };
    add(
        &mut world,
        &Item::Fact(fact("a2", Polarity::Absent, "f", "C2", &["X"])),
    );
    let resolved = judge(&issue, &world);
    assert_eq!(
        resolved,
        Judged::Resolved {
            basis: "a2".to_owned()
        }
    );
    let first = transition("OPEN", &resolved, None).expect("resolved");
    assert_eq!((first.to, first.kind), ("RESOLVED", "issue.resolved"));
    add(&mut world, &Item::Veto("C2".to_owned()));
    let vetoed = judge(&issue, &world);
    assert_eq!(
        vetoed,
        Judged::Unresolved {
            basis: Some("vetoed/C2".to_owned())
        }
    );
    let reopened = transition("RESOLVED", &vetoed, Some("a2")).expect("reopened");
    assert_eq!(
        (reopened.to, reopened.kind, reopened.undoes.as_deref()),
        ("REOPENED", "issue.reopened", Some("a2"))
    );
    assert_eq!(transition("REOPENED", &vetoed, Some("a2")), None);
    add(
        &mut world,
        &Item::Fact(fact("a3", Polarity::Absent, "f", "C3", &["X"])),
    );
    let again = transition("REOPENED", &judge(&issue, &world), Some("a2")).expect("resolved");
    assert_eq!((again.to, again.basis.as_str()), ("RESOLVED", "a3"));
    assert_ne!(again.key, first.key);
}

#[test]
fn a_recurrence_on_a_later_candidate_reopens_and_one_on_an_earlier_candidate_does_not_m2t18d_05() {
    // Decision 10 (M2T18D-05): resolved on C2; the same fingerprint present again on C3 reopens it; a late finding on
    // C1 (earlier than the confirmation) does not; only an absent fact naming the Issue resolves it.
    let issue = observed("X", "C1", "f");
    let base = Item::Fact(fact("a2", Polarity::Absent, "f", "C2", &["X"]));
    let late = Item::Fact(fact("n1", Polarity::Present, "f", "C1", &[]));
    assert!(
        every_order_agrees(
            &ordered(),
            std::slice::from_ref(&issue),
            &[base.clone(), late]
        )["X"]
    );
    let recurred = Item::Fact(fact("n3", Polarity::Present, "f", "C3", &[]));
    assert!(!every_order_agrees(&ordered(), std::slice::from_ref(&issue), &[base, recurred])["X"]);
    // An absent fact naming other Issues, or on the Issue's own candidate, resolves nothing.
    let other = Item::Fact(fact("a2o", Polarity::Absent, "f", "C2", &["Y"]));
    let own = Item::Fact(fact("a1", Polarity::Absent, "f", "C1", &["X"]));
    assert!(!every_order_agrees(&ordered(), std::slice::from_ref(&issue), &[other, own])["X"]);
}

#[test]
fn an_acceptance_issue_is_resolved_by_a_later_candidates_satisfying_rows_degraded_only_where_it_may_degrade()
 {
    // Decision 10 (M2T18D-250): C1's acceptance failure on t is resolved by C2's rows when they satisfy the AC: PASS,
    // or a compliant DEGRADED on a degradable requirement; not by a DEGRADED where the requirement may not degrade, a
    // vetoed C2, rows of another specification, or a FLAKY history on C2.
    let issue = acceptance("X", "C1", "AC-A", "t");
    let judged = |rows: Vec<AcRow>, vetoed: &[&str]| {
        judge(
            &issue,
            &World {
                order: ordered(),
                vetoed: ids(vetoed),
                rows,
                ..World::default()
            },
        )
        .resolved()
    };
    let fail = row("AC-A", "C1", Row::Fail, &["t"]);
    let mut degraded = row("AC-A", "C2", Row::Degraded, &[]);
    degraded.degradable = true;
    assert!(judged(vec![fail.clone(), degraded.clone()], &[]));
    degraded.degradable = false;
    assert!(!judged(vec![fail.clone(), degraded], &[]));
    assert!(judged(
        vec![fail.clone(), row("AC-A", "C2", Row::Pass, &[])],
        &[]
    ));
    assert!(!judged(
        vec![fail.clone(), row("AC-A", "C2", Row::Pass, &[])],
        &["C2"]
    ));
    let mut foreign = row("AC-A", "C2", Row::Pass, &[]);
    foreign.own_spec = false;
    assert!(!judged(vec![fail.clone(), foreign], &[]));
    assert!(!judged(
        vec![
            fail.clone(),
            row("AC-A", "C2", Row::Pass, &[]),
            row("AC-A", "C2", Row::Unknown, &[])
        ],
        &[]
    ));
    // Its own candidate's PASS does not resolve it: only a later candidate's.
    assert!(!judged(vec![fail, row("AC-A", "C1", Row::Pass, &[])], &[]));
}

#[test]
fn a_transition_is_written_only_when_the_state_differs_and_its_key_names_its_basis() {
    let resolved = Judged::Resolved {
        basis: "b".to_owned(),
    };
    let unresolved = Judged::Unresolved {
        basis: Some("n".to_owned()),
    };
    assert_eq!(transition("RESOLVED", &resolved, Some("b")), None);
    assert_eq!(transition("OPEN", &unresolved, None), None);
    assert_eq!(transition("REOPENED", &unresolved, Some("b")), None);
    let t = transition("OPEN", &resolved, None).expect("a resolution");
    assert_eq!(t.key, "resolved/b");
    let r = transition("RESOLVED", &unresolved, Some("b")).expect("a reopening");
    assert_eq!((r.key.as_str(), r.basis.as_str()), ("reopened/b", "n"));
}
