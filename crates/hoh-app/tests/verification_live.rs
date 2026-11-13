//! TEST-053-05 against the installed CLIs (live; model calls). Run only by scripts/product/adapter-live.sh, which sets
//! HOH_LIVE_OUT and turns the verdict written there (qa-live-verdict.json) into the check's evidence. The name carries
//! no TEST id, so the integration check never reports it as skipped.
//!
//! One candidate→QA loop through the product's host (docs/adr/ADR-029.md, TEST-053-05; AC-053-05): a seeded-defect
//! repository whose AC-090-01 check passes and whose AC-090-02 check (TEST-090-02) fails on every candidate, and a run
//! of one developer task over both criteria. The Developer, the QA and the Reviewer are the installed agents through
//! the product's executor, bound as AC-053-05 gives them: Developer claude, QA pi, Reviewer codex. A pi or codex not
//! installed or not logged in is replaced by claude only under an explicit HOH_LIVE_PROFILE=v0.1 (v0.1 allows two
//! Claude sessions); otherwise the run is BLOCKED. The Commander is a fake that proposes nothing (each of its turns
//! records an assumption). The loop ends when the candidate's first verification round closes, within [`LOOP`]; the run
//! is cancelled then, before it is judged, so no rework turn follows.
//!
//! PASS iff the QA's and the Developer's HoH sessions differ and so do their native ids, both rows in `sessions`; the
//! QA's report row is bound to the candidate C and its native_attempt's session_ids hold the report's session_id;
//! AC-090-02's label in the acceptance snapshot (`Reader::acceptance`) is FAIL, UNKNOWN, NOT_RUN or INVALID; and C is
//! not ACCEPTED. claude missing, not logged in or run as root, a default QA or Reviewer unavailable without the v0.1
//! flag, and a turn refused for its login (BLOCKED_AUTH) are BLOCKED, never PASS. The verdict records the bindings
//! (`agents`), the native sessions with their CLI versions and resolved models (`sessions`), and the observables: the
//! turns' raw logs with their sha256, the report and native attempt objects and the DB export, under qa-live/. Its turns
//! ended (what is left of them killed) and its temporary directories removed, the verdict is written.
//!
//! The verdict cannot tell an installed CLI from a stand-in that speaks its protocol: as for the other live tests, the
//! CLIs are the ones the script found on PATH (HOH_LIVE_*), and the check's live-sessions.json names the native sessions
//! for the release gate and its reviewers to find in the agents' own stores.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_agents::compat::Agent;
use hoh_agents::login::Login;
use hoh_app::host::{Executor, Gate, Host, HostConfig, Outcome, Refusal, Started, Work};
use hoh_core::persistence::SessionStart;
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::retry::FailureClass;
use serde_json::{Value, json};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How long the loop may take, from the run's start to its first verification round's close: with the build and the
/// other live tests, well within adapter-live's 7200 s (tests/check-plan.json).
const LOOP: Duration = Duration::from_secs(40 * 60);
/// How long the run's cancel may take to end its turns (the cancel ladder's graces, with room).
const CLEANUP: Duration = Duration::from_secs(120);
/// How often the store is looked at while the loop runs.
const POLL: Duration = Duration::from_secs(2);
/// The verdict's file in HOH_LIVE_OUT, and the directory of its observables.
const VERDICT: &str = "qa-live-verdict.json";
const OBSERVABLES: &str = "qa-live";
/// The developer task, and the criterion whose deterministic test fails on every candidate.
const TASK: &str = "T-LIVE";
const AC_B: &str = "AC-090-02";

/// The fixture's specification as committed (its sha256 is the run's spec digest).
const SPEC: &str = "{\"requirements\": [{\"id\": \"REQ-090\", \"acceptance\": [{\"id\": \"AC-090-01\", \"test_id\": \"TEST-090-01\"}, {\"id\": \"AC-090-02\", \"test_id\": \"TEST-090-02\"}]}]}\n";

/// The library with its seeded defect: b() prints 3, not 2.
const LIB: &str = "# a(): prints 1.\na() { echo 1; }\n\n# b(): prints 2.\nb() { echo 3; }\n";

const README: &str =
    "# lib.sh\n\nTwo shell functions, `a()` and `b()`; `tests/check-plan.json` checks them.\n";

/// AC-090-01's check: a() prints 1 (it does).
const CHECK_A: &str = r##"if . ./lib.sh && [ "$(a)" = 1 ]; then s=PASS; else s=FAIL; fi; printf '{"schema_version": "1.0", "test_id": "TEST-090-01", "name": "test_090_01_a_prints_1", "status": "%s"}\n' "$s" > "$HOH_EVIDENCE_DIR/test-results.jsonl"; test "$s" = PASS"##;

/// AC-090-02's deterministic test: it fails on every candidate, whatever its source (the seeded defect AC-053-05 gives).
const CHECK_B: &str = r##"printf '{"schema_version": "1.0", "test_id": "TEST-090-02", "name": "test_090_02_b_prints_2", "status": "FAIL"}\n' > "$HOH_EVIDENCE_DIR/test-results.jsonl"; echo 'TEST-090-02 fails on every candidate: the seeded defect of this repository' >&2; exit 1"##;

/// The run's goal and the developer task's title, as the Developer's prompt carries them.
const GOAL: &str = "维护 lib.sh：a() 输出 1，b() 输出 2。";
const TITLE: &str = "在 README.md 末尾追加一行 `- a() 已检查` 并提交这一处改动，然后报告 COMPLETED。只做这一处改动：不改 lib.sh、tests/ 与 contracts/。AC-090-02 的检查（feature-b）是本仓库植入的已知缺陷，在任何候选上都失败，由 QA 记录，不在本任务范围内。";

