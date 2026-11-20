//! A client's one stderr writer (docs/adr/ADR-031.md, 设计修正 2, C): made when the client's signal state is
//! registered, before any helper, and from then on the only thread that writes fd 2. Everything said on stderr is
//! queued to it: `say!`, the signal state's messages and the follow's lines and native stderr ([`crate::forward`]).
//! It writes in units of at most [`UNIT`] (`PIPE_BUF`), each finished at its offset after a short write before the next,
//! and waits for room with poll(2) on EAGAIN, never changing fd 2's `O_NONBLOCK`; so no unit is ever split by another
//! write.
//!
//! Two kinds of content: the client's own words (diagnostics, conclusions, rule 2's sentence, the exit's sentences) are
//! never dropped and have no byte budget; the follow's per-event progress and the native stderr share [`BUDGET`]: a
//! progress line that does not fit is dropped and counted as it is queued, native stderr waits for room on the
//! forwarder's own thread. At the exit ([`Writer::exit`]) progress and native stderr are no longer taken and what of
//! them is queued is dropped; rule 2's sentence goes first, then the client's own words in the order said, then what
//! the exit adds (what was not shown, and the stop's unfinished items); they are written within [`EXIT_WAIT`] of the
//! exit, and the process ends either way.
//!
//! The two written exceptions to "only the writer writes fd 2": a writer that ended (a panic, or fd 2 that can never
//! be written: EBADF, EPIPE, EIO) has the exit write the client's own words from a short-lived thread of its own,
//! within the same bound (nothing when no thread can be made); and the panic hook. Before the signal state is
//! registered (a usage error before any wait) there is no writer and [`say`] writes directly.

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::os::fd::AsFd as _;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The largest unit written at once (`PIPE_BUF`).
pub const UNIT: usize = 4096;
/// The budget the follow's progress lines and the native stderr share.
pub const BUDGET: usize = 1 << 20;
/// How long the exit writes the unit being written and the client's own words, from its start.
pub const EXIT_WAIT: Duration = Duration::from_millis(500);

/// What is queued.
#[derive(Debug)]
enum Entry {
    /// The client's own words: never dropped.
    Own(Vec<u8>),
    /// Rule 2's sentence: first among the client's own words at the exit.
    Front(Vec<u8>),
    /// A progress line of the follow: within the budget, dropped when it does not fit.
    Progress(Vec<u8>),
    /// A unit of attempt `attempt`'s native stderr: within the budget, the forwarder waits for room.
    Native { attempt: String, bytes: Vec<u8> },
}

impl Entry {
    fn bytes(&self) -> &[u8] {
        match self {
            Entry::Own(bytes)
            | Entry::Front(bytes)
            | Entry::Progress(bytes)
            | Entry::Native { bytes, .. } => bytes,
        }
    }

    fn budgeted(&self) -> usize {
        match self {
            Entry::Progress(bytes) | Entry::Native { bytes, .. } => bytes.len(),
            Entry::Own(_) | Entry::Front(_) => 0,
        }
    }
}

/// The writer's state, under its lock (nothing blocking is done under it).
#[derive(Debug, Default)]
struct Queue {
    entries: VecDeque<Entry>,
    /// Bytes of progress and native stderr queued.
    budgeted: usize,
    /// Progress lines dropped (at the queue, or at the exit).
    progress_dropped: u64,
    /// The exit began: progress and native stderr are no longer taken.
    closed: bool,
    /// The writer holds an entry outside the lock, writing it.
    writing: bool,
    /// What it holds is progress or native stderr.
    writing_budgeted: bool,
    /// The writer's thread ended (a panic, or fd 2 that cannot be written).
    ended: bool,
    /// Each forwarded attempt's directory and the bytes of its `*stderr.log` files the forwarder did not read.
    unshown: BTreeMap<String, (PathBuf, u64)>,
}

/// Where the writer writes: made once by the writer, and again by the exit when the writer ended.
pub type MakeSink = Arc<dyn Fn() -> Box<dyn Write + Send> + Send + Sync>;

/// A thread running `work`, or why it could not be made.
pub type Spawn<'a> = &'a dyn Fn(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>;

/// A client's one stderr writer.
pub struct Writer {
    queue: Mutex<Queue>,
    /// Woken on every change: the writer waits for entries, the forwarder for room, the exit for the end.
    changed: Condvar,
    make: MakeSink,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer").finish_non_exhaustive()
    }
}

/// The process's stderr (fd 2), written straight to its descriptor, never through std's lock: EINTR retried, EAGAIN
/// waited out with poll(2) (fd 2's `O_NONBLOCK` is left as it is).
struct Fd2;

