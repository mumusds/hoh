//! docs/adr/ADR-031.md decisions 6 to 8 against a real store (the test plan's 问题与出口 and SendInput): the routing of
//! every typed reason, the open questions and their anchors, and each exit of decision 8's table in the answer's own
//! transaction.

use super::*;

#[test]
fn every_typed_reason_has_its_decision_8_exit_and_choices() {
    // Correction A's guard (3): route is total over the 22 task pairs, with or without a candidate under verification,
    // and route_run over the 2 run reasons; each route has choices, its first the default. The table's rows: work's
    // own ends continue or cancel, its repairs rework or cancel; a source on a VERIFYING candidate reverify or rework
    // (flaky rework only), on any other rework only; a verifier redispatch, rebuilt or rebound for its two refusals.
    use super::Redispatch::{Plain, Rebind, RebuildHandoff, Report};
    let expected = |reason: TaskReason, verifying: bool| match reason {
        TaskReason::Work(
            WorkReason::LocalRepairsExhausted | WorkReason::ProtocolRepairsExhausted,
        ) => Route::Rework,
        TaskReason::Work(_) => Route::Continue,
        TaskReason::Source(SourceReason::Flaky) => Route::SourceRework,
        TaskReason::Source(_) if verifying => Route::Reverify,
        TaskReason::Source(_) => Route::SourceRework,
        TaskReason::Verifier(VerifierReason::InvalidHandoff) => Route::Redispatch(RebuildHandoff),
        TaskReason::Verifier(VerifierReason::SessionNotIndependent) => Route::Redispatch(Rebind),
        TaskReason::Verifier(VerifierReason::ReportRejected) => Route::Redispatch(Report),
        TaskReason::Verifier(_) => Route::Redispatch(Plain),
    };
    for reason in TaskReason::all() {
        for verifying in [true, false] {
            let route = route(reason, verifying);
            assert_eq!(route, expected(reason, verifying), "{reason:?} {verifying}");
            assert!(!route.choices().is_empty(), "{reason:?}");
        }
    }
    let ids = |route: Route| {
        route
            .choices()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(Route::Continue), ["continue", "cancel"]);
    assert_eq!(ids(Route::Rework), ["rework", "cancel"]);
    assert_eq!(ids(Route::Reverify), ["reverify", "rework"]);
    assert_eq!(ids(Route::SourceRework), ["rework"]);
    assert_eq!(ids(Route::Redispatch(Plain)), ["redispatch"]);
    assert_eq!(route_run(RunReason::Stalled), Route::Stalled);
    assert_eq!(ids(Route::Stalled), ["retry", "cancel"]);
    assert_eq!(route_run(RunReason::NoSpec), Route::Adopt);
    assert_eq!(ids(Route::Adopt), ["adopt"]);
    // No source exit offers a cancel, nor goes back to VERIFYING under the same join number (E6).
    for reason in SourceReason::ALL {
        for verifying in [true, false] {
            assert!(!ids(route(TaskReason::Source(*reason), verifying)).contains(&"cancel"));
        }
    }
}

use crate::persistence::{
    Change, CommandOutcome, Commit, FaultHooks, Input, Intent, NewEvent, Options, PersistError,
    RunTarget, StateCas, Store, SubmitCommand, selftest,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn open_store(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    open_with(dir, Arc::new(crate::persistence::NoFaults))
}

fn open_with(dir: &tempfile::TempDir, faults: Arc<dyn FaultHooks>) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        faults,
        ..Options::default()
    };
    Ok((Store::open(&path, options)?, path))
}

fn exec(path: &Path, sql: &str) -> R {
    Connection::open(path)?.execute_batch(sql)?;
    Ok(())
}

fn value(path: &Path, sql: &str) -> R<Value> {
    let conn = Connection::open(path)?;
    Ok(conn.query_row(sql, [], |r| {
        Ok(match r.get_ref(0)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(n) => json!(n),
            rusqlite::types::ValueRef::Text(t) => {
                let text = String::from_utf8_lossy(t).into_owned();
                serde_json::from_str(&text).unwrap_or(Value::String(text))
            }
            other => json!(format!("{other:?}")),
        })
    })?)
}

/// Run R of project p1 on spec s1 in `state` at revision 5, its resume state SPECIFYING, blocked by `claims` ((source,
/// reason) pairs; none: not blocked), with `extra` SQL.
fn world(path: &Path, state: &str, claims: &[(&str, &str)], extra: &str) -> R {
    let reasons: Vec<&str> = {
        let mut r: Vec<&str> = claims.iter().map(|(_, r)| *r).collect();
        r.sort_unstable();
        r.dedup();
        r
    };
    let open: Vec<Value> = claims.iter().map(|(s, r)| json!([s, r])).collect();
    let mut sql = format!(
        "INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-15T00:00:00Z');
         INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, resume_state, blocked_reason,
           blocked_reasons_json, created_at) VALUES ('R', 'p1', 's1', '{state}', 'AUTO', 5, 'SPECIFYING', {blocked},
           '{json}', '2026-11-15T00:00:00Z');",
        blocked = reasons
            .first()
            .map_or_else(|| "NULL".to_owned(), |r| format!("'{r}'")),
        json = json!(reasons),
    );
    if let Some((source, reason)) = claims.last() {
        sql.push_str(&format!(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1',
               'R/blocker/0', 'R', 'run.blocker_claimed', '{}', '2026-11-15T00:00:00Z');",
            json!({"reason": reason, "source": source, "open": open, "n": 0})
        ));
    }
    sql.push_str(extra);
    exec(path, &sql)
}

/// The Commander's PROPOSED request D1 (choices a and b) and its question.asked.
const COMMANDER: &str = "
INSERT INTO decisions(decision_id, run_id, role_epoch, expected_revision, payload_json, state) VALUES ('D1', 'R', 1, 4,
  '{\"kind\": \"request_user_decision\", \"question_id\": \"q-c\", \"question\": \"选哪个方案？\",
    \"choices\": [{\"choice_id\": \"a\", \"label\": \"方案A\"}, {\"choice_id\": \"b\", \"label\": \"方案B\"}],
    \"requires_user\": true}', 'PROPOSED');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'D1/question', 'R',
  'question.asked', '{\"question_id\": \"D1\", \"commander_question_id\": \"q-c\", \"question\": \"选哪个方案？\",
    \"choices\": [{\"choice_id\": \"a\", \"label\": \"方案A\"}, {\"choice_id\": \"b\", \"label\": \"方案B\"}],
    \"requires_user\": true}', '2026-11-15T00:00:00Z');";

/// Development task T BLOCKED (NEED_DECISION) at revision 3 after 9 attempts, its anchor need_decision{reason}.
fn work_task(reason: &str) -> String {
    format!(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, attempt_count,
           blocked_reason, layer) VALUES ('T', 'R', 's1', 'BLOCKED', 3, 'developer', 1, 1, 9, 'NEED_DECISION', 2);
         INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'T/need', 'R',
           'need_decision', '{{\"run_id\": \"R\", \"task_id\": \"T\", \"reason\": \"{reason}\", \"detail\": \"d\",
             \"fingerprint\": \"f1\"}}', '2026-11-15T00:00:00Z');"
    )
}

fn command(id: &str, revision: i64, kind: &str, event: &str, input: Input) -> SubmitCommand {
    SubmitCommand {
        command_id: id.to_owned(),
        idempotency_key: format!("k-{id}"),
        kind: kind.to_owned(),
        run_id: "R".to_owned(),
        expected_revision: revision,
        payload: json!({}),
        payload_sha256: crate::context::sha256_hex(id.as_bytes()),
        target: RunTarget::Input(input),
        event: NewEvent {
            intent: Intent::Progress,
            project_id: "p1".to_owned(),
            event_id: format!("e-{id}"),
            run_id: Some("R".to_owned()),
            kind: event.to_owned(),
            payload: json!({"command_id": id}),
        },
        outbox: None,
    }
}

fn answer(
    id: &str,
    revision: i64,
    question: &str,
    text: &str,
    choice: Option<&str>,
) -> SubmitCommand {
    command(
        id,
        revision,
        "AnswerQuestion",
        "question.answer_accepted",
        Input::Answer {
            question_id: question.to_owned(),
            answer: text.to_owned(),
            choice_id: choice.map(str::to_owned),
        },
    )
}

