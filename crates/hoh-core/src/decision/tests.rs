//! The verdict rules on in-memory snapshots; the database-backed acceptance tests are in persistence.

use super::*;
use crate::protocol::AllVariants;
use RejectionReason::*;
use serde_json::json;

fn caps(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_owned()).collect()
}

fn task(id: &str, state: TaskState) -> TaskView {
    TaskView {
        task_id: id.to_owned(),
        state,
        revision: 4,
        spec_id: "s2".to_owned(),
        role: "developer".to_owned(),
        cost_estimate: 1,
        dependencies: vec![("T0".to_owned(), TaskState::Accepted)],
        requirements: caps(&["REQ-1"]),
    }
}

/// Run at revision 7, epoch 3, RUNNING on spec s2; T1 READY; developer bound with what it needs; budget 1.
fn snapshot() -> Snapshot {
    Snapshot {
        run: RunView {
            state: RunState::Running,
            revision: 7,
            spec_id: Some("s2".to_owned()),
            spec_sha256: Some("b".repeat(64)),
        },
        commander_epoch: Some(3),
        task: Some(task("T1", TaskState::Ready)),
        target: Some(BindingView {
            role_epoch: 5,
            active: true,
            capabilities: caps(&["structured_events", "command_exec"]),
        }),
        required_capabilities: Some(caps(&["structured_events", "command_exec"])),
        budget_headroom: Some(1),
        // C1 is the run's one candidate in use; it has no passing evidence yet.
        evidence: Evidence {
            live: caps(&["C1"]),
            ..Evidence::default()
        },
    }
}

fn proposal(payload: serde_json::Value) -> Proposal {
    Proposal {
        decision_id: "d1".to_owned(),
        run_id: "R".to_owned(),
        role_epoch: 3,
        expected_revision: 7,
        spec_sha256: "b".repeat(64),
        candidate_id: Some("C1".to_owned()),
        payload: serde_json::from_value(payload).expect("payload"),
    }
}

fn dispatch_t1() -> Proposal {
    proposal(json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}))
}

fn reason(p: &Proposal, s: &Snapshot) -> RejectionReason {
    match validate(p, s) {
        Err(r) => r.reason,
        Ok(effect) => panic!("expected a rejection, got {effect:?}"),
    }
}

#[test]
fn a_valid_dispatch_leases_the_task_for_the_bound_epoch() {
    assert_eq!(
        validate(&dispatch_t1(), &snapshot()),
        Ok(Effect::Dispatch {
            task_id: "T1".to_owned(),
            task_revision: 4,
            role: "developer".to_owned(),
            role_epoch: 5,
        })
    );
}

#[test]
fn the_fences_come_first_in_order() {
    // A proposal stale in every way is refused for its epoch, then its revision, then its specification.
    let mut p = dispatch_t1();
    p.role_epoch = 2;
    p.expected_revision = 6;
    p.spec_sha256 = "a".repeat(64);
    let mut s = snapshot();
    s.task = None;
    assert_eq!(reason(&p, &s), StaleRoleEpoch);
    p.role_epoch = 3;
    assert_eq!(reason(&p, &s), StaleRevision);
    p.expected_revision = 7;
    assert_eq!(reason(&p, &s), SpecMismatch);
    // No commander binding: no epoch is current.
    let mut unbound = snapshot();
    unbound.commander_epoch = None;
    assert_eq!(reason(&dispatch_t1(), &unbound), StaleRoleEpoch);
    // A run without a specification yet has nothing to be stale against.
    let mut early = snapshot();
    early.run.spec_sha256 = None;
    assert!(
        validate(
            &proposal(json!({"kind": "record_assumption", "statement": "s", "basis": "b"})),
            &early
        )
        .is_ok()
    );
}

