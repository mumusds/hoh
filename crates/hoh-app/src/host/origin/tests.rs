use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

type R = Result<(), Box<dyn std::error::Error>>;

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
    assert_ne!(uid, 0, "this fixture needs the real non-root chain");
    let peer = proc.identity(std::process::id()).ok_or("peer identity")?;
    let mut ancestor = peer.clone();
    while ancestor.pid > 1 && ancestor.uids[1] == uid {
        ancestor = proc.identity(ancestor.ppid).ok_or("ancestor identity")?;
    }
    assert_ne!(ancestor.uids[1], uid, "fixture must actually cross uids");
    assert_eq!(ancestor.uids[1], 0);
    assert_eq!(classify(&proc, &peer, uid), Origin::User);
    Ok(())
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
    let client = tokio::net::TcpStream::connect(tcp.local_addr()?).await?;
    let (server, remote) = tcp.accept().await?;
    let peer = Peer::capture(Transport::Tcp {
        local: server.local_addr()?,
        remote,
    });
    assert_eq!(peer.origin(), Origin::User);
    assert_eq!(
        peer.origin(),
        Origin::User,
        "same keepalive connection is checked again"
    );
    let mut stale = peer.clone();
    stale.captured.as_mut().ok_or("captured peer")?.0.start += 1;
    assert_eq!(
        stale.origin(),
        Origin::Unknown,
        "PID reuse never inherits authority"
    );
    drop(client);
    assert_eq!(
        peer.origin(),
        Origin::Unknown,
        "closed peer cannot reuse a connection identity"
    );

    let (client, server) = tokio::net::UnixStream::pair()?;
    let credentials = server.peer_cred()?;
    let peer = Peer::capture(Transport::Unix {
        uid: credentials.uid(),
        pid: u32::try_from(credentials.pid().ok_or("pid")?)?,
        inode: rustix::fs::fstat(&server)?.st_ino,
    });
    assert_eq!(peer.origin(), Origin::User);
    drop(client);
    assert_eq!(peer.origin(), Origin::Unknown);
    Ok(())
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
