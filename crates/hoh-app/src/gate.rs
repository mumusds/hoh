//! `hoh __gate <program> <args…>` (docs/adr/ADR-022.md decision 13): the start gate an agent is started through, so
//! the runner can adopt the process into its attempt's cgroup and record its identity before anything of the agent
//! runs (ADR-019 decision 8, ADR-021 decision 8d), with no shell (REQ-024, REQ-025 behaviour 1).
//!
//! It reads stdin one byte at a time up to a line feed, so nothing after the line is consumed. If the line is `go`, it
//! execs the program in its own place: the same pid, process group, attempt marker and cgroup. The environment is
//! inherited except for the enclosing development check's evidence context (see [`agent_environment`]),
//! with every signal back to its default action and none blocked, whatever the gate inherited (a parent that ignores
//! SIGINT, as a shell's background job does, would otherwise make the agent deaf to its native cancel, M2T07R-203).
//! At the end of its input, or for any other line, it exits 0 and the agent never runs.
//!
//! The product's gate is checked (docs/adr/ADR-030.md decision 9, "执行时的身份"): `hoh __gate --expect <agent>
//! <launcher_path> <launcher_sha256> <implementation_path> <implementation_sha256> --stamp <launcher's
//! dev:ino:ctime_sec:ctime_nsec> <the implementation's> --plan <fd|path> --status <path> -- <program> <args…>`, all four
//! options or none (64 otherwise). It opens only on `go <key>` (64 hex digits, lower case; 64 for any other line),
//! then verifies that the agent's implementation is still the one expected: resolved again the probe's way to the same
//! path, the launcher and the implementation opened and hashed through their descriptors, the implementation's stamp
//! the same before and after it was hashed, both descriptors' stamps the expected ones. It records its decision as one
//! JSON line in the status file: a refusal with the key, then exit 65 and the agent never runs; `verified` (no key), then
//! it execs the implementation it hashed as the plan says; `exec_failed` and 127 if that fails. A decision it cannot
//! record is not acted on: no exec, 65. The key comes only on stdin and is never written anywhere but in a refusal.

use hoh_agents::adapter::{Expect, stamp_of_text};
use hoh_agents::compat::Agent;
use hoh_agents::probe::{self, Exec, Plan};
use hoh_core::staffing::{AgentIdentity, FileStamp, Stamp};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};

/// The longest line read before the gate gives up: `go`, or `go ` and a key of 64 hex digits.
const MAX_LINE: usize = 80;

/// A usage error: no program, the checking options not all given, a go line without its key.
const USAGE: i32 = 64;
/// A refusal, or a decision that could not be recorded: the agent never ran.
const REFUSED: i32 = 65;
/// The program could not be executed.
const EXEC_FAILED: i32 = 127;

/// What a checked gate verifies and where it records its decision.
struct Checked {
    expect: Expect,
    status: PathBuf,
}

/// The gate's command line: what it checks (none: today's unchecked gate), then the program and its arguments.
struct Call<'a> {
    checked: Option<Checked>,
    program: &'a OsString,
    rest: &'a [OsString],
}

/// `args` read as the gate's command line; `None` for a usage error.
fn parse(args: &[OsString]) -> Option<Call<'_>> {
    let text = |arg: &OsString| arg.to_str().map(str::to_owned);
    if args
        .first()?
        .to_str()
        .is_none_or(|first| !first.starts_with("--"))
    {
        let (program, rest) = args.split_first()?;
        return Some(Call {
            checked: None,
            program,
            rest,
        });
    }
    let (mut identity, mut stamp, mut plan, mut status) = (None, None, None, None);
    let mut agent = None;
    let mut at = 0;
    let values_at = |at: usize, n: usize| args.get(at + 1..at + 1 + n);
    loop {
        let option = args.get(at)?.to_str()?;
        let values = |n: usize| values_at(at, n);
        match option {
            "--" => break,
            "--expect" if identity.is_none() => {
                let v = values(5)?;
                agent = Some(Agent::named(v[0].to_str()?)?);
                identity = Some(AgentIdentity {
                    launcher_path: text(&v[1])?,
                    launcher_sha256: text(&v[2])?,
                    implementation_path: text(&v[3])?,
                    implementation_sha256: text(&v[4])?,
                });
                at += 6;
            }
            "--stamp" if stamp.is_none() => {
                let v = values(2)?;
                stamp = Some(Stamp {
                    launcher: stamp_of_text(v[0].to_str()?)?,
                    implementation: stamp_of_text(v[1].to_str()?)?,
                });
                at += 3;
            }
            "--plan" if plan.is_none() => {
                plan = Some(Exec::named(values(1)?[0].to_str()?)?);
                at += 2;
            }
            "--status" if status.is_none() => {
                status = Some(PathBuf::from(&values(1)?[0]));
                at += 2;
            }
            _ => return None,
        }
    }
    let (program, rest) = args.get(at + 1..)?.split_first()?;
    Some(Call {
        checked: Some(Checked {
            expect: Expect {
                agent: agent?,
                identity: identity?,
                stamp: stamp?,
                plan: plan?,
            },
            status: status?,
        }),
        program,
        rest,
    })
}

