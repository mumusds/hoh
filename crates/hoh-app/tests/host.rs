//! M2-T12 Engine host: the local command API, its idempotency and failure paths, the database settings and location
//! checks, and the openapi contract of the endpoints M2 uses (REQ-022, REQ-036, REQ-041, REQ-062; ADR-016).

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::host::{Host, HostConfig, HostHooks};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand, Step};
use hoh_core::persistence::{FaultHooks, PersistError, location};
use hoh_core::protocol::ErrorCode;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

type R<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Set for the child process the exit-code tests start: the project, and what to inject.
const CHILD_PROJECT: &str = "HOH_TEST_HOST_PROJECT";
const CHILD_MOUNTINFO: &str = "HOH_TEST_HOST_MOUNTINFO";
const CHILD_WAL_FAIL: &str = "HOH_TEST_HOST_WAL_FAIL";

/// The product binary's WAL self-test probe.
fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

/// A host configuration for `project` on a free port.
fn config(project: &Path) -> HostConfig {
    let mut config = HostConfig::new(project);
    config.port = 0;
    config.store.wal_probe = Some(probe());
    config
}

/// Run `script` (Python, sqlite3 module) against the project's database; its stdout. Test code: the product never
/// writes outside persistence.
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

/// CREATED runs `ids` in the host's project.
fn seed_runs(host: &Host, project: &Path, ids: &[String]) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=sys.stdin.readline().strip(); \
                  c.executemany(\"INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES (?, ?, 'CREATED', 'AUTO', '2026-11-06T00:00:00Z')\", \
                  [(l.strip(), p) for l in sys.stdin if l.strip()]); c.commit(); c.close()";
    let mut input = format!("{}\n", host.project_id);
    for id in ids {
        input.push_str(id);
        input.push('\n');
    }
    python(&db(project), script, &input).map(drop)
}

/// A query's single value, read outside the product.
fn scalar(project: &Path, sql: &str) -> R<String> {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print(c.execute(sys.stdin.read()).fetchone()[0])";
    Ok(python(&db(project), script, sql)?.trim().to_owned())
}

/// Where a request goes.
#[derive(Clone)]
enum To {
    Tcp(u16),
    Unix(PathBuf),
}

/// A response: status, lower-cased headers, body.
#[derive(Debug)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> R<Value> {
        Ok(serde_json::from_slice(&self.body)?)
    }
}

/// Write `request` and read the whole response (the connection is closed by the server after it).
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    request: Vec<u8>,
) -> R<Vec<u8>> {
    stream.write_all(&request).await?;
    let mut raw = Vec::new();
    // A reset after the request counts as an empty response.
    let _ = stream.read_to_end(&mut raw).await;
    Ok(raw)
}

fn request_bytes(
    to: &To,
    method: &str,
    path: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: &[u8],
) -> Vec<u8> {
    let host = match to {
        To::Tcp(port) => format!("127.0.0.1:{port}"),
        To::Unix(_) => "localhost".to_owned(),
    };
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("X-HoH-Token: {token}\r\n"));
    }
    for (name, value) in extra {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    if !body.is_empty() {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

/// Parse a complete response; `None` when no status line came (the connection closed without a response).
fn parse(raw: &[u8]) -> R<Option<Reply>> {
    let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return Ok(None);
    };
    let head = std::str::from_utf8(&raw[..end])?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .ok_or("no status line")?
        .parse()?;
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let mut body = raw[end + 4..].to_vec();
    if headers
        .iter()
        .any(|(n, v)| n == "transfer-encoding" && v.contains("chunked"))
    {
        body = dechunk(&body);
    }
    Ok(Some(Reply {
        status,
        headers,
        body,
    }))
}

/// The data of a chunked body (as much as arrived).
fn dechunk(mut raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = raw.windows(2).position(|w| w == b"\r\n") {
        let size = std::str::from_utf8(&raw[..line_end])
            .ok()
            .and_then(|s| usize::from_str_radix(s.split(';').next().unwrap_or("").trim(), 16).ok());
        let Some(size) = size else { break };
        let start = line_end + 2;
        if size == 0 || raw.len() < start + size {
            out.extend_from_slice(&raw[start.min(raw.len())..(start + size).min(raw.len())]);
            break;
        }
        out.extend_from_slice(&raw[start..start + size]);
        raw = &raw[(start + size + 2).min(raw.len())..];
    }
    out
}

/// One request; `Ok(None)` when the connection closed without a response.
async fn send(
    to: &To,
    method: &str,
    path: &str,
    token: Option<&str>,
    extra: &[(&str, &str)],
    body: &[u8],
) -> R<Option<Reply>> {
    let request = request_bytes(to, method, path, token, extra, body);
    let raw = match to {
        To::Tcp(port) => {
            exchange(
                tokio::net::TcpStream::connect(("127.0.0.1", *port)).await?,
                request,
            )
            .await?
        }
        To::Unix(path) => exchange(tokio::net::UnixStream::connect(path).await?, request).await?,
    };
    parse(&raw)
}

/// A request that must get a response.
async fn call(to: &To, method: &str, path: &str, token: &str, body: &[u8]) -> R<Reply> {
    send(to, method, path, Some(token), &[], body)
        .await?
        .ok_or_else(|| format!("{method} {path}: no response").into())
}

/// A PauseRun command body.
fn pause(command_id: &str, key: &str, run_id: &str, expected: i64, reason: &str) -> Vec<u8> {
    json!({
        "schema_version": "1.0",
        "command_id": command_id,
        "idempotency_key": key,
        "expected_revision": expected,
        "kind": "PauseRun",
        "run_id": run_id,
        "payload": {"reason": reason},
    })
    .to_string()
    .into_bytes()
}

/// An SSE subscriber reading in the background: the events seen so far, as (seq, Event).
struct Subscriber {
    seen: Arc<Mutex<Vec<(i64, Value)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Subscriber {
    async fn connect(to: To, token: &str, path: &str) -> R<Subscriber> {
        Self::connect_with(to, token, path, &[]).await
    }

    async fn connect_with(
        to: To,
        token: &str,
        path: &str,
        extra: &[(&str, &str)],
    ) -> R<Subscriber> {
        let request = request_bytes(&to, "GET", path, Some(token), extra, b"");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let into = seen.clone();
        let stream: Box<dyn AsyncReadWrite> = match &to {
            To::Tcp(port) => Box::new(tokio::net::TcpStream::connect(("127.0.0.1", *port)).await?),
            To::Unix(path) => Box::new(tokio::net::UnixStream::connect(path).await?),
        };
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut stream = stream;
            if stream.write_all(&request).await.is_err() {
                return;
            }
            let mut raw = Vec::new();
            let mut buf = vec![0u8; 8192];
            let mut ready = Some(ready_tx);
            loop {
                let Ok(n) = stream.read(&mut buf).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                raw.extend_from_slice(&buf[..n]);
                let Ok(Some(reply)) = parse(&raw) else {
                    continue;
                };
                if let Some(ready) = ready.take() {
                    let _ = ready.send(reply.header("content-type").unwrap_or("").to_owned());
                }
                let text = String::from_utf8_lossy(&reply.body).into_owned();
                let mut events = Vec::new();
                for block in text.split("\n\n") {
                    let id = block
                        .lines()
                        .find_map(|l| l.strip_prefix("id:"))
                        .map(str::trim);
                    let data = block
                        .lines()
                        .find_map(|l| l.strip_prefix("data:"))
                        .map(str::trim);
                    if let (Some(id), Some(data)) = (id, data)
                        && let (Ok(seq), Ok(event)) =
                            (id.parse::<i64>(), serde_json::from_str::<Value>(data))
                    {
                        events.push((seq, event));
                    }
                }
                if let Ok(mut seen) = into.lock() {
                    *seen = events;
                }
            }
        });
        let content_type = tokio::time::timeout(Duration::from_secs(5), ready_rx).await??;
        if content_type != "text/event-stream" {
            return Err(format!("the events stream answered {content_type}").into());
        }
        Ok(Subscriber { seen, task })
    }

    fn events(&self) -> Vec<(i64, Value)> {
        self.seen.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The events of `kind` for `run_id` seen so far.
    fn count(&self, run_id: &str, kind: &str) -> usize {
        self.events()
            .iter()
            .filter(|(_, e)| e["run_id"] == run_id && e["kind"] == kind)
            .count()
    }
}

impl Drop for Subscriber {
    fn drop(&mut self) {
        self.task.abort();
    }
}

trait AsyncReadWrite: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncReadWrite for T {}

/// Wait until `done` holds, at most `limit`.
async fn until(limit: Duration, done: impl Fn() -> bool) -> bool {
    let end = Instant::now() + limit;
    while Instant::now() < end {
        if done() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    done()
}

/// The formal database files under `.hoh/` (not the lock, the self-test's own files are removed by it).
fn db_files(project: &Path) -> Vec<String> {
    std::fs::read_dir(project.join(".hoh"))
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".db") || n.ends_with("-wal") || n.ends_with("-shm"))
                .collect()
        })
        .unwrap_or_default()
}

