//! Open questions and their answers (docs/adr/ADR-031.md decisions 6 to 8, correction A, E4, E5, E13, E16).
//!
//! A question is no row: [`open`] derives a run's from what holds it — the Commander's (a claim
//! `commander:<decision_id>` on its PROPOSED request), the Runtime's run-level ones (a claim `runtime:<reason>`, asked
//! by its `question.asked`) and one per task BLOCKED (NEED_DECISION), defined by its anchor `need_decision`: the
//! latest refusal of a repair while one stands (`refusal`), else the latest one of the task. Its reason is the
//! anchor's, never rewritten; its route is [`route`]'s (total over the typed reasons: every question that can be
//! written has an exit). [`answer`] applies one, in the command's transaction.

use super::blockers::{self, Claims};
use super::need::{self, RunReason, SourceReason, TaskReason, VerifierReason, WorkReason};
use super::writer::{self, Change, Commit, Ctx, Intent, NewEvent, StateCas};
use super::{PersistError, sqlite_error};
use crate::ids::child_id;
use crate::lifecycle::Lifecycle as _;
use crate::protocol::{BlockedReason, RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};

/// A redispatch of a verifier task (decision 8's verifier rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Redispatch {
    /// READY again, a new attempt and a new native session.
    Plain,
    /// A rejected report: as SESSION_NOT_INDEPENDENT when its reasons name the role's bound HoH session, else plain.
    Report,
    /// Its round's handoff rebuilt first (INVALID_HANDOFF).
    RebuildHandoff,
    /// Its role moved to its next epoch's independent session first (SESSION_NOT_INDEPENDENT).
    Rebind,
}

/// A question's exit (decision 8's table), by its reason and its candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// The Commander's own question: its answer applies its request (decision 7).
    Commander,
    /// `runtime:no_spec`: the project's specification adopted (decision 1's exit, `adoption::adopt_exit`).
    Adopt,
    /// `runtime:stalled`: retry or cancel.
    Stalled,
    /// A work task's own end: continue or cancel.
    Continue,
    /// A work task's repairs used up: rework or cancel.
    Rework,
    /// A source on a candidate still VERIFYING (join gate, reverifications used up): reverify or rework.
    Reverify,
    /// A source on a flaky candidate, or one no longer VERIFYING: rework.
    SourceRework,
    /// A verifier task: redispatch.
    Redispatch(Redispatch),
}

impl Route {
    /// Its choices (id, label), the first the default; the Commander's are its own.
    pub fn choices(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Route::Commander => &[],
            Route::Adopt => &[("adopt", "采用项目已批准的规格")],
            Route::Stalled => &[
                ("retry", "让 Commander 再试（可附指示）"),
                ("cancel", "取消本 Run"),
            ],
            Route::Continue => &[("continue", "继续（再试一次）"), ("cancel", "取消该 Task")],
            Route::Rework => &[("rework", "返工"), ("cancel", "放弃失败项并取消该 Task")],
            Route::Reverify => &[
                ("reverify", "在同一候选上重新验证"),
                ("rework", "返工（新的提交）"),
            ],
            Route::SourceRework => &[("rework", "返工（新的提交）")],
            Route::Redispatch(_) => &[("redispatch", "重新派发")],
        }
    }
}

/// The exit of task-level reason `reason`, its candidate VERIFYING or not (correction A's guard (3)): exhaustive over
/// the typed reasons, no wildcard; a new variant does not compile until it is routed.
pub fn route(reason: TaskReason, candidate_verifying: bool) -> Route {
    match reason {
        TaskReason::Work(work) => match work {
            WorkReason::RetriesExhausted
            | WorkReason::RefusedDeterministically
            | WorkReason::AgentFailed
            | WorkReason::AgentBlocked
            | WorkReason::AgentIncomplete
            | WorkReason::FreezeFailed
            | WorkReason::NoAcceptanceCriteria
            | WorkReason::VerificationTeamMissing
            | WorkReason::SpecMismatch => Route::Continue,
            WorkReason::LocalRepairsExhausted | WorkReason::ProtocolRepairsExhausted => {
                Route::Rework
            }
        },
        TaskReason::Source(source) => match (source, candidate_verifying) {
            (SourceReason::JoinGateRefused | SourceReason::ReverifyExhausted, true) => {
                Route::Reverify
            }
            (SourceReason::Flaky, true)
            | (
                SourceReason::JoinGateRefused
                | SourceReason::ReverifyExhausted
                | SourceReason::Flaky,
                false,
            ) => Route::SourceRework,
        },
        TaskReason::Verifier(verifier) => Route::Redispatch(match verifier {
            VerifierReason::RetriesExhausted
            | VerifierReason::RefusedDeterministically
            | VerifierReason::ProtocolRepairsExhausted
            | VerifierReason::ReverifyExhausted
            | VerifierReason::VerifierUnknownEffect => Redispatch::Plain,
            VerifierReason::ReportRejected => Redispatch::Report,
            VerifierReason::InvalidHandoff => Redispatch::RebuildHandoff,
            VerifierReason::SessionNotIndependent => Redispatch::Rebind,
        }),
    }
}

