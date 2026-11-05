//! The writer actor: the only read-write connection, owned by a dedicated thread (synchronous rusqlite never runs on
//! a Tokio worker). Requests arrive through a bounded queue; a caller waits at most busy_timeout for room and then
//! gets a retryable `DB_BUSY`, so nothing is dropped silently. Each [`Commit`] is one `BEGIN IMMEDIATE` transaction:
//! state compare-and-set, event appends and outbox inserts succeed together or not at all.

use super::attempts::{self, AttemptJob, AttemptMove, CancelLatch};
use super::decide::{self, Decided};
use super::{Options, PersistError, sqlite_error};
use crate::decision::{Context, Proposal};
use crate::lifecycle::AttemptState;
use crate::lifecycle::{self, Lifecycle};
use crate::protocol::{RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};

/// Entities with a state and a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Run,
    Task,
    /// Moved with `AttemptMove` (the attempts table has no revision column).
    Attempt,
    /// An `input_queue` row, moved with `InputMove` (no revision column either).
    Input,
}

impl Entity {
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Entity::Run => ("runs", "run_id"),
            Entity::Task => ("tasks", "task_id"),
            Entity::Attempt => ("attempts", "attempt_id"),
            Entity::Input => ("input_queue", "input_id"),
        }
    }
}

/// What a compare-and-set changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    /// Move the run to this state; the transition table and the resume_state rule decide whether that is legal
    /// (ADR-011 §1).
    Run(RunState),
    /// Move the task to this state, checked against the task machine.
    Task(TaskState),
    /// Grow only the revision (a decision that changes no state still orders later ones).
    Touch(Entity),
}

impl Change {
    pub fn entity(self) -> Entity {
        match self {
            Change::Run(_) => Entity::Run,
            Change::Task(_) => Entity::Task,
            Change::Touch(entity) => entity,
        }
    }
}

/// Apply `change` to entity `id` if its revision is still `expected_revision`; the revision then grows by one. An
/// illegal move fails the whole commit with `ILLEGAL_TRANSITION`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCas {
    pub id: String,
    pub expected_revision: i64,
    pub change: Change,
}

/// An event to append; its `seq` is assigned by the database.
#[derive(Debug, Clone, PartialEq)]
pub struct NewEvent {
    pub project_id: String,
    pub event_id: String,
    pub run_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// An outbox message for the event at index `event` of the same commit.
#[derive(Debug, Clone, PartialEq)]
pub struct NewOutbox {
    pub message_id: String,
    pub event: usize,
    pub run_id: Option<String>,
    pub recipient_role: String,
    pub role_epoch: i64,
    pub payload: serde_json::Value,
}

/// One atomic write.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Commit {
    pub cas: Vec<StateCas>,
    pub events: Vec<NewEvent>,
    pub outbox: Vec<NewOutbox>,
}

/// What a commit wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    /// The new revision of each compare-and-set, in order.
    pub revisions: Vec<i64>,
    /// The `seq` of each event, in order.
    pub event_seqs: Vec<i64>,
}

enum Job {
    Commit(Commit, oneshot::Sender<Result<Committed, PersistError>>),
    EnsureProject(String, String, oneshot::Sender<Result<(), PersistError>>),
    /// Project, run and task ids (and the repo path) for [`scratch_task`].
    ScratchTask(Box<[String; 4]>, oneshot::Sender<Result<(), PersistError>>),
    Decide(
        Box<(Proposal, Context)>,
        oneshot::Sender<Result<Decided, PersistError>>,
    ),
    Attempt(Box<AttemptJob>, oneshot::Sender<Result<(), PersistError>>),
    /// A terminal attempt move decided with its cancel latch.
    EndAttempt(
        Box<(AttemptMove, CancelLatch)>,
        oneshot::Sender<Result<AttemptState, PersistError>>,
    ),
}

/// Test-only fault injection inside the writer.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(super) enum Fault {
    /// Sleep before handling each request.
    Stall(std::time::Duration),
    /// Abort the process after the events of a commit are inserted, inside the open transaction.
    AbortAfterEvents,
}