/// The host run in a process of its own (this test binary, the child entry below): exit code and stderr.
fn host_process(
    project: &Path,
    mountinfo: Option<&Path>,
    wal_fail: Option<&str>,
) -> R<(i32, String)> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "host_child",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_PROJECT, project);
    if let Some(mountinfo) = mountinfo {
        command.env(CHILD_MOUNTINFO, mountinfo);
    }
    if let Some(step) = wal_fail {
        command.env(CHILD_WAL_FAIL, step);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    // A host that started would wait for a signal; these hosts must not start.
    let end = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            let out = child.wait_with_output()?;
            let code = status.code().ok_or("killed by a signal")?;
            return Ok((code, String::from_utf8_lossy(&out.stderr).into_owned()));
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the host process did not end".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The test's fault hooks (CR-006): the product's do nothing, these make the faults.
///
/// A disk that is full while the flag is set: the commit fails as SQLITE_FULL would.
#[derive(Debug)]
struct FullDisk(AtomicBool);

impl FaultHooks for FullDisk {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.load(Ordering::SeqCst) {
            Err(PersistError::Full(
                "injected: the file system is full".to_owned(),
            ))
        } else {
            Ok(())
        }
    }
}

/// A WAL self-test whose step fails.
#[derive(Debug)]
struct FailingStep(Step);

impl FaultHooks for FailingStep {
    fn before_wal_step(&self, step: Step) -> Result<(), String> {
        if step == self.0 {
            Err("injected".to_owned())
        } else {
            Ok(())
        }
    }
}

/// A connection dropped after the commit while the flag is set: the response is never written.
#[derive(Debug)]
struct DropAfterCommit(AtomicBool);

impl HostHooks for DropAfterCommit {
    fn after_commit(&self) {
        if self.0.load(Ordering::SeqCst) {
            std::panic::resume_unwind(Box::new("connection dropped after commit"));
        }
    }
}

/// The child entry of [`host_process`]: runs `hoh_app::host::run` and exits with its code.
#[test]
#[ignore = "run by the exit-code tests in a process of their own"]
fn host_child() {
    let Some(project) = std::env::var_os(CHILD_PROJECT) else {
        return;
    };
    let mut config = config(Path::new(&project));
    if let Some(file) = std::env::var_os(CHILD_MOUNTINFO) {
        config.mountinfo = std::fs::read_to_string(file).ok();
    }
    if let Some(step) = std::env::var_os(CHILD_WAL_FAIL) {
        let failing = match step.to_str() {
            Some("shared-memory") => Step::SharedMemory,
            _ => Step::Checkpoint,
        };
        config.store.faults = Arc::new(FailingStep(failing));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let code = runtime.block_on(hoh_app::host::run(config));
    std::process::exit(i32::from(code));
}

/// K+P twice is one command (the same bytes, a replay header the second time, one event); K+P' is 409 KEY_REUSE.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_022_02_same_key_replays_and_another_payload_is_key_reuse() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "0");
    let body = pause("c1", "K", "r1", 0, "P");
    let first = call(&to, "POST", "/v1/commands", &host.token, &body).await?;
    let second = call(&to, "POST", "/v1/commands", &host.token, &body).await?;
    assert_eq!(
        (first.status, second.status),
        (202, 202),
        "{first:?} {second:?}"
    );
    assert_eq!(first.body, second.body, "the replay is not byte-equal");
    assert_eq!(second.header("idempotent-replayed"), Some("true"));
    assert_eq!(first.header("idempotent-replayed"), None);
    let accepted = first.json()?;
    assert_eq!(accepted["command_id"], "c1");
    assert_eq!(accepted["revision"], 1);
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "1");
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM events")?, "1");
    let other = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "K", "r1", 0, "P'"),
    )
    .await?;
    assert_eq!(other.status, 409, "{other:?}");
    assert_eq!(other.json()?["code"], "KEY_REUSE");
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "1");
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM events")?, "1");
    host.shutdown().await;
    Ok(())
}