/// The gate's line on stdin, read one byte at a time up to its line feed.
enum Line {
    /// The input ended (or could not be read) first: the gate was never opened.
    Ended,
    /// Longer than [`MAX_LINE`].
    TooLong,
    Read(Vec<u8>),
}

fn read_line() -> Line {
    let stdin = rustix::stdio::stdin();
    let mut line = Vec::new();
    loop {
        let mut byte = [0u8; 1];
        match rustix::io::read(stdin, &mut byte) {
            Ok(0) => return Line::Ended,
            Ok(_) if byte[0] == b'\n' => return Line::Read(line),
            Ok(_) if line.len() < MAX_LINE => line.push(byte[0]),
            Ok(_) => return Line::TooLong,
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Line::Ended,
        }
    }
}

/// The key of a checked gate's go line: `go ` and 64 lower-case hex digits.
fn key_of(line: &[u8]) -> Option<String> {
    let key = line.strip_prefix(b"go ")?;
    (key.len() == 64
        && key
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)))
    .then(|| String::from_utf8_lossy(key).into_owned())
}

/// The gate's work; its exit code if it does not exec: 64 for a usage error, 0 when it stays closed, 65 when it
/// refused or could not record its decision, 127 when the program cannot be executed.
pub fn main(args: &[OsString]) -> i32 {
    let Some(call) = parse(args) else {
        return USAGE;
    };
    let line = match read_line() {
        Line::Ended => return 0,
        Line::TooLong => None,
        Line::Read(line) => Some(line),
    };
    let Some(checked) = call.checked else {
        if line.as_deref() != Some(b"go") {
            return 0;
        }
        defaults();
        let mut command = std::process::Command::new(call.program);
        command.args(call.rest);
        agent_environment(&mut command);
        let error = command.exec();
        say(&format!("{}: {error}", call.program.to_string_lossy()));
        return EXEC_FAILED;
    };
    let Some(key) = line.as_deref().and_then(key_of) else {
        return USAGE;
    };
    match verified(&checked.expect) {
        Err(mismatch) => {
            let _ = record(&checked.status, &mismatch.refused(&key));
            say(&format!("COMPAT_UNVERIFIED: {}", mismatch.item));
            REFUSED
        }
        Ok(verified) => {
            if let Err(e) = record(&checked.status, &verified.record()) {
                say(&format!(
                    "its decision cannot be recorded in {}: {e}",
                    checked.status.display()
                ));
                return REFUSED;
            }
            defaults();
            let error = exec(&verified, call.rest);
            let _ = record(
                &checked.status,
                &json!({"outcome": "exec_failed", "errno": error.raw_os_error()}),
            );
            say(&format!(
                "{}: {error}",
                verified.plan.implementation.display()
            ));
            EXEC_FAILED
        }
    }
}

pub(crate) mod report;

/// `hoh __check-gate <program> <args…>`: private SEQPACKET stdin carries `go` plus one SCM_RIGHTS report fd.
/// The host sends it only after adoption; recvmsg installs it CLOEXEC atomically. Exec closes it and replaces the
/// control stdin with /dev/null, so the command can affect neither channel. Protocol errors never report exec failure.
pub fn check_main(args: &[OsString]) -> i32 {
    let Some((program, rest)) = args.split_first() else {
        return USAGE;
    };
    let report = match report::receive(rustix::stdio::stdin()) {
        Ok(Some(report)) => report,
        Ok(None) => return 0,
        Err(e) => {
            say(&format!("cannot receive gate report descriptor: {e}"));
            return EXEC_FAILED;
        }
    };
    defaults();
    let error = std::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .exec();
    say(&format!("{}: {error}", program.to_string_lossy()));
    if let Err(e) = rustix::io::write(&report, b"exec_failed\n") {
        say(&format!("cannot report the failed exec: {e}"));
    }
    EXEC_FAILED
}