#[test]
fn decisions_apply_to_working_runs_only() {
    for state in [
        RunState::PauseRequested,
        RunState::Paused,
        RunState::WaitingExternal,
        RunState::Blocked,
        RunState::CancelRequested,
        RunState::Incomplete,
        RunState::Completed,
        RunState::Cancelled,
        RunState::Failed,
    ] {
        let mut s = snapshot();
        s.run.state = state;
        assert_eq!(reason(&dispatch_t1(), &s), IllegalTransition, "{state:?}");
    }
}

#[test]
fn a_dispatch_needs_accepted_dependencies_a_ready_task_capabilities_and_budget() {
    let check = |edit: fn(&mut Snapshot), expected: RejectionReason| {
        let mut s = snapshot();
        edit(&mut s);
        assert_eq!(reason(&dispatch_t1(), &s), expected);
    };
    check(|s| s.task = None, IllegalTransition);
    check(
        |s| {
            if let Some(t) = &mut s.task {
                t.spec_id = "s1".to_owned();
            }
        },
        SpecMismatch,
    );
    // A PENDING task whose dependency runs: the dependency is the reason, not the state.
    check(
        |s| {
            if let Some(t) = &mut s.task {
                t.state = TaskState::Pending;
                t.dependencies.push(("T3".to_owned(), TaskState::Running));
            }
        },
        DependencyNotAccepted,
    );
    check(
        |s| {
            if let Some(t) = &mut s.task {
                t.state = TaskState::Leased;
            }
        },
        IllegalTransition,
    );
    check(|s| s.target = None, RoleCapabilityMissing);
    check(
        |s| {
            if let Some(b) = &mut s.target {
                b.active = false;
            }
        },
        RoleCapabilityMissing,
    );
    check(
        |s| {
            s.target = Some(BindingView {
                role_epoch: 1,
                active: true,
                capabilities: caps(&["structured_events"]),
            })
        },
        RoleCapabilityMissing,
    );
    check(|s| s.required_capabilities = None, RoleCapabilityMissing);
    check(
        |s| {
            if let Some(t) = &mut s.task {
                t.cost_estimate = 3;
            }
        },
        BudgetNotReservable,
    );
    // No limit: any cost.
    let mut s = snapshot();
    s.budget_headroom = None;
    if let Some(t) = &mut s.task {
        t.cost_estimate = 5;
    }
    assert!(validate(&dispatch_t1(), &s).is_ok());
}

#[test]
fn the_commander_proposes_only_its_task_moves() {
    for &from in TaskState::ALL {
        for &to in TaskState::ALL {
            let mut s = snapshot();
            if let Some(t) = &mut s.task {
                t.state = from;
            }
            s.evidence
                .candidates
                .insert("C1".to_owned(), caps(&["REQ-1"]));
            let p = proposal(json!({"kind": "transition_task", "task_id": "T1", "to_state": to}));
            let verdict = validate(&p, &s);
            // ADR-011 §3.3, stated here independently of the implementation's table (M2T02R-05).
            use TaskState::*;
            let expected = matches!(
                (from, to),
                (Pending, Ready)
                    | (Pending, Cancelled)
                    | (Ready, Pending)
                    | (Ready, Cancelled)
                    | (Implemented, Ready)
                    | (Implemented, Cancelled)
                    | (Verifying, Accepted)
            );
            assert_eq!(verdict.is_ok(), expected, "{from:?} -> {to:?}: {verdict:?}");
            if !expected {
                assert_eq!(reason(&p, &s), IllegalTransition);
            }
        }
    }
}

#[test]
fn leasing_goes_through_dispatch_only() {
    // READY -> LEASED is in the table, but only a dispatch, with its dependency, capability and budget checks, takes it.
    let lease = proposal(json!({"kind": "transition_task", "task_id": "T1", "to_state": "LEASED"}));
    assert_eq!(reason(&lease, &snapshot()), IllegalTransition);
    assert!(validate(&dispatch_t1(), &snapshot()).is_ok());
}

