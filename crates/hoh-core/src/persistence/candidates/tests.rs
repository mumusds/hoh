//! A run's candidates against a real database (docs/adr/ADR-027.md decision 5, CR-017).

use super::*;
use crate::persistence::CandidateRow;
use crate::persistence::{Options, Store, selftest};
use crate::verification::{FileRef, Manifest};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Runs R and S of project p1; R holds an ACCEPTED, a VERIFYING and a STALE candidate already.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-11T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES
  ('R', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-11T00:00:00Z'),
  ('S', 'p1', 's1', 'RUNNING', 'AUTO', 1, '2026-11-11T00:00:00Z');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256,
  source_path, state, created_at) VALUES
  ('CAND-old-accepted', 'R', printf('%040d', 1), printf('%040d', 1), printf('%064d', 1), printf('%064d', 1),
   printf('%064d', 1), 'x', 'ACCEPTED', '2026-11-11T00:00:01Z'),
  ('CAND-old-verifying', 'R', printf('%040d', 2), printf('%040d', 2), printf('%064d', 1), printf('%064d', 1),
   printf('%064d', 1), 'x', 'VERIFYING', '2026-11-11T00:00:02Z'),
  ('CAND-old-stale', 'R', printf('%040d', 3), printf('%040d', 3), printf('%064d', 1), printf('%064d', 1),
   printf('%064d', 1), 'x', 'STALE', '2026-11-11T00:00:00Z');
";

fn open(dir: &Path) -> R<(Store, PathBuf)> {
    let path = dir.join(".hoh/state.db");
    let options = Options {
        wal_probe: Some(selftest::test_probe(None)),
        ..Options::default()
    };
    let store = Store::open(&path, options)?;
    Connection::open(&path)?.execute_batch(WORLD)?;
    Ok((store, path))
}

