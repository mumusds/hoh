//! Issues (docs/adr/ADR-029.md decisions 9 and 10): a report's findings as Issues and its observations as facts, in its
//! verifier's settle transaction; a failed close's acceptance failures as Issues; what a round's verifiers are asked to
//! confirm fixed and what they confirmed; and `settle_all`, which materialises whether each Issue of a run is resolved
//! (the state function of `verification::issues`) in every transaction that writes a fact, a row, a veto or an Issue.

use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::verification::acceptance;
use crate::verification::issues::{self as state, AcRow, Fact, Judged, Polarity, Subject, World};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// The kind of an observation fact (decision 10).
pub(super) const OBSERVATION: &str = "issue.observation";

/// At most this many confirmation entries are delivered with their description; the rest are listed by id only.
pub(super) const DELIVERED: usize = 50;

/// At most this much of an entry's description is delivered.
const DESCRIPTION_CHARS: usize = 300;

/// A severity's rank: critical first.
fn rank(severity: &str) -> u8 {
    match severity {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    }
}

/// Whether `severity` makes an Issue a failing input (critical or high).
fn grave(severity: &str) -> bool {
    rank(severity) <= 1
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_owned))
        .collect()
}

fn insert_event(
    tx: &Transaction<'_>,
    run_id: &str,
    event_id: &str,
    kind: &str,
    payload: &Value,
) -> Result<(), PersistError> {
    tx.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) \
         SELECT project_id, ?1, run_id, ?2, ?3, ?4 FROM runs WHERE run_id = ?5",
        params![
            event_id,
            kind,
            payload.to_string(),
            crate::time::now_rfc3339(),
            run_id
        ],
    )
    .map_err(sqlite_error)?;
    Ok(())
}