#[test]
fn a_task_is_accepted_only_on_evidence_and_ready_only_on_accepted_dependencies() {
    let accept =
        proposal(json!({"kind": "transition_task", "task_id": "T1", "to_state": "ACCEPTED"}));
    let mut s = snapshot();
    if let Some(t) = &mut s.task {
        t.state = TaskState::Verifying;
    }
    assert_eq!(reason(&accept, &s), EvidenceGateUnmet);
    // Evidence for another requirement, or a task without requirements, is not enough.
    s.evidence
        .candidates
        .insert("C1".to_owned(), caps(&["REQ-2"]));
    assert_eq!(reason(&accept, &s), EvidenceGateUnmet);
    let mut none = s.clone();
    if let Some(t) = &mut none.task {
        t.requirements.clear();
    }
    assert_eq!(reason(&accept, &none), EvidenceGateUnmet);
    s.evidence
        .candidates
        .insert("C1".to_owned(), caps(&["REQ-1", "REQ-2"]));
    assert!(validate(&accept, &s).is_ok());
    // Only the candidate the proposal names counts, and only as the run's one candidate in use (M2T02R-01).
    let mut unnamed = accept.clone();
    unnamed.candidate_id = None;
    assert_eq!(reason(&unnamed, &s), EvidenceGateUnmet);
    let mut other = accept.clone();
    other.candidate_id = Some("C2".to_owned());
    assert_eq!(reason(&other, &s), CandidateMismatch);
    let mut two = s.clone();
    two.evidence.live.insert("C2".to_owned());
    assert_eq!(reason(&accept, &two), CandidateMismatch);

    let ready = proposal(json!({"kind": "transition_task", "task_id": "T1", "to_state": "READY"}));
    let mut s = snapshot();
    if let Some(t) = &mut s.task {
        t.state = TaskState::Pending;
        t.dependencies.push(("T3".to_owned(), TaskState::Running));
    }
    assert_eq!(reason(&ready, &s), DependencyNotAccepted);
}

#[test]
fn the_commander_cannot_complete_a_run_without_evidence_or_drive_suspension() {
    let complete = |accepted: serde_json::Value| {
        proposal(
            json!({"kind": "transition_run", "to_state": "COMPLETED", "accepted_requirements": accepted}),
        )
    };
    let mut s = snapshot();
    s.run.state = RunState::Verifying;
    s.evidence.required = caps(&["REQ-1", "REQ-2"]);
    assert_eq!(reason(&complete(json!(["REQ-1"])), &s), EvidenceGateUnmet);
    s.evidence
        .candidates
        .insert("C1".to_owned(), caps(&["REQ-1"]));
    assert_eq!(reason(&complete(json!([])), &s), EvidenceGateUnmet);
    // Evidence of another candidate does not count for the named one (M2T02R-01).
    s.evidence
        .candidates
        .insert("C0".to_owned(), caps(&["REQ-1", "REQ-2"]));
    assert_eq!(reason(&complete(json!([])), &s), EvidenceGateUnmet);
    s.evidence
        .candidates
        .insert("C1".to_owned(), caps(&["REQ-1", "REQ-2"]));
    assert_eq!(
        validate(&complete(json!(["REQ-1"])), &s),
        Ok(Effect::TransitionRun {
            from: RunState::Verifying,
            to: RunState::Completed
        })
    );
    // An older candidate that passed is not the current one while a newer one is in use.
    let mut stale = complete(json!([]));
    stale.candidate_id = Some("C0".to_owned());
    assert_eq!(reason(&stale, &s), CandidateMismatch);
    let mut two = s.clone();
    two.evidence.live.insert("C0".to_owned());
    assert_eq!(reason(&complete(json!([])), &two), CandidateMismatch);
    // Claiming an extra requirement needs its evidence too.
    assert_eq!(reason(&complete(json!(["REQ-3"])), &s), EvidenceGateUnmet);
    // A specification without required requirements gives nothing to complete.
    s.evidence.required.clear();
    assert_eq!(reason(&complete(json!([])), &s), EvidenceGateUnmet);
    // Only from VERIFYING (the table), and never into or out of suspension (the Commander's boundary).
    let mut running = snapshot();
    running.evidence.required = caps(&["REQ-1"]);
    running
        .evidence
        .candidates
        .insert("C1".to_owned(), caps(&["REQ-1"]));
    assert_eq!(reason(&complete(json!([])), &running), IllegalTransition);
    for to in [
        RunState::PauseRequested,
        RunState::WaitingExternal,
        RunState::Blocked,
        RunState::CancelRequested,
    ] {
        let p = proposal(json!({"kind": "transition_run", "to_state": to}));
        assert_eq!(reason(&p, &running), IllegalTransition, "{to:?}");
    }
    let verify = proposal(json!({"kind": "transition_run", "to_state": "VERIFYING"}));
    assert!(validate(&verify, &running).is_ok());
    let claim = proposal(
        json!({"kind": "transition_run", "to_state": "VERIFYING", "accepted_requirements": ["REQ-9"]}),
    );
    assert_eq!(reason(&claim, &running), EvidenceGateUnmet);
    for to in [RunState::Incomplete, RunState::Failed] {
        let p = proposal(json!({"kind": "transition_run", "to_state": to}));
        assert!(validate(&p, &running).is_ok(), "{to:?}");
    }
}