/// The fixture's frozen check plan: each criterion's scenario check, a real command over the candidate's checkout.
fn plan() -> R<String> {
    let check = |argv: &str| {
        json!({"mode": "product", "artifacts": ["test-results.jsonl"], "timeout_seconds": 60,
               "argv": ["sh", "-c", argv]})
    };
    let plan = json!({"schema_version": "1.0", "plan_id": "seeded-defect",
                      "checks": {"feature-a": check(CHECK_A), "feature-b": check(CHECK_B)}});
    Ok(serde_json::to_string_pretty(&plan)? + "\n")
}

fn sha256_hex(bytes: &[u8]) -> String {
    <sha2::Sha256 as sha2::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The product binary's WAL self-test probe.
fn probe() -> ProbeCommand {
    ProbeCommand {
        program: env!("CARGO_BIN_EXE_hoh").into(),
        args: vec![PROBE_ARG.into()],
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

/// An installed agent CLI: its launcher on PATH, the first line of its `--version`, and what its login status command
/// says (none when it says nothing clear or has none to run, as pi without a default provider).
#[derive(Debug, Clone)]
struct Cli {
    agent: Agent,
    launcher: PathBuf,
    version: String,
    login: Option<Login>,
}

/// The executable `name` on `path`, as the shell's `command -v` finds it.
fn which(name: &str, path: &OsStr) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}

/// `agent` as installed on `path`, or why it cannot be used: not installed, or its own status command says it is not
/// logged in (no model call; a login it cannot tell is found by the run, BLOCKED_AUTH).
fn cli(agent: Agent, path: &OsStr) -> Result<Cli, String> {
    let name = agent.as_str();
    let launcher = which(agent.command(), path).ok_or(format!("{name} is not installed"))?;
    let version = Command::new(&launcher)
        .arg("--version")
        .env("PATH", path)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .map(|l| l.trim().to_owned())
        })
        .filter(|v| !v.is_empty())
        .ok_or(format!("{name} --version failed"))?;
    let env = hoh_agents::probe::ProbeEnv {
        path: path.to_owned(),
        fixture_root: None,
        timeout: Duration::from_secs(60),
        log: None,
        deadline: None,
        cancel: None,
    };
    let pi_dir = hoh_agents::login::pi_agent_dir();
    let login = hoh_agents::login::status(agent, &launcher, &env, pi_dir.as_deref())
        .ok()
        .and_then(|(out, _)| hoh_agents::login::conclusion(agent, &out));
    if login == Some(Login::Missing) {
        return Err(format!("{name} is not logged in"));
    }
    Ok(Cli {
        agent,
        launcher,
        version,
        login,
    })
}

/// The agents of the loop's roles.
#[derive(Debug)]
struct Team {
    developer: Cli,
    qa: Cli,
    reviewer: Cli,
    /// "default", or "v0.1" when claude stands in for a default QA or Reviewer.
    profile: &'static str,
    /// Why each stand-in was needed.
    fallbacks: Vec<String>,
}

impl Team {
    fn agents(&self) -> Value {
        json!({"developer": self.developer.agent.as_str(), "qa": self.qa.agent.as_str(),
               "reviewer": self.reviewer.agent.as_str()})
    }

    fn clis(&self) -> Value {
        let one = |role: &str, c: &Cli| {
            json!({"role": role, "agent": c.agent.as_str(), "launcher": c.launcher, "version": c.version,
                   "login": match c.login { Some(Login::LoggedIn) => "logged_in", Some(Login::Missing) => "missing",
                                            None => "unknown" }})
        };
        json!([
            one("developer", &self.developer),
            one("qa", &self.qa),
            one("reviewer", &self.reviewer)
        ])
    }
}

/// The loop's team on `path` (AC-053-05's given; docs/adr/ADR-029.md TEST-053-05): Developer claude, QA pi, Reviewer
/// codex; claude in place of an unavailable pi or codex only when `v01`. Otherwise why it is BLOCKED.
fn team(path: &OsStr, v01: bool) -> Result<Team, String> {
    let claude = cli(Agent::Claude, path)?;
    let mut fallbacks = Vec::new();
    let mut blocked = Vec::new();
    let mut default_or_claude = |agent: Agent, role: &str| match cli(agent, path) {
        Ok(found) => Some(found),
        Err(why) if v01 => {
            fallbacks.push(format!(
                "{role}: {why}; claude in its place (HOH_LIVE_PROFILE=v0.1)"
            ));
            Some(claude.clone())
        }
        Err(why) => {
            blocked.push(format!("{why} (the default {role})"));
            None
        }
    };
    let qa = default_or_claude(Agent::Pi, "QA");
    let reviewer = default_or_claude(Agent::Codex, "Reviewer");
    match (qa, reviewer) {
        (Some(qa), Some(reviewer)) => Ok(Team {
            developer: claude,
            qa,
            reviewer,
            profile: if fallbacks.is_empty() {
                "default"
            } else {
                "v0.1"
            },
            fallbacks,
        }),
        _ => Err(format!(
            "{}; claude stands in only under HOH_LIVE_PROFILE=v0.1",
            blocked.join("; ")
        )),
    }
}

