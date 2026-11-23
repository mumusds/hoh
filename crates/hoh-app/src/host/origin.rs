//! Local peer evidence for specification approvals (ADR-038). No client-supplied identity is trusted.
use hoh_core::origin::Origin;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    pid: u32,
    ppid: u32,
    start: u64,
    uids: [u32; 4],
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Evidence {
    environ: Vec<u8>,
    cgroup: Vec<u8>,
    executable: PathBuf,
}

trait Processes {
    fn identity(&self, pid: u32) -> Option<Identity>;
    fn evidence(&self, pid: u32) -> Option<Evidence>;
}

/// Scan only the same-uid chain. PID 1 and the first different-uid ancestor are boundaries,
/// not evidence of human intent (CTO 2026-11-22). Any incomplete observation fails closed.
fn classify(processes: &impl Processes, peer: &Identity, uid: u32) -> Origin {
    observe(processes, peer, uid).unwrap_or(Origin::Unknown)
}

fn observe(processes: &impl Processes, peer: &Identity, uid: u32) -> Option<Origin> {
    if peer.pid <= 1 || peer.uids != [uid; 4] || processes.identity(peer.pid)? != *peer {
        return None;
    }
    let mut chain = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut current = peer.clone();
    let boundary;
    loop {
        if !seen.insert(current.pid) || chain.len() >= 1024 {
            return None;
        }
        if current.pid == 1 || current.uids[1] != uid {
            boundary = current;
            break;
        }
        if current.uids != [uid; 4] {
            return None;
        }
        let evidence = processes.evidence(current.pid)?;
        let parent = processes.identity(current.ppid)?;
        chain.push((current, evidence));
        current = parent;
    }
    // Recheck the entire observed chain, not just each member immediately after its first read.
    // Parent changes, PID reuse, exec, marker changes and cgroup moves invalidate this observation.
    for (identity, evidence) in &chain {
        if processes.identity(identity.pid)? != *identity
            || processes.evidence(identity.pid)? != *evidence
        {
            return None;
        }
    }
    if processes.identity(boundary.pid)? != boundary {
        return None;
    }
    let marked = |e: &Evidence| {
        e.environ
            .split(|b| *b == 0)
            .any(|v| v.starts_with(b"HOH_ATTEMPT_ID="))
    };
    if marked(&chain.first()?.1) {
        return Some(Origin::Marker);
    }
    if chain.iter().skip(1).any(|(_, e)| marked(e)) {
        return Some(Origin::AncestorMarker);
    }
    // Parse every scanned cgroup, including ancestors': unreadable/malformed is never user.
    let groups = chain
        .iter()
        .map(|(_, e)| cgroups(&e.cgroup))
        .collect::<Option<Vec<_>>>()?;
    if groups.first()?.iter().any(|path| attempt_group(path)) {
        return Some(Origin::AttemptCgroup);
    }
    if service_or_proxy(&chain.first()?.1) {
        return None;
    }
    Some(Origin::User)
}

fn cgroups(bytes: &[u8]) -> Option<Vec<&str>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let paths = text
        .lines()
        .map(|line| {
            let mut fields = line.splitn(3, ':');
            fields.next()?.parse::<u32>().ok()?;
            fields.next()?;
            let path = fields.next()?;
            (path.starts_with('/') && !path.split('/').any(|p| p == "..")).then_some(path)
        })
        .collect::<Option<Vec<_>>>()?;
    (!paths.is_empty()).then_some(paths)
}

