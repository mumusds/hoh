//! The Workspace shell's state, keys and screen (ADR-014; docs/adr/ADR-031.md decision 13). It is opened with the
//! project's host connected: the top bar shows the project, the mode, the run attached (none at first) and the
//! project's baseline, and the body lists the runs. Enter records the line in the project's history; unattached, text
//! starting with `/` is the Workspace's own (`/resume` opens the selector of the runs not ended, the one `hoh attach`
//! opens; mode commands choose the next run's mode) and other text asks for a new run, which the shell then attaches; attached,
//! Enter sends plain text as input; mode commands and Shift+Tab ask SetMode (ADR-036). What is asked of the host is queued as a [`Request`], which the
//! loop does in order. The Ctrl+C choice offers only "脱离" with no run attached; attached (docs/adr/ADR-021.md decision
//! 10) the run's events are shown and counted on the status bar, and the choice also offers "暂停" and "取消任务".

use super::editor::{Editor, cells};
use super::history::History;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use serde_json::Value;
use unicode_segmentation::UnicodeSegmentation;

/// What the user chose in the Ctrl+C choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// Close the client; accepted runs go on, nothing is sent.
    Detach,
    /// Pause the attached run (PauseRun) and stay.
    Pause,
    /// Cancel the attached run (CancelRun) and stay.
    Cancel,
}

/// What the user asked of the host, done by the loop in the order asked (docs/adr/ADR-031.md decision 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// StartRun with this goal (Enter, unattached), then attach the new run.
    Start { goal: String, mode: String },
    /// Change the attached run through SetMode.
    SetMode(String),
    /// Cycle when performed, so consecutive keys observe the preceding request.
    CycleMode,
    /// SendInput with this text to the attached run (Enter, attached), or to the run a `Start` before it starts.
    Input(String),
    /// STEER to an explicitly resolved role; the host owns its binding and delivery epoch.
    InputTo { role: String, text: String },
    /// Read the latest team and running task roles before resolving a target.
    ResolveTarget {
        name: String,
        text: String,
        draft: String,
        role_only: bool,
    },
    /// Attach this run (chosen in the selector).
    Attach(String),
}

/// A run as list_runs gives it (docs/adr/ADR-031.md decision 13).
#[derive(Debug, Clone)]
struct Listed {
    run_id: String,
    state: String,
    label: String,
    blocked_reason: Option<String>,
    goal: Option<String>,
    created_at: String,
    /// A COMPLETED run's candidate accepted last: its commit and the seq it was accepted at.
    accepted: Option<(String, i64)>,
}

impl Listed {
    fn of(item: &Value) -> Listed {
        let text = |key: &str| item[key].as_str().map(str::to_owned);
        let state = text("state").unwrap_or_default();
        Listed {
            run_id: text("run_id").unwrap_or_default(),
            label: text("label").unwrap_or_else(|| state.clone()),
            state,
            blocked_reason: text("blocked_reason"),
            goal: text("goal"),
            created_at: text("created_at").unwrap_or_default(),
            accepted: item["accepted"]["git_commit"]
                .as_str()
                .zip(item["accepted"]["seq"].as_i64())
                .map(|(commit, seq)| (commit.to_owned(), seq)),
        }
    }

    /// In a terminal state: not attachable, listed after the others.
    fn ended(&self) -> bool {
        crate::follow::TERMINAL.contains(&self.state.as_str())
    }

    /// `<short id>  <label>  <blocked reason?>  <goal>` on one line of at most `width` cells: the short id is the run
    /// id's first 8 characters, a goal of several lines is joined, and what does not fit is cut with "…".
    fn line(&self, width: usize) -> String {
        let mut line: String = self.run_id.chars().take(8).collect();
        for part in [
            Some(&self.label),
            self.blocked_reason.as_ref(),
            self.goal.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|p| !p.trim().is_empty())
        {
            line.push_str("  ");
            line.extend(part.chars().map(|c| if c.is_control() { ' ' } else { c }));
        }
        cut(&line, width)
    }
}

/// `text` within `width` cells: whole graphemes, then "…" when some had to go.
fn cut(text: &str, width: usize) -> String {
    if text.graphemes(true).map(cells).sum::<usize>() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for g in text.graphemes(true) {
        if used + cells(g) + 1 > width {
            break;
        }
        used += cells(g);
        out.push_str(g);
    }
    if width > 0 {
        out.push('…');
    }
    out
}

/// The selector of the runs not ended (`/resume`, and `hoh attach` with several on a terminal).
#[derive(Debug)]
struct Picker {
    runs: Vec<Listed>,
    selected: usize,
    /// Esc leaves the shell (attach's selector) instead of closing the selector (`/resume`'s).
    leaves: bool,
}

