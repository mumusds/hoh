# 恢复HoH自主开发

先读AGENTS.md、CLAUDE.md、CONTRACT.md、goal.md、docs/19-ai-development.md（“持续推进与等待”“审查收敛”“恢复”三节）与 .hoh-dev/PROGRESS.md 顶部的现场。然后对账，不相信上次“完成”口头结论：`python3 tools/taskboard.py list`、`git worktree list` 与各 worktree 的分支/dirty 状态、`gh pr list` 与 CI、仍在运行的进程、`.hoh-dev/attempts/` 中已结束但未处理的结果（先收取，不重复派发）。仍为 running 的任务先核实其 owner 与实际副作用：继续用原 owner 完成，或用 `taskboard set <id> pending --owner <原owner> --note <原因>` 释放；禁止盲重放或覆盖用户修改。不要重新规划已采纳范围。然后选依赖满足的任务（`next` / `claim --owner`），继续 TDD→候选→独立Review→修复闭环；等待外部结果时用托管后台任务并记下收取条件。release_gate 对 v1 profile 通过之前不得宣告最终完成；到checkpoint持续更新 PROGRESS 顶部的现场。
