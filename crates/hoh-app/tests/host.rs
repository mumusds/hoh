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
/// The child's open-file limit, to run it out of descriptors.
const CHILD_NOFILE: &str = "HOH_TEST_HOST_NOFILE";
/// Set for a child whose writer panics at its first commit.
const CHILD_PANIC: &str = "HOH_TEST_HOST_PANIC";
const CHILD_PANIC_WHEN: &str = "HOH_TEST_HOST_PANIC_WHEN";
/// The child sends itself SIGTERM the moment its host has announced itself (host.json written).
const CHILD_TERM_AT_ANNOUNCE: &str = "HOH_TEST_HOST_TERM_AT_ANNOUNCE";

/// A commit hook that panics: the writer thread unwinds (CR-006: a fault made by test code).
#[derive(Debug)]
struct Panicking;

/// A commit hook that makes `<path>.entered`, waits until `path` exists, then panics: the commit is held while the
/// test stops the host, and the writer panics during the stop (CR-006: a fault made by test code).
#[derive(Debug)]
struct PanickingWhen(PathBuf);

impl FaultHooks for PanickingWhen {
    fn before_commit(&self) -> Result<(), PersistError> {
        let mut entered = self.0.clone().into_os_string();
        entered.push(".entered");
        let _ = std::fs::write(entered, "");
        let end = Instant::now() + Duration::from_secs(60);
        while !self.0.exists() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::panic::resume_unwind(Box::new("writer panic during the stop"));
    }
}

impl FaultHooks for Panicking {
    fn before_commit(&self) -> Result<(), PersistError> {
        std::panic::resume_unwind(Box::new("writer panic"));
    }
}

/// A child process that is always ended: on any way out of a test, a failed assertion included, it gets SIGTERM, a
/// moment, then SIGKILL, and is reaped (M2T12R-154).
struct ChildGuard(std::process::Child);

impl ChildGuard {
    fn spawn(command: &mut Command) -> R<ChildGuard> {
        Ok(ChildGuard(command.spawn()?))
    }
}

impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            if let Some(pid) = i32::try_from(self.0.id())
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
            }
            let end = Instant::now() + Duration::from_secs(5);
            while matches!(self.0.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

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

/// Start a host in a project another host of this process just let go. A process that spawns children (the WAL
/// self-test's probe of a parallel test) shares every open file with each child until it execs, so a lock just
/// released can still be held for that instant: a refusal (69) is tried again for a moment. Tests that expect the
/// refusal call [`Host::start`] themselves.
async fn start_when_free(config: HostConfig) -> Result<Host, hoh_app::host::StartError> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        match Host::start(config.clone()).await {
            Err(e) if e.exit_code == 69 && Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            other => return other,
        }
    }
}

/// What this process still has open of the project lock: the `.hoh` directory itself (the flock) or its pid file.
/// A child forked by a parallel test may hold copies for an instant, but this process's own table must be clear
/// once a start failed or a stop returned (ADR-016 decision 12, M2T12R-180).
fn lock_files_held(project: &Path) -> R<Vec<String>> {
    let files = project.join(".hoh").canonicalize()?;
    let directory = files.to_string_lossy().into_owned();
    let pid_file = files.join("host.lock").to_string_lossy().into_owned();
    let mut held = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")? {
        let Ok(target) = std::fs::read_link(entry?.path()) else {
            continue;
        };
        let target = target.to_string_lossy().into_owned();
        if target == directory || target.starts_with(&pid_file) {
            held.push(target);
        }
    }
    Ok(held)
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
    seed_runs_in(project, &host.project_id, ids)
}

/// CREATED runs `ids` in project `project_id`.
fn seed_runs_in(project: &Path, project_id: &str, ids: &[String]) -> R {
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=sys.stdin.readline().strip(); \
                  c.executemany(\"INSERT INTO runs(run_id, project_id, state, mode, created_at) VALUES (?, ?, 'CREATED', 'AUTO', '2026-11-06T00:00:00Z')\", \
                  [(l.strip(), p) for l in sys.stdin if l.strip()]); c.commit(); c.close()";
    let mut input = format!("{project_id}\n");
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
    // A server may answer and close before the whole request is written (an oversized body): what it sent is still
    // read (M2T12R-99).
    let _ = stream.write_all(&request).await;
    let mut raw = Vec::new();
    // A reset after the request counts as an empty response; a stream that does not end fails the test, not hangs it
    // (M2T12R-96).
    match tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut raw)).await {
        Ok(_) => Ok(raw),
        Err(_) => Err(format!(
            "no end of response after 30 s: {}",
            String::from_utf8_lossy(&raw)
        )
        .into()),
    }
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

