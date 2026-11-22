//! A test's bounded wait until a host has done what the test's last operation set going: [`await_quiescent`]. The
//! supervisor works after the operation that wakes it (docs/adr/ADR-025.md decision 2): it takes the run's events in
//! after its cursor, forms triggers, fires them once their debounce ends (or defers them: a run without a specification,
//! decision 4), gives an owed wake a Commander turn and disposes of the turn's output. A test that observes the
//! database, kills the host or makes a fault before that work is over sees it land afterwards: four tests did
//! (M3 flakes, FLK-01). This reads every condition in one read-only snapshot, so no condition is judged on a state
//! another condition did not see, and returns that snapshot; on its bound it names what was still going on.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The events the run's supervisor must have taken in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Watermark {
    /// Up to this event's seq: the operation just done (its last event).
    Seq(i64),
    /// Up to the last event committed when the snapshot is read: nothing committed is left to take in.
    Latest,
    /// The host has no supervisor (no runner: `HostConfig::runner` is None), so no cursor moves: nothing to take in.
    Unsupervised,
}

/// What a test waits for, explicitly.
#[derive(Debug, Clone, Copy)]
pub struct Quiet<'a> {
    /// The run whose supervisor is judged (cursor, triggers, wakes, outputs). Attempts are judged project-wide.
    pub run: &'a str,
    pub watermark: Watermark,
    /// Tasks whose attempts may be in flight (PREPARED, STARTING, RUNNING) at the snapshot: by task, since an
    /// attempt's id is made when it starts. Any other attempt in flight is not quiet.
    pub in_flight_tasks: &'a [&'a str],
    /// The reasons a PENDING trigger may wait for with its `supervisor.deferred` written (NO_SPEC: a run without a
    /// specification, decision 4). Any other PENDING trigger is owed a firing, due or once its debounce ends.
    pub deferrals: &'a [&'a str],
    /// The test's own reads in the same snapshot, by name: each query's first column of its first row, as text ("" for
    /// none or NULL), for `until`.
    pub reads: &'a [(&'a str, &'a str)],
    /// The database's logical dump (its user_version, then `iterdump`) in the same snapshot.
    pub dump: bool,
}

impl<'a> Quiet<'a> {
    /// Nothing allowed in flight, no deferral allowed, no reads, no dump.
    pub fn new(run: &'a str, watermark: Watermark) -> Quiet<'a> {
        Quiet {
            run,
            watermark,
            in_flight_tasks: &[],
            deferrals: &[],
            reads: &[],
            dump: false,
        }
    }
}

/// A PENDING trigger of the run.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Trigger {
    pub trigger_id: String,
    pub fingerprint: String,
    pub due_at: String,
    /// The reasons of its `supervisor.deferred` events.
    pub deferred: Vec<String>,
}

/// An attempt in flight (project-wide).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Attempt {
    pub attempt_id: String,
    pub task_id: String,
    pub state: String,
}

/// A wake of the run neither answered (its last turn SETTLED) nor superseded.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Wake {
    pub wake: String,
    /// Its last turn's attempt, its task and state ("" before its first turn or for a missing attempt).
    pub attempt_id: String,
    pub task_id: String,
    pub state: String,
}

/// What one read-only transaction saw.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct Snapshot {
    /// The last event's seq (0: none).
    pub max_seq: i64,
    /// The run's supervisor cursor (None: no row yet).
    pub cursor: Option<i64>,
    /// The run's state ("" for no such run).
    pub run_state: String,
    pub pending_triggers: Vec<Trigger>,
    pub in_flight: Vec<Attempt>,
    pub unanswered_wakes: Vec<Wake>,
    /// The run's turn outputs without an end (disposed of, or a decision applied or rejected).
    pub pending_outputs: Vec<String>,
    pub reads: BTreeMap<String, String>,
    pub dump: Option<String>,
}

impl Snapshot {
    /// Read `name` of [`Quiet::reads`] ("" when not asked for).
    pub fn read(&self, name: &str) -> &str {
        self.reads.get(name).map_or("", String::as_str)
    }

