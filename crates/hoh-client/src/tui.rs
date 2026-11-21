//! `hoh` with a terminal on stdin and stdout: the Workspace shell (REQ-001; ADR-014; docs/adr/ADR-031.md decision 13).
//! Signals are registered first; bare `hoh` then refuses an invalid configuration (78) and ensures the project's host
//! on a worker thread while it looks at the signal flags (a host that cannot be had is 69), lists every run, and only
//! then changes the terminal, so a SIGTERM, SIGHUP or SIGINT at any point after that restores it. One thread does the
//! rest: the loop waits for input in poll(2), in short slices, looks at the signal flags between them, reads only what
//! poll said is there and decodes it itself (`input`), does what Enter or the selector asked of the host in order, and
//! ends when the terminal hangs up. Opening, listing, attaching and leaving write nothing (E11); leaving only closes
//! this client.

mod app;
mod editor;
mod history;
mod input;
mod settings;
mod terminal;
#[cfg(test)]
mod tests;

use crate::client::{Failure, HostInfo};
use crate::exit::{ClientEvent, Ending, exit_code};
use crate::signals::State;
use app::{App, Exit, Request};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{Event, KeyEventKind};
use rustix::event::{PollFd, PollFlags, Timespec};
use serde_json::{Value, json};
use std::io::Stdin;
use std::path::{Path, PathBuf};
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
    crate::signals::code(signal)
}

/// What a wait of the shell answered, or that a signal ended it (docs/adr/ADR-031.md, 设计修正（客户端信号）, rule 3):
/// the loop then ends at its next look.
fn waited<T>(answer: Result<Result<T, String>, u8>) -> Result<T, String> {
    answer.unwrap_or_else(|code| Err(format!("收到信号（{code}）")))
}

/// The project's host as the shell talks to it: the one found last, which a host started again (another token)
/// replaces for the shell and its link (M2T06R-04).
pub struct Conn {
    host: Arc<std::sync::Mutex<HostInfo>>,
    project: PathBuf,
}

impl Conn {
    fn new(project: &Path, host: HostInfo) -> Conn {
        Conn {
            host: Arc::new(std::sync::Mutex::new(host)),
            project: project.to_path_buf(),
        }
    }

    fn host(&self) -> HostInfo {
        current(&self.host)
    }

    /// GET `path`, once more from the host found again when it does not answer or no longer knows the token; the
    /// client's signal state looked at while it waits.
    fn get(&self, path: &str) -> Result<crate::client::Reply, String> {
        let (project, host, path) = (self.project.clone(), self.host.clone(), path.to_owned());
        waited(crate::signals::wait(move || {
            get_from(&project, &host, &path)
        }))
    }

    /// Every run of the host (every page); the client's signal state looked at while it waits.
    fn runs(&self) -> Result<Vec<Value>, Failure> {
        let host = self.host();
        crate::signals::wait(move || crate::client::runs(&host))
            .unwrap_or_else(|code| Err(Failure::unreachable(format!("收到信号（{code}）"))))
    }

    fn submit(&self, command: Value) -> Result<Value, Failure> {
        crate::client::submit(&self.project, &self.host(), command)
    }

    /// The link to run `run_id`: a reader thread receives its events from the first on, from the host found last.
    fn link(&self, run_id: &str) -> Link {
        let (send, events) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let (host, run_id, stop) = (self.host.clone(), run_id.to_owned(), stop.clone());
            let project = self.project.clone();
            std::thread::spawn(move || {
                let mut last = 0i64;
                let mut trace = received_trace();
                while !stop.load(Ordering::SeqCst) {
                    let Ok(mut stream) =
                        crate::client::Events::open(&current(&host), &run_id, last)
                    else {
                        // The host went or was started again: found again, the stream goes on after `last`.
                        std::thread::sleep(Duration::from_millis(500));
                        let _ = again(&project, &host);
                        continue;
                    };
                    while !stop.load(Ordering::SeqCst) {
                        match stream.next() {
                            Ok(Some(event)) => {
                                if let Some(trace) = trace.as_mut() {
                                    use std::io::Write as _;
                                    let _ = writeln!(trace, "{}", event["seq"]);
                                }
                                last = event["seq"].as_i64().unwrap_or(last).max(last);
                                if send.send(Got::Event(event)).is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            // An event that cannot be read is never skipped (docs/adr/ADR-031.md decision 10), and
                            // reading it again gives the same: the following stops, no new event is shown, and the
                            // shell says which event it was (its SSE id).
                            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                                let _ = send.send(Got::Unreadable(e.to_string()));
                                return;
                            }
                            Err(_) => break,
                        }
                    }
                }
            });
        }
        Link {
            host: self.host.clone(),
            project: self.project.clone(),
            run_id: run_id.to_owned(),
            events,
            stop,
        }
    }
}

