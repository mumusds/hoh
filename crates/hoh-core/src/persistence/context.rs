//! What a turn's context is packed from (docs/adr/ADR-026.md decisions 3, 10 and 11): read in the runner's waiting
//! read for each dispatch, into the facts its mandatory items are rendered from and its candidate entries. Only
//! whitelisted fields of runtime records become entry text; raw logs, adapter frames and whole TaskResults never do.

use super::{PersistError, sqlite_error};
use crate::context::{Class, Entry, Facts, Kind, Params};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The five fields a wake's subject is read from (`supervise.rs` fingerprints, docs/adr/ADR-026.md decision 11).
pub(super) const SUBJECT_FIELDS: [&str; 5] =
    ["task_id", "issue_id", "attempt_id", "role", "candidate_id"];

/// Fields of a wake's event payload whose text is another session's or agent's output (an adapter's raw frame, a
/// whole TaskResult): references only, never entry text (docs/adr/ADR-026.md decisions 10 and 11).
const REFERENCE_ONLY: [&str; 2] = ["task_result_json", "frame"];

/// The facts of attempt `attempt_id` of task `task_id`, role `role`, in run `run_id`; `supervision` for a Commander
/// turn.
pub(super) fn facts(
    conn: &Connection,
    run_id: &str,
    task_id: &str,
    attempt_id: &str,
    supervision: Option<&super::Supervision>,
) -> Result<Facts, PersistError> {
    let goal = super::results::started_goal(conn, run_id)?;
    let (spec_id, spec, snapshot): (Option<String>, Option<(String, i64)>, Option<String>) = conn
        .query_row(
            "SELECT r.spec_id, s.sha256, s.revision, json_extract(r.config_snapshot_json, '$.context') \
             FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| {
                let sha: Option<String> = r.get(1)?;
                let revision: Option<i64> = r.get(2)?;
                Ok((r.get(0)?, sha.zip(revision), r.get(3)?))
            },
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or((None, None, None));
    let candidate =
        super::outbox::current_candidate(conn, run_id)?.map(|c| (c.id, c.git_commit, c.git_tree));
    let mut facts = Facts {
        goal,
        spec,
        candidate,
        params: snapshot.as_deref().and_then(params_of),
        ..Facts::default()
    };
    let titles = titles(conn, run_id)?;
    match supervision {
        Some(s) => wake(
            conn,
            &mut facts,
            s,
            [run_id, task_id, attempt_id],
            spec_id.as_deref(),
            &titles,
        )?,
        None => work(
            conn,
            &mut facts,
            task_id,
            attempt_id,
            spec_id.as_deref(),
            &titles,
        )?,
    }
    Ok(facts)
}

/// A run's packing parameters as its snapshot keeps them, if they are whole and sound.
fn params_of(text: &str) -> Option<Params> {
    let v: Value = serde_json::from_str(text).ok()?;
    let budget_tokens = v.get("budget_tokens")?.as_u64()?;
    let shares: Vec<u64> = v
        .get("shares")?
        .as_array()?
        .iter()
        .filter_map(Value::as_u64)
        .collect();
    let shares: [u64; 3] = shares.try_into().ok()?;
    (shares.iter().sum::<u64>() <= 100).then_some(Params {
        budget_tokens,
        shares,
    })
}

/// The tasks' titles as the run's plans imported them (the latest import wins).
fn titles(conn: &Connection, run_id: &str) -> Result<BTreeMap<String, String>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT json_extract(j.value, '$.task_id'), json_extract(j.value, '$.title') \
             FROM events e, json_each(json_extract(e.payload_json, '$.tasks')) j \
             WHERE e.run_id = ?1 AND e.kind = 'plan.imported' ORDER BY e.seq",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(Option<String>, Option<String>)> = statement
        .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows
        .into_iter()
        .filter_map(|(task, title)| Some((task?, title?)))
        .collect())
}

