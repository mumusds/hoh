//! The supervision core against REQ-021's timetables (docs/adr/ADR-025.md decision 1).

use super::*;
use serde_json::json;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `ms` milliseconds into the test's time.
fn t(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000) + Duration::from_millis(ms)
}

fn ms(at: SystemTime) -> u64 {
    u64::try_from(at.duration_since(t(0)).unwrap_or_default().as_millis()).unwrap_or(u64::MAX)
}

/// One trigger fired: its key, when (ms), and its events' seqs.
type Fired = (Key, u64, Vec<i64>);

/// The events `(ms, seq, kind, payload)`, run through classify and the debounce in 100 ms ticks up to `end` ms, as the
/// store does: each tick ingests the events committed by then (one pending trigger per key), then fires the triggers
/// due.
fn run(policy: Policy, events: &[(u64, i64, &str, serde_json::Value)], end: u64) -> Vec<Fired> {
    let mut pending: BTreeMap<Key, Pending> = BTreeMap::new();
    let mut fired = Vec::new();
    let mut next = 0;
    let mut now = 0;
    while now <= end {
        while next < events.len() && events[next].0 <= now {
            let (_, seq, kind, payload) = &events[next];
            if let Some(wake) = classify(policy, kind, payload, Some(3)) {
                pending
                    .entry(wake.key.clone())
                    .and_modify(|p| p.join(*seq, t(now), wake.severity))
                    .or_insert_with(|| Pending::new(*seq, t(now), wake.severity));
            }
            next += 1;
        }
        let due: Vec<Key> = pending
            .iter()
            .filter(|(_, p)| p.due_at() <= t(now))
            .map(|(k, _)| k.clone())
            .collect();
        for key in due {
            if let Some(p) = pending.remove(&key) {
                fired.push((key, now, p.seqs));
            }
        }
        now += 100;
    }
    fired
}

fn issue(fingerprint: &str, spec_revision: i64) -> serde_json::Value {
    json!({"severity": "high", "fingerprint": fingerprint, "spec_revision": spec_revision})
}

#[test]
fn test_021_02_k1_merges_three_at_3s_k2_k3_fire_alone_a_stream_is_capped_at_10s_integrity_same_tick()
 {
    // AC-021-02: K1 = (fp1, rev 3) at 0, 0.5 and 1.0 s; K2 = (fp2, rev 3) at 0.2 s; K3 = (fp1, rev 4) at 0.3 s; one K1
    // event a second for 15 s from 30 s; an IntegrityViolation at 60 s. K1's first trigger fires at 3.0 s (2 s quiet
    // after 1.0) with its three seqs; K2 and K3 fire alone; the stream's first trigger fires 10.0 s after its first
    // event, so a key never has two unhandled triggers; the integrity event fires in its own tick.
    let mut events = vec![
        (0, 1, "issue.opened", issue("fp1", 3)),
        (200, 2, "issue.opened", issue("fp2", 3)),
        (300, 3, "issue.opened", issue("fp1", 4)),
        (500, 4, "issue.opened", issue("fp1", 3)),
        (1000, 5, "issue.opened", issue("fp1", 3)),
    ];
    for i in 0..15 {
        events.push((
            30_000 + i * 1000,
            10 + i64::try_from(i).unwrap_or(0),
            "issue.opened",
            issue("fp1", 3),
        ));
    }
    events.push((
        60_000,
        99,
        "integrity_violation",
        json!({"reason": "candidate_changed"}),
    ));
    let fired = run(Policy::Balanced, &events, 70_000);
    let k1 = Key {
        fingerprint: "fp1".to_owned(),
        spec_revision: Some(3),
    };
    let k2 = Key {
        fingerprint: "fp2".to_owned(),
        spec_revision: Some(3),
    };
    let k3 = Key {
        fingerprint: "fp1".to_owned(),
        spec_revision: Some(4),
    };
    let first: Vec<&Fired> = fired.iter().filter(|f| f.1 < 30_000).collect();
    assert!(
        first.contains(&&(k1.clone(), 3000, vec![1, 4, 5])),
        "{first:?}"
    );
    assert!(first.contains(&&(k2, 2200, vec![2])), "{first:?}");
    assert!(first.contains(&&(k3, 2300, vec![3])), "{first:?}");
    assert_eq!(first.len(), 3, "{first:?}");
    let stream: Vec<&Fired> = fired
        .iter()
        .filter(|f| f.0 == k1 && f.1 >= 30_000)
        .collect();
    assert_eq!(
        stream[0].1, 40_000,
        "capped 10.0 s after its first event: {stream:?}"
    );
    assert_eq!(stream[0].2, (10..=20).collect::<Vec<i64>>(), "{stream:?}");
    assert_eq!(
        stream
            .iter()
            .flat_map(|f| f.2.clone())
            .collect::<Vec<i64>>(),
        (10..25).collect::<Vec<i64>>(),
        "every event of the stream in exactly one trigger: {stream:?}"
    );
    let integrity: Vec<&Fired> = fired.iter().filter(|f| f.2 == vec![99]).collect();
    assert_eq!(integrity.len(), 1);
    assert_eq!(integrity[0].1, 60_000, "the same tick: {integrity:?}");
}

