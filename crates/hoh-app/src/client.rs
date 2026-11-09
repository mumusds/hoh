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

/// `text` as one path segment or query value: every byte but the unreserved ones percent-encoded (RFC 3986), so a
/// run id the user typed names that run and nothing else (M2T06R-54).
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
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

/// The code of a refusal whose body names none, by its status: one of openapi Error's codes (M2T06R-61).
fn status_code(status: u16) -> &'static str {
    match status {
        400 => "BAD_REQUEST",
        403 => "FORBIDDEN",
        404 => "NOT_FOUND",
        409 => "CONFLICT",
        422 => "UNPROCESSABLE",
        _ => "GONE",
    }
}

/// The code a host's refused start names in its last line (its location or its database), else GONE.
fn refused_code(line: &str) -> &'static str {
    const CODES: [&str; 4] = [
        "DB_ON_NETWORK_FS",
        "DB_WAL_UNSUPPORTED",
        "DB_FULL",
        "DB_BUSY",
    ];
    CODES
        .into_iter()
        .find(|code| {
            line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .any(|w| w == *code)
        })
        .unwrap_or("GONE")
}

/// How a host can be started in a delegated scope of the user's systemd manager.
#[derive(Debug, Clone)]
pub struct Scope {
    systemd_run: std::path::PathBuf,
    /// `--expand-environment=no` (systemd 254 and later; older ones do not expand a scope's command).
    no_expansion: bool,
}

/// Whether `name` can name a user's file: not empty, no `/`, not `.` or `..` (M2T06R-556).
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.contains('/') && name != "." && name != ".."
}

/// Whether user `name` lingers by the records in `dir` (`/var/lib/systemd/linger`): a file named after it.
fn lingers_in(dir: &Path, name: &str) -> bool {
    plain_name(name) && dir.join(name).is_file()
}

/// The user this process runs as, by name (`$USER` when it is a plain name, else `/etc/passwd` by uid).
fn user_name() -> Option<String> {
    if let Some(name) = std::env::var_os("USER")
        .and_then(|n| n.into_string().ok())
        .filter(|n| plain_name(n))
    {
        return Some(name);
    }
    let uid = rustix::process::getuid().as_raw().to_string();
    std::fs::read_to_string("/etc/passwd")
        .ok()?
        .lines()
        .find_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            (fields.get(2) == Some(&uid.as_str())).then(|| fields[0].to_owned())
        })
}

/// Whether the user's systemd manager can keep a host running after the user's last session: it answers on
/// `$XDG_RUNTIME_DIR/systemd/private` and the user lingers (else it stops soon after the last session ends, and a host
/// it runs with it, which would end accepted work at an SSH disconnect, REQ-005 behaviour 2; M2T06R-453). Why not.
pub(crate) fn manager_that_lingers() -> Result<(), String> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .ok_or_else(|| "XDG_RUNTIME_DIR is not set: no user systemd manager to ask".to_owned())?;
    if !Path::new(&runtime).join("systemd/private").exists() {
        return Err(
            "the user's systemd manager does not answer ($XDG_RUNTIME_DIR/systemd/private)"
                .to_owned(),
        );
    }
    let name = user_name().ok_or_else(|| "this user's name is not known".to_owned())?;
    if !lingers_in(Path::new("/var/lib/systemd/linger"), &name) {
        return Err(format!(
            "user {name} does not linger: a host in the user's systemd manager would stop after the last session \
             (enable it with `loginctl enable-linger {name}`)"
        ));
    }
    Ok(())
}

/// How long `systemd-run --version` may take (M2T06R-553): it runs in a process group of its own, through the probe
/// runner, which ends the group at this limit or at `env`'s deadline, whichever comes first.
const SCOPE_PROBE_LIMIT: Duration = Duration::from_secs(2);

