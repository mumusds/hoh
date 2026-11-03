# CR-002 · 确认 M1/M2 设计选择，并修正四处设计缺陷

ID：CR-002
状态：APPROVED
批准人：项目所有者（2026-11-03 在 Claude Code 会话中确认 CR-001 批准人、v0.1 范围与全部暂定初值，并授权按评审建议执行）
日期：2026-11-03
基线：HOH-BASELINE-1.1 revision 2 → revision 3

## 确认内容

1. 确认 CR-001 的批准人记录。
2. 确认 v0.1 范围为 18 项需求：加入 REQ-001/005，不含 REQ-006。
3. M1/M2 需求中的暂定选择（原 `open_questions`）全部确认为基线设计选择，字段更名为 `decisions`。其中写作“初值”的数值是可经 CR 调整的基线值。

## 修正（复审后发现原选择站不住）

| 需求 | 原选择 | 问题 | 修正 |
|---|---|---|---|
| REQ-038 调度 | 仅靠 age_bonus 老化，上限 10 | 有上限的老化无法弥补超过 10/cost 的分差：score 0、cost 5 的任务面对持续到达的 score 6 任务会永远饥饿 | 新增 `max_wait_rounds`（初值 20）：达到上限的任务进入位于层0与层1之间的 FIFO 老化子层。层0为空时等待轮数有上界。新增 AC-038-06，参考模型同步实现 |
| REQ-042 预算 | 价格未知时费用上限判为 unknown，但继续派发 | 用户明确设置了费用上限，却在无法核算时继续花费，违背其约束 | 配置了费用上限而价格未知时，派发前 BLOCKED（`BUDGET_UNENFORCEABLE`）；未配置费用上限时行为不变。新增 AC-042-06 |
| REQ-062 持久化 | 只按名单拒绝网络文件系统，virtiofs 未定 | 名单无法覆盖所有共享/FUSE 文件系统；SQLite WAL 是否可靠取决于实际的锁与共享内存语义 | 名单外的文件系统在 host 启动时做 WAL 自检，失败即 `DB_WAL_UNSUPPORTED`。新增 AC-062-06 |
| REQ-026 Pi RPC | 只按 0x0A 分帧，CR 交给 JSON 解析 | Pi 1.5.0 官方文档要求剥离 LF 前可选的 CR | 按官方文档修正；其余帧名（disposition、agent_settled、abort、get_state、extension_ui_*、`--session-id`）已对照本机安装版本的文档核实 |

另外：SQL 的 `leases.kind` 放宽为 `writer`/`supervisor`/`reader-N`，与 REQ-037 一致。已在 CR-001 中解决的“需迁移/需统一”备注，改写为实际状态。

## 不变的内容

范围、门禁阈值、发布 profile 都不变。AC 数从 161 增至 164，新增的都是更严格的验收。

## 验证

`bash scripts/check.sh --bundle` 通过。参考调度模型新增有界等待测试：第 21 轮选中饥饿任务；层0任务仍然优先。SQL 新增租约 kind 测试。
