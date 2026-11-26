//! AC-029-03: installed release, no external network, fake CTO, audited host and client.
use crate::{installed, record, text, world};
use installed::R;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const TEST: &str = "test_029_03_offline_instantiation_in_installed_release";

fn sealed() -> R<bool> {
    if std::env::var_os("HOH_TEMPLATE_OFFLINE").is_some() {
        // /sys retains its mounting namespace; /proc/net is the calling process's netns.
        let net = std::fs::read_to_string("/proc/net/dev")?;
        let interfaces: Vec<_> = net
            .lines()
            .filter_map(|l| l.split_once(':').map(|(name, _)| name.trim()))
            .collect();
        assert_eq!(
            interfaces,
            ["lo"],
            "offline fixture must have only loopback"
        );
        assert!(!rustix::process::geteuid().is_root());
        return Ok(true);
    }
    // As the fault-recorder precedent: only the namespace setup is privileged. The
    // installer, installed HoH and fake agent all run as the development user.
    let mut cmd = Command::new("sudo");
    cmd.args(["-n", "unshare", "-n", "--", "sh", "-c",
        "set -e; ip link set lo up; uid=$1; gid=$2; shift 2; exec setpriv --reuid=\"$uid\" --regid=\"$gid\" --clear-groups env -i \"$@\"", "sh"])
        .arg(rustix::process::getuid().as_raw().to_string()).arg(rustix::process::getgid().as_raw().to_string())
        .args(["PATH=/usr/bin:/bin", "LANG=C.UTF-8", "HOH_TEMPLATE_OFFLINE=1"]);
    for name in [
        "HOH_RELEASE_BUNDLE",
        "HOH_EXPECTED_COMMIT",
        "HOH_EVIDENCE_DIR",
        "TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(name) {
            let mut pair = std::ffi::OsString::from(format!("{name}="));
            pair.push(value);
            cmd.arg(pair);
        }
    }
    let out = cmd
        .arg(std::env::current_exe()?)
        .args(["--ignored", "--exact", TEST, "--nocapture"])
        .output()?;
    assert!(out.status.success(), "offline child: {}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 passed"),
        "offline child ran no test: {}",
        text(&out)
    );
    Ok(false)
}

fn command(world: &world::World, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.env_clear()
        .envs(world.env())
        .current_dir(world.project())
        .stdin(Stdio::null());
    // Tools may come from /usr/bin, but agents are always the fixture's launchers first.
    cmd.env(
        "PATH",
        format!(
            "{}:{}/.local/bin:/usr/bin:/bin",
            world.bin().display(),
            world.home().display()
        ),
    );
    for (key, path) in [
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CACHE_HOME", ".cache"),
    ] {
        cmd.env(key, world.home().join(path));
    }
    cmd
}