/// A COMMIT that fails as a full disk would is a retryable error that no subscriber sees, and the run is unchanged;
/// once the fault is off, a new command is 202 and each subscriber sees exactly one event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_036_04_failed_commit_is_retryable_and_reaches_no_subscriber() -> R {
    let dir = tempfile::tempdir()?;
    let full = Arc::new(FullDisk(AtomicBool::new(true)));
    let mut config = config(dir.path());
    config.store.faults = full.clone();
    let host = Host::start(config).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let tcp = To::Tcp(host.port);
    let sse = Subscriber::connect(tcp.clone(), &host.token, "/v1/events").await?;
    // The TUI's projection: a fold of the events it reads over the socket.
    let projection =
        Subscriber::connect(To::Unix(host.socket.clone()), &host.token, "/v1/events").await?;
    let before = call(&tcp, "GET", "/v1/runs/r1", &host.token, b"")
        .await?
        .json()?;
    let failed = call(
        &tcp,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "k1", "r1", 0, "full"),
    )
    .await?;
    assert_ne!(failed.status, 202, "{failed:?}");
    let error = failed.json()?;
    assert_eq!(error["retryable"], true, "{error}");
    assert_eq!(error["code"], "DB_FULL", "{error}");
    tokio::time::sleep(Duration::from_secs(5)).await;
    assert_eq!(sse.count("r1", "run.pause_requested"), 0);
    assert_eq!(projection.count("r1", "run.pause_requested"), 0);
    let after = call(&tcp, "GET", "/v1/runs/r1", &host.token, b"")
        .await?
        .json()?;
    assert_eq!(
        (&after["state"], &after["revision"]),
        (&before["state"], &before["revision"])
    );
    full.0.store(false, Ordering::SeqCst);
    let applied = call(
        &tcp,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c2", "k2", "r1", 0, "room"),
    )
    .await?;
    assert_eq!(applied.status, 202, "{applied:?}");
    assert!(
        until(Duration::from_secs(5), || sse
            .count("r1", "run.pause_requested")
            == 1
            && projection.count("r1", "run.pause_requested") == 1)
        .await,
        "{:?} {:?}",
        sse.events(),
        projection.events()
    );
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(sse.count("r1", "run.pause_requested"), 1);
    assert_eq!(projection.count("r1", "run.pause_requested"), 1);
    drop((sse, projection));
    host.shutdown().await;
    Ok(())
}

/// A connection dropped after COMMIT and before the 202: the retry gets the first result, the command has one event,
/// another payload under the key is KEY_REUSE, and the key has one row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_041_04_retry_after_disconnect_returns_first_result() -> R {
    let dir = tempfile::tempdir()?;
    let dropping = Arc::new(DropAfterCommit(AtomicBool::new(true)));
    let mut config = config(dir.path());
    config.hooks = dropping.clone();
    let host = Host::start(config).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let body = pause("c1", "K", "r1", 0, "X");
    let lost = send(&to, "POST", "/v1/commands", Some(&host.token), &[], &body).await?;
    assert!(lost.is_none(), "a response came: {lost:?}");
    // Before any retry: the first request committed (its command, result, event and the run's move are there).
    let stored_text = scalar(
        dir.path(),
        "SELECT result_json FROM commands WHERE idempotency_key = 'K'",
    )?;
    let stored: Value = serde_json::from_str(&stored_text)?;
    assert_eq!(
        (&stored["command_id"], &stored["revision"]),
        (&json!("c1"), &json!(1)),
        "{stored}"
    );
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT count(*) FROM events WHERE json_extract(payload_json, '$.command_id') = 'c1'"
        )?,
        "1"
    );
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT state || ' ' || revision FROM runs WHERE run_id = 'r1'"
        )?,
        "PAUSE_REQUESTED 1"
    );
    dropping.0.store(false, Ordering::SeqCst);
    let retry = call(&to, "POST", "/v1/commands", &host.token, &body).await?;
    assert_eq!(retry.status, 202, "{retry:?}");
    // The retry is the first result replayed, byte for byte.
    assert_eq!(retry.header("idempotent-replayed"), Some("true"));
    assert_eq!(String::from_utf8(retry.body.clone())?, stored_text);
    let retried = retry.json()?;
    assert_eq!(
        (&retried["command_id"], &retried["revision"]),
        (&stored["command_id"], &stored["revision"])
    );
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT count(*) FROM events WHERE json_extract(payload_json, '$.command_id') = 'c1'"
        )?,
        "1"
    );
    let reuse = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "K", "r1", 0, "Y"),
    )
    .await?;
    assert_eq!(reuse.status, 409, "{reuse:?}");
    assert_eq!(reuse.json()?["code"], "KEY_REUSE");
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT count(*) FROM commands WHERE idempotency_key = 'K'"
        )?,
        "1"
    );
    host.shutdown().await;
    Ok(())
}

/// Every connection has the required settings and the readers refuse writes; with an outside connection holding the
/// write lock, a command waits (202 when the lock goes within the busy timeout, 503 DB_BUSY after 5 to 6 s when it
/// does not) while reads answer within 500 ms.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_062_01_settings_busy_writer_responsive_reads() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned(), "r2".to_owned()])?;
    let diagnostics = host.diagnostics().await?;
    for pragmas in std::iter::once(&diagnostics.writer).chain(&diagnostics.readers) {
        assert_eq!(
            (
                pragmas.journal_mode.as_str(),
                pragmas.foreign_keys,
                pragmas.busy_timeout_ms,
                pragmas.synchronous
            ),
            ("wal", 1, 5000, 2),
            "{pragmas:?}"
        );
    }
    assert!(!diagnostics.readers.is_empty());
    assert!(diagnostics.readers_read_only, "a reader accepted a write");
    let to = To::Tcp(host.port);
    for (hold, run, expect_busy) in [(2.0, "r1", false), (7.0, "r2", true)] {
        let before = call(&to, "GET", &format!("/v1/runs/{run}"), &host.token, b"")
            .await?
            .json()?;
        let mut locker = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys,time; c=sqlite3.connect(sys.argv[1], isolation_level=None); \
                 c.execute('BEGIN IMMEDIATE'); print('locked', flush=True); time.sleep(float(sys.argv[2])); \
                 c.execute('COMMIT')",
            ])
            .arg(db(dir.path()))
            .arg(hold.to_string())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufRead::read_line(
            &mut std::io::BufReader::new(locker.stdout.take().ok_or("no stdout")?),
            &mut line,
        )?;
        assert_eq!(line.trim(), "locked");
        let started = Instant::now();
        let command = tokio::spawn({
            let to = to.clone();
            let token = host.token.clone();
            let body = pause(&format!("c-{run}"), &format!("k-{run}"), run, 0, "busy");
            async move { call(&to, "POST", "/v1/commands", &token, &body).await }
        });
        tokio::time::sleep(Duration::from_millis(300)).await;
        let read_at = Instant::now();
        let read = call(&to, "GET", &format!("/v1/runs/{run}"), &host.token, b"").await?;
        let read_took = read_at.elapsed();
        assert_eq!(read.status, 200);
        assert!(
            read_took <= Duration::from_millis(500),
            "GET took {read_took:?}"
        );
        let reply = command.await??;
        let took = started.elapsed();
        if expect_busy {
            assert_eq!(reply.status, 503, "{reply:?}");
            let error = reply.json()?;
            assert_eq!(
                (&error["code"], &error["retryable"]),
                (&json!("DB_BUSY"), &json!(true))
            );
            assert!(
                took >= Duration::from_secs(5) && took <= Duration::from_secs(6),
                "503 after {took:?}"
            );
            let after = call(&to, "GET", &format!("/v1/runs/{run}"), &host.token, b"")
                .await?
                .json()?;
            assert_eq!(
                (&after["state"], &after["revision"]),
                (&before["state"], &before["revision"])
            );
        } else {
            assert_eq!(reply.status, 202, "{reply:?}");
            assert!(took <= Duration::from_secs(5), "202 after {took:?}");
        }
        locker.wait()?;
    }
    host.shutdown().await;
    Ok(())
}