fn attempt_group(path: &str) -> bool {
    path.split('/').any(|part| {
        part.strip_prefix("attempt-")
            .is_some_and(|hex| hex.len() == 16 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
    })
}

fn service_or_proxy(evidence: &Evidence) -> bool {
    evidence.environ.split(|b| *b == 0).any(|v| {
        v.starts_with(b"LISTEN_FDS=")
            || v.starts_with(b"LISTEN_PID=")
            || (v.starts_with(b"INVOCATION_ID=")
                && !cgroups(&evidence.cgroup).is_some_and(|groups| {
                    // systemd-run --scope also sets INVOCATION_ID. The closest unit, not an enclosing
                    // user@UID.service, decides whether that variable denotes a user scope.
                    groups.iter().any(|path| {
                        path.rsplit('/')
                            .find(|part| part.ends_with(".scope") || part.ends_with(".service"))
                            .is_some_and(|unit| unit.ends_with(".scope"))
                    })
                }))
    }) || evidence
        .executable
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                "socat"
                    | "ssh"
                    | "sshd"
                    | "sshd-session"
                    | "systemd"
                    | "nginx"
                    | "haproxy"
                    | "stunnel"
                    | "stunnel4"
                    | "docker-proxy"
                    | "kubectl"
            )
        })
}

struct ProcFs<'a>(&'a Path);

impl Processes for ProcFs<'_> {
    fn identity(&self, pid: u32) -> Option<Identity> {
        let directory = self.0.join(pid.to_string());
        // status, not directory ownership: a non-dumpable same-uid process has a root-owned /proc directory.
        // Name is the comm and need not be UTF-8, so the file is bytes and only the Uid line is decoded.
        let status = std::fs::read(directory.join("status")).ok()?;
        let uids = status_uid_fields(&status)?;
        let stat = std::fs::read(directory.join("stat")).ok()?;
        let end = stat.windows(2).rposition(|w| w == b") ")?;
        let fields = std::str::from_utf8(stat.get(end + 2..)?)
            .ok()?
            .split_whitespace()
            .collect::<Vec<_>>();
        if matches!(*fields.first()?, "Z" | "X" | "x") {
            return None;
        }
        Some(Identity {
            pid,
            ppid: fields.get(1)?.parse().ok()?,
            start: fields.get(19)?.parse().ok()?,
            uids: uids.try_into().ok()?,
        })
    }
    fn evidence(&self, pid: u32) -> Option<Evidence> {
        let directory = self.0.join(pid.to_string());
        Some(Evidence {
            environ: std::fs::read(directory.join("environ")).ok()?,
            cgroup: std::fs::read(directory.join("cgroup")).ok()?,
            executable: std::fs::read_link(directory.join("exe")).ok()?,
        })
    }
}

/// The listener's immutable connection facts; the accepted socket stays open while these are used.
#[derive(Debug, Clone)]
pub(super) enum Transport {
    Tcp {
        local: std::net::SocketAddr,
        remote: std::net::SocketAddr,
    },
    Unix {
        uid: u32,
        pid: u32,
        inode: u64,
    },
    Unknown,
}

#[derive(Debug, Clone)]
pub(super) struct Peer {
    transport: Transport,
    captured: Option<(Identity, u64)>,
}

impl Peer {
    pub(super) fn capture(transport: Transport) -> Self {
        let captured = transport.resolve();
        Self {
            transport,
            captured,
        }
    }
    pub(super) fn origin(&self) -> Origin {
        let Some((identity, inode)) = &self.captured else {
            return Origin::Unknown;
        };
        if self.transport.resolve().as_ref() != self.captured.as_ref() {
            return Origin::Unknown;
        }
        let origin = classify(
            &ProcFs(Path::new("/proc")),
            identity,
            rustix::process::getuid().as_raw(),
        );
        if self.transport.resolve().as_ref() != Some(&(identity.clone(), *inode)) {
            return Origin::Unknown;
        }
        origin
    }
}

