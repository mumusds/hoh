//! Runtime-owned specification drafting (REQ-008, ADR-038), outside the work scheduler.
use super::writer::{self, Change, Commit, Ctx, Entity, Intent, NewEvent, NewOutbox, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{Mode, RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

mod revisions;

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
    if revisions::step(conn, run, ctx)? {
        return Ok(true);
    }
    let read = |conn: &Connection| -> Result<Option<(String, String, i64)>, PersistError> {
        conn.query_row(
            "SELECT project_id,state,revision FROM runs r WHERE run_id=?1 AND spec_id IS NULL \
         AND EXISTS(SELECT 1 FROM events WHERE run_id=r.run_id AND kind='spec.draft_requested') \
         AND NOT EXISTS(SELECT 1 FROM tasks WHERE run_id=r.run_id AND kind='spec_draft')",
            [run],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)
    };
    // An ordinary run has no draft work. Do not open an empty writer transaction: its Begun hook would consume
    // a runner fault intended for a real receipt/effect. Re-read under the write lock before changing anything.
    if read(conn)?.is_none() {
        return Ok(false);
    }
    let tx = writer::begin(conn, ctx)?;
    let Some((project, state, revision)) = read(&tx)? else {
        return Ok(false);
    };
    let to = match state.as_str() {
        "CREATED" => RunState::Discovering,
        "DISCOVERING" if discovered && super::baseline::recorded(&tx, run)? => RunState::Specifying,
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
    let read = |conn: &Connection| {
        conn.query_row(
        "SELECT t.task_id,t.revision,r.project_id,r.revision FROM tasks t JOIN runs r ON r.run_id=t.run_id \
         WHERE t.run_id=?1 AND t.kind='spec_draft' AND t.state='READY' \
         AND (t.next_retry_at IS NULL OR t.next_retry_at<=?2) ORDER BY t.created_seq LIMIT 1",
        params![run,crate::time::rfc3339_utc(context.now())],|r|Ok((r.get::<_, String>(0)?,r.get::<_, i64>(1)?,r.get::<_, String>(2)?,r.get::<_, i64>(3)?))).optional().map_err(sqlite_error)
    };
    if read(conn)?.is_none() {
        return Ok(false);
    }
    let tx = writer::begin(conn, ctx)?;
    let Some((task, revision, project, run_revision)) = read(&tx)? else {
        return Ok(false);
    };
    let budget = super::budget_view::load(&tx, run, context.now(), context.wall())?;
    let admission = super::admission::admit(
        &tx,
        run,
        &[&task],
        super::admission::By::Draft,
        Some(super::admission::DraftContext {
            context,
            version: &ctx.resource_bundle.version(),
            budget: &budget,
        }),
    )?
    .remove(&task)
    .ok_or_else(|| PersistError::IntegrityViolation("draft admission absent".into()))?;
    let admission = match admission {
        Ok(admission) => admission,
        Err(super::admission::Refusal::DraftUnavailable(detail)) => {
            let commit = super::need::run_question(
                &tx,
                super::need::RunQuestion {
                    project_id: &project,
                    run_id: run,
                    reason: super::need::RunReason::DraftUnavailable,
                    question_id: child_id(&task, &format!("unavailable/{run_revision}")),
                    fields: json!({"task_id":task,"text":detail}),
                    at: &crate::time::rfc3339_utc(ctx.now()),
                    detail: "draft dispatch refused",
                },
                ctx,
            )?;
            writer::write(&tx, &commit, ctx)?;
            writer::commit(tx, ctx)?;
            return Ok(true);
        }
        Err(_) => return Ok(false),
    };
    let role_epoch = admission.draft_role_epoch.ok_or_else(|| {
        PersistError::IntegrityViolation("draft admission has no role epoch".into())
    })?;
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
            role_epoch,
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
                role_epoch: super::decide::to_i64(role_epoch, "role_epoch")?,
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
    prepare_effect(conn, attempt, "git-init", ctx)
}

pub(super) fn prepare_effect(
    conn: &mut Connection,
    attempt: &str,
    kind: &str,
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
            params![child_id(attempt, kind), attempt],
        )
        .map_err(sqlite_error)?;
    writer::commit(tx, ctx)?;
    Ok(n == 1)
}

/// Preparation belongs to the draft Task, across retries and host restarts. An OBSERVED
/// result is durable too; reconciliation only needs to commit its effect, not republish files.
pub(super) fn templates(conn: &Connection, attempt: &str) -> Result<Option<Value>, PersistError> {
    let text: Option<String> = conn.query_row(
        "SELECT e.payload_json FROM events e JOIN attempts a ON a.attempt_id=json_extract(e.payload_json,'$.attempt_id') \
         JOIN effects f ON f.effect_key=json_extract(e.payload_json,'$.effect_key') AND f.attempt_id=a.attempt_id \
         WHERE e.kind='templates.instantiated' AND a.task_id=(SELECT task_id FROM attempts WHERE attempt_id=?1) \
         AND f.state IN ('OBSERVED','COMMITTED') ORDER BY e.seq LIMIT 1",
        [attempt], |r| r.get(0),
    ).optional().map_err(sqlite_error)?;
    text.map(|s| serde_json::from_str(&s).map_err(|e| PersistError::Storage(e.to_string())))
        .transpose()
}

pub(super) fn template_selection(
    conn: &Connection,
    run: &str,
) -> Result<crate::template_files::Selection, PersistError> {
    let text: String = conn
        .query_row(
            "SELECT config_snapshot_json FROM runs WHERE run_id=?1",
            [run],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    let snapshot = serde_json::from_str(&text).map_err(|e| PersistError::Storage(e.to_string()))?;
    crate::template_files::Selection::from_snapshot(&snapshot)
        .map_err(|e| PersistError::Storage(e.to_string()))
}

pub(super) fn discovery_due(conn: &Connection, run: &str) -> Result<bool, PersistError> {
    if !super::baseline::recorded(conn, run)? {
        return Ok(false);
    }
    conn.query_row("SELECT EXISTS(SELECT 1 FROM runs r WHERE run_id=?1 AND state='DISCOVERING' AND spec_id IS NULL \
        AND EXISTS(SELECT 1 FROM events WHERE run_id=r.run_id AND kind='spec.draft_requested'))",[run],|r|r.get(0)).map_err(sqlite_error)
}

fn subject(tx: &Transaction<'_>, attempt: &str) -> Result<(String, String, String), PersistError> {
    tx.query_row("SELECT r.project_id,t.run_id,t.task_id FROM attempts a JOIN tasks t ON t.task_id=a.task_id JOIN runs r ON r.run_id=t.run_id WHERE a.attempt_id=?1 AND t.kind='spec_draft'",[attempt],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).map_err(sqlite_error)
}

pub(super) fn completed(
    tx: &Transaction<'_>,
    attempt: &str,
    git_commit: &str,
    files: &super::SpecFiles,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let (project, run, task) = subject(tx, attempt)?;
    let index =
        crate::specindex::generate_index(&files.sources).map_err(PersistError::SpecInvalid)?;
    if files.tracked_index.as_deref() != Some(index.bytes()) {
        return Err(PersistError::IntegrityViolation(
            "draft completion index was not committed".into(),
        ));
    }
    let command = child_id(attempt, "draft/completed");
    let (base, mut events) = super::spec::draft_proposal(tx, &project, &run, &command, files, ctx)?;
    events.push(event(&project,&run,command.clone(),"spec.draft_completed",json!({"task_id":task,"attempt_id":attempt,"spec_sha256":index.sha256(),"git_commit":git_commit,"base_spec_id":base})));
    super::draft_approval::move_task(tx, &task, TaskState::Implemented, ctx)?;
    writer::write(
        tx,
        &Commit {
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    ask(
        tx,
        &project,
        &run,
        &task,
        attempt,
        super::need::RunReason::SpecApproval,
        json!({"text":"规格已起草并提交。答复 approve 批准；其他答复退回修改。","spec_sha256":index.sha256(),"git_commit":git_commit,"base_spec_id":base}),
        ctx,
    )
}

#[allow(clippy::too_many_arguments)]
fn ask(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    task: &str,
    attempt: &str,
    reason: super::need::RunReason,
    mut fields: Value,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    fields["task_id"] = json!(task);
    fields["attempt_id"] = json!(attempt);
    let commit = super::need::run_question(
        tx,
        super::need::RunQuestion {
            project_id: project,
            run_id: run,
            reason,
            question_id: child_id(attempt, reason.as_str()),
            fields,
            at: &crate::time::rfc3339_utc(ctx.now()),
            detail: "draft needs the user",
        },
        ctx,
    )?;
    writer::write(tx, &commit, ctx)?;
    Ok(())
}

pub(super) fn blocked(
    tx: &Transaction<'_>,
    attempt: &str,
    detail: &str,
    paths: Option<&Vec<String>>,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let (project, run, task) = subject(tx, attempt)?;
    super::draft_approval::move_task(tx, &task, TaskState::Blocked, ctx)?;
    if let Some(paths) = paths {
        writer::write(
            tx,
            &Commit {
                events: vec![event(
                    &project,
                    &run,
                    child_id(attempt, "plan/write-violation"),
                    if paths.is_empty() {
                        "plan.write_observation_failed"
                    } else {
                        "plan.write_violation"
                    },
                    json!({"task_id":task,"attempt_id":attempt,"paths":paths,"detail":detail}),
                )],
                ..Commit::default()
            },
            ctx,
        )?;
    }
    ask(
        tx,
        &project,
        &run,
        &task,
        attempt,
        super::need::RunReason::DraftQuestion,
        json!({"text":detail,"paths":paths}),
        ctx,
    )
}
