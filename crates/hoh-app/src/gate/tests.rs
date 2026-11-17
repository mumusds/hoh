//! The checked start gate (docs/adr/ADR-030.md decision 9, "执行时的身份" and "闸门的结果通道"): what it verifies, the
//! inode it executes, the plan it executes it by, and the record it keeps. `gate::main` may exec, so it runs in a child:
//! this test binary again, running only [`gate_child`].

use super::*;
use hoh_agents::adapter::{GateCommand, GateStatus};
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

const KEY: &str = "5d0c6a1f9e8b7a6c5d4e3f2a1b0c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3b2c";

/// The child's arguments: a JSON array of `gate::main`'s; with [`PAUSE`] it verifies, makes `<dir>/verified`, waits
/// for `<dir>/go-on` and only then execs (the window a test replaces the implementation in).
const CHILD: &str = "HOH_GATE_TEST_CHILD";
const PAUSE: &str = "HOH_GATE_TEST_PAUSE";

/// Not a test of its own: the child the other tests run `gate::main` in.
#[test]
#[ignore = "the gate tests' child process"]
fn gate_child() {
    let Ok(args) = std::env::var(CHILD) else {
        return;
    };
    let args: Vec<String> = serde_json::from_str(&args).unwrap();
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    let Ok(dir) = std::env::var(PAUSE) else {
        std::process::exit(main(&args));
    };
    let dir = PathBuf::from(dir);
    let call = parse(&args).unwrap();
    let checked = call.checked.unwrap();
    let verified = verified(&checked.expect).unwrap();
    std::fs::write(dir.join("verified"), "").unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while !dir.join("go-on").exists() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let error = exec(&verified, call.rest);
    panic!("exec: {error}");
}

/// The child running `gate::main(args)` with `input` on stdin (`pause`: verify, wait, exec, see [`PAUSE`]).
fn spawn(args: &[OsString], pause: Option<&Path>) -> R<std::process::Child> {
    let args: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "gate::tests::gate_child",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, serde_json::to_string(&args)?)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = pause {
        command.env(PAUSE, dir);
    }
    Ok(command.spawn()?)
}

