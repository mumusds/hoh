//! Which event ends a follow and which replay history (docs/adr/ADR-031.md decisions 9, 10 and 12; test plan
//! "客户端", "attach", "回放标记").

use super::*;

fn event(seq: i64, kind: &str, payload: Value) -> Value {
    json!({"seq": seq, "kind": kind, "run_id": "R", "payload": payload})
}

fn result(seq: i64, state: &str) -> Value {
    event(
        seq,
        "run.result",
        json!({"state": state, "last_event_seq": seq}),
    )
}

/// What a run of events did: the shown seqs, each with whether it replays; the seq that ended it; the dropped seqs.
type Ran = (Vec<(i64, bool)>, Option<i64>, Vec<Option<i64>>);

/// The seqs of the shown events, each with whether it replays, until one ends the follow; dropped seqs apart.
fn run(watch: &mut Watch, events: Vec<Value>) -> Ran {
    let (mut shown, mut dropped) = (Vec::new(), Vec::new());
    for e in events {
        match watch.see(e) {
            Seen::Dropped(seq) => dropped.push(seq),
            Seen::Shown { event, ends } => {
                let seq = event["seq"].as_i64().unwrap_or(-1);
                assert!(
                    event.get("replay").is_none() || event["replay"] == json!(true),
                    "{event}"
                );
                shown.push((seq, event.get("replay").is_some()));
                if ends {
                    return (shown, Some(seq), dropped);
                }
            }
        }
    }
    (shown, None, dropped)
}

#[test]
fn a_prompt_ends_only_at_a_result_after_its_last_commands_acceptance() {
    // E7: the run's earlier results (one before the input, one the answered input's own commit wrote) end nothing; the
    // result after the ResumeRun's acceptance does. Its replay marks go up to the first command's acceptance.
    let mut watch = Watch::new(
        End::Accepted {
            command_id: "c-resume".to_owned(),
        },
        Replay::Before {
            command_id: "c-input".to_owned(),
        },
        0,
    );
    let (shown, ended, dropped) = run(
        &mut watch,
        vec![
            event(1, "run.created", json!({"command_id": "c-start"})),
            result(2, "INCOMPLETE"),
            event(
                3,
                "input.accepted",
                json!({"command_id": "c-input", "question_id": null}),
            ),
            result(4, "INCOMPLETE"),
            event(4, "run.duplicate", json!({})),
            event(3, "run.behind", json!({})),
            event(5, "reconcile.run", json!({"from": "INCOMPLETE"})),
            event(6, "run.resumed", json!({"command_id": "c-resume"})),
            event(7, "task.ready", json!({})),
            result(8, "INCOMPLETE"),
            event(9, "after.end", json!({})),
        ],
    );
    assert_eq!(
        shown,
        vec![
            (1, true),
            (2, true),
            (3, false),
            (4, false),
            (5, false),
            (6, false),
            (7, false),
            (8, false)
        ]
    );
    assert_eq!(ended, Some(8));
    assert_eq!(dropped, vec![Some(4), Some(3)]);
    assert_eq!(watch.first().map(|e| e["seq"].clone()), Some(json!(3)));
    assert_eq!(watch.accepted().map(|e| e["seq"].clone()), Some(json!(6)));
    assert_eq!((watch.last(), watch.results()), (8, 3));
}

#[test]
fn a_new_runs_prompt_replays_nothing_and_ends_at_its_first_result() {
    let mut watch = Watch::new(
        End::Accepted {
            command_id: "c1".to_owned(),
        },
        Replay::Before {
            command_id: "c1".to_owned(),
        },
        0,
    );
    let (shown, ended, _) = run(
        &mut watch,
        vec![
            event(10, "run.created", json!({"command_id": "c1"})),
            event(11, "run.paused", json!({})),
            result(12, "PAUSED"),
        ],
    );
    assert_eq!(shown, vec![(10, false), (11, false), (12, false)]);
    assert_eq!(ended, Some(12));
    // An event with no seq is dropped, never shown out of order.
    let mut watch = Watch::new(
        End::Accepted {
            command_id: "c1".to_owned(),
        },
        Replay::None,
        0,
    );
    assert_eq!(watch.see(json!({"kind": "x"})), Seen::Dropped(None));
}

#[test]
fn an_attach_ends_past_its_watermark_or_in_a_terminal_state_never_at_an_older_stop() {
    // Decision 12: W0 = 5. A PAUSED run's old result (2) ends nothing; its next result past W0 does.
    let mut watch = Watch::new(End::Attached { w0: 5 }, Replay::UpTo(5), 0);
    let (shown, ended, _) = run(
        &mut watch,
        vec![
            event(1, "run.created", json!({})),
            result(2, "PAUSED"),
            event(5, "run.noted", json!({})),
            event(6, "run.resumed", json!({})),
            result(7, "PAUSED"),
        ],
    );
    assert_eq!(
        shown,
        vec![(1, true), (2, true), (5, true), (6, false), (7, false)]
    );
    assert_eq!(ended, Some(7));
    // INCOMPLETE and BLOCKED: the same.
    for state in ["INCOMPLETE", "BLOCKED"] {
        let mut watch = Watch::new(End::Attached { w0: 3 }, Replay::None, 0);
        let (_, ended, _) = run(&mut watch, vec![result(3, state)]);
        assert_eq!(ended, None, "{state}");
    }
    // A run that had ended before the attach: its result, in the replay, ends it.
    for state in TERMINAL {
        let mut watch = Watch::new(End::Attached { w0: 9 }, Replay::UpTo(9), 0);
        let (shown, ended, _) = run(
            &mut watch,
            vec![
                result(2, "BLOCKED"),
                event(3, "run.cancel_requested", json!({})),
                result(4, state),
            ],
        );
        assert_eq!(ended, Some(4), "{state}");
        assert_eq!(shown, vec![(2, true), (3, true), (4, true)]);
    }
    assert_eq!(watch.results(), 2, "the PAUSED run's two results");
}