/// A stop signal at the instant the host announced itself (CR-006 F2: a signal the process sends itself, test code).
#[derive(Debug)]
struct TermAtAnnounce;

impl HostHooks for TermAtAnnounce {
    fn announced(&self) {
        let _ =
            rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::TERM);
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
    if std::env::var_os(CHILD_PANIC).is_some() {
        config.store.faults = Arc::new(Panicking);
    }
    if std::env::var_os(CHILD_TERM_AT_ANNOUNCE).is_some() {
        config.hooks = Arc::new(TermAtAnnounce);
    }
    if let Some(go) = std::env::var_os(CHILD_PANIC_WHEN) {
        config.store.faults = Arc::new(PanickingWhen(PathBuf::from(go)));
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
    if let Some(limit) =
        std::env::var_os(CHILD_NOFILE).and_then(|l| l.to_str()?.parse::<u64>().ok())
    {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        let old = getrlimit(Resource::Nofile);
        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(limit),
                maximum: old.maximum,
            },
        )
        .expect("the limit");
    }
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
    // The host's own reconcile events at its start aside (ADR-019 decision 5).
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT count(*) FROM events WHERE kind NOT LIKE 'reconcile.%'"
        )?,
        "1"
    );
    // PauseRun's notice to the commander is a runtime notice (ADR-016 decision 5, ADR-018 decision 4, M2T04R-251).
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT json_extract(payload_json, '$.notice') FROM outbox"
        )?,
        "runtime"
    );
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
    // The host's own reconcile events at its start aside (ADR-019 decision 5).
    assert_eq!(
        scalar(
            dir.path(),
            "SELECT count(*) FROM events WHERE kind NOT LIKE 'reconcile.%'"
        )?,
        "1"
    );
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
                 seqs=[r[0] for r in c.execute(\"SELECT seq FROM events WHERE kind NOT LIKE 'reconcile.%' ORDER BY rowid\")]; \
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
    // The points of M2-T05 (ADR-019 decisions 1, 8, 10): every one returns and does nothing (M2T05R-58).
    use hoh_app::host::RunnerPoint;
    use hoh_core::persistence::WritePoint;
    let points = std::panic::catch_unwind(|| {
        for point in [
            WritePoint::Begun,
            WritePoint::StateWritten,
            WritePoint::EventsWritten,
            WritePoint::OutboxWritten,
            WritePoint::Committed,
        ] {
            NoFaults.at("Commit", point);
        }
        for point in [
            RunnerPoint::BeforeSend,
            RunnerPoint::AfterSend,
            RunnerPoint::AfterAck,
            RunnerPoint::AfterSpawn,
            RunnerPoint::AfterEffectStarted,
            RunnerPoint::AfterEffectCommitted,
        ] {
            NoHostFaults.runner(point);
        }
        NoHostFaults.announced();
    });
    assert!(points.is_ok());
    // What a product configuration carries: no runner (no executor before M2-T07), the observer that confirms
    // nothing, the system clock.
    let config = HostConfig::new(Path::new("/project"));
    assert_eq!(format!("{:?}", config.hooks), "NoHostFaults");
    assert_eq!(format!("{:?}", config.store.faults), "NoFaults");
    assert!(config.runner.is_none());
    assert_eq!(format!("{:?}", config.reconcile.observer), "NoObserver");
    let now = std::time::SystemTime::now();
    let clock = config.clock.0.now();
    assert!(clock.duration_since(now).unwrap_or_default() < Duration::from_secs(5));
    assert!(now.duration_since(clock).unwrap_or_default() < Duration::from_secs(5));
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
    let first_seq: i64 = scalar(
        dir.path(),
        "SELECT min(seq) FROM events WHERE kind NOT LIKE 'reconcile.%'",
    )?
    .parse()?;
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

/// A host stops with event streams open: they end with the stop itself, well before the grace; host.json and the
/// socket go, and the lock is free again (M2T12R-01).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_host_stops_with_subscribers() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let tcp = Subscriber::connect(To::Tcp(host.port), &host.token, "/v1/events").await?;
    let unix =
        Subscriber::connect(To::Unix(host.socket.clone()), &host.token, "/v1/events").await?;
    let socket = host.socket.clone();
    let started = Instant::now();
    assert!(tokio::time::timeout(Duration::from_secs(15), host.shutdown()).await?);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "stopped after {:?}",
        started.elapsed()
    );
    assert!(!socket.exists());
    assert!(!dir.path().join(".hoh/host.json").exists());
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    drop((tcp, unix));
    let next = start_when_free(config(dir.path())).await?;
    assert_eq!(next.host_epoch, 2);
    next.shutdown().await;
    Ok(())
}

