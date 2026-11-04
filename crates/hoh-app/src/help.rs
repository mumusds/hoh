//! Help and version texts (REQ-003). The top-level text is pinned by crates/hoh-app/tests/golden/help.txt; every line
//! fits 80 terminal columns.

use crate::cli::Topic;

pub const TOP: &str = "\
hoh：多 Agent 协作的自主软件工程 Harness

用法：
  hoh [--project <目录>]       打开 TUI 工作区
  hoh -p <文本> [选项]         非交互运行，到终态或需要你决定时退出
  hoh <子命令> [选项]

子命令：
  attach [run_id]    重新接上一个 Run（关闭窗口不会取消任务）
  web                在浏览器中打开同一个 host 的界面
  doctor             检查原生 Agent 与运行环境的能力
  help               显示帮助；hoh help <子命令> 显示子命令的帮助

选项：
  -p <文本>          以这段文本创建一个非交互 Run（从不读取 stdin）
  --output <格式>    text（默认）、json 或 stream-json
  --detach           提交后输出 run_id 并立即返回（只与 -p 同用）
  --project <目录>   项目目录（默认为当前目录）
  --resume <run_id>  把输入交给已有的 Run（只与 -p 或 attach 同用）
  --help             显示帮助
  --version          显示版本

退出码：0 COMPLETED  1 FAILED  3 BLOCKED  4 INCOMPLETE  5 CANCELLED
6 PAUSED  64 用法错误  69 host 不可用  78 配置错误
130/143 被 SIGINT/SIGTERM 中断（后台任务继续）
";

pub const ATTACH: &str = "\
用法：hoh attach [run_id] [--output <格式>] [--project <目录>]

重新接上一个 Run：先回放已提交的事件，再接续实时事件；关闭或断开不会取消
任务。没有给 run_id 时，恰有一个可接上的 Run 就接上它；有多个时在终端中让你
选择，非终端时列出候选并以 64 退出，不会自动选最近的 Run。

选项：
  run_id, --resume <run_id>  要接上的 Run
  --output <格式>            text（默认）、json 或 stream-json
  --project <目录>           项目目录（默认为当前目录）
  --help                     显示本帮助
";

pub const WEB: &str = "\
用法：hoh web [--project <目录>]

启动或连接本项目的 host，并给出本机 Web 界面的地址。Web 与 TUI 显示同一组
Run，不另起任务。

选项：
  --project <目录>  项目目录（默认为当前目录）
  --help            显示本帮助
";

pub const DOCTOR: &str = "\
用法：hoh doctor [--live] [--output <格式>] [--project <目录>]

逐项报告 Codex、Claude、Pi 与运行依赖：是否安装、协议是否兼容、认证检查
（可能是 unknown）。默认不调用模型；退出码 0 只表示没有缺失项，不代表已认证。

选项：
  --live            用一次最小的原生调用确认确实能工作（会产生模型费用）
  --output <格式>   text（默认）或 json
  --project <目录>  项目目录（默认为当前目录）
  --help            显示本帮助
";

pub fn text(topic: Topic) -> &'static str {
    match topic {
        Topic::Top => TOP,
        Topic::Attach => ATTACH,
        Topic::Web => WEB,
        Topic::Doctor => DOCTOR,
    }
}

/// `hoh --version`; the first line is `hoh <semver>`.
pub fn version() -> String {
    format!(
        "hoh {}\ncore {}, agents {}\n",
        env!("CARGO_PKG_VERSION"),
        hoh_core::VERSION,
        hoh_agents::VERSION
    )
}

/// The one-line synopsis printed after a usage error.
pub const SYNOPSIS: &str = "usage: hoh [--project <目录>] | hoh -p <文本> [--output text|json|stream-json] [--detach] \
[--resume <run_id>] | hoh attach [run_id] | hoh web | hoh doctor [--live]（完整说明见 hoh --help）";

#[cfg(test)]
mod tests {
    use super::*;

    /// Terminal columns of a line: wide (CJK and fullwidth) characters take two.
    fn columns(line: &str) -> usize {
        line.chars()
            .map(|c| match c as u32 {
                0x1100..=0x115F
                | 0x2E80..=0xA4CF
                | 0xAC00..=0xD7A3
                | 0xF900..=0xFAFF
                | 0xFE30..=0xFE4F
                | 0xFF00..=0xFF60
                | 0xFFE0..=0xFFE6 => 2,
                _ => 1,
            })
            .sum()
    }

    #[test]
    fn every_help_line_fits_80_columns() {
        for (name, text) in [
            ("top", TOP),
            ("attach", ATTACH),
            ("web", WEB),
            ("doctor", DOCTOR),
        ] {
            for line in text.lines() {
                assert!(
                    columns(line) <= 80,
                    "{name}: {} columns: {line}",
                    columns(line)
                );
            }
        }
    }

    #[test]
    fn the_version_starts_with_the_semver() {
        let v = version();
        let first = v.lines().next().unwrap_or_default();
        let semver = first.strip_prefix("hoh ").unwrap_or_default();
        assert!(
            semver.split('.').count() == 3
                && semver
                    .split('.')
                    .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
            "{first}"
        );
    }
}