fn texts(path: &Path, sql: &str) -> R<Vec<String>> {
    let conn = Connection::open(path)?;
    let mut statement = conn.prepare(sql)?;
    let rows = statement
        .query_map([], |r| r.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

fn object(n: u8, mime: &str, kind: &str, name: &str) -> ArtifactRow {
    let sha256: String = format!("{n:x}").repeat(64).chars().take(64).collect();
    ArtifactRow {
        path: format!(".hoh/objects/sha256/{}/{sha256}", &sha256[..2]),
        sha256,
        bytes: 10 + u64::from(n),
        mime: mime.to_owned(),
        kind: kind.to_owned(),
        name: name.to_owned(),
    }
}

/// A freeze for `run`: spec, environment and test plan kept as objects 1, 2 and 3, the export as object 9.
fn freeze_of(run: &str, tree: char) -> Freeze {
    let file = |path: &str, n: u8| FileRef {
        path: path.to_owned(),
        sha256: object(n, "", "", "").sha256,
    };
    let sealed = Manifest {
        git_commit: "a".repeat(40),
        git_tree: tree.to_string().repeat(40),
        spec: file("contracts/requirements.json", 1),
        environment: file(".hoh/environment.json", 2),
        test_plan: file("tests/check-plan.json", 3),
        source_files: vec![file("src/lib.rs", 4)],
    }
    .seal();
    Freeze {
        run_id: run.to_owned(),
        sealed,
        export: object(9, "application/json", "candidate", "candidate.json"),
        inputs: vec![
            object(1, "application/json", "spec", "contracts/requirements.json"),
            object(
                2,
                "application/json",
                "environment",
                ".hoh/environment.json",
            ),
            object(3, "application/json", "test_plan", "tests/check-plan.json"),
        ],
    }
}

#[tokio::test]
async fn a_new_candidate_is_one_frozen_row_that_stales_its_runs_current_ones_and_is_recorded() -> R
{
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of("R", 'b');
    let id = job.sealed.candidate_id.clone();
    let frozen = store.freeze_candidate(job.clone()).await?;
    assert_eq!(
        frozen,
        Frozen::New {
            candidate_id: id.clone(),
            superseded: vec![
                "CAND-old-accepted".to_owned(),
                "CAND-old-verifying".to_owned()
            ],
        }
    );
    let m = &job.sealed.manifest;
    assert_eq!(
        texts(
            &path,
            &format!(
                "SELECT json_array(run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256, \
                 source_path, state) FROM candidates WHERE candidate_id = '{id}'"
            )
        )?,
        vec![
            json!([
                "R",
                m.git_commit,
                m.git_tree,
                m.spec.sha256,
                m.environment.sha256,
                m.test_plan.sha256,
                job.export.path,
                "FROZEN"
            ])
            .to_string()
        ]
    );
    // The run's earlier current candidates are STALE; the STALE one stays as it was; the other run is untouched.
    assert_eq!(
        texts(
            &path,
            "SELECT candidate_id || ':' || state FROM candidates WHERE candidate_id LIKE 'CAND-old-%' ORDER BY 1"
        )?,
        [
            "CAND-old-accepted:STALE",
            "CAND-old-stale:STALE",
            "CAND-old-verifying:STALE"
        ]
    );
    // Every reader of the current candidate sees the new one.
    let conn = Connection::open(&path)?;
    let current = crate::persistence::outbox::current_candidate(&conn, "R")?.ok_or("no current")?;
    assert_eq!(current.id, id);
    // The export and the three inputs are registered objects (decision 4).
    let mut registered = texts(&path, "SELECT path FROM artifacts ORDER BY path")?;
    let mut expected: Vec<String> = std::iter::once(&job.export)
        .chain(&job.inputs)
        .map(|a| a.path.clone())
        .collect();
    registered.sort();
    expected.sort();
    assert_eq!(registered, expected);
    // One candidate.frozen event naming it and what it superseded.
    let events = texts(
        &path,
        "SELECT json_array(event_id, run_id, kind, json(payload_json)) FROM events WHERE kind = 'candidate.frozen'",
    )?;
    assert_eq!(events.len(), 1);
    let event: Value = serde_json::from_str(&events[0])?;
    assert_eq!(event[1], "R");
    assert_eq!(
        event[3],
        json!({"candidate_id": id, "superseded": ["CAND-old-accepted", "CAND-old-verifying"],
               "source_path": job.export.path})
    );
    Ok(())
}

#[tokio::test]
async fn the_same_content_again_in_its_run_returns_its_id_and_state_and_revives_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of("R", 'b');
    let id = job.sealed.candidate_id.clone();
    store.freeze_candidate(job.clone()).await?;
    let count = |path: &Path, sql: &str| -> R<usize> { Ok(texts(path, sql)?.len()) };
    let before = count(&path, "SELECT event_id FROM events")?;
    assert_eq!(
        store.freeze_candidate(job.clone()).await?,
        Frozen::Existing {
            candidate_id: id.clone(),
            state: "FROZEN".to_owned()
        }
    );
    // A later candidate makes it STALE; freezing its content again returns it STALE, not FROZEN again.
    store.freeze_candidate(freeze_of("R", 'c')).await?;
    assert_eq!(
        store.freeze_candidate(job.clone()).await?,
        Frozen::Existing {
            candidate_id: id.clone(),
            state: "STALE".to_owned()
        }
    );
    assert_eq!(
        count(
            &path,
            &format!("SELECT candidate_id FROM candidates WHERE candidate_id = '{id}'")
        )?,
        1
    );
    // Only the second candidate's freeze was written meanwhile.
    assert_eq!(count(&path, "SELECT event_id FROM events")?, before + 1);
    Ok(())
}

#[tokio::test]
async fn the_same_content_in_another_run_is_refused_with_the_way_out() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of("R", 'b');
    store.freeze_candidate(job.clone()).await?;
    let before = texts(
        &path,
        "SELECT candidate_id || state || run_id FROM candidates ORDER BY 1",
    )?;
    let mut other = job.clone();
    other.run_id = "S".to_owned();
    let refused = store.freeze_candidate(other).await;
    let Err(e) = refused else {
        return Err(format!("frozen for another run: {refused:?}").into());
    };
    assert!(
        matches!(&e, PersistError::CandidateOfAnotherRun { candidate_id, run_id }
            if *candidate_id == job.sealed.candidate_id && run_id == "R"),
        "{e:?}"
    );
    assert!(e.to_string().contains("new commit"), "{e}");
    assert_eq!(
        texts(
            &path,
            "SELECT candidate_id || state || run_id FROM candidates ORDER BY 1"
        )?,
        before
    );
    Ok(())
}