fn write_verdict(out: &Path, status: &str, detail: &str, sessions: &[Value], rest: Value) -> R {
    let mut verdict = json!({"status": status, "detail": detail, "sessions": sessions});
    if let (Some(all), Value::Object(rest)) = (verdict.as_object_mut(), rest) {
        all.extend(rest);
    }
    std::fs::write(out.join(VERDICT), serde_json::to_vec_pretty(&verdict)?)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: runs the installed CLIs through scripts/product/adapter-live.sh"]
async fn verification_live_candidate_to_qa() -> R {
    let out = PathBuf::from(std::env::var_os("HOH_LIVE_OUT").ok_or("HOH_LIVE_OUT is not set")?);
    let path = std::env::var_os("PATH").ok_or("no PATH")?;
    let v01 = std::env::var("HOH_LIVE_PROFILE").is_ok_and(|p| p == "v0.1");
    // AC-025-04's rule for the Developer's claude: the adapter never bypasses permissions as root.
    if rustix::process::getuid().is_root() {
        return write_verdict(&out, "BLOCKED", "running as root", &[], json!({}));
    }
    let team = match team(&path, v01) {
        Ok(team) => team,
        Err(why) => {
            return write_verdict(
                &out,
                "BLOCKED",
                &why,
                &[],
                json!({"profile": if v01 { "v0.1" } else { "default" }}),
            );
        }
    };
    // The CLIs the script found are the ones run here.
    for (var, c) in [
        ("HOH_LIVE_CLAUDE", &team.developer),
        ("HOH_LIVE_PI", &team.qa),
        ("HOH_LIVE_CODEX", &team.reviewer),
    ] {
        let given = std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
        if let Some(given) = given
            && c.agent.as_str() == var["HOH_LIVE_".len()..].to_lowercase()
            && given.canonicalize().ok() != c.launcher.canonicalize().ok()
        {
            let why = format!(
                "PATH's {} is {}, not {var}",
                c.agent.as_str(),
                c.launcher.display()
            );
            write_verdict(&out, "FAIL", &why, &[], json!({"agents": team.agents()}))?;
            return Err(why.into());
        }
    }
    // Outside the system temp directory, as a deployment's state is (XDG state): agent plugins write their own state
    // into a working directory under it (pi's pi-lens writes `.pi-lens-probe-home/`), which the verifier's run-directory
    // check rightly counts as a change and vetoes the candidate (ADR-029 implementation correction 8). Not under the
    // HoH repository either: agents read instruction files from parent directories.
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
        .ok_or("neither XDG_CACHE_HOME nor HOME is set")?
        .join("hoh-live");
    std::fs::create_dir_all(&base)?;
    // A base under the system temp directory (XDG_CACHE_HOME or HOME set there) would bring back the plugin's write and
    // its veto: say so instead of producing a misleading verdict (M2T18R-201).
    let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    if real(&base).starts_with(real(&std::env::temp_dir())) {
        return write_verdict(
            &out,
            "BLOCKED",
            &format!(
                "the live state's base {} is under the system temp directory; set XDG_CACHE_HOME elsewhere",
                base.display()
            ),
            &[],
            json!({}),
        );
    }
    let world = tempfile::Builder::new()
        .prefix("hoh-live-qa-")
        .tempdir_in(&base)?;
    let looked = live(&out, &path, &team, world.path()).await;
    let rest = |observations: Value| {
        json!({"agents": team.agents(), "profile": team.profile, "fallbacks": team.fallbacks,
               "observations": observations})
    };
    match looked {
        Ok(found) => {
            write_verdict(
                &out,
                found.status,
                &found.detail,
                &found.sessions,
                rest(found.observations),
            )?;
            if found.status == "FAIL" {
                return Err(found.detail.into());
            }
            Ok(())
        }
        Err(e) => {
            write_verdict(
                &out,
                "FAIL",
                &e.to_string(),
                &[],
                rest(json!({"clis": team.clis()})),
            )?;
            Err(e)
        }
    }
}

/// What the loop showed.
#[derive(Debug)]
struct Found {
    status: &'static str,
    detail: String,
    /// The native sessions of the loop's turns (contracts/live-session.schema.json items).
    sessions: Vec<Value>,
    observations: Value,
}

/// How watching the loop ended.
#[derive(Debug)]
enum End {
    /// The candidate's first verification round closed.
    Closed,
    /// A turn or a binding was refused for its agent's login or availability.
    Blocked(String),
    /// The run or the developer task stopped short of a closed round.
    Stopped(String),
    TimedOut(Value),
}

/// The loop in a host of its own in `dir`: the fixture, the run, the watch, what it showed, then the run cancelled,
/// the host stopped and what is left of its turns killed; its observables kept under `out`.
async fn live(out: &Path, path: &OsString, team: &Team, dir: &Path) -> R<Found> {
    let project = dir.join("project");
    let spec_sha = fixture(&project, team)?;
    let plan_text = plan()?;
    let mut config = HostConfig::product_with(
        &project,
        hoh_core::config::Config::default(),
        env!("CARGO_BIN_EXE_hoh").into(),
        Arc::new(hoh_app::host::agents::Registry::new(path.clone())),
    );
    config.port = 0;
    config.store.wal_probe = Some(probe());
    let commander_turns = Arc::new(AtomicU32::new(0));
    let runner = config
        .runner
        .as_mut()
        .ok_or("the product's host has no runner")?;
    // Commander directories, and the run directories beside them, in the test's own directory.
    runner.context.commander_dir = Some(dir.join("state/hoh/commander"));
    runner.executor = Arc::new(Live {
        product: runner.executor.clone(),
        spec: spec_sha.clone(),
        turns: commander_turns.clone(),
    });
    let at = Instant::now();
    let host = Host::start(config).await?;
    let run = started(&host).await;
    let (end, shown, cleanup) = match &run {
        Ok(run_id) => {
            let end = async {
                seed(&project, run_id, &spec_sha)?;
                watch(&project, run_id).await
            }
            .await;
            // Cancelled at once, before anything is judged: a closed round that asks for rework would otherwise start
            // the Developer's next turn. A cancel neither freezes nor accepts a candidate, and the rows judged are
            // only appended to.
            let cancel = cancel(&host, &project, run_id).await;
            let shown = match &end {
                Ok(End::Closed) => Some(show(&host, &project, run_id, &plan_text, out).await),
                _ => None,
            };
            let left = drained(&project).await;
            (end, shown, json!({"cancel": cancel, "attempts_left": left}))
        }
        Err(e) => (Err(e.to_string().into()), None, json!(null)),
    };
    // Every native session the run's turns used, whatever the verdict (live-sessions.json lists what ran).
    let sessions = native_sessions(&project);
    let project_id = rows(&project, "SELECT project_id FROM projects")
        .ok()
        .and_then(|r| r[0]["project_id"].as_str().map(str::to_owned));
    let export = run.as_ref().ok().map(|run_id| export(&project, run_id));
    host.shutdown().await;
    let killed = project_id.as_deref().map(kill_marked).unwrap_or_default();
    std::fs::create_dir_all(out.join(OBSERVABLES))?;
    let logs = keep_logs(&project, &out.join(OBSERVABLES))?;
    if let Some(export) = export {
        std::fs::write(
            out.join(OBSERVABLES).join("db.json"),
            serde_json::to_vec_pretty(&export.unwrap_or_else(|e| json!({"error": e.to_string()})))?,
        )?;
    }
    let mut observations = json!({
        "run_id": run.as_ref().ok(), "elapsed_seconds": at.elapsed().as_secs(), "clis": team.clis(),
        "commander_turns": commander_turns.load(Ordering::SeqCst), "cleanup": cleanup, "killed_after_stop": killed,
        "raw_logs": logs, "db_export": format!("{OBSERVABLES}/db.json"),
    });
    let mut add = |more: Value| {
        if let (Some(all), Value::Object(more)) = (observations.as_object_mut(), more) {
            all.extend(more);
        }
    };
    let sessions = sessions?;
    let (status, detail) = match (end?, shown) {
        (End::Closed, Some(shown)) => {
            let shown = shown?;
            add(shown.observations);
            if shown.problems.is_empty() {
                let detail = format!(
                    "QA ({}) and Developer ({}) in distinct sessions; {AC_B} {} on {}, not ACCEPTED",
                    team.qa.agent.as_str(),
                    team.developer.agent.as_str(),
                    shown.ac_b,
                    shown.candidate
                );
                ("PASS", detail)
            } else {
                ("FAIL", shown.problems.join("; "))
            }
        }
        (End::Blocked(why), _) => ("BLOCKED", why),
        (End::Stopped(why), _) => ("FAIL", why),
        (End::TimedOut(last), _) => {
            add(json!({"last": last}));
            (
                "FAIL",
                format!("no verification round closed within {} s", LOOP.as_secs()),
            )
        }
        (End::Closed, None) => ("FAIL", "the round closed but nothing was read".to_owned()),
    };
    Ok(Found {
        status,
        detail,
        sessions,
        observations,
    })
}

/// `git` in `dir`, its stdout.
fn git(dir: &Path, args: &[&str]) -> R<String> {
    let out = Command::new("git").args(args).current_dir(dir).output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// The seeded-defect repository at `project`, committed, its roles bound to `team` in its `.hoh/config.toml` (a new
/// run reads its team from it); the specification's sha256.
fn fixture(project: &Path, team: &Team) -> R<String> {
    std::fs::create_dir_all(project)?;
    git(project, &["init", "-q"])?;
    // Commits the Developer makes are its own repository's: no user signing or hooks.
    let hooks = project.join(".git/hooks");
    for (key, value) in [
        ("user.name", "HoH live fixture"),
        ("user.email", "hoh-live@example.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", &hooks.display().to_string()),
    ] {
        git(project, &["config", key, value])?;
    }
    for (file, text) in [
        ("README.md", README.to_owned()),
        ("lib.sh", LIB.to_owned()),
        ("contracts/requirements.json", SPEC.to_owned()),
        ("tests/check-plan.json", plan()?),
    ] {
        let file = project.join(file);
        std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
        std::fs::write(file, text)?;
    }
    git(project, &["add", "."])?;
    git(project, &["commit", "-q", "-m", "seeded-defect fixture"])?;
    // The Commander's agent never runs (its turns are the fake's); every other role has the loop's agents.
    let mut config = "config_version = \"1.0\"\n".to_owned();
    for (role, cli) in [
        ("commander", &team.developer),
        ("cto", &team.developer),
        ("developer", &team.developer),
        ("qa", &team.qa),
        ("researcher", &team.qa),
        ("reviewer", &team.reviewer),
    ] {
        config.push_str(&format!(
            "\n[team.{role}]\nagent = \"{}\"\n",
            cli.agent.as_str()
        ));
    }
    std::fs::create_dir_all(project.join(".hoh"))?;
    std::fs::write(project.join(".hoh/config.toml"), config)?;
    Ok(sha256_hex(SPEC.as_bytes()))
}

/// The product's executor for every role but the Commander, whose turns a fake answers by recording an assumption
/// (it proposes nothing).
#[derive(Debug)]
struct Live {
    product: Arc<dyn Executor>,
    /// The run's specification digest, which the Commander's answers name.
    spec: String,
    /// The Commander's turns.
    turns: Arc<AtomicU32>,
}

impl Executor for Live {
    fn registry(&self) -> Option<Arc<hoh_app::host::agents::Registry>> {
        self.product.registry()
    }

    fn deliver(&self, work: &Work) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.product.deliver(work)
    }

    fn start(&self, work: &Work) -> Result<Started, Refusal> {
        if work.role == "commander" {
            self.commander(work)
        } else {
            self.product.start(work)
        }
    }

    fn open_session(
        &self,
        start: &SessionStart,
    ) -> tokio::sync::oneshot::Receiver<Result<(), String>> {
        self.product.open_session(start)
    }

    fn cancel(&self, work: &Work) -> bool {
        work.role != "commander" && self.product.cancel(work)
    }
}

impl Live {
    /// A Commander turn: a process held at its gate that ends at once, its one output a record_assumption decision on
    /// the wake it was given.
    fn commander(&self, work: &Work) -> Result<Started, Refusal> {
        use std::os::unix::process::CommandExt as _;
        let transient = FailureClass::Transient { retry_after: None };
        let refusal = |reason: String| Refusal {
            reason,
            class: transient,
            detail: None,
            details: None,
        };
        self.turns.fetch_add(1, Ordering::SeqCst);
        let input = work
            .assignment
            .supervision
            .clone()
            .ok_or_else(|| refusal("NOT_SUPERVISION".to_owned()))?;
        let message = json!({
            "schema_version": "1.0", "message_id": format!("{}/answer", work.attempt_id), "run_id": work.run_id,
            "task_id": "-", "attempt_id": work.attempt_id, "sender_role": "commander",
            "recipient_role": "runtime", "role_epoch": input.role_epoch, "correlation_id": "c",
            "causation_id": input.wake, "idempotency_key": format!("{}/k", work.attempt_id),
            "expected_revision": input.run_revision, "spec_sha256": self.spec, "candidate_id": null,
            "kind": "decision",
            "payload": {"kind": "record_assumption", "statement": "nothing to change", "basis": "the wake"}
        });
        let messages = vec![json!({"nonce": input.nonce, "message": message}).to_string()];
        let mut child = Command::new("sh")
            .args(["-c", "read -r go; exit 0"])
            .env(hoh_app::host::ATTEMPT_ENV, work.marker())
            .process_group(0)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| refusal(format!("SPAWN: {e}")))?;
        let pid = child.id();
        let stdin = child.stdin.take();
        let gate = Gate::new(move || {
            use std::io::Write as _;
            let mut stdin = stdin.ok_or("no stdin")?;
            stdin.write_all(b"go\n").map_err(|e| e.to_string())
        });
        let (end, ended) = tokio::sync::oneshot::channel();
        let raw_log_path = format!(
            ".hoh/attempts/{}/raw.jsonl",
            work.attempt_id.replace('/', "%2F")
        );
        std::thread::spawn(move || {
            let status = child.wait();
            let until = Instant::now() + Duration::from_secs(5);
            let mut group_gone = hoh_agents::identity::group_empty(pid);
            while !group_gone && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
                group_gone = hoh_agents::identity::group_empty(pid);
            }
            let _ = end.send(Outcome {
                success: status.as_ref().is_ok_and(std::process::ExitStatus::success),
                detail: "ended".to_owned(),
                group_gone,
                class: transient,
                observed: hoh_core::persistence::Observed {
                    messages,
                    raw_log_path: Some(raw_log_path),
                    ..Default::default()
                },
                unknown_effect: false,
            });
        });
        Ok(Started {
            pid,
            gate,
            ended,
            prompt: None,
        })
    }
}

