//! The retry policy of REQ-042 (docs/adr/ADR-020.md decisions 1 and 2): what a failed attempt means for its task.
//! A transient failure waits for its Retry-After, else for a full-jitter delay `uniform(0, min(cap, base·2^n))` for
//! the task's n-th failure (n from 0), at most [`MAX_RETRIES`] times after the first; a missing authentication or an
//! incompatible CLI blocks at once, with no backoff. Pure: the caller gives the failure time (the store's whole
//! seconds) and a uniform 32-bit draw, so a fixed seed replays the same delays.

use crate::protocol::BlockedReason;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The backoff's base (REQ-042 decision 1: an initial value, changed only by a CR).
pub const BASE: Duration = Duration::from_secs(2);
/// The backoff's cap.
pub const CAP: Duration = Duration::from_secs(300);
/// Retries after a task's first failure; the failure after them exhausts it (REQ-042 behaviour 3).
pub const MAX_RETRIES: u32 = 8;

/// How an attempt failed, as its executor reports it (ADR-020 decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// A 429, a 503 or other 5xx, a timeout, or a failure the executor cannot classify; `retry_after` only from a
    /// 429/503's Retry-After.
    Transient { retry_after: Option<Duration> },
    /// Missing authentication (BLOCKED_AUTH).
    Auth,
    /// An incompatible CLI found at run time (BLOCKED_CAPABILITY).
    Capability,
    /// A refusal before anything ran that the same input would meet again, such as CONTEXT_BUDGET_TOO_SMALL
    /// (docs/adr/ADR-026.md decision 9): no retry, a decision at once (NEED_DECISION).
    Decision,
}

impl FailureClass {
    /// As an OBSERVED effect and the retry events record it: `{"class": "transient"|"auth"|"capability"|"decision",
    /// "retry_after_ms": n|null}`, so a host that crashes after OBSERVED disposes of it by its class (ADR-020
    /// decision 1).
    pub fn to_json(self) -> serde_json::Value {
        let (class, retry_after) = match self {
            FailureClass::Transient { retry_after } => ("transient", retry_after),
            FailureClass::Auth => ("auth", None),
            FailureClass::Capability => ("capability", None),
            FailureClass::Decision => ("decision", None),
        };
        serde_json::json!({
            "class": class,
            "retry_after_ms": retry_after.map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
        })
    }

    /// The class `to_json` wrote; `None` for anything else (an old record without a class).
    pub fn from_json(value: &serde_json::Value) -> Option<FailureClass> {
        match value.get("class")?.as_str()? {
            "transient" => Some(FailureClass::Transient {
                retry_after: value
                    .get("retry_after_ms")
                    .and_then(serde_json::Value::as_u64)
                    .map(Duration::from_millis),
            }),
            "auth" => Some(FailureClass::Auth),
            "capability" => Some(FailureClass::Capability),
            "decision" => Some(FailureClass::Decision),
            _ => None,
        }
    }
}

/// What a failure means for its task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterFailure {
    /// READY again at `next_retry_at` (whole seconds), `delay` after the failure; `bound` is the jitter's bound,
    /// `None` when the delay is a Retry-After.
    Retry {
        next_retry_at: SystemTime,
        delay: Duration,
        bound: Option<Duration>,
    },
    /// BLOCKED for `reason`, with no retry.
    Blocked(BlockedReason),
    /// Its retries are used up: BLOCKED for a decision (NEED_DECISION).
    Exhausted,
    /// Retrying would meet the same refusal: BLOCKED for a decision (NEED_DECISION) at once.
    Decide,
}