/// The exit of run-level reason `reason` (correction A's guard (3)).
pub fn route_run(reason: RunReason) -> Route {
    match reason {
        RunReason::NoSpec => Route::Adopt,
        RunReason::Stalled => Route::Stalled,
    }
}

/// An open question (decision 6).
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub question_id: String,
    /// The seq it appeared at: questions are listed in that order.
    pub seq: i64,
    /// `commander` or `runtime`.
    pub source: &'static str,
    /// `run`, `work`, `source` or `verifier`; none for the Commander's.
    pub subject: Option<&'static str>,
    pub reason: Option<String>,
    pub task_id: Option<String>,
    /// A repair refusal of its task stands (decision 6).
    pub refusal: bool,
    pub round_id: Option<String>,
    pub round_root: Option<String>,
    pub join_id: Option<String>,
    pub candidate_id: Option<String>,
    /// A source's current round and the state of that round's candidate.
    pub current_round: Option<String>,
    pub candidate_state: Option<String>,
    pub text: String,
    /// (choice_id, label).
    pub choices: Vec<(String, String)>,
    /// The Runtime's first choice; none for the Commander's (a free answer).
    pub default: Option<String>,
    pub route: Route,
    /// The claim that holds it (run-level and the Commander's).
    pub claim: Option<String>,
    /// Its anchor's payload (task-level), or its `question.asked`'s.
    pub anchor: Value,
}

impl Question {
    /// As getRun's `projection.questions` shows it (decision 6).
    pub fn json(&self) -> Value {
        json!({
            "question_id": self.question_id,
            "source": self.source,
            "task_id": self.task_id,
            "subject": self.subject,
            "reason": self.reason,
            "refusal": self.refusal,
            "round_id": self.round_id,
            "round_root": self.round_root,
            "join_id": self.join_id,
            "candidate_id": self.candidate_id,
            "current_round": self.current_round,
            "candidate_state": self.candidate_state,
            "text": self.text,
            "choices": self.choices.iter().map(|(id, label)| json!({"choice_id": id, "label": label}))
                .collect::<Vec<_>>(),
            "default": self.default,
        })
    }
}

/// Run `run_id`'s open questions in the order they appeared (decision 6), read on `conn` (a writer's transaction or a
/// reader's).
pub fn open(conn: &Connection, run_id: &str) -> Result<Vec<Question>, PersistError> {
    let mut out = Vec::new();
    // The run's claims: the Commander's requests and the Runtime's run-level questions.
    let claims = Claims::load(conn, "", run_id)?;
    for source in claims.held(BlockedReason::NeedDecision) {
        if let Some(decision_id) = source.strip_prefix("commander:") {
            if let Some(question) = commander(conn, run_id, &source, decision_id)? {
                out.push(question);
            }
        } else if let Some(reason) = source.strip_prefix("runtime:").and_then(RunReason::parse)
            && let Some(question) = runtime(conn, run_id, &source, reason)?
        {
            out.push(question);
        }
    }
    // Every task BLOCKED (NEED_DECISION), by its anchor.
    let blocked: Vec<String> = {
        let mut statement = conn
            .prepare(
                "SELECT task_id FROM tasks WHERE run_id = ?1 AND state = 'BLOCKED' \
                 AND blocked_reason = 'NEED_DECISION' ORDER BY created_seq, task_id",
            )
            .map_err(sqlite_error)?;
        statement
            .query_map(params![run_id], |r| r.get(0))
            .map_err(sqlite_error)?
            .collect::<Result<_, _>>()
            .map_err(sqlite_error)?
    };
    if !blocked.is_empty() {
        let refused = super::verification::refused(conn, run_id)?;
        // The run's rounds, read once and only for a source's question (its current round).
        let mut rounds = None;
        for task in blocked {
            if let Some(question) = task_question(conn, run_id, &task, &refused, &mut rounds)? {
                out.push(question);
            }
        }
    }
    out.sort_by_key(|q| q.seq);
    Ok(out)
}

