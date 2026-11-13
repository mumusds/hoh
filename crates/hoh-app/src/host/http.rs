//! The host's local API (contracts/openapi.json; docs/adr/ADR-016.md): the same routes on the Unix socket and on the
//! loopback port, each request checked for its token and, on the port, for its Host and Origin before anything else.
//! A command is answered only after its transaction committed; events reach SSE clients only through the store's
//! committed watermark, never from a handler, so a client that disconnects cannot lose or hasten one.

use super::HostHooks;
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use futures_util::stream::{self, Stream};
use hoh_core::persistence::{
    CommandOutcome, EventRow, FileTeam, NewEvent, NewOutbox, PersistError, Reader, RunTarget,
    Store, SubmitCommand, TeamChange,
};
use hoh_core::protocol::{self, Command, CommandBody, RunState};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

/// What every request handler shares.
pub(super) struct Shared {
    pub(super) store: Store,
    /// Read-only connections, used off the async workers (`spawn_blocking`).
    pub(super) readers: Mutex<Vec<Reader>>,
    pub(super) project_id: String,
    pub(super) token: String,
    pub(super) port: u16,
    pub(super) hooks: Arc<dyn HostHooks>,
    /// Becomes true when the host stops: event streams end then, so the connections can finish (M2T12R-01).
    pub(super) stop: tokio::sync::watch::Receiver<bool>,
    /// The event streams open now (docs/adr/ADR-021.md decision 6).
    pub(super) subscribers: std::sync::atomic::AtomicUsize,
    /// Every event stream opened so far (M2T06R-57: a brief subscription shows here, not in the gauge).
    pub(super) opened: std::sync::atomic::AtomicUsize,
    /// The host's own cgroup if it is delegated: attempts are then tracked by cgroups of their own
    /// (docs/adr/ADR-021.md decision 8d v5 to v7).
    pub(super) cgroups: Option<super::cgroup::Delegated>,
    /// The project's root (prompt override files are checked against it, docs/adr/ADR-022.md decision 11).
    pub(super) root: std::path::PathBuf,
}

/// One open event stream, counted while it lives.
struct Subscribed(Arc<Shared>);

impl Subscribed {
    fn new(shared: Arc<Shared>) -> Subscribed {
        shared
            .subscribers
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        shared
            .opened
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Subscribed(shared)
    }
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        self.0
            .subscribers
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Which listener a request came through.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Via {
    Tcp,
    Unix,
}

/// Events read from the store per query while replaying.
const PAGE: usize = 500;

fn routes(shared: Arc<Shared>, via: Via) -> Router {
    Router::new()
        .route("/v1/commands", post(submit_command))
        .route("/v1/runs", get(list_runs))
        .route("/v1/runs/{run_id}", get(get_run))
        .route("/v1/runs/{run_id}/team", get(list_team))
        .route("/v1/runs/{run_id}/acceptance", get(list_acceptance))
        .route("/v1/runs/{run_id}/issues", get(list_issues))
        .route("/v1/events", get(stream_events))
        .route("/healthz", get(health))
        .fallback(no_route)
        .method_not_allowed_fallback(no_route)
        .layer(middleware::from_fn_with_state(
            (shared.clone(), via),
            check_request,
        ))
        .with_state(shared)
}

/// How long a stopping host lets its connections finish before it ends them (ADR-016 decision 12).
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(5);
/// How long a connection may take to send a request's header.
const HEADER_READ: std::time::Duration = std::time::Duration::from_secs(30);

/// A listener the host accepts on.
pub(super) enum Listener {
    Tcp(tokio::net::TcpListener),
    Unix(tokio::net::UnixListener),
}

trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Io for T {}

impl Listener {
    async fn accept(&self) -> std::io::Result<Box<dyn Io>> {
        Ok(match self {
            Listener::Tcp(l) => Box::new(l.accept().await?.0),
            Listener::Unix(l) => Box::new(l.accept().await?.0),
        })
    }
}

/// Serve `listener` until the host stops; every connection is a task of this loop, so none outlives it: on stop each
/// gets a graceful shutdown (it finishes the request in hand), then [`SHUTDOWN_GRACE`], then it is ended and awaited
/// (ADR-016 decision 12, M2T12R-01, -60).
pub(super) async fn serve(
    listener: Listener,
    shared: Arc<Shared>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let via = match listener {
        Listener::Tcp(_) => Via::Tcp,
        Listener::Unix(_) => Via::Unix,
    };
    let router = routes(shared, via);
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok(stream) => stream,
                    Err(e) if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused
                            | std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                    ) => continue,
                    // Out of descriptors and the like: the listener stays readable, so wait rather than spin
                    // (M2T12R-92).
                    Err(_) => {
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
                            _ = stopped(stop.clone()) => break,
                        }
                        continue;
                    }
                };
                let service = hyper_util::service::TowerToHyperService::new(router.clone());
                let mut conn_stop = stop.clone();
                connections.spawn(async move {
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder
                        .timer(hyper_util::rt::TokioTimer::new())
                        .header_read_timeout(HEADER_READ);
                    let conn = builder.serve_connection(hyper_util::rt::TokioIo::new(stream), service);
                    tokio::pin!(conn);
                    tokio::select! {
                        _ = conn.as_mut() => {}
                        _ = stopped(conn_stop.clone()) => {
                            conn.as_mut().graceful_shutdown();
                            let _ = conn.await;
                        }
                    }
                    let _ = conn_stop.borrow_and_update();
                });
            }
            // A finished connection is collected as it goes.
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            _ = stopped(stop.clone()) => break,
        }
    }
    let _ = stop.borrow_and_update();
    drop(listener);
    let finished = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if finished.is_err() {
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

async fn stopped(mut stop: tokio::sync::watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// An `Error` body (openapi `components.schemas.Error`).
fn error(
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
    retryable: bool,
    current_revision: Option<i64>,
) -> Response {
    let body = json!({
        "code": code,
        "message": message.into(),
        "retryable": retryable,
        "current_revision": current_revision,
    });
    (status, axum::Json(body)).into_response()
}

/// A refused team change (REQ-018, ADR-022 decision 5): 400 INVALID_TEAM_CONFIG with the key path and the team
/// revision that stays current.
fn team_refused(key_path: &str, message: &str, current_revision: Option<i64>) -> Response {
    let body = json!({
        "code": "INVALID_TEAM_CONFIG",
        "message": message,
        "retryable": false,
        "current_revision": current_revision,
        "details": {"key_path": key_path},
    });
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

/// The key path of a ConfigureTeam's schema violation that is the team's (ADR-022 decision 5): one inside the payload
/// is its path there; one of the payload itself is the name of the member it misses, else `payload` (M2T07R-100).
fn team_key_path(violation: &hoh_core::schema::Violation) -> Option<String> {
    if let Some(inside) = violation.instance_path.strip_prefix("/payload/") {
        return Some(inside.replace('/', "."));
    }
    if violation.instance_path != "/payload" {
        return None;
    }
    let missing = violation
        .schema_path
        .ends_with("/required")
        .then(|| violation.message.strip_prefix("missing required property "))
        .flatten();
    Some(missing.unwrap_or("payload").to_owned())
}

/// Anything else: an Error body too (M2T12R-03).
async fn no_route() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "NOT_FOUND",
        "no such route or method",
        false,
        None,
    )
}

/// A store failure's own text goes to the host's stderr, never to a client (ADR-016 decision 6, M2T12R-02).
fn log(detail: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "hoh host: {detail}");
}

