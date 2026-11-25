//! Non-terminal task migration through approval and the first subsequent plan import (ADR-033 Q5/Q6).
use super::*;

fn blocked(
    world: &World,
    run: &str,
    task: &str,
    requirement: &str,
    seq: i64,
    reason: &str,
    writer: bool,
) -> R {
    world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,blocked_reason,attempt_count,next_retry_at)
        SELECT '{task}',run_id,spec_id,'BLOCKED','developer',{writer},{seq},'NEED_DECISION',2,'2099-01-01T00:00:00Z' FROM runs WHERE run_id='{run}';
        INSERT INTO task_requirements(task_id,requirement_id,spec_id)
        SELECT '{task}','REQ-{requirement}',spec_id FROM runs WHERE run_id='{run}';
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at)
        SELECT project_id,'need-{task}',run_id,'need_decision','{{\"task_id\":\"{task}\",\"reason\":\"{reason}\",\"subject\":\"work\"}}',created_at FROM runs WHERE run_id='{run}';",writer=i32::from(writer)))?;
    Ok(())
}

fn remove_c(world: &World) -> R<Index> {
    let mut files = sources();
    files[0].bytes = String::from_utf8(files[0].bytes.clone())?
        .replacen("then: result 2", "then: revised 2", 1)
        .into_bytes();
    files[1].bytes = PREFIX.as_bytes().to_vec();
    for file in &files {
        std::fs::write(world.project().join(&file.path), &file.bytes)?;
    }
    Ok(specindex::generate_index(&files)?)
}

fn plan(ids: &[&str]) -> Value {
    json!({"tasks":ids.iter().map(|id| json!({"task_id":id,"title":id,"role":"developer","writer":true,"depends_on":[],"requirement_ids":["REQ-A"]})).collect::<Vec<_>>()})
}

