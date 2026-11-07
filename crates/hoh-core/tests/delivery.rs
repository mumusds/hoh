//! Outbox delivery, receipts, fences and verification handoffs against a real store (the integration check):
//! TEST-022-01, TEST-022-03, TEST-022-04, TEST-022-05, TEST-062-03. The database is seeded and read with Python's
//! sqlite3 module (test code; the product reaches SQLite only in persistence). The WAL self-test's probe is this
//! binary running `wal_probe_helper`.

// No SQLite outside hoh-core::persistence (AC-062-05; see clippy.toml), and no unsafe code (no FFI way around it).
#![forbid(clippy::disallowed_methods)]
#![forbid(unsafe_code)]

use hoh_core::delivery::{Clock, Consumer, DeliveryError, Dispatcher, LocalTransport, Transport};
use hoh_core::persistence::selftest::{self, ProbeCommand};
use hoh_core::persistence::{
    Change, Commit, ConsumeEffect, Entity, NewAttempt, NewEvent, NewOutbox, Options, OutboxRow,
    PersistError, StateCas, Store, TaskResultOutcome,
};
use hoh_core::protocol::{self, ContextHandoff, Message, RejectionReason, TaskResult};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The WAL self-test's second process: this binary, only this function.
#[test]
#[ignore = "run by the WAL self-test as its probe"]
fn wal_probe_helper() {
    if std::env::var_os(selftest::PROBE_DB_ENV).is_some() {
        std::process::exit(selftest::probe_main());
    }
}

fn open(dir: &tempfile::TempDir) -> R<(Store, PathBuf)> {
    open_with(dir, Options::default())
}

/// The store of `dir` with `options` and this binary as its WAL probe.
fn open_with(dir: &tempfile::TempDir, options: Options) -> R<(Store, PathBuf)> {
    let path = dir.path().join(".hoh/state.db");
    let probe = ProbeCommand {
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
    };
    let store = Store::open(
        &path,
        Options {
            wal_probe: Some(probe),
            ..options
        },
    )?;
    Ok((store, path))
}

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

fn rows(db: &Path, sql: &str) -> R<Vec<Vec<Value>>> {
    let out = python(
        db,
        "import sqlite3,sys,json\nc=sqlite3.connect(sys.argv[1],timeout=30)\nprint(json.dumps([list(r) for r in c.execute(sys.stdin.read())]))",
        sql,
    )?;
    Ok(serde_json::from_str(&out)?)
}

fn one(db: &Path, sql: &str) -> R<Value> {
    rows(db, sql)?
        .first()
        .and_then(|r| r.first())
        .cloned()
        .ok_or_else(|| format!("no row: {sql}").into())
}

fn keep(name: &str, value: &Value) -> R {
    if let Some(dir) = std::env::var_os("HOH_EVIDENCE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_vec_pretty(value)?,
        )?;
    }
    Ok(())
}

fn sha(n: u32) -> String {
    format!("{n:064}")
}

/// Run R (RUNNING) on spec s2 (S2); s1 (S1) is older. qa is bound at epoch 2, developer and reviewer at 1. Task T is
/// VERIFYING at revision 3 (what QA messages act on); task D is RUNNING with the Developer's attempt A and requirement
/// REQ-022, whose two acceptance criteria exist under s2. Candidate C2 is FROZEN on S2; C1 is an older, STALE one.
const WORLD: &str = "
INSERT INTO projects(project_id, repo_path, host_epoch, created_at) VALUES ('p1', '/repo', 1, '2026-11-07T00:00:00Z');
INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1), ('s2', 'p1', printf('%064d', 2), 2);
INSERT INTO runs(run_id, project_id, spec_id, state, mode, revision, created_at) VALUES ('R', 'p1', 's2', 'RUNNING', 'AUTO', 1, '2026-11-07T00:00:00Z');
INSERT INTO requirements(requirement_id, spec_id, title, required_release) VALUES ('REQ-022', 's2', 'delivery', 1);
INSERT INTO acceptance_criteria(ac_id, requirement_id, spec_id, body, check_id) VALUES
  ('AC-022-04', 'REQ-022', 's2', 'handoffs', 'integration'), ('AC-022-01', 'REQ-022', 's2', 'receipts', 'integration');
INSERT INTO sessions(session_id, agent, role, agent_version, capabilities_json) VALUES
  ('sd', 'claude', 'developer', '1', '{}'), ('sq', 'pi', 'qa', '1', '{}'), ('sr', 'codex', 'reviewer', '1', '{}');
INSERT INTO role_bindings(run_id, role, agent, session_id, role_epoch, config_revision, bound_at) VALUES
  ('R', 'developer', 'claude', 'sd', 1, 1, '2026-11-07T00:00:00Z'),
  ('R', 'qa', 'pi', 'sq', 2, 1, '2026-11-07T00:00:00Z'),
  ('R', 'reviewer', 'codex', 'sr', 1, 1, '2026-11-07T00:00:00Z');
INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES
  ('T', 'R', 's2', 'VERIFYING', 3, 'qa', 0, 1),
  ('D', 'R', 's2', 'RUNNING', 4, 'developer', 1, 2);
INSERT INTO task_requirements(task_id, requirement_id, spec_id) VALUES ('D', 'REQ-022', 's2');
INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('A', 'D', 'RUNNING', 1, 1, 1);
INSERT INTO leases(project_id, kind, attempt_id, epoch, expires_at) VALUES ('p1', 'writer', 'A', 1, '2100-01-01T00:00:00Z');
INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256, source_path, state, created_at) VALUES
  ('C1', 'R', printf('%040d', 1), printf('%040d', 11), printf('%064d', 1), printf('%064d', 3), printf('%064d', 4), '/c1', 'STALE', '2026-11-07T00:00:01Z'),
  ('C2', 'R', printf('%040d', 2), printf('%040d', 12), printf('%064d', 2), printf('%064d', 3), printf('%064d', 4), '/c2', 'FROZEN', '2026-11-07T00:00:02Z');
";

/// A message to qa about task T.
fn to_qa(id: &str, expected: u64, epoch: u64, spec: &str, candidate: Option<&str>) -> R<Message> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "message_id": id, "run_id": "R", "task_id": "T", "attempt_id": "-",
        "sender_role": "runtime", "recipient_role": "qa", "role_epoch": epoch, "correlation_id": "c",
        "causation_id": null, "idempotency_key": id, "expected_revision": expected, "spec_sha256": spec,
        "candidate_id": candidate, "kind": "steer", "payload": {"text": id}
    }))?)
}

/// Queue `messages` with one event, as a sender would.
async fn enqueue(store: &Store, tag: &str, messages: &[Message]) -> R {
    store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: format!("enqueue/{tag}"),
                run_id: Some("R".to_owned()),
                kind: "test.enqueued".to_owned(),
                payload: json!({"tag": tag}),
            }],
            outbox: messages
                .iter()
                .map(|m| NewOutbox::hcp(0, m))
                .collect::<Result<_, _>>()?,
            ..Commit::default()
        })
        .await?;
    Ok(())
}

fn row_of(message: &Message) -> R<OutboxRow> {
    Ok(OutboxRow {
        message_id: message.message_id.clone(),
        run_id: Some(message.run_id.clone()),
        recipient_role: message.recipient_role.clone(),
        role_epoch: i64::try_from(message.role_epoch)?,
        payload: protocol::encode(message)?,
        attempt_count: 1,
        next_retry_at: None,
    })
}

/// A clock the test moves.
struct FakeClock(Mutex<SystemTime>);

impl FakeClock {
    fn new() -> FakeClock {
        FakeClock(Mutex::new(UNIX_EPOCH + Duration::from_secs(1_800_000_000)))
    }
    fn set(&self, at: SystemTime) {
        if let Ok(mut now) = self.0.lock() {
            *now = at;
        }
    }
}

impl Clock for FakeClock {
    fn now(&self) -> SystemTime {
        self.0.lock().map(|t| *t).unwrap_or(UNIX_EPOCH)
    }
}

/// QA acting on a message: task T one revision on (a compare-and-set on the revision the message expects) and one
/// event of its own.
struct Touch;

impl Consumer for Touch {
    fn effect(&self, message: &Message) -> ConsumeEffect {
        ConsumeEffect {
            cas: vec![StateCas {
                id: message.task_id.clone(),
                expected_revision: i64::try_from(message.expected_revision).unwrap_or(-1),
                change: Change::Touch(Entity::Task),
            }],
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: format!("{}/touched", message.message_id),
                run_id: Some(message.run_id.clone()),
                kind: "qa.touched".to_owned(),
                payload: json!({"message_id": message.message_id}),
            }],
            attempts: Vec::new(),
        }
    }
}

/// A role that takes a message and changes nothing (delivery is all that is tested).
struct Noop;

impl Consumer for Noop {
    fn effect(&self, _: &Message) -> ConsumeEffect {
        ConsumeEffect::default()
    }
}

/// A transport that loses the first ACK of `drop_first` (after the consumer committed), never answers for
/// `never`, and logs every reply it saw.
struct FakeTransport<'a, T: Transport> {
    inner: &'a T,
    drop_first: Option<&'static str>,
    never: Option<&'static str>,
    dropped: Mutex<BTreeSet<String>>,
    replies: Mutex<Vec<(String, String)>>,
    /// Every delivery: the message, its generation, and what happened (the evidence, M2T04R-57).
    log: Mutex<Vec<Value>>,
}

