//! Mock drafting agents with real host SIGKILL, durable effects and user approvals; not live acceptance.
use super::world::World;
use super::*;
const ROOT: &str = "HOH_TEST_DRAFT_ROOT";
const KILL: &str = "HOH_TEST_DRAFT_KILL";
const WAIT: Duration = Duration::from_secs(50);
fn die() {
    let _ = rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::KILL);
}
#[derive(Debug)]
struct KillDraft {
    db: PathBuf,
    point: hoh_app::host::RunnerPoint,
    violate: bool,
}
impl hoh_app::host::HostHooks for KillDraft {
    fn runner_of(&self, point: hoh_app::host::RunnerPoint, id: &str) {
        if point != self.point || !super::query(&self.db,&format!("SELECT kind FROM tasks t JOIN attempts a ON a.task_id=t.task_id WHERE a.attempt_id='{id}'")).is_ok_and(|k|k=="spec_draft") {return;}
        if self.violate {
            let result = (|| -> R {
                let project = self.db.parent().and_then(Path::parent).ok_or("project")?;
                std::fs::write(project.join("product.rs"), "committed outside draft scope")?;
                for args in [
                    vec!["add", "product.rs"],
                    vec![
                        "-c",
                        "user.name=test",
                        "-c",
                        "user.email=test@example.invalid",
                        "commit",
                        "-qm",
                        "interrupted violation",
                    ],
                ] {
                    if !Command::new("git")
                        .current_dir(project)
                        .args(args)
                        .status()?
                        .success()
                    {
                        return Err("fixture git failed".into());
                    }
                }
                Ok(())
            })();
            assert!(result.is_ok(), "violation fixture: {result:?}");
        }
        die();
    }
}
fn team_config(root: &Path) -> HostConfig {
    let project = root.join("p");
    let mut config = HostConfig::new(&project);
    config.port = 0;
    config.store.wal_probe = Some(world::probe());
    config.runner = Some(hoh_app::host::RunnerConfig {
        // The violation hook is this attempt's only producer. Once the start gate opens, a normal drafter
        // could concurrently commit docs and contend for Git's index with the injected write.
        executor: if std::env::var(KILL).as_deref() == Ok("violation") {
            world::Team::working(|_| Vec::new(), Duration::from_secs(60))
        } else {
            world::Team::drafting(project)
        },
        context: hoh_core::decision::Context {
            required_capabilities: world::TEAM
                .iter()
                .map(|(r, _)| ((*r).into(), Default::default()))
                .collect(),
            commander_dir: Some(root.join("state/hoh/commander")),
            ..Default::default()
        },
    });
    config
}
#[test]
#[ignore = "child entry invoked only by the drafting recovery matrix"]
fn draft_host() -> R {
    let Some(root) = std::env::var_os(ROOT).map(PathBuf::from) else {
        return Ok(());
    };
    let mut config = team_config(&root);
    let point = match std::env::var(KILL)?.as_str() {
        "observed" => Some(hoh_app::host::RunnerPoint::AfterEffectObserved),
        "spawn" => Some(hoh_app::host::RunnerPoint::AfterSpawn),
        "received" => Some(hoh_app::host::RunnerPoint::AfterAck),
        "init" => None,
        "violation" => Some(hoh_app::host::RunnerPoint::AfterEffectStarted),
        _ => return Err("unknown kill point".into()),
    };
    if let Some(point) = point {
        config.hooks = Arc::new(KillDraft {
            db: root.join("p/.hoh/state.db"),
            point,
            violate: std::env::var(KILL)? == "violation",
        });
    }
    tokio::runtime::Runtime::new()?.block_on(hoh_app::host::run(config));
    Err("draft host was not killed".into())
}
async fn command(
    host: &Host,
    world: &World,
    run: &str,
    key: &str,
    kind: &str,
    payload: Value,
) -> R {
    for _ in 0..20 {
        let revision: i64 = world
            .query(&format!("SELECT revision FROM runs WHERE run_id='{run}'"))?
            .parse()?;
        let body = json!({"schema_version":"1.0","command_id":key,"idempotency_key":key,"expected_revision":revision,"kind":kind,"run_id":run,"payload":payload});
        let (status, reply) = world::exchange(host, "POST", "/v1/commands", Some(&body)).await?;
        if status == 409 && reply["code"] == "STALE_REVISION" {
            continue;
        }
        assert_eq!(status, 202, "{reply}");
        return Ok(());
    }
    Err("command remained stale".into())
}
fn given(world: &World) -> R<(String, String)> {
    world.configure(&[], "")?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let host = world.start_plain_host(None).await?;
        let result = async {
            let run = world::start_run(&host, "r1", "起草新规格").await?;
            let second = world::start_run(&host, "r2", "等待已批准规格").await?;
            assert_eq!(world.query("SELECT count(*) FROM tasks")?, "0");
            assert_eq!(world.query("SELECT count(*) FROM attempts")?, "0");
            world.staff(&run)?;
            world.staff(&second)?;
            command(
                &host,
                world,
                &run,
                "draft",
                "SendInput",
                json!({"text":"draft","target_role":null,"input_kind":"REPLY"}),
            )
            .await?;
            Ok((run, second))
        }
        .await;
        host.shutdown().await;
        result
    })
}
fn killed(world: &World, point: &str) -> R {
    let stderr_file = world.root().join(format!("draft-child-{point}.stderr"));
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args([
        "--exact",
        "recovery::drafting::draft_host",
        "--ignored",
        "--nocapture",
    ])
    .envs(world.env())
    .env(ROOT, world.root())
    .env(KILL, point)
    .stdout(std::process::Stdio::null())
    .stderr(std::fs::File::create(&stderr_file)?);
    if point == "init" || point == "observed" {
        let fake = world.root().join("draft-git");
        std::fs::create_dir(&fake)?;
        let git = world.tools().join("git");
        let flag = fake.join("once");
        let script = if point == "init" {
            "#!/bin/sh\nif [ \"$1 $2 $3 $4\" = '--no-optional-locks -c core.fsmonitor=false init' ] && [ ! -e \"$HOH_TEST_DRAFT_GIT_MARKER\" ]; then\n \"$HOH_TEST_DRAFT_REAL_GIT\" \"$@\" || exit 1\n : > \"$HOH_TEST_DRAFT_GIT_MARKER\"\n kill -9 $PPID\n exit 0\nfi\nexec \"$HOH_TEST_DRAFT_REAL_GIT\" \"$@\"\n"
        } else {
            // The mock agent commits, then waits before its process ends and its TaskResult is reported.
            // Waiting in the host's AfterEffectObserved hook would not satisfy the scenario's Given.
            "#!/bin/sh\n\"$HOH_TEST_DRAFT_REAL_GIT\" \"$@\" || exit $?\nif [ \"$5\" = commit ]; then\n : > \"$HOH_TEST_DRAFT_GIT_MARKER\"\n sleep 30\nfi\n"
        };
        std::fs::write(fake.join("git"), script)?;
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(fake.join("git"), std::fs::Permissions::from_mode(0o755))?;
        let mut path = fake.into_os_string();
        path.push(":");
        path.push(world.path());
        cmd.env("PATH", path)
            .env("HOH_TEST_DRAFT_REAL_GIT", git)
            .env("HOH_TEST_DRAFT_GIT_MARKER", flag);
    }
    let started = std::time::Instant::now();
    let child = super::ChildGuard(cmd.spawn()?);
    let ended = super::wait(child, WAIT);
    let stderr = std::fs::read_to_string(stderr_file)?;
    let (status, _) = ended.map_err(|error| {
        format!(
            "{point}: {error}; child stderr: {stderr}; events: {:?}",
            world.rows("SELECT kind,payload_json FROM events ORDER BY seq DESC LIMIT 12")
        )
    })?;
    assert_eq!(status.signal(), Some(9), "{point}: {status:?} {stderr}");
    if point == "observed" {
        assert!(world.root().join("draft-git/once").is_file());
        assert!(started.elapsed() >= Duration::from_secs(30));
        assert_eq!(
            world.query("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft' AND a.state='RUNNING'")?,
            "1",
            "host was killed before settling the drafting Attempt"
        );
    }
    Ok(())
}
#[test]
fn test_008_02_s8_git_init_started_is_observed_after_host_kill() -> R {
    recovery("init")
}
#[test]
fn test_008_02_first_draft_kill9_then_approve_and_late_adopt() -> R {
    for point in ["received", "spawn", "observed"] {
        recovery(point)?;
    }
    Ok(())
}
fn recovery(point: &str) -> R {
    let world = World::named("p")?;
    let (run, second) = given(&world)?;
    killed(&world, point)?;
    let task = world.query("SELECT task_id FROM tasks WHERE kind='spec_draft'")?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let host=Host::start(team_config(world.root())).await?;
        let result:R=async {
            world.until("SELECT count(*) FROM events WHERE kind='spec.draft_completed'","1",WAIT).await?;
            assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,"1");assert_eq!(world.query("SELECT task_id FROM tasks WHERE kind='spec_draft'")?,task);
            assert_eq!(world.query("SELECT count(*) FROM events WHERE kind='spec.drafting'")?,"1");assert_eq!(world.query("SELECT count(*) FROM candidates")?,"0");assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");
            assert_eq!(world.query("SELECT count(*) FROM effects WHERE effect_key IN (SELECT json_extract(payload_json,'$.effect_key') FROM events WHERE kind='effect.started' AND json_extract(payload_json,'$.detail.operation')='git init') AND state='COMMITTED'")?,"1");
            assert!(!std::fs::read_to_string(world.project().join(".git/config"))?.contains("[remote"));
            assert_eq!(world.query(&format!("SELECT state||'/'||resume_state FROM runs WHERE run_id='{run}'"))?,"BLOCKED/SPECIFYING");
            command(&host,&world,&run,"approve","SendInput",json!({"text":"approve","target_role":null,"input_kind":"REPLY"})).await?;
            assert_eq!(world.query("SELECT count(*) FROM specs WHERE revision=1")?,"1");assert_eq!(world.query("SELECT state||'/'||coalesce(spec_id,'NULL') FROM tasks WHERE kind='spec_draft'")?,"ACCEPTED/NULL");
            command(&host,&world,&second,"late-draft","SendInput",json!({"text":"draft","target_role":null,"input_kind":"REPLY"})).await?;
            assert_eq!(world.query("SELECT count(*) FROM tasks WHERE kind='spec_draft'")?,"1");assert_eq!(world.query(&format!("SELECT count(*) FROM runs WHERE run_id='{second}' AND spec_id IS NOT NULL"))?,"1");
            Ok(())
        }.await;host.shutdown().await;result
    })
}