impl Transport {
    fn resolve(&self) -> Option<(Identity, u64)> {
        // A fork can duplicate a CLOEXEC descriptor until exec closes it. For TCP the inode is the
        // client's socket: the approval helper creates it after exec and does not fork, and a child of
        // the test binary inherits only the accepted server socket, a different inode. Lengthening this
        // retry is not a fix for those fixtures (ADR-038, 2026-11-23). Persistent ambiguity is still
        // Unknown. Never retry a changed captured identity or unreadable ancestor evidence: those checks
        // remain outside this lookup.
        for observation in 0..3 {
            if let Some(peer) = self.resolve_once() {
                return Some(peer);
            }
            if observation < 2 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        None
    }

    fn resolve_once(&self) -> Option<(Identity, u64)> {
        let uid = rustix::process::getuid().as_raw();
        let (inode, expected_pid) = match self {
            Self::Tcp { local, remote } => (tcp_inode(*local, *remote, uid)?, None),
            Self::Unix {
                uid: peer_uid,
                pid,
                inode,
            } if *peer_uid == uid => {
                let Some(peer) = unix_peer_inode(*inode) else {
                    return origin_miss("unix-peer-inode");
                };
                (peer, Some(*pid))
            }
            _ => return origin_miss("transport"),
        };
        let Some(pid) = socket_owner(Path::new("/proc"), inode, uid) else {
            // `socket_owner` already recorded the step that returned none.
            return None;
        };
        if expected_pid.is_some_and(|expected| expected != pid) {
            return origin_miss("peercred-pid");
        }
        let Some(identity) = ProcFs(Path::new("/proc")).identity(pid) else {
            return origin_miss(&format!("identity pid={pid}"));
        };
        if identity.uids != [uid; 4] {
            return origin_miss("identity-uid");
        }
        Some((identity, inode))
    }
}

fn tcp_inode(local: std::net::SocketAddr, remote: std::net::SocketAddr, uid: u32) -> Option<u64> {
    // This host binds only IPv4 loopback. The endpoints come from the accepted socket, never HTTP headers.
    let (std::net::SocketAddr::V4(local), std::net::SocketAddr::V4(remote)) = (local, remote)
    else {
        return origin_miss("tcp-not-v4");
    };
    let address = |a: std::net::SocketAddrV4| {
        format!(
            "{:08X}:{:04X}",
            u32::from_ne_bytes(a.ip().octets()),
            a.port()
        )
    };
    let text = match std::fs::read_to_string("/proc/net/tcp") {
        Ok(text) => text,
        Err(error) => return origin_miss(&io_step("tcp-table", &error)),
    };
    tcp_match(&text, &address(remote), &address(local), uid)
}

fn tcp_match(text: &str, local: &str, remote: &str, uid: u32) -> Option<u64> {
    let mut found = None;
    for line in text.lines().skip(1) {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.get(1) == Some(&local) && fields.get(2) == Some(&remote) {
            if fields.get(3) != Some(&"01") {
                return origin_miss("tcp_match not-established");
            }
            let Some(row_uid) = fields.get(7).and_then(|value| value.parse::<u32>().ok()) else {
                return origin_miss("tcp_match uid-unreadable");
            };
            if row_uid != uid {
                return origin_miss("tcp_match uid");
            }
            if found.is_some() {
                return origin_miss("tcp_match duplicate");
            }
            let Some(inode) = fields.get(9).and_then(|value| value.parse::<u64>().ok()) else {
                return origin_miss("tcp_match inode-unreadable");
            };
            if inode == 0 {
                return origin_miss("tcp_match inode-zero");
            }
            found = Some(inode);
        }
    }
    found.or_else(|| origin_miss("tcp_match missing"))
}

/// One directory read. A per-entry error is kept. The `/proc` root refuses on any of them,
/// because the stream ends there. A per-pid fd listing still skips a pid that is gone.
trait ProcIo {
    fn list(&self, dir: &Path) -> std::io::Result<Vec<std::io::Result<std::ffi::OsString>>>;
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>>;
    fn read_link(&self, path: &Path) -> std::io::Result<PathBuf>;
}

struct StdProc;

impl ProcIo for StdProc {
    fn list(&self, dir: &Path) -> std::io::Result<Vec<std::io::Result<std::ffi::OsString>>> {
        let mut names = Vec::new();
        for entry in std::fs::read_dir(dir)? {
            names.push(entry.map(|entry| entry.file_name()));
        }
        Ok(names)
    }
    fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }
    fn read_link(&self, path: &Path) -> std::io::Result<PathBuf> {
        std::fs::read_link(path)
    }
}