impl<'a, T: Transport> FakeTransport<'a, T> {
    fn new(inner: &'a T, drop_first: Option<&'static str>, never: Option<&'static str>) -> Self {
        FakeTransport {
            inner,
            drop_first,
            never,
            dropped: Mutex::new(BTreeSet::new()),
            replies: Mutex::new(Vec::new()),
            log: Mutex::new(Vec::new()),
        }
    }
    fn replies_of(&self, message_id: &str) -> Vec<String> {
        self.replies
            .lock()
            .map(|r| {
                r.iter()
                    .filter(|(m, _)| m == message_id)
                    .map(|(_, r)| r.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl<T: Transport> Transport for FakeTransport<'_, T> {
    async fn deliver(&self, row: &OutboxRow) -> Result<String, DeliveryError> {
        let note = |what: &str, reply: Option<&str>| {
            if let Ok(mut log) = self.log.lock() {
                log.push(json!({"message_id": row.message_id, "generation": row.attempt_count, "outcome": what,
                                "reply_kind": reply.and_then(|r| serde_json::from_str::<Value>(r).ok())
                                    .map(|v| v["kind"].clone())}));
            }
        };
        if self.never == Some(row.recipient_role.as_str()) {
            note("no answer (never delivered)", None);
            return Err(DeliveryError::Lost("no answer".to_owned()));
        }
        let reply = self.inner.deliver(row).await?;
        if let Ok(mut log) = self.replies.lock() {
            log.push((row.message_id.clone(), reply.clone()));
        }
        if self.drop_first == Some(row.recipient_role.as_str())
            && self
                .dropped
                .lock()
                .map(|mut d| d.insert(row.message_id.clone()))
                .unwrap_or(false)
        {
            note(
                "consumed, its ACK dropped after the receipt committed",
                Some(&reply),
            );
            return Err(DeliveryError::Lost("ack lost".to_owned()));
        }
        note("answered", Some(&reply));
        Ok(reply)
    }
}

/// Tick until nothing is due any more, moving the clock to the next retry each time.
async fn drain<C: Transport>(
    dispatcher: &Dispatcher<'_, FakeClock, C>,
    db: &Path,
    clock: &FakeClock,
) -> R<(Vec<String>, Vec<Value>)> {
    let mut retries = Vec::new();
    let mut series = Vec::new();
    for _ in 0..20 {
        dispatcher.tick().await?;
        series.push(json!({"at": hoh_core::time::rfc3339_utc(clock.now()),
                           "outbox": rows(db, "SELECT message_id, state, attempt_count, next_retry_at FROM outbox ORDER BY message_id")?}));
        let next = rows(
            db,
            "SELECT min(next_retry_at) FROM outbox WHERE state IN ('PENDING', 'DELIVERING')",
        )?;
        let Some(Value::String(at)) = next.first().and_then(|r| r.first()) else {
            return Ok((retries, series));
        };
        retries.push(at.clone());
        clock.set(hoh_core::time::parse_rfc3339(at).ok_or("bad next_retry_at")?);
    }
    Err("deliveries did not settle".into())
}

fn developer_result() -> R<TaskResult> {
    Ok(serde_json::from_value(json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "D", "attempt_id": "A", "role": "developer",
        "role_epoch": 1, "status": "COMPLETED", "summary": "done", "candidate_id": "C2", "evidence": [],
        "issues": [], "next_action": "verify"
    }))?)
}

#[tokio::test]
async fn test_022_01_duplicates_and_reordering_apply_each_message_once() -> R {
    let mut evidence = json!({});
    for (group, order) in [
        ("A", ["m1", "m1", "m2", "m1"].as_slice()),
        ("B", ["m2", "m1", "m2"].as_slice()),
    ] {
        let dir = tempfile::tempdir()?;
        let (store, db) = open(&dir)?;
        seed(&db, WORLD)?;
        let m1 = to_qa("m1", 3, 2, &sha(2), None)?;
        let mut m2 = to_qa("m2", 4, 2, &sha(2), None)?;
        m2.causation_id = Some("m1".to_owned());
        enqueue(&store, "m", &[m1.clone(), m2.clone()]).await?;
        let clock = FakeClock::new();
        let transport = LocalTransport::new(&store, &clock).with("qa", Touch);
        let mut answers: Vec<(String, String)> = Vec::new();
        for id in order {
            let message = if *id == "m1" { &m1 } else { &m2 };
            let reply = transport
                .deliver(&row_of(message)?)
                .await
                .map_err(|e| format!("{e:?}"))?;
            answers.push(((*id).to_owned(), reply));
        }
        let revision = one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?;
        let moves = one(&db, "SELECT count(*) FROM events WHERE kind = 'qa.touched'")?;
        let receipts = one(&db, "SELECT count(*) FROM receipts")?;
        let replies: Vec<Value> = answers
            .iter()
            .map(|(id, r)| json!({"message": id, "reply": serde_json::from_str::<Value>(r).unwrap_or(Value::Null)}))
            .collect();
        evidence[group] = json!({"order": order, "replies": replies, "revision": revision, "moves": moves,
                                 "receipts": receipts,
                                 "requests": [protocol::encode(&m1)?, protocol::encode(&m2)?],
                                 "receipt_rows": rows(&db, "SELECT consumer, message_id, result_json FROM receipts ORDER BY message_id")?,
                                 "events": rows(&db, "SELECT seq, kind, payload_json FROM events ORDER BY seq")?,
                                 "tasks": rows(&db, "SELECT task_id, state, revision FROM tasks ORDER BY task_id")?});
        let first_of = |id: &str| {
            answers
                .iter()
                .find(|(m, _)| m == id)
                .map(|(_, r)| r.clone())
        };
        for (id, reply) in &answers {
            assert_eq!(
                Some(reply.clone()),
                first_of(id),
                "group {group}: {id} answered differently"
            );
        }
        if group == "A" {
            assert_eq!((revision, moves, receipts), (json!(5), json!(2), json!(2)));
        } else {
            let rejected: Value = serde_json::from_str(&first_of("m2").ok_or("no m2")?)?;
            assert_eq!(
                (
                    &rejected["kind"],
                    &rejected["reason_code"],
                    &rejected["payload"]["current_revision"]
                ),
                (&json!("rejected"), &json!("STALE_REVISION"), &json!(3)),
                "{rejected}"
            );
            assert_eq!((revision, moves), (json!(4), json!(1)));
        }
    }
    keep("test-022-01.json", &evidence)?;
    Ok(())
}

#[tokio::test]
async fn test_022_03_a_lost_ack_is_deduplicated_and_an_unanswered_message_dead_letters() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let TaskResultOutcome::Accepted { handoffs, .. } =
        store.accept_task_result(developer_result()?).await?
    else {
        return Err("the result was not accepted".into());
    };
    let (m, n) = (handoffs[0].clone(), handoffs[1].clone());
    assert!(
        m.ends_with("/qa") && n.ends_with("/reviewer"),
        "{handoffs:?}"
    );
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock)
        .with("qa", Noop)
        .with("reviewer", Noop);
    // qa's first ACK is lost; the reviewer never answers.
    let transport = FakeTransport::new(&local, Some("qa"), Some("reviewer"));
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &transport,
        roles: local.roles(),
        max_attempts: 3,
        batch: 10,
    };
    dispatcher.tick().await?;
    let after_first = rows(
        &db,
        "SELECT message_id, state, attempt_count, next_retry_at FROM outbox ORDER BY message_id",
    )?;
    assert!(
        after_first.iter().all(|r| r[3].is_string()),
        "next_retry_at not kept: {after_first:?}"
    );
    let (retries, series) = drain(&dispatcher, &db, &clock).await?;
    let outbox = rows(
        &db,
        "SELECT message_id, state, attempt_count FROM outbox ORDER BY message_id",
    )?;
    keep(
        "test-022-03.json",
        &json!({"after_first_delivery": after_first, "retry_times": retries, "outbox": outbox,
                "outbox_after_each_tick": series,
                "deliveries": transport.log.lock().map(|l| l.clone()).unwrap_or_default(),
                "tasks": rows(&db, "SELECT task_id, state, revision FROM tasks ORDER BY task_id")?,
                "events": rows(&db, "SELECT seq, kind, payload_json FROM events ORDER BY seq")?,
                "receipts": rows(&db, "SELECT consumer, message_id FROM receipts")?,
                "dead_letters": rows(&db, "SELECT payload_json FROM events WHERE kind = 'outbox.dead_letter'")?}),
    )?;
    assert_eq!(
        one(
            &db,
            &format!("SELECT count(*) FROM receipts WHERE consumer = 'qa' AND message_id = '{m}'")
        )?,
        json!(1)
    );
    assert_eq!(
        rows(
            &db,
            &format!("SELECT state, attempt_count FROM outbox WHERE message_id = '{m}'")
        )?,
        [[json!("ACKED"), json!(2)]]
    );
    assert_eq!(
        one(&db, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        json!("VERIFYING")
    );
    assert_eq!(
        rows(
            &db,
            &format!("SELECT state, attempt_count FROM outbox WHERE message_id = '{n}'")
        )?,
        [[json!("DEAD_LETTER"), json!(3)]]
    );
    assert_eq!(
        one(
            &db,
            &format!(
                "SELECT count(*) FROM events WHERE kind = 'outbox.dead_letter' AND json_extract(payload_json, '$.message_id') = '{n}'"
            )
        )?,
        json!(1)
    );
    // Both replies of m are the same bytes (the second from the receipt).
    let replies = transport.replies_of(&m);
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0], replies[1]);
    let reply: Value = serde_json::from_str(&replies[0])?;
    assert_eq!(reply["kind"], json!("ack"), "{reply}");
    Ok(())
}

