//! Dispatch, leases and plans against a real store (the integration check): TEST-035-02, TEST-037-01, TEST-038-03.
//! The database is seeded and read with Python's sqlite3 module (test code; the product reaches SQLite only in
//! persistence). The WAL self-test's probe is this binary running `wal_probe_helper`.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::decision::{Context, Proposal};
use hoh_core::persistence::selftest::{self, ProbeCommand};
use hoh_core::persistence::{Decided, Options, PersistError, Store};
use hoh_core::protocol::{ErrorCode, Message, RejectionReason};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The WAL self-test's second process: this binary, only this function.
#[test]
#[ignore = "run by the WAL self-test as its probe"]
fn wal_probe_helper() {
    if std::env::var_os(selftest::PROBE_DB_ENV).is_some() {
        std::process::exit(selftest::probe_main());
    }
}

fn probe() -> ProbeCommand {
    ProbeCommand {
        program: std::env::current_exe().unwrap_or_default(),
        args: [
            "wal_probe_helper",
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .map(OsString::from)
        .to_vec(),
        env: Vec::new(),
        answer_timeout: Duration::from_secs(10),
    }
}

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(probe()),
            ..Options::default()
        },
    )?;
    Ok((store, path))
}

/// Run `script` (Python, sqlite3) on `db` with `stdin`; its stdout.
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

fn seed(db: &Path, sql: &str) -> R {
    python(
        db,
        "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],timeout=30)\nc.execute('PRAGMA foreign_keys=ON')\nc.executescript(sys.stdin.read())\nc.commit()",
        sql,
    )?;
    Ok(())
}

/// Every row of `sql`, each column as JSON.
fn rows(db: &Path, sql: &str) -> R<Vec<Vec<Value>>> {
    let out = python(
        db,
        "import sqlite3,sys,json\nc=sqlite3.connect(sys.argv[1],timeout=30)\nprint(json.dumps([list(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?;
    Ok(serde_json::from_str(&out)?)
}

fn count(db: &Path, table: &str) -> R<i64> {
    let rows = rows(db, &format!("SELECT count(*) FROM {table}"))?;
    rows.first()
        .and_then(|r| r.first())
        .and_then(Value::as_i64)
        .ok_or_else(|| "no count".into())
}

/// The evidence a check keeps (HOH_EVIDENCE_DIR, set by run_check).
fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

/// Run R (RUNNING, revision 7, commander epoch 3, spec s2); T1 READY, a writer task, after T0 (ACCEPTED); T2 READY,
/// another writer task; the developer is bound at epoch 2 with what it needs.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-04T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s2', 'p1', printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's2', 'RUNNING', 'AUTO', 7, '2026-11-04T00:00:00Z');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sc', 'codex', 'commander', '1', '{\"structured_events\": true}'),
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'commander', 'codex', 'sc', 3, 1, '2026-11-04T00:00:00Z'),
  ('R', 'developer', 'claude', 'sd', 2, 1, '2026-11-04T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES
  ('T0', 'R', 's2', 'ACCEPTED', 5, 'developer', 1, 1, 1),
  ('T1', 'R', 's2', 'READY', 2, 'developer', 1, 2, 1),
  ('T2', 'R', 's2', 'READY', 1, 'developer', 1, 3, 1);
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('T1', 'T0');
";

fn context() -> Context {
    let caps: BTreeSet<String> = ["structured_events", "command_exec"]
        .map(str::to_owned)
        .into();
    Context {
        required_capabilities: BTreeMap::from([("developer".to_owned(), caps)]),
        ..Context::default()
    }
}

fn proposal(id: &str, revision: u64, payload: Value) -> R<Proposal> {
    let message: Message = serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "-", "attempt_id": "-",
        "sender_role": "commander", "recipient_role": "runtime", "role_epoch": 3, "correlation_id": "c",
        "causation_id": null, "idempotency_key": format!("k-{id}"), "expected_revision": revision,
        "spec_sha256": format!("{:064}", 2), "candidate_id": null, "kind": "decision", "payload": payload
    }))?;
    Ok(Proposal::from_message(&message)?)
}

