use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};

type R = Result<(), Box<dyn std::error::Error>>;

/// Trace the actual classifier's reads without logging environment values (which may contain secrets).
#[derive(Default)]
struct DiagnosticProc(RefCell<Vec<String>>);

impl Processes for DiagnosticProc {
    fn identity(&self, pid: u32) -> Option<Identity> {
        let identity = ProcFs(Path::new("/proc")).identity(pid);
        self.0
            .borrow_mut()
            .push(format!("identity({pid}) = {identity:?}"));
        identity
    }

    fn evidence(&self, pid: u32) -> Option<Evidence> {
        let evidence = ProcFs(Path::new("/proc")).evidence(pid);
        let mut trace = self.0.borrow_mut();
        if let Some(evidence) = &evidence {
            trace.push(format!(
                "evidence({pid}): exe={:?}, service_or_proxy={}, cgroups={:?}",
                evidence.executable,
                service_or_proxy(evidence),
                cgroups(&evidence.cgroup),
            ));
        } else {
            for leaf in ["environ", "cgroup", "exe"] {
                let path = format!("/proc/{pid}/{leaf}");
                let result = if leaf == "exe" {
                    std::fs::read_link(&path).map(|_| ())
                } else {
                    std::fs::read(&path).map(|_| ())
                };
                trace.push(format!("evidence({pid}): {leaf}={result:?}"));
            }
        }
        evidence
    }
}

fn peer_diagnostic(peer: &Peer) -> String {
    let proc = DiagnosticProc::default();
    let classified = peer
        .captured
        .as_ref()
        .map(|(identity, _)| classify(&proc, identity, rustix::process::getuid().as_raw()));
    format!(
        "captured={:?}, resolved={:?}, classification={classified:?}, trace={:?}",
        peer.captured,
        peer.transport.resolve(),
        proc.0.borrow(),
    )
}

// A real peer with a root-owned sudo boundary immediately above it. Neither the test runner's
// ancestry nor its environment is evidence of user authority. sudo is a required test prerequisite.
const CLIENT: &str = r#"
import os, signal, socket, sys
signal.alarm(20)
connection = None
if sys.argv[1] == 'tcp':
    connection = socket.create_connection(('127.0.0.1', int(sys.argv[2])), timeout=5)
elif sys.argv[1] == 'uds':
    connection = socket.socket(socket.AF_UNIX)
    connection.settimeout(5)
    connection.connect(sys.argv[2])
elif sys.argv[1] != 'idle':
    raise ValueError('unknown peer mode')
print(os.getpid(), flush=True)
while True:
    command = sys.stdin.buffer.read(1)
    if not command:
        break
    if command != b'c' or connection is None:
        raise ValueError('unknown peer command')
    connection.close()
    connection = None
    print('closed', flush=True)
"#;

// Model the unreadable same-uid service ancestor seen under systemd --user, without depending on
// the machine's service manager. The child is deliberately unmarked and readable; its parent is not.
const SERVICE_PARENT: &str = r#"
import ctypes, signal, subprocess, sys
signal.alarm(25)
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(4, 0, 0, 0, 0) != 0:  # PR_SET_DUMPABLE
    raise OSError(ctypes.get_errno(), 'PR_SET_DUMPABLE')
with subprocess.Popen([sys.executable, '-I', '-c', *sys.argv[1:]], env={}) as child:
    sys.exit(child.wait())
"#;

struct RealClient {
    child: Child,
    output: BufReader<ChildStdout>,
    identity: Identity,
}

impl RealClient {
    fn spawn(
        mode: &str,
        address: &str,
        service_parent: bool,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let uid = rustix::process::getuid().as_raw();
        assert_ne!(uid, 0, "this fixture requires a non-root user and sudo -n");
        let mut command = Command::new("sudo");
        command.args(["-n", "-u", &format!("#{uid}"), "--", "/usr/bin/env", "-i"]);
        if service_parent {
            command.arg("INVOCATION_ID=m3t04-test-service");
        }
        command.args(["/usr/bin/python3", "-I", "-c"]);
        if service_parent {
            command.arg(SERVICE_PARENT);
        }
        command.args([CLIENT, mode, address]);
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let output = BufReader::new(child.stdout.take().ok_or("peer stdout")?);
        // Install the owner before any fallible handshake so failures/panics also close and reap it.
        let mut client = Self {
            child,
            output,
            identity: Identity {
                pid: 0,
                ppid: 0,
                start: 0,
                uids: [uid; 4],
            },
        };
        let pid = client.read_line()?.parse()?;
        client.identity = ProcFs(Path::new("/proc"))
            .identity(pid)
            .ok_or("helper identity")?;
        assert_eq!(client.identity.uids, [uid; 4]);
        Ok(client)
    }

