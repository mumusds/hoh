//! TEST-005-06 (AC-005-06, check `deployment`; docs/adr/ADR-023.md decision 11): the candidate's release bundle,
//! installed into a fresh temporary HOME under a private user manager (`installed`), runs a detached run in a Git
//! fixture project and uninstalls, with no root and no sudo. `scripts/product/deployment.sh` builds the bundle from
//! the candidate and runs this with HOH_RELEASE_BUNDLE and HOH_EXPECTED_COMMIT; without them it fails, never passes.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

mod installed;

use installed::{Manager, R};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Output;

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn record(out: &Output) -> Value {
    json!({
        "exit": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
    })
}

/// Every path under `dir` with its type, mode, size and content digest: the `.hoh/` compared before and after.
fn manifest(dir: &Path) -> R<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    let mut todo = vec![dir.to_path_buf()];
    while let Some(at) = todo.pop() {
        for item in std::fs::read_dir(&at)? {
            let path = item?.path();
            let meta = std::fs::symlink_metadata(&path)?;
            let what = if meta.is_symlink() {
                format!("link {}", std::fs::read_link(&path)?.display())
            } else if meta.is_dir() {
                todo.push(path.clone());
                "dir".to_owned()
            } else {
                format!(
                    "file {} {}",
                    meta.len(),
                    installed::sha256(&std::fs::read(&path)?)
                )
            };
            out.insert(
                path.strip_prefix(dir)?.display().to_string(),
                format!(
                    "{what} mode {:o} uid {}",
                    meta.permissions().mode(),
                    meta.uid()
                ),
            );
        }
    }
    Ok(out)
}

/// The instance of `project`, computed here on its own (AC-005-06's `<项目 hash>`): the first 16 hex digits of the
/// SHA-256 of its canonical path.
fn instance(project: &Path) -> String {
    installed::sha256(project.as_os_str().as_encoded_bytes())[..16].to_owned()
}

/// What of this user's real environment an install could touch: its manager's hoh-host units and its home's hoh files.
fn real_environment() -> R<Value> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME")?);
    let mut files = BTreeMap::new();
    for file in [
        ".local/bin/hoh",
        ".local/share/hoh",
        ".local/share/systemd/user/hoh-host@.service",
        ".local/state/hoh/hosts",
    ] {
        let path = home.join(file);
        let meta = std::fs::symlink_metadata(&path).ok();
        files.insert(
            file,
            meta.map(|m| format!("{:o} {} {}", m.mode(), m.len(), m.mtime())),
        );
    }
    Ok(json!({"units": installed::real_units("hoh-host@*")?, "files": files}))
}