fn send(id: &str, revision: i64, text: &str, kind: &str, role: Option<&str>) -> SubmitCommand {
    command(
        id,
        revision,
        "SendInput",
        "input.accepted",
        Input::Send {
            text: text.to_owned(),
            target_role: role.map(str::to_owned),
            input_kind: kind.to_owned(),
        },
    )
}

/// The kinds of R's events after seq `after`, in order.
fn kinds_after(path: &Path, after: i64) -> R<Vec<String>> {
    Ok(serde_json::from_value(value(
        path,
        &format!(
            "SELECT json_group_array(kind) FROM (SELECT kind FROM events WHERE run_id = 'R' AND seq > {after} \
             ORDER BY seq)"
        ),
    )?)?)
}

fn max_seq(path: &Path) -> R<i64> {
    Ok(value(path, "SELECT coalesce(max(seq), 0) FROM events")?
        .as_i64()
        .unwrap_or(0))
}

fn run_state(path: &Path) -> R<Value> {
    value(
        path,
        "SELECT state || '/' || revision || coalesce('/' || blocked_reason, '') FROM runs WHERE run_id = 'R'",
    )
}

/// Everything a refused command must leave as it was.
fn snapshot(path: &Path) -> R<Value> {
    value(
        path,
        "SELECT json_array((SELECT count(*) FROM events), (SELECT count(*) FROM commands), \
         (SELECT count(*) FROM input_queue), (SELECT group_concat(state || revision) FROM runs), \
         (SELECT group_concat(task_id || state || revision || coalesce(blocked_reason, '')) FROM tasks), \
         (SELECT group_concat(decision_id || state) FROM decisions))",
    )
}

/// The latest event of R with `kind`: its payload.
fn last_of(path: &Path, kind: &str) -> R<Value> {
    value(
        path,
        &format!(
            "SELECT payload_json FROM events WHERE run_id = 'R' AND kind = '{kind}' ORDER BY seq DESC LIMIT 1"
        ),
    )
}

#[tokio::test]
async fn open_lists_each_question_by_its_claim_or_its_anchor_never_rewriting_the_reason() -> R {
    // Decision 6, E4: the Commander's request (its words and choices, no default), the stalled question (retry,
    // cancel), and one per task BLOCKED (NEED_DECISION) by its anchor: a work task's latest need_decision; a task whose
    // local repairs were refused and then re-blocked (repair_refused) keeps the refusal as its anchor, reason
    // local_repairs_exhausted and `refusal: true`; in the order they appeared. A blocked task with no anchor (seeded
    // by hand) is no question.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[
            ("commander:D1", "NEED_DECISION"),
            ("runtime:stalled", "NEED_DECISION"),
        ],
        &format!(
            "{COMMANDER}{}
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'S/q', 'R',
               'question.asked', '{{\"question_id\": \"S/q\", \"reason\": \"stalled\", \"progress\": \"p\",
                 \"turns\": 2, \"text\": \"停滞\", \"choices\": [\"retry\", \"cancel\"]}}', '2026-11-15T00:00:00Z');
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, blocked_reason)
               VALUES ('L', 'R', 's1', 'BLOCKED', 1, 'developer', 1, 2, 'NEED_DECISION'),
                      ('N', 'R', 's1', 'BLOCKED', 1, 'developer', 1, 3, 'NEED_DECISION');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
               ('p1', 'L/refused', 'R', 'need_decision', '{{\"run_id\": \"R\", \"task_id\": \"L\",
                  \"reason\": \"local_repairs_exhausted\", \"fingerprint\": \"f\", \"detail\": \"item\"}}', 'x'),
               ('p1', 'L/again', 'R', 'need_decision', '{{\"run_id\": \"R\", \"task_id\": \"L\",
                  \"reason\": \"repair_refused\", \"detail\": \"again\"}}', 'x');",
            work_task("retries_exhausted")
        ),
    )?;
    drop(store);
    let conn = Connection::open(&path)?;
    let open = open(&conn, "R")?;
    let shown: Vec<Value> = open.iter().map(Question::json).collect();
    assert_eq!(
        shown
            .iter()
            .map(|q| (
                q["question_id"].clone(),
                q["source"].clone(),
                q["subject"].clone(),
                q["reason"].clone(),
                q["refusal"].clone(),
                q["default"].clone()
            ))
            .collect::<Vec<_>>(),
        [
            (
                json!("D1"),
                json!("commander"),
                json!(null),
                json!(null),
                json!(false),
                json!(null)
            ),
            (
                json!("T/need"),
                json!("runtime"),
                json!("work"),
                json!("retries_exhausted"),
                json!(false),
                json!("continue")
            ),
            (
                json!("S/q"),
                json!("runtime"),
                json!("run"),
                json!("stalled"),
                json!(false),
                json!("retry")
            ),
            (
                json!("L/refused"),
                json!("runtime"),
                json!("work"),
                json!("local_repairs_exhausted"),
                json!(true),
                json!("rework")
            ),
        ]
    );
    assert_eq!(shown[0]["text"], "选哪个方案？");
    assert_eq!(
        shown[0]["choices"],
        json!([{"choice_id": "a", "label": "方案A"}, {"choice_id": "b", "label": "方案B"}])
    );
    assert_eq!(shown[1]["task_id"], "T");
    assert!(
        shown[1]["text"]
            .as_str()
            .is_some_and(|t| t.contains("T") && t.contains("d")),
        "{}",
        shown[1]
    );
    assert!(
        open.iter()
            .all(|q| q.reason.as_deref() != Some("repair_refused"))
    );
    Ok(())
}

#[tokio::test]
async fn the_commanders_answer_applies_its_request_releases_its_claim_and_wakes_it() -> R {
    // Decision 7, the Commander's exit: one IMMEDIATE transaction whose first event is the acceptance event (with its
    // command_id and question_id), then question.answered, the request PROPOSED -> APPLIED with decision.applied
    // {answered_by}, need_decision{answered} (the answer, its choice, fingerprint answer/<decision>), its claim released
    // and the run unblocked to its resume state; working again, no run.result. The 202's revision is the run's after it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[("commander:D1", "NEED_DECISION")],
        COMMANDER,
    )?;
    let before = max_seq(&path)?;
    let done = store
        .submit_command(answer("c1", 5, "D1", "  方案a ", None))
        .await?;
    let CommandOutcome::Accepted {
        result_json,
        revision,
        event_seq,
    } = done
    else {
        return Err("not accepted".into());
    };
    assert_eq!(
        kinds_after(&path, before)?,
        [
            "question.answer_accepted",
            "question.answered",
            "decision.applied",
            "need_decision",
            "run.blocker_released",
            "run.resumed"
        ]
    );
    assert_eq!(event_seq, before + 1);
    assert_eq!(run_state(&path)?, json!("SPECIFYING/6"));
    assert_eq!(revision, 6);
    assert_eq!(serde_json::from_str::<Value>(&result_json)?["revision"], 6);
    assert_eq!(
        value(
            &path,
            "SELECT state FROM decisions WHERE decision_id = 'D1'"
        )?,
        json!("APPLIED")
    );
    let accepted = last_of(&path, "question.answer_accepted")?;
    assert_eq!(
        (&accepted["command_id"], &accepted["question_id"]),
        (&json!("c1"), &json!("D1"))
    );
    let answered = last_of(&path, "question.answered")?;
    assert_eq!(
        (
            &answered["choice_id"],
            &answered["exit"],
            &answered["answer"]
        ),
        (&json!("a"), &json!("applied"), &json!("  方案a "))
    );
    let woke = last_of(&path, "need_decision")?;
    assert_eq!(
        (
            &woke["reason"],
            &woke["decision_id"],
            &woke["choice_id"],
            &woke["fingerprint"]
        ),
        (
            &json!("answered"),
            &json!("D1"),
            &json!("a"),
            &json!("answer/D1")
        )
    );
    assert_eq!(
        last_of(&path, "decision.applied")?["answered_by"],
        json!("c1")
    );
    assert_eq!(open(&Connection::open(&path)?, "R")?, Vec::new());
    // The same key and payload again: the stored 202, nothing more.
    let again = max_seq(&path)?;
    assert!(matches!(
        store
            .submit_command(answer("c1", 5, "D1", "  方案a ", None))
            .await?,
        CommandOutcome::Replayed { .. }
    ));
    assert_eq!(max_seq(&path)?, again);
    Ok(())
}

