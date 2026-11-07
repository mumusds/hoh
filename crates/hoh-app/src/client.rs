//! The client side of `hoh -p --detach` and `hoh attach` (REQ-005, docs/adr/ADR-021.md decisions 1-6): find the
//! project's host or start one in its own session and wait for it, then talk HTTP/1.1 to it over its Unix socket.

use serde_json::{Value, json};
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a client waits for a host to answer, or for one it starts to be ready (REQ-005 decision 1: 10 s).
pub const WAIT: Duration = Duration::from_secs(10);

/// How often it looks again meanwhile.
const POLL: Duration = Duration::from_millis(20);

/// What a client needs of a running host (its `host.json`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostInfo {
    pub pid: u32,
    pub socket: PathBuf,
    pub token: String,
}

/// Why a client could not do what it was asked, with the exit code it ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub code: String,
    pub message: String,
    pub exit: u8,
}

impl Failure {
    fn new(code: &str, message: impl Into<String>, exit: u8) -> Failure {
        Failure {
            code: code.to_owned(),
            message: message.into(),
            exit,
        }
    }

    /// No host can be reached or started (69, `GONE`).
    pub fn unreachable(message: impl Into<String>) -> Failure {
        Failure::new("GONE", message, 69)
    }
}

/// A response: its status and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub status: u16,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// The project's `host.json`, if it names a host.
fn host_json(project: &Path) -> Option<HostInfo> {
    let text = std::fs::read_to_string(project.join(".hoh/host.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    Some(HostInfo {
        pid: u32::try_from(value["pid"].as_u64()?).ok()?,
        socket: PathBuf::from(value["socket"].as_str()?),
        token: value["token"].as_str()?.to_owned(),
    })
}

/// Open a connection to `host` with read and write timeouts.
fn connect(host: &HostInfo, timeout: Duration) -> std::io::Result<UnixStream> {
    let stream = UnixStream::connect(&host.socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    Ok(stream)
}

/// The request's bytes: HTTP/1.1, the token, `Connection: close`.
fn request(
    host: &HostInfo,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    last_id: Option<i64>,
) -> Vec<u8> {
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nX-HoH-Token: {}\r\n",
        host.token
    );
    if let Some(id) = last_id {
        head.push_str(&format!("Last-Event-ID: {id}\r\n"));
    }
    if let Some(body) = body {
        head.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body.unwrap_or_default());
    bytes
}

/// Read a response's head from `reader`: its status and whether its body is chunked, and its length otherwise.
fn head(reader: &mut BufReader<UnixStream>) -> std::io::Result<(u16, bool, Option<usize>)> {
    let mut raw = Vec::new();
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the host closed the connection",
            ));
        }
        raw.extend_from_slice(&line);
        if line == b"\r\n" || line == b"\n" {
            break;
        }
        if raw.len() > 64 * 1024 {
            return Err(std::io::Error::other("a response head over 64 KiB"));
        }
    }
    let mut headers = [httparse::EMPTY_HEADER; 32];
    let mut response = httparse::Response::new(&mut headers);
    match response.parse(&raw) {
        Ok(httparse::Status::Complete(_)) => {}
        _ => return Err(std::io::Error::other("an unreadable response head")),
    }
    let status = response.code.unwrap_or(0);
    let mut chunked = false;
    let mut length = None;
    for header in response.headers.iter() {
        let value = String::from_utf8_lossy(header.value);
        if header.name.eq_ignore_ascii_case("transfer-encoding") && value.contains("chunked") {
            chunked = true;
        }
        if header.name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok();
        }
    }
    Ok((status, chunked, length))
}

/// The next chunk of a chunked body; empty at its end.
fn chunk(reader: &mut BufReader<UnixStream>) -> std::io::Result<Vec<u8>> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let size = usize::from_str_radix(line.trim().split(';').next().unwrap_or("0"), 16)
        .map_err(|_| std::io::Error::other("an unreadable chunk size"))?;
    let mut data = vec![0; size];
    reader.read_exact(&mut data)?;
    let mut end = [0; 2];
    reader.read_exact(&mut end)?;
    Ok(data)
}