/// What a link's reader thread hands the shell.
enum Got {
    /// An event of the run.
    Event(Value),
    /// An event whose data could not be read, as the error names it (its SSE id): the reader follows no further.
    Unreadable(String),
}

/// A run the shell is attached to (docs/adr/ADR-021.md decision 10): its host, its id, and its events as a reader
/// thread receives them over the host's socket. The host is the one found last: a host started again (another token)
/// is found again by the reader and by the commands, which go on after the last event and with the same key
/// (M2T06R-04).
pub struct Link {
    host: Arc<std::sync::Mutex<HostInfo>>,
    project: PathBuf,
    run_id: String,
    events: std::sync::mpsc::Receiver<Got>,
    /// Ends the reader thread (it looks between reads, at least every 200 ms).
    stop: Arc<AtomicBool>,
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

impl Link {
    fn host(&self) -> HostInfo {
        current(&self.host)
    }

    /// GET `path` from the run's host; once more from the host found again when it does not answer or no longer
    /// knows the token; the client's signal state looked at while it waits.
    fn get(&self, path: &str) -> Result<crate::client::Reply, String> {
        let (project, host, path) = (self.project.clone(), self.host.clone(), path.to_owned());
        waited(crate::signals::wait(move || {
            get_from(&project, &host, &path)
        }))
    }
}

/// What the shell's input needs of its run's host (docs/adr/ADR-031.md decision 13): the run as getRun shows it and
/// the commands; a script in the tests.
pub(crate) trait Port {
    /// The run (getRun), `None` when the host has no such run.
    fn run(&self) -> Result<Option<Value>, Failure>;
    /// Submit a command (`client::submit`): the accepted body, or why not.
    fn submit(&self, command: Value) -> Result<Value, Failure>;
}

impl Port for Link {
    fn run(&self) -> Result<Option<Value>, Failure> {
        match self.get(&format!("/v1/runs/{}", crate::client::encode(&self.run_id))) {
            Ok(reply) if reply.status == 200 => Ok(Some(reply.json())),
            Ok(reply) if reply.status == 404 => Ok(None),
            Ok(reply) => Err(Failure::unreachable(format!(
                "the host answered {}",
                reply.status
            ))),
            Err(e) => Err(Failure::unreachable(e)),
        }
    }