fn context() -> hoh_core::decision::Context {
    hoh_core::decision::Context {
        required_capabilities: [("developer".into(), Default::default())].into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn test_033_04_mismatch_tasks_migrate_or_replaced() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}'"))?;
    world.staff(&run)?;
    blocked(&world, &run, "T1", "A", 3, "spec_mismatch", true)?;
    blocked(&world, &run, "T2", "C", 2, "spec_mismatch", true)?;
    blocked(&world, &run, "T4", "C", 1, "spec_mismatch", true)?;
    // A migrated task keeps its unchanged requirement too. Unrelated questions and readers stay blocked.
    world.exec(&format!(
        "INSERT INTO task_requirements SELECT 'T1','REQ-B',spec_id FROM runs WHERE run_id='{run}'"
    ))?;
    blocked(&world, &run, "T-unaffected", "B", 4, "spec_mismatch", true)?;
    blocked(&world, &run, "T-other-reason", "A", 5, "agent_failed", true)?;
    blocked(&world, &run, "T-reader", "A", 6, "spec_mismatch", false)?;
    // READY work predates approval and must remain dispatchable after its Run moves.
    world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,user_value) SELECT 'old-ready',run_id,spec_id,'READY','developer',1,0,5 FROM runs WHERE run_id='{run}';
        INSERT INTO task_requirements SELECT 'old-ready','REQ-B',spec_id FROM runs WHERE run_id='{run}';"))?;
    let before = world.rows("SELECT * FROM tasks ORDER BY task_id")?;
    let next = remove_c(&world)?;
    submit_spec(&host, &run, "migration-propose", "ProposeSpecChange", &next).await?;
    assert_eq!(world.rows("SELECT * FROM tasks ORDER BY task_id")?, before);
    let approval = submit_spec(&host, &run, "migration-approve", "ApproveSpec", &next).await?;
    let current = world.query("SELECT spec_id FROM specs WHERE revision=2")?;
    let migrated=world.rows("SELECT state,spec_id,blocked_reason,next_retry_at,attempt_count FROM tasks WHERE task_id='T1'")?;
    assert_eq!(
        migrated,
        vec![
            json!({"state":"READY","spec_id":current,"blocked_reason":null,"next_retry_at":null,"attempt_count":2})
        ],
        "approval must release and migrate a SPEC_MISMATCH write task"
    );
    assert_eq!(world.rows("SELECT requirement_id,spec_id FROM task_requirements WHERE task_id='T1' ORDER BY requirement_id")?,vec![json!({"requirement_id":"REQ-A","spec_id":current}),json!({"requirement_id":"REQ-B","spec_id":current})]);
    for id in ["T2", "T4"] {
        assert_eq!(
            world.query(&format!("SELECT state FROM tasks WHERE task_id='{id}'"))?,
            "CANCELLED"
        );
        assert_eq!(
            world.rows(&format!(
                "SELECT blocked_reason,next_retry_at FROM tasks WHERE task_id='{id}'"
            ))?,
            vec![json!({"blocked_reason":null,"next_retry_at":null})]
        );
    }
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='T-unaffected'")?,
        "READY"
    );
    for id in ["T-other-reason", "T-reader"] {
        assert_eq!(
            world.query(&format!("SELECT state FROM tasks WHERE task_id='{id}'"))?,
            "BLOCKED"
        );
    }
    let migrated_events = event_payloads(&world, "task.migrated")?;
    let mut migrated_ids: Vec<_> = migrated_events
        .iter()
        .map(|e| e["task_id"].as_str().ok_or("task id"))
        .collect::<Result<_, _>>()?;
    migrated_ids.sort_unstable();
    assert_eq!(
        migrated_ids,
        [
            "T-other-reason",
            "T-reader",
            "T-unaffected",
            "T1",
            "old-ready"
        ]
    );
    assert!(migrated_events.iter().all(|e| e["spec_revision"] == 2));
    for id in migrated_ids {
        assert_eq!(
            world.query(&format!("SELECT spec_id FROM tasks WHERE task_id='{id}'"))?,
            current
        );
        assert_eq!(world.query(&format!("SELECT count(*) FROM task_requirements WHERE task_id='{id}' AND spec_id<>'{current}'"))?, "0");
    }
    for id in ["T-other-reason", "T-reader"] {
        assert_eq!(
            world.rows(&format!(
                "SELECT blocked_reason,next_retry_at,attempt_count FROM tasks WHERE task_id='{id}'"
            ))?,
            vec![
                json!({"blocked_reason":"NEED_DECISION","next_retry_at":"2099-01-01T00:00:00Z","attempt_count":2})
            ]
        );
    }
    let open = event_payloads(&world, "spec.task_replacements_opened")?;
    assert_eq!(open.len(), 1);
    assert_eq!(
        open[0]["cancelled"],
        json!([{"from":"T4","deleted_requirement_ids":["REQ-C"],"created_seq":1},{"from":"T2","deleted_requirement_ids":["REQ-C"],"created_seq":2}])
    );
    let (_, body) = world::exchange(&host, "GET", &format!("/v1/runs/{run}"), None).await?;
    assert!(
        body["projection"]["questions"]
            .as_array()
            .ok_or("questions")?
            .iter()
            .all(|q| !["T1", "T2", "T4", "T-unaffected"]
                .iter()
                .any(|t| q["task_id"] == *t))
    );
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='AnswerQuestion'")?,
        "0"
    );
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&approval))
            .await?
            .0,
        202
    );
    assert_eq!(
        event_payloads(&world, "spec.task_replacements_opened")?.len(),
        1
    );
    // An invalid import cannot consume the opening. A successful import is a Commander decision.
    assert!(
        host.store()
            .import_plan(&run, json!({"tasks":[]}))
            .await
            .is_err()
    );
    let result = decide(
        &world,
        &host,
        &run,
        "new-plan",
        None,
        json!({"kind":"create_tasks","plan":plan(&["Z-new","A-new","extra-new"])}),
    )
    .await?;
    assert!(
        matches!(result, hoh_core::persistence::Decided::Applied { .. }),
        "{result:?}"
    );
    let replaced = event_payloads(&world, "task.replaced")?;
    assert_eq!(
        replaced
            .iter()
            .map(|e| json!({"from":e["from"],"to":e["to"]}))
            .collect::<Vec<_>>(),
        vec![
            json!({"from":"T4","to":"A-new"}),
            json!({"from":"T2","to":"Z-new"})
        ]
    );
    assert_eq!(
        event_payloads(&world, "spec.task_replacements_consumed")?.len(),
        1
    );
    host.store().import_plan(&run, plan(&["later-new"])).await?;
    assert_eq!(
        event_payloads(&world, "task.replaced")?.len(),
        2,
        "a second plan cannot replace any cancelled task again"
    );
    let dispatched = host
        .store()
        .schedule_round(&run, context())
        .await?
        .dispatched
        .ok_or("migration not dispatchable")?;
    assert_eq!(dispatched.task_id, "old-ready");
    assert_eq!(world.query("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN specs s ON s.spec_id=t.spec_id WHERE s.revision=1")?,"0");
    assert_eq!(world.query("SELECT s.revision FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN specs s ON s.spec_id=t.spec_id")?,"2");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_first_import_consumes_unpaired_tasks_and_survives_restart() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}'"))?;
    blocked(&world, &run, "C-first", "C", 2, "spec_mismatch", true)?;
    blocked(&world, &run, "C-last", "C", 3, "spec_mismatch", true)?;
    let next = remove_c(&world)?;
    submit_spec(&host, &run, "restart-propose", "ProposeSpecChange", &next).await?;
    submit_spec(&host, &run, "restart-approve", "ApproveSpec", &next).await?;
    assert_eq!(
        world.query("SELECT count(*) FROM tasks WHERE state='CANCELLED'")?,
        "2"
    );
    assert!(
        event_payloads(&world, "task.replaced")?.is_empty(),
        "without a plan there is no replacement"
    );
    host.shutdown().await;
    let host = world.start_plain_host(None).await?;
    world.exec("CREATE TRIGGER refuse_replacement BEFORE INSERT ON events WHEN NEW.kind='task.replaced' BEGIN SELECT RAISE(ABORT,'fixture refuses replacement'); END;")?;
    assert!(
        host.store()
            .import_plan(&run, plan(&["only-first"]))
            .await
            .is_err()
    );
    assert_eq!(
        world.query("SELECT count(*) FROM tasks WHERE task_id='only-first'")?,
        "0"
    );
    assert!(event_payloads(&world, "spec.task_replacements_consumed")?.is_empty());
    world.exec("DROP TRIGGER refuse_replacement;")?;
    host.store()
        .import_plan(&run, plan(&["only-first"]))
        .await?;
    let first = event_payloads(&world, "task.replaced")?;
    assert_eq!(first.len(), 1);
    assert_eq!(first[0]["from"], "C-first");
    assert_eq!(first[0]["to"], "only-first");
    host.store()
        .import_plan(&run, plan(&["not-a-replacement"]))
        .await?;
    assert_eq!(event_payloads(&world, "task.replaced")?, first);
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='C-last'")?,
        "CANCELLED"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_approval_is_atomic_and_partial_deletion_cancels_the_whole_task() -> R {
    let world = World::new()?;
    let (host, _, _) = approved(&world).await?;
    // Start on an approved spec: the first drafting run's no_spec question belongs to M3-T04.
    let run = world::start_run(&host, "paced-run", "migrate blocked work").await?;
    world.exec(&format!("UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}'"))?;
    world.staff(&run)?;
    blocked(&world, &run, "survives", "A", 1, "spec_mismatch", true)?;
    blocked(
        &world,
        &run,
        "partly-deleted",
        "C",
        2,
        "spec_mismatch",
        true,
    )?;
    blocked(
        &world,
        &run,
        "newer-question",
        "A",
        3,
        "spec_mismatch",
        true,
    )?;
    world.exec(&format!("INSERT INTO task_requirements SELECT 'partly-deleted','REQ-A',spec_id FROM runs WHERE run_id='{run}';
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at) SELECT project_id,'newer-question-anchor',run_id,'need_decision','{{\"task_id\":\"newer-question\",\"reason\":\"agent_failed\",\"subject\":\"work\"}}',created_at FROM runs WHERE run_id='{run}';"))?;
    host.store().pace_run(&run, context()).await?;
    assert_eq!(
        world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?,
        "BLOCKED"
    );
    let next = remove_c(&world)?;
    submit_spec(&host, &run, "atomic-propose", "ProposeSpecChange", &next).await?;
    let body = command(
        &host,
        &run,
        "atomic-approve",
        "ApproveSpec",
        json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let before = snapshot(&world)?;
    let tasks = world.rows("SELECT * FROM tasks ORDER BY task_id")?;
    let mappings = world.rows("SELECT * FROM task_requirements ORDER BY task_id,requirement_id")?;
    world.exec("CREATE TRIGGER refuse_migration BEFORE INSERT ON events WHEN NEW.kind='task.migrated' BEGIN SELECT RAISE(ABORT,'fixture refuses migration'); END;")?;
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body))
            .await?
            .0,
        409
    );
    assert_eq!(snapshot(&world)?, before);
    assert_eq!(world.rows("SELECT * FROM tasks ORDER BY task_id")?, tasks);
    assert_eq!(
        world.rows("SELECT * FROM task_requirements ORDER BY task_id,requirement_id")?,
        mappings
    );
    world.exec("DROP TRIGGER refuse_migration;")?;
    assert_eq!(
        world::exchange(&host, "POST", "/v1/commands", Some(&body))
            .await?
            .0,
        202
    );
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='partly-deleted'")?,
        "CANCELLED"
    );
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='newer-question'")?,
        "BLOCKED",
        "a newer question must not be overridden by the older mismatch anchor"
    );
    assert_eq!(
        event_payloads(&world, "spec.task_replacements_opened")?[0]["cancelled"],
        json!([{"from":"partly-deleted","deleted_requirement_ids":["REQ-C"],"created_seq":2}])
    );
    // The existing pace releases its Run-level claim once migrated work is ready, without a user's answer.
    host.store().pace_run(&run, context()).await?;
    assert_eq!(
        world.query(&format!("SELECT state FROM runs WHERE run_id='{run}'"))?,
        "RUNNING"
    );
    assert_eq!(
        host.store()
            .schedule_round(&run, context())
            .await?
            .dispatched
            .ok_or("no migrated dispatch")?
            .task_id,
        "survives"
    );
    assert_eq!(
        world.query("SELECT count(*) FROM commands WHERE kind='AnswerQuestion'")?,
        "0"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_multiple_approvals_pair_each_new_task_only_once() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    blocked(&world, &run, "Z-oldest", "C", 1, "spec_mismatch", true)?;
    let second = remove_c(&world)?;
    submit_spec(&host, &run, "second-propose", "ProposeSpecChange", &second).await?;
    submit_spec(&host, &run, "second-approve", "ApproveSpec", &second).await?;
    blocked(&world, &run, "A-later", "B", 2, "spec_mismatch", true)?;
    let files = vec![
        Source {
            path: "docs/spec/a.md".into(),
            bytes: format!("{PREFIX}{}", req("A", 2)).into_bytes(),
        },
        Source {
            path: "docs/spec/b.md".into(),
            bytes: PREFIX.as_bytes().to_vec(),
        },
    ];
    for file in &files {
        std::fs::write(world.project().join(&file.path), &file.bytes)?;
    }
    let third = specindex::generate_index(&files)?;
    submit_spec(&host, &run, "third-propose", "ProposeSpecChange", &third).await?;
    submit_spec(&host, &run, "third-approve", "ApproveSpec", &third).await?;
    assert_eq!(
        event_payloads(&world, "spec.task_replacements_opened")?.len(),
        2
    );
    host.store().import_plan(&run, plan(&["one-new"])).await?;
    let paired = event_payloads(&world, "task.replaced")?;
    assert_eq!(paired.len(), 1);
    assert_eq!(paired[0]["from"], "Z-oldest");
    assert_eq!(paired[0]["to"], "one-new");
    assert_eq!(
        event_payloads(&world, "spec.task_replacements_consumed")?.len(),
        2
    );
    host.store().import_plan(&run, plan(&["later-new"])).await?;
    assert_eq!(event_payloads(&world, "task.replaced")?, paired);
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='A-later'")?,
        "CANCELLED"
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_every_nonterminal_task_of_each_moved_run_migrates() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let other = world::start_run(&host, "other-migrating", "other work").await?;
    let ended = world::start_run(&host, "ended-history", "historical work").await?;
    let states = [
        "PENDING",
        "READY",
        "LEASED",
        "RUNNING",
        "IMPLEMENTED",
        "VERIFYING",
        "BLOCKED",
        "ACCEPTED",
        "CANCELLED",
    ];
    for (r, prefix) in [(&run, "main"), (&other, "other"), (&ended, "ended")] {
        for state in states {
            for requirement in ["A", "B", "C"] {
                let task = format!("{prefix}-{state}-{requirement}");
                world.exec(&format!("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,blocked_reason,next_retry_at,attempt_count)
                    SELECT '{task}',run_id,spec_id,'{state}','developer',1,1,CASE WHEN '{state}'='BLOCKED' THEN 'BLOCKED_AUTH' END,'2099-01-01T00:00:00Z',2 FROM runs WHERE run_id='{r}';
                    INSERT INTO task_requirements SELECT '{task}','REQ-{requirement}',spec_id FROM runs WHERE run_id='{r}';"))?;
            }
        }
    }
    world.exec(&format!("UPDATE runs SET state='COMPLETED' WHERE run_id='{ended}';
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) SELECT 'empty-reader',run_id,spec_id,'PENDING','researcher',0,2 FROM runs WHERE run_id='{run}';"))?;
    let history = world.rows(&format!("SELECT * FROM tasks WHERE run_id='{ended}' OR state IN ('ACCEPTED','CANCELLED') ORDER BY task_id"))?;
    let next = remove_c(&world)?;
    submit_spec(&host, &run, "all-propose", "ProposeSpecChange", &next).await?;
    submit_spec(&host, &run, "all-approve", "ApproveSpec", &next).await?;
    let current = world.query("SELECT spec_id FROM specs WHERE revision=2")?;
    for prefix in ["main", "other"] {
        for state in &states[..7] {
            for requirement in ["A", "B"] {
                let task = format!("{prefix}-{state}-{requirement}");
                let row = world.rows(&format!("SELECT state,spec_id,blocked_reason,next_retry_at,attempt_count,revision FROM tasks WHERE task_id='{task}'"))?;
                assert_eq!(
                    row,
                    vec![
                        json!({"state":state,"spec_id":current,"blocked_reason":if *state=="BLOCKED" {Some("BLOCKED_AUTH")} else {None},"next_retry_at":"2099-01-01T00:00:00Z","attempt_count":2,"revision":1})
                    ],
                    "{task}"
                );
                assert_eq!(
                    world.query(&format!(
                        "SELECT spec_id FROM task_requirements WHERE task_id='{task}'"
                    ))?,
                    current
                );
            }
            let deleted = format!("{prefix}-{state}-C");
            assert_eq!(
                world.query(&format!(
                    "SELECT state FROM tasks WHERE task_id='{deleted}'"
                ))?,
                "CANCELLED",
                "{deleted}"
            );
            assert_eq!(world.query(&format!("SELECT s.revision FROM task_requirements tr JOIN specs s ON s.spec_id=tr.spec_id WHERE tr.task_id='{deleted}'"))?, "1");
        }
    }
    assert_eq!(world.rows(&format!("SELECT * FROM tasks WHERE run_id='{ended}' OR task_id IN (SELECT task_id FROM tasks WHERE task_id LIKE '%-ACCEPTED-%' OR task_id LIKE '%-CANCELLED-%') ORDER BY task_id"))?, history);
    assert_eq!(world.query("SELECT s.revision FROM tasks t JOIN specs s ON s.spec_id=t.spec_id WHERE task_id='empty-reader'")?, "2");
    assert_eq!(event_payloads(&world, "task.migrated")?.len(), 29);
    let opened = event_payloads(&world, "spec.task_replacements_opened")?;
    assert_eq!(opened.len(), 2);
    assert!(
        opened
            .iter()
            .all(|e| e["cancelled"].as_array().is_some_and(|a| a.len() == 7))
    );
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_deleted_requirement_cancels_unfinished_writer_attempts() -> R {
    for (task_state, attempt_state) in [
        ("RUNNING", "RUNNING"),
        ("LEASED", "PREPARED"),
        ("LEASED", "STARTING"),
    ] {
        let world = World::new()?;
        let (host, run, _) = approved(&world).await?;
        // A real, task-owned process stands in for the writer; no model or credentials are used.
        let mut process = if attempt_state == "RUNNING" {
            use std::os::unix::process::CommandExt as _;
            Some(pty::Hoh(
                std::process::Command::new("sleep")
                    .arg("60")
                    .process_group(0)
                    .spawn()?,
            ))
        } else {
            None
        };
        world.exec(&format!(
            "UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}';
             INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
             SELECT 'deleted-writer',run_id,spec_id,'{task_state}','developer',1,1 FROM runs WHERE run_id='{run}';
             INSERT INTO task_requirements SELECT 'deleted-writer','REQ-C',spec_id FROM runs WHERE run_id='{run}';
             INSERT INTO sessions(session_id,agent,role,agent_version,capabilities_json) VALUES('writer-session','claude','developer','fake','[]');
             INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision)
             VALUES('unfinished','deleted-writer','writer-session','{attempt_state}',1,1,0),
                   ('historical','deleted-writer','writer-session','SETTLED',1,1,0);
             INSERT INTO leases(project_id,kind,attempt_id,epoch,expires_at)
             SELECT project_id,'writer','unfinished',1,'2099-01-01T00:00:00Z' FROM runs WHERE run_id='{run}';
             INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
             SELECT 'unaffected',run_id,spec_id,'RUNNING','researcher',0,2 FROM runs WHERE run_id='{run}';
             INSERT INTO task_requirements SELECT 'unaffected','REQ-B',spec_id FROM runs WHERE run_id='{run}';
             INSERT INTO attempts(attempt_id,task_id,session_id,state,role_epoch,host_epoch,config_revision)
             VALUES('other-attempt','unaffected','writer-session','RUNNING',1,1,0);"
        ))?;
        if let Some(process) = &process {
            let identity = hoh_agents::identity::of(process.0.id())?.ok_or("writer identity")?;
            world.exec(&format!(
                "UPDATE attempts SET pid={},boot_id='{}',process_start_id='{}' WHERE attempt_id='unfinished'",
                identity.pid, identity.boot_id, identity.start_id
            ))?;
        }
        let next = remove_c(&world)?;
        submit_spec(&host, &run, "cancel-propose", "ProposeSpecChange", &next).await?;
        let before = world.rows("SELECT * FROM attempts ORDER BY attempt_id")?;
        let tasks_before = world.rows("SELECT * FROM tasks ORDER BY task_id")?;
        world.exec("CREATE TRIGGER refuse_writer_cancel BEFORE INSERT ON events WHEN NEW.kind='task.cancelled' BEGIN SELECT RAISE(ABORT,'fixture refuses writer cancellation'); END;")?;
        let body = command(
            &host,
            &run,
            "cancel-rollback",
            "ApproveSpec",
            json!({"spec_sha256":next.sha256(),"decision":"APPROVE_ONLY"}),
        )
        .await?;
        assert_ne!(
            world::exchange(&host, "POST", "/v1/commands", Some(&body))
                .await?
                .0,
            202
        );
        assert_eq!(
            world.rows("SELECT * FROM attempts ORDER BY attempt_id")?,
            before
        );
        assert_eq!(
            world.rows("SELECT * FROM tasks ORDER BY task_id")?,
            tasks_before
        );
        assert_eq!(world.query("SELECT count(*) FROM specs")?, "1");
        assert_eq!(
            world.query("SELECT attempt_id FROM leases WHERE kind='writer'")?,
            "unfinished"
        );
        world.exec("DROP TRIGGER refuse_writer_cancel;")?;

        let approval = submit_spec(&host, &run, "cancel-approve", "ApproveSpec", &next).await?;
        assert_eq!(
            world.query("SELECT state FROM tasks WHERE task_id='deleted-writer'")?,
            "CANCELLED"
        );
        assert_eq!(
            world.query("SELECT cancel_requested FROM attempts WHERE attempt_id='unfinished'")?,
            "1",
            "approval must request cancellation of the {attempt_state} Attempt of a {task_state} deleted writer"
        );
        // The host's existing single-Attempt controller stops the writer process, proves its end,
        // settles cancellation and releases the writer lease; no runner_error completion is involved.
        if let Some(process) = &mut process {
            use std::os::unix::process::ExitStatusExt as _;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            // This test spawned the process, so it must reap it before the host can prove the group empty.
            let status = loop {
                if let Some(status) = process.0.try_wait()? {
                    break status;
                }
                if std::time::Instant::now() >= deadline {
                    return Err("cancelled writer is still alive".into());
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            };
            assert_eq!(
                status.signal(),
                Some(15),
                "writer must end by cancellation, not its natural timeout"
            );
        }
        world
            .until(
                "SELECT state FROM attempts WHERE attempt_id='unfinished'",
                "CANCELLED",
                std::time::Duration::from_secs(5),
            )
            .await?;
        assert_eq!(world.query("SELECT count(*) FROM leases WHERE attempt_id='unfinished' AND expires_at>strftime('%Y-%m-%dT%H:%M:%SZ','now')")?, "0");
        assert_eq!(world.query("SELECT state FROM runs")?, "RUNNING");
        assert_eq!(world.query("SELECT count(*) FROM attempts WHERE attempt_id IN ('historical','other-attempt') AND cancel_requested=1")?, "0");
        assert_eq!(
            world.query("SELECT state FROM attempts WHERE attempt_id='other-attempt'")?,
            "RUNNING"
        );
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='cancel.reaped' AND json_extract(payload_json,'$.attempt_id')='unfinished'")?, "1");
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE payload_json LIKE '%runner_error%'")?,
            "0"
        );
        assert_eq!(
            world::exchange(&host, "POST", "/v1/commands", Some(&approval))
                .await?
                .0,
            202
        );
        assert_eq!(event_payloads(&world, "task.cancelled")?.len(), 1);
        host.shutdown().await;
    }
    Ok(())
}

#[tokio::test]
async fn test_033_02_approval_between_reconcile_and_schedule_never_dispatches_stale_verifiers() -> R
{
    use hoh_core::handoff::{Recipient, Verification};
    let world = World::new()?;
    let (host, run, first) = approved(&world).await?;
    let candidate = accepted_candidate(&world, &host, &run).await?;
    world.exec(&format!(r#"UPDATE tasks SET state='VERIFYING' WHERE task_id='TA';
        UPDATE candidates SET state='VERIFYING' WHERE candidate_id='{candidate}';
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
        SELECT 'ready-qa',run_id,spec_id,'READY','qa',0,10 FROM runs WHERE run_id='{run}';
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
        SELECT 'ready-reviewer',run_id,spec_id,'READY','reviewer',0,11 FROM runs WHERE run_id='{run}';
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at)
        SELECT project_id,'ready-round',run_id,'verification.round_opened',
        '{{"round_id":"ready-round","candidate_id":"{candidate}","source_task_ids":["TA"],"acceptance_ids":["AC-A-01","AC-A-02"],"tasks":{{"qa":"ready-qa","reviewer":"ready-reviewer"}}}}',created_at FROM runs WHERE run_id='{run}';"#))?;
    let row = world.rows("SELECT candidate_id AS id,git_commit,git_tree,spec_sha256,environment_sha256,test_plan_sha256 FROM candidates")?.remove(0);
    let messages = hoh_core::handoff::verification(
        &Verification {
            round_id: "ready-round".into(),
            run_id: run.clone(),
            task_id: "TA".into(),
            attempt_id: "observed".into(),
            candidate: serde_json::from_value(row)?,
            spec_sha256: first.sha256(),
            spec_revision: 1,
            acceptance_ids: vec!["AC-A-01".into(), "AC-A-02".into()],
            goal: "verify the candidate".into(),
            task_revision: 0,
            cause: "ready-round".into(),
            references: vec![],
            budget_tokens: 32000,
        },
        &[
            Recipient {
                role: "qa".into(),
                role_epoch: 1,
            },
            Recipient {
                role: "reviewer".into(),
                role_epoch: 1,
            },
        ],
    )
    .ok_or("verification handoffs")?;
    for message in messages {
        let payload = serde_json::to_string(&message)?.replace('\'', "''");
        world.exec(&format!("INSERT INTO outbox(message_id,event_seq,run_id,recipient_role,role_epoch,payload_json,state)
            SELECT '{}',seq,'{run}','{}',1,'{payload}','PENDING' FROM events WHERE event_id='ready-round'",message.message_id,message.recipient_role))?;
    }
    let verifier_context = |role: &str| hoh_core::decision::Context {
        required_capabilities: [(role.into(), Default::default())].into(),
        ..Default::default()
    };
    // Both verifiers really reach attempt creation before approval; rollback preserves the READY fixture.
    world.exec("CREATE TRIGGER dispatch_probe BEFORE INSERT ON attempts BEGIN SELECT RAISE(ABORT,'fixture: verifier is dispatchable'); END;")?;
    for role in ["qa", "reviewer"] {
        let probe = host
            .store()
            .schedule_round(&run, verifier_context(role))
            .await;
        assert!(
            probe
                .as_ref()
                .is_err_and(|e| e.to_string().contains("fixture: verifier is dispatchable")),
            "{role}: {probe:?}"
        );
    }
    world.exec("DROP TRIGGER dispatch_probe;")?;
    let next = revise_a(&world)?;
    submit_spec(&host, &run, "ready-propose", "ProposeSpecChange", &next).await?;
    host.store()
        .reconcile_verification(&run, Default::default())
        .await?;
    submit_spec(&host, &run, "ready-approve", "ApproveSpec", &next).await?;
    assert_eq!(world.query("SELECT state FROM candidates")?, "STALE");
    assert_eq!(world.query("SELECT count(*) FROM tasks t JOIN runs r ON r.run_id=t.run_id WHERE t.task_id IN ('ready-qa','ready-reviewer') AND t.state='READY' AND t.spec_id=r.spec_id")?, "2");
    let before = world.rows("SELECT * FROM attempts ORDER BY attempt_id")?;
    // Do not reconcile again: approval committed in the host's reconcile/schedule gap.
    for role in ["qa", "reviewer"] {
        let scheduled = host
            .store()
            .schedule_round(&run, verifier_context(role))
            .await?;
        assert!(
            scheduled.dispatched.is_none(),
            "stale {role} must be undispatchable before another reconcile: {scheduled:?}"
        );
    }
    assert_eq!(
        world.rows("SELECT * FROM attempts ORDER BY attempt_id")?,
        before
    );
    let reconciled = host
        .store()
        .reconcile_verification(&run, Default::default())
        .await?;
    assert_eq!(
        reconciled.closed,
        vec![("ready-round".into(), "superseded".into())]
    );
    assert_eq!(world.query("SELECT count(*) FROM tasks WHERE task_id IN ('ready-qa','ready-reviewer') AND state='CANCELLED'")?, "2");
    host.shutdown().await;
    Ok(())
}

#[tokio::test]
async fn test_033_04_stale_candidate_source_returns_ready_on_new_revision() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let candidate = accepted_candidate(&world, &host, &run).await?;
    // Persist a round with finished verifier tasks; this exercises reconciliation without running agents.
    world.exec(&format!(r#"UPDATE tasks SET state='VERIFYING' WHERE task_id='TA';
        UPDATE candidates SET state='VERIFYING' WHERE candidate_id='{candidate}';
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
        SELECT 'finished-qa',run_id,spec_id,'IMPLEMENTED','qa',0,10 FROM runs WHERE run_id='{run}';
        INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq)
        SELECT 'finished-reviewer',run_id,spec_id,'IMPLEMENTED','reviewer',0,11 FROM runs WHERE run_id='{run}';
        INSERT INTO events(project_id,event_id,run_id,kind,payload_json,created_at)
        SELECT project_id,'stale-source-round',run_id,'verification.round_opened',
        '{{"round_id":"stale-source-round","candidate_id":"{candidate}","source_task_ids":["TA"],"acceptance_ids":["AC-A-01","AC-A-02"],"tasks":{{"qa":"finished-qa","reviewer":"finished-reviewer"}}}}',created_at FROM runs WHERE run_id='{run}';"#))?;
    let next = revise_a(&world)?;
    submit_spec(&host, &run, "source-propose", "ProposeSpecChange", &next).await?;
    submit_spec(&host, &run, "source-approve", "ApproveSpec", &next).await?;
    assert_eq!(world.query("SELECT state FROM candidates")?, "STALE");
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='TA'")?,
        "VERIFYING"
    );
    let reconciled = host
        .store()
        .reconcile_verification(&run, Default::default())
        .await?;
    assert_eq!(
        reconciled.closed,
        vec![("stale-source-round".into(), "superseded".into())]
    );
    assert_eq!(
        world.query("SELECT state FROM tasks WHERE task_id='TA'")?,
        "READY"
    );
    assert_eq!(
        world.query(
            "SELECT s.revision FROM tasks t JOIN specs s ON s.spec_id=t.spec_id WHERE task_id='TA'"
        )?,
        "2",
        "a refreeze source must not be stranded on the stale candidate's revision"
    );
    assert_eq!(world.query("SELECT s.revision FROM task_requirements t JOIN specs s ON s.spec_id=t.spec_id WHERE task_id='TA'")?, "2");
    assert_eq!(
        event_payloads(&world, "task.migrated")?
            .iter()
            .filter(|e| e["task_id"] == "TA")
            .count(),
        1
    );
    host.store()
        .reconcile_verification(&run, Default::default())
        .await?;
    assert_eq!(
        event_payloads(&world, "verification.round_closed")?.len(),
        1
    );
    assert_eq!(
        host.store()
            .schedule_round(&run, context())
            .await?
            .dispatched
            .ok_or("source not dispatchable")?
            .task_id,
        "TA"
    );
    host.shutdown().await;
    Ok(())
}

/// M3B-52: a plan import consumes replacement openings of its own run only.
#[tokio::test]
async fn test_033_04_plan_import_pairs_replacements_per_run() -> R {
    let world = World::new()?;
    let (host, run, _) = approved(&world).await?;
    let other = world::start_run(&host, "other-import", "other work").await?;
    blocked(&world, &run, "main-c", "C", 1, "spec_mismatch", true)?;
    blocked(&world, &other, "other-c", "C", 1, "spec_mismatch", true)?;
    let next = remove_c(&world)?;
    submit_spec(&host, &run, "pair-propose", "ProposeSpecChange", &next).await?;
    submit_spec(&host, &run, "pair-approve", "ApproveSpec", &next).await?;
    for id in [&run, &other] {
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE run_id='{id}' AND kind='spec.task_replacements_opened'"
            ))?,
            "1",
            "{id}"
        );
    }
    host.store().import_plan(&run, plan(&["main-new"])).await?;
    assert_eq!(
        world.rows(&format!(
            "SELECT json_extract(payload_json,'$.from') AS \"from\", json_extract(payload_json,'$.to') AS \"to\" \
             FROM events WHERE run_id='{run}' AND kind='task.replaced' ORDER BY seq"
        ))?,
        vec![json!({"from": "main-c", "to": "main-new"})]
    );
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{other}' AND kind='spec.task_replacements_consumed'"
        ))?,
        "0",
        "importing one run must leave the other run's opening unconsumed"
    );
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{other}' AND kind='task.replaced'"
        ))?,
        "0"
    );
    host.store()
        .import_plan(&other, plan(&["other-new"]))
        .await?;
    assert_eq!(
        world.rows(&format!(
            "SELECT json_extract(payload_json,'$.from') AS \"from\", json_extract(payload_json,'$.to') AS \"to\" \
             FROM events WHERE run_id='{other}' AND kind='task.replaced' ORDER BY seq"
        ))?,
        vec![json!({"from": "other-c", "to": "other-new"})]
    );
    assert_eq!(
        world.query(&format!(
            "SELECT count(*) FROM events WHERE run_id='{other}' AND kind='spec.task_replacements_consumed'"
        ))?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// M3QA-019 (AC-033-04 / fault matrix G4): the product itself places a writer Task into SPEC_MISMATCH hold
/// (`completion.rs:672-677` WorkReason::SpecMismatch) when the specification advances before its rev-1 freeze
/// completes, and refuses dispatch of an old-spec Task (`admission.rs:283` Refusal::SpecMismatch); then the
/// subsequent approval releases & migrates T1 (REQ-A) and cancels T2 (REQ-C, replaced by T3).
#[tokio::test]
async fn test_033_04_product_places_writer_in_spec_mismatch_hold_then_approval_migrates_or_cancels()
-> R {
    fn revise_b(world: &World) -> R<Index> {
        let mut files = sources();
        files[0].bytes = String::from_utf8(files[0].bytes.clone())?
            .replacen("then: result 3", "then: result B-rev2", 1)
            .into_bytes();
        std::fs::write(world.project().join(&files[0].path), &files[0].bytes)?;
        Ok(specindex::generate_index(&files)?)
    }

    for (task_id, req_id, deleted) in [("T1", "REQ-A", false), ("T2", "REQ-C", true)] {
        let world = World::new()?;
        let (host, run, _) = approved(&world).await?;
        world.exec(&format!(
            "UPDATE runs SET state='RUNNING',blocked_reason=NULL,blocked_reasons_json='[]',resume_state=NULL WHERE run_id='{run}'"
        ))?;
        world.staff(&run)?;
        let rev1_spec = world.query("SELECT spec_id FROM specs WHERE revision=1")?;
        world.review_spec(&rev1_spec)?;
        // Import a real planned writer task on revision 1 via Commander decision (no SQL-seeded SPEC_MISMATCH).
        let planned = decide(
            &world,
            &host,
            &run,
            "plan-initial",
            None,
            json!({"kind":"create_tasks","plan":{"tasks":[
                {"task_id":task_id,"title":task_id,"role":"developer","writer":true,"depends_on":[],"requirement_ids":[req_id]}
            ]}}),
        )
        .await?;
        assert!(
            matches!(planned, hoh_core::persistence::Decided::Applied { .. }),
            "{planned:?}"
        );
        // Schedule and start the writer attempt on revision 1.
        let dispatched = host
            .store()
            .schedule_round(&run, context())
            .await?
            .dispatched
            .ok_or("writer task must dispatch on rev 1")?;
        assert_eq!(dispatched.task_id, task_id);
        let attempt_id = dispatched.attempt_id.clone();
        let project_id = world.query("SELECT project_id FROM projects")?;
        let notice = host
            .store()
            .runner_waiting(&project_id)
            .await?
            .dispatches
            .into_iter()
            .find(|n| n.attempt_id == attempt_id)
            .ok_or("dispatch notice for scheduled attempt")?;
        assert!(
            host.store()
                .runner_received(notice, std::time::SystemTime::now())
                .await?,
            "attempt must enter STARTING with PREPARED work effect"
        );
        assert!(
            host.store()
                .runner_running(
                    &attempt_id,
                    hoh_core::persistence::AttemptFacts::default(),
                    json!({}),
                )
                .await?,
            "attempt and task must enter RUNNING"
        );
        // Prepare the real freeze job on revision 1 while the writer is completing.
        let rev1_freeze = hoh_app::verify::freeze::prepare_job(
            &world.project(),
            &world.project().join(".hoh"),
            &run,
            &Inputs {
                spec: specindex::PATH.into(),
                environment: "environment.json".into(),
                test_plan: "tests/check-plan.json".into(),
            },
        )?;
        // Revision 2 is approved (modifying REQ-B) before the writer's completion commits.
        let rev2 = revise_b(&world)?;
        submit_spec(&host, &run, "rev2-propose", "ProposeSpecChange", &rev2).await?;
        submit_spec(&host, &run, "rev2-approve", "ApproveSpec", &rev2).await?;
        let rev2_spec = world.query("SELECT spec_id FROM specs WHERE revision=2")?;
        // Complete the writer with its rev-1 freeze job: the product itself rejects with SpecMismatch and places the task in SPEC_MISMATCH hold.
        let result: hoh_core::protocol::TaskResult = serde_json::from_value(json!({
            "schema_version": "1.0",
            "run_id": run,
            "task_id": task_id,
            "attempt_id": attempt_id,
            "role": "developer",
            "role_epoch": 1,
            "status": "COMPLETED",
            "summary": "completed against rev 1",
            "candidate_id": null,
            "evidence": [],
            "issues": [],
            "next_action": "verify"
        }))?;
        let completed = host
            .store()
            .complete_writer_attempt(hoh_core::persistence::WriterCompletion {
                attempt_id: attempt_id.clone(),
                ending: hoh_core::persistence::Ending {
                    success: true,
                    detail: "writer finished on rev 1".into(),
                    class: hoh_core::retry::FailureClass::Transient { retry_after: None },
                    at: std::time::SystemTime::now(),
                    draw: 0,
                    observed: hoh_core::persistence::Observed {
                        workspace: Some(json!({
                            "head": world.git(&["rev-parse", "HEAD"])?.trim(),
                            "dirty": false
                        })),
                        ..Default::default()
                    },
                    unknown_effect: false,
                },
                outcome: hoh_core::persistence::WriterOutcome::Freeze {
                    job: Box::new(rev1_freeze),
                    result: Box::new(result),
                    self_tests: Default::default(),
                },
            })
            .await?;
        assert!(
            matches!(
                completed,
                hoh_core::persistence::WriterCompleted::Blocked { ref reason } if reason == "spec_mismatch"
            ),
            "expected product to hold writer on SpecMismatch: {completed:?}"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT state || '/' || coalesce(blocked_reason,'NULL') FROM tasks WHERE task_id='{task_id}'"
            ))?,
            "BLOCKED/NEED_DECISION",
            "product must block {task_id} on SPEC_MISMATCH"
        );
        assert_eq!(
            world.query(&format!(
                "SELECT count(*) FROM events WHERE kind='need_decision' AND json_extract(payload_json,'$.task_id')='{task_id}' AND json_extract(payload_json,'$.reason')='spec_mismatch'"
            ))?,
            "1",
            "product must write need_decision(spec_mismatch) for {task_id}"
        );
        assert!(
            world.query(&format!(
                "SELECT payload_json FROM events WHERE kind='need_decision' AND json_extract(payload_json,'$.task_id')='{task_id}' AND json_extract(payload_json,'$.reason')='spec_mismatch'"
            ))?.contains("specification"),
            "need_decision must record the spec mismatch detail"
        );
        // Exercise admission.rs:283 Refusal::SpecMismatch: a READY task still bound to revision 1 is refused by schedule_round on revision 2.
        world.exec(&format!(
            "INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq) \
             VALUES('stale-rev1','{run}','{rev1_spec}','READY','developer',1,99);"
        ))?;
        let refused_round = host.store().schedule_round(&run, context()).await?;
        assert!(
            refused_round.dispatched.is_none(),
            "admission must refuse stale-spec task: {refused_round:?}"
        );
        world.exec("DELETE FROM tasks WHERE task_id='stale-rev1';")?;
        // Revision 3 rewrites REQ-A and deletes REQ-C: product releases T1 to READY (migrated) or cancels T2 (replaced by T3).
        let rev3 = remove_c(&world)?;
        submit_spec(&host, &run, "rev3-propose", "ProposeSpecChange", &rev3).await?;
        submit_spec(&host, &run, "rev3-approve", "ApproveSpec", &rev3).await?;
        let rev3_spec = world.query("SELECT spec_id FROM specs WHERE revision=3")?;
        world.review_spec(&rev3_spec)?;
        if !deleted {
            assert_eq!(
                world.query(&format!(
                    "SELECT state || '/' || spec_id || '/' || coalesce(blocked_reason,'NULL') FROM tasks WHERE task_id='{task_id}'"
                ))?,
                format!("READY/{rev3_spec}/NULL"),
                "product must release and migrate {task_id} from its product-written SPEC_MISMATCH hold"
            );
            assert_eq!(
                world.query(&format!(
                    "SELECT spec_id FROM task_requirements WHERE task_id='{task_id}' AND requirement_id='REQ-A'"
                ))?,
                rev3_spec
            );
            assert!(
                event_payloads(&world, "task.migrated")?
                    .iter()
                    .any(|e| e["task_id"] == task_id && e["spec_revision"] == 3)
            );
            let next_dispatch = host
                .store()
                .schedule_round(&run, context())
                .await?
                .dispatched
                .ok_or("migrated T1 must dispatch on rev 3")?;
            assert_eq!(next_dispatch.task_id, task_id);
            assert_eq!(
                world.query(&format!(
                    "SELECT s.revision FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN specs s ON s.spec_id=t.spec_id WHERE a.attempt_id='{}'",
                    next_dispatch.attempt_id
                ))?,
                "3"
            );
        } else {
            assert_eq!(
                world.query(&format!(
                    "SELECT state FROM tasks WHERE task_id='{task_id}'"
                ))?,
                "CANCELLED"
            );
            let replanned = decide(
                &world,
                &host,
                &run,
                "plan-t3",
                None,
                json!({"kind":"create_tasks","plan":plan(&["T3"])}),
            )
            .await?;
            assert!(
                matches!(replanned, hoh_core::persistence::Decided::Applied { .. }),
                "{replanned:?}"
            );
            let replaced = event_payloads(&world, "task.replaced")?;
            assert_eq!(
                replaced
                    .iter()
                    .map(|e| json!({"from": e["from"], "to": e["to"]}))
                    .collect::<Vec<_>>(),
                vec![json!({"from": "T2", "to": "T3"})]
            );
            let next_dispatch = host
                .store()
                .schedule_round(&run, context())
                .await?
                .dispatched
                .ok_or("replacement T3 must dispatch on rev 3")?;
            assert_eq!(next_dispatch.task_id, "T3");
            assert_eq!(
                world.query(&format!(
                    "SELECT s.revision FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN specs s ON s.spec_id=t.spec_id WHERE a.attempt_id='{}'",
                    next_dispatch.attempt_id
                ))?,
                "3"
            );
            assert_ne!(rev2_spec, rev3_spec);
        }
        host.shutdown().await;
    }
    Ok(())
}