/// With a writer queue of 8, 8 clients sending 500 state-changing commands each get 202 or a retryable error, never
/// SQLITE_BUSY; every 202 has exactly one event and its outbox row, and event seqs are unique and increasing.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn test_062_02_queue_8_under_4000_commands_loses_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let mut config = config(dir.path());
    config.store.queue = 8;
    let host = Host::start(config).await?;
    let runs: Vec<String> = (0..4000).map(|n| format!("run-{n}")).collect();
    seed_runs(&host, dir.path(), &runs)?;
    let to = To::Tcp(host.port);
    let mut clients = Vec::new();
    for client in 0..8 {
        let (to, token) = (to.clone(), host.token.clone());
        clients.push(tokio::spawn(async move {
            let mut accepted = Vec::new();
            let mut retryable = Vec::new();
            for n in 0..500 {
                let run = format!("run-{}", client * 500 + n);
                let id = format!("cmd-{client}-{n}");
                let reply = call(
                    &to,
                    "POST",
                    "/v1/commands",
                    &token,
                    &pause(&id, &id, &run, 0, "load"),
                )
                .await?;
                let text = String::from_utf8_lossy(&reply.body).into_owned();
                if text.contains("SQLITE_BUSY") {
                    return Err(format!("a client saw SQLITE_BUSY: {text}").into());
                }
                match reply.status {
                    202 => accepted.push(
                        json!({"id": id, "run": run, "body": text, "client": client, "n": n}),
                    ),
                    503 if reply.json()?["retryable"] == true => {
                        retryable.push(json!({"id": id, "run": run}))
                    }
                    other => return Err(format!("{id}: {other} {text}").into()),
                }
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>((accepted, retryable))
        }));
    }
    let (mut accepted, mut retried) = (Vec::new(), Vec::new());
    for client in clients {
        let (ok, again) = client.await??;
        accepted.extend(ok);
        retried.extend(again);
    }
    assert_eq!(accepted.len() + retried.len(), 4000);
    // Each 202: its command row with the exact result returned, the run moved to that revision, one event and one
    // outbox row pointing at it. Each retryable answer: nothing written, the run unchanged. Seqs are unique, and the
    // commands one client sent one after another got increasing seqs (commit order is seq order, M2T12R-06).
    let check = "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1]); d=json.load(sys.stdin); \
                 cmd={r[0]: r[1:] for r in c.execute('SELECT command_id, run_id, result_json FROM commands')}; \
                 run={r[0]: r[1:] for r in c.execute('SELECT run_id, state, revision FROM runs')}; \
                 ev={}; [ev.setdefault(r[0], []).append(r[1]) for r in c.execute(\"SELECT json_extract(payload_json,'$.command_id'), seq FROM events\")]; \
                 ob={}; [ob.setdefault(r[0], []).append(r[1]) for r in c.execute(\"SELECT json_extract(payload_json,'$.command_id'), event_seq FROM outbox\")]; \
                 seqs=[r[0] for r in c.execute('SELECT seq FROM events ORDER BY rowid')]; \
                 ok=d['accepted']; no=d['retried']; \
                 print(json.dumps({'commands': len(cmd), \
                   'missing_command': sum(1 for a in ok if a['id'] not in cmd), \
                   'result_differs': sum(1 for a in ok if a['id'] in cmd and (cmd[a['id']][0]!=a['run'] or cmd[a['id']][1]!=a['body'])), \
                   'run_not_moved': sum(1 for a in ok if tuple(run.get(a['run'], ())) != ('PAUSE_REQUESTED', json.loads(a['body'])['revision'])), \
                   'events_not_one': sum(1 for a in ok if len(ev.get(a['id'], []))!=1), \
                   'outbox_not_its_event': sum(1 for a in ok if ob.get(a['id'], []) != ev.get(a['id'], [None])), \
                   'retried_wrote': sum(1 for a in no if a['id'] in cmd or a['id'] in ev or tuple(run.get(a['run'], ())) != ('CREATED', 0)), \
                   'events': len(seqs), 'unique': len(set(seqs))==len(seqs), \
                   'client_order': all(all(x<y for x,y in zip(l, l[1:])) for l in [[ev[a['id']][0] for a in sorted((a for a in ok if a['client']==k and len(ev.get(a['id'], []))==1), key=lambda a: a['n'])] for k in range(8)])}))";
    let input = json!({"accepted": accepted, "retried": retried}).to_string();
    let summary: Value = serde_json::from_str(&python(&db(dir.path()), check, &input)?)?;
    assert_eq!(
        summary,
        json!({"commands": accepted.len(), "missing_command": 0, "result_differs": 0, "run_not_moved": 0,
               "events_not_one": 0, "outbox_not_its_event": 0, "retried_wrote": 0,
               "events": accepted.len(), "unique": true, "client_order": true})
    );
    host.shutdown().await;
    Ok(())
}

/// The location check refuses the network file systems of the fixture and passes ext4 and tmpfs; a host started on
/// such a location exits non-zero naming DB_ON_NETWORK_FS, with no database file made.
#[test]
fn test_062_04_network_fs_refused_and_no_db_files() -> R {
    let fixture = "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n\
                   2 1 0:61 / /p1 rw - nfs4 server:/a rw\n\
                   3 1 0:62 / /p2 rw - cifs //server/b rw\n\
                   4 1 0:63 / /p3 rw - fuse.sshfs user@server:/c rw\n\
                   5 1 8:2 / /p4 rw - ext4 /dev/sda2 rw\n\
                   6 1 0:64 / /p5 rw - tmpfs tmpfs rw\n";
    for (path, refused, fstype) in [
        ("/p1", true, "nfs4"),
        ("/p2", true, "cifs"),
        ("/p3", true, "fuse.sshfs"),
        ("/p4", false, "ext4"),
        ("/p5", false, "tmpfs"),
    ] {
        let checked = location::check(&Path::new(path).join(".hoh"), fixture);
        match checked {
            Err(found) => {
                assert!(refused, "{path} refused on {found}");
                assert_eq!(found, fstype);
                let e = PersistError::OnNetworkFs {
                    path: path.into(),
                    fstype: found,
                };
                assert_eq!(e.error_code(), Some(ErrorCode::DbOnNetworkFs));
                assert!(e.to_string().contains("DB_ON_NETWORK_FS"), "{e}");
            }
            Ok(found) => {
                assert!(!refused, "{path} passed");
                assert_eq!(found.as_deref(), Some(fstype));
            }
        }
    }
    // The host on /p1: the project directory is mounted from nfs4 in the fixture.
    let dir = tempfile::tempdir()?;
    let p1 = dir.path().canonicalize()?.join("p1");
    std::fs::create_dir(&p1)?;
    let mountinfo = dir.path().join("mountinfo");
    std::fs::write(
        &mountinfo,
        format!(
            "1 0 8:1 / / rw - ext4 /dev/sda1 rw\n2 1 0:61 / {} rw - nfs4 server:/a rw\n",
            p1.display()
        ),
    )?;
    let (code, stderr) = host_process(&p1, Some(&mountinfo), None)?;
    assert_ne!(code, 0, "{stderr}");
    assert!(stderr.contains("DB_ON_NETWORK_FS"), "{stderr}");
    assert_eq!(db_files(&p1), Vec::<String>::new());
    Ok(())
}

