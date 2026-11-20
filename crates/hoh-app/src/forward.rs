//! A client's stderr while it follows a run (docs/adr/ADR-031.md decision 11, as 设计修正 2, C has it): the thread that
//! reads events and writes stdout never waits for stderr. The client's own lines go to its one stderr writer
//! ([`crate::stderr`]), never dropped; the follow's per-event progress goes there within the budget it shares with the
//! native stderr, dropped and counted when it does not fit; and a forwarder thread of its own reads, every 100 ms, the
//! bytes each `*stderr.log` of the followed run's attempts gained (`<project>/.hoh/attempts/<component(attempt_id)>/`,
//! where the host keeps its agents' native stderr) and queues them as they are, waiting for room in the budget. The
//! host forwards nothing and emits no event for them: they never reach stdout.

use crate::stderr::Writer;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read as _, Seek as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// How much of a native stderr is read at once.
const CHUNK: usize = 64 * 1024;
/// How often the native stderr files are looked at.
const EVERY: Duration = Duration::from_millis(100);
/// How long the last look may take once the run's result is in.
const DRAIN: Duration = Duration::from_secs(1);
/// How long after it the finish waits for what was queued to be written.
const FLUSH: Duration = Duration::from_millis(500);

#[derive(Default)]
struct State {
    attempts: BTreeSet<String>,
    finish: Option<Instant>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// A follow's stderr.
pub struct Stderr {
    writer: Arc<Writer>,
    project: PathBuf,
    shared: Shared,
    done: std::sync::mpsc::Receiver<()>,
}

impl Stderr {
    /// Its forwarder, queueing to `writer` (the client's one stderr writer).
    pub fn start(project: &Path, writer: Arc<Writer>) -> Stderr {
        let shared: Shared = Arc::default();
        let (sent, done) = std::sync::mpsc::channel();
        let (theirs, dir, to) = (shared.clone(), project.to_path_buf(), writer.clone());
        // No thread for it: native stderr is not forwarded, and the exit says how much of it was not shown.
        let _ = std::thread::Builder::new()
            .name("hoh-forward".to_owned())
            .spawn(move || {
                forward_all(&theirs, &dir, &to);
                let _ = sent.send(());
            });
        Stderr {
            writer,
            project: project.to_path_buf(),
            shared,
            done,
        }
    }

    /// A line of the client's own: never dropped.
    pub fn say(&self, text: &str) {
        self.writer.own(text);
    }

    /// A progress line of the follow: dropped (and counted) when it does not fit the budget.
    pub fn progress(&self, text: &str) {
        self.writer.progress(text);
    }

    /// Attempt `attempt_id` belongs to the run followed: its native stderr is forwarded.
    pub fn attempt(&self, attempt_id: &str) {
        let (state, wake) = &*self.shared;
        let added = state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attempts
            .insert(attempt_id.to_owned());
        if added {
            self.writer.forwarding(
                attempt_id,
                hoh_app::layout::attempt_logs(&self.project, attempt_id),
            );
            wake.notify_one();
        }
    }

    /// The last look at every native stderr (at most [`DRAIN`]), then what was queued written (at most [`FLUSH`]
    /// more); waited for no longer than that, so a stderr nobody reads does not hold the client's exit. What was not
    /// read is told to the writer, whose exit says it.
    pub fn finish(self) {
        let started = Instant::now();
        {
            let (state, wake) = &*self.shared;
            state.lock().unwrap_or_else(PoisonError::into_inner).finish = Some(started + DRAIN);
            wake.notify_one();
        }
        let _ = self.done.recv_timeout(DRAIN + FLUSH);
        self.writer.flushed(started + DRAIN + FLUSH);
    }
}

/// The forwarder: the native files every [`EVERY`], until the finish's last look; then what each attempt's files hold
/// that was not read, told to the writer.
fn forward_all(shared: &Shared, project: &Path, writer: &Writer) {
    let (state, wake) = &**shared;
    let mut read: BTreeMap<PathBuf, u64> = BTreeMap::new();
    loop {
        let (attempts, finish) = {
            let s = state.lock().unwrap_or_else(PoisonError::into_inner);
            let s = if s.finish.is_none() {
                wake.wait_timeout(s, EVERY)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0
            } else {
                s
            };
            (s.attempts.clone(), s.finish)
        };
        let until = finish.unwrap_or_else(|| Instant::now() + EVERY);
        for attempt in &attempts {
            forward(project, attempt, &mut read, writer, until);
        }
        if finish.is_some() {
            for attempt in &attempts {
                writer.unread(attempt, unread(project, attempt, &read));
            }
            return;
        }
    }
}

/// Attempt `attempt`'s `*stderr.log` files, in order.
fn logs(project: &Path, attempt: &str) -> Vec<PathBuf> {
    let dir = hoh_app::layout::attempt_logs(project, attempt);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with("stderr.log"))
        })
        .collect();
    files.sort();
    files
}

/// The bytes of attempt `attempt`'s `*stderr.log` files past what `read` says was read.
fn unread(project: &Path, attempt: &str, read: &BTreeMap<PathBuf, u64>) -> u64 {
    logs(project, attempt)
        .iter()
        .map(|file| {
            let size = std::fs::metadata(file).map_or(0, |m| m.len());
            size.saturating_sub(read.get(file).copied().unwrap_or_default())
        })
        .sum()
}

/// What attempt `attempt`'s `*stderr.log` files gained since `read` says they were read, queued to `writer` as room is
/// made; until `until` at most.
fn forward(
    project: &Path,
    attempt: &str,
    read: &mut BTreeMap<PathBuf, u64>,
    writer: &Writer,
    until: Instant,
) {
    for file in logs(project, attempt) {
        let offset = read.entry(file.clone()).or_insert(0);
        let Ok(mut opened) = std::fs::File::open(&file) else {
            continue;
        };
        let mut chunk = vec![0; CHUNK];
        loop {
            if Instant::now() >= until || opened.seek(std::io::SeekFrom::Start(*offset)).is_err() {
                return;
            }
            match opened.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let queued = writer.native(attempt, &chunk[..n], until);
                    *offset += u64::try_from(queued).unwrap_or(0);
                    if queued < n {
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
