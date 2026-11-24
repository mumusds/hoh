//! TEST-047-03 (AC-047-03, check `deployment`; docs/adr/ADR-034.md decision 12 as amended): the scenario's upgrade on
//! the release bundles `scripts/product/deployment.sh` builds. v1 (the candidate's release) is installed into a fresh
//! HOME under a private user manager (`installed`) and its unit's host runs run R, whose developer contract has a
//! project-layer override based on v1; while R runs, with a developer Attempt in flight, v2 (a release whose developer
//! builtin contract and version differ) is installed over it and the host restarted, the documented upgrade (docs/11:
//! a running host stays on its version until it restarts). R's developer Attempts created before the next checkpoint
//! record v1, those after it v2, and the override is reported drifted with its three-way diff.
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]
use crate::installed::{self, Manager, R};
use hoh_core::resource_bundle::Bundle;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The project-layer override of the developer contract, saved against v1.
const OVERRIDE: &str = "项目覆盖：开发者契约（TEST-047-03 升级夹具）\n";

/// How long the host may take for a step of the scenario.
const STEP: Duration = Duration::from_secs(90);

fn text(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Run `script` (Python, sqlite3 module) against `db`; its stdout. Test code: the product never writes outside
/// persistence.
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(stdin.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// The first column of the first row of `sql` ("" for none or NULL).
fn query(db: &Path, sql: &str) -> R<String> {
    Ok(python(
        db,
        "import sqlite3,sys; r=sqlite3.connect(sys.argv[1], timeout=10).execute(sys.stdin.read()).fetchone(); \
         print('' if r is None or r[0] is None else r[0])",
        sql,
    )?
    .trim_end_matches('\n')
    .to_owned())
}

/// Every row of `sql`, as an object of its columns.
fn rows(db: &Path, sql: &str) -> R<Vec<Value>> {
    Ok(serde_json::from_str(&python(
        db,
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1], timeout=10); c.row_factory=sqlite3.Row; \
         print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?)?)
}

/// Run `sql` on the store, committed.
fn exec(db: &Path, sql: &str) -> R {
    python(
        db,
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); c.executescript(sys.stdin.read()); \
         c.commit(); c.close()",
        sql,
    )
    .map(drop)
}

