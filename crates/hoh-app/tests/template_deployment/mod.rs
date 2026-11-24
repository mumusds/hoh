//! AC-029-03: installed release, no external network, fake CTO, audited host and client.
use crate::{installed, record, text, world};
use installed::R;
use serde_json::{Value, json};
use std::{
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
    for line in trace.lines().filter(|line| line.contains("connect(")) {
        count += 1;
        let local = line.contains("sa_family=AF_UNIX")
            || (line.contains("sa_family=AF_INET,") && line.contains("inet_addr(\"127."))
            || (line.contains("sa_family=AF_INET6,")
                && line.contains("inet_pton(AF_INET6, \"::1\""));
        if !local {
            return Err(format!("non-local or undecodable connect: {line}").into());
        }
    }
    Ok(count)
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
            "trace=connect,execve",
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
            .args(["-f", "-e", "trace=connect", "-s", "4096", "-o"])
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
