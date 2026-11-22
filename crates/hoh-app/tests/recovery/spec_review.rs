//! A specification review's attempt under kill -9 (REQ-031 behaviour 3, docs/adr/ADR-035.md decision 7): a host of the
//! world's scripted Team (a fake reviewer: never reported as live) runs the review of revision 1 in a process of its
//! own and dies by SIGKILL at one point of the review's end; the next host's start reconciles it. Each kill point ends
//! in exactly one of two ways: the report taken in (the revision reviewed once), or the review task ended without a
//! report and the revision reviewed again under a new review id, once.

use super::world::{self, Verdict, World};
use super::{R, Value, json};
use hoh_app::host::HostConfig;
use hoh_core::persistence::{FaultHooks, WritePoint};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The world's root: the child runs a host of its project whose runner is the reviewing Team.
const CHILD_ROOT: &str = "HOH_TEST_REVIEW_ROOT";
/// Where the child kills itself: `runner:AfterEffectObserved` (the review attempt's work observed done, nothing of its
/// end written), `writer:Begun`, `writer:BeforeCommit` or `writer:Committed` (of the review end's writer job).
const CHILD_KILL: &str = "HOH_TEST_REVIEW_KILL";

/// The project's name in the world.
const PROJECT: &str = "p";

/// How long the test waits for a host to get where it looks.
const WAIT: Duration = Duration::from_secs(90);

fn kill() -> ! {
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
    loop {
        std::thread::park();
    }
}

/// SIGKILL when a reviewer's attempt reaches the runner's point.
#[derive(Debug)]
struct KillAtReview(PathBuf, hoh_app::host::RunnerPoint);

impl hoh_app::host::HostHooks for KillAtReview {
    fn runner_of(&self, point: hoh_app::host::RunnerPoint, attempt_id: &str) {
        if point != self.1 {
            return;
        }
        let role = super::query(
            &self.0,
            &format!(
                "SELECT t.role FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE a.attempt_id = '{attempt_id}'"
            ),
        );
        if role.is_ok_and(|r| r == "reviewer") {
            kill();
        }
    }
}

/// SIGKILL at a point of the review end's writer job (`FinishedReview`): its start, just before its commit (all of it
/// written), or after its commit.
#[derive(Debug)]
struct KillAtReviewEnd {
    point: &'static str,
    current: std::sync::Mutex<&'static str>,
}

impl FaultHooks for KillAtReviewEnd {
    fn at(&self, job: &'static str, point: WritePoint) {
        if point == WritePoint::Begun
            && let Ok(mut current) = self.current.lock()
        {
            *current = job;
        }
        let at = match point {
            WritePoint::Begun => "Begun",
            WritePoint::Committed => "Committed",
            _ => return,
        };
        if job == "FinishedReview" && at == self.point {
            kill();
        }
    }

    fn before_commit(&self) -> Result<(), hoh_core::persistence::PersistError> {
        if self.point == "BeforeCommit" && self.current.lock().is_ok_and(|c| *c == "FinishedReview")
        {
            kill();
        }
        Ok(())
    }
}

/// A host of the world at `root` whose runner is the reviewing Team (every review a PASS), as `World::team_host`
/// builds it.
fn team_config(root: &Path) -> HostConfig {
    let project = root.join(PROJECT);
    let team = world::Team::reviewing(|_| Vec::new(), project.clone(), |_| Verdict::Pass);
    let mut config = HostConfig::new(&project);
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.runner = Some(hoh_app::host::RunnerConfig {
        executor: team,
        context: hoh_core::decision::Context {
            required_capabilities: world::TEAM
                .iter()
                .map(|(role, _)| ((*role).to_owned(), Default::default()))
                .collect(),
            commander_dir: Some(root.join("state/hoh/commander")),
            ..hoh_core::decision::Context::default()
        },
    });
    config
}

/// The child entry: the reviewing Team's host of the world at [`CHILD_ROOT`], killed at [`CHILD_KILL`].
#[test]
#[ignore = "run by the review recovery tests in a process of their own"]
fn review_host() -> R {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return Ok(());
    };
    let mut config = team_config(&root);
    let kill = std::env::var(CHILD_KILL)?;
    match kill.split_once(':').ok_or("<where>:<point>")? {
        ("runner", "AfterEffectObserved") => {
            config.hooks = std::sync::Arc::new(KillAtReview(
                root.join(PROJECT).join(".hoh/state.db"),
                hoh_app::host::RunnerPoint::AfterEffectObserved,
            ));
        }
        ("writer", point @ ("Begun" | "BeforeCommit" | "Committed")) => {
            let point = match point {
                "Begun" => "Begun",
                "BeforeCommit" => "BeforeCommit",
                _ => "Committed",
            };
            config.store.faults = std::sync::Arc::new(KillAtReviewEnd {
                point,
                current: std::sync::Mutex::new(""),
            });
        }
        // Killed by the fake git of [`killed_in_checkout`] while it makes the review's checkout: no hook here.
        ("git", "clone") => {}
        other => return Err(format!("no kill point {other:?}").into()),
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(hoh_app::host::run(config));
    Err("the review host was not killed".into())
}

