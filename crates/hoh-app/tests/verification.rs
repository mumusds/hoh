//! Frozen candidates in the product (REQ-052; docs/adr/ADR-027.md decisions 3 to 5): the freeze driver against real
//! git repositories and a real store.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_app::verify::check::{CheckError, Ran, Request, pre_check, run};
use hoh_app::verify::freeze::{FreezeError, Inputs, freeze};
use hoh_core::persistence::selftest::{PROBE_ARG, ProbeCommand};
use hoh_core::persistence::{FaultHooks, Frozen, NoFaults, Options, PersistError, Store};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// `git` in `dir` (a test's own repository, no user configuration), its stdout.
fn git(dir: &Path, args: &[&str]) -> R<String> {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()?;
    if !out.status.success() {
        return Err(format!("git {args:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8(out.stdout)?)
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

/// The fixture's frozen check plan: `unit` runs in the product, `pack` is a bundle check the product refuses.
const PLAN: &str = r#"{"schema_version": "1.0", "plan_id": "fixture", "checks": {
  "unit": {"mode": "product", "argv": ["bash", "-c", "touch ../sentinel"]},
  "qa": {"mode": "product", "argv": ["bash", "qa.sh"], "artifacts": ["test-results.jsonl"], "timeout_seconds": 60},
  "slow": {"mode": "product", "argv": ["bash", "-c", "sleep 30"], "artifacts": ["test-results.jsonl"],
           "timeout_seconds": 1},
  "pack": {"mode": "bundle", "argv": ["true"]},
  "sleepy": {"mode": "product", "argv": ["bash", "-c", "sleep 30"], "timeout_seconds": 60},
  "blocked": {"mode": "product", "argv": ["bash", "-c", "exit 3"], "artifacts": ["test-results.jsonl"]}}}
"#;

/// The fixture's specification: REQ-001 with AC-001-01, whose TEST is TEST-001-01.
const SPEC: &str = r#"{"requirements": [{"id": "REQ-001", "acceptance": [{"id": "AC-001-01", "test_id": "TEST-001-01"}]}]}
"#;

/// The fixture's qa check: one passing result for TEST-001-01.
const QA_PASSES: &str = r#"printf '%s\n' '{"schema_version": "1.0", "test_id": "TEST-001-01", "name": "test_001_01_works", "status": "PASS"}' > "$HOH_EVIDENCE_DIR/test-results.jsonl"
"#;

/// A clean repository with three committed sources and its spec, environment and test plan files (AC-052-01's
/// fixture), a store under its `.hoh/` holding run R (and run S), and the inputs.
struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    inputs: Inputs,
}

impl Fixture {
    async fn new() -> R<Fixture> {
        Fixture::with_qa(QA_PASSES).await
    }

    /// The fixture with `qa` as its committed qa.sh.
    async fn with_qa(qa: &str) -> R<Fixture> {
        Fixture::with(PLAN, qa, Arc::new(NoFaults)).await
    }

    /// The fixture with `plan` as its check plan, `qa` as its qa.sh and `faults` in its store.
    async fn with(plan: &str, qa: &str, faults: Arc<dyn FaultHooks>) -> R<Fixture> {
        let dir = tempfile::tempdir()?;
        let root = &dir.path().join("repo");
        std::fs::create_dir_all(root)?;
        git(root, &["init", "-q"])?;
        for (path, text) in [
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/b.rs", "pub fn b() {}\n"),
            ("src/c.rs", "pub fn c() {}\n"),
            ("contracts/requirements.json", SPEC),
            ("environment.json", "{\"os\": \"linux\"}\n"),
            ("tests/check-plan.json", plan),
            ("qa.sh", qa),
        ] {
            let file = root.join(path);
            std::fs::create_dir_all(file.parent().ok_or("no parent")?)?;
            std::fs::write(file, text)?;
        }
        git(root, &["add", "."])?;
        git(root, &["commit", "-q", "-m", "fixture"])?;
        let state = root.join(".hoh");
        hoh_core::state_dir::ensure(&state)?;
        let options = Options {
            wal_probe: Some(probe()),
            faults,
            ..Options::default()
        };
        let store = Store::open(&state.join("state.db"), options)?;
        let repo = root.to_string_lossy().into_owned();
        store.scratch_task("p1", &repo, "R", "T").await?;
        Ok(Fixture {
            dir,
            store,
            inputs: Inputs {
                spec: "contracts/requirements.json".into(),
                environment: "environment.json".into(),
                test_plan: "tests/check-plan.json".into(),
            },
        })
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("repo")
    }

    fn state(&self) -> PathBuf {
        self.root().join(".hoh")
    }

    /// Where the `unit` check's command leaves its mark: outside the checkout, so it changes nothing there.
    fn sentinel(&self) -> PathBuf {
        self.dir.path().join("sentinel")
    }

    async fn freeze(&self, run: &str) -> Result<Frozen, FreezeError> {
        freeze(&self.store, &self.root(), &self.state(), run, &self.inputs).await
    }

    /// Every candidate row as `id run state`, by Python's sqlite3 (test code: the product writes only in
    /// persistence).
    fn candidates(&self) -> R<Vec<String>> {
        let out = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys\n\
                 c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)\n\
                 for r in c.execute('SELECT candidate_id, run_id, state FROM candidates ORDER BY 1'):print(*r)",
            ])
            .arg(self.state().join("state.db"))
            .output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8(out.stdout)?
            .lines()
            .map(str::to_owned)
            .collect())
    }
}

impl Fixture {
    /// Run `sql` on the project's database: the test's own fixture rows (Python's sqlite3, like host.rs's seeds).
    fn seed(&self, sql: &str) -> R {
        let out = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1])\nc.executescript(sys.argv[2])\nc.commit()",
            ])
            .arg(self.state().join("state.db"))
            .arg(sql)
            .output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(())
    }

    /// Run R's qa task Q with attempt `Q/attempt-1`, and a developer task D with attempt `D/attempt-1`.
    fn seed_attempts(&self) -> R {
        self.seed(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq)
               SELECT 'Q', 'R', spec_id, 'RUNNING', 'qa', 2 FROM runs WHERE run_id = 'R';
             INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq)
               SELECT 'D', 'R', spec_id, 'RUNNING', 'developer', 3 FROM runs WHERE run_id = 'R';
             INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
               ('Q/attempt-1', 'Q', 'RUNNING', 1, 1, 1), ('D/attempt-1', 'D', 'RUNNING', 1, 1, 1);",
        )
    }

    /// A check of `check_id` with `argv` by `producer` on candidate `candidate_id`, pre-checked.
    fn pre_check(
        &self,
        candidate_id: &str,
        check_id: &str,
        argv: &[&str],
        producer: &str,
    ) -> R<Result<hoh_app::verify::check::Ready, CheckError>> {
        let reader = self.store.reader()?;
        Ok(pre_check(
            &reader,
            &self.root(),
            &self.state(),
            &Request {
                candidate_id: candidate_id.to_owned(),
                check_id: check_id.to_owned(),
                argv: argv.iter().map(|a| (*a).to_owned()).collect(),
                producer_attempt_id: producer.to_owned(),
            },
        ))
    }
}

