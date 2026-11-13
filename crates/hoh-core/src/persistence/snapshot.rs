//! What a run's acceptance looks like now (docs/adr/ADR-029.md decisions 3, 5 and 15): the snapshot of its latest
//! candidate (`Reader::acceptance`), every candidate's projection and the Issues as the two HTTP lists give them, and a
//! finding's Issue traced to its report and raw session. Read only, each in one read transaction; the labels come from
//! the display projection and the Issues' states from the state function, evaluated here, never from a materialised
//! column.

use super::closing::{self, Round};
use super::{PersistError, sqlite_error};
use crate::verification::acceptance::{self, Label, Observation, RowFact, Standing, Why};
use crate::verification::classify::Report;
use crate::verification::report::{ChainIssue, ChainReport, IssueChain, ObjectRef};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};

/// The snapshot of a run's acceptance on its latest candidate (docs/adr/ADR-029.md decision 5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceView {
    pub run_id: String,
    pub spec_id: Option<String>,
    /// The run's latest candidate (its last `candidate.frozen`, whatever its state); none before any.
    pub candidate: Option<CandidateShown>,
    /// Every AC in the run's scope (its development tasks' requirements under its specification), by id.
    pub acs: Vec<AcShown>,
    /// The counted ACs whose label satisfies them.
    pub passing: usize,
    /// `passing` when the candidate is ACCEPTED, else 0.
    pub accepted: usize,
    /// The counted ACs.
    pub total: usize,
    #[serde(serialize_with = "report_code")]
    pub qa: Report,
    #[serde(serialize_with = "report_code")]
    pub review: Report,
    /// Every check with evidence on the candidate, by id.
    pub checks: Vec<CheckShown>,
    /// The event seq the snapshot is consistent with.
    pub watermark: i64,
}

/// A candidate and its state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateShown {
    pub candidate_id: String,
    pub state: String,
}

/// One AC of a snapshot: its label and reason, its rows on the candidate, and whether it is counted (a development
/// task not CANCELLED has it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcShown {
    pub ac_id: String,
    pub requirement_id: String,
    pub label: Label,
    pub why: Option<Why>,
    pub rows: Vec<RowShown>,
    pub counted: bool,
}

/// One acceptance row as a snapshot shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RowShown {
    pub status: String,
    pub evidence_id: String,
    pub reason: Option<String>,
}

/// One check's label over its evidence on the candidate (docs/adr/ADR-029.md decision 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckShown {
    pub check_id: String,
    pub label: Label,
}

/// One item of `GET /v1/runs/{run_id}/acceptance`: an AC in the run's scope on one of its candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcceptanceItem {
    pub spec_id: String,
    pub spec_revision: i64,
    pub candidate_id: String,
    pub candidate_state: String,
    pub ac_id: String,
    pub label: Label,
    pub why: Option<Why>,
    /// The candidate is the run's latest.
    pub latest: bool,
}

/// A page of [`AcceptanceItem`]s, newest candidate first, each candidate's ACs by id: the next page's cursor (the last
/// item's freeze seq and AC) when there are more, and the watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptancePage {
    pub items: Vec<AcceptanceItem>,
    /// The last item's candidate (freeze seq, row) and AC.
    pub next: Option<(i64, i64, String)>,
    pub watermark: i64,
}

/// One item of `GET /v1/runs/{run_id}/issues`: its state evaluated by the state function now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IssueItem {
    pub issue_id: String,
    pub candidate_id: Option<String>,
    pub severity: String,
    pub state: String,
    pub fingerprint: String,
    pub description: String,
    pub report_id: Option<String>,
    pub finding_id: Option<String>,
}

/// A page of [`IssueItem`]s, newest first: the next page's cursor (the last one's rowid) when there may be more, and
/// the watermark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesPage {
    pub items: Vec<IssueItem>,
    pub next: Option<i64>,
    pub watermark: i64,
}

fn report_code<S: serde::Serializer>(report: &Report, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(match report {
        Report::Pass => "PASS",
        Report::Fail => "FAIL",
        Report::Unknown => "UNKNOWN",
        Report::NotRun => "NOT_RUN",
    })
}

/// Run `conn`'s read in one read transaction.
fn read<T>(
    conn: &Connection,
    f: impl FnOnce() -> Result<T, PersistError>,
) -> Result<T, PersistError> {
    conn.execute_batch("BEGIN").map_err(sqlite_error)?;
    let read = f();
    // Ends the read transaction either way; nothing was written.
    let _ = conn.execute_batch("COMMIT");
    read
}