/// One response read from a kept-alive connection (by its Content-Length).
async fn read_one(stream: &mut tokio::net::TcpStream) -> R<Option<Reply>> {
    let mut raw = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        if let Some(end) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&raw[..end]).to_ascii_lowercase();
            let length: usize = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or(0);
            if raw.len() >= end + 4 + length {
                return parse(&raw[..end + 4 + length]);
            }
        }
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await??;
        if n == 0 {
            return parse(&raw);
        }
        raw.extend_from_slice(&buf[..n]);
    }
}

/// After the stop nothing more is taken: a new connection is refused, and a kept-alive one is closed or answered
/// 503 GONE (retryable); either way nothing is written (ADR-016 decision 12, M2T12R-90, -94).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_after_the_stop_get_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let (port, token) = (host.port, host.token.clone());
    let mut kept = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    kept.write_all(format!("GET /healthz HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .await?;
    assert_eq!(
        read_one(&mut kept).await?.ok_or("no health answer")?.status,
        200
    );
    let stopping = tokio::spawn(host.shutdown());
    // The stop has happened once the port takes no new connection.
    let end = Instant::now() + Duration::from_secs(10);
    while tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .is_ok()
    {
        assert!(Instant::now() < end, "the port still takes connections");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let body = pause("c-late", "k-late", "r1", 0, "late");
    let request = request_bytes(
        &To::Tcp(port),
        "POST",
        "/v1/commands",
        Some(&token),
        &[],
        &body,
    );
    let _ = kept.write_all(&request).await;
    if let Some(reply) = read_one(&mut kept).await.ok().flatten() {
        assert_eq!(reply.status, 503, "{reply:?}");
        let error = reply.json()?;
        assert_eq!(
            (&error["code"], &error["retryable"]),
            (&json!("GONE"), &json!(true)),
            "{error}"
        );
    }
    tokio::time::timeout(Duration::from_secs(15), stopping).await??;
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "0");
    Ok(())
}

/// A host process ends on SIGTERM though a client is subscribed, and removes host.json and the socket.
#[test]
fn a_host_process_ends_on_sigterm_with_a_subscriber() -> R {
    let dir = tempfile::tempdir()?;
    let mut child = ChildGuard::spawn(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "host_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_PROJECT, dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()),
    )?;
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
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    std::fs::write(files.join("host.sock"), "not a socket")?;
    let refused = start_when_free(config(dir.path()))
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

/// A commit held past the grace: its connection is ended without a response, but the project stays locked until the
/// writer has finished it, so no write of a stopped host can follow the next host's start (ADR-016 decision 12,
/// M2T12R-60, -91, -97).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopping_host_finishes_its_writes_before_the_lock_goes() -> R {
    use std::sync::mpsc;
    #[derive(Debug)]
    struct Latched(Mutex<(mpsc::Sender<()>, mpsc::Receiver<()>)>);
    impl FaultHooks for Latched {
        fn before_commit(&self) -> Result<(), PersistError> {
            let latch = self
                .0
                .lock()
                .map_err(|_| PersistError::Storage("latch".to_owned()))?;
            let _ = latch.0.send(());
            let _ = latch.1.recv();
            Ok(())
        }
    }
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let dir = tempfile::tempdir()?;
    let mut held = config(dir.path());
    held.store.faults = Arc::new(Latched(Mutex::new((entered_tx, release_rx))));
    let host = Host::start(held).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let token = host.token.clone();
    let command = tokio::spawn(async move {
        send(
            &to,
            "POST",
            "/v1/commands",
            Some(&token),
            &[],
            &pause("c-slow", "k-slow", "r1", 0, "slow"),
        )
        .await
    });
    tokio::task::spawn_blocking(move || entered.recv_timeout(Duration::from_secs(10))).await??;
    let stopping = tokio::spawn(host.shutdown());
    // Past the grace: the connection is ended, the commit still waits, and the project is still locked.
    tokio::time::sleep(Duration::from_secs(6)).await;
    let answer = tokio::time::timeout(Duration::from_secs(5), command).await???;
    assert!(answer.is_none(), "{answer:?}");
    assert!(
        !stopping.is_finished(),
        "shutdown returned with a write pending"
    );
    let refused = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("a second host started while the first wrote")?;
    assert_eq!(refused.exit_code, 69, "{refused}");
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "0");
    release.send(())?;
    assert!(tokio::time::timeout(Duration::from_secs(10), stopping).await??);
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM commands")?, "1");
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    let next = start_when_free(config(dir.path())).await?;
    assert_eq!(next.host_epoch, 2);
    next.shutdown().await;
    Ok(())
}

