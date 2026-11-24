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
    run: String,
    configured: Option<String>,
    diff: String,
    scroll: u16,
    notice: String,
    /// The host bundle the diff is measured against. The TUI binary uses `Bundle::current` (M3G-54).
    bundle: hoh_core::resource_bundle::Bundle,
}

impl Settings {
    pub fn open(
        root: &Path,
        run: &str,
        role: &str,
        bundle: &hoh_core::resource_bundle::Bundle,
    ) -> Result<Self, prompt::Error> {
        let config = config::load(root).map_err(prompt::unavailable)?;
        let configured = config.team.get(role).and_then(|r| r.prompt.clone());
        let resolved = prompt_files::load_with_bundle(
            root,
            prompt_files::global_root().as_deref(),
            run,
            role,
            configured.as_deref(),
            bundle,
        )?;
        let builtin = resolved
            .chain
            .iter()
            .find(|c| c.layer == "builtin")
            .ok_or_else(|| prompt::unavailable("该职位没有内置默认契约"))?;
        let effective = resolved.effective()?;
        let reports = prompt_files::drift_reports(root, &resolved, bundle)?;
        let mut diff =
            prompt::unified_diff(&builtin.text, &effective.text, "builtin", effective.layer);
        for report in &reports {
            diff.push_str(&format!(
                "\n覆盖漂移：{}\n",
                report["path"].as_str().unwrap_or_default()
            ));
            if report["base_status"] == "known" {
                for name in ["builtin", "override"] {
                    diff.push_str(report["diff"][name].as_str().unwrap_or_default());
                }
            } else {
                diff.push_str("基准缺失或与覆盖不匹配，旧内置未知；仅显示当前内置对照。\n");
                diff.push_str(report["current_diff"].as_str().unwrap_or_default());
            }
        }
        Ok(Self {
            role: role.to_owned(),
            run: run.to_owned(),
            configured,
            diff,
            scroll: 0,
            bundle: bundle.clone(),
            notice: format!(
                "{}↑↓ / PgUp PgDn 浏览 · r 恢复默认（删除 project 覆盖）· Esc 返回",
                if reports.is_empty() {
                    ""
                } else {
                    "覆盖漂移；三方对照见 diff 末尾。"
                }
            ),
        })
    }

    pub fn restore(&mut self, root: &Path) {
        self.notice =
            match prompt_files::restore_project(root, &self.role, self.configured.as_deref()) {
                // Ok(false) is the idempotent path: the conventional file was already absent (M3G-01).
                Ok(false) => match self.reload_diff(root) {
                    Ok(()) => "没有 project 覆盖可删除。Esc 返回".to_owned(),
                    Err(e) => format!("恢复失败：{e}"),
                },
                Ok(true) => match self.reload_diff(root) {
                    Ok(()) => {
                        "已恢复默认：project 覆盖已删除；下次 Attempt 重新读取覆盖链。Esc 返回"
                            .to_owned()
                    }
                    Err(e) => format!("恢复失败：{e}"),
                },
                Err(e) => format!("恢复失败：{e}"),
            };
    }

    /// Rebuild the diff from the files on disk. A successful restore otherwise keeps the deleted override on screen.
    fn reload_diff(&mut self, root: &Path) -> Result<(), prompt::Error> {
        let resolved = prompt_files::load_with_bundle(
            root,
            prompt_files::global_root().as_deref(),
            &self.run,
            &self.role,
            self.configured.as_deref(),
            &self.bundle,
        )?;
        let builtin = resolved
            .chain
            .iter()
            .find(|c| c.layer == "builtin")
            .ok_or_else(|| prompt::unavailable("该职位没有内置默认契约"))?;
        let effective = resolved.effective()?;
        let reports = prompt_files::drift_reports(root, &resolved, &self.bundle)?;
        let mut diff =
            prompt::unified_diff(&builtin.text, &effective.text, "builtin", effective.layer);
        for report in &reports {
            diff.push_str(&format!(
                "\n覆盖漂移：{}\n",
                report["path"].as_str().unwrap_or_default()
            ));
            if report["base_status"] == "known" {
                for name in ["builtin", "override"] {
                    diff.push_str(report["diff"][name].as_str().unwrap_or_default());
                }
            } else {
                diff.push_str("基准缺失或与覆盖不匹配，旧内置未知；仅显示当前内置对照。\n");
                diff.push_str(report["current_diff"].as_str().unwrap_or_default());
            }
        }
        self.diff = diff;
        Ok(())
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
