//! The client's stderr thread (docs/adr/ADR-031.md decision 11).

use super::*;
use crate::stderr::{MakeSink, thread};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A sink that keeps what it is given, and blocks every write while `held`.
#[derive(Clone, Default)]
struct Kept {
    bytes: Arc<Mutex<Vec<u8>>>,
    held: Arc<std::sync::atomic::AtomicBool>,
}

impl Write for Kept {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        while self.held.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.bytes
            .lock()
            .map_err(|_| std::io::Error::other("poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Kept {
    /// The client's stderr writer, writing here.
    fn writer(&self) -> Arc<Writer> {
        let kept = self.clone();
        let make: MakeSink = Arc::new(move || Box::new(kept.clone()));
        Writer::start(make, &thread("hoh-stderr")).expect("the writer")
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().map(|b| b.clone()).unwrap_or_default())
            .into_owned()
    }
}

#[test]
fn every_stderr_log_of_the_runs_attempts_is_forwarded_as_it_grows() -> R {
    let project = tempfile::tempdir()?;
    let attempt = "R/a1";
    let logs = hoh_app::layout::attempt_logs(project.path(), attempt);
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("exec.stderr.log"), "first\n")?;
    std::fs::write(logs.join("stdout.jsonl"), "not stderr\n")?;
    let kept = Kept::default();
    let writer = kept.writer();
    let stderr = Stderr::start(project.path(), writer.clone());
    stderr.progress("hoh: #1 run.created");
    stderr.attempt(attempt);
    stderr.attempt(attempt);
    std::thread::sleep(Duration::from_millis(400));
    assert!(kept.text().contains("first\n"), "{:?}", kept.text());
    std::fs::OpenOptions::new()
        .append(true)
        .open(logs.join("exec.stderr.log"))?
        .write_all(b"second\n")?;
    std::fs::write(logs.join("stderr.log"), "another\n")?;
    stderr.say("hoh: own");
    stderr.finish();
    writer.exit(&[], crate::stderr::EXIT_WAIT);
    let text = kept.text();
    for once in [
        "first\n",
        "second\n",
        "another\n",
        "hoh: #1 run.created\n",
        "hoh: own\n",
    ] {
        assert_eq!(text.matches(once).count(), 1, "{once:?} in {text:?}");
    }
    assert!(text.find("first") < text.find("second"), "{text:?}");
    assert!(!text.contains("not stderr"), "{text:?}");
    // All of it shown: the exit says nothing of it.
    assert!(!text.contains("未显示"), "{text:?}");
    Ok(())
}

#[test]
fn native_stderr_not_shown_is_said_with_its_directory_and_a_stuck_stderr_never_holds_the_client()
-> R {
    // 设计修正 2, C (CLT-950, CLT-1005, CLT-1104; verification 6): stderr is not read; the follow's progress never
    // waits for it, the finish returns within DRAIN and its flush, and the exit says, for the attempt, at least the
    // bytes of its native stderr that were not shown, with the directory they are in (component(attempt_id) under the
    // project's absolute path), and the progress lines not shown.
    let project = tempfile::tempdir()?;
    let attempt = "R/a%1";
    let logs = hoh_app::layout::attempt_logs(project.path(), attempt);
    std::fs::create_dir_all(&logs)?;
    let native = 3 * (1 << 20);
    std::fs::write(logs.join("stderr.log"), vec![b'n'; native])?;
    let kept = Kept::default();
    kept.held.store(true, std::sync::atomic::Ordering::SeqCst);
    let writer = kept.writer();
    let stderr = Stderr::start(project.path(), writer.clone());
    stderr.attempt(attempt);
    let started = Instant::now();
    for seq in 0..20_000 {
        stderr.progress(&format!("#{seq} run.progress"));
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let started = Instant::now();
    stderr.finish();
    assert!(
        started.elapsed() < DRAIN + FLUSH + Duration::from_millis(300),
        "{:?}",
        started.elapsed()
    );
    // The reader reads from now on: the exit's words come out.
    kept.held.store(false, std::sync::atomic::Ordering::SeqCst);
    writer.exit(&[], Duration::from_secs(5));
    let text = kept.text();
    let shown = text.matches(&"n".repeat(crate::stderr::UNIT)).count() * crate::stderr::UNIT;
    let dir = format!("{}/.hoh/attempts/R%2Fa%251/", project.path().display());
    assert!(Path::new(&dir).is_dir(), "{dir}");
    let said = text
        .lines()
        .find(|l| l.contains("字节原生 stderr 未显示"))
        .unwrap_or_else(|| panic!("{}", &text[text.len().saturating_sub(300)..]))
        .to_owned();
    assert_eq!(
        said,
        format!(
            "hoh: 至少 {} 字节原生 stderr 未显示，见 {dir}",
            native - shown
        )
    );
    assert!(text.trim_end().ends_with("行进度未显示"), "{said}");
    Ok(())
}