/// The run's specification: its id and revision; none when the run has none. None at all when there is no such run.
fn run_spec(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<Option<(String, i64)>>, PersistError> {
    let found: Option<(Option<String>, Option<i64>)> = conn
        .query_row(
            "SELECT r.spec_id, s.revision FROM runs r LEFT JOIN specs s ON s.spec_id = r.spec_id WHERE r.run_id = ?1",
            params![run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(found.map(|(spec, revision)| spec.zip(revision)))
}

/// An AC in a run's scope (decision 5): its requirement, whether its requirement may degrade, and whether a
/// development task not CANCELLED has it.
struct InScope {
    ac: String,
    requirement: String,
    degradable: bool,
    counted: bool,
}

/// The ACs in run `run_id`'s scope under specification `spec_id`, by id: its development tasks' (never a verifier
/// task's, J7) requirements' criteria; counted when one of those tasks is not CANCELLED (-102).
fn scope(conn: &Connection, run_id: &str, spec_id: &str) -> Result<Vec<InScope>, PersistError> {
    let verifiers = super::verification::verifier_tasks(conn, run_id)?;
    let mut statement = conn
        .prepare(
            "SELECT t.task_id, t.state, a.ac_id, a.requirement_id, q.release_class = 'degradable' \
             FROM tasks t JOIN task_requirements tr ON tr.task_id = t.task_id AND tr.spec_id = ?2 \
             JOIN acceptance_criteria a ON a.requirement_id = tr.requirement_id AND a.spec_id = tr.spec_id \
             JOIN requirements q ON q.requirement_id = a.requirement_id AND q.spec_id = a.spec_id \
             WHERE t.run_id = ?1 ORDER BY a.ac_id, t.task_id",
        )
        .map_err(sqlite_error)?;
    let found: Vec<(String, String, String, String, bool)> = statement
        .query_map(params![run_id, spec_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(sqlite_error)?
        .collect::<Result<_, _>>()
        .map_err(sqlite_error)?;
    let mut acs: BTreeMap<String, InScope> = BTreeMap::new();
    for (task, state, ac, requirement, degradable) in found {
        if verifiers.contains(&task) {
            continue;
        }
        let counted = state != "CANCELLED";
        acs.entry(ac.clone())
            .and_modify(|a| a.counted |= counted)
            .or_insert(InScope {
                ac,
                requirement,
                degradable,
                counted,
            });
    }
    Ok(acs.into_values().collect())
}

/// A candidate of a run: its id, state and its `candidate.frozen`'s seq (0 without one).
struct Frozen {
    id: String,
    state: String,
    seq: i64,
    /// Its row, which orders candidates with the same freeze seq (none written: older or imported data, M2T18R-53).
    rowid: i64,
}

/// Run `run_id`'s candidates, the latest first: by their `candidate.frozen`, the run's latest candidate being the one
/// its last `candidate.frozen` names, whatever its state (decision 5; `outbox::current_candidate` is empty once the
/// join accepted it).
fn candidates(conn: &Connection, run_id: &str) -> Result<Vec<Frozen>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT c.candidate_id, c.state, coalesce((SELECT max(e.seq) FROM events e WHERE e.run_id = c.run_id \
               AND e.kind = 'candidate.frozen' AND json_extract(e.payload_json, '$.candidate_id') = c.candidate_id), 0) \
               AS frozen, c.rowid \
             FROM candidates c WHERE c.run_id = ?1 ORDER BY frozen DESC, c.rowid DESC",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![run_id], |r| {
            Ok(Frozen {
                id: r.get(0)?,
                state: r.get(1)?,
                seq: r.get(2)?,
                rowid: r.get(3)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// What candidate `c`'s state and veto let its rows say (decision 2; J2): INVALID, or with an INVALID evidence
/// (decision 10's veto, -752), is Invalid; STALE is Stale; any other state lets the rows decide.
fn standing(conn: &Connection, c: &Frozen) -> Result<Standing, PersistError> {
    let vetoed: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM evidence WHERE candidate_id = ?1 AND outcome = 'INVALID')",
            params![c.id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok(match c.state.as_str() {
        _ if vetoed => Standing::Invalid,
        "INVALID" => Standing::Invalid,
        "STALE" => Standing::Stale,
        _ => Standing::Usable,
    })
}

/// Every row of AC `ac` on candidate `candidate` under specification `spec_id`, in their order.
fn rows(
    conn: &Connection,
    candidate: &str,
    ac: &str,
    spec_id: &str,
) -> Result<Vec<RowShown>, PersistError> {
    let mut statement = conn
        .prepare(
            "SELECT status, evidence_id, reason FROM acceptance_results WHERE candidate_id = ?1 AND ac_id = ?2 \
             AND spec_id = ?3 ORDER BY rowid",
        )
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![candidate, ac, spec_id], |r| {
            Ok(RowShown {
                status: r.get(0)?,
                evidence_id: r.get(1)?,
                reason: r.get(2)?,
            })
        })
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The ACs with an unobserved claim on candidate `candidate` (decision 3, REQ-053 failure mode 2): those in the scope
/// of a closed round on it whose verdict is not superseded, whose QA handed in a report that is not REJECTED (any
/// status), whose verifier attempts have no evidence of the AC's scenario check (whatever its outcome) and no row of the
/// AC on the candidate from their evidence. `checks` names each AC's scenario check.
fn claimed(
    conn: &Connection,
    run_id: &str,
    candidate: &str,
    rounds: &[Round],
    checks: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>, PersistError> {
    let verdicts = closing::closed(conn, run_id)?;
    let mut out = BTreeSet::new();
    for round in rounds.iter().filter(|r| r.candidate == candidate) {
        if verdicts
            .get(&round.id)
            .is_none_or(|verdict| verdict == "superseded")
        {
            continue;
        }
        let attempts = json!(closing::attempts(conn, round)?).to_string();
        let (reported, seen, observed): (bool, String, String) = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM independent_reports r JOIN attempts a ON a.attempt_id = r.attempt_id \
                   WHERE a.task_id = ?1 AND r.role = 'qa' AND r.state <> 'REJECTED'), \
                 (SELECT json_group_array(DISTINCT check_id) FROM evidence \
                   WHERE attempt_id IN (SELECT value FROM json_each(?2))), \
                 (SELECT json_group_array(DISTINCT ar.ac_id) FROM acceptance_results ar \
                   JOIN evidence ev ON ev.evidence_id = ar.evidence_id \
                   WHERE ar.candidate_id = ?3 AND ev.attempt_id IN (SELECT value FROM json_each(?2)))",
                params![round.qa, attempts, candidate],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(sqlite_error)?;
        if !reported {
            continue;
        }
        let seen: BTreeSet<String> = serde_json::from_str(&seen).unwrap_or_default();
        let observed: BTreeSet<String> = serde_json::from_str(&observed).unwrap_or_default();
        for ac in &round.scope {
            let checked = checks.get(ac).is_some_and(|check| seen.contains(check));
            if !checked && !observed.contains(ac) {
                out.insert(ac.clone());
            }
        }
    }
    Ok(out)
}

/// Every AC of specification `spec_id` with its scenario check.
fn scenario_checks(
    conn: &Connection,
    spec_id: &str,
) -> Result<BTreeMap<String, String>, PersistError> {
    let mut statement = conn
        .prepare("SELECT ac_id, check_id FROM acceptance_criteria WHERE spec_id = ?1")
        .map_err(sqlite_error)?;
    let rows = statement
        .query_map(params![spec_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sqlite_error)?;
    rows.collect::<Result<_, _>>().map_err(sqlite_error)
}

/// The ACs of `scope` on candidate `c` as the display projection shows them (decision 2, no profile), with an
/// unobserved claim where decision 3 finds one.
fn shown(
    conn: &Connection,
    run_id: &str,
    spec_id: &str,
    c: &Frozen,
    scope: &[InScope],
    rounds: &[Round],
) -> Result<Vec<AcShown>, PersistError> {
    let standing = standing(conn, c)?;
    let claims = claimed(
        conn,
        run_id,
        &c.id,
        rounds,
        &scenario_checks(conn, spec_id)?,
    )?;
    let mut out = Vec::with_capacity(scope.len());
    for a in scope {
        let rows = rows(conn, &c.id, &a.ac, spec_id)?;
        let observation = Observation {
            rows: rows
                .iter()
                .map(|r| RowFact::of(closing::row_of(&r.status)))
                .collect(),
            degradable: a.degradable,
            unobserved_claim: claims.contains(&a.ac),
        };
        let projected = acceptance::display(standing, &observation, None);
        out.push(AcShown {
            ac_id: a.ac.clone(),
            requirement_id: a.requirement.clone(),
            label: projected.label,
            why: projected.why,
            rows,
            counted: a.counted,
        });
    }
    Ok(out)
}

/// QA's and the reviewer's report status on candidate `candidate` (decision 5, the join's `report_status`): each
/// development task's current round on it (each reverification chain's, its task not CANCELLED), the latest CURRENT
/// report of the role in each, merged FAIL > UNKNOWN > NOT_RUN > PASS. QA's PASS is UNKNOWN when the union of those
/// rounds' scopes is empty or one of its ACs has no row on the candidate (REQ-053 behaviour 8; -163, -852); any other
/// status as it is (-203, -255).
fn reports(
    conn: &Connection,
    candidate: &str,
    spec_id: Option<&str>,
    rounds: &[Round],
) -> Result<(Report, Report), PersistError> {
    let sources: BTreeSet<&str> = rounds
        .iter()
        .filter(|r| r.candidate == candidate)
        .flat_map(|r| r.sources.iter().map(String::as_str))
        .collect();
    let mut current: Vec<&Round> = Vec::new();
    for task in sources {
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM tasks WHERE task_id = ?1",
                params![task],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if state.as_deref() == Some("CANCELLED") {
            continue;
        }
        if let Some(round) = closing::current(rounds, task).filter(|r| r.candidate == candidate)
            && !current.iter().any(|r| r.id == round.id)
        {
            current.push(round);
        }
    }
    current.sort_by_key(|r| r.seq);
    let mut attempts = Vec::with_capacity(current.len());
    for round in &current {
        attempts.push(closing::attempts(conn, round)?);
    }
    let (mut qa, _) = super::reports::report_status(conn, &attempts, "qa")?;
    let (review, _) = super::reports::report_status(conn, &attempts, "reviewer")?;
    if qa == Report::Pass {
        let union: BTreeSet<&String> = current.iter().flat_map(|r| r.scope.iter()).collect();
        let mut unobserved = union.is_empty();
        for ac in union {
            unobserved |= match spec_id {
                Some(spec_id) => rows(conn, candidate, ac, spec_id)?.is_empty(),
                None => true,
            };
        }
        if unobserved {
            qa = Report::Unknown;
        }
    }
    Ok((qa, review))
}

/// Each check with evidence on candidate `candidate`, labelled over all its runs there (decision 4).
fn checks(conn: &Connection, candidate: &str) -> Result<Vec<CheckShown>, PersistError> {
    let runs: Vec<acceptance::CheckRun> = closing::candidate_evidence(conn, candidate)?
        .iter()
        .map(closing::Evidence::run)
        .collect();
    let mut by_check: BTreeMap<&str, Vec<&acceptance::CheckRun>> = BTreeMap::new();
    for run in &runs {
        by_check.entry(run.check_id.as_str()).or_default().push(run);
    }
    Ok(by_check
        .into_iter()
        .map(|(check, runs)| CheckShown {
            check_id: check.to_owned(),
            label: acceptance::check_label(&runs),
        })
        .collect())
}

/// [`AcceptanceView`] of run `run_id` inside the caller's read transaction, at `watermark` (decision 5): its latest
/// candidate's labels for every AC in its scope; `passing` the counted ones satisfied, `accepted` that when the
/// candidate is ACCEPTED, `total` the counted ones; QA and the reviewer by `report_status`; each check's label. Without a
/// candidate every AC is NOT_RUN and nothing passes. None when there is no such run.
pub(super) fn view_in(
    conn: &Connection,
    run_id: &str,
    watermark: i64,
) -> Result<Option<AcceptanceView>, PersistError> {
    let Some(spec) = run_spec(conn, run_id)? else {
        return Ok(None);
    };
    let spec_id = spec.map(|(id, _)| id);
    let scope = match &spec_id {
        Some(spec_id) => scope(conn, run_id, spec_id)?,
        None => Vec::new(),
    };
    let latest = candidates(conn, run_id)?.into_iter().next();
    let rounds = closing::rounds(conn, run_id)?;
    let (acs, qa, review, checks) = match (&latest, &spec_id) {
        (Some(c), Some(spec_id)) => {
            let (qa, review) = reports(conn, &c.id, Some(spec_id), &rounds)?;
            (
                shown(conn, run_id, spec_id, c, &scope, &rounds)?,
                qa,
                review,
                checks(conn, &c.id)?,
            )
        }
        (Some(c), None) => {
            let (qa, review) = reports(conn, &c.id, None, &rounds)?;
            (Vec::new(), qa, review, checks(conn, &c.id)?)
        }
        (None, _) => (
            scope
                .iter()
                .map(|a| AcShown {
                    ac_id: a.ac.clone(),
                    requirement_id: a.requirement.clone(),
                    label: Label::NotRun,
                    why: None,
                    rows: Vec::new(),
                    counted: a.counted,
                })
                .collect(),
            Report::NotRun,
            Report::NotRun,
            Vec::new(),
        ),
    };
    let passing = acs
        .iter()
        .filter(|a| a.counted && a.label.satisfied())
        .count();
    let accepted = if latest.as_ref().is_some_and(|c| c.state == "ACCEPTED") {
        passing
    } else {
        0
    };
    let total = acs.iter().filter(|a| a.counted).count();
    Ok(Some(AcceptanceView {
        run_id: run_id.to_owned(),
        spec_id,
        candidate: latest.map(|c| CandidateShown {
            candidate_id: c.id,
            state: c.state,
        }),
        acs,
        passing,
        accepted,
        total,
        qa,
        review,
        checks,
        watermark,
    }))
}

/// [`AcceptanceView`] of run `run_id` in one read transaction (`Reader::acceptance`).
pub(super) fn acceptance(
    conn: &Connection,
    run_id: &str,
) -> Result<Option<AcceptanceView>, PersistError> {
    read(conn, || {
        view_in(conn, run_id, super::commands::max_seq(conn)?)
    })
}

/// A page of run `run_id`'s acceptance projection (`GET /v1/runs/{run_id}/acceptance`, decision 5): every candidate of
/// the run, the latest first, each AC in the run's scope on it by id, labelled by the display projection of its rows
/// there under the run's specification; at most `limit` items after `after` (the freeze seq, the candidate's row and the AC of the last item of
/// the previous page); the next cursor when more follow. None when there is no such run.
pub(super) fn acceptance_page(
    conn: &Connection,
    run_id: &str,
    limit: usize,
    after: Option<(i64, i64, String)>,
) -> Result<Option<AcceptancePage>, PersistError> {
    read(conn, || {
        let Some(spec) = run_spec(conn, run_id)? else {
            return Ok(None);
        };
        let watermark = super::commands::max_seq(conn)?;
        let Some((spec_id, spec_revision)) = spec else {
            return Ok(Some(AcceptancePage {
                items: Vec::new(),
                next: None,
                watermark,
            }));
        };
        let scope = scope(conn, run_id, &spec_id)?;
        let rounds = closing::rounds(conn, run_id)?;
        let all = candidates(conn, run_id)?;
        let latest = all.first().map(|c| c.id.clone());
        let mut keyed: Vec<((i64, i64, String), AcceptanceItem)> = Vec::new();
        for c in &all {
            // Only the candidates at or after the cursor's are read.
            if after
                .as_ref()
                .is_some_and(|(seq, row, _)| (c.seq, c.rowid) > (*seq, *row))
            {
                continue;
            }
            for a in shown(conn, run_id, &spec_id, c, &scope, &rounds)? {
                let key = (c.seq, c.rowid, a.ac_id.clone());
                if after.as_ref().is_some_and(|(seq, row, ac)| {
                    (c.seq, c.rowid) == (*seq, *row) && a.ac_id <= *ac
                }) {
                    continue;
                }
                keyed.push((
                    key,
                    AcceptanceItem {
                        spec_id: spec_id.clone(),
                        spec_revision,
                        candidate_id: c.id.clone(),
                        candidate_state: c.state.clone(),
                        ac_id: a.ac_id,
                        label: a.label,
                        why: a.why,
                        latest: latest.as_deref() == Some(c.id.as_str()),
                    },
                ));
            }
            if keyed.len() > limit {
                break;
            }
        }
        let next = (keyed.len() > limit)
            .then(|| keyed.get(limit.saturating_sub(1)).map(|(k, _)| k.clone()))
            .flatten();
        keyed.truncate(limit);
        Ok(Some(AcceptancePage {
            items: keyed.into_iter().map(|(_, item)| item).collect(),
            next,
            watermark,
        }))
    })
}

/// A page of run `run_id`'s Issues (`GET /v1/runs/{run_id}/issues`, decisions 5 and 10), newest first: at most `limit`
/// older than rowid `before`, each state from the state function evaluated now (a deciding reader; the materialised
/// column only says whether it was ever resolved: RESOLVED when resolved, REOPENED when not but once, else as it is);
/// the next cursor when the page is full. None when there is no such run.
pub(super) fn issues_page(
    conn: &Connection,
    run_id: &str,
    limit: usize,
    before: Option<i64>,
) -> Result<Option<IssuesPage>, PersistError> {
    read(conn, || {
        if run_spec(conn, run_id)?.is_none() {
            return Ok(None);
        }
        let judged: BTreeMap<String, crate::verification::issues::Judged> =
            super::issues::judged(conn, run_id)?
                .into_iter()
                .map(|(row, judged)| (row.issue.id, judged))
                .collect();
        let mut statement = conn
            .prepare(
                "SELECT rowid, issue_id, candidate_id, severity, state, fingerprint, description, report_id, finding_id \
                 FROM issues WHERE run_id = ?1 AND (?2 IS NULL OR rowid < ?2) ORDER BY rowid DESC LIMIT ?3",
            )
            .map_err(sqlite_error)?;
        let found: Vec<(i64, IssueItem)> = statement
            .query_map(
                params![run_id, before, i64::try_from(limit).unwrap_or(i64::MAX)],
                |r| {
                    Ok((
                        r.get(0)?,
                        IssueItem {
                            issue_id: r.get(1)?,
                            candidate_id: r.get(2)?,
                            severity: r.get(3)?,
                            state: r.get(4)?,
                            fingerprint: r.get(5)?,
                            description: r.get(6)?,
                            report_id: r.get(7)?,
                            finding_id: r.get(8)?,
                        },
                    ))
                },
            )
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?;
        let next = (found.len() == limit)
            .then(|| found.last().map(|(rowid, _)| *rowid))
            .flatten();
        let items = found
            .into_iter()
            .map(|(_, mut item)| {
                if let Some(judged) = judged.get(&item.issue_id)
                    && let Some(t) =
                        crate::verification::issues::transition(&item.state, judged, None)
                {
                    item.state = t.to.to_owned();
                }
                item
            })
            .collect();
        Ok(Some(IssuesPage {
            items,
            next,
            watermark: super::commands::max_seq(conn)?,
        }))
    })
}

/// Issue `issue_id` traced to its report and raw session (decision 15): a finding's Issue, its report row with the
/// report and raw session artifacts, and the native attempt artifact its settle's `verification.report` named and
/// registered; none when there is no such Issue or it is no finding's.
pub(super) fn issue_chain(
    conn: &Connection,
    issue_id: &str,
) -> Result<Option<IssueChain>, PersistError> {
    read(conn, || {
        let found: Option<(ChainIssue, ChainReport, ObjectRef, String)> = conn
            .query_row(
                "SELECT i.issue_id, i.candidate_id, i.severity, i.fingerprint, i.finding_id, r.report_id, r.state, \
                 ra.path, ra.sha256, rw.path, rw.sha256, r.run_id FROM issues i \
                 JOIN independent_reports r ON r.report_id = i.report_id \
                 JOIN artifacts ra ON ra.artifact_id = r.report_artifact_id \
                 JOIN artifacts rw ON rw.artifact_id = r.raw_session_artifact_id \
                 WHERE i.issue_id = ?1 AND i.finding_id IS NOT NULL",
                params![issue_id],
                |r| {
                    Ok((
                        ChainIssue {
                            issue_id: r.get(0)?,
                            candidate_id: r.get(1)?,
                            severity: r.get(2)?,
                            fingerprint: r.get(3)?,
                            finding_id: r.get(4)?,
                        },
                        ChainReport {
                            report_id: r.get(5)?,
                            state: r.get(6)?,
                            path: r.get(7)?,
                            sha256: r.get(8)?,
                        },
                        ObjectRef {
                            path: r.get(9)?,
                            sha256: r.get(10)?,
                        },
                        r.get(11)?,
                    ))
                },
            )
            .optional()
            .map_err(sqlite_error)?;
        let Some((issue, report, raw, run_id)) = found else {
            return Ok(None);
        };
        let native: Option<ObjectRef> = conn
            .query_row(
                "SELECT a.path, a.sha256 FROM events e JOIN artifacts a \
                   ON a.path = json_extract(e.payload_json, '$.native_attempt.path') \
                   AND a.sha256 = json_extract(e.payload_json, '$.native_attempt.sha256') \
                 WHERE e.run_id = ?1 AND e.kind = 'verification.report' \
                 AND json_extract(e.payload_json, '$.report_id') = ?2 ORDER BY e.seq DESC LIMIT 1",
                params![run_id, report.report_id],
                |r| {
                    Ok(ObjectRef {
                        path: r.get(0)?,
                        sha256: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(sqlite_error)?;
        Ok(Some(IssueChain {
            issue,
            report,
            raw,
            native,
        }))
    })
}

#[cfg(test)]
mod tests;
