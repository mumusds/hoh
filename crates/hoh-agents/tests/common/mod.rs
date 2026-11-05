//! Shared by the adapter test binaries: a store with a run and a task, opened with this binary as its WAL probe.

use hoh_agents::adapter::AttemptRef;
use hoh_core::persistence::selftest::{PROBE_DB_ENV, ProbeCommand, probe_main};
use hoh_core::persistence::{NewAttempt, Options, Store};
use std::ffi::OsString;
use std::time::Duration;

pub type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// The second process of the store's WAL self-test; does nothing unless started by it.
#[test]
#[ignore = "helper process of the WAL self-test"]
fn wal_probe_helper() {
    if std::env::var_os(PROBE_DB_ENV).is_some() {
        std::process::exit(probe_main());
    }
}

pub struct World {
    pub dir: tempfile::TempDir,
    pub store: Store,
}

impl World {
    /// A store with run R (RUNNING) and task T1 (LEASED), seeded with Python's sqlite3 (test code).
    pub fn new() -> R<World> {
        let dir = tempfile::tempdir()?;
        let db = dir.path().join(".hoh/state.db");
        let probe = ProbeCommand {
            program: std::env::current_exe()?,
            args: [
                "common::wal_probe_helper",
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
            &db,
            Options {
                wal_probe: Some(probe),
                ..Options::default()
            },
        )?;
        let seed = "INSERT INTO projects(project_id, repo_path, created_at) VALUES ('p1', '/repo', '2026-11-04T00:00:00Z');
             INSERT INTO specs(spec_id, project_id, sha256, revision) VALUES ('s1', 'p1', printf('%064d', 1), 1);
             INSERT INTO runs(run_id, project_id, spec_id, state, mode, created_at) VALUES ('R', 'p1', 's1', 'RUNNING', 'AUTO', '2026-11-04T00:00:00Z');
             INSERT INTO tasks(task_id, run_id, spec_id, state, role, created_seq) VALUES ('T1', 'R', 's1', 'LEASED', 'developer', 1);";
        let status = std::process::Command::new("python3")
            .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript(sys.argv[2]); c.commit()"])
            .arg(&db)
            .arg(seed)
            .status()?;
        if !status.success() {
            return Err("seeding the store failed".into());
        }
        Ok(World { dir, store })
    }

    /// Attempt `id` of task T1 by `agent` in session `session`, created PREPARED.
    pub async fn attempt_in(
        &self,
        agent: &str,
        id: &str,
        session: &str,
        version: &str,
    ) -> R<AttemptRef> {
        self.store
            .create_attempt(NewAttempt {
                attempt_id: id.to_owned(),
                task_id: "T1".to_owned(),
                session_id: None,
                role_epoch: 1,
                host_epoch: 1,
                config_revision: 0,
                agent: Some(agent.to_owned()),
            })
            .await?;
        Ok(AttemptRef {
            attempt_id: id.to_owned(),
            session_id: session.to_owned(),
            role: "developer".to_owned(),
            agent_version: version.to_owned(),
        })
    }
}

/// Probed capabilities as `probe()` reports them: every known capability unknown except `known`.
#[allow(dead_code)]
pub fn probed(known: &[(&str, bool)]) -> std::collections::BTreeMap<String, Option<bool>> {
    let mut caps: std::collections::BTreeMap<String, Option<bool>> =
        hoh_agents::probe::CAPABILITIES
            .iter()
            .map(|c| ((*c).to_owned(), None))
            .collect();
    for (name, value) in known {
        caps.insert((*name).to_owned(), Some(*value));
    }
    caps
}
