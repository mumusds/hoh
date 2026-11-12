//! A verifier attempt's run directory in the host (docs/adr/ADR-028.md decisions 6 and 8): prepared before its process
//! starts, looked at again on every end path (the runner's own end and a reconcile) before the attempt is settled, and
//! removed once it is.

use crate::verify::rundir::{self, Unprepared};
use hoh_core::persistence::{Commit, Ending, NewEvent, PersistError, Store};
use hoh_core::retry::FailureClass;
use hoh_core::verification::Sealed;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A verifier attempt's run directory as it was prepared.
#[derive(Debug, Clone)]
pub(super) struct Verifier {
    pub dir: PathBuf,
    pub sealed: Sealed,
    pub pre_tree: Value,
    pub candidate_id: String,
    /// The check its attempt-level record names (`independent-qa` or `independent-review`).
    pub check_id: &'static str,
}

impl Verifier {
    /// What its RUNNING commit records as `verification.attempt_started` (M2T16D-358).
    pub fn started(&self) -> Value {
        json!({"run_dir": self.dir.to_string_lossy(), "pre_tree": self.pre_tree, "candidate_id": self.candidate_id,
            "check_id": self.check_id})
    }
}

/// Prepare attempt `attempt_id`'s run directory under `runs` for the candidate its handoff names, by role `role`, from
/// the project at `project` (decision 6). Refused (the checkout is not the candidate), the attempt is to fail for a
/// decision; not done (git, a file), it is to fail transiently; either way nothing starts and the directory is gone.
pub(super) async fn prepare(
    store: &Store,
    project: &Path,
    runs: &Path,
    (project_id, attempt_id, role): (&str, &str, &str),
    candidate_id: Option<&str>,
) -> Result<Result<Verifier, (FailureClass, String)>, PersistError> {
    let Some(candidate_id) = candidate_id.map(str::to_owned) else {
        return Ok(Err((
            FailureClass::Decision,
            "RUN_DIR_REFUSED: its handoff names no candidate".to_owned(),
        )));
    };
    let dir = rundir::dir_of(runs, project_id, attempt_id);
    let reader = store.reader()?;
    let (project, held) = (project.to_owned(), dir.clone());
    let candidate = candidate_id.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        // Only a candidate under verification is verified (decision 10, M2T16D-253): one that no longer is ends its
        // attempt for the reconcile to cancel its task, never for a decision (M2T16R-55).
        let state = reader
            .candidate(&candidate)
            .map_err(|e| Unprepared::Io(e.to_string()))?
            .map(|row| row.state);
        if state.as_deref() != Some("VERIFYING") {
            return Ok(Err(format!(
                "candidate {candidate} is {}, not VERIFYING",
                state.as_deref().unwrap_or("not frozen here")
            )));
        }
        let (sealed, _) = rundir::sealed_of(&reader, &project, &candidate)
            .map_err(|e| Unprepared::Refused(vec![e]))?;
        let pre_tree = rundir::prepare(&project, &held, &sealed)?;
        Ok::<_, Unprepared>(Ok((sealed, pre_tree)))
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the run directory's preparation ended: {e}")))?;
    let prepared = match prepared {
        Ok(Err(not_verifying)) => {
            return Ok(Err((
                FailureClass::Transient { retry_after: None },
                format!("RUN_DIR_REFUSED: {not_verifying}"),
            )));
        }
        Ok(Ok(ready)) => Ok(ready),
        Err(e) => Err(e),
    };
    Ok(match prepared {
        Ok((sealed, pre_tree)) => Ok(Verifier {
            dir,
            sealed,
            pre_tree,
            candidate_id,
            check_id: rundir::check_of(role),
        }),
        Err(e) => {
            rundir::remove_off_thread(dir.clone()).await;
            let class = match e {
                Unprepared::Refused(_) => FailureClass::Decision,
                Unprepared::Io(_) => FailureClass::Transient { retry_after: None },
            };
            Err((class, format!("RUN_DIR_REFUSED: {e}")))
        }
    })
}

/// A failure of attempt-ending `ending`'s shape for a run directory not prepared.
pub(super) fn unprepared(mut ending: Ending, (class, detail): (FailureClass, String)) -> Ending {
    ending.class = class;
    ending.detail = detail;
    ending
}

/// The verifier's run directory looked at again before its attempt `attempt_id` is settled (decision 8): the
/// `verification.tree_checked` event (key, kind, payload) for the settle's commit; a directory found changed, or one
/// that cannot be read, is first recorded by its attempt-level marker record (INVALID evidence, the candidate INVALID
/// with integrity_violation). A record that fails is left to the record's recovery protocol (its pending facts).
pub(super) async fn check(
    store: &Store,
    project: &Path,
    attempt_id: &str,
    v: &Verifier,
) -> (String, String, Value) {
    let (dir, pre_tree, manifest) = (v.dir.clone(), v.pre_tree.clone(), v.sealed.manifest.clone());
    let checked = tokio::task::spawn_blocking(move || rundir::post(&dir, &pre_tree, &manifest))
        .await
        .unwrap_or_else(|e| rundir::TreeCheck {
            post_tree: json!({"error": e.to_string()}),
            equal: false,
            status: String::new(),
        });
    if !checked.equal {
        let change = crate::verify::check::AttemptChange {
            candidate_id: v.candidate_id.clone(),
            attempt_id: attempt_id.to_owned(),
            check_id: v.check_id.to_owned(),
            tree_before: v.pre_tree.clone(),
            tree_after: checked.post_tree.clone(),
            reasons: vec![format!(
                "the verifier's run directory changed or could not be read: {}",
                checked.status.trim()
            )],
        };
        if let Err(e) =
            crate::verify::check::record_attempt_change(store, &project.join(".hoh"), &change).await
        {
            use std::io::Write as _;
            let _ = writeln!(
                std::io::stderr(),
                "hoh host: the attempt-level record of {attempt_id}: {}",
                crate::verify::check::described(&e)
            );
        }
    }
    (
        "verification/tree_checked".to_owned(),
        "verification.tree_checked".to_owned(),
        json!({"attempt_id": attempt_id, "candidate_id": v.candidate_id, "pre_tree": v.pre_tree,
            "post_tree": checked.post_tree, "equal": checked.equal, "status": checked.status}),
    )
}

