//! The writer actor: the only read-write connection, owned by a dedicated thread (synchronous rusqlite never runs on
//! a Tokio worker). Requests arrive through a bounded queue; a caller waits at most busy_timeout for room and then
//! gets a retryable `DB_BUSY`, so nothing is dropped silently. Each [`Commit`] is one `BEGIN IMMEDIATE` transaction:
//! state compare-and-set, event appends and outbox inserts succeed together or not at all.

use super::attempts::{self, AttemptJob, AttemptMove, CancelLatch};
use super::commands::{self, CommandOutcome, HostStart, SubmitCommand};
use super::decide::{self, Decided};
use super::{FaultHooks, Options, PersistError, Pragmas, WritePoint, sqlite_error};
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
    /// A project's team revisions (ConfigureTeam, docs/adr/ADR-022.md decision 3): compared, never moved by a
    /// compare-and-set.
    Team,
}

impl Entity {
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Entity::Run => ("runs", "run_id"),
            Entity::Task => ("tasks", "task_id"),
            Entity::Attempt => ("attempts", "attempt_id"),
            Entity::Input => ("input_queue", "input_id"),
            Entity::Team => ("team_revisions", "project_id"),
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
    Reconcile(
        Box<super::reconcile::ReconcileJob>,
        oneshot::Sender<Result<super::reconcile::ReconcileDone, PersistError>>,
    ),
    Runner(
        Box<super::runner::RunnerJob>,
        oneshot::Sender<Result<super::runner::RunnerDone, PersistError>>,
    ),
    /// A pause or cancel write ([`super::control`]).
    Control(
        Box<super::control::ControlJob>,
        oneshot::Sender<Result<super::control::ControlDone, PersistError>>,
    ),
    /// Wakes a writer waiting on its queue so it takes an urgent request first (docs/adr/ADR-021.md decision 8a).
    Wake,
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

/// What a request of the writer runs with (ADR-019 decisions 1 and 2): its name, the fault hooks and the epoch fence
/// of the host it serves.
pub(super) struct Ctx<'a> {
    job: &'static str,
    hooks: &'a dyn FaultHooks,
    fence: Option<&'a Fence>,
    /// Test-only: inside a commit after its events (`Fault::AbortAfterEvents`); a no-op otherwise.
    after_events: &'a dyn Fn(),
}

#[cfg(test)]
fn nothing() {}

impl Ctx<'_> {
    /// A request outside the writer loop in a unit test: no hooks, no fence.
    #[cfg(test)]
    pub(super) fn quiet() -> Ctx<'static> {
        Ctx {
            job: "Test",
            hooks: &super::NoFaults,
            fence: None,
            after_events: &nothing,
        }
    }

    /// The request reached `point`.
    pub(super) fn at(&self, point: WritePoint) {
        if point == WritePoint::EventsWritten {
            (self.after_events)();
        }
        self.hooks.at(self.job, point);
    }

    /// Just before a commit: a fault hook's error fails the commit with it, and the transaction rolls back.
    pub(super) fn before_commit(&self) -> Result<(), PersistError> {
        self.hooks.before_commit()
    }
}

/// The epoch of the host a writer serves (REQ-041, ADR-019 decision 2): set when that host began on the database.
#[derive(Debug, Clone)]
pub(super) struct Fence {
    project_id: String,
    host_epoch: i64,
}

