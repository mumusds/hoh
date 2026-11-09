//! `hoh __gate` (docs/adr/ADR-022.md decision 13): the start gate an agent is started through, without a shell.

#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use std::io::Write as _;
use std::process::{Command, Stdio};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `hoh __gate <argv>` given `input` on stdin (then closed): its exit code and stdout, and its pid.
fn gate(argv: &[&str], input: &[u8]) -> R<(Option<i32>, String, u32)> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_hoh"))
        .arg("__gate")
        .args(argv)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    stdin.write_all(input)?;
    drop(stdin);
    let out = child.wait_with_output()?;
    Ok((out.status.code(), String::from_utf8(out.stdout)?, pid))
}

#[test]
fn an_opened_gate_execs_the_agent_in_its_place_with_default_signals_and_the_rest_of_its_input() -> R
{
    // The agent is the gate's process (same pid, so the runner's identity and cgroup hold), with no signal ignored or
    // blocked, and what follows "go" on stdin is the agent's own input.
    let (code, status, pid) = gate(&["cat", "/proc/self/status"], b"go\n")?;
    assert_eq!(code, Some(0));
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .map(|v| v.trim().to_owned())
    };
    assert_eq!(field("Name:").as_deref(), Some("cat"));
    assert_eq!(field("Pid:"), Some(pid.to_string()));
    assert_eq!(
        field("SigIgn:").as_deref(),
        Some("0000000000000000"),
        "{status}"
    );
    assert_eq!(
        field("SigBlk:").as_deref(),
        Some("0000000000000000"),
        "{status}"
    );
    let (code, out, _) = gate(&["cat"], b"go\nthe prompt\nand more")?;
    assert_eq!((code, out.as_str()), (Some(0), "the prompt\nand more"));
    Ok(())
}

#[test]
fn a_gate_closed_or_told_anything_but_go_runs_nothing() -> R {
    // ADR-019 decision 8: a gate that is not opened ends, and the agent never runs.
    let dir = tempfile::tempdir()?;
    let ran = dir.path().join("ran");
    let touch = ["touch", ran.to_str().ok_or("path")?];
    for input in [&b""[..], b"go", b"no\n", b"gone\n", b" go\n"] {
        let (code, out, _) = gate(&touch, input)?;
        assert_eq!((code, out.as_str()), (Some(0), ""), "{input:?}");
        assert!(!ran.exists(), "ran after {input:?}");
    }
    Ok(())
}

#[test]
fn a_gate_without_a_program_is_a_usage_error() -> R {
    let (code, _, _) = gate(&[], b"go\n")?;
    assert_eq!(code, Some(64));
    Ok(())
}

#[test]
fn an_opened_gate_gives_the_agent_default_signals_whatever_it_inherited() -> R {
    // M2T07R-203 (ADR-022 decision 13): started by a parent that ignores SIGINT, SIGTERM and SIGHUP and blocks SIGUSR1
    // (as a shell's background job or nohup leaves them), the agent still starts with no signal ignored or blocked, so
    // its native cancel (SIGINT) and the host's SIGTERM reach it.
    let script = "import signal, subprocess, sys\n\
                  for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):\n    signal.signal(s, signal.SIG_IGN)\n\
                  signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGUSR1})\n\
                  out = subprocess.run([sys.argv[1], '__gate', 'cat', '/proc/self/status'], input=b'go\\n',\n\
                                       capture_output=True, restore_signals=False).stdout.decode()\n\
                  print('\\n'.join(l for l in out.splitlines() if l.startswith(('SigIgn', 'SigBlk'))))\n";
    let out = Command::new("python3")
        .args(["-c", script, env!("CARGO_BIN_EXE_hoh")])
        .output()?;
    assert!(out.status.success(), "{out:?}");
    let status = String::from_utf8(out.stdout)?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .map(|v| v.trim().to_owned())
    };
    assert_eq!(
        field("SigIgn:").as_deref(),
        Some("0000000000000000"),
        "{status}"
    );
    assert_eq!(
        field("SigBlk:").as_deref(),
        Some("0000000000000000"),
        "{status}"
    );
    Ok(())
}