fn forbidden(message: &str) -> Response {
    error(StatusCode::FORBIDDEN, "FORBIDDEN", message, false, None)
}

/// Tokens compared in time independent of where they differ.
fn same_token(given: &[u8], expected: &[u8]) -> bool {
    given.len() == expected.len()
        && given
            .iter()
            .zip(expected)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// On the port: `Host` must be the bound loopback address and `Origin` absent or that address (a page elsewhere, or a
/// DNS-rebound name, is refused). On the socket: no `Origin` at all. Everywhere but `/healthz`: the token.
async fn check_request(
    State((shared, via)): State<(Arc<Shared>, Via)>,
    request: Request,
    next: Next,
) -> Response {
    // A stopping host takes nothing more (ADR-016 decision 12).
    if *shared.stop.borrow() {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "GONE",
            "the host is stopping; retry with the next host",
            true,
            None,
        );
    }
    let headers = request.headers();
    let origins: Vec<&HeaderValue> = headers.get_all(header::ORIGIN).iter().collect();
    // More than one Origin is refused, and so is a request target naming any authority (M2T12R-05).
    if origins.len() > 1 {
        return forbidden("more than one Origin");
    }
    let authority = request.uri().authority().map(|a| a.as_str().to_owned());
    match via {
        Via::Tcp => {
            let bound = format!("127.0.0.1:{}", shared.port);
            let hosts: Vec<&HeaderValue> = headers.get_all(header::HOST).iter().collect();
            if hosts.len() != 1 || hosts[0].to_str().ok() != Some(bound.as_str()) {
                return forbidden("the Host header is not the host's loopback address");
            }
            if authority.is_some_and(|a| a != bound) {
                return forbidden("the request target names another authority");
            }
            if origins
                .first()
                .is_some_and(|o| o.to_str().ok() != Some(format!("http://{bound}").as_str()))
            {
                return forbidden("the Origin is not the host's loopback address");
            }
        }
        Via::Unix => {
            if !origins.is_empty() {
                return forbidden("requests on the socket carry no Origin");
            }
            if authority.is_some() {
                return forbidden("the request target names an authority");
            }
        }
    }
    if request.uri().path() != "/healthz" {
        let given = headers
            .get("x-hoh-token")
            .map(HeaderValue::as_bytes)
            .unwrap_or_default();
        if !same_token(given, shared.token.as_bytes()) {
            return forbidden("missing or wrong X-HoH-Token");
        }
    }
    next.run(request).await
}

/// 200 while the host can write; 503 once its database writer has ended (M2T12R-150).
async fn health(State(shared): State<Arc<Shared>>) -> Response {
    if shared.store.writer_alive() {
        (StatusCode::OK, axum::Json(json!({"status": "ok"}))).into_response()
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(json!({"status": "the database writer stopped"})),
        )
            .into_response()
    }
}

/// Read with a pooled read-only connection, off the async workers (REQ-062: reads are never held up by the writer).
pub(super) async fn read<T: Send + 'static>(
    shared: &Arc<Shared>,
    query: impl FnOnce(&Reader) -> Result<T, PersistError> + Send + 'static,
) -> Result<T, PersistError> {
    let shared = shared.clone();
    tokio::task::spawn_blocking(move || {
        let pooled = shared.readers.lock().ok().and_then(|mut pool| pool.pop());
        let reader = match pooled {
            Some(reader) => reader,
            None => shared.store.reader()?,
        };
        let result = query(&reader);
        if let Ok(mut pool) = shared.readers.lock() {
            pool.push(reader);
        }
        result
    })
    .await
    .map_err(|e| PersistError::Storage(format!("a read did not finish: {e}")))?
}