/// A Host dropped without `shutdown` stops the same way: the project is locked until its writer has ended, and only
/// then can another host start (ADR-016 decision 12, M2T12R-93).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_host_keeps_the_project_until_its_writer_ends() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let subscriber =
        Subscriber::connect(To::Unix(host.socket.clone()), &host.token, "/v1/events").await?;
    let socket = host.socket.clone();
    drop(host);
    // Its files go with it (M2T12R-153).
    assert!(!socket.exists());
    assert!(!dir.path().join(".hoh/host.json").exists());
    let end = Instant::now() + Duration::from_secs(15);
    let next = loop {
        match Host::start(config(dir.path())).await {
            Ok(next) => break next,
            Err(e) if e.exit_code == 69 && Instant::now() < end => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };
    assert_eq!(next.host_epoch, 2);
    // The dropped host's stream ended with it.
    assert!(until(Duration::from_secs(5), || subscriber.task.is_finished()).await);
    next.shutdown().await;
    Ok(())
}

/// A start that fails after binding leaves nothing served: a port in use leaves no socket; a host.json that cannot
/// be written stops serving and removes the socket, and the lock is free afterwards (M2T12R-08, -63).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_start_leaves_nothing_served() -> R {
    let dir = tempfile::tempdir()?;
    let busy = std::net::TcpListener::bind("127.0.0.1:0")?;
    let mut taken = config(dir.path());
    taken.port = busy.local_addr()?.port();
    let refused = Host::start(taken)
        .await
        .err()
        .ok_or("started on a port in use")?;
    assert!(refused.message.contains("cannot bind"), "{refused}");
    assert!(!dir.path().join(".hoh/host.sock").exists());
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    drop(busy);
    // host.json is a directory with something in it: it cannot be replaced.
    std::fs::create_dir_all(dir.path().join(".hoh/host.json"))?;
    std::fs::write(dir.path().join(".hoh/host.json/keep"), "")?;
    let refused = start_when_free(config(dir.path()))
        .await
        .err()
        .ok_or("started without host.json")?;
    assert!(refused.message.contains("host.json"), "{refused}");
    assert!(!dir.path().join(".hoh/host.sock").exists());
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    std::fs::remove_dir_all(dir.path().join(".hoh/host.json"))?;
    let host = start_when_free(config(dir.path())).await?;
    assert!(host.shutdown().await);
    assert_eq!(lock_files_held(dir.path())?, Vec::<String>::new());
    Ok(())
}

/// On the socket a request target naming an authority is refused; a Last-Event-ID that is not ASCII is unreadable,
/// 409 (M2T12R-62, -63).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_socket_and_cursor_edges_are_refused() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let request = format!(
        "GET http://localhost/healthz HTTP/1.1\r\nHost: localhost\r\nX-HoH-Token: {}\r\nConnection: close\r\n\r\n",
        host.token
    );
    let raw = exchange(
        tokio::net::UnixStream::connect(&host.socket).await?,
        request.into_bytes(),
    )
    .await?;
    let reply = parse(&raw)?.ok_or("no response")?;
    assert_eq!(reply.status, 403, "{reply:?}");
    let mut request = format!(
        "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nConnection: close\r\nLast-Event-ID: ",
        host.port, host.token
    )
    .into_bytes();
    request.extend_from_slice(&[0xff, 0xfe]);
    request.extend_from_slice(b"\r\n\r\n");
    let raw = exchange(
        tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?,
        request,
    )
    .await?;
    let reply = parse(&raw)?.ok_or("no response")?;
    assert_eq!(reply.status, 409, "{reply:?}");
    assert_eq!(reply.json()?["code"], "CONFLICT");
    host.shutdown().await;
    Ok(())
}

/// CPU time (user + system, clock ticks) of process `pid`.
fn cpu_ticks(pid: u32) -> R<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let after = &stat[stat.rfind(')').ok_or("no comm")? + 2..];
    let fields: Vec<&str> = after.split(' ').collect();
    Ok(fields[11].parse::<u64>()? + fields[12].parse::<u64>()?)
}