fn dispatch(task: &str) -> Value {
    json!({"kind": "dispatch", "task_id": task, "role": "developer"})
}

#[tokio::test]
async fn test_035_02_an_applied_dispatch_leases_the_writer_in_its_one_transaction() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let (events, outbox) = (count(&db, "events")?, count(&db, "outbox")?);
    let first = proposal("d1", 7, dispatch("T1"))?;
    let decided = store.decide(first, context()).await?;
    let Decided::Applied { event_seqs, .. } = &decided else {
        return Err(format!("not applied: {decided:?}").into());
    };
    let exported = json!({
        "decisions": rows(&db, "SELECT decision_id, state, reason_code FROM decisions")?,
        "tasks": rows(&db, "SELECT task_id, state, revision FROM tasks ORDER BY task_id")?,
        "leases": rows(&db, "SELECT project_id, kind, attempt_id, epoch, expires_at FROM leases")?,
        "attempts": rows(&db, "SELECT attempt_id, task_id, state, role_epoch, host_epoch FROM attempts")?,
        "events": rows(&db, "SELECT seq, kind, payload_json FROM events")?,
        "outbox": rows(&db, "SELECT message_id, event_seq, recipient_role, payload_json FROM outbox")?,
    });
    keep("test-035-02-applied.json", &exported)?;
    assert_eq!(
        rows(&db, "SELECT state FROM decisions WHERE decision_id = 'd1'")?,
        [[json!("APPLIED")]]
    );
    assert_eq!(
        rows(
            &db,
            "SELECT state, revision FROM tasks WHERE task_id = 'T1'"
        )?,
        [[json!("LEASED"), json!(3)]]
    );
    let leases = rows(
        &db,
        "SELECT l.kind, a.task_id, l.epoch FROM leases l JOIN attempts a ON a.attempt_id = l.attempt_id \
         WHERE l.project_id = 'p1' AND l.kind = 'writer'",
    )?;
    assert_eq!(leases, [[json!("writer"), json!("T1"), json!(1)]]);
    assert_eq!(count(&db, "events")?, events + 1);
    assert_eq!(count(&db, "outbox")?, outbox + 1);
    let outbox_seq = rows(&db, "SELECT event_seq FROM outbox")?;
    assert_eq!(outbox_seq, [[json!(event_seqs[0])]]);
    // The same proposal again (a second decision, the same payload and expected revision) names a revision that
    // moved: refused, nothing changes.
    let replay = proposal("d1-again", 7, dispatch("T1"))?;
    let stale = store.decide(replay, context()).await?;
    assert!(
        matches!(
            stale,
            Decided::Rejected {
                reason: RejectionReason::StaleRevision,
                ..
            }
        ),
        "{stale:?}"
    );
    keep(
        "test-035-02-resubmitted.json",
        &json!({"decisions": rows(&db, "SELECT decision_id, state, reason_code FROM decisions")?}),
    )?;
    assert_eq!(
        rows(
            &db,
            "SELECT reason_code FROM decisions WHERE decision_id = 'd1-again'"
        )?,
        [[json!("STALE_REVISION")]]
    );
    Ok(())
}

#[tokio::test]
async fn a_second_writer_dispatch_while_the_lease_is_held_is_refused_and_t2_stays_ready() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    seed(
        &db,
        "UPDATE tasks SET wait_rounds = 5, first_eligible_seq = 3 WHERE task_id = 'T1';",
    )?;
    store
        .decide(proposal("d1", 7, dispatch("T1"))?, context())
        .await?;
    // A dispatch ends the task's waiting too (M2T03R-53): it ages from the start if it comes back to READY.
    assert_eq!(
        rows(
            &db,
            "SELECT wait_rounds, first_eligible_seq FROM tasks WHERE task_id = 'T1'"
        )?,
        [[json!(0), json!(null)]]
    );
    let before = rows(
        &db,
        "SELECT task_id, state, revision FROM tasks ORDER BY task_id",
    )?;
    let refused = store
        .decide(proposal("d2", 8, dispatch("T2"))?, context())
        .await?;
    assert!(
        matches!(
            refused,
            Decided::Rejected {
                reason: RejectionReason::LeaseUnavailable,
                ..
            }
        ),
        "{refused:?}"
    );
    assert_eq!(
        rows(
            &db,
            "SELECT task_id, state, revision FROM tasks ORDER BY task_id"
        )?,
        before
    );
    assert_eq!(count(&db, "leases")?, 1);
    Ok(())
}