const COMMANDS: &[&str] = &[
    "/plan", "/build", "/auto", "/team", "/model", "/status", "/resume", "/help",
];

#[derive(Debug)]
struct TargetPicker {
    roles: Vec<String>,
    selected: usize,
    text: String,
    draft: String,
}

/// The Ctrl+C choice with no run attached.
const CHOICES: &[(&str, Exit)] = &[("脱离", Exit::Detach)];

/// The Ctrl+C choice of a client attached to a run (REQ-043 behaviour 6).
const RUN_CHOICES: &[(&str, Exit)] = &[
    ("脱离", Exit::Detach),
    ("暂停", Exit::Pause),
    ("取消任务", Exit::Cancel),
];

/// The run a client is attached to, with how many of its events it received and the latest one's seq, and its attempts
/// tracked by the degraded /proc scan (docs/adr/ADR-021.md decision 8d, M2T06R-560).
#[derive(Debug)]
struct Attached {
    snapshot: Value,
    mode: String,
    pending_mode: Option<String>,
    run_id: String,
    label: String,
    events: u64,
    last: i64,
    degraded: std::collections::BTreeSet<String>,
}

/// The built-in default mode (REQ-001; docs/adr/ADR-022.md decision 6).
const DEFAULT_MODE: &str = "AUTO";

/// The mode the top bar shows for `project`: its configuration's `default_mode`, or the built-in one when it has no
/// configuration file or an invalid one (ADR-014; docs/adr/ADR-022.md decision 6, M2T07R-355).
pub fn mode_of(project: &std::path::Path) -> String {
    hoh_core::config::load(project)
        .map_or_else(|_| DEFAULT_MODE.to_owned(), |c| c.project.default_mode)
}

#[derive(Debug)]
struct Search {
    query: String,
    found: Option<usize>,
    /// The editor's text before the search, given back by Esc.
    draft: String,
}

#[derive(Debug)]
pub struct App {
    root: Option<std::path::PathBuf>,
    settings: Option<super::settings::Settings>,
    project: String,
    mode: String,
    mode_cycle: Vec<String>,
    editor: Editor,
    history: History,
    /// The history entry shown by Up/Down, with the draft from before browsing.
    browsing: Option<(usize, String)>,
    search: Option<Search>,
    /// The Ctrl+C choice is open, with this item selected.
    menu: Option<usize>,
    messages: Vec<String>,
    run: Option<Attached>,
    /// The project's runs, those not ended first and newest first (list_runs, every page).
    runs: Vec<Listed>,
    picker: Option<Picker>,
    target_picker: Option<TargetPicker>,
    slash: Option<usize>,
    requests: Vec<Request>,
    /// The commands of inputs sent whose acceptance event has not come yet.
    awaiting: Vec<String>,
}

impl App {
    pub fn new(project: String, history: History) -> App {
        let messages = history.notice().map(str::to_owned).into_iter().collect();
        App {
            root: None,
            settings: None,
            project,
            mode: DEFAULT_MODE.to_owned(),
            mode_cycle: hoh_core::config::Config::default().ui.mode_cycle,
            editor: Editor::default(),
            history,
            browsing: None,
            search: None,
            menu: None,
            messages,
            run: None,
            runs: Vec::new(),
            picker: None,
            target_picker: None,
            slash: None,
            requests: Vec::new(),
            awaiting: Vec::new(),
        }
    }