/// One request and its whole response.
pub fn call(
    host: &HostInfo,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
) -> std::io::Result<Reply> {
    let mut stream = connect(host, WAIT)?;
    stream.write_all(&request(host, method, path, body, None))?;
    let mut reader = BufReader::new(stream);
    let (status, chunked, length) = head(&mut reader)?;
    let mut body = Vec::new();
    if chunked {
        loop {
            let data = chunk(&mut reader)?;
            if data.is_empty() {
                break;
            }
            body.extend_from_slice(&data);
        }
    } else if let Some(length) = length {
        body.resize(length, 0);
        reader.read_exact(&mut body)?;
    } else {
        reader.read_to_end(&mut body)?;
    }
    Ok(Reply { status, body })
}

/// Whether `host` answers its health check.
fn healthy(host: &HostInfo) -> bool {
    call(host, "GET", "/healthz", None).is_ok_and(|r| r.status == 200)
}

/// The host of `project` if one answers.
pub fn running(project: &Path) -> Option<HostInfo> {
    host_json(project).filter(healthy)
}

/// The project's host: the one that answers, or one started now in its own session and waited for (REQ-005
/// behaviour 4, failure mode 4; ADR-021 decision 2). Concurrent clients take turns on a lock on the project directory,
/// so after the first ready there is exactly one host. A location the host would refuse creates nothing (78).
pub fn ensure(project: &Path) -> Result<HostInfo, Failure> {
    if let Some(host) = running(project) {
        return Ok(host);
    }
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")
        .map_err(|e| Failure::unreachable(format!("cannot read /proc/self/mountinfo: {e}")))?;
    let existing = hoh_core::persistence::location::existing_ancestor(&project.join(".hoh"))
        .map_err(|e| Failure::unreachable(e.to_string()))?;
    if let Err(fstype) = hoh_core::persistence::location::check(&existing, &mountinfo) {
        return Err(Failure::new(
            "DB_ON_NETWORK_FS",
            format!(
                "{} is on {fstype}: the database needs a local file system",
                existing.display()
            ),
            78,
        ));
    }
    let directory = std::fs::File::open(project)
        .map_err(|e| Failure::unreachable(format!("cannot open {}: {e}", project.display())))?;
    let end = Instant::now() + WAIT;
    while rustix::fs::flock(
        &directory,
        rustix::fs::FlockOperation::NonBlockingLockExclusive,
    )
    .is_err()
    {
        if Instant::now() >= end {
            return Err(Failure::unreachable(
                "another client is starting the host and it did not become ready",
            ));
        }
        std::thread::sleep(POLL);
    }
    // Under the lock: a host another client started meanwhile is the one.
    if let Some(host) = running(project) {
        return Ok(host);
    }
    let started = start(project, end);
    let _ = rustix::fs::flock(&directory, rustix::fs::FlockOperation::Unlock);
    started
}

/// Start `project`'s host (`hoh __host <project>`) and wait until it is ready, until `end`.
fn start(project: &Path, end: Instant) -> Result<HostInfo, Failure> {
    let program = std::env::current_exe()
        .map_err(|e| Failure::unreachable(format!("cannot find the hoh program: {e}")))?;
    let mut child = std::process::Command::new(program)
        .arg("__host")
        .arg(project)
        .current_dir(project)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Failure::unreachable(format!("cannot start the host: {e}")))?;
    let pid = child.id();
    // Its start errors come on its stderr until it is ready (then it writes .hoh/host.log).
    let mut stderr = child.stderr.take();
    let said = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(stderr) = stderr.as_mut() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    });
    loop {
        if let Some(host) = host_json(project).filter(|h| h.pid == pid && healthy(h)) {
            // Not waited for: the host runs on in its own session; its pipe closes when it moves stderr to its log.
            drop(child);
            return Ok(host);
        }
        if let Ok(Some(status)) = child.try_wait() {
            let text = said.join().unwrap_or_default();
            let line = text.lines().last().unwrap_or("the host ended").to_owned();
            return match status.code() {
                // Another host holds the project: it is the one, if it answers.
                Some(69) => running(project).ok_or_else(|| Failure::unreachable(line)),
                // Its location or database refused (DB_ON_NETWORK_FS, DB_WAL_UNSUPPORTED): its line says which.
                Some(78) => Err(Failure::new("GONE", line, 78)),
                _ => Err(Failure::unreachable(line)),
            };
        }
        if Instant::now() >= end {
            return Err(Failure::unreachable(
                "the host did not become ready in 10 s",
            ));
        }
        std::thread::sleep(POLL);
    }
}