/// A host out of file descriptors does not spin on its listener: an accept error other than a per-connection one
/// waits 1 s (M2T12R-92).
#[test]
fn a_host_out_of_descriptors_does_not_spin() -> R {
    let dir = tempfile::tempdir()?;
    let mut child = ChildGuard::spawn(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "host_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_PROJECT, dir.path())
            .env(CHILD_NOFILE, "80")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )?;
    let host_json = dir.path().join(".hoh/host.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !host_json.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let outcome = (|| -> R {
        let info: Value = serde_json::from_str(&std::fs::read_to_string(&host_json)?)?;
        let port = u16::try_from(info["port"].as_u64().ok_or("no port")?)?;
        // More connections than the child has descriptors for.
        let mut held = Vec::new();
        for _ in 0..150 {
            if let Ok(stream) = std::net::TcpStream::connect(("127.0.0.1", port)) {
                held.push(stream);
            }
        }
        std::thread::sleep(Duration::from_millis(500));
        let before = cpu_ticks(child.id())?;
        std::thread::sleep(Duration::from_secs(2));
        let used = cpu_ticks(child.id())? - before;
        // Spinning burns about 200 ticks (a whole core) in 2 s; waiting a few.
        assert!(used < 40, "the host used {used} ticks in 2 s");
        drop(held);
        Ok(())
    })();
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id())?).ok_or("no pid")?;
    let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    let end = Instant::now() + Duration::from_secs(15);
    while child.try_wait()?.is_none() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    if child.try_wait()?.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    outcome
}

/// A host whose writer committed after 200,000 events written outside it: the replay of all of them is due.
async fn host_with_a_long_replay(dir: &Path) -> R<Host> {
    let host = Host::start(config(dir)).await?;
    let script = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); p=sys.stdin.read().strip(); \
                  c.executemany(\"INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES (?, ?, NULL, 'test.fill', '{}', '2026-11-06T00:00:00Z')\", \
                  [(p, 'fill-%d' % i) for i in range(200000)]); c.commit(); c.close()";
    python(&db(dir), script, &host.project_id)?;
    // One command through the writer moves the watermark past the filled events.
    seed_runs(&host, dir, &["r1".to_owned()])?;
    let reply = call(
        &To::Tcp(host.port),
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "k1", "r1", 0, "fill"),
    )
    .await?;
    assert_eq!(reply.status, 202);
    Ok(host)
}

/// An SSE request on a new connection, read until the replay has begun.
async fn replay_begun(host: &Host) -> R<tokio::net::TcpStream> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream
        .write_all(
            format!(
                "GET /v1/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\n\r\n",
                host.port, host.token
            )
            .as_bytes(),
        )
        .await?;
    let mut buf = vec![0u8; 65536];
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("\"test.fill\"") {
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf)).await??;
        assert!(n > 0, "the stream closed before replaying");
        seen.extend_from_slice(&buf[..n]);
    }
    Ok(stream)
}

/// A replaying stream that its client keeps reading ends with the stop at its next step, long before the replay is
/// done (M2T12R-98, -121).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_replaying_stream_ends_with_the_stop() -> R {
    let dir = tempfile::tempdir()?;
    let host = host_with_a_long_replay(dir.path()).await?;
    let mut stream = replay_begun(&host).await?;
    // A slow reader: the stream keeps being polled, and the replay would take far longer than the grace.
    let reader = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => tokio::time::sleep(Duration::from_millis(2)).await,
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), host.shutdown()).await?;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "stopped after {:?}",
        started.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(5), reader).await??;
    Ok(())
}

/// A replaying stream whose client reads nothing is not polled, so it cannot see the stop: its connection is ended
/// after the grace (ADR-016 decision 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_blocked_stream_is_ended_after_the_grace() -> R {
    let dir = tempfile::tempdir()?;
    let host = host_with_a_long_replay(dir.path()).await?;
    let idle = replay_begun(&host).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), host.shutdown()).await?;
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "stopped after {:?}",
        started.elapsed()
    );
    drop(idle);
    Ok(())
}

