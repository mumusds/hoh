//! `probe::conform` (docs/adr/ADR-030.md decision 10, "探测读取的扩展" and the request conformance and frame acceptance
//! of "PASS 取决于被测二进制"): the probe of [`probe`], with the same commands, limits and stop, that also asks the tested
//! binary whether what the adapter sends for a compat suite's planned cases conforms to it. The App Server's requests,
//! its `initialized` notification and its headless answers are checked against the tested schema export as written,
//! `params` never opened, and so are the embedded notifications the adapter judges by; an argv is built by the
//! adapter's own builder from the tested help and checked against it. Each finding is three-valued: what cannot be
//! decided (the validator's depth, budget or time, a response schema the export lacks) is `Unknown`, never a yes.

use super::*;
use crate::adapter::{has_word, lists_command, offers, option_text};
use crate::compat::suite::{Finding, Part, Plan, Planned};
use serde_json::Value;

/// What the tested binary said of a plan, by case: each request conformance and frame acceptance finding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Conformance {
    cases: BTreeMap<String, Vec<Finding>>,
}

impl Conformance {
    /// The findings of `case` (none for a case the plan does not have).
    pub fn of(&self, case: &str) -> &[Finding] {
        self.cases.get(case).map_or(&[], Vec::as_slice)
    }

    fn push(&mut self, case: &str, finding: Finding) {
        self.cases.entry(case.to_owned()).or_default().push(finding);
    }

    /// `other`'s findings added to this one's.
    pub(super) fn merge(&mut self, other: Conformance) {
        for (case, findings) in other.cases {
            self.cases.entry(case).or_default().extend(findings);
        }
    }
}

/// Probe `agent` as [`probe`] does (the entry is the same) and check `plan` against the binary it found
/// (docs/adr/ADR-030.md decision 10): with the same commands, deadlines and stop (`env.cancel`); on codex's exec,
/// `codex exec resume --help` too when a planned launch resumes (the product reads it for a resumed turn). A plan for
/// another transport than the probe found is not checked: each of its cases is undecided.
pub fn conform(
    agent: Agent,
    env: &ProbeEnv,
    plan: &Plan,
) -> Result<(AgentEntry, Conformance), ProbeError> {
    let (entry, mut conformance) = probe_planned(agent, identify(agent, env)?, env, Some(plan))?;
    let found = crate::compat::suite::suite_of(entry.transport);
    if found != plan.suite {
        conformance = Conformance::default();
        for case in &plan.cases {
            conformance.push(
                &case.case,
                Finding::new(
                    Part::Requests,
                    Truth::Unknown,
                    format!("the plan is {}'s, the probe found {found}", plan.suite),
                ),
            );
        }
    }
    Ok((entry, conformance))
}

/// The options HoH passes with a value of its own fixing (claude `--output-format stream-json`, pi `--mode rpc`, codex
/// `--listen stdio://`, the read-only profile's claude `--permission-mode dontAsk`): the tested help must list the value
/// in the option's own text.
const FIXED_VALUE_OPTIONS: [&str; 4] =
    ["--output-format", "--mode", "--listen", "--permission-mode"];

/// One finding per planned launch, `judge` giving it.
fn launches(plan: &Plan, mut judge: impl FnMut(&Planned) -> Finding) -> Conformance {
    let mut conformance = Conformance::default();
    for case in &plan.cases {
        for planned in &case.launches {
            conformance.push(&case.case, judge(planned));
        }
    }
    conformance
}

/// Whether the argv a builder gave (or its refusal) conforms to the tested helps: each leading subcommand of `commands`
/// (in order, each with the help that must list it) listed in that help's Commands column ([`lists_command`]); each
/// option declared by `help` ([`offers`]); the value of each of [`FIXED_VALUE_OPTIONS`] listed in its option's text
/// ([`option_text`], [`has_word`]). Values and positional arguments (`-` among them) are HoH's own. A refusal is
/// non-conformance.
fn judge_argv(
    label: &str,
    variant: &str,
    built: Result<Vec<String>, String>,
    commands: &[(&str, &str)],
    help: &str,
) -> Finding {
    let args = match built {
        Ok(args) => args,
        Err(why) => {
            return Finding::new(
                Part::Requests,
                Truth::No,
                format!(
                    "{label} argv ({variant}): the adapter's builder refuses the tested help: {why}"
                ),
            );
        }
    };
    let mut problems = Vec::new();
    let mut at = 0;
    for (name, listing) in commands {
        if args.get(at).is_some_and(|arg| arg == name) {
            if !lists_command(listing, name) {
                problems.push(format!("the tested help lists no subcommand {name}"));
            }
            at += 1;
        }
    }
    while let Some(arg) = args.get(at) {
        at += 1;
        if !arg.starts_with('-') || arg == "-" {
            continue;
        }
        if !offers(help, arg) {
            problems.push(format!("the tested help declares no {arg}"));
            continue;
        }
        if FIXED_VALUE_OPTIONS.contains(&arg.as_str()) {
            let value = args.get(at).map(String::as_str).unwrap_or_default();
            at += 1;
            if !option_text(help, arg).is_some_and(|text| has_word(&text, value)) {
                problems.push(format!("the tested help lists no {value:?} for {arg}"));
            }
        }
    }
    let shown = args.join(" ");
    if problems.is_empty() {
        Finding::new(
            Part::Requests,
            Truth::Yes,
            format!("{label} argv ({variant}) `{shown}`: conforms to the tested help"),
        )
    } else {
        Finding::new(
            Part::Requests,
            Truth::No,
            format!(
                "{label} argv ({variant}) `{shown}`: {}",
                problems.join("; ")
            ),
        )
    }
}