/// A fresh id for a command and its idempotency key.
pub fn new_id() -> String {
    let mut bytes = [0u8; 16];
    let _ = rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty());
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// Submit a command (its body with a fresh id and key) and retry the very same bytes while the host is unreachable
/// or answers a retryable 503, within 10 s (ADR-021 decision 4): the accepted body, or why not.
pub fn submit(project: &Path, host: &HostInfo, mut command: Value) -> Result<Value, Failure> {
    let id = new_id();
    command["command_id"] = json!(id);
    command["idempotency_key"] = json!(id);
    command["schema_version"] = json!("1.0");
    let body = command.to_string().into_bytes();
    let end = Instant::now() + WAIT;
    let mut host = host.clone();
    loop {
        let refused = match call(&host, "POST", "/v1/commands", Some(&body)) {
            Ok(reply) if reply.status == 202 => return Ok(reply.json()),
            Ok(reply) => {
                let error = reply.json();
                let failure = Failure::new(
                    error["code"].as_str().unwrap_or("INTERNAL"),
                    error["message"].as_str().unwrap_or("refused"),
                    if reply.status == 503 { 69 } else { 1 },
                );
                if !(reply.status == 503 && error["retryable"] == json!(true)) {
                    return Err(failure);
                }
                failure
            }
            Err(e) => Failure::unreachable(format!("the host did not answer: {e}")),
        };
        if Instant::now() >= end {
            return Err(refused);
        }
        std::thread::sleep(Duration::from_millis(200));
        if let Some(again) = running(project) {
            host = again;
        }
    }
}

/// An open event stream of the host (`/v1/events`), read event by event.
pub struct Events {
    reader: BufReader<UnixStream>,
    pending: Vec<u8>,
}

impl Events {
    /// Subscribe to `run_id`'s events after `after`.
    pub fn open(host: &HostInfo, run_id: &str, after: i64) -> std::io::Result<Events> {
        let mut stream = connect(host, Duration::from_secs(1))?;
        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let path = format!("/v1/events?run_id={run_id}&after={after}");
        stream.write_all(&request(host, "GET", &path, None, None))?;
        stream.set_read_timeout(Some(WAIT))?;
        let mut reader = BufReader::new(stream);
        let (status, _, _) = head(&mut reader)?;
        if status != 200 {
            return Err(std::io::Error::other(format!("the host answered {status}")));
        }
        reader
            .get_ref()
            .set_read_timeout(Some(Duration::from_millis(200)))?;
        Ok(Events {
            reader,
            pending: Vec::new(),
        })
    }

    /// The next event, `Ok(None)` when none came within the read timeout (the caller looks at its signals and asks
    /// again); an error when the stream ended.
    pub fn next(&mut self) -> std::io::Result<Option<Value>> {
        loop {
            if let Some(end) = self.pending.windows(2).position(|w| w == b"\n\n") {
                let block: Vec<u8> = self.pending.drain(..end + 2).collect();
                let text = String::from_utf8_lossy(&block);
                let data: String = text
                    .lines()
                    .filter_map(|l| l.strip_prefix("data:"))
                    .map(str::trim_start)
                    .collect::<Vec<_>>()
                    .join("\n");
                if data.is_empty() {
                    continue;
                }
                return Ok(serde_json::from_str(&data).ok());
            }
            match chunk(&mut self.reader) {
                Ok(data) if data.is_empty() => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the event stream ended",
                    ));
                }
                Ok(data) => self.pending.extend_from_slice(&data),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }
}
