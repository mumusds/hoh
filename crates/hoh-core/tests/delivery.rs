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
            ..Options::default()
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
}

impl<'a, T: Transport> FakeTransport<'a, T> {
    fn new(inner: &'a T, drop_first: Option<&'static str>, never: Option<&'static str>) -> Self {
        FakeTransport {
            inner,
            drop_first,
            never,
            dropped: Mutex::new(BTreeSet::new()),
            replies: Mutex::new(Vec::new()),
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
        if self.never == Some(row.recipient_role.as_str()) {
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
            return Err(DeliveryError::Lost("ack lost".to_owned()));
        }
        Ok(reply)
    }
}

/// Tick until nothing is due any more, moving the clock to the next retry each time.
async fn drain<C: Transport>(
    dispatcher: &Dispatcher<'_, FakeClock, C>,
    db: &Path,
    clock: &FakeClock,
) -> R<Vec<String>> {
    let mut retries = Vec::new();
    for _ in 0..20 {
        dispatcher.tick().await?;
        let next = rows(
            db,
            "SELECT min(next_retry_at) FROM outbox WHERE state IN ('PENDING', 'DELIVERING')",
        )?;
        let Some(Value::String(at)) = next.first().and_then(|r| r.first()) else {
            return Ok(retries);
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
                                 "receipts": receipts});
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
    let retries = drain(&dispatcher, &db, &clock).await?;
    let outbox = rows(
        &db,
        "SELECT message_id, state, attempt_count FROM outbox ORDER BY message_id",
    )?;
    keep(
        "test-022-03.json",
        &json!({"after_first_delivery": after_first, "retry_times": retries, "outbox": outbox,
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
    let mut candidates = Vec::new();
    for row in &messages {
        let raw: Value = serde_json::from_str(row[1].as_str().ok_or("no payload")?)?;
        let message: Message = protocol::decode(&raw)?;
        let package: ContextHandoff = protocol::decode(&Value::Object(message.payload.clone()))?;
        assert_eq!(message.kind, protocol::MessageKind::Handoff);
        assert_eq!(package.spec_sha256, sha(2));
        assert!(!package.acceptance_ids.is_empty());
        assert_eq!(package.role, message.recipient_role);
        candidates.push(serde_json::to_value(&package.candidate)?);
    }
    assert_eq!(candidates[0], candidates[1]);
    assert_eq!(candidates[0], c2);
    keep(
        "test-022-04.json",
        &json!({"handoffs": messages, "candidate_row": c2}),
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
                "task_revision_and_acceptance_results": [before, after]}),
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
                "bad_write": format!("{refused:?}")}),
    )?;
    Ok(())
}