struct Host {
    pid: u32,
    start: Option<String>,
}
impl Host {
    fn stop(&self) {
        if self.start.is_some()
            && hoh_agents::identity::start_id(self.pid).ok().flatten() == self.start
            && let Some(pid) = rustix::process::Pid::from_raw(self.pid as i32)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_trace(mut child: Child) -> R {
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "host strace: {status}");
            return Ok(());
        }
        if Instant::now() > end {
            return Err("host strace did not finish after its owned host stopped".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

const TRACE_SYSCALLS: &str = "trace=socket,close,connect,dup,dup2,dup3,fcntl,close_range,clone,clone3,fork,vfork,unshare,execve,execveat";

const CALL_PREFIXES: &[&str] = &[
    "socket(",
    "close(",
    "connect(",
    "dup(",
    "dup2(",
    "dup3(",
    "fcntl(",
    "close_range(",
    "clone(",
    "clone3(",
    "fork(",
    "vfork(",
    "unshare(",
    "execve(",
    "execveat(",
    "???(",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FdState {
    cloexec: bool,
}

#[derive(Default)]
struct FdTables {
    next_id: usize,
    by_pid: BTreeMap<u32, usize>,
    tables: BTreeMap<usize, BTreeMap<i32, FdState>>,
}

impl FdTables {
    fn table_id(&mut self, pid: u32) -> usize {
        if let Some(&id) = self.by_pid.get(&pid) {
            return id;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.by_pid.insert(pid, id);
        self.tables.insert(id, BTreeMap::new());
        id
    }

    fn table_mut(&mut self, pid: u32) -> &mut BTreeMap<i32, FdState> {
        let id = self.table_id(pid);
        self.tables.entry(id).or_default()
    }

    fn get(&self, pid: u32, fd: i32) -> Option<FdState> {
        let id = self.by_pid.get(&pid)?;
        self.tables.get(id)?.get(&fd).copied()
    }

    fn share(&mut self, parent: u32, child: u32) {
        let parent_id = self.table_id(parent);
        let old = self.by_pid.insert(child, parent_id);
        if let Some(old_id) = old
            && old_id != parent_id
            && !self.by_pid.values().any(|&id| id == old_id)
        {
            self.tables.remove(&old_id);
        }
    }

    fn copy_table(&mut self, parent: u32, child: u32) {
        let cloned = self
            .by_pid
            .get(&parent)
            .and_then(|id| self.tables.get(id))
            .cloned()
            .unwrap_or_default();
        let new_id = self.next_id;
        self.next_id += 1;
        self.tables.insert(new_id, cloned);
        let old = self.by_pid.insert(child, new_id);
        if let Some(old_id) = old
            && old_id != new_id
            && !self.by_pid.values().any(|&id| id == old_id)
        {
            self.tables.remove(&old_id);
        }
    }

    fn unshare(&mut self, pid: u32) {
        let old_id = self.table_id(pid);
        if self.by_pid.values().filter(|&&id| id == old_id).count() > 1 {
            let cloned = self.tables.get(&old_id).cloned().unwrap_or_default();
            let new_id = self.next_id;
            self.next_id += 1;
            self.tables.insert(new_id, cloned);
            self.by_pid.insert(pid, new_id);
        }
    }

    fn on_exec(&mut self, pid: u32) {
        self.unshare(pid);
        self.table_mut(pid).retain(|_, state| !state.cloexec);
    }

    fn on_exit(&mut self, pid: u32) {
        if let Some(old_id) = self.by_pid.remove(&pid)
            && !self.by_pid.values().any(|&id| id == old_id)
        {
            self.tables.remove(&old_id);
        }
    }
}

fn parse_u32_bound(token: &str) -> Option<u32> {
    if matches!(token, "~0U" | "~0") {
        return Some(u32::MAX);
    }
    if let Some(hex) = token.strip_prefix("0x") {
        return u32::from_str_radix(hex, 16).ok();
    }
    token.parse::<u32>().ok()
}

fn has_ident(text: &str, ident: &str) -> bool {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|tok| tok == ident)
}

fn audit(trace: &str) -> R<usize> {
    let mut count = 0;
    let mut sockets = FdTables::default();
    let mut pending = BTreeMap::new();
    // Threads whose last call strace saw cut off by the thread's exit (pseudo result `= ?`): only that exit may follow.
    let mut exiting = BTreeSet::new();
    for line in trace.lines() {
        let (pid, body) = trace_line(line)?;
        if body.starts_with("+++ ") {
            // A call its thread did not return from, the exit printed without the call's pseudo result (FLK-15): checked
            // as that result `?` would be, on what strace decoded at its entry. Only with a thread id.
            if pid.is_some()
                && let Some(start) = pending.remove(&pid)
            {
                count +=
                    usize::from(check_call(&format!("{start}) = ?"), pid, line, &mut sockets)?.0);
            }
            if let Some(pid) = pid {
                sockets.on_exit(pid);
            }
            exiting.remove(&pid);
            continue;
        }
        if exiting.contains(&pid) {
            return Err(
                format!("syscall after a call cut off by its thread's exit: {line}").into(),
            );
        }
        if body.is_empty() || body.starts_with("--- ") {
            continue;
        }
        let call = if let Some(rest) = body.strip_prefix("<... ") {
            let (name, rest) = rest.split_once(" resumed>").ok_or("undecodable resume")?;
            let start: String = pending.remove(&pid).ok_or("resume without syscall start")?;
            if !start.starts_with(&format!("{name}(")) {
                return Err(format!("mismatched syscall resume: {line}").into());
            }
            format!("{start}{rest}")
        } else {
            if !CALL_PREFIXES.iter().any(|name| body.starts_with(name)) {
                return Err(format!("undecodable trace line: {line}").into());
            }
            if let Some(start) = body.strip_suffix("<unfinished ...>") {
                if pending.insert(pid, start.to_owned()).is_some() {
                    return Err(format!("overlapping syscalls: {line}").into());
                }
                continue;
            }
            if pending.contains_key(&pid) {
                return Err(format!("syscall before pending resume: {line}").into());
            }
            body.to_owned()
        };
        let (connect, result) = check_call(&call, pid, line, &mut sockets)?;
        count += usize::from(connect);
        if result.is_none() {
            let pid = pid.ok_or_else(|| format!("pseudo result without a thread id: {line}"))?;
            exiting.insert(Some(pid));
        }
    }
    if !pending.is_empty() {
        return Err("trace ended with an unfinished syscall".into());
    }
    if !exiting.is_empty() {
        return Err("trace ended before a thread with a cut-off call exited".into());
    }
    Ok(count)
}

/// Checks one whole call and applies it to the process's SOCK_DIAG fd table (M3O-800); gives whether it was a connect
/// and its result (`None` for strace's pseudo result `?`, a call whose thread exited before it returned).
fn check_call(
    call: &str,
    pid: Option<u32>,
    line: &str,
    sockets: &mut FdTables,
) -> R<(bool, Option<i64>)> {
    let invalid = || format!("non-local or undecodable syscall: {line}");
    let (name, rest) = call.split_once('(').ok_or_else(invalid)?;
    let (args, result) = rest.rsplit_once("= ").ok_or_else(invalid)?;
    let args = args
        .trim_end()
        .strip_suffix(')')
        .ok_or_else(invalid)?
        .trim_end();
    let token = result.split_whitespace().next().ok_or_else(invalid)?;
    let result = if token == "?" {
        None
    } else if let Some(hex) = token.strip_prefix("0x") {
        Some(i64::from_str_radix(hex, 16).map_err(|_| invalid())?)
    } else {
        Some(token.parse::<i64>().map_err(|_| invalid())?)
    };
    match name {
        "socket" => {
            let pid = pid.ok_or_else(invalid)?;
            let fields = args.split(", ").collect::<Vec<_>>();
            let [family, kind, protocol] = fields.as_slice() else {
                return Err(invalid().into());
            };
            // A socket whose result is unknown authorizes nothing.
            if let Some(fd) = result
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
            {
                let table = sockets.table_mut(pid);
                // Every successful socket replaces the previous identity of this fd.
                table.remove(&fd);
                let mut flags = kind.split('|');
                let primary = flags.next();
                let extra: Vec<_> = flags.collect();
                if *family == "AF_NETLINK"
                    && primary == Some("SOCK_RAW")
                    && extra
                        .iter()
                        .all(|v| matches!(*v, "SOCK_CLOEXEC" | "SOCK_NONBLOCK"))
                    && *protocol == "NETLINK_SOCK_DIAG"
                {
                    let cloexec = extra.contains(&"SOCK_CLOEXEC");
                    table.insert(fd, FdState { cloexec });
                }
            }
        }
        "close" => {
            let pid = pid.ok_or_else(invalid)?;
            let fd = args.parse::<i32>().map_err(|_| invalid())?;
            // Do not retain permission even on a failed/ambiguous close.
            sockets.table_mut(pid).remove(&fd);
        }
        "dup" => {
            let pid = pid.ok_or_else(invalid)?;
            let old = args.parse::<i32>().map_err(|_| invalid())?;
            if let Some(new_fd) = result
                .and_then(|v| i32::try_from(v).ok())
                .filter(|v| *v >= 0)
            {
                let inherited = sockets.get(pid, old).is_some();
                let table = sockets.table_mut(pid);
                if inherited {
                    table.insert(new_fd, FdState { cloexec: false });
                } else {
                    table.remove(&new_fd);
                }
            }
        }
        "dup2" => {
            let pid = pid.ok_or_else(invalid)?;
            let fields = args.split(", ").collect::<Vec<_>>();
            let [old, new] = fields.as_slice() else {
                return Err(invalid().into());
            };
            let old = old.parse::<i32>().map_err(|_| invalid())?;
            let new = new.parse::<i32>().map_err(|_| invalid())?;
            if old != new {
                if result.is_some_and(|v| v >= 0) {
                    let inherited = sockets.get(pid, old).is_some();
                    let table = sockets.table_mut(pid);
                    if inherited {
                        table.insert(new, FdState { cloexec: false });
                    } else {
                        table.remove(&new);
                    }
                } else {
                    // Fail closed on interrupted/ambiguous dup2 targeting an existing fd.
                    sockets.table_mut(pid).remove(&new);
                }
            }
        }
        "dup3" => {
            let pid = pid.ok_or_else(invalid)?;
            let fields = args.split(", ").collect::<Vec<_>>();
            let [old, new, flags] = fields.as_slice() else {
                return Err(invalid().into());
            };
            let old = old.parse::<i32>().map_err(|_| invalid())?;
            let new = new.parse::<i32>().map_err(|_| invalid())?;
            let valid_flags = *flags == "0" || flags.split('|').all(|f| f == "O_CLOEXEC");
            if !valid_flags {
                return Err(invalid().into());
            }
            let cloexec = flags.split('|').any(|f| f == "O_CLOEXEC");
            if result.is_some_and(|v| v >= 0) {
                let inherited = sockets.get(pid, old).is_some();
                let table = sockets.table_mut(pid);
                if inherited {
                    table.insert(new, FdState { cloexec });
                } else {
                    table.remove(&new);
                }
            } else if old != new {
                sockets.table_mut(pid).remove(&new);
            }
        }
        "fcntl" => {
            let pid = pid.ok_or_else(invalid)?;
            let mut parts = args.splitn(3, ", ");
            let fd = parts
                .next()
                .ok_or_else(invalid)?
                .parse::<i32>()
                .map_err(|_| invalid())?;
            let cmd = parts.next().ok_or_else(invalid)?;
            let arg = parts.next();
            match cmd {
                "F_DUPFD" | "F_DUPFD_CLOEXEC" => {
                    let _ = arg.ok_or_else(invalid)?;
                    let cloexec = cmd == "F_DUPFD_CLOEXEC";
                    if let Some(new_fd) = result
                        .and_then(|v| i32::try_from(v).ok())
                        .filter(|v| *v >= 0)
                    {
                        let inherited = sockets.get(pid, fd).is_some();
                        let table = sockets.table_mut(pid);
                        if inherited {
                            table.insert(new_fd, FdState { cloexec });
                        } else {
                            table.remove(&new_fd);
                        }
                    }
                }
                "F_SETFD" => {
                    let arg = arg.ok_or_else(invalid)?;
                    if sockets.get(pid, fd).is_some() {
                        let cloexec = if result == Some(0) {
                            arg != "0"
                        } else {
                            // Ambiguous or interrupted F_SETFD: fail safe toward closing on exec.
                            true
                        };
                        sockets.table_mut(pid).insert(fd, FdState { cloexec });
                    }
                }
                "F_GETFD" | "F_GETFL" | "F_SETFL" | "F_GETLK" | "F_SETLK" | "F_SETLKW"
                | "F_OFD_GETLK" | "F_OFD_SETLK" | "F_OFD_SETLKW" | "F_GETOWN" | "F_SETOWN"
                | "F_GETOWN_EX" | "F_SETOWN_EX" | "F_GETSIG" | "F_SETSIG" | "F_GETPIPE_SZ"
                | "F_SETPIPE_SZ" | "F_ADD_SEALS" | "F_GET_SEALS" | "F_GETLEASE" | "F_SETLEASE"
                | "F_NOTIFY" | "F_GET_RW_HINT" | "F_SET_RW_HINT" | "F_GET_FILE_RW_HINT"
                | "F_SET_FILE_RW_HINT" => {}
                _ => return Err(invalid().into()),
            }
        }
        "close_range" => {
            let pid = pid.ok_or_else(invalid)?;
            let fields = args.split(", ").collect::<Vec<_>>();
            let [first, last, flags] = fields.as_slice() else {
                return Err(invalid().into());
            };
            let first = parse_u32_bound(first).ok_or_else(invalid)?;
            let last = parse_u32_bound(last).ok_or_else(invalid)?;
            let valid_flags = *flags == "0"
                || flags
                    .split('|')
                    .all(|f| matches!(f, "CLOSE_RANGE_UNSHARE" | "CLOSE_RANGE_CLOEXEC"));
            if !valid_flags {
                return Err(invalid().into());
            }
            let unshare = flags.split('|').any(|f| f == "CLOSE_RANGE_UNSHARE");
            let set_cloexec = flags.split('|').any(|f| f == "CLOSE_RANGE_CLOEXEC");
            if unshare && result == Some(0) {
                sockets.unshare(pid);
            }
            let in_range = |fd: i32| u32::try_from(fd).is_ok_and(|u| u >= first && u <= last);
            if set_cloexec && result == Some(0) {
                for (&fd, state) in sockets.table_mut(pid).iter_mut() {
                    if in_range(fd) {
                        state.cloexec = true;
                    }
                }
            } else if !set_cloexec || result.is_none() {
                sockets.table_mut(pid).retain(|&fd, _| !in_range(fd));
            }
        }
        "clone" | "clone3" => {
            if let (Some(parent), Some(child)) = (
                pid,
                result
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|&v| v > 0),
            ) {
                if has_ident(args, "CLONE_FILES") {
                    sockets.share(parent, child);
                } else {
                    sockets.copy_table(parent, child);
                }
            }
        }
        "fork" | "vfork" => {
            if let (Some(parent), Some(child)) = (
                pid,
                result
                    .and_then(|v| u32::try_from(v).ok())
                    .filter(|&v| v > 0),
            ) {
                sockets.copy_table(parent, child);
            }
        }
        "unshare" => {
            if result == Some(0)
                && let Some(pid) = pid
                && has_ident(args, "CLONE_FILES")
            {
                sockets.unshare(pid);
            }
        }
        "execve" | "execveat" => {
            if result == Some(0)
                && let Some(pid) = pid
            {
                sockets.on_exec(pid);
            }
        }
        "connect" => {
            let (fd, address) = args.split_once(", ").ok_or_else(invalid)?;
            let fd = fd.parse::<i32>().map_err(|_| invalid())?;
            let (address, length) = address.rsplit_once(", ").ok_or_else(invalid)?;
            let _ = length.parse::<u32>().map_err(|_| invalid())?;
            let address = address
                .strip_prefix('{')
                .and_then(|v| v.strip_suffix('}'))
                .ok_or_else(invalid)?;
            let local = address.starts_with("sa_family=AF_UNIX,")
                || (address.starts_with("sa_family=AF_INET,")
                    && address.contains("inet_addr(\"127."))
                || (address.starts_with("sa_family=AF_INET6,")
                    && address.contains("inet_pton(AF_INET6, \"::1\""));
            let kernel = address
                .strip_prefix("sa_family=AF_NETLINK, nl_pid=0, nl_groups=")
                .is_some_and(|groups| {
                    groups.len() == 8 && groups.bytes().all(|b| b.is_ascii_hexdigit())
                })
                && pid.is_some_and(|pid| sockets.get(pid, fd).is_some());
            if !local && !kernel {
                return Err(invalid().into());
            }
            return Ok((true, result));
        }
        // strace prints `???(` when PTRACE_GET_SYSCALL_INFO fails at a syscall-entry stop. That fails only when the
        // thread has a fatal signal pending (ptrace_freeze_traced), and with one pending the kernel aborts the call
        // (ptrace_report_syscall_entry), so a `???` call never ran. It has no arguments and no result of its own.
        "???" if args.is_empty() && result.is_none() => {}
        _ => return Err(invalid().into()),
    }
    Ok((false, result))
}

fn trace_line(line: &str) -> R<(Option<u32>, &str)> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("[pid ") {
        let (pid, body) = rest.split_once(']').ok_or("undecodable trace pid")?;
        return Ok((Some(pid.trim().parse()?), body.trim_start()));
    }
    if line.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        let (pid, body) = line
            .split_once(char::is_whitespace)
            .ok_or("trace pid without syscall")?;
        return Ok((Some(pid.parse()?), body.trim_start()));
    }
    // Unprefixed local connects remain readable, but cannot authorize a netlink fd.
    Ok((None, line))
}

pub fn offline_instantiation() -> R {
    if !sealed()? {
        return Ok(());
    }
    let bundle = std::env::var_os("HOH_RELEASE_BUNDLE").ok_or("HOH_RELEASE_BUNDLE is required")?;
    let expected_commit = std::env::var("HOH_EXPECTED_COMMIT")?;
    let world = world::World::named("p")?;
    world.configure(&[("cto", "claude")], "")?;
    let expected = hoh_core::templates::load(
        &world.project(),
        None,
        "fixture",
        &hoh_core::resource_bundle::Bundle::current(),
    )?
    .render(
        "full",
        &hoh_core::templates::Inputs {
            goal: "离线模板起草".into(),
            owners: "cto".into(),
        },
    )?;
    let source = hoh_core::specindex::Source {
        path: "docs/spec/spec.md".into(),
        bytes: expected["docs/spec/spec.md"].as_bytes().to_vec(),
    };
    let index = hoh_core::specindex::generate_index(&[source])?;
    std::fs::write(world.fakes().join("draft-index.json"), index.bytes())?;
    let agent = world.fakes().join("agent.py");
    let script = std::fs::read_to_string(&agent)?.replace("elif role == 'developer':", "elif role == 'cto':\n        final = template_drafter(ident)\n    elif role == 'developer':");
    let draft = r#"
def template_drafter(ident):
    import pathlib
    before = {str(p): p.read_text() for p in pathlib.Path('docs').rglob('*.md')}
    append('templates-at-start.jsonl', before)
    pathlib.Path('docs/spec/index.json').write_bytes(pathlib.Path(WORLD, 'draft-index.json').read_bytes())
    git = ['git', '-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid']
    subprocess.run(git + ['add', 'docs/spec'], check=True, stdout=subprocess.DEVNULL)
    subprocess.run(git + ['commit', '-qm', 'draft'], check=True, stdout=subprocess.DEVNULL)
    task_result(ident, 'COMPLETED', 'scripted offline draft', 'approval')
    return '规格已提交'

"#;
    std::fs::write(
        &agent,
        script.replace("def turn(prompt):", &format!("{draft}\ndef turn(prompt):")),
    )?;
    world.register()?;
    let unpacked = world.root().join("unpacked");
    std::fs::create_dir(&unpacked)?;
    assert!(
        Command::new("tar")
            .arg("-xzf")
            .arg(&bundle)
            .arg("-C")
            .arg(&unpacked)
            .status()?
            .success()
    );
    let out = command(&world, "bash")
        .arg(unpacked.join("hoh/install.sh"))
        .output()?;
    assert!(out.status.success(), "{}", text(&out));
    let hoh = world.home().join(".local/bin/hoh");
    let version = command(&world, &hoh).arg("--version").output()?;
    assert!(
        version.status.success()
            && String::from_utf8_lossy(&version.stdout).contains(&expected_commit),
        "{}",
        text(&version)
    );
    assert!(!world.project().join(".hoh/host.json").exists());
    let doctor = command(&world, &hoh).arg("doctor").output()?;
    assert!(
        [Some(0), Some(3)].contains(&doctor.status.code()),
        "{}",
        text(&doctor)
    );
    assert!(
        String::from_utf8_lossy(&doctor.stdout).contains(&format!(
            "模板包版本：{}",
            hoh_core::objects::resource_bundle_version()
        )),
        "{}",
        text(&doctor)
    );
    let out = command(&world, &hoh)
        .args(["-p", "离线模板起草", "--output", "json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let first = world::finish(out, Duration::from_secs(60))?;
    assert_eq!(first.status.code(), Some(3), "{}", text(&first));
    let run: Value = serde_json::from_slice(&first.stdout)?;
    let run = run["run_id"].as_str().ok_or("no run id")?;
    let record_host: Value =
        serde_json::from_slice(&std::fs::read(world.project().join(".hoh/host.json"))?)?;
    let pid = record_host["pid"].as_u64().ok_or("no host pid")? as u32;
    let host = Host {
        pid,
        start: hoh_agents::identity::start_id(pid)?,
    };
    let host_trace = world.root().join("host.strace");
    let client_trace = world.root().join("client.strace");
    std::fs::File::create(&host_trace)?;
    let trace_err = world.root().join("trace.stderr");
    let tracer = Command::new("sudo")
        .args([
            "-n",
            "strace",
            "-f",
            "-e",
            TRACE_SYSCALLS,
            "-s",
            "4096",
            "-o",
        ])
        .arg(&host_trace)
        .arg("-p")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&trace_err)?)
        .spawn()?;
    let result: R<Value> = (|| {
        // Wait for attachment, so no preparation or child can race the start of the audit.
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let threads =
                std::fs::read_dir(format!("/proc/{pid}/task"))?.collect::<Result<Vec<_>, _>>()?;
            let all_traced = !threads.is_empty()
                && threads.iter().all(|thread| {
                    std::fs::read_to_string(thread.path().join("status")).is_ok_and(|status| {
                        status.lines().any(|l| {
                            l.strip_prefix("TracerPid:")
                                .is_some_and(|p| p.trim() != "0")
                        })
                    })
                });
            if all_traced {
                break;
            }
            if Instant::now() > end {
                return Err(format!(
                    "strace did not attach: {}",
                    std::fs::read_to_string(&trace_err)?
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let child = command(&world, "strace")
            .args(["-f", "-e", TRACE_SYSCALLS, "-s", "4096", "-o"])
            .arg(&client_trace)
            .arg(&hoh)
            .args(["-p", "draft", "--resume", run, "--output", "json"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let drafted = world::finish(child, Duration::from_secs(90))?;
        assert_eq!(drafted.status.code(), Some(3), "{}", text(&drafted));
        assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='question.asked' AND json_extract(payload_json,'$.reason')='spec_approval'")?, "1", "{}", text(&drafted));
        assert_eq!(
            world.query("SELECT count(*) FROM events WHERE kind='templates.instantiated'")?,
            "1"
        );
        let observed = world.log("templates-at-start.jsonl")?;
        assert_eq!(
            observed,
            [serde_json::to_value(&expected)?],
            "every profile template must exist before the fake CTO runs"
        );
        for (path, content) in &expected {
            assert_eq!(
                std::fs::read_to_string(world.project().join(path))?,
                *content
            );
            assert!(!content.contains("{{") && !content.contains("}}"));
            if path != "docs/spec/spec.md" {
                hoh_core::templates::validate(path, content)?;
            }
        }
        Ok(
            json!({"doctor":record(&doctor),"first":record(&first),"draft":record(&drafted),"files":expected.keys().collect::<Vec<_>>(),"at_start":observed,"host":record_host}),
        )
    })();
    host.stop();
    wait_trace(tracer)?;
    let host_log = std::fs::read_to_string(&host_trace)?;
    let client_log = std::fs::read_to_string(&client_trace)?;
    // Keep raw observations even when an audit assertion fails.
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let dir = Path::new(&dir);
        std::fs::copy(&host_trace, dir.join("templates-host.strace"))?;
        std::fs::copy(&client_trace, dir.join("templates-client.strace"))?;
        std::fs::write(
            dir.join("templates-before-audit.json"),
            serde_json::to_vec_pretty(
                &json!({"observed":result.as_ref().ok(),"error":result.as_ref().err().map(|e|e.to_string())}),
            )?,
        )?;
    }
    let host_connects = audit(&host_log)?;
    let client_connects = audit(&client_log)?;
    assert!(
        host_log.contains("execve(") && host_log.contains("/bin/claude"),
        "host trace must include the fake CTO child: {host_log}"
    );
    assert!(
        client_connects > 0,
        "client trace must contain its actual connection to the host"
    );
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        let dir = Path::new(&dir);
        std::fs::copy(&host_trace, dir.join("templates-host.strace"))?;
        std::fs::copy(&client_trace, dir.join("templates-client.strace"))?;
        std::fs::write(
            dir.join("templates-offline.json"),
            serde_json::to_vec_pretty(
                &json!({"observed":result.as_ref().ok(),"host_connects":host_connects,"client_connects":client_connects,"network_interfaces":["lo"]}),
            )?,
        )?;
    }
    result?;
    Ok(())
}

#[test]
fn connect_audit_accepts_correlated_kernel_sock_diag() -> R {
    let trace = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG) = 7\n\
                 101 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    assert_eq!(audit(trace)?, 1);
    Ok(())
}

#[test]
fn connect_audit_accepts_aligned_returns_and_errno_descriptions() -> R {
    // strace's default return-column alignment, observed in the installed-release trace.
    let trace = "101 close(28)                       = 0\n\
                 101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG) = 28\n\
                 101 connect(28, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n\
                 101 close(28)                       = -1 EINTR (Interrupted system call)\n";
    assert_eq!(audit(trace)?, 1);
    Ok(())
}

#[test]
fn connect_audit_uses_the_latest_socket_for_a_reused_fd() -> R {
    for history in [
        "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE) = 7\n101 close(7) = 0\n",
        "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG) = 7\n101 close(7) = 0\n",
        "102 socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE) = 7\n102 close(7) = 0\n",
    ] {
        for flags in ["SOCK_RAW", "SOCK_RAW|SOCK_NONBLOCK|SOCK_CLOEXEC"] {
            let trace = format!(
                "{history}101 socket(AF_NETLINK, {flags}, NETLINK_SOCK_DIAG) = 7\n\
                 101 connect(7, {{sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}}, 12) = 0\n"
            );
            assert_eq!(audit(&trace)?, 1, "{trace}");
        }
    }
    Ok(())
}

#[test]
fn connect_audit_rejects_unqualified_netlink_sockets_and_targets() {
    let socket = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG) = 7\n";
    let connect = "101 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    for trace in [
        connect.to_owned(),
        format!("{connect}{socket}"),
        format!(
            "{}{connect}",
            socket.replace("NETLINK_SOCK_DIAG", "NETLINK_ROUTE")
        ),
        format!(
            "{}{connect}",
            socket.replace("NETLINK_SOCK_DIAG", "NETLINK_USERSOCK")
        ),
        format!("{}{connect}", socket.replace("SOCK_RAW", "SOCK_DGRAM")),
        format!("{}{connect}", socket.replace("SOCK_RAW", "SOCK_STREAM")),
        format!("{}{connect}", socket.replace("AF_NETLINK", "AF_UNIX")),
        format!("{}{connect}", socket.replace("SOCK_CLOEXEC", "0x40000000")),
        format!(
            "{}{connect}",
            socket.replace("= 7", "= -1 EMFILE (Too many open files)")
        ),
        format!("{}{connect}", socket.replace("= 7", "= 8")),
        format!("{}{connect}", socket.replace("101 ", "102 ")),
        format!("{socket}{}", connect.replace("nl_pid=0,", "nl_pid=10,")),
        format!("{socket}{}", connect.replace("nl_pid=0,", "nl_pid=-1,")),
        format!("{socket}{}", connect.replace("101 ", "102 ")),
        format!("{socket}{}", connect.replace("101 ", "")),
        format!(
            "{socket}{}",
            connect.replace("nl_groups=00000000", "nl_groups=???")
        ),
        format!(
            "{socket}{}",
            connect.replace(
                "{sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}",
                "0x1234"
            )
        ),
        format!("{socket}{}", connect.replace("= 0", "= ?")),
        format!("{socket}101 close(7) = 0\n{connect}"),
        format!("{socket}101 close(7) = -1 EINTR (Interrupted system call)\n{connect}"),
        format!(
            "{socket}101 close(7) = 0\n{}{connect}",
            socket.replace("NETLINK_SOCK_DIAG", "NETLINK_ROUTE")
        ),
        // A newer socket result supersedes the old fd even if close was not observed.
        format!(
            "{socket}{}{connect}",
            socket.replace("NETLINK_SOCK_DIAG", "NETLINK_ROUTE")
        ),
    ] {
        assert!(audit(&trace).is_err(), "unexpectedly allowed: {trace}");
    }
}

#[test]
fn connect_audit_correlates_interleaved_unfinished_syscalls() -> R {
    let trace = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG <unfinished ...>\n\
                 102 socket(AF_NETLINK, SOCK_RAW, NETLINK_ROUTE) = 7\n\
                 101 <... socket resumed>) = 7\n\
                 101 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12 <unfinished ...>\n\
                 102 close(7) = 0\n\
                 101 <... connect resumed>) = 0\n\
                 101 close(7 <unfinished ...>\n\
                 101 <... close resumed>) = 0\n";
    assert_eq!(audit(trace)?, 1);
    assert_eq!(audit(&trace.replace("101 ", "[pid 101] "))?, 1);
    for invalid in [
        "101 connect(7,  <unfinished ...>",
        "101 <... connect resumed>) = 0",
        "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG <unfinished ...>\n101 <... close resumed>) = 7",
        "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG <unfinished ...>\n102 <... socket resumed>) = 7",
        "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG <unfinished ...>\n101 close(7) = 0",
    ] {
        assert!(audit(invalid).is_err(), "{invalid}");
    }
    Ok(())
}