    fn read_line(&mut self) -> Result<String, Box<dyn std::error::Error>> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut fds = [PollFd::new(self.output.get_ref(), PollFlags::IN)];
        if poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 5,
                tv_nsec: 0,
            }),
        )? == 0
        {
            return Err("peer handshake timed out (sudo -n and python3 are required)".into());
        }
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(format!(
                "peer exited before handshake: {:?}; sudo -n is required",
                self.child.try_wait()?
            )
            .into());
        }
        Ok(line.trim().into())
    }

    fn close_socket(&mut self) -> R {
        self.child
            .stdin
            .as_mut()
            .ok_or("peer stdin")?
            .write_all(b"c")?;
        assert_eq!(self.read_line()?, "closed");
        assert_eq!(
            ProcFs(Path::new("/proc")).identity(self.identity.pid),
            Some(self.identity.clone())
        );
        Ok(())
    }

    fn finish(mut self) -> R {
        drop(self.child.stdin.take());
        assert!(self.child.wait()?.success(), "peer must exit successfully");
        Ok(())
    }
}

impl Drop for RealClient {
    fn drop(&mut self) {
        // EOF releases only our child (and the optional service parent); its alarm bounds failures.
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

struct Fixture {
    identities: BTreeMap<u32, Identity>,
    evidence: BTreeMap<u32, Evidence>,
    reads: RefCell<Vec<u32>>,
    changed: Option<u32>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            identities: [(30, 20, 1000), (20, 10, 1000), (10, 1, 0)]
                .map(|(pid, ppid, uid)| {
                    (
                        pid,
                        Identity {
                            pid,
                            ppid,
                            start: u64::from(pid),
                            uids: [uid; 4],
                        },
                    )
                })
                .into(),
            evidence: [30, 20]
                .map(|pid| {
                    (
                        pid,
                        Evidence {
                            environ: b"PATH=/usr/bin\0".to_vec(),
                            cgroup: b"0::/system.slice/incus-agent.service\n".to_vec(),
                            executable: "/usr/bin/bash".into(),
                        },
                    )
                })
                .into(),
            reads: RefCell::default(),
            changed: None,
        }
    }
    fn origin(&self) -> Origin {
        classify(
            self,
            &Identity {
                pid: 30,
                ppid: 20,
                start: 30,
                uids: [1000; 4],
            },
            1000,
        )
    }
}
impl Processes for Fixture {
    fn identity(&self, pid: u32) -> Option<Identity> {
        let mut value = self.identities.get(&pid)?.clone();
        if self.changed == Some(pid) && self.reads.borrow().contains(&pid) {
            value.start += 1;
        }
        Some(value)
    }
    fn evidence(&self, pid: u32) -> Option<Evidence> {
        self.reads.borrow_mut().push(pid);
        self.evidence.get(&pid).cloned()
    }
}

#[test]
fn test_008_05_ancestor_scan_end_and_marker_priority() -> R {
    let mut f = Fixture::new();
    assert_eq!(f.origin(), Origin::User);
    assert!(
        !f.reads.borrow().contains(&10),
        "other uid's environ/cgroup must not be read"
    );
    f.evidence.get_mut(&30).ok_or("peer")?.cgroup =
        b"0::/hoh/attempt-0123456789abcdef/child\n".to_vec();
    assert_eq!(f.origin(), Origin::AttemptCgroup);
    f.evidence.get_mut(&20).ok_or("ancestor")?.environ =
        b"HOH_ATTEMPT_ID=other-project/attempt\0".to_vec();
    assert_eq!(f.origin(), Origin::AncestorMarker);
    f.evidence.get_mut(&30).ok_or("peer")?.environ = b"HOH_ATTEMPT_ID=\0".to_vec();
    assert_eq!(
        f.origin(),
        Origin::Marker,
        "even an empty marker is not user authority"
    );
    Ok(())
}

#[test]
fn test_008_05_origin_unknown_unreadable_same_uid_proxy_service_and_identity_races() -> R {
    for pid in [30, 20] {
        let mut f = Fixture::new();
        f.evidence.remove(&pid);
        assert_eq!(f.origin(), Origin::Unknown, "unreadable pid {pid}");
        let mut f = Fixture::new();
        f.changed = Some(pid);
        assert_eq!(f.origin(), Origin::Unknown, "pid reused during scan: {pid}");
    }
    for variable in ["INVOCATION_ID=service", "LISTEN_FDS=1"] {
        let mut f = Fixture::new();
        f.evidence.get_mut(&30).ok_or("peer")?.environ = variable.as_bytes().to_vec();
        assert_eq!(f.origin(), Origin::Unknown);
    }
    let mut f = Fixture::new();
    f.evidence.get_mut(&30).ok_or("peer")?.executable = "/usr/bin/socat".into();
    assert_eq!(f.origin(), Origin::Unknown);
    let mut f = Fixture::new();
    f.identities.get_mut(&30).ok_or("peer")?.uids = [1001; 4];
    assert_eq!(f.origin(), Origin::Unknown);
    let mut f = Fixture::new();
    f.identities.remove(&30);
    assert_eq!(f.origin(), Origin::Unknown);
    let mut f = Fixture::new();
    f.identities.get_mut(&20).ok_or("parent")?.ppid = 30;
    assert_eq!(f.origin(), Origin::Unknown, "parent cycle");
    Ok(())
}

