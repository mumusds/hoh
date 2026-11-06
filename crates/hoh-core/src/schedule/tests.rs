use super::*;
use crate::protocol::{SchedulerSettings, SchedulerWeights};

fn task(id: &str, layer: u8, seq: i64) -> Candidate {
    Candidate {
        task_id: id.to_owned(),
        created_seq: seq,
        layer,
        writer: true,
        user_value: 0,
        unblock_value: 0,
        risk_reduction: 0,
        information_gain: 0,
        cost_estimate: None,
        wait_rounds: 0,
        first_eligible_seq: None,
        eligibility: Eligibility::ALL,
    }
}

const FREE: Slots = Slots {
    writer: true,
    readers: 2,
};

/// A deterministic shuffle (SplitMix64-driven Fisher-Yates) with a fixed seed.
fn shuffled<T: Clone>(items: &[T], seed: u64) -> Vec<T> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut out = items.to_vec();
    for i in (1..out.len()).rev() {
        let j = usize::try_from(next() % u64::try_from(i + 1).unwrap_or(u64::MAX)).unwrap_or(0);
        out.swap(i, j);
    }
    out
}

fn ids(ranked: &[Ranked]) -> Vec<&str> {
    ranked.iter().map(|r| r.task_id.as_str()).collect()
}

#[test]
fn test_038_01_layers_scores_and_ties_are_stable() {
    let mut a = task("t-a", 0, 1);
    a.cost_estimate = Some(5);
    let mut b = task("t-b", 1, 2);
    b.unblock_value = 2;
    b.cost_estimate = Some(1);
    let mut c = task("t-c", 2, 10);
    c.user_value = 5;
    c.cost_estimate = Some(1);
    let mut f = c.clone();
    f.task_id = "t-f".to_owned();
    let mut d = task("t-d", 2, 12);
    d.user_value = 3;
    d.risk_reduction = 2;
    d.cost_estimate = Some(2);
    let mut e = d.clone();
    e.task_id = "t-e".to_owned();
    e.created_seq = 11;
    let set = vec![a, b, c, d, e, f];
    let params = Params::default();
    let expected = ["t-a", "t-b", "t-c", "t-f", "t-e", "t-d"];
    let mut orders = vec![rank(&params, &set)];
    for seed in 1..=10 {
        orders.push(rank(&params, &shuffled(&set, seed)));
    }
    for ranked in &orders {
        assert_eq!(ids(ranked), expected);
    }
    let scores: Vec<(&str, f64)> = orders[0]
        .iter()
        .map(|r| (r.task_id.as_str(), r.score.value()))
        .collect();
    for (id, want) in [
        ("t-a", 0.0),
        ("t-b", 6.0),
        ("t-c", 20.0),
        ("t-f", 20.0),
        ("t-e", 9.0),
        ("t-d", 9.0),
    ] {
        let got = scores
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, s)| *s)
            .unwrap_or(f64::NAN);
        assert!((got - want).abs() <= 1e-9, "{id}: {got} != {want}");
    }
}

/// The rounds of AC-038-04 / -06: each round a new L joins; the selected task leaves; the others that were eligible
/// count one more waited round and keep the seq of the round they first became eligible in.
struct Rounds {
    board: Vec<Candidate>,
    next_seq: i64,
    round_seq: i64,
}

impl Rounds {
    fn new(first: Candidate) -> Rounds {
        let next_seq = first.created_seq + 1;
        Rounds {
            board: vec![first],
            next_seq,
            round_seq: 0,
        }
    }

    fn run(&mut self, params: &Params) -> Ranked {
        self.round_seq += 1;
        let mut l = task(&format!("l-{:03}", self.next_seq), 2, self.next_seq);
        l.user_value = 3;
        l.cost_estimate = Some(2);
        self.next_seq += 1;
        self.board.push(l);
        for c in &mut self.board {
            c.first_eligible_seq.get_or_insert(self.round_seq);
        }
        let round = round(params, &self.board, FREE);
        let selected = round.selected.unwrap_or_else(|| panic!("nothing selected"));
        for &i in &round.waited {
            self.board[i].wait_rounds += 1;
        }
        self.board.retain(|c| c.task_id != selected.task_id);
        selected
    }
}