/// claude's or pi's planned launches (`suite`), each argv built by the adapter's builder from the tested `help`.
pub(super) fn argv(plan: &Plan, suite: &str, help: &str) -> Conformance {
    launches(plan, |planned| {
        let built = crate::compat::argv(suite, planned.launch, help, None);
        judge_argv(suite, &planned.variant, built, &[], help)
    })
}

/// codex exec's planned launches, each argv built by `codex::exec_args` from the tested `exec --help` (`exec_help`)
/// and, for a resumed one, `exec resume --help`, read here as the product reads it (`crate::launch::read`): one that
/// cannot be read is none, so the builder refuses the launch; one that did not answer in time cannot tell. `exec` must
/// be listed by `codex --help` (`top`), `resume` by `exec --help`.
pub(super) fn exec(
    plan: &Plan,
    launcher: &Path,
    env: &ProbeEnv,
    top: &str,
    exec_help: &str,
) -> Result<Conformance, ProbeError> {
    let resumes = plan
        .cases
        .iter()
        .flat_map(|case| &case.launches)
        .any(|planned| planned.launch.resume);
    let resume_help: Result<Option<String>, String> = if resumes {
        match run_ok(launcher, &["exec", "resume", "--help"], env) {
            Ok(out) => Ok(Some(String::from_utf8_lossy(&out.stdout).into_owned())),
            Err(e) if cancelled(env) => return Err(e),
            Err(e @ (ProbeError::TimedOut { .. } | ProbeError::NotStarted { .. })) => {
                Err(e.to_string())
            }
            Err(_) => Ok(None),
        }
    } else {
        Ok(None)
    };
    Ok(launches(plan, |planned| {
        let resume = match (&resume_help, planned.launch.resume) {
            (Err(why), true) => {
                return Finding::new(
                    Part::Requests,
                    Truth::Unknown,
                    format!("codex exec argv ({}): {why}", planned.variant),
                );
            }
            (Ok(help), _) => help.as_deref(),
            (Err(_), false) => None,
        };
        let built = crate::compat::argv("codex-exec", planned.launch, exec_help, resume);
        let options = match planned.launch.resume {
            true => resume.unwrap_or_default(),
            false => exec_help,
        };
        judge_argv(
            "codex exec",
            &planned.variant,
            built,
            &[("exec", top), ("resume", exec_help)],
            options,
        )
    }))
}

/// The App Server's launch (`codex::app_server_args` from the tested `app-server --help`), the same for every planned
/// launch: `app-server` listed by `codex --help` (`top`), `--listen stdio://` by its help.
pub(super) fn app_server_launch(plan: &Plan, top: &str, app_help: &str) -> Conformance {
    let built = crate::codex::app_server_args(app_help).map_err(|crate::codex::Blocked(why)| why);
    let mut conformance = Conformance::default();
    for case in &plan.cases {
        conformance.push(
            &case.case,
            judge_argv(
                "codex app-server",
                "every launch",
                built.clone(),
                &[("app-server", top)],
                app_help,
            ),
        );
    }
    conformance
}

/// The file of the schema export that describes the `result` of the answer to the server request `method`
/// ([`crate::compat::TOOL_SERVER_REQUESTS`]).
fn response_file(method: &str) -> Option<&'static str> {
    crate::compat::TOOL_SERVER_REQUESTS
        .iter()
        .find(|(m, _)| *m == method)
        .map(|(_, file)| *file)
}

/// The headless answers of `plan`'s launches, by server request method.
fn answers(plan: &Plan) -> impl Iterator<Item = (&String, &Value)> {
    plan.cases
        .iter()
        .flat_map(|case| &case.launches)
        .filter_map(|planned| planned.built["answers"].as_object())
        .flatten()
}

/// The response files of the schema export `plan`'s answers are checked against: the only files a reading parses
/// besides the three protocol files.
pub(super) fn needed_responses(plan: &Plan) -> std::collections::BTreeSet<String> {
    answers(plan)
        .filter_map(|(method, _)| response_file(method))
        .map(str::to_owned)
        .collect()
}

/// The parsed files of a schema export a plan is checked against: the three protocol files and the response files it
/// needs (each with its text, or why it cannot be judged).
pub(super) struct Export<'a> {
    pub client_requests: &'a Value,
    pub client_notifications: &'a Value,
    pub server_notifications: &'a Value,
    pub responses: &'a BTreeMap<String, Result<(Value, &'a str), String>>,
}