#[tokio::test]
async fn test_022_04_candidate_ready_hands_the_same_candidate_to_qa_and_reviewer() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let outcome = store.accept_task_result(developer_result()?).await?;
    assert!(
        matches!(outcome, TaskResultOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    let messages = rows(
        &db,
        "SELECT recipient_role, payload_json FROM outbox ORDER BY recipient_role",
    )?;
    assert_eq!(messages.len(), 2);
    let recipients: BTreeSet<String> = messages
        .iter()
        .filter_map(|r| r[0].as_str().map(str::to_owned))
        .collect();
    assert_eq!(recipients, ["qa".to_owned(), "reviewer".to_owned()].into());
    let c2 = rows(
        &db,
        "SELECT candidate_id, git_commit, git_tree, spec_sha256, environment_sha256, test_plan_sha256 FROM candidates WHERE candidate_id = 'C2'",
    )?;
    let c2 = json!({"id": c2[0][0], "git_commit": c2[0][1], "git_tree": c2[0][2], "spec_sha256": c2[0][3],
                    "environment_sha256": c2[0][4], "test_plan_sha256": c2[0][5]});
    let mut validation = Vec::new();
    let mut candidates = Vec::new();
    for row in &messages {
        let raw: Value = serde_json::from_str(row[1].as_str().ok_or("no payload")?)?;
        let message: Message = protocol::decode(&raw)?;
        let package: ContextHandoff = protocol::decode(&Value::Object(message.payload.clone()))?;
        assert_eq!(message.kind, protocol::MessageKind::Handoff);
        assert_eq!(package.spec_sha256, sha(2));
        // The envelope carries the current fences: the role's epoch, the task's revision once VERIFYING, the spec
        // and the candidate (M2T04R-50).
        let epoch = if message.recipient_role == "qa" { 2 } else { 1 };
        assert_eq!(
            (
                message.role_epoch,
                package.role_epoch,
                message.expected_revision
            ),
            (epoch, epoch, 6)
        );
        assert_eq!(message.spec_sha256, sha(2));
        assert_eq!(message.candidate_id.as_deref(), Some("C2"));
        assert!(message.message_id.len() <= 160);
        assert!(!package.acceptance_ids.is_empty());
        assert_eq!(package.role, message.recipient_role);
        // No StartRun command in this world: the fixed account names the task and the candidate (M2T04R-63).
        assert!(
            package.goal.contains('D') && package.goal.contains("C2"),
            "{}",
            package.goal
        );
        validation.push(
            json!({"message_id": message.message_id, "message": "valid Message",
                               "package": "valid ContextHandoff", "errors": []}),
        );
        candidates.push(serde_json::to_value(&package.candidate)?);
    }
    assert_eq!(candidates[0], candidates[1]);
    assert_eq!(candidates[0], c2);
    assert_eq!(
        rows(&db, "SELECT state, revision FROM tasks WHERE task_id = 'D'")?,
        [[json!("VERIFYING"), json!(6)]]
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM candidates WHERE candidate_id = 'C2'"
        )?,
        json!("VERIFYING")
    );
    keep(
        "test-022-04.json",
        &json!({"handoffs": messages, "candidate_row": c2, "schema_validation": validation}),
    )?;
    // The same result again makes no more handoffs.
    let again = store.accept_task_result(developer_result()?).await?;
    assert!(
        matches!(again, TaskResultOutcome::Replayed { ref handoffs } if handoffs.len() == 2),
        "{again:?}"
    );
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(2));
    // Both delivered and acknowledged: an ACK moves no task.
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock)
        .with("qa", Noop)
        .with("reviewer", Noop);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    drain(&dispatcher, &db, &clock).await?;
    assert_eq!(
        rows(&db, "SELECT DISTINCT state FROM outbox")?,
        [[json!("ACKED")]]
    );
    // Both consumers took their handoff (an ack, not a rejection that also settles ACKED).
    assert_eq!(
        rows(
            &db,
            "SELECT DISTINCT json_extract(result_json, '$.kind') FROM receipts"
        )?,
        [[json!("ack")]]
    );
    assert_ne!(
        one(&db, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        json!("ACCEPTED")
    );
    Ok(())
}

#[tokio::test]
async fn test_022_05_stale_epoch_spec_candidate_and_an_empty_handoff_are_refused() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let stale = [
        (
            to_qa("q-epoch", 3, 1, &sha(2), None)?,
            RejectionReason::StaleRoleEpoch,
        ),
        (
            to_qa("q-spec", 3, 2, &sha(1), None)?,
            RejectionReason::SpecMismatch,
        ),
        (
            to_qa("q-candidate", 3, 2, &sha(2), Some("C1"))?,
            RejectionReason::CandidateMismatch,
        ),
    ];
    enqueue(
        &store,
        "stale",
        &stale.iter().map(|(m, _)| m.clone()).collect::<Vec<_>>(),
    )
    .await?;
    // A handoff with no acceptance criteria cannot be queued by the product; one that got into the outbox anyway.
    let empty = json!({
        "schema_version": "1.0", "message_id": "q-empty", "run_id": "R", "task_id": "T", "attempt_id": "-",
        "sender_role": "runtime", "recipient_role": "qa", "role_epoch": 2, "correlation_id": "C2",
        "causation_id": null, "idempotency_key": "q-empty", "expected_revision": 3, "spec_sha256": sha(2),
        "candidate_id": "C2", "kind": "handoff",
        "payload": {
            "schema_version": "1.0", "run_id": "R", "task_id": "T", "goal": "verify", "role": "qa", "role_epoch": 2,
            "spec_sha256": sha(2),
            "candidate": {"id": "C2", "git_commit": format!("{:040}", 2), "git_tree": format!("{:040}", 12),
                          "spec_sha256": sha(2), "environment_sha256": sha(3), "test_plan_sha256": sha(4)},
            "acceptance_ids": [], "references": [], "output_contract": "TaskResult", "budget_tokens": 1000,
            "handoff_kind": "verification", "spec_revision": 2
        }
    });
    seed(
        &db,
        &format!(
            "INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
             VALUES ('q-empty', (SELECT max(seq) FROM events), 'R', 'qa', 2, '{}', 'PENDING');",
            empty.to_string().replace('\'', "''")
        ),
    )?;
    let before = (
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        one(&db, "SELECT count(*) FROM acceptance_results")?,
    );
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    let tick = dispatcher.tick().await?;
    assert_eq!(tick.invalid, ["q-empty"]);
    for (message, reason) in &stale {
        let reply: Value = serde_json::from_str(
            one(
                &db,
                &format!(
                    "SELECT result_json FROM receipts WHERE message_id = '{}'",
                    message.message_id
                ),
            )?
            .as_str()
            .ok_or("no receipt")?,
        )?;
        assert_eq!(reply["kind"], json!("rejected"), "{reply}");
        assert_eq!(
            reply["reason_code"],
            serde_json::to_value(reason)?,
            "{reply}"
        );
    }
    let after = (
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        one(&db, "SELECT count(*) FROM acceptance_results")?,
    );
    assert_eq!(after, before);
    assert_eq!(
        one(&db, "SELECT count(*) FROM events WHERE kind = 'qa.touched'")?,
        json!(0)
    );
    let refusals = rows(
        &db,
        "SELECT kind, json_extract(payload_json, '$.message_id') FROM events WHERE kind IN ('message.rejected', 'message.invalid') ORDER BY seq",
    )?;
    assert_eq!(refusals.len(), 4, "{refusals:?}");
    assert_eq!(
        one(&db, "SELECT state FROM outbox WHERE message_id = 'q-empty'")?,
        json!("DEAD_LETTER")
    );
    keep(
        "test-022-05.json",
        &json!({"refusal_events": refusals, "receipts": rows(&db, "SELECT message_id, result_json FROM receipts")?,
                "task_revision_and_acceptance_results": [before, after],
                "messages": rows(&db, "SELECT message_id, payload_json FROM outbox ORDER BY message_id")?}),
    )?;
    Ok(())
}