    /// What keeps this snapshot from being quiet under `quiet`, one line each; empty when it is.
    pub fn differences(&self, quiet: &Quiet<'_>) -> Vec<String> {
        let mut out = Vec::new();
        let wanted = match quiet.watermark {
            Watermark::Seq(seq) => Some(seq),
            Watermark::Latest => Some(self.max_seq),
            Watermark::Unsupervised => None,
        };
        if let Some(wanted) = wanted
            && self.cursor.is_none_or(|c| c < wanted)
        {
            out.push(format!(
                "the supervisor cursor of {} is {:?}, short of {wanted} (watermark {:?}, last event {})",
                quiet.run, self.cursor, quiet.watermark, self.max_seq
            ));
        }
        for t in &self.pending_triggers {
            if !t
                .deferred
                .iter()
                .any(|r| quiet.deferrals.contains(&r.as_str()))
            {
                out.push(format!(
                    "trigger {} ({}) PENDING, due at {}, deferred for {:?} (allowed: {:?})",
                    t.trigger_id, t.fingerprint, t.due_at, t.deferred, quiet.deferrals
                ));
            }
        }
        for a in &self.in_flight {
            if !quiet.in_flight_tasks.contains(&a.task_id.as_str()) {
                out.push(format!(
                    "attempt {} of task {} {} (allowed in flight: tasks {:?})",
                    a.attempt_id, a.task_id, a.state, quiet.in_flight_tasks
                ));
            }
        }
        for w in &self.unanswered_wakes {
            if !quiet.in_flight_tasks.contains(&w.task_id.as_str()) {
                out.push(format!(
                    "wake {} unanswered: its last turn {:?} of task {:?} {:?}",
                    w.wake, w.attempt_id, w.task_id, w.state
                ));
            }
        }
        for o in &self.pending_outputs {
            out.push(format!("output {o} not disposed of"));
        }
        out
    }
}

/// The snapshot: one read-only transaction (WAL: every read sees the same commit).
const SNAPSHOT: &str = r#"
import json, sqlite3, sys
db, run, want_dump = sys.argv[1], sys.argv[2], sys.argv[3] == "1"
reads = json.loads(sys.stdin.read())
c = sqlite3.connect("file:" + db + "?mode=ro", uri=True, timeout=10, isolation_level=None)
c.execute("BEGIN")
one = lambda sql, *a: c.execute(sql, a).fetchone()
rows = lambda sql, *a: c.execute(sql, a).fetchall()
out = {"max_seq": one("SELECT coalesce(max(seq), 0) FROM events")[0]}
cursor = one("SELECT last_handled_seq FROM supervisor_cursor WHERE run_id = ?", run)
out["cursor"] = cursor[0] if cursor else None
state = one("SELECT state FROM runs WHERE run_id = ?", run)
out["run_state"] = state[0] if state else ""
deferred = {}
for p, in rows("SELECT payload_json FROM events WHERE run_id = ? AND kind = 'supervisor.deferred'", run):
    p = json.loads(p)
    key = p.get("trigger_id") or p.get("wake")
    deferred.setdefault(key, []).append(p.get("reason") or "")
out["pending_triggers"] = [
    {"trigger_id": t, "fingerprint": f, "due_at": d, "deferred": deferred.get(t, [])}
    for t, f, d in rows("SELECT trigger_id, fingerprint, due_at FROM supervisor_triggers "
                        "WHERE run_id = ? AND state = 'PENDING' ORDER BY due_at, first_seq", run)
]
attempt = {a: (t, s) for a, t, s in rows("SELECT attempt_id, task_id, state FROM attempts")}
out["in_flight"] = [
    {"attempt_id": a, "task_id": t, "state": s}
    for a, (t, s) in sorted(attempt.items()) if s in ("PREPARED", "STARTING", "RUNNING")
]
# The run's wakes after its horizon (closed for good before it), as the product reads them (persistence/wakes.rs).
horizon = one("SELECT coalesce((SELECT json_extract(payload_json, '$.seq') FROM events WHERE run_id = ? "
              "AND kind = 'supervisor.horizon' ORDER BY seq DESC LIMIT 1), 0)", run)[0]
