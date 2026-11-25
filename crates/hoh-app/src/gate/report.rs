//! CR-038, DC-LS-A: the host never makes a report descriptor inheritable. The single-threaded gate receives it
//! atomically close-on-exec, in the same private packet as `go`; the checked command inherits neither channel.
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketFlags, SocketType, recvmsg, sendmsg, socketpair,
};
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};

pub(crate) struct Channel {
    control: OwnedFd,
    reader: OwnedFd,
    writer: Option<OwnedFd>,
}

impl Channel {
    pub(crate) fn new() -> io::Result<(Self, OwnedFd)> {
        let pair = |flags| socketpair(AddressFamily::UNIX, SocketType::SEQPACKET, flags, None);
        let (control, stdin) = pair(SocketFlags::CLOEXEC)?;
        let (reader, writer) = pair(SocketFlags::CLOEXEC | SocketFlags::NONBLOCK)?;
        Ok((
            Self {
                control,
                reader,
                writer: Some(writer),
            },
            stdin,
        ))
    }

    /// One bounded send after adoption. Even a failed send leaves no inheritable descriptor in the host.
    pub(crate) fn deliver(&mut self) -> io::Result<()> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| io::Error::other("report already delivered"))?;
        let fds = [writer.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        if !ancillary.push(SendAncillaryMessage::ScmRights(&fds)) {
            return Err(io::Error::other("report ancillary buffer too small"));
        }
        let n = sendmsg(
            &self.control,
            &[IoSlice::new(b"go\n")],
            &mut ancillary,
            SendFlags::NOSIGNAL | SendFlags::DONTWAIT,
        )?;
        if n != 3 {
            return Err(io::Error::other("short gate control packet"));
        }
        Ok(())
    }

    /// Only the exact gate report proves a failed exec; EOF, read errors and malformed reports prove nothing.
    pub(crate) fn exec_failed(&self) -> bool {
        let mut bytes = [0u8; 16];
        rustix::net::recv(
            &self.reader,
            &mut bytes,
            RecvFlags::DONTWAIT | RecvFlags::TRUNC,
        )
        .is_ok_and(|(n, _)| n == 12 && &bytes[..12] == b"exec_failed\n")
    }
}

pub(crate) fn receive(control: impl AsFd) -> io::Result<Option<OwnedFd>> {
    let mut bytes = [0u8; 32];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let message = recvmsg(
        control,
        &mut [IoSliceMut::new(&mut bytes)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    let mut fds = Vec::new();
    for message in ancillary.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            fds.extend(rights);
        }
    }
    if message
        .flags
        .intersects(rustix::net::ReturnFlags::CTRUNC | rustix::net::ReturnFlags::TRUNC)
    {
        return Err(io::Error::other("truncated gate control packet"));
    }
    if message.bytes != 3 || &bytes[..3] != b"go\n" {
        return Ok(None);
    }
    if fds.len() != 1 {
        return Err(io::Error::other(
            "gate control packet needs one report descriptor",
        ));
    }
    Ok(fds.pop())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    type R = Result<(), Box<dyn std::error::Error>>;

    /// Waits until the socket's peer is closed in every process (POLLHUP), at most 10 s.
    fn peer_gone(socket: &OwnedFd) -> R {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut fds = [PollFd::new(socket, PollFlags::empty())];
            let limit = Timespec {
                tv_sec: 0,
                tv_nsec: 10_000_000,
            };
            poll(&mut fds, Some(&limit))?;
            if fds[0].revents().contains(PollFlags::HUP) {
                return Ok(());
            }
            if std::time::Instant::now() >= deadline {
                return Err("the gate's end is still open in another process after 10 s".into());
            }
        }
    }

    #[test]
    fn test_052_04_an_unrelated_spawn_never_holds_the_checks_report_socket() -> R {
        let (mut channel, stdin) = Channel::new()?;
        let inspect = || -> io::Result<String> {
            // Deliberately bypass hoh_core::spawn, like host discovery and git helpers.
            let output = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        Command::new("sh")
                            .args(["-c", "for fd in /proc/$$/fd/*; do readlink \"$fd\"; done"])
                            .output()
                    })
                    .join()
                    .map_err(|_| std::io::Error::other("spawn thread panicked"))
                    .and_then(|result| result)
            })?;
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        };
        for stage in 0..3 {
            let held = inspect()?;
            assert!(
                !held.contains("socket:"),
                "stage {stage}: unrelated spawn holds a socket: {held}"
            );
            if stage == 0 {
                channel.deliver()?;
            }
        }
        let report = receive(&stdin)?.ok_or("missing report")?;
        assert!(rustix::io::fcntl_getfd(&report)?.contains(rustix::io::FdFlags::CLOEXEC));
        let held = inspect()?;
        assert!(!held.contains("socket:"), "received fd leaked: {held}");
        rustix::io::write(&report, b"exec_failed\n")?;
        assert!(channel.exec_failed());
        Ok(())
    }

    #[test]
    fn test_052_04_delivery_receive_and_read_failures_do_not_prove_not_started() -> R {
        let (mut channel, stdin) = Channel::new()?;
        drop(stdin);
        // FLK-13: a concurrent spawn by another test thread may hold a copy of the gate's end until its exec. Deliver
        // only once every copy is closed, as when the gate has exited.
        peer_gone(&channel.control)?;
        assert!(channel.deliver().is_err());
        assert!(!channel.exec_failed());
        let (mut channel, stdin) = Channel::new()?;
        assert!(!channel.exec_failed()); // EAGAIN
        channel.deliver()?;
        let report = receive(&stdin)?.ok_or("missing report")?;
        rustix::io::write(&report, b"exec_failed\nextra")?;
        assert!(!channel.exec_failed());
        drop(report);
        assert!(!channel.exec_failed()); // EOF
        let (channel, stdin) = Channel::new()?;
        rustix::net::send(&channel.control, b"go\n", SendFlags::NOSIGNAL)?;
        assert!(receive(&stdin).is_err());
        assert!(!channel.exec_failed());
        Ok(())
    }
}