/// Begin a write transaction (IMMEDIATE). Every write of the writer begins here (only `begin_host`, which moves the
/// epoch, does not), so once its host has begun every write is fenced: it goes on only while the project is still at
/// that host's epoch, else it changes 0 rows and fails as an IntegrityViolation (REQ-041 failure mode 4). The check
/// runs under the write lock, so no new host can begin between it and the commit.
pub(super) fn begin<'c>(
    conn: &'c mut Connection,
    ctx: &Ctx<'_>,
) -> Result<Transaction<'c>, PersistError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(sqlite_error)?;
    if let Some(fence) = ctx.fence {
        let current: Option<i64> = tx
            .query_row(
                "SELECT host_epoch FROM projects WHERE project_id = ?1",
                params![fence.project_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(sqlite_error)?;
        if current != Some(fence.host_epoch) {
            return Err(PersistError::IntegrityViolation(format!(
                "a write of host epoch {} of project {}, which is now at {}: 0 rows changed",
                fence.host_epoch,
                fence.project_id,
                current.map_or_else(|| "no epoch".to_owned(), |e| e.to_string())
            )));
        }
    }
    ctx.at(WritePoint::Begun);
    Ok(tx)
}

/// Send a request's answer; one that succeeded passes the Committed point first (after its commit, before its answer).
fn answer<T>(
    ctx: &Ctx<'_>,
    reply: oneshot::Sender<Result<T, PersistError>>,
    result: Result<T, PersistError>,
) {
    if result.is_ok() {
        ctx.at(WritePoint::Committed);
    }
    let _ = reply.send(result);
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

/// The priority lane's capacity.
const URGENT: usize = 16;

#[derive(Debug, Clone)]
pub(super) struct Writer {
    jobs: mpsc::Sender<Job>,
    /// The priority lane: what is queued here is taken before anything of `jobs` (a cancel, REQ-043 behaviour 3).
    urgent: mpsc::Sender<Job>,
    busy_timeout: std::time::Duration,
    /// Becomes true when the writer thread has ended normally: every request done, its connection closed, what it
    /// kept dropped. Its sender also goes when the thread unwinds from a panic, and then what it kept is leaked
    /// (still held) instead ([`Kept`]).
    ended: watch::Receiver<bool>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl Job {
    /// The request's name, as the fault hooks see it ([`FaultHooks::at`]).
    fn name(&self) -> &'static str {
        match self {
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
            Job::Reconcile(..) => "Reconcile",
            // A run's pace is a write of its own kind (fault hooks tell it from an attempt's runner writes).
            Job::Runner(job, _) if matches!(**job, super::runner::RunnerJob::Pace { .. }) => "Pace",
            // A run's team checkpoint is the run's, not an attempt's write (docs/adr/ADR-022.md decision 8).
            Job::Runner(job, _) if matches!(**job, super::runner::RunnerJob::Checkpoint(_)) => {
                "Checkpoint"
            }
            Job::Runner(..) => "Runner",
            Job::Control(..) => "Control",
            Job::Wake => "Wake",
            Job::Keep(..) => "Keep",
        }
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
        let (jobs, mut rx) = mpsc::channel::<Job>(options.queue.max(1));
        let (urgent, mut urgent_rx) = mpsc::channel::<Job>(URGENT);
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
                // The epoch of the host this writer serves, once it has begun (ADR-019 decision 2).
                let mut fence: Option<Fence> = None;
                loop {
                    // The priority lane first; a queued request after it (ADR-021 decision 8a). When every handle is
                    // gone, what is left of the lane is still done.
                    let job = match urgent_rx.try_recv() {
                        Ok(job) => job,
                        Err(_) => match rx.blocking_recv() {
                            Some(job) => job,
                            None => match urgent_rx.try_recv() {
                                Ok(job) => job,
                                Err(_) => break,
                            },
                        },
                    };
                    if matches!(job, Job::Wake) {
                        continue;
                    }
                    before();
                    let ctx = Ctx {
                        job: job.name(),
                        hooks: faults.as_ref(),
                        fence: fence.as_ref(),
                        after_events: &after_events,
                    };
                    match job {
                        Job::Commit(commit, reply) => {
                            answer(&ctx, reply, apply(&mut conn, &commit, &ctx));
                        }
                        Job::Command(command, reply) => {
                            answer(&ctx, reply, commands::submit(&mut conn, &command, &ctx));
                        }
                        Job::BeginHost(ids, reply) => {
                            let started = commands::begin_host(&mut conn, &ids.0, &ids.1);
                            let begun = started.as_ref().ok().map(|s| Fence {
                                project_id: s.project_id.clone(),
                                host_epoch: s.host_epoch,
                            });
                            answer(&ctx, reply, started);
                            if begun.is_some() {
                                fence = begun;
                            }
                        }
                        Job::Pragmas(reply) => {
                            let _ = reply.send(super::pragmas(&conn));
                        }
                        Job::Outbox(job, reply) => {
                            // A claim travels as a guard registered here: dropped before it is handed to a
                            // transport, wherever that happens, it is returned and undone (M2T04R-100, -200).
                            answer(
                                &ctx,
                                reply,
                                run_outbox(&mut conn, *job, &writer_claims, &ctx),
                            );
                        }
                        Job::Plan(request, reply) => {
                            let (run_id, plan) = *request;
                            answer(
                                &ctx,
                                reply,
                                super::plan::import(&mut conn, &run_id, &plan, &ctx),
                            );
                        }
                        Job::Round(request, reply) => {
                            let (run_id, context) = *request;
                            answer(
                                &ctx,
                                reply,
                                super::rounds::round(&mut conn, &run_id, &context, &ctx),
                            );
                        }
                        Job::Reconcile(job, reply) => {
                            answer(&ctx, reply, super::reconcile::run(&mut conn, *job, &ctx));
                        }
                        Job::Runner(job, reply) => {
                            answer(&ctx, reply, super::runner::run(&mut conn, *job, &ctx));
                        }
                        Job::Control(job, reply) => {
                            answer(&ctx, reply, super::control::run(&mut conn, *job, &ctx));
                        }
                        Job::Keep(held, reply) => {
                            kept.0.push(held);
                            let _ = reply.send(Ok(()));
                        }
                        // Taken above: it only wakes the loop.
                        Job::Wake => {}
                        Job::EnsureProject(id, repo, reply) => {
                            answer(&ctx, reply, ensure_project(&mut conn, &id, &repo, &ctx));
                        }
                        Job::ScratchTask(ids, reply) => {
                            answer(&ctx, reply, scratch_task(&mut conn, &ids, &ctx));
                        }
                        Job::Attempt(job, reply) => {
                            answer(&ctx, reply, attempts::apply(&mut conn, &job, &ctx));
                        }
                        Job::EndAttempt(request, reply) => {
                            let (change, latch) = *request;
                            answer(&ctx, reply, attempts::end(&mut conn, &change, &latch, &ctx));
                        }
                        Job::Decide(request, reply) => {
                            let (proposal, context) = *request;
                            answer(
                                &ctx,
                                reply,
                                decide::decide(&mut conn, &proposal, &context, &ctx),
                            );
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
                let close = Ctx {
                    job: "Close",
                    hooks: faults.as_ref(),
                    fence: fence.as_ref(),
                    after_events: &after_events,
                };
                let _ = super::outbox::unclaim_all(&mut conn, &left, &close);
                drop(conn);
                drop(kept);
                let _ = end.send(true);
            })
            .map_err(|e| PersistError::Storage(format!("cannot start the writer thread: {e}")))?;
        Ok(Writer {
            jobs,
            urgent,
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

    /// [`Writer::submit`] through the priority lane: taken before anything queued (ADR-021 decision 8a). A wake goes
    /// into the queue too, for a writer waiting on it; a full queue means the writer is busy and looks at the lane
    /// before its next request anyway.
    async fn submit_urgent<T>(
        &self,
        job: impl FnOnce(oneshot::Sender<Result<T, PersistError>>) -> Job,
    ) -> Result<T, PersistError> {
        let (tx, rx) = oneshot::channel();
        match self.urgent.send_timeout(job(tx), self.busy_timeout).await {
            Ok(()) => {}
            Err(mpsc::error::SendTimeoutError::Timeout(_)) => {
                return Err(PersistError::Busy(format!(
                    "the writer's priority lane stayed full for {:?}",
                    self.busy_timeout
                )));
            }
            Err(mpsc::error::SendTimeoutError::Closed(_)) => {
                return Err(PersistError::WriterStopped);
            }
        }
        let _ = self.jobs.try_send(Job::Wake);
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

    pub(super) async fn runner(
        &self,
        job: super::runner::RunnerJob,
    ) -> Result<super::runner::RunnerDone, PersistError> {
        self.submit(|tx| Job::Runner(Box::new(job), tx)).await
    }

    pub(super) async fn reconcile(
        &self,
        job: super::reconcile::ReconcileJob,
    ) -> Result<super::reconcile::ReconcileDone, PersistError> {
        self.submit(|tx| Job::Reconcile(Box::new(job), tx)).await
    }

    pub(super) async fn control(
        &self,
        job: super::control::ControlJob,
    ) -> Result<super::control::ControlDone, PersistError> {
        // A cancel's passes and its last transaction take the priority lane too (ADR-021 decision 8a).
        if matches!(
            job,
            super::control::ControlJob::CancelPass(_)
                | super::control::ControlJob::FinishCancel { .. }
        ) {
            return self
                .submit_urgent(|tx| Job::Control(Box::new(job), tx))
                .await;
        }
        self.submit(|tx| Job::Control(Box::new(job), tx)).await
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
        // A cancel takes the priority lane (REQ-043 behaviour 3).
        if command.target
            == super::commands::RunTarget::To(crate::protocol::RunState::CancelRequested)
        {
            return self
                .submit_urgent(|tx| Job::Command(Box::new(command), tx))
                .await;
        }
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
fn scratch_task(
    conn: &mut Connection,
    ids: &[String; 4],
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let [project_id, repo_path, run_id, task_id] = ids;
    let tx = begin(conn, ctx)?;
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
    conn: &mut Connection,
    project_id: &str,
    repo_path: &str,
    ctx: &Ctx<'_>,
) -> Result<(), PersistError> {
    let tx = begin(conn, ctx)?;
    tx.execute(
        "INSERT INTO projects(project_id, repo_path, created_at) VALUES (?1, ?2, ?3) ON CONFLICT(project_id) DO NOTHING",
        params![project_id, repo_path, crate::time::now_rfc3339()],
    )
    .map_err(sqlite_error)?;
    tx.commit().map_err(sqlite_error)
}

/// One commit as one IMMEDIATE transaction; any error drops (rolls back) the transaction.
fn apply(conn: &mut Connection, commit: &Commit, ctx: &Ctx<'_>) -> Result<Committed, PersistError> {
    let tx = begin(conn, ctx)?;
    let done = write(&tx, commit, ctx)?;
    ctx.before_commit()?;
    tx.commit().map_err(sqlite_error)?;
    Ok(done)
}

/// The writes of `commit` inside the caller's transaction.
pub(super) fn write(
    tx: &Transaction<'_>,
    commit: &Commit,
    ctx: &Ctx<'_>,
) -> Result<Committed, PersistError> {
    let mut revisions = Vec::with_capacity(commit.cas.len());
    for cas in &commit.cas {
        revisions.push(compare_and_set(tx, cas)?);
    }
    ctx.at(WritePoint::StateWritten);
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
    ctx.at(WritePoint::EventsWritten);
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
    ctx.at(WritePoint::OutboxWritten);
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
    ctx: &Ctx<'_>,
) -> Result<OutboxDone, PersistError> {
    use super::outbox;
    Ok(match job {
        OutboxJob::Claim(now, roles, max) => {
            // The returned claims are undone in this claim's own transaction, and messages with a live guard are not
            // claimed again; a claim that cannot run keeps the returns for the next (M2T04R-400, -450).
            let (returned, live) = claims.take_for_claim();
            match outbox::claim_next(conn, now, &roles, max, &returned, &live, ctx) {
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
            outbox::settle(conn, &id, generation, &settlement, max, ctx)?;
            OutboxDone::default()
        }
        OutboxJob::Consume(request) => {
            let (consumer, message, effect, now) = *request;
            OutboxDone {
                consumed: Some(outbox::consume(
                    conn, &consumer, &message, &effect, now, ctx,
                )?),
                ..OutboxDone::default()
            }
        }
        OutboxJob::TaskResult(result) => OutboxDone {
            task_result: Some(super::results::accept(conn, &result, ctx)?),
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
                    ctx,
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