/// The specification: REQ-1 with one criterion (ready but for its review).
const SPEC: &str = "# 用户\n记账用户\n# 范围\n记账\n# 非目标\n无\n# 决策\n无\n# 开放问题\n无\n# 需求\n## REQ-1 记一笔账\n\
                    ### AC-1-01\ngiven: 空账本\nwhen: 记一笔\nthen: 记下\ncheck: check-1\n";

/// The specification committed with its generated index: its digest.
fn commit_spec(world: &World) -> R<String> {
    let source = hoh_core::specindex::Source {
        path: "docs/spec/spec.md".to_owned(),
        bytes: SPEC.as_bytes().to_vec(),
    };
    let index = hoh_core::specindex::generate_index(std::slice::from_ref(&source))?;
    std::fs::create_dir_all(world.project().join("docs/spec"))?;
    for entry in std::fs::read_dir(world.project().join("docs/spec"))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "md") {
            std::fs::remove_file(path)?;
        }
    }
    std::fs::write(world.project().join(&source.path), &source.bytes)?;
    std::fs::write(
        world.project().join(hoh_core::specindex::PATH),
        index.bytes(),
    )?;
    world.git(&["add", "-A", "docs/spec"])?;
    world.git(&["commit", "-qm", "specification"])?;
    Ok(index.sha256())
}

/// Revision 1 approved (on a run of its own) and run R started on it, staffed and in SPECIFYING; R's id.
fn given(world: &World) -> R<String> {
    world.configure(&[], "")?;
    world.commit_project(&[])?;
    let sha256 = commit_spec(world)?;
    let runtime = tokio::runtime::Runtime::new()?;
    let run = runtime.block_on(async {
        let host = world.start_plain_host(None).await?;
        let outcome = async {
            let first = world::start_run(&host, "first", "批准修订 1").await?;
            let (_, got) = world::exchange(&host, "GET", &format!("/v1/runs/{first}"), None).await?;
            let body = json!({"schema_version": "1.0", "command_id": "c-approve-1", "idempotency_key": "approve-1",
                "expected_revision": got["revision"], "run_id": first, "kind": "ApproveSpec",
                "payload": {"spec_sha256": sha256, "decision": "APPROVE_ONLY"}});
            let (status, reply) = world::exchange(&host, "POST", "/v1/commands", Some(&body)).await?;
            if status != 202 {
                return Err(format!("ApproveSpec: {status} {reply}").into());
            }
            world::start_run(&host, "r", "做一个记账工具").await
        }
        .await;
        host.shutdown().await;
        outcome
    })?;
    world.staff(&run)?;
    world.exec(&format!(
        "UPDATE runs SET state = 'SPECIFYING', revision = revision + 1, blocked_reason = NULL, \
         blocked_reasons_json = '[]', resume_state = NULL WHERE run_id = '{run}';"
    ))?;
    Ok(run)
}

/// The child host killed at `kill`: its stderr.
fn killed_at(world: &World, kill: &str) -> R<String> {
    killed_with(world, kill, &[])
}

/// [`killed_at`] with `env` added to the child's environment.
fn killed_with(world: &World, kill: &str, env: &[(&str, std::ffi::OsString)]) -> R<String> {
    let mut command = std::process::Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            "recovery::spec_review::review_host",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .envs(world.env())
        .env(CHILD_ROOT, world.root())
        .env(CHILD_KILL, kill)
        .envs(env.iter().map(|(k, v)| (*k, v)))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let child = super::ChildGuard(command.spawn()?);
    let (status, stderr) = super::wait(child, WAIT)?;
    use std::os::unix::process::ExitStatusExt as _;
    if status.signal() != Some(9) {
        return Err(format!("the review host at {kill} ended {status:?}: {stderr}").into());
    }
    Ok(stderr)
}