const ARGV: &[&str] = &["bash", "-c", "touch ../sentinel"];

/// The new candidate's id of a freeze.
fn new_id(frozen: Frozen) -> R<String> {
    match frozen {
        Frozen::New { candidate_id, .. } => Ok(candidate_id),
        other => Err(format!("not new: {other:?}").into()),
    }
}

fn check_refused(result: Result<hoh_app::verify::check::Ready, CheckError>) -> R<Vec<String>> {
    match result {
        Err(CheckError::Refused(reasons)) => Ok(reasons),
        other => Err(format!("not refused: {other:?}").into()),
    }
}

#[tokio::test]
async fn a_check_on_its_frozen_candidate_by_a_qa_attempt_is_cleared_to_run() -> R {
    // The positive control of the pre-check (docs/adr/ADR-027.md decision 6).
    let f = Fixture::new().await?;
    f.seed_attempts()?;
    let id = new_id(f.freeze("R").await?)?;
    let ready = f
        .pre_check(&id, "unit", ARGV, "Q/attempt-1")?
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(ready.run_id, "R");
    assert_eq!(ready.sealed.candidate_id, id);
    assert_eq!(ready.planned.argv, ARGV);
    assert_eq!(ready.planned.mode, "product");
    assert_eq!(ready.planned.cwd, ".");
    Ok(())
}

#[tokio::test]
async fn every_difference_from_the_frozen_candidate_is_named_before_anything_runs() -> R {
    // REQ-052 behaviour 4, failure modes 3, 4 and 6 (docs/adr/ADR-027.md decision 6, principle two): each variant is
    // refused with its own named reason; the sentinel the check would create never appears.
    type Change = fn(&Fixture) -> R;
    /// A case: its name, the change after the freeze, the check, its argv, the producer, and the reason it names.
    type Variant = (
        &'static str,
        Change,
        &'static str,
        &'static [&'static str],
        &'static str,
        &'static str,
    );
    let variants: [Variant; 10] = [
        (
            "test plan changed by a byte",
            |f| {
                let plan = f.root().join("tests/check-plan.json");
                let mut text = std::fs::read_to_string(&plan)?;
                text.push(' ');
                Ok(std::fs::write(plan, text)?)
            },
            "unit",
            ARGV,
            "Q/attempt-1",
            "test_plan_sha256 mismatch",
        ),
        (
            "argv not the plan's",
            |_| Ok(()),
            "unit",
            &["bash", "-c", "true"],
            "Q/attempt-1",
            "argv differs from the frozen plan",
        ),
        (
            "check not in the plan",
            |_| Ok(()),
            "lint",
            ARGV,
            "Q/attempt-1",
            "not in the frozen plan",
        ),
        (
            "a bundle check",
            |_| Ok(()),
            "pack",
            &["true"],
            "Q/attempt-1",
            "bundle",
        ),
        (
            "spec changed",
            |f| {
                Ok(std::fs::write(
                    f.root().join("contracts/requirements.json"),
                    "{}
",
                )?)
            },
            "unit",
            ARGV,
            "Q/attempt-1",
            "spec_sha256 mismatch",
        ),
        (
            "a new commit",
            |f| {
                std::fs::write(
                    f.root().join("src/lib.rs"),
                    "pub fn a() { 1; }
",
                )?;
                git(&f.root(), &["commit", "-q", "-am", "next"])?;
                Ok(())
            },
            "unit",
            ARGV,
            "Q/attempt-1",
            "checkout changed",
        ),
        (
            "an untracked file",
            |f| {
                Ok(std::fs::write(
                    f.root().join("new.txt"),
                    "x
",
                )?)
            },
            "unit",
            ARGV,
            "Q/attempt-1",
            "worktree dirty",
        ),
        (
            "the developer as producer",
            |_| Ok(()),
            "unit",
            ARGV,
            "D/attempt-1",
            "PRODUCER_NOT_INDEPENDENT",
        ),
        (
            "no such producer",
            |_| Ok(()),
            "unit",
            ARGV,
            "nobody",
            "PRODUCER_MISSING",
        ),
        (
            "a source changed under assume-unchanged",
            |f| {
                git(
                    &f.root(),
                    &["update-index", "--assume-unchanged", "src/b.rs"],
                )?;
                Ok(std::fs::write(
                    f.root().join("src/b.rs"),
                    "pub fn b() { 2; }
",
                )?)
            },
            "unit",
            ARGV,
            "Q/attempt-1",
            "src/b.rs",
        ),
    ];
    for (case, change, check, argv, producer, named) in variants {
        let f = Fixture::new().await?;
        f.seed_attempts()?;
        let id = new_id(f.freeze("R").await?)?;
        change(&f)?;
        let reasons = check_refused(f.pre_check(&id, check, argv, producer)?)?;
        assert!(
            reasons.iter().any(|r| r.contains(named)),
            "{case}: {reasons:?}"
        );
        assert!(!f.sentinel().exists(), "{case}");
    }
    // An environment under .hoh/ changed after the freeze (failure mode 3).
    let mut f = Fixture::new().await?;
    f.seed_attempts()?;
    std::fs::write(f.state().join("environment.json"), "{\"os\": \"linux\"}\n")?;
    f.inputs.environment = ".hoh/environment.json".into();
    let id = new_id(f.freeze("R").await?)?;
    std::fs::write(f.state().join("environment.json"), "{\"os\": \"other\"}\n")?;
    let reasons = check_refused(f.pre_check(&id, "unit", ARGV, "Q/attempt-1")?)?;
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("environment_sha256 mismatch")),
        "{reasons:?}"
    );
    // A producer of another run.
    let f = Fixture::new().await?;
    f.seed_attempts()?;
    let id = new_id(f.freeze("R").await?)?;
    f.seed(
        "INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at)
           SELECT 'S', project_id, spec_id, 'RUNNING', 'AUTO', created_at FROM runs WHERE run_id = 'R';
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq)
           SELECT 'Q2', 'S', spec_id, 'RUNNING', 'qa', 4 FROM runs WHERE run_id = 'S';
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES
           ('Q2/attempt-1', 'Q2', 'RUNNING', 1, 1, 1);",
    )?;
    let reasons = check_refused(f.pre_check(&id, "unit", ARGV, "Q2/attempt-1")?)?;
    assert!(
        reasons.iter().any(|r| r.contains("PRODUCER_MISMATCH")),
        "{reasons:?}"
    );
    Ok(())
}

