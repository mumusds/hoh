//! Approved specifications and their derived rows (REQ-034; ADR-033).
//! The host supplies bytes; this module owns their command transaction.

mod tasks;
pub(super) use tasks::replace_after_import;

use super::commands::SubmitCommand;
use super::writer::{self, Change, Commit, Ctx, Entity, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{ApproveSpec, ProposeSpecChange, RunState, SpecDecision};
use crate::specindex::{self, Index, IndexError, Source};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// A workspace snapshot read by the host, including the existing bytes and their Git tracking identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SpecFiles {
    pub sources: Vec<Source>,
    pub existing_index: Option<Vec<u8>>,
    pub tracked_index: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecAction {
    Approve(ApproveSpec),
    Propose(ProposeSpecChange),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecRequest {
    /// Observed by the host for this request; never supplied by a client.
    pub origin: crate::origin::Origin,
    pub action: SpecAction,
    pub files: Result<SpecFiles, IndexError>,
}

/// The same guard is used before replay and at the approval application point.
pub(super) fn guard(input: &SpecRequest) -> Result<(), PersistError> {
    if matches!(input.action, SpecAction::Approve(_))
        && let Some(reason) = input.origin.refusal()
    {
        return Err(PersistError::SpecApprovalNotUser(reason));
    }
    Ok(())
}

struct Approved {
    id: String,
    revision: i64,
    sha256: String,
}

fn latest(tx: &Transaction<'_>, project: &str) -> Result<Option<Approved>, PersistError> {
    tx.query_row(
        "SELECT spec_id,revision,sha256 FROM specs WHERE project_id=?1 ORDER BY revision DESC LIMIT 1",
        params![project],
        |r| {
            Ok(Approved {
                id: r.get(0)?,
                revision: r.get(1)?,
                sha256: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(sqlite_error)
}

pub(super) fn apply(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    input: &SpecRequest,
    ctx: &Ctx<'_>,
) -> Result<(i64, i64), PersistError> {
    guard(input)?;
    let run: Option<(String, i64, String)> = tx
        .query_row(
            "SELECT state,revision,project_id FROM runs WHERE run_id=?1",
            params![command.run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, project)) = run else {
        return Err(PersistError::RunNotFound(command.run_id.clone()));
    };
    if writer::parse::<RunState>(&state)?.is_terminal() {
        return Err(PersistError::BadRequest(
            "an ended run cannot change specifications".into(),
        ));
    }
    if project != command.event.project_id {
        return Err(PersistError::BadRequest(
            "run belongs to another project".into(),
        ));
    }
    if revision != command.expected_revision {
        return Err(PersistError::StaleRevision {
            entity: Entity::Run,
            id: command.run_id.clone(),
            expected: command.expected_revision,
            actual: Some(revision),
        });
    }
    let files = input
        .files
        .as_ref()
        .map_err(|e| PersistError::SpecInvalid(e.clone()))?;
    let index = specindex::generate_index(&files.sources).map_err(PersistError::SpecInvalid)?;
    let expected = match &input.action {
        SpecAction::Approve(a) => &a.spec_sha256,
        SpecAction::Propose(p) => &p.spec_sha256,
    };
    let actual = index.sha256();
    let tracked_differs = matches!(input.action, SpecAction::Approve(_))
        && files.tracked_index.as_deref() != Some(index.bytes());
    if expected != &actual || tracked_differs {
        let mut details = json!({"expected":expected,"actual":actual});
        if tracked_differs {
            details["tracked"] = json!(files.tracked_index.as_deref().map(specindex::digest));
        }
        return Err(PersistError::SpecConflict(Box::new(details)));
    }
    // Every decision cites a source the project holds (REQ-030 behaviour 5), checked before anything is written; a
    // REJECT decides on a CR and states nothing.
    if !matches!(&input.action, SpecAction::Approve(a) if a.decision == SpecDecision::Reject) {
        super::spec_sources::check(tx, &project, index.decisions())?;
    }
    let question = super::draft_approval::pending(tx, &command.run_id)?;
    if let (Some(q), SpecAction::Approve(approval)) = (&question, &input.action)
        && q.anchor["spec_sha256"].as_str() != Some(approval.spec_sha256.as_str())
    {
        return Err(PersistError::SpecConflict(Box::new(
            json!({"expected":q.anchor["spec_sha256"],"actual":approval.spec_sha256}),
        )));
    }
    let base = latest(tx, &project)?;
    if let (Some(q), SpecAction::Approve(approval)) = (&question, &input.action)
        && approval.decision != SpecDecision::Reject
        && q.anchor["base_spec_id"].as_str() != base.as_ref().map(|b| b.id.as_str())
    {
        let (new_base, mut events) = draft_proposal(
            tx,
            &project,
            &command.run_id,
            &command.command_id,
            files,
            ctx,
        )?;
        let mut event = command.event.clone();
        event.event_id = child_id(&command.command_id, "draft/reconfirm");
        event.kind = "question.asked".into();
        event.payload = q.anchor.clone();
        event.payload["question_id"] = json!(event.event_id);
        event.payload["base_spec_id"] = json!(new_base);
        event.payload["text"] = json!(
            "项目已批准了另一修订，草稿已按当前 base 重新提出。请重新确认；旧批准未生效。若内容相同，可退回修改并说明无需变更。"
        );
        events.push(event);
        let done = writer::write(
            tx,
            &Commit {
                cas: vec![StateCas {
                    id: command.run_id.clone(),
                    expected_revision: revision,
                    change: Change::Touch(Entity::Run),
                }],
                events,
                ..Commit::default()
            },
            ctx,
        )?;
        return Ok((done.revisions[0], done.event_seqs[0]));
    }
    let mut events = match &input.action {
        SpecAction::Propose(proposal) => {
            let base = base.as_ref().ok_or_else(|| {
                PersistError::BadRequest(
                    "the first specification uses ApproveSpec, without a CR".into(),
                )
            })?;
            propose(tx, command, proposal, files, &index, base)?
        }
        SpecAction::Approve(approval) => approve(tx, command, approval, &index, base.as_ref())?,
    };
    // Accept the completed draft before migrating non-terminal tasks. Otherwise migrate's saved Task revision
    // would be stale after IMPLEMENTED -> VERIFYING -> ACCEPTED, and every CR approval would roll back.
    let draft_follow = if let (Some(q), SpecAction::Approve(approval)) = (&question, &input.action)
    {
        Some(super::draft_approval::answered(
            tx, command, q, approval, ctx,
        )?)
    } else {
        None
    };
    // Approvals may update other non-terminal runs (Q3). Their touches fence decisions against the old revision.
    let mut cas = vec![StateCas {
        id: command.run_id.clone(),
        expected_revision: revision,
        change: Change::Touch(Entity::Run),
    }];
    if matches!(&input.action, SpecAction::Approve(a) if a.decision != SpecDecision::Reject) {
        let id = child_id(&command.command_id, "spec");
        let rows = tx.prepare("SELECT r.run_id,r.revision,r.state FROM runs r LEFT JOIN specs s ON s.spec_id=r.spec_id WHERE r.project_id=?1 AND (r.run_id=?2 OR s.revision<=?3) ORDER BY r.run_id").map_err(sqlite_error)?
            .query_map(params![project,command.run_id,base.as_ref().map_or(0,|b| b.revision)], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?)))
            .map_err(sqlite_error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite_error)?;
        for (run, revision, state) in rows {
            if writer::parse::<RunState>(&state)?.is_terminal() {
                continue;
            }
            tx.execute(
                "UPDATE runs SET spec_id=?1 WHERE run_id=?2",
                params![id, run],
            )
            .map_err(sqlite_error)?;
            if run != command.run_id {
                super::hold::command(tx, &run)?;
                cas.push(StateCas {
                    id: run.clone(),
                    expected_revision: revision,
                    change: Change::Touch(Entity::Run),
                });
                let mut event = events[0].clone();
                event.event_id = child_id(&command.command_id, &format!("spec/changed/{run}"));
                event.run_id = Some(run.clone());
                event.payload["run_id"] = json!(run);
                events.push(event);
            }
            if let Some(base) = &base {
                let migration = tasks::migrate(tx, &run, (&id, base.revision + 1), &events[0])?;
                cas.extend(migration.cas);
                events.extend(migration.events);
                events.extend(super::recovery::spec_changed(
                    tx,
                    &run,
                    base.revision + 1,
                    &command.event,
                )?);
            }
        }
    }
    if let Some(follow) = draft_follow {
        if follow.cas.iter().any(|c| c.id == command.run_id) {
            cas.retain(|c| c.id != command.run_id);
        }
        cas.extend(follow.cas);
        events.extend(follow.events);
    } else if matches!(&input.action, SpecAction::Approve(a) if a.decision == SpecDecision::ApproveAndBuild)
    {
        super::modes::intent(
            tx,
            &command.run_id,
            &command.command_id,
            crate::protocol::Mode::Build,
            ctx,
        )?;
    }
    let done = writer::write(
        tx,
        &Commit {
            cas,
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    let revision = tx
        .query_row(
            "SELECT revision FROM runs WHERE run_id=?1",
            [&command.run_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    Ok((revision, done.event_seqs[0]))
}

fn propose(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    proposal: &ProposeSpecChange,
    files: &SpecFiles,
    index: &Index,
    base: &Approved,
) -> Result<Vec<NewEvent>, PersistError> {
    let mut event = command.event.clone();
    if index.sha256() == base.sha256 {
        // Regeneration repairs a generated file, but does not revise the approved specification.
        event.kind = "spec.regenerated".into();
        event.payload = json!({"command_id":command.command_id,"run_id":command.run_id,
            "spec_revision":base.revision,"spec_sha256":base.sha256});
    } else {
        let old = indexed(tx, &base.id)?;
        let mappings = tx.prepare("SELECT task_id,requirement_id FROM task_requirements WHERE spec_id=?1 ORDER BY task_id,requirement_id").map_err(sqlite_error)?
            .query_map(params![base.id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
            .map_err(sqlite_error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite_error)?;
        let impact = specindex::impact(&old, index.value(), &mappings);
        let id = child_id(&command.command_id, "cr");
        tx.execute("INSERT INTO change_requests(cr_id,project_id,base_spec_id,state,reason,impact_json,created_at) VALUES(?1,?2,?3,'PROPOSED',?4,?5,?6)",
            params![id,command.event.project_id,base.id,proposal.reason,impact.to_string(),crate::time::now_rfc3339()]).map_err(sqlite_error)?;
        // The locked CR row has no proposal digest column. This durable event binds it without changing Q8's shape.
        event.kind = "spec.proposed".into();
        event.payload = json!({"command_id":command.command_id,"run_id":command.run_id,"cr_id":id,"base_spec_id":base.id,
            "spec_revision":base.revision,"spec_sha256":index.sha256()});
    }
    let mut events = vec![event];
    if let Some(mut payload) = index.drift(files.existing_index.as_deref())
        && !known_digest(
            tx,
            &command.event.project_id,
            files.existing_index.as_deref(),
        )?
    {
        payload["spec_revision"] = json!(base.revision);
        payload["command_id"] = json!(command.command_id);
        let mut drift = command.event.clone();
        drift.event_id = child_id(&command.event.event_id, "generated-drift");
        drift.kind = "spec.generated_drift".into();
        drift.payload = payload;
        events.push(drift);
    }
    Ok(events)
}

/// A previously generated index can lag behind Markdown without being manually edited.
fn known_digest(
    tx: &Transaction<'_>,
    project: &str,
    bytes: Option<&[u8]>,
) -> Result<bool, PersistError> {
    let Some(bytes) = bytes else {
        return Ok(false);
    };
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM specs WHERE project_id=?1 AND sha256=?2)
         OR EXISTS(SELECT 1 FROM change_requests c JOIN events e ON json_extract(e.payload_json,'$.cr_id')=c.cr_id
                   WHERE c.project_id=?1 AND e.project_id=?1 AND e.kind='spec.proposed'
                     AND json_extract(e.payload_json,'$.spec_sha256')=?2)",
        params![project, specindex::digest(bytes)], |r| r.get(0),
    ).map_err(sqlite_error)
}

fn approve(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    approval: &ApproveSpec,
    index: &Index,
    base: Option<&Approved>,
) -> Result<Vec<NewEvent>, PersistError> {
    let mut cr = None;
    if let Some(base) = base {
        let proposed: Option<(String,String)> = tx.query_row(
            "SELECT c.cr_id,c.base_spec_id FROM change_requests c JOIN events e ON json_extract(e.payload_json,'$.cr_id')=c.cr_id WHERE c.project_id=?1 AND c.state='PROPOSED' AND e.kind='spec.proposed' AND json_extract(e.payload_json,'$.spec_sha256')=?2 ORDER BY e.seq DESC LIMIT 1",
            params![command.event.project_id,index.sha256()], |r| Ok((r.get(0)?,r.get(1)?))).optional().map_err(sqlite_error)?;
        if proposed.is_none()
            && approval.decision == SpecDecision::Reject
            && super::draft_approval::pending(tx, &command.run_id)?.is_some()
        {
            let mut event = command.event.clone();
            event.kind = "spec.draft_rejected".into();
            event.payload = json!({"command_id":command.command_id,"note":approval.note});
            return Ok(vec![event]);
        }
        let Some((id, from)) = proposed else {
            return Err(PersistError::SpecConflict(Box::new(
                json!({"expected":approval.spec_sha256,"actual":index.sha256(),"reason":"no proposed change for this digest"}),
            )));
        };
        if from != base.id {
            return Err(PersistError::SpecConflict(Box::new(
                json!({"expected":approval.spec_sha256,"actual":index.sha256(),"expected_base":from,"actual_base":base.id}),
            )));
        }
        if approval.decision != SpecDecision::Reject && index.sha256() == base.sha256 {
            return Err(PersistError::SpecConflict(Box::new(json!({
                "expected":approval.spec_sha256,"actual":base.sha256,
                "reason":"the proposed result equals its approved base"
            }))));
        }
        cr = Some(id);
    }
    let mut event = command.event.clone();
    if approval.decision == SpecDecision::Reject {
        if cr.is_none() && super::draft_approval::pending(tx, &command.run_id)?.is_none() {
            return Err(PersistError::BadRequest(
                "there is no proposed change to reject".into(),
            ));
        }
        tx.execute("UPDATE change_requests SET state='REJECTED',decided_by=?1 WHERE cr_id=?2 AND state='PROPOSED'",params![command.command_id,cr]).map_err(sqlite_error)?;
        event.kind = "spec.rejected".into();
        event.payload = json!({"command_id":command.command_id,"cr_id":cr,"spec_revision":base.map_or(0,|b| b.revision)});
    } else {
        let revision = base.map_or(1, |b| b.revision + 1);
        let id = child_id(&command.command_id, "spec");
        insert(tx, &command.event.project_id, &id, revision, index)?;
        if let Some(cr) = &cr {
            tx.execute("UPDATE change_requests SET state='APPLIED',result_spec_id=?1,decided_by=?2 WHERE cr_id=?3 AND state='PROPOSED'",params![id,command.command_id,cr]).map_err(sqlite_error)?;
        }
        event.kind = "spec.changed".into();
        event.payload = json!({"command_id":command.command_id,"run_id":command.run_id,"spec_id":id,"spec_revision":revision,
            "spec_sha256":index.sha256(),"cause":if base.is_some() {"changed"} else {"approved"},"from":base.map(|b| b.revision)});
        if let Some(cr) = cr {
            event.payload["cr_id"] = json!(cr);
            event.payload["to"] = json!(revision);
        }
        // The new revision's open questions (docs/adr/ADR-035.md decision 1), after its spec.changed: the first event
        // stays the one other runs' copies are made from.
        let questions =
            super::readiness::open_questions_event(&event, &id, revision, index.questions());
        return Ok(std::iter::once(event).chain(questions).collect());
    }
    Ok(vec![event])
}

/// A parse failure commits only a diagnostic. The rejected command gets no accepted row or Run touch.
pub(super) fn failed(
    tx: &Transaction<'_>,
    command: &SubmitCommand,
    error: &IndexError,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let mut event = command.event.clone();
    event.kind = "spec.index_failed".into();
    event.payload = json!({"command_id":command.command_id,"path":error.path,"line":error.line,"error":error.error,
        "spec_revision":latest(tx,&command.event.project_id)?.map_or(0,|b| b.revision)});
    writer::write(
        tx,
        &Commit {
            events: vec![event],
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(())
}

/// Reconstruct only the approved semantic index from its derived rows; source-byte digests are not impact edges.
pub(super) fn indexed(tx: &Connection, spec: &str) -> Result<Value, PersistError> {
    let rows = tx.prepare("SELECT requirement_id,title,release_class FROM requirements WHERE spec_id=?1 ORDER BY requirement_id").map_err(sqlite_error)?
        .query_map(params![spec],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))
        .map_err(sqlite_error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite_error)?;
    let mut requirements = Vec::new();
    let mut scenarios = Vec::new();
    for (id, title, class) in rows {
        let criteria=tx.prepare("SELECT ac_id,body,check_id FROM acceptance_criteria WHERE spec_id=?1 AND requirement_id=?2 ORDER BY ac_id").map_err(sqlite_error)?
            .query_map(params![spec,id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))
            .map_err(sqlite_error)?.collect::<Result<Vec<_>,_>>().map_err(sqlite_error)?;
        let mut acceptance = Vec::new();
        for (ac, body, check) in criteria {
            let body: Value = serde_json::from_str(&body).map_err(|e| {
                PersistError::IntegrityViolation(format!(
                    "approved AC {ac} is not indexed JSON: {e}"
                ))
            })?;
            scenarios.push(json!({"id":body["test_id"],"requirement_id":id,"acceptance_id":ac,"check_id":check}));
            acceptance.push(body);
        }
        requirements.push(json!({"id":id,"title":title,"release_class":class,"release_targets":["project"],"acceptance":acceptance}));
    }
    scenarios.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    Ok(json!({"requirements":requirements,"scenarios":scenarios}))
}

fn insert(
    tx: &Transaction<'_>,
    project: &str,
    id: &str,
    revision: i64,
    index: &Index,
) -> Result<(), PersistError> {
    tx.execute(
        "INSERT INTO specs(spec_id,project_id,sha256,revision) VALUES(?1,?2,?3,?4)",
        params![id, project, index.sha256(), revision],
    )
    .map_err(sqlite_error)?;
    // Only the pure parser constructs Index; these arrays and strings are guaranteed by its grammar.
    for requirement in index.value()["requirements"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let text = |key: &str| requirement[key].as_str().unwrap_or_default();
        tx.execute("INSERT INTO requirements(requirement_id,spec_id,title,required_release,release_class) VALUES(?1,?2,?3,1,?4)",
            params![text("id"), id, text("title"), text("release_class")]).map_err(sqlite_error)?;
        for ac in requirement["acceptance"].as_array().into_iter().flatten() {
            let ac_id = ac["id"].as_str().unwrap_or_default();
            let check = index.value()["scenarios"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|s| s["acceptance_id"] == ac["id"])
                .and_then(|s| s["check_id"].as_str())
                .ok_or_else(|| {
                    PersistError::IntegrityViolation(format!("missing scenario for {ac_id}"))
                })?;
            tx.execute("INSERT INTO acceptance_criteria(ac_id,requirement_id,spec_id,body,check_id) VALUES(?1,?2,?3,?4,?5)",
                params![ac_id,text("id"),id,ac.to_string(),check]).map_err(sqlite_error)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;

/// A completed draft based on the latest approved revision becomes a CR through the same proposal writer.
pub(super) fn draft_proposal(
    tx: &Transaction<'_>,
    project: &str,
    run: &str,
    id: &str,
    files: &SpecFiles,
    ctx: &Ctx<'_>,
) -> Result<(Option<String>, Vec<NewEvent>), PersistError> {
    let Some(base) = latest(tx, project)? else {
        return Ok((None, Vec::new()));
    };
    let current: Option<String> = tx
        .query_row("SELECT spec_id FROM runs WHERE run_id=?1", [run], |r| {
            r.get(0)
        })
        .map_err(sqlite_error)?;
    if current.as_deref() != Some(&base.id) {
        tx.execute(
            "UPDATE runs SET spec_id=?1 WHERE run_id=?2",
            params![base.id, run],
        )
        .map_err(sqlite_error)?;
        writer::write(
            tx,
            &Commit {
                events: vec![NewEvent {
                    intent: super::writer::Intent::Progress,
                    project_id: project.into(),
                    run_id: Some(run.into()),
                    event_id: child_id(id, "draft/adopted"),
                    kind: "spec.changed".into(),
                    payload: json!({"cause":"adopted","run_id":run,"spec_id":base.id,"spec_revision":base.revision,"spec_sha256":base.sha256}),
                }],
                ..Commit::default()
            },
            ctx,
        )?;
    }
    let index = specindex::generate_index(&files.sources).map_err(PersistError::SpecInvalid)?;
    let proposal = ProposeSpecChange {
        reason: "Plan 起草，请用户确认当前 base".into(),
        spec_sha256: index.sha256(),
    };
    let command = SubmitCommand {
        command_id: id.into(),
        idempotency_key: id.into(),
        kind: "ProposeSpecChange".into(),
        run_id: run.into(),
        expected_revision: 0,
        payload: json!(proposal),
        payload_sha256: index.sha256(),
        target: super::RunTarget::Specification(Box::new(SpecRequest {
            origin: crate::origin::Origin::Unknown,
            action: SpecAction::Propose(proposal.clone()),
            files: Ok(files.clone()),
        })),
        event: NewEvent {
            intent: super::writer::Intent::Progress,
            project_id: project.into(),
            run_id: Some(run.into()),
            event_id: child_id(id, "cr"),
            kind: "spec.proposed".into(),
            payload: json!({}),
        },
        outbox: None,
    };
    let events = propose(tx, &command, &proposal, files, &index, &base)?;
    Ok((Some(base.id), events))
}