    /// The shell of `project`: its name, its input history and its configured mode (see [`mode_of`]).
    pub fn open(project: &std::path::Path) -> App {
        let name = project.file_name().map_or_else(
            || project.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let mut app = App::new(name, History::load(project));
        app.mode = mode_of(project);
        app.root = Some(project.to_path_buf());
        if let Ok(config) = hoh_core::config::load(project) {
            app.mode_cycle = config.ui.mode_cycle;
        }
        app
    }

    /// The mode new runs start in: the one the top bar shows.
    pub fn mode(&self) -> &str {
        self.run.as_ref().map_or(self.mode.as_str(), |r| &r.mode)
    }

    pub fn projection(&mut self, value: &Value) {
        if let Some(run) = self.run.as_mut() {
            run.snapshot = value.clone();
            if let Some(mode) = value["projection"]["mode"].as_str() {
                run.mode = mode.to_owned();
                if run.pending_mode.as_deref() == Some(mode) {
                    run.pending_mode = None;
                }
            }
        }
    }

    pub fn mode_requested(&mut self, mode: &str) {
        if let Some(run) = self.run.as_mut() {
            run.pending_mode = Some(mode.to_owned());
        }
    }

    pub fn next_mode(&self) -> String {
        let current = self
            .run
            .as_ref()
            .and_then(|r| r.pending_mode.as_deref())
            .unwrap_or(self.mode());
        let next = self
            .mode_cycle
            .iter()
            .position(|m| m == current)
            .map_or(0, |i| (i + 1) % self.mode_cycle.len());
        self.mode_cycle[next].clone()
    }

    fn has_run(&self) -> bool {
        self.run.is_some()
            || self
                .requests
                .iter()
                .any(|r| matches!(r, Request::Start { .. }))
    }

    fn choose_mode(&mut self, mode: &str) {
        if self.has_run() {
            self.requests.push(Request::SetMode(mode.to_owned()));
        } else {
            self.mode = mode.to_owned();
        }
    }

    /// Bind the shell to run `run_id`, shown as `label`.
    pub fn attach(&mut self, run_id: &str, label: &str) {
        self.run = Some(Attached {
            snapshot: Value::Null,
            mode: self.mode.clone(),
            pending_mode: None,
            run_id: run_id.to_owned(),
            label: label.to_owned(),
            events: 0,
            last: 0,
            degraded: std::collections::BTreeSet::new(),
        });
        self.messages.clear();
        self.picker = None;
        self.target_picker = None;
    }

    /// The project's runs as list_runs gives them (every page): listed those not ended first, each group newest first.
    pub fn list(&mut self, items: &[Value]) {
        let mut runs: Vec<Listed> = items.iter().map(Listed::of).collect();
        runs.sort_by(|a, b| {
            a.ended()
                .cmp(&b.ended())
                .then_with(|| b.created_at.cmp(&a.created_at))
        });
        self.runs = runs;
    }

    /// The project's baseline (docs/adr/ADR-031.md decision 13): the first 12 hex digits of the commit of the candidate
    /// accepted last among the COMPLETED runs' ACCEPTED candidates, or "无".
    fn baseline(&self) -> String {
        self.runs
            .iter()
            .filter(|r| r.state == "COMPLETED")
            .filter_map(|r| r.accepted.as_ref())
            .max_by_key(|(_, seq)| *seq)
            .map_or_else(
                || "无".to_owned(),
                |(commit, _)| commit.chars().take(12).collect(),
            )
    }

    /// Open the selector of the runs not ended; false when there is none.
    fn open_picker(&mut self, leaves: bool) -> bool {
        let runs: Vec<Listed> = self.runs.iter().filter(|r| !r.ended()).cloned().collect();
        if runs.is_empty() {
            return false;
        }
        self.picker = Some(Picker {
            runs,
            selected: 0,
            leaves,
        });
        true
    }

    /// Open attach's selector (docs/adr/ADR-031.md decision 12): Esc leaves the shell.
    pub fn pick(&mut self) {
        if !self.open_picker(true) {
            self.messages.push("没有可附着的 Run。".to_owned());
        }
    }

    /// What was asked of the host since the last call, in order.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// The acceptance event of input command `command_id` is to be said when it comes.
    pub fn awaiting(&mut self, command_id: &str) {
        self.awaiting.push(command_id.to_owned());
    }

    /// An event of the attached run: shown and counted once (its seq past the latest one's); the acceptance of an
    /// input sent says whether it answered a question. True for a new `run.*` event (the run's label may change).
    pub fn event(&mut self, event: &Value) -> bool {
        let Some(run) = self.run.as_mut() else {
            return false;
        };
        let Some(seq) = event["seq"].as_i64().filter(|seq| *seq > run.last) else {
            return false;
        };
        run.last = seq;
        run.events += 1;
        let kind = event["kind"].as_str().unwrap_or_default();
        let payload = &event["payload"];
        if let Some(mode) = payload["to"].as_str() {
            if kind == "run.mode_changed" {
                run.mode = mode.to_owned();
                run.pending_mode = None;
            } else if kind == "run.mode_change_pending" {
                run.pending_mode = Some(mode.to_owned());
                self.messages
                    .push(format!("模式 {mode} 待当前写任务结束后生效"));
            }
        }
        match payload["attempt_id"].as_str() {
            // An attempt tracked by the degraded /proc scan: said with which and why, and counted on the status bar.
            Some(attempt) if kind == "attempt.tracking" && payload["tracking"] == "proc" => {
                run.degraded.insert(attempt.to_owned());
                self.messages.push(format!(
                    "#{seq} {kind}：{attempt} 的进程跟踪降级为 proc（{}）",
                    payload["reason"].as_str().unwrap_or("原因未记录")
                ));
            }
            _ => self.messages.push(format!("#{seq} {kind}")),
        }
        if kind == "input.applied"
            && let Some(answer) = payload["answer"].as_str()
        {
            self.messages.push(format!("Commander：{answer}"));
        }
        if kind == "input.rejected"
            && let Some(reason) = payload["reason"].as_str()
        {
            self.messages.push(format!("输入未应用：{reason}"));
        }
        if kind != "run.result"
            && let Some(command) = event["payload"]["command_id"].as_str()
            && let Some(at) = self.awaiting.iter().position(|c| c == command)
        {
            self.awaiting.remove(at);
            self.messages
                .push(match event["payload"]["question_id"].as_str() {
                    Some(question) => format!("已答复 {question}"),
                    None => "已排队".to_owned(),
                });
        }
        kind.starts_with("run.")
            || kind.starts_with("team.")
            || matches!(kind, "attempt.moved" | "attempt.ended")
    }

    /// The attached run's attempts tracked by the degraded /proc scan, as getRun's `projection.degraded_tracking`
    /// lists them (with those its events said since).
    pub fn tracking(&mut self, degraded: &Value) {
        if let Some(run) = self.run.as_mut() {
            run.degraded.extend(
                degraded
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|d| d["attempt_id"].as_str().map(str::to_owned)),
            );
        }
    }