#[test]
fn test_008_05_real_chain_stops_at_different_uid() -> R {
    let proc = ProcFs(Path::new("/proc"));
    let uid = rustix::process::getuid().as_raw();
    let client = RealClient::spawn("idle", "", false)?;
    let peer = &client.identity;
    let ancestor = proc.identity(peer.ppid).ok_or("sudo boundary identity")?;
    assert_eq!(
        ancestor.uids[1], 0,
        "the helper's immediate parent must be root"
    );
    let diagnostic = DiagnosticProc::default();
    assert_eq!(
        classify(&diagnostic, peer, uid),
        Origin::User,
        "classification trace: {:?}",
        diagnostic.0.borrow(),
    );
    client.finish()
}

#[test]
fn test_008_05_real_same_uid_service_parent_is_unknown() -> R {
    let client = RealClient::spawn("idle", "", true)?;
    let proc = ProcFs(Path::new("/proc"));
    let parent = proc
        .identity(client.identity.ppid)
        .ok_or("service identity")?;
    assert_eq!(
        parent.uids, client.identity.uids,
        "service must have the peer's uid"
    );
    assert_eq!(
        proc.identity(parent.ppid).ok_or("sudo boundary")?.uids[1],
        0
    );
    let evidence = proc
        .evidence(client.identity.pid)
        .ok_or("readable peer evidence")?;
    assert!(!service_or_proxy(&evidence), "only the parent is a service");
    let error = std::fs::read(format!("/proc/{}/environ", parent.pid))
        .expect_err("service is non-dumpable");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    let diagnostic = DiagnosticProc::default();
    assert_eq!(
        classify(&diagnostic, &client.identity, client.identity.uids[1]),
        Origin::Unknown,
        "unreadable same-uid service ancestor: {:?}",
        diagnostic.0.borrow()
    );
    client.finish()
}

#[test]
fn test_008_05_tcp_mapping_and_socket_ownership_ambiguity() -> R {
    let row = "0: 0100007F:1234 0100007F:5678 01 0:0 0:0 0 1000 0 888";
    let table = format!("header\n{row}\n");
    assert_eq!(
        tcp_match(&table, "0100007F:1234", "0100007F:5678", 1000),
        Some(888)
    );
    assert_eq!(
        tcp_match(&table, "0100007F:1234", "0100007F:5678", 1001),
        None
    );
    assert_eq!(
        tcp_match(&(table + row), "0100007F:1234", "0100007F:5678", 1000),
        None
    );
    assert_eq!(
        tcp_match("header", "0100007F:1234", "0100007F:5678", 1000),
        None
    );
    let dir = tempfile::Builder::new().prefix("m3t04-").tempdir()?;
    let root = dir.path();
    assert_eq!(socket_owner(root, 888, 1000), None);
    for pid in [30, 31] {
        let p = root.join(pid.to_string());
        std::fs::create_dir_all(p.join("fd"))?;
        std::fs::write(p.join("status"), "Uid:\t1000\t1000\t1000\t1000\n")?;
        std::os::unix::fs::symlink("socket:[888]", p.join("fd/3"))?;
        if pid == 30 {
            assert_eq!(socket_owner(root, 888, 1000), Some(30));
            std::os::unix::fs::symlink("socket:[888]", p.join("fd/4"))?;
            assert_eq!(
                socket_owner(root, 888, 1000),
                Some(30),
                "dup is still one owner"
            );
        }
    }
    assert_eq!(
        socket_owner(root, 888, 1000),
        None,
        "inherited/shared socket is ambiguous"
    );
    assert_eq!(Peer::capture(Transport::Unknown).origin(), Origin::Unknown);
    Ok(())
}