#[tokio::test]
async fn test_038_03_cycle_missing_and_out_of_range_plans_write_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let task = |id: &str, deps: &[&str]| {
        json!({"task_id": id, "title": id, "role": "developer", "writer": true, "depends_on": deps,
               "requirement_ids": []})
    };
    let mut high = task("p3", &[]);
    high["user_value"] = json!(7);
    let plans = [
        json!({"tasks": [task("a", &["b"]), task("b", &["a"])]}),
        json!({"tasks": [task("c", &["missing"])]}),
        json!({"tasks": [high]}),
    ];
    let before = (count(&db, "tasks")?, count(&db, "task_dependencies")?);
    let mut answers = Vec::new();
    for plan in plans {
        let refused = store
            .import_plan("R", plan)
            .await
            .err()
            .ok_or("a plan was imported")?;
        let PersistError::Plan(error) = &refused else {
            return Err(format!("not a plan error: {refused}").into());
        };
        answers.push(
            json!({"code": format!("{:?}", refused.error_code()), "details": error.details()}),
        );
        assert_eq!(
            (count(&db, "tasks")?, count(&db, "task_dependencies")?),
            before
        );
    }
    keep("test-038-03-refusals.json", &json!(answers))?;
    let codes: Vec<String> = answers
        .iter()
        .map(|a| a["code"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        codes,
        [
            format!("{:?}", Some(ErrorCode::DagCycle)),
            format!("{:?}", Some(ErrorCode::DagMissingDependency)),
            format!("{:?}", Some(ErrorCode::ScoreOutOfRange)),
        ]
    );
    let cycle = answers[0]["details"]["cycle"]
        .as_array()
        .ok_or("no cycle listed")?;
    assert!(
        cycle.contains(&json!("a")) && cycle.contains(&json!("b")),
        "{cycle:?}"
    );
    // A valid plan imports whole, in its canonical order, behind the run's tasks.
    let imported = store
        .import_plan(
            "R",
            json!({"tasks": [task("y", &["x", "T0"]), task("x", &[])]}),
        )
        .await?;
    assert_eq!(imported.task_ids, ["x", "y"]);
    assert_eq!(
        rows(
            &db,
            "SELECT task_id, state, layer, created_seq FROM tasks WHERE task_id IN ('x', 'y') ORDER BY task_id"
        )?,
        [
            [json!("x"), json!("READY"), json!(1), json!(4)],
            [json!("y"), json!("PENDING"), json!(2), json!(5)]
        ]
    );
    Ok(())
}

/// SplitMix64: a small deterministic generator for the lease sequences (the seed is kept with the evidence).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 0..n
    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Work {
    Code,
    Docs,
    Read,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Acquire,
    Release,
    Expire,
}

struct Group {
    tasks: Vec<Work>,
    ops: Vec<Op>,
}

fn groups(seed: u64, n: usize) -> Vec<Group> {
    let mut rng = Rng(seed);
    (0..n)
        .map(|_| {
            let tasks = (0..=rng.below(50))
                .map(|_| match rng.below(3) {
                    0 => Work::Code,
                    1 => Work::Docs,
                    _ => Work::Read,
                })
                .take(50)
                .collect();
            let ops = (0..=rng.below(40))
                .map(|_| match rng.below(10) {
                    0..=4 => Op::Acquire,
                    5..=7 => Op::Release,
                    _ => Op::Expire,
                })
                .collect();
            Group { tasks, ops }
        })
        .collect()
}

