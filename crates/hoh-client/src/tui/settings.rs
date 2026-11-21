//! Local prompt settings (ADR-034). The host reads overrides again for the next Attempt.
use hoh_core::{config, prompt, prompt_files};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::path::Path;

#[derive(Debug)]
pub(super) struct Settings {
    role: String,
    configured: Option<String>,
    diff: String,
    scroll: u16,
    notice: String,
}

impl Settings {
    pub fn open(root: &Path, run: &str, role: &str) -> Result<Self, prompt::Error> {
        let config = config::load(root).map_err(prompt::unavailable)?;
        let configured = config.team.get(role).and_then(|r| r.prompt.clone());
        let resolved = prompt_files::load(
            root,
            prompt_files::global_root().as_deref(),
            run,
            role,
            configured.as_deref(),
        )?;
        let builtin = resolved
            .chain
            .iter()
            .find(|c| c.layer == "builtin")
            .ok_or_else(|| prompt::unavailable("该职位没有内置默认契约"))?;
        let effective = resolved.effective()?;
        Ok(Self {
            role: role.to_owned(),
            configured,
            diff: prompt::unified_diff(&builtin.text, &effective.text, "builtin", effective.layer),
            scroll: 0,
            notice: "↑↓ / PgUp PgDn 浏览 · r 恢复默认（删除 project 覆盖）· Esc 返回".to_owned(),
        })
    }

    pub fn restore(&mut self, root: &Path) {
        self.notice =
            match prompt_files::restore_project(root, &self.role, self.configured.as_deref()) {
                Ok(_) => "已恢复默认：project 覆盖已删除；下次 Attempt 重新读取覆盖链。Esc 返回"
                    .to_owned(),
                Err(e) => format!("恢复失败：{e}"),
            };
    }

    pub fn scroll(&mut self, amount: i16) {
        self.scroll = self.scroll.saturating_add_signed(amount).min(
            self.diff
                .lines()
                .count()
                .saturating_sub(1)
                .try_into()
                .unwrap_or(u16::MAX),
        );
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        frame.render_widget(Clear, area);
        let block = Block::new()
            .borders(Borders::ALL)
            .title(format!("设置：{}", self.role));
        let inside = block.inner(area);
        frame.render_widget(block, area);
        let [notice, body] = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Length(2),
            ratatui::layout::Constraint::Min(0),
        ])
        .areas(inside);
        frame.render_widget(Paragraph::new(self.notice.as_str()), notice);
        frame.render_widget(
            Paragraph::new(self.diff.as_str()).scroll((self.scroll, 0)),
            body,
        );
    }
}