/// The project's team revision now, for a team command's `current_revision`; none when it cannot be read.
async fn team_revision(shared: &Arc<Shared>) -> Option<i64> {
    let project = shared.project_id.clone();
    read(shared, move |reader| reader.team_revision(&project))
        .await
        .ok()
}

/// The project's configuration file as a team revision (docs/adr/ADR-022.md decision 6), read at the host's start and at
/// each StartRun: when the file (or, without one, the built-in defaults) differs from the latest revision made from it,
/// its whole team becomes the next revision, through the same store path as a ConfigureTeam, its command
/// `config-file/<sha256>/<base revision>`. An invalid file changes nothing: one `config.invalid` event per file content,
/// and the error for StartRun to refuse with (REQ-018 failure mode 5).
pub(super) async fn sync_config(
    shared: &Arc<Shared>,
) -> Result<hoh_core::config::Config, Unsynced> {
    let root = shared.root.clone();
    let loaded = tokio::task::spawn_blocking(move || hoh_core::config::load_file(&root))
        .await
        .unwrap_or_else(|e| {
            Err(hoh_core::config::ConfigError {
                file: None,
                key: None,
                line: None,
                column: None,
                message: format!("the file could not be read: {e}"),
            })
        });
    let sha = |text: &[u8]| -> String {
        Sha256::digest(text)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let (config, text) = match loaded {
        Ok(found) => found,
        Err(error) => {
            let content =
                std::fs::read(shared.root.join(hoh_core::config::FILE)).unwrap_or_default();
            let event_id = format!("config-invalid/{}", sha(&content));
            let project = shared.project_id.clone();
            let seen = {
                let id = event_id.clone();
                read(shared, move |reader| reader.event_exists(&project, &id))
                    .await
                    .unwrap_or(false)
            };
            if !seen {
                let commit = hoh_core::persistence::Commit {
                    events: vec![NewEvent {
                        project_id: shared.project_id.clone(),
                        event_id,
                        run_id: None,
                        kind: "config.invalid".to_owned(),
                        payload: json!({
                            "path": error.file.as_ref().map(|f| f.display().to_string()),
                            "key_path": error.key, "line": error.line, "column": error.column,
                            "message": error.message,
                        }),
                    }],
                    ..hoh_core::persistence::Commit::default()
                };
                if let Err(e) = shared.store.commit(commit).await {
                    log(&format!("config.invalid could not be recorded: {e}"));
                }
            }
            return Err(Unsynced::Invalid(error));
        }
    };
    let source = match &text {
        Some(text) => {
            json!({"kind": "file", "path": hoh_core::config::FILE, "sha256": sha(text.as_bytes())})
        }
        None => json!({"kind": "builtin"}),
    };
    // Another host life, or a StartRun at the same time, may have made it already: the command is then a replay. A
    // file that could not be applied is not taken as applied (M2T07R-204).
    for _ in 0..3 {
        let project = shared.project_id.clone();
        let latest = read(shared, move |reader| {
            Ok((
                reader.team_file_source(&project)?,
                reader.team_revision(&project)?,
            ))
        })
        .await;
        let (made, base) = latest.map_err(Unsynced::Store)?;
        if made.as_ref().unwrap_or(&json!({"kind": "builtin"})) == &source {
            return Ok(config);
        }
        shared.hooks.before_file_revision();
        let tag = source["sha256"].as_str().unwrap_or("builtin").to_owned();
        let command_id = format!("config-file/{tag}/{base}");
        let payload = json!({"source": source});
        let submit = SubmitCommand {
            command_id: command_id.clone(),
            idempotency_key: command_id.clone(),
            kind: "ConfigureTeam".to_owned(),
            run_id: String::new(),
            expected_revision: base,
            payload: payload.clone(),
            payload_sha256: sha(payload.to_string().as_bytes()),
            target: RunTarget::Team(TeamChange {
                bindings: None,
                scheduler: None,
                root: None,
                file: Some(FileTeam {
                    team: config.team.clone(),
                    scheduler: config.scheduler,
                    source: source.clone(),
                }),
            }),
            event: NewEvent {
                project_id: shared.project_id.clone(),
                event_id: command_id.clone(),
                run_id: None,
                kind: "team.configured".to_owned(),
                payload: json!({"command_id": command_id, "revision": base + 1, "source": source}),
            },
            outbox: None,
        };
        match shared.store.submit_command(submit).await {
            Err(PersistError::StaleRevision { .. }) => continue,
            Err(e) => return Err(Unsynced::Store(e)),
            Ok(_) => return Ok(config),
        }
    }
    Err(Unsynced::Moving)
}

/// Why the project's configuration file was not made the current team revision (decision 6).
#[derive(Debug)]
pub(super) enum Unsynced {
    /// The file is invalid (REQ-018 failure mode 5): StartRun is refused with where.
    Invalid(hoh_core::config::ConfigError),
    /// Its revision could not be read or written.
    Store(PersistError),
    /// The team changed under it at each of its tries.
    Moving,
}

impl std::fmt::Display for Unsynced {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Unsynced::Invalid(e) => write!(f, "{e}"),
            Unsynced::Store(e) => write!(f, "its revision could not be written: {e}"),
            Unsynced::Moving => f.write_str("the team changed at each try to apply it"),
        }
    }
}