/// A writer that panics does not let the project go: the lock stays held (until the process ends) while the rest of
/// its host still runs, so no other host can take over (ADR-016 decision 12, M2T12R-120).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_panic_keeps_the_project_locked() -> R {
    let dir = tempfile::tempdir()?;
    let mut panicking = config(dir.path());
    panicking.store.faults = Arc::new(Panicking);
    let host = Host::start(panicking).await?;
    seed_runs(&host, dir.path(), &["r1".to_owned()])?;
    let to = To::Tcp(host.port);
    let reply = call(
        &to,
        "POST",
        "/v1/commands",
        &host.token,
        &pause("c1", "k1", "r1", 0, "boom"),
    )
    .await?;
    assert_eq!(reply.status, 503, "{reply:?}");
    let error = reply.json()?;
    assert_eq!(
        (&error["code"], &error["retryable"]),
        (&json!("GONE"), &json!(true)),
        "{error}"
    );
    // The writer's end, not a poll: GONE can reach the client while the panic still unwinds (M2T12R-181).
    let finished = tokio::time::timeout(Duration::from_secs(5), host.writer_ended()).await?;
    assert!(!finished, "the writer's panic read as a normal end");
    assert!(!host.writer_alive());
    let health = send(&to, "GET", "/healthz", None, &[], b"")
        .await?
        .ok_or("no health answer")?;
    assert_eq!(health.status, 503, "{health:?}");
    let refused = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("another host took over")?;
    assert_eq!(refused.exit_code, 69, "{refused}");
    assert!(
        !host.shutdown().await,
        "the stop reported a writer that panicked as finished"
    );
    let still = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("the lock went with the panic")?;
    assert_eq!(still.exit_code, 69, "{still}");
    Ok(())
}

/// A stopping host removes only its own host.json (by its token) and the socket it bound (by inode): files another
/// host put there are left (M2T12R-120).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stopping_host_removes_only_its_own_files() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let files = dir.path().join(".hoh");
    std::fs::write(files.join("host.json"), r#"{"token": "another host's"}"#)?;
    std::fs::remove_file(&host.socket)?;
    let other = std::os::unix::net::UnixListener::bind(&host.socket)?;
    host.shutdown().await;
    assert_eq!(
        std::fs::read_to_string(files.join("host.json"))?,
        r#"{"token": "another host's"}"#
    );
    assert!(files.join("host.sock").exists());
    drop(other);
    Ok(())
}

/// A host whose writer dies stops and its process exits 1: commands are 503 GONE (retry with the next host) and
/// `/healthz` 503 meanwhile; once the process is gone the next host starts (ADR-016 decision 12, M2T12R-150).
#[test]
fn a_host_whose_writer_dies_exits_non_zero() -> R {
    let dir = tempfile::tempdir()?;
    let mut child = ChildGuard::spawn(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "host_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_PROJECT, dir.path())
            .env(CHILD_PANIC, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )?;
    let host_json = dir.path().join(".hoh/host.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !host_json.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let info: Value = serde_json::from_str(&std::fs::read_to_string(&host_json)?)?;
    seed_runs_in(
        dir.path(),
        info["project_id"].as_str().ok_or("no project")?,
        &["r1".to_owned()],
    )?;
    let port = u16::try_from(info["port"].as_u64().ok_or("no port")?)?;
    let token = info["token"].as_str().ok_or("no token")?.to_owned();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let to = To::Tcp(port);
        let reply = call(
            &to,
            "POST",
            "/v1/commands",
            &token,
            &pause("c1", "k1", "r1", 0, "boom"),
        )
        .await?;
        assert_eq!(reply.status, 503, "{reply:?}");
        assert_eq!(reply.json()?["code"], "GONE");
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })?;
    let end = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        assert!(
            Instant::now() < end,
            "the host did not stop after its writer died"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1));
    assert!(!host_json.exists());
    // The child has exited: nothing of this process held the project, so the start must not need a retry.
    runtime.block_on(async {
        let next = Host::start(config(dir.path())).await?;
        assert_eq!(next.host_epoch, 2);
        next.shutdown().await;
        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(())
    })
}

