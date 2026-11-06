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
