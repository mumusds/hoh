//! TEST-036-01 (with the SQL enumeration half in persistence::tests): every ordered pair of the three machines is
//! judged by [`check`] exactly as an independent statement of ADR-011's rules says, and the pairs the acceptance
//! criterion names are all refused.

use super::*;
use RunState as Run;
use TaskState as Task;

/// ADR-011 §1, Run, restated from the rules rather than copied from the table.
fn run_rule(from: Run, to: Run) -> bool {
    use Run::*;
    let hold = [
        PauseRequested,
        WaitingExternal,
        Blocked,
        CancelRequested,
        Incomplete,
        Failed,
    ];
    let main = [
        (Created, Discovering),
        (Discovering, Specifying),
        (Specifying, Ready),
        (Ready, Running),
        (Running, Verifying),
        (Verifying, Running),
        (Verifying, Completed),
    ];
    if from == to {
        return false;
    }
    if main.contains(&(from, to)) {
        return true;
    }
    match from {
        f if WORKING.contains(&f) => hold.contains(&to),
        WaitingExternal => {
            WORKING.contains(&to)
                || [Blocked, PauseRequested, CancelRequested, Incomplete, Failed].contains(&to)
        }
        Blocked => {
            WORKING.contains(&to)
                || [PauseRequested, CancelRequested, Incomplete, Failed].contains(&to)
        }
        PauseRequested => [Paused, CancelRequested, Failed].contains(&to),
        Paused => {
            WORKING.contains(&to) || [Blocked, CancelRequested, Incomplete, Failed].contains(&to)
        }
        CancelRequested => [Cancelled, Failed].contains(&to),
        Incomplete => WORKING.contains(&to) || [CancelRequested, Failed].contains(&to),
        _ => false,
    }
}

/// ADR-011 §1, Task.
fn task_rule(from: Task, to: Task) -> bool {
    use Task::*;
    let forward = [
        (Pending, Ready),
        (Ready, Leased),
        (Leased, Running),
        (Running, Implemented),
        (Implemented, Verifying),
        (Verifying, Accepted),
        (Ready, Pending),
        (Blocked, Pending),
    ];
    if forward.contains(&(from, to)) {
        return true;
    }
    match from {
        Accepted | Cancelled => false,
        _ if from == to => false,
        // Back to READY after a released lease, a failed attempt, rework or a failed verification, or unblocked.
        Leased | Running | Implemented | Verifying | Blocked if to == Ready => true,
        Blocked => to == Cancelled,
        _ => matches!(to, Blocked | Cancelled),
    }
}

/// ADR-011 §1, Attempt.
fn attempt_rule(from: AttemptState, to: AttemptState) -> bool {
    use AttemptState::*;
    match from {
        Prepared => matches!(to, Starting | Failed | Cancelled),
        Starting => matches!(to, Running | Failed | Cancelled | UnknownEffect),
        Running => matches!(to, Settled | Failed | Cancelled | UnknownEffect),
        UnknownEffect => matches!(to, Settled | Failed | Cancelled),
        Settled | Failed | Cancelled => false,
    }
}

/// The pairs TEST-036-01 lists as refused.
fn must_reject_run(from: Run, to: Run) -> bool {
    terminal_by_spec(from)
        || (to == Run::Completed && from != Run::Verifying)
        || (to == Run::Paused && from != Run::PauseRequested)
        || (to == Run::Cancelled && from != Run::CancelRequested)
        || (from, to) == (Run::Created, Run::Running)
}

fn must_reject_task(from: Task, to: Task) -> bool {
    (to == Task::Accepted && from != Task::Verifying) || (from, to) == (Task::Ready, Task::Running)
}

fn must_reject_attempt(from: AttemptState, _to: AttemptState) -> bool {
    matches!(
        from,
        AttemptState::Settled | AttemptState::Failed | AttemptState::Cancelled
    )
}

/// REQ-036: the run's terminal states are COMPLETED, CANCELLED and FAILED.
fn terminal_by_spec(state: Run) -> bool {
    matches!(state, Run::Completed | Run::Cancelled | Run::Failed)
}