/// One project, run and team per group; its tasks READY with no dependencies (leases are what is under test).
fn group_sql(index: usize, group: &Group) -> String {
    let mut sql = format!(
        "INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p{index}', '/repo/{index}', 1, '2026-11-04T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s{index}', 'p{index}', printf('%064d', {index}), 1);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R{index}', 'p{index}', 's{index}', 'RUNNING', 'AUTO', 0, '2026-11-04T00:00:00Z');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R{index}', 'developer', 'claude', 'sd', 1, 1, '2026-11-04T00:00:00Z'), ('R{index}', 'qa', 'pi', 'sq', 1, 1, '2026-11-04T00:00:00Z');
"
    );
    for (i, work) in group.tasks.iter().enumerate() {
        let (role, writer) = match work {
            Work::Code | Work::Docs => ("developer", 1),
            Work::Read => ("qa", 0),
        };
        sql.push_str(&format!(
            "INSERT INTO tasks(task_id, run_id, spec_id, state, role, writer, created_seq, user_value) VALUES ('g{index}-t{i}', 'R{index}', 's{index}', 'READY', '{role}', {writer}, {i}, {});\n",
            i % 6
        ));
    }
    sql
}

#[tokio::test]
async fn test_037_01_random_lease_sequences_keep_one_writer_and_rising_epochs() -> R {
    use hoh_core::lifecycle::AttemptState;
    use hoh_core::persistence::{AttemptFacts, AttemptMove};
    use std::time::UNIX_EPOCH;
    const SEED: u64 = 0x0037_0001_5EED;
    const GROUPS: usize = 1000;
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    let all = groups(SEED, GROUPS);
    let mut sql = String::from(
        "INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{\"structured_events\": true}'), ('sq', 'pi', 'qa', '1', '{\"structured_events\": true}');
",
    );
    for (i, g) in all.iter().enumerate() {
        sql.push_str(&group_sql(i, g));
    }
    seed(&db, &sql)?;
    let caps: BTreeSet<String> = ["structured_events".to_owned()].into();
    let reader = store.reader()?;
    let mut rng = Rng(SEED ^ 0xA5A5);
    let mut log = Vec::with_capacity(GROUPS);
    let (mut grants, mut refused_rounds, mut steps) = (0usize, 0usize, 0usize);
    for (index, group) in all.iter().enumerate() {
        let mut now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let mut active: Vec<(String, bool)> = Vec::new();
        let mut last_writer_epoch = 0;
        let mut docs_dispatches = 0usize;
        let mut writer_epochs = Vec::new();
        for op in &group.ops {
            match op {
                Op::Acquire => {
                    let context = Context {
                        required_capabilities: BTreeMap::from([
                            ("developer".to_owned(), caps.clone()),
                            ("qa".to_owned(), caps.clone()),
                        ]),
                        now: Some(now),
                        ..Context::default()
                    };
                    let outcome = store.schedule_round(&format!("R{index}"), context).await?;
                    match outcome.dispatched {
                        Some(d) => {
                            grants += 1;
                            let task: usize = d
                                .task_id
                                .rsplit('t')
                                .next()
                                .and_then(|n| n.parse().ok())
                                .ok_or("task index")?;
                            let work = group.tasks[task];
                            let writer = d.lease.kind == "writer";
                            assert_eq!(
                                writer,
                                work != Work::Read,
                                "{} ({work:?}) got {}",
                                d.task_id,
                                d.lease.kind
                            );
                            if work == Work::Docs {
                                docs_dispatches += 1;
                            }
                            if writer {
                                assert!(
                                    d.lease.epoch > last_writer_epoch,
                                    "writer epoch {} after {last_writer_epoch}",
                                    d.lease.epoch
                                );
                                last_writer_epoch = d.lease.epoch;
                                writer_epochs.push(d.lease.epoch);
                            }
                            active.push((d.attempt_id, writer));
                        }
                        None => refused_rounds += 1,
                    }
                }
                Op::Release if !active.is_empty() => {
                    let (attempt_id, _) = active.remove(rng.below(active.len()));
                    store
                        .move_attempt(AttemptMove {
                            attempt_id,
                            from: AttemptState::Prepared,
                            to: AttemptState::Failed,
                            facts: AttemptFacts {
                                ended_at: Some(hoh_core::time::rfc3339_utc(now)),
                                ..AttemptFacts::default()
                            },
                        })
                        .await?;
                }
                Op::Release => {}
                Op::Expire => now += Duration::from_secs(121),
            }
            steps += 1;
            let leases = reader.leases(&format!("p{index}"), now)?;
            let writers = leases
                .iter()
                .filter(|l| l.effective && l.kind == "writer")
                .count();
            let readers = leases
                .iter()
                .filter(|l| l.effective && l.kind.starts_with("reader-"))
                .count();
            assert!(writers <= 1, "group {index}: {writers} writer leases");
            assert!(readers <= 2, "group {index}: {readers} reader leases");
            assert!(
                active.iter().filter(|(_, w)| *w).count() <= 1,
                "group {index}: two writer attempts live at once"
            );
        }
        log.push(json!({
            "group": index,
            "tasks": group.tasks.len(),
            "ops": group.ops.len(),
            "writer_epochs": writer_epochs,
            "docs_dispatches": docs_dispatches,
        }));
    }
    // Every docs-only writer task that was dispatched went through the writer lease (asserted per dispatch above);
    // the store agrees: no attempt of a writer task without a writer lease row naming it at some epoch.
    let unleased = rows(
        &db,
        "SELECT count(*) FROM attempts a JOIN tasks t ON t.task_id = a.task_id WHERE t.writer = 1 AND NOT EXISTS \
         (SELECT 1 FROM events e WHERE e.kind = 'schedule.round' AND json_extract(e.payload_json, '$.attempt_id') = a.attempt_id \
          AND json_extract(e.payload_json, '$.lease.kind') = 'writer')",
    )?;
    assert_eq!(unleased, [[json!(0)]]);
    keep(
        "test-037-01-sequences.json",
        &json!({"seed": format!("{SEED:#x}"), "groups": GROUPS, "steps": steps, "grants": grants,
                "rounds_without_grant": refused_rounds, "per_group": log}),
    )?;
    assert!(grants > GROUPS, "too few grants to mean anything: {grants}");
    assert!(refused_rounds > 0, "no round ever found its slot held");
    Ok(())
}

