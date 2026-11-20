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
//! forwarder's own thread. At the exit ([`Writer::close`], [`Writer::wait`]) progress and native stderr are no longer
//! taken and what of them is queued is dropped; the unit being written is finished, then rule 2's sentence (also one
//! for a signal that comes while the exit writes), then the client's own words in the order said, then what the exit
//! adds (what was not shown, and the stop's unfinished items); they are written within [`EXIT_WAIT`] of the exit, and
//! the process ends either way. The entry being written stays queued with how much of it was written, so a writer that
//! ends leaves the rest to the exit.
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
/// How often the exit's wait looks for a signal that came meanwhile.
const LOOK: Duration = Duration::from_millis(10);

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

    /// The same kind of entry holding `bytes`.
    fn with(&self, bytes: Vec<u8>) -> Entry {
        match self {
            Entry::Own(_) => Entry::Own(bytes),
            Entry::Front(_) => Entry::Front(bytes),
            Entry::Progress(_) => Entry::Progress(bytes),
            Entry::Native { attempt, .. } => Entry::Native {
                attempt: attempt.clone(),
                bytes,
            },
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
    /// How many bytes of the first entry are written: the entry being written stays first in the queue until it is
    /// all written (CLT-1203), so a writer that ended leaves what it did not write to the exit.
    offset: usize,
    /// Where the unit the writer is writing ends in the first entry, while it writes one.
    unit_end: Option<usize>,
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
        queue.unit_end = None;
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

    /// The thread: the first entry, one unit of at most [`UNIT`] at a time from where it was written to, each unit
    /// finished before the order is looked at again (the exit may have put rule 2's sentence next, CLT-1204); it
    /// returns when stderr cannot be written, leaving what it did not write queued.
    fn write(&self) {
        let mut sink = (self.make)();
        loop {
            let fresh = {
                let mut queue = self.locked();
                while queue.entries.is_empty() {
                    queue.unit_end = None;
                    self.changed.notify_all();
                    queue = self
                        .changed
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                queue.offset == 0
            };
            if fresh {
                // Where a test holds the writer with entries queued, none begun (debug builds only).
                crate::debug_hold::at("stderr-writer");
            }
            let unit = {
                let mut queue = self.locked();
                let start = queue.offset;
                let Some(first) = queue.entries.front() else {
                    continue;
                };
                let end = first.bytes().len().min(start + UNIT);
                let unit = first.bytes()[start..end].to_vec();
                queue.unit_end = Some(end);
                unit
            };
            let mut done = 0;
            while done < unit.len() {
                match sink.write(&unit[done..]) {
                    Ok(0) => return,
                    Ok(n) => {
                        done += n;
                        self.locked().offset += n;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => return,
                }
            }
            let mut queue = self.locked();
            queue.unit_end = None;
            let all = queue
                .entries
                .front()
                .is_some_and(|first| queue.offset >= first.bytes().len());
            if all && let Some(first) = queue.entries.pop_front() {
                queue.budgeted -= first.budgeted();
                queue.offset = 0;
            }
            drop(queue);
            // Room was made for the forwarder; the exit may be waiting for the end.
            self.changed.notify_all();
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
            if queue.entries.is_empty() {
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
            let budgeted_left = queue.budgeted > 0;
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

    /// The first entry split where the writer stands: what it has begun and not finished (the unit it writes, or
    /// nothing between units), which stays first; and the rest of that entry, not begun. The queue keeps neither.
    fn split_first(queue: &mut Queue) -> (Option<Entry>, Option<Entry>) {
        if queue.offset == 0 && queue.unit_end.is_none() {
            return (None, None);
        }
        let Some(first) = queue.entries.pop_front() else {
            return (None, None);
        };
        queue.budgeted -= first.budgeted();
        let keep = queue.unit_end.unwrap_or(queue.offset);
        let bytes = first.bytes();
        let head = (keep > queue.offset).then(|| first.with(bytes[..keep].to_vec()));
        let rest = (keep < bytes.len()).then(|| first.with(bytes[keep..].to_vec()));
        if head.is_none() {
            queue.offset = 0;
        }
        (head, rest)
    }

    /// The exit (each one the same): progress and native stderr are no longer taken and what of them is queued is
    /// dropped; the unit being written is finished, then rule 2's sentence, then the rest of the entry that unit was
    /// part of, then the client's own words in the order said, then `tail`, then what was not shown (each forwarded
    /// attempt whose native stderr was not all shown, and the progress lines dropped). [`Writer::wait`] then writes
    /// them within the exit's bound.
    pub fn close(&self, tail: &[String]) {
        // Where a test sees that the exit began (debug builds only).
        crate::debug_hold::mark("stderr-exit", "");
        let mut queue = self.locked();
        queue.closed = true;
        let mut dropped: BTreeMap<String, u64> = BTreeMap::new();
        let mut drop_entry = |queue: &mut Queue, entry: Entry| match entry {
            Entry::Progress(_) => {
                queue.progress_dropped += 1;
                None
            }
            Entry::Native { attempt, bytes } => {
                *dropped.entry(attempt).or_default() +=
                    u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                None
            }
            own => Some(own),
        };
        let (head, rest) = Writer::split_first(&mut queue);
        let mut fronts = VecDeque::new();
        let mut own = VecDeque::new();
        if let Some(rest) = rest.and_then(|rest| drop_entry(&mut queue, rest)) {
            match rest {
                Entry::Front(_) => fronts.push_back(rest),
                _ => own.push_back(rest),
            }
        }
        for entry in std::mem::take(&mut queue.entries) {
            match drop_entry(&mut queue, entry) {
                Some(entry @ Entry::Front(_)) => fronts.push_back(entry),
                Some(entry) => own.push_back(entry),
                None => {}
            }
        }
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
        if let Some(head) = head {
            queue.budgeted = head.budgeted();
            fronts.push_front(head);
        } else {
            queue.budgeted = 0;
        }
        queue.entries = fronts;
        drop(queue);
        self.changed.notify_all();
    }

    /// Rule 2's sentence `text` said during the exit: next after the unit being written, before the rest of the
    /// client's words.
    pub fn put_front(&self, text: &str) {
        let mut queue = self.locked();
        let (head, rest) = Writer::split_first(&mut queue);
        if let Some(rest) = rest {
            queue.budgeted += rest.budgeted();
            queue.entries.push_front(rest);
        }
        queue.entries.push_front(Entry::Front(line(text)));
        if let Some(head) = head {
            queue.budgeted += head.budgeted();
            queue.entries.push_front(head);
        }
        drop(queue);
        self.changed.notify_all();
    }

    /// Wait until everything queued is written, until `end` at most, looking at `late` while it waits: a sentence it
    /// gives (rule 2's, for a signal that came meanwhile) goes next ([`Writer::put_front`]). A writer that ended has
    /// what is left written from a thread of its own, within the same bound. Whatever is not written by then is not
    /// (the process ends; there is no other channel).
    pub fn wait(&self, end: Instant, late: &dyn Fn() -> Option<String>) {
        loop {
            if let Some(text) = late() {
                self.put_front(&text);
            }
            let queue = self.locked();
            if queue.entries.is_empty() {
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
            let _ = self
                .changed
                .wait_timeout(queue, left.min(LOOK))
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// [`Writer::close`] and [`Writer::wait`] for `bound` from now, nothing late.
    #[cfg(test)]
    pub fn exit(&self, tail: &[String], bound: Duration) {
        let end = Instant::now() + bound;
        self.close(tail);
        self.wait(end, &|| None);
    }

    /// The writer ended: what is queued and not written (the first entry from where it stopped), written from a
    /// short-lived thread of its own, waited for until `end` at most (nothing when no thread can be made).
    fn direct(&self, end: Instant) {
        let entries: Vec<Vec<u8>> = {
            let mut queue = self.locked();
            let offset = std::mem::take(&mut queue.offset);
            queue
                .entries
                .drain(..)
                .enumerate()
                .map(|(i, entry)| entry.bytes()[if i == 0 { offset } else { 0 }..].to_vec())
                .collect()
        };
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
