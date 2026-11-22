use super::*;
use crate::origin::Origin;
use crate::persistence::{CommandOutcome, Options, RunTarget, Store, selftest};
use crate::protocol::SpecDecision;

type R = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn test_008_05_store_origin_guard_precedes_replay_conflict_and_parse() -> R {
    let dir = tempfile::Builder::new().prefix("m3t04-").tempdir()?;
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(selftest::test_probe(None)),
            ..Options::new(crate::spawn::Direct::shared())
        },
    )?;
    store
        .ensure_project("P", dir.path().to_str().ok_or("path")?)
        .await?;
    let db = rusqlite::Connection::open(&path)?;
    // A previously accepted user's exact receipt. The request below has intentionally invalid files and a stale
    // revision: authority must be checked even when replay would otherwise return before those validations.
    db.execute_batch("INSERT INTO runs(run_id,project_id,state,mode,revision,created_at) VALUES('R','P','CREATED','PLAN',9,'2026-11-22T00:00:00Z');
        INSERT INTO commands(command_id,idempotency_key,kind,run_id,expected_revision,payload_json,payload_sha256,accepted_at,result_json)
        VALUES('user-command','user-key','ApproveSpec','R',0,'{}',printf('%064d',1),'2026-11-22T00:00:00Z','{\"status\":\"ACCEPTED\"}');")?;
    let before = db.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?;
    let command = |origin, decision, key: &str| SubmitCommand {
        command_id: "new-command".into(),
        idempotency_key: key.into(),
        kind: "ApproveSpec".into(),
        run_id: "R".into(),
        expected_revision: 0,
        payload: json!({}),
        payload_sha256: format!("{:064}", 1),
        target: RunTarget::Specification(Box::new(SpecRequest {
            origin,
            action: SpecAction::Approve(ApproveSpec {
                spec_sha256: "0".repeat(64),
                decision,
                note: None,
            }),
            files: Ok(SpecFiles {
                sources: vec![],
                existing_index: None,
                tracked_index: None,
            }),
        })),
        event: NewEvent {
            intent: crate::persistence::Intent::Progress,
            project_id: "P".into(),
            event_id: "event".into(),
            run_id: Some("R".into()),
            kind: "spec.changed".into(),
            payload: json!({}),
        },
        outbox: None,
    };
    for origin in [
        Origin::Unknown,
        Origin::Marker,
        Origin::AncestorMarker,
        Origin::AttemptCgroup,
    ] {
        for decision in [
            SpecDecision::ApproveOnly,
            SpecDecision::ApproveAndBuild,
            SpecDecision::Reject,
        ] {
            for key in ["fresh", "user-key"] {
                let outcome = store.submit_command(command(origin, decision, key)).await;
                assert!(
                    matches!(outcome, Err(PersistError::SpecApprovalNotUser(reason)) if Some(reason) == origin.refusal()),
                    "{origin:?}, {decision:?}, {key}: {outcome:?}"
                );
                assert_eq!(
                    db.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?,
                    before,
                    "zero writes"
                );
            }
        }
    }
    assert!(
        matches!(store.submit_command(command(Origin::User, SpecDecision::ApproveOnly, "user-key")).await?, CommandOutcome::Replayed { result_json } if result_json == "{\"status\":\"ACCEPTED\"}")
    );
    assert_eq!(
        db.query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))?,
        before
    );
    drop(store);
    Ok(())
}