#[derive(Debug, Clone)]
pub(super) struct Writer {
    jobs: mpsc::Sender<Job>,
    busy_timeout: std::time::Duration,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Job::Commit(..) => "Commit",
            Job::EnsureProject(..) => "EnsureProject",
            Job::ScratchTask(..) => "ScratchTask",
            Job::Decide(..) => "Decide",
            Job::Attempt(..) => "Attempt",
            Job::EndAttempt(..) => "EndAttempt",
        })
    }
}

impl Writer {
    pub(super) fn spawn(conn: Connection, options: &Options) -> Result<Writer, PersistError> {
        Self::start(conn, options, || {}, || {})
    }

    #[cfg(test)]
    pub(super) fn spawn_with(
        conn: Connection,
        options: &Options,
        fault: Fault,
    ) -> Result<Writer, PersistError> {
        let before = move || {
            if let Fault::Stall(d) = fault {
                std::thread::sleep(d);
            }
        };
        let after_events = move || {
            if let Fault::AbortAfterEvents = fault {
                std::process::abort();
            }
        };
        Self::start(conn, options, before, after_events)
    }

    /// `before` runs before each request and `after_events` inside a commit after its events (fault injection in
    /// tests; no-ops otherwise).
    fn start(
        mut conn: Connection,
        options: &Options,
        before: impl Fn() + Send + 'static,
        after_events: impl Fn() + Send + 'static,
    ) -> Result<Writer, PersistError> {
        let (jobs, mut rx) = mpsc::channel(options.queue.max(1));
        std::thread::Builder::new()
            .name("hoh-db-writer".to_owned())
            .spawn(move || {
                // Ends when every handle is gone; the connection closes with the thread.
                while let Some(job) = rx.blocking_recv() {
                    before();
                    match job {
                        Job::Commit(commit, reply) => {
                            let _ = reply.send(apply(&mut conn, &commit, &after_events));
                        }
                        Job::EnsureProject(id, repo, reply) => {
                            let _ = reply.send(ensure_project(&conn, &id, &repo));
                        }
                        Job::ScratchTask(ids, reply) => {
                            let _ = reply.send(scratch_task(&mut conn, &ids));
                        }
                        Job::Attempt(job, reply) => {
                            let _ = reply.send(attempts::apply(&mut conn, &job));
                        }
                        Job::EndAttempt(request, reply) => {
                            let (change, latch) = *request;
                            let _ = reply.send(attempts::end(&mut conn, &change, &latch));
                        }
                        Job::Decide(request, reply) => {
                            let (proposal, context) = *request;
                            let _ = reply.send(decide::decide(
                                &mut conn,
                                &proposal,
                                &context,
                                &after_events,
                            ));
                        }
                    }
                }
            })
            .map_err(|e| PersistError::Storage(format!("cannot start the writer thread: {e}")))?;
        Ok(Writer {
            jobs,
            busy_timeout: options.busy_timeout,
        })
    }

    async fn submit<T>(
        &self,
        job: impl FnOnce(oneshot::Sender<Result<T, PersistError>>) -> Job,
    ) -> Result<T, PersistError> {
        let (tx, rx) = oneshot::channel();
        match self.jobs.send_timeout(job(tx), self.busy_timeout).await {
            Ok(()) => {}
            Err(mpsc::error::SendTimeoutError::Timeout(_)) => {
                return Err(PersistError::Busy(format!(
                    "the writer queue stayed full for {:?}",
                    self.busy_timeout
                )));
            }
            Err(mpsc::error::SendTimeoutError::Closed(_)) => {
                return Err(PersistError::WriterStopped);
            }
        }
        rx.await.map_err(|_| PersistError::WriterStopped)?
    }

    pub(super) async fn commit(&self, commit: Commit) -> Result<Committed, PersistError> {
        self.submit(|tx| Job::Commit(commit, tx)).await
    }

    pub(super) async fn decide(
        &self,
        proposal: Proposal,
        context: Context,
    ) -> Result<Decided, PersistError> {
        self.submit(|tx| Job::Decide(Box::new((proposal, context)), tx))
            .await
    }

    pub(super) async fn attempt(&self, job: AttemptJob) -> Result<(), PersistError> {
        self.submit(|tx| Job::Attempt(Box::new(job), tx)).await
    }

