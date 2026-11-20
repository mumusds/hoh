//! The client's one stderr writer (docs/adr/ADR-031.md, 设计修正 2, C).

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// A sink that keeps what it is given and the size of every write; every write blocks while `held`, and fails while
/// `broken` (as a stderr that can never be written: EPIPE).
#[derive(Clone, Default)]
struct Kept {
    bytes: Arc<Mutex<Vec<u8>>>,
    writes: Arc<Mutex<Vec<usize>>>,
    held: Arc<AtomicBool>,
    broken: Arc<AtomicBool>,
}

impl Write for Kept {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        while self.held.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.broken.load(Ordering::SeqCst) {
            return Err(std::io::Error::from_raw_os_error(32));
        }
        self.bytes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        self.writes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(buf.len());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Kept {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().unwrap_or_else(PoisonError::into_inner))
            .into_owned()
    }

    fn make(&self) -> MakeSink {
        let kept = self.clone();
        Arc::new(move || Box::new(kept.clone()))
    }

    fn writer(&self) -> Arc<Writer> {
        Writer::start(self.make(), &thread("hoh-stderr")).expect("the writer's thread")
    }
}

fn no_thread(_work: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
    Err(std::io::Error::from_raw_os_error(11))
}

/// Wait until `kept` holds `text`, at most 5 s.
fn shows(kept: &Kept, text: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        if kept.text().contains(text) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    false
}

#[test]
fn the_writer_writes_in_units_of_at_most_pipe_buf_and_nothing_splits_a_unit() {
    // C: one thread writes fd 2, in units of at most 4 KiB, a unit finished before the next; the client's own words,
    // progress and native stderr said from several threads come out whole, in the order queued.
    let kept = Kept::default();
    let writer = kept.writer();
    let long = "L".repeat(10_000);
    writer.own(&long);
    let natives: Vec<std::thread::JoinHandle<()>> = (0..4)
        .map(|n| {
            let writer = writer.clone();
            std::thread::spawn(move || {
                let unit = vec![b'a' + n; UNIT];
                let until = Instant::now() + Duration::from_secs(5);
                assert_eq!(writer.native(&format!("A{n}"), &unit, until), UNIT);
            })
        })
        .collect();
    for n in 0..100 {
        writer.progress(&format!("#{n} run.progress"));
    }
    for native in natives {
        native.join().expect("a forwarder");
    }
    writer.own("the end");
    assert!(shows(&kept, "the end\n"));
    let writes = kept.writes.lock().map(|w| w.clone()).unwrap_or_default();
    assert!(writes.iter().all(|&n| n <= UNIT), "{writes:?}");
    let text = kept.text();
    assert!(
        text.starts_with(&format!("{long}\n")),
        "the long line is whole"
    );
    for n in 0..4u8 {
        let unit = String::from_utf8(vec![b'a' + n; UNIT]).expect("ascii");
        assert_eq!(text.matches(&unit).count(), 1, "unit {n} whole, once");
    }
    for n in 0..100 {
        assert!(text.contains(&format!("#{n} run.progress\n")), "{n}");
    }
}

#[test]
fn own_words_are_never_dropped_while_progress_past_the_budget_is_and_is_counted_at_the_exit() {
    // C (CLT-800, CLT-901): with stderr not read, progress past the shared 1 MiB budget is dropped as it is queued and
    // counted, so the queue does not grow with the events; the client's own words are kept whatever the budget, and
    // the exit says how many progress lines were not shown.
    let kept = Kept::default();
    kept.held.store(true, Ordering::SeqCst);
    let writer = kept.writer();
    let line = "p".repeat(1023);
    for _ in 0..4096 {
        writer.progress(&line);
    }
    let queued = writer.locked().budgeted;
    assert!(queued <= BUDGET, "{queued}");
    for n in 0..100 {
        writer.own(&format!("hoh: diagnostic {n}"));
    }
    let dropped = writer.locked().progress_dropped;
    // At most the budget queued, and one line being written.
    assert!(dropped >= 4096 - 1025, "{dropped}");
    kept.held.store(false, Ordering::SeqCst);
    writer.exit(&[], Duration::from_secs(5));
    let text = kept.text();
    for n in 0..100 {
        assert!(text.contains(&format!("hoh: diagnostic {n}\n")), "{n}");
    }
    let said = text.lines().last().unwrap_or_default().to_owned();
    let lines: u64 = said
        .trim_start_matches("hoh: ")
        .trim_end_matches(" 行进度未显示")
        .parse()
        .unwrap_or_else(|_| panic!("{said}"));
    // Every line not written is counted: the ones dropped at the queue and the ones the exit dropped.
    let written = u64::try_from(text.matches(&line).count()).expect("count");
    assert_eq!(written + lines, 4096, "{said}");
}

