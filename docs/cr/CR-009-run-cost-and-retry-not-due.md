# CR-009 · RunResult 的费用与剩余需求字段、RETRY_NOT_DUE 拒绝码

ID：CR-009
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权（独立 Review 会话 01a1b600-d1f0-76a2-8b7b-f3a1087b6eb9 判定 PASS，四项 D1 条件均满足；第一轮会话 01a1b5f7-eef5-7282-a902-75e28c73a34e 的 CR9-01..03 已关闭）
日期：2026-11-07
基线：HOH-BASELINE-1.1 revision 8 → revision 9

## 背景

REQ-042 要求价格未知时 cost 为 null 并标 unknown，不以估算冒充账单。以下两条 AC 的观察项都要用到 `cost_usd=null`、`cost_status=unknown`：

- AC-042-04：最终 JSON 结果要列出这两个字段（M2-T13）。
- AC-042-06：Rd 要求这两个字段，观察项里有"最终 JSON 结果"（M2-T19）。

但 `$defs.RunResult`（CR-005 F1）不允许其他字段，契约里也没有任何地方定义 `cost_status`。这是规格之间的不一致，也缺字段。

同样，AC-042-04 与 TEST-042-04 要求最终 JSON 列出未 accepted 的 REQ（REQ-042 失败模式 3），而 RunResult 只有 accepted/total 两个计数，也放不下这份列表（CR-009 第一轮审查 CR9-02）。

REQ-042 行为 2 要求 `next_retry_at` 之前不得派发该项。执行循环的调度轮可以直接把等重试的 Task 排除在候选之外（ADR-020 决定 4）。Commander 的 decide 路径则需要一个拒绝码，记入 decisions.reason_code 与 rejected 消息，而 `$defs.RejectionReason` 里没有合适的：

- `LEASE_UNAVAILABLE` 说的是租约；
- `BUDGET_NOT_RESERVABLE` 说的是预算。

这是缺失的原因码。

## 决定

1. **RunResult 增加两个费用必填字段**：
   - `cost_usd`：number ≥ 0 或 null；
   - `cost_status`：`known` 或 `unknown`。

   取值规则：只有同时满足下面两条时才是 `known`，cost_usd 为各 Attempt 费用之和（没有 Attempt 时为 0）：
   - Run 所用模型的价格已知，即 Run 级 usd 预算行的 price_known=1；
   - Run 的每个 Attempt 都有账单费用（attempts.cost_usd 非空）。

   其他情况一律为 `unknown`，cost_usd 为 null；包括价格未知（没有 usd 行，或 price_known=0），即使 Run 没有 Attempt（AC-042-06 的 Rc：派发前阻塞、0 个 Attempt，仍是 null/unknown，第一轮审查 CR9-01）。schema 用 if/then/else 约束 null 与 unknown 的对应关系。说明文字写明不以估算冒充账单。
2. **RunResult 增加必填字段 `remaining_requirements`**：
   - 内容：当前规格修订中尚未在该修订下 accepted 的需求 id，每个一次，升序；
   - 取值约束：元素形如 `REQ-NNN`，不重复；
   - 与计数的关系：长度等于 total − accepted；
   - spec_revision 为 null 时为空数组。

   它对应 REQ-042 失败模式 3 与 AC-042-04 的"最终 JSON 列出剩余项"。
3. **RejectionReason 增加 `RETRY_NOT_DUE`**：Task 正在等它持久化的 next_retry_at。
4. 同步 hoh-core `protocol.rs` 的 `RunResult`（新增 `CostStatus`）与 `RejectionReason`。schema 语料 `tests/fixtures/schema/corpus.json` 里 RunResult 与 RunResultEvent 的 21 个用例全部迁移到新字段，每个负例仍因原来的那个错误被拒（第一轮审查 CR9-03）。另外新增 12 个用例：费用的对应关系（unknown 时有数值、known 时为 null、负数、取值以外的状态）、三个新字段各自必填、剩余需求的 id 形式与不重复。

## 影响

- 只补齐 REQ-042 已要求的字段与原因码，没有删除或降级任何需求或 AC，没有放宽门禁，也不改 CONTRACT 的架构与交互决定（CR-003 D1 第 1–4 条）。
- RunResult 由 M2-T13 输出（`--output json` 与 `run.result` 事件），剩余需求列表也由它填写。M2-T19 提供费用投影 `Store::run_cost`，以及 decide 路径的 RETRY_NOT_DUE 拒绝（ADR-020 决定 9、11）。

## 不变内容

- RunResult 原有字段、退出码表与 blocked_reason 规则不变。
- 其余拒绝码与优先级不变。

## 验证方式

- `python3 tools/gen_spec_views.py`、`python3 tools/baseline.py relock --cr docs/cr/CR-009-run-cost-and-retry-not-due.md`、`bash scripts/check.sh --bundle`。
- hoh-core 的 schema 一致性与语料往返测试：类型与 schema 的字段、枚举一致，语料逐例判定一致；bundle 自检用 Python jsonschema 复核同一语料。
- 费用投影与 RETRY_NOT_DUE 的单元测试（M2-T19）。
