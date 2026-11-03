# CR-001 · 启动包加固、发布分级与规格滚动细化

ID：CR-001
状态：APPROVED
批准人：项目所有者（2026-11-03 在 Claude Code 会话中逐项选择）
日期：2026-11-03
基线：HOH-BASELINE-1.0 → HOH-BASELINE-1.1

## 背景

对 kit 1.0.0 的整体审查发现三类问题：

1. 规格不可判定。70 条 AC 共用同一句 `when`，功能规格的“边界”段是同一句话复制 70 次，任务的 `done_when` 全部相同。
2. 门禁可被轻易绕过。执行模式由调用者自报；命令不与测试计划比对；soak 时间线可以手写；AC 可以引用任意检查。
3. 发布范围和节奏风险高。三个第三方 Pi 插件是 v1 硬门禁；完整 v1 之前没有可用产物；产品 Runtime 不存在时就要求三 Agent 编排。

## 决定（项目所有者选择）

| # | 决定 | 影响 |
|---|---|---|
| D1 | 三个 Pi 插件保留在 v1 范围内，但不再是硬门禁。REQ-048/049/050 的 `release_class=degradable`；REQ-051（安装、诊断、显式降级）仍为 required | 插件不可用时，带诊断证据和上游引用的 DEGRADED 可以通过门禁，发布状态为 `COMPLETED_DEGRADED`，不能报告为 `COMPLETED` |
| D2 | 增加 v0.1 中间版本。v1 最终范围不变 | `release-policy.json` 增加 `v0.1` profile，覆盖 18 项需求（`release_targets` 含 `v0.1`）。门禁通过后状态为 `INTERIM_RELEASED`，永远不会是 `COMPLETED` |
| D3 | 自举阶段 M0–M2 只用一个主开发会话加一个独立 Review 会话。Codex 指挥和 Pi QA 在 M2 Runtime 可用后再接入 | 产品默认团队不变（CONTRACT 第 5 条）。`start.sh` 默认启动 Claude 主开发；M1/M2 的验收由独立会话完成；v0.1 发布仍需 QA 与 Reviewer 两个不同会话 |

## 随之进行的工程修正（不改变产品范围）

- **规格滚动细化。** 需求增加 `spec_maturity`。M1/M2 的 27 项需求细化为 detailed，每项 2–5 条可判定 AC，并附带行为、失败模式和测试场景。M3–M7 为 outline，各里程碑先执行 spec 任务把它们细化为 detailed；`taskboard` 拒绝领取依赖 outline 需求的实现任务。
- **ID 格式统一。** AC/TEST 统一为 `AC-NNN-MM` / `TEST-NNN-MM`。场景携带 `check_id`；门禁要求每条 AC 引用该检查，原始证据必须来自该检查的输出目录。
- **检查计划。** 测试计划改为机器可读的检查计划（`contracts/check-plan.schema.json`）。`run_check` 只执行计划中的 argv，mode 和 cwd 由计划决定，并哈希声明的产物。live 检查必须产出 `live-sessions.json`；soak 时间线必须是 soak 检查自己产出的产物，并绑定被测 HoH 候选。
- **独立报告绑定原生会话。** 独立报告必须引用原生 Agent 的 attempt 记录，且该记录中观察到报告的 session_id。
- **Schema 统一。** 状态枚举统一并补齐 UNKNOWN/DEGRADED；候选 ID 统一为 `CAND-<sha256>`；git id 只接受 40 或 64 位。
- **基线锁。** 基线锁纳入门禁工具、`docs/10-acceptance.md` 和全部新 schema。`development-tasks.json` 移出基线锁，作为可细化的计划。实现状态字段移出受锁文件。基线变更通过 `tools/baseline.py relock --cr` 进行。
- **工具修复。** Pi 安装补 `--approve`；全部 Agent 拒绝以 root 运行；嵌套顶层会话被拒绝；`taskboard` 增加 claim/owner/状态迁移/单写者/重开级联；校验器排除 `node_modules`/`target` 等目录；候选清单以排他方式写入。
- **SQL 与 OpenAPI。** 补齐 SQL 的约束、索引和缺失实体；OpenAPI 增加本地 token、过滤和类型化命令。

- **独立复核后的加固。** 由未参与开发的会话复核本 CR 的改动，发现并已修复以下问题，每项都补了负例测试：
  - `--amend` 可以改动已提交的 CR；
  - 门禁不校验 CR 状态；
  - AC 证据可借 `..`、重叠目录或额外检查来源；
  - 失败的可降级检查能让 required AC 通过；
  - 报告的 critical finding 被忽略；
  - 报告未绑定本候选的会话与输出；
  - run_check 对 SIGTERM 和项目外路径的处理不当；
  - 校验器会扫描产品文件。

## 不变的内容

70 项需求全部仍在 v1 范围内。门槛不降低：72h soak 与至少 20 个真实轮次、critical/high 为零、独立 QA/Review、mock 不得冒充 live。CONTRACT 的其余决定不变。

## 验证

`bash scripts/check.sh --bundle` 通过（包含 `tools/baseline.py verify` 和生成视图一致性检查）。门禁的正例与负例测试见 `tests/bundle/test_gate.py`。