fn socket_owner(root: &Path, inode: u64, uid: u32) -> Option<u32> {
    socket_owner_at(&StdProc, root, inode, uid)
}

/// One line on the host's stderr. `hoh __host` moves that descriptor to `.hoh/host.log` once ready.
/// A step that returns none is recorded here so a later `origin_unknown` names the step. Skipped
/// pids (already gone, or an unreadable fd of a process that is not the peer) are not recorded.
fn origin_miss<T>(step: &str) -> Option<T> {
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "hoh host: origin lookup returned none: {step}"
    );
    None
}

fn io_step(step: &str, error: &std::io::Error) -> String {
    format!(
        "{step} kind={:?} raw={:?}",
        error.kind(),
        error.raw_os_error()
    )
}

/// The decimal fields of the `Uid:` line. The rest of `status`, including `Name`, stays bytes:
/// a live process whose comm is not UTF-8 must not fail this read.
fn status_uid_fields(bytes: &[u8]) -> Option<Vec<u32>> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let Some(rest) = line.strip_prefix(b"Uid:") else {
            continue;
        };
        let text = std::str::from_utf8(rest).ok()?;
        return text
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<_>, _>>()
            .ok();
    }
    None
}

/// A pid that is no longer there. A task reaped between the directory read and the follow-up is
/// ESRCH (errno 3). `ErrorKind` does not map that to `NotFound` (ENOENT). Both mean this pid is
/// not a socket holder, so an unrelated exit must not fail the lookup. Any other status error
/// still fails closed. An unreadable same-uid ancestor is judged in `observe`, not here
/// (CTO 2026-11-22): that evidence is not retried and is not a user.
fn process_gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
        || error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error())
}

fn socket_owner_at(io: &impl ProcIo, root: &Path, inode: u64, uid: u32) -> Option<u32> {
    let target = PathBuf::from(format!("socket:[{inode}]"));
    let mut owner = None;
    let entries = match io.list(root) {
        Ok(entries) => entries,
        Err(error) => return origin_miss(&io_step("proc-root-open", &error)),
    };
    for entry in entries {
        // A root entry error ends the directory stream, so the names after it are missing.
        // Skipping it would truncate the scan and return an owner found before the gap.
        // A pid that disappears is skipped only on the per-pid reads below.
        let name = match entry {
            Ok(name) => name,
            Err(error) => return origin_miss(&io_step("proc-root", &error)),
        };
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let directory = root.join(pid.to_string());
        let status = match io.read(&directory.join("status")) {
            Ok(status) => status,
            Err(error) if process_gone(&error) => continue,
            Err(error) => {
                return origin_miss(&io_step(&format!("status pid={pid}"), &error));
            }
        };
        let Some(uids) = status_uid_fields(&status) else {
            return origin_miss(&format!("status pid={pid} uid-line"));
        };
        if uids.get(1) != Some(&uid) {
            continue;
        }
        let descriptors = match io.list(&directory.join("fd")) {
            Ok(descriptors) => descriptors,
            // An unrelated non-dumpable process (e.g. systemd --user) is not a peer candidate.
            // The selected peer's evidence still has to be readable. See ADR-038's fd visibility limit.
            Err(error)
                if process_gone(&error) || error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                continue;
            }
            Err(error) => return origin_miss(&io_step(&format!("fd-list pid={pid}"), &error)),
        };
        for descriptor in descriptors {
            let descriptor = match descriptor {
                Ok(descriptor) => descriptor,
                Err(error) if process_gone(&error) => continue,
                Err(error) => {
                    return origin_miss(&io_step(&format!("fd-entry pid={pid}"), &error));
                }
            };
            match io.read_link(&directory.join("fd").join(descriptor)) {
                Ok(link) if link == target => {
                    if owner.is_some_and(|old| old != pid) {
                        return origin_miss("owner-ambiguous");
                    }
                    owner = Some(pid);
                }
                Ok(_) => {}
                Err(error)
                    if process_gone(&error)
                        || error.kind() == std::io::ErrorKind::PermissionDenied => {}
                Err(error) => {
                    return origin_miss(&io_step(&format!("readlink pid={pid}"), &error));
                }
            }
        }
    }
    owner.or_else(|| origin_miss("socket-owner-missing"))
}