/// A writer that panics during the stop still makes the process exit 1: the signal came first, but the last write
/// failed (M2T12R-185).
#[test]
fn a_writer_panic_during_the_stop_exits_non_zero() -> R {
    let dir = tempfile::tempdir()?;
    let go = dir.path().join("go");
    let mut child = ChildGuard::spawn(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "host_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_PROJECT, dir.path())
            .env(CHILD_PANIC_WHEN, &go)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )?;
    let host_json = dir.path().join(".hoh/host.json");
    let end = Instant::now() + Duration::from_secs(30);
    while !host_json.exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(20));
    }
    let info: Value = serde_json::from_str(&std::fs::read_to_string(&host_json)?)?;
    seed_runs_in(
        dir.path(),
        info["project_id"].as_str().ok_or("no project")?,
        &["r1".to_owned()],
    )?;
    let port = u16::try_from(info["port"].as_u64().ok_or("no port")?)?;
    let token = info["token"].as_str().ok_or("no token")?.to_owned();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    // The commit waits in its hook; its connection is ended at the stop's grace without an answer.
    let command = runtime.spawn(async move {
        let _ = call(
            &To::Tcp(port),
            "POST",
            "/v1/commands",
            &token,
            &pause("c1", "k1", "r1", 0, "held"),
        )
        .await;
    });
    let entered = dir.path().join("go.entered");
    let end = Instant::now() + Duration::from_secs(20);
    while !entered.exists() {
        assert!(Instant::now() < end, "the commit never reached its hook");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.id())?).ok_or("no pid")?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM)?;
    // host.json goes only in the stop: the signal has been taken before the writer panics.
    let end = Instant::now() + Duration::from_secs(20);
    while host_json.exists() {
        assert!(Instant::now() < end, "the host did not begin its stop");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::fs::write(&go, "")?;
    let end = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        assert!(Instant::now() < end, "the host did not exit");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(1));
    runtime.block_on(async { tokio::time::timeout(Duration::from_secs(10), command).await })??;
    Ok(())
}

/// The lock is the `.hoh` directory itself: removing host.lock while a host runs does not let a second host start
/// (M2T12R-152).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removing_the_pid_file_does_not_free_the_project() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    std::fs::remove_file(dir.path().join(".hoh/host.lock"))?;
    let refused = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("a second host started")?;
    assert_eq!(refused.exit_code, 69, "{refused}");
    // The refused start does not make a pid file of its own, and says it cannot name the holder (M2T12R-182).
    assert!(refused.message.contains("pid unknown"), "{refused}");
    assert!(!dir.path().join(".hoh/host.lock").exists());
    host.shutdown().await;
    Ok(())
}

/// A pid file that cannot be opened (here a directory) does not hide a held project: the start is refused, 69
/// (M2T12R-182).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pid_file_that_cannot_be_opened_still_refuses_a_held_project() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    std::fs::remove_file(dir.path().join(".hoh/host.lock"))?;
    std::fs::create_dir(dir.path().join(".hoh/host.lock"))?;
    let refused = Host::start(config(dir.path()))
        .await
        .err()
        .ok_or("a second host started")?;
    assert_eq!(refused.exit_code, 69, "{refused}");
    host.shutdown().await;
    Ok(())
}

