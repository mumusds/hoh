//! S2 covers direct approval origins. Draft answer/REPLY integration belongs to S5.
use super::*;

async fn peer(host: &Host, body: &Value, unix: bool, mode: &str) -> R<(u16, Value)> {
    let request = format!(
        "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nX-HoH-Origin: user\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        host.port,
        host.token,
        body.to_string().len(),
        body
    );
    let address = if unix {
        host.socket.to_string_lossy().into_owned()
    } else {
        host.port.to_string()
    };
    let mode = mode.to_owned();
    let script = r#"
import os, signal, socket, subprocess, sys
signal.alarm(25)
if sys.argv[1] == 'ancestor':
    env = os.environ.copy()
    del env['HOH_ATTEMPT_ID']
    sys.exit(subprocess.run([sys.executable, '-I', '-c', sys.argv[2], 'direct', *sys.argv[2:]], env=env).returncode)
s = socket.socket(socket.AF_UNIX if sys.argv[3] == 'unix' else socket.AF_INET)
s.settimeout(15)
s.connect(sys.argv[4] if sys.argv[3] == 'unix' else ('127.0.0.1', int(sys.argv[4])))
child = None
if sys.argv[1] == 'shared':
    read, write = os.pipe()
    child = os.fork()
    if child == 0:
        os.close(write)
        os.read(read, 1)
        os._exit(0)
    os.close(read)
s.sendall(sys.argv[5].encode())
while True:
    data = s.recv(65536)
    if not data: break
    sys.stdout.buffer.write(data)
if child is not None:
    os.write(write, b'x')
    os.close(write)
    os.waitpid(child, 0)
"#;
    let output = tokio::task::spawn_blocking(move || {
        let environment = match mode.as_str() {
            "marker" | "ancestor" => vec![("HOH_ATTEMPT_ID", "another-project/another-attempt")],
            // INVOCATION_ID alone can denote a user scope; socket activation is an explicit
            // service marker regardless of the cgroup inherited from the test launcher.
            "service" => vec![("LISTEN_FDS", "1")],
            _ => vec![],
        };
        let mut child = world::approval::python(script, &environment);
        child.args([
            mode.as_str(),
            script,
            if unix { "unix" } else { "tcp" },
            &address,
            &request,
        ]);
        child.output()
    })
    .await??;
    assert!(
        output.status.success(),
        "peer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response = String::from_utf8(output.stdout)?;
    let (head, body) = response.split_once("\r\n\r\n").ok_or("no response")?;
    Ok((
        head.split_whitespace().nth(1).ok_or("no status")?.parse()?,
        serde_json::from_str(body)?,
    ))
}

fn state(world: &World) -> R<Value> {
    let mut result = json!({});
    for table in [
        "specs",
        "runs",
        "tasks",
        "commands",
        "input_queue",
        "events",
        "outbox",
    ] {
        result[table] = json!(world.rows(&format!("SELECT * FROM {table} ORDER BY rowid"))?);
    }
    Ok(result)
}

#[tokio::test]
async fn test_008_05_direct_approval_tcp_uds_marker_ancestor_and_replay() -> R {
    let world = World::new()?;
    let index = repository(&world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "origin-start", "origin fixture").await?;
    let body = command(
        &host,
        &run,
        "origin-approve",
        "ApproveSpec",
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let before = state(&world)?;
    for unix in [false, true] {
        for ancestor in [false, true] {
            let (status, refused) = peer(
                &host,
                &body,
                unix,
                if ancestor { "ancestor" } else { "marker" },
            )
            .await?;
            assert_eq!(status, 403, "unix={unix}, ancestor={ancestor}: {refused}");
            assert_eq!(refused["code"], "SPEC_APPROVAL_NOT_USER");
            assert_eq!(
                refused["details"]["reason"],
                if ancestor {
                    "ancestor_marker"
                } else {
                    "marker"
                },
                "unix={unix}, ancestor={ancestor}"
            );
            assert_eq!(state(&world)?, before, "refusal must write nothing");
        }
    }
    for mode in ["service", "shared"] {
        for unix in [false, true] {
            let (status, refused) = peer(&host, &body, unix, mode).await?;
            assert_eq!(status, 403, "{mode}: {refused}");
            assert_eq!(refused["details"]["reason"], "origin_unknown");
            assert_eq!(state(&world)?, before);
        }
    }
    for decision in ["APPROVE_AND_BUILD", "REJECT"] {
        let mut other = body.clone();
        other["payload"]["decision"] = json!(decision);
        let (status, refused) = peer(&host, &other, false, "marker").await?;
        assert_eq!(status, 403, "{decision}: {refused}");
        assert_eq!(state(&world)?, before);
    }
    let (status, approved) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202, "user approval: {approved}");
    let after = state(&world)?;
    let (status, uds_replay) = peer(&host, &body, true, "user").await?;
    assert_eq!(status, 202, "user UDS replay: {uds_replay}");
    assert_eq!(uds_replay, approved);
    for unix in [false, true] {
        let (status, refused) = peer(&host, &body, unix, "marker").await?;
        assert_eq!(
            status, 403,
            "an Attempt cannot replay a user's receipt: {refused}"
        );
        assert_eq!(refused["details"]["reason"], "marker");
        assert_eq!(state(&world)?, after);
    }
    let (status, replay) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
    assert_eq!(status, 202);
    assert_eq!(replay, approved);
    assert_eq!(state(&world)?, after);
    assert!(host.shutdown().await);
    Ok(())
}

#[tokio::test]
async fn test_008_05_real_attempt_cgroup_refuses_tcp_and_uds() -> R {
    const IN_SCOPE: &str = "HOH_M3T04_ORIGIN_SCOPE";
    if std::env::var_os(IN_SCOPE).is_none() {
        let exe = std::env::current_exe()?;
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("systemd-run")
                .args([
                    "--user",
                    "--scope",
                    "-p",
                    "Delegate=yes",
                    "--quiet",
                    "--collect",
                    "--expand-environment=no",
                    "--",
                ])
                .arg(exe)
                .args([
                    "spec_source::origin::test_008_05_real_attempt_cgroup_refuses_tcp_and_uds",
                    "--exact",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(IN_SCOPE, "1")
                .output()
        })
        .await??;
        assert!(
            output.status.success(),
            "delegated cgroup fixture failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        println!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Ok(());
    }
    let world = World::new()?;
    let index = repository(&world)?;
    let host = world.start_plain_host(None).await?;
    let run = world::start_run(&host, "cgroup-start", "origin cgroup fixture").await?;
    let body = command(
        &host,
        &run,
        "cgroup-approve",
        "ApproveSpec",
        json!({"spec_sha256":index.sha256(),"decision":"APPROVE_ONLY"}),
    )
    .await?;
    let before = state(&world)?;
    let cgroups = std::fs::read_to_string("/proc/self/cgroup")?;
    let group = cgroups
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .ok_or("no v2 group")?;
    let parent = std::path::Path::new("/sys/fs/cgroup").join(group);
    let unique = format!(
        "{:016x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as u64
    );
    let path = parent.join(format!("attempt-{unique}"));
    std::fs::create_dir(&path)?;
    // The exact created path is the only cgroup this fixture removes; no processes are killed.
    struct Owned(Option<std::path::PathBuf>);
    impl Drop for Owned {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = std::fs::remove_dir(path);
            }
        }
    }
    let mut owned = Owned(Some(path.clone()));
    eprintln!("M3-T04 S2 fixture owns {}", path.display());
    for unix in [false, true] {
        // Spawn an unmarked peer directly in our own cgroup, leaving the test/host outside it.
        let address = if unix {
            host.socket.to_string_lossy().into_owned()
        } else {
            host.port.to_string()
        };
        let request = format!(
            "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nConnection: close\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            host.port,
            host.token,
            body.to_string().len(),
            body
        );
        let path = path.clone();
        let output = tokio::task::spawn_blocking(move || {
            world::approval::python(
                r#"
import os, socket, sys
from pathlib import Path
assert 'HOH_ATTEMPT_ID' not in os.environ
Path(sys.argv[1], 'cgroup.procs').write_text(str(os.getpid()))
s = socket.socket(socket.AF_UNIX if sys.argv[2] == 'unix' else socket.AF_INET)
s.settimeout(15)
s.connect(sys.argv[3] if sys.argv[2] == 'unix' else ('127.0.0.1', int(sys.argv[3])))
s.sendall(sys.argv[4].encode())
while True:
    data=s.recv(65536)
    if not data: break
    sys.stdout.buffer.write(data)
"#,
                &[],
            )
            .arg(path)
            .arg(if unix { "unix" } else { "tcp" })
            .arg(address)
            .arg(request)
            .output()
        })
        .await??;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response = String::from_utf8(output.stdout)?;
        let (head, body) = response.split_once("\r\n\r\n").ok_or("no response")?;
        assert!(head.starts_with("HTTP/1.1 403"), "{head}: {body}");
        let refused: Value = serde_json::from_str(body)?;
        assert_eq!(refused["code"], "SPEC_APPROVAL_NOT_USER");
        assert_eq!(refused["details"]["reason"], "attempt_cgroup");
        assert_eq!(state(&world)?, before);
    }
    for unix in [false, true] {
        // Reuse the *same* live HTTP connection: a user becomes an Attempt cgroup member between commands.
        // The second command replays the first command's key, so both request freshness and replay ordering matter.
        let address = if unix {
            host.socket.to_string_lossy().into_owned()
        } else {
            host.port.to_string()
        };
        let request = format!(
            "POST /v1/commands HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nX-HoH-Token: {}\r\nConnection: keep-alive\r\nContent-Length: {}\r\nContent-Type: application/json\r\n\r\n{}",
            host.port,
            host.token,
            body.to_string().len(),
            body
        );
        let path = path.clone();
        let output = tokio::task::spawn_blocking(move || {
            world::approval::python(
                r#"
import json, os, socket, sys
from pathlib import Path
s = socket.socket(socket.AF_UNIX if sys.argv[2] == 'unix' else socket.AF_INET)
s.settimeout(15)
s.connect(sys.argv[3] if sys.argv[2] == 'unix' else ('127.0.0.1', int(sys.argv[3])))
f = s.makefile('rb')
def command():
    s.sendall(sys.argv[4].encode())
    status = int(f.readline().split()[1])
    headers = {}
    while True:
        line = f.readline()
        if line == b'\r\n': break
        if not line: raise RuntimeError('truncated headers')
        key, value = line.split(b':', 1)
        headers[key.lower()] = value.strip()
    body = json.loads(f.read(int(headers[b'content-length'])))
    return [status, body]
first = command()
Path(sys.argv[1], 'cgroup.procs').write_text(str(os.getpid()))
second = command()
print(json.dumps([first, second]))
"#,
                &[],
            )
            .arg(path)
            .arg(if unix { "unix" } else { "tcp" })
            .arg(address)
            .arg(request)
            .output()
        })
        .await??;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let responses: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(responses[0][0], 202, "keepalive user: {responses}");
        assert_eq!(
            responses[1][0], 403,
            "keepalive authority must be recomputed: {responses}"
        );
        assert_eq!(responses[1][1]["details"]["reason"], "attempt_cgroup");
        assert_eq!(world.rows("SELECT * FROM specs")?.len(), 1);
        assert_eq!(
            world
                .rows("SELECT * FROM commands WHERE kind='ApproveSpec'")?
                .len(),
            1
        );
    }
    std::fs::remove_dir(owned.0.take().ok_or("cgroup already removed")?)?;
    assert!(host.shutdown().await);
    Ok(())
}