/// On ext4 the self-test passes and commands are accepted; a self-test whose shared memory or checkpoint step fails
/// ends the host non-zero naming DB_WAL_UNSUPPORTED, with no formal database file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_062_06_wal_self_test_failure_refuses_start() -> R {
    let dir = tempfile::tempdir()?;
    // /p4 on ext4, as the Given says (the temporary directory may be tmpfs, M2T12R-07).
    let ext4 = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR"))?;
    let p4 = ext4.path().canonicalize()?.join("p4");
    std::fs::create_dir(&p4)?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo")?;
    assert_eq!(
        location::check(&p4, &mounts),
        Ok(Some("ext4".to_owned())),
        "{}",
        p4.display()
    );
    let host = Host::start(config(&p4)).await?;
    seed_runs(&host, &p4, &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let reply = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "k1", "r1", 0, "p4"),
    )
    .await?;
    assert_eq!(reply.status, 202, "{reply:?}");
    let run = call(&to, "GET", "/v1/runs/r1", &host.token, b"")
        .await?
        .json()?;
    assert_eq!(run["state"], "PAUSE_REQUESTED");
    host.shutdown().await;
    for (name, step) in [("p6", "shared-memory"), ("p7", "checkpoint")] {
        let project = dir.path().join(name);
        std::fs::create_dir(&project)?;
        let (code, stderr) = host_process(&project, None, Some(step))?;
        assert_ne!(code, 0, "{name}: {stderr}");
        assert!(stderr.contains("DB_WAL_UNSUPPORTED"), "{name}: {stderr}");
        assert_eq!(db_files(&project), Vec::<String>::new(), "{name}");
        // Nothing was served: no socket, no host.json.
        assert!(!project.join(".hoh/host.sock").exists(), "{name}");
        assert!(!project.join(".hoh/host.json").exists(), "{name}");
    }
    Ok(())
}

/// The fault hook traits and the one product implementation each may have.
const HOOKS: [(&str, &str); 3] = [
    ("FaultHooks", "NoFaults"),
    ("HostHooks", "NoHostFaults"),
    ("ProbeBehaviour", "NormalProbe"),
];

/// Violations of the hook rule in one product source file: an implementation of a hook trait other than its
/// product type, or the product type with a body (it must use the no-op defaults). Inline `#[cfg(test)]` modules are
/// test code and skipped.
fn hook_violations(source: &str) -> Vec<String> {
    let mut product = String::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if line.trim() == "#[cfg(test)]"
            && lines
                .peek()
                .is_some_and(|next| next.trim_start().starts_with("mod "))
        {
            // The test module runs to the end of the file (the convention of this workspace).
            break;
        }
        product.push_str(line);
        product.push('\n');
    }
    let mut violations = Vec::new();
    for (hook, only) in HOOKS {
        let needle = format!("impl {hook} for ");
        let mut rest = product.as_str();
        while let Some(at) = rest.find(&needle) {
            let after = &rest[at + needle.len()..];
            let ty: String = after
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let body = after[ty.len()..].trim_start();
            if ty != only {
                violations.push(format!("impl {hook} for {ty}"));
            } else if !body.starts_with("{}") {
                violations.push(format!("impl {hook} for {ty} has a body"));
            }
            rest = after;
        }
    }
    violations
}

/// Every product source file of the workspace, without unit-test files.
fn product_sources() -> R<Vec<PathBuf>> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut found = Vec::new();
    let mut stack = vec![
        crates.join("hoh-core/src"),
        crates.join("hoh-agents/src"),
        crates.join("hoh-app/src"),
    ];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs")
                && path.file_name().is_some_and(|n| n != "tests.rs")
            {
                found.push(path);
            }
        }
    }
    Ok(found)
}

/// The product implements each fault hook only with its no-op type, which keeps the trait's no-op defaults: every
/// implementation that makes a fault is in a test target (CR-006 F2, F4).
#[test]
fn only_the_no_op_fault_hooks_are_in_product_code() -> R {
    let mut seen = std::collections::BTreeMap::new();
    let mut violations = Vec::new();
    for path in product_sources()? {
        let source = std::fs::read_to_string(&path)?;
        for v in hook_violations(&source) {
            violations.push(format!("{}: {v}", path.display()));
        }
        for (hook, only) in HOOKS {
            if source.contains(&format!("impl {hook} for {only} {{}}")) {
                *seen.entry(hook).or_insert(0) += 1;
            }
        }
    }
    assert_eq!(violations, Vec::<String>::new());
    assert_eq!(
        seen,
        HOOKS
            .iter()
            .map(|(hook, _)| (*hook, 1))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "each hook has its product type exactly once"
    );
    // The scan itself: a no-op type with a body, another type, and test modules.
    assert_eq!(
        hook_violations(
            "impl FaultHooks for NoFaults {\n    fn before_commit(&self) -> R { Err(x) }\n}\n"
        ),
        vec!["impl FaultHooks for NoFaults has a body".to_owned()]
    );
    assert_eq!(
        hook_violations("impl HostHooks for Sneaky {}\n"),
        vec!["impl HostHooks for Sneaky".to_owned()]
    );
    assert_eq!(
        hook_violations(
            "impl ProbeBehaviour for NormalProbe {}\n#[cfg(test)]\nmod tests {\n    impl ProbeBehaviour for Stale {}\n}\n"
        ),
        Vec::<String>::new()
    );
    Ok(())
}

/// The product's hooks do nothing when called: commits and every self-test step go ahead, the response is sent.
#[test]
fn the_product_hooks_do_nothing() {
    use hoh_app::host::NoHostFaults;
    use hoh_core::persistence::NoFaults;
    assert!(NoFaults.before_commit().is_ok());
    for step in [
        Step::Open,
        Step::EnableWal,
        Step::Write,
        Step::SharedMemory,
        Step::Locking,
        Step::Checkpoint,
    ] {
        assert_eq!(NoFaults.before_wal_step(step), Ok(()), "{step:?}");
    }
    let answered = std::panic::catch_unwind(|| NoHostFaults.after_commit());
    assert!(answered.is_ok());
    // What a product configuration carries.
    let config = HostConfig::new(Path::new("/project"));
    assert_eq!(format!("{:?}", config.hooks), "NoHostFaults");
    assert_eq!(format!("{:?}", config.store.faults), "NoFaults");
}

