//! Net progress across verification rounds (REQ-040; ADR-037).
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub const WINDOW: usize = 3;

/// Only these five components represent net progress; commits are deliberately absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vector {
    pub accepted: u64,
    pub closed_issues: u64,
    pub reopened_issues: u64,
    pub new_critical_high: u64,
    pub decision_risks: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Round {
    pub seq: i64,
    pub spec_revision: u64,
    pub vector: Vector,
    pub open_critical_high: u64,
    pub open_issues: u64,
    pub fingerprints: BTreeSet<String>,
    pub spec_added_requirement_ids: BTreeSet<String>,
    pub release_stable: bool,
    /// In memory only. The persisted round is a fact vector; a commit count is not one of its components
    /// (REQ-040). Old events that still carry the field deserialize; new events omit it and read as 0 (M3K-06).
    #[serde(default, skip_serializing)]
    pub commit_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    rounds: Vec<Round>,
    previous_open: Option<(u64, u64)>,
    pub last_progress_seq: i64,
    reported: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    pub progress: bool,
    pub reset: Option<Value>,
    pub no_progress: Option<Value>,
}

impl Window {
    pub fn observe(&mut self, round: Round) -> Verdict {
        let stable_improvement = round.release_stable
            && self
                .previous_open
                .is_some_and(|(_, total)| round.open_issues < total);
        let severity_fell = self
            .previous_open
            .is_some_and(|(high, _)| round.open_critical_high < high);
        self.previous_open = Some((round.open_critical_high, round.open_issues));
        let progress = round.vector.accepted > 0
            || round.vector.closed_issues > 0
            || !round.spec_added_requirement_ids.is_empty()
            || stable_improvement;
        if progress {
            self.rounds.clear();
            self.reported = false;
            self.last_progress_seq = round.seq;
            return Verdict {
                progress: true,
                reset: (!round.spec_added_requirement_ids.is_empty()).then(|| {
                    json!({
                        "reason": "规格新增 REQ", "spec_revision": round.spec_revision,
                    })
                }),
                no_progress: None,
            };
        }
        // A severity decrease breaks the consecutive non-decreasing sequence, even without a closed issue.
        if severity_fell {
            self.rounds.clear();
            self.reported = false;
        }
        self.rounds.push(round);
        if self.rounds.len() > WINDOW {
            self.rounds.remove(0);
        }
        if self.reported || self.rounds.len() < WINDOW {
            return Verdict::default();
        }
        let mut repeated = self.rounds[0].fingerprints.clone();
        let mut vector = Vector::default();
        for round in &self.rounds {
            repeated.retain(|f| round.fingerprints.contains(f));
            vector.accepted += round.vector.accepted;
            vector.closed_issues += round.vector.closed_issues;
            vector.reopened_issues += round.vector.reopened_issues;
            vector.new_critical_high += round.vector.new_critical_high;
            vector.decision_risks = round.vector.decision_risks;
        }
        if repeated.is_empty() {
            return Verdict::default();
        }
        self.reported = true;
        Verdict {
            no_progress: Some(json!({"window": WINDOW, "fingerprints": repeated,
                "fingerprint": repeated.into_iter().collect::<Vec<_>>().join("\n"),
                "vector": vector, "last_progress_seq": self.last_progress_seq,
                "suggestions": ["升级", "研究", "改计划"]})),
            ..Verdict::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round(seq: i64) -> Round {
        Round {
            seq,
            fingerprints: ["F".into()].into(),
            open_critical_high: 1,
            open_issues: 5,
            spec_revision: 1,
            ..Round::default()
        }
    }

    #[test]
    fn test_040_02_docs_only_rounds_are_not_progress() {
        let mut answers = Vec::new();
        for commits in [[1, 2, 3], [100, 200, 900]] {
            let mut window = Window::default();
            for (i, count) in commits.into_iter().enumerate() {
                let mut fact = round(i as i64 + 1);
                fact.commit_count = count;
                let answer = window.observe(fact);
                if i < 2 {
                    assert!(answer.no_progress.is_none());
                } else {
                    answers.push(
                        answer
                            .no_progress
                            .expect("three docs-only rounds must diagnose no progress"),
                    );
                }
            }
            assert!(
                window.observe(round(4)).no_progress.is_none(),
                "only once per window"
            );
        }
        assert_eq!(answers[0], answers[1]);
        assert_eq!(answers[0]["fingerprints"], json!(["F"]));
        assert_eq!(answers[0]["window"], 3);
        assert_eq!(
            answers[0]["vector"],
            json!({"accepted":0,"closed_issues":0,"reopened_issues":0,"new_critical_high":0,"decision_risks":0})
        );
        assert!(answers[0]["vector"].get("commit_count").is_none());
    }

    #[test]
    fn test_040_02_persisted_round_omits_commit_count() {
        // M3K-06: progress.observed.round is serialized from Round. commit_count is not a fact.
        let mut fact = round(1);
        fact.commit_count = 7;
        let value = serde_json::to_value(&fact).expect("round");
        assert!(
            value.get("commit_count").is_none(),
            "serialized round must omit commit_count: {value}"
        );
        let mut old = serde_json::to_value(round(3)).expect("round");
        old["commit_count"] = json!(4);
        let restored: Round =
            serde_json::from_value(old).expect("old events still carry the field");
        assert_eq!((restored.seq, restored.commit_count), (3, 4));
        let fresh = serde_json::to_value(round(2)).expect("round");
        let omitted: Round = serde_json::from_value(fresh).expect("new events omit the field");
        assert_eq!(omitted.commit_count, 0);
    }

    #[test]
    fn test_040_03_window_resets_and_spec_growth_is_named() {
        for reset in 0..4 {
            let mut window = Window::default();
            window.observe(round(1));
            let mut second = round(2);
            match reset {
                0 => second.vector.closed_issues = 1,
                1 => second.vector.accepted = 1,
                2 => {
                    second.spec_added_requirement_ids.insert("REQ-NEW".into());
                    second.spec_revision = 2;
                }
                _ => {
                    second.release_stable = true;
                    second.open_issues = 3;
                }
            }
            let verdict = window.observe(second);
            assert!(verdict.progress, "reset sequence {reset}");
            assert_eq!(window.last_progress_seq, 2);
            if reset == 2 {
                assert_eq!(
                    verdict.reset,
                    Some(json!({"reason":"规格新增 REQ", "spec_revision":2}))
                );
            } else {
                assert!(verdict.reset.is_none());
            }
            assert!(window.observe(round(3)).no_progress.is_none());
        }
    }

    #[test]
    fn test_040_02_different_failures_or_falling_severity_do_not_diagnose_stall() {
        for different in [true, false] {
            let mut window = Window::default();
            for i in 1..=3 {
                let mut fact = round(i);
                if different {
                    fact.fingerprints = [format!("F{i}")].into();
                } else {
                    fact.open_critical_high = (3 - i) as u64;
                }
                assert!(window.observe(fact).no_progress.is_none());
            }
        }
        let mut window = Window::default();
        for i in 1..=3 {
            let mut fact = round(i);
            fact.fingerprints.clear();
            assert!(window.observe(fact).no_progress.is_none());
        }
    }

    #[test]
    fn test_040_03_reopens_and_risks_do_not_reset_and_window_survives_replay() {
        let mut window = Window::default();
        window.observe(round(1));
        let mut second = round(2);
        second.vector.reopened_issues = 1;
        second.vector.decision_risks = 2;
        assert!(!window.observe(second).progress);
        let mut recovered: Window =
            serde_json::from_value(serde_json::to_value(&window).unwrap()).unwrap();
        let answer = recovered.observe(round(3));
        assert!(answer.no_progress.is_some());
        assert_eq!(answer.no_progress.unwrap()["vector"]["reopened_issues"], 1);
        assert!(recovered.observe(round(4)).no_progress.is_none());
        let mut progress = round(5);
        progress.vector.accepted = 1;
        assert!(recovered.observe(progress).progress);
        for i in 6..8 {
            assert!(recovered.observe(round(i)).no_progress.is_none());
        }
        assert_eq!(
            recovered.observe(round(8)).no_progress.unwrap()["last_progress_seq"],
            5
        );
    }
}