#[tokio::test]
async fn a_round_ranks_with_the_snapshot_weights_and_the_tasks_left_wait() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    // Only information gain counts in R's snapshot: T2 (information 1) outranks T1 (user value 5), which would win
    // under the default weights.
    seed(
        &db,
        "UPDATE runs SET config_snapshot_json = '{\"scheduler\": {\"weights\": {\"user_value\": 0, \"unblock_value\": 0, \
         \"risk_reduction\": 0, \"information_gain\": 1}}}' WHERE run_id = 'R';
         UPDATE tasks SET user_value = 5 WHERE task_id = 'T1';
         UPDATE tasks SET information_gain = 1 WHERE task_id = 'T2';",
    )?;
    let round = store.schedule_round("R", context()).await?;
    let dispatched = round.dispatched.ok_or("nothing dispatched")?;
    assert_eq!(dispatched.task_id, "T2", "{:?}", dispatched.score);
    assert_eq!(round.waited, ["T1"]);
    let seq = round.event_seq.ok_or("no event")?;
    assert_eq!(
        rows(
            &db,
            "SELECT task_id, state, wait_rounds, first_eligible_seq FROM tasks WHERE task_id IN ('T1', 'T2') ORDER BY task_id"
        )?,
        [
            [json!("T1"), json!("READY"), json!(1), json!(seq)],
            [json!("T2"), json!("LEASED"), json!(0), json!(null)]
        ]
    );
    let event: Value = serde_json::from_str(
        rows(
            &db,
            "SELECT payload_json FROM events WHERE kind = 'schedule.round'",
        )?[0][0]
            .as_str()
            .ok_or("no payload")?,
    )?;
    assert_eq!(event["weights"]["information_gain"], json!(1));
    assert_eq!(event["weights"]["user_value"], json!(0));
    // Both are writer tasks and T2 holds the writer lease now: the next round can select nothing, so it writes
    // nothing (T1 does not age for it).
    let next = store.schedule_round("R", context()).await?;
    assert_eq!(next.dispatched, None);
    assert_eq!(
        rows(&db, "SELECT wait_rounds FROM tasks WHERE task_id = 'T1'")?,
        [[json!(1)]],
        "a round that selects nothing writes nothing"
    );
    Ok(())
}