/// R in BLOCKED with the Commander's question, then moved by commits of its own to `through` (its stops published by
/// their run.result).
async fn moved(store: &Store, path: &Path, through: &[RunState]) -> R {
    for (n, to) in through.iter().enumerate() {
        let revision = value(path, "SELECT revision FROM runs WHERE run_id = 'R'")?
            .as_i64()
            .unwrap_or(0);
        store
            .commit(Commit {
                cas: vec![StateCas {
                    id: "R".to_owned(),
                    expected_revision: revision,
                    change: Change::Run(*to),
                }],
                events: vec![NewEvent {
                    intent: Intent::Progress,
                    project_id: "p1".to_owned(),
                    event_id: format!("move-{n}"),
                    run_id: Some("R".to_owned()),
                    kind: "test.moved".to_owned(),
                    payload: json!({}),
                }],
                ..Commit::default()
            })
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn an_answer_on_a_stopped_run_is_accepted_and_ends_with_its_result() -> R {
    // Decision 7, E3; the test plan's 守卫 (b) and 停住之后的写入口 (9)'s AnswerQuestion variant: on a published
    // INCOMPLETE run (stopped, correction D) and a PAUSED one, the answer is the command's (no RunStopped): its exit
    // applied and its claim released, the run where it was (follow moves no PAUSED or INCOMPLETE run), its revision
    // touched, and the commit ends with its run.result. Another claim still open (BLOCKED_AUTH): BLOCKED stays, touched,
    // run.result{BLOCKED, BLOCKED_AUTH}.
    for (through, state) in [
        (vec![RunState::Incomplete], "INCOMPLETE"),
        (vec![RunState::PauseRequested, RunState::Paused], "PAUSED"),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, path) = open_store(&dir)?;
        world(
            &path,
            "BLOCKED",
            &[("commander:D1", "NEED_DECISION")],
            COMMANDER,
        )?;
        moved(&store, &path, &through).await?;
        assert_eq!(
            value(
                &path,
                "SELECT kind FROM events WHERE run_id = 'R' ORDER BY seq DESC LIMIT 1"
            )?,
            json!("run.result")
        );
        let revision = value(&path, "SELECT revision FROM runs WHERE run_id = 'R'")?
            .as_i64()
            .unwrap_or(0);
        let before = max_seq(&path)?;
        store
            .submit_command(answer("c1", revision, "D1", "b", None))
            .await?;
        let kinds = kinds_after(&path, before)?;
        assert_eq!(
            kinds.first().map(String::as_str),
            Some("question.answer_accepted"),
            "{state}"
        );
        assert_eq!(
            kinds.last().map(String::as_str),
            Some("run.result"),
            "{state}: {kinds:?}"
        );
        assert!(
            kinds.contains(&"need_decision".to_owned()),
            "{state}: {kinds:?}"
        );
        assert_eq!(
            run_state(&path)?,
            json!(format!("{state}/{}", revision + 1)),
            "{state}"
        );
        let result = last_of(&path, "run.result")?;
        assert_eq!(result["state"], state);
        assert_eq!(
            value(
                &path,
                "SELECT state FROM decisions WHERE decision_id = 'D1'"
            )?,
            json!("APPLIED")
        );
    }
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[("commander:D1", "NEED_DECISION"), ("pace", "BLOCKED_AUTH")],
        COMMANDER,
    )?;
    let before = max_seq(&path)?;
    store
        .submit_command(answer("c1", 5, "D1", "a", None))
        .await?;
    assert_eq!(run_state(&path)?, json!("BLOCKED/6/BLOCKED_AUTH"));
    let kinds = kinds_after(&path, before)?;
    assert_eq!(
        kinds.last().map(String::as_str),
        Some("run.result"),
        "{kinds:?}"
    );
    let result = last_of(&path, "run.result")?;
    assert_eq!(
        (&result["state"], &result["blocked_reason"]),
        (&json!("BLOCKED"), &json!("BLOCKED_AUTH"))
    );
    Ok(())
}

#[tokio::test]
async fn a_send_input_to_a_published_incomplete_run_is_the_commands_and_ends_with_its_result() -> R
{
    // The test plan's 守卫 (b) and 停住之后的写入口 (1)'s accepted SendInput (correction D, E3): on a run stopped INCOMPLETE
    // (published) with no question, a SendInput is accepted (no RunStopped, its events the command's), queued as a
    // STEER, the run INCOMPLETE and touched; the commit ends with run.result{INCOMPLETE}, so the run's latest event is
    // its result again and it stays stopped.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(&path, "RUNNING", &[], "")?;
    moved(&store, &path, &[RunState::Incomplete]).await?;
    let before = max_seq(&path)?;
    store
        .submit_command(send("c1", 6, "继续", "REPLY", None))
        .await?;
    assert_eq!(
        kinds_after(&path, before)?,
        ["input.accepted", "input.queued", "run.result"]
    );
    assert_eq!(run_state(&path)?, json!("INCOMPLETE/7"));
    assert_eq!(last_of(&path, "run.result")?["state"], "INCOMPLETE");
    assert_eq!(
        super::super::hold::stopped(&Connection::open(&path)?, "R")?,
        Some(RunState::Incomplete)
    );
    Ok(())
}

#[tokio::test]
async fn a_refused_answer_writes_nothing_and_says_why() -> R {
    // Decision 7, E5: a question that is not open (409 ILLEGAL_TRANSITION, details naming it); a choice_id that is not
    // one of its own (400 BAD_REQUEST); a stale revision (409 STALE_REVISION); a run that ended (409
    // ILLEGAL_TRANSITION, details.state, decision 9's 64): nothing written, the question still open. A run that does
    // not exist: RUN_NOT_FOUND.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[("commander:D1", "NEED_DECISION")],
        COMMANDER,
    )?;
    let before = snapshot(&path)?;
    let refused = store
        .submit_command(answer("c1", 5, "nope", "a", None))
        .await;
    assert!(
        matches!(&refused, Err(PersistError::IllegalTransition { details: Some(d), .. })
            if d["question_id"] == "nope" && d.get("state").is_none()),
        "{refused:?}"
    );
    let refused = store
        .submit_command(answer("c2", 5, "D1", "a", Some("z")))
        .await;
    assert!(
        matches!(&refused, Err(PersistError::BadRequest(_))),
        "{refused:?}"
    );
    let refused = store.submit_command(answer("c3", 4, "D1", "a", None)).await;
    assert!(
        matches!(
            &refused,
            Err(PersistError::StaleRevision {
                actual: Some(5),
                ..
            })
        ),
        "{refused:?}"
    );
    let mut missing = answer("c4", 5, "D1", "a", None);
    missing.run_id = "X".to_owned();
    let refused = store.submit_command(missing).await;
    assert!(
        matches!(&refused, Err(PersistError::RunNotFound(_))),
        "{refused:?}"
    );
    assert_eq!(snapshot(&path)?, before);
    assert_eq!(open(&Connection::open(&path)?, "R")?.len(), 1);
    for ended in ["COMPLETED", "FAILED", "CANCELLED"] {
        exec(
            &path,
            &format!("UPDATE runs SET state = '{ended}' WHERE run_id = 'R'"),
        )?;
        let before = snapshot(&path)?;
        for refused in [
            store.submit_command(answer("c5", 5, "D1", "a", None)).await,
            store
                .submit_command(send("c6", 5, "a", "REPLY", None))
                .await,
        ] {
            assert!(
                matches!(&refused, Err(PersistError::IllegalTransition { details: Some(d), .. })
                    if d["state"] == ended),
                "{ended}: {refused:?}"
            );
        }
        assert_eq!(snapshot(&path)?, before, "{ended}");
    }
    Ok(())
}

