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

async fn approval_world() -> Result<
    (
        tempfile::TempDir,
        Store,
        rusqlite::Connection,
        SubmitCommand,
    ),
    Box<dyn std::error::Error>,
> {
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
    let sources = vec![Source {path:"docs/spec/main.md".into(),bytes:"# 用户\n用户\n# 范围\n范围\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-A 标题\n### AC-A-01\ngiven: 前提\nwhen: 操作\nthen: 结果\ncheck: unit\n".as_bytes().to_vec()}];
    let index = specindex::generate_index(&sources)?;
    let db = rusqlite::Connection::open(&path)?;
    db.execute_batch("INSERT INTO runs(run_id,project_id,state,resume_state,blocked_reason,blocked_reasons_json,mode,revision,created_at) VALUES('R','P','BLOCKED','SPECIFYING','NEED_DECISION','[\"NEED_DECISION\"]','PLAN',5,'2026-11-22T00:00:00Z');
        INSERT INTO tasks(task_id,run_id,spec_id,kind,role,writer,state,created_seq) VALUES('D','R',NULL,'spec_draft','cto',1,'IMPLEMENTED',1);
        INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES('P','R','claim','run.blocker_claimed','{\"source\":\"runtime:spec_approval\",\"reason\":\"NEED_DECISION\"}','2026-11-22T00:00:00Z');")?;
    db.execute("INSERT INTO events(project_id,run_id,event_id,kind,payload_json,created_at) VALUES('P','R','Q','question.asked',?1,'2026-11-22T00:00:00Z')",[json!({"reason":"spec_approval","question_id":"Q","task_id":"D","attempt_id":"A","spec_sha256":index.sha256(),"git_commit":"a".repeat(40),"base_spec_id":null,"text":"请批准"}).to_string()])?;
    let command = SubmitCommand {
        command_id: "approve".into(),
        idempotency_key: "approve".into(),
        kind: "ApproveSpec".into(),
        run_id: "R".into(),
        expected_revision: 5,
        payload: json!({}),
        payload_sha256: "1".repeat(64),
        target: RunTarget::Specification(Box::new(SpecRequest {
            origin: Origin::User,
            action: SpecAction::Approve(ApproveSpec {
                spec_sha256: index.sha256(),
                decision: SpecDecision::ApproveOnly,
                note: None,
            }),
            files: Ok(SpecFiles {
                sources,
                existing_index: Some(index.bytes().into()),
                tracked_index: Some(index.bytes().into()),
            }),
        })),
        event: NewEvent {
            intent: crate::persistence::Intent::Progress,
            project_id: "P".into(),
            run_id: Some("R".into()),
            event_id: "approved-event".into(),
            kind: "spec.changed".into(),
            payload: json!({}),
        },
        outbox: None,
    };
    Ok((dir, store, db, command))
}

#[tokio::test]
async fn test_008_02_direct_approval_finishes_draft_without_candidate() -> R {
    let (_dir, store, db, command) = approval_world().await?;
    store.submit_command(command).await?;
    assert_eq!(
        db.query_row("SELECT state FROM tasks WHERE task_id='D'", [], |r| r
            .get::<_, String>(0))?,
        "ACCEPTED"
    );
    assert_eq!(
        db.query_row("SELECT state FROM runs WHERE run_id='R'", [], |r| r
            .get::<_, String>(0))?,
        "SPECIFYING"
    );
    assert_eq!(
        db.query_row("SELECT revision FROM runs WHERE run_id='R'", [], |r| r
            .get::<_, i64>(0))?,
        6
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM candidates", [], |r| r
            .get::<_, i64>(0))?,
        0
    );
    assert!(
        db.query_row("SELECT spec_id FROM tasks WHERE task_id='D'", [], |r| {
            r.get::<_, Option<String>>(0)
        })?
        .is_none()
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM specs WHERE revision=1", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}

fn input_approval(
    mut command: SubmitCommand,
    answer: bool,
    origin: Origin,
    text: &str,
) -> SubmitCommand {
    let RunTarget::Specification(request) = command.target else {
        panic!("fixture")
    };
    command.kind = if answer {
        "AnswerQuestion"
    } else {
        "SendInput"
    }
    .into();
    command.target = RunTarget::ObservedInput {
        input: if answer {
            super::super::Input::Answer {
                question_id: "Q".into(),
                choice_id: None,
                answer: text.into(),
            }
        } else {
            super::super::Input::Send {
                text: text.into(),
                target_role: None,
                input_kind: "REPLY".into(),
            }
        },
        origin,
        files: Box::new(request.files),
    };
    command
}
#[tokio::test]
async fn test_008_05_all_answer_entrances_guard_before_replay_and_rollback_conflict() -> R {
    for answer in [true, false] {
        let (_dir, store, db, command) = approval_world().await?;
        let before = db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?;
        for origin in [
            Origin::Unknown,
            Origin::Marker,
            Origin::AncestorMarker,
            Origin::AttemptCgroup,
        ] {
            let rejected = store
                .submit_command(input_approval(command.clone(), answer, origin, "approve"))
                .await;
            assert!(
                matches!(rejected, Err(PersistError::SpecApprovalNotUser(_))),
                "{rejected:?}"
            );
            assert_eq!(
                db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?,
                before
            );
        }
        let mut wrong = input_approval(command.clone(), answer, Origin::User, "approve");
        if let RunTarget::ObservedInput { files, .. } = &mut wrong.target {
            files.as_mut().as_mut().expect("files").sources[0]
                .bytes
                .push(b'\n');
        }
        assert!(matches!(
            store.submit_command(wrong).await,
            Err(PersistError::SpecConflict(_))
        ));
        assert_eq!(
            db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?,
            before,
            "conflict writes neither a receipt nor a T09 rejection"
        );
        let input = input_approval(command.clone(), answer, Origin::User, "approve");
        let accepted = store.submit_command(input.clone()).await?;
        assert!(matches!(
            accepted,
            CommandOutcome::Accepted { revision: 6, .. }
        ));
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM commands WHERE kind='ApproveSpec'",
                [],
                |r| r.get::<_, i64>(0)
            )?,
            1
        );
        assert!(matches!(
            store.submit_command(input).await?,
            CommandOutcome::Replayed { .. }
        ));
        let before = db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?;
        assert!(matches!(
            store
                .submit_command(input_approval(command, answer, Origin::Marker, "approve"))
                .await,
            Err(PersistError::SpecApprovalNotUser(_))
        ));
        assert_eq!(
            db.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))?,
            before
        );
    }
    Ok(())
}
#[tokio::test]
async fn test_008_02_reject_reuses_task_with_note_and_approve_and_build_sets_mode() -> R {
    for decision in [SpecDecision::Reject, SpecDecision::ApproveAndBuild] {
        let (_dir, store, db, mut command) = approval_world().await?;
        if let RunTarget::Specification(request) = &mut command.target
            && let SpecAction::Approve(a) = &mut request.action
        {
            a.decision = decision;
            a.note = Some("请完善边界".into());
        }
        store.submit_command(command).await?;
        assert_eq!(
            db.query_row("SELECT revision FROM runs", [], |r| r.get::<_, i64>(0))?,
            6,
            "one revision for approval including BUILD intent"
        );
        let reject = decision == SpecDecision::Reject;
        assert_eq!(
            db.query_row("SELECT state FROM tasks", [], |r| r.get::<_, String>(0))?,
            if reject { "READY" } else { "ACCEPTED" }
        );
        assert_eq!(
            db.query_row("SELECT mode FROM runs", [], |r| r.get::<_, String>(0))?,
            if reject { "PLAN" } else { "BUILD" }
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM specs", [], |r| r.get::<_, i64>(0))?,
            if reject { 0 } else { 1 }
        );
        assert_eq!(db.query_row("SELECT json_extract(payload_json,'$.answer') FROM events WHERE kind='question.answered'",[],|r|r.get::<_,String>(0))?,"请完善边界");
    }
    for text in ["", "please approve", "reject", "请修改"] {
        let (_dir, store, db, command) = approval_world().await?;
        store
            .submit_command(input_approval(command, true, Origin::User, text))
            .await?;
        assert_eq!(
            db.query_row("SELECT count(*) FROM specs", [], |r| r.get::<_, i64>(0))?,
            0,
            "only explicit approve approves"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_008_02_competing_first_approval_requires_reconfirmation_on_new_base() -> R {
    let (_dir, store, db, command) = approval_world().await?;
    db.execute_batch("INSERT INTO runs(run_id,project_id,state,mode,revision,created_at) VALUES('R2','P','SPECIFYING','PLAN',0,'2026-11-22T00:00:00Z');")?;
    let mut other = command.clone();
    other.command_id = "other".into();
    other.idempotency_key = "other".into();
    other.event.event_id = "other-event".into();
    other.event.run_id = Some("R2".into());
    other.run_id = "R2".into();
    other.expected_revision = 0;
    if let RunTarget::Specification(request) = &mut other.target {
        let files = request.files.as_mut().expect("files");
        files.sources[0].bytes.extend_from_slice(b"\n");
        let index = specindex::generate_index(&files.sources)?;
        files.existing_index = Some(index.bytes().into());
        files.tracked_index = Some(index.bytes().into());
        if let SpecAction::Approve(a) = &mut request.action {
            a.spec_sha256 = index.sha256();
        }
    }
    store.submit_command(other).await?;
    store.submit_command(command.clone()).await?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM specs", [], |r| r.get::<_, i64>(0))?,
        1,
        "old approve never crosses a base"
    );
    assert_eq!(
        db.query_row("SELECT state FROM tasks WHERE task_id='D'", [], |r| r
            .get::<_, String>(0))?,
        "IMPLEMENTED"
    );
    assert_eq!(
        db.query_row(
            "SELECT count(*) FROM change_requests WHERE state='PROPOSED'",
            [],
            |r| r.get::<_, i64>(0)
        )?,
        1
    );
    let q = super::super::draft_approval::pending(&db, "R")?.ok_or("reconfirmation")?;
    assert_ne!(q.question_id, "Q");
    assert!(q.anchor["base_spec_id"].is_string());
    let mut fresh = command;
    fresh.command_id = "fresh".into();
    fresh.idempotency_key = "fresh".into();
    fresh.event.event_id = "fresh-event".into();
    fresh.expected_revision =
        db.query_row("SELECT revision FROM runs WHERE run_id='R'", [], |r| {
            r.get(0)
        })?;
    store.submit_command(fresh).await?;
    assert_eq!(
        db.query_row("SELECT count(*) FROM specs", [], |r| r.get::<_, i64>(0))?,
        2
    );
    assert_eq!(
        db.query_row("SELECT state FROM tasks WHERE task_id='D'", [], |r| r
            .get::<_, String>(0))?,
        "ACCEPTED"
    );
    Ok(())
}

#[tokio::test]
async fn test_008_02_completion_and_blocked_draft_are_idempotent_user_questions() -> R {
    use super::super::{WriterCompleted, WriterCompletion, WriterOutcome};
    for blocked in [false, true] {
        let (_dir, store, db, command) = approval_world().await?;
        db.execute_batch("DELETE FROM events; UPDATE runs SET state='SPECIFYING',resume_state=NULL,blocked_reason=NULL,blocked_reasons_json='[]'; UPDATE tasks SET state='RUNNING';
    INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES('SD','claude','cto','test','{}');
    INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision,started_at) VALUES('A','D','SD','RUNNING',1,1,1,'2026-11-22T00:00:00Z');
    INSERT INTO leases(project_id,kind,attempt_id,epoch,expires_at) VALUES('P','writer','A',1,'2100-01-01T00:00:00Z');")?;
        let RunTarget::Specification(request) = command.target.clone() else {
            panic!("fixture")
        };
        let files = request.files?;
        let sha = specindex::generate_index(&files.sources)?.sha256();
        let outcome = if blocked {
            WriterOutcome::Decide {
                reason: super::super::need::WorkReason::AgentBlocked,
                detail: "目标用户是谁？".into(),
            }
        } else {
            WriterOutcome::Draft {
                git_commit: "a".repeat(40),
                files: Box::new(files.clone()),
            }
        };
        let job = WriterCompletion {
            attempt_id: "A".into(),
            ending: super::super::completion::tests::ending(),
            outcome,
        };
        let done = store.complete_writer_attempt(job.clone()).await?;
        assert!(if blocked {
            matches!(done, WriterCompleted::Blocked { .. })
        } else {
            matches!(done, WriterCompleted::Drafted)
        });
        assert!(matches!(
            store.complete_writer_attempt(job).await?,
            WriterCompleted::AlreadyEnded
        ));
        assert_eq!(
            db.query_row("SELECT count(*) FROM candidates", [], |r| r
                .get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM specs", [], |r| r.get::<_, i64>(0))?,
            0
        );
        assert_eq!(
            db.query_row("SELECT state||'/'||resume_state FROM runs", [], |r| r
                .get::<_, String>(0))?,
            "BLOCKED/SPECIFYING"
        );
        let qs = super::super::questions::open(&db, "R")?;
        assert_eq!(qs.len(), 1);
        assert_eq!(qs[0].default, None);
        if blocked {
            assert_eq!(qs[0].text, "目标用户是谁？");
            let mut note = command;
            note.kind = "AnswerQuestion".into();
            note.expected_revision = db.query_row("SELECT revision FROM runs", [], |r| r.get(0))?;
            note.target = RunTarget::Input(super::super::Input::Answer {
                question_id: qs[0].question_id.clone(),
                answer: "给管理员使用".into(),
                choice_id: None,
            });
            store.submit_command(note).await?;
            assert_eq!(
                db.query_row("SELECT state FROM tasks", [], |r| r.get::<_, String>(0))?,
                "READY"
            );
            assert_eq!(db.query_row("SELECT json_extract(payload_json,'$.note') FROM events WHERE kind='spec.draft_note'",[],|r|r.get::<_,String>(0))?,"给管理员使用");
        } else {
            assert_eq!(qs[0].anchor["spec_sha256"], sha);
            assert_eq!(
                db.query_row(
                    "SELECT count(*) FROM events WHERE kind='spec.draft_completed'",
                    [],
                    |r| r.get::<_, i64>(0)
                )?,
                1
            );
            assert!(super::super::spec_review::authors(&db, "R", "P", &sha)?.contains("SD"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_008_04_m3l102_draft_start_survives_attempt_retries() -> R {
    let (_dir, store, db, _) = approval_world().await?;
    db.execute_batch("INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES('SD','claude','cto','test','{}');
      INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision) VALUES
      ('A1','D','SD','FAILED',1,1,1),('A2','D','SD','RUNNING',1,1,1);")?;
    for (attempt, head) in [("A1", "original"), ("A2", "after-violation")] {
        db.execute("INSERT INTO events(event_id,project_id,run_id,kind,payload_json,created_at) VALUES(?1,'P','R','attempt.workspace_start',?2,'now')",
            params![child_id(attempt,"workspace_start"),json!({"attempt_id":attempt,"head":head}).to_string()])?;
    }
    assert_eq!(
        store.reader()?.draft_start("A2")?.ok_or("start")?["head"],
        "original"
    );
    Ok(())
}