    fn submit(&self, command: Value) -> Result<Value, Failure> {
        crate::client::submit(&self.project, &self.host(), command)
    }
}

/// What became of an input: what to say now, and the SendInput's command whose acceptance event is to be said
/// when it comes.
#[derive(Debug, Default)]
pub(crate) struct Sent {
    pub said: Vec<String>,
    pub awaiting: Option<String>,
}

/// Enter while attached (docs/adr/ADR-031.md decisions 13 and 16, the same as `hoh -p --resume`): SendInput{text,
/// target_role: null, REPLY} at the run's revision (read again while it is stale), then, when the run was
/// INCOMPLETE, ResumeRun at the revision the input left. A resume refused because the run's stop still holds names
/// that stop; any refusal is said with its code.
pub(crate) fn input(port: &impl Port, run_id: &str, text: &str) -> Sent {
    let said = |line: String| Sent {
        said: vec![line],
        awaiting: None,
    };
    let run = match port.run() {
        Ok(Some(run)) => run,
        Ok(None) => return said(format!("输入未发送：没有 Run {run_id}")),
        Err(f) => return said(format!("输入未发送：{}：{}", f.code, f.message)),
    };
    let read = || port.run();
    let accepted = match crate::client::fresh(
        |command| port.submit(command),
        read,
        run_id,
        run["revision"].as_i64().unwrap_or_default(),
        |revision| {
            json!({
                "expected_revision": revision,
                "kind": "SendInput",
                "run_id": run_id,
                "payload": {"text": text, "target_role": null, "input_kind": "REPLY"},
            })
        },
    ) {
        Ok(accepted) => accepted,
        Err(f) => return said(format!("输入未受理：{}：{}", f.code, f.message)),
    };
    let mut sent = Sent {
        said: Vec::new(),
        awaiting: accepted["command_id"].as_str().map(str::to_owned),
    };
    if run["state"] == "INCOMPLETE" {
        match crate::client::fresh(
            |command| port.submit(command),
            read,
            run_id,
            accepted["revision"].as_i64().unwrap_or_default(),
            |revision| {
                json!({
                    "expected_revision": revision,
                    "kind": "ResumeRun",
                    "run_id": run_id,
                    "payload": {"reason": "TUI"},
                })
            },
        ) {
            Ok(_) => sent.said.push("ResumeRun 已受理".to_owned()),
            Err(f) if crate::prompt::still_stopped(&f) => sent.said.push(format!(
                "Run 未恢复：使它停下的预算或期限仍耗尽（{}）；本版本没有提高预算或期限的入口，续做剩余需求请新建 Run",
                crate::prompt::stop_of(&f)
            )),
            Err(f) => sent
                .said
                .push(format!("ResumeRun 未受理：{}：{}", f.code, f.message)),
        }
    }
    sent
}

fn current(host: &std::sync::Mutex<HostInfo>) -> HostInfo {
    host.lock()
        .map_or_else(|p| p.into_inner().clone(), |h| h.clone())
}

/// GET `path` from `host`; once more from the host found again when it does not answer or no longer knows the token.
fn get_from(
    project: &Path,
    host: &std::sync::Mutex<HostInfo>,
    path: &str,
) -> Result<crate::client::Reply, String> {
    match crate::client::call(&current(host), "GET", path, None) {
        Ok(reply) if reply.status != 403 => return Ok(reply),
        _ => {}
    }
    let found = again(project, host).map_err(|f| format!("{}: {}", f.code, f.message))?;
    crate::client::call(&found, "GET", path, None).map_err(|e| e.to_string())
}

/// The project's host found (or started) again, kept for the link.
fn again(project: &Path, host: &std::sync::Mutex<HostInfo>) -> Result<HostInfo, Failure> {
    let found = crate::client::ensure(project)?;
    match host.lock() {
        Ok(mut h) => *h = found.clone(),
        Err(p) => *p.into_inner() = found.clone(),
    }
    Ok(found)
}

/// `hoh attach` with a terminal: the shell bound to `run` (getRun's answer) of `host`, with the signal `state` the
/// attach registered before it waited for the host; the exit code. A signal that came before ends it before the
/// terminal changes. Leaving, by the choice or a signal, only closes this client (REQ-043 behaviour 6).
pub fn attach(project: &Path, host: HostInfo, run: &Value, state: &State) -> u8 {
    if let Some(code) = state.received() {
        return code;
    }
    let run_id = run["run_id"].as_str().unwrap_or_default().to_owned();
    let label = run["projection"]["label"]
        .as_str()
        .or(run["state"].as_str())
        .unwrap_or_default()
        .to_owned();
    let conn = Conn::new(project, host);
    // The runs for the top bar's baseline (docs/adr/ADR-031.md decision 13); a signal while they are read ends it
    // with its code.
    let runs = match conn.runs() {
        Ok(runs) => runs,
        Err(failure) => {
            return state
                .received()
                .unwrap_or_else(|| crate::session::fail(&failure));
        }
    };
    let mut app = App::open(project);
    app.list(&runs);
    let link = conn.link(&run_id);
    app.attach(&run_id, &label);
    app.projection(run);
    app.tracking(&run["projection"]["degraded_tracking"]);
    run_with(app, state, &conn, Some(link))
}

/// `hoh attach` with no run named, several not ended and a terminal (docs/adr/ADR-031.md decision 12): the shell with
/// the selector of the runs not ended among `runs` (list_runs' items, every page) open; the run chosen is attached,
/// Esc or the Ctrl+C choice "脱离" leaves with 0. `state` as [`attach`]'s.
pub fn pick(project: &Path, host: HostInfo, runs: &[Value], state: &State) -> u8 {
    if let Some(code) = state.received() {
        return code;
    }
    let conn = Conn::new(project, host);
    let mut app = App::open(project);
    app.list(runs);
    app.pick();
    run_with(app, state, &conn, None)
}

/// Bare `hoh`: the shell for the project at `project`, unattached; the exit code (docs/adr/ADR-031.md decision 13).
/// Its signal state is registered before anything else and its exit consults it (the design correction, rules 1 and
/// 2); being the shell from the start, it ends on a signal without a word, as the shell does.
pub fn run(project: &Path) -> u8 {
    let state = match crate::signals::register() {
        Ok(state) => state,
        Err(code) => return code,
    };
    state.mark_said();
    let code = open(state, project);
    state.exit(code, "")
}

fn open(state: &State, project: &Path) -> u8 {
    // Before the terminal changes and before any host (REQ-001 failure mode 4).
    if let Err(e) = hoh_core::config::load(project) {
        say!("hoh：配置无效，没有连接或启动 host：{e}");
        return code(ClientEvent::Config);
    }
    let (host, runs) = match connect(project, state) {
        Ok(found) => found,
        Err(code) => return code,
    };
    // A signal that came while the runs were read ends it before the terminal changes.
    if let Some(code) = state.received() {
        return code;
    }
    let conn = Conn::new(project, host);
    let mut app = App::open(project);
    app.list(&runs);
    run_with(app, state, &conn, None)
}

/// The project's host (ensured: found, or started and waited for) and every run of it, while the client looks at its
/// signal `state` (the design correction, rule 3): a signal ends the wait with its code and the terminal untouched; a
/// host that cannot be had or read is said on stderr, with its code (69; 78 for a refused location).
fn connect(project: &Path, state: &State) -> Result<(HostInfo, Vec<Value>), u8> {
    let dir = project.to_path_buf();
    let found = state.wait(move || {
        crate::client::ensure(&dir)
            .and_then(|host| crate::client::runs(&host).map(|runs| (host, runs)))
    })?;
    found.map_err(|failure| crate::session::fail(&failure))
}

fn run_with(mut app: App, state: &State, conn: &Conn, mut link: Option<Link>) -> u8 {
    // The shell only restores the terminal: nothing else says a signal (the design correction, rule 2).
    state.mark_said();
    let (signal, resized) = (state.signal(), state.resized());
    terminal::restore_on_panic();
    let (guard, entered) = terminal::enter();
    let end = entered.and_then(|()| event_loop(&mut app, signal, resized, conn, &mut link));
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

/// The attached run's events received so far, shown and counted; a run event also refreshes its label. An event that
/// could not be read is said with its SSE id, and that no event after it is followed (docs/adr/ADR-031.md decision 10).
fn take_events(app: &mut App, link: &Link) -> bool {
    let mut any = false;
    while let Ok(got) = link.events.try_recv() {
        any = true;
        let event = match got {
            Got::Event(event) => event,
            Got::Unreadable(why) => {
                app.say(format!("{why}；之后的事件不再跟随（脱离后重新附着可再试）"));
                continue;
            }
        };
        if app.event(&event)
            && let Ok(reply) =
                link.get(&format!("/v1/runs/{}", crate::client::encode(&link.run_id)))
            && reply.status == 200
        {
            let run = reply.json();
            if let Some(label) = run["projection"]["label"].as_str() {
                app.relabel(label);
            }
            app.projection(&run);
            app.tracking(&run["projection"]["degraded_tracking"]);
        }
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

/// Do what Enter or the selector asked of the host (docs/adr/ADR-031.md decision 13): a new run is started and
/// attached, a chosen run attached, an input sent to the attached run; each outcome said on the message bar.
fn perform(app: &mut App, conn: &Conn, link: &mut Option<Link>, request: Request) {
    match request {
        Request::Start { goal, mode } => {
            let command = json!({
                "expected_revision": 0,
                "kind": "StartRun",
                "run_id": null,
                "payload": {"goal": goal, "mode": mode, "interactive": true},
            });
            match conn.submit(command) {
                Ok(accepted) => {
                    let run_id = accepted["run_id"].as_str().unwrap_or_default().to_owned();
                    if let Err(why) = attach_to(app, conn, link, &run_id) {
                        app.say(format!("已新建 Run {run_id}，但未能附着：{why}"));
                        return;
                    }
                    app.say(format!("已新建 Run {run_id}，已附着"));
                }
                Err(f) => app.say(format!("未提交：{}：{}", f.code, f.message)),
            }
        }
        Request::SetMode(mode) => set_mode(app, link.as_ref(), &mode),
        Request::CycleMode => {
            if let Some(link) = link.as_ref()
                && let Ok(Some(run)) = link.run()
            {
                app.projection(&run);
            }
            let mode = app.next_mode();
            set_mode(app, link.as_ref(), &mode);
        }
        Request::Attach(run_id) => match attach_to(app, conn, link, &run_id) {
            Ok(()) => app.say(format!("已附着 Run {run_id}")),
            Err(why) => app.say(format!("未能附着 Run {run_id}：{why}")),
        },
        Request::Input(text) => match link.as_ref() {
            Some(link) => {
                let sent = input(link, &link.run_id, &text);
                for line in sent.said {
                    app.say(line);
                }
                if let Some(command) = sent.awaiting {
                    app.awaiting(&command);
                }
            }
            None => app.say("输入未发送：没有附着的 Run".to_owned()),
        },
    }
}

/// The same SetMode command for Slash and Shift+Tab, refreshed on a revision conflict.
fn set_mode(app: &mut App, link: Option<&Link>, mode: &str) {
    let Some(link) = link else {
        app.say("模式未切换：没有附着的 Run".to_owned());
        return;
    };
    let result = link.run().and_then(|run| {
        let run = run.ok_or_else(|| Failure::unreachable("Run 不存在"))?;
        crate::client::fresh(
            |command| link.submit(command), || link.run(), &link.run_id,
            run["revision"].as_i64().unwrap_or_default(),
            |revision| json!({"expected_revision":revision,"kind":"SetMode","run_id":link.run_id,"payload":{"mode":mode}}),
        )
    });
    match result {
        Ok(_) => {
            app.mode_requested(mode);
            if let Ok(Some(run)) = link.run() {
                app.projection(&run);
            }
            app.say(format!("SetMode {mode} 已受理"));
        }
        Err(f) => app.say(format!("模式未切换：{}：{}", f.code, f.message)),
    }
}

/// Attach the shell to run `run_id` (getRun for its label) and read the runs again for the top bar's baseline;
/// why not, said.
fn attach_to(
    app: &mut App,
    conn: &Conn,
    link: &mut Option<Link>,
    run_id: &str,
) -> Result<(), String> {
    let reply = conn.get(&format!("/v1/runs/{}", crate::client::encode(run_id)))?;
    if reply.status != 200 {
        return Err(format!("host 答 {}", reply.status));
    }
    let run = reply.json();
    let label = run["projection"]["label"]
        .as_str()
        .or(run["state"].as_str())
        .unwrap_or_default()
        .to_owned();
    *link = Some(conn.link(run_id));
    app.attach(run_id, &label);
    app.projection(&run);
    app.tracking(&run["projection"]["degraded_tracking"]);
    if let Ok(runs) = conn.runs() {
        app.list(&runs);
    }
    Ok(())
}

fn event_loop(
    app: &mut App,
    signal: &AtomicUsize,
    resized: &AtomicBool,
    conn: &Conn,
    link: &mut Option<Link>,
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
        if let Some(link) = link.as_ref()
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
                if let Some(link) = link.as_ref() {
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
        // What Enter and the selector asked of the host, in order.
        for request in app.take_requests() {
            perform(app, conn, link, request);
            dirty = true;
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

/// Debug builds only: `HOH_DEBUG_TUI_SEQS=<path>` appends the seq of every event a link's reader receives to that file,
/// one per line in the order received, so the PTY test sees the sequence attach received (TEST-005-01, docs/adr/
/// ADR-031.md S5). It writes that file only, never the project's store (E11). Release builds have no such trace.
fn received_trace() -> Option<std::fs::File> {
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("HOH_DEBUG_TUI_SEQS") {
        return std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok();
    }
    None
}

/// Debug builds only: `HOH_DEBUG_TUI_PANIC` panics after a draw, so the PTY test can see the terminal restored
/// before the message (ADR-014 decision 8). Release builds have no such entry.
fn debug_panic() {
    #[cfg(debug_assertions)]
    if std::env::var_os("HOH_DEBUG_TUI_PANIC").is_some() {
        panic!("HOH_DEBUG_TUI_PANIC: a panic asked for by the PTY test");
    }
}
