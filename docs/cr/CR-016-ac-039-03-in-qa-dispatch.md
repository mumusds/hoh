# CR-016 · AC-039-03 改归 QA 派发任务

ID：CR-016
状态：PROPOSED（待独立审查；属 CR-003 D1 预授权的细化，审查判定 PASS 后由 CTO 批准）
批准人：（待定）
日期：2026-11-11
基线：不改受锁文件，不重新上锁（只改 `contracts/development-tasks.json` 的任务归属，重新生成 docs/18）

## 背景

AC-039-03 的 when 是“Runtime 为 QA 和新 Commander 生成 ContextHandoff 与 Prompt”，TEST-039-03 的夹具是“SQLite 临时库、含哨兵的 raw log 对象、FakeAgent 捕获输入”，步骤“触发两次交接，捕获两份 Prompt 并检索哨兵串”。所以 QA 的那一份 Prompt 必须由 Runtime 经产品的 QA 派发生成，并由假 Agent 捕获。

M2-T09（依赖 M2-T07、M2-T04、M2-T17）的依赖闭包里没有 QA 派发：验证交接包由 `accept_task_result` 生成，它在产品里没有调用者；从验证交接创建 QA Attempt 属 M2-T16（ADR-018 “不在本任务内”一节），而 M2-T16 依赖 M2-T09。M2-T09 的设计审查（M2T09D-05、-50）指出：在 M2-T09 里只能由测试直接调用装箱函数、或由夹具代替派发来生成 QA 的 Prompt，测不到 QA 的收集与派发路径上的污染与预算，按它判 PASS 等于降低门槛。docs/08 第 45 行（CR-004）规定每条 AC 只分给依赖闭包已经提供其所需能力（含 QA/Review 循环）的任务。

## 决定

1. AC-039-03 从 M2-T09 改归 M2-T16，做法与 CR-004 F2、CR-008、CR-010 相同：AC 分给了早于其所需能力的任务，就改归属。
   - M2-T09：acceptance 写明 AC-039-01、AC-039-02、AC-039-04；done_when 改为“AC-039-01、-02、-04 通过；交接只传引用（REQ-039 行为 5）由 Commander 交接的非 AC 测试覆盖。需要 QA 派发的 AC-039-03 归 M2-T16（CR-016）。”
   - M2-T16：requirements 加上 REQ-039；acceptance 加上 AC-039-03；done_when 加上“AC-039-03 通过（CR-016：QA 的 Prompt 经产品 QA 派发、同一收集函数与装箱器生成，由 FakeAgent 捕获）”。
   - `integration` 检查的所有者不变。
2. M2-T09 仍按 REQ-039 行为 5 与失败模式 3 实现交接只传引用（ADR-026 决定 10）：两种交接包的 references 与预算、只供引用的种类与字段白名单、QA 收集函数。它用不带 TEST id 的补充测试验证能验证的部分：新 Commander 经产品执行器与假 pi 的首个 Prompt 中哨兵（放在前任原始日志、adapter 原始帧与 TaskResult 非白名单字段里）出现 0 次，references 非空且每项为 64 位十六进制、文件存在，Prompt 的估算不超过交接包的 budget_tokens；QA 的收集与装箱有函数级测试。这些测试的名字里没有 `test_039_03`，不生成 TEST-039-03 的记录，不计为 AC-039-03 的证据；正式的 TEST-039-03 由 M2-T16 实现。

## 影响

- `contracts/development-tasks.json`：M2-T09 与 M2-T16 两项。
- 重新生成 `docs/18`（`tools/gen_spec_views.py`）。
- M2-T16 的 AC 数由 9 变 10、需求由 6 变 7。docs/08 “每个实现任务 ≤2 项需求、≤8 条 AC”是 M3–M7 拆分时的规则；M2-T16 在 CR-004 时已超出，这里沿用同一处理（能力所在的任务承接），不另拆任务。

## 不变内容

REQ-039 与 AC-039-03 的文本、TEST-039-03 的场景与检查、release policy 与基线锁。没有删除或降级任何需求或 AC；AC-039-03 仍是 v1 的必需项。不改 CONTRACT 的架构与交互决定。

## 是否属于 CR-003 D1 预授权

属于“修正任务归属与依赖能力的不一致”；不删范围、不降门槛、不改架构（见“不变内容”）。由独立审查判定后批准。

## 验证

- `python3 tools/gen_spec_views.py` 之后 docs/18 中 AC-039-03 的任务为 M2-T16；`bash scripts/check.sh --bundle` 通过。
- M2-T16 实现 TEST-039-03 时按场景原文执行（两份 Prompt 都经 Runtime 生成并由假 Agent 捕获）。
