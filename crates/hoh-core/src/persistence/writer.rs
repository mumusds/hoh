//! The writer actor: the only read-write connection, owned by a dedicated thread (synchronous rusqlite never runs on
//! a Tokio worker). Requests arrive through a bounded queue; a caller waits at most busy_timeout for room and then
//! gets a retryable `DB_BUSY`, so nothing is dropped silently. Each [`Commit`] is one `BEGIN IMMEDIATE` transaction:
//! state compare-and-set, event appends and outbox inserts succeed together or not at all.

use super::attempts::{self, AttemptJob, AttemptMove, CancelLatch};
use super::commands::{self, CommandOutcome, HostStart, SubmitCommand};
use super::decide::{self, Decided};
use super::{FaultHooks, Options, PersistError, Pragmas, sqlite_error};
use crate::decision::{Context, Proposal};
use crate::lifecycle::AttemptState;
use crate::lifecycle::{self, Lifecycle};
use crate::protocol::{RunState, TaskState};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot, watch};

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
    Command(
        Box<SubmitCommand>,
        oneshot::Sender<Result<CommandOutcome, PersistError>>,
    ),
    /// Project id and repo path for [`commands::begin_host`].
    BeginHost(
        Box<(String, String)>,
        oneshot::Sender<Result<HostStart, PersistError>>,
    ),
    /// The writer connection's settings.
    Pragmas(oneshot::Sender<Result<Pragmas, PersistError>>),
    /// An outbox operation ([`super::outbox`]).
    Outbox(
        Box<OutboxJob>,
        oneshot::Sender<Result<OutboxDone, PersistError>>,
    ),
    /// A run id and a plan for [`super::plan::import`].
    Plan(
        Box<(String, serde_json::Value)>,
        oneshot::Sender<Result<super::plan::Imported, PersistError>>,
    ),
    /// A run id and the context of [`super::rounds::round`].
    Round(
        Box<(String, Context)>,
        oneshot::Sender<Result<super::rounds::RoundOutcome, PersistError>>,
    ),
    /// Something to hold until the writer ends (a host's project lock, ADR-016 decision 12).
    Keep(
        Box<dyn std::any::Any + Send>,
        oneshot::Sender<Result<(), PersistError>>,
    ),
}

/// What the writer holds until it ends (a host's project lock). If the thread unwinds from a panic instead, what it
/// holds is leaked, not dropped: a lock stays held until the process ends rather than letting the project go while the
/// rest of its host still runs (ADR-016 decision 12, M2T12R-120).
struct Kept(Vec<Box<dyn std::any::Any + Send>>);

impl Drop for Kept {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::mem::forget(std::mem::take(&mut self.0));
        }
    }
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
    /// Becomes true when the writer thread has ended normally: every request done, its connection closed, what it
    /// kept dropped. Its sender also goes when the thread unwinds from a panic, and then what it kept is leaked
    /// (still held) instead ([`Kept`]).
    ended: watch::Receiver<bool>,
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
            Job::Command(..) => "Command",
            Job::BeginHost(..) => "BeginHost",
            Job::Pragmas(..) => "Pragmas",
            Job::Outbox(..) => "Outbox",
            Job::Plan(..) => "Plan",
            Job::Round(..) => "Round",
            Job::Keep(..) => "Keep",
        })
    }
}

impl Writer {
    /// The writer thread; `committed` gets the highest event seq after every request, so it moves only once a
    /// transaction that appended events has committed (REQ-036).
    pub(super) fn spawn(
        conn: Connection,
        options: &Options,
        committed: watch::Sender<i64>,
    ) -> Result<Writer, PersistError> {
        Self::start(conn, options, committed, || {}, || {})
    }

    #[cfg(test)]
    pub(super) fn spawn_with(
        conn: Connection,
        options: &Options,
        fault: Fault,
        committed: watch::Sender<i64>,
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
        Self::start(conn, options, committed, before, after_events)
    }