/// The policy for a task's failure: `failures` is its failed attempts including this one (`tasks.attempt_count`
/// after it, at least 1), `failed_at` the failure time (cut to the second), `draw` a uniform 32-bit sample.
pub fn after_failure(
    class: FailureClass,
    failures: u32,
    failed_at: SystemTime,
    draw: u32,
) -> AfterFailure {
    let retry_after = match class {
        FailureClass::Auth => return AfterFailure::Blocked(BlockedReason::BlockedAuth),
        FailureClass::Capability => return AfterFailure::Blocked(BlockedReason::BlockedCapability),
        FailureClass::Decision => return AfterFailure::Decide,
        FailureClass::Transient { retry_after } => retry_after,
    };
    if failures > MAX_RETRIES {
        return AfterFailure::Exhausted;
    }
    let (delay, bound) = match retry_after {
        Some(after) => (after, None),
        None => {
            let b = bound(failures.saturating_sub(1));
            let ms = u64::try_from(b.as_millis()).unwrap_or(u64::MAX);
            // floor(draw · (ms + 1) / 2^32): uniform over 0..=ms.
            let delay = (u128::from(draw) * (u128::from(ms) + 1)) >> 32;
            (
                Duration::from_millis(u64::try_from(delay).unwrap_or(ms)),
                Some(b),
            )
        }
    };
    // Whole seconds: the failure's second plus the delay rounded up.
    let wait = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
    AfterFailure::Retry {
        next_retry_at: whole_seconds(failed_at) + Duration::from_secs(wait),
        delay,
        bound,
    }
}

/// The full-jitter bound of the n-th failure (n from 0): `min(CAP, BASE·2^n)`.
pub fn bound(n: u32) -> Duration {
    let doubled = 1u64
        .checked_shl(n)
        .and_then(|f| BASE.as_secs().checked_mul(f))
        .unwrap_or(u64::MAX);
    Duration::from_secs(doubled.min(CAP.as_secs()))
}

/// A SplitMix64 generator (Steele, Lea and Flood): the host's source of jitter draws, seeded at its start.
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> SplitMix64 {
        SplitMix64(seed)
    }

    /// The next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// The next uniform 32-bit draw (the output's high half).
    pub fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }
}

