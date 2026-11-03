# 恢复HoH自主开发

先读AGENTS.md、CONTRACT.md、.hoh-dev/PROGRESS.md，运行 `python3 tools/taskboard.py list`；检查当前Git分支/dirty状态、最近测试和外部阻塞。不要重新规划已采纳范围，不相信上次“完成”口头结论。仍为 running 的任务先核实其 owner 与实际副作用：继续用原 owner 完成，或用 `taskboard set <id> pending --owner <原owner> --note <原因>` 释放；禁止盲重放或覆盖用户修改。然后选依赖满足的任务（`next` / `claim --owner`），继续 TDD→候选→独立Review→修复闭环。release_gate 对 v1 profile 通过之前不得宣告最终完成；到checkpoint持续更新恢复记录。
