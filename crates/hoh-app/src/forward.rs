//! A client's stderr (docs/adr/ADR-031.md decision 11): one thread writes it, so the thread that reads events and
//! writes stdout never waits for it. The thread writes the client's own lines, queued within a bound (a line that
//! does not fit is dropped and counted, the count said last), and every 100 ms the bytes each `*stderr.log` of the
//! followed run's attempts gained (`.hoh/attempts/<attempt>/`, where the host keeps its agents' native stderr), as
//! they are, 64 KiB at a time. The host forwards nothing and emits no event for them: they never reach stdout.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{Read as _, Seek as _, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// How many bytes of the client's own lines may wait for the thread; past it a line is dropped and counted.
const QUEUE_BYTES: usize = 1 << 20;
/// How much of a native stderr is read and written at once.
const CHUNK: usize = 64 * 1024;
/// How often the native stderr files are looked at.
const EVERY: Duration = Duration::from_millis(100);
/// How long the last look may take once the run's result is in.
const DRAIN: Duration = Duration::from_secs(1);

#[derive(Default)]
struct State {
    lines: VecDeque<Vec<u8>>,
    bytes: usize,
    dropped: u64,
    attempts: BTreeSet<String>,
    finish: Option<Instant>,
}

type Shared = Arc<(Mutex<State>, Condvar)>;

/// The writer of a client's stderr.
pub struct Stderr {
    shared: Shared,
    done: std::sync::mpsc::Receiver<()>,
}

impl Stderr {
    /// Its thread, writing to the process's stderr.
    pub fn start(project: &Path) -> Stderr {
        Stderr::start_to(project, Box::new(std::io::stderr()))
    }

    /// Its thread, writing to `sink`.
    pub fn start_to(project: &Path, sink: Box<dyn Write + Send>) -> Stderr {
        let shared: Shared = Arc::default();
        let (sent, done) = std::sync::mpsc::channel();
        let (theirs, project) = (shared.clone(), project.to_path_buf());
        std::thread::spawn(move || {
            write(&theirs, &project, sink);
            let _ = sent.send(());
        });
        Stderr { shared, done }
    }

    fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> Option<T> {
        let (state, wake) = &*self.shared;
        let out = state.lock().ok().map(|mut s| f(&mut s));
        wake.notify_one();
        out
    }

    /// A line of the client's own; dropped (and counted) when the queue is full.
    pub fn say(&self, text: &str) {
        let mut line = text.as_bytes().to_vec();
        line.push(b'\n');
        self.with(|s| {
            if s.bytes + line.len() > QUEUE_BYTES {
                s.dropped += u64::try_from(line.len()).unwrap_or(u64::MAX);
            } else {
                s.bytes += line.len();
                s.lines.push_back(line);
            }
        });
    }

    /// Attempt `attempt_id` belongs to the run followed: its native stderr is forwarded.
    pub fn attempt(&self, attempt_id: &str) {
        self.with(|s| s.attempts.insert(attempt_id.to_owned()));
    }

    /// The last look at every native stderr (at most [`DRAIN`]) and the last lines, then the count of what was dropped;
    /// waited for no longer than that, so a stderr nobody reads does not hold the client's exit.
    pub fn finish(self) {
        self.with(|s| s.finish = Some(Instant::now() + DRAIN));
        let _ = self.done.recv_timeout(DRAIN + Duration::from_millis(500));
    }
}

/// The thread: lines as they come, the native files every [`EVERY`], until the finish.
fn write(shared: &Shared, project: &Path, mut sink: Box<dyn Write + Send>) {
    let (state, wake) = &**shared;
    let mut read: BTreeMap<PathBuf, u64> = BTreeMap::new();
    let mut looked = Instant::now()
        .checked_sub(EVERY)
        .unwrap_or_else(Instant::now);
    loop {
        let (lines, attempts, finish, dropped) = {
            let Ok(mut s) = state.lock() else { return };
            if s.lines.is_empty() && s.finish.is_none() {
                let wait = EVERY.saturating_sub(looked.elapsed());
                s = match wake.wait_timeout(s, wait) {
                    Ok((s, _)) => s,
                    Err(_) => return,
                };
            }
            let lines: Vec<Vec<u8>> = s.lines.drain(..).collect();
            s.bytes = 0;
            (lines, s.attempts.clone(), s.finish, s.dropped)
        };
        for line in lines {
            let _ = sink.write_all(&line);
        }
        if finish.is_some() || looked.elapsed() >= EVERY {
            looked = Instant::now();
            for attempt in &attempts {
                forward(project, attempt, &mut read, &mut *sink, finish);
            }
        }
        if finish.is_some() {
            // Lines said while the files were read, then the count.
            let rest: Vec<Vec<u8>> = match state.lock() {
                Ok(mut s) => s.lines.drain(..).collect(),
                Err(_) => Vec::new(),
            };
            for line in rest {
                let _ = sink.write_all(&line);
            }
            let dropped = state.lock().map_or(dropped, |s| s.dropped);
            if dropped > 0 {
                let _ = writeln!(sink, "hoh: stderr 写不过来，省略了 {dropped} 字节");
            }
            let _ = sink.flush();
            return;
        }
        let _ = sink.flush();
    }
}

/// What attempt `attempt`'s `*stderr.log` files gained since `read` says they were read, written to `sink`; by the
/// finish's deadline at the latest.
fn forward(
    project: &Path,
    attempt: &str,
    read: &mut BTreeMap<PathBuf, u64>,
    sink: &mut dyn Write,
    deadline: Option<Instant>,
) {
    let dir = hoh_app::host::product::attempt_logs(project, attempt);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
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
    for file in files {
        let offset = read.entry(file.clone()).or_insert(0);
        let Ok(mut opened) = std::fs::File::open(&file) else {
            continue;
        };
        if opened.seek(std::io::SeekFrom::Start(*offset)).is_err() {
            continue;
        }
        let mut chunk = vec![0; CHUNK];
        loop {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                return;
            }
            match opened.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let _ = sink.write_all(&chunk[..n]);
                    *offset += u64::try_from(n).unwrap_or(0);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