/// Run `script` (Python, sqlite3 module) against `db` with `stdin`; its stdout. Test code: the product never writes
/// outside persistence.
fn python(db: &Path, script: &str, stdin: &str) -> R<String> {
    use std::io::Write as _;
    let mut child = Command::new("python3")
        .args(["-c", script])
        .arg(db)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
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

fn db(project: &Path) -> PathBuf {
    project.join(".hoh/state.db")
}

/// Every row of `sql` as an object of its columns.
fn rows(project: &Path, sql: &str) -> R<Value> {
    Ok(serde_json::from_str(&python(
        &db(project),
        "import sqlite3,sys,json; c=sqlite3.connect(sys.argv[1], timeout=10); c.row_factory=sqlite3.Row; \
         print(json.dumps([dict(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?)?)
}

/// Run `sql` (a script) against the project's database.
fn exec(project: &Path, sql: &str) -> R {
    python(
        &db(project),
        "import sqlite3,sys; c=sqlite3.connect(sys.argv[1], timeout=10); c.executescript(sys.stdin.read()); \
         c.commit(); c.close()",
        sql,
    )
    .map(drop)
}

/// `text` as an SQL string literal.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// One HTTP exchange with the host over its TCP port: the status and the JSON body.
async fn exchange(host: &Host, method: &str, path: &str, body: Option<&Value>) -> R<(u16, Value)> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nX-HoH-Token: {token}\r\n",
        port = host.port,
        token = host.token,
    );
    if !body.is_empty() {
        request.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
    }
    request.push_str("\r\n");
    request.push_str(&body);
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", host.port)).await?;
    stream.write_all(request.as_bytes()).await?;
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await?;
    let text = String::from_utf8(raw)?;
    let (head, body) = text.split_once("\r\n\r\n").ok_or("no response head")?;
    let status: u16 = head.split(' ').nth(1).ok_or("no status")?.parse()?;
    Ok((status, serde_json::from_str(body)?))
}

/// A started run's id.
async fn started(host: &Host) -> R<String> {
    let body = json!({
        "schema_version": "1.0", "command_id": "c-live-qa", "idempotency_key": "live-qa",
        "expected_revision": 0, "kind": "StartRun", "run_id": null,
        "payload": {"goal": GOAL, "mode": "AUTO", "interactive": false},
    });
    let (status, accepted) = exchange(host, "POST", "/v1/commands", Some(&body)).await?;
    if status != 202 {
        return Err(format!("StartRun: {status} {accepted}").into());
    }
    Ok(accepted["run_id"].as_str().ok_or("no run_id")?.to_owned())
}

/// Run `run_id` RUNNING on the fixture's specification (`spec_sha`) with REQ-090 (AC-090-01 checked by feature-a,
/// AC-090-02 by feature-b), and its developer task READY over it, titled by an imported plan.
fn seed(project: &Path, run_id: &str, spec_sha: &str) -> R {
    let run = quoted(run_id);
    let plan = json!({"run_id": run_id, "tasks": [{"task_id": TASK, "title": TITLE}]}).to_string();
    exec(
        project,
        &format!(
            "INSERT INTO specs(spec_id, project_id, sha256, revision) SELECT 'sv', project_id, '{spec_sha}', 1 FROM runs \
               WHERE run_id = {run};
             UPDATE runs SET state = 'RUNNING', spec_id = 'sv' WHERE run_id = {run};
             INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-090', 'sv', 'lib.sh', 1);
             INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
               ('AC-090-01', 'REQ-090', 'sv', 'lib.sh 的 a() 输出 1（检查 feature-a，TEST-090-01）', 'feature-a'),
               ('AC-090-02', 'REQ-090', 'sv', 'lib.sh 的 b() 输出 2（检查 feature-b，TEST-090-02）', 'feature-b');
             INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
               ('{TASK}', {run}, 'sv', 'READY', 1, 'developer', 1, (SELECT coalesce(max(created_seq), 0) + 1 FROM tasks), 1);
             INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('{TASK}', 'REQ-090', 'sv');
             INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) SELECT project_id, \
               {run} || '/plan/live', {run}, 'plan.imported', {}, strftime('%Y-%m-%dT%H:%M:%SZ', 'now') FROM runs \
               WHERE run_id = {run};",
            quoted(&plan)
        ),
    )
}

/// Watch run `run_id` until its first verification round closes, a turn or binding is refused for its agent, the run
/// or its developer task stops, or [`LOOP`] passes.
async fn watch(project: &Path, run_id: &str) -> R<End> {
    let run = quoted(run_id);
    let sql = format!(
        "SELECT (SELECT count(*) FROM events WHERE run_id = {run} AND kind = 'verification.round_closed') AS closed, \
         (SELECT state FROM runs WHERE run_id = {run}) AS run, \
         (SELECT blocked_reason FROM runs WHERE run_id = {run}) AS run_blocked, \
         (SELECT group_concat(role || ': ' || coalesce(blocked_reason, ''), '; ') FROM role_bindings \
           WHERE run_id = {run} AND state = 'BLOCKED') AS bindings, \
         (SELECT group_concat(a.attempt_id || ': ' || a.failure_reason, '; ') FROM attempts a \
           JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = {run} AND a.failure_reason LIKE '%BLOCKED_AUTH%') AS auth, \
         (SELECT state FROM tasks WHERE task_id = '{TASK}') AS developer, \
         (SELECT group_concat(t.role || ' ' || a.attempt_id || ' ' || a.state || coalesce(' ' || a.failure_reason, ''), '; ') \
           FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = {run} AND t.role != 'commander') AS attempts"
    );
    let until = Instant::now() + LOOP;
    loop {
        let found = rows(project, &sql)?;
        let now = &found[0];
        let text = |key: &str| now[key].as_str().unwrap_or_default().to_owned();
        if !text("auth").is_empty() {
            return Ok(End::Blocked(format!(
                "BLOCKED_AUTH: a turn was refused for its login: {}",
                text("auth")
            )));
        }
        if !text("bindings").is_empty() {
            return Ok(End::Blocked(format!(
                "a role's agent is unavailable: {}",
                text("bindings")
            )));
        }
        if now["closed"].as_i64().unwrap_or(0) > 0 {
            return Ok(End::Closed);
        }
        let state = text("run");
        if state == "BLOCKED" && text("run_blocked").contains("AUTH") {
            return Ok(End::Blocked(format!(
                "the run is BLOCKED: {}",
                text("run_blocked")
            )));
        }
        if matches!(
            state.as_str(),
            "BLOCKED" | "CANCELLED" | "FAILED" | "COMPLETED" | "INCOMPLETE"
        ) || text("developer") == "BLOCKED"
        {
            return Ok(End::Stopped(format!(
                "no round closed: the run {state} ({}), the developer task {}; attempts: {}",
                text("run_blocked"),
                text("developer"),
                text("attempts")
            )));
        }
        if Instant::now() > until {
            return Ok(End::TimedOut(found));
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What the closed loop showed, judged.
#[derive(Debug)]
struct Shown {
    problems: Vec<String>,
    candidate: String,
    ac_b: String,
    observations: Value,
}

/// The run `run_id` once its first round closed, against TEST-053-05's pass criteria (see the module's head); the
/// report and native attempt objects kept under `out`.
async fn show(host: &Host, project: &Path, run_id: &str, plan_text: &str, out: &Path) -> R<Shown> {
    let run = quoted(run_id);
    let mut problems = Vec::new();
    let candidates = rows(
        project,
        &format!(
            "SELECT candidate_id, state, git_commit, test_plan_sha256 FROM candidates WHERE run_id = {run} \
             ORDER BY created_at, rowid"
        ),
    )?;
    let c = candidates[0]["candidate_id"]
        .as_str()
        .ok_or("no candidate")?
        .to_owned();
    // The premise: the candidate's frozen plan is the fixture's, so AC-090-02's check is the seeded one.
    let commit = candidates[0]["git_commit"].as_str().unwrap_or_default();
    if git(
        project,
        &["show", &format!("{commit}:tests/check-plan.json")],
    )? != plan_text
    {
        problems.push(format!("{c}'s frozen test plan is not the fixture's"));
    }
    let developer = rows(
        project,
        &format!(
            "SELECT attempt_id, session_id, state, agent, resolved_model, failure_reason FROM attempts \
             WHERE task_id = '{TASK}' ORDER BY attempt_id"
        ),
    )?;
    let sessions = rows(
        project,
        "SELECT s.session_id, s.agent, s.role, s.native_id, s.agent_version, (SELECT a.resolved_model FROM attempts a \
         WHERE a.session_id = s.session_id AND a.resolved_model IS NOT NULL ORDER BY a.started_at DESC LIMIT 1) \
         AS resolved_model FROM sessions s ORDER BY s.session_id",
    )?;
    let reports = rows(
        project,
        &format!(
            "SELECT r.report_id, r.candidate_id, r.role, r.session_id, r.status, r.state, r.reject_reason, r.attempt_id, \
             a.sha256 AS report_sha256 FROM independent_reports r JOIN artifacts a ON a.artifact_id = r.report_artifact_id \
             WHERE r.run_id = {run} ORDER BY r.created_at, r.rowid"
        ),
    )?;
    let session_row = |id: &str| {
        sessions
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["session_id"] == json!(id))
            .cloned()
    };
    let native_of = |id: &str| {
        session_row(id).and_then(|s| {
            s["native_id"]
                .as_str()
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
        })
    };
    // 1. The QA's and the Developer's HoH sessions differ, and so do their native ids; both rows in `sessions`.
    let qa_reports: Vec<&Value> = reports
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["role"] == "qa")
        .collect();
    let qa = qa_reports
        .iter()
        .find(|r| r["state"] == "CURRENT")
        .or(qa_reports.last())
        .copied()
        .cloned();
    let developer_sessions: std::collections::BTreeSet<String> = developer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| a["session_id"].as_str().map(str::to_owned))
        .collect();
    let qa_session = qa
        .as_ref()
        .and_then(|r| r["session_id"].as_str())
        .unwrap_or_default()
        .to_owned();
    if developer_sessions.is_empty() {
        problems.push("the developer task's attempts have no session".to_owned());
    }
    let qa_native = native_of(&qa_session);
    if qa.is_some() && session_row(&qa_session).is_none() {
        problems.push(format!("the QA's session {qa_session} is not in sessions"));
    } else if qa.is_some() && qa_native.is_none() {
        problems.push(format!("the QA's session {qa_session} has no native id"));
    }
    for session in &developer_sessions {
        if *session == qa_session {
            problems.push(format!("the QA's HoH session {session} is the Developer's"));
        }
        match native_of(session) {
            None => problems.push(format!(
                "the Developer's session {session} has no native id in sessions"
            )),
            Some(native) if Some(&native) == qa_native.as_ref() => {
                problems.push(format!(
                    "the QA's native session {native} is the Developer's"
                ));
            }
            Some(_) => {}
        }
    }
    // 2. The QA's report row is bound to C, and its native_attempt's session_ids hold the report's session_id.
    let mut native_ids = Value::Null;
    match &qa {
        None => problems.push(format!("no QA report row in the run: {reports}")),
        Some(report) => {
            if report["candidate_id"] != json!(c) {
                problems.push(format!(
                    "the QA report is bound to {}, not {c}",
                    report["candidate_id"]
                ));
            }
            let kept = report["report_sha256"].as_str().ok_or("no report object")?;
            let body: Value = serde_json::from_slice(&hoh_core::objects::get(project, kept)?)?;
            let native_sha = body["native_attempt"]["sha256"]
                .as_str()
                .ok_or("the report has no native_attempt")?;
            let native: Value =
                serde_json::from_slice(&hoh_core::objects::get(project, native_sha)?)?;
            std::fs::create_dir_all(out.join(OBSERVABLES))?;
            std::fs::write(
                out.join(OBSERVABLES).join("qa-report.json"),
                serde_json::to_vec_pretty(&body)?,
            )?;
            std::fs::write(
                out.join(OBSERVABLES).join("qa-native-attempt.json"),
                serde_json::to_vec_pretty(&native)?,
            )?;
            native_ids = native["session_ids"].clone();
            if !native_ids
                .as_array()
                .is_some_and(|ids| ids.contains(&report["session_id"]))
            {
                problems.push(format!(
                    "the QA report's native_attempt session_ids {native_ids} lack its session_id {}",
                    report["session_id"]
                ));
            }
        }
    }
    // 3. AC-090-02's label in the acceptance snapshot is FAIL, UNKNOWN, NOT_RUN or INVALID, whether or not the QA ran it.
    let snapshot = host
        .store()
        .reader()?
        .acceptance(run_id)?
        .ok_or("no acceptance snapshot")?;
    let shown_candidate = snapshot.candidate.as_ref().map(|s| s.candidate_id.clone());
    if shown_candidate.as_deref() != Some(c.as_str()) {
        problems.push(format!(
            "the acceptance snapshot is of {shown_candidate:?}, not {c}"
        ));
    }
    let ac_b = snapshot
        .acs
        .iter()
        .find(|a| a.ac_id == AC_B)
        .map(|a| a.label.code().to_owned())
        .ok_or(format!("{AC_B} is not in the acceptance snapshot"))?;
    if !matches!(ac_b.as_str(), "FAIL" | "UNKNOWN" | "NOT_RUN" | "INVALID") {
        problems.push(format!("{AC_B} is {ac_b} on {c}"));
    }
    // 4. C is not ACCEPTED.
    let state = rows(
        project,
        &format!(
            "SELECT state FROM candidates WHERE candidate_id = {}",
            quoted(&c)
        ),
    )?;
    if state[0]["state"] == "ACCEPTED" {
        problems.push(format!("{c} is ACCEPTED"));
    }
    Ok(Shown {
        problems,
        ac_b,
        observations: json!({
            "candidate": c, "candidates": candidates, "developer_attempts": developer, "sessions_table": sessions,
            "reports": reports, "qa_report": qa, "native_attempt_session_ids": native_ids,
            "ac_b_label": snapshot.acs.iter().find(|a| a.ac_id == AC_B).map(|a| a.label.code()),
            "acceptance_snapshot": serde_json::to_value(&snapshot)?,
        }),
        candidate: c,
    })
}

/// The native sessions the project's turns ran in, as contracts/live-session.schema.json lists them: each session row
/// with a native id (the Commander's fake has none) with its agent, CLI version and latest resolved model.
fn native_sessions(project: &Path) -> R<Vec<Value>> {
    let found = rows(
        project,
        "SELECT s.agent, s.agent_version, s.native_id, (SELECT a.resolved_model FROM attempts a \
         WHERE a.session_id = s.session_id AND a.resolved_model IS NOT NULL ORDER BY a.started_at DESC LIMIT 1) \
         AS resolved_model FROM sessions s WHERE s.native_id IS NOT NULL AND s.native_id != '' ORDER BY s.session_id",
    )?;
    Ok(found
        .as_array()
        .into_iter()
        .flatten()
        .map(|s| {
            json!({"agent": s["agent"], "cli_version": s["agent_version"], "session_id": s["native_id"],
                   "resolved_model": s["resolved_model"].as_str().unwrap_or("unknown")})
        })
        .collect())
}

/// Cancel run `run_id` (CancelRun at its current revision, again on a stale one): the answers.
async fn cancel(host: &Host, project: &Path, run_id: &str) -> Value {
    let mut answers = Vec::new();
    for attempt in 0..5 {
        let revision = rows(
            project,
            &format!(
                "SELECT revision FROM runs WHERE run_id = {}",
                quoted(run_id)
            ),
        )
        .ok()
        .and_then(|r| r[0]["revision"].as_i64());
        let body = json!({
            "schema_version": "1.0", "command_id": format!("c-live-cancel-{attempt}"),
            "idempotency_key": format!("live-cancel-{attempt}"), "expected_revision": revision,
            "kind": "CancelRun", "run_id": run_id, "payload": {"reason": "TEST-053-05: the loop is over"},
        });
        let answer = exchange(host, "POST", "/v1/commands", Some(&body)).await;
        let accepted = matches!(answer, Ok((202, _)));
        answers.push(match answer {
            Ok((status, body)) => json!({"status": status, "body": body}),
            Err(e) => json!({"error": e.to_string()}),
        });
        if accepted {
            break;
        }
    }
    Value::Array(answers)
}

/// Wait, at most [`CLEANUP`], until none of the project's attempts is left unended: those left.
async fn drained(project: &Path) -> Value {
    let until = Instant::now() + CLEANUP;
    loop {
        let left = rows(
            project,
            "SELECT attempt_id, state FROM attempts WHERE state IN ('PREPARED', 'STARTING', 'RUNNING')",
        )
        .unwrap_or_else(|e| json!([{"error": e.to_string()}]));
        if left.as_array().is_some_and(Vec::is_empty) || Instant::now() > until {
            return left;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Kill every process whose environment carries this project's attempt marker (`HOH_ATTEMPT_ID=<project id>/…`, which
/// each turn's processes inherit): what is left of the run's turns once the host stopped. The killed pids.
fn kill_marked(project_id: &str) -> Vec<u32> {
    let marker = format!("{}={project_id}/", hoh_app::host::ATTEMPT_ENV);
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let found: Vec<u32> = entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| *pid != std::process::id())
        .filter(|pid| {
            std::fs::read(format!("/proc/{pid}/environ")).is_ok_and(|env| {
                env.split(|b| *b == 0)
                    .any(|v| v.starts_with(marker.as_bytes()))
            })
        })
        .collect();
    for pid in &found {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    found
}

/// The turns' raw logs (every file under the project's `.hoh/attempts/`) copied into `into/attempts/`: each one's
/// attempt directory, name, size and sha256.
fn keep_logs(project: &Path, into: &Path) -> R<Value> {
    let mut kept = Vec::new();
    let Ok(attempts) = std::fs::read_dir(project.join(".hoh/attempts")) else {
        return Ok(json!([]));
    };
    for attempt in attempts.flatten().filter(|e| e.path().is_dir()) {
        let target = into.join("attempts").join(attempt.file_name());
        std::fs::create_dir_all(&target)?;
        for file in std::fs::read_dir(attempt.path())?
            .flatten()
            .filter(|e| e.path().is_file())
        {
            std::fs::copy(file.path(), target.join(file.file_name()))?;
            let (sha256, bytes) = hoh_core::objects::hash_file(&file.path())?;
            kept.push(json!({"attempt": attempt.file_name().to_string_lossy(),
                             "name": file.file_name().to_string_lossy(), "bytes": bytes, "sha256": sha256}));
        }
    }
    Ok(Value::Array(kept))
}

/// The run's rows, for the record (TEST-053-05's DB export).
fn export(project: &Path, run_id: &str) -> R<Value> {
    let run = quoted(run_id);
    let mut all = serde_json::Map::new();
    for (name, sql) in [
        ("runs", format!("SELECT * FROM runs WHERE run_id = {run}")),
        (
            "role_bindings",
            format!("SELECT * FROM role_bindings WHERE run_id = {run}"),
        ),
        ("sessions", "SELECT * FROM sessions".to_owned()),
        ("tasks", format!("SELECT * FROM tasks WHERE run_id = {run}")),
        (
            "attempts",
            format!(
                "SELECT a.* FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.run_id = {run}"
            ),
        ),
        (
            "candidates",
            format!("SELECT * FROM candidates WHERE run_id = {run}"),
        ),
        (
            "evidence",
            format!(
                "SELECT e.* FROM evidence e JOIN candidates c ON c.candidate_id = e.candidate_id WHERE c.run_id = {run}"
            ),
        ),
        (
            "acceptance_results",
            format!(
                "SELECT r.* FROM acceptance_results r JOIN candidates c ON c.candidate_id = r.candidate_id \
                 WHERE c.run_id = {run}"
            ),
        ),
        (
            "independent_reports",
            format!("SELECT * FROM independent_reports WHERE run_id = {run}"),
        ),
        (
            "issues",
            format!("SELECT * FROM issues WHERE run_id = {run}"),
        ),
        (
            "events",
            format!(
                "SELECT seq, event_id, kind, payload_json, created_at FROM events WHERE run_id = {run} ORDER BY seq"
            ),
        ),
    ] {
        all.insert(name.to_owned(), rows(project, &sql)?);
    }
    Ok(Value::Object(all))
}