/// How a three-valued check reads.
fn said(truth: Truth) -> &'static str {
    match truth {
        Truth::Yes => "valid under",
        Truth::No => "not valid under",
        Truth::Unknown => "undecided (the validator's depth, work or time ran out) under",
    }
}

/// A response file the probe can judge with: well formed as the protocol files must be, every number read exactly.
fn well_formed(value: &Value, text: &str, work: &Work) -> Result<(), String> {
    draft7_well_formed(value, work).map_err(|e| format!("it is not a well-formed schema: {e}"))?;
    match inexact_number(text, work) {
        Ok(None) => Ok(()),
        Ok(Some(number)) => Err(format!(
            "it has a number the probe cannot read exactly: {number}"
        )),
        Err(()) => Err("it was not read within the time limit".to_owned()),
    }
}

/// What the tested schema `export` says of `plan` (the App Server): each planned launch's messages, a request (it has
/// an `id`) against `ClientRequest.json` and `initialized` against `ClientNotification.json`, `turn/steer` only where
/// the export lists it (the adapter sends it only then); each headless answer's `result` against its response file;
/// each planned notification against `ServerNotification.json`. Whole messages, `params` never opened, by the
/// probe's validator ([`valid`]) within `work`; a message judged once per file.
pub(super) fn schema(plan: &Plan, export: Export<'_>, work: &Work) -> Conformance {
    let steer = crate::codex::STEER_REQUEST;
    let steer_listed = crate::codex::listed_methods(export.client_requests).contains(steer);
    let mut judged: std::collections::HashMap<(&str, Vec<u8>), Truth> =
        std::collections::HashMap::new();
    let mut judge = |file: &'static str, schema: &Value, instance: &Value| -> Truth {
        *judged
            .entry((file, crate::compat::canonical(instance)))
            .or_insert_with(|| frame_valid(Some(schema), instance, work))
    };
    let responses: BTreeMap<&str, Result<&Value, String>> = export
        .responses
        .iter()
        .map(|(name, read)| {
            let usable = match read {
                Ok((value, text)) => well_formed(value, text, work).map(|()| value),
                Err(why) => Err(why.clone()),
            };
            (name.as_str(), usable)
        })
        .collect();
    let mut conformance = Conformance::default();
    for case in &plan.cases {
        let name = case.case.as_str();
        for planned in &case.launches {
            let variant = &planned.variant;
            let Some(messages) = planned.built["messages"].as_array() else {
                conformance.push(
                    name,
                    Finding::new(
                        Part::Requests,
                        Truth::Unknown,
                        format!("({variant}): the plan has no App Server messages"),
                    ),
                );
                continue;
            };
            for message in messages {
                let method = message["method"].as_str().unwrap_or("(no method)");
                if method == steer && !steer_listed {
                    conformance.push(
                        name,
                        Finding::new(
                            Part::Requests,
                            Truth::Yes,
                            format!(
                                "{method} ({variant}): not sent, the tested ClientRequest.json lists none"
                            ),
                        ),
                    );
                    continue;
                }
                let (file, schema) = match message.get("id") {
                    Some(_) => ("ClientRequest.json", export.client_requests),
                    None => ("ClientNotification.json", export.client_notifications),
                };
                let truth = if method == crate::codex::RESUME_REQUEST
                    && !declares_exclude_turns(schema, work)
                {
                    Truth::No
                } else {
                    judge(file, schema, message)
                };
                conformance.push(
                    name,
                    Finding::new(
                        Part::Requests,
                        truth,
                        format!("{method} ({variant}): {} the tested {file}", said(truth)),
                    ),
                );
            }
            for (method, answer) in planned.built["answers"].as_object().into_iter().flatten() {
                let (truth, why) = match (response_file(method), answer.get("result")) {
                    (None, _) => (
                        Truth::Unknown,
                        "no response file is known for it".to_owned(),
                    ),
                    (Some(_), None) => (
                        Truth::Unknown,
                        "an error answer is not checked against a response schema".to_owned(),
                    ),
                    (Some(file), Some(result)) => match responses.get(file) {
                        None => (Truth::Unknown, format!("the export has no {file}")),
                        Some(Err(why)) => (Truth::Unknown, format!("{file} cannot judge: {why}")),
                        Some(Ok(schema)) => {
                            let truth = judge(file, schema, result);
                            (truth, format!("{} the tested {file}", said(truth)))
                        }
                    },
                };
                conformance.push(
                    name,
                    Finding::new(
                        Part::Requests,
                        truth,
                        format!("{method} answer ({variant}): {why}"),
                    ),
                );
            }
        }
        for (n, frame) in case.frames.iter().enumerate() {
            let method = frame["method"].as_str().unwrap_or("(no method)");
            let truth = judge(
                "ServerNotification.json",
                export.server_notifications,
                frame,
            );
            conformance.push(
                name,
                Finding::new(
                    Part::Frames,
                    truth,
                    format!(
                        "{method} (notification {} of the embedded {name}): {} the tested ServerNotification.json",
                        n + 1,
                        said(truth)
                    ),
                ),
            );
        }
    }
    conformance
}
