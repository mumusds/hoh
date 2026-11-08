//! `hoh` with a terminal on stdin and stdout: the Workspace shell (REQ-001; ADR-014). Signals are registered before
//! the terminal changes, so a SIGTERM, SIGHUP or SIGINT at any point after that restores it. One thread does it all:
//! the loop waits for input in poll(2), in short slices, looks at the signal flags between them, reads only what poll
//! said is there and decodes it itself (`input`), and ends when the terminal hangs up. Leaving only closes this
//! client.

mod app;
mod editor;
mod history;
mod input;
mod terminal;
#[cfg(test)]
mod tests;

use crate::exit::{ClientEvent, Ending, exit_code};
use app::{App, Exit};
use history::History;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyEventKind};
use rustix::event::{PollFd, PollFlags, Timespec};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM, SIGWINCH};
use std::io::Stdin;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

/// How long one wait for input lasts before the signal flags are looked at again (a signal also ends it early).
const TICK: Duration = Duration::from_millis(100);

/// How long the loop waits for more after a lone ESC: then it is the Esc key.
const ESC_WAIT: Duration = Duration::from_millis(30);

/// How long the loop waits for the rest of a begun sequence (`ESC [`, a paste marker, a character in pieces), which
/// a slow link may split: then what came is settled (M1T08R-20). A signal still ends the wait at once.
const SEQUENCE_WAIT: Duration = Duration::from_millis(500);

/// What ended the loop.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Left(Exit),
    Signal(usize),
}

fn code(event: ClientEvent) -> u8 {
    exit_code(Ending::Client(event)).unwrap_or(1)
}

fn signal_code(signal: usize) -> u8 {
    match i32::try_from(signal) {
        Ok(SIGTERM) => code(ClientEvent::Sigterm),
        Ok(SIGHUP) => code(ClientEvent::Sighup),
        _ => code(ClientEvent::Sigint),
    }
}