/// A StartRun refused for the project's invalid configuration file (REQ-018 failure mode 5): 400 INVALID_TEAM_CONFIG
/// with the file's path, the key path, line and column, and the team's revision.
async fn config_refused(shared: &Arc<Shared>, e: &hoh_core::config::ConfigError) -> Response {
    let body = json!({
        "code": "INVALID_TEAM_CONFIG",
        "message": e.message,
        "retryable": false,
        "current_revision": team_revision(shared).await,
        "details": {
            "path": e.file.as_ref().map(|f| f.display().to_string()),
            "key_path": e.key, "line": e.line, "column": e.column,
        },
    });
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

/// The revision of `run_id` now, for a conflict's `current_revision`.
async fn current_revision(shared: &Arc<Shared>, run_id: &str) -> Option<i64> {
    let run_id = run_id.to_owned();
    read(shared, move |reader| reader.snapshot(&run_id))
        .await
        .ok()
        .flatten()
        .map(|(run, _)| run.revision)
}

/// A store error as the API answers it (ADR-016): conflicts are 409 with the current revision, a busy or full
/// database is 503 and retryable; the database's own text never reaches the client.
async fn store_error(shared: &Arc<Shared>, run_id: &str, e: PersistError) -> Response {
    match e {
        PersistError::KeyReuse(key) => error(
            StatusCode::CONFLICT,
            "KEY_REUSE",
            format!("idempotency key {key} was accepted with another payload"),
            false,
            current_revision(shared, run_id).await,
        ),
        PersistError::StaleRevision {
            entity,
            expected,
            actual,
            ..
        } => {
            let what = if entity == hoh_core::persistence::Entity::Team {
                "the team"
            } else {
                "the run"
            };
            error(
                StatusCode::CONFLICT,
                "STALE_REVISION",
                match actual {
                    Some(actual) => {
                        format!("expected revision {expected}, {what} is at revision {actual}")
                    }
                    None => format!("expected revision {expected}, {what} has no revision"),
                },
                false,
                actual,
            )
        }
        PersistError::InvalidTeamConfig {
            key_path,
            message,
            current_revision,
        } => team_refused(&key_path, &message, Some(current_revision)),
        PersistError::RunConfigFrozen(run_id) => {
            let body = json!({
                "code": "RUN_CONFIG_FROZEN",
                "message": format!(
                    "run {run_id} keeps the scheduler parameters of its snapshot; a change applies to runs started later"
                ),
                "retryable": false,
                "current_revision": null,
                "details": {"key_path": "scheduler"},
            });
            (StatusCode::CONFLICT, axum::Json(body)).into_response()
        }
        PersistError::IllegalTransition { from, to, why, .. } => error(
            StatusCode::CONFLICT,
            "ILLEGAL_TRANSITION",
            format!("{from} -> {to}: {why}"),
            false,
            current_revision(shared, run_id).await,
        ),
        PersistError::RunNotFound(id) => error(
            StatusCode::CONFLICT,
            "RUN_NOT_FOUND",
            format!("run {id} does not exist"),
            false,
            None,
        ),
        PersistError::Busy(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "DB_BUSY",
            "the database stayed locked for the busy timeout; nothing was written",
            true,
            current_revision(shared, run_id).await,
        ),
        // The writer ended (a panic): this host can write no more and stops; the next one can (M2T12R-150).
        PersistError::WriterStopped => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "GONE",
            "the host's database writer stopped; retry with the next host",
            true,
            None,
        ),
        PersistError::Full(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "DB_FULL",
            "the database's file system is full; nothing was written",
            true,
            current_revision(shared, run_id).await,
        ),
        PersistError::IntegrityViolation(detail) => {
            log(&detail);
            error(
                StatusCode::CONFLICT,
                "CONFLICT",
                "the command conflicts with stored data (its command_id may be used by another command)",
                false,
                current_revision(shared, run_id).await,
            )
        }
        other => {
            log(&other.to_string());
            error(
                StatusCode::SERVICE_UNAVAILABLE,
                "UNPROCESSABLE",
                "the store could not apply the request",
                false,
                None,
            )
        }
    }
}