/// A run with one task of each kind a round must pass over (M2T03R-50): its only eligible task is P2, promoted from
/// PENDING; a BLOCKED run dispatches nothing at all.
const GATES: &str = "
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sq', 'pi', 'qa', '1', '{\"structured_events\": true, \"command_exec\": true}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, state, blocked_reason, bound_at) VALUES
  ('R', 'qa', 'pi', 'sq', 1, 1, 'BLOCKED', 'BLOCKED_AUTH', '2026-11-04T00:00:00Z');
UPDATE tasks SET state = 'RUNNING', revision = 3 WHERE task_id = 'T2';
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, user_value, cost_estimate) VALUES
  ('P1', 'R', 's2', 'PENDING', 1, 'developer', 0, 10, 5, 1),
  ('P2', 'R', 's2', 'PENDING', 1, 'developer', 0, 11, 0, 1),
  ('D1', 'R', 's2', 'READY', 1, 'developer', 0, 12, 5, 1),
  ('S1', 'R', 's1', 'READY', 1, 'developer', 0, 13, 5, 1),
  ('C5', 'R', 's2', 'READY', 1, 'developer', 0, 14, 5, 5),
  ('Q1', 'R', 's2', 'READY', 1, 'qa', 0, 15, 5, 1);
INSERT INTO task_dependencies(task_id, depends_on) VALUES ('P1', 'T2'), ('P2', 'T0'), ('D1', 'T2');
DELETE FROM task_dependencies WHERE task_id = 'T1';
UPDATE tasks SET state = 'LEASED' WHERE task_id = 'T1';
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, blocked_reason, created_at) VALUES ('RB', 'p1', 's2', 'BLOCKED', 'AUTO', 1, 'NEED_DECISION', '2026-11-04T00:00:00Z');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES ('RB', 'developer', 'claude', 'sd', 1, 1, '2026-11-04T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq, cost_estimate) VALUES ('B1', 'RB', 's2', 'READY', 1, 'developer', 0, 1, 1);
";

#[tokio::test]
async fn a_round_passes_over_every_ineligible_task_and_a_blocked_run_dispatches_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    seed(&db, GATES)?;
    let mut ctx = context();
    ctx.required_capabilities.insert(
        "qa".to_owned(),
        ["structured_events".to_owned(), "command_exec".to_owned()].into(),
    );
    // C5 costs 5 against a headroom of 2; D1 waits for T2 (RUNNING); S1 is on the old specification; Q1's role is
    // bound BLOCKED; P1 waits for T2; P2's dependency T0 is ACCEPTED, so it is promoted and is the only choice even
    // though it scores lowest.
    ctx.budget_headroom = Some(2);
    let before = rows(
        &db,
        "SELECT task_id, state, revision FROM tasks ORDER BY task_id",
    )?;
    let round = store.schedule_round("R", ctx.clone()).await?;
    let dispatched = round.dispatched.ok_or("nothing dispatched")?;
    assert_eq!(dispatched.task_id, "P2");
    assert_eq!(round.promoted, ["P2"]);
    assert!(round.waited.is_empty(), "{:?}", round.waited);
    let after = rows(
        &db,
        "SELECT task_id, state, revision FROM tasks ORDER BY task_id",
    )?;
    for (was, now) in before.iter().zip(&after) {
        if was[0] == json!("P2") {
            // PENDING -> READY -> LEASED.
            assert_eq!(now[1..], [json!("LEASED"), json!(3)]);
        } else {
            assert_eq!(was, now, "a task the round passed over changed");
        }
    }
    // Nothing else can be chosen now: the next round writes nothing.
    let events = count(&db, "events")?;
    let next = store.schedule_round("R", ctx.clone()).await?;
    assert_eq!(next, hoh_core::persistence::RoundOutcome::default());
    assert_eq!(count(&db, "events")?, events);
    // A BLOCKED run dispatches nothing, though B1 is eligible otherwise.
    let blocked = store.schedule_round("RB", ctx).await?;
    assert_eq!(blocked, hoh_core::persistence::RoundOutcome::default());
    assert_eq!(count(&db, "events")?, events);
    assert_eq!(
        rows(&db, "SELECT state FROM tasks WHERE task_id = 'B1'")?,
        [[json!("READY")]]
    );
    Ok(())
}