/// A run the shell is attached to (docs/adr/ADR-021.md decision 10): its host, its id, and its events as a reader
/// thread receives them over the host's socket. The host is the one found last: a host started again (another token)
/// is found again by the reader and by the commands, which go on after the last event and with the same key
/// (M2T06R-04).
pub struct Link {
    host: Arc<std::sync::Mutex<crate::client::HostInfo>>,
    project: std::path::PathBuf,
    run_id: String,
    events: std::sync::mpsc::Receiver<serde_json::Value>,
    /// Ends the reader thread (it looks between reads, at least every 200 ms).
    stop: Arc<AtomicBool>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Link {
    fn host(&self) -> crate::client::HostInfo {
        current(&self.host)
    }

    /// GET `path` from the run's host; once more from the host found again when it does not answer or no longer
    /// knows the token.
    fn get(&self, path: &str) -> Result<crate::client::Reply, String> {
        match crate::client::call(&self.host(), "GET", path, None) {
            Ok(reply) if reply.status != 403 => return Ok(reply),
            _ => {}
        }
        let found =
            again(&self.project, &self.host).map_err(|f| format!("{}: {}", f.code, f.message))?;
        crate::client::call(&found, "GET", path, None).map_err(|e| e.to_string())
    }
}

fn current(host: &std::sync::Mutex<crate::client::HostInfo>) -> crate::client::HostInfo {
    host.lock()
        .map_or_else(|p| p.into_inner().clone(), |h| h.clone())
}

/// The project's host found (or started) again, kept for the link.
fn again(
    project: &Path,
    host: &std::sync::Mutex<crate::client::HostInfo>,
) -> Result<crate::client::HostInfo, crate::client::Failure> {
    let found = crate::client::ensure(project)?;
    match host.lock() {
        Ok(mut h) => *h = found.clone(),
        Err(p) => *p.into_inner() = found.clone(),
    }
    Ok(found)
}

/// `hoh attach` with a terminal: the shell bound to `run` (getRun's answer) of `host`; the exit code. Leaving, by the
/// choice or a signal, only closes this client (REQ-043 behaviour 6).
pub fn attach(project: &Path, host: crate::client::HostInfo, run: &serde_json::Value) -> u8 {
    let run_id = run["run_id"].as_str().unwrap_or_default().to_owned();
    let label = run["projection"]["label"]
        .as_str()
        .or(run["state"].as_str())
        .unwrap_or_default()
        .to_owned();
    let (send, events) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let host = Arc::new(std::sync::Mutex::new(host));
    {
        let (host, run_id, stop) = (host.clone(), run_id.clone(), stop.clone());
        let project = project.to_path_buf();
        std::thread::spawn(move || {
            let mut last = 0i64;
            while !stop.load(Ordering::SeqCst) {
                let Ok(mut stream) = crate::client::Events::open(&current(&host), &run_id, last)
                else {
                    // The host went or was started again: found again, the stream goes on after `last`.
                    std::thread::sleep(Duration::from_millis(500));
                    let _ = again(&project, &host);
                    continue;
                };
                while !stop.load(Ordering::SeqCst) {
                    match stream.next() {
                        Ok(Some(event)) => {
                            last = event["seq"].as_i64().unwrap_or(last).max(last);
                            if send.send(event).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
            }
        });
    }
    let link = Link {
        host,
        project: project.to_path_buf(),
        run_id,
        events,
        stop,
    };
    run_with(project, Some((link, label)))
}

/// Run the shell for the project at `project`; the exit code.
pub fn run(project: &Path) -> u8 {
    run_with(project, None)
}

fn run_with(project: &Path, link: Option<(Link, String)>) -> u8 {
    let signal = Arc::new(AtomicUsize::new(0));
    for sig in [SIGTERM, SIGHUP, SIGINT] {
        let value = usize::try_from(sig).unwrap_or(usize::MAX);
        if let Err(e) = signal_hook::flag::register_usize(sig, Arc::clone(&signal), value) {
            say!("hoh：无法登记信号处理：{e}");
            return code(ClientEvent::TerminalIo);
        }
    }
    let resized = Arc::new(AtomicBool::new(false));
    if let Err(e) = signal_hook::flag::register(SIGWINCH, Arc::clone(&resized)) {
        say!("hoh：无法登记信号处理：{e}");
        return code(ClientEvent::TerminalIo);
    }
    let name = project.file_name().map_or_else(
        || project.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let mut app = App::new(name, History::load(project));
    let link = link.map(|(link, label)| {
        app.attach(&link.run_id, &label);
        link
    });
    terminal::restore_on_panic();
    let (guard, entered) = terminal::enter();
    let end = entered.and_then(|()| event_loop(&mut app, &signal, &resized, link.as_ref()));
    drop(link);
    terminal::debug_restore_gate();
    let restored = guard.finish();
    // Loaded after the restore: a signal that came while restoring decides too (M1T08R-06).
    let (code, message) = outcome(end, restored, signal.load(Ordering::SeqCst));
    if let Some(message) = message {
        terminal::settle();
        say!("hoh：{message}");
    }
    code
}

/// The exit code of a way out and what to say on stderr (ADR-014 decision 3): a received signal decides the code on
/// every path (a hangup also makes the terminal fail; one may come while a detach is restored); otherwise a failing
/// terminal or a restore that did not complete is 74, never 0.
fn outcome(
    end: std::io::Result<End>,
    restored: std::io::Result<()>,
    signal: usize,
) -> (u8, Option<String>) {
    let not_restored = restored
        .err()
        .map(|e| format!("终端未能恢复：{e}；可运行 stty sane 或 reset"));
    let signal = match end {
        Ok(End::Signal(sig)) => sig,
        _ => signal,
    };
    if signal != 0 {
        return (signal_code(signal), not_restored);
    }
    match end {
        Ok(End::Signal(_)) => (signal_code(signal), not_restored),
        Ok(End::Left(Exit::Detach | Exit::Pause | Exit::Cancel)) => match not_restored {
            None => (0, None),
            Some(m) => (code(ClientEvent::TerminalIo), Some(m)),
        },
        Err(e) => (
            code(ClientEvent::TerminalIo),
            Some(match not_restored {
                None => format!("终端读写失败，TUI 已退出：{e}"),
                Some(m) => format!("终端读写失败，TUI 已退出：{e}；{m}"),
            }),
        ),
    }
}

/// The attached run's events received so far, shown; a run event also refreshes its label.
fn take_events(app: &mut App, link: &Link) -> bool {
    let mut any = false;
    while let Ok(event) = link.events.try_recv() {
        let kind = event["kind"].as_str().unwrap_or_default().to_owned();
        app.say(format!("#{} {kind}", event["seq"].as_i64().unwrap_or(0)));
        if kind.starts_with("run.")
            && let Ok(reply) =
                link.get(&format!("/v1/runs/{}", crate::client::encode(&link.run_id)))
            && reply.status == 200
        {
            let run = reply.json();
            if let Some(label) = run["projection"]["label"].as_str() {
                app.relabel(label);
            }
        }
        any = true;
    }
    any
}

/// Send PauseRun or CancelRun for the attached run at its current revision (once more on a stale one); what to
/// say about it.
fn control(link: &Link, kind: &str) -> String {
    let mut said = String::new();
    for _ in 0..2 {
        let revision = match link.get(&format!("/v1/runs/{}", crate::client::encode(&link.run_id)))
        {
            Ok(reply) if reply.status == 200 => reply.json()["revision"].as_i64().unwrap_or(0),
            Ok(reply) => return format!("{kind} 未发送：host 答 {}", reply.status),
            Err(e) => return format!("{kind} 未发送：{e}"),
        };
        let command = serde_json::json!({
            "expected_revision": revision,
            "kind": kind,
            "run_id": link.run_id,
            "payload": {"reason": "TUI"},
        });
        match crate::client::submit(&link.project, &link.host(), command) {
            Ok(_) => return format!("{kind} 已受理"),
            Err(failure) if failure.code == "STALE_REVISION" => {
                said = format!("{kind} 未受理：{}：{}", failure.code, failure.message);
            }
            Err(failure) => return format!("{kind} 未受理：{}：{}", failure.code, failure.message),
        }
    }
    said
}

fn event_loop(
    app: &mut App,
    signal: &AtomicUsize,
    resized: &AtomicBool,
    link: Option<&Link>,
) -> std::io::Result<End> {
    let mut terminal = Terminal::new(CrosstermBackend::new(terminal::Output::new()?))?;
    let stdin = std::io::stdin();
    let mut decoder = input::Decoder::default();
    let mut buf = [0u8; 4096];
    let mut dirty = true;
    loop {
        let sig = signal.load(Ordering::SeqCst);
        if sig != 0 {
            return Ok(End::Signal(sig));
        }
        if let Some(link) = link
            && take_events(app, link)
        {
            dirty = true;
        }
        if resized.swap(false, Ordering::SeqCst) {
            dirty = true;
        }
        if dirty {
            terminal.draw(|f| app.draw(f))?;
            dirty = false;
            debug_panic();
        }
        let limit = match decoder.pending() {
            input::Pending::Nothing => TICK,
            input::Pending::Esc => ESC_WAIT,
            input::Pending::Sequence => SEQUENCE_WAIT,
        };
        let idle = match wait(&stdin, limit)? {
            Wait::Input => {
                // A signal that came while the loop drew (a held terminal blocks the write) ends it before it reads
                // what was typed after the signal (M1T08R-19).
                if signal.load(Ordering::SeqCst) != 0 {
                    continue;
                }
                // poll said there is input (or end of file): this read does not block.
                let n = match rustix::io::read(&stdin, &mut buf) {
                    Ok(n) => n,
                    Err(rustix::io::Errno::INTR) => continue,
                    Err(e) => return Err(e.into()),
                };
                if n == 0 {
                    return Err(hangup());
                }
                decoder.feed(&buf[..n]);
                false
            }
            Wait::Idle => true,
            Wait::Interrupted => continue,
        };
        match handle(
            app,
            &mut decoder,
            idle,
            || signal.load(Ordering::SeqCst),
            &mut dirty,
        ) {
            // Pause and cancel are sent and the shell stays (REQ-043 behaviour 6, ADR-021 decision 10).
            Some(End::Left(exit @ (Exit::Pause | Exit::Cancel))) => {
                if let Some(link) = link {
                    let kind = if exit == Exit::Pause {
                        "PauseRun"
                    } else {
                        "CancelRun"
                    };
                    app.say(control(link, kind));
                }
                dirty = true;
            }
            Some(end) => return Ok(end),
            None => {}
        }
    }
}

/// Act on the events decoded from what was read, one at a time. The signal flag is looked at before each, so a long
/// batch (each Enter writes the history) cannot hold up the exit (M1T08R-21); what is left is not acted on.
fn handle(
    app: &mut App,
    decoder: &mut input::Decoder,
    idle: bool,
    signal: impl Fn() -> usize,
    dirty: &mut bool,
) -> Option<End> {
    loop {
        let sig = signal();
        if sig != 0 {
            return Some(End::Signal(sig));
        }
        match decoder.next(idle)? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if let Some(exit) = app.key(key) {
                    return Some(End::Left(exit));
                }
                *dirty = true;
            }
            Event::Paste(text) => {
                app.paste(&text);
                *dirty = true;
            }
            _ => {}
        }
    }
}

/// How a wait for input ended.
enum Wait {
    /// There is something to read.
    Input,
    /// Nothing came within the time.
    Idle,
    /// A signal came: its flag is to be looked at.
    Interrupted,
}

/// Wait up to `timeout` for input on the terminal (stdin, which bare `hoh` requires to be one), without reading
/// anything. A hung-up terminal (its window or connection closed) is an error.
fn wait(stdin: &Stdin, timeout: Duration) -> std::io::Result<Wait> {
    let mut fds = [PollFd::new(stdin, PollFlags::IN)];
    let limit = Timespec {
        tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(timeout.subsec_nanos()),
    };
    match rustix::event::poll(&mut fds, Some(&limit)) {
        Ok(0) => Ok(Wait::Idle),
        Ok(_)
            if fds[0]
                .revents()
                .intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) =>
        {
            Err(hangup())
        }
        Ok(_) => Ok(Wait::Input),
        Err(rustix::io::Errno::INTR) => Ok(Wait::Interrupted),
        Err(e) => Err(e.into()),
    }
}

fn hangup() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "终端已断开")
}

/// Debug builds only: `HOH_DEBUG_TUI_PANIC` panics after a draw, so the PTY test can see the terminal restored
/// before the message (ADR-014 decision 8). Release builds have no such entry.
fn debug_panic() {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOH_DEBUG_TUI_PANIC").is_some() {
        panic!("HOH_DEBUG_TUI_PANIC: a panic asked for by the PTY test");
    }
}
