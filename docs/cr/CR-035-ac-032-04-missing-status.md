# CR-035 · AC-032-04：没有成员证据时的追踪状态写为 MISSING

ID：CR-035
状态：APPROVED
批准人：CTO 依 CR-003 D1 预授权批准（2026-11-22），独立审查见“审查记录”。只改一条 AC 与其场景的状态用词，使其与受锁的追踪状态定义一致；不缩小范围、不降低门槛
日期：2026-11-22
关联：M3-T07（REQ-032，AC-032-04，TEST-032-04），ADR-027 原则一与决定 10，`scripts/product/traceability.py`

## 背景

AC-032-04 的 Then 写着“没有成员证据时 3 个 TEST 都是 NOT_RUN 而不是错误”，TEST-032-04 的通过标准写着“无证据时为 NOT_RUN”。追踪检查（`scripts/product/traceability.py`，ADR-027 原则一）对 TEST 状态的定义是：本次运行自带结果或场景检查有成员、但没有结论时为 NOT_RUN；场景检查**没有任何成员**时为 MISSING。AC-032-04 描述的正是后一种情形（刚冻结的候选，项目里还没有任何检查执行），按受锁实现得到的是 MISSING。

M3-T07 的实现按 ADR-027 断言 MISSING，并在 ADR-039 决定 1 写明“待 CTO 确认这一读法”；M3-T07 第 1 轮独立审查（M3N-006，medium）指出：测试与 AC 字面不一致，应遵从当前 AC，或先通过正式规格变更解决状态定义冲突，不能只由实现 ADR 改写预期。

## 决定

AC-032-04 的这一句改为“没有成员证据时 3 个 TEST 都是 MISSING（ADR-027 原则一：场景检查没有任何成员时的状态；与 NOT_RUN 一样是无结论、未满足）而不是错误”；TEST-032-04 的通过标准相应改为“无成员证据时为 MISSING（不是错误）”。

不改 ADR-027 的状态定义：MISSING 与 NOT_RUN 都不属于满足（`SATISFIED` 只有 PASS 与 DEGRADED），发布门禁对两者的处理相同；把这里改成 NOT_RUN 反而要改受锁追踪检查的定义，影响所有已有报告。AC 的本意（“不是错误”）不变。

## 不变的部分

需求、AC 数、场景检查（integration）、检查计划、发布策略与门禁都不变。不改 CONTRACT 的架构与交互决定。

## 是否属于 CR-003 D1

是：受锁验收文字的更正，使其与受锁的追踪状态定义一致；没有缩小范围，没有放宽任何通过条件（MISSING 与 NOT_RUN 同为未满足）。

## 验证方式

在本 CR 的工作树中：`python3 tools/gen_spec_views.py`；`python3 tools/baseline.py relock --cr docs/cr/CR-035-ac-032-04-missing-status.md`；`python3 tools/validate_bundle.py`；`bash scripts/check.sh --bundle`。

## 审查记录

| 轮 | 冻结提交 | 审查者 | 结论 | 发现与处置 |
|---|---|---|---|---|