#[test]
fn connect_audit_rejects_an_undecodable_syscall_line() {
    assert!(audit("unreadable connect(7, 0x1234, 12) = -1").is_err());
}

#[test]
fn connect_audit_rejects_external_and_undecodable_targets() -> R {
    for line in [
        "connect(3, {sa_family=AF_UNIX, sun_path=\"/a\"}, 12) = 0",
        "connect(3, {sa_family=AF_INET, sin_addr=inet_addr(\"127.2.3.4\")}, 16) = 0",
        "connect(3, {sa_family=AF_INET6, inet_pton(AF_INET6, \"::1\", &a)}, 28) = 0",
    ] {
        assert_eq!(audit(line)?, 1);
    }
    for line in [
        "connect(3, {sa_family=AF_INET, sin_addr=inet_addr(\"1.1.1.1\")}, 16) = -1",
        "connect(3, {sa_family=AF_INET6, inet_pton(AF_INET6, \"::ffff:1.1.1.1\", &a)}, 28) = -1",
        "connect(3, 0x1234, 16) = -1",
    ] {
        assert!(audit(line).is_err());
    }
    Ok(())
}

#[test]
fn connect_audit_accepts_a_syscall_cut_off_by_its_threads_exit() -> R {
    // FLK-15: strace prints `???(` when PTRACE_GET_SYSCALL_INFO fails at a syscall-entry stop, which happens only once
    // a fatal signal is pending; the kernel then aborts that syscall. `= ?` is strace's pseudo result for a call whose
    // thread exited before the call returned. The lines are verbatim from CI (8388) and from a local reproduction.
    let ci = "8388  ???( <unfinished ...>\n\
              8389  close(5)                    = 0\n\
              8388  <... ??? resumed>)              = ?\n\
              8388  +++ exited with 0 +++\n";
    assert_eq!(audit(ci)?, 0);
    assert_eq!(audit(&ci.replace("8388  ", "[pid  8388] "))?, 0);
    let joined = "2588809 ???()                           = ?\n2588809 +++ exited with 0 +++\n";
    assert_eq!(audit(joined)?, 0);
    // A thread killed without an exit stop: strace prints no pseudo result.
    assert_eq!(
        audit("8388  ???( <unfinished ...>\n8388  +++ killed by SIGKILL +++\n")?,
        0
    );
    let named = "2588810 socket(AF_INET, SOCK_STREAM, IPPROTO_IP <unfinished ...>\n\
                 2588807 close(4 <unfinished ...>\n\
                 2588810 <... socket resumed>)           = ?\n\
                 2588807 <... close resumed>)            = ? <unavailable>\n\
                 2588810 +++ exited with 0 +++\n\
                 2588807 +++ exited with 0 +++\n";
    assert_eq!(audit(named)?, 0);
    // Calls whose thread exited, its exit printed without their pseudo result (local reproduction, t.48).
    let unresumed = "2589424 socket(AF_INET, SOCK_STREAM, IPPROTO_IP <unfinished ...>\n\
                     2589422 close(3 <unfinished ...>\n\
                     2589424 +++ exited with 0 +++\n\
                     2589422 +++ exited with 0 +++\n";
    assert_eq!(audit(unresumed)?, 0);
    // A connect cut off by its thread's exit is still checked against its decoded target, with or without `= ?`.
    assert_eq!(
        audit(
            "101 connect(3, {sa_family=AF_UNIX, sun_path=\"/a\"}, 12 <unfinished ...>\n101 +++ exited with 0 +++\n"
        )?,
        1
    );
    let local =
        "101 connect(3, {sa_family=AF_UNIX, sun_path=\"/a\"}, 12) = ?\n101 +++ exited with 0 +++\n";
    assert_eq!(audit(local)?, 1);
    Ok(())
}