#[tokio::test]
async fn test_062_03_redelivery_applies_once_and_a_bad_foreign_key_rolls_back_its_event() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let m = to_qa("M", 3, 2, &sha(2), None)?;
    let bad = to_qa("M-bad", 4, 2, &sha(2), None)?;
    enqueue(&store, "M", &[m.clone(), bad.clone()]).await?;
    let (revision, events) = (
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        one(&db, "SELECT count(*) FROM events")?,
    );
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let transport = FakeTransport::new(&local, Some("qa"), None);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &transport,
        roles: local.roles(),
        max_attempts: 5,
        batch: 1,
    };
    // M only: the first delivery's ACK is lost, the second is answered from the receipt.
    dispatcher.tick().await?;
    clock.set(
        hoh_core::time::parse_rfc3339(
            one(
                &db,
                "SELECT next_retry_at FROM outbox WHERE message_id = 'M'",
            )?
            .as_str()
            .ok_or("no retry")?,
        )
        .ok_or("bad time")?,
    );
    dispatcher.tick().await?;
    let replies = transport.replies_of("M");
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(
        replies[0], replies[1],
        "the second delivery answered differently"
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM receipts WHERE consumer = 'qa' AND message_id = 'M'"
        )?,
        json!(1)
    );
    assert_eq!(
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        json!(revision.as_i64().unwrap_or(0) + 1)
    );
    let after_m = one(&db, "SELECT count(*) FROM events")?;
    // One transaction: a valid event, then an attempt of a task that does not exist.
    let effect = ConsumeEffect {
        events: vec![NewEvent {
            project_id: "p1".to_owned(),
            event_id: "bad/valid-event".to_owned(),
            run_id: Some("R".to_owned()),
            kind: "qa.valid".to_owned(),
            payload: json!({}),
        }],
        attempts: vec![NewAttempt {
            attempt_id: "bad-attempt".to_owned(),
            task_id: "no-such-task".to_owned(),
            session_id: None,
            role_epoch: 2,
            host_epoch: 1,
            config_revision: 1,
            agent: None,
        }],
        ..ConsumeEffect::default()
    };
    let refused = store.consume("qa", bad, effect, clock.now()).await;
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    assert_eq!(one(&db, "SELECT count(*) FROM events")?, after_m);
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE event_id = 'bad/valid-event'"
        )?,
        json!(0)
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM receipts WHERE message_id = 'M-bad'"
        )?,
        json!(0)
    );
    keep(
        "test-062-03.json",
        &json!({"revision_before": revision, "events_before": events, "events_after_m": after_m,
                "replies": replies, "receipts": rows(&db, "SELECT consumer, message_id FROM receipts")?,
                "bad_write": format!("{refused:?}"),
                "tasks": rows(&db, "SELECT task_id, state, revision FROM tasks ORDER BY task_id")?,
                "events": rows(&db, "SELECT seq, event_id, kind FROM events ORDER BY seq")?}),
    )?;
    Ok(())
}

#[tokio::test]
async fn a_stale_task_result_is_refused_once_and_changes_nothing() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let mut stale = developer_result()?;
    stale.role_epoch = 9;
    for _ in 0..2 {
        let outcome = store.accept_task_result(stale.clone()).await?;
        assert!(
            matches!(
                outcome,
                TaskResultOutcome::Rejected {
                    reason: RejectionReason::StaleRoleEpoch,
                    ..
                }
            ),
            "{outcome:?}"
        );
    }
    assert_eq!(
        one(&db, "SELECT state FROM tasks WHERE task_id = 'D'")?,
        json!("RUNNING")
    );
    assert_eq!(
        one(
            &db,
            "SELECT state FROM candidates WHERE candidate_id = 'C2'"
        )?,
        json!("FROZEN")
    );
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(0));
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'task_result.rejected'"
        )?,
        json!(1)
    );
    // The older candidate is refused too (from another attempt of the task, so nothing is a replay).
    seed(
        &db,
        "INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('A2', 'D', 'RUNNING', 1, 1, 1);
         UPDATE leases SET attempt_id = 'A2', epoch = 2 WHERE kind = 'writer';",
    )?;
    let mut old = developer_result()?;
    old.attempt_id = "A2".to_owned();
    old.candidate_id = Some("C1".to_owned());
    let outcome = store.accept_task_result(old).await?;
    assert!(
        matches!(
            outcome,
            TaskResultOutcome::Rejected {
                reason: RejectionReason::SpecMismatch,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(0));
    Ok(())
}

#[tokio::test]
async fn only_due_messages_of_served_roles_are_taken_and_settled_ones_never_again() -> R {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let mut other = to_qa("r1", 3, 1, &sha(2), None)?;
    other.recipient_role = "researcher".to_owned();
    enqueue(&store, "two", &[to_qa("q1", 3, 2, &sha(2), None)?, other]).await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    let tick = dispatcher.tick().await?;
    assert_eq!(tick.delivered, ["q1"]);
    assert_eq!(
        rows(
            &db,
            "SELECT message_id, state, attempt_count FROM outbox ORDER BY message_id"
        )?,
        [
            [json!("q1"), json!("ACKED"), json!(1)],
            [json!("r1"), json!("PENDING"), json!(0)]
        ]
    );
    clock.set(clock.now() + Duration::from_secs(3600));
    assert_eq!(
        dispatcher.tick().await?,
        hoh_core::delivery::Tick::default()
    );
    Ok(())
}

#[test]
fn a_handoff_without_acceptance_criteria_is_never_queued() -> R {
    let mut handoff = to_qa("h", 3, 2, &sha(2), Some("C2"))?;
    handoff.kind = protocol::MessageKind::Handoff;
    let package = json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T", "goal": "verify", "role": "qa", "role_epoch": 2,
        "spec_sha256": sha(2),
        "candidate": {"id": "C2", "git_commit": format!("{:040}", 2), "git_tree": format!("{:040}", 12),
                      "spec_sha256": sha(2), "environment_sha256": sha(3), "test_plan_sha256": sha(4)},
        "acceptance_ids": ["AC-022-04"], "references": [], "output_contract": "TaskResult", "budget_tokens": 1000,
        "handoff_kind": "verification", "spec_revision": 2
    });
    let Value::Object(mut payload) = package else {
        return Err("not an object".into());
    };
    handoff.payload = payload.clone();
    assert!(NewOutbox::hcp(0, &handoff).is_ok());
    payload.insert("acceptance_ids".to_owned(), json!([]));
    handoff.payload = payload;
    assert!(matches!(
        NewOutbox::hcp(0, &handoff),
        Err(PersistError::IntegrityViolation(_))
    ));
    Ok(())
}

#[tokio::test]
async fn a_late_failure_of_an_earlier_delivery_does_not_undo_a_later_success() -> R {
    // M2T04R-01: generation 1 is taken, given up after its backoff, and generation 2 is delivered and settled; the
    // answer of generation 1 that comes late (a failure) must not dead-letter the message.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "late", &[to_qa("L", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let qa = vec!["qa".to_owned()];
    let first = store
        .claim_outbox(clock.now(), qa.clone(), 2)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or("nothing due")?;
    clock.set(
        hoh_core::time::parse_rfc3339(first.next_retry_at.as_deref().ok_or("no retry")?)
            .ok_or("bad time")?,
    );
    let second = store
        .claim_outbox(clock.now(), qa.clone(), 2)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or("not due again")?;
    assert_eq!((first.attempt_count, second.attempt_count), (1, 2));
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    local.deliver(&second).await.map_err(|e| format!("{e:?}"))?;
    // The late failure first, then the success: and the other way round in a second message below.
    store
        .settle_outbox(
            "L",
            1,
            hoh_core::persistence::Settlement::Failed("late".to_owned()),
            2,
        )
        .await?;
    store
        .settle_outbox("L", 2, hoh_core::persistence::Settlement::Delivered, 2)
        .await?;
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'L'"
        )?,
        [[json!("ACKED"), json!(2)]]
    );
    store
        .settle_outbox(
            "L",
            1,
            hoh_core::persistence::Settlement::Failed("later still".to_owned()),
            2,
        )
        .await?;
    assert_eq!(
        one(&db, "SELECT state FROM outbox WHERE message_id = 'L'")?,
        json!("ACKED")
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'outbox.dead_letter'"
        )?,
        json!(0)
    );
    Ok(())
}

/// A transport whose delivery of `stuck` never returns.
struct Stuck<'a, T: Transport> {
    inner: &'a T,
    stuck: &'static str,
    calls: Mutex<Vec<String>>,
}

impl<T: Transport> Transport for Stuck<'_, T> {
    async fn deliver(&self, row: &OutboxRow) -> Result<String, DeliveryError> {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(row.message_id.clone());
        }
        if row.message_id == self.stuck {
            std::future::pending::<()>().await;
        }
        self.inner.deliver(row).await
    }
}

#[tokio::test]
async fn deliveries_taken_and_never_settled_stop_at_the_limit_and_cost_no_other_message() -> R {
    // M2T04R-02, -03: each tick hangs on `a` and is cancelled; `b`, behind it, must not lose deliveries it never got,
    // and `a` must go to DEAD_LETTER once its deliveries are spent, not be taken forever.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(
        &store,
        "ab",
        &[
            to_qa("a", 3, 2, &sha(2), None)?,
            to_qa("b", 3, 2, &sha(2), None)?,
        ],
    )
    .await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let transport = Stuck {
        inner: &local,
        stuck: "a",
        calls: Mutex::new(Vec::new()),
    };
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &transport,
        roles: local.roles(),
        max_attempts: 3,
        batch: 2,
    };
    for _ in 0..4 {
        let _ = tokio::time::timeout(Duration::from_millis(200), dispatcher.tick()).await;
        clock.set(clock.now() + Duration::from_secs(120));
    }
    let calls = transport
        .calls
        .lock()
        .map(|c| c.clone())
        .unwrap_or_default();
    assert_eq!(
        calls,
        ["a", "a", "a", "b"],
        "the transport was called {calls:?}"
    );
    assert_eq!(
        rows(
            &db,
            "SELECT message_id, state, attempt_count FROM outbox ORDER BY message_id"
        )?,
        [
            [json!("a"), json!("DEAD_LETTER"), json!(3)],
            [json!("b"), json!("ACKED"), json!(1)]
        ]
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'outbox.dead_letter'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn runtime_notices_in_the_outbox_are_left_for_their_own_consumer() -> R {
    // M2T04R-05: decide's and the round's dispatch notices and PauseRun's run.control are no HCP messages; the HCP
    // dispatcher neither delivers nor dead-letters them.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: "notice".to_owned(),
                run_id: Some("R".to_owned()),
                kind: "test.notice".to_owned(),
                payload: json!({}),
            }],
            outbox: vec![NewOutbox {
                message_id: "T/attempt-1/dispatch".to_owned(),
                event: 0,
                run_id: Some("R".to_owned()),
                recipient_role: "qa".to_owned(),
                role_epoch: 2,
                payload: json!({"notice": "runtime", "kind": "dispatch", "task_id": "T", "role": "qa", "attempt_id": "T/attempt-1"}),
            }],
            ..Commit::default()
        })
        .await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 3,
        batch: 10,
    };
    assert_eq!(
        dispatcher.tick().await?,
        hoh_core::delivery::Tick::default()
    );
    assert_eq!(
        rows(&db, "SELECT state, attempt_count FROM outbox")?,
        [[json!("PENDING"), json!(0)]]
    );
    Ok(())
}