/// The acceptance criteria of `task_id` under spec `spec_id`, by id, with their text.
fn acceptance(
    conn: &Connection,
    task_id: &str,
    spec_id: Option<&str>,
) -> Result<Vec<(String, String)>, PersistError> {
    let Some(spec_id) = spec_id else {
        return Ok(Vec::new());
    };
    let mut statement = conn
        .prepare(
            "SELECT a.ac_id, a.body FROM task_requirements tr JOIN acceptance_criteria a \
             ON a.requirement_id = tr.requirement_id AND a.spec_id = ?2 \
             WHERE tr.task_id = ?1 AND tr.spec_id = ?2 ORDER BY a.ac_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![task_id, spec_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(rows)
}

/// A work task's scope, its acceptance criteria and its earlier attempts' ends as evidence.
fn work(
    conn: &Connection,
    facts: &mut Facts,
    task_id: &str,
    attempt_id: &str,
    spec_id: Option<&str>,
    titles: &BTreeMap<String, String>,
) -> Result<(), PersistError> {
    let row: Option<(String, i64)> = conn
        .query_row(
            "SELECT role, writer FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (role, writer) = row.unwrap_or_default();
    let deps: Vec<String> = {
        let mut statement = conn
            .prepare(
                "SELECT depends_on FROM task_dependencies WHERE task_id = ?1 ORDER BY depends_on",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![task_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    facts.scope.push(format!(
        "任务：{task_id}{}（职位 {role}，{}；依赖：{}）",
        titles
            .get(task_id)
            .map(|t| format!(" {t}"))
            .unwrap_or_default(),
        if writer == 1 { "写者" } else { "非写者" },
        if deps.is_empty() {
            "无".to_owned()
        } else {
            deps.join("、")
        }
    ));
    facts.acceptance = acceptance(conn, task_id, spec_id)?;
    // What its previous attempt is to correct or do again (docs/adr/ADR-028.md decisions 3, 11 and 12, M2T16R-07): the
    // reasons the Runtime gave, in the mandatory scope, so the next turn knows them whatever the budget.
    facts.scope.extend(corrections(conn, task_id, attempt_id)?);
    // Its earlier attempts' ends: runtime facts only (state, failure reason, end), never what the agent said.
    let mut statement = conn
        .prepare(
            "SELECT attempt_id, state, failure_reason, ended_at FROM attempts \
             WHERE task_id = ?1 AND attempt_id <> ?2 ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let ends: Vec<(String, String, Option<String>, Option<String>)> = statement
        .query_map(params![task_id, attempt_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    for (n, (id, state, failure, ended)) in ends.into_iter().enumerate() {
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("attempts/{id}"),
            // Its own short form: the end and the first of the reason (the packer never summarises).
            summary: Some(format!(
                "{id} {state}：{}",
                failure.as_deref().unwrap_or("").chars().take(120).collect::<String>()
            )),
            text: json!({"attempt_id": id, "state": state, "failure_reason": failure, "ended_at": ended})
                .to_string(),
            relevance: 1,
            recency: n as u64 + 1,
            causal: 0,
        });
    }
    // A reworked task's next attempt: what its round found and the rejected candidate's changes (docs/adr/ADR-028.md
    // decisions 12 and 14).
    if let Some(r) = rework(conn, task_id)? {
        let evidence = |path: String, text: String| Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path,
            text,
            summary: None,
            relevance: 3,
            recency: 0,
            causal: 0,
        };
        for (n, item) in r.items.iter().enumerate() {
            facts.entries.push(evidence(
                format!("rework/items/{n}"),
                format!(
                    "返工的失败项（轮次 {}，被拒候选 {}）：{item}",
                    r.round_id, r.candidate_id
                ),
            ));
        }
        for (id, severity, description) in &r.issues {
            facts.entries.push(evidence(
                format!("rework/issues/{id}"),
                format!(
                    "被拒候选 {} 上未关闭的 Issue {id}（{severity}）：{description}",
                    r.candidate_id
                ),
            ));
        }
        for t in &r.traced {
            let observed = if t.observed.is_empty() {
                String::new()
            } else {
                format!("；验证者的观察：{}", t.observed.join("；"))
            };
            facts.entries.push(evidence(
                format!("rework/traced/{}", t.issue_id),
                format!(
                    "仍存在的 Issue {}（{}，候选 {}，报告 {}，finding {}）：{}{observed}",
                    t.issue_id,
                    t.severity,
                    t.candidate_id.as_deref().unwrap_or("-"),
                    t.report_id.as_deref().unwrap_or("-"),
                    t.finding_id.as_deref().unwrap_or("-"),
                    t.description
                ),
            ));
        }
        facts.code = r.base.map(|base| crate::context::CodeSource {
            base,
            commit: r.git_commit,
        });
    }
    Ok(())
}

/// The Runtime's reasons for task `task_id`'s next attempt (`attempt_id` excluded): the correction or verify-again not
/// yet answered — the latest an attempt's end gave, kept across ends that gave no result (a start that failed, a
/// transient failure, a cancel) and cleared by an accepted end (SETTLED with neither) — and a refreeze its candidate's
/// loss asked for since its last completion; each bounded (implementation correction 4, M2T16R-07, -102).
pub(super) fn corrections(
    conn: &Connection,
    task_id: &str,
    attempt_id: &str,
) -> Result<Vec<String>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT attempt_id, state FROM attempts WHERE task_id = ?1 AND attempt_id <> ?2 ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let earlier: Vec<(String, String)> = statement
        .query_map(params![task_id, attempt_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let bounded = |text: String| text.chars().take(2000).collect::<String>();
    // The task's corrections and reverifications, read once within its run (events_run_kind, M2T16R-201), by attempt.
    let run_id: Option<String> = conn
        .query_row(
            "SELECT run_id FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let mut owed: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    let mut statement = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.attempt_id'), kind, payload_json FROM events WHERE run_id = ?1 \
             AND kind IN ('task.protocol_correction', 'task.reverify') AND json_extract(payload_json, '$.task_id') = ?2 \
             ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let found = statement
        .query_map(params![run_id, task_id], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(sqlite_error)?;
    for row in found {
        let (attempt, kind, payload) = row.map_err(sqlite_error)?;
        if let Some(attempt) = attempt {
            owed.entry(attempt).or_default().push((kind, payload));
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (previous, state) in earlier {
        let rows = owed.remove(&previous).unwrap_or_default();
        if rows.is_empty() {
            // An accepted end answers what was owed; any other end leaves it owed.
            if state == "SETTLED" {
                out.clear();
            }
            continue;
        }
        out = rows
            .into_iter()
            .map(|(kind, payload)| {
                let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
                bounded(if kind == "task.protocol_correction" {
                    format!(
                        "上一次 Attempt {previous} 的输出需要纠正：{}",
                        p["reasons"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("；")
                    )
                } else {
                    format!(
                        "上一次 Attempt {previous} 以 {} 结束，需要重新验证：{}",
                        p["status"].as_str().unwrap_or("?"),
                        p["reason"].as_str().unwrap_or_default()
                    )
                })
            })
            .collect();
    }
    let superseded: Option<String> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.detail') FROM events WHERE run_id = ?2 AND kind = 'task.superseded' \
             AND json_extract(payload_json, '$.task_id') = ?1 \
             AND seq > coalesce((SELECT max(c.seq) FROM events c WHERE c.run_id = ?2 AND c.kind = 'candidate_ready' \
               AND json_extract(c.payload_json, '$.task_id') = ?1), 0) ORDER BY seq DESC LIMIT 1",
            params![task_id, run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    if let Some(detail) = superseded {
        out.push(bounded(format!("上一个候选已失效，需要重新冻结：{detail}")));
    }
    Ok(out)
}

/// How severe a wake's event is, most first: integrity 3, critical 2, high 1, the rest 0 (ADR-025 decision 7).
fn band(kind: &str, severity: Option<&str>) -> u64 {
    // The supervisor's own order (ADR-025 decision 7): integrity, critical, high (need_decision and the like), the rest.
    3 - u64::from(super::wakes::urgency(kind, severity).min(3))
}

/// `value` with every field named in [`REFERENCE_ONLY`], at any depth, replaced by a note that it is a reference only.
fn redacted(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if REFERENCE_ONLY.contains(&k.as_str()) {
                        (k.clone(), json!("（只供引用，见该事件）"))
                    } else {
                        (k.clone(), redacted(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redacted).collect()),
        other => other.clone(),
    }
}

/// A Commander turn's question, header, scope (the wake's whole snapshot), acceptance (every snapshot task's) and its
/// evidence: the wake's events by severity, cause and recency, then the run's state, each with a short form of its own,
/// so a large state entry can never push the events that woke it out (decision 11, M2T09R-51).
fn wake(
    conn: &Connection,
    facts: &mut Facts,
    s: &super::Supervision,
    [run_id, task_id, attempt_id]: [&str; 3],
    spec_id: Option<&str>,
    titles: &BTreeMap<String, String>,
) -> Result<(), PersistError> {
    let summary = &s.summary;
    let wake = &summary["wake"];
    let kinds: BTreeSet<&str> = summary["events"]
        .as_array()
        .map(|e| e.iter().filter_map(|e| e["kind"].as_str()).collect())
        .unwrap_or_default();
    facts.question = Some(format!(
        "唤醒 {}（种类 {}，严重度 {}，指纹 {}）",
        s.wake,
        kinds.into_iter().collect::<Vec<_>>().join("、"),
        wake["severity"].as_str().unwrap_or("?"),
        wake["fingerprint"].as_str().unwrap_or("?")
    ));
    let total = |key: &str| summary[key].as_u64().unwrap_or(0).to_string();
    // The turn's own identity first: its answers name it (commander.md's output contract, M2T09R-03).
    facts.header = vec![
        ("run_id".to_owned(), run_id.to_owned()),
        ("task_id".to_owned(), task_id.to_owned()),
        ("attempt_id".to_owned(), attempt_id.to_owned()),
        ("causation_id".to_owned(), s.wake.clone()),
        (
            "expected_revision（Run 的 revision）".to_owned(),
            s.run_revision.to_string(),
        ),
        ("role_epoch".to_owned(), s.role_epoch.to_string()),
        ("nonce".to_owned(), s.nonce.clone()),
        (
            "Run 状态".to_owned(),
            summary["run"]["state"].as_str().unwrap_or("?").to_owned(),
        ),
        ("唤醒的事件共".to_owned(), total("events_total")),
        ("快照任务共".to_owned(), total("tasks_total")),
        ("其他待执行的唤醒共".to_owned(), total("other_wakes_total")),
    ];
    let snapshots: BTreeMap<String, i64> = summary["snapshots"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_i64()?)))
                .collect()
        })
        .unwrap_or_default();
    facts.no_task = snapshots.is_empty();
    for (task, revision) in &snapshots {
        facts.scope.push(format!(
            "快照任务：{task}{}（分配时 revision {revision}）",
            titles
                .get(task)
                .map(|t| format!(" {t}"))
                .unwrap_or_default()
        ));
    }
    let subject: BTreeSet<(String, String)> = s.subject.iter().cloned().collect();
    // The acceptance of every snapshot task, whole (decision 3, v4: the mandatory items are REQ-039 b2's, never cut to
    // fit; too many for the budget is CONTEXT_BUDGET_TOO_SMALL).
    let mut seen = BTreeSet::new();
    for task in snapshots.keys() {
        for (id, body) in acceptance(conn, task, spec_id)? {
            if seen.insert(id.clone()) {
                facts.acceptance.push((id, body));
            }
        }
    }
    facts.acceptance.sort();
    // The run's state, after the events (relevance 0, recency 0), each with its count as its short form.
    for (key, what) in [
        ("tasks", "快照任务的状态"),
        ("roles", "职位绑定"),
        ("other_wakes", "其他待执行的唤醒"),
        ("recent_rejections", "最近被拒或无效的回答"),
    ] {
        let count = summary[key].as_array().map_or(0, Vec::len);
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("summary/{key}"),
            text: summary[key].to_string(),
            summary: Some(format!("{what}：{count} 条（全文见引用）")),
            relevance: 0,
            recency: 0,
            causal: 0,
        });
    }
    // Each shown event: its whole payload (a wake's kinds are the supervisor's closed set), a TaskResult inside it as a
    // reference only (decision 10, -162).
    let empty = Vec::new();
    let events = summary["events"].as_array().unwrap_or(&empty);
    let attempts = attempt_tasks(conn, events)?;
    for event in events {
        let seq = event["seq"].as_u64().unwrap_or(0);
        let kind = event["kind"].as_str().unwrap_or_default();
        let payload = redacted(&event["payload"]);
        let field = |name: &str| payload.get(name).and_then(Value::as_str).map(str::to_owned);
        let causal = if SUBJECT_FIELDS
            .iter()
            .any(|f| field(f).is_some_and(|v| subject.contains(&((*f).to_owned(), v))))
        {
            2
        } else if field("task_id").is_some_and(|t| snapshots.contains_key(&t))
            || field("attempt_id")
                .and_then(|a| attempts.get(&a).cloned())
                .is_some_and(|t| snapshots.contains_key(&t))
        {
            1
        } else {
            0
        };
        let severity = field("severity");
        let mut brief: BTreeMap<&str, Value> =
            ["task_id", "attempt_id", "reason", "reason_code", "severity"]
                .into_iter()
                .filter_map(|k| Some((k, payload.get(k)?.clone())))
                .collect();
        let mut text = json!({"seq": seq, "kind": kind, "payload": payload});
        // A change request's, a finding's or an acceptance failure's Issue is the Runtime's to rework once its round
        // closes (docs/adr/ADR-028.md decision 13, M2T16D-355; docs/adr/ADR-029.md decision 9, M2T18D-258): said, so no
        // repair of the Commander's own is planned for it.
        if kind == "issue.opened"
            && matches!(
                field("source").as_deref(),
                Some("change_request" | "finding" | "acceptance")
            )
        {
            brief.insert("handling", json!(REWORKED_AFTER_ROUND));
            text["handling"] = json!(REWORKED_AFTER_ROUND);
        }
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("events/{seq}"),
            text: text.to_string(),
            summary: Some(json!({"seq": seq, "kind": kind, "brief": brief}).to_string()),
            relevance: band(kind, severity.as_deref()) * 3 + causal,
            recency: seq,
            causal,
        });
    }
    Ok(())
}

/// How a wake shows a change request's Issue (docs/adr/ADR-028.md decision 13, M2T16D-355).
const REWORKED_AFTER_ROUND: &str = "轮次关闭后由 Runtime 返工（不另建修复任务）";

/// The task of each attempt the shown events name.
fn attempt_tasks(
    conn: &Connection,
    events: &[Value],
) -> Result<BTreeMap<String, String>, PersistError> {
    let ids: BTreeSet<&str> = events
        .iter()
        .filter_map(|e| e["payload"]["attempt_id"].as_str())
        .collect();
    let mut out = BTreeMap::new();
    for id in ids {
        if let Some(task) = conn
            .query_row(
                "SELECT task_id FROM attempts WHERE attempt_id = ?1",
                params![id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(sqlite_error)?
        {
            out.insert(id.to_owned(), task);
        }
    }
    Ok(out)
}

/// The facts of a verification turn from its package (docs/adr/ADR-026.md decisions 3 and 10): the collector M2-T16's
/// QA and Reviewer dispatch uses. Its scope is the package's task and candidate, its in-scope acceptance the package's
/// `acceptance_ids` under its spec, its budget the package's; its evidence the package's references (listed, never
/// read) and the Developer's own account of the candidate, only its status and summary, marked a claim and ranked
/// after everything else (docs/07 section 4: requirements, AC and artifacts first). From the round's sources
/// `sources` (docs/adr/ADR-028.md decision 14): the Developer's registered self-test records as diagnostic loads, the
/// code source from their earliest start to the candidate. The Issues it is to confirm fixed are its mandatory scope's,
/// added with its round by the store's assignment (docs/adr/ADR-029.md decision 10).
pub(super) fn verification(
    conn: &Connection,
    package: &crate::protocol::ContextHandoff,
    sources: &[String],
) -> Result<Facts, PersistError> {
    let run_id = package.run_id.as_str();
    let spec_id: Option<String> = match (&package.spec_sha256, package.spec_revision) {
        (Some(sha), Some(revision)) => conn
            .query_row(
                "SELECT s.spec_id FROM specs s JOIN runs r ON r.project_id = s.project_id \
                 WHERE r.run_id = ?1 AND s.sha256 = ?2 AND s.revision = ?3",
                params![run_id, sha, i64::try_from(revision).unwrap_or(i64::MAX)],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?,
        _ => None,
    };
    let mut facts = Facts {
        goal: Some(package.goal.clone()),
        spec: package
            .spec_sha256
            .clone()
            .zip(package.spec_revision.and_then(|r| i64::try_from(r).ok())),
        candidate: package
            .candidate
            .as_ref()
            .map(|c| (c.id.clone(), c.git_commit.clone(), c.git_tree.clone())),
        ..Facts::default()
    };
    let snapshot: Option<String> = conn
        .query_row(
            "SELECT json_extract(config_snapshot_json, '$.context') FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    let shares = snapshot
        .as_deref()
        .and_then(params_of)
        .unwrap_or(crate::context::CONTEXT_V1)
        .shares;
    facts.params = Some(Params {
        budget_tokens: package.budget_tokens,
        shares,
    });
    let candidate = package
        .candidate
        .as_ref()
        .map(|c| c.id.clone())
        .unwrap_or_default();
    facts.scope.push(format!(
        "验证任务 {} 的候选 {candidate}（职位 {}）",
        package.task_id.as_deref().unwrap_or("-"),
        package.role
    ));
    // Every AC the package names, whole: one missing, or a spec that cannot be found, is an error, never a context
    // without it (M2T09R-102).
    if !package.acceptance_ids.is_empty() {
        let spec_id = spec_id.as_deref().ok_or_else(|| {
            PersistError::IntegrityViolation(format!(
                "the verification package of {run_id} names a specification this project does not have"
            ))
        })?;
        for id in &package.acceptance_ids {
            let body: Option<String> = conn
                .query_row(
                    "SELECT body FROM acceptance_criteria WHERE ac_id = ?1 AND spec_id = ?2",
                    params![id, spec_id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            let body = body.ok_or_else(|| {
                PersistError::IntegrityViolation(format!(
                    "{id} of the verification package is not in its specification"
                ))
            })?;
            facts.acceptance.push((id.clone(), body));
        }
    }
    facts.acceptance.sort();
    for (n, r) in package.references.iter().enumerate() {
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("references/{n}"),
            text: format!(
                "引用（只供取回，不在此展开）：{} sha256:{}",
                r.path, r.sha256
            ),
            summary: None,
            relevance: 1,
            recency: 0,
            causal: 0,
        });
    }
    // The Developer's own account: status and summary only, a claim (docs/07 section 4, ADR-026 decision 10), marked
    // diagnostic (docs/adr/ADR-028.md decision 14).
    let ready: Option<(Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT json_extract(payload_json, '$.task_result_json'), json_extract(payload_json, '$.attempt_id') \
             FROM events WHERE run_id = ?1 AND kind = 'candidate_ready' \
             AND json_extract(payload_json, '$.candidate_id') = ?2 ORDER BY seq DESC LIMIT 1",
            params![run_id, candidate],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let (result, producer) = ready.unwrap_or_default();
    if let Some(result) = result.and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("claims/{candidate}"),
            text: format!(
                "开发者自述，仅作线索（diagnostic，不作验收依据）：status={}；summary={}",
                result["status"].as_str().unwrap_or("?"),
                result["summary"].as_str().unwrap_or("")
            ),
            summary: None,
            relevance: 0,
            recency: 0,
            causal: 0,
        });
    }
    if let Some(producer) = producer {
        facts.loads = self_tests(conn, &producer)?;
    }
    facts.code = match (code_base(conn, sources)?, package.candidate.as_ref()) {
        (Some(base), Some(c)) => Some(crate::context::CodeSource {
            base,
            commit: c.git_commit.clone(),
        }),
        _ => None,
    };
    Ok(facts)
}

/// The label of a Developer's self-test record in a verifier's context (docs/adr/ADR-028.md decision 14).
pub(super) const SELF_TEST_LABEL: &str = "开发者自测（diagnostic，仅作诊断，不作验收依据）";

/// The self-test records attempt `attempt_id`'s completion registered (`attempt.artifacts` of kind `self_test`), as
/// loads: only those objects are ever read (decision 14).
fn self_tests(
    conn: &Connection,
    attempt_id: &str,
) -> Result<Vec<crate::context::Load>, PersistError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1 AND kind = 'attempt.artifacts'",
            params![crate::ids::child_id(attempt_id, SELF_TESTS_EVENT)],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let payload: Value = payload
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or(Value::Null);
    Ok(payload["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["kind"] == "self_test")
        .filter_map(|i| {
            let (sha256, name) = (i["sha256"].as_str()?, i["name"].as_str()?);
            Some(crate::context::Load {
                path: format!("self-tests/{name}"),
                sha256: sha256.to_owned(),
                name: name.to_owned(),
                label: SELF_TEST_LABEL.to_owned(),
                relevance: 0,
            })
        })
        .collect())
}

/// The key (under its attempt) of the `attempt.artifacts` event a completion registers its self-test records in.
pub(super) const SELF_TESTS_EVENT: &str = "artifacts/self_tests";

/// git's empty tree: the base of a source whose first writer started in a repository without a commit.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// The base of the changed files of tasks `sources` (docs/adr/ADR-028.md decision 14, -462): the HEAD at the start of
/// their earliest writer attempt (`attempt.workspace_start`), git's empty tree when there was none; none when none of
/// them started one.
fn code_base(conn: &Connection, sources: &[String]) -> Result<Option<String>, PersistError> {
    let head: Option<Option<String>> = conn
        .query_row(
            "SELECT json_extract(e.payload_json, '$.head') FROM events e \
             JOIN attempts a ON a.attempt_id = json_extract(e.payload_json, '$.attempt_id') \
             WHERE e.kind = 'attempt.workspace_start' AND a.task_id IN (SELECT value FROM json_each(?1)) \
             ORDER BY e.seq LIMIT 1",
            params![json!(sources).to_string()],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(head.map(|h| h.unwrap_or_else(|| EMPTY_TREE.to_owned())))
}

/// What a reworked development task's next attempt is given (docs/adr/ADR-028.md decisions 12 and 14): the round
/// whose failures its rework took, the rejected candidate, those failure items, the candidate's open Issues and the
/// base of its changed files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReworkContext {
    pub round_id: String,
    pub candidate_id: String,
    pub git_commit: String,
    pub items: Vec<String>,
    /// `(issue_id, severity, description)`.
    pub issues: Vec<(String, String, String)>,
    pub base: Option<String>,
    /// The Issues its `issue:<fingerprint>` items name, across the run, with their reports, findings and what the
    /// verifiers observed (docs/adr/ADR-029.md decision 11, M2T18D-201).
    pub traced: Vec<super::issues::Traced>,
}

/// Task `task_id`'s rework context: from the failures its reworks or decisions took since its last completion, the
/// latest naming the round and the rejected candidate; none if none did.
pub(super) fn rework(
    conn: &Connection,
    task_id: &str,
) -> Result<Option<ReworkContext>, PersistError> {
    // Every failure its reworks (or decisions) took since its last completion (candidate_ready), oldest first: two
    // rounds on one candidate may each have given it items; the latest names the rejected candidate.
    let mut statement = conn
        .prepare(
            "SELECT payload_json FROM events WHERE kind = 'verification.failures_consumed' \
             AND json_extract(payload_json, '$.task_id') = ?1 \
             AND json_extract(payload_json, '$.by') IN ('rework', 'decision') \
             AND seq > coalesce((SELECT max(c.seq) FROM events c WHERE c.kind = 'candidate_ready' \
               AND json_extract(c.payload_json, '$.task_id') = ?1), 0) ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let all: Vec<Value> = statement
        .query_map(params![task_id], |r| r.get::<_, String>(0))
        .map_err(sqlite_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sqlite_error)?
        .iter()
        .filter_map(|t| serde_json::from_str(t).ok())
        .collect();
    let Some(taken) = all.last().cloned() else {
        return Ok(None);
    };
    let round_id = taken["round_id"].as_str().unwrap_or_default().to_owned();
    let opened: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'verification.round_opened' \
             AND json_extract(payload_json, '$.round_id') = ?1",
            params![round_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    // A failed join's failures name its id (docs/adr/ADR-029.md decision 11, J8): its `verification.joined` gives the
    // candidate, its participants (the sources) and its rounds.
    let opened: Value = match opened {
        Some(o) => serde_json::from_str(&o).unwrap_or(Value::Null),
        None => joined(conn, &round_id)?
            .map(|j| json!({"candidate_id": j["candidate_id"], "source_task_ids": j["tasks"]}))
            .unwrap_or(Value::Null),
    };
    let Some(candidate_id) = opened["candidate_id"].as_str().map(str::to_owned) else {
        return Ok(None);
    };
    let git_commit: String = conn
        .query_row(
            "SELECT git_commit FROM candidates WHERE candidate_id = ?1",
            params![candidate_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .unwrap_or_default();
    let mut statement = conn
        .prepare(
            "SELECT issue_id, severity, description FROM issues WHERE candidate_id = ?1 \
             AND state IN ('OPEN', 'REOPENED') ORDER BY issue_id",
        )
        .map_err(sqlite_error)?;
    let issues = statement
        .query_map(params![candidate_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let sources: Vec<String> = opened["source_task_ids"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t.as_str().map(str::to_owned))
        .collect();
    let items: Vec<String> = all
        .iter()
        .flat_map(|t| t["items"].as_array().cloned().unwrap_or_default())
        .filter_map(|i| i.as_str().map(str::to_owned))
        .collect();
    // Each `issue:<fingerprint>` item's Issues across the run, with their reports and findings, and what the verifiers
    // of the rounds it took failures from observed of them (docs/adr/ADR-029.md decision 11, M2T18D-201).
    let mut rounds: Vec<String> = Vec::new();
    for id in all.iter().filter_map(|t| t["round_id"].as_str()) {
        rounds.push(id.to_owned());
        if let Some(j) = joined(conn, id)? {
            rounds.extend(
                j["rounds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str().map(str::to_owned)),
            );
        }
    }
    let run_id: Option<String> = conn
        .query_row(
            "SELECT run_id FROM tasks WHERE task_id = ?1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let mut traced = Vec::new();
    if let Some(run_id) = run_id {
        let mut statement = conn
            .prepare(
                "SELECT json_extract(c.value, '$.issue_id'), json_extract(c.value, '$.observed') FROM events e,                  json_each(e.payload_json, '$.confirmations') c WHERE e.run_id = ?1 AND e.kind = 'verification.report'                  AND json_extract(e.payload_json, '$.round_id') IN (SELECT value FROM json_each(?2)) ORDER BY e.seq, c.key",
            )
            .map_err(sqlite_error)?;
        let observed: Vec<(String, String)> = statement
            .query_map(params![run_id, json!(rounds).to_string()], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let fingerprints: BTreeSet<&str> = items
            .iter()
            .filter_map(|i| i.strip_prefix("issue:"))
            .collect();
        for fingerprint in fingerprints {
            for mut issue in super::issues::of_fingerprint(conn, &run_id, fingerprint)? {
                issue.observed = observed
                    .iter()
                    .filter(|(id, _)| *id == issue.issue_id)
                    .map(|(_, text)| text.clone())
                    .collect();
                traced.push(issue);
            }
        }
    }
    Ok(Some(ReworkContext {
        round_id,
        git_commit,
        items,
        issues,
        base: code_base(conn, &sources)?,
        candidate_id,
        traced,
    }))
}

/// The `verification.joined` payload of join `join_id`, if it is one (docs/adr/ADR-029.md decision 11).
fn joined(conn: &Connection, join_id: &str) -> Result<Option<Value>, PersistError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE kind = 'verification.joined' \
             AND json_extract(payload_json, '$.join_id') = ?1 ORDER BY seq DESC LIMIT 1",
            params![join_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(payload.and_then(|p| serde_json::from_str(&p).ok()))
}

/// The objects attempts `attempts` produced, in that order and each attempt's in its own order, as references: from
/// their `attempt.artifacts` events (docs/adr/ADR-026.md decision 10) and the `evidence.recorded` of each check they
/// ran (docs/adr/ADR-027.md decision 8), merged in event order; an object produced twice once, at most `limit`.
pub(super) fn references(
    conn: &Connection,
    attempts: &[String],
    limit: usize,
) -> Result<Vec<crate::protocol::ArtifactRef>, PersistError> {
    let mut out: Vec<crate::protocol::ArtifactRef> = Vec::new();
    let mut seen = BTreeSet::new();
    for attempt in attempts {
        // The attempt's own artifacts (its raw logs and its self-test records, docs/adr/ADR-028.md decision 14) and each
        // check it ran (`evidence.recorded`, docs/adr/ADR-027.md decision 8), in the order they were written.
        let mut statement = conn
            .prepare(
                "SELECT kind, payload_json FROM events \
                 WHERE (event_id IN (?1, ?3) AND kind = 'attempt.artifacts') \
                    OR (kind = 'evidence.recorded' AND json_extract(payload_json, '$.attempt_id') = ?2) \
                 ORDER BY seq",
            )
            .map_err(sqlite_error)?;
        let payloads = statement
            .query_map(
                params![
                    crate::ids::child_id(attempt, "artifacts"),
                    attempt,
                    crate::ids::child_id(attempt, SELF_TESTS_EVENT)
                ],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .map_err(sqlite_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sqlite_error)?;
        for (kind, payload) in payloads {
            let payload: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
            let items = if kind == "attempt.artifacts" {
                &payload["items"]
            } else {
                &payload["artifacts"]
            };
            for item in items.as_array().cloned().unwrap_or_default() {
                let (Some(sha256), Some(path)) = (item["sha256"].as_str(), item["path"].as_str())
                else {
                    continue;
                };
                if out.len() >= limit {
                    return Ok(out);
                }
                if seen.insert((sha256.to_owned(), path.to_owned())) {
                    out.push(crate::protocol::ArtifactRef {
                        path: path.to_owned(),
                        sha256: sha256.to_owned(),
                    });
                }
            }
        }
    }
    Ok(out)
}

/// The context budget of run `run_id`'s packages: its snapshot's, else [`crate::context::CONTEXT_V1`]'s.
pub(super) fn budget(conn: &Connection, run_id: &str) -> Result<u64, PersistError> {
    let snapshot: Option<String> = conn
        .query_row(
            "SELECT json_extract(config_snapshot_json, '$.context') FROM runs WHERE run_id = ?1",
            params![run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?
        .flatten();
    Ok(snapshot
        .as_deref()
        .and_then(params_of)
        .unwrap_or(crate::context::CONTEXT_V1)
        .budget_tokens)
}

#[cfg(test)]
mod tests;