#[test]
fn test_038_04_age_bonus_lets_h_win_round_5_and_never_exceeds_10() {
    // H's id sorts after every L's: it can win the tie in round 5 only by its earlier created_seq.
    let mut h = task("z-h", 2, 1);
    h.user_value = 2;
    h.cost_estimate = Some(2);
    let params = Params::default();
    let mut rounds = Rounds::new(h);
    let mut picks = Vec::new();
    let mut max_bonus = 0;
    for _ in 0..12 {
        for c in &rounds.board {
            max_bonus = max_bonus.max(c.wait_rounds.min(AGE_BONUS_CAP));
        }
        picks.push(rounds.run(&params));
    }
    let names: Vec<&str> = picks.iter().map(|p| p.task_id.as_str()).collect();
    assert!(names[..4].iter().all(|n| n.starts_with("l-")), "{names:?}");
    let h = &picks[4];
    assert_eq!(h.task_id, "z-h", "{names:?}");
    assert_eq!(h.age_bonus, 4);
    assert!((h.score.value() - 6.0).abs() <= 1e-9, "{:?}", h.score);
    assert!(max_bonus <= AGE_BONUS_CAP);
    assert!(picks.iter().all(|p| p.age_bonus <= AGE_BONUS_CAP));
}

#[test]
fn test_038_06_aged_sublayer_selects_h_in_round_21() {
    let mut h = task("h", 2, 1);
    h.cost_estimate = Some(5);
    let params = Params::default();
    let mut rounds = Rounds::new(h);
    let mut picks = Vec::new();
    let mut longest_wait = 0;
    for _ in 0..25 {
        picks.push(rounds.run(&params));
        for c in &rounds.board {
            longest_wait = longest_wait.max(c.wait_rounds);
        }
    }
    assert!(
        picks[..20].iter().all(|p| p.task_id.starts_with("l-")),
        "{:?}",
        ids(&picks)
    );
    assert_eq!(picks[20].task_id, "h", "{:?}", ids(&picks));
    assert_eq!(picks[20].tier.label(), "aged");
    assert!(longest_wait <= 20, "waited {longest_wait} rounds");
}

#[test]
fn a_busy_slot_skips_its_tasks_and_they_wait() {
    let mut writer = task("w", 2, 1);
    writer.user_value = 5;
    let mut reader = task("r", 2, 2);
    reader.writer = false;
    reader.user_value = 1;
    let mut ineligible = task("x", 0, 3);
    ineligible.eligibility.dependencies_accepted = false;
    let params = Params::default();
    let set = vec![writer, reader, ineligible];
    let busy = Slots {
        writer: false,
        readers: 1,
    };
    let round = round(&params, &set, busy);
    assert_eq!(
        round.selected.map(|s| s.task_id),
        Some("r".to_owned()),
        "a free reader slot takes the reader"
    );
    assert_eq!(round.waited, vec![0]);
    let none = super::round(
        &params,
        &set,
        Slots {
            writer: false,
            readers: 0,
        },
    );
    assert_eq!(none.selected, None);
    assert_eq!(none.waited, vec![0, 1]);
}

#[test]
fn snapshot_settings_override_only_what_they_name() {
    assert_eq!(Params::from_settings(None), Params::default());
    let settings = SchedulerSettings {
        weights: Some(SchedulerWeights {
            user_value: Some(1),
            unblock_value: None,
            risk_reduction: Some(0),
            information_gain: None,
        }),
        max_wait_rounds: Some(7),
    };
    let params = Params::from_settings(Some(&settings));
    assert_eq!(
        params.weights,
        Weights {
            user_value: 1,
            unblock_value: 3,
            risk_reduction: 0,
            information_gain: 2,
        }
    );
    assert_eq!(params.max_wait_rounds, 7);
    let mut t = task("t", 2, 1);
    t.user_value = 5;
    t.unblock_value = 5;
    t.risk_reduction = 5;
    t.information_gain = 5;
    t.cost_estimate = None;
    t.wait_rounds = 30;
    // (1*5 + 3*5 + 0*5 + 2*5 + min(30, 10)) / 3
    assert_eq!(
        score(&params, &t),
        Score {
            numerator: 40,
            denominator: 3
        }
    );
}

#[test]
fn layer_0_stays_ahead_of_aged_tasks() {
    let mut aged = task("aged", 1, 1);
    aged.wait_rounds = 20;
    aged.first_eligible_seq = Some(1);
    let critical = task("crit", 0, 2);
    let ranked = rank(&Params::default(), &[aged, critical]);
    assert_eq!(ids(&ranked), ["crit", "aged"]);
    assert_eq!(ranked[1].tier, Tier::Aged);
}

