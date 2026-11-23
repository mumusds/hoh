//! docs/adr/ADR-031.md decision 1 (owner question Q1 (a)) against a real store: StartRun adopts the project's latest
//! approved specification and its `spec.changed{cause: "adopted"}` wakes the Commander; with none the run is BLOCKED
//! by the question `runtime:no_spec` in the same commit, its 202 revision 1; the question's answer `adopt` (W13's real
//! path through AnswerQuestion) adopts the specification approved since, or leaves the run BLOCKED while there is none.

use crate::config::Config;
use crate::persistence::{
    CommandOutcome, Intent, NewEvent, Options, RunTarget, Store, SubmitCommand, selftest,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::new(crate::spawn::Direct::shared())
    };
    let store = Store::open(&path, options)?;
    exec(
        &path,
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-15T00:00:00Z');",
    )?;
    Ok((store, path))
}

fn exec(path: &Path, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

/// Two approved specifications of p1, the later one (revision 2) inserted first.
const SPECS: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', 'p1', printf('%064d', 2), 2);
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
";

/// StartRun of run `run_id`, as the host submits it.
fn start(run_id: &str) -> SubmitCommand {
    SubmitCommand {
        command_id: format!("c-{run_id}"),
        idempotency_key: format!("k-{run_id}"),
        kind: "StartRun".to_owned(),
        run_id: run_id.to_owned(),
        expected_revision: 0,
        payload: json!({"goal": "g", "mode": "AUTO", "interactive": false}),
        payload_sha256: format!("{:064}", run_id.len()),
        target: RunTarget::Start {
            max_discussion_rounds: 2,
            mode: "AUTO".to_owned(),
            budget: Config::default().budget,
            policy: "balanced".to_owned(),
            assumptions: std::collections::BTreeMap::new(),
            unattended: true,
            project: crate::config::Config::default().project,
            mode_cycle: crate::config::Config::default().ui.mode_cycle,
        },
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{run_id}"),
            run_id: Some(run_id.to_owned()),
            kind: "run.created".to_owned(),
            payload: json!({"goal": "g"}),
        },
        outbox: None,
    }
}

/// The 202 body's revision of an accepted command, and the revision it reports.
fn accepted(outcome: &CommandOutcome) -> R<(Value, i64)> {
    let CommandOutcome::Accepted {
        result_json,
        revision,
        ..
    } = outcome
    else {
        return Err(format!("not accepted now: {outcome:?}").into());
    };
    let body: Value = serde_json::from_str(result_json)?;
    Ok((body["revision"].clone(), *revision))
}

/// Run `run`'s row: state, revision, spec, resume state, blocked reason and set.
fn run(path: &Path, run: &str) -> R<Value> {
    let text: String = Connection::open(path)?.query_row(
        "SELECT json_array(state, revision, spec_id, resume_state, blocked_reason, json(blocked_reasons_json)) \
         FROM runs WHERE run_id = ?1",
        [run],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&text)?)
}

/// Run `run`'s events from seq `after` on: (seq, event_id, kind, payload).
fn events(path: &Path, run: &str, after: i64) -> R<Vec<(i64, String, String, Value)>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(
        "SELECT seq, event_id, kind, payload_json FROM events WHERE run_id = ?1 AND seq > ?2 ORDER BY seq",
    )?;
    let rows = statement
        .query_map(rusqlite::params![run, after], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(seq, id, kind, payload)| Ok((seq, id, kind, serde_json::from_str(&payload)?)))
        .collect()
}

fn kinds(events: &[(i64, String, String, Value)]) -> Vec<&str> {
    events.iter().map(|e| e.2.as_str()).collect()
}

/// The adoption event of run `run` of specification `spec` (`spec_revision`, `sha` its digest's digit).
fn adopted(run: &str, spec: &str, spec_revision: i64, sha: u32) -> Value {
    json!({"run_id": run, "spec_id": spec, "spec_revision": spec_revision, "spec_sha256": format!("{sha:064}"),
        "from": null, "cause": "adopted"})
}