#[test]
fn connect_audit_rejects_a_cut_off_syscall_whose_thread_did_not_exit() {
    let killed = "101 +++ killed by SIGKILL +++\n";
    let diag = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG) = 7\n";
    let netlink = "101 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    for trace in [
        // The thread went on, so strace's view of the call was not cut off by its exit.
        "101 ???( <unfinished ...>\n".to_owned(),
        "101 ???( <unfinished ...>\n101 <... ??? resumed>) = ?\n".to_owned(),
        "101 ???( <unfinished ...>\n101 <... ??? resumed>) = ?\n101 close(3) = 0\n".to_owned(),
        "101 ???( <unfinished ...>\n101 close(3) = 0\n".to_owned(),
        "101 ???()                           = ?\n101 close(3) = 0\n".to_owned(),
        "101 socket(AF_INET, SOCK_STREAM, IPPROTO_IP) = ?\n".to_owned(),
        "101 socket(AF_INET, SOCK_STREAM, IPPROTO_IP) = ?\n101 close(3) = 0\n".to_owned(),
        // A call that returned a result was decodable, so `???` cannot stand for it.
        format!("101 ???( <unfinished ...>\n101 <... ??? resumed>) = 0\n{killed}"),
        format!("101 ???()                           = 3\n{killed}"),
        format!("101 ???(7, 0x1234, 12)              = ?\n{killed}"),
        format!("101 ???( <unfinished ...>\n101 <... ??? resumed>7, 0x1234, 12) = ?\n{killed}"),
        // Without a thread id the exit cannot be tied to the call.
        "???( <unfinished ...>\n<... ??? resumed>) = ?\n+++ exited with 0 +++\n".to_owned(),
        "???()                           = ?\n+++ exited with 0 +++\n".to_owned(),
        // A cut-off external connect is still refused, unfinished at its thread's exit too, as is one whose fd only a
        // cut-off socket opened.
        format!(
            "101 connect(3, {{sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr(\"1.1.1.1\")}}, 16 <unfinished ...>\n{killed}"
        ),
        "101 connect(3, {sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr(\"1.1.1.1\")}, 16 <unfinished ...>\n\
         102 close(4) = 0\n101 +++ exited with 0 +++\n"
            .to_owned(),
        format!(
            "{}{killed}",
            netlink.replace(") = 0", " <unfinished ...>")
        ),
        // An unfinished call without a thread id is not closed by an exit.
        "connect(3, {sa_family=AF_UNIX, sun_path=\"/a\"}, 12 <unfinished ...>\n+++ exited with 0 +++\n".to_owned(),
        format!(
            "101 connect(3, {{sa_family=AF_INET, sin_port=htons(443), sin_addr=inet_addr(\"1.1.1.1\")}}, 16) = ?\n{killed}"
        ),
        format!("{}{killed}", netlink.replace("= 0", "= ?")),
        format!(
            "{}{}{killed}",
            diag.replace("= 7", "= ?"),
            netlink.replace("= 0", "= ?")
        ),
    ] {
        assert!(audit(&trace).is_err(), "unexpectedly allowed: {trace}");
    }
    // The same unfinished netlink connect on an owned SOCK_DIAG socket passes: the vector above fails on its
    // authorization, not on its syntax (M3F-100).
    let owned = format!(
        "{diag}{}{killed}",
        netlink.replace(") = 0", " <unfinished ...>")
    );
    assert_eq!(audit(&owned).ok(), Some(1), "refused: {owned}");
}