/// `gate::main(args)` given `input` (then its end): its exit code and stderr.
fn gate(args: &[OsString], input: &[u8]) -> R<(Option<i32>, String)> {
    let mut child = spawn(args, None)?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    match stdin.write_all(input) {
        Err(e) if e.kind() != std::io::ErrorKind::BrokenPipe => return Err(e.into()),
        _ => {}
    }
    drop(stdin);
    let out = output_within(child, Duration::from_secs(60))?;
    Ok((
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

// The helpers' bounded wait: the integration tests' `tests/common/output.rs`, the same bytes (CIRF-53).
mod output;
use output::output_within;

#[test]
fn the_bounded_wait_is_the_integration_tests_own_code() -> R {
    // CIRF-53: one implementation in two files, the same bytes; see the file's header for why there are two.
    let here = concat!(env!("CARGO_MANIFEST_DIR"), "/src/gate/tests/output.rs");
    let there = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/common/output.rs");
    assert_eq!(
        std::fs::read_to_string(here)?,
        std::fs::read_to_string(there)?
    );
    Ok(())
}

fn sha256_of(path: &Path) -> R<String> {
    use sha2::{Digest as _, Sha256};
    Ok(Sha256::digest(std::fs::read(path)?)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn file_stamp(path: &Path) -> R<FileStamp> {
    use std::os::unix::fs::MetadataExt as _;
    let m = std::fs::metadata(path)?;
    Ok(FileStamp {
        dev: m.dev(),
        ino: m.ino(),
        ctime_sec: m.ctime(),
        ctime_nsec: m.ctime_nsec(),
    })
}

/// What a prepare would expect of `agent` found at `launcher` now: its identity, stamps and plan from the files.
fn expect_of(agent: Agent, launcher: &Path) -> R<Expect> {
    let implementation = probe::resolve_implementation(agent, launcher)?;
    Ok(Expect {
        agent,
        identity: AgentIdentity {
            launcher_path: launcher.display().to_string(),
            launcher_sha256: sha256_of(launcher)?,
            implementation_path: implementation.display().to_string(),
            implementation_sha256: sha256_of(&implementation)?,
        },
        stamp: Stamp {
            launcher: file_stamp(launcher)?,
            implementation: file_stamp(&implementation)?,
        },
        plan: probe::exec_plan(agent, launcher, &implementation).exec,
    })
}

/// The product's checked gate command line (`GateCommand::argv`, without `__gate`) for `expect`, recording to
/// `status`, then the program and `args`.
fn checked(expect: &Expect, status: &Path, program: &Path, args: &[&str]) -> Vec<OsString> {
    let gate = GateCommand {
        program: PathBuf::new(),
        args: Vec::new(),
        env: Vec::new(),
        expect: Some(expect.clone()),
        status: Some(GateStatus {
            path: status.to_path_buf(),
            key: KEY.to_owned(),
        }),
    };
    let mut argv = gate.argv();
    argv.push(program.as_os_str().to_owned());
    argv.extend(args.iter().map(OsString::from));
    argv
}

fn executable(path: &Path, text: &str) -> R {
    std::fs::write(path, text)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// A copy of dash at `path`: a real ELF, not a multi-call program, so any `argv[0]` runs it.
fn elf(path: &Path) -> R {
    std::fs::copy("/usr/bin/dash", path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// The JSON lines of a gate's record.
fn records(path: &Path) -> R<Vec<Value>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

/// Wait until `path` exists, for at most 30 s.
fn wait_for(path: &Path) -> R {
    let end = Instant::now() + Duration::from_secs(30);
    while !path.exists() {
        if Instant::now() > end {
            return Err(format!("{} never appeared", path.display()).into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

#[test]
fn the_gate_execs_the_inode_it_hashed() -> R {
    // Decision 9 (E1) and its implementation correction (execveat is unsafe in rustix; the gate executes the verified
    // descriptor's /proc/self/fd link): once A is verified, B renamed to A's path changes the path, not the descriptor
    // (its dev, ino and content are A's), and what is executed is the inode that was hashed, its /proc/<pid>/exe naming
    // the implementation's path (here "(deleted)": the path is B's since).
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = root.join("agent");
    elf(&agent)?;
    let a_sha = sha256_of(&agent)?;
    let a = file_stamp(&agent)?;
    let expect = expect_of(Agent::Claude, &agent)?;
    assert_eq!(expect.plan, Exec::Fd);
    let verified = verified(&expect).map_err(|m| format!("{m:?}"))?;
    let b = root.join("b");
    executable(&b, "#!/bin/sh\necho B\n")?;
    std::fs::rename(&b, &agent)?;
    let held = stamp_of(&verified.fd)?;
    assert_eq!((held.dev, held.ino), (a.dev, a.ino));
    let mut content = Vec::new();
    {
        use std::os::unix::fs::FileExt as _;
        let mut at = 0;
        let mut piece = [0u8; 1 << 16];
        loop {
            let n = verified.fd.read_at(&mut piece, at)?;
            if n == 0 {
                break;
            }
            content.extend_from_slice(&piece[..n]);
            at += n as u64;
        }
    }
    use sha2::{Digest as _, Sha256};
    let held_sha: String = Sha256::digest(&content)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(held_sha, a_sha);
    assert_ne!(file_stamp(&agent)?.ino, a.ino, "the path is B's now");
    assert_ne!(sha256_of(&agent)?, a_sha);
    drop(verified);

    // Executed: the child verifies A, waits while B is renamed over it, then execs what it verified.
    let run = |name: &str, replace: bool| -> R<String> {
        let agent = root.join(name);
        elf(&agent)?;
        let out = root.join(format!("{name}.out"));
        let pause = root.join(format!("{name}.pause"));
        std::fs::create_dir_all(&pause)?;
        let expect = expect_of(Agent::Claude, &agent)?;
        let script = format!(
            "readlink /proc/$$/exe > '{}'; echo A >> '{}'",
            out.display(),
            out.display()
        );
        let args = checked(
            &expect,
            &root.join(format!("{name}.jsonl")),
            &agent,
            &["-c", &script, "unused"],
        );
        let parsed = parse(&args).ok_or("unparsed")?;
        let mut child = spawn(&args, Some(&pause))?;
        drop(child.stdin.take());
        wait_for(&pause.join("verified"))?;
        if replace {
            let b = root.join(format!("{name}.b"));
            executable(&b, &format!("#!/bin/sh\necho B > '{}'\n", out.display()))?;
            std::fs::rename(&b, &agent)?;
        }
        std::fs::write(pause.join("go-on"), "")?;
        let status = child.wait()?;
        assert!(status.success(), "{status:?}");
        assert_eq!(parsed.rest.len(), 3);
        Ok(std::fs::read_to_string(&out)?)
    };
    let kept = run("kept", false)?;
    assert_eq!(kept, format!("{}\nA\n", root.join("kept").display()));
    let replaced = run("replaced", true)?;
    assert_eq!(
        replaced,
        format!("{} (deleted)\nA\n", root.join("replaced").display()),
        "the verified inode ran, not B"
    );
    Ok(())
}

/// A pi install at `root`: `bin/pi` its launcher, `install/current-version` naming `version`, each version's
/// `node_modules/.bin/pi` a link to its `dist/bundle/cli.js` script.
fn pi_install(root: &Path, versions: &[&str], current: &str) -> R<PathBuf> {
    for version in versions {
        let release = root.join("install/releases").join(version);
        let bundle = release.join("node_modules/pi-coding-agent/dist/bundle");
        std::fs::create_dir_all(&bundle)?;
        executable(
            &bundle.join("cli.js"),
            &format!("#!/usr/bin/env node\n// {version}\n"),
        )?;
        std::fs::create_dir_all(release.join("node_modules/.bin"))?;
        std::os::unix::fs::symlink(
            "../pi-coding-agent/dist/bundle/cli.js",
            release.join("node_modules/.bin/pi"),
        )?;
    }
    std::fs::create_dir_all(root.join("bin"))?;
    let launcher = root.join("bin/pi");
    executable(&launcher, "#!/bin/sh\nexec \"$pi_release_bin\" \"$@\"\n")?;
    std::fs::write(root.join("install/current-version"), format!("{current}\n"))?;
    Ok(launcher)
}

#[test]
fn a_changed_resolution_is_refused() -> R {
    // Decision 9: the gate resolves the implementation again the probe's way; pi's current-version flipped since the
    // expectation was taken resolves another implementation path: refused before anything is hashed.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let launcher = pi_install(&root, &["1.5.0", "1.6.0"], "1.5.0")?;
    let expect = expect_of(Agent::Pi, &launcher)?;
    assert!(
        expect
            .identity
            .implementation_path
            .ends_with("releases/1.5.0/node_modules/pi-coding-agent/dist/bundle/cli.js"),
        "{expect:?}"
    );
    assert!(verified(&expect).is_ok());
    std::fs::write(root.join("install/current-version"), "1.6.0\n")?;
    let mismatch = verified(&expect).err().ok_or("verified")?;
    assert_eq!(
        (mismatch.item, mismatch.stamp_only),
        ("implementation_path", false)
    );
    assert!(
        mismatch.seen.as_str().is_some_and(|s| s.contains("1.6.0")),
        "{mismatch:?}"
    );
    Ok(())
}

#[test]
fn exec_plan_follows_each_known_layout() -> R {
    // Decision 9 ("执行计划"): claude's link to its versioned binary, codex's npm wrapper and its vendored binary, and a
    // launcher that is its own ELF are E1, executed as their implementation (argv[0] its path; the npm wrapper's two
    // variables set); pi's launcher and its release's script, and a launcher that is its own script, are E2 (pi's
    // PI_MANAGED_INSTALL_ROOT, and its node's directory before PATH when that node is there).
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let none: Vec<(OsString, OsString)> = Vec::new();

    // claude: ~/.local/bin/claude -> versions/<version>.
    std::fs::create_dir_all(root.join("claude/versions"))?;
    std::fs::create_dir_all(root.join("claude/bin"))?;
    let version = root.join("claude/versions/2.1.318");
    elf(&version)?;
    let claude = root.join("claude/bin/claude");
    std::os::unix::fs::symlink(&version, &claude)?;
    let plan = probe::exec_plan(Agent::Claude, &claude, &version);
    assert_eq!(
        (
            plan.exec,
            &plan.implementation,
            &plan.env,
            &plan.path_prefix
        ),
        (Exec::Fd, &version, &none, &None)
    );

    // codex: <prefix>/bin/codex -> lib/node_modules/@openai/codex/bin/codex.js, which runs vendor/<triple>/bin/codex.
    let package = root.join("npm/lib/node_modules/@openai/codex");
    std::fs::create_dir_all(package.join("bin"))?;
    executable(&package.join("bin/codex.js"), "#!/usr/bin/env node\n")?;
    std::fs::write(
        package.join("package.json"),
        r#"{"name": "@openai/codex", "bin": {"codex": "bin/codex.js"}}"#,
    )?;
    let triple = match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-musl",
        _ => "aarch64-unknown-linux-musl",
    };
    let native = package.join("vendor").join(triple).join("bin/codex");
    std::fs::create_dir_all(native.parent().ok_or("parent")?)?;
    elf(&native)?;
    std::fs::create_dir_all(root.join("npm/bin"))?;
    let codex = root.join("npm/bin/codex");
    std::os::unix::fs::symlink(package.join("bin/codex.js"), &codex)?;
    let expect = expect_of(Agent::Codex, &codex)?;
    assert_eq!(
        Path::new(&expect.identity.implementation_path),
        native.as_path()
    );
    let codex_verified = verified(&expect).map_err(|m| format!("{m:?}"))?;
    let codex_env: Vec<(OsString, OsString)> = vec![
        (
            "CODEX_MANAGED_PACKAGE_ROOT".into(),
            package.clone().into_os_string(),
        ),
        ("CODEX_MANAGED_BY_NPM".into(), "1".into()),
    ];
    assert_eq!(
        (
            codex_verified.plan.exec,
            &codex_verified.plan.env,
            &codex_verified.plan.path_prefix
        ),
        (Exec::Fd, &codex_env, &None)
    );
    let call = invocation(&codex_verified, &["app-server".into()])?;
    use std::os::fd::AsRawFd as _;
    assert_eq!(
        call.program,
        PathBuf::from(format!("/proc/self/fd/{}", codex_verified.fd.as_raw_fd()))
    );
    assert_eq!(
        call.argv,
        vec![native.clone().into_os_string(), "app-server".into()]
    );
    assert_eq!(call.env, codex_env);

    // pi: <agent dir>/bin/pi runs install/releases/<version>/node_modules/.bin/pi, a link to dist/bundle/cli.js.
    let agent_dir = root.join("pi-agent");
    let pi = pi_install(&agent_dir, &["1.5.0"], "1.5.0")?;
    let script = probe::resolve_implementation(Agent::Pi, &pi)?;
    let home = root.join("data");
    let without = probe::exec_plan_in(Agent::Pi, &pi, &script, Some(&home));
    let pi_env: Vec<(OsString, OsString)> = vec![(
        "PI_MANAGED_INSTALL_ROOT".into(),
        agent_dir.join("install").into_os_string(),
    )];
    assert_eq!(
        (
            without.exec,
            &without.implementation,
            &without.env,
            &without.path_prefix
        ),
        (Exec::Path, &script, &pi_env, &None)
    );
    let node_dir = home.join("pi-node/current/bin");
    std::fs::create_dir_all(&node_dir)?;
    executable(&node_dir.join("node"), "#!/bin/sh\n")?;
    let with = probe::exec_plan_in(Agent::Pi, &pi, &script, Some(&home));
    assert_eq!(
        (with.exec, &with.env, &with.path_prefix),
        (Exec::Path, &pi_env, &Some(node_dir.clone()))
    );
    // E2: the script's `env node` is pi's node, found on the PATH its plan puts first; the script read by its path.
    let pi_verified = Verified {
        fd: File::open(&script)?,
        plan: with,
        stamp: file_stamp(&script)?,
    };
    let call = invocation(&pi_verified, &["--mode".into(), "rpc".into()])?;
    assert_eq!(call.program, node_dir.join("node"));
    assert_eq!(
        call.argv,
        vec![
            node_dir.join("node").into_os_string(),
            script.clone().into_os_string(),
            "--mode".into(),
            "rpc".into()
        ]
    );
    assert!(
        call.env.iter().any(|(k, v)| k == "PATH"
            && v.to_string_lossy()
                .starts_with(&format!("{}:", node_dir.display()))),
        "{:?}",
        call.env
    );

    // A launcher that is its own implementation: an ELF is E1, a script E2 (its interpreter as the shebang names it).
    let own_elf = root.join("own-elf");
    elf(&own_elf)?;
    let plan = probe::exec_plan(Agent::Codex, &own_elf, &own_elf);
    assert_eq!(
        (plan.exec, &plan.env, &plan.path_prefix),
        (Exec::Fd, &none, &None)
    );
    let own_script = root.join("own-script");
    executable(&own_script, "#!/bin/sh -e\necho hi\n")?;
    let expect = expect_of(Agent::Codex, &own_script)?;
    let script_verified = super::verified(&expect).map_err(|m| format!("{m:?}"))?;
    assert_eq!(
        (
            script_verified.plan.exec,
            &script_verified.plan.env,
            &script_verified.plan.path_prefix
        ),
        (Exec::Path, &none, &None)
    );
    let call = invocation(&script_verified, &["x".into()])?;
    assert_eq!(
        (call.program.as_path(), call.argv),
        (
            Path::new("/bin/sh"),
            vec![
                "/bin/sh".into(),
                "-e".into(),
                own_script.into_os_string(),
                "x".into()
            ]
        )
    );
    Ok(())
}

/// A script agent at `<root>/agent` that makes `<root>/ran` when it runs.
fn marking_agent(root: &Path) -> R<PathBuf> {
    let agent = root.join("agent");
    executable(
        &agent,
        &format!("#!/bin/sh\n: > '{}'\n", root.join("ran").display()),
    )?;
    Ok(agent)
}

#[test]
fn a_refusal_is_recorded_with_its_key_before_the_exit() -> R {
    // Decision 9 (-101): an implementation hash that is not the expected one: exactly one `refused` line with the go
    // line's key, item implementation_sha256, not stamp_only; exit 65 and a line for a person on stderr; the program
    // never ran.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = marking_agent(&root)?;
    let mut expect = expect_of(Agent::Claude, &agent)?;
    expect.identity.implementation_sha256 = "0".repeat(64);
    let status = root.join("logs/gate.jsonl");
    std::fs::create_dir_all(root.join("logs"))?;
    let (code, stderr) = gate(
        &checked(&expect, &status, &agent, &[]),
        format!("go {KEY}\n").as_bytes(),
    )?;
    assert_eq!(code, Some(65), "{stderr}");
    assert!(
        stderr.contains("hoh gate: COMPAT_UNVERIFIED: implementation_sha256"),
        "{stderr}"
    );
    let lines = records(&status)?;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        (
            &lines[0]["outcome"],
            &lines[0]["key"],
            &lines[0]["code"],
            &lines[0]["item"],
            &lines[0]["stamp_only"]
        ),
        (
            &json!("refused"),
            &json!(KEY),
            &json!("COMPAT_UNVERIFIED"),
            &json!("implementation_sha256"),
            &json!(false)
        )
    );
    assert_eq!(lines[0]["expected"], json!("0".repeat(64)));
    assert_eq!(lines[0]["seen"], json!(sha256_of(&agent)?));
    assert!(!root.join("ran").exists(), "the program ran");
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(std::fs::metadata(&status)?.mode() & 0o777, 0o600);
    }
    Ok(())
}

#[test]
fn a_stamp_only_mismatch_is_marked() -> R {
    // Decision 9 (-201): resolution and both hashes as expected, only the implementation's stamp (its ctime) not:
    // refused{item: stamp, stamp_only: true}, and the program never ran.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = marking_agent(&root)?;
    let mut expect = expect_of(Agent::Claude, &agent)?;
    expect.stamp.implementation.ctime_nsec =
        (expect.stamp.implementation.ctime_nsec + 1) % 1_000_000_000;
    let status = root.join("gate.jsonl");
    let (code, _) = gate(
        &checked(&expect, &status, &agent, &[]),
        format!("go {KEY}\n").as_bytes(),
    )?;
    assert_eq!(code, Some(65));
    let lines = records(&status)?;
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(
        (
            &lines[0]["outcome"],
            &lines[0]["key"],
            &lines[0]["item"],
            &lines[0]["stamp_only"]
        ),
        (
            &json!("refused"),
            &json!(KEY),
            &json!("stamp"),
            &json!(true)
        )
    );
    assert!(!root.join("ran").exists());
    // A hash that differs too is not stamp_only.
    expect.identity.launcher_sha256 = "1".repeat(64);
    std::fs::remove_file(&status)?;
    gate(
        &checked(&expect, &status, &agent, &[]),
        format!("go {KEY}\n").as_bytes(),
    )?;
    let lines = records(&status)?;
    assert_eq!(
        (&lines[0]["item"], &lines[0]["stamp_only"]),
        (&json!("launcher_sha256"), &json!(false))
    );
    Ok(())
}

/// A script that writes, to its first argument, its argv, environment, open descriptors with their targets and every
/// file of the directory its second argument names, with their contents.
const DUMP: &str = "#!/usr/bin/env python3\nimport json, os, sys\nfds = {}\nfor fd in os.listdir('/proc/self/fd'):\n    try:\n        fds[fd] = os.readlink('/proc/self/fd/' + fd)\n    except OSError as e:\n        fds[fd] = str(e)\nfiles = {}\nfor name in os.listdir(sys.argv[2]):\n    path = os.path.join(sys.argv[2], name)\n    if os.path.isfile(path):\n        files[name] = open(path, encoding='utf-8', errors='replace').read()\nopen(sys.argv[1], 'w').write(json.dumps({'argv': sys.argv, 'environ': dict(os.environ), 'fds': fds, 'files': files}))\n";

#[test]
fn a_verified_exec_leaves_no_key() -> R {
    // Decision 9 (-101, -201): verified, the gate records `verified` (no key, nothing of it) with the implementation's
    // stamp, then execs it; the program sees no key in its argv, environment, descriptors or the log directory, and
    // no descriptor of gate.jsonl.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = root.join("agent");
    executable(&agent, DUMP)?;
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs)?;
    let status = logs.join("gate.jsonl");
    let out = root.join("dump.json");
    let expect = expect_of(Agent::Claude, &agent)?;
    assert_eq!(expect.plan, Exec::Path);
    let (code, stderr) = gate(
        &checked(
            &expect,
            &status,
            &agent,
            &[out.to_str().ok_or("path")?, logs.to_str().ok_or("path")?],
        ),
        format!("go {KEY}\n").as_bytes(),
    )?;
    assert_eq!(code, Some(0), "{stderr}");
    let lines = records(&status)?;
    assert_eq!(lines.len(), 1, "{lines:?}");
    let stamp = file_stamp(&agent)?;
    assert_eq!(
        lines[0],
        json!({"outcome": "verified", "plan": "E2", "implementation": {"path": agent.display().to_string(),
               "dev": stamp.dev, "ino": stamp.ino, "ctime_sec": stamp.ctime_sec, "ctime_nsec": stamp.ctime_nsec}})
    );
    let dump = std::fs::read_to_string(&out)?;
    assert!(!dump.contains(KEY), "{dump}");
    let dump: Value = serde_json::from_str(&dump)?;
    assert_eq!(dump["argv"][0], json!(agent.display().to_string()));
    let fds = dump["fds"].as_object().ok_or("no fds")?;
    assert!(
        !fds.values()
            .any(|t| t.as_str().is_some_and(|t| t.contains("gate.jsonl"))),
        "{fds:?}"
    );
    assert!(dump["files"]["gate.jsonl"].is_string(), "{dump}");
    Ok(())
}

#[test]
fn a_status_gate_needs_its_key() -> R {
    // Decision 9: with --status the go line must be `go ` and 64 lower-case hex digits, else 64: nothing executed and
    // nothing recorded. The checking options are taken all together or not at all. Without them `go` still opens.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = marking_agent(&root)?;
    let expect = expect_of(Agent::Claude, &agent)?;
    let status = root.join("gate.jsonl");
    let args = checked(&expect, &status, &agent, &[]);
    for input in [
        "go\n".to_owned(),
        format!("go {}\n", &KEY[1..]),
        format!("go {}\n", KEY.to_uppercase()),
        format!("go {KEY}x\n"),
        format!("go  {KEY}\n"),
        format!("go {}\n", "g".repeat(64)),
    ] {
        let (code, _) = gate(&args, input.as_bytes())?;
        assert_eq!(code, Some(64), "{input:?}");
        assert!(!root.join("ran").exists(), "ran after {input:?}");
        assert!(!status.exists(), "recorded after {input:?}");
    }
    // An option missing, or one twice: a usage error, the input never read.
    let at = |name: &str| args.iter().position(|a| a == name).ok_or("no option");
    let (expect_at, stamp_at, plan_at, status_at) = (
        at("--expect")?,
        at("--stamp")?,
        at("--plan")?,
        at("--status")?,
    );
    for (from, to) in [
        (expect_at, expect_at + 6),
        (stamp_at, stamp_at + 3),
        (plan_at, plan_at + 2),
        (status_at, status_at + 2),
    ] {
        let mut partial = args.clone();
        partial.drain(from..to);
        let (code, _) = gate(&partial, format!("go {KEY}\n").as_bytes())?;
        assert_eq!(code, Some(64), "{partial:?}");
    }
    let mut twice = args.clone();
    twice.splice(plan_at..plan_at, ["--plan".into(), "fd".into()]);
    assert_eq!(gate(&twice, format!("go {KEY}\n").as_bytes())?.0, Some(64));
    assert!(!root.join("ran").exists() && !status.exists());
    // The input ending before its line: the gate stays closed.
    assert_eq!(gate(&args, b"")?.0, Some(0));
    // Without the options: today's gate.
    assert_eq!(gate(&[agent.clone().into_os_string()], b"go\n")?.0, Some(0));
    assert!(root.join("ran").exists());
    assert!(!status.exists());
    Ok(())
}

#[test]
fn an_unrecordable_decision_does_not_exec() -> R {
    // Decision 9: a decision the gate cannot record (its directory not writable) is not acted on: no exec, 65.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = marking_agent(&root)?;
    let expect = expect_of(Agent::Claude, &agent)?;
    let logs = root.join("logs");
    std::fs::create_dir_all(&logs)?;
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o500))?;
    let outcome = gate(
        &checked(&expect, &logs.join("gate.jsonl"), &agent, &[]),
        format!("go {KEY}\n").as_bytes(),
    );
    std::fs::set_permissions(&logs, std::fs::Permissions::from_mode(0o700))?;
    let (code, stderr) = outcome?;
    assert_eq!(code, Some(65), "{stderr}");
    assert!(!root.join("ran").exists());
    assert!(!logs.join("gate.jsonl").exists());
    Ok(())
}

#[test]
fn a_rewrite_while_hashing_is_refused() -> R {
    // Decision 9: the implementation's stamp read before and after it was hashed differs (a byte appended between the
    // two reads): what was hashed is not what is there, refused and not stamp_only.
    let dir = tempfile::tempdir()?;
    let root = dir.path().canonicalize()?;
    let agent = marking_agent(&root)?;
    let expect = expect_of(Agent::Claude, &agent)?;
    let appended = agent.clone();
    let mismatch = verified_with(&expect, &|| {
        // Past the ctime's granularity, so the change shows.
        std::thread::sleep(Duration::from_millis(20));
        let _ = std::fs::OpenOptions::new()
            .append(true)
            .open(&appended)
            .and_then(|mut f| f.write_all(b"\n"));
    })
    .err()
    .ok_or("verified")?;
    assert_eq!(
        (mismatch.item, mismatch.stamp_only),
        ("implementation_rewritten", false)
    );
    Ok(())
}
