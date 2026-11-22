//! Runtime-owned specification drafting (REQ-008, ADR-038), outside the work scheduler.
use super::writer::{self, Change, Commit, Ctx, Entity, Intent, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{Mode, RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

fn event(project: &str, run: &str, id: String, kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        intent: Intent::Progress,
        project_id: project.into(),
        run_id: Some(run.into()),
        event_id: id,
        kind: kind.into(),
        payload,
    }
}

pub(super) fn request(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    command: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    super::modes::intent(tx, run, command, Mode::Plan, ctx)?;
    writer::write(
        tx,
        &Commit {
            events: vec![event(
                project,
                run,
                child_id(run, "draft/request"),
                "spec.draft_requested",
                json!({"run_id":run,"command_id":command,"cause":"no_spec","base_spec_id":null,"generation":1}),
            )],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

/// One durable phase per step. Only the host's read-only discovery continuation permits the second edge.
pub(super) fn step(
    conn: &mut Connection,
    run: &str,
    discovered: bool,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let row: Option<(String, String, i64)> = tx
        .query_row(
            "SELECT project_id,state,revision FROM runs r WHERE run_id=?1 AND spec_id IS NULL \
         AND EXISTS(SELECT 1 FROM events WHERE run_id=r.run_id AND kind='spec.draft_requested') \
         AND NOT EXISTS(SELECT 1 FROM tasks WHERE run_id=r.run_id AND kind='spec_draft')",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((project, state, revision)) = row else {
        return Ok(false);
    };
    let to = match state.as_str() {
        "CREATED" => RunState::Discovering,
        "DISCOVERING" if discovered => RunState::Specifying,
        _ => return Ok(false),
    };
    // Another run's approval may have arrived after the user's draft answer. Adopt it before a NULL task is made.
    if super::adoption::adopt_exit(&tx, &project, run, ctx)? {
        writer::write(
            &tx,
            &Commit {
                cas: vec![StateCas {
                    id: run.into(),
                    expected_revision: revision,
                    change: Change::Touch(Entity::Run),
                }],
                ..Commit::default()
            },
            ctx,
        )?;
        writer::commit(tx, ctx)?;
        return Ok(true);
    }
    let mut events = vec![event(
        &project,
        run,
        child_id(run, &format!("draft/{}", to.as_str())),
        "run.transitioned",
        json!({"from":state,"to":to,"cause":"spec_draft"}),
    )];
    if to == RunState::Specifying {
        let task = child_id(run, "spec-draft/1");
        tx.execute("INSERT INTO tasks(task_id,run_id,spec_id,state,role,writer,created_seq,layer,kind) \
            VALUES(?1,?2,NULL,'READY','cto',1,(SELECT coalesce(max(created_seq),0)+1 FROM tasks WHERE run_id=?2),0,'spec_draft')",params![task,run]).map_err(sqlite_error)?;
        events.push(event(
            &project,
            run,
            child_id(run, "draft/created"),
            "spec.drafting",
            json!({"run_id":run,"task_id":task,"generation":1,"base_spec_id":null}),
        ));
    }
    writer::write(
        &tx,
        &Commit {
            cas: vec![StateCas {
                id: run.into(),
                expected_revision: revision,
                change: Change::Run(to),
            }],
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

pub(super) fn dispatch(
    conn: &mut Connection,
    run: &str,
    context: &crate::decision::Context,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let row:Option<(String,i64,String,i64,i64)> = tx.query_row(
        "SELECT t.task_id,t.revision,r.project_id,r.revision,t.cost_estimate FROM tasks t JOIN runs r ON r.run_id=t.run_id \
         WHERE t.run_id=?1 AND t.kind='spec_draft' AND t.state='READY' \
         AND (t.next_retry_at IS NULL OR t.next_retry_at<=?2) ORDER BY t.created_seq LIMIT 1",
        params![run,crate::time::rfc3339_utc(context.now())],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sqlite_error)?;
    let Some((task, revision, project, run_revision, cost)) = row else {
        return Ok(false);
    };
    let Ok(admission) = super::admission::admit_one(&tx, run, &task, super::admission::By::Draft)?
    else {
        return Ok(false);
    };
    if super::retry::budget_stop(&tx, run, context)?.is_some()
        || context.budget_headroom.is_some_and(|h| i64::from(h) < cost)
    {
        return Ok(false);
    }
    let Some(binding) =
        super::decide::load_binding(&tx, run, "cto", &ctx.resource_bundle.version())?
    else {
        return Ok(false);
    };
    if !binding.active
        || !context
            .required_capabilities
            .get("cto")
            .is_some_and(|required| required.is_subset(&binding.capabilities))
    {
        return Ok(false);
    }
    let Some(kind) =
        super::leases::free_slot(&tx, &project, true, context.reader_slots(), context.now())?
    else {
        return Ok(false);
    };
    let attempts: i64 = tx
        .query_row(
            "SELECT count(*) FROM attempts WHERE task_id=?1",
            [&task],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let leased = super::decide::lease_task(
        &tx,
        &super::decide::LeaseRequest {
            project_id: &project,
            run_id: run,
            task_id: &task,
            attempt_id: child_id(&task, &format!("attempt-{}", attempts + 1)),
            role: "cto",
            role_epoch: binding.role_epoch,
            kind: &kind,
            now: context.now(),
            admission,
        },
    )?;
    let lease = json!({"kind":leased.grant.kind,"epoch":leased.grant.epoch,"expires_at":leased.grant.expires_at});
    writer::write(
        &tx,
        &Commit {
            cas: vec![
                StateCas {
                    id: task.clone(),
                    expected_revision: revision,
                    change: Change::Task(TaskState::Leased),
                },
                StateCas {
                    id: run.into(),
                    expected_revision: run_revision,
                    change: Change::Touch(Entity::Run),
                },
            ],
            events: vec![event(
                &project,
                run,
                child_id(&leased.attempt_id, "draft/dispatch"),
                "spec.draft_dispatched",
                json!({"task_id":task,"attempt_id":leased.attempt_id,"lease":lease}),
            )],
            outbox: vec![NewOutbox {
                message_id: child_id(&leased.attempt_id, "dispatch"),
                event: 0,
                run_id: Some(run.into()),
                recipient_role: "cto".into(),
                role_epoch: super::decide::to_i64(binding.role_epoch, "role_epoch")?,
                payload: json!({"notice":"runtime","kind":"dispatch","origin":"spec_draft","task_id":task,"attempt_id":leased.attempt_id,"role":"cto","lease":lease}),
            }],
        },
        ctx,
    )?;
    writer::commit(tx, ctx)?;
    Ok(true)
}

pub(super) fn is_draft(conn: &Connection, task: &str) -> Result<bool, PersistError> {
    conn.query_row(
        "SELECT kind='spec_draft' FROM tasks WHERE task_id=?1",
        [task],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

pub(super) fn prepare_init(
    conn: &mut Connection,
    attempt: &str,
    ctx: &Ctx<'_>,
) -> Result<bool, PersistError> {
    let tx = writer::begin(conn, ctx)?;
    let allowed: bool = tx
        .query_row(
            "SELECT a.state='STARTING' AND a.cancel_requested=0 AND t.kind='spec_draft' \
        FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE a.attempt_id=?1",
            [attempt],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if !allowed {
        return Ok(false);
    }
    let (run,task):(String,String)=tx.query_row("SELECT t.run_id,t.task_id FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE a.attempt_id=?1",[attempt],|r|Ok((r.get(0)?,r.get(1)?))).map_err(sqlite_error)?;
    if super::admission::admit_one(&tx, &run, &task, super::admission::By::Draft)?.is_err() {
        return Ok(false);
    }
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO effects(effect_key,attempt_id,state) VALUES(?1,?2,'PREPARED')",
            params![child_id(attempt, "git-init"), attempt],
        )
        .map_err(sqlite_error)?;
    writer::commit(tx, ctx)?;
    Ok(n == 1)
}

pub(super) fn discovery_due(conn: &Connection, run: &str) -> Result<bool, PersistError> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM runs r WHERE run_id=?1 AND state='DISCOVERING' AND spec_id IS NULL \
        AND EXISTS(SELECT 1 FROM events WHERE run_id=r.run_id AND kind='spec.draft_requested'))",[run],|r|r.get(0)).map_err(sqlite_error)
}