#[test]
fn test_008_04_s8_reconcile_checks_failed_draft_writes_and_keeps_files() -> R {
    let world = World::named("p")?;
    let (run, _) = given(&world)?;
    killed(&world, "violation")?;
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async {
        let host=Host::start(team_config(world.root())).await?;
        let result:R=async {
            world.until("SELECT count(*) FROM events WHERE kind='plan.write_violation'","1",WAIT).await?;
            assert_eq!(world.query("SELECT state FROM tasks WHERE kind='spec_draft'")?,"BLOCKED");
            assert_eq!(world.query(&format!("SELECT state||'/'||resume_state FROM runs WHERE run_id='{run}'"))?,"BLOCKED/SPECIFYING");
            assert_eq!(world.query("SELECT json_extract(payload_json,'$.paths[0]') FROM events WHERE kind='plan.write_violation'")?,"product.rs");
            assert_eq!(world.query("SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id=a.task_id WHERE t.kind='spec_draft' AND a.state IN ('PREPARED','STARTING','RUNNING')")?,"0");
            assert_eq!(std::fs::read_to_string(world.project().join("product.rs"))?,"committed outside draft scope");
            assert_eq!(world.query("SELECT count(*) FROM specs")?,"0");Ok(())
        }.await;host.shutdown().await;result
    })
}

