//! The join at a candidate's verification barrier against a real database (docs/adr/ADR-029.md decisions 11, 12 and
//! 13, J3, J8, J9): accepted, failed, flaky, inconclusive and refused joins, its idempotence, a database M2-T16 left,
//! the barrier that waits for every round of its candidate, the confirmations it recomputes, its iterations and the
//! pace without a "waiting for the join" in flight.

use super::*;
use crate::ids::child_id;
use crate::persistence::completion::tests::{
    R, completion, evidence_of, evidence_with, freeze, freeze_of, open, result_of, second_writer,
    texts, verifiers,
};
use crate::persistence::issues::tests::{
    X_SUMMARY, agrees, closed, confirm, delivered_to, events, finding, freeze_d, items, reconcile,
    settle, state_of,
};
use crate::persistence::{Store, WriterCompletion};
use crate::verification::acceptance;
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

/// The `verification.joined` payloads of candidate `candidate`, in order.
fn joined(path: &Path, candidate: &str) -> R<Vec<Value>> {
    Ok(events(path, "verification.joined")?
        .into_iter()
        .filter(|e| e["candidate_id"] == json!(candidate))
        .collect())
}

/// One column of `sql`'s rows as text.
fn one(path: &Path, sql: &str) -> R<String> {
    Ok(texts(path, sql)?.pop().ok_or(format!("no row: {sql}"))?)
}

fn candidate_state(path: &Path, candidate: &str) -> R<String> {
    one(
        path,
        &format!("SELECT state FROM candidates WHERE candidate_id = '{candidate}'"),
    )
}

fn task_state(path: &Path, task: &str) -> R<String> {
    one(
        path,
        &format!(
            "SELECT state || coalesce(':' || blocked_reason, '') FROM tasks WHERE task_id = '{task}'"
        ),
    )
}

/// The verdicts of candidate `candidate`'s iterations, in order.
fn iterations(path: &Path, candidate: &str) -> R<Vec<String>> {
    texts(
        path,
        &format!(
            "SELECT json_extract(payload_json, '$.verdict') FROM events WHERE kind = 'iteration.completed' \
             AND json_extract(payload_json, '$.candidate_id') = '{candidate}' ORDER BY seq"
        ),
    )
}

/// Every READY verifier task dispatched and started (eight reader slots: no round waits for another's), each attempt's
/// delivery recorded as the executor records it from the store's assignment (`context.packed` `confirm_ids`).
async fn start_all(store: &Store, path: &Path) -> R {
    let context = crate::decision::Context {
        reader_slots: Some(8),
        ..verifiers()
    };
    while let Some(d) = store.schedule_round("R", context.clone()).await?.dispatched {
        let role = if d.task_id.ends_with("/qa") {
            "qa"
        } else {
            "reviewer"
        };
        let conn = Connection::open(path)?;
        conn.execute_batch(&format!(
            "UPDATE attempts SET state = 'RUNNING' WHERE attempt_id = '{0}';
             UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = '{1}';",
            d.attempt_id, d.task_id
        ))?;
        let assigned = crate::persistence::team::assignment(
            &conn,
            "p1",
            "R",
            &d.task_id,
            &d.attempt_id,
            role,
        )?;
        let ids = assigned
            .context
            .and_then(|f| f.confirm_ids)
            .ok_or("a verifier's facts carry its confirm_ids")?;
        conn.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
             'context.packed', json_object('attempt_id', ?2, 'confirm_ids', json(?3)), '2026-11-12T00:00:09Z')",
            params![
                child_id(&d.attempt_id, "context.packed"),
                d.attempt_id,
                json!(ids).to_string()
            ],
        )?;
    }
    Ok(())
}

/// Round `round`'s latest qa and reviewer attempts.
fn attempts_of(path: &Path, round: &str) -> R<(String, String)> {
    let of = |role: &str| {
        one(
            path,
            &format!(
                "SELECT attempt_id FROM attempts WHERE task_id = '{}' ORDER BY rowid DESC LIMIT 1",
                child_id(round, &format!("task/{role}"))
            ),
        )
    };
    Ok((of("qa")?, of("reviewer")?))
}

/// Round `round`'s verifiers (started) settled: QA's PASS evidence of `acs` on `candidate` as `record` (its rows
/// written at the round's close), then the reports `qa` and `review`.
async fn verify(
    store: &Store,
    path: &Path,
    (round, candidate, record): (&str, &str, &str),
    acs: &[&str],
    (qa, review): (&str, &str),
) -> R<(String, String)> {
    let (qa_attempt, reviewer) = attempts_of(path, round)?;
    let rows: Vec<(&str, &str)> = acs.iter().map(|ac| (*ac, "PASS")).collect();
    evidence_of(
        path,
        (&qa_attempt, candidate, record),
        ("unit", "PASS"),
        &rows,
        true,
    )?;
    settle(store, path, &qa_attempt, ("qa", qa), &[], &[]).await?;
    settle(store, path, &reviewer, ("reviewer", review), &[], &[]).await?;
    Ok((qa_attempt, reviewer))
}