/// A valid verification handoff to qa about T at revision 3 with C2 (the current candidate).
fn qa_handoff(id: &str) -> R<Message> {
    let mut m = to_qa(id, 3, 2, &sha(2), Some("C2"))?;
    m.kind = protocol::MessageKind::Handoff;
    let Value::Object(payload) = json!({
        "schema_version": "1.0", "run_id": "R", "task_id": "T", "goal": "verify", "role": "qa", "role_epoch": 2,
        "spec_sha256": sha(2),
        "candidate": {"id": "C2", "git_commit": format!("{:040}", 2), "git_tree": format!("{:040}", 12),
                      "spec_sha256": sha(2), "environment_sha256": sha(3), "test_plan_sha256": sha(4)},
        "acceptance_ids": ["AC-022-04"], "references": [], "output_contract": "TaskResult", "budget_tokens": 1000,
        "handoff_kind": "verification", "spec_revision": 2
    }) else {
        return Err("not an object".into());
    };
    m.payload = payload;
    Ok(m)
}

#[tokio::test]
async fn a_handoffs_package_is_fenced_in_full_and_must_agree_with_its_envelope() -> R {
    // M2T04R-04, -56: the candidate is fenced by its whole identity and never skipped by a null envelope; the package
    // agrees with its envelope (run, task, role, epoch, specification) and its specification revision is current.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let valid = qa_handoff("h-ok")?;
    assert!(NewOutbox::hcp(0, &valid).is_ok());
    let mut null_envelope = qa_handoff("h-null")?;
    null_envelope.candidate_id = None;
    let mut other_task = qa_handoff("h-task")?;
    other_task.payload.insert("task_id".to_owned(), json!("D"));
    let mut other_epoch = qa_handoff("h-epoch")?;
    other_epoch
        .payload
        .insert("role_epoch".to_owned(), json!(1));
    for bad in [&null_envelope, &other_task, &other_epoch] {
        assert!(
            NewOutbox::hcp(0, bad).is_err(),
            "{} was queued",
            bad.message_id
        );
    }
    let mut tree = qa_handoff("h-tree")?;
    tree.payload["candidate"]["git_tree"] = json!(format!("{:040}", 99));
    let mut revision = qa_handoff("h-revision")?;
    revision
        .payload
        .insert("spec_revision".to_owned(), json!(1));
    enqueue(&store, "packages", &[tree.clone(), revision.clone()]).await?;
    // The null-envelope one got into the outbox anyway: delivered, it is invalid, not applied.
    seed(
        &db,
        &format!(
            "INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
             VALUES ('h-null', (SELECT max(seq) FROM events), 'R', 'qa', 2, '{}', 'PENDING');",
            protocol::encode(&null_envelope)?
                .to_string()
                .replace('\'', "''")
        ),
    )?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    let tick = dispatcher.tick().await?;
    assert_eq!(tick.invalid, ["h-null"]);
    for (id, reason) in [
        ("h-tree", "CANDIDATE_MISMATCH"),
        ("h-revision", "SPEC_MISMATCH"),
    ] {
        let reply: Value = serde_json::from_str(
            one(
                &db,
                &format!("SELECT result_json FROM receipts WHERE message_id = '{id}'"),
            )?
            .as_str()
            .ok_or("no receipt")?,
        )?;
        assert_eq!(
            (&reply["kind"], &reply["reason_code"]),
            (&json!("rejected"), &json!(reason)),
            "{id}: {reply}"
        );
    }
    assert_eq!(
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        json!(3)
    );
    assert_eq!(
        one(&db, "SELECT count(*) FROM events WHERE kind = 'qa.touched'")?,
        json!(0)
    );
    Ok(())
}

#[tokio::test]
async fn a_message_whose_payload_is_not_its_kinds_is_never_queued_nor_consumed() -> R {
    // M2T04R-06: kind=result with a steer payload; the store refuses to queue it and to consume it directly.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let mut result = to_qa("r-bad", 3, 2, &sha(2), None)?;
    result.kind = protocol::MessageKind::Result;
    assert!(NewOutbox::hcp(0, &result).is_err());
    // Queued as a steer message, consumed directly as a result.
    enqueue(&store, "r", &[to_qa("r-bad", 3, 2, &sha(2), None)?]).await?;
    let refused = store
        .consume(
            "qa",
            result,
            Touch.effect(&to_qa("r-bad", 3, 2, &sha(2), None)?),
            FakeClock::new().now(),
        )
        .await;
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    assert_eq!(
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        json!(3)
    );
    assert_eq!(one(&db, "SELECT count(*) FROM receipts")?, json!(0));
    Ok(())
}

/// A fresh store on WORLD plus `extra`.
async fn world_with(extra: &str) -> R<(tempfile::TempDir, Store, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    if !extra.is_empty() {
        seed(&db, extra)?;
    }
    Ok((dir, store, db))
}

/// Nothing of D's verification was written: no outbox row or event, D RUNNING at 4, C2 FROZEN.
fn nothing_written(db: &Path) -> R {
    assert_eq!(one(db, "SELECT count(*) FROM outbox")?, json!(0));
    assert_eq!(
        one(
            db,
            "SELECT count(*) FROM events WHERE kind = 'candidate_ready'"
        )?,
        json!(0)
    );
    assert_eq!(
        rows(db, "SELECT state, revision FROM tasks WHERE task_id = 'D'")?,
        [[json!("RUNNING"), json!(4)]]
    );
    assert_eq!(
        one(db, "SELECT state FROM candidates WHERE candidate_id = 'C2'")?,
        json!("FROZEN")
    );
    Ok(())
}

#[tokio::test]
async fn a_task_result_is_taken_only_from_the_live_attempt_for_the_current_candidate() -> R {
    // M2T04R-51..55.
    let refused = |outcome: &TaskResultOutcome| match outcome {
        TaskResultOutcome::Rejected { reason, .. } => Some(*reason),
        _ => None,
    };
    // No reviewer bound, or no acceptance criteria: refused whole, nothing written.
    for extra in [
        "DELETE FROM role_bindings WHERE role = 'reviewer';",
        "DELETE FROM task_requirements WHERE task_id = 'D';",
    ] {
        let (_dir, store, db) = world_with(extra).await?;
        assert!(
            store.accept_task_result(developer_result()?).await.is_err(),
            "{extra}"
        );
        nothing_written(&db)?;
    }
    let c3 = "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, \
              test_plan_sha256, source_path, state, created_at) VALUES ('C3', 'R', printf('%040d', 3), \
              printf('%040d', 13), printf('%064d', 2), printf('%064d', 3), printf('%064d', 4), '/c3', 'FROZEN', \
              '2026-11-07T00:00:03Z');";
    let newer_attempt = "INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) \
                         VALUES ('A9', 'D', 'RUNNING', 1, 1, 1); UPDATE leases SET attempt_id = 'A9', epoch = 2;";
    // The task is not the result's role's; the candidate is no longer FROZEN; a newer candidate is current; a late
    // result of a failed attempt, of one that ran at another epoch, of one that lost its lease.
    let cases = [
        ("", true, RejectionReason::IllegalTransition),
        (
            "UPDATE candidates SET state = 'VERIFYING' WHERE candidate_id = 'C2';",
            false,
            RejectionReason::CandidateMismatch,
        ),
        (c3, false, RejectionReason::CandidateMismatch),
        (
            "UPDATE attempts SET state = 'FAILED' WHERE attempt_id = 'A';",
            false,
            RejectionReason::StaleLeaseEpoch,
        ),
        (
            "UPDATE attempts SET role_epoch = 7 WHERE attempt_id = 'A';",
            false,
            RejectionReason::StaleRoleEpoch,
        ),
        (newer_attempt, false, RejectionReason::StaleLeaseEpoch),
    ];
    for (extra, as_reviewer, want) in cases {
        let (_dir, store, db) = world_with(extra).await?;
        let mut result = developer_result()?;
        if as_reviewer {
            result.role = "reviewer".to_owned();
        }
        let outcome = store.accept_task_result(result).await?;
        assert_eq!(
            refused(&outcome),
            Some(want),
            "{extra} {as_reviewer}: {outcome:?}"
        );
        assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(0));
        assert_eq!(
            one(&db, "SELECT state FROM tasks WHERE task_id = 'D'")?,
            json!("RUNNING")
        );
    }
    // The same attempt's result with another payload is a reuse of its key, not a replay.
    let (_dir, store, _db) = world_with("").await?;
    store.accept_task_result(developer_result()?).await?;
    let mut other = developer_result()?;
    other.summary = "something else".to_owned();
    assert!(matches!(
        store.accept_task_result(other).await,
        Err(PersistError::KeyReuse(_))
    ));
    // A candidate id as long as the contract allows still makes valid, bounded message ids.
    let long = "C".repeat(150);
    let (_dir, store, db) = world_with(&format!(
        "INSERT INTO candidates(candidate_id, run_id, git_commit, git_tree, spec_sha256, environment_sha256, \
         test_plan_sha256, source_path, state, created_at) VALUES ('{long}', 'R', printf('%040d', 5), \
         printf('%040d', 15), printf('%064d', 2), printf('%064d', 3), printf('%064d', 4), '/c5', 'FROZEN', \
         '2026-11-07T00:00:09Z');"
    ))
    .await?;
    let mut result = developer_result()?;
    result.candidate_id = Some(long);
    let outcome = store.accept_task_result(result).await?;
    let TaskResultOutcome::Accepted { handoffs, .. } = outcome else {
        return Err(format!("not accepted: {outcome:?}").into());
    };
    assert!(handoffs.iter().all(|h| h.len() <= 160), "{handoffs:?}");
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(2));
    Ok(())
}

