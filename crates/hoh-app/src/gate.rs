//! `hoh __gate <program> <args…>` (docs/adr/ADR-022.md decision 13): the start gate an agent is started through, so
//! the runner can adopt the process into its attempt's cgroup and record its identity before anything of the agent
//! runs (ADR-019 decision 8, ADR-021 decision 8d), with no shell (REQ-024, REQ-025 behaviour 1).
//!
//! It reads stdin one byte at a time up to a line feed, so nothing after the line is consumed. If the line is `go`, it
//! execs the program in its own place: the same pid, process group, environment (the attempt's marker) and cgroup,
//! with every signal back to its default action and none blocked, whatever the gate inherited (a parent that ignores
//! SIGINT, as a shell's background job does, would otherwise make the agent deaf to its native cancel, M2T07R-203).
//! At the end of its input, or for any other line, it exits 0 and the agent never runs.

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
    defaults();
    let error = std::process::Command::new(program).args(rest).exec();
    use std::io::Write as _;
    let _ = writeln!(
        std::io::stderr(),
        "hoh gate: {}: {error}",
        program.to_string_lossy()
    );
    127
}

/// Every standard signal back to its default action in the program the gate execs, and none blocked: an action set
/// now (a caught signal) is reset by exec, an ignored one would be kept. SIGKILL and SIGSTOP have no other action;
/// SIGILL, SIGFPE and SIGSEGV are left as inherited (no handler may be set for them through signal-hook, and nothing
/// that starts the host ignores them). A signal that comes in the instant between is taken by its flag.
fn defaults() {
    for signal in 1..32 {
        if signal_hook::consts::FORBIDDEN.contains(&signal) {
            continue;
        }
        let _ = signal_hook::flag::register(
            signal,
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
    }
    let _ = nix::sys::signal::SigSet::empty().thread_set_mask();
}