/// E (a second writer, REQ-2 with AC-2-01) completes tree `tree` with attempt `attempt` (its first, or a later one):
/// the candidate.
async fn complete_e(store: &Store, path: &Path, attempt: &str, tree: char) -> R<String> {
    if attempt == "AE" {
        second_writer(path, "E", "AE")?;
    } else {
        Connection::open(path)?.execute_batch(&format!(
            "UPDATE tasks SET state = 'RUNNING', revision = revision + 1 WHERE task_id = 'E';
             INSERT INTO attempts(attempt_id, task_id, session_id, state, role_epoch, host_epoch, config_revision,
               started_at) VALUES ('{attempt}', 'E', 'sd', 'RUNNING', 1, 1, 1, '2026-11-12T00:00:05Z');"
        ))?;
    }
    let job = freeze_of(tree);
    let candidate = job.sealed.candidate_id.clone();
    store
        .complete_writer_attempt(WriterCompletion {
            attempt_id: attempt.to_owned(),
            ..completion(freeze(job, result_of("E", attempt)?))
        })
        .await?;
    Ok(candidate)
}

/// A row of AC-1-01 with `status` on `candidate` published after its round closed (the crash window, ADR-028 -601):
/// evidence `record` of QA attempt `attempt` (check unit, the row's outcome) whose row names TEST-001-01's line
/// `test_001_01_late` (failing when FAIL), with its row in `acceptance_results` at once.
fn late_row(path: &Path, (attempt, candidate, record): (&str, &str, &str), status: &str) -> R {
    evidence_with(
        path,
        (attempt, candidate, record),
        ("unit", status),
        &[("AC-1-01", "TEST-001-01", status)],
        &[("TEST-001-01", "test_001_01_late", status)],
    )?;
    Connection::open(path)?.execute(
        "INSERT INTO acceptance_results(ac_id, spec_id, candidate_id, evidence_id, status, reason) \
         VALUES ('AC-1-01', 's1', ?1, ?2, ?3, 'TEST-001-01 in check unit')",
        params![candidate, record, status],
    )?;
    Ok(())
}

/// C with D's round 1 and E's round 2 (the same content), every verifier started.
async fn two_sources(store: &Store, path: &Path) -> R<(String, String, String)> {
    let c = freeze_d(store, path, 0, 'b').await?;
    assert_eq!(complete_e(store, path, "AE", 'b').await?, c);
    start_all(store, path).await?;
    Ok((c.clone(), child_id(&c, "round/1"), child_id(&c, "round/2")))
}

#[tokio::test]
async fn a_single_tasks_passable_join_accepts_its_candidate_and_task_and_the_commander_completes_the_run()
-> R {
    // Decisions 11 to 13 (J3): D's round on C1 passable (both reports PASS, the check PASS, AC-1-01 PASS): the join in
    // the same reconcile, `verification.joined` passable, C1 and D ACCEPTED together (`candidate.accepted`,
    // `task.accepted{by: runtime}`), one iteration (passable); asked again, nothing more (J9). The Commander's
    // Run -> COMPLETED then passes the gate through `load_evidence` and `requirements_passed` (the positive control).
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
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
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    assert_eq!(done.joinable, std::slice::from_ref(&c1));
    let join = child_id(&c1, "join/1");
    let joined = joined(&path, &c1)?;
    assert_eq!(joined.len(), 1, "{joined:?}");
    assert_eq!(
        (
            &joined[0]["join_id"],
            &joined[0]["class"],
            &joined[0]["outcome"],
            &joined[0]["rounds"],
            &joined[0]["tasks"]
        ),
        (
            &json!(join),
            &json!("passable"),
            &json!("accepted"),
            &json!([r1]),
            &json!(["D"])
        )
    );
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT event_id FROM events WHERE kind = 'verification.joined' AND event_id = '{}'",
                child_id(&join, "joined")
            )
        )?
        .len(),
        1
    );
    assert_eq!(candidate_state(&path, &c1)?, "ACCEPTED");
    assert_eq!(task_state(&path, "D")?, "ACCEPTED");
    assert_eq!(
        events(&path, "candidate.accepted")?,
        [json!({"candidate_id": c1, "join_id": join})]
    );
    assert_eq!(
        events(&path, "task.accepted")?,
        [json!({"task_id": "D", "candidate_id": c1, "join_id": join, "by": "runtime"})]
    );
    assert_eq!(iterations(&path, &c1)?, ["passable"]);
    let count = texts(&path, "SELECT CAST(count(*) AS TEXT) FROM events")?;
    assert_eq!(reconcile(&store).await?, Reconciled::default());
    assert_eq!(
        texts(&path, "SELECT CAST(count(*) AS TEXT) FROM events")?,
        count
    );
    // The Commander completes the run on C1: the gate passes.
    Connection::open(&path)?.execute_batch(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
           ('sc', 'codex', 'commander', '1', '{}');
         INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
           ('R', 'commander', 'codex', 'sc', 1, 1, '2026-11-12T00:00:00Z');
         UPDATE runs SET state = 'VERIFYING' WHERE run_id = 'R';",
    )?;
    let revision: u64 = one(
        &path,
        "SELECT CAST(revision AS TEXT) FROM runs WHERE run_id = 'R'",
    )?
    .parse()?;
    let message: crate::protocol::Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": "d1", "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 1, "correlation_id": "c",
        "causation_id": null, "idempotency_key": "k1", "expected_revision": revision,
        "spec_sha256": "1".repeat(64), "candidate_id": c1, "kind": "decision",
        "payload": {"kind": "transition_run", "to_state": "COMPLETED", "accepted_requirements": ["REQ-1"],
            "reason": "every criterion passed on the accepted candidate"}
    }))?;
    let decided = store
        .decide(
            crate::decision::Proposal::from_message(&message)?,
            crate::decision::Context::default(),
        )
        .await?;
    assert!(
        matches!(decided, crate::persistence::Decided::Applied { .. }),
        "{decided:?}"
    );
    assert_eq!(
        one(&path, "SELECT state FROM runs WHERE run_id = 'R'")?,
        "COMPLETED"
    );
    Ok(())
}