    pub(super) async fn end_attempt(
        &self,
        change: AttemptMove,
        latch: CancelLatch,
    ) -> Result<AttemptState, PersistError> {
        self.submit(|tx| Job::EndAttempt(Box::new((change, latch)), tx))
            .await
    }

    pub(super) async fn ensure_project(
        &self,
        project_id: String,
        repo_path: String,
    ) -> Result<(), PersistError> {
        self.submit(|tx| Job::EnsureProject(project_id, repo_path, tx))
            .await
    }

    pub(super) async fn scratch_task(&self, ids: [String; 4]) -> Result<(), PersistError> {
        self.submit(|tx| Job::ScratchTask(Box::new(ids), tx)).await
    }
}

/// [`super::Store::scratch_task`]: the rows, in one IMMEDIATE transaction, only where no run exists.
fn scratch_task(conn: &mut Connection, ids: &[String; 4]) -> Result<(), PersistError> {
    let [project_id, repo_path, run_id, task_id] = ids;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let runs: i64 = tx
        .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))
        .map_err(sqlite_error)?;
    if runs > 0 {
        return Err(PersistError::NotScratch(format!(
            "the database already has {runs} run(s)"
        )));
    }
    let now = crate::time::now_rfc3339();
    let spec_id = format!("{run_id}-spec");
    tx.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO NOTHING",
        params![project_id, repo_path, now],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES (?1, ?2, ?3, 1)",
        params![spec_id, project_id, "0".repeat(64)],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES (?1, ?2, ?3, 'RUNNING', 'AUTO', ?4)",
        params![run_id, project_id, spec_id, now],
    )
    .map_err(sqlite_error)?;
    tx.execute(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES (?1, ?2, ?3, 'LEASED', 'doctor', 1)",
        params![task_id, run_id, spec_id],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

fn ensure_project(
    conn: &Connection,
    project_id: &str,
    repo_path: &str,
) -> Result<(), PersistError> {
    conn.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO NOTHING",
        params![project_id, repo_path, crate::time::now_rfc3339()],
    )
    .map(drop)
    .map_err(sqlite_error)
}

/// One commit as one IMMEDIATE transaction; any error drops (rolls back) the transaction.
fn apply(
    conn: &mut Connection,
    commit: &Commit,
    after_events: &dyn Fn(),
) -> Result<Committed, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let done = write(&tx, commit, after_events)?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

/// The writes of `commit` inside the caller's transaction.
pub(super) fn write(
    tx: &Transaction<'_>,
    commit: &Commit,
    after_events: &dyn Fn(),
) -> Result<Committed, PersistError> {
    let mut revisions = Vec::with_capacity(commit.cas.len());
    for cas in &commit.cas {
        revisions.push(compare_and_set(tx, cas)?);
    }
    let now = crate::time::now_rfc3339();
    let mut event_seqs = Vec::with_capacity(commit.events.len());
    for event in &commit.events {
        tx.execute(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.project_id,
                event.event_id,
                event.run_id,
                event.kind,
                event.payload.to_string(),
                now
            ],
        )
        .map_err(sqlite_error)?;
        event_seqs.push(tx.last_insert_rowid());
    }
    after_events();
    for message in &commit.outbox {
        let seq = *event_seqs.get(message.event).ok_or_else(|| {
            PersistError::IntegrityViolation(format!(
                "outbox message {} refers to event {} of a commit with {} events",
                message.message_id,
                message.event,
                event_seqs.len()
            ))
        })?;
        tx.execute(
            "INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'PENDING')",
            params![
                message.message_id,
                seq,
                message.run_id,
                message.recipient_role,
                message.role_epoch,
                message.payload.to_string()
            ],
        )
        .map_err(sqlite_error)?;
    }
    Ok(Committed {
        revisions,
        event_seqs,
    })
}