#[tokio::test]
async fn an_invalid_message_recorded_twice_is_one_event() -> R {
    // M2T04R-59: recorded, then the process stops before the row is settled; recording it again is not an error.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let first = store
        .record_invalid_message("qa", "x", Some("R"), "broken")
        .await?;
    let again = store
        .record_invalid_message("qa", "x", Some("R"), "broken")
        .await?;
    assert_eq!(first, again);
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'message.invalid'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn a_row_that_should_hold_a_message_and_does_not_is_recorded_not_skipped() -> R {
    // M2T04R-101: only rows marked as runtime notices are left alone; one missing schema_version is invalid.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let mut broken = protocol::encode(&to_qa("no-version", 3, 2, &sha(2), None)?)?;
    if let Some(o) = broken.as_object_mut() {
        o.remove("schema_version");
    }
    seed(
        &db,
        &format!(
            "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'e', 'R', 'x', '{{}}', 'now');
             INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
             VALUES ('no-version', (SELECT max(seq) FROM events), 'R', 'qa', 2, '{}', 'PENDING');",
            broken.to_string().replace('\'', "''")
        ),
    )?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    assert_eq!(dispatcher.tick().await?.invalid, ["no-version"]);
    assert_eq!(
        one(
            &db,
            "SELECT state FROM outbox WHERE message_id = 'no-version'"
        )?,
        json!("DEAD_LETTER")
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'message.invalid'"
        )?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn a_claim_whose_dispatcher_stopped_waiting_costs_no_delivery() -> R {
    // M2T04R-100: the writer is held up by another connection's lock; each tick is cancelled while it waits for its
    // claim. No delivery reached the transport, so none is counted, and the message is delivered once afterwards.
    use std::io::BufRead as _;
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "c", &[to_qa("c", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let transport = FakeTransport::new(&local, None, None);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &transport,
        roles: local.roles(),
        max_attempts: 3,
        batch: 1,
    };
    for _ in 0..3 {
        let mut holder = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys,time\nc=sqlite3.connect(sys.argv[1],isolation_level=None,timeout=30)\nc.execute('BEGIN IMMEDIATE')\nprint('locked',flush=True)\ntime.sleep(0.6)\nc.execute('COMMIT')",
            ])
            .arg(&db)
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufReader::new(holder.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
        assert_eq!(line.trim(), "locked");
        let cancelled = tokio::time::timeout(Duration::from_millis(150), dispatcher.tick()).await;
        assert!(cancelled.is_err(), "the tick was not held up");
        holder.wait()?;
        // The cancelled claim was returned when its answer could not be handed over; the next claim undoes it in its own
        // transaction (M2T04R-350, -453).
        store.claim_outbox(clock.now(), Vec::new(), 3).await?;
        clock.set(clock.now() + Duration::from_secs(120));
    }
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'c'"
        )?,
        [[json!("PENDING"), json!(0)]]
    );
    dispatcher.tick().await?;
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'c'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    assert_eq!(transport.replies_of("c").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_message_consumed_directly_must_be_a_valid_message_as_a_whole() -> R {
    // M2T04R-102: the envelope too, not only the payload.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let m = to_qa("v", 3, 2, &sha(2), None)?;
    enqueue(&store, "v", std::slice::from_ref(&m)).await?;
    let mut bad = m.clone();
    bad.schema_version = "9.9".to_owned();
    let refused = store
        .consume("qa", bad, Touch.effect(&m), FakeClock::new().now())
        .await;
    assert!(
        matches!(refused, Err(PersistError::IntegrityViolation(_))),
        "{refused:?}"
    );
    assert_eq!(
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        json!(3)
    );
    assert_eq!(one(&db, "SELECT count(*) FROM receipts")?, json!(0));
    Ok(())
}

#[tokio::test]
async fn a_late_answer_to_a_dead_lettered_message_makes_it_acked() -> R {
    // M2T04R-103: generation 1 is consumed but its answer is late; generation 2 fails at the limit (DEAD_LETTER); then
    // generation 1's answer arrives: the receipt proves the delivery.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "late", &[to_qa("D2", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let qa = vec!["qa".to_owned()];
    let first = store
        .claim_outbox(clock.now(), qa.clone(), 2)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or("nothing due")?;
    LocalTransport::new(&store, &clock)
        .with("qa", Touch)
        .deliver(&first)
        .await
        .map_err(|e| format!("{e:?}"))?;
    clock.set(clock.now() + Duration::from_secs(60));
    let second = store
        .claim_outbox(clock.now(), qa, 2)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or("not due again")?;
    store
        .settle_outbox(
            "D2",
            second.attempt_count,
            hoh_core::persistence::Settlement::Failed("lost".to_owned()),
            2,
        )
        .await?;
    assert_eq!(
        one(&db, "SELECT state FROM outbox WHERE message_id = 'D2'")?,
        json!("DEAD_LETTER")
    );
    store
        .settle_outbox(
            "D2",
            first.attempt_count,
            hoh_core::persistence::Settlement::Delivered,
            2,
        )
        .await?;
    assert_eq!(
        one(&db, "SELECT state FROM outbox WHERE message_id = 'D2'")?,
        json!("ACKED")
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'outbox.delivered_late'"
        )?,
        json!(1)
    );
    assert_eq!(
        one(&db, "SELECT count(*) FROM receipts WHERE message_id = 'D2'")?,
        json!(1)
    );
    Ok(())
}

#[tokio::test]
async fn a_settled_attempt_is_live_even_after_its_slot_went_to_another_task() -> R {
    // M2T04R-150: the lease row is the project's slot; another task's attempt holding it now does not supersede A.
    let (_dir, store, db) = world_with(
        "INSERT INTO tasks(task_id, run_id, spec_id, state, revision, role, writer, created_seq) VALUES ('O', 'R', 's2', 'LEASED', 1, 'developer', 1, 3);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('AO', 'O', 'PREPARED', 1, 1, 1);
         UPDATE attempts SET state = 'SETTLED' WHERE attempt_id = 'A';
         UPDATE leases SET attempt_id = 'AO', epoch = 2;",
    )
    .await?;
    let outcome = store.accept_task_result(developer_result()?).await?;
    assert!(
        matches!(outcome, TaskResultOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(2));
    Ok(())
}

#[tokio::test]
async fn the_remaining_refusals_of_results_and_packages_write_nothing() -> R {
    // M2T04R-151.
    // A refused result again with another payload is a reuse of its key.
    let (_dir, store, db) = world_with("").await?;
    let mut stale = developer_result()?;
    stale.role_epoch = 9;
    store.accept_task_result(stale.clone()).await?;
    stale.summary = "other".to_owned();
    assert!(matches!(
        store.accept_task_result(stale).await,
        Err(PersistError::KeyReuse(_))
    ));
    // The task is not RUNNING.
    let (_dir, store, db2) =
        world_with("UPDATE tasks SET state = 'LEASED' WHERE task_id = 'D';").await?;
    let outcome = store.accept_task_result(developer_result()?).await?;
    assert!(
        matches!(
            outcome,
            TaskResultOutcome::Rejected {
                reason: RejectionReason::IllegalTransition,
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(one(&db2, "SELECT count(*) FROM outbox")?, json!(0));
    // Packages that disagree with their envelope in run, role or specification; a result for another task.
    let mut run = qa_handoff("p-run")?;
    run.payload.insert("run_id".to_owned(), json!("R2"));
    let mut role = qa_handoff("p-role")?;
    role.payload.insert("role".to_owned(), json!("reviewer"));
    let mut spec = qa_handoff("p-spec")?;
    spec.payload.insert("spec_sha256".to_owned(), json!(sha(1)));
    let mut result = to_qa("p-result", 3, 2, &sha(2), None)?;
    result.kind = protocol::MessageKind::Result;
    let mut other = serde_json::to_value(developer_result()?)?;
    other["task_id"] = json!("T-other");
    let Value::Object(payload) = other else {
        return Err("not an object".into());
    };
    result.payload = payload;
    for bad in [&run, &role, &spec, &result] {
        assert!(
            NewOutbox::hcp(0, bad).is_err(),
            "{} was queued",
            bad.message_id
        );
        let refused = store
            .consume("qa", bad.clone(), Touch.effect(bad), FakeClock::new().now())
            .await;
        assert!(
            matches!(refused, Err(PersistError::IntegrityViolation(_))),
            "{}: {refused:?}",
            bad.message_id
        );
    }
    assert_eq!(one(&db2, "SELECT count(*) FROM receipts")?, json!(0));
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(0));
    Ok(())
}

#[tokio::test]
async fn a_claim_answered_but_never_read_returns_itself() -> R {
    // M2T04R-200: the writer has sent the claim's answer; the tick is dropped before it reads it. The answer dies with
    // its channel, and the claim with it.
    use std::io::BufRead as _;
    use std::task::{Context, Waker};
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "u", &[to_qa("u", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 3,
        batch: 1,
    };
    for _ in 0..3 {
        let mut holder = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys,time\nc=sqlite3.connect(sys.argv[1],isolation_level=None,timeout=30)\nc.execute('BEGIN IMMEDIATE')\nprint('locked',flush=True)\ntime.sleep(0.4)\nc.execute('COMMIT')",
            ])
            .arg(&db)
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufReader::new(holder.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
        // Boxed, so that dropping it drops the future (a pinned local would live to the end of the loop body).
        let mut tick = Box::pin(dispatcher.tick());
        assert!(
            tick.as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        holder.wait()?;
        // Queued behind the claim: when it returns, the claim's answer is waiting, unread, in its channel.
        store.claim_outbox(clock.now(), Vec::new(), 3).await?;
        // The precondition: the claim did run and count (M2T04R-351).
        assert_eq!(
            rows(
                &db,
                "SELECT state, attempt_count FROM outbox WHERE message_id = 'u'"
            )?,
            [[json!("DELIVERING"), json!(1)]]
        );
        drop(tick);
        // The next claim undoes the returned claim in its own transaction.
        store.claim_outbox(clock.now(), Vec::new(), 3).await?;
        assert_eq!(
            rows(
                &db,
                "SELECT state, attempt_count FROM outbox WHERE message_id = 'u'"
            )?,
            [[json!("PENDING"), json!(0)]]
        );
        clock.set(clock.now() + Duration::from_secs(120));
    }
    dispatcher.tick().await?;
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'u'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    Ok(())
}

#[tokio::test]
async fn only_the_runtime_mark_keeps_a_row_from_its_check() -> R {
    // M2T04R-201: notice false, 1 or another string is no runtime notice.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let mut sql = String::from(
        "INSERT INTO events(project_id, event_id, run_id, kind, payload_json, created_at) VALUES ('p1', 'e', 'R', 'x', '{}', 'now');\n",
    );
    for (id, mark) in [
        ("n-false", json!(false)),
        ("n-one", json!(1)),
        ("n-other", json!("other")),
    ] {
        let mut payload = protocol::encode(&to_qa(id, 3, 2, &sha(2), None)?)?;
        payload["notice"] = mark;
        sql.push_str(&format!(
            "INSERT INTO outbox(message_id, event_seq, run_id, recipient_role, role_epoch, payload_json, state) \
             VALUES ('{id}', (SELECT max(seq) FROM events), 'R', 'qa', 2, '{}', 'PENDING');\n",
            payload.to_string().replace('\'', "''")
        ));
    }
    seed(&db, &sql)?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Touch);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &local,
        roles: local.roles(),
        max_attempts: 5,
        batch: 10,
    };
    let tick = dispatcher.tick().await?;
    assert_eq!(tick.invalid, ["n-false", "n-one", "n-other"]);
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM events WHERE kind = 'message.invalid'"
        )?,
        json!(3)
    );
    Ok(())
}