#[tokio::test]
async fn two_tasks_on_two_chains_join_on_each_chains_current_round_only() -> R {
    // Decision 11 (-402, CR-018 "only each reverification chain's current round"): on C, D's round 1 is inconclusive
    // (the reviewer UNKNOWN) and verified again in round 3; E's round 2 is passable. The join waits for D's chain, then
    // reads rounds 2 and 3 only: round 1's UNKNOWN report counts for nothing, so C, D and E are ACCEPTED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    verify(
        &store,
        &path,
        (&r1, &c, "rec-1"),
        &["AC-1-01"],
        ("PASS", "UNKNOWN"),
    )
    .await?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(
        done.closed,
        [
            (r1.clone(), "reverify".to_owned()),
            (r2.clone(), "passable".to_owned())
        ]
    );
    let r3 = child_id(&c, "round/3");
    assert_eq!(done.opened, std::slice::from_ref(&r3));
    assert!(joined(&path, &c)?.is_empty(), "D's chain is still open");
    start_all(&store, &path).await?;
    verify(
        &store,
        &path,
        (&r3, &c, "rec-3"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r3.clone(), "passable".to_owned())]);
    let joined = joined(&path, &c)?;
    assert_eq!(joined.len(), 1, "{joined:?}");
    assert_eq!(joined[0]["join_id"], json!(child_id(&c, "join/3")));
    assert_eq!(joined[0]["rounds"], json!([r2, r3]));
    assert_eq!(joined[0]["tasks"], json!(["D", "E"]));
    assert_eq!(joined[0]["inputs"]["review"], json!("Pass"));
    assert_eq!(joined[0]["outcome"], json!("accepted"), "{joined:?}");
    assert_eq!(candidate_state(&path, &c)?, "ACCEPTED");
    assert_eq!(
        (task_state(&path, "D")?, task_state(&path, "E")?),
        ("ACCEPTED".to_owned(), "ACCEPTED".to_owned())
    );
    Ok(())
}