    /// The attached run's label changed.
    pub fn relabel(&mut self, label: &str) {
        if let Some(run) = self.run.as_mut() {
            run.label = label.to_owned();
        }
    }

    /// A line in the message area (an event of the run, a command's answer).
    pub fn say(&mut self, line: String) {
        self.messages.push(line);
    }

    fn choices(&self) -> &'static [(&'static str, Exit)] {
        if self.run.is_some() {
            RUN_CHOICES
        } else {
            CHOICES
        }
    }

    #[cfg(test)]
    pub fn editor_text(&self) -> &str {
        self.editor.text()
    }

    /// Handle one key; `Some` when the user leaves.
    pub fn key(&mut self, key: KeyEvent) -> Option<Exit> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(selected) = self.menu {
            let choices = self.choices();
            match key.code {
                KeyCode::Enter => {
                    self.menu = None;
                    return choices.get(selected).map(|(_, exit)| *exit);
                }
                KeyCode::Up => self.menu = Some(selected.saturating_sub(1)),
                KeyCode::Down => self.menu = Some((selected + 1).min(choices.len() - 1)),
                KeyCode::Esc => self.menu = None,
                KeyCode::Char('c') if ctrl => self.menu = None,
                _ => {}
            }
            return None;
        }
        if let Some(settings) = self.settings.as_mut() {
            match key.code {
                KeyCode::Esc => self.settings = None,
                KeyCode::Char('c') if ctrl => self.menu = Some(0),
                KeyCode::Char('r') => {
                    if let Some(root) = &self.root {
                        settings.restore(root);
                    }
                }
                KeyCode::Down => settings.scroll(1),
                KeyCode::Up => settings.scroll(-1),
                KeyCode::PageDown => settings.scroll(20),
                KeyCode::PageUp => settings.scroll(-20),
                _ => (),
            }
            return None;
        }
        if self.picker.is_some() {
            return self.picker_key(key);
        }
        if self.target_picker.is_some() {
            self.target_key(key);
            return None;
        }
        if let Some(selected) = self.slash {
            match key.code {
                KeyCode::Up => {
                    self.slash = Some(selected.saturating_sub(1));
                    return None;
                }
                KeyCode::Down => {
                    self.slash =
                        Some((selected + 1).min(self.slash_items().len().saturating_sub(1)));
                    return None;
                }
                KeyCode::Esc => {
                    self.slash = None;
                    return None;
                }
                KeyCode::Enter => {
                    if let Some(command) = self.slash_items().get(selected) {
                        self.editor.set(command);
                    }
                }
                _ => {}
            }
        }
        if self.search.is_some() && self.search_key(key) {
            return None;
        }
        let plain = !ctrl && !key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.editor.is_empty() {
                    self.menu = Some(0);
                } else {
                    self.editor.clear();
                    self.browsing = None;
                    self.slash = None;
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.slash = None;
                self.search = Some(Search {
                    query: String::new(),
                    found: None,
                    draft: self.editor.text().to_owned(),
                });
            }
            KeyCode::Char(c) if plain => {
                self.editor.insert(c.encode_utf8(&mut [0; 4]));
                self.browsing = None;
                self.update_slash();
            }
            KeyCode::BackTab => {
                if self.has_run() {
                    self.requests.push(Request::CycleMode);
                } else {
                    self.mode = self.next_mode();
                }
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => {
                self.editor.backspace();
                self.update_slash();
            }
            KeyCode::Delete => {
                self.editor.delete();
                self.update_slash();
            }
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Up => self.older(),
            KeyCode::Down => self.newer(),
            _ => {}
        }
        None
    }

    /// A key while the selector is open: Up and Down choose, Enter attaches the run chosen, Esc closes the selector (or
    /// leaves, attach's), Ctrl+C opens the choice to leave.
    fn picker_key(&mut self, key: KeyEvent) -> Option<Exit> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let picker = self.picker.as_mut()?;
        match key.code {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(picker.runs.len().saturating_sub(1));
            }
            KeyCode::Enter => {
                let run = picker.runs.get(picker.selected).map(|r| r.run_id.clone());
                self.picker = None;
                if let Some(run) = run {
                    self.requests.push(Request::Attach(run));
                }
            }
            KeyCode::Esc if picker.leaves => return Some(Exit::Detach),
            KeyCode::Esc => self.picker = None,
            KeyCode::Char('c') if ctrl => self.menu = Some(0),
            _ => {}
        }
        None
    }

    /// A key during Ctrl+R; `false` if it ended the search and is to be handled as a normal key.
    fn search_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        let total = self.history.entries().len();
        match key.code {
            KeyCode::Char('r') if ctrl => {
                let before = search.found.unwrap_or(total);
                if let Some(i) = self.history.search(&search.query, before) {
                    search.found = Some(i);
                }
            }
            KeyCode::Esc => {
                let draft = std::mem::take(&mut search.draft);
                self.editor.set(&draft);
                self.search = None;
                return true;
            }
            KeyCode::Char('c' | 'g') if ctrl => {
                let draft = std::mem::take(&mut search.draft);
                self.editor.set(&draft);
                self.search = None;
                return true;
            }
            KeyCode::Enter => {
                self.search = None;
                return true;
            }
            KeyCode::Backspace => {
                search.query.pop();
                search.found = self
                    .history
                    .search(&search.query, Self::from(search.found, total));
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                search.query.push(c);
                search.found = self
                    .history
                    .search(&search.query, Self::from(search.found, total));
            }
            _ => {
                self.search = None;
                return false;
            }
        }
        if let Some(text) = search.found.and_then(|i| self.history.entries().get(i)) {
            self.editor.set(text);
        }
        true
    }

    /// Where a changed query searches from: the current match itself if it still matches, else older ones.
    fn from(found: Option<usize>, total: usize) -> usize {
        found.map_or(total, |i| i + 1)
    }

    pub fn paste(&mut self, text: &str) {
        if self.menu.is_some()
            || self.picker.is_some()
            || self.target_picker.is_some()
            || self.settings.is_some()
        {
            return;
        }
        self.search = None;
        self.browsing = None;
        self.editor.insert(text);
        self.update_slash();
    }

    fn submit(&mut self) {
        if self.editor.text().trim().is_empty() {
            return;
        }
        let text = self.editor.take();
        self.slash = None;
        self.browsing = None;
        match self.history.append(&text) {
            Err(e) => self.messages.push(format!("输入历史未能保存：{e}")),
            Ok(Some(diagnostic)) => self
                .messages
                .push(format!("项目状态目录的权限未能收紧：{diagnostic}")),
            Ok(None) => {}
        }
        self.messages.push(format!("你：{text}"));
        let mut words = text.split_whitespace();
        if words.next() == Some("/settings") {
            let role = words.next().unwrap_or("developer");
            if let Some(root) = &self.root {
                match super::settings::Settings::open(
                    root,
                    self.run.as_ref().map_or("settings", |r| r.run_id.as_str()),
                    role,
                ) {
                    Ok(settings) => self.settings = Some(settings),
                    Err(e) => self.messages.push(format!("设置无法打开：{e}")),
                }
            }
            return;
        }
        let command = text.trim();
        match command {
            "/plan" => self.choose_mode("PLAN"),
            "/build" => self.choose_mode("BUILD"),
            "/auto" => self.choose_mode("AUTO"),
            "/resume" => {
                if !self.open_picker(false) {
                    self.messages.push("没有可附着的 Run。".to_owned());
                }
            }
            "/status" | "/help" | "/team" | "/model" => self.information(command),
            _ if command.split_whitespace().next() == Some("/agent") => {
                let rest = command.strip_prefix("/agent").unwrap_or_default().trim();
                let (role, body) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                self.requests.push(Request::ResolveTarget {
                    name: role.into(),
                    text: body.trim().into(),
                    draft: text.clone(),
                    role_only: true,
                });
            }
            _ if command.starts_with('/') => self
                .messages
                .push(format!("未知命令（unknown command）：{command}，未发送。")),
            _ if command.starts_with('@') => {
                let (name, body) = command[1..]
                    .split_once(char::is_whitespace)
                    .unwrap_or((&command[1..], ""));
                self.requests.push(Request::ResolveTarget {
                    name: name.into(),
                    text: body.trim().into(),
                    draft: text.clone(),
                    role_only: false,
                });
            }
            _ if self.has_run() => self.requests.push(Request::Input(text)),
            _ => self.requests.push(Request::Start {
                goal: text,
                mode: self.mode.clone(),
            }),
        }
    }

    fn slash_items(&self) -> Vec<&'static str> {
        let Some(query) = self.editor.text().strip_prefix('/') else {
            return Vec::new();
        };
        if query.chars().any(char::is_whitespace) {
            return Vec::new();
        }
        COMMANDS
            .iter()
            .copied()
            .filter(|command| {
                let mut chars = command[1..].chars();
                query.chars().all(|wanted| chars.any(|c| c == wanted))
            })
            .collect()
    }

    fn update_slash(&mut self) {
        self.slash = (!self.slash_items().is_empty()).then_some(0);
    }

    pub fn target(&mut self, name: &str, text: &str, draft: &str, role_only: bool) {
        let Some(run) = &self.run else {
            self.say("输入未发送：没有附着的 Run".into());
            return;
        };
        if name.is_empty() || text.is_empty() {
            self.say("输入未发送：请指定目标和正文（@role 正文 或 /agent <role> <text>）".into());
            return;
        }
        let members: Vec<_> = run.snapshot["projection"]["team"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|m| m["removed"] != true && m["agent"].is_string())
            .collect();
        let explicit = members.iter().find(|m| m["role"] == name);
        let mut roles: Vec<String> = if explicit.is_some() {
            vec![name.to_owned()]
        } else if role_only {
            Vec::new()
        } else {
            members
                .iter()
                .filter(|m| m["agent"] == name)
                .filter_map(|m| m["role"].as_str().map(str::to_owned))
                .collect()
        };
        roles.sort();
        roles.dedup();
        if roles.len() > 1 {
            let active: Vec<_> = roles
                .iter()
                .filter(|r| {
                    run.snapshot["projection"]["active_roles"]
                        .as_array()
                        .is_some_and(|active| active.iter().any(|a| a.as_str() == Some(r.as_str())))
                })
                .cloned()
                .collect();
            if active.len() == 1 {
                roles = active;
            }
        }
        match roles.as_slice() {
            [] => self.say(format!("输入未发送：未知目标 {name}")),
            [role] => self.send_to(role.clone(), text.to_owned()),
            _ => {
                self.target_picker = Some(TargetPicker {
                    roles,
                    selected: 0,
                    text: text.to_owned(),
                    draft: draft.to_owned(),
                })
            }
        }
    }

    fn send_to(&mut self, role: String, text: String) {
        self.say(format!("→ {role}：{text}"));
        self.requests.push(Request::InputTo { role, text });
    }

    fn target_key(&mut self, key: KeyEvent) {
        let Some(picker) = self.target_picker.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                picker.selected = (picker.selected + 1).min(picker.roles.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(picker) = self.target_picker.take()
                    && let Some(role) = picker.roles.get(picker.selected)
                {
                    self.send_to(role.clone(), picker.text);
                }
            }
            KeyCode::Esc => {
                let draft = picker.draft.clone();
                self.target_picker = None;
                self.editor.set(&draft);
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.target_picker = None;
                self.editor.clear();
            }
            _ => {}
        }
    }

    fn information(&mut self, command: &str) {
        if command == "/help" {
            self.say("键位：Enter 发送 · Shift+Tab 切换模式 · ↑↓ 选择/历史 · Esc 返回 · Ctrl+R 搜索 · Ctrl+C 清空/退出".into());
            self.say(format!(
                "命令：{}；/agent <role> <text> 显式转发；@role 正文指定职位",
                COMMANDS.join(" ")
            ));
            return;
        }
        let Some(run) = &self.run else {
            self.say(format!(
                "当前没有附着的 Run；下次启动模式 {}。",
                self.mode()
            ));
            return;
        };
        if command == "/status" {
            self.say(format!(
                "Run {} · 状态 {} · 模式 {} · revision {}",
                run.run_id,
                run.snapshot["state"].as_str().unwrap_or(&run.label),
                run.mode,
                run.snapshot["revision"]
            ));
        } else {
            let lines: Vec<_> = run.snapshot["projection"]["team"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| {
                    format!(
                        "{} → {} · model {}{}",
                        m["role"].as_str().unwrap_or("?"),
                        m["agent"].as_str().unwrap_or("未绑定"),
                        m["model"].as_str().unwrap_or("底座默认"),
                        if m["removed"] == true {
                            "（已移除）"
                        } else {
                            ""
                        }
                    )
                })
                .collect();
            self.messages.extend(lines);
            self.say("以上为当前 Run 的职位绑定与模型；项目团队/模型设置在 checkpoint 生效。TUI 编辑属后续任务。".into());
        }
    }

    fn older(&mut self) {
        let entries = self.history.entries();
        let index = match &self.browsing {
            Some((i, _)) => i.saturating_sub(1),
            None if !entries.is_empty() => entries.len() - 1,
            None => return,
        };
        let draft = match self.browsing.take() {
            Some((_, draft)) => draft,
            None => self.editor.text().to_owned(),
        };
        if let Some(text) = entries.get(index) {
            self.editor.set(text);
        }
        self.browsing = Some((index, draft));
    }

    fn newer(&mut self) {
        let Some((i, draft)) = self.browsing.take() else {
            return;
        };
        match self.history.entries().get(i + 1) {
            Some(text) => {
                self.editor.set(text);
                self.browsing = Some((i + 1, draft));
            }
            None => self.editor.set(&draft),
        }
    }

    pub fn draw(&self, frame: &mut Frame) {
        let [top, body, input, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(frame.area());
        let bar = Style::new().add_modifier(Modifier::REVERSED);
        let run = match &self.run {
            Some(run) => format!("run {} {}", run.run_id, run.label),
            None => "run 无".to_owned(),
        };
        frame.render_widget(
            Paragraph::new(format!(
                " HoH │ {} │ 模式 {} │ {run} │ baseline {} │ host 已连接 ",
                self.project,
                self.mode(),
                self.baseline()
            ))
            .style(bar),
            top,
        );
        // Unattached, the runs come first, then the latest messages that fit.
        let mut lines: Vec<Line> = Vec::new();
        if self.run.is_none() {
            if self.runs.is_empty() {
                lines.push(Line::from("还没有 Run：输入目标并回车新建一个。"));
            }
            let width = usize::from(body.width);
            lines.extend(
                self.runs
                    .iter()
                    .take(usize::from(body.height))
                    .map(|r| Line::from(r.line(width))),
            );
        }
        let room = usize::from(body.height).saturating_sub(lines.len());
        let shown = room.min(self.messages.len());
        lines.extend(
            self.messages[self.messages.len() - shown..]
                .iter()
                .map(|m| Line::from(m.as_str())),
        );
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
        let prompt = "> ";
        let room = usize::from(input.width).saturating_sub(prompt.len());
        let (visible, column) = self.editor.view(room);
        // Two spans: ratatui splits each into graphemes on its own, so the text's first character never joins the
        // prompt's space (a halfwidth sound mark would, three cells wide; M1T08R-38).
        frame.render_widget(
            Paragraph::new(Line::from(vec![Span::raw(prompt), Span::raw(visible)])),
            input,
        );
        let line = match &self.search {
            Some(s) => format!(
                " 反向搜索：{}{} │ Enter 选定 · Esc 取消 · Ctrl+R 更早",
                s.query,
                if s.found.is_none() && !s.query.is_empty() {
                    "（无匹配）"
                } else {
                    ""
                }
            ),
            None => match (&self.picker, &self.run) {
                (Some(picker), _) => format!(
                    " 选择 Run │ ↑↓ 选择 · Enter 附着 · Esc {}",
                    if picker.leaves { "脱离" } else { "返回" }
                ),
                (None, Some(run)) => format!(
                    " {} │ 事件 {} · #{}{} │ Enter 发送 · ↑↓ 历史 · Ctrl+R 搜索 · Ctrl+C 清空/退出",
                    self.mode(),
                    run.events,
                    run.last,
                    if run.degraded.is_empty() {
                        String::new()
                    } else {
                        format!(" · 跟踪降级 {}", run.degraded.len())
                    }
                ),
                (None, None) => format!(
                    " {} │ host 已连接 │ Enter 发送 · ↑↓ 历史 · Ctrl+R 搜索 · Ctrl+C 清空/退出",
                    self.mode()
                ),
            },
        };
        frame.render_widget(Paragraph::new(line).style(bar), status);
        if let Some(picker) = &self.picker {
            Self::draw_picker(frame, body, picker);
        }
        if let Some(picker) = &self.target_picker {
            Self::draw_options(
                frame,
                body,
                " 选择目标职位 · ↑↓ 选择 · Enter 发送 · Esc 返回 ",
                &picker.roles,
                picker.selected,
            );
        } else if let Some(selected) = self.slash {
            Self::draw_options(
                frame,
                body,
                " 命令 · 输入筛选 · ↑↓ 选择 · Enter 执行 · Esc 返回 ",
                &self.slash_items(),
                selected,
            );
        }
        if let Some(settings) = &self.settings {
            settings.draw(frame, body);
        }
        match self.menu {
            Some(selected) => self.draw_menu(frame, body, selected),
            None if self.picker.is_some() || self.target_picker.is_some() => {}
            None => {
                let x = input
                    .x
                    .saturating_add(u16::try_from(prompt.len() + column).unwrap_or(u16::MAX));
                frame.set_cursor_position((x.min(input.right().saturating_sub(1)), input.y));
            }
        }
    }

    fn draw_options(
        frame: &mut Frame,
        body: Rect,
        title: &str,
        options: &[impl AsRef<str>],
        selected: usize,
    ) {
        let block = Block::new().borders(Borders::ALL).title(title);
        let inner = block.inner(body);
        let first = selected.saturating_sub(usize::from(inner.height).saturating_sub(1));
        let lines: Vec<Line> = options
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(inner.height))
            .map(|(i, option)| {
                Line::from(format!(
                    "{} {}",
                    if i == selected { ">" } else { " " },
                    option.as_ref()
                ))
            })
            .collect();
        frame.render_widget(Clear, body);
        frame.render_widget(Paragraph::new(lines).block(block), body);
        frame.set_cursor_position((
            inner.x,
            inner
                .y
                .saturating_add(u16::try_from(selected - first).unwrap_or(0))
                .min(body.bottom().saturating_sub(1)),
        ));
    }

    /// The selector over the whole body, the cursor on the run chosen (every frame sets one, M1T08R-10).
    fn draw_picker(frame: &mut Frame, body: Rect, picker: &Picker) {
        let block = Block::new().borders(Borders::ALL).title(if picker.leaves {
            " 选择 Run · Enter 附着 · Esc 脱离 "
        } else {
            " 选择 Run · Enter 附着 · Esc 返回 "
        });
        let inner = block.inner(body);
        let (height, width) = (
            usize::from(inner.height).max(1),
            usize::from(inner.width).saturating_sub(2),
        );
        let first = picker.selected.saturating_sub(height - 1);
        let items: Vec<Line> = picker
            .runs
            .iter()
            .enumerate()
            .skip(first)
            .take(height)
            .map(|(i, r)| {
                Line::from(format!(
                    "{} {}",
                    if i == picker.selected { ">" } else { " " },
                    r.line(width)
                ))
            })
            .collect();
        frame.render_widget(Clear, body);
        frame.render_widget(Paragraph::new(items).block(block), body);
        let row = u16::try_from(picker.selected - first).unwrap_or(0);
        frame.set_cursor_position((
            inner.x,
            inner
                .y
                .saturating_add(row)
                .min(body.bottom().saturating_sub(1)),
        ));
    }

    fn draw_menu(&self, frame: &mut Frame, body: Rect, selected: usize) {
        let choices = self.choices();
        let height = u16::try_from(choices.len()).unwrap_or(1).saturating_add(2);
        let area = Rect {
            x: body.x + body.width.saturating_sub(24) / 2,
            y: body.y + body.height.saturating_sub(height) / 2,
            width: body.width.min(24),
            height: body.height.min(height),
        };
        let items: Vec<Line> = choices
            .iter()
            .enumerate()
            .map(|(i, (label, _))| {
                Line::from(format!("{} {label}", if i == selected { ">" } else { " " }))
            })
            .collect();
        // The cursor on the selected choice: every frame sets one, so ratatui never hides the cursor (M1T08R-10).
        let row = u16::try_from(selected).unwrap_or(0).saturating_add(1);
        frame.set_cursor_position((
            area.x.saturating_add(1),
            area.y
                .saturating_add(row)
                .min(area.bottom().saturating_sub(1)),
        ));
        frame.render_widget(Clear, area);
        frame.render_widget(
            Paragraph::new(items).block(
                Block::new()
                    .borders(Borders::ALL)
                    .title(" Enter 选择 · Esc 返回 "),
            ),
            area,
        );
    }
}
