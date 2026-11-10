//! Verification handoffs (REQ-022 b7; docs/adr/ADR-018.md decision 6): once the Developer's candidate is ready, one
//! `kind=handoff` Message per verifying role, each carrying a ContextHandoff with the same full candidate identity and
//! specification and the role's own current epoch; and the role-change package a new Commander takes over from
//! (REQ-019 b4). Pure: the store gathers the inputs and queues the messages.

use crate::protocol::{Candidate, ContextHandoff, HandoffKind, HandoffState, Message, MessageKind};
use serde_json::Value;

/// The context budget of a verification package until context packing sets it (M2-T09).
pub const BUDGET_TOKENS: u64 = 32_000;

/// What every verification handoff of one candidate shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    pub run_id: String,
    pub task_id: String,
    /// The Developer's attempt that produced the candidate.
    pub attempt_id: String,
    pub candidate: Candidate,
    pub spec_sha256: String,
    pub spec_revision: u64,
    /// The task's acceptance criteria under the current specification, sorted; never empty.
    pub acceptance_ids: Vec<String>,
    pub goal: String,
    /// The task's revision once it is VERIFYING: the revision both messages expect.
    pub task_revision: u64,
    /// The `candidate_ready` event the handoffs follow.
    pub cause: String,
}

/// A role that receives a handoff, at its current epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    pub role: String,
    pub role_epoch: u64,
}

/// What each role hands back: QA a TaskResult, the reviewer an independent report.
pub fn output_contract(role: &str) -> &'static str {
    if role == "reviewer" {
        "IndependentReport"
    } else {
        "TaskResult"
    }
}

/// One handoff Message per recipient, in the order given; `None` when there is nothing to verify against (no
/// acceptance criteria), so no handoff is made at all.
pub fn verification(v: &Verification, to: &[Recipient]) -> Option<Vec<Message>> {
    if v.acceptance_ids.is_empty() {
        return None;
    }
    to.iter()
        .map(|r| {
            let package = ContextHandoff {
                schema_version: "1.0".to_owned(),
                run_id: v.run_id.clone(),
                task_id: Some(v.task_id.clone()),
                goal: v.goal.clone(),
                role: r.role.clone(),
                role_epoch: r.role_epoch,
                spec_sha256: v.spec_sha256.clone(),
                candidate: Some(v.candidate.clone()),
                acceptance_ids: v.acceptance_ids.clone(),
                references: Vec::new(),
                output_contract: output_contract(&r.role).to_owned(),
                budget_tokens: BUDGET_TOKENS,
                handoff_kind: HandoffKind::Verification,
                spec_revision: v.spec_revision,
                state: None,
            };
            let Ok(Value::Object(payload)) = serde_json::to_value(&package) else {
                return None;
            };
            // Bounded however long the candidate id is (M2T04R-55, -153).
            let id = crate::ids::child_id(&v.candidate.id, &format!("verify/{}", r.role));
            Some(Message {
                schema_version: "1.0".to_owned(),
                message_id: id.clone(),
                run_id: v.run_id.clone(),
                task_id: v.task_id.clone(),
                attempt_id: v.attempt_id.clone(),
                sender_role: "runtime".to_owned(),
                recipient_role: r.role.clone(),
                role_epoch: r.role_epoch,
                correlation_id: v.candidate.id.clone(),
                causation_id: Some(v.cause.clone()),
                idempotency_key: id,
                expected_revision: v.task_revision,
                spec_sha256: v.spec_sha256.clone(),
                candidate_id: Some(v.candidate.id.clone()),
                kind: MessageKind::Handoff,
                payload,
                reason_code: None,
            })
        })
        .collect()
}

/// What the new Commander's checkpoint package carries (REQ-019 b4): the run's goal, its current specification and
/// candidate, and its state by row ids, gathered by the store in the transaction that hands the role over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleChange {
    pub run_id: String,
    pub goal: String,
    /// The Commander's epoch once the change is applied.
    pub role_epoch: u64,
    pub spec_sha256: String,
    pub spec_revision: u64,
    pub candidate: Option<Candidate>,
    pub state: HandoffState,
}

