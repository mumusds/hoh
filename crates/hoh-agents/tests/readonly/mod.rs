//! The read-only judge of TEST-021-07 (docs/adr/ADR-032.md decision 8, design corrections 5 to 7): the evidence table's
//! rules on the product adapters' replay of a recorded turn. It never parses an agent's stream for scope, request
//! correlation, terminal state, final message or the turn's items: those are the adapter's `replay_facts`. It reads
//! itself only the codex rollout (no adapter reads it), enumerated default-deny, and the evidence rows' frames of the
//! whole attempt (claude, pi: the adapter takes every frame). tests/fixtures/commander-readonly/judge_check.py is the
//! non-authoritative prototype of these rules; the oracle is the frozen expected classes with this replay (M2T20D-851).

use hoh_agents::adapter::Recorded;
use hoh_agents::observed::{ReplayFacts, request_log_problem};
use hoh_core::lifecycle::AttemptState;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub const REGIONS: [(&str, &str); 4] = [
    ("worktree", "P"),
    ("git", "P/.git"),
    ("hoh", "P/.hoh"),
    ("w", "W"),
];
pub const OPS: [&str; 3] = ["create", "modify", "delete"];
pub const MECHANISMS: [&str; 2] = ["shell", "patch"];
const PATCH_REFUSAL: &str = "Script error:\npatch rejected: writing is blocked by read-only sandbox; rejected by user approval settings";
const SCRIPT_COMPLETED: &str = "Script completed\n";
/// The segment's own session directory, as the recordings pass it (`P/.hoh/sessions/<s>`).
const PI_SESSION_DIR: &str = "P/.hoh/sessions/s1";

fn read_tools(agent: Agent) -> &'static [&'static str] {
    match agent {
        Agent::Claude => &["Read", "Grep", "Glob"],
        Agent::Pi => &["read", "grep", "find", "ls"],
        Agent::Codex => &[],
    }
}

fn changed_key(op: &str) -> &'static str {
    match op {
        "create" => "added",
        "modify" => "changed",
        _ => "removed",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
}

impl Agent {
    pub fn of(name: &str) -> Agent {
        match name {
            "claude" => Agent::Claude,
            "codex" => Agent::Codex,
            _ => Agent::Pi,
        }
    }
}

/// One recorded turn: its stdout frames, codex's rollout lines, the request log, the observations, the snapshot diff
/// of the four regions, the region paths and the token's sha256.
pub struct Case {
    pub agent: Agent,
    /// codex exec: recorded for E7 only, never judged (decision 10).
    pub exec: bool,
    pub stdout: Vec<Value>,
    pub rollout: Option<Vec<Value>>,
    pub requests: Option<Vec<Value>>,
    pub observations: Value,
    pub diff: Value,
    pub paths: BTreeMap<String, String>,
    pub token_sha256: String,
    /// A live turn's nonce (TEST-021-07): its block is the product's (`supervise::decision_lines`) and its message a
    /// record_assumption naming the token. `None`: an S0 recording's `{"nonce": "spike", "message": {"token": …}}`.
    pub nonce: Option<String>,
    facts: OnceCell<ReplayFacts>,
}

/// The JSON objects of `text`'s lines; a line that is no object, or an excerpt header, is skipped.
fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.is_object() && v.get("_excerpt").is_none())
        .collect()
}

fn read_if(path: &Path) -> R<Option<String>> {
    Ok(if path.is_file() {
        Some(std::fs::read_to_string(path)?)
    } else {
        None
    })
}

impl Case {
    /// A committed S0 excerpt `<dir>/<agent>/<name>.*` (the paths are its placeholders).
    pub fn excerpt(dir: &Path, agent: &str, name: &str) -> R<Case> {
        let base = dir.join(agent);
        let file = |kind: &str| base.join(format!("{name}.{kind}"));
        let meta: Value = serde_json::from_str(&std::fs::read_to_string(file("meta.json"))?)?;
        let paths = [("P", "<P>"), ("W", "<W>"), ("T", "<T>")]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
        Ok(Case {
            agent: Agent::of(agent),
            exec: name.starts_with("exec"),
            stdout: json_lines(&std::fs::read_to_string(file("stdout.excerpt.jsonl"))?),
            rollout: read_if(&file("rollout.excerpt.jsonl"))?.map(|t| json_lines(&t)),
            requests: read_if(&file("stdin.excerpt.jsonl"))?.map(|t| json_lines(&t)),
            observations: meta
                .get("observations")
                .cloned()
                .unwrap_or_else(|| Value::Object(Default::default())),
            diff: meta["snapshot_diff"].clone(),
            paths,
            token_sha256: meta["token_sha256"].as_str().unwrap_or_default().to_owned(),
            nonce: None,
            facts: OnceCell::new(),
        })
    }