#[tokio::test]
async fn send_input_answers_the_only_question_and_otherwise_queues_a_steer() -> R {
    // Decision 7's SendInput: a REPLY to no role with exactly one open question answers it (input_queue REPLY APPLIED
    // {text, question_id}; input.accepted names the question); with none or two, or another kind, it is queued
    // (REPLY as STEER, QUEUED, input.queued) and the run's revision touched. In a stay state the commit ends with its
    // run.result; in a working state there is none.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[("commander:D1", "NEED_DECISION")],
        COMMANDER,
    )?;
    let before = max_seq(&path)?;
    store
        .submit_command(send("c1", 5, "方案B", "REPLY", None))
        .await?;
    let accepted = last_of(&path, "input.accepted")?;
    assert_eq!(
        (
            &accepted["command_id"],
            &accepted["question_id"],
            &accepted["kind"]
        ),
        (&json!("c1"), &json!("D1"), &json!("REPLY"))
    );
    assert_eq!(kinds_after(&path, before)?[0], "input.accepted");
    assert_eq!(last_of(&path, "question.answered")?["choice_id"], "b");
    assert_eq!(
        value(
            &path,
            "SELECT json_array(kind, state, json(payload_json)) FROM input_queue"
        )?,
        json!(["REPLY", "APPLIED", {"text": "方案B", "question_id": "D1"}])
    );
    assert_eq!(run_state(&path)?, json!("SPECIFYING/6"));
    // No question now, a working run: queued as a STEER, touched, no run.result.
    let before = max_seq(&path)?;
    store
        .submit_command(send("c2", 6, "再看看", "REPLY", None))
        .await?;
    assert_eq!(
        kinds_after(&path, before)?,
        ["input.accepted", "input.queued"]
    );
    assert_eq!(
        last_of(&path, "input.accepted")?["question_id"],
        json!(null)
    );
    assert_eq!(last_of(&path, "input.accepted")?["kind"], "STEER");
    assert_eq!(run_state(&path)?, json!("SPECIFYING/7"));
    // A stay state with two questions: queued, and the commit ends with its run.result.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    world(
        &path,
        "BLOCKED",
        &[("commander:D1", "NEED_DECISION")],
        &format!("{COMMANDER}{}", work_task("agent_failed")),
    )?;
    let before = max_seq(&path)?;
    store
        .submit_command(send("c1", 5, "x", "REPLY", None))
        .await?;
    assert_eq!(
        kinds_after(&path, before)?,
        ["input.accepted", "input.queued", "run.result"]
    );
    store
        .submit_command(send("c2", 6, "x", "ASK", Some("developer")))
        .await?;
    assert_eq!(
        value(
            &path,
            "SELECT group_concat(kind || ':' || state || ':' || coalesce(target_role, '-'), ',') FROM input_queue"
        )?,
        json!("STEER:QUEUED:-,ASK:QUEUED:developer")
    );
    assert_eq!(open(&Connection::open(&path)?, "R")?.len(), 2);
    Ok(())
}

/// A store hook that fails every commit while it is set (an injected commit failure).
#[derive(Debug, Default)]
struct FailCommits(std::sync::atomic::AtomicBool);

impl FaultHooks for FailCommits {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.load(std::sync::atomic::Ordering::SeqCst) {
            Err(PersistError::Storage("injected".to_owned()))
        } else {
            Ok(())
        }
    }
}

/// R BLOCKED only by the pace's claim (resume RUNNING) for its task T's question of `reason`.
fn paced_world(path: &Path, reason: &str) -> R {
    world(
        path,
        "BLOCKED",
        &[("pace", "NEED_DECISION")],
        &format!(
            "UPDATE runs SET resume_state = 'RUNNING' WHERE run_id = 'R';{}",
            work_task(reason)
        ),
    )
}

#[tokio::test]
async fn a_work_tasks_continue_moves_it_back_unblocking_a_run_blocked_only_by_it() -> R {
    // Decision 8's work row, decision 7's pace recomputed, the exits table's "答复让 Task 回 READY 而 Run BLOCKED 只因
    // PACE": continue (the default, any text) clears T's blocked reason, BLOCKED -> READY, its attempt_count kept (one
    // more try per answer); the pace's claim recomputed by its predicate is released in the same commit and the run
    // goes back to RUNNING (run.resumed), with no BLOCKED result. The next attempt's prompt carries the answer.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    paced_world(&path, "retries_exhausted")?;
    let before = max_seq(&path)?;
    store
        .submit_command(answer("c1", 5, "T/need", "换个思路再试", None))
        .await?;
    assert_eq!(
        kinds_after(&path, before)?,
        [
            "question.answer_accepted",
            "question.answered",
            "run.blocker_released",
            "run.resumed"
        ]
    );
    assert_eq!(
        value(
            &path,
            "SELECT state || '/' || attempt_count || '/' || layer || coalesce(blocked_reason, '') FROM tasks"
        )?,
        json!("READY/9/2")
    );
    assert_eq!(run_state(&path)?, json!("RUNNING/6"));
    assert_eq!(last_of(&path, "question.answered")?["exit"], "continue");
    let lines = crate::persistence::context::answers(&Connection::open(&path)?, "T", "next")?;
    assert_eq!(lines, ["用户的答复（问题 T/need，continue）：换个思路再试"]);
    // cancel: T CANCELLED; nothing of the run blocks any more.
    let dir = tempfile::tempdir()?;
    let (store, path) = open_store(&dir)?;
    paced_world(&path, "agent_failed")?;
    store
        .submit_command(answer("c1", 5, "T/need", "取消该 Task", None))
        .await?;
    assert_eq!(value(&path, "SELECT state FROM tasks")?, json!("CANCELLED"));
    assert_eq!(last_of(&path, "question.answered")?["exit"], "cancel");
    assert_eq!(run_state(&path)?, json!("RUNNING/6"));
    Ok(())
}

#[tokio::test]
async fn every_exit_of_a_refused_repair_clears_the_refusal_in_its_own_transaction() -> R {
    // Decision 8's repair refusal (M2T13D-02, -62) and the work repair row: with a refusal standing (`refusal: true`),
    // rework clears it (repair.refusal_cleared, by the command) before T moves BLOCKED -> READY in layer 0, so it is no
    // longer refused; cancel clears it too, abandons the failure item and cancels T. An injected commit failure leaves
    // neither the clearing nor the move: the question still open, its refusal standing (E5).
    for (text, exit, state) in [
        ("rework", "rework", "READY/0"),
        ("cancel", "cancel", "CANCELLED/2"),
    ] {
        let dir = tempfile::tempdir()?;
        let faults = Arc::new(FailCommits::default());
        let (store, path) = open_with(&dir, faults.clone())?;
        paced_world(&path, "local_repairs_exhausted")?;
        let q = &open(&Connection::open(&path)?, "R")?[0];
        assert!(q.refusal && q.route == Route::Rework, "{q:?}");
        faults.0.store(true, std::sync::atomic::Ordering::SeqCst);
        let before = snapshot(&path)?;
        assert!(
            store
                .submit_command(answer("c1", 5, "T/need", text, None))
                .await
                .is_err()
        );
        assert_eq!(snapshot(&path)?, before);
        assert!(super::super::verification::refused(&Connection::open(&path)?, "R")?.contains("T"));
        faults.0.store(false, std::sync::atomic::Ordering::SeqCst);
        let before = max_seq(&path)?;
        store
            .submit_command(answer("c1", 5, "T/need", text, None))
            .await?;
        let kinds = kinds_after(&path, before)?;
        assert_eq!(
            kinds[..3],
            [
                "question.answer_accepted",
                "question.answered",
                "repair.refusal_cleared"
            ]
        );
        assert_eq!(
            last_of(&path, "repair.refusal_cleared")?["decision_id"],
            "c1"
        );
        assert_eq!(last_of(&path, "question.answered")?["exit"], exit);
        assert_eq!(
            value(&path, "SELECT state || '/' || layer FROM tasks")?,
            json!(state),
            "{text}"
        );
        assert!(
            !super::super::verification::refused(&Connection::open(&path)?, "R")?.contains("T")
        );
        if text == "cancel" {
            let abandoned = last_of(&path, "verification.failures_abandoned")?;
            assert_eq!(
                (&abandoned["task_id"], &abandoned["fingerprint"]),
                (&json!("T"), &json!("f1"))
            );
        }
        assert_eq!(open(&Connection::open(&path)?, "R")?, Vec::new());
    }
    Ok(())
}