/// One line on stderr, for a person.
fn say(what: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "hoh gate: {what}");
}

/// Append `record` as one JSON line to `path`: opened write-only, appending, created 0600, never through a link, not
/// inherited by the agent, and closed again.
fn record(path: &Path, record: &Value) -> std::io::Result<()> {
    use rustix::fs::{Mode, OFlags};
    use std::io::Write as _;
    let fd = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::APPEND | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::from_raw_mode(0o600),
    )?;
    File::from(fd).write_all(format!("{record}\n").as_bytes())
}

/// What did not match the gate's expectation (docs/adr/ADR-030.md decision 9): the first item that did not, whether the
/// stamps were all that did not, and what was expected and seen of that item.
#[derive(Debug, Clone, PartialEq)]
pub struct Mismatch {
    pub item: &'static str,
    pub stamp_only: bool,
    pub expected: Value,
    pub seen: Value,
}

impl Mismatch {
    fn of(item: &'static str, expected: Value, seen: Value) -> Mismatch {
        Mismatch {
            item,
            stamp_only: false,
            expected,
            seen,
        }
    }

    /// Its `refused` record, with the attempt's key: the only record that carries it.
    fn refused(&self, key: &str) -> Value {
        json!({"outcome": "refused", "key": key, "code": "COMPAT_UNVERIFIED", "item": self.item,
               "stamp_only": self.stamp_only, "expected": self.expected, "seen": self.seen})
    }
}

/// An implementation the gate verified: the descriptor it hashed, opened read-only and not inherited, how it is to be
/// executed, and its stamp.
#[derive(Debug)]
pub struct Verified {
    fd: File,
    pub plan: Plan,
    stamp: FileStamp,
}

impl Verified {
    /// Its `verified` record: no key, nothing derived from one.
    fn record(&self) -> Value {
        json!({"outcome": "verified", "plan": self.plan.exec.record(), "implementation": {
            "path": self.plan.implementation.display().to_string(), "dev": self.stamp.dev, "ino": self.stamp.ino,
            "ctime_sec": self.stamp.ctime_sec, "ctime_nsec": self.stamp.ctime_nsec}})
    }
}

/// Whether the agent `expect` names is still the one expected, and the descriptor of its implementation if it is
/// (docs/adr/ADR-030.md decision 9, "执行时的身份").
pub fn verified(expect: &Expect) -> Result<Verified, Mismatch> {
    verified_with(expect, &|| {})
}