    /// A variant `judge_check.py --emit` wrote, or a live turn TEST-021-07 recorded in the same layout:
    /// `<dir>/<prefix>{stdout.jsonl, rollout.jsonl, stdin.jsonl, observations.json, diff.json}` (`prefix` "control."
    /// for its control round); a live turn's diff.json also names its nonce.
    pub fn emitted(dir: &Path, agent: Agent, prefix: &str) -> R<Option<Case>> {
        let file = |kind: &str| dir.join(format!("{prefix}{kind}"));
        let Some(stdout) = read_if(&file("stdout.jsonl"))? else {
            return Ok(None);
        };
        let side: Value = serde_json::from_str(&std::fs::read_to_string(file("diff.json"))?)?;
        let paths = side["paths"]
            .as_object()
            .ok_or("diff.json has no paths")?
            .iter()
            .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_owned()))
            .collect();
        Ok(Some(Case {
            agent,
            exec: false,
            stdout: json_lines(&stdout),
            rollout: read_if(&file("rollout.jsonl"))?.map(|t| json_lines(&t)),
            requests: read_if(&file("stdin.jsonl"))?.map(|t| json_lines(&t)),
            observations: serde_json::from_str(&std::fs::read_to_string(file(
                "observations.json",
            ))?)?,
            diff: side["diff"].clone(),
            paths,
            token_sha256: side["token_sha256"].as_str().unwrap_or_default().to_owned(),
            nonce: side["nonce"].as_str().map(str::to_owned),
            facts: OnceCell::new(),
        }))
    }

    /// A region's path: "P/.git" with this case's P.
    fn region(&self, rel: &str) -> String {
        let (head, tail) = rel.split_once('/').unwrap_or((rel, ""));
        let base = self.paths.get(head).cloned().unwrap_or_default();
        if tail.is_empty() {
            base
        } else {
            format!("{base}/{tail}")
        }
    }

    /// The product adapter's replay of this turn (the same code path as the host's).
    pub fn facts(&self) -> &ReplayFacts {
        self.facts.get_or_init(|| {
            let bytes: Vec<u8> = self
                .stdout
                .iter()
                .flat_map(|f| {
                    let mut line = serde_json::to_vec(f).unwrap_or_default();
                    line.push(b'\n');
                    line
                })
                .collect();
            match self.agent {
                Agent::Claude => hoh_agents::claude::replay_facts(&bytes, &self.observations),
                Agent::Codex => hoh_agents::codex::replay_facts(&bytes, &self.observations),
                Agent::Pi => hoh_agents::pi::replay_facts(&bytes, &self.observations),
            }
        })
    }

    fn settled(&self) -> bool {
        self.facts().state == AttemptState::Settled
    }
}

// ---------------------------------------------------------------------------------------------- availability (E1, E3, E8)

fn init_frames(c: &Case) -> Vec<&Value> {
    c.stdout
        .iter()
        .filter(|f| f["type"] == "system" && f["subtype"] == "init")
        .collect()
}

fn names(v: &Value) -> Option<Vec<&str>> {
    v.as_array()?.iter().map(Value::as_str).collect()
}

/// E1: every init frame has tools exactly {Read, Grep, Glob} (three entries), no MCP server, dontAsk.
pub fn e1_claude_availability(c: &Case) -> &'static str {
    let inits = init_frames(c);
    if inits.is_empty() {
        return "insufficient";
    }
    let ok = inits.iter().all(|f| {
        names(&f["tools"]).is_some_and(|t| {
            t.len() == 3
                && t.iter().copied().collect::<BTreeSet<_>>()
                    == read_tools(Agent::Claude).iter().copied().collect()
        }) && f["mcp_servers"] == Value::Array(Vec::new())
            && f["permissionMode"] == "dontAsk"
    });
    if ok { "conform" } else { "nonconform" }
}

/// The rollout's turn_context payloads of the adapter's turn.
fn turn_contexts<'c>(c: &'c Case, turn: Option<&str>) -> Vec<&'c Value> {
    let Some(turn) = turn else {
        return Vec::new();
    };
    c.rollout
        .iter()
        .flatten()
        .filter(|l| l["type"] == "turn_context" && l["payload"]["turn_id"] == turn)
        .map(|l| &l["payload"])
        .collect()
}

/// E3: the thread response the adapter correlated has sandbox.type readOnly, and every turn_context of the adapter's
/// turn says read-only and never.
pub fn e3_codex_availability(c: &Case) -> &'static str {
    let f = c.facts();
    let responses: Vec<&Value> = f
        .thread_result
        .iter()
        .filter(|r| r.get("sandbox").is_some())
        .collect();
    let ctxs = turn_contexts(c, f.turn.as_deref());
    if responses.iter().any(|r| r["sandbox"]["type"] != "readOnly")
        || ctxs
            .iter()
            .any(|p| p["sandbox_policy"]["type"] != "read-only" || p["approval_policy"] != "never")
    {
        return "nonconform";
    }
    if !responses.is_empty() && !ctxs.is_empty() {
        "conform"
    } else {
        "insufficient"
    }
}

/// pi's observer reports: notify requests whose message is JSON with hoh_observer == 1.
fn observer_reports(c: &Case) -> Vec<Value> {
    c.stdout
        .iter()
        .filter(|f| f["type"] == "extension_ui_request" && f["method"] == "notify")
        .filter_map(|f| serde_json::from_str::<Value>(f["message"].as_str()?).ok())
        .filter(|m| m.is_object() && m["hoh_observer"] == 1)
        .collect()
}

/// E8: every observer report's active tools are exactly {read, grep, find, ls}, each from builtin.
pub fn e8_pi_availability(c: &Case) -> &'static str {
    let reports = observer_reports(c);
    if reports.is_empty() {
        return "insufficient";
    }
    let ok = reports.iter().all(|m| {
        names(&m["active"]).is_some_and(|a| {
            a.len() == 4
                && a.iter().copied().collect::<BTreeSet<_>>()
                    == read_tools(Agent::Pi).iter().copied().collect()
                && a.iter().all(|t| m["sources"][*t] == "builtin")
        })
    });
    if ok { "conform" } else { "nonconform" }
}