/// `POST /v1/commands` (openapi `submitCommand`). StartRun, PauseRun, ResumeRun and CancelRun are applied (ADR-021);
/// the other kinds come with their tasks and are refused as not yet supported, with nothing written.
async fn submit_command(
    State(shared): State<Arc<Shared>>,
    body: Result<axum::body::Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    let Ok(body) = body else {
        return error(
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "the request body could not be read (too large or broken)",
            false,
            None,
        );
    };
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                format!("the body is not JSON: {e}"),
                false,
                None,
            );
        }
    };
    let command: Command = match protocol::decode(&value) {
        Ok(command) => command,
        // A team change that does not match the contract (apply_at other than checkpoint, a bindings member of
        // another type, a payload missing apply_at): 400 INVALID_TEAM_CONFIG with the key path (REQ-018 failure mode 4,
        // ADR-022 decision 5). A violation of the payload or inside it is the team's; the envelope's stays BAD_REQUEST
        // (M2T07R-58, -100).
        Err(protocol::DecodeError::Schema(violations))
            if value["kind"] == "ConfigureTeam"
                && violations.iter().any(|v| team_key_path(v).is_some()) =>
        {
            let (key_path, message) = violations
                .iter()
                .find_map(|v| team_key_path(v).map(|key_path| (key_path, v.message.clone())))
                .unwrap_or_default();
            let revision = team_revision(&shared).await;
            return team_refused(&key_path, &message, revision);
        }
        Err(e) => {
            return error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                format!("the command does not match the contract: {e}"),
                false,
                None,
            );
        }
    };
    // A new run takes the project's configuration file as it is now: an invalid one refuses it, the last revision
    // staying (docs/adr/ADR-022.md decision 6, REQ-018 failure mode 5).
    let mut effective = None;
    if matches!(command.body, CommandBody::StartRun(_)) {
        match sync_config(&shared).await {
            Ok(config) => effective = Some(config),
            Err(Unsynced::Invalid(e)) => return config_refused(&shared, &e).await,
            // As any store failure of a command: nothing was started.
            Err(Unsynced::Store(e)) => return store_error(&shared, "", e).await,
            Err(Unsynced::Moving) => {
                return error(
                    StatusCode::CONFLICT,
                    "STALE_REVISION",
                    "the team changed while the project's configuration file was applied; retry",
                    true,
                    team_revision(&shared).await,
                );
            }
        }
    }
    // The control commands (docs/adr/ADR-021.md decisions 5, 7, 8): their kind, where the run moves, the event's kind
    // and payload, and the runtime notice for the host loop (ADR-016 decision 5, ADR-018 decision 4).
    let kind = command.kind();
    let (name, target, event_kind, control, details) = match &command.body {
        CommandBody::StartRun(start) => {
            let mode = json!(start.mode).as_str().unwrap_or("AUTO").to_owned();
            (
                "StartRun",
                RunTarget::Start {
                    mode: mode.clone(),
                    budget: effective.as_ref().map_or_else(
                        || hoh_core::config::Config::default().budget,
                        |config| config.budget.clone(),
                    ),
                    policy: effective.as_ref().map_or_else(
                        || hoh_core::config::Config::default().supervision.policy,
                        |config| config.supervision.policy.clone(),
                    ),
                },
                "run.created",
                None,
                json!({"goal": start.goal, "mode": mode, "interactive": start.interactive}),
            )
        }
        CommandBody::PauseRun(reason) => (
            "PauseRun",
            RunTarget::To(RunState::PauseRequested),
            "run.pause_requested",
            Some("PAUSE_REQUESTED"),
            json!({"state": "PAUSE_REQUESTED", "reason": reason.reason}),
        ),
        CommandBody::CancelRun(reason) => (
            "CancelRun",
            RunTarget::To(RunState::CancelRequested),
            "run.cancel_requested",
            Some("CANCEL_REQUESTED"),
            json!({"state": "CANCEL_REQUESTED", "reason": reason.reason}),
        ),
        // The project's team (docs/adr/ADR-022.md decisions 3 to 5): revision r+1 when accepted.
        CommandBody::ConfigureTeam(team) => (
            "ConfigureTeam",
            RunTarget::Team(TeamChange {
                bindings: team.bindings.clone().map(Value::Object),
                scheduler: team.scheduler.clone(),
                root: Some(shared.root.clone()),
                file: None,
            }),
            "team.configured",
            None,
            json!({
                "revision": command.expected_revision.saturating_add(1),
                "bindings": team.bindings,
                "scheduler": team.scheduler,
            }),
        ),
        // No notice: nothing waits for a resume but the rounds, which follow the run's state.
        CommandBody::ResumeRun(reason) => (
            "ResumeRun",
            RunTarget::Resume,
            "run.resumed",
            None,
            json!({"reason": reason.reason}),
        ),
        _ => {
            return error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "CAPABILITY_UNSUPPORTED",
                format!("{kind:?} is not supported by this host yet"),
                false,
                None,
            );
        }
    };
    // A new run's id is the host's (CR-003); every other control command names its run.
    let run_id = match (&command.body, &command.run_id) {
        (CommandBody::StartRun(_), None) => super::new_id(),
        (CommandBody::StartRun(_), Some(_)) => {
            return error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                "StartRun names no run_id: the host gives the new run its id",
                false,
                None,
            );
        }
        (_, Some(run_id)) => run_id.clone(),
        // A ConfigureTeam with no run is the project's.
        (CommandBody::ConfigureTeam(_), None) => String::new(),
        (_, None) => {
            return error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                format!("{name} needs a run_id"),
                false,
                None,
            );
        }
    };
    let Ok(expected_revision) = i64::try_from(command.expected_revision) else {
        return error(
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "expected_revision is out of range",
            false,
            None,
        );
    };
    // The key is compared on the whole command in canonical form (keys sorted), so two kinds with the same payload
    // differ.
    let canonical = serde_json::to_vec(&value).unwrap_or_default();
    let payload_sha256: String = Sha256::digest(&canonical)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let event_id = super::new_id();
    let mut payload = json!({"command_id": command.command_id});
    if let (Some(all), Some(more)) = (payload.as_object_mut(), details.as_object()) {
        all.extend(more.clone());
    }
    let submit = SubmitCommand {
        command_id: command.command_id.clone(),
        idempotency_key: command.idempotency_key.clone(),
        kind: name.to_owned(),
        run_id: run_id.clone(),
        expected_revision,
        payload: value.get("payload").cloned().unwrap_or_else(|| json!({})),
        payload_sha256,
        target,
        event: NewEvent {
            project_id: shared.project_id.clone(),
            event_id: event_id.clone(),
            run_id: (!run_id.is_empty()).then(|| run_id.clone()),
            kind: event_kind.to_owned(),
            payload,
        },
        outbox: control.map(|control| NewOutbox {
            message_id: super::new_id(),
            event: 0,
            run_id: Some(run_id.clone()),
            recipient_role: "commander".to_owned(),
            role_epoch: 1,
            payload: json!({
                "notice": "runtime",
                "kind": "run.control",
                "control": control,
                "run_id": run_id,
                "command_id": command.command_id,
                "event_id": event_id,
                "reason": details.get("reason"),
            }),
        }),
    };
    let run_id = &run_id;
    match shared.store.submit_command(submit).await {
        Ok(CommandOutcome::Accepted { result_json, .. }) => {
            shared.hooks.after_commit();
            accepted(result_json, false)
        }
        Ok(CommandOutcome::Replayed { result_json }) => accepted(result_json, true),
        // A team command's revision is the team's (M2T07R-04).
        Err(PersistError::KeyReuse(key)) if name == "ConfigureTeam" => error(
            StatusCode::CONFLICT,
            "KEY_REUSE",
            format!("idempotency key {key} was accepted with another payload"),
            false,
            team_revision(&shared).await,
        ),
        Err(e) => store_error(&shared, run_id, e).await,
    }
}