mod real {
    //! Decision 8's verification rows through the writers' real paths (M2T13D-02, -03): the completion world of
    //! completion/tests.rs (run R RUNNING, developer task D, qa and reviewer bound), its rounds opened by D's freezes,
    //! closed and joined by the reconcile; then the answer, and the next real admission or join.
    use super::super::{Route, open};
    use crate::ids::child_id;
    use crate::persistence::completion::tests::{
        R, completion, freeze, freeze_of, open as world, result_of, texts, verifiers,
    };
    use crate::persistence::issues::tests::{events, freeze_d, reconcile};
    use crate::persistence::join::tests::{start_all, two_sources, verify};
    use crate::persistence::{
        CommandOutcome, Input, Intent, NewEvent, PersistError, RunTarget, Store, SubmitCommand,
        WriterCompleted, WriterCompletion,
    };
    use rusqlite::Connection;
    use serde_json::{Value, json};
    use std::path::Path;

    fn one(path: &Path, sql: &str) -> R<String> {
        Ok(texts(path, sql)?.pop().ok_or(format!("no row: {sql}"))?)
    }

    fn revision(path: &Path) -> R<i64> {
        Ok(one(
            path,
            "SELECT CAST(revision AS TEXT) FROM runs WHERE run_id = 'R'",
        )?
        .parse()?)
    }

    fn task(path: &Path, task: &str) -> R<String> {
        one(
            path,
            &format!(
                "SELECT state || coalesce(':' || blocked_reason, '') FROM tasks WHERE task_id = '{task}'"
            ),
        )
    }

    async fn answer(
        store: &Store,
        path: &Path,
        id: &str,
        question: &str,
        text: &str,
    ) -> R<Result<CommandOutcome, PersistError>> {
        answer_with(store, path, id, question, text, None).await
    }

    async fn answer_with(
        store: &Store,
        path: &Path,
        id: &str,
        question: &str,
        text: &str,
        choice: Option<&str>,
    ) -> R<Result<CommandOutcome, PersistError>> {
        let command = SubmitCommand {
            command_id: id.to_owned(),
            idempotency_key: format!("k-{id}"),
            kind: "AnswerQuestion".to_owned(),
            run_id: "R".to_owned(),
            expected_revision: revision(path)?,
            payload: json!({}),
            payload_sha256: crate::context::sha256_hex(id.as_bytes()),
            target: RunTarget::Input(Input::Answer {
                question_id: question.to_owned(),
                answer: text.to_owned(),
                choice_id: choice.map(str::to_owned),
            }),
            event: NewEvent {
                intent: Intent::Progress,
                project_id: "p1".to_owned(),
                event_id: format!("e-{id}"),
                run_id: Some("R".to_owned()),
                kind: "question.answer_accepted".to_owned(),
                payload: json!({"command_id": id}),
            },
            outbox: None,
        };
        Ok(store.submit_command(command).await)
    }

    fn snapshot(path: &Path) -> R<Vec<String>> {
        texts(
            path,
            "SELECT json_array((SELECT count(*) FROM events), (SELECT count(*) FROM outbox), \
             (SELECT group_concat(task_id || state || revision || coalesce(blocked_reason, '')) FROM tasks), \
             (SELECT group_concat(role || role_epoch || coalesce(session_id, '-')) FROM role_bindings))",
        )
    }

    #[tokio::test]
    async fn a_join_gate_refusal_answered_reverify_opens_the_next_round_and_joins_under_the_next_number()
    -> R {
        // W9's real path (join.rs): D also holds a forged REQ-9 the gate refuses: D BLOCKED, need_decision
        // {join_gate_refused}. Its question: subject source, candidate VERIFYING, reverify or rework, no refusal.
        // Answered reverify: D BLOCKED -> VERIFYING and round 2 on the same candidate, its own chain's root, with its
        // opened_by; once its verifiers pass (REQ-9 gone), the join is join/2 and the candidate ACCEPTED (E6).
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        Connection::open(&path)?.execute_batch(
            "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-9', 's1', 'r9', 1);
             INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-9', 's1');",
        )?;
        let c1 = freeze_d(&store, &path, 0, 'b').await?;
        let r1 = child_id(&c1, "round/1");
        start_all(&store, &path).await?;
        verify(
            &store,
            &path,
            (&r1, &c1, "rec-1"),
            &["AC-1-01"],
            ("PASS", "PASS"),
        )
        .await?;
        reconcile(&store).await?;
        assert_eq!(task(&path, "D")?, "BLOCKED:NEED_DECISION");
        let questions = open(&Connection::open(&path)?, "R")?;
        assert_eq!(questions.len(), 1, "{questions:?}");
        let q = &questions[0];
        assert_eq!(
            (
                q.subject,
                q.reason.as_deref(),
                q.refusal,
                q.route,
                q.join_id.clone()
            ),
            (
                Some("source"),
                Some("join_gate_refused"),
                false,
                Route::Reverify,
                Some(child_id(&c1, "join/1"))
            )
        );
        assert_eq!(q.candidate_state.as_deref(), Some("VERIFYING"));
        answer(&store, &path, "c1", &q.question_id, "reverify").await??;
        assert_eq!(task(&path, "D")?, "VERIFYING");
        let r2 = child_id(&c1, "round/2");
        let opened = events(&path, "verification.round_opened")?;
        let second = opened
            .iter()
            .find(|o| o["round_id"] == json!(r2))
            .ok_or("round 2")?;
        assert_eq!(
            (
                &second["number"],
                &second["chain_root"],
                &second["reverify_of"],
                &second["source_task_ids"]
            ),
            (&json!(2), &json!(r2), &json!(r1), &json!(["D"]))
        );
        assert!(second["opened_by"]["cause"].is_string(), "{second}");
        Connection::open(&path)?
            .execute_batch("DELETE FROM task_requirements WHERE requirement_id = 'REQ-9';")?;
        start_all(&store, &path).await?;
        verify(
            &store,
            &path,
            (&r2, &c1, "rec-2"),
            &["AC-1-01"],
            ("PASS", "PASS"),
        )
        .await?;
        reconcile(&store).await?;
        let joined: Vec<Value> = events(&path, "verification.joined")?;
        assert_eq!(
            joined
                .iter()
                .map(|j| (j["join_id"].clone(), j["outcome"].clone()))
                .collect::<Vec<_>>(),
            [
                (json!(child_id(&c1, "join/1")), json!("refused")),
                (json!(child_id(&c1, "join/2")), json!("accepted"))
            ]
        );
        assert_eq!(
            one(
                &path,
                &format!("SELECT state FROM candidates WHERE candidate_id = '{c1}'")
            )?,
            "ACCEPTED"
        );
        Ok(())
    }

    /// The world of the join gate's refusal: D BLOCKED (join_gate_refused) on C1, its round closed; C1.
    async fn refused_by_the_gate(store: &Store, path: &Path) -> R<String> {
        Connection::open(path)?.execute_batch(
            "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-9', 's1', 'r9', 1);
             INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-9', 's1');",
        )?;
        let c1 = freeze_d(store, path, 0, 'b').await?;
        start_all(store, path).await?;
        verify(
            store,
            path,
            (&child_id(&c1, "round/1"), &c1, "rec-1"),
            &["AC-1-01"],
            ("PASS", "PASS"),
        )
        .await?;
        reconcile(store).await?;
        Ok(c1)
    }

