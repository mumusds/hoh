//! `hoh __gate <program> <args…>` (docs/adr/ADR-022.md decision 13): the start gate an agent is started through, so
//! the runner can adopt the process into its attempt's cgroup and record its identity before anything of the agent
//! runs (ADR-019 decision 8, ADR-021 decision 8d), with no shell (REQ-024, REQ-025 behaviour 1).
//!
//! It reads stdin one byte at a time up to a line feed, so nothing after the line is consumed. If the line is `go`, it
//! execs the program in its own place: the same pid, process group, environment (the attempt's marker) and cgroup;
//! std's exec clears the signal mask and gives SIGPIPE its default action, so the agent starts with no signal
//! ignored or blocked. At the end of its input, or for any other line, it exits 0 and the agent never runs.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;

/// The longest line read before the gate gives up (the only line it opens for is `go`).
const MAX_LINE: usize = 16;

/// The gate's work; its exit code if it does not exec (64 without a program, 0 when it stays closed, 127 when the
/// program cannot be executed).
pub fn main(args: &[OsString]) -> i32 {
    let Some((program, rest)) = args.split_first() else {
        return 64;
    };
    let stdin = rustix::stdio::stdin();
    let mut line = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match rustix::io::read(stdin, &mut byte) {
            Ok(0) => return 0,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if line.len() < MAX_LINE => line.push(byte[0]),
            Ok(_) => return 0,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return 0,
        }
    }
    if line != b"go" {
        return 0;
    }
    let error = std::process::Command::new(program).args(rest).exec();
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "hoh gate: {}: {error}",
        program.to_string_lossy()
    );
    127
}