/// The caller supplies a separately built, fixed pre-002 product binary and records its commit/hash.
/// This deliberately cannot substitute the current binary or an emulated migration list for the old product.
/// No TEST id: this optional ignored diagnostic is not evidence for the drafting recovery scenario.
/// Executed coverage of refusing a newer schema lives in persistence/migration/tests.rs:
/// test_008_02_v1_to_v2_preserves_every_original_column_and_row and
/// test_008_02_fk_off_precedes_begin_and_on_follows_every_outcome (SchemaTooNew, v2 vs v1), and in
/// tests/db_faults.rs: migration_a_database_of_a_newer_binary_is_refused_with_78_naming_both_versions_and_left_as_it_was
/// (the product binary refuses a newer schema without changing it).
#[test]
#[ignore = "requires HOH_TEST_PRE_002_BINARY pointing at a recorded pre-002 product build"]
fn old_binary_refuses_v2_database() -> R {
    let old =
        PathBuf::from(std::env::var_os("HOH_TEST_PRE_002_BINARY").ok_or("old binary missing")?);
    assert!(old.is_absolute() && old.is_file());
    let world = World::named("p")?;
    world.configure(&[], "")?;
    tokio::runtime::Runtime::new()?.block_on(async {
        let host = world.start_plain_host(None).await?;
        host.shutdown().await;
        Ok::<_, Box<dyn std::error::Error>>(())
    })?;
    assert_eq!(world.query("PRAGMA user_version")?, "2");
    let schema = world.query("SELECT sql FROM sqlite_master ORDER BY type,name")?;
    let events = world.query("SELECT count(*) FROM events")?;
    let child = ChildGuard(
        Command::new(&old)
            .args([std::ffi::OsStr::new("__host"), world.project().as_os_str()])
            .envs(world.env())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()?,
    );
    let (status, stderr) = super::wait(child, Duration::from_secs(10))?;
    assert_eq!(status.code(), Some(78), "{stderr}");
    assert!(
        stderr.contains("database schema version 2 is newer than this binary supports (1)"),
        "{stderr}"
    );
    assert_eq!(world.query("PRAGMA user_version")?, "2");
    assert_eq!(
        world.query("SELECT sql FROM sqlite_master ORDER BY type,name")?,
        schema
    );
    assert_eq!(world.query("SELECT count(*) FROM events")?, events);
    println!(
        "old binary {}: exit 78; v2 schema and event count unchanged",
        old.display()
    );
    Ok(())
}