/// The new Commander's first input: a `role_change` ContextHandoff naming rows, each id list sorted and each id once,
/// never a transcript of the old session (REQ-019 b4). No task of its own; it hands back decisions.
pub fn role_change(c: &RoleChange) -> ContextHandoff {
    let ids = |v: &[String]| {
        let mut v = v.to_vec();
        v.sort();
        v.dedup();
        v
    };
    ContextHandoff {
        schema_version: "1.0".to_owned(),
        run_id: c.run_id.clone(),
        task_id: None,
        goal: c.goal.clone(),
        role: "commander".to_owned(),
        role_epoch: c.role_epoch,
        spec_sha256: c.spec_sha256.clone(),
        candidate: c.candidate.clone(),
        acceptance_ids: Vec::new(),
        references: Vec::new(),
        output_contract: "DecisionPayload".to_owned(),
        budget_tokens: BUDGET_TOKENS,
        handoff_kind: HandoffKind::RoleChange,
        spec_revision: c.spec_revision,
        state: Some(HandoffState {
            applied_decision_ids: ids(&c.state.applied_decision_ids),
            open_issue_ids: ids(&c.state.open_issue_ids),
            unacked_message_ids: ids(&c.state.unacked_message_ids),
            queued_input_ids: ids(&c.state.queued_input_ids),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol;

    fn sha(n: u32) -> String {
        format!("{n:064}")
    }

    fn inputs() -> Verification {
        Verification {
            run_id: "R".to_owned(),
            task_id: "T".to_owned(),
            attempt_id: "A".to_owned(),
            candidate: Candidate {
                id: "C2".to_owned(),
                git_commit: "a".repeat(40),
                git_tree: "b".repeat(40),
                spec_sha256: sha(2),
                environment_sha256: sha(3),
                test_plan_sha256: sha(4),
            },
            spec_sha256: sha(2),
            spec_revision: 2,
            acceptance_ids: vec!["AC-1".to_owned(), "AC-2".to_owned()],
            goal: "g".to_owned(),
            task_revision: 5,
            cause: "e".to_owned(),
        }
    }

    #[test]
    fn each_role_gets_a_valid_package_with_the_same_candidate_and_its_own_epoch() {
        let to = [
            Recipient {
                role: "qa".to_owned(),
                role_epoch: 2,
            },
            Recipient {
                role: "reviewer".to_owned(),
                role_epoch: 7,
            },
        ];
        let messages = verification(&inputs(), &to).unwrap_or_default();
        assert_eq!(messages.len(), 2);
        for (m, r) in messages.iter().zip(&to) {
            let encoded = protocol::encode(m).expect("a valid Message");
            let back: Message = protocol::decode(&encoded).expect("decodes");
            let package: ContextHandoff = protocol::decode(&Value::Object(back.payload.clone()))
                .expect("a valid ContextHandoff");
            assert_eq!(package.role, r.role);
            assert_eq!(
                (m.role_epoch, package.role_epoch),
                (r.role_epoch, r.role_epoch)
            );
            assert_eq!(package.candidate, Some(inputs().candidate));
            assert_eq!(package.output_contract, output_contract(&r.role));
            assert_eq!(m.expected_revision, 5);
        }
        assert_ne!(messages[0].message_id, messages[1].message_id);
    }

    fn change() -> RoleChange {
        RoleChange {
            run_id: "R".to_owned(),
            goal: "g".to_owned(),
            role_epoch: 2,
            spec_sha256: sha(3),
            spec_revision: 3,
            candidate: Some(inputs().candidate),
            state: HandoffState {
                applied_decision_ids: vec!["D2".to_owned(), "D1".to_owned(), "D2".to_owned()],
                open_issue_ids: vec!["I2".to_owned(), "I1".to_owned()],
                unacked_message_ids: vec!["M1".to_owned()],
                queued_input_ids: vec!["Q1".to_owned(), "Q1".to_owned()],
            },
        }
    }

    #[test]
    fn a_role_change_package_is_schema_valid_and_carries_ids_only() {
        // REQ-019 b4: the new Commander's package names the run's state by row ids, sorted and once each, never a
        // transcript; valid against protocol.schema.json with a candidate and without one.
        for candidate in [Some(inputs().candidate), None] {
            let mut c = change();
            c.candidate = candidate.clone();
            let package = role_change(&c);
            let encoded = protocol::encode(&package).expect("a valid ContextHandoff");
            let back: ContextHandoff = protocol::decode(&encoded).expect("decodes");
            assert_eq!(back, package);
            assert_eq!(package.handoff_kind, HandoffKind::RoleChange);
            assert_eq!(
                (
                    package.role.as_str(),
                    package.role_epoch,
                    package.task_id.as_deref()
                ),
                ("commander", 2, None)
            );
            assert_eq!(
                (package.spec_sha256.as_str(), package.spec_revision),
                (sha(3).as_str(), 3)
            );
            assert_eq!(package.candidate, candidate);
            assert_eq!(package.output_contract, "DecisionPayload");
            assert_eq!(package.goal, "g");
            assert_eq!(
                package.state,
                Some(HandoffState {
                    applied_decision_ids: vec!["D1".to_owned(), "D2".to_owned()],
                    open_issue_ids: vec!["I1".to_owned(), "I2".to_owned()],
                    unacked_message_ids: vec!["M1".to_owned()],
                    queued_input_ids: vec!["Q1".to_owned()],
                })
            );
            assert!(package.acceptance_ids.is_empty() && package.references.is_empty());
        }
    }

    #[test]
    fn no_acceptance_criteria_no_handoff() {
        let mut v = inputs();
        v.acceptance_ids.clear();
        assert_eq!(
            verification(
                &v,
                &[Recipient {
                    role: "qa".to_owned(),
                    role_epoch: 1
                }]
            ),
            None
        );
    }
}