/// UNIX_DIAG_PEER links the accepted UDS inode to its peer inode; SO_PEERCRED alone survives fd handoff.
/// Linux UAPI: linux/unix_diag.h and linux/sock_diag.h. A single bounded query, no privileged operation.
fn unix_peer_inode(inode: u64) -> Option<u64> {
    use rustix::net::{self, AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType};
    let inode = u32::try_from(inode).ok()?;
    let socket = net::socket_with(
        AddressFamily::NETLINK,
        SocketType::DGRAM,
        SocketFlags::CLOEXEC,
        Some(net::netlink::SOCK_DIAG),
    )
    .ok()?;
    net::connect(&socket, &net::netlink::SocketAddrNetlink::new(0, 0)).ok()?;
    net::sockopt::set_socket_timeout(
        &socket,
        net::sockopt::Timeout::Recv,
        Some(std::time::Duration::from_millis(250)),
    )
    .ok()?;
    let mut request = Vec::new();
    request.extend_from_slice(&40u32.to_ne_bytes()); // nlmsghdr + unix_diag_req
    request.extend_from_slice(&20u16.to_ne_bytes()); // SOCK_DIAG_BY_FAMILY
    request.extend_from_slice(&1u16.to_ne_bytes()); // NLM_F_REQUEST
    request.extend_from_slice(&1u32.to_ne_bytes()); // sequence
    request.extend_from_slice(&0u32.to_ne_bytes());
    request.extend_from_slice(&[1, 0, 0, 0]); // AF_UNIX
    for value in [u32::MAX, inode, 4, u32::MAX, u32::MAX] {
        request.extend_from_slice(&value.to_ne_bytes());
    }
    if net::send(&socket, &request, SendFlags::empty()).ok()? != request.len() {
        return None;
    }
    let mut buffer = [0u8; 4096];
    let (size, _) = net::recv(&socket, &mut buffer[..], RecvFlags::empty()).ok()?;
    unix_diag_peer(&buffer[..size], inode)
}

fn unix_diag_peer(bytes: &[u8], inode: u32) -> Option<u64> {
    let u32_at = |offset| {
        Some(u32::from_ne_bytes(
            bytes.get(offset..offset + 4)?.try_into().ok()?,
        ))
    };
    let length = usize::try_from(u32_at(0)?).ok()?;
    if length > bytes.len()
        || length < 32
        || bytes.get(4..6)? != 20u16.to_ne_bytes()
        || u32_at(8)? != 1
        || bytes.get(16)? != &1
        || u32_at(20)? != inode
    {
        return None;
    }
    let mut offset = 32;
    while offset + 4 <= length {
        let size = usize::from(u16::from_ne_bytes(
            bytes.get(offset..offset + 2)?.try_into().ok()?,
        ));
        let kind = u16::from_ne_bytes(bytes.get(offset + 2..offset + 4)?.try_into().ok()?);
        if size < 4 || offset + size > length {
            return None;
        }
        if kind == 2 && size == 8 {
            let peer = u32_at(offset + 4)?;
            return (peer != 0).then_some(u64::from(peer));
        }
        offset += (size + 3) & !3;
    }
    None
}

#[cfg(test)]
mod tests;