fn refused(result: Result<Frozen, FreezeError>) -> R<Vec<String>> {
    match result {
        Err(FreezeError::Refused(reasons)) => Ok(reasons),
        other => Err(format!("not refused: {other:?}").into()),
    }
}

/// Whether Python's jsonschema accepts the export at `path` under the locked contracts/candidate.schema.json, and
/// tools/candidate.py computes `id` from it.
fn export_is_valid(path: &Path, id: &str) -> R {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let script = "import json,sys\n\
        sys.path.insert(0, sys.argv[1] + '/tools')\n\
        import jsonschema\n\
        from candidate import identity\n\
        data = json.load(open(sys.argv[2]))\n\
        jsonschema.validate(data, json.load(open(sys.argv[1] + '/contracts/candidate.schema.json')))\n\
        print(identity(data))\n";
    let out = Command::new("python3")
        .args(["-I", "-c", script, root])
        .arg(path)
        .output()?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
    }
    assert_eq!(String::from_utf8(out.stdout)?.trim(), id);
    Ok(())
}

#[tokio::test]
async fn test_052_01_the_same_content_freezes_to_one_candidate_and_an_untracked_file_is_refused()
-> R {
    // AC-052-01: freezing twice gives one id matching ^CAND-[a-f0-9]{64}$ whose export passes candidate.schema.json;
    // with an untracked file the third freeze fails and the candidates still number one.
    let f = Fixture::new().await?;
    let first = f.freeze("R").await?;
    let Frozen::New { candidate_id, .. } = &first else {
        return Err(format!("first freeze: {first:?}").into());
    };
    let id = candidate_id.clone();
    assert!(
        id.len() == 69
            && id.starts_with("CAND-")
            && id[5..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{id}"
    );
    let second = f.freeze("R").await?;
    assert_eq!(
        second,
        Frozen::Existing {
            candidate_id: id.clone(),
            state: "FROZEN".to_owned()
        }
    );
    // The export is kept as an object, valid and of the same identity for the bootstrap tool.
    let rows = f.candidates()?;
    assert_eq!(rows, vec![format!("{id} R FROZEN")]);
    let objects = f.state().join("objects/sha256");
    let export = walk(&objects)?
        .into_iter()
        .find(|p| {
            std::fs::read_to_string(p).is_ok_and(|t| {
                t.contains(&format!("\"candidate_id\":\"{id}\""))
                    || t.contains(&format!("\"candidate_id\": \"{id}\""))
            })
        })
        .ok_or("no export object")?;
    export_is_valid(&export, &id)?;
    // An untracked file: refused, naming it; still one candidate.
    std::fs::write(f.root().join("notes.txt"), "draft\n")?;
    let reasons = refused(f.freeze("R").await)?;
    assert!(
        reasons.iter().any(|r| r.contains("notes.txt")),
        "{reasons:?}"
    );
    assert_eq!(f.candidates()?.len(), 1);
    Ok(())
}

fn walk(dir: &Path) -> R<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            found.extend(walk(&path)?);
        } else {
            found.push(path);
        }
    }
    Ok(found)
}

#[tokio::test]
async fn hidden_changes_links_and_submodules_are_refused_by_name() -> R {
    // docs/adr/ADR-027.md decision 4 (failure mode 5, decision 2; M2T10D-04): an index flag that hides a change, a
    // tracked symbolic link and a submodule are refused, each naming its path; nothing is frozen.
    for (case, setup) in [
        ("assume-unchanged", "assume"),
        ("skip-worktree", "skip"),
        ("symbolic link", "link"),
        ("submodule", "submodule"),
        ("uncommitted change", "dirty"),
    ] {
        let f = Fixture::new().await?;
        let root = &f.root();
        let named = match setup {
            "assume" => {
                git(root, &["update-index", "--assume-unchanged", "src/lib.rs"])?;
                std::fs::write(root.join("src/lib.rs"), "pub fn a() { changed() }\n")?;
                "src/lib.rs"
            }
            "skip" => {
                git(root, &["update-index", "--skip-worktree", "src/b.rs"])?;
                std::fs::write(root.join("src/b.rs"), "pub fn b() { changed() }\n")?;
                "src/b.rs"
            }
            "link" => {
                std::os::unix::fs::symlink("a.rs", root.join("src/link.rs"))?;
                git(root, &["add", "src/link.rs"])?;
                git(root, &["commit", "-q", "-m", "link"])?;
                "src/link.rs"
            }
            "submodule" => {
                let head = git(root, &["rev-parse", "HEAD"])?;
                git(
                    root,
                    &[
                        "update-index",
                        "--add",
                        "--cacheinfo",
                        &format!("160000,{},vendor/sub", head.trim()),
                    ],
                )?;
                git(root, &["commit", "-q", "-m", "submodule"])?;
                "vendor/sub"
            }
            _ => {
                std::fs::write(root.join("src/c.rs"), "pub fn c() { changed() }\n")?;
                "src/c.rs"
            }
        };
        let reasons = refused(f.freeze("R").await)?;
        assert!(
            reasons.iter().any(|r| r.contains(named)),
            "{case}: {reasons:?}"
        );
        assert_eq!(f.candidates()?, Vec::<String>::new(), "{case}");
    }
    Ok(())
}