#[tokio::test]
async fn a_package_that_disagrees_with_its_envelope_is_refused_when_consumed_directly() -> R {
    // M2T04R-250: queued as valid messages (so the receipt's foreign key holds), consumed as variants that disagree.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    let ids = ["d-run", "d-role", "d-spec"];
    let valid = ids.iter().map(|id| qa_handoff(id)).collect::<R<Vec<_>>>()?;
    enqueue(&store, "d", &valid).await?;
    for (mut bad, (key, value)) in valid.into_iter().zip([
        ("run_id", json!("R2")),
        ("role", json!("reviewer")),
        ("spec_sha256", json!(sha(1))),
    ]) {
        bad.payload.insert(key.to_owned(), value);
        let refused = store
            .consume(
                "qa",
                bad.clone(),
                Touch.effect(&bad),
                FakeClock::new().now(),
            )
            .await;
        assert!(
            matches!(&refused, Err(PersistError::IntegrityViolation(e)) if e.contains("disagrees")),
            "{key}: {refused:?}"
        );
    }
    assert_eq!(
        one(&db, "SELECT revision FROM tasks WHERE task_id = 'T'")?,
        json!(3)
    );
    assert_eq!(one(&db, "SELECT count(*) FROM receipts")?, json!(0));
    Ok(())
}

#[tokio::test]
async fn attempts_are_ordered_by_creation_not_by_their_ids() -> R {
    // M2T04R-252: D/attempt-10 is newer than D/attempt-9 although it sorts before it as text.
    let (_dir, store, db) = world_with(
        "INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('D/attempt-9', 'D', 'FAILED', 1, 1, 1);
         INSERT INTO attempts(attempt_id, task_id, state, role_epoch, host_epoch, config_revision) VALUES ('D/attempt-10', 'D', 'RUNNING', 1, 1, 1);",
    )
    .await?;
    let mut result = developer_result()?;
    result.attempt_id = "D/attempt-10".to_owned();
    let outcome = store.accept_task_result(result).await?;
    assert!(
        matches!(outcome, TaskResultOutcome::Accepted { .. }),
        "{outcome:?}"
    );
    assert_eq!(one(&db, "SELECT count(*) FROM outbox")?, json!(2));
    Ok(())
}

#[tokio::test]
async fn the_same_result_with_an_extreme_number_replays() -> R {
    // M2T04R-253: compared as stored text, a number that needs an exact float round trip still matches. It tells a
    // comparison of reparsed values apart only when hoh-core is built alone (`cargo test -p hoh-core`): in a workspace
    // build hoh-agents turns on serde_json's float_roundtrip and reparsing is exact (M2T04R-352).
    let (_dir, store, _db) = world_with("").await?;
    let mut result = developer_result()?;
    // The exact f64 (a literal), not a parse of text: reparsing its stored text without an exact round trip may give
    // a neighbour, which a comparison of reparsed values would take for another result.
    let Value::Object(issue) = json!({"weight": 1.234_567_890_123_456_7e-300_f64}) else {
        return Err("not an object".into());
    };
    result.issues = vec![issue];
    store.accept_task_result(result.clone()).await?;
    let again = store.accept_task_result(result).await?;
    assert!(
        matches!(again, TaskResultOutcome::Replayed { .. }),
        "{again:?}"
    );
    Ok(())
}

/// A store whose writer gives up on a busy database after `busy`.
fn open_busy(dir: &tempfile::TempDir, busy: Duration) -> R<(Store, PathBuf)> {
    open_with(
        dir,
        Options {
            busy_timeout: busy,
            ..Options::default()
        },
    )
}

/// Another connection holding the database until [`Holder::release`] (no sleep: it waits for a line, M2T04R-454).
struct Holder(std::process::Child);

impl Holder {
    fn hold(db: &Path) -> R<Holder> {
        use std::io::BufRead as _;
        let mut child = Command::new("python3")
            .args([
                "-c",
                "import sqlite3,sys\nc=sqlite3.connect(sys.argv[1],isolation_level=None,timeout=30)\nc.execute('BEGIN IMMEDIATE')\nprint('locked',flush=True)\nsys.stdin.readline()\nc.execute('COMMIT')",
            ])
            .arg(db)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufReader::new(child.stdout.take().ok_or("no stdout")?).read_line(&mut line)?;
        assert_eq!(line.trim(), "locked");
        Ok(Holder(child))
    }