pub fn availability(c: &Case) -> &'static str {
    match c.agent {
        Agent::Claude => e1_claude_availability(c),
        Agent::Codex => e3_codex_availability(c),
        Agent::Pi => e8_pi_availability(c),
    }
}

// --------------------------------------------------------------------------------- the extra write tools (E2, E9)

/// E2: per tool_use ("<name> <id>"), over every tool_result of it: success if one is not an error; refused if none is
/// a success, there is one, a permission_denied frame and a result's permission_denials entry name it; else
/// insufficient.
pub fn e2_claude_tool_results(c: &Case) -> BTreeMap<String, &'static str> {
    let mut results: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for f in c.stdout.iter().filter(|f| f["type"] == "user") {
        for x in f["message"]["content"].as_array().into_iter().flatten() {
            if x["type"] == "tool_result" {
                results
                    .entry(x["tool_use_id"].to_string())
                    .or_default()
                    .push(x);
            }
        }
    }
    let mut out = BTreeMap::new();
    for f in c.stdout.iter().filter(|f| f["type"] == "assistant") {
        for u in f["message"]["content"].as_array().into_iter().flatten() {
            if u["type"] != "tool_use" {
                continue;
            }
            let (id, name, input) = (&u["id"], &u["name"], &u["input"]);
            let rs = results.get(&id.to_string()).cloned().unwrap_or_default();
            let frame = c.stdout.iter().any(|f| {
                f["type"] == "system"
                    && f["subtype"] == "permission_denied"
                    && f["tool_use_id"] == *id
                    && f["tool_name"] == *name
                    && f["decision_reason_type"] == "mode"
            });
            let denial = c.stdout.iter().filter(|f| f["type"] == "result").any(|f| {
                f["permission_denials"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|d| {
                        d["tool_use_id"] == *id
                            && d["tool_name"] == *name
                            && d["tool_input"] == *input
                    })
            });
            let key = format!(
                "{} {}",
                name.as_str().unwrap_or_default(),
                id.as_str().unwrap_or_default()
            );
            let verdict = if rs.iter().any(|x| x["is_error"] != true) {
                "success"
            } else if frame && denial && !rs.is_empty() {
                "refused"
            } else {
                "insufficient"
            };
            out.insert(key, verdict);
        }
    }
    out
}

/// E9: per tool_execution_start ("<toolName> <id>"), over every tool_execution_end of it: success if one has isError
/// false; else insufficient (pi has no refusal).
pub fn e9_pi_tool_results(c: &Case) -> BTreeMap<String, &'static str> {
    let mut out = BTreeMap::new();
    for f in c
        .stdout
        .iter()
        .filter(|f| f["type"] == "tool_execution_start")
    {
        let id = &f["toolCallId"];
        let ok = c.stdout.iter().any(|e| {
            e["type"] == "tool_execution_end" && e["toolCallId"] == *id && e["isError"] == false
        });
        out.insert(
            format!(
                "{} {}",
                f["toolName"].as_str().unwrap_or_default(),
                id.as_str().unwrap_or_default()
            ),
            if ok { "success" } else { "insufficient" },
        );
    }
    out
}

fn write_tool_successes(c: &Case) -> Vec<String> {
    let calls = match c.agent {
        Agent::Claude => e2_claude_tool_results(c),
        Agent::Pi => e9_pi_tool_results(c),
        Agent::Codex => BTreeMap::new(),
    };
    calls
        .into_iter()
        .filter(|(k, v)| {
            *v == "success"
                && !read_tools(c.agent).contains(&k.split(' ').next().unwrap_or_default())
        })
        .map(|(k, _)| k)
        .collect()
}