impl Write for Fd2 {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let stderr = std::io::stderr();
        let fd = stderr.as_fd();
        loop {
            match rustix::io::write(fd, bytes) {
                Ok(n) => return Ok(n),
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::AGAIN) => {
                    let mut fds = [rustix::event::PollFd::new(
                        &fd,
                        rustix::event::PollFlags::OUT,
                    )];
                    let _ = rustix::event::poll(&mut fds, None);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The sink of the process's stderr.
pub fn fd2() -> MakeSink {
    Arc::new(|| Box::new(Fd2))
}

/// [`Spawn`] by `std::thread::Builder`, the thread named `name`.
pub fn thread(
    name: &'static str,
) -> impl Fn(Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
    move |work| {
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(work)
            .map(drop)
    }
}

/// `text` as a line on stderr: to the client's writer once its signal state is registered, else (before it, or in a
/// process that has none) straight to fd 2.
pub fn say(text: &str) {
    match crate::signals::current() {
        Some(state) => state.writer().own(text),
        None => {
            let _ = Fd2.write_all(line(text).as_slice());
        }
    }
}

/// The panic hook's words (the second written exception: it writes fd 2 itself, the writer may be what panicked).
pub fn panicking(text: &str) {
    let _ = Fd2.write_all(line(text).as_slice());
}

/// Wait until the client's writer has written everything queued, `bound` at most (a debug seam that aborts right
/// after it says why; debug builds only).
#[cfg(debug_assertions)]
pub fn settle(bound: Duration) {
    if let Some(state) = crate::signals::current() {
        state.writer().drained(Instant::now() + bound);
    }
}

/// `text` and a newline.
fn line(text: &str) -> Vec<u8> {
    let mut line = Vec::with_capacity(text.len() + 1);
    line.extend_from_slice(text.as_bytes());
    line.push(b'\n');
    line
}

/// Marks the writer ended when its thread ends, however (a panic's unwinding included).
struct Ending(Arc<Writer>);

impl Drop for Ending {
    fn drop(&mut self) {
        let mut queue = self.0.locked();
        queue.ended = true;
        queue.writing = false;
        queue.writing_budgeted = false;
        drop(queue);
        self.0.changed.notify_all();
    }
}

impl Writer {
    /// The writer, its thread made by `spawn`, writing to what `make` makes; or why its thread could not be made.
    pub fn start(make: MakeSink, spawn: Spawn<'_>) -> std::io::Result<Arc<Writer>> {
        let writer = Arc::new(Writer {
            queue: Mutex::default(),
            changed: Condvar::new(),
            make,
        });
        let theirs = writer.clone();
        spawn(Box::new(move || {
            let ending = Ending(theirs);
            ending.0.write();
        }))?;
        Ok(writer)
    }

    fn locked(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The thread: each entry as it comes, in units of at most [`UNIT`]; it returns when stderr cannot be written.
    fn write(&self) {
        let mut sink = (self.make)();
        loop {
            {
                let mut queue = self.locked();
                while queue.entries.is_empty() {
                    queue.writing = false;
                    queue.writing_budgeted = false;
                    self.changed.notify_all();
                    queue = self
                        .changed
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            }
            // Where a test holds the writer with entries queued, none in hand (debug builds only).
            crate::debug_hold::at("stderr-writer");
            let entry = {
                let mut queue = self.locked();
                let Some(entry) = queue.entries.pop_front() else {
                    continue;
                };
                queue.budgeted -= entry.budgeted();
                queue.writing = true;
                queue.writing_budgeted = entry.budgeted() > 0;
                entry
            };
            // Room was made for the forwarder.
            self.changed.notify_all();
            for unit in entry.bytes().chunks(UNIT) {
                if sink.write_all(unit).is_err() {
                    return;
                }
            }
        }
    }

    fn push(&self, entry: Entry) {
        let mut queue = self.locked();
        queue.budgeted += entry.budgeted();
        queue.entries.push_back(entry);
        drop(queue);
        self.changed.notify_all();
    }

    /// `text` as a line of the client's own: never dropped.
    pub fn own(&self, text: &str) {
        self.push(Entry::Own(line(text)));
    }

    /// Rule 2's sentence: one of the client's own words, and the first of them at the exit.
    pub fn front(&self, text: &str) {
        self.push(Entry::Front(line(text)));
    }

    /// `text` as a progress line: dropped and counted when it does not fit the budget, or once the exit began.
    pub fn progress(&self, text: &str) {
        let line = line(text);
        let mut queue = self.locked();
        if queue.closed || queue.budgeted + line.len() > BUDGET {
            queue.progress_dropped += 1;
            return;
        }
        queue.budgeted += line.len();
        queue.entries.push_back(Entry::Progress(line));
        drop(queue);
        self.changed.notify_all();
    }

    /// Attempt `attempt`'s native stderr is forwarded from `dir`.
    pub fn forwarding(&self, attempt: &str, dir: PathBuf) {
        self.locked()
            .unshown
            .entry(attempt.to_owned())
            .or_insert((dir, 0));
    }

    /// `bytes` of attempt `attempt`'s native stderr, queued in units of at most [`UNIT`] as room is made, waiting
    /// for it until `until` at most; never once the exit began or the writer ended. How many bytes were queued.
    pub fn native(&self, attempt: &str, bytes: &[u8], until: Instant) -> usize {
        let mut queued = 0;
        for unit in bytes.chunks(UNIT) {
            let mut queue = self.locked();
            loop {
                if queue.closed || queue.ended {
                    return queued;
                }
                if queue.budgeted + unit.len() <= BUDGET {
                    break;
                }
                let left = until.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return queued;
                }
                queue = self
                    .changed
                    .wait_timeout(queue, left)
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
            queue.budgeted += unit.len();
            queue.entries.push_back(Entry::Native {
                attempt: attempt.to_owned(),
                bytes: unit.to_vec(),
            });
            drop(queue);
            self.changed.notify_all();
            queued += unit.len();
        }
        queued
    }

    /// The bytes of attempt `attempt`'s native stderr the forwarder did not read when it finished.
    pub fn unread(&self, attempt: &str, bytes: u64) {
        if let Some((_, unread)) = self.locked().unshown.get_mut(attempt) {
            *unread = bytes;
        }
    }

    /// Wait until nothing is queued or being written, until `until` at most: whether so (debug builds only).
    #[cfg(debug_assertions)]
    pub fn drained(&self, until: Instant) -> bool {
        let mut queue = self.locked();
        loop {
            if queue.entries.is_empty() && !queue.writing {
                return true;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() || queue.ended {
                return false;
            }
            queue = self
                .changed
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Wait until no progress or native stderr is queued or being written, until `until` at most: whether none is.
    pub fn flushed(&self, until: Instant) -> bool {
        let mut queue = self.locked();
        loop {
            let budgeted_left = queue.budgeted > 0 || queue.writing_budgeted;
            if !budgeted_left || queue.ended {
                return !budgeted_left;
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            queue = self
                .changed
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The exit (each one the same): progress and native stderr are no longer taken and what of them is queued is
    /// dropped; rule 2's sentence goes first, then the client's own words in the order said, then `tail`, then what
    /// was not shown (each forwarded attempt whose native stderr was not all shown, and the progress lines dropped).
    /// They are written within `bound` (from now); a writer that ended has them written from a thread of its own, within
    /// the same bound. Whatever is not written by then is not (the process ends; there is no other channel).
    pub fn exit(&self, tail: &[String], bound: Duration) {
        let end = Instant::now() + bound;
        // Where a test sees that the exit began (debug builds only).
        crate::debug_hold::mark("stderr-exit", "");
        let ended = {
            let mut queue = self.locked();
            queue.closed = true;
            let mut dropped: BTreeMap<String, u64> = BTreeMap::new();
            let mut fronts = VecDeque::new();
            let mut own = VecDeque::new();
            for entry in std::mem::take(&mut queue.entries) {
                match entry {
                    Entry::Progress(_) => queue.progress_dropped += 1,
                    Entry::Native { attempt, bytes } => {
                        *dropped.entry(attempt).or_default() +=
                            u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                    }
                    Entry::Front(_) => fronts.push_back(entry),
                    Entry::Own(_) => own.push_back(entry),
                }
            }
            queue.budgeted = 0;
            fronts.append(&mut own);
            for text in tail {
                fronts.push_back(Entry::Own(line(text)));
            }
            for (attempt, (dir, unread)) in std::mem::take(&mut queue.unshown) {
                let left = unread + dropped.get(&attempt).copied().unwrap_or_default();
                if left > 0 {
                    fronts.push_back(Entry::Own(line(&format!(
                        "hoh: 至少 {left} 字节原生 stderr 未显示，见 {}/",
                        dir.display()
                    ))));
                }
            }
            let lines = std::mem::take(&mut queue.progress_dropped);
            if lines > 0 {
                fronts.push_back(Entry::Own(line(&format!("hoh: {lines} 行进度未显示"))));
            }
            queue.entries = fronts;
            queue.ended
        };
        self.changed.notify_all();
        if ended {
            self.direct(end);
            return;
        }
        let mut queue = self.locked();
        loop {
            if queue.entries.is_empty() && !queue.writing {
                return;
            }
            if queue.ended {
                drop(queue);
                self.direct(end);
                return;
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            queue = self
                .changed
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// The writer ended: what is queued, written from a short-lived thread of its own, waited for until `end` at most
    /// (nothing when no thread can be made).
    fn direct(&self, end: Instant) {
        let entries: Vec<Vec<u8>> = self
            .locked()
            .entries
            .drain(..)
            .map(|entry| entry.bytes().to_vec())
            .collect();
        if entries.is_empty() {
            return;
        }
        let make = self.make.clone();
        let (done, written) = std::sync::mpsc::channel();
        let made = std::thread::Builder::new()
            .name("hoh-stderr-exit".to_owned())
            .spawn(move || {
                let mut sink = make();
                for entry in entries {
                    for unit in entry.chunks(UNIT) {
                        if sink.write_all(unit).is_err() {
                            return;
                        }
                    }
                }
                let _ = done.send(());
            });
        if made.is_ok() {
            let _ = written.recv_timeout(end.saturating_duration_since(Instant::now()));
        }
    }
}

#[cfg(test)]
mod tests;
