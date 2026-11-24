//! Mode intent and ordinary product-write admission, orthogonal to lifecycle (REQ-007/009, ADR-038).
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{Mode, RunState};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    Apply,
    Pending,
    Refused,
}

/// The complete state × current-mode × target-mode table: modes never add lifecycle edges.
/// Every accepted intent waits for an existing writer's safe point, including a repeated intent (ADR-036).
pub fn transition(state: RunState, _from: Mode, _to: Mode, writing: bool) -> Transition {
    if state.is_terminal() {
        Transition::Refused
    } else if writing {
        Transition::Pending
    } else {
        Transition::Apply
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteRefusal {
    Plan,
    NoSpec,
    NotReady,
    PendingMode,
    PendingDraft,
}
impl WriteRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::Plan => "plan_no_implementation",
            Self::NoSpec => "spec_not_approved",
            Self::NotReady => "readiness_not_met",
            Self::PendingMode => "mode_change_pending",
            Self::PendingDraft => "draft_approval_pending",
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct WriteFacts {
    pub approved: bool,
    pub ready: bool,
    pub mode_pending: bool,
    pub draft_pending: bool,
}
/// Read-only work and Runtime drafting are different admission subjects; this rule only judges product writers.
pub fn product_write(mode: Mode, facts: WriteFacts) -> Result<(), WriteRefusal> {
    if facts.mode_pending {
        Err(WriteRefusal::PendingMode)
    } else if mode == Mode::Plan {
        Err(WriteRefusal::Plan)
    } else if !facts.approved {
        Err(WriteRefusal::NoSpec)
    } else if !facts.ready {
        Err(WriteRefusal::NotReady)
    } else if facts.draft_pending {
        Err(WriteRefusal::PendingDraft)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::AllVariants;
    #[test]
    fn test_009_03_every_state_mode_target_and_writer_matches_the_table() {
        let rows = [
            (RunState::Created, true),
            (RunState::Discovering, true),
            (RunState::Specifying, true),
            (RunState::Ready, true),
            (RunState::Running, true),
            (RunState::Verifying, true),
            (RunState::PauseRequested, true),
            (RunState::Paused, true),
            (RunState::WaitingExternal, true),
            (RunState::Blocked, true),
            (RunState::CancelRequested, true),
            (RunState::Cancelled, false),
            (RunState::Incomplete, true),
            (RunState::Failed, false),
            (RunState::Completed, false),
        ];
        assert_eq!(rows.len(), RunState::ALL.len());
        for state in RunState::ALL {
            assert_eq!(rows.iter().filter(|r| r.0 == *state).count(), 1);
        }
        for (state, allowed) in rows {
            for from in Mode::ALL {
                for to in Mode::ALL {
                    for writing in [false, true] {
                        let expected = match (allowed, writing) {
                            (false, _) => Transition::Refused,
                            (true, true) => Transition::Pending,
                            (true, false) => Transition::Apply,
                        };
                        assert_eq!(
                            transition(state, *from, *to, writing),
                            expected,
                            "{state:?} {from:?}→{to:?}, writer={writing}"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn test_009_03_product_write_requires_approval_readiness_and_settled_intent() {
        for mode in Mode::ALL {
            for approved in [false, true] {
                for ready in [false, true] {
                    for pending in [false, true] {
                        for draft in [false, true] {
                            let permitted = (*mode == Mode::Build || *mode == Mode::Auto)
                                && approved
                                && ready
                                && !pending
                                && !draft;
                            assert_eq!(
                                product_write(
                                    *mode,
                                    WriteFacts {
                                        approved,
                                        ready,
                                        mode_pending: pending,
                                        draft_pending: draft
                                    }
                                )
                                .is_ok(),
                                permitted,
                                "{mode:?}, approved={approved}, ready={ready}, pending={pending}, draft={draft}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[test]
fn test_009_02_m3l103_readiness_refusal_precedes_pending_draft() {
    assert_eq!(
        product_write(
            Mode::Build,
            WriteFacts {
                approved: true,
                ready: false,
                mode_pending: false,
                draft_pending: true
            }
        ),
        Err(WriteRefusal::NotReady)
    );
}