/// [`verified`], `between` called after the implementation was hashed and before its stamp is read again.
fn verified_with(expect: &Expect, between: &dyn Fn()) -> Result<Verified, Mismatch> {
    use rustix::fs::{Mode, OFlags};
    let identity = &expect.identity;
    let launcher = Path::new(&identity.launcher_path);
    // Resolved again as the probe resolves it: another implementation path is another identity.
    let implementation = match probe::resolve_implementation(expect.agent, launcher) {
        Ok(path) if path.as_os_str() == identity.implementation_path.as_str() => path,
        Ok(path) => {
            return Err(Mismatch::of(
                "implementation_path",
                json!(identity.implementation_path),
                json!(path.display().to_string()),
            ));
        }
        Err(e) => {
            return Err(Mismatch::of(
                "implementation_path",
                json!(identity.implementation_path),
                json!(format!("not resolved: {e}")),
            ));
        }
    };
    let open = |path: &Path| {
        rustix::fs::open(path, OFlags::RDONLY | OFlags::CLOEXEC, Mode::empty()).map(File::from)
    };
    let unreadable = |item, expected: &str, e: &dyn std::fmt::Display| {
        Mismatch::of(item, json!(expected), json!(format!("unreadable: {e}")))
    };
    let launcher_fd =
        open(launcher).map_err(|e| unreadable("launcher_sha256", &identity.launcher_sha256, &e))?;
    let implementation_fd = open(&implementation)
        .map_err(|e| unreadable("implementation_sha256", &identity.implementation_sha256, &e))?;
    let before = stamp_of(&implementation_fd).map_err(|e| unreadable("stamp", "a stamp", &e))?;
    let launcher_sha256 = sha256(&launcher_fd)
        .map_err(|e| unreadable("launcher_sha256", &identity.launcher_sha256, &e))?;
    let implementation_sha256 = sha256(&implementation_fd)
        .map_err(|e| unreadable("implementation_sha256", &identity.implementation_sha256, &e))?;
    between();
    let after = stamp_of(&implementation_fd).map_err(|e| unreadable("stamp", "a stamp", &e))?;
    let launcher_stamp = stamp_of(&launcher_fd).map_err(|e| unreadable("stamp", "a stamp", &e))?;
    let mut failed = Vec::new();
    // Changed while it was read: what was hashed is not what is there.
    if (before.dev, before.ino, before.ctime_sec, before.ctime_nsec)
        != (after.dev, after.ino, after.ctime_sec, after.ctime_nsec)
    {
        failed.push(Mismatch::of(
            "implementation_rewritten",
            json!(before),
            json!(after),
        ));
    }
    if launcher_sha256 != identity.launcher_sha256 {
        failed.push(Mismatch::of(
            "launcher_sha256",
            json!(identity.launcher_sha256),
            json!(launcher_sha256),
        ));
    }
    if implementation_sha256 != identity.implementation_sha256 {
        failed.push(Mismatch::of(
            "implementation_sha256",
            json!(identity.implementation_sha256),
            json!(implementation_sha256),
        ));
    }
    // The files hashed and about to be executed are the ones the fact's look stamped (M2T15D-201).
    let seen = Stamp {
        launcher: launcher_stamp,
        implementation: after,
    };
    if seen != expect.stamp {
        failed.push(Mismatch::of("stamp", json!(expect.stamp), json!(seen)));
    }
    let plan = probe::exec_plan(expect.agent, launcher, &implementation);
    if plan.exec != expect.plan {
        failed.push(Mismatch::of(
            "plan",
            json!(expect.plan.as_str()),
            json!(plan.exec.as_str()),
        ));
    }
    let stamp_only = failed.len() == 1 && failed[0].item == "stamp";
    match failed.into_iter().next() {
        Some(first) => Err(Mismatch {
            stamp_only,
            ..first
        }),
        None => Ok(Verified {
            fd: implementation_fd,
            plan,
            stamp: after,
        }),
    }
}