/// 202 with the stored result bytes; a replay says so.
fn accepted(result_json: String, replayed: bool) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::ACCEPTED)
        .header(header::CONTENT_TYPE, "application/json");
    if replayed {
        response = response.header("idempotent-replayed", "true");
    }
    response
        .body(Body::from(result_json))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

#[derive(serde::Deserialize)]
struct RunsQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    state: Option<String>,
}

/// `GET /v1/runs` (openapi `listRuns`; docs/adr/ADR-021.md decision 6): a page of the project's runs, newest first,
/// each with its label; `cursor` is opaque. A query that cannot be read names no page: 404, within openapi's codes.
async fn list_runs(
    State(shared): State<Arc<Shared>>,
    query: Result<Query<RunsQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let unreadable = || {
        error(
            StatusCode::NOT_FOUND,
            "BAD_REQUEST",
            "limit must be 1 to 500, cursor one this host gave, state text",
            false,
            None,
        )
    };
    let Ok(Query(query)) = query else {
        return unreadable();
    };
    let limit = query.limit.unwrap_or(100);
    let before = match query.cursor.as_deref().map(str::parse::<i64>) {
        None => None,
        Some(Ok(before)) if before > 0 => Some(before),
        Some(_) => return unreadable(),
    };
    if !(1..=500).contains(&limit) {
        return unreadable();
    }
    let state = query.state;
    match read(&shared, move |reader| reader.list_runs(state.as_deref(), limit, before)).await {
        Ok(hoh_core::persistence::RunsPage {
            runs,
            next,
            watermark,
        }) => (
            StatusCode::OK,
            axum::Json(json!({
                "items": runs.iter().map(|(run, created_at)| json!({
                    "run_id": run.run_id,
                    "state": run.state,
                    "label": serde_json::from_value::<RunState>(json!(run.state))
                        .map_or_else(|_| run.state.clone(), |s| hoh_core::lifecycle::label(s).to_owned()),
                    "mode": run.mode,
                    "revision": run.revision,
                    "resume_state": run.resume_state,
                    "blocked_reason": run.blocked_reason,
                    "created_at": created_at,
                })).collect::<Vec<_>>(),
                "next_cursor": next.map(|n| n.to_string()),
                "watermark": watermark,
            })),
        )
            .into_response(),
        Err(e) => store_error(&shared, "", e).await,
    }
}

#[derive(serde::Deserialize)]
struct TeamQuery {
    limit: Option<usize>,
    cursor: Option<String>,
}

/// `GET /v1/runs/{run_id}/team` (openapi `listTeam`): a page of the run's role bindings in the role order, with the
/// watermark they were read at (docs/adr/ADR-022.md decision 18): at most `limit` (1 to 500, 100 by default) after
/// the role named by `cursor`; a query that cannot be read names no page, 404 as for `GET /v1/runs` (M2T07R-06).
async fn list_team(
    State(shared): State<Arc<Shared>>,
    run_id: Result<UrlPath<String>, axum::extract::rejection::PathRejection>,
    query: Result<Query<TeamQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let unreadable = || {
        error(
            StatusCode::NOT_FOUND,
            "BAD_REQUEST",
            "limit must be 1 to 500, cursor a role of this team",
            false,
            None,
        )
    };
    let Ok(Query(query)) = query else {
        return unreadable();
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return unreadable();
    }
    let Ok(UrlPath(run_id)) = run_id else {
        return error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            "no run has that id",
            false,
            None,
        );
    };
    let wanted = run_id.clone();
    match read(&shared, move |reader| reader.run_view(&wanted)).await {
        Ok(Some((_, team, watermark))) => {
            let from = match &query.cursor {
                None => 0,
                Some(cursor) => match team.members.iter().position(|m| &m.role == cursor) {
                    Some(at) => at + 1,
                    None => return unreadable(),
                },
            };
            let page: Vec<_> = team.members.iter().skip(from).take(limit).collect();
            let next = (from + page.len() < team.members.len())
                .then(|| page.last().map(|m| m.role.clone()))
                .flatten();
            (
                StatusCode::OK,
                axum::Json(json!({
                    "items": page,
                    "next_cursor": next,
                    "watermark": watermark,
                })),
            )
                .into_response()
        }
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            format!("run {run_id} does not exist"),
            false,
            None,
        ),
        Err(e) => store_error(&shared, &run_id, e).await,
    }
}

