//! Decision sources against a real database (REQ-030 behaviour 2 and 5, docs/adr/ADR-035.md decisions 2 and 9).

use super::*;
use crate::persistence::completion::tests::R;
use crate::persistence::{Options, Store, selftest};

/// Project p1 with run R: question R/a1/d0 (the Commander's own id q-scope) answered by an AnswerQuestion, question
/// R/a1/d1 asked but never answered, input in-1 of R, an applied record_assumption ev-assume and an applied
/// transition_run ev-move; project p2 with run R2, its answered question R2/a/d0, its input in-2 and its assumption
/// ev-other.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES
  ('p1', '/repo', 1, '2026-11-22T00:00:00Z'), ('p2', '/other', 1, '2026-11-22T00:00:00Z');
INSERT INTO runs(run_id, project_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 'RUNNING', 'AUTO', 1, '2026-11-22T00:00:00Z'), ('R2', 'p2', 'RUNNING', 'AUTO', 1, '2026-11-22T00:00:00Z');
INSERT INTO commands(command_id, run_id, idempotency_key, kind, payload_json, payload_sha256, expected_revision,
  accepted_at) VALUES
  ('c-ans', 'R', 'k1', 'AnswerQuestion', '{}', printf('%.64c', 'a'), 1, '2026-11-22T00:00:00Z'),
  ('c-ans2', 'R2', 'k2', 'AnswerQuestion', '{}', printf('%.64c', 'b'), 1, '2026-11-22T00:00:00Z');
INSERT INTO input_queue(input_id, run_id, kind, state, payload_json) VALUES
  ('in-1', 'R', 'STEER', 'APPLIED', '{}'), ('in-2', 'R2', 'STEER', 'APPLIED', '{}');
INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES
  ('p1', 'ev-asked', 'R', 'question.asked', json_object('question_id', 'R/a1/d0', 'commander_question_id', 'q-scope'),
   '2026-11-22T00:00:00Z'),
  ('p1', 'ev-answered', 'R', 'question.answered', json_object('question_id', 'R/a1/d0', 'command_id', 'c-ans'),
   '2026-11-22T00:00:00Z'),
  ('p1', 'ev-assume', 'R', 'decision.applied', json_object('kind', 'record_assumption'), '2026-11-22T00:00:00Z'),
  ('p1', 'ev-move', 'R', 'decision.applied', json_object('kind', 'transition_run'), '2026-11-22T00:00:00Z'),
  ('p2', 'ev-answered2', 'R2', 'question.answered', json_object('question_id', 'R2/a/d0', 'command_id', 'c-ans2'),
   '2026-11-22T00:00:00Z'),
  ('p2', 'ev-other', 'R2', 'decision.applied', json_object('kind', 'record_assumption'), '2026-11-22T00:00:00Z');
";

fn decision(kind: &str, id: &str, line: usize) -> Decision {
    Decision {
        text: format!("决策 {line}"),
        source_kind: kind.to_owned(),
        source_id: id.to_owned(),
        path: "docs/spec/a.md".to_owned(),
        line,
    }
}

/// TEST-030-03's store half: each kind of source is found only as the project holds it; every unsourced entry is
/// listed with its position, text, source and what is missing.
#[test]
fn test_030_03_unsourced_decisions_are_each_listed() -> R {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let _store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    let conn = Connection::open(&path)?;
    conn.execute_batch(WORLD)?;
    let sourced = [
        decision("question_id", "R/a1/d0", 8),
        decision("input_id", "in-1", 10),
        decision("event_id", "ev-assume", 12),
    ];
    assert!(check(&conn, "p1", &sourced).is_ok());
    let unsourced = [
        // The Commander's own question id, a question never answered, another project's answer.
        decision("question_id", "q-scope", 8),
        decision("question_id", "R/a1/d1", 10),
        decision("question_id", "R2/a/d0", 12),
        // Another project's input, an applied decision that is not an assumption, another project's assumption.
        decision("input_id", "in-2", 14),
        decision("event_id", "ev-move", 16),
        decision("event_id", "ev-other", 18),
    ];
    let mut all = sourced.to_vec();
    all.extend(unsourced.iter().cloned());
    let Err(PersistError::SpecUnsourced(details)) = check(&conn, "p1", &all) else {
        return Err("unsourced decisions accepted".into());
    };
    let listed = details["unsourced"].as_array().ok_or("no list")?;
    assert_eq!(
        listed
            .iter()
            .map(|e| format!("{}:{}", e["line"], e["missing"].as_str().unwrap_or("?")))
            .collect::<Vec<_>>(),
        [
            "8:question.answered",
            "10:question.answered",
            "12:question.answered",
            "14:input",
            "16:record_assumption",
            "18:record_assumption"
        ]
    );
    assert_eq!(
        listed[0],
        json!({"path": "docs/spec/a.md", "line": 8, "text": "决策 8",
               "source": {"kind": "question_id", "id": "q-scope"}, "missing": "question.answered"})
    );
    assert_eq!(
        PersistError::SpecUnsourced(details).error_code(),
        Some(crate::protocol::ErrorCode::SpecInvalid)
    );
    Ok(())
}
