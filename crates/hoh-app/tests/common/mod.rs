//! What several of this crate's integration tests share, whatever their world: a run command made at a revision the
//! test read before it, made again while the host says that revision is stale. A test binary using it declares
//! `mod common;`.

#![allow(dead_code)] // each test binary uses a part of it

use serde_json::Value;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// How many times a command is made, at most.
pub const FRESH_TRIES: usize = 20;

/// A run command made at `revision` (read from the database before it), and made again at the revision the host's
/// 409 STALE_REVISION names while it answers so — a host with a runner moves the run on between the test's read and
/// its command (a round's dispatch, a Commander's decision, the pace waking it) — as the client does
/// (`client::fresh`), at most [`FRESH_TRIES`] times. `submit` makes the command at a revision; the last answer, its
/// status and body.
pub async fn fresh<F, Fut>(revision: Value, mut submit: F) -> R<(u16, Value)>
where
    F: FnMut(Value) -> Fut,
    Fut: std::future::Future<Output = R<(u16, Value)>>,
{
    let mut revision = revision;
    for _ in 1..FRESH_TRIES {
        let (status, body) = submit(revision.clone()).await?;
        match stale(status, &body) {
            Some(current) => revision = current,
            None => return Ok((status, body)),
        }
    }
    submit(revision).await
}

/// [`fresh`] for a `submit` that blocks.
pub fn fresh_blocking(
    revision: Value,
    mut submit: impl FnMut(Value) -> R<(u16, Value)>,
) -> R<(u16, Value)> {
    let mut revision = revision;
    for _ in 1..FRESH_TRIES {
        let (status, body) = submit(revision.clone())?;
        match stale(status, &body) {
            Some(current) => revision = current,
            None => return Ok((status, body)),
        }
    }
    submit(revision)
}

/// The revision a 409 STALE_REVISION names, if `status` and `body` are one.
fn stale(status: u16, body: &Value) -> Option<Value> {
    (status == 409 && body["code"] == "STALE_REVISION").then(|| body["current_revision"].clone())
}

mod output;
pub mod spec;
#[allow(unused_imports)] // each test binary uses a part of it
pub use output::output_within;

pub mod quiescent;

/// Ends, however the test leaves, every process the records at `.0` name by its pid and start time: `proc <pid>
/// <start>` (written by a fixture's process at its fork, or for a member as soon as it started), `established <pid>
/// <sid> <ms> <handled|ignored> <start>` (control.rs's writer) and `w <pid> <sid> <start>` (its W). Each gets SIGKILL
/// through identity::signal_if_same, so a number the kernel handed out again gets nothing (ADR-022 decision 10;
/// CIR-800, -801, -850). None is the test's child: once ended, the reaper of orphans reaps it.
pub struct Recorded(pub std::path::PathBuf);

impl Drop for Recorded {
    fn drop(&mut self) {
        end_recorded(&self.0);
    }
}

/// The processes the records at `mark` name ([`Recorded`]).
pub fn recorded_processes(mark: &std::path::Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(mark)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split(' ').collect();
            let (pid, start) = match words.as_slice() {
                ["proc", pid, start] | ["w", pid, _, start] => (pid, start),
                ["established", pid, _, _, _, start] => (pid, start),
                _ => return None,
            };
            Some((pid.parse().ok()?, (*start).to_owned()))
        })
        .collect()
}

/// SIGKILL to each process the records at `mark` name, only while it is still that process ([`Recorded`]); then until
/// each has ended, and the records again, as long as a reading names a process not yet ended (at most 10 s in all). A
/// fixture's process registers each child it forks before the child goes on, and a child never registered ends by
/// itself (CIRF-02): with the creators outside the records ended first (the test's guards for them are dropped
/// before this one), a recorded creator that registered a child just before its end is the only source of a new name.
pub fn end_recorded(mark: &std::path::Path) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut done = std::collections::BTreeSet::new();
    loop {
        let fresh: Vec<(u32, String)> = recorded_processes(mark)
            .into_iter()
            .filter(|p| !done.contains(p))
            .collect();
        if fresh.is_empty() {
            return;
        }
        for (pid, start) in &fresh {
            let _ =
                hoh_agents::identity::signal_if_same(*pid, start, rustix::process::Signal::KILL);
        }
        until_ended_by(&fresh, end);
        done.extend(fresh);
        if std::time::Instant::now() > end {
            return;
        }
    }
}

/// Wait until each process (pid and start time) has ended, by identity::ended, for at most 10 s (cleanup: a look that
/// fails counts as ended).
pub fn until_ended(processes: &[(u32, String)]) {
    until_ended_by(
        processes,
        std::time::Instant::now() + std::time::Duration::from_secs(10),
    );
}

fn until_ended_by(processes: &[(u32, String)], end: std::time::Instant) {
    for (pid, start) in processes {
        while !hoh_agents::identity::ended(*pid, Some(start)).unwrap_or(true)
            && std::time::Instant::now() < end
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// SIGKILL to the group led by process `pid` started at `start`, only while that leader is still that process:
/// identity::find holds it by a pidfd and checks its start time, Found::kill signals the group only right after a look
/// that shows it held. A leader reaped meanwhile, whose number the kernel may have handed out again, gets nothing
/// (ADR-022 decision 10); its members are ended by their own records ([`Recorded`]).
pub fn end_group_of(pid: u32, start: &str) {
    let Ok(boot) = hoh_agents::identity::boot_id() else {
        return;
    };
    if let Ok(Ok(found)) =
        hoh_agents::identity::find(Some(i64::from(pid)), Some(&boot), Some(start))
    {
        found.kill();
    }
}

/// [`end_group_of`] however the test leaves.
pub struct GroupOf(pub u32, pub String);

impl Drop for GroupOf {
    fn drop(&mut self) {
        end_group_of(self.0, &self.1);
    }
}

/// `pid`'s start time now, for a process the test knows to be running (its own unreaped child, or one that runs until
/// it is stopped): an error when it cannot be read.
pub fn start_of(pid: u32) -> R<String> {
    Ok(hoh_agents::identity::start_id(pid)?
        .ok_or_else(|| format!("process {pid} is not running"))?)
}

/// Set in a test process started inside a delegated scope ([`in_delegated_scope`]).
pub const IN_SCOPE: &str = "HOH_TEST_IN_DELEGATED_SCOPE";

/// Run this binary's ignored test `name` inside a delegated scope of the user's systemd manager
/// (`systemd-run --user --scope -p Delegate=yes`), where an in-process host tracks its attempts by cgroups of their own
/// (docs/adr/ADR-021.md decision 8d v5, CR-011). It must have run and passed.
pub fn in_delegated_scope(name: &str) -> R {
    let exe = std::env::current_exe()?;
    let out = std::process::Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "-p",
            "Delegate=yes",
            "--quiet",
            "--collect",
            "--expand-environment=no",
            "--",
        ])
        .arg(&exe)
        .args([
            name,
            "--exact",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(IN_SCOPE, "1")
        .output()?;
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    if !out.status.success() || !stdout.contains("1 passed") {
        return Err(format!(
            "{name} in a delegated scope: {}\n{stdout}\n{stderr}",
            out.status
        )
        .into());
    }
    Ok(())
}
