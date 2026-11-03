# HoH 自主开发入口

**先判断执行层级。** 输入以`[HOH_DELEGATED_TASK]`开头时，你只承担该子任务和指定role；不执行全项目bootstrap，不读取LAUNCH_PROMPT后另启总指挥，不领取其他工作包、不更新全局完成状态。提交当前任务结果后结束；QA/Reviewer不得修改被测源码。下列连续推进规则只适用于用户启动的顶层会话或`[HOH_TOP_LEVEL_BOOTSTRAP]`，其余项目约束仍适用。

1. 先读 CONTRACT.md、docs/19-ai-development.md 和 contracts/requirements.json；这是已采纳规格，不重新采访已确定决定。
2. 本包不是HoH成品；目标是完整实现所有required_release需求，不止MVP。
3. 默认Codex指挥、Claude设计/编码、Pi研究/QA、独立Codex会话review；岗位可配置。
4. Rust模块化单体；原生Agent优先；禁止第四Coding Agent与外层Python无限继续循环。
5. Shell仅hoh/-p及attach/web/doctor；/plan在TUI；CLI与TUI统一语义而非一一命令。
6. 执行bootstrap、bundle checks和preflight，按development-tasks依赖小步TDD/实现/测试/审查。
7. 单写者+冻结候选+独立QA/Review；指令、代码、规格和证据版本关联。
8. 保留并完成TUI/Web、角色/Prompt自定义、内置Skill/模板和三Pi插件，不偷偷删范围。
9. 不伪造来源、模型能力、执行结果、费用和通过率；mock/live/not_run严格区分。
10. 不修改baseline/required gates以绿灯；需要变更写CR/ADR，遵守用户授权。
11. 每小步更新.hoh-dev/PROGRESS.md和任务板；会话压缩前保存恢复点。
12. 不在M1/MVP提前停工；未遇真实外部阻塞继续下一任务。
13. YOLO仅在用户声明的隔离VM；不做隐私/沙箱专项改造，但保留工程完整性。
14. 最终必须运行真实验收；只有全部required通过才COMPLETED，否则明确INCOMPLETE/BLOCKED。