#[test]
fn the_exit_says_rule_twos_sentence_first_then_its_own_words_then_what_was_not_shown() {
    // C (CLT-551, CLT-653, CLT-1002, CLT-950, CLT-1005): at the exit, progress and native stderr still queued are
    // dropped; rule 2's sentence comes first, then the client's own words as said, then the stop's items, then each
    // forwarded attempt's native stderr not shown (what the forwarder did not read plus what the exit dropped), with
    // its directory, and the progress lines not shown.
    let kept = Kept::default();
    kept.held.store(true, Ordering::SeqCst);
    let writer = kept.writer();
    writer.own("hoh: first");
    // The writer takes the first line and is held writing it.
    let end = Instant::now() + Duration::from_secs(5);
    while !writer.locked().writing && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(1));
    }
    let dir = PathBuf::from("/p/.hoh/attempts/R%2Fa1");
    writer.forwarding("R/a1", dir.clone());
    let until = Instant::now() + Duration::from_secs(1);
    assert_eq!(writer.native("R/a1", &[b'n'; 3 * UNIT], until), 3 * UNIT);
    writer.progress("#7 run.progress");
    writer.own("hoh: an error");
    writer.front("hoh: 收到信号，已脱离 Run R");
    writer.unread("R/a1", 100);
    let done = {
        let writer = writer.clone();
        std::thread::spawn(move || {
            writer.exit(
                &["hoh: 一次启动仍未返回，退出后可能出现进程（pid 未知）".to_owned()],
                Duration::from_secs(5),
            );
        })
    };
    while !writer.locked().closed {
        std::thread::sleep(Duration::from_millis(1));
    }
    // Nothing more is taken once the exit began.
    assert_eq!(writer.native("R/a1", b"late", until), 0);
    kept.held.store(false, Ordering::SeqCst);
    done.join().expect("the exit");
    assert_eq!(
        kept.text(),
        format!(
            "hoh: first\nhoh: 收到信号，已脱离 Run R\nhoh: an error\n\
             hoh: 一次启动仍未返回，退出后可能出现进程（pid 未知）\n\
             hoh: 至少 {} 字节原生 stderr 未显示，见 {}/\nhoh: 1 行进度未显示\n",
            3 * UNIT + 100,
            dir.display()
        )
    );
}

#[test]
fn the_exit_ends_within_its_bound_when_stderr_takes_nothing() {
    // C, "the bound first" (CLT-201, CLT-956): a reader that never reads holds the writer in its unit; the exit
    // returns within its bound all the same.
    let kept = Kept::default();
    kept.held.store(true, Ordering::SeqCst);
    let writer = kept.writer();
    writer.own("hoh: never read");
    writer.front("hoh: 收到信号，没有附着");
    let started = Instant::now();
    writer.exit(&[], EXIT_WAIT);
    let took = started.elapsed();
    assert!(took >= EXIT_WAIT && took < EXIT_WAIT * 2, "{took:?}");
    kept.held.store(false, Ordering::SeqCst);
}

#[test]
fn a_writer_that_ended_has_the_exit_write_its_own_words_directly_within_the_bound() {
    // C: the writer ended (stderr could not be written); the exit finds it gone and writes the client's own words
    // from a short-lived thread of its own (the first written exception), dropping progress as any exit does.
    let kept = Kept::default();
    let writer = kept.writer();
    kept.broken.store(true, Ordering::SeqCst);
    writer.own("hoh: lost to a broken stderr");
    let end = Instant::now() + Duration::from_secs(5);
    while !writer.locked().ended && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(writer.locked().ended, "the writer ended");
    writer.own("hoh: an error");
    writer.progress("#1 run.created");
    kept.broken.store(false, Ordering::SeqCst);
    writer.exit(&["hoh: tail".to_owned()], EXIT_WAIT);
    assert_eq!(
        kept.text(),
        "hoh: an error\nhoh: tail\nhoh: 1 行进度未显示\n"
    );
}

#[test]
fn no_writer_thread_is_an_error_and_nothing_is_written() {
    // C (CLT-900, CLT-1003): no thread for the writer: an error, and nothing written.
    let kept = Kept::default();
    assert!(Writer::start(kept.make(), &no_thread).is_err());
    assert_eq!(kept.text(), "");
}

#[test]
fn native_stderr_waits_for_room_until_its_deadline_and_says_how_much_it_queued() {
    // C: native stderr is never dropped as it is queued; the forwarder waits for room, until its deadline.
    let kept = Kept::default();
    kept.held.store(true, Ordering::SeqCst);
    let writer = kept.writer();
    let until = Instant::now() + Duration::from_millis(300);
    let chunk = vec![b'x'; 2 * BUDGET];
    let queued = writer.native("A", &chunk, until);
    assert!(
        (BUDGET - UNIT..=BUDGET + UNIT).contains(&queued),
        "{queued}"
    );
    assert!(Instant::now() >= until);
    assert!(!writer.flushed(Instant::now() + Duration::from_millis(50)));
    kept.held.store(false, Ordering::SeqCst);
    assert!(writer.flushed(Instant::now() + Duration::from_secs(5)));
}