fn exists(conn: &Connection, event_id: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM events WHERE event_id = ?1)",
        params![event_id],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The confirmation entries verifier attempt `attempt_id` was delivered with their description (its `context.packed`
/// `confirm_ids`, fixed at its dispatch).
pub(super) fn delivered(
    conn: &Connection,
    attempt_id: &str,
) -> Result<BTreeSet<String>, PersistError> {
    let payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![child_id(attempt_id, "context.packed")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let payload: Value = payload
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or(Value::Null);
    Ok(strings(&payload["confirm_ids"]).into_iter().collect())
}

/// The confirmations among a verifier's TaskResult `issues` (decision 10): `{issue_id, state: RESOLVED|OPEN, observed}`
/// of an Issue its attempt `attempt_id` was delivered; the others dropped, each with why (a diagnostic, never making
/// the result invalid).
pub(super) fn confirmations(
    conn: &Connection,
    attempt_id: &str,
    given: &[Value],
) -> Result<(Vec<Value>, Vec<Value>), PersistError> {
    let delivered = delivered(conn, attempt_id)?;
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for entry in given {
        let id = entry["issue_id"].as_str();
        let status = entry["state"].as_str();
        let observed = entry["observed"].as_str().filter(|o| !o.trim().is_empty());
        let why = match (id, status, observed) {
            (None, ..) => Some("no issue_id"),
            (Some(id), ..) if !delivered.contains(id) => Some("not delivered to this attempt"),
            (_, Some("RESOLVED" | "OPEN"), Some(_)) => None,
            (_, Some("RESOLVED" | "OPEN"), None) => Some("no observed"),
            _ => Some("state is neither RESOLVED nor OPEN"),
        };
        match (why, id, status, observed) {
            (None, Some(id), Some(status), Some(observed)) => {
                kept.push(json!({"issue_id": id, "state": status, "observed": observed}))
            }
            (why, ..) => {
                dropped.push(json!({"entry": entry, "reason": why.unwrap_or("malformed")}))
            }
        }
    }
    Ok((kept, dropped))
}

/// A report's findings registered as Issues and its observations as present facts (decisions 9 and 10), in its
/// verifier's settle transaction (or `Store::record_report`'s): each finding of report `report_id` (CURRENT or STALE;
/// a REJECTED report has none) one OPEN Issue `child_id(report, "finding/<id>")` on the report's candidate with its
/// `issue.opened{source: finding}` (no round_id); one present fact per fingerprint for attempt `attempt_id`, its
/// sources merged: each finding by its fingerprint, by the fingerprint of the Issue its id is, and each confirmation
/// `OPEN` (of `confirmations`) by its Issue's. A replay writes nothing new. The Issues registered.
pub(super) fn take_report(
    tx: &Transaction<'_>,
    attempt_id: &str,
    report_id: &str,
    report: &Value,
    confirmations: &[Value],
) -> Result<Vec<String>, PersistError> {
    let (run_id, candidate): (String, String) = tx
        .query_row(
            "SELECT run_id, candidate_id FROM independent_reports WHERE report_id = ?1",
            params![report_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let fingerprint_of = |issue: &str| -> Result<Option<String>, PersistError> {
        tx.query_row(
            "SELECT fingerprint FROM issues WHERE issue_id = ?1 AND run_id = ?2",
            params![issue, run_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)
    };
    let mut registered = Vec::new();
    // fingerprint -> (Issue ids, sources)
    let mut present: BTreeMap<String, (BTreeSet<String>, BTreeSet<String>)> = BTreeMap::new();
    for finding in report["findings"].as_array().into_iter().flatten() {
        let id = finding["id"].as_str().unwrap_or_default();
        let severity = finding["severity"].as_str().unwrap_or("medium");
        let summary = finding["summary"].as_str().unwrap_or_default();
        let locator: Vec<String> = finding["evidence"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e["path"].as_str().map(str::to_owned))
            .collect();
        // Reported again by an Issue's id: still present, by that Issue's fingerprint (M2T18D-500, -551).
        if let Some(fingerprint) = fingerprint_of(id)? {
            let (ids, sources) = present.entry(fingerprint).or_default();
            ids.insert(id.to_owned());
            sources.insert(format!("reissued:{id}"));
        }
        let issue_id = child_id(report_id, &format!("finding/{id}"));
        let fingerprint = acceptance::fingerprint(severity, summary, &locator);
        let (ids, sources) = present.entry(fingerprint.clone()).or_default();
        ids.insert(issue_id.clone());
        sources.insert(format!("finding:{id}"));
        let known: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM issues WHERE issue_id = ?1)",
                params![issue_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if known {
            continue;
        }
        tx.execute(
            "INSERT INTO issues(issue_id, run_id, candidate_id, fingerprint, severity, state, description, report_id, \
             finding_id) VALUES (?1, ?2, ?3, ?4, ?5, 'OPEN', ?6, ?7, ?8)",
            params![issue_id, run_id, candidate, fingerprint, severity, summary, report_id, id],
        )
        .map_err(sqlite_error)?;
        // No round_id: a finding belongs to its round by its report (closing.rs `round_issues`), so a low or medium one
        // never fails a round (decision 9).
        insert_event(
            tx,
            &run_id,
            &child_id(&issue_id, "opened"),
            "issue.opened",
            &json!({"issue_id": issue_id, "candidate_id": candidate, "report_id": report_id, "finding_id": id,
                "severity": severity, "fingerprint": fingerprint, "attempt_id": attempt_id, "source": "finding"}),
        )?;
        registered.push(issue_id);
    }
    for confirmation in confirmations.iter().filter(|c| c["state"] == "OPEN") {
        let id = confirmation["issue_id"].as_str().unwrap_or_default();
        if let Some(fingerprint) = fingerprint_of(id)? {
            let (ids, sources) = present.entry(fingerprint).or_default();
            ids.insert(id.to_owned());
            sources.insert(format!("confirmation:{id}"));
        }
    }
    for (fingerprint, (ids, sources)) in present {
        observe(
            tx,
            &run_id,
            &Observed {
                at: attempt_id,
                attempt: true,
                polarity: Polarity::Present,
                fingerprint: &fingerprint,
                candidate: &candidate,
                issue_ids: &ids,
                sources: &sources,
            },
        )?;
    }
    Ok(registered)
}

/// One observation fact to write.
struct Observed<'a> {
    /// The attempt or the round it is of.
    at: &'a str,
    attempt: bool,
    polarity: Polarity,
    fingerprint: &'a str,
    candidate: &'a str,
    issue_ids: &'a BTreeSet<String>,
    sources: &'a BTreeSet<String>,
}

/// Write observation `o` once: `issue.observation` whose id (and event id) is `child_id(<attempt or round>,
/// "observation/<polarity>/<fingerprint>")`; nothing when written before.
fn observe(tx: &Transaction<'_>, run_id: &str, o: &Observed<'_>) -> Result<(), PersistError> {
    let id = child_id(
        o.at,
        &format!("observation/{}/{}", o.polarity.code(), o.fingerprint),
    );
    if exists(tx, &id)? {
        return Ok(());
    }
    let mut payload = json!({"observation_id": id, "fingerprint": o.fingerprint, "polarity": o.polarity.code(),
        "candidate_id": o.candidate, "issue_ids": o.issue_ids, "sources": o.sources});
    payload[if o.attempt { "attempt_id" } else { "round_id" }] = json!(o.at);
    insert_event(tx, run_id, &id, OBSERVATION, &payload)
}

/// An acceptance failure of a failed close: its AC and, when the item names one, its failing test.
pub(super) type AcFailure = (String, Option<String>);

/// The acceptance failures among failure items `items` (`ac-fail:<AC>[:<test>]`).
pub(super) fn ac_failures<'a>(items: impl Iterator<Item = &'a str>) -> BTreeSet<AcFailure> {
    items
        .filter_map(|item| item.strip_prefix("ac-fail:"))
        .map(|rest| match rest.split_once(':') {
            Some((ac, test)) => (ac.to_owned(), Some(test.to_owned())),
            None => (rest.to_owned(), None),
        })
        .collect()
}

/// One Issue per acceptance failure of a failed close on candidate `candidate` (decision 9): `child_id(candidate,
/// "acceptance/<AC>/<test>")`, high, no report, its description naming the AC, the candidate, the test and the evidence
/// of a FAIL row of that test; `issue.opened{source: acceptance}` (no round_id, no report: never a failing input) only
/// when new, so two rounds sharing the AC on one candidate have one.
pub(super) fn open_acceptance(
    tx: &Transaction<'_>,
    run_id: &str,
    candidate: &str,
    failures: &BTreeSet<AcFailure>,
) -> Result<Vec<String>, PersistError> {
    let mut opened = Vec::new();
    for (ac, test) in failures {
        let (suffix, text) = match test {
            Some(t) => (format!("acceptance/{ac}/{t}"), format!("ac-fail {ac} {t}")),
            None => (format!("acceptance/{ac}"), format!("ac-fail {ac}")),
        };
        let issue_id = child_id(candidate, &suffix);
        let fingerprint = acceptance::fingerprint("high", &text, &[]);
        let evidence = failing_evidence(tx, candidate, ac, test.as_deref())?;
        let description = format!(
            "{ac} 在候选 {candidate} 上 FAIL：{}（证据 {}）",
            test.as_deref().unwrap_or("-"),
            evidence.as_deref().unwrap_or("-")
        );
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO issues(issue_id, run_id, candidate_id, fingerprint, severity, state, \
                 description) VALUES (?1, ?2, ?3, ?4, 'high', 'OPEN', ?5)",
                params![issue_id, run_id, candidate, fingerprint, description],
            )
            .map_err(sqlite_error)?;
        if changed == 0 {
            continue;
        }
        insert_event(
            tx,
            run_id,
            &child_id(&issue_id, "opened"),
            "issue.opened",
            &json!({"issue_id": issue_id, "candidate_id": candidate, "severity": "high", "fingerprint": fingerprint,
                "source": "acceptance", "ac_id": ac, "test": test, "evidence_id": evidence}),
        )?;
        opened.push(issue_id);
    }
    Ok(opened)
}

/// The evidence of the first FAIL row of AC `ac` on candidate `candidate` whose failing tests hold `test` (any FAIL
/// row when none is named).
fn failing_evidence(
    conn: &Connection,
    candidate: &str,
    ac: &str,
    test: Option<&str>,
) -> Result<Option<String>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT ar.evidence_id, ar.reason, (SELECT payload_json FROM events WHERE event_id = ar.evidence_id || '/evidence') \
             FROM acceptance_results ar WHERE ar.candidate_id = ?1 AND ar.ac_id = ?2 AND ar.status = 'FAIL' \
             ORDER BY ar.rowid",
        )
        .map_err(sqlite_error)?;
    let rows: Vec<(String, String, Option<String>)> = statement
        .query_map(params![candidate, ac], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    for (evidence, reason, payload) in rows {
        let tests = row_failing(payload.as_deref(), ac, &reason);
        if test.is_none_or(|t| tests.contains(t)) {
            return Ok(Some(evidence));
        }
    }
    Ok(None)
}

/// The failing tests of the FAIL row on AC `ac` that evidence event `payload` recorded (its `failing`, else the TEST its
/// reason names).
fn row_failing(payload: Option<&str>, ac: &str, reason: &str) -> BTreeSet<String> {
    let payload: Value = payload
        .and_then(|p| serde_json::from_str(p).ok())
        .unwrap_or(Value::Null);
    let failing: Vec<String> = payload["rows"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|r| r["ac_id"] == json!(ac))
        .map(|r| strings(&r["failing"]))
        .unwrap_or_default();
    state::failing_tests(&failing, reason)
}

/// One side of a round's close: the attempt of a verifier's latest non-REJECTED report, the Issue ids it was delivered
/// with their description, its confirmations (id, RESOLVED or OPEN) and the fingerprints its settle observed present.
#[derive(Debug, Clone, Default)]
pub(super) struct Side {
    pub attempt_id: String,
    pub delivered: BTreeSet<String>,
    pub confirmations: Vec<(String, String)>,
    pub present: BTreeSet<String>,
}

/// The side of `role` among a round's verifier `attempts` (in their order): none without a non-REJECTED report.
pub(super) fn side(
    conn: &Connection,
    attempts: &[String],
    role: &str,
) -> Result<Option<Side>, PersistError> {
    let mut latest = None;
    for attempt in attempts {
        let reported: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM independent_reports WHERE attempt_id = ?1 AND role = ?2 \
                 AND state <> 'REJECTED')",
                params![attempt, role],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        if reported {
            latest = Some(attempt.clone());
        }
    }
    let Some(attempt_id) = latest else {
        return Ok(None);
    };
    let report: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM events WHERE event_id = ?1",
            params![child_id(&attempt_id, "verification_report")],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    let report: Value = report
        .and_then(|p| serde_json::from_str(&p).ok())
        .unwrap_or(Value::Null);
    let confirmations = report["confirmations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            Some((
                c["issue_id"].as_str()?.to_owned(),
                c["state"].as_str()?.to_owned(),
            ))
        })
        .collect();
    let mut statement = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.fingerprint') FROM events \
             WHERE run_id = (SELECT t.run_id FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE a.attempt_id = ?2) \
             AND kind = ?1 AND json_extract(payload_json, '$.attempt_id') = ?2 \
             AND json_extract(payload_json, '$.polarity') = 'present'",
        )
        .map_err(sqlite_error)?;
    let present = statement
        .query_map(params![OBSERVATION, attempt_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(Some(Side {
        delivered: delivered(conn, &attempt_id)?,
        attempt_id,
        confirmations,
        present,
    }))
}

/// A confirmation entry as a round's close judges it (decision 10): the Issues of one fingerprint delivered to its
/// verifiers.
#[derive(Debug, Clone)]
pub(super) struct Entry {
    pub fingerprint: String,
    /// The highest severity of its delivered Issues.
    pub severity: String,
    /// Its Issue ids each side was delivered with their description.
    pub ids: [BTreeSet<String>; 2],
    /// A side confirmed one of its Issues RESOLVED.
    pub resolved: bool,
    /// The round's own settles observed it present (a confirmation OPEN, or reported again by id or fingerprint).
    pub reported: bool,
    /// A present fact of its fingerprint on the round's candidate or a later one (`still_present`).
    pub still_present: bool,
}

impl Entry {
    /// The Issue ids both sides were delivered.
    pub fn both(&self) -> BTreeSet<String> {
        self.ids[0].intersection(&self.ids[1]).cloned().collect()
    }

    /// `confirmed(X)`: delivered to both, one confirmed it fixed, not still present.
    pub fn confirmed(&self) -> bool {
        !self.both().is_empty() && self.resolved && !self.still_present
    }

    /// `unconfirmed(X)`: both sides were delivered one of its Issues, it is neither still present nor confirmed.
    pub fn unconfirmed(&self) -> bool {
        !self.ids[0].is_empty()
            && !self.ids[1].is_empty()
            && !self.still_present
            && !self.confirmed()
    }

    /// Whether it is critical or high: a failing input when still present, unconfirmed leaves the round unpassable.
    pub fn grave(&self) -> bool {
        grave(&self.severity)
    }
}

/// The confirmation entries delivered to a round on candidate `candidate` of run `run_id`, from its `sides` (QA's,
/// the reviewer's), by fingerprint.
pub(super) fn entries(
    conn: &Connection,
    run_id: &str,
    candidate: &str,
    sides: &[Option<Side>; 2],
) -> Result<Vec<Entry>, PersistError> {
    let mut by: BTreeMap<String, Entry> = BTreeMap::new();
    for (n, side) in sides.iter().enumerate() {
        let Some(side) = side else { continue };
        for id in &side.delivered {
            let found: Option<(String, String)> = conn
                .query_row(
                    "SELECT fingerprint, severity FROM issues WHERE issue_id = ?1 AND run_id = ?2",
                    params![id, run_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(sqlite_error)?;
            let Some((fingerprint, severity)) = found else {
                continue;
            };
            let entry = by.entry(fingerprint.clone()).or_insert_with(|| Entry {
                fingerprint: fingerprint.clone(),
                severity: severity.clone(),
                ids: Default::default(),
                resolved: false,
                reported: false,
                still_present: false,
            });
            if rank(&severity) < rank(&entry.severity) {
                entry.severity = severity;
            }
            entry.ids[n].insert(id.clone());
        }
    }
    let world = world(conn, run_id, false)?;
    for entry in by.values_mut() {
        for (n, side) in sides.iter().enumerate() {
            let Some(side) = side else { continue };
            for (id, status) in &side.confirmations {
                if entry.ids[n].contains(id) && status == "RESOLVED" {
                    entry.resolved = true;
                }
            }
            if side.present.contains(&entry.fingerprint) {
                entry.reported = true;
            }
        }
        entry.still_present = world.facts.iter().any(|f| {
            f.polarity == Polarity::Present
                && f.fingerprint == entry.fingerprint
                && (f.candidate == candidate
                    || matches!((world.order.get(&f.candidate), world.order.get(candidate)),
                        (Some(a), Some(b)) if a > b))
        });
    }
    Ok(by.into_values().collect())
}

/// The absent facts of a round's close (decision 10, any verdict): with both sides' non-REJECTED reports, each entry
/// both were delivered one of whose Issues a side confirmed RESOLVED, and neither observed present, is absent on the
/// round's candidate for the Issue ids both were delivered.
pub(super) fn observe_absent(
    tx: &Transaction<'_>,
    run_id: &str,
    round_id: &str,
    candidate: &str,
    sides: &[Option<Side>; 2],
    entries: &[Entry],
) -> Result<(), PersistError> {
    let [Some(qa), Some(reviewer)] = sides else {
        return Ok(());
    };
    for entry in entries {
        let both = entry.both();
        if both.is_empty() || !entry.resolved || entry.reported {
            continue;
        }
        let sources: BTreeSet<String> = [qa, reviewer]
            .iter()
            .zip(&entry.ids)
            .filter(|(s, ids)| {
                s.confirmations
                    .iter()
                    .any(|(id, status)| ids.contains(id) && status == "RESOLVED")
            })
            .map(|(s, _)| format!("confirmation:{}", s.attempt_id))
            .collect();
        observe(
            tx,
            run_id,
            &Observed {
                at: round_id,
                attempt: false,
                polarity: Polarity::Absent,
                fingerprint: &entry.fingerprint,
                candidate,
                issue_ids: &both,
                sources: &sources,
            },
        )?;
    }
    Ok(())
}

/// One Issue of a run as `settle_all` reads it.
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub issue: state::Issue,
    pub state: String,
}

/// Run `run_id`'s Issues and its world: the freeze order, the vetoes, the observation facts (a change request's
/// `issue.opened` read as present, M2T18D-701, -800) and, when `rows` or an acceptance Issue needs them, the acceptance
/// rows of its candidates.
fn world(conn: &Connection, run_id: &str, rows: bool) -> Result<World, PersistError> {
    let mut world = World::default();
    let mut statement = conn
        .prepare(
            "SELECT json_extract(payload_json, '$.candidate_id'), min(seq) FROM events \
             WHERE run_id = ?1 AND kind = 'candidate.frozen' GROUP BY 1",
        )
        .map_err(sqlite_error)?;
    world.order = statement
        .query_map(params![run_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT DISTINCT ev.candidate_id FROM evidence ev JOIN candidates c ON c.candidate_id = ev.candidate_id \
             WHERE c.run_id = ?1 AND ev.outcome = 'INVALID'",
        )
        .map_err(sqlite_error)?;
    world.vetoed = statement
        .query_map(params![run_id], |r| r.get(0))
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut statement = conn
        .prepare(
            "SELECT event_id, kind, payload_json FROM events WHERE run_id = ?1 AND (kind = ?2 OR (kind = 'issue.opened' \
             AND json_extract(payload_json, '$.source') = 'change_request')) ORDER BY seq",
        )
        .map_err(sqlite_error)?;
    let events: Vec<(String, String, String)> = statement
        .query_map(params![run_id, OBSERVATION], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    for (event_id, kind, payload) in events {
        let p: Value = serde_json::from_str(&payload).unwrap_or(Value::Null);
        let text = |key: &str| p[key].as_str().unwrap_or_default().to_owned();
        world.facts.push(if kind == OBSERVATION {
            Fact {
                id: text("observation_id"),
                fingerprint: text("fingerprint"),
                polarity: if p["polarity"] == "absent" {
                    Polarity::Absent
                } else {
                    Polarity::Present
                },
                candidate: text("candidate_id"),
                issue_ids: strings(&p["issue_ids"]).into_iter().collect(),
            }
        } else {
            Fact {
                id: event_id,
                fingerprint: text("fingerprint"),
                polarity: Polarity::Present,
                candidate: text("candidate_id"),
                issue_ids: [text("issue_id")].into_iter().collect(),
            }
        });
    }
    if rows {
        let mut statement = conn
            .prepare(
                "SELECT ar.ac_id, ar.candidate_id, ar.status, ar.reason, \
                 coalesce(s.sha256 = c.spec_sha256, 0), \
                 coalesce((SELECT r.release_class FROM acceptance_criteria a JOIN requirements r \
                   ON r.requirement_id = a.requirement_id AND r.spec_id = a.spec_id \
                   WHERE a.ac_id = ar.ac_id AND a.spec_id = ar.spec_id), '') = 'degradable', \
                 (SELECT payload_json FROM events WHERE event_id = ar.evidence_id || '/evidence') \
                 FROM acceptance_results ar JOIN candidates c ON c.candidate_id = ar.candidate_id \
                 LEFT JOIN specs s ON s.spec_id = ar.spec_id WHERE c.run_id = ?1 ORDER BY ar.rowid",
            )
            .map_err(sqlite_error)?;
        world.rows = statement
            .query_map(params![run_id], |r| {
                let (ac, status, reason): (String, String, String) =
                    (r.get(0)?, r.get(2)?, r.get(3)?);
                let payload: Option<String> = r.get(6)?;
                Ok(AcRow {
                    failing: if status == "FAIL" {
                        row_failing(payload.as_deref(), &ac, &reason)
                    } else {
                        BTreeSet::new()
                    },
                    status: super::closing::row_of(&status),
                    candidate: r.get(1)?,
                    own_spec: r.get(4)?,
                    degradable: r.get(5)?,
                    ac,
                })
            })
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
    }
    Ok(world)
}

/// Run `run_id`'s Issues as the state function reads them: an acceptance failure's (its `issue.opened` says so) by its
/// AC and test, any other by its fingerprint; with its state now.
fn issues(conn: &Connection, run_id: &str) -> Result<Vec<Row>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT i.issue_id, coalesce(i.candidate_id, ''), i.fingerprint, i.state, \
             (SELECT e.payload_json FROM events e WHERE e.run_id = i.run_id AND e.kind = 'issue.opened' \
               AND json_extract(e.payload_json, '$.issue_id') = i.issue_id ORDER BY e.seq LIMIT 1) \
             FROM issues i WHERE i.run_id = ?1 ORDER BY i.issue_id",
        )
        .map_err(sqlite_error)?;
    let found: Vec<(String, String, String, String, Option<String>)> = statement
        .query_map(params![run_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    Ok(found
        .into_iter()
        .map(|(id, candidate, fingerprint, state, opened)| {
            let opened: Value = opened
                .and_then(|p| serde_json::from_str(&p).ok())
                .unwrap_or(Value::Null);
            let subject = match (opened["source"].as_str(), opened["ac_id"].as_str()) {
                (Some("acceptance"), Some(ac)) => Subject::Acceptance {
                    ac: ac.to_owned(),
                    test: opened["test"].as_str().map(str::to_owned),
                },
                _ => Subject::Observed { fingerprint },
            };
            Row {
                issue: state::Issue {
                    id,
                    candidate,
                    subject,
                },
                state,
            }
        })
        .collect())
}

/// Whether each Issue of run `run_id` is resolved, evaluated now (decision 10's deciding readers): its state as read
/// and the judgement.
pub(super) fn judged(conn: &Connection, run_id: &str) -> Result<Vec<(Row, Judged)>, PersistError> {
    let issues = issues(conn, run_id)?;
    let rows = issues
        .iter()
        .any(|i| matches!(i.issue.subject, Subject::Acceptance { .. }));
    let world = world(conn, run_id, rows)?;
    Ok(issues
        .into_iter()
        .map(|row| {
            let judged = state::judge(&row.issue, &world);
            (row, judged)
        })
        .collect())
}

/// Materialise whether each Issue of run `run_id` is resolved (decision 10, `issues::settle_all`): only an Issue whose
/// state differs from the function's changes, RESOLVED with `issue.resolved{issue_id, basis}` or REOPENED with
/// `issue.reopened{issue_id, basis, undoes}`, each event's id from its Issue and basis. Called before the commit of
/// every transaction that writes an observation fact, an acceptance row, an INVALID evidence or an Issue, and at the
/// start of each verification reconcile, so a committed state agrees with the function. The changes, (Issue, state).
pub(super) fn settle_all(
    tx: &Transaction<'_>,
    run_id: &str,
) -> Result<Vec<(String, &'static str)>, PersistError> {
    let mut changed = Vec::new();
    for (row, judged) in judged(tx, run_id)? {
        let last: Option<String> = if row.state == "RESOLVED" && !judged.resolved() {
            tx.query_row(
                "SELECT json_extract(payload_json, '$.basis') FROM events WHERE run_id = ?1 AND kind = 'issue.resolved' \
                 AND json_extract(payload_json, '$.issue_id') = ?2 ORDER BY seq DESC LIMIT 1",
                params![run_id, row.issue.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?
        } else {
            None
        };
        let Some(t) = state::transition(&row.state, &judged, last.as_deref()) else {
            continue;
        };
        tx.execute(
            "UPDATE issues SET state = ?2 WHERE issue_id = ?1",
            params![row.issue.id, t.to],
        )
        .map_err(sqlite_error)?;
        let mut payload = json!({"issue_id": row.issue.id, "basis": t.basis});
        if let Some(undoes) = &t.undoes {
            payload["undoes"] = json!(undoes);
        }
        insert_event(
            tx,
            run_id,
            &child_id(&row.issue.id, &t.key),
            t.kind,
            &payload,
        )?;
        changed.push((row.issue.id, t.to));
    }
    Ok(changed)
}

/// An Issue a round's verifiers may confirm fixed (decision 10's `confirm_set`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Confirmable {
    pub issue_id: String,
    pub fingerprint: String,
    pub severity: String,
    pub description: String,
    /// The seq of its `issue.opened`.
    pub opened: i64,
}

/// `confirm_set(run, sources, candidate, before_seq)` (decision 10): the finding and change-request Issues of run
/// `run_id` OPEN or REOPENED, raised on another candidate than `candidate`, by a round opened before `before_seq` that
/// had one of `sources` among its own and is closed (a change request's by its `issue.opened` round, a finding's by its
/// report's attempt); acceptance failures are never in it.
pub(super) fn confirm_set(
    conn: &Connection,
    run_id: &str,
    sources: &[String],
    candidate: &str,
    before_seq: i64,
) -> Result<Vec<Confirmable>, PersistError> {
    let mut statement = conn
        .prepare(
            "WITH opened AS (SELECT e.seq, json_extract(e.payload_json, '$.issue_id') AS issue_id, \
               coalesce(json_extract(e.payload_json, '$.round_id'), \
                 (SELECT json_extract(o.payload_json, '$.round_id') FROM events o JOIN attempts a \
                   ON a.attempt_id = json_extract(e.payload_json, '$.attempt_id') \
                   WHERE o.run_id = ?1 AND o.kind = 'verification.round_opened' \
                   AND a.task_id IN (json_extract(o.payload_json, '$.tasks.qa'), \
                     json_extract(o.payload_json, '$.tasks.reviewer')))) AS round_id \
               FROM events e WHERE e.run_id = ?1 AND e.kind = 'issue.opened' \
               AND json_extract(e.payload_json, '$.source') IN ('finding', 'change_request')) \
             SELECT i.issue_id, i.fingerprint, i.severity, i.description, op.seq FROM issues i \
             JOIN opened op ON op.issue_id = i.issue_id \
             JOIN events r ON r.run_id = ?1 AND r.kind = 'verification.round_opened' \
               AND json_extract(r.payload_json, '$.round_id') = op.round_id \
             WHERE i.run_id = ?1 AND i.state IN ('OPEN', 'REOPENED') AND coalesce(i.candidate_id, '') <> ?2 \
             AND r.seq < ?3 \
             AND EXISTS (SELECT 1 FROM json_each(r.payload_json, '$.source_task_ids') s \
               WHERE s.value IN (SELECT value FROM json_each(?4))) \
             AND EXISTS (SELECT 1 FROM events c WHERE c.run_id = ?1 AND c.kind = 'verification.round_closed' \
               AND json_extract(c.payload_json, '$.round_id') = op.round_id) \
             ORDER BY op.seq, i.issue_id",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(
            params![run_id, candidate, before_seq, json!(sources).to_string()],
            |r| {
                Ok(Confirmable {
                    issue_id: r.get(0)?,
                    fingerprint: r.get(1)?,
                    severity: r.get(2)?,
                    description: r.get(3)?,
                    opened: r.get(4)?,
                })
            },
        )
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// What a round's verifier is given to confirm (decision 10): its mandatory scope's lines, every entry of the round's
/// `confirm_set` (one per fingerprint, severity then opening order), the first [`DELIVERED`] with their description,
/// the rest by id only; and the Issue ids delivered with their description (`confirm_ids`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Delivery {
    pub lines: Vec<String>,
    pub confirm_ids: Vec<String>,
}

/// [`Delivery`] for a verifier of round `round_id` of run `run_id` whose sources are `sources`.
pub(super) fn deliver(
    conn: &Connection,
    run_id: &str,
    round_id: &str,
    sources: &[String],
) -> Result<Delivery, PersistError> {
    let opened: Option<(i64, String)> = conn
        .query_row(
            "SELECT seq, json_extract(payload_json, '$.candidate_id') FROM events WHERE run_id = ?1 \
             AND kind = 'verification.round_opened' AND json_extract(payload_json, '$.round_id') = ?2",
            params![run_id, round_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((seq, candidate)) = opened else {
        return Ok(Delivery::default());
    };
    let set = confirm_set(conn, run_id, sources, &candidate, seq)?;
    let mut entries: BTreeMap<&str, Vec<&Confirmable>> = BTreeMap::new();
    for issue in &set {
        entries
            .entry(issue.fingerprint.as_str())
            .or_default()
            .push(issue);
    }
    let mut entries: Vec<Vec<&Confirmable>> = entries.into_values().collect();
    let severity = |e: &[&Confirmable]| e.iter().map(|i| rank(&i.severity)).min().unwrap_or(3);
    let first = |e: &[&Confirmable]| e.iter().map(|i| i.opened).min().unwrap_or(0);
    entries.sort_by_key(|e| (severity(e), first(e)));
    let mut delivery = Delivery::default();
    if entries.is_empty() {
        return Ok(delivery);
    }
    delivery.lines.push(format!(
        "请确认已修复（{} 条；带描述的 {} 条逐条在 TaskResult.issues 写 {{\"issue_id\", \"state\": \"RESOLVED\" 或 \"OPEN\", \
         \"observed\"}}；只列 id 的本轮不确认）",
        entries.len(),
        entries.len().min(DELIVERED)
    ));
    for (n, entry) in entries.iter().enumerate() {
        let ids: Vec<&str> = entry.iter().map(|i| i.issue_id.as_str()).collect();
        let highest = match entry.iter().min_by_key(|i| rank(&i.severity)) {
            Some(i) => i.severity.as_str(),
            None => "-",
        };
        if n < DELIVERED {
            let description: String = entry
                .first()
                .map(|i| i.description.chars().take(DESCRIPTION_CHARS).collect())
                .unwrap_or_default();
            delivery.lines.push(format!(
                "请确认已修复：Issue {}（{highest}）：{description}",
                ids.join("、")
            ));
            delivery
                .confirm_ids
                .extend(ids.iter().map(|i| (*i).to_owned()));
        } else {
            delivery.lines.push(format!(
                "下一批再确认（本轮只列出）：Issue {}（{highest}）",
                ids.join("、")
            ));
        }
    }
    Ok(delivery)
}

/// The Issues of fingerprint `fingerprint` across run `run_id` (decision 11's rework context, M2T18D-201): (issue,
/// severity, description, candidate, report, finding), in their order.
pub(super) fn of_fingerprint(
    conn: &Connection,
    run_id: &str,
    fingerprint: &str,
) -> Result<Vec<Traced>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT issue_id, severity, description, candidate_id, report_id, finding_id FROM issues \
             WHERE run_id = ?1 AND fingerprint = ?2 ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id, fingerprint], |r| {
            Ok(Traced {
                fingerprint: fingerprint.to_owned(),
                issue_id: r.get(0)?,
                severity: r.get(1)?,
                description: r.get(2)?,
                candidate_id: r.get(3)?,
                report_id: r.get(4)?,
                finding_id: r.get(5)?,
                observed: Vec::new(),
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// An Issue a rework's `issue:<fingerprint>` item names, with where it came from and what the verifiers observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traced {
    pub fingerprint: String,
    pub issue_id: String,
    pub severity: String,
    pub description: String,
    pub candidate_id: Option<String>,
    pub report_id: Option<String>,
    pub finding_id: Option<String>,
    /// The `observed` of the confirmations of it in the rounds whose failures the rework took.
    pub observed: Vec<String>,
}

#[cfg(test)]
mod tests;