/// Wait until `ready` holds, polling every 100 ms, at most `limit`.
fn until(what: &str, limit: Duration, mut ready: impl FnMut() -> R<bool>) -> R {
    let end = Instant::now() + limit;
    loop {
        if ready()? {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!("{what}: not within {limit:?}").into());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// AC-047-03's three-way diff (ADR-034 decision 10: old builtin -> new builtin, old builtin -> override), checked
/// without the product's diff code (M3PD-01): `report` is the drift report's `diff`, and each of its two parts must be a
/// unified diff with the expected headers that, applied to the old builtin, gives the new builtin or the override.
pub fn assert_three_way_diff(report: &Value, old: &str, new: &str, custom: &str) -> R {
    for (part, to, want) in [
        ("builtin", "new-builtin", new),
        ("override", "override", custom),
    ] {
        let diff = report[part]
            .as_str()
            .ok_or(format!("three-way diff has no {part} part: {report}"))?;
        let got = apply_unified(diff, "old-builtin", to, old)
            .map_err(|why| format!("{part} part: {why}\n{diff}"))?;
        if got != want {
            return Err(
                format!("{part} part applied to the old builtin is not its {to}:\n{diff}").into(),
            );
        }
    }
    Ok(())
}

/// `diff` applied to `old`: a unified diff `--- from` / `+++ to` with at least one hunk, whose hunk headers count its
/// lines and whose context and '-' lines are `old`'s lines at the hunk's position. Every text ends with a newline.
fn apply_unified(diff: &str, from: &str, to: &str, old: &str) -> R<String> {
    let old: Vec<&str> = old.split_inclusive('\n').collect();
    let mut lines = diff.split_inclusive('\n').peekable();
    if lines.next() != Some(&format!("--- {from}\n"))
        || lines.next() != Some(&format!("+++ {to}\n"))
    {
        return Err(format!("headers are not `--- {from}` / `+++ {to}`").into());
    }
    let (mut out, mut at, mut hunks) = (Vec::<&str>::new(), 0usize, 0);
    while let Some(header) = lines.next() {
        let range = |side: &str| -> R<(usize, usize)> {
            let (start, count) = side.split_once(',').unwrap_or((side, "1"));
            Ok((start.parse()?, count.parse()?))
        };
        let (old_range, new_range) = header
            .strip_prefix("@@ -")
            .and_then(|h| h.strip_suffix(" @@\n"))
            .and_then(|h| h.split_once(" +"))
            .ok_or(format!("not a hunk header: {header:?}"))?;
        let ((old_start, old_count), (new_start, new_count)) =
            (range(old_range)?, range(new_range)?);
        // A hunk starts at its first old line (an empty one after line `old_start`); lines before it are unchanged.
        let first = if old_count == 0 {
            old_start
        } else {
            old_start.checked_sub(1).ok_or("old start 0")?
        };
        if first < at || first > old.len() {
            return Err(format!("hunk {header:?} out of order or past the old text").into());
        }
        out.extend(&old[at..first]);
        at = first;
        if (if new_count == 0 {
            new_start
        } else {
            new_start.wrapping_sub(1)
        }) != out.len()
        {
            return Err(
                format!("hunk {header:?} new start does not follow the lines before it").into(),
            );
        }
        let (mut seen_old, mut seen_new) = (0, 0);
        while let Some(line) = lines.next_if(|l| !l.starts_with("@@")) {
            let (kind, text) = line.split_at(1);
            if matches!(kind, " " | "-") {
                if old.get(at) != Some(&text) {
                    return Err(format!("line {line:?} is not old line {}", at + 1).into());
                }
                at += 1;
                seen_old += 1;
            }
            match kind {
                " " | "+" => {
                    out.push(text);
                    seen_new += 1;
                }
                "-" => {}
                _ => return Err(format!("unexpected diff line {line:?}").into()),
            }
        }
        if (seen_old, seen_new) != (old_count, new_count) {
            return Err(
                format!("hunk {header:?} has {seen_old} old and {seen_new} new lines").into(),
            );
        }
        hunks += 1;
    }
    if hunks == 0 {
        return Err("no hunk".into());
    }
    out.extend(&old[at..]);
    Ok(out.concat())
}

/// The release bundle `bundle` unpacked under `at`: its `hoh/` directory.
fn unpack(bundle: &Path, at: &Path) -> R<PathBuf> {
    std::fs::create_dir_all(at)?;
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(bundle)
        .arg("-C")
        .arg(at)
        .status()?;
    if !status.success() {
        return Err(format!("tar -xzf {}", bundle.display()).into());
    }
    Ok(at.join("hoh"))
}

/// The version a bundle's binary names (`hoh <semver>`, the first line of `--version`).
fn semver(hoh: &Path) -> R<String> {
    let out = Command::new(hoh.join("bin/hoh"))
        .arg("--version")
        .output()?;
    assert_eq!(out.status.code(), Some(0), "{}", text(&out));
    Ok(String::from_utf8(out.stdout)?
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("hoh "))
        .ok_or("no version line")?
        .to_owned())
}

/// The resource bundle an unpacked release's binary holds: its version, and the files `Bundle::current` embeds read
/// from the release's resources/. The output schemas it also embeds (contracts/, not in a release bundle) are this
/// build's: deployment.sh's v2 changes only the developer contract and the version.
fn release_bundle(hoh: &Path) -> R<Bundle> {
    let mut bundle = Bundle::current();
    bundle.release = semver(hoh)?;
    for (path, text) in &mut bundle.files {
        if path.starts_with("resources/") {
            *text = std::fs::read_to_string(hoh.join(path))?;
        } else {
            assert!(path.starts_with("contracts/"), "{path}");
        }
    }
    Ok(bundle)
}

/// The digest of the executable process `pid` runs (its `/proc/<pid>/exe`).
fn running_image(pid: &str) -> R<String> {
    Ok(installed::sha256(&std::fs::read(format!(
        "/proc/{pid}/exe"
    ))?))
}

/// The fake claude (2.1.318 by its recorded probe answers; `HOH_ATTEMPT_ID` is its attempt's marker, ending in the
/// attempt id): a turn of T2's first attempt replays the recorded
/// `success` turn's first frame and then waits for a file that never appears (it is in flight when v2 is installed and
/// ends with its host's unit); every other turn writes its attempt id to `claude.d/completed` and replays `success`.
fn unit_claude(bin: &Path) -> R {
    let protocol = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fixtures/protocol");
    std::fs::create_dir_all(bin.join("claude.d"))?;
    installed::script(
        &bin.join("claude"),
        &format!(
            "#!/bin/bash\nf='{protocol}/claude/2.1.318'; d='{bin}/claude.d'\n\
             case \"$1\" in --version) echo '2.1.318 (Claude Code)'; exit 0;; --help) cat \"$f/help.txt\"; exit 0;; esac\n\
             cat > \"$d/stdin.$$\"\n\
             case \"$HOH_ATTEMPT_ID\" in\n\
              T2/attempt-1|*/T2/attempt-1) echo $$ > \"$d/t2.pid\"; head -n 1 \"$f/success/stdout.jsonl\"; while [ ! -e \"$d/never\" ]; do sleep 0.1; done;;\n\
             esac\n\
             echo \"$HOH_ATTEMPT_ID\" >> \"$d/completed\"\n\
             cat \"$f/success/stdout.jsonl\"\n",
            bin = bin.display()
        ),
    )
}

/// Bind every built-in role whose agent is not `agent` to it, in the project's `.hoh/config.toml`.
fn team_on(project: &Path, agent: &str) -> R {
    std::fs::create_dir_all(project.join(".hoh"))?;
    let mut text = "config_version = \"1.0\"\n".to_owned();
    for (role, binding) in hoh_core::config::Config::default().team {
        if binding.agent.as_str() != agent {
            text.push_str(&format!("[team.{role}]\nagent = \"{agent}\"\n"));
        }
    }
    std::fs::write(project.join(".hoh/config.toml"), text)?;
    Ok(())
}

/// R's state as the check's evidence and a failure's diagnosis: its developer pins, attempts and drift events.
const STATE: &str = r#"
import json, sqlite3, sys
c = sqlite3.connect(sys.argv[1], timeout=10)
c.row_factory = sqlite3.Row
rows = lambda sql: [dict(r) for r in c.execute(sql)]
print(json.dumps({
    "runs": rows("SELECT run_id, state FROM runs"),
    "tasks": rows("SELECT task_id, state, role FROM tasks"),
    "attempts": rows("SELECT attempt_id, task_id, state, host_epoch, resource_bundle_version, failure_reason FROM attempts ORDER BY rowid"),
    "events": [dict(e, payload=json.loads(e["payload_json"])) for e in rows("SELECT seq, event_id, kind, run_id, payload_json FROM events WHERE kind IN ('resource.pinned', 'resource.drift', 'schedule.round', 'attempt.moved', 'attempt.start_refused', 'host.started') ORDER BY seq")],
}))
"#;

#[test]
#[ignore = "runs in the deployment check (AC-047-03); needs the v1 and v2 release bundles: scripts/product/deployment.sh"]
fn test_047_03_an_installed_v1_upgraded_to_v2_under_a_running_run_pins_until_its_checkpoint() -> R {
    let bundle = |var: &str| -> R<PathBuf> {
        Ok(PathBuf::from(std::env::var_os(var).ok_or(format!(
            "{var} is not set: no release bundle to install"
        ))?))
    };
    let (v1_bundle, v2_bundle) = (
        bundle("HOH_RELEASE_BUNDLE")?,
        bundle("HOH_RELEASE_BUNDLE_V2")?,
    );
    let manager = Manager::start()?;
    let root = manager.root().to_path_buf();
    let mut seen = serde_json::Map::new();
    let project = root.join("p");
    let db = project.join(".hoh/state.db");
    let outcome = (|| -> R {
        let v1 = unpack(&v1_bundle, &root.join("v1"))?;
        let v2 = unpack(&v2_bundle, &root.join("v2"))?;
        let (old_bundle, new_bundle) = (release_bundle(&v1)?, release_bundle(&v2)?);
        let (old_version, new_version) = (old_bundle.version(), new_bundle.version());
        let old_builtin = old_bundle
            .text("resources/prompts/developer.md")?
            .to_owned();
        let new_builtin = new_bundle
            .text("resources/prompts/developer.md")?
            .to_owned();
        // v2 is another release whose developer builtin contract changed.
        assert_ne!(old_bundle.release, new_bundle.release);
        assert_ne!(old_builtin, new_builtin);
        assert_ne!(old_version, new_version);
        // v1 is this candidate's release: the bundle this test was built with.
        assert_eq!(old_version, Bundle::current().version());
        seen.insert(
            "bundles".into(),
            json!({"v1": {"release": old_bundle.release, "resource_bundle_version": old_version},
                   "v2": {"release": new_bundle.release, "resource_bundle_version": new_version}}),
        );

        // A Git project, v1 installed with its host unit enabled, and the fake agent the unit's host finds.
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
        let project = std::fs::canonicalize(&project)?;
        let unit = hoh_app::unit::unit_name(&hoh_app::unit::instance(&project));
        let out = manager.install(&v1, &["--project", project.to_str().ok_or("path")?])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        unit_claude(&manager.bin())?;
        team_on(&project, "claude")?;
        let tools = root.join("tools");
        std::fs::create_dir(&tools)?;
        for tool in ["cat", "head", "sleep", "python3", "systemctl", "git"] {
            let found = ["/usr/bin", "/bin"]
                .iter()
                .map(|d| Path::new(d).join(tool))
                .find(|p| p.exists())
                .ok_or(format!("no {tool}"))?;
            std::os::unix::fs::symlink(found, tools.join(tool))?;
        }
        let path = format!(
            "{}:{}:{}",
            manager.bin().display(),
            tools.display(),
            manager.home().join(".local/bin").display()
        );
        let hoh = |args: &[&str]| {
            manager
                .command(manager.home().join(".local/bin/hoh"))
                .env("PATH", &path)
                .args(args)
                .current_dir(&project)
                .stdin(Stdio::null())
                .output()
        };
        let detached = |goal: &str| -> R<String> {
            let out = hoh(&["-p", goal, "--detach", "--output", "json"])?;
            assert_eq!(out.status.code(), Some(0), "{}", text(&out));
            Ok(serde_json::from_slice::<Value>(&out.stdout)?["run_id"]
                .as_str()
                .ok_or("no run_id")?
                .to_owned())
        };
        let host_pid = || -> R<String> {
            let info: Value =
                serde_json::from_str(&std::fs::read_to_string(project.join(".hoh/host.json"))?)?;
            Ok(info["pid"].as_u64().ok_or("no pid")?.to_string())
        };
        // A first run creates the host's store; it has no specification (BLOCKED on runtime:no_spec). The project's
        // specification is approved after it, then the developer override is saved against v1's builtin, then R.
        detached("规格之前")?;
        exec(
            &db,
            "INSERT INTO specs(spec_id, project_id, sha256, revision) \
             SELECT 's1', project_id, printf('%064d', 1), 1 FROM projects;",
        )?;
        hoh_core::prompt_files::save_override(&project, ".hoh/prompts/developer.md", OVERRIDE)?;
        let base: Value = serde_json::from_str(&std::fs::read_to_string(
            project.join(".hoh/prompts/developer.md.base.json"),
        )?)?;
        assert_eq!(base["resource_bundle_version"], json!(old_version));
        assert_eq!(base["builtin"], json!(old_builtin));
        let run = detached("资源升级")?;
        let pinned = format!(
            "SELECT json_extract(payload_json, '$.resource_bundle_version') FROM events WHERE kind = 'resource.pinned' \
             AND run_id = '{run}' AND json_extract(payload_json, '$.role') = 'developer' ORDER BY seq DESC LIMIT 1"
        );
        // R started with resource bundle v1.
        assert_eq!(query(&db, &pinned)?, old_version);
        // R RUNNING with T1 and T2 READY for the developer.
        exec(
            &db,
            &format!(
                "UPDATE runs SET state = 'RUNNING', spec_id = 's1' WHERE run_id = '{run}';
                 INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T1', '{run}', 's1', 'READY', 1, 'developer', 0, 1, 1), \
                        ('T2', '{run}', 's1', 'READY', 1, 'developer', 0, 2, 1);"
            ),
        )?;
        let completed = manager.bin().join("claude.d/completed");
        let t2_pid = manager.bin().join("claude.d/t2.pid");
        until("T1's attempt to end and T2's to run under v1", STEP, || {
            Ok(std::fs::read_to_string(&completed).is_ok_and(|c| {
                c.lines()
                    .any(|l| l == "T1/attempt-1" || l.ends_with("/T1/attempt-1"))
            }) && t2_pid.exists()
                && query(
                    &db,
                    "SELECT state FROM attempts WHERE attempt_id = 'T2/attempt-1'",
                )? == "RUNNING")
        })?;
        let old_pid = host_pid()?;
        assert_eq!(
            running_image(&old_pid)?,
            installed::sha256(&std::fs::read(v1.join("bin/hoh"))?)
        );

        // While R runs (T2's attempt in flight), v2 is installed over v1. The running host stays on v1 until its
        // unit restarts it, which it then does on the installed command, now v2's (docs/11).
        let out = manager.install(&v2, &[])?;
        seen.insert("install_v2".into(), json!(text(&out)));
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        assert_eq!(host_pid()?, old_pid);
        assert_eq!(
            running_image(&old_pid)?,
            installed::sha256(&std::fs::read(v1.join("bin/hoh"))?)
        );
        assert_eq!(
            query(
                &db,
                &format!("SELECT state FROM runs WHERE run_id = '{run}'")
            )?,
            "RUNNING"
        );
        assert_eq!(
            query(
                &db,
                "SELECT state FROM attempts WHERE attempt_id = 'T2/attempt-1'"
            )?,
            "RUNNING"
        );
        let out = manager.systemctl(&["restart", &unit])?;
        assert_eq!(out.status.code(), Some(0), "{}", text(&out));
        let v2_image = installed::sha256(&std::fs::read(v2.join("bin/hoh"))?);
        let mut new_pid = String::new();
        until("the unit's host to run v2", STEP, || {
            new_pid = host_pid().unwrap_or_default();
            Ok(!new_pid.is_empty()
                && new_pid != old_pid
                && running_image(&new_pid).is_ok_and(|image| image == v2_image))
        })?;
        // Its next checkpoint moves R's developer to v2 once no developer Attempt is in flight; a task the Commander
        // plans after it runs on v2.
        until("R's developer pin to move to v2", STEP, || {
            Ok(query(&db, &pinned)? == new_version)
        })?;
        exec(
            &db,
            &format!(
                "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) \
                 VALUES ('T3', '{run}', 's1', 'READY', 1, 'developer', 0, 3, 1);"
            ),
        )?;
        until("T3's attempt to be packed", STEP, || {
            Ok(query(
                &db,
                "SELECT count(*) FROM events WHERE event_id = 'T3/attempt-1/context.packed'",
            )? == "1")
        })?;

        // Then: every developer Attempt of R created before the checkpoint records v1, every one after it v2; T1's
        // and T2's first attempts are before it (T2's ended with v1's host and was not relabelled), T3's after. An
        // attempt is created with the event of its dispatch (`schedule.round` here, `decide::lease_task`), the first
        // that names it.
        let checkpoint: i64 = query(
            &db,
            &format!(
                "SELECT min(seq) FROM events WHERE kind = 'resource.pinned' AND run_id = '{run}' \
                 AND json_extract(payload_json, '$.role') = 'developer' \
                 AND json_extract(payload_json, '$.resource_bundle_version') = '{new_version}'"
            ),
        )?
        .parse()?;
        let attempts = rows(
            &db,
            &format!(
                "SELECT a.attempt_id, a.resource_bundle_version, a.state, \
                 (SELECT min(e.seq) FROM events e WHERE json_extract(e.payload_json, '$.attempt_id') = a.attempt_id \
                  OR substr(e.event_id, 1, length(a.attempt_id) + 1) = a.attempt_id || '/') AS created \
                 FROM attempts a JOIN tasks t USING(task_id) \
                 WHERE t.run_id = '{run}' AND t.role = 'developer' ORDER BY created"
            ),
        )?;
        seen.insert("checkpoint_seq".into(), json!(checkpoint));
        seen.insert("attempts".into(), json!(attempts));
        for attempt in &attempts {
            let created = attempt["created"].as_i64().ok_or("no dispatch event")?;
            let want = if created < checkpoint {
                &old_version
            } else {
                &new_version
            };
            assert_eq!(attempt["resource_bundle_version"], json!(want), "{attempt}");
        }
        let created = |id: &str| {
            attempts
                .iter()
                .find(|a| a["attempt_id"] == id)
                .and_then(|a| a["created"].as_i64())
        };
        assert!(created("T1/attempt-1").is_some_and(|s| s < checkpoint));
        assert!(created("T2/attempt-1").is_some_and(|s| s < checkpoint));
        assert!(created("T3/attempt-1").is_some_and(|s| s > checkpoint));
        let version = |id: &str| -> R<String> {
            query(
                &db,
                &format!("SELECT resource_bundle_version FROM attempts WHERE attempt_id = '{id}'"),
            )
        };
        assert_eq!(version("T1/attempt-1")?, old_version);
        assert_eq!(version("T2/attempt-1")?, old_version);
        assert_ne!(
            query(
                &db,
                "SELECT state FROM attempts WHERE attempt_id = 'T2/attempt-1'"
            )?,
            "RUNNING"
        );
        assert_eq!(version("T3/attempt-1")?, new_version);
        let builtin_of = |id: &str| -> R<Value> {
            let packed: Value = serde_json::from_str(&query(
                &db,
                &format!("SELECT payload_json FROM events WHERE event_id = '{id}/context.packed'"),
            )?)?;
            let first = &packed["override_chain"][0];
            assert_eq!(first["layer"], "builtin", "{packed}");
            Ok(first["sha256"].clone())
        };
        assert_eq!(
            builtin_of("T1/attempt-1")?,
            json!(installed::sha256(old_builtin.as_bytes()))
        );
        assert_eq!(
            builtin_of("T3/attempt-1")?,
            json!(installed::sha256(new_builtin.as_bytes()))
        );
        // v1's host found no drift (the override's base is its builtin); v2's marks the override drifted with the
        // three-way diff of v1's builtin, v2's builtin and the override.
        assert_eq!(
            query(
                &db,
                "SELECT count(*) FROM events WHERE event_id = 'T1/attempt-1/resource.drift'"
            )?,
            "0"
        );
        let drift: Value = serde_json::from_str(&query(
            &db,
            "SELECT payload_json FROM events WHERE event_id = 'T3/attempt-1/resource.drift'",
        )?)?;
        seen.insert("drift".into(), drift.clone());
        assert_eq!(drift["role"], "developer", "{drift}");
        let refs = drift["reports"].as_array().ok_or("no reports")?;
        assert_eq!(refs.len(), 1, "{drift}");
        assert_eq!(refs[0]["layer"], "project", "{drift}");
        assert_eq!(refs[0]["base_status"], "known", "{drift}");
        let report: Value = serde_json::from_slice(&hoh_core::objects::get(
            &project,
            refs[0]["sha256"].as_str().ok_or("no report")?,
        )?)?;
        seen.insert("report".into(), report.clone());
        assert_eq!(report["base_resource_bundle_version"], json!(old_version));
        assert_eq!(report["resource_bundle_version"], json!(new_version));
        assert_three_way_diff(&report["diff"], &old_builtin, &new_builtin, OVERRIDE)?;
        Ok(())
    })();
    seen.insert(
        "state".into(),
        python(&db, STATE, "").map_or_else(
            |e| json!(e.to_string()),
            |s| serde_json::from_str(&s).unwrap_or_else(|_| json!(s)),
        ),
    );
    if let Err(why) = &outcome {
        eprintln!(
            "TEST-047-03 upgrade: {why}\n{}",
            serde_json::to_string_pretty(&seen)?
        );
        eprintln!("manager log:\n{}", manager.log());
    }
    drop(manager);
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join("TEST-047-03-upgrade.json"),
            serde_json::to_string_pretty(&Value::Object(seen))?,
        )?;
    }
    outcome
}