/// TEST-038-05: a running run's scheduler weights cannot change (CR-007: 409 RUN_CONFIG_FROZEN, nothing written);
/// its snapshot stays (4,3,3,2) and the next round scores with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_038_05_a_running_runs_weights_change_is_409_and_its_snapshot_stays() -> R {
    let dir = tempfile::tempdir()?;
    let host = Host::start(config(dir.path())).await?;
    let world = "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.stdin.read()); c.commit()";
    let snapshot = r#"{"scheduler": {"weights": {"user_value": 4, "unblock_value": 3, "risk_reduction": 3, "information_gain": 2}, "max_wait_rounds": 20}}"#;
    let sql = format!(
        "INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', '{p}', printf('%064d', 1), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, config_snapshot_json, created_at) VALUES ('R', '{p}', 's1', 'RUNNING', 'AUTO', 4, '{snapshot}', '2026-11-06T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES ('sd', 'claude', 'developer', '1', '{{\"structured_events\": true}}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-06T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, role, writer, created_seq, user_value, unblock_value, risk_reduction, information_gain, cost_estimate) VALUES ('T1', 'R', 's1', 'READY', 'developer', 1, 1, 2, 1, 1, 1, 2);",
        p = host.project_id
    );
    python(&db(dir.path()), world, &sql)?;
    let read_snapshot = || {
        scalar(
            dir.path(),
            "SELECT config_snapshot_json FROM runs WHERE run_id = 'R'",
        )
    };
    let before = read_snapshot()?;
    let (commands, events) = (
        scalar(dir.path(), "SELECT count(*) FROM commands")?,
        scalar(dir.path(), "SELECT count(*) FROM events")?,
    );
    let request = serde_json::to_vec(&json!({
        "schema_version": "1.0", "command_id": "c1", "idempotency_key": "k1", "expected_revision": 4,
        "kind": "ConfigureTeam", "run_id": "R",
        "payload": {"scheduler": {"weights": {"user_value": 1, "unblock_value": 1, "risk_reduction": 1,
                                              "information_gain": 1}}, "apply_at": "checkpoint"}
    }))?;
    let reply = call(
        &To::Tcp(host.port),
        "POST",
        "/v1/commands",
        &host.token,
        &request,
    )
    .await?;
    assert_eq!(reply.status, 409, "{reply:?}");
    let body = reply.json()?;
    assert_eq!(
        (
            &body["code"],
            &body["retryable"],
            &body["current_revision"],
            &body["details"]["key_path"]
        ),
        (
            &json!("RUN_CONFIG_FROZEN"),
            &json!(false),
            &json!(null),
            &json!("scheduler")
        ),
        "{body}"
    );
    let after = read_snapshot()?;
    assert_eq!(after, before);
    let weights: Value = serde_json::from_str(&after)?;
    assert_eq!(
        weights["scheduler"]["weights"],
        json!({"user_value": 4, "unblock_value": 3, "risk_reduction": 3, "information_gain": 2})
    );
    assert_eq!(
        scalar(dir.path(), "SELECT count(*) FROM commands")?,
        commands
    );
    assert_eq!(scalar(dir.path(), "SELECT count(*) FROM events")?, events);
    // A run that does not exist is 409 RUN_NOT_FOUND; bindings alone are not served yet (M2-T07): 422.
    let mut missing: Value = serde_json::from_slice(&request)?;
    missing["run_id"] = json!("nope");
    let reply = call(
        &To::Tcp(host.port),
        "POST",
        "/v1/commands",
        &host.token,
        &serde_json::to_vec(&missing)?,
    )
    .await?;
    assert_eq!(
        (reply.status, reply.json()?["code"].clone()),
        (409, json!("RUN_NOT_FOUND"))
    );
    let mut bindings: Value = serde_json::from_slice(&request)?;
    bindings["payload"] = json!({"bindings": {"qa": "pi"}, "apply_at": "checkpoint"});
    let reply = call(
        &To::Tcp(host.port),
        "POST",
        "/v1/commands",
        &host.token,
        &serde_json::to_vec(&bindings)?,
    )
    .await?;
    assert_eq!(reply.status, 422, "{reply:?}");
    host.shutdown().await;
    // One round on the same database scores T1 with the snapshot's weights: (4*2 + 3*1 + 3*1 + 2*1) / 2 = 8.
    let store = hoh_core::persistence::Store::open(&db(dir.path()), config(dir.path()).store)?;
    let context = hoh_core::decision::Context {
        required_capabilities: [(
            "developer".to_owned(),
            ["structured_events".to_owned()].into(),
        )]
        .into(),
        ..Default::default()
    };
    let round = store.schedule_round("R", context).await?;
    let dispatched = round.dispatched.ok_or("nothing dispatched")?;
    assert_eq!(dispatched.task_id, "T1");
    assert!(
        (dispatched.score.value() - 8.0).abs() <= 1e-9,
        "{:?}",
        dispatched.score
    );
    let event: Value = serde_json::from_str(&scalar(
        dir.path(),
        "SELECT payload_json FROM events WHERE kind = 'schedule.round'",
    )?)?;
    assert_eq!(
        event["weights"],
        json!({"user_value": 4, "unblock_value": 3, "risk_reduction": 3, "information_gain": 2})
    );
    if let Some(evidence) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&evidence).join("test-038-05.json"),
            serde_json::to_vec_pretty(&json!({
                "request": serde_json::from_slice::<Value>(&request)?,
                "response": {"status": 409, "body": body},
                "snapshot_before": serde_json::from_str::<Value>(&before)?,
                "snapshot_after": weights,
                "round_event": event,
            }))?,
        )?;
    }
    Ok(())
}

/// A SIGTERM that arrives as soon as the host has written host.json (a supervisor stopping it right after its start)
/// stops it as any other: exit 0, host.json and the socket removed (ADR-019 decision 10; the CI run of the recovery
/// check met this window).
#[test]
fn a_stop_signal_as_the_host_announces_itself_stops_it_cleanly() -> R {
    let dir = tempfile::tempdir()?;
    let mut child = ChildGuard::spawn(
        Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "host_child",
                "--ignored",
                "--test-threads=1",
                "--nocapture",
            ])
            .env(CHILD_PROJECT, dir.path())
            .env(CHILD_TERM_AT_ANNOUNCE, "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped()),
    )?;
    let end = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > end {
            return Err("the host did not end".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(0), "{status:?}");
    assert!(!dir.path().join(".hoh/host.json").exists());
    assert!(!dir.path().join(".hoh/host.sock").exists());
    Ok(())
}