/// How a generated task starts (AC-038-02: some are accepted already, leased, bound to an old spec revision,
/// missing a role capability or over budget).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Start {
    Open,
    Accepted,
    Leased,
    OldSpec,
    Incapable,
    OverBudget,
}

/// One generated task: dependencies are picks among the tasks before it, so the graph is acyclic.
#[derive(Debug, Clone)]
struct GenTask {
    picks: Vec<u16>,
    layer: u8,
    writer: bool,
    scores: [u8; 4],
    cost: Option<u8>,
    start: Start,
    waited: u32,
    name: u32,
}

fn gen_task() -> impl proptest::strategy::Strategy<Value = GenTask> {
    use proptest::prelude::*;
    let start = prop_oneof![
        14 => Just(Start::Open),
        2 => Just(Start::Accepted),
        1 => Just(Start::Leased),
        1 => Just(Start::OldSpec),
        1 => Just(Start::Incapable),
        1 => Just(Start::OverBudget),
    ];
    (
        proptest::collection::vec(any::<u16>(), 0..=3),
        0u8..=2,
        any::<bool>(),
        proptest::array::uniform4(0u8..=5),
        proptest::option::of(1u8..=5),
        start,
        0u32..=25,
        any::<u32>(),
    )
        .prop_map(
            |(picks, layer, writer, scores, cost, start, waited, name)| GenTask {
                picks,
                layer,
                writer,
                scores,
                cost,
                start,
                waited,
                name,
            },
        )
}

/// A generated world: tasks, their dependency lists (by index) and their dependents.
struct World {
    tasks: Vec<GenTask>,
    deps: Vec<Vec<usize>>,
    dependents: Vec<Vec<usize>>,
    ids: Vec<String>,
}

impl World {
    fn new(tasks: Vec<GenTask>) -> World {
        let mut deps = Vec::with_capacity(tasks.len());
        let mut dependents = vec![Vec::new(); tasks.len()];
        for (i, t) in tasks.iter().enumerate() {
            let mut mine: Vec<usize> = if i == 0 {
                Vec::new()
            } else {
                t.picks.iter().map(|p| usize::from(*p) % i).collect()
            };
            mine.sort_unstable();
            mine.dedup();
            for &d in &mine {
                dependents[d].push(i);
            }
            deps.push(mine);
        }
        let ids = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| format!("t{:08x}-{i}", t.name))
            .collect();
        World {
            tasks,
            deps,
            dependents,
            ids,
        }
    }

    fn candidate(&self, i: usize) -> Candidate {
        let t = &self.tasks[i];
        Candidate {
            task_id: self.ids[i].clone(),
            created_seq: i64::try_from(i).unwrap_or(i64::MAX),
            layer: t.layer,
            writer: t.writer,
            user_value: t.scores[0],
            unblock_value: t.scores[1],
            risk_reduction: t.scores[2],
            information_gain: t.scores[3],
            cost_estimate: t.cost,
            wait_rounds: t.waited,
            first_eligible_seq: None,
            eligibility: Eligibility {
                dependencies_accepted: true,
                current_spec: t.start != Start::OldSpec,
                not_leased: t.start != Start::Leased,
                capable: t.start != Start::Incapable,
                budget_reservable: t.start != Start::OverBudget,
            },
        }
    }

    /// Select until nothing is eligible, the tasks given in `order`; a selected task counts as accepted. Each pick is
    /// checked against the world itself (not against the scheduler's own facts). The dispatch sequence and the number
    /// of picks that broke an eligibility condition.
    fn dispatch(&self, params: &Params, order: &[usize]) -> (Vec<String>, usize) {
        let n = self.tasks.len();
        let mut accepted: Vec<bool> = self
            .tasks
            .iter()
            .map(|t| t.start == Start::Accepted)
            .collect();
        let mut open: Vec<usize> = (0..n)
            .map(|i| self.deps[i].iter().filter(|d| !accepted[**d]).count())
            .collect();
        // The board as two parallel vectors: world index and candidate (the scheduler reads the candidates as is).
        let position: Vec<usize> = {
            let mut at = vec![0; n];
            for (p, &i) in order.iter().enumerate() {
                at[i] = p;
            }
            at
        };
        let mut index: Vec<usize> = order
            .iter()
            .copied()
            .filter(|i| !accepted[*i] && open[*i] == 0)
            .collect();
        let mut board: Vec<Candidate> = index.iter().map(|&i| self.candidate(i)).collect();
        let mut sequence = Vec::new();
        let mut violations = 0;
        let mut round_seq = 0_i64;
        loop {
            round_seq += 1;
            for c in &mut board {
                if c.eligibility.eligible() {
                    c.first_eligible_seq.get_or_insert(round_seq);
                }
            }
            let outcome = round(params, &board, FREE);
            let Some(selected) = outcome.selected else {
                break;
            };
            for &w in &outcome.waited {
                board[w].wait_rounds += 1;
            }
            let at = board
                .iter()
                .position(|c| c.task_id == selected.task_id)
                .unwrap_or(usize::MAX);
            board.swap_remove(at);
            let chosen = index.swap_remove(at);
            let t = &self.tasks[chosen];
            let fits = self.deps[chosen].iter().all(|d| accepted[*d])
                && !accepted[chosen]
                && t.start == Start::Open;
            if !fits {
                violations += 1;
            }
            sequence.push(selected.task_id);
            accepted[chosen] = true;
            let mut newly: Vec<usize> = Vec::new();
            for &d in &self.dependents[chosen] {
                open[d] -= 1;
                if open[d] == 0 && !accepted[d] {
                    newly.push(d);
                }
            }
            // They join in the given order's sense: by their position in `order`.
            newly.sort_by_key(|d| position[*d]);
            for d in newly {
                index.push(d);
                board.push(self.candidate(d));
            }
        }
        (sequence, violations)
    }
}

