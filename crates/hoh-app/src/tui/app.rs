//! The Workspace shell's state, keys and screen (ADR-014). Bare `hoh` has no host yet (M2-T13), so the top bar says
//! so, Enter only records the line in the project's history (and says it was not sent), and the Ctrl+C choice offers
//! only "脱离": with no run there is nothing to pause or cancel. `hoh attach` binds it to a run (docs/adr/ADR-021.md
//! decision 10): the run and its events are shown, and the Ctrl+C choice also offers "暂停" and "取消任务".

use super::editor::Editor;
use super::history::History;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

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

/// The Ctrl+C choice with no run attached.
const CHOICES: &[(&str, Exit)] = &[("脱离", Exit::Detach)];

/// The Ctrl+C choice of a client attached to a run (REQ-043 behaviour 6).
const RUN_CHOICES: &[(&str, Exit)] = &[
    ("脱离", Exit::Detach),
    ("暂停", Exit::Pause),
    ("取消任务", Exit::Cancel),
];

/// The run a client is attached to.
#[derive(Debug)]
struct Attached {
    run_id: String,
    label: String,
}

/// The built-in default mode; the configured `default_mode` is read once the config parser exists (M2-T07).
const DEFAULT_MODE: &str = "AUTO";

#[derive(Debug)]
struct Search {
    query: String,
    found: Option<usize>,
    /// The editor's text before the search, given back by Esc.
    draft: String,
}

#[derive(Debug)]
pub struct App {
    project: String,
    mode: &'static str,
    editor: Editor,
    history: History,
    /// The history entry shown by Up/Down, with the draft from before browsing.
    browsing: Option<(usize, String)>,
    search: Option<Search>,
    /// The Ctrl+C choice is open, with this item selected.
    menu: Option<usize>,
    messages: Vec<String>,
    run: Option<Attached>,
}

impl App {
    pub fn new(project: String, history: History) -> App {
        let messages = history.notice().map(str::to_owned).into_iter().collect();
        App {
            project,
            mode: DEFAULT_MODE,
            editor: Editor::default(),
            history,
            browsing: None,
            search: None,
            menu: None,
            messages,
            run: None,
        }
    }

    /// Bind the shell to run `run_id`, shown as `label`.
    pub fn attach(&mut self, run_id: &str, label: &str) {
        self.run = Some(Attached {
            run_id: run_id.to_owned(),
            label: label.to_owned(),
        });
        self.messages.clear();
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
                }
            }
            KeyCode::Char('r') if ctrl => {
                self.search = Some(Search {
                    query: String::new(),
                    found: None,
                    draft: self.editor.text().to_owned(),
                });
            }
            KeyCode::Char(c) if plain => {
                self.editor.insert(c.encode_utf8(&mut [0; 4]));
                self.browsing = None;
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
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
        if self.menu.is_some() {
            return;
        }
        self.search = None;
        self.browsing = None;
        self.editor.insert(text);
    }

    fn submit(&mut self) {
        if self.editor.text().trim().is_empty() {
            return;
        }
        let text = self.editor.take();
        self.browsing = None;
        if let Err(e) = self.history.append(&text) {
            self.messages.push(format!("输入历史未能保存：{e}"));
        }
        self.messages.push(format!("你：{text}"));
        self.messages.push(if self.run.is_some() {
            "输入尚未接入（M2-T13），未发送。".to_owned()
        } else {
            "host 离线，未发送。".to_owned()
        });
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
        frame.render_widget(
            Paragraph::new(match &self.run {
                Some(run) => format!(
                    " HoH │ {} │ 模式 {} │ run {} {} │ baseline 未知 │ host 已连接 ",
                    self.project, self.mode, run.run_id, run.label
                ),
                None => format!(
                    " HoH │ {} │ 模式 {} │ run 无 │ baseline 未知 │ host 离线 ",
                    self.project, self.mode
                ),
            })
            .style(bar),
            top,
        );
        let lines: Vec<Line> = if self.messages.is_empty() {
            vec![Line::from(
                "工作区外壳（M1）：host 尚未接入。输入只记入本项目的输入历史，不会发给任何 Agent。",
            )]
        } else {
            let shown = usize::from(body.height).min(self.messages.len());
            self.messages[self.messages.len() - shown..]
                .iter()
                .map(|m| Line::from(m.as_str()))
                .collect()
        };
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
            None => format!(
                " {} │ {} │ Enter 记录 · ↑↓ 历史 · Ctrl+R 搜索 · Ctrl+C 清空/退出",
                self.mode,
                if self.run.is_some() {
                    "host 已连接"
                } else {
                    "host 离线"
                }
            ),
        };
        frame.render_widget(Paragraph::new(line).style(bar), status);
        match self.menu {
            Some(selected) => self.draw_menu(frame, body, selected),
            None => {
                let x = input
                    .x
                    .saturating_add(u16::try_from(prompt.len() + column).unwrap_or(u16::MAX));
                frame.set_cursor_position((x.min(input.right().saturating_sub(1)), input.y));
            }
        }
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