/// The openapi schema at `pointer` (a response body), with its references made local: protocol `$defs` and the
/// components it uses are copied in, so the product's validator checks it as one document.
fn openapi_schema(pointer: &str) -> R<hoh_core::schema::Schema> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
    let openapi: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("openapi.json"))?)?;
    let protocol: Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("protocol.schema.json"))?)?;
    let schema = openapi
        .pointer(pointer)
        .ok_or(format!("no {pointer}"))?
        .clone();
    let mut defs = protocol["$defs"]
        .as_object()
        .cloned()
        .ok_or("no protocol $defs")?;
    for (name, value) in openapi["components"]["schemas"]
        .as_object()
        .ok_or("no components")?
    {
        defs.insert(format!("Api{name}"), value.clone());
    }
    let mut document = schema;
    document["$defs"] = Value::Object(defs);
    let text = document
        .to_string()
        .replace("./protocol.schema.json#/$defs/", "#/$defs/")
        .replace("#/components/schemas/", "#/$defs/Api");
    Ok(hoh_core::schema::Schema::new(serde_json::from_str(&text)?)?)
}

fn conforms(schema: &hoh_core::schema::Schema, body: &Value) -> R {
    schema
        .validate(body)
        .map_err(|violations| format!("{body} violates the contract: {violations:?}").into())
}

/// submitCommand and getRun answer as openapi says: 202 bodies, a replay's header, and Error bodies for 400, 409,
/// 422 and 404.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn commands_and_runs_follow_the_openapi_contract() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let accepted = openapi_schema(
        "/paths/~1v1~1commands/post/responses/202/content/application~1json/schema",
    )?;
    let error = openapi_schema("/components/schemas/Error")?;
    let run = openapi_schema(
        "/paths/~1v1~1runs~1{run_id}/get/responses/200/content/application~1json/schema",
    )?;
    let body = pause("c1", "k1", "r1", 0, "contract");
    let first = call(&to, "POST", "/v1/commands", &host.token, &body).await?;
    assert_eq!(first.status, 202);
    assert_eq!(first.header("content-type"), Some("application/json"));
    conforms(&accepted, &first.json()?)?;
    let replay = call(&to, "POST", "/v1/commands", &host.token, &body).await?;
    assert_eq!(
        (replay.status, replay.header("idempotent-replayed")),
        (202, Some("true"))
    );
    conforms(&accepted, &replay.json()?)?;
    for (request, status, code) in [
        (b"not json".to_vec(), 400, "BAD_REQUEST"),
        (br#"{"schema_version":"1.0"}"#.to_vec(), 400, "BAD_REQUEST"),
        (pause("c1", "k1", "r1", 0, "other"), 409, "KEY_REUSE"),
        (pause("c2", "k2", "r1", 0, "stale"), 409, "STALE_REVISION"),
        (pause("c3", "k3", "missing", 0, "none"), 409, "RUN_NOT_FOUND"),
        (
            json!({"schema_version": "1.0", "command_id": "c4", "idempotency_key": "k4", "expected_revision": 1,
                   "kind": "ResumeRun", "run_id": "r1", "payload": {"reason": "later"}})
            .to_string()
            .into_bytes(),
            422,
            "CAPABILITY_UNSUPPORTED",
        ),
    ] {
        let reply = call(&to, "POST", "/v1/commands", &host.token, &request).await?;
        assert_eq!(reply.status, status, "{reply:?}");
        let body = reply.json()?;
        conforms(&error, &body)?;
        assert_eq!(body["code"], code, "{body}");
    }
    let stale = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c5", "k5", "r1", 0, "stale"),
    )
    .await?
    .json()?;
    assert_eq!(stale["current_revision"], 1, "{stale}");
    let got = call(&to, "GET", "/v1/runs/r1", &host.token, b"").await?;
    assert_eq!(got.status, 200);
    let got = got.json()?;
    conforms(&run, &got)?;
    assert_eq!(
        (&got["state"], &got["revision"]),
        (&json!("PAUSE_REQUESTED"), &json!(1))
    );
    let missing = call(&to, "GET", "/v1/runs/missing", &host.token, b"").await?;
    assert_eq!(missing.status, 404);
    let missing = missing.json()?;
    conforms(&error, &missing)?;
    assert_eq!(missing["code"], "RUN_NOT_FOUND");
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "1");
    host.shutdown().await;
    Ok(())
}

/// Every route refuses a request without the token, on the port one whose Host is not the bound loopback address or
/// whose Origin is elsewhere, and on the socket one with any Origin, with 403 FORBIDDEN; `/healthz` needs no token.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_are_checked_before_anything_else() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let tcp = To::Tcp(host.port);
    let unix = To::Unix(host.socket.clone());
    let error = openapi_schema("/components/schemas/Error")?;
    let body = pause("c1", "k1", "r1", 0, "forbidden");
    let rebound = format!("evil.example:{}", host.port);
    let elsewhere = "http://evil.example";
    let wrong = "0".repeat(64);
    for (to, token, extra, method, path, payload) in [
        (&tcp, None, vec![], "POST", "/v1/commands", body.as_slice()),
        (
            &tcp,
            Some(wrong.as_str()),
            vec![],
            "GET",
            "/v1/runs/r1",
            b"".as_slice(),
        ),
        (
            &tcp,
            Some(host.token.as_str()),
            vec![("Origin", elsewhere)],
            "POST",
            "/v1/commands",
            body.as_slice(),
        ),
        (&unix, None, vec![], "GET", "/v1/events", b"".as_slice()),
        (
            &unix,
            Some(host.token.as_str()),
            vec![("Origin", elsewhere)],
            "GET",
            "/v1/runs/r1",
            b"".as_slice(),
        ),
    ] {
        let reply = send(to, method, path, token, &extra, payload)
            .await?
            .ok_or("no response")?;
        assert_eq!(reply.status, 403, "{method} {path} {extra:?}: {reply:?}");
        let body = reply.json()?;
        conforms(&error, &body)?;
        assert_eq!(body["code"], "FORBIDDEN");
    }
    // A request naming another Host (DNS rebinding): refused before its token is looked at.
    let rebinding = format!(
        "GET /v1/runs/r1 HTTP/1.1\r\nHost: {rebound}\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",
        host.token
    );
    let raw = exchange(
        tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?,
        rebinding.into_bytes(),
    )
    .await?;
    let reply = parse(&raw)?.ok_or("no response")?;
    assert_eq!(reply.status, 403, "{reply:?}");
    conforms(&error, &reply.json()?)?;
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "0");
    for to in [&tcp, &unix] {
        let health = send(to, "GET", "/healthz", None, &[], b"")
            .await?
            .ok_or("no response")?;
        assert_eq!(health.status, 200, "{health:?}");
    }
    host.shutdown().await;
    Ok(())
}

