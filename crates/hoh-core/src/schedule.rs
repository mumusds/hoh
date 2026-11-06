//! Task ranking and selection (REQ-038; docs/07-algorithms.md; tools/reference_model.py `choose`).
//!
//! Pure: the caller gives the candidates with their stored inputs (layer, the four scores, cost, waited rounds, the
//! first round seq at which each became eligible) and the five eligibility facts; nothing here reads history. A round
//! ranks the eligible candidates, selects the first whose slot is free, and names the eligible candidates that waited.
//! Ordering never depends on the input order: every key ends in `created_seq` and `task_id`, and scores are compared
//! as exact fractions.

use crate::protocol::SchedulerSettings;
use std::cmp::Ordering;

/// The age bonus is capped (REQ-038: 0..=10).
pub const AGE_BONUS_CAP: u32 = 10;
/// A missing cost estimate is the median bucket of 1..=5 (REQ-038).
pub const DEFAULT_COST: u8 = 3;

/// The four score weights of REQ-038.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Weights {
    pub user_value: u32,
    pub unblock_value: u32,
    pub risk_reduction: u32,
    pub information_gain: u32,
}

/// The scheduling parameters of one run (CR-007: the run config snapshot's `scheduler`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub weights: Weights,
    /// Eligible rounds without selection after which a task of layer 1 or 2 enters the aged sub-layer.
    pub max_wait_rounds: u32,
}

impl Default for Params {
    /// docs/07 and CR-002: weights 4/3/3/2, max_wait_rounds 20.
    fn default() -> Params {
        Params {
            weights: Weights {
                user_value: 4,
                unblock_value: 3,
                risk_reduction: 3,
                information_gain: 2,
            },
            max_wait_rounds: 20,
        }
    }
}

impl Params {
    /// The parameters a snapshot records; what it leaves out is the default (CR-007).
    pub fn from_settings(settings: Option<&SchedulerSettings>) -> Params {
        let mut params = Params::default();
        let Some(settings) = settings else {
            return params;
        };
        if let Some(weights) = &settings.weights {
            let w = &mut params.weights;
            for (value, slot) in [
                (weights.user_value, &mut w.user_value),
                (weights.unblock_value, &mut w.unblock_value),
                (weights.risk_reduction, &mut w.risk_reduction),
                (weights.information_gain, &mut w.information_gain),
            ] {
                if let Some(value) = value {
                    *slot = u32::from(value);
                }
            }
        }
        if let Some(rounds) = settings.max_wait_rounds {
            params.max_wait_rounds = rounds;
        }
        params
    }
}

/// The five eligibility facts of REQ-038, established by the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Eligibility {
    pub dependencies_accepted: bool,
    pub current_spec: bool,
    pub not_leased: bool,
    pub capable: bool,
    pub budget_reservable: bool,
}

impl Eligibility {
    /// Every fact holds.
    pub const ALL: Eligibility = Eligibility {
        dependencies_accepted: true,
        current_spec: true,
        not_leased: true,
        capable: true,
        budget_reservable: true,
    };

    pub fn eligible(&self) -> bool {
        self.dependencies_accepted
            && self.current_spec
            && self.not_leased
            && self.capable
            && self.budget_reservable
    }
}

/// One task as the scheduler sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub task_id: String,
    pub created_seq: i64,
    /// 0 critical regression or data integrity, 1 unblocking, 2 normal.
    pub layer: u8,
    pub writer: bool,
    pub user_value: u8,
    pub unblock_value: u8,
    pub risk_reduction: u8,
    pub information_gain: u8,
    pub cost_estimate: Option<u8>,
    /// Rounds it was eligible and not selected.
    pub wait_rounds: u32,
    /// The seq of the round in which it first became eligible (the aged sub-layer's FIFO key).
    pub first_eligible_seq: Option<i64>,
    pub eligibility: Eligibility,
}

/// A score as the exact fraction `numerator / denominator`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Score {
    pub numerator: u64,
    pub denominator: u64,
}

impl Score {
    pub fn value(&self) -> f64 {
        // Both are far below 2^53: exact as f64 before the division.
        let numerator = u32::try_from(self.numerator).map_or(f64::MAX, f64::from);
        let denominator = u32::try_from(self.denominator).map_or(f64::MAX, f64::from);
        numerator / denominator
    }

    /// Higher first.
    fn descending(&self, other: &Score) -> Ordering {
        let mine = u128::from(self.numerator) * u128::from(other.denominator);
        let theirs = u128::from(other.numerator) * u128::from(self.denominator);
        theirs.cmp(&mine)
    }
}

/// Where a task ranks: layer 0, the aged sub-layer, layer 1, layer 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    Critical,
    Aged,
    Unblock,
    Normal,
}

