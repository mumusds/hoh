//! The project lock (`DirLock`).

use super::DirLock;

/// A lock released while a copy of its descriptor lives on (a child another thread forked and has not yet exec'd
/// holds one) is free: the lock belongs to the open file description, so closing one descriptor alone would leave it
/// held until the copy is closed (the doctor tests' "a host holds" flake, M2-T15 S6b).
#[test]
fn a_released_project_lock_is_free_while_a_copy_of_its_descriptor_lives() {
    let dir = tempfile::tempdir().expect("temp dir");
    let held = DirLock::try_take(dir.path())
        .expect("open")
        .expect("the lock is free");
    let copy = held.0.try_clone().expect("a copy of the descriptor");
    assert!(
        DirLock::try_take(dir.path()).expect("open").is_none(),
        "held while the lock lives"
    );
    drop(held);
    assert!(
        DirLock::try_take(dir.path()).expect("open").is_some(),
        "free once released, the copy still open"
    );
    drop(copy);
}

#[test]
fn a_bound_socket_is_private_before_the_host_can_publish_it() -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("host.sock");
    let _bound = super::bind_socket(&socket)?;
    assert_eq!(std::fs::metadata(&socket)?.permissions().mode() & 0o077, 0);
    Ok(())
}

#[test]
fn the_host_socket_keeps_the_platform_default_backlog() -> std::io::Result<()> {
    use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    // Keep the connections pending: no accept thread and no scheduling-dependent burst against a running host.
    fn pending(path: &std::path::Path) -> std::io::Result<Vec<std::os::fd::OwnedFd>> {
        let address = SocketAddrUnix::new(path)?;
        let mut clients = Vec::new();
        for _ in 0..256 {
            let fd = rustix::net::socket_with(
                AddressFamily::UNIX,
                SocketType::STREAM,
                SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
                None,
            )?;
            match rustix::net::connect(&fd, &address) {
                Ok(()) => clients.push(fd),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(e) => return Err(e.into()),
            }
        }
        Ok(clients)
    }
    let dir = tempfile::tempdir()?;
    let default_path = dir.path().join("default.sock");
    let _default = std::os::unix::net::UnixListener::bind(&default_path)?;
    let expected = pending(&default_path)?;
    let host_path = dir.path().join("host.sock");
    let host = super::bind_socket(&host_path)?;
    super::listen_socket(&host)?;
    let actual = pending(&host_path)?;
    assert_eq!(
        actual.len(),
        expected.len(),
        "pending connections before accept"
    );
    Ok(())
}
