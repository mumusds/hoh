//! The writer actor: the only read-write connection, owned by a dedicated thread (synchronous rusqlite never runs on
//! a Tokio worker). Requests arrive through a bounded queue; a caller waits at most busy_timeout for room and then
//! gets a retryable `DB_BUSY`, so nothing is dropped silently. Each [`Commit`] is one `BEGIN IMMEDIATE` transaction:
//! state compare-and-set, event appends and outbox inserts succeed together or not at all.

use super::{Options, PersistError, sqlite_error};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use tokio::sync::{mpsc, oneshot};

/// Entities with a state and a revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Entity {
    Run,
    Task,
}

impl Entity {
    fn table(self) -> (&'static str, &'static str) {
        match self {
            Entity::Run => ("runs", "run_id"),
            Entity::Task => ("tasks", "task_id"),
        }
    }
}

/// Move `entity` `id` to `state` if its revision is still `expected_revision`; the revision then grows by one.
/// Whether the move is legal is the state machine's decision (M2-T02), not the store's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateCas {
    pub entity: Entity,
    pub id: String,
    pub expected_revision: i64,
    pub state: String,
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
}

/// Test-only fault injection inside the writer.
#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fault {
    /// Sleep before handling each request.
    Stall(std::time::Duration),
    /// Abort the process after the events of a commit are inserted, inside the open transaction.
    AbortAfterEvents,
}

#[derive(Debug, Clone)]
pub(crate) struct Writer {
    jobs: mpsc::Sender<Job>,
    busy_timeout: std::time::Duration,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Job::Commit(..) => "Commit",
            Job::EnsureProject(..) => "EnsureProject",
        })
    }
}

impl Writer {
    pub(crate) fn spawn(conn: Connection, options: &Options) -> Result<Writer, PersistError> {
        Self::start(conn, options, || {}, || {})
    }

    #[cfg(test)]
    pub(crate) fn spawn_with(
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

    pub(crate) async fn commit(&self, commit: Commit) -> Result<Committed, PersistError> {
        self.submit(|tx| Job::Commit(commit, tx)).await
    }

    pub(crate) async fn ensure_project(
        &self,
        project_id: String,
        repo_path: String,
    ) -> Result<(), PersistError> {
        self.submit(|tx| Job::EnsureProject(project_id, repo_path, tx))
            .await
    }
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
    let mut revisions = Vec::with_capacity(commit.cas.len());
    for cas in &commit.cas {
        let (table, key) = cas.entity.table();
        let changed = tx
            .execute(
                &format!("UPDATE {table} SET state = ?1, revision = revision + 1 WHERE {key} = ?2 AND revision = ?3"),
                params![cas.state, cas.id, cas.expected_revision],
            )
            .map_err(sqlite_error)?;
        if changed != 1 {
            let actual = tx
                .query_row(
                    &format!("SELECT revision FROM {table} WHERE {key} = ?1"),
                    params![cas.id],
                    |r| r.get::<_, i64>(0),
                )
                .optional()
                .map_err(sqlite_error)?;
            return Err(PersistError::StaleRevision {
                entity: cas.entity,
                id: cas.id.clone(),
                expected: cas.expected_revision,
                actual,
            });
        }
        revisions.push(cas.expected_revision + 1);
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
    tx.commit().map_err(sqlite_error)?;
    Ok(Committed {
        revisions,
        event_seqs,
    })
}