    #[tokio::test]
    async fn a_reverify_that_cannot_open_its_round_is_refused_with_nothing_written() -> R {
        // Decision 7's precondition, E5, E13: the task's current round not closed (its close removed by hand): 409
        // ILLEGAL_TRANSITION, details {question_id, precondition: reverify, detail}; nothing written, D still BLOCKED,
        // the question still open. A candidate no longer VERIFYING changes the question itself: rework only, so a
        // reverify by its choice_id is no choice of it (400) and the text "reverify" takes the default, rework.
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        refused_by_the_gate(&store, &path).await?;
        let q = open(&Connection::open(&path)?, "R")?.remove(0);
        Connection::open(&path)?.execute(
            "DELETE FROM events WHERE kind = 'verification.round_closed'",
            [],
        )?;
        let before = snapshot(&path)?;
        let refused = answer(&store, &path, "c1", &q.question_id, "reverify").await?;
        assert!(
            matches!(&refused, Err(PersistError::IllegalTransition { details: Some(d), .. })
                if d["question_id"] == json!(q.question_id) && d["precondition"] == "reverify"),
            "{refused:?}"
        );
        assert_eq!(snapshot(&path)?, before);
        assert_eq!(task(&path, "D")?, "BLOCKED:NEED_DECISION");
        assert_eq!(open(&Connection::open(&path)?, "R")?.len(), 1);
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        let c1 = refused_by_the_gate(&store, &path).await?;
        Connection::open(&path)?.execute(
            &format!("UPDATE candidates SET state = 'REJECTED' WHERE candidate_id = '{c1}'"),
            [],
        )?;
        let q = open(&Connection::open(&path)?, "R")?.remove(0);
        assert_eq!((q.route, q.choices.len()), (Route::SourceRework, 1));
        let before = snapshot(&path)?;
        let refused =
            answer_with(&store, &path, "c1", &q.question_id, "", Some("reverify")).await?;
        assert!(
            matches!(&refused, Err(PersistError::BadRequest(_))),
            "{refused:?}"
        );
        assert_eq!(snapshot(&path)?, before);
        answer(&store, &path, "c2", &q.question_id, "reverify").await??;
        assert_eq!(task(&path, "D")?, "READY");
        Ok(())
    }