/// The answer to a page query that names no page (openapi has no 400 for these lists: 404, as `GET /v1/runs`).
fn unreadable_page() -> Response {
    error(
        StatusCode::NOT_FOUND,
        "BAD_REQUEST",
        "limit must be 1 to 500, cursor one this host gave",
        false,
        None,
    )
}

/// The answer for a run that does not exist.
fn no_run(run_id: &str) -> Response {
    error(
        StatusCode::NOT_FOUND,
        "RUN_NOT_FOUND",
        format!("run {run_id} does not exist"),
        false,
        None,
    )
}

/// `GET /v1/runs/{run_id}/acceptance` (openapi `listAcceptance`; docs/adr/ADR-029.md decision 5): a page of the run's
/// acceptance projection, each AC in its scope on each of its candidates, the latest candidate first, with the
/// watermark it was read at; at most `limit` (1 to 500, 100 by default) after the opaque `cursor`. A query that cannot
/// be read names no page: 404, as for `GET /v1/runs`.
async fn list_acceptance(
    State(shared): State<Arc<Shared>>,
    run_id: Result<UrlPath<String>, axum::extract::rejection::PathRejection>,
    query: Result<Query<TeamQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return unreadable_page();
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return unreadable_page();
    }
    // The cursor is `<freeze seq>:<AC>` of the last item of the previous page.
    let after = match query.cursor.as_deref().map(|c| c.split_once(':')) {
        None => None,
        Some(Some((seq, ac))) => match seq.parse::<i64>() {
            Ok(seq) if seq >= 0 && !ac.is_empty() => Some((seq, ac.to_owned())),
            _ => return unreadable_page(),
        },
        Some(None) => return unreadable_page(),
    };
    let Ok(UrlPath(run_id)) = run_id else {
        return error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            "no run has that id",
            false,
            None,
        );
    };
    let wanted = run_id.clone();
    match read(&shared, move |reader| {
        reader.acceptance_page(&wanted, limit, after)
    })
    .await
    {
        Ok(Some(page)) => (
            StatusCode::OK,
            axum::Json(json!({
                "items": page.items,
                "next_cursor": page.next.map(|(seq, ac)| format!("{seq}:{ac}")),
                "watermark": page.watermark,
            })),
        )
            .into_response(),
        Ok(None) => no_run(&run_id),
        Err(e) => store_error(&shared, &run_id, e).await,
    }
}

/// `GET /v1/runs/{run_id}/issues` (openapi `listIssues`; docs/adr/ADR-029.md decisions 5 and 10): a page of the run's
/// Issues, newest first, each state evaluated by the state function now, with the watermark it was read at; at most
/// `limit` (1 to 500, 100 by default) older than the opaque `cursor`. A query that cannot be read names no page: 404.
async fn list_issues(
    State(shared): State<Arc<Shared>>,
    run_id: Result<UrlPath<String>, axum::extract::rejection::PathRejection>,
    query: Result<Query<TeamQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    let Ok(Query(query)) = query else {
        return unreadable_page();
    };
    let limit = query.limit.unwrap_or(100);
    if !(1..=500).contains(&limit) {
        return unreadable_page();
    }
    let before = match query.cursor.as_deref().map(str::parse::<i64>) {
        None => None,
        Some(Ok(before)) if before > 0 => Some(before),
        Some(_) => return unreadable_page(),
    };
    let Ok(UrlPath(run_id)) = run_id else {
        return error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            "no run has that id",
            false,
            None,
        );
    };
    let wanted = run_id.clone();
    match read(&shared, move |reader| {
        reader.issues_page(&wanted, limit, before)
    })
    .await
    {
        Ok(Some(page)) => (
            StatusCode::OK,
            axum::Json(json!({
                "items": page.items,
                "next_cursor": page.next.map(|n| n.to_string()),
                "watermark": page.watermark,
            })),
        )
            .into_response(),
        Ok(None) => no_run(&run_id),
        Err(e) => store_error(&shared, &run_id, e).await,
    }
}

/// `GET /v1/runs/{run_id}` (openapi `getRun`): the run with the watermark it was read at.
async fn get_run(
    State(shared): State<Arc<Shared>>,
    run_id: Result<UrlPath<String>, axum::extract::rejection::PathRejection>,
) -> Response {
    // A run id that is not text names no run (openapi getRun answers 404, M2T12R-03).
    let Ok(UrlPath(run_id)) = run_id else {
        return error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            "no run has that id",
            false,
            None,
        );
    };
    let wanted = run_id.clone();
    match read(&shared, move |reader| reader.run_projection(&wanted)).await {
        Ok(Some((run, team, acceptance, watermark))) => (
            StatusCode::OK,
            axum::Json(json!({
                "run_id": run.run_id,
                "state": run.state,
                "revision": run.revision,
                "watermark": watermark,
                "projection": {
                    "mode": run.mode,
                    // The latest candidate's acceptance summary (docs/adr/ADR-029.md decision 5).
                    "acceptance": {
                        "candidate_id": acceptance.candidate.as_ref().map(|c| c.candidate_id.clone()),
                        "candidate_state": acceptance.candidate.as_ref().map(|c| c.state.clone()),
                        "accepted": acceptance.accepted,
                        "passing": acceptance.passing,
                        "total": acceptance.total,
                    },
                    // docs/adr/ADR-022.md decision 18.
                    "team": team.members,
                    "team_revision": team.team_revision,
                    "resume_state": run.resume_state,
                    "blocked_reason": run.blocked_reason,
                    // CANCEL_REQUESTED is shown as CANCELLING (REQ-043 behaviour 3, ADR-021 decision 8h).
                    "label": serde_json::from_value::<RunState>(json!(run.state))
                        .map_or_else(|_| run.state.clone(), |s| hoh_core::lifecycle::label(s).to_owned()),
                },
            })),
        )
            .into_response(),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "RUN_NOT_FOUND",
            format!("run {run_id} does not exist"),
            false,
            None,
        ),
        Err(e) => store_error(&shared, &run_id, e).await,
    }
}

