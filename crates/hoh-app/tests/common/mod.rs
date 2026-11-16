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

/// `child`'s output once it has ended, waited for at most `limit` (its stdout and stderr read meanwhile, so a full pipe
/// cannot hold it up); past the limit the child, this test's own and not yet reaped, is killed and reaped, and that is
/// an error naming what it wrote to stderr so far. A helper's wait has a bound of its own, so a hung child fails its
/// test instead of holding it until the suite's bound (ADR-022 decision 10; CIR-502).
pub fn output_within(
    mut child: std::process::Child,
    limit: std::time::Duration,
) -> R<std::process::Output> {
    fn drain(
        pipe: Option<impl std::io::Read + Send + 'static>,
    ) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sent, got) = std::sync::mpsc::channel();
        match pipe {
            Some(mut pipe) => {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = std::io::Read::read_to_end(&mut pipe, &mut bytes);
                    let _ = sent.send(bytes);
                });
            }
            None => {
                let _ = sent.send(Vec::new());
            }
        }
        got
    }
    let (stdout, stderr) = (drain(child.stdout.take()), drain(child.stderr.take()));
    let end = std::time::Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            let said = stderr
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap_or_default();
            return Err(format!(
                "still running after {limit:?}, so killed; its stderr: {}",
                String::from_utf8_lossy(&said)
            )
            .into());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    // Its pipes close at its end, unless a process it left holds them: read for a bounded while more.
    let left = |pipe: &std::sync::mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "its output was still held open 5 s after it ended")
    };
    Ok(std::process::Output {
        status,
        stdout: left(&stdout)?,
        stderr: left(&stderr)?,
    })
}

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

/// SIGKILL to each process the records at `mark` name, only while it is still that process ([`Recorded`]).
pub fn end_recorded(mark: &std::path::Path) {
    for (pid, start) in recorded_processes(mark) {
        let _ = hoh_agents::identity::signal_if_same(pid, &start, rustix::process::Signal::KILL);
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