/// The stamp of the open file `file` (`fstat`).
fn stamp_of(file: &File) -> std::io::Result<FileStamp> {
    use std::os::unix::fs::MetadataExt as _;
    let m = file.metadata()?;
    Ok(FileStamp {
        dev: m.dev(),
        ino: m.ino(),
        ctime_sec: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

/// The sha256 of the open file `file`, read through it from where it is (just opened: its start) in fixed-size pieces,
/// as the probe hashes a file (`adapter::sha256_file`).
fn sha256(mut file: &File) -> std::io::Result<String> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    let mut hasher = Sha256::new();
    let mut piece = vec![0u8; 1 << 16];
    loop {
        match file.read(&mut piece) {
            Ok(0) => break,
            Ok(n) => hasher.update(&piece[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// What the gate executes for a verified implementation: the program, its whole argv (`argv[0]` first) and what it
/// adds to the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: PathBuf,
    pub argv: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// How `verified` is executed with `args` (docs/adr/ADR-030.md decision 9, "执行计划"). E1: the inode the gate hashed,
/// through its descriptor's `/proc/self/fd` link (the open file, not the path: a rename, deletion or new link of the path
/// since does not change what runs; the safe equivalent of `execveat(fd, "", AT_EMPTY_PATH)`, which rustix offers only
/// as `unsafe`), its path as `argv[0]`. E2: the interpreter its shebang (read through that descriptor) names, an
/// `env` one's program found on the agent's `PATH`, given the implementation's path to read. The plan's variables and
/// `PATH` prefix are added.
pub fn invocation(verified: &Verified, args: &[OsString]) -> std::io::Result<Invocation> {
    let plan = &verified.plan;
    let mut env = plan.env.clone();
    let path = match &plan.path_prefix {
        Some(prefix) => {
            let mut dirs = vec![prefix.clone()];
            dirs.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            let joined = std::env::join_paths(dirs).map_err(std::io::Error::other)?;
            env.push(("PATH".into(), joined.clone()));
            joined
        }
        None => std::env::var_os("PATH").unwrap_or_default(),
    };
    let (program, mut argv) = match plan.exec {
        Exec::Fd => {
            use std::os::fd::AsRawFd as _;
            (
                PathBuf::from(format!("/proc/self/fd/{}", verified.fd.as_raw_fd())),
                vec![plan.implementation.clone().into_os_string()],
            )
        }
        Exec::Path => {
            let (interpreter, arg) = shebang(&verified.fd)?;
            let named = arg
                .as_ref()
                .filter(|_| interpreter.file_name() == Some("env".as_ref()))
                .and_then(|a| a.to_str())
                .filter(|a| !a.is_empty() && !a.starts_with('-') && !a.contains([' ', '\t']));
            match named {
                Some(name) => {
                    let found = probe::find_on_path(name, &path)
                        .ok_or_else(|| std::io::Error::from(rustix::io::Errno::NOENT))?;
                    (found.clone(), vec![found.into_os_string()])
                }
                None => {
                    let mut argv = vec![interpreter.clone().into_os_string()];
                    argv.extend(arg);
                    (interpreter, argv)
                }
            }
        }
    };
    if plan.exec == Exec::Path {
        argv.push(plan.implementation.clone().into_os_string());
    }
    argv.extend(args.iter().cloned());
    Ok(Invocation { program, argv, env })
}

/// The interpreter and its one argument a script's `#!` line names, read through its descriptor as the kernel reads
/// it (its first 256 bytes; spaces and tabs around).
fn shebang(file: &File) -> std::io::Result<(PathBuf, Option<OsString>)> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::FileExt as _;
    let mut head = vec![0u8; 256];
    let n = file.read_at(&mut head, 0)?;
    head.truncate(n);
    let not_a_script = || std::io::Error::from(rustix::io::Errno::NOEXEC);
    let line = head.strip_prefix(b"#!").ok_or_else(not_a_script)?;
    let line = line.split(|b| *b == b'\n').next().unwrap_or_default();
    let blank = |b: &u8| *b == b' ' || *b == b'\t';
    let start = line
        .iter()
        .position(|b| !blank(b))
        .ok_or_else(not_a_script)?;
    let line = &line[start..];
    let end = line.iter().position(blank).unwrap_or(line.len());
    let interpreter = PathBuf::from(std::ffi::OsStr::from_bytes(&line[..end]));
    let rest = &line[end..];
    let arg = rest.iter().position(|b| !blank(b)).map(|from| {
        let to = rest
            .iter()
            .rposition(|b| !blank(b))
            .map_or(rest.len(), |at| at + 1);
        std::ffi::OsStr::from_bytes(&rest[from..to]).to_owned()
    });
    Ok((interpreter, arg))
}

/// Execute `verified` with `args` in the gate's place ([`invocation`]); returns only if that failed. Its descriptor is
/// open until then.
pub fn exec(verified: &Verified, args: &[OsString]) -> std::io::Error {
    let invocation = match invocation(verified, args) {
        Ok(invocation) => invocation,
        Err(e) => return e,
    };
    let mut command = std::process::Command::new(&invocation.program);
    if let Some((first, rest)) = invocation.argv.split_first() {
        command.arg0(first).args(rest);
    }
    command.envs(invocation.env);
    agent_environment(&mut command);
    command.exec()
}

/// An agent turn is not the enclosing development check. In particular, a fixture command run by the agent must
/// not find the parent's evidence sink or candidate identity in its environment. Inner Runtime checks assign their
/// own context when they execute the frozen plan; ordinary configuration and HOH_ATTEMPT_ID stay inherited.
fn agent_environment(command: &mut std::process::Command) {
    for name in [
        "HOH_EVIDENCE_DIR",
        "HOH_CHECK_ID",
        "HOH_CANDIDATE_ID",
        "HOH_SPEC_SHA256",
        "HOH_CANDIDATE_FILE",
        "HOH_CHECK_RECORD_ID",
        "HOH_LIVE_PROFILE",
        "HOH_LIVE_OUT",
    ] {
        command.env_remove(name);
    }
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

#[cfg(test)]
mod tests;