/// The Commander's question of PROPOSED request `decision_id`, held by claim `source`.
fn commander(
    conn: &Connection,
    run_id: &str,
    source: &str,
    decision_id: &str,
) -> Result<Option<Question>, PersistError> {
    let row: Option<(String, String)> = conn
        .query_row(
            "SELECT payload_json, state FROM decisions WHERE decision_id = ?1 AND run_id = ?2",
            params![decision_id, run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((payload, state)) = row else {
        return Ok(None);
    };
    if state != "PROPOSED" {
        return Ok(None);
    }
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("decision {decision_id}: {e}")))?;
    let seq = asked_seq(conn, run_id, decision_id)?;
    Ok(Some(Question {
        question_id: decision_id.to_owned(),
        seq,
        source: "commander",
        subject: None,
        reason: None,
        task_id: None,
        refusal: false,
        round_id: None,
        round_root: None,
        join_id: None,
        candidate_id: None,
        current_round: None,
        candidate_state: None,
        text: payload["question"].as_str().unwrap_or_default().to_owned(),
        choices: payload["choices"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| {
                Some((
                    c["choice_id"].as_str()?.to_owned(),
                    c["label"].as_str()?.to_owned(),
                ))
            })
            .collect(),
        default: None,
        route: Route::Commander,
        claim: Some(source.to_owned()),
        anchor: payload,
    }))
}

/// The seq of the `question.asked` of question `question_id`, else of its claim (a request asked before M2-T13).
fn asked_seq(conn: &Connection, run_id: &str, question_id: &str) -> Result<i64, PersistError> {
    let seq: Option<i64> = conn
        .query_row(
            "SELECT min(seq) FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND json_extract(payload_json, '$.question_id') = ?3",
            params![run_id, need::QUESTION_ASKED, question_id],
            |r| r.get(0),
        )
        .map_err(sqlite_error)?;
    if let Some(seq) = seq {
        return Ok(seq);
    }
    conn.query_row(
        "SELECT coalesce(max(seq), 0) FROM events WHERE run_id = ?1 AND kind = 'run.blocker_claimed' \
         AND json_extract(payload_json, '$.source') = ?2",
        params![run_id, format!("commander:{question_id}")],
        |r| r.get(0),
    )
    .map_err(sqlite_error)
}

/// The Runtime's run-level question of `reason`, held by claim `source`: its latest `question.asked`.
fn runtime(
    conn: &Connection,
    run_id: &str,
    source: &str,
    reason: RunReason,
) -> Result<Option<Question>, PersistError> {
    let row: Option<(i64, String, String)> = conn
        .query_row(
            "SELECT seq, event_id, payload_json FROM events WHERE run_id = ?1 AND kind = ?2 \
             AND json_extract(payload_json, '$.reason') = ?3 ORDER BY seq DESC LIMIT 1",
            params![run_id, need::QUESTION_ASKED, reason.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((seq, event_id, payload)) = row else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("question {event_id}: {e}")))?;
    let route = route_run(reason);
    Ok(Some(Question {
        question_id: event_id,
        seq,
        source: "runtime",
        subject: Some("run"),
        reason: Some(reason.as_str().to_owned()),
        task_id: None,
        refusal: false,
        round_id: None,
        round_root: None,
        join_id: None,
        candidate_id: None,
        current_round: None,
        candidate_state: None,
        text: payload["text"].as_str().unwrap_or_default().to_owned(),
        choices: owned(route.choices()),
        default: route.choices().first().map(|(id, _)| (*id).to_owned()),
        route,
        claim: Some(source.to_owned()),
        anchor: payload,
    }))
}

fn owned(choices: &[(&str, &str)]) -> Vec<(String, String)> {
    choices
        .iter()
        .map(|(id, label)| ((*id).to_owned(), (*label).to_owned()))
        .collect()
}

