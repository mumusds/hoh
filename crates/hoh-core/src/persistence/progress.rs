//! Closed-round progress facts and their producers, in the existing verification transaction (ADR-037).
use super::writer::{self, Intent, NewEvent};
use super::{PersistError, closing, sqlite_error};
use crate::ids::child_id;
use crate::progress::{Round, Vector, Window};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    requirements: BTreeSet<String>,
    accepted: BTreeSet<String>,
    issues: BTreeMap<String, (String, String)>,
    window: Window,
}
fn exists(tx: &Transaction<'_>, id: &str) -> Result<bool, PersistError> {
    closing::exists(tx, id)
}
fn event(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    id: String,
    kind: &str,
    payload: Value,
) -> Result<(), PersistError> {
    if !exists(tx, &id)? {
        writer::insert_event(
            tx,
            &NewEvent {
                project_id: project.into(),
                run_id: Some(run.into()),
                event_id: id,
                kind: kind.into(),
                payload,
                intent: Intent::Progress,
            },
        )?;
    }
    Ok(())
}
fn grave(state: &(String, String)) -> bool {
    matches!(state.0.as_str(), "OPEN" | "REOPENED")
        && matches!(state.1.as_str(), "critical" | "high")
}
fn open(state: &(String, String)) -> bool {
    matches!(state.0.as_str(), "OPEN" | "REOPENED")
}