/// Before a reconcile ends verifier attempt `attempt_id` of run `run_id` (decision 8's reconcile path): its run
/// directory, as its RUNNING commit recorded it, looked at again and its `verification.tree_checked` committed (once);
/// the directory, to remove after the end. None for an attempt that is no verifier's or never started one.
pub(super) async fn check_at_reconcile(
    store: &Store,
    project: &Path,
    (project_id, run_id, attempt_id): (&str, &str, &str),
) -> Result<Option<PathBuf>, PersistError> {
    let reader = store.reader()?;
    let id = attempt_id.to_owned();
    let read = tokio::task::spawn_blocking(move || {
        Ok::<_, PersistError>((
            reader.attempt_event("verification.attempt_started", &id)?,
            reader.attempt_event("verification.tree_checked", &id)?,
        ))
    })
    .await
    .map_err(|e| PersistError::Storage(format!("the reconcile's look-up ended: {e}")))??;
    let (Some(started), checked) = read else {
        return Ok(None);
    };
    let dir = PathBuf::from(started["run_dir"].as_str().unwrap_or_default());
    if checked.is_some() {
        return Ok(Some(dir));
    }
    let candidate_id = started["candidate_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let reader = store.reader()?;
    let (held, candidate) = (project.to_owned(), candidate_id.clone());
    let sealed = tokio::task::spawn_blocking(move || rundir::sealed_of(&reader, &held, &candidate))
        .await
        .map_err(|e| PersistError::Storage(format!("the reconcile's look-up ended: {e}")))?;
    let Ok((sealed, _)) = sealed else {
        return Ok(Some(dir));
    };
    let check_id = rundir::check_of(if started["check_id"] == json!("independent-review") {
        "reviewer"
    } else {
        "qa"
    });
    let v = Verifier {
        dir: dir.clone(),
        sealed,
        pre_tree: started["pre_tree"].clone(),
        candidate_id,
        check_id,
    };
    let (key, kind, payload) = check(store, project, attempt_id, &v).await;
    store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: project_id.to_owned(),
                event_id: hoh_core::ids::child_id(attempt_id, &key),
                run_id: Some(run_id.to_owned()),
                kind,
                payload,
            }],
            ..Commit::default()
        })
        .await?;
    Ok(Some(dir))
}

/// At the host's start (docs/adr/ADR-028.md decision 16): the run directories of project `project_id` under `runs`
/// and the request directories in the project at `project` that no attempt of `live` uses, removed. Blocking.
pub(super) fn remove_unused(project: &Path, runs: &Path, project_id: &str, live: &[String]) {
    // A live attempt's, and one a check still finishing reads (its attempt ended by a cancel, M2T16R-150).
    let mut kept: std::collections::BTreeSet<PathBuf> = live
        .iter()
        .map(|a| rundir::dir_of(runs, project_id, a))
        .collect();
    kept.extend(crate::verify::check::checkouts_in_flight());
    if let Some(own) = rundir::dir_of(runs, project_id, "-").parent() {
        for entry in std::fs::read_dir(own).into_iter().flatten().flatten() {
            if !kept.contains(&entry.path()) {
                rundir::remove(&entry.path());
            }
        }
    }
    let used: std::collections::BTreeSet<PathBuf> = live
        .iter()
        .filter_map(|a| {
            super::product::bridge_of(project, a)
                .parent()
                .map(|d| d.join("requests"))
        })
        .collect();
    for entry in std::fs::read_dir(project.join(".hoh/attempts"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let requests = entry.path().join("requests");
        if !used.contains(&requests)
            && std::fs::symlink_metadata(&requests).is_ok_and(|m| m.is_dir())
        {
            let _ = std::fs::remove_dir_all(&requests);
        }
    }
}

#[cfg(test)]
mod in_flight_tests {
    use super::*;

    #[test]
    fn a_run_directory_whose_check_is_still_finishing_is_kept_until_it_ends() {
        // M2T16R-150 (docs/adr/ADR-028.md decision 8, -158): an attempt ended (a cancel), but the check it started is
        // still finishing in its run directory (its post-check reads the checkout): the per-tick removal keeps the
        // directory until that check's finishing ends, then removes it.
        let dir = tempfile::tempdir().expect("temp dir");
        let (project, runs) = (dir.path().join("p"), dir.path().join("runs"));
        let run = rundir::dir_of(&runs, "p1", "R/round/1/task/qa/attempt-1");
        std::fs::create_dir_all(run.join("src")).expect("run dir");
        std::fs::create_dir_all(project.join(".hoh/attempts")).expect("attempts");
        let finishing = crate::verify::check::hold_checkout(&run);
        remove_unused(&project, &runs, "p1", &[]);
        assert!(run.exists(), "kept while its check finishes");
        drop(finishing);
        remove_unused(&project, &runs, "p1", &[]);
        assert!(!run.exists(), "removed once it ended");
    }
}