/// Whether a host started now can run in a delegated cgroup v2 (docs/adr/ADR-021.md decisions 2 and 8d, CR-011):
/// cgroup v2 is mounted, the user's systemd manager answers on `$XDG_RUNTIME_DIR/systemd/private`, the user lingers
/// (else that manager, and a host in one of its scopes, stops soon after the user's last session ends, which would end
/// accepted work at an SSH disconnect, REQ-005 behaviour 2; M2T06R-453) and `systemd-run` is on PATH. How, or why not
/// (then attempts are tracked by the /proc scan, degraded).
pub fn delegated_scope(env: &hoh_agents::probe::ProbeEnv) -> Result<Scope, String> {
    if !Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        return Err("cgroup v2 is not mounted at /sys/fs/cgroup".to_owned());
    }
    manager_that_lingers()?;
    let systemd_run = std::env::split_paths(&env.path)
        .map(|dir| dir.join("systemd-run"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| "systemd-run is not on PATH".to_owned())?;
    // `systemd-run --version` says `systemd <n> (...)` (M2T06R-455); bounded, and in doctor's probes (M2T06R-553).
    let bounded = hoh_agents::probe::ProbeEnv {
        timeout: env.timeout.min(SCOPE_PROBE_LIMIT),
        ..env.clone()
    };
    let version = hoh_agents::probe::run_status(&systemd_run, &["--version"], &bounded)
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .nth(1)
                .and_then(|v| v.parse::<u32>().ok())
        })
        .ok_or_else(|| {
            format!(
                "systemd-run --version gave no version within {} s ({})",
                bounded.timeout.as_secs(),
                systemd_run.display()
            )
        })?;
    Ok(Scope {
        systemd_run,
        no_expansion: version >= 254,
    })
}

/// Start `project`'s host and wait until it is ready, until `end`: in a delegated scope of the user's systemd
/// manager when there is one, so its attempts are tracked by cgroups of their own; directly otherwise, or when the
/// scope cannot be had (decision 2).
fn start(project: &Path, end: Instant) -> Result<HostInfo, Failure> {
    let env = hoh_agents::probe::ProbeEnv {
        path: std::env::var_os("PATH").unwrap_or_default(),
        fixture_root: None,
        timeout: SCOPE_PROBE_LIMIT,
        log: None,
        deadline: Some(end),
        cancel: None,
    };
    let why = match delegated_scope(&env) {
        Ok(scope) => match launch(project, end, Some(&scope), None) {
            Err(failure) if failure.exit != 78 && Instant::now() < end => {
                // The scope was refused (or the host ended for another reason): once more, directly.
                format!(
                    "systemd-run did not start it in a delegated scope ({})",
                    failure.message
                )
            }
            other => return other,
        },
        Err(why) => why,
    };
    launch(project, end, None, Some(&why))
}