/// Judge every ordered pair of `S`; returns (pairs, legal pairs, rows of the result table).
fn judge<S: Lifecycle>(
    rule: fn(S, S) -> bool,
    must_reject: fn(S, S) -> bool,
) -> (usize, usize, Vec<String>) {
    let mut legal = 0;
    let mut rows = Vec::new();
    for &from in S::ALL {
        for &to in S::ALL {
            let verdict = check(from, to);
            assert_eq!(
                verdict.is_ok(),
                rule(from, to),
                "{} {} -> {}: the table and ADR-011 disagree",
                S::MACHINE,
                from.as_str(),
                to.as_str()
            );
            if must_reject(from, to) {
                let Err(e) = verdict else {
                    panic!(
                        "{} {} -> {} must be refused",
                        S::MACHINE,
                        from.as_str(),
                        to.as_str()
                    );
                };
                assert_eq!(e.reason(), RejectionReason::IllegalTransition);
            }
            legal += usize::from(verdict.is_ok());
            rows.push(format!(
                "{}\t{}\t{}\t{}",
                S::MACHINE,
                from.as_str(),
                to.as_str(),
                if verdict.is_ok() {
                    "ALLOWED"
                } else {
                    "ILLEGAL_TRANSITION"
                }
            ));
        }
    }
    (S::ALL.len() * S::ALL.len(), legal, rows)
}

#[test]
fn test_036_01_every_ordered_pair_is_judged_as_the_table_says() {
    let (run_pairs, run_legal, mut rows) = judge(run_rule, must_reject_run);
    let (task_pairs, task_legal, task_rows) = judge(task_rule, must_reject_task);
    let (attempt_pairs, attempt_legal, attempt_rows) = judge(attempt_rule, must_reject_attempt);
    assert_eq!(
        (run_pairs, task_pairs, attempt_pairs),
        (225, 81, 49),
        "15, 9 and 7 states"
    );
    assert_eq!(run_pairs + task_pairs + attempt_pairs, 355);
    assert_eq!(
        (run_legal, task_legal, attempt_legal),
        (87, 26, 14),
        "legal edges per ADR-011"
    );
    rows.extend(task_rows);
    rows.extend(attempt_rows);
    assert_eq!(rows.len(), 355);
    // The result table is an observable of TEST-036-01: kept with the check's evidence when run under run_check.
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let path = std::path::Path::new(&dir).join("transition-pairs.tsv");
        let table = format!("machine\tfrom\tto\tverdict\n{}\n", rows.join("\n"));
        std::fs::write(&path, table).expect("write the pair table");
    }
}

#[test]
fn terminal_states_are_exactly_those_without_successors() {
    for &s in Run::ALL {
        assert_eq!(s.is_terminal(), terminal_by_spec(s), "{s:?}");
    }
    let task: Vec<_> = Task::ALL.iter().filter(|s| s.is_terminal()).collect();
    assert_eq!(task, [&Task::Accepted, &Task::Cancelled]);
    let attempt: Vec<_> = AttemptState::ALL
        .iter()
        .filter(|s| s.is_terminal())
        .collect();
    assert_eq!(
        attempt,
        [
            &AttemptState::Settled,
            &AttemptState::Failed,
            &AttemptState::Cancelled
        ]
    );
}

fn names_round_trip<S: Lifecycle + Serialize>() {
    for &s in S::ALL {
        assert_eq!(
            serde_json::to_value(s).expect("serialize"),
            serde_json::Value::String(s.as_str().to_owned()),
            "{s:?}"
        );
        assert_eq!(S::parse(s.as_str()), Some(s));
    }
    assert_eq!(S::parse("BOGUS"), None);
}

#[test]
fn stored_names_are_the_wire_names() {
    names_round_trip::<Run>();
    names_round_trip::<Task>();
    names_round_trip::<AttemptState>();
}

#[test]
fn an_illegal_transition_names_the_machine_and_both_states() {
    let e = check(Run::Created, Run::Running).expect_err("CREATED -> RUNNING");
    assert_eq!(
        e.to_string(),
        "ILLEGAL_TRANSITION: run CREATED -> RUNNING is not in the transition table"
    );
}

#[test]
fn resuming_returns_only_to_the_saved_working_state() {
    // Suspending saves the state left; suspended-to-suspended keeps it; resuming must match and clears it.
    assert_eq!(
        resume_state(Run::Running, Run::Blocked, None),
        Ok(Some(Run::Running))
    );
    assert_eq!(
        resume_state(
            Run::WaitingExternal,
            Run::PauseRequested,
            Some(Run::Running)
        ),
        Ok(Some(Run::Running))
    );
    assert_eq!(
        resume_state(Run::Paused, Run::Running, Some(Run::Running)),
        Ok(None)
    );
    for saved in [None, Some(Run::Verifying)] {
        assert_eq!(
            resume_state(Run::Paused, Run::Running, saved),
            Err(IllegalTransition {
                from: Run::Paused,
                to: Run::Running
            })
        );
    }
    // Working-to-working and terminal moves keep nothing; the table still applies first.
    assert_eq!(resume_state(Run::Ready, Run::Running, None), Ok(None));
    assert_eq!(
        resume_state(Run::Paused, Run::Failed, Some(Run::Ready)),
        Ok(None)
    );
    assert!(resume_state(Run::Created, Run::Running, None).is_err());
}