pub(super) fn closed(
    tx: &Transaction<'_>,
    run: &str,
    rounds: &[(String, String)],
) -> Result<(), PersistError> {
    let spec:Option<(String,String,i64)>=tx.query_row("SELECT r.project_id,s.spec_id,s.revision FROM runs r JOIN specs s ON s.spec_id=r.spec_id WHERE r.run_id=?1",params![run],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional().map_err(sqlite_error)?;
    let Some((project, spec, revision)) = spec else {
        return Ok(());
    };
    for (round, _) in rounds.iter().filter(|(_, v)| v != "superseded") {
        let id = child_id(round, "progress");
        if exists(tx, &id)? {
            continue;
        }
        let old:Option<String>=tx.query_row("SELECT payload_json FROM events WHERE run_id=?1 AND kind='progress.observed' ORDER BY seq DESC LIMIT 1",params![run],|r|r.get(0)).optional().map_err(sqlite_error)?;
        let previous: Option<Snapshot> = old
            .map(|s| {
                let value: Value = serde_json::from_str(&s)
                    .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
                serde_json::from_value(value["snapshot"].clone())
                    .map_err(|e| PersistError::IntegrityViolation(e.to_string()))
            })
            .transpose()?;
        let mut now = Snapshot::default();
        let required = {
            let mut q=tx.prepare("SELECT requirement_id,required_release FROM requirements WHERE spec_id=?1 ORDER BY requirement_id").map_err(sqlite_error)?;
            let rows = q
                .query_map(params![spec], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                })
                .map_err(sqlite_error)?;
            let mut required = BTreeSet::new();
            for row in rows {
                let (req, release) = row.map_err(sqlite_error)?;
                now.requirements.insert(req.clone());
                if release {
                    required.insert(req);
                }
            }
            required
        };
        let candidates = {
            let mut q = tx
                .prepare("SELECT candidate_id FROM candidates WHERE run_id=?1 AND state='ACCEPTED'")
                .map_err(sqlite_error)?;
            q.query_map(params![run], |r| r.get::<_, String>(0))
                .map_err(sqlite_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(sqlite_error)?
        };
        let mut passed = BTreeSet::new();
        for candidate in candidates {
            passed.extend(super::decide::requirements_passed(
                tx, run, &spec, &candidate,
            )?);
            for ((_, ac), satisfied) in
                super::decide::criteria_satisfied(tx, run, &spec, &candidate)?
            {
                if satisfied {
                    now.accepted.insert(ac);
                }
            }
        }
        {
            let mut q = tx
                .prepare("SELECT issue_id,state,severity FROM issues WHERE run_id=?1")
                .map_err(sqlite_error)?;
            for row in q
                .query_map(params![run], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        (r.get::<_, String>(1)?, r.get::<_, String>(2)?),
                    ))
                })
                .map_err(sqlite_error)?
            {
                let (id, state) = row.map_err(sqlite_error)?;
                now.issues.insert(id, state);
            }
        }
        let baseline = Snapshot::default();
        let before = previous.as_ref().unwrap_or(&baseline);
        let seq: i64 = tx
            .query_row(
                "SELECT seq FROM events WHERE event_id=?1",
                params![child_id(round, "closed")],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        let risks:i64=tx.query_row("SELECT (SELECT count(*) FROM decisions WHERE run_id=?1 AND state='PROPOSED') + (SELECT count(*) FROM tasks WHERE run_id=?1 AND blocked_reason='NEED_DECISION' AND state='BLOCKED')",params![run],|r|r.get(0)).map_err(sqlite_error)?;
        let mut fingerprints = BTreeSet::new();
        if let Some(closed) = closing::rounds(tx, run)?.iter().find(|r| r.id == *round) {
            fingerprints.extend(
                closing::round_issues(tx, closed)?
                    .into_iter()
                    .map(|(_, fp)| fp),
            );
        }
        let failures: Option<String> = tx
            .query_row(
                "SELECT payload_json FROM events WHERE event_id=?1",
                params![child_id(round, "failures")],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if let Some(failures) = failures {
            let failures: Value = serde_json::from_str(&failures)
                .map_err(|e| PersistError::IntegrityViolation(e.to_string()))?;
            for item in failures["items"].as_array().into_iter().flatten() {
                if !item["item"]
                    .as_str()
                    .is_some_and(|i| i.starts_with("issue:"))
                    && let Some(fp) = item["fingerprint"].as_str()
                {
                    fingerprints.insert(fp.into());
                }
            }
        }
        let milestone = child_id(run, &format!("milestone/{spec}"));
        let release = child_id(run, &format!("release/{spec}"));
        let fact = Round {
            seq,
            spec_revision: revision as u64,
            vector: Vector {
                accepted: now.accepted.difference(&before.accepted).count() as u64,
                closed_issues: now
                    .issues
                    .iter()
                    .filter(|(id, s)| s.0 == "RESOLVED" && before.issues.get(*id).is_some_and(open))
                    .count() as u64,
                reopened_issues: now
                    .issues
                    .iter()
                    .filter(|(id, s)| {
                        s.0 == "REOPENED"
                            && before.issues.get(*id).is_some_and(|s| s.0 == "RESOLVED")
                    })
                    .count() as u64,
                new_critical_high: now
                    .issues
                    .iter()
                    .filter(|(id, s)| grave(s) && before.issues.get(*id).is_none_or(|s| !grave(s)))
                    .count() as u64,
                decision_risks: risks as u64,
            },
            open_critical_high: now.issues.values().filter(|s| grave(s)).count() as u64,
            open_issues: now.issues.values().filter(|s| open(s)).count() as u64,
            fingerprints,
            spec_added_requirement_ids: if previous.is_some() {
                now.requirements
                    .difference(&before.requirements)
                    .cloned()
                    .collect()
            } else {
                BTreeSet::new()
            },
            release_stable: exists(tx, &milestone)? || exists(tx, &release)?,
            commit_count: 0,
        };
        now.window = previous.map(|s| s.window).unwrap_or_default();
        let verdict = now.window.observe(fact.clone());
        if let Some(reset) = verdict.reset {
            event(
                tx,
                &project,
                run,
                child_id(round, "progress/reset"),
                "progress.reset",
                reset,
            )?;
        }
        if let Some(mut payload) = verdict.no_progress {
            let suppressed=super::blockers::Claims::load(tx,&project,run)?.held_by("runtime:stalled")
                || tx.query_row("SELECT EXISTS(SELECT 1 FROM supervisor_triggers WHERE run_id=?1 AND state='PENDING' AND fingerprint LIKE 'stalled/%')",params![run],|r|r.get::<_,bool>(0)).map_err(sqlite_error)?;
            if !suppressed {
                payload["spec_revision"] = json!(revision);
                let key = child_id(
                    run,
                    &format!(
                        "no_progress/{}/{}",
                        now.window.last_progress_seq,
                        payload["fingerprint"].as_str().unwrap_or_default()
                    ),
                );
                event(tx, &project, run, key, "no_progress", payload)?;
            }
        }
        if !now.requirements.is_empty() && now.requirements.is_subset(&passed) {
            event(
                tx,
                &project,
                run,
                milestone,
                "milestone_ready",
                json!({"spec_id":spec,"spec_revision":revision}),
            )?;
        }
        if !required.is_empty() && required.is_subset(&passed) && fact.open_critical_high == 0 {
            event(
                tx,
                &project,
                run,
                release,
                "release_request",
                json!({"spec_id":spec,"spec_revision":revision}),
            )?;
        }
        event(
            tx,
            &project,
            run,
            id,
            "progress.observed",
            json!({"round_id":round,"round":fact,"snapshot":now}),
        )?;
    }
    Ok(())
}