#[test]
#[ignore = "needs the candidate's release bundle: scripts/product/deployment.sh"]
fn test_005_06_the_release_bundle_installs_runs_a_detached_run_and_uninstalls_without_root() -> R {
    let bundle = PathBuf::from(
        std::env::var_os("HOH_RELEASE_BUNDLE")
            .ok_or("HOH_RELEASE_BUNDLE is not set: no bundle to install")?,
    );
    let commit = std::env::var("HOH_EXPECTED_COMMIT")
        .map_err(|_| "HOH_EXPECTED_COMMIT is not set: no commit to expect")?;
    if rustix::process::geteuid().is_root() {
        return Err("run as root: AC-005-06 installs without root".into());
    }
    let real_before = real_environment()?;
    let manager = Manager::start()?;
    let mut seen = serde_json::Map::new();
    seen.insert(
        "bundle".into(),
        json!({
            "path": bundle.display().to_string(),
            "sha256": installed::sha256(&std::fs::read(&bundle)?),
        }),
    );
    // A fresh HOME: no ~/.local/bin/hoh, no hoh user unit.
    assert!(!manager.home().join(".local/bin/hoh").exists());
    assert!(
        !manager
            .data()
            .join("systemd/user/hoh-host@.service")
            .exists()
    );
    let sudo = manager.bin().join("sudo");
    installed::script(
        &sudo,
        &format!(
            "#!/bin/sh\necho \"$@\" >> {}/sudo-called\nexit 1\n",
            manager.root().display()
        ),
    )?;
    let unpacked = manager.root().join("unpacked");
    std::fs::create_dir(&unpacked)?;
    let status = std::process::Command::new("tar")
        .arg("-xzf")
        .arg(&bundle)
        .arg("-C")
        .arg(&unpacked)
        .status()?;
    assert!(status.success(), "tar -xzf {}", bundle.display());
    let hoh_dir = unpacked.join("hoh");
    // A fixture project with Git initialised.
    let project = manager.root().join("p");
    std::fs::create_dir(&project)?;
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    ] {
        let out = manager
            .command("git")
            .args(args)
            .current_dir(&project)
            .output()?;
        assert!(out.status.success(), "git {args:?}: {}", text(&out));
    }
    let project = std::fs::canonicalize(project)?;
    let unit = format!("hoh-host@{}.service", instance(&project));

    // install.sh from the bundle, enabling the fixture project's host unit.
    let out = manager.install(&hoh_dir, &["--project", project.to_str().ok_or("path")?])?;
    seen.insert("install".into(), record(&out));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let command = manager.home().join(".local/bin/hoh");
    let found = manager
        .command("sh")
        .args(["-c", "command -v hoh"])
        .output()?;
    assert_eq!(
        String::from_utf8(found.stdout)?.trim_end(),
        command.to_str().ok_or("path")?
    );
    let version = manager.command("hoh").arg("--version").output()?;
    seen.insert("version".into(), record(&version));
    assert_eq!(version.status.code(), Some(0), "{}", text(&version));
    assert!(
        String::from_utf8(version.stdout.clone())?.contains(&commit),
        "hoh --version does not name {commit}: {}",
        text(&version)
    );
    let enabled = manager.systemctl(&["is-enabled", &unit])?;
    seen.insert("is_enabled".into(), record(&enabled));
    assert_eq!(
        String::from_utf8(enabled.stdout)?.trim(),
        "enabled",
        "{unit}"
    );
    let shown = manager.show(&unit, "LoadState,ExecStart,FragmentPath")?;
    seen.insert("show".into(), json!(shown));
    let exec = shown.get("ExecStart").cloned().unwrap_or_default();
    assert!(
        exec.starts_with(&format!("{{ path={} ; ", command.display())),
        "ExecStart is not the installed binary: {exec}"
    );

    // A detached run in the fixture project, and an attach to it.
    let out = manager
        .command("hoh")
        .args(["-p", "x", "--detach", "--output", "json"])
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .output()?;
    seen.insert("detach".into(), record(&out));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let stdout = String::from_utf8(out.stdout)?;
    assert_eq!(stdout.lines().count(), 1, "one object: {stdout}");
    let accepted: Value = serde_json::from_str(&stdout)?;
    let run_id = accepted["run_id"].as_str().ok_or("no run_id")?.to_owned();
    let main = manager.property(&unit, "MainPID")?;
    let info: Value =
        serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
    assert_eq!(
        info["pid"].as_u64().map(|p| p.to_string()),
        Some(main),
        "{info}"
    );
    let out = manager
        .command("hoh")
        .args(["attach", &run_id, "--output", "json"])
        .current_dir(&project)
        .stdin(std::process::Stdio::null())
        .output()?;
    seen.insert("attach".into(), record(&out));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));

    // The host stopped as it stops normally, then the .hoh/ it leaves, then the uninstall.
    let out = manager.systemctl(&["stop", &unit])?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    let before = manifest(&project.join(".hoh"))?;
    let out = manager.install(&hoh_dir, &["--uninstall"])?;
    seen.insert("uninstall".into(), record(&out));
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    for gone in [
        command.clone(),
        manager.data().join("hoh"),
        manager.data().join("systemd/user/hoh-host@.service"),
        manager
            .home()
            .join(format!(".config/systemd/user/default.target.wants/{unit}")),
        manager
            .state()
            .join(format!("hoh/hosts/{}.env", instance(&project))),
    ] {
        assert!(
            !gone.exists() && !gone.is_symlink(),
            "{} left",
            gone.display()
        );
    }
    assert_eq!(manager.property(&unit, "LoadState")?, "not-found");
    let after = manifest(&project.join(".hoh"))?;
    seen.insert(
        "hoh_manifest".into(),
        json!({"before": before, "after": after}),
    );
    assert_eq!(after, before, "the uninstall changed the project's .hoh/");

    // No root, no sudo, and nothing of the user's own touched.
    assert!(!rustix::process::geteuid().is_root());
    assert!(
        !manager.root().join("sudo-called").exists(),
        "sudo was called"
    );
    seen.insert("manager_log".into(), json!(manager.log()));
    drop(manager);
    assert_eq!(
        real_environment()?,
        real_before,
        "the real user environment changed"
    );
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        seen.insert("expected_commit".into(), json!(commit));
        std::fs::write(
            Path::new(&dir).join("TEST-005-06.json"),
            serde_json::to_string_pretty(&Value::Object(seen))?,
        )?;
    }
    Ok(())
}
