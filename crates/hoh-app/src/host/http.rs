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
    CommandOutcome, EventRow, NewEvent, NewOutbox, PersistError, Reader, Store, SubmitCommand,
};
use hoh_core::protocol::{self, Command, CommandBody, CommandKind, RunState};
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
        .route("/v1/runs/{run_id}", get(get_run))
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
async fn read<T: Send + 'static>(
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
            expected, actual, ..
        } => error(
            StatusCode::CONFLICT,
            "STALE_REVISION",
            match actual {
                Some(actual) => {
                    format!("expected revision {expected}, the run is at revision {actual}")
                }
                None => format!("expected revision {expected}, the run has no revision"),
            },
            false,
            actual,
        ),
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

/// `POST /v1/commands` (openapi `submitCommand`). PauseRun is applied; the other kinds come with their tasks and are
/// refused as not yet supported, with nothing written.
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
    let (CommandBody::PauseRun(reason), Some(run_id)) = (&command.body, &command.run_id) else {
        let kind = command.kind();
        return if kind == CommandKind::PauseRun {
            error(
                StatusCode::BAD_REQUEST,
                "BAD_REQUEST",
                "PauseRun needs a run_id",
                false,
                None,
            )
        } else {
            error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "CAPABILITY_UNSUPPORTED",
                format!("{kind:?} is not supported by this host yet"),
                false,
                None,
            )
        };
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
    let submit = SubmitCommand {
        command_id: command.command_id.clone(),
        idempotency_key: command.idempotency_key.clone(),
        kind: "PauseRun".to_owned(),
        run_id: run_id.clone(),
        expected_revision,
        payload: value.get("payload").cloned().unwrap_or_else(|| json!({})),
        payload_sha256,
        target: RunState::PauseRequested,
        event: NewEvent {
            project_id: shared.project_id.clone(),
            event_id: event_id.clone(),
            run_id: Some(run_id.clone()),
            kind: "run.pause_requested".to_owned(),
            payload: json!({
                "command_id": command.command_id,
                "state": "PAUSE_REQUESTED",
                "reason": reason.reason,
            }),
        },
        outbox: Some(NewOutbox {
            message_id: super::new_id(),
            event: 0,
            run_id: Some(run_id.clone()),
            recipient_role: "commander".to_owned(),
            role_epoch: 1,
            payload: json!({
                "kind": "run.control",
                "control": "PAUSE_REQUESTED",
                "run_id": run_id,
                "command_id": command.command_id,
                "event_id": event_id,
                "reason": reason.reason,
            }),
        }),
    };
    match shared.store.submit_command(submit).await {
        Ok(CommandOutcome::Accepted { result_json, .. }) => {
            shared.hooks.after_commit();
            accepted(result_json, false)
        }
        Ok(CommandOutcome::Replayed { result_json }) => accepted(result_json, true),
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
    match read(&shared, move |reader| reader.snapshot(&wanted)).await {
        Ok(Some((run, watermark))) => (
            StatusCode::OK,
            axum::Json(json!({
                "run_id": run.run_id,
                "state": run.state,
                "revision": run.revision,
                "watermark": watermark,
                "projection": {
                    "mode": run.mode,
                    "resume_state": run.resume_state,
                    "blocked_reason": run.blocked_reason,
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