#[test]
fn test_021_01_a_token_tool_heartbeat_and_medium_issue_storm_never_wakes() {
    // AC-021-01 (the classification half): token deltas, tool output, heartbeats, medium and low issues and a routine
    // candidate_ready never wake the Commander, under any policy.
    for policy in [Policy::Efficient, Policy::Balanced, Policy::Intensive] {
        for (kind, payload) in [
            ("token.delta", json!({"text": "x"})),
            ("tool.output", json!({"line": "y"})),
            ("heartbeat", json!({})),
            (
                "issue.opened",
                json!({"severity": "medium", "fingerprint": "f"}),
            ),
            (
                "issue.opened",
                json!({"severity": "low", "fingerprint": "f"}),
            ),
            ("candidate_ready", json!({"candidate_id": "C1"})),
            ("attempt.moved", json!({"to": "RUNNING"})),
        ] {
            assert_eq!(
                classify(policy, kind, &payload, Some(1)),
                None,
                "{policy:?} {kind} {payload}"
            );
        }
    }
}

#[test]
fn important_events_wake_under_every_policy_and_intensive_wakes_at_least_balanced_at_least_efficient()
 {
    // REQ-021 behaviour 1 and decision 5: the important set wakes under all three policies; on one fixture the wakes
    // are ordered intensive >= balanced >= efficient.
    let important = [
        ("need_decision", json!({"task_id": "T1"})),
        (
            "issue.opened",
            json!({"severity": "critical", "fingerprint": "c"}),
        ),
        (
            "issue.opened",
            json!({"severity": "high", "fingerprint": "h"}),
        ),
        ("no_progress", json!({})),
        ("budget.risk", json!({"kind": "usd"})),
        ("team.binding_blocked", json!({"role": "qa"})),
        ("spec.changed", json!({"spec_revision": 4})),
        ("milestone_ready", json!({"milestone": "M1"})),
        ("release_request", json!({})),
        ("integrity_violation", json!({"reason": "db"})),
    ];
    for policy in [Policy::Efficient, Policy::Balanced, Policy::Intensive] {
        for (kind, payload) in &important {
            assert!(
                classify(policy, kind, payload, Some(1)).is_some(),
                "{policy:?} {kind}"
            );
        }
    }
    let fixture = [
        ("iteration.completed", json!({"iteration_id": "I1"})),
        (
            "task.implemented",
            json!({"task_id": "T1", "risk_reduction": 4}),
        ),
        (
            "task.implemented",
            json!({"task_id": "T2", "risk_reduction": 1}),
        ),
        ("need_decision", json!({"task_id": "T3"})),
    ];
    let count = |policy| {
        fixture
            .iter()
            .filter(|(kind, payload)| classify(policy, kind, payload, Some(1)).is_some())
            .count()
    };
    let (efficient, balanced, intensive) = (
        count(Policy::Efficient),
        count(Policy::Balanced),
        count(Policy::Intensive),
    );
    assert!(
        intensive >= balanced && balanced >= efficient,
        "{intensive} {balanced} {efficient}"
    );
    assert!(intensive > efficient, "the policies differ on the fixture");
}