#[tokio::test]
async fn hohs_own_state_must_be_ignored_and_untracked_and_its_environment_may_live_there() -> R {
    // docs/adr/ADR-027.md decision 3 (M2T10D-154) and decision 4 (-69): a HoH path that is tracked or not ignored is
    // refused by name; an environment description under .hoh/ (ignored, not tracked) is bound by its digest.
    let f = Fixture::new().await?;
    std::fs::write(f.state().join("host.json"), "{}\n")?;
    git(&f.root(), &["add", "-f", ".hoh/host.json"])?;
    git(&f.root(), &["commit", "-q", "-m", "tracked state"])?;
    let reasons = refused(f.freeze("R").await)?;
    assert!(
        reasons.iter().any(|r| r.contains(".hoh/host.json")),
        "{reasons:?}"
    );

    let f = Fixture::new().await?;
    std::fs::write(f.state().join(".gitignore"), "state.db*\n.gitignore\n")?;
    std::fs::create_dir_all(f.state().join("objects"))?;
    let reasons = refused(f.freeze("R").await)?;
    assert!(
        reasons.iter().any(|r| r.contains(".hoh/objects")),
        "{reasons:?}"
    );

    let mut f = Fixture::new().await?;
    std::fs::write(
        f.state().join("environment.json"),
        "{\"os\": \"linux\", \"agents\": []}\n",
    )?;
    f.inputs.environment = ".hoh/environment.json".into();
    let Frozen::New { .. } = f.freeze("R").await? else {
        return Err("not frozen".into());
    };
    // The specification must be a tracked file of the candidate.
    let mut g = Fixture::new().await?;
    std::fs::write(g.state().join("spec.json"), "{}\n")?;
    g.inputs.spec = ".hoh/spec.json".into();
    let reasons = refused(g.freeze("R").await)?;
    assert!(reasons.iter().any(|r| r.contains("spec")), "{reasons:?}");
    Ok(())
}

#[tokio::test]
async fn the_same_content_for_another_run_is_refused_by_the_store() -> R {
    // CR-017 (REQ-052 decision 1 within one run; docs/adr/ADR-027.md decision 5).
    let f = Fixture::new().await?;
    f.freeze("R").await?;
    // A second run of the project, written as the test's own fixture row (Python's sqlite3, like host.rs's seeds).
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3,sys\n\
             c=sqlite3.connect(sys.argv[1])\n\
             c.execute(\"INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) \
             SELECT 'S', project_id, spec_id, 'RUNNING', 'AUTO', created_at FROM runs WHERE run_id='R'\")\n\
             c.commit()",
        ])
        .arg(f.state().join("state.db"))
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let other = f.freeze("S").await;
    assert!(
        matches!(&other, Err(FreezeError::Store(e)) if e.to_string().contains("new commit")),
        "{other:?}"
    );
    Ok(())
}

impl Fixture {
    /// The run's spec as the store knows it: the fixture spec's digest, REQ-001 and its AC-001-01, scenario-checked by
    /// qa.
    fn seed_spec(&self) -> R {
        let (sha, _) =
            hoh_core::objects::hash_file(&self.root().join("contracts/requirements.json"))?;
        self.seed(&format!(
            "UPDATE specs SET sha256 = '{sha}' WHERE spec_id = (SELECT spec_id FROM runs WHERE run_id = 'R');
             INSERT INTO requirements(requirement_id, spec_id, title, required_release)
               SELECT 'REQ-001', spec_id, 'one', 1 FROM runs WHERE run_id = 'R';
             INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id)
               SELECT 'AC-001-01', 'REQ-001', spec_id, 'it works', 'qa' FROM runs WHERE run_id = 'R';"
        ))
    }

    /// One text column of `sql` over the project's database, by Python's sqlite3.
    fn query(&self, sql: &str) -> R<Vec<String>> {
        let out = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys\n\
                 c=sqlite3.connect('file:'+sys.argv[1]+'?mode=ro',uri=True)\n\
                 for r in c.execute(sys.argv[2]):print(r[0])",
            ])
            .arg(self.state().join("state.db"))
            .arg(sql)
            .output()?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned().into());
        }
        Ok(String::from_utf8(out.stdout)?
            .lines()
            .map(str::to_owned)
            .collect())
    }

    /// A frozen candidate of run R with its qa attempt and spec seeded.
    async fn frozen(&self) -> R<String> {
        self.seed_attempts()?;
        self.seed_spec()?;
        new_id(self.freeze("R").await?)
    }

    async fn run(&self, candidate: &str, check: &str, argv: &[&str]) -> Result<Ran, CheckError> {
        run(
            &self.store,
            &self.root(),
            &self.state(),
            &Request {
                candidate_id: candidate.to_owned(),
                check_id: check.to_owned(),
                argv: argv.iter().map(|a| (*a).to_owned()).collect(),
                producer_attempt_id: "Q/attempt-1".to_owned(),
            },
        )
        .await
    }

    /// The record a run wrote.
    fn record(&self, ran: &Ran) -> R<Value> {
        Ok(serde_json::from_slice(&std::fs::read(
            ran.record_dir.join("execution.json"),
        )?)?)
    }
}

const QA: &[&str] = &["bash", "qa.sh"];