#[tokio::test]
async fn test_008_05_real_tcp_and_uds_peer_lifetime() -> R {
    let tcp = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let mut client = RealClient::spawn("tcp", &tcp.local_addr()?.port().to_string(), false)?;
    let (server, remote) =
        tokio::time::timeout(std::time::Duration::from_secs(5), tcp.accept()).await??;
    let peer = Peer::capture(Transport::Tcp {
        local: server.local_addr()?,
        remote,
    });
    assert_eq!(
        peer.captured.as_ref().map(|(identity, _)| identity),
        Some(&client.identity),
        "TCP must resolve the helper: {}",
        peer_diagnostic(&peer)
    );
    assert_eq!(peer.origin(), Origin::User, "{}", peer_diagnostic(&peer));
    assert_eq!(
        peer.origin(),
        Origin::User,
        "same keepalive connection is checked again: {}",
        peer_diagnostic(&peer),
    );
    let mut stale = peer.clone();
    stale.captured.as_mut().ok_or("captured peer")?.0.start += 1;
    assert_eq!(
        stale.origin(),
        Origin::Unknown,
        "PID reuse never inherits authority"
    );
    client.close_socket()?;
    assert_eq!(
        peer.origin(),
        Origin::Unknown,
        "closed peer cannot reuse a connection identity"
    );
    client.finish()?;

    let directory = tempfile::Builder::new().prefix("m3t04-").tempdir()?;
    let path = directory.path().join("peer.sock");
    let uds = tokio::net::UnixListener::bind(&path)?;
    let mut client = RealClient::spawn("uds", path.to_str().ok_or("socket path UTF-8")?, false)?;
    let (server, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), uds.accept()).await??;
    let credentials = server.peer_cred()?;
    let peer = Peer::capture(Transport::Unix {
        uid: credentials.uid(),
        pid: u32::try_from(credentials.pid().ok_or("pid")?)?,
        inode: rustix::fs::fstat(&server)?.st_ino,
    });
    assert_eq!(
        peer.captured.as_ref().map(|(identity, _)| identity),
        Some(&client.identity),
        "UDS must resolve the helper: {}",
        peer_diagnostic(&peer)
    );
    assert_eq!(peer.origin(), Origin::User, "{}", peer_diagnostic(&peer));
    assert_eq!(
        peer.origin(),
        Origin::User,
        "UDS keepalive: {}",
        peer_diagnostic(&peer)
    );
    let mut stale = peer.clone();
    stale.captured.as_mut().ok_or("captured UDS peer")?.0.start += 1;
    assert_eq!(
        stale.origin(),
        Origin::Unknown,
        "UDS PID reuse cannot inherit authority"
    );
    client.close_socket()?;
    assert_eq!(peer.origin(), Origin::Unknown);
    client.finish()
}

#[test]
fn test_008_05_same_uid_ancestor_environ_and_cgroup_read_failures() -> R {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::Builder::new().prefix("m3t04-").tempdir()?;
    let root = directory.path();
    for (pid, ppid, uid) in [(30, 20, 1000), (20, 10, 1000), (10, 1, 0)] {
        let path = root.join(pid.to_string());
        std::fs::create_dir(&path)?;
        let mut fields = vec!["0".to_owned(); 20];
        fields[0] = "S".into();
        fields[1] = ppid.to_string();
        fields[19] = pid.to_string();
        std::fs::write(
            path.join("stat"),
            format!("{pid} (fixture) {}", fields.join(" ")),
        )?;
        std::fs::write(
            path.join("status"),
            format!("Uid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )?;
        if uid == 1000 {
            std::fs::write(path.join("environ"), b"PATH=/usr/bin\0")?;
            std::fs::write(path.join("cgroup"), b"0::/session\n")?;
            std::os::unix::fs::symlink("/usr/bin/bash", path.join("exe"))?;
        }
    }
    let proc = ProcFs(root);
    let peer = proc.identity(30).ok_or("peer")?;
    assert_eq!(classify(&proc, &peer, 1000), Origin::User);
    for leaf in ["environ", "cgroup"] {
        let path = root.join("20").join(leaf);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o0))?;
        assert_eq!(
            classify(&proc, &peer, 1000),
            Origin::Unknown,
            "unreadable same-uid ancestor {leaf}"
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        assert_eq!(classify(&proc, &peer, 1000), Origin::User);
    }
    Ok(())
}

#[test]
fn test_008_05_systemd_scope_invocation_is_not_a_service() -> R {
    let mut f = Fixture::new();
    let peer = f.evidence.get_mut(&30).ok_or("peer")?;
    peer.environ = b"INVOCATION_ID=scope\0".to_vec();
    peer.cgroup =
        b"0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-user.scope\n".to_vec();
    assert_eq!(f.origin(), Origin::User);
    f.evidence.get_mut(&30).ok_or("peer")?.cgroup =
        b"0::/user.slice/user@1000.service/app.slice/proxy.service\n".to_vec();
    assert_eq!(f.origin(), Origin::Unknown);
    Ok(())
}