#[test]
fn connect_audit_follows_fd_duplication_close_range_exec_and_clone_files() -> R {
    let diag_cloexec = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_SOCK_DIAG) = 7\n";
    let diag_plain = "101 socket(AF_NETLINK, SOCK_RAW, NETLINK_SOCK_DIAG) = 7\n";
    let route = "101 socket(AF_NETLINK, SOCK_RAW|SOCK_CLOEXEC, NETLINK_ROUTE) = 8\n";
    let connect_7 =
        "101 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    let connect_9 =
        "101 connect(9, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    let connect_9_nonkernel =
        "101 connect(9, {sa_family=AF_NETLINK, nl_pid=10, nl_groups=00000000}, 12) = 0\n";
    let exec_ok = "101 execve(\"/bin/true\", [\"true\"], 0x7fff /* 1 vars */) = 0\n";
    let exec_fail = "101 execve(\"/no/such\", [\"true\"], 0x7fff /* 1 vars */) = -1 ENOENT (No such file or directory)\n";
    let execat_ok = "101 execveat(AT_FDCWD, \"/bin/true\", [\"true\"], 0x7fff, 0) = 0\n";
    let clone_files = "101 clone3({flags=CLONE_VM|CLONE_FS|CLONE_FILES|CLONE_SIGHAND|CLONE_THREAD, stack=0x0}, 88) = 102\n";
    let fork_ok = "101 fork() = 102\n";
    let vfork_ok = "101 vfork() = 102\n";
    let clone_no_files = "101 clone(child_stack=NULL, flags=CLONE_CHILD_CLEARTID|CLONE_CHILD_SETTID|SIGCHLD) = 102\n";
    let clone3_no_files = "101 clone3({flags=0, exit_signal=SIGCHLD}, 88) = 102\n";
    let child_connect_7 =
        "102 connect(7, {sa_family=AF_NETLINK, nl_pid=0, nl_groups=00000000}, 12) = 0\n";
    let child_exec_ok = "102 execve(\"/bin/true\", [\"true\"], 0x7fff /* 1 vars */) = 0\n";

    // Legitimate dups of a NETLINK_SOCK_DIAG socket remain admitted for nl_pid=0 only.
    for trace in [
        format!("{diag_cloexec}101 dup(7) = 9\n101 close(7) = 0\n{connect_9}"),
        format!("{diag_cloexec}101 dup2(7, 9) = 9\n101 close(7) = 0\n{connect_9}"),
        format!("{diag_cloexec}101 dup3(7, 9, O_CLOEXEC) = 9\n101 close(7) = 0\n{connect_9}"),
        format!("{diag_cloexec}101 fcntl(7, F_DUPFD, 9) = 9\n101 close(7) = 0\n{connect_9}"),
        format!(
            "{diag_cloexec}101 fcntl(7, F_DUPFD_CLOEXEC, 9) = 9\n101 close(7) = 0\n{connect_9}"
        ),
        // Failed execve does not close CLOEXEC sockets; non-CLOEXEC sockets survive execve.
        format!("{diag_cloexec}{exec_fail}{connect_7}"),
        format!("{diag_plain}{exec_ok}{connect_7}"),
        // dup2 clears FD_CLOEXEC on the new fd, so fd 9 survives execve while fd 7 closes.
        format!("{diag_cloexec}101 dup2(7, 9) = 9\n{exec_ok}{connect_9}"),
        // Benign fcntl metadata calls with hex returns are decoded cleanly.
        format!("{diag_cloexec}101 fcntl(7, F_GETFD) = 0x1 (flags FD_CLOEXEC)\n{connect_7}"),
        // Sibling thread sharing fd table via CLONE_FILES may connect on the process's SOCK_DIAG socket.
        format!("{clone_files}{diag_cloexec}{child_connect_7}"),
        // M4L3-102: fork, vfork, and clone/clone3 without CLONE_FILES copy the parent's fd table, so a child's
        // legal nl_pid=0 connect on an inherited SOCK_DIAG fd is admitted.
        format!("{diag_cloexec}{fork_ok}{child_connect_7}"),
        format!("{diag_cloexec}{vfork_ok}{child_connect_7}"),
        format!("{diag_cloexec}{clone_no_files}{child_connect_7}"),
        format!("{diag_cloexec}{clone3_no_files}{child_connect_7}"),
        // Copied fd table is independent: child closing fd 7 leaves parent's fd 7 intact, and non-CLOEXEC survives child execve.
        format!("{diag_cloexec}{fork_ok}102 close(7) = 0\n{connect_7}"),
        format!("{diag_plain}{fork_ok}{child_exec_ok}{child_connect_7}"),
    ] {
        assert_eq!(audit(&trace)?, 1, "unexpectedly refused: {trace}");
    }

    // Overwriting, closing, exec-closing, or non-kernel connects on dup'd fds MUST be rejected.
    for trace in [
        // dup2 / dup3 overwriting a SOCK_DIAG fd with a NETLINK_ROUTE socket must revoke SOCK_DIAG.
        format!("{diag_cloexec}{route}101 dup2(8, 7) = 7\n{connect_7}"),
        format!("{diag_cloexec}{route}101 dup3(8, 7, O_CLOEXEC) = 7\n{connect_7}"),
        // Duplicating a non-SOCK_DIAG socket never authorizes connect.
        format!("{route}101 dup(8) = 9\n{connect_9}"),
        format!("{route}101 dup2(8, 9) = 9\n{connect_9}"),
        format!("{route}101 dup3(8, 9, 0) = 9\n{connect_9}"),
        format!("{route}101 fcntl(8, F_DUPFD, 9) = 9\n{connect_9}"),
        format!("{route}101 fcntl(8, F_DUPFD_CLOEXEC, 9) = 9\n{connect_9}"),
        // Non-kernel nl_pid on a dup'd SOCK_DIAG socket is still rejected.
        format!("{diag_cloexec}101 dup(7) = 9\n{connect_9_nonkernel}"),
        // close_range revokes SOCK_DIAG fds in range (both numeric and ~0U bounds).
        format!("{diag_cloexec}101 close_range(3, 10, 0) = 0\n{connect_7}"),
        format!("{diag_cloexec}101 close_range(3, ~0U, 0) = 0\n{connect_7}"),
        // Exec-time closes: SOCK_CLOEXEC, dup3 O_CLOEXEC, F_DUPFD_CLOEXEC, F_SETFD FD_CLOEXEC, CLOSE_RANGE_CLOEXEC.
        format!("{diag_cloexec}{exec_ok}{connect_7}"),
        format!("{diag_cloexec}{execat_ok}{connect_7}"),
        format!("{diag_plain}101 dup3(7, 9, O_CLOEXEC) = 9\n{exec_ok}{connect_9}"),
        format!("{diag_plain}101 fcntl(7, F_DUPFD_CLOEXEC, 9) = 9\n{exec_ok}{connect_9}"),
        format!("{diag_plain}101 fcntl(7, F_SETFD, FD_CLOEXEC) = 0\n{exec_ok}{connect_7}"),
        format!("{diag_plain}101 close_range(7, 7, CLOSE_RANGE_CLOEXEC) = 0\n{exec_ok}{connect_7}"),
        // CLONE_FILES sibling closing or overwriting fd 7 revokes it for the creator thread too.
        format!("{clone_files}{diag_cloexec}102 close(7) = 0\n{connect_7}"),
        format!("{clone_files}{diag_cloexec}{route}102 dup2(8, 7) = 7\n{connect_7}"),
        format!("{clone_files}{diag_cloexec}102 close_range(7, 7, 0) = 0\n{connect_7}"),
        // M4L3-102: without CLONE_FILES, a socket opened AFTER fork is not visible to the child, and child execve
        // closes inherited SOCK_CLOEXEC fds.
        format!("{fork_ok}{diag_cloexec}{child_connect_7}"),
        format!("{clone_no_files}{diag_cloexec}{child_connect_7}"),
        format!("{diag_cloexec}{fork_ok}{child_exec_ok}{child_connect_7}"),
    ] {
        assert!(audit(&trace).is_err(), "unexpectedly allowed: {trace}");
    }
    Ok(())
}