/// Start `hoh __host <project>`, in `scope` if given, and wait until it is ready, until `end`. A host started
/// directly is told why (`unscoped`), which it records for `hoh doctor` (CR-012, CR12-03).
fn launch(
    project: &Path,
    end: Instant,
    scope: Option<&Scope>,
    unscoped: Option<&str>,
) -> Result<HostInfo, Failure> {
    let program = std::env::current_exe()
        .map_err(|e| Failure::unreachable(format!("cannot find the hoh program: {e}")))?;
    let mut command = match scope {
        // `--scope` runs the command itself (the pid stays the child's); `$` in the path is not expanded.
        Some(scope) => {
            let mut command = std::process::Command::new(&scope.systemd_run);
            command.args([
                "--user",
                "--scope",
                "-p",
                "Delegate=yes",
                "--quiet",
                "--collect",
            ]);
            if scope.no_expansion {
                command.arg("--expand-environment=no");
            }
            command.arg("--").arg(&program);
            command
        }
        None => std::process::Command::new(&program),
    };
    match unscoped {
        Some(why) => command.env(hoh_app::host::UNSCOPED_ENV, why),
        None => command.env_remove(hoh_app::host::UNSCOPED_ENV),
    };
    let mut child = command
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
                // Its location or database refused (DB_ON_NETWORK_FS, DB_WAL_UNSUPPORTED): its line says which
                // (M2T06R-60).
                Some(78) => Err(Failure::new(refused_code(&line), line, 78)),
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

/// Submit a command (its body with a fresh id and key) and retry the very same bytes while the host is unreachable,
/// answers a retryable 503 or no longer knows the token (a host started again), within 10 s (ADR-021 decision 4): the
/// accepted body, or why not.
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
                    error["code"]
                        .as_str()
                        .unwrap_or_else(|| status_code(reply.status)),
                    error["message"].as_str().unwrap_or("refused"),
                    if reply.status == 503 { 69 } else { 1 },
                );
                // A host started again meanwhile does not know the token this client read: the very same bytes go to
                // the one that answers now (M2T06R-04).
                if reply.status == 403 {
                    match running(project) {
                        Some(again) if again != host && Instant::now() < end => {
                            host = again;
                            continue;
                        }
                        _ => return Err(failure),
                    }
                }
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
        let path = format!("/v1/events?run_id={}&after={after}", encode(run_id));
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hoh_core::persistence::selftest::ProbeCommand;

    pub(crate) type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn only_a_plain_user_name_with_a_linger_file_lingers() -> R {
        // M2T06R-556: an empty name (or `.`, `..`, one with `/`) named the linger directory itself, which exists, so
        // a user who does not linger got a host in a scope of a manager that stops after the last session.
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("ai"), b"")?;
        std::fs::create_dir(dir.path().join("dir"))?;
        // A path that names an existing file through a directory is still no user name (M2T06R-653).
        std::fs::write(dir.path().join("dir").join("ai"), b"")?;
        assert!(lingers_in(dir.path(), "ai"));
        for name in ["", ".", "..", "ai/", "dir/ai", "./ai", "dir", "nobody"] {
            assert!(!lingers_in(dir.path(), name), "{name:?}");
        }
        Ok(())
    }

    /// Set for the probe process of the hosts these tests start.
    const PROBE_ENV: &str = "HOH_CLIENT_TESTS_PROBE";

    /// The WAL self-test's probe process of the hosts these tests start; does nothing unless started by them.
    #[test]
    #[ignore = "helper process of the WAL self-test of the hosts these tests start"]
    fn probe_helper() {
        if std::env::var_os(PROBE_ENV).is_some() {
            std::process::exit(hoh_core::persistence::selftest::probe_main());
        }
    }

    /// Start a host in a project another host of this process just let go. A process that spawns children (a parallel
    /// test's agents, or the WAL self-test's probe) shares every open file with each child until it execs, so a lock just
    /// released can still be held for that instant: a refusal (69) is tried again for a moment (as tests/host.rs does).
    pub(crate) async fn start_when_free(
        config: hoh_app::host::HostConfig,
    ) -> Result<hoh_app::host::Host, hoh_app::host::StartError> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match hoh_app::host::Host::start(config.clone()).await {
                Err(e) if e.exit_code == 69 && std::time::Instant::now() < end => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                other => return other,
            }
        }
    }

    /// An in-process host's configuration for `project` (the WAL probe is this test binary's helper).
    pub(crate) fn config(project: &Path) -> R<hoh_app::host::HostConfig> {
        let mut config = hoh_app::host::HostConfig::new(project);
        config.port = 0;
        config.store.wal_probe = Some(ProbeCommand {
            program: std::env::current_exe()?,
            args: [
                "client::tests::probe_helper",
                "--exact",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ]
            .map(std::ffi::OsString::from)
            .to_vec(),
            env: vec![(PROBE_ENV.into(), "1".into())],
            answer_timeout: Duration::from_secs(10),
        });
        Ok(config)
    }

    /// A reader over a socket whose other end has had `bytes` written and is then closed (or kept open with
    /// `keep`), so a read past them ends (or times out).
    fn fed(bytes: &[u8], keep: bool) -> R<(BufReader<UnixStream>, Option<UnixStream>)> {
        let (ours, mut theirs) = UnixStream::pair()?;
        theirs.write_all(bytes)?;
        ours.set_read_timeout(Some(Duration::from_millis(50)))?;
        Ok((BufReader::new(ours), keep.then_some(theirs)))
    }

    #[test]
    fn a_response_head_gives_its_status_and_how_its_body_is_framed() -> R {
        // ADR-021 decision 4: httparse reads the head; chunked or a Content-Length, the head over at the blank line.
        let (mut r, _) = fed(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nrest",
            false,
        )?;
        assert_eq!(head(&mut r)?, (200, true, None));
        let mut rest = String::new();
        r.read_to_string(&mut rest)?;
        assert_eq!(rest, "rest", "the body is left unread");
        let (mut r, _) = fed(
            b"HTTP/1.1 404 Not Found\r\ncontent-length: 12\r\n\r\n",
            false,
        )?;
        assert_eq!(head(&mut r)?, (404, false, Some(12)));
        let (mut r, _) = fed(b"HTTP/1.1 200 OK\r\n", false)?;
        assert_eq!(
            head(&mut r).map_err(|e| e.kind()),
            Err(std::io::ErrorKind::UnexpectedEof)
        );
        let (mut r, _) = fed(b"not http at all\r\n\r\n", false)?;
        assert!(head(&mut r).is_err());
        Ok(())
    }

    #[test]
    fn a_chunked_body_is_read_chunk_by_chunk_to_its_empty_end() -> R {
        let (mut r, _) = fed(b"5\r\nhello\r\n3;x=1\r\nabc\r\n0\r\n\r\n", false)?;
        assert_eq!(chunk(&mut r)?, b"hello");
        assert_eq!(chunk(&mut r)?, b"abc", "a chunk extension is ignored");
        assert_eq!(chunk(&mut r)?, b"");
        let (mut r, _) = fed(b"zz\r\nhello\r\n", false)?;
        assert!(chunk(&mut r).is_err());
        Ok(())
    }

    #[test]
    fn the_event_stream_gives_each_events_data_and_nothing_for_a_comment() -> R {
        // An event split over two chunks, a keep-alive comment, an event whose data spans two lines; then nothing for
        // a while (None), then the stream's end (an error, so the caller resubscribes after its last seq).
        let frames: &[&[u8]] = &[
            b"id: 3\ndata: {\"seq\":",
            b"3}\n\n",
            b": keep-alive\n\n",
            b"id: 4\ndata: {\"seq\":\ndata: 4}\n\n",
        ];
        let mut body = Vec::new();
        for frame in frames {
            body.extend_from_slice(format!("{:x}\r\n", frame.len()).as_bytes());
            body.extend_from_slice(frame);
            body.extend_from_slice(b"\r\n");
        }
        let (reader, theirs) = fed(&body, true)?;
        let mut events = Events {
            reader,
            pending: Vec::new(),
        };
        assert_eq!(events.next()?, Some(json!({"seq": 3})));
        assert_eq!(events.next()?, Some(json!({"seq": 4})));
        assert_eq!(events.next()?, None, "nothing new yet");
        let mut theirs = theirs.ok_or("no writer")?;
        theirs.write_all(b"0\r\n\r\n")?;
        assert_eq!(
            events.next().map_err(|e| e.kind()),
            Err(std::io::ErrorKind::UnexpectedEof)
        );
        Ok(())
    }

    #[test]
    fn a_refusal_is_named_by_a_code_of_the_contract() {
        // M2T06R-60, -61: a refused start by the code its line names; a body with no code by its status.
        assert_eq!(
            refused_code("hoh host: DB_WAL_UNSUPPORTED: the probe saw no lock"),
            "DB_WAL_UNSUPPORTED"
        );
        assert_eq!(refused_code("hoh host: the schema is too new"), "GONE");
        assert_eq!(status_code(404), "NOT_FOUND");
        assert_eq!(status_code(500), "GONE");
    }

    #[test]
    fn a_path_segment_keeps_only_unreserved_bytes() {
        assert_eq!(encode("R-1_a.b~"), "R-1_a.b~");
        assert_eq!(encode("R1?x"), "R1%3Fx");
        assert_eq!(encode("a b/c#%"), "a%20b%2Fc%23%25");
        assert_eq!(encode("é"), "%C3%A9");
    }

    #[test]
    fn a_command_given_a_host_started_again_since_goes_to_the_new_one() -> R {
        // M2T06R-04: the client read a host's token, that host stopped normally and another started (another token);
        // the command is accepted by the new host.
        let dir = tempfile::Builder::new().prefix("hoh").tempdir_in("/tmp")?;
        let project = dir.path().join("p");
        std::fs::create_dir_all(&project)?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let first = runtime.block_on(hoh_app::host::Host::start(config(&project)?))?;
        let stale = running(&project).ok_or("no host")?;
        runtime.block_on(first.shutdown());
        let second = runtime.block_on(start_when_free(config(&project)?))?;
        let now = running(&project).ok_or("no second host")?;
        assert_ne!(now.token, stale.token);
        let accepted = submit(
            &project,
            &stale,
            json!({
                "expected_revision": 0,
                "kind": "StartRun",
                "run_id": null,
                "payload": {"goal": "x", "mode": "AUTO", "interactive": false},
            }),
        )
        .map_err(|f| format!("{}: {}", f.code, f.message))?;
        assert_eq!(accepted["status"], "ACCEPTED");
        let run_id = accepted["run_id"].as_str().ok_or("no run_id")?;
        let run = call(&now, "GET", &format!("/v1/runs/{run_id}"), None)?;
        assert_eq!(run.status, 200);
        runtime.block_on(second.shutdown());
        Ok(())
    }
}