#[tokio::test]
async fn a_started_run_adopts_the_projects_latest_approved_specification_and_wakes_the_commander()
-> R {
    // Decision 1, 有规格: StartRun takes the specification of the project's highest revision (s2, inserted before s1)
    // into the run's row and writes spec.changed{cause: adopted} after run.created in its commit; the run stays CREATED
    // at revision 0, the 202 body's revision 0; the event is a wake of the Commander (REQ-021 behaviour 1).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    exec(&path, SPECS)?;
    let outcome = store.submit_command(start("R")).await?;
    assert_eq!(accepted(&outcome)?, (json!(0), 0));
    assert_eq!(
        run(&path, "R")?,
        json!(["CREATED", 0, "s2", null, null, []])
    );
    let written = events(&path, "R", 0)?;
    assert_eq!(
        kinds(&written),
        [
            "run.created",
            "spec.changed",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned"
        ]
    );
    assert_eq!(written[1].3, adopted("R", "s2", 2, 2));
    let wake = crate::supervise::classify(
        crate::supervise::Policy::Efficient,
        &written[1].2,
        &written[1].3,
        None,
    )
    .ok_or("spec.changed is no wake")?;
    assert_eq!(wake.key.spec_revision, Some(2));
    // Its supervisor takes it in: a trigger of the run's for it.
    store
        .supervise("R", crate::decision::Context::default())
        .await?;
    let trigger: (String, String) = Connection::open(&path)?.query_row(
        "SELECT state, event_seqs_json FROM supervisor_triggers WHERE run_id = 'R'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(trigger.0, "PENDING");
    assert_eq!(
        serde_json::from_str::<Value>(&trigger.1)?,
        json!([written[1].0])
    );
    Ok(())
}

#[tokio::test]
async fn a_started_run_without_a_specification_is_blocked_by_its_no_spec_question_in_its_commit()
-> R {
    // Decision 1, 没有规格 (W13; 出口表 "StartRun，项目没有规格"): in StartRun's own commit, after run.created, the claim
    // runtime:no_spec NEED_DECISION, question.asked{no_spec, choices [adopt]} (its id the question's), run.blocked and,
    // last, the run's run.result BLOCKED NEED_DECISION; the run BLOCKED from CREATED at revision 1, the 202 body's
    // revision 1, its replay the same bytes.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    let outcome = store.submit_command(start("R")).await?;
    assert_eq!(accepted(&outcome)?, (json!(1), 1));
    assert_eq!(
        run(&path, "R")?,
        json!([
            "BLOCKED",
            1,
            null,
            "CREATED",
            "NEED_DECISION",
            ["NEED_DECISION"]
        ])
    );
    let written = events(&path, "R", 0)?;
    assert_eq!(
        kinds(&written),
        [
            "run.created",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "resource.pinned",
            "run.blocker_claimed",
            "question.asked",
            "run.blocked",
            "run.result"
        ]
    );
    assert_eq!(
        (&written[7].3["source"], &written[7].3["reason"]),
        (&json!("runtime:no_spec"), &json!("NEED_DECISION"))
    );
    let question = &written[8];
    assert_eq!(question.3["question_id"], json!(question.1));
    assert_eq!(question.3["reason"], "no_spec");
    assert_eq!(question.3["choices"], json!(["adopt"]));
    assert!(
        question.3["text"]
            .as_str()
            .is_some_and(|t| t.contains("/plan") && t.contains("已批准的规格")),
        "{}",
        question.3
    );
    let result = &written[10].3;
    assert_eq!(
        (
            &result["state"],
            &result["blocked_reason"],
            &result["exit_code"]
        ),
        (&json!("BLOCKED"), &json!("NEED_DECISION"), &json!(3))
    );
    assert_eq!(result["last_event_seq"], json!(written[10].0));
    let last: i64 =
        Connection::open(&path)?.query_row("SELECT max(seq) FROM events", [], |r| r.get(0))?;
    assert_eq!(
        last, written[10].0,
        "the run.result is the commit's last event"
    );
    // The same command again: the stored body.
    let CommandOutcome::Replayed { result_json } = store.submit_command(start("R")).await? else {
        return Err("not a replay".into());
    };
    assert_eq!(serde_json::from_str::<Value>(&result_json)?["revision"], 1);
    Ok(())
}

/// AnswerQuestion of run `run`'s question `question` with its one choice, adopt, at `revision`, as the host submits it.
fn answer_adopt(id: &str, run: &str, revision: i64, question: &str) -> SubmitCommand {
    SubmitCommand {
        command_id: id.to_owned(),
        idempotency_key: format!("k-{id}"),
        kind: "AnswerQuestion".to_owned(),
        run_id: run.to_owned(),
        expected_revision: revision,
        payload: json!({"question_id": question, "answer": "采用", "choice_id": "adopt"}),
        payload_sha256: crate::context::sha256_hex(id.as_bytes()),
        target: RunTarget::Input(crate::persistence::Input::Answer {
            question_id: question.to_owned(),
            answer: "采用".to_owned(),
            choice_id: Some("adopt".to_owned()),
        }),
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{id}"),
            run_id: Some(run.to_owned()),
            kind: "question.answer_accepted".to_owned(),
            payload: json!({"command_id": id, "question_id": question}),
        },
        outbox: None,
    }
}

