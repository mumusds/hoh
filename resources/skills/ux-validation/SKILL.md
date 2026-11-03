---
name: hoh-ux-validation
description: 交互与使用体验验证——CTO设计UX时把旅程写成可执行交互断言；QA在冻结候选上用真实PTY与浏览器走旅程，覆盖宽度、宽字符、键盘、断线等矩阵，先断言后截图。
---
# 交互与使用体验验证

## 何时使用
- CTO：编写或修改UX规格，需要把用户旅程转为可执行的交互断言。
- QA：涉及TUI、Web或CLI输出的候选验收；发布前的全量UX检查。

## 输入
- UX规格（项目的ux文档；HoH自身见 `docs/03-ux.md`）、相关AC与TEST id。
- QA模式：冻结候选；终端与浏览器环境说明（终端类型、TERM、locale、浏览器版本）。

## 步骤（CTO设计模式）
1. 列核心旅程：每步写用户动作、期望可见状态与可判定的断言。
2. 为每个视图定义loading/empty/error/stale状态，以及焦点与返回行为。
3. 列出键盘路径并检查快捷键冲突；鼠标只作增强，不是唯一入口。
4. 把断言映射到TEST id，写入 `resources/templates/ux.md` 的“交互测试与截图证据”一节。

## 步骤（QA验证模式）
1. 校验候选身份，在独立目录构建并启动；记录版本与环境。
2. 旅程清单（按任务范围裁剪，未覆盖项标NOT_RUN）：
   - [ ] 输入与发送；模式切换（如 /plan、Shift+Tab）
   - [ ] Workspace、任务展开、Timeline、Focus、团队设置
   - [ ] 暂停/恢复/取消；关闭客户端后后台任务继续
   - [ ] 错误展示与恢复建议（认证缺失、能力不支持、预算耗尽）
   - [ ] Web断线：stale标记、最后事件序号、重连后先补历史再接live
3. 环境矩阵：
   - [ ] 宽度80/120/180列（跨越<100、100–159、≥160三档布局阈值）及较小高度
   - [ ] 中文与emoji宽字符的对齐、换行与光标位置
   - [ ] bracketed paste与多行输入的回退键
   - [ ] 纯键盘、NO_COLOR、无鼠标、ASCII图标回退
   - [ ] 在SSH与tmux中运行
   - [ ] SIGTERM与panic后终端恢复（raw mode与alternate screen）
4. 断言优先：TUI用真实PTY驱动，断言终端buffer内容、光标与退出码；Web用DOM与可访问角色查询断言，最后再截图。截图只证明渲染，不证明功能。
5. 执行检查计划中的条目（经run_check类工具，mode由计划决定）；保存PTY转录、buffer快照、截图与浏览器trace，并在计划中声明为产物以便哈希。
6. 按“旅程×矩阵单元”分类PASS/FAIL/BLOCKED/UNKNOWN/NOT_RUN；FAIL写Issue（复现步骤、宽度与终端、期望/观察、证据路径）。

## 产出物与格式
- 设计模式：`resources/templates/ux.md` 各节（用户流程与模式、视图与焦点模型、输入与快捷键、各种状态、响应式与无障碍、交互测试与截图证据）及TEST映射。
- 验证模式：执行记录（`contracts/execution.schema.json`）与证据文件；按AC的观察记录并入independent-qa报告；TaskResult的issues列UX缺陷。

## 完成判据
- 范围内每条旅程在要求的矩阵单元上都有状态与证据；未覆盖的单元明确标NOT_RUN及原因。
- 每张截图都有对应的交互断言。

## 常见错误
- 截图好看就判PASS。
- 只测默认宽度或英文输入；用字节长度计算显示宽度。
- 用模拟终端快照代替真实PTY却不标注。
- 快捷键全局劫持（如输入框里的“?”），或鼠标成为唯一入口。
- 忽略断线、慢客户端与版本冲突路径。

来源：HoH原创工程规范。上游方法参考见research/SOURCES.md；不是Matt原文件的逐字副本。
