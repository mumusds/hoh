//! The replay's input contract on the S0 recordings' observations (docs/adr/ADR-032.md design correction 7 item 2).

use super::*;
use serde_json::json;

/// The observations of a committed excerpt (`<agent>/<case>.meta.json`).
fn recorded(path: &str) -> Value {
    let meta: Value = serde_json::from_str(
        &std::fs::read_to_string(format!(
            "{}/../../tests/fixtures/commander-readonly/excerpts/{path}.meta.json",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("the committed excerpt"),
    )
    .expect("JSON");
    meta["observations"].clone()
}

/// `value` with the field at `path` set (or removed, for `None`).
fn with(value: &Value, path: &[&str], to: Option<Value>) -> Value {
    let mut v = value.clone();
    let mut at = &mut v;
    for key in &path[..path.len() - 1] {
        at = &mut at[*key];
    }
    let object = at.as_object_mut().expect("an object");
    match to {
        Some(to) => object.insert(path[path.len() - 1].to_owned(), to),
        None => object.remove(path[path.len() - 1]),
    };
    v
}

fn refused(agent: Agent, v: &Value, what: &str) {
    match parse(agent, v) {
        Err(why) => assert!(
            why.starts_with(NOT_OBSERVED) && why.contains(what),
            "{what}: {why}"
        ),
        Ok(o) => panic!("{what}: accepted as {o:?}"),
    }
}

#[test]
fn the_recorded_observations_are_the_contracts() {
    let o = parse(Agent::Claude, &recorded("claude/ro-new")).expect("claude");
    assert_eq!(
        (o.version.as_str(), o.exit, o.logs_complete, o.prompt_fed),
        ("2.1.325", Some(Termination::Code(0)), true, Some(true))
    );
    let o = parse(Agent::Codex, &recorded("codex/app-ro-new")).expect("codex");
    assert_eq!(
        o.codex,
        Some(CodexLaunch {
            profile: Profile::ReadOnly,
            resume: None,
            model: None
        })
    );
    let o = parse(Agent::Codex, &recorded("codex/app-ro-resumed")).expect("codex resumed");
    assert!(o.codex.and_then(|l| l.resume).is_some());
    let o = parse(Agent::Codex, &recorded("codex/app-control")).expect("codex control");
    assert_eq!(o.codex.map(|l| l.profile), Some(Profile::Yolo));
    let o = parse(Agent::Pi, &recorded("pi/noext-resumed")).expect("pi resumed");
    let launch = o.pi.expect("pi launch");
    assert!(launch.resume);
    assert_eq!(launch.opened.as_ref(), launch.noted.first());
    let o = parse(Agent::Pi, &recorded("pi/noext-new")).expect("pi new");
    assert_eq!(o.pi.map(|l| l.resume), Some(false));
    // pi's recordings before run 20261117T002630Z have no launch: not observed.
    refused(Agent::Pi, &recorded("pi/ro-new"), "launch missing");
}

#[test]
fn every_required_observation_missing_or_ill_typed_is_not_observed() {
    // Each JSON type a field does not accept, containers included (M2T20D-900, -1001).
    let samples = [
        Value::Null,
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(""),
        json!("x"),
        json!([]),
        json!(["x"]),
        json!({}),
        json!({"x": 1}),
    ];
    let cases: [(Agent, &str, &[&[&str]]); 4] = [
        (
            Agent::Claude,
            "claude/ro-new",
            &[
                &["version"],
                &["exit"],
                &["exit", "returncode"],
                &["logs_complete"],
                &["prompt_fed"],
            ],
        ),
        (
            Agent::Codex,
            "codex/app-ro-resumed",
            &[
                &["version"],
                &["exit"],
                &["exit", "returncode"],
                &["logs_complete"],
                &["launch"],
                &["launch", "profile"],
                &["launch", "resume"],
                &["launch", "model"],
            ],
        ),
        (
            Agent::Pi,
            "pi/noext-resumed",
            &[
                &["version"],
                &["exit"],
                &["exit", "returncode"],
                &["logs_complete"],
                &["launch"],
                &["launch", "session_id"],
                &["launch", "resume"],
                &["noted"],
                &["opened"],
            ],
        ),
        (
            Agent::Pi,
            "pi/noext-new",
            &[&["launch", "resume"], &["launch", "session_id"]],
        ),
    ];
    for (agent, case, paths) in cases {
        let base = recorded(case);
        parse(agent, &base).expect(case);
        for path in paths {
            let name = path.join(".");
            refused(agent, &with(&base, path, None), &name);
            for sample in &samples {
                let changed = with(&base, path, Some(sample.clone()));
                if parse(agent, &changed).is_ok() {
                    // Only values of the field's own type pass: a version or thread id string, a null resume or model
                    // or opened, a boolean flag, an integer or null exit code, an empty noted list.
                    let accepted = matches!(
                        (name.as_str(), sample),
                        ("version" | "launch.resume" | "launch.model" | "launch.session_id", Value::String(s)) if !s.is_empty()
                    ) || matches!(
                        (name.as_str(), sample),
                        (
                            "launch.resume" | "launch.model" | "opened" | "exit.returncode",
                            Value::Null
                        )
                    ) || matches!(
                        (name.as_str(), sample),
                        (
                            "logs_complete" | "prompt_fed" | "launch.resume",
                            Value::Bool(_)
                        )
                    ) || (name == "exit.returncode" && sample.is_i64())
                        || (name == "noted" && sample == &json!([]));
                    assert!(accepted, "{case} {name} = {sample} was accepted");
                }
            }
        }
    }
    // An unknown field anywhere: the top level, exit, a launch.
    for (agent, case, path) in [
        (Agent::Claude, "claude/ro-new", &["sandbox"][..]),
        (Agent::Claude, "claude/control", &["exit", "signal"][..]),
        (Agent::Codex, "codex/app-ro-new", &["exit", "signal"][..]),
        (Agent::Codex, "codex/app-ro-new", &["launch", "sandbox"][..]),
        (Agent::Pi, "pi/noext-resumed", &["launch", "sandbox"][..]),
        (Agent::Pi, "pi/noext-new", &["exit", "signal"][..]),
    ] {
        refused(
            agent,
            &with(&recorded(case), path, Some(json!(9))),
            &format!("{} unknown", path.join(".")),
        );
    }
    // A float is no integer exit code, a boolean no profile.
    let base = recorded("codex/app-ro-new");
    refused(
        Agent::Codex,
        &with(&base, &["exit", "returncode"], Some(json!(0.0))),
        "exit.returncode ill-typed",
    );
    refused(
        Agent::Codex,
        &with(&base, &["launch", "profile"], Some(json!(false))),
        "launch.profile ill-typed",
    );
    refused(
        Agent::Codex,
        &with(&base, &["launch", "profile"], Some(json!("readonly"))),
        "launch.profile ill-typed",
    );
}

#[test]
fn what_no_adapter_reads_is_not_required() {
    // exit.how, pi's prompt_sent and get_state_session_file; a new pi session's noted and opened files.
    let base = recorded("pi/noext-resumed");
    for path in [
        &["exit", "how"][..],
        &["prompt_sent"][..],
        &["get_state_session_file"][..],
    ] {
        assert!(
            parse(Agent::Pi, &with(&base, path, None)).is_ok(),
            "{}",
            path.join(".")
        );
    }
    let new = recorded("pi/noext-new");
    parse(
        Agent::Pi,
        &with(&with(&new, &["noted"], None), &["opened"], None),
    )
    .expect("a new session");
    // A signal is a negative code.
    let o = parse(
        Agent::Claude,
        &with(
            &recorded("claude/ro-new"),
            &["exit", "returncode"],
            Some(json!(-9)),
        ),
    )
    .expect("signal");
    assert_eq!(o.exit, Some(Termination::Signal(9)));
}
