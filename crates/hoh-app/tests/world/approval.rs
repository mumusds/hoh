//! Approval fixtures use a known same-uid peer below a root-owned sudo boundary (ADR-038).
//! The test runner's launcher/ancestors are never the evidence for a user's authority.
use super::{Command, R};

pub fn python(script: &str, environment: &[(&str, &str)]) -> Command {
    let uid = rustix::process::getuid().as_raw();
    assert_ne!(
        uid, 0,
        "approval fixtures require a non-root user and sudo -n"
    );
    let mut command = Command::new("sudo");
    command.args(["-n", "-u", &format!("#{uid}"), "--", "/usr/bin/env", "-i"]);
    for (key, value) in environment {
        command.arg(format!("{key}={value}"));
    }
    // Assert the boundary before sending anything. Failure is a fixture error, never a skipped test.
    // The alarm bounds failures; output() owns and reaps this exact child on every exit path.
    let script = format!(
        "import os, signal\n\
         from pathlib import Path\n\
         signal.alarm(30)\n\
         assert os.getresuid() == ({uid}, {uid}, {uid})\n\
         parent = Path('/proc', str(os.getppid()), 'status').read_text()\n\
         uids = next(line.split()[1:] for line in parent.splitlines() if line.startswith('Uid:'))\n\
         assert int(uids[1]) == 0, ('expected a root-owned sudo boundary', os.getppid(), uids)\n\
         {script}"
    );
    command.args(["/usr/bin/python3", "-I", "-c", &script]);
    command
}

/// Send the supplied HTTP bytes unchanged, from a known unmarked user peer over TCP or UDS.
pub fn request(unix: bool, address: &str, request: &str) -> R<(u16, String)> {
    let output = python(
        r#"
import socket, sys
s = socket.socket(socket.AF_UNIX if sys.argv[1] == 'unix' else socket.AF_INET)
s.settimeout(15)
s.connect(sys.argv[2] if sys.argv[1] == 'unix' else ('127.0.0.1', int(sys.argv[2])))
s.sendall(sys.argv[3].encode())
while True:
    data = s.recv(65536)
    if not data: break
    sys.stdout.buffer.write(data)
"#,
        &[],
    )
    .args([if unix { "unix" } else { "tcp" }, address, request])
    .output()?;
    if !output.status.success() {
        return Err(format!(
            "approval peer failed (sudo -n and python3 required): {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    let response = String::from_utf8(output.stdout)?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or("no response head")?;
    Ok((
        head.split_whitespace().nth(1).ok_or("no status")?.parse()?,
        body.to_owned(),
    ))
}