/// One compare-and-set inside `tx`: read the row, check the revision and the move, then write it guarded by the
/// state and revision just read. Returns the new revision.
fn compare_and_set(tx: &Transaction<'_>, cas: &StateCas) -> Result<i64, PersistError> {
    let entity = cas.change.entity();
    if matches!(entity, Entity::Attempt | Entity::Input) {
        return Err(PersistError::IntegrityViolation(format!(
            "{entity:?} {} has no revision; it moves with its own move",
            cas.id
        )));
    }
    let (table, key) = entity.table();
    let stale = |actual| PersistError::StaleRevision {
        entity,
        id: cas.id.clone(),
        expected: cas.expected_revision,
        actual,
    };
    // The state, the revision, the saved resume state and whether blockers are recorded (the set is judged by its JSON
    // content, so `[ ]` is empty; anything but an array counts as blocked, M2T02R-04).
    let row: Option<(String, i64, Option<String>, bool)> = tx
        .query_row(
            &format!(
                "SELECT state, revision, {} FROM {table} WHERE {key} = ?1",
                if entity == Entity::Run {
                    "resume_state, blocked_reason IS NOT NULL OR json_type(blocked_reasons_json) <> 'array' \
                     OR json_array_length(blocked_reasons_json) > 0"
                } else {
                    "NULL, blocked_reason IS NOT NULL"
                }
            ),
            params![cas.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sqlite_error)?;
    let Some((state, revision, saved, blocked)) = row else {
        return Err(stale(None));
    };
    if revision != cas.expected_revision {
        return Err(stale(Some(revision)));
    }
    let illegal = |from: &str, to: &str, why: String| PersistError::IllegalTransition {
        entity,
        id: cas.id.clone(),
        from: from.to_owned(),
        to: to.to_owned(),
        why,
    };
    let not_in_table = || "not in the transition table".to_owned();
    // Blockers are facts: no move clears them, and no run works or task becomes ready while one is recorded; only
    // the blocking mechanism removes them (ADR-011 §1, M2T02R-02).
    let unresolved = || "blockers are recorded and unresolved".to_owned();
    let changed = match cas.change {
        Change::Touch(_) => tx.execute(
            &format!("UPDATE {table} SET revision = revision + 1 WHERE {key} = ?1 AND revision = ?2"),
            params![cas.id, revision],
        ),
        Change::Run(to) => {
            let from = parse::<RunState>(&state)?;
            let saved = saved.as_deref().map(parse::<RunState>).transpose()?;
            let resume = lifecycle::resume_state(from, to, saved).map_err(|_| {
                let why = if lifecycle::check(from, to).is_ok() {
                    format!("resume_state is {}", saved.map_or("unset", Lifecycle::as_str))
                } else {
                    not_in_table()
                };
                illegal(from.as_str(), to.as_str(), why)
            })?;
            if lifecycle::is_working(to) && blocked {
                return Err(illegal(from.as_str(), to.as_str(), unresolved()));
            }
            tx.execute(
                "UPDATE runs SET state = ?1, revision = revision + 1, resume_state = ?2 \
                 WHERE run_id = ?3 AND revision = ?4 AND state = ?5",
                params![to.as_str(), resume.map(Lifecycle::as_str), cas.id, revision, state],
            )
        }
        Change::Task(to) => {
            let from = parse::<TaskState>(&state)?;
            lifecycle::check(from, to).map_err(|_| illegal(from.as_str(), to.as_str(), not_in_table()))?;
            if matches!(to, TaskState::Pending | TaskState::Ready) && blocked {
                return Err(illegal(from.as_str(), to.as_str(), unresolved()));
            }
            tx.execute(
                "UPDATE tasks SET state = ?1, revision = revision + 1 WHERE task_id = ?2 AND revision = ?3 AND state = ?4",
                params![to.as_str(), cas.id, revision, state],
            )
        }
    }
    .map_err(sqlite_error)?;
    if changed != 1 {
        return Err(stale(Some(revision)));
    }
    Ok(revision + 1)
}

/// A state read from the database; the CHECK constraints keep unknown names out.
pub(super) fn parse<S: Lifecycle>(name: &str) -> Result<S, PersistError> {
    S::parse(name).ok_or_else(|| {
        PersistError::IntegrityViolation(format!(
            "unknown {} state {name:?} in the database",
            S::MACHINE
        ))
    })
}