    /// `before` runs before each request and `after_events` inside a commit after its events (fault injection in
    /// tests; no-ops otherwise).
    fn start(
        mut conn: Connection,
        options: &Options,
        committed: watch::Sender<i64>,
        before: impl Fn() + Send + 'static,
        after_events: impl Fn() + Send + 'static,
    ) -> Result<Writer, PersistError> {
        let (jobs, mut rx) = mpsc::channel(options.queue.max(1));
        // The claims handed out and not yet handed to a transport (ADR-018 decision 4).
        let writer_claims = Registry::default();
        let faults = options.faults.clone();
        let (end, ended) = watch::channel(false);
        std::thread::Builder::new()
            .name("hoh-db-writer".to_owned())
            .spawn(move || {
                // Ends when every handle is gone and every queued request is done.
                let mut kept = Kept(Vec::new());
                // However the thread ends, a panic included, no guard can hand its row over afterwards (M2T04R-402).
                let _closer = Closer(writer_claims.clone());
                while let Some(job) = rx.blocking_recv() {
                    before();
                    match job {
                        Job::Commit(commit, reply) => {
                            let _ = reply.send(apply(
                                &mut conn,
                                &commit,
                                &after_events,
                                faults.as_ref(),
                            ));
                        }
                        Job::Command(command, reply) => {
                            let _ =
                                reply.send(commands::submit(&mut conn, &command, faults.as_ref()));
                        }
                        Job::BeginHost(ids, reply) => {
                            let _ = reply.send(commands::begin_host(&mut conn, &ids.0, &ids.1));
                        }
                        Job::Pragmas(reply) => {
                            let _ = reply.send(super::pragmas(&conn));
                        }
                        Job::Outbox(job, reply) => {
                            // A claim travels as a guard registered here: dropped before it is handed to a
                            // transport, wherever that happens, it is returned and undone (M2T04R-100, -200).
                            let _ = reply.send(run_outbox(
                                &mut conn,
                                *job,
                                &writer_claims,
                                &after_events,
                            ));
                        }
                        Job::Plan(request, reply) => {
                            let (run_id, plan) = *request;
                            let _ = reply.send(super::plan::import(
                                &mut conn,
                                &run_id,
                                &plan,
                                &after_events,
                            ));
                        }
                        Job::Round(request, reply) => {
                            let (run_id, context) = *request;
                            let _ = reply.send(super::rounds::round(
                                &mut conn,
                                &run_id,
                                &context,
                                &after_events,
                            ));
                        }
                        Job::Keep(held, reply) => {
                            kept.0.push(held);
                            let _ = reply.send(Ok(()));
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
                    // Only what has committed is visible to this connection's next read.
                    if let Ok(now) = commands::max_seq(&conn) {
                        committed.send_if_modified(|seq| {
                            let moved = now > *seq;
                            if moved {
                                *seq = now;
                            }
                            moved
                        });
                    }
                }
                // Every claim not handed to a transport is undone before the store closes, the guards of some
                // still alive: they cannot be handed over any more (M2T04R-301). Then the connection, then what was
                // kept (a lock goes only after the last write), then the word.
                let left = writer_claims.close_and_take_all();
                let _ = super::outbox::unclaim_all(&mut conn, &left);
                drop(conn);
                drop(kept);
                let _ = end.send(true);
            })
            .map_err(|e| PersistError::Storage(format!("cannot start the writer thread: {e}")))?;
        Ok(Writer {
            jobs,
            busy_timeout: options.busy_timeout,
            ended,
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

    pub(super) async fn outbox(&self, job: OutboxJob) -> Result<OutboxDone, PersistError> {
        self.submit(|tx| Job::Outbox(Box::new(job), tx)).await
    }

    pub(super) async fn import_plan(
        &self,
        run_id: String,
        plan: serde_json::Value,
    ) -> Result<super::plan::Imported, PersistError> {
        self.submit(|tx| Job::Plan(Box::new((run_id, plan)), tx))
            .await
    }

    pub(super) async fn schedule_round(
        &self,
        run_id: String,
        context: Context,
    ) -> Result<super::rounds::RoundOutcome, PersistError> {
        self.submit(|tx| Job::Round(Box::new((run_id, context)), tx))
            .await
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

    pub(super) async fn command(
        &self,
        command: SubmitCommand,
    ) -> Result<CommandOutcome, PersistError> {
        self.submit(|tx| Job::Command(Box::new(command), tx)).await
    }

    pub(super) async fn begin_host(
        &self,
        project_id: String,
        repo_path: String,
    ) -> Result<HostStart, PersistError> {
        self.submit(|tx| Job::BeginHost(Box::new((project_id, repo_path)), tx))
            .await
    }

    pub(super) async fn pragmas(&self) -> Result<Pragmas, PersistError> {
        self.submit(Job::Pragmas).await
    }

    pub(super) async fn keep(
        &self,
        held: Box<dyn std::any::Any + Send>,
    ) -> Result<(), PersistError> {
        self.submit(|tx| Job::Keep(held, tx)).await
    }

    /// Whether the writer thread still takes requests.
    pub(super) fn alive(&self) -> bool {
        !self.jobs.is_closed()
    }

    /// Resolves once the writer thread has ended: true when it finished (the word is sent after its last job), false
    /// when it ended by a panic (the sender went without the word).
    pub(super) fn ended(&self) -> impl std::future::Future<Output = bool> + Send + 'static {
        let mut ended = self.ended.clone();
        async move {
            loop {
                if *ended.borrow_and_update() {
                    return true;
                }
                if ended.changed().await.is_err() {
                    return *ended.borrow();
                }
            }
        }
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
    faults: &dyn FaultHooks,
) -> Result<Committed, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    let done = write(&tx, commit, after_events)?;
    faults.before_commit()?;
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

/// An outbox operation for the writer thread.
pub(super) enum OutboxJob {
    /// Now, roles, the delivery limit.
    Claim(std::time::SystemTime, Vec<String>, i64),
    /// Message id, generation, settlement, the delivery limit.
    Settle(String, i64, super::outbox::Settlement, i64),
    Consume(
        Box<(
            String,
            crate::protocol::Message,
            super::outbox::ConsumeEffect,
            std::time::SystemTime,
        )>,
    ),
    /// Consumer, message id, run id, violations.
    Invalid(Box<(String, String, Option<String>, String)>),
    TaskResult(Box<crate::protocol::TaskResult>),
}

/// What an outbox operation gives back.
#[derive(Debug, Default)]
pub(super) struct OutboxDone {
    pub(super) rows: Vec<Claimed>,
    pub(super) consumed: Option<super::outbox::Consumed>,
    pub(super) event_seq: Option<i64>,
    pub(super) task_result: Option<super::results::TaskResultOutcome>,
}

fn run_outbox(
    conn: &mut Connection,
    job: OutboxJob,
    claims: &Registry,
    after_events: &dyn Fn(),
) -> Result<OutboxDone, PersistError> {
    use super::outbox;
    Ok(match job {
        OutboxJob::Claim(now, roles, max) => {
            // The returned claims are undone in this claim's own transaction, and messages with a live guard are not
            // claimed again; a claim that cannot run keeps the returns for the next (M2T04R-400, -450).
            let (returned, live) = claims.take_for_claim();
            match outbox::claim_next(conn, now, &roles, max, &returned, &live, after_events) {
                Ok(rows) => OutboxDone {
                    rows: rows.into_iter().map(|row| claims.register(row)).collect(),
                    ..OutboxDone::default()
                },
                Err(e) => {
                    claims.put_back(returned);
                    return Err(e);
                }
            }
        }
        OutboxJob::Settle(id, generation, settlement, max) => {
            outbox::settle(conn, &id, generation, &settlement, max, after_events)?;
            OutboxDone::default()
        }
        OutboxJob::Consume(request) => {
            let (consumer, message, effect, now) = *request;
            OutboxDone {
                consumed: Some(outbox::consume(
                    conn,
                    &consumer,
                    &message,
                    &effect,
                    now,
                    after_events,
                )?),
                ..OutboxDone::default()
            }
        }
        OutboxJob::TaskResult(result) => OutboxDone {
            task_result: Some(super::results::accept(conn, &result, after_events)?),
            ..OutboxDone::default()
        },
        OutboxJob::Invalid(request) => {
            let (consumer, id, run_id, violations) = *request;
            OutboxDone {
                event_seq: Some(outbox::invalid(
                    conn,
                    &consumer,
                    &id,
                    run_id.as_deref(),
                    &violations,
                    after_events,
                )?),
                ..OutboxDone::default()
            }
        }
    })
}

/// The claims a writer handed out (ADR-018 decision 4): outstanding until handed to a transport, returned when their
/// guard is dropped first, undone by the next claim's own transaction. Shared by the writer thread and the guards; no
/// channel, so a return never depends on the writer's queue or on the writer still running.
#[derive(Debug, Clone, Default)]
pub(super) struct Registry(std::sync::Arc<std::sync::Mutex<Claims>>);

#[derive(Debug, Default)]
struct Claims {
    next: u64,
    outstanding: std::collections::BTreeMap<u64, super::outbox::OutboxRow>,
    returned: Vec<super::outbox::OutboxRow>,
    /// The writer has ended: nothing is handed over any more.
    closed: bool,
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, Claims> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn register(&self, row: super::outbox::OutboxRow) -> Claimed {
        let mut claims = self.lock();
        claims.next += 1;
        let id = claims.next;
        claims.outstanding.insert(id, row.clone());
        Claimed {
            id,
            row: Some(row),
            registry: self.clone(),
        }
    }

    /// The returned claims (taken: the claim undoes them) and the messages that have a live guard.
    fn take_for_claim(
        &self,
    ) -> (
        Vec<super::outbox::OutboxRow>,
        std::collections::BTreeSet<String>,
    ) {
        let mut claims = self.lock();
        let live = claims
            .outstanding
            .values()
            .map(|row| row.message_id.clone())
            .collect();
        (std::mem::take(&mut claims.returned), live)
    }

    /// Returns a claim could not undo (its transaction did not run), for the next claim.
    fn put_back(&self, rows: Vec<super::outbox::OutboxRow>) {
        self.lock().returned.extend(rows);
    }

    /// The store closes: every claim not handed over, returned or still held, to be undone; nothing starts after.
    fn close_and_take_all(&self) -> Vec<super::outbox::OutboxRow> {
        let mut claims = self.lock();
        claims.closed = true;
        let mut rows = std::mem::take(&mut claims.returned);
        rows.extend(std::mem::take(&mut claims.outstanding).into_values());
        rows
    }
}

/// Closes the registry when the writer thread ends, by a panic too.
struct Closer(Registry);

impl Drop for Closer {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

/// An outbox row claimed for delivery (ADR-018 decision 4). The claim belongs to the store until [`Claimed::start`]
/// hands the row to a transport (from then the delivery counts); dropped before that, wherever it is (the writer's
/// reply, an unread answer in its channel, the dispatcher), it is returned to the writer, which undoes it.
pub struct Claimed {
    id: u64,
    row: Option<super::outbox::OutboxRow>,
    registry: Registry,
}

impl Claimed {
    /// The row claimed.
    pub fn row(&self) -> Option<&super::outbox::OutboxRow> {
        self.row.as_ref()
    }

    /// Hand the row to the transport: from here the delivery counts. `None` when the store has taken the claim back
    /// (it closed meanwhile): nothing is to be delivered then.
    pub fn start(mut self) -> Option<super::outbox::OutboxRow> {
        let handed = {
            let mut claims = self.registry.lock();
            !claims.closed && claims.outstanding.remove(&self.id).is_some()
        };
        let row = self.row.take();
        if handed { row } else { None }
    }
}

impl std::fmt::Debug for Claimed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Claimed")
            .field("id", &self.id)
            .field("row", &self.row)
            .finish_non_exhaustive()
    }
}

impl Drop for Claimed {
    fn drop(&mut self) {
        let mut claims = self.registry.lock();
        if let Some(row) = claims.outstanding.remove(&self.id)
            && !claims.closed
        {
            claims.returned.push(row);
        }
    }
}