/// streamEvents: an SSE stream whose ids are event seqs and whose data are protocol Events, replaying (after, H] and
/// then going live; `after` and Last-Event-ID that disagree are 409.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn events_follow_the_openapi_contract() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(
        &host,
        dir.path(),
        &["r1".to_owned(), "r2".to_owned(), "r3".to_owned()],
    )?;
    let to = To::Tcp(host.port);
    let error = openapi_schema("/components/schemas/Error")?;
    let protocol_event = {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts");
        let protocol: Value =
            serde_json::from_str(&std::fs::read_to_string(root.join("protocol.schema.json"))?)?;
        let mut document = json!({"$ref": "#/$defs/Event"});
        document["$defs"] = protocol["$defs"].clone();
        hoh_core::schema::Schema::new(document)?
    };
    for (n, run) in ["r1", "r2"].iter().enumerate() {
        let reply = call(
            &to,
            "POST",
            "/v1/commands",
            &host.token,
            &pause(&format!("c{n}"), &format!("k{n}"), run, 0, "replay"),
        )
        .await?;
        assert_eq!(reply.status, 202);
    }
    let first_seq: i64 = scalar(dir.path(), "SELECT min(seq) FROM events")?.parse()?;
    // After the first event: the second is replayed, the third arrives live.
    let subscriber = Subscriber::connect(
        to.clone(),
        &host.token,
        &format!("/v1/events?after={first_seq}"),
    )
    .await?;
    assert!(
        until(Duration::from_secs(5), || subscriber.events().len() == 1).await,
        "{:?}",
        subscriber.events()
    );
    let reply = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c9", "k9", "r3", 0, "live"),
    )
    .await?;
    assert_eq!(reply.status, 202);
    assert!(
        until(Duration::from_secs(5), || subscriber.events().len() == 2).await,
        "{:?}",
        subscriber.events()
    );
    let events = subscriber.events();
    let runs: Vec<&Value> = events.iter().map(|(_, e)| &e["run_id"]).collect();
    assert_eq!(runs, vec![&json!("r2"), &json!("r3")]);
    for (seq, event) in &events {
        conforms(&protocol_event, event)?;
        assert_eq!(event["seq"], *seq);
        assert!(*seq > first_seq);
    }
    let filtered = Subscriber::connect(
        to.clone(),
        &host.token,
        "/v1/events?run_id=r3&kinds=run.pause_requested",
    )
    .await?;
    assert!(
        until(Duration::from_secs(5), || filtered.events().len() == 1).await,
        "{:?}",
        filtered.events()
    );
    assert_eq!(filtered.events()[0].1["run_id"], "r3");
    let conflict = send(
        &to,
        "GET",
        "/v1/events?after=1",
        Some(&host.token),
        &[("Last-Event-ID", "2")],
        b"",
    )
    .await?
    .ok_or("no response")?;
    assert_eq!(conflict.status, 409, "{conflict:?}");
    conforms(&error, &conflict.json()?)?;
    drop((subscriber, filtered));
    host.shutdown().await;
    Ok(())
}

/// A host stops with event streams open and a connection that never sent a whole request: the streams end, the
/// connections get the grace, host.json and the socket go, and the lock is free again (M2T12R-01).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_stops_with_subscribers_and_half_requests() -> R {
    let dir = tempfile::tempdir()?;
    // Event streams end with the stop itself, well before the grace (M2T12R-01).
    let host = Host::start(config(dir.path())).await?;
    let tcp = Subscriber::connect(To::Tcp(host.port), &host.token, "/v1/events").await?;
    let unix =
        Subscriber::connect(To::Unix(host.socket.clone()), &host.token, "/v1/events").await?;
    let socket = host.socket.clone();
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), host.shutdown()).await?;
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stopped after {:?}",
        started.elapsed()
    );
    assert!(!socket.exists());
    assert!(!dir.path().join(".hoh/host.json").exists());
    drop((tcp, unix));
    // A connection that never sends a whole request is ended after the grace; the lock is free again.
    let host = Host::start(config(dir.path())).await?;
    assert_eq!(host.host_epoch, 2);
    let mut half = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    half.write_all(
        format!(
            "GET /v1/runs/r1 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n",
            host.port
        )
        .as_bytes(),
    )
    .await?;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), host.shutdown()).await?;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "stopped after {:?}",
        started.elapsed()
    );
    assert!(!dir.path().join(".hoh/host.json").exists());
    let next = Host::start(config(dir.path())).await?;
    next.shutdown().await;
    drop(half);
    Ok(())
}

/// A host process ends on SIGTERM though a client is subscribed, and removes host.json and the socket.
#[test]
fn a_host_process_ends_on_sigterm_with_a_subscriber() -> R {
    let dir = tempfile::tempdir()?;
    let mut child = Command::new(std::env::current_exe()?)
        .args([
            "--exact",
            "host_child",
            "--ignored",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_PROJECT, dir.path())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let host_json = dir.path().join(".hoh/host.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !host_json.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let info: Value = serde_json::from_str(&std::fs::read_to_string(&host_json)?)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let subscriber = runtime.block_on(Subscriber::connect(
        To::Unix(PathBuf::from(info["socket"].as_str().ok_or("no socket")?)),
        info["token"].as_str().ok_or("no token")?,
        "/v1/events",
    ))?;
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    let end = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            return Err("the host did not end on SIGTERM".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(subscriber);
    assert_eq!(status.code(), Some(0));
    assert!(!host_json.exists());
    assert!(!dir.path().join(".hoh/host.sock").exists());
    Ok(())
}

/// What a running host leaves for its clients (ADR-016 decision 1): host.json readable by the owner only, written
/// whole, naming the pid, project, epoch, port, socket and token; the socket also 0600; a second host in the project
/// exits 69 naming the holder (M2T12R-04).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_leaves_its_files_and_holds_the_project() -> R {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let files = dir.path().join(".hoh");
    let mode = |path: &Path| std::fs::metadata(path).map(|m| m.permissions().mode() & 0o777);
    assert_eq!(mode(&files.join("host.json"))?, 0o600);
    assert_eq!(mode(&host.socket)?, 0o600);
    let info: Value = serde_json::from_str(&std::fs::read_to_string(files.join("host.json"))?)?;
    assert_eq!(
        info,
        json!({"pid": std::process::id(), "project_id": host.project_id, "host_epoch": host.host_epoch,
               "port": host.port, "socket": host.socket, "token": host.token})
    );
    assert_eq!(host.token.len(), 64);
    let leftovers: Vec<String> = std::fs::read_dir(&files)?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("host.json."))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
    let second = Host::start(config(dir.path())).await;
    let refused = second.err().ok_or("a second host started")?;
    assert_eq!(refused.exit_code, 69, "{refused}");
    assert!(
        refused
            .message
            .contains(&format!("pid {}", std::process::id())),
        "{refused}"
    );
    host.shutdown().await;
    Ok(())
}

