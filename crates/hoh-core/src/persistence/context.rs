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

/// Relevance above every event's, so the run's state goes in before the events that led to it.
const STATE_RELEVANCE: u64 = 100;

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
        Some(s) => wake(conn, &mut facts, s, spec_id.as_deref(), &titles)?,
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
    Ok(())
}

/// How severe a wake's event is, most first: integrity 3, critical 2, high 1, the rest 0 (ADR-025 decision 7).
fn band(kind: &str, severity: Option<&str>) -> u64 {
    match (kind, severity) {
        ("integrity_violation", _) => 3,
        (_, Some("critical")) => 2,
        (_, Some("high")) => 1,
        _ => 0,
    }
}

/// A Commander turn's question, header, scope (the wake's whole snapshot), acceptance (its subject tasks') and its
/// evidence: the run's state first, then the wake's events by severity, cause and recency (decision 11).
fn wake(
    conn: &Connection,
    facts: &mut Facts,
    s: &super::Supervision,
    spec_id: Option<&str>,
    titles: &BTreeMap<String, String>,
) -> Result<(), PersistError> {
    let summary = &s.summary;
    let wake = &summary["wake"];
    facts.question = Some(format!(
        "唤醒 {}（严重度 {}，指纹 {}）",
        s.wake,
        wake["severity"].as_str().unwrap_or("?"),
        wake["fingerprint"].as_str().unwrap_or("?")
    ));
    let total = |key: &str| summary[key].as_u64().unwrap_or(0).to_string();
    facts.header = vec![
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
    // The run's state, before the events.
    for key in ["tasks", "roles", "other_wakes", "recent_rejections"] {
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("summary/{key}"),
            text: summary[key].to_string(),
            summary: None,
            relevance: STATE_RELEVANCE,
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
        let mut payload = event["payload"].clone();
        if let Some(object) = payload.as_object_mut()
            && object.contains_key("task_result_json")
        {
            object.insert(
                "task_result_json".to_owned(),
                json!("（只供引用，见该事件）"),
            );
        }
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
        let brief: BTreeMap<&str, Value> =
            ["task_id", "attempt_id", "reason", "reason_code", "severity"]
                .into_iter()
                .filter_map(|k| Some((k, payload.get(k)?.clone())))
                .collect();
        facts.entries.push(Entry {
            class: Class::Evidence,
            kind: Kind::Plain,
            path: format!("events/{seq}"),
            text: json!({"seq": seq, "kind": kind, "payload": payload}).to_string(),
            summary: Some(json!({"seq": seq, "kind": kind, "brief": brief}).to_string()),
            relevance: band(kind, severity.as_deref()) * 3 + causal,
            recency: seq,
            causal,
        });
    }
    Ok(())
}

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

#[cfg(test)]
mod tests;