impl Tier {
    /// The sub-layer mark recorded in round events (`aged` for the aged sub-layer).
    pub fn label(self) -> &'static str {
        match self {
            Tier::Critical => "critical",
            Tier::Aged => "aged",
            Tier::Unblock => "unblock",
            Tier::Normal => "normal",
        }
    }
}

/// A ranked candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ranked {
    pub task_id: String,
    pub tier: Tier,
    pub score: Score,
    pub age_bonus: u32,
    pub writer: bool,
}

/// The score of `task` under `params` (REQ-038: `(Σ w·v + age_bonus) / max(cost, 1)`).
pub fn score(params: &Params, task: &Candidate) -> Score {
    let w = &params.weights;
    let age_bonus = task.wait_rounds.min(AGE_BONUS_CAP);
    let numerator = u64::from(w.user_value) * u64::from(task.user_value)
        + u64::from(w.unblock_value) * u64::from(task.unblock_value)
        + u64::from(w.risk_reduction) * u64::from(task.risk_reduction)
        + u64::from(w.information_gain) * u64::from(task.information_gain)
        + u64::from(age_bonus);
    let cost = task.cost_estimate.unwrap_or(DEFAULT_COST).max(1);
    Score {
        numerator,
        denominator: u64::from(cost),
    }
}

fn tier(params: &Params, task: &Candidate) -> Tier {
    match task.layer {
        0 => Tier::Critical,
        _ if task.wait_rounds >= params.max_wait_rounds => Tier::Aged,
        1 => Tier::Unblock,
        _ => Tier::Normal,
    }
}

/// What a candidate is ordered by, borrowed (no allocation per comparison).
struct Key<'a> {
    tier: Tier,
    score: Score,
    first_eligible_seq: i64,
    created_seq: i64,
    task_id: &'a str,
}

fn key<'a>(params: &Params, task: &'a Candidate) -> Key<'a> {
    Key {
        tier: tier(params, task),
        score: score(params, task),
        first_eligible_seq: task.first_eligible_seq.unwrap_or(i64::MAX),
        created_seq: task.created_seq,
        task_id: &task.task_id,
    }
}

/// The ranking order: tier; then FIFO by first eligibility in the aged sub-layer and score elsewhere; then
/// created_seq, task_id.
fn order(a: &Key<'_>, b: &Key<'_>) -> Ordering {
    a.tier
        .cmp(&b.tier)
        .then_with(|| {
            if a.tier == Tier::Aged {
                a.first_eligible_seq.cmp(&b.first_eligible_seq)
            } else {
                a.score.descending(&b.score)
            }
        })
        .then_with(|| a.created_seq.cmp(&b.created_seq))
        .then_with(|| a.task_id.cmp(b.task_id))
}

fn ranked(params: &Params, task: &Candidate) -> Ranked {
    Ranked {
        task_id: task.task_id.clone(),
        tier: tier(params, task),
        score: score(params, task),
        age_bonus: task.wait_rounds.min(AGE_BONUS_CAP),
        writer: task.writer,
    }
}

/// The eligible candidates, best first.
pub fn rank(params: &Params, candidates: &[Candidate]) -> Vec<Ranked> {
    let mut eligible: Vec<(Key<'_>, &Candidate)> = candidates
        .iter()
        .filter(|c| c.eligibility.eligible())
        .map(|c| (key(params, c), c))
        .collect();
    eligible.sort_by(|a, b| order(&a.0, &b.0));
    eligible.iter().map(|(_, c)| ranked(params, c)).collect()
}

/// Which slots are free for this round.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Slots {
    pub writer: bool,
    pub readers: u32,
}

/// The outcome of one round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Round {
    pub selected: Option<Ranked>,
    /// The positions (in the given candidates) of the eligible candidates not selected, in that order.
    pub waited: Vec<usize>,
}

/// One round: the best eligible candidate whose slot is free is selected; every other eligible one waited. Linear in
/// the candidates (no sort), so a large ready set stays cheap.
pub fn round(params: &Params, candidates: &[Candidate], slots: Slots) -> Round {
    let mut best: Option<(usize, Key<'_>)> = None;
    for (i, task) in candidates.iter().enumerate() {
        let free = if task.writer {
            slots.writer
        } else {
            slots.readers > 0
        };
        if !task.eligibility.eligible() || !free {
            continue;
        }
        let candidate = key(params, task);
        if best
            .as_ref()
            .is_none_or(|(_, b)| order(&candidate, b) == Ordering::Less)
        {
            best = Some((i, candidate));
        }
    }
    let chosen = best.map(|(i, _)| i);
    let waited = candidates
        .iter()
        .enumerate()
        .filter(|(i, c)| c.eligibility.eligible() && Some(*i) != chosen)
        .map(|(i, _)| i)
        .collect();
    Round {
        selected: chosen
            .and_then(|i| candidates.get(i))
            .map(|c| ranked(params, c)),
        waited,
    }
}

#[cfg(test)]
mod tests;