#[test]
fn kinds_applied_by_later_tasks_are_reported_not_judged() {
    let ask = proposal(
        json!({"kind": "request_user_decision", "question_id": "q", "question": "?", "requires_user": true}),
    );
    assert!(!ask.applicable());
    assert_eq!(
        validate(&ask, &snapshot()),
        Ok(Effect::Unsupported("request_user_decision"))
    );
    let plan = proposal(json!({"kind": "create_tasks", "plan": {"tasks": [{
        "task_id": "T9", "title": "t", "role": "developer", "writer": true, "depends_on": [], "requirement_ids": []}]}}));
    assert!(!plan.applicable());
    assert!(dispatch_t1().applicable());
}

fn message(payload: serde_json::Value) -> Message {
    serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": "m1", "run_id": "R", "task_id": "T1", "attempt_id": "A1",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 3, "correlation_id": "c1",
        "causation_id": null, "idempotency_key": "k1", "expected_revision": 7, "spec_sha256": "b".repeat(64),
        "candidate_id": "C1", "kind": "decision", "payload": payload
    }))
    .expect("message")
}

#[test]
fn a_proposal_is_read_from_a_commander_decision_message_with_a_schema_valid_payload() {
    let p = Proposal::from_message(&message(
        json!({"kind": "dispatch", "task_id": "T1", "role": "developer"}),
    ))
    .expect("proposal");
    assert_eq!(
        (
            p.decision_id.as_str(),
            p.run_id.as_str(),
            p.role_epoch,
            p.expected_revision
        ),
        ("m1", "R", 3, 7)
    );
    assert_eq!(p.candidate_id.as_deref(), Some("C1"));
    assert_eq!(
        (p.kind(), p.task_id(), p.target_role()),
        ("dispatch", Some("T1"), Some("developer"))
    );
    for bad in [
        json!({"kind": "dispatch", "task_id": "", "role": "developer"}),
        json!({"kind": "dispatch", "task_id": "T1"}),
        json!({"kind": "dispatch", "task_id": "T1", "role": "developer", "extra": 1}),
        json!({"kind": "transition_task", "task_id": "T1", "to_state": "DONE"}),
        json!({"kind": "launch"}),
    ] {
        assert!(
            matches!(
                Proposal::from_message(&message(bad.clone())),
                Err(ProposalError::Payload(DecodeError::Schema(_)))
            ),
            "{bad}"
        );
    }
    let mut m = message(json!({"kind": "record_assumption", "statement": "s", "basis": "b"}));
    m.kind = MessageKind::Steer;
    assert!(matches!(
        Proposal::from_message(&m),
        Err(ProposalError::NotADecision(MessageKind::Steer))
    ));
    m.kind = MessageKind::Decision;
    m.sender_role = "developer".to_owned();
    assert!(matches!(
        Proposal::from_message(&m),
        Err(ProposalError::NotFromCommander(_))
    ));
}