#[derive(serde::Deserialize)]
struct EventsQuery {
    after: Option<i64>,
    run_id: Option<String>,
    kinds: Option<String>,
}

/// The protocol `Event` of a stored one.
fn event_json(event: &EventRow) -> Value {
    json!({
        "schema_version": "1.0",
        "project_id": event.project_id,
        "seq": event.seq,
        "event_id": event.event_id,
        "run_id": event.run_id,
        "attempt_id": null,
        "at": event.created_at,
        "kind": event.kind,
        "payload": event.payload,
    })
}

/// `GET /v1/events` (openapi `streamEvents`): the high watermark H is taken first, then (after, H] is replayed from
/// the database and every later commit is read from it as the watermark moves, so a client sees each committed event
/// once, in seq order, and nothing that did not commit.
async fn stream_events(
    State(shared): State<Arc<Shared>>,
    query: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
    headers: HeaderMap,
) -> Response {
    // A cursor or filter that cannot be read is answered within openapi's codes for streamEvents: 409 (M2T12R-03).
    let Ok(Query(query)) = query else {
        return error(
            StatusCode::CONFLICT,
            "CONFLICT",
            "after must be a non-negative event seq; run_id and kinds must be text",
            false,
            None,
        );
    };
    // A Last-Event-ID that is not ASCII text is unreadable, not absent (M2T12R-62).
    let last_id = headers.get("last-event-id").map(|v| {
        v.to_str()
            .map_err(|_| ())
            .and_then(|v| v.trim().parse::<i64>().map_err(|_| ()))
    });
    let after = match (query.after, last_id) {
        (_, Some(Err(_))) => {
            return error(
                StatusCode::CONFLICT,
                "CONFLICT",
                "Last-Event-ID is not an event seq",
                false,
                None,
            );
        }
        (Some(a), Some(Ok(b))) if a != b => {
            return error(
                StatusCode::CONFLICT,
                "CONFLICT",
                format!("after={a} and Last-Event-ID={b} disagree"),
                false,
                None,
            );
        }
        (Some(a), _) => a,
        (None, Some(Ok(b))) => b,
        (None, None) => 0,
    };
    if after < 0 {
        return error(
            StatusCode::CONFLICT,
            "CONFLICT",
            "after must be a non-negative event seq",
            false,
            None,
        );
    }
    let kinds: Option<Vec<String>> = query
        .kinds
        .map(|k| k.split(',').map(|s| s.trim().to_owned()).collect());
    let mut watermark = shared.store.watermark();
    let high = *watermark.borrow_and_update();
    let state = Stream_ {
        stop: shared.stop.clone(),
        _subscribed: Subscribed::new(shared.clone()),
        shared,
        watermark,
        cursor: after,
        target: high,
        pending: std::collections::VecDeque::new(),
        run_id: query.run_id,
        kinds,
    };
    Sse::new(events(state))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Where one SSE stream stands.
struct Stream_ {
    shared: Arc<Shared>,
    /// Counts the stream while it is open.
    _subscribed: Subscribed,
    /// The host's stop: the stream ends with it.
    stop: tokio::sync::watch::Receiver<bool>,
    watermark: tokio::sync::watch::Receiver<i64>,
    /// The last seq read from the database.
    cursor: i64,
    /// The seq to read up to before waiting for the next commit.
    target: i64,
    pending: std::collections::VecDeque<EventRow>,
    run_id: Option<String>,
    kinds: Option<Vec<String>>,
}

fn events(state: Stream_) -> impl Stream<Item = Result<SseEvent, Infallible>> {
    stream::unfold(state, |mut s| async move {
        loop {
            // Every step looks at the stop, so a stream that is replaying ends with it too (M2T12R-98).
            if *s.stop.borrow() {
                return None;
            }
            if let Some(event) = s.pending.pop_front() {
                let wanted = s
                    .run_id
                    .as_ref()
                    .is_none_or(|id| event.run_id.as_ref() == Some(id))
                    && s.kinds.as_ref().is_none_or(|k| k.contains(&event.kind));
                if !wanted {
                    continue;
                }
                let sse = SseEvent::default()
                    .id(event.seq.to_string())
                    .data(event_json(&event).to_string());
                return Some((Ok(sse), s));
            }
            if s.cursor < s.target {
                let (after, upto) = (s.cursor, s.target);
                let page = read(&s.shared, move |reader| reader.events_after(after, PAGE)).await;
                match page {
                    Ok(rows) if !rows.is_empty() => {
                        for row in rows {
                            if row.seq > upto {
                                break;
                            }
                            s.cursor = row.seq;
                            s.pending.push_back(row);
                        }
                        if s.pending.is_empty() {
                            s.cursor = upto;
                        }
                    }
                    // The watermark said there is more; an empty or failed read ends the stream, the client resumes.
                    _ => return None,
                }
                continue;
            }
            if *s.stop.borrow() {
                return None;
            }
            tokio::select! {
                changed = s.watermark.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                }
                _ = stopped(s.stop.clone()) => return None,
            }
            s.target = *s.watermark.borrow_and_update();
        }
    })
}