/// The payloads of the events of `kind`, in order.
fn events(world: &World, kind: &str) -> R<Vec<Value>> {
    world
        .rows(&format!(
            "SELECT payload_json FROM events WHERE kind = '{kind}' ORDER BY seq"
        ))?
        .iter()
        .map(|r| {
            Ok(serde_json::from_str(
                r["payload_json"].as_str().ok_or("payload")?,
            )?)
        })
        .collect()
}

#[test]
fn test_031_04_a_review_killed_at_each_point_of_its_end_is_taken_in_or_reopened_once() -> R {
    let mut kept = Vec::new();
    for (kill, reopened) in [
        ("runner:AfterEffectObserved", true),
        ("writer:Begun", true),
        ("writer:BeforeCommit", true),
        ("writer:Committed", false),
    ] {
        let world = World::named(PROJECT)?;
        let run = given(&world)?;
        killed_at(&world, kill)?;
        let before = world.rows(
            "SELECT a.attempt_id, a.state, t.state AS task FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
             WHERE t.role = 'reviewer'",
        )?;
        assert_eq!(before.len(), 1, "{kill}: {before:?}");
        let runtime = tokio::runtime::Runtime::new()?;
        runtime.block_on(async {
            let host = hoh_app::host::Host::start(team_config(world.root())).await?;
            let outcome = async {
                let end = Instant::now() + WAIT;
                let ready = format!("SELECT state FROM runs WHERE run_id = '{run}'");
                while world.query(&ready)? != "READY" {
                    if Instant::now() > end {
                        return Err(format!(
                            "{kill}: R not READY: {:?} {:?}",
                            world
                                .rows("SELECT task_id, state FROM tasks WHERE role = 'reviewer'")?,
                            events(&world, "spec_review.opened")?
                        )
                        .into());
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                // Nothing more is opened once the revision has its CURRENT report.
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<_, Box<dyn std::error::Error>>(())
            }
            .await;
            host.shutdown().await;
            outcome
        })?;
        let (opened, reported, ended) = (
            events(&world, "spec_review.opened")?,
            events(&world, "spec_review.reported")?,
            events(&world, "spec_review.ended")?,
        );
        assert_eq!(
            opened.len(),
            if reopened { 2 } else { 1 },
            "{kill}: {opened:?}"
        );
        assert_eq!(reported.len(), 1, "{kill}: {reported:?}");
        assert_eq!(reported[0]["state"], "CURRENT", "{kill}: {reported:?}");
        assert_eq!(
            reported[0]["review_id"],
            opened[opened.len() - 1]["review_id"],
            "{kill}"
        );
        let first_task = opened[0]["task_id"].as_str().ok_or("task")?;
        assert_eq!(
            world.query(&format!(
                "SELECT state FROM tasks WHERE task_id = '{first_task}'"
            ))?,
            "ACCEPTED",
            "{kill}"
        );
        if reopened {
            assert_eq!(ended.len(), 1, "{kill}: {ended:?}");
            assert_eq!(ended[0]["review_id"], opened[0]["review_id"], "{kill}");
            assert_eq!(ended[0]["attempt_id"], before[0]["attempt_id"], "{kill}");
            assert_ne!(opened[1]["review_id"], opened[0]["review_id"], "{kill}");
        } else {
            assert!(ended.is_empty(), "{kill}: {ended:?}");
        }
        kept.push(
            json!({"kill": kill, "before_restart": before, "opened": opened, "reported": reported,
            "ended": ended}),
        );
    }
    super::keep("test-031-04-review-kill.json", &json!(kept))
}

/// A process as `/proc/<pid>/stat` names it: its pid and start time (field 22), if it is there and not a zombie.
fn alive(pid: u32) -> Option<(u32, u64)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = &stat[stat.rfind(") ")? + 2..];
    let fields: Vec<&str> = rest.split(' ').collect();
    if matches!(fields.first(), Some(&("Z" | "X"))) {
        return None;
    }
    Some((pid, fields.get(19)?.parse().ok()?))
}

/// Ends process `pid` started at `start` (this test's own fake git), if it is still that process.
struct EndFake(Option<(u32, u64)>);

impl Drop for EndFake {
    fn drop(&mut self) {
        if let Some((pid, start)) = self.0
            && alive(pid).is_some_and(|(_, s)| s == start)
            && let Some(pid) = i32::try_from(pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

/// The program `name` on the test's own PATH.
fn on_path(name: &str) -> R<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").ok_or("no PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| format!("no {name} on the PATH").into())
}

#[test]
fn test_031_04_a_host_killed_while_git_makes_a_reviews_checkout_leaves_no_git_and_no_directory_in_use()
-> R {
    // The Grok finding of the S5-S7 review: the review's checkout is made by git in a process group of its own while
    // its attempt is STARTING with no pid. The host dies by SIGKILL while git works in the checkout (a fake git: the
    // first shared clone writes its pid and target, enters the target and kills its parent, the host, then sleeps
    // there). The next host's start must stop that git before the attempt ends and its directory is removed: after
    // it, the git is gone, the directory is gone, and the revision is reviewed (R READY on a CURRENT report).
    let world = World::named(PROJECT)?;
    let run = given(&world)?;
    let fake = world.root().join("fake-git");
    std::fs::create_dir_all(&fake)?;
    let (flag, pid_file, target_file) =
        (fake.join("killed"), fake.join("pid"), fake.join("target"));
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1 $2 $3 $4\" = \"--no-optional-locks -c core.fsmonitor=false clone\" ] && [ ! -e '{flag}' ]; then\n\
         \x20 : > '{flag}'\n\
         \x20 for target; do :; done\n\
         \x20 mkdir -p \"$target\" && cd \"$target\" || exit 1\n\
         \x20 echo \"$target\" > '{target_file}'\n\
         \x20 echo $$ > '{pid_file}'\n\
         \x20 kill -9 $PPID\n\
         \x20 exec '{sleep}' 120\n\
         fi\n\
         exec '{git}' \"$@\"\n",
        flag = flag.display(),
        target_file = target_file.display(),
        pid_file = pid_file.display(),
        sleep = on_path("sleep")?.display(),
        git = world.tools().join("git").display(),
    );
    std::fs::write(fake.join("git"), script)?;
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(fake.join("git"), std::fs::Permissions::from_mode(0o755))?;
    let mut path = fake.clone().into_os_string();
    path.push(":");
    path.push(world.path());
    killed_with(&world, "git:clone", &[("PATH", path)])?;
    let pid: u32 = std::fs::read_to_string(&pid_file)?.trim().parse()?;
    let target = PathBuf::from(std::fs::read_to_string(&target_file)?.trim());
    // The scenario: the git outlived its host, in the review's checkout, its attempt STARTING with no pid.
    let git = alive(pid).ok_or("the fake git did not outlive its host")?;
    let _end = EndFake(Some(git));
    assert_eq!(std::fs::read_link(format!("/proc/{pid}/cwd"))?, target);
    let before = world.rows(
        "SELECT a.attempt_id, a.state, a.pid FROM attempts a JOIN tasks t ON t.task_id = a.task_id \
         WHERE t.role = 'reviewer'",
    )?;
    assert_eq!(before.len(), 1, "{before:?}");
    assert_eq!(before[0]["state"], "STARTING", "{before:?}");
    assert_eq!(before[0]["pid"], Value::Null, "{before:?}");
    assert!(
        target.ends_with(
            before[0]["attempt_id"]
                .as_str()
                .ok_or("attempt")?
                .replace('%', "%25")
                .replace('/', "%2F")
        ),
        "{target:?} {before:?}"
    );
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let host = hoh_app::host::Host::start(team_config(world.root())).await?;
        let outcome = async {
            let end = Instant::now() + WAIT;
            let ready = format!("SELECT state FROM runs WHERE run_id = '{run}'");
            while world.query(&ready)? != "READY" {
                if Instant::now() > end {
                    return Err(format!(
                        "R not READY: {:?}",
                        world.rows("SELECT task_id, state FROM tasks WHERE role = 'reviewer'")?
                    )
                    .into());
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;
        host.shutdown().await;
        outcome
    })?;
    assert_eq!(
        alive(pid).filter(|&(_, start)| start == git.1),
        None,
        "the git of the killed host's checkout still runs"
    );
    assert!(
        std::fs::symlink_metadata(&target).is_err(),
        "{target:?} is left"
    );
    let reported = events(&world, "spec_review.reported")?;
    assert_eq!(reported.len(), 1, "{reported:?}");
    assert_eq!(reported[0]["state"], "CURRENT", "{reported:?}");
    let after = world.rows(&format!(
        "SELECT state FROM attempts WHERE attempt_id = '{}'",
        before[0]["attempt_id"].as_str().ok_or("attempt")?
    ))?;
    super::keep(
        "test-031-04-review-checkout-kill.json",
        &json!({"before_restart": before, "after_restart": after, "target": target, "reported": reported}),
    )
}