#[tokio::test]
async fn a_fail_row_published_after_its_rounds_close_fails_the_join_and_its_owner_is_reworked_at_once()
-> R {
    // Decision 11's late rows (Q12, J8): D's round passable while E's runs; a FAIL row of AC-1-01 on C arrives after D's
    // round closed (history for that round). E's round passable: the join reads every row on C, so it fails: C
    // REJECTED, the item `ac-fail:AC-1-01:<test>` D's alone (E's scope has no AC-1-01), its acceptance Issue on C, one
    // iteration (repair), and D reworked in the same transaction; D's rework context resolves the join's id through
    // `verification.joined` (the rejected candidate, the item, the code base) and is in its next prompt.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    let (qa1, _) = verify(
        &store,
        &path,
        (&r1, &c, "rec-1"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    assert!(joined(&path, &c)?.is_empty(), "E's round is open");
    late_row(&path, (&qa1, &c, "rec-late"), "FAIL")?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2.clone(), "passable".to_owned())]);
    let join = child_id(&c, "join/2");
    let joined = joined(&path, &c)?;
    assert_eq!(joined.len(), 1, "{joined:?}");
    assert_eq!(
        (&joined[0]["class"], &joined[0]["outcome"]),
        (&json!("failed"), &json!("rejected"))
    );
    assert_eq!(candidate_state(&path, &c)?, "REJECTED");
    assert_eq!(
        events(&path, "candidate.rejected")?,
        [json!({"candidate_id": c, "join_id": join})]
    );
    let item = "ac-fail:AC-1-01:test_001_01_late";
    assert_eq!(items(&path, &join)?, [format!("D:{item}")]);
    assert_eq!(done.reworked, ["D"]);
    assert_eq!(task_state(&path, "D")?, "READY");
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.round_id') FROM events WHERE kind = 'task.rework'"
        )?,
        std::slice::from_ref(&join)
    );
    assert_eq!(
        state_of(&path, &child_id(&c, "acceptance/AC-1-01/test_001_01_late"))?,
        "OPEN"
    );
    assert_eq!(iterations(&path, &c)?, ["repair"]);
    // E waits for D's successor (ADR-028 I7).
    assert_eq!(task_state(&path, "E")?, "VERIFYING");
    let rework = store.reader()?.rework_context("D")?.ok_or("no rework")?;
    assert_eq!(rework.round_id, join);
    assert_eq!(rework.candidate_id, c);
    assert_eq!(rework.items, [item]);
    assert_eq!(rework.git_commit, "a".repeat(40));
    let facts =
        crate::persistence::context::facts(&Connection::open(&path)?, "R", "D", "A-next", None)?;
    assert!(
        facts
            .entries
            .iter()
            .any(|e| e.text.contains(item) && e.text.contains(&c)),
        "{:?}",
        facts.entries
    );
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn an_unknown_row_published_after_its_rounds_close_makes_the_join_flaky_and_blocks_every_participant()
-> R {
    // Decision 11 (flaky): as above with a late UNKNOWN row beside AC-1-01's PASS: retesting C cannot satisfy it, so D
    // and E are BLOCKED for a decision (`need_decision{reason: flaky, candidate_id, join_id}`), C stays VERIFYING, and
    // no iteration is counted.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
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
    late_row(&path, (&qa1, &c, "rec-late"), "UNKNOWN")?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2.clone(), "passable".to_owned())]);
    let join = child_id(&c, "join/2");
    let joined = joined(&path, &c)?;
    assert_eq!(
        (&joined[0]["class"], &joined[0]["outcome"]),
        (&json!("flaky"), &json!("flaky")),
        "{joined:?}"
    );
    for task in ["D", "E"] {
        assert_eq!(task_state(&path, task)?, "BLOCKED:NEED_DECISION");
    }
    let decisions: Vec<Value> = events(&path, "need_decision")?;
    assert_eq!(decisions.len(), 2, "{decisions:?}");
    for d in &decisions {
        assert_eq!(
            (&d["reason"], &d["candidate_id"], &d["join_id"]),
            (&json!("flaky"), &json!(c), &json!(join))
        );
    }
    assert_eq!(candidate_state(&path, &c)?, "VERIFYING");
    assert!(iterations(&path, &c)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn a_join_that_sees_an_inconclusive_check_verifies_each_chain_again_counted_on_that_chain()
-> R {
    // Decisions 4, 11 and 13 (-52): D's round 1 ran `e2e`, which timed out (and its reviewer was UNKNOWN), so round 1
    // was verified again in round 3, which did not run it: round 3 passable on its own evidence, E's round 2 passable.
    // The join reads every check on C: `e2e` is inconclusive, so the join is too: each chain verified again (D's on
    // round 1's chain, its second; E's on round 2's, its first), no iteration counted. Once `e2e` is run again and
    // passes, the join is passable: C, D and E ACCEPTED, one iteration.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    let (qa1, _) = attempts_of(&path, &r1)?;
    evidence_of(&path, (&qa1, &c, "rec-e2e"), ("e2e", "FAIL"), &[], true)?;
    Connection::open(&path)?.execute(
        "UPDATE events SET payload_json = json_set(payload_json, '$.outcome_reason', 'TIMED_OUT') \
         WHERE event_id = 'rec-e2e/evidence'",
        [],
    )?;
    verify(
        &store,
        &path,
        (&r1, &c, "rec-1"),
        &["AC-1-01"],
        ("PASS", "UNKNOWN"),
    )
    .await?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    reconcile(&store).await?;
    let r3 = child_id(&c, "round/3");
    start_all(&store, &path).await?;
    verify(
        &store,
        &path,
        (&r3, &c, "rec-3"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r3.clone(), "passable".to_owned())]);
    let joined_first = joined(&path, &c)?;
    assert_eq!(joined_first.len(), 1, "{joined_first:?}");
    assert_eq!(
        (
            &joined_first[0]["class"],
            &joined_first[0]["outcome"],
            &joined_first[0]["inputs"]["checks"]
        ),
        (
            &json!("inconclusive"),
            &json!("reverify"),
            &json!("Inconclusive")
        )
    );
    // Each chain's next round, the same sources and scope.
    let opened: BTreeMap<String, Value> = events(&path, "verification.round_opened")?
        .into_iter()
        .filter(|o| {
            done.opened
                .contains(&o["round_id"].as_str().unwrap_or_default().to_owned())
        })
        .map(|o| {
            (
                o["source_task_ids"][0]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                o,
            )
        })
        .collect();
    assert_eq!(opened.len(), 2, "{:?}", done.opened);
    assert_eq!(
        (
            &opened["D"]["chain_root"],
            &opened["D"]["reverify_of"],
            &opened["D"]["acceptance_ids"]
        ),
        (&json!(r1), &json!(r3), &json!(["AC-1-01"]))
    );
    assert_eq!(
        (
            &opened["E"]["chain_root"],
            &opened["E"]["reverify_of"],
            &opened["E"]["acceptance_ids"]
        ),
        (&json!(r2), &json!(r2), &json!(["AC-2-01"]))
    );
    let counted = texts(
        &path,
        "SELECT json_extract(payload_json, '$.round_root') || ':' || json_extract(payload_json, '$.count') FROM events \
         WHERE kind = 'repair.reverify' ORDER BY 1",
    )?;
    let mut want = vec![format!("{r1}:1"), format!("{r1}:2"), format!("{r2}:1")];
    want.sort();
    assert_eq!(counted, want);
    assert!(
        iterations(&path, &c)?.is_empty(),
        "no iteration for an inconclusive join"
    );
    assert_eq!(candidate_state(&path, &c)?, "VERIFYING");
    // `e2e` run again and passing in D's next round: the join is passable.
    start_all(&store, &path).await?;
    let (rd, re) = (
        opened["D"]["round_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        opened["E"]["round_id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
    );
    let (qa_d, _) = attempts_of(&path, &rd)?;
    evidence_of(&path, (&qa_d, &c, "rec-e2e-2"), ("e2e", "PASS"), &[], true)?;
    verify(
        &store,
        &path,
        (&rd, &c, "rec-4"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    verify(
        &store,
        &path,
        (&re, &c, "rec-5"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    reconcile(&store).await?;
    let joined = joined(&path, &c)?;
    assert_eq!(joined.len(), 2, "{joined:?}");
    assert_eq!(joined[1]["outcome"], json!("accepted"), "{joined:?}");
    assert_eq!(candidate_state(&path, &c)?, "ACCEPTED");
    assert_eq!(iterations(&path, &c)?, ["passable"]);
    Ok(())
}

#[tokio::test]
async fn a_passable_join_the_gate_disagrees_with_accepts_nothing_and_waits_for_a_decision() -> R {
    // Decision 11 (J3, fail-closed): D also holds REQ-9, which has no criterion (forged: the classification never sees
    // it), so the gate refuses D: `integrity_violation{join_gate_disagrees}`, D BLOCKED for a decision
    // (`join_gate_refused`), C1 still VERIFYING, nothing ACCEPTED, no iteration; the join is not tried again.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
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
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    let join = child_id(&c1, "join/1");
    let joined = joined(&path, &c1)?;
    assert_eq!(
        (&joined[0]["class"], &joined[0]["outcome"]),
        (&json!("passable"), &json!("refused")),
        "{joined:?}"
    );
    let violations = events(&path, "integrity_violation")?;
    assert_eq!(
        violations,
        [
            json!({"candidate_id": c1, "join_id": join, "tasks": ["D"], "reason": "join_gate_disagrees"})
        ]
    );
    assert_eq!(task_state(&path, "D")?, "BLOCKED:NEED_DECISION");
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.reason') FROM events WHERE kind = 'need_decision'"
        )?,
        ["join_gate_refused"]
    );
    assert_eq!(candidate_state(&path, &c1)?, "VERIFYING");
    assert!(events(&path, "candidate.accepted")?.is_empty());
    assert!(events(&path, "task.accepted")?.is_empty());
    assert!(iterations(&path, &c1)?.is_empty());
    reconcile(&store).await?;
    assert_eq!(joined_count(&path)?, 1);
    Ok(())
}

fn joined_count(path: &Path) -> R<usize> {
    Ok(events(path, "verification.joined")?.len())
}

/// D's round 1 on C1 passable and closed while D was held off its barrier (forged BLOCKED), then put back to
/// VERIFYING: the store and C1, the barrier's join not done.
async fn closed_without_its_join(store: &Store, path: &Path) -> R<(String, String)> {
    let c1 = freeze_d(store, path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    start_all(store, path).await?;
    verify(
        store,
        path,
        (&r1, &c1, "rec-1"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    Connection::open(path)?.execute_batch(
        "UPDATE tasks SET state = 'BLOCKED', revision = revision + 1 WHERE task_id = 'D';",
    )?;
    let done = reconcile(store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    assert!(joined(path, &c1)?.is_empty());
    Connection::open(path)?.execute_batch(
        "UPDATE tasks SET state = 'VERIFYING', revision = revision + 1 WHERE task_id = 'D';",
    )?;
    Ok((c1, r1))
}

#[tokio::test]
async fn a_join_whose_candidate_is_not_verifying_writes_no_move_and_the_rest_of_its_transaction_commits()
-> R {
    // Decision 11 (-61), the seam no barrier reaches (-164): `join::join` called on C1 forged REJECTED with a passable
    // class: the ACCEPTED update changes no row, so the join writes `integrity_violation{join_candidate_not_verifying}`
    // and `verification.joined{outcome: skipped}` and nothing else (C1 REJECTED, D VERIFYING, no acceptance), and the
    // transaction's other writes commit with it.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c1, _) = closed_without_its_join(&store, &path).await?;
    Connection::open(&path)?.execute(
        "UPDATE candidates SET state = 'REJECTED' WHERE candidate_id = ?1",
        [&c1],
    )?;
    let mut conn = Connection::open(&path)?;
    let ctx = crate::persistence::writer::Ctx::quiet();
    let tx = crate::persistence::writer::begin(&mut conn, &ctx)?;
    let all = crate::persistence::closing::rounds(&tx, "R")?;
    let mut done = Reconciled::default();
    let outcome = join(&tx, "R", &c1, &all, &ctx, &mut done)?;
    tx.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'marker', \
         'R', 'test.marker', '{}', '2026-11-12T00:00:09Z')",
        [],
    )?;
    tx.commit()?;
    let join = child_id(&c1, "join/1");
    assert_eq!(
        outcome,
        Some(Joined {
            join_id: join.clone(),
            class: Class::Passable,
            outcome: "skipped",
        })
    );
    assert_eq!(
        events(&path, "integrity_violation")?,
        [json!({"candidate_id": c1, "join_id": join, "reason": "join_candidate_not_verifying"})]
    );
    assert_eq!(joined(&path, &c1)?[0]["outcome"], json!("skipped"));
    assert_eq!(candidate_state(&path, &c1)?, "REJECTED");
    assert_eq!(task_state(&path, "D")?, "VERIFYING");
    assert!(events(&path, "candidate.accepted")?.is_empty());
    assert!(events(&path, "task.accepted")?.is_empty());
    assert_eq!(events(&path, "test.marker")?.len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_barrier_m2t16_left_without_its_join_is_joined_on_the_first_tick_its_iteration_counted_once()
-> R {
    // Decision 11 (-03): a database M2-T16 left: C1's barrier event written and its iteration counted there
    // (verdict passable), C1 and D VERIFYING, no `verification.joined`. The first reconcile joins it (J9 by the joined
    // event, not the barrier's): C1 and D ACCEPTED, the iteration not counted again.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c1, r1) = closed_without_its_join(&store, &path).await?;
    {
        let mut conn = Connection::open(&path)?;
        let ctx = crate::persistence::writer::Ctx::quiet();
        let tx = crate::persistence::writer::begin(&mut conn, &ctx)?;
        tx.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
             'verification.candidate_ready_for_join', ?2, '2026-11-12T00:00:09Z')",
            params![
                child_id(&c1, "join/1"),
                json!({"candidate_id": c1, "rounds": [r1]}).to_string()
            ],
        )?;
        crate::persistence::closing::iteration(&tx, ("p1", "R"), &c1, "passable", &ctx)?;
        tx.commit()?;
    }
    let done = reconcile(&store).await?;
    assert_eq!(done.joinable, std::slice::from_ref(&c1));
    assert_eq!(joined(&path, &c1)?.len(), 1);
    assert_eq!(candidate_state(&path, &c1)?, "ACCEPTED");
    assert_eq!(task_state(&path, "D")?, "ACCEPTED");
    assert_eq!(iterations(&path, &c1)?, ["passable"]);
    assert_eq!(
        texts(&path, "SELECT CAST(count(*) AS TEXT) FROM iterations")?,
        ["1"]
    );
    assert_eq!(
        texts(
            &path,
            "SELECT CAST(count(*) AS TEXT) FROM events WHERE kind = 'verification.candidate_ready_for_join'"
        )?,
        ["1"]
    );
    Ok(())
}

#[tokio::test]
async fn the_join_waits_for_every_round_of_its_candidate_and_a_cancelled_sources_failed_round_rejects_it()
-> R {
    // Decision 11 (-252, -353): on C, D's round 1 and E's round 2; D is cancelled while round 1 runs. E's round passes:
    // no barrier and no join while round 1 is open (its verifiers settle as usual). Round 1's reviewer then reports a
    // critical finding: round 1 failed, C REJECTED, never joined, and E is sent back to be frozen anew.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    Connection::open(&path)?.execute_batch(
        "UPDATE tasks SET state = 'CANCELLED', revision = revision + 1 WHERE task_id = 'D';",
    )?;
    verify(
        &store,
        &path,
        (&r2, &c, "rec-2"),
        &["AC-2-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let (qa1, reviewer1) = attempts_of(&path, &r1)?;
    evidence_of(
        &path,
        (&qa1, &c, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    settle(&store, &path, &qa1, ("qa", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r2.clone(), "passable".to_owned())]);
    assert!(done.joinable.is_empty());
    assert!(joined(&path, &c)?.is_empty());
    assert!(
        events(&path, "verification.candidate_ready_for_join")?.is_empty(),
        "no barrier while round 1 is open"
    );
    assert_eq!(candidate_state(&path, &c)?, "VERIFYING");
    settle(
        &store,
        &path,
        &reviewer1,
        ("reviewer", "FAIL"),
        &[finding("F1", "critical", X_SUMMARY, &["src/a.rs"])],
        &[],
    )
    .await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "repair".to_owned())]);
    assert_eq!(candidate_state(&path, &c)?, "REJECTED");
    assert!(joined(&path, &c)?.is_empty());
    assert_eq!(task_state(&path, "E")?, "READY");
    assert_eq!(
        texts(
            &path,
            "SELECT json_extract(payload_json, '$.task_id') FROM events WHERE kind = 'task.superseded'"
        )?,
        ["E"]
    );
    Ok(())
}

#[tokio::test]
async fn an_issue_reopened_after_its_delivery_holds_the_join_and_the_next_round_finds_it_still_present()
-> R {
    // Decisions 10 and 11 (-556, -653): X (critical) found on C1 by D's round; on C2, D's round 2 is delivered X and
    // confirms it fixed (passable, X RESOLVED) while E's round 3 runs; round 3's reviewer reports again by X's id (a low
    // finding): X REOPENED, round 3 passable (X was not its to confirm). The join recomputes the participants' confirm
    // set now: X unresolved, so it is inconclusive (`unconfirmed`), and each chain is verified again; D's next round is
    // delivered X, finds it still present and fails on it: C2 REJECTED, X unresolved.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    start_all(&store, &path).await?;
    let (qa1, reviewer1) = attempts_of(&path, &r1)?;
    evidence_of(
        &path,
        (&qa1, &c1, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    settle(&store, &path, &qa1, ("qa", "PASS"), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer1,
        ("reviewer", "FAIL"),
        &[finding("F1", "critical", X_SUMMARY, &["src/a.rs"])],
        &[],
    )
    .await?;
    assert_eq!(
        reconcile(&store).await?.closed,
        [(r1.clone(), "repair".to_owned())]
    );
    let x = child_id(&child_id(&reviewer1, "report"), "finding/F1");
    let fingerprint = acceptance::fingerprint("critical", X_SUMMARY, &["src/a.rs".to_owned()]);
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    assert_eq!(complete_e(&store, &path, "AE", 'c').await?, c2);
    let (r2, r3) = (child_id(&c2, "round/1"), child_id(&c2, "round/2"));
    start_all(&store, &path).await?;
    let (qa2, reviewer2) = attempts_of(&path, &r2)?;
    assert_eq!(delivered_to(&path, &qa2)?, std::slice::from_ref(&x));
    evidence_of(
        &path,
        (&qa2, &c2, "rec-2"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    settle(
        &store,
        &path,
        &qa2,
        ("qa", "PASS"),
        &[],
        &[confirm(&x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(&store, &path, &reviewer2, ("reviewer", "PASS"), &[], &[]).await?;
    assert_eq!(
        reconcile(&store).await?.closed,
        [(r2.clone(), "passable".to_owned())]
    );
    assert_eq!(state_of(&path, &x)?, "RESOLVED");
    let (qa3, reviewer3) = attempts_of(&path, &r3)?;
    assert!(delivered_to(&path, &qa3)?.is_empty());
    evidence_of(
        &path,
        (&qa3, &c2, "rec-3"),
        ("unit", "PASS"),
        &[("AC-2-01", "PASS")],
        true,
    )?;
    settle(&store, &path, &qa3, ("qa", "PASS"), &[], &[]).await?;
    settle(
        &store,
        &path,
        &reviewer3,
        ("reviewer", "PASS"),
        &[finding(&x, "low", "仍有一处未检查长度", &[])],
        &[],
    )
    .await?;
    assert_eq!(state_of(&path, &x)?, "REOPENED");
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r3.clone(), "passable".to_owned())]);
    let joined = joined(&path, &c2)?;
    assert_eq!(joined.len(), 1, "{joined:?}");
    assert_eq!(
        (
            &joined[0]["class"],
            &joined[0]["outcome"],
            &joined[0]["inputs"]["unconfirmed"]
        ),
        (&json!("inconclusive"), &json!("reverify"), &json!(true))
    );
    assert_eq!(done.opened.len(), 2, "{done:?}");
    // D's next round is delivered X, confirmed fixed again, but X is still present on C2: failed on it.
    start_all(&store, &path).await?;
    let r4 = done
        .opened
        .iter()
        .find(|r| {
            texts(
                &path,
                &format!(
                    "SELECT 'D' FROM events WHERE kind = 'verification.round_opened' \
                     AND json_extract(payload_json, '$.round_id') = '{r}' \
                     AND json_extract(payload_json, '$.source_task_ids[0]') = 'D'"
                ),
            )
            .is_ok_and(|r| !r.is_empty())
        })
        .cloned()
        .ok_or("no next round for D")?;
    let (qa4, reviewer4) = attempts_of(&path, &r4)?;
    assert_eq!(delivered_to(&path, &qa4)?, std::slice::from_ref(&x));
    evidence_of(
        &path,
        (&qa4, &c2, "rec-4"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    settle(
        &store,
        &path,
        &qa4,
        ("qa", "PASS"),
        &[],
        &[confirm(&x, "RESOLVED", "已修复")],
    )
    .await?;
    settle(&store, &path, &reviewer4, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert!(
        done.closed.contains(&(r4.clone(), "repair".to_owned())),
        "{done:?}"
    );
    assert!(items(&path, &r4)?.contains(&format!("D:issue:{fingerprint}")));
    assert_eq!(closed(&path, &r4)?["inputs"]["failing_issues"], json!(true));
    assert_eq!(candidate_state(&path, &c2)?, "REJECTED");
    assert_ne!(state_of(&path, &x)?, "RESOLVED");
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn fifty_one_grave_issues_are_confirmed_in_two_batches_through_the_joins_reverification() -> R
{
    // Decisions 10 and 11 (-200, -251): 51 critical Issues of 51 fingerprints on C1; on C2 the first round is
    // delivered 50 (with their description), confirms them fixed and is passable; the join recomputes the confirm set,
    // one critical Issue unresolved: inconclusive, verified again; the next round is delivered the last one, confirms
    // it, and the join is passable: C2 and D ACCEPTED, every Issue RESOLVED.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let c1 = freeze_d(&store, &path, 0, 'b').await?;
    let r1 = child_id(&c1, "round/1");
    start_all(&store, &path).await?;
    let (qa1, reviewer1) = attempts_of(&path, &r1)?;
    evidence_of(
        &path,
        (&qa1, &c1, "rec-1"),
        ("unit", "PASS"),
        &[("AC-1-01", "PASS")],
        true,
    )?;
    settle(&store, &path, &qa1, ("qa", "PASS"), &[], &[]).await?;
    let findings: Vec<Value> = (1..=51)
        .map(|n| {
            finding(
                &format!("F{n:02}"),
                "critical",
                &format!("缺陷 {n}：边界 {n}"),
                &[],
            )
        })
        .collect();
    settle(
        &store,
        &path,
        &reviewer1,
        ("reviewer", "FAIL"),
        &findings,
        &[],
    )
    .await?;
    reconcile(&store).await?;
    let c2 = freeze_d(&store, &path, 1, 'c').await?;
    let mut round = child_id(&c2, "round/1");
    let mut confirmed = 0;
    for batch in [50_usize, 1] {
        start_all(&store, &path).await?;
        let (qa, reviewer) = attempts_of(&path, &round)?;
        let delivered = delivered_to(&path, &qa)?;
        assert_eq!(delivered.len(), batch, "{round}");
        evidence_of(
            &path,
            (&qa, &c2, &format!("rec-{round}")),
            ("unit", "PASS"),
            &[("AC-1-01", "PASS")],
            true,
        )?;
        let confirmations: Vec<Value> = delivered
            .iter()
            .map(|id| confirm(id, "RESOLVED", "已修复"))
            .collect();
        settle(&store, &path, &qa, ("qa", "PASS"), &[], &confirmations).await?;
        settle(&store, &path, &reviewer, ("reviewer", "PASS"), &[], &[]).await?;
        let done = reconcile(&store).await?;
        assert_eq!(done.closed, [(round.clone(), "passable".to_owned())]);
        confirmed += batch;
        let last = joined(&path, &c2)?.pop().ok_or("not joined")?;
        if confirmed < 51 {
            assert_eq!(
                (&last["class"], &last["inputs"]["unconfirmed"]),
                (&json!("inconclusive"), &json!(true))
            );
            round = done.opened.first().cloned().ok_or("not verified again")?;
        } else {
            assert_eq!(last["outcome"], json!("accepted"), "{last}");
        }
    }
    assert_eq!(candidate_state(&path, &c2)?, "ACCEPTED");
    assert_eq!(task_state(&path, "D")?, "ACCEPTED");
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || CAST(count(*) AS TEXT) FROM issues GROUP BY state"
        )?,
        ["RESOLVED:51"]
    );
    assert_eq!(iterations(&path, &c2)?, ["passable"]);
    agrees(&path)?;
    Ok(())
}

#[tokio::test]
async fn a_source_at_its_candidates_barrier_is_not_in_flight_and_its_run_blocks_for_the_siblings_reason()
-> R {
    // Decision 13 (ADR-028 decision 10's transition removed): D's round on C passable, E's round's QA BLOCKED_AUTH and
    // its reviewer done, and a barrier event M2-T16 left for C. Nothing waits for a join (it is the barrier's own
    // transaction): D and E are not in flight, so the run is BLOCKED for E's verifier's reason.
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let (c, r1, r2) = two_sources(&store, &path).await?;
    verify(
        &store,
        &path,
        (&r1, &c, "rec-1"),
        &["AC-1-01"],
        ("PASS", "PASS"),
    )
    .await?;
    let (qa2, reviewer2) = attempts_of(&path, &r2)?;
    Connection::open(&path)?.execute_batch(&format!(
        "UPDATE tasks SET state = 'BLOCKED', blocked_reason = 'BLOCKED_AUTH', revision = revision + 1
           WHERE task_id = '{0}';
         UPDATE attempts SET state = 'FAILED' WHERE attempt_id = '{qa2}';",
        child_id(&r2, "task/qa")
    ))?;
    settle(&store, &path, &reviewer2, ("reviewer", "PASS"), &[], &[]).await?;
    let done = reconcile(&store).await?;
    assert_eq!(done.closed, [(r1.clone(), "passable".to_owned())]);
    Connection::open(&path)?.execute(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', ?1, 'R', \
         'verification.candidate_ready_for_join', ?2, '2026-11-12T00:00:09Z')",
        params![
            child_id(&c, "join/2"),
            json!({"candidate_id": c, "rounds": [r1, r2]}).to_string()
        ],
    )?;
    store.pace_run("R", verifiers()).await?;
    assert_eq!(
        texts(
            &path,
            "SELECT state || ':' || blocked_reasons_json FROM runs WHERE run_id = 'R'"
        )?,
        ["BLOCKED:[\"BLOCKED_AUTH\"]"]
    );
    assert!(joined(&path, &c)?.is_empty());
    Ok(())
}