#[tokio::test]
async fn a_freeze_without_its_inputs_kept_or_for_no_run_writes_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let mut job = freeze_of("R", 'b');
    job.inputs.retain(|a| a.kind != "environment");
    let refused = store.freeze_candidate(job).await;
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    let missing = store.freeze_candidate(freeze_of("NO-RUN", 'b')).await;
    assert!(
        matches!(missing, Err(PersistError::RunNotFound(_))),
        "{missing:?}"
    );
    assert!(texts(&path, "SELECT path FROM artifacts")?.is_empty());
    assert_eq!(
        texts(
            &path,
            "SELECT candidate_id FROM candidates WHERE state = 'FROZEN'"
        )?,
        Vec::<String>::new()
    );
    Ok(())
}

#[tokio::test]
async fn a_reader_gives_a_candidates_row_and_an_attempts_run_and_role() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    let job = freeze_of("R", 'b');
    store.freeze_candidate(job.clone()).await?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES
           ('Q', 'R', 's1', 'RUNNING', 'qa', 1), ('D', 'S', 's1', 'RUNNING', 'developer', 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
           ('Q/attempt-1', 'Q', 'RUNNING', 1, 1, 1), ('D/attempt-1', 'D', 'RUNNING', 1, 1, 1);",
    )?;
    let reader = store.reader()?;
    let m = &job.sealed.manifest;
    assert_eq!(
        reader.candidate(&job.sealed.candidate_id)?,
        Some(CandidateRow {
            candidate_id: job.sealed.candidate_id.clone(),
            run_id: "R".to_owned(),
            state: "FROZEN".to_owned(),
            source_path: job.export.path.clone(),
            git_commit: m.git_commit.clone(),
            git_tree: m.git_tree.clone(),
            spec_sha256: m.spec.sha256.clone(),
            environment_sha256: m.environment.sha256.clone(),
            test_plan_sha256: m.test_plan.sha256.clone(),
        })
    );
    assert_eq!(reader.candidate("CAND-none")?, None);
    let producer = |run: &str, role: &str| crate::verification::Producer {
        run_id: run.to_owned(),
        role: role.to_owned(),
    };
    assert_eq!(reader.producer("Q/attempt-1")?, Some(producer("R", "qa")));
    assert_eq!(
        reader.producer("D/attempt-1")?,
        Some(producer("S", "developer"))
    );
    assert_eq!(reader.producer("nobody")?, None);
    Ok(())
}

#[tokio::test]
async fn a_reader_gives_a_runs_spec_digest_and_its_acceptance_criteria() -> R {
    let dir = tempfile::tempdir()?;
    let (store, path) = open(dir.path())?;
    Connection::open(&path)?.execute_batch(
        "INSERT INTO requirements(requirement_id, spec_id, title, required_release, release_class) VALUES
           ('REQ-001', 's1', 'one', 1, 'required'), ('REQ-048', 's1', 'plugins', 1, 'degradable');
         INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
           ('AC-001-01', 'REQ-001', 's1', 'a', 'unit'), ('AC-048-01', 'REQ-048', 's1', 'b', 'plugin-live');",
    )?;
    let got = store.reader()?.acceptance("R")?.ok_or("no run")?;
    assert_eq!(got.spec_id, "s1");
    assert_eq!(got.spec_sha256, format!("{:064}", 1));
    assert_eq!(
        got.criteria,
        vec![
            ("AC-001-01".to_owned(), "unit".to_owned(), false),
            ("AC-048-01".to_owned(), "plugin-live".to_owned(), true)
        ]
    );
    assert_eq!(store.reader()?.acceptance("NO-RUN")?, None);
    Ok(())
}