#[test]
fn test_038_02_random_dags_dispatch_identically_in_any_order() {
    use proptest::prelude::*;
    use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
    use std::sync::Mutex;
    let config = Config {
        cases: 1000,
        failure_persistence: None,
        max_shrink_iters: 0,
        ..Config::default()
    };
    let seed = *b"hoh-test-038-02-fixed-seed-00001";
    let mut runner =
        TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &seed));
    // Mostly sizes across 10..=999, about one in twenty exactly 1000.
    let size = prop_oneof![19 => 10usize..=999, 1 => Just(1000usize)];
    let worlds = size.prop_flat_map(|n| proptest::collection::vec(gen_task(), n));
    let log: Mutex<Vec<(usize, usize, usize)>> = Mutex::new(Vec::new());
    let params = Params::default();
    let result = runner.run(&worlds, |tasks| {
        let world = World::new(tasks);
        let n = world.tasks.len();
        let original: Vec<usize> = (0..n).collect();
        let (first, violations) = world.dispatch(&params, &original);
        prop_assert_eq!(violations, 0, "a pick broke an eligibility condition");
        for seed in 1..=3_u64 {
            let order = shuffled(&original, seed ^ u64::try_from(n).unwrap_or(0));
            let (again, violations) = world.dispatch(&params, &order);
            prop_assert_eq!(violations, 0, "a pick broke an eligibility condition");
            prop_assert_eq!(
                &again,
                &first,
                "the order of the input changed the dispatch"
            );
        }
        if let Ok(mut log) = log.lock() {
            log.push((n, first.len(), violations));
        }
        Ok(())
    });
    assert!(result.is_ok(), "{result:?}");
    let log = log.into_inner().unwrap_or_default();
    assert_eq!(log.len(), 1000);
    assert!(
        log.iter().any(|(n, _, _)| *n == 1000),
        "no case of 1000 tasks"
    );
    assert!(log.iter().all(|(n, _, _)| (10..=1000).contains(n)));
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let cases: Vec<serde_json::Value> = log
            .iter()
            .map(|(n, picks, violations)| {
                serde_json::json!({"tasks": n, "dispatched": picks, "violations": violations})
            })
            .collect();
        let doc = serde_json::json!({
            "test": "TEST-038-02",
            "seed": String::from_utf8_lossy(&seed),
            "rng": "ChaCha",
            "orders_per_case": 4,
            "cases": cases,
        });
        let _ = std::fs::write(
            std::path::Path::new(&dir).join("test-038-02-cases.json"),
            serde_json::to_vec_pretty(&doc).unwrap_or_default(),
        );
    }
}