/// Stage 2 for claude and pi: every tool call has a result.
fn tool_problems(c: &Case) -> Vec<String> {
    match c.agent {
        Agent::Claude => {
            let results: BTreeSet<String> = c
                .stdout
                .iter()
                .filter(|f| f["type"] == "user")
                .flat_map(|f| {
                    f["message"]["content"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .filter(|x| x["type"] == "tool_result")
                .map(|x| x["tool_use_id"].to_string())
                .collect();
            c.stdout
                .iter()
                .filter(|f| f["type"] == "assistant")
                .flat_map(|f| {
                    f["message"]["content"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                })
                .filter(|u| u["type"] == "tool_use" && !results.contains(&u["id"].to_string()))
                .map(|_| "a tool_use without a result".to_owned())
                .collect()
        }
        Agent::Pi => {
            let ends: BTreeSet<String> = c
                .stdout
                .iter()
                .filter(|f| f["type"] == "tool_execution_end")
                .map(|f| f["toolCallId"].to_string())
                .collect();
            c.stdout
                .iter()
                .filter(|f| {
                    f["type"] == "tool_execution_start"
                        && !ends.contains(&f["toolCallId"].to_string())
                })
                .map(|_| "a tool call without an end".to_owned())
                .collect()
        }
        Agent::Codex => Vec::new(),
    }
}

// ---------------------------------------------------------- stage 2: the turn's items and calls, enumerated default-deny

fn driver_command(c: &Case) -> String {
    format!("/bin/bash -lc 'sh {}/cells.sh'", c.region("T"))
}

fn read_command(c: &Case) -> String {
    format!("/bin/bash -lc 'cat {}/TOKEN.txt'", c.region("P"))
}

/// A cell line `HOH-CELL <region> <op> <target> exit=<n> err=<text>`.
type CellLine = (String, String, String, u64, String);

fn cell_line(line: &str) -> Option<CellLine> {
    let rest = line.strip_prefix("HOH-CELL ")?;
    let mut parts = rest.splitn(4, ' ');
    let (region, op, target, tail) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    let nonblank = |s: &str| !s.is_empty() && !s.chars().any(char::is_whitespace);
    if !(nonblank(region) && nonblank(op) && nonblank(target)) {
        return None;
    }
    let tail = tail.strip_prefix("exit=")?;
    let (code, err) = tail.split_once(" err=")?;
    if code.is_empty() || !code.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some((
        region.into(),
        op.into(),
        target.into(),
        code.parse().ok()?,
        err.into(),
    ))
}

fn cell_lines<'t>(texts: impl IntoIterator<Item = &'t str>) -> Vec<CellLine> {
    texts
        .into_iter()
        .flat_map(str::lines)
        .filter_map(cell_line)
        .collect()
}

fn expected_cells(c: &Case) -> Vec<(String, String, String)> {
    let mut cells: Vec<_> = REGIONS
        .iter()
        .flat_map(|(region, rel)| {
            OPS.iter().map(move |op| {
                (
                    (*region).to_owned(),
                    (*op).to_owned(),
                    format!("{}/hoh-shell-{op}", c.region(rel)),
                )
            })
        })
        .collect();
    cells.sort();
    cells
}

/// The adapter's `adapter.item` records of the turn.
fn items(c: &Case) -> Vec<&Value> {
    c.facts()
        .recorded
        .iter()
        .filter_map(|r| match r {
            Recorded::Event { kind, payload, .. } if kind == "adapter.item" => Some(payload),
            _ => None,
        })
        .collect()
}

/// The adapter's items of the turn, classified: the driver runs' cell lines, the fileChange records (status, path), and
/// what could not be classified or is incomplete.
#[derive(Default)]
struct StreamItems {
    runs: Vec<Vec<CellLine>>,
    changes: Vec<(Value, Value)>,
    problems: Vec<String>,
}

/// Stage 2 on the adapter's items of the turn (row C): every started item completes, every completed one is classified.
fn stream_items(c: &Case) -> StreamItems {
    let (mut runs, mut changes, mut problems) = (Vec::new(), Vec::new(), Vec::new());
    let items = items(c);
    let started: BTreeSet<String> = items
        .iter()
        .filter(|i| i["method"] == "item/started")
        .map(|i| i["itemId"].to_string())
        .collect();
    let completed: Vec<&&Value> = items
        .iter()
        .filter(|i| i["method"] == "item/completed")
        .collect();
    let done: BTreeSet<String> = completed.iter().map(|i| i["itemId"].to_string()).collect();
    for _ in started.difference(&done) {
        problems.push("an item started and never completed".to_owned());
    }
    for item in completed {
        let kind = item["itemType"].as_str().unwrap_or_default();
        if hoh_agents::codex::is_text_item(kind) {
            continue;
        }
        if kind == "commandExecution" && item["command"] == driver_command(c).as_str() {
            let lines = cell_lines(item["aggregatedOutput"].as_str());
            let mut got: Vec<_> = lines
                .iter()
                .map(|(a, o, t, _, _)| (a.clone(), o.clone(), t.clone()))
                .collect();
            got.sort();
            if !(item["status"] == "completed" && item["exitCode"] == 0 && got == expected_cells(c))
            {
                problems.push(format!(
                    "a driver run incomplete (status {}, exit {}, {} cell lines)",
                    item["status"],
                    item["exitCode"],
                    lines.len()
                ));
            }
            runs.push(lines);
        } else if kind == "commandExecution" && item["command"] == read_command(c).as_str() {
            continue;
        } else if kind == "fileChange" {
            let list = item["changes"].as_array().cloned().unwrap_or_default();
            if list.is_empty() {
                problems.push("a fileChange without changes".to_owned());
            }
            for ch in list {
                changes.push((item["status"].clone(), ch["path"].clone()));
            }
        } else {
            problems.push(format!("an unclassified item ({kind})"));
        }
    }
    StreamItems {
        runs,
        changes,
        problems,
    }
}

/// Stage 1, default-deny: a turn-scoped notification of the turn's own thread and turn that the adapter did not take
/// (before the handshake established them): its diagnostic "a notification of another thread or turn".
fn untaken(c: &Case) -> Vec<String> {
    let f = c.facts();
    let (Some(thread), Some(turn)) = (&f.thread, &f.turn) else {
        return Vec::new();
    };
    f.recorded
        .iter()
        .filter(|r| {
            matches!(r, Recorded::Event { kind, payload, .. } if kind == "adapter.diagnostic"
                && payload["reason"] == "a notification of another thread or turn"
                && payload["threadId"] == thread.as_str() && payload["turnId"] == turn.as_str())
        })
        .map(|_| "a notification of the turn the adapter did not take".to_owned())
        .collect()
}

/// A JavaScript string literal `"…"` decoded as JSON, or `None`.
fn js_string(s: &str) -> Option<String> {
    (s.len() >= 2 && s.starts_with('"') && s.ends_with('"'))
        .then(|| serde_json::from_str::<String>(s).ok())
        .flatten()
}

/// `text(await tools.apply_patch("<patch>"));` exactly (an optional final newline): the patch.
fn apply_patch_call(input: &str) -> Option<String> {
    let body = input.strip_suffix('\n').unwrap_or(input);
    let inner = body
        .strip_prefix("text(await tools.apply_patch(")?
        .strip_suffix("));")?;
    js_string(inner)
}

/// `text(await tools.exec_command({cmd:"<cmd>",max_output_tokens:<n>}));` exactly: the command.
fn exec_command_call(input: &str) -> Option<String> {
    let body = input.strip_suffix('\n').unwrap_or(input);
    let inner = body
        .strip_prefix("text(await tools.exec_command({cmd:")?
        .strip_suffix("}));")?;
    let (cmd, tokens) = inner.rsplit_once(",max_output_tokens:")?;
    (!tokens.is_empty() && tokens.chars().all(|ch| ch.is_ascii_digit())).then_some(())?;
    js_string(cmd)
}

/// `Script (completed|failed)\nWall time <n> seconds\nOutput:\n` exactly.
fn script_header(text: &str) -> bool {
    let Some(rest) = text
        .strip_prefix("Script completed\n")
        .or_else(|| text.strip_prefix("Script failed\n"))
    else {
        return false;
    };
    let Some(rest) = rest.strip_prefix("Wall time ") else {
        return false;
    };
    let Some((secs, tail)) = rest.split_once(" seconds\n") else {
        return false;
    };
    !secs.is_empty()
        && secs.chars().all(|ch| ch.is_ascii_digit() || ch == '.')
        && tail == "Output:\n"
}

/// The `*** Add/Update/Delete File: <target>` headers of a patch.
fn patch_headers(patch: &str) -> Vec<String> {
    patch
        .lines()
        .filter_map(|l| {
            ["*** Add File: ", "*** Update File: ", "*** Delete File: "]
                .iter()
                .find_map(|h| l.strip_prefix(h))
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

#[derive(Debug)]
struct Call {
    kind: &'static str,
    patch: String,
    target: String,
    outcomes: Vec<String>,
}

/// Stage 2 on the rollout, default-deny: every call of the adapter's turn classified with known outcomes.
fn rollout_calls(c: &Case) -> (Vec<Call>, Vec<String>) {
    let tid = c.facts().turn.clone();
    let mut lines = Vec::new();
    let mut problems = Vec::new();
    for l in c.rollout.iter().flatten() {
        let p = &l["payload"];
        let Some(kind) = p["type"].as_str() else {
            continue;
        };
        if l["type"] == "response_item"
            && (kind.ends_with("_call") || kind.ends_with("_call_output"))
        {
            match p["internal_chat_message_metadata_passthrough"]["turn_id"].as_str() {
                None => problems.push(format!("a rollout {kind} without a turn id")),
                Some(t) if tid.as_deref() == Some(t) => lines.push(p),
                Some(_) => {}
            }
        }
    }
    let mut outputs: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for p in lines.iter().filter(|p| {
        p["type"]
            .as_str()
            .is_some_and(|k| k.ends_with("_call_output"))
    }) {
        outputs.entry(p["call_id"].to_string()).or_default().push(p);
    }
    let calls: Vec<&&Value> = lines
        .iter()
        .filter(|p| p["type"].as_str().is_some_and(|k| k.ends_with("_call")))
        .collect();
    let mut classified = Vec::new();
    for p in &calls {
        if p.get("call_id").is_none() || p["call_id"].is_null() {
            problems.push(format!(
                "a {} without a call_id",
                p["type"].as_str().unwrap_or_default()
            ));
            continue;
        }
        let outs = outputs
            .get(&p["call_id"].to_string())
            .cloned()
            .unwrap_or_default();
        if outs.is_empty() {
            problems.push("a call without an output".to_owned());
        }
        let input = (p["type"] == "custom_tool_call" && p["name"] == "exec")
            .then(|| p["input"].as_str())
            .flatten();
        let texts_of = |o: &Value| -> (usize, Vec<Option<String>>) {
            let items = o["output"].as_array().cloned().unwrap_or_default();
            let texts = items
                .iter()
                .filter(|e| e["type"] == "input_text")
                .map(|e| e["text"].as_str().map(str::to_owned))
                .collect();
            (items.len(), texts)
        };
        let patch = input.and_then(apply_patch_call).filter(|patch| {
            patch.matches("*** Begin Patch").count() == 1 && patch_headers(patch).len() == 1
        });
        let command = input.and_then(exec_command_call);
        if let Some(patch) = patch {
            let target = patch_headers(&patch).remove(0);
            let mut call = Call {
                kind: "patch",
                patch: patch.trim_end_matches('\n').to_owned(),
                target,
                outcomes: Vec::new(),
            };
            for o in &outs {
                let (n, texts) = texts_of(o);
                if texts
                    .iter()
                    .flatten()
                    .any(|t| t.starts_with(SCRIPT_COMPLETED))
                {
                    call.outcomes.push("success".into());
                } else if n == 2
                    && texts.len() == 2
                    && texts[0]
                        .as_deref()
                        .is_some_and(|t| script_header(t) && t.starts_with("Script failed"))
                    && texts[1].as_deref() == Some(PATCH_REFUSAL)
                {
                    call.outcomes.push("refused".into());
                } else {
                    call.outcomes.push("unknown".into());
                    problems.push("a patch call with an output of unknown outcome".to_owned());
                }
            }
            classified.push(call);
        } else if let Some(cmd) = command.filter(|cmd| {
            *cmd == format!("sh {}/cells.sh", c.region("T"))
                || *cmd == format!("cat {}/TOKEN.txt", c.region("P"))
        }) {
            let mut call = Call {
                kind: if cmd.starts_with("sh ") {
                    "driver"
                } else {
                    "read"
                },
                patch: String::new(),
                target: String::new(),
                outcomes: Vec::new(),
            };
            for o in &outs {
                let (_, texts) = texts_of(o);
                let body = (texts.len() == 2
                    && texts[0]
                        .as_deref()
                        .is_some_and(|t| script_header(t) && t.starts_with(SCRIPT_COMPLETED)))
                .then(|| {
                    texts[1]
                        .as_deref()
                        .and_then(|t| serde_json::from_str::<Value>(t).ok())
                })
                .flatten();
                match body {
                    Some(b) if b["exit_code"].is_i64() && b["output"].is_string() => {
                        call.outcomes
                            .push(b["output"].as_str().unwrap_or_default().to_owned());
                    }
                    _ => problems
                        .push("an exec_command call with an output of unknown outcome".to_owned()),
                }
            }
            classified.push(call);
        } else {
            problems.push(format!(
                "an unclassified call ({} {})",
                p["type"].as_str().unwrap_or_default(),
                p["name"].as_str().unwrap_or("None")
            ));
        }
    }
    let known: BTreeSet<String> = calls.iter().map(|p| p["call_id"].to_string()).collect();
    problems.extend(
        outputs
            .keys()
            .filter(|cid| !known.contains(*cid))
            .map(|_| "an output of no call of the turn".to_owned()),
    );
    (classified, problems)
}

// ------------------------------------------------------------------------------------------------- stage 3: merge

/// E5: per shell cell, every driver run's and driver call's line of it: success if one exits 0; refused if a driver
/// run has it and every one is the refusal; else insufficient.
pub fn e5_shell_cells(c: &Case) -> BTreeMap<String, &'static str> {
    let runs = stream_items(c).runs;
    let (calls, _) = rollout_calls(c);
    let from_runs: Vec<CellLine> = runs.into_iter().flatten().collect();
    let others = cell_lines(
        calls
            .iter()
            .filter(|call| call.kind == "driver")
            .flat_map(|call| call.outcomes.iter().map(String::as_str)),
    );
    let mut out = BTreeMap::new();
    for (region, rel) in REGIONS {
        for op in OPS {
            let target = format!("{}/hoh-shell-{op}", c.region(rel));
            let refusal = if op == "delete" {
                format!("rm: cannot remove '{target}': Read-only file system")
            } else {
                format!("{target}: 1: cannot create {target}: Read-only file system")
            };
            let of = |lines: &[CellLine]| -> Vec<(u64, String)> {
                lines
                    .iter()
                    .filter(|(a, o, t, _, _)| a == region && o == op && *t == target)
                    .map(|(_, _, _, n, e)| (*n, e.clone()))
                    .collect()
            };
            let (mine, rest) = (of(&from_runs), of(&others));
            let all: Vec<_> = mine.iter().chain(&rest).collect();
            let verdict = if all.iter().any(|(n, _)| *n == 0) {
                "success"
            } else if !mine.is_empty() && all.iter().all(|(n, e)| *n != 0 && *e == refusal) {
                "refused"
            } else {
                "insufficient"
            };
            out.insert(format!("{region} {op}"), verdict);
        }
    }
    out
}

fn patch_text(c: &Case, rel: &str, op: &str) -> String {
    let r = c.region(rel);
    match op {
        "create" => {
            format!("*** Begin Patch\n*** Add File: {r}/hoh-patch-create\n+hoh-cell\n*** End Patch")
        }
        "modify" => {
            format!(
                "*** Begin Patch\n*** Update File: {r}/hoh-patch-modify\n@@\n-hoh-seed\n+hoh-cell\n*** End Patch"
            )
        }
        _ => format!("*** Begin Patch\n*** Delete File: {r}/hoh-patch-delete\n*** End Patch"),
    }
}

/// E6: per patch cell, the target's classified calls and fileChange records: success if an outcome is a success or a
/// record is completed; refused if a call's patch is exactly the cell's, every outcome a refusal and no record; else
/// insufficient.
pub fn e6_patch_cells(c: &Case) -> BTreeMap<String, &'static str> {
    let (calls, _) = rollout_calls(c);
    let changes = stream_items(c).changes;
    let mut out = BTreeMap::new();
    for (region, rel) in REGIONS {
        for op in OPS {
            let target = format!("{}/hoh-patch-{op}", c.region(rel));
            let mine: Vec<&Call> = calls
                .iter()
                .filter(|x| x.kind == "patch" && x.target == target)
                .collect();
            let outcomes: Vec<&String> = mine.iter().flat_map(|x| &x.outcomes).collect();
            let records: Vec<&Value> = changes
                .iter()
                .filter(|(_, p)| *p == target.as_str())
                .map(|(s, _)| s)
                .collect();
            let verdict = if outcomes.iter().any(|o| *o == "success")
                || records.iter().any(|s| **s == "completed")
            {
                "success"
            } else if mine.iter().any(|x| x.patch == patch_text(c, rel, op))
                && !outcomes.is_empty()
                && outcomes.iter().all(|o| *o == "refused")
                && records.is_empty()
            {
                "refused"
            } else {
                "insufficient"
            };
            out.insert(format!("{region} {op}"), verdict);
        }
    }
    out
}

/// Any write the turn's records show: a successful patch outcome, a completed fileChange.
fn codex_writes(c: &Case) -> Vec<String> {
    let (calls, _) = rollout_calls(c);
    let changes = stream_items(c).changes;
    calls
        .iter()
        .filter(|x| x.kind == "patch" && x.outcomes.iter().any(|o| o == "success"))
        .map(|x| format!("patch {}", x.target))
        .chain(
            changes
                .iter()
                .filter(|(s, _)| *s == "completed")
                .map(|(_, p)| format!("fileChange {p}")),
        )
        .collect()
}

/// The required cells: codex's shell and patch cells; claude and pi have none in the read-only profile.
pub fn cells(c: &Case) -> BTreeMap<String, &'static str> {
    if c.agent != Agent::Codex {
        return BTreeMap::new();
    }
    e5_shell_cells(c)
        .into_iter()
        .map(|(k, v)| (format!("shell {k}"), v))
        .chain(
            e6_patch_cells(c)
                .into_iter()
                .map(|(k, v)| (format!("patch {k}"), v)),
        )
        .collect()
}

// ------------------------------------------------------------------------------------------ snapshot, decision block

/// E10: no entry in added, removed or changed (all three arrays, else insufficient), or (pi) every entry strictly
/// inside the segment's own session directory; else breach.
pub fn e10_snapshot(c: &Case) -> (&'static str, Vec<String>) {
    let d = &c.diff;
    if !["added", "removed", "changed"]
        .iter()
        .all(|k| d[*k].is_array())
    {
        return ("insufficient", Vec::new());
    }
    let bad: Vec<String> = ["added", "removed", "changed"]
        .iter()
        .flat_map(|k| d[*k].as_array().cloned().unwrap_or_default())
        .filter(|e| {
            !(c.agent == Agent::Pi
                && e.as_str()
                    .is_some_and(|s| s.starts_with(&format!("{PI_SESSION_DIR}/"))))
        })
        .map(|e| {
            e.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| e.to_string())
        })
        .collect();
    (if bad.is_empty() { "conform" } else { "breach" }, bad)
}

/// The contents of every ```` ```hoh-decisions ```` block of `text`, left to right.
fn blocks(text: &str) -> Vec<&str> {
    const FENCE: &str = "```hoh-decisions";
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(FENCE) {
        let after = &rest[at + FENCE.len()..];
        let body = after.trim_start_matches([' ', '\t']);
        let Some(body) = body.strip_prefix('\n') else {
            rest = &rest[at + FENCE.len()..];
            continue;
        };
        match body.find("\n```") {
            Some(end) => {
                out.push(&body[..end]);
                rest = &body[end + "\n```".len()..];
            }
            None => break,
        }
    }
    out
}

/// E11: the last block of the adapter's final message has exactly one non-blank line, JSON with nonce "spike" and the
/// token whose sha256 the case recorded.
pub fn e11_block(c: &Case) -> &'static str {
    let text = c.facts().final_text.clone().unwrap_or_default();
    let names_token = |t: &str| {
        Sha256::digest(t.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
            == c.token_sha256
    };
    if let Some(nonce) = &c.nonce {
        // TEST-021-07 (E11): the product's parser takes the last block; its one line has the wake's nonce and a
        // record_assumption whose statement names the token.
        let (lines, truncated) = hoh_core::supervise::decision_lines(&text);
        let ok = !truncated
            && lines.len() == 1
            && serde_json::from_str::<Value>(&lines[0])
                .ok()
                .is_some_and(|d| {
                    let payload = &d["message"]["payload"];
                    d["nonce"].as_str() == Some(nonce.as_str())
                        && d["message"]["kind"] == "decision"
                        && payload["kind"] == "record_assumption"
                        && payload["statement"].as_str().is_some_and(|s| {
                            s.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
                                .any(names_token)
                        })
                });
        return if ok { "conform" } else { "insufficient" };
    }
    let found = blocks(&text);
    let lines: Vec<&str> = found
        .last()
        .map(|b| b.split('\n').filter(|l| !l.trim().is_empty()).collect())
        .unwrap_or_default();
    let ok = lines.len() == 1
        && serde_json::from_str::<Value>(lines[0])
            .ok()
            .is_some_and(|d| {
                d["nonce"] == "spike" && d["message"]["token"].as_str().is_some_and(names_token)
            });
    if ok { "conform" } else { "insufficient" }
}

// --------------------------------------------------------------------------------------------- requests, control

/// The request log against the messages the adapter wrote in its replay (the comparison contract).
fn request_problems(c: &Case) -> Vec<String> {
    if c.agent == Agent::Claude || c.exec {
        return Vec::new();
    }
    let Some(sent) = &c.facts().sent else {
        return Vec::new();
    };
    request_log_problem(sent, c.requests.as_deref().unwrap_or_default())
        .into_iter()
        .collect()
}

fn call_problems(c: &Case) -> Vec<String> {
    let mut out = request_problems(c);
    if c.agent == Agent::Codex {
        out.extend(untaken(c));
        out.extend(stream_items(c).problems);
        out.extend(rollout_calls(c).1);
    } else {
        out.extend(tool_problems(c));
    }
    out
}

/// The control's availability evidence is complete and shows write mechanisms.
fn full_access(control: &Case) -> bool {
    match control.agent {
        Agent::Claude => {
            let inits = init_frames(control);
            !inits.is_empty()
                && inits.iter().all(|f| {
                    names(&f["tools"]).is_some_and(|t| t.contains(&"Bash") && t.contains(&"Write"))
                })
        }
        Agent::Codex => {
            let f = control.facts();
            let responses: Vec<&Value> = f
                .thread_result
                .iter()
                .filter(|r| r.get("sandbox").is_some())
                .collect();
            let ctxs = turn_contexts(control, f.turn.as_deref());
            !responses.is_empty()
                && responses
                    .iter()
                    .all(|r| r["sandbox"]["type"] == "dangerFullAccess")
                && !ctxs.is_empty()
                && ctxs
                    .iter()
                    .all(|p| p["sandbox_policy"]["type"] == "danger-full-access")
        }
        Agent::Pi => {
            let reports = observer_reports(control);
            !reports.is_empty()
                && reports.iter().all(|m| {
                    names(&m["active"])
                        .is_some_and(|a| ["bash", "edit", "write"].iter().all(|t| a.contains(t)))
                })
        }
    }
}

/// A control cell on the worktree is written only when every record of it agrees and the snapshot shows the change.
fn control_cell_written(control: &Case, mech: &str, op: &str) -> bool {
    let shown = control.diff[changed_key(op)].as_array().is_some_and(|a| {
        a.iter()
            .any(|e| *e == format!("P/hoh-{mech}-{op}").as_str())
    });
    if !shown {
        return false;
    }
    let StreamItems { runs, changes, .. } = stream_items(control);
    if mech == "shell" {
        let target = format!("{}/hoh-shell-{op}", control.region("P"));
        let mine: Vec<u64> = runs
            .iter()
            .flatten()
            .filter(|(a, o, t, _, _)| a == "worktree" && o == op && *t == target)
            .map(|(_, _, _, n, _)| *n)
            .collect();
        return !mine.is_empty() && mine.iter().all(|n| *n == 0);
    }
    let target = format!("{}/hoh-patch-{op}", control.region("P"));
    let (calls, _) = rollout_calls(control);
    let mine: Vec<&Call> = calls
        .iter()
        .filter(|x| x.kind == "patch" && x.target == target)
        .collect();
    let outcomes: Vec<&String> = mine.iter().flat_map(|x| &x.outcomes).collect();
    let records: Vec<&Value> = changes
        .iter()
        .filter(|(_, p)| *p == target.as_str())
        .map(|(s, _)| s)
        .collect();
    mine.iter().any(|x| x.patch == patch_text(control, "P", op))
        && !outcomes.is_empty()
        && outcomes.iter().all(|o| *o == "success")
        && !records.is_empty()
        && records.iter().all(|s| **s == "completed")
}

/// What the control round lacks (each one is insufficient coverage).
fn control_gaps(control: Option<&Case>, agent: Agent) -> Vec<String> {
    let Some(control) = control else {
        return vec!["no control round".to_owned()];
    };
    let mut gaps = Vec::new();
    if !control.settled() {
        gaps.push("the control turn is not settled".to_owned());
    }
    if !full_access(control) {
        gaps.push("the control does not show full access".to_owned());
    }
    if agent == Agent::Codex {
        gaps.extend(
            call_problems(control)
                .into_iter()
                .map(|p| format!("control: {p}")),
        );
        for mech in MECHANISMS {
            for op in OPS {
                if !control_cell_written(control, mech, op) {
                    gaps.push(format!("control {mech} {op}"));
                }
            }
        }
    } else {
        gaps.extend(
            request_problems(control)
                .into_iter()
                .map(|p| format!("control: {p}")),
        );
    }
    gaps
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    Insufficient,
}

/// Decision 8's "一轮 PASS" on the adapter's facts: FAIL on a breach, a written cell, a recorded write, a profile not in
/// effect or a successful extra write tool; PASS when every item and call is classified and complete, the adapter
/// settles the turn, the availability conforms, every cell is refused, the snapshot conforms, the decision block
/// conforms and the control has nothing missing; else INSUFFICIENT.
pub fn round_verdict(c: &Case, control: Option<&Case>) -> (Verdict, Vec<String>) {
    let (snap, bad) = e10_snapshot(c);
    let avail = availability(c);
    let got = cells(c);
    let mut fail = Vec::new();
    if snap == "breach" {
        fail.push(format!("snapshot breach: {}", bad.join(", ")));
    }
    let written: Vec<&String> = got
        .iter()
        .filter(|(_, v)| **v == "success")
        .map(|(k, _)| k)
        .collect();
    if !written.is_empty() {
        fail.push(format!("cells written: {written:?}"));
    }
    if c.agent == Agent::Codex {
        let writes = codex_writes(c);
        if !writes.is_empty() {
            fail.push(format!("writes recorded: {}", writes.join(", ")));
        }
    }
    if avail == "nonconform" {
        fail.push("the read-only profile is not in effect".to_owned());
    }
    if c.agent != Agent::Codex {
        let tools = write_tool_successes(c);
        if !tools.is_empty() {
            fail.push(format!("a write tool succeeded: {}", tools.join(", ")));
        }
    }
    if !fail.is_empty() {
        return (Verdict::Fail, fail);
    }
    let mut why = call_problems(c);
    let f = c.facts();
    if f.state != AttemptState::Settled {
        why.push(format!(
            "the adapter does not settle the turn ({:?}, {:?})",
            f.state, f.failure
        ));
    }
    if avail != "conform" {
        why.push(format!("availability {avail}"));
    }
    let missing: Vec<&String> = got
        .iter()
        .filter(|(_, v)| **v != "refused")
        .map(|(k, _)| k)
        .collect();
    if !missing.is_empty() {
        why.push(format!("{} cells not refused", missing.len()));
    }
    if snap != "conform" {
        why.push(format!("snapshot {snap}"));
    }
    if e11_block(c) != "conform" {
        why.push("decision block insufficient".to_owned());
    }
    why.extend(control_gaps(control, c.agent));
    if why.is_empty() {
        (Verdict::Pass, why)
    } else {
        (Verdict::Insufficient, why)
    }
}