#[test]
fn a_key_is_the_events_fingerprint_and_spec_revision_else_its_kind_and_subject_and_the_runs_revision()
 {
    // REQ-021 behaviour 3: events name their fingerprint and spec revision; one that names neither keys by its kind
    // and subject and the run's current spec revision.
    let named = classify(Policy::Balanced, "issue.opened", &issue("fp9", 7), Some(2));
    assert_eq!(
        named.map(|w| w.key),
        Some(Key {
            fingerprint: "fp9".to_owned(),
            spec_revision: Some(7)
        })
    );
    let unnamed = classify(
        Policy::Balanced,
        "need_decision",
        &json!({"task_id": "T4"}),
        Some(2),
    );
    assert_eq!(
        unnamed.map(|w| w.key),
        Some(Key {
            fingerprint: "need_decision/T4".to_owned(),
            spec_revision: Some(2)
        })
    );
    let integrity = classify(
        Policy::Balanced,
        "integrity_violation",
        &json!({"reason": "db"}),
        None,
    );
    assert_eq!(integrity.map(|w| w.severity), Some(Severity::Integrity));
}

#[test]
fn a_trigger_is_due_after_two_quiet_seconds_capped_ten_seconds_after_its_first_event_and_integrity_at_once()
 {
    // REQ-021 decision 1 and 2.
    let mut p = Pending::new(1, t(0), Severity::High);
    assert_eq!(ms(p.due_at()), 2000);
    p.join(2, t(1500), Severity::High);
    assert_eq!(ms(p.due_at()), 3500);
    p.join(3, t(9000), Severity::Normal);
    assert_eq!(ms(p.due_at()), 10_000, "capped");
    assert_eq!(p.severity, Severity::High, "the highest severity stays");
    p.join(4, t(9500), Severity::Integrity);
    assert_eq!(
        ms(p.due_at()),
        9500,
        "an integrity event makes it due at once"
    );
    assert_eq!(p.seqs, vec![1, 2, 3, 4]);
}

#[test]
fn the_wake_set_is_req_021s_closed_set_with_the_stale_retrigger() {
    // docs/adr/ADR-025.md decision 1 (fifth to ninth versions; M2T08D-262, -164): a refused team change no longer
    // wakes (it goes in the next wake's summary); a stale decision's re-trigger does, under every policy, keyed on its
    // base fingerprint and carrying no #retrigger chain.
    for policy in [Policy::Efficient, Policy::Balanced, Policy::Intensive] {
        assert_eq!(
            classify(
                policy,
                "team.binding_refused",
                &json!({"role": "qa"}),
                Some(1)
            ),
            None,
            "{policy:?}"
        );
        let retrigger = classify(
            policy,
            "supervisor.retrigger",
            &json!({"base_fingerprint": "need_decision/T1", "task_ids": ["T1"], "reason": "STALE_REVISION"}),
            Some(1),
        );
        assert_eq!(
            retrigger.map(|w| (w.key.fingerprint, w.severity)),
            Some(("need_decision/T1#retrigger".to_owned(), Severity::High)),
            "{policy:?}"
        );
    }
}

#[test]
fn an_ordinary_fingerprint_is_escaped_so_only_the_runtimes_suffixes_carry_hash_and_at() {
    // M2T08R-07 (docs/adr/ADR-025.md v13 decision 2): the stored key is injective by kind: an event's own fingerprint
    // has %, # and @ escaped (%25, %23, %40), so it never equals a re-trigger chain's "<base>#retrigger" nor another
    // revision's "<fingerprint>@r<revision>"; a default fingerprint (kind/subject) likewise; a re-trigger's base is a
    // stored key already and is kept.
    let named = classify(
        Policy::Balanced,
        "need_decision",
        &json!({"fingerprint": "fp%1#retrigger@r9"}),
        Some(3),
    );
    assert_eq!(
        named.map(|w| w.key.fingerprint),
        Some("fp%251%23retrigger%40r9".to_owned())
    );
    let default = classify(
        Policy::Balanced,
        "need_decision",
        &json!({"task_id": "T#1@x"}),
        Some(3),
    );
    assert_eq!(
        default.map(|w| w.key.fingerprint),
        Some("need_decision/T%231%40x".to_owned())
    );
    let chain = classify(
        Policy::Balanced,
        "supervisor.retrigger",
        &json!({"base_fingerprint": "fp%23x@r9#retrigger"}),
        Some(3),
    );
    assert_eq!(
        chain.map(|w| w.key.fingerprint),
        Some("fp%23x@r9#retrigger".to_owned())
    );
}
