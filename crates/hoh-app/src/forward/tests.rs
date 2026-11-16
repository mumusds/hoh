//! The client's stderr thread (docs/adr/ADR-031.md decision 11).

use super::*;
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
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes.lock().map(|b| b.clone()).unwrap_or_default())
            .into_owned()
    }
}

#[test]
fn every_stderr_log_of_the_runs_attempts_is_forwarded_as_it_grows() -> R {
    let project = tempfile::tempdir()?;
    let attempt = "R/a1";
    let logs = hoh_app::host::product::attempt_logs(project.path(), attempt);
    std::fs::create_dir_all(&logs)?;
    std::fs::write(logs.join("exec.stderr.log"), "first\n")?;
    std::fs::write(logs.join("stdout.jsonl"), "not stderr\n")?;
    let kept = Kept::default();
    let stderr = Stderr::start_to(project.path(), Box::new(kept.clone()));
    stderr.say("hoh: #1 run.created");
    stderr.attempt(attempt);
    stderr.attempt(attempt);
    std::thread::sleep(Duration::from_millis(400));
    assert!(kept.text().contains("first\n"), "{:?}", kept.text());
    {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(logs.join("exec.stderr.log"))?
            .write_all(b"second\n")?;
    }
    std::fs::write(logs.join("stderr.log"), "another\n")?;
    stderr.finish();
    let text = kept.text();
    for once in ["first\n", "second\n", "another\n", "hoh: #1 run.created\n"] {
        assert_eq!(text.matches(once).count(), 1, "{once:?} in {text:?}");
    }
    assert!(text.find("first") < text.find("second"), "{text:?}");
    assert!(!text.contains("not stderr"), "{text:?}");
    Ok(())
}

#[test]
fn a_stderr_that_blocks_never_holds_the_caller_and_what_did_not_fit_is_counted() -> R {
    let project = tempfile::tempdir()?;
    let kept = Kept::default();
    kept.held.store(true, std::sync::atomic::Ordering::SeqCst);
    let stderr = Stderr::start_to(project.path(), Box::new(kept.clone()));
    let line = "x".repeat(1023);
    let started = Instant::now();
    for _ in 0..4096 {
        stderr.say(&line);
    }
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    kept.held.store(false, std::sync::atomic::Ordering::SeqCst);
    stderr.finish();
    let text = kept.text();
    let written = text.matches(&line).count();
    assert!(written > 0 && written < 4096, "{written}");
    let omitted = format!("省略了 {} 字节", (4096 - written) * 1024);
    assert!(
        text.trim_end().ends_with(&omitted),
        "{omitted}: {:?}",
        &text[text.len().saturating_sub(200)..]
    );
    // A stderr that never takes anything: the finish still returns, within its bound.
    let stuck = Kept::default();
    stuck.held.store(true, std::sync::atomic::Ordering::SeqCst);
    let stderr = Stderr::start_to(project.path(), Box::new(stuck.clone()));
    stderr.say("never written");
    let started = Instant::now();
    stderr.finish();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    stuck.held.store(false, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}