/// Task `task`'s question, by its anchor (decision 6): with a repair refusal standing, its latest refusal; else its
/// latest `need_decision` (a re-block while refused is never one).
fn task_question(
    conn: &Connection,
    run_id: &str,
    task: &str,
    refused: &std::collections::BTreeSet<String>,
    rounds: &mut Option<Vec<super::closing::Round>>,
) -> Result<Option<Question>, PersistError> {
    let refusal = refused.contains(task);
    let row: Option<(i64, String, String)> = conn
        .query_row(
            &format!(
                "SELECT seq, event_id, payload_json FROM events n WHERE run_id = ?1 AND kind = 'need_decision' \
                 AND json_extract(payload_json, '$.task_id') = ?2 AND {} ORDER BY seq DESC LIMIT 1",
                if refusal {
                    "json_extract(payload_json, '$.reason') IN \
                     ('local_repairs_exhausted', 'protocol_repairs_exhausted', 'reverify_exhausted') \
                     AND seq > coalesce((SELECT max(c.seq) FROM events c WHERE c.kind = 'repair.refusal_cleared' \
                       AND c.run_id = ?1 AND json_extract(c.payload_json, '$.task_id') = ?2), 0)"
                } else {
                    "json_extract(payload_json, '$.reason') <> 'repair_refused'"
                }
            ),
            params![run_id, task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    // A task blocked for a decision with no anchor (seeded by hand, or before every writer wrote one): no question.
    let Some((seq, event_id, payload)) = row else {
        return Ok(None);
    };
    let anchor: Value = serde_json::from_str(&payload)
        .map_err(|e| PersistError::IntegrityViolation(format!("need_decision {event_id}: {e}")))?;
    let verifier = need::is_verifier(conn, run_id, task)?;
    let text = anchor["reason"].as_str().unwrap_or_default();
    let reason = TaskReason::of(verifier, text).ok_or_else(|| {
        PersistError::IntegrityViolation(format!(
            "task {task}'s question {event_id} has reason {text}, which no writer writes for its subject"
        ))
    })?;
    // A source's current round and its candidate's state now.
    let (mut current_round, mut candidate_state, mut verifying) = (None, None, false);
    if let TaskReason::Source(_) = reason
        && rounds.is_none()
    {
        *rounds = Some(super::closing::rounds(conn, run_id)?);
    }
    if let TaskReason::Source(_) = reason
        && let Some(round) = rounds
            .as_deref()
            .and_then(|all| super::closing::current(all, task))
    {
        let state: Option<String> = conn
            .query_row(
                "SELECT state FROM candidates WHERE candidate_id = ?1",
                params![round.candidate],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        verifying = state.as_deref() == Some("VERIFYING");
        current_round = Some(round.id.clone());
        candidate_state = state;
    }
    let route = route(reason, verifying);
    let field = |name: &str| anchor[name].as_str().map(str::to_owned);
    let round_id = field("round_id").or_else(|| {
        verifier
            .then(|| {
                super::verification::round_of_task(conn, run_id, task)
                    .ok()
                    .flatten()
                    .map(|(round, _)| round)
            })
            .flatten()
    });
    Ok(Some(Question {
        question_id: event_id,
        seq,
        source: "runtime",
        subject: Some(reason.subject()),
        reason: Some(reason.as_str().to_owned()),
        task_id: Some(task.to_owned()),
        refusal,
        round_id,
        round_root: field("round_root"),
        join_id: field("join_id"),
        candidate_id: field("candidate_id"),
        current_round,
        candidate_state,
        text: format!(
            "Task {task} 需要你的决定（{}）：{}",
            describe(reason),
            anchor["detail"].as_str().unwrap_or_default()
        ),
        choices: owned(route.choices()),
        default: route.choices().first().map(|(id, _)| (*id).to_owned()),
        route,
        claim: None,
        anchor,
    }))
}

/// What reason `reason` says, in the product's words.
fn describe(reason: TaskReason) -> &'static str {
    match reason {
        TaskReason::Work(WorkReason::RetriesExhausted)
        | TaskReason::Verifier(VerifierReason::RetriesExhausted) => "重试次数已用尽",
        TaskReason::Work(WorkReason::RefusedDeterministically)
        | TaskReason::Verifier(VerifierReason::RefusedDeterministically) => {
            "启动前被拒绝，同样的输入会再被拒绝"
        }
        TaskReason::Work(WorkReason::ProtocolRepairsExhausted)
        | TaskReason::Verifier(VerifierReason::ProtocolRepairsExhausted) => {
            "输出格式的纠正次数已用尽"
        }
        TaskReason::Work(WorkReason::LocalRepairsExhausted) => "失败项的返工次数已用尽",
        TaskReason::Work(WorkReason::AgentFailed) => "Agent 报告失败",
        TaskReason::Work(WorkReason::AgentBlocked) => "Agent 报告受阻",
        TaskReason::Work(WorkReason::AgentIncomplete) => "Agent 报告未完成",
        TaskReason::Work(WorkReason::FreezeFailed) => "无法冻结候选",
        TaskReason::Work(WorkReason::NoAcceptanceCriteria) => "在 Run 的规格下没有验收标准",
        TaskReason::Work(WorkReason::VerificationTeamMissing) => "没有绑定 QA 或 Reviewer",
        TaskReason::Work(WorkReason::SpecMismatch) => "结果引用的规格与 Run 的不同",
        TaskReason::Source(SourceReason::JoinGateRefused) => "汇合的门禁拒绝了该 Task 的需求",
        TaskReason::Source(SourceReason::ReverifyExhausted)
        | TaskReason::Verifier(VerifierReason::ReverifyExhausted) => "重新验证的次数已用尽",
        TaskReason::Source(SourceReason::Flaky) => "该候选上有不稳定的验收标准",
        TaskReason::Verifier(VerifierReason::VerifierUnknownEffect) => "验证 Attempt 的效果未确认",
        TaskReason::Verifier(VerifierReason::InvalidHandoff) => "轮次的交接无效",
        TaskReason::Verifier(VerifierReason::SessionNotIndependent) => "验证会话不独立",
        TaskReason::Verifier(VerifierReason::ReportRejected) => "验证报告被拒收",
    }
}

/// An answer to apply (decisions 7 and 8).
pub(super) struct Answer<'a> {
    pub project_id: &'a str,
    pub run_id: &'a str,
    pub question_id: &'a str,
    pub answer: &'a str,
    pub choice_id: Option<&'a str>,
    pub command_id: &'a str,
}

/// A precondition of an exit that does not hold (decision 7): 409 ILLEGAL_TRANSITION with its details, nothing
/// written.
fn unmet(
    run_id: &str,
    from: &str,
    question_id: &str,
    precondition: &str,
    detail: String,
) -> PersistError {
    PersistError::IllegalTransition {
        entity: writer::Entity::Run,
        id: run_id.to_owned(),
        from: from.to_owned(),
        to: "answered".to_owned(),
        why: format!("问题 {question_id} 的出口前提不成立（{precondition}）：{detail}"),
        details: Some(Box::new(
            json!({"question_id": question_id, "precondition": precondition, "detail": detail}),
        )),
    }
}

/// The choice an answer takes: `choice_id` when it is one of the question's (else 400), else the choice whose id or
/// label the answer's text is (trimmed, any case), else the default (none for the Commander's: a free answer).
fn choose(
    question: &Question,
    choice_id: Option<&str>,
    text: &str,
) -> Result<Option<String>, PersistError> {
    if let Some(id) = choice_id {
        return if question.choices.iter().any(|(c, _)| c == id) {
            Ok(Some(id.to_owned()))
        } else {
            Err(PersistError::BadRequest(format!(
                "choice {id} is not one of question {}'s: {}",
                question.question_id,
                question
                    .choices
                    .iter()
                    .map(|(c, _)| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )))
        };
    }
    let said = text.trim().to_lowercase();
    Ok(question
        .choices
        .iter()
        .find(|(id, label)| id.to_lowercase() == said || label.trim().to_lowercase() == said)
        .map(|(id, _)| id.clone())
        .or_else(|| question.default.clone()))
}

/// Apply `a` in the command's transaction (decisions 7 and 8, E5): the question must be open (409 otherwise), the
/// choice one of its own (400); then its `question.answered`, its repair refusal cleared when one stands (every exit,
/// before the task moves), its exit, and the run's claims followed — the Commander's or the run-level question's
/// released, the pace's recomputed after a task's exit (`retry::stall_reasons`) — with the run's move, or its revision
/// touched. A precondition that does not hold refuses the whole command (409 with details). The exit's name.
pub(super) fn answer(
    tx: &Transaction<'_>,
    a: &Answer<'_>,
    ctx: &Ctx<'_>,
) -> Result<&'static str, PersistError> {
    let (state, revision): (String, i64) = tx
        .query_row(
            "SELECT state, revision FROM runs WHERE run_id = ?1",
            params![a.run_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sqlite_error)?;
    let Some(question) = open(tx, a.run_id)?
        .into_iter()
        .find(|q| q.question_id == a.question_id)
    else {
        return Err(PersistError::IllegalTransition {
            entity: writer::Entity::Run,
            id: a.run_id.to_owned(),
            from: state,
            to: "answered".to_owned(),
            why: format!("问题 {} 不是待答的", a.question_id),
            details: Some(Box::new(json!({"question_id": a.question_id}))),
        });
    };
    let choice = choose(&question, a.choice_id, a.answer)?;
    let picked = choice.as_deref().unwrap_or_default();
    let exit = exit_name(question.route, picked);
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        intent: Intent::Progress,
        project_id: a.project_id.to_owned(),
        event_id: id,
        run_id: Some(a.run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    let mut first = vec![event(
        child_id(a.command_id, "answered"),
        "question.answered",
        json!({"question_id": question.question_id, "source": question.source, "task_id": question.task_id,
            "subject": question.subject, "reason": question.reason, "choice_id": choice, "answer": a.answer,
            "command_id": a.command_id, "exit": exit}),
    )];
    // A repair refusal standing is cleared by every exit, before the task moves (decision 8, M2T13D-02).
    if question.refusal
        && let Some(task) = &question.task_id
    {
        first.push(super::verification::refusal_cleared(
            a.project_id,
            a.run_id,
            task,
            a.command_id,
        ));
    }
    writer::write(
        tx,
        &Commit {
            events: first,
            ..Commit::default()
        },
        ctx,
    )?;
    let at = crate::time::rfc3339_utc(ctx.now());
    let mut claims = Claims::load(tx, a.project_id, a.run_id)?;
    let mut moved_run = false;
    match question.route {
        Route::Commander => {
            let decision_id = &question.question_id;
            let changed = tx
                .execute(
                    "UPDATE decisions SET state = 'APPLIED' WHERE decision_id = ?1 AND state = 'PROPOSED'",
                    params![decision_id],
                )
                .map_err(sqlite_error)?;
            if changed != 1 {
                return Err(PersistError::IntegrityViolation(format!(
                    "request {decision_id} is no longer PROPOSED"
                )));
            }
            let mut fields = json!({"question_id": decision_id, "decision_id": decision_id,
                "answer": a.answer, "choice_id": choice, "fingerprint": format!("answer/{decision_id}")});
            if let Some(asked) = question.anchor["question_id"].as_str() {
                fields["commander_question_id"] = json!(asked);
            }
            writer::write(
                tx,
                &Commit {
                    events: vec![
                        event(
                            child_id(decision_id, "applied"),
                            "decision.applied",
                            json!({"decision_id": decision_id, "kind": "request_user_decision",
                                "answered_by": a.command_id}),
                        ),
                        need::answered(
                            a.project_id,
                            a.run_id,
                            child_id(a.command_id, need::KIND),
                            fields,
                        ),
                    ],
                    ..Commit::default()
                },
                ctx,
            )?;
            claims.set(question.claim.as_deref().unwrap_or_default(), &[]);
        }
        Route::Adopt => {
            // Decision 8's row runtime:no_spec (W13's exit, decision 1): the project's latest approved specification
            // adopted, its spec.changed written and the claim released, the run back in its resume state by the
            // follow below unless another claim holds it; still none: nothing, the run stays BLOCKED with the question
            // open, its revision touched below and the commit ending with its run.result.
            if super::adoption::adopt_exit(tx, a.project_id, a.run_id, ctx)? {
                claims.set(question.claim.as_deref().unwrap_or_default(), &[]);
            }
        }
        Route::Stalled => {
            claims.set(question.claim.as_deref().unwrap_or_default(), &[]);
            if picked == "cancel" {
                // As a CancelRun (commands.rs, RunTarget::To(CANCEL_REQUESTED)): its attempts marked (a stalled run
                // has none), then the cancel lands.
                let events = claims_events(tx, claims)?;
                tx.execute(
                    "UPDATE attempts SET cancel_requested = 1 WHERE state IN ('PREPARED', 'STARTING', 'RUNNING') \
                     AND task_id IN (SELECT task_id FROM tasks WHERE run_id = ?1)",
                    params![a.run_id],
                )
                .map_err(sqlite_error)?;
                let mut all = events;
                all.push(event(
                    child_id(a.command_id, "cancel_requested"),
                    "run.cancel_requested",
                    json!({"state": "CANCEL_REQUESTED", "command_id": a.command_id,
                        "reason": format!("问题 {} 答复 cancel", question.question_id)}),
                ));
                writer::write(
                    tx,
                    &Commit {
                        cas: vec![StateCas {
                            id: a.run_id.to_owned(),
                            expected_revision: revision,
                            change: Change::Run(RunState::CancelRequested),
                        }],
                        events: all,
                        ..Commit::default()
                    },
                    ctx,
                )?;
                return Ok(exit);
            }
            // retry: the user's words to the Commander's next turn (decision 7's mechanism). Its id is the
            // question's own (answered once), so the stall detector reads it by id (stall.rs `retry_answers`).
            let fields = json!({"question_id": question.question_id, "answer": a.answer, "choice_id": "retry",
                "fingerprint": format!("answer/{}", question.question_id), "progress": question.anchor["progress"]});
            writer::write(
                tx,
                &Commit {
                    events: vec![need::answered(
                        a.project_id,
                        a.run_id,
                        child_id(&question.question_id, "retry"),
                        fields,
                    )],
                    ..Commit::default()
                },
                ctx,
            )?;
        }
        Route::Continue
        | Route::Rework
        | Route::Reverify
        | Route::SourceRework
        | Route::Redispatch(_) => {
            task_exit(tx, a, &question, picked, ctx)?;
            // The pace's claim recomputed by its own predicate (decision 7), in every state an answer is taken in: an
            // answer that lets a task move clears it — in a stop (PAUSED, INCOMPLETE …) too, where the pace does not
            // look and a ResumeRun reads the blocker set it leaves (M2T13R-301); the run stays where it stopped.
            claims.set(
                blockers::PACE,
                &super::retry::stall_reasons(tx, a.run_id, ctx.now())?,
            );
        }
    }
    let changed = claims.changed();
    let reasons = claims.reasons();
    let events = claims.store(tx)?;
    let follow = blockers::follow(
        tx,
        a.project_id,
        a.run_id,
        changed,
        &reasons,
        &at,
        "an answer leaves the run blocked",
        ctx,
    )?;
    moved_run |= follow.cas.iter().any(|c| c.id == a.run_id);
    let mut commit = Commit {
        events,
        ..Commit::default()
    };
    commit.cas.extend(follow.cas);
    commit.events.extend(follow.events);
    if !moved_run {
        let now: i64 = tx
            .query_row(
                "SELECT revision FROM runs WHERE run_id = ?1",
                params![a.run_id],
                |r| r.get(0),
            )
            .map_err(sqlite_error)?;
        commit.cas.push(StateCas {
            id: a.run_id.to_owned(),
            expected_revision: now,
            change: Change::Touch(writer::Entity::Run),
        });
    }
    writer::write(tx, &commit, ctx)?;
    Ok(exit)
}

/// The claim events of `claims` stored, for a commit that moves the run itself.
fn claims_events(tx: &Transaction<'_>, claims: Claims) -> Result<Vec<NewEvent>, PersistError> {
    claims.store(tx)
}

/// The exit's name `question.answered` records.
fn exit_name(route: Route, choice: &str) -> &'static str {
    match (route, choice) {
        (Route::Commander, _) => "applied",
        (Route::Adopt, _) => "adopt",
        (Route::Stalled, "cancel") => "cancel",
        (Route::Stalled, _) => "retry",
        (Route::Continue | Route::Rework, "cancel") => "cancel",
        (Route::Continue, _) => "continue",
        (Route::Rework | Route::SourceRework, _) => "rework",
        (Route::Reverify, "rework") => "rework",
        (Route::Reverify, _) => "reverify",
        (Route::Redispatch(_), _) => "redispatch",
    }
}

/// A task's state and revision now.
fn task_now(tx: &Transaction<'_>, task: &str) -> Result<(TaskState, i64, String), PersistError> {
    let (state, revision, role): (String, i64, String) = tx
        .query_row(
            "SELECT state, revision, role FROM tasks WHERE task_id = ?1",
            params![task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sqlite_error)?;
    Ok((writer::parse::<TaskState>(&state)?, revision, role))
}

/// Task `task` moved from BLOCKED to `to`, its blocked reason cleared (and its layer 0 for a rework).
fn move_task(
    tx: &Transaction<'_>,
    task: &str,
    to: TaskState,
    rework: bool,
    events: Vec<NewEvent>,
    ctx: &Ctx<'_>,
) -> Result<i64, PersistError> {
    let (_, revision, _) = task_now(tx, task)?;
    tx.execute(
        &format!(
            "UPDATE tasks SET blocked_reason = NULL, next_retry_at = NULL{} WHERE task_id = ?1",
            if rework { ", layer = 0" } else { "" }
        ),
        params![task],
    )
    .map_err(sqlite_error)?;
    let done = writer::write(
        tx,
        &Commit {
            cas: vec![StateCas {
                id: task.to_owned(),
                expected_revision: revision,
                change: Change::Task(to),
            }],
            events,
            ..Commit::default()
        },
        ctx,
    )?;
    Ok(done.revisions[0])
}

/// A task-level question's exit (decision 8's rows).
fn task_exit(
    tx: &Transaction<'_>,
    a: &Answer<'_>,
    q: &Question,
    choice: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let task = q.task_id.as_deref().unwrap_or_default();
    let (state, _, role) = task_now(tx, task)?;
    if state != TaskState::Blocked {
        return Err(PersistError::IntegrityViolation(format!(
            "task {task} of an open question is {}",
            state.as_str()
        )));
    }
    let event = |id: String, kind: &str, payload: Value| NewEvent {
        intent: Intent::Progress,
        project_id: a.project_id.to_owned(),
        event_id: id,
        run_id: Some(a.run_id.to_owned()),
        kind: kind.to_owned(),
        payload,
    };
    match (q.route, choice) {
        (Route::Continue, "cancel") => {
            move_task(tx, task, TaskState::Cancelled, false, Vec::new(), ctx)?;
        }
        (Route::Rework, "cancel") => {
            let abandoned = event(
                child_id(a.command_id, "abandoned"),
                "verification.failures_abandoned",
                json!({"task_id": task, "reason": "answered", "question_id": q.question_id,
                    "command_id": a.command_id, "fingerprint": q.anchor["fingerprint"],
                    "detail": q.anchor["detail"]}),
            );
            move_task(tx, task, TaskState::Cancelled, false, vec![abandoned], ctx)?;
        }
        (Route::Continue, _) => {
            // READY again, its attempt count as it is: one more try per answer (REQ-042 behaviour 3).
            move_task(tx, task, TaskState::Ready, false, Vec::new(), ctx)?;
        }
        (Route::Rework | Route::SourceRework, _) | (Route::Reverify, "rework") => {
            move_task(tx, task, TaskState::Ready, true, Vec::new(), ctx)?;
        }
        (Route::Reverify, _) => reverify(tx, a, q, task, ctx)?,
        (Route::Redispatch(kind), _) => {
            let kind = match kind {
                Redispatch::Report if report_conflicts(tx, a.run_id, &role, &q.anchor)? => {
                    Redispatch::Rebind
                }
                Redispatch::Report => Redispatch::Plain,
                other => other,
            };
            match kind {
                Redispatch::RebuildHandoff => {
                    let round = q.round_id.as_deref().unwrap_or_default();
                    let commit = super::verification::rebuild_handoff(
                        tx,
                        a.run_id,
                        round,
                        &role,
                        a.command_id,
                        ctx,
                    )?
                    .map_err(|detail| {
                        unmet(
                            a.run_id,
                            "BLOCKED",
                            &q.question_id,
                            "rebuilt_handoff",
                            detail,
                        )
                    })?;
                    writer::write(tx, &commit, ctx)?;
                }
                Redispatch::Rebind => {
                    let round = q.round_id.as_deref().unwrap_or_default();
                    let commit =
                        super::staffing::rebind(tx, a.run_id, round, &role)?.map_err(|detail| {
                            unmet(
                                a.run_id,
                                "BLOCKED",
                                &q.question_id,
                                "independent_session",
                                detail,
                            )
                        })?;
                    writer::write(tx, &commit, ctx)?;
                }
                Redispatch::Plain | Redispatch::Report => {}
            }
            move_task(tx, task, TaskState::Ready, false, Vec::new(), ctx)?;
        }
        (Route::Commander | Route::Adopt | Route::Stalled, _) => {
            return Err(PersistError::IntegrityViolation(format!(
                "question {} is no task's",
                q.question_id
            )));
        }
    }
    Ok(())
}

/// Whether a rejected report's reasons name an HoH session conflict (SESSION_ROLE_MISMATCH, SESSION_NOT_INDEPENDENT)
/// of the session its role is bound to now (decision 8's report_rejected row): then the same session would only
/// conflict again, and the answer moves the role to its next epoch as for SESSION_NOT_INDEPENDENT.
fn report_conflicts(
    tx: &Transaction<'_>,
    run_id: &str,
    role: &str,
    anchor: &Value,
) -> Result<bool, PersistError> {
    let detail = anchor["detail"].as_str().unwrap_or_default();
    // The intake's reasons, joined: "<CODE>: session conflict: session <id> …" (reports.rs).
    let named: Vec<&str> = detail
        .split("; ")
        .filter(|r| {
            r.starts_with("SESSION_ROLE_MISMATCH:") || r.starts_with("SESSION_NOT_INDEPENDENT:")
        })
        .filter_map(|r| r.split("conflict: session ").nth(1))
        .filter_map(|rest| rest.split_whitespace().next())
        .collect();
    if named.is_empty() {
        return Ok(false);
    }
    let bound: Option<Option<String>> = tx
        .query_row(
            "SELECT session_id FROM role_bindings WHERE run_id = ?1 AND role = ?2",
            params![run_id, role],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite_error)?;
    Ok(bound
        .flatten()
        .is_some_and(|session| named.contains(&session.as_str())))
}

/// The reverify exit (decision 8): the candidate still VERIFYING and the task's current round closed, else refused;
/// then the task BLOCKED -> VERIFYING and its next round on the same candidate, its own chain's root (a new chain the
/// user authorised: REVERIFY_LIMIT counts anew), so the candidate's rounds and its join number grow by one (E6).
fn reverify(
    tx: &Transaction<'_>,
    a: &Answer<'_>,
    q: &Question,
    task: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let rounds = super::closing::rounds(tx, a.run_id)?;
    let Some(round) = super::closing::current(&rounds, task).cloned() else {
        return Err(unmet(
            a.run_id,
            "BLOCKED",
            &q.question_id,
            "reverify",
            format!("task {task} has no round"),
        ));
    };
    let candidate_state = super::closing::candidate_state(tx, &round.candidate)?;
    if candidate_state.as_deref() != Some("VERIFYING") {
        return Err(unmet(
            a.run_id,
            "BLOCKED",
            &q.question_id,
            "reverify",
            format!(
                "candidate {} is {}, not VERIFYING",
                round.candidate,
                candidate_state.unwrap_or_default()
            ),
        ));
    }
    if !super::closing::closed(tx, a.run_id)?.contains_key(&round.id) {
        return Err(unmet(
            a.run_id,
            "BLOCKED",
            &q.question_id,
            "reverify",
            format!("round {} of task {task} is not closed", round.id),
        ));
    }
    let revision = move_task(tx, task, TaskState::Verifying, false, Vec::new(), ctx)?;
    let (_, next) = super::verification::next_round(tx, a.run_id, &round.candidate)?;
    super::verification::reverify(
        tx,
        a.run_id,
        &round.candidate,
        &round.id,
        &next,
        vec![super::verification::Source {
            task_id: task.to_owned(),
            revision,
        }],
        ctx,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