#[tokio::test]
async fn an_empty_scheduler_snapshot_reads_as_the_defaults() -> R {
    // M2T03R-58: `{}` for the scheduler or its weights is the defaults, as a missing key is; the run still dispatches.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    for snapshot in [
        r#"{"scheduler": {}}"#,
        r#"{"scheduler": {"weights": {}, "max_wait_rounds": 20}}"#,
    ] {
        seed(
            &db,
            &format!("UPDATE runs SET config_snapshot_json = '{snapshot}' WHERE run_id = 'R';"),
        )?;
        let round = store.schedule_round("R", context()).await?;
        let dispatched = round.dispatched.ok_or("nothing dispatched")?;
        let seq = round.event_seq.ok_or("no event")?;
        let payload = rows(
            &db,
            &format!("SELECT payload_json FROM events WHERE seq = {seq}"),
        )?;
        let event: Value = serde_json::from_str(payload[0][0].as_str().ok_or("no payload")?)?;
        assert_eq!(
            event["weights"],
            json!({"user_value": 4, "unblock_value": 3, "risk_reduction": 3, "information_gain": 2})
        );
        // Let the next one have the writer slot again.
        store
            .move_attempt(hoh_core::persistence::AttemptMove {
                attempt_id: dispatched.attempt_id,
                from: hoh_core::lifecycle::AttemptState::Prepared,
                to: hoh_core::lifecycle::AttemptState::Cancelled,
                facts: hoh_core::persistence::AttemptFacts::default(),
            })
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn a_plan_names_only_the_runs_requirements_and_no_other_runs_task_ids() -> R {
    // M2T03R-55: an unknown requirement is a plan error (a bad request), not a constraint failure; another run's task
    // id is a duplicate; a valid plan's requirements are rows under the run's specification.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    seed(
        &db,
        "INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-038', 's2', 'DAG', 1);
         INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES ('R2', 'p1', 's2', 'RUNNING', 'AUTO', '2026-11-04T00:00:00Z');
         INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('Z1', 'R2', 's2', 'READY', 'developer', 1);",
    )?;
    let task = |id: &str, requirements: &[&str]| {
        json!({"task_id": id, "title": id, "role": "developer", "writer": false, "depends_on": [],
               "requirement_ids": requirements})
    };
    let before = count(&db, "tasks")?;
    let unknown = store
        .import_plan("R", json!({"tasks": [task("n1", &["REQ-999"])]}))
        .await
        .err()
        .ok_or("imported")?;
    let PersistError::Plan(error) = &unknown else {
        return Err(format!("not a plan error: {unknown}").into());
    };
    assert_eq!(unknown.error_code(), None, "{error:?}");
    assert!(error.details().to_string().contains("REQ-999"), "{error:?}");
    let taken = store
        .import_plan("R", json!({"tasks": [task("Z1", &[])]}))
        .await
        .err()
        .ok_or("imported")?;
    assert_eq!(taken.error_code(), Some(ErrorCode::DagDuplicateTask));
    assert_eq!(count(&db, "tasks")?, before);
    store
        .import_plan("R", json!({"tasks": [task("n2", &["REQ-038"])]}))
        .await?;
    assert_eq!(
        rows(
            &db,
            "SELECT task_id, requirement_id, spec_id FROM task_requirements"
        )?,
        [[json!("n2"), json!("REQ-038"), json!("s2")]]
    );
    Ok(())
}