    fn release(mut self) -> R {
        use std::io::Write as _;
        self.0.stdin.take().ok_or("no stdin")?.write_all(b"\n")?;
        self.0.wait()?;
        Ok(())
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn a_returned_claim_that_meets_a_busy_database_is_undone_by_a_later_claim() -> R {
    // M2T04R-300, -400, -450: the claim that should undo a returned claim cannot run while another connection holds
    // the database; the return is kept, and no claim of that row happens before its undo, even once it is due again.
    let dir = tempfile::tempdir()?;
    let (store, db) = open_busy(&dir, Duration::from_millis(80))?;
    seed(&db, WORLD)?;
    enqueue(&store, "b", &[to_qa("b", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let qa = vec!["qa".to_owned()];
    let claim = store
        .claim_outbox(clock.now(), qa.clone(), 3)
        .await?
        .ok_or("nothing due")?;
    drop(claim);
    let holder = Holder::hold(&db)?;
    assert!(matches!(
        store.claim_outbox(clock.now(), qa.clone(), 3).await,
        Err(PersistError::Busy(_))
    ));
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'b'"
        )?,
        [[json!("DELIVERING"), json!(1)]]
    );
    holder.release()?;
    // Past its backoff: the next claim undoes it first, then claims it as its first delivery.
    clock.set(clock.now() + Duration::from_secs(120));
    let again = store
        .claim_outbox(clock.now(), qa, 3)
        .await?
        .and_then(hoh_core::persistence::Claimed::start)
        .ok_or("not claimed again")?;
    assert_eq!(again.attempt_count, 1);
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    local.deliver(&again).await.map_err(|e| format!("{e:?}"))?;
    store
        .settle_outbox(
            "b",
            again.attempt_count,
            hoh_core::persistence::Settlement::Delivered,
            3,
        )
        .await?;
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'b'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    Ok(())
}

#[tokio::test]
async fn a_returned_claim_does_not_make_other_writes_wait_longer_for_a_busy_database() -> R {
    // M2T04R-401, -451: with a return waiting, a command still gets DB_BUSY after one busy timeout, not after two.
    let dir = tempfile::tempdir()?;
    let (store, db) = open_busy(&dir, Duration::from_millis(300))?;
    seed(&db, WORLD)?;
    enqueue(&store, "w", &[to_qa("w", 3, 2, &sha(2), None)?]).await?;
    let claim = store
        .claim_outbox(FakeClock::new().now(), vec!["qa".to_owned()], 3)
        .await?
        .ok_or("nothing due")?;
    drop(claim);
    let holder = Holder::hold(&db)?;
    let write = tokio::spawn({
        let started = std::time::Instant::now();
        let commit = Commit {
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: "w/event".to_owned(),
                run_id: Some("R".to_owned()),
                kind: "test.write".to_owned(),
                payload: json!({}),
            }],
            ..Commit::default()
        };
        let store = std::sync::Arc::new(store);
        let writer = store.clone();
        async move { (writer.commit(commit).await, started.elapsed(), store) }
    });
    // Released after one and a half busy timeouts: a write that also waited for the undo would succeed after it.
    tokio::time::sleep(Duration::from_millis(450)).await;
    holder.release()?;
    let (result, took, _store) = write.await?;
    assert!(
        matches!(result, Err(PersistError::Busy(_))),
        "{result:?} after {took:?}"
    );
    Ok(())
}

#[tokio::test]
async fn claims_left_unread_across_their_backoff_do_not_stack() -> R {
    // M2T04R-400: three ticks each take the claim and are never polled again while the clock passes the backoff;
    // a message with a live guard is not claimed again, so one delivery at most is pending, and none counts.
    use std::task::{Context, Waker};
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "k", &[to_qa("k", 3, 2, &sha(2), None)?]).await?;
    let clock = FakeClock::new();
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let transport = FakeTransport::new(&local, None, None);
    let dispatcher = Dispatcher {
        store: &store,
        clock: &clock,
        transport: &transport,
        roles: local.roles(),
        max_attempts: 3,
        batch: 1,
    };
    let mut ticks = Vec::new();
    for _ in 0..3 {
        let mut tick = Box::pin(dispatcher.tick());
        let _ = tick.as_mut().poll(&mut Context::from_waker(Waker::noop()));
        // Let the claim be answered, unread.
        store.writer_pragmas().await?;
        ticks.push(tick);
        clock.set(clock.now() + Duration::from_secs(120));
    }
    // The precondition: the second and third claims did run, and took nothing (M2T04R-553).
    for tick in &mut ticks[1..] {
        assert!(matches!(
            tick.as_mut().poll(&mut Context::from_waker(Waker::noop())),
            std::task::Poll::Ready(Ok(ref t)) if *t == hoh_core::delivery::Tick::default()
        ));
    }
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'k'"
        )?,
        [[json!("DELIVERING"), json!(1)]]
    );
    drop(ticks);
    dispatcher.tick().await?;
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'k'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    assert_eq!(transport.replies_of("k").len(), 1);
    Ok(())
}

#[tokio::test]
async fn a_claim_held_when_the_writer_panics_cannot_start() -> R {
    // M2T04R-402: the writer ends by a panic (a test fault in a commit); a guard alive then hands nothing over.
    #[derive(Debug)]
    struct Panicking;
    impl hoh_core::persistence::FaultHooks for Panicking {
        fn before_commit(&self) -> Result<(), PersistError> {
            std::panic::resume_unwind(Box::new("writer panic"));
        }
    }
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "p", &[to_qa("p", 3, 2, &sha(2), None)?]).await?;
    drop(store);
    let (store, _) = open_with(
        &dir,
        Options {
            faults: std::sync::Arc::new(Panicking),
            ..Options::default()
        },
    )?;
    let claim = store
        .claim_outbox(FakeClock::new().now(), vec!["qa".to_owned()], 3)
        .await?
        .ok_or("nothing due")?;
    let ended = store.writer_ended();
    let _ = store
        .commit(Commit {
            events: vec![NewEvent {
                project_id: "p1".to_owned(),
                event_id: "boom".to_owned(),
                run_id: Some("R".to_owned()),
                kind: "test.boom".to_owned(),
                payload: json!({}),
            }],
            ..Commit::default()
        })
        .await;
    assert!(!ended.await, "the writer did not panic");
    assert_eq!(claim.start(), None);
    Ok(())
}

#[tokio::test]
async fn a_claim_still_held_when_the_store_closes_is_undone_and_cannot_start() -> R {
    // M2T04R-301: the store closes while a guard is alive; the writer undoes the claim before it lets the database go,
    // and the guard can no longer hand the row to a transport.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(&store, "s", &[to_qa("s", 3, 2, &sha(2), None)?]).await?;
    let claim = store
        .claim_outbox(FakeClock::new().now(), vec!["qa".to_owned()], 3)
        .await?
        .ok_or("nothing due")?;
    let ended = store.writer_ended();
    drop(store);
    assert!(ended.await, "the writer did not finish");
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 's'"
        )?,
        [[json!("PENDING"), json!(0)]]
    );
    assert_eq!(claim.start(), None);
    Ok(())
}

#[tokio::test]
async fn a_returned_claim_is_uncounted_even_when_an_earlier_delivery_was_acked_first() -> R {
    // M2T04R-500: generation 1 is delivered but its answer is late; past its backoff generation 2 is claimed. For C it
    // is returned unstarted and then generation 1's answer makes the row ACKED before the next claim undoes it; for N
    // the answer comes while its guard is still held, and the close undoes it (M2T04R-651). Either way generation 2 is
    // taken back: one delivery, one count.
    let dir = tempfile::tempdir()?;
    let (store, db) = open(&dir)?;
    seed(&db, WORLD)?;
    enqueue(
        &store,
        "acked",
        &[
            to_qa("N", 3, 2, &sha(2), None)?,
            to_qa("C", 3, 2, &sha(2), None)?,
        ],
    )
    .await?;
    let clock = FakeClock::new();
    let qa = vec!["qa".to_owned()];
    let local = LocalTransport::new(&store, &clock).with("qa", Noop);
    let mut first = Vec::new();
    for _ in 0..2 {
        let row = store
            .claim_outbox(clock.now(), qa.clone(), 3)
            .await?
            .and_then(hoh_core::persistence::Claimed::start)
            .ok_or("nothing due")?;
        local.deliver(&row).await.map_err(|e| format!("{e:?}"))?;
        first.push(row);
    }
    drop(local);
    clock.set(clock.now() + Duration::from_secs(60));
    let mut held = Vec::new();
    for _ in 0..2 {
        held.push(
            store
                .claim_outbox(clock.now(), qa.clone(), 3)
                .await?
                .ok_or("not due again")?,
        );
    }
    // Taken in message order: C, then N. C's claim is returned before the answers, N's is held through them.
    let close = held.pop().ok_or("no claim")?;
    let next = held.pop().ok_or("no claim")?;
    assert_eq!(
        (
            next.row().map(|r| r.message_id.as_str()),
            close.row().map(|r| r.message_id.as_str())
        ),
        (Some("C"), Some("N"))
    );
    drop(next);
    for row in &first {
        store
            .settle_outbox(
                &row.message_id,
                row.attempt_count,
                hoh_core::persistence::Settlement::Delivered,
                3,
            )
            .await?;
    }
    // The precondition: both rows ACKED while generation 2 is still counted (C's return not yet undone).
    assert_eq!(
        rows(
            &db,
            "SELECT message_id, state, attempt_count FROM outbox WHERE message_id IN ('N', 'C') ORDER BY message_id"
        )?,
        [
            [json!("C"), json!("ACKED"), json!(2)],
            [json!("N"), json!("ACKED"), json!(2)]
        ]
    );
    assert!(store.claim_outbox(clock.now(), qa, 3).await?.is_none());
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'C'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    let ended = store.writer_ended();
    drop(store);
    assert!(ended.await, "the writer did not finish");
    assert_eq!(close.start(), None);
    assert_eq!(
        rows(
            &db,
            "SELECT state, attempt_count FROM outbox WHERE message_id = 'N'"
        )?,
        [[json!("ACKED"), json!(1)]]
    );
    assert_eq!(
        one(
            &db,
            "SELECT count(*) FROM receipts WHERE message_id IN ('N', 'C')"
        )?,
        json!(2)
    );
    Ok(())
}