/// A socket left by a host that did not clean up is removed at the next start; a file that is not a socket is left
/// and the start refused (ADR-016 decision 1, M2T12R-04).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stale_socket_is_replaced_and_anything_else_is_left() -> R {
    let dir = tempfile::tempdir()?;
    let files = dir.path().join(".hoh");
    std::fs::create_dir_all(&files)?;
    drop(std::os::unix::net::UnixListener::bind(
        files.join("host.sock"),
    )?);
    assert!(files.join("host.sock").exists());
    let host = Host::start(config(dir.path())).await?;
    let reply = call(
        &To::Unix(host.socket.clone()),
        "GET",
        "/healthz",
        &host.token,
        b"",
    )
    .await?;
    assert_eq!(reply.status, 200);
    host.shutdown().await;
    std::fs::write(files.join("host.sock"), "not a socket")?;
    let refused = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("started over a file")?;
    assert!(refused.message.contains("is not a socket"), "{refused}");
    assert_eq!(
        std::fs::read_to_string(files.join("host.sock"))?,
        "not a socket"
    );
    assert!(!files.join("host.json").exists());
    Ok(())
}

/// More of the request checks (M2T12R-04, -05): the loopback Origin passes; /healthz checks Host; a second Host or
/// Origin, and a request target naming another authority, are refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_request_checks_hold_at_their_edges() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let loopback = format!("http://127.0.0.1:{}", host.port);
    let ok = send(
        &to,
        "GET",
        "/v1/runs/r1",
        Some(&host.token),
        &[("Origin", &loopback)],
        b"",
    )
    .await?
    .ok_or("no response")?;
    assert_eq!(ok.status, 200, "{ok:?}");
    let raw = |text: String| {
        let port = host.port;
        async move {
            let bytes = exchange(
                tokio::net::TcpStream::connect(("127.0.0.1", port)).await?,
                text.into_bytes(),
            )
            .await?;
            parse(&bytes)?
                .ok_or_else(|| Box::<dyn std::error::Error + Send + Sync>::from("no response"))
        }
    };
    let bound = format!("127.0.0.1:{}", host.port);
    for request in [
        "GET /healthz HTTP/1.1\r\nHost: evil.example\r\nConnection: close\r\n\r\n".to_owned(),
        format!(
            "GET /v1/runs/r1 HTTP/1.1\r\nHost: {bound}\r\nHost: evil.example\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",
            host.token
        ),
        format!(
            "GET /v1/runs/r1 HTTP/1.1\r\nHost: {bound}\r\nOrigin: {loopback}\r\nOrigin: http://evil.example\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",
            host.token
        ),
        format!(
            "GET http://evil.example/v1/runs/r1 HTTP/1.1\r\nHost: {bound}\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",
            host.token
        ),
    ] {
        let reply = raw(request.clone()).await?;
        assert_eq!(reply.status, 403, "{request:?}: {reply:?}");
        assert_eq!(reply.json()?["code"], "FORBIDDEN");
    }
    host.shutdown().await;
    Ok(())
}

/// Malformed requests get Error bodies with the contract's codes, and no database text reaches a client
/// (M2T12R-02, -03): a bad cursor is 409, a run id that is not text 404, an oversized body 400, an unknown route 404,
/// a command_id another command used 409 CONFLICT with a fixed message; and a resume by Last-Event-ID alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn malformed_requests_get_error_bodies() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned(), "r2".to_owned()])?;
    let to = To::Tcp(host.port);
    let error = openapi_schema("/components/schemas/Error")?;
    let big = vec![b' '; 3 << 20];
    for (method, path, body, status, code) in [
        (
            "GET",
            "/v1/events?after=abc",
            b"".as_slice(),
            409,
            "CONFLICT",
        ),
        (
            "GET",
            "/v1/events?after=99999999999999999999",
            b"".as_slice(),
            409,
            "CONFLICT",
        ),
        (
            "GET",
            "/v1/events?after=-1",
            b"".as_slice(),
            409,
            "CONFLICT",
        ),
        ("GET", "/v1/runs/%FF", b"".as_slice(), 404, "RUN_NOT_FOUND"),
        ("POST", "/v1/commands", big.as_slice(), 400, "BAD_REQUEST"),
        ("GET", "/v1/nothing", b"".as_slice(), 404, "NOT_FOUND"),
        ("DELETE", "/v1/runs/r1", b"".as_slice(), 404, "NOT_FOUND"),
    ] {
        let reply = call(&to, method, path, &host.token, body).await?;
        assert_eq!(reply.status, status, "{method} {path}: {reply:?}");
        let body = reply.json()?;
        conforms(&error, &body)?;
        assert_eq!(body["code"], code, "{method} {path}: {body}");
    }
    let bad_id = send(
        &to,
        "GET",
        "/v1/events",
        Some(&host.token),
        &[("Last-Event-ID", "x")],
        b"",
    )
    .await?
    .ok_or("no response")?;
    assert_eq!(bad_id.status, 409, "{bad_id:?}");
    // The same command_id under another key: refused without the database's text.
    let first = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("dup", "ka", "r1", 0, "a"),
    )
    .await?;
    assert_eq!(first.status, 202);
    let clash = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("dup", "kb", "r2", 0, "b"),
    )
    .await?;
    assert_eq!(clash.status, 409, "{clash:?}");
    let text = String::from_utf8_lossy(&clash.body).into_owned();
    conforms(&error, &clash.json()?)?;
    assert_eq!(clash.json()?["code"], "CONFLICT");
    for database_text in ["UNIQUE", "constraint", "commands.", "SQLITE", "sqlite"] {
        assert!(!text.contains(database_text), "{text}");
    }
    assert_eq!(
        scalar(dir.path(), "SELECT state FROM runs WHERE run_id = 'r2'")?,
        "CREATED"
    );
    // Resuming by Last-Event-ID alone: only what came after it.
    let first_seq = scalar(dir.path(), "SELECT max(seq) FROM events")?;
    let resumed = Subscriber::connect_with(
        to.clone(),
        &host.token,
        "/v1/events",
        &[("Last-Event-ID", &first_seq)],
    )
    .await?;
    let next = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c2", "k2", "r2", 0, "next"),
    )
    .await?;
    assert_eq!(next.status, 202);
    assert!(
        until(Duration::from_secs(5), || resumed.events().len() == 1).await,
        "{:?}",
        resumed.events()
    );
    assert_eq!(resumed.events()[0].1["run_id"], "r2");
    drop(resumed);
    host.shutdown().await;
    Ok(())
}