woken, superseded, last = [], set(), {}
for kind, p in rows("SELECT kind, payload_json FROM events WHERE run_id = ? AND seq >= ? AND kind IN "
                    "('supervisor.woken', 'supervisor.allocated', 'supervisor.superseded') ORDER BY seq", run, horizon):
    p = json.loads(p)
    wake = p.get("wake")
    if kind == "supervisor.woken":
        wake = wake or p.get("trigger_id")
        if "merged_into" not in p and wake not in woken:
            woken.append(wake)
    elif kind == "supervisor.allocated":
        if wake and p.get("attempt_id"):
            last[wake] = p["attempt_id"]
    elif wake:
        superseded.add(wake)
out["unanswered_wakes"] = []
for wake in woken:
    a = last.get(wake, "")
    t, s = attempt.get(a, ("", ""))
    if wake not in superseded and s != "SETTLED":
        out["unanswered_wakes"].append({"wake": wake, "attempt_id": a, "task_id": t, "state": s})
ended = {o for o, in rows("SELECT json_extract(payload_json, '$.output_id') FROM events WHERE run_id = ? AND seq >= ? "
                          "AND kind IN ('supervisor.output_disposed', 'decision.applied', 'decision.rejected')",
                          run, horizon)}
out["pending_outputs"] = [
    o for o, in rows("SELECT json_extract(payload_json, '$.output_id') FROM events WHERE run_id = ? AND seq >= ? "
                     "AND kind = 'supervisor.output' ORDER BY seq", run, horizon)
    if o is not None and o not in ended
]
out["reads"] = {}
for name, sql in reads:
    r = c.execute(sql).fetchone()
    out["reads"][name] = "" if r is None or r[0] is None else str(r[0])
out["dump"] = None
if want_dump:
    out["dump"] = (str(c.execute("PRAGMA user_version").fetchone()[0]) + "\n" + "\n".join(c.iterdump())).strip()
c.execute("COMMIT")
print(json.dumps(out))
"#;

/// One snapshot of `db` under `quiet`.
pub fn snapshot(db: &Path, quiet: &Quiet<'_>) -> R<Snapshot> {
    use std::io::Write as _;
    let mut child = std::process::Command::new("python3")
        .args(["-c", SNAPSHOT])
        .arg(db)
        .arg(quiet.run)
        .arg(if quiet.dump { "1" } else { "0" })
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let reads: Vec<[&str; 2]> = quiet.reads.iter().map(|&(n, s)| [n, s]).collect();
    child
        .stdin
        .take()
        .ok_or("no stdin")?
        .write_all(serde_json::to_string(&reads)?.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(format!(
            "the snapshot failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The first snapshot of `db`, within `limit`, that is quiet under `quiet` and for which `until` holds (the test's
/// own condition on the same snapshot: its reads, the run's state). On the bound: an error naming what the last
/// snapshot still had going on and its reads. Polled, not slept on: it returns as soon as a snapshot is quiet.
pub fn await_quiescent(
    db: &Path,
    quiet: &Quiet<'_>,
    limit: Duration,
    until: impl Fn(&Snapshot) -> bool,
) -> R<Snapshot> {
    const POLL: Duration = Duration::from_millis(50);
    let end = Instant::now() + limit;
    loop {
        let seen = snapshot(db, quiet)?;
        let differences = seen.differences(quiet);
        let held = until(&seen);
        if differences.is_empty() && held {
            return Ok(seen);
        }
        if Instant::now() > end {
            let message = format!(
                "not quiescent within {limit:?}: {}; the test's condition {}; run {} {}, reads {:?}",
                if differences.is_empty() {
                    "the supervisor and attempts quiet".to_owned()
                } else {
                    differences.join("; ")
                },
                if held { "held" } else { "did not hold" },
                quiet.run,
                seen.run_state,
                seen.reads
            );
            eprintln!("await_quiescent: {message}");
            return Err(message.into());
        }
        std::thread::sleep(POLL);
    }
}