/// `at` cut to the second.
fn whole_seconds(at: SystemTime) -> SystemTime {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64, millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis)
    }

    fn transient() -> FailureClass {
        FailureClass::Transient { retry_after: None }
    }

    #[test]
    fn a_refusal_that_would_refuse_the_same_input_again_asks_for_a_decision_at_once() {
        // ADR-026 decision 9: CONTEXT_BUDGET_TOO_SMALL is deterministic; no retry, whatever the count.
        for failures in [1, 2, MAX_RETRIES, MAX_RETRIES + 1] {
            assert_eq!(
                after_failure(FailureClass::Decision, failures, at(1_000, 0), 7),
                AfterFailure::Decide
            );
        }
        let json = FailureClass::Decision.to_json();
        assert_eq!(json["class"], "decision");
        assert_eq!(FailureClass::from_json(&json), Some(FailureClass::Decision));
    }

    #[test]
    fn the_bound_doubles_from_two_seconds_up_to_the_cap() {
        let bounds: Vec<u64> = (0..10).map(|n| bound(n).as_secs()).collect();
        assert_eq!(bounds, [2, 4, 8, 16, 32, 64, 128, 256, 300, 300]);
        assert_eq!(bound(u32::MAX), CAP);
    }

    #[test]
    fn a_jittered_delay_stays_within_its_bound_in_whole_seconds() {
        // REQ-042 behaviour 1: the n-th failure waits uniform(0, min(300, 2·2^n)) s; whole seconds: the delay is
        // rounded up, so next_retry_at − the failure's whole second lies in [0, bound].
        let failed = at(1_000, 0);
        for failures in 1..=MAX_RETRIES {
            let n = failures - 1;
            for draw in [0, 1, u32::MAX / 3, u32::MAX / 2, u32::MAX - 1, u32::MAX] {
                let AfterFailure::Retry {
                    next_retry_at,
                    delay,
                    bound: Some(b),
                } = after_failure(transient(), failures, failed, draw)
                else {
                    panic!("failure {failures} did not wait");
                };
                assert_eq!(b, bound(n));
                assert!(delay <= b, "{delay:?} > {b:?}");
                let waited = next_retry_at
                    .duration_since(failed)
                    .expect("not before the failure");
                assert_eq!(waited.subsec_nanos(), 0);
                assert!(waited <= b, "failure {failures}: waits {waited:?} > {b:?}");
            }
        }
        // The extremes: no wait at all, and the whole bound.
        let wait = |draw| match after_failure(transient(), 3, failed, draw) {
            AfterFailure::Retry { next_retry_at, .. } => next_retry_at.duration_since(failed).ok(),
            _ => None,
        };
        assert_eq!(wait(0), Some(Duration::ZERO));
        assert_eq!(wait(u32::MAX), Some(Duration::from_secs(8)));
    }

    #[test]
    fn the_failure_time_counts_from_its_whole_second() {
        let AfterFailure::Retry { next_retry_at, .. } = after_failure(
            FailureClass::Transient {
                retry_after: Some(Duration::from_secs(120)),
            },
            6,
            at(1_000, 999),
            0,
        ) else {
            panic!("no wait");
        };
        assert_eq!(next_retry_at, at(1_120, 0));
    }

    #[test]
    fn a_retry_after_is_waited_exactly_and_not_capped() {
        // REQ-042 behaviour 1: Retry-After first (AC-042-01: the 429 waits 120 s); it is bounded by the deadline only
        // (ADR-020 decision 8), not by the cap.
        for (secs, failures) in [(120, 6), (600, 1), (1, 8)] {
            let retry_after = Some(Duration::from_secs(secs));
            assert_eq!(
                after_failure(
                    FailureClass::Transient { retry_after },
                    failures,
                    at(50, 0),
                    u32::MAX
                ),
                AfterFailure::Retry {
                    next_retry_at: at(50 + secs, 0),
                    delay: Duration::from_secs(secs),
                    bound: None,
                }
            );
        }
        // A part of a second is waited as a whole one.
        let AfterFailure::Retry { next_retry_at, .. } = after_failure(
            FailureClass::Transient {
                retry_after: Some(Duration::from_millis(1_500)),
            },
            1,
            at(50, 0),
            0,
        ) else {
            panic!("no wait");
        };
        assert_eq!(next_retry_at, at(52, 0));
    }

    #[test]
    fn the_ninth_failure_exhausts_the_retries() {
        // REQ-042 behaviour 3: at most 8 retries after the first failure (AC-042-03 R3: 9 attempts).
        assert!(matches!(
            after_failure(transient(), MAX_RETRIES, at(0, 0), 7),
            AfterFailure::Retry { .. }
        ));
        for failures in [MAX_RETRIES + 1, MAX_RETRIES + 2, u32::MAX] {
            assert_eq!(
                after_failure(transient(), failures, at(0, 0), 7),
                AfterFailure::Exhausted
            );
        }
        let retry_after = Some(Duration::from_secs(5));
        assert_eq!(
            after_failure(
                FailureClass::Transient { retry_after },
                MAX_RETRIES + 1,
                at(0, 0),
                7
            ),
            AfterFailure::Exhausted
        );
    }

    #[test]
    fn authentication_and_capability_block_at_once() {
        // REQ-042 behaviour 4: no backoff (AC-042-03 R1: attempt_count 1, next_retry_at empty).
        for failures in [1, 5, MAX_RETRIES + 1] {
            assert_eq!(
                after_failure(FailureClass::Auth, failures, at(0, 0), 0),
                AfterFailure::Blocked(BlockedReason::BlockedAuth)
            );
            assert_eq!(
                after_failure(FailureClass::Capability, failures, at(0, 0), 0),
                AfterFailure::Blocked(BlockedReason::BlockedCapability)
            );
        }
    }

    #[test]
    fn a_class_survives_its_record() {
        for class in [
            transient(),
            FailureClass::Transient {
                retry_after: Some(Duration::from_secs(120)),
            },
            FailureClass::Auth,
            FailureClass::Capability,
        ] {
            assert_eq!(FailureClass::from_json(&class.to_json()), Some(class));
        }
        assert_eq!(FailureClass::from_json(&serde_json::json!({})), None);
        assert_eq!(
            FailureClass::from_json(&serde_json::json!({"class": "permanent"})),
            None
        );
    }

    #[test]
    fn splitmix64_gives_the_reference_sequence() {
        let mut zero = SplitMix64::new(0);
        assert_eq!(zero.next_u64(), 0xe220_a839_7b1d_cdaf);
        assert_eq!(zero.next_u64(), 0x6e78_9e6a_a1b9_65f4);
        let mut seeded = SplitMix64::new(42);
        assert_eq!(seeded.next_u32(), 0xbdd7_3226);
        assert_eq!(seeded.next_u64(), 0x28ef_e333_b266_f103);
    }
}
