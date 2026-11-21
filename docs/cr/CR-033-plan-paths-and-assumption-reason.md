# CR-033 · 计划任务的路径范围与“假设未列出”的拒绝码

ID：CR-033
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权批准（2026-11-22），独立审查见“审查记录”。本 CR 只给受锁协议增加一个可选字段和一个拒绝码，不缩小范围、不降低门槛
日期：2026-11-22
关联：M3-T06（REQ-031、REQ-030）、REQ-039（按任务范围选代码，ADR-026 决定 12，CR-016）、REQ-028（AC-028-03）、M3-T07

## 背景

M3-T06 的实现清单（`.hoh-dev/evidence/m3-t06-prep/inventory.md` 第 8 节 Q3、Q10）指出两处受锁契约缺口：

1. **路径范围没有载体。** M3-T06 的 done_when 要求“Task 的路径范围随计划与 readiness 出现（CR-016），M3-T07 的脏文件判定使用它”，AC-028-03 的给定也要求计划携带路径范围。今天 `contracts/protocol.schema.json` 的 `PlanProposal.tasks.items` 是 `additionalProperties: false`，没有可以放路径的字段，不经 CR 加不进去。
2. **未列出的假设没有恰当的拒绝码。** REQ-030 行为 4：无人值守时 `record_assumption` 的 basis 不是 `[assumptions]` 中列出的键，决策被拒（REJECTED）。`decisions.reason_code` 是持久、机器可读的值；现有 `RejectionReason` 中没有语义相符的码，挪用 `ILLEGAL_TRANSITION` 会误导客户端与 Commander。

## 决定

1. `$defs.PlanProposal.properties.tasks.items.properties` 增加可选的 `paths`：字符串数组（每项 1–4096 字符，`uniqueItems`），项目相对的文件或目录。Runtime 拒绝绝对路径、空串与含 `..` 分量的路径（整份计划被拒，不导入任何 Task），与分值越界由 Runtime 拒绝的写法一致（schema 只写形状；产品的正则不支持环视，不在 schema 里写模式）。缺省或空数组：该 Task 没有代码类上下文来源，也不是 readiness 的不就绪原因。存放位置（`plan.imported` 的载荷，不加 SQL 列）由 M3-T06 的 ADR-035 写定。
2. `$defs.RejectionReason.enum` 末尾增加 `ASSUMPTION_NOT_LISTED`，描述写明它用于 REQ-030 行为 4 / AC-030-02 的拒绝，details 名出 basis 键。Rust 的 `RejectionReason` 在同一提交按 schema 顺序增加 `AssumptionNotListed`（`protocol_roundtrip` 要求逐项相等）。

`PlannedTask` 的 Rust 字段 `paths` 与 Runtime 的拒绝、存放、读取由 M3-T06 的切片 S4 实现（它也负责 REQ-039 的选码）；本 CR 只改契约与枚举，不改行为。旧计划（没有 `paths`）照旧有效。

## 不变的部分

需求、AC、场景、检查计划、发布策略与门禁都不变；AC 数不变。`PlanProposal` 的必需字段不变。不改 CONTRACT 的架构、角色分工与交互决定（`paths` 是 Commander 计划的可选数据，不是新的用户交互）。

## 是否属于 CR-003 D1

是：受锁契约的加法式细化，为已批准 AC（AC-028-03、AC-030-02）与已批准任务义务（M3-T06 的 done_when、CR-016）提供载体，不缩小范围、不降低门槛。

## 验证方式

在本 CR 的工作树中（不是副本）：`python3 tools/gen_spec_views.py`；`python3 tools/baseline.py relock --cr docs/cr/CR-033-plan-paths-and-assumption-reason.md`；`python3 tools/validate_bundle.py`；`bash scripts/check.sh --bundle`；`cargo test -p hoh-core --locked --test protocol_roundtrip`；`cargo test --workspace --locked`。结果记在提交说明与 `.hoh-dev/evidence/cr033/`。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 | 发现与处置 |
|---|---|---|---|---|
| 1 | `0486fd80` | Grok，会话 ['01a1ff3b-9a05-7f83-b0e9-50e33c234913']（attempt `050226f4-37f9-4e05-a6dc-f8b08cf15985`） | PASS | CR33-01 low：`PlannedTask` 还没有 `paths`，schema 接受的 `paths` 被 serde 丢弃，绝对路径与 `..` 也尚未拒绝。按设计由 M3-T06 S4 实现（本 CR“决定”末段）；记入 M3-T06 的 done_when 检查，不另开项。 |