#[tokio::test]
async fn test_052_02_a_check_that_changes_the_source_is_invalid_even_when_it_exits_zero() -> R {
    // AC-052-02 (REQ-052 behaviour 5, failure mode 2; docs/adr/ADR-027.md decisions 6, 7, 9, 12): the positive control
    // in a repository and database of its own (M2T10D-158) gives valid evidence and a PASS row; a qa check that
    // rewrites src/lib.rs (variant: creates an untracked file) and exits 0 gives a record with exit_code 0 and
    // source_unchanged false, evidence INVALID, no PASS row for C, an integrity event, and a vetoed candidate.
    let control = Fixture::new().await?;
    let c0 = control.frozen().await?;
    let ran = control
        .run(&c0, "qa", QA)
        .await
        .map_err(|e| format!("{e:?}"))?;
    let record = control.record(&ran)?;
    assert_eq!(
        (
            record["exit_code"].as_i64(),
            record["source_unchanged"].as_bool()
        ),
        (Some(0), Some(true))
    );
    // The record the product wrote is valid under the locked execution.schema.json (docs/adr/ADR-027.md decision 8,
    // M2T10D-255), by Python's jsonschema.
    let script = "import json,sys\nimport jsonschema\n\
        schema = json.load(open(sys.argv[1] + '/contracts/execution.schema.json'))\n\
        jsonschema.validate(json.load(open(sys.argv[2])), schema, format_checker=jsonschema.FormatChecker())\n";
    let out = Command::new("python3")
        .args([
            "-I",
            "-c",
            script,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../.."),
        ])
        .arg(ran.record_dir.join("execution.json"))
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(ran.outcome.status.code(), "PASS", "{:?}", ran.reasons);
    assert_eq!(control.query("SELECT outcome FROM evidence")?, ["PASS"]);
    assert_eq!(
        control.query("SELECT ac_id || ':' || status FROM acceptance_results")?,
        ["AC-001-01:PASS"]
    );
    for (variant, script) in [
        (
            "rewrites src/lib.rs",
            format!("printf 'pub fn a() {{ 1; }}\\n' > src/lib.rs\n{QA_PASSES}"),
        ),
        (
            "creates an untracked file",
            format!("touch new.rs\n{QA_PASSES}"),
        ),
    ] {
        let f = Fixture::with_qa(&script).await?;
        let c = f.frozen().await?;
        let ran = f
            .run(&c, "qa", QA)
            .await
            .map_err(|e| format!("{variant}: {e:?}"))?;
        let record = f.record(&ran)?;
        assert_eq!(record["exit_code"], 0, "{variant}");
        assert_eq!(record["source_unchanged"], false, "{variant}");
        assert_eq!(ran.outcome.status.code(), "INVALID", "{variant}");
        assert!(
            ran.reasons.contains(&"SOURCE_CHANGED"),
            "{variant}: {:?}",
            ran.reasons
        );
        assert_eq!(
            f.query("SELECT outcome FROM evidence")?,
            ["INVALID"],
            "{variant}"
        );
        let rows = f.query(&format!(
            "SELECT status FROM acceptance_results WHERE candidate_id = '{c}'"
        ))?;
        assert!(
            !rows.is_empty() && !rows.iter().any(|r| r == "PASS"),
            "{variant}: {rows:?}"
        );
        assert_eq!(
            f.query("SELECT state FROM candidates")?,
            ["INVALID"],
            "{variant}"
        );
        assert_eq!(
            f.query("SELECT count(*) FROM events WHERE kind = 'integrity_violation'")?,
            ["1"],
            "{variant}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn test_052_03_a_check_that_differs_from_its_frozen_candidate_never_runs() -> R {
    // AC-052-03 (REQ-052 behaviour 4, failure mode 6; docs/adr/ADR-027.md decisions 6 and 12): the positive control
    // runs the sentinel check on the clean candidate and the sentinel appears; with the test plan changed by a byte, or
    // with argv not the plan's, the check is refused before it runs: no sentinel, no record.
    let control = Fixture::new().await?;
    let c0 = control.frozen().await?;
    control
        .run(&c0, "unit", ARGV)
        .await
        .map_err(|e| format!("{e:?}"))?;
    assert!(control.sentinel().exists(), "the control ran its command");
    for (variant, plan_byte, argv, named) in [
        ("test plan changed by a byte", true, ARGV, "test_plan"),
        (
            "argv not the plan's",
            false,
            &["bash", "-c", "touch ../sentinel; true"][..],
            "argv differs from the frozen plan",
        ),
    ] {
        let f = Fixture::new().await?;
        let c = f.frozen().await?;
        if plan_byte {
            let plan = f.root().join("tests/check-plan.json");
            let mut text = std::fs::read_to_string(&plan)?;
            text.push(' ');
            std::fs::write(plan, text)?;
        }
        let refused = f.run(&c, "unit", argv).await;
        let Err(CheckError::Refused(reasons)) = refused else {
            return Err(format!("{variant}: not refused: {refused:?}").into());
        };
        assert!(
            reasons.iter().any(|r| r.contains(named)),
            "{variant}: {reasons:?}"
        );
        assert!(!f.sentinel().exists(), "{variant}: the command ran");
        assert!(
            !f.state().join("evidence").exists(),
            "{variant}: a record was written"
        );
        assert!(
            f.query("SELECT evidence_id FROM evidence")?.is_empty(),
            "{variant}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn a_check_past_its_timeout_fails_without_vetoing_its_candidate() -> R {
    // docs/adr/ADR-027.md principle one (outcome (2), rule 12 on a run that claims nothing): the record says timed out,
    // its missing results are no veto, the evidence is FAIL (TIMED_OUT), the candidate stays FROZEN.
    let f = Fixture::new().await?;
    let c = f.frozen().await?;
    let ran = f
        .run(&c, "slow", &["bash", "-c", "sleep 30"])
        .await
        .map_err(|e| format!("{e:?}"))?;
    let record = f.record(&ran)?;
    assert_eq!(record["timed_out"], true);
    assert_eq!(
        record["missing_artifacts"],
        serde_json::json!(["test-results.jsonl"])
    );
    assert_eq!(
        (ran.outcome.status.code(), ran.outcome.reason),
        ("FAIL", Some("TIMED_OUT"))
    );
    assert!(ran.reasons.is_empty(), "{:?}", ran.reasons);
    assert_eq!(f.query("SELECT state FROM candidates")?, ["FROZEN"]);
    Ok(())
}

/// Principle one's replica, scripts/product/traceability.py, on the fixture's records: per record its outcome, its row
/// on AC-001-01 (decision 9: its conclusion on TEST-001-01, a vetoed record's INVALID only where it is the scenario
/// check or names the TEST) and its reasons; the TEST's status over all of them; the invalid members' reasons.
const REPLICA: &str = "import importlib.util,json,sys\nfrom pathlib import Path\n\
    spec=importlib.util.spec_from_file_location('traceability',sys.argv[1]+'/scripts/product/traceability.py')\n\
    tr=importlib.util.module_from_spec(spec);spec.loader.exec_module(tr)\n\
    repo=Path(sys.argv[2]);candidate=json.load(open(repo/sys.argv[3]))\n\
    reqs=[{'id':'REQ-001','release_targets':['v0.1'],'release_class':'required',\
    'acceptance':[{'id':'AC-001-01','test_id':'TEST-001-01'}]}]\n\
    w=tr.World(repo,candidate,'v0.1',requirements=reqs,scenarios={'TEST-001-01':'qa'})\n\
    found=tr.members(w);test=w.tests()['TEST-001-01'];report=tr.report(w,found,[])\n\
    named=lambda m:m.check=='qa' or any(isinstance(o,dict) and o.get('test_id')=='TEST-001-01' for o in m.lines)\n\
    row=lambda m:('INVALID' if named(m) else None) if m.reasons else tr.conclude(w,m,'TEST-001-01',test)\n\
    print(json.dumps({'members':{m.record['record_id']:[m.outcome[0],m.outcome[1],row(m),sorted(m.reasons)]\
    for m in found},\
    'status':report['tests']['TEST-001-01']['status'],'invalid':[i['reasons'] for i in report['invalid']]}))\n";

#[tokio::test]
async fn the_replica_concludes_on_the_products_records_as_the_product_does() -> R {
    // docs/adr/ADR-027.md decision 9: on the same members the product's outcomes and acceptance rows and the
    // replica's outcomes and conclusions agree, record by record; the replica's TEST status follows their history (a
    // FAIL then a PASS is FLAKY) and names the vetoed member, whose veto set the product's candidate INVALID.
    let failing = QA_PASSES.replace("\"PASS\"", "\"FAIL\"");
    let f = Fixture::with_qa(&format!(
        "if [ -e ../fail ]; then\n{failing}exit 1\nfi\n{QA_PASSES}"
    ))
    .await?;
    let c = f.frozen().await?;
    let mut runs: Vec<Ran> = Vec::new();
    let mut go = async |check: &str, argv: &[&str]| -> R {
        runs.push(
            f.run(&c, check, argv)
                .await
                .map_err(|e| format!("{check}: {e:?}"))?,
        );
        Ok(())
    };
    go("qa", QA).await?;
    go("slow", &["bash", "-c", "sleep 30"]).await?;
    std::fs::write(f.dir.path().join("fail"), b"")?;
    go("qa", QA).await?;
    std::fs::remove_file(f.dir.path().join("fail"))?;
    go("qa", QA).await?;
    go("blocked", &["bash", "-c", "exit 3"]).await?;
    // A check claiming success that observed nothing vetoes its candidate (rule 12): last.
    go("unit", &["bash", "-c", "touch ../sentinel"]).await?;
    let outcomes: Vec<_> = runs
        .iter()
        .map(|r| (r.outcome.status.code(), r.outcome.reason))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("PASS", None),
            ("FAIL", Some("TIMED_OUT")),
            ("FAIL", None),
            ("PASS", None),
            ("BLOCKED", None),
            ("INVALID", Some("INSUFFICIENT"))
        ]
    );
    // A run its caller dropped: interrupted (M2T10R-55).
    let dropped = tokio::time::timeout(
        Duration::from_secs(3),
        f.run(&c, "sleepy", &["bash", "-c", "sleep 30"]),
    )
    .await;
    assert!(dropped.is_err());
    for _ in 0..200 {
        if f.query("SELECT count(*) FROM evidence")? == [(runs.len() + 1).to_string()] {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let manifest = f.record(&runs[0])?["candidate_manifest"]["path"]
        .as_str()
        .ok_or("no manifest path")?
        .to_owned();
    let out = Command::new("python3")
        .args([
            "-I",
            "-c",
            REPLICA,
            concat!(env!("CARGO_MANIFEST_DIR"), "/../.."),
        ])
        .arg(f.root())
        .arg(&manifest)
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let judged: Value = serde_json::from_slice(&out.stdout)?;
    let rows = f.query("SELECT evidence_id || ' ' || status FROM acceptance_results")?;
    // The product's verdict on each record, as its evidence event carries it.
    let recorded: Vec<Value> = f
        .query("SELECT payload_json FROM events WHERE kind = 'evidence.recorded'")?
        .iter()
        .map(|p| serde_json::from_str(p))
        .collect::<Result<_, _>>()?;
    assert_eq!(recorded.len(), runs.len() + 1);
    assert_eq!(
        judged["members"].as_object().map(|m| m.len()),
        Some(recorded.len())
    );
    let mut seen = Vec::new();
    for product in &recorded {
        let id = product["record_id"].as_str().ok_or("no record id")?;
        let row = rows.iter().find_map(|r| r.strip_prefix(&format!("{id} ")));
        let mut reasons: Vec<&str> = product["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        reasons.sort_unstable();
        assert_eq!(
            judged["members"][id],
            serde_json::json!([product["outcome"], product["outcome_reason"], row, reasons]),
            "{id}"
        );
        seen.push((
            product["outcome"].clone(),
            product["outcome_reason"].clone(),
        ));
    }
    assert!(seen.contains(&(serde_json::json!("FAIL"), serde_json::json!("INTERRUPTED"))));
    assert!(seen.contains(&(serde_json::json!("BLOCKED"), Value::Null)));
    assert_eq!(judged["status"], "FLAKY");
    assert_eq!(judged["invalid"], serde_json::json!([["NO_OBSERVATIONS"]]));
    assert_eq!(f.query("SELECT state FROM candidates")?, ["INVALID"]);
    Ok(())
}

#[tokio::test]
async fn a_results_file_that_is_not_utf8_is_a_result_line_invalid() -> R {
    // Rule 1 (docs/adr/ADR-027.md principle one): every line of the results file is read as bytes and held to the
    // schema, as the gate and the replica read it; a file that is not UTF-8 is not taken for one without lines.
    let f = Fixture::with_qa("printf '\\xff\\xfe\\n' > \"$HOH_EVIDENCE_DIR/test-results.jsonl\"\n")
        .await?;
    let c = f.frozen().await?;
    let ran = f.run(&c, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(ran.outcome.status.code(), "INVALID");
    assert_eq!(ran.reasons, ["RESULT_LINE_INVALID"]);
    Ok(())
}

/// A plan whose checks are not of the locked contracts/check-plan.schema.json's shape.
const PLAN_OFF_SCHEMA: &str = r#"{"schema_version": "1.0", "plan_id": "fixture", "checks": {
  "rooted": {"mode": "product", "argv": ["bash", "qa.sh"], "cwd": "/tmp", "artifacts": ["test-results.jsonl"]},
  "above": {"mode": "product", "argv": ["bash", "qa.sh"], "cwd": "..", "artifacts": ["test-results.jsonl"]},
  "named": {"mode": "product", "argv": ["bash", "qa.sh"], "artifacts": ["../test-results.jsonl"]},
  "extra": {"mode": "product", "argv": ["bash", "qa.sh"], "artifacts": ["test-results.jsonl"], "retries": 2}}}
"#;

#[tokio::test]
async fn a_check_planned_off_the_plan_schema_never_runs() -> R {
    // docs/adr/ADR-027.md principle one rule 1 (M2T10R-01): a cwd that is absolute or leaves the checkout, an artifact
    // name that is a path, a key the schema does not know: refused before anything runs, each named.
    let f = Fixture::with(PLAN_OFF_SCHEMA, QA_PASSES, Arc::new(NoFaults)).await?;
    let c = f.frozen().await?;
    for (check, needle) in [
        ("rooted", "cwd"),
        ("above", "cwd"),
        ("named", "artifact"),
        ("extra", "retries"),
    ] {
        let reasons = check_refused(f.pre_check(&c, check, QA, "Q/attempt-1")?)?;
        assert!(
            reasons.iter().any(|r| r.contains(needle)),
            "{check}: {reasons:?}"
        );
        let refused = f.run(&c, check, QA).await;
        assert!(
            matches!(refused, Err(CheckError::Refused(_))),
            "{check}: {refused:?}"
        );
    }
    assert!(!f.state().join("evidence").join(&c).exists());
    Ok(())
}

#[tokio::test]
async fn a_post_check_that_cannot_read_git_is_recorded_invalid() -> R {
    // M2T10R-02: a check that takes the repository's .git away leaves a post-check that cannot prove the source
    // unchanged: the record is written (source_unchanged false), the evidence INVALID, the candidate vetoed and the
    // integrity event raised, as for any other change.
    let f = Fixture::with_qa(&format!("mv .git ../git.saved\n{QA_PASSES}")).await?;
    let c = f.frozen().await?;
    let ran = f.run(&c, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    let record = f.record(&ran)?;
    assert_eq!(record["source_unchanged"], false);
    assert_eq!(ran.outcome.status.code(), "INVALID");
    assert!(ran.reasons.contains(&"SOURCE_CHANGED"), "{:?}", ran.reasons);
    std::fs::rename(f.dir.path().join("git.saved"), f.root().join(".git"))?;
    assert_eq!(f.query("SELECT outcome FROM evidence")?, ["INVALID"]);
    assert_eq!(f.query("SELECT state FROM candidates")?, ["INVALID"]);
    assert_eq!(
        f.query("SELECT count(*) FROM events WHERE kind = 'integrity_violation'")?,
        ["1"]
    );
    Ok(())
}

#[tokio::test]
async fn a_tracked_file_turned_into_a_fifo_is_a_change_not_a_hang() -> R {
    // M2T10R-04: the post-check never blocks on a tracked path that is no longer a regular file; it names it.
    let f = Fixture::with_qa(&format!("rm src/lib.rs && mkfifo src/lib.rs\n{QA_PASSES}")).await?;
    let c = f.frozen().await?;
    let ran = tokio::time::timeout(Duration::from_secs(30), f.run(&c, "qa", QA))
        .await
        .map_err(|_| "the post-check blocked on a FIFO")?
        .map_err(|e| format!("{e:?}"))?;
    assert_eq!(ran.outcome.status.code(), "INVALID");
    let integrity =
        f.query("SELECT payload_json FROM events WHERE kind = 'integrity_violation'")?;
    assert!(
        integrity.iter().any(|p| p.contains("src/lib.rs")),
        "{integrity:?}"
    );
    std::fs::remove_file(f.root().join("src/lib.rs"))?;
    Ok(())
}

#[tokio::test]
async fn a_dropped_run_still_stops_its_command_and_records_it_interrupted() -> R {
    // M2T10R-03 (docs/adr/ADR-027.md decision 6): dropping the run's future is its cancellation: the command is
    // stopped, and the record (exit 130, never a success claim) and the evidence are still written.
    let f = Fixture::new().await?;
    let c = f.frozen().await?;
    let started = tokio::time::timeout(
        Duration::from_secs(3),
        f.run(&c, "sleepy", &["bash", "-c", "sleep 30"]),
    )
    .await;
    assert!(
        started.is_err(),
        "the run should still be going: {started:?}"
    );
    let mut outcomes = Vec::new();
    for _ in 0..200 {
        outcomes = f.query("SELECT outcome FROM evidence")?;
        if !outcomes.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(outcomes, ["FAIL"]);
    let dirs: Vec<_> = std::fs::read_dir(f.state().join("evidence").join(&c).join("sleepy"))?
        .collect::<Result<_, _>>()?;
    assert_eq!(dirs.len(), 1);
    let record: Value =
        serde_json::from_slice(&std::fs::read(dirs[0].path().join("execution.json"))?)?;
    assert_eq!(
        (record["exit_code"].as_i64(), record["timed_out"].as_bool()),
        (Some(130), Some(false))
    );
    assert!(
        record["duration_seconds"].as_f64().unwrap_or(99.0) < 20.0,
        "{record}"
    );
    Ok(())
}

/// Fails the next commit once armed.
#[derive(Debug, Default)]
struct FailOnce(AtomicBool);

impl FaultHooks for FailOnce {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.swap(false, Ordering::SeqCst) {
            return Err(PersistError::Storage("injected commit failure".to_owned()));
        }
        Ok(())
    }
}

#[tokio::test]
async fn a_record_whose_evidence_failed_to_commit_is_recorded_before_the_next_run() -> R {
    // M2T10R-05: a record written to disk whose evidence did not commit is kept as pending and recorded first when the
    // next check runs, so a failure is never replaced by a later pass; the history holds both.
    let faults = Arc::new(FailOnce::default());
    let failing = QA_PASSES.replace("\"PASS\"", "\"FAIL\"");
    let f = Fixture::with(
        PLAN,
        &format!("if [ -e ../fail ]; then\n{failing}exit 1\nfi\n{QA_PASSES}"),
        faults.clone(),
    )
    .await?;
    let c = f.frozen().await?;
    std::fs::write(f.dir.path().join("fail"), b"")?;
    faults.0.store(true, Ordering::SeqCst);
    let first = f.run(&c, "qa", QA).await;
    assert!(matches!(first, Err(CheckError::Store(_))), "{first:?}");
    assert!(f.query("SELECT outcome FROM evidence")?.is_empty());
    std::fs::remove_file(f.dir.path().join("fail"))?;
    let second = f.run(&c, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(second.outcome.status.code(), "PASS");
    let mut outcomes = f.query("SELECT outcome FROM evidence")?;
    outcomes.sort();
    assert_eq!(outcomes, ["FAIL", "PASS"]);
    let mut rows = f.query("SELECT status FROM acceptance_results")?;
    rows.sort();
    assert_eq!(rows, ["FAIL", "PASS"]);
    Ok(())
}

#[tokio::test]
async fn hohs_state_left_unignored_after_the_freeze_is_refused_before_the_run() -> R {
    // M2T10R-06 (docs/adr/ADR-027.md decision 3): the pre-check repeats the freeze's guard on HoH's own state, naming
    // each path git would see, before any record directory is made.
    let f = Fixture::new().await?;
    let c = f.frozen().await?;
    std::fs::write(
        f.state().join(".gitignore"),
        "*\n!evidence/\n!evidence/**\n",
    )?;
    let reasons = check_refused(f.pre_check(&c, "qa", QA, "Q/attempt-1")?)?;
    assert!(
        reasons.iter().any(|r| r.contains("evidence")),
        "{reasons:?}"
    );
    assert!(matches!(
        f.run(&c, "qa", QA).await,
        Err(CheckError::Refused(_))
    ));
    assert!(!f.state().join("evidence").join(&c).exists());
    Ok(())
}

/// Holds the next commit for two seconds once armed: a run's window between its files and its commit.
#[derive(Debug, Default)]
struct SlowOnce(AtomicBool);

impl FaultHooks for SlowOnce {
    fn before_commit(&self) -> Result<(), PersistError> {
        if self.0.swap(false, Ordering::SeqCst) {
            std::thread::sleep(Duration::from_secs(2));
        }
        Ok(())
    }
}

#[tokio::test]
async fn checks_of_one_candidate_run_side_by_side_without_taking_each_others_records() -> R {
    // M2T10R-101, -150 (REQ-037: QA and Review verify a candidate in parallel): a run's reconciliation leaves alone the
    // record another run is still committing, and both runs record their own evidence.
    let slow = Arc::new(SlowOnce::default());
    let f = Fixture::with(PLAN, QA_PASSES, slow.clone()).await?;
    let c = f.frozen().await?;
    slow.0.store(true, Ordering::SeqCst);
    let (first, second) = tokio::join!(f.run(&c, "qa", QA), async {
        tokio::time::sleep(Duration::from_millis(600)).await;
        f.run(&c, "blocked", &["bash", "-c", "exit 3"]).await
    });
    let (first, second) = (
        first.map_err(|e| format!("{e:?}"))?,
        second.map_err(|e| format!("{e:?}"))?,
    );
    assert_eq!(
        (first.outcome.status.code(), second.outcome.status.code()),
        ("PASS", "BLOCKED")
    );
    let mut outcomes = f.query("SELECT outcome FROM evidence")?;
    outcomes.sort();
    assert_eq!(outcomes, ["BLOCKED", "PASS"]);
    Ok(())
}

/// The fixture's qa failing (a FAIL line, exit 1) while `../fail` exists beside the checkout.
fn qa_failing_on_flag() -> String {
    let failing = QA_PASSES.replace("\"PASS\"", "\"FAIL\"");
    format!("if [ -e ../fail ]; then\n{failing}exit 1\nfi\n{QA_PASSES}")
}

/// The record directories of `check` on candidate `c`.
fn record_dirs(f: &Fixture, c: &str, check: &str) -> R<Vec<PathBuf>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(f.state().join("evidence").join(c).join(check))?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    dirs.retain(|d| d.is_dir());
    Ok(dirs)
}

#[tokio::test]
async fn a_pending_record_that_cannot_be_read_holds_back_its_candidates_runs_only() -> R {
    // M2T10R-102, -151: an uncommitted record whose pending evidence cannot be read stops the next run of its
    // candidate, naming the file (a later pass must not take its place); once readable it is recorded first. Another
    // candidate's runs are not held back by it.
    use std::os::unix::fs::PermissionsExt as _;
    let faults = Arc::new(FailOnce::default());
    let f = Fixture::with(PLAN, &qa_failing_on_flag(), faults.clone()).await?;
    let c = f.frozen().await?;
    std::fs::write(f.dir.path().join("fail"), b"")?;
    faults.0.store(true, Ordering::SeqCst);
    assert!(f.run(&c, "qa", QA).await.is_err());
    std::fs::remove_file(f.dir.path().join("fail"))?;
    let pending = record_dirs(&f, &c, "qa")?[0].join("recording.json");
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o000))?;
    let held = f.run(&c, "qa", QA).await;
    assert!(
        matches!(&held, Err(CheckError::Io(why)) if why.contains("recording.json")),
        "{held:?}"
    );
    assert!(f.query("SELECT outcome FROM evidence")?.is_empty());
    assert_eq!(
        record_dirs(&f, &c, "qa")?.len(),
        1,
        "the held run made no record"
    );
    // A new commit's candidate runs.
    std::fs::write(f.root().join("src/d.rs"), "pub fn d() {}\n")?;
    git(&f.root(), &["add", "src/d.rs"])?;
    git(&f.root(), &["commit", "-q", "-m", "d"])?;
    let c2 = new_id(f.freeze("R").await?)?;
    let other = f.run(&c2, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(other.outcome.status.code(), "PASS");
    std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o644))?;
    let mut outcomes = f.query(&format!(
        "SELECT outcome FROM evidence WHERE candidate_id = '{c}'"
    ))?;
    assert!(outcomes.is_empty());
    super_reconcile(&f, &c).await?;
    outcomes = f.query(&format!(
        "SELECT outcome FROM evidence WHERE candidate_id = '{c}'"
    ))?;
    assert_eq!(outcomes, ["FAIL"]);
    Ok(())
}

/// The reconciliation of candidate `c` alone, as a run of it begins.
async fn super_reconcile(f: &Fixture, c: &str) -> R {
    hoh_app::verify::check::reconcile(&f.store.evidence_handle(), &f.root(), &f.state(), c)
        .await
        .map_err(|e| format!("{e:?}"))?;
    Ok(())
}

#[tokio::test]
async fn a_pending_record_is_judged_on_its_files_as_they_are_when_it_is_recorded() -> R {
    // M2T10R-103: the reconciliation judges the record anew, never replays the judgement it was written with: its
    // stdout changed since, it is INVALID (rule 7) and vetoes its candidate, as the file scan says.
    let faults = Arc::new(FailOnce::default());
    let f = Fixture::with(PLAN, QA_PASSES, faults.clone()).await?;
    let c = f.frozen().await?;
    faults.0.store(true, Ordering::SeqCst);
    assert!(f.run(&c, "qa", QA).await.is_err());
    let dir = record_dirs(&f, &c, "qa")?.remove(0);
    std::fs::write(dir.join("stdout.log"), b"rewritten")?;
    super_reconcile(&f, &c).await?;
    assert_eq!(f.query("SELECT outcome FROM evidence")?, ["INVALID"]);
    let payload = f.query("SELECT payload_json FROM events WHERE kind = 'evidence.recorded'")?;
    assert!(payload[0].contains("OUTPUT_CHANGED"), "{payload:?}");
    assert_eq!(f.query("SELECT state FROM candidates")?, ["INVALID"]);
    Ok(())
}

#[tokio::test]
async fn a_committed_records_pending_file_is_never_read_again() -> R {
    // M2T10R-151: once its evidence is recorded, a record's pending file plays no part: damaged later, it stops nothing.
    let f = Fixture::new().await?;
    let c = f.frozen().await?;
    let ran = f.run(&c, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    std::fs::write(ran.record_dir.join("recording.json"), b"{")?;
    let again = f.run(&c, "qa", QA).await.map_err(|e| format!("{e:?}"))?;
    assert_eq!(again.outcome.status.code(), "PASS");
    assert_eq!(f.query("SELECT count(*) FROM evidence")?, ["2"]);
    Ok(())
}