    /// Every question of R now, as (subject, reason, refusal, route): correction A's guard (4), "every real writer's
    /// pair routes".
    type Routed = (Option<&'static str>, Option<String>, bool, Route);

    fn routed(path: &Path) -> R<Vec<Routed>> {
        Ok(open(&Connection::open(path)?, "R")?
            .into_iter()
            .map(|q| (q.subject, q.reason, q.refusal, q.route))
            .collect())
    }

    #[tokio::test]
    async fn a_spent_reverify_chain_answered_reverify_starts_a_new_chain_and_its_join_accepts() -> R
    {
        // W7's real path (M2T13D-02): D's rounds inconclusive until its chain is spent (REVERIFY_LIMIT 2): the
        // reconcile's need_decision{reverify_exhausted}. Its question: source, refusal, reverify or rework, no cancel.
        // Answered reverify: in one commit repair.refusal_cleared, D BLOCKED -> VERIFYING and round 4, its chain root
        // itself; once its verifiers pass with the check's evidence, the join is join/4 and C1 ACCEPTED.
        use crate::persistence::completion::tests::{evidence_of, settle_verifiers, verified};
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        let (_, candidate, _) = verified(&store, &path, "PASS", "PASS").await?;
        reconcile(&store).await?;
        settle_verifiers(&store, &path, "PASS", "PASS").await?;
        reconcile(&store).await?;
        settle_verifiers(&store, &path, "PASS", "PASS").await?;
        reconcile(&store).await?;
        assert_eq!(task(&path, "D")?, "BLOCKED:NEED_DECISION");
        assert_eq!(
            routed(&path)?,
            [(
                Some("source"),
                Some("reverify_exhausted".to_owned()),
                true,
                Route::Reverify
            )]
        );
        let q = open(&Connection::open(&path)?, "R")?.remove(0);
        assert!(q.choices.iter().all(|(id, _)| id != "cancel"), "{q:?}");
        let before: i64 = one(&path, "SELECT CAST(max(seq) AS TEXT) FROM events")?.parse()?;
        answer(&store, &path, "c1", &q.question_id, "reverify").await??;
        let kinds = texts(
            &path,
            &format!("SELECT kind FROM events WHERE seq > {before} ORDER BY seq"),
        )?;
        let cleared = kinds
            .iter()
            .position(|k| k == "repair.refusal_cleared")
            .ok_or("cleared")?;
        let opened = kinds
            .iter()
            .position(|k| k == "verification.round_opened")
            .ok_or("opened")?;
        assert!(cleared < opened, "{kinds:?}");
        assert_eq!(task(&path, "D")?, "VERIFYING");
        let r4 = child_id(&candidate, "round/4");
        let round = events(&path, "verification.round_opened")?
            .into_iter()
            .find(|o| o["round_id"] == json!(r4))
            .ok_or("round 4")?;
        assert_eq!(
            (&round["number"], &round["chain_root"]),
            (&json!(4), &json!(r4))
        );
        let [qa, _] = settle_verifiers(&store, &path, "PASS", "PASS").await?;
        evidence_of(
            &path,
            (&qa, &candidate, "rec-4"),
            ("unit", "PASS"),
            &[("AC-1-01", "PASS")],
            true,
        )?;
        reconcile(&store).await?;
        let joined = events(&path, "verification.joined")?;
        assert_eq!(
            joined
                .last()
                .map(|j| (j["join_id"].clone(), j["outcome"].clone())),
            Some((json!(child_id(&candidate, "join/4")), json!("accepted")))
        );
        assert_eq!(
            one(
                &path,
                &format!("SELECT state FROM candidates WHERE candidate_id = '{candidate}'")
            )?,
            "ACCEPTED"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_verifiers_own_spent_reverifications_and_unknown_effect_are_redispatched_to_the_next_admission()
    -> R {
        // W4's and W10's real paths: the qa verifier's own FAILED results reverified within its round until refused
        // (need_decision{reverify_exhausted} on the verifier task: subject verifier, refusal, redispatch only); and,
        // in another world, its attempt ended UNKNOWN_EFFECT (verifier_unknown_effect, no refusal). Answered: the
        // refusal cleared (no longer refused, so the next scheduling round does not filter it out), the task READY
        // with its attempt count kept, and the next scheduling round leases it again.
        use crate::persistence::completion::tests::{
            ending, verifier_completion, verifier_running,
        };
        use crate::persistence::{VerifierCompleted, VerifierOutcome};
        for unknown in [false, true] {
            let dir = tempfile::tempdir()?;
            let (store, path) = world(dir.path())?;
            let (_, task_id, mut attempt) = verifier_running(&store, &path).await?;
            if unknown {
                let mut ended = ending();
                ended.unknown_effect = true;
                store.runner_finished(&attempt, ended).await?;
            } else {
                for n in 0..3 {
                    let done = store
                        .complete_verifier_attempt(verifier_completion(
                            &attempt,
                            VerifierOutcome::Reverify {
                                status: "FAILED".to_owned(),
                                reason: "it failed".to_owned(),
                            },
                        ))
                        .await?;
                    if n < 2 {
                        assert!(
                            matches!(done, VerifierCompleted::Reverified { .. }),
                            "{done:?}"
                        );
                        attempt = format!("{task_id}/again-{n}");
                        Connection::open(&path)?.execute_batch(&format!(
                            "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{task_id}';
                             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch,
                               config_revision) VALUES ('{attempt}', '{task_id}', 'sq', 'RUNNING', 1, 1, 1);"
                        ))?;
                    }
                }
            }
            assert_eq!(task(&path, &task_id)?, "BLOCKED:NEED_DECISION");
            let reason = if unknown {
                "verifier_unknown_effect"
            } else {
                "reverify_exhausted"
            };
            assert_eq!(
                routed(&path)?,
                [(
                    Some("verifier"),
                    Some(reason.to_owned()),
                    !unknown,
                    Route::Redispatch(super::super::Redispatch::Plain)
                )]
            );
            let count = one(
                &path,
                &format!(
                    "SELECT CAST(attempt_count AS TEXT) FROM tasks WHERE task_id = '{task_id}'"
                ),
            )?;
            let q = open(&Connection::open(&path)?, "R")?.remove(0);
            answer(&store, &path, "c1", &q.question_id, "好").await??;
            assert_eq!(task(&path, &task_id)?, "READY");
            assert_eq!(
                one(
                    &path,
                    &format!(
                        "SELECT CAST(attempt_count AS TEXT) FROM tasks WHERE task_id = '{task_id}'"
                    )
                )?,
                count
            );
            assert!(
                !crate::persistence::verification::refused(&Connection::open(&path)?, "R")?
                    .contains(&task_id)
            );
            let mut again = None;
            while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
                if d.task_id == task_id {
                    again = Some(d.attempt_id);
                }
            }
            assert!(
                again.is_some(),
                "the verifier dispatched again (unknown {unknown})"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_developers_own_end_and_spent_corrections_ask_the_work_question_and_its_answer_dispatches_it()
    -> R {
        // W12's and W3's real paths (completion.rs): D's attempt reports FAILED (need_decision{agent_failed}: work,
        // continue or cancel); in another world its output is to be corrected past max_protocol_repairs (2): the
        // refusal need_decision{protocol_repairs_exhausted} (work, refusal, rework or cancel). Answered continue and
        // rework: D READY (rework in layer 0, its refusal cleared) and the next scheduling round leases it again.
        use crate::persistence::WriterOutcome;
        use crate::persistence::need::WorkReason;
        for correct in [false, true] {
            let dir = tempfile::tempdir()?;
            let (store, path) = world(dir.path())?;
            let mut attempt = "A".to_owned();
            for n in 0..3 {
                let outcome = if correct {
                    WriterOutcome::Correct {
                        reasons: vec!["no TaskResult".to_owned()],
                    }
                } else {
                    WriterOutcome::Decide {
                        reason: WorkReason::AgentFailed,
                        detail: "{}".to_owned(),
                    }
                };
                store
                    .complete_writer_attempt(WriterCompletion {
                        attempt_id: attempt.clone(),
                        ..completion(outcome)
                    })
                    .await?;
                if !correct || n == 2 {
                    break;
                }
                attempt = format!("A{n}");
                Connection::open(&path)?.execute_batch(&format!(
                    "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'D';
                     INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch,
                       config_revision, started_at) VALUES ('{attempt}', 'D', 'sd', 'RUNNING', 1, 1, 1,
                       '2026-11-12T00:00:0{n}Z');"
                ))?;
            }
            assert_eq!(task(&path, "D")?, "BLOCKED:NEED_DECISION");
            let (reason, route, text) = if correct {
                ("protocol_repairs_exhausted", Route::Rework, "rework")
            } else {
                ("agent_failed", Route::Continue, "continue")
            };
            assert_eq!(
                routed(&path)?,
                [(Some("work"), Some(reason.to_owned()), correct, route)]
            );
            let q = open(&Connection::open(&path)?, "R")?.remove(0);
            answer(&store, &path, "c1", &q.question_id, text).await??;
            assert_eq!(task(&path, "D")?, "READY");
            let context = crate::decision::Context {
                required_capabilities: [("developer".to_owned(), Default::default())].into(),
                ..verifiers()
            };
            let d = store
                .schedule_round("R", context)
                .await?
                .dispatched
                .ok_or("D dispatched")?;
            assert_eq!(d.task_id, "D", "correct {correct}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_rejected_report_is_redispatched_and_rebound_only_when_its_conflict_is_the_bound_session()
    -> R {
        // W11 report_rejected's real path (the report's intake, reports.rs) and decision 8's row: the qa's report
        // names the reviewer's session sr (REPORT_SESSION_MISMATCH, SESSION_ROLE_MISMATCH, SESSION_NOT_INDEPENDENT of
        // sr): the conflicting session is not the one qa is bound to, so the answer redispatches it plainly (epoch 1).
        // When the qa's own bound session sq is the conflicting one (the developer's binding forged onto it before the
        // report), the answer rebinds qa to epoch 2 as for SESSION_NOT_INDEPENDENT.
        use crate::persistence::completion::tests::{
            consistent, fresh_objects, verifier_completion, verifier_running,
        };
        use crate::persistence::{VerifierCompleted, VerifierOutcome};
        for own in [false, true] {
            let dir = tempfile::tempdir()?;
            let (store, path) = world(dir.path())?;
            let (_, task_id, attempt) = verifier_running(&store, &path).await?;
            let mut report = consistent(&path, &attempt, ("qa", "FAIL"), fresh_objects(&path)?)?;
            if own {
                Connection::open(&path)?.execute_batch(
                    "UPDATE role_bindings SET session_id = 'sq' WHERE run_id = 'R' AND role = 'developer';",
                )?;
            } else {
                report.report_json["session_id"] = json!("sr");
            }
            let done = store
                .complete_verifier_attempt(verifier_completion(
                    &attempt,
                    VerifierOutcome::Settle {
                        report: Box::new(report),
                    },
                ))
                .await?;
            assert!(
                matches!(done, VerifierCompleted::ReportRejected { .. }),
                "{done:?}"
            );
            assert_eq!(
                routed(&path)?,
                [(
                    Some("verifier"),
                    Some("report_rejected".to_owned()),
                    false,
                    Route::Redispatch(super::super::Redispatch::Report)
                )]
            );
            let q = open(&Connection::open(&path)?, "R")?.remove(0);
            answer(&store, &path, "c1", &q.question_id, "").await??;
            assert_eq!(task(&path, &task_id)?, "READY");
            assert_eq!(
                one(
                    &path,
                    "SELECT CAST(role_epoch AS TEXT) FROM role_bindings WHERE role = 'qa'"
                )?,
                if own { "2" } else { "1" },
                "own {own}"
            );
            assert_eq!(
                texts(
                    &path,
                    "SELECT json_extract(payload_json, '$.cause') FROM events \
                    WHERE kind = 'team.binding_applied'"
                )?,
                if own {
                    vec!["rebind".to_owned()]
                } else {
                    Vec::new()
                }
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_flaky_join_answered_rework_reworks_and_the_same_content_is_corrected_not_verified_again()
    -> R {
        // W8's real path for a join (join.rs): a late UNKNOWN row makes C's join flaky: D and E BLOCKED
        // need_decision{flaky}. D's question: rework only; any answer reworks it (READY, layer 0). Its next attempt
        // freezes the same content: the same-content guard sees the join's flaky class (no flaky round on C) and
        // corrects it instead of opening another round on C (REQ-053 decision 1).
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        let (c, r1, r2) = two_sources(&store, &path).await?;
        let (qa1, _) = verify(
            &store,
            &path,
            (&r1, &c, "rec-1"),
            &["AC-1-01"],
            ("PASS", "PASS"),
        )
        .await?;
        reconcile(&store).await?;
        crate::persistence::completion::tests::evidence_with(
            &path,
            (&qa1, &c, "rec-late"),
            ("unit", "UNKNOWN"),
            &[("AC-1-01", "TEST-001-01", "UNKNOWN")],
            &[("TEST-001-01", "test_001_01_late", "UNKNOWN")],
        )?;
        Connection::open(&path)?.execute(
            "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status, reason) \
             VALUES ('AC-1-01', 's1', ?1, 'rec-late', 'UNKNOWN', 'TEST-001-01 in check unit')",
            [&c],
        )?;
        verify(
            &store,
            &path,
            (&r2, &c, "rec-2"),
            &["AC-2-01"],
            ("PASS", "PASS"),
        )
        .await?;
        reconcile(&store).await?;
        let flaky_rounds = texts(
            &path,
            "SELECT event_id FROM events WHERE kind = 'verification.round_closed' \
             AND json_extract(payload_json, '$.verdict') = 'flaky'",
        )?;
        assert!(
            flaky_rounds.is_empty(),
            "only the join is flaky: {flaky_rounds:?}"
        );
        let questions = open(&Connection::open(&path)?, "R")?;
        let q = questions
            .iter()
            .find(|q| q.task_id.as_deref() == Some("D"))
            .ok_or("D's question")?;
        assert_eq!(
            (q.subject, q.reason.as_deref(), q.route, q.choices.len()),
            (Some("source"), Some("flaky"), Route::SourceRework, 1)
        );
        answer(&store, &path, "c1", &q.question_id, "随便").await??;
        assert_eq!(task(&path, "D")?, "READY");
        assert_eq!(
            one(
                &path,
                "SELECT CAST(layer AS TEXT) FROM tasks WHERE task_id = 'D'"
            )?,
            "0"
        );
        // D's next attempt freezes the same content.
        Connection::open(&path)?.execute_batch(
            "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'D';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
               started_at) VALUES ('A9', 'D', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:09Z');",
        )?;
        let rounds = events(&path, "verification.round_opened")?.len();
        let done = store
            .complete_writer_attempt(WriterCompletion {
                attempt_id: "A9".to_owned(),
                ..completion(freeze(freeze_of('b'), result_of("D", "A9")?))
            })
            .await?;
        assert!(
            matches!(done, WriterCompleted::Corrected { .. }),
            "{done:?}"
        );
        assert_eq!(events(&path, "verification.round_opened")?.len(), rounds);
        Ok(())
    }

    #[tokio::test]
    async fn an_invalid_handoff_answered_is_rebuilt_and_the_next_admission_takes_it() -> R {
        // W11 INVALID_HANDOFF's real path (verification.rs admit): round 1's qa handoff corrupted (its package's
        // epoch disagrees with its envelope): the dispatch refuses it, the qa task BLOCKED. Answered: the handoff
        // rebuilt from the round's round_opened and opened_by (verification.handoff_reissued{reason: rebuilt}, a new
        // outbox row), the qa task READY; the next scheduling round's admission goes with the rebuilt handoff and leases
        // a PREPARED attempt. Without opened_by (a round opened before it was recorded): 409, nothing written (E13).
        for recorded in [true, false] {
            let dir = tempfile::tempdir()?;
            let (store, path) = world(dir.path())?;
            let c1 = freeze_d(&store, &path, 0, 'b').await?;
            let round = child_id(&c1, "round/1");
            let qa = child_id(&round, "verify/qa");
            let qa_task = child_id(&round, "task/qa");
            Connection::open(&path)?.execute(
                "UPDATE outbox SET payload_json = json_set(payload_json, '$.payload.role_epoch', 7) WHERE message_id = ?1",
                [&qa],
            )?;
            if !recorded {
                Connection::open(&path)?.execute(
                    "UPDATE events SET payload_json = json_remove(payload_json, '$.opened_by') \
                     WHERE kind = 'verification.round_opened'",
                    [],
                )?;
            }
            while store
                .schedule_round("R", verifiers())
                .await?
                .dispatched
                .is_some()
            {}
            assert_eq!(task(&path, &qa_task)?, "BLOCKED:NEED_DECISION");
            let q = open(&Connection::open(&path)?, "R")?.remove(0);
            assert_eq!(
                (
                    q.subject,
                    q.reason.as_deref(),
                    q.task_id.as_deref(),
                    q.round_id.as_deref()
                ),
                (
                    Some("verifier"),
                    Some("INVALID_HANDOFF"),
                    Some(qa_task.as_str()),
                    Some(round.as_str())
                )
            );
            if !recorded {
                let before = snapshot(&path)?;
                let refused = answer(&store, &path, "c1", &q.question_id, "redispatch").await?;
                assert!(
                    matches!(&refused, Err(PersistError::IllegalTransition { details: Some(d), .. })
                        if d["precondition"] == "rebuilt_handoff"),
                    "{refused:?}"
                );
                assert_eq!(snapshot(&path)?, before);
                continue;
            }
            answer(&store, &path, "c1", &q.question_id, "").await??;
            let rebuilt = child_id(&qa, "rebuilt");
            let reissued = events(&path, "verification.handoff_reissued")?;
            assert_eq!(
                (
                    &reissued[0]["from"],
                    &reissued[0]["to"],
                    &reissued[0]["reason"],
                    &reissued[0]["command_id"]
                ),
                (&json!(qa), &json!(rebuilt), &json!("rebuilt"), &json!("c1"))
            );
            assert_eq!(task(&path, &qa_task)?, "READY");
            let mut leased = None;
            while let Some(d) = store.schedule_round("R", verifiers()).await?.dispatched {
                if d.task_id == qa_task {
                    leased = Some(d.attempt_id);
                }
            }
            let attempt = leased.ok_or("the qa task dispatched again")?;
            assert_eq!(
                one(
                    &path,
                    &format!("SELECT state FROM attempts WHERE attempt_id = '{attempt}'")
                )?,
                "PREPARED"
            );
            assert_eq!(
                texts(
                    &path,
                    &format!(
                        "SELECT json_extract(payload_json, '$.handoff') FROM outbox \
                         WHERE json_extract(payload_json, '$.attempt_id') = '{attempt}'"
                    )
                )?,
                [rebuilt]
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_verifier_not_independent_answered_moves_its_role_to_the_next_epoch_and_is_admitted()
    -> R {
        // W11 SESSION_NOT_INDEPENDENT's real path: a forged binding gives the reviewer the developer's session: its
        // dispatch is refused. While the reviewer has an attempt in flight in the run the answer is refused (409,
        // nothing written). Once not: the binding at epoch 2 (same agent, no session, team.binding_applied{cause:
        // rebind}), the task READY; its session granted (session_id(R, reviewer, 2)), the next admission goes, issuing
        // the handoff at epoch 2, and the attempt's session is not one the round refuses (E13).
        let dir = tempfile::tempdir()?;
        let (store, path) = world(dir.path())?;
        let c1 = freeze_d(&store, &path, 0, 'b').await?;
        let round = child_id(&c1, "round/1");
        let reviewer = child_id(&round, "task/reviewer");
        Connection::open(&path)?.execute_batch(
            "UPDATE role_bindings SET session_id = 'sd' WHERE run_id = 'R' AND role = 'reviewer';",
        )?;
        while store
            .schedule_round("R", verifiers())
            .await?
            .dispatched
            .is_some()
        {}
        assert_eq!(task(&path, &reviewer)?, "BLOCKED:NEED_DECISION");
        let q = open(&Connection::open(&path)?, "R")?.remove(0);
        assert_eq!(q.reason.as_deref(), Some("SESSION_NOT_INDEPENDENT"));
        // A reviewer attempt in flight (forged on another round's task): refused.
        Connection::open(&path)?.execute_batch(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq)
               VALUES ('V2', 'R', 's1', 'RUNNING', 1, 'reviewer', 0, 99);
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision)
               VALUES ('AV2', 'V2', 'sr', 'RUNNING', 1, 1, 1);",
        )?;
        let before = snapshot(&path)?;
        let refused = answer(&store, &path, "c1", &q.question_id, "redispatch").await?;
        assert!(
            matches!(&refused, Err(PersistError::IllegalTransition { details: Some(d), .. })
                if d["precondition"] == "independent_session"),
            "{refused:?}"
        );
        assert_eq!(snapshot(&path)?, before);
        Connection::open(&path)?.execute_batch(
            "UPDATE attempts SET state = 'SETTLED' WHERE attempt_id = 'AV2';
                 UPDATE tasks SET state = 'CANCELLED' WHERE task_id = 'V2';",
        )?;
        answer(&store, &path, "c1", &q.question_id, "redispatch").await??;
        assert_eq!(
            one(
                &path,
                "SELECT role_epoch || '/' || coalesce(session_id, '-') || '/' || state FROM role_bindings \
                WHERE role = 'reviewer'"
            )?,
            "2/-/ACTIVE"
        );
        let applied = events(&path, "team.binding_applied")?;
        assert_eq!(
            applied.last().map(|a| a["cause"].clone()),
            Some(json!("rebind"))
        );
        assert_eq!(task(&path, &reviewer)?, "READY");
        let session = crate::persistence::team::session_id("R", "reviewer", 2);
        Connection::open(&path)?.execute(
            "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES (?1, 'codex', \
             'reviewer', '1', '{}')",
            [&session],
        )?;
        // Staffing's grant and the handover's delivery, as the host's tick does them: the session bound, its
        // session.start ACKED.
        Connection::open(&path)?.execute(
            "UPDATE role_bindings SET session_id = ?1 WHERE role = 'reviewer'",
            [&session],
        )?;
        Connection::open(&path)?.execute(
            "UPDATE outbox SET state = 'ACKED' WHERE message_id = ?1",
            [child_id("R", "session-start/reviewer/2")],
        )?;
        let mut leased = None;
        let slots = crate::decision::Context {
            reader_slots: Some(8),
            ..verifiers()
        };
        while let Some(d) = store.schedule_round("R", slots.clone()).await?.dispatched {
            if d.task_id == reviewer {
                leased = Some(d.attempt_id);
            }
        }
        let attempt = leased.ok_or("the reviewer dispatched again")?;
        assert_eq!(
            one(
                &path,
                &format!(
                    "SELECT coalesce(session_id, '-') || '/' || role_epoch FROM attempts \
                WHERE attempt_id = '{attempt}'"
                )
            )?,
            format!("{session}/2")
        );
        let conflicting = crate::persistence::verification::conflicting_sessions(
            &Connection::open(&path)?,
            "R",
            &round,
            "reviewer",
        )?;
        assert!(!conflicting.contains(&session), "{conflicting:?}");
        Ok(())
    }
}