/// Run `run`'s open questions as getRun's projection lists them.
fn open_questions(path: &Path, run: &str) -> R<Vec<Value>> {
    let conn = Connection::open(path)?;
    Ok(crate::persistence::questions::open(&conn, run)?
        .iter()
        .map(crate::persistence::questions::Question::json)
        .collect())
}

#[tokio::test]
async fn w13_a_run_without_a_specification_adopts_one_approved_since_by_its_answer() -> R {
    // W13's real path (decision 1; decision 8's row runtime:no_spec): StartRun without a specification -> BLOCKED,
    // its question listed by questions::open (run-level, reason no_spec, the one choice adopt, the default); answered
    // adopt while there is still none: the run stays BLOCKED, its revision moved once (a Touch), the answer's commit
    // ends with its run.result BLOCKED NEED_DECISION, the question still open; a specification approved, answered adopt
    // again: runs.spec_id names it, spec.changed{cause: adopted, from: null}, the claim released, run.resumed BLOCKED
    // -> CREATED, the revision moved once (that Unblock), no run.result, no question open.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(&dir)?;
    store.submit_command(start("R")).await?;
    let question = events(&path, "R", 0)?
        .into_iter()
        .find(|e| e.2 == "question.asked")
        .ok_or("no question")?
        .1;
    let listed = open_questions(&path, "R")?;
    let [listed] = listed.as_slice() else {
        return Err(format!("not one question: {listed:?}").into());
    };
    assert_eq!(
        (
            &listed["question_id"],
            &listed["source"],
            &listed["subject"],
            &listed["reason"],
            &listed["default"]
        ),
        (
            &json!(question),
            &json!("runtime"),
            &json!("run"),
            &json!("no_spec"),
            &json!("adopt")
        )
    );
    assert_eq!(
        listed["choices"]
            .as_array()
            .map(|c| c.iter().map(|c| c["choice_id"].clone()).collect::<Vec<_>>()),
        Some(vec![json!("adopt")])
    );
    // Still no specification.
    let before = events(&path, "R", 0)?.last().map_or(0, |e| e.0);
    let outcome = store
        .submit_command(answer_adopt("a1", "R", 1, &question))
        .await?;
    assert_eq!(accepted(&outcome)?, (json!(2), 2));
    let written = events(&path, "R", before)?;
    assert_eq!(
        kinds(&written),
        [
            "question.answer_accepted",
            "question.answered",
            "run.result"
        ]
    );
    assert_eq!(written[1].3["exit"], "adopt");
    assert_eq!(
        (&written[2].3["state"], &written[2].3["blocked_reason"]),
        (&json!("BLOCKED"), &json!("NEED_DECISION"))
    );
    assert_eq!(
        run(&path, "R")?,
        json!([
            "BLOCKED",
            2,
            null,
            "CREATED",
            "NEED_DECISION",
            ["NEED_DECISION"]
        ])
    );
    assert_eq!(open_questions(&path, "R")?.len(), 1, "still open");
    // A specification approved since.
    exec(&path, SPECS)?;
    let before = written.last().map_or(before, |e| e.0);
    let outcome = store
        .submit_command(answer_adopt("a2", "R", 2, &question))
        .await?;
    assert_eq!(accepted(&outcome)?, (json!(3), 3));
    let written = events(&path, "R", before)?;
    assert_eq!(
        kinds(&written),
        [
            "question.answer_accepted",
            "question.answered",
            "spec.changed",
            "run.blocker_released",
            "run.resumed"
        ]
    );
    assert_eq!(written[2].3, adopted("R", "s2", 2, 2));
    assert_eq!(
        (&written[3].3["source"], &written[3].3["reason"]),
        (&json!("runtime:no_spec"), &json!("NEED_DECISION"))
    );
    assert_eq!(
        (&written[4].3["from"], &written[4].3["to"]),
        (&json!("BLOCKED"), &json!("CREATED"))
    );
    assert_eq!(
        run(&path, "R")?,
        json!(["CREATED", 3, "s2", null, null, []])
    );
    assert_eq!(open_questions(&path, "R")?, Vec::<Value>::new());
    Ok(())
}
