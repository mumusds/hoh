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

fn audit(trace: &str) -> R<usize> {
    let mut count = 0;
    let mut sockets = BTreeSet::new();
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
            sockets.retain(|(owner, _)| Some(*owner) != pid);
            exiting.remove(&pid);
            continue;
        }
        if exiting.contains(&pid) {
            return Err(
                format!("syscall after a call cut off by its thread's exit: {line}").into(),
            );
        }
        if body.is_empty() || body.starts_with("--- ") || body.starts_with("execve(") {
            continue;
        }
        let call = if let Some(rest) = body.strip_prefix("<... ") {
            let (name, rest) = rest.split_once(" resumed>").ok_or("undecodable resume")?;
            if name == "execve" {
                continue;
            }
            let start: String = pending.remove(&pid).ok_or("resume without syscall start")?;
            if !start.starts_with(&format!("{name}(")) {
                return Err(format!("mismatched syscall resume: {line}").into());
            }
            format!("{start}{rest}")
        } else {
            if !["socket(", "close(", "connect(", "???("]
                .iter()
                .any(|name| body.starts_with(name))
            {
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

/// Checks one whole call and applies it to the thread's SOCK_DIAG sockets; gives whether it was a connect and its result
/// (`None` for strace's pseudo result `?`, a call whose thread exited before it returned).
fn check_call(
    call: &str,
    pid: Option<u32>,
    line: &str,
    sockets: &mut BTreeSet<(u32, i32)>,
) -> R<(bool, Option<i32>)> {
    let invalid = || format!("non-local or undecodable syscall: {line}");
    let (name, rest) = call.split_once('(').ok_or_else(invalid)?;
    let (args, result) = rest.rsplit_once("= ").ok_or_else(invalid)?;
    let args = args
        .trim_end()
        .strip_suffix(')')
        .ok_or_else(invalid)?
        .trim_end();
    let result = match result.split_whitespace().next() {
        Some("?") => None,
        v => Some(v.and_then(|v| v.parse::<i32>().ok()).ok_or_else(invalid)?),
    };
    match name {
        "socket" => {
            let pid = pid.ok_or_else(invalid)?;
            let fields = args.split(", ").collect::<Vec<_>>();
            let [family, kind, protocol] = fields.as_slice() else {
                return Err(invalid().into());
            };
            // A socket whose result is unknown authorizes nothing.
            if let Some(result) = result.filter(|v| *v >= 0) {
                // Every successful socket replaces the previous identity of this fd.
                sockets.remove(&(pid, result));
                let mut flags = kind.split('|');
                if *family == "AF_NETLINK"
                    && flags.next() == Some("SOCK_RAW")
                    && flags.all(|v| matches!(v, "SOCK_CLOEXEC" | "SOCK_NONBLOCK"))
                    && *protocol == "NETLINK_SOCK_DIAG"
                {
                    sockets.insert((pid, result));
                }
            }
        }
        "close" => {
            let pid = pid.ok_or_else(invalid)?;
            let fd = args.parse::<i32>().map_err(|_| invalid())?;
            // Do not retain permission even on a failed/ambiguous close.
            sockets.remove(&(pid, fd));
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
                && pid.is_some_and(|pid| sockets.contains(&(pid, fd)));
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
            "trace=socket,close,connect,execve",
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
            .args(["-f", "-e", "trace=socket,close,connect", "-s", "4096", "-o"])
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
            netlink.replace("= 0", "<unfinished ...>")
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
}
